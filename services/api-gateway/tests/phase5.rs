//! Phase 5 API tests: transcripts as content (redaction, audited search),
//! the transcription toggle, re-transcription, holds and deletion,
//! analytics, the retention sweeper, and the transcription worker against
//! the stub service.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use api_gateway::config::{LimitsConfig, RetentionConfig};
use api_gateway::retention::Retention;
use api_gateway::transcription::Transcription;
use api_gateway::{AppState, public_app};
use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
use platform_auth::testing::TestIssuer;
use platform_auth::{OidcConfig, OidcVerifier, Permission};
use platform_common::{ScannerId, SiteId, TenantId, TransmissionId, TransmissionStart};
use platform_storage::model::{TranscriptionResult, TranscriptionState};
use platform_storage::{MemoryStore, PageRequest, Store};
use scanner_recorder::{RecordingStore, SegmentPolicy, StorageConfig};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

const ISS: &str = "https://idp.example.test";
const AUD: &str = "radio-platform";

fn roles() -> BTreeMap<String, Vec<Permission>> {
    use Permission::*;
    BTreeMap::from([
        ("admin".into(), Permission::ALL.to_vec()),
        ("viewer".into(), vec![ScannerView, EventView]),
        ("listener".into(), vec![ScannerView, EventView, AudioListen]),
        (
            "operator".into(),
            vec![ScannerView, ScannerManage, EventView],
        ),
        (
            "records".into(),
            vec![ScannerView, EventView, RecordingManage],
        ),
    ])
}

struct Ctx {
    app: Router,
    state: Arc<AppState>,
    store: Arc<MemoryStore>,
    issuer: TestIssuer,
    tenant: TenantId,
    site: SiteId,
    scanner: ScannerId,
}

impl Ctx {
    fn token(&self, role: &str) -> String {
        self.issuer.token(
            role,
            600,
            json!({ "tenant_id": self.tenant.to_string(), "roles": [role], "sites": ["*"] }),
        )
    }

    async fn call(
        &self,
        method: Method,
        uri: &str,
        role: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let b = Request::builder()
            .method(method)
            .uri(uri)
            .header("authorization", format!("Bearer {}", self.token(role)));
        let req = match body {
            Some(v) => b
                .header("content-type", "application/json")
                .body(Body::from(v.to_string()))
                .unwrap(),
            None => b.body(Body::empty()).unwrap(),
        };
        let resp = self.app.clone().oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes)
                .unwrap_or(Value::String(String::from_utf8_lossy(&bytes).into_owned())),
        )
    }

    async fn transmission(
        &self,
        started: time::OffsetDateTime,
        tgid: &str,
        ms: u64,
    ) -> TransmissionId {
        let id = TransmissionId::new();
        let s = TransmissionStart {
            transmission_id: id,
            scanner_id: self.scanner,
            site_id: Some(self.site),
            started_at: started,
            frequency_hz: Some(154_430_000),
            tgid: Some(tgid.into()),
            unit_id: None,
            system: None,
            department: None,
            channel: Some("Dispatch".into()),
            modulation: None,
            signal_bars: None,
            rssi: None,
        };
        self.store
            .start_transmission(self.tenant, self.site, &s)
            .await
            .unwrap();
        self.store
            .end_transmission(
                id,
                started + time::Duration::milliseconds(i64::try_from(ms).unwrap()),
                ms,
            )
            .await
            .unwrap();
        id
    }

    async fn audit_actions(&self) -> Vec<(String, String, String)> {
        let page = self
            .store
            .list_audit(
                self.tenant,
                &platform_storage::model::AuditFilter::default(),
                &PageRequest {
                    limit: 200,
                    cursor: None,
                },
            )
            .await
            .unwrap();
        page.items
            .into_iter()
            .map(|a| (a.actor, a.action, a.outcome.as_str().to_owned()))
            .collect()
    }
}

async fn ctx() -> Ctx {
    let issuer = TestIssuer::new(ISS, AUD);
    let mut oidc = OidcConfig::new(ISS, AUD);
    oidc.role_permissions = roles();
    let verifier = OidcVerifier::with_static_jwks(oidc, issuer.jwks());
    let store = Arc::new(MemoryStore::new());
    let state = AppState::new(
        Arc::clone(&store) as Arc<dyn Store>,
        Arc::new(verifier),
        LimitsConfig::default(),
    );
    let app = public_app(Arc::clone(&state));
    let mut c = Ctx {
        app,
        state,
        store,
        issuer,
        tenant: TenantId::new(),
        site: SiteId::new(),
        scanner: ScannerId::new(),
    };
    let (s, site) = c
        .call(
            Method::POST,
            "/api/v1/sites",
            "admin",
            Some(json!({ "name": "Site A", "allowed_networks": ["192.168.50.0/24"] })),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{site}");
    c.site = site["id"].as_str().unwrap().parse().unwrap();
    let (s, _) = c
        .call(
            Method::POST,
            "/api/v1/agents",
            "admin",
            Some(json!({ "agent_id": "agent-a", "site_id": c.site })),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED);
    let (s, sc) = c
        .call(
            Method::POST,
            "/api/v1/scanners",
            "admin",
            Some(json!({ "site_id": c.site, "agent_id": "agent-a", "name": "SDS200", "host": "192.168.50.10" })),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{sc}");
    c.scanner = sc["id"].as_str().unwrap().parse().unwrap();
    c
}

fn transcription_cfg(
    endpoint: &str,
    max_attempts: u32,
) -> scanner_transcription::TranscriptionConfig {
    toml::from_str(&format!(
        "endpoint = \"{endpoint}\"\nmax_attempts = {max_attempts}\ntimeout_secs = 10\nconcurrency = 2"
    ))
    .unwrap()
}

/// A tone recording encoded like the agent does.
fn tone_recording(hz: f64, ms: usize) -> Vec<u8> {
    let mut enc = scanner_audio::opus_enc::Encoder::new().unwrap();
    let mut w = scanner_audio::ogg::OggOpusWriter::new(1, enc.pre_skip(), 8000, "test", &[]);
    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
    let pcm: Vec<i16> = (0..ms * 8)
        .map(|i| ((i as f64 * hz * std::f64::consts::TAU / 8000.0).sin() * 12_000.0) as i16)
        .collect();
    for f in enc.push(&pcm).unwrap() {
        w.push(&f.data, scanner_audio::FRAME_48K);
    }
    w.finish()
}

#[tokio::test]
async fn transcripts_are_content() {
    let c = ctx().await;
    let id = c
        .transmission(platform_common::now_utc(), "100", 3000)
        .await;
    c.store
        .set_transmission_audio(c.tenant, id, "k", true, platform_common::now_utc())
        .await
        .unwrap();
    c.store
        .finish_transcription(
            c.tenant,
            id,
            TranscriptionResult::Done {
                text: "Engine 5 responding to Main Street".into(),
                language: Some("en".into()),
                model: Some("whisper-1".into()),
            },
            platform_common::now_utc(),
        )
        .await
        .unwrap();

    // Without audio_listen the transcript is removed; status stays.
    let (s, page) = c
        .call(Method::GET, "/api/v1/transmissions", "viewer", None)
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(page["items"][0]["transcript"], Value::Null);
    assert_eq!(page["items"][0]["transcription_state"], "done");
    let (_, one) = c
        .call(
            Method::GET,
            &format!("/api/v1/transmissions/{id}"),
            "viewer",
            None,
        )
        .await;
    assert_eq!(one["transcript"], Value::Null);
    let (_, one) = c
        .call(
            Method::GET,
            &format!("/api/v1/transmissions/{id}"),
            "listener",
            None,
        )
        .await;
    assert_eq!(one["transcript"], "Engine 5 responding to Main Street");
    assert_eq!(one["transcript_model"], "whisper-1");

    // Text search needs audio_listen and is audited either way.
    let (s, _) = c
        .call(
            Method::GET,
            "/api/v1/transmissions?q=engine",
            "viewer",
            None,
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, page) = c
        .call(
            Method::GET,
            "/api/v1/transmissions?q=main%20street",
            "listener",
            None,
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(page["items"].as_array().unwrap().len(), 1);
    let (_, page) = c
        .call(
            Method::GET,
            "/api/v1/transmissions?q=ladder",
            "listener",
            None,
        )
        .await;
    assert!(page["items"].as_array().unwrap().is_empty());
    let (s, _) = c
        .call(
            Method::GET,
            &format!("/api/v1/transmissions?q={}", "a".repeat(300)),
            "listener",
            None,
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let audit = c.audit_actions().await;
    assert!(audit.contains(&(
        "viewer".into(),
        "transmission.search".into(),
        "denied".into()
    )));
    assert_eq!(
        audit
            .iter()
            .filter(|a| a.0 == "listener" && a.1 == "transmission.search" && a.2 == "success")
            .count(),
        2
    );
    let (_, page) = c
        .call(
            Method::GET,
            "/api/v1/transmissions?transcription=done&recorded=true",
            "viewer",
            None,
        )
        .await;
    assert_eq!(page["items"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn transcription_toggle_and_requests() {
    let c = ctx().await;
    let scanner = format!("/api/v1/scanners/{}", c.scanner);
    // Not configured on the gateway.
    let (s, _) = c
        .call(
            Method::PATCH,
            &scanner,
            "admin",
            Some(json!({ "transcription_enabled": true })),
        )
        .await;
    assert_eq!(s, StatusCode::CONFLICT);
    let id = c
        .transmission(platform_common::now_utc(), "100", 3000)
        .await;
    let (s, _) = c
        .call(
            Method::POST,
            &format!("/api/v1/transmissions/{id}/transcribe"),
            "admin",
            None,
        )
        .await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE);

    let recordings = RecordingStore::open(&StorageConfig::Memory).unwrap();
    c.state.media.enable_recording(
        recordings,
        SegmentPolicy::default(),
        Arc::clone(&c.store) as Arc<dyn Store>,
    );
    c.state.media.enable_transcription(Arc::new(
        Transcription::new(transcription_cfg("http://127.0.0.1:9", 3)).unwrap(),
    ));
    // Needs transcription_manage.
    let (s, _) = c
        .call(
            Method::PATCH,
            &scanner,
            "operator",
            Some(json!({ "transcription_enabled": true })),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    // Needs recording.
    let (s, _) = c
        .call(
            Method::PATCH,
            &scanner,
            "admin",
            Some(json!({ "transcription_enabled": true })),
        )
        .await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (s, sc) = c
        .call(
            Method::PATCH,
            &scanner,
            "admin",
            Some(json!({ "audio_enabled": true, "recording_enabled": true, "transcription_enabled": true })),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{sc}");
    assert_eq!(sc["transcription_enabled"], true);
    let (_, sc) = c
        .call(
            Method::PATCH,
            &scanner,
            "admin",
            Some(json!({ "recording_enabled": false })),
        )
        .await;
    assert_eq!(sc["transcription_enabled"], false, "follows recording");

    // Re-transcription: needs a recording and transcription_manage.
    let url = format!("/api/v1/transmissions/{id}/transcribe");
    let (s, _) = c.call(Method::POST, &url, "admin", None).await;
    assert_eq!(s, StatusCode::CONFLICT);
    c.store
        .set_transmission_audio(c.tenant, id, "k", false, platform_common::now_utc())
        .await
        .unwrap();
    let (s, _) = c.call(Method::POST, &url, "records", None).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = c.call(Method::POST, &url, "admin", None).await;
    assert_eq!(s, StatusCode::ACCEPTED);
    let t = c.store.get_transmission(c.tenant, id).await.unwrap();
    assert_eq!(t.transcription_state, Some(TranscriptionState::Pending));

    let (s, st) = c
        .call(Method::GET, "/api/v1/transcription/status", "viewer", None)
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(st["enabled"], true);
    assert_eq!(st["queue"]["pending"], 1);
    assert_eq!(st["service"], Value::Null, "endpoint hidden from viewers");
    let (_, st) = c
        .call(Method::GET, "/api/v1/transcription/status", "admin", None)
        .await;
    assert!(
        st["service"]
            .as_str()
            .unwrap()
            .starts_with("openai http://127.0.0.1")
    );
    assert!(c.audit_actions().await.contains(&(
        "admin".into(),
        "transmission.transcribe".into(),
        "success".into()
    )));
}

#[tokio::test]
async fn holds_and_recording_deletion() {
    let c = ctx().await;
    let recordings = RecordingStore::open(&StorageConfig::Memory).unwrap();
    c.state.media.enable_recording(
        recordings.clone(),
        SegmentPolicy::default(),
        Arc::clone(&c.store) as Arc<dyn Store>,
    );
    let id = c
        .transmission(platform_common::now_utc(), "100", 3000)
        .await;
    recordings
        .put("rec/1.opus", tone_recording(500.0, 1000))
        .await
        .unwrap();
    c.store
        .set_transmission_audio(
            c.tenant,
            id,
            "rec/1.opus",
            false,
            platform_common::now_utc(),
        )
        .await
        .unwrap();
    let hold = format!("/api/v1/transmissions/{id}/hold");
    let audio = format!("/api/v1/transmissions/{id}/audio");
    let (s, _) = c
        .call(Method::PUT, &hold, "viewer", Some(json!({ "hold": true })))
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, t) = c
        .call(Method::PUT, &hold, "records", Some(json!({ "hold": true })))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(t["hold"], true);
    let (s, _) = c.call(Method::DELETE, &audio, "records", None).await;
    assert_eq!(s, StatusCode::CONFLICT, "held");
    assert!(recordings.get("rec/1.opus").await.is_ok());
    c.call(
        Method::PUT,
        &hold,
        "records",
        Some(json!({ "hold": false })),
    )
    .await;
    let (s, _) = c.call(Method::DELETE, &audio, "viewer", None).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = c.call(Method::DELETE, &audio, "records", None).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    assert!(
        recordings.get("rec/1.opus").await.is_err(),
        "object deleted"
    );
    let t = c.store.get_transmission(c.tenant, id).await.unwrap();
    assert!(t.audio_object.is_none());
    let (s, _) = c.call(Method::DELETE, &audio, "records", None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let audit = c.audit_actions().await;
    for a in [
        "transmission.hold",
        "transmission.release",
        "transmission.audio.delete",
    ] {
        assert!(
            audit.contains(&("records".into(), a.into(), "success".into())),
            "{a}"
        );
    }
}

#[tokio::test]
async fn analytics_fill_gaps_and_validate() {
    let c = ctx().await;
    let base = time::macros::datetime!(2026-11-20 08:00 UTC);
    c.transmission(base + time::Duration::minutes(5), "100", 2000)
        .await;
    c.transmission(base + time::Duration::minutes(50), "100", 3000)
        .await;
    c.transmission(base + time::Duration::hours(2), "200", 1000)
        .await;
    let from = "2026-11-20T08:00:00Z";
    let to = "2026-11-20T12:00:00Z";
    let (s, a) = c
        .call(
            Method::GET,
            &format!("/api/v1/analytics/activity?from={from}&to={to}"),
            "viewer",
            None,
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{a}");
    let pts: Vec<(u64, u64)> = a["points"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| {
            (
                p["transmissions"].as_u64().unwrap(),
                p["airtime_ms"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(pts, vec![(2, 5000), (0, 0), (1, 1000), (0, 0)]);
    let (_, top) = c
        .call(
            Method::GET,
            &format!("/api/v1/analytics/top?from={from}&to={to}&by=talkgroup"),
            "viewer",
            None,
        )
        .await;
    assert_eq!(top["entries"][0]["key"], "100");
    assert_eq!(top["entries"][0]["transmissions"], 2);
    let (_, top) = c
        .call(
            Method::GET,
            &format!("/api/v1/analytics/top?from={from}&to={to}&by=scanner"),
            "viewer",
            None,
        )
        .await;
    assert_eq!(top["entries"][0]["key"], c.scanner.to_string());
    for bad in [
        format!("/api/v1/analytics/activity?from={to}&to={from}"),
        "/api/v1/analytics/activity?from=2026-01-01T00:00:00Z&to=2026-03-01T00:00:00Z".into(),
        format!("/api/v1/analytics/top?from={from}&to={to}&limit=500"),
        format!("/api/v1/analytics/top?from={from}&to={to}&by=password"),
    ] {
        let (s, _) = c.call(Method::GET, &bad, "viewer", None).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{bad}");
    }
    // Daily buckets over 60 days are fine.
    let (s, a) = c
        .call(
            Method::GET,
            "/api/v1/analytics/activity?bucket=day&from=2026-10-01T00:00:00Z&to=2026-11-30T00:00:00Z",
            "viewer",
            None,
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(a["points"].as_array().unwrap().len(), 60);
    // Another site's user sees nothing.
    let other = c.issuer.token(
        "elsewhere",
        600,
        json!({ "tenant_id": c.tenant.to_string(), "roles": ["viewer"], "sites": [SiteId::new().to_string()] }),
    );
    let req = Request::builder()
        .uri(format!("/api/v1/analytics/top?from={from}&to={to}"))
        .header("authorization", format!("Bearer {other}"))
        .body(Body::empty())
        .unwrap();
    let resp = c.app.clone().oneshot(req).await.unwrap();
    let body: Value =
        serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert!(body["entries"].as_array().unwrap().is_empty(), "{body}");
}

#[tokio::test]
async fn retention_sweeps_respect_holds_and_audit() {
    let c = ctx().await;
    let recordings = RecordingStore::open(&StorageConfig::Memory).unwrap();
    let now = platform_common::now_utc();
    let old = now - time::Duration::days(40);
    let ids = [
        c.transmission(old, "100", 2000).await,
        c.transmission(old, "100", 2000).await,
        c.transmission(now, "100", 2000).await,
    ];
    for (i, id) in ids.iter().enumerate() {
        let key = format!("rec/{i}.opus");
        recordings.put(&key, vec![1, 2, 3]).await.unwrap();
        c.store
            .set_transmission_audio(c.tenant, *id, &key, false, now)
            .await
            .unwrap();
    }
    c.store
        .set_transmission_hold(c.tenant, ids[1], true)
        .await
        .unwrap();
    let r = Retention::new(RetentionConfig {
        recordings_days: Some(30),
        transmissions_days: Some(35),
        ..RetentionConfig::default()
    });
    let rep = r.sweep(&*c.store, Some(&recordings), now).await;
    assert_eq!(
        (rep.recordings, rep.transmissions, rep.errors),
        (1, 1, 0),
        "{rep:?}"
    );
    assert!(recordings.get("rec/0.opus").await.is_err());
    assert!(recordings.get("rec/1.opus").await.is_ok(), "held");
    assert!(recordings.get("rec/2.opus").await.is_ok(), "recent");
    assert!(
        c.store.get_transmission(c.tenant, ids[0]).await.is_err(),
        "row deleted after its recording"
    );
    assert!(c.store.get_transmission(c.tenant, ids[1]).await.is_ok());
    // A second sweep finds nothing.
    let rep = r.sweep(&*c.store, Some(&recordings), now).await;
    assert_eq!(rep.recordings + rep.transmissions, 0);
    let audit = c.audit_actions().await;
    assert_eq!(
        audit
            .iter()
            .filter(|a| a.0 == "system:retention" && a.1 == "retention.sweep")
            .count(),
        1,
        "one audit record per tenant per sweep that removed something"
    );
    // Status: policy for everyone, sweep totals for auditors only.
    let _ = c.state.retention.set(Arc::new(r));
    let (_, st) = c
        .call(Method::GET, "/api/v1/retention", "viewer", None)
        .await;
    assert_eq!(st["recordings_days"], 30);
    assert_eq!(st["last_sweep"], Value::Null);
    let (_, st) = c
        .call(Method::GET, "/api/v1/retention", "admin", None)
        .await;
    assert!(st["last_sweep"]["at"].is_string());
}

#[tokio::test]
async fn worker_transcribes_retries_and_gives_up() {
    let c = ctx().await;
    let cancel = CancellationToken::new();
    let stub = sds_simulator::whisper::start("127.0.0.1:0".parse().unwrap(), cancel.clone())
        .await
        .unwrap();
    let recordings = RecordingStore::open(&StorageConfig::Memory).unwrap();
    let now = platform_common::now_utc();
    let good = c.transmission(now, "100", 1500).await;
    let short = c.transmission(now, "100", 200).await;
    recordings
        .put("rec/good.opus", tone_recording(620.0, 1500))
        .await
        .unwrap();
    recordings
        .put("rec/short.opus", tone_recording(620.0, 200))
        .await
        .unwrap();
    c.store
        .set_transmission_audio(c.tenant, good, "rec/good.opus", true, now)
        .await
        .unwrap();
    c.store
        .set_transmission_audio(c.tenant, short, "rec/short.opus", true, now)
        .await
        .unwrap();

    let t = Arc::new(Transcription::new(transcription_cfg(&stub.url(), 1)).unwrap());
    let wake = Arc::new(tokio::sync::Notify::new());
    tokio::spawn(Arc::clone(&t).run(
        Arc::clone(&c.store) as Arc<dyn Store>,
        recordings.clone(),
        Arc::clone(&wake),
        cancel.clone(),
    ));
    let wait_state = |id: TransmissionId, want: TranscriptionState| {
        let store = Arc::clone(&c.store);
        let tenant = c.tenant;
        async move {
            for _ in 0..200 {
                let r = store.get_transmission(tenant, id).await.unwrap();
                if r.transcription_state == Some(want) {
                    return r;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            panic!("{id} never reached {want:?}");
        }
    };
    let r = wait_state(good, TranscriptionState::Done).await;
    let text = r.transcript.unwrap();
    assert!(
        text.starts_with("Simulated transcript: 620 hertz tone"),
        "{text}"
    );
    assert_eq!(r.transcript_language.as_deref(), Some("en"));
    assert_eq!(r.transcript_model.as_deref(), Some("whisper-1"));
    let r = wait_state(short, TranscriptionState::Skipped).await;
    assert!(r.transcription_error.unwrap().contains("shorter than"));
    assert_eq!(
        stub.state.requests.load(Ordering::Relaxed),
        1,
        "short clips are never sent"
    );

    // A transient failure with attempts left is retried later, not now.
    cancel.cancel(); // stop the first worker (max_attempts = 1)
    tokio::time::sleep(Duration::from_millis(50)).await;
    let cancel2 = CancellationToken::new();
    let stub2 = sds_simulator::whisper::start("127.0.0.1:0".parse().unwrap(), cancel2.clone())
        .await
        .unwrap();
    stub2.state.fail_next.store(1, Ordering::Relaxed);
    let retry = c.transmission(now, "200", 1500).await;
    recordings
        .put("rec/retry.opus", tone_recording(400.0, 1500))
        .await
        .unwrap();
    c.store
        .set_transmission_audio(c.tenant, retry, "rec/retry.opus", true, now)
        .await
        .unwrap();
    let t2 = Arc::new(Transcription::new(transcription_cfg(&stub2.url(), 3)).unwrap());
    tokio::spawn(Arc::clone(&t2).run(
        Arc::clone(&c.store) as Arc<dyn Store>,
        recordings.clone(),
        Arc::new(tokio::sync::Notify::new()),
        cancel2.clone(),
    ));
    for _ in 0..200 {
        if t2.stats().retried == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(t2.stats().retried, 1);
    let r = c.store.get_transmission(c.tenant, retry).await.unwrap();
    assert_eq!(r.transcription_state, Some(TranscriptionState::Pending));
    assert!(r.transcription_error.unwrap().contains("503"));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        stub2.state.requests.load(Ordering::Relaxed),
        1,
        "backoff: no immediate retry"
    );
    cancel2.cancel();
    tokio::time::sleep(Duration::from_millis(50)).await;

    // With no attempts left, a transient failure is final.
    let t3 = Transcription::new(transcription_cfg("http://127.0.0.1:9", 1)).unwrap();
    let last = c.transmission(now, "300", 1500).await;
    recordings
        .put("rec/last.opus", tone_recording(400.0, 1500))
        .await
        .unwrap();
    c.store
        .set_transmission_audio(c.tenant, last, "rec/last.opus", true, now)
        .await
        .unwrap();
    let cancel3 = CancellationToken::new();
    let t3 = Arc::new(t3);
    tokio::spawn(Arc::clone(&t3).run(
        Arc::clone(&c.store) as Arc<dyn Store>,
        recordings,
        Arc::new(tokio::sync::Notify::new()),
        cancel3.clone(),
    ));
    let r = wait_state(last, TranscriptionState::Failed).await;
    assert!(r.transcription_error.unwrap().contains("after 1 attempts"));
    cancel3.cancel();
}
