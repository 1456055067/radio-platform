# Configure

Both services read a TOML file. The annotated examples in
`deployments/examples/` are the starting point; this page explains the
settings administrators most often change. The full list is in
[Deployment](../deployment.md).

!!! tip "Secrets stay out of the file"
    Put the database URL in `DATABASE_URL`, S3 credentials in
    `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY` (or an instance role) and
    the transcription key in the variable named by `api_key_env`.

## Gateway (`gateway.toml`)

| Section | Key settings | Notes |
|---|---|---|
| `[http]` | `bind`, `tls` | Public API and console. TLS 1.3 only. `insecure_plaintext` only behind a TLS-terminating ingress. |
| `[http.ui]`, `[http.ui.auth]` | `dir`, `authority`, `client_id`, `scope` | Serves the console and tells it which IdP to use. |
| `[agents]` | `bind`, `cert`, `key`, `client_ca`, `client_crl`, `crl_refresh_secs` | Agent listener. Without `client_crl` the gateway warns at start-up. |
| `[database]` | `backend`, `max_connections`, `insecure_transport` | `insecure_transport = true` is a deliberate, logged opt-out of verified TLS. |
| `[auth]` | `issuer`, `audience`, `tenant_claim`, `sites_claim`, `roles_claim`, `max_session_secs`, `jwks_file` | `jwks_file` supports air-gapped sites. |
| `[auth.role_permissions]` | role → permissions | See [Users, roles and control](access-control.md). |
| `[limits]` | `requests_per_minute`, `requests_per_minute_per_ip`, `trusted_proxies`, `scanner_ports`, `audio_ports`, `lease_default_secs`, `lease_max_secs` | List reverse proxies in `trusted_proxies` so rate limits and audit use the real client IP. |
| `[cluster]` | `enabled` | Needed for more than one replica; replicas share PostgreSQL and need NTP. |
| `[metrics]` | `bind` | Prometheus endpoint, no authentication. Bind internally. |
| `[media]` | `udp_bind`, `public`, `max_sessions`, `max_sessions_per_subject` | Live audio. Absent means disabled. |
| `[recording]` | `storage`, `pre_buffer_ms`, `post_roll_ms`, `max_duration_secs` | Absent means disabled. |
| `[transcription]` | `endpoint`, `api`, `model`, `language`, `prompt`, `api_key_env`, `concurrency`, `max_attempts` | Needs `[recording]`. |
| `[retention]` | `recordings_days`, `transcripts_days`, `transmissions_days`, `events_days`, `sweep_interval_secs` | Unset limits keep data forever. |

## Agent (`agent.toml`)

| Section | Key settings | Notes |
|---|---|---|
| top level | `state_dir`, `max_buffered_events`, `max_scanners` | Without `state_dir`, events buffered during an outage are lost on restart. |
| `[gateway]` | `url`, `ca`, `cert`, `key` | `url` is `wss://<gateway>:<agent port>/agent/v1`. |
| `[network]` | `allowed_networks`, `allowed_ports`, `audio_ports` | **Enforced locally.** The agent never contacts a scanner outside these ranges, whatever the gateway asks. |
| `[telemetry]` | `mode` (`push` or `poll`), `interval_ms`, `hang_time_ms` | `hang_time_ms` is how long squelch must stay closed to end a transmission. |
| `[audio]` | `cooldown_secs`, `backoff_max_secs`, `linger_secs` | Spaces out audio sessions; the scanner's audio server is reported to be fragile. |
| `[[scanners]]` | `scanner_id`, `name`, `host` | Optional bootstrap so collection starts before the gateway is reachable. |

## Logging

Both services write JSON logs to stdout. Set the level with `RUST_LOG`,
for example `RUST_LOG=info,api_gateway=debug`.
