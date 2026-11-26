# scanner-recorder

Transmission recording: segments the agents' Opus stream by transmission
events (pre-buffer, post-roll, duration cap), writes Ogg Opus, and stores it
in S3-compatible storage, a local directory, or memory.

**Off by default.** Recording needs a `[recording]` section on the gateway
and `recording_enabled` on the scanner, which only `recording_manage` users
can set. Record only communications you are authorized to record.
