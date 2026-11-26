//! WebRTC audio sessions (one per listening browser), served over a single
//! UDP socket with str0m (sans-I/O) and the AWS-LC crypto provider.
//!
//! Signalling is WHEP-style: the browser POSTs an SDP offer for a
//! receive-only audio transceiver and gets an answer; media then flows
//! over ICE/DTLS-SRTP. Only Opus is negotiated. The agent already encodes
//! Opus, so frames are forwarded without transcoding.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use platform_common::{AgentId, ScannerId};
use str0m::change::SdpOffer;
use str0m::media::{Frequency, MediaKind, MediaTime, Mid, Pt};
use str0m::net::{Protocol, Receive};
use str0m::{Candidate, Event, IceConnectionState, Input, Output, Rtc, RtcConfig};
use thiserror::Error;
use tokio::net::UdpSocket;
use tokio::sync::{broadcast, mpsc, oneshot};
use tracing::{debug, info, warn};

use crate::hub::{MediaHub, Subscription};

/// Session limits and addressing.
#[derive(Debug, Clone)]
pub struct WebRtcConfig {
    /// UDP address to bind for media.
    pub bind: SocketAddr,
    /// Address advertised to browsers (required when `bind` is a wildcard
    /// or behind NAT).
    pub public: Option<SocketAddr>,
    /// Maximum concurrent sessions on this replica.
    pub max_sessions: usize,
    /// Drop sessions that have not connected within this time.
    pub connect_timeout: Duration,
}

impl WebRtcConfig {
    /// Defaults for `bind`.
    #[must_use]
    pub fn new(bind: SocketAddr) -> Self {
        Self {
            bind,
            public: None,
            max_sessions: 256,
            connect_timeout: Duration::from_secs(30),
        }
    }
}

/// Session failures.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum WebRtcError {
    /// The offer is not valid SDP or has no usable audio.
    #[error("invalid offer: {0}")]
    Offer(String),
    /// Too many sessions.
    #[error("too many listening sessions")]
    Busy,
    /// The media task is gone.
    #[error("media service unavailable")]
    Unavailable,
}

/// Session identifier (used in the WHEP resource URL).
pub type SessionId = uuid::Uuid;

enum Cmd {
    Create {
        scanner_id: ScannerId,
        agent: AgentId,
        offer: String,
        reply: oneshot::Sender<Result<(SessionId, String), WebRtcError>>,
    },
    Close(SessionId),
}

/// Handle to the media task.
#[derive(Debug, Clone)]
pub struct WebRtcServer {
    cmd: mpsc::Sender<Cmd>,
    sessions: Arc<AtomicUsize>,
    local: SocketAddr,
}

impl std::fmt::Debug for Cmd {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Create { scanner_id, .. } => write!(f, "Create({scanner_id})"),
            Self::Close(id) => write!(f, "Close({id})"),
        }
    }
}

struct Session {
    rtc: Rtc,
    sub: Subscription,
    mid: Option<Mid>,
    pt: Option<Pt>,
    connected: bool,
    created: Instant,
    rtp: u64,
    timeout: Instant,
}

impl WebRtcServer {
    /// Binds the media socket and starts the session task.
    ///
    /// # Errors
    /// The socket cannot be bound, or no usable advertised address.
    pub async fn start(cfg: WebRtcConfig, hub: MediaHub) -> std::io::Result<Self> {
        let sock = UdpSocket::bind(cfg.bind).await?;
        let local = sock.local_addr()?;
        let advertised = match cfg.public {
            Some(p) => p,
            None if !local.ip().is_unspecified() => local,
            None => {
                return Err(std::io::Error::other(
                    "media.public must be set when binding a wildcard address",
                ));
            }
        };
        let (cmd, rx) = mpsc::channel(64);
        let sessions = Arc::new(AtomicUsize::new(0));
        let crypto = Arc::new(str0m::crypto::from_feature_flags());
        tokio::spawn(drive(
            sock,
            local,
            advertised,
            cfg,
            hub,
            rx,
            Arc::clone(&sessions),
            crypto,
        ));
        info!(%local, %advertised, "WebRTC media listening");
        Ok(Self {
            cmd,
            sessions,
            local,
        })
    }

    /// Local media address.
    #[must_use]
    pub fn local_addr(&self) -> SocketAddr {
        self.local
    }

    /// Active sessions.
    #[must_use]
    pub fn sessions(&self) -> usize {
        self.sessions.load(Ordering::Relaxed)
    }

    /// Accepts an SDP offer for `scanner_id`; returns the session id and
    /// the SDP answer.
    ///
    /// # Errors
    /// [`WebRtcError`].
    pub async fn create(
        &self,
        scanner_id: ScannerId,
        agent: &AgentId,
        offer: &str,
    ) -> Result<(SessionId, String), WebRtcError> {
        let (reply, rx) = oneshot::channel();
        self.cmd
            .send(Cmd::Create {
                scanner_id,
                agent: agent.clone(),
                offer: offer.to_owned(),
                reply,
            })
            .await
            .map_err(|_| WebRtcError::Unavailable)?;
        rx.await.map_err(|_| WebRtcError::Unavailable)?
    }

    /// Ends a session (idempotent).
    pub fn close(&self, id: SessionId) {
        let _ = self.cmd.try_send(Cmd::Close(id));
    }
}

/// Drains one session's output; returns false if it should be removed.
fn drain(s: &mut Session, sock: &UdpSocket) -> bool {
    loop {
        match s.rtc.poll_output() {
            Ok(Output::Timeout(t)) => {
                s.timeout = t;
                return s.rtc.is_alive();
            }
            Ok(Output::Transmit(t)) => {
                let _ = sock.try_send_to(&t.contents, t.destination);
            }
            Ok(Output::Event(e)) => match e {
                Event::Connected => s.connected = true,
                Event::MediaAdded(m) if m.kind == MediaKind::Audio && s.mid.is_none() => {
                    s.mid = Some(m.mid);
                }
                Event::IceConnectionStateChange(IceConnectionState::Disconnected) => return false,
                _ => {}
            },
            Err(e) => {
                debug!(error = %e, "WebRTC session error");
                return false;
            }
        }
    }
}

fn accept(
    offer: &str,
    advertised: SocketAddr,
    crypto: &Arc<str0m::crypto::CryptoProvider>,
    now: Instant,
) -> Result<(Rtc, String), WebRtcError> {
    let offer = SdpOffer::from_sdp_string(offer).map_err(|e| WebRtcError::Offer(e.to_string()))?;
    let mut rtc = RtcConfig::new()
        .set_crypto_provider(Arc::clone(crypto))
        .clear_codecs()
        .enable_opus(true, false)
        .build(now);
    let cand = Candidate::host(advertised, "udp").map_err(|e| WebRtcError::Offer(e.to_string()))?;
    rtc.add_local_candidate(cand);
    let answer = rtc
        .sdp_api()
        .accept_offer(offer)
        .map_err(|e| WebRtcError::Offer(e.to_string()))?;
    Ok((rtc, answer.to_sdp_string()))
}

#[allow(clippy::too_many_arguments)]
async fn drive(
    sock: UdpSocket,
    local: SocketAddr,
    advertised: SocketAddr,
    cfg: WebRtcConfig,
    hub: MediaHub,
    mut cmds: mpsc::Receiver<Cmd>,
    count: Arc<AtomicUsize>,
    crypto: Arc<str0m::crypto::CryptoProvider>,
) {
    let mut sessions: HashMap<SessionId, Session> = HashMap::new();
    let mut buf = vec![0u8; 2000];
    loop {
        // Remove dead or never-connected sessions.
        let now = Instant::now();
        sessions.retain(|id, s| {
            let keep = s.rtc.is_alive()
                && (s.connected || now.duration_since(s.created) < cfg.connect_timeout);
            if !keep {
                debug!(session = %id, "WebRTC session ended");
            }
            keep
        });
        count.store(sessions.len(), Ordering::Relaxed);
        let next = sessions
            .values()
            .map(|s| s.timeout)
            .min()
            .unwrap_or(now + Duration::from_secs(1))
            .max(now + Duration::from_millis(1));

        tokio::select! {
            cmd = cmds.recv() => match cmd {
                None => return,
                Some(Cmd::Close(id)) => {
                    if let Some(mut s) = sessions.remove(&id) {
                        s.rtc.disconnect();
                        let _ = drain(&mut s, &sock);
                    }
                }
                Some(Cmd::Create { scanner_id, agent, offer, reply }) => {
                    if sessions.len() >= cfg.max_sessions {
                        let _ = reply.send(Err(WebRtcError::Busy));
                        continue;
                    }
                    match accept(&offer, advertised, &crypto, Instant::now()) {
                        Ok((rtc, answer)) => {
                            let id = uuid::Uuid::new_v4();
                            let mut s = Session {
                                rtc,
                                sub: hub.subscribe(scanner_id, &agent),
                                mid: None,
                                pt: None,
                                connected: false,
                                created: Instant::now(),
                                rtp: 0,
                                timeout: Instant::now(),
                            };
                            if drain(&mut s, &sock) {
                                sessions.insert(id, s);
                                let _ = reply.send(Ok((id, answer)));
                            } else {
                                let _ = reply.send(Err(WebRtcError::Offer("session failed to start".into())));
                            }
                        }
                        Err(e) => { let _ = reply.send(Err(e)); }
                    }
                }
            },
            r = sock.recv_from(&mut buf) => {
                let Ok((n, source)) = r else { continue };
                let Some(data) = buf.get(..n) else { continue };
                let Ok(contents) = data.try_into() else { continue };
                let input = Input::Receive(Instant::now(), Receive { proto: Protocol::Udp, source, destination: local, contents });
                let mut dead = None;
                for (id, s) in &mut sessions {
                    if s.rtc.accepts(&input) {
                        if s.rtc.handle_input(input).is_err() || !drain(s, &sock) {
                            dead = Some(*id);
                        }
                        break;
                    }
                }
                if let Some(id) = dead { sessions.remove(&id); }
            }
            () = hub.published() => {
                let now = Instant::now();
                let mut dead = Vec::new();
                for (id, s) in &mut sessions {
                    loop {
                        let frame = match s.sub.rx.try_recv() {
                            Ok(f) => f,
                            Err(broadcast::error::TryRecvError::Lagged(n)) => {
                                warn!(session = %id, skipped = n, "listener lagged");
                                continue;
                            }
                            Err(_) => break,
                        };
                        let (Some(mid), true) = (s.mid, s.connected) else { continue };
                        let Some(w) = s.rtc.writer(mid) else { continue };
                        let pt = match s.pt {
                            Some(p) => p,
                            None => match w.payload_params().next().map(str0m::format::PayloadParams::pt) {
                                Some(p) => { s.pt = Some(p); p }
                                None => continue,
                            },
                        };
                        let t = MediaTime::new(s.rtp, Frequency::FORTY_EIGHT_KHZ);
                        s.rtp += 960;
                        if w.write(pt, now, t, frame.opus.clone()).is_err() || !drain(s, &sock) {
                            dead.push(*id);
                            break;
                        }
                    }
                }
                for id in dead { sessions.remove(&id); }
            }
            () = tokio::time::sleep_until(tokio::time::Instant::from_std(next)) => {
                let now = Instant::now();
                let mut dead = Vec::new();
                for (id, s) in &mut sessions {
                    if s.timeout <= now && (s.rtc.handle_input(Input::Timeout(now)).is_err() || !drain(s, &sock)) {
                        dead.push(*id);
                    }
                }
                for id in dead { sessions.remove(&id); }
            }
        }
    }
}
