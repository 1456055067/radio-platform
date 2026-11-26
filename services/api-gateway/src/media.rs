//! Gateway side of audio: the per-replica [`MediaHub`], optional WebRTC
//! listening, and optional transmission recording.
//!
//! Listening sessions are created on the replica that holds the scanner's
//! agent (other replicas forward the WHEP offer over the cluster bus), so
//! listener demand and the audio itself are always local to that replica.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use media_gateway::{MediaHub, SessionId, WebRtcServer};
use platform_common::wire::media::MediaFrame;
use platform_common::{ScannerId, SiteId, TenantId, TransmissionId};
use platform_storage::Store;
use scanner_recorder::{RecordingStore, SegmentPolicy, Segmenter};
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::hub::AgentHub;

/// Why a listening session could not be created.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ListenError {
    /// Live audio is not configured on the replica holding the agent.
    Disabled,
    /// The agent is not connected to any replica.
    AgentOffline,
    /// The offer was rejected.
    Offer {
        /// Reason.
        message: String,
    },
    /// Too many sessions (replica or user).
    Busy {
        /// Reason.
        message: String,
    },
}

/// A listening session's owner, for authorization of hang-ups and limits.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SessionMeta {
    /// Subject that created it.
    pub subject: String,
    /// Tenant.
    pub tenant_id: TenantId,
    /// Scanner.
    pub scanner_id: ScannerId,
}

/// Recording events from agent sessions.
#[derive(Debug)]
pub enum RecordEvent {
    /// A transmission started on a recording scanner.
    Start {
        /// Tenant.
        tenant_id: TenantId,
        /// Site.
        site_id: SiteId,
        /// Scanner.
        scanner_id: ScannerId,
        /// Transmission.
        transmission_id: TransmissionId,
        /// Start (agent clock).
        started_at: time::OffsetDateTime,
        /// Queue the recording for transcription.
        transcribe: bool,
    },
    /// A transmission ended.
    End {
        /// Transmission.
        transmission_id: TransmissionId,
        /// End (agent clock).
        ended_at: time::OffsetDateTime,
    },
}

/// Recorder handle.
#[derive(Debug, Clone)]
pub struct Recorder {
    /// Storage (also used for playback).
    pub store: RecordingStore,
    events: mpsc::Sender<RecordEvent>,
    stats: Arc<Mutex<RecorderStats>>,
}

/// Recorder counters.
#[derive(Debug, Clone, Copy, Default, serde::Serialize, utoipa::ToSchema)]
pub struct RecorderStats {
    /// Recordings stored.
    pub stored: u64,
    /// Recordings that failed to store.
    pub failed: u64,
    /// Recordings in progress.
    pub in_progress: u64,
}

impl Recorder {
    /// Delivers a recording event (dropped if the recorder is overloaded).
    pub fn event(&self, e: RecordEvent) {
        if self.events.try_send(e).is_err() {
            warn!("recorder busy; recording event dropped");
        }
    }

    /// Counters.
    #[must_use]
    pub fn stats(&self) -> RecorderStats {
        *self
            .stats
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Audio state of a gateway replica.
#[derive(Debug)]
pub struct Media {
    /// Frame fan-out.
    pub hub: MediaHub,
    webrtc: OnceLock<WebRtcServer>,
    recorder: OnceLock<Recorder>,
    /// Listening sessions created on this replica.
    pub sessions: Mutex<HashMap<SessionId, SessionMeta>>,
    /// Per-subject session limit.
    pub per_subject: std::sync::atomic::AtomicUsize,
    transcription: OnceLock<Arc<crate::transcription::Transcription>>,
    /// Wakes the transcription worker when a job is queued.
    pub transcription_wake: Arc<tokio::sync::Notify>,
}

impl Media {
    /// Creates the hub and starts relaying listener demand to agents.
    #[must_use]
    pub fn new(agents: AgentHub) -> Self {
        let (hub, mut demand) = MediaHub::new();
        tokio::spawn(async move {
            while let Some(d) = demand.recv().await {
                if !agents.send_audio(&d.agent, d.scanner_id, d.listen) {
                    warn!(agent = %d.agent, scanner = %d.scanner_id, "could not deliver audio demand");
                }
            }
        });
        Self {
            hub,
            webrtc: OnceLock::new(),
            recorder: OnceLock::new(),
            sessions: Mutex::new(HashMap::new()),
            per_subject: std::sync::atomic::AtomicUsize::new(4),
            transcription: OnceLock::new(),
            transcription_wake: Arc::new(tokio::sync::Notify::new()),
        }
    }

    /// Enables WebRTC listening.
    pub fn enable_webrtc(&self, server: WebRtcServer, per_subject: usize) {
        self.per_subject
            .store(per_subject.max(1), std::sync::atomic::Ordering::Relaxed);
        let _ = self.webrtc.set(server);
    }

    /// Creates a listening session on this replica.
    ///
    /// # Errors
    /// [`ListenError`].
    pub async fn listen(
        &self,
        agent: &platform_common::AgentId,
        offer: &str,
        meta: SessionMeta,
    ) -> Result<(SessionId, String), ListenError> {
        let server = self.webrtc().ok_or(ListenError::Disabled)?;
        let limit = self.per_subject.load(std::sync::atomic::Ordering::Relaxed);
        {
            let s = self
                .sessions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if s.values().filter(|m| m.subject == meta.subject).count() >= limit {
                return Err(ListenError::Busy {
                    message: format!("at most {limit} listening sessions per user"),
                });
            }
        }
        let (id, answer) =
            server
                .create(meta.scanner_id, agent, offer)
                .await
                .map_err(|e| match e {
                    media_gateway::WebRtcError::Offer(m) => ListenError::Offer { message: m },
                    media_gateway::WebRtcError::Busy => ListenError::Busy {
                        message: "too many listening sessions".into(),
                    },
                    media_gateway::WebRtcError::Unavailable => ListenError::Disabled,
                })?;
        self.sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(id, meta);
        Ok((id, answer))
    }

    /// Ends a local session if `subject` owns it or `admin`; returns
    /// `Some(true)` if closed, `Some(false)` if not permitted, `None` if
    /// the session is not on this replica.
    pub fn hangup(
        &self,
        id: SessionId,
        tenant: TenantId,
        subject: &str,
        admin: bool,
    ) -> Option<bool> {
        let mut s = self
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let m = s.get(&id)?;
        if m.tenant_id != tenant || (m.subject != subject && !admin) {
            return Some(false);
        }
        s.remove(&id);
        if let Some(w) = self.webrtc() {
            w.close(id);
        }
        Some(true)
    }

    /// Forgets sessions the media task has already dropped.
    pub fn prune(&self) {
        if let Some(w) = self.webrtc()
            && w.sessions() == 0
        {
            self.sessions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clear();
        }
    }

    /// The WebRTC server, if live audio is enabled.
    #[must_use]
    pub fn webrtc(&self) -> Option<&WebRtcServer> {
        self.webrtc.get()
    }

    /// Enables transcription of new recordings.
    pub fn enable_transcription(&self, t: Arc<crate::transcription::Transcription>) {
        let _ = self.transcription.set(t);
    }

    /// Transcription, if configured.
    #[must_use]
    pub fn transcription(&self) -> Option<&Arc<crate::transcription::Transcription>> {
        self.transcription.get()
    }

    /// The recorder, if recording is enabled.
    #[must_use]
    pub fn recorder(&self) -> Option<&Recorder> {
        self.recorder.get()
    }

    /// Enables recording: taps the hub, segments transmissions and stores
    /// them, recording each object key on its transmission.
    pub fn enable_recording(
        &self,
        store: RecordingStore,
        policy: SegmentPolicy,
        db: Arc<dyn Store>,
    ) {
        let (tap, frames) = mpsc::channel::<Arc<MediaFrame>>(4096);
        let (events, ev_rx) = mpsc::channel(1024);
        self.hub.set_tap(tap);
        let stats = Arc::new(Mutex::new(RecorderStats::default()));
        tokio::spawn(record(
            frames,
            ev_rx,
            policy,
            store.clone(),
            db,
            Arc::clone(&stats),
            Arc::clone(&self.transcription_wake),
        ));
        info!(backend = %store.describe(), "transmission recording enabled");
        let _ = self.recorder.set(Recorder {
            store,
            events,
            stats,
        });
    }
}

fn unix_ms(t: time::OffsetDateTime) -> i64 {
    i64::try_from(t.unix_timestamp_nanos() / 1_000_000).unwrap_or(0)
}

struct Meta {
    tenant_id: TenantId,
    site_id: SiteId,
    scanner_id: ScannerId,
    started_at: time::OffsetDateTime,
    transcribe: bool,
}

async fn record(
    mut frames: mpsc::Receiver<Arc<MediaFrame>>,
    mut events: mpsc::Receiver<RecordEvent>,
    policy: SegmentPolicy,
    store: RecordingStore,
    db: Arc<dyn Store>,
    stats: Arc<Mutex<RecorderStats>>,
    wake: Arc<tokio::sync::Notify>,
) {
    let mut seg = Segmenter::new(policy);
    let mut meta: HashMap<TransmissionId, Meta> = HashMap::new();
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    loop {
        let done = tokio::select! {
            f = frames.recv() => match f { Some(f) => seg.frame(f), None => return },
            e = events.recv() => match e {
                Some(RecordEvent::Start { tenant_id, site_id, scanner_id, transmission_id, started_at, transcribe }) => {
                    meta.insert(transmission_id, Meta { tenant_id, site_id, scanner_id, started_at, transcribe });
                    seg.start(transmission_id, scanner_id, unix_ms(started_at));
                    Vec::new()
                }
                Some(RecordEvent::End { transmission_id, ended_at }) => {
                    seg.end(transmission_id, unix_ms(ended_at));
                    Vec::new()
                }
                None => return,
            },
            _ = tick.tick() => seg.flush_stale(unix_ms(platform_common::now_utc())),
        };
        if let Ok(mut s) = stats.lock() {
            s.in_progress = seg.open_count() as u64;
        }
        for segment in done {
            let Some(m) = meta.remove(&segment.transmission_id) else {
                continue;
            };
            let key = RecordingStore::key(
                m.tenant_id,
                m.site_id,
                m.scanner_id,
                m.started_at,
                segment.transmission_id,
            );
            let bytes = scanner_recorder::encode_ogg(&segment);
            let (store, db, stats, wake) = (
                store.clone(),
                Arc::clone(&db),
                Arc::clone(&stats),
                Arc::clone(&wake),
            );
            tokio::spawn(async move {
                let r = match store.put(&key, bytes).await {
                    Ok(()) => db
                        .set_transmission_audio(
                            m.tenant_id,
                            segment.transmission_id,
                            &key,
                            m.transcribe,
                            platform_common::now_utc(),
                        )
                        .await
                        .map_err(|e| e.to_string()),
                    Err(e) => Err(e.to_string()),
                };
                if r.is_ok() && m.transcribe {
                    wake.notify_one();
                }
                let mut s = stats
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                match r {
                    Ok(()) => s.stored += 1,
                    Err(e) => {
                        s.failed += 1;
                        warn!(transmission = %segment.transmission_id, error = %e, "recording not stored");
                    }
                }
            });
        }
        // Forget metadata for transmissions whose segment never produced audio.
        if meta.len() > 10_000 {
            meta.clear();
        }
    }
}
