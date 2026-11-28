# Architecture decision records

Each record explains one significant design decision: the context, what
was decided, and the consequences. Superseded records stay for history.

| Decision | Status |
|---|---|
| [ADR 0001: Sans-I/O protocol crate](0001-sans-io-protocol-crate.md) | Accepted |
| [ADR 0002: One actor and one in-flight command per scanner](0002-serialized-actor-per-scanner.md) | Accepted |
| [ADR 0003: Track provenance for every command](0003-provenance-tracking.md) | Accepted |
| [ADR 0004: Agents dial out to the gateway over WebSocket with mutual TLS 1.3](0004-agent-dials-out-over-mtls-websocket.md) | Accepted |
| [ADR 0005: The agent is the final enforcement point for scanner networks](0005-agent-enforces-network-policy.md) | Accepted |
| [ADR 0006: Operator control leases are held in gateway memory (Phase 2)](0006-leases-in-gateway-memory.md) | Superseded by ADR 0008 |
| [ADR 0007: Tenant-scoped `Store` trait, PostgreSQL via sqlx 0.9 with static SQL](0007-storage-trait-and-static-sql.md) | Accepted |
| [ADR 0008: Shared leases and a PostgreSQL bus for multiple gateway replicas](0008-shared-leases-and-cluster-bus.md) | Accepted |
| [ADR 0009: Audio pipeline: one RTSP client at the edge, Opus upstream, WebRTC out](0009-audio-pipeline.md) | Accepted |
| [ADR 0010: Transcription, search, retention and analytics](0010-transcription-search-retention.md) | Accepted |
| [ADR 0011: Tamper-evident audit log](0011-tamper-evident-audit.md) | Accepted |
| [ADR 0012: PostgreSQL transport policy (verified TLS off loopback)](0012-postgres-transport-policy.md) | Accepted |
| [ADR 0013: Security event log for authentication failures and rate limits](0013-security-event-log.md) | Accepted |
