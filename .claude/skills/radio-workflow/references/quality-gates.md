# Quality Gates

The commands mirror `.github/workflows/ci.yml`. Scope them to the touched crates
(`-p <crate>`) while iterating, and run the workspace-wide form once before the final gate.

| Gate | Command | Required |
|---|---|---|
| Format | `cargo fmt --all -- --check` | Yes |
| Lint | `cargo clippy --workspace --all-targets -- -D warnings` | Yes |
| Tests | `cargo test --workspace` | Yes |
| Rustdoc | `RUSTDOCFLAGS=-D warnings cargo doc --workspace --no-deps` | Yes for public API changes |
| Example configs | `cargo run -q -p api-gateway -- --check-config deployments/examples/gateway.example.toml` and the same for `scanner-agent` | When config changed |
| Authz matrix | `cargo test -p api-gateway --test authz_matrix` | Yes when routes changed (the test fails for an unlisted route) |
| PostgreSQL | `DATABASE_URL=postgres://… cargo test -p platform-storage -- --include-ignored` | When storage or migrations changed; NOT RUN without a database |
| e2e | `cargo test -p scanner-agent --test e2e` | When the agent, the gateway or the wire protocol changed |
| Frontend | `cd frontend && npm audit && npm run typecheck && npm test && npm run build` | When `frontend/` changed |
| Browser | `cd frontend && npm run build && npm run e2e` | When the console or a UI-facing API changed |
| Fuzz | `cd fuzz && cargo +nightly fuzz run <target> -- -max_total_time=60` | When a parser changed |
| Benches | `cargo bench -p radio-benches` (compare with `docs/benchmarks.md`) | When a hot path changed |
| Advisories | `cargo audit` | Yes |
| Supply chain | `cargo deny check` | When `Cargo.toml` or `Cargo.lock` changed |
| Helm | `helm lint deployments/helm/radio-platform` plus the render check in `ci.yml` | When `deployments/` changed |
| Workflows | `actionlint` | When `.github/workflows/` changed |
| Annotations | `grep -rc "NIST 800-53 Rev5" crates services \| awk -F: '{s+=$2} END {print s}'` (the count must not drop) | Yes for code changes |
| Hardware | `SDS200_ADDR=… cargo test -p sds-interoperability -- --ignored` | NOT RUN without an SDS200; never reported as PASS |

## Performance budget

- No heap allocation per RTP packet or per decoded status frame beyond what decoding needs.
- No blocking I/O, and no `std::sync` lock held across `.await`, on scanner, media or live fan-out tasks.
- Audit, storage, transcription and cluster-bus hand-offs never stall scanner
  polling or audio. Overflow is counted.
- A regression of more than 10% on an existing bench is a MAJOR finding unless it is justified.

## Rules

- Record every command in `qa-report.md` with its exit status and the last
  relevant lines of output. "Tests pass" without the command is not evidence.
- A gate whose environment is missing (no PostgreSQL, Podman/Docker, helm, nightly,
  hardware) is **NOT RUN**, never PASS.
- No new `#[allow(...)]`, `#[ignore]`, `unwrap()` or `expect()` in production
  code without a justification comment and a finding entry. `unsafe` is forbidden.
