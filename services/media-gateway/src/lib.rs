//! Live scanner audio for browsers.
//!
//! - [`hub::MediaHub`] receives Opus frames from site agents (via the
//!   gateway's agent connection) and fans them out per scanner, tracking
//!   listener demand so agents only stream while someone listens or the
//!   scanner records.
//! - [`webrtc::WebRtcServer`] serves WHEP-style WebRTC sessions (Opus over
//!   DTLS-SRTP, AWS-LC crypto) from one UDP socket.

#![warn(missing_docs)]

pub mod hub;
pub mod webrtc;

pub use hub::{Demand, MediaHub, StreamStatus, Subscription};
pub use webrtc::{SessionId, WebRtcConfig, WebRtcError, WebRtcServer};
