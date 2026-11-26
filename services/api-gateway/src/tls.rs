//! TLS 1.3 configuration (rustls with the AWS-LC provider).

use std::path::Path;
use std::sync::Arc;

use platform_common::AgentId;
use rustls::RootCertStore;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, CertificateRevocationListDer, PrivateKeyDer};
use rustls::server::WebPkiClientVerifier;
use thiserror::Error;

/// TLS setup errors.
#[derive(Debug, Error)]
pub enum TlsError {
    /// PEM could not be read.
    #[error("{path}: {reason}")]
    Pem {
        /// File.
        path: String,
        /// Reason.
        reason: String,
    },
    /// rustls rejected the configuration.
    #[error("TLS configuration: {0}")]
    Rustls(String),
}

fn pem_err(path: &Path, e: impl std::fmt::Display) -> TlsError {
    TlsError::Pem {
        path: path.display().to_string(),
        reason: e.to_string(),
    }
}

/// Loads a PEM certificate chain.
///
/// # Errors
/// Returns an error if the file is unreadable or holds no certificates.
pub fn load_certs(path: &Path) -> Result<Vec<CertificateDer<'static>>, TlsError> {
    let certs = CertificateDer::pem_file_iter(path)
        .map_err(|e| pem_err(path, e))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| pem_err(path, e))?;
    if certs.is_empty() {
        return Err(pem_err(path, "no certificates found"));
    }
    Ok(certs)
}

/// Loads a PEM private key.
///
/// # Errors
/// Returns an error if the file is unreadable or holds no key.
pub fn load_key(path: &Path) -> Result<PrivateKeyDer<'static>, TlsError> {
    PrivateKeyDer::from_pem_file(path).map_err(|e| pem_err(path, e))
}

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::aws_lc_rs::default_provider())
}

/// Server config for the public API: TLS 1.3 only.
///
/// # Errors
/// Returns an error for invalid key material.
pub fn server_config(cert: &Path, key: &Path) -> Result<Arc<rustls::ServerConfig>, TlsError> {
    let mut cfg = rustls::ServerConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|e| TlsError::Rustls(e.to_string()))?
        .with_no_client_auth()
        .with_single_cert(load_certs(cert)?, load_key(key)?)
        .map_err(|e| TlsError::Rustls(e.to_string()))?;
    cfg.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(Arc::new(cfg))
}

/// Server config for the agent listener: TLS 1.3 with **required** client
/// certificates issued by `client_ca`.
///
/// # Errors
/// Returns an error for invalid key material or CA.
pub fn mtls_server_config(
    cert: &Path,
    key: &Path,
    client_ca: &Path,
) -> Result<Arc<rustls::ServerConfig>, TlsError> {
    mtls_server_config_with_crls(cert, key, client_ca, Vec::new())
}

/// Loads PEM certificate revocation lists.
///
/// # Errors
/// Returns an error if the file is unreadable or holds no CRL.
pub fn load_crls(path: &Path) -> Result<Vec<CertificateRevocationListDer<'static>>, TlsError> {
    let crls = CertificateRevocationListDer::pem_file_iter(path)
        .map_err(|e| pem_err(path, e))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| pem_err(path, e))?;
    if crls.is_empty() {
        return Err(pem_err(path, "no CRLs found"));
    }
    Ok(crls)
}

/// Like [`mtls_server_config`], additionally checking client certificates
/// against `crls`. With CRLs present, revocation checking is strict: a
/// revoked certificate, a certificate whose revocation status cannot be
/// determined from the CRLs, or an **expired** CRL (past `nextUpdate`) all
/// fail the handshake. Publish fresh CRLs before they expire.
///
/// # Errors
/// Returns an error for invalid key material, CA or CRLs.
pub fn mtls_server_config_with_crls(
    cert: &Path,
    key: &Path,
    client_ca: &Path,
    crls: Vec<CertificateRevocationListDer<'static>>,
) -> Result<Arc<rustls::ServerConfig>, TlsError> {
    let mut roots = RootCertStore::empty();
    for c in load_certs(client_ca)? {
        roots.add(c).map_err(|e| TlsError::Rustls(e.to_string()))?;
    }
    let mut builder = WebPkiClientVerifier::builder_with_provider(Arc::new(roots), provider());
    if !crls.is_empty() {
        builder = builder.with_crls(crls).enforce_revocation_expiration();
    }
    let verifier = builder
        .build()
        .map_err(|e| TlsError::Rustls(e.to_string()))?;
    let mut cfg = rustls::ServerConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|e| TlsError::Rustls(e.to_string()))?
        .with_client_cert_verifier(verifier)
        .with_single_cert(load_certs(cert)?, load_key(key)?)
        .map_err(|e| TlsError::Rustls(e.to_string()))?;
    cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(Arc::new(cfg))
}

/// Derives the agent identity from a verified client certificate: the first
/// DNS subject-alternative name, otherwise the subject common name.
#[must_use]
pub fn agent_identity(cert: &CertificateDer<'_>) -> Option<AgentId> {
    use x509_parser::extensions::GeneralName;
    let (_, parsed) = x509_parser::parse_x509_certificate(cert.as_ref()).ok()?;
    if let Ok(Some(san)) = parsed.subject_alternative_name() {
        for name in &san.value.general_names {
            if let GeneralName::DNSName(dns) = name {
                return AgentId::new(*dns).ok();
            }
        }
    }
    let cn = parsed.subject().iter_common_name().next()?.as_str().ok()?;
    AgentId::new(cn).ok()
}
