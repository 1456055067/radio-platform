//! In-memory [`Store`] for tests and single-process development.
//!
//! Semantics match [`crate::PgStore`]; both are checked by the same
//! conformance suite. Data is not persisted.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Mutex;

use async_trait::async_trait;
use platform_common::{
    AgentId, GroupId, LeaseId, ScannerHealth, ScannerId, SiteId, TelemetrySnapshot, TenantId,
    TransmissionId, TransmissionStart, now_utc,
};
use time::OffsetDateTime;

use crate::model::*;
use crate::page::{Page, PageRequest};
use crate::store::{Store, StoreError, StoreResult, validate_name};

#[derive(Debug, Default)]
struct Inner {
    tenants: HashSet<TenantId>,
    sites: BTreeMap<SiteId, Site>,
    agents: BTreeMap<AgentId, Agent>,
    scanners: BTreeMap<ScannerId, ScannerRecord>,
    telemetry: HashMap<ScannerId, TelemetrySnapshot>,
    events: Vec<(EventRecord, uuid::Uuid, u64)>,
    transmissions: HashMap<TransmissionId, TransmissionRecord>,
    audit: Vec<AuditRecord>,
    groups: BTreeMap<GroupId, ScannerGroup>,
    leases: HashMap<ScannerId, LeaseRecord>,
    /// Transcription queue bookkeeping (attempts, next attempt, claim).
    queue: HashMap<TransmissionId, (u32, OffsetDateTime, Option<OffsetDateTime>)>,
    /// Audit chain heads.
    heads: HashMap<TenantId, (u64, [u8; 32])>,
}

/// In-memory store.
#[derive(Debug, Default)]
pub struct MemoryStore {
    inner: Mutex<Inner>,
}

impl MemoryStore {
    /// Creates an empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

fn in_scope(site: SiteId, scope: Option<&[SiteId]>) -> bool {
    scope.is_none_or(|s| s.contains(&site))
}

fn page_by_key<T: Clone>(
    items: impl Iterator<Item = T>,
    page: &PageRequest,
    key: impl Fn(&T) -> String,
    after: impl Fn(&T, &str) -> bool,
) -> StoreResult<Page<T>> {
    let cursor = page.decoded_cursor()?;
    let limit = page.limit();
    let v: Vec<T> = items
        .filter(|i| cursor.as_deref().is_none_or(|c| after(i, c)))
        .take(limit as usize + 1)
        .collect();
    Ok(Page::from_overfetch(v, limit, key))
}

fn tx_key(t: &TransmissionRecord) -> String {
    format!("{}|{}", t.started_at.unix_timestamp_nanos(), t.id)
}

#[async_trait]
impl Store for MemoryStore {
    async fn ensure_tenant(&self, id: TenantId, name: &str) -> StoreResult<()> {
        validate_name("tenant name", name)?;
        self.lock().tenants.insert(id);
        Ok(())
    }

    async fn create_site(&self, s: NewSite) -> StoreResult<Site> {
        validate_name("site name", &s.name)?;
        let mut g = self.lock();
        if !g.tenants.contains(&s.tenant_id) {
            return Err(StoreError::Conflict("unknown tenant".into()));
        }
        if g.sites
            .values()
            .any(|x| x.tenant_id == s.tenant_id && x.name == s.name)
        {
            return Err(StoreError::Conflict(format!(
                "site `{}` already exists",
                s.name
            )));
        }
        let site = Site {
            id: SiteId::new(),
            tenant_id: s.tenant_id,
            name: s.name,
            description: s.description,
            latitude: s.latitude,
            longitude: s.longitude,
            allowed_networks: s.allowed_networks,
            created_at: now_utc(),
        };
        g.sites.insert(site.id, site.clone());
        Ok(site)
    }

    async fn get_site(&self, tenant: TenantId, id: SiteId) -> StoreResult<Site> {
        self.lock()
            .sites
            .get(&id)
            .filter(|s| s.tenant_id == tenant)
            .cloned()
            .ok_or(StoreError::NotFound)
    }

    async fn list_sites(
        &self,
        tenant: TenantId,
        scope: Option<&[SiteId]>,
        page: &PageRequest,
    ) -> StoreResult<Page<Site>> {
        let g = self.lock();
        page_by_key(
            g.sites
                .values()
                .filter(|s| s.tenant_id == tenant && in_scope(s.id, scope))
                .cloned(),
            page,
            |s| s.id.to_string(),
            |s, c| s.id.to_string().as_str() > c,
        )
    }

    async fn list_agents(
        &self,
        tenant: TenantId,
        scope: Option<&[SiteId]>,
        page: &PageRequest,
    ) -> StoreResult<Page<Agent>> {
        let g = self.lock();
        page_by_key(
            g.agents
                .values()
                .filter(|a| a.tenant_id == tenant && in_scope(a.site_id, scope))
                .cloned(),
            page,
            |a| a.id.as_str().to_owned(),
            |a, c| a.id.as_str() > c,
        )
    }

    async fn upsert_agent(&self, a: Agent) -> StoreResult<Agent> {
        let mut g = self.lock();
        if g.sites
            .get(&a.site_id)
            .is_none_or(|s| s.tenant_id != a.tenant_id)
        {
            return Err(StoreError::Conflict(
                "agent site not found in tenant".into(),
            ));
        }
        g.agents.insert(a.id.clone(), a.clone());
        Ok(a)
    }

    async fn get_agent(&self, id: &AgentId) -> StoreResult<Agent> {
        self.lock()
            .agents
            .get(id)
            .cloned()
            .ok_or(StoreError::NotFound)
    }

    async fn touch_agent(&self, id: &AgentId, at: OffsetDateTime) -> StoreResult<()> {
        let mut g = self.lock();
        let a = g.agents.get_mut(id).ok_or(StoreError::NotFound)?;
        a.last_seen_at = Some(at);
        Ok(())
    }

    async fn create_scanner(&self, s: NewScanner) -> StoreResult<ScannerRecord> {
        validate_name("scanner name", &s.name)?;
        if s.port == 0 {
            return Err(StoreError::Invalid("port must be 1-65535".into()));
        }
        let mut g = self.lock();
        if g.sites
            .get(&s.site_id)
            .is_none_or(|x| x.tenant_id != s.tenant_id)
        {
            return Err(StoreError::Conflict("site not found in tenant".into()));
        }
        if g.agents
            .get(&s.agent_id)
            .is_none_or(|a| a.tenant_id != s.tenant_id)
        {
            return Err(StoreError::Conflict("agent not found in tenant".into()));
        }
        if g.scanners
            .values()
            .any(|x| x.agent_id == s.agent_id && x.host == s.host && x.port == s.port)
        {
            return Err(StoreError::Conflict(
                "scanner endpoint already enrolled".into(),
            ));
        }
        let now = now_utc();
        let rec = ScannerRecord {
            id: ScannerId::new(),
            tenant_id: s.tenant_id,
            site_id: s.site_id,
            agent_id: s.agent_id,
            name: s.name,
            host: s.host,
            port: s.port,
            enabled: s.enabled,
            model: None,
            firmware: None,
            health: None,
            audio_enabled: false,
            recording_enabled: false,
            transcription_enabled: false,
            rtsp_port: 554,
            created_at: now,
            updated_at: now,
        };
        g.scanners.insert(rec.id, rec.clone());
        Ok(rec)
    }

    async fn get_scanner(&self, tenant: TenantId, id: ScannerId) -> StoreResult<ScannerRecord> {
        self.lock()
            .scanners
            .get(&id)
            .filter(|s| s.tenant_id == tenant)
            .cloned()
            .ok_or(StoreError::NotFound)
    }

    async fn list_scanners(
        &self,
        tenant: TenantId,
        f: &ScannerFilter,
        page: &PageRequest,
    ) -> StoreResult<Page<ScannerRecord>> {
        let g = self.lock();
        page_by_key(
            g.scanners
                .values()
                .filter(|s| {
                    s.tenant_id == tenant
                        && f.site_id.is_none_or(|x| x == s.site_id)
                        && in_scope(s.site_id, f.site_ids.as_deref())
                        && f.agent_id.as_ref().is_none_or(|a| *a == s.agent_id)
                })
                .cloned(),
            page,
            |s| s.id.to_string(),
            |s, c| s.id.to_string().as_str() > c,
        )
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
        let mut g = self.lock();
        let current = g
            .scanners
            .get(&id)
            .filter(|s| s.tenant_id == tenant)
            .cloned()
            .ok_or(StoreError::NotFound)?;
        let host = p.host.unwrap_or(current.host);
        let port = p.port.unwrap_or(current.port);
        if g.scanners.values().any(|x| {
            x.id != id && x.agent_id == current.agent_id && x.host == host && x.port == port
        }) {
            return Err(StoreError::Conflict(
                "scanner endpoint already enrolled".into(),
            ));
        }
        let s = g.scanners.get_mut(&id).ok_or(StoreError::NotFound)?;
        if let Some(n) = p.name {
            s.name = n;
        }
        if let Some(e) = p.enabled {
            s.enabled = e;
        }
        if let Some(a) = p.audio_enabled {
            s.audio_enabled = a;
        }
        if let Some(r) = p.recording_enabled {
            s.recording_enabled = r;
        }
        if let Some(r) = p.rtsp_port {
            s.rtsp_port = r;
        }
        if let Some(t) = p.transcription_enabled {
            s.transcription_enabled = t;
        }
        if !s.audio_enabled {
            s.recording_enabled = false;
        }
        if !s.recording_enabled {
            s.transcription_enabled = false;
        }
        s.host = host;
        s.port = port;
        s.updated_at = now_utc();
        Ok(s.clone())
    }

    async fn delete_scanner(&self, tenant: TenantId, id: ScannerId) -> StoreResult<()> {
        let mut g = self.lock();
        if g.scanners.get(&id).is_none_or(|s| s.tenant_id != tenant) {
            return Err(StoreError::NotFound);
        }
        g.scanners.remove(&id);
        g.telemetry.remove(&id);
        g.leases.remove(&id);
        g.transmissions.retain(|_, t| t.scanner_id != id);
        for grp in g.groups.values_mut() {
            grp.members.retain(|m| *m != id);
        }
        Ok(())
    }

    async fn scanners_for_agent(&self, agent: &AgentId) -> StoreResult<Vec<ScannerRecord>> {
        Ok(self
            .lock()
            .scanners
            .values()
            .filter(|s| s.agent_id == *agent)
            .cloned()
            .collect())
    }

    async fn set_scanner_status(
        &self,
        id: ScannerId,
        model: Option<String>,
        firmware: Option<String>,
        health: Option<ScannerHealth>,
    ) -> StoreResult<()> {
        let mut g = self.lock();
        let s = g.scanners.get_mut(&id).ok_or(StoreError::NotFound)?;
        if model.is_some() {
            s.model = model;
        }
        if firmware.is_some() {
            s.firmware = firmware;
        }
        if health.is_some() {
            s.health = health;
        }
        s.updated_at = now_utc();
        Ok(())
    }

    async fn put_telemetry(&self, scanner: ScannerId, snap: &TelemetrySnapshot) -> StoreResult<()> {
        let mut g = self.lock();
        if !g.scanners.contains_key(&scanner) {
            return Err(StoreError::NotFound);
        }
        g.telemetry.insert(scanner, snap.clone());
        Ok(())
    }

    async fn get_telemetry(
        &self,
        tenant: TenantId,
        scanner: ScannerId,
    ) -> StoreResult<Option<TelemetrySnapshot>> {
        let g = self.lock();
        if g.scanners
            .get(&scanner)
            .is_none_or(|s| s.tenant_id != tenant)
        {
            return Err(StoreError::NotFound);
        }
        Ok(g.telemetry.get(&scanner).cloned())
    }

    async fn append_event(&self, e: NewEvent) -> StoreResult<Option<i64>> {
        if e.agent_seq == 0 {
            return Err(StoreError::Invalid("agent_seq must be >= 1".into()));
        }
        let mut g = self.lock();
        if g.events
            .iter()
            .any(|(r, s, q)| r.agent_id == e.agent_id && *s == e.agent_session && *q == e.agent_seq)
        {
            return Ok(None);
        }
        let id = g.events.last().map_or(1, |(r, _, _)| r.id + 1);
        g.events.push((
            EventRecord {
                id,
                tenant_id: e.tenant_id,
                site_id: e.site_id,
                scanner_id: e.scanner_id,
                agent_id: e.agent_id,
                kind: e.kind,
                payload: e.payload,
                received_at: now_utc(),
            },
            e.agent_session,
            e.agent_seq,
        ));
        Ok(Some(id))
    }

    async fn list_events(
        &self,
        tenant: TenantId,
        f: &EventFilter,
        page: &PageRequest,
    ) -> StoreResult<Page<EventRecord>> {
        let g = self.lock();
        let cursor: Option<i64> = page
            .decoded_cursor()?
            .map(|c| {
                c.parse()
                    .map_err(|_| StoreError::Invalid("malformed cursor".into()))
            })
            .transpose()?;
        let limit = page.limit();
        let v: Vec<EventRecord> = g
            .events
            .iter()
            .rev()
            .map(|(r, _, _)| r)
            .filter(|r| {
                r.tenant_id == tenant
                    && cursor.is_none_or(|c| r.id < c)
                    && f.scanner_id.is_none_or(|s| s == r.scanner_id)
                    && in_scope(r.site_id, f.site_ids.as_deref())
                    && f.kind.as_ref().is_none_or(|k| *k == r.kind)
            })
            .take(limit as usize + 1)
            .cloned()
            .collect();
        Ok(Page::from_overfetch(v, limit, |r| r.id.to_string()))
    }

    async fn start_transmission(
        &self,
        tenant: TenantId,
        site: SiteId,
        s: &TransmissionStart,
    ) -> StoreResult<()> {
        let mut g = self.lock();
        if g.scanners
            .get(&s.scanner_id)
            .is_none_or(|x| x.tenant_id != tenant)
        {
            return Err(StoreError::NotFound);
        }
        g.transmissions
            .entry(s.transmission_id)
            .or_insert_with(|| TransmissionRecord {
                id: s.transmission_id,
                tenant_id: tenant,
                site_id: site,
                scanner_id: s.scanner_id,
                started_at: s.started_at,
                ended_at: None,
                duration_ms: None,
                frequency_hz: s.frequency_hz,
                tgid: s.tgid.clone(),
                unit_id: s.unit_id.clone(),
                system_name: s.system.clone(),
                department_name: s.department.clone(),
                channel_name: s.channel.clone(),
                modulation: s.modulation.clone(),
                signal_bars: s.signal_bars,
                rssi: s.rssi,
                audio_object: None,
                transcript: None,
                transcription_state: None,
                transcription_error: None,
                transcript_language: None,
                transcript_model: None,
                transcribed_at: None,
                hold: false,
            });
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
        let mut g = self.lock();
        let t = g
            .transmissions
            .get_mut(&id)
            .filter(|t| t.tenant_id == tenant)
            .ok_or(StoreError::NotFound)?;
        t.audio_object = Some(object_key.to_owned());
        if transcribe {
            t.transcription_state = Some(TranscriptionState::Pending);
            t.transcription_error = None;
            g.queue.insert(id, (0, now, None));
        }
        Ok(())
    }

    async fn end_transmission(
        &self,
        id: TransmissionId,
        ended_at: OffsetDateTime,
        duration_ms: u64,
    ) -> StoreResult<()> {
        let mut g = self.lock();
        let t = g.transmissions.get_mut(&id).ok_or(StoreError::NotFound)?;
        t.ended_at = Some(ended_at);
        t.duration_ms = Some(duration_ms);
        Ok(())
    }

    async fn get_transmission(
        &self,
        tenant: TenantId,
        id: TransmissionId,
    ) -> StoreResult<TransmissionRecord> {
        self.lock()
            .transmissions
            .get(&id)
            .filter(|t| t.tenant_id == tenant)
            .cloned()
            .ok_or(StoreError::NotFound)
    }

    async fn list_transmissions(
        &self,
        tenant: TenantId,
        f: &TransmissionFilter,
        page: &PageRequest,
    ) -> StoreResult<Page<TransmissionRecord>> {
        let g = self.lock();
        let mut v: Vec<TransmissionRecord> = g
            .transmissions
            .values()
            .filter(|t| {
                t.tenant_id == tenant
                    && f.scanner_id.is_none_or(|s| s == t.scanner_id)
                    && f.site_id.is_none_or(|s| s == t.site_id)
                    && in_scope(t.site_id, f.site_ids.as_deref())
                    && f.frequency_hz.is_none_or(|x| t.frequency_hz == Some(x))
                    && f.tgid.as_ref().is_none_or(|x| t.tgid.as_ref() == Some(x))
                    && f.from.is_none_or(|x| t.started_at >= x)
                    && f.to.is_none_or(|x| t.started_at < x)
                    && f.recorded.is_none_or(|r| t.audio_object.is_some() == r)
                    && f.transcription_state
                        .is_none_or(|s| t.transcription_state == Some(s))
                    && f.text
                        .as_deref()
                        .is_none_or(|q| text_matches(t.transcript.as_deref(), q))
            })
            .cloned()
            .collect();
        v.sort_by(|a, b| b.started_at.cmp(&a.started_at).then(b.id.cmp(&a.id)));
        let cursor = page.decoded_cursor()?;
        let parsed = cursor.as_deref().map(parse_tx_cursor).transpose()?;
        let limit = page.limit();
        let items: Vec<_> = v
            .into_iter()
            .filter(|t| {
                parsed.is_none_or(|(n, id)| (t.started_at.unix_timestamp_nanos(), t.id.0) < (n, id))
            })
            .take(limit as usize + 1)
            .collect();
        Ok(Page::from_overfetch(items, limit, tx_key))
    }

    async fn request_transcription(
        &self,
        tenant: TenantId,
        id: TransmissionId,
        now: OffsetDateTime,
    ) -> StoreResult<()> {
        let mut g = self.lock();
        let t = g
            .transmissions
            .get_mut(&id)
            .filter(|t| t.tenant_id == tenant)
            .ok_or(StoreError::NotFound)?;
        if t.audio_object.is_none() {
            return Err(StoreError::Conflict("transmission has no recording".into()));
        }
        t.transcription_state = Some(TranscriptionState::Pending);
        t.transcription_error = None;
        g.queue.insert(id, (0, now, None));
        Ok(())
    }

    async fn claim_transcriptions(
        &self,
        now: OffsetDateTime,
        lease_until: OffsetDateTime,
        limit: u32,
    ) -> StoreResult<Vec<TranscriptionJob>> {
        let mut g = self.lock();
        let mut due: Vec<(OffsetDateTime, TransmissionId)> = g
            .queue
            .iter()
            .filter(|(id, (_, next, lock))| {
                *next <= now
                    && lock.is_none_or(|l| l < now)
                    && g.transmissions.get(id).is_some_and(|t| {
                        t.transcription_state == Some(TranscriptionState::Pending)
                            && t.audio_object.is_some()
                    })
            })
            .map(|(id, (_, next, _))| (*next, *id))
            .collect();
        due.sort();
        due.truncate(limit as usize);
        let mut out = Vec::new();
        for (_, id) in due {
            let Some(q) = g.queue.get_mut(&id) else {
                continue;
            };
            q.0 += 1;
            q.2 = Some(lease_until);
            let attempts = q.0;
            if let Some(t) = g.transmissions.get(&id)
                && let Some(key) = &t.audio_object
            {
                out.push(TranscriptionJob {
                    tenant_id: t.tenant_id,
                    transmission_id: id,
                    scanner_id: t.scanner_id,
                    audio_object: key.clone(),
                    duration_ms: t.duration_ms,
                    attempts,
                });
            }
        }
        Ok(out)
    }

    async fn finish_transcription(
        &self,
        tenant: TenantId,
        id: TransmissionId,
        result: TranscriptionResult,
        now: OffsetDateTime,
    ) -> StoreResult<()> {
        let mut g = self.lock();
        let t = g
            .transmissions
            .get_mut(&id)
            .filter(|t| t.tenant_id == tenant)
            .ok_or(StoreError::NotFound)?;
        let mut requeue = None;
        match result {
            TranscriptionResult::Done {
                text,
                language,
                model,
            } => {
                t.transcription_state = Some(TranscriptionState::Done);
                t.transcript = Some(text);
                t.transcript_language = language;
                t.transcript_model = model;
                t.transcribed_at = Some(now);
                t.transcription_error = None;
            }
            TranscriptionResult::Retry { error, at } => {
                t.transcription_state = Some(TranscriptionState::Pending);
                t.transcription_error = Some(error);
                requeue = Some(at);
            }
            TranscriptionResult::Failed { error } => {
                t.transcription_state = Some(TranscriptionState::Failed);
                t.transcription_error = Some(error);
            }
            TranscriptionResult::Skipped { reason } => {
                t.transcription_state = Some(TranscriptionState::Skipped);
                t.transcription_error = Some(reason);
            }
        }
        match requeue {
            Some(at) => {
                if let Some(q) = g.queue.get_mut(&id) {
                    q.1 = at;
                    q.2 = None;
                }
            }
            None => {
                g.queue.remove(&id);
            }
        }
        Ok(())
    }

    async fn transcription_stats(
        &self,
        tenant: TenantId,
        since: OffsetDateTime,
    ) -> StoreResult<TranscriptionStats> {
        let g = self.lock();
        let mut s = TranscriptionStats::default();
        for t in g.transmissions.values().filter(|t| t.tenant_id == tenant) {
            match t.transcription_state {
                Some(TranscriptionState::Pending) => s.pending += 1,
                Some(TranscriptionState::Failed) if t.started_at >= since => s.failed += 1,
                Some(TranscriptionState::Done) if t.started_at >= since => s.done += 1,
                _ => {}
            }
        }
        Ok(s)
    }

    async fn set_transmission_hold(
        &self,
        tenant: TenantId,
        id: TransmissionId,
        hold: bool,
    ) -> StoreResult<TransmissionRecord> {
        let mut g = self.lock();
        let t = g
            .transmissions
            .get_mut(&id)
            .filter(|t| t.tenant_id == tenant)
            .ok_or(StoreError::NotFound)?;
        t.hold = hold;
        Ok(t.clone())
    }

    async fn expired_recordings(
        &self,
        before: OffsetDateTime,
        limit: u32,
    ) -> StoreResult<Vec<ExpiredRecording>> {
        let g = self.lock();
        let mut v: Vec<&TransmissionRecord> = g
            .transmissions
            .values()
            .filter(|t| !t.hold && t.audio_object.is_some() && t.started_at < before)
            .collect();
        v.sort_by_key(|t| (t.started_at, t.id));
        Ok(v.into_iter()
            .take(limit as usize)
            .filter_map(|t| {
                Some(ExpiredRecording {
                    tenant_id: t.tenant_id,
                    transmission_id: t.id,
                    audio_object: t.audio_object.clone()?,
                })
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
        let mut g = self.lock();
        let Some(t) = g
            .transmissions
            .get_mut(&id)
            .filter(|t| t.tenant_id == tenant)
        else {
            return Ok(false);
        };
        if t.hold || t.audio_object.as_deref() != Some(object_key) {
            return Ok(false);
        }
        t.audio_object = None;
        if t.transcription_state == Some(TranscriptionState::Pending) {
            t.transcription_state = Some(TranscriptionState::Failed);
            t.transcription_error = Some("recording deleted".into());
        }
        if with_transcript {
            t.transcript = None;
            t.transcript_language = None;
            t.transcript_model = None;
            t.transcribed_at = None;
            t.transcription_state = None;
            t.transcription_error = None;
        }
        g.queue.remove(&id);
        Ok(true)
    }

    async fn expire_transcripts(
        &self,
        before: OffsetDateTime,
        limit: u32,
    ) -> StoreResult<TenantCounts> {
        let mut g = self.lock();
        let mut out = TenantCounts::new();
        for t in g
            .transmissions
            .values_mut()
            .filter(|t| !t.hold && t.transcript.is_some() && t.started_at < before)
            .take(limit as usize)
        {
            t.transcript = None;
            t.transcript_language = None;
            t.transcription_error = Some("transcript removed by retention policy".into());
            add_count(&mut out, t.tenant_id);
        }
        Ok(out)
    }

    async fn delete_transmissions_before(
        &self,
        before: OffsetDateTime,
        limit: u32,
    ) -> StoreResult<TenantCounts> {
        let mut g = self.lock();
        let doomed: Vec<(TransmissionId, TenantId)> = g
            .transmissions
            .values()
            .filter(|t| !t.hold && t.audio_object.is_none() && t.started_at < before)
            .take(limit as usize)
            .map(|t| (t.id, t.tenant_id))
            .collect();
        let mut out = TenantCounts::new();
        for (id, tenant) in doomed {
            g.transmissions.remove(&id);
            g.queue.remove(&id);
            add_count(&mut out, tenant);
        }
        Ok(out)
    }

    async fn delete_events_before(
        &self,
        before: OffsetDateTime,
        limit: u32,
    ) -> StoreResult<TenantCounts> {
        let mut g = self.lock();
        let mut out = TenantCounts::new();
        let mut removed = 0u32;
        g.events.retain(|(e, _, _)| {
            if removed < limit && e.received_at < before {
                removed += 1;
                add_count(&mut out, e.tenant_id);
                false
            } else {
                true
            }
        });
        Ok(out)
    }

    async fn transmission_activity(
        &self,
        tenant: TenantId,
        f: &AnalyticsFilter,
        bucket: Bucket,
    ) -> StoreResult<Vec<ActivityPoint>> {
        let g = self.lock();
        let mut m: BTreeMap<OffsetDateTime, (u64, u64)> = BTreeMap::new();
        for t in g
            .transmissions
            .values()
            .filter(|t| analytics_match(t, tenant, f))
        {
            let e = m.entry(bucket.floor(t.started_at)).or_default();
            e.0 += 1;
            e.1 += t.duration_ms.unwrap_or(0);
        }
        Ok(m.into_iter()
            .map(|(start, (n, a))| ActivityPoint {
                start,
                transmissions: n,
                airtime_ms: a,
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
        let g = self.lock();
        let mut m: HashMap<String, (u64, u64)> = HashMap::new();
        for t in g
            .transmissions
            .values()
            .filter(|t| analytics_match(t, tenant, f))
        {
            let key = match by {
                TopBy::Talkgroup => t.tgid.clone(),
                TopBy::Channel => t.channel_name.clone(),
                TopBy::Frequency => t.frequency_hz.map(|f| f.to_string()),
                TopBy::Scanner => Some(t.scanner_id.to_string()),
                TopBy::Unit => t.unit_id.clone(),
            };
            if let Some(k) = key {
                let e = m.entry(k).or_default();
                e.0 += 1;
                e.1 += t.duration_ms.unwrap_or(0);
            }
        }
        let mut v: Vec<TopEntry> = m
            .into_iter()
            .map(|(key, (n, a))| TopEntry {
                key,
                transmissions: n,
                airtime_ms: a,
            })
            .collect();
        v.sort_by(|a, b| {
            b.transmissions
                .cmp(&a.transmissions)
                .then(a.key.cmp(&b.key))
        });
        v.truncate(limit as usize);
        Ok(v)
    }

    async fn append_audit(&self, r: NewAudit) -> StoreResult<i64> {
        let mut g = self.lock();
        let id = g.audit.last().map_or(1, |a| a.id + 1);
        let (seq, prev) = g
            .heads
            .get(&r.tenant_id)
            .copied()
            .unwrap_or((0, crate::audit_chain::GENESIS));
        let seq = seq + 1;
        let mut rec = AuditRecord {
            id,
            at: crate::audit_chain::micros(now_utc()),
            tenant_id: r.tenant_id,
            actor: r.actor,
            action: r.action,
            resource_type: r.resource_type,
            resource_id: r.resource_id,
            outcome: r.outcome,
            details: r.details,
            source_ip: r.source_ip,
            seq: Some(seq),
            prev_hash: Some(crate::audit_chain::hex(&prev)),
            hash: None,
        };
        let h = crate::audit_chain::link(&prev, &rec, seq);
        rec.hash = Some(crate::audit_chain::hex(&h));
        g.heads.insert(rec.tenant_id, (seq, h));
        g.audit.push(rec);
        Ok(id)
    }

    async fn list_audit(
        &self,
        tenant: TenantId,
        f: &AuditFilter,
        page: &PageRequest,
    ) -> StoreResult<Page<AuditRecord>> {
        let g = self.lock();
        let cursor: Option<i64> = page
            .decoded_cursor()?
            .map(|c| {
                c.parse()
                    .map_err(|_| StoreError::Invalid("malformed cursor".into()))
            })
            .transpose()?;
        let limit = page.limit();
        let v: Vec<_> = g
            .audit
            .iter()
            .rev()
            .filter(|a| a.tenant_id == tenant && cursor.is_none_or(|c| a.id < c) && f.matches(a))
            .take(limit as usize + 1)
            .cloned()
            .collect();
        Ok(Page::from_overfetch(v, limit, |a| a.id.to_string()))
    }

    async fn audit_chain(
        &self,
        tenant: TenantId,
        after_seq: u64,
        limit: u32,
    ) -> StoreResult<Vec<AuditRecord>> {
        let g = self.lock();
        let mut v: Vec<AuditRecord> = g
            .audit
            .iter()
            .filter(|a| a.tenant_id == tenant && a.seq.is_some_and(|s| s > after_seq))
            .cloned()
            .collect();
        v.sort_by_key(|a| a.seq);
        v.truncate(limit as usize);
        Ok(v)
    }

    async fn audit_head(&self, tenant: TenantId) -> StoreResult<AuditHead> {
        let g = self.lock();
        let (seq, h) = g
            .heads
            .get(&tenant)
            .copied()
            .unwrap_or((0, crate::audit_chain::GENESIS));
        let unchained = g
            .audit
            .iter()
            .filter(|a| a.tenant_id == tenant && a.seq.is_none())
            .count() as u64;
        Ok(AuditHead {
            seq,
            hash: crate::audit_chain::hex(&h),
            unchained,
        })
    }

    async fn audit_heads(&self) -> StoreResult<Vec<(TenantId, AuditHead)>> {
        let g = self.lock();
        Ok(g.heads
            .iter()
            .map(|(t, (seq, h))| {
                (
                    *t,
                    AuditHead {
                        seq: *seq,
                        hash: crate::audit_chain::hex(h),
                        unchained: 0,
                    },
                )
            })
            .collect())
    }

    async fn create_group(&self, tenant: TenantId, name: &str) -> StoreResult<ScannerGroup> {
        validate_name("group name", name)?;
        let mut g = self.lock();
        if g.groups
            .values()
            .any(|x| x.tenant_id == tenant && x.name == name)
        {
            return Err(StoreError::Conflict(format!(
                "group `{name}` already exists"
            )));
        }
        let grp = ScannerGroup {
            id: GroupId::new(),
            tenant_id: tenant,
            name: name.to_owned(),
            members: vec![],
        };
        g.groups.insert(grp.id, grp.clone());
        Ok(grp)
    }

    async fn add_group_member(
        &self,
        tenant: TenantId,
        group: GroupId,
        scanner: ScannerId,
    ) -> StoreResult<()> {
        let mut g = self.lock();
        if g.scanners
            .get(&scanner)
            .is_none_or(|s| s.tenant_id != tenant)
        {
            return Err(StoreError::NotFound);
        }
        let grp = g
            .groups
            .get_mut(&group)
            .filter(|x| x.tenant_id == tenant)
            .ok_or(StoreError::NotFound)?;
        if !grp.members.contains(&scanner) {
            grp.members.push(scanner);
            grp.members.sort();
        }
        Ok(())
    }

    async fn get_group(&self, tenant: TenantId, group: GroupId) -> StoreResult<ScannerGroup> {
        self.lock()
            .groups
            .get(&group)
            .filter(|g| g.tenant_id == tenant)
            .cloned()
            .ok_or(StoreError::NotFound)
    }

    async fn remove_group_member(
        &self,
        tenant: TenantId,
        group: GroupId,
        scanner: ScannerId,
    ) -> StoreResult<()> {
        let mut g = self.lock();
        let grp = g
            .groups
            .get_mut(&group)
            .filter(|x| x.tenant_id == tenant)
            .ok_or(StoreError::NotFound)?;
        let before = grp.members.len();
        grp.members.retain(|m| *m != scanner);
        if grp.members.len() == before {
            return Err(StoreError::NotFound);
        }
        Ok(())
    }

    async fn delete_group(&self, tenant: TenantId, group: GroupId) -> StoreResult<()> {
        let mut g = self.lock();
        if g.groups.get(&group).is_none_or(|x| x.tenant_id != tenant) {
            return Err(StoreError::NotFound);
        }
        g.groups.remove(&group);
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
        let mut g = self.lock();
        if g.scanners
            .get(&scanner)
            .is_none_or(|s| s.tenant_id != tenant)
        {
            return Err(StoreError::NotFound);
        }
        let expires_at = now.saturating_add(ttl);
        match g.leases.get_mut(&scanner) {
            Some(l) if l.expires_at > now && l.holder != holder => {
                Ok(LeaseAcquire::Held(l.clone()))
            }
            Some(l) if l.expires_at > now => {
                l.expires_at = expires_at;
                Ok(LeaseAcquire::Granted(l.clone()))
            }
            _ => {
                let l = LeaseRecord {
                    id: new_id,
                    scanner_id: scanner,
                    holder: holder.to_owned(),
                    acquired_at: now,
                    expires_at,
                };
                g.leases.insert(scanner, l.clone());
                Ok(LeaseAcquire::Granted(l))
            }
        }
    }

    async fn current_leases(
        &self,
        tenant: TenantId,
        scanners: &[ScannerId],
        now: OffsetDateTime,
    ) -> StoreResult<Vec<LeaseRecord>> {
        let g = self.lock();
        Ok(scanners
            .iter()
            .filter(|s| g.scanners.get(s).is_some_and(|r| r.tenant_id == tenant))
            .filter_map(|s| g.leases.get(s))
            .filter(|l| l.expires_at > now)
            .cloned()
            .collect())
    }

    async fn delete_lease(
        &self,
        tenant: TenantId,
        scanner: ScannerId,
        lease: LeaseId,
        now: OffsetDateTime,
    ) -> StoreResult<Option<LeaseRecord>> {
        let mut g = self.lock();
        if g.scanners
            .get(&scanner)
            .is_none_or(|s| s.tenant_id != tenant)
        {
            return Ok(None);
        }
        if g.leases
            .get(&scanner)
            .is_some_and(|l| l.id == lease && l.expires_at > now)
        {
            return Ok(g.leases.remove(&scanner));
        }
        Ok(None)
    }

    async fn purge_expired_leases(&self, now: OffsetDateTime) -> StoreResult<u64> {
        let mut g = self.lock();
        let before = g.leases.len();
        g.leases.retain(|_, l| l.expires_at > now);
        Ok((before - g.leases.len()) as u64)
    }

    async fn list_groups(&self, tenant: TenantId) -> StoreResult<Vec<ScannerGroup>> {
        let mut v: Vec<_> = self
            .lock()
            .groups
            .values()
            .filter(|x| x.tenant_id == tenant)
            .cloned()
            .collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(v)
    }

    async fn ping(&self) -> StoreResult<()> {
        Ok(())
    }
}

/// Lower-case alphanumeric words.
fn words(s: &str) -> impl Iterator<Item = String> + '_ {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
}

/// Approximates `websearch_to_tsquery('simple', q)`: every word of the
/// query must appear as a word of the transcript.
fn text_matches(transcript: Option<&str>, q: &str) -> bool {
    let Some(t) = transcript else { return false };
    let have: HashSet<String> = words(t).collect();
    let mut want = words(q).peekable();
    want.peek().is_some() && want.all(|w| have.contains(&w))
}

fn add_count(v: &mut TenantCounts, tenant: TenantId) {
    match v.iter_mut().find(|(t, _)| *t == tenant) {
        Some((_, n)) => *n += 1,
        None => v.push((tenant, 1)),
    }
}

fn analytics_match(t: &TransmissionRecord, tenant: TenantId, f: &AnalyticsFilter) -> bool {
    t.tenant_id == tenant
        && t.started_at >= f.from
        && t.started_at < f.to
        && f.site_id.is_none_or(|s| s == t.site_id)
        && in_scope(t.site_id, f.site_ids.as_deref())
        && f.scanner_id.is_none_or(|s| s == t.scanner_id)
}

pub(crate) fn parse_tx_cursor(c: &str) -> StoreResult<(i128, uuid::Uuid)> {
    let bad = || StoreError::Invalid("malformed cursor".into());
    let (n, id) = c.split_once('|').ok_or_else(bad)?;
    Ok((
        n.parse().map_err(|_| bad())?,
        uuid::Uuid::parse_str(id).map_err(|_| bad())?,
    ))
}
