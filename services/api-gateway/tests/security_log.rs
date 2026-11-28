//! AU-2 security log: every authentication failure and rate-limit refusal
//! on the public and agent listeners is written as one structured event on
//! the `security` target, throttled per source, and never carries the
//! token, unverified claims, headers, the raw path or the query string.
//!
//! The log is captured through the production layer
//! (`security_log::testing::LogCapture`), installed as the thread's default
//! subscriber; `#[tokio::test]` runs on the current thread, so spawned
//! server tasks write into the same capture.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use api_gateway::config::LimitsConfig;
use api_gateway::security_log::testing::LogCapture;
use api_gateway::security_log::{
    self, Listener, Reason, SecurityEvent, SecurityLog, SecurityLogLimits,
};
use api_gateway::serve::{ClientAddr, PeerAgent, serve};
use api_gateway::{AppState, agent_app, public_app};
use axum::body::Body;
use axum::http::{HeaderValue, Method, Request, StatusCode};
use axum::routing::get;
use axum::{Extension, Router};
use http_body_util::BodyExt;
use platform_auth::testing::TestIssuer;
use platform_auth::{OidcConfig, OidcVerifier, Permission};
use platform_common::{AgentId, TenantId};
use platform_storage::MemoryStore;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;
use tracing::level_filters::LevelFilter;
use tracing_subscriber::EnvFilter;

const ISS: &str = "https://idp.example.test";
const AUD: &str = "radio-platform";

/// Strings that must never reach the security log.
const CANARY_COOKIE: &str = "canary-cookie-5d1e";
const CANARY_HEADER: &str = "canary-custom-header-77b0";
const CANARY_QUERY: &str = "canary-query-c0ffee";
const CANARY_SUB: &str = "canary-unverified-sub";
const CANARY_ISS: &str = "canary-iss.invalid";

struct Ctx {
    app: Router,
    state: Arc<AppState>,
    issuer: TestIssuer,
    tenant: TenantId,
}

fn roles() -> BTreeMap<String, Vec<Permission>> {
    BTreeMap::from([
        ("admin".into(), Permission::ALL.to_vec()),
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
        app: public_app(Arc::clone(&state)),
        state,
        issuer,
        tenant: TenantId::new(),
    }
}

fn ctx() -> Ctx {
    ctx_with(LimitsConfig::default())
}

impl Ctx {
    fn token(&self, sub: &str, ttl: i64) -> String {
        self.issuer.token(
            sub,
            ttl,
            json!({ "tenant_id": self.tenant.to_string(), "roles": ["admin"], "sites": ["*"] }),
        )
    }

    async fn send(&self, req: Request<Body>) -> StatusCode {
        let resp = self.app.clone().oneshot(req).await.unwrap();
        let s = resp.status();
        let _ = resp.into_body().collect().await;
        s
    }

    /// `GET uri` with an optional bearer token and client address.
    async fn get(&self, uri: &str, token: Option<&str>, ip: Option<&str>) -> StatusCode {
        let mut b = Request::get(uri);
        if let Some(t) = token {
            b = b.header("authorization", format!("Bearer {t}"));
        }
        self.send(with_ip(b.body(Body::empty()).unwrap(), ip)).await
    }
}

fn with_ip(mut req: Request<Body>, ip: Option<&str>) -> Request<Body> {
    if let Some(ip) = ip {
        let addr: IpAddr = ip.parse().unwrap();
        req.extensions_mut()
            .insert(ClientAddr(SocketAddr::new(addr, 5000)));
    }
    req
}

/// Every captured line, parsed; also checks the common schema.
fn events(cap: &LogCapture) -> Vec<Value> {
    let raw = cap.contents();
    let lines: Vec<&str> = raw.lines().collect();
    let evs = cap.events();
    assert_eq!(
        lines.len(),
        evs.len(),
        "every line is one JSON object: {raw}"
    );
    for e in &evs {
        assert_eq!(e["target"], "security", "{e}");
        assert_eq!(e["level"], "WARN", "{e}");
        assert_utc_rfc3339(e);
        assert!(e["message"].is_string(), "{e}");
        assert!(e.get("span").is_none() && e.get("spans").is_none(), "{e}");
        assert!(e.get("fields").is_none(), "flattened: {e}");
    }
    evs
}

/// The `timestamp` is RFC 3339 in UTC (AU-8).
fn assert_utc_rfc3339(e: &Value) {
    let ts = e["timestamp"]
        .as_str()
        .unwrap_or_else(|| panic!("timestamp: {e}"));
    let t = time::OffsetDateTime::parse(ts, &time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|err| panic!("timestamp `{ts}` is not RFC 3339: {err}"));
    assert!(t.offset().is_utc() && ts.ends_with('Z'), "not UTC: {ts}");
}

fn only_event(cap: &LogCapture) -> Value {
    let evs = events(cap);
    assert_eq!(evs.len(), 1, "exactly one event: {evs:?}");
    evs.into_iter().next().unwrap()
}

fn assert_absent(cap: &LogCapture, needles: &[&str]) {
    let out = cap.contents();
    for n in needles {
        assert!(!n.is_empty());
        assert!(
            !out.contains(n),
            "`{n}` leaked into the security log:\n{out}"
        );
    }
}

/// The token, and every `.`-separated segment of it.
fn token_parts(t: &str) -> Vec<&str> {
    let mut v = vec![t];
    v.extend(t.split('.').filter(|s| s.len() >= 8));
    v
}

fn assert_auth_failure(e: &Value, reason: &str, status: u16) {
    assert_eq!(e["event"], "auth_failure", "{e}");
    assert_eq!(e["reason"], reason, "{e}");
    assert_eq!(e["status"], status, "{e}");
    assert_eq!(e["message"], "authentication failed", "{e}");
}

fn assert_no_identity(e: &Value) {
    for f in ["subject", "tenant", "agent"] {
        assert!(e.get(f).is_none(), "unverified request carries `{f}`: {e}");
    }
}

// ---------------------------------------------------------------- 401 reasons

#[tokio::test]
async fn missing_token_is_logged_with_route_template_and_no_raw_path() {
    let c = ctx();
    let cap = LogCapture::new();
    let _g = cap.install();
    let id = uuid::Uuid::new_v4().to_string();
    let uri = format!("/api/v1/scanners/{id}/status?cursor={CANARY_QUERY}");
    let req = Request::get(uri)
        .header("cookie", format!("session={CANARY_COOKIE}"))
        .header("x-canary", CANARY_HEADER)
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        c.send(with_ip(req, Some("198.51.100.20"))).await,
        StatusCode::UNAUTHORIZED
    );
    let e = only_event(&cap);
    assert_auth_failure(&e, "missing", 401);
    assert_eq!(e["route"], "/api/v1/scanners/{id}/status");
    assert_eq!(e["method"], "GET");
    assert_eq!(e["listener"], "public");
    assert_eq!(e["src_ip"], "198.51.100.20");
    assert!(e.get("claim").is_none());
    assert_no_identity(&e);
    assert_absent(
        &cap,
        &[&id, CANARY_QUERY, CANARY_COOKIE, CANARY_HEADER, "cursor"],
    );
}

#[tokio::test]
async fn non_bearer_schemes_and_unreadable_headers_count_as_missing() {
    let c = ctx();
    let cap = LogCapture::new();
    let _g = cap.install();
    let basic = "Basic Y2FuYXJ5LWJhc2ljOnB3";
    let lower = "bearer canary-lowercase-scheme-token";
    for v in [basic, lower] {
        let req = Request::get("/api/v1/scanners")
            .header("authorization", v)
            .body(Body::empty())
            .unwrap();
        assert_eq!(c.send(req).await, StatusCode::UNAUTHORIZED, "{v}");
    }
    // Not valid UTF-8 (obs-text): unreadable, so treated as missing.
    let mut req = Request::get("/api/v1/scanners")
        .body(Body::empty())
        .unwrap();
    req.headers_mut().insert(
        "authorization",
        HeaderValue::from_bytes(b"Bearer canary-obs-text-\xff-token").unwrap(),
    );
    assert_eq!(c.send(req).await, StatusCode::UNAUTHORIZED);
    // A query-string token is not a credential and is never logged.
    let qs = "canary-query-string-access-token";
    assert_eq!(
        c.get(&format!("/api/v1/scanners?access_token={qs}"), None, None)
            .await,
        StatusCode::UNAUTHORIZED
    );
    let evs = events(&cap);
    assert_eq!(evs.len(), 4, "{evs:?}");
    for e in &evs {
        assert_auth_failure(e, "missing", 401);
        assert_eq!(e["route"], "/api/v1/scanners");
        assert!(e.get("src_ip").is_none(), "no client address known: {e}");
        assert_no_identity(e);
    }
    assert_absent(
        &cap,
        &[
            "Y2FuYXJ5LWJhc2ljOnB3",
            "canary-lowercase-scheme-token",
            "canary-obs-text",
            qs,
            "access_token",
        ],
    );
}

#[tokio::test]
async fn malformed_tokens_are_logged_without_their_content() {
    let c = ctx();
    let cap = LogCapture::new();
    let _g = cap.install();
    let oversized = format!("canary-oversized-{}", "A".repeat(17 * 1024));
    let tokens = [
        "radio-canary-malformed-7f3a".to_owned(),
        // JSON-breaking characters must not break the line format.
        "canary-json\"},{\"injected\":\"x".to_owned(),
        // Empty after the scheme.
        String::new(),
        // alg=none is not a parseable algorithm.
        "eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0.eyJpc3MiOiJodHRwczovL2lkcC5leGFtcGxlLnRlc3QiLCJhdWQiOiJyYWRpby1wbGF0Zm9ybSIsInN1YiI6ImNhbmFyeS1ocy1zdWIiLCJleHAiOjQxMDI0NDQ4MDB9.".to_owned(),
        oversized.clone(),
    ];
    for t in &tokens {
        let req = Request::get("/api/v1/sites")
            .header("authorization", format!("Bearer {t}"))
            .body(Body::empty())
            .unwrap();
        assert_eq!(c.send(req).await, StatusCode::UNAUTHORIZED, "{t:.40}");
    }
    let evs = events(&cap);
    assert_eq!(evs.len(), tokens.len(), "{evs:?}");
    for e in &evs {
        assert_auth_failure(e, "malformed", 401);
        assert_eq!(e["route"], "/api/v1/sites");
        assert_no_identity(e);
        assert!(e.get("injected").is_none(), "{e}");
    }
    assert_absent(
        &cap,
        &[
            "radio-canary-malformed-7f3a",
            "canary-json",
            "canary-oversized",
            "AAAAAAAA",
            "eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0",
            "canary-hs-sub",
        ],
    );
}

#[tokio::test]
async fn invalid_tokens_never_log_unverified_claims() {
    let c = ctx();
    let cap = LogCapture::new();
    let _g = cap.install();
    let extra = json!({ "tenant_id": c.tenant.to_string(), "roles": ["admin"] });
    // Unknown signing key, foreign issuer.
    let impostor = TestIssuer::new(&format!("https://{CANARY_ISS}"), AUD).token(
        CANARY_SUB,
        600,
        extra.clone(),
    );
    // Our key, wrong issuer / wrong audience.
    let wrong_iss = c.issuer.sign(&json!({
        "iss": format!("https://{CANARY_ISS}"), "aud": AUD, "sub": CANARY_SUB,
        "exp": 4_102_444_800_i64, "tenant_id": c.tenant.to_string(),
    }));
    let wrong_aud = c.issuer.sign(&json!({
        "iss": ISS, "aud": "canary-aud-mismatch", "sub": CANARY_SUB,
        "exp": 4_102_444_800_i64, "tenant_id": c.tenant.to_string(),
    }));
    // HS256 (not an allowed algorithm), forged signature.
    let hs256 = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJpc3MiOiJodHRwczovL2lkcC5leGFtcGxlLnRlc3QiLCJhdWQiOiJyYWRpby1wbGF0Zm9ybSIsInN1YiI6ImNhbmFyeS1ocy1zdWIiLCJleHAiOjQxMDI0NDQ4MDB9.Y2FuYXJ5LXNpZw".to_owned();
    // Valid token with a tampered signature.
    let good = c.token("canary-tampered-sub", 600);
    let tampered = format!("{}x", &good[..good.len() - 1]);
    let all = [&impostor, &wrong_iss, &wrong_aud, &hs256, &tampered];
    for t in all {
        assert_eq!(
            c.get("/api/v1/scanners", Some(t), Some("198.51.100.21"))
                .await,
            StatusCode::UNAUTHORIZED
        );
    }
    let evs = events(&cap);
    assert_eq!(evs.len(), all.len(), "{evs:?}");
    for e in &evs {
        assert_auth_failure(e, "invalid", 401);
        assert_eq!(e["src_ip"], "198.51.100.21");
        assert_no_identity(e);
    }
    let mut needles = vec![
        CANARY_SUB,
        CANARY_ISS,
        "canary-aud-mismatch",
        "canary-hs-sub",
        "canary-tampered-sub",
    ];
    let tenant = c.tenant.to_string();
    needles.push(&tenant);
    for t in all {
        needles.extend(token_parts(t));
    }
    assert_absent(&cap, &needles);
}

#[tokio::test]
async fn expired_and_claim_failures_are_logged_by_reason() {
    let c = ctx();
    let cap = LogCapture::new();
    let _g = cap.install();
    let expired = c.token("canary-expired-sub", -120);
    let no_tenant = c
        .issuer
        .token("canary-claim-sub", 600, json!({ "roles": ["admin"] }));
    let bad_sites = c.issuer.token(
        "canary-sites-sub",
        600,
        json!({ "tenant_id": c.tenant.to_string(), "sites": [1, 2] }),
    );
    for t in [&expired, &no_tenant, &bad_sites] {
        assert_eq!(
            c.get("/api/v1/scanners", Some(t), None).await,
            StatusCode::UNAUTHORIZED
        );
    }
    let evs = events(&cap);
    assert_eq!(evs.len(), 3, "{evs:?}");
    assert_auth_failure(&evs[0], "expired", 401);
    assert!(evs[0].get("claim").is_none());
    assert_auth_failure(&evs[1], "claim", 401);
    assert_eq!(evs[1]["claim"], "tenant_id");
    assert_auth_failure(&evs[2], "claim", 401);
    assert_eq!(evs[2]["claim"], "sites");
    for e in &evs {
        assert_no_identity(e);
    }
    let tenant = c.tenant.to_string();
    let mut needles = vec![
        "canary-expired-sub",
        "canary-claim-sub",
        "canary-sites-sub",
        &tenant,
    ];
    for t in [&expired, &no_tenant, &bad_sites] {
        needles.extend(token_parts(t));
    }
    assert_absent(&cap, &needles);
}

#[tokio::test]
async fn expiry_at_authorization_carries_the_verified_identity() {
    let c = ctx();
    let cap = LogCapture::new();
    let _g = cap.install();
    // Past `exp` but inside the 30 s verification leeway: `verify` accepts,
    // `authorize` refuses with 401.
    let t = c.token("vera-verified", -10);
    assert_eq!(
        c.get("/api/v1/scanners", Some(&t), Some("198.51.100.22"))
            .await,
        StatusCode::UNAUTHORIZED
    );
    let e = only_event(&cap);
    assert_auth_failure(&e, "expired", 401);
    assert_eq!(e["subject"], "vera-verified");
    assert_eq!(e["tenant"], c.tenant.to_string());
    assert_eq!(e["route"], "/api/v1/scanners");
    assert_eq!(e["method"], "GET");
    assert_eq!(e["src_ip"], "198.51.100.22");
    assert!(e.get("agent").is_none());
    assert_absent(&cap, &token_parts(&t));
}

#[tokio::test]
async fn identity_provider_outage_is_not_a_security_event() {
    // A JWKS endpoint that answers once (empty key set) and then fails.
    let hits = Arc::new(AtomicUsize::new(0));
    let h = Arc::clone(&hits);
    let jwks = Router::new().route(
        "/jwks",
        get(move || {
            let h = Arc::clone(&h);
            async move {
                if h.fetch_add(1, Ordering::SeqCst) == 0 {
                    (StatusCode::OK, r#"{"keys":[]}"#)
                } else {
                    (StatusCode::SERVICE_UNAVAILABLE, "down")
                }
            }
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let cancel = CancellationToken::new();
    let _stop = cancel.clone().drop_guard();
    tokio::spawn(serve(listener, None, jwks, 4, cancel));

    let issuer = TestIssuer::new("http://idp.canary.test", AUD);
    let mut oidc = OidcConfig::new("http://idp.canary.test", AUD);
    oidc.jwks_uri = Some(format!("http://{addr}/jwks"));
    oidc.allow_insecure_http = true;
    oidc.jwks_min_refresh_secs = 0;
    let verifier = OidcVerifier::discover(oidc).await.unwrap();
    let state = AppState::new(
        Arc::new(MemoryStore::new()),
        Arc::new(verifier),
        LimitsConfig::default(),
    );
    let app = public_app(Arc::clone(&state));

    let cap = LogCapture::new();
    let _g = cap.install();
    let t = issuer.token("canary-provider-sub", 600, json!({}));
    let req = Request::get("/api/v1/sites")
        .header("authorization", format!("Bearer {t}"))
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(hits.load(Ordering::SeqCst) >= 2, "JWKS refresh attempted");
    assert_eq!(
        cap.contents(),
        "",
        "Provider errors are not client failures (and other targets are filtered)"
    );
    let stats = state.security.stats();
    for r in Reason::ALL {
        assert_eq!(stats.events(r), 0, "{r:?}");
    }
}

// ---------------------------------------------------------------- WebSocket

#[tokio::test]
async fn stream_subprotocol_token_is_rejected_and_never_logged() {
    let c = ctx();
    let cap = LogCapture::new();
    let _g = cap.install();
    let impostor = TestIssuer::new(ISS, AUD).token(
        CANARY_SUB,
        600,
        json!({ "tenant_id": c.tenant.to_string(), "roles": ["admin"] }),
    );
    let malformed = "canary-subprotocol-malformed-91c2";
    for t in [impostor.as_str(), malformed] {
        let req = Request::get("/api/v1/events/stream?scanner_id=canary-stream-query")
            .header("connection", "upgrade")
            .header("upgrade", "websocket")
            .header("sec-websocket-version", "13")
            .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
            .header(
                "sec-websocket-protocol",
                format!("radio-platform.v1, radio-platform.bearer.{t}"),
            )
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            c.send(with_ip(req, Some("198.51.100.23"))).await,
            StatusCode::UNAUTHORIZED
        );
    }
    let evs = events(&cap);
    assert_eq!(evs.len(), 2, "{evs:?}");
    assert_auth_failure(&evs[0], "invalid", 401);
    assert_auth_failure(&evs[1], "malformed", 401);
    for e in &evs {
        assert_eq!(e["route"], "/api/v1/events/stream");
        assert_eq!(e["listener"], "public");
        assert_eq!(e["src_ip"], "198.51.100.23");
        assert_no_identity(e);
    }
    let mut needles = token_parts(&impostor);
    needles.extend([
        malformed,
        CANARY_SUB,
        "radio-platform.bearer",
        "canary-stream-query",
    ]);
    assert_absent(&cap, &needles);
}

// ---------------------------------------------------------------- 429 paths

#[tokio::test]
async fn per_ip_refusal_is_logged_before_authentication() {
    let c = ctx_with(LimitsConfig {
        requests_per_minute_per_ip: 2,
        ..LimitsConfig::default()
    });
    let cap = LogCapture::new();
    let _g = cap.install();
    let tok = c.token("ivan", 600);
    for _ in 0..2 {
        assert_eq!(
            c.get("/api/v1/sites", Some(&tok), Some("198.51.100.30"))
                .await,
            StatusCode::OK
        );
    }
    let id = uuid::Uuid::new_v4().to_string();
    let uri = format!("/api/v1/scanners/{id}/status?cursor={CANARY_QUERY}");
    assert_eq!(
        c.get(&uri, Some(&tok), Some("198.51.100.30")).await,
        StatusCode::TOO_MANY_REQUESTS
    );
    let e = only_event(&cap);
    assert_eq!(e["event"], "rate_limited");
    assert_eq!(e["reason"], "rate_limited_ip");
    assert_eq!(e["status"], 429);
    assert_eq!(e["message"], "request rate limited");
    assert_eq!(e["src_ip"], "198.51.100.30");
    assert_eq!(e["route"], "/api/v1/scanners/{id}/status");
    assert_eq!(e["listener"], "public");
    // Refused before authentication: no identity, even with a valid token.
    assert_no_identity(&e);
    let mut needles = token_parts(&tok);
    needles.extend([id.as_str(), CANARY_QUERY, "ivan"]);
    assert_absent(&cap, &needles);
}

#[tokio::test]
async fn per_ip_refusal_on_unmatched_paths_and_extension_methods() {
    let c = ctx_with(LimitsConfig {
        requests_per_minute_per_ip: 1,
        ..LimitsConfig::default()
    });
    let cap = LogCapture::new();
    let _g = cap.install();
    assert_eq!(
        c.get("/api/v1/sites", None, Some("198.51.100.31")).await,
        StatusCode::UNAUTHORIZED
    );
    cap.clear();
    let raw_path = "/canary-unmatched-path/0451";
    assert_eq!(
        c.get(raw_path, None, Some("198.51.100.31")).await,
        StatusCode::TOO_MANY_REQUESTS
    );
    let method = Method::from_bytes(b"CANARYMETHOD").unwrap();
    let req = Request::builder()
        .method(method)
        .uri("/api/v1/sites")
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        c.send(with_ip(req, Some("198.51.100.31"))).await,
        StatusCode::TOO_MANY_REQUESTS
    );
    let evs = events(&cap);
    assert_eq!(evs.len(), 2, "{evs:?}");
    assert_eq!(evs[0]["reason"], "rate_limited_ip");
    assert_eq!(evs[0]["route"], "unmatched");
    assert_eq!(evs[0]["method"], "GET");
    assert_eq!(evs[1]["reason"], "rate_limited_ip");
    assert_eq!(evs[1]["route"], "/api/v1/sites");
    assert_eq!(evs[1]["method"], "OTHER");
    assert_absent(&cap, &["canary-unmatched-path", "CANARYMETHOD"]);
}

#[tokio::test]
async fn per_principal_refusal_carries_the_verified_identity() {
    let c = ctx_with(LimitsConfig {
        requests_per_minute: 3,
        ..LimitsConfig::default()
    });
    let cap = LogCapture::new();
    let _g = cap.install();
    let tok = c.token("pat-principal", 600);
    for _ in 0..3 {
        assert_eq!(c.get("/api/v1/me", Some(&tok), None).await, StatusCode::OK);
    }
    assert_eq!(cap.contents(), "", "successful requests log nothing");
    assert_eq!(
        c.get("/api/v1/me", Some(&tok), None).await,
        StatusCode::TOO_MANY_REQUESTS
    );
    let e = only_event(&cap);
    assert_eq!(e["event"], "rate_limited");
    assert_eq!(e["reason"], "rate_limited_principal");
    assert_eq!(e["status"], 429);
    assert_eq!(e["subject"], "pat-principal");
    assert_eq!(e["tenant"], c.tenant.to_string());
    assert_eq!(e["route"], "/api/v1/me");
    assert!(e.get("src_ip").is_none());
    assert_absent(&cap, &token_parts(&tok));
}

// ---------------------------------------------------------------- client IP

#[tokio::test]
async fn source_ip_honours_trusted_proxies_only() {
    let c = ctx_with(LimitsConfig {
        trusted_proxies: vec!["10.0.0.0/8".parse().unwrap()],
        ..LimitsConfig::default()
    });
    let cap = LogCapture::new();
    let _g = cap.install();
    let xff = "canary-xff-garbage, 203.0.113.9";
    for (peer, header) in [
        ("10.1.2.3", xff),
        ("198.51.100.40", "203.0.113.9"),
        ("10.1.2.3", "canary-xff-only-garbage"),
    ] {
        let req = Request::get("/api/v1/sites")
            .header("x-forwarded-for", header)
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            c.send(with_ip(req, Some(peer))).await,
            StatusCode::UNAUTHORIZED
        );
    }
    let evs = events(&cap);
    assert_eq!(evs.len(), 3, "{evs:?}");
    assert_eq!(
        evs[0]["src_ip"], "203.0.113.9",
        "trusted proxy: client from XFF"
    );
    assert_eq!(
        evs[1]["src_ip"], "198.51.100.40",
        "untrusted peer: XFF ignored"
    );
    assert_eq!(evs[2]["src_ip"], "10.1.2.3", "unparseable XFF: the proxy");
    assert_absent(&cap, &["canary-xff", "x-forwarded-for"]);
}

// ---------------------------------------------------------------- throttle

#[tokio::test]
async fn throttle_caps_one_source_and_summarises_the_rest() {
    let c = ctx();
    let cap = LogCapture::new();
    let _g = cap.install();
    for _ in 0..15 {
        assert_eq!(
            c.get(
                "/api/v1/sites",
                Some("canary-flood-token"),
                Some("198.51.100.50")
            )
            .await,
            StatusCode::UNAUTHORIZED
        );
    }
    for _ in 0..3 {
        c.get("/api/v1/sites", None, Some("198.51.100.51")).await;
    }
    let evs = events(&cap);
    let from = |ip: &str| evs.iter().filter(|e| e["src_ip"] == ip).count();
    assert_eq!(from("198.51.100.50"), 10, "per-source cap");
    assert_eq!(from("198.51.100.51"), 3, "other sources unaffected");
    assert_eq!(evs.len(), 13);

    let stats = c.state.security.stats();
    assert_eq!(
        stats.events(Reason::Malformed),
        15,
        "metric counts every event"
    );
    assert_eq!(stats.events(Reason::Missing), 3);
    assert_eq!(stats.suppressed, 5);
    assert_eq!(stats.tracked_sources, 2);

    // Before the window ends nothing is due.
    cap.clear();
    c.state.security.flush();
    assert_eq!(cap.contents(), "");

    c.state
        .security
        .flush_at(Instant::now() + Duration::from_secs(61));
    let e = only_event(&cap);
    assert_eq!(e["event"], "suppressed");
    assert_eq!(e["scope"], "source");
    assert_eq!(e["source"], "198.51.100.50");
    assert_eq!(e["suppressed"], 5);
    assert_eq!(e["window_secs"], 60);
    assert_eq!(e["message"], "security events suppressed");
    for f in ["status", "route", "method", "listener", "reason", "src_ip"] {
        assert!(e.get(f).is_none(), "summary carries `{f}`: {e}");
    }
    assert_eq!(c.state.security.tracked_sources(), 0, "table drained");
    assert_absent(&cap, &["canary-flood-token"]);
}

#[tokio::test]
async fn ipv6_sources_are_throttled_per_64() {
    let c = ctx();
    let cap = LogCapture::new();
    let _g = cap.install();
    for i in 1..=12 {
        let ip = format!("2001:db8:1:2::{i:x}");
        c.get("/api/v1/sites", None, Some(&ip)).await;
    }
    // IPv4-mapped IPv6 is the IPv4 source.
    c.get("/api/v1/sites", None, Some("::ffff:198.51.100.60"))
        .await;
    let evs = events(&cap);
    assert_eq!(evs.len(), 11, "{evs:?}");
    assert_eq!(
        evs[0]["src_ip"], "2001:db8:1:2::1",
        "events keep the full address"
    );
    assert_eq!(c.state.security.tracked_sources(), 2);
    cap.clear();
    c.state
        .security
        .flush_at(Instant::now() + Duration::from_secs(61));
    let e = only_event(&cap);
    assert_eq!(e["source"], "2001:db8:1:2::/64");
    assert_eq!(e["suppressed"], 2);
}

#[tokio::test]
async fn global_budget_caps_many_sources() {
    let c = ctx();
    let cap = LogCapture::new();
    let _g = cap.install();
    for i in 0..320u32 {
        let ip = format!("10.{}.{}.{}", i >> 16, (i >> 8) & 0xff, i & 0xff);
        assert_eq!(
            c.get("/api/v1/sites", None, Some(&ip)).await,
            StatusCode::UNAUTHORIZED
        );
    }
    assert_eq!(events(&cap).len(), 300, "replica-wide budget");
    let stats = c.state.security.stats();
    assert_eq!(stats.events(Reason::Missing), 320);
    assert_eq!(stats.suppressed, 20);

    cap.clear();
    c.state
        .security
        .flush_at(Instant::now() + Duration::from_secs(61));
    let total: u64 = events(&cap)
        .iter()
        .map(|e| {
            assert_eq!(e["event"], "suppressed");
            e["suppressed"].as_u64().unwrap()
        })
        .sum();
    assert_eq!(total, 20, "no suppressed count lost");
}

#[tokio::test]
async fn tracked_source_table_is_bounded_and_counts_survive() {
    let c = ctx();
    let cap = LogCapture::new();
    let _g = cap.install();
    let n = 5000u32;
    for i in 0..n {
        let ip = format!("10.{}.{}.{}", i >> 16, (i >> 8) & 0xff, i & 0xff);
        c.get("/api/v1/sites", None, Some(&ip)).await;
    }
    let stats = c.state.security.stats();
    assert!(stats.tracked_sources <= 4096, "{}", stats.tracked_sources);
    assert!(stats.untracked > 0, "the table filled");
    assert_eq!(stats.events(Reason::Missing), u64::from(n));
    assert_eq!(stats.suppressed, u64::from(n) - 300);
    assert_eq!(events(&cap).len(), 300);

    cap.clear();
    c.state
        .security
        .flush_at(Instant::now() + Duration::from_secs(61));
    let evs = events(&cap);
    let sources = evs.iter().filter(|e| e["scope"] == "source").count();
    let globals = evs.iter().filter(|e| e["scope"] == "global").count();
    assert!(sources <= 128, "summary cap: {sources}");
    assert_eq!(globals, 1);
    let total: u64 = evs.iter().map(|e| e["suppressed"].as_u64().unwrap()).sum();
    assert_eq!(total, u64::from(n) - 300, "no suppressed count lost");
    assert_eq!(c.state.security.tracked_sources(), 0);
}

#[test]
fn small_limits_bound_the_table_directly() {
    let cap = LogCapture::new();
    let _g = cap.install();
    let log = SecurityLog::with_limits(SecurityLogLimits {
        shards: 2,
        shard_capacity: 2,
        global: 1_000,
        ..SecurityLogLimits::default()
    });
    let now = Instant::now();
    for i in 0..50u8 {
        let ip: IpAddr = format!("192.0.2.{i}").parse().unwrap();
        let ev = SecurityEvent {
            reason: Reason::Invalid,
            claim: None,
            status: 401,
            ip: Some(ip),
            method: &Method::POST,
            route: "/api/v1/sites",
            listener: Listener::Public,
            subject: None,
            tenant: None,
            agent: None,
        };
        log.record_at(&ev, now);
        log.record_at(&ev, now);
        assert!(log.tracked_sources() <= 4);
    }
    // Untracked sources fall back to the global budget, so all lines land.
    assert_eq!(events(&cap).len(), 100);
    log.flush_at(now + Duration::from_secs(61));
    assert_eq!(log.tracked_sources(), 0);
}

fn unauthenticated(ip: IpAddr) -> SecurityEvent<'static> {
    SecurityEvent {
        reason: Reason::Missing,
        claim: None,
        status: 401,
        ip: Some(ip),
        method: &Method::GET,
        route: "/api/v1/sites",
        listener: Listener::Public,
        subject: None,
        tenant: None,
        agent: None,
    }
}

#[test]
fn verified_identity_events_survive_an_unauthenticated_flood() {
    let cap = LogCapture::new();
    let _g = cap.install();
    let log = SecurityLog::new();
    let now = Instant::now();
    // 5000 sources: the general budget (300) is spent, the table is full.
    for i in 0..5_000u32 {
        let ip = IpAddr::from([10, 1, (i >> 8) as u8, i as u8]);
        log.record_at(&unauthenticated(ip), now);
    }
    assert_eq!(events(&cap).len(), 300);
    assert!(log.stats().untracked > 0);
    // Another unauthenticated source is suppressed ...
    log.record_at(&unauthenticated("192.0.2.1".parse().unwrap()), now);
    assert_eq!(events(&cap).len(), 300);
    // ... but verified-identity events from new sources are written.
    let tenant = TenantId::new();
    log.record_at(
        &SecurityEvent {
            reason: Reason::RateLimitedPrincipal,
            status: 429,
            subject: Some("verified-sub"),
            tenant: Some(tenant),
            ..unauthenticated("192.0.2.2".parse().unwrap())
        },
        now,
    );
    let agent = AgentId::new("agent-flood").unwrap();
    log.record_at(
        &SecurityEvent {
            reason: Reason::AgentUnregistered,
            status: 403,
            listener: Listener::Agent,
            route: "/agent/v1",
            agent: Some(&agent),
            ..unauthenticated("192.0.2.3".parse().unwrap())
        },
        now,
    );
    let evs = events(&cap);
    assert_eq!(evs.len(), 302);
    assert_eq!(evs[300]["reason"], "rate_limited_principal");
    assert_eq!(evs[300]["subject"], "verified-sub");
    assert_eq!(evs[301]["reason"], "agent_unregistered");
    assert_eq!(evs[301]["agent"], "agent-flood");
    // The verified budget is itself bounded (60 per window) ...
    for i in 0..100u8 {
        log.record_at(
            &SecurityEvent {
                reason: Reason::Expired,
                subject: Some("verified-sub"),
                ..unauthenticated(IpAddr::from([203, 0, 113, i]))
            },
            now,
        );
    }
    assert_eq!(events(&cap).len(), 300 + 60);
    // ... and the per-source limit still applies to verified events.
    let one = SecurityLog::new();
    cap.clear();
    for _ in 0..15 {
        one.record_at(
            &SecurityEvent {
                reason: Reason::RateLimitedPrincipal,
                status: 429,
                subject: Some("verified-sub"),
                ..unauthenticated("198.51.100.9".parse().unwrap())
            },
            now,
        );
    }
    assert_eq!(events(&cap).len(), 10);
    assert_eq!(one.stats().suppressed, 5);
}

// ------------------------------------------------- process-wide subscriber

/// Installs `security_log::subscriber` with `env` on this thread, writing the
/// general log to `general` and the security log to `security`.
fn install_subscriber(
    env: &str,
    general: &LogCapture,
    security: &LogCapture,
) -> tracing::subscriber::DefaultGuard {
    let sub = security_log::subscriber(EnvFilter::new(env), general.clone(), security.clone());
    let guard = tracing::subscriber::set_default(sub);
    tracing::callsite::rebuild_interest_cache();
    guard
}

fn record_one(log: &SecurityLog) {
    log.record(&unauthenticated("192.0.2.77".parse().unwrap()));
}

#[test]
fn rust_log_off_or_error_cannot_silence_the_security_log() {
    for env in ["off", "error", "info,security=off", "warn,security=trace"] {
        let (general, security) = (LogCapture::new(), LogCapture::new());
        let _g = install_subscriber(env, &general, &security);
        let log = SecurityLog::new();
        record_one(&log);
        // An explicit target: this test crate is `security_log`, which an
        // EnvFilter directive `security=off` would match by prefix.
        tracing::error!(target: "gateway", "general error line");
        let ev = only_event(&security);
        assert_eq!(ev["reason"], "missing", "RUST_LOG={env}");
        let out = general.contents();
        assert!(
            !out.contains("authentication failed"),
            "RUST_LOG={env}: {out}"
        );
        if env == "off" {
            assert!(out.is_empty(), "{out}");
        } else {
            assert!(out.contains("general error line"), "RUST_LOG={env}: {out}");
        }
    }
}

#[test]
fn debug_general_log_never_carries_security_events_or_span_context() {
    let (general, security) = (LogCapture::new(), LogCapture::new());
    let _g = install_subscriber("debug", &general, &security);
    let log = SecurityLog::new();
    let span = tracing::info_span!("request", uri = "/api/v1/sites?q=canary-span-uri");
    let entered = span.enter();
    record_one(&log);
    tracing::debug!("general debug line");
    tracing::warn!(target: "security_x", "lookalike target");
    drop(entered);
    // The security writer: one flattened line, no span fields, no canary.
    let ev = only_event(&security);
    assert_eq!(ev["reason"], "missing");
    assert_absent(&security, &["canary-span-uri", "request"]);
    // The general writer: the debug line (with its span), never a
    // `security` target line; the lookalike target is general output.
    let lines: Vec<Value> = general
        .contents()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert!(
        lines
            .iter()
            .any(|l| l["fields"]["message"] == "general debug line")
    );
    assert!(lines.iter().any(|l| l["target"] == "security_x"));
    assert!(
        lines.iter().all(|l| l["target"] != security_log::TARGET),
        "{lines:?}"
    );
    for l in &lines {
        assert_utc_rfc3339(l);
    }
}

#[test]
fn subscriber_keeps_the_env_filter_level_hint() {
    let hint = |env: &str| {
        let sub =
            security_log::subscriber(EnvFilter::new(env), LogCapture::new(), LogCapture::new());
        tracing::Subscriber::max_level_hint(&sub)
    };
    // The production default: disabled debug!/trace! keep their fast path.
    assert_eq!(hint("info"), Some(LevelFilter::INFO));
    assert_eq!(hint("debug"), Some(LevelFilter::DEBUG));
    // The security layer still needs WARN when the general log is off.
    assert_eq!(hint("off"), Some(LevelFilter::WARN));
    assert_eq!(hint("error"), Some(LevelFilter::WARN));
}

// ---------------------------------------------------------------- metrics

#[tokio::test]
async fn metrics_count_every_rejection_including_suppressed() {
    let c = ctx_with(LimitsConfig {
        requests_per_minute_per_ip: 12,
        ..LimitsConfig::default()
    });
    let cap = LogCapture::new();
    let _g = cap.install();
    for _ in 0..12 {
        c.get("/api/v1/sites", None, Some("198.51.100.70")).await;
    }
    assert_eq!(
        c.get("/api/v1/sites", None, Some("198.51.100.70")).await,
        StatusCode::TOO_MANY_REQUESTS
    );
    let req = Request::get("/metrics").body(Body::empty()).unwrap();
    let resp = api_gateway::metrics::router(Arc::clone(&c.state))
        .oneshot(req)
        .await
        .unwrap();
    let text = String::from_utf8(
        resp.into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
    .unwrap();
    for line in [
        "radio_security_events_total{reason=\"missing\"} 12",
        "radio_security_events_total{reason=\"rate_limited_ip\"} 1",
        "radio_security_events_total{reason=\"invalid\"} 0",
        "radio_security_events_total{reason=\"agent_unregistered\"} 0",
        // 13 events from one source: 10 written, 3 suppressed.
        "radio_security_log_suppressed_total 3",
        "radio_security_log_tracked_sources 1",
    ] {
        assert!(
            text.lines().any(|l| l == line),
            "missing `{line}` in:\n{text}"
        );
    }
    assert!(text.contains("# TYPE radio_security_events_total counter"));
    assert!(text.contains("# TYPE radio_security_log_tracked_sources gauge"));
    assert_eq!(
        text.matches("radio_security_events_total{").count(),
        Reason::ALL.len()
    );
    assert_eq!(events(&cap).len(), 10);
}

// ---------------------------------------------------------------- success

#[tokio::test]
async fn successful_requests_log_nothing() {
    let c = ctx();
    let cap = LogCapture::new();
    let _g = cap.install();
    let tok = c.token("sam-success", 600);
    let id = uuid::Uuid::new_v4();
    for uri in [
        "/api/v1/me".to_owned(),
        "/api/v1/sites".to_owned(),
        "/api/v1/scanners".to_owned(),
        format!("/api/v1/scanners/{id}"),
    ] {
        let s = c.get(&uri, Some(&tok), Some("198.51.100.80")).await;
        assert!(s.is_success() || s == StatusCode::NOT_FOUND, "{uri}: {s}");
    }
    for uri in ["/healthz", "/readyz", "/api/v1/openapi.json"] {
        assert_eq!(
            c.get(uri, None, Some("198.51.100.80")).await,
            StatusCode::OK
        );
    }
    assert_eq!(cap.contents(), "");
    let stats = c.state.security.stats();
    for r in Reason::ALL {
        assert_eq!(stats.events(r), 0, "{r:?}");
    }
    assert_eq!(stats.tracked_sources, 0, "no slot for successful sources");
}

// ---------------------------------------------------------------- agent listener

/// Serves `app` on loopback without TLS (so `PeerAgent(None)`, unless the
/// router overrides it) and returns the address.
async fn spawn(app: Router, cancel: &CancellationToken) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(serve(listener, None, app, 8, cancel.clone()));
    addr
}

/// Sends a WebSocket upgrade to the agent path; returns the status code.
async fn agent_upgrade(addr: SocketAddr) -> u16 {
    let mut s = TcpStream::connect(addr).await.unwrap();
    let req = format!(
        "GET {} HTTP/1.1\r\nHost: localhost\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\
         Sec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
         Authorization: Bearer canary-agent-bearer\r\nCookie: {CANARY_COOKIE}\r\n\r\n",
        platform_common::wire::AGENT_PATH
    );
    s.write_all(req.as_bytes()).await.unwrap();
    let mut buf = vec![0u8; 2048];
    let n = tokio::time::timeout(Duration::from_secs(5), s.read(&mut buf))
        .await
        .unwrap()
        .unwrap();
    let head = String::from_utf8_lossy(&buf[..n]);
    head.split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or_else(|| panic!("no status line: {head}"))
}

#[tokio::test]
async fn agent_without_certificate_identity_is_logged() {
    let c = ctx();
    let cancel = CancellationToken::new();
    let _stop = cancel.clone().drop_guard();
    let addr = spawn(agent_app(Arc::clone(&c.state)), &cancel).await;
    let cap = LogCapture::new();
    let _g = cap.install();
    assert_eq!(agent_upgrade(addr).await, 401);
    let e = only_event(&cap);
    assert_auth_failure(&e, "agent_identity", 401);
    assert_eq!(e["listener"], "agent");
    assert_eq!(e["route"], "/agent/v1");
    assert_eq!(e["method"], "GET");
    assert_eq!(e["src_ip"], "127.0.0.1");
    assert_no_identity(&e);
    assert_absent(&cap, &["canary-agent-bearer", CANARY_COOKIE]);
}

#[tokio::test]
async fn unregistered_agent_is_logged_with_its_verified_id() {
    let c = ctx();
    let cancel = CancellationToken::new();
    let _stop = cancel.clone().drop_guard();
    let id = AgentId::new("canary-unregistered-agent").unwrap();
    let app = agent_app(Arc::clone(&c.state)).layer(Extension(PeerAgent(Some(id))));
    let addr = spawn(app, &cancel).await;
    let cap = LogCapture::new();
    let _g = cap.install();
    assert_eq!(agent_upgrade(addr).await, 403);
    let e = only_event(&cap);
    assert_auth_failure(&e, "agent_unregistered", 403);
    assert_eq!(e["agent"], "canary-unregistered-agent");
    assert_eq!(e["listener"], "agent");
    assert_eq!(e["route"], "/agent/v1");
    assert_eq!(e["src_ip"], "127.0.0.1");
    assert!(e.get("subject").is_none() && e.get("tenant").is_none());
    assert_absent(&cap, &["canary-agent-bearer", CANARY_COOKIE]);
    let stats = c.state.security.stats();
    assert_eq!(stats.events(Reason::AgentUnregistered), 1);
}

#[tokio::test]
async fn registered_agent_upgrade_logs_nothing() {
    let c = ctx();
    let admin = c.token("admin", 600);
    let req = Request::post("/api/v1/sites")
        .header("authorization", format!("Bearer {admin}"))
        .header("content-type", "application/json")
        .body(Body::from(
            json!({ "name": "Site A", "allowed_networks": ["192.168.50.0/24"] }).to_string(),
        ))
        .unwrap();
    let resp = c.app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let site: Value =
        serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let req = Request::post("/api/v1/agents")
        .header("authorization", format!("Bearer {admin}"))
        .header("content-type", "application/json")
        .body(Body::from(
            json!({ "agent_id": "agent-ok", "site_id": site["id"] }).to_string(),
        ))
        .unwrap();
    assert_eq!(c.send(req).await, StatusCode::CREATED);

    let cancel = CancellationToken::new();
    let _stop = cancel.clone().drop_guard();
    let id = AgentId::new("agent-ok").unwrap();
    let app = agent_app(Arc::clone(&c.state)).layer(Extension(PeerAgent(Some(id))));
    let addr = spawn(app, &cancel).await;
    let cap = LogCapture::new();
    let _g = cap.install();
    assert_eq!(agent_upgrade(addr).await, 101);
    assert_eq!(cap.contents(), "");
    assert_eq!(c.state.security.stats().tracked_sources, 0);
}
