//! The app's words in German or English, by the system's UI language,
//! like the Python app's tray.

pub struct Strings {
    pub idle: &'static str,
    pub loading: &'static str,
    pub recording: &'static str,
    pub transcribing: &'static str,
    pub listening: &'static str,
    pub hearing: &'static str,
    pub failed: &'static str,
    pub model_menu: &'static str,
    pub key_menu: &'static str,
    pub lang_menu: &'static str,
    pub lang_auto: &'static str,
    pub insert_menu: &'static str,
    pub insert_paste: &'static str,
    pub insert_type: &'static str,
    pub history_menu: &'static str,
    pub history_empty: &'static str,
    pub copied: &'static str,
    pub listen: &'static str,
    pub autostart: &'static str,
    pub open_logs: &'static str,
    pub quit: &'static str,
    pub model_loading: &'static str,
    pub nothing_heard: &'static str,
}

pub const EN: Strings = Strings {
    idle: "ready",
    loading: "loading the model …",
    recording: "recording …",
    transcribing: "transcribing …",
    listening: "listening",
    hearing: "hearing you …",
    failed: "error",
    model_menu: "Model",
    key_menu: "Push-to-talk key",
    lang_menu: "Language",
    lang_auto: "Auto-detect",
    insert_menu: "Insert method",
    insert_paste: "Paste (clipboard)",
    insert_type: "Type (keystrokes)",
    history_menu: "Recent dictations",
    history_empty: "(none yet)",
    copied: "copied — paste with Ctrl+V",
    listen: "Continuous listening",
    autostart: "Start with Windows",
    open_logs: "Open logs",
    quit: "Quit",
    model_loading: "loading {model} — the first time downloads {size}",
    nothing_heard: "nothing heard",
};

pub const DE: Strings = Strings {
    idle: "bereit",
    loading: "lädt das Modell …",
    recording: "Aufnahme …",
    transcribing: "transkribiert …",
    listening: "hört zu",
    hearing: "hört dich …",
    failed: "Fehler",
    model_menu: "Modell",
    key_menu: "Push-to-talk-Taste",
    lang_menu: "Sprache",
    lang_auto: "Automatisch erkennen",
    insert_menu: "Einfügemethode",
    insert_paste: "Einfügen (Zwischenablage)",
    insert_type: "Tippen (Tastenanschläge)",
    history_menu: "Letzte Diktate",
    history_empty: "(noch keine)",
    copied: "kopiert — mit Strg+V einfügen",
    listen: "Dauerzuhören",
    autostart: "Mit Windows starten",
    open_logs: "Logs öffnen",
    quit: "Beenden",
    model_loading: "lade {model} — beim ersten Mal werden {size} heruntergeladen",
    nothing_heard: "nichts gehört",
};

/// (model name, download size) for the Model menu, as in the Python app.
pub const MODELS: &[(&str, &str)] = &[
    ("tiny", "~75 MB"),
    ("base", "~150 MB"),
    ("small", "~500 MB"),
    ("medium", "~1.5 GB"),
    ("large-v3-turbo", "~1.6 GB"),
    ("large-v3", "~3 GB"),
];

/// (code, label) for the Language menu; None is auto-detect.
pub const LANGUAGES: &[(Option<&str>, &str)] = &[
    (None, ""),
    (Some("de"), "Deutsch"),
    (Some("en"), "English"),
    (Some("it"), "Italiano"),
    (Some("es"), "Español"),
    (Some("fr"), "Français"),
    (Some("kn"), "Kannada (ಕನ್ನಡ)"),
    (Some("zh"), "中文 (Chinese)"),
];

/// (config name, label) for the key menu: keys that do nothing when held.
pub const KEY_LABELS: &[(&str, &str)] = &[
    ("caps_lock", "Caps Lock"),
    ("right_ctrl", "Right Ctrl"),
    ("f8", "F8"),
    ("f9", "F9"),
    ("scroll_lock", "Scroll Lock"),
];

#[cfg(windows)]
pub fn current() -> &'static Strings {
    // 0x07: German, in the primary-language bits of the UI language id.
    let lang = unsafe { windows_sys::Win32::Globalization::GetUserDefaultUILanguage() };
    if lang & 0x3FF == 0x07 {
        &DE
    } else {
        &EN
    }
}

/// `template` with `{name}` replaced by `value`.
pub fn fill(template: &str, pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .fold(template.to_string(), |text, (name, value)| {
            text.replace(&format!("{{{name}}}"), value)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_key_in_the_menu_is_a_known_key() {
        for (name, _) in KEY_LABELS {
            assert!(crate::config::virtual_key(name).is_some(), "{name}");
        }
    }

    #[test]
    fn templates_fill_in() {
        assert_eq!(
            fill(DE.model_loading, &[("model", "tiny"), ("size", "~75 MB")]),
            "lade tiny — beim ersten Mal werden ~75 MB heruntergeladen"
        );
    }
}
