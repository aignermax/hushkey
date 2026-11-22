use super::{autostart, network, Action, App, Duration, Instant, Mode};
use crate::config::AUTO_MODEL;
use crate::settings::{Client, Host, Intent, Link, Preferences, SettingsWindow, Snapshot};
use crate::strings::{KEY_LABELS, LANGUAGES, MODELS};
use hushkey_net::Server;
use std::sync::{Arc, LazyLock, OnceLock};

pub(super) struct Settings {
    pub window: SettingsWindow,
    pub feedback: Option<(Instant, String)>,
    next_refresh: Instant,
}

impl Settings {
    pub fn incoming_session(&mut self) -> Option<u64> {
        self.next_refresh = Instant::now();
        #[cfg(target_os = "linux")]
        {
            // Mapping a new X11 or Wayland window can activate it even when
            // started inactive. Only a user gesture opens a cold host window.
            self.window.open_session()
        }
        #[cfg(not(target_os = "linux"))]
        {
            Some(self.window.show_incoming())
        }
    }

    pub fn new() -> Self {
        Self {
            window: SettingsWindow::new(),
            feedback: None,
            next_refresh: Instant::now(),
        }
    }
}

/// Two local builds can have the same release version. Hash the actual program
/// once, off the tray thread, and only if the user opens the window.
fn running_build() -> String {
    static ID: LazyLock<Arc<OnceLock<String>>> = LazyLock::new(|| {
        let id = Arc::new(OnceLock::new());
        let output = id.clone();
        std::thread::spawn(move || {
            let hash = std::env::current_exe()
                .ok()
                .and_then(|p| crate::update::sha256_file(&p).ok())
                .map(|hash| hash[..12].to_string())
                .unwrap_or_else(|| "—".into());
            let _ = output.set(hash);
        });
        id
    });
    ID.get().cloned().unwrap_or_else(|| "…".into())
}

impl App {
    pub(super) fn open_settings(&mut self) {
        let already_open = self.settings.window.visible();
        self.show_settings_window();
        if !already_open {
            self.find_servers(true);
        }
    }

    pub(super) fn show_settings_window(&mut self) -> u64 {
        let session = self.settings.window.show();
        if let Some(incoming) = &mut self.connections.incoming {
            incoming.attach_to(session);
        }
        self.settings.next_refresh = Instant::now();
        session
    }

    pub(super) fn poll_settings(&mut self) {
        if let Some(error) = self.settings.window.take_error() {
            let label = network::words(self.s, "Einstellungsfenster", "Settings window");
            crate::log::line(&format!("{label}: {error}"));
            self.failure_notice(&format!("{label}: {error}"));
        }
        self.drain_settings_actions();
        let now = Instant::now();
        if now >= self.settings.next_refresh && self.settings.window.visible() {
            self.settings.next_refresh = now + Duration::from_millis(300);
            let snapshot = self.settings_snapshot();
            self.settings.window.update(&snapshot);
        }
    }

    pub(super) fn drain_settings_actions(&mut self) {
        for (session, intent) in self.settings.window.actions() {
            self.settings.feedback = None;
            self.apply_setting(session, intent);
            self.settings.next_refresh = Instant::now();
        }
    }

    fn apply_setting(&mut self, session: u64, intent: Intent) {
        let action = match intent {
            Intent::Discover => return self.find_servers(true),
            Intent::Probe => return self.begin_check(),
            Intent::DecidePairing { id, allow } => {
                if self
                    .connections
                    .incoming
                    .as_ref()
                    .is_some_and(|p| p.window_session == Some(session))
                {
                    self.decide_pairing(id, allow);
                }
                return;
            }
            Intent::CancelPairing(id) => return self.cancel_pairing(id),
            Intent::WindowClosed => return self.close_session_pairings(session),
            Intent::Connect(peer) => return self.connect_remote(peer, session),
            Intent::Local => Action::NetworkLocal,
            Intent::Server(on) if on != self.config.server_enabled => Action::NetworkServer,
            Intent::Revoke => Action::NetworkRevoke,
            Intent::Retry => Action::NetworkRetry,
            Intent::Discard => Action::NetworkDiscard,
            Intent::Key(key) => match KEY_LABELS.iter().find(|(k, _)| *k == key) {
                Some(&(k, _)) => Action::Key(k),
                None => return,
            },
            Intent::Language(code) => match LANGUAGES.iter().find(|(c, _)| *c == code.as_deref()) {
                Some(&(c, _)) => Action::Language(c),
                None => return,
            },
            Intent::Model(model) if model == AUTO_MODEL => Action::Model(AUTO_MODEL),
            Intent::Model(model) => match MODELS.iter().find(|(m, _)| *m == model) {
                Some(&(m, _)) => Action::Model(m),
                None => return,
            },
            Intent::Insert(mode) => Action::Insert(mode),
            Intent::RestoreClipboard(on) => Action::RestoreClipboard(on),
            Intent::Listen(on) if on != (self.config.mode == Mode::Listen) => Action::Listen,
            Intent::Autostart(on) if on != autostart::enabled() => Action::Autostart,
            Intent::AutoUpdates(on) => Action::Updates(crate::config::Updates::from_options(
                on,
                self.config.updates.includes_previews(),
            )),
            Intent::IncludePrereleases(on) => Action::Updates(
                crate::config::Updates::from_options(self.config.updates.automatic(), on),
            ),
            Intent::CheckUpdate => return self.check_update(true),
            Intent::InstallUpdate(version) => return self.install_offered_update(&version),
            Intent::CopyDictation(text) => {
                // A dictation may have completed while this click was in transit.
                // Never copy the new occupant of an old row.
                match self
                    .history
                    .entries()
                    .iter()
                    .position(|entry| entry.text == text)
                {
                    Some(index) => Action::Copy(index),
                    None => {
                        return self.failure_notice(network::words(
                            self.s,
                            "Dieses Diktat ist nicht mehr im Verlauf.",
                            "This dictation is no longer in your history.",
                        ))
                    }
                }
            }
            Intent::UseGpu(on) => {
                if self.config.use_gpu != on && self.config.remote.is_none() && self.can_switch() {
                    self.stop_listening(true);
                    self.config.use_gpu = on;
                    self.save(|c| c.use_gpu = on);
                    self.load_engine();
                }
                return;
            }
            Intent::Theme(theme) => Action::Theme(theme),
            Intent::OpenLogs => Action::OpenLogs,
            // Repeated "set" requests must not flip a checkbox back.
            Intent::Server(_) | Intent::Listen(_) | Intent::Autostart(_) => return,
        };
        self.menu(action);
    }

    fn settings_snapshot(&self) -> Snapshot {
        let net = self.network.as_ref();
        let server = net.and_then(|n| n.server.as_ref()).filter(|s| s.alive());
        let mut peers = self.connections.peers.clone();
        if let Some(saved) = &self.config.remote {
            if !peers.iter().any(|p| p.id == saved.id) {
                peers.push(saved.clone());
            }
        }
        Snapshot {
            german: network::german(self.s),
            version: env!("CARGO_PKG_VERSION").into(),
            build: running_build(),
            ready: self.dictation_status().into(),
            update: self.update_status(),
            history: self
                .history
                .entries()
                .iter()
                .map(|entry| entry.text.clone())
                .collect(),
            local_recognition: self.local_recognition_label(),
            preferences: Preferences {
                key: self.config.key.clone(),
                language: self.config.language.clone(),
                model: self.config.model.clone(),
                insert: self.config.insert,
                restore_clipboard: self.config.restore_clipboard,
                listening: self.config.mode == Mode::Listen,
                autostart: autostart::enabled(),
                auto_updates: self.config.updates.automatic(),
                include_prereleases: self.config.updates.includes_previews(),
                use_gpu: self.config.use_gpu,
                theme: self.config.theme,
            },
            host: Host {
                device_id: net.map(|n| n.identity.id()),
                name: net.map(|n| n.identity.name()).unwrap_or_else(|| {
                    network::words(self.s, "Dieser Rechner", "This computer").into()
                }),
                enabled: self.config.server_enabled,
                listening: server.is_some(),
                model_ready: self.engine.is_some(),
                clients: server.map(Server::clients).unwrap_or_default(),
                pending: self
                    .connections
                    .incoming
                    .as_ref()
                    .filter(|pending| pending.window_session.is_some() && pending.live())
                    .map(|pending| pending.pairing.clone()),
                error: self.connections.host_error.clone(),
            },
            client: Client {
                pairing: self
                    .connecting
                    .as_ref()
                    .map(|pending| pending.pairing.clone()),
                pairing_error: self
                    .connections
                    .pairing_error
                    .as_ref()
                    .map(|(peer, reason)| crate::settings::PairingFailure {
                        peer: peer.clone(),
                        reason: reason.clone(),
                        can_retry: self
                            .connections
                            .can_select(peer, self.config.remote.as_ref()),
                    }),
                selected: self.config.remote.clone(),
                peers,
                link: self.connections.link,
                checking: self.connections.checking,
                searching: self.discovering,
                search_status: self.connections.search_status.clone(),
                detail: self.connections.client_status.clone(),
                can_retry: self.has_failed()
                    && !self.busy
                    && self.connecting.is_none()
                    && self.revoke_dialog.is_none(),
            },
            feedback: self
                .settings
                .feedback
                .as_ref()
                .filter(|(t, _)| t.elapsed() < Duration::from_secs(12))
                .map(|(_, s)| s.clone()),
        }
    }

    fn local_recognition_label(&self) -> String {
        use hushkey_core::Acceleration;
        let w = |de, en| network::words(self.s, de, en);
        let (Some(engine), Some(model)) = (&self.engine, self.effective_model()) else {
            return w("Modell nicht bereit", "Model not ready").into();
        };
        let device: String = match engine.acceleration() {
            Acceleration::Gpu { backend, device } if !device.is_empty() && device != backend => {
                format!("{device} · {backend}")
            }
            Acceleration::Gpu { backend, .. } => backend.clone(),
            Acceleration::Cpu if self.gpu_crashed && self.config.use_gpu => w(
                "CPU · GPU beim letzten Start abgestürzt",
                "CPU · GPU crashed at the last start",
            )
            .into(),
            Acceleration::Cpu => "CPU".into(),
            Acceleration::LowGpuMemory {
                free_mib,
                needed_mib,
            } => {
                let (full, free, needed) = if network::german(self.s) {
                    ("CPU · Grafikspeicher belegt", "frei", "nötig")
                } else {
                    ("CPU · graphics memory full", "free", "needed")
                };
                format!("{full} ({free_mib} MiB {free}, {needed_mib} MiB {needed})")
            }
            Acceleration::CpuFallback => {
                w("CPU · GPU nicht verfügbar", "CPU · GPU unavailable").into()
            }
            Acceleration::QuantizedCpu => {
                w("CPU · quantisiertes Modell", "CPU · quantized model").into()
            }
            Acceleration::CpuBuild => w(
                "CPU · Programm ohne GPU-Unterstützung",
                "CPU · CPU-only build",
            )
            .into(),
            Acceleration::Unknown => w("Gerät nicht ermittelt", "Device undetermined").into(),
        };
        format!("{model} · {device}")
    }

    fn dictation_status(&self) -> &'static str {
        let w = |de, en| network::words(self.s, de, en);
        if self.recording.is_some() || self.hearing {
            w("Aufnahme läuft", "Recording")
        } else if self.busy {
            self.s.transcribing
        } else if self.has_failed() {
            w("Diktat wartet", "Dictation needs attention")
        } else if self.connecting.is_some() {
            w("Verbindet …", "Connecting …")
        } else if self.config.remote.is_some() && self.connections.link == Link::Waiting {
            w("Datentest steht aus", "Awaiting data test")
        } else if self.config.remote.is_some()
            && matches!(self.connections.link, Link::Offline | Link::Revoked)
        {
            w("Verbindung getrennt", "Disconnected")
        } else if self.backend.is_none() {
            w(
                "Warte auf Spracherkennung",
                "Waiting for speech recognition",
            )
        } else if self.listening.is_some() {
            self.s.listening
        } else {
            w("Bereit zum Diktieren", "Ready to dictate")
        }
    }
}

#[cfg(all(target_os = "linux", test))]
mod tests {
    use super::Settings;

    #[test]
    fn an_incoming_request_never_maps_a_cold_linux_window() {
        let mut settings = Settings::new();
        assert_eq!(settings.incoming_session(), None);
        assert!(!settings.window.visible());
    }
}
