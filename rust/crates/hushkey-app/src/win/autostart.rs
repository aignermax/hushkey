//! Start with Windows: a value under the user's Run key, pointing at this
//! executable (no admin rights needed, removed by switching it off).

use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::System::Registry::{
    RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ,
};

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE: &str = "hushkey-rs";

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// The command the Run key starts: this executable, quoted.
fn command() -> Result<String, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    Ok(format!("\"{}\"", exe.display()))
}

/// Whether the Run key starts this executable.
pub fn enabled() -> bool {
    let (key, value) = (wide(RUN_KEY), wide(VALUE));
    let mut buf = [0u16; 1024];
    let mut size = std::mem::size_of_val(&buf) as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            buf.as_mut_ptr().cast(),
            &mut size,
        )
    };
    if status != ERROR_SUCCESS {
        return false;
    }
    let len = (size as usize / 2).saturating_sub(1);
    let stored = String::from_utf16_lossy(&buf[..len.min(buf.len())]);
    // An entry for another copy (an old install path) does not count.
    command().is_ok_and(|c| c.eq_ignore_ascii_case(&stored))
}

pub fn set(on: bool) -> Result<(), String> {
    let (key, value) = (wide(RUN_KEY), wide(VALUE));
    let status = if on {
        let data = wide(&command()?);
        unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                value.as_ptr(),
                REG_SZ,
                data.as_ptr().cast(),
                (data.len() * 2) as u32,
            )
        }
    } else {
        let status = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), value.as_ptr()) };
        if status == windows_sys::Win32::Foundation::ERROR_FILE_NOT_FOUND {
            ERROR_SUCCESS
        } else {
            status
        }
    };
    if status == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(format!("the Run key could not be changed (error {status})"))
    }
}
