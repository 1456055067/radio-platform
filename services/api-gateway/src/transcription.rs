//! Transcription worker (ADR 0010).
//!
//! Every replica with `[transcription]` runs one worker. Jobs live on the
//! transmission rows; replicas claim them with a lease, so work spreads
//! across replicas and survives restarts. Transient failures are retried
//! with backoff (this is not a scanner command, so retrying is safe).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use platform_storage::Store;
use platform_storage::model::{TranscriptionJob, TranscriptionResult};
use scanner_recorder::RecordingStore;
use scanner_transcription::{TranscriptionConfig, WhisperClient};
use serde::Serialize;
use tokio::sync::{Notify, Semaphore};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};
use utoipa::ToSchema;

/// Longest recording decoded, in seconds.
const MAX_SECS: u32 = 1800;

/// Worker counters (this replica, since start).
#[derive(Debug, Clone, Copy, Default, Serialize, ToSchema)]
pub struct WorkerStats {
    /// Transcribed.
    pub completed: u64,
    /// Scheduled for another attempt.
    pub retried: u64,
    /// Gave up.
    pub failed: u64,
    /// Not sent (too short).
    pub skipped: u64,
    /// Requests in progress.
    pub in_flight: u64,
}

/// Transcription on this replica.
#[derive(Debug)]
pub struct Transcription {
    /// Service client.
    pub client: WhisperClient,
    /// Settings.
    pub cfg: TranscriptionConfig,
    stats: Mutex<WorkerStats>,
}

/// Delay before attempt `attempts + 1`: 30 s doubling, at most 30 min.
#[must_use]
pub fn backoff(attempts: u32) -> time::Duration {
    let secs = 30u64.saturating_mul(1u64 << attempts.saturating_sub(1).min(10));
    time::Duration::seconds(i64::try_from(secs.min(1800)).unwrap_or(1800))
}

impl Transcription {
    /// Builds the client.
    ///
    /// # Errors
    /// Invalid configuration.
    pub fn new(cfg: TranscriptionConfig) -> Result<Self, scanner_transcription::ConfigError> {
        Ok(Self {
            client: WhisperClient::new(&cfg)?,
            cfg,
            stats: Mutex::new(WorkerStats::default()),
        })
    }

    /// Counters.
    #[must_use]
    pub fn stats(&self) -> WorkerStats {
        *self
            .stats
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn count(&self, f: impl FnOnce(&mut WorkerStats)) {
        f(&mut self
            .stats
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner));
    }

    /// Runs the worker until `cancel`.
    pub async fn run(
        self: Arc<Self>,
        db: Arc<dyn Store>,
        recordings: RecordingStore,
        wake: Arc<Notify>,
        cancel: CancellationToken,
    ) {
        info!(service = %self.client.describe(), concurrency = self.cfg.concurrency, "transcription worker started");
        let slots = Arc::new(Semaphore::new(self.cfg.concurrency));
        let lease =
            time::Duration::seconds(i64::try_from(self.cfg.timeout_secs).unwrap_or(120) + 60);
        loop {
            let free = slots.available_permits();
            let mut claimed = 0;
            if free > 0 {
                let now = platform_common::now_utc();
                match db
                    .claim_transcriptions(now, now + lease, u32::try_from(free).unwrap_or(1))
                    .await
                {
                    Ok(jobs) => {
                        claimed = jobs.len();
                        for job in jobs {
                            let Ok(permit) = Arc::clone(&slots).acquire_owned().await else {
                                return;
                            };
                            let (me, db, rec, wake) = (
                                Arc::clone(&self),
                                Arc::clone(&db),
                                recordings.clone(),
                                Arc::clone(&wake),
                            );
                            tokio::spawn(async move {
                                me.process(&*db, &rec, job).await;
                                drop(permit);
                                wake.notify_one();
                            });
                        }
                    }
                    Err(e) => warn!(error = %e, "could not claim transcription jobs"),
                }
            }
            if claimed > 0 && claimed == free {
                continue; // more may be waiting
            }
            tokio::select! {
                () = cancel.cancelled() => return,
                () = wake.notified() => {}
                () = tokio::time::sleep(Duration::from_secs(5)) => {}
            }
        }
    }

    async fn process(&self, db: &dyn Store, recordings: &RecordingStore, job: TranscriptionJob) {
        self.count(|s| s.in_flight += 1);
        let result = self.attempt(recordings, &job).await;
        self.count(|s| {
            s.in_flight = s.in_flight.saturating_sub(1);
            match &result {
                TranscriptionResult::Done { .. } => s.completed += 1,
                TranscriptionResult::Retry { .. } => s.retried += 1,
                TranscriptionResult::Failed { .. } => s.failed += 1,
                TranscriptionResult::Skipped { .. } => s.skipped += 1,
            }
        });
        match &result {
            TranscriptionResult::Retry { error, .. } | TranscriptionResult::Failed { error } => {
                warn!(transmission = %job.transmission_id, attempt = job.attempts, error = %error, "transcription failed");
            }
            _ => debug!(transmission = %job.transmission_id, "transcription finished"),
        }
        if let Err(e) = db
            .finish_transcription(
                job.tenant_id,
                job.transmission_id,
                result,
                platform_common::now_utc(),
            )
            .await
        {
            // The claim lease expires and another attempt follows.
            warn!(transmission = %job.transmission_id, error = %e, "could not record transcription result");
        }
    }

    fn retry_or_fail(&self, job: &TranscriptionJob, error: String) -> TranscriptionResult {
        if job.attempts < self.cfg.max_attempts {
            TranscriptionResult::Retry {
                error,
                at: platform_common::now_utc() + backoff(job.attempts),
            }
        } else {
            TranscriptionResult::Failed {
                error: format!("{error} (after {} attempts)", job.attempts),
            }
        }
    }

    async fn attempt(
        &self,
        recordings: &RecordingStore,
        job: &TranscriptionJob,
    ) -> TranscriptionResult {
        let too_short = || TranscriptionResult::Skipped {
            reason: format!("shorter than {} ms", self.cfg.min_duration_ms),
        };
        if job
            .duration_ms
            .is_some_and(|d| d < self.cfg.min_duration_ms)
        {
            return too_short();
        }
        let bytes = match recordings.get(&job.audio_object).await {
            Ok(b) => b,
            Err(e) => return self.retry_or_fail(job, e.to_string()),
        };
        let pcm = match tokio::task::spawn_blocking(move || {
            scanner_transcription::decode_ogg_opus(&bytes, MAX_SECS)
        })
        .await
        {
            Ok(Ok(p)) => p,
            Ok(Err(e)) => {
                return TranscriptionResult::Failed {
                    error: e.to_string(),
                };
            }
            Err(e) => return self.retry_or_fail(job, e.to_string()),
        };
        let min_samples =
            self.cfg.min_duration_ms * u64::from(scanner_transcription::SAMPLE_RATE) / 1000;
        if (pcm.len() as u64) < min_samples {
            return too_short();
        }
        let wav = scanner_transcription::wav(&pcm, scanner_transcription::SAMPLE_RATE);
        match self.client.transcribe(wav).await {
            Ok(t) => TranscriptionResult::Done {
                text: t.text,
                language: t.language,
                model: self.client.model().map(str::to_owned),
            },
            Err(e) if e.is_transient() => self.retry_or_fail(job, e.to_string()),
            Err(e) => TranscriptionResult::Failed {
                error: e.to_string(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_and_caps() {
        assert_eq!(backoff(1), time::Duration::seconds(30));
        assert_eq!(backoff(2), time::Duration::seconds(60));
        assert_eq!(backoff(4), time::Duration::seconds(240));
        assert_eq!(backoff(9), time::Duration::seconds(1800));
        assert_eq!(backoff(100), time::Duration::seconds(1800));
    }
}
