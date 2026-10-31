//! Errors of the Govee backend.

use thiserror::Error;

/// What can go wrong between the service and a light.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum GoveeError {
    /// No Bluetooth adapter is usable from this process.
    #[error("Bluetooth unavailable: {0}")]
    NoAdapter(String),
    /// No advertising light matched the configured selector.
    #[error("no Govee light advertising a name containing {match_name:?} within {scan_secs} s")]
    NotFound {
        /// The configured `match_name`.
        match_name: String,
        /// How long the scan lasted.
        scan_secs: u64,
    },
    /// The light advertises a model this backend has no frame layout for.
    #[error("Govee model {0} is not supported (known: H6061, H6076)")]
    UnsupportedModel(String),
    /// The encrypted channel could not be opened.
    #[error("handshake failed: {0}")]
    Handshake(String),
    /// Any other Bluetooth failure, with the step it happened at.
    #[error("{step}: {message}")]
    Bluetooth {
        /// What the backend was doing.
        step: &'static str,
        /// The library's description.
        message: String,
    },
}

impl GoveeError {
    /// Wraps a library error with the step it interrupted.
    pub(crate) fn at(step: &'static str, error: impl std::fmt::Display) -> Self {
        GoveeError::Bluetooth {
            step,
            message: error.to_string(),
        }
    }
}
