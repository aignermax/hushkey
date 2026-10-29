//! Installing, updating and the move from the Python app, on macOS: the
//! program is a whole bundle, `~/Applications/hushkey.app`, swapped as one.
//! There is no menu entry beyond the bundle itself (Launchpad and Spotlight
//! find it there).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The release asset for this system: a zip holding `hushkey.app`.
pub const ASSET: &str = if cfg!(target_arch = "aarch64") {
    "hushkey-macos-arm64.zip"
} else {
    "hushkey-macos-x86_64.zip"
};

const BUNDLE: &str = "hushkey.app";
const PYTHON_LABEL: &str = "com.whisper-ptt";

fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(std::env::temp_dir)
}

fn installed_bundle() -> PathBuf {
    home().join("Applications").join(BUNDLE)
}

/// `~/Applications/hushkey.app/Contents/MacOS/hushkey`.
pub fn installed_exe() -> PathBuf {
    exe_in(&installed_bundle())
}

fn exe_in(bundle: &Path) -> PathBuf {
    bundle.join("Contents/MacOS/hushkey")
}

/// The `.app` an executable sits in (`X.app/Contents/MacOS/exe`), if any.
fn bundle_of(exe: &Path) -> Option<PathBuf> {
    let macos = exe.parent()?;
    let contents = macos.parent()?;
    let bundle = contents.parent()?;
    (macos.file_name()? == "MacOS"
        && contents.file_name()? == "Contents"
        && bundle.extension()? == "app")
        .then(|| bundle.to_path_buf())
}

fn run(command: &mut Command) -> Result<(), String> {
    let output = command
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("{command:?}: {e}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{command:?}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

fn remove_dir(path: &Path) {
    if path.exists() {
        let _ = std::fs::remove_dir_all(path);
    }
}

/// A name next to `target` for a bundle on its way in or out
/// (`clean_after_update` removes what is left of them).
fn aside(target: &Path, kind: &str) -> PathBuf {
    target.with_file_name(format!(".hushkey.app.{kind}-{}", std::process::id()))
}

/// Put the program at `source` in place of the installed one (which may be
/// running). `source` is either the running executable (inside some
/// `hushkey.app`: that whole bundle is copied) or a downloaded, verified
/// `.zip` holding `hushkey.app`. The bundles are swapped by rename, the old
/// one removed. Returns the installed executable.
pub fn place(source: &Path) -> Result<PathBuf, String> {
    let target = installed_bundle();
    let apps = target.parent().ok_or("no Applications folder")?;
    std::fs::create_dir_all(apps).map_err(|e| format!("{}: {e}", apps.display()))?;
    // Staged next to the target, so the swap is a rename on one volume.
    let staging = aside(&target, "new");
    remove_dir(&staging);
    std::fs::create_dir_all(&staging).map_err(|e| format!("{}: {e}", staging.display()))?;
    let result = (|| {
        let new = staging.join(BUNDLE);
        if source
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("zip"))
        {
            // ditto keeps what the bundle's signature covers (modes, links,
            // extended attributes), as Finder's unzip does.
            run(Command::new("ditto")
                .arg("-x")
                .arg("-k")
                .arg(source)
                .arg(&staging))?;
        } else {
            let bundle = bundle_of(source)
                .ok_or_else(|| format!("{} is not inside a hushkey.app", source.display()))?;
            if target.exists() && bundle.canonicalize().ok() == target.canonicalize().ok() {
                return Ok(()); // already the installed one
            }
            run(Command::new("ditto").arg(&bundle).arg(&new))?;
        }
        if !exe_in(&new).is_file() {
            return Err(format!(
                "no {BUNDLE} with a hushkey program in {}",
                source.display()
            ));
        }
        swap(&new, &target)
    })();
    remove_dir(&staging);
    result.map(|()| installed_exe())
}

/// Move `new` to `target`, the old bundle aside first (a running program
/// keeps running from its moved files) and then removed.
fn swap(new: &Path, target: &Path) -> Result<(), String> {
    let old = aside(target, "old");
    remove_dir(&old);
    let had_old = target.exists();
    if had_old {
        std::fs::rename(target, &old).map_err(|e| format!("{}: {e}", target.display()))?;
    }
    if let Err(e) = std::fs::rename(new, target) {
        if had_old {
            let _ = std::fs::rename(&old, target);
        }
        return Err(format!("{}: {e}", target.display()));
    }
    remove_dir(&old);
    Ok(())
}

/// Nothing beyond the bundle in `~/Applications` to register.
pub fn register(_exe: &Path) -> Result<(), String> {
    Ok(())
}

/// The installed bundle, all of it (the shared uninstall removes only the
/// program file, which would leave a broken app behind). macOS lets a
/// running program's bundle go: it runs on until it quits.
pub fn unregister() -> Result<(), String> {
    let bundle = installed_bundle();
    match std::fs::remove_dir_all(&bundle) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            Err(format!("{}: {e}", bundle.display()))
        }
        _ => Ok(()),
    }
}

/// The process ids of this user's processes named exactly `name`, but not
/// this one.
fn others_named(name: &str) -> Vec<i32> {
    let uid = unsafe { libc::getuid() }.to_string();
    let Ok(output) = Command::new("pgrep")
        .args(["-U", &uid, "-x", name])
        .stdin(Stdio::null())
        .output()
    else {
        return Vec::new();
    };
    let me = std::process::id() as i32;
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.trim().parse().ok())
        .filter(|pid| *pid != me)
        .collect()
}

/// End `pids` politely (SIGTERM), then for good after a few seconds.
fn stop(pids: &[i32]) {
    for pid in pids {
        unsafe { libc::kill(*pid, libc::SIGTERM) };
    }
    let until = Instant::now() + Duration::from_secs(3);
    let alive = |pid: &i32| unsafe { libc::kill(*pid, 0) } == 0;
    while pids.iter().any(alive) && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(100));
    }
    for pid in pids.iter().filter(|p| alive(p)) {
        unsafe { libc::kill(*pid, libc::SIGKILL) };
    }
}

/// Stop other running hushkey (Rust) processes of this user.
pub fn stop_others() {
    stop(&others_named("hushkey"));
}

/// Start `exe` with `args`, detached from this process: through Launch
/// Services when it is in a bundle (as a double-click would, so the
/// permissions granted to the app apply), directly otherwise.
pub fn launch(exe: &Path, args: &[&str]) -> Result<(), String> {
    let mut command = match bundle_of(exe) {
        Some(bundle) => {
            let mut c = Command::new("open");
            c.arg("-n").arg("-a").arg(bundle);
            if !args.is_empty() {
                c.arg("--args").args(args);
            }
            c
        }
        None => {
            use std::os::unix::process::CommandExt;
            let mut c = Command::new(exe);
            c.args(args).process_group(0);
            c
        }
    };
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(drop)
        .map_err(|e| format!("{}: {e}", exe.display()))
}

/// Where the Python app keeps its settings, history and log.
pub fn python_state_dir() -> PathBuf {
    home().join("Library/Logs/whisper-ptt")
}

/// The Python hushkey off for good: its LaunchAgent unloaded and removed,
/// its processes stopped. Its files stay.
pub fn retire_python() -> Result<(), String> {
    // This process may have been started by the Python app's updater, in
    // the LaunchAgent's process group, which launchd ends with the agent:
    // leave the group first. (Fails harmlessly if already a group leader.)
    unsafe { libc::setsid() };
    let uid = unsafe { libc::getuid() };
    // Unloading ends the agent (KeepAlive would restart a killed tray.py);
    // it fails when the agent is not loaded, which is fine.
    let _ = Command::new("launchctl")
        .arg("bootout")
        .arg(format!("gui/{uid}/{PYTHON_LABEL}"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let plist = home()
        .join("Library/LaunchAgents")
        .join(format!("{PYTHON_LABEL}.plist"));
    match std::fs::remove_file(&plist) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            return Err(format!("{}: {e}", plist.display()));
        }
        _ => {}
    }
    // Started by hand, or outliving the agent: the tray and the dictation
    // daemon, run by a Python (`…/python …/tray.py`).
    let Ok(output) = Command::new("pgrep")
        .args([
            "-U",
            &uid.to_string(),
            "-f",
            r"python[0-9.]* .*(tray|dictate)\.py",
        ])
        .stdin(Stdio::null())
        .output()
    else {
        return Ok(());
    };
    let me = std::process::id() as i32;
    let pids: Vec<i32> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.trim().parse().ok())
        .filter(|pid| *pid != me)
        .collect();
    stop(&pids);
    Ok(())
}

/// After an update: remove bundles left aside by earlier swaps.
pub fn clean_after_update() {
    let Some(apps) = installed_bundle().parent().map(Path::to_path_buf) else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(&apps) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(".hushkey.app.old-") || name.starts_with(".hushkey.app.new-") {
            remove_dir(&entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bundle_of_an_executable_is_found() {
        assert_eq!(
            bundle_of(Path::new("/x/hushkey.app/Contents/MacOS/hushkey")),
            Some(PathBuf::from("/x/hushkey.app"))
        );
        assert_eq!(bundle_of(Path::new("/x/target/release/hushkey")), None);
        assert!(installed_exe().ends_with("Applications/hushkey.app/Contents/MacOS/hushkey"));
    }

    #[test]
    fn a_bundle_moves_in_by_rename_and_the_old_one_goes() {
        let dir = std::env::temp_dir().join(format!("hushkey-swap-{}", std::process::id()));
        let (new, target) = (dir.join("new.app"), dir.join("hushkey.app"));
        std::fs::create_dir_all(new.join("Contents")).unwrap();
        std::fs::write(new.join("Contents/v"), "2").unwrap();
        std::fs::create_dir_all(target.join("Contents")).unwrap();
        std::fs::write(target.join("Contents/v"), "1").unwrap();
        swap(&new, &target).unwrap();
        assert_eq!(
            std::fs::read_to_string(target.join("Contents/v")).unwrap(),
            "2"
        );
        assert!(!new.exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
