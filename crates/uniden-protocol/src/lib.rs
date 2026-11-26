//! Sans-I/O implementation of the Uniden SDS-series remote command protocol.
//!
//! This crate performs no I/O. It provides:
//!
//! - [`Command`]: typed commands with wire encoding, [`Effect`] (whether a
//!   command may change scanner state) and [`Provenance`] (whether it comes
//!   from official documentation or community reports);
//! - [`FrameAssembler`]: bounded reassembly of UDP datagrams into line and
//!   XML [`Frame`]s, including multi-part `Footer` sequences;
//! - [`decode_reply`]: typed decoding into [`Reply`], with scanner
//!   rejections (`ERR`, `NG`, ...) surfaced as [`ScannerError`];
//! - safe XML parsing ([`xml`]) for `GSI`/`PSI` and `GLT` replies.
//!
//! # Example
//!
//! ```
//! use uniden_protocol::{Command, FrameAssembler, Reply, decode_reply, xml::XmlLimits};
//!
//! let wire = Command::GetModel.encode()?;
//! assert_eq!(wire, b"MDL\r");
//!
//! let mut assembler = FrameAssembler::default();
//! let frame = assembler.push(b"MDL,SDS200\r")?.remove(0);
//! let reply = decode_reply(&Command::GetModel, frame, &XmlLimits::default())?;
//! assert!(matches!(reply, Reply::Model(m) if m.is_sds200()));
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! # Sources
//!
//! See `docs/protocol-compliance.md` in the repository for the per-command
//! provenance and verification status.

#![warn(missing_docs)]

pub mod command;
pub mod error;
pub mod frame;
pub mod response;
pub mod types;
pub mod xml;

pub use command::{
    Arg, AvoidStatus, Command, Effect, JumpModeKind, ListRequest, MAX_COMMAND_LEN, Provenance,
    RawCommand, ReplyFormat, SCAN_FROM_TOP, Target, TargetKeyword,
};
pub use error::{DecodeError, EncodeError};
pub use frame::{Frame, FrameAssembler, FrameLimits, LineFrame, XmlFrame};
pub use response::{
    DisplayLine, DisplayStatus, Location, QuickKeyState, QuickKeys, Reply, ResponseError,
    ScannerDateTime, ScannerError, answers, decode_reply, scanner_error,
};
pub use types::{FirmwareVersion, Frequency, KeyCode, KeyMode, Model, Squelch, Volume};
