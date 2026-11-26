-- Phase 5: transcription (opt-in per scanner, needs recording), full-text
-- search, retention holds, and indexes for retention sweeps.
ALTER TABLE scanners
    ADD COLUMN transcription_enabled BOOLEAN NOT NULL DEFAULT false,
    ADD CONSTRAINT transcription_needs_recording CHECK (NOT transcription_enabled OR recording_enabled);

ALTER TABLE transmissions
    ADD COLUMN transcription_state TEXT
        CHECK (transcription_state IN ('pending', 'done', 'failed', 'skipped')),
    ADD COLUMN transcription_attempts     INTEGER NOT NULL DEFAULT 0 CHECK (transcription_attempts >= 0),
    ADD COLUMN transcription_next_at      TIMESTAMPTZ,
    ADD COLUMN transcription_locked_until TIMESTAMPTZ,
    ADD COLUMN transcription_error        TEXT,
    ADD COLUMN transcript_language        TEXT,
    ADD COLUMN transcript_model           TEXT,
    ADD COLUMN transcribed_at             TIMESTAMPTZ,
    ADD COLUMN hold                       BOOLEAN NOT NULL DEFAULT false,
    -- 'simple': no stemming or stop words; radio traffic is codes and names.
    ADD COLUMN transcript_tsv tsvector
        GENERATED ALWAYS AS (to_tsvector('simple', coalesce(transcript, ''))) STORED;

CREATE INDEX transmissions_transcript_tsv ON transmissions USING GIN (transcript_tsv);
CREATE INDEX transmissions_transcription_due ON transmissions (transcription_next_at)
    WHERE transcription_state = 'pending';
CREATE INDEX transmissions_started ON transmissions (started_at);
CREATE INDEX events_received ON events (received_at);
