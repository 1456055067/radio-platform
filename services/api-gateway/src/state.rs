//! Shared application state and the authentication extractor.

use std::net::IpAddr;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::FromRequestParts;
use axum::http::StatusCode;
use axum::http::request::Parts;
use governor::{DefaultKeyedRateLimiter, Quota};
use platform_auth::{OidcVerifier, Permission, Principal, Resource};
use platform_common::now_utc;
use platform_storage::Store;
use platform_storage::model::{AuditOutcome, NewAudit};
use scanner_controller::LeasePolicy;

use crate::config::LimitsConfig;
use crate::error::ApiError;
use crate::hub::AgentHub;
use crate::serve::ClientAddr;

/// Application state shared by all handlers.
#[derive(Debug)]
pub struct AppState {
    /// Persistence.
    pub store: Arc<dyn Store>,
    /// Token verification.
    pub verifier: Arc<OidcVerifier>,
    /// Connected agents.
    pub hub: AgentHub,
    /// Control leases.
    pub leases: crate::leases::Leases,
    /// Per-subject rate limiter.
    pub limiter: DefaultKeyedRateLimiter<String>,
    /// Per-client-IP rate limiter (applied before authentication).
    pub ip_limiter: DefaultKeyedRateLimiter<IpAddr>,
    /// Limits.
    pub limits: LimitsConfig,
    /// Live audio, recording and transcription.
    pub media: Arc<crate::media::Media>,
    /// Retention policy, if any limit is configured.
    pub retention: std::sync::OnceLock<Arc<crate::retention::Retention>>,
    /// Prometheus metrics.
    pub metrics: crate::metrics::Metrics,
}

impl AppState {
    /// Builds state.
    #[must_use]
    pub fn new(
        store: Arc<dyn Store>,
        verifier: Arc<OidcVerifier>,
        limits: LimitsConfig,
    ) -> Arc<Self> {
        Self::with_hub(store, verifier, limits, AgentHub::new())
    }

    /// Builds state for one replica of a multi-replica deployment. All
    /// replicas must share the store and the bus.
    #[must_use]
    pub fn clustered(
        store: Arc<dyn Store>,
        verifier: Arc<OidcVerifier>,
        limits: LimitsConfig,
        bus: Arc<dyn platform_storage::MessageBus>,
    ) -> Arc<Self> {
        Self::with_hub(store, verifier, limits, AgentHub::clustered(bus))
    }

    fn with_hub(
        store: Arc<dyn Store>,
        verifier: Arc<OidcVerifier>,
        limits: LimitsConfig,
        hub: AgentHub,
    ) -> Arc<Self> {
        let rpm = NonZeroU32::new(limits.requests_per_minute).unwrap_or(NonZeroU32::MIN);
        let rpm_ip = NonZeroU32::new(limits.requests_per_minute_per_ip).unwrap_or(NonZeroU32::MIN);
        Arc::new(Self {
            store: Arc::clone(&store),
            verifier,
            media: {
                let m = Arc::new(crate::media::Media::new(hub.clone()));
                hub.set_media(&m);
                m
            },
            hub,
            leases: crate::leases::Leases::new(
                Arc::clone(&store),
                LeasePolicy {
                    default_ttl: Duration::from_secs(limits.lease_default_secs),
                    max_ttl: Duration::from_secs(limits.lease_max_secs),
                },
            ),
            limiter: DefaultKeyedRateLimiter::keyed(Quota::per_minute(rpm)),
            ip_limiter: DefaultKeyedRateLimiter::keyed(Quota::per_minute(rpm_ip)),
            limits,
            retention: std::sync::OnceLock::new(),
            metrics: crate::metrics::Metrics::default(),
        })
    }

    /// Timeout for relayed commands.
    #[must_use]
    pub fn command_timeout(&self) -> Duration {
        Duration::from_secs(self.limits.command_timeout_secs.max(1))
    }
}

/// An authenticated request context.
#[derive(Debug, Clone)]
pub struct Auth {
    /// The caller.
    pub principal: Principal,
    /// Client address, if known.
    pub ip: Option<IpAddr>,
}

pub(crate) fn bearer(parts: &Parts) -> Option<String> {
    if let Some(v) = parts
        .headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    {
        return v.strip_prefix("Bearer ").map(|t| t.trim().to_owned());
    }
    // Browsers cannot set headers on WebSocket requests, so the token may be
    // carried as a subprotocol: `radio-platform.bearer.<jwt>`.
    parts
        .headers
        .get(axum::http::header::SEC_WEBSOCKET_PROTOCOL)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| {
            v.split(',')
                .map(str::trim)
                .find_map(|p| p.strip_prefix("radio-platform.bearer."))
        })
        .map(str::to_owned)
}

impl FromRequestParts<Arc<AppState>> for Auth {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        let token = bearer(parts)
            .ok_or_else(|| ApiError::new(StatusCode::UNAUTHORIZED, "missing bearer token"))?;
        let principal = state.verifier.verify(&token, now_utc()).await?;
        if state.limiter.check_key(&principal.subject).is_err() {
            return Err(ApiError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "rate limit exceeded",
            ));
        }
        let ip = client_ip(
            &parts.headers,
            &parts.extensions,
            &state.limits.trusted_proxies,
        );
        Ok(Self { principal, ip })
    }
}

/// What an audited action concerns.
#[derive(Debug, Clone)]
pub struct AuditTarget<'a> {
    /// Action name.
    pub action: &'a str,
    /// Resource type.
    pub resource_type: &'a str,
    /// Resource id.
    pub resource_id: Option<String>,
}

impl AppState {
    /// Authorizes and, on denial, writes a `denied` audit record.
    ///
    /// # Errors
    /// Returns the mapped denial.
    pub async fn authorize(
        &self,
        auth: &Auth,
        perm: Permission,
        res: Resource,
        target: Option<&AuditTarget<'_>>,
    ) -> Result<(), ApiError> {
        match auth.principal.authorize(perm, res, now_utc()) {
            Ok(()) => Ok(()),
            Err(denied) => {
                if let Some(t) = target {
                    self.audit(
                        auth,
                        t,
                        AuditOutcome::Denied,
                        serde_json::json!({ "reason": denied.to_string() }),
                    )
                    .await;
                }
                Err(denied.into())
            }
        }
    }

    /// Writes an audit record. Failures are logged, never surfaced.
    pub async fn audit(
        &self,
        auth: &Auth,
        t: &AuditTarget<'_>,
        outcome: AuditOutcome,
        details: serde_json::Value,
    ) {
        let rec = NewAudit {
            tenant_id: auth.principal.tenant_id,
            actor: auth.principal.subject.clone(),
            action: t.action.to_owned(),
            resource_type: t.resource_type.to_owned(),
            resource_id: t.resource_id.clone(),
            outcome,
            details,
            source_ip: auth.ip,
        };
        if let Err(e) = self.store.append_audit(rec).await {
            tracing::error!(error = %e, action = t.action, "failed to write audit record");
        }
    }
}

/// The client's address: the TCP peer, or, when the peer is a trusted
/// proxy, the right-most `X-Forwarded-For` entry that is not itself a
/// trusted proxy. Entries left of an untrusted hop are attacker-controlled
/// and ignored.
#[must_use]
pub fn client_ip(
    headers: &axum::http::HeaderMap,
    extensions: &axum::http::Extensions,
    trusted: &[ipnet::IpNet],
) -> Option<IpAddr> {
    let peer = extensions.get::<ClientAddr>().map(|c| c.0.ip())?;
    if !trusted.iter().any(|n| n.contains(&peer)) {
        return Some(peer);
    }
    let mut client = peer;
    let hops: Vec<&str> = headers
        .get_all("x-forwarded-for")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(str::trim)
        .collect();
    for hop in hops.iter().rev() {
        let Ok(ip) = hop.parse::<IpAddr>() else {
            break;
        };
        client = ip;
        if !trusted.iter().any(|n| n.contains(&ip)) {
            break;
        }
    }
    Some(client)
}

/// Middleware: per-IP limit before any authentication work.
pub async fn ip_rate_limit(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let path = req.uri().path();
    if path == "/healthz" || path == "/readyz" {
        return next.run(req).await;
    }
    if let Some(ip) = client_ip(
        req.headers(),
        req.extensions(),
        &state.limits.trusted_proxies,
    ) && state.ip_limiter.check_key(&ip).is_err()
    {
        let mut resp =
            ApiError::new(StatusCode::TOO_MANY_REQUESTS, "rate limit exceeded").into_response();
        resp.headers_mut().insert(
            axum::http::header::RETRY_AFTER,
            axum::http::HeaderValue::from_static("5"),
        );
        return resp;
    }
    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{Extensions, HeaderMap, HeaderValue};

    fn ext(peer: &str) -> Extensions {
        let mut e = Extensions::new();
        e.insert(ClientAddr(
            format!("{peer}:1234")
                .parse()
                .unwrap_or_else(|e| panic!("{e}")),
        ));
        e
    }

    #[test]
    fn forwarded_for_is_only_trusted_from_trusted_proxies() {
        let mut h = HeaderMap::new();
        h.insert(
            "x-forwarded-for",
            HeaderValue::from_static("6.6.6.6, 203.0.113.9, 10.0.0.2"),
        );
        let trusted: Vec<ipnet::IpNet> =
            vec!["10.0.0.0/24".parse().unwrap_or_else(|e| panic!("{e}"))];
        let ip = |s: &str| s.parse::<IpAddr>().ok();
        // Untrusted peer: the header is ignored entirely.
        assert_eq!(
            client_ip(&h, &ext("198.51.100.7"), &trusted),
            ip("198.51.100.7")
        );
        // Trusted peer: skip trusted hops from the right; stop at the first
        // untrusted one (the forged left-most entry is never used).
        assert_eq!(client_ip(&h, &ext("10.0.0.1"), &trusted), ip("203.0.113.9"));
        // No proxies configured: always the peer.
        assert_eq!(client_ip(&h, &ext("10.0.0.1"), &[]), ip("10.0.0.1"));
        // Garbage header from a trusted proxy: fall back to the proxy itself.
        let mut bad = HeaderMap::new();
        bad.insert("x-forwarded-for", HeaderValue::from_static("not-an-ip"));
        assert_eq!(client_ip(&bad, &ext("10.0.0.1"), &trusted), ip("10.0.0.1"));
    }
}
