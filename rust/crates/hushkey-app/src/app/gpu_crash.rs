//! whisper.cpp aborts the whole process on some GPU failures (a model
//! that does not fit in graphics memory, which the engine checks for, and
//! whatever it cannot foresee). So that such a crash does not take every
//! later start with it, a marker file exists while a model loads or warms
//! up on the GPU. One found at the next start means that load never ended:
//! that run stays on the CPU. The run after tries the GPU again: if that
//! crashes too, hushkey stays on the CPU until the user switches the GPU
//! on again in Settings or an update arrives. A GPU load that ends without
//! a crash, however it ends, breaks the series.
//!
//! A load that ends any other way than with hushkey's own quit (SIGTERM
//! and the menu both go through it) counts as a crash too: on Windows and
//! macOS that includes a logoff or shutdown within the seconds the first
//! load takes after login.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// Consecutive crashes after which hushkey stops trying the GPU.
const GIVE_UP: u32 = 2;

/// What earlier runs' GPU loads say about this one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Crashes {
    None,
    /// The last run died loading on the GPU: this run uses the CPU, the
    /// next one tries the GPU again.
    Last,
    /// The last run died loading on the GPU again: from now on the CPU,
    /// until the user switches the GPU on again.
    Again,
    /// An earlier run gave up on the GPU.
    GaveUp,
}

pub(super) struct Marker {
    loading: PathBuf,
    /// The number of consecutive crashes, and the version they happened in.
    count: PathBuf,
    version: String,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    /// GPU loads in progress: a model switch can start one while another
    /// still runs, and only the last to end may remove the file.
    active: usize,
    /// hushkey is quitting: a load that starts now is no crash either.
    quitting: bool,
}

/// The marker in hushkey's state folder.
pub(super) fn marker() -> &'static Marker {
    static MARKER: OnceLock<Marker> = OnceLock::new();
    MARKER.get_or_init(|| Marker::new(&crate::config::state_dir(), env!("CARGO_PKG_VERSION")))
}

impl Marker {
    fn new(dir: &Path, version: &str) -> Marker {
        Marker {
            loading: dir.join("gpu-loading"),
            count: dir.join("gpu-crashes"),
            version: version.to_owned(),
            state: Mutex::default(),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// At start: counts a GPU load the last run did not finish, and says
    /// whether this run is to keep off the GPU.
    pub(super) fn crashes(&self) -> Crashes {
        let mut count = self.read_count();
        let crashed = std::fs::remove_file(&self.loading).is_ok();
        if crashed {
            count += 1;
            let _ = std::fs::write(&self.count, format!("{count} {}", self.version));
        }
        match (crashed, count >= GIVE_UP) {
            (true, true) => Crashes::Again,
            (false, true) => Crashes::GaveUp,
            (true, false) => Crashes::Last,
            (false, false) => Crashes::None,
        }
    }

    /// Crashes counted in this version; an update may have fixed them.
    fn read_count(&self) -> u32 {
        let text = std::fs::read_to_string(&self.count).unwrap_or_default();
        match text.split_once(' ') {
            Some((count, version)) if version == self.version => count.parse().unwrap_or(0),
            _ => 0,
        }
    }

    /// Earlier crashes no longer count: the user switched the GPU on, or a
    /// GPU load ended without one.
    pub(super) fn forget(&self) {
        let _ = std::fs::remove_file(&self.count);
    }

    /// Marks a GPU load until the returned guard is dropped.
    pub(super) fn hold(&self) -> Hold<'_> {
        let mut state = self.state();
        if state.active == 0 && !state.quitting {
            // The state folder exists: the log opened in it at start.
            let _ = std::fs::write(&self.loading, b"");
        }
        state.active += 1;
        Hold(self)
    }

    /// A clean exit during a GPU load is no crash, nor is one that starts
    /// while hushkey quits.
    pub(super) fn clear(&self) {
        self.state().quitting = true;
        let _ = std::fs::remove_file(&self.loading);
    }
}

pub(super) struct Hold<'a>(&'a Marker);

impl Drop for Hold<'_> {
    fn drop(&mut self) {
        let mut state = self.0.state();
        state.active -= 1;
        if state.active == 0 {
            let _ = std::fs::remove_file(&self.0.loading);
        }
        // The process lived through it, whether the GPU worked or not.
        self.0.forget();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A start of hushkey in `dir`; `crash` ends its GPU load by dying.
    fn start(dir: &Path, version: &str, crash: bool) -> Crashes {
        let marker = Marker::new(dir, version);
        let crashes = marker.crashes();
        if crashes == Crashes::None {
            let load = marker.hold();
            if crash {
                std::mem::forget(load);
            }
        }
        crashes
    }

    #[test]
    fn a_crash_costs_one_run_on_the_cpu() {
        let dir = tempfile::tempdir().unwrap();
        let dir = dir.path();
        assert_eq!(start(dir, "1", true), Crashes::None);
        assert_eq!(start(dir, "1", false), Crashes::Last);
        assert_eq!(start(dir, "1", false), Crashes::None);
        // The GPU load ended normally: a later crash is a first one.
        assert_eq!(start(dir, "1", true), Crashes::None);
        assert_eq!(start(dir, "1", false), Crashes::Last);
    }

    #[test]
    fn a_repeated_crash_keeps_the_cpu_until_forgotten_or_updated() {
        let dir = tempfile::tempdir().unwrap();
        let dir = dir.path();
        assert_eq!(start(dir, "1", true), Crashes::None);
        assert_eq!(start(dir, "1", false), Crashes::Last);
        assert_eq!(start(dir, "1", true), Crashes::None);
        assert_eq!(start(dir, "1", false), Crashes::Again);
        assert_eq!(start(dir, "1", false), Crashes::GaveUp);
        assert_eq!(start(dir, "1", false), Crashes::GaveUp);
        // An update tries the GPU again.
        assert_eq!(start(dir, "2", false), Crashes::None);
        assert_eq!(start(dir, "2", true), Crashes::None);
        assert_eq!(start(dir, "2", true), Crashes::Last);
        assert_eq!(start(dir, "2", true), Crashes::None);
        assert_eq!(start(dir, "2", false), Crashes::Again);
        // So does switching the GPU on in Settings.
        Marker::new(dir, "2").forget();
        assert_eq!(start(dir, "2", false), Crashes::None);
    }

    #[test]
    fn a_load_that_ends_without_a_crash_breaks_the_series() {
        let dir = tempfile::tempdir().unwrap();
        let dir = dir.path();
        assert_eq!(start(dir, "1", true), Crashes::None);
        let marker = Marker::new(dir, "1");
        assert_eq!(marker.crashes(), Crashes::Last);
        // A GPU load in that run (the GPU switched on again) that fails
        // with an error, or runs: either way no crash.
        drop(marker.hold());
        assert_eq!(marker.read_count(), 0);
        assert!(!marker.loading.exists());
    }

    #[test]
    fn overlapping_loads_keep_the_marker_until_the_last_ends() {
        let dir = tempfile::tempdir().unwrap();
        let marker = Marker::new(dir.path(), "1");
        let first = marker.hold();
        let second = marker.hold();
        drop(first);
        assert!(marker.loading.exists());
        drop(second);
        assert!(!marker.loading.exists());
    }

    #[test]
    fn quitting_during_or_before_a_load_is_no_crash() {
        let dir = tempfile::tempdir().unwrap();
        let marker = Marker::new(dir.path(), "1");
        let load = marker.hold();
        marker.clear();
        // A load the quit overtook starts only now.
        let late = marker.hold();
        // The process ends with both.
        std::mem::forget(load);
        std::mem::forget(late);
        assert_eq!(Marker::new(dir.path(), "1").crashes(), Crashes::None);
    }
}
