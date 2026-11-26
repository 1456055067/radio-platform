//! SDK error type.

use thiserror::Error;
use uniden_protocol::{DecodeError, EncodeError, ScannerError};

/// Errors returned by the SDK.
#[derive(Debug, Error)]
pub enum Error {
    /// Socket error.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// No valid reply arrived in time.
    #[error(
        "`{command}` timed out after {attempts} attempt(s){}",
        if *.state_change_unknown { "; scanner state may have changed" } else { "" }
    )]
    Timeout {
        /// Command name.
        command: String,
        /// Attempts made.
        attempts: u32,
        /// True for state-changing commands: the command may have been
        /// executed even though no reply was received.
        state_change_unknown: bool,
    },
    /// The scanner rejected the command.
    #[error("`{command}` rejected: {error}")]
    Rejected {
        /// Command name.
        command: String,
        /// Scanner-reported error.
        error: ScannerError,
    },
    /// A reply could not be decoded.
    #[error("`{command}` reply could not be decoded: {error}")]
    Decode {
        /// Command name.
        command: String,
        /// Decoding error.
        error: DecodeError,
    },
    /// The command could not be encoded.
    #[error(transparent)]
    Encode(#[from] EncodeError),
    /// The reply decoded but was not the expected kind.
    #[error("`{command}` returned an unexpected reply: {reply}")]
    UnexpectedReply {
        /// Command name.
        command: String,
        /// Debug rendering of the reply.
        reply: String,
    },
    /// The per-scanner command queue is full.
    #[error("command queue is full")]
    QueueFull,
    /// The client has shut down.
    #[error("scanner client is closed")]
    Closed,
}

impl Error {
    /// True if the scanner explicitly rejected the command as unsupported
    /// or invalid (`ERR`).
    #[must_use]
    pub fn is_unsupported(&self) -> bool {
        matches!(
            self,
            Self::Rejected {
                error: ScannerError::CommandError,
                ..
            }
        )
    }
}

/// SDK result alias.
pub type Result<T, E = Error> = std::result::Result<T, E>;
