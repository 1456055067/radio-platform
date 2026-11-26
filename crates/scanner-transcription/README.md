# scanner-transcription

**Status: implemented (Phase 5); tested against a stub service only.**
See [ADR 0010](../../docs/decisions/0010-transcription-search-retention.md).

- `decode_ogg_opus` turns a stored recording (Ogg Opus) into 16 kHz mono
  PCM, trimming the encoder pre-skip; `wav` writes it as a WAV file.
- `WhisperClient` posts the WAV to a Whisper-compatible HTTP service:
  - `api = "openai"`: `POST {endpoint}/v1/audio/transcriptions`;
  - `api = "whisper_cpp"`: `POST {endpoint}/inference`.
- `TranscriptionConfig::validate` enforces HTTPS unless the host is
  loopback (or `allow_http`), rejects credentials in the URL, and reads
  the API key only from the environment variable named in `api_key_env`.

The request styles follow the services' public documentation; they have
not been tested against the real servers. Machine transcripts can be wrong
and Whisper-family models can invent text on silence; present them as
machine-generated. The gateway's worker (`api_gateway::transcription`)
owns the queue, retries and storage.
