//! hushkey on Windows: a low-level keyboard hook for the key, the clipboard
//! and SendInput to insert, a GDI overlay, the Run key for autostart, and
//! the Win32 message loop the tray lives on.

pub mod autostart;
mod clipboard;
mod hotkey;
mod insert;
mod overlay;

use std::path::Path;
use std::process::ExitCode;
use std::ptr;
use std::time::Duration;

use windows_sys::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, MessageBoxW, MsgWaitForMultipleObjects, PeekMessageW, TranslateMessage,
    MB_ICONERROR, MB_OK, MSG, PM_REMOVE, QS_ALLINPUT, WM_QUIT,
};

use crate::config;
use crate::log;
pub use hotkey::{forget_press, since_last_seen, KeyEvent};
pub use insert::Restore;
pub use overlay::Overlay;

const VK_CAPITAL: u32 = 0x14;

pub fn init() {
    // Physical pixels everywhere: the overlay sizes itself per monitor.
    unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
}

pub fn fatal(message: &str) -> ExitCode {
    log::line(&format!("fatal: {message}"));
    let text: Vec<u16> = format!("hushkey could not start:\n\n{message}")
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let title: Vec<u16> = "hushkey".encode_utf16().chain(Some(0)).collect();
    unsafe {
        MessageBoxW(
            ptr::null_mut(),
            text.as_ptr(),
            title.as_ptr(),
            MB_OK | MB_ICONERROR,
        )
    };
    ExitCode::FAILURE
}

/// True if another hushkey (Rust) already runs in this session. The mutex
/// handle is kept for the process's life.
pub fn already_running() -> bool {
    let name: Vec<u16> = "Local\\hushkey-rs".encode_utf16().chain(Some(0)).collect();
    unsafe {
        let handle = CreateMutexW(ptr::null(), 0, name.as_ptr());
        !handle.is_null() && GetLastError() == ERROR_ALREADY_EXISTS
    }
}

/// Handle the window messages that are waiting (the tray and its menu live
/// on them). False once the app is asked to quit.
pub fn pump() -> bool {
    unsafe {
        let mut msg: MSG = std::mem::zeroed();
        while PeekMessageW(&mut msg, ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
            if msg.message == WM_QUIT {
                return false;
            }
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    true
}

/// Sleep until a window message arrives, at most `timeout`.
pub fn wait(timeout: Duration) {
    unsafe {
        MsgWaitForMultipleObjects(0, ptr::null(), 0, timeout.as_millis() as u32, QS_ALLINPUT)
    };
}

/// The push-to-talk key, watched while this lives.
pub struct Hotkey(#[allow(dead_code)] hotkey::Hotkey);

impl Hotkey {
    pub fn install(
        key: &str,
        on_key: impl Fn(KeyEvent) + Send + 'static,
    ) -> Result<Hotkey, String> {
        let vk = config::virtual_key(key).ok_or_else(|| format!("unknown key {key:?}"))?;
        hotkey::Hotkey::install(vk, on_key).map(Hotkey)
    }
}

/// The key was only tapped: the hook swallowed it, so Caps Lock still
/// toggles.
pub fn tapped(key: &str) {
    if config::virtual_key(key) == Some(VK_CAPITAL) {
        insert::tap_key(VK_CAPITAL as u16);
    }
}

/// Put `text` into the focused window: pasted (typed if that fails), or
/// typed.
pub fn insert(text: &str, paste: bool, restore: bool) -> Result<Restore, String> {
    if paste {
        insert::paste(text, restore).or_else(|e| {
            log::line(&format!("paste failed ({e}); typing instead"));
            insert::type_text(text).map(|()| Restore::Nothing)
        })
    } else {
        insert::type_text(text).map(|()| Restore::Nothing)
    }
}

/// Put `text` on the clipboard (a history entry the user picked).
pub fn copy(text: &str) -> Result<(), String> {
    clipboard::write_text(text, false)
}

pub fn open_folder(path: &Path) {
    let _ = std::process::Command::new("explorer.exe").arg(path).spawn();
}
