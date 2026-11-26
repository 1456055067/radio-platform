//! Attribute-based access control.
//!
//! A decision combines:
//! - **subject attributes**: tenant, site scope, permissions and token
//!   expiry;
//! - **resource attributes**: tenant and site;
//! - **the action**: a [`Permission`].
//!
//! Every rule must pass. There is no implicit administrator; administrators
//! simply hold every permission.

use std::collections::BTreeSet;

use platform_common::{SiteId, TenantId};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::OffsetDateTime;
use utoipa::ToSchema;

/// Actions subject to authorization.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Permission {
    /// View scanners, status and telemetry.
    ScannerView,
    /// Acquire control leases and issue control commands.
    ScannerControl,
    /// Issue raw protocol commands and revoke others' leases.
    ScannerAdmin,
    /// Enroll, modify and delete scanners.
    ScannerManage,
    /// Create and modify sites.
    SiteManage,
    /// View events and transmissions.
    EventView,
    /// Listen to live audio (Phase 3).
    AudioListen,
    /// Configure recording (Phase 3).
    RecordingManage,
    /// Configure transcription (Phase 5).
    TranscriptionManage,
    /// Export recordings and data.
    Export,
    /// View audit records.
    AuditView,
}

impl Permission {
    /// Every permission.
    pub const ALL: [Self; 11] = [
        Self::ScannerView,
        Self::ScannerControl,
        Self::ScannerAdmin,
        Self::ScannerManage,
        Self::SiteManage,
        Self::EventView,
        Self::AudioListen,
        Self::RecordingManage,
        Self::TranscriptionManage,
        Self::Export,
        Self::AuditView,
    ];
}

/// Which sites a principal may access within its tenant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SiteScope {
    /// Every site in the tenant.
    All,
    /// Only these sites.
    Sites(BTreeSet<SiteId>),
}

impl SiteScope {
    /// True if `site` is covered.
    #[must_use]
    pub fn covers(&self, site: SiteId) -> bool {
        match self {
            Self::All => true,
            Self::Sites(s) => s.contains(&site),
        }
    }

    /// The site list for storage filters (`None` = unrestricted).
    #[must_use]
    pub fn as_filter(&self) -> Option<Vec<SiteId>> {
        match self {
            Self::All => None,
            Self::Sites(s) => Some(s.iter().copied().collect()),
        }
    }
}

/// An authenticated subject.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Principal {
    /// Stable subject identifier (OIDC `sub`).
    pub subject: String,
    /// Display name, if provided.
    pub name: Option<String>,
    /// Tenant.
    pub tenant_id: TenantId,
    /// Site scope.
    pub sites: SiteScope,
    /// Granted permissions.
    pub permissions: BTreeSet<Permission>,
    /// Session expiry.
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
}

/// Resource attributes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resource {
    /// Owning tenant.
    pub tenant_id: TenantId,
    /// Owning site, if the resource is site-scoped.
    pub site_id: Option<SiteId>,
}

/// Why access was denied.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum Denied {
    /// Session expired.
    #[error("session expired")]
    Expired,
    /// Resource belongs to another tenant.
    #[error("resource belongs to another tenant")]
    Tenant,
    /// Site outside the principal's scope.
    #[error("site is outside the caller's scope")]
    Site,
    /// Permission missing.
    #[error("missing permission {0:?}")]
    Permission(Permission),
}

impl Principal {
    /// Authorizes `permission` on `resource` at time `now`.
    ///
    /// # Errors
    /// Returns the first failed rule.
    pub fn authorize(
        &self,
        permission: Permission,
        resource: Resource,
        now: OffsetDateTime,
    ) -> Result<(), Denied> {
        if now >= self.expires_at {
            return Err(Denied::Expired);
        }
        if resource.tenant_id != self.tenant_id {
            return Err(Denied::Tenant);
        }
        if let Some(site) = resource.site_id
            && !self.sites.covers(site)
        {
            return Err(Denied::Site);
        }
        if !self.permissions.contains(&permission) {
            return Err(Denied::Permission(permission));
        }
        Ok(())
    }

    /// True if the principal holds `permission` (tenant-level check only).
    #[must_use]
    pub fn has(&self, permission: Permission) -> bool {
        self.permissions.contains(&permission)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    fn principal(perms: &[Permission], sites: SiteScope) -> Principal {
        Principal {
            subject: "alice".into(),
            name: None,
            tenant_id: TenantId(uuid::Uuid::nil()),
            sites,
            permissions: perms.iter().copied().collect(),
            expires_at: datetime!(2026-11-24 13:00 UTC),
        }
    }

    const NOW: OffsetDateTime = datetime!(2026-11-24 12:00 UTC);

    #[test]
    fn all_rules_apply() {
        let site = SiteId::new();
        let p = principal(&[Permission::ScannerView], SiteScope::Sites([site].into()));
        let res = Resource {
            tenant_id: p.tenant_id,
            site_id: Some(site),
        };
        assert_eq!(p.authorize(Permission::ScannerView, res, NOW), Ok(()));
        assert_eq!(
            p.authorize(Permission::ScannerControl, res, NOW),
            Err(Denied::Permission(Permission::ScannerControl))
        );
        assert_eq!(
            p.authorize(
                Permission::ScannerView,
                Resource {
                    site_id: Some(SiteId::new()),
                    ..res
                },
                NOW
            ),
            Err(Denied::Site)
        );
        assert_eq!(
            p.authorize(
                Permission::ScannerView,
                Resource {
                    tenant_id: TenantId::new(),
                    ..res
                },
                NOW
            ),
            Err(Denied::Tenant)
        );
        assert_eq!(
            p.authorize(
                Permission::ScannerView,
                res,
                datetime!(2026-11-24 13:00 UTC)
            ),
            Err(Denied::Expired)
        );
    }

    #[test]
    fn all_sites_scope() {
        let p = principal(&Permission::ALL, SiteScope::All);
        let res = Resource {
            tenant_id: p.tenant_id,
            site_id: Some(SiteId::new()),
        };
        for perm in Permission::ALL {
            assert_eq!(p.authorize(perm, res, NOW), Ok(()));
        }
        assert_eq!(SiteScope::All.as_filter(), None);
    }
}
