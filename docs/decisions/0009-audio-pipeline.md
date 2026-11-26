# ADR 0009: Audio pipeline: one RTSP client at the edge, Opus upstream, WebRTC out

- **Status:** Accepted
- **Date:** 2026-11-25

## Context

The SDS200 serves audio over RTSP/RTP (PCMU, 8 kHz). Community reports
(`docs/protocol-compliance.md`) say:
- it accepts **one** client at a time;
- it can lock up until power-cycled after a missing `TEARDOWN`, a quick
  reconnect, or an out-of-order request.

None of this is hardware-verified yet. The scanner is on a site LAN; port
554 must never be exposed. Browsers need low-latency audio.

## Decision

1. **The site agent is the only RTSP client** of each scanner:
   - it opens a session only when audio is enabled for the scanner **and**
     someone listens or the scanner records;
   - it lingers briefly after demand ends, and spaces sessions with a
     cooldown and backoff;
   - it always sends `TEARDOWN`, and never probes port 554;
   - the RTSP endpoint must pass the agent's local network policy
     (`audio_ports`).
2. **The agent decodes µ-law and encodes Opus**: 8 kHz mono VoIP, 20 ms,
   about 16 kbit/s. The site's WAN uplink carries about a quarter of the
   PCMU bitrate. Frames travel as binary messages on the agent's existing
   mTLS WebSocket, so they reach the replica that already holds the agent;
   no new port or connection is needed.
3. **The gateway fans audio out over WebRTC** using str0m (sans-I/O, AWS-LC
   crypto):
   - signalling is WHEP-style: the client POSTs an SDP offer and gets an
     answer;
   - one UDP port serves every session;
   - only Opus is negotiated, and frames are forwarded without
     transcoding;
   - in a cluster, a WHEP offer is forwarded to the replica holding the
     agent, and media flows from that replica directly.
4. **Recording happens on the gateway**:
   - transmission events (same agent clock) cut the continuous Opus stream
     into segments, with a pre-buffer, post-roll and duration cap;
   - each segment is written as Ogg Opus to S3-compatible, local or memory
     storage, and its key is stored on the transmission;
   - recording is off unless configured on the gateway **and** enabled per
     scanner by a `recording_manage` user.
5. Audio and recording are **off by default** per scanner. Listening needs
   `audio_listen`. Every listen and playback is audited.

## Consequences

- No hardware is touched unless an operator enables audio for that
  scanner.
- **Audio is not buffered during WAN outages:**
  - live audio pauses;
  - transmissions during the outage are recorded as events without audio.
- **Recording timing depends on the agent's clock:** frames and events
  share it, so segments line up regardless of gateway clock skew.
- **Media port:** browsers must reach each replica's media UDP port, or a
  TURN server must be added later.
- **Build dependency:** libopus (BSD-3) is compiled from source, which needs
  cmake and a C compiler.
- **Unmaintained transitive crates:** object_store brings in `chrono` and
  `humantime`, which RustSec marks as unmaintained (warnings only).
