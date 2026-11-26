//! Prometheus metrics (text exposition format 0.0.4), served on a separate
//! internal listener (`[metrics]`), never on the public API.
//!
//! HTTP metrics are labelled with the **route template** (for example
//! `/api/v1/scanners/{id}`), never the concrete path, so label cardinality
//! stays bounded and identifiers do not leak into the monitoring system.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use axum::extract::{MatchedPath, Request, State};
use axum::middleware::Next;
use axum::response::Response;

use crate::state::AppState;

/// Latency buckets in seconds.
const BUCKETS: [f64; 11] = [
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 15.0,
];

#[derive(Debug, Default, Clone)]
struct Hist {
    count: u64,
    sum: f64,
    buckets: [u64; BUCKETS.len()],
}

/// Gateway metrics.
#[derive(Debug)]
pub struct Metrics {
    started: Instant,
    http: Mutex<BTreeMap<(String, String, u16), Hist>>,
}

impl Default for Metrics {
    fn default() -> Self {
        Self {
            started: Instant::now(),
            http: Mutex::new(BTreeMap::new()),
        }
    }
}

impl Metrics {
    /// Records one HTTP request.
    pub fn observe(&self, method: &str, route: &str, status: u16, secs: f64) {
        let mut m = self
            .http
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let h = m
            .entry((method.to_owned(), route.to_owned(), status))
            .or_default();
        h.count += 1;
        h.sum += secs;
        for (slot, le) in h.buckets.iter_mut().zip(BUCKETS) {
            if secs <= le {
                *slot += 1;
            }
        }
    }
}

/// Middleware: times every routed request.
pub async fn track(State(state): State<Arc<AppState>>, req: Request, next: Next) -> Response {
    let route = req
        .extensions()
        .get::<MatchedPath>()
        .map_or_else(|| "unmatched".to_owned(), |p| p.as_str().to_owned());
    let method = req.method().as_str().to_owned();
    let t = Instant::now();
    let resp = next.run(req).await;
    state.metrics.observe(
        &method,
        &route,
        resp.status().as_u16(),
        t.elapsed().as_secs_f64(),
    );
    resp
}

fn esc(v: &str) -> String {
    v.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

fn metric(out: &mut String, name: &str, kind: &str, help: &str) {
    let _ = writeln!(out, "# HELP {name} {help}");
    let _ = writeln!(out, "# TYPE {name} {kind}");
}

#[allow(clippy::cast_precision_loss)]
fn gauge(out: &mut String, name: &str, help: &str, v: f64) {
    metric(out, name, "gauge", help);
    let _ = writeln!(out, "{name} {v}");
}

fn counter(out: &mut String, name: &str, help: &str, v: u64) {
    metric(out, name, "counter", help);
    let _ = writeln!(out, "{name} {v}");
}

/// Renders every metric.
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn render(state: &AppState) -> String {
    let mut out = String::new();
    metric(&mut out, "radio_build_info", "gauge", "Build information.");
    let _ = writeln!(
        out,
        "radio_build_info{{version=\"{}\"}} 1",
        env!("CARGO_PKG_VERSION")
    );
    gauge(
        &mut out,
        "radio_uptime_seconds",
        "Seconds since the gateway started.",
        state.metrics.started.elapsed().as_secs_f64(),
    );

    // HTTP.
    let http = state
        .metrics
        .http
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    metric(
        &mut out,
        "radio_http_requests_total",
        "counter",
        "HTTP requests by method, route template and status.",
    );
    for ((m, r, s), h) in &http {
        let _ = writeln!(
            out,
            "radio_http_requests_total{{method=\"{m}\",route=\"{}\",status=\"{s}\"}} {}",
            esc(r),
            h.count
        );
    }
    metric(
        &mut out,
        "radio_http_request_duration_seconds",
        "histogram",
        "HTTP request latency by method and route template.",
    );
    let mut by_route: BTreeMap<(&str, &str), Hist> = BTreeMap::new();
    for ((m, r, _), h) in &http {
        let e = by_route.entry((m.as_str(), r.as_str())).or_default();
        e.count += h.count;
        e.sum += h.sum;
        for (a, b) in e.buckets.iter_mut().zip(h.buckets) {
            *a += b;
        }
    }
    for ((m, r), h) in &by_route {
        let labels = format!("method=\"{m}\",route=\"{}\"", esc(r));
        for (le, n) in BUCKETS.iter().zip(h.buckets) {
            let _ = writeln!(
                out,
                "radio_http_request_duration_seconds_bucket{{{labels},le=\"{le}\"}} {n}"
            );
        }
        let _ = writeln!(
            out,
            "radio_http_request_duration_seconds_bucket{{{labels},le=\"+Inf\"}} {}",
            h.count
        );
        let _ = writeln!(
            out,
            "radio_http_request_duration_seconds_sum{{{labels}}} {}",
            h.sum
        );
        let _ = writeln!(
            out,
            "radio_http_request_duration_seconds_count{{{labels}}} {}",
            h.count
        );
    }

    // Agents and media.
    gauge(
        &mut out,
        "radio_agents_connected",
        "Agents connected to this replica.",
        state.hub.local_agents().len() as f64,
    );
    let (streams, listeners) = state.media.hub.totals();
    gauge(
        &mut out,
        "radio_media_streams",
        "Scanners streaming audio to this replica.",
        streams as f64,
    );
    gauge(
        &mut out,
        "radio_media_listeners",
        "Listener subscriptions on this replica.",
        listeners as f64,
    );
    gauge(
        &mut out,
        "radio_media_webrtc_sessions",
        "WebRTC listening sessions on this replica.",
        state
            .media
            .webrtc()
            .map_or(0, media_gateway::WebRtcServer::sessions) as f64,
    );
    if let Some(r) = state.media.recorder() {
        let s = r.stats();
        counter(
            &mut out,
            "radio_recordings_stored_total",
            "Recordings stored.",
            s.stored,
        );
        counter(
            &mut out,
            "radio_recordings_failed_total",
            "Recordings that failed to store.",
            s.failed,
        );
        gauge(
            &mut out,
            "radio_recordings_in_progress",
            "Recordings being assembled.",
            s.in_progress as f64,
        );
    }
    if let Some(t) = state.media.transcription() {
        let s = t.stats();
        counter(
            &mut out,
            "radio_transcriptions_completed_total",
            "Transcriptions completed.",
            s.completed,
        );
        counter(
            &mut out,
            "radio_transcriptions_retried_total",
            "Transcription attempts scheduled for retry.",
            s.retried,
        );
        counter(
            &mut out,
            "radio_transcriptions_failed_total",
            "Transcriptions given up.",
            s.failed,
        );
        counter(
            &mut out,
            "radio_transcriptions_skipped_total",
            "Recordings not sent for transcription.",
            s.skipped,
        );
        gauge(
            &mut out,
            "radio_transcriptions_in_flight",
            "Transcription requests in progress.",
            s.in_flight as f64,
        );
    }
    if let Some(r) = state.retention.get()
        && let Some(last) = r.last()
    {
        gauge(
            &mut out,
            "radio_retention_last_sweep_timestamp_seconds",
            "Unix time of this replica's last retention sweep.",
            last.at.map_or(0.0, |t| t.unix_timestamp() as f64),
        );
        metric(
            &mut out,
            "radio_retention_last_sweep_removed",
            "gauge",
            "Items removed by the last sweep.",
        );
        for (kind, n) in [
            ("recordings", last.recordings),
            ("transcripts", last.transcripts),
            ("transmissions", last.transmissions),
            ("events", last.events),
        ] {
            let _ = writeln!(
                out,
                "radio_retention_last_sweep_removed{{kind=\"{kind}\"}} {n}"
            );
        }
        gauge(
            &mut out,
            "radio_retention_last_sweep_errors",
            "Errors in the last sweep.",
            last.errors as f64,
        );
    }
    out
}

/// Internal router: `GET /metrics`.
pub fn router(state: Arc<AppState>) -> axum::Router {
    axum::Router::new()
        .route(
            "/metrics",
            axum::routing::get(|State(s): State<Arc<AppState>>| async move {
                (
                    [(
                        axum::http::header::CONTENT_TYPE,
                        "text/plain; version=0.0.4; charset=utf-8",
                    )],
                    render(&s),
                )
            }),
        )
        .with_state(state)
}
