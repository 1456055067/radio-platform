//! Test helpers: an ES256 signing key with a matching JWKS, and token
//! minting. Enabled only with the `testing` feature.

#![allow(clippy::expect_used, missing_docs)]

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use jsonwebtoken::jwk::JwkSet;
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use rcgen::{KeyPair, PKCS_ECDSA_P256_SHA256};

#[derive(Debug)]
pub struct TestIssuer {
    pub issuer: String,
    pub audience: String,
    pub kid: String,
    encoding: EncodingKey,
    jwks: JwkSet,
}

impl TestIssuer {
    #[must_use]
    pub fn new(issuer: &str, audience: &str) -> Self {
        let kp = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).expect("keygen");
        let encoding =
            EncodingKey::from_ec_pem(kp.serialize_pem().as_bytes()).expect("encoding key");
        let raw = kp.public_key_raw();
        let (x, y) = raw
            .get(1..33)
            .zip(raw.get(33..65))
            .expect("uncompressed P-256 point");
        let kid = format!("test-{}", uuid::Uuid::new_v4().simple());
        let jwks: JwkSet = serde_json::from_value(serde_json::json!({
            "keys": [{
                "kty": "EC", "crv": "P-256", "alg": "ES256", "use": "sig", "kid": kid,
                "x": URL_SAFE_NO_PAD.encode(x), "y": URL_SAFE_NO_PAD.encode(y),
            }]
        }))
        .expect("jwks");
        Self {
            issuer: issuer.into(),
            audience: audience.into(),
            kid,
            encoding,
            jwks,
        }
    }

    #[must_use]
    pub fn jwks(&self) -> JwkSet {
        self.jwks.clone()
    }

    /// Mints a token with standard claims plus `extra`.
    #[must_use]
    pub fn token(&self, sub: &str, ttl_secs: i64, extra: serde_json::Value) -> String {
        let now = time::OffsetDateTime::now_utc().unix_timestamp();
        let mut claims = serde_json::json!({
            "iss": self.issuer, "aud": self.audience, "sub": sub,
            "iat": now, "nbf": now - 1, "exp": now + ttl_secs,
        });
        if let (Some(c), serde_json::Value::Object(e)) = (claims.as_object_mut(), extra) {
            c.extend(e);
        }
        self.sign(&claims)
    }

    /// Signs arbitrary claims.
    #[must_use]
    pub fn sign(&self, claims: &serde_json::Value) -> String {
        let mut h = Header::new(Algorithm::ES256);
        h.kid = Some(self.kid.clone());
        encode(&h, claims, &self.encoding).expect("sign")
    }
}
