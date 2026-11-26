//! Asynchronous, strongly typed SDK for Uniden SDS200 scanners over Ethernet.
//!
//! The SDK speaks the scanner's native UDP command protocol (default port
//! [`DEFAULT_PORT`]) directly; no ProScan, Sentinel or USB connection is
//! required. Protocol encoding and decoding live in the sans-I/O
//! [`uniden_protocol`] crate, re-exported as [`protocol`].
//!
//! # Guarantees
//!
//! - **Serialized execution**: one command in flight per scanner; all
//!   [`Scanner`] clones share a bounded queue.
//! - **Safe retries**: only [`protocol::Effect::ReadOnly`] commands are
//!   retried (with exponential backoff). A timed-out state-changing command
//!   reports `state_change_unknown`.
//! - **Untrusted input**: replies are size-bounded, XML is parsed without DTD
//!   support, and only datagrams from the configured scanner address are
//!   accepted.
//! - **Honest data**: values the scanner does not report are `None`.
//!
//! # Protocol trust boundary
//!
//! The native protocol is unauthenticated and unencrypted. Anyone on the
//! scanner's network segment can send it commands. Keep scanners on an
//! isolated network reachable only by the scanner agent.
//!
//! # Example
//!
//! ```no_run
//! use uniden_sds::{Scanner, ScannerConfig, protocol::Volume};
//!
//! # async fn demo() -> uniden_sds::Result<()> {
//! let scanner = Scanner::connect(ScannerConfig::new("192.0.2.10:50536".parse().expect("addr"))).await?;
//! let caps = scanner.probe_capabilities().await;
//! if caps.supports("VOL") {
//!     scanner.set_volume(Volume::new(8)?).await?;
//! }
//! let mut updates = scanner.subscribe();
//! scanner.start_push(500).await?;
//! while let Ok(info) = updates.recv().await {
//!     println!("{:?} {:?}", info.system.as_ref().and_then(|s| s.name.clone()), info.active_tgid());
//! }
//! # Ok(())
//! # }
//! ```

#![warn(missing_docs)]

mod client;
mod config;
mod error;
mod health;
mod worker;

pub use client::{Capabilities, Scanner, Support};
pub use config::{Backoff, DEFAULT_PORT, ScannerConfig};
pub use error::{Error, Result};
pub use health::Health;
pub use uniden_protocol as protocol;
pub use worker::{Stats, StatsSnapshot};
