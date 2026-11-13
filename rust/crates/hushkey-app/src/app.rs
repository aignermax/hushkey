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
    model, Brain, CommandSet, Engine, EngineConfig, Event, EventSink, ListenConfig, ListenSession,
    Microphone, RecordConfig, RecordSession, TranscribeOptions,
};
use tray_icon::menu::MenuEvent;

use crate::config::{self, Config, Insert, Mode, Updates};
use crate::history::History;
use crate::icon::Badge;
use crate::log;
use crate::platform::{self, autostart, Hotkey, KeyEvent, Overlay, Restore};
use crate::strings::{self, Strings, MODELS};
use crate::tray::{Action, Tray};
use crate::update::{self, Release};

/// A press shorter than this is a tap: no dictation, and Caps Lock keeps
/// its normal job of toggling.
const MIN_HOLD: Duration = Duration::from_millis(400);
/// While a key is held, the system repeats its key-down every ~30 ms after
/// the initial delay (at most 1 s). Silence longer than this means the
/// release happened where hushkey cannot see it (UAC, Ctrl+Alt+Del, a
/// locked screen). Generous: on a busy machine repeats can come late, and
/// a dictation cut off mid-sentence is worse than one that ends late.
const RELEASE_UNSEEN: Duration = Duration::from_secs(6);
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
    Key(KeyEvent),
    /// Engine load number `.0` finished (or failed).
    Loaded(u64, Result<Arc<Engine>, String>),
    /// What the recording has heard so far.
    Preview(String),
    /// A push-to-talk dictation finished: its text (or why not).
    Dictated(Result<String, String>),
    /// Listening session number `.0` reported something.
    Listen(u64, Event),
    /// The inserter finished one text.
    Inserted(Result<Restore, String>),
    /// An update check found this (`.1`: asked for from the menu).
    UpdateChecked(Result<Option<Release>, String>, bool),
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
    let (inserter, inserter_thread) =
        spawn_inserter(paste.clone(), config.restore_clipboard, tx.clone());
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
        inserter: Some(inserter),
        inserter_thread: Some(inserter_thread),
        engine: None,
        recording: None,
        pressed_at: None,
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
        update: None,
        updating: false,
        removing_old: false,
        quit_requested: false,
        next_update_check: Instant::now() + Duration::from_secs(30),
    };
    app.load_engine();
    app.run(rx)
}

fn install_hotkey(key: &str, tx: Sender<Msg>) -> Result<Hotkey, String> {
    Hotkey::install(key, move |event| {
        let _ = tx.send(Msg::Key(event));
    })
}

/// One thread inserts, so dictations never interleave.
fn spawn_inserter(
    paste: Arc<AtomicBool>,
    restore: bool,
    tx: Sender<Msg>,
) -> (Sender<String>, JoinHandle<()>) {
    let (text_tx, text_rx) = mpsc::channel::<String>();
    let thread = std::thread::spawn(move || {
        for text in text_rx {
            let result = platform::insert(&text, paste.load(Ordering::Relaxed), restore);
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
    inserter: Option<Sender<String>>,
    inserter_thread: Option<JoinHandle<()>>,
    engine: Option<Arc<Engine>>,
    recording: Option<RecordSession>,
    pressed_at: Option<Instant>,
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
    /// A newer release, offered in the tray.
    update: Option<Release>,
    /// An update is being downloaded and put in place.
    updating: bool,
    /// The Python hushkey's leftovers are being removed.
    removing_old: bool,
    /// The updated program is starting; this one goes.
    quit_requested: bool,
    next_update_check: Instant,
}

impl App {
    fn run(mut self, rx: Receiver<Msg>) -> ExitCode {
        loop {
            // The system's events first: the tray and its menu live on them.
            if !platform::pump() {
                return self.quit();
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
            if self.quit_requested {
                return self.quit();
            }
            if let Some(quiet) = platform::since_last_seen().filter(|q| *q > RELEASE_UNSEEN) {
                platform::forget_press();
                if self.recording.is_some() {
                    log::line(&format!(
                        "the key's release was not seen ({} ms without its repeats); ending the dictation",
                        quiet.as_millis()
                    ));
                    self.key_up();
                }
            }
            if Instant::now() > self.next_update_check {
                self.next_update_check = Instant::now() + UPDATE_CHECK;
                if self.config.updates != Updates::Off {
                    self.check_update(false);
                }
            }
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
        self.overlay.hide();
        if let Some(mut session) = self.recording.take() {
            let _ = session.stop();
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

    fn insert(&self, text: String) {
        if let Some(inserter) = &self.inserter {
            let _ = inserter.send(text);
        }
    }

    fn load_engine(&mut self) {
        self.engine = None;
        self.load_generation += 1;
        let generation = self.load_generation;
        let size = MODELS
            .iter()
            .find(|(name, _)| *name == self.config.model)
            .map_or("?", |(_, size)| size);
        let text = strings::fill(
            self.s.model_loading,
            &[("model", &self.config.model), ("size", size)],
        );
        self.tray.show(Badge::Loading, &text);
        if self.config.guide && self.recording.is_none() && !self.busy {
            // A new install downloads its model first, which takes a
            // while: say so where it is seen, not only in the tray.
            self.overlay.show(GREY, &text);
        }
        let (config, tx) = (self.config.clone(), self.tx.clone());
        std::thread::spawn(move || {
            let result = (|| {
                let model =
                    model::ensure_whisper_model(&config.model).map_err(|e| e.to_string())?;
                let mut engine = EngineConfig::new(model);
                engine.use_gpu = config.use_gpu;
                // Listening needs it, and it keeps whisper from inventing
                // text in a dictation's pauses.
                engine.vad_model = Some(model::ensure_vad_model().map_err(|e| e.to_string())?);
                let engine = Engine::new(engine)
                    .map(Arc::new)
                    .map_err(|e| e.to_string())?;
                // Only a GPU has pipelines to build; on the CPU this would
                // just be seconds of work.
                if engine.on_gpu() {
                    warm_up(&engine);
                } else if config.use_gpu
                    && cfg!(any(feature = "vulkan", feature = "metal", feature = "cuda"))
                {
                    log::line("quantized model: it runs on the CPU in this build");
                }
                Ok(engine)
            })();
            let _ = tx.send(Msg::Loaded(generation, result));
        });
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
            Msg::Loaded(generation, result) if generation != self.load_generation => {
                // Superseded by a later switch while it loaded.
                drop(result);
            }
            Msg::Loaded(_, Ok(engine)) => {
                log::line("model loaded");
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
            Msg::Key(KeyEvent::Down) => self.key_down(),
            Msg::Key(KeyEvent::Up) => self.key_up(),
            Msg::Preview(text) => {
                if self.recording.is_some() {
                    // The first steps keep saying to let go.
                    let text = if self.config.guide {
                        format!("{} — {text}", self.s.guide_listening)
                    } else {
                        text
                    };
                    self.overlay.show(RED, &text);
                }
            }
            Msg::Dictated(Ok(text)) => {
                self.busy = false;
                let text = text.trim().to_string();
                if text.is_empty() {
                    log::line("nothing heard");
                    self.notice(GREY, self.s.nothing_heard);
                    self.idle();
                } else {
                    log::line(&format!("dictation: {} chars", text.chars().count()));
                    self.overlay.hide();
                    // Kept before inserting: a misdirected paste is recoverable.
                    self.history.add(&text);
                    self.tray.set_history(self.history.entries());
                    self.insert(text + " ");
                }
            }
            Msg::Dictated(Err(e)) => {
                self.busy = false;
                self.overlay.hide();
                log::line(&format!("dictation failed: {e}"));
                self.idle();
            }
            Msg::Inserted(result) => {
                let arrived = result.as_ref().is_ok_and(platform::Restore::arrived);
                match result {
                    Ok(restore) => log::line(&format!("inserted, clipboard: {restore:?}")),
                    Err(e) => log::line(&format!("insert failed: {e}")),
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
            Msg::UpdateChecked(result, asked) => match result {
                Ok(release) => {
                    if let Some(r) = &release {
                        log::line(&format!("update available: v{}", r.version));
                    } else if asked {
                        self.notice(GREY, self.s.up_to_date);
                    }
                    let version = release.as_ref().map(|r| r.version.to_string());
                    self.tray.offer_update(version.as_deref());
                    self.update = release;
                }
                Err(e) => {
                    log::line(&format!("update check: {e}"));
                    if asked {
                        self.notice(GREY, self.s.update_failed);
                    }
                }
            },
            Msg::UpdateInstalled(Ok(None)) => {
                log::line("update cancelled at the password dialog");
                self.updating = false;
                // Replaces "installing the update …", which has no end.
                self.notice(GREY, self.s.update_cancelled);
                self.idle();
            }
            Msg::UpdateInstalled(Ok(Some(exe))) => {
                log::line(&format!("update in place, restarting: {}", exe.display()));
                match platform::setup::launch(&exe, &["--after-update"]) {
                    Ok(()) => self.quit_requested = true,
                    Err(e) => {
                        log::line(&format!("update: could not start it: {e}"));
                        self.updating = false;
                        self.notice(GREY, self.s.update_failed);
                        self.idle();
                    }
                }
            }
            Msg::UpdateInstalled(Err(e)) => {
                log::line(&format!("update failed: {e}"));
                self.updating = false;
                let text = if e.starts_with(crate::setup::PACKAGE_BUSY) {
                    self.s.package_busy
                } else {
                    self.s.update_failed
                };
                self.failure_notice(text);
                self.idle();
            }
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
        self.notice_for(GREY, text, FAILURE_NOTICE);
    }

    fn notice_for(&mut self, colour: [u8; 3], text: &str, how_long: Duration) {
        self.overlay.show(colour, text);
        self.notice_until = Some(Instant::now() + how_long);
    }

    fn idle(&mut self) {
        if self.engine.is_none() {
            return; // still loading: the tray says so
        }
        let state = if self.listening.is_some() {
            self.s.listening
        } else {
            self.s.idle
        };
        let text = format!(
            "hushkey {} · {} — {state}",
            env!("CARGO_PKG_VERSION"),
            self.config.model
        );
        let badge = if self.listening.is_some() {
            Badge::Listening
        } else {
            Badge::None
        };
        self.tray.show(badge, &text);
        self.guide();
    }

    /// Ready for the key, with nothing else going on.
    fn waiting(&self) -> bool {
        self.engine.is_some()
            && self.config.mode == Mode::PushToTalk
            && self.listening.is_none()
            && self.recording.is_none()
            && !self.busy
            && !self.updating
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
        self.pressed_at = Some(Instant::now());
        if self.listening.is_some() || self.recording.is_some() || self.busy {
            return;
        }
        let Some(engine) = self.engine.clone() else {
            return;
        };
        let config = RecordConfig {
            preview_interval: Duration::from_millis(1500),
            options: self.options(),
            ..RecordConfig::default()
        };
        let tx = self.tx.clone();
        let sink: EventSink = Arc::new(move |event| match event {
            Event::Preview { text } => {
                let _ = tx.send(Msg::Preview(text));
            }
            Event::Error { message } => log::line(&format!("recording: {message}")),
            _ => {}
        });
        match RecordSession::start(engine, Box::new(Microphone::new(None)), config, sink) {
            Ok(session) => {
                self.recording = Some(session);
                self.notice_until = None;
                let text = if self.config.guide {
                    self.s.guide_listening
                } else {
                    self.s.recording
                };
                self.overlay.show(RED, text);
                self.tray.show(Badge::Recording, self.s.recording);
            }
            Err(e) => {
                log::line(&format!("could not record: {e}"));
                self.tray
                    .show(Badge::Failed, &format!("{}: {e}", self.s.failed));
            }
        }
    }

    fn key_up(&mut self) {
        let tap = self
            .pressed_at
            .take()
            .is_some_and(|t| t.elapsed() < MIN_HOLD);
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
                let _ = session.stop();
            });
            self.overlay.hide();
            self.idle();
            return;
        }
        self.busy = true;
        self.overlay.show(ORANGE, self.s.transcribing);
        self.tray.show(Badge::Transcribing, self.s.transcribing);
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = session
                .stop()
                .map(|t| t.map(|t| t.text).unwrap_or_default())
                .map_err(|e| e.to_string());
            let _ = tx.send(Msg::Dictated(result));
        });
    }

    fn menu(&mut self, action: Action) {
        match action {
            Action::Model(name) => {
                if self.config.model != name {
                    // Listening, if on, resumes once the new model loaded.
                    self.stop_listening(true);
                    self.config.model = name.to_string();
                    self.save(|c| c.model = name.to_string());
                    self.load_engine();
                }
            }
            Action::Key(name) => {
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
            Action::Copy(i) => {
                if let Some(entry) = self.history.entries().get(i) {
                    match platform::copy(&entry.text) {
                        Ok(()) => self.notice(GREY, &self.s.copied()),
                        Err(e) => log::line(&format!("copy failed: {e}")),
                    }
                }
            }
            Action::Listen => self.set_listening(self.config.mode != Mode::Listen),
            Action::Autostart => {
                let on = !autostart::enabled();
                if let Err(e) = autostart::set(on) {
                    log::line(&format!("autostart: {e}"));
                }
            }
            Action::OpenLogs => platform::open_folder(&config::state_dir()),
            Action::Update => match self.update.clone() {
                Some(release) if !self.updating => self.install_update(release),
                Some(_) => {}
                None => self.check_update(true),
            },
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

    fn check_update(&self, asked: bool) {
        // A program some other package manager put in /usr is that one's
        // to update (hushkey's own .deb updates through apt, see
        // update::install).
        if !platform::setup::updates_itself() {
            let _ = self.tx.send(Msg::UpdateChecked(Ok(None), asked));
            return;
        }
        let (tx, pre) = (self.tx.clone(), update::wants_pre(self.config.updates));
        std::thread::spawn(move || {
            let _ = tx.send(Msg::UpdateChecked(update::check(pre), asked));
        });
    }

    fn install_update(&mut self, release: Release) {
        log::line(&format!("installing the update to v{}", release.version));
        self.updating = true;
        self.notice_until = None;
        self.overlay.show(GREY, self.s.update_running);
        self.tray.show(Badge::Loading, self.s.update_running);
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(Msg::UpdateInstalled(update::install(&release)));
        });
    }

    /// Change the settings file as it is now (the user may have edited it),
    /// not as it was at start.
    fn save(&self, change: impl FnOnce(&mut Config)) {
        if let Err(e) = Config::update(change) {
            log::line(&format!("settings not saved: {e}"));
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
            self.insert(text.to_string() + " ");
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
