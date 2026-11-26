//! Store conformance suite, run against every backend.
//!
//! PostgreSQL tests need `TEST_DATABASE_URL` pointing at a server where the
//! user may create databases. Each run creates and drops its own database.
//!
//! ```text
//! TEST_DATABASE_URL=postgres://postgres@127.0.0.1:55441/postgres cargo test -p platform-storage -- --include-ignored
//! ```

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::net::IpAddr;

use platform_common::{
    AgentId, LeaseId, ScannerHealth, ScannerId, SiteId, TelemetrySnapshot, TenantId,
    TransmissionId, TransmissionStart, now_utc,
};
use platform_storage::model::*;
use platform_storage::{MemoryStore, PageRequest, PgStore, Store, StoreError};
use sqlx::Connection;

fn page(limit: u32, cursor: Option<String>) -> PageRequest {
    PageRequest { limit, cursor }
}

async fn tenant_with_site(store: &dyn Store, name: &str) -> (TenantId, Site, AgentId) {
    let t = TenantId::new();
    store.ensure_tenant(t, name).await.unwrap();
    store.ensure_tenant(t, name).await.unwrap(); // idempotent
    let site = store
        .create_site(NewSite {
            tenant_id: t,
            name: "Site A".into(),
            description: Some("north".into()),
            latitude: Some(30.2),
            longitude: Some(-93.2),
            allowed_networks: vec!["192.168.50.0/24".parse().unwrap()],
        })
        .await
        .unwrap();
    let agent = AgentId::new(format!("agent-{}", uuid::Uuid::new_v4().simple())).unwrap();
    store
        .upsert_agent(Agent {
            id: agent.clone(),
            tenant_id: t,
            site_id: site.id,
            description: None,
            last_seen_at: None,
        })
        .await
        .unwrap();
    (t, site, agent)
}

fn new_scanner(t: TenantId, site: SiteId, agent: &AgentId, last_octet: u8) -> NewScanner {
    NewScanner {
        tenant_id: t,
        site_id: site,
        agent_id: agent.clone(),
        name: format!("Scanner {last_octet}"),
        host: IpAddr::from([192, 168, 50, last_octet]),
        port: 50536,
        enabled: true,
    }
}

async fn suite(store: &dyn Store) {
    store.ping().await.unwrap();
    let (t, site, agent) = tenant_with_site(store, "Tenant One").await;
    let (t2, site2, agent2) = tenant_with_site(store, "Tenant Two").await;

    // Sites
    assert_eq!(
        store
            .get_site(t, site.id)
            .await
            .unwrap()
            .allowed_networks
            .len(),
        1
    );
    assert_eq!(
        store.get_site(t2, site.id).await,
        Err(StoreError::NotFound),
        "tenant isolation"
    );
    let dup = store
        .create_site(NewSite {
            tenant_id: t,
            name: "Site A".into(),
            description: None,
            latitude: None,
            longitude: None,
            allowed_networks: vec![],
        })
        .await;
    assert!(matches!(dup, Err(StoreError::Conflict(_))));
    assert!(matches!(
        store
            .create_site(NewSite {
                tenant_id: t,
                name: " ".into(),
                description: None,
                latitude: None,
                longitude: None,
                allowed_networks: vec![]
            })
            .await,
        Err(StoreError::Invalid(_))
    ));
    assert_eq!(
        store
            .list_sites(t, None, &page(10, None))
            .await
            .unwrap()
            .items
            .len(),
        1
    );
    assert_eq!(
        store
            .list_sites(t, Some(&[]), &page(10, None))
            .await
            .unwrap()
            .items
            .len(),
        0,
        "empty site scope"
    );

    // Agents
    let a = store.get_agent(&agent).await.unwrap();
    assert_eq!(a.tenant_id, t);
    store.touch_agent(&agent, now_utc()).await.unwrap();
    assert!(
        store
            .get_agent(&agent)
            .await
            .unwrap()
            .last_seen_at
            .is_some()
    );
    assert!(
        matches!(
            store
                .upsert_agent(Agent {
                    id: agent.clone(),
                    tenant_id: t,
                    site_id: site2.id,
                    description: None,
                    last_seen_at: None
                })
                .await,
            Err(StoreError::Conflict(_))
        ),
        "agent cannot be bound to another tenant's site"
    );
    let agents = store.list_agents(t, None, &page(10, None)).await.unwrap();
    assert_eq!(
        agents.items.iter().map(|a| &a.id).collect::<Vec<_>>(),
        vec![&agent]
    );
    assert!(
        store
            .list_agents(t, Some(&[]), &page(10, None))
            .await
            .unwrap()
            .items
            .is_empty(),
        "agents honour site scope"
    );
    assert!(
        store
            .list_agents(site2.tenant_id, None, &page(10, None))
            .await
            .unwrap()
            .items
            .iter()
            .all(|a| a.id != agent),
        "agents are tenant-scoped"
    );

    // Scanners + pagination
    let mut ids = Vec::new();
    for i in 1..=5 {
        ids.push(
            store
                .create_scanner(new_scanner(t, site.id, &agent, i))
                .await
                .unwrap()
                .id,
        );
    }
    ids.sort();
    assert!(matches!(
        store
            .create_scanner(new_scanner(t, site.id, &agent, 1))
            .await,
        Err(StoreError::Conflict(_))
    ));
    assert!(
        matches!(
            store
                .create_scanner(new_scanner(t, site2.id, &agent2, 1))
                .await,
            Err(StoreError::Conflict(_))
        ),
        "cross-tenant site/agent"
    );
    let p1 = store
        .list_scanners(t, &ScannerFilter::default(), &page(2, None))
        .await
        .unwrap();
    assert_eq!(p1.items.len(), 2);
    let p2 = store
        .list_scanners(
            t,
            &ScannerFilter::default(),
            &page(2, p1.next_cursor.clone()),
        )
        .await
        .unwrap();
    let p3 = store
        .list_scanners(
            t,
            &ScannerFilter::default(),
            &page(2, p2.next_cursor.clone()),
        )
        .await
        .unwrap();
    assert_eq!(p3.items.len(), 1);
    assert!(p3.next_cursor.is_none());
    let all: Vec<ScannerId> = [p1.items, p2.items, p3.items]
        .concat()
        .iter()
        .map(|s| s.id)
        .collect();
    assert_eq!(all, ids);
    assert!(matches!(
        store
            .list_scanners(t, &ScannerFilter::default(), &page(2, Some("!!".into())))
            .await,
        Err(StoreError::Invalid(_))
    ));
    assert_eq!(
        store
            .list_scanners(t2, &ScannerFilter::default(), &page(50, None))
            .await
            .unwrap()
            .items
            .len(),
        0
    );
    assert_eq!(
        store
            .list_scanners(
                t,
                &ScannerFilter {
                    site_ids: Some(vec![SiteId::new()]),
                    ..ScannerFilter::default()
                },
                &page(50, None)
            )
            .await
            .unwrap()
            .items
            .len(),
        0
    );

    let sid = ids[0];
    let up = store
        .update_scanner(
            t,
            sid,
            ScannerPatch {
                name: Some("Renamed".into()),
                enabled: Some(false),
                ..ScannerPatch::default()
            },
        )
        .await
        .unwrap();
    assert_eq!((up.name.as_str(), up.enabled), ("Renamed", false));
    assert_eq!(
        store.update_scanner(t2, sid, ScannerPatch::default()).await,
        Err(StoreError::NotFound)
    );
    let other = store.get_scanner(t, ids[1]).await.unwrap();
    assert!(matches!(
        store
            .update_scanner(
                t,
                sid,
                ScannerPatch {
                    host: Some(other.host),
                    ..ScannerPatch::default()
                }
            )
            .await,
        Err(StoreError::Conflict(_))
    ));
    store
        .set_scanner_status(
            sid,
            Some("SDS200".into()),
            Some("Version 1.26.01".into()),
            Some(ScannerHealth::Healthy),
        )
        .await
        .unwrap();
    let s = store.get_scanner(t, sid).await.unwrap();
    assert_eq!(s.model.as_deref(), Some("SDS200"));
    assert_eq!(s.health, Some(ScannerHealth::Healthy));
    assert_eq!(store.scanners_for_agent(&agent).await.unwrap().len(), 5);

    // Telemetry
    assert_eq!(store.get_telemetry(t, sid).await.unwrap(), None);
    let mut snap = TelemetrySnapshot::empty(now_utc());
    snap.frequency_hz = Some(154_415_000);
    store.put_telemetry(sid, &snap).await.unwrap();
    assert_eq!(
        store
            .get_telemetry(t, sid)
            .await
            .unwrap()
            .unwrap()
            .frequency_hz,
        Some(154_415_000)
    );
    assert_eq!(
        store.get_telemetry(t2, sid).await,
        Err(StoreError::NotFound)
    );

    // Events: dedupe and pagination
    let session = uuid::Uuid::new_v4();
    for seq in 1..=4u64 {
        let ev = NewEvent {
            tenant_id: t,
            site_id: site.id,
            scanner_id: sid,
            agent_id: agent.clone(),
            agent_session: session,
            agent_seq: seq,
            kind: if seq % 2 == 0 {
                "health_changed".into()
            } else {
                "transmission_started".into()
            },
            payload: serde_json::json!({ "seq": seq }),
        };
        assert!(store.append_event(ev.clone()).await.unwrap().is_some());
        assert!(
            store.append_event(ev).await.unwrap().is_none(),
            "replay is deduplicated"
        );
    }
    let e1 = store
        .list_events(t, &EventFilter::default(), &page(3, None))
        .await
        .unwrap();
    assert_eq!(e1.items.len(), 3);
    assert_eq!(e1.items[0].payload["seq"], 4, "newest first");
    let e2 = store
        .list_events(t, &EventFilter::default(), &page(3, e1.next_cursor))
        .await
        .unwrap();
    assert_eq!(e2.items.len(), 1);
    let kinds = store
        .list_events(
            t,
            &EventFilter {
                kind: Some("health_changed".into()),
                ..EventFilter::default()
            },
            &page(10, None),
        )
        .await
        .unwrap();
    assert_eq!(kinds.items.len(), 2);
    assert!(
        store
            .list_events(t2, &EventFilter::default(), &page(10, None))
            .await
            .unwrap()
            .items
            .is_empty()
    );

    // Transmissions
    let base = now_utc();
    let mut tx_ids = Vec::new();
    for i in 0..3i64 {
        let start = TransmissionStart {
            transmission_id: TransmissionId::new(),
            scanner_id: sid,
            site_id: Some(site.id),
            started_at: base + time::Duration::seconds(i),
            frequency_hz: Some(if i == 2 { 460_000_000 } else { 154_415_000 }),
            tgid: if i == 1 { Some("1234".into()) } else { None },
            unit_id: None,
            system: Some("County".into()),
            department: None,
            channel: Some("Dispatch".into()),
            modulation: Some("NFM".into()),
            signal_bars: Some(3),
            rssi: None,
        };
        store.start_transmission(t, site.id, &start).await.unwrap();
        store.start_transmission(t, site.id, &start).await.unwrap(); // idempotent
        tx_ids.push(start.transmission_id);
    }
    store
        .end_transmission(tx_ids[0], base + time::Duration::seconds(5), 5000)
        .await
        .unwrap();
    let tx = store.get_transmission(t, tx_ids[0]).await.unwrap();
    assert_eq!(tx.duration_ms, Some(5000));
    assert_eq!(tx.unit_id, None, "unreported metadata stays null");
    assert_eq!(
        store.get_transmission(t2, tx_ids[0]).await,
        Err(StoreError::NotFound)
    );
    let tp1 = store
        .list_transmissions(t, &TransmissionFilter::default(), &page(2, None))
        .await
        .unwrap();
    assert_eq!(
        tp1.items.iter().map(|x| x.id).collect::<Vec<_>>(),
        vec![tx_ids[2], tx_ids[1]]
    );
    let tp2 = store
        .list_transmissions(t, &TransmissionFilter::default(), &page(2, tp1.next_cursor))
        .await
        .unwrap();
    assert_eq!(
        tp2.items.iter().map(|x| x.id).collect::<Vec<_>>(),
        vec![tx_ids[0]]
    );
    let by_freq = store
        .list_transmissions(
            t,
            &TransmissionFilter {
                frequency_hz: Some(154_415_000),
                ..TransmissionFilter::default()
            },
            &page(10, None),
        )
        .await
        .unwrap();
    assert_eq!(by_freq.items.len(), 2);
    let by_tg = store
        .list_transmissions(
            t,
            &TransmissionFilter {
                tgid: Some("1234".into()),
                ..TransmissionFilter::default()
            },
            &page(10, None),
        )
        .await
        .unwrap();
    assert_eq!(by_tg.items.len(), 1);
    let window = store
        .list_transmissions(
            t,
            &TransmissionFilter {
                from: Some(base + time::Duration::seconds(1)),
                to: Some(base + time::Duration::seconds(2)),
                ..TransmissionFilter::default()
            },
            &page(10, None),
        )
        .await
        .unwrap();
    assert_eq!(
        window.items.iter().map(|x| x.id).collect::<Vec<_>>(),
        vec![tx_ids[1]]
    );
    let foreign = TransmissionStart {
        transmission_id: TransmissionId::new(),
        scanner_id: sid,
        site_id: None,
        started_at: base,
        frequency_hz: None,
        tgid: None,
        unit_id: None,
        system: None,
        department: None,
        channel: None,
        modulation: None,
        signal_bars: None,
        rssi: None,
    };
    assert_eq!(
        store.start_transmission(t2, site2.id, &foreign).await,
        Err(StoreError::NotFound)
    );

    // Audit
    for i in 0..3 {
        store
            .append_audit(NewAudit {
                tenant_id: t,
                actor: "alice".into(),
                action: format!("scanner.command.{i}"),
                resource_type: "scanner".into(),
                resource_id: Some(sid.to_string()),
                outcome: if i == 1 {
                    AuditOutcome::Denied
                } else {
                    AuditOutcome::Success
                },
                details: serde_json::json!({}),
                source_ip: Some("10.0.0.9".parse().unwrap()),
            })
            .await
            .unwrap();
    }
    let au = store
        .list_audit(t, &AuditFilter::default(), &page(2, None))
        .await
        .unwrap();
    assert_eq!(au.items[0].action, "scanner.command.2");
    assert_eq!(au.items[1].outcome, AuditOutcome::Denied);
    assert_eq!(au.items[0].source_ip, Some("10.0.0.9".parse().unwrap()));
    assert!(
        store
            .list_audit(t2, &AuditFilter::default(), &page(10, None))
            .await
            .unwrap()
            .items
            .is_empty()
    );

    // Groups
    let g = store.create_group(t, "North").await.unwrap();
    store.add_group_member(t, g.id, ids[1]).await.unwrap();
    store.add_group_member(t, g.id, ids[1]).await.unwrap();
    assert_eq!(
        store.add_group_member(t2, g.id, ids[1]).await,
        Err(StoreError::NotFound)
    );
    let groups = store.list_groups(t).await.unwrap();
    assert_eq!(groups[0].members, vec![ids[1]]);
    assert!(matches!(
        store.create_group(t, "North").await,
        Err(StoreError::Conflict(_))
    ));
    store.add_group_member(t, g.id, ids[2]).await.unwrap();
    assert_eq!(store.get_group(t, g.id).await.unwrap().members.len(), 2);
    assert_eq!(store.get_group(t2, g.id).await, Err(StoreError::NotFound));
    assert_eq!(
        store.remove_group_member(t2, g.id, ids[2]).await,
        Err(StoreError::NotFound),
        "foreign tenant cannot edit membership"
    );
    store.remove_group_member(t, g.id, ids[2]).await.unwrap();
    assert_eq!(
        store.remove_group_member(t, g.id, ids[2]).await,
        Err(StoreError::NotFound)
    );
    assert_eq!(
        store.get_group(t, g.id).await.unwrap().members,
        vec![ids[1]]
    );
    let g2 = store.create_group(t, "Temporary").await.unwrap();
    assert_eq!(
        store.delete_group(t2, g2.id).await,
        Err(StoreError::NotFound)
    );
    store.delete_group(t, g2.id).await.unwrap();
    assert_eq!(store.get_group(t, g2.id).await, Err(StoreError::NotFound));

    // Leases
    let t0 = now_utc();
    let ttl = time::Duration::seconds(300);
    let first = LeaseId::new();
    let LeaseAcquire::Granted(l) = store
        .acquire_lease(t, sid, "alice", first, ttl, t0)
        .await
        .unwrap()
    else {
        panic!("free scanner must be granted")
    };
    assert_eq!(l.id, first);
    assert_eq!(l.holder, "alice");
    // Renewal keeps the id and extends the expiry.
    let later = t0 + time::Duration::seconds(60);
    let LeaseAcquire::Granted(r) = store
        .acquire_lease(t, sid, "alice", LeaseId::new(), ttl, later)
        .await
        .unwrap()
    else {
        panic!("holder renews")
    };
    assert_eq!(r.id, first);
    assert_eq!(r.acquired_at, l.acquired_at);
    assert!(r.expires_at > l.expires_at);
    // Someone else is refused while it is live.
    match store
        .acquire_lease(t, sid, "bob", LeaseId::new(), ttl, later)
        .await
        .unwrap()
    {
        LeaseAcquire::Held(h) => assert_eq!(h.holder, "alice"),
        LeaseAcquire::Granted(_) => panic!("lease stolen"),
    }
    // Tenant isolation.
    assert_eq!(
        store
            .acquire_lease(t2, sid, "mallory", LeaseId::new(), ttl, later)
            .await,
        Err(StoreError::NotFound)
    );
    assert!(
        store
            .current_leases(t2, &[sid], later)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        store.delete_lease(t2, sid, first, later).await.unwrap(),
        None
    );
    assert_eq!(
        store
            .current_leases(t, &[sid, ids[1]], later)
            .await
            .unwrap()
            .len(),
        1
    );
    // Wrong id does not delete; the right one does.
    assert_eq!(
        store
            .delete_lease(t, sid, LeaseId::new(), later)
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        store
            .delete_lease(t, sid, first, later)
            .await
            .unwrap()
            .map(|l| l.id),
        Some(first)
    );
    assert!(
        store
            .current_leases(t, &[sid], later)
            .await
            .unwrap()
            .is_empty()
    );
    // After expiry, another holder takes over with a fresh id.
    let LeaseAcquire::Granted(_) = store
        .acquire_lease(t, sid, "alice", first, ttl, later)
        .await
        .unwrap()
    else {
        panic!()
    };
    let expired = later + time::Duration::seconds(301);
    let second = LeaseId::new();
    let LeaseAcquire::Granted(b) = store
        .acquire_lease(t, sid, "bob", second, ttl, expired)
        .await
        .unwrap()
    else {
        panic!("expired lease must be taken over")
    };
    assert_eq!((b.id, b.holder.as_str()), (second, "bob"));
    assert_eq!(store.purge_expired_leases(expired).await.unwrap(), 0);
    assert_eq!(
        store
            .purge_expired_leases(expired + time::Duration::hours(1))
            .await
            .unwrap(),
        1
    );
    // Concurrent acquisition by different holders: exactly one wins.
    let holders: Vec<String> = (0..8).map(|i| format!("op{i}")).collect();
    let tasks: Vec<_> = holders
        .iter()
        .map(|h| store.acquire_lease(t, ids[1], h, LeaseId::new(), ttl, expired))
        .collect();
    let results = futures_util::future::join_all(tasks).await;
    let granted = results
        .iter()
        .filter(|r| matches!(r, Ok(LeaseAcquire::Granted(_))))
        .count();
    assert_eq!(granted, 1, "exactly one concurrent acquirer wins");

    // Deletion cascades
    store.delete_scanner(t, sid).await.unwrap();
    assert_eq!(store.get_scanner(t, sid).await, Err(StoreError::NotFound));
    assert_eq!(
        store.delete_scanner(t, sid).await,
        Err(StoreError::NotFound)
    );
    assert_eq!(
        store.get_transmission(t, tx_ids[0]).await,
        Err(StoreError::NotFound)
    );
}

/// Phase 5: transcription queue, search, holds, retention and analytics.
/// Uses timestamps in 2000 so retention cut-offs touch only this data.
async fn phase5(store: &dyn Store) {
    let (t, site, agent) = tenant_with_site(store, "Phase 5").await;
    let sc = store
        .create_scanner(new_scanner(t, site.id, &agent, 77))
        .await
        .unwrap();
    // Transcription needs recording, which needs audio.
    let s = store
        .update_scanner(
            t,
            sc.id,
            ScannerPatch {
                transcription_enabled: Some(true),
                ..ScannerPatch::default()
            },
        )
        .await
        .unwrap();
    assert!(!s.transcription_enabled, "needs recording");
    let s = store
        .update_scanner(
            t,
            sc.id,
            ScannerPatch {
                audio_enabled: Some(true),
                recording_enabled: Some(true),
                transcription_enabled: Some(true),
                ..ScannerPatch::default()
            },
        )
        .await
        .unwrap();
    assert!(s.transcription_enabled);
    let s = store
        .update_scanner(
            t,
            sc.id,
            ScannerPatch {
                recording_enabled: Some(false),
                ..ScannerPatch::default()
            },
        )
        .await
        .unwrap();
    assert!(
        !s.transcription_enabled,
        "turning recording off turns transcription off"
    );

    let base = time::macros::datetime!(2000-01-01 10:00 UTC);
    let mut ids = Vec::new();
    for i in 0..4i64 {
        let st = TransmissionStart {
            transmission_id: TransmissionId::new(),
            scanner_id: sc.id,
            site_id: Some(site.id),
            started_at: base + time::Duration::minutes(i * 30),
            frequency_hz: Some(154_000_000 + u64::try_from(i % 2).unwrap()),
            tgid: Some(if i < 3 { "100".into() } else { "200".into() }),
            unit_id: None,
            system: None,
            department: None,
            channel: Some("Fire".into()),
            modulation: None,
            signal_bars: None,
            rssi: None,
        };
        store.start_transmission(t, site.id, &st).await.unwrap();
        store
            .end_transmission(
                st.transmission_id,
                st.started_at + time::Duration::seconds(2),
                2000,
            )
            .await
            .unwrap();
        ids.push(st.transmission_id);
    }
    let now = base + time::Duration::days(1);
    // 0 and 1 recorded and queued; 2 recorded only; 3 not recorded.
    for (i, id) in ids.iter().enumerate().take(3) {
        store
            .set_transmission_audio(t, *id, &format!("key-{i}"), i < 2, now)
            .await
            .unwrap();
    }
    assert!(matches!(
        store.request_transcription(t, ids[3], now).await,
        Err(StoreError::Conflict(_))
    ));
    assert_eq!(
        store
            .request_transcription(TenantId::new(), ids[0], now)
            .await,
        Err(StoreError::NotFound)
    );
    let stats = store.transcription_stats(t, base).await.unwrap();
    assert_eq!(stats.pending, 2);

    // Claims are exclusive until the lease expires.
    let lease = now + time::Duration::minutes(5);
    let jobs = store.claim_transcriptions(now, lease, 10).await.unwrap();
    let mine: Vec<_> = jobs.iter().filter(|j| j.tenant_id == t).collect();
    assert_eq!(mine.len(), 2);
    assert!(
        mine.iter()
            .all(|j| j.attempts == 1 && j.duration_ms == Some(2000))
    );
    assert!(
        store
            .claim_transcriptions(now, lease, 10)
            .await
            .unwrap()
            .iter()
            .all(|j| j.tenant_id != t)
    );
    let later = lease + time::Duration::seconds(1);
    let again = store
        .claim_transcriptions(later, later + time::Duration::minutes(5), 10)
        .await
        .unwrap();
    assert_eq!(
        again.iter().filter(|j| j.tenant_id == t).count(),
        2,
        "expired claims are reclaimed"
    );
    assert!(
        again
            .iter()
            .filter(|j| j.tenant_id == t)
            .all(|j| j.attempts == 2)
    );

    store
        .finish_transcription(
            t,
            ids[0],
            TranscriptionResult::Done {
                text: "Engine 5 responding to Main Street".into(),
                language: Some("en".into()),
                model: Some("whisper-1".into()),
            },
            later,
        )
        .await
        .unwrap();
    let retry_at = later + time::Duration::minutes(1);
    store
        .finish_transcription(
            t,
            ids[1],
            TranscriptionResult::Retry {
                error: "503".into(),
                at: retry_at,
            },
            later,
        )
        .await
        .unwrap();
    let r = store.get_transmission(t, ids[0]).await.unwrap();
    assert_eq!(r.transcription_state, Some(TranscriptionState::Done));
    assert_eq!(r.transcript_language.as_deref(), Some("en"));
    assert_eq!(r.transcribed_at, Some(later));
    let r1 = store.get_transmission(t, ids[1]).await.unwrap();
    assert_eq!(r1.transcription_state, Some(TranscriptionState::Pending));
    assert_eq!(r1.transcription_error.as_deref(), Some("503"));
    assert!(
        store
            .claim_transcriptions(later, retry_at, 10)
            .await
            .unwrap()
            .iter()
            .all(|j| j.tenant_id != t),
        "not before the retry time"
    );
    let j = store
        .claim_transcriptions(retry_at, retry_at + time::Duration::minutes(5), 10)
        .await
        .unwrap();
    assert_eq!(
        j.iter()
            .filter(|j| j.tenant_id == t)
            .map(|j| j.attempts)
            .collect::<Vec<_>>(),
        vec![3]
    );
    store
        .finish_transcription(
            t,
            ids[1],
            TranscriptionResult::Failed {
                error: "gave up".into(),
            },
            retry_at,
        )
        .await
        .unwrap();
    let stats = store.transcription_stats(t, base).await.unwrap();
    assert_eq!((stats.pending, stats.done, stats.failed), (0, 1, 1));

    // Full-text search: whole words, all terms, case-insensitive.
    let search = |text: &str| TransmissionFilter {
        text: Some(text.into()),
        ..TransmissionFilter::default()
    };
    for (q, n) in [
        ("engine", 1),
        ("MAIN street", 1),
        ("engine ladder", 0),
        ("eng", 0),
    ] {
        let p = store
            .list_transmissions(t, &search(q), &page(50, None))
            .await
            .unwrap();
        assert_eq!(p.items.len(), n, "query {q:?}");
    }
    let recorded = store
        .list_transmissions(
            t,
            &TransmissionFilter {
                recorded: Some(true),
                ..TransmissionFilter::default()
            },
            &page(50, None),
        )
        .await
        .unwrap();
    assert_eq!(recorded.items.len(), 3);
    let failed = store
        .list_transmissions(
            t,
            &TransmissionFilter {
                transcription_state: Some(TranscriptionState::Failed),
                ..TransmissionFilter::default()
            },
            &page(50, None),
        )
        .await
        .unwrap();
    assert_eq!(
        failed.items.iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![ids[1]]
    );

    // Analytics.
    let af = AnalyticsFilter {
        from: base,
        to: base + time::Duration::days(1),
        site_id: None,
        site_ids: None,
        scanner_id: Some(sc.id),
    };
    let hours = store
        .transmission_activity(t, &af, Bucket::Hour)
        .await
        .unwrap();
    assert_eq!(
        hours
            .iter()
            .map(|p| (p.start, p.transmissions, p.airtime_ms))
            .collect::<Vec<_>>(),
        vec![(base, 2, 4000), (base + time::Duration::HOUR, 2, 4000)]
    );
    let days = store
        .transmission_activity(t, &af, Bucket::Day)
        .await
        .unwrap();
    assert_eq!(days.len(), 1);
    assert_eq!(days[0].start, time::macros::datetime!(2000-01-01 0:00 UTC));
    let top = store
        .transmission_top(t, &af, TopBy::Talkgroup, 10)
        .await
        .unwrap();
    assert_eq!(
        top.iter()
            .map(|e| (e.key.as_str(), e.transmissions))
            .collect::<Vec<_>>(),
        vec![("100", 3), ("200", 1)]
    );
    let top = store
        .transmission_top(t, &af, TopBy::Frequency, 1)
        .await
        .unwrap();
    assert_eq!(top.len(), 1);
    assert!(
        store
            .transmission_top(t, &af, TopBy::Unit, 10)
            .await
            .unwrap()
            .is_empty()
    );
    let other = store
        .transmission_top(TenantId::new(), &af, TopBy::Talkgroup, 10)
        .await
        .unwrap();
    assert!(other.is_empty(), "tenant isolation");

    // Holds exempt from retention.
    let held = store.set_transmission_hold(t, ids[2], true).await.unwrap();
    assert!(held.hold);
    let cutoff = base + time::Duration::days(2);
    let expired = store.expired_recordings(cutoff, 100).await.unwrap();
    let mine: Vec<_> = expired.iter().filter(|e| e.tenant_id == t).collect();
    assert_eq!(mine.len(), 2, "held recording excluded");
    assert!(
        !store
            .clear_transmission_audio(t, ids[2], "key-2", false)
            .await
            .unwrap(),
        "held"
    );
    assert!(
        !store
            .clear_transmission_audio(t, ids[0], "other-key", false)
            .await
            .unwrap(),
        "key changed"
    );
    for e in mine {
        assert!(
            store
                .clear_transmission_audio(t, e.transmission_id, &e.audio_object, false)
                .await
                .unwrap()
        );
    }
    let r = store.get_transmission(t, ids[0]).await.unwrap();
    assert!(r.audio_object.is_none());
    assert!(r.transcript.is_some(), "transcript has its own retention");
    let n = store.expire_transcripts(cutoff, 100).await.unwrap();
    assert!(n.contains(&(t, 1)));
    let r = store.get_transmission(t, ids[0]).await.unwrap();
    assert!(r.transcript.is_none());
    // Unheld rows without recordings go; the held one stays.
    let n = store
        .delete_transmissions_before(cutoff, 100)
        .await
        .unwrap();
    assert!(n.contains(&(t, 3)), "{n:?}");
    assert!(store.get_transmission(t, ids[2]).await.is_ok());
    store.set_transmission_hold(t, ids[2], false).await.unwrap();
    assert!(
        store
            .clear_transmission_audio(t, ids[2], "key-2", true)
            .await
            .unwrap()
    );
    assert_eq!(
        store
            .delete_transmissions_before(cutoff, 100)
            .await
            .unwrap(),
        vec![(t, 1)]
    );
    assert_eq!(
        store
            .set_transmission_hold(t, ids[2], true)
            .await
            .map(|_| ()),
        Err(StoreError::NotFound)
    );

    // Events.
    let n = store
        .append_event(NewEvent {
            tenant_id: t,
            site_id: site.id,
            scanner_id: sc.id,
            agent_id: agent.clone(),
            agent_session: uuid::Uuid::new_v4(),
            agent_seq: 1,
            kind: "health_changed".into(),
            payload: serde_json::json!({}),
        })
        .await
        .unwrap();
    assert!(n.is_some());
    let gone = store
        .delete_events_before(now_utc() + time::Duration::seconds(5), 10_000)
        .await
        .unwrap();
    assert!(gone.contains(&(t, 1)), "{gone:?}");
}

/// Phase 6: the audit hash chain survives storage, detects tampering, and
/// stays linear under concurrent appends.
async fn audit_chain(store: &dyn Store) {
    use platform_storage::audit_chain::Verifier;
    let (t, _site, _agent) = tenant_with_site(store, "Chain").await;
    let details = [
        serde_json::json!({ "z": 1, "a": "x" }),
        serde_json::json!({ "nested": { "b": [1, 2.5, -3, null, true], "a": "ünïcode ✓" }, "big": 9_007_199_254_740_993_u64 }),
        serde_json::json!({ "f": 0.1, "g": 1e-7, "h": 123_456.789, "e": "", "q": "quote \" and \\ slash" }),
        serde_json::json!(null),
        serde_json::json!([3, 2, 1]),
    ];
    for (i, d) in details.iter().enumerate() {
        store
            .append_audit(NewAudit {
                tenant_id: t,
                actor: format!("user-{i}"),
                action: if i % 2 == 0 {
                    "scanner.update".into()
                } else {
                    "site.create".into()
                },
                resource_type: "scanner".into(),
                resource_id: (i > 1).then(|| format!("r{i}")),
                outcome: if i == 3 {
                    AuditOutcome::Denied
                } else {
                    AuditOutcome::Success
                },
                details: d.clone(),
                source_ip: (i != 2).then(|| "2001:db8::1".parse().unwrap()),
            })
            .await
            .unwrap();
    }
    // Concurrent appends stay one linear chain.
    let tasks: Vec<_> = (0..20)
        .map(|i| {
            store.append_audit(NewAudit {
                tenant_id: t,
                actor: "burst".into(),
                action: "scanner.command.get_status".into(),
                resource_type: "scanner".into(),
                resource_id: None,
                outcome: AuditOutcome::Success,
                details: serde_json::json!({ "i": i }),
                source_ip: None,
            })
        })
        .collect();
    for r in futures_util::future::join_all(tasks).await {
        r.unwrap();
    }
    let head = store.audit_head(t).await.unwrap();
    assert_eq!((head.seq, head.unchained), (25, 0));
    let chain = store.audit_chain(t, 0, 1000).await.unwrap();
    assert_eq!(chain.len(), 25);
    let mut v = Verifier::default();
    for r in &chain {
        v.push(r).unwrap_or_else(|e| panic!("{e:?} on {r:?}"));
    }
    v.finish(head.seq, &head.hash).unwrap();
    // Paging through the chain.
    assert_eq!(
        store
            .audit_chain(t, 20, 3)
            .await
            .unwrap()
            .iter()
            .map(|r| r.seq)
            .collect::<Vec<_>>(),
        vec![Some(21), Some(22), Some(23)]
    );
    // Another tenant's chain is separate.
    let (t2, _, _) = tenant_with_site(store, "Chain 2").await;
    assert_eq!(store.audit_head(t2).await.unwrap().seq, 0);
    assert!(store.audit_chain(t2, 0, 10).await.unwrap().is_empty());
    assert!(
        store
            .audit_heads()
            .await
            .unwrap()
            .iter()
            .any(|(x, h)| *x == t && h.seq == 25)
    );

    // Filters.
    let f = |f: AuditFilter| async move {
        store
            .list_audit(t, &f, &page(100, None))
            .await
            .unwrap()
            .items
            .len()
    };
    assert_eq!(
        f(AuditFilter {
            action: Some("scanner.".into()),
            ..AuditFilter::default()
        })
        .await,
        23
    );
    assert_eq!(
        f(AuditFilter {
            actor: Some("user-1".into()),
            ..AuditFilter::default()
        })
        .await,
        1
    );
    assert_eq!(
        f(AuditFilter {
            outcome: Some(AuditOutcome::Denied),
            ..AuditFilter::default()
        })
        .await,
        1
    );
    assert_eq!(
        f(AuditFilter {
            resource_id: Some("r4".into()),
            ..AuditFilter::default()
        })
        .await,
        1
    );
    let future = now_utc() + time::Duration::hours(1);
    assert_eq!(
        f(AuditFilter {
            from: Some(future),
            ..AuditFilter::default()
        })
        .await,
        0
    );
    assert_eq!(
        f(AuditFilter {
            to: Some(future),
            ..AuditFilter::default()
        })
        .await,
        25
    );
}

/// PostgreSQL only: the append-only trigger, and detection of changes made
/// by someone who bypasses it (the database owner).
async fn audit_tampering(url: &str) {
    use platform_storage::audit_chain::Verifier;
    let store = PgStore::connect(url, 2).await.unwrap();
    let (t, _, _) = tenant_with_site(&store, "Tamper").await;
    for i in 0..5 {
        store
            .append_audit(NewAudit {
                tenant_id: t,
                actor: "alice".into(),
                action: format!("a.{i}"),
                resource_type: "x".into(),
                resource_id: None,
                outcome: AuditOutcome::Success,
                details: serde_json::json!({ "i": i }),
                source_ip: None,
            })
            .await
            .unwrap();
    }
    let mut conn = sqlx::PgConnection::connect(url).await.unwrap();
    let upd =
        sqlx::query("UPDATE audit_log SET actor = 'mallory' WHERE tenant_id = $1 AND seq = 3")
            .bind(t.0)
            .execute(&mut conn)
            .await;
    assert!(upd.is_err(), "UPDATE is refused");
    let del = sqlx::query("DELETE FROM audit_log WHERE tenant_id = $1 AND seq = 5")
        .bind(t.0)
        .execute(&mut conn)
        .await;
    assert!(del.is_err(), "DELETE is refused");
    let verify = |store: PgStore| async move {
        let head = store.audit_head(t).await.unwrap();
        let mut v = Verifier::default();
        for r in store.audit_chain(t, 0, 100).await.unwrap() {
            v.push(&r)?;
        }
        v.finish(head.seq, &head.hash)
    };
    assert!(
        verify(PgStore::connect(url, 1).await.unwrap())
            .await
            .is_ok()
    );
    // The owner disables the trigger and edits a record: detected.
    for sql in [
        "ALTER TABLE audit_log DISABLE TRIGGER audit_log_append_only",
        "UPDATE audit_log SET actor = 'mallory' WHERE seq = 3",
        "ALTER TABLE audit_log ENABLE TRIGGER audit_log_append_only",
    ] {
        sqlx::query(sqlx::AssertSqlSafe(sql))
            .execute(&mut conn)
            .await
            .unwrap();
    }
    let e = verify(PgStore::connect(url, 1).await.unwrap())
        .await
        .unwrap_err();
    assert_eq!(e.seq, 3, "{e:?}");
    // Deleting the newest record: detected through the head.
    for sql in [
        "ALTER TABLE audit_log DISABLE TRIGGER audit_log_append_only",
        "UPDATE audit_log SET actor = 'alice' WHERE seq = 3",
        "DELETE FROM audit_log WHERE seq = 5",
        "ALTER TABLE audit_log ENABLE TRIGGER audit_log_append_only",
    ] {
        sqlx::query(sqlx::AssertSqlSafe(sql))
            .execute(&mut conn)
            .await
            .unwrap();
    }
    let e = verify(PgStore::connect(url, 1).await.unwrap())
        .await
        .unwrap_err();
    assert_eq!(e.seq, 5, "{e:?}");
}

#[tokio::test]
async fn memory_store_conformance() {
    let store = MemoryStore::new();
    suite(&store).await;
    phase5(&store).await;
    audit_chain(&store).await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn postgres_store_conformance_and_migrations() {
    let admin_url = std::env::var("TEST_DATABASE_URL").expect("set TEST_DATABASE_URL");
    let db = format!("rp_test_{}", uuid::Uuid::new_v4().simple());
    let mut admin = sqlx::PgConnection::connect(&admin_url).await.unwrap();
    // `db` is a generated hex identifier, never external input.
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {db}")))
        .execute(&mut admin)
        .await
        .unwrap();
    let url = match admin_url.rsplit_once('/') {
        Some((base, _)) => format!("{base}/{db}"),
        None => panic!("TEST_DATABASE_URL must include a database name"),
    };
    {
        let store = PgStore::connect(&url, 4).await.unwrap();
        store.migrate().await.unwrap();
        store.migrate().await.unwrap(); // re-running is a no-op
        suite(&store).await;
        phase5(&store).await;
        audit_chain(&store).await;
        audit_tampering(&url).await;
    }
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "DROP DATABASE {db} WITH (FORCE)"
    )))
    .execute(&mut admin)
    .await
    .unwrap();
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn postgres_bus_delivers_small_and_spilled_payloads() {
    use platform_storage::{MessageBus, PgBus};
    let admin_url = std::env::var("TEST_DATABASE_URL").expect("set TEST_DATABASE_URL");
    let db = format!("rp_bus_{}", uuid::Uuid::new_v4().simple());
    let mut admin = sqlx::PgConnection::connect(&admin_url).await.unwrap();
    // `db` is a generated hex identifier, never external input.
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {db}")))
        .execute(&mut admin)
        .await
        .unwrap();
    let url = format!("{}/{db}", admin_url.rsplit_once('/').unwrap().0);
    {
        let a = PgStore::connect(&url, 2).await.unwrap();
        a.migrate().await.unwrap();
        let b = PgStore::connect(&url, 2).await.unwrap();
        let bus_a = PgBus::start(a.pool().clone()).await.unwrap();
        let bus_b = PgBus::start(b.pool().clone()).await.unwrap();
        let mut rx = bus_b.subscribe();
        let big = "x".repeat(50_000);
        bus_a.publish("hello").await.unwrap();
        bus_a.publish(&big).await.unwrap();
        let wait = std::time::Duration::from_secs(5);
        assert_eq!(
            &*tokio::time::timeout(wait, rx.recv())
                .await
                .unwrap()
                .unwrap(),
            "hello"
        );
        assert_eq!(
            tokio::time::timeout(wait, rx.recv())
                .await
                .unwrap()
                .unwrap()
                .len(),
            50_000,
            "spilled payload delivered intact"
        );
        assert_eq!(
            bus_a.purge_spill(std::time::Duration::ZERO).await.unwrap(),
            1
        );
    }
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "DROP DATABASE {db} WITH (FORCE)"
    )))
    .execute(&mut admin)
    .await
    .unwrap();
}
