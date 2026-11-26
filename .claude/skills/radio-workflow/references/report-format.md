# Report Formats

## Findings file (`findings-<role>.md`)

```markdown
# Findings — <role>

Scope: <base>...<head> (<n> files), plan: <run>/plan.md
Verdict: APPROVE | APPROVE WITH COMMENTS | CHANGES REQUESTED

## <ROLE-PREFIX>-001 [BLOCKER] <one-line title>

- **Location**: `services/api-gateway/src/api/control.rs:123`
- **Reference**: NIST 800-53 AC-3 | protocol-compliance.md F12 | RFC 3550 §5.1 | CWE-...
- **Problem**: what is wrong, in one or two sentences.
- **Failure scenario**: concrete input or state → concrete bad outcome.
- **Fix**: the smallest change that resolves it.
- **Confidence**: high | medium | low
```

Role prefixes: `SEC` (security), `NIST` (800-53), `PROTO` (protocol),
`PERF` (performance), `QA` (testing), `REL` (release).

### Severity

| Severity | Meaning | Gate effect |
|---|---|---|
| BLOCKER | A panic or crash from scanner, agent or API input; tenant or site isolation bypass; an auth bypass; a state-changing command retried or sent without a lease; SSRF or an exposed scanner port; an audit bypass or lost attribution; a fabricated or upgraded protocol verification level; decryption, or recording or transcription on by default | Must fix |
| MAJOR | A missing required control or test (including an authz matrix row); unbounded state or DoS amplification; a fail-open path; a false NIST annotation or matrix status; a spec SHOULD ignored without a reason; a hot-path regression over 10%; a blocking call on a scanner or media task | Must fix |
| MINOR | Hardening, a missing annotation, a weak test, an unclear error, a small performance win | Advisory |
| NIT | Style, naming, comments | Advisory |

A finding without a location and a failure scenario is MINOR at most.

## Fix response (`fixes-round-<n>.md`)

```markdown
| Finding | Action | Change |
|---|---|---|
| SEC-001 | Fixed | `assembler.rs:120-131` bounds the part count before allocating |
| PROTO-002 | Rebutted | protocol-compliance.md F7 documents the Foot variant (C3) |
```

## Gate summary (`gate.md`)

```markdown
# Gate — <task>

Verdict: PASS | PASS WITH POA&M | FAIL

| Gate | Result | Evidence |
|---|---|---|
| fmt | PASS | `cargo fmt --all -- --check` |
| clippy | PASS | `cargo clippy --workspace --all-targets -- -D warnings` |
| tests | PASS (184) | `cargo test --workspace` |
| PostgreSQL | NOT RUN | no DATABASE_URL |
| browser | PASS (16) | `npm run e2e` |
| benches | PASS (gsi decode −1%) | `cargo bench -p radio-benches` |
| hardware | NOT RUN | no SDS200 |
| security review | APPROVE | findings-security.md |
| NIST 800-53 review | APPROVE WITH COMMENTS | findings-nist.md (2 MINOR) |
| protocol review | APPROVE | findings-proto.md |
| performance review | APPROVE | findings-perf.md |
| compliance docs | UPDATED | docs-update.md |
| release | CHANGELOG | release-notes.md |

## Open items
## POA&M
## Needs SDS200 hardware
## Dropped findings
```
