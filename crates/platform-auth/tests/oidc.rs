//! OIDC verification tests.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::collections::BTreeMap;

use platform_auth::testing::TestIssuer;
use platform_auth::{AuthError, OidcConfig, OidcVerifier, Permission, SiteScope};
use platform_common::{SiteId, TenantId, now_utc};
use serde_json::json;

const ISS: &str = "https://idp.example.test/realms/radio";
const AUD: &str = "radio-platform";

fn config() -> OidcConfig {
    let mut c = OidcConfig::new(ISS, AUD);
    c.role_permissions = BTreeMap::from([
        (
            "operator".into(),
            vec![Permission::ScannerView, Permission::ScannerControl],
        ),
        ("viewer".into(), vec![Permission::ScannerView]),
    ]);
    c
}

fn verifier(issuer: &TestIssuer, c: OidcConfig) -> OidcVerifier {
    OidcVerifier::with_static_jwks(c, issuer.jwks())
}

#[tokio::test]
async fn valid_token_yields_principal() {
    let iss = TestIssuer::new(ISS, AUD);
    let v = verifier(&iss, config());
    let tenant = TenantId::new();
    let site = SiteId::new();
    let tok = iss.token("alice", 300, json!({
        "tenant_id": tenant.to_string(), "roles": ["operator", "unknown-role"], "sites": [site.to_string()], "name": "Alice"
    }));
    let p = v.verify(&tok, now_utc()).await.unwrap();
    assert_eq!(p.subject, "alice");
    assert_eq!(p.tenant_id, tenant);
    assert_eq!(p.sites, SiteScope::Sites([site].into()));
    assert!(p.has(Permission::ScannerControl));
    assert!(
        !p.has(Permission::ScannerAdmin),
        "unknown roles grant nothing"
    );

    let all = iss.token(
        "bob",
        300,
        json!({ "tenant_id": tenant.to_string(), "sites": ["*"] }),
    );
    let p = v.verify(&all, now_utc()).await.unwrap();
    assert_eq!(p.sites, SiteScope::All);
    assert!(p.permissions.is_empty());
}

#[tokio::test]
async fn rejects_bad_tokens() {
    let iss = TestIssuer::new(ISS, AUD);
    let v = verifier(&iss, config());
    let tenant = TenantId::new().to_string();
    let now = now_utc();

    assert_eq!(v.verify("", now).await, Err(AuthError::Malformed));
    assert_eq!(v.verify("not.a.jwt", now).await, Err(AuthError::Malformed));
    // Expired (beyond leeway).
    let t = iss.token("a", -120, json!({ "tenant_id": tenant }));
    assert_eq!(v.verify(&t, now).await, Err(AuthError::Expired));
    // Wrong audience / issuer.
    let other_aud = TestIssuer::new(ISS, "other");
    let v2 = OidcVerifier::with_static_jwks(config(), other_aud.jwks());
    assert_eq!(
        v2.verify(
            &other_aud.token("a", 300, json!({ "tenant_id": tenant })),
            now
        )
        .await,
        Err(AuthError::Invalid)
    );
    let other_iss = TestIssuer::new("https://evil.example.test", AUD);
    let v3 = OidcVerifier::with_static_jwks(config(), other_iss.jwks());
    assert_eq!(
        v3.verify(
            &other_iss.token("a", 300, json!({ "tenant_id": tenant })),
            now
        )
        .await,
        Err(AuthError::Invalid)
    );
    // Signed by a key not in the JWKS.
    let impostor = TestIssuer::new(ISS, AUD);
    assert_eq!(
        v.verify(
            &impostor.token("a", 300, json!({ "tenant_id": tenant })),
            now
        )
        .await,
        Err(AuthError::Invalid)
    );
    // Missing / malformed tenant and sites claims.
    assert!(matches!(
        v.verify(&iss.token("a", 300, json!({})), now).await,
        Err(AuthError::Claim(_))
    ));
    assert!(matches!(
        v.verify(&iss.token("a", 300, json!({ "tenant_id": "nope" })), now)
            .await,
        Err(AuthError::Claim(_))
    ));
    assert!(matches!(
        v.verify(
            &iss.token("a", 300, json!({ "tenant_id": tenant, "sites": "all" })),
            now
        )
        .await,
        Err(AuthError::Claim(_))
    ));
    // `alg: none` and HMAC are refused outright.
    let good = iss.token("a", 300, json!({ "tenant_id": tenant }));
    let parts: Vec<&str> = good.split('.').collect();
    let none_header = base64_url(r#"{"alg":"none","typ":"JWT"}"#);
    assert!(
        v.verify(&format!("{none_header}.{}.", parts[1]), now)
            .await
            .is_err()
    );
    let hs_header = base64_url(&format!(
        r#"{{"alg":"HS256","typ":"JWT","kid":"{}"}}"#,
        iss.kid
    ));
    assert_eq!(
        v.verify(&format!("{hs_header}.{}.{}", parts[1], parts[2]), now)
            .await,
        Err(AuthError::Invalid)
    );
}

fn base64_url(s: &str) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(s)
}

#[tokio::test]
async fn max_session_age_is_enforced() {
    let iss = TestIssuer::new(ISS, AUD);
    let mut c = config();
    c.max_session_secs = Some(60);
    let v = verifier(&iss, c);
    let tenant = TenantId::new().to_string();
    let now = now_utc().unix_timestamp();
    let old = iss.sign(&json!({ "iss": ISS, "aud": AUD, "sub": "a", "iat": now - 120, "exp": now + 600, "tenant_id": tenant }));
    assert_eq!(v.verify(&old, now_utc()).await, Err(AuthError::Expired));
    let fresh = iss.token("a", 600, json!({ "tenant_id": tenant }));
    let p = v.verify(&fresh, now_utc()).await.unwrap();
    assert!(
        p.expires_at <= now_utc() + time::Duration::seconds(61),
        "expiry capped at session age"
    );
}

#[tokio::test]
async fn discovery_and_key_rotation() {
    use axum::{Json, Router, routing::get};
    use std::sync::{Arc, Mutex};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let first = TestIssuer::new(&base, AUD);
    let jwks = Arc::new(Mutex::new(serde_json::to_value(first.jwks()).unwrap()));
    let disco = json!({ "issuer": base, "jwks_uri": format!("{base}/jwks") });
    let j = Arc::clone(&jwks);
    let app = Router::new()
        .route(
            "/.well-known/openid-configuration",
            get(move || async move { Json(disco) }),
        )
        .route(
            "/jwks",
            get(move || {
                let j = Arc::clone(&j);
                async move { Json(j.lock().unwrap().clone()) }
            }),
        );
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let mut c = OidcConfig::new(base.clone(), AUD);
    // http:// is refused unless explicitly allowed.
    assert!(matches!(
        OidcVerifier::discover(c.clone()).await,
        Err(AuthError::Config(_))
    ));
    c.allow_insecure_http = true;
    c.jwks_min_refresh_secs = 0;
    let v = OidcVerifier::discover(c).await.unwrap();
    let tenant = TenantId::new().to_string();
    v.verify(
        &first.token("a", 300, json!({ "tenant_id": tenant })),
        now_utc(),
    )
    .await
    .unwrap();

    // The provider rotates to a new key; an unknown kid triggers a refresh.
    let second = TestIssuer::new(&base, AUD);
    *jwks.lock().unwrap() = serde_json::to_value(second.jwks()).unwrap();
    v.verify(
        &second.token("a", 300, json!({ "tenant_id": tenant })),
        now_utc(),
    )
    .await
    .unwrap();
}

/// Pins the registered-claim checks that the JWT library performs for us,
/// so a dependency upgrade that relaxes one of them fails here.
#[tokio::test]
async fn rejects_registered_claim_edge_cases() {
    let iss = TestIssuer::new(ISS, AUD);
    let v = verifier(&iss, config());
    let tenant = TenantId::new().to_string();
    let now = now_utc();
    let ts = now.unix_timestamp();
    let base = || {
        json!({
            "iss": ISS, "aud": AUD, "sub": "a", "tenant_id": tenant,
            "iat": ts, "nbf": ts - 1, "exp": ts + 300,
        })
    };
    // Control: the base claims are accepted.
    assert!(v.verify(&iss.sign(&base()), now).await.is_ok());

    type Mutate = Box<dyn Fn(&mut serde_json::Value)>;
    let cases: Vec<(&str, Mutate)> = vec![
        (
            "nbf in the future",
            Box::new(move |c| c["nbf"] = json!(ts + 3600)),
        ),
        (
            "missing exp",
            Box::new(|c| {
                c.as_object_mut().unwrap().remove("exp");
            }),
        ),
        (
            "missing iss",
            Box::new(|c| {
                c.as_object_mut().unwrap().remove("iss");
            }),
        ),
        (
            "missing aud",
            Box::new(|c| {
                c.as_object_mut().unwrap().remove("aud");
            }),
        ),
        (
            "missing sub",
            Box::new(|c| {
                c.as_object_mut().unwrap().remove("sub");
            }),
        ),
        (
            "exp as string",
            Box::new(move |c| c["exp"] = json!((ts + 300).to_string())),
        ),
        ("iss as number", Box::new(|c| c["iss"] = json!(1))),
        ("iss as array", Box::new(|c| c["iss"] = json!([ISS]))),
        ("aud as number array", Box::new(|c| c["aud"] = json!([123]))),
        (
            "aud as object",
            Box::new(|c| c["aud"] = json!({ "a": AUD })),
        ),
        ("sub as number", Box::new(|c| c["sub"] = json!(7))),
        (
            "aud array without ours",
            Box::new(|c| c["aud"] = json!(["other"])),
        ),
    ];
    for (name, mutate) in cases {
        let mut claims = base();
        mutate(&mut claims);
        assert!(
            v.verify(&iss.sign(&claims), now).await.is_err(),
            "accepted a token with {name}"
        );
    }
}
