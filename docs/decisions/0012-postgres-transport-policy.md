# ADR 0012: PostgreSQL transport policy (verified TLS off loopback)

- **Status:** Accepted
- **Date:** 2026-11-26

## Context

Until now the workspace `sqlx` dependency enabled no TLS feature, so the
gateway could not use TLS to PostgreSQL. Database credentials, audit
records, transcripts, lease rows and cluster-bus messages crossed the
network in plaintext, and the Helm chart's advice to use
`sslmode=verify-full` could not work (NIST SC-8, formerly POA&M item 1).

Constraints:
- the TLS stack must be rustls with AWS-LC; `deny.toml` bans OpenSSL and
  native-tls;
- a private CA must be usable;
- loopback and Unix-socket deployments (development, CI, a database on the
  same host) must keep working without TLS;
- the rule must apply to the URL actually used (`DATABASE_URL` overrides
  `[database] url`, and sqlx also reads `PGHOST`, `PGHOSTADDR`,
  `PGSSLMODE` and `PGSSLROOTCERT`), and `--check-config` must check it
  without network or file access;
- the database URL holds the password, so nothing may log or echo it.

How sqlx 0.9.1 behaves, from its sources
(`sqlx-core/src/net/tls/tls_rustls.rs`, `sqlx-postgres/src/options/`):
- `tls-rustls-aws-lc-rs` uses rustls with the AWS-LC provider and the
  rustls **safe default protocol versions: TLS 1.2 and 1.3**. sqlx has no
  setting to require TLS 1.3.
- The root store is always the compiled-in Mozilla bundle (`webpki-roots`).
  `sslrootcert` (a file or inline PEM) is **added** to it; it does not
  replace it, unlike libpq, where `sslrootcert` is the whole trust store.
  `sslrootcert=system` is not understood (sqlx treats it as a file name).
- `verify-full` checks the chain and the host name. `verify-ca` checks the
  chain only. `require` and `prefer` verify nothing, and `prefer` (the
  default when `sslmode` is absent) silently falls back to plaintext.
- The TCP destination is `hostaddr` when it is set, else the host.
- The TLS connector is built once and cached.

## Decision

1. **TLS stack.** Enable the sqlx feature `tls-rustls-aws-lc-rs`. The only
   new crate is `webpki-roots`; no OpenSSL, native-tls or ring enters the
   normal build.
2. **Off loopback, only `sslmode=verify-full` is accepted.** The gateway
   refuses to start, and `--check-config` exits 2, for a non-loopback
   destination with `disable`, `allow`, `prefer`, `require`, `verify-ca`
   or no `sslmode`. Loopback and Unix-socket destinations are always
   accepted (verdict `Local`), whatever the `sslmode`.
3. **`verify-ca` is refused off loopback.** Because the public roots are
   always in the store, `verify-ca` accepts any publicly trusted
   certificate for **any** name. Anyone can get one (for example through
   ACME for a domain they own), so `verify-ca` does not authenticate the
   database. `verify-full` with the same roots still requires a
   certificate for the database's own name.
4. **`hostaddr` decides the destination.** The policy classifies
   `hostaddr` (query or `PGHOSTADDR`) when it is set, because that is where
   sqlx connects. A loopback-looking host with a remote `hostaddr` is
   Remote. A remote host name with `hostaddr=127.0.0.1` is Local; under
   `verify-full` the certificate is still checked against the host name.
5. **Loopback is decided lexically, as libpq does.** Loopback means the
   literal `localhost` (ASCII case-insensitive), an address in
   `127.0.0.0/8`, or `::1` (brackets stripped). Nothing is resolved. Names
   that are not loopback, and so fail closed, include `localhost.`,
   `*.localhost`, `::ffff:127.0.0.1` and any other name, even one that
   resolves to loopback. The literal `localhost` is resolved by the
   operating system (NSS, `/etc/hosts`) at connect time, the same trust
   libpq places in it; a host whose resolver maps `localhost` elsewhere is
   misconfigured.
6. **One parser, checked against sqlx.**
   `platform_storage::pg_transport::PgTarget::parse` parses the URL with
   `url` and the `PG*` variables through an injected closure (no files, no
   DNS). It refuses unknown or duplicate destination and TLS parameters,
   URLs over 8 KiB, more than 32 parameters and `sslrootcert=system`.
   Unknown keys must be refused because sqlx logs unrecognised parameters
   **with their values**. `PgTarget::check` applies the policy and is the
   only constructor of `CheckedTarget`; `PgStore::connect_target` accepts
   only a `CheckedTarget`, re-parses the URL with the process environment
   and compares the result with the target and with sqlx's own
   `PgConnectOptions`, refusing to connect on any difference. The cluster
   bus `LISTEN` connection comes from the same pool, so it uses the same
   TLS settings.
7. **Explicit opt-out.** `[database] insecure_transport = true` (Helm
   `database.insecureTransport: true`) accepts an off-loopback database
   without `verify-full`. It defaults to `false` in the TOML and in Helm.
   When it is used, the gateway logs a warning at every start (host, port
   and sslmode only), `--check-config` prints a warning, and Helm
   `NOTES.txt` shows one. A misspelt key is ignored, so the gateway fails
   closed. The name is "transport", not "plaintext", because it also
   admits encrypted but unverified modes.
8. **No secrets in errors or logs.** Error and log text is built from
   host, port and sslmode only; `PgConnectOptions`, whose `Debug` includes
   the password, is never formatted. Configuration parse errors print only
   the message and the line and column (a serde type or variant mismatch
   can still name the mis-typed value).

## Alternatives considered

- **Require TLS everywhere, including loopback.** Rejected: it breaks the
  development stack, the CI `two_replicas_over_postgres` test and
  Unix-socket deployments, and adds nothing on a link that does not leave
  the host.
- **Accept `verify-ca` (as first proposed), or accept it only when
  `sslrootcert` is set.** Rejected: with additive public roots both forms
  accept a publicly issued certificate for any name (Decision 3).
- **Decide loopback by resolving the host.** Rejected: non-deterministic,
  needs network access in `--check-config`, and can be steered through DNS.
- **Trust `PgConnectOptions` alone.** Rejected: it has no getter for
  `hostaddr`, its parse reads `~/.pgpass`, sqlx logs unknown parameter
  values, and its `Debug` includes the password.
- **`tls-rustls-aws-lc-rs-native-roots`** (the operating system's roots).
  Rejected: on the distroless image it only swaps one public bundle for
  another, adds a dependency and still does not replace the roots with
  `sslrootcert`. `tls-rustls` uses ring and `tls-native-tls` is banned.
- **Helm default `insecureTransport: true` with a warning.** Rejected: the
  project chose fail closed. In-cluster plaintext PostgreSQL must opt in on
  upgrade.

## Consequences

- **Breaking:** a non-loopback `DATABASE_URL` without
  `sslmode=verify-full` now refuses to start. In-cluster PostgreSQL without
  TLS (for example `pg.db.svc.cluster.local`) is not loopback and needs TLS
  or `database.insecureTransport: true`.
- `sslrootcert` adds to the public roots, so a public CA that issues a
  certificate for the database's DNS name is trusted. This needs control
  of a public name and cannot happen for private names such as
  `*.svc.cluster.local`. A follow-up is an upstream sqlx option without
  default roots.
- The link uses TLS 1.2 or 1.3. TLS 1.3 is negotiated with current
  PostgreSQL (the CI test asserts `TLSv1.3` against PostgreSQL 18), but
  only the server can forbid 1.2: set `ssl_min_protocol_version = 'TLSv1.3'`.
- There is no client-certificate authentication to PostgreSQL yet; the
  database authenticates the gateway by password (SCRAM).
- The connector is cached, so a CA change or a server certificate from a
  new CA needs a gateway restart.
- Each new pooled connection pays one TLS handshake, including
  replacements for connections closed after the pool's idle timeout. With
  TLS compiled in, the default `prefer` also sends an `SSLRequest` on
  loopback; against a server without TLS it falls back to plaintext as
  before.
- `PgStore::connect` stays public for tests and the development stack and
  does not apply the policy; a guard test
  (`no_production_code_connects_to_postgres_around_the_policy`) keeps
  production code on `connect_target`.
- Evidence: the policy is tested without a database. The real TLS test
  (`postgres_tls_verify_full_with_private_ca`) runs only in the CI
  `postgres` job and, as of this ADR, has not yet run on GitHub Actions.
