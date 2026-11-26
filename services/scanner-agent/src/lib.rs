//! Scanner agent: manages local SDS200 scanners and connects outbound to
//! the gateway over WebSocket within mutually authenticated TLS 1.3.
//!
//! - Scanner endpoints are checked against the **local** network policy, so
//!   a compromised gateway cannot point the agent at arbitrary hosts.
//! - Events are buffered (bounded) during upstream outages and replayed on
//!   reconnect. The gateway de-duplicates them by `(agent, session, seq)`.
//! - With `state_dir` set, unacknowledged events are journaled to disk and
//!   survive restarts (same session id, same sequence numbers).
//! - Reconnection uses exponential backoff (250 ms – 30 s).

#![warn(missing_docs)]

mod agent;
pub mod audio;
pub mod buffer;
pub mod config;
pub mod discovery;
pub mod journal;
mod tls;

pub use agent::{AgentHandle, AgentStatus, start};

/// Agent errors.
#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    /// Invalid configuration.
    #[error("configuration: {0}")]
    Config(String),
    /// Upstream connection failure.
    #[error("upstream: {0}")]
    Upstream(String),
}
