//! The app itself, the same on every system: the model, push to talk,
//! listening, history, settings and updates. What differs per system (the
//! key, inserting text, the tray's event loop, the overlay, autostart) is
//! behind [`crate::platform`].
//!
//! Threads: the main thread runs the system's event loop (tray, menu) and
//! the app state; the key is watched on its own thread; an inserter thread
//! puts text into windows one dictation at a time; sessions run their own
//! workers. Everything reports to the main thread through one channel.

use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use hushkey_brain::{OpenAiChat, OpenAiConfig};
use hushkey_core::{
    Brain, CommandSet, Engine, Event, EventSink, ListenConfig, ListenSession, Microphone,
    RecordConfig, RecordSession, TranscribeOptions, Transcriber,
};
use tray_icon::menu::MenuEvent;
use tray_icon::TrayIconEvent;

use crate::config::{self, Config, Insert, Mode};
use crate::history::History;
use crate::icon::Badge;
use crate::log;
use crate::network::{self, Network, Recoverable};
use crate::network_dialog;
use crate::platform::{self, autostart, Hotkey, InsertTarget, KeyEvent, Overlay, Restore};
use crate::strings::{self, Strings, MODELS};
use crate::tray::{Action, Tray};
use crate::update::{self, Release};

mod connections;
mod model_selection;
mod settings;
mod updates;
use crate::network::Link;
use connections::Connections;
use settings::Settings;

/// A press shorter than this is a tap: no dictation, and Caps Lock keeps
/// its normal job of toggling.
const MIN_HOLD: Duration = Duration::from_millis(400);
/// While a key is held, the system repeats its key-down every ~30 ms after
/// the initial delay (at most 1 s). Silence longer than this means the
/// release happened where hushkey cannot see it (UAC, Ctrl+Alt+Del, a
/// locked screen). Very generous: repeats have been seen to pause for six
/// seconds and more while the key stayed down, and a dictation cut off
/// mid-sentence is far worse than one that ends late. A release that was
/// missed ends the dictation at the next press and release anyway.
const RELEASE_UNSEEN: Duration = Duration::from_secs(120);
/// A RustDesk session can capture keyboard events, and an elevated window
/// can hide them from the hook. End a dictation if repeats disappear there.
const OUT_OF_SIGHT: Duration = Duration::from_millis(1200);
/// A pause in the held key's repeats worth a line in the log.
const NOTABLE_PAUSE: Duration = Duration::from_millis(1500);
/// How long a notice ("copied …") stays in the overlay.
const NOTICE: Duration = Duration::from_secs(2);
/// A failure to act on stays longer.
const FAILURE_NOTICE: Duration = Duration::from_secs(8);
/// The first steps' last word ("the text goes where the cursor is").
const GUIDE_DONE: Duration = Duration::from_secs(8);
/// How long the welcome stays up per start: someone who does not want to
/// dictate right now is not followed by it; the next start shows it again.
const GUIDE_UP: Duration = Duration::from_secs(60);
/// Updates are looked for at start and then this often.
const UPDATE_CHECK: Duration = Duration::from_secs(4 * 60 * 60);

/// Overlay dot colours, as the Python app's.
const RED: [u8; 3] = [0xe5, 0x3e, 0x3e];
const ORANGE: [u8; 3] = [0xdd, 0x6b, 0x20];
const GREY: [u8; 3] = [0x71, 0x80, 0x96];
/// The first steps.
const BLUE: [u8; 3] = [0x31, 0x82, 0xce];
const GREEN: [u8; 3] = [0x38, 0xa1, 0x69];

enum Msg {
    Network(network::Event),
    #[cfg(windows)]
    Firewall(Result<(), String>),
    RevokeConfirmed(bool),
    KeyDown,
    KeyUp(InsertTarget),
    /// The automatic choice is known and its files are about to be opened.
    LoadingModel(u64, &'static str),
    /// Engine load number `.0` finished (or failed).
    Loaded(u64, Result<(String, Arc<Engine>), String>),
    /// What the recording has heard so far.
    Preview(u64, String),
    RecordingError(u64, String),
    /// A push-to-talk dictation finished: its text (or why not).
    Dictated(Option<u64>, Result<String, String>),
    /// Listening session number `.0` reported something.
    Listen(u64, Event),
    /// The inserter finished one text.
    Inserted(Result<Restore, String>),
    UpdateChecked(u64, Result<Option<Release>, String>),
    /// An update was put in place: the program to start (`None`: the
    /// password dialog was dismissed), or why not.
    UpdateInstalled(Result<Option<std::path::PathBuf>, String>),
    /// The Python hushkey's leftovers were removed (`false`: the password
    /// dialog was dismissed), or why not.
    OldRemoved(Result<bool, String>),
}

pub fn run(after_update: bool) -> ExitCode {
    platform::init();
    log::open();
    let mut running = platform::already_running();
    if after_update {
        // The updated program starts while the old one is still quitting.
        let deadline = Instant::now() + Duration::from_secs(15);
        while running && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(200));
            running = platform::already_running();
        }
        crate::setup::after_update();
        log::line(&format!("updated to {}", env!("CARGO_PKG_VERSION")));
    }
    // Asked once more it would find our own lock (Windows, Linux).
    if running {
        log::line("already running; this start does nothing");
        return ExitCode::SUCCESS;
    }
    // Before the settings are read, so imported ones are the ones used.
    crate::setup::complete_python_handover();
    let s = strings::current();
    let config = match Config::load() {
        Ok(c) => c,
        Err(e) => return platform::fatal(&e),
    };
    let (tx, rx) = mpsc::channel::<Msg>();
    let network_tx = tx.clone();
    let (network, network_error) = match Network::start(
        config.server_enabled,
        Arc::new(move |event| {
            let _ = network_tx.send(Msg::Network(event));
        }),
    ) {
        Ok(network) => {
            let error = network.server_error.clone();
            (Some(network), error)
        }
        Err(e) => (None, Some(e)),
    };
    let hotkey = match install_hotkey(&config.key, tx.clone()) {
        Ok(h) => h,
        Err(e) => return platform::fatal(&e),
    };
    let old_version = crate::setup::python_leftovers();
    let mut tray = match Tray::new(s, &config, autostart::enabled(), old_version) {
        Ok(t) => t,
        Err(e) => return platform::fatal(&e),
    };
    let overlay = match Overlay::new() {
        Ok(o) => o,
        Err(e) => return platform::fatal(&e),
    };
    let history = History::load(&config::state_dir().join("history.json"));
    tray.set_history(history.entries());
    let paste = Arc::new(AtomicBool::new(config.insert == Insert::Paste));
    let restore_clipboard = Arc::new(AtomicBool::new(config.restore_clipboard));
    let (inserter, inserter_thread) =
        spawn_inserter(paste.clone(), restore_clipboard.clone(), tx.clone());
    log::line(&format!(
        "started, key {}, model {}",
        config.key, config.model
    ));
    let mut app = App {
        s,
        hotkey: Some(hotkey),
        config,
        tx,
        tray,
        overlay,
        history,
        paste,
        restore_clipboard,
        inserter: Some(inserter),
        inserter_thread: Some(inserter_thread),
        engine: None,
        loaded_model: None,
        network,
        connections: Connections::new(),
        settings: Settings::new(),
        backend: None,
        connecting: false,
        discovering: false,
        connect_generation: 0,
        dialogs: Vec::new(),
        deferred_approval: None,
        pending_inserts: 0,
        revoking: false,
        recording: None,
        pressed_at: None,
        longest_pause: Duration::ZERO,
        previewed: false,
        record_generation: 0,
        dictation_target: None,
        listening: None,
        listen_generation: 0,
        draining: None,
        resume_after_drain: false,
        hearing: false,
        load_generation: 0,
        busy: false,
        notice_until: None,
        guide_since: None,
        guide_away: false,
        updates: updates::UpdateState::default(),
        removing_old: false,
        quit_requested: false,
        next_update_check: Instant::now() + Duration::from_secs(30),
    };
    if let Some(network) = &app.network {
        app.tray
            .network_identity(&network.identity.name(), &network.identity.id());
    }
    if app.config.remote.is_some() {
        app.open_settings();
        // Remote-only clients never need to download or load a local model.
        if app.config.mode == Mode::Listen {
            app.config.mode = Mode::PushToTalk;
            app.save(|c| c.mode = Mode::PushToTalk);
        }
        app.begin_check();
    } else {
        app.load_engine();
    }
    if let Some(e) = network_error {
        // Preserve the requested host role so the listener can recover.
        app.connections.host_error = Some(e.clone());
        app.tray.sync(&app.config, autostart::enabled());
        app.failure_notice(&format!("Network: {e}"));
        log::line(&format!("network unavailable: {e}"));
    }
    app.run(rx)
}

fn install_hotkey(key: &str, tx: Sender<Msg>) -> Result<Hotkey, String> {
    Hotkey::install(key, move |event| {
        let _ = tx.send(match event {
            KeyEvent::Down => Msg::KeyDown,
            KeyEvent::Up => Msg::KeyUp(InsertTarget::capture()),
        });
    })
}

/// One thread inserts, so dictations never interleave.
fn spawn_inserter(
    paste: Arc<AtomicBool>,
    restore_clipboard: Arc<AtomicBool>,
    tx: Sender<Msg>,
) -> (Sender<(InsertTarget, String)>, JoinHandle<()>) {
    let (text_tx, text_rx) = mpsc::channel::<(InsertTarget, String)>();
    let thread = std::thread::spawn(move || {
        for (target, text) in text_rx {
            let result = platform::insert_into(
                target,
                &text,
                paste.load(Ordering::Relaxed),
                restore_clipboard.load(Ordering::Relaxed),
            );
            let _ = tx.send(Msg::Inserted(result));
        }
    });
    (text_tx, thread)
}

struct App {
    s: &'static Strings,
    hotkey: Option<Hotkey>,
    config: Config,
    tx: Sender<Msg>,
    tray: Tray,
    overlay: Overlay,
    history: History,
    /// Paste (true) or type, as the menu last said; read by the inserter.
    paste: Arc<AtomicBool>,
    restore_clipboard: Arc<AtomicBool>,
    inserter: Option<Sender<(InsertTarget, String)>>,
    inserter_thread: Option<JoinHandle<()>>,
    engine: Option<Arc<Engine>>,
    loaded_model: Option<String>,
    network: Option<Network>,
    connections: Connections,
    settings: Settings,
    backend: Option<Arc<Recoverable>>,
    connecting: bool,
    discovering: bool,
    connect_generation: u64,
    dialogs: Vec<(bool, network_dialog::Pending)>,
    deferred_approval: Option<(hushkey_net::Approval, mpsc::SyncSender<bool>, Instant)>,
    pending_inserts: usize,
    revoking: bool,
    recording: Option<RecordSession>,
    pressed_at: Option<Instant>,
    /// The longest pause in the held key's repeats during this dictation.
    longest_pause: Duration,
    /// This dictation showed some text while recording.
    previewed: bool,
    record_generation: u64,
    /// The window at key release, kept through a remote retry of the same audio.
    dictation_target: Option<InsertTarget>,
    listening: Option<ListenSession>,
    /// Numbers listening sessions, so events of one already stopped are
    /// told apart from those of the current one.
    listen_generation: u64,
    /// A session stopped for a model or language switch: its last
    /// utterance still counts (one stopped by switching listening off
    /// does not).
    draining: Option<u64>,
    /// Listening is to start again once the draining session has let go of
    /// the microphone.
    resume_after_drain: bool,
    /// Someone is speaking (between SpeechStart and SpeechEnd).
    hearing: bool,
    /// Numbers engine loads; only the newest one's result is used.
    load_generation: u64,
    /// A dictation is being transcribed.
    busy: bool,
    /// A notice is in the overlay until then.
    notice_until: Option<Instant>,
    /// When the welcome first came up in this run.
    guide_since: Option<Instant>,
    /// The welcome has had its time in this run (the hints while recording
    /// and the last word after the first dictation still come).
    guide_away: bool,
    updates: updates::UpdateState,
    /// The Python hushkey's leftovers are being removed.
    removing_old: bool,
    /// The updated program is starting; this one goes.
    quit_requested: bool,
    next_update_check: Instant,
}

impl App {
    fn has_failed(&self) -> bool {
        self.backend
            .as_ref()
            .is_some_and(|backend| backend.has_failed())
    }

    fn can_switch(&mut self) -> bool {
        if self.recording.is_some() || self.busy || self.connecting || self.has_failed() {
            self.failure_notice(network::words(self.s,
                "Bitte das laufende Diktat oder die Verbindung beenden; fehlgeschlagenes Diktat erneut versuchen oder verwerfen.",
                "Finish the dictation or connection first; retry or discard any failed dictation."));
            false
        } else {
            true
        }
    }

    fn connect_remote(&mut self, peer: hushkey_net::Peer) {
        let same_identity = self.config.remote.as_ref().is_some_and(|p| p.id == peer.id);
        if self.recording.is_some()
            || self.busy
            || self.connecting
            || (self.has_failed() && !same_identity)
        {
            self.failure_notice(network::words(self.s,
                "Bitte zuerst das Diktat beenden. Eine gespeicherte Aufnahme kann nur mit demselben freigegebenen Rechner erneut verbunden werden; für einen anderen Rechner bitte ausdrücklich verwerfen.",
                "Finish the dictation first. Retained audio can reconnect only to the same approved device; discard explicitly before selecting another device."));
            return;
        }
        if self.network.is_none() {
            self.failure_notice(network::words(
                self.s,
                "Netzwerk nicht verfügbar. Bitte Hush-Key neu starten und Log prüfen.",
                "Network unavailable. Restart Hush-Key and check the log.",
            ));
            return;
        }
        self.stop_listening(false);
        self.config.mode = Mode::PushToTalk;
        self.save(|c| c.mode = Mode::PushToTalk);
        self.connecting = true;
        self.connections.checking = false;
        self.connections.link = Link::Connecting;
        self.connections.client_status = format!(
            "{} · {}",
            peer.name,
            network::words(self.s, "Verbindung wird aufgebaut …", "Connecting …")
        );
        self.connect_generation += 1;
        let text = network::words(
            self.s,
            "Remote-Verbindung wird aufgebaut …",
            "Connecting to remote computation …",
        );
        self.tray.show(Badge::Loading, text);
        self.notice(GREY, text);
        self.network
            .as_ref()
            .unwrap()
            .connect(peer, self.connect_generation);
    }

    fn show_approval(
        &mut self,
        request: hushkey_net::Approval,
        hosting: bool,
        reply: mpsc::SyncSender<bool>,
        expires: Instant,
    ) {
        if expires <= Instant::now() || (hosting && !self.config.server_enabled) {
            let _ = reply.send(false);
            return;
        }
        let title = format!("Hush-Key · LAN · {}", request.code);
        if hosting {
            self.connections.request = Some(request.peer.clone());
        }
        let dialog = network_dialog::request(
            &title,
            &network::consent(self.s, &request, hosting),
            expires.saturating_duration_since(Instant::now()),
            move |allowed| {
                let _ = reply.send(allowed);
            },
        );
        self.dialogs.push((hosting, dialog));
    }

    fn network_event(&mut self, event: network::Event) {
        match event {
            network::Event::Checked {
                generation,
                peer,
                result,
            } => {
                if generation == self.connect_generation
                    && self.config.remote.as_ref().is_some_and(|p| p.id == peer.id)
                {
                    self.end_check(peer, result);
                }
            }
            network::Event::Approval {
                request,
                hosting,
                reply,
                expires,
            } => {
                if hosting
                    && (self.recording.is_some()
                        || self.listening.is_some()
                        || self.busy
                        || self.pending_inserts > 0)
                {
                    // Do not let an unsolicited request steal a dictation's focus.
                    self.deferred_approval = Some((request, reply, expires));
                } else {
                    self.show_approval(request, hosting, reply, expires);
                }
            }
            network::Event::Discovered(result) => {
                self.discovering = false;
                self.tray.network_searching(false);
                match result {
                    Ok(peers) => {
                        self.tray.set_network_peers(&peers);
                        self.connections.peers = peers.clone();
                        self.connections.search_status = format!(
                            "{}: {}",
                            network::words(self.s, "Gefundene Rechner", "Computers found"),
                            peers.len()
                        );
                        if self.connections.quiet_search {
                            return;
                        }
                        if peers.is_empty() {
                            self.failure_notice(network::words(self.s,
                                "Keine Rechner gefunden. Auf dem anderen PC Hush-Key starten und LAN-Berechnung anbieten. Firewall: TCP/UDP 47957 im privaten Netz erlauben. Gast-WLAN kann die Suche blockieren.",
                                "No computers found. Run Hush-Key and offer LAN computation on the other PC. Allow TCP/UDP 47957 on private networks. Guest Wi-Fi may block discovery."));
                        } else {
                            self.notice(
                                GREEN,
                                network::words(
                                    self.s,
                                    "Rechner gefunden — im Netzwerkmenü auswählen",
                                    "Computers found — select one in the network menu",
                                ),
                            );
                        }
                    }
                    Err(e) => {
                        self.connections.search_status = e.clone();
                        if !self.connections.quiet_search {
                            self.failure_notice(&format!("Network: {e}"));
                        }
                    }
                }
            }
            network::Event::Connected { generation, result } => {
                if generation != self.connect_generation {
                    return;
                }
                self.connecting = false;
                match result {
                    Ok((peer, backend)) => {
                        self.connections.paused = false;
                        self.connections.link = Link::Waiting;
                        self.connections.client_status = network::words(
                            self.s,
                            "Gekoppelt · Datentest steht aus",
                            "Paired · awaiting data test",
                        )
                        .into();
                        self.connections.next_check = Instant::now();
                        let retained = self.has_failed()
                            && self.config.remote.as_ref().is_some_and(|p| p.id == peer.id);
                        self.load_generation += 1; // ignore a superseded local model load
                        self.config.remote = Some(peer.clone());
                        self.config.mode = Mode::PushToTalk;
                        self.save(|c| {
                            c.remote = Some(peer);
                            c.mode = Mode::PushToTalk;
                        });
                        if retained {
                            self.backend.as_ref().unwrap().reconnect(&backend);
                        } else {
                            self.backend = Some(backend);
                        }
                        self.tray.recovery_available(self.has_failed());
                        self.notice_until = None;
                        self.overlay.hide();
                        self.idle();
                        self.notice(
                            GREEN,
                            &network::overlay_text(
                                self.config.remote.as_ref(),
                                network::words(
                                    self.s,
                                    "verbunden · verschlüsselt",
                                    "connected · encrypted",
                                ),
                            ),
                        );
                    }
                    Err(e) => {
                        self.connections.link = Link::Offline;
                        self.connections.client_status = format!(
                            "{}: {e}",
                            network::words(
                                self.s,
                                "Verbindung fehlgeschlagen",
                                "Connection failed"
                            )
                        );
                        self.idle();
                        self.failure_notice(&format!("Remote: {e}"));
                        if self.backend.is_none() {
                            self.tray.show(Badge::Failed, &format!("Remote: {e}"));
                        }
                    }
                }
                self.tray.sync(&self.config, autostart::enabled());
            }
        }
    }

    fn run(mut self, rx: Receiver<Msg>) -> ExitCode {
        loop {
            // The system's events first: the tray and its menu live on them.
            if !platform::pump() {
                return self.quit();
            }
            while let Ok(event) = TrayIconEvent::receiver().try_recv() {
                if let Some(action) = Tray::icon_action(event) {
                    self.menu(action);
                }
            }
            while let Ok(event) = MenuEvent::receiver().try_recv() {
                if let Some(action) = self.tray.action(&event.id) {
                    if action == Action::Quit {
                        return self.quit();
                    }
                    self.menu(action);
                }
            }
            while let Ok(msg) = rx.try_recv() {
                self.handle(msg);
            }
            self.dialogs.retain(|(_, dialog)| !dialog.done());
            self.poll_settings();
            self.poll_connections();
            self.restart_after_update();
            if self.recording.is_none()
                && self.listening.is_none()
                && !self.busy
                && self.pending_inserts == 0
            {
                if let Some((request, reply, expires)) = self.deferred_approval.take() {
                    self.show_approval(request, true, reply, expires);
                }
            }
            if self.quit_requested {
                return self.quit();
            }
            let quiet = platform::since_last_seen();
            if self.recording.is_some() {
                self.longest_pause = self.longest_pause.max(quiet.unwrap_or_default());
            }
            if self.recording.is_some() && quiet.is_some_and(|q| q > OUT_OF_SIGHT) {
                let target = InsertTarget::capture();
                if let Some(app) = target.may_hide_key_release() {
                    log::line(&format!(
                        "the key is out of sight in {app}; ending the dictation"
                    ));
                    platform::forget_press();
                    self.key_up(target);
                }
            }
            if let Some(quiet) = quiet.filter(|q| *q > RELEASE_UNSEEN) {
                platform::forget_press();
                if self.recording.is_some() {
                    log::line(&format!(
                        "the key's release was not seen ({} ms without its repeats); ending the dictation",
                        quiet.as_millis()
                    ));
                    self.key_up(InsertTarget::capture());
                }
            }
            if Instant::now() > self.next_update_check {
                self.next_update_check = Instant::now() + UPDATE_CHECK;
                if self.config.updates.automatic() {
                    self.check_update(false);
                }
            }
            self.notify_available_update();
            if self
                .notice_until
                .is_some_and(|until| Instant::now() > until)
            {
                self.notice_until = None;
                self.overlay.hide();
                self.guide();
            }
            if !self.guide_away
                && self
                    .guide_since
                    .is_some_and(|since| since.elapsed() > GUIDE_UP)
            {
                self.guide_away = true;
                if self.config.guide && self.waiting() && self.notice_until.is_none() {
                    self.overlay.hide();
                    log::line("first steps put away until the next start");
                }
            }
            // Sleep until a system event arrives, or poll the channels
            // again soon.
            platform::wait(Duration::from_millis(50));
        }
    }

    fn quit(mut self) -> ExitCode {
        for (_, dialog) in &self.dialogs {
            dialog.cancel();
        }
        if let Some((_, reply, _)) = self.deferred_approval.take() {
            let _ = reply.send(false);
        }
        self.overlay.hide();
        if let Some(mut session) = self.recording.take() {
            let _ = session.cancel();
        }
        if let Some(mut session) = self.listening.take() {
            let _ = session.stop();
        }
        // A paste in progress owns the clipboard; let it finish (render,
        // restore) rather than leave an entry nobody can render.
        self.inserter = None;
        if let Some(thread) = self.inserter_thread.take() {
            let _ = thread.join();
        }
        log::line("quit");
        ExitCode::SUCCESS
    }

    fn insert(&mut self, target: InsertTarget, text: String) {
        if let Some(inserter) = &self.inserter {
            if inserter.send((target, text)).is_ok() {
                self.pending_inserts += 1;
            }
        }
    }

    fn load_engine(&mut self) {
        self.engine = None;
        self.loaded_model = None;
        self.backend = None;
        if let Some(network) = &self.network {
            *network.engine.lock().unwrap_or_else(|p| p.into_inner()) = None;
        }
        self.load_generation += 1;
        let generation = self.load_generation;
        let model = self.config.model.clone();
        let use_gpu = self.config.use_gpu;
        self.show_loading((model != config::AUTO_MODEL).then_some(model.as_str()));
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = model_selection::load(&model, use_gpu, |name| {
                let _ = tx.send(Msg::LoadingModel(generation, name));
            })
            .map(|(name, engine)| {
                log::line(&format!(
                    "Whisper acceleration: {:?}",
                    engine.acceleration()
                ));
                // Only a GPU has pipelines to build.
                if engine.on_gpu() {
                    warm_up(&engine);
                }
                (name.to_owned(), Arc::new(engine))
            })
            .map_err(|e| e.to_string());
            let _ = tx.send(Msg::Loaded(generation, result));
        });
    }

    fn show_loading(&mut self, model: Option<&str>) {
        let text = match model {
            Some(model) => {
                let size = MODELS
                    .iter()
                    .find(|(name, _)| *name == model)
                    .map_or("?", |(_, size)| size);
                strings::fill(self.s.model_loading, &[("model", model), ("size", size)])
            }
            None => self.s.loading.into(),
        };
        self.tray.show(Badge::Loading, &text);
        if self.config.guide && self.recording.is_none() && !self.busy {
            // A new install downloads its model first, which takes a
            // while: say so where it is seen, not only in the tray.
            self.overlay.show(GREY, &text);
        }
    }

    pub(super) fn effective_model(&self) -> Option<&str> {
        self.loaded_model.as_deref()
    }

    /// The language for the next transcription: the menu's choice, or
    /// detection.
    fn options(&self) -> TranscribeOptions {
        TranscribeOptions {
            language: Some(
                self.config
                    .language
                    .clone()
                    .unwrap_or_else(|| "auto".into()),
            ),
            ..TranscribeOptions::accurate()
        }
    }

    fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::Network(event) => self.network_event(event),
            Msg::RevokeConfirmed(allowed) => {
                self.revoking = false;
                if allowed {
                    self.connect_generation += 1;
                    self.connecting = false;
                    self.connections.checking = false;
                    self.connections.paused = true;
                    self.connections.link = Link::Revoked;
                    self.connections.client_status = network::words(
                        self.s,
                        "Freigabe widerrufen · zum Koppeln auf Verbinden klicken",
                        "Permission revoked · choose Connect to pair again",
                    )
                    .into();
                    for (_, dialog) in &self.dialogs {
                        dialog.cancel();
                    }
                    if let Some((_, reply, _)) = self.deferred_approval.take() {
                        let _ = reply.send(false);
                    }
                    if let Some(network) = &self.network {
                        if let Err(e) = network.revoke_all() {
                            self.failure_notice(&format!("Revoke: {e}"));
                            return;
                        }
                    }
                    self.notice(GREY, network::words(self.s, "Gerätefreigaben widerrufen. Zum Remote-Rechner bei Bedarf neu verbinden.", "Device permissions revoked. Reconnect to the remote computer when needed."));
                    self.idle();
                }
            }
            #[cfg(windows)]
            Msg::Firewall(result) => match result {
                Ok(()) => self.notice(
                    GREEN,
                    network::words(
                        self.s,
                        "Netzwerkzugriff im privaten LAN erlaubt",
                        "Private LAN access allowed",
                    ),
                ),
                Err(e) => self.failure_notice(&e),
            },
            Msg::LoadingModel(generation, name) => {
                if generation == self.load_generation {
                    self.show_loading(Some(name));
                }
            }
            Msg::Loaded(generation, result) if generation != self.load_generation => {
                // Superseded by a later switch while it loaded.
                drop(result);
            }
            Msg::Loaded(_, Ok((name, engine))) => {
                log::line(&format!("model loaded: {name}"));
                self.loaded_model = Some(name);
                if let Some(network) = &self.network {
                    *network.engine.lock().unwrap_or_else(|p| p.into_inner()) =
                        Some(engine.clone());
                }
                self.backend = Some(Arc::new(Recoverable::new(engine.clone())));
                self.engine = Some(engine);
                if self.config.mode == Mode::Listen {
                    self.set_listening(true);
                } else {
                    self.idle();
                }
            }
            Msg::Loaded(_, Err(e)) => {
                log::line(&format!("model failed: {e}"));
                let text = format!("{}: {e}", self.s.failed);
                self.tray.show(Badge::Failed, &text);
                if self.config.guide {
                    // In place of the loading notice.
                    self.failure_notice(&text);
                }
            }
            Msg::KeyDown => self.key_down(),
            Msg::KeyUp(target) => self.key_up(target),
            Msg::RecordingError(generation, message) => {
                if self.recording.is_some() && generation == self.record_generation {
                    self.overlay.show(
                        ORANGE,
                        &network::overlay_text(self.config.remote.as_ref(), &message),
                    );
                }
            }
            Msg::Preview(generation, text) => {
                if self.recording.is_some() && generation == self.record_generation {
                    self.previewed = true;
                    // The first steps keep saying to let go.
                    let text = if self.config.guide {
                        format!("{} — {text}", self.s.guide_listening)
                    } else {
                        text
                    };
                    self.overlay.show(
                        RED,
                        &network::overlay_text(self.config.remote.as_ref(), &text),
                    );
                }
            }
            Msg::Dictated(epoch, _) if !network::current_result(epoch, self.connect_generation) => {
                self.busy = false;
                if let Some(backend) = &self.backend {
                    backend.reject_result();
                }
                self.tray.recovery_available(self.has_failed());
                self.idle();
                self.failure_notice(network::words(self.s,
                    "Remote-Ergebnis nach Widerruf verworfen. Zum Wiederholen mit demselben Gerät neu verbinden.",
                    "Remote result discarded after revocation. Reconnect to the same device to retry."));
            }
            Msg::Dictated(_, Ok(text)) => {
                if let Some(backend) = &self.backend {
                    backend.acknowledge();
                }
                self.busy = false;
                self.tray.recovery_available(false);
                let target = self.dictation_target.take();
                let text = text.trim().to_string();
                if text.is_empty() {
                    // Silence from the microphone shows as no preview.
                    log::line(&format!(
                        "nothing heard ({} while recording)",
                        if self.previewed {
                            "text shown"
                        } else {
                            "no text shown"
                        }
                    ));
                    self.notice(GREY, self.s.nothing_heard);
                    self.idle();
                } else {
                    log::line(&format!("dictation: {} chars", text.chars().count()));
                    self.overlay.hide();
                    // Kept before inserting: a misdirected paste is recoverable.
                    self.history.add(&text);
                    self.tray.set_history(self.history.entries());
                    self.insert(target.unwrap_or_else(InsertTarget::capture), text + " ");
                }
            }
            Msg::Dictated(_, Err(e)) => {
                self.busy = false;
                log::line(&format!("dictation failed: {e}"));
                self.tray.recovery_available(self.has_failed());
                self.idle();
                let hint = network::words(
                    self.s,
                    "Diktat fehlgeschlagen. Im Netzwerkmenü erneut versuchen oder verwerfen.",
                    "Dictation failed. Retry or discard it in the network menu.",
                );
                self.failure_notice(&network::overlay_text(
                    self.config.remote.as_ref(),
                    &format!("{hint} {e}"),
                ));
            }
            Msg::Inserted(result) => {
                self.pending_inserts = self.pending_inserts.saturating_sub(1);
                let arrived = result.as_ref().is_ok_and(platform::Restore::arrived);
                match result {
                    #[cfg(windows)]
                    Ok(Restore::ManualPaste(app)) => {
                        log::line("dictation copied for manual paste");
                        let hint = self.s.copied(app.as_deref());
                        self.notice_for(GREY, &hint, FAILURE_NOTICE);
                    }
                    Ok(restore) => log::line(&format!("inserted, clipboard: {restore:?}")),
                    Err(e) => {
                        log::line(&format!("insert failed: {e}"));
                        self.failure_notice(&format!("{}: {e}", self.s.failed));
                    }
                }
                if arrived && self.config.guide {
                    // The first dictation is in: the first steps are done.
                    log::line("first steps done");
                    self.config.guide = false;
                    self.save(|c| c.guide = false);
                    // Not over a dictation that has started since.
                    if self.waiting() {
                        self.notice_for(GREEN, self.s.guide_done, GUIDE_DONE);
                    }
                }
                if !self.busy && self.recording.is_none() && !self.hearing {
                    self.idle();
                }
            }
            Msg::UpdateChecked(id, result) => self.update_checked(id, result),
            Msg::UpdateInstalled(result) => self.update_installed(result),
            Msg::OldRemoved(result) => {
                self.removing_old = false;
                match result {
                    Ok(true) => {
                        log::line("the old version is removed");
                        self.tray.old_version_removed();
                        self.notice(GREY, self.s.removed_old);
                    }
                    Ok(false) => {
                        // Dismissed; the entry stays for later.
                        self.tray.old_version_busy(false);
                        self.overlay.hide();
                        self.guide();
                    }
                    Err(e) => {
                        log::line(&format!("removing the old version: {e}"));
                        let text = if e.contains(crate::setup::PACKAGE_BUSY) {
                            self.s.package_busy
                        } else {
                            self.s.remove_old_failed
                        };
                        self.failure_notice(text);
                        // What could go did; the entry stays only for the rest.
                        if crate::setup::python_leftovers() {
                            self.tray.old_version_busy(false);
                        } else {
                            self.tray.old_version_removed();
                        }
                    }
                }
            }
            Msg::Listen(generation, event) => {
                if generation == self.listen_generation && self.listening.is_some() {
                    self.listen_event(event);
                } else if Some(generation) == self.draining {
                    match event {
                        Event::Utterance { transcript, .. } => self.utterance(&transcript.text),
                        Event::Stopped => {
                            self.draining = None;
                            self.history.close();
                            if std::mem::take(&mut self.resume_after_drain)
                                && self.config.mode == Mode::Listen
                            {
                                self.set_listening(true);
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    fn notice(&mut self, colour: [u8; 3], text: &str) {
        self.notice_for(colour, text, NOTICE);
    }

    /// A notice that something did not work, long enough to be read; the
    /// details are in the log.
    fn failure_notice(&mut self, text: &str) {
        self.settings.feedback = Some((Instant::now(), text.to_owned()));
        self.notice_for(GREY, text, FAILURE_NOTICE);
    }

    fn notice_for(&mut self, colour: [u8; 3], text: &str, how_long: Duration) {
        self.overlay.show(colour, text);
        self.notice_until = Some(Instant::now() + how_long);
    }

    fn idle(&mut self) {
        if self.backend.is_none() {
            return; // still loading: the tray says so
        }
        let state = if self.listening.is_some() {
            self.s.listening
        } else {
            self.s.idle
        };
        let text = if self.config.remote.is_some() {
            format!("hushkey {} — {state}", env!("CARGO_PKG_VERSION"))
        } else {
            format!(
                "hushkey {} · {} — {state}",
                env!("CARGO_PKG_VERSION"),
                self.config.model
            )
        };
        let badge = if self.listening.is_some() {
            Badge::Listening
        } else {
            Badge::None
        };
        self.tray.show(
            badge,
            &network::overlay_text(self.config.remote.as_ref(), &text),
        );
        self.guide();
    }

    /// Ready for the key, with nothing else going on.
    fn waiting(&self) -> bool {
        self.backend.is_some()
            && self.config.mode == Mode::PushToTalk
            && self.listening.is_none()
            && self.recording.is_none()
            && !self.busy
            && !self.updates.installing()
    }

    /// The first steps, for a new install until its first dictation: how
    /// to dictate, in the overlay whenever hushkey waits for the key. In
    /// listening mode there is no key: whatever the first steps (or the
    /// model's loading) left in the overlay goes.
    fn guide(&mut self) {
        if !self.config.guide || self.notice_until.is_some() {
            return;
        }
        if self.waiting() {
            if self.guide_away {
                return;
            }
            self.guide_since.get_or_insert_with(Instant::now);
            let key = self.s.key_name(&self.config.key);
            self.overlay
                .show(BLUE, &strings::fill(self.s.guide_hold, &[("key", key)]));
        } else if self.config.mode == Mode::Listen && !self.hearing {
            self.overlay.hide();
        }
    }

    fn key_down(&mut self) {
        // An incoming consent prompt must never hold up capture/key release.
        for (hosting, dialog) in &self.dialogs {
            if *hosting {
                dialog.cancel();
            }
        }
        self.pressed_at = Some(Instant::now());
        if self.listening.is_some()
            || self.recording.is_some()
            || self.busy
            || self.draining.is_some()
            || self.updates.installing()
        {
            return;
        }
        if self.connecting || self.has_failed() {
            self.failure_notice(network::words(self.s,
                "Verbindung läuft oder ein Diktat wartet auf Wiederholung. Bitte im Netzwerkmenü prüfen.",
                "Connecting or a failed dictation needs attention. Check the network menu."));
            return;
        }
        self.longest_pause = Duration::ZERO;
        self.previewed = false;
        self.record_generation += 1;
        let Some(backend) = self.backend.clone() else {
            self.failure_notice(network::words(
                self.s,
                "Berechnung noch nicht bereit. Netzwerkverbindung oder Modell prüfen.",
                "Computation is not ready. Check the network connection or model.",
            ));
            return;
        };
        let config = RecordConfig {
            preview_interval: Duration::from_millis(1500),
            max_duration: self
                .config
                .remote
                .as_ref()
                .map(|_| Duration::from_secs(600)),
            options: self.options(),
            ..RecordConfig::default()
        };
        let tx = self.tx.clone();
        let generation = self.record_generation;
        let sink: EventSink = Arc::new(move |event| match event {
            Event::Preview { text } => {
                let _ = tx.send(Msg::Preview(generation, text));
            }
            Event::Error { message } => {
                log::line(&format!("recording: {message}"));
                let _ = tx.send(Msg::RecordingError(generation, message));
            }
            _ => {}
        });
        let backend: Arc<dyn Transcriber> = backend;
        match RecordSession::start(backend, Box::new(Microphone::new(None)), config, sink) {
            Ok(session) => {
                self.recording = Some(session);
                self.notice_until = None;
                let text = if self.config.guide {
                    self.s.guide_listening
                } else {
                    self.s.recording
                };
                let text = network::overlay_text(self.config.remote.as_ref(), text);
                self.overlay.show(RED, &text);
                self.tray.show(Badge::Recording, &text);
            }
            Err(e) => {
                log::line(&format!("could not record: {e}"));
                self.tray
                    .show(Badge::Failed, &format!("{}: {e}", self.s.failed));
            }
        }
    }

    fn key_up(&mut self, target: InsertTarget) {
        let held = self.pressed_at.take().map(|t| t.elapsed());
        let tap = held.is_some_and(|held| held < MIN_HOLD);
        if tap {
            // Where the key was swallowed, a tap still does its own job
            // (Caps Lock toggles).
            platform::tapped(&self.config.key);
        }
        let Some(mut session) = self.recording.take() else {
            return;
        };
        if tap {
            // Too short to be a dictation: end the recording, insert nothing.
            std::thread::spawn(move || {
                let _ = session.cancel();
            });
            self.overlay.hide();
            self.idle();
            return;
        }
        if self.longest_pause > NOTABLE_PAUSE {
            // Not a reason to end the dictation (see RELEASE_UNSEEN), but
            // worth knowing how long and how often.
            log::line(&format!(
                "the key's repeats paused for {} ms during a press of {} ms",
                self.longest_pause.as_millis(),
                held.unwrap_or_default().as_millis()
            ));
        }
        self.busy = true;
        self.dictation_target = Some(target);
        let text = network::overlay_text(self.config.remote.as_ref(), self.s.transcribing);
        self.overlay.show(ORANGE, &text);
        self.tray.show(Badge::Transcribing, &text);
        let tx = self.tx.clone();
        let remote_epoch = self.config.remote.as_ref().map(|_| self.connect_generation);
        std::thread::spawn(move || {
            let result = session
                .stop()
                .map(|t| t.map(|t| t.text).unwrap_or_default())
                .map_err(|e| e.to_string());
            let _ = tx.send(Msg::Dictated(remote_epoch, result));
        });
    }

    fn menu(&mut self, action: Action) {
        match action {
            Action::NetworkWindow => self.open_settings(),
            Action::NetworkDiscover => self.find_servers(false),
            Action::NetworkConnect(peer) => {
                self.settings.window.show();
                self.connect_remote(peer);
            }
            Action::NetworkLocal => {
                if self.can_switch() {
                    self.stop_listening(false);
                    self.config.remote = None;
                    self.connect_generation += 1;
                    self.connections.checking = false;
                    self.connections.paused = false;
                    self.connections.client_status.clear();
                    self.connections.link = Link::Local;
                    self.backend = None;
                    self.save(|c| c.remote = None);
                    self.load_engine();
                }
            }
            Action::NetworkServer => {
                let enabled = !self.config.server_enabled;
                if let Some(network) = &mut self.network {
                    if let Err(e) = network.set_enabled(enabled) {
                        self.connections.host_error = Some(e.clone());
                        self.failure_notice(&format!("Server: {e}"));
                    } else {
                        self.connections.host_error = None;
                    }
                } else {
                    self.failure_notice(network::words(
                        self.s,
                        "Netzwerk nicht verfügbar. Bitte Hush-Key neu starten.",
                        "Network unavailable. Restart Hush-Key.",
                    ));
                    return;
                }
                if !enabled {
                    for (hosting, dialog) in &self.dialogs {
                        if *hosting {
                            dialog.cancel();
                        }
                    }
                    if let Some((_, reply, _)) = self.deferred_approval.take() {
                        let _ = reply.send(false);
                    }
                }
                self.config.server_enabled = enabled;
                self.save(|c| c.server_enabled = enabled);
                if enabled {
                    self.settings.window.show();
                }
            }
            Action::NetworkRevoke => {
                if !self.revoking {
                    self.revoking = true;
                    let tx = self.tx.clone();
                    let dialog = network_dialog::request("Hush-Key", network::words(self.s,
                        "Alle Gerätefreigaben widerrufen? Geräte müssen danach erneut genehmigt werden.",
                        "Revoke all device permissions? Devices will need approval again."), Duration::from_secs(85),
                        move |allowed| { let _ = tx.send(Msg::RevokeConfirmed(allowed)); });
                    self.dialogs.push((false, dialog));
                }
            }
            Action::NetworkRetry => {
                if !self.busy && self.recording.is_none() && self.has_failed() {
                    if let Some(backend) = self.backend.clone() {
                        self.busy = true;
                        self.tray.recovery_available(false);
                        let text =
                            network::overlay_text(self.config.remote.as_ref(), self.s.transcribing);
                        self.overlay.show(ORANGE, &text);
                        let tx = self.tx.clone();
                        let remote_epoch =
                            self.config.remote.as_ref().map(|_| self.connect_generation);
                        std::thread::spawn(move || {
                            let result = backend.retry().map(|t| t.text).map_err(|e| e.to_string());
                            let _ = tx.send(Msg::Dictated(remote_epoch, result));
                        });
                    }
                }
            }
            Action::NetworkDiscard => {
                if !self.busy {
                    if let Some(backend) = &self.backend {
                        backend.discard();
                    }
                    self.dictation_target = None;
                    self.tray.recovery_available(false);
                    self.overlay.hide();
                    self.idle();
                }
            }
            #[cfg(windows)]
            Action::NetworkFirewall => {
                let tx = self.tx.clone();
                std::thread::spawn(move || {
                    let _ = tx.send(Msg::Firewall(network::allow_firewall()));
                });
            }
            Action::Model(name) => {
                if self.config.model != name && self.config.remote.is_none() && self.can_switch() {
                    // Listening, if on, resumes once the new model loaded.
                    self.stop_listening(true);
                    self.config.model = name.to_string();
                    self.save(|c| c.model = name.to_string());
                    self.load_engine();
                }
            }
            Action::Key(name) => {
                if self.recording.is_some() || self.pressed_at.is_some() {
                    self.failure_notice(network::words(
                        self.s,
                        "Zum Ändern zuerst die Sprechtaste loslassen.",
                        "Release the talk key before changing it.",
                    ));
                    return;
                }
                // One watcher per process: the old one goes first.
                self.hotkey = None;
                match install_hotkey(name, self.tx.clone()) {
                    Ok(h) => {
                        self.hotkey = Some(h);
                        self.config.key = name.to_string();
                        self.save(|c| c.key = name.to_string());
                        // The first steps name the new key.
                        self.guide();
                    }
                    Err(e) => {
                        log::line(&format!("key not changed: {e}"));
                        self.hotkey = install_hotkey(&self.config.key, self.tx.clone()).ok();
                        self.failure_notice(&e);
                    }
                }
            }
            Action::Language(code) => {
                self.config.language = code.map(str::to_string);
                self.save(|c| c.language = code.map(str::to_string));
                if self.listening.is_some() {
                    // A session keeps its language: restart it (once the
                    // old one has stopped, see set_listening).
                    self.stop_listening(true);
                    self.set_listening(true);
                }
            }
            Action::Insert(mode) => {
                self.config.insert = mode;
                self.paste.store(mode == Insert::Paste, Ordering::Relaxed);
                self.save(|c| c.insert = mode);
            }
            Action::RestoreClipboard(restore) => {
                self.config.restore_clipboard = restore;
                self.restore_clipboard.store(restore, Ordering::Relaxed);
                self.save(|c| c.restore_clipboard = restore);
            }
            Action::Theme(theme) => {
                self.config.theme = theme;
                self.save(|c| c.theme = theme);
            }
            Action::Updates(policy) => {
                if self.config.updates != policy && !self.updates.installing() {
                    self.updates.policy_changed(self.config.updates, policy);
                    self.config.updates = policy;
                    self.save(|c| c.updates = policy);
                    self.tray
                        .offer_update(self.updates.offered_version().as_deref());
                    self.next_update_check = Instant::now();
                }
            }
            Action::Copy(i) => {
                if let Some(entry) = self.history.entries().get(i) {
                    match platform::copy(&entry.text) {
                        Ok(()) => {
                            let text = self.s.copied(None);
                            self.settings.feedback = Some((Instant::now(), text.clone()));
                            self.notice(GREY, &text);
                        }
                        Err(e) => {
                            log::line(&format!("copy failed: {e}"));
                            let label =
                                network::words(self.s, "Kopieren fehlgeschlagen", "Copy failed");
                            self.failure_notice(&format!("{label}: {e}"));
                        }
                    }
                }
            }
            Action::Listen => self.set_listening(self.config.mode != Mode::Listen),
            Action::Autostart => {
                let on = !autostart::enabled();
                if let Err(e) = autostart::set(on) {
                    log::line(&format!("autostart: {e}"));
                    self.failure_notice(&e);
                }
            }
            Action::OpenLogs => platform::open_folder(&config::state_dir()),
            Action::Update => {
                if let Some(version) = self.updates.offered_version() {
                    self.install_offered_update(&version);
                } else {
                    self.check_update(true);
                }
            }
            Action::RemoveOld if !self.removing_old => {
                self.removing_old = true;
                // The entry says so while it runs (that can take minutes),
                // and cannot be picked twice.
                self.tray.old_version_busy(true);
                self.notice(GREY, self.s.removing_old);
                let tx = self.tx.clone();
                std::thread::spawn(move || {
                    let _ = tx.send(Msg::OldRemoved(crate::setup::remove_python()));
                });
            }
            Action::RemoveOld => {}
            Action::Quit => {}
        }
        self.tray.sync(&self.config, autostart::enabled());
    }

    /// Change the settings file as it is now (the user may have edited it),
    /// not as it was at start.
    fn save(&mut self, change: impl FnOnce(&mut Config)) {
        if let Err(e) = Config::update(change) {
            log::line(&format!("settings not saved: {e}"));
            let label = network::words(
                self.s,
                "Einstellungen konnten nicht gespeichert werden",
                "Settings could not be saved",
            );
            self.failure_notice(&format!("{label}: {e}"));
        }
    }

    /// End the listening session, if any. Stopping finishes an utterance in
    /// progress, off the UI thread; with `keep_last` (a model or language
    /// switch, listening goes on) that utterance is still inserted,
    /// without (listening switched off) it is dropped.
    fn stop_listening(&mut self, keep_last: bool) {
        if let Some(mut session) = self.listening.take() {
            let (tx, generation) = (self.tx.clone(), self.listen_generation);
            std::thread::spawn(move || {
                let _ = session.stop();
                // The session says Stopped itself, unless it had ended
                // already; a second one is ignored.
                let _ = tx.send(Msg::Listen(generation, Event::Stopped));
            });
            if keep_last {
                self.draining = Some(self.listen_generation);
            } else {
                self.history.close();
            }
            self.hearing = false;
            self.overlay.hide();
        }
    }

    fn set_listening(&mut self, on: bool) {
        if on && self.updates.installing() {
            return;
        }
        if on
            && (self.config.check_listen_available().is_err()
                || self.connecting
                || self.recording.is_some()
                || self.busy
                || self.has_failed())
        {
            self.failure_notice(network::words(self.s,
                "Dauerhören ist im Remote-Modus nicht verfügbar. Wähle zuerst lokale Berechnung und schließe offene Diktate ab.",
                "Continuous listening is unavailable in remote mode. Select local computation and resolve pending dictations first."));
            self.tray.sync(&self.config, autostart::enabled());
            return;
        }
        if !on {
            self.resume_after_drain = false;
            self.stop_listening(false);
            if self.config.mode != Mode::PushToTalk {
                self.config.mode = Mode::PushToTalk;
                self.save(|c| c.mode = Mode::PushToTalk);
            }
            self.tray.sync(&self.config, autostart::enabled());
            self.idle();
            return;
        }
        if self.config.mode != Mode::Listen {
            self.config.mode = Mode::Listen;
            self.save(|c| c.mode = Mode::Listen);
        }
        self.tray.sync(&self.config, autostart::enabled());
        let Some(engine) = self.engine.clone() else {
            return; // starts once the model is loaded
        };
        if self.listening.is_some() {
            return;
        }
        if self.draining.is_some() {
            // The stopping session still holds the microphone: two would
            // capture at once, or the second fail to open it. Start once
            // it has stopped.
            self.resume_after_drain = true;
            return;
        }
        match self.start_listening(engine) {
            Ok(session) => {
                self.listening = Some(session);
                log::line("listening");
                self.idle();
            }
            Err(e) => {
                log::line(&format!("could not listen: {e}"));
                self.config.mode = Mode::PushToTalk;
                self.save(|c| c.mode = Mode::PushToTalk);
                self.tray.sync(&self.config, autostart::enabled());
                self.tray
                    .show(Badge::Failed, &format!("{}: {e}", self.s.failed));
            }
        }
    }

    fn start_listening(&mut self, engine: Arc<Engine>) -> Result<ListenSession, String> {
        let mut listen = ListenConfig::default().with_end_silence(self.config.end_silence_ms);
        listen.options = self.options();
        if let Some(path) = &self.config.commands {
            let json =
                std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
            listen.commands = Some(Arc::new(
                CommandSet::from_json(&json).map_err(|e| e.to_string())?,
            ));
        }
        listen.brain = brain(&self.config)?;
        let tx = self.tx.clone();
        let generation = self.listen_generation + 1;
        let sink: EventSink = Arc::new(move |event| {
            let _ = tx.send(Msg::Listen(generation, event));
        });
        let session = ListenSession::start(engine, Box::new(Microphone::new(None)), listen, sink)
            .map_err(|e| e.to_string())?;
        self.listen_generation = generation;
        Ok(session)
    }

    fn listen_event(&mut self, event: Event) {
        match event {
            Event::SpeechStart { .. } => {
                self.hearing = true;
                self.notice_until = None; // speech replaces an older notice
                self.overlay.show(RED, self.s.hearing);
                self.tray.show(Badge::Recording, self.s.hearing);
            }
            Event::Preview { text } => {
                self.notice_until = None;
                self.overlay.show(RED, &text);
            }
            Event::Utterance { transcript, .. } => self.utterance(&transcript.text),
            Event::SpeechEnd { .. } => {
                self.hearing = false;
                // A command's or reply's notice stays for its time.
                if self.notice_until.is_none() {
                    self.overlay.hide();
                }
                self.idle();
            }
            Event::Command { command, .. } => {
                // The name only: arguments are spoken words, which the log
                // never keeps.
                log::line(&format!("command {}", command.name));
                let text = format!("{} {}", command.name, command.args.join(" "));
                self.notice(GREY, &text);
            }
            Event::Reply { text, .. } => self.notice(GREY, &text),
            Event::Error { message } => log::line(&format!("listening: {message}")),
            Event::Stopped => {
                if self.listening.take().is_some() {
                    // Ended by itself (the microphone went away).
                    self.history.close();
                    self.hearing = false;
                    self.overlay.hide();
                    self.config.mode = Mode::PushToTalk;
                    self.save(|c| c.mode = Mode::PushToTalk);
                    self.tray.sync(&self.config, autostart::enabled());
                    self.idle();
                }
            }
            Event::Final { .. } => {}
        }
    }

    fn utterance(&mut self, text: &str) {
        // With commands or a brain configured, utterances are meant for
        // them, not for the focused window.
        if self.config.commands.is_none() && self.config.brain.is_none() {
            // One listening session is one entry in the history.
            self.history.extend(text);
            self.tray.set_history(self.history.entries());
            self.insert(InsertTarget::capture(), text.to_string() + " ");
        }
        log::line(&format!("utterance: {} chars", text.chars().count()));
    }
}

fn brain(config: &Config) -> Result<Option<Box<dyn Brain>>, String> {
    let Some(b) = &config.brain else {
        return Ok(None);
    };
    let ca_cert_pem = match &b.ca_cert {
        Some(path) => Some(std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?),
        None => None,
    };
    // "openai" is the only kind the settings accept; another assistant is
    // reached by implementing Brain against the library.
    Ok(Some(Box::new(OpenAiChat::new(OpenAiConfig {
        url: b.url.clone(),
        model: b.model.clone().unwrap_or_default(),
        api_key: b.token.clone(),
        system_prompt: b.system_prompt.clone(),
        max_history: 20,
        ca_cert_pem,
    })?)))
}

/// One transcription right after loading, so the first dictation does not
/// pay for the GPU's start: a Vulkan driver without its shader cache (the
/// first start, or after a driver update) builds its pipelines then, about
/// 3 s on a laptop GPU. A quiet tone rather than silence, which whisper
/// skips without running the model.
fn warm_up(engine: &Engine) {
    let started = Instant::now();
    let tone: Vec<f32> = (0..2 * 16_000)
        .map(|i| (i as f32 * 0.07).sin() * 0.02)
        .collect();
    let options = TranscribeOptions {
        keep_pauses: true,
        ..TranscribeOptions::accurate()
    };
    match engine.transcribe(&tone, &options) {
        Ok(_) => log::line(&format!(
            "model warmed up in {} ms",
            started.elapsed().as_millis()
        )),
        Err(e) => log::line(&format!("warm-up failed: {e}")),
    }
}
