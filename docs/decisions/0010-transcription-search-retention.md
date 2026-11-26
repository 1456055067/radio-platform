# ADR 0010: Transcription, search, retention and analytics

- **Status:** Accepted
- **Date:** 2026-11-25

## Context

Phase 3 stores recorded transmissions as Ogg Opus. Phase 5 adds machine
transcripts, full-text search over them, retention rules, and simple
traffic analytics. Constraints:

- Transcripts reveal the content of communications, just like audio.
- Audio sent to a transcription service leaves the platform. Many
  operators will need it to stay on premises.
- Several gateway replicas may run at once (ADR 0008).
- Whisper and similar models invent text on silence and noise.

## Decision

1. **Whisper-compatible HTTP services, not an embedded model.** The
   gateway posts each recording as 16 kHz mono WAV to a configured
   endpoint, in one of two documented request styles:
   - `openai`: `POST {endpoint}/v1/audio/transcriptions` (OpenAI and
     compatible self-hosted servers);
   - `whisper_cpp`: `POST {endpoint}/inference` (whisper.cpp's server).

   No model or GPU runtime enters the gateway, so the build and the
   security boundary stay small. Operators choose local or hosted models.
2. **Off by default, twice.** Transcription runs only when the gateway has
   a `[transcription]` section **and** the scanner has transcription
   enabled. Transcription requires recording, and enabling it requires
   `transcription_manage`.
3. **Audio must not leave by accident.**
   - The endpoint must use HTTPS unless it is a loopback address or
     `allow_http` is set.
   - The API key comes from an environment variable, never the
     configuration file.
   - The gateway logs the endpoint host at start-up.
4. **A durable queue on the transmission row.** Job state
   (`pending`, `done`, `failed`, `skipped`), attempts, next attempt and a
   claim lease live in `transmissions`. Each replica claims jobs with
   `FOR UPDATE SKIP LOCKED`, so the work spreads across replicas and
   survives restarts. Transcription is not a scanner command, so retrying
   it is safe: transient failures (network, timeout, 429, 5xx) retry with
   backoff up to `max_attempts`; anything else fails at once.
5. **Transcripts are content.**
   - Reading them, and searching their text, needs `audio_listen` as well
     as `event_view`. Without it, transcript fields are removed from
     responses.
   - Text searches and re-transcription are audited.
   - The UI labels every transcript as machine-generated.
   - Recordings shorter than `min_duration_ms` are skipped rather than
     sent, which avoids the worst invented text.
6. **Full-text search** uses a generated `tsvector` (`simple`
   configuration, no stemming, because radio traffic is full of codes and
   unit names) with a GIN index and `websearch_to_tsquery`. The in-memory
   store approximates it with whole-word, all-terms matching.
7. **Retention is time-based, global, and off until configured.**
   - Separate limits for recordings, transcripts, transmissions and
     events. Recordings and transcripts never outlive their transmission.
   - A sweeper on every replica deletes the stored object first and then
     clears the key (only if it still matches). A crash between the two
     steps leaves a key that the next sweep cleans up. Replicas sweeping
     at once are harmless: deleting an object twice is a no-op.
   - A **hold** on a transmission exempts it from retention and from
     manual deletion until released.
   - Each sweep that deletes anything writes one `system:retention` audit
     record per tenant. The audit log itself is never swept.
8. **Analytics** are aggregate SQL over transmissions: counts and airtime
   per hour or day, and top talkgroups, channels, frequencies or scanners.
   The time window is capped so queries stay bounded.

## Consequences

- No transcription happens unless an operator configures a service and
  enables it per scanner.
- Transcript quality depends entirely on the chosen model. Narrowband
  (8 kHz) radio audio is hard for every model. Nothing is verified on
  real scanner audio yet.
- The two request styles are implemented from their public
  documentation and tested only against a stub server.
- Retention is global, not per tenant or per site. Per-tenant policies
  would need a policy table.
- Text search is not offered for audio without a transcript.
