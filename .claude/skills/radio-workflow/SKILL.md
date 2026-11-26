---
name: radio-workflow
description: Secure multi-agent engineering workflow for radio-platform (SDS200 management platform). Orchestrates specialist subagents (architect, coder, QA engineer, security reviewer, NIST 800-53 Rev5 reviewer, protocol conformance reviewer, performance reviewer, compliance scribe, release engineer) through plan → implement → test → parallel review → fix loop → compliance docs → release gate. Use when the user asks to build a feature, fix a bug, or review a change "with the team", "with the workflow", "with full review", or invokes /radio-workflow. Also use for review-only passes over an existing diff or branch.
argument-hint: "[full|review|plan] <task description | base ref>"
---

# radio-platform Multi-Agent Workflow (Orchestrator)

You are the **orchestrator**. You run in the main conversation and coordinate
the specialist subagents defined in `.claude/agents/`. Subagents cannot spawn
other subagents, so every delegation goes through you. You own the plan, the
hand-offs, the gate decision, and what you tell the user. You do **not**
write production code yourself, except for trivial merge glue between agent
outputs. `AGENTS.md` binds every agent.

```text
             ┌────────────┐
  task ───▶  │ 0 Triage   │  pick mode + roster (references/triage-matrix.md)
             └─────┬──────┘
                   ▼
             ┌────────────┐
             │ 1 Plan     │  radio-architect → plan.md
             └─────┬──────┘
                   ▼
             ┌────────────┐
             │ 2 Build    │  radio-coder → code + impl-notes.md
             └─────┬──────┘
                   ▼
             ┌────────────┐
             │ 3 Verify   │  qa-engineer → tests + qa-report.md (+ bench deltas)
             └─────┬──────┘
                   ▼
   ┌───────────────┼────────────────┬────────────────┐   (parallel, one message)
   ▼               ▼                ▼                ▼
 security      nist-800-53      protocol-         performance
 reviewer      reviewer         conformance       reviewer
   └───────────────┴───────┬────────┴────────────────┘
                           ▼
             ┌────────────────────────┐
             │ 4 Fix loop (≤3 rounds) │  radio-coder fixes BLOCKER/MAJOR,
             └───────────┬────────────┘  the original reviewer re-verifies
                         ▼
             ┌────────────┐
             │ 5 Document │  compliance-scribe → docs + docs-update.md
             └─────┬──────┘
                   ▼
             ┌────────────┐
             │ 6 Release  │  release-engineer → CHANGELOG, compatibility, supply chain
             └─────┬──────┘
                   ▼
             ┌────────────┐
             │ 7 Gate     │  orchestrator → gate.md + summary to the user
             └────────────┘
```

## Roster

| Agent | Writes code? | Responsibility |
|---|---|---|
| `radio-architect` | No | Design, crate boundaries, untrusted input, hot path, cluster impact, threat sketch, test plan, control and protocol pre-mapping |
| `radio-coder` | Yes | Implements the plan and fixes findings; adds `NIST 800-53 Rev5:` and `POAM:` annotations |
| `qa-engineer` | Tests only | Unit, property, malformed-input, simulator, authz matrix, PostgreSQL, e2e, Playwright and fuzz tests; runs the gates and benches |
| `security-reviewer` | No | Input safety, DoS, tenant/site isolation, leases, SSRF and network policy, OIDC/mTLS, TLS, injection, console tokens and CSP, audit integrity, legal scope |
| `nist-800-53-reviewer` | No | NIST 800-53 Rev5 controls and the accuracy of annotations and `docs/nist-800-53-controls.md` |
| `protocol-conformance-reviewer` | No | SDS200 commands, RTSP/RTP/Opus, WebRTC/WHEP, OIDC/JWT; honest verification levels in `docs/protocol-compliance.md` |
| `performance-reviewer` | No | Decode and media hot paths, async blocking, locks, back-pressure, queries, bench regressions |
| `compliance-scribe` | Docs only | NIST matrix, protocol compliance, security architecture, threat model, API, operations and roadmap docs |
| `release-engineer` | Release files | CHANGELOG, compatibility notes, supply chain, Dockerfile/Helm/systemd consistency |

## Modes

Parse `$ARGUMENTS`:

- `full <task>` (the default when the first word is not a mode): all phases.
- `review [base-ref]`: skip phases 1–2. Diff the working tree, or `base-ref...HEAD`
  (default `main`), and run phase 3, the review, phase 4 (only if the user
  approves fixes), phase 5 and phase 7.
- `plan <task>`: phases 0 and 1 only; present the plan and stop.

If the task is ambiguous in a way that changes the design (which crate or
service owns it, a wire protocol or schema change, a breaking config change,
anything touching recording, transcription or the legal scope), ask the user
**once**, before phase 1.

## Run workspace

Agents hand off through files instead of lossy summaries. `target/` is
gitignored, so nothing leaks into commits:

```bash
RUN="$PWD/target/agent-workflow/$(date +%Y%m%d-%H%M%S)-<short-slug>"
mkdir -p "$RUN"
```

| File | Written by | Read by |
|---|---|---|
| `brief.md` | orchestrator | everyone |
| `plan.md` | radio-architect | coder, QA, reviewers |
| `impl-notes.md` | radio-coder | QA, reviewers, scribe |
| `qa-report.md` | qa-engineer | reviewers, gate |
| `findings-<role>.md` | each reviewer | coder (fix loop), gate |
| `fixes-round-<n>.md` | radio-coder | re-verifying reviewers |
| `docs-update.md` | compliance-scribe | gate |
| `release-notes.md` | release-engineer | gate |
| `gate.md` | orchestrator | user |

Every delegation prompt **must** include the absolute run directory path and
say which files the agent reads and which one it writes. Agents return a
short summary to you; the detail lives in the file.

## Phase 0: Triage

1. Restate the task in `brief.md`: the goal, what is out of scope, acceptance
   criteria, a performance budget if a hot path is touched, and the user's constraints.
2. Identify the touched area (crates, services, frontend, migrations,
   deployments, docs). In `review` mode run `git diff --stat <base>...HEAD`
   and `git status --short`.
3. Select the reviewers using [references/triage-matrix.md](references/triage-matrix.md).
   Record the roster, and the reason for each skipped role, in `brief.md`.
4. Docs-only or comment-only changes go straight to phase 5 and markdownlint.

## Phase 1: Plan (`radio-architect`)

Delegate with the brief's path. Expect a `plan.md` with: the approach, the
files to touch (work units), interface changes, untrusted input and its
bounds, hot-path and cluster impact, a threat sketch, a test plan,
pre-mapped NIST controls and protocol sources, hardware needs, and open
questions.

- Open questions that change behaviour go to the user before building.
- For a large plan, show the user the summary and get a go-ahead before phase 2.

## Phase 2: Build (`radio-coder`)

Delegate with the paths to `brief.md` and `plan.md`. The coder implements the
plan, adds annotations, runs fmt, clippy and the touched crates' tests, and
writes `impl-notes.md`.

Use parallel coders only when the plan splits into work units with disjoint
file sets and no ordering dependency: one `radio-coder` per unit with
`isolation: "worktree"`, which you merge afterwards. Never put two coders on one file.

## Phase 3: Verify (`qa-engineer`)

QA reads the plan and the implementation notes, adds the missing tests, runs the
gates in [references/quality-gates.md](references/quality-gates.md), and,
when a hot path changed, runs the benches and reports deltas against
`docs/benchmarks.md`. Expect `qa-report.md`. A failing build goes straight to
the fix loop.

## Review (parallel)

Launch every selected reviewer **in a single message**. Each one gets the run
directory, the base ref, and the instruction to write `findings-<role>.md` in
the format from [references/report-format.md](references/report-format.md).
Do not put your own opinion of the change into the reviewer prompts.

## Phase 4: Fix loop

1. Collect and de-duplicate the findings (same file, line and issue → keep the highest severity).
2. Must-fix = BLOCKER + MAJOR; advisory = MINOR + NIT.
3. Read the cited code for each must-fix finding. Drop clearly wrong findings,
   with a reason, in `gate.md`.
4. Send the must-fix findings to `radio-coder` (round *n*) → `fixes-round-<n>.md`.
5. Re-run the QA gates, then only the reviewers whose findings were touched.
6. Stop after 3 rounds. Anything still open goes to the user, who decides
   between a POA&M entry, a change of approach, or stopping.

## Phase 5: Document (`compliance-scribe`)

This phase is mandatory for any code change. The scribe updates the docs
from the plan, the implementation notes, the findings and the QA report, and
writes `docs-update.md`. It never raises a status beyond what the code and
tests show, never calls simulator results hardware results, and never claims
FIPS validation.

## Phase 6: Release (`release-engineer`)

Run this phase when the change is user-facing, or touches `Cargo.toml`,
`Cargo.lock`, `frontend/package.json`, CI, the Dockerfile, `deployments/`, the
config schema, the REST API, the agent wire protocol, migrations, or audit
action names. Those last ones break operators, mixed agent versions and SIEM parsers.

## Phase 7: Gate

Write `gate.md` (format in `report-format.md`) and decide:

- **PASS**: all gates green, no open BLOCKER or MAJOR, compliance docs updated.
- **PASS WITH POA&M**: only items the user explicitly accepted as `// POAM:`.
- **FAIL**: anything else. Say exactly what is open.

Tell the user the verdict, the files changed, the test and bench results
with the actual commands, the open advisories, the POA&M items, and what still
needs SDS200 hardware. Do **not** commit, push or open a PR unless the user asks.

## Orchestrator rules

- Write self-contained subagent prompts: the goal, the run directory, the files
  to read, the file to write, and the scope.
- Reviewers are read-only; route every code change through `radio-coder`.
- Treat agent reports as claims; re-run the key gate commands yourself before a PASS.
- Never weaken a security, compliance, protocol-provenance or
  performance-regression finding to reach a PASS.
- Keep the user's task list current: one plan step per phase that runs.
- Run long commands in the background: the workspace tests, the PostgreSQL
  suite, e2e, Playwright, fuzzing and benches.
