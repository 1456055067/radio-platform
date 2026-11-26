//! RFC 9457 problem responses.

use axum::Json;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use platform_auth::{AuthError, Denied};
use platform_common::CommandError;
use platform_storage::StoreError;
use scanner_controller::LeaseError;
use serde::Serialize;
use utoipa::ToSchema;

/// Problem details body.
#[derive(Debug, Serialize, ToSchema)]
pub struct Problem {
    /// Problem type URI.
    #[serde(rename = "type")]
    pub kind: &'static str,
    /// Short summary.
    pub title: &'static str,
    /// HTTP status.
    pub status: u16,
    /// Human-readable detail.
    pub detail: String,
}

/// An API error.
#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    detail: String,
}

impl ApiError {
    /// Human-readable detail.
    #[must_use]
    pub fn detail(&self) -> &str {
        &self.detail
    }

    /// Creates an error.
    #[must_use]
    pub fn new(status: StatusCode, detail: impl Into<String>) -> Self {
        Self {
            status,
            detail: detail.into(),
        }
    }
    /// 400.
    pub fn bad_request(d: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, d)
    }
    /// 404.
    pub fn not_found() -> Self {
        Self::new(StatusCode::NOT_FOUND, "resource not found")
    }
    /// 403.
    pub fn forbidden(d: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, d)
    }
    /// 500 without leaking internals.
    pub fn internal() -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
    }
    /// The status code.
    #[must_use]
    pub fn status(&self) -> StatusCode {
        self.status
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = Problem {
            kind: "about:blank",
            title: self.status.canonical_reason().unwrap_or("Error"),
            status: self.status.as_u16(),
            detail: self.detail,
        };
        let mut r = (self.status, Json(body)).into_response();
        r.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/problem+json"),
        );
        if self.status == StatusCode::UNAUTHORIZED {
            r.headers_mut()
                .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        }
        r
    }
}

impl From<StoreError> for ApiError {
    fn from(e: StoreError) -> Self {
        match e {
            StoreError::NotFound => Self::not_found(),
            StoreError::Conflict(d) => Self::new(StatusCode::CONFLICT, d),
            StoreError::Invalid(d) => Self::bad_request(d),
            StoreError::Backend(d) => {
                tracing::error!(error = %d, "storage failure");
                Self::new(StatusCode::SERVICE_UNAVAILABLE, "storage unavailable")
            }
        }
    }
}

impl From<AuthError> for ApiError {
    fn from(e: AuthError) -> Self {
        match e {
            AuthError::Provider => Self::new(StatusCode::SERVICE_UNAVAILABLE, e.to_string()),
            AuthError::Config(_) => Self::internal(),
            _ => Self::new(StatusCode::UNAUTHORIZED, e.to_string()),
        }
    }
}

impl From<Denied> for ApiError {
    fn from(e: Denied) -> Self {
        match e {
            // Do not reveal resources in other tenants.
            Denied::Tenant => Self::not_found(),
            Denied::Expired => Self::new(StatusCode::UNAUTHORIZED, e.to_string()),
            _ => Self::forbidden(e.to_string()),
        }
    }
}

impl From<LeaseError> for ApiError {
    fn from(e: LeaseError) -> Self {
        match e {
            LeaseError::Required => Self::new(
                StatusCode::PRECONDITION_REQUIRED,
                "an active control lease is required (X-Lease-Id header)",
            ),
            LeaseError::NotHolder => Self::forbidden(e.to_string()),
            LeaseError::TtlTooLong(_) => Self::bad_request(e.to_string()),
            LeaseError::Held { .. } | LeaseError::NotFound => {
                Self::new(StatusCode::CONFLICT, e.to_string())
            }
        }
    }
}

impl From<CommandError> for ApiError {
    fn from(e: CommandError) -> Self {
        let status = match &e {
            CommandError::Invalid { .. } => StatusCode::BAD_REQUEST,
            CommandError::Unsupported => StatusCode::NOT_IMPLEMENTED,
            CommandError::Rejected { .. } => StatusCode::CONFLICT,
            CommandError::Timeout { .. } => StatusCode::GATEWAY_TIMEOUT,
            CommandError::ScannerNotFound | CommandError::Unavailable { .. } => {
                StatusCode::SERVICE_UNAVAILABLE
            }
            CommandError::Internal { .. } => StatusCode::BAD_GATEWAY,
        };
        Self::new(status, e.to_string())
    }
}
