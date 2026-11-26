# Changelog

All notable changes are recorded here. The format follows Keep a Changelog.

## [Unreleased]

### Fixed
- The stored telemetry snapshot (`GET /api/v1/scanners/{id}/telemetry`)
  could stay up to 30 s behind the scanner. The gateway writes it at most
  once per second per scanner and dropped the write for a snapshot that
  arrived inside that second; with no further change, the next write came
  only with the agent's 30 s heartbeat. The live stream was not affected.
  Such snapshots are now written when the second has passed, and on agent
  disconnect.

### Changed
- Dependency updates (supersede Dependabot PRs #1-8 and #10-15):
  jsonwebtoken 11, x509-parser 0.18, tower-http 0.7, tokio-tungstenite
  0.29, utoipa 6, base64 0.23; react-router 8 and the npm minor group;
  actions/cache v6, actions/setup-node v7, docker/login-action v4,
  docker/setup-buildx-action v4, docker/build-push-action v7,
  azure/setup-helm v5. No code changes were needed.
- The console now needs Node 22.22 or later (react-router 8).
- Dependabot no longer proposes `@types/node` majors beyond the Node
  version CI runs, or majors from the `/fuzz` entry (they edited the root
  `Cargo.toml` without its lockfile and broke the `--locked` image build).

### Security
- OIDC: a token whose `iss` is not a single string, or whose `aud` is not
  a string or an array of strings, is now refused. jsonwebtoken 10 and 11
  accepted `"iss": ["<issuer>"]`; the signature and issuer value were
  still checked, so this was a conformance gap, not a bypass.
- Tests pin the JWT registered-claim checks, the agent identity taken
  from a client certificate (x509-parser 0.18 skips malformed SAN entries
  instead of failing the extension), and a 404 for `/assets/<file>/`.

## [0.2.0] - 2026-11-26

PostgreSQL transport security release. **Breaking:** an off-loopback
database URL without `sslmode=verify-full` now stops the gateway (see
below and ADR 0012). Still tested against `sds-simulator` only; **not
verified on SDS200 hardware**.

### Changed — BREAKING
- **Off-loopback PostgreSQL now requires `sslmode=verify-full`.** The
  gateway refuses to start (and `--check-config` exits 2) when the database
  URL actually used (`DATABASE_URL`, else `[database] url`) points at a host
  that is not `localhost`, `127.0.0.0/8` or `::1` and does not set
  `sslmode=verify-full`. `disable`, `allow`, `prefer`, `require`,
  `verify-ca` and a missing `sslmode` (the sqlx default `prefer`) are all
  refused. Loopback and Unix-socket databases are unaffected. `hostaddr` /
  `PGHOSTADDR` decide the destination, and the check is lexical (no DNS):
  any other name counts as remote, even one that resolves to loopback.
  - Opt-out: `[database] insecure_transport = true` in `gateway.toml`, or
    `database.insecureTransport=true` in the Helm chart. The gateway then
    starts and logs a warning at every start (also in `--check-config` and
    Helm NOTES).
  - **Helm default fails closed:** `database.insecureTransport` defaults to
    `false`, so an upgrade with an in-cluster plaintext PostgreSQL
    (for example `postgres://…@pg.db.svc.cluster.local/radio`) will not
    start. Before upgrading, either enable TLS on PostgreSQL and use
    `?sslmode=verify-full&sslrootcert=/etc/radio/database-ca/ca.crt` with
    `secrets.databaseCa`, or set `database.insecureTransport=true`.
    Run `api-gateway --check-config` with the production `DATABASE_URL` to
    check in advance.
  - The URL is now checked strictly: unknown query parameters, duplicate
    destination/TLS parameters and `sslrootcert=system` are rejected
    (previously ignored or misread by sqlx).
  - Use `verify-full`, not `verify-ca`: `sslrootcert` adds to the built-in
    Mozilla roots rather than replacing them. Rotating the database CA or
    server certificate needs a gateway restart.

### Added
- TLS to PostgreSQL (rustls with AWS-LC; no OpenSSL, no native-tls), with
  certificate and hostname verification and a private CA through
  `sslrootcert`. The cluster bus LISTEN connection uses the same TLS
  settings. New crate: `webpki-roots` (via sqlx).
- Helm: `database.insecureTransport` (default `false`) and
  `secrets.databaseCa` (optional Secret with key `ca.crt`, mounted at
  `/etc/radio/database-ca/ca.crt`).
- CI: the `postgres` job runs a TLS PostgreSQL with a throw-away test CA
  (verify-full, wrong CA, name mismatch, TLS on the LISTEN backend); the
  `helm` job checks that the rendered config refuses an in-cluster
  plaintext database by default and accepts it with the opt-out.
- Secure engineering workflow (`.claude/skills/radio-workflow`, nine agents
  in `.claude/agents`): plan, build, QA, parallel security / NIST 800-53 /
  protocol / performance review, fix loop, compliance docs, release gate.
- `docs/nist-800-53-controls.md`: NIST SP 800-53 Rev 5 control matrix (51
  controls) with cited implementations, tests and a POA&M list.
- `cargo deny` policy (`deny.toml`: licenses, crates.io only, OpenSSL
  banned) with a CI job, and weekly Dependabot updates with a 14-day
  cooldown.
- Playwright browser end-to-end tests (`frontend/e2e`, 13 tests) against
  `radio-devstack`, and an `e2e` CI job that uploads traces on failure.
- Console code-splitting: each page is loaded when first visited, React
  and the router are a separately cached chunk, and `oidc-client-ts` loads
  only in OIDC mode. A page whose chunk cannot be loaded (for example after
  a redeploy) offers a reload.

### Fixed
- The gateway served `index.html` (200, `text/html`) for a missing file
  under `/assets/`; it now answers 404, so a stale chunk fails cleanly.
- A TOML syntax error in the gateway or agent configuration no longer
  echoes the offending source line, which could contain a database URL
  with its password (SEC-003). Both binaries, at start-up and in
  `--check-config`, now report only `line L, column C: <message>`.

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
