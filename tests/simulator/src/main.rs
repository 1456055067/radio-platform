//! `sds-simulator` command-line entry point.
//!
//! ```text
//! sds-simulator [--bind ADDR] [--model NAME] [--firmware VERSION]
//!               [--drop P] [--duplicate P] [--malformed P] [--latency-ms MS]
//!               [--split-xml-header] [--seed N] [--audio ADDR]
//! ```

use std::process::ExitCode;
use std::time::Duration;

use sds_simulator::{Simulator, SimulatorConfig};

fn parse_args() -> Result<SimulatorConfig, String> {
    let mut cfg = SimulatorConfig {
        bind: std::net::SocketAddr::from(([127, 0, 0, 1], 50536)),
        ..SimulatorConfig::default()
    };
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let mut value = || {
            args.next()
                .ok_or_else(|| format!("{flag} requires a value"))
        };
        let prob = |v: String| -> Result<f64, String> {
            v.parse::<f64>()
                .ok()
                .filter(|p| (0.0..=1.0).contains(p))
                .ok_or_else(|| format!("invalid probability `{v}`"))
        };
        match flag.as_str() {
            "--bind" => cfg.bind = value()?.parse().map_err(|e| format!("--bind: {e}"))?,
            "--model" => cfg.state.model = value()?,
            "--firmware" => cfg.state.firmware = value()?,
            "--drop" => {
                let p = prob(value()?)?;
                cfg.faults.drop_request = p;
                cfg.faults.drop_reply = p;
            }
            "--duplicate" => cfg.faults.duplicate = prob(value()?)?,
            "--malformed" => cfg.faults.malformed = prob(value()?)?,
            "--latency-ms" => {
                cfg.faults.latency = Duration::from_millis(
                    value()?.parse().map_err(|e| format!("--latency-ms: {e}"))?,
                );
            }
            "--split-xml-header" => cfg.faults.split_xml_header = true,
            "--audio" => {
                cfg.audio = Some(value()?.parse().map_err(|e| format!("--audio: {e}"))?);
            }
            "--seed" => cfg.seed = value()?.parse().map_err(|e| format!("--seed: {e}"))?,
            "--help" | "-h" => return Err(String::new()),
            other => return Err(format!("unknown argument `{other}`")),
        }
    }
    Ok(cfg)
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .json()
        .init();
    let cfg = match parse_args() {
        Ok(c) => c,
        Err(e) => {
            if !e.is_empty() {
                eprintln!("error: {e}");
            }
            eprintln!(
                "usage: sds-simulator [--bind ADDR] [--model NAME] [--firmware VER] [--drop P] \
                 [--duplicate P] [--malformed P] [--latency-ms MS] [--split-xml-header] [--seed N] [--audio ADDR]"
            );
            return ExitCode::from(2);
        }
    };
    let sim = match Simulator::start(cfg).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: cannot start simulator: {e}");
            return ExitCode::FAILURE;
        }
    };
    tracing::info!(addr = %sim.addr(), "SDS200 simulator listening (simulated behaviour, not hardware-verified)");
    let _ = tokio::signal::ctrl_c().await;
    sim.shutdown().await;
    ExitCode::SUCCESS
}
