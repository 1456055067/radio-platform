//! Connection health tracking.

use tokio::time::Instant;

/// Health of the scanner link as observed by the SDK.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    /// No exchange has completed yet.
    Unknown,
    /// The last exchange reached the scanner.
    Healthy,
    /// Recent exchanges failed, but fewer than the unreachable threshold.
    Degraded {
        /// Consecutive failed exchanges.
        consecutive_failures: u32,
    },
    /// Consecutive failures reached the configured threshold.
    Unreachable {
        /// Consecutive failed exchanges.
        consecutive_failures: u32,
    },
}

#[derive(Debug, Default)]
pub(crate) struct HealthTracker {
    consecutive_failures: u32,
    pub(crate) last_success: Option<Instant>,
}

impl HealthTracker {
    pub(crate) fn record(&mut self, reached: bool, unreachable_after: u32) -> Health {
        if reached {
            self.consecutive_failures = 0;
            self.last_success = Some(Instant::now());
            return Health::Healthy;
        }
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        if self.consecutive_failures >= unreachable_after.max(1) {
            Health::Unreachable {
                consecutive_failures: self.consecutive_failures,
            }
        } else {
            Health::Degraded {
                consecutive_failures: self.consecutive_failures,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transitions() {
        let mut t = HealthTracker::default();
        assert_eq!(
            t.record(false, 2),
            Health::Degraded {
                consecutive_failures: 1
            }
        );
        assert_eq!(
            t.record(false, 2),
            Health::Unreachable {
                consecutive_failures: 2
            }
        );
        assert_eq!(t.record(true, 2), Health::Healthy);
        assert!(t.last_success.is_some());
    }
}
