# ADR 0001: Sans-I/O protocol crate

- **Status:** Accepted
- **Date:** 2026-11-24

## Context

Protocol parsing must be fuzzable, property-tested and reusable by the
simulator, the agent and other tools, independent of the async runtime.

## Decision

`uniden-protocol` performs no I/O and has no async code. It contains:
- typed commands;
- the frame assembler;
- XML parsing;
- reply decoding.

`uniden-sds` adds Tokio networking on top.

## Consequences

- Parsers can be fuzzed directly.
- The simulator is written independently of the codec, so agreement
  between the two is a meaningful test rather than a tautology.
