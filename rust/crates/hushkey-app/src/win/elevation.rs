//! Windows run as administrator. Windows keeps a normal program's input
//! away from them (UIPI): while one has the focus, the keyboard hook sees
//! no keys, not even the held push-to-talk key's release, and SendInput
//! types nothing into it. The clipboard still reaches it.

use std::path::Path;
use std::ptr;
use std::sync::OnceLock;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::Security::{
    GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, QueryFullProcessImageNameW,
    PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

/// Captured by the key hook. Opening the process and reading its token
/// are left to the inserter thread so the hook can return immediately.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InsertTarget {
    window: usize,
    pid: u32,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Delivery {
    Automatic,
    Clipboard,
    RustDesk,
    Elevated(String),
}

impl InsertTarget {
    pub fn capture() -> Self {
        unsafe {
            let window = GetForegroundWindow();
            let mut pid = 0;
            GetWindowThreadProcessId(window, &mut pid);
            Self {
                window: window as usize,
                pid,
            }
        }
    }

    fn delivery(self, current: Self, name: &str, elevated: bool) -> Delivery {
        if self.pid == 0 || self != current {
            Delivery::Clipboard
        } else if elevated {
            Delivery::Elevated(name.to_owned())
        } else if name.eq_ignore_ascii_case("rustdesk") {
            // A local clipboard read does not acknowledge a remote paste.
            // Keep a normal, synchronizable copy until the user pastes it.
            Delivery::RustDesk
        } else {
            Delivery::Automatic
        }
    }

    pub(super) fn resolve(self) -> Delivery {
        static SELF_ELEVATED: OnceLock<bool> = OnceLock::new();
        unsafe {
            let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, self.pid);
            if process.is_null() {
                return Delivery::Clipboard;
            }
            let own = *SELF_ELEVATED.get_or_init(|| elevated(GetCurrentProcess()) == Some(true));
            let out_of_reach = !own && elevated(process) == Some(true);
            let name = program_name(process);
            CloseHandle(process);
            self.delivery(Self::capture(), &name, out_of_reach)
        }
    }

    pub fn may_hide_key_release(self) -> Option<String> {
        match self.resolve() {
            Delivery::Elevated(app) => Some(app),
            Delivery::RustDesk => Some("RustDesk".into()),
            _ => None,
        }
    }
}

/// Whether `process` runs elevated; None when Windows does not say.
unsafe fn elevated(process: HANDLE) -> Option<bool> {
    let mut token: HANDLE = ptr::null_mut();
    if OpenProcessToken(process, TOKEN_QUERY, &mut token) == 0 {
        return None;
    }
    let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
    let mut len = 0u32;
    let ok = GetTokenInformation(
        token,
        TokenElevation,
        (&mut elevation as *mut TOKEN_ELEVATION).cast(),
        std::mem::size_of::<TOKEN_ELEVATION>() as u32,
        &mut len,
    );
    CloseHandle(token);
    (ok != 0).then_some(elevation.TokenIsElevated != 0)
}

/// "RustDesk" for C:\Program Files\RustDesk\RustDesk.exe.
unsafe fn program_name(process: HANDLE) -> String {
    let mut buf = [0u16; 1024];
    let mut len = buf.len() as u32;
    if QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len) == 0 {
        return "?".into();
    }
    let path = String::from_utf16_lossy(&buf[..len as usize]);
    Path::new(&path)
        .file_stem()
        .map_or_else(|| path.clone(), |s| s.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rustdesk_uses_a_persistent_copy_even_without_elevation() {
        let target = InsertTarget { window: 1, pid: 2 };
        assert_eq!(
            target.delivery(target, "RuStDeSk", false),
            Delivery::RustDesk
        );
        assert_eq!(
            target.delivery(target, "notepad", false),
            Delivery::Automatic
        );
        assert_eq!(
            target.delivery(target, "notepad", true),
            Delivery::Elevated("notepad".into())
        );
    }

    #[test]
    fn a_changed_window_after_release_gets_no_keys() {
        let target = InsertTarget { window: 1, pid: 2 };
        for current in [
            InsertTarget { window: 3, pid: 2 },
            InsertTarget { window: 1, pid: 3 },
        ] {
            assert_eq!(
                target.delivery(current, "notepad", false),
                Delivery::Clipboard
            );
        }
    }

    #[test]
    fn a_normal_process_is_not_elevated_or_cannot_be_told() {
        // CI runners may run elevated; either way this one answers.
        assert!(unsafe { elevated(GetCurrentProcess()) }.is_some());
    }
}
