# ADR 0004: Agents dial out to the gateway over WebSocket with mutual TLS 1.3

- **Status:** Accepted
- **Date:** 2026-11-24

## Context

Scanner sites are often behind NAT or carrier-grade NAT, and opening inbound
ports there is undesirable. The agent ↔ gateway channel has three kinds of
traffic:
- request/response commands;
- reliable event delivery;
- best-effort telemetry.

## Decision

- Each agent opens an outbound TLS 1.3 connection to a dedicated gateway
  listener.
- The connection uses rustls with the AWS-LC provider and **requires** a
  client certificate.
- The agent's identity is the certificate's first DNS SAN (or CN). It must
  be registered (`POST /api/v1/agents`) before the gateway accepts the
  WebSocket upgrade.
- Messages are JSON text frames (`platform_common::wire`), each at most
  1 MiB:
  - events carry `(session, seq)` and are acknowledged once durably stored;
  - telemetry is latest-value only;
  - commands are correlated by `RequestId`.

gRPC was considered and rejected for now. It would add protoc and codegen
for a small, slow-changing message set; JSON over WebSocket is easy to
inspect and fuzz. This can be revisited if throughput requires it.

## Consequences

- Sites need no inbound ports.
- A newer connection from the same agent supersedes the older one.
- Certificate issuance and rotation become an operational requirement (see
  `deployment.md`).
