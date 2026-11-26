//! Agent ↔ gateway protocol, carried as JSON text frames over a WebSocket
//! inside mutually authenticated TLS 1.3.
//!
//! - **Events** are sequenced. The agent buffers them until the gateway
//!   acknowledges them, so they survive upstream outages, up to the agent's
//!   buffer bound.
//! - **Telemetry** is latest-value only. It is not buffered; the agent sends
//!   a fresh snapshot after it reconnects.
//! - **Commands** are request/response, correlated by [`RequestId`].
//!
//! Versions: 1 is the original protocol; 2 adds [`GatewayMessage::Discover`];
//! 3 adds audio ([`ScannerAssignment::audio`], [`GatewayMessage::Audio`],
//! [`AgentMessage::AudioState`] and binary [`media`] frames).
//! The gateway accepts agents from [`MIN_PROTOCOL_VERSION`] up to
//! [`PROTOCOL_VERSION`] and never sends a message an agent's version does
//! not know.

use std::net::{IpAddr, SocketAddr};

use serde::{Deserialize, Serialize};

use crate::command::{CommandOutcome, ScannerCommand};
use crate::ids::{RequestId, ScannerId, SiteId};
use crate::telemetry::{ScannerEvent, ScannerHealth, TelemetrySnapshot};

/// Wire protocol version spoken by this build.
pub const PROTOCOL_VERSION: u16 = 3;
/// First protocol version with audio.
pub const AUDIO_PROTOCOL_VERSION: u16 = 3;
/// Oldest agent protocol the gateway still accepts.
pub const MIN_PROTOCOL_VERSION: u16 = 1;
/// First protocol version with [`GatewayMessage::Discover`].
pub const DISCOVERY_PROTOCOL_VERSION: u16 = 2;

/// A scanner that answered a discovery probe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Discovered {
    /// Address that answered.
    #[schema(value_type = String)]
    pub host: IpAddr,
    /// UDP port.
    pub port: u16,
    /// Model string from the `MDL` reply, verbatim.
    pub model: String,
}

/// Maximum accepted WebSocket message size in bytes.
pub const MAX_MESSAGE_BYTES: usize = 1024 * 1024;

/// WebSocket path served by the gateway's agent listener.
pub const AGENT_PATH: &str = "/agent/v1";

/// A scanner as configured on an agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScannerAssignment {
    /// Scanner identifier.
    pub scanner_id: ScannerId,
    /// Site identifier.
    pub site_id: Option<SiteId>,
    /// Display name.
    pub name: String,
    /// Scanner IP address.
    pub host: IpAddr,
    /// Scanner UDP command port.
    pub port: u16,
    /// Whether the agent should manage it.
    pub enabled: bool,
    /// Audio settings; `None` means the agent must never open the
    /// scanner's RTSP server (protocol ≥ 3; absent from older gateways).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio: Option<AudioAssignment>,
}

/// Audio settings for one scanner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioAssignment {
    /// RTSP port (554 on the SDS200).
    pub rtsp_port: u16,
    /// Stream continuously so the gateway can record transmissions.
    pub record: bool,
}

/// State of a scanner's audio pipeline on the agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum AudioState {
    /// Not streaming (no listeners and not recording).
    Idle,
    /// Waiting out the reconnect cooldown.
    Cooldown {
        /// Seconds until the next attempt.
        retry_in_secs: u64,
    },
    /// RTSP session being set up.
    Starting,
    /// Streaming.
    Streaming {
        /// RTP packets received.
        received: u64,
        /// RTP packets lost.
        lost: u64,
    },
    /// The last session failed.
    Failed {
        /// Reason.
        message: String,
    },
}

/// Scanner status reported in `Hello`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScannerReport {
    /// Scanner.
    pub scanner_id: ScannerId,
    /// Health.
    pub health: ScannerHealth,
    /// Model, once known.
    pub model: Option<String>,
    /// Firmware, once known.
    pub firmware: Option<String>,
}

/// Messages from agent to gateway.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentMessage {
    /// First message after connecting.
    Hello {
        /// The agent's protocol version (see the module docs).
        protocol_version: u16,
        /// Agent session identifier: random per process, or persisted with
        /// the event journal so it survives restarts. Event sequence numbers
        /// are unique per `(agent, session)`, which lets the gateway discard
        /// duplicates replayed after a reconnect.
        session: uuid::Uuid,
        /// Agent software version.
        agent_version: String,
        /// Managed scanners.
        scanners: Vec<ScannerReport>,
    },
    /// A buffered, sequenced event. Sequence numbers increase by one.
    Event {
        /// Sequence number (starts at 1 per agent process).
        seq: u64,
        /// Scanner.
        scanner_id: ScannerId,
        /// Event.
        event: ScannerEvent,
    },
    /// Latest telemetry (not buffered).
    Telemetry {
        /// Scanner.
        scanner_id: ScannerId,
        /// Snapshot.
        snapshot: Box<TelemetrySnapshot>,
    },
    /// Reply to [`GatewayMessage::Execute`].
    CommandResult {
        /// Request being answered.
        request_id: RequestId,
        /// Outcome.
        outcome: CommandOutcome,
    },
    /// Current scanner status (model, firmware, health), sent after each
    /// change of assignments.
    Status {
        /// Managed scanners.
        scanners: Vec<ScannerReport>,
    },
    /// Audio pipeline state change (protocol ≥ 3).
    AudioState {
        /// Scanner.
        scanner_id: ScannerId,
        /// New state.
        state: AudioState,
    },
    /// Reply to [`GatewayMessage::Ping`].
    Pong {
        /// Echoed nonce.
        nonce: u64,
    },
}

/// Messages from gateway to agent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GatewayMessage {
    /// Reply to `Hello`, carrying centrally managed scanner assignments.
    Welcome {
        /// Scanner assignments for this agent.
        scanners: Vec<ScannerAssignment>,
    },
    /// Events up to and including `seq` are durably stored.
    Ack {
        /// Highest acknowledged sequence number.
        seq: u64,
    },
    /// Execute a command.
    Execute {
        /// Correlation identifier.
        request_id: RequestId,
        /// Target scanner.
        scanner_id: ScannerId,
        /// Command.
        command: ScannerCommand,
    },
    /// Probe for scanners (protocol ≥ 2). The agent answers with
    /// `CommandResult` whose value is a JSON array of [`Discovered`]. It
    /// refuses networks or ports outside its own policy and never probes
    /// `exclude`.
    Discover {
        /// Correlation identifier.
        request_id: RequestId,
        /// Networks to probe (each within the agent's policy).
        networks: Vec<ipnet::IpNet>,
        /// UDP port to probe.
        port: u16,
        /// Endpoints never to probe (already enrolled or managed).
        exclude: Vec<SocketAddr>,
    },
    /// Live-listening demand for a scanner's audio (protocol ≥ 3). The
    /// agent streams while `listen` is true or the assignment records.
    Audio {
        /// Scanner.
        scanner_id: ScannerId,
        /// At least one live listener.
        listen: bool,
    },
    /// Replace scanner assignments.
    Configure {
        /// New assignments.
        scanners: Vec<ScannerAssignment>,
    },
    /// Liveness check.
    Ping {
        /// Nonce to echo.
        nonce: u64,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let msgs = vec![
            AgentMessage::Hello {
                protocol_version: 1,
                session: uuid::Uuid::new_v4(),
                agent_version: "0.1.0".into(),
                scanners: vec![],
            },
            AgentMessage::CommandResult {
                request_id: RequestId::new(),
                outcome: Err(crate::command::CommandError::Timeout {
                    state_change_unknown: true,
                }),
            },
            AgentMessage::Event {
                seq: 4,
                scanner_id: ScannerId::new(),
                event: ScannerEvent::HealthChanged {
                    health: ScannerHealth::Healthy,
                },
            },
        ];
        for m in msgs {
            let s = serde_json::to_string(&m).unwrap();
            assert_eq!(serde_json::from_str::<AgentMessage>(&s).unwrap(), m);
        }
        let g: GatewayMessage = serde_json::from_str(r#"{"type":"ack","seq":9}"#).unwrap();
        assert_eq!(g, GatewayMessage::Ack { seq: 9 });
    }
}

/// Binary media frames from agent to gateway (protocol ≥ 3), sent as
/// WebSocket binary messages:
///
/// | Bytes | Field |
/// |---|---|
/// | 1 | version (1) |
/// | 16 | scanner id (UUID) |
/// | 8 | capture time, Unix ms (big-endian i64) |
/// | 4 | frame sequence (big-endian u32) |
/// | 1 | flags (bit 0: concealed loss) |
/// | rest | one 20 ms Opus packet (1–1275 bytes) |
pub mod media {
    use crate::ids::ScannerId;

    /// Header length.
    pub const HEADER: usize = 30;
    /// Largest Opus packet (RFC 6716).
    pub const MAX_OPUS: usize = 1275;
    const VERSION: u8 = 1;

    /// A decoded media frame.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct MediaFrame {
        /// Scanner.
        pub scanner_id: ScannerId,
        /// Capture time (Unix ms, agent clock).
        pub captured_ms: i64,
        /// Per-scanner frame sequence.
        pub seq: u32,
        /// Stands in for lost input.
        pub concealed: bool,
        /// Opus packet.
        pub opus: Vec<u8>,
    }

    /// Encodes a frame.
    #[must_use]
    pub fn encode(f: &MediaFrame) -> Vec<u8> {
        let mut v = Vec::with_capacity(HEADER + f.opus.len());
        v.push(VERSION);
        v.extend_from_slice(f.scanner_id.0.as_bytes());
        v.extend_from_slice(&f.captured_ms.to_be_bytes());
        v.extend_from_slice(&f.seq.to_be_bytes());
        v.push(u8::from(f.concealed));
        v.extend_from_slice(&f.opus);
        v
    }

    /// Decodes a frame; `None` if malformed.
    #[must_use]
    pub fn decode(b: &[u8]) -> Option<MediaFrame> {
        if *b.first()? != VERSION {
            return None;
        }
        let id: [u8; 16] = b.get(1..17)?.try_into().ok()?;
        let ts: [u8; 8] = b.get(17..25)?.try_into().ok()?;
        let seq: [u8; 4] = b.get(25..29)?.try_into().ok()?;
        let flags = *b.get(29)?;
        let opus = b.get(HEADER..)?;
        if opus.is_empty() || opus.len() > MAX_OPUS || flags & !1 != 0 {
            return None;
        }
        Some(MediaFrame {
            scanner_id: ScannerId(uuid::Uuid::from_bytes(id)),
            captured_ms: i64::from_be_bytes(ts),
            seq: u32::from_be_bytes(seq),
            concealed: flags & 1 == 1,
            opus: opus.to_vec(),
        })
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn round_trip_and_rejects() {
            let f = MediaFrame {
                scanner_id: ScannerId::new(),
                captured_ms: 1_795_000_000_000,
                seq: 7,
                concealed: true,
                opus: vec![0xF8, 1, 2],
            };
            let b = encode(&f);
            assert_eq!(decode(&b), Some(f));
            assert_eq!(
                decode(b.get(..HEADER).unwrap_or_default()),
                None,
                "empty payload"
            );
            let mut bad = b.clone();
            bad[0] = 2;
            assert_eq!(decode(&bad), None, "unknown version");
            let mut bad = b;
            bad[29] = 0x80;
            assert_eq!(decode(&bad), None, "unknown flags");
            let mut big = encode(&MediaFrame {
                scanner_id: ScannerId::new(),
                captured_ms: 0,
                seq: 0,
                concealed: false,
                opus: vec![0; MAX_OPUS + 1],
            });
            assert_eq!(decode(&big), None);
            big.truncate(5);
            assert_eq!(decode(&big), None);
        }
    }
}
