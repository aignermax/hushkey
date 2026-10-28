//! Continuous listening: every utterance becomes an event, no key needed.
//!
//! The voice activity detector runs over the newest audio every few frames;
//! the [`Endpointer`] turns its probabilities into utterance boundaries, and
//! each finished utterance is transcribed at once. While someone speaks,
//! previews of the utterance so far follow.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{JoinHandle, ThreadId};
use std::time::{Duration, Instant};

use crate::audio::{AudioInput, AudioSink, SAMPLE_RATE};
use crate::endpoint::{frames, Boundary, EndpointConfig, Endpointer};
use crate::engine::{Engine, TranscribeOptions, VAD_FRAME};
use crate::error::{Error, Result};
use crate::event::{Event, EventSink};

/// Frames the detector re-reads before the new ones: it starts fresh on
/// every call and needs context to judge the first new frame.
const CONTEXT_FRAMES: usize = 16; // ~0.5 s
/// New frames gathered before the detector runs again (~160 ms): each run
/// re-reads the context, so a longer hop costs less, a shorter one reacts
/// sooner to the end of an utterance.
const HOP_FRAMES: usize = 5;
/// A preview covers at most this many of the newest frames (~10 s), so it
/// stays quick however long the utterance gets.
const PREVIEW_MAX_FRAMES: usize = 312;
/// Consecutive detector failures that end the session.
const MAX_VAD_FAILURES: u32 = 5;

#[derive(Clone, Debug)]
pub struct ListenConfig {
    pub endpoint: EndpointConfig,
    /// Emit [`Event::Preview`]s while an utterance is being spoken.
    pub preview: bool,
    pub preview_interval: Duration,
    /// Settings for transcribing each utterance.
    pub options: TranscribeOptions,
}

impl Default for ListenConfig {
    fn default() -> Self {
        ListenConfig {
            endpoint: EndpointConfig::default(),
            preview: true,
            preview_interval: Duration::from_secs(1),
            options: TranscribeOptions::accurate(),
        }
    }
}

impl ListenConfig {
    /// Pause that ends an utterance; shorter reacts faster to commands,
    /// longer lets people think mid-sentence.
    pub fn with_end_silence(mut self, ms: u32) -> Self {
        self.endpoint.end_silence_frames = frames(ms);
        self
    }
}

#[derive(Default)]
struct State {
    /// Audio from absolute sample `base` on.
    audio: Vec<f32>,
    base: usize,
    stop: bool,
    input_ended: bool,
    input_errors: Vec<String>,
}

impl State {
    fn end(&self) -> usize {
        self.base + self.audio.len()
    }

    /// Absolute samples `[from, to)`, clamped to what is still buffered.
    fn slice(&self, from: usize, to: usize) -> Vec<f32> {
        let from = from.max(self.base).min(self.end());
        let to = to.max(from).min(self.end());
        self.audio[from - self.base..to - self.base].to_vec()
    }

    fn drop_before(&mut self, sample: usize) {
        if sample > self.base {
            let n = (sample - self.base).min(self.audio.len());
            self.audio.drain(..n);
            self.base += n;
        }
    }
}

struct Shared {
    state: Mutex<State>,
    wake: Condvar,
    /// Set by stop(); aborts a preview in flight so the last utterance is
    /// not delayed by it.
    stopping: AtomicBool,
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }
}

/// Listening in progress. Stop it (or drop it) to end; a finite input (a
/// file) ends it by itself once everything was heard.
pub struct ListenSession {
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
    worker_id: ThreadId,
}

impl ListenSession {
    /// The engine must have a VAD model ([`crate::EngineConfig::vad_model`]).
    pub fn start(
        engine: Arc<Engine>,
        mut input: Box<dyn AudioInput>,
        config: ListenConfig,
        sink: EventSink,
    ) -> Result<ListenSession> {
        if !engine.has_vad() {
            return Err(Error::InvalidArgument(
                "listening needs an engine with a VAD model".into(),
            ));
        }
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            wake: Condvar::new(),
            stopping: AtomicBool::new(false),
        });
        let (on_samples, on_error, on_end) = (shared.clone(), shared.clone(), shared.clone());
        input.start(AudioSink {
            on_samples: Box::new(move |samples| {
                on_samples.lock().audio.extend_from_slice(samples);
                on_samples.wake.notify_all();
            }),
            on_error: Box::new(move |message| {
                on_error.lock().input_errors.push(message);
                on_error.wake.notify_all();
            }),
            on_end: Box::new(move || {
                on_end.lock().input_ended = true;
                on_end.wake.notify_all();
            }),
        })?;
        let worker_shared = shared.clone();
        let worker = std::thread::Builder::new()
            .name("hushkey-listen".into())
            .spawn(move || {
                let on_panic = sink.clone();
                // The host waits for Stopped before freeing its state; a
                // panic must still end with it.
                let worker = Worker::new(engine, config, sink, worker_shared);
                if catch_unwind(AssertUnwindSafe(|| worker.run(input))).is_err() {
                    on_panic(Event::Error {
                        message: "internal error: the listening thread panicked".into(),
                    });
                    on_panic(Event::Stopped);
                }
            })
            .map_err(Error::Io)?;
        let worker_id = worker.thread().id();
        Ok(ListenSession {
            shared,
            worker: Some(worker),
            worker_id,
        })
    }

    /// Stop listening. An utterance in progress is transcribed first, then
    /// [`Event::Stopped`] follows. From inside the session's own callback
    /// this returns at once and the stop completes after the callback.
    pub fn stop(&mut self) -> Result<()> {
        self.shared.stopping.store(true, Ordering::Relaxed);
        self.shared.lock().stop = true;
        self.shared.wake.notify_all();
        if std::thread::current().id() == self.worker_id {
            return Ok(());
        }
        match self.worker.take() {
            None => Err(Error::Stopped),
            Some(worker) => worker
                .join()
                .map_err(|_| Error::Transcribe("the listening thread panicked".into())),
        }
    }

    /// Block until the session ends by itself (a finite input ran out) or
    /// is stopped from elsewhere.
    pub fn wait(&mut self) -> Result<()> {
        match self.worker.take() {
            None => Err(Error::Stopped),
            Some(worker) => worker
                .join()
                .map_err(|_| Error::Transcribe("the listening thread panicked".into())),
        }
    }
}

impl Drop for ListenSession {
    fn drop(&mut self) {
        if self.worker.is_some() {
            let _ = self.stop();
        }
    }
}

struct Worker {
    engine: Arc<Engine>,
    config: ListenConfig,
    sink: EventSink,
    shared: Arc<Shared>,
    endpointer: Endpointer,
    last_preview: Option<Instant>,
    preview_text: String,
    vad_failures: u32,
}

fn ms(sample: usize) -> i64 {
    (sample as i64 * 1000) / SAMPLE_RATE as i64
}

impl Worker {
    fn new(
        engine: Arc<Engine>,
        config: ListenConfig,
        sink: EventSink,
        shared: Arc<Shared>,
    ) -> Self {
        let mut config = config;
        // Utterances are already cut to speech; masking again with the
        // detector's defaults would blank the pre-roll and drop short words.
        config.options.keep_pauses = true;
        let endpointer = Endpointer::new(config.endpoint.clone());
        Worker {
            engine,
            config,
            sink,
            shared,
            endpointer,
            last_preview: None,
            preview_text: String::new(),
            vad_failures: 0,
        }
    }

    fn run(mut self, mut input: Box<dyn AudioInput>) {
        loop {
            let (window, window_start, finishing, errors) = {
                let mut state = self.shared.lock();
                let available = |s: &State| s.end() / VAD_FRAME;
                while !state.stop
                    && !state.input_ended
                    && state.input_errors.is_empty()
                    && available(&state) < self.endpointer.next_frame() + HOP_FRAMES
                {
                    state = self
                        .shared
                        .wake
                        .wait(state)
                        .unwrap_or_else(|p| p.into_inner());
                }
                let errors = std::mem::take(&mut state.input_errors);
                let finishing = state.stop || state.input_ended || !errors.is_empty();
                let first = self.endpointer.next_frame().saturating_sub(CONTEXT_FRAMES);
                let window_start =
                    (first * VAD_FRAME).max(state.base.div_ceil(VAD_FRAME) * VAD_FRAME);
                // At the end, the last partial frame is padded with silence
                // rather than left unheard.
                let window_end = if finishing {
                    state.end().div_ceil(VAD_FRAME) * VAD_FRAME
                } else {
                    available(&state) * VAD_FRAME
                };
                let mut window = state.slice(window_start, window_end);
                window.resize(window_end.saturating_sub(window_start), 0.0);
                (window, window_start, finishing, errors)
            };
            // Events go out with the lock released: the host may stop the
            // session from its callback, which takes the lock.
            for message in errors {
                (self.sink)(Event::Error { message });
            }
            let finishing = self.classify(&window, window_start / VAD_FRAME) || finishing;
            if finishing {
                if let Some(Boundary::Ended { start, end }) = self.endpointer.flush() {
                    self.finish_utterance(start, end);
                }
                break;
            }
            self.maybe_preview();
            let keep = self.endpointer.keep_from().saturating_sub(CONTEXT_FRAMES) * VAD_FRAME;
            self.shared.lock().drop_before(keep);
        }
        input.stop();
        (self.sink)(Event::Stopped);
    }

    /// Run the detector over `window` (starting at absolute frame `first`)
    /// and feed the frames the endpointer has not seen yet. Returns true when
    /// the detector keeps failing and listening has to end.
    fn classify(&mut self, window: &[f32], first: usize) -> bool {
        let frames = window.len() / VAD_FRAME;
        let probabilities = match self.engine.speech_probabilities(window) {
            Ok(Some(p)) => {
                self.vad_failures = 0;
                p
            }
            Ok(None) => return true,
            Err(e) => {
                self.vad_failures += 1;
                let fatal = self.vad_failures >= MAX_VAD_FAILURES;
                if self.vad_failures == 1 || fatal {
                    let message = if fatal {
                        format!("{e}; listening stopped after {MAX_VAD_FAILURES} failures in a row")
                    } else {
                        e.to_string()
                    };
                    (self.sink)(Event::Error { message });
                }
                // Count the frames as silence so listening moves on instead
                // of retrying the same audio forever.
                vec![0.0; frames]
            }
        };
        for (i, &p) in probabilities.iter().enumerate().take(frames) {
            if first + i < self.endpointer.next_frame() {
                continue; // context only
            }
            match self.endpointer.push(p) {
                Some(Boundary::Started { start }) => {
                    self.last_preview = Some(Instant::now());
                    self.preview_text.clear();
                    (self.sink)(Event::SpeechStart {
                        at_ms: ms(start * VAD_FRAME),
                    });
                }
                Some(Boundary::Ended { start, end }) => self.finish_utterance(start, end),
                None => {}
            }
        }
        self.vad_failures >= MAX_VAD_FAILURES
    }

    fn maybe_preview(&mut self) {
        let (Some(start), Some(since)) = (self.endpointer.utterance_start(), self.last_preview)
        else {
            return;
        };
        if !self.config.preview || since.elapsed() < self.config.preview_interval {
            return;
        }
        self.last_preview = Some(Instant::now());
        let end = self.endpointer.next_frame();
        let from = start.max(end.saturating_sub(PREVIEW_MAX_FRAMES));
        let audio = self.shared.lock().slice(from * VAD_FRAME, end * VAD_FRAME);
        let options = TranscribeOptions {
            beam_size: 1,
            ..self.config.options.clone()
        };
        // The detector waits while a preview decodes; one that overruns its
        // budget is dropped so the end of the utterance is noticed in time.
        let began = Instant::now();
        let budget = self.config.preview_interval * 2;
        let abort = || self.shared.stopping.load(Ordering::Relaxed) || began.elapsed() > budget;
        match self
            .engine
            .transcribe_cancellable(&audio, &options, Some(&abort))
        {
            Ok(t) if !t.text.is_empty() && t.text != self.preview_text => {
                self.preview_text = t.text.clone();
                (self.sink)(Event::Preview { text: t.text });
            }
            Ok(_) | Err(Error::Cancelled) => {}
            Err(e) => (self.sink)(Event::Error {
                message: e.to_string(),
            }),
        }
    }

    fn finish_utterance(&mut self, start: usize, end: usize) {
        let (from, to) = (start * VAD_FRAME, end * VAD_FRAME);
        let audio = self.shared.lock().slice(from, to);
        self.last_preview = None;
        match self.engine.transcribe(&audio, &self.config.options) {
            // Breath, a cough, a door: the detector heard something, whisper
            // found no words.
            Ok(t) if t.text.is_empty() => {}
            Ok(transcript) => (self.sink)(Event::Utterance {
                transcript,
                start_ms: ms(from),
                end_ms: ms(to),
            }),
            Err(e) => (self.sink)(Event::Error {
                message: e.to_string(),
            }),
        }
    }
}
