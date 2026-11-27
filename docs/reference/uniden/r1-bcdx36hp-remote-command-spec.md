# R1: BCD436HP / BCD536HP Remote Command Specification v1.03

| | |
|---|---|
| **Publisher** | Uniden |
| **Title** | *BCD436HP (UB376Z) / BCD536HP (UB375Z) Remote Command Specification* |
| **Version / date** | Version 1.03, 2015-10-29 |
| **Covers** | The BCD436HP handheld and BCD536HP base scanners: the published predecessor of the SDS series protocol |
| **On Uniden's site** | [BCDx36HP_RemoteCommand_Specification_V1_03.pdf](https://info.uniden.com/twiki/pub/UnidenMan4/BCD536HPFirmwareUpdate/BCDx36HP_RemoteCommand_Specification_V1_03.pdf) |
| **Status** | **Obtained and read.** The only official Uniden document the project holds. |
| **Verification level it supports** | Spec |

## Why it matters

R1 describes the protocol the SDS200's predecessors used. It was the
project's only official document until 2026-11-27, so it is the original
basis for the SDK's command syntax, reply formats and `ScannerInfo` XML
model (`Provenance::PredecessorSpec` in the code).

The SDS200's own specification, [R6](r6-sds-remote-command-spec-v1-02.md),
was created from a later edition of this document (v1.05) and agrees with
R1 for every command the SDK types. R6 now takes precedence; R1 remains
cited where R6 is silent. Every fact still needs confirming on SDS200
hardware.

## What the project takes from R1

**Framing**

- Requests end in a carriage return (`\r`).
- Replies are either one line (`NAME,field...\r`) or an XML reply
  (`NAME,<XML>,\r<?xml ...?>...`), from R1's `GLT` and `MSI` examples.
- Error replies: `CMD,NG` (valid but not possible now, as in `JPM`) and
  `CMD,ERR,code` (as in `URC`).

**Commands with syntax from R1**

| Command | Purpose | Notes |
|---|---|---|
| `MDL`, `VER` | Model and firmware version | R1 lists BCD models; the SDS200 model string is from the community |
| `KEY` | Press a front-panel key | Syntax only; R1's copy has no full key table (see [R2](r2-sds100-remote-command-spec.md)) |
| `QSH` | Quick search hold on a frequency | Unit (100 Hz) inferred from R1's `AVD` example; needs hardware check |
| `STS` | Display status | R1 advises "PSI is better than STS" |
| `JNT` | Jump to a number tag | Ranges enforced from R1 |
| `NXT`, `PRV` | Next / previous | Raw only |
| `FQK`, `SQK`, `DQK` | Quick-key states | |
| `PSI`, `GSI` | Pushed and polled scanner info (XML) | Existence and interval from R1; `PSI,<ms>` details are community |
| `GLT` | Lists (favorites, systems, departments, sites, channels, …) | Multi-part footer behaviour is community |
| `HLD`, `AVD`, `JPM` | Hold, avoid, jump mode | `JPM` index `0xFFFFFFFF` encoding unverified |
| `DTM`, `LCR` | Date/time, location | Read only in the SDK |
| `URC` | User recording control | Error codes 0001–0004 |
| `AST`, `APR`, `MNU`, `MSI`, `MSV`, `MSB`, `SVC`, `BFH` | Analyse mode, menus, service types, band scope | Raw only |

**`ScannerInfo` model (GSI / PSI)**

Element and attribute names come from R1's "PSI, GSI Element/Attribute"
sheets: `ScannerInfo@Mode/V_Screen`, `MonitorList` (including
`DB_Counter`, which invalidates indexes when it changes), `System`,
`Department`, `Site`, the channel elements, `SiteFrequency`, `Property`
(volume, squelch, signal, and others) and `ViewDescription` (including
popup buttons and the key code each sends).

R1 also gives the ranges used to bound `VOL` (0–29 or 0–15) and `SQL`
(0–19 or 0–15), even though the `VOL` and `SQL` commands themselves are
not in R1.

## What R1 does not settle

- The key-code table (now in [R6](r6-sds-remote-command-spec-v1-02.md),
  for the SDS100).
- `KAL` keepalive (from [R3](r3-sds-remote-command-spec-v2.md)).
- Network transport (UDP port 50536) and audio over RTSP (now in
  [R4](r4-sds200-virtual-serial-and-rtsp.md)).
- Any discovery mechanism: none appears in R1, R4 or R6.
- `GST` (now in R6).
- Commands the SDK uses that are community-only, in R6 too: `VOL`, `SQL`,
  `POF`, `MSM`, `GLG`.

## See also

- [Protocol compliance → Commands](../../protocol-compliance.md#commands)
- [Protocol compliance → ScannerInfo](../../protocol-compliance.md#scannerinfo-gsi-psi)
