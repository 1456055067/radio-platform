//! Client configuration.

use std::net::SocketAddr;
use std::time::Duration;

use uniden_protocol::FrameLimits;
use uniden_protocol::xml::XmlLimits;

/// Default SDS200 network command port (community-verified; see
/// `docs/protocol-compliance.md`).
pub const DEFAULT_PORT: u16 = 50536;

/// Exponential backoff between retries of read-only commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Backoff {
    /// Delay before the first retry.
    pub initial: Duration,
    /// Upper bound on any delay.
    pub max: Duration,
    /// Multiplier applied after each retry (≥ 1).
    pub multiplier: u32,
}

impl Default for Backoff {
    fn default() -> Self {
        Self {
            initial: Duration::from_millis(100),
            max: Duration::from_secs(2),
            multiplier: 2,
        }
    }
}

impl Backoff {
    /// Delay before retry number `retry` (1-based).
    #[must_use]
    pub fn delay(&self, retry: u32) -> Duration {
        let factor = self
            .multiplier
            .max(1)
            .saturating_pow(retry.saturating_sub(1));
        self.initial.saturating_mul(factor).min(self.max)
    }
}

/// Configuration for one scanner connection.
#[derive(Debug, Clone)]
pub struct ScannerConfig {
    /// Scanner address (IP and UDP port).
    pub address: SocketAddr,
    /// Local address to bind; defaults to an ephemeral port on the
    /// unspecified address of the scanner's family.
    pub bind: Option<SocketAddr>,
    /// Reply timeout for line commands.
    pub line_timeout: Duration,
    /// Reply timeout for XML commands. Community reports describe `GSI`
    /// taking 1–4 s on some firmware.
    pub xml_timeout: Duration,
    /// Additional attempts for read-only commands after a timeout or
    /// undecodable reply. State-changing commands are never retried.
    pub retries: u32,
    /// Retry backoff.
    pub backoff: Backoff,
    /// Maximum queued commands; further requests fail with
    /// [`crate::Error::QueueFull`].
    pub queue_depth: usize,
    /// Receive buffer size (maximum datagram accepted).
    pub max_datagram: usize,
    /// Frame reassembly limits.
    pub frame_limits: FrameLimits,
    /// XML parsing limits.
    pub xml_limits: XmlLimits,
    /// Send `KAL` after this much idle time. Disabled by default because the
    /// command is community-reported only.
    pub keepalive_interval: Option<Duration>,
    /// Consecutive failed exchanges after which the scanner is reported as
    /// [`crate::Health::Unreachable`].
    pub unreachable_after: u32,
    /// Capacity of the push (PSI) broadcast channel.
    pub push_capacity: usize,
}

impl ScannerConfig {
    /// Configuration with defaults for the given scanner address.
    #[must_use]
    pub fn new(address: SocketAddr) -> Self {
        Self {
            address,
            bind: None,
            line_timeout: Duration::from_millis(1500),
            xml_timeout: Duration::from_secs(5),
            retries: 2,
            backoff: Backoff::default(),
            queue_depth: 32,
            max_datagram: 65_535,
            frame_limits: FrameLimits::default(),
            xml_limits: XmlLimits::default(),
            keepalive_interval: None,
            unreachable_after: 3,
            push_capacity: 16,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_and_caps() {
        let b = Backoff::default();
        assert_eq!(b.delay(1), Duration::from_millis(100));
        assert_eq!(b.delay(2), Duration::from_millis(200));
        assert_eq!(b.delay(3), Duration::from_millis(400));
        assert_eq!(b.delay(30), Duration::from_secs(2));
    }
}
