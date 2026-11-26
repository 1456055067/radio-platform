//! Public REST API (`/api/v1`).

pub mod analytics;
pub mod audio;
pub mod audit;
pub mod control;
pub mod discovery;
pub mod groups;
pub mod history;
pub mod identity;
pub mod records;
pub mod scanners;
pub mod sites;
pub mod stream;

use std::sync::Arc;

use axum::extract::FromRequestParts;
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use platform_common::LeaseId;
use platform_storage::PageRequest;
use serde::Deserialize;
use utoipa::IntoParams;

use crate::error::ApiError;
use crate::state::AppState;

/// Pagination query parameters.
#[derive(Debug, Default, Deserialize, IntoParams)]
pub struct PageQuery {
    /// Page size (1–200, default 50).
    pub limit: Option<u32>,
    /// Opaque cursor from `next_cursor`.
    pub cursor: Option<String>,
}

impl PageQuery {
    pub(crate) fn page(&self) -> Result<PageRequest, ApiError> {
        if let Some(c) = &self.cursor
            && c.len() > 512
        {
            return Err(ApiError::bad_request("cursor too long"));
        }
        Ok(PageRequest {
            limit: self.limit.unwrap_or(0),
            cursor: self.cursor.clone(),
        })
    }
}

/// Optional `X-Lease-Id` header.
#[derive(Debug, Clone, Copy)]
pub struct LeaseHeader(pub Option<LeaseId>);

/// Header name carrying the control lease.
pub const LEASE_HEADER: &str = "x-lease-id";

impl<S: Send + Sync> FromRequestParts<S> for LeaseHeader {
    type Rejection = ApiError;
    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        match parts.headers.get(LEASE_HEADER) {
            None => Ok(Self(None)),
            Some(v) => v
                .to_str()
                .ok()
                .and_then(|s| s.parse::<LeaseId>().ok())
                .map(|id| Self(Some(id)))
                .ok_or_else(|| ApiError::bad_request("malformed X-Lease-Id header")),
        }
    }
}

async fn healthz() -> &'static str {
    "ok"
}

async fn readyz(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
) -> Result<&'static str, ApiError> {
    state
        .store
        .ping()
        .await
        .map_err(|_| ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "storage unavailable"))?;
    Ok("ready")
}

async fn openapi() -> Json<utoipa::openapi::OpenApi> {
    Json(crate::openapi::document())
}

/// Builds the public router.
pub fn router(state: Arc<AppState>) -> Router {
    let v1 = Router::new()
        .route("/sites", get(sites::list_sites).post(sites::create_site))
        .route("/sites/{id}/discover", post(discovery::discover))
        .route("/me", get(identity::me))
        .route(
            "/agents",
            get(identity::list_agents).post(sites::register_agent),
        )
        .route("/scanners", get(scanners::list).post(scanners::create))
        .route(
            "/scanners/{id}",
            get(scanners::get)
                .patch(scanners::update)
                .delete(scanners::remove),
        )
        .route("/scanners/{id}/status", get(control::status))
        .route("/scanners/{id}/capabilities", get(control::capabilities))
        .route("/scanners/{id}/telemetry", get(control::telemetry))
        .route(
            "/scanners/{id}/volume",
            get(control::get_volume).put(control::set_volume),
        )
        .route(
            "/scanners/{id}/squelch",
            get(control::get_squelch).put(control::set_squelch),
        )
        .route("/scanners/{id}/hold", post(control::hold))
        .route("/scanners/{id}/resume", post(control::resume))
        .route("/scanners/{id}/commands", post(control::command))
        .route("/scanners/{id}/leases", post(control::acquire_lease))
        .route(
            "/scanners/{id}/leases/{lease}",
            delete(control::release_lease),
        )
        .route("/groups", get(groups::list).post(groups::create))
        .route("/groups/{id}", get(groups::get).delete(groups::remove))
        .route(
            "/groups/{id}/members/{scanner_id}",
            axum::routing::put(groups::add_member).delete(groups::remove_member),
        )
        .route("/scanners/{id}/audio", get(audio::status))
        .route("/scanners/{id}/audio/whep", post(audio::whep))
        .route("/audio/sessions/{session}", delete(audio::hangup))
        .route("/media/status", get(audio::media_status))
        .route(
            "/transmissions/{id}/audio",
            get(audio::recording).delete(records::delete_recording),
        )
        .route("/transmissions", get(history::list_transmissions))
        .route("/transmissions/{id}", get(history::get_transmission))
        .route("/transmissions/{id}/transcribe", post(records::transcribe))
        .route(
            "/transmissions/{id}/hold",
            axum::routing::put(records::hold),
        )
        .route("/transcription/status", get(records::transcription_status))
        .route("/retention", get(records::retention_status))
        .route("/analytics/activity", get(analytics::activity))
        .route("/analytics/top", get(analytics::top))
        .route("/events", get(history::list_events))
        .route("/events/stream", get(stream::live))
        .route("/audit", get(history::list_audit))
        .route("/audit/verify", get(audit::verify))
        .route("/audit/export", get(audit::export))
        .route("/openapi.json", get(openapi));
    Router::new()
        .nest("/api/v1", v1)
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .with_state(state)
}
