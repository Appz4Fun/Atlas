//! Native storage through the actual background-indexer entry point.
mod common;
#[path = "common/pg_outage.rs"]
mod pg_outage;

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
    let deadline = Instant::now() + Duration::from_secs(12);
    loop {
        assert!(runner.0.try_wait().unwrap().is_none(), "owner loss must pause the same process");
        let fresh: bool = schema
            .client
            .query_one(
                "select exists(select 1 from pg_locks where locktype='advisory' and granted and objsubid=2
             and database=(select oid from pg_database where datname=current_database())
             and classid::bigint=(hashtext('atlas.indexer')::bigint & 4294967295)
             and objid::bigint=(hashtext($1)::bigint & 4294967295) and pid<>$2)",
                &[&schema.name, &owner],
            )
            .unwrap()
            .get(0);
        if fresh {
            break;
        }
        assert!(Instant::now() < deadline, "a fresh owner was not acquired after outage");
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(runner.stop().success());

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
fn native_startup_offline_pauses_without_provider_or_sqlite_and_stops_promptly() {
    let home = tempfile::tempdir().unwrap();
    let server = Server::new(vec![]);
    let port = spawn_server(Arc::clone(&server));
    let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let unused = socket.local_addr().unwrap().port();
    drop(socket);
    let url = format!("host=127.0.0.1 port={unused} dbname=example user=example connect_timeout=1");
    let config = configuration(&url, "example", port);
    std::fs::write(home.path().join("config.json"), config.to_string()).unwrap();
    let mut runner = start(home.path(), "offline.log");
    std::thread::sleep(Duration::from_secs(4));
    assert!(runner.0.try_wait().unwrap().is_none(), "startup outage must keep the process alive");
    assert_eq!(server.accepted.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert!(!home.path().join("header-store").exists());
    assert_no_sqlite(home.path());
    let status: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(home.path().join("status.json")).unwrap()).unwrap();
    assert_eq!(status["status"], "postgres_paused");
    let began = Instant::now();
    assert!(runner.stop().success());
    assert!(began.elapsed() < Duration::from_secs(3));
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
        let mut magic = Vec::with_capacity(16);
        File::open(path).unwrap().take(16).read_to_end(&mut magic).unwrap();
        assert_ne!(magic.as_slice(), b"SQLite format 3\0", "native background opened a SQLite store");
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

fn wait_until(runner: &mut Runner, home: &std::path::Path, seconds: u64, mut condition: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(seconds);
    while !condition() {
        assert!(
            runner.0.try_wait().unwrap().is_none(),
            "outage runner exited: {}",
            std::fs::read_to_string(home.join("outage.log")).unwrap()
        );
        assert!(
            Instant::now() < until,
            "owned outage condition timed out: {}",
            std::fs::read_to_string(home.join("outage.log")).unwrap()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn paused(home: &std::path::Path) -> bool {
    std::fs::read_to_string(home.join("status.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .is_some_and(|status| status["status"] == "postgres_paused")
}

fn owned_backend(schema: &mut Schema) -> Option<i32> {
    let rows = schema
        .client
        .query(
            "select pid from pg_locks where locktype='advisory' and granted and objsubid=2
        and database=(select oid from pg_database where datname=current_database())
        and classid::bigint=(hashtext('atlas.indexer')::bigint & 4294967295)
        and objid::bigint=(hashtext($1)::bigint & 4294967295)",
            &[&schema.name],
        )
        .unwrap();
    assert!(rows.len() <= 1, "concurrent advisory owners");
    rows.first().map(|row| row.get(0))
}

fn identities(schema: &mut Schema) -> Vec<String> {
    [
        "select id,group_name,encode(identity_key,'hex') identity_key,name from releases order by id",
        "select id,release_id,encode(filename_key,'hex') filename_key,filename from files order by id",
        "select release_id,file_id,encode(message_id,'hex') message_id,part,bytes from segments order by release_id,message_key",
        "select release_id,generation from pp_queue order by release_id",
    ].iter().map(|query| {
        let query = query.replace("from releases", &format!("from {}.releases",schema.name))
            .replace("from files", &format!("from {}.files",schema.name))
            .replace("from segments", &format!("from {}.segments",schema.name))
            .replace("from pp_queue", &format!("from {}.pp_queue",schema.name));
        schema.client.query_one(&format!("select coalesce(json_agg(row_to_json(t)),'[]')::text from ({query}) t"), &[]).unwrap().get(0)
    }).collect()
}

fn fifty_posts() -> Vec<common::Post> {
    (1..=50)
        .map(|n| {
            post(n, &format!(r#""Example.Release.{}.part{}.rar" yEnc (1/1)"#, (n - 1) / 10, (n - 1) % 10), 100, vec![])
        })
        .collect()
}

#[test]
fn owned_proxy_repeated_outages_and_lost_commit_ack_resume_exact_data() {
    use std::sync::atomic::Ordering::SeqCst;
    let Ok(url) = std::env::var("ATLAS_PG_TEST_URL") else { return };
    if url.is_empty() {
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let name = format!("t_outage_{}", std::process::id());
    let mut client = postgres::Client::connect(&url, postgres::NoTls).unwrap();
    atlas::pg::migrate::run(&mut client, &name).unwrap();
    let mut schema = Schema { client, name };
    let proxy = pg_outage::Proxy::new(&url);
    let server = Server::new(fifty_posts());
    let port = spawn_server(server.clone());
    let config = configuration(&proxy.url(&url), &schema.name, port);
    std::fs::write(home.path().join("config.json"), config.to_string()).unwrap();
    let mut runner = start(home.path(), "outage.log");
    wait_until(&mut runner, home.path(), 5, || paused(home.path()));
    std::thread::sleep(Duration::from_secs(2));
    assert_eq!(server.accepted.load(SeqCst), 0);
    assert!(!home.path().join("header-store").exists());
    assert_no_sqlite(home.path());
    proxy.suppress_commit.store(true, SeqCst);
    proxy.online.store(true, SeqCst);
    wait_until(&mut runner, home.path(), 35, || {
        schema
            .client
            .query_one(&format!("select count(*) from {}.segments", schema.name), &[])
            .unwrap()
            .get::<_, i64>(0)
            == 50
            && proxy.ambiguous.load(SeqCst) == 1
            && !paused(home.path())
    });
    // Let durable cursor acknowledgment catch up with the final release save.
    std::thread::sleep(Duration::from_secs(2));
    let expected = identities(&mut schema);
    let generation = schema
        .client
        .query_one(&format!("select count(*),min(generation),max(generation) from {}.pp_queue", schema.name), &[])
        .unwrap();
    assert_eq!(
        (generation.get::<_, i64>(0), generation.get::<_, i64>(1), generation.get::<_, i64>(2)),
        (5, 1, 1),
        "ambiguous replay incremented semantic queue generations"
    );
    let mut owner = owned_backend(&mut schema).unwrap();
    let mut seen = std::collections::HashSet::from([owner]);
    let baseline_resources = child_resources(runner.0.id());
    let mut prior_cursor: Option<i64> = None;
    for _ in 0..3 {
        proxy.drop_connections();
        wait_until(&mut runner, home.path(), 6, || paused(home.path()));
        let fetched = server.xovers.load(SeqCst);
        std::thread::sleep(Duration::from_millis(1200));
        assert_eq!(server.xovers.load(SeqCst), fetched, "provider ingest continued while paused");
        assert_no_sqlite(home.path());
        proxy.online.store(true, SeqCst);
        wait_until(&mut runner, home.path(), 15, || {
            owned_backend(&mut schema).is_some_and(|pid| pid != owner) && !paused(home.path())
        });
        owner = owned_backend(&mut schema).unwrap();
        assert!(seen.insert(owner), "reused a dead ownership session");
        assert_eq!(identities(&mut schema), expected, "replay changed persisted identities or queue generations");
        let cursor: Option<i64> = schema
            .client
            .query_one(&format!("select min(backfill_cursor) from {}.groups", schema.name), &[])
            .unwrap()
            .get(0);
        if let (Some(old), Some(new)) = (prior_cursor, cursor) {
            assert!(new <= old, "backfill cursor moved backward in progress");
        }
        prior_cursor = cursor;
        let resources = child_resources(runner.0.id());
        assert!(
            resources.0 <= baseline_resources.0 + 4,
            "file descriptors accumulated: {baseline_resources:?} -> {resources:?}"
        );
        assert!(
            resources.1 <= baseline_resources.1 + 4,
            "threads accumulated: {baseline_resources:?} -> {resources:?}"
        );
        eprintln!("recovered child resources: fds={} threads={}", resources.0, resources.1);
    }
    assert_eq!(proxy.ambiguous.load(SeqCst), 1);
    assert!(runner.stop().success());
    assert_no_sqlite(home.path());
}

#[test]
fn silent_owned_forward_quarantines_until_return_then_uses_fresh_owner() {
    use std::sync::atomic::Ordering::SeqCst;
    let Ok(url) = std::env::var("ATLAS_PG_TEST_URL") else { return };
    if url.is_empty() {
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let name = format!("t_blackhole_{}", std::process::id());
    let mut client = postgres::Client::connect(&url, postgres::NoTls).unwrap();
    atlas::pg::migrate::run(&mut client, &name).unwrap();
    let mut schema = Schema { client, name };
    let proxy = pg_outage::Proxy::new(&url);
    proxy.online.store(true, SeqCst);
    let server = Server::new(fifty_posts());
    let port = spawn_server(server.clone());
    let config = configuration(&proxy.url(&url), &schema.name, port);
    std::fs::write(home.path().join("config.json"), config.to_string()).unwrap();
    let mut runner = start(home.path(), "outage.log");
    wait_until(&mut runner, home.path(), 25, || {
        schema
            .client
            .query_one(&format!("select count(*) from {}.segments", schema.name), &[])
            .unwrap()
            .get::<_, i64>(0)
            == 50
    });
    std::thread::sleep(Duration::from_secs(2));
    let expected = identities(&mut schema);
    let owner = owned_backend(&mut schema).unwrap();
    proxy.blackhole.store(true, SeqCst);
    wait_until(&mut runner, home.path(), 35, || paused(home.path()));
    assert_eq!(owned_backend(&mut schema), Some(owner), "replaced an unresolved old writer");
    let fetched = server.xovers.load(SeqCst);
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(server.xovers.load(SeqCst), fetched);
    proxy.blackhole.store(false, SeqCst);
    wait_until(&mut runner, home.path(), 15, || {
        owned_backend(&mut schema).is_some_and(|pid| pid != owner) && !paused(home.path())
    });
    assert_eq!(identities(&mut schema), expected);
    // Operator stop remains monotonic while an old callback is quarantined.
    proxy.blackhole.store(true, SeqCst);
    std::thread::sleep(Duration::from_millis(1300));
    unsafe {
        libc::kill(runner.0.id() as libc::pid_t, libc::SIGTERM);
    }
    std::thread::sleep(Duration::from_millis(200));
    proxy.blackhole.store(false, SeqCst);
    assert!(wait_for_exit(&mut runner, Duration::from_secs(6)).success());
    assert!(owned_backend(&mut schema).is_none());
    assert_no_sqlite(home.path());
}

#[test]
fn native_changed_catalog_identity_after_outage_is_fatal_before_ingest() {
    use std::sync::atomic::Ordering::SeqCst;
    let Ok(url) = std::env::var("ATLAS_PG_TEST_URL") else { return };
    if url.is_empty() {
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let name = format!("t_identity_{}", std::process::id());
    let mut client = postgres::Client::connect(&url, postgres::NoTls).unwrap();
    atlas::pg::migrate::run(&mut client, &name).unwrap();
    let mut schema = Schema { client, name };
    let proxy = pg_outage::Proxy::new(&url);
    proxy.online.store(true, SeqCst);
    let server = Server::new(vec![]);
    let port = spawn_server(server.clone());
    let config = configuration(&proxy.url(&url), &schema.name, port);
    std::fs::write(home.path().join("config.json"), config.to_string()).unwrap();
    let mut runner = start(home.path(), "outage.log");
    wait_until(&mut runner, home.path(), 10, || owned_backend(&mut schema).is_some() && !paused(home.path()));
    proxy.drop_connections();
    wait_until(&mut runner, home.path(), 6, || paused(home.path()));
    // This is the unique test-owned schema only.
    schema.client.batch_execute(&format!("drop schema {} cascade", schema.name)).unwrap();
    atlas::pg::migrate::run(&mut schema.client, &schema.name).unwrap();
    let accepted = server.accepted.load(SeqCst);
    let before: i64 =
        schema.client.query_one(&format!("select count(*) from {}.meta", schema.name), &[]).unwrap().get(0);
    proxy.online.store(true, SeqCst);
    assert!(!wait_for_exit(&mut runner, Duration::from_secs(12)).success());
    assert_eq!(server.accepted.load(SeqCst), accepted);
    let rows: i64 = schema.client.query_one(&format!("select count(*) from {}.meta", schema.name), &[]).unwrap().get(0);
    assert_eq!(rows, before, "identity refusal mutated the replacement catalog");
}

#[cfg(target_os = "macos")]
fn child_resources(pid: u32) -> (usize, usize) {
    let mut info = std::mem::MaybeUninit::<libc::proc_taskinfo>::zeroed();
    // SAFETY: this exact test-owned live child PID is read only; buffers have
    // the sizes expected by proc_pidinfo and are initialized only on success.
    let count = unsafe {
        libc::proc_pidinfo(
            pid as i32,
            libc::PROC_PIDTASKINFO,
            0,
            info.as_mut_ptr().cast(),
            std::mem::size_of::<libc::proc_taskinfo>() as i32,
        )
    };
    assert_eq!(count as usize, std::mem::size_of::<libc::proc_taskinfo>());
    let info = unsafe { info.assume_init() };
    let bytes = unsafe { libc::proc_pidinfo(pid as i32, libc::PROC_PIDLISTFDS, 0, std::ptr::null_mut(), 0) };
    assert!(bytes > 0);
    let unit = std::mem::size_of::<libc::proc_fdinfo>();
    let mut descriptors = vec![std::mem::MaybeUninit::<libc::proc_fdinfo>::zeroed(); bytes as usize / unit + 32];
    let actual = unsafe {
        libc::proc_pidinfo(
            pid as i32,
            libc::PROC_PIDLISTFDS,
            0,
            descriptors.as_mut_ptr().cast(),
            (descriptors.len() * unit) as i32,
        )
    };
    assert!(actual > 0);
    (actual as usize / unit, info.pti_threadnum as usize)
}
#[cfg(target_os = "linux")]
fn child_resources(pid: u32) -> (usize, usize) {
    let fds = std::fs::read_dir(format!("/proc/{pid}/fd")).unwrap().count();
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap();
    let threads = status.lines().find_map(|line| line.strip_prefix("Threads:")).unwrap().trim().parse().unwrap();
    (fds, threads)
}
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn child_resources(_: u32) -> (usize, usize) {
    (0, 0)
}

#[test]
fn unsupported_reload_while_postgres_paused_is_fatal() {
    let home = tempfile::tempdir().unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let unused = listener.local_addr().unwrap().port();
    drop(listener);
    let config = configuration(
        &format!("host=127.0.0.1 port={unused} dbname=example user=example connect_timeout=1"),
        "example",
        1,
    );
    std::fs::write(home.path().join("config.json"), config.to_string()).unwrap();
    let mut runner = start(home.path(), "outage.log");
    wait_until(&mut runner, home.path(), 5, || paused(home.path()) && home.path().join("config.toml").exists());
    edit_config(home.path(), |document| document["postprocess"]["enabled"] = toml_edit::value(true));
    assert!(!wait_for_exit(&mut runner, Duration::from_secs(4)).success());
    assert!(!home.path().join("header-store").exists());
    assert_no_sqlite(home.path());
}

#[test]
fn authentication_failure_is_fatal_before_provider_or_nxv_startup() {
    let Ok(url) = std::env::var("ATLAS_PG_TEST_URL") else { return };
    if url.is_empty() {
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let proxy = pg_outage::Proxy::new(&url);
    proxy.online.store(true, std::sync::atomic::Ordering::Release);
    let server = Server::new(vec![]);
    let port = spawn_server(server.clone());
    let url = format!("{} user=outage_missing_role_{}", proxy.url(&url), std::process::id());
    let config = configuration(&url, "example", port);
    std::fs::write(home.path().join("config.json"), config.to_string()).unwrap();
    let mut runner = start(home.path(), "outage.log");
    assert!(!wait_for_exit(&mut runner, Duration::from_secs(5)).success());
    assert_eq!(server.accepted.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert!(!home.path().join("header-store").exists());
    assert_no_sqlite(home.path());
}

#[test]
fn quarantined_pre_provider_reply_never_admits_the_old_attempt() {
    use std::sync::{Mutex, atomic::Ordering::SeqCst};
    let Ok(url) = std::env::var("ATLAS_PG_TEST_URL") else { return };
    if url.is_empty() {
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let name = format!("t_preflight_{}", std::process::id());
    let mut client = postgres::Client::connect(&url, postgres::NoTls).unwrap();
    atlas::pg::migrate::run(&mut client, &name).unwrap();
    let mut schema = Schema { client, name };
    let proxy = pg_outage::Proxy::new(&url);
    proxy.online.store(true, SeqCst);
    proxy.hold_preflight.store(true, SeqCst);
    let observed = Arc::new(Mutex::new(Vec::<i32>::new()));
    let captured = observed.clone();
    let audit_name = schema.name.clone();
    let audit = Mutex::new(postgres::Client::connect(&url, postgres::NoTls).unwrap());
    let mut server = Server::new(vec![]);
    Arc::get_mut(&mut server).unwrap().before_connection = Some(Arc::new(move || {
        let mut client = audit.lock().unwrap();
        let owner: i32 = client
            .query_one(
                "select pid from pg_locks where locktype='advisory' and granted and objsubid=2
             and database=(select oid from pg_database where datname=current_database())
             and classid::bigint=(hashtext('atlas.indexer')::bigint & 4294967295)
             and objid::bigint=(hashtext($1)::bigint & 4294967295)",
                &[&audit_name],
            )
            .unwrap()
            .get(0);
        captured.lock().unwrap().push(owner);
    }));
    let port = spawn_server(server.clone());
    let config = configuration(&proxy.url(&url), &schema.name, port);
    std::fs::write(home.path().join("config.json"), config.to_string()).unwrap();
    let mut runner = start(home.path(), "outage.log");
    wait_until(&mut runner, home.path(), 10, || proxy.preflight_held.load(SeqCst) && !paused(home.path()));
    let old = owned_backend(&mut schema).unwrap();
    wait_until(&mut runner, home.path(), 35, || paused(home.path()));
    assert_eq!(server.accepted.load(SeqCst), 0);
    proxy.release_preflight.store(true, SeqCst);
    wait_until(&mut runner, home.path(), 15, || !observed.lock().unwrap().is_empty());
    let owners = observed.lock().unwrap().clone();
    assert!(
        owners.iter().all(|&owner| owner != old),
        "quarantined old owner contacted a provider: {owners:?}, old={old}"
    );
    assert!(owned_backend(&mut schema).is_some_and(|owner| owner != old));
    assert!(runner.stop().success());
    assert_no_sqlite(home.path());
}

fn operator_term_at_admission(held_preflight: bool) {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
    let Ok(url) = std::env::var("ATLAS_PG_TEST_URL") else { return };
    if url.is_empty() {
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let phase = if held_preflight { "preflight" } else { "greeting" };
    let name = format!("t_operator_admission_{}_{phase}", std::process::id());
    let mut client = postgres::Client::connect(&url, postgres::NoTls).unwrap();
    atlas::pg::migrate::run(&mut client, &name).unwrap();
    let mut schema = Schema { client, name };
    let proxy = pg_outage::Proxy::new(&url);
    proxy.online.store(true, SeqCst);
    proxy.hold_preflight.store(held_preflight, SeqCst);
    let connections = Arc::new(AtomicUsize::new(0));
    let release_greeting = Arc::new(AtomicBool::new(false));
    struct Release(Arc<AtomicBool>);
    impl Drop for Release {
        fn drop(&mut self) {
            self.0.store(true, SeqCst);
        }
    }
    let _release_on_exit = Release(release_greeting.clone());
    let (accepted, released) = (connections.clone(), release_greeting.clone());
    let mut server = Server::new(vec![]);
    Arc::get_mut(&mut server).unwrap().before_connection = Some(Arc::new(move || {
        accepted.fetch_add(1, SeqCst);
        if !held_preflight {
            let until = Instant::now() + Duration::from_secs(10);
            while !released.load(SeqCst) && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }));
    let port = spawn_server(server.clone());
    let config = configuration(&proxy.url(&url), &schema.name, port);
    std::fs::write(home.path().join("config.json"), config.to_string()).unwrap();
    let mut runner = start(home.path(), "outage.log");
    wait_until(&mut runner, home.path(), 10, || {
        if held_preflight {
            proxy.preflight_held.load(SeqCst) && !paused(home.path())
        } else {
            connections.load(SeqCst) == 1
        }
    });
    assert!(owned_backend(&mut schema).is_some());
    let began = Instant::now();
    // Signal only this fixture's Atlas PID. A pending successful reply remains
    // held until after TERM, so cancellation and its drain are exercised.
    assert_eq!(unsafe { libc::kill(runner.0.id() as libc::pid_t, libc::SIGTERM) }, 0);
    if held_preflight {
        std::thread::sleep(Duration::from_millis(100));
        assert!(runner.0.try_wait().unwrap().is_none(), "held synchronous work was abandoned");
        proxy.release_preflight.store(true, SeqCst);
    }
    let status = wait_for_exit(&mut runner, Duration::from_secs(3));
    assert!(
        status.success(),
        "ordinary TERM during {phase} must exit successfully, got {status}: {}",
        std::fs::read_to_string(home.path().join("outage.log")).unwrap()
    );
    assert!(began.elapsed() < Duration::from_secs(3));
    assert!(owned_backend(&mut schema).is_none(), "operator stop retained its owner");
    assert_eq!(connections.load(SeqCst), usize::from(!held_preflight), "operator stop re-admitted provider work");
    let status: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(home.path().join("status.json")).unwrap()).unwrap();
    assert_eq!(status["status"], "stopped");
    assert_eq!(status["running"], false);
    assert_eq!(status["error"], false);
    assert_eq!(status["error_count"], 0);
    let log = std::fs::read_to_string(home.path().join("outage.log")).unwrap();
    assert!(!log.contains("native PostgreSQL indexing failed"), "operator stop reported a fatal failure");
    assert!(!log.contains("native PostgreSQL recovered"), "operator stop entered recovery");
    assert_no_sqlite(home.path());
}

#[test]
fn native_operator_term_during_pending_greeting_exits_cleanly() {
    operator_term_at_admission(false);
}

#[test]
fn native_operator_term_after_held_preflight_exits_cleanly() {
    operator_term_at_admission(true);
}
