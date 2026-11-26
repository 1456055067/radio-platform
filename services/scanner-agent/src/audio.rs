//! Per-scanner audio pipelines on the agent.
//!
//! The agent is the **only** RTSP client of each scanner (the audio server
//! reportedly accepts one client and is fragile; see
//! `docs/protocol-compliance.md`). For every scanner whose assignment
//! enables audio, a worker:
//! - streams only while wanted: connected to the gateway **and** (a live
//!   listener exists **or** the scanner records);
//! - checks the RTSP endpoint against the agent's **local** network policy;
//! - spaces sessions with a cooldown and backs off after failures;
//! - lingers briefly after demand ends, so listeners coming and going do
//!   not churn sessions;
//! - forwards Opus frames to the upstream session and reports its state.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use platform_common::wire::media::MediaFrame;
use platform_common::wire::{AudioAssignment, AudioState, ScannerAssignment};
use platform_common::{NetworkPolicy, ScannerId};
use scanner_audio::{RtpStats, SessionConfig, SessionEnd, Supervisor, run_session};
use serde::Deserialize;
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

/// Agent audio settings.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct AudioSettings {
    /// Minimum seconds between RTSP sessions to one scanner.
    pub cooldown_secs: u64,
    /// Maximum backoff after repeated failures, in seconds.
    pub backoff_max_secs: u64,
    /// Keep streaming this long after the last listener leaves.
    pub linger_secs: u64,
}

impl Default for AudioSettings {
    fn default() -> Self {
        Self {
            cooldown_secs: 10,
            backoff_max_secs: 300,
            linger_secs: 15,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Demand {
    connected: bool,
    listen: bool,
    record: bool,
}

impl Demand {
    fn wanted(self) -> bool {
        self.connected && (self.listen || self.record)
    }
}

struct Worker {
    assignment: AudioAssignment,
    host: std::net::IpAddr,
    demand: watch::Sender<Demand>,
    cancel: CancellationToken,
    task: tokio::task::JoinHandle<()>,
}

/// All audio workers of this agent.
pub struct AudioManager {
    policy: NetworkPolicy,
    settings: AudioSettings,
    workers: HashMap<ScannerId, Worker>,
    connected: bool,
    media: mpsc::Sender<MediaFrame>,
    states: mpsc::Sender<(ScannerId, AudioState)>,
}

impl std::fmt::Debug for AudioManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioManager")
            .field("workers", &self.workers.len())
            .finish()
    }
}

impl AudioManager {
    /// Creates a manager sending frames to `media` and state changes to
    /// `states`.
    #[must_use]
    pub fn new(
        policy: NetworkPolicy,
        settings: AudioSettings,
        media: mpsc::Sender<MediaFrame>,
        states: mpsc::Sender<(ScannerId, AudioState)>,
    ) -> Self {
        Self {
            policy,
            settings,
            workers: HashMap::new(),
            connected: false,
            media,
            states,
        }
    }

    /// Reconciles workers with the assignments.
    pub async fn apply(&mut self, assignments: &[ScannerAssignment]) {
        let wanted: HashMap<ScannerId, (&ScannerAssignment, AudioAssignment)> = assignments
            .iter()
            .filter(|a| a.enabled)
            .filter_map(|a| a.audio.map(|au| (a.scanner_id, (a, au))))
            .collect();
        let stale: Vec<ScannerId> = self
            .workers
            .iter()
            .filter(|(id, w)| {
                wanted.get(id).is_none_or(|(a, au)| {
                    a.host != w.host || au.rtsp_port != w.assignment.rtsp_port
                })
            })
            .map(|(id, _)| *id)
            .collect();
        for id in stale {
            self.stop(id).await;
        }
        for (id, (a, au)) in wanted {
            if let Some(w) = self.workers.get_mut(&id) {
                w.assignment = au;
                w.demand.send_modify(|d| d.record = au.record);
                continue;
            }
            let endpoint = SocketAddr::new(a.host, au.rtsp_port);
            if let Err(v) = self.policy.check_audio(endpoint) {
                warn!(scanner = %id, %endpoint, reason = %v, "refusing audio outside network policy");
                let _ = self.states.try_send((
                    id,
                    AudioState::Failed {
                        message: format!("audio endpoint refused by agent policy: {v}"),
                    },
                ));
                continue;
            }
            let (demand, rx) = watch::channel(Demand {
                connected: self.connected,
                listen: false,
                record: au.record,
            });
            let cancel = CancellationToken::new();
            let task = tokio::spawn(worker(
                id,
                endpoint,
                self.settings.clone(),
                rx,
                cancel.clone(),
                self.media.clone(),
                self.states.clone(),
            ));
            self.workers.insert(
                id,
                Worker {
                    assignment: au,
                    host: a.host,
                    demand,
                    cancel,
                    task,
                },
            );
        }
    }

    /// Sets live-listener demand for a scanner.
    pub fn set_listen(&self, id: ScannerId, listen: bool) {
        if let Some(w) = self.workers.get(&id) {
            w.demand.send_modify(|d| d.listen = listen);
        }
    }

    /// Records whether the gateway connection is up. Going down clears all
    /// listener demand (the gateway re-sends it after reconnecting).
    pub fn set_connected(&mut self, connected: bool) {
        self.connected = connected;
        for w in self.workers.values() {
            w.demand.send_modify(|d| {
                d.connected = connected;
                if !connected {
                    d.listen = false;
                }
            });
        }
    }

    async fn stop(&mut self, id: ScannerId) {
        if let Some(w) = self.workers.remove(&id) {
            w.cancel.cancel();
            let _ = w.task.await;
        }
    }

    /// Stops every worker (each tears down its session).
    pub async fn shutdown(&mut self) {
        let ids: Vec<_> = self.workers.keys().copied().collect();
        for id in ids {
            self.stop(id).await;
        }
    }
}

async fn wait_for(
    rx: &mut watch::Receiver<Demand>,
    cancel: &CancellationToken,
    want: bool,
) -> bool {
    loop {
        if rx.borrow_and_update().wanted() == want {
            return true;
        }
        tokio::select! {
            () = cancel.cancelled() => return false,
            r = rx.changed() => if r.is_err() { return false },
        }
    }
}

async fn worker(
    id: ScannerId,
    endpoint: SocketAddr,
    settings: AudioSettings,
    mut demand: watch::Receiver<Demand>,
    cancel: CancellationToken,
    media: mpsc::Sender<MediaFrame>,
    states: mpsc::Sender<(ScannerId, AudioState)>,
) {
    let report = |s: AudioState| {
        let _ = states.try_send((id, s));
    };
    let mut sup = Supervisor::new(
        Duration::from_secs(settings.cooldown_secs.max(1)),
        Duration::from_secs(settings.backoff_max_secs.max(settings.cooldown_secs)),
    );
    let linger = Duration::from_secs(settings.linger_secs);
    let mut seq: u32 = 0;
    report(AudioState::Idle);
    loop {
        if !wait_for(&mut demand, &cancel, true).await {
            return;
        }
        let wait = sup.wait(Instant::now());
        if !wait.is_zero() {
            report(AudioState::Cooldown {
                retry_in_secs: wait.as_secs().max(1),
            });
            tokio::select! {
                () = cancel.cancelled() => return,
                () = tokio::time::sleep(wait) => {}
            }
            if !demand.borrow().wanted() {
                report(AudioState::Idle);
                continue;
            }
        }
        report(AudioState::Starting);
        info!(scanner = %id, %endpoint, "starting audio session");
        let stop = cancel.child_token();
        let (ftx, mut frx) = mpsc::channel(256);
        let (stx, srx) = watch::channel(RtpStats::default());
        let cfg = SessionConfig::new(endpoint);
        let session = run_session(&cfg, &ftx, &stx, &stop);
        tokio::pin!(session);
        let mut linger_deadline: Option<tokio::time::Instant> = None;
        let mut tick = tokio::time::interval(Duration::from_secs(2));
        let mut streaming = false;
        let end = loop {
            tokio::select! {
                end = &mut session => break end,
                Some(f) = frx.recv() => {
                    seq = seq.wrapping_add(1);
                    let _ = media.try_send(MediaFrame {
                        scanner_id: id,
                        captured_ms: f.captured_ms,
                        seq,
                        concealed: f.frame.concealed,
                        opus: f.frame.data,
                    });
                    if !streaming {
                        streaming = true;
                        report(AudioState::Streaming { received: 0, lost: 0 });
                    }
                }
                r = demand.changed() => {
                    if r.is_err() { stop.cancel(); continue; }
                    let d = *demand.borrow_and_update();
                    if !d.connected {
                        stop.cancel(); // nobody to send to: tear down now
                    } else if d.wanted() {
                        linger_deadline = None;
                    } else if linger_deadline.is_none() {
                        linger_deadline = Some(tokio::time::Instant::now() + linger);
                    }
                }
                () = async { if let Some(d) = linger_deadline { tokio::time::sleep_until(d).await } else { std::future::pending().await } } => {
                    stop.cancel();
                }
                _ = tick.tick() => {
                    if streaming {
                        let s = *srx.borrow();
                        report(AudioState::Streaming { received: s.received, lost: s.lost });
                    }
                }
            }
        };
        sup.ended(&end, Instant::now());
        match &end {
            SessionEnd::Stopped => {
                info!(scanner = %id, "audio session stopped");
                report(AudioState::Idle);
            }
            SessionEnd::Failed(e) => {
                warn!(scanner = %id, error = %e, failures = sup.failures(), "audio session failed");
                report(AudioState::Failed { message: e.clone() });
            }
        }
        if cancel.is_cancelled() {
            return;
        }
    }
}
