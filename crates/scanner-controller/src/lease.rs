//! Exclusive operator control lease types.
//!
//! One operator at a time may hold control of a scanner. Any number of
//! users may observe it read-only without a lease. Leases expire; holders
//! renew them by acquiring again. The leases themselves live in the shared
//! store (`platform_storage::Store::acquire_lease`) so every gateway
//! replica agrees on the holder; see ADR 0008.

use std::time::Duration;

use platform_common::{LeaseId, ScannerId};
use serde::Serialize;
use thiserror::Error;
use time::OffsetDateTime;

/// Lease duration limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeasePolicy {
    /// TTL when none is requested.
    pub default_ttl: Duration,
    /// Maximum TTL a holder may request.
    pub max_ttl: Duration,
}

impl Default for LeasePolicy {
    fn default() -> Self {
        Self {
            default_ttl: Duration::from_secs(300),
            max_ttl: Duration::from_secs(3600),
        }
    }
}

/// An active lease.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Lease {
    /// Lease identifier.
    pub id: LeaseId,
    /// Scanner under control.
    pub scanner_id: ScannerId,
    /// Holder (OIDC subject).
    pub holder: String,
    /// Acquisition time.
    #[serde(with = "time::serde::rfc3339")]
    pub acquired_at: OffsetDateTime,
    /// Expiry time.
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
}

/// Lease errors.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum LeaseError {
    /// Another holder controls the scanner.
    #[error("scanner is controlled by another operator until {expires_at}")]
    Held {
        /// Current holder.
        holder: String,
        /// Current expiry.
        expires_at: OffsetDateTime,
    },
    /// The lease does not exist or has expired.
    #[error("lease not found or expired")]
    NotFound,
    /// The caller does not hold the lease.
    #[error("lease is held by another operator")]
    NotHolder,
    /// Requested TTL exceeds the policy maximum.
    #[error("requested lease duration exceeds the maximum of {0:?}")]
    TtlTooLong(Duration),
    /// A control command was attempted without a lease.
    #[error("an active control lease is required")]
    Required,
}
