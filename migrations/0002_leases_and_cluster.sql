-- Phase 2 completion: shared control leases and the multi-replica bus.

-- Operator control leases, shared by all gateway replicas. One row per
-- scanner; acquisition and renewal are a single atomic upsert.
CREATE TABLE scanner_leases (
    scanner_id  UUID PRIMARY KEY REFERENCES scanners(id) ON DELETE CASCADE,
    tenant_id   UUID NOT NULL,
    lease_id    UUID NOT NULL,
    holder      TEXT NOT NULL CHECK (length(holder) BETWEEN 1 AND 512),
    acquired_at TIMESTAMPTZ NOT NULL,
    expires_at  TIMESTAMPTZ NOT NULL
);
CREATE INDEX scanner_leases_expiry ON scanner_leases (expires_at);

-- Cluster bus messages too large for a NOTIFY payload (8000 bytes). The
-- notification carries only the row id. Rows are short-lived and purged.
CREATE UNLOGGED TABLE cluster_bus_spill (
    id         BIGSERIAL PRIMARY KEY,
    payload    TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
