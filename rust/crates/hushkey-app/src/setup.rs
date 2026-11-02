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

/// How an install that this session cannot run yet begins its error.
pub const NOT_READY: &str = "hushkey cannot run in this session yet";

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
        // A Python hushkey that works stays until this program can take its
        // place: switched off for one that cannot read the key, the user
        // would be left with no dictation at all.
        setup::ready().map_err(|e| {
            log::line(&format!("install: the Python hushkey stays for now: {e}"));
            format!("{NOT_READY}: {e}")
        })?;
        import_python(&setup::python_state_dir());
    }
    // The package manager owns the program and the menu entry; only the
    // per-user half (autostart, settings, the Python handover) is
    // hushkey's to do, and the program is started where it already is.
    let installed = if setup::packaged() {
        if !autostart::enabled() {
            autostart::set(true)?;
        }
        log::line(&format!(
            "installed by the package manager: {}",
            exe.display()
        ));
        exe
    } else {
        // A fresh install starts at login; a reinstall keeps the user's
        // choice.
        let fresh = !setup::installed_exe().exists();
        let installed = setup::place(&exe)?;
        setup::register(&installed)?;
        if fresh || from_python {
            autostart::set(true)?;
        }
        log::line(&format!("installed: {}", installed.display()));
        installed
    };
    // Only now, with this program in place and set to start at login: any
    // failure above leaves the Python hushkey as it was.
    if from_python {
        if let Err(e) = setup::retire_python() {
            log::line(&format!(
                "install: the Python hushkey not fully switched off: {e}"
            ));
        }
    }
    setup::launch(&installed, &[])
}

/// Autostart, menu entry and program gone; settings and log stay.
pub fn uninstall() -> Result<(), String> {
    if setup::packaged() {
        // Removing the program is the package manager's to do; undoing the
        // per-user half here would leave a half-removed install behind.
        setup::stop_others();
        autostart::set(false)?;
        log::line("autostart off; remove the package itself with the package manager");
        return Err("hushkey was installed as a package: remove it with \
                    `sudo apt remove hushkey`. Autostart is off already."
            .into());
    }
    setup::stop_others();
    autostart::set(false)?;
    setup::unregister()?;
    let installed = setup::installed_exe();
    // Windows cannot remove the running program: uninstall.ps1 runs a
    // copy, and Settings > Apps the installed program itself, which then
    // leaves the removal to a process that outlives it. Linux and macOS
    // can remove it right away.
    #[cfg(windows)]
    if std::env::current_exe().ok().as_deref() == Some(installed.as_path()) {
        setup::remove_after_exit(&installed)?;
        log::line("uninstalled; the program file goes once this process has exited");
        return Ok(());
    }
    match std::fs::remove_file(&installed) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            return Err(format!("{}: {e}", installed.display()));
        }
        _ => {}
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

/// The model the Python app last loaded, from its log
/// (`... model loaded: medium/cuda`). Without one in its config it picked
/// it by the hardware (medium on a GPU, small without), which this program
/// cannot tell apart before its first start.
fn model_from_python_log(log: &str) -> Option<String> {
    log.lines().rev().find_map(|line| {
        let (_, rest) = line.split_once(" model loaded: ")?;
        let name = rest.split('/').next()?.trim();
        (!name.is_empty()).then(|| name.to_string())
    })
}

/// Our settings from the Python ones, as the Python app resolved them: its
/// environment variables (`PTT_KEY`, `WHISPER_MODEL`, `WHISPER_LANG`,
/// `PTT_INSERT`) first, then its config file, then its own defaults (Right
/// Ctrl, German, the model it last ran as `ran`). Names we do not know keep
/// ours.
fn config_from_python(
    text: &str,
    ran: Option<String>,
    env: impl Fn(&str) -> Option<String>,
) -> Config {
    let python: PythonConfig = serde_json::from_str(text).unwrap_or_default();
    let mut config = Config::default();
    let model = env("WHISPER_MODEL")
        .filter(|m| !m.is_empty())
        .or(python.model.filter(|m| !m.is_empty()))
        .or(ran);
    if let Some(model) = model.filter(|m| crate::strings::MODELS.iter().any(|(name, _)| name == m))
    {
        config.model = model;
    }
    let key = env("PTT_KEY")
        .filter(|k| !k.is_empty())
        .or(python.ptt_key.filter(|k| !k.is_empty()))
        .unwrap_or_else(|| "ctrl_r".into());
    if let Some(key) = key_from_python(&key) {
        config.key = key.to_string();
    }
    let insert = env("PTT_INSERT").map(|i| i.trim().to_ascii_lowercase());
    if insert.or(python.insert).as_deref() == Some("type") {
        config.insert = Insert::Type;
    }
    // WHISPER_LANG set (even empty) wins; "" and "auto" detect.
    let lang = env("WHISPER_LANG")
        .or(python.lang.filter(|l| !l.is_empty()))
        .unwrap_or_else(|| "de".into());
    config.language = Some(lang.to_ascii_lowercase()).filter(|l| !l.is_empty() && l != "auto");
    config
}

/// Settings and history of the Python hushkey in `dir`, where we have none
/// of our own yet.
/// Whether a program started with no arguments should install itself first.
///
/// Downloading the program and opening it is how most people will arrive:
/// from the website, or straight from the release. Running once out of the
/// Downloads folder is not what they mean by that — there would be no menu
/// entry, no start at login, and moving or deleting the file would take
/// hushkey with it.
///
/// It stays out of the way where installing is not its business: a package
/// manager put the program there, it is already the installed copy, or an
/// installation exists that this copy is not part of (someone is trying a
/// second one on purpose).
pub fn should_install_itself() -> bool {
    if setup::packaged() {
        return false;
    }
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    let installed = setup::installed_exe();
    exe != installed && !installed.exists()
}

/// Finish a handover that no `--install` ran: a package manager put this
/// program in place while the Python hushkey was still set to start at
/// login. Its settings come over (if this program has none yet) and it is
/// switched off, so the two do not fight over the key.
pub fn complete_python_handover() {
    if !setup::python_still_autostarts() {
        return;
    }
    if let Err(e) = setup::ready() {
        log::line(&format!("the Python hushkey stays for now: {e}"));
        return;
    }
    log::line("a Python hushkey still starts at login: taking over from it");
    import_python(&setup::python_state_dir());
    // It started at login, so this program does from now on. Before it is
    // switched off: without autostart, keep it.
    if !autostart::enabled() {
        if let Err(e) = autostart::set(true) {
            log::line(&format!(
                "start at login not set up ({e}); the Python hushkey stays for now"
            ));
            return;
        }
    }
    if let Err(e) = setup::retire_python() {
        log::line(&format!("the Python hushkey not fully switched off: {e}"));
    }
    // Its .deb, if it was installed that way, is the package manager's to
    // remove: it still owns /opt/hushkey and a menu entry that now starts
    // nothing. Said here too, for anyone reading the log after the fact.
    if setup::old_package_installed() {
        log::line(
            "the old hushkey package is still installed; \
             remove it with: sudo apt remove hushkey",
        );
    }
}

fn import_python(dir: &Path) {
    let ours = config::state_dir();
    let _ = std::fs::create_dir_all(&ours);
    // Without a config file the Python app ran on its defaults; those carry
    // over too (where it was installed at all).
    if !ours.join("config.json").exists() && dir.is_dir() {
        let text = std::fs::read_to_string(dir.join("config.json")).unwrap_or_default();
        let ran = std::fs::read(dir.join("dictate.log"))
            .ok()
            .and_then(|log| model_from_python_log(&String::from_utf8_lossy(&log)));
        match config_from_python(&text, ran, |v| std::env::var(v).ok()).save() {
            Ok(()) => log::line("install: settings taken over from the Python hushkey"),
            Err(e) => log::line(&format!("install: settings not taken over: {e}")),
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
        let none = |_: &str| None;
        let config = config_from_python(
            r#"{"model": "large-v3-turbo", "ptt_key": "ctrl_r", "lang": "en", "insert": "type"}"#,
            None,
            none,
        );
        assert_eq!(config.model, "large-v3-turbo");
        assert_eq!(config.key, "right_ctrl");
        assert_eq!(config.language.as_deref(), Some("en"));
        assert_eq!(config.insert, Insert::Type);

        let config = config_from_python(r#"{"ptt_key": "caps_lock", "lang": "auto"}"#, None, none);
        assert_eq!(config.key, "caps_lock");
        assert_eq!(config.language, None);

        // The Python defaults: Right Ctrl, German.
        for text in ["{}", "not json", ""] {
            let config = config_from_python(text, None, none);
            assert_eq!(config.key, "right_ctrl", "{text}");
            assert_eq!(config.language.as_deref(), Some("de"), "{text}");
            assert_eq!(config.model, Config::default().model);
        }
        // Names we do not know keep ours.
        let config = config_from_python(
            r#"{"model": "distil-whatever", "ptt_key": "menu"}"#,
            None,
            none,
        );
        assert_eq!(config.model, Config::default().model);
        assert_eq!(config.key, Config::default().key);
    }

    #[test]
    fn the_python_environment_wins_over_its_file() {
        let env = |v: &str| match v {
            "PTT_KEY" => Some("f9".to_string()),
            "WHISPER_MODEL" => Some("small".to_string()),
            "WHISPER_LANG" => Some(String::new()),
            "PTT_INSERT" => Some("TYPE".to_string()),
            _ => None,
        };
        let config = config_from_python(
            r#"{"model": "medium", "ptt_key": "ctrl_r", "lang": "de", "insert": "paste"}"#,
            Some("tiny".into()),
            env,
        );
        assert_eq!(config.key, "f9");
        assert_eq!(config.model, "small");
        assert_eq!(config.language, None); // empty: detect
        assert_eq!(config.insert, Insert::Type);
    }

    #[test]
    fn the_model_the_python_app_ran_carries_over() {
        let log = "2026-10-30 07:48:47 model loaded: small/cpu\n\
                   2026-10-30 08:58:07 model loaded: medium/cuda\n\
                   2026-10-30 09:00:00 recording\n";
        assert_eq!(model_from_python_log(log).as_deref(), Some("medium"));
        assert_eq!(model_from_python_log("nothing here"), None);
        let none = |_: &str| None;
        // Without a model in its config, the one it last ran.
        let config = config_from_python("{}", model_from_python_log(log), none);
        assert_eq!(config.model, "medium");
        // Its config names one: that wins.
        let config = config_from_python(r#"{"model": "tiny"}"#, Some("medium".into()), none);
        assert_eq!(config.model, "tiny");
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
