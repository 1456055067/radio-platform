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
| R2 | Uniden, *SDS100 Remote Command Specification* (v0.01, 2018-04-13, per RadioReference forum) | **Not obtained, superseded.** R6's revision history lists v0.01 (2018-04-13, "based on BCD536HP/BCD436HP Remote Command Specification Version 1.05") as its first edition, so R6 replaces R2. |
| R3 | Uniden, SDS remote command specification **V2.00 (2025-07-07)**, introduces `KAL` | **Not obtained.** Cited in `jentfoo/SignalSentinel` `docs/specs-sds200.md`. Not on Uniden's SDS200 page, which carries R6 (V1.02). |
| R4 | Uniden, *SDS200 Virtual Serial on Network Protocol Specification*, Version 1.00, 2019-01-23 (history: 1st edition 2018-04-13), and RTSP notes ("RTSP.pdf", undated). Both at `info.uniden.com/twiki/pub/UnidenMan4/SDS200FirmwareUpdate/` | **Obtained and read** (2026-11-27). Cited below as R4-VS and R4-RTSP. |
| R5 | SDS200 firmware release notes: `info.uniden.com/twiki/bin/view/UnidenMan4/SDS200FirmwareUpdate` | Located, not yet reviewed. |
| R6 | Uniden, *SDS100 / SDS200 Remote Command Specification*, Version 1.02, 2023-12-22 (1.01 added Waterfall; 1.02 added `GST`). `info.uniden.com/twiki/pub/UnidenMan4/SDS200FirmwareUpdate/SDS200_RemoteCommand_Specification_V1_02.pdf` | **Obtained and read** (2026-11-27). The first official **SDS200** command specification this project holds. |
| C1 | `github.com/sean-foley/noctule-scan` (Swift, Apache-2.0): `LESSONS.md` hardware log (fw 1.24.00 / 1.26.01), `Status/ScannerInfo.swift`, `Audio/RTSPClient.swift`, `Models/FrontPanelKey.swift` | Community |
| C2 | `github.com/immakinbacon/sds200ha` (Python): `app/xml_lists.py` (fw 1.23.15), `app/protocol.py`, `app/key_codes.py` | Community |
| C3 | `github.com/stevenboyd78/sdsctl` (Python): `src/sds200/network.py`, `rtsp.py`, `rtp.py`, `docs/audio.md`, `docs/transports.md` | Community |
| C4 | `github.com/neilharvey94044/ScannerClient` (C++, GPL-2.0): captured RTSP replies in `src/tests/response_test_data.h` | Community |
| C5 | `github.com/jentfoo/SignalSentinel` (Go, GPL-3.0): `internal/sds200/protocol.go`, `docs/specs-sds200.md` | Community (partly a spec summary) |
| C6 | `github.com/DannyBoyk/scanner-transcriber`: `docs/sds200-rtsp-lockout.md` (fw 1.21.00) | Community |
| C7 | `github.com/SorcerTek/uniden-connect` (ESP32 C++): `src/sds200.cpp`, `src/rtsp_audio.cpp` | Community |

No code was copied from community projects; they were read for protocol facts only.

**Action items:**
- Obtain R3 (V2.00) from Uniden; it is the only cited source for `KAL`.
- Review R5 for behaviour changes between firmware versions.
- R6 is an SDS100/SDS200 document written against firmware of its date
  (2023-12). "Spec R6" means Uniden documents it, not that current
  SDS200 firmware or this project's use of it is verified. Hardware
  verification is still required for every row.
- `Provenance::PredecessorSpec` in `uniden-protocol` still names only R1.
  Every command it covers is also in R6; the enum and its docs have not
  yet been updated.

## Transport

| Function | Protocol | Port | Evidence | Status |
|---|---|---|---|---|
| Control and telemetry | UDP, one command per datagram | **50536** (scanner side; client port arbitrary) | Spec R4-VS ("uses UDP Port 50536 for compatibility BCD536HP"; no negotiation or protocol header). Community (C1, C2, C3, C5, C7) agree | Implemented, Simulated |
| Audio session control | RTSP/1.0 over TCP | **554** | Spec R4-RTSP (sequence diagram, `dst port=554(rtsp)`). Community (C1, C3, C4, C7) agree | See [Audio](#audio-rtsp-rtp) |
| Audio transport | RTP/AVP over unicast UDP; port negotiated in `SETUP` | negotiated | Spec R4-RTSP (`Transport: RTP/AVP;unicast;client_port=…`, reply adds `server_port`, `ssrc`). Community (C4) agrees | See [Audio](#audio-rtsp-rtp) |

UDP behaviour:

- **Request framing.** `NAME[,arg...]\r`. The terminator `\r` is from Spec R1 and R6. One command per datagram is Spec R4-VS ("send packet which remote command is written to"; example `MDL\r` → `MDL,SDS200`).
- **Reply framing.** One of two forms:
  - a single line, `NAME,field...\r` (Spec R1);
  - an XML reply, `NAME,<XML>,\r<?xml ...?>\r<Root>...</Root>\r` (Spec R1, from the `GLT` and `MSI` examples).
- **Header-only datagrams.** Some reports show the `NAME,<XML>,\r\n` header arriving as its own datagram, followed by the document (Community, `sds200-broadcastify`). The SDK handles this.
- **Multi-part XML.** Long replies such as `GLT,SYS` arrive as a sequence of self-contained documents. Each document carries `<Footer No="n" EOT="0|1"/>` just before the closing root tag, and some firmware spells the element `<Foot>`. `No` starts at 1 and increments by 1, and `EOT="1"` marks the last part. A document without a footer is complete on its own. One report saw `GLT,SYS` exceed 40 datagrams for 359 systems on fw 1.23.15 (Community C2; reassembly as in C1/C3/C5).
  - **Spec R4-VS** confirms the core rules: "Response of XML are divided multi packets"; watch `EOT` to find the last packet and `No` to detect a lost one; "Retry request, if packet is lost." Its `GLT,FL` example shows **every part repeating the `GLT,<XML>,` header and XML declaration**, with `<Footer No="2" EOT="0"/>` and `<Footer No="3" EOT="1"/>` before `</GLT>`. The prose calls it the "Foot node" while the example uses `<Footer>`; the `<Foot>` spelling remains Community. R4-VS does not say whether part 1 always carries a footer, or a maximum datagram size.
  - The SDK merges the parts and **rejects any sequence gap**. A rejected read-only request is re-sent, as R4-VS prescribes.
- **`GSI`/`PSI` size.** Reported as a single datagram of about 1 KB with no footer (Community C1, C2).
- **Correlation.** The protocol has no request identifiers, so the SDK matches replies by command name, with one command in flight per scanner (Community C2 does the same).
  - A bare `ERR` carries no name, so it is attributed to the in-flight command.
  - Datagrams queued before a send are drained first.
- **Latency.** `GSI` is reported as taking 1–4 s, or sometimes not answering (Community C2, fw 1.23.15). The SDK's default XML timeout is 5 s.
- **Keepalive.** `KAL` gets no reply (Spec (cited) R3). `KAL` is **not** in R6 (V1.02) or R4-VS.
  - SDS100/SDS150 reportedly stop `PSI` pushes about 183 s after the `PSI` command unless a `KAL` is sent. This is **not shown for SDS200 over UDP** (Community C1).
  - The SDK supports an optional `KAL` interval, which is **disabled by default**.
- **Source filtering.** The SDK uses a *connected* UDP socket, so the OS drops datagrams that do not come from the scanner's address and port.

## Commands

The request and response syntax below quotes R1 unless the Evidence column
says otherwise. Where a row cites "R1, R6", R6 (the SDS100/SDS200
specification) gives the same syntax as R1 unless the Notes say how it
differs. R6's command list (30 commands) is: `MDL VER KEY QSH STS JNT NXT
PRV FQK SQK DQK PSI GSI GLT HLD AVD SVC JPM DTM LCR AST APR URC MNU MSI MSV
MSB GST PWF GWF`. In the Status column, `Impl` means the command is
implemented, `Sim` that the simulator supports it, and `HW` that it is
hardware-verified. No command is hardware-verified yet.

| Command | Request | Response | Evidence | Effect | Status | Notes |
|---|---|---|---|---|---|---|
| `MDL` | `MDL` | `MDL,[MODEL_NAME]` | Spec R1, R6, R4-VS | read | Impl, Sim | R6 lists `SDS100` and `SDS200`; R4-VS's example reply is `MDL,SDS200`. |
| `VER` | `VER` | `VER,[VERSION]` with `Version x.xx.xx` | Spec R1, R6 | read | Impl, Sim | Parsed into numeric components when possible. |
| `KEY` | `KEY,[KEY_CODE],[KEY_MODE]` | `KEY,OK` | Spec R1, R6 (syntax); key table Spec R6 | write | Impl, Sim | R6's "key code for KEY Command" sheet lists `M F L 1-9 0 . E > < ^ V Q Y A B C Z T R`, matching Community (C1, C2, C3). The sheet has **BCD536HP and SDS100 columns, no SDS200 column**. On SDS100, `Q` (squelch knob) and `T` (service type) are "(none)" and `V` is "Backlight"; SDS200 mapping still needs HW. `KEY_MODE` values are not defined in R6; mode `P` is from R1's and R6's popup example (`KEY,E,P`), mode `L` (long press) is Community (C7). |
| `QSH` | `QSH,[FRQ]` | `QSH,OK` | Spec R1, R6 | write | Impl, Sim | Neither spec gives `FRQ` units. 100 Hz is assumed by analogy with the `AVD` example (`4060000` = 406.0 MHz, in R1 and R6). **HW verification required.** Invalid in Menu, Direct Entry and Quick Save. |
| `STS` | `STS` | `STS,[DSP_FORM],[L1_CHAR],[L1_MODE],…,[L20_MODE],` + 9 trailing fields | Spec R1, R6 | read | Impl, Sim | R1 names the trailing fields `[RSV]×7,[BK_COLOR],[BK_DIMMER]`; R6 marks all nine `[RSV]`. Both: "PSI is better than STS". Characters ≥ 0x80 are scanner font glyphs (R6: see "Font Data Specification") and are preserved as latin-1. |
| `JNT` | `JNT,[FL_TAG],[SYS_TAG],[CHAN_TAG]` | `JNT,OK` | Spec R1, R6 | write | Impl, Sim | Ranges 0–99, 0–99, 0–999 (R6) are enforced. |
| `NXT` / `PRV` | `NXT,[tkw],[xxx1],[xxx2],[COUNT]` | `NXT,OK` | Spec R1, R6 | write | Raw only | Not yet typed. R6: `COUNT` is 1–8. |
| `FQK` | `FQK` / `FQK,[S0..S99]` | `FQK,[S0..S99]` / `FQK,OK` | Spec R1, R6 | read/write | Get: Impl, Sim | Each state is 0 (absent), 1 (disabled) or 2 (enabled); R6: a 0 sent by the controller is ignored. Set not exposed yet. |
| `SQK` | `SQK,[FAV_QK]` | `SQK,[FAV_QK],[SYS_QK],[S0..S99]` | Spec R1, R6 | read | Impl | Both specs show the `[SYS_QK]` field; the decoder skips 2 leading fields. **HW verification required.** |
| `DQK` | `DQK,[FAV_QK],[SYS_QK]` | `DQK,[FAV_QK],[SYS_QK],[S0..S99]` | Spec R1, R6 | read | Impl | |
| `PSI` | `PSI,[interval]` | `PSI,OK`, then periodic `PSI,<XML>,…` | Spec R1, R6 (existence; "User can change interval by parameter"); Community C1 (`PSI,<ms>`, `PSI,0` stops) | write | Impl, Sim | Neither spec gives the interval's unit. Pushes are delivered through `Scanner::subscribe()`. |
| `GSI` | `GSI` | `GSI,<XML>,` + `ScannerInfo` | Spec R1, R6 | read | Impl, Sim | See the `ScannerInfo` section. |
| `GLT` | `GLT,FL` · `GLT,SYS,[fl]` · `GLT,DEPT,[sys]` · `GLT,SITE,[sys]` · `GLT,CFREQ,[dept]` · `GLT,TGID,[dept]` · `GLT,SFREQ,[site]` · `GLT,AFREQ` · `GLT,ATGID,[sys]` · `GLT,FTO` · `GLT,CS_BANK` · `GLT,UREC` · `GLT,IREC_FILE` · `GLT,UREC_FILE,[folder]` · `GLT,TRN_DISCOV` · `GLT,CNV_DISCOV` | `GLT,<XML>,` + `<GLT>` entries | Spec R1, R6 | read | Impl. Sim: FL, SYS | Footer multi-part: Spec R4-VS (see Transport). |
| `HLD` | `HLD,[tkw],[xxx1],[xxx2]` | `HLD,OK` | Spec R1, R6 | write | Impl, Sim | Cannot hold a favorites list or a site frequency. R6's option table gives `[none]` as the second option for `SYS`, `SITE` and channels. The SDK always emits both option fields, so an empty second option is sent as a trailing comma (`HLD,SYS,283,`); neither spec says whether that is accepted. **HW verification required.** |
| `AVD` | `AVD,[tkw],[xxx1],[xxx2],[STATUS]` | `AVD,OK` | Spec R1, R6 (example `AVD,AFREQ,4060000,,1`) | write | Impl, Sim | Status 1 is permanent, 2 is temporary, 3 stops avoiding. R6's syntax line omits the comma before `[STATUS]`, but its example includes it. R6: use `GSI` or `GLT` to read avoid status. |
| `JPM` | `JPM,[JUMP_MODE],[INDEX]` | `JPM,OK` / `NG` | Spec R1, R6 | write | Impl, Sim | `Scanner::resume_scan()` sends `JPM,SCN_MODE,4294967295`. Both specs say index `0xFFFFFFFF` means "scan from top channel" without giving the encoding. R4-VS's `GLT,FL` example writes the same value as decimal `Index="4294967295"`, which supports, but does not prove, decimal. **HW verification required.** This restarts the scan rather than releasing the hold in place. R6 adds `WF_MODE` and `NG` cases for a temporary clock. |
| `DTM` | `DTM` | `DTM,[DST],[YYYY],[MM],[DD],[hh],[mm],[ss],[RTC]` | Spec R1, R6 | read | Impl, Sim | R6: RTC status 0 = NG, 1 = OK. Set not exposed. |
| `LCR` | `LCR` | `LCR,[LAT],[LON],[RANGE]` | Spec R1, R6 | read | Impl, Sim | R6: latitude and longitude in degree format. Set not exposed. |
| `URC` | `URC` / `URC,[0/1]` | `URC,[STATUS]` / `URC,OK` / `URC,ERR,[code]` | Spec R1, R6 | read/write | Impl, Sim | Error codes: 0001 file access, 0002 low battery, 0003 session limit, 0004 RTC lost. |
| `AST`, `APR` | analyse mode | XML streams | Spec R1, R6 | write | Raw only | Not typed. |
| `MNU`, `MSI`, `MSV`, `MSB` | menu control | XML for `MSI` | Spec R1, R6 | write | Raw only | Not typed. R6 lists the `MNU` menu IDs; `MSV` replaces `,` in values with tab. |
| `SVC` | service types | `SVC,[PST1..PST37],[CST1..CST10]` | Spec R1, R6 | read/write | Raw only | |
| `BFH` | band scope hold | | Spec R1 | write | Raw only | **Not in R6's command list.** |
| `GST` | `GST` | `GST,[DSP_FORM],[L1_CHAR],[L1_MODE],…,[L20_MODE],[MUTE],[RSV],[RSV],[WF_MODE],[FREQ],[MOD],[MF_POS],[CF],[LOWER],[UPPER],[RSV],[FFT_SIZE]` | Spec R6 ("Get scanner status (for WF)", added in V1.02); Community C2 (20–70 ms round trip) | read | Raw only | `WF_MODE`: 0 normal, 1 waterfall, 2 menu/direct entry. `FFT_SIZE`: 0–3 = 25–100 %. All-space fields become `""`; `,` in text becomes tab. Not typed. |
| `PWF`, `GWF` | `PWF,[FFT_TYPE],[ON/OFF]` · `GWF,[TYPE],[ON/OFF]` | `PWF,[DATA1..n],` · `GWF,[DATA1..DATA240],` | Spec R6 (V1.01, Waterfall) | read | Raw only | Waterfall FFT push / get; type 1 is the displayed FFT (240 points). Not typed. |
| `VOL` | `VOL` / `VOL,[n]` | `VOL,[n]` / `VOL,OK` | **Community only.** Not in R1's or R6's command list. | read/write | Impl, Sim | The range 0–29 comes from the GSI `Property VOL` ("0-29 or 0-15", R1 and R6). Probe before use. |
| `SQL` | `SQL` / `SQL,[n]` | `SQL,[n]` / `SQL,OK` | **Community only.** Not in R1 or R6. | read/write | Impl, Sim | The range 0–19 comes from the GSI `Property SQL` ("0-19 or 0-15", R1 and R6). Volume and squelch are also readable from `GSI` `Property`, which is Spec. |
| `POF` | `POF` | `POF,OK` (assumed) | **Community only.** Not in R1 or R6. | write | Impl, Sim | Never retried. |
| `KAL` | `KAL` | none | Spec (cited) R3. Not in R6 (V1.02). | read | Impl, Sim | Fire-and-forget. |
| `GLG` | `GLG` | legacy reception status | Community (RadioReference command sieve on SDS100 fw 1.03.01). Not in R6. | read | Raw only | The field layout is not documented for SDS; not typed. |
| `PWR` | `PWR` | RSSI / frequency (older Uniden models) | **Not found** for SDS (not in R6) | read | Raw only | Not typed; use `GSI Property@Rssi`. |
| `MSM` | `MSM,…` | `MSM,OK`; reboots the scanner | Community C1, C6 (used to recover RTSP lockups). Not in R6. | write | Raw only | **Undocumented and destructive.** Administrators only. |

Raw commands (`Command::Raw`) are validated: the name must be 2–8 uppercase
letters and the arguments printable ASCII without `,`, CR or LF. Raw
commands count as **state-changing**, and so are never retried, unless the
caller explicitly declares them read-only.

### Error responses

| Token | Meaning | Evidence |
|---|---|---|
| `ERR` (bare) | Invalid or unsupported command | Long-standing Uniden convention; Community |
| `CMD,NG` | Valid but not possible in the current state | Spec R1, R6 (`JPM`: "scanner sends NG response") |
| `CMD,ERR,code` | Command-specific error | Spec R1, R6 (`URC,ERR,[ERROR CODE]`) |
| `FER`, `ORER` | Framing / overrun error | Older Uniden serial convention; **not observed for SDS** |

The SDK never retries rejections.

## `ScannerInfo` (GSI / PSI)

Element and attribute names come from R1 ("PSI, GSI Element/Attribute"
sheets) unless a row says otherwise. R6 has the same sheets for the
SDS100/SDS200 and agrees with every row below except where noted; it also
adds `Property` attributes `F`, `Battery`, `A_Led` and `Dir`, waterfall
elements (`WaterfallBand`, `WaterfallSettings`) and per-channel `RecSlot`,
`LVL` and `IFX`. Every field is optional. Values the
scanner does not report are `None`, never synthesised.

| Element | Attributes modelled | Notes |
|---|---|---|
| `ScannerInfo` (root) | `Mode`, `V_Screen` | `V_Screen` values include `conventional_scan`, `trunk_scan`, `custom_search`, `quick_search`, `close_call`, `wx_alert`, `discovery_*`, `analyze*`, `band_scope`, `menu_*`, `plain_text` (R1). |
| `MonitorList` | `Name`, `Index`, `Q_Key`, `N_Tag`, `DB_Counter` | Indexes become invalid when `DB_Counter` changes (R1). |
| `System` | `Name`, `Index`, `Avoid`, `SystemType`, `Q_Key`, `N_Tag`, `Hold` | |
| `Department` | `Name`, `Index`, `Avoid`, `Q_Key`, `Hold` | |
| `Site` | `Name`, `Index`, `Avoid`, `Q_Key`, `Hold`, `Mod` | |
| `ConvFrequency`, `TGID`, `SrchFrequency`, `CcHitsChannel`, `ToneOutChannel`, `WxChannel` | `Name`, `Index`, `Avoid`, `Hold`, `N_Tag`, `Freq` (`xxxx.xxxxMHz`, leading space observed), `Mod`, `TGID`, `SvcType`, `SAD` | The first one present becomes `ScannerInfo::channel`. `TGID` values: R6 gives `TGID:xxxx` on the `TGID` element and `TGID xxxx/None` on `ConvFrequency`, `SrchFrequency` and the discovery elements (Spec R6). The SDK strips the `TGID:` prefix and maps `---`, `None` and empty to `None`; `---` is Community (C1, `sds200-broadcastify`). |
| `SiteFrequency` | `Freq` | Control or voice frequency when trunking. |
| `UnitID` | `U_Id` | **Community** (C1) as a separate `UnitID` element. R6 documents `U_Id` (`UID xxxx/None`) only as an **attribute of** `ConvFrequency`, `SrchFrequency`, `ConventionalDiscovery` and `TrunkingDiscovery` (Spec R6); the SDK does not yet read it there. `UID` and `UID:` prefixes are stripped; `UID None` maps to `None`. |
| `Property` | `VOL`, `SQL`, `Sig` (0–4), `Rssi`, `Mute`, `Att`, `Rec`, `KeyLock`, `P25Status`, `WiFi`, `Backlight` | R6: `Rssi` range "0-" with example `Rssi="0.377"`; the scale and units remain undocumented. `Rssi="-999"` while idle maps to `None` (Community C1). R6 lists `WiFi` as `Off / 0-3 / AP`. |
| `ViewDescription` | `InfoArea1/2@Text`, `OverWrite@Text`, `PopupScreen@Text` + `Button@Text/KeyCode`, `PlainText@Text` | Popup buttons give the `KEY` code to send (R1). |

Elements not modelled above, such as `AGC`, `DualWatch`, `SystemStatus`,
`SearchRange`, `BandScope` and `ReplayDescription`, remain available in
`ScannerInfo::raw`.

**Signal quality.** R1 and R6 document `SystemStatus@Signal/Quality/Activity`
(0–100) only in `analyze_system_status` mode. In normal scanning only `Sig`
(bars) and `Rssi` are available.

## Audio (RTSP / RTP)

Uniden's RTSP notes (R4-RTSP) now document the session itself; behaviour
under failure is still community-reported only. Phase 3 implements these
facts as **Impl, Sim** (not HW): `scanner-audio` follows them, and the
simulator's audio server reproduces them, including the lock-up after a
lost session. Each needs hardware verification, so audio stays **off by
default** per scanner (ADR 0009).

| Item | Finding | Evidence |
|---|---|---|
| URL | `rtsp://<ip>/au:scanner.au` (port 554). `SETUP` targets `…/au:scanner.au/trackID=1`; `PLAY`, `GET_PARAMETER` and `TEARDOWN` target the `Content-Base` `…/au:scanner.au/` (trailing slash) | Spec R4-RTSP; C1, C3, C4, C5, C7 |
| Version | RTSP/1.0; audio only | Spec R4-RTSP |
| Server | `Server: Scanner Audio Server 0.0.1`; `Cache-Control: no-cache`; `Supported: play.basic, con.persistent` | Spec R4-RTSP; C4 capture |
| Methods | `OPTIONS, DESCRIBE, SETUP, TEARDOWN, PLAY, GET_PARAMETER` | Spec R4-RTSP (`Public:` header); C4 capture |
| Sequence | `OPTIONS` → `DESCRIBE` → `SETUP` → `PLAY` → `GET_PARAMETER` → RTP → `TEARDOWN` | Spec R4-RTSP (sequence diagram). R4-RTSP labels `GET_PARAMETER` "To Get Supported RTSP Method" and does not give an interval |
| SDP | `m=audio 0 RTP/AVP 0`, `a=control:trackID=1`, `a=control:*`, `s=scanner.au`, **no `a=rtpmap`** | Spec R4-RTSP; C4 capture |
| Codec | G.711 µ-law, RTP payload type 0 (PCMU), 8 kHz mono | Spec R4-RTSP ("Media Format G.711 u-law"; PT 0). 8 kHz mono is implied by static PT 0 (RFC 3551); C1, C3 |
| Transport | `RTP/AVP;unicast;client_port=N` with a **single** port; the reply adds `source`, `server_port` and `ssrc` | Spec R4-RTSP; C3 (`docs/audio.md`) |
| Session timeout | `Session: <id>;timeout=60` | Spec R4-RTSP |
| Keepalive interval | `GET_PARAMETER` every 15–45 s | C1, C3, C2, C5 |
| Concurrency | **One RTSP client at a time** | C1, C6. Not stated in R4-RTSP |
| Fragility | Missing `TEARDOWN`, an outage over ~60 s, or an out-of-order request sequence can lock up port 554 until a power cycle; reconnecting immediately can hang the server | C1 (fw 1.24.00/1.26.01), C6 (fw 1.21.00). Not stated in R4-RTSP |

**Differences from R4-RTSP (not changed yet; hardware decides):**
- The client starts with `DESCRIBE`; R4-RTSP's sequence starts with
  `OPTIONS`. Given the reported sensitivity to request order, consider
  sending `OPTIONS` first.
- `PLAY`, `GET_PARAMETER` and `TEARDOWN` use the request URL
  (`…/au:scanner.au`, no trailing slash); R4-RTSP uses the `Content-Base`
  (`…/au:scanner.au/`).

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
the obtained specifications (R1, R4-VS, R6) or in the community sources
C1–C3. R4-VS says no negotiation or protocol header is needed. The platform's discovery therefore sends the documented read-only
`MDL` command to each address in an operator-approved range, and treats a
`MDL,<model>` reply as a candidate scanner (Impl, Sim; **not HW**).

How the SDS200 treats a command from a second UDP source is undocumented.
The simulator, for example, sends PSI pushes to whichever address spoke
last. Discovery therefore never probes an endpoint that is enrolled or
managed.

## Unresolved questions (hardware verification required)

1. R4-VS fixes UDP 50536 "for compatibility BCD536HP". Is it fixed on all current SDS200 firmware, or configurable in the menu?
2. `VOL`, `SQL` and `POF` are in neither R1 nor R6. Do they exist on the SDS200 over UDP, and what are their exact reply formats and ranges?
3. Are `FER` and `ORER` ever sent over UDP?
4. What units does `QSH,[FRQ]` take (100 Hz?), and what argument format does `JPM` take (decimal 4294967295?)? R6 does not say.
5. Is `HLD` with a trailing empty field accepted? Is there a documented hold-release command, or does `HLD` toggle? R6 does not say.
6. Multi-part replies: R4-VS's example shows every part repeating `CMD,<XML>,`. Does current firmware match, does part 1 carry a footer, and what is the maximum datagram size?
7. Does the SDS200 stop `PSI` pushes without `KAL` over UDP?
8. R6's key-code table has BCD536HP and SDS100 columns only. Which codes does the SDS200 accept, in particular `Q`, `T` and `V`, and which `KEY_MODE` values exist?
9. What RTP packetization interval and packet size does the scanner use? R4-RTSP does not say.
10. R6 documents `U_Id` as an attribute of channel elements. Does the SDS200 also send a `UnitID` element (C1), and on which firmware?
11. How much command traffic can the scanner handle without dropping replies?
12. Does an `MDL` probe from a second UDP source disturb an existing control session or redirect `PSI` pushes? Does the SDS200 answer probes at all while another client is connected?
13. Audio on hardware:
    - Does the SDS200 accept the DESCRIBE → SETUP (single `client_port`) → PLAY → `GET_PARAMETER` → TEARDOWN sequence as implemented?
    - Is the 10 s cooldown enough to avoid the reported hang?
    - Does audio keep flowing while the control port is in use?
