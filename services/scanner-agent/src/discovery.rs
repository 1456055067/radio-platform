//! Authorization-gated scanner discovery.
//!
//! The SDS200 has no documented discovery mechanism (no broadcast or
//! multicast announcement is described in the official specification or in
//! the community sources reviewed). Discovery therefore sends the read-only
//! `MDL` (model) command to each address in an operator-chosen range and
//! reports the hosts that answer like a Uniden scanner.
//!
//! Safety rules, all enforced here regardless of what the gateway asks:
//! - every network must lie inside the agent's **local** network policy,
//!   and the port must be an allowed port (the same SSRF rule as
//!   enrollment);
//! - at most [`MAX_HOSTS`] addresses per run;
//! - probes are paced ([`BATCH`] per [`PACE`]);
//! - excluded endpoints are never probed, and the agent adds every scanner
//!   it currently manages. How the scanner treats a second UDP client is
//!   not documented, so a probe must never disturb a managed session.
//! - only `MDL` is sent.
//!
//! Hardware behaviour of probing is **unverified**.

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use ipnet::IpNet;
use platform_common::NetworkPolicy;
use platform_common::wire::Discovered;
use tokio::net::UdpSocket;
use uniden_protocol::{Command, FrameAssembler, FrameLimits, Reply, decode_reply};

/// Maximum addresses probed in one run.
pub const MAX_HOSTS: usize = 1024;
/// Probes sent per pacing interval.
pub const BATCH: usize = 32;
/// Pacing interval.
pub const PACE: Duration = Duration::from_millis(25);
/// How long to wait for answers after the last probe.
pub const LISTEN_AFTER: Duration = Duration::from_millis(1500);

/// Expands and validates a discovery request into probe targets.
///
/// # Errors
/// A human-readable reason if the request breaks policy or is too large.
pub fn plan(
    policy: &NetworkPolicy,
    networks: &[IpNet],
    port: u16,
    exclude: &HashSet<SocketAddr>,
) -> Result<Vec<SocketAddr>, String> {
    if networks.is_empty() {
        return Err("no networks to probe".into());
    }
    if !policy.allowed_ports.contains(&port) {
        return Err(format!("port {port} is not allowed by the agent's policy"));
    }
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for net in networks {
        let net = net.trunc();
        if !policy.allowed_networks.iter().any(|a| a.contains(&net)) {
            return Err(format!("{net} is outside the agent's authorized networks"));
        }
        // Refuse before enumerating huge ranges.
        let host_bits = u32::from(net.max_prefix_len() - net.prefix_len());
        if host_bits > 11 {
            return Err(format!(
                "{net} is too large; at most {MAX_HOSTS} addresses per run"
            ));
        }
        let hosts: Box<dyn Iterator<Item = IpAddr>> = if host_bits == 0 {
            Box::new(std::iter::once(net.addr()))
        } else {
            Box::new(net.hosts())
        };
        for ip in hosts {
            let ep = SocketAddr::new(ip, port);
            if exclude.contains(&ep) || policy.check(ep).is_err() || !seen.insert(ep) {
                continue;
            }
            out.push(ep);
            if out.len() > MAX_HOSTS {
                return Err(format!("more than {MAX_HOSTS} addresses requested"));
            }
        }
    }
    Ok(out)
}

fn model_of(datagram: &[u8]) -> Option<String> {
    let mut asm = FrameAssembler::new(FrameLimits::default());
    let frames = asm.push(datagram).ok()?;
    frames.into_iter().find_map(|f| {
        match decode_reply(
            &Command::GetModel,
            f,
            &uniden_protocol::xml::XmlLimits::default(),
        ) {
            Ok(Reply::Model(m)) => Some(m.0),
            _ => None,
        }
    })
}

/// Probes `targets` and returns those that answered `MDL`.
///
/// # Errors
/// The probe socket cannot be opened or encoding fails.
pub async fn probe(targets: &[SocketAddr]) -> std::io::Result<Vec<Discovered>> {
    let mdl = Command::GetModel
        .encode()
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    let want: HashSet<SocketAddr> = targets.iter().copied().collect();
    let mut sockets: HashMap<bool, UdpSocket> = HashMap::new();
    for v6 in [false, true] {
        if targets.iter().any(|t| t.is_ipv6() == v6) {
            let bind = if v6 {
                SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0)
            } else {
                SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0)
            };
            sockets.insert(v6, UdpSocket::bind(bind).await?);
        }
    }
    let mut found: HashMap<SocketAddr, String> = HashMap::new();
    let mut buf = vec![0u8; 2048];

    let drain = |sock: &UdpSocket, buf: &mut [u8], found: &mut HashMap<SocketAddr, String>| {
        while let Ok((n, from)) = sock.try_recv_from(buf) {
            let from = SocketAddr::new(from.ip().to_canonical(), from.port());
            if want.contains(&from)
                && let Some(m) = buf.get(..n).and_then(model_of)
            {
                found.insert(from, m);
            }
        }
    };

    for chunk in targets.chunks(BATCH) {
        for t in chunk {
            if let Some(s) = sockets.get(&t.is_ipv6()) {
                // A host that refuses the datagram is simply not a scanner.
                let _ = s.send_to(&mdl, t).await;
            }
        }
        for s in sockets.values() {
            drain(s, &mut buf, &mut found);
        }
        tokio::time::sleep(PACE).await;
    }
    let deadline = tokio::time::Instant::now() + LISTEN_AFTER;
    while tokio::time::Instant::now() < deadline {
        for s in sockets.values() {
            drain(s, &mut buf, &mut found);
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let mut out: Vec<Discovered> = found
        .into_iter()
        .map(|(ep, model)| Discovered {
            host: ep.ip(),
            port: ep.port(),
            model,
        })
        .collect();
    out.sort_by_key(|d| (d.host, d.port));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(nets: &[&str], ports: &[u16]) -> NetworkPolicy {
        NetworkPolicy {
            allowed_networks: nets
                .iter()
                .map(|n| n.parse().unwrap_or_else(|e| panic!("{e}")))
                .collect(),
            allowed_ports: ports.to_vec(),
            audio_ports: vec![554],
        }
    }
    fn nets(n: &[&str]) -> Vec<IpNet> {
        n.iter()
            .map(|n| n.parse().unwrap_or_else(|e| panic!("{e}")))
            .collect()
    }

    #[test]
    fn plan_enforces_local_policy_and_limits() {
        let p = policy(&["192.168.50.0/24"], &[50536]);
        let none = HashSet::new();
        let t =
            plan(&p, &nets(&["192.168.50.0/28"]), 50536, &none).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(t.len(), 14, "host addresses only");
        assert!(
            plan(&p, &nets(&["192.168.51.0/28"]), 50536, &none).is_err(),
            "outside policy"
        );
        assert!(
            plan(&p, &nets(&["192.168.0.0/16"]), 50536, &none).is_err(),
            "wider than policy"
        );
        assert!(
            plan(&p, &nets(&["192.168.50.0/28"]), 22, &none).is_err(),
            "port not allowed"
        );
        assert!(plan(&p, &[], 50536, &none).is_err());
        let wide = policy(&["10.0.0.0/8"], &[50536]);
        assert!(
            plan(&wide, &nets(&["10.0.0.0/20"]), 50536, &none).is_err(),
            "too many hosts"
        );
        assert_eq!(
            plan(&wide, &nets(&["10.0.0.0/22"]), 50536, &none).map(|v| v.len()),
            Ok(1022)
        );
        let ex: HashSet<SocketAddr> = ["192.168.50.5:50536"
            .parse()
            .unwrap_or_else(|e| panic!("{e}"))]
        .into();
        let t = plan(&p, &nets(&["192.168.50.0/28"]), 50536, &ex).unwrap_or_else(|e| panic!("{e}"));
        assert!(
            !t.iter().any(|e| ex.contains(e)),
            "excluded endpoints are never probed"
        );
        assert_eq!(
            plan(&p, &nets(&["192.168.50.7/32"]), 50536, &none).map(|v| v.len()),
            Ok(1)
        );
    }

    #[test]
    fn recognises_model_replies_only() {
        assert_eq!(model_of(b"MDL,SDS200\r"), Some("SDS200".into()));
        assert_eq!(model_of(b"ERR\r"), None);
        assert_eq!(model_of(b"SSH-2.0-OpenSSH\r\n"), None);
        assert_eq!(model_of(b""), None);
    }
}
