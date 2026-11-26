# Security Policy

## Reporting a vulnerability

Report vulnerabilities through GitHub private vulnerability reporting
("Report a vulnerability" on the repository's Security tab). Do not open
public issues for vulnerabilities. Include affected versions, a
reproduction and impact. Expect an acknowledgement within 5 business days.

## Scope and known properties

- **The native scanner protocol is untrusted, unauthenticated and
  unencrypted.**
  - Anyone who can reach UDP 50536 or TCP 554 on a scanner can control it
    or listen to it.
  - Isolate scanners on a dedicated network segment that only the scanner
    agent can reach.
  - Never port-forward these ports.
- All data received from scanners is parsed with size limits. XML is
  parsed without DTD support. Fuzz targets cover every parser.
- The SDK accepts UDP datagrams only from the configured scanner address,
  using a connected socket. This guards against injection from other
  hosts. It does **not** protect against IP spoofing on the local segment.
- Platform communications (Phase 2 onward) will use TLS 1.3 through
  rustls with the AWS-LC provider, with mutual TLS between agents and
  central services.
- **No FIPS compliance is claimed.** A claim would require a validated
  module, version, configuration and operating environment.

## Supply chain

- Run `cargo audit` and dependency review in CI.
- SBOMs will be generated for container images (Phase 6).
