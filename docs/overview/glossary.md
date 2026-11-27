# Glossary

Agent (site box)
:   A small computer at each site that talks to the scanners and connects
    out to the central server.

Audit log
:   A permanent record of who did what and when. Each entry is chained to
    the previous one so that changes or deletions can be detected.

Gateway (central server)
:   The central service that browsers and agents connect to. It checks
    permissions, stores history and routes commands.

Lease
:   Temporary, exclusive control of one scanner by one person.

mTLS (mutual TLS)
:   An encrypted connection where *both* ends prove who they are with a
    certificate. Used between agents and the gateway.

OIDC / single sign-on
:   The standard way the console lets you sign in with your organisation's
    existing account.

PostgreSQL
:   The database the gateway uses to store sites, scanners, history and
    the audit log.

Retention
:   How long recordings, transcripts and history are kept before they are
    deleted automatically.

RTSP / RTP
:   The protocols the SDS200 uses to send its audio over the network.

SDS200
:   Uniden's desktop digital radio scanner, the device this platform
    manages.

Simulator
:   Software that imitates an SDS200, used to build and test the platform
    without real hardware.

Site
:   A location with one or more scanners and an agent.

Squelch
:   The level a signal must reach before the scanner plays it, which keeps
    background noise quiet.

Tenant
:   A separate organisation using the same installation. Tenants cannot see
    each other's data.

Transcript
:   Text produced automatically from a recording by speech-recognition
    software. It will contain errors.

Transmission
:   One burst of radio traffic the scanner received, from when the signal
    opened to when it closed.

UDP 50536
:   The network port the SDS200 listens on for control commands. It has no
    password or encryption.

Virtual scanner
:   The console page that mirrors a scanner's front panel.
