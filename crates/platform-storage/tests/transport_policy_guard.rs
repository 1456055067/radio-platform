//! `PgStore::connect_target` connects only where the transport policy looked,
//! and no production code reaches PostgreSQL around it. No database needed.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use platform_storage::{CheckedTarget, PgStore, PgTarget, StoreError, TransportVerdict};

const SENTINEL: &str = "Sup3rS3cret-sentinel";

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok()
}

async fn connect_target_err(url: &str, target: &CheckedTarget) -> StoreError {
    // TEST-NET-1 (192.0.2.0/24) is never routed, so a real connection
    // attempt would hang until the pool's 5 s acquire timeout. Finishing
    // well inside 2 s proves the refusal happens before any network I/O.
    match tokio::time::timeout(
        Duration::from_secs(2),
        PgStore::connect_target(url, target, 1),
    )
    .await
    {
        Err(_) => panic!("connect_target attempted a connection for a mismatched target"),
        Ok(Ok(_)) => panic!("connect_target unexpectedly connected"),
        Ok(Err(e)) => e,
    }
}

#[tokio::test]
async fn connect_target_refuses_a_target_from_another_url_before_any_io() {
    let checked = PgTarget::parse("postgres://radio@localhost/radio?sslmode=disable", &env)
        .unwrap()
        .check(false)
        .unwrap();
    for url in [
        // Different host (the classic bypass: check one URL, connect to another).
        format!("postgres://radio:{SENTINEL}@192.0.2.1:5432/radio?sslmode=disable"),
        // Same host, but hostaddr sends the TCP connection elsewhere.
        format!("postgres://radio:{SENTINEL}@localhost/radio?sslmode=disable&hostaddr=192.0.2.1"),
        // Same host, different sslmode.
        format!("postgres://radio:{SENTINEL}@localhost/radio?sslmode=require"),
        // An unknown key never reaches sqlx (which would log its value).
        format!("postgres://radio@localhost/radio?sslmode=disable&passwd={SENTINEL}"),
    ] {
        let e = connect_target_err(&url, &checked).await;
        assert!(matches!(e, StoreError::Invalid(_)), "{e}");
        assert!(!e.to_string().contains(SENTINEL), "{e}");
        assert!(!format!("{e:?}").contains(SENTINEL), "{e:?}");
    }
}

#[test]
fn a_remote_plaintext_target_is_only_connectable_through_check() {
    // `connect_target` takes a `CheckedTarget`, which only `PgTarget::check`
    // creates (the compile_fail doctests on `CheckedTarget` show that a bare
    // `PgTarget` and a struct literal do not compile). So for a remote
    // `sslmode=disable` target, the policy decides whether one exists.
    let url = format!("postgres://radio:{SENTINEL}@192.0.2.1:5432/radio?sslmode=disable");
    let target = PgTarget::parse(&url, &env).unwrap();
    let e = target.check(false).unwrap_err();
    assert!(e.to_string().contains("verify-full"), "{e}");
    assert!(!e.to_string().contains(SENTINEL), "{e}");

    // With the opt-out, the policy hands out a checked target, and its
    // verdict is the one the caller must warn about.
    let checked = target.check(true).unwrap();
    assert_eq!(checked.verdict(), TransportVerdict::InsecureAllowed);
    assert_eq!(checked.target(), &target);
    assert_eq!(checked.to_string(), "192.0.2.1:5432 sslmode=disable");
    assert!(!format!("{checked:?}").contains(SENTINEL));
}

/// Production sources: every `src/` directory of the workspace crates,
/// services and test-support binaries (the devstack and simulator are not
/// deployed, but they are built from the same tree).
fn production_sources() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut dirs = Vec::new();
    for group in ["crates", "services", "tests"] {
        for entry in std::fs::read_dir(root.join(group)).unwrap() {
            let src = entry.unwrap().path().join("src");
            if src.is_dir() {
                dirs.push(src);
            }
        }
    }
    let mut files = Vec::new();
    while let Some(d) = dirs.pop() {
        for entry in std::fs::read_dir(&d).unwrap() {
            let p = entry.unwrap().path();
            if p.is_dir() {
                dirs.push(p);
            } else if p.extension().is_some_and(|e| e == "rs") {
                files.push(p);
            }
        }
    }
    assert!(files.len() > 20, "found the workspace sources");
    files
}

/// Strips `#[cfg(test)] mod tests { ... }` (always the file's tail here).
fn non_test_part(src: &str) -> &str {
    src.find("#[cfg(test)]").map_or(src, |i| &src[..i])
}

#[test]
fn no_production_code_connects_to_postgres_around_the_policy() {
    // Ways to open a PostgreSQL connection or pool from a URL or options.
    let needles = [
        "PgStore::connect(",
        "PgPoolOptions",
        "PgPool::connect",
        "PgConnectOptions",
        "PgConnection::connect",
        "PgListener::connect(",
        "PgStore::from_pool(",
    ];
    // The storage crate's own definitions, which the policy wraps.
    let allowed = [
        "crates/platform-storage/src/postgres.rs",
        "crates/platform-storage/src/pg_transport.rs",
    ];
    let mut hits = Vec::new();
    for file in production_sources() {
        let path = file.to_string_lossy().replace('\\', "/");
        if allowed.iter().any(|a| path.ends_with(a)) {
            continue;
        }
        let src = std::fs::read_to_string(&file).unwrap();
        for n in needles {
            if non_test_part(&src).contains(n) {
                hits.push(format!("{path}: {n}"));
            }
        }
    }
    assert!(
        hits.is_empty(),
        "production code bypasses PgStore::connect_target:\n{}",
        hits.join("\n")
    );

    // In the storage crate, `connect_options` is the only source of
    // `PgConnectOptions`, and `PgStore::connect` is the only other pool.
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let pg = std::fs::read_to_string(root.join("src/postgres.rs")).unwrap();
    assert_eq!(
        pg.matches("PgPoolOptions::new()").count(),
        2,
        "connect and connect_target"
    );
    assert!(pg.contains("pg_transport::connect_options(url, checked.target())"));
    assert!(
        pg.contains("checked: &CheckedTarget,"),
        "connect_target requires a checked target"
    );

    // `PgTarget::check` is the only constructor of `CheckedTarget`.
    let pt = std::fs::read_to_string(root.join("src/pg_transport.rs")).unwrap();
    let code: String = non_test_part(&pt)
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(
        code.matches("CheckedTarget {\n").count(),
        4,
        "struct, two impls and the literal in check: no other CheckedTarget literal"
    );
    assert_eq!(
        code.matches("Ok(CheckedTarget {").count(),
        1,
        "only check builds one"
    );
    assert!(code.contains("pub struct CheckedTarget {\n    target: PgTarget,\n    verdict"));

    // The gateway uses the policy-checked constructor.
    let main =
        std::fs::read_to_string(root.join("../../services/api-gateway/src/main.rs")).unwrap();
    assert!(main.contains("PgStore::connect_target("));
    assert!(main.contains("check_database_transport("));
}
