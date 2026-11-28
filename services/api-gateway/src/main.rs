//! `api-gateway` binary.

use std::process::ExitCode;
use std::sync::Arc;

use api_gateway::config::{DatabaseConfig, DbTransport, GatewayConfig};
use api_gateway::{AppState, agent_app, serve, tls, ui, with_ui};
use platform_auth::{JwkSet, OidcVerifier};
use platform_storage::{CheckedTarget, MemoryStore, PgStore, Store, TransportVerdict};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

/// Waits for Ctrl-C or, on Unix, SIGTERM (what Kubernetes and systemd send).
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = term.recv() => {}
                }
                return;
            }
            Err(e) => tracing::warn!(error = %e, "cannot listen for SIGTERM"),
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}

/// Logs the accepted database transport. Never logs the URL.
fn log_database_transport(checked: &CheckedTarget) {
    let target = checked.target();
    let (host, port, sslmode) = (target.host(), target.port(), target.ssl_mode());
    match checked.verdict() {
        TransportVerdict::InsecureAllowed => warn!(
            host,
            port,
            sslmode,
            "database connection is not verified TLS (insecure_transport = true); \
             credentials, audit records and transcripts may be read or altered on the network"
        ),
        TransportVerdict::Verified => {
            info!(host, port, sslmode, "database connection uses verified TLS");
        }
        TransportVerdict::Local => info!(host, port, sslmode, "database is on this host"),
    }
}

async fn run(cfg: GatewayConfig) -> Result<(), String> {
    cfg.validate()?;
    let mut bus: Option<platform_storage::PgBus> = None;
    let store: Arc<dyn Store> = match &cfg.database {
        DatabaseConfig::Postgres {
            max_connections, ..
        } => {
            let env = |k: &str| std::env::var(k).ok();
            let checked = match cfg.check_database_transport(&env)? {
                DbTransport::Checked(checked) => {
                    log_database_transport(&checked);
                    checked
                }
                DbTransport::UrlNotSet | DbTransport::NotPostgres => {
                    return Err("database url not configured (set DATABASE_URL)".into());
                }
            };
            let url = cfg
                .database
                .effective_url(&env)
                .ok_or("database url not configured (set DATABASE_URL)")?;
            let pg = PgStore::connect_target(&url, &checked, *max_connections)
                .await
                .map_err(|e| e.to_string())?;
            pg.migrate().await.map_err(|e| e.to_string())?;
            if cfg.cluster.enabled {
                bus = Some(
                    platform_storage::PgBus::start(pg.pool().clone())
                        .await
                        .map_err(|e| e.to_string())?,
                );
                info!("cluster mode: coordinating with other replicas over PostgreSQL");
            }
            Arc::new(pg)
        }
        DatabaseConfig::Memory => {
            if cfg.cluster.enabled {
                return Err("cluster.enabled requires the postgres database backend".into());
            }
            warn!("using the non-persistent in-memory store (development only)");
            Arc::new(MemoryStore::new())
        }
    };
    let verifier = match &cfg.auth.jwks_file {
        Some(path) => {
            let raw =
                std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
            let jwks: JwkSet =
                serde_json::from_str(&raw).map_err(|e| format!("{}: {e}", path.display()))?;
            OidcVerifier::with_static_jwks(cfg.auth.oidc.clone(), jwks)
        }
        None => OidcVerifier::discover(cfg.auth.oidc.clone())
            .await
            .map_err(|e| e.to_string())?,
    };
    let state = match &bus {
        Some(b) => AppState::clustered(
            store,
            Arc::new(verifier),
            cfg.limits.clone(),
            Arc::new(b.clone()),
        ),
        None => AppState::new(store, Arc::new(verifier), cfg.limits.clone()),
    };

    if let Some(m) = &cfg.media {
        let mut w = media_gateway::WebRtcConfig::new(m.udp_bind);
        w.public = m.public;
        w.max_sessions = m.max_sessions;
        let server = media_gateway::WebRtcServer::start(w, state.media.hub.clone())
            .await
            .map_err(|e| format!("media: {e}"))?;
        state
            .media
            .enable_webrtc(server, m.max_sessions_per_subject);
    } else {
        info!("live audio disabled (no [media] section)");
    }
    if let Some(r) = &cfg.recording {
        let store =
            scanner_recorder::RecordingStore::open(&r.storage).map_err(|e| e.to_string())?;
        state.media.enable_recording(
            store,
            scanner_recorder::SegmentPolicy {
                pre_buffer_ms: i64::from(r.pre_buffer_ms),
                post_roll_ms: i64::from(r.post_roll_ms),
                max_duration_ms: i64::from(r.max_duration_secs) * 1000,
                ..scanner_recorder::SegmentPolicy::default()
            },
            Arc::clone(&state.store),
        );
    } else {
        info!("recording disabled (no [recording] section)");
    }
    let transcription = match &cfg.transcription {
        Some(t) => {
            if cfg.recording.is_none() {
                return Err("[transcription] requires [recording]".into());
            }
            let t = Arc::new(
                api_gateway::transcription::Transcription::new(t.clone())
                    .map_err(|e| e.to_string())?,
            );
            if !t.client.is_local() {
                warn!(service = %t.client.describe(), "recordings are sent to a transcription service outside this host");
            }
            state.media.enable_transcription(Arc::clone(&t));
            Some(t)
        }
        None => {
            info!("transcription disabled (no [transcription] section)");
            None
        }
    };
    if cfg.retention.enabled() {
        let _ = state
            .retention
            .set(Arc::new(api_gateway::retention::Retention::new(
                cfg.retention.clone(),
            )));
    } else {
        info!("retention disabled: everything is kept (no [retention] limits)");
    }
    let public_tls = match (&cfg.http.tls, cfg.http.insecure_plaintext) {
        (Some(t), _) => Some(tls::server_config(&t.cert, &t.key).map_err(|e| e.to_string())?),
        (None, true) => {
            warn!("public API is serving plaintext HTTP; terminate TLS 1.3 in front of it");
            None
        }
        (None, false) => {
            return Err("http.tls is required unless http.insecure_plaintext = true".into());
        }
    };
    let (agent_tls, crl_reloader) = match &cfg.agents.client_crl {
        Some(crl) => {
            let (r, rx) = api_gateway::crl::CrlReloader::new(
                &cfg.agents.cert,
                &cfg.agents.key,
                &cfg.agents.client_ca,
                crl,
            )
            .map_err(|e| e.to_string())?;
            info!(crl = %crl.display(), "agent certificate revocation checking enabled");
            (serve::TlsSource::Reloadable(rx), Some(r))
        }
        None => {
            warn!("no agents.client_crl configured: revoked agent certificates are not detected");
            (
                serve::TlsSource::Static(
                    tls::mtls_server_config(
                        &cfg.agents.cert,
                        &cfg.agents.key,
                        &cfg.agents.client_ca,
                    )
                    .map_err(|e| e.to_string())?,
                ),
                None,
            )
        }
    };

    let ui_router = match &cfg.http.ui {
        Some(u) => {
            info!(dir = %u.dir.display(), "serving the web UI");
            Some(ui::router(u)?)
        }
        None => None,
    };
    let mut public_router = with_ui(Arc::clone(&state), ui_router);
    if public_tls.is_some() {
        public_router = public_router.layer(
            tower_http::set_header::SetResponseHeaderLayer::if_not_present(
                axum::http::header::STRICT_TRANSPORT_SECURITY,
                axum::http::HeaderValue::from_static("max-age=31536000"),
            ),
        );
    }
    let cancel = CancellationToken::new();
    let public = TcpListener::bind(cfg.http.bind)
        .await
        .map_err(|e| e.to_string())?;
    let agents = TcpListener::bind(cfg.agents.bind)
        .await
        .map_err(|e| e.to_string())?;
    let max = cfg.limits.max_connections;
    let t1 = tokio::spawn(serve::serve(
        public,
        public_tls,
        public_router,
        max,
        cancel.clone(),
    ));
    if let Some(r) = crl_reloader {
        tokio::spawn(r.run(
            std::time::Duration::from_secs(cfg.agents.crl_refresh_secs),
            state.hub.clone(),
            cancel.clone(),
        ));
    }
    let t2 = tokio::spawn(serve::serve_with(
        agents,
        Some(agent_tls),
        agent_app(Arc::clone(&state)),
        max,
        cancel.clone(),
    ));
    let recordings = state.media.recorder().map(|r| r.store.clone());
    if let (Some(t), Some(rec)) = (transcription, recordings.clone()) {
        tokio::spawn(t.run(
            Arc::clone(&state.store),
            rec,
            Arc::clone(&state.media.transcription_wake),
            cancel.clone(),
        ));
    }
    if let Some(r) = state.retention.get() {
        tokio::spawn(Arc::clone(r).run(Arc::clone(&state.store), recordings, cancel.clone()));
    }
    if let Some(m) = &cfg.metrics {
        if !m.bind.ip().is_loopback() {
            warn!(bind = %m.bind, "metrics listener is not on loopback; restrict it to the monitoring network");
        }
        let l = TcpListener::bind(m.bind).await.map_err(|e| e.to_string())?;
        info!(bind = %m.bind, "metrics listening");
        tokio::spawn(serve::serve(
            l,
            None,
            api_gateway::metrics::router(Arc::clone(&state)),
            16,
            cancel.clone(),
        ));
    }
    let leases = {
        let state = Arc::clone(&state);
        let cancel = cancel.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(30));
            let mut ticks: u64 = 0;
            loop {
                tokio::select! {
                    () = cancel.cancelled() => break,
                    _ = tick.tick() => {
                        // Hourly (and at start): log every tenant's audit
                        // chain head so it is anchored outside the database.
                        if ticks.is_multiple_of(120) {
                            match state.store.audit_heads().await {
                                Ok(heads) => for (t, h) in heads {
                                    info!(tenant = %t, seq = h.seq, hash = %h.hash, "audit chain head");
                                },
                                Err(e) => warn!(error = %e, "could not read audit chain heads"),
                            }
                        }
                        ticks += 1;
                        state.leases.purge_expired(platform_common::now_utc()).await;
                        state.limiter.retain_recent();
                        state.ip_limiter.retain_recent();
                        // Summaries for sources that went quiet; frees their slots.
                        state.security.flush();
                        if let Some(b) = &bus {
                            let _ = b.purge_spill(std::time::Duration::from_secs(300)).await;
                        }
                    }
                }
            }
        })
    };
    shutdown_signal().await;
    info!("shutting down");
    cancel.cancel();
    let _ = tokio::join!(t1, t2, leases);
    Ok(())
}

/// `--check-config`: static checks plus the database transport policy for
/// the URL in this environment. Exit 2 on any refusal.
fn check_config(path: &str, cfg: &GatewayConfig) -> ExitCode {
    if let Err(e) = cfg.validate() {
        eprintln!("error: {path}: {e}");
        return ExitCode::from(2);
    }
    match cfg.check_database_transport(&|k| std::env::var(k).ok()) {
        Err(e) => {
            eprintln!("error: {path}: {e}");
            return ExitCode::from(2);
        }
        Ok(DbTransport::UrlNotSet) => println!(
            "{path}: note: database URL not set here (DATABASE_URL); the transport policy is \
             checked at start-up"
        ),
        Ok(DbTransport::Checked(checked))
            if checked.verdict() == TransportVerdict::InsecureAllowed =>
        {
            eprintln!(
                "warning: {path}: database {checked} is not verified TLS; allowed only because \
                 [database] insecure_transport = true"
            );
        }
        Ok(DbTransport::Checked(checked)) => println!("{path}: database {checked}: OK"),
        Ok(DbTransport::NotPostgres) => {}
    }
    println!("{path}: configuration OK");
    ExitCode::SUCCESS
}

/// Two layers: the general log (JSON, `RUST_LOG`, default `info`) without
/// security events, and the security log (see `api_gateway::security_log`),
/// always on at `WARN` and written without span context, so the request
/// span's raw URI never reaches a security event.
fn init_tracing() {
    use tracing_subscriber::util::SubscriberInitExt as _;

    let env =
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());
    // NIST 800-53 Rev5: AU-12 (the security layer is independent of RUST_LOG; see security_log::subscriber)
    api_gateway::security_log::subscriber(env, std::io::stdout, std::io::stdout).init();
}

#[tokio::main]
async fn main() -> ExitCode {
    init_tracing();
    let args: Vec<String> = std::env::args().collect();
    let (check, path) = match args.get(1).map(String::as_str) {
        Some("--check-config") => (
            true,
            args.get(2)
                .cloned()
                .unwrap_or_else(|| "gateway.toml".into()),
        ),
        Some(p) => (false, p.to_owned()),
        None => (false, "gateway.toml".into()),
    };
    let cfg: GatewayConfig = match std::fs::read_to_string(&path)
        .map_err(|e| e.to_string())
        .and_then(|s| api_gateway::config::parse_toml(&s))
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: cannot load configuration {path}: {e}");
            return ExitCode::from(2);
        }
    };
    if check {
        return check_config(&path, &cfg);
    }
    match run(cfg).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            error!(error = %e, "gateway failed");
            ExitCode::FAILURE
        }
    }
}
