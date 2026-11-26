//! Agent certificate revocation: periodic CRL reload for the agent
//! listener.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::hub::AgentHub;
use crate::tls::{TlsError, load_crls, mtls_server_config_with_crls};

/// Rebuilds the agent listener's TLS configuration when the CRL file
/// changes. A failed reload keeps the last good configuration; an expired
/// CRL is still rejected at handshake time, so stale CRLs fail closed.
#[derive(Debug)]
pub struct CrlReloader {
    cert: PathBuf,
    key: PathBuf,
    client_ca: PathBuf,
    crl: PathBuf,
    last: Vec<u8>,
    tx: watch::Sender<Arc<rustls::ServerConfig>>,
}

impl CrlReloader {
    /// Loads the initial configuration.
    ///
    /// # Errors
    /// Fails if the certificates, key, CA or CRL cannot be loaded.
    pub fn new(
        cert: &Path,
        key: &Path,
        client_ca: &Path,
        crl: &Path,
    ) -> Result<(Self, watch::Receiver<Arc<rustls::ServerConfig>>), TlsError> {
        let last = std::fs::read(crl).map_err(|e| TlsError::Pem {
            path: crl.display().to_string(),
            reason: e.to_string(),
        })?;
        let cfg = mtls_server_config_with_crls(cert, key, client_ca, load_crls(crl)?)?;
        let (tx, rx) = watch::channel(cfg);
        Ok((
            Self {
                cert: cert.to_owned(),
                key: key.to_owned(),
                client_ca: client_ca.to_owned(),
                crl: crl.to_owned(),
                last,
                tx,
            },
            rx,
        ))
    }

    /// Re-reads the CRL; returns `true` if a new configuration was installed.
    ///
    /// # Errors
    /// Fails if the changed CRL cannot be read or parsed (the previous
    /// configuration stays active).
    pub fn reload_if_changed(&mut self) -> Result<bool, TlsError> {
        let bytes = std::fs::read(&self.crl).map_err(|e| TlsError::Pem {
            path: self.crl.display().to_string(),
            reason: e.to_string(),
        })?;
        if bytes == self.last {
            return Ok(false);
        }
        let cfg = mtls_server_config_with_crls(
            &self.cert,
            &self.key,
            &self.client_ca,
            load_crls(&self.crl)?,
        )?;
        self.last = bytes;
        let _ = self.tx.send(cfg);
        Ok(true)
    }

    /// Reloads every `every` until cancelled. On change, connected agents are
    /// disconnected so they re-handshake against the new CRL.
    pub async fn run(mut self, every: Duration, hub: AgentHub, cancel: CancellationToken) {
        let mut tick = tokio::time::interval(every.max(Duration::from_secs(5)));
        tick.tick().await;
        loop {
            tokio::select! {
                () = cancel.cancelled() => break,
                _ = tick.tick() => match self.reload_if_changed() {
                    Ok(true) => {
                        let n = hub.disconnect_all();
                        info!(reconnecting = n, "agent CRL changed; agents must re-verify");
                    }
                    Ok(false) => {}
                    Err(e) => error!(error = %e, "CRL reload failed; keeping the previous CRL"),
                },
            }
        }
        warn!("CRL reloader stopped");
    }
}
