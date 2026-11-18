//! The tray icon (hushkey's logo with a state badge) and its menu, laid out
//! like the Python app's: status, model, key, language, insert method,
//! recent dictations, continuous listening, start with Windows, logs,
//! updates, quit — and, while the Python hushkey left files behind, an
//! entry that removes them.

use std::collections::HashMap;

use tray_icon::menu::{CheckMenuItem, Menu, MenuId, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

use crate::config::{self, Config, Insert, Mode};
use crate::history::{self, Entry};
use crate::icon::{self, Badge};
use crate::strings::{self, Strings, KEY_LABELS, LANGUAGES, MODELS};

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
    /// Install the offered update, or look for one.
    Update,
    /// Remove what the Python hushkey left behind.
    RemoveOld,
    NetworkDiscover,
    NetworkConnect(hushkey_net::Peer),
    NetworkLocal,
    NetworkServer,
    NetworkRevoke,
    NetworkRetry,
    NetworkDiscard,
    #[cfg(windows)]
    NetworkFirewall,
    Quit,
}

pub struct Tray {
    icon: TrayIcon,
    menu: Menu,
    s: &'static Strings,
    status: MenuItem,
    update: MenuItem,
    /// There while the Python hushkey's leftovers are.
    remove_old: Option<MenuItem>,
    listen: CheckMenuItem,
    autostart: CheckMenuItem,
    models: Vec<(CheckMenuItem, &'static str)>,
    keys: Vec<(CheckMenuItem, &'static str)>,
    languages: Vec<(CheckMenuItem, Option<&'static str>)>,
    inserts: Vec<(CheckMenuItem, Insert)>,
    history: Submenu,
    history_items: Vec<MenuItem>,
    network: Submenu,
    network_items: Vec<MenuItem>,
    network_identity: MenuItem,
    network_search: MenuItem,
    local: CheckMenuItem,
    server: CheckMenuItem,
    retry: MenuItem,
    discard: MenuItem,
    actions: HashMap<MenuId, Action>,
    badge: Option<Badge>,
}

fn image(badge: Badge) -> Icon {
    Icon::from_rgba(icon::tray(badge), icon::SIZE, icon::SIZE).expect("a square RGBA icon")
}

/// Menu text shows `&` only when doubled; a single one marks a shortcut.
fn menu_text(text: &str) -> String {
    text.replace('&', "&&")
}

fn network_label(s: &Strings, en: &'static str, de: &'static str) -> &'static str {
    if std::ptr::eq(s, &strings::DE) {
        de
    } else {
        en
    }
}

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

impl Tray {
    /// `old_version`: offer to remove the Python hushkey's leftovers.
    pub fn new(
        s: &'static Strings,
        config: &Config,
        autostart: bool,
        old_version: bool,
    ) -> Result<Tray, String> {
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
        let autostart_item = check(s.autostart(), Action::Autostart);
        // All uses of `check` finish before borrowing `actions` directly.
        let label = |en, de| network_label(s, en, de);
        let network = Submenu::new(label("Network", "Netzwerk"), true);
        let network_identity =
            MenuItem::new(label("This computer: …", "Dieser Rechner: …"), false, None);
        let network_search = MenuItem::new(
            label("Find network computers", "Netzwerkrechner suchen"),
            true,
            None,
        );
        let local = CheckMenuItem::new(
            label("Compute locally", "Lokal berechnen"),
            true,
            config.remote.is_none(),
            None,
        );
        let server = CheckMenuItem::new(
            label(
                "Offer LAN computation (approval required)",
                "Berechnung im LAN anbieten (Genehmigung erforderlich)",
            ),
            true,
            config.server_enabled,
            None,
        );
        let revoke = MenuItem::new(
            label("Revoke device approvals", "Gerätefreigaben widerrufen"),
            true,
            None,
        );
        let retry = MenuItem::new(
            label(
                "Retry failed dictation",
                "Fehlgeschlagenes Diktat erneut versuchen",
            ),
            false,
            None,
        );
        let discard = MenuItem::new(
            label(
                "Discard failed dictation",
                "Fehlgeschlagenes Diktat verwerfen",
            ),
            false,
            None,
        );
        for (id, action) in [
            (network_search.id(), Action::NetworkDiscover),
            (local.id(), Action::NetworkLocal),
            (server.id(), Action::NetworkServer),
            (revoke.id(), Action::NetworkRevoke),
            (retry.id(), Action::NetworkRetry),
            (discard.id(), Action::NetworkDiscard),
        ] {
            actions.insert(id.clone(), action);
        }
        network
            .append_items(&[
                &network_identity,
                &network_search,
                &local,
                &server,
                &revoke,
                &retry,
                &discard,
            ])
            .map_err(err)?;
        #[cfg(windows)]
        {
            let firewall = MenuItem::new(
                label(
                    "Allow network access (private network)",
                    "Netzwerkzugriff erlauben (privates Netz)",
                ),
                true,
                None,
            );
            network.append(&firewall).map_err(err)?;
            actions.insert(firewall.id().clone(), Action::NetworkFirewall);
        }
        network
            .append(&PredefinedMenuItem::separator())
            .map_err(err)?;
        let open_logs = MenuItem::new(s.open_logs, true, None);
        let update = MenuItem::new(s.update_check, true, None);
        actions.insert(update.id().clone(), Action::Update);
        let remove_old = old_version.then(|| MenuItem::new(s.remove_old, true, None));
        if let Some(item) = &remove_old {
            actions.insert(item.id().clone(), Action::RemoveOld);
        }
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
            &network,
            &key_menu,
            &lang_menu,
            &insert_menu,
            &history,
            &PredefinedMenuItem::separator(),
            &listen,
            &autostart_item,
            &open_logs,
            &update,
        ])
        .map_err(err)?;
        if let Some(item) = &remove_old {
            menu.append(item).map_err(err)?;
        }
        menu.append_items(&[&PredefinedMenuItem::separator(), &quit])
            .map_err(err)?;
        let icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu.clone()))
            .with_tooltip("hushkey")
            .with_icon(image(Badge::Loading))
            .build()
            .map_err(err)?;
        let mut tray = Tray {
            icon,
            menu,
            s,
            status,
            update,
            remove_old,
            listen,
            autostart: autostart_item,
            models,
            keys,
            languages,
            inserts,
            history,
            history_items: Vec::new(),
            network,
            network_items: Vec::new(),
            network_identity,
            network_search,
            local,
            server,
            retry,
            discard,
            actions,
            badge: Some(Badge::Loading),
        };
        tray.sync(config, autostart);
        tray.set_network_peers(&[]);
        Ok(tray)
    }

    pub fn action(&self, id: &MenuId) -> Option<Action> {
        self.actions.get(id).cloned()
    }

    /// Replace discovery results, retaining each complete peer in its action.
    pub fn set_network_peers(&mut self, peers: &[hushkey_net::Peer]) {
        for item in self.network_items.drain(..) {
            item.set_enabled(false);
            self.actions.remove(item.id());
            if let Err(e) = self.network.remove(&item) {
                crate::log::line(&format!("network menu entry not removed: {e}"));
            }
        }
        if peers.is_empty() {
            let item = MenuItem::new(
                network_label(self.s, "No computers found", "Keine Rechner gefunden"),
                false,
                None,
            );
            if let Err(e) = self.network.append(&item) {
                crate::log::line(&format!("network menu entry not added: {e}"));
            } else {
                self.network_items.push(item);
            }
            return;
        }
        for peer in peers {
            let short_id: String = peer.id.chars().take(12).collect();
            let text = menu_text(&format!("{} [{short_id}] — {}", peer.name, peer.address));
            let item = MenuItem::new(text, true, None);
            if let Err(e) = self.network.append(&item) {
                crate::log::line(&format!("network menu entry not added: {e}"));
                continue;
            }
            self.actions
                .insert(item.id().clone(), Action::NetworkConnect(peer.clone()));
            self.network_items.push(item);
        }
    }

    pub fn network_searching(&mut self, searching: bool) {
        self.network_search.set_enabled(!searching);
        self.network_search.set_text(if searching {
            network_label(
                self.s,
                "Searching for network computers…",
                "Netzwerkrechner werden gesucht…",
            )
        } else {
            network_label(self.s, "Find network computers", "Netzwerkrechner suchen")
        });
    }

    pub fn network_identity(&mut self, name: &str, id: &str) {
        let label = network_label(self.s, "This computer", "Dieser Rechner");
        let short_id: String = id.chars().take(12).collect();
        self.network_identity
            .set_text(menu_text(&format!("{label}: {name} [{short_id}]")));
    }

    pub fn recovery_available(&mut self, available: bool) {
        self.retry.set_enabled(available);
        self.discard.set_enabled(available);
    }

    /// The Python hushkey's leftovers are gone: so is their entry.
    pub fn old_version_removed(&mut self) {
        if let Some(item) = self.remove_old.take() {
            if let Err(e) = self.menu.remove(&item) {
                // Still shown, but greyed out: it has nothing left to do.
                crate::log::line(&format!("menu entry not removed: {e}"));
                item.set_enabled(false);
            }
            self.actions.remove(item.id());
        }
    }

    /// While the leftovers are being removed, the entry says so and cannot
    /// be picked again.
    pub fn old_version_busy(&mut self, busy: bool) {
        if let Some(item) = &self.remove_old {
            item.set_enabled(!busy);
            item.set_text(if busy {
                self.s.removing_old
            } else {
                self.s.remove_old
            });
        }
    }

    /// The update entry: an offered version, or "check for updates".
    pub fn offer_update(&mut self, version: Option<&str>) {
        let text = match version {
            Some(v) => strings::fill(self.s.update_install, &[("version", v)]),
            None => self.s.update_check.to_string(),
        };
        self.update.set_text(menu_text(&text));
    }

    /// Tick the entries that match `config` (a click toggles a check item
    /// by itself; this puts the radio-like groups right again).
    pub fn sync(&mut self, config: &Config, autostart: bool) {
        for (item, name) in &self.models {
            item.set_checked(*name == config.model);
            item.set_enabled(config.remote.is_none());
        }
        // By key code: the file may spell it "F9" or "Caps Lock".
        let key = config::virtual_key(&config.key);
        for (item, name) in &self.keys {
            item.set_checked(config::virtual_key(name) == key);
        }
        for (item, code) in &self.languages {
            item.set_checked(*code == config.language.as_deref());
        }
        for (item, mode) in &self.inserts {
            item.set_checked(*mode == config.insert);
        }
        self.listen.set_checked(config.mode == Mode::Listen);
        self.autostart.set_checked(autostart);
        self.server.set_checked(config.server_enabled);
        self.local.set_checked(config.remote.is_none());
    }

    /// The badge and the status line (also the tooltip).
    pub fn show(&mut self, badge: Badge, text: &str) {
        if self.badge != Some(badge) {
            let _ = self.icon.set_icon(Some(image(badge)));
            self.badge = Some(badge);
        }
        self.status.set_text(menu_text(text));
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
            let item = MenuItem::new(menu_text(&history::label(&entry.text)), true, None);
            self.actions.insert(item.id().clone(), Action::Copy(i));
            let _ = self.history.append(&item);
            self.history_items.push(item);
        }
    }
}
