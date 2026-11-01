//! Errors of the Sonix backend.

use thiserror::Error;

/// What can go wrong between the service and a keyboard.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum SonixError {
    /// The HID library could not be initialized or could not enumerate.
    #[error("HID unavailable: {0}")]
    Hid(String),
    /// No supported keyboard matched the configured selector.
    #[error("no supported Sonix keyboard whose name contains {match_name:?} is plugged in")]
    NotFound {
        /// The configured `match_name`.
        match_name: String,
    },
    /// A feature report could not be written or read.
    #[error("{step}: {message}")]
    Io {
        /// What the backend was doing.
        step: &'static str,
        /// The library's description.
        message: String,
    },
    /// The keyboard never echoed the command with its acknowledgement byte.
    #[error("command {command:#04x} not acknowledged after {attempts} attempts")]
    NotAcknowledged {
        /// The command byte.
        command: u8,
        /// How many times it was sent.
        attempts: usize,
    },
}

impl SonixError {
    /// Wraps a library error with the step it interrupted.
    pub(crate) fn at(step: &'static str, error: impl std::fmt::Display) -> Self {
        SonixError::Io {
            step,
            message: error.to_string(),
        }
    }
}
