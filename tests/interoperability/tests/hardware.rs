//! Hardware interoperability tests against a real SDS200.
//!
//! All tests are `#[ignore]`d so they never run (or silently pass) in CI.
//! Run them explicitly:
//!
//! ```text
//! SDS200_ADDR=192.168.1.50:50536 cargo test -p sds-interoperability -- --ignored --test-threads=1 --nocapture
//! ```
//!
//! Tests that change scanner state additionally require
//! `SDS200_ALLOW_STATE_CHANGE=1`; they restore the original values.
//! Record results in `docs/hardware-testing.md`.

// Test code: panicking on failure is the intended behaviour.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::time::Duration;

use uniden_sds::protocol::{ListRequest, Volume};
use uniden_sds::{Scanner, ScannerConfig};

fn address() -> std::net::SocketAddr {
    let raw = std::env::var("SDS200_ADDR")
        .unwrap_or_else(|_| panic!("set SDS200_ADDR=<ip>:<port> to run hardware tests"));
    raw.parse()
        .unwrap_or_else(|e| panic!("invalid SDS200_ADDR `{raw}`: {e}"))
}

fn state_change_allowed() -> bool {
    std::env::var("SDS200_ALLOW_STATE_CHANGE").is_ok_and(|v| v == "1")
}

async fn connect() -> Scanner {
    let mut c = ScannerConfig::new(address());
    c.xml_timeout = Duration::from_secs(6);
    Scanner::connect(c).await.expect("socket")
}

#[tokio::test]
#[ignore = "requires SDS200 hardware (SDS200_ADDR)"]
async fn hw_identity() {
    let s = connect().await;
    let model = s.model().await.expect("MDL");
    let fw = s.firmware_version().await.expect("VER");
    println!("MDL={model} VER={fw} components={:?}", fw.components);
    assert!(model.is_sds200(), "unexpected model {model}");
}

#[tokio::test]
#[ignore = "requires SDS200 hardware (SDS200_ADDR)"]
async fn hw_capability_probe() {
    let s = connect().await;
    let caps = s.probe_capabilities().await;
    for (cmd, support) in &caps.commands {
        println!("{cmd:8} {support:?}");
    }
    assert!(caps.model.is_some());
}

#[tokio::test]
#[ignore = "requires SDS200 hardware (SDS200_ADDR)"]
async fn hw_scanner_info() {
    let s = connect().await;
    let info = s.scanner_info().await.expect("GSI");
    println!("mode={:?} v_screen={:?}", info.mode, info.v_screen);
    println!(
        "system={:?}",
        info.system.as_ref().and_then(|i| i.name.clone())
    );
    println!(
        "department={:?}",
        info.department.as_ref().and_then(|i| i.name.clone())
    );
    println!("channel={:?}", info.channel);
    println!(
        "frequency={:?} tgid={:?} uid={:?}",
        info.active_frequency(),
        info.active_tgid(),
        info.unit_id
    );
    println!("property={:?}", info.property);
    let names: Vec<&str> = info.raw.children.iter().map(|c| c.name.as_str()).collect();
    println!("elements={names:?}");
    assert!(info.mode.is_some());
}

#[tokio::test]
#[ignore = "requires SDS200 hardware (SDS200_ADDR)"]
async fn hw_favorites_list() {
    let s = connect().await;
    let list = s.list(ListRequest::FavoritesLists).await.expect("GLT,FL");
    println!("favorites lists: {}", list.entries.len());
    for e in list.entries.iter().take(5) {
        println!("  {:?} {:?}", e.index, e.name);
    }
}

#[tokio::test]
#[ignore = "requires SDS200 hardware (SDS200_ADDR)"]
async fn hw_read_levels() {
    let s = connect().await;
    println!("VOL -> {:?}", s.volume().await);
    println!("SQL -> {:?}", s.squelch().await);
}

#[tokio::test]
#[ignore = "requires SDS200 hardware and SDS200_ALLOW_STATE_CHANGE=1"]
async fn hw_set_volume_and_restore() {
    assert!(
        state_change_allowed(),
        "set SDS200_ALLOW_STATE_CHANGE=1 to run state-changing tests"
    );
    let s = connect().await;
    let original = s.volume().await.expect("VOL read");
    let probe = Volume::new(if original.get() == 0 {
        1
    } else {
        original.get() - 1
    })
    .expect("level");
    s.set_volume(probe).await.expect("VOL set");
    let read_back = s.volume().await.expect("VOL read back");
    s.set_volume(original).await.expect("VOL restore");
    assert_eq!(read_back, probe);
}

#[tokio::test]
#[ignore = "requires SDS200 hardware (SDS200_ADDR)"]
async fn hw_push_updates() {
    assert!(
        state_change_allowed(),
        "PSI changes push state; set SDS200_ALLOW_STATE_CHANGE=1"
    );
    let s = connect().await;
    let mut rx = s.subscribe();
    s.start_push(500).await.expect("PSI");
    let mut received = 0;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        if let Ok(Ok(_)) = tokio::time::timeout(Duration::from_secs(1), rx.recv()).await {
            received += 1;
        }
    }
    s.stop_push().await.expect("PSI,0");
    println!("PSI pushes in 5 s: {received}");
    assert!(received > 0);
}
