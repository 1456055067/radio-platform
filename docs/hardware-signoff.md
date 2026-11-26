# Hardware Interoperability Sign-off

**Status: not signed off.** No SDS200 has been tested yet. Every protocol
and audio behaviour in this platform is implemented from specifications
and community reports and tested against `sds-simulator` only.

This checklist turns the first hardware session into a repeatable
procedure. Sign-off means every **required** row has a recorded result on a
named firmware version, and `docs/protocol-compliance.md` has been updated
to match what the hardware did (including where it disagreed).

## Before you start

- [ ] Record the model, serial (last 4 digits is enough), firmware (`VER`)
      and the date.
- [ ] Put the scanner on an isolated segment with the test machine. Stop
      every other client (ProScan, Sentinel, VLC).
- [ ] Be able to power-cycle the scanner: the audio server is **reported**
      to lock up after protocol mistakes.
- [ ] Note the scanner's current volume and squelch; state-changing tests
      restore them, but check.
- [ ] Capture traffic for the whole session
      (`tcpdump -i <if> -w sds200-<date>.pcap host <scanner-ip>`); attach
      it to the results.

## 1. Control protocol (UDP 50536)

```bash
SDS200_ADDR=<ip>:50536 cargo test -p sds-interoperability --test hardware -- --ignored --test-threads=1 --nocapture
SDS200_ADDR=<ip>:50536 SDS200_ALLOW_STATE_CHANGE=1 cargo test -p sds-interoperability --test hardware -- --ignored --test-threads=1 --nocapture
```

| # | Required | Check | Test | Answers question |
|---|---|---|---|---|
| C1 | yes | `MDL` and `VER` | `hw_identity` | — |
| C2 | yes | Read-only capability probe | `hw_capability_probe` | 2 |
| C3 | yes | `GSI` structure and fields | `hw_scanner_info` | 10 |
| C4 | yes | Multi-part XML (`GLT,FL`) | `hw_favorites_list` | 6 |
| C5 | yes | `VOL`/`SQL` reads | `hw_read_levels` | 2 |
| C6 | yes | `VOL,n` set and restore | `hw_set_volume_and_restore` | 2 |
| C7 | yes | `PSI` push start and stop | `hw_push_updates` | 7 |
| C8 | no | Hold and resume from the console (`HLD`, `JPM`) | manual (devstack against the real scanner) | 4, 5 |
| C9 | no | Key presses from the virtual scanner | manual | 8 |
| C10 | yes | Load: 2 `GSI`/s for 10 min with no lost replies | manual (agent `[telemetry]` `mode = "poll"`, `interval_ms = 500`) | 11 |
| C11 | yes | Discovery `MDL` probe while a session is active | manual (`POST /sites/{id}/discover`) | 12 |

## 2. Audio (RTSP 554 / RTP)

```bash
SDS200_ADDR=<ip>:50536 SDS200_ALLOW_AUDIO=1 \
  cargo test -p sds-interoperability --test audio -- --ignored --test-threads=1 --nocapture
```

| # | Required | Check | Test | Answers question |
|---|---|---|---|---|
| A1 | yes | DESCRIBE → SETUP → PLAY → TEARDOWN accepted; ≥ 400 frames in 10 s; < 1 % loss | `hw_audio_session_and_teardown` | 13a, 9 |
| A2 | yes | Three sessions 10 s apart all deliver audio (no lock-up) | `hw_audio_reconnect_after_cooldown` | 13b |
| A3 | yes | Audio and `GSI` polling together | `hw_audio_with_control_traffic` | 13c |
| A4 | yes | Live listening in a browser through agent and gateway | manual | — |
| A5 | yes | A recorded transmission plays back and lines up with the squelch | manual | — |
| A6 | no | Packet size and interval from the pcap | manual | 9 |

If A2 fails, raise the agent's `audio.cooldown_secs` and repeat **after a
power cycle**; record the value that works.

## 3. End to end

| # | Required | Check |
|---|---|---|
| E1 | yes | Agent enrolls the scanner and reports it online; health survives a network unplug and replug |
| E2 | yes | Transmission events match what the scanner display shows (frequency, talkgroup, channel) for 20 transmissions |
| E3 | yes | Control lease: volume and squelch from the console change the scanner; a second operator is refused |
| E4 | no | Transcription of real traffic with the chosen model: note word error rate on 20 transmissions |

## Recording results

Add one row per test to the results log in `docs/hardware-testing.md`
(date, model, firmware, tester, test, result, notes) and keep the raw
`--nocapture` output and the pcap. Then update each affected row of
`docs/protocol-compliance.md` to **HW-verified** (with the firmware) or
correct it. Never edit a result to match expectations.

Sign-off is recorded here:

| Date | Model | Firmware | Tester | Required rows passed | Notes |
|---|---|---|---|---|---|
| — | — | — | — | — | **Not signed off** |
