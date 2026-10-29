//! What Linux and macOS share: the single-instance lock and stopping
//! processes.

use std::fs::File;
use std::os::fd::AsRawFd;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::log;

static LOCK: Mutex<Option<File>> = Mutex::new(None);

/// True if another hushkey holds the lock file `path`. Otherwise the lock is
/// now ours, for the process's life; asking again (an update waits for its
/// predecessor) answers false without locking a second time, which our own
/// lock would refuse.
pub fn already_running(path: &Path) -> bool {
    let mut lock = LOCK.lock().unwrap();
    if lock.is_some() {
        return false;
    }
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let file = match std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
    {
        Ok(file) => file,
        Err(e) => {
            // No lock to be had: run rather than not at all.
            log::line(&format!("single-instance lock unavailable: {e}"));
            return false;
        }
    };
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return std::io::Error::last_os_error().raw_os_error() == Some(libc::EWOULDBLOCK);
    }
    *lock = Some(file);
    false
}

/// End `pids` politely (SIGTERM), then for good after a few seconds.
pub fn stop(pids: &[i32]) {
    for pid in pids {
        unsafe { libc::kill(*pid, libc::SIGTERM) };
    }
    let until = Instant::now() + Duration::from_secs(3);
    let alive = |pid: &i32| unsafe { libc::kill(*pid, 0) } == 0;
    while pids.iter().any(alive) && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(50));
    }
    for pid in pids.iter().filter(|p| alive(p)) {
        unsafe { libc::kill(*pid, libc::SIGKILL) };
    }
}

/// Whether a command line runs the Python hushkey: its tray.py or
/// dictate.py, in its own folder (the installer's `whisper-ptt`, a clone
/// named `hushkey`, or the .deb's `/opt/hushkey`).
pub fn is_python_hushkey(args: &[String]) -> bool {
    args.iter().any(|a| {
        (a.ends_with("/tray.py") || a.ends_with("/dictate.py"))
            && (a.contains("whisper-ptt") || a.contains("/hushkey/"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_python_hushkey_is_recognised() {
        let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(is_python_hushkey(&args(&[
            "/home/u/.local/share/whisper-ptt/.venv/bin/python",
            "/home/u/.local/share/whisper-ptt/tray.py"
        ])));
        assert!(is_python_hushkey(&args(&[
            "python3",
            "/opt/hushkey/dictate.py"
        ])));
        assert!(is_python_hushkey(&args(&[
            "python3",
            "/Users/u/src/hushkey/tray.py"
        ])));
        assert!(!is_python_hushkey(&args(&[
            "python3",
            "/home/u/projects/foo/tray.py"
        ])));
        assert!(!is_python_hushkey(&args(&[
            "vim",
            "/home/u/whisper-ptt/README.md"
        ])));
    }

    #[test]
    fn a_second_question_does_not_trip_over_our_own_lock() {
        let path = std::env::temp_dir().join(format!("hk-lock-{}", std::process::id()));
        assert!(!already_running(&path));
        assert!(!already_running(&path));
        std::fs::remove_file(&path).ok();
    }
}
