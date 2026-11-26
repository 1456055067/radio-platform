# Implementation Roadmap

Legend: `[x]` means implemented, tested and documented. `[~]` means
partially done (see the note). `[ ]` means not started. **HW** marks items
that need verification on a real SDS200 before they count as complete.

## Phase 1 — Protocol Foundation (in progress)

- [x] Research official Uniden specifications. Obtained BCDx36HP v1.03.
  The SDS100 spec, V2.00 spec, *Virtual Serial on Network* spec and release
  notes were **not** obtained; see `protocol-compliance.md` R2–R5.
- [x] Research existing open-source SDS200 implementations (C1–C7).
- [x] Initialise the Rust workspace (edition 2024, workspace lints,
  `unsafe` forbidden).
- [x] `AGENTS.md`, `docs/protocol-compliance.md`, and this roadmap.
- [x] UDP transport: a connected socket per scanner, bounded receive buffer.
- [x] Command encoding with argument validation and a length limit.
- [x] Frame reassembly: CR-terminated lines, `CMD,<XML>,` replies,
  header-only datagrams, `Footer`/`Foot` multi-part sequences with gap
  detection, and size, part and field limits.
- [x] Safe XML parsing: no DTDs; depth, element, attribute and value limits.
- [x] Typed decoders for MDL, VER, STS, FQK, SQK, DQK, GSI/PSI, GLT, DTM,
  LCR, URC, VOL, SQL and OK replies; `ERR`/`NG`/`FER`/`ORER` and
  `CMD,ERR,code`.
- [x] Typed `ScannerInfo`: mode, system, department, site, channel,
  frequency, TGID, unit ID, signal, RSSI, volume, squelch, display areas and
  popups.
- [x] SDK reliability:
  - [x] timeouts (per line or XML format);
  - [x] configurable retries, for read-only commands only;
  - [x] exponential backoff;
  - [x] serialized per-scanner execution;
  - [x] bounded queue with `QueueFull`;
  - [x] reply correlation by command name;
  - [x] stale and duplicate draining;
  - [x] health tracking;
  - [x] graceful shutdown and cancellation;
  - [x] `state_change_unknown` on timed-out writes.
- [x] Capability detection using read-only probes.
- [x] PSI push subscription (broadcast channel).
- [x] Optional `KAL` keepalive (off by default).
- [x] Front-panel `KEY` emulation, `HLD`, `AVD`, `JPM` resume, `QSH`, `JNT`
  and `POF`.
- [x] Simulator:
  - [x] state, transmissions, multi-part lists and PSI pushes;
  - [x] faults: loss, latency, duplicates, malformed replies, split
    headers, outages;
  - [x] unsupported commands and power-off.
- [x] Unit, property-based and SDK-versus-simulator integration tests.
- [x] Four fuzz targets: assembler, XML, reply decoder, value parsers.
- [x] Hardware interoperability suite (opt-in, with separate opt-in for
  state changes).
- [ ] **HW**: validate model, firmware, volume, squelch and status
  retrieval on a real SDS200.
- [ ] Obtain SDS remote command spec V2.00 and the network spec; re-verify
  `protocol-compliance.md`.
- [ ] Typed `NXT`/`PRV`, `FQK`/`SQK`/`DQK` set, `DTM`/`LCR` set, `MSI`
  menu XML and `AST`/`APR` analyse streams.
- [ ] CI with GitHub Actions (workflow committed; not yet run on GitHub).

## Phase 2 — Scanner Management (core complete)

- [x] `platform-common`:
  - [x] typed IDs, telemetry and events, commands;
  - [x] `NetworkPolicy` (SSRF control);
  - [x] agent wire protocol.
- [x] `scanner-telemetry`:
  - [x] GSI/PSI snapshots;
  - [x] single-receiver transmission detector. **HW**: the `Mute`-based
    reception heuristic needs verification.
  - [x] push collector with poll fallback, plus health events.
- [x] `scanner-controller`:
  - [x] registry reconcile, with an agent-local policy check and a scanner
    limit;
  - [x] command mapping;
  - [x] exclusive leases.
- [x] `platform-auth`:
  - [x] OIDC (discovery or static JWKS, key rotation, max session age);
  - [x] ABAC (tenant, site scope, permissions).
- [x] `platform-storage`:
  - [x] migrations;
  - [x] `PgStore` and `MemoryStore`;
  - [x] shared conformance suite (PostgreSQL 18 verified locally; CI job
    added).
- [x] `api-gateway`:
  - [x] v1 REST endpoints, except audio and transcript (Phases 3 and 5);
  - [x] OpenAPI, pagination, validation, RFC 9457 errors;
  - [x] rate limiting, audit;
  - [x] live WebSocket;
  - [x] mTLS agent listener.
- [x] `scanner-agent`:
  - [x] mTLS 1.3 client;
  - [x] centralized assignments plus local bootstrap;
  - [x] bounded outage buffer with replay and dedupe;
  - [x] reconnection with backoff;
  - [x] remote command execution.
- [x] Multiple sites and multiple scanners per site; scanner groups at the
  storage level.
- [x] End-to-end test covering simulator, agent, mTLS, gateway and API,
  including outage replay and hostile-config rejection.
- [x] Multiple gateway replicas (ADR 0008):
  - leases in PostgreSQL, with atomic acquire and renew (8 concurrent
    acquirers, exactly one winner, verified on PostgreSQL 18);
  - a `LISTEN`/`NOTIFY` cluster bus for agent presence, command and
    discovery forwarding, live fan-out and configuration pushes;
  - two-replica end-to-end tests: in-process, and over PostgreSQL in CI.
- [x] Agent listing (`GET /api/v1/agents`, with live connection state). Done
  in Phase 4.
- [x] Scanner groups API: list, get, create and delete (tenant-wide scope),
  and membership (by the scanner's site). Listings never reveal members
  outside the caller's sites.
- [x] Scanner discovery (`POST /api/v1/sites/{id}/discover`):
  - **Research:** neither the obtained spec nor the community sources
    describe a discovery mechanism, so the agent probes with the read-only
    `MDL` command.
  - **Limits:** inside both the site's and the agent's own network policy,
    at most 1024 hosts, paced.
  - **Safety:** never probes enrolled or managed endpoints.
  - **Protocol:** agent wire protocol v2; v1 agents still connect and get
    `501`.
  - Probe behaviour on hardware is **unverified**.
- [x] Durable agent event journal (`state_dir`): fsynced appends,
  torn-tail recovery, compaction, and the session id persisted so the
  gateway still de-duplicates after an agent restart.
- [x] Agent certificate revocation:
  - CRLs on the agent listener, strict (revoked, unknown status or expired
    CRL are all refused);
  - periodic reload;
  - connected agents re-verify after a change.
- [x] Per-IP rate limiting before authentication, with an
  `X-Forwarded-For` trust list.
- [ ] Prometheus and OpenTelemetry instrumentation (moved to Phase 6
  observability).

## Phase 3 — Audio

Implemented and tested against the simulator only; **not verified on
hardware**. See ADR 0009.

- [x] RTSP client (`scanner-audio`) per the `protocol-compliance.md` audio
  findings:
  - [x] a single client per scanner (the site agent), on demand only;
  - [x] a cooldown before reconnecting, backoff and linger;
  - [x] `TEARDOWN` always sent (exact request order tested);
  - [x] no bare probes of port 554;
  - [x] the agent's local policy covers RTSP ports (`audio_ports`).
- [x] RTP reception: header validation, source-IP and payload-type checks, a
  reordering jitter buffer, and loss, late, duplicate and RFC 3550 jitter
  statistics. Loss is concealed with silence to keep timing.
- [x] PCMU decode and Opus encode (libopus, 8 kHz VoIP, 20 ms) at the agent;
  binary media frames on the agent's mTLS WebSocket (wire protocol v3).
- [x] WebRTC distribution (`media-gateway`: str0m with AWS-LC, WHEP, Opus
  only, one UDP port, session limits), listener demand relayed to agents,
  and cluster forwarding of WHEP offers.
- [x] Simulator: an RTSP server and PCMU RTP (channel-specific tones), one
  client at a time, a lock-up model and loss/reorder faults.
- [x] Transmission segmentation (pre-buffer, post-roll, duration cap) and
  Ogg Opus recording to S3-compatible, local or memory storage
  (`scanner-recorder`), **off by default** at both the gateway and the
  scanner level.
- [x] Console: live listening, recording playback, audio and recording
  settings, and media status.
- [x] Fuzz targets: RTSP responses, RTP with the jitter buffer, media
  frames.
- [ ] Hardware verification of every audio behaviour (see
  `protocol-compliance.md`).
- [ ] TURN support for browsers that cannot reach a replica's media UDP
  port.
- [ ] Buffering audio across WAN outages (audio during an outage is not
  recorded today).
- [x] Retention and deletion policies for recordings (Phase 5).

## Phase 4 — User Interface

The console lives in `frontend/` and uses React 19, TypeScript, Vite and
Cloudscape. It has been verified only against simulated scanners
(`radio-devstack`).

- [x] Gateway support:
  - `GET /api/v1/me`;
  - `GET /api/v1/agents`;
  - live state in the scanner list;
  - admins can see a lease id so they can revoke it;
  - static UI serving with a strict CSP and SPA fallback;
  - `/ui-config.json`.
- [x] Sign-in: OIDC authorization code with PKCE as a public client, tokens
  held in memory only. A development-only role picker works with
  `radio-devstack`.
- [x] Shared live stream: one WebSocket per tab, reconnect with backoff,
  re-authentication after a 4001 close.
- [x] Dashboard:
  - totals, online/offline and receiving-now counts;
  - scanner health and active transmissions;
  - sites, with a relative-position map that needs no map tiles;
  - agent status;
  - platform services, with later-phase services shown as "not
    available".
- [x] Virtual scanner:
  - live display, channel information and signal;
  - control ownership: take, renew, release and admin revoke;
  - volume and squelch (debounced, never retried);
  - hold system/department/channel, resume, and quick-search tune;
  - front-panel keys, labelled by provenance.
- [x] Device administration:
  - enroll, edit, enable/disable and remove scanners;
  - create sites with authorized networks;
  - register agents.
- [x] Transmission explorer:
  - filters for site, scanner, frequency, talkgroup and time range;
  - timeline;
  - detail view;
  - CSV/JSON export of loaded results, gated on `export`, with
    formula-injection guarding.
- [x] Events and audit log pages.
- [x] Frontend tests: 25 Vitest/Testing Library tests, including role
  gating, the lease flow, the live reducer, stream auth and reconnect, and
  CSV escaping.
- [x] `radio-devstack`: simulators, agents, gateway and a test issuer in one
  process, for UI work without hardware. Loopback only.
- [x] Live audio playback and recording playback (Phase 3).
- [x] Transcripts in the explorer (Phase 5): transcript search, status,
  re-transcription, holds and recording deletion.
- [ ] Server-side, audited export endpoint (today's export is client-side,
  covering what the user has already loaded).
- [x] Browser end-to-end tests in CI (Playwright, Chromium) against
  `radio-devstack`: roles, navigation, control lease with two users,
  live WebRTC audio, transcript search and playback, Analytics, audit
  verification.
- [x] Code-splitting: pages load on demand, React and the router are a
  separately cached chunk, and the OIDC client loads only in OIDC mode.
  Opening the Dashboard fetches 308 kB of JS (gzip) instead of 437 kB; a
  chunk from an older build shows a reload prompt (the gateway answers
  404 for it).

## Phase 5 — Advanced Capabilities

Verified only against simulated scanners and a **stub** transcription
service (ADR 0010).

- [x] Whisper-compatible transcription (`scanner-transcription`), **off by
  default** at both the gateway and the scanner level:
  - Ogg Opus decode to 16 kHz WAV;
  - `openai` (`/v1/audio/transcriptions`) and `whisper_cpp` (`/inference`)
    request styles;
  - HTTPS off loopback, API key from the environment, no redirects;
  - a durable queue on the transmission row, claimed with
    `FOR UPDATE SKIP LOCKED`, with retries for transient failures;
  - short recordings skipped.
- [x] Transcripts treated as content: `audio_listen` to read or search;
  searches audited.
- [x] Full-text search (`tsvector`, GIN, web-search syntax).
- [x] Retention (recordings, transcripts, transmissions, events), holds,
  manual recording deletion, and audited sweeps.
- [x] Analytics: activity per hour or day, and the busiest talkgroups,
  channels, frequencies, scanners and units.
- [x] Console: transcript search and display, Analytics page, transcription
  and retention status, the per-scanner transcription switch.
- [x] Stub Whisper service in `sds-simulator` (names the simulator's tone;
  it does not recognise speech).
- [x] Fuzz target: Ogg Opus read and decode.
- [ ] Transcription quality on real scanner audio with a real model (not
  measured).
- [ ] Testing against real whisper.cpp and OpenAI-compatible servers
  (implemented from their documentation only).
- [ ] Per-tenant or per-site retention policies.
- [ ] Word-level timestamps and speaker or unit attribution.

## Phase 6 — Production Hardening

- [x] Threat model through T53, and a route-by-route authorization matrix
  test (permissions, missing tokens, cross-tenant access) that fails for
  unlisted routes.
- [x] Audit logging: tamper-evident hash chain (ADR 0011), append-only
  trigger, verification endpoint, filters, audited export, hourly head
  logging.
- [x] Continuous fuzzing: nightly workflow with a persistent corpus and
  committed seeds.
- [x] Prometheus metrics on a separate listener.
- [x] Helm chart and multi-arch, signed images with SBOM and provenance;
  sandboxed systemd unit for agents.
- [x] Graceful SIGTERM shutdown; `--check-config`.
- [x] Benchmarks of hot paths (`docs/benchmarks.md`) and operations
  documentation (metrics, alerts, upgrades, backup and restore, incidents).
- [x] Hardware sign-off procedure (`docs/hardware-signoff.md`) and opt-in
  hardware audio tests.
- [ ] **Hardware interoperability sign-off itself** (needs an SDS200).
- [ ] Container image build verified locally (the local build ran out of
  VM disk; CI builds it).
- [ ] Helm chart installed on a real cluster (rendered and linted only).
- [ ] Load test of a deployed gateway with PostgreSQL and TLS.
- [ ] OpenTelemetry tracing; agent-side metrics.
- [ ] Partitioning of `events` and `transmissions`.
- [x] Console code-splitting (from Phase 4).
