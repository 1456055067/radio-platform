-- Phase 3: audio. Live audio and recording are opt-in per scanner and off
-- by default; recording requires audio.
ALTER TABLE scanners
    ADD COLUMN audio_enabled     BOOLEAN NOT NULL DEFAULT false,
    ADD COLUMN recording_enabled BOOLEAN NOT NULL DEFAULT false,
    ADD COLUMN rtsp_port         INTEGER NOT NULL DEFAULT 554 CHECK (rtsp_port BETWEEN 1 AND 65535),
    ADD CONSTRAINT recording_needs_audio CHECK (NOT recording_enabled OR audio_enabled);
