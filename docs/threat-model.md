# Threat Model (Phases 1–6)

The method is STRIDE applied to the agent ↔ scanner boundary. The platform
threat model is completed in Phase 6.

| # | Threat | Vector | Mitigation | Residual risk |
|---|---|---|---|---|
| T1 | Spoofed replies | Attacker on the scanner segment sends UDP from the scanner's IP and port | Connected socket filters other sources | Spoofing on the same L2 segment remains possible; isolate the segment |
| T2 | Command injection | Operator input with `\r` or `,` | `Arg`/`KeyCode` validation; `RawCommand` allowed for admins only (Phase 2 ABAC) | None known |
| T3 | Memory exhaustion | Huge or endless XML, many parts, many fields | `FrameLimits`, `XmlLimits`, bounded buffer | Within limits |
| T4 | XML entity attacks (billion laughs, XXE) | Malicious DTD | DTDs rejected in assembler and parser; only predefined and numeric entities | None known |
| T5 | Parser crash (DoS) | Malformed bytes | No-panic lints, proptest, fuzzing | Continuous fuzzing planned |
| T6 | Unintended state change | Retried write after a lost reply | Writes never retried; `state_change_unknown` reported | Operator must re-read state |
| T7 | Queue flooding | Many concurrent callers | Bounded queue → `QueueFull` | Phase 2 rate limiting |
| T8 | Eavesdropping or control by LAN peers | Plaintext native protocol | Network isolation (documented) | Inherent to the device |
| T9 | RTSP lock-up of the scanner | Multiple or abrupt RTSP clients | Phase 3 design: single client, `TEARDOWN`, cooldown | Firmware defect; recovery may need a power cycle |
| T10 | Destructive undocumented commands (`MSM` reboot, `POF`) | Raw command misuse | Raw commands are treated as writes; admin-only (Phase 2) | Requires ABAC |
| T11 | SSRF through enrollment | Enrolling an arbitrary address (metadata IP, LAN host) | Site `allowed_networks` + port allow-list at enrollment; agent-local policy before any packet (ADR 0005) | Operator must configure ranges narrowly |
| T12 | Rogue agent | Attacker connects to the agent listener | Mandatory client certs from the agent CA; identity must be registered; CRL checking with reload and forced re-verification | Revocation takes effect within `crl_refresh_secs`; no OCSP |
| T13 | Compromised gateway/DB directs agents | Malicious `Configure` | Agent enforces its own policy; tested in `full_stack` | Gateway can still issue in-policy commands |
| T14 | Cross-tenant access | Guessing IDs, forged claims | JWT signature + iss/aud; tenant-scoped storage; 404 for foreign resources | IdP compromise is out of scope |
| T15 | Agent identity hijack | Registering another tenant's agent id | Refused with 409 | — |
| T16 | Token replay / long sessions | Stolen bearer tokens | Short `exp`, `max_session_secs`, live stream closes at expiry, TLS 1.3 | Tokens are bearer credentials |
| T17 | Event spoofing by an agent | Agent reports scanners it does not manage | Gateway ignores events/telemetry for scanners not assigned to that agent | — |
| T18 | Replay flood after outage | Large backlog | Bounded buffer (drop-oldest, counted); dedupe by `(agent, session, seq)` | Oldest events lost beyond the bound |
| T19 | Control contention | Two operators issuing writes, possibly on different replicas | Exclusive leases in the shared database with an atomic upsert (tested with 8 concurrent acquirers) | Lease expiry depends on replica clocks (NTP) |
| T20 | API abuse | Floods, huge bodies, slowloris, token-verification floods | Per-IP limit before auth, per-subject limit, body limit, timeouts, connection caps, TLS handshake timeout | Limits are per replica; large botnets need upstream protection |
| T21 | XSS in the console | Hostile text from a scanner (names, popups) or the API | React escaping only, never raw HTML; CSP `script-src 'self'` | Tokens are in memory, so a successful XSS could use them during the session |
| T22 | Clickjacking of control buttons | Framing the console | `frame-ancestors 'none'`, `X-Frame-Options: DENY` | — |
| T23 | Token theft at rest | Malware or other origins reading storage | Tokens are never written to localStorage or sessionStorage | Memory is readable by same-origin script (see T21) |
| T24 | Development backdoor shipped | `radio-devstack` exposed or deployed | Loopback-only bind enforced; separate binary; the gateway has no `/dev` routes (tested) | Operator error: do not package the devstack |
| T25 | Abandoned control | Operator closes the tab while holding a lease | Release on `pagehide`; renewal only while open; lease TTL ≤ 300 s from the UI; admin revoke | Up to one TTL if the release request is lost |
| T26 | CSV formula injection | Exported fields starting with `=`, `+`, `-` or `@` | Prefixed with `'` | — |
| T27 | Bearer token in WebSocket subprotocol logged | Proxy or access logs record `Sec-WebSocket-Protocol` | Documented: do not log that header; short token lifetimes | Depends on deployment |
| T28 | Discovery as a scanning tool | Operator or compromised gateway asks an agent to sweep networks | Gateway checks site networks and ports; the agent re-checks its own policy; 1024-host cap; paced; read-only `MDL` only; `scanner_manage` required; audited | Probe effect on real scanners unverified |
| T29 | Discovery disturbs a live session | `MDL` from a second UDP source reaches a managed scanner | Enrolled and managed endpoints are never probed | Unenrolled scanners being used by other software can still be probed |
| T30 | Spoofed X-Forwarded-For | Client forges the header to evade per-IP limits or falsify audit IPs | Header ignored unless the TCP peer is a trusted proxy; right-most untrusted hop used | Misconfigured trust lists |
| T31 | Cluster bus abuse | Party with database access publishes commands | Same trust boundary as the database (ADR 0008) | Protect database credentials |
| T32 | Journal tampering or disclosure on the agent host | Local user reads or edits buffered events | `0700`/`0600` permissions; events carry metadata only, no audio | Root on the agent host is trusted |
| T33 | Stale or missing CRL | CA stops publishing | Expired CRL refuses all agents (fail closed); failed reload keeps the last good CRL | Outage until a fresh CRL is published |
| T34 | Unauthorized listening | User without the right role listens or replays | `audio_listen` + site scope on every WHEP offer and playback; audit records | IdP role mapping must be correct |
| T35 | Unlawful recording | Recording enabled where not authorized | Off by default at gateway and scanner level; `recording_manage` required; UI warnings; audited | Organisational policy |
| T36 | Scanner audio lock-up (DoS) | Extra RTSP clients, missing TEARDOWN, rapid reconnects | Agent is the only client; TEARDOWN always; cooldown and backoff; linger; simulator lock-up model in tests | Firmware behaviour unverified on hardware |
| T37 | SSRF via RTSP port | Admin sets `rtsp_port` to reach another service | Gateway `audio_ports` allow-list; agent's local `audio_ports` + networks | — |
| T38 | Spoofed RTP from the LAN | Host on the scanner segment injects audio | Source IP must be the scanner; PT 0 only; jitter buffer bounds | Same-L2 spoofing of the scanner IP (as T1) |
| T39 | Hostile agent media | Agent sends frames for other scanners or malformed frames | Frames only for its assigned scanners; malformed frames close the connection | — |
| T40 | Media port abuse | Internet hosts hit the WebRTC UDP port | STUN/DTLS only accepted for created sessions (str0m `accepts`); session limits; sessions expire if not connected in 30 s | Volumetric floods need upstream filtering |
| T41 | Recording exfiltration | Bucket misconfiguration or stolen credentials | Tenant-prefixed keys, env-only credentials, no public URLs (playback proxied and audited) | Bucket policy and encryption are the operator's responsibility |
| T42 | Audio sent to an unintended service | Misconfigured or hijacked transcription endpoint, redirect, plaintext network | Off by default; HTTPS required off loopback; redirects not followed; optional private CA; endpoint logged at start-up and shown to `transcription_manage` | A hosted service receives the audio by design; choose a local model where that is not acceptable |
| T43 | Transcript disclosure | Users without listening rights read content through transcripts or search | Transcript fields removed without `audio_listen`; `q` requires `audio_listen`; searches audited | Displaying a transcript in a list is not audited individually (playback and searches are) |
| T44 | Misleading transcripts | Model invents or mishears text (silence, noise, narrowband audio) | Short recordings skipped; UI labels every transcript machine-generated with model and time; recording stays the record of truth | Accuracy depends on the model; not measured on real scanner audio |
| T45 | Hostile transcription service | Huge, slow or malformed responses; markup in text | 1 MB cap, request timeout, JSON parsing, text sanitised and rendered as text | — |
| T46 | Tampered recordings in storage | Attacker with bucket write access plants crafted Ogg files | Page checksums, packet and size limits, bounded decode (2 h); fuzzed | Content substitution is not detected (no signatures) |
| T47 | Evidence destruction | Retention or a user deletes material under legal hold | Holds exempt from retention and manual deletion; deletions and sweeps audited; audit log never swept | A user with `recording_manage` can release a hold (audited) |
| T49 | Audit tampering | DB administrator, SQL injection or a bug edits or deletes audit records | Append-only trigger; per-tenant hash chain; `/audit/verify`; heads logged hourly | Someone who rewrites records **and** head can rebuild a valid chain unless heads are anchored outside the database |
| T50 | Authorization regression | A new or changed route forgets a permission or tenant check | Route-by-route matrix test; fails for unlisted routes | Covers documented routes; logic inside a handler (for example site scope on list filters) is tested separately |
| T51 | Metrics leakage | Scraping reveals identifiers or reaches the Internet | Separate listener, warning if not loopback, NetworkPolicy, route-template labels | Counts and route names are visible to whoever can scrape |
| T52 | Supply-chain substitution of images | Registry or pipeline compromise | Distroless base, SBOM and provenance attestations, cosign keyless signatures; deploy by digest | Base images pinned by tag in the Dockerfile; pin by digest in production |
| T53 | Unclean shutdown | Orchestrator kills the process mid-request | SIGTERM handled; journal written durably; commands never retried automatically | In-flight commands on a stopping replica fail |
| T48 | Expensive queries | Very long analytics windows or search terms | Window caps, `limit` 1–50, 256-character queries, GIN index, rate limits | — |
