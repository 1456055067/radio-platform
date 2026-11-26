//! REST API tests: authentication, ABAC, tenant/site isolation, SSRF
//! policy, leases, validation, audit and rate limiting.

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
use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
use platform_auth::testing::TestIssuer;
use platform_auth::{OidcConfig, OidcVerifier, Permission};
use platform_common::TenantId;
use platform_storage::MemoryStore;
use serde_json::{Value, json};
use tower::ServiceExt;

const ISS: &str = "https://idp.example.test";
const AUD: &str = "radio-platform";

struct Ctx {
    app: Router,
    issuer: TestIssuer,
    tenant: TenantId,
}

fn roles() -> BTreeMap<String, Vec<Permission>> {
    BTreeMap::from([
        ("admin".into(), Permission::ALL.to_vec()),
        (
            "operator".into(),
            vec![
                Permission::ScannerView,
                Permission::ScannerControl,
                Permission::EventView,
            ],
        ),
        (
            "viewer".into(),
            vec![Permission::ScannerView, Permission::EventView],
        ),
    ])
}

fn ctx_with(limits: LimitsConfig) -> Ctx {
    let issuer = TestIssuer::new(ISS, AUD);
    let mut oidc = OidcConfig::new(ISS, AUD);
    oidc.role_permissions = roles();
    let verifier = OidcVerifier::with_static_jwks(oidc, issuer.jwks());
    let state = AppState::new(Arc::new(MemoryStore::new()), Arc::new(verifier), limits);
    Ctx {
        app: public_app(state),
        issuer,
        tenant: TenantId::new(),
    }
}

fn ctx() -> Ctx {
    ctx_with(LimitsConfig::default())
}

impl Ctx {
    fn token(&self, sub: &str, role: &str, sites: Value) -> String {
        self.token_for(self.tenant, sub, role, sites)
    }
    fn token_for(&self, tenant: TenantId, sub: &str, role: &str, sites: Value) -> String {
        self.issuer.token(
            sub,
            600,
            json!({ "tenant_id": tenant.to_string(), "roles": [role], "sites": sites }),
        )
    }
    async fn call(
        &self,
        method: Method,
        uri: &str,
        token: Option<&str>,
        body: Option<Value>,
        headers: &[(&str, &str)],
    ) -> (StatusCode, Value) {
        let mut b = Request::builder().method(method).uri(uri);
        if let Some(t) = token {
            b = b.header("authorization", format!("Bearer {t}"));
        }
        for (k, v) in headers {
            b = b.header(*k, *v);
        }
        let req = match body {
            Some(v) => b
                .header("content-type", "application/json")
                .body(Body::from(v.to_string()))
                .unwrap(),
            None => b.body(Body::empty()).unwrap(),
        };
        let resp = self.app.clone().oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let v = serde_json::from_slice(&bytes)
            .unwrap_or(Value::String(String::from_utf8_lossy(&bytes).into_owned()));
        (status, v)
    }
}

async fn seed(c: &Ctx) -> (String, String, String, String) {
    let admin = c.token("admin", "admin", json!(["*"]));
    let (s, site) = c
        .call(
            Method::POST,
            "/api/v1/sites",
            Some(&admin),
            Some(json!({ "name": "Site A", "allowed_networks": ["192.168.50.0/24"] })),
            &[],
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{site}");
    let site_id = site["id"].as_str().unwrap().to_owned();
    let (s, _) = c
        .call(
            Method::POST,
            "/api/v1/agents",
            Some(&admin),
            Some(json!({ "agent_id": "agent-a", "site_id": site_id })),
            &[],
        )
        .await;
    assert_eq!(s, StatusCode::CREATED);
    let (s, sc) = c
        .call(Method::POST, "/api/v1/scanners", Some(&admin), Some(json!({ "site_id": site_id, "agent_id": "agent-a", "name": "SDS200 #1", "host": "192.168.50.10" })), &[])
        .await;
    assert_eq!(s, StatusCode::CREATED, "{sc}");
    assert_eq!(sc["agent_connected"], false);
    (
        admin,
        site_id,
        sc["id"].as_str().unwrap().to_owned(),
        "agent-a".into(),
    )
}

#[tokio::test]
async fn authentication_is_required() {
    let c = ctx();
    let (s, body) = c
        .call(Method::GET, "/api/v1/scanners", None, None, &[])
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert_eq!(body["status"], 401);
    let (s, _) = c
        .call(Method::GET, "/api/v1/scanners", Some("garbage"), None, &[])
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let impostor = TestIssuer::new(ISS, AUD).token(
        "x",
        600,
        json!({ "tenant_id": c.tenant.to_string(), "roles": ["admin"] }),
    );
    let (s, _) = c
        .call(Method::GET, "/api/v1/scanners", Some(&impostor), None, &[])
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    // Unauthenticated endpoints.
    assert_eq!(
        c.call(Method::GET, "/healthz", None, None, &[]).await.0,
        StatusCode::OK
    );
    assert_eq!(
        c.call(Method::GET, "/readyz", None, None, &[]).await.0,
        StatusCode::OK
    );
    let (s, doc) = c
        .call(Method::GET, "/api/v1/openapi.json", None, None, &[])
        .await;
    assert_eq!(s, StatusCode::OK);
    assert!(doc["paths"]["/api/v1/scanners/{id}/volume"]["put"].is_object());
}

#[tokio::test]
async fn permissions_and_audit_of_denials() {
    let c = ctx();
    let (admin, site_id, _, _) = seed(&c).await;
    let viewer = c.token("vera", "viewer", json!(["*"]));
    let (s, _) = c
        .call(
            Method::POST,
            "/api/v1/sites",
            Some(&viewer),
            Some(json!({ "name": "X", "allowed_networks": [] })),
            &[],
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = c
        .call(Method::POST, "/api/v1/scanners", Some(&viewer), Some(json!({ "site_id": site_id, "agent_id": "agent-a", "name": "n", "host": "192.168.50.11" })), &[])
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    // Viewer cannot read audit; admin sees the denials.
    assert_eq!(
        c.call(Method::GET, "/api/v1/audit", Some(&viewer), None, &[])
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let (s, audit) = c
        .call(Method::GET, "/api/v1/audit", Some(&admin), None, &[])
        .await;
    assert_eq!(s, StatusCode::OK);
    let denied: Vec<&Value> = audit["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|a| a["outcome"] == "denied")
        .collect();
    assert_eq!(denied.len(), 2, "{audit}");
    assert!(denied.iter().all(|a| a["actor"] == "vera"));
}

#[tokio::test]
async fn enrollment_network_policy_prevents_ssrf() {
    let c = ctx();
    let (admin, site_id, _, _) = seed(&c).await;
    for (host, port) in [
        ("169.254.169.254", 50536),
        ("127.0.0.1", 50536),
        ("10.0.0.5", 50536),
        ("192.168.50.20", 22),
    ] {
        let (s, body) = c
            .call(Method::POST, "/api/v1/scanners", Some(&admin), Some(json!({ "site_id": site_id, "agent_id": "agent-a", "name": "x", "host": host, "port": port })), &[])
            .await;
        assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{host}:{port} {body}");
    }
    let (s, _) = c
        .call(Method::POST, "/api/v1/scanners", Some(&admin), Some(json!({ "site_id": site_id, "agent_id": "agent-a", "name": "dup", "host": "192.168.50.10" })), &[])
        .await;
    assert_eq!(s, StatusCode::CONFLICT);
    // Sites cannot authorize the whole address space.
    let (s, _) = c
        .call(
            Method::POST,
            "/api/v1/sites",
            Some(&admin),
            Some(json!({ "name": "Any", "allowed_networks": ["0.0.0.0/0"] })),
            &[],
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn tenant_and_site_isolation() {
    let c = ctx();
    let (_, _, scanner, _) = seed(&c).await;
    let other_tenant = c.token_for(TenantId::new(), "mallory", "admin", json!(["*"]));
    let (s, _) = c
        .call(
            Method::GET,
            &format!("/api/v1/scanners/{scanner}"),
            Some(&other_tenant),
            None,
            &[],
        )
        .await;
    assert_eq!(
        s,
        StatusCode::NOT_FOUND,
        "other tenants must not learn the scanner exists"
    );
    let (_, list) = c
        .call(
            Method::GET,
            "/api/v1/scanners",
            Some(&other_tenant),
            None,
            &[],
        )
        .await;
    assert_eq!(list["items"].as_array().unwrap().len(), 0);
    // Agent identities cannot be hijacked across tenants.
    let (_, site) = c
        .call(
            Method::POST,
            "/api/v1/sites",
            Some(&other_tenant),
            Some(json!({ "name": "M", "allowed_networks": ["10.9.0.0/16"] })),
            &[],
        )
        .await;
    let (s, _) = c
        .call(
            Method::POST,
            "/api/v1/agents",
            Some(&other_tenant),
            Some(json!({ "agent_id": "agent-a", "site_id": site["id"] })),
            &[],
        )
        .await;
    assert_eq!(s, StatusCode::CONFLICT);

    let scoped = c.token(
        "sam",
        "viewer",
        json!([platform_common::SiteId::new().to_string()]),
    );
    let (s, _) = c
        .call(
            Method::GET,
            &format!("/api/v1/scanners/{scanner}"),
            Some(&scoped),
            None,
            &[],
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (_, list) = c
        .call(Method::GET, "/api/v1/scanners", Some(&scoped), None, &[])
        .await;
    assert_eq!(
        list["items"].as_array().unwrap().len(),
        0,
        "site scope filters listings"
    );
}

#[tokio::test]
async fn leases_gate_control_commands() {
    let c = ctx();
    let (admin, _, scanner, _) = seed(&c).await;
    let alice = c.token("alice", "operator", json!(["*"]));
    let bob = c.token("bob", "operator", json!(["*"]));
    let viewer = c.token("vera", "viewer", json!(["*"]));
    let vol = format!("/api/v1/scanners/{scanner}/volume");
    let leases = format!("/api/v1/scanners/{scanner}/leases");

    // Without a lease.
    let (s, _) = c
        .call(
            Method::PUT,
            &vol,
            Some(&alice),
            Some(json!({ "level": 5 })),
            &[],
        )
        .await;
    assert_eq!(s, StatusCode::PRECONDITION_REQUIRED);
    assert_eq!(
        c.call(Method::POST, &leases, Some(&viewer), None, &[])
            .await
            .0,
        StatusCode::FORBIDDEN
    );

    let (s, lease) = c
        .call(
            Method::POST,
            &leases,
            Some(&alice),
            Some(json!({ "ttl_secs": 120 })),
            &[],
        )
        .await;
    assert_eq!(s, StatusCode::CREATED);
    let lease_id = lease["lease_id"].as_str().unwrap().to_owned();
    let (s, _) = c.call(Method::POST, &leases, Some(&bob), None, &[]).await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (s, _) = c
        .call(
            Method::POST,
            &leases,
            Some(&alice),
            Some(json!({ "ttl_secs": 999_999 })),
            &[],
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    // Detail shows the holder; only the holder sees the lease id.
    let (_, d) = c
        .call(
            Method::GET,
            &format!("/api/v1/scanners/{scanner}"),
            Some(&bob),
            None,
            &[],
        )
        .await;
    assert_eq!(d["control"]["holder"], "alice");
    assert!(d["control"]["lease_id"].is_null());

    // Bob cannot use Alice's lease; Alice can, but the agent is offline.
    let (s, _) = c
        .call(
            Method::PUT,
            &vol,
            Some(&bob),
            Some(json!({ "level": 5 })),
            &[("x-lease-id", &lease_id)],
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = c
        .call(
            Method::PUT,
            &vol,
            Some(&alice),
            Some(json!({ "level": 5 })),
            &[("x-lease-id", &lease_id)],
        )
        .await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE);
    let (s, _) = c
        .call(
            Method::PUT,
            &vol,
            Some(&alice),
            Some(json!({ "level": 5 })),
            &[("x-lease-id", "not-a-uuid")],
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    // Reads need no lease.
    assert_eq!(
        c.call(Method::GET, &vol, Some(&viewer), None, &[]).await.0,
        StatusCode::SERVICE_UNAVAILABLE
    );

    // Raw commands need scanner_admin.
    let raw = json!({ "command": "raw", "name": "MDL", "args": [], "read_only": true });
    let cmd = format!("/api/v1/scanners/{scanner}/commands");
    assert_eq!(
        c.call(Method::POST, &cmd, Some(&alice), Some(raw.clone()), &[])
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        c.call(Method::POST, &cmd, Some(&admin), Some(raw), &[])
            .await
            .0,
        StatusCode::SERVICE_UNAVAILABLE
    );

    // An admin sees the lease id (to revoke it) but cannot command with it.
    let (_, d) = c
        .call(
            Method::GET,
            &format!("/api/v1/scanners/{scanner}"),
            Some(&admin),
            None,
            &[],
        )
        .await;
    assert_eq!(d["control"]["lease_id"], lease_id.as_str());
    let (s, _) = c
        .call(
            Method::PUT,
            &vol,
            Some(&admin),
            Some(json!({ "level": 5 })),
            &[("x-lease-id", &lease_id)],
        )
        .await;
    assert_eq!(
        s,
        StatusCode::FORBIDDEN,
        "seeing a lease id is not holding it"
    );

    // Release: Bob cannot, an admin can force it.
    let del = format!("{leases}/{lease_id}");
    assert_eq!(
        c.call(Method::DELETE, &del, Some(&bob), None, &[]).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        c.call(Method::DELETE, &del, Some(&admin), None, &[])
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        c.call(Method::POST, &leases, Some(&bob), None, &[]).await.0,
        StatusCode::CREATED
    );
}

#[tokio::test]
async fn validation_and_pagination() {
    let c = ctx();
    let (admin, site_id, scanner, _) = seed(&c).await;
    let (s, _) = c
        .call(Method::POST, "/api/v1/scanners", Some(&admin), Some(json!({ "site_id": site_id, "agent_id": "agent-a", "name": "x", "host": "192.168.50.30", "evil": true })), &[])
        .await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "unknown fields are rejected"
    );
    let (s, _) = c
        .call(
            Method::GET,
            "/api/v1/scanners?cursor=%21%21",
            Some(&admin),
            None,
            &[],
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    for i in 0..4 {
        c.call(Method::POST, "/api/v1/scanners", Some(&admin), Some(json!({ "site_id": site_id, "agent_id": "agent-a", "name": format!("s{i}"), "host": format!("192.168.50.{}", 40 + i) })), &[])
            .await;
    }
    let (_, p1) = c
        .call(
            Method::GET,
            "/api/v1/scanners?limit=3",
            Some(&admin),
            None,
            &[],
        )
        .await;
    assert_eq!(p1["items"].as_array().unwrap().len(), 3);
    let cursor = p1["next_cursor"].as_str().unwrap();
    let (_, p2) = c
        .call(
            Method::GET,
            &format!("/api/v1/scanners?limit=3&cursor={cursor}"),
            Some(&admin),
            None,
            &[],
        )
        .await;
    assert_eq!(p2["items"].as_array().unwrap().len(), 2);
    let (s, _) = c
        .call(
            Method::PATCH,
            &format!("/api/v1/scanners/{scanner}"),
            Some(&admin),
            Some(json!({ "host": "8.8.8.8" })),
            &[],
        )
        .await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "updates are re-checked against the network policy"
    );
    let big = json!({ "name": "x".repeat(70_000), "allowed_networks": [] });
    assert_eq!(
        c.call(Method::POST, "/api/v1/sites", Some(&admin), Some(big), &[])
            .await
            .0,
        StatusCode::PAYLOAD_TOO_LARGE
    );
    let (s, tx) = c
        .call(
            Method::GET,
            "/api/v1/transmissions?frequency_hz=154415000&from=2026-01-01T00:00:00Z",
            Some(&admin),
            None,
            &[],
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{tx}");
    assert_eq!(
        c.call(
            Method::DELETE,
            &format!("/api/v1/scanners/{scanner}"),
            Some(&admin),
            None,
            &[]
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        c.call(
            Method::GET,
            &format!("/api/v1/scanners/{scanner}"),
            Some(&admin),
            None,
            &[]
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn rate_limiting() {
    let c = ctx_with(LimitsConfig {
        requests_per_minute: 3,
        ..LimitsConfig::default()
    });
    let t = c.token("rita", "viewer", json!(["*"]));
    for _ in 0..3 {
        assert_eq!(
            c.call(Method::GET, "/api/v1/scanners", Some(&t), None, &[])
                .await
                .0,
            StatusCode::OK
        );
    }
    assert_eq!(
        c.call(Method::GET, "/api/v1/scanners", Some(&t), None, &[])
            .await
            .0,
        StatusCode::TOO_MANY_REQUESTS
    );
    // Limits are per subject.
    let other = c.token("other", "viewer", json!(["*"]));
    assert_eq!(
        c.call(Method::GET, "/api/v1/scanners", Some(&other), None, &[])
            .await
            .0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn me_and_agent_listing_respect_scope() {
    let c = ctx();
    let (_admin, site_id, _scanner, _agent) = seed(&c).await;
    let viewer = c.token("vera", "viewer", json!([site_id]));
    let (s, me) = c
        .call(Method::GET, "/api/v1/me", Some(&viewer), None, &[])
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(me["subject"], "vera");
    assert_eq!(me["tenant_id"], c.tenant.to_string());
    assert_eq!(me["sites"], json!([site_id]));
    assert_eq!(me["permissions"], json!(["scanner_view", "event_view"]));
    let admin = c.token("admin", "admin", json!(["*"]));
    let (_, me) = c
        .call(Method::GET, "/api/v1/me", Some(&admin), None, &[])
        .await;
    assert_eq!(me["sites"], Value::Null, "null means every site");

    let (s, agents) = c
        .call(Method::GET, "/api/v1/agents", Some(&viewer), None, &[])
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(agents["items"].as_array().unwrap().len(), 1);
    assert_eq!(agents["items"][0]["connected"], false);

    // Another site's viewer sees no agents; another tenant sees none either.
    let elsewhere = c.token("eve", "viewer", json!([uuid::Uuid::new_v4().to_string()]));
    let (_, agents) = c
        .call(Method::GET, "/api/v1/agents", Some(&elsewhere), None, &[])
        .await;
    assert_eq!(agents["items"], json!([]));
    let other = c.token_for(TenantId::new(), "mallory", "admin", json!(["*"]));
    let (_, agents) = c
        .call(Method::GET, "/api/v1/agents", Some(&other), None, &[])
        .await;
    assert_eq!(agents["items"], json!([]));
    let (s, _) = c
        .call(
            Method::GET,
            &format!("/api/v1/agents?site_id={}", uuid::Uuid::new_v4()),
            Some(&viewer),
            None,
            &[],
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "explicit site outside scope");
}

#[tokio::test]
async fn ui_is_served_with_security_headers() {
    use api_gateway::ui::{UiAuth, UiConfig};
    let dir = std::env::temp_dir().join(format!("rp-ui-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(dir.join("assets")).unwrap();
    std::fs::write(dir.join("index.html"), "<!doctype html><title>ui</title>").unwrap();
    std::fs::write(dir.join("assets/app-abc123.js"), "console.log(1)").unwrap();
    let ui = api_gateway::ui::router(&UiConfig {
        dir: dir.clone(),
        auth: UiAuth::Oidc {
            authority: "https://idp.example.test/realms/radio".into(),
            client_id: "radio-ui".into(),
            scope: "openid".into(),
        },
    })
    .unwrap();
    let issuer = TestIssuer::new(ISS, AUD);
    let mut oidc = OidcConfig::new(ISS, AUD);
    oidc.role_permissions = roles();
    let state = AppState::new(
        Arc::new(MemoryStore::new()),
        Arc::new(OidcVerifier::with_static_jwks(oidc, issuer.jwks())),
        LimitsConfig::default(),
    );
    let app = api_gateway::with_ui(state, Some(ui));
    let get = |uri: &str| {
        let app = app.clone();
        let req = Request::get(uri).body(Body::empty()).unwrap();
        async move { app.oneshot(req).await.unwrap() }
    };

    let r = get("/scanners/123/virtual").await;
    assert_eq!(
        r.status(),
        StatusCode::OK,
        "client routes fall back to index"
    );
    let csp = r.headers()["content-security-policy"]
        .to_str()
        .unwrap()
        .to_owned();
    assert!(csp.contains("script-src 'self';"));
    assert!(csp.contains("connect-src 'self' https://idp.example.test;"));
    assert_eq!(r.headers()["x-frame-options"], "DENY");
    assert_eq!(r.headers()["x-content-type-options"], "nosniff");
    assert_eq!(r.headers()["cache-control"], "no-cache");

    let r = get("/assets/app-abc123.js").await;
    assert_eq!(r.status(), StatusCode::OK);
    assert!(
        r.headers()["cache-control"]
            .to_str()
            .unwrap()
            .contains("immutable")
    );

    let r = get("/ui-config.json").await;
    let body = r.into_body().collect().await.unwrap().to_bytes();
    let cfg: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(cfg["auth"]["mode"], "oidc");
    assert_eq!(cfg["auth"]["client_id"], "radio-ui");

    let r = get("/api/v1/nope").await;
    assert_eq!(
        r.status(),
        StatusCode::NOT_FOUND,
        "API paths never fall back to the UI"
    );
    assert_eq!(r.headers()["content-type"], "application/problem+json");
    let r = get("/dev/token").await;
    assert_eq!(
        r.status(),
        StatusCode::NOT_FOUND,
        "the gateway has no dev token route"
    );
    let r = get("/api/v1/sites").await;
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(r.headers()["cache-control"], "no-store");
    let r = get("/../../etc/passwd").await;
    let body = r.into_body().collect().await.unwrap().to_bytes();
    assert!(
        !String::from_utf8_lossy(&body).contains("root:"),
        "no traversal"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn groups_respect_site_scope_and_tenant() {
    let c = ctx();
    let (admin, site_a, scanner_a, _) = seed(&c).await;
    // A second site with its own scanner.
    let (_, site_b) = c
        .call(
            Method::POST,
            "/api/v1/sites",
            Some(&admin),
            Some(json!({ "name": "Site B", "allowed_networks": ["10.9.0.0/24"] })),
            &[],
        )
        .await;
    let site_b = site_b["id"].as_str().unwrap().to_owned();
    c.call(
        Method::POST,
        "/api/v1/agents",
        Some(&admin),
        Some(json!({ "agent_id": "agent-b", "site_id": site_b })),
        &[],
    )
    .await;
    let (_, sb) = c
        .call(Method::POST, "/api/v1/scanners", Some(&admin), Some(json!({ "site_id": site_b, "agent_id": "agent-b", "name": "B1", "host": "10.9.0.5" })), &[])
        .await;
    let scanner_b = sb["id"].as_str().unwrap().to_owned();

    // Site-scoped managers cannot create tenant-wide groups.
    let local_mgr = c.issuer.token(
        "lucy",
        600,
        json!({ "tenant_id": c.tenant.to_string(), "roles": ["admin"], "sites": [site_a] }),
    );
    let (s, _) = c
        .call(
            Method::POST,
            "/api/v1/groups",
            Some(&local_mgr),
            Some(json!({ "name": "All" })),
            &[],
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    let (s, g) = c
        .call(
            Method::POST,
            "/api/v1/groups",
            Some(&admin),
            Some(json!({ "name": "Fire" })),
            &[],
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{g}");
    let gid = g["id"].as_str().unwrap().to_owned();
    for sc in [&scanner_a, &scanner_b] {
        let (s, _) = c
            .call(
                Method::PUT,
                &format!("/api/v1/groups/{gid}/members/{sc}"),
                Some(&admin),
                None,
                &[],
            )
            .await;
        assert_eq!(s, StatusCode::NO_CONTENT);
    }
    let (s, _) = c
        .call(
            Method::POST,
            "/api/v1/groups",
            Some(&admin),
            Some(json!({ "name": "Fire" })),
            &[],
        )
        .await;
    assert_eq!(s, StatusCode::CONFLICT);

    // The site-A viewer only sees site-A members.
    let viewer_a = c.token("vic", "viewer", json!([site_a]));
    let (_, g) = c
        .call(
            Method::GET,
            &format!("/api/v1/groups/{gid}"),
            Some(&viewer_a),
            None,
            &[],
        )
        .await;
    assert_eq!(g["members"], json!([scanner_a]));
    let (_, all) = c
        .call(Method::GET, "/api/v1/groups", Some(&admin), None, &[])
        .await;
    assert_eq!(all[0]["members"].as_array().unwrap().len(), 2);

    // A site-A manager cannot touch site-B membership but can manage site A.
    let (s, _) = c
        .call(
            Method::DELETE,
            &format!("/api/v1/groups/{gid}/members/{scanner_b}"),
            Some(&local_mgr),
            None,
            &[],
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = c
        .call(
            Method::DELETE,
            &format!("/api/v1/groups/{gid}/members/{scanner_a}"),
            Some(&local_mgr),
            None,
            &[],
        )
        .await;
    assert_eq!(s, StatusCode::NO_CONTENT);

    // Other tenants see nothing.
    let other = c.token_for(TenantId::new(), "mallory", "admin", json!(["*"]));
    let (s, _) = c
        .call(
            Method::GET,
            &format!("/api/v1/groups/{gid}"),
            Some(&other),
            None,
            &[],
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = c
        .call(
            Method::DELETE,
            &format!("/api/v1/groups/{gid}"),
            Some(&other),
            None,
            &[],
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    let (s, _) = c
        .call(
            Method::DELETE,
            &format!("/api/v1/groups/{gid}"),
            Some(&admin),
            None,
            &[],
        )
        .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (_, audit) = c
        .call(Method::GET, "/api/v1/audit", Some(&admin), None, &[])
        .await;
    let actions: Vec<&str> = audit["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|a| a["action"].as_str())
        .collect();
    assert!(actions.contains(&"group.delete") && actions.contains(&"group.member.add"));
}

#[tokio::test]
async fn per_ip_limit_applies_before_authentication() {
    let c = ctx_with(LimitsConfig {
        requests_per_minute_per_ip: 3,
        ..LimitsConfig::default()
    });
    let send = |ip: &str, path: &str| {
        let mut req = Request::get(path).body(Body::empty()).unwrap();
        req.extensions_mut().insert(api_gateway::serve::ClientAddr(
            format!("{ip}:5000").parse().unwrap(),
        ));
        let app = c.app.clone();
        async move { app.oneshot(req).await.unwrap() }
    };
    for _ in 0..3 {
        assert_eq!(
            send("198.51.100.1", "/api/v1/sites").await.status(),
            StatusCode::UNAUTHORIZED
        );
    }
    let r = send("198.51.100.1", "/api/v1/sites").await;
    assert_eq!(
        r.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "unauthenticated floods are cut off"
    );
    assert!(r.headers().contains_key("retry-after"));
    assert_eq!(
        send("198.51.100.2", "/api/v1/sites").await.status(),
        StatusCode::UNAUTHORIZED,
        "other clients unaffected"
    );
    assert_eq!(
        send("198.51.100.1", "/healthz").await.status(),
        StatusCode::OK,
        "probes exempt"
    );
}
