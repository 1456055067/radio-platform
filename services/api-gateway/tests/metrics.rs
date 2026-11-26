//! Prometheus metrics: route templates, never concrete identifiers.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::sync::Arc;

use api_gateway::config::LimitsConfig;
use api_gateway::{AppState, public_app};
use axum::body::Body;
use axum::http::Request;
use http_body_util::BodyExt;
use platform_auth::testing::TestIssuer;
use platform_auth::{OidcConfig, OidcVerifier, Permission};
use platform_storage::MemoryStore;
use serde_json::json;
use tower::ServiceExt;

#[tokio::test]
async fn http_metrics_use_route_templates() {
    let issuer = TestIssuer::new("https://idp.example.test", "radio-platform");
    let mut oidc = OidcConfig::new("https://idp.example.test", "radio-platform");
    oidc.role_permissions = [("admin".to_owned(), Permission::ALL.to_vec())].into();
    let state = AppState::new(
        Arc::new(MemoryStore::new()),
        Arc::new(OidcVerifier::with_static_jwks(oidc, issuer.jwks())),
        LimitsConfig::default(),
    );
    let app = public_app(Arc::clone(&state));
    let tok = issuer.token("a", 600, json!({ "tenant_id": uuid::Uuid::new_v4().to_string(), "roles": ["admin"], "sites": ["*"] }));
    let id = uuid::Uuid::new_v4();
    for uri in [
        format!("/api/v1/scanners/{id}"),
        "/api/v1/me".to_owned(),
        "/healthz".to_owned(),
    ] {
        let req = Request::get(uri)
            .header("authorization", format!("Bearer {tok}"))
            .body(Body::empty())
            .unwrap();
        app.clone().oneshot(req).await.unwrap();
    }
    let req = Request::get("/metrics").body(Body::empty()).unwrap();
    let resp = api_gateway::metrics::router(Arc::clone(&state))
        .oneshot(req)
        .await
        .unwrap();
    assert!(
        resp.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/plain; version=0.0.4")
    );
    let text = String::from_utf8(
        resp.into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
    .unwrap();
    assert!(text.contains(r#"radio_http_requests_total{method="GET",route="/api/v1/scanners/{id}",status="404"} 1"#), "{text}");
    assert!(
        text.contains(
            r#"radio_http_requests_total{method="GET",route="/api/v1/me",status="200"} 1"#
        )
    );
    assert!(text.contains(
        r#"radio_http_request_duration_seconds_bucket{method="GET",route="/api/v1/me",le="+Inf"} 1"#
    ));
    assert!(
        !text.contains(&id.to_string()),
        "identifiers never reach labels"
    );
    assert!(text.contains("radio_agents_connected 0"));
    assert!(text.contains("# TYPE radio_http_request_duration_seconds histogram"));
}
