# Uniden reference documents

These are the official Uniden documents that Radio Platform's protocol
work cites. Each has its own page saying what it is, whether the project
has read it, and which behaviour depends on it.

The IDs (R1–R6) are the ones used throughout
[Protocol compliance](../../protocol-compliance.md), which remains the
authoritative record. These pages summarise it; they do not reproduce
Uniden's documents.

| ID | Document | On Uniden's site | Read by the project? | Main use in this project |
|---|---|---|---|---|
| [R6](r6-sds-remote-command-spec-v1-02.md) | SDS100 / SDS200 Remote Command Specification, v1.02 (2023-12-22) | [PDF](https://info.uniden.com/twiki/pub/UnidenMan4/SDS200FirmwareUpdate/SDS200_RemoteCommand_Specification_V1_02.pdf) | **Yes** (2026-11-27) | The SDS200's own command set, replies, `ScannerInfo` XML and key codes |
| [R4](r4-sds200-virtual-serial-and-rtsp.md) | SDS200 *Virtual Serial on Network* specification v1.00, and RTSP notes | [Virtual Serial PDF](https://info.uniden.com/twiki/pub/UnidenMan4/SDS200FirmwareUpdate/SDS200_Virtual_Serial_on_Network_Specification_V1_00.pdf) · [RTSP PDF](https://info.uniden.com/twiki/pub/UnidenMan4/SDS200FirmwareUpdate/RTSP.pdf) | **Yes** (2026-11-27) | UDP port 50536, multi-part XML replies, the RTSP audio session |
| [R1](r1-bcdx36hp-remote-command-spec.md) | BCD436HP / BCD536HP Remote Command Specification, v1.03 (2015-10-29) | [PDF](https://info.uniden.com/twiki/pub/UnidenMan4/BCD536HPFirmwareUpdate/BCDx36HP_RemoteCommand_Specification_V1_03.pdf) | **Yes** | The predecessor protocol; the original basis for the SDK |
| [R5](r5-sds200-firmware-release-notes.md) | SDS200 firmware release notes | [Change log](https://info.uniden.com/twiki/bin/view/UnidenMan4/SDS200FirmwareUpdate) | Not yet | Firmware-specific behaviour changes |
| [R3](r3-sds-remote-command-spec-v2.md) | SDS remote command specification V2.00 (2025-07-07) | Not found | No (known via a third-party summary) | The only cited source for `KAL` keepalive |
| [R2](r2-sds100-remote-command-spec.md) | SDS100 Remote Command Specification (v0.01, 2018-04-13) | Not found | No; **superseded by R6** | Historical: R6's first edition |

Uniden publishes its SDS200 documents as attachments to the
[SDS200 Firmware Update Change Log](https://info.uniden.com/twiki/bin/view/UnidenMan4/SDS200FirmwareUpdate)
page. Links were checked on 2026-11-27.

## What reading R4 and R6 changed

- The UDP control port, one-command-per-datagram framing and the multi-part
  `Footer` rules are now backed by an official SDS200 document, not just
  community reports.
- The RTSP audio session (URL, methods, SDP, G.711 µ-law, a single
  `client_port`, a 60 s session timeout) is now documented by Uniden.
- The key-code table is official, but only for the BCD536HP and SDS100.
- `GST`, `PWF` and `GWF` (waterfall) are documented.
- `VOL`, `SQL`, `POF`, `KAL`, `GLG`, `MSM` and `PWR` are still **not** in
  any official document the project holds.
- Two differences between the audio client and Uniden's RTSP sequence were
  found (no `OPTIONS` first; request URL vs `Content-Base`). They are
  recorded in [Protocol compliance → Audio](../../protocol-compliance.md#audio-rtsp-rtp)
  for hardware testing to settle.

None of this is hardware verification. Every row still needs checking on a
real SDS200.

## Verification levels

Every protocol fact in the project carries one of these levels:

| Level | Meaning |
|---|---|
| **Spec** | In an official Uniden document the project obtained and read (R1, R4, R6). |
| **Spec (cited)** | In an official Uniden document the project has *not* obtained; known only from a third-party summary. |
| **Community** | Observed on hardware by an independent open-source project. Not confirmed here. |
| **Implemented** | Encoded/decoded by the SDK. |
| **Simulated** | Implemented by the simulator. Proves only that the SDK and simulator agree. |
| **HW-verified** | Confirmed by this project on a real SDS200, with the firmware version recorded. No item has reached this level yet. |

Community sources (C1–C7) are listed in
[Protocol compliance → Documentation references](../../protocol-compliance.md#documentation-references).
No code was copied from them; they were read for protocol facts only.
