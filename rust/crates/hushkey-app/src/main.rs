//! `hushkey`: hold a key, speak, release, and the text appears where you
//! type. Built on hushkey's Rust core; Windows first (see
//! docs/superpowers/specs/2026-10-28-rust-app-design.md).

// A tray app: no console window on Windows.
#![cfg_attr(windows, windows_subsystem = "windows")]

// The app runs on Windows so far; elsewhere its shared parts are still
// built and tested.
#[cfg(windows)]
mod app;
#[cfg_attr(not(windows), allow(dead_code))]
mod config;
#[cfg_attr(not(windows), allow(dead_code))]
mod history;
#[cfg_attr(not(windows), allow(dead_code))]
mod icon;
#[cfg_attr(not(windows), allow(dead_code))]
mod log;
#[cfg(windows)]
mod setup;
#[cfg_attr(not(windows), allow(dead_code))]
mod strings;
#[cfg(windows)]
mod tray;
#[cfg(windows)]
mod update;
#[cfg(windows)]
mod win;

/// What differs per system, behind one interface: `init`, `fatal`,
/// `already_running`, `pump`/`wait` (the event loop the tray lives on),
/// `Hotkey`/`KeyEvent` (+ `since_last_seen`/`forget_press`), `tapped`,
/// `insert`/`Restore`, `copy`, `Overlay`, `autostart`, `open_folder`.
#[cfg(windows)]
mod platform {
    pub use crate::win::*;
}

/// A log line (the app's log file, and stderr).
#[cfg_attr(not(windows), allow(dead_code))]
pub fn log_line(message: &str) {
    log::line(message);
}

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    // SAFETY: first thing in main, before any other thread exists.
    unsafe { hushkey_core::init_process() };
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match setup::Start::from_args(&args) {
        Ok(setup::Start::App { after_update }) => return app::run(after_update),
        Ok(setup::Start::Install { from_python }) => {
            log::open();
            setup::install(from_python)
        }
        Ok(setup::Start::Uninstall) => {
            log::open();
            setup::uninstall()
        }
        Err(e) => Err(e),
    };
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => platform::fatal(&e),
    }
}

#[cfg(not(windows))]
fn main() -> std::process::ExitCode {
    eprintln!(
        "hushkey (Rust) supports Windows so far; on this system use the Python          hushkey, or hushkey-cli for the recognition alone."
    );
    std::process::ExitCode::from(2)
}
