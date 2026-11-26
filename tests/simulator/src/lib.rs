//! Software SDS200 network simulator.
//!
//! Implements the scanner side of the UDP command protocol for development
//! and automated testing. **Simulated behaviour is not hardware-verified.**
//! Reply formats follow the BCDx36HP Remote Command Specification v1.03 and,
//! where noted, community reports; see `docs/protocol-compliance.md`.
//!
//! [`audio`] simulates the RTSP/RTP audio server (from community reports)
//! and [`whisper`] is a stand-in transcription service for tests.

pub mod audio;
pub mod whisper;

use std::collections::{HashSet, VecDeque};
use std::fmt::Write as _;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use tokio::net::UdpSocket;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

/// Fault injection settings. Probabilities are in `0.0..=1.0`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Faults {
    /// Probability that an incoming command is silently dropped.
    pub drop_request: f64,
    /// Probability that a reply datagram is dropped.
    pub drop_reply: f64,
    /// Fixed delay added before each reply.
    pub latency: Duration,
    /// Probability that a reply datagram is sent twice.
    pub duplicate: f64,
    /// Probability that a reply is replaced by malformed bytes.
    pub malformed: f64,
    /// Send `CMD,<XML>,` as its own datagram before XML documents.
    pub split_xml_header: bool,
    /// Simulate an interruption: ignore all traffic while true.
    pub offline: bool,
}

/// A simulated active transmission.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Transmission {
    /// Frequency in Hz.
    pub frequency_hz: u64,
    /// Talkgroup ID (trunked) or `None` (conventional).
    pub tgid: Option<u32>,
    /// Radio unit ID.
    pub unit_id: Option<u32>,
    /// System name.
    pub system: String,
    /// Department name.
    pub department: String,
    /// Channel name.
    pub channel: String,
    /// Modulation (`NFM`, `FM`, `AM`, ...).
    pub modulation: String,
    /// Signal bars 0–4.
    pub signal_bars: u8,
    /// RSSI value.
    pub rssi: f32,
}

/// Mutable simulated scanner state.
#[derive(Debug, Clone)]
pub struct State {
    /// Model string returned by `MDL`.
    pub model: String,
    /// Firmware string returned by `VER`.
    pub firmware: String,
    /// Volume 0–29.
    pub volume: u8,
    /// Squelch 0–19.
    pub squelch: u8,
    /// Hold active.
    pub hold: bool,
    /// Current mode string for `ScannerInfo@Mode`.
    pub mode: String,
    /// Active transmission, if any.
    pub transmission: Option<Transmission>,
    /// Last quick-search-hold frequency in Hz.
    pub quick_search_hz: Option<u64>,
    /// User recording active.
    pub recording: bool,
    /// Number of systems served by `GLT,SYS`.
    pub system_count: u32,
    /// Powered off by `POF`.
    pub powered_off: bool,
    /// PSI push interval.
    pub push_interval: Option<Duration>,
    /// Commands that reply `ERR` (simulating unsupported firmware).
    pub unsupported: HashSet<String>,
    /// Raw commands received, newest last (bounded).
    pub received: VecDeque<String>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            model: "SDS200".into(),
            firmware: "Version 1.26.01".into(),
            volume: 10,
            squelch: 3,
            hold: false,
            mode: "Scan Mode".into(),
            transmission: None,
            quick_search_hz: None,
            recording: false,
            system_count: 5,
            powered_off: false,
            push_interval: None,
            unsupported: HashSet::new(),
            received: VecDeque::new(),
        }
    }
}

/// Simulator configuration.
#[derive(Debug, Clone)]
pub struct SimulatorConfig {
    /// Address to bind (use port 0 for an ephemeral port).
    pub bind: SocketAddr,
    /// Initial state.
    pub state: State,
    /// Initial faults.
    pub faults: Faults,
    /// Maximum XML payload bytes per datagram for multi-part `GLT` replies.
    pub max_xml_datagram: usize,
    /// PRNG seed for reproducible fault injection.
    pub seed: u64,
    /// Also run the simulated RTSP audio server on this address.
    pub audio: Option<SocketAddr>,
}

impl Default for SimulatorConfig {
    fn default() -> Self {
        Self {
            bind: SocketAddr::from(([127, 0, 0, 1], 0)),
            state: State::default(),
            faults: Faults::default(),
            max_xml_datagram: 1200,
            seed: 0x5d5_200,
            audio: None,
        }
    }
}

const MAX_RECEIVED_LOG: usize = 1024;

#[derive(Debug)]
struct Shared {
    state: Mutex<State>,
    faults: Mutex<Faults>,
    rng: Mutex<Rng>,
    client: Mutex<Option<SocketAddr>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A running simulator.
#[derive(Debug)]
pub struct Simulator {
    addr: SocketAddr,
    rtsp: Option<SocketAddr>,
    audio: Arc<audio::AudioShared>,
    shared: Arc<Shared>,
    cancel: CancellationToken,
    task: Option<JoinHandle<()>>,
}

impl Simulator {
    /// Binds and starts the simulator.
    ///
    /// # Errors
    /// Returns an error if the socket cannot be bound.
    pub async fn start(config: SimulatorConfig) -> std::io::Result<Self> {
        let socket = Arc::new(UdpSocket::bind(config.bind).await?);
        let addr = socket.local_addr()?;
        let shared = Arc::new(Shared {
            state: Mutex::new(config.state),
            faults: Mutex::new(config.faults),
            rng: Mutex::new(Rng::new(config.seed)),
            client: Mutex::new(None),
        });
        let cancel = CancellationToken::new();
        let audio = Arc::new(audio::AudioShared::default());
        let rtsp = match config.audio {
            Some(bind) => {
                let l = tokio::net::TcpListener::bind(bind).await?;
                let a = l.local_addr()?;
                tokio::spawn(audio::serve(
                    l,
                    Arc::clone(&shared),
                    Arc::clone(&audio),
                    cancel.clone(),
                ));
                Some(a)
            }
            None => None,
        };
        let task = tokio::spawn(serve(
            Arc::clone(&socket),
            Arc::clone(&shared),
            cancel.clone(),
            config.max_xml_datagram.max(256),
        ));
        Ok(Self {
            addr,
            rtsp,
            audio,
            shared,
            cancel,
            task: Some(task),
        })
    }

    /// The bound address.
    #[must_use]
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// The simulated RTSP audio server's address, if enabled.
    #[must_use]
    pub fn rtsp_addr(&self) -> Option<SocketAddr> {
        self.rtsp
    }

    /// Audio server statistics.
    #[must_use]
    pub fn audio_stats(&self) -> audio::AudioStats {
        lock(&self.audio.stats).clone()
    }

    /// Sets RTP impairments.
    pub fn set_audio_faults(&self, f: audio::AudioFaults) {
        *lock(&self.audio.faults) = f;
    }

    /// Clears an audio lock-up, as power-cycling the scanner would.
    pub fn power_cycle_audio(&self) {
        let mut s = lock(&self.audio.stats);
        s.locked = false;
        s.active = false;
    }

    /// Replaces the fault settings.
    pub fn set_faults(&self, faults: Faults) {
        *lock(&self.shared.faults) = faults;
    }

    /// Mutates simulated state.
    pub fn with_state<R>(&self, f: impl FnOnce(&mut State) -> R) -> R {
        f(&mut lock(&self.shared.state))
    }

    /// Snapshot of simulated state.
    #[must_use]
    pub fn state(&self) -> State {
        lock(&self.shared.state).clone()
    }

    /// Commands received so far.
    #[must_use]
    pub fn received(&self) -> Vec<String> {
        lock(&self.shared.state).received.iter().cloned().collect()
    }

    /// Starts a simulated transmission.
    pub fn begin_transmission(&self, t: Transmission) {
        self.with_state(|s| s.transmission = Some(t));
    }

    /// Ends the simulated transmission.
    pub fn end_transmission(&self) {
        self.with_state(|s| s.transmission = None);
    }

    /// Stops the simulator.
    pub async fn shutdown(mut self) {
        self.cancel.cancel();
        if let Some(t) = self.task.take() {
            let _ = t.await;
        }
    }
}

impl Drop for Simulator {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

/// Small deterministic PRNG (xorshift64*), sufficient for fault injection.
#[derive(Debug)]
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed | 1)
    }
    fn next_f64(&mut self) -> f64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        let v = x.wrapping_mul(0x2545_f491_4f6c_dd1d);
        // 53 high bits → [0, 1).
        #[allow(clippy::cast_precision_loss)]
        let f = (v >> 11) as f64 / (1u64 << 53) as f64;
        f
    }
}

fn chance(shared: &Shared, p: f64) -> bool {
    p > 0.0 && lock(&shared.rng).next_f64() < p
}

async fn serve(
    socket: Arc<UdpSocket>,
    shared: Arc<Shared>,
    cancel: CancellationToken,
    max_xml_datagram: usize,
) {
    let mut buf = vec![0u8; 65_535];
    let mut push_tick = tokio::time::interval(Duration::from_millis(50));
    let mut last_push = tokio::time::Instant::now();
    loop {
        tokio::select! {
            () = cancel.cancelled() => break,
            _ = push_tick.tick() => {
                let interval = lock(&shared.state).push_interval;
                let client = *lock(&shared.client);
                if let (Some(interval), Some(client)) = (interval, client)
                    && last_push.elapsed() >= interval
                    && !lock(&shared.faults).offline
                {
                    {
                        last_push = tokio::time::Instant::now();
                        let doc = scanner_info_xml(&lock(&shared.state));
                        let replies = xml_reply("PSI", vec![doc], &lock(&shared.faults));
                        send_replies(&socket, &shared, client, replies).await;
                    }
                }
            }
            r = socket.recv_from(&mut buf) => {
                let (n, peer) = match r {
                    Ok(v) => v,
                    Err(e) => { warn!(error = %e, "simulator recv error"); continue; }
                };
                let faults = lock(&shared.faults).clone();
                if faults.offline || chance(&shared, faults.drop_request) {
                    continue;
                }
                *lock(&shared.client) = Some(peer);
                let text = String::from_utf8_lossy(buf.get(..n).unwrap_or_default()).into_owned();
                for line in text.split(['\r', '\n']).filter(|l| !l.is_empty()) {
                    let replies = handle(line, &shared, max_xml_datagram, &faults);
                    let socket = Arc::clone(&socket);
                    let shared = Arc::clone(&shared);
                    if faults.latency.is_zero() {
                        send_replies(&socket, &shared, peer, replies).await;
                    } else {
                        let latency = faults.latency;
                        tokio::spawn(async move {
                            tokio::time::sleep(latency).await;
                            send_replies(&socket, &shared, peer, replies).await;
                        });
                    }
                }
            }
        }
    }
}

async fn send_replies(
    socket: &UdpSocket,
    shared: &Shared,
    peer: SocketAddr,
    replies: Vec<Vec<u8>>,
) {
    let faults = lock(&shared.faults).clone();
    for mut datagram in replies {
        if chance(shared, faults.drop_reply) {
            continue;
        }
        if chance(shared, faults.malformed) {
            datagram = b"\xff\xfe<<garbage,\x00\r".to_vec();
        }
        let copies = if chance(shared, faults.duplicate) {
            2
        } else {
            1
        };
        for _ in 0..copies {
            if let Err(e) = socket.send_to(&datagram, peer).await {
                debug!(error = %e, "simulator send error");
            }
        }
    }
}

fn line(s: impl Into<String>) -> Vec<Vec<u8>> {
    let mut s = s.into();
    s.push('\r');
    vec![s.into_bytes()]
}

fn xml_reply(cmd: &str, documents: Vec<String>, faults: &Faults) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    for doc in documents {
        if faults.split_xml_header {
            out.push(format!("{cmd},<XML>,\r\n").into_bytes());
            out.push(doc.into_bytes());
        } else {
            out.push(format!("{cmd},<XML>,\r{doc}").into_bytes());
        }
    }
    out
}

fn handle(raw: &str, shared: &Shared, max_xml: usize, faults: &Faults) -> Vec<Vec<u8>> {
    let mut st = lock(&shared.state);
    if st.received.len() >= MAX_RECEIVED_LOG {
        st.received.pop_front();
    }
    st.received.push_back(raw.to_owned());
    if st.powered_off {
        return Vec::new();
    }
    let parts: Vec<&str> = raw.split(',').collect();
    let cmd = parts.first().copied().unwrap_or_default();
    let args = parts.get(1..).unwrap_or_default();
    if st.unsupported.contains(cmd) {
        return line("ERR");
    }
    match (cmd, args) {
        ("MDL", []) => line(format!("MDL,{}", st.model)),
        ("VER", []) => line(format!("VER,{}", st.firmware)),
        ("GSI", []) => xml_reply("GSI", vec![scanner_info_xml(&st)], faults),
        ("PSI", []) => line("PSI,OK"),
        ("PSI", [ms]) => match ms.parse::<u64>() {
            Ok(0) => {
                st.push_interval = None;
                line("PSI,OK")
            }
            Ok(ms) => {
                st.push_interval = Some(Duration::from_millis(ms.max(100)));
                line("PSI,OK")
            }
            Err(_) => line("ERR"),
        },
        ("STS", []) => {
            let (l1, l2) = match &st.transmission {
                Some(t) => (t.system.clone(), t.channel.clone()),
                None => ("Scanning...".to_owned(), String::new()),
            };
            line(format!("STS,11,{l1}, ,{l2}, ,0,0,0,0,0,0,0,0,0"))
        }
        ("VOL", []) => line(format!("VOL,{}", st.volume)),
        ("VOL", [v]) => match v.parse::<u8>() {
            Ok(v) if v <= 29 => {
                st.volume = v;
                line("VOL,OK")
            }
            _ => line("ERR"),
        },
        ("SQL", []) => line(format!("SQL,{}", st.squelch)),
        ("SQL", [v]) => match v.parse::<u8>() {
            Ok(v) if v <= 19 => {
                st.squelch = v;
                line("SQL,OK")
            }
            _ => line("ERR"),
        },
        ("KEY", [code, mode]) if code.len() == 1 && mode.len() == 1 => line("KEY,OK"),
        ("HLD", [kw, idx, _]) if ["SYS", "DEPT", "SITE", "CFREQ", "TGID"].contains(kw) => {
            if idx.parse::<u32>().is_ok() {
                st.hold = true;
                st.mode = "Scan Hold".into();
                line("HLD,OK")
            } else {
                line("ERR")
            }
        }
        ("AVD", [_, _, _, status]) if ["1", "2", "3"].contains(status) => line("AVD,OK"),
        ("JPM", [mode, _]) if mode.ends_with("_MODE") => {
            if *mode == "SCN_MODE" {
                st.hold = false;
                st.mode = "Scan Mode".into();
                st.quick_search_hz = None;
            }
            line("JPM,OK")
        }
        ("QSH", [f]) => match f.parse::<u64>() {
            Ok(units) if units > 0 => {
                st.quick_search_hz = Some(units * 100);
                st.hold = true;
                st.mode = "Quick Search Hold".into();
                line("QSH,OK")
            }
            _ => line("ERR"),
        },
        ("JNT", [_, _, _]) => line("JNT,OK"),
        ("FQK", []) => {
            let mut s = String::from("FQK");
            for i in 0..100 {
                s.push_str(if i < 3 { ",2" } else { ",0" });
            }
            line(s)
        }
        ("DTM", []) => line("DTM,0,2026,11,24,12,00,00,1"),
        ("LCR", []) => line("LCR,30.2241,-93.2174,5.0"),
        ("URC", []) => line(format!("URC,{}", u8::from(st.recording))),
        ("URC", [v]) if *v == "0" || *v == "1" => {
            st.recording = *v == "1";
            line("URC,OK")
        }
        ("GLT", ["FL"]) => xml_reply(
            "GLT",
            glt_parts(
                (0..3).map(|i| {
                    format!(
                        "<FL Index=\"{i}\" Name=\"Favorites List {}\" Monitor=\"On\" Q_Key=\"{i}\" N_Tag=\"None\" />",
                        i + 1
                    )
                }),
                max_xml,
            ),
            faults,
        ),
        ("GLT", ["SYS", fl]) if fl.parse::<u32>().is_ok() => xml_reply(
            "GLT",
            glt_parts(
                (0..st.system_count).map(|i| {
                    format!(
                        "<SYS Index=\"{}\" Name=\"Simulated System {i}\" Avoid=\"Off\" Type=\"Conventional\" Q_Key=\"None\" N_Tag=\"None\" />",
                        100 + i
                    )
                }),
                max_xml,
            ),
            faults,
        ),
        ("KAL", []) => Vec::new(),
        ("POF", []) => {
            st.powered_off = true;
            line("POF,OK")
        }
        _ => line("ERR"),
    }
}

/// Splits list entries into self-contained documents with
/// `<Footer No="n" EOT="0|1"/>`, as community-reported for SDS200 firmware.
fn glt_parts(entries: impl Iterator<Item = String>, max_xml: usize) -> Vec<String> {
    const HEAD: &str = "<?xml version=\"1.0\" encoding=\"utf-8\"?>\r<GLT>\r";
    let mut bodies: Vec<String> = vec![String::new()];
    for e in entries {
        let current_len = bodies.last().map_or(0, String::len);
        if current_len > 0 && HEAD.len() + current_len + e.len() + 64 > max_xml {
            bodies.push(String::new());
        }
        if let Some(b) = bodies.last_mut() {
            b.push_str(&e);
            b.push('\r');
        }
    }
    let total = bodies.len();
    bodies
        .into_iter()
        .enumerate()
        .map(|(i, body)| {
            format!(
                "{HEAD}{body}<Footer No=\"{}\" EOT=\"{}\"/>\r</GLT>\r",
                i + 1,
                u8::from(i + 1 == total)
            )
        })
        .collect()
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Builds a `ScannerInfo` document from state. The structure is modelled on
/// the BCDx36HP v1.03 specification example, not on a hardware capture.
#[must_use]
pub fn scanner_info_xml(st: &State) -> String {
    let mut x = String::from("<?xml version=\"1.0\" encoding=\"utf-8\"?>\r");
    let hold = if st.hold { "On" } else { "Off" };
    let (v_screen, mode) = match (&st.transmission, st.quick_search_hz) {
        (_, Some(_)) => ("quick_search", st.mode.clone()),
        (Some(t), _) if t.tgid.is_some() => ("trunk_scan", st.mode.clone()),
        _ => ("conventional_scan", st.mode.clone()),
    };
    let _ = write!(
        x,
        "<ScannerInfo Mode=\"{}\" V_Screen=\"{v_screen}\">\r",
        esc(&mode)
    );
    x.push_str("<MonitorList Name=\"Full Database\" Index=\"4294967295\" ListType=\"FullDb\" Q_Key=\"None\" N_Tag=\"None\" DB_Counter=\"1\" />\r");
    let (sig, rssi) = match &st.transmission {
        Some(t) => (t.signal_bars, t.rssi),
        None => (0, -999.0),
    };
    if let Some(hz) = st.quick_search_hz {
        let _ = write!(
            x,
            "<SrchFrequency Freq=\" {}.{:04}MHz\" Mod=\"Auto\" Hold=\"On\" />\r",
            hz / 1_000_000,
            (hz % 1_000_000) / 100
        );
    } else if let Some(t) = &st.transmission {
        let _ = write!(
            x,
            "<System Name=\"{}\" Index=\"100\" Avoid=\"Off\" SystemType=\"{}\" Q_Key=\"None\" N_Tag=\"None\" Hold=\"{hold}\" />\r",
            esc(&t.system),
            if t.tgid.is_some() {
                "P25 Trunk"
            } else {
                "Conventional"
            }
        );
        let _ = write!(
            x,
            "<Department Name=\"{}\" Index=\"200\" Avoid=\"Off\" Q_Key=\"None\" Hold=\"Off\" />\r",
            esc(&t.department)
        );
        let freq = format!(
            " {}.{:06}MHz",
            t.frequency_hz / 1_000_000,
            t.frequency_hz % 1_000_000
        );
        match t.tgid {
            Some(tgid) => {
                let _ = write!(
                    x,
                    "<TGID Name=\"{}\" Index=\"300\" Avoid=\"Off\" TGID=\"TGID: {tgid}\" N_Tag=\"None\" Hold=\"{hold}\" SvcType=\"Law Dispatch\" P_Ch=\"Off\" LVL=\"0\" />\r",
                    esc(&t.channel)
                );
                let _ = write!(x, "<SiteFrequency Freq=\"{freq}\" IFX=\"Off\" />\r");
                let uid = t
                    .unit_id
                    .map_or_else(|| "UID None".to_owned(), |u| format!("UID:{u}"));
                let _ = write!(x, "<UnitID U_Id=\"{uid}\" />\r");
            }
            None => {
                let _ = write!(
                    x,
                    "<ConvFrequency Name=\"{}\" Index=\"300\" Avoid=\"Off\" Freq=\"{freq}\" Mod=\"{}\" N_Tag=\"None\" Hold=\"{hold}\" SvcType=\"Fire Dispatch\" P_Ch=\"Off\" SAS=\"All\" SAD=\"None\" LVL=\"0\" IFX=\"Off\" />\r",
                    esc(&t.channel),
                    esc(&t.modulation)
                );
            }
        }
    }
    let _ = write!(
        x,
        "<Property VOL=\"{}\" SQL=\"{}\" Sig=\"{sig}\" WiFi=\"Off\" Att=\"Off\" Rec=\"{}\" KeyLock=\"Off\" P25Status=\"None\" Mute=\"{}\" Backlight=\"100\" Rssi=\"{rssi}\" />\r",
        st.volume,
        st.squelch,
        if st.recording { "On" } else { "Off" },
        if st.transmission.is_some() {
            "Unmute"
        } else {
            "Mute"
        }
    );
    x.push_str("<ViewDescription>\r<InfoArea1 Text=\"F0:01234-6*789\" />\r<InfoArea2 Text=\"S3:01234-6*---\" />\r</ViewDescription>\r</ScannerInfo>\r");
    x
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glt_parts_are_numbered_and_bounded() {
        let parts = glt_parts(
            (0..50).map(|i| format!("<SYS Index=\"{i}\" Name=\"System {i}\"/>")),
            400,
        );
        assert!(parts.len() > 1);
        for (i, p) in parts.iter().enumerate() {
            assert!(p.contains(&format!("No=\"{}\"", i + 1)));
            assert!(p.len() <= 400 + 64, "{}", p.len());
        }
        assert!(parts.last().is_some_and(|p| p.contains("EOT=\"1\"")));
    }

    #[test]
    fn rng_is_deterministic_and_in_range() {
        let mut a = Rng::new(1);
        let mut b = Rng::new(1);
        for _ in 0..100 {
            let x = a.next_f64();
            assert!((0.0..1.0).contains(&x));
            assert!((x - b.next_f64()).abs() < f64::EPSILON);
        }
    }
}
