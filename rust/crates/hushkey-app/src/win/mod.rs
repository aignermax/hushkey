//! hushkey on Windows: tray, push-to-talk key, insertion, listening.
//!
//! Threads: the main thread runs the Win32 message loop (tray, menu) and
//! the app state; the hook thread reports the key; an inserter thread puts
//! text into windows one dictation at a time; sessions run their own
//! workers. Everything reports to the main thread through one channel.

mod clipboard;
mod hotkey;
mod insert;
mod tray;

use std::process::ExitCode;
use std::ptr;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use hushkey_brain::{Claudio, ClaudioConfig, OpenAiChat, OpenAiConfig};
use hushkey_core::{
    model, Brain, CommandSet, Engine, EngineConfig, Event, EventSink, ListenConfig, ListenSession,
    Microphone, RecordConfig, RecordSession,
};
use tray_icon::menu::MenuEvent;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, MessageBoxW, MsgWaitForMultipleObjects, PeekMessageW, TranslateMessage,
    MB_ICONERROR, MB_OK, MSG, PM_REMOVE, QS_ALLINPUT, WM_QUIT,
};

use crate::config::{self, Config, Insert, Mode};
use crate::log;
use hotkey::{Hotkey, KeyEvent};
use insert::Restore;
use tray::{Look, Tray};

/// A press shorter than this is a tap: no dictation, and Caps Lock keeps
/// its normal job of toggling.
const MIN_HOLD: Duration = Duration::from_millis(400);
/// While a key is held, Windows repeats its key-down every ~30 ms after the
/// initial delay (at most 1 s). Silence longer than this means the release
/// happened where the hook cannot see it (UAC, Ctrl+Alt+Del, a locked
/// screen).
const RELEASE_UNSEEN: Duration = Duration::from_millis(2500);
const VK_CAPITAL: u32 = 0x14;

enum Msg {
    Key(KeyEvent),
    Loaded(Result<Arc<Engine>, String>),
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

pub fn run() -> ExitCode {
    log::open();
    let config = match Config::load() {
        Ok(c) => c,
        Err(e) => return fatal(&e),
    };
    let (tx, rx) = mpsc::channel::<Msg>();
    let vk = config::virtual_key(&config.key).expect("validated on load");
    let key_tx = tx.clone();
    let _hotkey = match Hotkey::install(vk, move |event| {
        let _ = key_tx.send(Msg::Key(event));
    }) {
        Ok(h) => h,
        Err(e) => return fatal(&e),
    };
    let tray = match Tray::new(config.mode == Mode::Listen) {
        Ok(t) => t,
        Err(e) => return fatal(&e),
    };
    let (inserter, inserter_thread) =
        spawn_inserter(config.insert, config.restore_clipboard, tx.clone());
    load_engine(&config, tx.clone());
    log::line(&format!("started, key {}", config.key));
    App {
        vk,
        config,
        tx,
        tray,
        inserter: Some(inserter),
        inserter_thread: Some(inserter_thread),
        engine: None,
        recording: None,
        pressed_at: None,
        listening: None,
        listen_generation: 0,
        busy: false,
    }
    .run(rx)
}

fn load_engine(config: &Config, tx: Sender<Msg>) {
    let config = config.clone();
    std::thread::spawn(move || {
        let result = (|| {
            let model = model::ensure_whisper_model(&config.model).map_err(|e| e.to_string())?;
            let mut engine = EngineConfig::new(model);
            engine.use_gpu = config.use_gpu;
            engine.language = config.language.clone();
            // Listening needs it, and it keeps whisper from inventing text
            // in a dictation's pauses.
            engine.vad_model = Some(model::ensure_vad_model().map_err(|e| e.to_string())?);
            Engine::new(engine).map(Arc::new).map_err(|e| e.to_string())
        })();
        let _ = tx.send(Msg::Loaded(result));
    });
}

/// One thread inserts, so dictations never interleave.
fn spawn_inserter(how: Insert, restore: bool, tx: Sender<Msg>) -> (Sender<String>, JoinHandle<()>) {
    let (text_tx, text_rx) = mpsc::channel::<String>();
    let thread = std::thread::spawn(move || {
        for text in text_rx {
            let result = match how {
                Insert::Paste => insert::paste(&text, restore).or_else(|e| {
                    log::line(&format!("paste failed ({e}); typing instead"));
                    insert::type_text(&text).map(|()| Restore::Nothing)
                }),
                Insert::Type => insert::type_text(&text).map(|()| Restore::Nothing),
            };
            let _ = tx.send(Msg::Inserted(result));
        }
    });
    (text_tx, thread)
}

struct App {
    vk: u32,
    config: Config,
    tx: Sender<Msg>,
    tray: Tray,
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
                if event.id == self.tray.quit {
                    return self.quit();
                } else if event.id == self.tray.settings {
                    open_settings();
                } else if event.id == *self.tray.listen.id() {
                    self.set_listening(self.tray.listen.is_checked());
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
            // Sleep until a window message arrives, or poll the channels
            // again soon.
            unsafe { MsgWaitForMultipleObjects(0, ptr::null(), 0, 50, QS_ALLINPUT) };
        }
    }

    fn quit(mut self) -> ExitCode {
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

    fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::Loaded(Ok(engine)) => {
                log::line("model loaded");
                self.engine = Some(engine);
                if self.config.mode == Mode::Listen {
                    self.set_listening(true);
                } else {
                    self.idle();
                }
            }
            Msg::Loaded(Err(e)) => {
                log::line(&format!("model failed: {e}"));
                self.tray
                    .show(Look::Failed, &format!("The model did not load: {e}"));
            }
            Msg::Key(KeyEvent::Down) => self.key_down(),
            Msg::Key(KeyEvent::Up) => self.key_up(),
            Msg::Dictated(Ok(text)) => {
                self.busy = false;
                if text.is_empty() {
                    log::line("nothing heard");
                    self.idle();
                } else {
                    log::line(&format!("dictation: {} chars", text.chars().count()));
                    self.insert(text + " ");
                }
            }
            Msg::Dictated(Err(e)) => {
                self.busy = false;
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

    fn idle(&mut self) {
        if self.listening.is_some() {
            self.tray.show(Look::Listening, "Listening");
        } else if self.engine.is_some() {
            let text = format!(
                "Ready: hold {} to dictate",
                self.config.key.replace('_', " ")
            );
            self.tray.show(Look::Idle, &text);
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
            // No preview window yet; previews would only cost GPU time.
            preview: false,
            ..RecordConfig::default()
        };
        let errors: EventSink = Arc::new(|event| {
            if let Event::Error { message } = event {
                log::line(&format!("recording: {message}"));
            }
        });
        match RecordSession::start(engine, Box::new(Microphone::new(None)), config, errors) {
            Ok(session) => {
                self.recording = Some(session);
                self.tray.show(Look::Recording, "Recording…");
            }
            Err(e) => {
                log::line(&format!("could not record: {e}"));
                self.tray
                    .show(Look::Failed, &format!("Could not record: {e}"));
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
            self.idle();
            return;
        }
        self.busy = true;
        self.tray.show(Look::Transcribing, "Transcribing…");
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = session
                .stop()
                .map(|t| t.map(|t| t.text).unwrap_or_default())
                .map_err(|e| e.to_string());
            let _ = tx.send(Msg::Dictated(result));
        });
    }

    /// Remember the mode in the settings file without overwriting what the
    /// user changed there since start.
    fn remember_mode(&mut self, mode: Mode) {
        if self.config.mode == mode {
            return;
        }
        self.config.mode = mode;
        if let Err(e) = Config::save_mode(mode) {
            log::line(&format!("mode not saved: {e}"));
        }
    }

    fn set_listening(&mut self, on: bool) {
        self.tray.listen.set_checked(on);
        if !on {
            if let Some(mut session) = self.listening.take() {
                // Stopping finishes an utterance in progress: off the UI
                // thread, and its events are ignored (another generation).
                std::thread::spawn(move || {
                    let _ = session.stop();
                });
            }
            self.remember_mode(Mode::PushToTalk);
            self.idle();
            return;
        }
        self.remember_mode(Mode::Listen);
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
                self.tray.listen.set_checked(false);
                self.tray
                    .show(Look::Failed, &format!("Could not listen: {e}"));
            }
        }
    }

    fn start_listening(&mut self, engine: Arc<Engine>) -> Result<ListenSession, String> {
        let mut listen = ListenConfig::default().with_end_silence(self.config.end_silence_ms);
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
            Event::SpeechStart { .. } => self.tray.show(Look::Recording, "Hearing you…"),
            Event::Utterance { transcript, .. } => {
                // With commands or a brain configured, utterances are meant
                // for them, not for the focused window.
                if self.config.commands.is_none() && self.config.brain.is_none() {
                    self.insert(transcript.text.clone() + " ");
                }
                log::line(&format!(
                    "utterance: {} chars",
                    transcript.text.chars().count()
                ));
                self.idle();
            }
            Event::Command { command, .. } => {
                log::line(&format!(
                    "command {}({})",
                    command.name,
                    command.args.join(", ")
                ));
                self.tray.show(
                    Look::Listening,
                    &format!("Command: {} {}", command.name, command.args.join(" ")),
                );
            }
            Event::SpeechEnd { .. } => self.idle(),
            Event::Reply { text, .. } => self.tray.show(Look::Listening, &text),
            Event::Error { message } => log::line(&format!("listening: {message}")),
            Event::Stopped => {
                if self.listening.take().is_some() {
                    // Ended by itself (the microphone went away).
                    self.tray.listen.set_checked(false);
                    self.idle();
                }
            }
            Event::Preview { .. } | Event::Final { .. } => {}
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

fn open_settings() {
    let path = config::state_dir().join("config.json");
    // notepad is always there; the user's .json association may not be.
    let _ = std::process::Command::new("notepad.exe").arg(&path).spawn();
}
