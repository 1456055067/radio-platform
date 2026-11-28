//! The agent WebSocket endpoint (`/agent/v1`, mTLS listener only).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{CloseFrame, Message, Utf8Bytes, WebSocket, WebSocketUpgrade};
use axum::extract::{Extension, State};
use axum::http::{Method, StatusCode};
use axum::response::{IntoResponse, Response};

use platform_common::wire::{
    AgentMessage, GatewayMessage, MAX_MESSAGE_BYTES, MIN_PROTOCOL_VERSION, PROTOCOL_VERSION,
    ScannerAssignment,
};
use platform_common::{AgentId, ScannerEvent, ScannerId, SiteId, TenantId, now_utc};
use platform_storage::StoreError;
use platform_storage::model::{Agent, NewEvent, ScannerRecord};
use tokio::time::Instant;
use tracing::{debug, info, warn};

use crate::error::ApiError;
use crate::hub::{LiveEvent, LiveKind};
use crate::security_log::{Listener, Reason, SecurityEvent};
use crate::serve::{ClientAddr, PeerAgent};
use crate::state::AppState;

const HELLO_TIMEOUT: Duration = Duration::from_secs(10);
const PING_EVERY: Duration = Duration::from_secs(20);
const IDLE_LIMIT: Duration = Duration::from_secs(60);
/// Minimum interval between telemetry writes per scanner. A snapshot that
/// arrives sooner is kept and written when the interval has passed, so the
/// stored snapshot is never older than this behind the latest one received.
const TELEMETRY_WRITE_INTERVAL: Duration = Duration::from_secs(1);
/// How often deferred telemetry writes are checked.
const TELEMETRY_FLUSH_EVERY: Duration = Duration::from_millis(250);

/// Scanner assignments for an agent.
#[must_use]
pub fn assignments(records: &[ScannerRecord]) -> Vec<ScannerAssignment> {
    records
        .iter()
        .map(|s| ScannerAssignment {
            scanner_id: s.id,
            site_id: Some(s.site_id),
            name: s.name.clone(),
            host: s.host,
            port: s.port,
            enabled: s.enabled,
            audio: s
                .audio_enabled
                .then_some(platform_common::wire::AudioAssignment {
                    rtsp_port: s.rtsp_port,
                    record: s.recording_enabled,
                }),
        })
        .collect()
}

/// Upgrade handler.
pub async fn agent_socket(
    State(state): State<Arc<AppState>>,
    Extension(peer): Extension<PeerAgent>,
    client: Option<Extension<ClientAddr>>,
    method: Method,
    ws: WebSocketUpgrade,
) -> Response {
    // The agent listener trusts no proxy: the source is the TCP peer.
    let reject = |reason: Reason, status: StatusCode, agent: Option<&AgentId>| {
        state.security.record(&SecurityEvent {
            reason,
            claim: None,
            status: status.as_u16(),
            ip: client.as_ref().map(|c| c.0.0.ip()),
            method: &method,
            route: platform_common::wire::AGENT_PATH,
            listener: Listener::Agent,
            subject: None,
            tenant: None,
            agent,
        });
    };
    let Some(agent_id) = peer.0 else {
        // NIST 800-53 Rev5: AU-2, AU-12 (CA-verified client certificate without a usable agent identity)
        reject(Reason::AgentIdentity, StatusCode::UNAUTHORIZED, None);
        return ApiError::new(
            StatusCode::UNAUTHORIZED,
            "client certificate identity required",
        )
        .into_response();
    };
    let agent = match state.store.get_agent(&agent_id).await {
        Ok(a) => a,
        Err(StoreError::NotFound) => {
            // NIST 800-53 Rev5: AU-2, AU-12 (connection from an agent that is not registered; the id comes from the CA-verified certificate)
            reject(
                Reason::AgentUnregistered,
                StatusCode::FORBIDDEN,
                Some(&agent_id),
            );
            return ApiError::forbidden("agent is not registered").into_response();
        }
        Err(e) => {
            // POAM: a storage failure during the agent lookup is still answered with 403 "not registered" (503 would be more accurate but changes agent-visible behaviour).
            tracing::error!(error = %e, agent = %agent_id, "storage failure");
            return ApiError::forbidden("agent is not registered").into_response();
        }
    };
    ws.max_message_size(MAX_MESSAGE_BYTES)
        .max_frame_size(MAX_MESSAGE_BYTES)
        .on_upgrade(move |socket| async move {
            run(state, agent, socket).await;
        })
}

struct Session {
    state: Arc<AppState>,
    agent: Agent,
    session: uuid::Uuid,
    scanners: HashMap<ScannerId, (TenantId, SiteId)>,
    recording: std::collections::HashSet<ScannerId>,
    transcribing: std::collections::HashSet<ScannerId>,
    last_telemetry_write: HashMap<ScannerId, Instant>,
    /// Latest snapshot per scanner that arrived inside the write interval
    /// and is not stored yet (at most one per managed scanner).
    pending_telemetry: HashMap<ScannerId, platform_common::TelemetrySnapshot>,
}

impl Session {
    async fn reload(&mut self) {
        if let Ok(list) = self.state.store.scanners_for_agent(&self.agent.id).await {
            self.scanners = list
                .iter()
                .map(|s| (s.id, (s.tenant_id, s.site_id)))
                .collect();
            self.recording = list
                .iter()
                .filter(|s| s.enabled && s.audio_enabled && s.recording_enabled)
                .map(|s| s.id)
                .collect();
            self.transcribing = list
                .iter()
                .filter(|s| self.recording.contains(&s.id) && s.transcription_enabled)
                .map(|s| s.id)
                .collect();
        }
    }

    async fn owner(&mut self, scanner: ScannerId) -> Option<(TenantId, SiteId)> {
        if !self.scanners.contains_key(&scanner) {
            self.reload().await;
        }
        let r = self.scanners.get(&scanner).copied();
        if r.is_none() {
            warn!(agent = %self.agent.id, %scanner, "agent reported a scanner it does not manage; ignored");
        }
        r
    }

    /// Stores an event; returns true once it is durable (new or duplicate).
    async fn event(&mut self, seq: u64, scanner: ScannerId, event: ScannerEvent) -> bool {
        let Some((tenant, site)) = self.owner(scanner).await else {
            // Acknowledge so the agent does not replay it forever.
            return true;
        };
        let payload = match serde_json::to_value(&event) {
            Ok(v) => v,
            Err(_) => return true,
        };
        let kind = payload
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_owned();
        let stored = self
            .state
            .store
            .append_event(NewEvent {
                tenant_id: tenant,
                site_id: site,
                scanner_id: scanner,
                agent_id: self.agent.id.clone(),
                agent_session: self.session,
                agent_seq: seq,
                kind,
                payload: payload.clone(),
            })
            .await;
        let fresh = match stored {
            Ok(Some(_)) => true,
            Ok(None) => false,
            Err(e) => {
                warn!(error = %e, "failed to store event; not acknowledging");
                return false;
            }
        };
        if fresh {
            let store = &self.state.store;
            let res = match &event {
                ScannerEvent::TransmissionStarted(s) => {
                    store.start_transmission(tenant, site, s).await
                }
                ScannerEvent::TransmissionEnded {
                    transmission_id,
                    ended_at,
                    duration_ms,
                } => {
                    store
                        .end_transmission(*transmission_id, *ended_at, *duration_ms)
                        .await
                }
                ScannerEvent::HealthChanged { health } => {
                    store
                        .set_scanner_status(scanner, None, None, Some(*health))
                        .await
                }
            };
            if let Err(e) = res {
                debug!(error = %e, "event side effect not applied");
            }
            if let Some(rec) = self.state.media.recorder() {
                match &event {
                    ScannerEvent::TransmissionStarted(s) if self.recording.contains(&scanner) => {
                        rec.event(crate::media::RecordEvent::Start {
                            tenant_id: tenant,
                            site_id: site,
                            scanner_id: scanner,
                            transmission_id: s.transmission_id,
                            started_at: s.started_at,
                            transcribe: self.transcribing.contains(&scanner)
                                && self.state.media.transcription().is_some(),
                        });
                    }
                    ScannerEvent::TransmissionEnded {
                        transmission_id,
                        ended_at,
                        ..
                    } => rec.event(crate::media::RecordEvent::End {
                        transmission_id: *transmission_id,
                        ended_at: *ended_at,
                    }),
                    _ => {}
                }
            }
            self.state.hub.publish(LiveEvent {
                tenant_id: tenant,
                site_id: site,
                scanner_id: scanner,
                kind: LiveKind::Event,
                data: payload,
            });
        }
        true
    }

    async fn telemetry(
        &mut self,
        scanner: ScannerId,
        snapshot: platform_common::TelemetrySnapshot,
    ) {
        let Some((tenant, site)) = self.owner(scanner).await else {
            return;
        };
        if self.write_due(scanner) {
            self.pending_telemetry.remove(&scanner);
            self.store_telemetry(scanner, &snapshot).await;
        } else {
            self.pending_telemetry.insert(scanner, snapshot.clone());
        }
        if let Ok(data) = serde_json::to_value(&snapshot) {
            self.state.hub.publish(LiveEvent {
                tenant_id: tenant,
                site_id: site,
                scanner_id: scanner,
                kind: LiveKind::Telemetry,
                data,
            });
        }
    }
}

impl Session {
    fn write_due(&self, scanner: ScannerId) -> bool {
        self.last_telemetry_write
            .get(&scanner)
            .is_none_or(|t| t.elapsed() >= TELEMETRY_WRITE_INTERVAL)
    }

    async fn store_telemetry(
        &mut self,
        scanner: ScannerId,
        snapshot: &platform_common::TelemetrySnapshot,
    ) {
        self.last_telemetry_write.insert(scanner, Instant::now());
        if let Err(e) = self.state.store.put_telemetry(scanner, snapshot).await {
            debug!(error = %e, "telemetry not stored");
        }
    }

    /// Writes deferred snapshots whose write interval has passed, or all of
    /// them when `all` is set (the session is ending).
    async fn flush_telemetry(&mut self, all: bool) {
        if self.pending_telemetry.is_empty() {
            return;
        }
        let ready: Vec<ScannerId> = self
            .pending_telemetry
            .keys()
            .copied()
            .filter(|id| all || self.write_due(*id))
            .collect();
        for id in ready {
            if let Some(snapshot) = self.pending_telemetry.remove(&id) {
                self.store_telemetry(id, &snapshot).await;
            }
        }
    }
}

fn text(msg: &GatewayMessage) -> Option<Message> {
    serde_json::to_string(msg)
        .ok()
        .map(|s| Message::Text(Utf8Bytes::from(s)))
}

async fn close(socket: &mut WebSocket, code: u16, reason: &'static str) {
    let _ = socket
        .send(Message::Close(Some(CloseFrame {
            code,
            reason: Utf8Bytes::from_static(reason),
        })))
        .await;
}

async fn run(state: Arc<AppState>, agent: Agent, mut socket: WebSocket) {
    // Hello.
    let hello = match tokio::time::timeout(HELLO_TIMEOUT, socket.recv()).await {
        Ok(Some(Ok(Message::Text(t)))) => serde_json::from_str::<AgentMessage>(&t).ok(),
        _ => None,
    };
    let Some(AgentMessage::Hello {
        protocol_version,
        session,
        agent_version,
        scanners: reports,
    }) = hello
    else {
        close(&mut socket, 1002, "expected hello").await;
        return;
    };
    if !(MIN_PROTOCOL_VERSION..=PROTOCOL_VERSION).contains(&protocol_version) {
        close(&mut socket, 1002, "unsupported protocol version").await;
        return;
    }
    let agent_id: AgentId = agent.id.clone();
    info!(agent = %agent_id, %agent_version, protocol = protocol_version, %session, "agent connected");
    let reg = state.hub.register(&agent_id, protocol_version);
    let mut outbound = reg.outbound;
    let _ = state.store.touch_agent(&agent_id, now_utc()).await;

    let mut s = Session {
        state: Arc::clone(&state),
        agent,
        session,
        scanners: HashMap::new(),
        recording: std::collections::HashSet::new(),
        transcribing: std::collections::HashSet::new(),
        last_telemetry_write: HashMap::new(),
        pending_telemetry: HashMap::new(),
    };
    s.reload().await;
    for r in reports {
        if s.scanners.contains_key(&r.scanner_id) {
            let _ = state
                .store
                .set_scanner_status(r.scanner_id, r.model, r.firmware, Some(r.health))
                .await;
        }
    }
    let records = state
        .store
        .scanners_for_agent(&agent_id)
        .await
        .unwrap_or_default();
    let welcome = GatewayMessage::Welcome {
        scanners: assignments(&records),
    };
    if let Some(m) = text(&welcome)
        && socket.send(m).await.is_err()
    {
        state.hub.unregister(&agent_id, reg.conn);
        return;
    }
    // Restore live-listener demand after a reconnect.
    if protocol_version >= platform_common::wire::AUDIO_PROTOCOL_VERSION {
        for scanner_id in state.media.hub.listened(&agent_id) {
            if let Some(m) = text(&GatewayMessage::Audio {
                scanner_id,
                listen: true,
            }) && socket.send(m).await.is_err()
            {
                break;
            }
        }
    }

    let mut last_rx = Instant::now();
    let mut ping = tokio::time::interval(PING_EVERY);
    let mut flush = tokio::time::interval(TELEMETRY_FLUSH_EVERY);
    flush.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut nonce: u64 = 0;
    loop {
        tokio::select! {
            _ = flush.tick() => s.flush_telemetry(false).await,
            out = outbound.recv() => {
                let Some(msg) = out else {
                    // Replaced by a newer connection.
                    close(&mut socket, 1012, "superseded").await;
                    break;
                };
                if let GatewayMessage::Configure { .. } = &msg { s.reload().await; }
                let Some(m) = text(&msg) else { continue };
                if socket.send(m).await.is_err() { break; }
            }
            incoming = socket.recv() => {
                let Some(Ok(frame)) = incoming else { break };
                last_rx = Instant::now();
                match frame {
                    Message::Text(t) => {
                        let Ok(msg) = serde_json::from_str::<AgentMessage>(&t) else {
                            close(&mut socket, 1003, "malformed message").await;
                            break;
                        };
                        match msg {
                            AgentMessage::Event { seq, scanner_id, event } => {
                                if s.event(seq, scanner_id, event).await
                                    && let Some(m) = text(&GatewayMessage::Ack { seq })
                                        && socket.send(m).await.is_err() { break; }
                            }
                            AgentMessage::Telemetry { scanner_id, snapshot } => s.telemetry(scanner_id, *snapshot).await,
                            AgentMessage::CommandResult { request_id, outcome } => state.hub.complete(&agent_id, request_id, outcome),
                            AgentMessage::Status { scanners } => {
                                for r in scanners {
                                    if s.owner(r.scanner_id).await.is_some() {
                                        let _ = state.store.set_scanner_status(r.scanner_id, r.model, r.firmware, Some(r.health)).await;
                                    }
                                }
                            }
                            AgentMessage::AudioState { scanner_id, state: audio } => {
                                if let Some((tenant, site)) = s.owner(scanner_id).await {
                                    state.media.hub.set_state(scanner_id, audio.clone());
                                    if let Ok(data) = serde_json::to_value(&audio) {
                                        state.hub.publish(crate::hub::LiveEvent {
                                            tenant_id: tenant,
                                            site_id: site,
                                            scanner_id,
                                            kind: crate::hub::LiveKind::Audio,
                                            data,
                                        });
                                    }
                                }
                            }
                            AgentMessage::Pong { .. } => {}
                            AgentMessage::Hello { .. } => {
                                close(&mut socket, 1002, "unexpected hello").await;
                                break;
                            }
                        }
                    }
                    Message::Close(_) => break,
                    Message::Binary(b) => {
                        // Media frames: accepted only for scanners this agent manages.
                        match platform_common::wire::media::decode(&b) {
                            Some(f) if s.scanners.contains_key(&f.scanner_id) => state.media.hub.publish(f),
                            Some(_) => {}
                            None => {
                                close(&mut socket, 1003, "malformed media frame").await;
                                break;
                            }
                        }
                    }
                    Message::Ping(_) | Message::Pong(_) => {}
                }
            }
            _ = ping.tick() => {
                if last_rx.elapsed() > IDLE_LIMIT {
                    warn!(agent = %agent_id, "agent idle; closing");
                    close(&mut socket, 1001, "idle").await;
                    break;
                }
                nonce = nonce.wrapping_add(1);
                if let Some(m) = text(&GatewayMessage::Ping { nonce })
                    && socket.send(m).await.is_err() { break; }
            }
        }
    }
    s.flush_telemetry(true).await;
    state.hub.unregister(&agent_id, reg.conn);
    let _ = state.store.touch_agent(&agent_id, now_utc()).await;
    info!(agent = %agent_id, "agent disconnected");
}
