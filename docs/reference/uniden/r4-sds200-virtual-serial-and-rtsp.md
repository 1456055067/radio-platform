# R4: SDS200 Virtual Serial on Network and RTSP notes

| | |
|---|---|
| **Publisher** | Uniden |
| **Documents** | *SDS200 Virtual Serial on Network Protocol Specification*, Version 1.00, 2019-01-23 (1st edition 2018-04-13), cited as **R4-VS**; and RTSP notes ("RTSP.pdf", undated), cited as **R4-RTSP** |
| **Covers** | Control over the SDS200's Ethernet port, and audio streaming over RTSP/RTP |
| **On Uniden's site** | [SDS200_Virtual_Serial_on_Network_Specification_V1_00.pdf](https://info.uniden.com/twiki/pub/UnidenMan4/SDS200FirmwareUpdate/SDS200_Virtual_Serial_on_Network_Specification_V1_00.pdf) (4 pages) and [RTSP.pdf](https://info.uniden.com/twiki/pub/UnidenMan4/SDS200FirmwareUpdate/RTSP.pdf) (3 pages), attached to the [SDS200 firmware change log](https://info.uniden.com/twiki/bin/view/UnidenMan4/SDS200FirmwareUpdate) page |
| **Status** | **Obtained and read** (2026-11-27) |
| **Verification level it supports** | Spec |

## Why it matters

These are the two network interfaces the whole platform depends on. Until
they were read, every fact below rested on community reports.

## Control transport (R4-VS)

| Item | What R4-VS says |
|---|---|
| Port | UDP **50536**, "for compatibility BCD536HP" |
| Session | No negotiation and no protocol header; send a datagram containing a remote command, get its reply |
| Command set | Refers to the *Remote Command Specification* ([R6](r6-sds-remote-command-spec-v1-02.md)) |
| Example | `MDL\r` → `MDL,SDS200` |
| Multi-part XML | Long XML replies are split across packets. Use `Footer@EOT` to find the last packet and `Footer@No` to detect a lost one; **retry the request if a packet is lost** |
| Part format | In the `GLT,FL` example every part repeats `GLT,<XML>,` and the XML declaration, and ends with `<Footer No="n" EOT="0"/>` or `EOT="1"` on the last |

Still open: whether the first part always has a footer, the maximum
datagram size, and whether the `<Foot>` spelling some firmware reportedly
uses still occurs. R4-VS's prose says "Foot node" but its example shows
`<Footer>`.

## Audio (R4-RTSP)

| Item | What R4-RTSP says |
|---|---|
| Protocol | RTSP/1.0 on TCP 554; audio only |
| URL | `rtsp://<scanner>/au:scanner.au` |
| Server | `Scanner Audio Server 0.0.1`; `Cache-Control: no-cache` |
| Methods | `OPTIONS, DESCRIBE, SETUP, TEARDOWN, PLAY, GET_PARAMETER` |
| Sequence | `OPTIONS` → `DESCRIBE` → `SETUP` (`…/trackID=1`) → `PLAY` → `GET_PARAMETER` → RTP stream → `TEARDOWN` |
| SDP | `m=audio 0 RTP/AVP 0`, `a=control:trackID=1`, no `a=rtpmap` |
| Codec | G.711 µ-law (RTP payload type 0) |
| Transport | `RTP/AVP;unicast;client_port=N`, a single port; the reply adds `server_port` and `ssrc` |
| Session | `Session: <id>;timeout=60` |

R4-RTSP does **not** cover: how often to send `GET_PARAMETER`, the RTP
packet size or interval, or how the scanner behaves with more than one
client or after a lost session. Those remain community findings, which is
why audio stays **off by default** per scanner
([ADR 0009](../../decisions/0009-audio-pipeline.md)).

## Differences found in the audio client

- It starts with `DESCRIBE`; R4-RTSP starts with `OPTIONS`.
- It sends `PLAY`, `GET_PARAMETER` and `TEARDOWN` to `…/au:scanner.au`;
  R4-RTSP uses the `Content-Base` `…/au:scanner.au/` (trailing slash).

Neither has been changed. Community reports say the scanner's audio
server is sensitive to request order, so these should be settled on
hardware.

## See also

- [Protocol compliance → Transport](../../protocol-compliance.md#transport)
- [Protocol compliance → Audio](../../protocol-compliance.md#audio-rtsp-rtp)
