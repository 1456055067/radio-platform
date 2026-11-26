//! Scanner network policy.
//!
//! The policy restricts which scanner endpoints may be contacted. It is the
//! primary defence against SSRF through scanner enrollment. It is enforced
//! both by the API gateway (at enrollment) and by each scanner agent (before
//! connecting), so a compromised gateway still cannot direct an agent to
//! arbitrary hosts.

use std::net::{IpAddr, SocketAddr};

use ipnet::IpNet;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Why an endpoint was refused.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PolicyViolation {
    /// Never-valid address class (unspecified, multicast, broadcast).
    #[error("address {0} is not a unicast host address")]
    NotUnicast(IpAddr),
    /// Address is outside every authorized scanner network.
    #[error("address {0} is outside the authorized scanner networks")]
    NetworkNotAllowed(IpAddr),
    /// Port is not an authorized scanner port.
    #[error("port {0} is not an authorized scanner port")]
    PortNotAllowed(u16),
}

/// Authorized scanner networks and ports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkPolicy {
    /// Authorized networks (CIDR). Empty means nothing is authorized.
    pub allowed_networks: Vec<IpNet>,
    /// Authorized UDP command ports.
    #[serde(default = "default_ports")]
    pub allowed_ports: Vec<u16>,
    /// Authorized RTSP (TCP) audio ports.
    #[serde(default = "default_audio_ports")]
    pub audio_ports: Vec<u16>,
}

fn default_ports() -> Vec<u16> {
    vec![50536]
}

fn default_audio_ports() -> Vec<u16> {
    vec![554]
}

impl Default for NetworkPolicy {
    /// Deny-all policy (no networks), allowing port 50536.
    fn default() -> Self {
        Self {
            allowed_networks: Vec::new(),
            allowed_ports: default_ports(),
            audio_ports: default_audio_ports(),
        }
    }
}

impl NetworkPolicy {
    /// Checks an endpoint.
    ///
    /// Loopback addresses are permitted only if a configured network
    /// contains them (for example `127.0.0.1/32` for the simulator).
    ///
    /// # Errors
    /// Returns the first violated rule.
    pub fn check(&self, endpoint: SocketAddr) -> Result<(), PolicyViolation> {
        let ip = endpoint.ip().to_canonical();
        let not_unicast = match ip {
            IpAddr::V4(v4) => v4.is_unspecified() || v4.is_multicast() || v4.is_broadcast(),
            IpAddr::V6(v6) => v6.is_unspecified() || v6.is_multicast(),
        };
        if not_unicast {
            return Err(PolicyViolation::NotUnicast(ip));
        }
        if !self.allowed_networks.iter().any(|n| n.contains(&ip)) {
            return Err(PolicyViolation::NetworkNotAllowed(ip));
        }
        if !self.allowed_ports.contains(&endpoint.port()) {
            return Err(PolicyViolation::PortNotAllowed(endpoint.port()));
        }
        Ok(())
    }

    /// Checks an RTSP audio endpoint: same networks, audio ports.
    ///
    /// # Errors
    /// Returns the first violated rule.
    pub fn check_audio(&self, endpoint: SocketAddr) -> Result<(), PolicyViolation> {
        let as_control = Self {
            allowed_ports: self.audio_ports.clone(),
            ..self.clone()
        };
        as_control.check(endpoint)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(nets: &[&str]) -> NetworkPolicy {
        NetworkPolicy {
            allowed_networks: nets.iter().map(|n| n.parse().unwrap()).collect(),
            allowed_ports: vec![50536],
            audio_ports: vec![554],
        }
    }

    #[test]
    fn audio_ports_are_separate() {
        let p = policy(&["192.168.50.0/24"]);
        assert!(p.check_audio("192.168.50.7:554".parse().unwrap()).is_ok());
        assert!(
            p.check_audio("192.168.50.7:50536".parse().unwrap())
                .is_err()
        );
        assert!(p.check("192.168.50.7:554".parse().unwrap()).is_err());
        assert!(p.check_audio("10.0.0.1:554".parse().unwrap()).is_err());
    }

    #[test]
    fn default_denies_everything() {
        let p = NetworkPolicy::default();
        assert!(p.check("192.168.1.5:50536".parse().unwrap()).is_err());
    }

    #[test]
    fn allows_only_configured_ranges_and_ports() {
        let p = policy(&["192.168.50.0/24", "fd00:50::/64"]);
        assert!(p.check("192.168.50.7:50536".parse().unwrap()).is_ok());
        assert!(p.check("[fd00:50::7]:50536".parse().unwrap()).is_ok());
        assert_eq!(
            p.check("192.168.51.7:50536".parse().unwrap()),
            Err(PolicyViolation::NetworkNotAllowed(
                "192.168.51.7".parse().unwrap()
            ))
        );
        assert_eq!(
            p.check("192.168.50.7:22".parse().unwrap()),
            Err(PolicyViolation::PortNotAllowed(22))
        );
        // Metadata endpoints and loopback are refused unless configured.
        assert!(p.check("169.254.169.254:50536".parse().unwrap()).is_err());
        assert!(p.check("127.0.0.1:50536".parse().unwrap()).is_err());
    }

    #[test]
    fn rejects_non_unicast_even_if_covered() {
        let p = policy(&["0.0.0.0/0", "::/0"]);
        assert!(matches!(
            p.check("0.0.0.0:50536".parse().unwrap()),
            Err(PolicyViolation::NotUnicast(_))
        ));
        assert!(p.check("224.0.0.1:50536".parse().unwrap()).is_err());
        assert!(p.check("255.255.255.255:50536".parse().unwrap()).is_err());
        assert!(p.check("[ff02::1]:50536".parse().unwrap()).is_err());
    }

    #[test]
    fn ipv4_mapped_ipv6_is_canonicalised() {
        let p = policy(&["10.0.0.0/8"]);
        assert!(p.check("[::ffff:10.1.2.3]:50536".parse().unwrap()).is_ok());
        assert!(p.check("[::ffff:11.1.2.3]:50536".parse().unwrap()).is_err());
    }
}
