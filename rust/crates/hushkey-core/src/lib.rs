//! hushkey's speech recognition as a library.
//!
//! An [`Engine`] holds a loaded whisper.cpp model and transcribes 16 kHz mono
//! audio. A [`RecordSession`] records from an [`AudioInput`] (the microphone,
//! or a WAV file for tests and integrations), emits live [`Event::Preview`]s
//! while recording and one [`Event::Final`] when stopped. A
//! [`ListenSession`] listens continuously and reports each utterance as an
//! [`Event::Utterance`].

mod audio;
mod brain;
pub mod commands;
pub mod endpoint;
mod engine;
mod error;
mod event;
mod listen;
pub mod model;
mod resample;
mod session;

pub use audio::{
    read_wav_16k, AudioInput, AudioSink, Microphone, SamplesFn, WavReplay, SAMPLE_RATE,
};
pub use brain::Brain;
pub use commands::{CommandMatch, CommandSet};
pub use endpoint::{EndpointConfig, Endpointer};
pub use engine::{
    init_process, Engine, EngineConfig, Segment, TranscribeOptions, Transcript, VAD_FRAME,
};
pub use error::{Error, Result};
pub use event::{Event, EventSink};
pub use listen::{ListenConfig, ListenSession};
pub use resample::Resampler;
pub use session::{RecordConfig, RecordSession};
