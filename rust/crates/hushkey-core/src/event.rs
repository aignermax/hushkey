use std::sync::Arc;

use crate::engine::Transcript;

/// What a session reports, in order, from its worker thread.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// The recording so far (its last seconds), transcribed quickly. Each
    /// preview replaces the previous one.
    Preview { text: String },
    /// The whole recording, transcribed accurately, after stop.
    Final { transcript: Transcript },
    /// Listening: someone started speaking, `at_ms` after the session began.
    SpeechStart { at_ms: i64 },
    /// Listening: one finished utterance. `start_ms`/`end_ms` place it on
    /// the session's timeline; segment times inside the transcript are
    /// relative to `start_ms`.
    Utterance {
        transcript: Transcript,
        start_ms: i64,
        end_ms: i64,
    },
    /// Something went wrong; the session continues if it can.
    Error { message: String },
    /// The session is over; nothing follows.
    Stopped,
}

/// Receives a session's events. Called from the session's worker thread;
/// stopping the session from inside the callback is allowed.
pub type EventSink = Arc<dyn Fn(Event) + Send + Sync>;
