//! Retention sweeps (ADR 0010).
//!
//! Recordings are deleted from storage first and detached second (only if
//! the key still matches and the transmission is not held), so a crash in
//! between leaves a dangling key that the next sweep cleans up. Sweeps on
//! several replicas at once are harmless.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use platform_common::TenantId;
use platform_storage::Store;
use platform_storage::model::{AuditOutcome, NewAudit, TenantCounts};
use scanner_recorder::RecordingStore;
use serde::Serialize;
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use utoipa::ToSchema;

use crate::config::RetentionConfig;

/// Rows per store call.
const BATCH: u32 = 500;
/// Store calls per step per sweep (bounds one sweep's work).
const MAX_BATCHES: usize = 40;

/// What one sweep removed (this replica).
#[derive(Debug, Clone, Default, Serialize, ToSchema)]
pub struct SweepReport {
    /// When it finished.
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub at: Option<OffsetDateTime>,
    /// Recordings deleted.
    pub recordings: u64,
    /// Transcripts removed.
    pub transcripts: u64,
    /// Transmissions deleted.
    pub transmissions: u64,
    /// Events deleted.
    pub events: u64,
    /// Errors (details in the log).
    pub errors: u64,
}

#[derive(Default)]
struct PerTenant {
    recordings: u64,
    transcripts: u64,
    transmissions: u64,
    events: u64,
}

/// Retention on this replica.
#[derive(Debug)]
pub struct Retention {
    /// Policy.
    pub cfg: RetentionConfig,
    last: Mutex<Option<SweepReport>>,
}

fn days(d: Option<u32>) -> Option<time::Duration> {
    d.map(|d| time::Duration::days(i64::from(d)))
}

fn min_opt(a: Option<time::Duration>, b: Option<time::Duration>) -> Option<time::Duration> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.min(y)),
        (x, None) => x,
        (None, y) => y,
    }
}

fn add(
    m: &mut BTreeMap<TenantId, PerTenant>,
    counts: &TenantCounts,
    f: fn(&mut PerTenant) -> &mut u64,
) -> u64 {
    let mut total = 0;
    for (t, n) in counts {
        *f(m.entry(*t).or_default()) += n;
        total += n;
    }
    total
}

impl Retention {
    /// Creates the sweeper.
    #[must_use]
    pub fn new(cfg: RetentionConfig) -> Self {
        Self {
            cfg,
            last: Mutex::new(None),
        }
    }

    /// The last sweep on this replica.
    #[must_use]
    pub fn last(&self) -> Option<SweepReport> {
        self.last
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Runs one sweep.
    pub async fn sweep(
        &self,
        db: &dyn Store,
        recordings: Option<&RecordingStore>,
        now: OffsetDateTime,
    ) -> SweepReport {
        let tx = days(self.cfg.transmissions_days);
        let rec_age = min_opt(days(self.cfg.recordings_days), tx);
        let txt_age = min_opt(days(self.cfg.transcripts_days), tx);
        let mut report = SweepReport::default();
        let mut per: BTreeMap<TenantId, PerTenant> = BTreeMap::new();

        if let (Some(age), Some(store)) = (rec_age, recordings) {
            'batches: for _ in 0..MAX_BATCHES {
                let batch = match db.expired_recordings(now - age, BATCH).await {
                    Ok(b) => b,
                    Err(e) => {
                        warn!(error = %e, "retention: listing recordings failed");
                        report.errors += 1;
                        break;
                    }
                };
                if batch.is_empty() {
                    break;
                }
                let mut progressed = false;
                for r in &batch {
                    if let Err(e) = store.delete(&r.audio_object).await {
                        warn!(transmission = %r.transmission_id, error = %e, "retention: deleting recording failed");
                        report.errors += 1;
                        continue;
                    }
                    match db
                        .clear_transmission_audio(
                            r.tenant_id,
                            r.transmission_id,
                            &r.audio_object,
                            false,
                        )
                        .await
                    {
                        Ok(true) => {
                            progressed = true;
                            report.recordings += 1;
                            per.entry(r.tenant_id).or_default().recordings += 1;
                        }
                        Ok(false) => {}
                        Err(e) => {
                            warn!(error = %e, "retention: detaching recording failed");
                            report.errors += 1;
                        }
                    }
                }
                if !progressed {
                    break 'batches;
                }
            }
        } else if rec_age.is_some() {
            warn!(
                "retention: recording limits set but recording storage is not configured; recordings are kept"
            );
        }

        type Step<'a> = (
            Option<time::Duration>,
            &'a str,
            fn(&mut PerTenant) -> &mut u64,
        );
        let steps: [Step<'_>; 3] = [
            (txt_age, "transcripts", |p| &mut p.transcripts),
            (tx, "transmissions", |p| &mut p.transmissions),
            (days(self.cfg.events_days), "events", |p| &mut p.events),
        ];
        for (age, what, field) in steps {
            let Some(age) = age else { continue };
            for _ in 0..MAX_BATCHES {
                let r = match what {
                    "transcripts" => db.expire_transcripts(now - age, BATCH).await,
                    "transmissions" => db.delete_transmissions_before(now - age, BATCH).await,
                    _ => db.delete_events_before(now - age, BATCH).await,
                };
                match r {
                    Ok(c) if c.is_empty() => break,
                    Ok(c) => {
                        let n = add(&mut per, &c, field);
                        match what {
                            "transcripts" => report.transcripts += n,
                            "transmissions" => report.transmissions += n,
                            _ => report.events += n,
                        }
                    }
                    Err(e) => {
                        warn!(error = %e, what, "retention step failed");
                        report.errors += 1;
                        break;
                    }
                }
            }
        }

        for (tenant, p) in &per {
            let rec = NewAudit {
                tenant_id: *tenant,
                actor: "system:retention".into(),
                action: "retention.sweep".into(),
                resource_type: "tenant".into(),
                resource_id: Some(tenant.to_string()),
                outcome: AuditOutcome::Success,
                details: serde_json::json!({
                    "recordings": p.recordings,
                    "transcripts": p.transcripts,
                    "transmissions": p.transmissions,
                    "events": p.events,
                    "policy": {
                        "recordings_days": self.cfg.recordings_days,
                        "transcripts_days": self.cfg.transcripts_days,
                        "transmissions_days": self.cfg.transmissions_days,
                        "events_days": self.cfg.events_days,
                    },
                }),
                source_ip: None,
            };
            if let Err(e) = db.append_audit(rec).await {
                warn!(error = %e, "retention: audit record not written");
            }
        }
        report.at = Some(now);
        if report.recordings + report.transcripts + report.transmissions + report.events > 0 {
            info!(
                recordings = report.recordings,
                transcripts = report.transcripts,
                transmissions = report.transmissions,
                events = report.events,
                "retention sweep"
            );
        }
        *self
            .last
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(report.clone());
        report
    }

    /// Sweeps every `sweep_interval_secs` (first after one minute) until
    /// `cancel`.
    pub async fn run(
        self: Arc<Self>,
        db: Arc<dyn Store>,
        recordings: Option<RecordingStore>,
        cancel: CancellationToken,
    ) {
        info!(policy = ?self.cfg, "retention enabled");
        let mut wait = Duration::from_secs(60);
        loop {
            tokio::select! {
                () = cancel.cancelled() => return,
                () = tokio::time::sleep(wait) => {}
            }
            self.sweep(&*db, recordings.as_ref(), platform_common::now_utc())
                .await;
            wait = Duration::from_secs(self.cfg.sweep_interval_secs);
        }
    }
}
