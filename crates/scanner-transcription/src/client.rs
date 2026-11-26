//! HTTP client for Whisper-compatible services.

use std::time::Duration;

use reqwest::StatusCode;
use reqwest::multipart::{Form, Part};
use serde::Deserialize;
use thiserror::Error;

use crate::config::{ApiStyle, ConfigError, TranscriptionConfig};

/// Longest transcript kept, in characters.
pub const MAX_TEXT: usize = 16_384;

/// Largest response body read.
const MAX_RESPONSE: usize = 1024 * 1024;

/// A transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transcript {
    /// Cleaned text (may be empty).
    pub text: String,
    /// Language, if the service reported one.
    pub language: Option<String>,
}

/// A failed request.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum TranscribeError {
    /// Worth retrying later: network failure, timeout, 408, 429, 5xx.
    #[error("transient: {0}")]
    Transient(String),
    /// Retrying will not help: other 4xx, unreadable response.
    #[error("{0}")]
    Permanent(String),
}

impl TranscribeError {
    /// True for [`TranscribeError::Transient`].
    #[must_use]
    pub fn is_transient(&self) -> bool {
        matches!(self, Self::Transient(_))
    }
}

/// Removes control characters, collapses whitespace and caps the length.
#[must_use]
pub fn clean_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len().min(MAX_TEXT));
    for word in s.split_whitespace() {
        let w: String = word.chars().filter(|c| !c.is_control()).collect();
        if w.is_empty() {
            continue;
        }
        if out.chars().count() + w.chars().count() + 1 > MAX_TEXT {
            break;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&w);
    }
    out
}

#[derive(Deserialize)]
struct Response {
    text: String,
    #[serde(default)]
    language: Option<String>,
}

/// A configured client.
#[derive(Debug, Clone)]
pub struct WhisperClient {
    http: reqwest::Client,
    url: reqwest::Url,
    style: ApiStyle,
    model: String,
    language: Option<String>,
    prompt: Option<String>,
    key: Option<String>,
}

impl WhisperClient {
    /// Builds a client: validates the settings, reads the API key from its
    /// environment variable, and loads any extra CA. Redirects are never
    /// followed, so audio cannot be sent anywhere but the endpoint.
    ///
    /// # Errors
    /// [`ConfigError`].
    pub fn new(cfg: &TranscriptionConfig) -> Result<Self, ConfigError> {
        let base = cfg.validate()?;
        let path = match cfg.api {
            ApiStyle::Openai => "v1/audio/transcriptions",
            ApiStyle::WhisperCpp => "inference",
        };
        let mut base_s = base.to_string();
        if !base_s.ends_with('/') {
            base_s.push('/');
        }
        let url = reqwest::Url::parse(&base_s)
            .and_then(|b| b.join(path))
            .map_err(|e| ConfigError(format!("endpoint: {e}")))?;
        let key = match &cfg.api_key_env {
            Some(var) => Some(std::env::var(var).map_err(|_| {
                ConfigError(format!(
                    "api_key_env: environment variable {var} is not set"
                ))
            })?),
            None => None,
        };
        let mut b = reqwest::Client::builder()
            .timeout(Duration::from_secs(cfg.timeout_secs))
            .connect_timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("radio-platform/", env!("CARGO_PKG_VERSION")));
        if let Some(ca) = &cfg.ca_file {
            let pem = std::fs::read(ca).map_err(|e| ConfigError(format!("ca_file: {e}")))?;
            let cert = reqwest::Certificate::from_pem(&pem)
                .map_err(|e| ConfigError(format!("ca_file: {e}")))?;
            b = b.add_root_certificate(cert);
        }
        let http = b.build().map_err(|e| ConfigError(e.to_string()))?;
        Ok(Self {
            http,
            url,
            style: cfg.api,
            model: cfg.model.clone(),
            language: cfg.language.clone(),
            prompt: cfg.prompt.clone(),
            key,
        })
    }

    /// Where requests go (no secrets), for logs and status pages.
    #[must_use]
    pub fn describe(&self) -> String {
        let style = match self.style {
            ApiStyle::Openai => "openai",
            ApiStyle::WhisperCpp => "whisper_cpp",
        };
        format!(
            "{style} {}://{}{}",
            self.url.scheme(),
            self.url.host_str().unwrap_or_default(),
            self.url.port().map(|p| format!(":{p}")).unwrap_or_default()
        )
    }

    /// True if the endpoint is on this host (loopback).
    #[must_use]
    pub fn is_local(&self) -> bool {
        crate::config::is_loopback_host(self.url.host_str().unwrap_or_default())
    }

    /// The model name reported with transcripts (`openai` style only).
    #[must_use]
    pub fn model(&self) -> Option<&str> {
        match self.style {
            ApiStyle::Openai => Some(&self.model),
            ApiStyle::WhisperCpp => None,
        }
    }

    /// Transcribes a WAV file.
    ///
    /// # Errors
    /// [`TranscribeError`].
    pub async fn transcribe(&self, wav: Vec<u8>) -> Result<Transcript, TranscribeError> {
        let file = Part::bytes(wav)
            .file_name("transmission.wav")
            .mime_str("audio/wav")
            .map_err(|e| TranscribeError::Permanent(e.to_string()))?;
        let mut form = Form::new()
            .part("file", file)
            .text("response_format", "json")
            .text("temperature", "0");
        if self.style == ApiStyle::Openai {
            form = form.text("model", self.model.clone());
        }
        if let Some(l) = &self.language {
            form = form.text("language", l.clone());
        }
        if let Some(p) = &self.prompt {
            form = form.text("prompt", p.clone());
        }
        let mut req = self.http.post(self.url.clone()).multipart(form);
        if let Some(k) = &self.key {
            req = req.bearer_auth(k);
        }
        let mut resp = req.send().await.map_err(|e| {
            TranscribeError::Transient(format!("request failed: {}", without_url(&e)))
        })?;
        let status = resp.status();
        let mut body = Vec::new();
        loop {
            match resp.chunk().await {
                Ok(Some(c)) => {
                    if body.len() + c.len() > MAX_RESPONSE {
                        return Err(TranscribeError::Permanent("response too large".into()));
                    }
                    body.extend_from_slice(&c);
                }
                Ok(None) => break,
                Err(e) => {
                    return Err(TranscribeError::Transient(format!(
                        "reading response: {}",
                        without_url(&e)
                    )));
                }
            }
        }
        if !status.is_success() {
            let snippet: String = String::from_utf8_lossy(&body)
                .chars()
                .filter(|c| !c.is_control())
                .take(200)
                .collect();
            let msg = format!("service returned {status}: {snippet}");
            return Err(
                if status == StatusCode::TOO_MANY_REQUESTS
                    || status == StatusCode::REQUEST_TIMEOUT
                    || status.is_server_error()
                {
                    TranscribeError::Transient(msg)
                } else {
                    TranscribeError::Permanent(msg)
                },
            );
        }
        let r: Response = serde_json::from_slice(&body)
            .map_err(|e| TranscribeError::Permanent(format!("unreadable response: {e}")))?;
        let language = r
            .language
            .map(|l| clean_text(&l))
            .filter(|l| !l.is_empty() && l.len() <= 32);
        Ok(Transcript {
            text: clean_text(&r.text),
            language,
        })
    }
}

/// Error text without the URL (which never carries secrets here, but keeps
/// messages short and stable).
fn without_url(e: &reqwest::Error) -> String {
    let mut s = e.to_string();
    if let Some(u) = e.url() {
        s = s.replace(u.as_str(), "endpoint");
    }
    if e.is_timeout() {
        s = format!("timed out ({s})");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    fn cfg(endpoint: &str, api: &str) -> TranscriptionConfig {
        toml::from_str(&format!(
            "endpoint = \"{endpoint}\"\napi = \"{api}\"\nlanguage = \"en\"\nprompt = \"Engine 5\"\ntimeout_secs = 5"
        ))
        .unwrap_or_else(|e| panic!("{e}"))
    }

    fn tone_wav(hz: f64) -> Vec<u8> {
        #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
        let pcm: Vec<i16> = (0..16_000)
            .map(|i| ((f64::from(i) * hz * std::f64::consts::TAU / 16_000.0).sin() * 9000.0) as i16)
            .collect();
        crate::wav(&pcm, 16_000)
    }

    #[test]
    fn cleans_text() {
        assert_eq!(
            clean_text("  Engine\t5 \n\u{7} responding  "),
            "Engine 5 responding"
        );
        assert_eq!(
            clean_text(&"a ".repeat(20_000)).chars().count(),
            MAX_TEXT - 1
        );
        assert_eq!(clean_text(""), "");
    }

    #[tokio::test]
    async fn both_request_styles_against_the_stub() {
        let cancel = tokio_util::sync::CancellationToken::new();
        let stub = sds_simulator::whisper::start(
            "127.0.0.1:0".parse().unwrap_or_else(|e| panic!("{e}")),
            cancel.clone(),
        )
        .await
        .unwrap_or_else(|e| panic!("{e}"));
        for (api, path) in [
            ("openai", "/v1/audio/transcriptions"),
            ("whisper_cpp", "/inference"),
        ] {
            let c = WhisperClient::new(&cfg(&stub.url(), api)).unwrap_or_else(|e| panic!("{e}"));
            let t = c
                .transcribe(tone_wav(440.0))
                .await
                .unwrap_or_else(|e| panic!("{e}"));
            assert_eq!(t.text, "Simulated transcript: 440 hertz tone, 1.0 seconds.");
            assert_eq!(t.language.as_deref(), Some("en"));
            assert_eq!(
                stub.state
                    .last_path
                    .lock()
                    .map(|p| p.clone())
                    .unwrap_or_default(),
                path
            );
            let f = stub
                .state
                .last_fields
                .lock()
                .map(|f| f.clone())
                .unwrap_or_default();
            assert_eq!(f.get("language").map(String::as_str), Some("en"));
            assert_eq!(f.get("prompt").map(String::as_str), Some("Engine 5"));
            assert_eq!(f.contains_key("model"), api == "openai");
        }
        let c = WhisperClient::new(&cfg(&stub.url(), "openai")).unwrap_or_else(|e| panic!("{e}"));
        assert!(c.describe().starts_with("openai http://127.0.0.1:"));
        // 503 is transient, 400 permanent.
        stub.state.fail_next.store(1, Ordering::Relaxed);
        assert!(
            c.transcribe(tone_wav(440.0))
                .await
                .is_err_and(|e| e.is_transient())
        );
        stub.state.reject_next.store(1, Ordering::Relaxed);
        assert!(
            c.transcribe(tone_wav(440.0))
                .await
                .is_err_and(|e| !e.is_transient())
        );
        // Not WAV: the stub rejects it (permanent).
        assert!(
            c.transcribe(b"nope".to_vec())
                .await
                .is_err_and(|e| !e.is_transient())
        );
        // Missing key → 401, permanent.
        if let Ok(mut k) = stub.state.api_key.lock() {
            *k = Some("sekrit".into());
        }
        assert!(
            c.transcribe(tone_wav(440.0))
                .await
                .is_err_and(|e| !e.is_transient())
        );
        cancel.cancel();
        // Nothing listening: transient.
        let dead = WhisperClient::new(&cfg("http://127.0.0.1:9", "openai"))
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(
            dead.transcribe(tone_wav(440.0))
                .await
                .is_err_and(|e| e.is_transient())
        );
    }

    #[test]
    fn missing_key_variable_is_a_config_error() {
        let mut c = cfg("http://127.0.0.1:1", "openai");
        c.api_key_env = Some("RADIO_PLATFORM_TEST_UNSET_KEY_VAR".into());
        assert!(WhisperClient::new(&c).is_err());
    }
}
