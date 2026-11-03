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

/// Whether the system's package manager put this program here.
///
/// The .deb installs `/usr/bin/hushkey` and owns both the program file and
/// the menu entry in `/usr/share/applications`. hushkey then leaves both
/// alone: it cannot write there as the user anyway, and replacing itself
/// would leave dpkg's records pointing at a file it no longer wrote. Such
/// an installation is updated with the package manager: an update offered
/// in the menu is the next .deb, installed by apt ([`install_update`]).
pub fn packaged() -> bool {
    std::env::current_exe()
        .map(|exe| exe.starts_with("/usr/") || exe.starts_with("/opt/"))
        .unwrap_or(false)
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

/// Whether this program can work in this session, so that a Python
/// hushkey may be switched off for it: it has to read the push-to-talk key.
pub fn ready() -> Result<(), String> {
    super::hotkey::readable(evdev::KeyCode::KEY_A)
}

/// Whether a Python hushkey is still set up to start itself.
///
/// Its systemd unit or its autostart entry being there is enough: both
/// bring it up at the next login, next to this program. An install through
/// the package manager never runs `--install --from-python`, so this is
/// what notices a handover that was never completed.
pub fn python_still_autostarts() -> bool {
    let config = dirs::config_dir().unwrap_or_else(|| home().join(".config"));
    config.join("systemd/user/whisper-ptt.service").exists()
        || config.join("autostart/whisper-ptt.desktop").exists()
}

/// The version of the `hushkey` package dpkg has installed, if any (one
/// removed down to its configuration files does not count).
fn package_version() -> Option<String> {
    let out = Command::new("dpkg-query")
        .args(["-W", "-f=${db:Status-Status} ${Version}", "hushkey"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let (status, version) = text.trim().split_once(' ')?;
    (!matches!(status, "not-installed" | "config-files")).then(|| version.to_string())
}

/// Whether a Python-era hushkey package (0.10 or older) is still
/// installed. Only dpkg can remove it (`/opt/hushkey`), which takes the
/// administrator's password: [`remove_python_package`].
pub fn python_package() -> bool {
    package_version().is_some_and(|v| python_package_version(&v))
}

/// Run `args` as root, the password asked for in the desktop's own
/// dialog (polkit's `pkexec`); what they print goes to the log.
/// `Ok(false)`: the user dismissed the dialog, and nothing ran.
fn as_root(args: &[&str]) -> Result<bool, String> {
    let out = Command::new("pkexec")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("pkexec: {e}"))?;
    let printed = [&out.stdout, &out.stderr]
        .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
        .join("\n");
    for line in printed.lines().filter(|l| !l.trim().is_empty()) {
        crate::log::line(&format!("  {line}"));
    }
    match out.status.code() {
        Some(0) => Ok(true),
        // pkexec's own codes: dismissed, and not allowed (or no dialog).
        Some(126) => Ok(false),
        Some(127) => Err("pkexec: not authorized, or no password dialog in this session".into()),
        _ => Err(format!("{}: {}", args[0], out.status)),
    }
}

/// As root: the Python-era package removed. Checked once more here, as
/// the package may have changed since the menu was built. Its postrm
/// deletes the udev rule that lets the `input` group use /dev/uinput —
/// which this program types through on Wayland — so the rule is put
/// back: the user granted that access, removing old files must not take
/// it away.
const REMOVE_PYTHON_PACKAGE: &str = r#"set -e
version="$(dpkg-query -W -f='${Version}' hushkey 2>/dev/null)" || exit 0
case "$version" in
    0.[0-9]|0.[0-9][.~+-]*|0.10|0.10[.~+-]*) ;;
    *) echo "hushkey $version is not the Python version; left alone" >&2; exit 3 ;;
esac
rule=/etc/udev/rules.d/99-whisper-ptt-uinput.rules
kept=""
if [ -f "$rule" ]; then kept="$(cat "$rule")"; fi
dpkg --remove hushkey
if [ -n "$kept" ]; then printf '%s\n' "$kept" > "$rule"; fi
"#;

/// Remove the Python-era package, asking for the password.
/// `Ok(false)`: the dialog was dismissed.
pub fn remove_python_package() -> Result<bool, String> {
    as_root(&["/bin/sh", "-c", REMOVE_PYTHON_PACKAGE])
}

/// Whether an update can be installed here: always for the program in the
/// home folder, and for this program's own .deb through dpkg. Anything
/// else in `/usr` or `/opt` belongs to a package manager hushkey does not
/// know, and is left to it.
pub fn updates_itself() -> bool {
    !packaged() || package_version().is_some_and(|v| !python_package_version(&v))
}

/// dpkg's name for this system's architecture.
#[cfg(target_arch = "x86_64")]
const DEB_ARCH: &str = "amd64";
#[cfg(target_arch = "aarch64")]
const DEB_ARCH: &str = "arm64";

/// Whether `name` is the release asset an update installs here: the
/// program itself, or for an install from the .deb the .deb
/// (`hushkey_0.12.0_amd64.deb`; GitHub shows a pre-release's `~` as `.`).
pub fn update_asset(name: &str) -> bool {
    if packaged() {
        package_asset(name)
    } else {
        name == ASSET
    }
}

fn package_asset(name: &str) -> bool {
    name.starts_with("hushkey_")
        && name.ends_with(&format!("_{DEB_ARCH}.deb"))
        && !name.contains(['/', '\\'])
}

/// Put a downloaded and checked update in place: renamed over the program
/// in the home folder, or for the .deb installed by apt (dpkg where there
/// is no apt), which asks for the password. `Ok(None)`: the dialog was
/// dismissed.
pub fn install_update(file: &Path) -> Result<Option<PathBuf>, String> {
    if !packaged() {
        return place(file).map(Some);
    }
    let path = file.to_str().ok_or("the update's path is not UTF-8")?;
    let args: &[&str] = if Path::new("/usr/bin/apt-get").exists() {
        // apt rather than dpkg: it also installs a library a new version
        // may have started to need.
        &["/usr/bin/apt-get", "install", "-y", path]
    } else {
        &["/usr/bin/dpkg", "-i", path]
    };
    Ok(as_root(args)?.then(|| PathBuf::from("/usr/bin/hushkey")))
}

/// Whether a hushkey package version is one of the Python era: 0.10.x and
/// older. Compared by number, not with semver's `<`: that puts
/// 0.11.0-rc.1 below 0.11.0 and would call this program's own pre-release
/// package old — and the advice that follows is to remove the package.
fn python_package_version(version: &str) -> bool {
    let mut parts = version.split(['.', '-', '+', '~']);
    let mut number = || parts.next().and_then(|p| p.parse::<u64>().ok());
    match (number(), number()) {
        (Some(major), Some(minor)) => (major, minor) < (0, 11),
        _ => false,
    }
}

/// The Python hushkey's state folder (settings, history).
pub fn python_state_dir() -> PathBuf {
    dirs::state_dir()
        .unwrap_or_else(|| home().join(".local/state"))
        .join("whisper-ptt")
}

/// Where the Python hushkey was installed (by its install.sh, and by its
/// .deb, which copied itself there): its sources and its Python
/// environment of some 2–3 GB.
pub fn python_install_dir() -> PathBuf {
    data_dir().join("whisper-ptt")
}

/// The Python hushkey's own data folder: the GPU engines it downloaded
/// and `acceleration.json`. Nothing of this program's is in it.
pub fn python_data_dir() -> PathBuf {
    data_dir().join("hushkey")
}

/// The Python hushkey off for good: its service stopped and its unit files
/// removed (so nothing can start it again), its autostart entry removed,
/// its processes stopped. Its settings and history stay — `import_python`
/// has already taken what hushkey needs.
pub fn retire_python() -> Result<(), String> {
    let systemctl = |args: &[&str]| {
        // Absent on most systems by now; failing is fine.
        let _ = Command::new("systemctl")
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    };
    systemctl(&["--user", "disable", "--now", "whisper-ptt.service"]);
    let config = dirs::config_dir().unwrap_or_else(|| home().join(".config"));
    // `disable` only drops the .wants symlinks: the unit file stays, and
    // `systemctl --user restart whisper-ptt.service` would start the
    // Python app again. The .deb's menu entry does exactly that, so the
    // unit has to go, not just its enablement.
    let units = config.join("systemd/user");
    let mut removed_a_unit = false;
    for unit in [
        "whisper-ptt.service",
        "whisper-ptt.service.d",
        "ydotoold.service",
    ] {
        let path = units.join(unit);
        let outcome = if path.is_dir() {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        match outcome {
            Ok(()) => removed_a_unit = true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            // A unit left behind is not worth failing the handover over.
            Err(e) => crate::log::line(&format!("{}: {e}", path.display())),
        }
    }
    if removed_a_unit {
        systemctl(&["--user", "daemon-reload"]);
    }
    let autostart = config.join("autostart/whisper-ptt.desktop");
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
    fn only_python_era_packages_count_as_old() {
        for old in ["0.7.5", "0.10.10", "0.10.0~rc1", "0.1.0"] {
            assert!(python_package_version(old), "{old}");
        }
        for native in [
            "0.11.0",
            "0.11.0~rc.1",
            "0.11.0-rc.3",
            "0.12.4",
            "1.0.0",
            "",
            "x",
        ] {
            assert!(!python_package_version(native), "{native}");
        }
    }

    #[test]
    fn a_package_update_is_this_systems_deb() {
        assert!(package_asset(&format!("hushkey_0.12.0_{DEB_ARCH}.deb")));
        // A pre-release, as GitHub names it.
        assert!(package_asset(&format!(
            "hushkey_0.12.0.rc.1_{DEB_ARCH}.deb"
        )));
        assert!(!package_asset(ASSET));
        assert!(!package_asset("hushkey_0.12.0_mips.deb"));
        assert!(!package_asset(&format!("hushkey_/../x_{DEB_ARCH}.deb")));
    }

    /// The root script, with stand-ins for dpkg-query and dpkg and the
    /// udev rule in a scratch folder: (exit code, what dpkg was asked, the
    /// rule afterwards).
    fn remove_package_with(
        version: Option<&str>,
        rule: Option<&str>,
    ) -> (i32, String, Option<String>) {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!(
            "hk-rm-{}-{}",
            std::process::id(),
            version.unwrap_or("none")
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (rule_path, log) = (dir.join("99.rules"), dir.join("dpkg.log"));
        if let Some(text) = rule {
            std::fs::write(&rule_path, text).unwrap();
        }
        let stub = |name: &str, body: &str| {
            let path = dir.join(name);
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        };
        stub(
            "dpkg-query",
            r#"[ -n "$STUB_VERSION" ] || exit 1; printf %s "$STUB_VERSION""#,
        );
        // Removing the package takes the rule along, as its postrm does.
        stub("dpkg", r#"echo "$@" >> "$STUB_LOG"; rm -f "$STUB_RULE""#);
        let script = REMOVE_PYTHON_PACKAGE.replace(
            "/etc/udev/rules.d/99-whisper-ptt-uinput.rules",
            rule_path.to_str().unwrap(),
        );
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", &script])
            .env("PATH", format!("{}:/usr/bin:/bin", dir.display()))
            .env("STUB_LOG", &log)
            .env("STUB_RULE", &rule_path)
            .env_remove("STUB_VERSION");
        if let Some(v) = version {
            command.env("STUB_VERSION", v);
        }
        let status = command.output().unwrap().status;
        let result = (
            status.code().unwrap(),
            std::fs::read_to_string(&log).unwrap_or_default(),
            std::fs::read_to_string(&rule_path).ok(),
        );
        std::fs::remove_dir_all(&dir).unwrap();
        result
    }

    #[test]
    fn only_the_python_package_is_removed_and_the_uinput_rule_stays() {
        let rule = "KERNEL==\"uinput\", GROUP=\"input\"\n";
        let (code, dpkg, kept) = remove_package_with(Some("0.7.5"), Some(rule));
        assert_eq!((code, dpkg.as_str()), (0, "--remove hushkey\n"));
        assert_eq!(kept.as_deref(), Some(rule));
        // Without a rule, none is made.
        let (code, dpkg, kept) = remove_package_with(Some("0.10.10"), None);
        assert_eq!((code, dpkg.as_str(), kept), (0, "--remove hushkey\n", None));
        // This program's own package, or none at all: left alone.
        for native in ["0.11.0", "0.11.0~rc.3", "1.0.0"] {
            let (code, dpkg, _) = remove_package_with(Some(native), None);
            assert_eq!((code, dpkg.as_str()), (3, ""), "{native}");
        }
        let (code, dpkg, _) = remove_package_with(None, None);
        assert_eq!((code, dpkg.as_str()), (0, ""));
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
