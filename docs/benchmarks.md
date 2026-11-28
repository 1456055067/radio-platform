# Benchmarks

Run with:

```bash
cargo bench -p radio-benches
```

The benchmarks (`tests/benches/benches/hot_paths.rs`) measure single-thread
hot paths with criterion. They are not load tests: there is no network,
TLS, PostgreSQL or real scanner in the loop.

## Results

Machine: Apple M2 Max (12 cores), macOS, release profile (thin LTO,
1 codegen unit), 2026-11-26. Criterion 1 s warm-up, 3 s measurement; the
middle estimate is shown.

| Benchmark | Time | What it means |
|---|---|---|
| `protocol/gsi_frame_and_decode` | 4.12 µs (113 MiB/s) | Frame and parse one simulator `GSI` reply. At the agent's default 2 replies/s per scanner this is far below 0.1 % of a core. |
| `audio/ulaw_decode_20ms` | 16 ns | One 20 ms RTP payload. |
| `audio/opus_encode_20ms` | 43.9 µs | One 20 ms frame: about 0.22 % of a core per streaming scanner, so one core could encode roughly 450 streams. |
| `audio/media_frame_decode` | 21.8 ns | Gateway side of each agent audio frame. |
| `audio/recording_10s_decode_to_16k` | 2.24 ms | Decoding a 10 s recording for transcription (the HTTP request to the model dominates). |
| `audit/chain_link` | 0.96 µs | SHA-256 link of one audit record (AWS-LC). Appends are bounded by the database transaction, not hashing. |
| `api/list_scanners_50` | 81.6 µs | `GET /api/v1/scanners` with 50 scanners through the full middleware (per-IP and per-subject limits, JWT verification, ABAC, metrics) on the in-memory store: about 12 000 requests/s per core before TLS and PostgreSQL. |

### Security event log throttle (2026-11-27)

The rejection path only (`SecurityLog::record`, ADR 0013), with no
subscriber installed: the throttle decision, not the cost of writing a
line. Run with:

```bash
cargo bench -p radio-benches --bench hot_paths -- security/
```

These are **single runs on the development machine**, with no saved
baseline; the two figures per row are two separate runs (the coder's
and the performance reviewer's) on 2026-11-27, with no code change between
them. Treat the spread as machine variance.

| Benchmark | Time (two runs) | What it means |
|---|---|---|
| `security/record_one_source_suppressed` | 45.2 ns / 44.48 ns | One source past its per-source limit: every event is counted and suppressed. |
| `security/record_many_sources_full_table` | 63.4 ns / 68.40 ns | 10 000 sources cycling through the full 4096-slot table, budget spent: most events are untracked. |

The success path does no security-log work; `api/list_scanners_50`
showed no regression against `main` in an A/B run on the same machine.

## Not measured yet

- Throughput with PostgreSQL, TLS and many concurrent clients (a load test
  against a deployed gateway).
- Agents per gateway replica, and WebRTC listeners per replica.
- Everything on real hardware, including the scanner's own limits
  (`docs/hardware-signoff.md`, C10).
