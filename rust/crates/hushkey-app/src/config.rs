//! Settings, kept in `<local data>/hushkey-rs/config.json`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Resolve the local Whisper model against the current hardware at load time.
pub const AUTO_MODEL: &str = "auto";

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
    /// Check automatically for stable releases.
    #[default]
    Auto,
    /// Pre-releases too.
    Pre,
    /// Check for stable releases on request.
    Off,
    /// Check for stable releases and previews on request.
    ManualPre,
}

impl Updates {
    pub fn automatic(self) -> bool {
        matches!(self, Self::Auto | Self::Pre)
    }

    pub fn includes_previews(self) -> bool {
        matches!(self, Self::Pre | Self::ManualPre)
    }

    pub fn from_options(automatic: bool, previews: bool) -> Self {
        match (automatic, previews) {
            (true, false) => Self::Auto,
            (true, true) => Self::Pre,
            (false, false) => Self::Off,
            (false, true) => Self::ManualPre,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    #[default]
    Light,
    Dark,
}

impl Theme {
    fn is_light(&self) -> bool {
        *self == Self::Light
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BrainSettings {
    /// "openai".
    pub kind: String,
    pub url: String,
    /// The model name.
    pub model: Option<String>,
    pub token: Option<String>,
    /// PEM certificate to trust, for a server with its own CA.
    pub ca_cert: Option<PathBuf>,
    pub system_prompt: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Automatic selection, a Whisper model name, or a local model file.
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
    /// A pinned LAN destination; None runs inference locally.
    pub remote: Option<hushkey_net::Peer>,
    /// Advertise and accept pairing requests, never audio without consent.
    pub server_enabled: bool,
    pub updates: Updates,
    #[serde(skip_serializing_if = "Theme::is_light")]
    pub theme: Theme,
    /// Show the first steps (hold the key, speak, let go) until the first
    /// dictation. Set for a new install only: missing means not wanted.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub guide: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            model: AUTO_MODEL.into(),
            language: None,
            key: "caps_lock".into(),
            insert: Insert::Paste,
            restore_clipboard: true,
            mode: Mode::PushToTalk,
            end_silence_ms: 700,
            commands: None,
            brain: None,
            use_gpu: true,
            remote: None,
            server_enabled: true,
            updates: Updates::Auto,
            theme: Theme::Light,
            guide: false,
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
    // Separate profiles are useful for portable installs and for verifying
    // a new LAN release without touching an existing installation's state.
    if let Some(dir) = std::env::var_os("HUSHKEY_DATA_DIR").filter(|p| !p.is_empty()) {
        return PathBuf::from(dir);
    }
    dirs::data_local_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("hushkey-rs")
}

impl Config {
    pub fn check_listen_available(&self) -> Result<(), String> {
        if self.remote.is_some() {
            Err("Remote computation supports push-to-talk only; select local computation for continuous listening.".into())
        } else {
            Ok(())
        }
    }

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
                // A new install (a handover from the Python app has
                // written the file by now): show it the first steps.
                let config = Config {
                    guide: true,
                    ..Config::default()
                };
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
            if brain.kind != "openai" {
                return Err(format!("brain kind {:?}: use \"openai\"", brain.kind));
            }
        }
        if let Some(peer) = &config.remote {
            if peer.id.len() != 64
                || !peer
                    .id
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                || peer.name.trim().is_empty()
                || peer.name.len() > 256
                || peer.name.chars().count() > 64
                || peer.name.chars().any(char::is_control)
                || peer.address.port() == 0
                || peer.address.ip().is_unspecified()
                || peer.address.ip().is_multicast()
            {
                return Err("invalid remote device identity or address".into());
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
    fn auto_is_the_default_but_updates_preserve_explicit_model_and_gpu_choices() {
        let fresh = Config::parse("{}").unwrap();
        assert_eq!(fresh.model, AUTO_MODEL);
        assert!(fresh.use_gpu);
        for model in ["large-v3-turbo", "tiny", "models/custom.bin"] {
            let config =
                Config::parse(&format!(r#"{{"model":"{model}","use_gpu":false}}"#)).unwrap();
            assert_eq!(config.model, model);
            assert!(!config.use_gpu);
        }
    }

    #[test]
    fn update_channels_preserve_existing_settings_and_manual_preferences() {
        assert_eq!(Config::parse("{}").unwrap().updates, Updates::Auto);
        for (value, automatic, previews) in [
            ("auto", true, false),
            ("off", false, false),
            ("pre", true, true),
            ("manual_pre", false, true),
        ] {
            let policy = Config::parse(&format!(r#"{{"updates":"{value}"}}"#))
                .unwrap()
                .updates;
            assert_eq!(policy.automatic(), automatic);
            assert_eq!(policy.includes_previews(), previews);
            assert_eq!(Updates::from_options(automatic, previews), policy);
            assert_eq!(serde_json::to_value(policy).unwrap(), value);
        }
    }

    #[test]
    fn old_settings_keep_local_computation_and_offer_a_consent_gated_server() {
        let config = Config::parse(r#"{"key":"f9"}"#).unwrap();
        assert!(config.remote.is_none());
        assert!(config.server_enabled);
    }

    #[test]
    fn remote_destination_survives_restart_and_continuous_mode_is_rejected() {
        let text = r#"{"remote":{"name":"GPU PC","id":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef","address":"192.168.1.3:47957"},"server_enabled":false}"#;
        let config = Config::parse(text).unwrap();
        assert_eq!(config.remote.as_ref().unwrap().name, "GPU PC");
        assert!(!config.server_enabled);
        let saved = serde_json::to_string(&config).unwrap();
        assert_eq!(Config::parse(&saved).unwrap(), config);
        assert!(config.check_listen_available().is_err());
        assert!(Config::default().check_listen_available().is_ok());
    }

    #[test]
    fn defaults_round_trip_and_partial_files_fill_in_defaults() {
        let text = serde_json::to_string(&Config::default()).unwrap();
        assert_eq!(Config::parse(&text).unwrap(), Config::default());
        let partial = Config::parse(r#"{"key": "f9", "language": "de"}"#).unwrap();
        assert_eq!(partial.key, "f9");
        assert_eq!(partial.language.as_deref(), Some("de"));
        assert_eq!(partial.model, AUTO_MODEL);
    }

    #[test]
    fn existing_profiles_use_light_and_dark_survives_restart() {
        let mut config = Config::parse(r#"{"key":"f9","updates":"pre"}"#).unwrap();
        assert_eq!(config.theme, Theme::Light);
        assert!(serde_json::to_value(&config)
            .unwrap()
            .get("theme")
            .is_none());
        config.theme = Theme::Dark;
        config.restore_clipboard = false;
        let json = serde_json::to_string(&config).unwrap();
        assert_eq!(Config::parse(&json).unwrap(), config);
    }

    #[test]
    fn the_first_steps_are_for_new_installs_only() {
        // Settings written before there were first steps: none shown.
        assert!(!Config::parse(r#"{"key": "f9"}"#).unwrap().guide);
        let new = Config {
            guide: true,
            ..Config::default()
        };
        let text = serde_json::to_string(&new).unwrap();
        assert!(Config::parse(&text).unwrap().guide);
        // Once done, the setting leaves the file.
        let done = serde_json::to_string(&Config::default()).unwrap();
        assert!(!done.contains("guide"), "{done}");
    }

    #[test]
    fn bad_settings_are_refused_with_the_choices() {
        let err = Config::parse(r#"{"key": "a"}"#).unwrap_err();
        assert!(err.contains("caps_lock"), "{err}");
        assert!(Config::parse(r#"{"modell": "x"}"#).is_err()); // typo
        let err = Config::parse(r#"{"brain": {"kind": "gpt", "url": "x"}}"#).unwrap_err();
        assert!(err.contains("openai"), "{err}");
    }

    #[test]
    fn key_names_are_forgiving_about_spelling() {
        assert_eq!(virtual_key("Caps Lock"), Some(0x14));
        assert_eq!(virtual_key("right-ctrl"), Some(0xA3));
        assert_eq!(virtual_key("F9"), Some(0x78));
        assert_eq!(virtual_key("space"), None);
    }
}
