//! The real conversion command is isolated from live-home/config discovery.
#[allow(dead_code)]
mod history_catalog_support;
use history_catalog_support::{destination, manifest};
use std::path::Path;
use std::process::{Command, Output};

fn run(home: &Path, manifest: &Path, extra: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_atlas"))
        .args(["--import-history", "--manifest"])
        .arg(manifest)
        .args(["--verify-bytes", "1048576"])
        .args(extra)
        .env("ATLAS_HOME", home)
        .env("ATLAS_NO_KEYRING", "1")
        .env("ATLAS_SAB_DIR", home.join("sab"))
        .output()
        .unwrap()
}
#[test]
fn dry_verification_does_not_discover_or_migrate_live_home_configuration() {
    let source = tempfile::tempdir().unwrap();
    let path = manifest(source.path());
    let home = tempfile::tempdir().unwrap();
    let config = b"this deliberately is not valid JSON";
    std::fs::write(home.path().join("config.json"), config).unwrap();
    let out = run(home.path(), &path, &[]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let result: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(result["state"], "verified_source");
    assert_eq!(std::fs::read(home.path().join("config.json")).unwrap(), config);
    assert_eq!(std::fs::read_dir(home.path()).unwrap().count(), 1);
    assert!(!source.path().join("source-1.db-wal").exists());
}

#[test]
fn explicit_apply_resumes_pages_and_stops_at_identity_ready() {
    let source = tempfile::tempdir().unwrap();
    let manifest = manifest(source.path());
    let home = tempfile::tempdir().unwrap();
    let Some((mut target, expected)) = destination(home.path()) else { return };
    let spool = home.path().join("spool");
    std::fs::create_dir(&spool).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&spool, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let common = [
        "--apply",
        "--destination",
        expected.to_str().unwrap(),
        "--url-env",
        "ATLAS_PG_TEST_URL",
        "--spool",
        spool.to_str().unwrap(),
        "--spool-quota",
        "4194304",
        "--allow-plaintext",
    ];
    let mut first = common.to_vec();
    first.extend(["--max-pages", "1"]);
    let out = run(home.path(), &manifest, &first);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let result: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(result["state"], "loading");
    assert_eq!(result["sequence"], 1);
    let out = run(home.path(), &manifest, &common);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let result: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(result["state"], "identity_ready");
    assert_eq!(result["activation"], false);
    target.client.batch_execute(&format!("set search_path to \"{}\"", target.name)).unwrap();
    assert_eq!(target.client.query_one("select count(*) from releases", &[]).unwrap().get::<_, i64>(0), 0);
    assert_eq!(target.client.query_one("select public_id from history_catalog", &[]).unwrap().get::<_, i64>(0), 3);
    assert_eq!(
        target
            .client
            .query_one("select nextval(pg_get_serial_sequence('releases','id'))", &[])
            .unwrap()
            .get::<_, i64>(0),
        900
    );
    assert!(atlas::pg::release_queue::claim(&mut target.client, 0, 1, 1).is_err());
    let out = run(home.path(), &manifest, &common);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(
        target.client.query_one("select count(*) from history_collection_aliases", &[]).unwrap().get::<_, i64>(0),
        2
    );
}
#[test]
fn impossible_page_budget_is_rejected_before_target_schema_creation() {
    let source = tempfile::tempdir().unwrap();
    let manifest = manifest(source.path());
    let home = tempfile::tempdir().unwrap();
    let Some((mut target, expected)) = destination(home.path()) else { return };
    let spool = home.path().join("spool");
    std::fs::create_dir(&spool).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&spool, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let out = run(
        home.path(),
        &manifest,
        &[
            "--apply",
            "--destination",
            expected.to_str().unwrap(),
            "--url-env",
            "ATLAS_PG_TEST_URL",
            "--spool",
            spool.to_str().unwrap(),
            "--spool-quota",
            "4194304",
            "--source-bytes",
            "1",
            "--allow-plaintext",
        ],
    );
    assert!(!out.status.success());
    let exists: bool = target
        .client
        .query_one("select exists(select 1 from pg_namespace where nspname=$1)", &[&target.name])
        .unwrap()
        .get(0);
    assert!(!exists, "a rejected page budget must not register a partial import");
}
