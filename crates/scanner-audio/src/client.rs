//! Async RTSP/RTP audio session against one scanner.
//!
//! Safety rules from `docs/protocol-compliance.md` (community-reported,
//! **not hardware-verified**):
//! - one session per scanner: the caller must own the only [`run_session`]
//!   for an endpoint (the agent's audio manager guarantees this);
//! - `TEARDOWN` is always sent once a session exists, on every exit path;
//! - no connection is ever opened just to probe port 554;
//! - sessions are spaced by a cooldown, with backoff after failures
//!   ([`Supervisor`]).

use std::net::SocketAddr;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::g711;
use crate::jitter::{JitterBuffer, Played, RtpStats};
use crate::opus_enc::{Encoder, FRAME_SAMPLES, OpusFrame};
use crate::rtp::RtpPacket;
use crate::rtsp::{Request, Response, RtspError, RtspSession, parse_response};

/// Session settings.
#[derive(Debug, Clone)]
pub struct SessionConfig {
    /// Scanner RTSP endpoint (port 554 on real hardware).
    pub rtsp: SocketAddr,
    /// Stream path, `/au:scanner.au` on the SDS200.
    pub path: String,
    /// Local UDP port for RTP (0 = any free port).
    pub rtp_port: u16,
    /// TCP connect and response timeout.
    pub timeout: Duration,
    /// Jitter buffer depth in packets.
    pub jitter_depth: usize,
    /// End the session if no RTP arrives for this long.
    pub idle_timeout: Duration,
}

impl SessionConfig {
    /// Defaults for an SDS200 at `rtsp`.
    #[must_use]
    pub fn new(rtsp: SocketAddr) -> Self {
        Self {
            rtsp,
            path: "/au:scanner.au".into(),
            rtp_port: 0,
            timeout: Duration::from_secs(5),
            jitter_depth: 3,
            idle_timeout: Duration::from_secs(10),
        }
    }

    fn url(&self) -> String {
        let host = match self.rtsp {
            SocketAddr::V4(a) => a.ip().to_string(),
            SocketAddr::V6(a) => format!("[{}]", a.ip()),
        };
        if self.rtsp.port() == 554 {
            format!("rtsp://{host}{}", self.path)
        } else {
            format!("rtsp://{host}:{}{}", self.rtsp.port(), self.path)
        }
    }
}

/// An Opus frame with its capture time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedFrame {
    /// Wall-clock time the frame completed, Unix milliseconds.
    pub captured_ms: i64,
    /// Frame.
    pub frame: OpusFrame,
}

/// How a session ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionEnd {
    /// Stopped on request.
    Stopped,
    /// Failed (and was torn down if it had been set up).
    Failed(String),
}

/// Spacing between sessions: a fixed cooldown after every session (the
/// audio server is reported to hang on quick reconnects) and exponential
/// backoff after consecutive failures.
#[derive(Debug, Clone)]
pub struct Supervisor {
    cooldown: Duration,
    backoff_max: Duration,
    last_end: Option<Instant>,
    failures: u32,
}

impl Supervisor {
    /// Creates a supervisor.
    #[must_use]
    pub fn new(cooldown: Duration, backoff_max: Duration) -> Self {
        Self {
            cooldown,
            backoff_max,
            last_end: None,
            failures: 0,
        }
    }

    /// How long to wait before the next session may start.
    #[must_use]
    pub fn wait(&self, now: Instant) -> Duration {
        let Some(end) = self.last_end else {
            return Duration::ZERO;
        };
        let backoff = if self.failures == 0 {
            self.cooldown
        } else {
            let exp = self.cooldown.saturating_mul(1 << self.failures.min(10));
            exp.min(self.backoff_max).max(self.cooldown)
        };
        (end + backoff).saturating_duration_since(now)
    }

    /// Records a session end.
    pub fn ended(&mut self, how: &SessionEnd, now: Instant) {
        self.last_end = Some(now);
        match how {
            SessionEnd::Stopped => self.failures = 0,
            SessionEnd::Failed(_) => self.failures = self.failures.saturating_add(1),
        }
    }

    /// Consecutive failures.
    #[must_use]
    pub fn failures(&self) -> u32 {
        self.failures
    }
}

fn unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

struct Conn {
    tcp: TcpStream,
    buf: Vec<u8>,
    timeout: Duration,
}

impl Conn {
    async fn exchange(&mut self, req: &Request) -> Result<Response, String> {
        tokio::time::timeout(self.timeout, self.tcp.write_all(&req.bytes))
            .await
            .map_err(|_| format!("{} write timed out", req.method))?
            .map_err(|e| format!("{} write: {e}", req.method))?;
        let deadline = tokio::time::Instant::now() + self.timeout;
        loop {
            if let Some((resp, used)) = parse_response(&self.buf).map_err(|e| e.to_string())? {
                self.buf.drain(..used);
                return Ok(resp);
            }
            let mut chunk = [0u8; 4096];
            let n = tokio::time::timeout_at(deadline, self.tcp.read(&mut chunk))
                .await
                .map_err(|_| format!("{} response timed out", req.method))?
                .map_err(|e| format!("{} read: {e}", req.method))?;
            if n == 0 {
                return Err(format!(
                    "server closed the connection during {}",
                    req.method
                ));
            }
            self.buf
                .extend_from_slice(chunk.get(..n).unwrap_or_default());
        }
    }
}

fn rtsp_err(e: &RtspError) -> String {
    e.to_string()
}

/// Runs one session until `stop` is cancelled or it fails, sending Opus
/// frames to `out` (dropped, not blocked on, if the receiver lags) and
/// statistics to `stats`.
pub async fn run_session(
    cfg: &SessionConfig,
    out: &mpsc::Sender<CapturedFrame>,
    stats: &watch::Sender<RtpStats>,
    stop: &CancellationToken,
) -> SessionEnd {
    let udp_bind = SocketAddr::new(
        if cfg.rtsp.is_ipv4() {
            std::net::Ipv4Addr::UNSPECIFIED.into()
        } else {
            std::net::Ipv6Addr::UNSPECIFIED.into()
        },
        cfg.rtp_port,
    );
    let udp = match UdpSocket::bind(udp_bind).await {
        Ok(s) => s,
        Err(e) => return SessionEnd::Failed(format!("RTP socket: {e}")),
    };
    let client_port = match udp.local_addr() {
        Ok(a) => a.port(),
        Err(e) => return SessionEnd::Failed(format!("RTP socket: {e}")),
    };
    let tcp = match tokio::time::timeout(cfg.timeout, TcpStream::connect(cfg.rtsp)).await {
        Ok(Ok(t)) => t,
        Ok(Err(e)) => return SessionEnd::Failed(format!("RTSP connect: {e}")),
        Err(_) => return SessionEnd::Failed("RTSP connect timed out".into()),
    };
    let mut conn = Conn {
        tcp,
        buf: Vec::new(),
        timeout: cfg.timeout,
    };
    let mut session = RtspSession::new(&cfg.url(), client_port);
    let result = stream(cfg, &mut conn, &mut session, &udp, out, stats, stop).await;
    // Always tear down a session that was set up, whatever happened.
    if let Some(td) = session.teardown() {
        match conn.exchange(&td).await {
            Ok(r) if (200..300).contains(&r.status) => debug!("RTSP TEARDOWN acknowledged"),
            Ok(r) => warn!(status = r.status, "RTSP TEARDOWN refused"),
            Err(e) => warn!(error = %e, "RTSP TEARDOWN not acknowledged"),
        }
    }
    let _ = conn.tcp.shutdown().await;
    result
}

async fn stream(
    cfg: &SessionConfig,
    conn: &mut Conn,
    session: &mut RtspSession,
    udp: &UdpSocket,
    out: &mpsc::Sender<CapturedFrame>,
    stats: &watch::Sender<RtpStats>,
    stop: &CancellationToken,
) -> SessionEnd {
    macro_rules! step {
        ($req:expr, $handle:ident) => {{
            let req = match $req {
                Ok(r) => r,
                Err(e) => return SessionEnd::Failed(rtsp_err(&e)),
            };
            let resp = tokio::select! {
                () = stop.cancelled() => return SessionEnd::Stopped,
                r = conn.exchange(&req) => match r {
                    Ok(r) => r,
                    Err(e) => return SessionEnd::Failed(e),
                },
            };
            if let Err(e) = session.$handle(&req, &resp) {
                return SessionEnd::Failed(rtsp_err(&e));
            }
        }};
    }
    step!(session.describe(), on_describe);
    step!(session.setup(), on_setup);
    step!(session.play(), on_play);
    info!(scanner = %cfg.rtsp, client_port = udp.local_addr().map(|a| a.port()).unwrap_or(0), "audio session playing");

    let mut jitter = JitterBuffer::new(cfg.jitter_depth);
    let mut encoder = match Encoder::new() {
        Ok(e) => e,
        Err(e) => return SessionEnd::Failed(e.to_string()),
    };
    let mut keepalive = tokio::time::interval(session.keepalive_interval());
    keepalive.tick().await;
    let mut publish = tokio::time::interval(Duration::from_secs(1));
    let mut last_rtp = Instant::now();
    let started = Instant::now();
    let mut buf = vec![0u8; 2048];
    let mut pcm = Vec::with_capacity(FRAME_SAMPLES * 2);
    let mut last_samples = FRAME_SAMPLES;
    let mut dropped: u64 = 0;
    let scanner_ip = cfg.rtsp.ip().to_canonical();
    loop {
        tokio::select! {
            () = stop.cancelled() => return SessionEnd::Stopped,
            _ = keepalive.tick() => {
                let req = match session.keepalive() {
                    Ok(r) => r,
                    Err(e) => return SessionEnd::Failed(rtsp_err(&e)),
                };
                match conn.exchange(&req).await {
                    Ok(resp) => {
                        if let Err(e) = session.on_keepalive(&req, &resp) {
                            return SessionEnd::Failed(rtsp_err(&e));
                        }
                    }
                    Err(e) => return SessionEnd::Failed(e),
                }
            }
            _ = publish.tick() => {
                stats.send_replace(jitter.stats());
                if last_rtp.elapsed() > cfg.idle_timeout {
                    return SessionEnd::Failed(format!("no RTP for {:?}", cfg.idle_timeout));
                }
            }
            r = udp.recv_from(&mut buf) => {
                let (n, from) = match r {
                    Ok(v) => v,
                    Err(e) => return SessionEnd::Failed(format!("RTP receive: {e}")),
                };
                if from.ip().to_canonical() != scanner_ip {
                    continue; // never accept audio from anyone but the scanner
                }
                let Ok(pkt) = RtpPacket::parse(buf.get(..n).unwrap_or_default()) else { continue };
                if pkt.payload_type != 0 || pkt.payload.is_empty() || pkt.payload.len() > 1200 {
                    continue;
                }
                last_rtp = Instant::now();
                let arrival = u32::try_from(started.elapsed().as_millis().saturating_mul(8) % (1u128 << 32)).unwrap_or(0);
                jitter.push(pkt.ssrc, pkt.sequence, pkt.timestamp, pkt.payload, arrival);
                for played in jitter.pop_ready() {
                    let frames = match played {
                        Played::Packet { payload, .. } => {
                            last_samples = payload.len();
                            pcm.clear();
                            g711::decode_into(&payload, &mut pcm);
                            encoder.push(&pcm)
                        }
                        Played::Lost => encoder.conceal(last_samples),
                    };
                    let frames = match frames {
                        Ok(f) => f,
                        Err(e) => return SessionEnd::Failed(e.to_string()),
                    };
                    let now = unix_ms();
                    for frame in frames {
                        if out.try_send(CapturedFrame { captured_ms: now, frame }).is_err() {
                            dropped += 1;
                            if dropped.is_power_of_two() {
                                warn!(dropped, "audio consumer lagging; frames dropped");
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supervisor_spaces_sessions() {
        let mut s = Supervisor::new(Duration::from_secs(10), Duration::from_secs(300));
        let t0 = Instant::now();
        assert_eq!(s.wait(t0), Duration::ZERO, "first session starts at once");
        s.ended(&SessionEnd::Stopped, t0);
        assert_eq!(
            s.wait(t0),
            Duration::from_secs(10),
            "cooldown after a clean stop"
        );
        assert_eq!(s.wait(t0 + Duration::from_secs(4)), Duration::from_secs(6));
        s.ended(&SessionEnd::Failed("x".into()), t0);
        assert_eq!(s.wait(t0), Duration::from_secs(20));
        for _ in 0..10 {
            s.ended(&SessionEnd::Failed("x".into()), t0);
        }
        assert_eq!(s.wait(t0), Duration::from_secs(300), "capped");
        s.ended(&SessionEnd::Stopped, t0);
        assert_eq!(s.failures(), 0);
    }

    #[test]
    fn urls() {
        let mut c = SessionConfig::new("192.168.1.5:554".parse().unwrap_or_else(|e| panic!("{e}")));
        assert_eq!(c.url(), "rtsp://192.168.1.5/au:scanner.au");
        c.rtsp = "[fd00::5]:8554".parse().unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(c.url(), "rtsp://[fd00::5]:8554/au:scanner.au");
    }
}
