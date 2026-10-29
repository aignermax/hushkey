//! The tray icon (hushkey's logo with a state badge) and its menu, laid out
//! like the Python app's: status, model, key, language, insert method,
//! recent dictations, continuous listening, start with Windows, logs, quit.

use std::collections::HashMap;

use tray_icon::menu::{CheckMenuItem, Menu, MenuId, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

use crate::config::{Config, Insert, Mode};
use crate::history::{self, Entry};
use crate::icon::{self, Badge};
use crate::strings::{Strings, KEY_LABELS, LANGUAGES, MODELS};

/// What a menu entry does.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    Model(&'static str),
    Key(&'static str),
    Language(Option<&'static str>),
    Insert(Insert),
    Copy(usize),
    Listen,
    Autostart,
    OpenLogs,
    Quit,
}

pub struct Tray {
    icon: TrayIcon,
    s: &'static Strings,
    status: MenuItem,
    listen: CheckMenuItem,
    autostart: CheckMenuItem,
    models: Vec<(CheckMenuItem, &'static str)>,
    keys: Vec<(CheckMenuItem, &'static str)>,
    languages: Vec<(CheckMenuItem, Option<&'static str>)>,
    inserts: Vec<(CheckMenuItem, Insert)>,
    history: Submenu,
    history_items: Vec<MenuItem>,
    actions: HashMap<MenuId, Action>,
    badge: Option<Badge>,
}

fn image(badge: Badge) -> Icon {
    Icon::from_rgba(icon::tray(badge), icon::SIZE, icon::SIZE).expect("a square RGBA icon")
}

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

impl Tray {
    pub fn new(s: &'static Strings, config: &Config, autostart: bool) -> Result<Tray, String> {
        let mut actions = HashMap::new();
        let mut check = |text: &str, action: Action| {
            let item = CheckMenuItem::new(text, true, false, None);
            actions.insert(item.id().clone(), action);
            item
        };
        let models: Vec<_> = MODELS
            .iter()
            .map(|(name, size)| {
                (
                    check(&format!("{name} ({size})"), Action::Model(name)),
                    *name,
                )
            })
            .collect();
        let keys: Vec<_> = KEY_LABELS
            .iter()
            .map(|(name, label)| (check(label, Action::Key(name)), *name))
            .collect();
        let languages: Vec<_> = LANGUAGES
            .iter()
            .map(|(code, label)| {
                let label = if code.is_none() { s.lang_auto } else { label };
                (check(label, Action::Language(*code)), *code)
            })
            .collect();
        let inserts = vec![
            (
                check(s.insert_paste, Action::Insert(Insert::Paste)),
                Insert::Paste,
            ),
            (
                check(s.insert_type, Action::Insert(Insert::Type)),
                Insert::Type,
            ),
        ];
        let listen = check(s.listen, Action::Listen);
        let autostart_item = check(s.autostart, Action::Autostart);
        let open_logs = MenuItem::new(s.open_logs, true, None);
        let quit = MenuItem::new(s.quit, true, None);
        actions.insert(open_logs.id().clone(), Action::OpenLogs);
        actions.insert(quit.id().clone(), Action::Quit);

        let submenu = |text: &str, items: Vec<&CheckMenuItem>| -> Result<Submenu, String> {
            let menu = Submenu::new(text, true);
            for item in items {
                menu.append(item).map_err(err)?;
            }
            Ok(menu)
        };
        let model_menu = submenu(s.model_menu, models.iter().map(|m| &m.0).collect())?;
        let key_menu = submenu(s.key_menu, keys.iter().map(|k| &k.0).collect())?;
        let lang_menu = submenu(s.lang_menu, languages.iter().map(|l| &l.0).collect())?;
        let insert_menu = submenu(s.insert_menu, inserts.iter().map(|i| &i.0).collect())?;
        let history = Submenu::new(s.history_menu, true);
        let status = MenuItem::new(s.loading, false, None);

        let menu = Menu::new();
        menu.append_items(&[
            &status,
            &PredefinedMenuItem::separator(),
            &model_menu,
            &key_menu,
            &lang_menu,
            &insert_menu,
            &history,
            &PredefinedMenuItem::separator(),
            &listen,
            &autostart_item,
            &open_logs,
            &PredefinedMenuItem::separator(),
            &quit,
        ])
        .map_err(err)?;
        let icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("hushkey")
            .with_icon(image(Badge::Loading))
            .build()
            .map_err(err)?;
        let mut tray = Tray {
            icon,
            s,
            status,
            listen,
            autostart: autostart_item,
            models,
            keys,
            languages,
            inserts,
            history,
            history_items: Vec::new(),
            actions,
            badge: Some(Badge::Loading),
        };
        tray.sync(config, autostart);
        Ok(tray)
    }

    pub fn action(&self, id: &MenuId) -> Option<Action> {
        self.actions.get(id).cloned()
    }

    /// Tick the entries that match `config` (a click toggles a check item
    /// by itself; this puts the radio-like groups right again).
    pub fn sync(&mut self, config: &Config, autostart: bool) {
        for (item, name) in &self.models {
            item.set_checked(*name == config.model);
        }
        for (item, name) in &self.keys {
            item.set_checked(*name == config.key);
        }
        for (item, code) in &self.languages {
            item.set_checked(*code == config.language.as_deref());
        }
        for (item, mode) in &self.inserts {
            item.set_checked(*mode == config.insert);
        }
        self.listen.set_checked(config.mode == Mode::Listen);
        self.autostart.set_checked(autostart);
    }

    /// The badge and the status line (also the tooltip).
    pub fn show(&mut self, badge: Badge, text: &str) {
        if self.badge != Some(badge) {
            let _ = self.icon.set_icon(Some(image(badge)));
            self.badge = Some(badge);
        }
        self.status.set_text(text);
        // Windows limits tooltips to 127 characters.
        let tooltip: String = text.chars().take(127).collect();
        let _ = self.icon.set_tooltip(Some(tooltip));
    }

    pub fn set_history(&mut self, entries: &[Entry]) {
        for item in self.history_items.drain(..) {
            let _ = self.history.remove(&item);
            self.actions.remove(item.id());
        }
        if entries.is_empty() {
            let item = MenuItem::new(self.s.history_empty, false, None);
            let _ = self.history.append(&item);
            self.history_items.push(item);
            return;
        }
        for (i, entry) in entries.iter().enumerate() {
            let item = MenuItem::new(history::label(&entry.text), true, None);
            self.actions.insert(item.id().clone(), Action::Copy(i));
            let _ = self.history.append(&item);
            self.history_items.push(item);
        }
    }
}
