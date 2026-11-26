//! A stand-in for a Whisper-compatible transcription service, for tests
//! and the dev stack. **It does not recognise speech.** It parses the
//! uploaded WAV, estimates the dominant tone (the simulator's audio is a
//! tone per channel), and returns a sentence naming it, so tests can prove
//! that the right audio reached the service.
//!
//! Serves both request styles the gateway supports:
//! `POST /v1/audio/transcriptions` (OpenAI style) and `POST /inference`
//! (whisper.cpp style).

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::extract::{DefaultBodyLimit, Multipart, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

/// Stub behaviour and observations.
#[derive(Debug, Default)]
pub struct WhisperState {
    /// Requests received (including failed ones).
    pub requests: AtomicU64,
    /// Answer this many upcoming requests with 503.
    pub fail_next: AtomicU32,
    /// Answer this many upcoming requests with 400.
    pub reject_next: AtomicU32,
    /// Delay before answering, in ms.
    pub delay_ms: AtomicU64,
    /// Required bearer token, if any.
    pub api_key: Mutex<Option<String>>,
    /// Text form fields of the last request (the file is omitted).
    pub last_fields: Mutex<HashMap<String, String>>,
    /// Path of the last request.
    pub last_path: Mutex<String>,
}

/// A running stub.
#[derive(Debug, Clone)]
pub struct WhisperStub {
    /// Listening address.
    pub addr: SocketAddr,
    /// Shared state.
    pub state: Arc<WhisperState>,
}

impl WhisperStub {
    /// Base URL (`http://addr`).
    #[must_use]
    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }
}

/// Starts the stub on `bind` until `cancel`.
///
/// # Errors
/// The address cannot be bound.
pub async fn start(bind: SocketAddr, cancel: CancellationToken) -> std::io::Result<WhisperStub> {
    let listener = TcpListener::bind(bind).await?;
    let addr = listener.local_addr()?;
    let state = Arc::new(WhisperState::default());
    let app = Router::new()
        .route("/v1/audio/transcriptions", post(transcribe))
        .route("/inference", post(transcribe))
        .layer(DefaultBodyLimit::max(64 * 1024 * 1024))
        .with_state(Arc::clone(&state));
    tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async move { cancel.cancelled().await })
            .await;
    });
    Ok(WhisperStub { addr, state })
}

fn le16(b: &[u8], at: usize) -> Option<u16> {
    b.get(at..at + 2)
        .and_then(|s| <[u8; 2]>::try_from(s).ok())
        .map(u16::from_le_bytes)
}

fn le32(b: &[u8], at: usize) -> Option<u32> {
    b.get(at..at + 4)
        .and_then(|s| <[u8; 4]>::try_from(s).ok())
        .map(u32::from_le_bytes)
}

/// Parses a 16-bit mono PCM WAV; returns the rate and samples.
#[must_use]
pub fn parse_wav(b: &[u8]) -> Option<(u32, Vec<i16>)> {
    if b.get(..4) != Some(&b"RIFF"[..]) || b.get(8..12) != Some(&b"WAVE"[..]) {
        return None;
    }
    let mut at = 12;
    let mut rate = None;
    while at + 8 <= b.len() {
        let id = b.get(at..at + 4)?;
        let len = usize::try_from(le32(b, at + 4)?).ok()?;
        let body = at + 8;
        if id == b"fmt " {
            if le16(b, body)? != 1 || le16(b, body + 2)? != 1 || le16(b, body + 14)? != 16 {
                return None; // not PCM, not mono, or not 16-bit
            }
            rate = Some(le32(b, body + 4)?);
        } else if id == b"data" {
            let data = b.get(body..body + len)?;
            let pcm = data
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| i16::from_le_bytes(*c))
                .collect();
            return Some((rate?, pcm));
        }
        at = body + len + (len & 1);
    }
    None
}

/// Estimates the dominant frequency by counting zero crossings in the
/// loud 20 ms windows only (quiet windows, such as the recording's
/// pre-buffer, are mostly noise).
#[must_use]
pub fn estimate_tone(rate: u32, pcm: &[i16]) -> Option<f64> {
    let win = usize::try_from(rate / 50).ok()?.max(1);
    #[allow(clippy::cast_precision_loss)]
    let rms = |w: &[i16]| {
        (w.iter().map(|&s| f64::from(s) * f64::from(s)).sum::<f64>() / w.len().max(1) as f64).sqrt()
    };
    let peak = pcm.chunks(win).map(rms).fold(0.0, f64::max);
    if peak < 100.0 {
        return None;
    }
    let (mut crossings, mut samples) = (0usize, 0usize);
    for (i, w) in pcm.chunks(win).enumerate() {
        if rms(w) < peak * 0.3 {
            continue;
        }
        // Include the previous window's last sample so a crossing at the
        // boundary is counted.
        let start = (i * win).saturating_sub(1);
        let span = pcm.get(start..i * win + w.len()).unwrap_or(w);
        crossings += span
            .windows(2)
            .filter(|p| matches!(p, [a, b] if (*a < 0) != (*b < 0)))
            .count();
        samples += w.len();
    }
    if samples < win * 10 {
        return None;
    }
    #[allow(clippy::cast_precision_loss)]
    let secs = samples as f64 / f64::from(rate);
    #[allow(clippy::cast_precision_loss)]
    Some(crossings as f64 / 2.0 / secs)
}

/// Decrements a positive counter; true if it was positive.
fn take_one(a: &AtomicU32) -> bool {
    let mut cur = a.load(Ordering::Relaxed);
    while cur > 0 {
        match a.compare_exchange(cur, cur - 1, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return true,
            Err(now) => cur = now,
        }
    }
    false
}

async fn transcribe(
    State(state): State<Arc<WhisperState>>,
    uri: axum::http::Uri,
    headers: HeaderMap,
    mut form: Multipart,
) -> Response {
    state.requests.fetch_add(1, Ordering::Relaxed);
    if let Ok(mut p) = state.last_path.lock() {
        *p = uri.path().to_owned();
    }
    let delay = state.delay_ms.load(Ordering::Relaxed);
    if delay > 0 {
        tokio::time::sleep(Duration::from_millis(delay)).await;
    }
    let want = state.api_key.lock().ok().and_then(|k| k.clone());
    if let Some(k) = want {
        let ok = headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v == format!("Bearer {k}"));
        if !ok {
            return (StatusCode::UNAUTHORIZED, "bad key").into_response();
        }
    }
    if take_one(&state.fail_next) {
        return (StatusCode::SERVICE_UNAVAILABLE, "busy").into_response();
    }
    if take_one(&state.reject_next) {
        return (StatusCode::BAD_REQUEST, "rejected").into_response();
    }
    let mut fields = HashMap::new();
    let mut wav = None;
    while let Ok(Some(field)) = form.next_field().await {
        let name = field.name().unwrap_or_default().to_owned();
        if name == "file" {
            wav = field.bytes().await.ok();
        } else if let Ok(v) = field.text().await {
            fields.insert(name, v);
        }
    }
    if let Ok(mut f) = state.last_fields.lock() {
        *f = fields;
    }
    let Some((rate, pcm)) = wav.as_deref().and_then(parse_wav) else {
        return (StatusCode::BAD_REQUEST, "file must be 16-bit mono PCM WAV").into_response();
    };
    let text = match estimate_tone(rate, &pcm) {
        Some(hz) => {
            #[allow(clippy::cast_precision_loss)]
            let secs = pcm.len() as f64 / f64::from(rate);
            format!(
                "Simulated transcript: {:.0} hertz tone, {secs:.1} seconds.",
                (hz / 10.0).round() * 10.0
            )
        }
        None => String::new(),
    };
    axum::Json(serde_json::json!({ "text": text, "language": "en" })).into_response()
}
