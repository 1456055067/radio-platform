//! Agent certificate revocation: real TLS 1.3 handshakes against the agent
//! listener with CRLs that change at runtime.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use api_gateway::crl::CrlReloader;
use api_gateway::serve::{TlsSource, serve_with};
use axum::Router;
use axum::routing::get;
use rcgen::{
    BasicConstraints, CertificateParams, CertificateRevocationListParams, DnType,
    ExtendedKeyUsagePurpose, IsCa, Issuer, KeyIdMethod, KeyPair, RevocationReason,
    RevokedCertParams, SanType, SerialNumber,
};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use time::{Duration, OffsetDateTime};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;

struct Pki {
    dir: PathBuf,
    issuer: Issuer<'static, KeyPair>,
}

impl Pki {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("rp-crl-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut ca = CertificateParams::new(Vec::<String>::new()).unwrap();
        ca.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca.distinguished_name.push(DnType::CommonName, "Agent CA");
        let key = KeyPair::generate().unwrap();
        std::fs::write(dir.join("ca.pem"), ca.self_signed(&key).unwrap().pem()).unwrap();
        let issuer = Issuer::new(ca, key);
        let pki = Self { dir, issuer };
        pki.leaf(
            "gateway",
            "localhost",
            1,
            ExtendedKeyUsagePurpose::ServerAuth,
        );
        pki.leaf(
            "agent-a",
            "agent-a",
            10,
            ExtendedKeyUsagePurpose::ClientAuth,
        );
        pki.leaf(
            "agent-b",
            "agent-b",
            11,
            ExtendedKeyUsagePurpose::ClientAuth,
        );
        pki
    }
    fn leaf(&self, file: &str, dns: &str, serial: u64, eku: ExtendedKeyUsagePurpose) {
        let mut p = CertificateParams::new(vec![dns.to_owned()]).unwrap();
        p.subject_alt_names
            .push(SanType::IpAddress([127, 0, 0, 1].into()));
        p.distinguished_name.push(DnType::CommonName, dns);
        p.extended_key_usages = vec![eku];
        p.serial_number = Some(SerialNumber::from(serial));
        let k = KeyPair::generate().unwrap();
        std::fs::write(
            self.p(&format!("{file}.pem")),
            p.signed_by(&k, &self.issuer).unwrap().pem(),
        )
        .unwrap();
        std::fs::write(self.p(&format!("{file}-key.pem")), k.serialize_pem()).unwrap();
    }
    fn crl(&self, number: u64, revoked: &[u64], next_update: OffsetDateTime) {
        let now = OffsetDateTime::now_utc();
        let params = CertificateRevocationListParams {
            this_update: next_update.min(now) - Duration::hours(1),
            next_update,
            crl_number: SerialNumber::from(number),
            issuing_distribution_point: None,
            revoked_certs: revoked
                .iter()
                .map(|s| RevokedCertParams {
                    serial_number: SerialNumber::from(*s),
                    revocation_time: now - Duration::minutes(5),
                    reason_code: Some(RevocationReason::KeyCompromise),
                    invalidity_date: None,
                })
                .collect(),
            key_identifier_method: KeyIdMethod::Sha256,
        };
        std::fs::write(
            self.p("crl.pem"),
            params.signed_by(&self.issuer).unwrap().pem().unwrap(),
        )
        .unwrap();
    }
    fn p(&self, n: &str) -> PathBuf {
        self.dir.join(n)
    }
}

impl Drop for Pki {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// True if an mTLS HTTP request with `who`'s certificate gets a response.
async fn accepted(pki: &Pki, addr: SocketAddr, who: &str) -> bool {
    let mut roots = rustls::RootCertStore::empty();
    for c in CertificateDer::pem_file_iter(pki.p("ca.pem")).unwrap() {
        roots.add(c.unwrap()).unwrap();
    }
    let chain: Vec<_> = CertificateDer::pem_file_iter(pki.p(&format!("{who}.pem")))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let key = PrivateKeyDer::from_pem_file(pki.p(&format!("{who}-key.pem"))).unwrap();
    let cfg = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])
    .unwrap()
    .with_root_certificates(roots)
    .with_client_auth_cert(chain, key)
    .unwrap();
    let tcp = TcpStream::connect(addr).await.unwrap();
    let Ok(mut tls) = tokio_rustls::TlsConnector::from(Arc::new(cfg))
        .connect(ServerName::try_from("localhost").unwrap(), tcp)
        .await
    else {
        return false;
    };
    // TLS 1.3: the server judges the client certificate after the client
    // finishes, so the verdict shows up on the first exchange.
    if tls
        .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await
        .is_err()
    {
        return false;
    }
    let mut buf = Vec::new();
    let _ = tls.read_to_end(&mut buf).await;
    buf.starts_with(b"HTTP/1.1 200")
}

fn reloader(
    pki: &Pki,
) -> (
    CrlReloader,
    tokio::sync::watch::Receiver<Arc<rustls::ServerConfig>>,
) {
    let p = |n: &str| -> PathBuf { pki.p(n) };
    CrlReloader::new(
        Path::new(&p("gateway.pem")),
        Path::new(&p("gateway-key.pem")),
        Path::new(&p("ca.pem")),
        Path::new(&p("crl.pem")),
    )
    .unwrap()
}

#[tokio::test]
async fn revoked_agents_are_refused_after_reload_and_expired_crls_fail_closed() {
    let pki = Pki::new();
    let day = OffsetDateTime::now_utc() + Duration::days(1);
    pki.crl(1, &[], day);
    let (mut reloader, rx) = reloader(&pki);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let cancel = CancellationToken::new();
    let _guard = cancel.clone().drop_guard();
    tokio::spawn(serve_with(
        listener,
        Some(TlsSource::Reloadable(rx)),
        Router::new().route("/", get(|| async { "ok" })),
        16,
        cancel,
    ));

    assert!(accepted(&pki, addr, "agent-a").await);
    assert!(accepted(&pki, addr, "agent-b").await);
    assert!(
        !reloader.reload_if_changed().unwrap(),
        "unchanged CRL is not reloaded"
    );

    pki.crl(2, &[11], day);
    assert!(reloader.reload_if_changed().unwrap());
    assert!(accepted(&pki, addr, "agent-a").await);
    assert!(
        !accepted(&pki, addr, "agent-b").await,
        "revoked agent refused"
    );

    // A garbage CRL is rejected and the previous one stays in force.
    std::fs::write(pki.p("crl.pem"), "not a crl").unwrap();
    assert!(reloader.reload_if_changed().is_err());
    assert!(accepted(&pki, addr, "agent-a").await);
    assert!(!accepted(&pki, addr, "agent-b").await);

    // An expired CRL fails closed for everyone.
    pki.crl(3, &[], OffsetDateTime::now_utc() - Duration::hours(1));
    assert!(reloader.reload_if_changed().unwrap());
    assert!(
        !accepted(&pki, addr, "agent-a").await,
        "stale CRL must not pass"
    );
}
