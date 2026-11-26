//! Live audio (WHEP) and recording playback.
//!
//! Only monitor or record communications you are authorized to receive.
//! Every listening session and every playback is audited.

use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use platform_auth::{Permission, Resource};
use platform_common::wire::AudioState;
use platform_common::{ScannerId, TransmissionId};
use platform_storage::model::AuditOutcome;
use serde::Serialize;
use utoipa::ToSchema;

use super::scanners::load;
use crate::error::ApiError;
use crate::media::{ListenError, RecorderStats, SessionMeta};
use crate::state::{AppState, AuditTarget, Auth};

/// Largest SDP offer accepted.
const MAX_OFFER: usize = 16 * 1024;

fn listen_error(e: &ListenError) -> ApiError {
    match e {
        ListenError::Disabled => ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "live audio is not configured on the gateway",
        ),
        ListenError::AgentOffline => {
            ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "the site agent is offline")
        }
        ListenError::Offer { message } => {
            ApiError::bad_request(format!("invalid SDP offer: {message}"))
        }
        ListenError::Busy { message } => {
            ApiError::new(StatusCode::TOO_MANY_REQUESTS, message.clone())
        }
    }
}

/// Starts listening (WHEP): body is an SDP offer with a receive-only audio
/// transceiver; the response is the SDP answer, with the session URL in
/// `Location`.
#[utoipa::path(post, path = "/api/v1/scanners/{id}/audio/whep", params(("id" = ScannerId, Path)), request_body(content = String, content_type = "application/sdp"), responses((status = 201, description = "SDP answer", content_type = "application/sdp"), (status = 403), (status = 409, description = "Audio is disabled for this scanner"), (status = 429), (status = 503)), security(("bearer" = [])))]
pub async fn whep(
    State(state): State<Arc<AppState>>,
    auth: Auth,
    Path(id): Path<ScannerId>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    let target = AuditTarget {
        action: "scanner.audio.listen",
        resource_type: "scanner",
        resource_id: Some(id.to_string()),
    };
    let rec = load(&state, &auth, id, Permission::AudioListen, Some(&target)).await?;
    if headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_none_or(|v| !v.starts_with("application/sdp"))
    {
        return Err(ApiError::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "send the offer as application/sdp",
        ));
    }
    if body.len() > MAX_OFFER {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "SDP offer too large",
        ));
    }
    if !rec.audio_enabled || !rec.enabled {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "live audio is disabled for this scanner",
        ));
    }
    let offer =
        std::str::from_utf8(&body).map_err(|_| ApiError::bad_request("offer is not UTF-8"))?;
    let meta = SessionMeta {
        subject: auth.principal.subject.clone(),
        tenant_id: rec.tenant_id,
        scanner_id: rec.id,
    };
    let result = state
        .hub
        .listen(&rec.agent_id, offer, meta, Duration::from_secs(10))
        .await;
    match result {
        Ok((session, answer)) => {
            state
                .audit(
                    &auth,
                    &target,
                    AuditOutcome::Success,
                    serde_json::json!({ "session": session }),
                )
                .await;
            let mut r = (StatusCode::CREATED, answer).into_response();
            let h = r.headers_mut();
            h.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/sdp"),
            );
            if let Ok(loc) = HeaderValue::from_str(&format!("/api/v1/audio/sessions/{session}")) {
                h.insert(header::LOCATION, loc);
            }
            Ok(r)
        }
        Err(e) => {
            state
                .audit(
                    &auth,
                    &target,
                    AuditOutcome::Failure,
                    serde_json::json!({ "error": e }),
                )
                .await;
            Err(listen_error(&e))
        }
    }
}

/// Stops listening. The session's creator or a `scanner_admin` may end it.
#[utoipa::path(delete, path = "/api/v1/audio/sessions/{session}", params(("session" = String, Path)), responses((status = 204), (status = 403)), security(("bearer" = [])))]
pub async fn hangup(
    State(state): State<Arc<AppState>>,
    auth: Auth,
    Path(session): Path<uuid::Uuid>,
) -> Result<StatusCode, ApiError> {
    let p = &auth.principal;
    match state.hub.hangup(
        session,
        p.tenant_id,
        &p.subject,
        p.has(Permission::ScannerAdmin),
    ) {
        Some(false) => Err(ApiError::forbidden("not your listening session")),
        _ => Ok(StatusCode::NO_CONTENT),
    }
}

/// A scanner's audio status.
#[derive(Debug, Serialize, ToSchema)]
pub struct AudioStatus {
    /// Live audio allowed for this scanner.
    pub audio_enabled: bool,
    /// Transmissions are recorded.
    pub recording_enabled: bool,
    /// Live audio is configured on this gateway replica.
    pub live_available: bool,
    /// Agent pipeline state (if this replica holds the agent).
    pub state: Option<AudioState>,
    /// Listeners on this replica.
    pub listeners: usize,
    /// Audio frames are arriving now.
    pub receiving: bool,
}

/// Audio status of one scanner.
#[utoipa::path(get, path = "/api/v1/scanners/{id}/audio", params(("id" = ScannerId, Path)), responses((status = 200, body = AudioStatus)), security(("bearer" = [])))]
pub async fn status(
    State(state): State<Arc<AppState>>,
    auth: Auth,
    Path(id): Path<ScannerId>,
) -> Result<Json<AudioStatus>, ApiError> {
    let rec = load(&state, &auth, id, Permission::ScannerView, None).await?;
    let s = state.media.hub.status(rec.id);
    Ok(Json(AudioStatus {
        audio_enabled: rec.audio_enabled,
        recording_enabled: rec.recording_enabled,
        live_available: state.media.webrtc().is_some(),
        state: s.state,
        listeners: s.listeners,
        receiving: s.receiving,
    }))
}

/// Media status of this replica.
#[derive(Debug, Serialize, ToSchema)]
pub struct MediaStatus {
    /// Live audio configured.
    pub live_enabled: bool,
    /// Listening sessions on this replica.
    pub sessions: usize,
    /// Scanners currently streaming audio to this replica.
    pub streams: usize,
    /// Recording configured.
    pub recording_enabled: bool,
    /// Recording storage backend.
    pub recording_backend: Option<String>,
    /// Recording counters (since start).
    pub recordings: Option<RecorderStats>,
}

/// Media status for dashboards.
#[utoipa::path(get, path = "/api/v1/media/status", responses((status = 200, body = MediaStatus)), security(("bearer" = [])))]
pub async fn media_status(
    State(state): State<Arc<AppState>>,
    auth: Auth,
) -> Result<Json<MediaStatus>, ApiError> {
    state
        .authorize(
            &auth,
            Permission::ScannerView,
            Resource {
                tenant_id: auth.principal.tenant_id,
                site_id: None,
            },
            None,
        )
        .await?;
    state.media.prune();
    let (streams, _) = state.media.hub.totals();
    let rec = state.media.recorder();
    Ok(Json(MediaStatus {
        live_enabled: state.media.webrtc().is_some(),
        sessions: state
            .media
            .webrtc()
            .map_or(0, media_gateway::WebRtcServer::sessions),
        streams,
        recording_enabled: rec.is_some(),
        recording_backend: rec.map(|r| r.store.describe()),
        recordings: rec.map(crate::media::Recorder::stats),
    }))
}

/// Plays a recorded transmission (`audio/ogg; codecs=opus`).
#[utoipa::path(get, path = "/api/v1/transmissions/{id}/audio", params(("id" = TransmissionId, Path)), responses((status = 200, description = "Ogg Opus", content_type = "audio/ogg"), (status = 404)), security(("bearer" = [])))]
pub async fn recording(
    State(state): State<Arc<AppState>>,
    auth: Auth,
    Path(id): Path<TransmissionId>,
) -> Result<Response, ApiError> {
    let target = AuditTarget {
        action: "transmission.audio.play",
        resource_type: "transmission",
        resource_id: Some(id.to_string()),
    };
    let t = state
        .store
        .get_transmission(auth.principal.tenant_id, id)
        .await?;
    let res = Resource {
        tenant_id: t.tenant_id,
        site_id: Some(t.site_id),
    };
    state
        .authorize(&auth, Permission::EventView, res, Some(&target))
        .await?;
    state
        .authorize(&auth, Permission::AudioListen, res, Some(&target))
        .await?;
    let key = t.audio_object.ok_or_else(|| {
        ApiError::new(StatusCode::NOT_FOUND, "no recording for this transmission")
    })?;
    let rec = state.media.recorder().ok_or_else(|| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "recording storage is not configured",
        )
    })?;
    let bytes = rec.store.get(&key).await.map_err(|e| {
        tracing::warn!(error = %e, "recording fetch failed");
        ApiError::new(StatusCode::NOT_FOUND, "recording not available")
    })?;
    state
        .audit(&auth, &target, AuditOutcome::Success, serde_json::json!({}))
        .await;
    let mut r = bytes.into_response();
    r.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(scanner_recorder::CONTENT_TYPE),
    );
    r.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    Ok(r)
}
