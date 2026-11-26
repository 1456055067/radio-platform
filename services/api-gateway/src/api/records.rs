//! Transcription requests, retention holds, recording deletion, and the
//! transcription and retention status pages.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use platform_auth::{Permission, Resource};
use platform_common::TransmissionId;
use platform_storage::model::{AuditOutcome, TranscriptionStats, TransmissionRecord};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::history::redact;
use crate::error::ApiError;
use crate::retention::SweepReport;
use crate::state::{AppState, AuditTarget, Auth};
use crate::transcription::WorkerStats;

async fn load(
    state: &AppState,
    auth: &Auth,
    id: TransmissionId,
    perm: Permission,
    target: &AuditTarget<'_>,
) -> Result<TransmissionRecord, ApiError> {
    let t = state
        .store
        .get_transmission(auth.principal.tenant_id, id)
        .await?;
    let res = Resource {
        tenant_id: t.tenant_id,
        site_id: Some(t.site_id),
    };
    state
        .authorize(auth, Permission::EventView, res, Some(target))
        .await?;
    state.authorize(auth, perm, res, Some(target)).await?;
    Ok(t)
}

fn tx_target(action: &'static str, id: TransmissionId) -> AuditTarget<'static> {
    AuditTarget {
        action,
        resource_type: "transmission",
        resource_id: Some(id.to_string()),
    }
}

/// Queues a recorded transmission for (re-)transcription. Needs
/// `transcription_manage`. The recording is sent to the configured
/// transcription service.
#[utoipa::path(post, path = "/api/v1/transmissions/{id}/transcribe", params(("id" = TransmissionId, Path)), responses((status = 202), (status = 404), (status = 409, description = "No recording"), (status = 503, description = "Transcription is not configured")), security(("bearer" = [])))]
pub async fn transcribe(
    State(state): State<Arc<AppState>>,
    auth: Auth,
    Path(id): Path<TransmissionId>,
) -> Result<StatusCode, ApiError> {
    let target = tx_target("transmission.transcribe", id);
    let t = load(&state, &auth, id, Permission::TranscriptionManage, &target).await?;
    if state.media.transcription().is_none() {
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "transcription is not configured on this gateway",
        ));
    }
    state
        .store
        .request_transcription(t.tenant_id, id, platform_common::now_utc())
        .await?;
    state.media.transcription_wake.notify_one();
    state
        .audit(&auth, &target, AuditOutcome::Success, serde_json::json!({}))
        .await;
    Ok(StatusCode::ACCEPTED)
}

/// Hold request.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct HoldRequest {
    /// Place (`true`) or release (`false`) the hold.
    pub hold: bool,
}

/// Places or releases a retention hold: a held transmission, its recording
/// and transcript are exempt from retention and cannot be deleted. Needs
/// `recording_manage`.
#[utoipa::path(put, path = "/api/v1/transmissions/{id}/hold", params(("id" = TransmissionId, Path)), request_body = HoldRequest, responses((status = 200, body = TransmissionRecord), (status = 404)), security(("bearer" = [])))]
pub async fn hold(
    State(state): State<Arc<AppState>>,
    auth: Auth,
    Path(id): Path<TransmissionId>,
    Json(req): Json<HoldRequest>,
) -> Result<Json<TransmissionRecord>, ApiError> {
    let target = tx_target(
        if req.hold {
            "transmission.hold"
        } else {
            "transmission.release"
        },
        id,
    );
    let t = load(&state, &auth, id, Permission::RecordingManage, &target).await?;
    let mut t = state
        .store
        .set_transmission_hold(t.tenant_id, id, req.hold)
        .await?;
    state
        .audit(
            &auth,
            &target,
            AuditOutcome::Success,
            serde_json::json!({ "hold": req.hold }),
        )
        .await;
    redact(&auth, &mut t);
    Ok(Json(t))
}

/// Deletes a transmission's recording and transcript (the transmission
/// record stays). Needs `recording_manage`; refused while held.
#[utoipa::path(delete, path = "/api/v1/transmissions/{id}/audio", params(("id" = TransmissionId, Path)), responses((status = 204), (status = 404), (status = 409, description = "Held"), (status = 503)), security(("bearer" = [])))]
pub async fn delete_recording(
    State(state): State<Arc<AppState>>,
    auth: Auth,
    Path(id): Path<TransmissionId>,
) -> Result<StatusCode, ApiError> {
    let target = tx_target("transmission.audio.delete", id);
    let t = load(&state, &auth, id, Permission::RecordingManage, &target).await?;
    if t.hold {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "the transmission is on hold; release the hold first",
        ));
    }
    let key = t.audio_object.ok_or_else(|| {
        ApiError::new(StatusCode::NOT_FOUND, "no recording for this transmission")
    })?;
    let rec = state.media.recorder().ok_or_else(|| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "recording storage is not configured",
        )
    })?;
    if let Err(e) = rec.store.delete(&key).await {
        tracing::warn!(error = %e, "recording delete failed");
        state
            .audit(&auth, &target, AuditOutcome::Failure, serde_json::json!({}))
            .await;
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "recording storage is unavailable",
        ));
    }
    let cleared = state
        .store
        .clear_transmission_audio(t.tenant_id, id, &key, true)
        .await?;
    state
        .audit(
            &auth,
            &target,
            AuditOutcome::Success,
            serde_json::json!({ "detached": cleared }),
        )
        .await;
    Ok(StatusCode::NO_CONTENT)
}

/// Transcription status.
#[derive(Debug, Serialize, ToSchema)]
pub struct TranscriptionStatus {
    /// Configured on this replica.
    pub enabled: bool,
    /// Request style and endpoint host (shown to `transcription_manage`).
    pub service: Option<String>,
    /// Queue for the caller's tenant (`done`/`failed`: last 24 hours).
    pub queue: TranscriptionStats,
    /// This replica's worker counters since start.
    pub worker: Option<WorkerStats>,
}

/// Transcription status for dashboards.
#[utoipa::path(get, path = "/api/v1/transcription/status", responses((status = 200, body = TranscriptionStatus)), security(("bearer" = [])))]
pub async fn transcription_status(
    State(state): State<Arc<AppState>>,
    auth: Auth,
) -> Result<Json<TranscriptionStatus>, ApiError> {
    let p = &auth.principal;
    state
        .authorize(
            &auth,
            Permission::EventView,
            Resource {
                tenant_id: p.tenant_id,
                site_id: None,
            },
            None,
        )
        .await?;
    let t = state.media.transcription();
    let queue = state
        .store
        .transcription_stats(
            p.tenant_id,
            platform_common::now_utc() - time::Duration::DAY,
        )
        .await?;
    Ok(Json(TranscriptionStatus {
        enabled: t.is_some(),
        service: t
            .filter(|_| p.has(Permission::TranscriptionManage))
            .map(|t| t.client.describe()),
        queue,
        worker: t.map(|t| t.stats()),
    }))
}

/// Retention policy and the last sweep.
#[derive(Debug, Serialize, ToSchema)]
pub struct RetentionStatus {
    /// Any limit is configured.
    pub enabled: bool,
    /// Recording limit in days.
    pub recordings_days: Option<u32>,
    /// Transcript limit in days.
    pub transcripts_days: Option<u32>,
    /// Transmission limit in days.
    pub transmissions_days: Option<u32>,
    /// Event limit in days.
    pub events_days: Option<u32>,
    /// The last sweep on this replica (all tenants).
    pub last_sweep: Option<SweepReport>,
}

/// Retention policy (global) and this replica's last sweep.
#[utoipa::path(get, path = "/api/v1/retention", responses((status = 200, body = RetentionStatus)), security(("bearer" = [])))]
pub async fn retention_status(
    State(state): State<Arc<AppState>>,
    auth: Auth,
) -> Result<Json<RetentionStatus>, ApiError> {
    state
        .authorize(
            &auth,
            Permission::EventView,
            Resource {
                tenant_id: auth.principal.tenant_id,
                site_id: None,
            },
            None,
        )
        .await?;
    let r = state.retention.get();
    let cfg = r.map(|r| r.cfg.clone()).unwrap_or_default();
    // Sweep totals span tenants; only auditors see them.
    let last = r
        .and_then(|r| r.last())
        .filter(|_| auth.principal.has(Permission::AuditView));
    Ok(Json(RetentionStatus {
        enabled: cfg.enabled(),
        recordings_days: cfg.recordings_days,
        transcripts_days: cfg.transcripts_days,
        transmissions_days: cfg.transmissions_days,
        events_days: cfg.events_days,
        last_sweep: last,
    }))
}
