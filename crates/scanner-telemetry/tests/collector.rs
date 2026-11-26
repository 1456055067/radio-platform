//! Collector tests against the simulator (simulated behaviour only).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::time::Duration;

use platform_common::{ScannerEvent, ScannerHealth, ScannerId};
use scanner_telemetry::{
    CollectionMode, CollectorConfig, CollectorOutput, DetectorConfig, run_collector,
};
use sds_simulator::{Faults, Simulator, SimulatorConfig, Transmission};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uniden_sds::{Scanner, ScannerConfig};

async fn next_event(
    rx: &mut mpsc::Receiver<CollectorOutput>,
    pred: impl Fn(&ScannerEvent) -> bool,
) -> ScannerEvent {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(CollectorOutput::Event(_, e)) = rx.recv().await
                && pred(&e)
            {
                return *e;
            }
        }
    })
    .await
    .expect("event within timeout")
}

async fn run(mode: CollectionMode) {
    let sim = Simulator::start(SimulatorConfig::default()).await.unwrap();
    let mut cfg = ScannerConfig::new(sim.addr());
    cfg.line_timeout = Duration::from_millis(200);
    cfg.xml_timeout = Duration::from_millis(300);
    cfg.retries = 0;
    cfg.unreachable_after = 2;
    let scanner = Scanner::connect(cfg).await.unwrap();
    let (tx, mut rx) = mpsc::channel(64);
    let cancel = CancellationToken::new();
    let id = ScannerId::new();
    let config = CollectorConfig {
        mode,
        detector: DetectorConfig {
            hang_time: Duration::from_millis(300),
        },
        heartbeat: Duration::from_secs(30),
    };
    let task = tokio::spawn(run_collector(scanner, id, None, config, tx, cancel.clone()));

    sim.begin_transmission(Transmission {
        frequency_hz: 154_415_000,
        system: "County".into(),
        department: "Fire".into(),
        channel: "Dispatch".into(),
        modulation: "NFM".into(),
        signal_bars: 3,
        ..Transmission::default()
    });
    let started = next_event(&mut rx, |e| {
        matches!(e, ScannerEvent::TransmissionStarted(_))
    })
    .await;
    let ScannerEvent::TransmissionStarted(start) = started else {
        unreachable!()
    };
    assert_eq!(start.frequency_hz, Some(154_415_000));
    assert_eq!(start.channel.as_deref(), Some("Dispatch"));
    assert_eq!(start.tgid, None);

    sim.end_transmission();
    let ended = next_event(&mut rx, |e| {
        matches!(e, ScannerEvent::TransmissionEnded { .. })
    })
    .await;
    let ScannerEvent::TransmissionEnded {
        transmission_id, ..
    } = ended
    else {
        unreachable!()
    };
    assert_eq!(transmission_id, start.transmission_id);

    sim.set_faults(Faults {
        offline: true,
        ..Faults::default()
    });
    next_event(&mut rx, |e| {
        matches!(
            e,
            ScannerEvent::HealthChanged {
                health: ScannerHealth::Unreachable { .. }
            }
        )
    })
    .await;
    sim.set_faults(Faults::default());
    next_event(&mut rx, |e| {
        matches!(
            e,
            ScannerEvent::HealthChanged {
                health: ScannerHealth::Healthy
            }
        )
    })
    .await;

    cancel.cancel();
    task.await.unwrap();
}

#[tokio::test]
async fn poll_mode_detects_transmissions_and_health() {
    run(CollectionMode::Poll {
        interval: Duration::from_millis(100),
    })
    .await;
}

#[tokio::test]
async fn push_mode_detects_transmissions_and_health() {
    run(CollectionMode::Push { interval_ms: 100 }).await;
}
