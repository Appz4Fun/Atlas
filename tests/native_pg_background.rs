//! Native storage through the actual background-indexer entry point.
mod common;

use std::fs::File;
use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{GROUP, Server, post, spawn_server};

struct Runner(Child);

impl Runner {
    fn stop(&mut self) -> std::process::ExitStatus {
        unsafe { libc::kill(self.0.id() as libc::pid_t, libc::SIGTERM) };
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(status) = self.0.try_wait().unwrap() {
                return status;
            }
            if Instant::now() >= deadline {
                self.0.kill().unwrap();
                return self.0.wait().unwrap();
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Runner {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            self.stop();
        }
    }
}

struct Schema {
    client: postgres::Client,
    name: String,
}

impl Drop for Schema {
    fn drop(&mut self) {
        let _ = self.client.batch_execute(&format!("drop schema {} cascade", self.name));
    }
}

#[test]
fn background_entrypoint_persists_postgres_and_nxv_without_sqlite() {
    let Ok(url) = std::env::var("ATLAS_PG_TEST_URL") else { return };
    if url.is_empty() {
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let name = format!("t_native_background_{}", std::process::id());
    let mut client = postgres::Client::connect(&url, postgres::NoTls).unwrap();
    atlas::pg::migrate::run(&mut client, &name).unwrap();
    let mut schema = Schema { client, name };
    let posts = (1..=50)
        .map(|number| {
            post(
                number,
                &format!(r#""Example.Release.{}.part{}.rar" yEnc (1/1)"#, (number - 1) / 10, (number - 1) % 10),
                100,
                vec![],
            )
        })
        .collect();
    let server = Server::new(posts);
    let port = spawn_server(Arc::clone(&server));
    let config = configuration(&url, &schema.name, port);
    for case in ["headers_disabled", "postprocessing_enabled", "missing_schema"] {
        let invalid_home = tempfile::tempdir().unwrap();
        let mut invalid = config.clone();
        match case {
            "headers_disabled" => invalid["header_store"]["enabled"] = false.into(),
            "postprocessing_enabled" => invalid["postprocess"]["enabled"] = true.into(),
            "missing_schema" => invalid["storage"]["schema"] = format!("{}_missing", schema.name).into(),
            _ => unreachable!(),
        }
        std::fs::write(invalid_home.path().join("config.json"), invalid.to_string()).unwrap();
        let mut rejected = start(invalid_home.path(), "rejected.log");
        assert!(!wait_for_exit(&mut rejected, Duration::from_secs(5)).success(), "{case}");
        assert_eq!(server.accepted.load(std::sync::atomic::Ordering::SeqCst), 0, "{case}");
        assert!(!invalid_home.path().join("header-store").exists(), "{case}");
        assert_no_sqlite(invalid_home.path());
    }
    std::fs::write(home.path().join("config.json"), config.to_string()).unwrap();
    let mut runner = start(home.path(), "indexer.log");
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        assert_no_sqlite(home.path());
        let count: i64 =
            schema.client.query_one(&format!("select count(*) from {}.segments", schema.name), &[]).unwrap().get(0);
        if count == 50 {
            break;
        }
        assert!(
            runner.0.try_wait().unwrap().is_none(),
            "native background indexer exited before saving: {}",
            std::fs::read_to_string(home.path().join("indexer.log")).unwrap()
        );
        assert!(Instant::now() < deadline, "native background indexer did not commit its input");
        std::thread::sleep(Duration::from_millis(50));
    }
    let conflict_home = tempfile::tempdir().unwrap();
    std::fs::write(conflict_home.path().join("config.json"), config.to_string()).unwrap();
    let accepted = server.accepted.load(std::sync::atomic::Ordering::SeqCst);
    let mut conflict = start(conflict_home.path(), "conflict.log");
    let rejected = wait_for_exit(&mut conflict, Duration::from_secs(5));
    assert!(!rejected.success(), "a second indexer acquired the same schema");
    assert_eq!(server.accepted.load(std::sync::atomic::Ordering::SeqCst), accepted);
    assert!(!conflict_home.path().join("header-store").exists());

    let owners = schema
        .client
        .query(
            "select distinct l.pid from pg_locks l
         where l.locktype = 'advisory' and l.granted and l.objsubid = 2
           and l.database = (select oid from pg_database where datname = current_database())
           and l.classid::bigint = (hashtext('atlas.indexer')::bigint & 4294967295)
           and l.objid::bigint = (hashtext($1)::bigint & 4294967295)",
            &[&schema.name],
        )
        .unwrap();
    assert_eq!(owners.len(), 1, "fixture must identify exactly its own owner");
    let owner: i32 = owners[0].get(0);
    let terminated: bool = schema.client.query_one("select pg_terminate_backend($1)", &[&owner]).unwrap().get(0);
    assert!(terminated);
    assert!(
        !wait_for_exit(&mut runner, Duration::from_secs(10)).success(),
        "loss of ownership must stop the indexer with failure"
    );

    let mut restarted = start(home.path(), "restarted.log");
    let deadline = Instant::now() + Duration::from_secs(10);
    while server.accepted.load(std::sync::atomic::Ordering::SeqCst) == accepted {
        assert!(restarted.0.try_wait().unwrap().is_none(), "native restart failed");
        assert!(Instant::now() < deadline, "restart did not reconnect");
        std::thread::sleep(Duration::from_millis(20));
    }
    edit_config(home.path(), |document| document["request_size"] = toml_edit::value(8));
    std::thread::sleep(Duration::from_millis(5500));
    assert!(restarted.0.try_wait().unwrap().is_none(), "allowed reload stopped the indexer");
    edit_config(home.path(), |document| document["postprocess"]["enabled"] = toml_edit::value(true));
    assert!(
        !wait_for_exit(&mut restarted, Duration::from_secs(7)).success(),
        "unsupported service activation was not refused"
    );
    assert_no_sqlite(home.path());
    edit_config(home.path(), |document| document["postprocess"]["enabled"] = toml_edit::value(false));
    let accepted_before = server.accepted.load(std::sync::atomic::Ordering::SeqCst);
    let mut final_run = start(home.path(), "final.log");
    let deadline = Instant::now() + Duration::from_secs(5);
    while server.accepted.load(std::sync::atomic::Ordering::SeqCst) == accepted_before {
        assert!(final_run.0.try_wait().unwrap().is_none(), "final restart failed");
        assert!(Instant::now() < deadline, "final restart did not connect");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(final_run.stop().success(), "native background shutdown failed");
    assert_no_sqlite(home.path());
    for (table, expected) in [("releases", 5i64), ("files", 50), ("segments", 50)] {
        let actual: i64 =
            schema.client.query_one(&format!("select count(*) from {}.{table}", schema.name), &[]).unwrap().get(0);
        assert_eq!(actual, expected, "{table}");
    }
    let root = home.path().join("header-store");
    let keep =
        atlas::nxv_store::KeepSet { live_from: i64::MIN, claimed: Default::default(), unlinked: Default::default() };
    let (store, _) =
        atlas::nxv_store::store::Store::open(atlas::nxv_store::store::StoreConfig::new(&root), keep).unwrap();
    let mut rows = 0u64;
    for (group, day, _) in store.segments() {
        let (map, entries) = store.day_blocks(&group, day);
        let map = map.unwrap();
        for entry in entries {
            let view =
                nntp_xover_engine::container::NxvBlockView::from_mmap_slice(&map[entry.offset as usize..]).unwrap();
            assert!(view.verify_body());
            if view.is_cold() {
                view.verify_cold_body().unwrap();
            }
            rows += u64::from(view.header.row_count);
        }
    }
    assert!(rows >= 50, "native background lost durable NXV input");
    assert_no_sqlite(home.path());
}

#[test]
fn background_linker_publishes_an_obfuscated_upload_on_postgres() {
    let Ok(url) = std::env::var("ATLAS_PG_TEST_URL") else { return };
    if url.is_empty() {
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let name = format!("t_native_linker_{}", std::process::id());
    let mut client = postgres::Client::connect(&url, postgres::NoTls).unwrap();
    atlas::pg::migrate::run(&mut client, &name).unwrap();
    let mut schema = Schema { client, name };
    let root = home.path().join("header-store");
    let keep = atlas::nxv_store::KeepSet { live_from: i64::MIN, ..Default::default() };
    let (store, _) =
        atlas::nxv_store::store::Store::open(atlas::nxv_store::store::StoreConfig::new(&root), keep).unwrap();
    let first = chrono::Utc::now().timestamp() - 8 * 3600;
    let mut lines = Vec::new();
    for file in 0..=5 {
        for part in 0..if file == 0 { 1 } else { 4 } {
            let number = lines.len() + 1;
            let subject = format!("aa{file:03}p{part:03}g0");
            let date = chrono::DateTime::from_timestamp(first + number as i64, 0)
                .unwrap()
                .format("%a, %d %b %Y %H:%M:%S +0000");
            lines.push(
                format!("{number}\t{subject}\taa{file:03}@x.y\t{date}\t<{subject}@ngPost>\t\t740000\t5692")
                    .into_bytes(),
            );
        }
    }
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let (_, ticket) = runtime
        .block_on(store.record_live(
            "fixture",
            GROUP,
            lines,
            atlas::nxv_store::store::Clamp::Unknown { now: first + 100 },
            None,
        ))
        .unwrap();
    drop(runtime);
    let days: Vec<_> = (first..first + 100).map(|t| t.div_euclid(86400)).collect();
    store.flush_days(&days, Duration::from_secs(5)).unwrap();
    ticket.wait(Duration::from_secs(5)).unwrap();
    store.close().unwrap();
    drop(store);
    let server = Server::new(vec![]);
    let port = spawn_server(server);
    let config = configuration(&url, &schema.name, port);
    std::fs::write(home.path().join("config.json"), config.to_string()).unwrap();
    let mut runner = start(home.path(), "linker.log");
    let deadline = Instant::now() + Duration::from_secs(12);
    loop {
        let count: i64 =
            schema.client.query_one(&format!("select count(*) from {}.segments", schema.name), &[]).unwrap().get(0);
        if count == 21 {
            break;
        }
        assert!(
            runner.0.try_wait().unwrap().is_none(),
            "indexer exited: {}",
            std::fs::read_to_string(home.path().join("linker.log")).unwrap()
        );
        assert!(
            Instant::now() < deadline,
            "linker did not publish its durable input: {}",
            std::fs::read_to_string(home.path().join("linker.log")).unwrap()
        );
        std::thread::sleep(Duration::from_millis(40));
    }
    assert!(runner.stop().success());
    let releases: i64 =
        schema.client.query_one(&format!("select count(*) from {}.releases", schema.name), &[]).unwrap().get(0);
    assert_eq!(releases, 1);
    assert_no_sqlite(home.path());
}

fn configuration(url: &str, schema: &str, port: u16) -> serde_json::Value {
    serde_json::json!({
        "usenet_servers": [{
            "host": "127.0.0.1", "username": "bob", "password": "secret",
            "port": port, "ssl": false, "connections": 1, "priority": 1
        }],
        "groups": [GROUP], "index_mode": "backfill", "batch_size": 50,
        "request_size": 10, "parallel_groups": 1, "auto_run_compact": false,
        "inferred_parts": false, "postprocess": {"enabled": false},
        "predb": {"enabled": false}, "spots": {"enabled": false},
        "archive": {"enabled": false}, "header_store": {"enabled": true},
        "media": {"tvmaze": {"enabled": false}, "allocine": {"enabled": false}, "details_per_minute": 0},
        "metrics": {"enabled": false},
        "storage": {
            "backend": "postgres", "url": url, "schema": schema,
            "max_connections": 4, "allow_plaintext": true
        }
    })
}

fn assert_no_sqlite(home: &std::path::Path) {
    for path in walk_files(home) {
        let mut magic = [0; 16];
        File::open(path).unwrap().read(&mut magic).unwrap();
        assert_ne!(&magic, b"SQLite format 3\0", "native background opened a SQLite store");
    }
}

fn edit_config(home: &std::path::Path, edit: impl FnOnce(&mut toml_edit::DocumentMut)) {
    let path = home.join("config.toml");
    let mut document: toml_edit::DocumentMut = std::fs::read_to_string(&path).unwrap().parse().unwrap();
    edit(&mut document);
    let temporary = home.join("config.tmp");
    std::fs::write(&temporary, document.to_string()).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    std::fs::rename(temporary, path).unwrap();
}

fn start(home: &std::path::Path, log_name: &str) -> Runner {
    let log = File::create(home.join(log_name)).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_atlas"));
    for (key, _) in std::env::vars_os() {
        if key.to_str().is_some_and(|key| key.starts_with("ATLAS_")) {
            command.env_remove(key);
        }
    }
    Runner(
        command
            .arg("--bg-indexer")
            .env("ATLAS_HOME", home)
            .env("ATLAS_NO_KEYRING", "1")
            .env("ATLAS_SAB_DIR", home.join("no-sab"))
            .stdout(Stdio::from(log.try_clone().unwrap()))
            .stderr(Stdio::from(log))
            .spawn()
            .unwrap(),
    )
}

fn wait_for_exit(runner: &mut Runner, timeout: Duration) -> std::process::ExitStatus {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = runner.0.try_wait().unwrap() {
            return status;
        }
        assert!(Instant::now() < deadline, "background indexer did not stop");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn walk_files(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    if let Ok(entries) = root.read_dir() {
        for entry in entries {
            let path = entry.unwrap().path();
            if path.is_dir() {
                files.extend(walk_files(&path));
            } else {
                files.push(path);
            }
        }
    }
    files
}

#[test]
fn historical_readiness_rejects_actual_startup_before_cursor_or_provider_work() {
    let Ok(url) = std::env::var("ATLAS_PG_TEST_URL") else { return };
    if url.is_empty() {
        return;
    }
    for (index, state) in ["loading", "identity_ready"].iter().enumerate() {
        let home = tempfile::tempdir().unwrap();
        let name = format!("t_native_history_{}_{}", std::process::id(), index);
        let mut client = postgres::Client::connect(&url, postgres::NoTls).unwrap();
        atlas::pg::migrate::run(&mut client, &name).unwrap();
        let mut schema = Schema { client, name };
        let snapshot = |client: &mut postgres::Client, name: &str| -> Vec<(String, Option<i64>, Option<String>)> {
            client
                .query(&format!("select key,value,text_value from {name}.meta order by key"), &[])
                .unwrap()
                .iter()
                .map(|r| (r.get(0), r.get(1), r.get(2)))
                .collect()
        };
        let before = snapshot(&mut schema.client, &schema.name);
        let id = [1u8; 32];
        schema.client.execute(&format!("insert into {}.history_imports(import_id,source_fingerprint,contract_version,state,expected_database,expected_schema,expected_role,expected_owner,reserved_next_seq,checkpoint,identity_digest) values($1,$1,1,$2,current_database(),$3,current_user,current_user,900,'',case when $2='identity_ready' then $1::bytea else null::bytea end)",schema.name),&[&&id[..],state,&schema.name]).unwrap();
        let server = Server::new(vec![]);
        let port = spawn_server(Arc::clone(&server));
        let config = configuration(&url, &schema.name, port);
        std::fs::write(home.path().join("config.json"), config.to_string()).unwrap();
        let mut rejected = start(home.path(), "historical-rejected.log");
        assert!(!wait_for_exit(&mut rejected, Duration::from_secs(5)).success());
        let log = std::fs::read_to_string(home.path().join("historical-rejected.log")).unwrap();
        assert!(
            log.contains("historical readiness before native cursor initialization"),
            "startup must reach the persistent readiness guard: {log}"
        );
        assert_eq!(
            snapshot(&mut schema.client, &schema.name),
            before,
            "readiness must precede main cursor initialization"
        );
        assert_eq!(
            server.accepted.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "readiness must precede provider connections"
        );
        assert_no_sqlite(home.path());
    }
}
