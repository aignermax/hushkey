//! Push-to-talk: record until stopped, with a live preview.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{JoinHandle, ThreadId};
use std::time::Duration;

use crate::audio::{AudioInput, AudioSink, SAMPLE_RATE};
use crate::engine::{Engine, TranscribeOptions, Transcript};
use crate::error::{Error, Result};
use crate::event::{Event, EventSink};

#[derive(Clone, Debug)]
pub struct RecordConfig {
    /// Emit [`Event::Preview`]s while recording.
    pub preview: bool,
    /// How often the preview is refreshed.
    pub preview_interval: Duration,
    /// How much of the recording's end the preview transcribes; bounds its
    /// cost on long dictations.
    pub preview_window: Duration,
    /// Settings for the final transcription.
    pub options: TranscribeOptions,
}

impl Default for RecordConfig {
    fn default() -> Self {
        RecordConfig {
            preview: true,
            preview_interval: Duration::from_secs(2),
            preview_window: Duration::from_secs(30),
            options: TranscribeOptions::accurate(),
        }
    }
}

#[derive(Default)]
struct State {
    audio: Vec<f32>,
    stop: bool,
    input_errors: Vec<String>,
}

struct Shared {
    state: Mutex<State>,
    wake: Condvar,
    /// Set by stop(); also aborts a preview transcription in flight, so the
    /// final one is not delayed by it.
    stopping: AtomicBool,
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }
}

/// A recording in progress. Stop it to get the final transcript; dropping it
/// stops it too.
pub struct RecordSession {
    shared: Arc<Shared>,
    worker: Option<JoinHandle<Option<Transcript>>>,
    worker_id: ThreadId,
}

impl RecordSession {
    pub fn start(
        engine: Arc<Engine>,
        mut input: Box<dyn AudioInput>,
        config: RecordConfig,
        sink: EventSink,
    ) -> Result<RecordSession> {
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            wake: Condvar::new(),
            stopping: AtomicBool::new(false),
        });
        let (for_samples, for_errors) = (shared.clone(), shared.clone());
        input.start(AudioSink {
            on_samples: Box::new(move |samples| {
                for_samples.lock().audio.extend_from_slice(samples)
            }),
            on_error: Box::new(move |message| {
                for_errors.lock().input_errors.push(message);
                for_errors.wake.notify_all();
            }),
            // A file ran out: keep the session open until the host stops it,
            // exactly like a microphone that went quiet.
            on_end: Box::new(|| {}),
        })?;
        let worker_shared = shared.clone();
        let worker = std::thread::Builder::new()
            .name("hushkey-record".into())
            .spawn(move || {
                let on_panic = sink.clone();
                // The host waits for Stopped (it may have stopped from its
                // callback and can only free its state then): a panic must
                // still end with it.
                catch_unwind(AssertUnwindSafe(|| {
                    run(engine, input, config, sink, worker_shared)
                }))
                .unwrap_or_else(|_| {
                    on_panic(Event::Error {
                        message: "internal error: the recording thread panicked".into(),
                    });
                    on_panic(Event::Stopped);
                    None
                })
            })
            .map_err(Error::Io)?;
        let worker_id = worker.thread().id();
        Ok(RecordSession {
            shared,
            worker: Some(worker),
            worker_id,
        })
    }

    /// Stop recording and transcribe it. Emits [`Event::Final`] and
    /// [`Event::Stopped`]. Returns the transcript, or `None` when called
    /// from inside the session's own event callback: the stop then completes
    /// as soon as that callback returns.
    pub fn stop(&mut self) -> Result<Option<Transcript>> {
        self.shared.stopping.store(true, Ordering::Relaxed);
        self.shared.lock().stop = true;
        self.shared.wake.notify_all();
        if std::thread::current().id() == self.worker_id {
            return Ok(None);
        }
        match self.worker.take() {
            None => Err(Error::Stopped),
            Some(worker) => worker
                .join()
                .map_err(|_| Error::Transcribe("the recording thread panicked".into())),
        }
    }
}

impl Drop for RecordSession {
    fn drop(&mut self) {
        if self.worker.is_some() {
            let _ = self.stop();
        }
    }
}

fn run(
    engine: Arc<Engine>,
    mut input: Box<dyn AudioInput>,
    config: RecordConfig,
    sink: EventSink,
    shared: Arc<Shared>,
) -> Option<Transcript> {
    let window = (config.preview_window.as_secs_f64() * SAMPLE_RATE as f64) as usize;
    let min_preview = SAMPLE_RATE as usize / 2;
    let mut previewed_len = 0;
    let mut last_preview = String::new();
    loop {
        let (snapshot, errors, stop) = {
            let mut state = shared.lock();
            if !state.stop && state.input_errors.is_empty() {
                state = shared
                    .wake
                    .wait_timeout(state, config.preview_interval)
                    .unwrap_or_else(|p| p.into_inner())
                    .0;
            }
            let errors = std::mem::take(&mut state.input_errors);
            let wants_preview = config.preview
                && !state.stop
                && state.audio.len() >= min_preview
                && state.audio.len() > previewed_len;
            let snapshot = wants_preview.then(|| {
                previewed_len = state.audio.len();
                state.audio[state.audio.len().saturating_sub(window)..].to_vec()
            });
            (snapshot, errors, state.stop)
        };
        let input_failed = !errors.is_empty();
        for message in errors {
            sink(Event::Error { message });
        }
        if stop || input_failed {
            break;
        }
        if let Some(audio) = snapshot {
            let options = TranscribeOptions {
                beam_size: 1,
                ..config.options.clone()
            };
            match engine.transcribe_cancellable(&audio, &options, Some(&shared.stopping)) {
                Ok(t) if t.text != last_preview => {
                    last_preview = t.text.clone();
                    sink(Event::Preview { text: t.text });
                }
                Ok(_) | Err(Error::Cancelled) => {}
                Err(e) => sink(Event::Error {
                    message: e.to_string(),
                }),
            }
        }
    }
    input.stop();
    let audio = std::mem::take(&mut shared.lock().audio);
    let transcript = match engine.transcribe(&audio, &config.options) {
        Ok(t) => {
            sink(Event::Final {
                transcript: t.clone(),
            });
            Some(t)
        }
        Err(e) => {
            sink(Event::Error {
                message: e.to_string(),
            });
            None
        }
    };
    sink(Event::Stopped);
    transcript
}
