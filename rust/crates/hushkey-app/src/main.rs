//! `hushkey`: hold a key, speak, release, and the text appears where you
//! type. Built on hushkey's Rust core; Windows first (see
//! docs/superpowers/specs/2026-10-28-rust-app-design.md).

// A tray app: no console window on Windows.
#![cfg_attr(windows, windows_subsystem = "windows")]

mod config;
#[cfg(windows)]
mod log;
#[cfg(windows)]
mod win;

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    // SAFETY: first thing in main, before any other thread exists.
    unsafe { hushkey_core::init_process() };
    win::run()
}

#[cfg(not(windows))]
fn main() -> std::process::ExitCode {
    eprintln!(
        "hushkey (Rust) supports Windows so far; on this system use the Python \
         hushkey, or hushkey-cli for the recognition alone."
    );
    std::process::ExitCode::from(2)
}
