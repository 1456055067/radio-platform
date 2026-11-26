//! A str0m client plays the browser: it offers a receive-only audio
//! transceiver, connects over real UDP, and must receive the published
//! Opus frames unchanged.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use media_gateway::{MediaHub, WebRtcConfig, WebRtcError, WebRtcServer};
use platform_common::wire::media::MediaFrame;
use platform_common::{AgentId, ScannerId};
use str0m::change::SdpAnswer;
use str0m::media::{Direction, MediaKind};
use str0m::net::{Protocol, Receive};
use str0m::{Candidate, Event, Input, Output, RtcConfig};
use tokio::net::UdpSocket;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn browser_receives_opus_frames() {
    let (hub, mut demand) = MediaHub::new();
    let server = WebRtcServer::start(
        WebRtcConfig::new("127.0.0.1:0".parse().unwrap()),
        hub.clone(),
    )
    .await
    .unwrap();
    let scanner = ScannerId::new();
    let agent = AgentId::new("agent-a").unwrap();

    // Client side.
    let crypto = Arc::new(str0m::crypto::from_feature_flags());
    let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let local = sock.local_addr().unwrap();
    let mut rtc = RtcConfig::new()
        .set_crypto_provider(crypto)
        .build(Instant::now());
    rtc.add_local_candidate(Candidate::host(local, "udp").unwrap());
    let mut api = rtc.sdp_api();
    api.add_media(MediaKind::Audio, Direction::RecvOnly, None, None, None);
    let (offer, pending) = api.apply().unwrap();

    assert!(matches!(
        server.create(scanner, &agent, "not sdp").await,
        Err(WebRtcError::Offer(_))
    ));
    let (_id, answer) = server
        .create(scanner, &agent, &offer.to_sdp_string())
        .await
        .unwrap();
    assert!(answer.contains("opus"), "{answer}");
    assert!(!answer.to_ascii_lowercase().contains("vp8"), "audio only");
    rtc.sdp_api()
        .accept_answer(pending, SdpAnswer::from_sdp_string(&answer).unwrap())
        .unwrap();
    assert_eq!(
        demand.recv().await.map(|d| d.listen),
        Some(true),
        "listening raises demand"
    );
    assert_eq!(server.sessions(), 1);

    // Publisher: 20 ms frames with recognisable payloads.
    let pub_hub = hub.clone();
    let publisher = tokio::spawn(async move {
        for seq in 0..400u32 {
            pub_hub.publish(MediaFrame {
                scanner_id: scanner,
                captured_ms: 0,
                seq,
                concealed: false,
                opus: vec![0xF8, (seq % 251) as u8, 0x55],
            });
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    });

    let mut buf = vec![0u8; 2000];
    let mut got: Vec<Vec<u8>> = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(8);
    while got.len() < 20 && Instant::now() < deadline {
        let timeout = loop {
            match rtc.poll_output().unwrap() {
                Output::Timeout(t) => break t,
                Output::Transmit(t) => {
                    sock.send_to(&t.contents, t.destination).await.unwrap();
                }
                Output::Event(Event::MediaData(m)) => got.push(m.data.to_vec()),
                Output::Event(_) => {}
            }
        };
        let wait = timeout
            .saturating_duration_since(Instant::now())
            .max(Duration::from_millis(1));
        match tokio::time::timeout(wait, sock.recv_from(&mut buf)).await {
            Ok(Ok((n, source))) => {
                let input = Input::Receive(
                    Instant::now(),
                    Receive {
                        proto: Protocol::Udp,
                        source,
                        destination: local,
                        contents: buf[..n].try_into().unwrap(),
                    },
                );
                rtc.handle_input(input).unwrap();
            }
            _ => rtc.handle_input(Input::Timeout(Instant::now())).unwrap(),
        }
    }
    publisher.abort();
    assert!(got.len() >= 20, "received {} frames", got.len());
    assert!(
        got.iter()
            .all(|f| f.len() == 3 && f[0] == 0xF8 && f[2] == 0x55),
        "frames arrive unchanged"
    );
    // Consecutive frames, in order.
    let seqs: Vec<u8> = got.iter().map(|f| f[1]).collect();
    assert!(
        seqs.windows(2)
            .filter(|w| w[1] == w[0].wrapping_add(1) || (w[0] == 250 && w[1] == 0))
            .count()
            >= seqs.len() - 3,
        "{seqs:?}"
    );

    // Hanging up releases the demand.
    rtc.disconnect();
    for _ in 0..20 {
        while let Ok(Output::Transmit(t)) = rtc.poll_output() {
            let _ = sock.send_to(&t.contents, t.destination).await;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    server.close(_id);
    let d = tokio::time::timeout(Duration::from_secs(5), demand.recv())
        .await
        .unwrap();
    assert_eq!(d.map(|d| d.listen), Some(false));
}

#[tokio::test]
async fn session_limit() {
    let (hub, _demand) = MediaHub::new();
    let mut cfg = WebRtcConfig::new("127.0.0.1:0".parse().unwrap());
    cfg.max_sessions = 1;
    let server = WebRtcServer::start(cfg, hub).await.unwrap();
    let offer = || {
        let mut rtc = RtcConfig::new().build(Instant::now());
        let mut api = rtc.sdp_api();
        api.add_media(MediaKind::Audio, Direction::RecvOnly, None, None, None);
        api.apply().unwrap().0.to_sdp_string()
    };
    let a = AgentId::new("a").unwrap();
    assert!(server.create(ScannerId::new(), &a, &offer()).await.is_ok());
    assert_eq!(
        server.create(ScannerId::new(), &a, &offer()).await,
        Err(WebRtcError::Busy)
    );
}

#[tokio::test]
async fn wildcard_bind_requires_public_address() {
    let (hub, _d) = MediaHub::new();
    assert!(
        WebRtcServer::start(WebRtcConfig::new("0.0.0.0:0".parse().unwrap()), hub)
            .await
            .is_err()
    );
}
