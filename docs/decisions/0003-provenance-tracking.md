# ADR 0003: Track provenance for every command

- **Status:** Accepted
- **Date:** 2026-11-24

## Context

The official SDS-series specifications could not be obtained during
Phase 1. The only official document available is the predecessor BCDx36HP
v1.03 specification. Commands such as `VOL`, `SQL` and `POF`, which the
SDK needs, are only community-reported.

## Decision

Every `Command` exposes a `Provenance`: `PredecessorSpec`,
`CommunityReported` or `Raw`. `docs/protocol-compliance.md` cites the
source for each. Capability probing decides at runtime whether a command
works on a given scanner.

## Consequences

- Callers can tell which commands are documented and which are
  community-reported.
- Provenance values must be updated as official documents and hardware
  results arrive.
