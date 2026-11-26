//! Connection acceptance: optional TLS, bounded concurrency, handshake
//! timeouts and graceful shutdown. The client address and the verified
//! agent identity (mTLS) are attached to every request as extensions.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::Request;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto::Builder;
use hyper_util::server::graceful::GracefulShutdown;
use hyper_util::service::TowerToHyperService;
use platform_common::AgentId;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use tokio_rustls::TlsAcceptor;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;
use tracing::{debug, info, warn};

use crate::tls::agent_identity;

/// The TCP peer address.
#[derive(Debug, Clone, Copy)]
pub struct ClientAddr(pub SocketAddr);

/// Identity from a verified client certificate, if any.
#[derive(Debug, Clone)]
pub struct PeerAgent(pub Option<AgentId>);

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Where a listener gets its TLS configuration.
#[derive(Debug, Clone)]
pub enum TlsSource {
    /// Fixed for the listener's lifetime.
    Static(Arc<rustls::ServerConfig>),
    /// Re-read for every new connection (for example after a CRL reload).
    Reloadable(tokio::sync::watch::Receiver<Arc<rustls::ServerConfig>>),
}

impl TlsSource {
    fn current(&self) -> TlsAcceptor {
        match self {
            Self::Static(c) => TlsAcceptor::from(Arc::clone(c)),
            Self::Reloadable(rx) => TlsAcceptor::from(Arc::clone(&rx.borrow())),
        }
    }
}

async fn serve_conn<S>(
    io: S,
    app: Router,
    peer: SocketAddr,
    agent: Option<AgentId>,
    graceful: &GracefulShutdown,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let svc = app.map_request(move |mut req: Request<hyper::body::Incoming>| {
        req.extensions_mut().insert(ClientAddr(peer));
        req.extensions_mut().insert(PeerAgent(agent.clone()));
        req.map(axum::body::Body::new)
    });
    let builder = Builder::new(TokioExecutor::new());
    let conn =
        builder.serve_connection_with_upgrades(TokioIo::new(io), TowerToHyperService::new(svc));
    if let Err(e) = graceful.watch(conn.into_owned()).await {
        debug!(%peer, error = %e, "connection closed with error");
    }
}

/// Serves `app` until `cancel` fires, then drains connections for up to
/// 10 seconds.
pub async fn serve(
    listener: TcpListener,
    tls: Option<Arc<rustls::ServerConfig>>,
    app: Router,
    max_connections: usize,
    cancel: CancellationToken,
) {
    serve_with(
        listener,
        tls.map(TlsSource::Static),
        app,
        max_connections,
        cancel,
    )
    .await;
}

/// Like [`serve`], with a possibly reloadable TLS configuration.
pub async fn serve_with(
    listener: TcpListener,
    tls: Option<TlsSource>,
    app: Router,
    max_connections: usize,
    cancel: CancellationToken,
) {
    let acceptor = tls;
    let permits = Arc::new(Semaphore::new(max_connections.max(1)));
    let graceful = Arc::new(GracefulShutdown::new());
    if let Ok(addr) = listener.local_addr() {
        info!(%addr, tls = acceptor.is_some(), "listening");
    }
    loop {
        let (tcp, peer) = tokio::select! {
            () = cancel.cancelled() => break,
            r = listener.accept() => match r {
                Ok(v) => v,
                Err(e) => { warn!(error = %e, "accept failed"); tokio::time::sleep(Duration::from_millis(100)).await; continue; }
            },
        };
        let Ok(permit) = Arc::clone(&permits).try_acquire_owned() else {
            warn!(%peer, "connection limit reached; dropping connection");
            continue;
        };
        let app = app.clone();
        let acceptor = acceptor.as_ref().map(TlsSource::current);
        let graceful = Arc::clone(&graceful);
        tokio::spawn(async move {
            let _permit = permit;
            match acceptor {
                None => serve_conn(tcp, app, peer, None, &graceful).await,
                Some(acc) => match tokio::time::timeout(HANDSHAKE_TIMEOUT, acc.accept(tcp)).await {
                    Ok(Ok(stream)) => {
                        let agent = stream
                            .get_ref()
                            .1
                            .peer_certificates()
                            .and_then(|c| c.first())
                            .and_then(agent_identity);
                        serve_conn(stream, app, peer, agent, &graceful).await;
                    }
                    Ok(Err(e)) => debug!(%peer, error = %e, "TLS handshake failed"),
                    Err(_) => debug!(%peer, "TLS handshake timed out"),
                },
            }
        });
    }
    if let Ok(g) = Arc::try_unwrap(graceful) {
        let _ = tokio::time::timeout(Duration::from_secs(10), g.shutdown()).await;
    }
}
