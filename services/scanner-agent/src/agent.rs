//! Agent runtime: scanner registry, outage buffer and the upstream session.

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use platform_common::wire::{
    AgentMessage, GatewayMessage, MAX_MESSAGE_BYTES, PROTOCOL_VERSION, ScannerAssignment,
    ScannerReport,
};
use platform_common::{CommandError, RequestId, ScannerCommand, ScannerId};
use rustls::pki_types::ServerName;
use scanner_controller::{ApplyResult, RegistryConfig, ScannerRegistry};
use scanner_telemetry::{CollectionMode, CollectorConfig, CollectorOutput, DetectorConfig};
use tokio::net::TcpStream;
use tokio::sync::{Mutex, Notify, mpsc, oneshot};
use tokio_rustls::TlsConnector;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::Uri;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};
use uniden_sds::Scanner;

use crate::AgentError;
use crate::buffer::Buffer;
use crate::config::{AgentConfig, TelemetryMode};
use crate::tls::client_config;

const WELCOME_TIMEOUT: Duration = Duration::from_secs(10);
const IDLE_LIMIT: Duration = Duration::from_secs(60);
const PING_EVERY: Duration = Duration::from_secs(20);
const BACKOFF_MIN: Duration = Duration::from_millis(250);
const BACKOFF_MAX: Duration = Duration::from_secs(30);
/// Maximum concurrently executing remote commands.
const MAX_INFLIGHT_COMMANDS: usize = 64;

enum RegistryCmd {
    Apply(Vec<ScannerAssignment>, oneshot::Sender<Vec<ApplyResult>>),
    Reports(oneshot::Sender<Vec<ScannerReport>>),
    Scanner(ScannerId, oneshot::Sender<Option<Scanner>>),
    Endpoints(oneshot::Sender<Vec<std::net::SocketAddr>>),
}

/// Handle used by the session to reach the registry actor.
#[derive(Clone)]
struct Registry(mpsc::Sender<RegistryCmd>);

impl Registry {
    async fn apply(&self, a: Vec<ScannerAssignment>) -> Vec<ApplyResult> {
        let (tx, rx) = oneshot::channel();
        if self.0.send(RegistryCmd::Apply(a, tx)).await.is_err() {
            return Vec::new();
        }
        rx.await.unwrap_or_default()
    }
    async fn reports(&self) -> Vec<ScannerReport> {
        let (tx, rx) = oneshot::channel();
        if self.0.send(RegistryCmd::Reports(tx)).await.is_err() {
            return Vec::new();
        }
        rx.await.unwrap_or_default()
    }
    async fn endpoints(&self) -> Vec<std::net::SocketAddr> {
        let (tx, rx) = oneshot::channel();
        if self.0.send(RegistryCmd::Endpoints(tx)).await.is_err() {
            return Vec::new();
        }
        rx.await.unwrap_or_default()
    }
    async fn scanner(&self, id: ScannerId) -> Option<Scanner> {
        let (tx, rx) = oneshot::channel();
        self.0.send(RegistryCmd::Scanner(id, tx)).await.ok()?;
        rx.await.ok().flatten()
    }
}

struct Shared {
    audio: Mutex<crate::audio::AudioManager>,
    media_rx: Mutex<mpsc::Receiver<platform_common::wire::media::MediaFrame>>,
    audio_states: Mutex<mpsc::Receiver<(ScannerId, platform_common::wire::AudioState)>>,
    policy: platform_common::NetworkPolicy,
    discovery: Arc<tokio::sync::Semaphore>,
    buffer: Mutex<Buffer>,
    changed: Notify,
    status: tokio::sync::watch::Sender<AgentStatus>,
    connected: std::sync::atomic::AtomicBool,
    sessions: std::sync::atomic::AtomicU64,
}

impl Shared {
    fn set_connected(&self, c: bool) {
        self.connected.store(c, std::sync::atomic::Ordering::SeqCst);
    }

    /// Publishes status; call with the buffer lock held or just released.
    fn publish(&self, b: &Buffer) {
        self.status.send_replace(AgentStatus {
            connected: self.connected.load(std::sync::atomic::Ordering::SeqCst),
            pending_events: b.pending(),
            dropped_events: b.dropped(),
            durable: b.durable(),
            sessions: self.sessions.load(std::sync::atomic::Ordering::SeqCst),
        });
    }
}

/// Observable agent status (for tests and health endpoints).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AgentStatus {
    /// Connected to the gateway.
    pub connected: bool,
    /// Unacknowledged buffered events.
    pub pending_events: usize,
    /// Events dropped due to buffer overflow.
    pub dropped_events: u64,
    /// Buffered events are being persisted to the journal.
    pub durable: bool,
    /// Completed upstream sessions.
    pub sessions: u64,
}

/// A running agent.
#[derive(Debug)]
pub struct AgentHandle {
    status: tokio::sync::watch::Receiver<AgentStatus>,
    cancel: CancellationToken,
    task: tokio::task::JoinHandle<()>,
}

impl AgentHandle {
    /// Current status.
    #[must_use]
    pub fn status(&self) -> AgentStatus {
        *self.status.borrow()
    }

    /// A receiver observing status changes.
    #[must_use]
    pub fn watch(&self) -> tokio::sync::watch::Receiver<AgentStatus> {
        self.status.clone()
    }

    /// Stops the agent and waits for it to finish.
    pub async fn shutdown(self) {
        self.cancel.cancel();
        let _ = self.task.await;
    }
}

type Ws = tokio_tungstenite::WebSocketStream<tokio_rustls::client::TlsStream<TcpStream>>;

struct Upstream {
    connector: TlsConnector,
    host: String,
    port: u16,
    url: String,
}

impl Upstream {
    fn new(cfg: &AgentConfig) -> Result<Self, AgentError> {
        let uri: Uri = cfg
            .gateway
            .url
            .parse()
            .map_err(|_| AgentError::Config("invalid gateway url".into()))?;
        if uri.scheme_str() != Some("wss") {
            return Err(AgentError::Config("gateway url must use wss://".into()));
        }
        let host = uri
            .host()
            .ok_or_else(|| AgentError::Config("gateway url has no host".into()))?;
        let host = host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .to_owned();
        let tls = client_config(&cfg.gateway.ca, &cfg.gateway.cert, &cfg.gateway.key)?;
        Ok(Self {
            connector: TlsConnector::from(tls),
            port: uri.port_u16().unwrap_or(443),
            host,
            url: cfg.gateway.url.clone(),
        })
    }

    async fn connect(&self) -> Result<Ws, AgentError> {
        let tcp = tokio::time::timeout(
            Duration::from_secs(10),
            TcpStream::connect((self.host.as_str(), self.port)),
        )
        .await
        .map_err(|_| AgentError::Upstream("connect timeout".into()))?
        .map_err(|e| AgentError::Upstream(e.to_string()))?;
        let name = ServerName::try_from(self.host.clone())
            .map_err(|e| AgentError::Config(e.to_string()))?;
        let tls = tokio::time::timeout(Duration::from_secs(10), self.connector.connect(name, tcp))
            .await
            .map_err(|_| AgentError::Upstream("TLS handshake timeout".into()))?
            .map_err(|e| AgentError::Upstream(format!("TLS: {e}")))?;
        let req = self
            .url
            .as_str()
            .into_client_request()
            .map_err(|e| AgentError::Config(e.to_string()))?;
        let ws_cfg = WebSocketConfig::default()
            .max_message_size(Some(MAX_MESSAGE_BYTES))
            .max_frame_size(Some(MAX_MESSAGE_BYTES));
        let (ws, _) = tokio_tungstenite::client_async_with_config(req, tls, Some(ws_cfg))
            .await
            .map_err(|e| AgentError::Upstream(format!("WebSocket: {e}")))?;
        Ok(ws)
    }
}

fn text(m: &AgentMessage) -> Option<Message> {
    serde_json::to_string(m)
        .ok()
        .map(|s| Message::Text(s.into()))
}

async fn send(ws: &mut Ws, m: &AgentMessage) -> Result<(), AgentError> {
    let msg = text(m).ok_or_else(|| AgentError::Upstream("serialization failed".into()))?;
    ws.send(msg)
        .await
        .map_err(|e| AgentError::Upstream(e.to_string()))
}

fn log_apply(results: &[ApplyResult]) {
    for r in results {
        if let ApplyResult::Refused(id, why) = r {
            warn!(scanner = %id, reason = %why, "scanner assignment refused");
        }
    }
}

/// Starts the agent.
///
/// # Errors
/// Returns an error for invalid configuration (bad URL or key material).
pub fn start(cfg: AgentConfig) -> Result<AgentHandle, AgentError> {
    let upstream = Upstream::new(&cfg)?;
    let cancel = CancellationToken::new();
    let (status_tx, status) = tokio::sync::watch::channel(AgentStatus::default());
    let task = tokio::spawn(run(cfg, upstream, cancel.clone(), status_tx));
    Ok(AgentHandle {
        status,
        cancel,
        task,
    })
}

async fn run(
    cfg: AgentConfig,
    upstream: Upstream,
    cancel: CancellationToken,
    status: tokio::sync::watch::Sender<AgentStatus>,
) {
    let (out_tx, mut out_rx) = mpsc::channel::<CollectorOutput>(1024);
    let mode = match cfg.telemetry.mode {
        TelemetryMode::Push => CollectionMode::Push {
            interval_ms: cfg.telemetry.interval_ms,
        },
        TelemetryMode::Poll => CollectionMode::Poll {
            interval: Duration::from_millis(u64::from(cfg.telemetry.interval_ms.max(50))),
        },
    };
    let registry = ScannerRegistry::new(
        RegistryConfig {
            policy: cfg.network.clone(),
            max_scanners: cfg.max_scanners,
            collector: CollectorConfig {
                mode,
                detector: DetectorConfig {
                    hang_time: Duration::from_millis(cfg.telemetry.hang_time_ms),
                },
                ..CollectorConfig::default()
            },
            sdk: None,
        },
        out_tx,
    );
    let (reg_tx, mut reg_rx) = mpsc::channel::<RegistryCmd>(32);
    let reg_cancel = cancel.clone();
    let reg_task = tokio::spawn(async move {
        let mut registry = registry;
        loop {
            tokio::select! {
                () = reg_cancel.cancelled() => break,
                cmd = reg_rx.recv() => match cmd {
                    None => break,
                    Some(RegistryCmd::Apply(a, tx)) => { let _ = tx.send(registry.apply(a).await); }
                    Some(RegistryCmd::Reports(tx)) => { let _ = tx.send(registry.reports()); }
                    Some(RegistryCmd::Scanner(id, tx)) => { let _ = tx.send(registry.scanner(id)); }
                    Some(RegistryCmd::Endpoints(tx)) => { let _ = tx.send(registry.endpoints()); }
                },
            }
        }
        registry.shutdown().await;
    });
    let registry = Registry(reg_tx);
    let (buffer, session_id) = match &cfg.state_dir {
        Some(dir) => match Buffer::open(cfg.max_buffered_events, dir) {
            Ok((b, session)) => {
                info!(dir = %dir.display(), recovered = b.pending(), "durable event journal open");
                (b, session)
            }
            Err(e) => {
                error!(error = %e, "cannot open the event journal; buffering in memory only");
                (Buffer::new(cfg.max_buffered_events), uuid::Uuid::new_v4())
            }
        },
        None => (Buffer::new(cfg.max_buffered_events), uuid::Uuid::new_v4()),
    };
    let (media_tx, media_rx) = mpsc::channel(512);
    let (states_tx, audio_states) = mpsc::channel(256);
    let shared = Arc::new(Shared {
        audio: Mutex::new(crate::audio::AudioManager::new(
            cfg.network.clone(),
            cfg.audio.clone(),
            media_tx,
            states_tx,
        )),
        media_rx: Mutex::new(media_rx),
        audio_states: Mutex::new(audio_states),
        policy: cfg.network.clone(),
        discovery: Arc::new(tokio::sync::Semaphore::new(1)),
        buffer: Mutex::new(buffer),
        changed: Notify::new(),
        status,
        connected: std::sync::atomic::AtomicBool::new(false),
        sessions: std::sync::atomic::AtomicU64::new(0),
    });

    // Collector outputs → buffer (always, connected or not).
    let pump = {
        let shared = Arc::clone(&shared);
        let cancel = cancel.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    () = cancel.cancelled() => break,
                    o = out_rx.recv() => {
                        let Some(o) = o else { break };
                        let mut b = shared.buffer.lock().await;
                        match o {
                            CollectorOutput::Event(id, e) => { b.push_event(id, *e); }
                            CollectorOutput::Telemetry(id, t) => b.put_telemetry(id, *t),
                        }
                        shared.publish(&b);
                        drop(b);
                        shared.changed.notify_one();
                    }
                }
            }
        })
    };

    // Bootstrap from local configuration so collection works before (and
    // without) the gateway.
    let local: Vec<ScannerAssignment> = cfg.scanners.iter().map(Into::into).collect();
    if !local.is_empty() {
        log_apply(&registry.apply(local).await);
    }

    shared.publish(&*shared.buffer.lock().await);
    let mut backoff = BACKOFF_MIN;
    while !cancel.is_cancelled() {
        let ws = tokio::select! {
            () = cancel.cancelled() => break,
            r = upstream.connect() => r,
        };
        match ws {
            Ok(ws) => {
                backoff = BACKOFF_MIN;
                let r = session(ws, session_id, &registry, &shared, &cancel).await;
                shared.set_connected(false);
                shared.audio.lock().await.set_connected(false);
                shared
                    .sessions
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                shared.publish(&*shared.buffer.lock().await);
                match r {
                    Ok(()) => info!("upstream session ended"),
                    Err(e) => warn!(error = %e, "upstream session failed"),
                }
            }
            Err(e) => {
                warn!(error = %e, retry_in = ?backoff, "cannot reach gateway");
                tokio::select! {
                    () = cancel.cancelled() => break,
                    () = tokio::time::sleep(backoff) => {}
                }
                backoff = (backoff * 2).min(BACKOFF_MAX);
            }
        }
    }
    cancel.cancel();
    shared.audio.lock().await.shutdown().await;
    let _ = tokio::join!(reg_task, pump);
}

async fn session(
    mut ws: Ws,
    session_id: uuid::Uuid,
    registry: &Registry,
    shared: &Shared,
    cancel: &CancellationToken,
) -> Result<(), AgentError> {
    send(
        &mut ws,
        &AgentMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
            session: session_id,
            agent_version: env!("CARGO_PKG_VERSION").into(),
            scanners: registry.reports().await,
        },
    )
    .await?;
    let welcome = tokio::time::timeout(WELCOME_TIMEOUT, ws.next())
        .await
        .map_err(|_| AgentError::Upstream("no welcome".into()))?;
    let assignments = match welcome {
        Some(Ok(Message::Text(t))) => match serde_json::from_str::<GatewayMessage>(&t) {
            Ok(GatewayMessage::Welcome { scanners }) => scanners,
            _ => return Err(AgentError::Upstream("expected welcome".into())),
        },
        Some(Ok(Message::Close(f))) => {
            return Err(AgentError::Upstream(format!("closed by gateway: {f:?}")));
        }
        other => return Err(AgentError::Upstream(format!("handshake failed: {other:?}"))),
    };
    info!(scanners = assignments.len(), "connected to gateway");
    {
        let mut audio = shared.audio.lock().await;
        audio.apply(&assignments).await;
        audio.set_connected(true);
    }
    log_apply(&registry.apply(assignments).await);
    let mut media_rx = shared.media_rx.lock().await;
    let mut audio_states = shared.audio_states.lock().await;
    // Drop frames captured while disconnected: live audio is not buffered.
    while media_rx.try_recv().is_ok() {}
    send(
        &mut ws,
        &AgentMessage::Status {
            scanners: registry.reports().await,
        },
    )
    .await?;
    shared.set_connected(true);
    {
        let mut b = shared.buffer.lock().await;
        b.mark_all_dirty();
        shared.publish(&b);
    }

    let (res_tx, mut res_rx) =
        mpsc::channel::<(RequestId, platform_common::CommandOutcome)>(MAX_INFLIGHT_COMMANDS);
    let inflight = Arc::new(tokio::sync::Semaphore::new(MAX_INFLIGHT_COMMANDS));
    let mut last_sent: u64 = 0;
    let mut last_rx = tokio::time::Instant::now();
    let mut ping = tokio::time::interval(PING_EVERY);
    shared.changed.notify_one();
    loop {
        tokio::select! {
            () = cancel.cancelled() => {
                let _ = ws.close(None).await;
                return Ok(());
            }
            () = shared.changed.notified() => {
                let (events, telemetry) = {
                    let mut b = shared.buffer.lock().await;
                    (b.events_after(last_sent), b.take_dirty_telemetry())
                };
                for e in events {
                    send(&mut ws, &AgentMessage::Event { seq: e.seq, scanner_id: e.scanner_id, event: e.event }).await?;
                    last_sent = e.seq;
                }
                for (scanner_id, snapshot) in telemetry {
                    send(&mut ws, &AgentMessage::Telemetry { scanner_id, snapshot: Box::new(snapshot) }).await?;
                }
            }
            r = res_rx.recv() => {
                if let Some((request_id, outcome)) = r {
                    send(&mut ws, &AgentMessage::CommandResult { request_id, outcome }).await?;
                }
            }
            Some(frame) = media_rx.recv() => {
                let bytes = platform_common::wire::media::encode(&frame);
                ws.send(Message::Binary(bytes.into())).await.map_err(|e| AgentError::Upstream(e.to_string()))?;
            }
            Some((scanner_id, state)) = audio_states.recv() => {
                send(&mut ws, &AgentMessage::AudioState { scanner_id, state }).await?;
            }
            msg = ws.next() => {
                last_rx = tokio::time::Instant::now();
                let text = match msg {
                    Some(Ok(Message::Text(t))) => t,
                    Some(Ok(Message::Close(_))) | None => return Ok(()),
                    Some(Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_))) => continue,
                    Some(Ok(Message::Binary(_))) => return Err(AgentError::Upstream("unexpected binary frame".into())),
                    Some(Err(e)) => return Err(AgentError::Upstream(e.to_string())),
                };
                let Ok(m) = serde_json::from_str::<GatewayMessage>(&text) else {
                    return Err(AgentError::Upstream("malformed gateway message".into()));
                };
                match m {
                    GatewayMessage::Ack { seq } => {
                        let mut b = shared.buffer.lock().await;
                        b.ack(seq);
                        shared.publish(&b);
                    }
                    GatewayMessage::Execute { request_id, scanner_id, command } => {
                        let Ok(permit) = Arc::clone(&inflight).try_acquire_owned() else {
                            send(&mut ws, &AgentMessage::CommandResult { request_id, outcome: Err(CommandError::Unavailable { message: "agent busy".into() }) }).await?;
                            continue;
                        };
                        let scanner = registry.scanner(scanner_id).await;
                        let tx = res_tx.clone();
                        tokio::spawn(async move {
                            let _permit = permit;
                            let outcome = match scanner {
                                Some(s) => execute(&s, &command).await,
                                None => Err(CommandError::ScannerNotFound),
                            };
                            let _ = tx.send((request_id, outcome)).await;
                        });
                    }
                    GatewayMessage::Discover { request_id, networks, port, exclude } => {
                        let Ok(permit) = Arc::clone(&shared.discovery).try_acquire_owned() else {
                            send(&mut ws, &AgentMessage::CommandResult { request_id, outcome: Err(CommandError::Unavailable { message: "a discovery is already running".into() }) }).await?;
                            continue;
                        };
                        let mut skip: std::collections::HashSet<std::net::SocketAddr> = exclude.into_iter().collect();
                        skip.extend(registry.endpoints().await);
                        let policy = shared.policy.clone();
                        let tx = res_tx.clone();
                        tokio::spawn(async move {
                            let _permit = permit;
                            let outcome = match crate::discovery::plan(&policy, &networks, port, &skip) {
                                Err(message) => Err(CommandError::Invalid { message }),
                                Ok(targets) => {
                                    info!(targets = targets.len(), "discovery probe started");
                                    match crate::discovery::probe(&targets).await {
                                        Ok(found) => serde_json::to_value(found).map_err(|e| CommandError::Internal { message: e.to_string() }),
                                        Err(e) => Err(CommandError::Internal { message: e.to_string() }),
                                    }
                                }
                            };
                            let _ = tx.send((request_id, outcome)).await;
                        });
                    }
                    GatewayMessage::Audio { scanner_id, listen } => {
                        shared.audio.lock().await.set_listen(scanner_id, listen);
                    }
                    GatewayMessage::Configure { scanners } => {
                        shared.audio.lock().await.apply(&scanners).await;
                        log_apply(&registry.apply(scanners).await);
                        send(&mut ws, &AgentMessage::Status { scanners: registry.reports().await }).await?;
                    }
                    GatewayMessage::Ping { nonce } => send(&mut ws, &AgentMessage::Pong { nonce }).await?,
                    GatewayMessage::Welcome { .. } => return Err(AgentError::Upstream("unexpected welcome".into())),
                }
            }
            _ = ping.tick() => {
                if last_rx.elapsed() > IDLE_LIMIT {
                    return Err(AgentError::Upstream("gateway idle".into()));
                }
                ws.send(Message::Ping(Vec::new().into())).await.map_err(|e| AgentError::Upstream(e.to_string()))?;
            }
        }
    }
}

async fn execute(scanner: &Scanner, command: &ScannerCommand) -> platform_common::CommandOutcome {
    debug!(command = command.kind(), "executing remote command");
    scanner_controller::execute(scanner, command).await
}
