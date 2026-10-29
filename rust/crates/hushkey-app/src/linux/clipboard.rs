//! The clipboard: wl-clipboard on Wayland, the X11 CLIPBOARD (arboard)
//! elsewhere, or where wl-clipboard is missing (XWayland passes it on).
//!
//! A clipboard on Linux belongs to a running client that hands the text
//! out on request. On Wayland that is a `wl-copy` left running: it exits
//! by itself once something else is copied, and must never be killed
//! (the compositor would be left pointing at a dead owner). On X11 it is
//! arboard's thread in this process.

use std::io::Write;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::{on_path, wayland};

/// How long `wl-copy` must survive to count as the clipboard's owner (it
/// exits at once without a compositor).
const HANDOFF: Duration = Duration::from_millis(200);
/// The most a `wl-paste` may take; a stuck one would hold up the paste.
const READ_TIMEOUT: Duration = Duration::from_secs(3);

/// The `wl-copy` that owns the clipboard, as far as hushkey knows.
static WL_COPY: Mutex<Option<Child>> = Mutex::new(None);
/// The X11 clipboard; kept for the process's life, as it serves the text.
static X11: Mutex<Option<arboard::Clipboard>> = Mutex::new(None);

fn use_wl_clipboard() -> bool {
    wayland() && on_path("wl-copy") && on_path("wl-paste")
}

fn with_x11<T>(f: impl FnOnce(&mut arboard::Clipboard) -> Result<T, String>) -> Result<T, String> {
    let mut clipboard = X11.lock().unwrap();
    if clipboard.is_none() {
        *clipboard = Some(arboard::Clipboard::new().map_err(|e| format!("clipboard: {e}"))?);
    }
    f(clipboard.as_mut().expect("just made"))
}

/// The clipboard's text, if it holds text.
pub fn read_text() -> Option<String> {
    if use_wl_clipboard() {
        wl_paste()
    } else {
        with_x11(|c| c.get_text().map_err(|e| e.to_string())).ok()
    }
}

fn wl_paste() -> Option<String> {
    // Text only: an image restored as text would be garbage.
    let mut child = Command::new("wl-paste")
        .args(["--no-newline", "--type", "text/plain"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut stdout, &mut bytes).map(|_| bytes)
    });
    let deadline = Instant::now() + READ_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                // wl-paste owns nothing: stopping it harms no one.
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    let bytes = reader.join().ok()?.ok()?;
    if !status.success() {
        return None; // empty, or no text on it
    }
    String::from_utf8(bytes).ok()
}

/// Put `text` on the clipboard, where it stays until something else is
/// copied.
pub fn write_text(text: &str) -> Result<(), String> {
    if use_wl_clipboard() {
        wl_copy(text)
    } else {
        with_x11(|c| c.set_text(text).map_err(|e| format!("clipboard: {e}")))
    }
}

fn wl_copy(text: &str) -> Result<(), String> {
    let mut owner = WL_COPY.lock().unwrap();
    // The previous one exits by itself once the new one owns the
    // clipboard; reap it if it already has, never signal it.
    if let Some(mut previous) = owner.take() {
        if previous.try_wait().ok().flatten().is_none() {
            std::thread::spawn(move || {
                let _ = previous.wait();
            });
        }
    }
    let mut child = {
        use std::os::unix::process::CommandExt;
        Command::new("wl-copy")
            // This process stays the owner (not a fork), so whether it
            // still is can be seen.
            .args(["--foreground", "--type", "text/plain"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            // Not in hushkey's process group: a Ctrl+C on hushkey in a
            // terminal must not take the clipboard with it.
            .process_group(0)
            .spawn()
            .map_err(|e| format!("wl-copy: {e}"))?
    };
    let mut stdin = child.stdin.take().expect("piped");
    stdin
        .write_all(text.as_bytes())
        .map_err(|e| format!("wl-copy would not take the text: {e}"))?;
    drop(stdin);
    std::thread::sleep(HANDOFF);
    if let Ok(Some(status)) = child.try_wait() {
        return Err(format!(
            "wl-copy exited at once ({status}): the text is not on the clipboard"
        ));
    }
    *owner = Some(child);
    Ok(())
}

/// Whether the clipboard still holds `text` as hushkey put it there
/// (nothing was copied over it since).
pub fn holds(text: &str) -> bool {
    if use_wl_clipboard() {
        // wl-copy exits when it loses the clipboard.
        WL_COPY
            .lock()
            .unwrap()
            .as_mut()
            .is_some_and(|c| c.try_wait().ok().flatten().is_none())
    } else {
        with_x11(|c| c.get_text().map_err(|e| e.to_string())).is_ok_and(|t| t == text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "changes the desktop's clipboard; run with --ignored (Wayland or X11)"]
    fn text_goes_onto_the_clipboard_and_is_told_from_a_later_copy() {
        let before = read_text();
        write_text("hushkey test: grüße 你好").unwrap();
        assert_eq!(read_text().as_deref(), Some("hushkey test: grüße 你好"));
        assert!(holds("hushkey test: grüße 你好"));
        // Someone else copies (another process, as it would be).
        if use_wl_clipboard() {
            let mut other = Command::new("wl-copy")
                .arg("from elsewhere")
                .spawn()
                .unwrap();
            other.wait().unwrap();
            std::thread::sleep(Duration::from_millis(300));
        } else {
            let mut other = arboard::Clipboard::new().unwrap();
            other.set_text("from elsewhere").unwrap();
        }
        assert_eq!(read_text().as_deref(), Some("from elsewhere"));
        assert!(!holds("hushkey test: grüße 你好"));
        if let Some(before) = before {
            write_text(&before).unwrap();
        }
    }
}
