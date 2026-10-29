//! Putting text into the focused window: paste (clipboard + Ctrl+V from
//! hushkey's uinput keyboard) or type (xdotool, X11 only).
//!
//! As the Python hushkey: Wayland always pastes (a uinput keyboard types
//! by key position, so on a German layout `z` would come out as `y`); X11
//! terminals are typed into, as xterm and urxvt have no paste chord in
//! common with the others; and text with characters the X11 layout lacks
//! is pasted, never typed.

use std::process::{Command, Stdio};
use std::time::Duration;

use evdev::KeyCode;
use x11rb::protocol::xproto::{AtomEnum, ConnectionExt, Window};

use super::{clipboard, on_path, uinput, wayland};

/// How long the target gets to fetch the dictation from the clipboard
/// before the previous text comes back. Linux tells no one when a paste
/// fetched it, so this is a fixed wait (the Python app's, plus margin).
const SETTLE: Duration = Duration::from_millis(500);

/// What became of the previous clipboard.
#[derive(Debug, PartialEq, Eq)]
pub enum Restore {
    Restored,
    /// There was no text to restore, or restoring was off.
    Nothing,
    /// Something new was copied meanwhile; it stays.
    CopiedOver,
    Failed(String),
}

/// X11 WM_CLASS names of terminals.
const TERMINALS: &[&str] = &[
    "alacritty",
    "contour",
    "cool-retro-term",
    "deepin-terminal",
    "foot",
    "ghostty",
    "gnome-terminal",
    "gnome-terminal-server",
    "guake",
    "hyper",
    "kitty",
    "konsole",
    "lxterminal",
    "mate-terminal",
    "pantheon-terminal",
    "ptyxis",
    "qterminal",
    "st",
    "tabby",
    "terminator",
    "terminology",
    "tilix",
    "tilda",
    "urxvt",
    "uxterm",
    "wezterm",
    "xfce4-terminal",
    "xterm",
    "yakuake",
];

/// Whether the focused window is a terminal. X11 only: Wayland does not
/// say which window has the focus.
pub fn terminal_focused() -> bool {
    !wayland()
        && std::env::var_os("DISPLAY").is_some()
        && x11_focus_class().is_some_and(|c| {
            c.iter()
                .any(|name| TERMINALS.contains(&name.to_lowercase().as_str()))
        })
}

/// WM_CLASS (instance and class) of the window with the input focus. The
/// focus usually sits on a child of the client window (GTK and Qt hand it
/// to an inner window); only the client window has WM_CLASS, so walk up.
fn x11_focus_class() -> Option<Vec<String>> {
    let (conn, _) = x11rb::connect(None).ok()?;
    let mut window: Window = conn.get_input_focus().ok()?.reply().ok()?.focus;
    for _ in 0..16 {
        // 0 is None, 1 PointerRoot: no window has the focus.
        if window <= 1 {
            return None;
        }
        let class = conn
            .get_property(false, window, AtomEnum::WM_CLASS, AtomEnum::STRING, 0, 256)
            .ok()?
            .reply()
            .ok()?;
        if !class.value.is_empty() {
            return Some(
                class
                    .value
                    .split(|b| *b == 0)
                    .filter(|s| !s.is_empty())
                    .map(|s| String::from_utf8_lossy(s).into_owned())
                    .collect(),
            );
        }
        let tree = conn.query_tree(window).ok()?.reply().ok()?;
        if tree.parent == tree.root || tree.parent == window {
            return None;
        }
        window = tree.parent;
    }
    None
}

/// The keysym that types `c`: Latin-1 has its own, the rest of Unicode is
/// 0x01000000 + code point. (Legacy keysyms of other scripts are not
/// looked up: such text is pasted instead, which is always safe.)
fn keysym(c: char) -> u32 {
    match c as u32 {
        cp @ (0x20..=0x7e | 0xa0..=0xff) => cp,
        cp => 0x0100_0000 | cp,
    }
}

/// Whether every character of `text` is on the X11 keyboard layout.
/// xdotool types the others by remapping a key for a moment, which races
/// the target's keymap update: they come out wrong or not at all.
fn on_layout(text: &str) -> Result<bool, String> {
    let (conn, _) = x11rb::connect(None).map_err(|e| format!("X11: {e}"))?;
    let setup = x11rb::connection::Connection::setup(&conn);
    let (min, max) = (setup.min_keycode, setup.max_keycode);
    let map = conn
        .get_keyboard_mapping(min, max - min + 1)
        .map_err(|e| e.to_string())?
        .reply()
        .map_err(|e| e.to_string())?;
    let keysyms: std::collections::HashSet<u32> = map.keysyms.into_iter().collect();
    Ok(text
        .chars()
        .all(|c| matches!(c, '\n' | '\t') || keysyms.contains(&keysym(c))))
}

/// Type `text` with xdotool (X11): it types by character through the
/// current keyboard layout. Text with characters not on the layout is
/// refused (the caller pastes it).
pub fn type_text(text: &str) -> Result<(), String> {
    if wayland() || std::env::var_os("DISPLAY").is_none() {
        return Err("typing works on X11 only".into());
    }
    if !on_path("xdotool") {
        return Err("typing needs xdotool (sudo apt install xdotool)".into());
    }
    let text = text.replace("\r\n", "\n");
    if !on_layout(&text)? {
        return Err("the text has characters the keyboard layout lacks".into());
    }
    let status = Command::new("xdotool")
        .args(["type", "--clearmodifiers", "--delay", "8", "--", &text])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| format!("xdotool: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("xdotool type failed ({status})"))
    }
}

/// Press `keys` (evdev codes) on the uinput keyboard, or, if there is
/// none, `keysyms` through xdotool on X11. xdotool's keys go through the
/// X server, not the kernel, so the key watcher never sees them either.
pub fn chord(keys: &[KeyCode], keysyms: &str) -> Result<(), String> {
    uinput::chord(keys).or_else(|e| {
        if !wayland() && on_path("xdotool") {
            let status = Command::new("xdotool")
                .args(["key", keysyms])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map_err(|x| format!("{e}; xdotool: {x}"))?;
            if status.success() {
                return Ok(());
            }
        }
        Err(e)
    })
}

/// Ctrl+V; Ctrl+Shift+V into X11 terminals (which get pasted text only
/// when it cannot be typed).
fn paste_chord() -> Result<(), String> {
    if terminal_focused() {
        chord(
            &[
                KeyCode::KEY_LEFTCTRL,
                KeyCode::KEY_LEFTSHIFT,
                KeyCode::KEY_V,
            ],
            "ctrl+shift+v",
        )
    } else {
        chord(&[KeyCode::KEY_LEFTCTRL, KeyCode::KEY_V], "ctrl+v")
    }
}

/// Paste `text` into the focused window. With `restore`, the previous
/// clipboard text comes back once the target has had time to fetch the
/// dictation. An error means nothing was pasted (the caller may type
/// instead).
pub fn paste(text: &str, restore: bool) -> Result<Restore, String> {
    let previous = if restore {
        clipboard::read_text()
    } else {
        None
    };
    if let Err(e) = clipboard::write_text(text) {
        // The attempt may have emptied the clipboard: put the old text back.
        if let Some(previous) = &previous {
            if clipboard::read_text().is_none() {
                let _ = clipboard::write_text(previous);
            }
        }
        return Err(e);
    }
    if let Err(e) = paste_chord() {
        if let Some(previous) = &previous {
            let _ = clipboard::write_text(previous);
        }
        return Err(e);
    }
    let Some(previous) = previous else {
        return Ok(Restore::Nothing); // the dictation stays pasteable
    };
    std::thread::sleep(SETTLE);
    if !clipboard::holds(text) {
        return Ok(Restore::CopiedOver);
    }
    Ok(match clipboard::write_text(&previous) {
        Ok(()) => Restore::Restored,
        Err(e) => Restore::Failed(e),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_names_are_lowercase() {
        for name in TERMINALS {
            assert_eq!(*name, name.to_lowercase());
        }
    }

    /// A focused GTK entry; `insert` runs off the main thread while GTK
    /// keeps serving the window. Returns what the entry got.
    fn into_entry(insert: impl FnOnce() + Send + 'static) -> String {
        use gtk::prelude::*;
        gtk::gdk::set_allowed_backends("x11");
        gtk::init().unwrap();
        let window = gtk::Window::new(gtk::WindowType::Toplevel);
        let entry = gtk::Entry::new();
        window.add(&entry);
        window.show_all();
        window.present();
        let pump = |ms: u64| {
            let until = std::time::Instant::now() + Duration::from_millis(ms);
            while std::time::Instant::now() < until {
                while gtk::events_pending() {
                    gtk::main_iteration_do(false);
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        };
        pump(1000);
        assert!(!terminal_focused());
        let worker = std::thread::spawn(insert);
        while !worker.is_finished() {
            pump(20);
        }
        worker.join().unwrap();
        pump(300);
        entry.text().to_string()
    }

    #[test]
    #[ignore = "X11 only (run with WAYLAND_DISPLAY unset), needs xdotool; types into a window"]
    fn x11_pastes_and_restores_the_clipboard() {
        clipboard::write_text("before").unwrap();
        let got = into_entry(|| {
            assert_eq!(paste("grüße 你好", true), Ok(Restore::Restored));
        });
        assert_eq!(got, "grüße 你好");
        assert_eq!(clipboard::read_text().as_deref(), Some("before"));
    }

    #[test]
    #[ignore = "X11 only (run with WAYLAND_DISPLAY unset), needs xdotool; types into a window"]
    fn x11_types_and_pastes_what_the_layout_lacks() {
        let got = into_entry(|| {
            type_text("Hallo Welt, 42! ").unwrap();
            // Not on a US layout: typing refuses, insert pastes it.
            assert!(crate::linux::insert("你好", false, false).is_ok());
        });
        assert_eq!(got, "Hallo Welt, 42! 你好");
    }
}
