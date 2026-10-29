//! hushkey on Windows: tray, push-to-talk key, insertion, listening.
//!
//! Threads: the main thread runs the Win32 message loop (tray, menu) and
//! the app state; the hook thread reports the key; an inserter thread puts
//! text into windows one dictation at a time; the overlay has its own
//! window thread; sessions run their own workers. Everything reports to the
//! main thread through one channel.

mod autostart;
mod clipboard;
mod hotkey;
mod insert;
mod overlay;
mod tray;

use std::process::ExitCode;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use hushkey_brain::{Claudio, ClaudioConfig, OpenAiChat, OpenAiConfig};
use hushkey_core::{
    model, Brain, CommandSet, Engine, EngineConfig, Event, EventSink, ListenConfig, ListenSession,
    Microphone, RecordConfig, RecordSession, TranscribeOptions,
};
use tray_icon::menu::MenuEvent;
use windows_sys::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, MessageBoxW, MsgWaitForMultipleObjects, PeekMessageW, TranslateMessage,
    MB_ICONERROR, MB_OK, MSG, PM_REMOVE, QS_ALLINPUT, WM_QUIT,
};

use crate::config::{self, Config, Insert, Mode};
use crate::history::History;
use crate::icon::Badge;
use crate::log;
use crate::strings::{self, Strings, MODELS};
use hotkey::{Hotkey, KeyEvent};
use insert::Restore;
use overlay::Overlay;
use tray::{Action, Tray};

/// A press shorter than this is a tap: no dictation, and Caps Lock keeps
/// its normal job of toggling.
const MIN_HOLD: Duration = Duration::from_millis(400);
/// While a key is held, Windows repeats its key-down every ~30 ms after the
/// initial delay (at most 1 s). Silence longer than this means the release
/// happened where the hook cannot see it (UAC, Ctrl+Alt+Del, a locked
/// screen).
const RELEASE_UNSEEN: Duration = Duration::from_millis(2500);
const VK_CAPITAL: u32 = 0x14;
/// How long a notice ("copied …") stays in the overlay.
const NOTICE: Duration = Duration::from_secs(2);

/// Overlay dot colours, as the Python app's.
const RED: [u8; 3] = [0xe5, 0x3e, 0x3e];
const ORANGE: [u8; 3] = [0xdd, 0x6b, 0x20];
const GREY: [u8; 3] = [0x71, 0x80, 0x96];

enum Msg {
    Key(KeyEvent),
    /// The engine for model `.0` loaded (or not).
    Loaded(String, Result<Arc<Engine>, String>),
    /// What the recording has heard so far.
    Preview(String),
    /// A push-to-talk dictation finished: its text (or why not).
    Dictated(Result<String, String>),
    /// Listening session number `.0` reported something.
    Listen(u64, Event),
    /// The inserter finished one text.
    Inserted(Result<Restore, String>),
}

fn fatal(message: &str) -> ExitCode {
    log::line(&format!("fatal: {message}"));
    let text: Vec<u16> = format!("hushkey could not start:\n\n{message}")
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let title: Vec<u16> = "hushkey".encode_utf16().chain(Some(0)).collect();
    unsafe {
        MessageBoxW(
            ptr::null_mut(),
            text.as_ptr(),
            title.as_ptr(),
            MB_OK | MB_ICONERROR,
        )
    };
    ExitCode::FAILURE
}

/// True if another hushkey (Rust) already runs in this session. The mutex
/// handle is kept for the process's life.
fn already_running() -> bool {
    let name: Vec<u16> = "Local\\hushkey-rs".encode_utf16().chain(Some(0)).collect();
    unsafe {
        let handle = CreateMutexW(ptr::null(), 0, name.as_ptr());
        !handle.is_null() && GetLastError() == ERROR_ALREADY_EXISTS
    }
}

pub fn run() -> ExitCode {
    // Physical pixels everywhere: the overlay sizes itself per monitor.
    unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    log::open();
    if already_running() {
        log::line("already running; this start does nothing");
        return ExitCode::SUCCESS;
    }
    let s = strings::current();
    let config = match Config::load() {
        Ok(c) => c,
        Err(e) => return fatal(&e),
    };
    let (tx, rx) = mpsc::channel::<Msg>();
    let vk = config::virtual_key(&config.key).expect("validated on load");
    let hotkey = match install_hotkey(vk, tx.clone()) {
        Ok(h) => h,
        Err(e) => return fatal(&e),
    };
    let mut tray = match Tray::new(s, &config, autostart::enabled()) {
        Ok(t) => t,
        Err(e) => return fatal(&e),
    };
    let overlay = match Overlay::new() {
        Ok(o) => o,
        Err(e) => return fatal(&e),
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
        vk,
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
        busy: false,
        notice_until: None,
    };
    app.load_engine();
    app.run(rx)
}

fn install_hotkey(vk: u32, tx: Sender<Msg>) -> Result<Hotkey, String> {
    Hotkey::install(vk, move |event| {
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
            let result = if paste.load(Ordering::Relaxed) {
                insert::paste(&text, restore).or_else(|e| {
                    log::line(&format!("paste failed ({e}); typing instead"));
                    insert::type_text(&text).map(|()| Restore::Nothing)
                })
            } else {
                insert::type_text(&text).map(|()| Restore::Nothing)
            };
            let _ = tx.send(Msg::Inserted(result));
        }
    });
    (text_tx, thread)
}

struct App {
    s: &'static Strings,
    vk: u32,
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
    /// A dictation is being transcribed.
    busy: bool,
    /// A notice is in the overlay until then.
    notice_until: Option<Instant>,
}

impl App {
    fn run(mut self, rx: Receiver<Msg>) -> ExitCode {
        loop {
            // Win32 messages first: the tray and its menu live on them.
            unsafe {
                let mut msg: MSG = std::mem::zeroed();
                while PeekMessageW(&mut msg, ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                    if msg.message == WM_QUIT {
                        return self.quit();
                    }
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
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
            if hotkey::since_last_seen().is_some_and(|quiet| quiet > RELEASE_UNSEEN) {
                hotkey::forget_press();
                if self.recording.is_some() {
                    log::line("the key's release was not seen; ending the dictation");
                    self.key_up();
                }
            }
            if self
                .notice_until
                .is_some_and(|until| Instant::now() > until)
            {
                self.notice_until = None;
                self.overlay.hide();
            }
            // Sleep until a window message arrives, or poll the channels
            // again soon.
            unsafe { MsgWaitForMultipleObjects(0, ptr::null(), 0, 50, QS_ALLINPUT) };
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
        let size = MODELS
            .iter()
            .find(|(name, _)| *name == self.config.model)
            .map_or("?", |(_, size)| size);
        let text = strings::fill(
            self.s.model_loading,
            &[("model", &self.config.model), ("size", size)],
        );
        self.tray.show(Badge::Loading, &text);
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
                Engine::new(engine).map(Arc::new).map_err(|e| e.to_string())
            })();
            let _ = tx.send(Msg::Loaded(config.model, result));
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
            Msg::Loaded(name, result) if name != self.config.model => {
                // A model switched away from while it loaded.
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
                self.tray
                    .show(Badge::Failed, &format!("{}: {e}", self.s.failed));
            }
            Msg::Key(KeyEvent::Down) => self.key_down(),
            Msg::Key(KeyEvent::Up) => self.key_up(),
            Msg::Preview(text) => {
                if self.recording.is_some() {
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
                match result {
                    Ok(restore) => log::line(&format!("inserted, clipboard: {restore:?}")),
                    Err(e) => log::line(&format!("insert failed: {e}")),
                }
                if !self.busy && self.recording.is_none() {
                    self.idle();
                }
            }
            Msg::Listen(generation, event) => {
                if generation == self.listen_generation && self.listening.is_some() {
                    self.listen_event(event);
                }
            }
        }
    }

    fn notice(&mut self, colour: [u8; 3], text: &str) {
        self.overlay.show(colour, text);
        self.notice_until = Some(Instant::now() + NOTICE);
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
                self.overlay.show(RED, self.s.recording);
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
        if tap && self.vk == VK_CAPITAL {
            // The hook swallowed the press; a tap still toggles Caps Lock.
            insert::tap_key(VK_CAPITAL as u16);
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
                    self.stop_listening();
                    self.config.model = name.to_string();
                    self.save(|c| c.model = name.to_string());
                    self.load_engine();
                }
            }
            Action::Key(name) => {
                let vk = config::virtual_key(name).expect("menu keys are known");
                // One hook per process: the old one goes first.
                self.hotkey = None;
                match install_hotkey(vk, self.tx.clone()) {
                    Ok(h) => {
                        self.hotkey = Some(h);
                        self.vk = vk;
                        self.config.key = name.to_string();
                        self.save(|c| c.key = name.to_string());
                    }
                    Err(e) => {
                        log::line(&format!("key not changed: {e}"));
                        self.hotkey = install_hotkey(self.vk, self.tx.clone()).ok();
                    }
                }
            }
            Action::Language(code) => {
                self.config.language = code.map(str::to_string);
                self.save(|c| c.language = code.map(str::to_string));
                if self.listening.is_some() {
                    // A session keeps its language: restart it.
                    self.stop_listening();
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
                    match clipboard::write_text(&entry.text, false) {
                        Ok(()) => self.notice(GREY, self.s.copied),
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
            Action::OpenLogs => {
                let _ = std::process::Command::new("explorer.exe")
                    .arg(config::state_dir())
                    .spawn();
            }
            Action::Quit => {}
        }
        self.tray.sync(&self.config, autostart::enabled());
    }

    /// Change the settings file as it is now (the user may have edited it),
    /// not as it was at start.
    fn save(&self, change: impl FnOnce(&mut Config)) {
        if let Err(e) = Config::update(change) {
            log::line(&format!("settings not saved: {e}"));
        }
    }

    /// End the listening session, if any, keeping the mode (a model or
    /// language switch restarts it).
    fn stop_listening(&mut self) {
        if let Some(mut session) = self.listening.take() {
            // Stopping finishes an utterance in progress: off the UI
            // thread, and its events are ignored (another generation).
            std::thread::spawn(move || {
                let _ = session.stop();
            });
            self.history.close();
            self.overlay.hide();
        }
    }

    fn set_listening(&mut self, on: bool) {
        if !on {
            self.stop_listening();
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
        match self.start_listening(engine) {
            Ok(session) => {
                self.listening = Some(session);
                log::line("listening");
                self.idle();
            }
            Err(e) => {
                log::line(&format!("could not listen: {e}"));
                self.config.mode = Mode::PushToTalk;
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
                self.overlay.show(RED, self.s.hearing);
                self.tray.show(Badge::Recording, self.s.hearing);
            }
            Event::Preview { text } => self.overlay.show(RED, &text),
            Event::Utterance { transcript, .. } => {
                // With commands or a brain configured, utterances are meant
                // for them, not for the focused window.
                if self.config.commands.is_none() && self.config.brain.is_none() {
                    // One listening session is one entry in the history.
                    self.history.extend(&transcript.text);
                    self.tray.set_history(self.history.entries());
                    self.insert(transcript.text.clone() + " ");
                }
                log::line(&format!(
                    "utterance: {} chars",
                    transcript.text.chars().count()
                ));
            }
            Event::SpeechEnd { .. } => {
                self.overlay.hide();
                self.idle();
            }
            Event::Command { command, .. } => {
                log::line(&format!(
                    "command {}({})",
                    command.name,
                    command.args.join(", ")
                ));
                let text = format!("{} {}", command.name, command.args.join(" "));
                self.notice(GREY, &text);
            }
            Event::Reply { text, .. } => self.notice(GREY, &text),
            Event::Error { message } => log::line(&format!("listening: {message}")),
            Event::Stopped => {
                if self.listening.take().is_some() {
                    // Ended by itself (the microphone went away).
                    self.history.close();
                    self.config.mode = Mode::PushToTalk;
                    self.tray.sync(&self.config, autostart::enabled());
                    self.idle();
                }
            }
            Event::Final { .. } => {}
        }
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
    Ok(Some(match b.kind.as_str() {
        "claudio" => Box::new(Claudio::new(ClaudioConfig {
            url: b.url.clone(),
            profile_id: b.model.clone(),
            token: b.token.clone(),
            ca_cert_pem,
        })?),
        _ => Box::new(OpenAiChat::new(OpenAiConfig {
            url: b.url.clone(),
            model: b.model.clone().unwrap_or_default(),
            api_key: b.token.clone(),
            system_prompt: b.system_prompt.clone(),
            max_history: 20,
            ca_cert_pem,
        })?),
    }))
}
