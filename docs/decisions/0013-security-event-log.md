# ADR 0013: Security event log for authentication failures and rate limits

- **Status:** Accepted
- **Date:** 2026-11-27

## Context

NIST AU-2 (POA&M item 4) required that failed authentication and
rate-limit refusals be recorded. Before this change, 401s (missing,
malformed, expired, forged or mis-claimed tokens) and 429s on the public
API, and refusals on the agent listener, appeared only in metrics, and
per-IP 429s did not appear even there.

The existing audit log (ADR 0011) is a per-tenant hash chain
(`audit_log.tenant_id NOT NULL`). A rejected request usually has **no
verified tenant**:

- the tenant claimed by an unverified token cannot be trusted, and writing
  into it would let an attacker fill a victim tenant's audit log;
- unauthenticated traffic must not drive database writes, or a flood of
  bad tokens becomes a database load and storage attack.

Rejections can be produced at line rate by anyone who can reach the
listener, so the record must be bounded in volume, memory and CPU, and
must never echo attacker-controlled data (the token, its claims, headers,
the raw path or the query string).

## Decision

1. **A separate security log, not the audit chain.** Rejections are
   structured `tracing` events on the dedicated target `security`
   (`services/api-gateway/src/security_log.rs` → `TARGET`), written as
   flattened JSON lines to stdout. No schema change and no database write.
   The audit chain keeps recording authenticated operator actions and
   denials as before.
2. **What is recorded.** Seven call sites: missing token, token
   verification errors and the per-subject 429 (`state.rs` →
   `Auth::from_request_parts`), the per-IP 429 (`state.rs` →
   `ip_rate_limit`), expiry at authorization (`state.rs` →
   `AppState::authorize`), and on the agent listener a certificate
   without a usable identity and an unregistered agent (`agent_ws.rs` →
   `agent_socket`). `AuthError::Provider` and `AuthError::Config` are
   identity-provider or configuration failures, not client failures, and
   are not security events.
3. **What an event contains.** Only enumerated values (`event`, `reason`,
   `claim` name, `listener`, `method` or `OTHER`), the HTTP `status`, the
   parsed source address (`src_ip`, from `state::client_ip`, so
   `X-Forwarded-For` only from trusted proxies; the TCP peer on the agent
   listener), the route **template**, and identities only when verified
   (`subject`/`tenant` of a verified principal, `agent` from a CA-verified
   certificate). `SecurityEvent` has no field that could hold the token,
   unverified claims, headers, the raw path or the query, and the layer
   writes no span context (the HTTP request span carries the raw URI).
4. **Always on.** `security_log::subscriber`, called from `main.rs` →
   `init_tracing`, composes two layers: the general log, filtered by
   `RUST_LOG` and excluding exactly the `security` target, and the
   security layer, enabled for exactly the `security` target at `WARN`
   and above whatever `RUST_LOG` says. The general filter keeps the
   `EnvFilter` level hint, so disabled `debug!`/`trace!` callsites keep
   their fast path. Both layers use an explicit UTC RFC 3339 timer.
5. **Throttled per source.** Constants in code (`SecurityLogLimits`
   `Default`), not configuration:

   | Constant | Value | Meaning |
   |---|---|---|
   | `WINDOW` | 60 s | Fixed window per source and for the budgets |
   | `PER_SOURCE` | 10 | Individual lines per source per window |
   | `GLOBAL` | 300 | Lines per replica per window for events without a verified identity, plus inline summaries |
   | `VERIFIED_GLOBAL` | 60 | Separate budget per replica per window for events with a verified `subject` or `agent` |
   | `SHARDS` × `SHARD_CAPACITY` | 16 × 256 | Fixed source table (4096 sources), pre-sized, never grown |
   | `SUMMARY_CAP` | 128 | `scope=source` summaries per flush; the rest roll into one `scope=global` summary |

   - A source is an IPv4 address, an IPv6 /64 (after canonicalising
     IPv4-mapped addresses), or `unknown`. The shard is picked by a
     per-process random hash, so a client cannot aim at one shard.
   - Events past a limit are counted, not written, and reported as one
     `event=suppressed` summary per source: inline when the source sends
     again after its window, or from `SecurityLog::flush` on the
     gateway's 30 s maintenance tick when it went quiet.
   - When the table is full of active sources, new sources are
     untracked: their events still draw on the budgets and are counted,
     and their suppressed count appears in the `scope=global` summary.
     A full shard skips the eviction scan until a slot can expire.
   - Shard locks are short, never held across `.await` or while writing.
     The budgets are lock-free atomics (approximate at the window edge).
     The success path does no security-log work.
   - Every rejection is counted before throttling in
     `radio_security_events_total{reason}`; suppressed lines in
     `radio_security_log_suppressed_total`.

## Consequences

- Security events are only as durable as the deployer's log pipeline:
  they are not in the hash chain, not covered by `/audit/verify`, and the
  platform keeps no copy (AU-9). Operators must collect stdout into a SIEM
  (`docs/operations.md`, "Security event log (SIEM)").
- The event schema is an operator-facing interface. Field names and
  enumerated values must stay stable; changes need a CHANGELOG entry.
- The throttle and its budgets are **per replica**: with R replicas one
  source can produce up to R × 10 lines per window. Aggregate across hosts
  in the SIEM.
- A flood can hide individual lines (log blinding) but not counts: the
  per-source limit, the shared budget and the 4096-source table bound
  what is written, while metrics and summaries keep every count.
- Residuals (POA&M item 4 in `docs/nist-800-53-controls.md`):
  - verified-identity events share their source's per-source slot, so
    unauthenticated failures from the same address (one NAT) can suppress
    them (untested);
  - one verified principal can use up the 60-line verified budget for a
    window (SEC-005), hiding other principals' verified lines (still
    counted and summarised); a per-subject cap is a possible follow-up;
  - the per-IP rate limiter itself is keyed by the full IPv6 address;
  - rustls handshake and CRL failures are not logged by the application;
  - a storage failure during the agent lookup is answered 403 and logged
    as `storage failure`, not as a security event (untested).
- The verified `subject` in `rate_limited` and `expired` events is
  personal data; the log's retention and access are the deployer's to
  set.
- Not chosen: writing into the claimed tenant's audit chain (lets an
  attacker fill a victim's log); a platform-wide audit chain (database
  writes driven by unauthenticated traffic); throttle constants in TOML
  (no operator need yet; can be added later without a schema change).
