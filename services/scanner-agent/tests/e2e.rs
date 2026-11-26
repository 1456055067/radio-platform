//! End-to-end: simulator ⇄ agent ⇄ (mTLS WebSocket via a severable TCP
//! proxy) ⇄ gateway ⇄ REST/WebSocket clients. Simulated scanner only.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use api_gateway::config::LimitsConfig;
use api_gateway::{AppState, agent_app, public_app, serve, tls};
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use futures_util::StreamExt;
use http_body_util::BodyExt;
use platform_auth::testing::TestIssuer;
use platform_auth::{OidcConfig, OidcVerifier, Permission};
use platform_common::wire::ScannerAssignment;
use platform_common::{AgentId, NetworkPolicy, ScannerId, TenantId};
use platform_storage::MemoryStore;
use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    SanType,
};
use scanner_agent::config::{AgentConfig, GatewayConfig, TelemetryConfig, TelemetryMode};
use scanner_agent::{AgentHandle, AgentStatus};
use sds_simulator::{Simulator, SimulatorConfig, Transmission};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

// ---------- PKI ----------

struct Pki {
    dir: PathBuf,
}

fn ca(name: &str) -> (CertificateParams, KeyPair, String) {
    let mut p = CertificateParams::new(Vec::<String>::new()).unwrap();
    p.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    p.distinguished_name.push(DnType::CommonName, name);
    let k = KeyPair::generate().unwrap();
    let pem = p.self_signed(&k).unwrap().pem();
    (p, k, pem)
}

fn leaf(
    issuer: &Issuer<'_, KeyPair>,
    dns: &str,
    ip: Option<&str>,
    eku: ExtendedKeyUsagePurpose,
) -> (String, String) {
    let mut p = CertificateParams::new(vec![dns.to_owned()]).unwrap();
    if let Some(ip) = ip {
        p.subject_alt_names
            .push(SanType::IpAddress(ip.parse().unwrap()));
    }
    p.distinguished_name.push(DnType::CommonName, dns);
    p.extended_key_usages = vec![eku];
    let k = KeyPair::generate().unwrap();
    (p.signed_by(&k, issuer).unwrap().pem(), k.serialize_pem())
}

impl Pki {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("rp-e2e-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let (ca_p, ca_k, ca_pem) = ca("Radio Test CA");
        let issuer = Issuer::new(ca_p, ca_k);
        let (rogue_p, rogue_k, _) = ca("Rogue CA");
        let rogue = Issuer::new(rogue_p, rogue_k);
        let w = |n: &str, s: &str| std::fs::write(dir.join(n), s).unwrap();
        w("ca.pem", &ca_pem);
        let (c, k) = leaf(
            &issuer,
            "localhost",
            Some("127.0.0.1"),
            ExtendedKeyUsagePurpose::ServerAuth,
        );
        w("gw.pem", &c);
        w("gw-key.pem", &k);
        for (name, iss) in [("agent-a", &issuer), ("agent-unregistered", &issuer)] {
            let (c, k) = leaf(iss, name, None, ExtendedKeyUsagePurpose::ClientAuth);
            w(&format!("{name}.pem"), &c);
            w(&format!("{name}-key.pem"), &k);
        }
        let (c, k) = leaf(&rogue, "agent-a", None, ExtendedKeyUsagePurpose::ClientAuth);
        w("rogue.pem", &c);
        w("rogue-key.pem", &k);
        Self { dir }
    }
    fn p(&self, n: &str) -> PathBuf {
        self.dir.join(n)
    }
}

impl Drop for Pki {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

// ---------- Severable TCP proxy (simulates WAN outages) ----------

struct Proxy {
    addr: SocketAddr,
    down: Arc<AtomicBool>,
    conns: Arc<Mutex<Vec<tokio::task::AbortHandle>>>,
}

impl Proxy {
    async fn start(upstream: SocketAddr) -> Self {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        let down = Arc::new(AtomicBool::new(false));
        let conns: Arc<Mutex<Vec<tokio::task::AbortHandle>>> = Arc::default();
        let (d, c) = (Arc::clone(&down), Arc::clone(&conns));
        tokio::spawn(async move {
            loop {
                let Ok((mut inbound, _)) = l.accept().await else {
                    break;
                };
                if d.load(Ordering::SeqCst) {
                    continue;
                }
                let h = tokio::spawn(async move {
                    if let Ok(mut out) = tokio::net::TcpStream::connect(upstream).await {
                        let _ = tokio::io::copy_bidirectional(&mut inbound, &mut out).await;
                    }
                });
                c.lock().unwrap().push(h.abort_handle());
            }
        });
        Self { addr, down, conns }
    }
    fn cut(&self) {
        self.down.store(true, Ordering::SeqCst);
        for h in self.conns.lock().unwrap().drain(..) {
            h.abort();
        }
    }
    fn restore(&self) {
        self.down.store(false, Ordering::SeqCst);
    }
}

// ---------- Harness ----------

const ISS: &str = "https://idp.example.test";
const AUD: &str = "radio-platform";

struct Env {
    state: Arc<AppState>,
    issuer: TestIssuer,
    tenant: TenantId,
    agent_addr: SocketAddr,
    public_addr: SocketAddr,
    _cancel: tokio_util::sync::DropGuard,
}

impl Env {
    fn token(&self, sub: &str, role: &str) -> String {
        self.issuer.token(
            sub,
            600,
            json!({ "tenant_id": self.tenant.to_string(), "roles": [role], "sites": ["*"] }),
        )
    }
    async fn call(
        &self,
        method: Method,
        uri: &str,
        token: &str,
        body: Option<Value>,
        lease: Option<&str>,
    ) -> (StatusCode, Value) {
        let mut b = Request::builder()
            .method(method)
            .uri(uri)
            .header("authorization", format!("Bearer {token}"));
        if let Some(l) = lease {
            b = b.header("x-lease-id", l);
        }
        let req = match body {
            Some(v) => b
                .header("content-type", "application/json")
                .body(Body::from(v.to_string()))
                .unwrap(),
            None => b.body(Body::empty()).unwrap(),
        };
        let resp = public_app(Arc::clone(&self.state))
            .oneshot(req)
            .await
            .unwrap();
        let s = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (s, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }
}

async fn gateway(pki: &Pki, scanner_port: u16) -> Env {
    gateway_ports(pki, &[scanner_port]).await
}

async fn gateway_ports(pki: &Pki, scanner_ports: &[u16]) -> Env {
    let issuer = TestIssuer::new(ISS, AUD);
    let mut oidc = OidcConfig::new(ISS, AUD);
    oidc.role_permissions = BTreeMap::from([
        ("admin".into(), Permission::ALL.to_vec()),
        (
            "operator".into(),
            vec![
                Permission::ScannerView,
                Permission::ScannerControl,
                Permission::EventView,
            ],
        ),
    ]);
    let limits = LimitsConfig {
        scanner_ports: scanner_ports.to_vec(),
        command_timeout_secs: 5,
        ..LimitsConfig::default()
    };
    let state = AppState::new(
        Arc::new(MemoryStore::new()),
        Arc::new(OidcVerifier::with_static_jwks(oidc, issuer.jwks())),
        limits,
    );
    let cancel = CancellationToken::new();
    let mtls =
        tls::mtls_server_config(&pki.p("gw.pem"), &pki.p("gw-key.pem"), &pki.p("ca.pem")).unwrap();
    let al = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let agent_addr = al.local_addr().unwrap();
    tokio::spawn(serve::serve(
        al,
        Some(mtls),
        agent_app(Arc::clone(&state)),
        64,
        cancel.clone(),
    ));
    let pl = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let public_addr = pl.local_addr().unwrap();
    tokio::spawn(serve::serve(
        pl,
        None,
        public_app(Arc::clone(&state)),
        64,
        cancel.clone(),
    ));
    Env {
        state,
        issuer,
        tenant: TenantId::new(),
        agent_addr,
        public_addr,
        _cancel: cancel.drop_guard(),
    }
}

fn agent_config(pki: &Pki, gw: SocketAddr, cert: &str, sim: SocketAddr) -> AgentConfig {
    AgentConfig {
        gateway: GatewayConfig {
            url: format!("wss://127.0.0.1:{}/agent/v1", gw.port()),
            ca: pki.p("ca.pem"),
            cert: pki.p(&format!("{cert}.pem")),
            key: pki.p(&format!("{cert}-key.pem")),
        },
        network: NetworkPolicy {
            allowed_networks: vec!["127.0.0.1/32".parse().unwrap()],
            allowed_ports: vec![sim.port()],
            audio_ports: vec![554],
        },
        telemetry: TelemetryConfig {
            mode: TelemetryMode::Poll,
            interval_ms: 100,
            hang_time_ms: 300,
        },
        max_buffered_events: 1000,
        state_dir: None,
        max_scanners: 8,
        scanners: vec![],
        audio: scanner_agent::audio::AudioSettings::default(),
    }
}

fn agent_with_paths(
    pki: &Pki,
    gw: SocketAddr,
    cert: &Path,
    key: &Path,
    sim: SocketAddr,
) -> AgentConfig {
    let mut c = agent_config(pki, gw, "agent-a", sim);
    c.gateway.cert = cert.to_owned();
    c.gateway.key = key.to_owned();
    c
}

async fn eventually<F, Fut>(what: &str, timeout: Duration, mut f: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + timeout;
    while tokio::time::Instant::now() < deadline {
        if f().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("timed out waiting for: {what}");
}

async fn wait_status(a: &AgentHandle, what: &str, pred: impl Fn(AgentStatus) -> bool) {
    eventually(what, Duration::from_secs(10), || {
        let s = a.status();
        let ok = pred(s);
        async move { ok }
    })
    .await;
}

async fn transmissions(env: &Env, token: &str) -> Vec<Value> {
    env.call(Method::GET, "/api/v1/transmissions", token, None, None)
        .await
        .1["items"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn full_stack() {
    let sim = Simulator::start(SimulatorConfig::default()).await.unwrap();
    let pki = Pki::new();
    let env = gateway(&pki, sim.addr().port()).await;
    let proxy = Proxy::start(env.agent_addr).await;
    let admin = env.token("admin", "admin");
    let alice = env.token("alice", "operator");

    // Enrollment through the API.
    let (s, site) = env
        .call(
            Method::POST,
            "/api/v1/sites",
            &admin,
            Some(json!({ "name": "Lab", "allowed_networks": ["127.0.0.1/32"] })),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::CREATED);
    let (s, _) = env
        .call(
            Method::POST,
            "/api/v1/agents",
            &admin,
            Some(json!({ "agent_id": "agent-a", "site_id": site["id"] })),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::CREATED);
    let (s, sc) = env
        .call(Method::POST, "/api/v1/scanners", &admin, Some(json!({ "site_id": site["id"], "agent_id": "agent-a", "name": "Sim", "host": "127.0.0.1", "port": sim.addr().port() })), None)
        .await;
    assert_eq!(s, StatusCode::CREATED, "{sc}");
    let scanner: ScannerId = sc["id"].as_str().unwrap().parse().unwrap();
    let base = format!("/api/v1/scanners/{scanner}");

    // Agent connects via the proxy and picks up the central assignment.
    let agent =
        scanner_agent::start(agent_config(&pki, proxy.addr, "agent-a", sim.addr())).unwrap();
    wait_status(&agent, "agent connected", |s| s.connected).await;
    eventually("model reported", Duration::from_secs(10), || async {
        env.call(Method::GET, &base, &admin, None, None).await.1["model"] == "SDS200"
    })
    .await;
    eventually("telemetry stored", Duration::from_secs(10), || async {
        !env.call(
            Method::GET,
            &format!("{base}/telemetry"),
            &admin,
            None,
            None,
        )
        .await
        .1["snapshot"]
            .is_null()
    })
    .await;

    // Live stream over a real WebSocket.
    let mut req = tokio_tungstenite::tungstenite::client::IntoClientRequest::into_client_request(
        format!("ws://{}/api/v1/events/stream", env.public_addr),
    )
    .unwrap();
    req.headers_mut()
        .insert("authorization", format!("Bearer {alice}").parse().unwrap());
    let tcp = tokio::net::TcpStream::connect(env.public_addr)
        .await
        .unwrap();
    let (mut live, _) = tokio_tungstenite::client_async(req, tcp).await.unwrap();

    // Control through a lease.
    let (s, lease) = env
        .call(
            Method::POST,
            &format!("{base}/leases"),
            &alice,
            Some(json!({})),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::CREATED);
    let lease_id = lease["lease_id"].as_str().unwrap().to_owned();
    let (s, body) = env
        .call(
            Method::PUT,
            &format!("{base}/volume"),
            &alice,
            Some(json!({ "level": 12 })),
            Some(&lease_id),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{body}");
    assert_eq!(sim.state().volume, 12);
    let (_, v) = env
        .call(Method::GET, &format!("{base}/volume"), &alice, None, None)
        .await;
    assert_eq!(v["level"], 12);
    let (s, _) = env
        .call(
            Method::PUT,
            &format!("{base}/squelch"),
            &alice,
            Some(json!({ "level": 99 })),
            Some(&lease_id),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "SDK validation surfaces as 400");
    assert_eq!(
        env.call(
            Method::POST,
            &format!("{base}/hold"),
            &alice,
            Some(json!({ "kind": "system", "index": 100 })),
            Some(&lease_id)
        )
        .await
        .0,
        StatusCode::OK
    );
    assert!(sim.state().hold);
    assert_eq!(
        env.call(
            Method::POST,
            &format!("{base}/resume"),
            &alice,
            None,
            Some(&lease_id)
        )
        .await
        .0,
        StatusCode::OK
    );
    assert!(!sim.state().hold);
    let (s, caps) = env
        .call(
            Method::GET,
            &format!("{base}/capabilities"),
            &alice,
            None,
            None,
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(caps["commands"]["GSI"]["status"], "supported");
    let (s, raw) = env
        .call(
            Method::POST,
            &format!("{base}/commands"),
            &admin,
            Some(json!({ "command": "raw", "name": "MDL", "args": [], "read_only": true })),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(raw["fields"][0], "SDS200");

    // Live telemetry reaches the WebSocket subscriber.
    let msg = tokio::time::timeout(Duration::from_secs(5), live.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let live_json: Value = serde_json::from_str(msg.to_text().unwrap()).unwrap();
    assert_eq!(live_json["scanner_id"], scanner.to_string());

    // Transmission detection end-to-end.
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
    eventually("transmission started", Duration::from_secs(10), || async {
        transmissions(&env, &alice).await.len() == 1
    })
    .await;
    sim.end_transmission();
    eventually("transmission ended", Duration::from_secs(10), || async {
        transmissions(&env, &alice)
            .await
            .first()
            .is_some_and(|t| !t["ended_at"].is_null())
    })
    .await;
    let t = &transmissions(&env, &alice).await[0];
    assert_eq!(t["tgid"], "1234");
    assert_eq!(t["unit_id"], "501234");
    assert_eq!(t["frequency_hz"], 769_268_750u64);
    assert_eq!(t["channel_name"], "PD Dispatch");

    // WAN outage: events are buffered and replayed after reconnect.
    proxy.cut();
    wait_status(&agent, "agent disconnected", |s| !s.connected).await;
    let (s, _) = env
        .call(Method::GET, &format!("{base}/volume"), &alice, None, None)
        .await;
    assert_eq!(
        s,
        StatusCode::SERVICE_UNAVAILABLE,
        "commands fail fast while the agent is offline"
    );
    sim.begin_transmission(Transmission {
        frequency_hz: 460_000_000,
        system: "County".into(),
        channel: "Fire".into(),
        modulation: "FM".into(),
        ..Transmission::default()
    });
    tokio::time::sleep(Duration::from_millis(400)).await;
    sim.end_transmission();
    wait_status(&agent, "events buffered", |s| s.pending_events >= 2).await;
    proxy.restore();
    wait_status(&agent, "replayed and acknowledged", |s| {
        s.connected && s.pending_events == 0
    })
    .await;
    eventually(
        "buffered transmission stored",
        Duration::from_secs(10),
        || async {
            transmissions(&env, &alice)
                .await
                .iter()
                .any(|t| t["frequency_hz"] == 460_000_000u64 && !t["ended_at"].is_null())
        },
    )
    .await;

    // Audit trail of operator actions.
    let (_, audit) = env
        .call(Method::GET, "/api/v1/audit?limit=200", &admin, None, None)
        .await;
    let actions: Vec<&str> = audit["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|a| a["outcome"] == "success")
        .filter_map(|a| a["action"].as_str())
        .collect();
    for a in [
        "scanner.create",
        "scanner.lease.acquire",
        "scanner.command.set_volume",
        "scanner.command.hold",
        "scanner.command.raw",
    ] {
        assert!(actions.contains(&a), "missing audit {a}: {actions:?}");
    }

    // A hostile gateway cannot make the agent contact out-of-policy hosts.
    let sim2 = Simulator::start(SimulatorConfig::default()).await.unwrap();
    let hostile = vec![ScannerAssignment {
        scanner_id: ScannerId::new(),
        site_id: None,
        name: "x".into(),
        host: sim2.addr().ip(),
        port: sim2.addr().port(),
        enabled: true,
        audio: None,
    }];
    assert!(
        env.state
            .hub
            .configure(&AgentId::new("agent-a").unwrap(), hostile)
    );
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        sim2.received().is_empty(),
        "agent must refuse endpoints outside its local policy"
    );

    agent.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mtls_rejects_untrusted_and_unregistered_agents() {
    let sim = Simulator::start(SimulatorConfig::default()).await.unwrap();
    let pki = Pki::new();
    let env = gateway(&pki, sim.addr().port()).await;
    let admin = env.token("admin", "admin");
    let (_, site) = env
        .call(
            Method::POST,
            "/api/v1/sites",
            &admin,
            Some(json!({ "name": "Lab", "allowed_networks": ["127.0.0.1/32"] })),
            None,
        )
        .await;
    env.call(
        Method::POST,
        "/api/v1/agents",
        &admin,
        Some(json!({ "agent_id": "agent-a", "site_id": site["id"] })),
        None,
    )
    .await;

    // Certificate from an untrusted CA (even with a registered identity).
    let rogue = scanner_agent::start(agent_with_paths(
        &pki,
        env.agent_addr,
        &pki.p("rogue.pem"),
        &pki.p("rogue-key.pem"),
        sim.addr(),
    ))
    .unwrap();
    // Trusted CA, but the identity is not registered.
    let unreg = scanner_agent::start(agent_config(
        &pki,
        env.agent_addr,
        "agent-unregistered",
        sim.addr(),
    ))
    .unwrap();
    // Control: the legitimate agent connects.
    let good =
        scanner_agent::start(agent_config(&pki, env.agent_addr, "agent-a", sim.addr())).unwrap();
    wait_status(&good, "legitimate agent connected", |s| s.connected).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(!rogue.status().connected && rogue.status().sessions == 0);
    assert!(!unreg.status().connected && unreg.status().sessions == 0);
    assert!(
        !env.state
            .hub
            .is_connected(&AgentId::new("agent-unregistered").unwrap())
    );
    for a in [rogue, unreg, good] {
        a.shutdown().await;
    }
}

/// Agent restart during a WAN outage: with a state directory, buffered
/// events survive the restart and reach the gateway exactly once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn buffered_events_survive_agent_restart() {
    let sim = Simulator::start(SimulatorConfig::default()).await.unwrap();
    let pki = Pki::new();
    let env = gateway(&pki, sim.addr().port()).await;
    let proxy = Proxy::start(env.agent_addr).await;
    let admin = env.token("admin", "admin");
    let (_, site) = env
        .call(
            Method::POST,
            "/api/v1/sites",
            &admin,
            Some(json!({ "name": "Lab", "allowed_networks": ["127.0.0.1/32"] })),
            None,
        )
        .await;
    env.call(
        Method::POST,
        "/api/v1/agents",
        &admin,
        Some(json!({ "agent_id": "agent-a", "site_id": site["id"] })),
        None,
    )
    .await;
    let (s, _) = env
        .call(Method::POST, "/api/v1/scanners", &admin, Some(json!({ "site_id": site["id"], "agent_id": "agent-a", "name": "Sim", "host": "127.0.0.1", "port": sim.addr().port() })), None)
        .await;
    assert_eq!(s, StatusCode::CREATED);

    let state_dir = pki.p("agent-state");
    let mut cfg = agent_config(&pki, proxy.addr, "agent-a", sim.addr());
    cfg.state_dir = Some(state_dir.clone());
    let agent = scanner_agent::start(cfg.clone()).unwrap();
    wait_status(&agent, "connected and durable", |s| {
        s.connected && s.durable
    })
    .await;
    // Let the central assignment arrive so collection runs.
    tokio::time::sleep(Duration::from_millis(500)).await;

    proxy.cut();
    wait_status(&agent, "disconnected", |s| !s.connected).await;
    sim.begin_transmission(Transmission {
        frequency_hz: 155_475_000,
        system: "Example".into(),
        channel: "Durable".into(),
        modulation: "FM".into(),
        ..Transmission::default()
    });
    tokio::time::sleep(Duration::from_millis(400)).await;
    sim.end_transmission();
    wait_status(&agent, "events buffered", |s| s.pending_events >= 2).await;

    // Restart the agent while the gateway is still unreachable.
    agent.shutdown().await;
    let agent = scanner_agent::start(cfg).unwrap();
    wait_status(&agent, "events recovered from disk", |s| {
        s.pending_events >= 2 && !s.connected
    })
    .await;

    proxy.restore();
    wait_status(&agent, "replayed and acknowledged", |s| {
        s.connected && s.pending_events == 0
    })
    .await;
    eventually(
        "recovered transmission stored",
        Duration::from_secs(10),
        || async {
            transmissions(&env, &admin)
                .await
                .iter()
                .any(|t| t["frequency_hz"] == 155_475_000u64 && !t["ended_at"].is_null())
        },
    )
    .await;
    let matching = transmissions(&env, &admin)
        .await
        .into_iter()
        .filter(|t| t["frequency_hz"] == 155_475_000u64)
        .count();
    assert_eq!(matching, 1, "replayed exactly once");
    agent.shutdown().await;
}

// ---------- Two gateway replicas ----------

struct Replica {
    state: Arc<AppState>,
    agent_addr: SocketAddr,
    public_addr: SocketAddr,
}

async fn replica(
    pki: &Pki,
    issuer: &TestIssuer,
    store: Arc<dyn platform_storage::Store>,
    bus: Arc<dyn platform_storage::MessageBus>,
    scanner_port: u16,
    cancel: &CancellationToken,
) -> Replica {
    let mut oidc = OidcConfig::new(ISS, AUD);
    oidc.role_permissions = BTreeMap::from([
        ("admin".into(), Permission::ALL.to_vec()),
        (
            "operator".into(),
            vec![
                Permission::ScannerView,
                Permission::ScannerControl,
                Permission::EventView,
            ],
        ),
    ]);
    let state = AppState::clustered(
        store,
        Arc::new(OidcVerifier::with_static_jwks(oidc, issuer.jwks())),
        LimitsConfig {
            scanner_ports: vec![scanner_port],
            command_timeout_secs: 5,
            ..LimitsConfig::default()
        },
        bus,
    );
    let mtls =
        tls::mtls_server_config(&pki.p("gw.pem"), &pki.p("gw-key.pem"), &pki.p("ca.pem")).unwrap();
    let al = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let agent_addr = al.local_addr().unwrap();
    tokio::spawn(serve::serve(
        al,
        Some(mtls),
        agent_app(Arc::clone(&state)),
        64,
        cancel.clone(),
    ));
    let pl = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let public_addr = pl.local_addr().unwrap();
    tokio::spawn(serve::serve(
        pl,
        None,
        public_app(Arc::clone(&state)),
        64,
        cancel.clone(),
    ));
    Replica {
        state,
        agent_addr,
        public_addr,
    }
}

async fn call_on(
    r: &Replica,
    method: Method,
    uri: &str,
    token: &str,
    body: Option<Value>,
    lease: Option<&str>,
) -> (StatusCode, Value) {
    let mut b = Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {token}"));
    if let Some(l) = lease {
        b = b.header("x-lease-id", l);
    }
    let req = match body {
        Some(v) => b
            .header("content-type", "application/json")
            .body(Body::from(v.to_string()))
            .unwrap(),
        None => b.body(Body::empty()).unwrap(),
    };
    let resp = public_app(Arc::clone(&r.state)).oneshot(req).await.unwrap();
    let s = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (s, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

/// The agent is connected to replica A; every client request goes to
/// replica B. Leases, commands, live updates, configuration pushes and
/// presence must all work across the bus.
async fn two_replicas(
    store_a: Arc<dyn platform_storage::Store>,
    store_b: Arc<dyn platform_storage::Store>,
    bus_a: Arc<dyn platform_storage::MessageBus>,
    bus_b: Arc<dyn platform_storage::MessageBus>,
) {
    let sim = Simulator::start(SimulatorConfig::default()).await.unwrap();
    let pki = Pki::new();
    let issuer = TestIssuer::new(ISS, AUD);
    let cancel = CancellationToken::new();
    let _guard = cancel.clone().drop_guard();
    let port = sim.addr().port();
    let a = replica(&pki, &issuer, store_a, bus_a, port, &cancel).await;
    let b = replica(&pki, &issuer, store_b, bus_b, port, &cancel).await;
    let tenant = TenantId::new();
    let tok = |sub: &str, role: &str| {
        issuer.token(
            sub,
            600,
            json!({ "tenant_id": tenant.to_string(), "roles": [role], "sites": ["*"] }),
        )
    };
    let (admin, alice, bob) = (
        tok("admin", "admin"),
        tok("alice", "operator"),
        tok("bob", "operator"),
    );

    let (_, site) = call_on(
        &b,
        Method::POST,
        "/api/v1/sites",
        &admin,
        Some(json!({ "name": "Lab", "allowed_networks": ["127.0.0.1/32"] })),
        None,
    )
    .await;
    call_on(
        &b,
        Method::POST,
        "/api/v1/agents",
        &admin,
        Some(json!({ "agent_id": "agent-a", "site_id": site["id"] })),
        None,
    )
    .await;
    let (s, sc) = call_on(&b, Method::POST, "/api/v1/scanners", &admin, Some(json!({ "site_id": site["id"], "agent_id": "agent-a", "name": "Sim", "host": "127.0.0.1", "port": port })), None).await;
    assert_eq!(s, StatusCode::CREATED, "{sc}");
    let base = format!("/api/v1/scanners/{}", sc["id"].as_str().unwrap());

    // Agent dials replica A only.
    let agent =
        scanner_agent::start(agent_config(&pki, a.agent_addr, "agent-a", sim.addr())).unwrap();
    wait_status(&agent, "agent connected to A", |s| s.connected).await;
    eventually(
        "B learns the agent is connected (presence)",
        Duration::from_secs(10),
        || async {
            call_on(&b, Method::GET, &base, &admin, None, None).await.1["agent_connected"] == true
        },
    )
    .await;

    // Live stream on B sees telemetry produced at A.
    let mut req = tokio_tungstenite::tungstenite::client::IntoClientRequest::into_client_request(
        format!("ws://{}/api/v1/events/stream", b.public_addr),
    )
    .unwrap();
    req.headers_mut()
        .insert("authorization", format!("Bearer {alice}").parse().unwrap());
    let tcp = tokio::net::TcpStream::connect(b.public_addr).await.unwrap();
    let (mut live, _) = tokio_tungstenite::client_async(req, tcp).await.unwrap();
    sim.with_state(|s| s.volume = 4);
    let got = tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(Ok(m)) = live.next().await {
            if let tokio_tungstenite::tungstenite::Message::Text(t) = m {
                let v: Value = serde_json::from_str(&t).unwrap();
                if v["kind"] == "telemetry" {
                    return true;
                }
            }
        }
        false
    })
    .await;
    assert_eq!(got, Ok(true), "telemetry from A reaches B's subscribers");

    // Leases are shared: alice holds via B, bob is refused via A.
    let (s, lease) = call_on(
        &b,
        Method::POST,
        &format!("{base}/leases"),
        &alice,
        Some(json!({})),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    let lease_id = lease["lease_id"].as_str().unwrap().to_owned();
    let (s, _) = call_on(
        &a,
        Method::POST,
        &format!("{base}/leases"),
        &bob,
        Some(json!({})),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "lease visible on every replica");

    // A command sent to B is forwarded to A, which holds the agent.
    let (s, body) = call_on(
        &b,
        Method::PUT,
        &format!("{base}/volume"),
        &alice,
        Some(json!({ "level": 17 })),
        Some(&lease_id),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{body}");
    assert_eq!(sim.state().volume, 17);
    let (s, v) = call_on(&b, Method::GET, &format!("{base}/volume"), &bob, None, None).await;
    assert_eq!((s, v["level"].clone()), (StatusCode::OK, json!(17)));
    // The lease can be used from either replica.
    let (s, _) = call_on(
        &a,
        Method::PUT,
        &format!("{base}/volume"),
        &alice,
        Some(json!({ "level": 9 })),
        Some(&lease_id),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(sim.state().volume, 9);

    // Configuration pushed from B reaches the agent on A: disabling the
    // scanner stops the agent talking to it.
    let (s, _) = call_on(
        &b,
        Method::PATCH,
        &base,
        &admin,
        Some(json!({ "enabled": false })),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    tokio::time::sleep(Duration::from_millis(500)).await;
    let before = sim.received().len();
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!(
        sim.received().len(),
        before,
        "agent on A stopped polling after B's change"
    );

    // Disconnect is noticed on B.
    agent.shutdown().await;
    eventually(
        "B learns the agent left",
        Duration::from_secs(10),
        || async {
            call_on(&b, Method::GET, &base, &admin, None, None).await.1["agent_connected"] == false
        },
    )
    .await;
    let (s, _) = call_on(&b, Method::GET, &format!("{base}/volume"), &bob, None, None).await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_replicas_in_process() {
    let store: Arc<dyn platform_storage::Store> = Arc::new(MemoryStore::new());
    let bus = platform_storage::MemoryBus::new();
    two_replicas(
        Arc::clone(&store),
        store,
        Arc::new(bus.clone()),
        Arc::new(bus),
    )
    .await;
}

/// Same scenario over PostgreSQL: separate pools and LISTEN connections
/// per replica, as in production.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires TEST_DATABASE_URL"]
async fn two_replicas_over_postgres() {
    use sqlx::Connection;
    let admin_url = std::env::var("TEST_DATABASE_URL").expect("set TEST_DATABASE_URL");
    let db = format!("rp_cluster_{}", uuid::Uuid::new_v4().simple());
    let mut admin = sqlx::PgConnection::connect(&admin_url).await.unwrap();
    // `db` is a generated hex identifier, never external input.
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {db}")))
        .execute(&mut admin)
        .await
        .unwrap();
    let url = format!("{}/{db}", admin_url.rsplit_once('/').unwrap().0);
    {
        let pa = platform_storage::PgStore::connect(&url, 4).await.unwrap();
        pa.migrate().await.unwrap();
        let pb = platform_storage::PgStore::connect(&url, 4).await.unwrap();
        let bus_a = platform_storage::PgBus::start(pa.pool().clone())
            .await
            .unwrap();
        let bus_b = platform_storage::PgBus::start(pb.pool().clone())
            .await
            .unwrap();
        two_replicas(Arc::new(pa), Arc::new(pb), Arc::new(bus_a), Arc::new(bus_b)).await;
    }
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "DROP DATABASE {db} WITH (FORCE)"
    )))
    .execute(&mut admin)
    .await
    .unwrap();
}

/// Discovery finds an unenrolled simulator, never probes the enrolled one,
/// and is refused by the agent outside its own policy even when the site
/// would allow it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn discovery_respects_policy_and_skips_enrolled() {
    let enrolled = Simulator::start(SimulatorConfig::default()).await.unwrap();
    let stray = Simulator::start(SimulatorConfig::default()).await.unwrap();
    let (p1, p2) = (enrolled.addr().port(), stray.addr().port());
    let pki = Pki::new();
    let env = gateway_ports(&pki, &[p1, p2]).await;
    let admin = env.token("admin", "admin");
    let operator = env.token("olga", "operator");
    // The site allows a /24; the agent itself only allows 127.0.0.1/32.
    let (_, site) = env
        .call(
            Method::POST,
            "/api/v1/sites",
            &admin,
            Some(json!({ "name": "Lab", "allowed_networks": ["127.0.0.0/24"] })),
            None,
        )
        .await;
    env.call(
        Method::POST,
        "/api/v1/agents",
        &admin,
        Some(json!({ "agent_id": "agent-a", "site_id": site["id"] })),
        None,
    )
    .await;
    let (s, _) = env
        .call(Method::POST, "/api/v1/scanners", &admin, Some(json!({ "site_id": site["id"], "agent_id": "agent-a", "name": "Known", "host": "127.0.0.1", "port": p1 })), None)
        .await;
    assert_eq!(s, StatusCode::CREATED);
    let mut cfg = agent_config(&pki, env.agent_addr, "agent-a", enrolled.addr());
    cfg.network.allowed_ports = vec![p1, p2];
    let agent = scanner_agent::start(cfg).unwrap();
    wait_status(&agent, "connected", |s| s.connected).await;
    eventually(
        "enrolled scanner managed",
        Duration::from_secs(10),
        || async {
            env.call(Method::GET, "/api/v1/scanners", &admin, None, None)
                .await
                .1["items"][0]["model"]
                == "SDS200"
        },
    )
    .await;
    let uri = format!("/api/v1/sites/{}/discover", site["id"].as_str().unwrap());
    let mdl_count = |sim: &Simulator| {
        sim.received()
            .iter()
            .filter(|c| c.starts_with("MDL"))
            .count()
    };
    let before = mdl_count(&enrolled);

    let (s, r) = env
        .call(
            Method::POST,
            &uri,
            &admin,
            Some(json!({ "agent_id": "agent-a", "networks": ["127.0.0.1/32"], "port": p2 })),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{r}");
    assert_eq!(
        r["candidates"],
        json!([{ "host": "127.0.0.1", "port": p2, "model": "SDS200" }])
    );
    assert_eq!(mdl_count(&stray), 1, "one read-only probe");

    let (s, r) = env
        .call(
            Method::POST,
            &uri,
            &admin,
            Some(json!({ "agent_id": "agent-a", "networks": ["127.0.0.1/32"], "port": p1 })),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{r}");
    assert_eq!(r["candidates"], json!([]));
    assert_eq!(r["skipped_enrolled"], 1);
    assert_eq!(
        mdl_count(&enrolled),
        before,
        "the managed scanner is never probed"
    );

    // Inside the site but outside the agent's own policy: the agent refuses.
    let (s, r) = env
        .call(
            Method::POST,
            &uri,
            &admin,
            Some(json!({ "agent_id": "agent-a", "port": p2 })),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{r}");
    assert!(
        r["detail"].as_str().unwrap().contains("outside the agent"),
        "{r}"
    );
    // Outside the site: refused by the gateway.
    let (s, _) = env
        .call(
            Method::POST,
            &uri,
            &admin,
            Some(json!({ "agent_id": "agent-a", "networks": ["10.0.0.0/30"], "port": p2 })),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let (s, _) = env
        .call(
            Method::POST,
            &uri,
            &admin,
            Some(json!({ "agent_id": "agent-a", "networks": ["127.0.0.1/32"], "port": 22 })),
            None,
        )
        .await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "port must be an allowed scanner port"
    );
    // Operators cannot discover.
    let (s, _) = env
        .call(
            Method::POST,
            &uri,
            &operator,
            Some(json!({ "agent_id": "agent-a", "networks": ["127.0.0.1/32"], "port": p2 })),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    agent.shutdown().await;
}

// ---------- Audio (Phase 3) ----------

/// Full audio path against simulated hardware: scanner RTSP/RTP → agent
/// (PCMU → Opus) → gateway → WebRTC listener (a str0m client standing in
/// for the browser) and → Ogg Opus recording of a transmission.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_audio_and_recording() {
    use str0m::change::SdpAnswer;
    use str0m::media::{Direction, MediaKind};
    use str0m::net::{Protocol, Receive};
    use str0m::{Candidate, Event, Input, Output, RtcConfig};

    let sim = Simulator::start(SimulatorConfig {
        audio: Some("127.0.0.1:0".parse().unwrap()),
        ..SimulatorConfig::default()
    })
    .await
    .unwrap();
    let rtsp = sim.rtsp_addr().unwrap();
    let pki = Pki::new();
    let mut env = gateway(&pki, sim.addr().port()).await;
    // Rebuild state with the simulator's RTSP port allowed.
    {
        let mut limits = env.state.limits.clone();
        limits.audio_ports = vec![rtsp.port()];
        let mut oidc = OidcConfig::new(ISS, AUD);
        oidc.role_permissions = BTreeMap::from([
            ("admin".into(), Permission::ALL.to_vec()),
            (
                "listener".into(),
                vec![
                    Permission::ScannerView,
                    Permission::EventView,
                    Permission::AudioListen,
                ],
            ),
            (
                "viewer".into(),
                vec![Permission::ScannerView, Permission::EventView],
            ),
        ]);
        let state = AppState::new(
            Arc::new(MemoryStore::new()),
            Arc::new(OidcVerifier::with_static_jwks(oidc, env.issuer.jwks())),
            limits,
        );
        let mtls =
            tls::mtls_server_config(&pki.p("gw.pem"), &pki.p("gw-key.pem"), &pki.p("ca.pem"))
                .unwrap();
        let al = TcpListener::bind("127.0.0.1:0").await.unwrap();
        env.agent_addr = al.local_addr().unwrap();
        let cancel = CancellationToken::new();
        tokio::spawn(serve::serve(
            al,
            Some(mtls),
            agent_app(Arc::clone(&state)),
            64,
            cancel.clone(),
        ));
        std::mem::forget(cancel.clone().drop_guard());
        let server = media_gateway::WebRtcServer::start(
            media_gateway::WebRtcConfig::new("127.0.0.1:0".parse().unwrap()),
            state.media.hub.clone(),
        )
        .await
        .unwrap();
        state.media.enable_webrtc(server, 4);
        let recordings =
            scanner_recorder::RecordingStore::open(&scanner_recorder::StorageConfig::Memory)
                .unwrap();
        state.media.enable_recording(
            recordings.clone(),
            scanner_recorder::SegmentPolicy::default(),
            Arc::clone(&state.store),
        );
        // Transcription against the stub service (it names the tone).
        let stub = sds_simulator::whisper::start("127.0.0.1:0".parse().unwrap(), cancel.clone())
            .await
            .unwrap();
        let tcfg: scanner_transcription::TranscriptionConfig =
            toml::from_str(&format!("endpoint = \"{}\"", stub.url())).unwrap();
        let t = Arc::new(api_gateway::transcription::Transcription::new(tcfg).unwrap());
        state.media.enable_transcription(Arc::clone(&t));
        tokio::spawn(t.run(
            Arc::clone(&state.store),
            recordings,
            Arc::clone(&state.media.transcription_wake),
            cancel.clone(),
        ));
        env.state = state;
    }
    let admin = env.token("admin", "admin");
    let listener = env.token("lena", "listener");
    let viewer = env.token("vic", "viewer");

    let (_, site) = env
        .call(
            Method::POST,
            "/api/v1/sites",
            &admin,
            Some(json!({ "name": "Lab", "allowed_networks": ["127.0.0.1/32"] })),
            None,
        )
        .await;
    env.call(
        Method::POST,
        "/api/v1/agents",
        &admin,
        Some(json!({ "agent_id": "agent-a", "site_id": site["id"] })),
        None,
    )
    .await;
    let (_, sc) = env.call(Method::POST, "/api/v1/scanners", &admin, Some(json!({ "site_id": site["id"], "agent_id": "agent-a", "name": "Sim", "host": "127.0.0.1", "port": sim.addr().port() })), None).await;
    let base = format!("/api/v1/scanners/{}", sc["id"].as_str().unwrap());
    assert_eq!(sc["audio_enabled"], false, "audio is off by default");

    // Recording needs recording_manage; then enable audio + recording.
    let (s, _) = env
        .call(
            Method::PATCH,
            &base,
            &admin,
            Some(json!({ "rtsp_port": 9 })),
            None,
        )
        .await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "RTSP port must be allowed"
    );
    let (s, body) = env
        .call(Method::PATCH, &base, &admin, Some(json!({ "audio_enabled": true, "recording_enabled": true, "transcription_enabled": true, "rtsp_port": rtsp.port() })), None)
        .await;
    assert_eq!(s, StatusCode::OK, "{body}");
    assert_eq!(
        (
            body["audio_enabled"].clone(),
            body["recording_enabled"].clone()
        ),
        (json!(true), json!(true))
    );

    let mut cfg = agent_config(&pki, env.agent_addr, "agent-a", sim.addr());
    cfg.network.audio_ports = vec![rtsp.port()];
    cfg.audio = scanner_agent::audio::AudioSettings {
        cooldown_secs: 1,
        backoff_max_secs: 2,
        linger_secs: 1,
    };
    let agent = scanner_agent::start(cfg).unwrap();
    wait_status(&agent, "connected", |s| s.connected).await;
    eventually(
        "recording scanner streams audio",
        Duration::from_secs(10),
        || async {
            sim.audio_stats().sessions == 1
                && env
                    .state
                    .media
                    .hub
                    .status(sc["id"].as_str().unwrap().parse().unwrap())
                    .receiving
        },
    )
    .await;

    // A recorded transmission.
    sim.begin_transmission(Transmission {
        frequency_hz: 851_012_500,
        channel: "FD Main".into(),
        system: "Example".into(),
        modulation: "FM".into(),
        ..Transmission::default()
    });
    tokio::time::sleep(Duration::from_millis(1500)).await;
    sim.end_transmission();
    eventually("recording attached", Duration::from_secs(15), || async {
        transmissions(&env, &admin)
            .await
            .iter()
            .any(|t| !t["audio_object"].is_null())
    })
    .await;
    let tx_id = transmissions(&env, &admin)
        .await
        .iter()
        .find(|t| !t["audio_object"].is_null())
        .and_then(|t| t["id"].as_str().map(str::to_owned))
        .unwrap();
    let play = |token: String| {
        let req = Request::get(format!("/api/v1/transmissions/{tx_id}/audio"))
            .header("authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();
        let app = public_app(Arc::clone(&env.state));
        async move { app.oneshot(req).await.unwrap() }
    };
    // The recording is transcribed; the stub names the channel's tone.
    eventually("transcribed", Duration::from_secs(15), || async {
        transmissions(&env, &admin)
            .await
            .iter()
            .any(|t| t["id"] == tx_id.as_str() && t["transcription_state"] == "done")
    })
    .await;
    let (_, t) = env
        .call(
            Method::GET,
            &format!("/api/v1/transmissions/{tx_id}"),
            &listener,
            None,
            None,
        )
        .await;
    let text = t["transcript"].as_str().unwrap().to_owned();
    let hz: f64 = text
        .strip_prefix("Simulated transcript: ")
        .and_then(|r| r.split(' ').next())
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("{text}"));
    let want = sds_simulator::audio::tone_hz("FD Main");
    assert!(
        (hz - want).abs() <= 20.0,
        "transcribed tone {hz} Hz, simulator sent {want} Hz"
    );
    let (_, t) = env
        .call(
            Method::GET,
            &format!("/api/v1/transmissions/{tx_id}"),
            &viewer,
            None,
            None,
        )
        .await;
    assert!(t["transcript"].is_null(), "viewers do not see transcripts");
    let (_, found) = env
        .call(
            Method::GET,
            "/api/v1/transmissions?q=hertz%20tone",
            &listener,
            None,
            None,
        )
        .await;
    assert_eq!(found["items"].as_array().map(Vec::len), Some(1));
    let r = play(viewer.clone()).await;
    assert_eq!(
        r.status(),
        StatusCode::FORBIDDEN,
        "playback needs audio_listen"
    );
    let r = play(listener.clone()).await;
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(r.headers()["content-type"], "audio/ogg; codecs=opus");
    let ogg = r.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&ogg[..4], b"OggS");
    let pages = ogg.windows(4).filter(|w| w == b"OggS").count();
    // 1.5 s talk + 1 s pre-buffer + 0.5 s post-roll ≈ 150 frames, give or take event timing.
    assert!((80..=220).contains(&pages), "{pages} Ogg pages");

    // Live listening through WHEP.
    let crypto = Arc::new(str0m::crypto::from_feature_flags());
    let sock = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let local = sock.local_addr().unwrap();
    let mut rtc = RtcConfig::new()
        .set_crypto_provider(crypto)
        .build(std::time::Instant::now());
    rtc.add_local_candidate(Candidate::host(local, "udp").unwrap());
    let mut api = rtc.sdp_api();
    api.add_media(MediaKind::Audio, Direction::RecvOnly, None, None, None);
    let (offer, pending) = api.apply().unwrap();
    let whep = |token: String, sdp: String| {
        let req = Request::post(format!("{base}/audio/whep"))
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/sdp")
            .body(Body::from(sdp))
            .unwrap();
        let app = public_app(Arc::clone(&env.state));
        async move { app.oneshot(req).await.unwrap() }
    };
    assert_eq!(
        whep(viewer.clone(), offer.to_sdp_string()).await.status(),
        StatusCode::FORBIDDEN
    );
    let r = whep(listener.clone(), offer.to_sdp_string()).await;
    assert_eq!(r.status(), StatusCode::CREATED);
    let location = r.headers()["location"].to_str().unwrap().to_owned();
    let answer =
        String::from_utf8(r.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap();
    rtc.sdp_api()
        .accept_answer(pending, SdpAnswer::from_sdp_string(&answer).unwrap())
        .unwrap();
    sim.begin_transmission(Transmission {
        channel: "PD North".into(),
        ..Transmission::default()
    });
    let mut frames = 0;
    let mut buf = vec![0u8; 2000];
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while frames < 25 && std::time::Instant::now() < deadline {
        let timeout = loop {
            match rtc.poll_output().unwrap() {
                Output::Timeout(t) => break t,
                Output::Transmit(t) => {
                    sock.send_to(&t.contents, t.destination).await.unwrap();
                }
                Output::Event(Event::MediaData(m)) => {
                    if !m.data.is_empty() {
                        frames += 1;
                    }
                }
                Output::Event(_) => {}
            }
        };
        let wait = timeout
            .saturating_duration_since(std::time::Instant::now())
            .max(Duration::from_millis(1));
        match tokio::time::timeout(wait, sock.recv_from(&mut buf)).await {
            Ok(Ok((n, source))) => rtc
                .handle_input(Input::Receive(
                    std::time::Instant::now(),
                    Receive {
                        proto: Protocol::Udp,
                        source,
                        destination: local,
                        contents: buf[..n].try_into().unwrap(),
                    },
                ))
                .unwrap(),
            _ => rtc
                .handle_input(Input::Timeout(std::time::Instant::now()))
                .unwrap(),
        }
    }
    sim.end_transmission();
    assert!(frames >= 25, "live listener received {frames} Opus frames");

    // Someone else cannot hang up this session; the listener can.
    let hang = |token: String| {
        let req = Request::delete(location.clone())
            .header("authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();
        let app = public_app(Arc::clone(&env.state));
        async move { app.oneshot(req).await.unwrap().status() }
    };
    let other_listener = env.issuer.token(
        "otto",
        600,
        json!({ "tenant_id": env.tenant.to_string(), "roles": ["listener"], "sites": ["*"] }),
    );
    assert_eq!(hang(other_listener).await, StatusCode::FORBIDDEN);
    assert_eq!(hang(listener.clone()).await, StatusCode::NO_CONTENT);

    // Turning audio off tears the RTSP session down cleanly.
    let (s, _) = env
        .call(
            Method::PATCH,
            &base,
            &admin,
            Some(json!({ "audio_enabled": false })),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    eventually("RTSP torn down", Duration::from_secs(10), || async {
        !sim.audio_stats().active
    })
    .await;
    let st = sim.audio_stats();
    assert!(st.teardowns >= 1, "{st:?}");
    assert_eq!(st.lockups, 0, "the scanner's audio server never locked up");
    let (_, audit) = env
        .call(Method::GET, "/api/v1/audit?limit=200", &admin, None, None)
        .await;
    let actions: Vec<&str> = audit["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|a| a["action"].as_str())
        .collect();
    assert!(
        actions.contains(&"scanner.audio.listen") && actions.contains(&"transmission.audio.play"),
        "{actions:?}"
    );
    agent.shutdown().await;
}
