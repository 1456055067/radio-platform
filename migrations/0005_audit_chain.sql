-- Phase 6: tamper-evident audit log (ADR 0011). Each tenant's records form
-- a SHA-256 hash chain; the head is kept per tenant. Records written before
-- this migration stay unchained (seq NULL).
ALTER TABLE audit_log
    ADD COLUMN seq       BIGINT CHECK (seq > 0),
    ADD COLUMN prev_hash BYTEA CHECK (octet_length(prev_hash) = 32),
    ADD COLUMN hash      BYTEA CHECK (octet_length(hash) = 32);
CREATE UNIQUE INDEX audit_tenant_seq ON audit_log (tenant_id, seq) WHERE seq IS NOT NULL;
CREATE INDEX audit_tenant_at ON audit_log (tenant_id, at);

CREATE TABLE audit_chain_heads (
    tenant_id UUID PRIMARY KEY REFERENCES tenants(id) ON DELETE CASCADE,
    seq       BIGINT NOT NULL CHECK (seq >= 0),
    hash      BYTEA NOT NULL CHECK (octet_length(hash) = 32)
);

-- The application never changes or deletes audit records. This trigger
-- turns an accidental or injected UPDATE/DELETE into an error; the hash
-- chain detects changes made by anyone able to bypass it.
CREATE FUNCTION audit_log_append_only() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'audit_log is append-only';
END;
$$;
CREATE TRIGGER audit_log_append_only BEFORE UPDATE OR DELETE ON audit_log
    FOR EACH ROW EXECUTE FUNCTION audit_log_append_only();
