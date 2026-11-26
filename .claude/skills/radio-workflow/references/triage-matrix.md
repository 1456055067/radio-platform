# Triage Matrix: which roles run

When in doubt, include the reviewer. A skipped reviewer is a silent gap in
the evidence; an extra one costs only tokens.

## Always on for any code change

- `qa-engineer`
- `security-reviewer`
- `nist-800-53-reviewer`
- `compliance-scribe`

## Path-triggered roles

| Touched path or topic | Add |
|---|---|
| `crates/uniden-protocol`, `crates/uniden-sds` | `protocol-conformance-reviewer`, `performance-reviewer`; flag SI-10 and the no-retry rule |
| `crates/scanner-telemetry`, `crates/scanner-discovery`, `crates/scanner-controller` | `protocol-conformance-reviewer`; flag SC-7 (discovery, network policy) and leases |
| `crates/scanner-audio`, `services/media-gateway`, `crates/scanner-recorder` | `protocol-conformance-reviewer`, `performance-reviewer`; flag SC-5, SC-8 and the legal scope |
| `crates/scanner-transcription`, `services/api-gateway/src/transcription.rs` | `performance-reviewer`; flag SC-7 (outbound URL), SC-8 and privacy (`audio_listen`) |
| `crates/platform-auth` | `protocol-conformance-reviewer` (OIDC/JWT); flag IA-2, IA-5, AC-3, AC-6 and AC-12 |
| `crates/platform-common` (wire protocol, `NetworkPolicy`) | `protocol-conformance-reviewer`; `release-engineer` on a wire protocol change; flag SC-7 |
| `crates/platform-storage`, `migrations/` | `performance-reviewer` (queries, locks); `release-engineer` on a migration; flag AU-9 and SC-28 |
| `services/api-gateway/src/api/**`, `history.rs` | flag AC-3, AU-2 and SI-10; the authz matrix row is mandatory; `release-engineer` on an API change |
| `services/api-gateway/src/{serve,tls,crl,ui}.rs` | flag SC-8, SC-23 and IA-3; CSP and caching headers |
| `services/api-gateway/src/{metrics,retention}.rs`, audit code | `performance-reviewer`; flag AU-4, AU-5, AU-9 and AU-11 |
| `services/scanner-agent` | `performance-reviewer`, `protocol-conformance-reviewer`; flag SC-7 (local policy) and CP-10 (outage buffer, journal) |
| `frontend/**` | flag the console checks (tokens, CSP, role gating); QA adds a Playwright flow; `release-engineer` on a dependency change |
| `Cargo.toml`, `Cargo.lock`, `deny.toml`, `.cargo/audit.toml`, `frontend/package*.json` | `release-engineer` |
| `deployments/**`, `.dockerignore` | `release-engineer`; flag CM-6, CM-7 and SC-7 |
| `.github/workflows/**`, `.github/dependabot.yml` | `release-engineer` (pinning, permissions, secrets) |
| `tests/**`, `fuzz/**`, `frontend/e2e/**` only | `qa-engineer` only (plus `performance-reviewer` for benches) |
| `docs/protocol-compliance.md`, `tests/simulator/**` | `protocol-conformance-reviewer` (provenance) |

## Phase skips

| Situation | Skip |
|---|---|
| Docs or comments only | Phases 1–4; run the scribe and markdownlint only (still run `protocol-conformance-reviewer` for protocol docs) |
| A bug fix with an obvious one-file cause | Phase 1 may be a short inline plan by the orchestrator |
| An internal refactor with no change to behaviour or API | Phase 6 (still run the benches if a hot path is touched) |
| `review` mode | Phases 1–2 |

Record every skipped role or phase, with the reason, in `brief.md`.
