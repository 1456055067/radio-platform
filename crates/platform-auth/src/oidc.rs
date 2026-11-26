//! OpenID Connect bearer-token verification.
//!
//! - Tokens are verified against the issuer's JWKS, either fetched through
//!   discovery or supplied statically for air-gapped deployments.
//! - Only asymmetric algorithms are accepted, which prevents
//!   algorithm-confusion attacks.
//! - Issuer, audience, expiry and not-before are enforced, and so is an
//!   optional maximum session age based on `iat`.
//! - Tenant, sites and roles come from configurable claims. Roles map to
//!   [`Permission`]s through configuration; unknown roles grant nothing.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use jsonwebtoken::jwk::JwkSet;
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
use platform_common::{SiteId, TenantId};
use serde::Deserialize;
use thiserror::Error;
use time::OffsetDateTime;
use tokio::sync::RwLock;
use tracing::{debug, warn};

use crate::abac::{Permission, Principal, SiteScope};

/// OIDC configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct OidcConfig {
    /// Expected `iss`.
    pub issuer: String,
    /// Expected `aud`.
    pub audience: String,
    /// Explicit JWKS URI; discovered from the issuer if absent.
    pub jwks_uri: Option<String>,
    /// Claim carrying the tenant UUID.
    #[serde(default = "default_tenant_claim")]
    pub tenant_claim: String,
    /// Claim carrying site UUIDs (array). Absent or containing `"*"` means
    /// all sites.
    #[serde(default = "default_sites_claim")]
    pub sites_claim: String,
    /// Claim carrying role names (array of strings).
    #[serde(default = "default_roles_claim")]
    pub roles_claim: String,
    /// Role → permissions mapping.
    #[serde(default)]
    pub role_permissions: BTreeMap<String, Vec<Permission>>,
    /// Clock skew allowance in seconds.
    #[serde(default = "default_leeway")]
    pub leeway_secs: u64,
    /// Reject tokens issued longer ago than this (seconds), even if not yet
    /// expired. `None` disables the check.
    pub max_session_secs: Option<u64>,
    /// Minimum seconds between JWKS refreshes triggered by unknown `kid`s.
    #[serde(default = "default_refresh")]
    pub jwks_min_refresh_secs: u64,
    /// Permit `http://` issuer and JWKS URLs (tests only).
    #[serde(default)]
    pub allow_insecure_http: bool,
}

fn default_tenant_claim() -> String {
    "tenant_id".into()
}
fn default_sites_claim() -> String {
    "sites".into()
}
fn default_roles_claim() -> String {
    "roles".into()
}
fn default_leeway() -> u64 {
    30
}
fn default_refresh() -> u64 {
    60
}

impl OidcConfig {
    /// Minimal configuration with defaults.
    #[must_use]
    pub fn new(issuer: impl Into<String>, audience: impl Into<String>) -> Self {
        Self {
            issuer: issuer.into(),
            audience: audience.into(),
            jwks_uri: None,
            tenant_claim: default_tenant_claim(),
            sites_claim: default_sites_claim(),
            roles_claim: default_roles_claim(),
            role_permissions: BTreeMap::new(),
            leeway_secs: default_leeway(),
            max_session_secs: None,
            jwks_min_refresh_secs: default_refresh(),
            allow_insecure_http: false,
        }
    }
}

/// Authentication failures. Messages are safe to return to clients.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AuthError {
    /// Missing or malformed credentials.
    #[error("missing or malformed bearer token")]
    Malformed,
    /// Token failed verification.
    #[error("invalid token")]
    Invalid,
    /// Token expired or session too old.
    #[error("token expired")]
    Expired,
    /// Required claim missing or invalid.
    #[error("token is missing a valid `{0}` claim")]
    Claim(String),
    /// Identity provider unreachable or misconfigured.
    #[error("identity provider unavailable")]
    Provider,
    /// Configuration error.
    #[error("OIDC configuration error: {0}")]
    Config(String),
}

const ALLOWED_ALGS: [Algorithm; 6] = [
    Algorithm::RS256,
    Algorithm::RS384,
    Algorithm::RS512,
    Algorithm::ES256,
    Algorithm::ES384,
    Algorithm::EdDSA,
];

#[derive(Debug)]
struct KeyCache {
    keys: JwkSet,
    fetched_at: Option<tokio::time::Instant>,
}

/// Verifies bearer tokens.
#[derive(Debug)]
pub struct OidcVerifier {
    config: OidcConfig,
    jwks_uri: Option<String>,
    http: Option<reqwest::Client>,
    cache: RwLock<KeyCache>,
}

#[derive(Deserialize)]
struct Discovery {
    issuer: String,
    jwks_uri: String,
}

fn check_url(url: &str, allow_http: bool) -> Result<(), AuthError> {
    if url.starts_with("https://") || (allow_http && url.starts_with("http://")) {
        Ok(())
    } else {
        Err(AuthError::Config(format!("URL must use https: {url}")))
    }
}

impl OidcVerifier {
    /// A verifier with a fixed key set (air-gapped deployments and tests).
    #[must_use]
    pub fn with_static_jwks(config: OidcConfig, keys: JwkSet) -> Self {
        Self {
            config,
            jwks_uri: None,
            http: None,
            cache: RwLock::new(KeyCache {
                keys,
                fetched_at: None,
            }),
        }
    }

    /// Discovers the JWKS URI (unless configured) and fetches keys.
    ///
    /// # Errors
    /// Returns an error if discovery or the initial key fetch fails, or if
    /// the discovered issuer does not match the configuration.
    pub async fn discover(config: OidcConfig) -> Result<Self, AuthError> {
        check_url(&config.issuer, config.allow_insecure_http)?;
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .https_only(!config.allow_insecure_http)
            .build()
            .map_err(|e| AuthError::Config(e.to_string()))?;
        let jwks_uri = match &config.jwks_uri {
            Some(u) => u.clone(),
            None => {
                let url = format!(
                    "{}/.well-known/openid-configuration",
                    config.issuer.trim_end_matches('/')
                );
                let d: Discovery = http
                    .get(&url)
                    .send()
                    .await
                    .and_then(reqwest::Response::error_for_status)
                    .map_err(|e| {
                        warn!(error = %e, "OIDC discovery failed");
                        AuthError::Provider
                    })?
                    .json()
                    .await
                    .map_err(|_| AuthError::Provider)?;
                if d.issuer != config.issuer {
                    return Err(AuthError::Config(
                        "discovered issuer does not match configuration".into(),
                    ));
                }
                d.jwks_uri
            }
        };
        check_url(&jwks_uri, config.allow_insecure_http)?;
        let v = Self {
            config,
            jwks_uri: Some(jwks_uri),
            http: Some(http),
            cache: RwLock::new(KeyCache {
                keys: JwkSet { keys: vec![] },
                fetched_at: None,
            }),
        };
        v.refresh().await?;
        Ok(v)
    }

    async fn refresh(&self) -> Result<(), AuthError> {
        let (Some(http), Some(uri)) = (&self.http, &self.jwks_uri) else {
            return Ok(());
        };
        let keys: JwkSet = http
            .get(uri)
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(|e| {
                warn!(error = %e, "JWKS fetch failed");
                AuthError::Provider
            })?
            .json()
            .await
            .map_err(|_| AuthError::Provider)?;
        let mut cache = self.cache.write().await;
        cache.keys = keys;
        cache.fetched_at = Some(tokio::time::Instant::now());
        Ok(())
    }

    async fn key_for(&self, kid: Option<&str>) -> Result<DecodingKey, AuthError> {
        let find = |set: &JwkSet| -> Option<DecodingKey> {
            let jwk = match kid {
                Some(k) => set.find(k)?,
                None if set.keys.len() == 1 => set.keys.first()?,
                None => return None,
            };
            DecodingKey::from_jwk(jwk).ok()
        };
        if let Some(k) = find(&self.cache.read().await.keys) {
            return Ok(k);
        }
        // Unknown kid: maybe the provider rotated keys. Refresh, rate-limited.
        let due =
            self.cache.read().await.fetched_at.is_none_or(|t| {
                t.elapsed() >= Duration::from_secs(self.config.jwks_min_refresh_secs)
            });
        if due && self.http.is_some() {
            debug!("unknown kid; refreshing JWKS");
            self.refresh().await?;
            if let Some(k) = find(&self.cache.read().await.keys) {
                return Ok(k);
            }
        }
        Err(AuthError::Invalid)
    }

    /// Verifies a bearer token and builds a [`Principal`].
    ///
    /// # Errors
    /// See [`AuthError`].
    pub async fn verify(&self, token: &str, now: OffsetDateTime) -> Result<Principal, AuthError> {
        if token.is_empty() || token.len() > 16 * 1024 {
            return Err(AuthError::Malformed);
        }
        let header = decode_header(token).map_err(|_| AuthError::Malformed)?;
        if !ALLOWED_ALGS.contains(&header.alg) {
            return Err(AuthError::Invalid);
        }
        let key = self.key_for(header.kid.as_deref()).await?;
        let mut validation = Validation::new(header.alg);
        validation.algorithms = vec![header.alg];
        validation.set_issuer(&[&self.config.issuer]);
        validation.set_audience(&[&self.config.audience]);
        validation.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);
        validation.leeway = self.config.leeway_secs;
        validation.validate_nbf = true;
        let data = decode::<serde_json::Map<String, serde_json::Value>>(token, &key, &validation)
            .map_err(|e| match e.kind() {
            jsonwebtoken::errors::ErrorKind::ExpiredSignature => AuthError::Expired,
            _ => AuthError::Invalid,
        })?;
        self.principal(&data.claims, now)
    }

    fn principal(
        &self,
        c: &serde_json::Map<String, serde_json::Value>,
        now: OffsetDateTime,
    ) -> Result<Principal, AuthError> {
        let claim = |name: &str| AuthError::Claim(name.to_owned());
        // The JWT library checks the values of `iss` and `aud` but accepts
        // `iss` as an array (jsonwebtoken 10 and 11). RFC 7519 4.1.1 makes
        // `iss` a single string; `aud` is a string or an array of strings.
        // Check the types here so the result does not depend on the library.
        // NIST 800-53 Rev5: IA-2 (only well-formed tokens from our issuer).
        if c.get("iss").and_then(|v| v.as_str()) != Some(self.config.issuer.as_str()) {
            return Err(claim("iss"));
        }
        let audience_ok = match c.get("aud") {
            Some(serde_json::Value::String(a)) => a == &self.config.audience,
            Some(serde_json::Value::Array(list)) => {
                list.iter().all(serde_json::Value::is_string)
                    && list
                        .iter()
                        .any(|a| a.as_str() == Some(self.config.audience.as_str()))
            }
            _ => false,
        };
        if !audience_ok {
            return Err(claim("aud"));
        }
        let subject = c
            .get("sub")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty() && s.len() <= 256)
            .ok_or_else(|| claim("sub"))?;
        let exp = c
            .get("exp")
            .and_then(serde_json::Value::as_i64)
            .ok_or_else(|| claim("exp"))?;
        let mut expires_at = OffsetDateTime::from_unix_timestamp(exp).map_err(|_| claim("exp"))?;
        if let Some(max) = self.config.max_session_secs {
            let iat = c
                .get("iat")
                .and_then(serde_json::Value::as_i64)
                .ok_or_else(|| claim("iat"))?;
            let issued = OffsetDateTime::from_unix_timestamp(iat).map_err(|_| claim("iat"))?;
            let session_end =
                issued + time::Duration::seconds(i64::try_from(max).unwrap_or(i64::MAX / 2));
            if now >= session_end {
                return Err(AuthError::Expired);
            }
            expires_at = expires_at.min(session_end);
        }
        let tenant_id = c
            .get(&self.config.tenant_claim)
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<TenantId>().ok())
            .ok_or_else(|| claim(&self.config.tenant_claim))?;
        let sites = match c.get(&self.config.sites_claim) {
            None => SiteScope::All,
            Some(serde_json::Value::Array(items)) => {
                let strs: Vec<&str> = items.iter().filter_map(|v| v.as_str()).collect();
                if strs.len() != items.len() {
                    return Err(claim(&self.config.sites_claim));
                }
                if strs.contains(&"*") {
                    SiteScope::All
                } else {
                    let set = strs
                        .iter()
                        .map(|s| s.parse::<SiteId>())
                        .collect::<Result<BTreeSet<_>, _>>()
                        .map_err(|_| claim(&self.config.sites_claim))?;
                    SiteScope::Sites(set)
                }
            }
            Some(_) => return Err(claim(&self.config.sites_claim)),
        };
        let mut permissions = BTreeSet::new();
        if let Some(serde_json::Value::Array(roles)) = c.get(&self.config.roles_claim) {
            for role in roles.iter().filter_map(|r| r.as_str()) {
                if let Some(perms) = self.config.role_permissions.get(role) {
                    permissions.extend(perms.iter().copied());
                }
            }
        }
        Ok(Principal {
            subject: subject.to_owned(),
            name: c.get("name").and_then(|v| v.as_str()).map(str::to_owned),
            tenant_id,
            sites,
            permissions,
            expires_at,
        })
    }
}
