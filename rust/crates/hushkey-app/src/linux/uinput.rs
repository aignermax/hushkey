//! hushkey's own virtual keyboard (/dev/uinput) for the paste chord and
//! the Caps Lock restore. It sits below the display server, so X11 and
//! every Wayland compositor take its keys like a real keyboard's. Writing
//! /dev/uinput needs the Python installer's udev rule (group `input`).
//!
//! Keys are sent by position (evdev codes), not by character: fine for
//! chords like Ctrl+V on the usual layouts, useless for typing text.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use evdev::uinput::VirtualDevice;
use evdev::{AttributeSet, EventType, InputEvent, KeyCode};

use crate::log;

/// The device's name; the key watcher ignores it.
pub const NAME: &str = "hushkey virtual keyboard";
/// A new input device is taken in by the desktop (udev, libinput) only
/// after a moment; keys sent before that are lost.
const SETTLE: Duration = Duration::from_millis(400);
/// Between the events of a chord: some apps miss a modifier pressed in the
/// same instant as the key.
const GAP: Duration = Duration::from_millis(12);

struct Keyboard {
    device: VirtualDevice,
    created: Instant,
}

static KEYBOARD: Mutex<Option<Keyboard>> = Mutex::new(None);

fn create() -> Result<Keyboard, String> {
    // Every ordinary key, so libinput and the desktop see a full keyboard.
    let keys: AttributeSet<KeyCode> = (1..=248).map(KeyCode::new).collect();
    let device = VirtualDevice::builder()
        .and_then(|b| b.name(NAME).with_keys(&keys))
        .and_then(|b| b.build())
        .map_err(|e| {
            format!(
                "/dev/uinput: {e} (hushkey needs write access to it: the 'input' group \
                 and the udev rule from the installer)"
            )
        })?;
    Ok(Keyboard {
        device,
        created: Instant::now(),
    })
}

/// Make the keyboard now, so it is known to the desktop by the time it is
/// first needed.
pub fn prepare() {
    let mut keyboard = KEYBOARD.lock().unwrap();
    if keyboard.is_none() {
        match create() {
            Ok(k) => *keyboard = Some(k),
            Err(e) => log::line(&e),
        }
    }
}

fn with_keyboard(f: impl FnOnce(&mut VirtualDevice) -> std::io::Result<()>) -> Result<(), String> {
    let mut keyboard = KEYBOARD.lock().unwrap();
    if keyboard.is_none() {
        *keyboard = Some(create()?);
    }
    let keyboard = keyboard.as_mut().expect("just made");
    if let Some(left) = SETTLE.checked_sub(keyboard.created.elapsed()) {
        std::thread::sleep(left);
    }
    f(&mut keyboard.device).map_err(|e| format!("virtual keyboard: {e}"))
}

fn key(device: &mut VirtualDevice, code: KeyCode, down: bool) -> std::io::Result<()> {
    device.emit(&[InputEvent::new(EventType::KEY.0, code.code(), down as i32)])?;
    std::thread::sleep(GAP);
    Ok(())
}

/// Press `keys` in order, release them in reverse.
pub fn chord(keys: &[KeyCode]) -> Result<(), String> {
    with_keyboard(|device| {
        for k in keys {
            key(device, *k, true)?;
        }
        for k in keys.iter().rev() {
            key(device, *k, false)?;
        }
        Ok(())
    })
}
