//! Transmissions, events and audit records.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use platform_auth::{Permission, Resource};
use platform_common::{ScannerId, SiteId, TransmissionId};
use platform_storage::Page;
use platform_storage::model::{
    AuditFilter, AuditOutcome, AuditRecord, EventFilter, EventRecord, TranscriptionState,
    TransmissionFilter, TransmissionRecord,
};
use serde::Deserialize;
use time::OffsetDateTime;
use utoipa::IntoParams;

use super::PageQuery;
use crate::error::ApiError;
use crate::state::{AppState, AuditTarget, Auth};

/// Longest full-text query.
const MAX_QUERY: usize = 256;

/// Removes transcript content unless the caller may listen to audio
/// (transcripts are the content of communications).
pub(crate) fn redact(auth: &Auth, t: &mut TransmissionRecord) {
    if !auth.principal.has(Permission::AudioListen) {
        t.redact_transcript();
    }
}

/// Transmission search parameters.
#[derive(Debug, Deserialize, IntoParams)]
pub struct TransmissionQuery {
    /// Scanner.
    pub scanner_id: Option<ScannerId>,
    /// Site.
    pub site_id: Option<SiteId>,
    /// Exact frequency in Hz.
    pub frequency_hz: Option<u64>,
    /// Exact talkgroup.
    pub tgid: Option<String>,
    /// Started at or after (RFC 3339).
    #[serde(default, with = "time::serde::rfc3339::option")]
    #[param(value_type = Option<String>)]
    pub from: Option<OffsetDateTime>,
    /// Started before (RFC 3339).
    #[serde(default, with = "time::serde::rfc3339::option")]
    #[param(value_type = Option<String>)]
    pub to: Option<OffsetDateTime>,
    /// Full-text search over transcripts (web-search syntax: words,
    /// `"quoted phrases"`, `or`, `-excluded`). Needs `audio_listen`;
    /// audited.
    pub q: Option<String>,
    /// Only transmissions with (`true`) or without (`false`) a recording.
    pub recorded: Option<bool>,
    /// Transcription status.
    #[param(value_type = Option<TranscriptionState>)]
    pub transcription: Option<TranscriptionState>,
    /// Page size.
    pub limit: Option<u32>,
    /// Cursor.
    pub cursor: Option<String>,
}

/// Searches transmissions, newest first. Transcript fields are removed for
/// callers without `audio_listen`.
#[utoipa::path(get, path = "/api/v1/transmissions", params(TransmissionQuery), responses((status = 200, body = Page<TransmissionRecord>)), security(("bearer" = [])))]
pub async fn list_transmissions(
    State(s): State<Arc<AppState>>,
    auth: Auth,
    Query(q): Query<TransmissionQuery>,
) -> Result<Json<Page<TransmissionRecord>>, ApiError> {
    let p = &auth.principal;
    s.authorize(
        &auth,
        Permission::EventView,
        Resource {
            tenant_id: p.tenant_id,
            site_id: q.site_id,
        },
        None,
    )
    .await?;
    if q.tgid.as_ref().is_some_and(|t| t.len() > 64) {
        return Err(ApiError::bad_request("tgid too long"));
    }
    let text = q.q.as_deref().map(str::trim).filter(|t| !t.is_empty());
    if let Some(text) = text {
        if text.chars().count() > MAX_QUERY || text.chars().any(char::is_control) {
            return Err(ApiError::bad_request(format!(
                "q must be at most {MAX_QUERY} printable characters"
            )));
        }
        let target = AuditTarget {
            action: "transmission.search",
            resource_type: "transmission",
            resource_id: None,
        };
        s.authorize(
            &auth,
            Permission::AudioListen,
            Resource {
                tenant_id: p.tenant_id,
                site_id: q.site_id,
            },
            Some(&target),
        )
        .await?;
        s.audit(
            &auth,
            &target,
            AuditOutcome::Success,
            serde_json::json!({ "q": text }),
        )
        .await;
    }
    let filter = TransmissionFilter {
        scanner_id: q.scanner_id,
        site_id: q.site_id,
        site_ids: p.sites.as_filter(),
        frequency_hz: q.frequency_hz,
        tgid: q.tgid,
        from: q.from,
        to: q.to,
        text: text.map(str::to_owned),
        recorded: q.recorded,
        transcription_state: q.transcription,
    };
    let page = PageQuery {
        limit: q.limit,
        cursor: q.cursor,
    }
    .page()?;
    let mut out = s
        .store
        .list_transmissions(p.tenant_id, &filter, &page)
        .await?;
    for t in &mut out.items {
        redact(&auth, t);
    }
    Ok(Json(out))
}

/// Gets a transmission.
#[utoipa::path(get, path = "/api/v1/transmissions/{id}", params(("id" = TransmissionId, Path)), responses((status = 200, body = TransmissionRecord), (status = 404)), security(("bearer" = [])))]
pub async fn get_transmission(
    State(s): State<Arc<AppState>>,
    auth: Auth,
    Path(id): Path<TransmissionId>,
) -> Result<Json<TransmissionRecord>, ApiError> {
    let mut t = s
        .store
        .get_transmission(auth.principal.tenant_id, id)
        .await?;
    s.authorize(
        &auth,
        Permission::EventView,
        Resource {
            tenant_id: t.tenant_id,
            site_id: Some(t.site_id),
        },
        None,
    )
    .await?;
    redact(&auth, &mut t);
    Ok(Json(t))
}

/// Event filters.
#[derive(Debug, Deserialize, IntoParams)]
pub struct EventQuery {
    /// Scanner.
    pub scanner_id: Option<ScannerId>,
    /// Kind (`transmission_started`, `transmission_ended`, `health_changed`).
    pub kind: Option<String>,
    /// Page size.
    pub limit: Option<u32>,
    /// Cursor.
    pub cursor: Option<String>,
}

/// Lists events, newest first.
#[utoipa::path(get, path = "/api/v1/events", params(EventQuery), responses((status = 200, body = Page<EventRecord>)), security(("bearer" = [])))]
pub async fn list_events(
    State(s): State<Arc<AppState>>,
    auth: Auth,
    Query(q): Query<EventQuery>,
) -> Result<Json<Page<EventRecord>>, ApiError> {
    let p = &auth.principal;
    s.authorize(
        &auth,
        Permission::EventView,
        Resource {
            tenant_id: p.tenant_id,
            site_id: None,
        },
        None,
    )
    .await?;
    if q.kind.as_ref().is_some_and(|k| k.len() > 64) {
        return Err(ApiError::bad_request("kind too long"));
    }
    let filter = EventFilter {
        scanner_id: q.scanner_id,
        site_ids: p.sites.as_filter(),
        kind: q.kind,
    };
    let page = PageQuery {
        limit: q.limit,
        cursor: q.cursor,
    }
    .page()?;
    Ok(Json(
        s.store.list_events(p.tenant_id, &filter, &page).await?,
    ))
}

/// Audit filters.
#[derive(Debug, Deserialize, IntoParams)]
pub struct AuditQuery {
    /// Exact actor.
    pub actor: Option<String>,
    /// Action prefix (`scanner.` matches `scanner.update`).
    pub action: Option<String>,
    /// Outcome.
    #[param(value_type = Option<AuditOutcome>)]
    pub outcome: Option<AuditOutcome>,
    /// Resource type.
    pub resource_type: Option<String>,
    /// Resource id.
    pub resource_id: Option<String>,
    /// At or after (RFC 3339).
    #[serde(default, with = "time::serde::rfc3339::option")]
    #[param(value_type = Option<String>)]
    pub from: Option<OffsetDateTime>,
    /// Before (RFC 3339).
    #[serde(default, with = "time::serde::rfc3339::option")]
    #[param(value_type = Option<String>)]
    pub to: Option<OffsetDateTime>,
    /// Page size.
    pub limit: Option<u32>,
    /// Cursor.
    pub cursor: Option<String>,
}

impl AuditQuery {
    pub(crate) fn filter(&self) -> Result<AuditFilter, ApiError> {
        for v in [
            &self.actor,
            &self.action,
            &self.resource_type,
            &self.resource_id,
        ]
        .into_iter()
        .flatten()
        {
            if v.len() > 256 {
                return Err(ApiError::bad_request(
                    "filter values are limited to 256 bytes",
                ));
            }
        }
        Ok(AuditFilter {
            actor: self.actor.clone(),
            action: self.action.clone(),
            outcome: self.outcome,
            resource_type: self.resource_type.clone(),
            resource_id: self.resource_id.clone(),
            from: self.from,
            to: self.to,
        })
    }
}

pub(crate) async fn authorize_audit(s: &AppState, auth: &Auth) -> Result<(), ApiError> {
    s.authorize(
        auth,
        Permission::AuditView,
        Resource {
            tenant_id: auth.principal.tenant_id,
            site_id: None,
        },
        None,
    )
    .await
}

/// Lists audit records, newest first.
#[utoipa::path(get, path = "/api/v1/audit", params(AuditQuery), responses((status = 200, body = Page<AuditRecord>)), security(("bearer" = [])))]
pub async fn list_audit(
    State(s): State<Arc<AppState>>,
    auth: Auth,
    Query(q): Query<AuditQuery>,
) -> Result<Json<Page<AuditRecord>>, ApiError> {
    authorize_audit(&s, &auth).await?;
    let filter = q.filter()?;
    let page = PageQuery {
        limit: q.limit,
        cursor: q.cursor,
    }
    .page()?;
    Ok(Json(
        s.store
            .list_audit(auth.principal.tenant_id, &filter, &page)
            .await?,
    ))
}
