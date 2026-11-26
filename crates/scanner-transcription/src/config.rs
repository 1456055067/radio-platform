//! Transcription service configuration (the gateway's `[transcription]`).

use std::net::IpAddr;
use std::path::PathBuf;

use serde::Deserialize;
use thiserror::Error;

/// Request style of the service.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiStyle {
    /// `POST {endpoint}/v1/audio/transcriptions` with `file`, `model`,
    /// `response_format=json`, and optional `language` and `prompt`
    /// (OpenAI and compatible servers).
    #[default]
    Openai,
    /// `POST {endpoint}/inference` with `file`, `response_format=json`,
    /// `temperature` and optional `language` and `prompt` (whisper.cpp's
    /// `whisper-server`).
    WhisperCpp,
}

/// `[transcription]`. Absent means transcription is off.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranscriptionConfig {
    /// Base URL of the service, without the request path (for example
    /// `http://127.0.0.1:8080`). HTTPS is required unless the host is a
    /// loopback address or `allow_http` is set.
    pub endpoint: String,
    /// Request style.
    #[serde(default)]
    pub api: ApiStyle,
    /// Model name sent with `openai`-style requests.
    #[serde(default = "default_model")]
    pub model: String,
    /// ISO 639-1 language hint (for example `en`). Unset lets the service
    /// detect it.
    pub language: Option<String>,
    /// Vocabulary hint (unit names, street names, ten-codes).
    pub prompt: Option<String>,
    /// Environment variable holding the API key, sent as a bearer token.
    /// Keys are never read from the configuration file.
    pub api_key_env: Option<String>,
    /// Extra CA certificate (PEM) for a privately issued endpoint
    /// certificate.
    pub ca_file: Option<PathBuf>,
    /// Allow plain HTTP to a non-loopback endpoint. Audio then crosses
    /// the network unencrypted; use only on an isolated network.
    #[serde(default)]
    pub allow_http: bool,
    /// Per-request timeout in seconds.
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
    /// Concurrent requests per gateway replica.
    #[serde(default = "default_concurrency")]
    pub concurrency: usize,
    /// Attempts per recording before giving up (transient failures only).
    #[serde(default = "default_attempts")]
    pub max_attempts: u32,
    /// Recordings shorter than this are skipped, not sent (short clips
    /// are mostly squelch noise and invite invented text).
    #[serde(default = "default_min")]
    pub min_duration_ms: u64,
}

fn default_model() -> String {
    "whisper-1".into()
}
fn default_timeout() -> u64 {
    120
}
fn default_concurrency() -> usize {
    2
}
fn default_attempts() -> u32 {
    4
}
fn default_min() -> u64 {
    600
}

/// Invalid configuration.
#[derive(Debug, Error, PartialEq, Eq)]
#[error("transcription: {0}")]
pub struct ConfigError(pub String);

/// True for `localhost` and loopback IP addresses.
#[must_use]
pub fn is_loopback_host(host: &str) -> bool {
    let h = host.trim_start_matches('[').trim_end_matches(']');
    h.eq_ignore_ascii_case("localhost") || h.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

impl TranscriptionConfig {
    /// Checks the settings.
    ///
    /// # Errors
    /// [`ConfigError`].
    pub fn validate(&self) -> Result<reqwest::Url, ConfigError> {
        let url = reqwest::Url::parse(&self.endpoint)
            .map_err(|e| ConfigError(format!("endpoint: {e}")))?;
        let host = url.host_str().unwrap_or_default().to_owned();
        match url.scheme() {
            "https" => {}
            "http" if is_loopback_host(&host) || self.allow_http => {}
            "http" => {
                return Err(ConfigError(
                    "endpoint must use https unless it is a loopback address (or allow_http = true)"
                        .into(),
                ));
            }
            s => return Err(ConfigError(format!("unsupported endpoint scheme {s}"))),
        }
        if host.is_empty() || !url.username().is_empty() || url.password().is_some() {
            return Err(ConfigError(
                "endpoint must name a host and must not embed credentials (use api_key_env)".into(),
            ));
        }
        if url.query().is_some() || url.fragment().is_some() {
            return Err(ConfigError(
                "endpoint must not have a query or fragment".into(),
            ));
        }
        if let Some(l) = &self.language
            && (l.len() < 2
                || l.len() > 8
                || !l.chars().all(|c| c.is_ascii_alphabetic() || c == '-'))
        {
            return Err(ConfigError(
                "language must be a language code such as `en`".into(),
            ));
        }
        if self
            .prompt
            .as_ref()
            .is_some_and(|p| p.chars().count() > 1000)
        {
            return Err(ConfigError("prompt is limited to 1000 characters".into()));
        }
        if self.model.is_empty() || self.model.len() > 128 {
            return Err(ConfigError("model must be 1-128 characters".into()));
        }
        if !(1..=32).contains(&self.concurrency) {
            return Err(ConfigError("concurrency must be 1-32".into()));
        }
        if !(1..=10).contains(&self.max_attempts) {
            return Err(ConfigError("max_attempts must be 1-10".into()));
        }
        if !(5..=1800).contains(&self.timeout_secs) {
            return Err(ConfigError("timeout_secs must be 5-1800".into()));
        }
        Ok(url)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(endpoint: &str) -> TranscriptionConfig {
        toml::from_str(&format!("endpoint = \"{endpoint}\"")).unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn https_required_off_loopback() {
        assert!(cfg("http://127.0.0.1:8080").validate().is_ok());
        assert!(cfg("http://[::1]:8080").validate().is_ok());
        assert!(cfg("http://localhost:8080").validate().is_ok());
        assert!(cfg("https://asr.example.org").validate().is_ok());
        assert!(cfg("http://10.0.0.5:8080").validate().is_err());
        let mut c = cfg("http://10.0.0.5:8080");
        c.allow_http = true;
        assert!(c.validate().is_ok());
        assert!(cfg("ftp://x").validate().is_err());
        assert!(cfg("https://user:pw@asr.example.org").validate().is_err());
        assert!(cfg("https://asr.example.org/?key=1").validate().is_err());
    }

    #[test]
    fn defaults_and_limits() {
        let c = cfg("http://127.0.0.1:1");
        assert_eq!(c.api, ApiStyle::Openai);
        assert_eq!(
            (c.concurrency, c.max_attempts, c.min_duration_ms),
            (2, 4, 600)
        );
        let mut c2 = c.clone();
        c2.language = Some("english!".into());
        assert!(c2.validate().is_err());
        let mut c3 = c;
        c3.concurrency = 0;
        assert!(c3.validate().is_err());
        assert!(
            toml::from_str::<TranscriptionConfig>("endpoint = \"x\"\nunknown = 1").is_err(),
            "unknown keys are rejected"
        );
    }
}
