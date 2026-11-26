# NIST SP 800-53 Rev 5 Control Implementation Matrix

This matrix maps radio-platform to NIST SP 800-53 Rev 5 controls. It is
**evidence, not a claim of compliance**. A control appears only where a
concrete implementation can be cited (file and item). A status of
"Implemented (tested)" also requires a named test or CI check that
exercises it. Controls the platform does not meet are listed as gaps.

- **As of:** 2026-11-26 (after 0.1.0, including the PostgreSQL transport
  policy of ADR 0012, not yet released).
- **Hardware:** everything has been verified **against simulators only**
  (`tests/simulator`, the Whisper stub, in-process PKI). No SDS200 has
  been tested. The hardware tests in `tests/interoperability` are
  `#[ignore]`d and have not been run.
- **FIPS:** no FIPS 140 validation is claimed (see SC-13).

## Status legend

| Status | Meaning |
|---|---|
| Implemented (tested) | Code or configuration is cited, and a named test or CI step exercises it |
| Implemented (untested) | Code exists; no test exercises it |
| Partial | Some of the control is implemented; what is missing is stated |
| Inherited / deployer | The control is met by the IdP, the hosting platform or the operator; the cell says what the platform provides |
| Not implemented | Relevant, and not met; a one-line gap is given |

"Test evidence" names Rust test functions (`fn` name and file), frontend
tests (`it(...)` title and file) or CI jobs (`.github/workflows/ci.yml`
job name). PostgreSQL-only tests run only in the CI `postgres` job
(`#[ignore]` locally without `TEST_DATABASE_URL`). The database TLS test
`postgres_tls_verify_full_with_private_ca`
(`crates/platform-storage/tests/postgres_tls.rs`) is `#[ignore]` without
`TEST_TLS_DATABASE_URL`. It and the CI steps added with it (the TLS
PostgreSQL step of the `postgres` job and the `helm` step "Rendered
gateway.toml applies the database transport policy") **have not yet run on
GitHub Actions**, and the TLS test was **NOT RUN** locally for the final
tree (the local PostgreSQL server had no disk space). Rows that cite them
say so.

## Scope

**Inside the system boundary:** the API gateway (`services/api-gateway`),
the media gateway (`services/media-gateway`), the site agent
(`services/scanner-agent`), the web console (`frontend/`), the shared
crates (`crates/*`), their datastores as the platform uses them (the
PostgreSQL schema in `migrations/`, the recording object layout, the
agent journal), and the deployment artifacts (`deployments/`).

**External (not assessed here):** the OIDC identity provider; PostgreSQL
hosting; the S3-compatible object store; Kubernetes and the container
runtime; the agent CA; the transcription service; the scanners
themselves. The scanner's native protocol (UDP 50536, RTSP/RTP) is
unauthenticated and plaintext by design of the device. It is outside
what the platform can protect, apart from isolation and strict parsing.

Paths are relative to the repository root.

## AC: Access Control

| Control | Title | Status | Implementation (file → item) | Test evidence |
|---|---|---|---|---|
| AC-2 | Account Management | Inherited / deployer | IdP owns user accounts. Platform: `crates/platform-auth/src/oidc.rs` → `OidcVerifier::principal` maps roles to permissions; unknown roles grant nothing. Agent identities: `services/api-gateway/src/api/sites.rs` → `register_agent` | `valid_token_yields_principal` (`crates/platform-auth/tests/oidc.rs`); `tenant_and_site_isolation` (`services/api-gateway/tests/api.rs`, agent-id hijack → 409) |
| AC-3 | Access Enforcement | Implemented (tested) | `crates/platform-auth/src/abac.rs` → `Principal::authorize` (tenant, site, permission, expiry); `services/api-gateway/src/state.rs` → `AppState::authorize`; tenant-scoped `Store` trait (`crates/platform-storage/src/store.rs`) | `table_covers_every_route`, `every_route_enforces_its_permissions` (`services/api-gateway/tests/authz_matrix.rs`); `all_rules_apply` (`abac.rs`); `tenant_and_site_isolation` (`api.rs`) |
| AC-4 | Information Flow Enforcement | Implemented (tested) | `crates/platform-common/src/net.rs` → `NetworkPolicy::check`, `check_audio`; gateway: `services/api-gateway/src/api/scanners.rs` → `check_endpoint`; agent-local re-check: `crates/scanner-controller/src/registry.rs` → `start` | `full_stack` (`services/scanner-agent/tests/e2e.rs`, out-of-policy assignment gets no packet); `enrollment_network_policy_prevents_ssrf` (`api.rs`); `policy_and_limits_are_enforced` (`crates/scanner-controller/tests/registry.rs`); `memory_store_conformance` (`crates/platform-storage/tests/conformance.rs`, tenant isolation) |
| AC-6 | Least Privilege | Implemented (tested) | `abac.rs` → `Permission` (11 permissions; no implicit admin); `crates/platform-common/src/command.rs` → `ScannerCommand::requires_admin` (raw needs `scanner_admin`); exclusive control leases (`services/api-gateway/src/leases.rs` → `Leases::acquire`) | `leases_gate_control_commands` (`api.rs`); `control_and_admin_requirements` (`command.rs`); `every_route_enforces_its_permissions` |
| AC-7 | Unsuccessful Logon Attempts | Inherited / deployer | No passwords reach the platform; lockout belongs to the IdP. Platform: per-IP rate limit **before** token verification (`state.rs` → `ip_rate_limit`) | `per_ip_limit_applies_before_authentication` (`api.rs`) |
| AC-12 | Session Termination | Implemented (tested) | `oidc.rs` → `OidcVerifier::principal` (`max_session_secs` caps `expires_at`); `abac.rs` → `Principal::authorize` (expired → deny); `services/api-gateway/src/api/stream.rs` → `pump` (closes WebSocket 4001 at expiry) | `max_session_age_is_enforced` (`oidc.rs` tests); `all_rules_apply`. The stream close at expiry is untested. `max_session_secs` is optional (unset = token `exp` only) |
| AC-17 | Remote Access | Implemented (tested) | All API access needs an OIDC bearer (`state.rs` → `Auth` extractor). Agents dial out over mTLS (ADR 0004; `services/api-gateway/src/tls.rs` → `mtls_server_config_with_crls`) | `authentication_is_required` (`api.rs`); `mtls_rejects_untrusted_and_unregistered_agents` (`e2e.rs`). TLS 1.3-only on the public listener (`tls::server_config`) has no test |

## AU: Audit and Accountability

| Control | Title | Status | Implementation (file → item) | Test evidence |
|---|---|---|---|---|
| AU-2 | Event Logging | Partial | Audited: operator actions, denials, playback, searches, exports, deletions, holds, retention sweeps (`state.rs` → `AppState::audit`, `AppState::authorize`). **Missing:** failed token verification (401) and rate-limit rejections (429) are not audited, only counted in metrics. Transcripts shown in lists are not audited one by one | `permissions_and_audit_of_denials` (`api.rs`); `full_stack` (`e2e.rs`, command audit actions); `transcripts_are_content` (`services/api-gateway/tests/phase5.rs`, search audited) |
| AU-3 | Content of Audit Records | Implemented (tested) | `crates/platform-storage/src/model.rs` → `NewAudit` (actor, action, resource type/id, outcome, details, source IP; time set on append); `state.rs` → `client_ip` (XFF only from trusted proxies) | `verify_filter_and_export` (`services/api-gateway/tests/audit.rs`); `forwarded_for_is_only_trusted_from_trusted_proxies` (`state.rs`) |
| AU-4 | Audit Log Storage Capacity | Not implemented | The audit table grows without bound. There is no capacity metric, alert or offload; sizing is left to the operator | — |
| AU-5 | Response to Audit Logging Process Failures | Partial | `state.rs` → `AppState::audit` logs `failed to write audit record`. **Missing:** the action still proceeds (no fail-closed option); no metric or alert; no test | — (untested) |
| AU-6 | Audit Record Review, Analysis, and Reporting | Implemented (tested) | `services/api-gateway/src/api/history.rs` → `list_audit` (actor, action prefix, outcome, resource, time filters); `api/audit.rs` → `export` (JSON Lines/CSV); console `frontend/src/pages/Audit.tsx`. Correlation and SIEM are the deployer's | `verify_filter_and_export` (`audit.rs` tests); `filters, and reports a broken chain plainly` (`frontend/src/pages/Audit.test.tsx`) |
| AU-8 | Time Stamps | Inherited / deployer | `crates/platform-storage/src/postgres.rs` → `PgStore::append_audit` stamps UTC (`now_utc`, truncated by `audit_chain::micros`). Clock sync (NTP) is the deployer's | `postgres_store_conformance_and_migrations` (`conformance.rs`, chain verifies after the PostgreSQL round trip; CI `postgres`) |
| AU-9 | Protection of Audit Information | Implemented (tested) | `migrations/0005_audit_chain.sql` → trigger `audit_log_append_only`; `crates/platform-storage/src/audit_chain.rs` → `link`, `Verifier`; `PgStore::append_audit` (head row lock, one transaction); read needs `audit_view` (`history.rs` → `authorize_audit`) | `detects_alteration_removal_and_truncation` (`audit_chain.rs`); `postgres_store_conformance_and_migrations` → `audit_tampering` (`conformance.rs`, CI `postgres`); `memory_store_conformance` (concurrent appends stay linear); `verify_filter_and_export` |
| AU-10 | Non-repudiation | Partial | Actor is the verified token `sub`; per-tenant hash chain; hourly head log line `audit chain head` (`services/api-gateway/src/main.rs` → `run`). **Missing:** no digital signatures. Anyone who can rewrite both records and head can rebuild a valid chain unless the operator anchors heads outside the database. Head logging is untested | `verify_filter_and_export` (chain verification only) |
| AU-11 | Audit Record Retention | Partial | No `Store` method deletes audit records (`store.rs`); retention never touches them and writes one `retention.sweep` record (`services/api-gateway/src/retention.rs` → `Retention::sweep`). **Missing:** no configurable audit retention period or archival; records are kept indefinitely | `retention_sweeps_respect_holds_and_audit` (`phase5.rs`) |
| AU-12 | Audit Record Generation | Implemented (tested) | `state.rs` → `AppState::audit` called from handlers; `api/audit.rs` → `export` audits itself; `retention.rs` → `Retention::sweep` audits as `system:retention` | `permissions_and_audit_of_denials`; `verify_filter_and_export` (export audited); `retention_sweeps_respect_holds_and_audit` |

## CM: Configuration Management

| Control | Title | Status | Implementation (file → item) | Test evidence |
|---|---|---|---|---|
| CM-2 | Baseline Configuration | Implemented (tested) | `deployments/examples/gateway.example.toml`, `agent.example.toml`; `deployments/helm/radio-platform/values.yaml` + `values.schema.json` | `example_configuration_parses` (`services/api-gateway/src/config.rs`); `example_configuration_parses_with_every_key_in_place` (`services/scanner-agent/src/config.rs`); CI `check` (`--check-config` on examples), CI `helm` (rendered `gateway.toml` validated) |
| CM-6 | Configuration Settings | Implemented (tested) | `services/api-gateway/src/config.rs` → `GatewayConfig::validate` (TLS required unless `insecure_plaintext`; transcription needs recording); `NetworkPolicy` default denies; audio, recording, transcription off by default; `crates/scanner-transcription/src/config.rs` → `validate` (HTTPS off loopback); `deployments/helm/radio-platform/templates/validate.yaml`. Database transport (ADR 0012): `services/api-gateway/src/config.rs` → `GatewayConfig::check_database_transport` (applied to the effective URL, `DATABASE_URL` over `[database] url`, and the `PG*` variables), run by `--check-config` (`services/api-gateway/src/main.rs` → `check_config`, exit 2 on refusal) and before connecting in `run` | `default_denies_everything` (`net.rs`); `https_required_off_loopback` (`scanner-transcription` `config.rs`); `retention_limits` (`api-gateway` `config.rs`); CI `helm` "Guard rails refuse unsafe values"; `database_url_env_overrides_file`, `check_database_transport_url_not_set`, `memory_backend_skips_transport_check` (`api-gateway` `config.rs`); `check_config_refuses_remote_plaintext_database_without_leaking_password`, `qa_check_config_uses_file_url_and_database_url_overrides_it`, `qa_check_config_applies_pg_environment`, `qa_start_refuses_remote_plaintext_before_connecting_and_logs_no_secret` (`services/api-gateway/tests/check_config.rs`). Supporting, not yet run on GitHub Actions: CI `helm` step "Rendered gateway.toml applies the database transport policy" (in-cluster plaintext URL refused by the default render) |
| CM-7 | Least Functionality | Implemented (tested) | Distroless non-root images (`deployments/container/Dockerfile`); no `/dev/*` routes in the gateway (`services/api-gateway/src/ui.rs`); metrics on their own listener (`metrics.rs` → `router`); discovery sends only `MDL` (`services/scanner-agent/src/discovery.rs` → `plan`, `probe`); the insecure database transport is an explicit opt-out, off by default in the TOML and Helm (`[database] insecure_transport`, `database.insecureTransport: false`) and warned at every start, in `--check-config` and in Helm `NOTES.txt` (`crates/platform-storage/src/pg_transport.rs` → `PgTarget::check`; `services/api-gateway/src/main.rs` → `log_database_transport`) | `ui_is_served_with_security_headers` (`api.rs`, `/dev/token` not served); `plan_enforces_local_policy_and_limits`, `recognises_model_replies_only` (`discovery.rs`); CI `container` (user 65532); `insecure_transport_parses_and_defaults_false` (`api-gateway` `config.rs`); `opt_out_allows_with_warning_verdict` (`pg_transport.rs`); `check_config_warns_with_insecure_transport`, `qa_check_config_opt_out_does_not_warn_for_safe_targets` (misspelt opt-out fails closed), `qa_start_logs_transport_verdict_without_secret` (start-up `WARN`) (`check_config.rs`) |
| CM-8 | System Component Inventory | Partial | Image SBOMs (`.github/workflows/images.yml`, `sbom: true`); scanner and agent inventory through the API (`GET /api/v1/agents`, `/scanners`). **Missing:** SBOMs are built only on release tags and never checked in CI; the inventory of deployed instances is the operator's | `me_and_agent_listing_respect_scope` (`api.rs`) |
| CM-10 | Software Usage Restrictions | Partial | `deny.toml` `[licenses]` allow-list for Rust crates. **Missing:** no license check for npm dependencies | CI `deny` (`cargo deny check … licenses`) |

## CP: Contingency Planning

| Control | Title | Status | Implementation (file → item) | Test evidence |
|---|---|---|---|---|
| CP-9 | System Backup | Inherited / deployer | No backup tooling ships. Platform: all state is in PostgreSQL and the object store; a backup and restore procedure is documented (`docs/operations.md`, "Backup and restore"); `GET /api/v1/audit/verify` (`api/audit.rs` → `verify`) checks a restored chain | `verify_filter_and_export` (verification endpoint only; no backup test) |
| CP-10 | System Recovery and Reconstitution | Inherited / deployer | Idempotent, forward-only migrations (`postgres.rs` → `PgStore::migrate`); durable agent journal replay (`services/scanner-agent/src/journal.rs` → `Journal`); SIGTERM handling (`main.rs` → `shutdown_signal`). The restore drill is manual; downgrades are unsupported | `postgres_store_conformance_and_migrations` (migrate twice); `buffered_events_survive_agent_restart` (`e2e.rs`); `survives_restart_with_same_session_and_sequence` (`journal.rs`) |

## IA: Identification and Authentication

| Control | Title | Status | Implementation (file → item) | Test evidence |
|---|---|---|---|---|
| IA-2 | Identification and Authentication (Organizational Users) | Implemented (tested) | `oidc.rs` → `OidcVerifier::verify` (JWKS; asymmetric algorithms only; `iss`, `aud`, `exp`, `nbf`; `sub` required). The IdP authenticates. MFA is the IdP's; the gateway does not check `acr`/`amr` | `valid_token_yields_principal`, `rejects_bad_tokens` (`alg: none` and HS256 refused) (`crates/platform-auth/tests/oidc.rs`); `authentication_is_required` (`api.rs`) |
| IA-3 | Device Identification and Authentication | Implemented (tested) | Agents: `tls.rs` → `mtls_server_config_with_crls`, `agent_identity` (DNS SAN, else CN); `services/api-gateway/src/agent_ws.rs` → `agent_socket` (must be registered). Scanners cannot authenticate (device protocol); only source-address filtering applies | `mtls_rejects_untrusted_and_unregistered_agents` (`e2e.rs`); `revoked_agents_are_refused_after_reload_and_expired_crls_fail_closed` (`services/api-gateway/tests/crl.rs`) |
| IA-5 | Authenticator Management | Partial | No user passwords stored. JWKS refresh on unknown `kid`, rate-limited (`oidc.rs` → `OidcVerifier::key_for`); agent certificate revocation by CRL (`services/api-gateway/src/crl.rs` → `CrlReloader`). Database credentials: off loopback they cross the network only inside verified TLS unless `insecure_transport` is set (`pg_transport.rs` → `PgTarget::check`); the URL and `PgConnectOptions` are never logged or echoed (errors and logs carry host, port and sslmode only; unknown URL parameters are refused before sqlx, which would log their values). **Missing:** no agent certificate issuance, renewal or expiry monitoring; no OCSP; agent private key is a plain PEM file; S3 and transcription secrets come only from environment variables; the database authenticates the gateway by password only (no client certificate) | `discovery_and_key_rotation` (`oidc.rs` tests); `revoked_agents_are_refused_after_reload_and_expired_crls_fail_closed`; `secrets_never_in_errors` (proptest), `qa_secrets_absent_from_every_error_variant` (`pg_transport.rs`); `qa_start_logs_transport_verdict_without_secret` (`check_config.rs`, `RUST_LOG=trace`) |
| IA-8 | Identification and Authentication (Non-Organizational Users) | Inherited / deployer | Every user goes through the same OIDC verification (`OidcVerifier::verify`); federation and guest accounts are configured at the IdP | `valid_token_yields_principal` |

## RA: Risk Assessment

| Control | Title | Status | Implementation (file → item) | Test evidence |
|---|---|---|---|---|
| RA-5 | Vulnerability Monitoring and Scanning | Partial | `cargo audit` with a justified ignore and a build-graph guard (`.cargo/audit.toml`); `cargo deny` advisories (`deny.toml`); `npm audit --audit-level=moderate`; fuzzing (`fuzz/fuzz_targets/`, `.github/workflows/fuzz-nightly.yml`). **Missing:** advisory scans run only on push and PR, not on a schedule against released code; no container image scanning | CI `audit`, `deny`, `frontend`, `fuzz-smoke`; `fuzz-nightly` workflow |

## SA: System and Services Acquisition

| Control | Title | Status | Implementation (file → item) | Test evidence |
|---|---|---|---|---|
| SA-8 | Security and Privacy Engineering Principles | Implemented (tested) | `Cargo.toml` `[workspace.lints]` (`unsafe_code = "forbid"`; `unwrap_used`, `expect_used`, `panic` denied); sans-I/O codec (ADR 0001); deny-by-default `NetworkPolicy`; untrusted-input rules in `AGENTS.md` | CI `check` (clippy `-D warnings`); `default_denies_everything` (`net.rs`); `assembler_never_panics` (`crates/uniden-protocol/tests/properties.rs`) |
| SA-10 | Developer Configuration Management | Partial | Git; CI on every push and PR with `permissions: contents: read` (`.github/workflows/ci.yml`); `Cargo.lock` and `--locked` image builds; `CHANGELOG.md`; ADRs in `docs/decisions/`. **Missing:** branch protection and required review are not evidenced in the repository; Actions are referenced by tag, not commit SHA | CI `check` |
| SA-11 | Developer Testing and Evaluation | Implemented (tested) | Unit, integration and property tests; route authorization matrix; PostgreSQL conformance; Playwright e2e (`frontend/e2e`); 8 cargo-fuzz targets. **Limit:** simulator only. No SDS200 hardware has been tested; `tests/interoperability` has not been run | `cargo test --workspace` (CI `check`); `every_route_enforces_its_permissions`; CI `e2e`, `postgres`, `fuzz-smoke` |
| SA-15 | Development Process, Standards, and Tools | Partial | `AGENTS.md` quality gates; `rustfmt.toml`, `clippy.toml`; frontend exact pins (`frontend/.npmrc`). **Missing:** `rust-toolchain.toml` tracks `stable`, and so does CI (`dtolnay/rust-toolchain@stable`); only the Dockerfile pins `RUST_VERSION=1.100.0` | CI `check` (`cargo fmt --check`, clippy) |

## SC: System and Communications Protection

| Control | Title | Status | Implementation (file → item) | Test evidence |
|---|---|---|---|---|
| SC-5 | Denial-of-Service Protection | Implemented (tested) | `state.rs` → `ip_rate_limit` (before auth) and the per-subject limiter in `Auth`; `services/api-gateway/src/lib.rs` → `with_ui` (body limit, 60 s timeout); `services/api-gateway/src/serve.rs` → `serve_with` (connection cap, 10 s TLS handshake timeout); bounded queues (`uniden-sds` `ScannerConfig::queue_depth`, agent `Buffer`, `wire::MAX_MESSAGE_BYTES`); listening limits (`services/api-gateway/src/media.rs` → `Media::listen`, media gateway `max_sessions`) | `rate_limiting`, `per_ip_limit_applies_before_authentication`, `validation_and_pagination` (413) (`api.rs`); `bounded_queue_rejects_overload` (`crates/uniden-sds/tests/simulator.rs`); `sequencing_ack_and_overflow` (`services/scanner-agent/src/buffer.rs`); `session_limit` (`services/media-gateway/tests/webrtc.rs`). Connection cap and handshake timeout are untested; limits are per replica |
| SC-7 | Boundary Protection | Implemented (tested) | Agents dial out, so sites need no inbound ports (ADR 0004); site allow-lists plus agent-local policy (ADR 0005, `NetworkPolicy`); Helm NetworkPolicy, with metrics reachable only from monitoring (`deployments/helm/radio-platform/templates/policy.yaml`); connected UDP socket (`crates/uniden-sds/src/client.rs` → `Scanner::connect`). Scanner-segment isolation is the deployer's | `enrollment_network_policy_prevents_ssrf` (`api.rs`); `full_stack`, `discovery_respects_policy_and_skips_enrolled` (`e2e.rs`); `allows_only_configured_ranges_and_ports`, `rejects_non_unicast_even_if_covered` (`net.rs`). Untested: NetworkPolicy template (only `helm lint`), UDP source filtering |
| SC-8 | Transmission Confidentiality and Integrity | Partial | Gateway listeners and agents: TLS 1.3 only, AWS-LC: `tls.rs` → `server_config`, `mtls_server_config_with_crls`; agent `services/scanner-agent/src/tls.rs` → `client_config`; WebRTC DTLS-SRTP (`services/media-gateway/src/webrtc.rs`, str0m `aws-lc-rs`); transcription over HTTPS off loopback. **Database link (ADR 0012):** sqlx built with `tls-rustls-aws-lc-rs` (`Cargo.toml`), **TLS 1.2 or 1.3** (rustls safe defaults; TLS 1.3-only cannot be enforced through sqlx 0.9, so the server should set `ssl_min_protocol_version`); off loopback the gateway refuses to start unless `sslmode=verify-full` (`crates/platform-storage/src/pg_transport.rs` → `PgTarget::parse`, `PgTarget::check` → `CheckedTarget`); `crates/platform-storage/src/postgres.rs` → `PgStore::connect_target` accepts only a `CheckedTarget` and re-checks it against the URL and sqlx's options (the cluster bus `LISTEN` connection comes from the same pool); `services/api-gateway/src/config.rs` → `GatewayConfig::check_database_transport`. Loopback and Unix-socket databases need no TLS. **Missing:** (1) the scanner segment is plaintext by device design. (2) `http.insecure_plaintext` permits plaintext behind a proxy. (3) Public-listener TLS has no test. (4) `[database] insecure_transport = true` (Helm `database.insecureTransport`) admits a plaintext or unverified database link, warned but not prevented. (5) Database-link residuals in POA&M item 3: `sslrootcert` adds to the public roots, TLS 1.2 accepted, no client certificate, CA rotation needs a restart. (6) The real TLS handshake is evidenced only by a CI test that has not yet run on GitHub Actions | `mtls_rejects_untrusted_and_unregistered_agents` (`e2e.rs`); `browser_receives_opus_frames` (`webrtc.rs` tests); `https_required_off_loopback`. Database policy (no database needed): `loopback_plaintext_allowed`, `unix_socket_allowed`, `remote_verify_full_ok`, `remote_unverified_refused`, `hostaddr_decides_destination`, `not_loopback_edge_cases`, `qa_verify_ca_refused_off_loopback`, `qa_pg_environment_variables` (`pg_transport.rs`); `connect_target_refuses_a_target_from_another_url_before_any_io`, `a_remote_plaintext_target_is_only_connectable_through_check`, `no_production_code_connects_to_postgres_around_the_policy` (`crates/platform-storage/tests/transport_policy_guard.rs`); two `compile_fail` doctests on `CheckedTarget` (`pg_transport.rs`: an unchecked `PgTarget` and a struct literal do not compile); `check_config_refuses_remote_plaintext_database_without_leaking_password`, `check_config_accepts_verify_full_and_loopback` (`check_config.rs`). Real TLS: `postgres_tls_verify_full_with_private_ca` (`postgres_tls.rs`: verify-full with a private CA, `pg_stat_ssl` shows TLS on every backend including `LISTEN`) — **CI `postgres` only, NOT RUN locally, not yet run on GitHub Actions** |
| SC-10 | Network Disconnect | Implemented (untested) | `api/stream.rs` → `pump` (closes at session expiry); 60 s request timeout (`lib.rs` → `with_ui`); `crl.rs` → `CrlReloader::run` disconnects every agent on a CRL change (`AgentHub::disconnect_all`) | None. The CRL test calls `reload_if_changed`, not `run` |
| SC-12 | Cryptographic Key Establishment and Management | Inherited / deployer | The operator runs the agent CA (key offline, `docs/operations.md`), server certificates and IdP keys. Platform: PEM loading (`tls.rs` → `load_certs`, `load_key`, `load_crls`), CRL hot reload, JWKS refresh. Server certificate rotation needs a restart | `discovery_and_key_rotation`; `revoked_agents_are_refused_after_reload_and_expired_crls_fail_closed` |
| SC-13 | Cryptographic Protection | Implemented (tested) | rustls with AWS-LC (`tls.rs` → `provider`; agent `client_config`); JWT verification via `jsonwebtoken` (`aws_lc_rs` feature); audit SHA-256 via `aws_lc_rs::digest` (`audit_chain.rs` → `link`); DTLS via str0m `aws-lc-rs`; OpenSSL and native-tls banned (`deny.toml`). **No FIPS 140 validation is claimed.** AWS-LC is used in its default, non-FIPS build (`aws-lc-rs` without the `fips` feature). PostgreSQL TLS: sqlx feature `tls-rustls-aws-lc-rs` (`Cargo.toml`), so the database link uses rustls with the AWS-LC provider, same non-FIPS build. sqlx's PostgreSQL SCRAM uses RustCrypto, not AWS-LC | `rejects_bad_tokens`; `detects_alteration_removal_and_truncation`; `mtls_rejects_untrusted_and_unregistered_agents`; CI `deny` (no OpenSSL or native-tls with the sqlx TLS feature); `postgres_tls_verify_full_with_private_ca` (CI `postgres` only, not yet run on GitHub Actions) |
| SC-17 | Public Key Infrastructure Certificates | Inherited / deployer | The operator issues agent certificates. Platform: chain validation (`WebPkiClientVerifier` in `mtls_server_config_with_crls`) with strict CRL checking, including expired CRLs. Without `agents.client_crl` there is no revocation check, only a start-up warning (`main.rs`). PostgreSQL server certificate (`sslmode=verify-full`): chain to the compiled-in Mozilla roots (`webpki-roots`) **plus** any CA in `sslrootcert`, and the host name; the operator issues the database certificate. Residual: the public roots stay trusted beside `sslrootcert` (POA&M item 3); no revocation check of database certificates | `revoked_agents_are_refused_after_reload_and_expired_crls_fail_closed`; `mtls_rejects_untrusted_and_unregistered_agents`; `postgres_tls_verify_full_with_private_ca` (wrong CA gives `UnknownIssuer`, name mismatch gives `not valid for name`; CI `postgres` only, not yet run on GitHub Actions) |
| SC-23 | Session Authenticity | Implemented (tested) | TLS 1.3 sessions on the gateway listeners and agents; the off-loopback database link must be `sslmode=verify-full`, which authenticates the server by certificate and name (`pg_transport.rs` → `PgTarget::check`; `verify-ca`, `require` and `prefer` refused); leases bound to their holder (`leases.rs`, atomic `Store::acquire_lease`); agent events de-duplicated on `(agent, session, seq)` (`agent_ws.rs` → `Session::event`; unique key in `migrations/0001_initial.sql`) | `leases_gate_control_commands` (another user's lease refused); `buffered_events_survive_agent_restart` (exactly once); `memory_store_conformance` (one concurrent lease winner); `remote_verify_full_ok`, `remote_unverified_refused`, `qa_verify_ca_refused_off_loopback` (`pg_transport.rs`, policy only); `postgres_tls_verify_full_with_private_ca` for the handshake (CI `postgres` only, not yet run on GitHub Actions) |
| SC-28 | Protection of Information at Rest | Not implemented | The platform does not encrypt at rest. Recordings (`crates/scanner-recorder/src/store.rs` → `RecordingStore`), PostgreSQL data and the agent journal rely on bucket or disk encryption, which the operator configures (`docs/deployment.md`) | — |

## SI: System and Information Integrity

| Control | Title | Status | Implementation (file → item) | Test evidence |
|---|---|---|---|---|
| SI-2 | Flaw Remediation | Partial | `SECURITY.md` (private reporting, acknowledgement within 5 business days); `.github/dependabot.yml` (weekly, 14-day cooldown); CI advisory checks. **Missing:** no remediation timelines | CI `audit`, `deny`, `frontend` |
| SI-4 | System Monitoring | Inherited / deployer | `services/api-gateway/src/metrics.rs` → `track`, `render` (route-template labels); log `audit chain verification failed` (`api/audit.rs` → `verify`); warning when metrics are off loopback (`main.rs`); suggested alerts in `docs/operations.md`. Collection, alerting and a scheduled `/audit/verify` are the operator's; no alert rules ship | `http_metrics_use_route_templates` (`services/api-gateway/tests/metrics.rs`) |
| SI-7 | Software, Firmware, and Information Integrity | Partial | Audit hash chain (`audit_chain.rs`); stored recordings parsed with Ogg CRC and size limits (`crates/scanner-audio/src/ogg.rs` → `read_ogg_opus`; `crates/scanner-transcription/src/audio.rs` → `decode_ogg_opus`); cosign-signed images with provenance (`images.yml`). **Missing:** recordings are not signed, so substitution goes undetected (T46); image signing runs only on release tags and is untested; the chart does not enforce signature verification; base images are pinned by tag | `detects_alteration_removal_and_truncation`; `crc_known_vector`, `reads_back_what_it_writes` (`ogg.rs`); `enforces_limits_and_rejects_garbage` (`scanner-transcription` `audio.rs`) |
| SI-10 | Information Input Validation | Implemented (tested) | `crates/uniden-protocol/src/frame.rs` → `FrameLimits`; `xml/mod.rs` → `XmlLimits`, `parse_documents` (no DTD); `command.rs` → `Arg::new`, `RawCommand::new`; `types.rs` → `KeyCode::new`; RTP/RTSP parsers (`crates/scanner-audio/src/rtp.rs`, `rtsp.rs`); transcript `clean_text` (`crates/scanner-transcription/src/client.rs`); fuzz targets (`fuzz/fuzz_targets/`) | `rejects_dtd_and_enforces_limits` (`frame.rs`); `rejects_dangerous_or_malformed_xml` (`xml/mod.rs`); `key_code_rejects_framing_characters` (`types.rs`); `raw_commands_are_validated_and_state_changing_by_default` (`command.rs`); `invalid_arguments_are_rejected_before_sending` (`uniden-sds` `simulator.rs`); `rejects_malformed` (`rtp.rs`); `rejects_bad_responses` (`rtsp.rs`); `cleans_text`; `validation_and_pagination`; CI `fuzz-smoke` |
| SI-11 | Error Handling | Implemented (untested) | `services/api-gateway/src/error.rs` → `ApiError::internal`, `From<StoreError>` (backend detail logged; generic "internal error" returned); RFC 9457 problem details. Configuration errors: `config::parse_toml` in both services (`services/api-gateway/src/config.rs`, `services/scanner-agent/src/config.rs`) prints the message and `line, column` only, never the source line; database URL errors never contain the URL, user information or query values (`pg_transport.rs` → `PgTransportError`). Residual (`// POAM:` in both `parse_toml`): a serde type or variant mismatch can still name the mis-typed value, for example a URL pasted into `backend` | No test shows that a 500 hides internals. `authentication_is_required` checks only the problem-details shape. Configuration and database-URL errors only: `parse_errors_do_not_echo_the_source_line` (both `config.rs`); `toml_syntax_error_never_echoes_the_source_line`, `qa_check_config_refuses_malformed_urls_without_leaking` (`check_config.rs`); `secrets_never_in_errors` (`pg_transport.rs`) |
| SI-16 | Memory Protection | Partial | `Cargo.toml` `unsafe_code = "forbid"`; systemd `MemoryDenyWriteExecute=yes` (`deployments/systemd/radio-agent.service`). **Missing:** the C code in AWS-LC and libopus is outside `forbid(unsafe)`; the systemd unit is untested | CI `check` (forbid is a compile error); `assembler_never_panics`, `xmlish_never_panics` (`properties.rs`); CI `fuzz-smoke` |

## SR: Supply Chain Risk Management

| Control | Title | Status | Implementation (file → item) | Test evidence |
|---|---|---|---|---|
| SR-3 | Supply Chain Controls and Processes | Implemented (tested) | `deny.toml` (crates.io only; no git sources; OpenSSL and native-tls banned; still true with the sqlx `tls-rustls-aws-lc-rs` feature, whose only new crate is `webpki-roots`); `frontend/.npmrc` (`ignore-scripts=true`, `save-exact=true`); `Cargo.lock` plus `--locked` builds | CI `deny`; CI `frontend` (`npm ci`); CI `audit` |
| SR-4 | Provenance | Partial | `images.yml` → buildx `provenance: mode=max`, `sbom: true`. **Missing:** runs only on `v*` tags, with no CI test; Actions pinned by tag, not SHA; base images pinned by tag, not digest | — (release workflow only) |
| SR-11 | Component Authenticity | Partial | Keyless cosign signatures (`images.yml` → `merge` job); `Cargo.lock` checksums; `frontend/package-lock.json` integrity hashes. **Missing:** the chart does not require verified signatures (`image.digest` optional); signing is untested | CI `frontend` (`npm ci` checks integrity hashes) |

## Gaps / POA&M candidates (highest risk first)

1. **No encryption at rest by the platform (SC-28).** Recordings,
   transcripts, the audit log and the agent journal depend wholly on the
   operator's storage encryption. At minimum, document it as a deployment
   prerequisite and check it; consider envelope encryption for recordings.
2. **Audit failures do not block actions, and nothing alerts on them
   (AU-5).** Add a failure metric and alert, and an optional fail-closed
   mode for privileged actions.
3. **Database transport residuals (SC-8, SC-17, IA-5; ADR 0012).** The
   gateway now uses TLS to PostgreSQL and refuses an off-loopback database
   without `sslmode=verify-full`. What remains, each marked `// POAM:` in
   the code:
   1. `sslrootcert` **adds to** the compiled-in public roots instead of
      replacing them (a sqlx 0.9 limitation), so a public CA that issues a
      certificate for the database's DNS name is trusted. Not reachable for
      private names such as `*.svc.cluster.local`. Follow-up: an upstream
      sqlx option without default roots
      (`crates/platform-storage/src/pg_transport.rs` → `PgTarget::check`).
   2. No client-certificate authentication to PostgreSQL: the database
      authenticates the gateway by password only (`PgTarget::check`).
   3. The link accepts **TLS 1.2 or 1.3**; TLS 1.3 only cannot be enforced
      through sqlx 0.9. Operators should set
      `ssl_min_protocol_version = 'TLSv1.3'` on the server
      (`PgTarget::check`).
   4. The `[database] insecure_transport` opt-out (Helm
      `database.insecureTransport`) admits a plaintext or unverified link;
      it is off by default and warned, not prevented (`PgTarget::check`).
   5. sqlx caches the TLS connector, so rotating the database CA or the
      server certificate to a new CA needs a gateway restart
      (`crates/platform-storage/src/postgres.rs` →
      `PgStore::connect_target`).

   Evidence gap: the real TLS test `postgres_tls_verify_full_with_private_ca`
   and the new CI steps have not yet run on GitHub Actions and were NOT RUN
   locally for the final tree.
4. **Authentication failures are not audited (AU-2).** 401s from bad,
   expired or forged tokens and 429s appear only in metrics. Audit them,
   rate-limited, with source IP.
5. **The audit chain can be rebuilt by a database-level attacker
   (AU-10, AU-9).** Anchoring depends on the operator keeping hourly head
   log lines. Head logging is untested. Consider signed heads or an
   external anchor sink.
6. **No audit capacity management or retention policy (AU-4, AU-11).**
   Records are kept forever with no size metric or archival path.
7. **Agent credential lifecycle (IA-5, SC-12, SC-17).** No issuance,
   renewal or expiry monitoring; no OCSP; no revocation unless
   `agents.client_crl` is set; private keys are plain PEM files; server
   certificate rotation needs a restart.
8. **Supply-chain provenance gaps (SR-4, SR-11, SI-7, SA-10).** Pin
   Actions by SHA and base images by digest; verify signatures at deploy;
   test SBOM and signing outside release tags.
9. **Vulnerability scanning cadence (RA-5, SI-2).** No scheduled advisory
   scan of released code, no image scanning, no npm license check
   (CM-10), no remediation timelines.
10. **Untested security paths (SC-10, SI-11, SC-5, SC-7, AC-12).**
    Stream close at expiry, agent disconnect on CRL change, generic 500
    bodies, connection cap, handshake timeout, UDP source filtering,
    TLS 1.3-only on the public listener, and the Helm NetworkPolicy and
    pod security settings have no tests. Configuration parse errors no
    longer echo the source line, but a serde type or variant mismatch can
    still name the mis-typed value (`// POAM:` in both services'
    `config::parse_toml`).
11. **MFA is not observable (IA-2).** The gateway does not check
    `acr`/`amr`, so it cannot require MFA for privileged roles.
12. **Toolchain not pinned (SA-15).** `rust-toolchain.toml` and CI track
    `stable`.
13. **No hardware verification (SA-11).** Every result is from
    simulators. Run the `tests/interoperability` suite on an SDS200 and
    record the firmware version before relying on device-facing controls
    (SI-10 parsing, audio TEARDOWN and cooldown behaviour).
