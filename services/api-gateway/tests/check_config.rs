//! `api-gateway --check-config` applies the database transport policy to the
//! URL in its environment and never prints the password.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use std::process::{Command, Output};

const EXAMPLE: &str = include_str!("../../../deployments/examples/gateway.example.toml");
const SENTINEL: &str = "Sup3rS3cret-sentinel";

fn write_config(name: &str, insecure_transport: bool) -> PathBuf {
    let mut raw = EXAMPLE.to_owned();
    if insecure_transport {
        let at = raw
            .find("backend = \"postgres\"")
            .expect("example uses the postgres backend");
        raw.insert_str(at, "insecure_transport = true\n");
    }
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("check_config_{name}.toml"));
    std::fs::write(&path, raw).unwrap();
    path
}

fn check(config: &PathBuf, database_url: Option<&str>) -> (Output, String, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_api-gateway"));
    cmd.arg("--check-config").arg(config);
    for k in [
        "DATABASE_URL",
        "PGHOST",
        "PGHOSTADDR",
        "PGPORT",
        "PGSSLMODE",
        "PGSSLROOTCERT",
    ] {
        cmd.env_remove(k);
    }
    if let Some(url) = database_url {
        cmd.env("DATABASE_URL", url);
    }
    let out = cmd.output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    (out, stdout, stderr)
}

#[test]
fn check_config_refuses_remote_plaintext_database_without_leaking_password() {
    let cfg = write_config("refuse", false);
    for url in [
        format!("postgres://radio:{SENTINEL}@db.example.org:5432/radio?sslmode=require"),
        format!("postgres://radio:{SENTINEL}@db.example.org:5432/radio"),
        format!("postgres://radio@db.example.org/radio?password={SENTINEL}&sslmode=verify-ca"),
        format!("postgres://radio:{SENTINEL}@db.example.org/radio?sslmod=verify-full"),
    ] {
        let (out, stdout, stderr) = check(&cfg, Some(&url));
        assert_eq!(out.status.code(), Some(2), "{stdout}{stderr}");
        assert!(
            !stdout.contains(SENTINEL) && !stderr.contains(SENTINEL),
            "{stdout}{stderr}"
        );
        assert!(!stdout.contains("configuration OK"), "{stdout}");
    }
    let (_, _, stderr) = check(
        &cfg,
        Some(&format!(
            "postgres://radio:{SENTINEL}@db.example.org:5432/radio?sslmode=require"
        )),
    );
    assert!(
        stderr.contains("verify-full") && stderr.contains("insecure_transport"),
        "{stderr}"
    );
    assert!(stderr.contains("db.example.org:5432"), "{stderr}");
}

#[test]
fn check_config_accepts_verify_full_and_loopback() {
    let cfg = write_config("accept", false);
    for url in [
        format!("postgres://radio:{SENTINEL}@db.example.org:5432/radio?sslmode=verify-full"),
        format!("postgres://radio:{SENTINEL}@127.0.0.1:5432/radio?sslmode=disable"),
        format!("postgres://radio:{SENTINEL}@localhost/radio"),
        "postgres://radio@%2Frun%2Fpostgresql/radio".to_owned(),
    ] {
        let (out, stdout, stderr) = check(&cfg, Some(&url));
        assert!(out.status.success(), "{url}: {stdout}{stderr}");
        assert!(stdout.contains("configuration OK"), "{stdout}");
        assert!(
            !stdout.contains(SENTINEL) && !stderr.contains(SENTINEL),
            "{stdout}{stderr}"
        );
        assert!(!stderr.contains("warning"), "{stderr}");
    }
    // No URL in this environment: passes with a note.
    let (out, stdout, _) = check(&cfg, None);
    assert!(out.status.success());
    assert!(stdout.contains("DATABASE_URL"), "{stdout}");
}

#[test]
fn check_config_warns_with_insecure_transport() {
    let cfg = write_config("warn", true);
    let url = format!("postgres://radio:{SENTINEL}@db.example.org:5432/radio?sslmode=require");
    let (out, stdout, stderr) = check(&cfg, Some(&url));
    assert!(out.status.success(), "{stdout}{stderr}");
    assert!(
        stderr.contains("warning") && stderr.contains("insecure_transport"),
        "{stderr}"
    );
    assert!(
        !stdout.contains(SENTINEL) && !stderr.contains(SENTINEL),
        "{stdout}{stderr}"
    );
}

// ---- QA additions (run 20261126-125150-pg-tls) ----

/// The example configuration with its `[database]` table replaced.
fn write_db_config(name: &str, database: &str) -> PathBuf {
    let start = EXAMPLE
        .find("[database]\n")
        .expect("example has [database]");
    let end = EXAMPLE[start..]
        .find("\n[auth]")
        .map(|i| start + i)
        .expect("[auth] follows [database]");
    let raw = format!(
        "{}[database]\n{database}\n{}",
        &EXAMPLE[..start],
        &EXAMPLE[end..]
    );
    let path =
        PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("check_config_qa_{name}.toml"));
    std::fs::write(&path, raw).unwrap();
    path
}

const PG_VARS: [&str; 7] = [
    "DATABASE_URL",
    "PGHOST",
    "PGHOSTADDR",
    "PGPORT",
    "PGSSLMODE",
    "PGSSLROOTCERT",
    "RUST_LOG",
];

/// Runs the binary with exactly `vars` set (out of `PG_VARS`).
fn run_gateway(args: &[&str], config: &PathBuf, vars: &[(&str, &str)]) -> (Option<i32>, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_api-gateway"));
    cmd.args(args).arg(config);
    for k in PG_VARS {
        cmd.env_remove(k);
    }
    cmd.envs(vars.iter().copied());
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    let mut child = cmd.spawn().unwrap();
    // The gateway must exit on its own; never leave it running.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("api-gateway did not exit within 30 s");
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let out = child.wait_with_output().unwrap();
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.code(), all)
}

fn assert_no_secret(all: &str) {
    assert!(!all.contains(SENTINEL), "password leaked:\n{all}");
    let encoded = SENTINEL.replace('-', "%2D");
    assert!(!all.contains(&encoded), "password leaked:\n{all}");
}

#[test]
fn qa_check_config_uses_file_url_and_database_url_overrides_it() {
    // Unverified remote URL in the file only: refused.
    let file_plain = write_db_config(
        "file_plain",
        &format!(
            "backend = \"postgres\"\nurl = \"postgres://radio:{SENTINEL}@db.example.org/radio?sslmode=disable\""
        ),
    );
    let (code, all) = run_gateway(&["--check-config"], &file_plain, &[]);
    assert_eq!(code, Some(2), "{all}");
    assert!(
        all.contains("db.example.org") && all.contains("sslmode=disable"),
        "{all}"
    );
    assert_no_secret(&all);
    // DATABASE_URL replaces it: a loopback URL passes.
    let (code, all) = run_gateway(
        &["--check-config"],
        &file_plain,
        &[("DATABASE_URL", "postgres://radio@127.0.0.1/radio")],
    );
    assert_eq!(code, Some(0), "{all}");
    assert_no_secret(&all);
    // A verified URL in the file does not excuse an unverified DATABASE_URL.
    let file_ok = write_db_config(
        "file_ok",
        &format!(
            "backend = \"postgres\"\nurl = \"postgres://radio:{SENTINEL}@db.example.org/radio?sslmode=verify-full\""
        ),
    );
    let (code, all) = run_gateway(&["--check-config"], &file_ok, &[]);
    assert_eq!(code, Some(0), "{all}");
    assert!(
        all.contains("db.example.org:5432 sslmode=verify-full: OK"),
        "{all}"
    );
    assert_no_secret(&all);
    let (code, all) = run_gateway(
        &["--check-config"],
        &file_ok,
        &[(
            "DATABASE_URL",
            &format!("postgres://radio:{SENTINEL}@db2.example.org/radio?sslmode=require"),
        )],
    );
    assert_eq!(code, Some(2), "{all}");
    assert!(all.contains("db2.example.org"), "{all}");
    assert_no_secret(&all);
}

#[test]
fn qa_check_config_applies_pg_environment() {
    let cfg = write_db_config("pgenv", "backend = \"postgres\"");
    let remote = format!("postgres://radio:{SENTINEL}@db.example.org/radio");
    // PGSSLMODE=verify-full makes a URL without sslmode verified.
    let (code, all) = run_gateway(
        &["--check-config"],
        &cfg,
        &[("DATABASE_URL", &remote), ("PGSSLMODE", "verify-full")],
    );
    assert_eq!(code, Some(0), "{all}");
    assert_no_secret(&all);
    // PGSSLMODE=require does not.
    let (code, all) = run_gateway(
        &["--check-config"],
        &cfg,
        &[("DATABASE_URL", &remote), ("PGSSLMODE", "require")],
    );
    assert_eq!(code, Some(2), "{all}");
    assert_no_secret(&all);
    // PGHOSTADDR sends a localhost URL off the machine.
    let (code, all) = run_gateway(
        &["--check-config"],
        &cfg,
        &[
            (
                "DATABASE_URL",
                &format!("postgres://radio:{SENTINEL}@localhost/radio"),
            ),
            ("PGHOSTADDR", "10.0.0.5"),
        ],
    );
    assert_eq!(code, Some(2), "{all}");
    assert!(all.contains("hostaddr 10.0.0.5"), "{all}");
    assert_no_secret(&all);
    // PGSSLROOTCERT=system is refused.
    let (code, all) = run_gateway(
        &["--check-config"],
        &cfg,
        &[
            ("DATABASE_URL", &format!("{remote}?sslmode=verify-full")),
            ("PGSSLROOTCERT", "system"),
        ],
    );
    assert_eq!(code, Some(2), "{all}");
    assert_no_secret(&all);
}

#[test]
fn qa_check_config_refuses_malformed_urls_without_leaking() {
    let cfg = write_db_config("malformed", "backend = \"postgres\"");
    for url in [
        format!("postgres://radio:{SENTINEL}@[::1/radio"),
        format!("mysql://radio:{SENTINEL}@localhost/radio"),
        format!(
            "postgres://radio:{SENTINEL}@db.example.org/radio?sslmode=verify-full&sslmode=disable"
        ),
        format!(
            "postgres://radio:{SENTINEL}@db.example.org/radio?sslmode=verify-ca&sslrootcert=/ca.crt"
        ),
        format!(
            "postgres://radio:{SENTINEL}@db.example.org/radio?sslmode=verify-full&sslrootcert=system"
        ),
        format!("postgres://radio@localhost/radio?pwd={SENTINEL}"),
        format!("postgres://radio@localhost/radio?sslmode={SENTINEL}"),
        format!("postgres://radio:{SENTINEL}@localhost/{}", "a".repeat(9000)),
    ] {
        let (code, all) = run_gateway(&["--check-config"], &cfg, &[("DATABASE_URL", &url)]);
        assert_eq!(code, Some(2), "{all}");
        assert!(!all.contains("configuration OK"), "{all}");
        assert_no_secret(&all);
    }
}

#[test]
fn qa_check_config_memory_backend_ignores_database_url() {
    let cfg = write_db_config("memory", "backend = \"memory\"");
    let (code, all) = run_gateway(
        &["--check-config"],
        &cfg,
        &[(
            "DATABASE_URL",
            &format!("postgres://radio:{SENTINEL}@db.example.org/radio?sslmode=disable"),
        )],
    );
    assert_eq!(code, Some(0), "{all}");
    assert!(all.contains("configuration OK"), "{all}");
    assert_no_secret(&all);
}

#[test]
fn qa_check_config_opt_out_does_not_warn_for_safe_targets() {
    let cfg = write_db_config(
        "optout_safe",
        "backend = \"postgres\"\ninsecure_transport = true",
    );
    for url in [
        format!("postgres://radio:{SENTINEL}@db.example.org/radio?sslmode=verify-full"),
        format!("postgres://radio:{SENTINEL}@127.0.0.1/radio?sslmode=disable"),
    ] {
        let (code, all) = run_gateway(&["--check-config"], &cfg, &[("DATABASE_URL", &url)]);
        assert_eq!(code, Some(0), "{all}");
        assert!(!all.contains("warning"), "{all}");
        assert_no_secret(&all);
    }
    // A misspelt opt-out is ignored, so the gateway fails closed.
    let typo = write_db_config(
        "optout_typo",
        "backend = \"postgres\"\ninsecure_transprot = true",
    );
    let (code, all) = run_gateway(
        &["--check-config"],
        &typo,
        &[(
            "DATABASE_URL",
            &format!("postgres://radio:{SENTINEL}@db.example.org/radio?sslmode=require"),
        )],
    );
    assert_eq!(code, Some(2), "{all}");
    assert_no_secret(&all);
    // The opt-out must be a boolean.
    let wrong = write_db_config(
        "optout_string",
        "backend = \"postgres\"\ninsecure_transport = \"yes\"",
    );
    let (code, all) = run_gateway(&["--check-config"], &wrong, &[]);
    assert_eq!(code, Some(2), "{all}");
}

#[test]
fn qa_start_refuses_remote_plaintext_before_connecting_and_logs_no_secret() {
    let cfg = write_db_config("start_refuse", "backend = \"postgres\"");
    // Trace logging: every crate logs as much as it can.
    let (code, all) = run_gateway(
        &[],
        &cfg,
        &[
            (
                "DATABASE_URL",
                &format!("postgres://radio:{SENTINEL}@db.example.org:5432/radio?sslmode=require"),
            ),
            ("RUST_LOG", "trace"),
        ],
    );
    assert_eq!(code, Some(1), "{all}");
    assert!(all.contains("requires sslmode=verify-full"), "{all}");
    assert_no_secret(&all);
}

#[test]
fn qa_start_logs_transport_verdict_without_secret() {
    // Remote by policy (0.0.0.0 is not loopback), but the TCP connection
    // goes to this machine's closed port 1 and fails at once. The opt-out
    // must produce the start-up warning.
    let cfg = write_db_config(
        "start_warn",
        "backend = \"postgres\"\ninsecure_transport = true",
    );
    let (code, all) = run_gateway(
        &[],
        &cfg,
        &[
            (
                "DATABASE_URL",
                &format!(
                    "postgres://radio:{SENTINEL}@db.example.org:1/radio?hostaddr=0.0.0.0&sslmode=require"
                ),
            ),
            ("RUST_LOG", "trace"),
        ],
    );
    assert_eq!(code, Some(1), "{all}");
    assert!(all.contains("\"level\":\"WARN\""), "{all}");
    assert!(all.contains("insecure_transport = true"), "{all}");
    assert!(
        all.contains("db.example.org") && all.contains("require"),
        "{all}"
    );
    assert_no_secret(&all);

    // Local verdict: an info line, then a connection error on port 1.
    let cfg = write_db_config("start_local", "backend = \"postgres\"");
    let (code, all) = run_gateway(
        &[],
        &cfg,
        &[
            (
                "DATABASE_URL",
                &format!("postgres://radio:{SENTINEL}@127.0.0.1:1/radio?sslmode=disable"),
            ),
            ("RUST_LOG", "trace"),
        ],
    );
    assert_eq!(code, Some(1), "{all}");
    assert!(all.contains("database is on this host"), "{all}");
    assert_no_secret(&all);
}

#[test]
fn toml_syntax_error_never_echoes_the_source_line() {
    // A password with an unescaped quote, or an unterminated string, on the
    // `url =` line. The TOML parser's default message quotes that line.
    for (name, line) in [
        (
            "toml_quote",
            format!("url = \"postgres://radio:{SENTINEL}\"x@db.example.org/radio\""),
        ),
        (
            "toml_unterminated",
            format!("url = \"postgres://radio:{SENTINEL}@db.example.org/radio"),
        ),
    ] {
        let cfg = write_db_config(name, &format!("backend = \"postgres\"\n{line}"));
        let raw = std::fs::read_to_string(&cfg).unwrap();
        let url_line = raw.lines().position(|l| l.starts_with("url = ")).unwrap() + 1;
        for args in [&["--check-config"][..], &[][..]] {
            let (code, all) = run_gateway(args, &cfg, &[("RUST_LOG", "trace")]);
            assert_eq!(code, Some(2), "{args:?}: {all}");
            assert!(
                all.contains(&format!("line {url_line}, column ")),
                "{args:?}: {all}"
            );
            assert!(!all.contains("configuration OK"), "{all}");
            assert!(!all.contains("db.example.org"), "source line echoed: {all}");
            assert_no_secret(&all);
        }
    }
}
