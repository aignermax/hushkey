//! Installing, updating and uninstalling on Linux, and retiring the Python
//! hushkey. The program is `~/.local/bin/hushkey`, the menu entry
//! `~/.local/share/applications/hushkey.desktop` (the Python app's entry
//! of the same name is replaced on purpose), autostart is
//! [`super::autostart`].

use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The release asset with this system's program.
#[cfg(target_arch = "x86_64")]
pub const ASSET: &str = "hushkey-linux-x86_64";
#[cfg(target_arch = "aarch64")]
pub const ASSET: &str = "hushkey-linux-aarch64";

/// The logo, for the menu entry.
const LOGO: &[u8] = include_bytes!("../../../../../assets/logo.png");

fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp"))
}

fn data_dir() -> PathBuf {
    dirs::data_dir().unwrap_or_else(|| home().join(".local/share"))
}

pub fn installed_exe() -> PathBuf {
    home().join(".local/bin/hushkey")
}

fn menu_entry() -> PathBuf {
    data_dir().join("applications/hushkey.desktop")
}

fn icon() -> PathBuf {
    data_dir().join("icons/hicolor/512x512/apps/hushkey.png")
}

/// Where a new program waits before it is renamed over the installed one.
fn staged() -> PathBuf {
    installed_exe().with_file_name(".hushkey.new")
}

fn err(path: &Path) -> impl Fn(std::io::Error) -> String + '_ {
    move |e| format!("{}: {e}", path.display())
}

/// `path` as one argument of a desktop entry's Exec line.
fn exec_arg(path: &Path) -> String {
    let text = path.to_string_lossy();
    let plain = text
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "/._-+,@".contains(c));
    let text = if plain {
        text.into_owned()
    } else {
        let mut quoted = String::from("\"");
        for c in text.chars() {
            if matches!(c, '"' | '`' | '$' | '\\') {
                quoted.push('\\');
            }
            quoted.push(c);
        }
        quoted + "\""
    };
    // The value is a string first (backslashes escaped), and % starts a
    // field code.
    text.replace('\\', "\\\\").replace('%', "%%")
}

/// A desktop entry starting `exe`, with `extra` lines.
pub(super) fn desktop_entry(exe: &Path, extra: &str) -> String {
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=hushkey\n\
         GenericName=Push-to-talk dictation\n\
         GenericName[de]=Push-to-Talk-Diktat\n\
         Comment=Local, offline dictation: hold a key, speak, release\n\
         Comment[de]=Lokales Offline-Diktat: Taste halten, sprechen, loslassen\n\
         Exec={}\n\
         Icon={}\n\
         Terminal=false\n\
         StartupNotify=false\n\
         {extra}",
        exec_arg(exe),
        icon().display()
    )
}

/// Put the program at `source` (the running exe, or a downloaded and
/// checked release asset) in place of the installed one, which may be
/// running: a running program keeps its old file when a new one is renamed
/// over it. Returns the installed exe.
pub fn place(source: &Path) -> Result<PathBuf, String> {
    let installed = installed_exe();
    let dir = installed.parent().expect("a file in a folder");
    std::fs::create_dir_all(dir).map_err(err(dir))?;
    if let (Ok(a), Ok(b)) = (source.canonicalize(), installed.canonicalize()) {
        if a == b {
            return Ok(installed); // started from the installed copy
        }
    }
    let staged = staged();
    std::fs::copy(source, &staged).map_err(err(&staged))?;
    std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))
        .map_err(err(&staged))?;
    std::fs::rename(&staged, &installed).map_err(err(&installed))?;
    Ok(installed)
}

/// The menu entry (and its icon) for `exe`.
pub fn register(exe: &Path) -> Result<(), String> {
    let icon = icon();
    std::fs::create_dir_all(icon.parent().expect("in a folder")).map_err(err(&icon))?;
    std::fs::write(&icon, LOGO).map_err(err(&icon))?;
    let entry = menu_entry();
    std::fs::create_dir_all(entry.parent().expect("in a folder")).map_err(err(&entry))?;
    let text = desktop_entry(
        exe,
        "Categories=Utility;Accessibility;\n\
         Keywords=dictation;speech;voice;whisper;whisper-ptt;transcribe;push-to-talk;Diktat;Sprache;Spracheingabe;\n",
    );
    std::fs::write(&entry, text).map_err(err(&entry))
}

pub fn unregister() -> Result<(), String> {
    for path in [menu_entry(), icon()] {
        match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(err(&path)(e)),
            _ => {}
        }
    }
    Ok(())
}

/// Processes of this user other than this one, with their command lines.
fn processes() -> Vec<(i32, PathBuf, Vec<String>)> {
    let me = std::process::id() as i32;
    let uid = unsafe { libc::getuid() };
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let pid: i32 = entry.file_name().to_str()?.parse().ok()?;
            if pid == me || entry.metadata().ok()?.uid() != uid {
                return None;
            }
            let dir = entry.path();
            let exe = std::fs::read_link(dir.join("exe")).unwrap_or_default();
            let cmdline = std::fs::read(dir.join("cmdline")).unwrap_or_default();
            let args = cmdline
                .split(|b| *b == 0)
                .filter(|a| !a.is_empty())
                .map(|a| String::from_utf8_lossy(a).into_owned())
                .collect();
            Some((pid, exe, args))
        })
        .collect()
}

/// Stop other hushkey (Rust) processes of this user, not this one. A
/// running program renamed over shows as "hushkey (deleted)".
pub fn stop_others() {
    let pids: Vec<i32> = processes()
        .into_iter()
        .filter(|(_, exe, _)| {
            exe.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n == "hushkey" || n == "hushkey (deleted)")
        })
        .map(|(pid, _, _)| pid)
        .collect();
    crate::unix::stop(&pids);
}

/// Start `exe` with `args` on its own (own session, no terminal), not
/// waiting for it.
pub fn launch(exe: &Path, args: &[&str]) -> Result<(), String> {
    use std::os::unix::process::CommandExt;
    let mut command = Command::new(exe);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: setsid is async-signal-safe.
    unsafe {
        command.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    command.spawn().map(drop).map_err(err(exe))
}

/// The Python hushkey's state folder (settings, history).
pub fn python_state_dir() -> PathBuf {
    dirs::state_dir()
        .unwrap_or_else(|| home().join(".local/state"))
        .join("whisper-ptt")
}

/// The Python hushkey off for good: its service disabled and stopped, its
/// autostart entry (if any) removed, its processes stopped. Its files stay.
pub fn retire_python() -> Result<(), String> {
    // Absent on most systems by now; failing is fine.
    let _ = Command::new("systemctl")
        .args(["--user", "disable", "--now", "whisper-ptt.service"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let autostart = dirs::config_dir()
        .unwrap_or_else(|| home().join(".config"))
        .join("autostart/whisper-ptt.desktop");
    match std::fs::remove_file(&autostart) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(err(&autostart)(e)),
        _ => {}
    }
    let pids: Vec<i32> = processes()
        .into_iter()
        .filter(|(_, _, args)| crate::unix::is_python_hushkey(args))
        .map(|(pid, _, _)| pid)
        .collect();
    crate::unix::stop(&pids);
    Ok(())
}

/// Leftovers of an update: a staged program that was never renamed in.
pub fn clean_after_update() {
    let _ = std::fs::remove_file(staged());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_lines_quote_what_needs_it() {
        assert_eq!(
            exec_arg(Path::new("/home/a/.local/bin/hushkey")),
            "/home/a/.local/bin/hushkey"
        );
        assert_eq!(
            exec_arg(Path::new("/home/a b/50%/hushkey")),
            "\"/home/a b/50%%/hushkey\""
        );
        assert_eq!(exec_arg(Path::new("/x/$y")), "\"/x/\\\\$y\"");
    }

    #[test]
    fn python_processes_are_told_by_their_script() {
        let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(crate::unix::is_python_hushkey(&args(&[
            "/home/a/.local/share/whisper-ptt/.venv/bin/python",
            "/home/a/.local/share/whisper-ptt/tray.py"
        ])));
        assert!(crate::unix::is_python_hushkey(&args(&[
            "python3",
            "/opt/hushkey/dictate.py"
        ])));
        assert!(!crate::unix::is_python_hushkey(&args(&[
            "python3",
            "/home/a/tray.py"
        ])));
    }

    #[test]
    #[ignore = "points HOME elsewhere for the whole process; run alone with --ignored"]
    fn install_autostart_and_uninstall_in_a_scratch_home() {
        let home = std::env::temp_dir().join(format!("hushkey-home-{}", std::process::id()));
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("HOME", &home);
        for v in ["XDG_DATA_HOME", "XDG_CONFIG_HOME", "XDG_STATE_HOME"] {
            std::env::remove_var(v);
        }
        let exe = place(&std::env::current_exe().unwrap()).unwrap();
        assert_eq!(exe, home.join(".local/bin/hushkey"));
        let mode = std::fs::metadata(&exe).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o755);
        // Again over the existing one, as an update does.
        place(&std::env::current_exe().unwrap()).unwrap();
        register(&exe).unwrap();
        let entry = std::fs::read_to_string(menu_entry()).unwrap();
        assert!(
            entry.contains(&format!("Exec={}\n", exe.display())),
            "{entry}"
        );
        assert!(icon().exists());
        assert!(!super::super::autostart::enabled());
        super::super::autostart::set(true).unwrap();
        assert!(super::super::autostart::enabled());
        super::super::autostart::set(false).unwrap();
        assert!(!super::super::autostart::enabled());
        unregister().unwrap();
        assert!(!menu_entry().exists() && !icon().exists());
        assert_eq!(python_state_dir(), home.join(".local/state/whisper-ptt"));
        std::fs::remove_dir_all(&home).unwrap();
    }
}
