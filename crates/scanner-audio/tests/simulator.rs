//! The audio client against the simulated SDS200 audio server.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::time::Duration;

use scanner_audio::{CapturedFrame, RtpStats, SessionConfig, SessionEnd, run_session};
use sds_simulator::audio::{AudioFaults, tone_hz};
use sds_simulator::{Simulator, SimulatorConfig, Transmission};
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

async fn sim() -> Simulator {
    Simulator::start(SimulatorConfig {
        audio: Some("127.0.0.1:0".parse().unwrap()),
        ..SimulatorConfig::default()
    })
    .await
    .unwrap()
}

/// Dominant frequency of 48 kHz PCM via zero crossings.
fn freq(pcm: &[i16]) -> f64 {
    let crossings = pcm.windows(2).filter(|w| (w[0] < 0) != (w[1] < 0)).count();
    crossings as f64 / 2.0 / (pcm.len() as f64 / 48_000.0)
}

struct Running {
    rx: mpsc::Receiver<CapturedFrame>,
    stats: watch::Receiver<RtpStats>,
    stop: CancellationToken,
    task: tokio::task::JoinHandle<SessionEnd>,
}

fn start(sim: &Simulator) -> Running {
    let cfg = SessionConfig::new(sim.rtsp_addr().unwrap());
    let (tx, rx) = mpsc::channel(1024);
    let (stx, stats) = watch::channel(RtpStats::default());
    let stop = CancellationToken::new();
    let s = stop.clone();
    let task = tokio::spawn(async move { run_session(&cfg, &tx, &stx, &s).await });
    Running {
        rx,
        stats,
        stop,
        task,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn streams_tone_and_always_tears_down() {
    let sim = sim().await;
    sim.begin_transmission(Transmission {
        channel: "FD Main".into(),
        ..Transmission::default()
    });
    let mut r = start(&sim);
    let mut dec = opus::Decoder::new(48_000, opus::Channels::Mono).unwrap();
    let mut pcm = Vec::new();
    for _ in 0..30 {
        let f = tokio::time::timeout(Duration::from_secs(5), r.rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(!f.frame.concealed);
        let mut out = vec![0i16; 960];
        let n = dec.decode(&f.frame.data, &mut out, false).unwrap();
        pcm.extend_from_slice(&out[..n]);
    }
    let expected = tone_hz("FD Main");
    let got = freq(&pcm[960 * 10..]);
    assert!(
        (got - expected).abs() < expected * 0.05,
        "tone {got:.0} Hz, expected {expected:.0} Hz"
    );

    r.stop.cancel();
    assert_eq!(r.task.await.unwrap(), SessionEnd::Stopped);
    let st = sim.audio_stats();
    assert_eq!(
        st.requests,
        vec!["DESCRIBE", "SETUP", "PLAY", "TEARDOWN"],
        "exact request order"
    );
    assert_eq!(
        (st.sessions, st.teardowns, st.lockups, st.active),
        (1, 1, 0, false)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn second_client_is_refused_and_still_torn_down_cleanly() {
    let sim = sim().await;
    let mut first = start(&sim);
    first.rx.recv().await.unwrap();
    let second = start(&sim);
    match second.task.await.unwrap() {
        SessionEnd::Failed(e) => assert!(e.contains("453"), "{e}"),
        other => panic!("{other:?}"),
    }
    first.stop.cancel();
    first.task.await.unwrap();
    let st = sim.audio_stats();
    assert_eq!((st.rejected, st.lockups, st.locked), (1, 0, false));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn loss_and_reordering_are_concealed_and_counted() {
    let sim = sim().await;
    sim.set_audio_faults(AudioFaults {
        loss: 0.1,
        reorder: 0.1,
    });
    let mut r = start(&sim);
    let mut frames = 0;
    let mut concealed = 0;
    while frames < 100 {
        let f = tokio::time::timeout(Duration::from_secs(5), r.rx.recv())
            .await
            .unwrap()
            .unwrap();
        frames += 1;
        concealed += usize::from(f.frame.concealed);
    }
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let s = *r.stats.borrow();
    assert!(s.lost > 0 && s.reordered > 0, "{s:?}");
    assert!(
        concealed > 0,
        "lost packets become concealed frames, keeping timing"
    );
    r.stop.cancel();
    r.task.await.unwrap();
    assert_eq!(sim.audio_stats().teardowns, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_locked_server_fails_by_timeout_not_hang() {
    let sim = sim().await;
    // Lock the simulated server: a raw client sets up and vanishes.
    {
        use tokio::io::AsyncWriteExt;
        let mut t = tokio::net::TcpStream::connect(sim.rtsp_addr().unwrap())
            .await
            .unwrap();
        t.write_all(b"SETUP rtsp://x/au:scanner.au/trackID=1 RTSP/1.0\r\nCSeq: 1\r\nTransport: RTP/AVP;unicast;client_port=9\r\n\r\n").await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(sim.audio_stats().locked, "simulated fragility");
    let r = start(&sim);
    let end = tokio::time::timeout(Duration::from_secs(10), r.task)
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(end, SessionEnd::Failed(ref e) if e.contains("timed out")),
        "{end:?}"
    );
    sim.power_cycle_audio();
    let mut r = start(&sim);
    assert!(
        tokio::time::timeout(Duration::from_secs(5), r.rx.recv())
            .await
            .unwrap()
            .is_some()
    );
    r.stop.cancel();
}
