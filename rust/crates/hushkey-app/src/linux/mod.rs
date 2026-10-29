//! hushkey on Linux, X11 and Wayland: the key read from /dev/input (evdev),
//! the clipboard and a uinput keyboard (or xdotool) to insert, a GTK
//! overlay, an XDG autostart entry, and the GTK main loop the tray lives
//! on.

pub mod autostart;
mod clipboard;
mod hotkey;
mod insert;
mod overlay;
pub mod setup;
mod uinput;

use std::cell::Cell;
use std::path::Path;
use std::process::ExitCode;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use gtk::prelude::*;
use gtk::{gdk, glib};

use crate::config;
use crate::log;
pub use hotkey::{forget_press, since_last_seen, KeyEvent};
pub use insert::Restore;
pub use overlay::Overlay;

/// Set by SIGTERM, SIGINT or SIGHUP: quit like from the menu.
static QUIT: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_: libc::c_int) {
    QUIT.store(true, Ordering::SeqCst);
}

pub fn init() {
    // The overlay must stay out of the focus and sit at the top of the
    // screen: Wayland lets a client do neither, X11 (XWayland too) both.
    // The tray is unaffected: it talks to the desktop over D-Bus.
    gdk::set_allowed_backends("x11,*");
    if let Err(e) = gtk::init() {
        log::line(&format!(
            "fatal: GTK could not start ({e}); hushkey needs a desktop session"
        ));
        std::process::exit(1);
    }
    for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
        unsafe { libc::signal(signal, on_signal as *const () as libc::sighandler_t) };
    }
}

pub fn fatal(message: &str) -> ExitCode {
    log::line(&format!("fatal: {message}"));
    let dialog = gtk::MessageDialog::new(
        None::<&gtk::Window>,
        gtk::DialogFlags::MODAL,
        gtk::MessageType::Error,
        gtk::ButtonsType::Ok,
        &format!("hushkey could not start:\n\n{message}"),
    );
    dialog.set_title("hushkey");
    dialog.run();
    dialog.close();
    pump();
    ExitCode::FAILURE
}

/// Handle the GTK events that are waiting (the tray and its menu live on
/// them). False once the app is asked to quit (a signal).
pub fn pump() -> bool {
    while gtk::events_pending() {
        gtk::main_iteration_do(false);
    }
    !QUIT.load(Ordering::SeqCst)
}

/// Sleep until a GTK event arrives, at most `timeout`.
pub fn wait(timeout: Duration) {
    let fired = Rc::new(Cell::new(false));
    let source = glib::timeout_add_local_once(timeout, {
        let fired = fired.clone();
        move || fired.set(true)
    });
    gtk::main_iteration_do(true);
    if !fired.get() {
        source.remove();
    }
}

/// The push-to-talk key, watched while this lives.
pub struct Hotkey(#[allow(dead_code)] hotkey::Hotkey);

impl Hotkey {
    pub fn install(
        key: &str,
        on_key: impl Fn(KeyEvent) + Send + 'static,
    ) -> Result<Hotkey, String> {
        let code = config::virtual_key(key)
            .and_then(hotkey::key_code)
            .ok_or_else(|| format!("unknown key {key:?}"))?;
        hotkey::Hotkey::install(code, on_key).map(Hotkey)
    }
}

/// Nothing to give back: the key is not swallowed on Linux, so a tapped
/// Caps Lock has toggled already.
pub fn tapped(_key: &str) {}

/// Put `text` into the focused window: pasted (typed if that fails), or
/// typed (pasted where typing is not possible: Wayland, or characters the
/// keyboard layout lacks). X11 terminals are typed into.
pub fn insert(text: &str, paste: bool, restore: bool) -> Result<Restore, String> {
    if paste && !insert::terminal_focused() {
        insert::paste(text, restore).or_else(|e| {
            log::line(&format!("paste failed ({e}); typing instead"));
            insert::type_text(text).map(|()| Restore::Nothing)
        })
    } else {
        insert::type_text(text)
            .map(|()| Restore::Nothing)
            .or_else(|e| {
                log::line(&format!("typing unavailable ({e}); pasting instead"));
                insert::paste(text, restore)
            })
    }
}

/// Put `text` on the clipboard (a history entry the user picked).
pub fn copy(text: &str) -> Result<(), String> {
    clipboard::write_text(text)
}

pub fn open_folder(path: &Path) {
    if let Ok(mut child) = std::process::Command::new("xdg-open").arg(path).spawn() {
        std::thread::spawn(move || child.wait());
    }
}

/// Whether the session is Wayland (GTK itself runs on XWayland there).
fn wayland() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some()
        && std::env::var("XDG_SESSION_TYPE").map_or(true, |t| t != "x11")
}

/// Whether `program` is on the PATH.
fn on_path(program: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|dir| {
            std::fs::metadata(dir.join(program)).is_ok_and(|m| {
                m.is_file()
                    && std::os::unix::fs::PermissionsExt::mode(&m.permissions()) & 0o111 != 0
            })
        })
    })
}

/// True if another hushkey (Rust) already runs for this user: it holds the
/// lock on `hushkey-rs.lock` in the runtime folder.
pub fn already_running() -> bool {
    let dir = dirs::runtime_dir().unwrap_or_else(config::state_dir);
    crate::unix::already_running(&dir.join("hushkey-rs.lock"))
}
