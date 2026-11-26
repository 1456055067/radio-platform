//! Simulated SDS200 audio server: RTSP/1.0 on TCP, PCMU RTP on UDP.
//!
//! It mirrors the community-reported behaviour in
//! `docs/protocol-compliance.md` so the platform's safety rules can be
//! tested:
//! - the SDP has `m=audio 0 RTP/AVP 0` and `a=control:trackID=1`, with no
//!   rtpmap;
//! - **one client at a time**: a second client gets `453 Not Enough
//!   Bandwidth`;
//! - a session that disappears **without `TEARDOWN`** locks the server
//!   (connections are accepted but never answered) until
//!   [`crate::Simulator::power_cycle_audio`];
//! - RTP is 20 ms of µ-law every 20 ms: a tone during a simulated
//!   transmission, silence otherwise.
//!
//! The µ-law encoder and RTP header writer here are deliberately separate
//! from the platform's decoder, so tests do not check the client against
//! itself.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio_util::sync::CancellationToken;

use crate::{Shared, lock};

/// Audio server statistics and fault settings.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AudioStats {
    /// Sessions that reached PLAY.
    pub sessions: u64,
    /// TEARDOWNs received.
    pub teardowns: u64,
    /// Clients refused because another session was active.
    pub rejected: u64,
    /// Times the server locked up.
    pub lockups: u64,
    /// Currently locked (needs a power cycle).
    pub locked: bool,
    /// A session is active.
    pub active: bool,
    /// RTP packets sent.
    pub packets: u64,
    /// RTSP requests received, in order (method names).
    pub requests: Vec<String>,
}

/// RTP impairments.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct AudioFaults {
    /// Probability that a packet is not sent.
    pub loss: f64,
    /// Probability that a packet is held back and sent after the next one.
    pub reorder: f64,
}

#[derive(Debug, Default)]
pub(crate) struct AudioShared {
    pub(crate) stats: Mutex<AudioStats>,
    pub(crate) faults: Mutex<AudioFaults>,
}

fn ulaw(sample: i16) -> u8 {
    // Classic G.711 µ-law encoder (independent of the platform's codec).
    let mut s = i32::from(sample);
    let sign: u8 = if s < 0 {
        s = -s;
        0x80
    } else {
        0
    };
    s = s.min(32635) + 132;
    let mut exp: u8 = 7;
    let mut mask = 0x4000;
    while exp > 0 && (s & mask) == 0 {
        exp -= 1;
        mask >>= 1;
    }
    let mant = ((s >> (exp + 3)) & 0x0F) as u8;
    !(sign | (exp << 4) | mant)
}

fn rtp(seq: u16, ts: u32, ssrc: u32, payload: &[u8]) -> Vec<u8> {
    let mut v = vec![0x80, 0x00];
    v.extend_from_slice(&seq.to_be_bytes());
    v.extend_from_slice(&ts.to_be_bytes());
    v.extend_from_slice(&ssrc.to_be_bytes());
    v.extend_from_slice(payload);
    v
}

/// Tone frequency for a channel name (stable, 300–1200 Hz).
#[must_use]
pub fn tone_hz(channel: &str) -> f64 {
    let h = channel
        .bytes()
        .fold(0u32, |a, b| a.wrapping_mul(31).wrapping_add(u32::from(b)));
    300.0 + f64::from(h % 900)
}

const SDP: &str = "v=0\r\no=- 1 1 IN IP4 0.0.0.0\r\ns=Scanner Audio\r\nt=0 0\r\nm=audio 0 RTP/AVP 0\r\na=control:trackID=1\r\n";

pub(crate) async fn serve(
    listener: TcpListener,
    shared: Arc<Shared>,
    audio: Arc<AudioShared>,
    cancel: CancellationToken,
) {
    loop {
        let (tcp, peer) = tokio::select! {
            () = cancel.cancelled() => return,
            r = listener.accept() => match r { Ok(v) => v, Err(_) => continue },
        };
        let (shared, audio, cancel) = (Arc::clone(&shared), Arc::clone(&audio), cancel.clone());
        tokio::spawn(async move { connection(tcp, peer, shared, audio, cancel).await });
    }
}

fn header<'a>(req: &'a str, name: &str) -> Option<&'a str> {
    req.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim().eq_ignore_ascii_case(name).then(|| v.trim())
    })
}

async fn connection(
    mut tcp: TcpStream,
    peer: SocketAddr,
    shared: Arc<Shared>,
    audio: Arc<AudioShared>,
    cancel: CancellationToken,
) {
    if lock(&audio.stats).locked {
        // Locked up: accept but never answer.
        let mut sink = [0u8; 1024];
        while let Ok(n) = tcp.read(&mut sink).await {
            if n == 0 {
                break;
            }
        }
        return;
    }
    let mut buf = Vec::new();
    let mut session: Option<String> = None;
    let mut playing: Option<CancellationToken> = None;
    let mut client_port: Option<u16> = None;
    let mut torn_down = false;
    let mut owns_slot = false;
    loop {
        let mut chunk = [0u8; 2048];
        let n = tokio::select! {
            () = cancel.cancelled() => 0,
            r = tcp.read(&mut chunk) => r.unwrap_or(0),
        };
        if n == 0 {
            break;
        }
        buf.extend_from_slice(chunk.get(..n).unwrap_or_default());
        while let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let req = String::from_utf8_lossy(buf.get(..end).unwrap_or_default()).into_owned();
            buf.drain(..end + 4);
            let method = req.split(' ').next().unwrap_or("").to_owned();
            let cseq = header(&req, "CSeq").unwrap_or("0").to_owned();
            lock(&audio.stats).requests.push(method.clone());
            let reply = |status: &str, extra: &str, body: &str| {
                format!(
                    "RTSP/1.0 {status}\r\nCSeq: {cseq}\r\nServer: Scanner Audio Server 0.0.1 (simulated)\r\n{extra}{}\r\n{body}",
                    if body.is_empty() {
                        String::new()
                    } else {
                        format!(
                            "Content-Type: application/sdp\r\nContent-Length: {}\r\n",
                            body.len()
                        )
                    }
                )
            };
            let out = match method.as_str() {
                "OPTIONS" => reply(
                    "200 OK",
                    "Public: OPTIONS, DESCRIBE, SETUP, TEARDOWN, PLAY, GET_PARAMETER\r\n",
                    "",
                ),
                "DESCRIBE" => {
                    let mut st = lock(&audio.stats);
                    if st.active && !owns_slot {
                        st.rejected += 1;
                        reply("453 Not Enough Bandwidth", "", "")
                    } else {
                        reply("200 OK", "", SDP)
                    }
                }
                "SETUP" => {
                    let port = header(&req, "Transport")
                        .and_then(|t| {
                            t.split(';')
                                .find_map(|p| p.trim().strip_prefix("client_port="))
                        })
                        .and_then(|p| p.split('-').next())
                        .and_then(|p| p.parse::<u16>().ok());
                    let mut st = lock(&audio.stats);
                    match port {
                        None => reply("461 Unsupported Transport", "", ""),
                        Some(_) if st.active && !owns_slot => {
                            st.rejected += 1;
                            reply("453 Not Enough Bandwidth", "", "")
                        }
                        Some(p) => {
                            st.active = true;
                            owns_slot = true;
                            client_port = Some(p);
                            let id = format!("{:08X}", 0x5D5_2000 + st.sessions);
                            session = Some(id.clone());
                            reply(
                                "200 OK",
                                &format!(
                                    "Session: {id};timeout=60\r\nTransport: RTP/AVP;unicast;client_port={p};server_port=6970\r\n"
                                ),
                                "",
                            )
                        }
                    }
                }
                "PLAY" => match (&session, client_port) {
                    (Some(id), Some(port)) if header(&req, "Session") == Some(id.as_str()) => {
                        if playing.is_none() {
                            lock(&audio.stats).sessions += 1;
                            let stop = cancel.child_token();
                            tokio::spawn(stream_rtp(
                                SocketAddr::new(peer.ip(), port),
                                Arc::clone(&shared),
                                Arc::clone(&audio),
                                stop.clone(),
                            ));
                            playing = Some(stop);
                        }
                        reply("200 OK", &format!("Session: {id}\r\n"), "")
                    }
                    _ => reply("455 Method Not Valid in This State", "", ""),
                },
                "GET_PARAMETER" => reply("200 OK", "", ""),
                "TEARDOWN" => {
                    if let Some(p) = playing.take() {
                        p.cancel();
                    }
                    let mut st = lock(&audio.stats);
                    if owns_slot {
                        st.teardowns += 1;
                        st.active = false;
                        owns_slot = false;
                    }
                    torn_down = true;
                    session = None;
                    reply("200 OK", "", "")
                }
                _ => reply("501 Not Implemented", "", ""),
            };
            if tcp.write_all(out.as_bytes()).await.is_err() {
                break;
            }
        }
    }
    if let Some(p) = playing.take() {
        p.cancel();
    }
    if owns_slot && !torn_down {
        // Reported fragility: losing a session without TEARDOWN hangs the
        // audio server until the scanner is power-cycled.
        let mut st = lock(&audio.stats);
        st.active = false;
        st.locked = true;
        st.lockups += 1;
    }
}

async fn stream_rtp(
    to: SocketAddr,
    shared: Arc<Shared>,
    audio: Arc<AudioShared>,
    stop: CancellationToken,
) {
    let bind: SocketAddr = if to.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" }
        .parse()
        .unwrap_or_else(|_| SocketAddr::from(([0, 0, 0, 0], 0)));
    let Ok(sock) = UdpSocket::bind(bind).await else {
        return;
    };
    let ssrc: u32 = 0x5D52_0001;
    let (mut seq, mut ts, mut phase) = (0u16, 0u32, 0f64);
    let mut held: Option<Vec<u8>> = None;
    let mut tick = tokio::time::interval(Duration::from_millis(20));
    loop {
        tokio::select! {
            () = stop.cancelled() => return,
            _ = tick.tick() => {}
        }
        let tone = lock(&shared.state)
            .transmission
            .as_ref()
            .map(|t| tone_hz(&t.channel));
        let payload: Vec<u8> = (0..160)
            .map(|_| match tone {
                Some(hz) => {
                    phase += 2.0 * std::f64::consts::PI * hz / 8000.0;
                    #[allow(clippy::cast_possible_truncation)]
                    let s = (phase.sin() * 9000.0) as i16;
                    ulaw(s)
                }
                None => 0xFF,
            })
            .collect();
        let pkt = rtp(seq, ts, ssrc, &payload);
        seq = seq.wrapping_add(1);
        ts = ts.wrapping_add(160);
        let faults = *lock(&audio.faults);
        if crate::chance(&shared, faults.loss) {
            continue;
        }
        if held.is_none() && crate::chance(&shared, faults.reorder) {
            held = Some(pkt);
            continue;
        }
        if sock.send_to(&pkt, to).await.is_ok() {
            lock(&audio.stats).packets += 1;
        }
        if let Some(h) = held.take() {
            let _ = sock.send_to(&h, to).await;
            lock(&audio.stats).packets += 1;
        }
    }
}
