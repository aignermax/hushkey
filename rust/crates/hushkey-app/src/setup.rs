//! The program installs itself (`hushkey --install`), so installers only
//! download it and start it once. `--from-python` also moves an existing
//! Python hushkey over: its settings and history come along, and it is
//! switched off for good (its files stay).

use std::path::Path;

use serde::Deserialize;

use crate::config::{self, Config, Insert};
use crate::history::{self, History};
use crate::log;
use crate::platform::{autostart, setup};

/// What this start of the program is for.
#[derive(Debug, PartialEq, Eq)]
pub enum Start {
    /// The app; `after_update` when an update just put this program in place.
    App {
        after_update: bool,
    },
    Install {
        from_python: bool,
    },
    Uninstall,
    /// Print the version (installers and CI check it).
    Version,
}

impl Start {
    pub fn from_args(args: &[String]) -> Result<Start, String> {
        let has = |flag: &str| args.iter().any(|a| a == flag);
        if let Some(unknown) = args.iter().find(|a| {
            ![
                "--install",
                "--from-python",
                "--uninstall",
                "--after-update",
                "--version",
            ]
            .contains(&a.as_str())
        }) {
            return Err(format!(
                "unknown argument {unknown:?} (known: --install [--from-python], --uninstall, --version)"
            ));
        }
        Ok(if has("--version") {
            Start::Version
        } else if has("--install") {
            Start::Install {
                from_python: has("--from-python"),
            }
        } else if has("--uninstall") {
            Start::Uninstall
        } else {
            Start::App {
                after_update: has("--after-update"),
            }
        })
    }
}

/// Install this program, start it with every login, and start it now.
pub fn install(from_python: bool) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    // Run again by the installed program itself (the Python app's last
    // tray.py does that until it is switched off): nothing to replace.
    let reinstall = exe == setup::installed_exe();
    if !reinstall {
        // A running copy holds the program (Windows) and the key.
        setup::stop_others();
    }
    if from_python {
        import_python(&setup::python_state_dir());
        // Before register: the menu entry replaces the Python one.
        if let Err(e) = setup::retire_python() {
            log::line(&format!(
                "install: the Python hushkey not fully switched off: {e}"
            ));
        }
    }
    let installed = setup::place(&exe)?;
    setup::register(&installed)?;
    autostart::set(true)?;
    log::line(&format!("installed: {}", installed.display()));
    setup::launch(&installed, &[])
}

/// Autostart, menu entry and program gone; settings and log stay.
pub fn uninstall() -> Result<(), String> {
    setup::stop_others();
    autostart::set(false)?;
    setup::unregister()?;
    let installed = setup::installed_exe();
    let me = std::env::current_exe().ok();
    if me.as_deref() != Some(installed.as_path()) && installed.exists() {
        std::fs::remove_file(&installed).map_err(|e| format!("{}: {e}", installed.display()))?;
    }
    log::line("uninstalled");
    Ok(())
}

/// The Python app's settings file, as far as they mean something here.
#[derive(Default, Deserialize)]
#[serde(default)]
struct PythonConfig {
    model: Option<String>,
    ptt_key: Option<String>,
    insert: Option<String>,
    lang: Option<String>,
}

#[derive(Deserialize)]
struct PythonEntry {
    text: String,
}

/// The Python key names, as ours.
fn key_from_python(name: &str) -> Option<&'static str> {
    let ours = match name.trim().to_ascii_lowercase().as_str() {
        "ctrl_r" | "right_ctrl" => "right_ctrl",
        "alt_r" | "alt_gr" | "right_alt" => "right_alt",
        "caps_lock" => "caps_lock",
        "scroll_lock" => "scroll_lock",
        "insert" => "insert",
        other => {
            return config::KEYS
                .iter()
                .find(|(n, _)| *n == other)
                .map(|(n, _)| *n)
        }
    };
    Some(ours)
}

/// Our settings from the Python ones (defaults where they have none, or
/// one we do not know).
fn config_from_python(text: &str) -> Config {
    let python: PythonConfig = serde_json::from_str(text).unwrap_or_default();
    let mut config = Config::default();
    if let Some(model) = python
        .model
        .filter(|m| crate::strings::MODELS.iter().any(|(name, _)| name == m))
    {
        config.model = model;
    }
    if let Some(key) = python.ptt_key.as_deref().and_then(key_from_python) {
        config.key = key.to_string();
    }
    if python.insert.as_deref() == Some("type") {
        config.insert = Insert::Type;
    }
    config.language = python
        .lang
        .filter(|l| !l.is_empty() && l != "auto")
        .map(|l| l.to_ascii_lowercase());
    config
}

/// Settings and history of the Python hushkey in `dir`, where we have none
/// of our own yet.
fn import_python(dir: &Path) {
    let ours = config::state_dir();
    let _ = std::fs::create_dir_all(&ours);
    if !ours.join("config.json").exists() {
        if let Ok(text) = std::fs::read_to_string(dir.join("config.json")) {
            match config_from_python(&text).save() {
                Ok(()) => log::line("install: settings taken over from the Python hushkey"),
                Err(e) => log::line(&format!("install: settings not taken over: {e}")),
            }
        }
    }
    let history = ours.join("history.json");
    if !history.exists() {
        import_history(&dir.join("history.json"), &history);
    }
}

/// The Python history file `from` (newest first) into ours at `to`.
fn import_history(from: &Path, to: &Path) {
    let Ok(text) = std::fs::read_to_string(from) else {
        return;
    };
    let entries: Vec<PythonEntry> = serde_json::from_str(&text).unwrap_or_default();
    let mut history = History::load(to);
    // Oldest first, so the newest ends up on top.
    for entry in entries.iter().take(history::SIZE).rev() {
        if !entry.text.trim().is_empty() {
            history.add(entry.text.trim());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_pick_the_start() {
        let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            Start::from_args(&args(&[])),
            Ok(Start::App {
                after_update: false
            })
        );
        assert_eq!(
            Start::from_args(&args(&["--after-update"])),
            Ok(Start::App { after_update: true })
        );
        assert_eq!(
            Start::from_args(&args(&["--install", "--from-python"])),
            Ok(Start::Install { from_python: true })
        );
        assert_eq!(
            Start::from_args(&args(&["--uninstall"])),
            Ok(Start::Uninstall)
        );
        assert!(Start::from_args(&args(&["--instal"])).is_err());
    }

    #[test]
    fn python_settings_carry_over() {
        let config = config_from_python(
            r#"{"model": "large-v3-turbo", "ptt_key": "ctrl_r", "lang": "de", "insert": "type"}"#,
        );
        assert_eq!(config.model, "large-v3-turbo");
        assert_eq!(config.key, "right_ctrl");
        assert_eq!(config.language.as_deref(), Some("de"));
        assert_eq!(config.insert, Insert::Type);

        let config = config_from_python(r#"{"ptt_key": "caps_lock", "lang": "auto"}"#);
        assert_eq!(config.key, "caps_lock");
        assert_eq!(config.language, None);

        // Unknown names and a broken file leave the defaults.
        let config = config_from_python(r#"{"model": "distil-whatever", "ptt_key": "menu"}"#);
        assert_eq!(config, Config::default());
        assert_eq!(config_from_python("not json"), Config::default());
    }

    #[test]
    fn python_history_carries_over_newest_first() {
        let dir = std::env::temp_dir().join(format!("hk-py-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("history.json"),
            r#"[{"text": "newest", "ts": 3}, {"text": "older", "ts": 2}]"#,
        )
        .unwrap();
        let target = dir.join("ours.json");
        import_history(&dir.join("history.json"), &target);
        let texts: Vec<_> = History::load(&target)
            .entries()
            .iter()
            .map(|e| e.text.clone())
            .collect();
        assert_eq!(texts, ["newest", "older"]);
        std::fs::remove_dir_all(&dir).ok();
    }
}
