# R3: SDS Remote Command Specification V2.00

| | |
|---|---|
| **Publisher** | Uniden |
| **Title** | SDS remote command specification |
| **Version / date** | V2.00, 2025-07-07 |
| **Covers** | SDS-series scanners; reportedly introduces the `KAL` keepalive command |
| **On Uniden's site** | Not found. Uniden's [SDS200 firmware change log](https://info.uniden.com/twiki/bin/view/UnidenMan4/SDS200FirmwareUpdate) page carries [R6](r6-sds-remote-command-spec-v1-02.md) (V1.02) instead. Cited in the community project `jentfoo/SignalSentinel`, `docs/specs-sds200.md` (C5) |
| **Status** | **Not obtained.** Known only from that third-party summary |
| **Verification level it supports** | Spec (cited) |

## Why it matters

R3 appears to be a newer edition of [R6](r6-sds-remote-command-spec-v1-02.md).
It is the only cited source for the `KAL` keepalive command, which is
**not** in R6 (V1.02).

## What depends on it

| Item | What the project does | Open question |
|---|---|---|
| `KAL` | Implemented and simulated as fire-and-forget; no reply expected | Confirm the command and that it has no reply on SDS200 |
| PSI push keepalive | An optional `KAL` interval, **disabled by default** | SDS100/SDS150 reportedly stop `PSI` pushes about 183 s after the `PSI` command without `KAL`; not shown for SDS200 over UDP (community C1) |
| SDS200 key codes | R6 has BCD536HP and SDS100 columns only | Does V2.00 add an SDS200 column? |

## To do

Ask Uniden for V2.00. If it confirms `KAL` behaviour for the SDS200 over
UDP, decide whether to turn the keepalive on by default.
