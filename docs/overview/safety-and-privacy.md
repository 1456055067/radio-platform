# Safety, privacy and the law

## Only monitor what you are allowed to

Laws on listening to, recording and keeping radio communications differ
by country, state and the type of traffic. **Only monitor and record
communications you are legally authorised to receive and retain.** Check
with your legal or compliance team before switching on recording or
transcription.

Radio Platform does not and will not decrypt protected (encrypted) radio
communications.

## Off by default

The features that capture content are all switched off until an
administrator turns them on, one scanner at a time:

- live listening;
- recording;
- transcription (turning recordings into text).

How long recordings, transcripts and history are kept is also set by an
administrator. Anything without a limit is kept until deleted.

## Where your data goes

- Recordings are stored in storage your organisation controls.
- Transcription can run on your own server. If an administrator points it
  at an outside service instead, **every recording sent for transcription
  goes to that service**.
- The audit log is never deleted automatically.

## Keeping it secure

- People sign in through your organisation's existing single sign-on.
- Every connection between browser, central server and site boxes is
  encrypted.
- Each site box has its own certificate and can be cut off individually.
- The scanners themselves have no security of their own, so they must sit
  on a private network that only the site box can reach. Never connect a
  scanner's network port directly to the internet.
- Every action is logged in an audit trail designed to reveal tampering.

The project maps its security controls to the NIST SP 800-53 catalogue.
That mapping is evidence for reviewers, **not a compliance certification**.
See [NIST 800-53 controls](../nist-800-53-controls.md).
