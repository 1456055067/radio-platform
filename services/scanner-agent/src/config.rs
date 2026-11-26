//! Agent configuration (TOML).

use std::path::PathBuf;

use platform_common::wire::ScannerAssignment;
use platform_common::{NetworkPolicy, ScannerId, SiteId};
use serde::Deserialize;

/// Upstream gateway connection.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GatewayConfig {
    /// `wss://host:port/agent/v1`.
    pub url: String,
    /// CA bundle trusted for the gateway's server certificate.
    pub ca: PathBuf,
    /// Agent client certificate (its DNS SAN is the agent identity).
    pub cert: PathBuf,
    /// Agent private key.
    pub key: PathBuf,
}

/// Telemetry collection mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TelemetryMode {
    /// `PSI` pushes with polling fallback.
    Push,
    /// `GSI` polling.
    Poll,
}

/// Telemetry settings.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TelemetryConfig {
    /// Mode.
    #[serde(default = "default_mode")]
    pub mode: TelemetryMode,
    /// Push or poll interval in milliseconds.
    #[serde(default = "default_interval")]
    pub interval_ms: u32,
    /// Squelch-closed time that ends a transmission, in milliseconds.
    #[serde(default = "default_hang")]
    pub hang_time_ms: u64,
}

fn default_mode() -> TelemetryMode {
    TelemetryMode::Push
}
fn default_interval() -> u32 {
    500
}
fn default_hang() -> u64 {
    2000
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            mode: default_mode(),
            interval_ms: default_interval(),
            hang_time_ms: default_hang(),
        }
    }
}

/// A locally configured scanner (manual enrollment). Used until the gateway
/// sends central assignments, so collection continues during outages.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalScanner {
    /// Identifier (as enrolled centrally).
    pub scanner_id: ScannerId,
    /// Site.
    pub site_id: Option<SiteId>,
    /// Name.
    pub name: String,
    /// Address.
    pub host: std::net::IpAddr,
    /// Port.
    #[serde(default = "default_port")]
    pub port: u16,
}

fn default_port() -> u16 {
    50536
}

impl From<&LocalScanner> for ScannerAssignment {
    fn from(s: &LocalScanner) -> Self {
        Self {
            scanner_id: s.scanner_id,
            site_id: s.site_id,
            name: s.name.clone(),
            host: s.host,
            port: s.port,
            enabled: true,
            audio: None,
        }
    }
}

/// Complete agent configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentConfig {
    /// Gateway.
    pub gateway: GatewayConfig,
    /// Authorized scanner networks. **Enforced locally** regardless of what
    /// the gateway requests.
    pub network: NetworkPolicy,
    /// Telemetry.
    #[serde(default)]
    pub telemetry: TelemetryConfig,
    /// Maximum buffered events during upstream outages (oldest dropped).
    #[serde(default = "default_buffer")]
    pub max_buffered_events: usize,
    /// Directory for the durable event journal (created `0700`). Without
    /// it, buffered events are lost if the agent restarts during an outage.
    #[serde(default)]
    pub state_dir: Option<std::path::PathBuf>,
    /// Maximum managed scanners.
    #[serde(default = "default_max_scanners")]
    pub max_scanners: usize,
    /// Bootstrap scanners.
    #[serde(default)]
    pub scanners: Vec<LocalScanner>,
    /// Audio pipeline settings.
    #[serde(default)]
    pub audio: crate::audio::AudioSettings,
}

fn default_buffer() -> usize {
    10_000
}
fn default_max_scanners() -> usize {
    32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_configuration_parses_with_every_key_in_place() {
        let raw = include_str!("../../../deployments/examples/agent.example.toml");
        let cfg: AgentConfig = toml::from_str(raw).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(cfg.max_buffered_events, 10_000);
        assert_eq!(cfg.max_scanners, 32);
        assert!(cfg.state_dir.is_some());
        // A misplaced key is an error, not silently ignored.
        let misplaced = "[gateway]\nurl='wss://x/agent/v1'\nca='a'\ncert='b'\nkey='c'\n[network]\nallowed_networks=[]\n[telemetry]\nmax_scanners = 3\n";
        assert!(toml::from_str::<AgentConfig>(misplaced).is_err());
    }
}
