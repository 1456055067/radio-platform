# R5: SDS200 firmware release notes

| | |
|---|---|
| **Publisher** | Uniden |
| **Title** | SDS200 firmware release notes |
| **Covers** | Changes between SDS200 firmware versions |
| **On Uniden's site** | [SDS200 Firmware Update Change Log](https://info.uniden.com/twiki/bin/view/UnidenMan4/SDS200FirmwareUpdate). Updates are applied with Uniden's Sentinel software |
| **Status** | **Located but not yet reviewed** by the project |
| **Verification level it would support** | Spec |

## Why it matters

Protocol behaviour can change between firmware releases. Community
findings the project relies on were observed on specific versions, and
hardware results must record the firmware used. The release notes would
show which behaviour changed and when.

## Firmware versions seen in the evidence

| Firmware | Where it appears |
|---|---|
| 1.21.00 | Community C6: RTSP lock-up report |
| 1.23.15 | Community C2: `GLT,SYS` over 40 datagrams; `GSI` latency |
| 1.24.00, 1.26.01 | Community C1: hardware log, RTSP fragility |
| SDS100 1.03.01 | RadioReference command sieve (`GLG`) |

No firmware has been tested by this project yet. The
[hardware sign-off](../../hardware-signoff.md) and
[hardware testing](../../hardware-testing.md) pages say how results and
firmware versions are recorded.

## To do

Obtain the release notes and check them for changes to commands, XML
replies, the network port, keepalive and RTSP behaviour.
