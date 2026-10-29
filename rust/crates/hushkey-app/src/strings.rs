//! The app's words in the system's language: English, German, Italian,
//! Spanish, French, Chinese and Kannada (the languages it dictates), and
//! English for any other.

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
    /// `{paste}`: the system's paste keys.
    copied: &'static str,
    /// Ctrl+V as this language writes it (macOS: always Cmd+V).
    paste_keys: &'static str,
    pub listen: &'static str,
    autostart_windows: &'static str,
    autostart_login: &'static str,
    pub open_logs: &'static str,
    pub quit: &'static str,
    pub model_loading: &'static str,
    pub nothing_heard: &'static str,
    pub update_check: &'static str,
    pub update_install: &'static str,
    pub update_running: &'static str,
    pub up_to_date: &'static str,
    pub update_failed: &'static str,
}

impl Strings {
    /// "Start with Windows", or "Start at login" elsewhere.
    pub fn autostart(&self) -> &'static str {
        if cfg!(windows) {
            self.autostart_windows
        } else {
            self.autostart_login
        }
    }

    /// "copied — paste with Ctrl+V" (Cmd+V on macOS).
    pub fn copied(&self) -> String {
        let keys = if cfg!(target_os = "macos") {
            "Cmd+V"
        } else {
            self.paste_keys
        };
        fill(self.copied, &[("paste", keys)])
    }
}

pub static EN: Strings = Strings {
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
    copied: "copied — paste with {paste}",
    paste_keys: "Ctrl+V",
    listen: "Continuous listening",
    autostart_windows: "Start with Windows",
    autostart_login: "Start at login",
    open_logs: "Open logs",
    quit: "Quit",
    model_loading: "loading {model} — the first time downloads {size}",
    nothing_heard: "nothing heard",
    update_check: "Check for updates",
    update_install: "Install update: v{version}",
    update_running: "installing the update …",
    up_to_date: "hushkey is up to date",
    update_failed: "update failed",
};

pub static DE: Strings = Strings {
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
    copied: "kopiert — mit {paste} einfügen",
    paste_keys: "Strg+V",
    listen: "Dauerzuhören",
    autostart_windows: "Mit Windows starten",
    autostart_login: "Beim Anmelden starten",
    open_logs: "Logs öffnen",
    quit: "Beenden",
    model_loading: "lade {model} — beim ersten Mal werden {size} heruntergeladen",
    nothing_heard: "nichts gehört",
    update_check: "Nach Updates suchen",
    update_install: "Update installieren: v{version}",
    update_running: "das Update wird installiert …",
    up_to_date: "hushkey ist aktuell",
    update_failed: "Update fehlgeschlagen",
};

pub static IT: Strings = Strings {
    idle: "pronto",
    loading: "caricamento del modello …",
    recording: "registrazione …",
    transcribing: "trascrizione …",
    listening: "in ascolto",
    hearing: "ti sento …",
    failed: "errore",
    model_menu: "Modello",
    key_menu: "Tasto push-to-talk",
    lang_menu: "Lingua",
    lang_auto: "Rilevamento automatico",
    insert_menu: "Metodo di inserimento",
    insert_paste: "Incolla (appunti)",
    insert_type: "Digita (tasti)",
    history_menu: "Dettature recenti",
    history_empty: "(ancora nessuna)",
    copied: "copiato — incolla con {paste}",
    paste_keys: "Ctrl+V",
    listen: "Ascolto continuo",
    autostart_windows: "Avvia con Windows",
    autostart_login: "Avvia all'accesso",
    open_logs: "Apri i log",
    quit: "Esci",
    model_loading: "caricamento di {model} — la prima volta scarica {size}",
    nothing_heard: "non ho sentito nulla",
    update_check: "Cerca aggiornamenti",
    update_install: "Installa aggiornamento: v{version}",
    update_running: "installazione dell'aggiornamento …",
    up_to_date: "hushkey è aggiornato",
    update_failed: "aggiornamento non riuscito",
};

pub static ES: Strings = Strings {
    idle: "listo",
    loading: "cargando el modelo …",
    recording: "grabando …",
    transcribing: "transcribiendo …",
    listening: "escuchando",
    hearing: "te escucho …",
    failed: "error",
    model_menu: "Modelo",
    key_menu: "Tecla para hablar",
    lang_menu: "Idioma",
    lang_auto: "Detección automática",
    insert_menu: "Método de inserción",
    insert_paste: "Pegar (portapapeles)",
    insert_type: "Escribir (pulsaciones)",
    history_menu: "Dictados recientes",
    history_empty: "(ninguno todavía)",
    copied: "copiado — pega con {paste}",
    paste_keys: "Ctrl+V",
    listen: "Escucha continua",
    autostart_windows: "Iniciar con Windows",
    autostart_login: "Abrir al iniciar sesión",
    open_logs: "Abrir registros",
    quit: "Salir",
    model_loading: "cargando {model} — la primera vez descarga {size}",
    nothing_heard: "no se oyó nada",
    update_check: "Buscar actualizaciones",
    update_install: "Instalar actualización: v{version}",
    update_running: "instalando la actualización …",
    up_to_date: "hushkey está actualizado",
    update_failed: "la actualización falló",
};

pub static FR: Strings = Strings {
    idle: "prêt",
    loading: "chargement du modèle …",
    recording: "enregistrement …",
    transcribing: "transcription …",
    listening: "à l'écoute",
    hearing: "je t'écoute …",
    failed: "erreur",
    model_menu: "Modèle",
    key_menu: "Touche push-to-talk",
    lang_menu: "Langue",
    lang_auto: "Détection automatique",
    insert_menu: "Méthode d'insertion",
    insert_paste: "Coller (presse-papiers)",
    insert_type: "Taper (frappes)",
    history_menu: "Dictées récentes",
    history_empty: "(aucune pour l'instant)",
    copied: "copié — collez avec {paste}",
    paste_keys: "Ctrl+V",
    listen: "Écoute continue",
    autostart_windows: "Démarrer avec Windows",
    autostart_login: "Démarrer à l'ouverture de session",
    open_logs: "Ouvrir les journaux",
    quit: "Quitter",
    model_loading: "chargement de {model} — la première fois télécharge {size}",
    nothing_heard: "rien entendu",
    update_check: "Rechercher des mises à jour",
    update_install: "Installer la mise à jour : v{version}",
    update_running: "installation de la mise à jour …",
    up_to_date: "hushkey est à jour",
    update_failed: "échec de la mise à jour",
};

pub static ZH: Strings = Strings {
    idle: "就绪",
    loading: "正在加载模型 …",
    recording: "正在录音 …",
    transcribing: "正在转写 …",
    listening: "正在聆听",
    hearing: "正在听你说 …",
    failed: "错误",
    model_menu: "模型",
    key_menu: "按住说话键",
    lang_menu: "语言",
    lang_auto: "自动检测",
    insert_menu: "插入方式",
    insert_paste: "粘贴（剪贴板）",
    insert_type: "键入（按键）",
    history_menu: "最近的听写",
    history_empty: "（暂无）",
    copied: "已复制 — 用 {paste} 粘贴",
    paste_keys: "Ctrl+V",
    listen: "持续聆听",
    autostart_windows: "随 Windows 启动",
    autostart_login: "登录时启动",
    open_logs: "打开日志",
    quit: "退出",
    model_loading: "正在加载 {model} — 首次需下载 {size}",
    nothing_heard: "没有听到内容",
    update_check: "检查更新",
    update_install: "安装更新：v{version}",
    update_running: "正在安装更新 …",
    up_to_date: "hushkey 已是最新版本",
    update_failed: "更新失败",
};

pub static KN: Strings = Strings {
    idle: "ಸಿದ್ಧ",
    loading: "ಮಾದರಿಯನ್ನು ಲೋಡ್ ಮಾಡಲಾಗುತ್ತಿದೆ …",
    recording: "ರೆಕಾರ್ಡ್ ಆಗುತ್ತಿದೆ …",
    transcribing: "ಪಠ್ಯಕ್ಕೆ ಬದಲಾಯಿಸಲಾಗುತ್ತಿದೆ …",
    listening: "ಆಲಿಸುತ್ತಿದೆ",
    hearing: "ನಿಮ್ಮನ್ನು ಕೇಳುತ್ತಿದೆ …",
    failed: "ದೋಷ",
    model_menu: "ಮಾದರಿ",
    key_menu: "ಮಾತನಾಡಲು ಒತ್ತುವ ಕೀ",
    lang_menu: "ಭಾಷೆ",
    lang_auto: "ಸ್ವಯಂ ಪತ್ತೆ",
    insert_menu: "ಸೇರಿಸುವ ವಿಧಾನ",
    insert_paste: "ಅಂಟಿಸಿ (ಕ್ಲಿಪ್‌ಬೋರ್ಡ್)",
    insert_type: "ಟೈಪ್ ಮಾಡಿ (ಕೀಲಿಗಳು)",
    history_menu: "ಇತ್ತೀಚಿನ ಡಿಕ್ಟೇಶನ್‌ಗಳು",
    history_empty: "(ಇನ್ನೂ ಇಲ್ಲ)",
    copied: "ನಕಲಿಸಲಾಗಿದೆ — {paste} ಮೂಲಕ ಅಂಟಿಸಿ",
    paste_keys: "Ctrl+V",
    listen: "ನಿರಂತರ ಆಲಿಸುವಿಕೆ",
    autostart_windows: "Windows ಜೊತೆ ಪ್ರಾರಂಭಿಸಿ",
    autostart_login: "ಲಾಗಿನ್ ಆದಾಗ ಪ್ರಾರಂಭಿಸಿ",
    open_logs: "ಲಾಗ್‌ಗಳನ್ನು ತೆರೆಯಿರಿ",
    quit: "ನಿರ್ಗಮಿಸಿ",
    model_loading: "{model} ಲೋಡ್ ಆಗುತ್ತಿದೆ — ಮೊದಲ ಬಾರಿ {size} ಡೌನ್‌ಲೋಡ್ ಆಗುತ್ತದೆ",
    nothing_heard: "ಏನೂ ಕೇಳಿಸಲಿಲ್ಲ",
    update_check: "ಅಪ್‌ಡೇಟ್‌ಗಳಿಗಾಗಿ ಪರಿಶೀಲಿಸಿ",
    update_install: "ಅಪ್‌ಡೇಟ್ ಸ್ಥಾಪಿಸಿ: v{version}",
    update_running: "ಅಪ್‌ಡೇಟ್ ಸ್ಥಾಪಿಸಲಾಗುತ್ತಿದೆ …",
    up_to_date: "hushkey ಇತ್ತೀಚಿನದು",
    update_failed: "ಅಪ್‌ಡೇಟ್ ವಿಫಲವಾಗಿದೆ",
};

/// The words for a language code (`de`, `de-AT`, `zh_CN.UTF-8`, `kn`);
/// English for any language without its own. Chinese means the simplified
/// script: Traditional Chinese (Taiwan, Hong Kong, Macau, `Hant`) gets
/// English rather than the wrong script.
pub fn for_language(code: &str) -> Option<&'static Strings> {
    let code = code.trim();
    let primary = code
        .split(|c: char| !c.is_ascii_alphabetic())
        .next()
        .unwrap_or("");
    let is = |name: &str| primary.eq_ignore_ascii_case(name);
    if is("zh") {
        let upper = code.to_ascii_uppercase();
        let traditional = ["HANT", "TW", "HK", "MO"].iter().any(|tag| {
            upper
                .split(|c: char| !c.is_ascii_alphanumeric())
                .any(|t| t == *tag)
        });
        return (!traditional).then_some(&ZH);
    }
    [
        ("en", &EN),
        ("de", &DE),
        ("it", &IT),
        ("es", &ES),
        ("fr", &FR),
        ("kn", &KN),
    ]
    .into_iter()
    .find(|(name, _)| is(name))
    .map(|(_, strings)| strings)
}

/// The words in the system's language: the first of its preferred
/// languages that has them, else English.
pub fn current() -> &'static Strings {
    system_languages()
        .iter()
        .find_map(|code| for_language(code))
        .unwrap_or(&EN)
}

/// The user's UI languages in order of preference, e.g. `de-DE`
/// (names, not LCIDs: language packs may have no LCID).
#[cfg(windows)]
fn system_languages() -> Vec<String> {
    use windows_sys::Win32::Globalization::{GetUserPreferredUILanguages, MUI_LANGUAGE_NAME};
    let (mut count, mut len) = (0u32, 0u32);
    unsafe {
        if GetUserPreferredUILanguages(
            MUI_LANGUAGE_NAME,
            &mut count,
            std::ptr::null_mut(),
            &mut len,
        ) == 0
        {
            return Vec::new();
        }
        let mut buf = vec![0u16; len as usize];
        if GetUserPreferredUILanguages(MUI_LANGUAGE_NAME, &mut count, buf.as_mut_ptr(), &mut len)
            == 0
        {
            return Vec::new();
        }
        // A double-null-terminated list.
        buf.split(|&c| c == 0)
            .filter(|name| !name.is_empty())
            .map(String::from_utf16_lossy)
            .collect()
    }
}

/// As gettext reads the environment: the locale is the first of LC_ALL,
/// LC_MESSAGES and LANG that is set; unless it is C/POSIX, LANGUAGE (a
/// colon-separated list) comes first.
#[cfg(target_os = "linux")]
fn system_languages() -> Vec<String> {
    linux_languages(|v| std::env::var(v).ok())
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn linux_languages(env: impl Fn(&str) -> Option<String>) -> Vec<String> {
    let locale = ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .filter_map(|v| env(v))
        .find(|v| !v.is_empty())
        .unwrap_or_default();
    let base = locale.split(['.', '@']).next().unwrap_or("");
    if base.is_empty() || base == "C" || base == "POSIX" {
        return Vec::new();
    }
    let mut languages: Vec<String> = env("LANGUAGE")
        .unwrap_or_default()
        .split(':')
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();
    languages.push(locale);
    languages
}

#[cfg(target_os = "macos")]
fn system_languages() -> Vec<String> {
    crate::platform::preferred_languages()
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
fn system_languages() -> Vec<String> {
    Vec::new()
}

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

    #[test]
    fn the_system_language_picks_the_words() {
        let pick = |code: &str| for_language(code).map(|s| s as *const Strings);
        assert_eq!(pick("de-DE"), Some(&DE as *const _));
        assert_eq!(pick("de_AT.UTF-8"), Some(&DE as *const _));
        assert_eq!(pick("it-IT"), Some(&IT as *const _));
        assert_eq!(pick("es-419"), Some(&ES as *const _));
        assert_eq!(pick("fr_CA"), Some(&FR as *const _));
        assert_eq!(pick("zh-Hans-CN"), Some(&ZH as *const _));
        assert_eq!(pick("zh_CN.UTF-8"), Some(&ZH as *const _));
        assert_eq!(pick("kn-IN"), Some(&KN as *const _));
        assert_eq!(pick("en-US"), Some(&EN as *const _));
        for traditional in ["zh-TW", "zh-Hant-HK", "zh_HK.UTF-8", "zh-MO"] {
            assert_eq!(pick(traditional), None, "{traditional}");
        }
        for other in ["pt-BR", "ja", "", "C"] {
            assert_eq!(pick(other), None, "{other}");
        }
    }

    #[test]
    fn linux_reads_the_environment_like_gettext() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |v: &str| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == v)
                    .map(|(_, x)| x.to_string())
            }
        };
        assert_eq!(
            linux_languages(env(&[("LANGUAGE", "pt_BR:de"), ("LANG", "en_US.UTF-8")])),
            ["pt_BR", "de", "en_US.UTF-8"]
        );
        // C ignores LANGUAGE, as gettext does.
        assert!(linux_languages(env(&[("LANGUAGE", "de"), ("LANG", "C.UTF-8")])).is_empty());
        assert_eq!(
            linux_languages(env(&[("LC_ALL", "fr_FR.UTF-8"), ("LANG", "de_DE.UTF-8")])),
            ["fr_FR.UTF-8"]
        );
    }

    #[test]
    fn every_language_keeps_the_placeholders() {
        for s in [&EN, &DE, &IT, &ES, &FR, &ZH, &KN] {
            assert!(s.model_loading.contains("{model}") && s.model_loading.contains("{size}"));
            assert!(s.update_install.contains("{version}"));
            assert!(!s.copied().contains('{'));
        }
        if !cfg!(target_os = "macos") {
            assert!(DE.copied().contains("Strg+V"));
            assert!(EN.copied().contains("Ctrl+V"));
        }
    }
}
