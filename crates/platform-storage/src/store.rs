//! The storage abstraction.
//!
//! Every read and write of tenant data takes a [`TenantId`], so tenant
//! isolation is enforced at the storage boundary. Lookups by agent identity
//! (`get_agent`, `scanners_for_agent`) are the exception, because the agent's
//! mTLS identity determines its tenant.

use async_trait::async_trait;
use platform_common::{
    AgentId, GroupId, LeaseId, ScannerHealth, ScannerId, SiteId, TelemetrySnapshot, TenantId,
    TransmissionId, TransmissionStart,
};
use thiserror::Error;
use time::OffsetDateTime;

use crate::model::{
    ActivityPoint, Agent, AnalyticsFilter, AuditFilter, AuditHead, AuditRecord, Bucket,
    EventFilter, EventRecord, ExpiredRecording, LeaseAcquire, LeaseRecord, NewAudit, NewEvent,
    NewScanner, NewSite, ScannerFilter, ScannerGroup, ScannerPatch, ScannerRecord, Site,
    TenantCounts, TopBy, TopEntry, TranscriptionJob, TranscriptionResult, TranscriptionStats,
    TransmissionFilter, TransmissionRecord,
};
use crate::page::{Page, PageRequest};

/// Storage errors.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum StoreError {
    /// No such record (or not visible to the tenant).
    #[error("not found")]
    NotFound,
    /// Uniqueness or reference conflict.
    #[error("conflict: {0}")]
    Conflict(String),
    /// Invalid input (for example a malformed cursor).
    #[error("invalid: {0}")]
    Invalid(String),
    /// Backend failure.
    #[error("storage backend error: {0}")]
    Backend(String),
}

/// Result alias.
pub type StoreResult<T> = Result<T, StoreError>;

/// Platform persistence.
#[async_trait]
pub trait Store: Send + Sync + std::fmt::Debug {
    /// Creates the tenant if absent.
    async fn ensure_tenant(&self, id: TenantId, name: &str) -> StoreResult<()>;

    /// Creates a site.
    async fn create_site(&self, site: NewSite) -> StoreResult<Site>;
    /// Gets a site.
    async fn get_site(&self, tenant: TenantId, id: SiteId) -> StoreResult<Site>;
    /// Lists sites ordered by id.
    async fn list_sites(
        &self,
        tenant: TenantId,
        site_ids: Option<&[SiteId]>,
        page: &PageRequest,
    ) -> StoreResult<Page<Site>>;

    /// Creates or updates an agent registration.
    async fn upsert_agent(&self, agent: Agent) -> StoreResult<Agent>;
    /// Gets an agent by identity (any tenant).
    async fn get_agent(&self, id: &AgentId) -> StoreResult<Agent>;
    /// Lists a tenant's agents ordered by identity.
    async fn list_agents(
        &self,
        tenant: TenantId,
        site_ids: Option<&[SiteId]>,
        page: &PageRequest,
    ) -> StoreResult<Page<Agent>>;
    /// Records an agent connection.
    async fn touch_agent(&self, id: &AgentId, at: OffsetDateTime) -> StoreResult<()>;

    /// Creates a scanner.
    async fn create_scanner(&self, scanner: NewScanner) -> StoreResult<ScannerRecord>;
    /// Gets a scanner.
    async fn get_scanner(&self, tenant: TenantId, id: ScannerId) -> StoreResult<ScannerRecord>;
    /// Lists scanners ordered by id.
    async fn list_scanners(
        &self,
        tenant: TenantId,
        filter: &ScannerFilter,
        page: &PageRequest,
    ) -> StoreResult<Page<ScannerRecord>>;
    /// Updates a scanner.
    async fn update_scanner(
        &self,
        tenant: TenantId,
        id: ScannerId,
        patch: ScannerPatch,
    ) -> StoreResult<ScannerRecord>;
    /// Deletes a scanner.
    async fn delete_scanner(&self, tenant: TenantId, id: ScannerId) -> StoreResult<()>;
    /// All scanners assigned to an agent.
    async fn scanners_for_agent(&self, agent: &AgentId) -> StoreResult<Vec<ScannerRecord>>;
    /// Records model, firmware and health reported by an agent.
    async fn set_scanner_status(
        &self,
        id: ScannerId,
        model: Option<String>,
        firmware: Option<String>,
        health: Option<ScannerHealth>,
    ) -> StoreResult<()>;

    /// Stores the latest telemetry snapshot.
    async fn put_telemetry(
        &self,
        scanner: ScannerId,
        snapshot: &TelemetrySnapshot,
    ) -> StoreResult<()>;
    /// Gets the latest telemetry snapshot.
    async fn get_telemetry(
        &self,
        tenant: TenantId,
        scanner: ScannerId,
    ) -> StoreResult<Option<TelemetrySnapshot>>;

    /// Appends an event. Returns `None` if `(agent, session, seq)` was
    /// already stored (replay).
    async fn append_event(&self, event: NewEvent) -> StoreResult<Option<i64>>;
    /// Lists events, newest first.
    async fn list_events(
        &self,
        tenant: TenantId,
        filter: &EventFilter,
        page: &PageRequest,
    ) -> StoreResult<Page<EventRecord>>;

    /// Records the start of a transmission (idempotent by id).
    async fn start_transmission(
        &self,
        tenant: TenantId,
        site: SiteId,
        start: &TransmissionStart,
    ) -> StoreResult<()>;
    /// Attaches a stored recording to a transmission; with `transcribe`,
    /// also queues it for transcription.
    async fn set_transmission_audio(
        &self,
        tenant: TenantId,
        id: TransmissionId,
        object_key: &str,
        transcribe: bool,
        now: OffsetDateTime,
    ) -> StoreResult<()>;
    /// Records the end of a transmission.
    async fn end_transmission(
        &self,
        id: TransmissionId,
        ended_at: OffsetDateTime,
        duration_ms: u64,
    ) -> StoreResult<()>;
    /// Gets a transmission.
    async fn get_transmission(
        &self,
        tenant: TenantId,
        id: TransmissionId,
    ) -> StoreResult<TransmissionRecord>;
    /// Lists transmissions, newest first.
    async fn list_transmissions(
        &self,
        tenant: TenantId,
        filter: &TransmissionFilter,
        page: &PageRequest,
    ) -> StoreResult<Page<TransmissionRecord>>;

    /// Queues (or re-queues) a recorded transmission for transcription.
    /// `NotFound` if absent; `Conflict` if it has no recording.
    async fn request_transcription(
        &self,
        tenant: TenantId,
        id: TransmissionId,
        now: OffsetDateTime,
    ) -> StoreResult<()>;
    /// Claims up to `limit` due jobs (all tenants) until `lease_until`,
    /// incrementing their attempt count. A job whose claim expires is
    /// claimable again.
    async fn claim_transcriptions(
        &self,
        now: OffsetDateTime,
        lease_until: OffsetDateTime,
        limit: u32,
    ) -> StoreResult<Vec<TranscriptionJob>>;
    /// Records the result of a claimed job.
    async fn finish_transcription(
        &self,
        tenant: TenantId,
        id: TransmissionId,
        result: TranscriptionResult,
        now: OffsetDateTime,
    ) -> StoreResult<()>;
    /// Queue counters (`failed` and `done` count transmissions started
    /// since `since`).
    async fn transcription_stats(
        &self,
        tenant: TenantId,
        since: OffsetDateTime,
    ) -> StoreResult<TranscriptionStats>;

    /// Places or releases a retention hold.
    async fn set_transmission_hold(
        &self,
        tenant: TenantId,
        id: TransmissionId,
        hold: bool,
    ) -> StoreResult<TransmissionRecord>;
    /// Unheld recordings of transmissions started before `before` (all
    /// tenants), oldest first.
    async fn expired_recordings(
        &self,
        before: OffsetDateTime,
        limit: u32,
    ) -> StoreResult<Vec<ExpiredRecording>>;
    /// Detaches a recording if its key is still `object_key` and the
    /// transmission is not held. A pending transcription is marked failed.
    /// With `with_transcript`, the transcript is removed too. Returns
    /// whether anything changed.
    async fn clear_transmission_audio(
        &self,
        tenant: TenantId,
        id: TransmissionId,
        object_key: &str,
        with_transcript: bool,
    ) -> StoreResult<bool>;
    /// Removes transcripts of unheld transmissions started before `before`.
    async fn expire_transcripts(
        &self,
        before: OffsetDateTime,
        limit: u32,
    ) -> StoreResult<TenantCounts>;
    /// Deletes unheld transmissions without a recording started before
    /// `before`.
    async fn delete_transmissions_before(
        &self,
        before: OffsetDateTime,
        limit: u32,
    ) -> StoreResult<TenantCounts>;
    /// Deletes events received before `before`.
    async fn delete_events_before(
        &self,
        before: OffsetDateTime,
        limit: u32,
    ) -> StoreResult<TenantCounts>;

    /// Transmission counts and airtime per bucket (empty buckets omitted).
    async fn transmission_activity(
        &self,
        tenant: TenantId,
        filter: &AnalyticsFilter,
        bucket: Bucket,
    ) -> StoreResult<Vec<ActivityPoint>>;
    /// The most frequent values of `by`, by transmission count.
    async fn transmission_top(
        &self,
        tenant: TenantId,
        filter: &AnalyticsFilter,
        by: TopBy,
        limit: u32,
    ) -> StoreResult<Vec<TopEntry>>;

    /// Appends an audit record, linking it into the tenant's hash chain
    /// ([`crate::audit_chain`]) atomically with the chain head.
    async fn append_audit(&self, record: NewAudit) -> StoreResult<i64>;
    /// Lists audit records, newest first.
    async fn list_audit(
        &self,
        tenant: TenantId,
        filter: &AuditFilter,
        page: &PageRequest,
    ) -> StoreResult<Page<AuditRecord>>;
    /// Chained records with `seq > after_seq`, in sequence order.
    async fn audit_chain(
        &self,
        tenant: TenantId,
        after_seq: u64,
        limit: u32,
    ) -> StoreResult<Vec<AuditRecord>>;
    /// The tenant's chain head and count of unchained records.
    async fn audit_head(&self, tenant: TenantId) -> StoreResult<AuditHead>;
    /// Chain heads of every tenant (for anchoring outside the database).
    async fn audit_heads(&self) -> StoreResult<Vec<(TenantId, AuditHead)>>;

    /// Creates a scanner group.
    async fn create_group(&self, tenant: TenantId, name: &str) -> StoreResult<ScannerGroup>;
    /// Adds a scanner to a group.
    async fn add_group_member(
        &self,
        tenant: TenantId,
        group: GroupId,
        scanner: ScannerId,
    ) -> StoreResult<()>;
    /// Lists groups with members.
    async fn list_groups(&self, tenant: TenantId) -> StoreResult<Vec<ScannerGroup>>;
    /// Gets one group with members.
    async fn get_group(&self, tenant: TenantId, group: GroupId) -> StoreResult<ScannerGroup>;
    /// Removes a scanner from a group (`NotFound` if it was not a member).
    async fn remove_group_member(
        &self,
        tenant: TenantId,
        group: GroupId,
        scanner: ScannerId,
    ) -> StoreResult<()>;
    /// Deletes a group (scanners are unaffected).
    async fn delete_group(&self, tenant: TenantId, group: GroupId) -> StoreResult<()>;

    /// Atomically acquires or renews the control lease on a scanner.
    ///
    /// - No lease, or an expired one: a new lease `new_id` for `holder`.
    /// - An unexpired lease of `holder`: renewed to `now + ttl`, keeping its id.
    /// - An unexpired lease of someone else: [`LeaseAcquire::Held`].
    ///
    /// `NotFound` if the scanner is not in the tenant.
    async fn acquire_lease(
        &self,
        tenant: TenantId,
        scanner: ScannerId,
        holder: &str,
        new_id: LeaseId,
        ttl: time::Duration,
        now: OffsetDateTime,
    ) -> StoreResult<LeaseAcquire>;
    /// Unexpired leases for these scanners (in the tenant).
    async fn current_leases(
        &self,
        tenant: TenantId,
        scanners: &[ScannerId],
        now: OffsetDateTime,
    ) -> StoreResult<Vec<LeaseRecord>>;
    /// Deletes the lease if it is the current, unexpired lease `lease`;
    /// returns what was deleted.
    async fn delete_lease(
        &self,
        tenant: TenantId,
        scanner: ScannerId,
        lease: LeaseId,
        now: OffsetDateTime,
    ) -> StoreResult<Option<LeaseRecord>>;
    /// Removes expired leases (all tenants); returns how many.
    async fn purge_expired_leases(&self, now: OffsetDateTime) -> StoreResult<u64>;

    /// Backend liveness check (readiness probe).
    async fn ping(&self) -> StoreResult<()>;
}

pub(crate) fn validate_name(field: &str, name: &str) -> StoreResult<()> {
    let n = name.trim();
    if n.is_empty() || n.chars().count() > 128 || n.chars().any(char::is_control) {
        return Err(StoreError::Invalid(format!(
            "{field} must be 1-128 printable characters"
        )));
    }
    Ok(())
}
