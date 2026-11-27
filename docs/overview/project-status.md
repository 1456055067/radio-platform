# Project status

!!! warning "The key fact"
    **No feature has been verified on a real SDS200 yet.** Everything has
    been built and tested against a simulator that behaves the way Uniden's
    documents and other open-source projects say the scanner behaves.
    Hardware testing is the next major milestone.

## What has been built

| Stage | What it delivers | State |
|---|---|---|
| 1. Protocol foundation | The software that speaks the scanner's language | Built, simulator-tested |
| 2. Scanner management | Site boxes, central server, sign-in, permissions, control, audit, database | Built, simulator-tested |
| 3. Audio | Live listening in the browser and recording | Built, simulator-tested, off by default |
| 4. Web console | The website people use | Built, simulator-tested |
| 5. Advanced features | Transcripts, search, retention, analytics | Built; transcription tested against a stand-in service only |
| 6. Production hardening | Security tests, tamper-evident audit, monitoring, packaging | Built |

## Why hardware testing matters

Most of the scanner's behaviour is now described in Uniden's own SDS200
documents, which the project read in November 2026. Some features the
platform uses, such as setting volume and squelch directly, appear only in
what other projects have observed. And a document only says how the
scanner *should* behave on the firmware of its date. Until each behaviour
is checked on a real scanner, it is treated as unconfirmed. The
[Uniden reference](../reference/uniden/index.md) section shows exactly
which documents the project has and which it is still missing.

## Where to follow progress

- [Roadmap](../roadmap.md): every item and its state.
- [Hardware sign-off](../hardware-signoff.md): the checklist that will be
  run on real scanners.
