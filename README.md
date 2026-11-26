# radio-platform

An open-source platform for managing Uniden SDS200 scanners over their
native Ethernet protocols. It needs no ProScan, Sentinel, USB or
proprietary middleware.

> **Status:** implemented so far:
> - Phase 1, the protocol SDK;
> - the core of Phase 2, distributed management: site agents connect to a
>   central API gateway over mutual TLS 1.3, and the gateway has OIDC,
>   ABAC, leases, audit and PostgreSQL;
> - Phase 3's audio: live listening over WebRTC and transmission
>   recording, both off by default;
> - Phase 4's web console;
> - Phase 5: Whisper-compatible transcription, transcript search,
>   retention and analytics, all off by default;
> - Phase 6 hardening: authorization matrix tests, a tamper-evident audit
>   log, metrics, container images, a Helm chart and operations docs.
>
> Hardware sign-off is pending; see
> [docs/hardware-signoff.md](docs/hardware-signoff.md) and
> [docs/roadmap.md](docs/roadmap.md). Transcription is tested only against
> a stub service. **No feature has been verified on
> SDS200 hardware yet.** Everything is tested against the bundled simulator.

## What works today

| Component | Path | Description |
|---|---|---|
| `uniden-protocol` | `crates/uniden-protocol` | Sans-I/O codec: typed commands, datagram and multi-part XML reassembly, safe XML, typed `ScannerInfo` (GSI/PSI) and `GLT` lists. |
| `uniden-sds` | `crates/uniden-sds` | Async Tokio SDK. Runs one actor per scanner and provides serialized commands, retries for read-only commands only, a bounded queue, health tracking, PSI push subscription and capability probing. |
| `sds-simulator` | `tests/simulator` | A UDP simulator of SDS200 command behaviour with fault injection. |
| Hardware tests | `tests/interoperability` | Opt-in tests against a real scanner. |
| Fuzzing | `fuzz` | cargo-fuzz targets for every untrusted parser. |
| `scanner-agent` | `services/scanner-agent` | Site agent: manages local scanners, detects transmissions, buffers during outages, dials the gateway over mTLS. |
| `api-gateway` | `services/api-gateway` | REST v1 + live WebSocket (OIDC/ABAC), mTLS agent listener, leases, audit, OpenAPI. |
| Shared crates | `crates/platform-*`, `crates/scanner-*` | Auth, storage (PostgreSQL), telemetry, controller, wire types. |
| `scanner-audio` | `crates/scanner-audio` | RTSP/RTP client, jitter buffer, µ-law → Opus, Ogg Opus. |
| `media-gateway` | `services/media-gateway` | WebRTC (WHEP) fan-out of live audio. |
| `scanner-recorder` | `crates/scanner-recorder` | Transmission segmentation and recording to S3-compatible storage. |
| `scanner-transcription` | `crates/scanner-transcription` | Recording decode and a client for Whisper-compatible transcription services. |
| Web console | `frontend` | React + Cloudscape: dashboard, virtual scanner, device administration, transmission explorer with transcripts, analytics, events, audit. |
| `radio-devstack` | `tests/devstack` | **Development only.** Simulators, agents, gateway and a test token issuer in one process, so the console runs without hardware. |

## Quick start

```bash
cargo test --workspace
cargo run -p sds-simulator -- --bind 127.0.0.1:50536 --drop 0.05 --duplicate 0.05
```

To try the console against simulated scanners, build the UI and start the
devstack, then open http://127.0.0.1:8080 and pick a role:

```bash
(cd frontend && npm ci && npm run build)
cargo run -p radio-devstack -- --ui-dir frontend/dist
```

```rust,no_run
use uniden_sds::{Scanner, ScannerConfig};

# async fn demo() -> uniden_sds::Result<()> {
let scanner = Scanner::connect(ScannerConfig::new("127.0.0.1:50536".parse().expect("addr"))).await?;
let info = scanner.scanner_info().await?;
println!("{:?} {:?} {:?}", info.mode, info.active_frequency(), info.active_tgid());
# Ok(()) }
```

## Protocol knowledge

[docs/protocol-compliance.md](docs/protocol-compliance.md) is the
authoritative record of each command. For every command it gives the
source (official spec or community report), whether the code implements
it, whether the simulator supports it, and whether it has been verified on
hardware.

## Security

The scanner protocol is unauthenticated and unencrypted. **Never expose UDP
50536 or TCP 554 to the Internet.** See [SECURITY.md](SECURITY.md) and
[docs/security-architecture.md](docs/security-architecture.md).
[docs/nist-800-53-controls.md](docs/nist-800-53-controls.md) maps the
platform to NIST SP 800-53 Rev 5 controls, with evidence and open gaps;
it is not a compliance claim.

## Legal

Only monitor and record communications you are legally authorized to
receive and retain. This project does not and will not implement
decryption of protected communications.

Licensed under the [Apache License 2.0](LICENSE).
