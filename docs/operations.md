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
| Database transport | Start-up log `database connection uses verified TLS`, `database is on this host`, or the `WARN` `database connection is not verified TLS (insecure_transport = true)…` (fields `host`, `port`, `sslmode`; never the URL) |

Both services log JSON to stdout; set `RUST_LOG` (for example
`info,api_gateway=debug`).

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
