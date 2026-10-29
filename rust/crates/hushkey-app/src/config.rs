//! Settings, kept in `<local data>/hushkey-rs/config.json`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Insert {
    /// Clipboard and a paste chord; the previous clipboard text comes back.
    #[default]
    Paste,
    /// One key event per character.
    Type,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// Hold the key, speak, release.
    #[default]
    PushToTalk,
    /// Every utterance is inserted, no key needed.
    Listen,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Updates {
    /// Releases; pre-releases too while running one.
    #[default]
    Auto,
    /// Pre-releases too.
    Pre,
    /// No checks.
    Off,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BrainSettings {
    /// "claudio" or "openai".
    pub kind: String,
    pub url: String,
    /// Claudio profile, or the OpenAI model name.
    pub model: Option<String>,
    pub token: Option<String>,
    /// PEM certificate to trust (Claudio's server.pem).
    pub ca_cert: Option<PathBuf>,
    pub system_prompt: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Whisper model name or file.
    pub model: String,
    /// Language code; detected when absent.
    pub language: Option<String>,
    /// The push-to-talk key (see [`KEYS`]).
    pub key: String,
    pub insert: Insert,
    /// Put the previous clipboard text back after pasting.
    pub restore_clipboard: bool,
    pub mode: Mode,
    /// Pause in ms that ends an utterance when listening.
    pub end_silence_ms: u32,
    /// Voice commands (JSON file) for listening: matched utterances are
    /// reported, not inserted.
    pub commands: Option<PathBuf>,
    /// Who answers utterances when listening; its replies are shown, the
    /// utterances not inserted.
    pub brain: Option<BrainSettings>,
    pub use_gpu: bool,
    pub updates: Updates,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            model: "large-v3-turbo".into(),
            language: None,
            key: "caps_lock".into(),
            insert: Insert::Paste,
            restore_clipboard: true,
            mode: Mode::PushToTalk,
            end_silence_ms: 700,
            commands: None,
            brain: None,
            use_gpu: true,
            updates: Updates::Auto,
        }
    }
}

/// Keys that can be held to talk, and their Windows virtual-key codes.
/// Keys without a typing job, so holding them does not get in the way.
pub const KEYS: &[(&str, u32)] = &[
    ("caps_lock", 0x14),
    ("right_ctrl", 0xA3),
    ("right_alt", 0xA5),
    ("scroll_lock", 0x91),
    ("insert", 0x2D),
    ("f8", 0x77),
    ("f9", 0x78),
    ("f10", 0x79),
    ("f11", 0x7A),
    ("f12", 0x7B),
];

pub fn virtual_key(name: &str) -> Option<u32> {
    let name = name.trim().to_ascii_lowercase().replace([' ', '-'], "_");
    KEYS.iter().find(|(n, _)| *n == name).map(|(_, vk)| *vk)
}

/// The folder for config and log.
pub fn state_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("hushkey-rs")
}

impl Config {
    /// The saved settings, or defaults written to disk on first start.
    pub fn load() -> Result<Config, String> {
        let path = state_dir().join("config.json");
        match std::fs::read_to_string(&path) {
            Ok(text) => match Config::parse(&text) {
                Ok(config) => Ok(config),
                Err(e) => {
                    // A broken file must not keep hushkey from starting (it
                    // may be starting with Windows): put it aside, start
                    // with defaults, and keep it for the user to look at.
                    let aside = path.with_extension("json.broken");
                    std::fs::rename(&path, &aside).map_err(|r| {
                        format!("{}: {e} (and moving it failed: {r})", path.display())
                    })?;
                    crate::log_line(&format!(
                        "settings unreadable ({e}); moved to {} and started with defaults",
                        aside.display()
                    ));
                    let config = Config::default();
                    config.save()?;
                    Ok(config)
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let config = Config::default();
                config.save()?;
                Ok(config)
            }
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    pub fn parse(text: &str) -> Result<Config, String> {
        let config: Config = serde_json::from_str(text).map_err(|e| e.to_string())?;
        if virtual_key(&config.key).is_none() {
            let names: Vec<&str> = KEYS.iter().map(|(n, _)| *n).collect();
            return Err(format!(
                "unknown key {:?}; use one of {}",
                config.key,
                names.join(", ")
            ));
        }
        if let Some(brain) = &config.brain {
            if !matches!(brain.kind.as_str(), "claudio" | "openai") {
                return Err(format!(
                    "brain kind {:?}: use \"claudio\" or \"openai\"",
                    brain.kind
                ));
            }
        }
        Ok(config)
    }

    /// Apply `change` to the settings file as it is now (the user may have
    /// edited it since start), keeping everything else.
    pub fn update(change: impl FnOnce(&mut Config)) -> Result<(), String> {
        let path = state_dir().join("config.json");
        let mut current = match std::fs::read_to_string(&path) {
            Ok(text) => Config::parse(&text).map_err(|e| format!("{}: {e}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Config::default(),
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        change(&mut current);
        current.save()
    }

    pub fn save(&self) -> Result<(), String> {
        let dir = state_dir();
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let path = dir.join("config.json");
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        // Write a sibling, then rename: a crash mid-write leaves the old file.
        let partial = path.with_extension("json.part");
        std::fs::write(&partial, text + "\n").map_err(|e| format!("{}: {e}", partial.display()))?;
        std::fs::rename(&partial, &path).map_err(|e| format!("{}: {e}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_round_trip_and_partial_files_fill_in_defaults() {
        let text = serde_json::to_string(&Config::default()).unwrap();
        assert_eq!(Config::parse(&text).unwrap(), Config::default());
        let partial = Config::parse(r#"{"key": "f9", "language": "de"}"#).unwrap();
        assert_eq!(partial.key, "f9");
        assert_eq!(partial.language.as_deref(), Some("de"));
        assert_eq!(partial.model, "large-v3-turbo");
    }

    #[test]
    fn bad_settings_are_refused_with_the_choices() {
        let err = Config::parse(r#"{"key": "a"}"#).unwrap_err();
        assert!(err.contains("caps_lock"), "{err}");
        assert!(Config::parse(r#"{"modell": "x"}"#).is_err()); // typo
        let err = Config::parse(r#"{"brain": {"kind": "gpt", "url": "x"}}"#).unwrap_err();
        assert!(err.contains("claudio"), "{err}");
    }

    #[test]
    fn key_names_are_forgiving_about_spelling() {
        assert_eq!(virtual_key("Caps Lock"), Some(0x14));
        assert_eq!(virtual_key("right-ctrl"), Some(0xA3));
        assert_eq!(virtual_key("F9"), Some(0x78));
        assert_eq!(virtual_key("space"), None);
    }
}
