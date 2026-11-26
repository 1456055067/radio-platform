# ADR 0008: Shared leases and a PostgreSQL bus for multiple gateway replicas

- **Status:** Accepted
- **Date:** 2026-11-25
- **Supersedes:** ADR 0006

## Context

ADR 0006 kept control leases in gateway memory, which limited the platform
to one gateway replica, and lost leases on restart. Making replicas
interchangeable behind a load balancer needs more than shared leases:
- each agent holds one WebSocket to **one** replica, but a client's REST
  call or live stream may land on any replica;
- commands must reach the replica that holds the agent;
- live events must reach subscribers on every replica.

## Decision

1. **Leases live in the store.** `Store::acquire_lease` is a single atomic
   upsert. It inserts a new lease, takes over an expired one, or renews the
   caller's own, and refuses everyone else (`scanner_leases`, one row per
   scanner). Release deletes by lease id. `LeaseManager`'s in-memory table
   is removed.
2. **Replicas talk over a message bus** (`platform_storage::MessageBus`):
   - `PgBus` uses `LISTEN`/`NOTIFY` on the gateway's own database, so no
     new infrastructure is needed. Payloads over 7000 bytes go through
     the unlogged `cluster_bus_spill` table.
   - `MemoryBus` runs two replicas in one process for tests.
3. **The bus carries four kinds of message:**
   - **presence:** each replica's connected agents, every 5 s and on
     change, expiring after 15 s;
   - **forwarded agent requests** (commands and discovery) and their
     outcomes;
   - **live updates** from local agents;
   - **configuration pushes.**

   Authorization, lease checks and audit happen on the replica that
   received the client request. The owning replica only relays to its
   agent.
4. Enabled with `[cluster] enabled = true`, which requires PostgreSQL.

## Consequences

- Any number of replicas can serve clients and agents. Leases survive
  restarts.
- **The bus is best effort and at most once.** Messages published while a
  listener reconnects are lost:
  - forwarded commands time out (`504`, with `state_change_unknown` for
    writes);
  - presence heals within 5 s.
- **Ownership is briefly ambiguous after a reconnect.** If an agent moves
  to another replica before the old connection is noticed dead (up to the
  60 s idle limit), commands routed to the old replica fail as unavailable
  until the stale connection closes.
- **Lease expiry uses the replicas' clocks.** Keep replica clocks in sync
  with NTP, as JWT validation already requires.
- **Live traffic adds database load.** Each telemetry or event update
  becomes one `NOTIFY`, roughly 1 kB. For large fleets, measure before
  scaling and lower agent telemetry rates if needed. A dedicated broker
  can replace `PgBus` behind the same trait.
- **The bus is trusted.** Anyone with the gateway's database credentials
  can publish commands. Those credentials already allow rewriting scanner
  assignments, so this adds no new privilege. Protect them accordingly.
- **Rate limits are per replica.** The effective limit is the configured
  limit multiplied by the number of replicas.
