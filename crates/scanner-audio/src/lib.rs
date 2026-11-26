//! SDS200 audio: the RTSP client and RTP receiver the site agent uses to
//! pull a scanner's audio, decode G.711 µ-law, and re-encode it as Opus.
//!
//! The RTSP/RTP behaviour implemented here follows community reports (see
//! `docs/protocol-compliance.md`, "Audio"). It is exercised against the
//! simulator only and is **not verified on SDS200 hardware**.

#![warn(missing_docs)]

pub mod client;
pub mod g711;
pub mod jitter;
pub mod ogg;
pub mod opus_enc;
pub mod rtp;
pub mod rtsp;

pub use client::{CapturedFrame, SessionConfig, SessionEnd, Supervisor, run_session};
pub use jitter::RtpStats;
pub use opus_enc::{FRAME_48K, FRAME_MS, OpusFrame};
