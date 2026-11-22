//! whisper.cpp aborts the whole process on some GPU failures (a model
//! that does not fit in graphics memory, which the engine checks for, and
//! whatever it cannot foresee). So that one such crash does not take every
//! later start with it, a marker file exists while a model loads or warms
//! up on the GPU. One found at the next start means that load never ended:
//! that run stays on the CPU, and the one after tries the GPU again.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

pub(super) struct Marker {
    path: PathBuf,
    /// GPU loads in progress: a model switch can start one while another
    /// still runs, and only the last to end may remove the file.
    active: Mutex<usize>,
}

/// The marker in hushkey's state folder.
pub(super) fn marker() -> &'static Marker {
    static MARKER: OnceLock<Marker> = OnceLock::new();
    MARKER.get_or_init(|| Marker::new(&crate::config::state_dir()))
}

impl Marker {
    fn new(dir: &Path) -> Marker {
        Marker {
            path: dir.join("gpu-loading"),
            active: Mutex::new(0),
        }
    }

    /// Whether a GPU load of an earlier run never ended. Removes the marker.
    pub(super) fn take_crash(&self) -> bool {
        std::fs::remove_file(&self.path).is_ok()
    }

    /// Marks a GPU load until the returned guard is dropped.
    pub(super) fn hold(&self) -> Hold<'_> {
        let mut active = self.active.lock().unwrap_or_else(|p| p.into_inner());
        if *active == 0 {
            // The state folder exists: the log opened in it at start.
            let _ = std::fs::write(&self.path, b"");
        }
        *active += 1;
        Hold(self)
    }

    /// A clean exit during a GPU load is no crash.
    pub(super) fn clear(&self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

pub(super) struct Hold<'a>(&'a Marker);

impl Drop for Hold<'_> {
    fn drop(&mut self) {
        let mut active = self.0.active.lock().unwrap_or_else(|p| p.into_inner());
        *active -= 1;
        if *active == 0 {
            let _ = std::fs::remove_file(&self.0.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unfinished_gpu_load_is_found_once() {
        let dir = tempfile::tempdir().unwrap();
        let marker = Marker::new(dir.path());
        assert!(!marker.take_crash());
        {
            let _load = marker.hold();
            // The process dies here: the next start finds the marker.
            let restarted = Marker::new(dir.path());
            assert!(restarted.take_crash());
            assert!(!restarted.take_crash(), "the run after retries the GPU");
        }
        {
            let _load = marker.hold();
        }
        assert!(!marker.take_crash(), "a finished load leaves no marker");
    }

    #[test]
    fn overlapping_loads_keep_the_marker_until_the_last_ends() {
        let dir = tempfile::tempdir().unwrap();
        let marker = Marker::new(dir.path());
        let first = marker.hold();
        let second = marker.hold();
        drop(first);
        assert!(marker.path.exists());
        drop(second);
        assert!(!marker.path.exists());
    }

    #[test]
    fn quitting_during_a_load_is_no_crash() {
        let dir = tempfile::tempdir().unwrap();
        let marker = Marker::new(dir.path());
        let load = marker.hold();
        marker.clear();
        std::mem::forget(load); // the process ends with the load
        assert!(!Marker::new(dir.path()).take_crash());
    }
}
