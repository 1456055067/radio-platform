# ADR 0002: One actor and one in-flight command per scanner

- **Status:** Accepted
- **Date:** 2026-11-24

## Context

The UDP protocol has no request identifiers, so replies can only be
correlated by command name. Community clients also send commands one at a
time.

## Decision

Each `Scanner` owns a single worker task and a connected UDP socket, fed by
a bounded mpsc queue. The worker:
- drains stale datagrams before each send;
- accepts a frame only if it names the in-flight command (or is a bare
  error);
- discards answering frames that fail to decode until the deadline passes;
- retries only `Effect::ReadOnly` commands.

## Consequences

- Correlation is sound.
- Throughput is limited to one round trip at a time per scanner. This is
  acceptable because the device is a single receiver.
- A bare `ERR` that arrives late could be attributed to the next command.
  The drain step reduces this risk but cannot eliminate it.
