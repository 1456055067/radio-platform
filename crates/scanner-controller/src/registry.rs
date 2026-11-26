//! Agent-side scanner registry.
//!
//! Reconciles centrally managed [`ScannerAssignment`]s with running SDK
//! clients and telemetry collectors. Every endpoint is checked against the
//! agent's local [`NetworkPolicy`] before any packet is sent, so the agent
//! remains the enforcement point for authorized scanner networks.

use std::collections::HashMap;
use std::net::SocketAddr;

use platform_common::wire::{ScannerAssignment, ScannerReport};
use platform_common::{
    CommandError, CommandOutcome, NetworkPolicy, ScannerCommand, ScannerHealth, ScannerId,
};
use scanner_telemetry::{CollectorConfig, CollectorOutput, run_collector};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use uniden_sds::{Health, Scanner, ScannerConfig};

use crate::mapping;

/// Registry configuration.
#[derive(Debug, Clone)]
pub struct RegistryConfig {
    /// Authorized scanner networks.
    pub policy: NetworkPolicy,
    /// Maximum managed scanners.
    pub max_scanners: usize,
    /// Telemetry collection settings.
    pub collector: CollectorConfig,
    /// Template for SDK settings; `address` is replaced per scanner.
    pub sdk: Option<ScannerConfig>,
}

/// Outcome of reconciling one assignment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyResult {
    /// Started managing.
    Started(ScannerId),
    /// Already running with the same endpoint.
    Unchanged(ScannerId),
    /// Stopped (removed or disabled).
    Stopped(ScannerId),
    /// Refused, with reason.
    Refused(ScannerId, String),
}

struct Managed {
    assignment: ScannerAssignment,
    scanner: Scanner,
    cancel: CancellationToken,
    task: JoinHandle<()>,
    model: Option<String>,
    firmware: Option<String>,
}

/// Registry of managed scanners.
pub struct ScannerRegistry {
    config: RegistryConfig,
    output: mpsc::Sender<CollectorOutput>,
    managed: HashMap<ScannerId, Managed>,
}

impl std::fmt::Debug for ScannerRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScannerRegistry")
            .field("managed", &self.managed.len())
            .finish()
    }
}

fn health(h: Health) -> ScannerHealth {
    match h {
        Health::Unknown => ScannerHealth::Unknown,
        Health::Healthy => ScannerHealth::Healthy,
        Health::Degraded {
            consecutive_failures,
        } => ScannerHealth::Degraded {
            consecutive_failures,
        },
        Health::Unreachable {
            consecutive_failures,
        } => ScannerHealth::Unreachable {
            consecutive_failures,
        },
    }
}

impl ScannerRegistry {
    /// Creates an empty registry. Telemetry and events from all scanners
    /// are sent to `output`.
    #[must_use]
    pub fn new(config: RegistryConfig, output: mpsc::Sender<CollectorOutput>) -> Self {
        Self {
            config,
            output,
            managed: HashMap::new(),
        }
    }

    /// Number of managed scanners.
    #[must_use]
    pub fn len(&self) -> usize {
        self.managed.len()
    }

    /// True if no scanners are managed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.managed.is_empty()
    }

    /// Reconciles to exactly `assignments` (enabled ones are managed; all
    /// others are stopped).
    pub async fn apply(&mut self, assignments: Vec<ScannerAssignment>) -> Vec<ApplyResult> {
        let mut results = Vec::new();
        let wanted: HashMap<ScannerId, ScannerAssignment> = assignments
            .into_iter()
            .filter(|a| a.enabled)
            .map(|a| (a.scanner_id, a))
            .collect();
        let stale: Vec<ScannerId> = self
            .managed
            .iter()
            .filter(|(id, m)| {
                wanted
                    .get(id)
                    .is_none_or(|w| w.host != m.assignment.host || w.port != m.assignment.port)
            })
            .map(|(id, _)| *id)
            .collect();
        for id in stale {
            self.stop(id).await;
            if !wanted.contains_key(&id) {
                results.push(ApplyResult::Stopped(id));
            }
        }
        let mut ids: Vec<_> = wanted.keys().copied().collect();
        ids.sort();
        for id in ids {
            let Some(a) = wanted.get(&id).cloned() else {
                continue;
            };
            if let Some(m) = self.managed.get_mut(&id) {
                m.assignment = a;
                results.push(ApplyResult::Unchanged(id));
                continue;
            }
            results.push(self.start(a).await);
        }
        results
    }

    async fn start(&mut self, a: ScannerAssignment) -> ApplyResult {
        let id = a.scanner_id;
        let endpoint = SocketAddr::new(a.host, a.port);
        if let Err(v) = self.config.policy.check(endpoint) {
            warn!(scanner = %id, %endpoint, reason = %v, "refusing scanner outside network policy");
            return ApplyResult::Refused(id, v.to_string());
        }
        if self.managed.len() >= self.config.max_scanners {
            return ApplyResult::Refused(
                id,
                format!(
                    "agent limit of {} scanners reached",
                    self.config.max_scanners
                ),
            );
        }
        let mut sdk = self
            .config
            .sdk
            .clone()
            .unwrap_or_else(|| ScannerConfig::new(endpoint));
        sdk.address = endpoint;
        let scanner = match Scanner::connect(sdk).await {
            Ok(s) => s,
            Err(e) => return ApplyResult::Refused(id, e.to_string()),
        };
        let (model, firmware) = (
            scanner.model().await.ok().map(|m| m.0),
            scanner.firmware_version().await.ok().map(|f| f.raw),
        );
        let cancel = CancellationToken::new();
        let task = tokio::spawn(run_collector(
            scanner.clone(),
            id,
            a.site_id,
            self.config.collector,
            self.output.clone(),
            cancel.clone(),
        ));
        info!(scanner = %id, %endpoint, ?model, ?firmware, "managing scanner");
        self.managed.insert(
            id,
            Managed {
                assignment: a,
                scanner,
                cancel,
                task,
                model,
                firmware,
            },
        );
        ApplyResult::Started(id)
    }

    async fn stop(&mut self, id: ScannerId) {
        if let Some(m) = self.managed.remove(&id) {
            m.cancel.cancel();
            let _ = m.task.await;
            m.scanner.shutdown().await;
            info!(scanner = %id, "stopped managing scanner");
        }
    }

    /// Stops all scanners.
    pub async fn shutdown(&mut self) {
        let ids: Vec<_> = self.managed.keys().copied().collect();
        for id in ids {
            self.stop(id).await;
        }
    }

    /// Status of every managed scanner.
    #[must_use]
    pub fn reports(&self) -> Vec<ScannerReport> {
        let mut v: Vec<_> = self
            .managed
            .iter()
            .map(|(id, m)| ScannerReport {
                scanner_id: *id,
                health: health(m.scanner.health()),
                model: m.model.clone(),
                firmware: m.firmware.clone(),
            })
            .collect();
        v.sort_by_key(|r| r.scanner_id);
        v
    }

    /// Endpoints of every managed scanner.
    #[must_use]
    pub fn endpoints(&self) -> Vec<std::net::SocketAddr> {
        self.managed
            .values()
            .map(|m| std::net::SocketAddr::new(m.assignment.host, m.assignment.port))
            .collect()
    }

    /// The SDK handle for a scanner (cheap to clone), for executing commands
    /// without holding the registry.
    #[must_use]
    pub fn scanner(&self, id: ScannerId) -> Option<Scanner> {
        self.managed.get(&id).map(|m| m.scanner.clone())
    }

    /// Executes a command on a managed scanner.
    ///
    /// # Errors
    /// [`CommandError::ScannerNotFound`] if the scanner is not managed.
    pub async fn execute(&self, id: ScannerId, command: &ScannerCommand) -> CommandOutcome {
        let scanner = self.scanner(id).ok_or(CommandError::ScannerNotFound)?;
        mapping::execute(&scanner, command).await
    }
}
