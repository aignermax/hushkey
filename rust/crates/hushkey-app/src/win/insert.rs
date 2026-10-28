//! Putting text into the focused window: paste (clipboard + chord) or type.

use std::time::Duration;

use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE,
    VIRTUAL_KEY, VK_CONTROL, VK_INSERT, VK_RETURN, VK_SHIFT,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{GetClassNameW, GetForegroundWindow};

use super::clipboard::{self, window_pid, Offer};

/// How long the target may take to fetch a pasted dictation before the
/// previous clipboard is given up rather than restored.
const FETCH_TIMEOUT: Duration = Duration::from_secs(5);
/// After the first fetch: some apps read once to inspect and again to
/// insert; the second read must still get the dictation.
const SETTLE: Duration = Duration::from_millis(400);

fn key(vk: VIRTUAL_KEY, scan: u16, flags: u32) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn send(inputs: &[INPUT]) -> Result<(), String> {
    let sent = unsafe {
        SendInput(
            inputs.len() as u32,
            inputs.as_ptr(),
            std::mem::size_of::<INPUT>() as i32,
        )
    };
    if sent as usize == inputs.len() {
        Ok(())
    } else {
        // Blocked by UIPI: the target runs elevated and hushkey does not.
        Err("the focused window refused the key events (is it running as administrator?)".into())
    }
}

/// Type `text` as Unicode key events.
pub fn type_text(text: &str) -> Result<(), String> {
    let mut inputs = Vec::new();
    for line_or_break in text.split_inclusive('\n') {
        let (line, newline) = match line_or_break.strip_suffix('\n') {
            Some(line) => (line, true),
            None => (line_or_break, false),
        };
        for unit in line.trim_end_matches('\r').encode_utf16() {
            inputs.push(key(0, unit, KEYEVENTF_UNICODE));
            inputs.push(key(0, unit, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP));
        }
        if newline {
            inputs.push(key(VK_RETURN, 0, 0));
            inputs.push(key(VK_RETURN, 0, KEYEVENTF_KEYUP));
        }
    }
    // In blocks, so a long dictation does not flood the input queue.
    for block in inputs.chunks(200) {
        send(block)?;
        std::thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}

const TERMINAL_CLASSES: &[&str] = &["ConsoleWindowClass", "CASCADIA_HOSTING_WINDOW_CLASS"];
const TERMINAL_PROCESSES: &[&str] = &[
    "windowsterminal.exe",
    "wezterm-gui.exe",
    "alacritty.exe",
    "mintty.exe",
    "conemu.exe",
    "conemu64.exe",
    "tabby.exe",
    "hyper.exe",
];

/// Whether the focused window is a terminal: they all paste on
/// Shift+Insert, not all on Ctrl+V.
pub fn foreground_is_terminal() -> bool {
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_null() {
            return false;
        }
        let mut class = [0u16; 256];
        let n = GetClassNameW(hwnd, class.as_mut_ptr(), class.len() as i32);
        let class = String::from_utf16_lossy(&class[..n.max(0) as usize]);
        if TERMINAL_CLASSES.contains(&class.as_str()) {
            return true;
        }
        let Some(pid) = window_pid(hwnd) else {
            return false;
        };
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return false;
        }
        let mut path = [0u16; 512];
        let mut len = path.len() as u32;
        let ok =
            QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, path.as_mut_ptr(), &mut len);
        CloseHandle(process);
        if ok == 0 {
            return false;
        }
        let path = String::from_utf16_lossy(&path[..len as usize]).to_lowercase();
        let exe = path.rsplit('\\').next().unwrap_or("");
        TERMINAL_PROCESSES.contains(&exe)
    }
}

fn paste_chord() -> Result<(), String> {
    let modifier = if foreground_is_terminal() {
        VK_SHIFT
    } else {
        VK_CONTROL
    };
    let letter = if modifier == VK_SHIFT {
        VK_INSERT
    } else {
        b'V' as VIRTUAL_KEY
    };
    send(&[
        key(modifier, 0, 0),
        key(letter, 0, 0),
        key(letter, 0, KEYEVENTF_KEYUP),
        key(modifier, 0, KEYEVENTF_KEYUP),
    ])
}

/// What became of the previous clipboard.
#[derive(Debug, PartialEq, Eq)]
pub enum Restore {
    Restored,
    /// There was no text to restore, or restoring was off.
    Nothing,
    /// The target did not fetch the dictation in time: restoring could
    /// let a late fetch paste the previous text.
    NotFetched,
    /// Something new was copied meanwhile; it stays.
    CopiedOver,
    Failed(String),
}

/// Paste `text` into the focused window. With `restore`, the previous
/// clipboard text comes back once the target has fetched the dictation.
/// An error means nothing was pasted (the caller may type instead).
pub fn paste(text: &str, restore: bool) -> Result<Restore, String> {
    let previous = if restore {
        clipboard::read_text()
    } else {
        None
    };
    let offer = match Offer::new(text) {
        Ok(offer) => offer,
        Err(e) => {
            // The offer may have emptied the clipboard before failing.
            if let Some(previous) = &previous {
                if clipboard::read_text().is_none() {
                    let _ = clipboard::write_text(previous, true);
                }
            }
            return Err(e);
        }
    };
    offer.arm();
    if let Err(e) = paste_chord() {
        offer.close();
        if let Some(previous) = &previous {
            let _ = clipboard::write_text(previous, true);
        }
        return Err(e);
    }
    let Some(previous) = previous else {
        offer.close(); // closing keeps the text pasteable
        return Ok(Restore::Nothing);
    };
    if !offer.wait_fetched(FETCH_TIMEOUT) {
        offer.close();
        return Ok(Restore::NotFetched);
    }
    std::thread::sleep(SETTLE);
    if !offer.owned() {
        offer.close();
        return Ok(Restore::CopiedOver);
    }
    let restored = clipboard::write_text(&previous, true);
    offer.close();
    Ok(match restored {
        Ok(()) => Restore::Restored,
        Err(e) => Restore::Failed(e),
    })
}
