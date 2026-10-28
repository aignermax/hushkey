//! hushkey's speech recognition as a library.
//!
//! An [`Engine`] holds a loaded whisper.cpp model and transcribes 16 kHz mono
//! audio. A [`RecordSession`] records from an [`AudioInput`] (the microphone,
//! or a WAV file for tests and integrations), emits live [`Event::Preview`]s
//! while recording and one [`Event::Final`] when stopped.

mod audio;
mod engine;
mod error;
mod event;
pub mod model;
mod resample;
mod session;

pub use audio::{
    read_wav_16k, AudioInput, AudioSink, Microphone, SamplesFn, WavReplay, SAMPLE_RATE,
};
pub use engine::{init_process, Engine, EngineConfig, Segment, TranscribeOptions, Transcript};
pub use error::{Error, Result};
pub use event::{Event, EventSink};
pub use resample::Resampler;
pub use session::{RecordConfig, RecordSession};
