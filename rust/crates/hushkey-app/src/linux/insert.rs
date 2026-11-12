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

use super::{clipboard, on_path, selection, uinput, wayland};
use crate::log;

/// How long the target's clipboard requests must stay quiet for the paste
/// to count as finished. On X11 this follows an observed fetch rather than
/// standing in for one; on Wayland, where no fetch can be seen, it is the
/// whole wait.
const SETTLE: Duration = Duration::from_millis(400);

/// How long the target gets to fetch the dictation at all. Generous: a busy
/// Electron app can take seconds, and restoring before it read would paste
/// the previous clipboard instead of the dictation.
const FETCH_TIMEOUT: Duration = Duration::from_secs(5);

/// What became of the previous clipboard.
#[derive(Debug, PartialEq, Eq)]
pub enum Restore {
    Restored,
    /// There was no text to restore, or restoring was off.
    Nothing,
    /// The target did not fetch the dictation in time: restoring could let
    /// a late fetch paste the previous text.
    NotFetched,
    /// Something new was copied meanwhile; it stays.
    CopiedOver,
    Failed(String),
}

impl Restore {
    /// Whether the text reached a window: a paste nobody fetched did not.
    pub fn arrived(&self) -> bool {
        *self != Restore::NotFetched
    }
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
/// none, through the X server's XTEST extension on X11 (xdotool's
/// `keysyms` as the last resort). Keys sent through the X server do not
/// reach the evdev key watcher, and the X11 one ignores the XTEST device.
pub fn chord(keys: &[KeyCode], keysyms: &str) -> Result<(), String> {
    uinput::chord(keys).or_else(|e| {
        if wayland() || std::env::var_os("DISPLAY").is_none() {
            return Err(e);
        }
        let e = match xtest_chord(keys) {
            Ok(()) => return Ok(()),
            Err(x) => format!("{e}; {x}"),
        };
        if on_path("xdotool") {
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

/// The keysym a chord key stands for, where the layout decides which key
/// that is: `v` is not at V's position on Dvorak. Caps Lock is not among
/// them: its restore has to press the very key the user held, wherever an
/// option like caps:swapescape put the Caps_Lock keysym.
fn chord_keysym(key: KeyCode) -> Option<u32> {
    Some(match key {
        KeyCode::KEY_LEFTCTRL => 0xffe3,  // Control_L
        KeyCode::KEY_LEFTSHIFT => 0xffe1, // Shift_L
        KeyCode::KEY_V => 0x0076,         // v
        _ => return None,
    })
}

/// The X keycode for `key` on the current layout: the key whose first
/// level carries its keysym (a shifted level would match the Caps_Lock
/// that shift:both_capslock puts on Shift). Where none does — a
/// Cyrillic layout has no `v` — the key at the same position (evdev code
/// + 8), which is what apps match Ctrl+V against on such layouts anyway.
fn x_keycode(key: KeyCode, min: u8, per_keycode: usize, keysyms: &[u32]) -> u8 {
    let by_position = u8::try_from(key.code() + 8).unwrap_or(0);
    let Some(wanted) = chord_keysym(key) else {
        return by_position;
    };
    if per_keycode == 0 {
        return by_position;
    }
    keysyms
        .chunks(per_keycode)
        .position(|syms| syms.first() == Some(&wanted))
        .and_then(|i| u8::try_from(usize::from(min) + i).ok())
        .unwrap_or(by_position)
}

/// Press `keys` in order and release them in reverse through XTEST, each
/// as the key the layout puts its character on (see [`x_keycode`]).
fn xtest_chord(keys: &[KeyCode]) -> Result<(), String> {
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{KEY_PRESS_EVENT, KEY_RELEASE_EVENT};
    use x11rb::protocol::xtest::ConnectionExt as _;
    let x = |e: &dyn std::fmt::Display| format!("XTEST: {e}");
    let (conn, screen) = x11rb::connect(None).map_err(|e| x(&e))?;
    let setup = conn.setup();
    let root = setup.roots[screen].root;
    let (min, max) = (setup.min_keycode, setup.max_keycode);
    conn.xtest_get_version(2, 2)
        .map_err(|e| x(&e))?
        .reply()
        .map_err(|e| x(&e))?;
    let mapping = conn
        .get_keyboard_mapping(min, max - min + 1)
        .map_err(|e| x(&e))?
        .reply()
        .map_err(|e| x(&e))?;
    let codes: Vec<u8> = keys
        .iter()
        .map(|k| {
            x_keycode(
                *k,
                min,
                usize::from(mapping.keysyms_per_keycode),
                &mapping.keysyms,
            )
        })
        .collect();
    let send = |kind: u8, code: u8| -> Result<(), String> {
        conn.xtest_fake_input(kind, code, x11rb::CURRENT_TIME, root, 0, 0, 0)
            .map_err(|e| x(&e))?;
        conn.flush().map_err(|e| x(&e))?;
        // As on the uinput keyboard: some apps miss a modifier pressed in
        // the same instant as the key.
        std::thread::sleep(Duration::from_millis(12));
        Ok(())
    };
    for code in &codes {
        send(KEY_PRESS_EVENT, *code)?;
    }
    for code in codes.iter().rev() {
        send(KEY_RELEASE_EVENT, *code)?;
    }
    // A round trip: the server has taken every key before this returns.
    conn.get_input_focus()
        .map_err(|e| x(&e))?
        .reply()
        .map_err(|e| x(&e))?;
    Ok(())
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

/// Paste from an offer hushkey serves itself, restoring once the target
/// has really fetched the dictation. `offer` already holds the text.
fn paste_watched(offer: &selection::Offer, previous: Option<String>) -> Result<Restore, String> {
    // Count requests only from here on, and attribute them to whoever has
    // the focus now — before the chord, so the target's first request
    // cannot arrive unattributed.
    offer.arm();
    if let Err(e) = paste_chord() {
        if let Some(previous) = &previous {
            let _ = clipboard::write_text(previous);
        }
        return Err(e);
    }
    let Some(previous) = previous else {
        return Ok(Restore::Nothing); // the dictation stays pasteable
    };
    if !offer.wait(FETCH_TIMEOUT) {
        return Ok(Restore::NotFetched);
    }
    if !offer.owned() {
        return Ok(Restore::CopiedOver);
    }
    Ok(match clipboard::write_text(&previous) {
        Ok(()) => Restore::Restored,
        Err(e) => Restore::Failed(e),
    })
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
    // On X11 hushkey serves the selection itself, so it sees the target
    // fetch the dictation and the restore can wait for the paste rather
    // than guess at it. Chromium/Electron apps read in several round trips
    // and drop the paste when the owner changes in between.
    if !wayland() && std::env::var_os("DISPLAY").is_some() {
        match selection::Offer::new(text, SETTLE) {
            Ok(offer) => return paste_watched(&offer, previous),
            // The selection would not come over: carry on with the plain
            // path, which is no worse than before.
            Err(e) => log::line(&format!("clipboard: {e}; pasting unwatched")),
        }
    }
    // Wayland does not say which window has the focus, and terminals take
    // no Ctrl+V. Shift+Insert pastes in both: GTK, Qt, browsers and editors
    // read the clipboard, terminals (VTE, kitty, foot, Alacritty) the
    // primary selection. So the dictation goes into both. The previous
    // primary is read before anything is written: a manager that syncs the
    // two would otherwise hand back the dictation as the previous one.
    let use_primary = wayland() && clipboard::primary_available();
    let previous_primary = if use_primary && restore {
        clipboard::read_primary()
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
    // One after the other, not at once: on GNOME each wl-copy takes the
    // focus for a moment to set its selection, and two would compete.
    let primary = use_primary
        && match clipboard::write_primary(text) {
            Ok(()) => true,
            Err(e) => {
                log::line(&format!("{e}; pasting with Ctrl+V"));
                false
            }
        };
    let previous_primary = previous_primary.filter(|_| primary);
    let pasted = if primary {
        chord(
            &[KeyCode::KEY_LEFTSHIFT, KeyCode::KEY_INSERT],
            "shift+Insert",
        )
    } else {
        paste_chord()
    };
    if let Err(e) = pasted {
        if let Some(previous) = &previous {
            let _ = clipboard::write_text(previous);
        }
        if let Some(previous) = &previous_primary {
            let _ = clipboard::write_primary(previous);
        }
        return Err(e);
    }
    if previous.is_none() && previous_primary.is_none() {
        return Ok(Restore::Nothing); // the dictation stays pasteable
    }
    std::thread::sleep(SETTLE);
    // Whether the clipboard still holds the dictation, asked before
    // anything is put back. A manager that syncs clipboard and primary took
    // the clipboard over when the primary was written (so hushkey's owner
    // is gone), but with the same text: that is no copy by the user.
    let clipboard_ours = previous.is_some()
        && (clipboard::holds(text) || clipboard::read_text().as_deref() == Some(text));
    if let Some(previous) = &previous_primary {
        if clipboard::holds_primary() {
            if let Err(e) = clipboard::write_primary(previous) {
                log::line(&format!("primary selection not restored: {e}"));
            }
        }
    }
    // The clipboard last: under a syncing manager the last write wins in
    // both, and the clipboard is what Ctrl+V pastes.
    let Some(previous) = previous else {
        return Ok(Restore::Nothing);
    };
    if !clipboard_ours {
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
    fn chord_keys_follow_the_layout() {
        // Keycodes 8.. with two keysyms each: 8 is `k`, 9 is `v` (as on
        // Dvorak, where `v` is not at V's position, evdev 47 + 8 = 55).
        let keysyms = [0x6b, 0x4b, 0x76, 0x56, 0xffe3, 0];
        assert_eq!(x_keycode(KeyCode::KEY_V, 8, 2, &keysyms), 9);
        assert_eq!(x_keycode(KeyCode::KEY_LEFTCTRL, 8, 2, &keysyms), 10);
        // Not on this layout at all: by position.
        assert_eq!(x_keycode(KeyCode::KEY_LEFTSHIFT, 8, 2, &keysyms), 42 + 8);
        // Only a key's first level counts: `V` (shifted) is not `v`.
        let shifted_only = [0x6b, 0x76, 0, 0];
        assert_eq!(x_keycode(KeyCode::KEY_V, 8, 2, &shifted_only), 47 + 8);
        // Caps Lock goes by position, whatever carries Caps_Lock.
        let swapped = [0xffe5, 0, 0, 0];
        assert_eq!(x_keycode(KeyCode::KEY_CAPSLOCK, 8, 2, &swapped), 58 + 8);
        // Keys that are not chord keys: by position.
        assert_eq!(x_keycode(KeyCode::KEY_F9, 8, 2, &keysyms), 67 + 8);
    }

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

    /// Wayland gives no way to focus a window from here: start one that
    /// takes keys (a terminal, a text field) first, so it has the focus.
    #[test]
    #[ignore = "Wayland only, needs wl-clipboard; pastes into the focused window"]
    fn wayland_pastes_and_restores_both_selections() {
        clipboard::write_text("before").unwrap();
        clipboard::write_primary("primary before").unwrap();
        assert_eq!(paste("grüße 你好", true), Ok(Restore::Restored));
        assert_eq!(clipboard::read_text().as_deref(), Some("before"));
        assert_eq!(clipboard::read_primary().as_deref(), Some("primary before"));
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
