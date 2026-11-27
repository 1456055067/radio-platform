# R6: SDS100 / SDS200 Remote Command Specification v1.02

| | |
|---|---|
| **Publisher** | Uniden |
| **Title** | *SDS100 / SDS200 Remote Command Specification* |
| **Version / date** | Version 1.02, 2023-12-22 |
| **History** | 0.01 (2018-04-13): created, based on the BCD536HP/BCD436HP Remote Command Specification v1.05 · 1.01 (2023-12-11): added Waterfall · 1.02 (2023-12-22): added `GST` |
| **Covers** | SDS100 and SDS200 |
| **On Uniden's site** | [SDS200_RemoteCommand_Specification_V1_02.pdf](https://info.uniden.com/twiki/pub/UnidenMan4/SDS200FirmwareUpdate/SDS200_RemoteCommand_Specification_V1_02.pdf), attached to the [SDS200 firmware change log](https://info.uniden.com/twiki/bin/view/UnidenMan4/SDS200FirmwareUpdate) page |
| **Status** | **Obtained and read** (2026-11-27). 38 pages |
| **Verification level it supports** | Spec |

## Why it matters

R6 is the first official command specification for the SDS200 that the
project holds. It replaces [R2](r2-sds100-remote-command-spec.md), whose
v0.01 is R6's first edition. For most commands it confirms what the SDK
already took from [R1](r1-bcdx36hp-remote-command-spec.md).

## What it confirms

- **The command set.** 30 commands: `MDL VER KEY QSH STS JNT NXT PRV FQK
  SQK DQK PSI GSI GLT HLD AVD SVC JPM DTM LCR AST APR URC MNU MSI MSV MSB
  GST PWF GWF`. Syntax matches R1 for everything the SDK types.
- **The model string.** `MDL` returns `SDS100` or `SDS200`.
- **The `ScannerInfo` XML** (GSI/PSI) element and attribute sheets, which
  match the SDK's model.
- **The key-code table** for `KEY`: `M F L 0-9 . E > < ^ V Q Y A B C Z T R`.
- **Error replies.** `NG` (for example from `JPM` in some modes) and
  `URC,ERR,code`.

## What is new compared with R1

| Item | Detail |
|---|---|
| `GST` | "Get scanner status (for WF)": display lines plus mute, waterfall mode, marker, centre/lower/upper frequency and FFT size |
| `PWF`, `GWF` | Waterfall FFT push and get (240 points) |
| `JPM,WF_MODE` | Jump to waterfall mode |
| `ScannerInfo` additions | `Property@F`, `Battery`, `A_Led`, `Dir`; `WaterfallBand`, `WaterfallSettings`; channel `RecSlot`, `LVL`, `IFX`; `U_Id` on channel and discovery elements |
| `STS` | The last nine fields are all reserved (R1 named two of them `BK_COLOR`, `BK_DIMMER`) |
| `BFH` | Not in R6's command list |

## What it does not settle

- **The SDS200 key mapping.** The key-code sheet has BCD536HP and SDS100
  columns only. On the SDS100, `Q` and `T` have no key and `V` is
  Backlight. `KEY_MODE` values are not listed.
- **Units and encodings.** `QSH` frequency units, the `PSI` interval unit
  and whether `JPM`'s `0xFFFFFFFF` index is sent as decimal.
- **`KAL` keepalive.** Not in R6; see [R3](r3-sds-remote-command-spec-v2.md).
- **`VOL`, `SQL`, `POF`, `GLG`, `MSM`, `PWR`.** Not in R6; still community
  reports only.
- **Network transport.** R6 refers to the serial command set; the network
  side is in [R4](r4-sds200-virtual-serial-and-rtsp.md).
- **Current firmware.** R6 dates from December 2023. Later firmware (see
  [R5](r5-sds200-firmware-release-notes.md)) may differ.

## Follow-ups for the code

- `Provenance::PredecessorSpec` in `uniden-protocol` still names only R1.
- The SDK reads unit IDs from a `UnitID` element (community C1), not from
  the `U_Id` attribute R6 documents on channel elements.
- `GST`, `PWF` and `GWF` are raw-only.

## See also

- [Protocol compliance → Commands](../../protocol-compliance.md#commands)
- [Protocol compliance → ScannerInfo](../../protocol-compliance.md#scannerinfo-gsi-psi)
