//! Real TLS to PostgreSQL with a private CA and `sslmode=verify-full`.
//!
//! Needs a TLS-enabled server whose certificate is issued by a test CA for
//! the DNS name `pg-tls.test` (see the `postgres` job in
//! `.github/workflows/ci.yml` for the recipe):
//!
//! ```text
//! TEST_TLS_DATABASE_URL='postgres://postgres:not-a-real-secret@pg-tls.test:5433/postgres?sslmode=verify-full&sslrootcert=/path/ca.crt' \
//! TEST_TLS_OTHER_CA=/path/other.crt \
//! cargo test -p platform-storage --test postgres_tls -- --include-ignored
//! ```
//!
//! Without an `/etc/hosts` entry for `pg-tls.test`, add `&hostaddr=127.0.0.1`
//! to the URL. The policy then classifies the target as local (the TCP
//! destination is loopback), but the certificate is still verified against
//! the name `pg-tls.test`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::time::Duration;

use platform_storage::{MessageBus, PgBus, PgStore, PgTarget, TransportVerdict};

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok()
}

/// Returns `url` with one query parameter replaced (or added).
fn with_param(url: &str, key: &str, value: &str) -> String {
    let mut u = url::Url::parse(url).unwrap();
    let pairs: Vec<(String, String)> = u
        .query_pairs()
        .filter(|(k, _)| k != key)
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    u.query_pairs_mut()
        .clear()
        .extend_pairs(pairs)
        .append_pair(key, value);
    u.to_string()
}

async fn connect_err(url: &str) -> String {
    // Loopback or verify-full by construction of the test URLs.
    let checked = PgTarget::parse(url, &env).unwrap().check(false).unwrap();
    match PgStore::connect_target(url, &checked, 1).await {
        Ok(_) => panic!("connection unexpectedly succeeded"),
        Err(e) => e.to_string(),
    }
}

#[tokio::test]
#[ignore = "requires TEST_TLS_DATABASE_URL"]
async fn postgres_tls_verify_full_with_private_ca() {
    let admin_url = std::env::var("TEST_TLS_DATABASE_URL").expect("set TEST_TLS_DATABASE_URL");
    let other_ca = std::env::var("TEST_TLS_OTHER_CA").expect("set TEST_TLS_OTHER_CA");
    let secret = url::Url::parse(&admin_url)
        .unwrap()
        .password()
        .unwrap_or("not-a-real-secret")
        .to_owned();

    let admin_target = PgTarget::parse(&admin_url, &env).unwrap();
    assert_eq!(admin_target.ssl_mode(), "verify-full");
    let admin_checked = admin_target.check(false).unwrap();
    let verdict = admin_checked.verdict();
    if admin_target.hostaddr().is_some() {
        assert!(
            matches!(
                verdict,
                TransportVerdict::Verified | TransportVerdict::Local
            ),
            "{verdict:?}"
        );
    } else {
        assert_eq!(
            verdict,
            TransportVerdict::Verified,
            "pg-tls.test is not loopback"
        );
    }

    let admin = PgStore::connect_target(&admin_url, &admin_checked, 1)
        .await
        .unwrap();
    let db = format!("rp_tls_{}", uuid::Uuid::new_v4().simple());
    // `db` is a generated hex identifier, never external input.
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {db}")))
        .execute(admin.pool())
        .await
        .unwrap();

    let db_url = {
        let mut u = url::Url::parse(&admin_url).unwrap();
        u.set_path(&format!("/{db}"));
        u.to_string()
    };
    {
        let checked = PgTarget::parse(&db_url, &env)
            .unwrap()
            .check(false)
            .unwrap();
        let store = PgStore::connect_target(&db_url, &checked, 2).await.unwrap();
        store.migrate().await.unwrap();

        let (ssl, version): (bool, Option<String>) =
            sqlx::query_as("SELECT ssl, version FROM pg_stat_ssl WHERE pid = pg_backend_pid()")
                .fetch_one(store.pool())
                .await
                .unwrap();
        assert!(ssl, "pool connection uses TLS");
        assert_eq!(version.as_deref(), Some("TLSv1.3"));

        // The cluster bus LISTEN connection comes from the same pool.
        let bus = PgBus::start(store.pool().clone()).await.unwrap();
        let mut rx = bus.subscribe();
        bus.publish("over-tls").await.unwrap();
        let got = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&*got, "over-tls");

        let (plain,): (i64,) = sqlx::query_as(
            "SELECT count(*) FROM pg_stat_activity a JOIN pg_stat_ssl s USING (pid) \
             WHERE a.datname = $1 AND NOT s.ssl",
        )
        .bind(&db)
        .fetch_one(store.pool())
        .await
        .unwrap();
        assert_eq!(plain, 0, "no plaintext backend for the test database");
        let listen: Vec<(bool,)> = sqlx::query_as(
            "SELECT s.ssl FROM pg_stat_activity a JOIN pg_stat_ssl s USING (pid) \
             WHERE a.datname = $1 AND a.query ILIKE 'LISTEN%'",
        )
        .bind(&db)
        .fetch_all(store.pool())
        .await
        .unwrap();
        assert!(!listen.is_empty(), "the bus LISTEN backend is visible");
        assert!(listen.iter().all(|(ssl,)| *ssl), "LISTEN uses TLS");
        drop(bus);
        store.pool().close().await;
    }

    // Wrong CA: the chain does not verify.
    let wrong_ca = with_param(&db_url, "sslrootcert", &other_ca);
    let e = connect_err(&wrong_ca).await;
    assert!(e.contains("UnknownIssuer"), "{e}");
    assert!(!e.contains(&secret), "{e}");

    // Name mismatch: same server and CA, but the name is 127.0.0.1, which
    // the certificate does not cover. Loopback, so the policy allows it, and
    // verify-full still checks the name.
    let by_ip = {
        let mut u = url::Url::parse(&db_url).unwrap();
        u.set_host(Some("127.0.0.1")).unwrap();
        let pairs: Vec<(String, String)> = u
            .query_pairs()
            .filter(|(k, _)| k != "hostaddr")
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        u.query_pairs_mut().clear().extend_pairs(pairs);
        u.to_string()
    };
    let t = PgTarget::parse(&by_ip, &env).unwrap();
    assert_eq!(t.check(false).unwrap().verdict(), TransportVerdict::Local);
    let e = connect_err(&by_ip).await;
    assert!(e.contains("not valid for name"), "{e}");
    assert!(!e.contains(&secret), "{e}");

    sqlx::query(sqlx::AssertSqlSafe(format!(
        "DROP DATABASE {db} WITH (FORCE)"
    )))
    .execute(admin.pool())
    .await
    .unwrap();
}
