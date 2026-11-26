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

## Capacity

See `docs/benchmarks.md` for single-core figures of the hot paths. Size
PostgreSQL for the event and transmission rates of your fleet; set
retention limits so tables stay bounded.
