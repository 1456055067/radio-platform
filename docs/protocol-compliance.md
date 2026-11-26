# Protocol Compliance

This document records what is known about the Uniden SDS200 network protocol,
where each fact comes from, and how far it has been verified. It is the
authority for what the SDK may claim. **Nothing here has been verified on
SDS200 hardware by this project yet.**

## Verification levels

| Level | Meaning |
|---|---|
| **Spec** | In an official Uniden document that this project obtained and read. |
| **Spec (cited)** | In an official Uniden document that this project has *not* obtained; known only from a third-party summary. |
| **Community** | Observed against hardware by an independent open-source project (source file cited). Not confirmed by this project. |
| **Implemented** | Encoded/decoded by `uniden-protocol` and exposed by `uniden-sds`. |
| **Simulated** | Implemented by `sds-simulator`. Simulated behaviour proves only that the SDK and the simulator agree. |
| **HW-verified** | Confirmed by this project on a real SDS200 with firmware recorded in [`hardware-testing.md`](hardware-testing.md). |

## Documentation references

| ID | Document | Status |
|---|---|---|
| R1 | Uniden, *BCD436HP (UB376Z) / BCD536HP (UB375Z) Remote Command Specification*, Version 1.03, 2015-10-29. `info.uniden.com/twiki/pub/UnidenMan4/BCD536HPFirmwareUpdate/BCDx36HP_RemoteCommand_Specification_V1_03.pdf` | Obtained and read. The SDS series' published **predecessor** protocol. |
| R2 | Uniden, *SDS100 Remote Command Specification* (v0.01, 2018-04-13, per RadioReference forum) | **Not obtained.** The RadioReference forum host returned HTTP 503. |
| R3 | Uniden, SDS remote command specification **V2.00 (2025-07-07)**, introduces `KAL` | **Not obtained.** Cited in `jentfoo/SignalSentinel` `docs/specs-sds200.md`. |
| R4 | Uniden, *SDS200 Virtual Serial on Network* specification and RTSP notes ("RTSP.pdf") | **Not obtained.** Cited by community projects. |
| R5 | SDS200 firmware release notes | **Not obtained.** |
| C1 | `github.com/sean-foley/noctule-scan` (Swift, Apache-2.0): `LESSONS.md` hardware log (fw 1.24.00 / 1.26.01), `Status/ScannerInfo.swift`, `Audio/RTSPClient.swift`, `Models/FrontPanelKey.swift` | Community |
| C2 | `github.com/immakinbacon/sds200ha` (Python): `app/xml_lists.py` (fw 1.23.15), `app/protocol.py`, `app/key_codes.py` | Community |
| C3 | `github.com/stevenboyd78/sdsctl` (Python): `src/sds200/network.py`, `rtsp.py`, `rtp.py`, `docs/audio.md`, `docs/transports.md` | Community |
| C4 | `github.com/neilharvey94044/ScannerClient` (C++, GPL-2.0): captured RTSP replies in `src/tests/response_test_data.h` | Community |
| C5 | `github.com/jentfoo/SignalSentinel` (Go, GPL-3.0): `internal/sds200/protocol.go`, `docs/specs-sds200.md` | Community (partly a spec summary) |
| C6 | `github.com/DannyBoyk/scanner-transcriber`: `docs/sds200-rtsp-lockout.md` (fw 1.21.00) | Community |
| C7 | `github.com/SorcerTek/uniden-connect` (ESP32 C++): `src/sds200.cpp`, `src/rtsp_audio.cpp` | Community |

No code was copied from community projects; they were read for protocol facts only.

**Action item:** obtain R2–R5 directly from Uniden and re-verify every row below.

## Transport

| Function | Protocol | Port | Evidence | Status |
|---|---|---|---|---|
| Control and telemetry | UDP, one command per datagram | **50536** (scanner side; client port arbitrary) | Community (C1, C2, C3, C5, C7 default to 50536; forum reports) | Implemented, Simulated |
| Audio session control | RTSP over TCP | **554** | Community (C1, C3, C4, C7) | Not implemented (Phase 3) |
| Audio transport | RTP over unicast UDP; port negotiated in `SETUP` | negotiated | Community (C4 captured `Transport:` reply) | Not implemented (Phase 3) |

UDP behaviour:

- **Request framing.** `NAME[,arg...]\r`. The terminator `\r` is from Spec R1. One command per datagram is Community.
- **Reply framing.** One of two forms:
  - a single line, `NAME,field...\r` (Spec R1);
  - an XML reply, `NAME,<XML>,\r<?xml ...?>\r<Root>...</Root>\r` (Spec R1, from the `GLT` and `MSI` examples).
- **Header-only datagrams.** Some reports show the `NAME,<XML>,\r\n` header arriving as its own datagram, followed by the document (Community, `sds200-broadcastify`). The SDK handles this.
- **Multi-part XML.** Long replies such as `GLT,SYS` arrive as a sequence of self-contained documents. Each document carries `<Footer No="n" EOT="0|1"/>` just before the closing root tag, and some firmware spells the element `<Foot>`. `No` starts at 1 and increments by 1, and `EOT="1"` marks the last part. A document without a footer is complete on its own. One report saw `GLT,SYS` exceed 40 datagrams for 359 systems on fw 1.23.15 (Community C2; reassembly as in C1/C3/C5).
  - The SDK merges the parts and **rejects any sequence gap**. A rejected read-only request is re-sent.
- **`GSI`/`PSI` size.** Reported as a single datagram of about 1 KB with no footer (Community C1, C2).
- **Correlation.** The protocol has no request identifiers, so the SDK matches replies by command name, with one command in flight per scanner (Community C2 does the same).
  - A bare `ERR` carries no name, so it is attributed to the in-flight command.
  - Datagrams queued before a send are drained first.
- **Latency.** `GSI` is reported as taking 1–4 s, or sometimes not answering (Community C2, fw 1.23.15). The SDK's default XML timeout is 5 s.
- **Keepalive.** `KAL` gets no reply (Spec (cited) R3).
  - SDS100/SDS150 reportedly stop `PSI` pushes about 183 s after the `PSI` command unless a `KAL` is sent. This is **not shown for SDS200 over UDP** (Community C1).
  - The SDK supports an optional `KAL` interval, which is **disabled by default**.
- **Source filtering.** The SDK uses a *connected* UDP socket, so the OS drops datagrams that do not come from the scanner's address and port.

## Commands

The request and response syntax below quotes R1 unless the Evidence column
says otherwise. In the Status column, `Impl` means the command is
implemented, `Sim` that the simulator supports it, and `HW` that it is
hardware-verified. No command is hardware-verified yet.

| Command | Request | Response | Evidence | Effect | Status | Notes |
|---|---|---|---|---|---|---|
| `MDL` | `MDL` | `MDL,[MODEL_NAME]` | Spec R1 | read | Impl, Sim | R1 lists BCD536HP/BCD436HP; SDS200 value `SDS200` per community. |
| `VER` | `VER` | `VER,[VERSION]` with `Version x.xx.xx` | Spec R1 | read | Impl, Sim | Parsed into numeric components when possible. |
| `KEY` | `KEY,[KEY_CODE],[KEY_MODE]` | `KEY,OK` | Spec R1 (syntax); key table **not in obtained copy** | write | Impl, Sim | `E`, `.` and mode `P` are in R1's popup example. The other codes are Community (C1, C2, C3 identical): `M F L 0-9 . E > < ^ V Q Y A B C Z T R`. `Q` and `T` are flagged unverified on SDS200. Mode `L` (long press) is Community (C7). |
| `QSH` | `QSH,[FRQ]` | `QSH,OK` | Spec R1 | write | Impl, Sim | `FRQ` is assumed to be in 100 Hz units, by analogy with R1's `AVD` example (`4060000` = 406.0 MHz). **HW verification required.** Invalid in Menu, Direct Entry and Quick Save. |
| `STS` | `STS` | `STS,[DSP_FORM],[L1_CHAR],[L1_MODE],…,[L20_MODE],[RSV]×7,[BK_COLOR],[BK_DIMMER]` | Spec R1 | read | Impl, Sim | R1: "PSI is better than STS". Characters ≥ 0x80 are scanner font glyphs and are preserved as latin-1. |
| `JNT` | `JNT,[FL_TAG],[SYS_TAG],[CHAN_TAG]` | `JNT,OK` | Spec R1 | write | Impl, Sim | Ranges 0–99, 0–99, 0–999 are enforced. |
| `NXT` / `PRV` | `NXT,[tkw],[xxx1],[xxx2],[COUNT]` | `NXT,OK` | Spec R1 | write | Raw only | Not yet typed. |
| `FQK` | `FQK` / `FQK,[S0..S99]` | `FQK,[S0..S99]` / `FQK,OK` | Spec R1 | read/write | Get: Impl, Sim | Each state is 0 (absent), 1 (disabled) or 2 (enabled). Set not exposed yet. |
| `SQK` | `SQK,[FAV_QK]` | `SQK,[FAV_QK],[SYS_QK],[S0..S99]` | Spec R1 | read | Impl | The spec shows a `[SYS_QK]` echo; the decoder skips 2 leading fields. **HW verification required.** |
| `DQK` | `DQK,[FAV_QK],[SYS_QK]` | `DQK,[FAV_QK],[SYS_QK],[S0..S99]` | Spec R1 | read | Impl | |
| `PSI` | `PSI,[interval]` | `PSI,OK`, then periodic `PSI,<XML>,…` | Spec R1 (existence; "interval by parameter"); Community C1 (`PSI,<ms>`, `PSI,0` stops) | write | Impl, Sim | Pushes are delivered through `Scanner::subscribe()`. |
| `GSI` | `GSI` | `GSI,<XML>,` + `ScannerInfo` | Spec R1 | read | Impl, Sim | See the `ScannerInfo` section. |
| `GLT` | `GLT,FL` · `GLT,SYS,[fl]` · `GLT,DEPT,[sys]` · `GLT,SITE,[sys]` · `GLT,CFREQ,[dept]` · `GLT,TGID,[dept]` · `GLT,SFREQ,[site]` · `GLT,AFREQ` · `GLT,ATGID,[sys]` · `GLT,FTO` · `GLT,CS_BANK` · `GLT,UREC` · `GLT,IREC_FILE` · `GLT,UREC_FILE,[folder]` · `GLT,TRN_DISCOV` · `GLT,CNV_DISCOV` | `GLT,<XML>,` + `<GLT>` entries | Spec R1 | read | Impl. Sim: FL, SYS | Footer multi-part is Community. |
| `HLD` | `HLD,[tkw],[xxx1],[xxx2]` | `HLD,OK` | Spec R1 | write | Impl, Sim | Cannot hold a favorites list or a site frequency. The SDK always emits both option fields, so an empty second option is sent as a trailing comma (`HLD,SYS,283,`). **HW verification required.** |
| `AVD` | `AVD,[tkw],[xxx1],[xxx2],[STATUS]` | `AVD,OK` | Spec R1 (example `AVD,AFREQ,4060000,,1`) | write | Impl, Sim | Status 1 is permanent, 2 is temporary, 3 stops avoiding. |
| `JPM` | `JPM,[JUMP_MODE],[INDEX]` | `JPM,OK` / `NG` | Spec R1 | write | Impl, Sim | `Scanner::resume_scan()` sends `JPM,SCN_MODE,4294967295`. R1 says index `0xFFFFFFFF` means "scan from top channel", but **whether it must be sent as decimal or hex is unverified**. This restarts the scan rather than releasing the hold in place. |
| `DTM` | `DTM` | `DTM,[DST],[YYYY],[MM],[DD],[hh],[mm],[ss],[RTC]` | Spec R1 | read | Impl, Sim | Set not exposed. |
| `LCR` | `LCR` | `LCR,[LAT],[LON],[RANGE]` | Spec R1 | read | Impl, Sim | Set not exposed. |
| `URC` | `URC` / `URC,[0/1]` | `URC,[STATUS]` / `URC,OK` / `URC,ERR,[code]` | Spec R1 | read/write | Impl, Sim | Error codes: 0001 file access, 0002 low battery, 0003 session limit, 0004 RTC lost. |
| `AST`, `APR` | analyse mode | XML streams | Spec R1 | write | Raw only | Not typed. |
| `MNU`, `MSI`, `MSV`, `MSB` | menu control | XML for `MSI` | Spec R1 | write | Raw only | Not typed. |
| `SVC`, `BFH` | service types, band scope hold | | Spec R1 | read/write | Raw only | |
| `VOL` | `VOL` / `VOL,[n]` | `VOL,[n]` / `VOL,OK` | **Community only.** Not in R1's command list. | read/write | Impl, Sim | The range 0–29 comes from R1's GSI `Property VOL` ("0-29 or 0-15"). Probe before use. |
| `SQL` | `SQL` / `SQL,[n]` | `SQL,[n]` / `SQL,OK` | **Community only.** | read/write | Impl, Sim | The range 0–19 comes from R1's GSI `Property SQL` ("0-19 or 0-15"). Volume and squelch are also readable from `GSI` `Property`, which is in R1. |
| `POF` | `POF` | `POF,OK` (assumed) | **Community only** | write | Impl, Sim | Never retried. |
| `KAL` | `KAL` | none | Spec (cited) R3 | read | Impl, Sim | Fire-and-forget. |
| `GLG` | `GLG` | legacy reception status | Community (RadioReference command sieve on SDS100 fw 1.03.01) | read | Raw only | The field layout is not documented for SDS; not typed. |
| `PWR` | `PWR` | RSSI / frequency (older Uniden models) | **Not found** for SDS | read | Raw only | Not typed; use `GSI Property@Rssi`. |
| `MSM` | `MSM,…` | `MSM,OK`; reboots the scanner | Community C1, C6 (used to recover RTSP lockups) | write | Raw only | **Undocumented and destructive.** Administrators only. |
| `GST` | `GST` | unknown | Community C2 (20–70 ms round trip) | read | Raw only | Format unknown. |

Raw commands (`Command::Raw`) are validated: the name must be 2–8 uppercase
letters and the arguments printable ASCII without `,`, CR or LF. Raw
commands count as **state-changing**, and so are never retried, unless the
caller explicitly declares them read-only.

### Error responses

| Token | Meaning | Evidence |
|---|---|---|
| `ERR` (bare) | Invalid or unsupported command | Long-standing Uniden convention; Community |
| `CMD,NG` | Valid but not possible in the current state | Spec R1 (`JPM`: "scanner sends NG response") |
| `CMD,ERR,code` | Command-specific error | Spec R1 (`URC,ERR,[ERROR CODE]`) |
| `FER`, `ORER` | Framing / overrun error | Older Uniden serial convention; **not observed for SDS** |

The SDK never retries rejections.

## `ScannerInfo` (GSI / PSI)

Element and attribute names come from R1 ("PSI, GSI Element/Attribute"
sheets) unless a row says otherwise. Every field is optional. Values the
scanner does not report are `None`, never synthesised.

| Element | Attributes modelled | Notes |
|---|---|---|
| `ScannerInfo` (root) | `Mode`, `V_Screen` | `V_Screen` values include `conventional_scan`, `trunk_scan`, `custom_search`, `quick_search`, `close_call`, `wx_alert`, `discovery_*`, `analyze*`, `band_scope`, `menu_*`, `plain_text` (R1). |
| `MonitorList` | `Name`, `Index`, `Q_Key`, `N_Tag`, `DB_Counter` | Indexes become invalid when `DB_Counter` changes (R1). |
| `System` | `Name`, `Index`, `Avoid`, `SystemType`, `Q_Key`, `N_Tag`, `Hold` | |
| `Department` | `Name`, `Index`, `Avoid`, `Q_Key`, `Hold` | |
| `Site` | `Name`, `Index`, `Avoid`, `Q_Key`, `Hold`, `Mod` | |
| `ConvFrequency`, `TGID`, `SrchFrequency`, `CcHitsChannel`, `ToneOutChannel`, `WxChannel` | `Name`, `Index`, `Avoid`, `Hold`, `N_Tag`, `Freq` (`xxxx.xxxxMHz`, leading space observed), `Mod`, `TGID`, `SvcType`, `SAD` | The first one present becomes `ScannerInfo::channel`. `TGID` values: the `TGID:` prefix is stripped, and placeholders `---`, `None` and empty map to `None` (Community C1, `sds200-broadcastify`). |
| `SiteFrequency` | `Freq` | Control or voice frequency when trunking. |
| `UnitID` | `U_Id` | **Community** (C1). `UID` and `UID:` prefixes are stripped; `UID None` maps to `None`. |
| `Property` | `VOL`, `SQL`, `Sig` (0–4), `Rssi`, `Mute`, `Att`, `Rec`, `KeyLock`, `P25Status`, `WiFi`, `Backlight` | `Rssi="-999"` while idle maps to `None` (Community C1). The `Rssi` scale is undocumented. |
| `ViewDescription` | `InfoArea1/2@Text`, `OverWrite@Text`, `PopupScreen@Text` + `Button@Text/KeyCode`, `PlainText@Text` | Popup buttons give the `KEY` code to send (R1). |

Elements not modelled above, such as `AGC`, `DualWatch`, `SystemStatus`,
`SearchRange`, `BandScope` and `ReplayDescription`, remain available in
`ScannerInfo::raw`.

**Signal quality.** R1 documents `SystemStatus@Signal/Quality/Activity`
(0–100) only in `analyze_system_status` mode. In normal scanning only `Sig`
(bars) and `Rssi` are available.

## Audio (RTSP / RTP)

These facts are community-reported. Phase 3 implements them as
**Impl, Sim** (not HW): `scanner-audio` follows them, and the simulator's
audio server reproduces them, including the lock-up after a lost session.
Each needs hardware verification, so audio stays **off by default** per
scanner (ADR 0009).

| Item | Finding | Evidence |
|---|---|---|
| URL | `rtsp://<ip>/au:scanner.au` (port 554) | C1, C3, C4, C5, C7 |
| Server | `Server: Scanner Audio Server 0.0.1` | C4 capture |
| Methods | `OPTIONS, DESCRIBE, SETUP, TEARDOWN, PLAY, GET_PARAMETER` | C4 capture (`Public:` header) |
| SDP | `m=audio 0 RTP/AVP 0`, `a=control:trackID=1`, **no `a=rtpmap`** | C4 capture |
| Codec | RTP payload type 0, G.711 µ-law (PCMU), 8 kHz mono | C1, C3; implied by static PT 0 (RFC 3551) |
| Transport | Unicast UDP; send a single `client_port=N` | C3 (`docs/audio.md`); others send a pair |
| Keepalive | `GET_PARAMETER` every 15–45 s; 60 s inactivity timeout reported | C1, C3, C2, C5 |
| Concurrency | **One RTSP client at a time** | C1, C6 |
| Fragility | Missing `TEARDOWN`, an outage over ~60 s, or an out-of-order request sequence can lock up port 554 until a power cycle; reconnecting immediately can hang the server | C1 (fw 1.24.00/1.26.01), C6 (fw 1.21.00) |

How the implementation follows them (`crates/scanner-audio`,
`services/scanner-agent/src/audio.rs`):

- **Single client:** the **site agent** is the only RTSP client per scanner.
  It streams only while someone listens or the scanner records, and the
  gateway fans the audio out.
- **No probes:** the agent never opens a TCP connection to port 554 except
  as a real session.
- **TEARDOWN:** always sent on every exit path once a session exists. The
  simulator test checks the exact sequence DESCRIBE → SETUP → PLAY →
  TEARDOWN.
- **Spacing:** a cooldown between sessions (default 10 s), exponential
  backoff after failures (to 300 s), and a 15 s linger after the last
  listener.
- **Keepalive:** `GET_PARAMETER` at half the server's session timeout
  (≤ 25 s).
- **Transport:** a single `client_port` is sent.
- **SDP:** PT 0 is accepted without `a=rtpmap`, and a control URL on
  another host is refused.
- **RTP:** accepted only from the scanner's IP and only with PT 0, then
  passed through a reordering jitter buffer that counts loss.

## Discovery

No discovery mechanism (broadcast, multicast or announcement) appears in
the obtained BCDx36HP v1.03 specification or in the community sources
C1–C3. The platform's discovery therefore sends the documented read-only
`MDL` command to each address in an operator-approved range, and treats a
`MDL,<model>` reply as a candidate scanner (Impl, Sim; **not HW**).

How the SDS200 treats a command from a second UDP source is undocumented.
The simulator, for example, sends PSI pushes to whichever address spoke
last. Discovery therefore never probes an endpoint that is enrolled or
managed.

## Unresolved questions (hardware verification required)

1. Is UDP 50536 fixed on all SDS200 firmware? Can it be changed in the menu?
2. Do `VOL`, `SQL` and `POF` exist on the SDS200 over UDP, and what are their exact reply formats and ranges?
3. Are `FER` and `ORER` ever sent over UDP?
4. What units does `QSH,[FRQ]` take (100 Hz?), and what argument format does `JPM` take (decimal 4294967295?)?
5. Is `HLD` with a trailing empty field accepted? Is there a documented hold-release command, or does `HLD` toggle?
6. What are the multi-part footer rules on current firmware: does every part repeat `CMD,<XML>,`? What is the maximum datagram size?
7. Does the SDS200 stop `PSI` pushes without `KAL` over UDP?
8. What does the full key-code table in R2/R3 say for the SDS200?
9. What RTP packetization interval and packet size does the scanner use?
10. Does `GSI` expose a unit ID (`UnitID@U_Id`) on every firmware?
11. How much command traffic can the scanner handle without dropping replies?
12. Does an `MDL` probe from a second UDP source disturb an existing control session or redirect `PSI` pushes? Does the SDS200 answer probes at all while another client is connected?
13. Audio on hardware:
    - Does the SDS200 accept the DESCRIBE → SETUP (single `client_port`) → PLAY → `GET_PARAMETER` → TEARDOWN sequence as implemented?
    - Is the 10 s cooldown enough to avoid the reported hang?
    - Does audio keep flowing while the control port is in use?
