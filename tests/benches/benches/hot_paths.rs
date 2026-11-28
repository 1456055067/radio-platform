//! Hot-path benchmarks. Run with `cargo bench -p radio-benches`; results
//! and the machine they came from are recorded in docs/benchmarks.md.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, missing_docs)]

use std::hint::black_box;
use std::sync::Arc;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};

fn protocol(c: &mut Criterion) {
    use uniden_protocol::xml::XmlLimits;
    use uniden_protocol::{Command, FrameAssembler, FrameLimits, decode_reply};
    let xml = sds_simulator::scanner_info_xml(&sds_simulator::State::default());
    let datagram = format!("GSI,<XML>,\r{xml}").into_bytes();
    let mut g = c.benchmark_group("protocol");
    g.throughput(Throughput::Bytes(datagram.len() as u64));
    g.bench_function("gsi_frame_and_decode", |b| {
        b.iter(|| {
            let mut fa = FrameAssembler::new(FrameLimits::default());
            let frames = fa.push(black_box(&datagram)).unwrap();
            for f in frames {
                black_box(
                    decode_reply(&Command::GetScannerInfo, f, &XmlLimits::default()).unwrap(),
                );
            }
        });
    });
    g.finish();
}

fn audio(c: &mut Criterion) {
    let mut g = c.benchmark_group("audio");
    // One 20 ms RTP payload of µ-law (160 samples at 8 kHz).
    let ulaw: Vec<u8> = (0..160u8).map(|i| i.wrapping_mul(37)).collect();
    g.bench_function("ulaw_decode_20ms", |b| {
        let mut out = Vec::with_capacity(160);
        b.iter(|| {
            out.clear();
            scanner_audio::g711::decode_into(black_box(&ulaw), &mut out);
            black_box(&out);
        });
    });
    #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
    let pcm: Vec<i16> = (0..160)
        .map(|i| ((i as f64 * 0.3).sin() * 9000.0) as i16)
        .collect();
    let mut enc = scanner_audio::opus_enc::Encoder::new().unwrap();
    g.bench_function("opus_encode_20ms", |b| {
        b.iter(|| black_box(enc.push(black_box(&pcm)).unwrap()));
    });
    let frame = platform_common::wire::media::MediaFrame {
        scanner_id: platform_common::ScannerId::new(),
        captured_ms: 1,
        seq: 1,
        concealed: false,
        opus: vec![7; 40],
    };
    let bytes = platform_common::wire::media::encode(&frame);
    g.bench_function("media_frame_decode", |b| {
        b.iter(|| black_box(platform_common::wire::media::decode(black_box(&bytes)).unwrap()));
    });
    // A 10 s recording, decoded for transcription.
    let mut enc = scanner_audio::opus_enc::Encoder::new().unwrap();
    let mut w = scanner_audio::ogg::OggOpusWriter::new(1, enc.pre_skip(), 8000, "bench", &[]);
    #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
    let tone: Vec<i16> = (0..80_000)
        .map(|i| ((i as f64 * 0.4).sin() * 9000.0) as i16)
        .collect();
    for f in enc.push(&tone).unwrap() {
        w.push(&f.data, scanner_audio::FRAME_48K);
    }
    let ogg = w.finish();
    g.sample_size(20);
    g.bench_function("recording_10s_decode_to_16k", |b| {
        b.iter(|| black_box(scanner_transcription::decode_ogg_opus(black_box(&ogg), 60).unwrap()));
    });
    g.finish();
}

fn audit(c: &mut Criterion) {
    use platform_storage::audit_chain;
    use platform_storage::model::{AuditOutcome, AuditRecord};
    let r = AuditRecord {
        id: 1,
        at: audit_chain::micros(platform_common::now_utc()),
        tenant_id: platform_common::TenantId::new(),
        actor: "alice@example.org".into(),
        action: "scanner.command.set_volume".into(),
        resource_type: "scanner".into(),
        resource_id: Some(uuid::Uuid::new_v4().to_string()),
        outcome: AuditOutcome::Success,
        details: serde_json::json!({ "level": 7, "lease": uuid::Uuid::new_v4() }),
        source_ip: Some("192.0.2.10".parse().unwrap()),
        seq: Some(1),
        prev_hash: None,
        hash: None,
    };
    c.bench_function("audit/chain_link", |b| {
        b.iter(|| black_box(audit_chain::link(&audit_chain::GENESIS, black_box(&r), 1)));
    });
}

/// The full public middleware stack (rate limits, JWT verification, ABAC,
/// metrics) over the in-memory store: `GET /api/v1/scanners` with 50
/// scanners.
fn api(c: &mut Criterion) {
    use axum::body::Body;
    use axum::http::Request;
    use platform_auth::testing::TestIssuer;
    use platform_auth::{OidcConfig, OidcVerifier, Permission};
    use tower::ServiceExt;
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let issuer = TestIssuer::new("https://idp.example.test", "radio-platform");
    let mut oidc = OidcConfig::new("https://idp.example.test", "radio-platform");
    oidc.role_permissions = [("admin".to_owned(), Permission::ALL.to_vec())].into();
    let limits = api_gateway::config::LimitsConfig {
        requests_per_minute: u32::MAX,
        requests_per_minute_per_ip: u32::MAX,
        ..Default::default()
    };
    let state = api_gateway::AppState::new(
        Arc::new(platform_storage::MemoryStore::new()),
        Arc::new(OidcVerifier::with_static_jwks(oidc, issuer.jwks())),
        limits,
    );
    let app = api_gateway::public_app(state);
    let tenant = uuid::Uuid::new_v4().to_string();
    let tok = issuer.token(
        "a",
        3600,
        serde_json::json!({ "tenant_id": tenant, "roles": ["admin"], "sites": ["*"] }),
    );
    let send = |method: &str, uri: &str, body: Option<serde_json::Value>| {
        let mut b = Request::builder()
            .method(method)
            .uri(uri)
            .header("authorization", format!("Bearer {tok}"));
        if body.is_some() {
            b = b.header("content-type", "application/json");
        }
        b.body(body.map_or_else(Body::empty, |v| Body::from(v.to_string())))
            .unwrap()
    };
    rt.block_on(async {
        let r = app.clone().oneshot(send("POST", "/api/v1/sites", Some(serde_json::json!({ "name": "S", "allowed_networks": ["10.1.0.0/16"] })))).await.unwrap();
        let site: serde_json::Value = serde_json::from_slice(&axum::body::to_bytes(r.into_body(), 1 << 20).await.unwrap()).unwrap();
        app.clone().oneshot(send("POST", "/api/v1/agents", Some(serde_json::json!({ "agent_id": "agent-a", "site_id": site["id"] })))).await.unwrap();
        for i in 0..50 {
            app.clone()
                .oneshot(send("POST", "/api/v1/scanners", Some(serde_json::json!({ "site_id": site["id"], "agent_id": "agent-a", "name": format!("S{i}"), "host": format!("10.1.0.{}", i + 10) }))))
                .await
                .unwrap();
        }
    });
    c.bench_function("api/list_scanners_50", |b| {
        b.to_async(&rt).iter(|| async {
            let r = app
                .clone()
                .oneshot(send("GET", "/api/v1/scanners?limit=50", None))
                .await
                .unwrap();
            assert!(r.status().is_success());
            black_box(axum::body::to_bytes(r.into_body(), 1 << 22).await.unwrap());
        });
    });
}

/// The security-log throttle on the rejection path, with no subscriber
/// installed (the decision cost only; writing is the subscriber's cost).
fn security(c: &mut Criterion) {
    use api_gateway::security_log::{Listener, Reason, SecurityEvent, SecurityLog};
    use std::net::{IpAddr, Ipv4Addr};
    let ev = |ip: IpAddr| SecurityEvent {
        reason: Reason::Malformed,
        claim: None,
        status: 401,
        ip: Some(ip),
        method: &axum::http::Method::GET,
        route: "/api/v1/scanners",
        listener: Listener::Public,
        subject: None,
        tenant: None,
        agent: None,
    };

    // One source past its per-source limit: every event is suppressed.
    let log = SecurityLog::new();
    let one = ev(IpAddr::V4(Ipv4Addr::new(198, 51, 100, 1)));
    for _ in 0..20 {
        log.record(&one);
    }
    c.bench_function("security/record_one_source_suppressed", |b| {
        b.iter(|| log.record(black_box(&one)));
    });

    // 10 000 sources cycling through a full 4096-slot table: most events are
    // untracked, the replica-wide budget is spent.
    let log = SecurityLog::new();
    let sources: Vec<IpAddr> = (0..10_000u32)
        .map(|i| IpAddr::V4(Ipv4Addr::from(0x0a00_0000 + i)))
        .collect();
    for ip in &sources {
        log.record(&ev(*ip));
    }
    let mut i = 0usize;
    c.bench_function("security/record_many_sources_full_table", |b| {
        b.iter(|| {
            i = (i + 1) % sources.len();
            log.record(black_box(&ev(sources[i])));
        });
    });
}

criterion_group!(benches, protocol, audio, audit, api, security);
criterion_main!(benches);
