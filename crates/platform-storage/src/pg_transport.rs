//! PostgreSQL transport policy: verified TLS off loopback (ADR 0012).
//!
//! The gateway connects to PostgreSQL with a libpq-style URL. Before sqlx sees
//! that URL, [`PgTarget::parse`] works out where the connection really goes
//! (Unix socket, loopback, or a remote host) and which `sslmode` applies, and
//! [`PgTarget::check`] decides whether that is acceptable:
//!
//! - Unix sockets and loopback (`localhost`, `127.0.0.0/8`, `::1`) are always
//!   allowed.
//! - A remote destination needs `sslmode=verify-full`: certificate chain *and*
//!   host name verification. `verify-ca` is refused because sqlx adds
//!   `sslrootcert` to the public Mozilla roots, so without a name check any
//!   publicly trusted certificate would pass.
//! - Anything else off loopback is refused unless the operator opts out
//!   (`[database] insecure_transport = true`).
//!
//! The parser mirrors `sqlx-postgres` 0.9 (`options/parse.rs` and
//! `PgConnectOptions::new_without_pgpass`), reads `PG*` variables only
//! through an injected closure, and touches no files or network. It never
//! puts the URL, its user information or query values into an error.

use std::fmt;
use std::net::IpAddr;
use std::str::FromStr;

use sqlx::postgres::{PgConnectOptions, PgSslMode};
use url::Url;

/// Longest database URL accepted, in bytes.
pub const MAX_URL_LEN: usize = 8 * 1024;
/// Most query parameters accepted in a database URL.
pub const MAX_QUERY_PAIRS: usize = 32;

/// Where a PostgreSQL connection goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PgDestination {
    /// A Unix-domain socket on this host.
    UnixSocket,
    /// A loopback address on this host (lexical check, no DNS).
    Loopback,
    /// Anything else, including names that might resolve to loopback.
    Remote,
}

/// Outcome of the transport policy for an accepted target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportVerdict {
    /// Unix socket or loopback: no TLS needed.
    Local,
    /// Remote with `sslmode=verify-full`.
    Verified,
    /// Remote without verified TLS, allowed only by the explicit opt-out.
    /// The caller must warn.
    InsecureAllowed,
}

/// Why a database URL was refused. The text never contains the URL, its
/// user information or any query value.
#[derive(Debug, thiserror::Error)]
pub enum PgTransportError {
    /// The URL could not be parsed (`url`'s messages never echo the input).
    #[error("database url is not a valid URL: {0}")]
    InvalidUrl(url::ParseError),
    /// The URL is longer than [`MAX_URL_LEN`].
    #[error("database url is longer than {MAX_URL_LEN} bytes")]
    TooLong,
    /// The URL has more than [`MAX_QUERY_PAIRS`] query parameters.
    #[error("database url has more than {MAX_QUERY_PAIRS} query parameters")]
    TooManyParameters,
    /// The scheme is not `postgres` or `postgresql`.
    #[error("database url scheme must be postgres:// or postgresql://")]
    UnsupportedScheme,
    /// A query parameter that sqlx does not support (and would only log).
    #[error("database url parameter {key:?} is not supported")]
    UnsupportedParameter {
        /// The key if it is a known libpq or sqlx parameter name (a near miss
        /// such as `SSLMODE`), otherwise `<unrecognised>`.
        key: String,
    },
    /// A destination or TLS parameter appears more than once.
    #[error("database url parameter {key:?} is given more than once")]
    DuplicateParameter {
        /// Canonical name of the parameter.
        key: &'static str,
    },
    /// `sslmode` is not a libpq mode.
    #[error(
        "database url sslmode is not one of disable, allow, prefer, require, verify-ca, verify-full"
    )]
    InvalidSslMode,
    /// `hostaddr` is not an IP address.
    #[error("database url hostaddr is not an IP address")]
    InvalidHostaddr,
    /// `port` is not a TCP port number.
    #[error("database url port is not a number from 0 to 65535")]
    InvalidPort,
    /// `sslrootcert=system` (libpq only; sqlx would treat it as a file name).
    #[error(
        "sslrootcert=system is not supported; omit sslrootcert to use the built-in public roots, or name a CA file"
    )]
    SystemRootCertUnsupported,
    /// Remote destination without `sslmode=verify-full` and no opt-out.
    #[error(
        "database {host}:{port} is not on loopback and sslmode={sslmode}; off-loopback PostgreSQL \
         requires sslmode=verify-full (set [database] insecure_transport = true to accept an \
         unverified connection)"
    )]
    UnverifiedOffLoopback {
        /// Host name or address (never user information).
        host: String,
        /// TCP port.
        port: u16,
        /// Effective sslmode.
        sslmode: &'static str,
    },
    /// sqlx would connect somewhere other than the policy checked.
    #[error("database url: sqlx connection options disagree with the transport check")]
    OptionsMismatch,
    /// sqlx rejected the URL (detail dropped because it may quote input).
    #[error("database url rejected by the PostgreSQL driver")]
    Invalid,
}

/// The parsed destination and TLS mode of a database URL. Holds no secrets,
/// so it may be logged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PgTarget {
    destination: PgDestination,
    /// Authority host, query host or socket directory; never user info.
    display_host: String,
    /// TCP address when `hostaddr` (or `PGHOSTADDR`) is set.
    hostaddr: Option<IpAddr>,
    port: u16,
    ssl_mode: SslMode,
    has_root_cert: bool,
    /// The host came from the built-in default (no URL host, no `PGHOST`).
    default_host: bool,
}

/// `PgSslMode` without its non-comparable sqlx type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SslMode {
    Disable,
    Allow,
    Prefer,
    Require,
    VerifyCa,
    VerifyFull,
}

impl From<PgSslMode> for SslMode {
    fn from(m: PgSslMode) -> Self {
        match m {
            PgSslMode::Disable => Self::Disable,
            PgSslMode::Allow => Self::Allow,
            PgSslMode::Prefer => Self::Prefer,
            PgSslMode::Require => Self::Require,
            PgSslMode::VerifyCa => Self::VerifyCa,
            PgSslMode::VerifyFull => Self::VerifyFull,
        }
    }
}

impl SslMode {
    fn name(self) -> &'static str {
        match self {
            Self::Disable => "disable",
            Self::Allow => "allow",
            Self::Prefer => "prefer",
            Self::Require => "require",
            Self::VerifyCa => "verify-ca",
            Self::VerifyFull => "verify-full",
        }
    }
}

/// Query parameters that sqlx 0.9 understands. Others are refused, because
/// sqlx logs an unrecognised parameter together with its value.
fn param_group(key: &str) -> Option<Option<&'static str>> {
    // Some(Some(name)) = may appear once; Some(None) = may repeat.
    Some(match key {
        "sslmode" | "ssl-mode" => Some("sslmode"),
        "sslrootcert" | "ssl-root-cert" | "ssl-ca" => Some("sslrootcert"),
        "sslcert" | "ssl-cert" => Some("sslcert"),
        "sslkey" | "ssl-key" => Some("sslkey"),
        "host" => Some("host"),
        "hostaddr" => Some("hostaddr"),
        "port" => Some("port"),
        "statement-cache-capacity"
        | "dbname"
        | "user"
        | "password"
        | "application_name"
        | "options" => None,
        k if k.starts_with("options[") => None,
        _ => return None,
    })
}

/// libpq and sqlx parameter names that may be echoed in an error, in the
/// normalised form of [`known_key`]. Anything else might be a pasted secret.
const KNOWN_KEYS: &[&str] = &[
    "application_name",
    "channel_binding",
    "client_encoding",
    "connect_timeout",
    "dbname",
    "fallback_application_name",
    "gssencmode",
    "host",
    "hostaddr",
    "keepalives",
    "keepalives_count",
    "keepalives_idle",
    "keepalives_interval",
    "krbsrvname",
    "load_balance_hosts",
    "options",
    "passfile",
    "password",
    "port",
    "replication",
    "require_auth",
    "service",
    "ssl_ca",
    "ssl_max_protocol_version",
    "ssl_min_protocol_version",
    "sslcert",
    "sslcertmode",
    "sslcompression",
    "sslcrl",
    "sslkey",
    "sslmode",
    "sslnegotiation",
    "sslpassword",
    "sslrootcert",
    "statement_cache_capacity",
    "target_session_attrs",
];

/// True if `key` names a known libpq or sqlx parameter, ignoring ASCII case
/// and treating `-` and `_` as equal (so `SSLMODE`, `ssl-mode` and
/// `ssl_root_cert` count).
fn known_key(key: &str) -> bool {
    if key.len() > 32 {
        return false;
    }
    let norm: String = key
        .chars()
        .map(|c| {
            if c == '-' {
                '_'
            } else {
                c.to_ascii_lowercase()
            }
        })
        .collect();
    let squashed = norm.replace('_', "");
    KNOWN_KEYS
        .iter()
        .any(|k| *k == norm || k.replace('_', "") == squashed)
}

/// The key as shown in an error: only a known parameter name is echoed, so a
/// password pasted without `password=` never reaches the logs.
fn printable_key(key: &str) -> String {
    if known_key(key) {
        key.to_owned()
    } else {
        "<unrecognised>".to_owned()
    }
}

/// Percent-decodes `s` as sqlx does for the authority host. `None` if the
/// result is not UTF-8.
fn percent_decode(s: &str) -> Option<String> {
    fn hex(b: u8) -> Option<u8> {
        match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            b'A'..=b'F' => Some(b - b'A' + 10),
            _ => None,
        }
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while let Some(&b) = bytes.get(i) {
        if b == b'%'
            && let (Some(h), Some(l)) = (
                bytes.get(i + 1).copied().and_then(hex),
                bytes.get(i + 2).copied().and_then(hex),
            )
        {
            out.push((h << 4) | l);
            i += 3;
            continue;
        }
        out.push(b);
        i += 1;
    }
    String::from_utf8(out).ok()
}

fn strip_brackets(h: &str) -> &str {
    h.strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(h)
}

fn ip_is_loopback(ip: IpAddr) -> bool {
    // `Ipv6Addr::is_loopback` is `::1` only, so `::ffff:127.0.0.1` is remote.
    ip.is_loopback()
}

/// Lexical loopback check: the literal `localhost` or a loopback IP literal.
/// No DNS, so `localhost.`, `*.localhost` and other names are remote.
fn host_is_loopback(host: &str) -> bool {
    let h = strip_brackets(host);
    h.eq_ignore_ascii_case("localhost") || h.parse::<IpAddr>().is_ok_and(ip_is_loopback)
}

impl PgTarget {
    /// Parses a database URL the way sqlx will, reading `PGHOST`,
    /// `PGHOSTADDR`, `PGPORT`, `PGSSLMODE` and `PGSSLROOTCERT` through `env`.
    /// Reads no files and does no DNS.
    ///
    /// # Errors
    /// A malformed or oversized URL, an unsupported or repeated parameter,
    /// an invalid `sslmode`, `hostaddr` or `port`, or `sslrootcert=system`.
    pub fn parse(
        url: &str,
        env: &dyn Fn(&str) -> Option<String>,
    ) -> Result<Self, PgTransportError> {
        if url.len() > MAX_URL_LEN {
            return Err(PgTransportError::TooLong);
        }
        let parsed = Url::parse(url).map_err(PgTransportError::InvalidUrl)?;
        if !matches!(parsed.scheme(), "postgres" | "postgresql") {
            return Err(PgTransportError::UnsupportedScheme);
        }

        // Defaults from the environment, as in `new_without_pgpass`.
        let mut port: u16 = env("PGPORT").and_then(|v| v.parse().ok()).unwrap_or(5432);
        let env_host = env("PGHOST");
        let mut default_host = env_host.is_none();
        let mut host: String = env_host.unwrap_or_else(|| "localhost".to_owned());
        let mut socket: Option<String> = None;
        let mut hostaddr: Option<IpAddr> = env("PGHOSTADDR").and_then(|v| v.parse().ok());
        let mut ssl_mode: SslMode = env("PGSSLMODE")
            .and_then(|v| PgSslMode::from_str(&v).ok())
            .map_or(SslMode::Prefer, SslMode::from);
        let mut root_cert: Option<String> = env("PGSSLROOTCERT");

        if let Some(h) = parsed.host_str() {
            let decoded = percent_decode(h).ok_or(PgTransportError::Invalid)?;
            if decoded.starts_with('/') {
                socket = Some(decoded);
            } else {
                h.clone_into(&mut host);
                default_host = false;
            }
        }
        if let Some(p) = parsed.port() {
            port = p;
        }

        let mut seen: Vec<&'static str> = Vec::new();
        for (n, (key, value)) in parsed.query_pairs().enumerate() {
            if n >= MAX_QUERY_PAIRS {
                return Err(PgTransportError::TooManyParameters);
            }
            let Some(group) = param_group(&key) else {
                return Err(PgTransportError::UnsupportedParameter {
                    key: printable_key(&key),
                });
            };
            if let Some(name) = group {
                if seen.contains(&name) {
                    return Err(PgTransportError::DuplicateParameter { key: name });
                }
                seen.push(name);
            }
            match group {
                Some("sslmode") => {
                    ssl_mode = PgSslMode::from_str(&value)
                        .map(SslMode::from)
                        .map_err(|_| PgTransportError::InvalidSslMode)?;
                }
                Some("sslrootcert") => root_cert = Some(value.into_owned()),
                Some("host") => {
                    if value.starts_with('/') {
                        socket = Some(value.into_owned());
                    } else {
                        host = value.into_owned();
                        default_host = false;
                    }
                }
                Some("hostaddr") => {
                    hostaddr = Some(
                        value
                            .parse::<IpAddr>()
                            .map_err(|_| PgTransportError::InvalidHostaddr)?,
                    );
                }
                Some("port") => {
                    port = value.parse().map_err(|_| PgTransportError::InvalidPort)?;
                }
                _ => {}
            }
        }

        if root_cert
            .as_deref()
            .is_some_and(|r| r.eq_ignore_ascii_case("system"))
        {
            return Err(PgTransportError::SystemRootCertUnsupported);
        }

        // sqlx (`make_socket_path`): an explicit socket wins, then a host that
        // starts with `/`; otherwise TCP to `hostaddr`, else the host.
        let (destination, display_host) = if let Some(s) = socket {
            (PgDestination::UnixSocket, s)
        } else if host.starts_with('/') {
            (PgDestination::UnixSocket, host)
        } else if let Some(addr) = hostaddr {
            let d = if ip_is_loopback(addr) {
                PgDestination::Loopback
            } else {
                PgDestination::Remote
            };
            (d, host)
        } else if default_host || host.is_empty() || host_is_loopback(&host) {
            // The built-in default is a local socket directory or
            // `localhost`; an empty host cannot leave this machine.
            (PgDestination::Loopback, host)
        } else {
            (PgDestination::Remote, host)
        };

        Ok(Self {
            destination,
            display_host,
            hostaddr,
            port,
            ssl_mode,
            has_root_cert: root_cert.is_some(),
            default_host,
        })
    }

    /// Applies the transport policy. The only way to obtain a
    /// [`CheckedTarget`], which [`crate::PgStore::connect_target`] requires.
    ///
    /// # Errors
    /// [`PgTransportError::UnverifiedOffLoopback`] for a remote destination
    /// without `sslmode=verify-full` when `insecure_transport` is false.
    pub fn check(&self, insecure_transport: bool) -> Result<CheckedTarget, PgTransportError> {
        // NIST 800-53 Rev5: SC-8 (off-loopback database traffic requires verified TLS)
        // NIST 800-53 Rev5: SC-23 (verify-full authenticates the server by name)
        // POAM: sqlx adds sslrootcert to the public webpki roots instead of
        // replacing them; verify-full still checks the name, but any public CA
        // that issues a certificate for the database name is trusted.
        // POAM: no client-certificate authentication to PostgreSQL: the
        // policy does not require sslcert/sslkey and the chart mounts no client
        // key, so the database authenticates the gateway by password only.
        // POAM: the database link accepts TLS 1.2 or 1.3 (sqlx uses rustls
        // safe default protocol versions); TLS 1.3 only cannot be enforced
        // through sqlx 0.9, so set ssl_min_protocol_version on the server.
        let verdict = match (self.destination, self.ssl_mode) {
            (PgDestination::UnixSocket | PgDestination::Loopback, _) => TransportVerdict::Local,
            (PgDestination::Remote, SslMode::VerifyFull) => TransportVerdict::Verified,
            // NIST 800-53 Rev5: CM-7 (the insecure mode is an explicit, off-by-default opt-out)
            // POAM: insecure_transport = true accepts an unverified or plaintext
            // off-loopback database link (credentials, audit records and
            // transcripts exposed on the network); it is warned, not prevented.
            (PgDestination::Remote, _) if insecure_transport => TransportVerdict::InsecureAllowed,
            (PgDestination::Remote, mode) => {
                return Err(PgTransportError::UnverifiedOffLoopback {
                    host: self.describe_host(),
                    port: self.port,
                    sslmode: mode.name(),
                });
            }
        };
        Ok(CheckedTarget {
            target: self.clone(),
            verdict,
        })
    }

    /// Where the connection goes.
    #[must_use]
    pub fn destination(&self) -> PgDestination {
        self.destination
    }

    /// Host name, address or socket directory (no user information).
    #[must_use]
    pub fn host(&self) -> &str {
        &self.display_host
    }

    /// The TCP address from `hostaddr`, if set.
    #[must_use]
    pub fn hostaddr(&self) -> Option<IpAddr> {
        self.hostaddr
    }

    /// TCP port (or the socket file suffix).
    #[must_use]
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Effective libpq `sslmode`.
    #[must_use]
    pub fn ssl_mode(&self) -> &'static str {
        self.ssl_mode.name()
    }

    /// True if `sslrootcert` (or `PGSSLROOTCERT`) adds a CA.
    #[must_use]
    pub fn has_root_cert(&self) -> bool {
        self.has_root_cert
    }

    fn describe_host(&self) -> String {
        match self.hostaddr {
            Some(a) => format!("{} (hostaddr {a})", self.display_host),
            None => self.display_host.clone(),
        }
    }
}

impl fmt::Display for PgTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}:{} sslmode={}",
            self.describe_host(),
            self.port,
            self.ssl_mode()
        )
    }
}

/// A [`PgTarget`] that the transport policy accepted, with the verdict.
///
/// Only [`PgTarget::check`] creates one (the fields are private and there
/// is no other constructor), so holding one proves the policy ran. This is
/// what [`crate::PgStore::connect_target`] takes, so a parsed but unchecked
/// target cannot be connected:
///
/// ```compile_fail,E0308
/// # use platform_storage::{PgStore, PgTarget};
/// async fn bypass(url: &str) {
///     let target = PgTarget::parse(url, &|_| None).unwrap();
///     let _ = PgStore::connect_target(url, &target, 1).await;
/// }
/// ```
///
/// ```compile_fail,E0451
/// # use platform_storage::{CheckedTarget, PgTarget, TransportVerdict};
/// fn forge(target: PgTarget) -> CheckedTarget {
///     CheckedTarget { target, verdict: TransportVerdict::Verified }
/// }
/// ```
///
/// The checked form compiles:
///
/// ```
/// # use platform_storage::{PgStore, PgTarget};
/// async fn connect(url: &str) {
///     let checked = PgTarget::parse(url, &|_| None).unwrap().check(false).unwrap();
///     let _ = PgStore::connect_target(url, &checked, 1).await;
/// }
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckedTarget {
    target: PgTarget,
    verdict: TransportVerdict,
}

impl CheckedTarget {
    /// The accepted destination and TLS mode (safe to log).
    #[must_use]
    pub fn target(&self) -> &PgTarget {
        &self.target
    }

    /// The policy outcome; [`TransportVerdict::InsecureAllowed`] must be
    /// reported as a warning.
    #[must_use]
    pub fn verdict(&self) -> TransportVerdict {
        self.verdict
    }
}

impl fmt::Display for CheckedTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.target.fmt(f)
    }
}

/// Builds sqlx options for `url` and checks that sqlx will connect where
/// `target` says. Re-parses with the process environment, so unknown
/// parameters never reach sqlx (which would log their values).
///
/// The returned options contain the password: never format them.
///
/// # Errors
/// Any [`PgTarget::parse`] error, [`PgTransportError::Invalid`] if sqlx
/// rejects the URL, or [`PgTransportError::OptionsMismatch`] if sqlx's view
/// of the destination or `sslmode` differs from `target`.
pub(crate) fn connect_options(
    url: &str,
    target: &PgTarget,
) -> Result<PgConnectOptions, PgTransportError> {
    let process_env = |k: &str| std::env::var(k).ok();
    connect_options_with(url, target, &process_env)
}

fn connect_options_with(
    url: &str,
    target: &PgTarget,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<PgConnectOptions, PgTransportError> {
    if PgTarget::parse(url, env)? != *target {
        return Err(PgTransportError::OptionsMismatch);
    }
    let opts = PgConnectOptions::from_str(url).map_err(|_| PgTransportError::Invalid)?;
    if !options_agree(&opts, target) {
        return Err(PgTransportError::OptionsMismatch);
    }
    Ok(opts)
}

/// Compares sqlx's effective options with the policy's parse.
fn options_agree(opts: &PgConnectOptions, target: &PgTarget) -> bool {
    if SslMode::from(opts.get_ssl_mode()) != target.ssl_mode || opts.get_port() != target.port {
        return false;
    }
    let sqlx_host = opts.get_host();
    let sqlx_socket = opts.get_socket().is_some() || sqlx_host.starts_with('/');
    match target.destination {
        PgDestination::UnixSocket => sqlx_socket,
        // The default host may become a socket directory (sqlx probes the
        // file system); every outcome of that probe is local.
        _ if target.default_host && target.hostaddr.is_none() => {
            sqlx_socket || sqlx_host.eq_ignore_ascii_case("localhost")
        }
        _ => !sqlx_socket && strip_brackets(sqlx_host) == strip_brackets(&target.display_host),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::collections::HashMap;

    use proptest::prelude::*;

    use super::*;

    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let m: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |k| m.get(k).cloned()
    }

    fn no_env(_: &str) -> Option<String> {
        None
    }

    fn verdict(url: &str, insecure: bool) -> Result<TransportVerdict, PgTransportError> {
        PgTarget::parse(url, &no_env)?
            .check(insecure)
            .map(|c| c.verdict())
    }

    const UNVERIFIED: [&str; 6] = [
        "?sslmode=disable",
        "?sslmode=allow",
        "?sslmode=prefer",
        "?sslmode=require",
        "?sslmode=verify-ca",
        "",
    ];

    #[test]
    fn loopback_plaintext_allowed() {
        for host in [
            "127.0.0.1",
            "127.255.0.1",
            "localhost",
            "LOCALHOST",
            "[::1]",
        ] {
            for q in ["?sslmode=disable", ""] {
                let url = format!("postgres://u:pw@{host}:5432/db{q}");
                let t = PgTarget::parse(&url, &no_env).unwrap();
                assert_eq!(t.destination(), PgDestination::Loopback, "{url}");
                assert_eq!(
                    t.check(false).unwrap().verdict(),
                    TransportVerdict::Local,
                    "{url}"
                );
            }
        }
    }

    #[test]
    fn ipv6_loopback_bracketed_and_query_host() {
        assert_eq!(
            verdict("postgres://u@[::1]/db", false).unwrap(),
            TransportVerdict::Local
        );
        assert_eq!(
            verdict("postgres://u@db.example.org/db?host=::1", false).unwrap(),
            TransportVerdict::Local
        );
        assert!(verdict("postgres://u@[::2]/db", false).is_err());
    }

    #[test]
    fn unix_socket_allowed() {
        let t = PgTarget::parse("postgres://u@%2Fvar%2Frun%2Fpostgresql/db", &no_env).unwrap();
        assert_eq!(t.destination(), PgDestination::UnixSocket);
        assert_eq!(t.host(), "/var/run/postgresql");
        let t = PgTarget::parse(
            "postgres://u@db.example.org/db?host=/run/postgresql",
            &no_env,
        )
        .unwrap();
        assert_eq!(t.destination(), PgDestination::UnixSocket);
        let pghost = env_of(&[("PGHOST", "/tmp")]);
        let t = PgTarget::parse("postgres:db", &pghost).unwrap();
        assert_eq!(t.destination(), PgDestination::UnixSocket);
        assert_eq!(t.check(false).unwrap().verdict(), TransportVerdict::Local);
        for url in ["postgres:///db", "postgres:db", "postgres:///db?host=/tmp"] {
            let t = PgTarget::parse(url, &pghost).unwrap();
            assert_eq!(
                t.check(false).unwrap().verdict(),
                TransportVerdict::Local,
                "{url}"
            );
            let t = PgTarget::parse(url, &no_env).unwrap();
            assert_eq!(
                t.check(false).unwrap().verdict(),
                TransportVerdict::Local,
                "{url}"
            );
        }
    }

    #[test]
    fn remote_verify_full_ok() {
        for host in ["db.example.org", "10.0.0.5", "[2001:db8::1]"] {
            let url = format!("postgresql://u:pw@{host}:5432/db?sslmode=verify-full");
            assert_eq!(verdict(&url, false).unwrap(), TransportVerdict::Verified);
            assert_eq!(verdict(&url, true).unwrap(), TransportVerdict::Verified);
        }
        assert_eq!(
            verdict("postgres://u@db.example.org/db?sslmode=VERIFY-FULL", false).unwrap(),
            TransportVerdict::Verified
        );
    }

    #[test]
    fn remote_unverified_refused() {
        for q in UNVERIFIED {
            let url = format!("postgres://u:pw@db.example.org:6432/db{q}");
            let e = verdict(&url, false).unwrap_err();
            assert!(
                matches!(e, PgTransportError::UnverifiedOffLoopback { .. }),
                "{url}: {e}"
            );
            let msg = e.to_string();
            let mode = q.strip_prefix("?sslmode=").unwrap_or("prefer");
            assert!(msg.contains("db.example.org:6432"), "{msg}");
            assert!(msg.contains(&format!("sslmode={mode}")), "{msg}");
            assert!(msg.contains("verify-full") && msg.contains("insecure_transport"));
        }
    }

    #[test]
    fn opt_out_allows_with_warning_verdict() {
        for q in UNVERIFIED {
            let url = format!("postgres://u@db.example.org/db{q}");
            assert_eq!(
                verdict(&url, true).unwrap(),
                TransportVerdict::InsecureAllowed
            );
        }
    }

    #[test]
    fn hostaddr_decides_destination() {
        let e = verdict("postgres://u@localhost/db?hostaddr=10.0.0.5", false).unwrap_err();
        assert!(e.to_string().contains("hostaddr 10.0.0.5"), "{e}");
        assert_eq!(
            verdict("postgres://u@db.example.org/db?hostaddr=127.0.0.1", false).unwrap(),
            TransportVerdict::Local
        );
        let env = env_of(&[("PGHOSTADDR", "10.0.0.5")]);
        let t = PgTarget::parse("postgres://u@localhost/db", &env).unwrap();
        assert_eq!(t.destination(), PgDestination::Remote);
        assert!(t.check(false).is_err());
        // An unparsable PGHOSTADDR is ignored, as by sqlx.
        let env = env_of(&[("PGHOSTADDR", "nope")]);
        let t = PgTarget::parse("postgres://u@localhost/db", &env).unwrap();
        assert_eq!(t.destination(), PgDestination::Loopback);
        assert!(matches!(
            PgTarget::parse("postgres://u@localhost/db?hostaddr=nope", &no_env),
            Err(PgTransportError::InvalidHostaddr)
        ));
    }

    #[test]
    fn env_sslmode_applies() {
        let env = env_of(&[("PGSSLMODE", "verify-full")]);
        let t = PgTarget::parse("postgres://u@db.example.org/db", &env).unwrap();
        assert_eq!(
            t.check(false).unwrap().verdict(),
            TransportVerdict::Verified
        );
        let t = PgTarget::parse("postgres://u@db.example.org/db?sslmode=require", &env).unwrap();
        assert!(t.check(false).is_err(), "query sslmode wins over PGSSLMODE");
        let env = env_of(&[("PGSSLMODE", "bogus")]);
        let t = PgTarget::parse("postgres://u@db.example.org/db", &env).unwrap();
        assert_eq!(t.ssl_mode(), "prefer");
        let env = env_of(&[("PGHOST", "db.example.org"), ("PGPORT", "6543")]);
        let t = PgTarget::parse("postgres:db", &env).unwrap();
        assert_eq!((t.destination(), t.port()), (PgDestination::Remote, 6543));
    }

    #[test]
    fn not_loopback_edge_cases() {
        for host in [
            "[::ffff:127.0.0.1]",
            "localhost.",
            "foo.localhost",
            "127.0.0.1.nip.io",
            "127.1",
            "0x7f000001",
        ] {
            let url = format!("postgres://u@{host}/db");
            let t = PgTarget::parse(&url, &no_env).unwrap();
            assert_eq!(t.destination(), PgDestination::Remote, "{url}");
        }
    }

    #[test]
    fn rejects_unknown_and_duplicate_params() {
        let e = PgTarget::parse("postgres://u@localhost/db?sslmod=disable", &no_env).unwrap_err();
        assert!(
            matches!(e, PgTransportError::UnsupportedParameter { ref key } if key == "<unrecognised>")
        );
        let e = PgTarget::parse("postgres://u@localhost/db?passwd=hunter2", &no_env).unwrap_err();
        assert!(!e.to_string().contains("hunter2") && !format!("{e:?}").contains("hunter2"));
        let e = PgTarget::parse("postgres://u@localhost/db?hunter2%20x=1", &no_env).unwrap_err();
        assert!(!e.to_string().contains("hunter2"), "{e}");
        for q in [
            "sslmode=verify-full&sslmode=disable",
            "sslmode=verify-full&ssl-mode=disable",
            "host=a&host=b",
            "sslrootcert=/a&ssl-ca=/b",
            "hostaddr=10.0.0.1&hostaddr=10.0.0.2",
            "port=1&port=2",
        ] {
            let url = format!("postgres://u@db.example.org/db?{q}");
            assert!(
                matches!(
                    PgTarget::parse(&url, &no_env),
                    Err(PgTransportError::DuplicateParameter { .. })
                ),
                "{q}"
            );
        }
        assert!(matches!(
            PgTarget::parse("postgres://u@h/db?sslmode=hunter2", &no_env),
            Err(PgTransportError::InvalidSslMode)
        ));
        let ok = "postgres://u@localhost/db?application_name=gw&options[search_path]=x&options=-c%20a%3Db&statement-cache-capacity=50";
        assert!(PgTarget::parse(ok, &no_env).is_ok());
        assert!(matches!(
            PgTarget::parse("mysql://u@localhost/db", &no_env),
            Err(PgTransportError::UnsupportedScheme)
        ));
        let long = format!("postgres://u@localhost/{}", "a".repeat(MAX_URL_LEN));
        assert!(matches!(
            PgTarget::parse(&long, &no_env),
            Err(PgTransportError::TooLong)
        ));
        let many = format!("postgres://u@localhost/db?{}", "options=a&".repeat(33));
        assert!(matches!(
            PgTarget::parse(&many, &no_env),
            Err(PgTransportError::TooManyParameters)
        ));
    }

    #[test]
    fn sslrootcert_system_rejected() {
        assert!(matches!(
            PgTarget::parse(
                "postgres://u@db.example.org/db?sslmode=verify-full&sslrootcert=system",
                &no_env
            ),
            Err(PgTransportError::SystemRootCertUnsupported)
        ));
        let env = env_of(&[("PGSSLROOTCERT", "system")]);
        assert!(matches!(
            PgTarget::parse("postgres://u@db.example.org/db?sslmode=verify-full", &env),
            Err(PgTransportError::SystemRootCertUnsupported)
        ));
        let t = PgTarget::parse(
            "postgres://u@db.example.org/db?sslmode=verify-full&sslrootcert=/etc/ca.crt",
            &no_env,
        )
        .unwrap();
        assert!(t.has_root_cert());
    }

    #[test]
    fn connect_options_cross_check() {
        for url in [
            "postgres://u:pw@db.example.org:6432/db?sslmode=verify-full",
            "postgres://u:pw@127.0.0.1/db?sslmode=disable",
            "postgres://u@[::1]:5433/db",
            "postgres://u@%2Fvar%2Frun%2Fpostgresql/db",
            "postgres://u@db.example.org/db?host=/run/postgresql",
            "postgres://u@localhost/db?hostaddr=10.0.0.5&sslmode=verify-full",
            "postgres://u@db%2Fx.example.org/db?sslmode=verify-full",
            "postgres:///db",
            "postgres:db",
        ] {
            let target = PgTarget::parse(url, &no_env).unwrap();
            let opts = connect_options_with(url, &target, &no_env)
                .unwrap_or_else(|e| panic!("{url}: {e}"));
            assert!(options_agree(&opts, &target), "{url}");
        }
        // A target parsed from a different URL, or a different environment,
        // is refused.
        let a = PgTarget::parse("postgres://u@localhost/db", &no_env).unwrap();
        assert!(matches!(
            connect_options_with("postgres://u@db.example.org/db", &a, &no_env),
            Err(PgTransportError::OptionsMismatch)
        ));
        let env = env_of(&[("PGSSLMODE", "verify-full")]);
        let b = PgTarget::parse("postgres://u@db.example.org/db", &env).unwrap();
        assert!(matches!(
            connect_options_with("postgres://u@db.example.org/db", &b, &no_env),
            Err(PgTransportError::OptionsMismatch)
        ));
        // sqlx's own view disagreeing with the parse is caught too.
        let mut c = PgTarget::parse("postgres://u@db.example.org/db", &no_env).unwrap();
        let opts = PgConnectOptions::from_str("postgres://u@db.example.org/db").unwrap();
        "elsewhere.example.org".clone_into(&mut c.display_host);
        assert!(!options_agree(&opts, &c));
    }

    // ---- QA additions (run 20261126-125150-pg-tls) ----

    fn dest(url: &str, env: &dyn Fn(&str) -> Option<String>) -> PgDestination {
        PgTarget::parse(url, env)
            .unwrap_or_else(|e| panic!("{url}: {e}"))
            .destination()
    }

    /// Every accepted URL must also agree with sqlx's own parse, so the
    /// policy looked where sqlx will connect.
    fn assert_sqlx_agrees(url: &str) {
        let t = PgTarget::parse(url, &no_env).unwrap_or_else(|e| panic!("{url}: {e}"));
        connect_options_with(url, &t, &no_env).unwrap_or_else(|e| panic!("{url}: {e}"));
    }

    #[test]
    fn qa_ipv6_loopback_forms() {
        for url in [
            "postgres://u@[0:0:0:0:0:0:0:1]/db",
            "postgres://u@[::1]:6543/db?sslmode=disable",
            "postgres://u@db.example.org/db?host=[::1]",
            "postgres://u@db.example.org/db?hostaddr=::1",
            "postgres://u@[::1]/db?sslmode=verify-ca",
        ] {
            assert_eq!(dest(url, &no_env), PgDestination::Loopback, "{url}");
            assert_eq!(verdict(url, false).unwrap(), TransportVerdict::Local);
            assert_sqlx_agrees(url);
        }
        // Not loopback, so these fail closed.
        for url in [
            "postgres://u@[::]/db",
            "postgres://u@[fe80::1]/db",
            "postgres://u@[::ffff:7f00:1]/db",
            "postgres://u@localhost/db?hostaddr=::ffff:127.0.0.1",
            "postgres://u@[::1]/db?hostaddr=2001:db8::1",
        ] {
            assert_eq!(dest(url, &no_env), PgDestination::Remote, "{url}");
            assert!(
                matches!(
                    verdict(url, false),
                    Err(PgTransportError::UnverifiedOffLoopback { .. })
                ),
                "{url}"
            );
        }
    }

    #[test]
    fn qa_hostaddr_overrides_host() {
        // Remote hostaddr with verify-full is accepted (the name is still
        // verified against the certificate).
        let url = "postgres://u@db.example.org/db?hostaddr=10.0.0.5&sslmode=verify-full";
        assert_eq!(verdict(url, false).unwrap(), TransportVerdict::Verified);
        assert_sqlx_agrees(url);
        // IPv6 remote hostaddr behind a loopback-looking name is refused, and
        // the error names the real destination.
        let e = verdict("postgres://u@127.0.0.1/db?hostaddr=2001:db8::1", false).unwrap_err();
        assert!(e.to_string().contains("hostaddr 2001:db8::1"), "{e}");
        // The query hostaddr wins over PGHOSTADDR, in both directions.
        let env = env_of(&[("PGHOSTADDR", "127.0.0.1")]);
        assert_eq!(
            dest("postgres://u@localhost/db?hostaddr=10.0.0.5", &env),
            PgDestination::Remote
        );
        let env = env_of(&[("PGHOSTADDR", "10.0.0.5")]);
        assert_eq!(
            dest("postgres://u@db.example.org/db?hostaddr=127.0.0.1", &env),
            PgDestination::Loopback
        );
        // PGHOSTADDR applies to a remote-looking host too.
        let env = env_of(&[("PGHOSTADDR", "127.0.0.1")]);
        assert_eq!(
            dest("postgres://u@db.example.org/db", &env),
            PgDestination::Loopback
        );
        // A socket wins over hostaddr, as in sqlx `make_socket_path`.
        for url in [
            "postgres://u@db.example.org/db?host=/run/postgresql&hostaddr=10.0.0.5",
            "postgres://u@%2Frun%2Fpostgresql/db?hostaddr=10.0.0.5",
            // Sticky socket: a later TCP host does not undo it.
            "postgres://u@%2Frun%2Fpostgresql/db?host=db.example.org",
        ] {
            assert_eq!(dest(url, &no_env), PgDestination::UnixSocket, "{url}");
            assert_sqlx_agrees(url);
        }
        // The query host overrides the authority host.
        assert_eq!(
            dest("postgres://u@localhost/db?host=db.example.org", &no_env),
            PgDestination::Remote
        );
        assert_eq!(
            dest("postgres://u@db.example.org/db?host=localhost", &no_env),
            PgDestination::Loopback
        );
        assert_sqlx_agrees("postgres://u@localhost/db?host=db.example.org&sslmode=verify-full");
    }

    #[test]
    fn qa_pg_environment_variables() {
        // PGHOST names a remote server when the URL has no host.
        let env = env_of(&[("PGHOST", "db.example.org")]);
        assert!(matches!(
            PgTarget::parse("postgres:db", &env).unwrap().check(false),
            Err(PgTransportError::UnverifiedOffLoopback { .. })
        ));
        let env = env_of(&[("PGHOST", "db.example.org"), ("PGSSLMODE", "verify-full")]);
        assert_eq!(
            PgTarget::parse("postgres:db", &env)
                .unwrap()
                .check(false)
                .unwrap()
                .verdict(),
            TransportVerdict::Verified
        );
        // A URL host overrides PGHOST, in both directions.
        let env = env_of(&[("PGHOST", "db.example.org")]);
        assert_eq!(
            dest("postgres://u@localhost/db", &env),
            PgDestination::Loopback
        );
        // `postgres:///db` has no host for `url` (nor for sqlx, which uses the
        // same parser), so PGHOST still applies.
        assert_eq!(dest("postgres:///db", &env), PgDestination::Remote);
        assert_eq!(
            dest("postgres:///db?host=db.example.org", &no_env),
            PgDestination::Remote
        );
        let env = env_of(&[("PGHOST", "localhost")]);
        assert_eq!(
            dest("postgres://u@db.example.org/db", &env),
            PgDestination::Remote
        );
        // A libpq-style host list is not loopback.
        let env = env_of(&[("PGHOST", "localhost,db.example.org")]);
        assert_eq!(dest("postgres:db", &env), PgDestination::Remote);
        // PGSSLMODE applies off loopback; an unverified one is refused.
        for mode in ["disable", "allow", "prefer", "require", "verify-ca"] {
            let pairs = [("PGSSLMODE", mode)];
            let env = env_of(&pairs);
            let t = PgTarget::parse("postgres://u@db.example.org/db", &env).unwrap();
            assert_eq!(t.ssl_mode(), mode);
            assert!(t.check(false).is_err(), "{mode}");
            assert_eq!(
                t.check(true).unwrap().verdict(),
                TransportVerdict::InsecureAllowed
            );
        }
        // A query sslmode wins over PGSSLMODE in the safe direction too.
        let env = env_of(&[("PGSSLMODE", "disable")]);
        let t =
            PgTarget::parse("postgres://u@db.example.org/db?sslmode=verify-full", &env).unwrap();
        assert_eq!(
            t.check(false).unwrap().verdict(),
            TransportVerdict::Verified
        );
        // PGSSLROOTCERT and PGPORT.
        let env = env_of(&[("PGSSLROOTCERT", "/etc/ca.crt"), ("PGPORT", "bogus")]);
        let t = PgTarget::parse("postgres://u@db.example.org/db", &env).unwrap();
        assert!(t.has_root_cert());
        assert_eq!(t.port(), 5432, "an unparsable PGPORT is ignored");
        // URL authority port, then query port, override PGPORT.
        let env = env_of(&[("PGPORT", "7000")]);
        assert_eq!(
            PgTarget::parse("postgres://u@h:6000/db", &env)
                .unwrap()
                .port(),
            6000
        );
        assert_eq!(
            PgTarget::parse("postgres://u@h:6000/db?port=6001", &env)
                .unwrap()
                .port(),
            6001
        );
        // The environment the policy saw must be the one sqlx sees.
        let env = env_of(&[("PGHOSTADDR", "10.0.0.5")]);
        let t = PgTarget::parse("postgres://u@localhost/db", &env).unwrap();
        assert!(matches!(
            connect_options_with("postgres://u@localhost/db", &t, &no_env),
            Err(PgTransportError::OptionsMismatch)
        ));
    }

    #[test]
    fn qa_unix_socket_ignores_sslmode() {
        for q in UNVERIFIED {
            for base in [
                "postgres://u@%2Frun%2Fpostgresql/db",
                "postgres://u@db.example.org/db?host=/run/postgresql",
            ] {
                let sep = if base.contains('?') { "&" } else { "?" };
                let url = format!("{base}{}", q.replacen('?', sep, 1));
                assert_eq!(dest(&url, &no_env), PgDestination::UnixSocket, "{url}");
                assert_eq!(verdict(&url, false).unwrap(), TransportVerdict::Local);
            }
        }
        // PGHOST socket, overridden by a remote URL host, is remote.
        let env = env_of(&[("PGHOST", "/run/postgresql")]);
        assert_eq!(
            dest("postgres://u@db.example.org/db", &env),
            PgDestination::Remote
        );
    }

    #[test]
    fn qa_unknown_parameters_fail_closed() {
        // sqlx matches keys case-sensitively and would log the value of any
        // other key, so all of these are refused. Known libpq/sqlx names
        // (ignoring case, `-` = `_`) are echoed to help the operator.
        for (q, shown) in [
            ("SSLMODE=disable", "SSLMODE"),
            ("sslMode=disable", "sslMode"),
            ("SSL_MODE=disable", "SSL_MODE"),
            ("ssl_root_cert=x", "ssl_root_cert"),
            ("Ssl-Ca=x", "Ssl-Ca"),
            ("sslnegotiation=direct", "sslnegotiation"),
            ("connect_timeout=5", "connect_timeout"),
            ("connect-timeout=5", "connect-timeout"),
            ("target_session_attrs=read-write", "target_session_attrs"),
            ("PASSFILE=x", "PASSFILE"),
        ] {
            let url = format!("postgres://u:Sup3rS3cret@localhost/db?{q}");
            let e = PgTarget::parse(&url, &no_env).unwrap_err();
            assert!(
                matches!(e, PgTransportError::UnsupportedParameter { ref key } if key == shown),
                "{q}: {e}"
            );
            assert!(!e.to_string().contains("Sup3rS3cret"), "{e}");
            assert!(!format!("{e:?}").contains("Sup3rS3cret"), "{e:?}");
        }
        // Anything else is never echoed: it may be a password pasted
        // without `password=`, including identifier-shaped ones.
        for q in [
            "Xk3pQ9vT2mLw".to_owned(),
            "Sup3rS3cret=1".to_owned(),
            "pass=Sup3rS3cret".to_owned(),
            "sslmod=disable".to_owned(),
            format!("{}=1", "k".repeat(65)),
            "=Sup3rS3cret".to_owned(),
            "p%C3%A4ss=1".to_owned(),
            "a.b=1".to_owned(),
            "ssl..mode=1".to_owned(),
            format!("{}sslmode=1", "_".repeat(40)),
        ] {
            let url = format!("postgres://u@localhost/db?{q}");
            let e = PgTarget::parse(&url, &no_env).unwrap_err();
            assert!(
                matches!(e, PgTransportError::UnsupportedParameter { ref key } if key == "<unrecognised>"),
                "{q}: {e}"
            );
            assert!(!e.to_string().contains("Xk3pQ9vT2mLw"), "{e}");
            assert!(!e.to_string().contains("Sup3rS3cret"), "{e}");
        }
    }

    #[test]
    fn qa_duplicate_parameters_all_aliases() {
        for q in [
            "sslmode=verify-full&sslmode=verify-full",
            "ssl-mode=verify-full&sslmode=verify-full",
            "sslrootcert=/a&ssl-root-cert=/b",
            "ssl-ca=/a&ssl-root-cert=/b",
            "sslcert=/a&ssl-cert=/b",
            "sslkey=/a&ssl-key=/b",
            "host=db.example.org&host=localhost",
            "hostaddr=127.0.0.1&hostaddr=10.0.0.5",
            "port=5432&port=5432",
        ] {
            let url = format!("postgres://u:Sup3rS3cret@db.example.org/db?{q}");
            let e = PgTarget::parse(&url, &no_env).unwrap_err();
            assert!(
                matches!(e, PgTransportError::DuplicateParameter { .. }),
                "{q}: {e}"
            );
            assert!(!e.to_string().contains("Sup3rS3cret"), "{e}");
        }
        // Non-destination keys may repeat, and host with hostaddr is fine.
        for q in [
            "options=-c%20a%3Db&options=-c%20c%3Dd",
            "application_name=a&application_name=b",
            "host=localhost&hostaddr=127.0.0.1",
        ] {
            let url = format!("postgres://u@localhost/db?{q}");
            assert!(PgTarget::parse(&url, &no_env).is_ok(), "{q}");
        }
    }

    #[test]
    fn qa_verify_ca_refused_off_loopback() {
        for url in [
            "postgres://u@db.example.org/db?sslmode=verify-ca",
            "postgres://u@db.example.org/db?sslmode=verify-ca&sslrootcert=/etc/ca.crt",
            "postgres://u@db.example.org/db?ssl-mode=verify-ca&ssl-ca=/etc/ca.crt",
            "postgres://u@localhost/db?sslmode=verify-ca&hostaddr=10.0.0.5",
            "postgres://u@10.0.0.5/db?sslmode=VERIFY-CA",
        ] {
            let e = verdict(url, false).unwrap_err();
            assert!(
                matches!(
                    e,
                    PgTransportError::UnverifiedOffLoopback {
                        sslmode: "verify-ca",
                        ..
                    }
                ),
                "{url}: {e}"
            );
            assert_eq!(
                verdict(url, true).unwrap(),
                TransportVerdict::InsecureAllowed
            );
        }
        let env = env_of(&[("PGSSLMODE", "verify-ca"), ("PGSSLROOTCERT", "/etc/ca.crt")]);
        let t = PgTarget::parse("postgres://u@db.example.org/db", &env).unwrap();
        assert!(t.check(false).is_err());
        // On loopback verify-ca is fine.
        assert_eq!(
            verdict("postgres://u@127.0.0.1/db?sslmode=verify-ca", false).unwrap(),
            TransportVerdict::Local
        );
    }

    #[test]
    fn qa_secrets_absent_from_every_error_variant() {
        let pw = "Sup3rS3cret-sentinel";
        let urls = [
            format!("postgres://u:{pw}@[::1/db"),
            format!(
                "postgres://u:{pw}@db.example.org/{}",
                "a".repeat(MAX_URL_LEN)
            ),
            format!(
                "postgres://u:{pw}@db.example.org/db?{}",
                "options=a&".repeat(MAX_QUERY_PAIRS + 1)
            ),
            format!("postgres://u:{pw}@db.example.org/db?sslmode=verify-full&ssl-mode=disable"),
            format!("postgres://u@db.example.org/db?password={pw}&sslmode=require"),
            format!("postgres://u@db.example.org/db?sslmode={pw}"),
            format!("postgres://u@db.example.org/db?hostaddr={pw}"),
            format!("postgres://u@db.example.org/db?port={pw}"),
            format!("postgres://u@db.example.org/db?sslrootcert=SYSTEM&password={pw}"),
            format!("https://u:{pw}@db.example.org/db"),
            format!("postgres://u:{pw}%40x@db.example.org/db?sslmode=allow"),
        ];
        for url in &urls {
            let e = match PgTarget::parse(url, &no_env).and_then(|t| t.check(false)) {
                Err(e) => e,
                Ok(v) => panic!("{url} accepted: {v:?}"),
            };
            assert!(!e.to_string().contains(pw), "{e}");
            assert!(!format!("{e:?}").contains(pw), "{e:?}");
        }
        // The mismatch and driver errors carry nothing either.
        let t = PgTarget::parse("postgres://u@localhost/db", &no_env).unwrap();
        let e = connect_options_with(&format!("postgres://u:{pw}@10.0.0.5/db"), &t, &no_env)
            .unwrap_err();
        assert!(!format!("{e} {e:?}").contains(pw));
    }

    fn all_error_urls(pw: &str, enc: &str) -> Vec<String> {
        vec![
            format!("postgres://u:{pw}@db.example.org/db?sslmode=require"),
            format!("postgres://u:{pw}@db.example.org/db"),
            format!("postgres://u@db.example.org/db?password={enc}&sslmode=disable"),
            format!("postgres://u:{pw}@db.example.org/db?sslmode=bogus"),
            format!("postgres://u:{pw}@db.example.org/db?hostaddr=bogus"),
            format!("postgres://u:{pw}@db.example.org/db?port=bogus"),
            format!("postgres://u:{pw}@db.example.org/db?sslmod=x"),
            format!("postgres://u:{pw}@db.example.org/db?sslrootcert=system"),
            format!("postgres://u:{pw}@db.example.org/db?host=a&host=b"),
            format!("mysql://u:{pw}@db.example.org/db"),
            format!("postgres://u:{pw}@db.example.org:99999/db"),
            format!("postgres://u@db.example.org/db?password={enc}&x=1"),
        ]
    }

    proptest! {
        #[test]
        fn secrets_never_in_errors(pw in "[A-Za-z0-9!$&'()*+,;=._~-]{8,32}") {
            let enc: String = url::form_urlencoded::byte_serialize(pw.as_bytes()).collect();
            for url in all_error_urls(&pw, &enc) {
                let e = match PgTarget::parse(&url, &no_env).and_then(|t| t.check(false)) {
                    Err(e) => e,
                    Ok(v) => panic!("{url} unexpectedly accepted: {v:?}"),
                };
                prop_assert!(!e.to_string().contains(&pw), "{}", e);
                prop_assert!(!format!("{e:?}").contains(&pw), "{:?}", e);
                prop_assert!(!e.to_string().contains(&enc));
            }
            // The target itself holds no secret.
            let t = PgTarget::parse(
                &format!("postgres://u:{pw}@db.example.org/db?sslmode=verify-full"),
                &no_env,
            ).unwrap();
            let shown = format!("{t:?} {t}");
            prop_assert!(!shown.contains(&pw));
        }
    }
}
