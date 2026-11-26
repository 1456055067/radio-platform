//! Authentication and authorization for the radio platform.
//!
//! - [`OidcVerifier`] verifies OIDC bearer tokens (JWT) against the issuer's
//!   JWKS and produces a [`Principal`].
//! - [`Principal::authorize`] applies attribute-based rules: tenant, site
//!   scope, permission and session expiry.

#![warn(missing_docs)]

mod abac;
mod oidc;

pub use abac::{Denied, Permission, Principal, Resource, SiteScope};
pub use jsonwebtoken::jwk::JwkSet;
pub use oidc::{AuthError, OidcConfig, OidcVerifier};

#[cfg(feature = "testing")]
pub mod testing;
