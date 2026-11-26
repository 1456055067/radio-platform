# ADR 0006: Operator control leases are held in gateway memory (Phase 2)

- **Status:** Superseded by ADR 0008 (2026-11-25)
- **Date:** 2026-11-24

## Context

Each physical scanner is a single receiver: exactly one operator may
control it at a time, while any number of users observe. Leases are
short-lived (default 5 minutes, configurable maximum).

## Decision

`scanner_controller::LeaseManager` keeps leases in the gateway process.
- State-changing commands require `X-Lease-Id` and are checked against the
  lease holder.
- Read-only commands need no lease.
- Administrators (`scanner_admin`) may revoke any lease.

## Consequences

- Leases are lost on gateway restart. Operators simply re-acquire them;
  scanner state is unaffected.
- **Only a single gateway replica is supported** until leases move to
  PostgreSQL, using advisory locks or a `leases` table with expiry. This is
  tracked in the roadmap.
