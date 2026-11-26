//! Gateway configuration (TOML).

use std::net::SocketAddr;
use std::path::PathBuf;

use platform_auth::OidcConfig;
use platform_storage::{CheckedTarget, PgTarget};
use serde::Deserialize;

/// Parses a configuration file. On error, returns only the parser's message
/// and the `line:column` of the problem: never the offending source line,
/// which may hold a secret such as a database password.
///
/// # Errors
/// The text is not valid TOML or does not match the configuration schema.
pub fn parse_toml<T: serde::de::DeserializeOwned>(src: &str) -> Result<T, String> {
    // NIST 800-53 Rev5: SI-11 (configuration errors do not echo the source line)
    // POAM: a serde type or variant mismatch can still name the mis-typed value
    toml::from_str(src).map_err(|e| {
        let message = e.message().trim_end();
        match e.span().and_then(|s| src.get(..s.start)) {
            Some(before) => {
                let line = before.matches('\n').count() + 1;
                let column = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
                format!("line {line}, column {column}: {message}")
            }
            None => message.to_owned(),
        }
    })
}

/// PEM file locations for a TLS listener.
#[derive(Debug, Clone, Deserialize)]
pub struct TlsFiles {
    /// Certificate chain (PEM).
    pub cert: PathBuf,
    /// Private key (PEM).
    pub key: PathBuf,
}

/// Public API listener.
#[derive(Debug, Clone, Deserialize)]
pub struct HttpConfig {
    /// Bind address.
    pub bind: SocketAddr,
    /// TLS 1.3 certificate. Required unless `insecure_plaintext` is set.
    pub tls: Option<TlsFiles>,
    /// Serve plaintext HTTP. Only for deployments where a TLS-terminating
    /// ingress sits in front of the gateway.
    #[serde(default)]
    pub insecure_plaintext: bool,
    /// Serve the web UI from this listener.
    pub ui: Option<crate::ui::UiConfig>,
}

/// Agent listener (mutual TLS required).
#[derive(Debug, Clone, Deserialize)]
pub struct AgentListenerConfig {
    /// Bind address.
    pub bind: SocketAddr,
    /// Server certificate.
    pub cert: PathBuf,
    /// Server key.
    pub key: PathBuf,
    /// CA bundle that issues agent client certificates.
    pub client_ca: PathBuf,
    /// PEM CRL(s) from the agent CA. When set, revoked agents are refused,
    /// the file is re-read every `crl_refresh_secs`, and on change every
    /// connected agent is made to reconnect (and re-verify).
    pub client_crl: Option<PathBuf>,
    /// CRL re-read interval in seconds.
    #[serde(default = "default_crl_refresh")]
    pub crl_refresh_secs: u64,
}

fn default_crl_refresh() -> u64 {
    300
}

/// Storage backend.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "backend", rename_all = "snake_case")]
pub enum DatabaseConfig {
    /// PostgreSQL.
    Postgres {
        /// Connection URL. Prefer `DATABASE_URL` in the environment.
        url: Option<String>,
        /// Pool size.
        #[serde(default = "default_pool")]
        max_connections: u32,
        /// Accept a non-loopback database without `sslmode=verify-full`
        /// (plaintext or unverified TLS). Off by default; the gateway warns
        /// at every start when it is used.
        #[serde(default)]
        insecure_transport: bool,
    },
    /// Non-persistent in-memory store (development only).
    Memory,
}

fn default_pool() -> u32 {
    10
}

impl DatabaseConfig {
    /// The database URL actually used: `DATABASE_URL` from `env`, else
    /// `[database] url`. `None` for the memory backend or when neither is set.
    #[must_use]
    pub fn effective_url(&self, env: &dyn Fn(&str) -> Option<String>) -> Option<String> {
        match self {
            Self::Postgres { url, .. } => env("DATABASE_URL").or_else(|| url.clone()),
            Self::Memory => None,
        }
    }
}

/// Result of [`GatewayConfig::check_database_transport`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DbTransport {
    /// The memory backend: no database connection.
    NotPostgres,
    /// PostgreSQL, but no URL here (`DATABASE_URL` is typically set only at
    /// run time); the policy is checked again at start-up.
    UrlNotSet,
    /// The URL passed the transport policy: the target (safe to log) and the
    /// verdict, whose [`platform_storage::TransportVerdict::InsecureAllowed`]
    /// must be reported as a warning. Required by `PgStore::connect_target`.
    Checked(CheckedTarget),
}

/// API behaviour limits.
#[derive(Debug, Clone, Deserialize)]
pub struct LimitsConfig {
    /// Requests per minute per authenticated subject.
    #[serde(default = "default_rpm")]
    pub requests_per_minute: u32,
    /// Requests per minute per client IP, enforced **before**
    /// authentication (protects token verification and the login-less
    /// endpoints). Health probes are exempt.
    #[serde(default = "default_rpm_ip")]
    pub requests_per_minute_per_ip: u32,
    /// Reverse proxies whose `X-Forwarded-For` is trusted to name the
    /// client. Empty (the default) means the TCP peer is the client.
    #[serde(default)]
    pub trusted_proxies: Vec<ipnet::IpNet>,
    /// Timeout for commands relayed to agents, in seconds.
    #[serde(default = "default_cmd_timeout")]
    pub command_timeout_secs: u64,
    /// Maximum request body size in bytes.
    #[serde(default = "default_body")]
    pub max_body_bytes: usize,
    /// Maximum concurrent connections per listener.
    #[serde(default = "default_conns")]
    pub max_connections: usize,
    /// Authorized scanner UDP ports for enrollment.
    #[serde(default = "default_ports")]
    pub scanner_ports: Vec<u16>,
    /// Authorized scanner RTSP (audio) ports.
    #[serde(default = "default_audio_ports")]
    pub audio_ports: Vec<u16>,
    /// Default lease TTL in seconds.
    #[serde(default = "default_lease")]
    pub lease_default_secs: u64,
    /// Maximum lease TTL in seconds.
    #[serde(default = "default_lease_max")]
    pub lease_max_secs: u64,
}

fn default_rpm() -> u32 {
    600
}
fn default_rpm_ip() -> u32 {
    1200
}
fn default_cmd_timeout() -> u64 {
    15
}
fn default_body() -> usize {
    64 * 1024
}
fn default_conns() -> usize {
    1024
}
fn default_ports() -> Vec<u16> {
    vec![50536]
}
fn default_audio_ports() -> Vec<u16> {
    vec![554]
}
fn default_lease() -> u64 {
    300
}
fn default_lease_max() -> u64 {
    3600
}

impl Default for LimitsConfig {
    fn default() -> Self {
        Self {
            requests_per_minute: default_rpm(),
            requests_per_minute_per_ip: default_rpm_ip(),
            trusted_proxies: Vec::new(),
            command_timeout_secs: default_cmd_timeout(),
            max_body_bytes: default_body(),
            max_connections: default_conns(),
            scanner_ports: default_ports(),
            audio_ports: default_audio_ports(),
            lease_default_secs: default_lease(),
            lease_max_secs: default_lease_max(),
        }
    }
}

/// Identity provider settings.
#[derive(Debug, Clone, Deserialize)]
pub struct AuthConfig {
    /// OIDC settings.
    #[serde(flatten)]
    pub oidc: OidcConfig,
    /// Static JWKS file for air-gapped deployments (skips discovery).
    pub jwks_file: Option<PathBuf>,
}

/// Live audio over WebRTC (Phase 3). Off unless configured.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaConfig {
    /// UDP address for WebRTC media.
    pub udp_bind: std::net::SocketAddr,
    /// Address advertised to browsers (required for wildcard binds or NAT).
    pub public: Option<std::net::SocketAddr>,
    /// Maximum concurrent listening sessions on this replica.
    #[serde(default = "default_media_sessions")]
    pub max_sessions: usize,
    /// Maximum concurrent sessions per user.
    #[serde(default = "default_media_per_subject")]
    pub max_sessions_per_subject: usize,
}

fn default_media_sessions() -> usize {
    256
}
fn default_media_per_subject() -> usize {
    4
}

/// Transmission recording (Phase 3). **Off unless configured**, and then
/// only for scanners with recording enabled.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordingConfig {
    /// Storage backend.
    pub storage: scanner_recorder::StorageConfig,
    /// Audio kept from before each transmission, in ms.
    #[serde(default = "default_pre")]
    pub pre_buffer_ms: u32,
    /// Audio kept after each transmission, in ms.
    #[serde(default = "default_post")]
    pub post_roll_ms: u32,
    /// Longest recording, in seconds.
    #[serde(default = "default_max_rec")]
    pub max_duration_secs: u32,
}

fn default_pre() -> u32 {
    1000
}
fn default_post() -> u32 {
    500
}
fn default_max_rec() -> u32 {
    300
}

/// Retention (Phase 5). Every limit is in days and **unset means keep
/// forever**. Recordings and transcripts never outlive their transmission;
/// held transmissions are exempt. The audit log is never swept.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetentionConfig {
    /// Delete recordings (audio objects) older than this.
    pub recordings_days: Option<u32>,
    /// Remove transcripts older than this.
    pub transcripts_days: Option<u32>,
    /// Delete transmission records older than this.
    pub transmissions_days: Option<u32>,
    /// Delete scanner events older than this.
    pub events_days: Option<u32>,
    /// Seconds between sweeps.
    #[serde(default = "default_sweep")]
    pub sweep_interval_secs: u64,
}

fn default_sweep() -> u64 {
    3600
}

impl Default for RetentionConfig {
    fn default() -> Self {
        Self {
            recordings_days: None,
            transcripts_days: None,
            transmissions_days: None,
            events_days: None,
            sweep_interval_secs: default_sweep(),
        }
    }
}

impl RetentionConfig {
    /// True if any limit is set.
    #[must_use]
    pub fn enabled(&self) -> bool {
        self.recordings_days.is_some()
            || self.transcripts_days.is_some()
            || self.transmissions_days.is_some()
            || self.events_days.is_some()
    }

    /// Checks the limits.
    ///
    /// # Errors
    /// A limit outside 1-36500 days, or a sweep interval outside 60 s-1 day.
    pub fn validate(&self) -> Result<(), String> {
        for (name, v) in [
            ("recordings_days", self.recordings_days),
            ("transcripts_days", self.transcripts_days),
            ("transmissions_days", self.transmissions_days),
            ("events_days", self.events_days),
        ] {
            if v.is_some_and(|d| !(1..=36_500).contains(&d)) {
                return Err(format!("retention.{name} must be 1-36500"));
            }
        }
        if !(60..=86_400).contains(&self.sweep_interval_secs) {
            return Err("retention.sweep_interval_secs must be 60-86400".into());
        }
        Ok(())
    }
}

/// Prometheus metrics listener (absent = no metrics endpoint). Serves
/// plain HTTP without authentication: bind it to an internal address.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetricsConfig {
    /// Bind address, for example `127.0.0.1:9464`.
    pub bind: SocketAddr,
}

/// Multi-replica operation (ADR 0008).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ClusterConfig {
    /// Coordinate with other replicas over PostgreSQL `LISTEN`/`NOTIFY`.
    /// Requires the `postgres` backend. Leave off for a single replica.
    #[serde(default)]
    pub enabled: bool,
}

/// Complete gateway configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct GatewayConfig {
    /// Public API.
    pub http: HttpConfig,
    /// Agent listener.
    pub agents: AgentListenerConfig,
    /// Database.
    pub database: DatabaseConfig,
    /// OIDC.
    pub auth: AuthConfig,
    /// Limits.
    #[serde(default)]
    pub limits: LimitsConfig,
    /// Multi-replica coordination.
    #[serde(default)]
    pub cluster: ClusterConfig,
    /// Live audio (absent = disabled).
    pub media: Option<MediaConfig>,
    /// Recording (absent = disabled).
    pub recording: Option<RecordingConfig>,
    /// Transcription (absent = disabled; requires recording).
    pub transcription: Option<scanner_transcription::TranscriptionConfig>,
    /// Retention (absent = keep everything).
    #[serde(default)]
    pub retention: RetentionConfig,
    /// Metrics listener (absent = disabled).
    pub metrics: Option<MetricsConfig>,
}

impl GatewayConfig {
    /// Static checks (no network or file access), run at start-up and by
    /// `api-gateway --check-config`.
    ///
    /// # Errors
    /// The first problem found.
    pub fn validate(&self) -> Result<(), String> {
        if self.http.tls.is_none() && !self.http.insecure_plaintext {
            return Err("http.tls is required unless http.insecure_plaintext = true".into());
        }
        if self.cluster.enabled && matches!(self.database, DatabaseConfig::Memory) {
            return Err("cluster.enabled requires the postgres database backend".into());
        }
        if self.transcription.is_some() && self.recording.is_none() {
            return Err("[transcription] requires [recording]".into());
        }
        if let Some(t) = &self.transcription {
            t.validate().map_err(|e| e.to_string())?;
        }
        self.retention.validate()?;
        if let Some(m) = &self.metrics
            && (m.bind == self.http.bind || m.bind == self.agents.bind)
        {
            return Err("metrics.bind must differ from the API and agent listeners".into());
        }
        if self.http.bind == self.agents.bind {
            return Err("http.bind and agents.bind must differ".into());
        }
        Ok(())
    }

    /// Applies the database transport policy to the URL that will be used
    /// (`DATABASE_URL` from `env` overrides `[database] url`). Reads `PG*`
    /// variables through `env` and nothing else: no files, no DNS.
    ///
    /// # Errors
    /// The URL is malformed or names a non-loopback host without
    /// `sslmode=verify-full` and `insecure_transport` is false. The message
    /// never contains the URL or its password.
    pub fn check_database_transport(
        &self,
        env: &dyn Fn(&str) -> Option<String>,
    ) -> Result<DbTransport, String> {
        let DatabaseConfig::Postgres {
            insecure_transport, ..
        } = &self.database
        else {
            return Ok(DbTransport::NotPostgres);
        };
        let Some(url) = self.database.effective_url(env) else {
            return Ok(DbTransport::UrlNotSet);
        };
        // NIST 800-53 Rev5: SC-8 (refuse an unverified off-loopback database link)
        // NIST 800-53 Rev5: CM-6 (enforced in --check-config and at start-up)
        let checked = PgTarget::parse(&url, env)
            .and_then(|t| t.check(*insecure_transport))
            .map_err(|e| e.to_string())?;
        Ok(DbTransport::Checked(checked))
    }
}

#[cfg(test)]
mod tests {
    use platform_storage::TransportVerdict;

    use super::*;

    #[test]
    fn example_configuration_parses() {
        let raw = include_str!("../../../deployments/examples/gateway.example.toml");
        let cfg: GatewayConfig = toml::from_str(raw).unwrap_or_else(|e| panic!("{e}"));
        let ui = cfg
            .http
            .ui
            .unwrap_or_else(|| panic!("example enables the UI"));
        assert!(
            matches!(ui.auth, crate::ui::UiAuth::Oidc { ref client_id, .. } if client_id == "radio-console")
        );
        assert_eq!(cfg.limits.scanner_ports, vec![50536]);
        assert!(
            cfg.transcription.is_none(),
            "transcription is off in the example"
        );
        assert!(cfg.retention.validate().is_ok());
    }

    fn base_config(database: &str) -> GatewayConfig {
        let raw = include_str!("../../../deployments/examples/gateway.example.toml");
        let mut doc: toml::Table = toml::from_str(raw).unwrap_or_else(|e| panic!("{e}"));
        let db: toml::Table = toml::from_str(database).unwrap_or_else(|e| panic!("{e}"));
        doc.insert("database".into(), toml::Value::Table(db));
        doc.try_into().unwrap_or_else(|e| panic!("{e}"))
    }

    fn env_of(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |k| {
            pairs
                .iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| (*v).to_owned())
        }
    }

    #[test]
    fn database_url_env_overrides_file() {
        let cfg = base_config(
            "backend = \"postgres\"\nurl = \"postgres://u@db.example.org/db?sslmode=disable\"",
        );
        let none = env_of(&[]);
        assert!(cfg.check_database_transport(&none).is_err());
        let env = env_of(&[(
            "DATABASE_URL",
            "postgres://u@db.example.org/db?sslmode=verify-full",
        )]);
        assert_eq!(
            cfg.database.effective_url(&env).as_deref(),
            Some("postgres://u@db.example.org/db?sslmode=verify-full")
        );
        match cfg.check_database_transport(&env) {
            Ok(DbTransport::Checked(c)) => {
                assert_eq!(c.verdict(), TransportVerdict::Verified);
                assert_eq!(c.target().host(), "db.example.org");
            }
            other => panic!("{other:?}"),
        }
        let local = env_of(&[("DATABASE_URL", "postgres://u:pw@127.0.0.1/db")]);
        assert!(matches!(
            cfg.check_database_transport(&local),
            Ok(DbTransport::Checked(c)) if c.verdict() == TransportVerdict::Local
        ));
    }

    #[test]
    fn insecure_transport_parses_and_defaults_false() {
        let cfg = base_config("backend = \"postgres\"");
        assert!(matches!(
            cfg.database,
            DatabaseConfig::Postgres {
                insecure_transport: false,
                ..
            }
        ));
        let cfg = base_config("backend = \"postgres\"\ninsecure_transport = true");
        assert!(matches!(
            cfg.database,
            DatabaseConfig::Postgres {
                insecure_transport: true,
                ..
            }
        ));
        let env = env_of(&[(
            "DATABASE_URL",
            "postgres://u:Sup3rS3cret@db.example.org/db?sslmode=require",
        )]);
        assert!(matches!(
            cfg.check_database_transport(&env),
            Ok(DbTransport::Checked(c)) if c.verdict() == TransportVerdict::InsecureAllowed
        ));
        let strict = base_config("backend = \"postgres\"");
        let e = strict
            .check_database_transport(&env)
            .err()
            .unwrap_or_else(|| panic!("refused"));
        assert!(
            e.contains("verify-full") && !e.contains("Sup3rS3cret"),
            "{e}"
        );
    }

    #[test]
    fn check_database_transport_url_not_set() {
        let cfg = base_config("backend = \"postgres\"");
        assert_eq!(
            cfg.check_database_transport(&env_of(&[])),
            Ok(DbTransport::UrlNotSet)
        );
    }

    #[test]
    fn memory_backend_skips_transport_check() {
        let cfg = base_config("backend = \"memory\"");
        let env = env_of(&[("DATABASE_URL", "postgres://u@db.example.org/db")]);
        assert_eq!(cfg.database.effective_url(&env), None);
        assert_eq!(
            cfg.check_database_transport(&env),
            Ok(DbTransport::NotPostgres)
        );
    }

    #[test]
    fn retention_limits() {
        let r: RetentionConfig =
            toml::from_str("recordings_days = 30").unwrap_or_else(|e| panic!("{e}"));
        assert!(r.enabled() && r.validate().is_ok());
        let bad: RetentionConfig =
            toml::from_str("events_days = 0").unwrap_or_else(|e| panic!("{e}"));
        assert!(bad.validate().is_err());
        assert!(!RetentionConfig::default().enabled());
        assert!(
            RetentionConfig::default().validate().is_ok(),
            "no [retention] must still start"
        );
        assert!(
            toml::from_str::<RetentionConfig>("recording_days = 3").is_err(),
            "typos are rejected"
        );
    }

    #[test]
    fn parse_errors_do_not_echo_the_source_line() {
        let src =
            "[database]\nbackend = \"postgres\"\nurl = \"postgres://u:Sup3rS3cret\"x@db/db\"\n";
        let e = parse_toml::<GatewayConfig>(src)
            .err()
            .unwrap_or_else(|| panic!("malformed"));
        assert!(!e.contains("Sup3rS3cret"), "{e}");
        assert!(e.starts_with("line 3, column "), "{e}");
    }
}
