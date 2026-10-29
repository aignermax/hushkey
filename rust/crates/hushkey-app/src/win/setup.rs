//! Installing on Windows: the program in `%LOCALAPPDATA%\Programs\hushkey`,
//! a Start Menu shortcut, and the Python hushkey switched off.

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

pub fn installed_exe() -> PathBuf {
    local_app_data().join(r"Programs\hushkey\hushkey.exe")
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
    let script = format!(
        "$l = (New-Object -ComObject WScript.Shell).CreateShortcut('{}'); \
         $l.TargetPath = '{}'; $l.WorkingDirectory = '{}'; \
         $l.Description = 'Local push-to-talk dictation'; $l.Save()",
        ps_quote(&link),
        ps_quote(exe),
        ps_quote(exe.parent().unwrap_or(exe)),
    );
    quiet(
        "powershell.exe",
        &["-NoProfile", "-NonInteractive", "-Command", &script],
    )?;
    // The development installer's entry, if any.
    let _ = std::fs::remove_file(start_menu().join("hushkey (Rust).lnk"));
    Ok(())
}

/// A path inside a single-quoted PowerShell string.
fn ps_quote(path: &Path) -> String {
    path.display().to_string().replace('\'', "''")
}

pub fn unregister() -> Result<(), String> {
    match std::fs::remove_file(start_menu().join("hushkey.lnk")) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
        _ => Ok(()),
    }
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
