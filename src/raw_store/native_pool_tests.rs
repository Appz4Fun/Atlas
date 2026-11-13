//! The original 48-by-20 cache oracle on one genuine native owner.
use super::{ProbeRecord, get_probe_checked, put_probe_checked};
use std::{
    collections::BTreeMap,
    io::Read,
    path::Path,
    process::{Command, Stdio},
    sync::{Arc, Barrier},
    time::{Duration, Instant},
};

const SELECTOR: &str = "raw_store::native_pool_tests::many_threads_write_and_read_the_raw_store_at_once";
const CHILD: &str = "ATLAS_TEST_NATIVE_RAW_POOL_CHILD";
const GROUP: &str = "alt.binaries.test";

fn no_sqlite_files(root: &Path) {
    let mut pending = vec![(root.to_path_buf(), 0usize)];
    let mut entries = 0;
    while let Some((dir, depth)) = pending.pop() {
        assert!(depth <= 8, "fixture directory depth exceeded");
        for entry in std::fs::read_dir(dir).unwrap() {
            entries += 1;
            assert!(entries <= 256, "fixture directory inventory exceeded");
            let entry = entry.unwrap();
            let kind = entry.file_type().unwrap();
            assert!(!kind.is_symlink(), "fixture must not escape its private home");
            let name = entry.file_name();
            let name = name.to_string_lossy();
            assert!(
                !name.ends_with(".db")
                    && !name.ends_with(".db-wal")
                    && !name.ends_with(".db-shm")
                    && !name.contains(".sqlite"),
                "native cache created a SQLite file: {name}"
            );
            if kind.is_dir() {
                pending.push((entry.path(), depth + 1));
            }
        }
    }
}

fn write_config(home: &Path, schema: &crate::pg::testing::Schema) {
    let mut config = toml_edit::DocumentMut::new();
    config["storage"]["backend"] = toml_edit::value("postgres");
    config["storage"]["url"] = toml_edit::value(schema.url.as_str());
    config["storage"]["schema"] = toml_edit::value(schema.name.as_str());
    config["storage"]["max_connections"] = toml_edit::value(1);
    config["storage"]["allow_plaintext"] = toml_edit::value(true);
    config["postprocess"]["enabled"] = toml_edit::value(false);
    config["predb"]["enabled"] = toml_edit::value(false);
    config["spots"]["enabled"] = toml_edit::value(false);
    config["media"]["tvmaze"]["enabled"] = toml_edit::value(false);
    config["media"]["allocine"]["enabled"] = toml_edit::value(false);
    config["media"]["details_per_minute"] = toml_edit::value(0);
    config["inferred_parts"] = toml_edit::value(false);
    std::fs::write(home.join("config.toml"), config.to_string()).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(home.join("config.toml"), std::fs::Permissions::from_mode(0o600)).unwrap();
    }
}

fn configured_child() {
    let storage = crate::config::checked_storage_config().expect("selected private PostgreSQL fixture config");
    let mut config = crate::config::Config::default();
    config.postprocess.enabled = false;
    config.predb.enabled = false;
    config.spots.enabled = false;
    config.inferred_parts = false;
    config.media.tvmaze.enabled = false;
    config.media.allocine.enabled = false;
    config.media.details_per_minute = 0;
    let home = crate::paths::app_dir();
    let headers =
        crate::config::HeaderStoreConfig { enabled: true, dir: home.join("headers").to_str().unwrap().into() };
    let runtime = crate::pg::indexer_runtime::Runtime::open(&config, storage, headers, &[], &mut None)
        .expect("actual original native owner admission");
    assert!(get_probe_checked("Rel.0.0", GROUP, b"cid", 10, true, 1).is_err(), "unbound scope is not a cache miss");
    let ready = Arc::new(Barrier::new(48));
    let threads: Vec<_> = (0..48)
        .map(|t| {
            let owner = runtime.owner.clone();
            let ready = ready.clone();
            std::thread::spawn(move || {
                // TLS guards cannot be transferred between threads. Each caller
                // must bind the same actual owner under the selected namespace.
                let scope = crate::pg::cache_actor::NativeCacheScope::bind(&owner).expect("genuine per-thread scope");
                ready.wait();
                for n in 0..20 {
                    let name = format!("Rel.{t}.{n}");
                    let info = format!("{{\"file\":\"{name}\"}}");
                    put_probe_checked(&ProbeRecord {
                        name: &name,
                        group_name: GROUP,
                        content_id: b"cid",
                        crc32: Some(i64::from(t)),
                        agent: None,
                        info: &info,
                        moviehash: None,
                        parts: 10,
                        complete: true,
                        config_stamp: 1,
                        nfo_password: None,
                        cover: None,
                    })
                    .expect("actual native probe write");
                    let saved =
                        get_probe_checked(&name, GROUP, b"cid", 10, true, 1).expect("actual checked probe read");
                    assert_eq!(saved.map(|s| s.info), Some(info), "{name}");
                }
                drop(scope);
                assert!(get_probe_checked("Rel.0.0", GROUP, b"cid", 10, true, 1).is_err(), "ended scope is not a miss");
            })
        })
        .collect();
    let mut failed = false;
    for thread in threads {
        failed |= thread.join().is_err();
    }
    assert!(!failed, "a native cache worker failed; every worker was joined");
    assert_eq!(runtime.owner.failure(), None, "successful concurrent cache work must not poison its owner");
    runtime
        .owner
        .with(|client| {
            assert_eq!(client.query_one("select count(*) from ext_raw_probes", &[])?.get::<_, i64>(0), 960);
            Ok(())
        })
        .unwrap();
    let scope = crate::pg::cache_actor::NativeCacheScope::bind(&runtime.owner).unwrap();
    runtime.owner.close();
    let ended = get_probe_checked("Rel.0.0", GROUP, b"cid", 10, true, 1)
        .expect_err("closed owner must not become a cache miss");
    assert!(crate::pg::outage::is_attempt_ended(&ended), "original owner cause must survive the cache API");
    assert_eq!(runtime.owner.failure(), None, "intentional owner close is not a storage failure");
    drop(scope);
    no_sqlite_files(&home);
}

fn verify_persisted_rows(client: &mut postgres::Client) {
    assert_eq!(client.query_one("select count(*) from ext_raw_probes", &[]).unwrap().get::<_, i64>(0), 960);
    // Read-only verification after the actual original owner and all worker
    // guards ended. Bound the transfer even if the writer stores wrong facts.
    let rows = client
        .query(
            "select case when octet_length(name)<=64 then name end,
        case when octet_length(group_name)<=64 then group_name end,
        case when octet_length(content_id)<=16 then content_id end,crc32,
        agent is null,case when octet_length(info)<=128 then info end,moviehash is null,
        parts,complete,config_stamp from ext_raw_probes order by name limit 961",
            &[],
        )
        .unwrap();
    assert_eq!(rows.len(), 960);
    let mut expected = BTreeMap::new();
    for t in 0..48 {
        for n in 0..20 {
            let name = format!("Rel.{t}.{n}");
            expected.insert(name.clone(), (i64::from(t), format!("{{\"file\":\"{name}\"}}")));
        }
    }
    for row in rows {
        let name: String = row.get(0);
        let (crc, info) = expected.remove(&name).expect("unique actual fixture natural name");
        assert_eq!(row.get::<_, String>(1), GROUP);
        assert_eq!(row.get::<_, Vec<u8>>(2).as_slice(), b"cid");
        assert_eq!(row.get::<_, Option<i64>>(3), Some(crc));
        assert!(row.get::<_, bool>(4));
        assert_eq!(row.get::<_, String>(5), info);
        assert!(row.get::<_, bool>(6));
        assert_eq!(row.get::<_, Option<i64>>(7), Some(10));
        assert_eq!(row.get::<_, Option<i64>>(8), Some(1));
        assert_eq!(row.get::<_, Option<i64>>(9), Some(1));
    }
    assert!(expected.is_empty());
}

fn bounded_log(mut stream: impl Read) -> Vec<u8> {
    let mut bytes = Vec::new();
    stream.by_ref().take(65_537).read_to_end(&mut bytes).unwrap();
    assert!(bytes.len() <= 65_536, "native cache child output exceeded admission");
    bytes
}

#[test]
#[ignore = "requires explicit isolated PostgreSQL for genuine 48-thread native cache scopes"]
fn many_threads_write_and_read_the_raw_store_at_once() {
    if std::env::var(CHILD).ok().as_deref() == Some("owned-pool-v1") {
        configured_child();
        return;
    }
    let schema = crate::pg::testing::Schema::new().expect("explicit isolated PostgreSQL required");
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir(home.path().join("tmp")).unwrap();
    write_config(home.path(), &schema);
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", SELECTOR, "--nocapture", "--test-threads=1"])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", home.path())
        .env("ATLAS_HOME", home.path())
        .env("TMPDIR", home.path().join("tmp"))
        .env("ATLAS_SAB_DIR", home.path().join("no-sab"))
        .env("ATLAS_NO_KEYRING", "1")
        .env(CHILD, "owned-pool-v1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let out = child.stdout.take().unwrap();
    let err = child.stderr.take().unwrap();
    let out = std::thread::spawn(move || bounded_log(out));
    let err = std::thread::spawn(move || bounded_log(err));
    let until = Instant::now() + Duration::from_secs(180);
    let mut timed_out = false;
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= until {
            timed_out = true;
            child.kill().unwrap();
            break child.wait().unwrap();
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let output = [out.join().unwrap(), err.join().unwrap()].concat();
    assert!(!timed_out, "owned native cache child exceeded its finite deadline after joined termination");
    assert!(
        status.success(),
        "actual native cache child failed: {}",
        String::from_utf8_lossy(&output).replace(&schema.url, "[private PG destination]")
    );
    verify_persisted_rows(&mut schema.pool.get().unwrap());
    no_sqlite_files(home.path());
}
