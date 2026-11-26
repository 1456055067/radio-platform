//! PostgreSQL [`Store`].

use std::net::IpAddr;
use std::str::FromStr;
use std::time::Duration;

use async_trait::async_trait;
use ipnet::IpNet;
use platform_common::{
    AgentId, GroupId, LeaseId, ScannerHealth, ScannerId, SiteId, TelemetrySnapshot, TenantId,
    TransmissionId, TransmissionStart,
};
use sqlx::postgres::{PgPool, PgPoolOptions};
use sqlx::types::Json;
use sqlx::{FromRow, QueryBuilder};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::memory::parse_tx_cursor;
use crate::model::*;
use crate::page::{Page, PageRequest};
use crate::pg_transport::{self, CheckedTarget};
use crate::store::{Store, StoreError, StoreResult, validate_name};

/// Embedded migrations from the workspace `migrations/` directory.
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("../../migrations");

/// PostgreSQL-backed store.
#[derive(Debug, Clone)]
pub struct PgStore {
    pool: PgPool,
}

fn backend(e: sqlx::Error) -> StoreError {
    match &e {
        sqlx::Error::RowNotFound => StoreError::NotFound,
        sqlx::Error::Database(db) => match db.code().as_deref() {
            Some("23505") => StoreError::Conflict("already exists".into()),
            Some("23503") => StoreError::Conflict("referenced record not found".into()),
            Some("23514" | "22P02" | "22003") => StoreError::Invalid(db.message().to_owned()),
            _ => StoreError::Backend(e.to_string()),
        },
        _ => StoreError::Backend(e.to_string()),
    }
}

fn u64_from(v: Option<i64>) -> Option<u64> {
    v.and_then(|x| u64::try_from(x).ok())
}

fn i64_from(v: u64) -> StoreResult<i64> {
    i64::try_from(v).map_err(|_| StoreError::Invalid("value out of range".into()))
}

fn agent_id(s: String) -> StoreResult<AgentId> {
    AgentId::new(s).map_err(|e| StoreError::Backend(e.to_string()))
}

impl PgStore {
    /// The connection pool (shared with the cluster bus).
    #[must_use]
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Connects with a bounded pool.
    ///
    /// This does **not** apply the transport policy in
    /// [`crate::pg_transport`]; it is for tests and the development stack.
    /// Production code uses [`PgStore::connect_target`].
    ///
    /// # Errors
    /// Returns an error if the database is unreachable.
    pub async fn connect(url: &str, max_connections: u32) -> StoreResult<Self> {
        let pool = PgPoolOptions::new()
            .max_connections(max_connections.max(1))
            .acquire_timeout(Duration::from_secs(5))
            .connect(url)
            .await
            .map_err(backend)?;
        Ok(Self { pool })
    }

    /// Connects with a bounded pool to a URL that has passed the transport
    /// policy. `checked` comes only from [`PgTarget::check`] on the result of
    /// [`PgTarget::parse`] for the same `url`, so the policy has accepted the
    /// target before this is reachable. The URL is parsed again with the
    /// process environment and compared with the checked target and with
    /// sqlx's own options, so the pool (and the cluster bus `LISTEN`
    /// connection, which comes from it) connects exactly where the policy
    /// looked.
    ///
    /// [`PgTarget::check`]: crate::PgTarget::check
    /// [`PgTarget::parse`]: crate::PgTarget::parse
    ///
    /// # Errors
    /// [`StoreError::Invalid`] if the URL no longer matches the checked
    /// target or sqlx rejects it (the message never contains the URL);
    /// [`StoreError::Backend`] if the database is unreachable or the TLS
    /// handshake or certificate verification fails.
    pub async fn connect_target(
        url: &str,
        checked: &CheckedTarget,
        max_connections: u32,
    ) -> StoreResult<Self> {
        // NIST 800-53 Rev5: SC-8 (only a policy-accepted target, re-checked against URL and sqlx)
        // POAM: sqlx builds and caches the TLS connector (CA roots) once per
        // process, so rotating the database CA or server certificate to a new
        // CA needs a gateway restart.
        let opts = pg_transport::connect_options(url, checked.target())
            .map_err(|e| StoreError::Invalid(e.to_string()))?;
        let pool = PgPoolOptions::new()
            .max_connections(max_connections.max(1))
            .acquire_timeout(Duration::from_secs(5))
            .connect_with(opts)
            .await
            .map_err(backend)?;
        Ok(Self { pool })
    }

    /// Wraps an existing pool.
    #[must_use]
    pub fn from_pool(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Applies pending migrations.
    ///
    /// # Errors
    /// Returns an error if a migration fails.
    pub async fn migrate(&self) -> StoreResult<()> {
        MIGRATOR
            .run(&self.pool)
            .await
            .map_err(|e| StoreError::Backend(e.to_string()))
    }
}

#[derive(FromRow)]
struct SiteRow {
    id: Uuid,
    tenant_id: Uuid,
    name: String,
    description: Option<String>,
    latitude: Option<f64>,
    longitude: Option<f64>,
    allowed_networks: Vec<String>,
    created_at: OffsetDateTime,
}

impl SiteRow {
    fn into_site(self) -> StoreResult<Site> {
        let allowed_networks = self
            .allowed_networks
            .iter()
            .map(|n| IpNet::from_str(n).map_err(|e| StoreError::Backend(e.to_string())))
            .collect::<StoreResult<Vec<_>>>()?;
        Ok(Site {
            id: SiteId(self.id),
            tenant_id: TenantId(self.tenant_id),
            name: self.name,
            description: self.description,
            latitude: self.latitude,
            longitude: self.longitude,
            allowed_networks,
            created_at: self.created_at,
        })
    }
}

#[derive(FromRow)]
struct AgentRow {
    id: String,
    tenant_id: Uuid,
    site_id: Uuid,
    description: Option<String>,
    last_seen_at: Option<OffsetDateTime>,
}

impl AgentRow {
    fn into_agent(self) -> StoreResult<Agent> {
        Ok(Agent {
            id: agent_id(self.id)?,
            tenant_id: TenantId(self.tenant_id),
            site_id: SiteId(self.site_id),
            description: self.description,
            last_seen_at: self.last_seen_at,
        })
    }
}

#[derive(FromRow)]
struct ScannerRow {
    id: Uuid,
    tenant_id: Uuid,
    site_id: Uuid,
    agent_id: String,
    name: String,
    host: IpNet,
    port: i32,
    enabled: bool,
    model: Option<String>,
    firmware: Option<String>,
    health: Option<Json<ScannerHealth>>,
    audio_enabled: bool,
    recording_enabled: bool,
    transcription_enabled: bool,
    rtsp_port: i32,
    created_at: OffsetDateTime,
    updated_at: OffsetDateTime,
}

impl ScannerRow {
    fn into_record(self) -> StoreResult<ScannerRecord> {
        Ok(ScannerRecord {
            id: ScannerId(self.id),
            tenant_id: TenantId(self.tenant_id),
            site_id: SiteId(self.site_id),
            agent_id: agent_id(self.agent_id)?,
            name: self.name,
            host: self.host.addr(),
            port: u16::try_from(self.port).map_err(|e| StoreError::Backend(e.to_string()))?,
            enabled: self.enabled,
            model: self.model,
            firmware: self.firmware,
            health: self.health.map(|j| j.0),
            audio_enabled: self.audio_enabled,
            recording_enabled: self.recording_enabled,
            transcription_enabled: self.transcription_enabled,
            rtsp_port: u16::try_from(self.rtsp_port)
                .map_err(|e| StoreError::Backend(e.to_string()))?,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}

/// Scanner column list, expanded at compile time so every query string is
/// static (sqlx rejects dynamically built SQL).
macro_rules! scanner_cols {
    () => {
        "id, tenant_id, site_id, agent_id, name, host, port, enabled, model, firmware, health, audio_enabled, recording_enabled, transcription_enabled, rtsp_port, created_at, updated_at"
    };
}

#[derive(FromRow)]
struct EventRow {
    id: i64,
    tenant_id: Uuid,
    site_id: Uuid,
    scanner_id: Uuid,
    agent_id: String,
    kind: String,
    payload: Json<serde_json::Value>,
    received_at: OffsetDateTime,
}

#[derive(FromRow)]
struct TxRow {
    id: Uuid,
    tenant_id: Uuid,
    site_id: Uuid,
    scanner_id: Uuid,
    started_at: OffsetDateTime,
    ended_at: Option<OffsetDateTime>,
    duration_ms: Option<i64>,
    frequency_hz: Option<i64>,
    tgid: Option<String>,
    unit_id: Option<String>,
    system_name: Option<String>,
    department_name: Option<String>,
    channel_name: Option<String>,
    modulation: Option<String>,
    signal_bars: Option<i16>,
    rssi: Option<f32>,
    audio_object: Option<String>,
    transcript: Option<String>,
    transcription_state: Option<String>,
    transcription_error: Option<String>,
    transcript_language: Option<String>,
    transcript_model: Option<String>,
    transcribed_at: Option<OffsetDateTime>,
    hold: bool,
}

/// Transmission column list (excludes the queue bookkeeping and the
/// search vector).
macro_rules! tx_cols {
    () => {
        "id, tenant_id, site_id, scanner_id, started_at, ended_at, duration_ms, frequency_hz, tgid, unit_id,
         system_name, department_name, channel_name, modulation, signal_bars, rssi, audio_object, transcript,
         transcription_state, transcription_error, transcript_language, transcript_model, transcribed_at, hold"
    };
}

impl From<TxRow> for TransmissionRecord {
    fn from(r: TxRow) -> Self {
        Self {
            id: TransmissionId(r.id),
            tenant_id: TenantId(r.tenant_id),
            site_id: SiteId(r.site_id),
            scanner_id: ScannerId(r.scanner_id),
            started_at: r.started_at,
            ended_at: r.ended_at,
            duration_ms: u64_from(r.duration_ms),
            frequency_hz: u64_from(r.frequency_hz),
            tgid: r.tgid,
            unit_id: r.unit_id,
            system_name: r.system_name,
            department_name: r.department_name,
            channel_name: r.channel_name,
            modulation: r.modulation,
            signal_bars: r.signal_bars.and_then(|v| u8::try_from(v).ok()),
            rssi: r.rssi,
            audio_object: r.audio_object,
            transcript: r.transcript,
            transcription_state: r
                .transcription_state
                .as_deref()
                .and_then(TranscriptionState::parse),
            transcription_error: r.transcription_error,
            transcript_language: r.transcript_language,
            transcript_model: r.transcript_model,
            transcribed_at: r.transcribed_at,
            hold: r.hold,
        }
    }
}

#[derive(FromRow)]
struct AuditRow {
    id: i64,
    at: OffsetDateTime,
    tenant_id: Uuid,
    actor: String,
    action: String,
    resource_type: String,
    resource_id: Option<String>,
    outcome: String,
    details: Json<serde_json::Value>,
    source_ip: Option<IpNet>,
    seq: Option<i64>,
    prev_hash: Option<Vec<u8>>,
    hash: Option<Vec<u8>>,
}

impl From<AuditRow> for AuditRecord {
    fn from(r: AuditRow) -> Self {
        Self {
            id: r.id,
            at: r.at,
            tenant_id: TenantId(r.tenant_id),
            actor: r.actor,
            action: r.action,
            resource_type: r.resource_type,
            resource_id: r.resource_id,
            outcome: AuditOutcome::parse(&r.outcome),
            details: r.details.0,
            source_ip: r.source_ip.map(|n| n.addr()),
            seq: r.seq.and_then(|s| u64::try_from(s).ok()),
            prev_hash: r.prev_hash.map(|h| crate::audit_chain::hex(&h)),
            hash: r.hash.map(|h| crate::audit_chain::hex(&h)),
        }
    }
}

const AUDIT_COLS: &str = "id, at, tenant_id, actor, action, resource_type, resource_id, outcome, details, source_ip, seq, prev_hash, hash";

fn head_bytes(h: &[u8]) -> StoreResult<[u8; 32]> {
    <[u8; 32]>::try_from(h).map_err(|_| StoreError::Backend("malformed audit chain head".into()))
}

fn tenant_counts(rows: Vec<(Uuid,)>) -> TenantCounts {
    let mut out = TenantCounts::new();
    for (t,) in rows {
        let t = TenantId(t);
        match out.iter_mut().find(|(x, _)| *x == t) {
            Some((_, n)) => *n += 1,
            None => out.push((t, 1)),
        }
    }
    out
}

fn push_analytics_filter(
    q: &mut QueryBuilder<sqlx::Postgres>,
    tenant: TenantId,
    f: &AnalyticsFilter,
) {
    q.push(" FROM transmissions WHERE tenant_id = ")
        .push_bind(tenant.0)
        .push(" AND started_at >= ")
        .push_bind(f.from)
        .push(" AND started_at < ")
        .push_bind(f.to);
    if let Some(s) = f.site_id {
        q.push(" AND site_id = ").push_bind(s.0);
    }
    if let Some(s) = site_scope(f.site_ids.as_deref()) {
        q.push(" AND site_id = ANY(").push_bind(s).push(")");
    }
    if let Some(s) = f.scanner_id {
        q.push(" AND scanner_id = ").push_bind(s.0);
    }
}

fn limit_i64(limit: u32) -> i64 {
    i64::from(limit.max(1))
}

fn site_scope(scope: Option<&[SiteId]>) -> Option<Vec<Uuid>> {
    scope.map(|s| s.iter().map(|x| x.0).collect())
}

fn cursor_i64(page: &PageRequest) -> StoreResult<Option<i64>> {
    page.decoded_cursor()?
        .map(|c| {
            c.parse()
                .map_err(|_| StoreError::Invalid("malformed cursor".into()))
        })
        .transpose()
}

fn cursor_uuid(page: &PageRequest) -> StoreResult<Option<Uuid>> {
    page.decoded_cursor()?
        .map(|c| Uuid::parse_str(&c).map_err(|_| StoreError::Invalid("malformed cursor".into())))
        .transpose()
}

fn host_net(ip: IpAddr) -> IpNet {
    IpNet::from(ip)
}

#[derive(FromRow)]
struct LeaseRow {
    lease_id: Uuid,
    scanner_id: Uuid,
    holder: String,
    acquired_at: OffsetDateTime,
    expires_at: OffsetDateTime,
}

impl From<LeaseRow> for LeaseRecord {
    fn from(r: LeaseRow) -> Self {
        Self {
            id: LeaseId(r.lease_id),
            scanner_id: ScannerId(r.scanner_id),
            holder: r.holder,
            acquired_at: r.acquired_at,
            expires_at: r.expires_at,
        }
    }
}

#[async_trait]
impl Store for PgStore {
    async fn ensure_tenant(&self, id: TenantId, name: &str) -> StoreResult<()> {
        validate_name("tenant name", name)?;
        sqlx::query("INSERT INTO tenants (id, name) VALUES ($1, $2) ON CONFLICT (id) DO NOTHING")
            .bind(id.0)
            .bind(name)
            .execute(&self.pool)
            .await
            .map_err(backend)?;
        Ok(())
    }

    async fn create_site(&self, s: NewSite) -> StoreResult<Site> {
        validate_name("site name", &s.name)?;
        let nets: Vec<String> = s.allowed_networks.iter().map(ToString::to_string).collect();
        sqlx::query_as::<_, SiteRow>(
            "INSERT INTO sites (id, tenant_id, name, description, latitude, longitude, allowed_networks)
             VALUES ($1, $2, $3, $4, $5, $6, $7)
             RETURNING id, tenant_id, name, description, latitude, longitude, allowed_networks, created_at",
        )
        .bind(SiteId::new().0)
        .bind(s.tenant_id.0)
        .bind(&s.name)
        .bind(&s.description)
        .bind(s.latitude)
        .bind(s.longitude)
        .bind(&nets)
        .fetch_one(&self.pool)
        .await
        .map_err(backend)?
        .into_site()
    }

    async fn get_site(&self, tenant: TenantId, id: SiteId) -> StoreResult<Site> {
        sqlx::query_as::<_, SiteRow>(
            "SELECT id, tenant_id, name, description, latitude, longitude, allowed_networks, created_at
             FROM sites WHERE tenant_id = $1 AND id = $2",
        )
        .bind(tenant.0)
        .bind(id.0)
        .fetch_one(&self.pool)
        .await
        .map_err(backend)?
        .into_site()
    }

    async fn list_sites(
        &self,
        tenant: TenantId,
        scope: Option<&[SiteId]>,
        page: &PageRequest,
    ) -> StoreResult<Page<Site>> {
        let limit = page.limit();
        let rows = sqlx::query_as::<_, SiteRow>(
            "SELECT id, tenant_id, name, description, latitude, longitude, allowed_networks, created_at
             FROM sites
             WHERE tenant_id = $1 AND ($2::uuid IS NULL OR id > $2) AND ($3::uuid[] IS NULL OR id = ANY($3))
             ORDER BY id LIMIT $4",
        )
        .bind(tenant.0)
        .bind(cursor_uuid(page)?)
        .bind(site_scope(scope))
        .bind(i64::from(limit) + 1)
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?;
        let items = rows
            .into_iter()
            .map(SiteRow::into_site)
            .collect::<StoreResult<Vec<_>>>()?;
        Ok(Page::from_overfetch(items, limit, |s| s.id.to_string()))
    }

    async fn upsert_agent(&self, a: Agent) -> StoreResult<Agent> {
        let site_ok: Option<(Uuid,)> =
            sqlx::query_as("SELECT id FROM sites WHERE id = $1 AND tenant_id = $2")
                .bind(a.site_id.0)
                .bind(a.tenant_id.0)
                .fetch_optional(&self.pool)
                .await
                .map_err(backend)?;
        if site_ok.is_none() {
            return Err(StoreError::Conflict(
                "agent site not found in tenant".into(),
            ));
        }
        sqlx::query_as::<_, AgentRow>(
            "INSERT INTO agents (id, tenant_id, site_id, description, last_seen_at) VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (id) DO UPDATE SET tenant_id = EXCLUDED.tenant_id, site_id = EXCLUDED.site_id,
                 description = EXCLUDED.description
             RETURNING id, tenant_id, site_id, description, last_seen_at",
        )
        .bind(a.id.as_str())
        .bind(a.tenant_id.0)
        .bind(a.site_id.0)
        .bind(&a.description)
        .bind(a.last_seen_at)
        .fetch_one(&self.pool)
        .await
        .map_err(backend)?
        .into_agent()
    }

    async fn get_agent(&self, id: &AgentId) -> StoreResult<Agent> {
        sqlx::query_as::<_, AgentRow>(
            "SELECT id, tenant_id, site_id, description, last_seen_at FROM agents WHERE id = $1",
        )
        .bind(id.as_str())
        .fetch_one(&self.pool)
        .await
        .map_err(backend)?
        .into_agent()
    }

    async fn list_agents(
        &self,
        tenant: TenantId,
        scope: Option<&[SiteId]>,
        page: &PageRequest,
    ) -> StoreResult<Page<Agent>> {
        let limit = page.limit();
        let rows = sqlx::query_as::<_, AgentRow>(
            "SELECT id, tenant_id, site_id, description, last_seen_at
             FROM agents
             WHERE tenant_id = $1 AND ($2::text IS NULL OR id COLLATE \"C\" > $2) AND ($3::uuid[] IS NULL OR site_id = ANY($3))
             ORDER BY id COLLATE \"C\" LIMIT $4",
        )
        .bind(tenant.0)
        .bind(page.decoded_cursor()?)
        .bind(site_scope(scope))
        .bind(i64::from(limit) + 1)
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?;
        let items = rows
            .into_iter()
            .map(AgentRow::into_agent)
            .collect::<StoreResult<Vec<_>>>()?;
        Ok(Page::from_overfetch(items, limit, |a| {
            a.id.as_str().to_owned()
        }))
    }

    async fn touch_agent(&self, id: &AgentId, at: OffsetDateTime) -> StoreResult<()> {
        let r = sqlx::query("UPDATE agents SET last_seen_at = $2 WHERE id = $1")
            .bind(id.as_str())
            .bind(at)
            .execute(&self.pool)
            .await
            .map_err(backend)?;
        if r.rows_affected() == 0 {
            Err(StoreError::NotFound)
        } else {
            Ok(())
        }
    }

    async fn create_scanner(&self, s: NewScanner) -> StoreResult<ScannerRecord> {
        validate_name("scanner name", &s.name)?;
        if s.port == 0 {
            return Err(StoreError::Invalid("port must be 1-65535".into()));
        }
        // Site and agent must belong to the same tenant.
        let ok: Option<(i32,)> = sqlx::query_as(
            "SELECT 1 FROM sites s JOIN agents a ON a.tenant_id = s.tenant_id
             WHERE s.id = $1 AND s.tenant_id = $2 AND a.id = $3",
        )
        .bind(s.site_id.0)
        .bind(s.tenant_id.0)
        .bind(s.agent_id.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(backend)?;
        if ok.is_none() {
            return Err(StoreError::Conflict(
                "site or agent not found in tenant".into(),
            ));
        }
        sqlx::query_as::<_, ScannerRow>(concat!(
            "INSERT INTO scanners (id, tenant_id, site_id, agent_id, name, host, port, enabled)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING ",
            scanner_cols!()
        ))
        .bind(ScannerId::new().0)
        .bind(s.tenant_id.0)
        .bind(s.site_id.0)
        .bind(s.agent_id.as_str())
        .bind(&s.name)
        .bind(host_net(s.host))
        .bind(i32::from(s.port))
        .bind(s.enabled)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| match backend(e) {
            StoreError::Conflict(_) => {
                StoreError::Conflict("scanner endpoint already enrolled".into())
            }
            other => other,
        })?
        .into_record()
    }

    async fn get_scanner(&self, tenant: TenantId, id: ScannerId) -> StoreResult<ScannerRecord> {
        sqlx::query_as::<_, ScannerRow>(concat!(
            "SELECT ",
            scanner_cols!(),
            " FROM scanners WHERE tenant_id = $1 AND id = $2"
        ))
        .bind(tenant.0)
        .bind(id.0)
        .fetch_one(&self.pool)
        .await
        .map_err(backend)?
        .into_record()
    }

    async fn list_scanners(
        &self,
        tenant: TenantId,
        f: &ScannerFilter,
        page: &PageRequest,
    ) -> StoreResult<Page<ScannerRecord>> {
        let limit = page.limit();
        let rows = sqlx::query_as::<_, ScannerRow>(concat!("SELECT ", scanner_cols!(), " FROM scanners
             WHERE tenant_id = $1 AND ($2::uuid IS NULL OR id > $2) AND ($3::uuid IS NULL OR site_id = $3)
               AND ($4::uuid[] IS NULL OR site_id = ANY($4)) AND ($5::text IS NULL OR agent_id = $5)
             ORDER BY id LIMIT $6"))
        .bind(tenant.0)
        .bind(cursor_uuid(page)?)
        .bind(f.site_id.map(|s| s.0))
        .bind(site_scope(f.site_ids.as_deref()))
        .bind(f.agent_id.as_ref().map(AgentId::as_str))
        .bind(i64::from(limit) + 1)
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?;
        let items = rows
            .into_iter()
            .map(ScannerRow::into_record)
            .collect::<StoreResult<Vec<_>>>()?;
        Ok(Page::from_overfetch(items, limit, |s| s.id.to_string()))
    }

    async fn update_scanner(
        &self,
        tenant: TenantId,
        id: ScannerId,
        p: ScannerPatch,
    ) -> StoreResult<ScannerRecord> {
        if let Some(n) = &p.name {
            validate_name("scanner name", n)?;
        }
        if p.port == Some(0) || p.rtsp_port == Some(0) {
            return Err(StoreError::Invalid("port must be 1-65535".into()));
        }
        sqlx::query_as::<_, ScannerRow>(concat!(
            "UPDATE scanners SET name = COALESCE($3, name), enabled = COALESCE($4, enabled),
                 host = COALESCE($5, host), port = COALESCE($6, port),
                 audio_enabled = COALESCE($7, audio_enabled),
                 recording_enabled = COALESCE($8, recording_enabled) AND COALESCE($7, audio_enabled),
                 transcription_enabled = COALESCE($10, transcription_enabled)
                     AND COALESCE($8, recording_enabled) AND COALESCE($7, audio_enabled),
                 rtsp_port = COALESCE($9, rtsp_port),
                 updated_at = now()
             WHERE tenant_id = $1 AND id = $2 RETURNING ",
            scanner_cols!()
        ))
        .bind(tenant.0)
        .bind(id.0)
        .bind(p.name)
        .bind(p.enabled)
        .bind(p.host.map(host_net))
        .bind(p.port.map(i32::from))
        .bind(p.audio_enabled)
        .bind(p.recording_enabled)
        .bind(p.rtsp_port.map(i32::from))
        .bind(p.transcription_enabled)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| match backend(e) {
            StoreError::Conflict(_) => {
                StoreError::Conflict("scanner endpoint already enrolled".into())
            }
            other => other,
        })?
        .into_record()
    }

    async fn delete_scanner(&self, tenant: TenantId, id: ScannerId) -> StoreResult<()> {
        let r = sqlx::query("DELETE FROM scanners WHERE tenant_id = $1 AND id = $2")
            .bind(tenant.0)
            .bind(id.0)
            .execute(&self.pool)
            .await
            .map_err(backend)?;
        if r.rows_affected() == 0 {
            Err(StoreError::NotFound)
        } else {
            Ok(())
        }
    }

    async fn scanners_for_agent(&self, agent: &AgentId) -> StoreResult<Vec<ScannerRecord>> {
        sqlx::query_as::<_, ScannerRow>(concat!(
            "SELECT ",
            scanner_cols!(),
            " FROM scanners WHERE agent_id = $1 ORDER BY id"
        ))
        .bind(agent.as_str())
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?
        .into_iter()
        .map(ScannerRow::into_record)
        .collect()
    }

    async fn set_scanner_status(
        &self,
        id: ScannerId,
        model: Option<String>,
        firmware: Option<String>,
        health: Option<ScannerHealth>,
    ) -> StoreResult<()> {
        let r = sqlx::query(
            "UPDATE scanners SET model = COALESCE($2, model), firmware = COALESCE($3, firmware),
                 health = COALESCE($4, health), updated_at = now() WHERE id = $1",
        )
        .bind(id.0)
        .bind(model)
        .bind(firmware)
        .bind(health.map(Json))
        .execute(&self.pool)
        .await
        .map_err(backend)?;
        if r.rows_affected() == 0 {
            Err(StoreError::NotFound)
        } else {
            Ok(())
        }
    }

    async fn put_telemetry(&self, scanner: ScannerId, snap: &TelemetrySnapshot) -> StoreResult<()> {
        sqlx::query(
            "INSERT INTO telemetry_latest (scanner_id, observed_at, snapshot) VALUES ($1, $2, $3)
             ON CONFLICT (scanner_id) DO UPDATE SET observed_at = EXCLUDED.observed_at, snapshot = EXCLUDED.snapshot
             WHERE telemetry_latest.observed_at <= EXCLUDED.observed_at",
        )
        .bind(scanner.0)
        .bind(snap.observed_at)
        .bind(Json(snap))
        .execute(&self.pool)
        .await
        .map_err(|e| match backend(e) {
            StoreError::Conflict(_) => StoreError::NotFound,
            other => other,
        })?;
        Ok(())
    }

    async fn get_telemetry(
        &self,
        tenant: TenantId,
        scanner: ScannerId,
    ) -> StoreResult<Option<TelemetrySnapshot>> {
        let row: Option<(Option<Json<TelemetrySnapshot>>,)> = sqlx::query_as(
            "SELECT t.snapshot FROM scanners s LEFT JOIN telemetry_latest t ON t.scanner_id = s.id
             WHERE s.tenant_id = $1 AND s.id = $2",
        )
        .bind(tenant.0)
        .bind(scanner.0)
        .fetch_optional(&self.pool)
        .await
        .map_err(backend)?;
        match row {
            None => Err(StoreError::NotFound),
            Some((snap,)) => Ok(snap.map(|j| j.0)),
        }
    }

    async fn append_event(&self, e: NewEvent) -> StoreResult<Option<i64>> {
        if e.agent_seq == 0 {
            return Err(StoreError::Invalid("agent_seq must be >= 1".into()));
        }
        let row: Option<(i64,)> = sqlx::query_as(
            "INSERT INTO events (tenant_id, site_id, scanner_id, agent_id, agent_session, agent_seq, kind, payload)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
             ON CONFLICT (agent_id, agent_session, agent_seq) DO NOTHING RETURNING id",
        )
        .bind(e.tenant_id.0)
        .bind(e.site_id.0)
        .bind(e.scanner_id.0)
        .bind(e.agent_id.as_str())
        .bind(e.agent_session)
        .bind(i64_from(e.agent_seq)?)
        .bind(&e.kind)
        .bind(Json(&e.payload))
        .fetch_optional(&self.pool)
        .await
        .map_err(backend)?;
        Ok(row.map(|r| r.0))
    }

    async fn list_events(
        &self,
        tenant: TenantId,
        f: &EventFilter,
        page: &PageRequest,
    ) -> StoreResult<Page<EventRecord>> {
        let limit = page.limit();
        let rows = sqlx::query_as::<_, EventRow>(
            "SELECT id, tenant_id, site_id, scanner_id, agent_id, kind, payload, received_at FROM events
             WHERE tenant_id = $1 AND ($2::bigint IS NULL OR id < $2) AND ($3::uuid IS NULL OR scanner_id = $3)
               AND ($4::uuid[] IS NULL OR site_id = ANY($4)) AND ($5::text IS NULL OR kind = $5)
             ORDER BY id DESC LIMIT $6",
        )
        .bind(tenant.0)
        .bind(cursor_i64(page)?)
        .bind(f.scanner_id.map(|s| s.0))
        .bind(site_scope(f.site_ids.as_deref()))
        .bind(f.kind.as_deref())
        .bind(i64::from(limit) + 1)
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?;
        let items = rows
            .into_iter()
            .map(|r| {
                Ok(EventRecord {
                    id: r.id,
                    tenant_id: TenantId(r.tenant_id),
                    site_id: SiteId(r.site_id),
                    scanner_id: ScannerId(r.scanner_id),
                    agent_id: agent_id(r.agent_id)?,
                    kind: r.kind,
                    payload: r.payload.0,
                    received_at: r.received_at,
                })
            })
            .collect::<StoreResult<Vec<_>>>()?;
        Ok(Page::from_overfetch(items, limit, |r| r.id.to_string()))
    }

    async fn start_transmission(
        &self,
        tenant: TenantId,
        site: SiteId,
        s: &TransmissionStart,
    ) -> StoreResult<()> {
        let freq = s.frequency_hz.map(i64_from).transpose()?;
        let r = sqlx::query(
            "INSERT INTO transmissions (id, tenant_id, site_id, scanner_id, started_at, frequency_hz, tgid, unit_id,
                 system_name, department_name, channel_name, modulation, signal_bars, rssi)
             SELECT $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14
             WHERE EXISTS (SELECT 1 FROM scanners WHERE id = $4 AND tenant_id = $2)
             ON CONFLICT (id) DO NOTHING",
        )
        .bind(s.transmission_id.0)
        .bind(tenant.0)
        .bind(site.0)
        .bind(s.scanner_id.0)
        .bind(s.started_at)
        .bind(freq)
        .bind(&s.tgid)
        .bind(&s.unit_id)
        .bind(&s.system)
        .bind(&s.department)
        .bind(&s.channel)
        .bind(&s.modulation)
        .bind(s.signal_bars.map(i16::from))
        .bind(s.rssi)
        .execute(&self.pool)
        .await
        .map_err(backend)?;
        if r.rows_affected() == 0 {
            // Either a replay (already present) or an unknown scanner.
            let exists: Option<(i32,)> =
                sqlx::query_as("SELECT 1 FROM transmissions WHERE id = $1")
                    .bind(s.transmission_id.0)
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(backend)?;
            if exists.is_none() {
                return Err(StoreError::NotFound);
            }
        }
        Ok(())
    }

    async fn set_transmission_audio(
        &self,
        tenant: TenantId,
        id: TransmissionId,
        object_key: &str,
        transcribe: bool,
        now: OffsetDateTime,
    ) -> StoreResult<()> {
        let r = sqlx::query(
            "UPDATE transmissions SET audio_object = $3,
                 transcription_state = CASE WHEN $4 THEN 'pending' ELSE transcription_state END,
                 transcription_attempts = CASE WHEN $4 THEN 0 ELSE transcription_attempts END,
                 transcription_next_at = CASE WHEN $4 THEN $5 ELSE transcription_next_at END,
                 transcription_locked_until = CASE WHEN $4 THEN NULL ELSE transcription_locked_until END,
                 transcription_error = CASE WHEN $4 THEN NULL ELSE transcription_error END
             WHERE tenant_id = $1 AND id = $2",
        )
        .bind(tenant.0)
        .bind(id.0)
        .bind(object_key)
        .bind(transcribe)
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(backend)?;
        if r.rows_affected() == 0 {
            return Err(StoreError::NotFound);
        }
        Ok(())
    }

    async fn end_transmission(
        &self,
        id: TransmissionId,
        ended_at: OffsetDateTime,
        duration_ms: u64,
    ) -> StoreResult<()> {
        let r =
            sqlx::query("UPDATE transmissions SET ended_at = $2, duration_ms = $3 WHERE id = $1")
                .bind(id.0)
                .bind(ended_at)
                .bind(i64_from(duration_ms)?)
                .execute(&self.pool)
                .await
                .map_err(backend)?;
        if r.rows_affected() == 0 {
            Err(StoreError::NotFound)
        } else {
            Ok(())
        }
    }

    async fn get_transmission(
        &self,
        tenant: TenantId,
        id: TransmissionId,
    ) -> StoreResult<TransmissionRecord> {
        sqlx::query_as::<_, TxRow>(concat!(
            "SELECT ",
            tx_cols!(),
            " FROM transmissions WHERE tenant_id = $1 AND id = $2"
        ))
        .bind(tenant.0)
        .bind(id.0)
        .fetch_one(&self.pool)
        .await
        .map(Into::into)
        .map_err(backend)
    }

    async fn list_transmissions(
        &self,
        tenant: TenantId,
        f: &TransmissionFilter,
        page: &PageRequest,
    ) -> StoreResult<Page<TransmissionRecord>> {
        let limit = page.limit();
        let cursor = page
            .decoded_cursor()?
            .as_deref()
            .map(parse_tx_cursor)
            .transpose()?;
        let mut q = QueryBuilder::new(concat!(
            "SELECT ",
            tx_cols!(),
            " FROM transmissions WHERE tenant_id = "
        ));
        q.push_bind(tenant.0);
        if let Some((nanos, id)) = cursor {
            let ts = OffsetDateTime::from_unix_timestamp_nanos(nanos)
                .map_err(|_| StoreError::Invalid("malformed cursor".into()))?;
            q.push(" AND (started_at, id) < (")
                .push_bind(ts)
                .push(", ")
                .push_bind(id)
                .push(")");
        }
        if let Some(s) = f.scanner_id {
            q.push(" AND scanner_id = ").push_bind(s.0);
        }
        if let Some(s) = f.site_id {
            q.push(" AND site_id = ").push_bind(s.0);
        }
        if let Some(s) = site_scope(f.site_ids.as_deref()) {
            q.push(" AND site_id = ANY(").push_bind(s).push(")");
        }
        if let Some(fr) = f.frequency_hz {
            q.push(" AND frequency_hz = ").push_bind(i64_from(fr)?);
        }
        if let Some(t) = &f.tgid {
            q.push(" AND tgid = ").push_bind(t.clone());
        }
        if let Some(from) = f.from {
            q.push(" AND started_at >= ").push_bind(from);
        }
        if let Some(to) = f.to {
            q.push(" AND started_at < ").push_bind(to);
        }
        if let Some(r) = f.recorded {
            q.push(if r {
                " AND audio_object IS NOT NULL"
            } else {
                " AND audio_object IS NULL"
            });
        }
        if let Some(st) = f.transcription_state {
            q.push(" AND transcription_state = ").push_bind(st.as_str());
        }
        if let Some(text) = &f.text {
            q.push(" AND transcript_tsv @@ websearch_to_tsquery('simple', ")
                .push_bind(text.clone())
                .push(")");
        }
        q.push(" ORDER BY started_at DESC, id DESC LIMIT ")
            .push_bind(i64::from(limit) + 1);
        let rows = q
            .build_query_as::<TxRow>()
            .fetch_all(&self.pool)
            .await
            .map_err(backend)?;
        let items: Vec<TransmissionRecord> = rows.into_iter().map(Into::into).collect();
        Ok(Page::from_overfetch(items, limit, |t| {
            format!("{}|{}", t.started_at.unix_timestamp_nanos(), t.id)
        }))
    }

    async fn request_transcription(
        &self,
        tenant: TenantId,
        id: TransmissionId,
        now: OffsetDateTime,
    ) -> StoreResult<()> {
        let row: Option<(bool,)> = sqlx::query_as(
            "UPDATE transmissions SET
                 transcription_state = CASE WHEN audio_object IS NULL THEN transcription_state ELSE 'pending' END,
                 transcription_attempts = CASE WHEN audio_object IS NULL THEN transcription_attempts ELSE 0 END,
                 transcription_next_at = CASE WHEN audio_object IS NULL THEN transcription_next_at ELSE $3 END,
                 transcription_locked_until = CASE WHEN audio_object IS NULL THEN transcription_locked_until ELSE NULL END,
                 transcription_error = CASE WHEN audio_object IS NULL THEN transcription_error ELSE NULL END
             WHERE tenant_id = $1 AND id = $2 RETURNING audio_object IS NOT NULL",
        )
        .bind(tenant.0)
        .bind(id.0)
        .bind(now)
        .fetch_optional(&self.pool)
        .await
        .map_err(backend)?;
        match row {
            None => Err(StoreError::NotFound),
            Some((false,)) => Err(StoreError::Conflict("transmission has no recording".into())),
            Some((true,)) => Ok(()),
        }
    }

    async fn claim_transcriptions(
        &self,
        now: OffsetDateTime,
        lease_until: OffsetDateTime,
        limit: u32,
    ) -> StoreResult<Vec<TranscriptionJob>> {
        #[derive(FromRow)]
        struct JobRow {
            tenant_id: Uuid,
            id: Uuid,
            scanner_id: Uuid,
            audio_object: Option<String>,
            duration_ms: Option<i64>,
            transcription_attempts: i32,
        }
        let rows = sqlx::query_as::<_, JobRow>(
            "UPDATE transmissions t
                 SET transcription_attempts = t.transcription_attempts + 1,
                     transcription_locked_until = $2
             FROM (SELECT id FROM transmissions
                   WHERE transcription_state = 'pending' AND transcription_next_at <= $1
                     AND (transcription_locked_until IS NULL OR transcription_locked_until < $1)
                     AND audio_object IS NOT NULL
                   ORDER BY transcription_next_at LIMIT $3
                   FOR UPDATE SKIP LOCKED) due
             WHERE t.id = due.id
             RETURNING t.tenant_id, t.id, t.scanner_id, t.audio_object, t.duration_ms, t.transcription_attempts",
        )
        .bind(now)
        .bind(lease_until)
        .bind(limit_i64(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?;
        Ok(rows
            .into_iter()
            .filter_map(|r| {
                Some(TranscriptionJob {
                    tenant_id: TenantId(r.tenant_id),
                    transmission_id: TransmissionId(r.id),
                    scanner_id: ScannerId(r.scanner_id),
                    audio_object: r.audio_object?,
                    duration_ms: u64_from(r.duration_ms),
                    attempts: u32::try_from(r.transcription_attempts).unwrap_or(0),
                })
            })
            .collect())
    }

    async fn finish_transcription(
        &self,
        tenant: TenantId,
        id: TransmissionId,
        result: TranscriptionResult,
        now: OffsetDateTime,
    ) -> StoreResult<()> {
        let q = match result {
            TranscriptionResult::Done {
                text,
                language,
                model,
            } => sqlx::query(
                "UPDATE transmissions SET transcription_state = 'done', transcript = $3,
                     transcript_language = $4, transcript_model = $5, transcribed_at = $6,
                     transcription_error = NULL, transcription_next_at = NULL,
                     transcription_locked_until = NULL
                 WHERE tenant_id = $1 AND id = $2",
            )
            .bind(tenant.0)
            .bind(id.0)
            .bind(text)
            .bind(language)
            .bind(model)
            .bind(now),
            TranscriptionResult::Retry { error, at } => sqlx::query(
                "UPDATE transmissions SET transcription_state = 'pending', transcription_error = $3,
                     transcription_next_at = $4, transcription_locked_until = NULL
                 WHERE tenant_id = $1 AND id = $2",
            )
            .bind(tenant.0)
            .bind(id.0)
            .bind(error)
            .bind(at),
            TranscriptionResult::Failed { error } => sqlx::query(
                "UPDATE transmissions SET transcription_state = 'failed', transcription_error = $3,
                     transcription_next_at = NULL, transcription_locked_until = NULL
                 WHERE tenant_id = $1 AND id = $2",
            )
            .bind(tenant.0)
            .bind(id.0)
            .bind(error),
            TranscriptionResult::Skipped { reason } => sqlx::query(
                "UPDATE transmissions SET transcription_state = 'skipped', transcription_error = $3,
                     transcription_next_at = NULL, transcription_locked_until = NULL
                 WHERE tenant_id = $1 AND id = $2",
            )
            .bind(tenant.0)
            .bind(id.0)
            .bind(reason),
        };
        let r = q.execute(&self.pool).await.map_err(backend)?;
        if r.rows_affected() == 0 {
            return Err(StoreError::NotFound);
        }
        Ok(())
    }

    async fn transcription_stats(
        &self,
        tenant: TenantId,
        since: OffsetDateTime,
    ) -> StoreResult<TranscriptionStats> {
        let (pending, failed, done): (i64, i64, i64) = sqlx::query_as(
            "SELECT count(*) FILTER (WHERE transcription_state = 'pending'),
                    count(*) FILTER (WHERE transcription_state = 'failed' AND started_at >= $2),
                    count(*) FILTER (WHERE transcription_state = 'done' AND started_at >= $2)
             FROM transmissions
             WHERE tenant_id = $1 AND (transcription_state = 'pending' OR started_at >= $2)",
        )
        .bind(tenant.0)
        .bind(since)
        .fetch_one(&self.pool)
        .await
        .map_err(backend)?;
        let n = |v: i64| u64::try_from(v).unwrap_or(0);
        Ok(TranscriptionStats {
            pending: n(pending),
            failed: n(failed),
            done: n(done),
        })
    }

    async fn set_transmission_hold(
        &self,
        tenant: TenantId,
        id: TransmissionId,
        hold: bool,
    ) -> StoreResult<TransmissionRecord> {
        sqlx::query_as::<_, TxRow>(concat!(
            "UPDATE transmissions SET hold = $3 WHERE tenant_id = $1 AND id = $2 RETURNING ",
            tx_cols!()
        ))
        .bind(tenant.0)
        .bind(id.0)
        .bind(hold)
        .fetch_one(&self.pool)
        .await
        .map(Into::into)
        .map_err(backend)
    }

    async fn expired_recordings(
        &self,
        before: OffsetDateTime,
        limit: u32,
    ) -> StoreResult<Vec<ExpiredRecording>> {
        let rows: Vec<(Uuid, Uuid, String)> = sqlx::query_as(
            "SELECT tenant_id, id, audio_object FROM transmissions
             WHERE started_at < $1 AND NOT hold AND audio_object IS NOT NULL
             ORDER BY started_at LIMIT $2",
        )
        .bind(before)
        .bind(limit_i64(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?;
        Ok(rows
            .into_iter()
            .map(|(t, id, key)| ExpiredRecording {
                tenant_id: TenantId(t),
                transmission_id: TransmissionId(id),
                audio_object: key,
            })
            .collect())
    }

    async fn clear_transmission_audio(
        &self,
        tenant: TenantId,
        id: TransmissionId,
        object_key: &str,
        with_transcript: bool,
    ) -> StoreResult<bool> {
        let r = sqlx::query(
            "UPDATE transmissions SET audio_object = NULL,
                 transcription_state = CASE WHEN $4 THEN NULL
                     WHEN transcription_state = 'pending' THEN 'failed' ELSE transcription_state END,
                 transcription_error = CASE WHEN $4 THEN NULL
                     WHEN transcription_state = 'pending' THEN 'recording deleted' ELSE transcription_error END,
                 transcript = CASE WHEN $4 THEN NULL ELSE transcript END,
                 transcript_language = CASE WHEN $4 THEN NULL ELSE transcript_language END,
                 transcript_model = CASE WHEN $4 THEN NULL ELSE transcript_model END,
                 transcribed_at = CASE WHEN $4 THEN NULL ELSE transcribed_at END,
                 transcription_next_at = NULL, transcription_locked_until = NULL
             WHERE tenant_id = $1 AND id = $2 AND audio_object = $3 AND NOT hold",
        )
        .bind(tenant.0)
        .bind(id.0)
        .bind(object_key)
        .bind(with_transcript)
        .execute(&self.pool)
        .await
        .map_err(backend)?;
        Ok(r.rows_affected() > 0)
    }

    async fn expire_transcripts(
        &self,
        before: OffsetDateTime,
        limit: u32,
    ) -> StoreResult<TenantCounts> {
        let rows: Vec<(Uuid,)> = sqlx::query_as(
            "UPDATE transmissions SET transcript = NULL, transcript_language = NULL,
                 transcription_error = 'transcript removed by retention policy'
             WHERE id IN (SELECT id FROM transmissions
                          WHERE started_at < $1 AND NOT hold AND transcript IS NOT NULL LIMIT $2)
             RETURNING tenant_id",
        )
        .bind(before)
        .bind(limit_i64(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?;
        Ok(tenant_counts(rows))
    }

    async fn delete_transmissions_before(
        &self,
        before: OffsetDateTime,
        limit: u32,
    ) -> StoreResult<TenantCounts> {
        let rows: Vec<(Uuid,)> = sqlx::query_as(
            "DELETE FROM transmissions
             WHERE id IN (SELECT id FROM transmissions
                          WHERE started_at < $1 AND NOT hold AND audio_object IS NULL LIMIT $2)
             RETURNING tenant_id",
        )
        .bind(before)
        .bind(limit_i64(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?;
        Ok(tenant_counts(rows))
    }

    async fn delete_events_before(
        &self,
        before: OffsetDateTime,
        limit: u32,
    ) -> StoreResult<TenantCounts> {
        let rows: Vec<(Uuid,)> = sqlx::query_as(
            "DELETE FROM events
             WHERE id IN (SELECT id FROM events WHERE received_at < $1 LIMIT $2)
             RETURNING tenant_id",
        )
        .bind(before)
        .bind(limit_i64(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?;
        Ok(tenant_counts(rows))
    }

    async fn transmission_activity(
        &self,
        tenant: TenantId,
        f: &AnalyticsFilter,
        bucket: Bucket,
    ) -> StoreResult<Vec<ActivityPoint>> {
        let mut q = QueryBuilder::new("SELECT date_trunc(");
        q.push_bind(match bucket {
            Bucket::Hour => "hour",
            Bucket::Day => "day",
        })
        .push(", started_at, 'UTC') AS start, count(*)::bigint, coalesce(sum(duration_ms), 0)::bigint");
        push_analytics_filter(&mut q, tenant, f);
        q.push(" GROUP BY 1 ORDER BY 1");
        let rows: Vec<(OffsetDateTime, i64, i64)> = q
            .build_query_as()
            .fetch_all(&self.pool)
            .await
            .map_err(backend)?;
        Ok(rows
            .into_iter()
            .map(|(start, n, a)| ActivityPoint {
                start,
                transmissions: u64::try_from(n).unwrap_or(0),
                airtime_ms: u64::try_from(a).unwrap_or(0),
            })
            .collect())
    }

    async fn transmission_top(
        &self,
        tenant: TenantId,
        f: &AnalyticsFilter,
        by: TopBy,
        limit: u32,
    ) -> StoreResult<Vec<TopEntry>> {
        // Static column expressions only (never user input).
        let col = match by {
            TopBy::Talkgroup => "tgid",
            TopBy::Channel => "channel_name",
            TopBy::Frequency => "frequency_hz::text",
            TopBy::Scanner => "scanner_id::text",
            TopBy::Unit => "unit_id",
        };
        let mut q = QueryBuilder::new("SELECT ");
        q.push(col)
            .push(" AS key, count(*)::bigint AS n, coalesce(sum(duration_ms), 0)::bigint");
        push_analytics_filter(&mut q, tenant, f);
        q.push(" AND ")
            .push(col)
            .push(" IS NOT NULL GROUP BY 1 ORDER BY n DESC, key LIMIT ")
            .push_bind(limit_i64(limit));
        let rows: Vec<(String, i64, i64)> = q
            .build_query_as()
            .fetch_all(&self.pool)
            .await
            .map_err(backend)?;
        Ok(rows
            .into_iter()
            .map(|(key, n, a)| TopEntry {
                key,
                transmissions: u64::try_from(n).unwrap_or(0),
                airtime_ms: u64::try_from(a).unwrap_or(0),
            })
            .collect())
    }

    async fn append_audit(&self, r: NewAudit) -> StoreResult<i64> {
        let mut tx = self.pool.begin().await.map_err(backend)?;
        sqlx::query(
            "INSERT INTO audit_chain_heads (tenant_id, seq, hash) VALUES ($1, 0, $2)
             ON CONFLICT (tenant_id) DO NOTHING",
        )
        .bind(r.tenant_id.0)
        .bind(&crate::audit_chain::GENESIS[..])
        .execute(&mut *tx)
        .await
        .map_err(backend)?;
        // Serializes appends per tenant.
        let (seq, prev): (i64, Vec<u8>) = sqlx::query_as(
            "SELECT seq, hash FROM audit_chain_heads WHERE tenant_id = $1 FOR UPDATE",
        )
        .bind(r.tenant_id.0)
        .fetch_one(&mut *tx)
        .await
        .map_err(backend)?;
        let prev = head_bytes(&prev)?;
        let seq = u64::try_from(seq).unwrap_or(0) + 1;
        let mut rec = AuditRecord {
            id: 0,
            at: crate::audit_chain::micros(platform_common::now_utc()),
            tenant_id: r.tenant_id,
            actor: r.actor,
            action: r.action,
            resource_type: r.resource_type,
            resource_id: r.resource_id,
            outcome: r.outcome,
            details: r.details,
            source_ip: r.source_ip,
            seq: Some(seq),
            prev_hash: None,
            hash: None,
        };
        let h = crate::audit_chain::link(&prev, &rec, seq);
        let (id,): (i64,) = sqlx::query_as(
            "INSERT INTO audit_log (tenant_id, actor, action, resource_type, resource_id, outcome, details, source_ip, at, seq, prev_hash, hash)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12) RETURNING id",
        )
        .bind(rec.tenant_id.0)
        .bind(&rec.actor)
        .bind(&rec.action)
        .bind(&rec.resource_type)
        .bind(&rec.resource_id)
        .bind(rec.outcome.as_str())
        .bind(Json(&rec.details))
        .bind(rec.source_ip.map(host_net))
        .bind(rec.at)
        .bind(i64_from(seq)?)
        .bind(&prev[..])
        .bind(&h[..])
        .fetch_one(&mut *tx)
        .await
        .map_err(backend)?;
        sqlx::query("UPDATE audit_chain_heads SET seq = $2, hash = $3 WHERE tenant_id = $1")
            .bind(rec.tenant_id.0)
            .bind(i64_from(seq)?)
            .bind(&h[..])
            .execute(&mut *tx)
            .await
            .map_err(backend)?;
        tx.commit().await.map_err(backend)?;
        rec.id = id;
        Ok(id)
    }

    async fn list_audit(
        &self,
        tenant: TenantId,
        f: &AuditFilter,
        page: &PageRequest,
    ) -> StoreResult<Page<AuditRecord>> {
        let limit = page.limit();
        let mut q = QueryBuilder::new("SELECT ");
        q.push(AUDIT_COLS)
            .push(" FROM audit_log WHERE tenant_id = ")
            .push_bind(tenant.0);
        if let Some(c) = cursor_i64(page)? {
            q.push(" AND id < ").push_bind(c);
        }
        if let Some(a) = &f.actor {
            q.push(" AND actor = ").push_bind(a.clone());
        }
        if let Some(a) = &f.action {
            q.push(" AND starts_with(action, ")
                .push_bind(a.clone())
                .push(")");
        }
        if let Some(o) = f.outcome {
            q.push(" AND outcome = ").push_bind(o.as_str());
        }
        if let Some(t) = &f.resource_type {
            q.push(" AND resource_type = ").push_bind(t.clone());
        }
        if let Some(t) = &f.resource_id {
            q.push(" AND resource_id = ").push_bind(t.clone());
        }
        if let Some(t) = f.from {
            q.push(" AND at >= ").push_bind(t);
        }
        if let Some(t) = f.to {
            q.push(" AND at < ").push_bind(t);
        }
        q.push(" ORDER BY id DESC LIMIT ")
            .push_bind(i64::from(limit) + 1);
        let rows = q
            .build_query_as::<AuditRow>()
            .fetch_all(&self.pool)
            .await
            .map_err(backend)?;
        let items = rows.into_iter().map(Into::into).collect();
        Ok(Page::from_overfetch(items, limit, |a: &AuditRecord| {
            a.id.to_string()
        }))
    }

    async fn audit_chain(
        &self,
        tenant: TenantId,
        after_seq: u64,
        limit: u32,
    ) -> StoreResult<Vec<AuditRecord>> {
        let mut q = QueryBuilder::new("SELECT ");
        q.push(AUDIT_COLS)
            .push(" FROM audit_log WHERE tenant_id = ")
            .push_bind(tenant.0)
            .push(" AND seq > ")
            .push_bind(i64_from(after_seq)?)
            .push(" ORDER BY seq LIMIT ")
            .push_bind(limit_i64(limit));
        let rows = q
            .build_query_as::<AuditRow>()
            .fetch_all(&self.pool)
            .await
            .map_err(backend)?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    async fn audit_head(&self, tenant: TenantId) -> StoreResult<AuditHead> {
        let head: Option<(i64, Vec<u8>)> =
            sqlx::query_as("SELECT seq, hash FROM audit_chain_heads WHERE tenant_id = $1")
                .bind(tenant.0)
                .fetch_optional(&self.pool)
                .await
                .map_err(backend)?;
        let (unchained,): (i64,) =
            sqlx::query_as("SELECT count(*) FROM audit_log WHERE tenant_id = $1 AND seq IS NULL")
                .bind(tenant.0)
                .fetch_one(&self.pool)
                .await
                .map_err(backend)?;
        let (seq, hash) = match head {
            Some((s, h)) => (u64::try_from(s).unwrap_or(0), head_bytes(&h)?),
            None => (0, crate::audit_chain::GENESIS),
        };
        Ok(AuditHead {
            seq,
            hash: crate::audit_chain::hex(&hash),
            unchained: u64::try_from(unchained).unwrap_or(0),
        })
    }

    async fn audit_heads(&self) -> StoreResult<Vec<(TenantId, AuditHead)>> {
        let rows: Vec<(Uuid, i64, Vec<u8>)> = sqlx::query_as(
            "SELECT tenant_id, seq, hash FROM audit_chain_heads ORDER BY tenant_id LIMIT 10000",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?;
        rows.into_iter()
            .map(|(t, s, h)| {
                Ok((
                    TenantId(t),
                    AuditHead {
                        seq: u64::try_from(s).unwrap_or(0),
                        hash: crate::audit_chain::hex(&head_bytes(&h)?),
                        unchained: 0,
                    },
                ))
            })
            .collect()
    }

    async fn create_group(&self, tenant: TenantId, name: &str) -> StoreResult<ScannerGroup> {
        validate_name("group name", name)?;
        let id = GroupId::new();
        sqlx::query("INSERT INTO scanner_groups (id, tenant_id, name) VALUES ($1, $2, $3)")
            .bind(id.0)
            .bind(tenant.0)
            .bind(name)
            .execute(&self.pool)
            .await
            .map_err(backend)?;
        Ok(ScannerGroup {
            id,
            tenant_id: tenant,
            name: name.to_owned(),
            members: vec![],
        })
    }

    async fn add_group_member(
        &self,
        tenant: TenantId,
        group: GroupId,
        scanner: ScannerId,
    ) -> StoreResult<()> {
        let r = sqlx::query(
            "INSERT INTO scanner_group_members (group_id, scanner_id)
             SELECT g.id, s.id FROM scanner_groups g, scanners s
             WHERE g.id = $1 AND s.id = $2 AND g.tenant_id = $3 AND s.tenant_id = $3
             ON CONFLICT DO NOTHING",
        )
        .bind(group.0)
        .bind(scanner.0)
        .bind(tenant.0)
        .execute(&self.pool)
        .await
        .map_err(backend)?;
        if r.rows_affected() == 0 {
            let present: Option<(i32,)> = sqlx::query_as(
                "SELECT 1 FROM scanner_group_members m JOIN scanner_groups g ON g.id = m.group_id
                 WHERE m.group_id = $1 AND m.scanner_id = $2 AND g.tenant_id = $3",
            )
            .bind(group.0)
            .bind(scanner.0)
            .bind(tenant.0)
            .fetch_optional(&self.pool)
            .await
            .map_err(backend)?;
            if present.is_none() {
                return Err(StoreError::NotFound);
            }
        }
        Ok(())
    }

    async fn list_groups(&self, tenant: TenantId) -> StoreResult<Vec<ScannerGroup>> {
        let rows: Vec<(Uuid, String, Vec<Uuid>)> = sqlx::query_as(
            "SELECT g.id, g.name, COALESCE(array_agg(m.scanner_id ORDER BY m.scanner_id)
                 FILTER (WHERE m.scanner_id IS NOT NULL), '{}')
             FROM scanner_groups g LEFT JOIN scanner_group_members m ON m.group_id = g.id
             WHERE g.tenant_id = $1 GROUP BY g.id, g.name ORDER BY g.name",
        )
        .bind(tenant.0)
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?;
        Ok(rows
            .into_iter()
            .map(|(id, name, members)| ScannerGroup {
                id: GroupId(id),
                tenant_id: tenant,
                name,
                members: members.into_iter().map(ScannerId).collect(),
            })
            .collect())
    }

    async fn get_group(&self, tenant: TenantId, group: GroupId) -> StoreResult<ScannerGroup> {
        let row: Option<(String, Vec<Uuid>)> = sqlx::query_as(
            "SELECT g.name, COALESCE(array_agg(m.scanner_id ORDER BY m.scanner_id)
                 FILTER (WHERE m.scanner_id IS NOT NULL), '{}')
             FROM scanner_groups g LEFT JOIN scanner_group_members m ON m.group_id = g.id
             WHERE g.tenant_id = $1 AND g.id = $2 GROUP BY g.id, g.name",
        )
        .bind(tenant.0)
        .bind(group.0)
        .fetch_optional(&self.pool)
        .await
        .map_err(backend)?;
        let (name, members) = row.ok_or(StoreError::NotFound)?;
        Ok(ScannerGroup {
            id: group,
            tenant_id: tenant,
            name,
            members: members.into_iter().map(ScannerId).collect(),
        })
    }

    async fn remove_group_member(
        &self,
        tenant: TenantId,
        group: GroupId,
        scanner: ScannerId,
    ) -> StoreResult<()> {
        let r = sqlx::query(
            "DELETE FROM scanner_group_members m USING scanner_groups g
             WHERE m.group_id = g.id AND g.id = $1 AND g.tenant_id = $2 AND m.scanner_id = $3",
        )
        .bind(group.0)
        .bind(tenant.0)
        .bind(scanner.0)
        .execute(&self.pool)
        .await
        .map_err(backend)?;
        if r.rows_affected() == 0 {
            return Err(StoreError::NotFound);
        }
        Ok(())
    }

    async fn delete_group(&self, tenant: TenantId, group: GroupId) -> StoreResult<()> {
        let r = sqlx::query("DELETE FROM scanner_groups WHERE id = $1 AND tenant_id = $2")
            .bind(group.0)
            .bind(tenant.0)
            .execute(&self.pool)
            .await
            .map_err(backend)?;
        if r.rows_affected() == 0 {
            return Err(StoreError::NotFound);
        }
        Ok(())
    }

    async fn acquire_lease(
        &self,
        tenant: TenantId,
        scanner: ScannerId,
        holder: &str,
        new_id: LeaseId,
        ttl: time::Duration,
        now: OffsetDateTime,
    ) -> StoreResult<LeaseAcquire> {
        let expires_at = now.saturating_add(ttl);
        // One statement: insert, take over an expired lease, or renew our
        // own. A live lease of someone else matches no row.
        let row: Option<LeaseRow> = sqlx::query_as(
            "INSERT INTO scanner_leases AS l (scanner_id, tenant_id, lease_id, holder, acquired_at, expires_at)
             SELECT s.id, s.tenant_id, $3, $4, $5, $6 FROM scanners s WHERE s.id = $1 AND s.tenant_id = $2
             ON CONFLICT (scanner_id) DO UPDATE SET
                 lease_id    = CASE WHEN l.expires_at <= $5 THEN EXCLUDED.lease_id ELSE l.lease_id END,
                 acquired_at = CASE WHEN l.expires_at <= $5 THEN EXCLUDED.acquired_at ELSE l.acquired_at END,
                 holder      = EXCLUDED.holder,
                 expires_at  = EXCLUDED.expires_at
             WHERE l.expires_at <= $5 OR l.holder = EXCLUDED.holder
             RETURNING l.lease_id, l.scanner_id, l.holder, l.acquired_at, l.expires_at",
        )
        .bind(scanner.0)
        .bind(tenant.0)
        .bind(new_id.0)
        .bind(holder)
        .bind(now)
        .bind(expires_at)
        .fetch_optional(&self.pool)
        .await
        .map_err(backend)?;
        if let Some(r) = row {
            return Ok(LeaseAcquire::Granted(r.into()));
        }
        let held = self.current_leases(tenant, &[scanner], now).await?;
        match held.into_iter().next() {
            Some(l) => Ok(LeaseAcquire::Held(l)),
            None => {
                // Either the scanner is not in the tenant, or the holder's
                // lease expired between the two statements: retry once.
                let exists: Option<(Uuid,)> =
                    sqlx::query_as("SELECT id FROM scanners WHERE id = $1 AND tenant_id = $2")
                        .bind(scanner.0)
                        .bind(tenant.0)
                        .fetch_optional(&self.pool)
                        .await
                        .map_err(backend)?;
                if exists.is_none() {
                    return Err(StoreError::NotFound);
                }
                Box::pin(self.acquire_lease(tenant, scanner, holder, new_id, ttl, now)).await
            }
        }
    }

    async fn current_leases(
        &self,
        tenant: TenantId,
        scanners: &[ScannerId],
        now: OffsetDateTime,
    ) -> StoreResult<Vec<LeaseRecord>> {
        let ids: Vec<Uuid> = scanners.iter().map(|s| s.0).collect();
        let rows: Vec<LeaseRow> = sqlx::query_as(
            "SELECT lease_id, scanner_id, holder, acquired_at, expires_at FROM scanner_leases
             WHERE tenant_id = $1 AND scanner_id = ANY($2) AND expires_at > $3",
        )
        .bind(tenant.0)
        .bind(&ids)
        .bind(now)
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    async fn delete_lease(
        &self,
        tenant: TenantId,
        scanner: ScannerId,
        lease: LeaseId,
        now: OffsetDateTime,
    ) -> StoreResult<Option<LeaseRecord>> {
        let row: Option<LeaseRow> = sqlx::query_as(
            "DELETE FROM scanner_leases
             WHERE scanner_id = $1 AND tenant_id = $2 AND lease_id = $3 AND expires_at > $4
             RETURNING lease_id, scanner_id, holder, acquired_at, expires_at",
        )
        .bind(scanner.0)
        .bind(tenant.0)
        .bind(lease.0)
        .bind(now)
        .fetch_optional(&self.pool)
        .await
        .map_err(backend)?;
        Ok(row.map(Into::into))
    }

    async fn purge_expired_leases(&self, now: OffsetDateTime) -> StoreResult<u64> {
        let r = sqlx::query("DELETE FROM scanner_leases WHERE expires_at <= $1")
            .bind(now)
            .execute(&self.pool)
            .await
            .map_err(backend)?;
        Ok(r.rows_affected())
    }

    async fn ping(&self) -> StoreResult<()> {
        sqlx::query("SELECT 1")
            .execute(&self.pool)
            .await
            .map_err(backend)?;
        Ok(())
    }
}
