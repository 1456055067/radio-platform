# Operations

## Health and signals

| Signal | Where |
|---|---|
| Liveness | `GET /healthz` |
| Readiness | `GET /readyz` (database ping) |
| Metrics | `GET /metrics` on the `[metrics]` listener (Prometheus text format; bind it internally) |
| Agent connections | `radio_agents_connected`; `scanners[].agent_connected`; logs `agent connected` / `agent disconnected` |
| Scanner health | `scanners[].health`, `health_changed` events |
| Outage buffering | Agent logs; `pending_events`, `dropped_events` in `AgentStatus` |
| Operator actions | `GET /api/v1/audit` (filters: actor, action prefix, outcome, resource, time) |
| Audit integrity | `GET /api/v1/audit/verify`; hourly `audit chain head` log lines |
| Authentication failures and rate limits | `security` log stream (see "Security event log (SIEM)"); `radio_security_events_total{reason}` |
| Database transport | Start-up log `database connection uses verified TLS`, `database is on this host`, or the `WARN` `database connection is not verified TLS (insecure_transport = true)…` (fields `host`, `port`, `sslmode`; never the URL) |

Both services log JSON to stdout; set `RUST_LOG` (for example
`info,api_gateway=debug`). The gateway also writes the security event log
to stdout, whatever `RUST_LOG` says (see "Security event log (SIEM)").
Gateway log timestamps (general and security) come from an explicit UTC
RFC 3339 timer (`2026-11-27T08:41:02.513274Z`; the number of fractional
digits can vary).

Do **not** run production gateways at `RUST_LOG=debug` (or `trace`): the
general log then includes the HTTP request span, which carries the raw
request URI **with its query string** (identifiers and any value a client
put there). The security log never writes span context.

## Metrics

| Metric | Type | Notes |
|---|---|---|
| `radio_http_requests_total{method,route,status}` | counter | `route` is the template (`/api/v1/scanners/{id}`), never an id |
| `radio_http_request_duration_seconds{method,route}` | histogram | 5 ms – 15 s buckets |
| `radio_agents_connected` | gauge | per replica |
| `radio_media_streams`, `radio_media_listeners`, `radio_media_webrtc_sessions` | gauge | |
| `radio_recordings_stored_total`, `radio_recordings_failed_total`, `radio_recordings_in_progress` | counter / gauge | when recording is configured |
| `radio_transcriptions_{completed,retried,failed,skipped}_total`, `radio_transcriptions_in_flight` | counter / gauge | when transcription is configured |
| `radio_retention_last_sweep_timestamp_seconds`, `radio_retention_last_sweep_removed{kind}`, `radio_retention_last_sweep_errors` | gauge | when retention is configured |
| `radio_build_info{version}`, `radio_uptime_seconds` | gauge | |
| `radio_security_events_total{reason}` | counter | Authentication failures and rate-limit refusals, counted **before** log throttling. All nine `reason` values are always rendered. Includes per-IP 429s, which `radio_http_requests_total` does not see |
| `radio_security_log_suppressed_total` | counter | Security events not written as individual lines because of throttling (reported in `suppressed` summaries) |
| `radio_security_log_tracked_sources` | gauge | Sources in this replica's throttle table (at most 4096) |

Suggested alerts (PromQL):

| Alert | Expression |
|---|---|
| Gateway errors | `sum(rate(radio_http_requests_total{status=~"5.."}[5m])) / sum(rate(radio_http_requests_total[5m])) > 0.02` |
| Slow API | `histogram_quantile(0.95, sum by (le) (rate(radio_http_request_duration_seconds_bucket{route!="/api/v1/events/stream"}[5m]))) > 1` |
| Agents lost | `sum(radio_agents_connected) < <expected agents>` for 5 m |
| Many denials (probing) | `sum(rate(radio_http_requests_total{status="403"}[10m])) > 1` |
| Recording failures | `increase(radio_recordings_failed_total[1h]) > 0` |
| Transcription giving up | `increase(radio_transcriptions_failed_total[1h]) > 5` |
| Retention stalled | `time() - radio_retention_last_sweep_timestamp_seconds > 3 * <sweep interval>` |

Also alert on the log line `audit chain verification failed` and run
`/audit/verify` on a schedule (for example daily, from a job holding an
`audit_view` token).

## Security event log (SIEM)

The gateway records authentication failures and rate-limit refusals as
structured JSON lines on the `tracing` target `security` (ADR 0013). They
go to stdout beside the general log; select them with `target ==
"security"`. They are **not** in the tenant audit log, are not covered by
`/audit/verify`, and the platform keeps no copy: collect them into your
SIEM and set retention and integrity protection there.

**Always on.** The security log is written at `WARN` whatever `RUST_LOG`
says; `RUST_LOG=off` or `error` does not silence it. In particular
`RUST_LOG=info,security=off` does **not** silence the security log, but
because `RUST_LOG` directives match target **prefixes** it does silence
general-log targets whose names start with `security` (none exist today).
The general log never carries `target=security` lines.

### Schema

Every line is one JSON object with these fields (key order is not part of
the schema; a field marked "if known" or "only …" is **absent**, not
`null`, when it does not apply):

| Field | Type | Present | Value |
|---|---|---|---|
| `timestamp` | string | always | RFC 3339, UTC (`Z`) |
| `level` | string | always | `WARN` |
| `target` | string | always | `security` |
| `message` | string | always | `authentication failed`, `request rate limited` or `security events suppressed` (by `event`) |
| `event` | string | always | `auth_failure`, `rate_limited` or `suppressed` |

`event=auth_failure` and `event=rate_limited`:

| Field | Type | Present | Value |
|---|---|---|---|
| `reason` | string | always | See the reason table below |
| `claim` | string | only with `reason=claim` | The claim that failed: `iss`, `aud`, `sub`, `exp`, `iat`, or the configured tenant or sites claim name (defaults `tenant_id`, `sites`). Never a claim value |
| `status` | integer | always | HTTP status returned: 401, 403 or 429 |
| `listener` | string | always | `public` (REST/WebSocket API) or `agent` (mTLS agent listener) |
| `src_ip` | string | if known | Client address: on the public listener the same address the per-IP limiter uses (`X-Forwarded-For` only from `limits.trusted_proxies`, right-most untrusted hop); on the agent listener the TCP peer. The full address, also for IPv6 |
| `method` | string | always | `GET`, `POST`, `PUT`, `PATCH`, `DELETE`, `HEAD`, `OPTIONS`, `CONNECT`, `TRACE`, or `OTHER` for extension methods |
| `route` | string | always | Route template (`/api/v1/scanners/{id}/status`), `unmatched` for unknown paths, or `/agent/v1` on the agent listener. Never the raw path or query |
| `subject` | string | only for a **verified** principal | `rate_limited_principal`, and `expired` raised at authorization |
| `tenant` | string (UUID) | with `subject` | The verified principal's tenant |
| `agent` | string | only with `reason=agent_unregistered` | Agent id from the CA-verified client certificate (`auth_failure` only) |

Reasons:

| `event` | `reason` | `status` | When |
|---|---|---|---|
| `auth_failure` | `missing` | 401 | No `Authorization: Bearer` token (also other schemes, a lowercase `bearer`, or an unreadable header) |
| `auth_failure` | `malformed` | 401 | The token cannot be parsed |
| `auth_failure` | `invalid` | 401 | Verification failed: signature, algorithm, key, issuer or audience |
| `auth_failure` | `expired` | 401 | Token expired at verification (no identity), or the session expired at authorization (with the verified `subject`/`tenant`) |
| `auth_failure` | `claim` | 401 | A required claim is missing or invalid (see `claim`) |
| `auth_failure` | `agent_identity` | 401 | Agent listener: CA-verified client certificate without a usable agent identity |
| `auth_failure` | `agent_unregistered` | 403 | Agent listener: the certificate's agent is not registered (`agent` set) |
| `rate_limited` | `rate_limited_ip` | 429 | Per-IP limit, before authentication (no identity, even with a valid token) |
| `rate_limited` | `rate_limited_principal` | 429 | Per-subject limit, after authentication (`subject`/`tenant` set) |

Never logged: the token or any part of it, claims of an unverified token
(an unverified `sub`, `iss` or claimed tenant), headers, cookies, the raw
path and the query string. Identity-provider outages (for example the JWKS
endpoint failing, answered 503) and verifier configuration errors are
**not** security events. Successful requests write nothing.

`event=suppressed` (throttling summaries):

| Field | Type | Present | Value |
|---|---|---|---|
| `scope` | string | always | `source` (one source) or `global` (sources over the per-flush cap plus untracked sources) |
| `source` | string | only with `scope=source` | IPv4 address, IPv6 `/64` prefix (`2001:db8:1:2::/64`), or `unknown` |
| `suppressed` | integer | always | Events not written individually. A source's count can cover more than one window when the budget was spent at a window roll |
| `window_secs` | integer | always | `60` |

Summaries carry no request fields (`reason`, `src_ip`, `route` and the
identity fields are absent).

Example lines (values illustrative):

```json
{"timestamp":"2026-11-27T08:41:02.513274Z","level":"WARN","message":"authentication failed","event":"auth_failure","reason":"invalid","status":401,"listener":"public","src_ip":"203.0.113.7","method":"GET","route":"/api/v1/scanners/{id}/status","target":"security"}
{"timestamp":"2026-11-27T08:41:03.004911Z","level":"WARN","message":"authentication failed","event":"auth_failure","reason":"claim","claim":"tenant_id","status":401,"listener":"public","src_ip":"203.0.113.7","method":"GET","route":"/api/v1/me","target":"security"}
{"timestamp":"2026-11-27T08:41:05.771002Z","level":"WARN","message":"request rate limited","event":"rate_limited","reason":"rate_limited_principal","status":429,"listener":"public","src_ip":"198.51.100.20","method":"GET","route":"/api/v1/scanners","subject":"3f6c2a9e-user","tenant":"0b8e7c1d-5a4f-4e2b-9c3d-1a2b3c4d5e6f","target":"security"}
{"timestamp":"2026-11-27T08:41:09.120334Z","level":"WARN","message":"authentication failed","event":"auth_failure","reason":"agent_unregistered","status":403,"listener":"agent","src_ip":"192.0.2.44","method":"GET","route":"/agent/v1","agent":"site-7-agent","target":"security"}
{"timestamp":"2026-11-27T08:42:30.000187Z","level":"WARN","message":"security events suppressed","event":"suppressed","scope":"source","source":"203.0.113.7","suppressed":5,"window_secs":60,"target":"security"}
{"timestamp":"2026-11-27T08:42:30.000402Z","level":"WARN","message":"security events suppressed","event":"suppressed","scope":"global","suppressed":20,"window_secs":60,"target":"security"}
```

### Throttling

Per replica, per fixed 60 s window:

- at most **10** individual lines per source (an IPv4 address or an IPv6
  /64; IPv4-mapped IPv6 counts as IPv4);
- at most **300** lines in total for events without a verified identity
  and for inline summaries, and, separately, at most **60** lines for
  events with a verified `subject` or `agent`, so an unauthenticated flood
  cannot use up the verified budget;
- at most **4096** tracked sources. Beyond that, new sources are
  untracked: their events still use the budgets, and their suppressed
  count goes into the `scope=global` summary.

Events over a limit are counted and summarised (`event=suppressed`),
never silently dropped: inline when the source sends again after its
window, or from the 30 s maintenance tick once its window has ended, so a
summary can arrive up to about 90 s after the first suppressed event. At
most 128 `scope=source` summaries are written per tick; the rest are added
to one `scope=global` summary. `radio_security_events_total` counts every
rejection whether or not a line was written.

Limits of the throttle (POA&M item 4 in
`docs/nist-800-53-controls.md`):

- **Per replica.** With R replicas one source can produce up to R × 10
  lines per window, and each replica has its own budgets and table.
  Aggregate by `src_ip` / `source` across all gateway hosts in the SIEM,
  and sum `radio_security_events_total` across replicas.
- **Log blinding.** A flood from many sources can spend the 300-line
  budget, so individual lines for other sources are not written in that
  window. Counts survive in summaries and metrics; alert on
  `radio_security_log_suppressed_total`.
- **Shared addresses.** Verified-identity events share their source's
  10-line slot, so unauthenticated failures from the same address (for
  example many users behind one NAT or proxy) can suppress them.
- **One principal can spend the verified budget** (SEC-005). A valid-token
  holder that keeps hitting its own per-subject limit from several
  addresses can use up the 60 verified lines of a window; other
  principals' verified lines in that window are then only counted and
  summarised.

### Personal data

`subject` (and `tenant`) are the verified principal's identifiers from the
IdP and are **personal data**; `src_ip` can be too. Restrict access to the
security log and set a retention period that fits your privacy obligations.

### Suggested alerts and queries

These are suggestions, not shipped rules; tune thresholds to your traffic.

| Alert | Expression (PromQL) |
|---|---|
| Authentication failures rising | `sum(rate(radio_security_events_total{reason=~"missing\|malformed\|invalid\|expired\|claim"}[5m])) > 1` |
| Forged or foreign tokens | `sum(increase(radio_security_events_total{reason="invalid"}[10m])) > 20` |
| Agent refusals | `sum(increase(radio_security_events_total{reason=~"agent_identity\|agent_unregistered"}[15m])) > 0` |
| Per-IP limit hit | `sum(rate(radio_security_events_total{reason="rate_limited_ip"}[5m])) > 1` |
| Security log throttling (possible flood or log blinding) | `sum(increase(radio_security_log_suppressed_total[10m])) > 0` |
| Throttle table near full | `max(radio_security_log_tracked_sources) > 3500` |

In the SIEM (suggestions): count `event=auth_failure` by `src_ip` and
`reason` over all gateway hosts; add the `suppressed` values of
`event=suppressed` lines to those counts; alert on `agent_unregistered`
for an `agent` you know; correlate `rate_limited_principal` by `subject`.

## Deployment shapes

- **Gateway:** Helm chart `deployments/helm/radio-platform` (TLS
  terminated by the gateway, agent mTLS on an L4 load balancer, restricted
  pod security, NetworkPolicy, PodDisruptionBudget). Images are built by
  `deployments/container/Dockerfile` (distroless, non-root) for amd64 and
  arm64, with SBOM and provenance, and signed with cosign
  (`.github/workflows/images.yml`).
- **Agents:** `deployments/systemd/radio-agent.service` (sandboxed,
  dynamic user, state in `/var/lib/radio-agent`) or the `agent` image.
- Validate configuration before rollout: `api-gateway --check-config
  gateway.toml`, `scanner-agent --check-config agent.toml`.

## Shutdown and upgrades

- Both binaries stop cleanly on SIGTERM or SIGINT. The agent's event
  journal is written durably as events happen (when `state_dir` is set);
  buffered events are replayed after reconnecting and de-duplicated by the
  gateway.
- **Gateway upgrades** are rolling: migrations run at start-up and are
  forward-only. Keep them additive (new nullable or defaulted columns, new
  tables) so the previous release keeps working during a rollout;
  0001–0005 follow this. Records an older replica writes to the audit log
  during a rollout are unchained, and `/audit/verify` counts them. Agents reconnect
  to another replica. Leases live in PostgreSQL and survive. In-flight
  commands on a stopping replica fail and are **not** retried; operators
  retry deliberately.
- **Agent upgrades**: gateways accept agent protocols 1–3; upgrade
  gateways first, then agents.
- **Database transport (ADR 0012):** from this release a gateway whose
  database is not on loopback refuses to start without
  `sslmode=verify-full`. Run `api-gateway --check-config` with the
  production `DATABASE_URL` before rolling out, and see
  `docs/deployment.md`, "Database TLS", for in-cluster PostgreSQL without
  TLS.
- Downgrades across a migration are not supported; restore from backup.

## Backup and restore

| Data | Backup | Notes |
|---|---|---|
| PostgreSQL | `pg_dump -Fc` daily plus WAL archiving (or your provider's PITR) | Holds everything except recordings. Leases and the cluster bus are transient. |
| Recordings | Bucket versioning and replication (S3) or filesystem snapshots (`local`) | Retention deletes objects; versioning keeps deleted ones for your bucket's lifecycle period. |
| Audit anchors | Keep the hourly `audit chain head` log lines in your log archive | Lets you detect a rewritten chain after a restore or compromise. |
| Configuration and PKI | Your configuration management; the agent CA key **offline** | |

Restore drill (do it before you need it):
1. Restore PostgreSQL to a new instance; point a single gateway at it.
2. `GET /api/v1/audit/verify` must report `ok`; compare `head_hash` with
   the last archived anchor before the backup time.
3. Spot-check that `audio_object` keys of recent transmissions exist in
   the bucket.
4. Scale back out.

## Incident procedures

- **Compromised agent:** revoke its certificate in the agent CA, publish
  the CRL (`agents.client_crl`, re-read every `crl_refresh_secs`; every
  connected agent is made to reconnect and re-verify), and disable its
  scanners (`PATCH /scanners/{id}` `enabled=false`).
- **Compromised operator account:** disable it at the IdP. Tokens expire
  within their lifetime (`max_session_secs` caps sessions). Release their
  leases (`DELETE /scanners/{id}/leases/{lease}` as `scanner_admin`).
  Export their audit trail: `GET /audit/export?actor=<subject>`.
- **Suspected audit tampering:** run `/audit/verify`, export the log, and
  compare the head with the archived anchors.
- **Audio lock-up on a scanner:** disable audio for that scanner, power-cycle
  it, and raise `audio.cooldown_secs` on the agent.

## Troubleshooting the database connection

| Symptom | Meaning and action |
|---|---|
| Start-up fails (exit 1, log `gateway failed`) or `--check-config` exits 2 with `database <host>:<port> is not on loopback and sslmode=<mode>; off-loopback PostgreSQL requires sslmode=verify-full (set [database] insecure_transport = true to accept an unverified connection)` | The transport policy refused the URL. Add `sslmode=verify-full` (and `sslrootcert` for a private CA), or opt out deliberately (`docs/deployment.md`, "Database TLS"). `host` may read `<name> (hostaddr <ip>)` when `hostaddr` decided the destination |
| `database url parameter "<key>" is not supported`, `… is given more than once`, `sslrootcert=system is not supported…` | Fix the URL. An unknown key is shown only if it is a known libpq or sqlx name, otherwise as `<unrecognised>`; values are never shown |
| `database url: sqlx connection options disagree with the transport check` | The URL or a `PG*` variable changed between the check and the connect, or sqlx parses it differently. The gateway refuses to connect; check `PGHOST`, `PGHOSTADDR`, `PGSSLMODE` and `PGSSLROOTCERT` in the process environment |
| `storage backend error: pool timed out while waiting for an open connection`, after about 5 s | No connection could be opened within the pool's 5 s acquire timeout. This usually means a **TLS or network failure**: the server is unreachable or refuses the connection, TLS is not enabled on the server, the certificate does not chain to the built-in roots or `sslrootcert`, or its name does not match the host. Check reachability, the server's `ssl` setting and the certificate. Certificate errors may also appear directly (`UnknownIssuer`, `not valid for name`) |
| Log `cluster bus connection lost; reconnecting (messages may be missed)` repeating every few seconds, with `/readyz` failing | A permanent connection failure, often an expired server certificate or a CA change. The bus retries indefinitely at a fixed interval and logs each attempt; fix the certificate, then restart |

**Certificate and CA rotation.** The gateway reads the database CA
(`sslrootcert`) once per process and caches the TLS configuration. A
server certificate renewed under the **same** CA should need no gateway
action, because new connections verify it against the cached roots (not
tested).
Moving to a **new** CA (or changing the `sslrootcert` file) needs a gateway
restart; do a rolling restart after the new CA file is in place. During a
CA migration, put both the old and the new CA in the `sslrootcert` file.

**Latency after idle periods.** The pool opens connections on demand and
closes idle ones after sqlx's idle timeout (10 minutes). After an idle
period, the first requests open a new connection, which now includes a
TLS handshake (one extra round trip plus a few milliseconds of CPU). Over a
high-latency link to a managed database the first requests after a quiet
period are therefore slower. Pool warm-up and handshake cost have not been
measured.

## Capacity

See `docs/benchmarks.md` for single-core figures of the hot paths. Size
PostgreSQL for the event and transmission rates of your fleet; set
retention limits so tables stay bounded.
