//! Operator control leases, stored in the shared [`Store`] so every gateway
//! replica sees the same holder (ADR 0008). Acquisition and renewal are a
//! single atomic store operation.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use platform_common::{LeaseId, ScannerId, TenantId};
use platform_storage::Store;
use platform_storage::model::{LeaseAcquire, LeaseRecord};
use scanner_controller::{Lease, LeaseError, LeasePolicy};
use time::OffsetDateTime;

use crate::error::ApiError;

fn lease(r: LeaseRecord) -> Lease {
    Lease {
        id: r.id,
        scanner_id: r.scanner_id,
        holder: r.holder,
        acquired_at: r.acquired_at,
        expires_at: r.expires_at,
    }
}

/// Lease operations with the configured policy.
#[derive(Debug, Clone)]
pub struct Leases {
    store: Arc<dyn Store>,
    policy: LeasePolicy,
}

impl Leases {
    /// Creates the service.
    #[must_use]
    pub fn new(store: Arc<dyn Store>, policy: LeasePolicy) -> Self {
        Self { store, policy }
    }

    /// Acquires or renews control for `holder`.
    ///
    /// # Errors
    /// `TtlTooLong`, `Held`, or storage failures.
    pub async fn acquire(
        &self,
        tenant: TenantId,
        scanner: ScannerId,
        holder: &str,
        ttl: Option<Duration>,
        now: OffsetDateTime,
    ) -> Result<Lease, ApiError> {
        let ttl = ttl.unwrap_or(self.policy.default_ttl);
        if ttl > self.policy.max_ttl {
            return Err(LeaseError::TtlTooLong(self.policy.max_ttl).into());
        }
        let ttl = time::Duration::try_from(ttl).unwrap_or(time::Duration::MAX);
        match self
            .store
            .acquire_lease(tenant, scanner, holder, LeaseId::new(), ttl, now)
            .await?
        {
            LeaseAcquire::Granted(r) => Ok(lease(r)),
            LeaseAcquire::Held(r) => Err(LeaseError::Held {
                holder: r.holder,
                expires_at: r.expires_at,
            }
            .into()),
        }
    }

    /// Releases a lease. `force` (admins) may release another holder's.
    ///
    /// # Errors
    /// `NotFound`, `NotHolder`, or storage failures.
    pub async fn release(
        &self,
        tenant: TenantId,
        scanner: ScannerId,
        lease_id: LeaseId,
        holder: &str,
        force: bool,
        now: OffsetDateTime,
    ) -> Result<Lease, ApiError> {
        let current = self.current(tenant, scanner, now).await?;
        let l = current
            .filter(|l| l.id == lease_id)
            .ok_or(LeaseError::NotFound)?;
        if l.holder != holder && !force {
            return Err(LeaseError::NotHolder.into());
        }
        self.store
            .delete_lease(tenant, scanner, lease_id, now)
            .await?
            .map(lease)
            .ok_or_else(|| LeaseError::NotFound.into())
    }

    /// Checks that `holder` may send a control command with `lease_id`.
    ///
    /// # Errors
    /// `Required`, `NotFound`, `NotHolder`, or storage failures.
    pub async fn authorize(
        &self,
        tenant: TenantId,
        scanner: ScannerId,
        lease_id: Option<LeaseId>,
        holder: &str,
        now: OffsetDateTime,
    ) -> Result<(), ApiError> {
        let lease_id = lease_id.ok_or(LeaseError::Required)?;
        let l = self
            .current(tenant, scanner, now)
            .await?
            .filter(|l| l.id == lease_id)
            .ok_or(LeaseError::NotFound)?;
        if l.holder == holder {
            Ok(())
        } else {
            Err(LeaseError::NotHolder.into())
        }
    }

    /// The current lease on a scanner, if any.
    ///
    /// # Errors
    /// Storage failures.
    pub async fn current(
        &self,
        tenant: TenantId,
        scanner: ScannerId,
        now: OffsetDateTime,
    ) -> Result<Option<Lease>, ApiError> {
        Ok(self
            .store
            .current_leases(tenant, &[scanner], now)
            .await?
            .into_iter()
            .next()
            .map(lease))
    }

    /// Current leases for many scanners.
    ///
    /// # Errors
    /// Storage failures.
    pub async fn current_many(
        &self,
        tenant: TenantId,
        scanners: &[ScannerId],
        now: OffsetDateTime,
    ) -> Result<HashMap<ScannerId, Lease>, ApiError> {
        Ok(self
            .store
            .current_leases(tenant, scanners, now)
            .await?
            .into_iter()
            .map(|r| (r.scanner_id, lease(r)))
            .collect())
    }

    /// Deletes expired leases.
    pub async fn purge_expired(&self, now: OffsetDateTime) {
        if let Err(e) = self.store.purge_expired_leases(now).await {
            tracing::warn!(error = %e, "lease purge failed");
        }
    }
}
