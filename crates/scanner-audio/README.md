# scanner-audio

The SDS200 audio client used by the site agent:
- RTSP session (sans-I/O messages plus an async runner) following the
  community-reported rules: one client, `TEARDOWN` always, cooldown and
  backoff (`Supervisor`), no probes;
- RTP parsing, a reordering jitter buffer with loss statistics;
- G.711 µ-law decoding, Opus encoding (libopus, 8 kHz VoIP, 20 ms);
- an Ogg Opus writer (RFC 7845).

**Status:** Phase 3. Tested against the simulator's audio server only;
**not verified on hardware.** See `docs/protocol-compliance.md` and ADR 0009.
