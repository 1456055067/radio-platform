//! End-to-end SDK tests against the software simulator.
//!
//! These exercise simulated behaviour only; they are not evidence of
//! hardware compatibility.

// Test code: panicking on failure is the intended behaviour.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::time::Duration;

use sds_simulator::{Faults, Simulator, SimulatorConfig, State, Transmission};
use uniden_sds::protocol::{
    Command, KeyCode, KeyMode, ListRequest, ScannerError, Squelch, Target, Volume,
};
use uniden_sds::{Backoff, Error, Health, Scanner, ScannerConfig, Support};

async fn setup(sim_cfg: SimulatorConfig) -> (Simulator, Scanner) {
    let sim = Simulator::start(sim_cfg).await.unwrap();
    let scanner = Scanner::connect(fast_config(&sim)).await.unwrap();
    (sim, scanner)
}

fn fast_config(sim: &Simulator) -> ScannerConfig {
    let mut c = ScannerConfig::new(sim.addr());
    c.line_timeout = Duration::from_millis(250);
    c.xml_timeout = Duration::from_millis(400);
    c.backoff = Backoff {
        initial: Duration::from_millis(10),
        max: Duration::from_millis(40),
        multiplier: 2,
    };
    c
}

fn count(sim: &Simulator, prefix: &str) -> usize {
    sim.received()
        .iter()
        .filter(|c| c.starts_with(prefix))
        .count()
}

#[tokio::test]
async fn identity_and_levels() {
    let (sim, s) = setup(SimulatorConfig::default()).await;
    assert!(s.model().await.unwrap().is_sds200());
    assert_eq!(
        s.firmware_version().await.unwrap().components,
        Some((1, 26, 1))
    );
    assert_eq!(s.volume().await.unwrap().get(), 10);
    s.set_volume(Volume::new(17).unwrap()).await.unwrap();
    assert_eq!(s.volume().await.unwrap().get(), 17);
    s.set_squelch(Squelch::new(5).unwrap()).await.unwrap();
    assert_eq!(s.squelch().await.unwrap().get(), 5);
    assert_eq!(sim.state().volume, 17);
    assert_eq!(s.health(), Health::Healthy);
    s.shutdown().await;
}

#[tokio::test]
async fn telemetry_from_scanner_info() {
    let (sim, s) = setup(SimulatorConfig::default()).await;
    let idle = s.scanner_info().await.unwrap();
    assert_eq!(idle.active_frequency(), None);
    assert_eq!(idle.property.as_ref().unwrap().rssi, None);

    sim.begin_transmission(Transmission {
        frequency_hz: 769_268_750,
        tgid: Some(1234),
        unit_id: Some(501_234),
        system: "Metro P25".into(),
        department: "Police".into(),
        channel: "PD Dispatch".into(),
        modulation: "NFM".into(),
        signal_bars: 4,
        rssi: 0.5,
    });
    let info = s.scanner_info().await.unwrap();
    assert_eq!(info.active_frequency().unwrap().hz(), 769_268_750);
    assert_eq!(info.active_tgid(), Some("1234"));
    assert_eq!(info.unit_id.as_deref(), Some("501234"));
    assert_eq!(info.system.unwrap().name.as_deref(), Some("Metro P25"));
    assert_eq!(
        info.channel.unwrap().item.name.as_deref(),
        Some("PD Dispatch")
    );
    let p = info.property.unwrap();
    assert_eq!(p.signal_bars, Some(4));
    assert_eq!(p.muted, Some(false));

    sim.end_transmission();
    sim.begin_transmission(Transmission {
        frequency_hz: 154_415_000,
        system: "County".into(),
        department: "Fire & EMS".into(),
        channel: "Fire Dispatch".into(),
        modulation: "NFM".into(),
        ..Transmission::default()
    });
    let info = s.scanner_info().await.unwrap();
    assert_eq!(info.active_frequency().unwrap().hz(), 154_415_000);
    assert_eq!(info.active_tgid(), None);
    assert_eq!(info.unit_id, None);
    assert_eq!(info.department.unwrap().name.as_deref(), Some("Fire & EMS"));
}

#[tokio::test]
async fn hold_resume_and_quick_search() {
    let (sim, s) = setup(SimulatorConfig::default()).await;
    s.hold(Target::system(100)).await.unwrap();
    assert!(s.scanner_info().await.unwrap().is_holding());
    s.resume_scan().await.unwrap();
    assert!(!sim.state().hold);
    let f = uniden_sds::protocol::Frequency::from_hz(462_562_500).unwrap();
    s.quick_search_hold(f).await.unwrap();
    assert_eq!(sim.state().quick_search_hz, Some(462_562_500));
    assert_eq!(s.active_frequency().await.unwrap(), Some(f));
    s.press_key(KeyCode::MENU, KeyMode::Press).await.unwrap();
    assert!(sim.received().contains(&"KEY,M,P".to_owned()));
}

#[tokio::test]
async fn multipart_list_reassembly() {
    let mut cfg = SimulatorConfig::default();
    cfg.state.system_count = 250;
    cfg.max_xml_datagram = 600;
    let (_sim, s) = setup(cfg).await;
    let list = s
        .list(ListRequest::Systems { favorites_list: 0 })
        .await
        .unwrap();
    assert_eq!(list.entries.len(), 250);
    assert_eq!(list.entries[249].index, Some(349));
    let fl = s.list(ListRequest::FavoritesLists).await.unwrap();
    assert_eq!(fl.entries.len(), 3);
}

#[tokio::test]
async fn split_xml_header_datagrams() {
    let cfg = SimulatorConfig {
        faults: Faults {
            split_xml_header: true,
            ..Faults::default()
        },
        ..SimulatorConfig::default()
    };
    let (_sim, s) = setup(cfg).await;
    assert!(s.scanner_info().await.is_ok());
    assert_eq!(
        s.list(ListRequest::FavoritesLists)
            .await
            .unwrap()
            .entries
            .len(),
        3
    );
}

#[tokio::test]
async fn read_only_commands_survive_packet_loss() {
    let cfg = SimulatorConfig {
        faults: Faults {
            drop_reply: 0.3,
            drop_request: 0.1,
            ..Faults::default()
        },
        seed: 42,
        ..SimulatorConfig::default()
    };
    let sim = Simulator::start(cfg).await.unwrap();
    let mut c = fast_config(&sim);
    c.retries = 8;
    c.line_timeout = Duration::from_millis(100);
    let s = Scanner::connect(c).await.unwrap();
    for _ in 0..20 {
        assert!(s.model().await.unwrap().is_sds200());
    }
    assert!(s.stats().retries > 0, "{:?}", s.stats());
}

#[tokio::test]
async fn state_changing_commands_are_never_retried() {
    let cfg = SimulatorConfig {
        faults: Faults {
            drop_reply: 1.0,
            ..Faults::default()
        },
        ..SimulatorConfig::default()
    };
    let (sim, s) = setup(cfg).await;
    let err = s.set_volume(Volume::new(3).unwrap()).await.unwrap_err();
    assert!(
        matches!(
            err,
            Error::Timeout {
                attempts: 1,
                state_change_unknown: true,
                ..
            }
        ),
        "{err:?}"
    );
    assert_eq!(count(&sim, "VOL,3"), 1);
    // The command executed even though the reply was lost.
    assert_eq!(sim.state().volume, 3);

    // Read-only commands are retried `retries + 1` times in total.
    let err = s.model().await.unwrap_err();
    assert!(matches!(
        err,
        Error::Timeout {
            attempts: 3,
            state_change_unknown: false,
            ..
        }
    ));
    assert_eq!(count(&sim, "MDL"), 3);
}

#[tokio::test]
async fn duplicate_datagrams_are_discarded() {
    let cfg = SimulatorConfig {
        faults: Faults {
            duplicate: 1.0,
            ..Faults::default()
        },
        ..SimulatorConfig::default()
    };
    let (_sim, s) = setup(cfg).await;
    for level in [1u8, 2, 3] {
        s.set_volume(Volume::new(level).unwrap()).await.unwrap();
        assert_eq!(s.volume().await.unwrap().get(), level);
        assert!(s.model().await.unwrap().is_sds200());
    }
    assert!(s.stats().stale_frames > 0);
}

#[tokio::test]
async fn malformed_replies_do_not_poison_the_client() {
    let cfg = SimulatorConfig {
        faults: Faults {
            malformed: 1.0,
            ..Faults::default()
        },
        ..SimulatorConfig::default()
    };
    let (sim, s) = setup(cfg).await;
    let err = s.model().await.unwrap_err();
    assert!(
        matches!(err, Error::Decode { .. } | Error::Timeout { .. }),
        "{err:?}"
    );
    assert!(s.stats().decode_errors > 0);
    sim.set_faults(Faults::default());
    assert!(s.model().await.unwrap().is_sds200());
}

#[tokio::test]
async fn unsupported_commands_are_detected() {
    let mut state = State::default();
    state.unsupported.insert("VOL".into());
    state.unsupported.insert("STS".into());
    let (sim, s) = setup(SimulatorConfig {
        state,
        ..SimulatorConfig::default()
    })
    .await;
    let err = s.volume().await.unwrap_err();
    assert!(err.is_unsupported());
    // Rejections are not retried.
    assert_eq!(count(&sim, "VOL"), 1);
    let caps = s.probe_capabilities().await;
    assert!(caps.model.as_ref().unwrap().is_sds200());
    assert!(caps.firmware.is_some());
    assert!(caps.supports("GSI"));
    assert!(caps.supports("GLT,FL"));
    assert_eq!(
        caps.commands["VOL"],
        Support::Rejected(ScannerError::CommandError)
    );
    assert_eq!(
        caps.commands["STS"],
        Support::Rejected(ScannerError::CommandError)
    );
    // Probing must not send any state-changing command.
    assert!(
        sim.received()
            .iter()
            .all(|c| !c.contains(',') || c == "GLT,FL"),
        "{:?}",
        sim.received()
    );
}

#[tokio::test]
async fn health_tracks_interruptions() {
    let (sim, s) = setup(SimulatorConfig::default()).await;
    let mut c = fast_config(&sim);
    c.retries = 0;
    c.unreachable_after = 2;
    let s2 = Scanner::connect(c).await.unwrap();
    assert_eq!(s2.health(), Health::Unknown);
    s2.model().await.unwrap();
    sim.set_faults(Faults {
        offline: true,
        ..Faults::default()
    });
    assert!(s2.model().await.is_err());
    assert_eq!(
        s2.health(),
        Health::Degraded {
            consecutive_failures: 1
        }
    );
    assert!(s2.model().await.is_err());
    assert_eq!(
        s2.health(),
        Health::Unreachable {
            consecutive_failures: 2
        }
    );
    sim.set_faults(Faults::default());
    s2.model().await.unwrap();
    assert_eq!(s2.health(), Health::Healthy);
    drop(s);
}

#[tokio::test]
async fn concurrent_callers_are_serialized() {
    let (sim, s) = setup(SimulatorConfig::default()).await;
    let mut tasks = Vec::new();
    for i in 0..24u8 {
        let s = s.clone();
        tasks.push(tokio::spawn(async move {
            match i % 4 {
                0 => assert!(s.model().await.unwrap().is_sds200()),
                1 => assert!(s.firmware_version().await.unwrap().components.is_some()),
                2 => assert!(s.scanner_info().await.unwrap().property.is_some()),
                _ => assert!(s.squelch().await.unwrap().get() <= Squelch::MAX),
            }
        }));
    }
    for t in tasks {
        t.await.unwrap();
    }
    assert_eq!(sim.received().len(), 24);
    assert_eq!(s.stats().stale_frames, 0);
}

#[tokio::test]
async fn bounded_queue_rejects_overload() {
    let cfg = SimulatorConfig {
        faults: Faults {
            latency: Duration::from_millis(100),
            ..Faults::default()
        },
        ..SimulatorConfig::default()
    };
    let sim = Simulator::start(cfg).await.unwrap();
    let mut c = fast_config(&sim);
    c.queue_depth = 1;
    let s = Scanner::connect(c).await.unwrap();
    let mut tasks = Vec::new();
    for _ in 0..6 {
        let s = s.clone();
        tasks.push(tokio::spawn(async move { s.model().await }));
    }
    let mut full = 0;
    let mut ok = 0;
    for t in tasks {
        match t.await.unwrap() {
            Ok(_) => ok += 1,
            Err(Error::QueueFull) => full += 1,
            Err(e) => panic!("{e:?}"),
        }
    }
    assert!(full > 0 && ok > 0, "ok={ok} full={full}");
}

#[tokio::test]
async fn latency_beyond_timeout_times_out_and_recovers() {
    let cfg = SimulatorConfig {
        faults: Faults {
            latency: Duration::from_millis(400),
            ..Faults::default()
        },
        ..SimulatorConfig::default()
    };
    let sim = Simulator::start(cfg).await.unwrap();
    let mut c = fast_config(&sim);
    c.retries = 0;
    let s = Scanner::connect(c).await.unwrap();
    assert!(matches!(s.model().await, Err(Error::Timeout { .. })));
    sim.set_faults(Faults {
        latency: Duration::from_millis(20),
        ..Faults::default()
    });
    tokio::time::sleep(Duration::from_millis(450)).await;
    // The late reply from the earlier attempt is drained, not mistaken for
    // the answer to VER.
    assert!(s.firmware_version().await.is_ok());
    assert!(s.stats().stale_frames >= 1);
}

#[tokio::test]
async fn push_updates_are_delivered() {
    let (sim, s) = setup(SimulatorConfig::default()).await;
    let mut rx = s.subscribe();
    s.start_push(100).await.unwrap();
    for _ in 0..2 {
        let info = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(info.property.is_some());
    }
    // Commands still work while pushes arrive.
    assert!(s.model().await.unwrap().is_sds200());
    s.stop_push().await.unwrap();
    assert_eq!(sim.state().push_interval, None);
    assert!(s.stats().push_frames >= 2);
}

#[tokio::test]
async fn shutdown_and_closed() {
    let (_sim, s) = setup(SimulatorConfig::default()).await;
    let clone = s.clone();
    s.shutdown().await;
    assert!(matches!(clone.model().await, Err(Error::Closed)));
}

#[tokio::test]
async fn power_off_and_keepalive() {
    let (sim, s) = setup(SimulatorConfig::default()).await;
    // KAL has no reply and returns immediately.
    assert!(s.execute(Command::KeepAlive).await.is_ok());
    s.power_off().await.unwrap();
    assert!(sim.state().powered_off);
    assert!(matches!(s.model().await, Err(Error::Timeout { .. })));
}

#[tokio::test]
async fn invalid_arguments_are_rejected_before_sending() {
    let (sim, s) = setup(SimulatorConfig::default()).await;
    assert!(matches!(
        s.jump_number_tag(100, 0, 0).await,
        Err(Error::Encode(_))
    ));
    assert!(sim.received().is_empty());
}
