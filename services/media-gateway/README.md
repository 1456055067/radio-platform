# media-gateway

Live scanner audio for browsers, used by the API gateway:
- `MediaHub`: per-scanner Opus fan-out and listener demand (the agent
  streams only while someone listens or the scanner records);
- `WebRtcServer`: WHEP-style WebRTC sessions on one UDP port (str0m,
  AWS-LC DTLS-SRTP, Opus only, session limits).

**Status:** Phase 3. Tested end to end with a str0m client and in the
browser against the dev stack (simulated scanners).
