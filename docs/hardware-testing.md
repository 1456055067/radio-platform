# Hardware Testing

## Running

```bash
# Read-only tests
SDS200_ADDR=192.168.1.50:50536 \
  cargo test -p sds-interoperability -- --ignored --test-threads=1 --nocapture

# Include tests that change and restore state (volume, PSI)
SDS200_ADDR=192.168.1.50:50536 SDS200_ALLOW_STATE_CHANGE=1 \
  cargo test -p sds-interoperability -- --ignored --test-threads=1 --nocapture
```

Every test is `#[ignore]`d, so CI never runs them and they never pass
silently. Run with `--test-threads=1`, because the scanner handles one
client conversation at a time.

Prerequisites:
- the scanner's network settings are configured and it is reachable on its
  isolated segment;
- no other client, such as ProScan, is talking to it.

## Test inventory

| Test | State change | Verifies |
|---|---|---|
| `hw_identity` | no | `MDL`, `VER` |
| `hw_capability_probe` | no | Support for read-only commands (GSI, STS, VOL, SQL, FQK, DTM, LCR, URC, GLT FL, VER) |
| `hw_scanner_info` | no | `GSI` structure, element names, frequency/TGID/UID |
| `hw_favorites_list` | no | `GLT,FL` and multi-part framing |
| `hw_read_levels` | no | `VOL` and `SQL` reads (community-reported commands) |
| `hw_set_volume_and_restore` | **yes** | `VOL,n` set and read-back |
| `hw_push_updates` | **yes** | `PSI,500` pushes and `PSI,0` |
| `hw_audio_session_and_teardown` | audio | RTSP sequence, TEARDOWN, frame rate, loss (`--test audio`, needs `SDS200_ALLOW_AUDIO=1`) |
| `hw_audio_reconnect_after_cooldown` | audio | Three sessions 10 s apart (the reported lock-up) |
| `hw_audio_with_control_traffic` | audio | Audio while `GSI` is polled twice a second |

Audio tests open the scanner's RTSP server, which is reported to lock up
until power-cycled after mistakes; they run only with
`SDS200_ALLOW_AUDIO=1`. `SDS200_RTSP` overrides the endpoint (default: the
scanner's IP, port 554). The audio tests were checked against
`sds-simulator --audio` to confirm the test code works; that says nothing
about the hardware.

The full first-session procedure is `docs/hardware-signoff.md`.

## Results log

| Date | Model | Firmware | Tester | Test | Result | Notes |
|---|---|---|---|---|---|---|
| — | — | — | — | — | **No hardware runs recorded yet** | |

Record raw output alongside results. Never edit results to match
expectations.
