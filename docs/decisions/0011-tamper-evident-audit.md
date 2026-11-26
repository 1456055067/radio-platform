# ADR 0011: Tamper-evident audit log

- **Status:** Accepted
- **Date:** 2026-11-26

## Context

The audit log records every operator action, denial, playback, search,
deletion and retention sweep. Its value depends on trusting that nobody
changed or removed records, including someone with database access or an
application bug. Several gateway replicas append concurrently.

## Decision

1. **Hash chain per tenant.** Each record stores `seq` (from 1),
   `prev_hash` and `hash = SHA-256(prev_hash || canonical(record))`, with
   AWS-LC. The canonical form is a JSON array in a fixed field order with
   `details` keys sorted, and the timestamp in whole microseconds, so it
   survives a round trip through PostgreSQL `JSONB` and `TIMESTAMPTZ`.
2. **Atomic, serialized appends.** The record and the per-tenant head
   (`audit_chain_heads`) are written in one transaction holding a row lock
   on the head, so replicas cannot fork the chain.
3. **Append-only table.** A trigger rejects `UPDATE` and `DELETE` on
   `audit_log`. The chain detects changes made by anyone able to bypass
   the trigger.
4. **Verification.** `GET /api/v1/audit/verify` recomputes the whole chain
   and compares its end with the head. Exports (`/audit/export`, JSON
   Lines) include the hashes, so a copy can be verified offline with
   `platform_storage::audit_chain::Verifier`.
5. **Anchoring.** Someone who can rewrite both the records and the head
   can rebuild a valid chain. The gateway therefore logs every tenant's
   head hourly; operators keep those log lines (or periodic exports) where
   the database administrator cannot write.

## Consequences

- Records written before migration 0005 are not chained; verification
  reports how many.
- Appends for one tenant are serialized by a row lock. At the platform's
  audit rates (operator actions) this is not a bottleneck.
- Deleting a tenant now requires disabling the trigger deliberately.
- The audit log is never swept by retention (ADR 0010).
