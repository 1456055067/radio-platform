//! Hardware audio tests (RTSP/RTP) against a real SDS200.
//!
//! The scanner's audio server is **reported** to lock up until power-cycled
//! after a missing TEARDOWN or a quick reconnect (docs/protocol-compliance.md,
//! "Audio"). These tests are therefore doubly opt-in:
//!
//! ```text
//! SDS200_ADDR=192.168.1.50:50536 SDS200_ALLOW_AUDIO=1 \
//!   cargo test -p sds-interoperability --test audio -- --ignored --test-threads=1 --nocapture
//! ```
//!
//! `SDS200_RTSP` overrides the RTSP endpoint (default: the scanner's IP on
//! port 554). Stop every other audio client (ProScan, VLC) first, and be
//! ready to power-cycle the scanner. Record results in
//! docs/hardware-testing.md.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use scanner_audio::{RtpStats, SessionConfig, SessionEnd, run_session};
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

fn rtsp() -> SocketAddr {
    if let Ok(v) = std::env::var("SDS200_RTSP") {
        return v
            .parse()
            .unwrap_or_else(|e| panic!("SDS200_RTSP `{v}`: {e}"));
    }
    let raw = std::env::var("SDS200_ADDR").unwrap_or_else(|_| panic!("set SDS200_ADDR"));
    let a: SocketAddr = raw
        .parse()
        .unwrap_or_else(|e| panic!("SDS200_ADDR `{raw}`: {e}"));
    SocketAddr::new(a.ip(), 554)
}

fn require_opt_in() {
    assert!(
        std::env::var("SDS200_ALLOW_AUDIO").is_ok_and(|v| v == "1"),
        "set SDS200_ALLOW_AUDIO=1: these tests open the scanner's audio server, which may lock up"
    );
}

/// One session of `secs` seconds; returns frames, RTP stats and how it ended.
async fn session(secs: u64) -> (usize, RtpStats, SessionEnd) {
    let cfg = SessionConfig::new(rtsp());
    let (tx, mut rx) = mpsc::channel(4096);
    let (stats_tx, stats_rx) = watch::channel(RtpStats::default());
    let stop = CancellationToken::new();
    let s2 = stop.clone();
    let task = tokio::spawn(async move { run_session(&cfg, &tx, &stats_tx, &s2).await });
    let deadline = Instant::now() + Duration::from_secs(secs);
    let mut frames = 0usize;
    while Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(200), rx.recv()).await {
            Ok(Some(_)) => frames += 1,
            Ok(None) => break,
            Err(_) => {}
        }
    }
    stop.cancel(); // run_session sends TEARDOWN on this path
    let end = task.await.unwrap();
    while rx.try_recv().is_ok() {
        frames += 1;
    }
    let stats = *stats_rx.borrow();
    (frames, stats, end)
}

#[tokio::test]
#[ignore = "requires SDS200 hardware, SDS200_ADDR and SDS200_ALLOW_AUDIO=1"]
async fn hw_audio_session_and_teardown() {
    require_opt_in();
    let (frames, stats, end) = session(10).await;
    println!("frames={frames} stats={stats:?} end={end:?}");
    assert_eq!(
        end,
        SessionEnd::Stopped,
        "session must end cleanly (TEARDOWN sent)"
    );
    // 20 ms frames: about 500 in 10 s. Allow for start-up.
    assert!(frames >= 400, "expected about 500 frames, got {frames}");
    assert!(
        stats.lost * 100 <= stats.received.max(1),
        "more than 1% loss: {stats:?}"
    );
}

/// The open question in protocol-compliance.md: does a 10 s gap between
/// sessions (the agent's default cooldown) avoid the reported lock-up?
#[tokio::test]
#[ignore = "requires SDS200 hardware, SDS200_ADDR and SDS200_ALLOW_AUDIO=1"]
async fn hw_audio_reconnect_after_cooldown() {
    require_opt_in();
    for round in 1..=3 {
        let (frames, stats, end) = session(5).await;
        println!("round {round}: frames={frames} stats={stats:?} end={end:?}");
        assert_eq!(end, SessionEnd::Stopped, "round {round}");
        assert!(
            frames >= 150,
            "round {round}: audio server stopped answering ({frames} frames)"
        );
        tokio::time::sleep(Duration::from_secs(10)).await;
    }
}

/// Audio while the control port is polled: both must keep working.
#[tokio::test]
#[ignore = "requires SDS200 hardware, SDS200_ADDR and SDS200_ALLOW_AUDIO=1"]
async fn hw_audio_with_control_traffic() {
    require_opt_in();
    let addr: SocketAddr = std::env::var("SDS200_ADDR").unwrap().parse().unwrap();
    let scanner = uniden_sds::Scanner::connect(uniden_sds::ScannerConfig::new(addr))
        .await
        .unwrap();
    let poll = tokio::spawn(async move {
        let mut ok = 0;
        for _ in 0..20 {
            if scanner.scanner_info().await.is_ok() {
                ok += 1;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        ok
    });
    let (frames, _, end) = session(10).await;
    let polled = poll.await.unwrap();
    println!("frames={frames} gsi_ok={polled}/20");
    assert_eq!(end, SessionEnd::Stopped);
    assert!(
        frames >= 400,
        "audio degraded under control traffic: {frames}"
    );
    assert!(polled >= 18, "control degraded under audio: {polled}/20");
}
