//! The one error type of the crate. Each variant says what a caller can DO about it: fetch the model,
//! distrust a file, retry a download, or read a model message.

use std::fmt;
use std::path::PathBuf;

/// What went wrong, worded for the operator who reads it.
#[derive(Debug)]
pub enum NlpError {
    /// A pinned file is not in the model folder: the model was never fetched there.
    NotFetched {
        /// The file that is missing.
        path: PathBuf,
        /// The model's name, for the `vike-nlp fetch` line the message carries.
        model: &'static str,
    },
    /// A file's size or sha256 differs from its pin: a truncated download, a swapped file, or a
    /// repository that changed under the pinned commit. Nothing is loaded from it.
    Digest {
        /// The file (a local path, or the URL of a download).
        what: String,
        /// The pinned value (`size N` or the sha256).
        expected: String,
        /// What was found.
        actual: String,
    },
    /// The download failed: no connection, or an HTTP status other than 2xx.
    Fetch(String),
    /// A filesystem error, with the path it happened on.
    Io { path: PathBuf, source: std::io::Error },
    /// The tokenizer or the tensor engine refused (a malformed `config.json`, a missing tensor, a
    /// shape mismatch). Its own message, prefixed with what was being done.
    Model(String),
}

impl fmt::Display for NlpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NlpError::NotFetched { path, model } => write!(
                f,
                "{} is missing: fetch the model first (`vike-nlp fetch --project <project> --model {model}`)",
                path.display()
            ),
            NlpError::Digest { what, expected, actual } => {
                write!(f, "{what} does not match its pin: expected {expected}, found {actual}")
            }
            NlpError::Fetch(message) => write!(f, "download failed: {message}"),
            NlpError::Io { path, source } => write!(f, "{}: {source}", path.display()),
            NlpError::Model(message) => write!(f, "model: {message}"),
        }
    }
}

impl std::error::Error for NlpError {}

impl From<candle_core::Error> for NlpError {
    fn from(error: candle_core::Error) -> Self {
        NlpError::Model(error.to_string())
    }
}

/// Wrap a tokenizer or tensor-engine error with what was being done.
pub(crate) fn model_error(doing: &str, error: impl fmt::Display) -> NlpError {
    NlpError::Model(format!("{doing}: {error}"))
}
