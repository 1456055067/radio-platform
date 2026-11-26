//! Route-by-route authorization matrix (Phase 6).
//!
//! Every route in the OpenAPI document must appear in [`ROUTES`] with the
//! permissions it needs. For each route, on a freshly seeded gateway:
//! - no token → 401;
//! - a token with every permission → not 401/403;
//! - for each required permission, a token with every permission **except**
//!   that one → 403 (the permission is necessary);
//! - a token for another tenant with every permission → never 2xx on
//!   tenant-owned resources.
//!
//! Adding a route without adding it here fails `table_covers_every_route`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use api_gateway::config::LimitsConfig;
use api_gateway::{AppState, public_app};
use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
use platform_auth::testing::TestIssuer;
use platform_auth::{OidcConfig, OidcVerifier, Permission};
use platform_common::{TenantId, TransmissionId, TransmissionStart};
use platform_storage::{MemoryStore, Store};
use serde_json::{Value, json};
use tower::ServiceExt;

const ISS: &str = "https://idp.example.test";
const AUD: &str = "radio-platform";

use Permission::*;

/// Permissions a route needs. `&[]` means any authenticated caller.
struct Route {
    method: &'static str,
    path: &'static str,
    needs: &'static [Permission],
    body: Option<&'static str>,
    /// Tenant-owned resource in the path (cross-tenant access must fail).
    owned: bool,
}

const fn r(
    method: &'static str,
    path: &'static str,
    needs: &'static [Permission],
    body: Option<&'static str>,
    owned: bool,
) -> Route {
    Route {
        method,
        path,
        needs,
        body,
        owned,
    }
}

const ROUTES: &[Route] = &[
    r("GET", "/api/v1/me", &[], None, false),
    r("GET", "/api/v1/sites", &[ScannerView], None, false),
    r(
        "POST",
        "/api/v1/sites",
        &[SiteManage],
        Some(r#"{"name":"Second","allowed_networks":["10.9.0.0/24"]}"#),
        false,
    ),
    r(
        "POST",
        "/api/v1/sites/{site}/discover",
        &[ScannerManage],
        Some(r#"{"agent_id":"agent-a","networks":["192.168.50.0/30"]}"#),
        true,
    ),
    r("GET", "/api/v1/agents", &[ScannerView], None, false),
    r(
        "POST",
        "/api/v1/agents",
        &[SiteManage],
        Some(r#"{"agent_id":"agent-z","site_id":"{site}"}"#),
        false,
    ),
    r("GET", "/api/v1/scanners", &[ScannerView], None, false),
    r(
        "POST",
        "/api/v1/scanners",
        &[ScannerManage],
        Some(r#"{"site_id":"{site}","agent_id":"agent-a","name":"New","host":"192.168.50.30"}"#),
        false,
    ),
    r(
        "GET",
        "/api/v1/scanners/{scanner}",
        &[ScannerView],
        None,
        true,
    ),
    r(
        "PATCH",
        "/api/v1/scanners/{scanner}",
        &[ScannerManage],
        Some(r#"{"name":"Renamed"}"#),
        true,
    ),
    r(
        "DELETE",
        "/api/v1/scanners/{scanner}",
        &[ScannerManage],
        None,
        true,
    ),
    r(
        "GET",
        "/api/v1/scanners/{scanner}/status",
        &[ScannerView],
        None,
        true,
    ),
    r(
        "GET",
        "/api/v1/scanners/{scanner}/capabilities",
        &[ScannerView],
        None,
        true,
    ),
    r(
        "GET",
        "/api/v1/scanners/{scanner}/telemetry",
        &[ScannerView],
        None,
        true,
    ),
    r(
        "GET",
        "/api/v1/scanners/{scanner}/volume",
        &[ScannerView],
        None,
        true,
    ),
    r(
        "PUT",
        "/api/v1/scanners/{scanner}/volume",
        &[ScannerControl],
        Some(r#"{"level":5}"#),
        true,
    ),
    r(
        "GET",
        "/api/v1/scanners/{scanner}/squelch",
        &[ScannerView],
        None,
        true,
    ),
    r(
        "PUT",
        "/api/v1/scanners/{scanner}/squelch",
        &[ScannerControl],
        Some(r#"{"level":2}"#),
        true,
    ),
    r(
        "POST",
        "/api/v1/scanners/{scanner}/hold",
        &[ScannerControl],
        Some(r#"{"kind":"system","index":1}"#),
        true,
    ),
    r(
        "POST",
        "/api/v1/scanners/{scanner}/resume",
        &[ScannerControl],
        None,
        true,
    ),
    r(
        "POST",
        "/api/v1/scanners/{scanner}/commands",
        &[ScannerView],
        Some(r#"{"command":"get_status"}"#),
        true,
    ),
    r(
        "POST",
        "/api/v1/scanners/{scanner}/leases",
        &[ScannerControl],
        Some("{}"),
        true,
    ),
    r(
        "DELETE",
        "/api/v1/scanners/{scanner}/leases/{lease}",
        &[ScannerControl],
        None,
        true,
    ),
    r("GET", "/api/v1/groups", &[ScannerView], None, false),
    r(
        "POST",
        "/api/v1/groups",
        &[ScannerManage],
        Some(r#"{"name":"New group"}"#),
        false,
    ),
    r("GET", "/api/v1/groups/{group}", &[ScannerView], None, true),
    r(
        "DELETE",
        "/api/v1/groups/{group}",
        &[ScannerManage],
        None,
        true,
    ),
    r(
        "PUT",
        "/api/v1/groups/{group}/members/{scanner}",
        &[ScannerManage],
        None,
        true,
    ),
    r(
        "DELETE",
        "/api/v1/groups/{group}/members/{scanner}",
        &[ScannerManage],
        None,
        true,
    ),
    r(
        "GET",
        "/api/v1/scanners/{scanner}/audio",
        &[ScannerView],
        None,
        true,
    ),
    r(
        "POST",
        "/api/v1/scanners/{scanner}/audio/whep",
        &[AudioListen],
        Some("v=0"),
        true,
    ),
    // Ending a session needs only authentication; ownership is checked
    // against the session (unknown sessions are a no-op).
    r(
        "DELETE",
        "/api/v1/audio/sessions/{session}",
        &[],
        None,
        false,
    ),
    r("GET", "/api/v1/media/status", &[ScannerView], None, false),
    r("GET", "/api/v1/transmissions", &[EventView], None, false),
    r(
        "GET",
        "/api/v1/transmissions/{tx}",
        &[EventView],
        None,
        true,
    ),
    r(
        "GET",
        "/api/v1/transmissions/{tx}/audio",
        &[EventView, AudioListen],
        None,
        true,
    ),
    r(
        "DELETE",
        "/api/v1/transmissions/{tx}/audio",
        &[EventView, RecordingManage],
        None,
        true,
    ),
    r(
        "POST",
        "/api/v1/transmissions/{tx}/transcribe",
        &[EventView, TranscriptionManage],
        None,
        true,
    ),
    r(
        "PUT",
        "/api/v1/transmissions/{tx}/hold",
        &[EventView, RecordingManage],
        Some(r#"{"hold":true}"#),
        true,
    ),
    r(
        "GET",
        "/api/v1/transcription/status",
        &[EventView],
        None,
        false,
    ),
    r("GET", "/api/v1/retention", &[EventView], None, false),
    r(
        "GET",
        "/api/v1/analytics/activity",
        &[EventView],
        None,
        false,
    ),
    r("GET", "/api/v1/analytics/top", &[EventView], None, false),
    r("GET", "/api/v1/events", &[EventView], None, false),
    r("GET", "/api/v1/audit", &[AuditView], None, false),
    r("GET", "/api/v1/audit/verify", &[AuditView], None, false),
    r(
        "GET",
        "/api/v1/audit/export",
        &[AuditView, Export],
        None,
        false,
    ),
];

/// Routes not authorized by bearer token in the usual way, checked
/// separately: the WebSocket stream (token in a subprotocol) and the
/// public OpenAPI document.
const SPECIAL: &[(&str, &str)] = &[
    ("GET", "/api/v1/events/stream"),
    ("GET", "/api/v1/openapi.json"),
];

fn roles() -> BTreeMap<String, Vec<Permission>> {
    let mut m = BTreeMap::from([
        ("all".to_owned(), Permission::ALL.to_vec()),
        ("none".to_owned(), Vec::new()),
    ]);
    for p in Permission::ALL {
        m.insert(
            format!("all-but-{p:?}"),
            Permission::ALL
                .iter()
                .copied()
                .filter(|x| *x != p)
                .collect(),
        );
    }
    m
}

struct Ctx {
    app: Router,
    issuer: TestIssuer,
    tenant: TenantId,
    ids: BTreeMap<&'static str, String>,
}

impl Ctx {
    fn token(&self, tenant: TenantId, role: &str) -> String {
        self.issuer.token(
            "tester",
            600,
            json!({ "tenant_id": tenant.to_string(), "roles": [role], "sites": ["*"] }),
        )
    }

    fn fill(&self, s: &str) -> String {
        let mut out = s.to_owned();
        for (k, v) in &self.ids {
            out = out.replace(&format!("{{{k}}}"), v);
        }
        out
    }

    async fn call(&self, route: &Route, token: Option<&str>) -> (StatusCode, String) {
        let mut b = Request::builder()
            .method(Method::from_bytes(route.method.as_bytes()).unwrap())
            .uri(self.fill(route.path));
        if let Some(t) = token {
            b = b.header("authorization", format!("Bearer {t}"));
        }
        let req = match route.body {
            Some(body) if route.path.ends_with("/whep") => b
                .header("content-type", "application/sdp")
                .body(Body::from(body.to_owned()))
                .unwrap(),
            Some(body) => b
                .header("content-type", "application/json")
                .body(Body::from(self.fill(body)))
                .unwrap(),
            None => b.body(Body::empty()).unwrap(),
        };
        let resp = self.app.clone().oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }
}

async fn seeded() -> Ctx {
    let issuer = TestIssuer::new(ISS, AUD);
    let mut oidc = OidcConfig::new(ISS, AUD);
    oidc.role_permissions = roles();
    let verifier = OidcVerifier::with_static_jwks(oidc, issuer.jwks());
    let store = Arc::new(MemoryStore::new());
    let state = AppState::new(
        Arc::clone(&store) as Arc<dyn Store>,
        Arc::new(verifier),
        LimitsConfig::default(),
    );
    let mut c = Ctx {
        app: public_app(state),
        issuer,
        tenant: TenantId::new(),
        ids: BTreeMap::new(),
    };
    let all = c.token(c.tenant, "all");
    let post = |path: &'static str, body: String| Route {
        method: "POST",
        path,
        needs: &[],
        body: Some(Box::leak(body.into_boxed_str())),
        owned: false,
    };
    let (s, site) = c
        .call(
            &post(
                "/api/v1/sites",
                json!({ "name": "Site A", "allowed_networks": ["192.168.50.0/24"] }).to_string(),
            ),
            Some(&all),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{site}");
    let site: Value = serde_json::from_str(&site).unwrap();
    c.ids
        .insert("site", site["id"].as_str().unwrap().to_owned());
    let (s, _) = c
        .call(
            &post(
                "/api/v1/agents",
                json!({ "agent_id": "agent-a", "site_id": site["id"] }).to_string(),
            ),
            Some(&all),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED);
    let (s, sc) = c
        .call(
            &post(
                "/api/v1/scanners",
                json!({ "site_id": site["id"], "agent_id": "agent-a", "name": "SDS200", "host": "192.168.50.10" }).to_string(),
            ),
            Some(&all),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{sc}");
    let sc: Value = serde_json::from_str(&sc).unwrap();
    c.ids
        .insert("scanner", sc["id"].as_str().unwrap().to_owned());
    let (s, g) = c
        .call(
            &post("/api/v1/groups", json!({ "name": "Group" }).to_string()),
            Some(&all),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{g}");
    let g: Value = serde_json::from_str(&g).unwrap();
    c.ids.insert("group", g["id"].as_str().unwrap().to_owned());
    let tx = TransmissionId::new();
    store
        .start_transmission(
            c.tenant,
            site["id"].as_str().unwrap().parse().unwrap(),
            &TransmissionStart {
                transmission_id: tx,
                scanner_id: sc["id"].as_str().unwrap().parse().unwrap(),
                site_id: None,
                started_at: platform_common::now_utc(),
                frequency_hz: None,
                tgid: None,
                unit_id: None,
                system: None,
                department: None,
                channel: None,
                modulation: None,
                signal_bars: None,
                rssi: None,
            },
        )
        .await
        .unwrap();
    store
        .set_transmission_audio(c.tenant, tx, "k", false, platform_common::now_utc())
        .await
        .unwrap();
    c.ids.insert("tx", tx.to_string());
    c.ids.insert("lease", uuid::Uuid::new_v4().to_string());
    c.ids.insert("session", uuid::Uuid::new_v4().to_string());
    c
}

fn denied(s: StatusCode) -> bool {
    s == StatusCode::UNAUTHORIZED || s == StatusCode::FORBIDDEN
}

#[tokio::test]
async fn table_covers_every_route() {
    let doc = serde_json::to_value(api_gateway::openapi::document()).unwrap();
    let mut documented = BTreeSet::new();
    for (path, item) in doc["paths"].as_object().unwrap() {
        for method in item.as_object().unwrap().keys() {
            let normalized = path
                .replace("{id}", "{x}")
                .replace("{scanner_id}", "{x}")
                .replace("{lease}", "{x}")
                .replace("{session}", "{x}");
            documented.insert((method.to_uppercase(), normalized));
        }
    }
    let norm = |p: &str| {
        [
            "{site}",
            "{scanner}",
            "{group}",
            "{tx}",
            "{lease}",
            "{session}",
        ]
        .iter()
        .fold(p.to_owned(), |acc, k| acc.replace(k, "{x}"))
    };
    let tabled: BTreeSet<(String, String)> = ROUTES
        .iter()
        .map(|r| (r.method.to_owned(), norm(r.path)))
        .chain(SPECIAL.iter().map(|(m, p)| ((*m).to_owned(), norm(p))))
        .collect();
    let missing: Vec<_> = documented.difference(&tabled).collect();
    assert!(
        missing.is_empty(),
        "routes without an authorization entry: {missing:?}"
    );
    // The OpenAPI document does not list itself.
    let stale: Vec<_> = tabled
        .difference(&documented)
        .filter(|(_, p)| p != "/api/v1/openapi.json")
        .collect();
    assert!(
        stale.is_empty(),
        "table entries for undocumented routes: {stale:?}"
    );
}

#[tokio::test]
async fn every_route_enforces_its_permissions() {
    let mut failures = Vec::new();
    for route in ROUTES {
        let label = format!("{} {}", route.method, route.path);
        // No token.
        let c = seeded().await;
        let (s, _) = c.call(route, None).await;
        if s != StatusCode::UNAUTHORIZED {
            failures.push(format!("{label}: no token gave {s}"));
        }
        // Every permission: authorization passes (other errors are fine).
        let c = seeded().await;
        let (s, body) = c.call(route, Some(&c.token(c.tenant, "all"))).await;
        if denied(s) {
            failures.push(format!("{label}: all permissions gave {s}: {body}"));
        }
        // Each required permission is necessary.
        for p in route.needs {
            let c = seeded().await;
            let (s, body) = c
                .call(route, Some(&c.token(c.tenant, &format!("all-but-{p:?}"))))
                .await;
            if s != StatusCode::FORBIDDEN {
                failures.push(format!("{label}: without {p:?} gave {s}: {body}"));
            }
        }
        // No permissions at all.
        if !route.needs.is_empty() {
            let c = seeded().await;
            let (s, _) = c.call(route, Some(&c.token(c.tenant, "none"))).await;
            if s != StatusCode::FORBIDDEN {
                failures.push(format!("{label}: no permissions gave {s}"));
            }
        }
        // Another tenant never reaches this tenant's resources.
        if route.owned {
            let c = seeded().await;
            let (s, body) = c.call(route, Some(&c.token(TenantId::new(), "all"))).await;
            if s.is_success() {
                failures.push(format!("{label}: another tenant gave {s}: {body}"));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[tokio::test]
async fn special_routes() {
    let c = seeded().await;
    // The live stream refuses a plain request without a token.
    let req = Request::get("/api/v1/events/stream")
        .body(Body::empty())
        .unwrap();
    let resp = c.app.clone().oneshot(req).await.unwrap();
    assert!(
        denied(resp.status()) || resp.status() == StatusCode::BAD_REQUEST,
        "{}",
        resp.status()
    );
    // The OpenAPI document is public by design (it holds no data).
    let req = Request::get("/api/v1/openapi.json")
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        c.app.clone().oneshot(req).await.unwrap().status(),
        StatusCode::OK
    );
}
