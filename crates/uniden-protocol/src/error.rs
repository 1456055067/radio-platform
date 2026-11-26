//! Error types for protocol encoding and decoding.

use thiserror::Error;

/// Errors raised while building a command for transmission.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum EncodeError {
    /// A command argument is outside its documented range or contains
    /// characters that would corrupt protocol framing.
    #[error("invalid argument `{name}`: {reason}")]
    InvalidArgument {
        /// Argument name as used in the specification.
        name: &'static str,
        /// Human-readable reason.
        reason: String,
    },
    /// The encoded command would exceed [`crate::MAX_COMMAND_LEN`].
    #[error("encoded command is {len} bytes, exceeding the {max}-byte limit")]
    TooLong {
        /// Encoded length in bytes.
        len: usize,
        /// Maximum permitted length.
        max: usize,
    },
}

/// Errors raised while decoding data received from a scanner.
///
/// All data received from a scanner is treated as untrusted; every decoding
/// failure is reported through this type rather than by panicking.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DecodeError {
    /// The frame does not follow `CMD[,field...]` syntax.
    #[error("malformed frame: {0}")]
    Malformed(String),
    /// The frame was a reply to a different command than expected.
    #[error("unexpected response: expected `{expected}`, received `{received}`")]
    UnexpectedCommand {
        /// Command name that was awaited.
        expected: String,
        /// Command name that was received.
        received: String,
    },
    /// A field could not be interpreted.
    #[error("invalid field `{field}`: {reason}")]
    InvalidField {
        /// Field name.
        field: &'static str,
        /// Human-readable reason.
        reason: String,
    },
    /// Received data exceeded a configured resource limit.
    #[error("{what} exceeds limit of {limit} bytes")]
    LimitExceeded {
        /// What exceeded the limit.
        what: &'static str,
        /// The limit that was exceeded.
        limit: usize,
    },
    /// XML payload could not be parsed safely.
    #[error("XML error: {0}")]
    Xml(String),
}
