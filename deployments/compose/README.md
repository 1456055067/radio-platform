# compose

No Compose file is provided. To evaluate the platform on one machine, use
`radio-devstack` (simulated scanners, development only; see the main
README). For production, use the Helm chart in `deployments/helm` for the
gateway and `deployments/systemd/radio-agent.service` (or the `agent`
container image) at each site.
