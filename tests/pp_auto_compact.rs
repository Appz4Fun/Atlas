//! The stage inside the indexer: it lets a compaction have every shard, and carries on.
//!
//! One test, and the environment is set before any thread starts.

mod common;

use std::os::unix::fs::MetadataExt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use atlas::postprocess::step::{Needs, Outcome};
use common::stage::{Log, TestStep};
use common::{GROUP, Server, post, spawn_server};

#[test]
fn the_stage_stops_for_a_compaction_and_carries_on() {
    let home = tempfile::tempdir().unwrap();
    // SAFETY: only test in this binary, set before any thread is spawned
    unsafe {
        std::env::set_var("ATLAS_HOME", home.path());
        std::env::set_var("ATLAS_NO_KEYRING", "1");
        std::env::set_var("ATLAS_SAB_DIR", home.path().join("no-sabnzbd"));
        std::env::set_var("ATLAS_COMPACT_EVERY_SECS", "2");
    }
    // more than the stage gets through before the first compaction
    let posts: Vec<_> = (1..=600).map(|n| post(n, &format!(r#""release{n}.bin" yEnc (1/1)"#), 10, vec![])).collect();
    let port = spawn_server(Server::new(posts));
    let config = serde_json::json!({
        "usenet_servers": [{"host": "127.0.0.1", "username": "bob", "password": "secret", "port": port, "ssl": false, "connections": 2, "priority": 1}],
        "groups": [GROUP], "index_mode": "backfill", "request_size": 50, "auto_run_compact": true,
        "postprocess": {"settle_secs": 1, "complete_settle_secs": 1},
    });
    std::fs::write(home.path().join("config.json"), config.to_string()).unwrap();
    atlas::db::create_db().unwrap();

    let log: Log = Default::default();
    let step = TestStep::new(9, "slow", 200, Needs::Database, &log).runs(|_| {
        std::thread::sleep(Duration::from_millis(100));
        Ok(Outcome::default())
    });
    atlas::postprocess::set_test_steps(vec![step.arc()]);

    let main = home.path().join("atlas.db");
    let shards = atlas::store::shard_paths(&main);
    let inodes = || shards.iter().map(|p| std::fs::metadata(p).unwrap().ino()).collect::<Vec<_>>();
    let before = inodes();
    let shard = atlas::store::shard_path(&main, atlas::store::shard_of(GROUP));
    let states = || -> i64 {
        atlas::db::open_shard(&shard)
            .and_then(|conn| conn.query_row("select count(*) from pp_state", [], |r| r.get(0)))
            .unwrap_or(0)
    };

    let stop = Arc::new(AtomicBool::new(false));
    let cfg = atlas::config::load_config().unwrap();
    let runner = {
        let stop = stop.clone();
        std::thread::spawn(move || atlas::bg_indexer::run_until(cfg, stop))
    };

    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        let conn = atlas::db::open_at(&main).unwrap();
        if atlas::store::get_meta(&conn, "last_compact").unwrap().is_some() {
            break;
        }
        assert!(Instant::now() < deadline, "never compacted");
        std::thread::sleep(Duration::from_millis(200));
    }
    assert!(
        inodes().iter().zip(&before).all(|(now, was)| now != was),
        "every shard was rewritten: a thread still reading fails one"
    );
    let after_compaction = states();

    let deadline = Instant::now() + Duration::from_secs(60);
    while states() <= after_compaction {
        assert!(Instant::now() < deadline, "the stage didnt carry on, {after_compaction} rows");
        std::thread::sleep(Duration::from_millis(100));
    }

    // what it tells the dashboards
    let deadline = Instant::now() + Duration::from_secs(30);
    let stats = loop {
        let stats: serde_json::Value =
            serde_json::from_slice(&std::fs::read(home.path().join("stats.json")).unwrap_or_default())
                .unwrap_or_default();
        if stats["postprocess"]["queue_due"].is_i64() {
            break stats;
        }
        assert!(Instant::now() < deadline, "no postprocess in stats.json");
        std::thread::sleep(Duration::from_millis(200));
    };
    assert!(stats["postprocess"]["walk"].is_object());
    let status: serde_json::Value =
        serde_json::from_slice(&std::fs::read(home.path().join("status.json")).unwrap()).unwrap();
    assert!(status["postprocess_backlog"].is_i64());

    stop.store(true, Ordering::Relaxed);
    assert_eq!(runner.join().unwrap(), 0);
}
