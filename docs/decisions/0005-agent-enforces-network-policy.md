# ADR 0005: The agent is the final enforcement point for scanner networks

- **Status:** Accepted
- **Date:** 2026-11-24

## Context

Enrolling a scanner tells an agent to send UDP to an address. Without
controls, this is an SSRF primitive: an attacker could point it at cloud
metadata endpoints, internal services or other hosts on the agent's LAN.

## Decision

The policy is checked twice:
1. The gateway checks each enrollment against the site's
   `allowed_networks` and the configured ports. A site may not authorize
   `/0`.
2. Each agent checks every assignment against its **local**
   `[network]` policy before sending any packet. Unspecified, multicast and
   broadcast addresses are always refused. IPv4-mapped IPv6 addresses are
   canonicalised first.

## Consequences

- A compromised gateway or database cannot direct agents outside their
  authorized ranges. The end-to-end test `full_stack` verifies this.
- Operators must keep the agent's local policy in step with the site
  policy.
