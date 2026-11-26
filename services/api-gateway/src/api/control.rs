//! Scanner status, control commands and operator leases.

use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use platform_auth::Permission;
use platform_common::{HoldKind, LeaseId, ScannerCommand, ScannerId, TelemetrySnapshot, now_utc};
use platform_storage::model::{AuditOutcome, ScannerRecord};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::LeaseHeader;
use super::scanners::{ControlView, load};
use crate::error::ApiError;
use crate::state::{AppState, AuditTarget, Auth};

/// Relays a command after authorization and lease checks.
pub(crate) async fn dispatch(
    state: &AppState,
    auth: &Auth,
    rec: &ScannerRecord,
    command: ScannerCommand,
    lease: Option<LeaseId>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let action = format!("scanner.command.{}", command.kind());
    let audited = command.requires_control() || command.requires_admin();
    let target = AuditTarget {
        action: &action,
        resource_type: "scanner",
        resource_id: Some(rec.id.to_string()),
    };
    let perm = if command.requires_admin() {
        Permission::ScannerAdmin
    } else if command.requires_control() {
        Permission::ScannerControl
    } else {
        Permission::ScannerView
    };
    let resource = platform_auth::Resource {
        tenant_id: rec.tenant_id,
        site_id: Some(rec.site_id),
    };
    state
        .authorize(auth, perm, resource, audited.then_some(&target))
        .await?;
    if command.requires_control()
        && let Err(e) = state
            .leases
            .authorize(
                rec.tenant_id,
                rec.id,
                lease,
                &auth.principal.subject,
                now_utc(),
            )
            .await
    {
        state
            .audit(
                auth,
                &target,
                AuditOutcome::Denied,
                serde_json::json!({ "reason": e.detail() }),
            )
            .await;
        return Err(e);
    }
    let details = serde_json::to_value(&command).unwrap_or_default();
    let result = state
        .hub
        .execute(&rec.agent_id, rec.id, command, state.command_timeout())
        .await;
    if audited {
        let (outcome, d) = match &result {
            Ok(_) => (
                AuditOutcome::Success,
                serde_json::json!({ "command": details }),
            ),
            Err(e) => (
                AuditOutcome::Failure,
                serde_json::json!({ "command": details, "error": e.to_string() }),
            ),
        };
        state.audit(auth, &target, outcome, d).await;
    }
    Ok(Json(result?))
}

async fn run(
    state: &AppState,
    auth: &Auth,
    id: ScannerId,
    command: ScannerCommand,
    lease: Option<LeaseId>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let rec = load(state, auth, id, Permission::ScannerView, None).await?;
    dispatch(state, auth, &rec, command, lease).await
}

/// Level request body.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LevelBody {
    /// Level (volume 0–29, squelch 0–19; validated by the agent's SDK).
    pub level: u8,
}

/// Hold request body.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct HoldBody {
    /// Target kind.
    pub kind: HoldKind,
    /// Scanner index from telemetry (`system_index`, ...).
    pub index: u32,
}

/// Live scanner status (GSI through the agent).
#[utoipa::path(get, path = "/api/v1/scanners/{id}/status", params(("id" = ScannerId, Path)), responses((status = 200, body = TelemetrySnapshot), (status = 503), (status = 504)), security(("bearer" = [])))]
pub async fn status(
    State(s): State<Arc<AppState>>,
    auth: Auth,
    Path(id): Path<ScannerId>,
) -> Result<Json<serde_json::Value>, ApiError> {
    run(&s, &auth, id, ScannerCommand::GetStatus, None).await
}

/// Capability probe (read-only commands only).
#[utoipa::path(get, path = "/api/v1/scanners/{id}/capabilities", params(("id" = ScannerId, Path)), responses((status = 200)), security(("bearer" = [])))]
pub async fn capabilities(
    State(s): State<Arc<AppState>>,
    auth: Auth,
    Path(id): Path<ScannerId>,
) -> Result<Json<serde_json::Value>, ApiError> {
    run(&s, &auth, id, ScannerCommand::ProbeCapabilities, None).await
}

/// Stored telemetry response.
#[derive(Debug, Serialize, ToSchema)]
pub struct TelemetryResponse {
    /// Scanner.
    pub scanner_id: ScannerId,
    /// Latest snapshot, if any has been received.
    pub snapshot: Option<TelemetrySnapshot>,
    /// Whether the agent is connected (snapshot may be stale otherwise).
    pub agent_connected: bool,
}

/// Latest stored telemetry.
#[utoipa::path(get, path = "/api/v1/scanners/{id}/telemetry", params(("id" = ScannerId, Path)), responses((status = 200, body = TelemetryResponse)), security(("bearer" = [])))]
pub async fn telemetry(
    State(s): State<Arc<AppState>>,
    auth: Auth,
    Path(id): Path<ScannerId>,
) -> Result<Json<TelemetryResponse>, ApiError> {
    let rec = load(&s, &auth, id, Permission::ScannerView, None).await?;
    let snapshot = s.store.get_telemetry(rec.tenant_id, id).await?;
    Ok(Json(TelemetryResponse {
        scanner_id: id,
        snapshot,
        agent_connected: s.hub.is_connected(&rec.agent_id),
    }))
}

/// Reads volume.
#[utoipa::path(get, path = "/api/v1/scanners/{id}/volume", params(("id" = ScannerId, Path)), responses((status = 200)), security(("bearer" = [])))]
pub async fn get_volume(
    State(s): State<Arc<AppState>>,
    auth: Auth,
    Path(id): Path<ScannerId>,
) -> Result<Json<serde_json::Value>, ApiError> {
    run(&s, &auth, id, ScannerCommand::GetVolume, None).await
}

/// Sets volume (requires a control lease in `X-Lease-Id`).
#[utoipa::path(put, path = "/api/v1/scanners/{id}/volume", params(("id" = ScannerId, Path), ("x-lease-id" = String, Header)), request_body = LevelBody, responses((status = 200), (status = 428)), security(("bearer" = [])))]
pub async fn set_volume(
    State(s): State<Arc<AppState>>,
    auth: Auth,
    Path(id): Path<ScannerId>,
    lease: LeaseHeader,
    Json(b): Json<LevelBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    run(
        &s,
        &auth,
        id,
        ScannerCommand::SetVolume { level: b.level },
        lease.0,
    )
    .await
}

/// Reads squelch.
#[utoipa::path(get, path = "/api/v1/scanners/{id}/squelch", params(("id" = ScannerId, Path)), responses((status = 200)), security(("bearer" = [])))]
pub async fn get_squelch(
    State(s): State<Arc<AppState>>,
    auth: Auth,
    Path(id): Path<ScannerId>,
) -> Result<Json<serde_json::Value>, ApiError> {
    run(&s, &auth, id, ScannerCommand::GetSquelch, None).await
}

/// Sets squelch (requires a control lease).
#[utoipa::path(put, path = "/api/v1/scanners/{id}/squelch", params(("id" = ScannerId, Path), ("x-lease-id" = String, Header)), request_body = LevelBody, responses((status = 200), (status = 428)), security(("bearer" = [])))]
pub async fn set_squelch(
    State(s): State<Arc<AppState>>,
    auth: Auth,
    Path(id): Path<ScannerId>,
    lease: LeaseHeader,
    Json(b): Json<LevelBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    run(
        &s,
        &auth,
        id,
        ScannerCommand::SetSquelch { level: b.level },
        lease.0,
    )
    .await
}

/// Holds on a system, department, site or channel (requires a lease).
#[utoipa::path(post, path = "/api/v1/scanners/{id}/hold", params(("id" = ScannerId, Path), ("x-lease-id" = String, Header)), request_body = HoldBody, responses((status = 200)), security(("bearer" = [])))]
pub async fn hold(
    State(s): State<Arc<AppState>>,
    auth: Auth,
    Path(id): Path<ScannerId>,
    lease: LeaseHeader,
    Json(b): Json<HoldBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    run(
        &s,
        &auth,
        id,
        ScannerCommand::Hold {
            kind: b.kind,
            index: b.index,
        },
        lease.0,
    )
    .await
}

/// Resumes scanning (requires a lease).
#[utoipa::path(post, path = "/api/v1/scanners/{id}/resume", params(("id" = ScannerId, Path), ("x-lease-id" = String, Header)), responses((status = 200)), security(("bearer" = [])))]
pub async fn resume(
    State(s): State<Arc<AppState>>,
    auth: Auth,
    Path(id): Path<ScannerId>,
    lease: LeaseHeader,
) -> Result<Json<serde_json::Value>, ApiError> {
    run(&s, &auth, id, ScannerCommand::Resume, lease.0).await
}

/// Executes a validated command. `raw` commands require `scanner_admin`.
#[utoipa::path(post, path = "/api/v1/scanners/{id}/commands", params(("id" = ScannerId, Path), ("x-lease-id" = Option<String>, Header)), request_body = ScannerCommand, responses((status = 200)), security(("bearer" = [])))]
pub async fn command(
    State(s): State<Arc<AppState>>,
    auth: Auth,
    Path(id): Path<ScannerId>,
    lease: LeaseHeader,
    Json(c): Json<ScannerCommand>,
) -> Result<Json<serde_json::Value>, ApiError> {
    run(&s, &auth, id, c, lease.0).await
}

/// Lease request.
#[derive(Debug, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LeaseBody {
    /// Requested duration in seconds (default 300, maximum configurable).
    pub ttl_secs: Option<u64>,
}

/// Acquires (or renews) exclusive control.
#[utoipa::path(post, path = "/api/v1/scanners/{id}/leases", params(("id" = ScannerId, Path)), request_body = LeaseBody, responses((status = 201, body = ControlView), (status = 409, description = "Held by another operator")), security(("bearer" = [])))]
pub async fn acquire_lease(
    State(s): State<Arc<AppState>>,
    auth: Auth,
    Path(id): Path<ScannerId>,
    body: Option<Json<LeaseBody>>,
) -> Result<(StatusCode, Json<ControlView>), ApiError> {
    let target = AuditTarget {
        action: "scanner.lease.acquire",
        resource_type: "scanner",
        resource_id: Some(id.to_string()),
    };
    let rec = load(&s, &auth, id, Permission::ScannerControl, Some(&target)).await?;
    let ttl = body.and_then(|b| b.0.ttl_secs).map(Duration::from_secs);
    let lease = s
        .leases
        .acquire(
            rec.tenant_id,
            rec.id,
            &auth.principal.subject,
            ttl,
            now_utc(),
        )
        .await?;
    s.audit(&auth, &target, AuditOutcome::Success, serde_json::json!({ "lease_id": lease.id, "expires_at": lease.expires_at.unix_timestamp() })).await;
    Ok((
        StatusCode::CREATED,
        Json(ControlView {
            lease_id: Some(lease.id),
            holder: lease.holder,
            expires_at: lease.expires_at,
        }),
    ))
}

/// Releases control. Administrators (`scanner_admin`) may revoke any lease.
#[utoipa::path(delete, path = "/api/v1/scanners/{id}/leases/{lease}", params(("id" = ScannerId, Path), ("lease" = LeaseId, Path)), responses((status = 204), (status = 403)), security(("bearer" = [])))]
pub async fn release_lease(
    State(s): State<Arc<AppState>>,
    auth: Auth,
    Path((id, lease)): Path<(ScannerId, LeaseId)>,
) -> Result<StatusCode, ApiError> {
    let target = AuditTarget {
        action: "scanner.lease.release",
        resource_type: "scanner",
        resource_id: Some(id.to_string()),
    };
    let rec = load(&s, &auth, id, Permission::ScannerControl, Some(&target)).await?;
    let force = auth.principal.has(Permission::ScannerAdmin);
    match s
        .leases
        .release(
            rec.tenant_id,
            rec.id,
            lease,
            &auth.principal.subject,
            force,
            now_utc(),
        )
        .await
    {
        Ok(l) => {
            s.audit(&auth, &target, AuditOutcome::Success, serde_json::json!({ "lease_id": l.id, "holder": l.holder, "forced": l.holder != auth.principal.subject })).await;
            Ok(StatusCode::NO_CONTENT)
        }
        Err(e) => {
            s.audit(
                &auth,
                &target,
                AuditOutcome::Denied,
                serde_json::json!({ "reason": e.detail() }),
            )
            .await;
            Err(e)
        }
    }
}
