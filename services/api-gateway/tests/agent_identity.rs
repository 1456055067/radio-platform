//! Pins how an agent's identity is taken from its verified client
//! certificate (`tls::agent_identity`), so a change in the X.509 parser
//! (for example x509-parser 0.18 skipping malformed SAN entries instead of
//! failing the whole extension) shows up as a test failure.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::{IpAddr, Ipv4Addr};

use api_gateway::tls::agent_identity;
use rcgen::{CertificateParams, DnType, KeyPair, SanType};
use rustls::pki_types::CertificateDer;

fn cert(cn: Option<&str>, sans: Vec<SanType>) -> CertificateDer<'static> {
    let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
    params.distinguished_name = rcgen::DistinguishedName::new();
    if let Some(cn) = cn {
        params.distinguished_name.push(DnType::CommonName, cn);
    }
    params.subject_alt_names = sans;
    let key = KeyPair::generate().unwrap();
    params.self_signed(&key).unwrap().der().clone()
}

fn dns(name: &str) -> SanType {
    SanType::DnsName(name.try_into().unwrap())
}

fn id(c: &CertificateDer<'_>) -> Option<String> {
    agent_identity(c).map(|a| a.to_string())
}

#[test]
fn dns_san_wins_over_common_name() {
    let c = cert(Some("cn-agent"), vec![dns("san-agent")]);
    assert_eq!(id(&c).as_deref(), Some("san-agent"));
}

#[test]
fn first_dns_san_is_used() {
    let c = cert(None, vec![dns("first"), dns("second")]);
    assert_eq!(id(&c).as_deref(), Some("first"));
}

#[test]
fn non_dns_sans_are_skipped() {
    let ip = SanType::IpAddress(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)));
    let c = cert(Some("cn-agent"), vec![ip.clone(), dns("san-agent")]);
    assert_eq!(id(&c).as_deref(), Some("san-agent"));
    // No DNS SAN at all: fall back to the common name.
    let c = cert(Some("cn-agent"), vec![ip]);
    assert_eq!(id(&c).as_deref(), Some("cn-agent"));
}

#[test]
fn common_name_is_the_fallback() {
    let c = cert(Some("site-a.agent"), vec![]);
    assert_eq!(id(&c).as_deref(), Some("site-a.agent"));
}

#[test]
fn invalid_names_yield_no_identity() {
    // A DNS SAN that is not a valid agent id is not skipped in favour of
    // the CN: the first DNS SAN decides.
    let c = cert(Some("cn-agent"), vec![dns("*.wild")]);
    assert_eq!(id(&c), None);
    let c = cert(Some("has space"), vec![]);
    assert_eq!(id(&c), None);
    let c = cert(None, vec![]);
    assert_eq!(id(&c), None);
}
