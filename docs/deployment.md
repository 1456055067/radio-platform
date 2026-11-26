# Deployment

**Status:** the binaries exist; container images, Compose and Helm come in
Phase 6.

## Components

| Binary | Where it runs | Listens on |
|---|---|---|
| `api-gateway` | Central | Public API (TLS 1.3) and agent listener (TLS 1.3 + client certs) |
| `scanner-agent` | Each site, for example a Raspberry Pi-class ARM64 host on the scanner segment | Nothing (dials out) |
| PostgreSQL ≥ 14 | Central | |

## PKI requirements

- **Agent CA:** issues one client certificate per agent.
  - The first DNS SAN, or the CN if there is no SAN, is the agent ID, for
    example `site-a-agent-1`.
  - Set the extended key usage `clientAuth`.
  - Before the agent connects, register the ID with `POST /api/v1/agents`.
- **Gateway certificate:** SANs must match the hostname or IP in the
  agent's `gateway.url`, with extended key usage `serverAuth`.
- Only TLS 1.3 is negotiated (rustls with the AWS-LC provider).
- Keep private keys readable only by the service user.

## Quick start (development)

```bash
cargo run --release -p sds-simulator -- --bind 127.0.0.1:50536
DATABASE_URL=postgres://... cargo run --release -p api-gateway -- deployments/examples/gateway.example.toml
cargo run --release -p scanner-agent -- deployments/examples/agent.example.toml
```

Then, in order:
1. Create a site with `allowed_networks` covering the scanner.
2. Register the agent ID.
3. Enroll the scanner.

The agent picks up the assignment immediately.

## Web console

1. Build it: `cd frontend && npm ci && npm run build`. This needs Node ≥ 22.12;
   `.npmrc` disables dependency install scripts. Copy `frontend/dist` to the
   gateway host.
2. Register a **public** OIDC client for the console:
   - authorization code flow with PKCE, and no client secret;
   - redirect URI `https://<console host>/auth/callback`;
   - post-logout redirect `https://<console host>/`;
   - the access token's `aud` must equal the gateway's `auth.audience` and
     carry the tenant, site and role claims;
   - refresh tokens are recommended: tokens are kept in memory only and
     renewed silently with the refresh token. Without refresh tokens a
     session ends when the token expires, or on reload if the identity
     provider has no session.
3. Add `[http.ui]` to the gateway configuration (see
   `deployments/examples/gateway.example.toml`). The console is then served
   from the API's own origin, so no CORS is needed. The Content Security
   Policy allows network connections only to that origin and the identity
   provider.

For UI development without hardware, run the simulated stack:

```bash
cargo run -p radio-devstack -- --ui-dir frontend/dist   # or run `npm run dev` in frontend/ and omit --ui-dir
```

`radio-devstack` is **development only**:
- it hands out tokens for any role at `/dev/token`;
- it refuses to bind anything but loopback;
- every scanner in it is a simulator, and the traffic is invented.

## Network rules

- Never expose UDP 50536 or TCP 554 beyond the scanner segment.
- Agents need outbound TCP to the gateway's agent port only.
- Gateway replicas:
  - one by default;
  - several with `[cluster] enabled = true` and a shared PostgreSQL
    (ADR 0008).

  Replicas must keep their clocks in sync with NTP. Any replica can accept
  agents and clients. A load balancer needs TCP passthrough for the agent
  port, because agents use mTLS end to end.
- Behind a reverse proxy, list it in `limits.trusted_proxies` so per-IP
  limits and audit source addresses use the real client.

## Audio (Phase 3)

Audio is **off by default** at every level. Only monitor or record
communications you are authorized to receive.

1. **Gateway, live listening:** add `[media]` (see
   `deployments/examples/gateway.example.toml`):
   - `udp_bind` is the WebRTC media port;
   - `public` is the address browsers can reach, which is required for
     wildcard binds or NAT;
   - browsers must be able to reach that UDP port on the replica holding
     the agent. In a cluster, WHEP offers are forwarded to that replica.
2. **Gateway, recording:** add `[recording]` with a storage backend:
   - `s3` reads credentials from `AWS_ACCESS_KEY_ID` and
     `AWS_SECRET_ACCESS_KEY` (or the instance role); keep credentials out
     of the file;
   - `local` writes to a directory;
   - `memory` is for development.

   Enable bucket encryption at rest and restrict bucket access to the
   gateway's identity.
3. **Agent:**
   - add the scanners' RTSP port to `network.audio_ports` (default
     `[554]`);
   - optionally tune `[audio]` (`cooldown_secs`, `backoff_max_secs`,
     `linger_secs`);
   - agents and gateways must both run protocol v3 for audio.
4. **Per scanner:** enable `audio_enabled` (and `rtsp_port` if it is not
   554). A `recording_manage` user may enable `recording_enabled`.

Build note: the agent and gateway compile libopus from source, which needs
`cmake` and a C compiler. GitHub's Ubuntu runners include both.

Port 554 stays on the scanner LAN: only the agent talks to it.

## Transcription and retention (Phase 5)

Both are **off by default**.

1. **Transcription** needs `[recording]` and a `[transcription]` section
   (see `deployments/examples/gateway.example.toml`):
   - `endpoint` is the service's base URL; `api` is `openai`
     (`/v1/audio/transcriptions`) or `whisper_cpp` (`/inference`);
   - prefer a service on the same host or network (for example
     whisper.cpp's `whisper-server` or another OpenAI-compatible server).
     A hosted service receives every recording you transcribe;
   - HTTPS is required unless the host is loopback or `allow_http = true`.
     Put the API key in an environment variable and name it in
     `api_key_env`. `ca_file` adds a private CA;
   - `min_duration_ms` (default 600) skips short clips, `concurrency`
     limits requests per replica, and `max_attempts` caps retries.

   Then a `transcription_manage` user enables transcription per scanner.
   Narrowband scanner audio is hard for every model; check quality before
   relying on transcripts.
2. **Retention** is set with `[retention]`; every limit is in days and an
   unset limit keeps data forever:
   - `recordings_days` deletes audio objects;
   - `transcripts_days` removes transcript text;
   - `transmissions_days` deletes transmission records (their recordings
     and transcripts go first);
   - `events_days` deletes scanner events.

   The audit log is never swept. Held transmissions are kept. Each replica
   sweeps every `sweep_interval_secs` (default 3600); running several is
   safe. Check your legal retention obligations before setting limits.

## Production packaging (Phase 6)

- **Images:** `deployments/container/Dockerfile` builds `gateway` (with the
  console at `/usr/share/radio-platform/ui`) and `agent` targets on
  distroless, running as UID 65532. CI publishes amd64 and arm64 images
  with SBOM and provenance, signed with cosign (keyless). Verify before
  deploying:
  `cosign verify ghcr.io/<owner>/radio-gateway@<digest> --certificate-identity-regexp '^https://github.com/<owner>/' --certificate-oidc-issuer https://token.actions.githubusercontent.com`.
  Pin images by digest.
- **Kubernetes:** `helm install radio deployments/helm/radio-platform -f
  my-values.yaml`. Create the TLS, agent CA and database secrets first (see
  `values.yaml`). Chart settings go in `config` as TOML; the chart owns the
  listener, TLS, database, cluster, metrics and media tables and refuses
  overrides. More than one replica enables cluster mode. Live audio is
  supported with one replica only.
- **Site agents:** `deployments/systemd/radio-agent.service`.
- **Metrics:** `[metrics] bind = "127.0.0.1:9464"` (or the pod IP behind
  the chart's NetworkPolicy). The endpoint has no authentication.
- **Checks:** `api-gateway --check-config` and `scanner-agent
  --check-config` validate a file without starting the service.

## Agent certificate revocation

Set `agents.client_crl` to a PEM CRL file published by the agent CA.
- **What is refused:** agents whose certificate is revoked, or whose
  status the CRL cannot answer.
- **Expiry is fatal:** once the CRL passes its `nextUpdate`, **every**
  agent is refused. Automate re-publication well before expiry.
- **Reload:** the file is re-read every `crl_refresh_secs`. When it
  changes, all connected agents are made to reconnect, so a revoked agent
  is dropped promptly.
- **Bad files:** an unreadable or invalid new file is logged and the
  previous CRL stays in force.

Without `client_crl`, the gateway logs a warning at start-up.

## Agent state

Set `state_dir`, for example `/var/lib/radio-agent`, owned by the agent's
service user. Unacknowledged events are journaled there (directory `0700`,
files `0600`) and replayed after a restart or power loss. Without it, events
buffered during an outage are lost if the agent restarts.
