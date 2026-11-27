# Run and maintain

The full runbook is [Operations](../operations.md). This page is the
short version.

## Is it healthy?

| Check | How |
|---|---|
| Gateway alive | `GET /healthz` |
| Gateway ready (database reachable) | `GET /readyz` |
| Agents connected | Metric `radio_agents_connected`; `agent_connected` on each scanner; the dashboard |
| Scanner health | `health` on each scanner; `health_changed` events |
| Agent outage buffer | `pending_events` and `dropped_events` in the agent status |
| Audit log intact | `GET /api/v1/audit/verify` returns `ok` |

Scrape `/metrics` from the internal `[metrics]` listener with Prometheus.
Useful series include request rates and latencies by route, connected
agents, media sessions, recordings stored or failed, transcription
outcomes and retention sweeps.

## Backups

| Data | How |
|---|---|
| PostgreSQL | Daily `pg_dump -Fc` plus WAL archiving, or your provider's point-in-time recovery |
| Recordings | Bucket versioning and replication (S3), or filesystem snapshots |
| Audit anchors | Keep the hourly `audit chain head` log lines in your log archive |
| Configuration and PKI | Configuration management; agent CA key offline |

Practise a restore before you need one. The drill is in
[Operations → Backup and restore](../operations.md#backup-and-restore).

## Upgrades

- **Gateways first, then agents.** Gateways accept older agent protocols.
- Gateway upgrades are rolling. Database migrations run at start-up, are
  forward-only and additive.
- Downgrades across a migration are not supported: restore from backup.
- Commands in flight on a stopping replica fail and are not retried.
- Run `--check-config` with the production settings before rolling out.

## Incidents

| Situation | First steps |
|---|---|
| Compromised agent | Revoke its certificate, publish the CRL, disable its scanners |
| Compromised user | Disable at the IdP, release their leases, export their audit trail |
| Suspected audit tampering | Run `/audit/verify`, export the log, compare with archived anchors |
| Scanner audio locked up | Disable audio for it, power-cycle, raise `audio.cooldown_secs` |
| Gateway will not start: database | Usually the TLS policy; see [Troubleshooting the database connection](../operations.md#troubleshooting-the-database-connection) |
