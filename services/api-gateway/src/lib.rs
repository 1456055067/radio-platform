//! Radio platform API gateway.
//!
//! Two listeners:
//! - **Public API** (`/api/v1`): TLS 1.3, OIDC bearer authentication, ABAC,
//!   per-subject rate limiting, RFC 9457 errors, OpenAPI at
//!   `/api/v1/openapi.json`, live updates over WebSocket.
//! - **Agent listener** (`/agent/v1`): TLS 1.3 with mandatory client
//!   certificates. Agents dial in, so sites need no inbound ports.

#![warn(missing_docs)]

pub mod agent_ws;
pub mod api;
pub mod cluster;
pub mod config;
pub mod crl;
pub mod error;
pub mod hub;
pub mod leases;
pub mod media;
pub mod metrics;
pub mod openapi;
pub mod retention;
pub mod serve;
pub mod state;
pub mod tls;
pub mod transcription;
pub mod ui;

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::http::{HeaderName, HeaderValue, header};
use axum::routing::get;
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::TraceLayer;

pub use state::AppState;

/// Public API router with standard middleware.
pub fn public_app(state: Arc<AppState>) -> Router {
    with_ui(state, None)
}

/// Public API router, optionally also serving the web UI (see [`ui`]).
pub fn with_ui(state: Arc<AppState>, ui: Option<Router>) -> Router {
    let body_limit = state.limits.max_body_bytes;
    let limiter_state = Arc::clone(&state);
    let metrics_state = Arc::clone(&state);
    let api = api::router(state)
        .layer(axum::middleware::from_fn_with_state(
            metrics_state,
            metrics::track,
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-store"),
        ));
    let app = match ui {
        Some(ui) => api.merge(ui),
        None => api,
    };
    security_headers(app)
        .layer(axum::middleware::from_fn_with_state(
            limiter_state,
            state::ip_rate_limit,
        ))
        .layer(RequestBodyLimitLayer::new(body_limit))
        .layer(TimeoutLayer::with_status_code(
            axum::http::StatusCode::REQUEST_TIMEOUT,
            Duration::from_secs(60),
        ))
        .layer(TraceLayer::new_for_http())
}

fn security_headers(app: Router) -> Router {
    let set = |name: HeaderName, value: &'static str| {
        SetResponseHeaderLayer::if_not_present(name, HeaderValue::from_static(value))
    };
    app.layer(set(header::X_CONTENT_TYPE_OPTIONS, "nosniff"))
        .layer(set(header::X_FRAME_OPTIONS, "DENY"))
        .layer(set(header::REFERRER_POLICY, "no-referrer"))
        .layer(set(
            HeaderName::from_static("cross-origin-opener-policy"),
            "same-origin",
        ))
        .layer(set(
            HeaderName::from_static("permissions-policy"),
            "camera=(), microphone=(), geolocation=(), payment=()",
        ))
}

/// Agent listener router.
pub fn agent_app(state: Arc<AppState>) -> Router {
    Router::new()
        .route(
            platform_common::wire::AGENT_PATH,
            get(agent_ws::agent_socket),
        )
        .with_state(state)
        .layer(TraceLayer::new_for_http())
}
