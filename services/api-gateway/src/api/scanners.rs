//! Scanner enrollment and management.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use platform_auth::{Permission, Resource};
use platform_common::{AgentId, ScannerId, SiteId, now_utc};
use platform_storage::Page;
use platform_storage::model::{
    AuditOutcome, NewScanner, ScannerFilter, ScannerPatch, ScannerRecord,
};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use super::PageQuery;
use crate::agent_ws::assignments;
use crate::error::ApiError;
use crate::state::{AppState, AuditTarget, Auth};

/// Enrollment request.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateScanner {
    /// Site.
    pub site_id: SiteId,
    /// Managing agent (registered at the same site).
    #[schema(value_type = String)]
    pub agent_id: AgentId,
    /// Name.
    pub name: String,
    /// Scanner IP address (must be inside the site's authorized networks).
    #[schema(value_type = String)]
    pub host: IpAddr,
    /// UDP port (default 50536).
    pub port: Option<u16>,
    /// Managed immediately (default true).
    pub enabled: Option<bool>,
}

/// Partial update.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateScanner {
    /// Name.
    pub name: Option<String>,
    /// Enabled.
    pub enabled: Option<bool>,
    /// Address.
    #[schema(value_type = Option<String>)]
    pub host: Option<IpAddr>,
    /// Port.
    pub port: Option<u16>,
    /// Allow live audio (pulls the scanner's RTSP stream on demand).
    pub audio_enabled: Option<bool>,
    /// Record transmissions (requires `recording_manage`, audio, and
    /// recording configured on the gateway). Only record communications
    /// you are authorized to record.
    pub recording_enabled: Option<bool>,
    /// Transcribe recordings (requires `transcription_manage`, recording,
    /// and transcription configured on the gateway). Recordings are sent
    /// to the configured transcription service.
    pub transcription_enabled: Option<bool>,
    /// RTSP port (must be an allowed audio port).
    pub rtsp_port: Option<u16>,
}

/// Current control holder.
#[derive(Debug, Serialize, ToSchema)]
pub struct ControlView {
    /// Lease identifier (visible only to the holder and to `scanner_admin`).
    pub lease_id: Option<platform_common::LeaseId>,
    /// Holder subject.
    pub holder: String,
    /// Expiry.
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub expires_at: time::OffsetDateTime,
}

/// Scanner with live status.
#[derive(Debug, Serialize, ToSchema)]
pub struct ScannerDetail {
    /// Stored record.
    #[serde(flatten)]
    pub scanner: ScannerRecord,
    /// Whether the managing agent is connected.
    pub agent_connected: bool,
    /// Current operator control, if any.
    pub control: Option<ControlView>,
}

/// Listing filters.
#[derive(Debug, Deserialize, IntoParams)]
pub struct ScannerQuery {
    /// Site.
    pub site_id: Option<SiteId>,
    /// Agent.
    pub agent_id: Option<String>,
    /// Page size.
    pub limit: Option<u32>,
    /// Cursor.
    pub cursor: Option<String>,
}

pub(crate) async fn load(
    state: &AppState,
    auth: &Auth,
    id: ScannerId,
    perm: Permission,
    audit: Option<&AuditTarget<'_>>,
) -> Result<ScannerRecord, ApiError> {
    let rec = state
        .store
        .get_scanner(auth.principal.tenant_id, id)
        .await?;
    state
        .authorize(
            auth,
            perm,
            Resource {
                tenant_id: rec.tenant_id,
                site_id: Some(rec.site_id),
            },
            audit,
        )
        .await?;
    Ok(rec)
}

fn view(auth: &Auth, l: scanner_controller::Lease) -> ControlView {
    ControlView {
        // Admins see it too so they can revoke the lease; commands still
        // require being the holder.
        lease_id: (l.holder == auth.principal.subject
            || auth.principal.has(Permission::ScannerAdmin))
        .then_some(l.id),
        holder: l.holder,
        expires_at: l.expires_at,
    }
}

pub(crate) async fn detail(
    state: &AppState,
    auth: &Auth,
    scanner: ScannerRecord,
) -> Result<ScannerDetail, ApiError> {
    let control = state
        .leases
        .current(scanner.tenant_id, scanner.id, now_utc())
        .await?
        .map(|l| view(auth, l));
    Ok(ScannerDetail {
        agent_connected: state.hub.is_connected(&scanner.agent_id),
        control,
        scanner,
    })
}

async fn push_config(state: &AppState, agent: &AgentId) {
    if let Ok(list) = state.store.scanners_for_agent(agent).await {
        state.hub.configure(agent, assignments(&list));
    }
}

fn check_endpoint(
    state: &AppState,
    site: &platform_storage::model::Site,
    host: IpAddr,
    port: u16,
) -> Result<(), ApiError> {
    site.policy(
        state.limits.scanner_ports.clone(),
        state.limits.audio_ports.clone(),
    )
    .check(SocketAddr::new(host, port))
    .map_err(|v| ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, v.to_string()))
}

/// Lists scanners with their live agent connection and control state.
#[utoipa::path(get, path = "/api/v1/scanners", params(ScannerQuery), responses((status = 200, body = Page<ScannerDetail>)), security(("bearer" = [])))]
pub async fn list(
    State(state): State<Arc<AppState>>,
    auth: Auth,
    Query(q): Query<ScannerQuery>,
) -> Result<Json<Page<ScannerDetail>>, ApiError> {
    let p = &auth.principal;
    state
        .authorize(
            &auth,
            Permission::ScannerView,
            Resource {
                tenant_id: p.tenant_id,
                site_id: q.site_id,
            },
            None,
        )
        .await?;
    let agent_id = q
        .agent_id
        .map(AgentId::new)
        .transpose()
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    let filter = ScannerFilter {
        site_id: q.site_id,
        site_ids: p.sites.as_filter(),
        agent_id,
    };
    let page = PageQuery {
        limit: q.limit,
        cursor: q.cursor,
    }
    .page()?;
    let records = state
        .store
        .list_scanners(p.tenant_id, &filter, &page)
        .await?;
    let ids: Vec<ScannerId> = records.items.iter().map(|r| r.id).collect();
    let mut leases = state
        .leases
        .current_many(p.tenant_id, &ids, now_utc())
        .await?;
    Ok(Json(Page {
        items: records
            .items
            .into_iter()
            .map(|r| ScannerDetail {
                agent_connected: state.hub.is_connected(&r.agent_id),
                control: leases.remove(&r.id).map(|l| view(&auth, l)),
                scanner: r,
            })
            .collect(),
        next_cursor: records.next_cursor,
    }))
}

/// Enrolls a scanner.
#[utoipa::path(post, path = "/api/v1/scanners", request_body = CreateScanner, responses((status = 201, body = ScannerDetail), (status = 409), (status = 422, description = "Endpoint outside authorized networks")), security(("bearer" = [])))]
pub async fn create(
    State(state): State<Arc<AppState>>,
    auth: Auth,
    Json(req): Json<CreateScanner>,
) -> Result<(StatusCode, Json<ScannerDetail>), ApiError> {
    let tenant = auth.principal.tenant_id;
    let target = AuditTarget {
        action: "scanner.create",
        resource_type: "scanner",
        resource_id: None,
    };
    state
        .authorize(
            &auth,
            Permission::ScannerManage,
            Resource {
                tenant_id: tenant,
                site_id: Some(req.site_id),
            },
            Some(&target),
        )
        .await?;
    let site = state.store.get_site(tenant, req.site_id).await?;
    let agent = state
        .store
        .get_agent(&req.agent_id)
        .await
        .map_err(|_| ApiError::bad_request("unknown agent"))?;
    if agent.tenant_id != tenant || agent.site_id != site.id {
        return Err(ApiError::bad_request(
            "agent is not registered at this site",
        ));
    }
    let port = req.port.unwrap_or(50536);
    if let Err(e) = check_endpoint(&state, &site, req.host, port) {
        state
            .audit(
                &auth,
                &target,
                AuditOutcome::Denied,
                serde_json::json!({ "host": req.host, "port": port, "reason": "network policy" }),
            )
            .await;
        return Err(e);
    }
    let rec = state
        .store
        .create_scanner(NewScanner {
            tenant_id: tenant,
            site_id: site.id,
            agent_id: agent.id.clone(),
            name: req.name,
            host: req.host,
            port,
            enabled: req.enabled.unwrap_or(true),
        })
        .await?;
    let target = AuditTarget {
        resource_id: Some(rec.id.to_string()),
        ..target
    };
    state
        .audit(
            &auth,
            &target,
            AuditOutcome::Success,
            serde_json::json!({ "host": rec.host, "port": rec.port, "agent_id": rec.agent_id }),
        )
        .await;
    push_config(&state, &agent.id).await;
    Ok((StatusCode::CREATED, Json(detail(&state, &auth, rec).await?)))
}

/// Gets a scanner.
#[utoipa::path(get, path = "/api/v1/scanners/{id}", params(("id" = ScannerId, Path)), responses((status = 200, body = ScannerDetail), (status = 404)), security(("bearer" = [])))]
pub async fn get(
    State(state): State<Arc<AppState>>,
    auth: Auth,
    Path(id): Path<ScannerId>,
) -> Result<Json<ScannerDetail>, ApiError> {
    let rec = load(&state, &auth, id, Permission::ScannerView, None).await?;
    Ok(Json(detail(&state, &auth, rec).await?))
}

/// Updates a scanner.
#[utoipa::path(patch, path = "/api/v1/scanners/{id}", params(("id" = ScannerId, Path)), request_body = UpdateScanner, responses((status = 200, body = ScannerDetail)), security(("bearer" = [])))]
pub async fn update(
    State(state): State<Arc<AppState>>,
    auth: Auth,
    Path(id): Path<ScannerId>,
    Json(req): Json<UpdateScanner>,
) -> Result<Json<ScannerDetail>, ApiError> {
    let target = AuditTarget {
        action: "scanner.update",
        resource_type: "scanner",
        resource_id: Some(id.to_string()),
    };
    let rec = load(&state, &auth, id, Permission::ScannerManage, Some(&target)).await?;
    if req.recording_enabled.is_some() {
        state
            .authorize(
                &auth,
                Permission::RecordingManage,
                Resource {
                    tenant_id: rec.tenant_id,
                    site_id: Some(rec.site_id),
                },
                Some(&target),
            )
            .await?;
        if req.recording_enabled == Some(true) && state.media.recorder().is_none() {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "recording is not configured on this gateway",
            ));
        }
        if req.recording_enabled == Some(true) && !req.audio_enabled.unwrap_or(rec.audio_enabled) {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "recording requires audio to be enabled",
            ));
        }
    }
    if req.transcription_enabled.is_some() {
        state
            .authorize(
                &auth,
                Permission::TranscriptionManage,
                Resource {
                    tenant_id: rec.tenant_id,
                    site_id: Some(rec.site_id),
                },
                Some(&target),
            )
            .await?;
        if req.transcription_enabled == Some(true) {
            if state.media.transcription().is_none() {
                return Err(ApiError::new(
                    StatusCode::CONFLICT,
                    "transcription is not configured on this gateway",
                ));
            }
            if !req.recording_enabled.unwrap_or(rec.recording_enabled)
                || !req.audio_enabled.unwrap_or(rec.audio_enabled)
            {
                return Err(ApiError::new(
                    StatusCode::CONFLICT,
                    "transcription requires recording to be enabled",
                ));
            }
        }
    }
    if let Some(p) = req.rtsp_port
        && !state.limits.audio_ports.contains(&p)
    {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("RTSP port {p} is not an allowed audio port"),
        ));
    }
    if req.host.is_some() || req.port.is_some() {
        let site = state.store.get_site(rec.tenant_id, rec.site_id).await?;
        check_endpoint(
            &state,
            &site,
            req.host.unwrap_or(rec.host),
            req.port.unwrap_or(rec.port),
        )?;
    }
    let updated = state
        .store
        .update_scanner(
            rec.tenant_id,
            id,
            ScannerPatch {
                name: req.name,
                enabled: req.enabled,
                host: req.host,
                port: req.port,
                audio_enabled: req.audio_enabled,
                recording_enabled: req.recording_enabled,
                transcription_enabled: req.transcription_enabled,
                rtsp_port: req.rtsp_port,
            },
        )
        .await?;
    state.audit(&auth, &target, AuditOutcome::Success, serde_json::json!({ "enabled": updated.enabled, "host": updated.host, "port": updated.port, "audio_enabled": updated.audio_enabled, "recording_enabled": updated.recording_enabled, "transcription_enabled": updated.transcription_enabled, "rtsp_port": updated.rtsp_port })).await;
    push_config(&state, &updated.agent_id).await;
    Ok(Json(detail(&state, &auth, updated).await?))
}

/// Deletes a scanner.
#[utoipa::path(delete, path = "/api/v1/scanners/{id}", params(("id" = ScannerId, Path)), responses((status = 204)), security(("bearer" = [])))]
pub async fn remove(
    State(state): State<Arc<AppState>>,
    auth: Auth,
    Path(id): Path<ScannerId>,
) -> Result<StatusCode, ApiError> {
    let target = AuditTarget {
        action: "scanner.delete",
        resource_type: "scanner",
        resource_id: Some(id.to_string()),
    };
    let rec = load(&state, &auth, id, Permission::ScannerManage, Some(&target)).await?;
    state.store.delete_scanner(rec.tenant_id, id).await?;
    state
        .audit(&auth, &target, AuditOutcome::Success, serde_json::json!({}))
        .await;
    push_config(&state, &rec.agent_id).await;
    Ok(StatusCode::NO_CONTENT)
}
