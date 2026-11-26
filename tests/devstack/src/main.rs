//! `radio-devstack`: a complete, **simulated** platform in one process, for
//! developing and demonstrating the web UI without SDS200 hardware.
//!
//! It starts:
//! - four simulated SDS200s (one deliberately offline);
//! - two site agents, each with its own mTLS client certificate from a
//!   throwaway CA;
//! - the API gateway with the in-memory store, live audio, in-memory
//!   recording, a retention policy, and transcription against a **stub**
//!   service that names the simulator's tone instead of recognising speech;
//! - a test OIDC issuer and a `/dev/token` route that hands out tokens for
//!   the roles `admin`, `operator` and `viewer`.
//!
//! It then enrolls sites, agents and scanners through the real REST API and
//! plays fictional radio traffic on the simulators.
//!
//! **Never deploy this.** `/dev/token` grants any role to anyone who can
//! reach it, so the binary refuses to listen on anything but loopback. All
//! system, talkgroup and unit names are invented.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use api_gateway::config::LimitsConfig;
use api_gateway::ui::{UiAuth, UiConfig};
use api_gateway::{AppState, agent_app, serve, tls, with_ui};
use axum::Router;
use axum::body::Body;
use axum::extract::Query;
use axum::http::{Method, Request, StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::get;
use http_body_util::BodyExt;
use platform_auth::testing::TestIssuer;
use platform_auth::{OidcConfig, OidcVerifier, Permission};
use platform_common::{NetworkPolicy, TenantId};
use platform_storage::MemoryStore;
use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    SanType,
};
use scanner_agent::AgentHandle;
use scanner_agent::config::{AgentConfig, GatewayConfig, TelemetryConfig, TelemetryMode};
use sds_simulator::{Faults, Simulator, SimulatorConfig, Transmission};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;
use tracing::{info, warn};

const ISS: &str = "https://devstack.invalid";
const AUD: &str = "radio-platform";
const TOKEN_TTL_SECS: i64 = 3600;

type Res<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

// ---------- arguments ----------

struct Args {
    bind: SocketAddr,
    ui_dir: Option<PathBuf>,
    traffic: bool,
}

fn parse_args() -> Res<Args> {
    let port = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(8080);
    let mut args = Args {
        bind: SocketAddr::from(([127, 0, 0, 1], port)),
        ui_dir: None,
        traffic: true,
    };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--bind" => {
                args.bind = it
                    .next()
                    .ok_or("--bind needs an address")?
                    .parse()
                    .map_err(err)?;
            }
            "--ui-dir" => args.ui_dir = Some(it.next().ok_or("--ui-dir needs a path")?.into()),
            "--no-traffic" => args.traffic = false,
            "-h" | "--help" => {
                println!(
                    "usage: radio-devstack [--bind 127.0.0.1:$PORT|8080] [--ui-dir frontend/dist] [--no-traffic]\n\
                     Development only. Serves unauthenticated test tokens at /dev/token."
                );
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument `{other}`")),
        }
    }
    if !args.bind.ip().is_loopback() {
        return Err(format!(
            "refusing to bind {}: the devstack hands out tokens to anyone and must stay on loopback",
            args.bind
        ));
    }
    Ok(args)
}

// ---------- throwaway PKI ----------

struct Pki {
    dir: PathBuf,
}

impl Pki {
    fn new(agents: &[&str]) -> Res<Self> {
        let dir =
            std::env::temp_dir().join(format!("radio-devstack-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).map_err(err)?;
        let mut ca = CertificateParams::new(Vec::<String>::new()).map_err(err)?;
        ca.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca.distinguished_name
            .push(DnType::CommonName, "radio-devstack throwaway CA");
        let ca_key = KeyPair::generate().map_err(err)?;
        let ca_pem = ca.self_signed(&ca_key).map_err(err)?.pem();
        let issuer = Issuer::new(ca, ca_key);
        let write = |name: &str, body: &str| std::fs::write(dir.join(name), body).map_err(err);
        write("ca.pem", &ca_pem)?;
        let leaf = |dns: &str, eku: ExtendedKeyUsagePurpose, ip: bool| -> Res<(String, String)> {
            let mut p = CertificateParams::new(vec![dns.to_owned()]).map_err(err)?;
            if ip {
                p.subject_alt_names
                    .push(SanType::IpAddress([127, 0, 0, 1].into()));
            }
            p.distinguished_name.push(DnType::CommonName, dns);
            p.extended_key_usages = vec![eku];
            let k = KeyPair::generate().map_err(err)?;
            Ok((
                p.signed_by(&k, &issuer).map_err(err)?.pem(),
                k.serialize_pem(),
            ))
        };
        let (c, k) = leaf("localhost", ExtendedKeyUsagePurpose::ServerAuth, true)?;
        write("gateway.pem", &c)?;
        write("gateway-key.pem", &k)?;
        for a in agents {
            let (c, k) = leaf(a, ExtendedKeyUsagePurpose::ClientAuth, false)?;
            write(&format!("{a}.pem"), &c)?;
            write(&format!("{a}-key.pem"), &k)?;
        }
        Ok(Self { dir })
    }
    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }
}

impl Drop for Pki {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

// ---------- fictional traffic ----------

/// System, department, channel, frequency (Hz), talkgroup, modulation.
type Channel = (
    &'static str,
    &'static str,
    &'static str,
    u64,
    Option<u32>,
    &'static str,
);

/// Invented systems; none corresponds to a real network.
const CATALOG: &[Channel] = &[
    (
        "Example County P25",
        "Fire Dispatch",
        "FD Main",
        851_012_500,
        Some(1201),
        "P25",
    ),
    (
        "Example County P25",
        "Fire Dispatch",
        "FD Tac 2",
        851_037_500,
        Some(1203),
        "P25",
    ),
    (
        "Example County P25",
        "Law Dispatch",
        "PD North",
        852_212_500,
        Some(2101),
        "P25",
    ),
    (
        "Example County P25",
        "Public Works",
        "Streets",
        853_487_500,
        Some(3405),
        "P25",
    ),
    (
        "Riverside Conventional",
        "Marine",
        "Harbor Ops",
        156_800_000,
        None,
        "FM",
    ),
    (
        "Riverside Conventional",
        "Rail",
        "Yard 1",
        160_890_000,
        None,
        "NFM",
    ),
    (
        "Riverside Conventional",
        "Air",
        "Tower",
        120_100_000,
        None,
        "AM",
    ),
    (
        "Metro DMR",
        "Transit",
        "Bus Ops",
        462_550_000,
        Some(5001),
        "NFM",
    ),
];

/// Small deterministic PRNG (xorshift64*); the traffic is decorative.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next() % n }
    }
}

async fn play_traffic(sims: Vec<Arc<Simulator>>, cancel: CancellationToken) {
    let mut rng = Rng(0x5d5_200_u64 ^ u64::from(std::process::id()));
    loop {
        let Some(sim) = usize::try_from(rng.below(sims.len() as u64))
            .ok()
            .and_then(|i| sims.get(i))
        else {
            return;
        };
        let pick = usize::try_from(rng.below(CATALOG.len() as u64)).unwrap_or(0);
        if let Some(&(system, department, channel, hz, tg, modulation)) = CATALOG.get(pick) {
            let bars = u8::try_from(1 + rng.below(4)).unwrap_or(1);
            sim.begin_transmission(Transmission {
                frequency_hz: hz,
                tgid: tg,
                unit_id: tg.map(|_| u32::try_from(100_000 + rng.below(900_000)).unwrap_or(0)),
                system: system.into(),
                department: department.into(),
                channel: channel.into(),
                modulation: modulation.into(),
                signal_bars: bars,
                rssi: -110.0 + f32::from(bars) * 12.0,
            });
            let talk = Duration::from_millis(1500 + rng.below(4500));
            tokio::select! {
                () = cancel.cancelled() => return,
                () = tokio::time::sleep(talk) => {}
            }
            sim.end_transmission();
        }
        let gap = Duration::from_millis(1000 + rng.below(5000));
        tokio::select! {
            () = cancel.cancelled() => return,
            () = tokio::time::sleep(gap) => {}
        }
    }
}

// ---------- dev token route ----------

#[derive(Debug, Deserialize)]
struct TokenQuery {
    role: Option<String>,
}

fn dev_routes(issuer: Arc<TestIssuer>, tenant: TenantId, ui: bool) -> Router {
    let token = get(move |Query(q): Query<TokenQuery>| {
        let issuer = Arc::clone(&issuer);
        async move {
            let role = q.role.unwrap_or_else(|| "admin".into());
            if !matches!(role.as_str(), "admin" | "operator" | "viewer") {
                return (
                    StatusCode::BAD_REQUEST,
                    "role must be admin, operator or viewer",
                )
                    .into_response();
            }
            let sub = format!("dev-{role}");
            let access_token = issuer.token(
                &sub,
                TOKEN_TTL_SECS,
                json!({
                    "tenant_id": tenant.to_string(),
                    "roles": [role],
                    "sites": ["*"],
                    "name": format!("Dev {role}"),
                }),
            );
            (
                [(header::CACHE_CONTROL, "no-store")],
                axum::Json(json!({ "access_token": access_token, "expires_in": TOKEN_TTL_SECS })),
            )
                .into_response()
        }
    });
    let r = Router::new().route("/dev/token", token);
    if ui {
        r
    } else {
        // Without --ui-dir the Vite dev server proxies here for its config.
        r.route(
            "/ui-config.json",
            get(|| async { axum::Json(json!({ "auth": { "mode": "dev" } })) }),
        )
    }
}

// ---------- seeding through the real API ----------

async fn call(app: &Router, token: &str, method: Method, uri: &str, body: Value) -> Res<Value> {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .map_err(err)?;
    let resp = app.clone().oneshot(req).await.map_err(err)?;
    let status = resp.status();
    let bytes = resp.into_body().collect().await.map_err(err)?.to_bytes();
    let v: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    if status.is_success() {
        Ok(v)
    } else {
        Err(format!("{uri}: {status} {v}"))
    }
}

fn str_field(v: &Value, k: &str) -> Res<String> {
    v.get(k)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("response missing `{k}`: {v}"))
}

struct SiteSpec {
    name: &'static str,
    description: &'static str,
    lat: f64,
    lon: f64,
    agent: &'static str,
    scanners: &'static [&'static str],
}

const SITES: &[SiteSpec] = &[
    SiteSpec {
        name: "North Ridge",
        description: "Hilltop shelter, simulated",
        lat: 44.97,
        lon: -93.26,
        agent: "agent-north-ridge",
        scanners: &["NR Primary", "NR Secondary"],
    },
    SiteSpec {
        name: "Harbor District",
        description: "Port office rack, simulated (one unit offline)",
        lat: 44.95,
        lon: -93.09,
        agent: "agent-harbor",
        scanners: &["Harbor Main", "Harbor Spare (offline)"],
    },
];

fn agent_config(
    pki: &Pki,
    agent: &str,
    gw: SocketAddr,
    ports: Vec<u16>,
    audio_ports: Vec<u16>,
) -> AgentConfig {
    AgentConfig {
        gateway: GatewayConfig {
            url: format!("wss://127.0.0.1:{}/agent/v1", gw.port()),
            ca: pki.path("ca.pem"),
            cert: pki.path(&format!("{agent}.pem")),
            key: pki.path(&format!("{agent}-key.pem")),
        },
        network: NetworkPolicy {
            allowed_networks: vec![ipnet_loopback()],
            allowed_ports: ports,
            audio_ports,
        },
        telemetry: TelemetryConfig {
            mode: TelemetryMode::Push,
            interval_ms: 400,
            hang_time_ms: 1200,
        },
        max_buffered_events: 10_000,
        state_dir: None,
        max_scanners: 8,
        scanners: vec![],
        audio: scanner_agent::audio::AudioSettings {
            cooldown_secs: 3,
            backoff_max_secs: 30,
            linger_secs: 5,
        },
    }
}

fn ipnet_loopback() -> ipnet::IpNet {
    ipnet::IpNet::from(std::net::IpAddr::from([127, 0, 0, 1]))
}

async fn run(args: Args) -> Res<()> {
    let agent_names: Vec<&str> = SITES.iter().map(|s| s.agent).collect();
    let pki = Pki::new(&agent_names)?;

    // Simulators, one per scanner.
    let mut sims: Vec<Vec<Arc<Simulator>>> = Vec::new();
    for site in SITES {
        let mut v = Vec::new();
        for _ in site.scanners {
            v.push(Arc::new(
                Simulator::start(SimulatorConfig {
                    audio: Some(SocketAddr::from(([127, 0, 0, 1], 0))),
                    ..SimulatorConfig::default()
                })
                .await
                .map_err(err)?,
            ));
        }
        sims.push(v);
    }
    // One extra simulator at the first site that is never enrolled, so
    // discovery has something to find.
    let stray = Simulator::start(SimulatorConfig::default())
        .await
        .map_err(err)?;
    let mut all_ports: Vec<u16> = sims.iter().flatten().map(|s| s.addr().port()).collect();
    all_ports.push(stray.addr().port());

    // Gateway.
    let issuer = Arc::new(TestIssuer::new(ISS, AUD));
    let mut oidc = OidcConfig::new(ISS, AUD);
    oidc.role_permissions = BTreeMap::from([
        ("admin".into(), Permission::ALL.to_vec()),
        (
            "operator".into(),
            vec![
                Permission::ScannerView,
                Permission::ScannerControl,
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
        Arc::new(OidcVerifier::with_static_jwks(oidc, issuer.jwks())),
        LimitsConfig {
            scanner_ports: all_ports.clone(),
            audio_ports: sims
                .iter()
                .flatten()
                .filter_map(|s| s.rtsp_addr())
                .map(|a| a.port())
                .collect(),
            command_timeout_secs: 5,
            ..LimitsConfig::default()
        },
    );
    let ui = match &args.ui_dir {
        Some(dir) => Some(api_gateway::ui::router(&UiConfig {
            dir: dir.clone(),
            auth: UiAuth::Dev,
        })?),
        None => None,
    };
    // Live audio on loopback and in-memory recording (development only).
    let webrtc = media_gateway::WebRtcServer::start(
        media_gateway::WebRtcConfig::new(SocketAddr::from(([127, 0, 0, 1], 0))),
        state.media.hub.clone(),
    )
    .await
    .map_err(err)?;
    state.media.enable_webrtc(webrtc, 4);
    let recordings =
        scanner_recorder::RecordingStore::open(&scanner_recorder::StorageConfig::Memory)
            .map_err(err)?;
    state.media.enable_recording(
        recordings.clone(),
        scanner_recorder::SegmentPolicy::default(),
        Arc::clone(&state.store),
    );
    let cancel = CancellationToken::new();
    // Transcription against the stub service: it does not recognise speech,
    // it names the simulator's tone (development only).
    let stub = sds_simulator::whisper::start(SocketAddr::from(([127, 0, 0, 1], 0)), cancel.clone())
        .await
        .map_err(err)?;
    let transcription = Arc::new(
        api_gateway::transcription::Transcription::new(
            scanner_transcription::TranscriptionConfig {
                endpoint: stub.url(),
                api: scanner_transcription::ApiStyle::Openai,
                model: "stub".into(),
                language: None,
                prompt: None,
                api_key_env: None,
                ca_file: None,
                allow_http: false,
                timeout_secs: 30,
                concurrency: 2,
                max_attempts: 3,
                min_duration_ms: 600,
            },
        )
        .map_err(err)?,
    );
    state.media.enable_transcription(Arc::clone(&transcription));
    tokio::spawn(transcription.run(
        Arc::clone(&state.store),
        recordings.clone(),
        Arc::clone(&state.media.transcription_wake),
        cancel.clone(),
    ));
    // A retention policy so the console shows one (nothing is old enough
    // to be swept in a dev session).
    let retention = Arc::new(api_gateway::retention::Retention::new(
        api_gateway::config::RetentionConfig {
            recordings_days: Some(30),
            transcripts_days: Some(90),
            transmissions_days: Some(365),
            events_days: Some(90),
            sweep_interval_secs: 3600,
        },
    ));
    let _ = state.retention.set(Arc::clone(&retention));
    tokio::spawn(retention.run(Arc::clone(&state.store), Some(recordings), cancel.clone()));
    let tenant = TenantId::new();
    let public = dev_routes(Arc::clone(&issuer), tenant, ui.is_some())
        .merge(with_ui(Arc::clone(&state), ui));

    let mtls = tls::mtls_server_config(
        &pki.path("gateway.pem"),
        &pki.path("gateway-key.pem"),
        &pki.path("ca.pem"),
    )
    .map_err(err)?;
    let agent_listener = TcpListener::bind("127.0.0.1:0").await.map_err(err)?;
    let agent_addr = agent_listener.local_addr().map_err(err)?;
    tokio::spawn(serve::serve(
        agent_listener,
        Some(mtls),
        agent_app(Arc::clone(&state)),
        64,
        cancel.clone(),
    ));
    let public_listener = TcpListener::bind(args.bind).await.map_err(err)?;
    tokio::spawn(serve::serve(
        public_listener,
        None,
        public.clone(),
        256,
        cancel.clone(),
    ));

    // Enroll through the API, exactly as an administrator would.
    let admin = issuer.token(
        "devstack-seeder",
        600,
        json!({ "tenant_id": tenant.to_string(), "roles": ["admin"], "sites": ["*"] }),
    );
    let mut agents: Vec<AgentHandle> = Vec::new();
    for (site, site_sims) in SITES.iter().zip(&sims) {
        let s = call(
            &public,
            &admin,
            Method::POST,
            "/api/v1/sites",
            json!({
                "name": site.name, "description": site.description,
                "latitude": site.lat, "longitude": site.lon,
                "allowed_networks": ["127.0.0.1/32"],
            }),
        )
        .await?;
        let site_id = str_field(&s, "id")?;
        call(
            &public,
            &admin,
            Method::POST,
            "/api/v1/agents",
            json!({ "agent_id": site.agent, "site_id": site_id, "description": format!("{} agent (simulated)", site.name) }),
        )
        .await?;
        for (i, (name, sim)) in site.scanners.iter().zip(site_sims).enumerate() {
            let sc = call(
                &public,
                &admin,
                Method::POST,
                "/api/v1/scanners",
                json!({
                    "site_id": site_id, "agent_id": site.agent, "name": name,
                    "host": "127.0.0.1", "port": sim.addr().port(),
                }),
            )
            .await?;
            if let Some(rtsp) = sim.rtsp_addr() {
                // Audio everywhere; recording and (stub) transcription only
                // on the first scanner of the first site, to show both states.
                let record = agents.is_empty() && i == 0;
                call(
                    &public,
                    &admin,
                    Method::PATCH,
                    &format!("/api/v1/scanners/{}", str_field(&sc, "id")?),
                    json!({ "audio_enabled": true, "rtsp_port": rtsp.port(), "recording_enabled": record, "transcription_enabled": record }),
                )
                .await?;
            }
        }
        let audio_ports: Vec<u16> = site_sims
            .iter()
            .filter_map(|s| s.rtsp_addr())
            .map(|a| a.port())
            .collect();
        let mut ports: Vec<u16> = site_sims.iter().map(|s| s.addr().port()).collect();
        if agents.is_empty() {
            ports.push(stray.addr().port());
        }
        agents.push(
            scanner_agent::start(agent_config(
                &pki,
                site.agent,
                agent_addr,
                ports,
                audio_ports,
            ))
            .map_err(err)?,
        );
    }

    // The last scanner at the last site never answers.
    if let Some(dead) = sims.last().and_then(|v| v.last()) {
        dead.set_faults(Faults {
            offline: true,
            ..Faults::default()
        });
    }
    let live: Vec<Arc<Simulator>> = {
        let mut v: Vec<Arc<Simulator>> = sims.iter().flatten().cloned().collect();
        v.pop();
        v
    };
    if args.traffic {
        tokio::spawn(play_traffic(live, cancel.clone()));
    }

    let url = format!("http://{}", args.bind);
    info!(%url, "radio-devstack ready (simulated hardware only)");
    println!("\nradio-devstack ready: {url}");
    match &args.ui_dir {
        Some(d) => println!("  UI:     {url}/ (from {})", d.display()),
        None => {
            println!("  UI:     run `npm run dev` in frontend/ and open http://localhost:5173/")
        }
    }
    println!("  Tokens: {url}/dev/token?role=admin|operator|viewer  (development only)");
    println!(
        "  Discovery: an unenrolled simulator answers at 127.0.0.1:{} (North Ridge; use that port and 127.0.0.1/32)",
        stray.addr().port()
    );
    println!("  All scanners are simulators; all radio traffic is invented.\n");

    let _ = tokio::signal::ctrl_c().await;
    info!("shutting down");
    for a in agents {
        a.shutdown().await;
    }
    cancel.cancel();
    Ok(())
}

fn ui_dir_hint(p: &Path) -> String {
    format!(
        "{} (build it with `npm run build` in frontend/)",
        p.display()
    )
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,api_gateway=info,scanner_agent=info".into()),
        )
        .init();
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {e}");
            return std::process::ExitCode::from(2);
        }
    };
    if let Some(d) = &args.ui_dir
        && !d.join("index.html").is_file()
    {
        eprintln!("error: no UI at {}", ui_dir_hint(d));
        return std::process::ExitCode::from(2);
    }
    warn!("DEVELOPMENT ONLY: /dev/token issues tokens without authentication");
    match run(args).await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}
