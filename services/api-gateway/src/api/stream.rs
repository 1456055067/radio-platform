//! Live updates over WebSocket (`/api/v1/events/stream`).
//!
//! Authenticate with `Authorization: Bearer` or, from browsers, by offering
//! the subprotocols `radio-platform.v1` and `radio-platform.bearer.<token>`.
//! Messages are JSON [`LiveEvent`]s filtered by tenant, site scope and
//! permission. The stream closes when the caller's session expires.

use std::sync::Arc;

use axum::extract::ws::{CloseFrame, Message, Utf8Bytes, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::response::Response;
use platform_auth::{Permission, Principal, Resource};
use platform_common::{ScannerId, now_utc};
use serde::Deserialize;
use tokio::sync::broadcast::error::RecvError;
use utoipa::IntoParams;

use crate::error::ApiError;
use crate::hub::{LiveEvent, LiveKind};
use crate::state::{AppState, Auth};

/// Stream filters.
#[derive(Debug, Deserialize, IntoParams)]
pub struct StreamQuery {
    /// Only this scanner.
    pub scanner_id: Option<ScannerId>,
}

fn visible(p: &Principal, e: &LiveEvent, only: Option<ScannerId>) -> bool {
    let perm = match e.kind {
        LiveKind::Telemetry | LiveKind::Audio => Permission::ScannerView,
        LiveKind::Event => Permission::EventView,
    };
    only.is_none_or(|s| s == e.scanner_id)
        && p.authorize(
            perm,
            Resource {
                tenant_id: e.tenant_id,
                site_id: Some(e.site_id),
            },
            now_utc(),
        )
        .is_ok()
}

/// Opens a live stream.
#[utoipa::path(get, path = "/api/v1/events/stream", params(StreamQuery), responses((status = 101, description = "WebSocket upgrade")), security(("bearer" = [])))]
pub async fn live(
    State(s): State<Arc<AppState>>,
    auth: Auth,
    Query(q): Query<StreamQuery>,
    ws: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    s.authorize(
        &auth,
        Permission::ScannerView,
        Resource {
            tenant_id: auth.principal.tenant_id,
            site_id: None,
        },
        None,
    )
    .await?;
    let rx = s.hub.subscribe();
    Ok(ws
        .protocols(["radio-platform.v1"])
        .max_message_size(4096)
        .on_upgrade(move |socket| pump(socket, auth.principal, rx, q.scanner_id)))
}

async fn pump(
    mut socket: WebSocket,
    p: Principal,
    mut rx: tokio::sync::broadcast::Receiver<LiveEvent>,
    only: Option<ScannerId>,
) {
    let remaining = (p.expires_at - now_utc()).max(time::Duration::ZERO);
    let expiry = tokio::time::sleep(std::time::Duration::try_from(remaining).unwrap_or_default());
    tokio::pin!(expiry);
    loop {
        tokio::select! {
            () = &mut expiry => {
                let _ = socket.send(Message::Close(Some(CloseFrame { code: 4001, reason: Utf8Bytes::from_static("session expired") }))).await;
                break;
            }
            ev = rx.recv() => match ev {
                Ok(e) if visible(&p, &e, only) => {
                    let Ok(json) = serde_json::to_string(&e) else { continue };
                    if socket.send(Message::Text(Utf8Bytes::from(json))).await.is_err() { break; }
                }
                Ok(_) => {}
                Err(RecvError::Lagged(n)) => {
                    let notice = serde_json::json!({ "kind": "lagged", "skipped": n }).to_string();
                    if socket.send(Message::Text(Utf8Bytes::from(notice))).await.is_err() { break; }
                }
                Err(RecvError::Closed) => break,
            },
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Close(_)) | Err(_)) | None => break,
                _ => {}
            },
        }
    }
}
