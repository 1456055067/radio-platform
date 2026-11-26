//! Registry tests against the simulator.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::time::Duration;

use platform_common::wire::ScannerAssignment;
use platform_common::{CommandError, HoldKind, NetworkPolicy, ScannerCommand, ScannerId};
use scanner_controller::{ApplyResult, RegistryConfig, ScannerRegistry};
use scanner_telemetry::{CollectionMode, CollectorConfig};
use sds_simulator::{Simulator, SimulatorConfig};
use tokio::sync::mpsc;

fn policy_for(sim: &Simulator) -> NetworkPolicy {
    NetworkPolicy {
        allowed_networks: vec!["127.0.0.1/32".parse().unwrap()],
        allowed_ports: vec![sim.addr().port()],
        audio_ports: vec![554],
    }
}

fn assignment(sim: &Simulator, id: ScannerId) -> ScannerAssignment {
    ScannerAssignment {
        scanner_id: id,
        site_id: None,
        name: "sim".into(),
        host: sim.addr().ip(),
        port: sim.addr().port(),
        enabled: true,
        audio: None,
    }
}

fn config(policy: NetworkPolicy, max: usize) -> RegistryConfig {
    RegistryConfig {
        policy,
        max_scanners: max,
        collector: CollectorConfig {
            mode: CollectionMode::Poll {
                interval: Duration::from_millis(200),
            },
            ..CollectorConfig::default()
        },
        sdk: None,
    }
}

#[tokio::test]
async fn reconcile_execute_and_stop() {
    let sim = Simulator::start(SimulatorConfig::default()).await.unwrap();
    let (tx, mut rx) = mpsc::channel(64);
    let mut reg = ScannerRegistry::new(config(policy_for(&sim), 4), tx);
    let id = ScannerId::new();
    assert_eq!(
        reg.apply(vec![assignment(&sim, id)]).await,
        vec![ApplyResult::Started(id)]
    );
    assert_eq!(
        reg.apply(vec![assignment(&sim, id)]).await,
        vec![ApplyResult::Unchanged(id)]
    );
    let r = &reg.reports()[0];
    assert_eq!(r.model.as_deref(), Some("SDS200"));

    let v = reg
        .execute(id, &ScannerCommand::SetVolume { level: 21 })
        .await
        .unwrap();
    assert_eq!(v["ok"], true);
    assert_eq!(sim.state().volume, 21);
    let v = reg.execute(id, &ScannerCommand::GetVolume).await.unwrap();
    assert_eq!(v["level"], 21);
    reg.execute(
        id,
        &ScannerCommand::Hold {
            kind: HoldKind::System,
            index: 100,
        },
    )
    .await
    .unwrap();
    assert!(sim.state().hold);
    reg.execute(id, &ScannerCommand::Resume).await.unwrap();
    assert!(!sim.state().hold);
    let status = reg.execute(id, &ScannerCommand::GetStatus).await.unwrap();
    assert_eq!(status["volume"], 21);
    let caps = reg
        .execute(id, &ScannerCommand::ProbeCapabilities)
        .await
        .unwrap();
    assert_eq!(caps["commands"]["GSI"]["status"], "supported");
    let raw = reg
        .execute(
            id,
            &ScannerCommand::Raw {
                name: "MDL".into(),
                args: vec![],
                read_only: true,
            },
        )
        .await
        .unwrap();
    assert_eq!(raw["fields"][0], "SDS200");

    assert!(matches!(
        reg.execute(id, &ScannerCommand::SetVolume { level: 99 })
            .await,
        Err(CommandError::Invalid { .. })
    ));
    assert!(matches!(
        reg.execute(
            id,
            &ScannerCommand::Raw {
                name: "bad".into(),
                args: vec![],
                read_only: true
            }
        )
        .await,
        Err(CommandError::Invalid { .. })
    ));
    assert_eq!(
        reg.execute(ScannerId::new(), &ScannerCommand::GetVolume)
            .await,
        Err(CommandError::ScannerNotFound)
    );

    // Telemetry flows from the collector.
    let got = tokio::time::timeout(Duration::from_secs(3), rx.recv())
        .await
        .unwrap();
    assert!(got.is_some());

    assert_eq!(reg.apply(vec![]).await, vec![ApplyResult::Stopped(id)]);
    assert!(reg.is_empty());
}

#[tokio::test]
async fn policy_and_limits_are_enforced() {
    let sim = Simulator::start(SimulatorConfig::default()).await.unwrap();
    let (tx, _rx) = mpsc::channel(64);
    // Default policy denies everything, including loopback.
    let mut reg = ScannerRegistry::new(config(NetworkPolicy::default(), 4), tx.clone());
    let id = ScannerId::new();
    let res = reg.apply(vec![assignment(&sim, id)]).await;
    assert!(matches!(&res[..], [ApplyResult::Refused(i, _)] if *i == id));
    assert!(
        sim.received().is_empty(),
        "no packet may be sent to a refused endpoint"
    );

    let mut reg = ScannerRegistry::new(config(policy_for(&sim), 1), tx);
    let res = reg
        .apply(vec![
            assignment(&sim, ScannerId::new()),
            assignment(&sim, ScannerId::new()),
        ])
        .await;
    assert_eq!(
        res.iter()
            .filter(|r| matches!(r, ApplyResult::Started(_)))
            .count(),
        1
    );
    assert_eq!(
        res.iter()
            .filter(|r| matches!(r, ApplyResult::Refused(..)))
            .count(),
        1
    );
    reg.shutdown().await;
}
