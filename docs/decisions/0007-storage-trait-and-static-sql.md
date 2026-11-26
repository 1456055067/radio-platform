# ADR 0007: Tenant-scoped `Store` trait, PostgreSQL via sqlx 0.9 with static SQL

- **Status:** Accepted
- **Date:** 2026-11-24

## Decision

- All persistence goes through `platform_storage::Store`.
  - Every tenant-data method takes a `TenantId`, so tenant isolation is
    enforced at the storage boundary as well as by ABAC.
  - `PgStore` is the production backend.
  - `MemoryStore` serves tests and development.
  - One conformance suite runs against both; the PostgreSQL run uses a
    throw-away database.
- sqlx 0.9 rejects dynamically built SQL strings at compile time. Every
  query is a literal or a `concat!` of literals; values are always bound
  parameters. Optional filters use `QueryBuilder::push_bind`.
- Migrations are embedded (`sqlx::migrate!("../../migrations")`) and applied
  at gateway start-up.

## Consequences

- Fast, deterministic API tests run without a database.
- SQL injection through query construction is prevented by the type system.
