# Install

Install in this order: database, gateway, console, then one site at a
time. Validate every configuration file before starting a service:

```bash
api-gateway --check-config gateway.toml
scanner-agent --check-config agent.toml
```

## 1. Try it without hardware (optional)

The development stack runs simulated scanners, agents, the gateway and a
test token issuer in one process, so you can explore the console first:

```bash
(cd frontend && npm ci && npm run build)
cargo run -p radio-devstack -- --ui-dir frontend/dist
```

Open <http://127.0.0.1:8080> and pick a role.

!!! danger "Development only"
    `radio-devstack` hands out tokens for any role and binds to loopback
    only. Never package or deploy it.

## 2. Gateway

Choose one packaging:

=== "Kubernetes (Helm)"

    1. Create the secrets the chart expects: TLS, agent CA and database
       (plus `secrets.databaseCa` for a private database CA). See
       `deployments/helm/radio-platform/values.yaml`.
    2. Put your settings in `config` as TOML. The chart owns the listener,
       TLS, database, cluster, metrics and media tables.
    3. Install:

        ```bash
        helm install radio deployments/helm/radio-platform -f my-values.yaml
        ```

    More than one replica turns on cluster mode. Live audio is supported
    with one replica only.

=== "Container"

    `deployments/container/Dockerfile` builds a `gateway` image (with the
    console at `/usr/share/radio-platform/ui`). Published images are signed
    with cosign; verify and pin by digest:

    ```bash
    cosign verify ghcr.io/<owner>/radio-gateway@<digest> \
      --certificate-identity-regexp '^https://github.com/<owner>/' \
      --certificate-oidc-issuer https://token.actions.githubusercontent.com
    ```

=== "From source"

    ```bash
    cargo build --release -p api-gateway
    DATABASE_URL='postgres://…?sslmode=verify-full&sslrootcert=…' \
      target/release/api-gateway gateway.toml
    ```

    Building needs `cmake` and a C compiler (libopus is compiled from
    source).

Start from `deployments/examples/gateway.example.toml`. Check
`GET /healthz` and `GET /readyz` once it is running.

## 3. Web console

If you use the container image the console is already included. Otherwise:

1. Build it with Node ≥ 22.12: `cd frontend && npm ci && npm run build`.
2. Copy `frontend/dist` to the gateway host.
3. Add `[http.ui]` (directory) and `[http.ui.auth]` (OIDC authority and
   client ID) to the gateway configuration.

The console is served from the API's own origin, so no CORS configuration
is needed.

## 4. A site

For each site:

1. **Create the site** in the console (or `POST /api/v1/sites`), with
   `allowed_networks` covering the scanner segment.
2. **Register the agent ID** (the certificate's DNS SAN) to that site
   (`POST /api/v1/agents`).
3. **Install the agent** on the site host:
    - as a service with `deployments/systemd/radio-agent.service`
      (sandboxed, state in `/var/lib/radio-agent`), or
    - with the `agent` container image.

    Start from `deployments/examples/agent.example.toml`. Set `state_dir`
    so events survive restarts, and set `[network] allowed_networks` to
    the scanner segment only.
4. **Enroll scanners** in the console, or use discovery on the site to
   find SDS200s in an approved range. The agent picks up assignments
   immediately.

Check the scanner appears online on the dashboard. Then run the
[hardware sign-off](../hardware-signoff.md).
