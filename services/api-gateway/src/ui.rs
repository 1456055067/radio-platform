//! Serves the web UI (a static single-page application) and its runtime
//! configuration from the public listener.
//!
//! - `/ui-config.json` tells the browser how to sign in. It contains only
//!   public OIDC client settings; it holds no secrets.
//! - Hashed assets under `/assets/` are cached for a year; `index.html` and
//!   the config are revalidated on every load.
//! - Every UI response carries a strict Content Security Policy: scripts
//!   only from this origin, network access only to this origin and the
//!   identity provider, no framing.
//! - Unknown paths outside `/api/` fall back to `index.html` so client-side
//!   routes survive a reload. Unknown `/api/` paths still return a JSON 404,
//!   and unknown `/assets/` files a plain 404: a browser still running an
//!   older build must see a failed chunk load, not HTML served as a script.

use std::path::PathBuf;

use axum::Router;
use axum::body::Body;
use axum::extract::Request;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde::{Deserialize, Serialize};
use tower::ServiceExt;
use tower_http::services::{ServeDir, ServeFile};

use crate::error::ApiError;

/// Browser sign-in settings published at `/ui-config.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum UiAuth {
    /// OIDC authorization code flow with PKCE (a public client: no secret).
    Oidc {
        /// Issuer URL (the discovery base).
        authority: String,
        /// Public client identifier registered for the UI.
        client_id: String,
        /// Requested scopes.
        #[serde(default = "default_scope")]
        scope: String,
    },
    /// Development only: the UI fetches tokens from `/dev/token`, which only
    /// the `radio-devstack` binary serves. The production gateway never
    /// mounts that route.
    Dev,
}

fn default_scope() -> String {
    "openid profile".into()
}

/// UI settings.
#[derive(Debug, Clone, Deserialize)]
pub struct UiConfig {
    /// Directory containing the built UI (`frontend/dist`).
    pub dir: PathBuf,
    /// Sign-in settings.
    pub auth: UiAuth,
}

#[derive(Debug, Serialize)]
struct PublicConfig<'a> {
    auth: &'a UiAuth,
}

fn origin_of(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    if scheme != "https" && scheme != "http" {
        return None;
    }
    let host = rest.split(['/', '?', '#']).next()?;
    if host.is_empty() || host.contains(['\'', ';', ' ', '"']) {
        return None;
    }
    Some(format!("{scheme}://{host}"))
}

/// The Content Security Policy for UI documents.
#[must_use]
pub fn content_security_policy(auth: &UiAuth) -> String {
    let idp = match auth {
        UiAuth::Oidc { authority, .. } => origin_of(authority)
            .map(|o| format!(" {o}"))
            .unwrap_or_default(),
        UiAuth::Dev => String::new(),
    };
    format!(
        "default-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline'; \
         img-src 'self' data:; font-src 'self' data:; connect-src 'self'{idp}; \
         form-action 'self'{idp}; frame-ancestors 'none'; base-uri 'none'; \
         object-src 'none'; manifest-src 'self'; media-src 'self' blob:"
    )
}

/// Builds the UI router. Merge it after the API router.
///
/// # Errors
/// Fails if `dir/index.html` does not exist or the policy is unusable.
pub fn router(cfg: &UiConfig) -> Result<Router, String> {
    let index = cfg.dir.join("index.html");
    if !index.is_file() {
        return Err(format!(
            "{} not found; build the UI with `npm run build` in frontend/",
            index.display()
        ));
    }
    if let UiAuth::Oidc { authority, .. } = &cfg.auth
        && origin_of(authority).is_none()
    {
        return Err(format!(
            "ui.auth.authority `{authority}` is not an http(s) URL"
        ));
    }
    let csp = HeaderValue::from_str(&content_security_policy(&cfg.auth))
        .map_err(|e| format!("content security policy: {e}"))?;
    let body = serde_json::to_vec(&PublicConfig { auth: &cfg.auth })
        .map_err(|e| format!("ui config: {e}"))?;
    let static_files = ServeDir::new(&cfg.dir)
        .append_index_html_on_directories(false)
        .fallback(ServeFile::new(index));
    let assets = ServeDir::new(&cfg.dir).append_index_html_on_directories(false);

    let config_route = get(move || {
        let body = body.clone();
        async move {
            (
                [
                    (header::CONTENT_TYPE, "application/json"),
                    (header::CACHE_CONTROL, "no-cache"),
                ],
                body,
            )
        }
    });

    let fallback = move |req: Request| {
        let files = static_files.clone();
        let assets = assets.clone();
        let csp = csp.clone();
        async move { serve_static(files, assets, csp, req).await }
    };

    Ok(Router::new()
        .route("/ui-config.json", config_route)
        .fallback(fallback))
}

async fn serve_static(
    files: ServeDir<ServeFile>,
    assets: ServeDir,
    csp: HeaderValue,
    req: Request,
) -> Response {
    let path = req.uri().path().to_owned();
    if path.starts_with("/api/") || path == "/api" || path.starts_with("/dev/") {
        return ApiError::new(StatusCode::NOT_FOUND, "no such endpoint").into_response();
    }
    if req.method() != axum::http::Method::GET && req.method() != axum::http::Method::HEAD {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    let served = if path.starts_with("/assets/") {
        assets.oneshot(req).await
    } else {
        files.oneshot(req).await
    };
    let mut resp = match served {
        Ok(r) => r.map(Body::new),
        Err(e) => match e {},
    };
    let immutable = path.starts_with("/assets/") && resp.status() == StatusCode::OK;
    let h = resp.headers_mut();
    h.insert(header::CONTENT_SECURITY_POLICY, csp);
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(if immutable {
            "public, max-age=31536000, immutable"
        } else {
            "no-cache"
        }),
    );
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csp_includes_only_the_idp_origin() {
        let csp = content_security_policy(&UiAuth::Oidc {
            authority: "https://idp.example.org/realms/radio?x=1".into(),
            client_id: "ui".into(),
            scope: default_scope(),
        });
        assert!(csp.contains("connect-src 'self' https://idp.example.org;"));
        assert!(csp.contains("script-src 'self';"));
        assert!(csp.contains("frame-ancestors 'none'"));
        assert!(!csp.contains("/realms"));
    }

    #[test]
    fn hostile_authority_is_rejected() {
        assert_eq!(origin_of("javascript:alert(1)"), None);
        assert_eq!(origin_of("https://a.example; script-src *"), None);
        assert_eq!(origin_of("https://"), None);
    }
}
