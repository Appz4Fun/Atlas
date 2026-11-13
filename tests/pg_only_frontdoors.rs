//! The normal binary must refuse retired storage and absent PostgreSQL destinations
//! before configuration migration, local storage creation, or provider startup.

use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

fn run(home: &std::path::Path, args: &[&str]) -> Output {
    let provider = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    provider.set_nonblocking(true).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_atlas"))
        .args(args)
        .env_clear()
        .env("ATLAS_HOME", home)
        .env("ATLAS_NO_KEYRING", "1")
        .env("ATLAS_NNTP_HOST", "127.0.0.1")
        .env("ATLAS_NNTP_PORT", provider.local_addr().unwrap().port().to_string())
        .env("ATLAS_NNTP_USER", "fixture")
        .env("ATLAS_NNTP_PASS", "fixture")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        matches!(provider.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
        "frontdoor refusal must precede provider connections"
    );
    output
}

#[test]
fn retired_storage_commands_refuse_before_any_configuration_or_storage() {
    for command in [
        "--convert",
        "--compact",
        "--reset-shards",
        "--sealed-log-move",
        "--shared-catalog-fill",
        "--shared-catalog-check",
        "--search-indexes",
        "--thaw",
        "--frozen-stats",
        "--export-keep",
        "--move-ext",
        "--restore-keep",
        "--import-main",
        "--import-history",
    ] {
        let home = tempfile::tempdir().unwrap();
        let output = run(home.path(), &["--dump-ext", command, "--apply"]);
        assert_eq!(output.status.code(), Some(1), "{command}");
        assert!(String::from_utf8_lossy(&output.stderr).contains("is retired"), "{command}");
        assert!(std::fs::read_dir(home.path()).unwrap().next().is_none(), "{command} created storage");
    }
}

#[test]
fn runtime_and_backup_require_explicit_usable_postgres_storage() {
    for arguments in [vec![], vec!["--bg-indexer"], vec!["--dump-ext"], vec!["--restore-ext", "missing"]] {
        for config in [None, Some("{}"), Some(r#"{"storage":{"backend":"sqlite","url":"fixture-secret-sentinel"}}"#)] {
            let home = tempfile::tempdir().unwrap();
            let path = home.path().join("config.json");
            if let Some(source) = config {
                std::fs::write(&path, source).unwrap();
            }
            let output = run(home.path(), &arguments);
            assert_eq!(output.status.code(), Some(1));
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(stderr.contains("PostgreSQL storage configuration"));
            assert!(!stderr.contains("fixture-secret-sentinel"));
            let names =
                std::fs::read_dir(home.path()).unwrap().map(|entry| entry.unwrap().file_name()).collect::<Vec<_>>();
            assert_eq!(
                names.len(),
                usize::from(config.is_some()),
                "startup created a catalog, status or migration file"
            );
            if let Some(source) = config {
                assert_eq!(std::fs::read_to_string(&path).unwrap(), source);
            }
        }
    }
}

#[test]
fn selected_toml_failure_cannot_fall_back_to_valid_postgres_json() {
    for kind in ["malformed", "directory", "dangling"] {
        let home = tempfile::tempdir().unwrap();
        let json = r#"{"storage":{"backend":"postgres","url":"host=127.0.0.1 port=9 dbname=fixture"}}"#;
        std::fs::write(home.path().join("config.json"), json).unwrap();
        let selected = home.path().join("config.toml");
        match kind {
            "malformed" => std::fs::write(&selected, "[storage\nfixture-secret-sentinel").unwrap(),
            "directory" => std::fs::create_dir(&selected).unwrap(),
            "dangling" => {
                #[cfg(unix)]
                std::os::unix::fs::symlink(home.path().join("missing.toml"), &selected).unwrap();
                #[cfg(not(unix))]
                continue;
            }
            _ => unreachable!(),
        }
        let output = run(home.path(), &["--bg-indexer"]);
        assert_eq!(output.status.code(), Some(1));
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("PostgreSQL storage configuration is unavailable"));
        assert!(!stderr.contains("fixture-secret-sentinel"));
        assert_eq!(std::fs::read_to_string(home.path().join("config.json")).unwrap(), json);
        assert_eq!(std::fs::read_dir(home.path()).unwrap().count(), 2);
    }
}

#[test]
fn required_maintenance_refusal_is_distinct_from_storage_retirement() {
    let home = tempfile::tempdir().unwrap();
    for command in ["--purge", "--reprocess", "--predb-import", "--replay-raw-predb"] {
        let output = run(home.path(), &[command, "--apply"]);
        assert_eq!(output.status.code(), Some(1));
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("requires a native PostgreSQL maintenance implementation")
        );
    }
    assert!(std::fs::read_dir(home.path()).unwrap().next().is_none());
}

#[test]
fn replay_without_a_native_destination_cannot_select_the_legacy_writer() {
    let home = tempfile::tempdir().unwrap();
    let output = run(home.path(), &["--save-replay", "--store", "missing", "--db", "scratch"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("requires --pg-database and --pg-schema"));
    assert!(std::fs::read_dir(home.path()).unwrap().next().is_none());
}

#[test]
fn product_help_describes_native_and_pure_tools_without_retired_commands() {
    let home = tempfile::tempdir().unwrap();
    let output = run(home.path(), &["--help"]);
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    for retained in ["--dump-ext", "--restore-ext", "--save-replay", "--pg-schema", "--header-store", "--overview"] {
        assert!(help.contains(retained));
    }
    for retired in ["--convert", "--reset-shards", "--move-ext", "--import-history", "--sealed-log-move"] {
        assert!(!help.contains(retired));
    }
    assert!(std::fs::read_dir(home.path()).unwrap().next().is_none());
}
