# Admin guide

This guide is for the people who deploy and run Radio Platform. It goes
further than the [overview](../overview/index.md) and links to the
detailed engineering references wherever exact settings matter.

!!! warning "Simulator only"
    No feature has been verified on SDS200 hardware yet. Plan a pilot with
    one scanner and run the [hardware sign-off](../hardware-signoff.md)
    checklist before relying on the platform.

## The path through this guide

1. [Plan a deployment](planning.md): components, network layout,
   certificates, identity provider and database.
2. [Install](installation.md): the gateway, the web console and a site
   agent, in order.
3. [Configure](configuration.md): the settings that matter most in the
   gateway and agent files.
4. [Users, roles and control](access-control.md): map identity-provider
   roles to permissions; how leases work.
5. [Audio, recording and transcripts](audio-and-recording.md): switch on
   content features deliberately, with retention.
6. [Run and maintain](operations-overview.md): health checks, metrics,
   backups, upgrades and incident response.

## Detailed references

| Document | Use it for |
|---|---|
| [Deployment](../deployment.md) | Every deployment setting, PKI, database TLS, packaging |
| [Operations](../operations.md) | Health signals, metrics, backup, incidents, troubleshooting |
| [API reference](../api-reference.md) | REST endpoints, permissions, leases, errors |
| [Hardware testing](../hardware-testing.md) | Running the opt-in tests against a real scanner |
| [Hardware sign-off](../hardware-signoff.md) | The acceptance checklist for real hardware |
