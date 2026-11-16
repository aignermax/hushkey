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

/// The program behind the focused window when it runs as administrator
/// and hushkey does not; None when it is within reach, or cannot be told.
pub fn foreground_out_of_reach() -> Option<String> {
    static SELF_ELEVATED: OnceLock<bool> = OnceLock::new();
    if *SELF_ELEVATED.get_or_init(|| unsafe { elevated(GetCurrentProcess()) } == Some(true)) {
        return None;
    }
    unsafe {
        let window = GetForegroundWindow();
        if window.is_null() {
            return None;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(window, &mut pid);
        if pid == 0 {
            return None;
        }
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return None;
        }
        let name = (elevated(process) == Some(true)).then(|| program_name(process));
        CloseHandle(process);
        name
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
    fn a_normal_process_is_not_elevated_or_cannot_be_told() {
        // CI runners may run elevated; either way this one answers.
        assert!(unsafe { elevated(GetCurrentProcess()) }.is_some());
    }
}
