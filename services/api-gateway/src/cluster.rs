//! Running several gateway replicas (ADR 0008).
//!
//! An agent holds one WebSocket to one replica, but REST and live-stream
//! clients may arrive at any replica. Replicas coordinate over a
//! [`MessageBus`]:
//! - **Presence.** Every replica announces its connected agents every 5 s
//!   and on each connect or disconnect. Entries expire after 15 s.
//! - **Commands.** A replica without the agent forwards the command to the
//!   owner and waits for the outcome. Authorization, lease checks and audit
//!   happen on the replica that received the request.
//! - **Live updates.** Events and telemetry from a replica's agents are
//!   re-published to every other replica's WebSocket subscribers.
//! - **Configuration.** Assignment changes reach the agent wherever it is
//!   connected.
//!
//! Leases and all records live in the shared database; nothing else is
//! replicated. The bus is trusted: anyone able to publish on it (for
//! PostgreSQL, anyone with the gateway's database credentials) can send
//! commands to agents, which is no more than those credentials already
//! allow.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use platform_common::wire::ScannerAssignment;
use platform_common::{
    AgentId, CommandError, CommandOutcome, RequestId, ScannerId, SiteId, TenantId,
};
use platform_storage::MessageBus;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, warn};

use crate::hub::{AgentHub, AgentOp, LiveEvent, LiveKind};

/// Presence announcement interval.
pub const PRESENCE_EVERY: Duration = Duration::from_secs(5);
/// Presence entries older than this are ignored.
pub const PRESENCE_TTL: Duration = Duration::from_secs(15);
/// Time the owner keeps for itself out of a forwarded command's timeout,
/// so its outcome arrives before the origin gives up.
const FORWARD_MARGIN: Duration = Duration::from_millis(500);
const OUTBOUND_QUEUE: usize = 4096;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Envelope {
    from: uuid::Uuid,
    #[serde(flatten)]
    msg: Msg,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
enum Msg {
    Live {
        tenant_id: TenantId,
        site_id: SiteId,
        scanner_id: ScannerId,
        kind: LiveKind,
        data: serde_json::Value,
    },
    Execute {
        to: uuid::Uuid,
        request_id: RequestId,
        agent: AgentId,
        op: AgentOp,
        timeout_ms: u64,
    },
    Outcome {
        to: uuid::Uuid,
        request_id: RequestId,
        outcome: CommandOutcome,
    },
    Configure {
        agent: AgentId,
        scanners: Vec<ScannerAssignment>,
    },
    Presence {
        agents: Vec<AgentId>,
    },
    Listen {
        to: uuid::Uuid,
        request_id: RequestId,
        agent: AgentId,
        offer: String,
        meta: crate::media::SessionMeta,
    },
    ListenResult {
        to: uuid::Uuid,
        request_id: RequestId,
        result: Result<(uuid::Uuid, String), crate::media::ListenError>,
    },
    Hangup {
        session: uuid::Uuid,
        tenant_id: TenantId,
        subject: String,
        admin: bool,
    },
}

/// This replica's view of the cluster.
#[derive(Debug)]
pub struct Cluster {
    me: uuid::Uuid,
    out: mpsc::Sender<Envelope>,
    remote: Mutex<HashMap<uuid::Uuid, (HashSet<AgentId>, Instant)>>,
    pending: Mutex<HashMap<RequestId, oneshot::Sender<CommandOutcome>>>,
    listens: Mutex<HashMap<RequestId, oneshot::Sender<ListenResult>>>,
}

type ListenResult = Result<(uuid::Uuid, String), crate::media::ListenError>;

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Cluster {
    /// Joins the bus. Spawns the publisher, the receiver and the presence
    /// heartbeat; they run for the life of the process.
    pub fn join(bus: Arc<dyn MessageBus>, hub: AgentHub) -> Arc<Self> {
        let (out, mut rx) = mpsc::channel::<Envelope>(OUTBOUND_QUEUE);
        let me = uuid::Uuid::new_v4();
        let cluster = Arc::new(Self {
            me,
            out,
            remote: Mutex::new(HashMap::new()),
            pending: Mutex::new(HashMap::new()),
            listens: Mutex::new(HashMap::new()),
        });
        // Publisher: one task keeps per-replica ordering.
        let pub_bus = Arc::clone(&bus);
        tokio::spawn(async move {
            while let Some(env) = rx.recv().await {
                match serde_json::to_string(&env) {
                    Ok(s) => {
                        if let Err(e) = pub_bus.publish(&s).await {
                            warn!(error = %e, "cluster publish failed");
                        }
                    }
                    Err(e) => warn!(error = %e, "cluster message not serializable"),
                }
            }
        });
        // Receiver.
        let mut sub = bus.subscribe();
        let (c, h) = (Arc::clone(&cluster), hub.clone());
        tokio::spawn(async move {
            loop {
                match sub.recv().await {
                    Ok(p) => c.handle(&p, &h),
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        warn!(skipped = n, "cluster receiver lagged");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
        // Presence heartbeat.
        let (c, h) = (Arc::clone(&cluster), hub);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(PRESENCE_EVERY);
            loop {
                tick.tick().await;
                c.announce(h.local_agents());
            }
        });
        cluster
    }

    /// This replica's id.
    #[must_use]
    pub fn id(&self) -> uuid::Uuid {
        self.me
    }

    fn send(&self, msg: Msg) -> bool {
        let ok = self.out.try_send(Envelope { from: self.me, msg }).is_ok();
        if !ok {
            warn!("cluster outbound queue full; message dropped");
        }
        ok
    }

    /// Announces this replica's agents now.
    pub fn announce(&self, agents: Vec<AgentId>) {
        self.send(Msg::Presence { agents });
    }

    /// The replica currently holding `agent`, if any other replica does.
    #[must_use]
    pub fn owner(&self, agent: &AgentId) -> Option<uuid::Uuid> {
        let now = Instant::now();
        lock(&self.remote)
            .iter()
            .filter(|(_, (agents, at))| {
                now.duration_since(*at) < PRESENCE_TTL && agents.contains(agent)
            })
            .max_by_key(|(_, (_, at))| *at)
            .map(|(id, _)| *id)
    }

    /// Re-publishes a live update from a local agent to other replicas.
    pub fn publish_live(&self, e: &LiveEvent) {
        self.send(Msg::Live {
            tenant_id: e.tenant_id,
            site_id: e.site_id,
            scanner_id: e.scanner_id,
            kind: e.kind,
            data: e.data.clone(),
        });
    }

    /// Sends assignments to an agent held by another replica.
    pub fn configure(&self, agent: &AgentId, scanners: Vec<ScannerAssignment>) -> bool {
        self.owner(agent).is_some()
            && self.send(Msg::Configure {
                agent: agent.clone(),
                scanners,
            })
    }

    /// Forwards a command to the replica holding the agent.
    pub async fn forward(
        &self,
        to: uuid::Uuid,
        agent: &AgentId,
        op: AgentOp,
        timeout: Duration,
    ) -> CommandOutcome {
        let state_change_unknown = op.may_change_state();
        let request_id = RequestId::new();
        let (tx, rx) = oneshot::channel();
        lock(&self.pending).insert(request_id, tx);
        let sent = self.send(Msg::Execute {
            to,
            request_id,
            agent: agent.clone(),
            op,
            timeout_ms: u64::try_from(timeout.saturating_sub(FORWARD_MARGIN).as_millis())
                .unwrap_or(u64::MAX),
        });
        if !sent {
            lock(&self.pending).remove(&request_id);
            return Err(CommandError::Unavailable {
                message: "cluster bus congested".into(),
            });
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(_)) => Err(CommandError::Unavailable {
                message: "cluster request dropped".into(),
            }),
            Err(_) => {
                lock(&self.pending).remove(&request_id);
                Err(CommandError::Timeout {
                    state_change_unknown,
                })
            }
        }
    }

    /// Asks the replica holding the agent to create a listening session.
    pub async fn forward_listen(
        &self,
        to: uuid::Uuid,
        agent: &AgentId,
        offer: &str,
        meta: crate::media::SessionMeta,
        timeout: Duration,
    ) -> ListenResult {
        use crate::media::ListenError;
        let request_id = RequestId::new();
        let (tx, rx) = oneshot::channel();
        lock(&self.listens).insert(request_id, tx);
        if !self.send(Msg::Listen {
            to,
            request_id,
            agent: agent.clone(),
            offer: offer.to_owned(),
            meta,
        }) {
            lock(&self.listens).remove(&request_id);
            return Err(ListenError::Busy {
                message: "cluster bus congested".into(),
            });
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(r)) => r,
            _ => {
                lock(&self.listens).remove(&request_id);
                Err(ListenError::AgentOffline)
            }
        }
    }

    /// Asks every replica to end a session (only its owner acts).
    pub fn hangup(&self, session: uuid::Uuid, tenant_id: TenantId, subject: &str, admin: bool) {
        self.send(Msg::Hangup {
            session,
            tenant_id,
            subject: subject.to_owned(),
            admin,
        });
    }

    fn handle(self: &Arc<Self>, payload: &str, hub: &AgentHub) {
        let env: Envelope = match serde_json::from_str(payload) {
            Ok(e) => e,
            Err(e) => {
                debug!(error = %e, "ignoring malformed cluster message");
                return;
            }
        };
        if env.from == self.me {
            return;
        }
        match env.msg {
            Msg::Live {
                tenant_id,
                site_id,
                scanner_id,
                kind,
                data,
            } => hub.publish_local(LiveEvent {
                tenant_id,
                site_id,
                scanner_id,
                kind,
                data,
            }),
            Msg::Presence { agents } => {
                lock(&self.remote).insert(env.from, (agents.into_iter().collect(), Instant::now()));
            }
            Msg::Configure { agent, scanners } => {
                hub.configure_local(&agent, scanners);
            }
            Msg::Execute {
                to,
                request_id,
                agent,
                op,
                timeout_ms,
            } if to == self.me => {
                let (c, h) = (Arc::clone(self), hub.clone());
                tokio::spawn(async move {
                    let outcome = h
                        .request_local(&agent, op, Duration::from_millis(timeout_ms))
                        .await;
                    c.send(Msg::Outcome {
                        to: env.from,
                        request_id,
                        outcome,
                    });
                });
            }
            Msg::Outcome {
                to,
                request_id,
                outcome,
            } if to == self.me => {
                if let Some(tx) = lock(&self.pending).remove(&request_id) {
                    let _ = tx.send(outcome);
                }
            }
            Msg::Listen {
                to,
                request_id,
                agent,
                offer,
                meta,
            } if to == self.me => {
                let (c, h) = (Arc::clone(self), hub.clone());
                tokio::spawn(async move {
                    let result = match h.media() {
                        Some(m) => m.listen(&agent, &offer, meta).await,
                        None => Err(crate::media::ListenError::Disabled),
                    };
                    c.send(Msg::ListenResult {
                        to: env.from,
                        request_id,
                        result,
                    });
                });
            }
            Msg::ListenResult {
                to,
                request_id,
                result,
            } if to == self.me => {
                if let Some(tx) = lock(&self.listens).remove(&request_id) {
                    let _ = tx.send(result);
                }
            }
            Msg::Hangup {
                session,
                tenant_id,
                subject,
                admin,
            } => {
                if let Some(m) = hub.media() {
                    let _ = m.hangup(session, tenant_id, &subject, admin);
                }
            }
            Msg::Execute { .. }
            | Msg::Outcome { .. }
            | Msg::Listen { .. }
            | Msg::ListenResult { .. } => {}
        }
    }
}
