//! Connected agents: command round trips and live event fan-out.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use platform_common::wire::{GatewayMessage, ScannerAssignment};
use platform_common::{
    AgentId, CommandError, CommandOutcome, RequestId, ScannerCommand, ScannerId, SiteId, TenantId,
};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc, oneshot};
use utoipa::ToSchema;

/// A request to an agent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum AgentOp {
    /// A scanner command.
    Command {
        /// Scanner.
        scanner_id: ScannerId,
        /// Command.
        command: ScannerCommand,
    },
    /// Scanner discovery (agent protocol ≥ 2).
    Discover {
        /// Networks to probe.
        networks: Vec<ipnet::IpNet>,
        /// UDP port.
        port: u16,
        /// Endpoints never to probe.
        exclude: Vec<std::net::SocketAddr>,
    },
}

impl AgentOp {
    fn min_protocol(&self) -> u16 {
        match self {
            Self::Command { .. } => platform_common::wire::MIN_PROTOCOL_VERSION,
            Self::Discover { .. } => platform_common::wire::DISCOVERY_PROTOCOL_VERSION,
        }
    }
    /// Whether a timeout leaves scanner state uncertain.
    #[must_use]
    pub fn may_change_state(&self) -> bool {
        match self {
            Self::Command { command, .. } => command.requires_control(),
            Self::Discover { .. } => false,
        }
    }
    fn into_message(self, request_id: RequestId) -> GatewayMessage {
        match self {
            Self::Command {
                scanner_id,
                command,
            } => GatewayMessage::Execute {
                request_id,
                scanner_id,
                command,
            },
            Self::Discover {
                networks,
                port,
                exclude,
            } => GatewayMessage::Discover {
                request_id,
                networks,
                port,
                exclude,
            },
        }
    }
}

/// Maximum in-flight commands per agent.
pub const MAX_PENDING_PER_AGENT: usize = 256;
/// Outbound queue depth per agent connection.
pub const AGENT_QUEUE: usize = 256;

/// Kind of live update.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum LiveKind {
    /// Telemetry snapshot.
    Telemetry,
    /// Scanner event.
    Event,
    /// Audio pipeline state.
    Audio,
}

/// A live update for WebSocket subscribers.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct LiveEvent {
    /// Tenant (used for filtering; not trusted from clients).
    #[serde(skip)]
    pub tenant_id: TenantId,
    /// Site.
    pub site_id: SiteId,
    /// Scanner.
    pub scanner_id: ScannerId,
    /// Kind.
    pub kind: LiveKind,
    /// Payload.
    #[schema(value_type = Object)]
    pub data: serde_json::Value,
}

type Pending = Arc<Mutex<HashMap<RequestId, oneshot::Sender<CommandOutcome>>>>;

#[derive(Debug, Clone)]
struct Handle {
    conn: u64,
    protocol: u16,
    tx: mpsc::Sender<GatewayMessage>,
    pending: Pending,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Registry of connected agents.
#[derive(Debug, Clone)]
pub struct AgentHub {
    agents: Arc<Mutex<HashMap<AgentId, Handle>>>,
    next_conn: Arc<AtomicU64>,
    live: broadcast::Sender<LiveEvent>,
    cluster: Arc<std::sync::OnceLock<Arc<crate::cluster::Cluster>>>,
    media: Arc<std::sync::OnceLock<std::sync::Weak<crate::media::Media>>>,
}

impl Default for AgentHub {
    fn default() -> Self {
        Self::new()
    }
}

/// A registered agent connection.
#[derive(Debug)]
pub struct Registration {
    /// Connection number (to avoid unregistering a newer connection).
    pub conn: u64,
    /// Messages to send to the agent.
    pub outbound: mpsc::Receiver<GatewayMessage>,
}

impl AgentHub {
    /// Creates an empty hub.
    #[must_use]
    pub fn new() -> Self {
        let (live, _) = broadcast::channel(1024);
        Self {
            agents: Arc::new(Mutex::new(HashMap::new())),
            next_conn: Arc::new(AtomicU64::new(1)),
            live,
            cluster: Arc::default(),
            media: Arc::default(),
        }
    }

    /// Connects the replica's media state (set once by `AppState`).
    pub fn set_media(&self, media: &Arc<crate::media::Media>) {
        let _ = self.media.set(Arc::downgrade(media));
    }

    /// This replica's media state.
    #[must_use]
    pub fn media(&self) -> Option<Arc<crate::media::Media>> {
        self.media.get().and_then(std::sync::Weak::upgrade)
    }

    /// Creates a listening session on the replica that holds `agent`.
    ///
    /// # Errors
    /// [`crate::media::ListenError`].
    pub async fn listen(
        &self,
        agent: &AgentId,
        offer: &str,
        meta: crate::media::SessionMeta,
        timeout: Duration,
    ) -> Result<(uuid::Uuid, String), crate::media::ListenError> {
        use crate::media::ListenError;
        if !lock(&self.agents).contains_key(agent)
            && let Some(c) = self.cluster.get()
        {
            return match c.owner(agent) {
                Some(owner) => c.forward_listen(owner, agent, offer, meta, timeout).await,
                None => Err(ListenError::AgentOffline),
            };
        }
        if !lock(&self.agents).contains_key(agent) {
            return Err(ListenError::AgentOffline);
        }
        let media = self.media().ok_or(ListenError::Disabled)?;
        media.listen(agent, offer, meta).await
    }

    /// Ends a listening session wherever it lives. Returns `Some(false)` if
    /// the session is local and the caller may not end it, `Some(true)` if
    /// ended locally, `None` if it was not local (other replicas were told).
    pub fn hangup(
        &self,
        id: uuid::Uuid,
        tenant: TenantId,
        subject: &str,
        admin: bool,
    ) -> Option<bool> {
        if let Some(r) = self
            .media()
            .and_then(|m| m.hangup(id, tenant, subject, admin))
        {
            return Some(r);
        }
        if let Some(c) = self.cluster.get() {
            c.hangup(id, tenant, subject, admin);
        }
        None
    }

    /// Creates a hub that cooperates with other gateway replicas over `bus`.
    #[must_use]
    pub fn clustered(bus: Arc<dyn platform_storage::MessageBus>) -> Self {
        let hub = Self::new();
        let c = crate::cluster::Cluster::join(bus, hub.clone());
        let _ = hub.cluster.set(c);
        hub
    }

    /// The cluster membership, if clustered.
    #[must_use]
    pub fn cluster(&self) -> Option<&Arc<crate::cluster::Cluster>> {
        self.cluster.get()
    }

    /// Agents connected to this replica.
    #[must_use]
    pub fn local_agents(&self) -> Vec<AgentId> {
        lock(&self.agents).keys().cloned().collect()
    }

    fn announce(&self) {
        if let Some(c) = self.cluster.get() {
            c.announce(self.local_agents());
        }
    }

    /// Registers a connection, replacing (and thereby disconnecting) any
    /// previous connection from the same agent.
    pub fn register(&self, agent: &AgentId, protocol: u16) -> Registration {
        let (tx, outbound) = mpsc::channel(AGENT_QUEUE);
        let conn = self.next_conn.fetch_add(1, Ordering::Relaxed);
        let old = lock(&self.agents).insert(
            agent.clone(),
            Handle {
                conn,
                protocol,
                tx,
                pending: Arc::default(),
            },
        );
        if let Some(old) = old {
            fail_all(&old.pending);
        }
        self.announce();
        Registration { conn, outbound }
    }

    /// Unregisters a connection if it is still the current one.
    pub fn unregister(&self, agent: &AgentId, conn: u64) {
        let mut agents = lock(&self.agents);
        if agents.get(agent).is_some_and(|h| h.conn == conn)
            && let Some(h) = agents.remove(agent)
        {
            fail_all(&h.pending);
            drop(agents);
            self.announce();
        }
    }

    /// Drops every agent connection (they reconnect and re-handshake, for
    /// example after a CRL update). Pending commands fail as unavailable.
    pub fn disconnect_all(&self) -> usize {
        let drained: Vec<Handle> = lock(&self.agents).drain().map(|(_, h)| h).collect();
        for h in &drained {
            fail_all(&h.pending);
        }
        self.announce();
        drained.len()
    }

    /// True if the agent is connected to this or (when clustered) another
    /// replica.
    #[must_use]
    pub fn is_connected(&self, agent: &AgentId) -> bool {
        lock(&self.agents).contains_key(agent)
            || self.cluster.get().is_some_and(|c| c.owner(agent).is_some())
    }

    /// Sends a command and waits for the result, forwarding it to the
    /// replica that holds the agent when it is not connected here.
    pub async fn execute(
        &self,
        agent: &AgentId,
        scanner_id: ScannerId,
        command: ScannerCommand,
        timeout: Duration,
    ) -> CommandOutcome {
        self.request(
            agent,
            AgentOp::Command {
                scanner_id,
                command,
            },
            timeout,
        )
        .await
    }

    /// Runs `op` on the agent, wherever it is connected.
    pub async fn request(&self, agent: &AgentId, op: AgentOp, timeout: Duration) -> CommandOutcome {
        if !lock(&self.agents).contains_key(agent)
            && let Some(c) = self.cluster.get()
            && let Some(owner) = c.owner(agent)
        {
            return c.forward(owner, agent, op, timeout).await;
        }
        self.request_local(agent, op, timeout).await
    }

    /// Runs `op` on an agent connected to this replica.
    pub async fn request_local(
        &self,
        agent: &AgentId,
        op: AgentOp,
        timeout: Duration,
    ) -> CommandOutcome {
        let unavailable = |m: &str| CommandError::Unavailable {
            message: m.to_owned(),
        };
        let state_change_unknown = op.may_change_state();
        let handle = lock(&self.agents)
            .get(agent)
            .cloned()
            .ok_or_else(|| unavailable("agent not connected"))?;
        if handle.protocol < op.min_protocol() {
            return Err(CommandError::Unsupported);
        }
        let request_id = RequestId::new();
        let (tx, rx) = oneshot::channel();
        {
            let mut p = lock(&handle.pending);
            if p.len() >= MAX_PENDING_PER_AGENT {
                return Err(unavailable("agent has too many commands in flight"));
            }
            p.insert(request_id, tx);
        }
        if handle.tx.try_send(op.into_message(request_id)).is_err() {
            lock(&handle.pending).remove(&request_id);
            return Err(unavailable("agent queue full or disconnected"));
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(_)) => Err(unavailable("agent disconnected")),
            Err(_) => {
                lock(&handle.pending).remove(&request_id);
                Err(CommandError::Timeout {
                    state_change_unknown,
                })
            }
        }
    }

    /// Delivers a command result from the agent.
    pub fn complete(&self, agent: &AgentId, request_id: RequestId, outcome: CommandOutcome) {
        let handle = lock(&self.agents).get(agent).cloned();
        if let Some(h) = handle
            && let Some(tx) = lock(&h.pending).remove(&request_id)
        {
            let _ = tx.send(outcome);
        }
    }

    /// Tells a local agent whether anyone is listening to a scanner. Agents
    /// older than protocol 3 have no audio and are skipped.
    pub fn send_audio(&self, agent: &AgentId, scanner_id: ScannerId, listen: bool) -> bool {
        let handle = lock(&self.agents).get(agent).cloned();
        handle.is_some_and(|h| {
            h.protocol >= platform_common::wire::AUDIO_PROTOCOL_VERSION
                && h.tx
                    .try_send(GatewayMessage::Audio { scanner_id, listen })
                    .is_ok()
        })
    }

    /// Pushes new scanner assignments to the agent, wherever it is
    /// connected.
    pub fn configure(&self, agent: &AgentId, scanners: Vec<ScannerAssignment>) -> bool {
        if lock(&self.agents).contains_key(agent) {
            return self.configure_local(agent, scanners);
        }
        self.cluster
            .get()
            .is_some_and(|c| c.configure(agent, scanners))
    }

    /// Pushes assignments to an agent connected to this replica.
    pub fn configure_local(&self, agent: &AgentId, scanners: Vec<ScannerAssignment>) -> bool {
        let handle = lock(&self.agents).get(agent).cloned();
        handle.is_some_and(|h| {
            h.tx.try_send(GatewayMessage::Configure { scanners })
                .is_ok()
        })
    }

    /// Publishes a live update from a local agent (to this replica's
    /// subscribers and, when clustered, to every other replica).
    pub fn publish(&self, event: LiveEvent) {
        if let Some(c) = self.cluster.get() {
            c.publish_live(&event);
        }
        let _ = self.live.send(event);
    }

    /// Publishes to this replica's subscribers only.
    pub fn publish_local(&self, event: LiveEvent) {
        let _ = self.live.send(event);
    }

    /// Subscribes to live updates.
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<LiveEvent> {
        self.live.subscribe()
    }
}

fn fail_all(p: &Pending) {
    for (_, tx) in lock(p).drain() {
        let _ = tx.send(Err(CommandError::Unavailable {
            message: "agent disconnected".into(),
        }));
    }
}
