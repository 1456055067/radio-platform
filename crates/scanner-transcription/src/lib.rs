//! Transcription of recorded transmissions (Phase 5). **Disabled by
//! default**: it runs only when the gateway configures a service and a
//! `transcription_manage` user enables it per scanner.
//!
//! - [`audio`] decodes a recording (Ogg Opus) to 16 kHz mono PCM and
//!   writes it as WAV, the input every Whisper-compatible server accepts.
//! - [`client::WhisperClient`] posts it to a Whisper-compatible HTTP
//!   service in one of two request styles ([`ApiStyle`]).
//!
//! Machine transcripts can be wrong, and Whisper-family models can invent
//! text on silence or noise. Callers must present results as
//! machine-generated. Implemented from the services' public API
//! documentation and tested against a stub only.

#![warn(missing_docs)]

pub mod audio;
pub mod client;
pub mod config;

pub use audio::{AudioError, SAMPLE_RATE, decode_ogg_opus, wav};
pub use client::{TranscribeError, Transcript, WhisperClient, clean_text};
pub use config::{ApiStyle, ConfigError, TranscriptionConfig};
