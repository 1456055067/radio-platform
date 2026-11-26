//! `scanner-agent` binary.

use std::process::ExitCode;

use scanner_agent::config::AgentConfig;

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

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .json()
        .init();
    let args: Vec<String> = std::env::args().collect();
    let (check, path) = match args.get(1).map(String::as_str) {
        Some("--check-config") => (
            true,
            args.get(2).cloned().unwrap_or_else(|| "agent.toml".into()),
        ),
        Some(p) => (false, p.to_owned()),
        None => (false, "agent.toml".into()),
    };
    let cfg: AgentConfig = match std::fs::read_to_string(&path)
        .map_err(|e| e.to_string())
        .and_then(|s| scanner_agent::config::parse_toml(&s))
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: cannot load configuration {path}: {e}");
            return ExitCode::from(2);
        }
    };
    if check {
        println!("{path}: configuration OK");
        return ExitCode::SUCCESS;
    }
    let agent = match scanner_agent::start(cfg) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    shutdown_signal().await;
    agent.shutdown().await;
    ExitCode::SUCCESS
}
