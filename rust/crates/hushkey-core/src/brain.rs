//! Something that answers what was said: Claudio, a local or online model.
//!
//! Listening hands every utterance that is no command to the brain, one at
//! a time, on a thread of its own, so listening goes on while it thinks.
//! Adapters for Claudio and OpenAI-compatible servers are in the
//! `hushkey-brain` crate; hosts can implement their own.

/// An answer.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Reply {
    /// What to show or say.
    pub text: String,
    /// Anything else the brain returned, as JSON, for the host to act on:
    /// Claudio, for one, proposes experiment and desktop actions next to its
    /// text, which hushkey does not carry out itself.
    pub details: Option<String>,
}

/// Answers utterances. Keeps its own conversation history if it wants one.
pub trait Brain: Send {
    /// The answer to `utterance`, or why there is none.
    fn reply(&mut self, utterance: &str) -> Result<Reply, String>;
}
