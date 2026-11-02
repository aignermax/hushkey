//! The push-to-talk key, read from the keyboards in /dev/input (evdev) on
//! its own thread. X11 has no reliable global key-up and Wayland gives
//! clients no global keys at all; the kernel devices work under both.
//! Reading them needs the `input` group (the Python installer's udev rule
//! and group, or `sudo usermod -aG input $USER`). Without it an X11
//! session still works: the X server reports the key instead
//! ([`super::xkey`]), as it did for the Python hushkey.
//!
//! The devices are not grabbed: the key keeps reaching the desktop. So a
//! Caps Lock hold has toggled Caps Lock like a tap would; a hold that was
//! a dictation gets a second, synthetic tap from hushkey's own uinput
//! keyboard (xdotool on X11 without it), so the caps state is as before. Releases are always seen
//! (the kernel reports them even with the screen locked), so no watchdog
//! is needed.

use std::collections::HashMap;
use std::io::ErrorKind;
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use evdev::{Device, EventType, KeyCode};

use super::{insert, uinput, xkey};
use crate::log;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyEvent {
    Down,
    Up,
}

/// A hold at least this long is a dictation (the app's `MIN_HOLD`); only
/// then is Caps Lock tapped back.
const HOLD: Duration = Duration::from_millis(400);
/// How often new keyboards (plugged in, woken up) are looked for.
const RESCAN: Duration = Duration::from_secs(2);
/// Devices whose key events are synthetic: hushkey's own, and ydotoold's
/// (the Python hushkey's paste chords).
const IGNORED: &[&str] = &[uinput::NAME, "ydotoold virtual device"];

/// The evdev key code for a config key name (by its Windows virtual-key
/// code, so every spelling `config::virtual_key` accepts works here too).
pub fn key_code(vk: u32) -> Option<KeyCode> {
    Some(match vk {
        0x14 => KeyCode::KEY_CAPSLOCK,
        0xA3 => KeyCode::KEY_RIGHTCTRL,
        0xA5 => KeyCode::KEY_RIGHTALT,
        0x91 => KeyCode::KEY_SCROLLLOCK,
        0x2D => KeyCode::KEY_INSERT,
        0x77 => KeyCode::KEY_F8,
        0x78 => KeyCode::KEY_F9,
        0x79 => KeyCode::KEY_F10,
        0x7A => KeyCode::KEY_F11,
        0x7B => KeyCode::KEY_F12,
        _ => return None,
    })
}

/// How long the held key has been silent; None: releases are always seen.
pub fn since_last_seen() -> Option<Duration> {
    None
}

/// Nothing to forget: the watcher always sees the release.
pub fn forget_press() {}

/// Why a device node was not opened.
enum Skip {
    Denied,
    NotKeyboard,
}

/// Open `path` if it is a keyboard with `key`.
fn open(path: &PathBuf, key: KeyCode) -> Result<Device, Skip> {
    let device = Device::open(path).map_err(|e| match e.kind() {
        ErrorKind::PermissionDenied => Skip::Denied,
        _ => Skip::NotKeyboard,
    })?;
    if device.name().is_some_and(|name| IGNORED.contains(&name)) {
        return Err(Skip::NotKeyboard);
    }
    // Letter keys tell keyboards from mice, lid switches and power
    // buttons, which also report keys.
    let keyboard = device
        .supported_keys()
        .is_some_and(|keys| keys.contains(key) && keys.contains(KeyCode::KEY_A));
    if !keyboard || device.set_nonblocking(true).is_err() {
        return Err(Skip::NotKeyboard);
    }
    Ok(device)
}

/// The event nodes in /dev/input.
fn nodes() -> std::io::Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for entry in std::fs::read_dir("/dev/input")? {
        let path = entry?.path();
        if path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("event"))
        {
            paths.push(path);
        }
    }
    Ok(paths)
}

/// The key's state, whichever way it is read.
struct Press {
    key: KeyCode,
    on_key: Box<dyn Fn(KeyEvent) + Send>,
    /// The key is down (on any keyboard) since then.
    pressed: Option<Instant>,
}

impl Press {
    fn down(&mut self) {
        if self.pressed.is_none() {
            self.pressed = Some(Instant::now());
            (self.on_key)(KeyEvent::Down);
        }
    }

    fn up(&mut self) {
        let Some(pressed) = self.pressed.take() else {
            return;
        };
        if self.key == KeyCode::KEY_CAPSLOCK && pressed.elapsed() >= HOLD {
            // The hold toggled Caps Lock on its way; toggle it back.
            if let Err(e) = insert::chord(&[KeyCode::KEY_CAPSLOCK], "Caps_Lock") {
                log::line(&format!("caps lock not restored: {e}"));
            }
        }
        (self.on_key)(KeyEvent::Up);
    }
}

struct Watcher {
    key: KeyCode,
    press: Press,
    devices: HashMap<PathBuf, Device>,
    /// Nodes looked at that are not keyboards, so a rescan only opens new
    /// ones.
    skipped: Vec<PathBuf>,
}

impl Watcher {
    fn new(key: KeyCode, on_key: Box<dyn Fn(KeyEvent) + Send>) -> Watcher {
        Watcher {
            key,
            press: Press {
                key,
                on_key,
                pressed: None,
            },
            devices: HashMap::new(),
            skipped: Vec::new(),
        }
    }

    /// Open keyboards not open yet; drop what is gone. Counts nodes that
    /// could not be read.
    fn scan(&mut self) -> std::io::Result<usize> {
        let paths = nodes()?;
        self.skipped.retain(|p| paths.contains(p));
        let mut denied = 0;
        for path in paths {
            if self.devices.contains_key(&path) || self.skipped.contains(&path) {
                continue;
            }
            match open(&path, self.key) {
                Ok(device) => {
                    log::line(&format!(
                        "key: watching {} ({})",
                        path.display(),
                        device.name().unwrap_or("?")
                    ));
                    self.devices.insert(path, device);
                }
                // A node just plugged in may be unreadable until udev has
                // set its group: try it again next time.
                Err(Skip::Denied) => denied += 1,
                Err(Skip::NotKeyboard) => self.skipped.push(path),
            }
        }
        Ok(denied)
    }

    /// Read what `path` has; false if the device went away.
    fn read(&mut self, path: &PathBuf) -> bool {
        let Some(device) = self.devices.get_mut(path) else {
            return false;
        };
        let mut changes = Vec::new();
        match device.fetch_events() {
            Ok(events) => {
                for event in events {
                    if event.event_type() == EventType::KEY && event.code() == self.key.code() {
                        // 1 down, 0 up, 2 auto-repeat (ignored).
                        changes.push(event.value());
                    }
                }
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock => {}
            Err(_) => return false,
        }
        for value in changes {
            match value {
                1 => self.press.down(),
                0 => self.press.up(),
                _ => {}
            }
        }
        true
    }

    fn drop_device(&mut self, path: &PathBuf) {
        self.devices.remove(path);
        log::line(&format!("key: {} went away", path.display()));
        // A key held on a keyboard that vanished is never released.
        self.press.up();
    }

    fn run(mut self, stop: Arc<AtomicBool>) {
        let mut scanned = Instant::now();
        while !stop.load(Ordering::SeqCst) {
            if scanned.elapsed() > RESCAN {
                let _ = self.scan();
                scanned = Instant::now();
            }
            let paths: Vec<PathBuf> = self.devices.keys().cloned().collect();
            let mut fds: Vec<libc::pollfd> = paths
                .iter()
                .map(|p| libc::pollfd {
                    fd: self.devices[p].as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                })
                .collect();
            // Wakes for input, and at least twice a second to see `stop`.
            let ready = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, 500) };
            if ready <= 0 {
                if fds.is_empty() {
                    // poll returns at once without fds on some kernels.
                    std::thread::sleep(Duration::from_millis(500));
                }
                continue;
            }
            for (fd, path) in fds.iter().zip(&paths) {
                if fd.revents == 0 {
                    continue;
                }
                let gone = fd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0;
                if gone || !self.read(path) {
                    self.drop_device(path);
                }
            }
        }
    }
}

/// The watcher thread; dropping it stops watching.
pub struct Hotkey {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

/// Where the key comes from in this session.
enum Source {
    /// Keyboards in /dev/input (possibly none yet: the rescans find them).
    Evdev,
    /// The X server, as no keyboard in /dev/input can be read.
    X11(Box<xkey::XKeys>),
}

/// What to do when neither /dev/input nor an X server can tell the key.
const NO_ACCESS: &str = "hushkey may not read the keyboard (/dev/input/event*): add \
     yourself to the 'input' group (sudo usermod -aG input $USER) and log in again";

/// Decide where the key comes from, after `watcher` looked at /dev/input.
/// The one decision both the app and the Python handover go by
/// ([`readable`]): the handover must not switch the Python app off for a
/// program that then cannot see the key.
fn source(watcher: &mut Watcher) -> Result<Source, String> {
    let listed = watcher.scan();
    if !watcher.devices.is_empty() {
        return Ok(Source::Evdev);
    }
    // No keyboard there to read: on X11 the server can tell the key — with
    // /dev/input denied, missing (a container) or empty alike.
    let x11 = if xkey::available() {
        // X keycodes are evdev codes + 8 on every evdev-based server.
        match xkey::XKeys::open(u32::from(watcher.key.code()) + 8, IGNORED) {
            Ok(keys) => return Ok(Source::X11(Box::new(keys))),
            Err(e) => Some(e),
        }
    } else {
        None
    };
    let why = match listed {
        Err(e) => {
            format!("/dev/input: {e}; hushkey reads the key from the keyboard devices there")
        }
        Ok(denied) if denied > 0 => NO_ACCESS.to_string(),
        // Nothing denied, nothing there yet (a Bluetooth keyboard asleep):
        // the rescans find it.
        Ok(_) => return Ok(Source::Evdev),
    };
    Err(match x11 {
        Some(e) => format!("{why} ({e})"),
        None => why,
    })
}

impl Hotkey {
    /// Watch for `key` on every keyboard; `on_key` runs on the watcher
    /// thread.
    pub fn install(
        key: KeyCode,
        on_key: impl Fn(KeyEvent) + Send + 'static,
    ) -> Result<Hotkey, String> {
        let mut watcher = Watcher::new(key, Box::new(on_key));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = match source(&mut watcher)? {
            Source::Evdev => {
                if watcher.devices.is_empty() {
                    log::line("key: no keyboard found yet; looking again every 2 s");
                }
                // The caps restore and paste chords come from this
                // keyboard; the desktop needs a moment to take a new one
                // in, so make it now.
                uinput::prepare();
                std::thread::Builder::new()
                    .name("hushkey-hotkey".into())
                    .spawn({
                        let stop = stop.clone();
                        move || watcher.run(stop)
                    })
            }
            Source::X11(keys) => {
                log::line(
                    "key: no keyboard in /dev/input can be read; the X server reports the key",
                );
                let mut press = watcher.press;
                std::thread::Builder::new()
                    .name("hushkey-hotkey".into())
                    .spawn({
                        let stop = stop.clone();
                        move || keys.run(stop, |down| if down { press.down() } else { press.up() })
                    })
            }
        }
        .map_err(|e| e.to_string())?;
        Ok(Hotkey {
            stop,
            thread: Some(thread),
        })
    }
}

/// Whether the push-to-talk key can be read in this session, decided as
/// [`Hotkey::install`] decides it. `key` is any key: keyboards have them
/// all, and the X server reports every one.
pub fn readable(key: KeyCode) -> Result<(), String> {
    source(&mut Watcher::new(key, Box::new(|_| {}))).map(drop)
}

impl Drop for Hotkey {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{self, KEYS};

    #[test]
    fn every_config_key_has_an_evdev_code() {
        for (name, _) in KEYS {
            let vk = config::virtual_key(name).unwrap();
            assert!(key_code(vk).is_some(), "{name}");
        }
        assert_eq!(
            key_code(config::virtual_key("Caps Lock").unwrap()),
            Some(KeyCode::KEY_CAPSLOCK)
        );
    }
}
