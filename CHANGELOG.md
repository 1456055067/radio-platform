# Changelog

All notable changes are recorded here. The format follows Keep a Changelog.

## [Unreleased]

### Added
- Playwright browser end-to-end tests (`frontend/e2e`, 13 tests) against
  `radio-devstack`, and an `e2e` CI job that uploads traces on failure.

## [0.1.0] - 2026-11-26

First release. Tested against `sds-simulator` only; **not verified on
SDS200 hardware** (see `docs/hardware-signoff.md`).

### Added — Phase 6 (hardening)
- Authorization matrix test over every documented route.
- Tamper-evident audit log (ADR 0011, migration 0005): per-tenant SHA-256
  chain, append-only trigger, `GET /audit/verify`, audit filters, audited
  `GET /audit/export` (JSON Lines or CSV), hourly chain-head log lines.
- Prometheus metrics on a separate `[metrics]` listener.
- `--check-config` for both binaries; gateway start-up validation.
- Container images (distroless, non-root), Helm chart, sandboxed systemd
  unit for agents, image workflow (multi-arch, SBOM, provenance, cosign),
  nightly fuzzing, chart and container CI jobs.
- Criterion benchmarks (`radio-benches`), opt-in hardware audio tests, the
  simulator's `--audio` flag.
- Docs: operations (metrics, alerts, upgrades, backup, incidents),
  benchmarks, hardware sign-off, threat model T49–T53.
- Console: audit filters, chain verification, export.

### Fixed
- Both binaries now shut down gracefully on SIGTERM (previously only
  Ctrl-C).

### Added — Phase 5 (transcription, search, retention, analytics; simulator and stub only)
- `scanner-transcription`: Ogg Opus decode to 16 kHz WAV, a Whisper-compatible
  HTTP client (`openai` and `whisper_cpp` request styles), configuration
  checks (HTTPS off loopback, key from the environment, no redirects).
- `scanner-audio`: Ogg Opus reader with checksum and size limits.
- Gateway:
  - `[transcription]` and `[retention]` (both off by default);
  - transcription worker with a durable queue on transmissions, claim
    leases, backoff and an attempt cap;
  - retention sweeper with holds and per-tenant audit records;
  - endpoints: transcript search (`q`), `recorded` and `transcription`
    filters, re-transcribe, hold, delete recording, transcription status,
    retention status, analytics activity and top;
  - transcript fields removed without `audio_listen`.
- Store: transcription queue, holds, retention and analytics methods on
  both backends; migration `0004_transcription_retention.sql`.
- Simulator: stub Whisper service that names the simulator's tone.
- Console: transcript search and display, re-transcription, holds and
  deletion, Analytics page, transcription and retention status, the
  per-scanner transcription switch.
- Devstack: stub transcription on the recording scanner, and a sample
  retention policy.
- Fuzz target `ogg_opus`. ADR 0010.

### Fixed
- `RecordingStore::delete` treats a missing object as deleted, so
  concurrent sweeps cannot fail each other.

### Added — Phase 3 (audio; simulator-verified only)
- `scanner-audio`:
  - RTSP client: sans-I/O messages, strict request order, TEARDOWN on
    every path, cooldown and backoff supervisor;
  - RTP parser and jitter buffer with loss statistics;
  - G.711 µ-law, Opus (libopus) encoder, Ogg Opus writer.
- Simulator audio server: RTSP plus PCMU RTP tones, one client at a time,
  the lock-up model, and loss and reorder faults.
- Agent:
  - per-scanner audio workers (on demand, linger, local `audio_ports`
    policy, `AudioState` reports);
  - binary Opus media frames;
  - `[audio]` settings.
- Wire protocol v3: `ScannerAssignment.audio`, `GatewayMessage::Audio`,
  `AgentMessage::AudioState`, `wire::media`.
- `media-gateway`: `MediaHub` with listener demand, and `WebRtcServer`
  (str0m, AWS-LC, WHEP, Opus only, limits).
- `scanner-recorder`: segmenter (pre-buffer, post-roll, cap), Ogg
  encoding, and S3, local or memory storage.
- Gateway:
  - `[media]` and `[recording]` (both off by default);
  - WHEP listen and hang-up endpoints, audio status, recording playback,
    media status;
  - scanner `audio_enabled`, `recording_enabled` and `rtsp_port` (migration
    0003);
  - cluster forwarding of WHEP offers;
  - `limits.audio_ports`;
  - CSP `media-src 'self' blob:`.
- Console: live listening (WHEP), recording playback, audio and recording
  settings, and dashboard media status.
- Tests:
  - simulator audio-client tests;
  - WebRTC test with a str0m client;
  - an end-to-end audio test (live listening, recording, playback,
    permissions, teardown without lock-up);
  - segmenter and recorder tests;
  - frontend WHEP test.
- Fuzz targets: `rtsp_response`, `rtp_jitter`, `media_frame`.
- ADR 0009.

### Added — Phase 2 completion
- **Multiple gateway replicas** (ADR 0008, supersedes 0006):
  - control leases in PostgreSQL (`scanner_leases`, atomic upsert;
    migration `0002`);
  - `MessageBus` with `PgBus` (`LISTEN`/`NOTIFY`, spill table for large
    payloads) and `MemoryBus`;
  - the gateway `Cluster`: presence, request forwarding, live fan-out,
    configuration pushes;
  - `[cluster] enabled`.
- **Scanner groups API:** `/api/v1/groups`, membership endpoints and
  scope-aware listings.
- **Scanner discovery:**
  - `POST /api/v1/sites/{id}/discover`, using read-only `MDL` probes from
    the site agent;
  - double policy enforcement, a 1024-host cap, pacing, and enrolled or
    managed endpoints never probed;
  - wire protocol v2 (v1 agents still accepted; discovery returns 501 for
    them);
  - a console **Discover** dialog that prefills enrollment.
- **Durable agent journal** (`state_dir`): restarts keep unacknowledged
  events and the session id.
- **Agent certificate revocation:** `agents.client_crl` with strict
  checking, hot reload and forced re-verification.
- **Per-IP rate limiting before authentication:**
  `limits.requests_per_minute_per_ip` and `limits.trusted_proxies`.
- The devstack runs an unenrolled simulator for trying discovery.
- Tests:
  - lease conformance on both stores (including concurrency);
  - Postgres bus delivery, inline and spilled;
  - two-replica end-to-end, in-process and over PostgreSQL;
  - agent restart durability;
  - discovery policy;
  - real-handshake CRL tests;
  - groups and per-IP limit API tests.

### Fixed
- `deployments/examples/agent.example.toml` placed `max_buffered_events`
  and `max_scanners` under `[telemetry]`, where they were silently ignored.
  The agent configuration now rejects unknown keys.

### Removed
- `scanner_controller::LeaseManager`; leases now live in the store.

### Added — Phase 4 (web console)
- `frontend/`: React 19 + TypeScript + Vite + Cloudscape console.
  - Dashboard:
    - fleet counts and scanner health;
    - live active transmissions;
    - sites, with a relative-position map;
    - agent status;
    - platform services, with later-phase services shown as "not
      available".
  - Virtual scanner:
    - live display and channel information;
    - control ownership: take, renew, release and admin revoke;
    - volume and squelch;
    - hold, resume and quick-search tune;
    - front-panel keys labelled by provenance.
  - Device administration: scanners, sites and agents.
  - Transmission explorer:
    - filters, timeline and detail view;
    - CSV/JSON export with formula-injection guarding.
  - Events and audit log pages.
  - OIDC authorization code with PKCE, tokens in memory only.
  - One shared live WebSocket with backoff and re-authentication.
  - 25 Vitest/Testing Library tests.
- Gateway:
  - `GET /api/v1/me`;
  - `GET /api/v1/agents` (`Store::list_agents` on both backends, in the
    conformance suite);
  - live `agent_connected` and `control` in `GET /api/v1/scanners`;
  - optional static UI serving (`[http.ui]`) with `/ui-config.json`, a
    strict CSP, security headers on all responses, `no-store` on the API
    and HSTS when the gateway terminates TLS.
- `tests/devstack` (`radio-devstack`), development only:
  - four simulators (one offline), two mTLS agents, the gateway and a
    test issuer;
  - enrollment through the real API;
  - invented radio traffic;
  - loopback only.
- CI: a `frontend` job (npm ci, audit, typecheck, tests, build).

### Changed
- `scanner_admin` users now receive `control.lease_id`, so revocation is
  possible. Previously the API allowed revoking but never revealed the id.
  Commands are still checked against the lease holder.

### Added — Phase 2
- `platform-common`:
  - shared IDs, telemetry, events and commands;
  - `NetworkPolicy`;
  - the agent wire protocol.
- `scanner-telemetry`: snapshot conversion, single-receiver transmission
  detector, and a push/poll collector.
- `scanner-controller`: agent scanner registry, command mapping and
  exclusive operator leases.
- `platform-auth`: OIDC JWT verification (JWKS discovery, rotation, static
  JWKS) and ABAC.
- `platform-storage`:
  - the `Store` trait;
  - PostgreSQL (sqlx 0.9, static SQL) and in-memory backends;
  - migrations and a conformance suite.
- `api-gateway`:
  - v1 REST API and live WebSocket;
  - mTLS agent listener;
  - leases, audit, rate limiting and OpenAPI.
- `scanner-agent`: mTLS 1.3 client, outage buffering with replay,
  reconnection, and local network-policy enforcement.
- End-to-end tests; CI jobs for PostgreSQL and a lockfile-advisory guard;
  ADRs 0004–0007; example configurations.

### Added — Phase 1
- Rust 2024 workspace with strict lints (`unsafe` forbidden; no unwrap,
  expect or panic in production code).
- `uniden-protocol`:
  - typed SDS command set with `Effect` and `Provenance` metadata;
  - datagram and multi-part XML frame assembler;
  - safe bounded XML parser;
  - typed `ScannerInfo` and `GLT` decoding.
- `uniden-sds`:
  - async per-scanner UDP actor with serialized execution;
  - timeouts, plus retries with backoff for read-only commands only;
  - bounded queue;
  - health tracking, statistics, PSI subscription, capability probing and
    graceful shutdown.
- `sds-simulator`: UDP SDS200 simulator with fault injection, and a CLI.
- Unit, property-based, integration, fuzz and opt-in hardware test suites.
- Protocol research and documentation: `docs/protocol-compliance.md`,
  architecture, security, threat model, roadmap and ADRs.
