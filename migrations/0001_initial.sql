-- Phase 2 initial schema. Every table is tenant-scoped; metadata the scanner
-- does not report is nullable.

CREATE TABLE tenants (
    id          UUID PRIMARY KEY,
    name        TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 128),
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE sites (
    id          UUID PRIMARY KEY,
    tenant_id   UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    name        TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 128),
    description TEXT CHECK (length(description) <= 1024),
    latitude    DOUBLE PRECISION CHECK (latitude BETWEEN -90 AND 90),
    longitude   DOUBLE PRECISION CHECK (longitude BETWEEN -180 AND 180),
    -- Authorized scanner networks for this site (SSRF control at enrollment).
    allowed_networks TEXT[] NOT NULL DEFAULT '{}',
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, name)
);

CREATE TABLE agents (
    id           TEXT PRIMARY KEY CHECK (id ~ '^[A-Za-z0-9._-]{1,128}$'),
    tenant_id    UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    site_id      UUID NOT NULL REFERENCES sites(id) ON DELETE CASCADE,
    description  TEXT,
    last_seen_at TIMESTAMPTZ,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE scanners (
    id          UUID PRIMARY KEY,
    tenant_id   UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    site_id     UUID NOT NULL REFERENCES sites(id) ON DELETE CASCADE,
    agent_id    TEXT NOT NULL REFERENCES agents(id) ON DELETE RESTRICT,
    name        TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 128),
    host        INET NOT NULL,
    port        INTEGER NOT NULL CHECK (port BETWEEN 1 AND 65535),
    enabled     BOOLEAN NOT NULL DEFAULT TRUE,
    model       TEXT,
    firmware    TEXT,
    health      JSONB,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (agent_id, host, port)
);
CREATE INDEX scanners_tenant_site ON scanners (tenant_id, site_id);

CREATE TABLE scanner_groups (
    id          UUID PRIMARY KEY,
    tenant_id   UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    name        TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 128),
    UNIQUE (tenant_id, name)
);

CREATE TABLE scanner_group_members (
    group_id    UUID NOT NULL REFERENCES scanner_groups(id) ON DELETE CASCADE,
    scanner_id  UUID NOT NULL REFERENCES scanners(id) ON DELETE CASCADE,
    PRIMARY KEY (group_id, scanner_id)
);

CREATE TABLE telemetry_latest (
    scanner_id  UUID PRIMARY KEY REFERENCES scanners(id) ON DELETE CASCADE,
    observed_at TIMESTAMPTZ NOT NULL,
    snapshot    JSONB NOT NULL
);

CREATE TABLE events (
    id            BIGSERIAL PRIMARY KEY,
    tenant_id     UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    site_id       UUID NOT NULL,
    scanner_id    UUID NOT NULL,
    agent_id      TEXT NOT NULL,
    agent_session UUID NOT NULL,
    agent_seq     BIGINT NOT NULL CHECK (agent_seq > 0),
    kind          TEXT NOT NULL,
    payload       JSONB NOT NULL,
    received_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- Replayed events after an agent reconnect are de-duplicated here.
    UNIQUE (agent_id, agent_session, agent_seq)
);
CREATE INDEX events_tenant_id ON events (tenant_id, id DESC);
CREATE INDEX events_scanner ON events (scanner_id, id DESC);

CREATE TABLE transmissions (
    id              UUID PRIMARY KEY,
    tenant_id       UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    site_id         UUID NOT NULL,
    scanner_id      UUID NOT NULL REFERENCES scanners(id) ON DELETE CASCADE,
    started_at      TIMESTAMPTZ NOT NULL,
    ended_at        TIMESTAMPTZ,
    duration_ms     BIGINT CHECK (duration_ms >= 0),
    frequency_hz    BIGINT CHECK (frequency_hz > 0),
    tgid            TEXT,
    unit_id         TEXT,
    system_name     TEXT,
    department_name TEXT,
    channel_name    TEXT,
    modulation      TEXT,
    signal_bars     SMALLINT,
    rssi            REAL,
    -- Phase 3/5: populated only when recording/transcription is enabled.
    audio_object    TEXT,
    transcript      TEXT
);
CREATE INDEX transmissions_tenant_started ON transmissions (tenant_id, started_at DESC, id DESC);
CREATE INDEX transmissions_scanner_started ON transmissions (scanner_id, started_at DESC);
CREATE INDEX transmissions_frequency ON transmissions (tenant_id, frequency_hz);
CREATE INDEX transmissions_tgid ON transmissions (tenant_id, tgid);

CREATE TABLE audit_log (
    id            BIGSERIAL PRIMARY KEY,
    at            TIMESTAMPTZ NOT NULL DEFAULT now(),
    tenant_id     UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    actor         TEXT NOT NULL,
    action        TEXT NOT NULL,
    resource_type TEXT NOT NULL,
    resource_id   TEXT,
    outcome       TEXT NOT NULL CHECK (outcome IN ('success', 'denied', 'failure')),
    details       JSONB NOT NULL DEFAULT '{}'::jsonb,
    source_ip     INET
);
CREATE INDEX audit_tenant_id ON audit_log (tenant_id, id DESC);
