//! Audit API (Phase 6): filters, chain verification, and an audited export
//! that can be verified offline.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::collections::BTreeMap;
use std::sync::Arc;

use api_gateway::config::LimitsConfig;
use api_gateway::{AppState, public_app};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use platform_auth::testing::TestIssuer;
use platform_auth::{OidcConfig, OidcVerifier, Permission};
use platform_common::TenantId;
use platform_storage::audit_chain::Verifier;
use platform_storage::model::AuditRecord;
use platform_storage::{MemoryStore, Store};
use serde_json::{Value, json};
use tower::ServiceExt;

#[tokio::test]
async fn verify_filter_and_export() {
    let issuer = TestIssuer::new("https://idp.example.test", "radio-platform");
    let mut oidc = OidcConfig::new("https://idp.example.test", "radio-platform");
    oidc.role_permissions = BTreeMap::from([
        ("admin".into(), Permission::ALL.to_vec()),
        ("auditor".into(), vec![Permission::AuditView]),
    ]);
    let store = Arc::new(MemoryStore::new());
    let state = AppState::new(
        Arc::clone(&store) as Arc<dyn Store>,
        Arc::new(OidcVerifier::with_static_jwks(oidc, issuer.jwks())),
        LimitsConfig::default(),
    );
    let app = public_app(state);
    let tenant = TenantId::new();
    let tok = |role: &str| {
        issuer.token(
            role,
            600,
            json!({ "tenant_id": tenant.to_string(), "roles": [role], "sites": ["*"] }),
        )
    };
    let call = |method: &str, uri: String, role: &str, body: Option<Value>| {
        let app = app.clone();
        let t = tok(role);
        let method = method.to_owned();
        async move {
            let mut b = Request::builder()
                .method(method.as_str())
                .uri(uri)
                .header("authorization", format!("Bearer {t}"));
            let req = match body {
                Some(v) => {
                    b = b.header("content-type", "application/json");
                    b.body(Body::from(v.to_string())).unwrap()
                }
                None => b.body(Body::empty()).unwrap(),
            };
            let r = app.oneshot(req).await.unwrap();
            let status = r.status();
            let headers = r.headers().clone();
            let bytes = r.into_body().collect().await.unwrap().to_bytes();
            (
                status,
                headers,
                String::from_utf8_lossy(&bytes).into_owned(),
            )
        }
    };
    // Generate audited actions: sites created (success) and a denial.
    for i in 0..3 {
        let (s, _, _) = call(
            "POST",
            "/api/v1/sites".into(),
            "admin",
            Some(json!({ "name": format!("S{i}"), "allowed_networks": ["10.0.0.0/24"] })),
        )
        .await;
        assert_eq!(s, StatusCode::CREATED);
    }
    let (s, _, _) = call(
        "POST",
        "/api/v1/sites".into(),
        "auditor",
        Some(json!({ "name": "X", "allowed_networks": ["10.0.1.0/24"] })),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    let (s, _, v) = call("GET", "/api/v1/audit/verify".into(), "auditor", None).await;
    assert_eq!(s, StatusCode::OK);
    let v: Value = serde_json::from_str(&v).unwrap();
    assert_eq!(v["ok"], true, "{v}");
    assert_eq!(v["verified"], 4);
    assert_eq!(v["head_seq"], 4);

    let (_, _, page) = call(
        "GET",
        "/api/v1/audit?outcome=denied".into(),
        "auditor",
        None,
    )
    .await;
    let page: Value = serde_json::from_str(&page).unwrap();
    assert_eq!(page["items"].as_array().unwrap().len(), 1);
    assert_eq!(page["items"][0]["actor"], "auditor");
    let (_, _, page) = call(
        "GET",
        "/api/v1/audit?action=site.&actor=admin".into(),
        "auditor",
        None,
    )
    .await;
    assert_eq!(
        serde_json::from_str::<Value>(&page).unwrap()["items"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    let (s, _, _) = call(
        "GET",
        format!("/api/v1/audit?actor={}", "a".repeat(300)),
        "auditor",
        None,
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    // Export needs `export` as well; the denial is audited.
    let (s, _, _) = call("GET", "/api/v1/audit/export".into(), "auditor", None).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, h, body) = call("GET", "/api/v1/audit/export".into(), "admin", None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(h["content-type"], "application/jsonl");
    assert!(
        h["content-disposition"]
            .to_str()
            .unwrap()
            .contains("audit.jsonl")
    );
    // The export verifies offline, oldest first.
    let rows: Vec<AuditRecord> = body
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(rows.len(), 5, "4 actions + the denied export");
    let mut ver = Verifier::default();
    for r in &rows {
        ver.push(r).unwrap();
    }
    // The export itself is audited (after the snapshot it returned).
    let head = store.audit_head(tenant).await.unwrap();
    assert_eq!(head.seq, 6);
    let last = store.audit_chain(tenant, 5, 10).await.unwrap();
    assert_eq!(last[0].action, "audit.export");
    assert_eq!(last[0].details["records"], 5);
    let (s, h, csv) = call(
        "GET",
        "/api/v1/audit/export?format=csv&outcome=denied".into(),
        "admin",
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert!(h["content-type"].to_str().unwrap().starts_with("text/csv"));
    assert_eq!(csv.lines().count(), 3, "header + 2 denials: {csv}");
}
