# AGENTS.md — Repository Development Instructions

These instructions apply to all human and AI contributors.

## Ground rules

1. **Inspect before changing.** Read the relevant crate, its tests and
   `docs/protocol-compliance.md` before editing.
2. **Never fabricate protocol behaviour or test results.** Every protocol
   fact must cite a source in `docs/protocol-compliance.md` and carry a
   verification level: Spec, Spec (cited), Community, Implemented, Simulated
   or HW-verified. Do not record a hardware result without running it on
   hardware, and record the firmware version used.
3. **Keep simulated and verified behaviour separate.** The simulator
   (`tests/simulator`) models documented or community-reported behaviour.
   Passing simulator tests is not hardware evidence.
4. **Do not retry state-changing commands.** Every `Command` declares an
   `Effect`; only `ReadOnly` commands may be retried.
5. **Treat the scanner protocol as untrusted input.** Bound every buffer,
   queue and parser. Reject DTDs. Never panic on input.
6. **No `unwrap()`/`expect()`/`panic!` in production paths.** This is
   enforced by workspace clippy lints and allowed only in tests. Avoid
   `unsafe`; it is forbidden workspace-wide.
7. **No placeholders for completed requirements.** A roadmap item is
   checked only when it is implemented, tested and documented. Directories
   for future phases contain only a README stating their status.
8. **Legal scope.** Support only monitoring and recording that the operator
   is authorised to receive and retain. Never implement decryption or
   encryption bypass of protected radio communications.

## Layout

- `crates/uniden-protocol`: sans-I/O codec. No sockets, no async.
- `crates/uniden-sds`: async SDK (Tokio UDP actor per scanner).
- `crates/platform-common`: shared types, `NetworkPolicy`, agent wire protocol.
- `crates/scanner-telemetry`: snapshots, transmission detection, collector.
- `crates/scanner-controller`: agent registry, command mapping, leases.
- `crates/platform-auth`: OIDC verification and ABAC.
- `crates/platform-storage`: `Store` trait, PostgreSQL and in-memory backends.
- `services/api-gateway`: REST/WebSocket API and mTLS agent listener.
- `services/scanner-agent`: site agent.
- `migrations/`: PostgreSQL schema (embedded into the gateway).
- `tests/simulator`: `sds-simulator` library and binary.
- `tests/interoperability`: `#[ignore]`d hardware tests.
- `fuzz`: cargo-fuzz targets (nightly; outside the workspace).
- `crates/scanner-transcription`: recording decode and the Whisper-compatible
  client (the worker lives in the gateway).
- `frontend`: web console (React, TypeScript, Vite, Cloudscape). The API
  types in `src/api/types.ts` mirror the gateway JSON; update both together.
- `tests/devstack`: `radio-devstack`, a **development-only**, loopback-only
  simulated stack for UI work. Never package or deploy it.
- `docs`: architecture, protocol, security and roadmap. Update these
  **alongside** code changes.

The other `crates/*` and `services/*` directories belong to later phases;
see `docs/roadmap.md`.

## Quality gates (run before every milestone or commit)

```bash
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo audit
cargo deny check            # licenses, sources, bans, advisories (deny.toml)
(cd fuzz && cargo +nightly fuzz build)
(cd frontend && npm ci && npm audit && npm run typecheck && npm test && npm run build)
(cd frontend && npm run e2e)   # browser tests; needs `npx playwright install chromium` once
```

## Secure engineering workflow

Non-trivial changes go through the multi-agent workflow in
`.claude/skills/radio-workflow/` (`/radio-workflow full|review|plan …`):
plan → build → QA → parallel security, NIST 800-53, protocol and
performance review → fix loop (max 3 rounds) → compliance docs → release
gate. The agents live in `.claude/agents/`. Run artifacts go to
`target/agent-workflow/` and are never committed.

- `docs/nist-800-53-controls.md` is evidence. A control is "Implemented
  (tested)" only with a cited file and an existing test. Annotate code that
  implements a control with `// NIST 800-53 Rev5: <ID> (<reason>)`, and
  only where it really does; mark known gaps with `// POAM: <description>`.
- A gate whose environment is missing (PostgreSQL, hardware, helm, Podman)
  is NOT RUN, never PASS.

Frontend rules:
- Pin exact dependency versions, chosen from releases at least two weeks
  old. Keep `ignore-scripts=true`.
- Never retry state-changing requests.
- Never put tokens in localStorage or sessionStorage.
- Never render HTML from data.
- Show features that later phases add as "not available"; never fake them.

The PostgreSQL conformance suite runs when a server is available:

```bash
TEST_DATABASE_URL=postgres://postgres@127.0.0.1:5432/postgres cargo test -p platform-storage -- --include-ignored
TEST_DATABASE_URL=postgres://postgres@127.0.0.1:5432/postgres cargo test -p scanner-agent --test e2e -- --include-ignored two_replicas_over_postgres
# TLS (verify-full with a private CA; see the CI `postgres` job for the set-up):
TEST_TLS_DATABASE_URL='postgres://…@pg-tls.test:5433/postgres?sslmode=verify-full&sslrootcert=…/ca.crt' cargo test -p platform-storage --test postgres_tls -- --include-ignored
```

Security invariants that tests enforce. Do not weaken them:
- The agent checks every scanner endpoint against its **local** network
  policy.
- Every `Store` method on tenant data takes a `TenantId`.
- Control commands require an exclusive lease. `raw` requires
  `scanner_admin`.
- All SQL is static; values are bound parameters.
- **Database transport (ADR 0012):** off-loopback PostgreSQL requires
  `sslmode=verify-full` unless `[database] insecure_transport = true`.
  Connect only through `PgTarget::check` → `PgStore::connect_target`
  (`PgStore::connect` is for tests and the devstack). Never log or format a
  database URL or `PgConnectOptions`.
- The gateway never serves `/dev/*`. `radio-devstack` binds loopback only.
- Discovery never probes enrolled or managed endpoints. It sends only
  `MDL` and stays within the agent's local policy.
- Leases are acquired only through `Store::acquire_lease`, which is
  atomic. Never check-then-insert.
- Never send an agent a wire message newer than its negotiated protocol
  (`AgentOp::min_protocol`, `send_audio`).
- **Audio:**
  - only the agent opens a scanner's RTSP server, only while wanted;
  - always `TEARDOWN`, keep the cooldown, never probe port 554;
  - audio and recording stay off by default.
- **Authorization:** every route must be listed with its permissions in
  `services/api-gateway/tests/authz_matrix.rs`.
- **Audit (ADR 0011):** append only through `Store::append_audit` (it
  links the hash chain); never update or delete audit records; never sweep
  them.
- **Metrics** stay on their own listener and use route templates as
  labels, never identifiers.
- **Transcription and retention** (ADR 0010):
  - both stay off by default; transcription needs recording;
  - transcripts are content: `audio_listen` to read or search them;
  - audio goes only to the configured endpoint (HTTPS off loopback, no
    redirects, key from the environment);
  - retention deletes the object before detaching it, honours holds, and
    never sweeps the audit log.
- Building needs `cmake` and a C compiler (libopus).

Hardware tests are opt-in:

```bash
SDS200_ADDR=<ip>:50536 cargo test -p sds-interoperability -- --ignored --test-threads=1 --nocapture
```

Tests that change scanner state also need `SDS200_ALLOW_STATE_CHANGE=1`.

## Adding a protocol command

1. Find the official specification text. If none exists, mark the command
   `Provenance::CommunityReported` and cite the community source.
2. Add a typed `Command` variant with its `Effect` and `ReplyFormat`.
3. Add decoding to `response.rs`, with unit tests that use spec examples
   verbatim where they exist.
4. Add simulator support and an SDK integration test.
5. Extend fuzz targets if the change adds a new parser.
6. Update `docs/protocol-compliance.md` (syntax, evidence, status) and
   `docs/roadmap.md`.
7. Add a hardware test in `tests/interoperability` and list it in
   `docs/hardware-testing.md`.

## Style

- Edition 2024, `rustfmt.toml` (max width 100).
- Public items need Rustdoc with `# Errors` sections.
- Keep errors typed (`thiserror`).
- Prefer bounded `tokio::sync` channels and explicit limits.
- Record significant design decisions as ADRs in `docs/decisions/`.
