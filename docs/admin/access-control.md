# Users, roles and control

## How authorization works

Every request carries an OIDC access token. The gateway verifies it
(issuer, audience, expiry, signature with an asymmetric algorithm) and
then applies attribute-based rules:

1. **Tenant.** Resources in another tenant return 404, as if they did not
   exist.
2. **Site scope.** Resources at a site outside the token's `sites` claim
   return 403.
3. **Permission.** The token's roles are mapped to permissions in the
   gateway configuration.
4. **Lease.** State-changing scanner commands also need the caller's
   active lease.

The console hides controls you cannot use, but that is only a
convenience: the gateway checks every call.

## Permissions

| Permission | Grants |
|---|---|
| `scanner_view` | See scanners, status, telemetry and the live stream |
| `scanner_control` | Take and release leases; control commands |
| `scanner_admin` | Raw protocol commands; revoke other people's leases |
| `scanner_manage` | Enroll, change and delete scanners; discovery; groups |
| `site_manage` | Create sites; register agents |
| `event_view` | Events, transmissions, analytics |
| `audit_view` | Read and verify the audit log |
| `audio_listen` | Live listening, recording playback and transcripts |
| `recording_manage` | Turn recording on per scanner; hold or delete recordings |
| `transcription_manage` | Turn transcription on per scanner; re-transcribe |
| `export` | Export the audit log |

## Mapping roles

Map your identity provider's role names to permissions:

```toml
[auth.role_permissions]
radio-admin    = ["scanner_view", "scanner_control", "scanner_admin", "scanner_manage",
                  "site_manage", "event_view", "audio_listen", "recording_manage",
                  "transcription_manage", "export", "audit_view"]
radio-operator = ["scanner_view", "scanner_control", "event_view", "audio_listen"]
radio-viewer   = ["scanner_view", "event_view"]
```

Give `scanner_admin` sparingly. Raw commands go straight to the scanner,
and some undocumented ones (for example `MSM`) reboot it.

## Control leases

Each scanner has one controller at a time.

- A lease is taken for a limited time (default 300 s, at most
  `limits.lease_max_secs`). The console renews it every 120 s while the
  page is open and releases it when you leave.
- Commands without the lease are refused (428 without the header, 403 with
  someone else's lease).
- `scanner_admin` users can revoke anyone's lease.
- Leases are stored in PostgreSQL, so they survive gateway restarts and are
  shared across replicas.
- State-changing commands are **never retried automatically**. If one
  times out, it may or may not have run; the operator decides whether to
  send it again.

## Limiting sessions

- `auth.max_session_secs` caps how long a sign-in lasts (8 h in the
  example), regardless of refresh tokens.
- Rate limits apply per user and per client IP.

## When an account is compromised

Disable it at the identity provider, release its leases, and export its
audit trail. See [Operations → Incident procedures](../operations.md#incident-procedures).
