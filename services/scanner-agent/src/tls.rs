//! mTLS client configuration (TLS 1.3 only, AWS-LC provider).

use std::path::Path;
use std::sync::Arc;

use rustls::RootCertStore;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};

use crate::AgentError;

fn certs(path: &Path) -> Result<Vec<CertificateDer<'static>>, AgentError> {
    let v = CertificateDer::pem_file_iter(path)
        .and_then(|it| it.collect::<Result<Vec<_>, _>>())
        .map_err(|e| AgentError::Config(format!("{}: {e}", path.display())))?;
    if v.is_empty() {
        return Err(AgentError::Config(format!(
            "{}: no certificates",
            path.display()
        )));
    }
    Ok(v)
}

/// Builds the client configuration.
///
/// # Errors
/// Returns an error for unreadable or invalid key material.
pub(crate) fn client_config(
    ca: &Path,
    cert: &Path,
    key: &Path,
) -> Result<Arc<rustls::ClientConfig>, AgentError> {
    let mut roots = RootCertStore::empty();
    for c in certs(ca)? {
        roots
            .add(c)
            .map_err(|e| AgentError::Config(e.to_string()))?;
    }
    let key = PrivateKeyDer::from_pem_file(key)
        .map_err(|e| AgentError::Config(format!("{}: {e}", key.display())))?;
    let cfg = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])
    .map_err(|e| AgentError::Config(e.to_string()))?
    .with_root_certificates(roots)
    .with_client_auth_cert(certs(cert)?, key)
    .map_err(|e| AgentError::Config(e.to_string()))?;
    Ok(Arc::new(cfg))
}
