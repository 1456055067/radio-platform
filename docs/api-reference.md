# API Reference (v1)

The machine-readable OpenAPI 3.1 document is served without authentication
at `GET /api/v1/openapi.json`, generated from code. This page summarises
the conventions.

## Authentication and authorization

- Send `Authorization: Bearer <OIDC access token>` with every request.
  - The token is verified against the issuer's JWKS, either discovered or
    loaded from a static file.
  - Only asymmetric algorithms are accepted.
  - The gateway enforces `iss`, `aud`, `exp`, `nbf` and the optional
    maximum session age.
- Token claims are configurable:

  | Claim (default) | Meaning |
  |---|---|
  | `tenant_id` | Tenant UUID (required) |
  | `sites` | Array of site UUIDs, or `["*"]`. Absent means all sites. |
  | `roles` | Roles, mapped to permissions in configuration |

- Permissions:

  | Permission | Grants |
  |---|---|
  | `scanner_view` | List and get scanners, status, telemetry, volume and squelch reads, the live stream |
  | `scanner_control` | Acquire and release leases; control commands |
  | `scanner_admin` | Raw protocol commands; revoking others' leases |
  | `scanner_manage` | Enroll, update and delete scanners |
  | `site_manage` | Create sites; register agents |
  | `event_view` | Events and transmissions |
  | `audit_view` | Audit log |
  | `audio_listen`, `recording_manage`, `transcription_manage`, `export` | Reserved for Phases 3–5 |

- Resources in other tenants return **404**. Resources outside the caller's
  site scope return **403**.
- Rate limits, both per gateway replica, return **429** (with
  `Retry-After` for the per-IP limit):
  - per authenticated subject (`limits.requests_per_minute`);
  - per client IP, **before** authentication
    (`limits.requests_per_minute_per_ip`). Health probes are exempt.
    `X-Forwarded-For` is honoured only from `limits.trusted_proxies`.
- Every 401 and 429 (and every refusal on the agent listener) is also
  written to the gateway's security event log, throttled per source, and
  counted in `radio_security_events_total{reason}` (ADR 0013; schema in
  `docs/operations.md`, "Security event log (SIEM)").

## Control leases

Each scanner is a single receiver with a single controller.

Leases are stored in the database, so they survive gateway restarts and
are shared by all replicas (ADR 0008).

1. Acquire control with `POST /scanners/{id}/leases` and body
   `{"ttl_secs": 300}`, which returns `lease_id`. Calling it again while you
   hold the lease renews it; if someone else holds it you get **409**.
2. Send state-changing requests with `X-Lease-Id: <lease_id>`. A missing
   header returns **428**, and someone else's lease returns **403**.
3. Release with `DELETE /scanners/{id}/leases/{lease}`. Holders and
   `scanner_admin` users may release.

`GET /scanners/{id}` (and each item of `GET /scanners`) reports the current
`control.holder`. The `lease_id` is included for the holder and for
`scanner_admin` users, who need it to revoke the lease. Knowing a lease id
does not let anyone else send commands: commands are checked against the
holder.

## Endpoints

| Method | Path | Permission | Notes |
|---|---|---|---|
| GET | `/api/v1/me` | any valid token | Caller's subject, tenant, site scope, permissions and expiry. The UI uses it to choose which controls to show; the server still authorizes every request. |
| GET/POST | `/api/v1/sites` | view / site_manage | Sites carry `allowed_networks`, which drive the SSRF policy |
| GET | `/api/v1/agents` | view | Agents in scope with live `connected`. Filter: `site_id` |
| POST | `/api/v1/agents` | site_manage | Registers an agent identity (its certificate DNS SAN) to a site |
| GET/POST | `/api/v1/scanners` | view / scanner_manage | Items include `agent_connected` and `control`. Enrollment checks the site's networks and allowed ports (**422** otherwise) |
| GET/PATCH/DELETE | `/api/v1/scanners/{id}` | view / scanner_manage | Changes are pushed to the agent immediately. `PATCH` also takes:<ul><li>`audio_enabled`;</li><li>`rtsp_port` (must be in `limits.audio_ports`, otherwise **422**);</li><li>`recording_enabled` (additionally needs `recording_manage` and recording configured on the gateway, otherwise **409**; requires audio);</li><li>`transcription_enabled` (additionally needs `transcription_manage` and transcription configured on the gateway, otherwise **409**; requires recording). Turning recording or audio off turns transcription off.</li></ul> |
| GET | `/api/v1/scanners/{id}/status` | view | Live GSI through the agent |
| GET | `/api/v1/scanners/{id}/capabilities` | view | Read-only capability probe |
| GET | `/api/v1/scanners/{id}/telemetry` | view | Latest stored snapshot (writes throttled to 1/s per scanner) |
| GET/PUT | `/api/v1/scanners/{id}/volume` | view / control + lease | `{"level": n}` |
| GET/PUT | `/api/v1/scanners/{id}/squelch` | view / control + lease | |
| POST | `/api/v1/scanners/{id}/hold` | control + lease | `{"kind": "system", "index": 283}` |
| POST | `/api/v1/scanners/{id}/resume` | control + lease | Restarts the scan from the top (see protocol-compliance) |
| POST | `/api/v1/scanners/{id}/commands` | view, control or admin | Typed `ScannerCommand`. `raw` requires `scanner_admin` |
| POST/DELETE | `/api/v1/scanners/{id}/leases[/{lease}]` | control | |
| GET | `/api/v1/transmissions` | event_view | Filters: `scanner_id`, `site_id`, `frequency_hz`, `tgid`, `from`, `to` (RFC 3339), `recorded` (`true`/`false`), `transcription` (`pending`, `done`, `failed`, `skipped`), and `q`: full-text search over transcripts in web-search syntax (words, `"phrases"`, `or`, `-word`; at most 256 characters). `q` needs `audio_listen` too and is audited (`transmission.search`). Transcript fields are removed for callers without `audio_listen` |
| GET | `/api/v1/transmissions/{id}` | event_view | Metadata; `audio_object` is set when a recording exists. Transcript fields as above |
| POST | `/api/v1/transmissions/{id}/transcribe` | event_view + transcription_manage | Queues the recording for (re-)transcription: **202**. **409** without a recording, **503** if transcription is not configured. Audited |
| PUT | `/api/v1/transmissions/{id}/hold` | event_view + recording_manage | `{"hold": true}` exempts the transmission, its recording and transcript from retention and deletion; `false` releases it. Audited (`transmission.hold` / `transmission.release`) |
| DELETE | `/api/v1/transmissions/{id}/audio` | event_view + recording_manage | Deletes the recording and its transcript; the record stays. **409** while held. Audited (`transmission.audio.delete`) |
| GET | `/api/v1/transcription/status` | view | `{enabled, service, queue: {pending, done, failed}, worker}`. `queue` is the caller's tenant (done/failed: last 24 hours); `service` (request style and host) only for `transcription_manage` |
| GET | `/api/v1/retention` | view | The global retention limits; `last_sweep` (this replica, all tenants) only for `audit_view` |
| GET | `/api/v1/analytics/activity` | event_view | `from`, `to`, `bucket` (`hour`, up to 31 days; `day`, up to 366), `site_id`, `scanner_id`. Every bucket from `from` (floored, UTC) to `to`, including empty ones: `{start, transmissions, airtime_ms}` |
| GET | `/api/v1/analytics/top` | event_view | Same window parameters plus `by` (`talkgroup`, `channel`, `frequency`, `scanner`, `unit`) and `limit` (1–50, default 10) |
| GET | `/api/v1/transmissions/{id}/audio` | event_view + audio_listen | The recording as `audio/ogg; codecs=opus`. Audited (`transmission.audio.play`) |
| POST | `/api/v1/scanners/{id}/audio/whep` | audio_listen | WHEP: body is an SDP offer (`application/sdp`, ≤ 16 kB) with a receive-only audio transceiver. **201** with the SDP answer and `Location: /api/v1/audio/sessions/{session}`. **409** if audio is disabled for the scanner, **429** over the session limits (default 4 per user), **503** if live audio is not configured or the agent is offline. Audited (`scanner.audio.listen`) |
| DELETE | `/api/v1/audio/sessions/{session}` | creator or scanner_admin | Ends a listening session |
| GET | `/api/v1/scanners/{id}/audio` | view | `{audio_enabled, recording_enabled, live_available, state, listeners, receiving}` |
| GET | `/api/v1/media/status` | view | Live audio and recording status of this replica |
| GET | `/api/v1/events` | event_view | Filters: `scanner_id`, `kind` |
| GET (WS) | `/api/v1/events/stream` | view | Live telemetry and events; see below |
| GET | `/api/v1/audit` | audit_view | Filters: `actor` (exact), `action` (prefix), `outcome`, `resource_type`, `resource_id`, `from`, `to`. Records carry `seq`, `prev_hash` and `hash` (ADR 0011) |
| GET | `/api/v1/audit/verify` | audit_view | Recomputes the tenant's hash chain: `{ok, verified, unchained, head_seq, head_hash, failure}` |
| GET | `/api/v1/audit/export` | audit_view + export | Same filters plus `format` (`jsonl`, default, or `csv`); oldest first, at most 100 000 records (**422** otherwise). JSON Lines include hashes and verify offline. Audited (`audit.export`) |
| GET/POST | `/api/v1/groups` | view / scanner_manage + all sites | Members outside the caller's site scope are hidden |
| GET/DELETE | `/api/v1/groups/{id}` | view / scanner_manage + all sites | Deleting a group leaves its scanners alone |
| PUT/DELETE | `/api/v1/groups/{id}/members/{scanner_id}` | scanner_manage at the scanner's site | `PUT` is idempotent |
| POST | `/api/v1/sites/{id}/discover` | scanner_manage at the site | Body `{"agent_id", "networks"?, "port"?}`. Returns `{"candidates": [{host, port, model}], "skipped_enrolled"}`. Networks must be inside the site's (and the agent's) authorized networks, at most 1024 addresses (**422** or **400**). **501** for agents older than protocol v2. Nothing is enrolled automatically |
| GET | `/healthz`, `/readyz` | none | Liveness; readiness (database ping) |
| GET | `/metrics` | none, **separate listener** | Prometheus metrics on `[metrics] bind` only, never on the public listener. Metric list in `docs/operations.md`, "Metrics", including `radio_security_events_total{reason}`, `radio_security_log_suppressed_total` and `radio_security_log_tracked_sources` |

Every route's required permissions are pinned by
`services/api-gateway/tests/authz_matrix.rs`, which fails when a route is
added without an entry.

Transcripts are content: reading them needs `audio_listen`, like playback.
They are machine-generated and can be wrong. Retention runs only when the
gateway sets limits; see `docs/deployment.md`.

## Pagination

List endpoints accept `limit` (1–200, default 50) and `cursor`, and return
`{"items": [...], "next_cursor": "..." | null}`. Cursors are opaque keyset
positions.

## Errors

Errors use RFC 9457 `application/problem+json`:
`{"type", "title", "status", "detail"}`.

How command failures map to HTTP status:

| Failure | Status |
|---|---|
| Invalid arguments | 400 |
| Scanner rejected the command (`NG`) | 409 |
| Unsupported by the firmware (`ERR`) | 501 |
| Agent offline | 503 |
| No reply in time | 504 (`detail` says whether state may have changed) |

## Live stream

Connect a WebSocket to `/api/v1/events/stream?scanner_id=...` (the
`scanner_id` filter is optional). Browsers cannot set the `Authorization`
header on a WebSocket, so they offer two subprotocols instead:
`radio-platform.v1` and `radio-platform.bearer.<token>`.

Each message is `{"site_id", "scanner_id", "kind": "telemetry"|"event"|"audio", "data"}`
(`audio` carries the agent's audio pipeline state).
- The stream is filtered by tenant, site scope and permission.
- It closes with code 4001 when the session expires.

## Web UI

When `[http.ui]` is configured, the public listener also serves the console:

- `GET /ui-config.json` holds the public OIDC client settings (`mode`,
  `authority`, `client_id`, `scope`) and no secrets.
- Static assets: hashed files under `/assets/` are cached as immutable,
  and `index.html` is revalidated on every load.
- Unknown non-API paths fall back to `index.html`. Unknown `/api/` paths
  still return a JSON 404.
- UI documents carry `Content-Security-Policy`:
  - `script-src 'self'`;
  - `connect-src 'self' <idp origin>`;
  - `frame-ancestors 'none'`;
  - `base-uri 'none'`;
  - `object-src 'none'`;
  - `media-src 'self' blob:` (recording playback).
- All responses carry:
  - `X-Content-Type-Options: nosniff`;
  - `X-Frame-Options: DENY`;
  - `Referrer-Policy: no-referrer`;
  - `Cross-Origin-Opener-Policy: same-origin`;
  - a restrictive `Permissions-Policy`.
- API responses also carry `Cache-Control: no-store`. When the gateway
  terminates TLS itself, it adds `Strict-Transport-Security`.
