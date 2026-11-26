//! Async telemetry collector for one scanner.

use std::time::Duration;

use platform_common::{ScannerEvent, ScannerHealth, ScannerId, SiteId, TelemetrySnapshot, now_utc};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};
use uniden_sds::{Health, Scanner};

use crate::detector::{DetectorConfig, TransmissionDetector};
use crate::snapshot::{materially_different, snapshot_from_info};

/// How telemetry is obtained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollectionMode {
    /// Poll `GSI` at a fixed interval.
    Poll {
        /// Poll interval.
        interval: Duration,
    },
    /// Request `PSI` pushes. If no push arrives within three intervals, the
    /// collector polls once and re-sends `PSI`.
    Push {
        /// Requested push interval in milliseconds.
        interval_ms: u32,
    },
}

/// Collector configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CollectorConfig {
    /// Collection mode.
    pub mode: CollectionMode,
    /// Transmission detector settings.
    pub detector: DetectorConfig,
    /// Send an unchanged snapshot at least this often.
    pub heartbeat: Duration,
}

impl Default for CollectorConfig {
    fn default() -> Self {
        Self {
            mode: CollectionMode::Push { interval_ms: 500 },
            detector: DetectorConfig::default(),
            heartbeat: Duration::from_secs(30),
        }
    }
}

/// Output of a collector.
#[derive(Debug, Clone, PartialEq)]
pub enum CollectorOutput {
    /// Latest telemetry. Dropped if the consumer is slow (latest-value
    /// semantics).
    Telemetry(ScannerId, Box<TelemetrySnapshot>),
    /// A discrete event. Never dropped; the collector waits for capacity.
    Event(ScannerId, Box<ScannerEvent>),
}

fn to_health(h: Health) -> ScannerHealth {
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

struct Run {
    scanner: Scanner,
    scanner_id: ScannerId,
    tx: mpsc::Sender<CollectorOutput>,
    detector: TransmissionDetector,
    last_sent: Option<(TelemetrySnapshot, tokio::time::Instant)>,
    heartbeat: Duration,
}

impl Run {
    async fn event(&self, event: ScannerEvent) -> bool {
        self.tx
            .send(CollectorOutput::Event(self.scanner_id, Box::new(event)))
            .await
            .is_ok()
    }

    async fn process(&mut self, snapshot: TelemetrySnapshot) -> bool {
        for e in self.detector.observe(&snapshot) {
            if !self.event(e).await {
                return false;
            }
        }
        let due = match &self.last_sent {
            None => true,
            Some((prev, at)) => {
                materially_different(prev, &snapshot) || at.elapsed() >= self.heartbeat
            }
        };
        if due {
            self.last_sent = Some((snapshot.clone(), tokio::time::Instant::now()));
            match self.tx.try_send(CollectorOutput::Telemetry(
                self.scanner_id,
                Box::new(snapshot),
            )) {
                Ok(()) | Err(mpsc::error::TrySendError::Full(_)) => {}
                Err(mpsc::error::TrySendError::Closed(_)) => return false,
            }
        }
        true
    }

    async fn poll_once(&mut self) -> bool {
        match self.scanner.scanner_info().await {
            Ok(info) => self.process(snapshot_from_info(&info, now_utc())).await,
            Err(e) => {
                debug!(scanner = %self.scanner_id, error = %e, "telemetry poll failed");
                true
            }
        }
    }

    async fn health_changed(&mut self, h: Health) -> bool {
        if matches!(h, Health::Unreachable { .. })
            && let Some(e) = self.detector.flush()
            && !self.event(e).await
        {
            return false;
        }
        self.event(ScannerEvent::HealthChanged {
            health: to_health(h),
        })
        .await
    }
}

/// Runs a collector until `cancel` fires or the output channel closes.
pub async fn run_collector(
    scanner: Scanner,
    scanner_id: ScannerId,
    site_id: Option<SiteId>,
    config: CollectorConfig,
    tx: mpsc::Sender<CollectorOutput>,
    cancel: CancellationToken,
) {
    let mut health = scanner.watch_health();
    let mut pushes = scanner.subscribe();
    let mut run = Run {
        detector: TransmissionDetector::new(scanner_id, site_id, config.detector),
        scanner,
        scanner_id,
        tx,
        last_sent: None,
        heartbeat: config.heartbeat,
    };
    let (poll_every, push_ms) = match config.mode {
        CollectionMode::Poll { interval } => (interval, None),
        CollectionMode::Push { interval_ms } => (
            Duration::from_millis(u64::from(interval_ms.max(100)) * 3),
            Some(interval_ms),
        ),
    };
    let arm_push = |s: Scanner| async move {
        if let Some(ms) = push_ms
            && let Err(e) = s.start_push(ms).await
        {
            warn!(error = %e, "PSI request failed; continuing with polling fallback");
        }
    };
    arm_push(run.scanner.clone()).await;
    let mut deadline = tokio::time::Instant::now() + poll_every;
    loop {
        tokio::select! {
            biased;
            () = cancel.cancelled() => break,
            changed = health.changed() => {
                if changed.is_err() { break; }
                let h = *health.borrow_and_update();
                if !run.health_changed(h).await { break; }
            }
            push = pushes.recv(), if push_ms.is_some() => {
                match push {
                    Ok(info) => {
                        deadline = tokio::time::Instant::now() + poll_every;
                        if !run.process(snapshot_from_info(&info, now_utc())).await { break; }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        debug!(skipped = n, "collector lagged behind pushes");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
            () = tokio::time::sleep_until(deadline) => {
                deadline = tokio::time::Instant::now() + poll_every;
                if !run.poll_once().await { break; }
                if push_ms.is_some() {
                    arm_push(run.scanner.clone()).await;
                }
            }
        }
    }
    if let Some(e) = run.detector.flush() {
        let _ = run.event(e).await;
    }
    if push_ms.is_some() {
        // Best effort: stop pushes on shutdown.
        let _ = tokio::time::timeout(Duration::from_secs(1), run.scanner.stop_push()).await;
    }
}
