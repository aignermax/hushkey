//! Start at login: an XDG autostart entry,
//! `~/.config/autostart/hushkey.desktop`, starting the installed program
//! (or this one, when not installed).

use std::path::PathBuf;

use super::setup::{self, desktop_entry};

fn path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".config"))
        .join("autostart/hushkey.desktop")
}

/// The program autostart starts.
fn target() -> Result<PathBuf, String> {
    let installed = setup::installed_exe();
    if installed.exists() {
        Ok(installed)
    } else {
        std::env::current_exe().map_err(|e| e.to_string())
    }
}

fn entry() -> Result<String, String> {
    Ok(desktop_entry(
        &target()?,
        "X-GNOME-Autostart-enabled=true\n",
    ))
}

/// Whether the autostart entry starts this program. An entry for another
/// copy, or one the desktop's settings switched off, does not count.
pub fn enabled() -> bool {
    let Ok(text) = std::fs::read_to_string(path()) else {
        return false;
    };
    let Ok(expected) = entry() else {
        return false;
    };
    let exec = |text: &str| {
        text.lines()
            .find(|l| l.starts_with("Exec="))
            .map(str::to_string)
    };
    let off = text
        .lines()
        .any(|l| matches!(l.trim(), "Hidden=true" | "X-GNOME-Autostart-enabled=false"));
    !off && exec(&text).is_some() && exec(&text) == exec(&expected)
}

pub fn set(on: bool) -> Result<(), String> {
    let path = path();
    let result = if on {
        let text = entry()?;
        let dir = path.parent().expect("in a folder");
        std::fs::create_dir_all(dir).and_then(|()| std::fs::write(&path, text))
    } else {
        match std::fs::remove_file(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            other => other,
        }
    };
    result.map_err(|e| format!("{}: {e}", path.display()))
}
