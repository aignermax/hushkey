use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("model not found: {0}")]
    ModelNotFound(String),
    #[error("could not load model {path}: {reason}")]
    ModelLoad { path: PathBuf, reason: String },
    #[error("audio: {0}")]
    Audio(String),
    #[error("transcription failed: {0}")]
    Transcribe(String),
    #[error("invalid argument: {0}")]
    InvalidArgument(String),
    #[error("the session has already stopped")]
    Stopped,
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
