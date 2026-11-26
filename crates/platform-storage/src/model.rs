//! Stored records.

use std::net::IpAddr;

use ipnet::IpNet;
use platform_common::{
    AgentId, GroupId, LeaseId, NetworkPolicy, ScannerHealth, ScannerId, SiteId, TenantId,
    TransmissionId,
};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use utoipa::ToSchema;

/// A geographic site.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Site {
    /// Identifier.
    pub id: SiteId,
    /// Owning tenant.
    pub tenant_id: TenantId,
    /// Name (unique per tenant).
    pub name: String,
    /// Description.
    pub description: Option<String>,
    /// Latitude in degrees.
    pub latitude: Option<f64>,
    /// Longitude in degrees.
    pub longitude: Option<f64>,
    /// Networks in which scanners at this site may be enrolled.
    #[schema(value_type = Vec<String>)]
    pub allowed_networks: Vec<IpNet>,
    /// Creation time.
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub created_at: OffsetDateTime,
}

impl Site {
    /// The enrollment network policy for this site.
    #[must_use]
    pub fn policy(&self, allowed_ports: Vec<u16>, audio_ports: Vec<u16>) -> NetworkPolicy {
        NetworkPolicy {
            allowed_networks: self.allowed_networks.clone(),
            allowed_ports,
            audio_ports,
        }
    }
}

/// Input for creating a site.
#[derive(Debug, Clone, PartialEq)]
pub struct NewSite {
    /// Tenant.
    pub tenant_id: TenantId,
    /// Name.
    pub name: String,
    /// Description.
    pub description: Option<String>,
    /// Latitude.
    pub latitude: Option<f64>,
    /// Longitude.
    pub longitude: Option<f64>,
    /// Authorized scanner networks.
    pub allowed_networks: Vec<IpNet>,
}

/// A registered scanner agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Agent {
    /// Identity (mTLS certificate).
    pub id: AgentId,
    /// Tenant.
    pub tenant_id: TenantId,
    /// Site the agent serves.
    pub site_id: SiteId,
    /// Description.
    pub description: Option<String>,
    /// Last connection time.
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub last_seen_at: Option<OffsetDateTime>,
}

/// A scanner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ScannerRecord {
    /// Identifier.
    pub id: ScannerId,
    /// Tenant.
    pub tenant_id: TenantId,
    /// Site.
    pub site_id: SiteId,
    /// Managing agent.
    pub agent_id: AgentId,
    /// Display name.
    pub name: String,
    /// IP address.
    #[schema(value_type = String)]
    pub host: IpAddr,
    /// UDP command port.
    pub port: u16,
    /// Managed by the agent.
    pub enabled: bool,
    /// Reported model.
    pub model: Option<String>,
    /// Reported firmware.
    pub firmware: Option<String>,
    /// Last reported health.
    pub health: Option<ScannerHealth>,
    /// Live audio may be pulled from the scanner (off by default).
    pub audio_enabled: bool,
    /// Transmissions are recorded (off by default; needs audio).
    pub recording_enabled: bool,
    /// Recordings are transcribed (off by default; needs recording).
    pub transcription_enabled: bool,
    /// Scanner RTSP port (554 on the SDS200).
    pub rtsp_port: u16,
    /// Creation time.
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub created_at: OffsetDateTime,
    /// Last update time.
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub updated_at: OffsetDateTime,
}

/// Input for creating a scanner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewScanner {
    /// Tenant.
    pub tenant_id: TenantId,
    /// Site.
    pub site_id: SiteId,
    /// Agent.
    pub agent_id: AgentId,
    /// Name.
    pub name: String,
    /// Address.
    pub host: IpAddr,
    /// Port.
    pub port: u16,
    /// Enabled.
    pub enabled: bool,
}

/// Partial scanner update.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScannerPatch {
    /// New name.
    pub name: Option<String>,
    /// New enabled flag.
    pub enabled: Option<bool>,
    /// New address.
    pub host: Option<IpAddr>,
    /// New port.
    pub port: Option<u16>,
    /// Enable or disable live audio.
    pub audio_enabled: Option<bool>,
    /// Enable or disable recording.
    pub recording_enabled: Option<bool>,
    /// Enable or disable transcription.
    pub transcription_enabled: Option<bool>,
    /// New RTSP port.
    pub rtsp_port: Option<u16>,
}

/// Scanner listing filter.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScannerFilter {
    /// Restrict to a site.
    pub site_id: Option<SiteId>,
    /// Restrict to a set of sites (ABAC site scope); `None` means all.
    pub site_ids: Option<Vec<SiteId>>,
    /// Restrict to an agent.
    pub agent_id: Option<AgentId>,
}

/// A stored scanner event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct EventRecord {
    /// Monotonic identifier.
    pub id: i64,
    /// Tenant.
    pub tenant_id: TenantId,
    /// Site.
    pub site_id: SiteId,
    /// Scanner.
    pub scanner_id: ScannerId,
    /// Reporting agent.
    pub agent_id: AgentId,
    /// Event kind (`transmission_started`, ...).
    pub kind: String,
    /// Event payload.
    #[schema(value_type = Object)]
    pub payload: serde_json::Value,
    /// Receipt time.
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub received_at: OffsetDateTime,
}

/// Input for appending an event.
#[derive(Debug, Clone, PartialEq)]
pub struct NewEvent {
    /// Tenant.
    pub tenant_id: TenantId,
    /// Site.
    pub site_id: SiteId,
    /// Scanner.
    pub scanner_id: ScannerId,
    /// Agent.
    pub agent_id: AgentId,
    /// Agent process session.
    pub agent_session: uuid::Uuid,
    /// Agent sequence number (≥ 1).
    pub agent_seq: u64,
    /// Kind.
    pub kind: String,
    /// Payload.
    pub payload: serde_json::Value,
}

/// Event listing filter.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EventFilter {
    /// Restrict to a scanner.
    pub scanner_id: Option<ScannerId>,
    /// Restrict to a set of sites; `None` means all.
    pub site_ids: Option<Vec<SiteId>>,
    /// Restrict to a kind.
    pub kind: Option<String>,
}

/// A transmission's transcription status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptionState {
    /// Queued (or being retried).
    Pending,
    /// Transcribed (the transcript may be empty if no speech was found).
    Done,
    /// Gave up; see `transcription_error`.
    Failed,
    /// Not sent (for example too short).
    Skipped,
}

impl TranscriptionState {
    /// Database representation.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
        }
    }

    /// Parses the database representation.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(Self::Pending),
            "done" => Some(Self::Done),
            "failed" => Some(Self::Failed),
            "skipped" => Some(Self::Skipped),
            _ => None,
        }
    }
}

/// A transmission and, when enabled, its recording and transcript.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct TransmissionRecord {
    /// Identifier.
    pub id: TransmissionId,
    /// Tenant.
    pub tenant_id: TenantId,
    /// Site.
    pub site_id: SiteId,
    /// Scanner.
    pub scanner_id: ScannerId,
    /// Start (UTC).
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub started_at: OffsetDateTime,
    /// End (UTC).
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub ended_at: Option<OffsetDateTime>,
    /// Duration in milliseconds.
    pub duration_ms: Option<u64>,
    /// Frequency in Hz.
    pub frequency_hz: Option<u64>,
    /// Talkgroup.
    pub tgid: Option<String>,
    /// Radio unit ID.
    pub unit_id: Option<String>,
    /// System name.
    pub system_name: Option<String>,
    /// Department name.
    pub department_name: Option<String>,
    /// Channel name.
    pub channel_name: Option<String>,
    /// Modulation.
    pub modulation: Option<String>,
    /// Signal bars.
    pub signal_bars: Option<u8>,
    /// RSSI.
    pub rssi: Option<f32>,
    /// Audio object key (Phase 3).
    pub audio_object: Option<String>,
    /// Machine transcript (Phase 5). Removed from API responses for
    /// principals without `audio_listen`.
    pub transcript: Option<String>,
    /// Transcription status (`None`: never requested).
    pub transcription_state: Option<TranscriptionState>,
    /// Last transcription error.
    pub transcription_error: Option<String>,
    /// Language reported by the transcription service.
    pub transcript_language: Option<String>,
    /// Model that produced the transcript.
    pub transcript_model: Option<String>,
    /// When the transcript was produced.
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub transcribed_at: Option<OffsetDateTime>,
    /// Exempt from retention and deletion.
    pub hold: bool,
}

impl TransmissionRecord {
    /// Removes transcript content (for principals without `audio_listen`).
    pub fn redact_transcript(&mut self) {
        self.transcript = None;
        self.transcript_language = None;
        self.transcription_error = None;
    }
}

/// Transmission listing filter.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TransmissionFilter {
    /// Scanner.
    pub scanner_id: Option<ScannerId>,
    /// Restrict to a site.
    pub site_id: Option<SiteId>,
    /// Restrict to a set of sites; `None` means all.
    pub site_ids: Option<Vec<SiteId>>,
    /// Exact frequency in Hz.
    pub frequency_hz: Option<u64>,
    /// Exact talkgroup.
    pub tgid: Option<String>,
    /// Started at or after.
    pub from: Option<OffsetDateTime>,
    /// Started before.
    pub to: Option<OffsetDateTime>,
    /// Full-text query over transcripts.
    pub text: Option<String>,
    /// Only transmissions with (`true`) or without (`false`) a recording.
    pub recorded: Option<bool>,
    /// Transcription status.
    pub transcription_state: Option<TranscriptionState>,
}

/// A claimed transcription job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptionJob {
    /// Tenant.
    pub tenant_id: TenantId,
    /// Transmission.
    pub transmission_id: TransmissionId,
    /// Scanner.
    pub scanner_id: ScannerId,
    /// Recording key.
    pub audio_object: String,
    /// Transmission duration, if ended.
    pub duration_ms: Option<u64>,
    /// Attempts including this one.
    pub attempts: u32,
}

/// The result of a transcription attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranscriptionResult {
    /// Transcribed.
    Done {
        /// Text (may be empty).
        text: String,
        /// Reported language.
        language: Option<String>,
        /// Model.
        model: Option<String>,
    },
    /// Try again at `at`.
    Retry {
        /// Error.
        error: String,
        /// Next attempt.
        at: OffsetDateTime,
    },
    /// Gave up.
    Failed {
        /// Error.
        error: String,
    },
    /// Not sent.
    Skipped {
        /// Reason.
        reason: String,
    },
}

/// Transcription queue counters for one tenant.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TranscriptionStats {
    /// Waiting or being retried.
    pub pending: u64,
    /// Failed (transmissions started in the window).
    pub failed: u64,
    /// Done (transmissions started in the window).
    pub done: u64,
}

/// A recording past its retention limit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpiredRecording {
    /// Tenant.
    pub tenant_id: TenantId,
    /// Transmission.
    pub transmission_id: TransmissionId,
    /// Recording key.
    pub audio_object: String,
}

/// Rows removed per tenant by a retention step.
pub type TenantCounts = Vec<(TenantId, u64)>;

/// Analytics time bucket.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Bucket {
    /// One hour.
    Hour,
    /// One day (UTC).
    Day,
}

impl Bucket {
    /// Length.
    #[must_use]
    pub fn duration(self) -> time::Duration {
        match self {
            Self::Hour => time::Duration::HOUR,
            Self::Day => time::Duration::DAY,
        }
    }

    /// Truncates `t` (UTC) to the bucket start.
    #[must_use]
    pub fn floor(self, t: OffsetDateTime) -> OffsetDateTime {
        let t = t.to_offset(time::UtcOffset::UTC);
        let t = t.replace_nanosecond(0).unwrap_or(t);
        let t = t
            .replace_second(0)
            .unwrap_or(t)
            .replace_minute(0)
            .unwrap_or(t);
        match self {
            Self::Hour => t,
            Self::Day => t.replace_hour(0).unwrap_or(t),
        }
    }
}

/// What to rank in [`crate::Store::transmission_top`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TopBy {
    /// Talkgroup.
    Talkgroup,
    /// Channel name.
    Channel,
    /// Frequency.
    Frequency,
    /// Scanner.
    Scanner,
    /// Radio unit ID.
    Unit,
}

/// Analytics filter (`from`/`to` are required by the API).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalyticsFilter {
    /// Started at or after.
    pub from: OffsetDateTime,
    /// Started before.
    pub to: OffsetDateTime,
    /// Site.
    pub site_id: Option<SiteId>,
    /// Site scope; `None` means all.
    pub site_ids: Option<Vec<SiteId>>,
    /// Scanner.
    pub scanner_id: Option<ScannerId>,
}

/// One analytics time bucket.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ActivityPoint {
    /// Bucket start (UTC).
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub start: OffsetDateTime,
    /// Transmissions started in the bucket.
    pub transmissions: u64,
    /// Their total duration in ms.
    pub airtime_ms: u64,
}

/// One ranked value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TopEntry {
    /// Talkgroup, channel, frequency (Hz), scanner id or unit id.
    pub key: String,
    /// Transmissions.
    pub transmissions: u64,
    /// Total duration in ms.
    pub airtime_ms: u64,
}

/// Audit outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AuditOutcome {
    /// Action performed.
    Success,
    /// Authorization denied.
    Denied,
    /// Action attempted and failed.
    Failure,
}

impl AuditOutcome {
    /// Database representation.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Denied => "denied",
            Self::Failure => "failure",
        }
    }

    pub(crate) fn parse(s: &str) -> Self {
        match s {
            "success" => Self::Success,
            "denied" => Self::Denied,
            _ => Self::Failure,
        }
    }
}

/// A stored audit record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct AuditRecord {
    /// Monotonic identifier.
    pub id: i64,
    /// Time.
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub at: OffsetDateTime,
    /// Tenant.
    pub tenant_id: TenantId,
    /// Actor (OIDC subject or agent id).
    pub actor: String,
    /// Action (`scanner.create`, `scanner.command.set_volume`, ...).
    pub action: String,
    /// Resource type.
    pub resource_type: String,
    /// Resource identifier.
    pub resource_id: Option<String>,
    /// Outcome.
    pub outcome: AuditOutcome,
    /// Details.
    #[schema(value_type = Object)]
    pub details: serde_json::Value,
    /// Client address.
    #[schema(value_type = Option<String>)]
    pub source_ip: Option<IpAddr>,
    /// Position in the tenant's hash chain (from 1). `None` for records
    /// written before the chain existed (migration 0005).
    pub seq: Option<u64>,
    /// Hash of the previous record in the chain (hex SHA-256).
    pub prev_hash: Option<String>,
    /// This record's hash (hex SHA-256), see [`crate::audit_chain`].
    pub hash: Option<String>,
}

/// Audit listing filter.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuditFilter {
    /// Exact actor.
    pub actor: Option<String>,
    /// Action prefix (`scanner.` matches `scanner.update`).
    pub action: Option<String>,
    /// Outcome.
    pub outcome: Option<AuditOutcome>,
    /// Resource type.
    pub resource_type: Option<String>,
    /// Resource id.
    pub resource_id: Option<String>,
    /// At or after.
    pub from: Option<OffsetDateTime>,
    /// Before.
    pub to: Option<OffsetDateTime>,
}

impl AuditFilter {
    /// True if `a` passes the filter (the in-memory store's semantics).
    #[must_use]
    pub fn matches(&self, a: &AuditRecord) -> bool {
        self.actor.as_ref().is_none_or(|x| *x == a.actor)
            && self
                .action
                .as_ref()
                .is_none_or(|x| a.action.starts_with(x.as_str()))
            && self.outcome.is_none_or(|x| x == a.outcome)
            && self
                .resource_type
                .as_ref()
                .is_none_or(|x| *x == a.resource_type)
            && self
                .resource_id
                .as_ref()
                .is_none_or(|x| a.resource_id.as_ref() == Some(x))
            && self.from.is_none_or(|x| a.at >= x)
            && self.to.is_none_or(|x| a.at < x)
    }
}

/// The newest link of a tenant's audit chain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AuditHead {
    /// Sequence number of the newest chained record (0 if none).
    pub seq: u64,
    /// Its hash (hex; all zeros if none).
    pub hash: String,
    /// Records written before the chain existed.
    pub unchained: u64,
}

/// Input for an audit record.
#[derive(Debug, Clone, PartialEq)]
pub struct NewAudit {
    /// Tenant.
    pub tenant_id: TenantId,
    /// Actor.
    pub actor: String,
    /// Action.
    pub action: String,
    /// Resource type.
    pub resource_type: String,
    /// Resource id.
    pub resource_id: Option<String>,
    /// Outcome.
    pub outcome: AuditOutcome,
    /// Details.
    pub details: serde_json::Value,
    /// Client address.
    pub source_ip: Option<IpAddr>,
}

/// A scanner group.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ScannerGroup {
    /// Identifier.
    pub id: GroupId,
    /// Tenant.
    pub tenant_id: TenantId,
    /// Name.
    pub name: String,
    /// Members.
    pub members: Vec<ScannerId>,
}

/// An operator control lease on one scanner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseRecord {
    /// Lease identifier (stable across renewals).
    pub id: LeaseId,
    /// Scanner.
    pub scanner_id: ScannerId,
    /// Holder subject.
    pub holder: String,
    /// First acquired.
    #[serde(with = "time::serde::rfc3339")]
    pub acquired_at: OffsetDateTime,
    /// Expiry.
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
}

/// Result of [`crate::Store::acquire_lease`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeaseAcquire {
    /// Granted or renewed (renewal keeps the lease id).
    Granted(LeaseRecord),
    /// Another holder has an unexpired lease.
    Held(LeaseRecord),
}
