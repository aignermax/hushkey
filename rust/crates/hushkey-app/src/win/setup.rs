//! Installing on Windows: the program in `%LOCALAPPDATA%\Programs\hushkey`,
//! a Start Menu shortcut, an entry in Settings > Apps, and the Python
//! hushkey switched off.

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub const ASSET: &str = "hushkey-windows-x86_64.exe";

const DETACHED_PROCESS: u32 = 0x0000_0008;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

fn local_app_data() -> PathBuf {
    dirs::data_local_dir().unwrap_or_else(std::env::temp_dir)
}

fn start_menu() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join(r"Microsoft\Windows\Start Menu\Programs")
}

/// Whether a package manager owns this program. Only the Linux .deb does;
/// here hushkey always installs and updates itself.
pub fn packaged() -> bool {
    false
}

/// Whether this program can work here, so that a Python hushkey may be
/// switched off for it. Nothing to check: the permissions it needs are
/// asked for when it starts.
pub fn ready() -> Result<(), String> {
    Ok(())
}

/// Whether a Python hushkey is still set up to start itself. Only the
/// Linux package can be installed without running `--install`, so here the
/// handover is always complete by the time the program runs.
pub fn python_still_autostarts() -> bool {
    false
}

/// Whether the Python hushkey's own entry in Settings > Apps is still
/// there (its Inno Setup install, per user).
pub fn python_package() -> bool {
    key_exists(PYTHON_UNINSTALL_KEY)
}

/// Run the Python hushkey's uninstaller, without a window or questions:
/// it stops the Python processes, removes its Python environment
/// (uninstall.ps1 -Purge) and its files, and its entry in Settings > Apps.
/// It needs no administrator: the install was per user. Never `Ok(false)`
/// here — nothing asks.
pub fn remove_python_package() -> Result<bool, String> {
    let command = get_string(PYTHON_UNINSTALL_KEY, "UninstallString").unwrap_or_default();
    let uninstaller = PathBuf::from(command.trim().trim_matches('"'));
    if command.trim().is_empty() || !uninstaller.is_file() {
        // Its folder was deleted by hand (the README said it could be):
        // only the entry is left, and nothing can run it.
        delete_key(PYTHON_UNINSTALL_KEY)?;
        crate::log::line("the old version's uninstaller was gone; its entry is removed");
        return Ok(true);
    }
    let status = Command::new(&uninstaller)
        .args(["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART"])
        .stdin(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .status()
        .map_err(|e| format!("{}: {e}", uninstaller.display()))?;
    if !status.success() {
        return Err(format!("{}: {status}", uninstaller.display()));
    }
    // Inno's uninstaller hands the work to a copy of itself in %TEMP% and
    // returns before it is done; removing a Python environment of some
    // GB, with a virus scanner looking at every file, takes minutes. Its
    // entry goes when it is done.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15 * 60);
    while key_exists(PYTHON_UNINSTALL_KEY) {
        if std::time::Instant::now() > deadline {
            return Err("the old version's uninstaller is still running; try again later".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    // The oldest installers used this program's own folder, and an
    // uninstall.ps1 of then took the Start Menu entry along: put this
    // program's entries back.
    let installed = installed_exe();
    if installed.is_file() {
        if let Err(e) = register(&installed) {
            crate::log::line(&format!("menu entry not restored: {e}"));
        }
    }
    Ok(true)
}

/// How to remove the Python hushkey's entry without hushkey.
pub const PYTHON_PACKAGE_BY_HAND: &str =
    "Settings > Apps > \"hushkey - old Python version (files only)\" > Uninstall";

/// Where the Python hushkey's installer put it.
pub fn python_install_dir() -> PathBuf {
    local_app_data().join(r"Programs\whisper-ptt")
}

/// Folders the Python hushkey may have lived in next to someone else's
/// files: the oldest installers (0.3, 0.4) used this program's own
/// folder. Only a Python environment in them is the Python hushkey's.
pub fn python_shared_dirs() -> Vec<PathBuf> {
    installed_exe()
        .parent()
        .map(Path::to_path_buf)
        .into_iter()
        .collect()
}

/// The Python hushkey's own data folder: the GPU engines it downloaded
/// and `acceleration.json`. This program's models are in it too
/// (`models`), and stay.
pub fn python_data_dir() -> PathBuf {
    local_app_data().join("hushkey")
}

/// Updates always install here: the program is in the user's folder.
pub fn updates_itself() -> bool {
    true
}

/// Whether `name` is the release asset an update installs here.
pub fn update_asset(name: &str) -> bool {
    name == ASSET
}

/// Put a downloaded and checked update in place (`_sha256`: checked
/// already, and the file is the user's own).
pub fn install_update(file: &Path, _sha256: &str) -> Result<Option<PathBuf>, String> {
    place(file).map(Some)
}

pub fn installed_exe() -> PathBuf {
    local_app_data().join(r"Programs\hushkey\hushkey.exe")
}

/// Settings > Apps lists per-user programs from here.
const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\hushkey";
/// The Python hushkey's Inno Setup entry (its AppId, per-user install).
const PYTHON_UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\{7F3A9C2E-4B6D-4E1A-9C5F-2D8E6A1B3F47}_is1";

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// Set a string (`Some`) or number value under HKCU\`key`, creating the key.
fn set_value(key: &str, name: &str, text: Option<&str>, number: u32) -> Result<(), String> {
    use windows_sys::Win32::System::Registry::{
        RegSetKeyValueW, HKEY_CURRENT_USER, REG_DWORD, REG_SZ,
    };
    let (key_w, name_w) = (wide(key), wide(name));
    let status = match text {
        Some(text) => {
            let data = wide(text);
            unsafe {
                RegSetKeyValueW(
                    HKEY_CURRENT_USER,
                    key_w.as_ptr(),
                    name_w.as_ptr(),
                    REG_SZ,
                    data.as_ptr().cast(),
                    (data.len() * 2) as u32,
                )
            }
        }
        None => unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                key_w.as_ptr(),
                name_w.as_ptr(),
                REG_DWORD,
                (&number as *const u32).cast(),
                4,
            )
        },
    };
    if status == windows_sys::Win32::Foundation::ERROR_SUCCESS {
        Ok(())
    } else {
        Err(format!("registry {key}\\{name}: error {status}"))
    }
}

/// A string value under HKCU\`key`.
fn get_string(key: &str, name: &str) -> Option<String> {
    use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_SZ};
    let (key_w, name_w) = (wide(key), wide(name));
    let mut size: u32 = 0;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key_w.as_ptr(),
            name_w.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut size,
        )
    };
    if status != windows_sys::Win32::Foundation::ERROR_SUCCESS || size == 0 {
        return None;
    }
    let mut buf = vec![0u16; (size as usize).div_ceil(2)];
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key_w.as_ptr(),
            name_w.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            buf.as_mut_ptr().cast(),
            &mut size,
        )
    };
    if status != windows_sys::Win32::Foundation::ERROR_SUCCESS {
        return None;
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..len]))
}

/// Whether HKCU\`key` exists.
fn key_exists(key: &str) -> bool {
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegOpenKeyExW, HKEY, HKEY_CURRENT_USER, KEY_READ,
    };
    let key_w = wide(key);
    let mut handle: HKEY = std::ptr::null_mut();
    let status =
        unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, key_w.as_ptr(), 0, KEY_READ, &mut handle) };
    if status == windows_sys::Win32::Foundation::ERROR_SUCCESS {
        unsafe { RegCloseKey(handle) };
        true
    } else {
        false
    }
}

/// The entry in Settings > Apps: its Uninstall button runs
/// `hushkey.exe --uninstall`.
fn register_app(exe: &Path) -> Result<(), String> {
    let exe_text = exe.display().to_string();
    let dir = exe.parent().unwrap_or(exe).display().to_string();
    let uninstall = format!("\"{exe_text}\" --uninstall");
    for (name, value) in [
        ("DisplayName", "hushkey"),
        ("DisplayVersion", env!("CARGO_PKG_VERSION")),
        ("Publisher", "hushkey"),
        ("URLInfoAbout", "https://hushkey.eu"),
        ("DisplayIcon", exe_text.as_str()),
        ("InstallLocation", dir.as_str()),
        ("UninstallString", uninstall.as_str()),
        ("QuietUninstallString", uninstall.as_str()),
    ] {
        set_value(UNINSTALL_KEY, name, Some(value), 0)?;
    }
    set_value(UNINSTALL_KEY, "NoModify", None, 1)?;
    set_value(UNINSTALL_KEY, "NoRepair", None, 1)?;
    Ok(())
}

fn unregister_app() -> Result<(), String> {
    delete_key(UNINSTALL_KEY)
}

/// HKCU\`key` and everything under it gone (fine if it was not there).
fn delete_key(key: &str) -> Result<(), String> {
    use windows_sys::Win32::System::Registry::{RegDeleteTreeW, HKEY_CURRENT_USER};
    let key_w = wide(key);
    let status = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, key_w.as_ptr()) };
    match status {
        windows_sys::Win32::Foundation::ERROR_SUCCESS
        | windows_sys::Win32::Foundation::ERROR_FILE_NOT_FOUND => Ok(()),
        _ => Err(format!("registry {key}: error {status}")),
    }
}

/// A program run without a console window, waited for.
fn quiet(program: &str, args: &[&str]) -> Result<(), String> {
    let status = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .status()
        .map_err(|e| format!("{program}: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{program} {}: {status}", args.join(" ")))
    }
}

/// Put `source` in place of the installed program. A running program can
/// be renamed but not overwritten, so the old one is moved aside first
/// (and removed by [`clean_after_update`] once it has exited).
pub fn place(source: &Path) -> Result<PathBuf, String> {
    let target = installed_exe();
    if source == target {
        return Ok(target);
    }
    let dir = target.parent().expect("an exe in a folder");
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let partial = target.with_extension("exe.part");
    std::fs::copy(source, &partial).map_err(|e| format!("{}: {e}", partial.display()))?;
    if target.exists() {
        clean_after_update();
        let aside = (0..100)
            .map(|n| target.with_extension(format!("exe.old{n}")))
            .find(|p| !p.exists())
            .ok_or("too many old copies of hushkey.exe")?;
        std::fs::rename(&target, &aside).map_err(|e| format!("{}: {e}", target.display()))?;
    }
    std::fs::rename(&partial, &target).map_err(|e| format!("{}: {e}", target.display()))?;
    Ok(target)
}

/// The Start Menu shortcut (it replaces the Python hushkey's, same name).
pub fn register(exe: &Path) -> Result<(), String> {
    let link = start_menu().join("hushkey.lnk");
    // Shortcuts are COM objects; PowerShell's WScript.Shell is the short way.
    // CreateShortcut opens one that exists: the Python app's had tray.py as
    // its argument and its own icon, neither of which may survive.
    let script = format!(
        "$l = (New-Object -ComObject WScript.Shell).CreateShortcut('{}'); \
         $l.TargetPath = '{}'; $l.WorkingDirectory = '{}'; \
         $l.Arguments = ''; $l.IconLocation = '{},0'; \
         $l.Description = 'Local push-to-talk dictation'; $l.Save()",
        ps_quote(&link),
        ps_quote(exe),
        ps_quote(exe.parent().unwrap_or(exe)),
        ps_quote(exe),
    );
    quiet(
        "powershell.exe",
        &["-NoProfile", "-NonInteractive", "-Command", &script],
    )?;
    // The development installer's entry, if any.
    let _ = std::fs::remove_file(start_menu().join("hushkey (Rust).lnk"));
    register_app(exe)
}

/// A path inside a single-quoted PowerShell string.
fn ps_quote(path: &Path) -> String {
    path.display().to_string().replace('\'', "''")
}

pub fn unregister() -> Result<(), String> {
    match std::fs::remove_file(start_menu().join("hushkey.lnk")) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.to_string()),
        _ => {}
    }
    unregister_app()
}

/// Remove the installed program once this process, which is it, has
/// exited: Windows cannot delete a running program, and Settings > Apps
/// runs the installed one to uninstall.
pub fn remove_after_exit(exe: &Path) -> Result<(), String> {
    let dir = exe.parent().unwrap_or(exe);
    // A few seconds for this process to end, then the file and its folder
    // (empty by then, except copies an update moved aside).
    let script = format!(
        "ping -n 4 127.0.0.1 >nul & del /f /q \"{}\" \"{}\\hushkey.exe.old*\" & rmdir \"{}\"",
        exe.display(),
        dir.display(),
        dir.display()
    );
    Command::new("cmd.exe")
        .raw_arg(format!("/d /c {script}"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)
        .spawn()
        .map(drop)
        .map_err(|e| format!("cmd.exe: {e}"))
}

/// Stop every other hushkey.exe of this user (the Python app runs as
/// pythonw.exe, so it is not among them).
pub fn stop_others() {
    let me = format!("PID ne {}", std::process::id());
    let _ = quiet(
        "taskkill.exe",
        &["/F", "/FI", "IMAGENAME eq hushkey.exe", "/FI", &me],
    );
    // Give Windows a moment to release the program file and the hook.
    std::thread::sleep(std::time::Duration::from_millis(500));
}

pub fn launch(exe: &Path, args: &[&str]) -> Result<(), String> {
    Command::new(exe)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)
        .spawn()
        .map(drop)
        .map_err(|e| format!("{}: {e}", exe.display()))
}

pub fn python_state_dir() -> PathBuf {
    local_app_data().join("whisper-ptt")
}

/// The Python hushkey off for good: its logon task or Startup shortcut
/// gone, its tray and daemon stopped. Its files stay; its Start Menu entry
/// is replaced by ours ([`register`]).
pub fn retire_python() -> Result<(), String> {
    let mut problems = Vec::new();
    // Absent when the Startup folder was used instead; not a problem.
    let _ = quiet("schtasks.exe", &["/Delete", "/F", "/TN", "whisper-ptt"]);
    if let Some(startup) = dirs::config_dir() {
        let link = startup.join(r"Microsoft\Windows\Start Menu\Programs\Startup\whisper-ptt.lnk");
        if let Err(e) = std::fs::remove_file(&link) {
            if e.kind() != std::io::ErrorKind::NotFound {
                problems.push(format!("{}: {e}", link.display()));
            }
        }
    }
    // The tray and its daemon; not the update helper, which may be the one
    // running this install (it starts tray.py again, which hands over to
    // us).
    let stop = "Get-CimInstance Win32_Process -Filter \"Name like 'python%'\" | \
                Where-Object { $_.CommandLine -match 'whisper-ptt' -and \
                $_.CommandLine -match '(tray|dictate)\\.py' } | \
                ForEach-Object { Stop-Process -Id $_.ProcessId -Force }";
    if let Err(e) = quiet(
        "powershell.exe",
        &["-NoProfile", "-NonInteractive", "-Command", stop],
    ) {
        problems.push(e);
    }
    // Its entry in Settings > Apps stays, so its files can still be
    // removed — but under a name that says so: next to this program's own
    // "hushkey" entry, two of the same name would look like one program
    // installed twice. (Its uninstaller no longer touches this program,
    // see uninstall.ps1 -Purge.)
    if key_exists(PYTHON_UNINSTALL_KEY) {
        if let Err(e) = set_value(
            PYTHON_UNINSTALL_KEY,
            "DisplayName",
            Some("hushkey - old Python version (files only)"),
            0,
        ) {
            problems.push(e);
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("; "))
    }
}

/// Remove the copies an update moved aside (those still running stay).
pub fn clean_after_update() {
    let target = installed_exe();
    for n in 0..100 {
        let _ = std::fs::remove_file(target.with_extension(format!("exe.old{n}")));
    }
}
