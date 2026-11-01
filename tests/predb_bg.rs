//! The feeds and the late path inside the indexer, with compactions going on.
//!
//! One test: it points ATLAS_HOME at a temp dir before any thread is spawned.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use atlas::predb::poll::REAL_REQUESTS;
use atlas::{db, store};
use common::*;

const T: &str = "Alpha.Beta.Gamma.2024.1080p.WEB-DL.H.264-TESTGRP";

fn shard(main: &std::path::Path) -> rusqlite::Connection {
    db::open_shard(&store::shard_path(main, store::shard_of(GROUP))).unwrap()
}

#[test]
#[cfg(unix)]
fn every_shard_compacts_with_the_late_thread_busy() {
    let home = tempfile::tempdir().unwrap();
    // SAFETY: only test in this binary, set before any thread is spawned
    unsafe {
        std::env::set_var("ATLAS_HOME", home.path());
        std::env::set_var("ATLAS_NO_KEYRING", "1");
        std::env::set_var("ATLAS_SAB_DIR", home.path().join("no-sabnzbd"));
        std::env::set_var("ATLAS_COMPACT_EVERY_SECS", "2");
    }
    let mut posts: Vec<Post> = (1..=200)
        .map(|n| post(n, &format!(r#""r{}.part{}.rar" yEnc (1/1)"#, n / 20, n % 20 + 1), 10, vec![]))
        .collect();
    posts.push(post(201, &format!(r#""{T}.mkv" yEnc (1/1)"#), 10, vec![]));
    let port = spawn_server(Server::new(posts));
    let config = serde_json::json!({
        "usenet_servers": [{"host": "127.0.0.1", "username": "bob", "password": "secret", "port": port, "ssl": false, "connections": 2, "priority": 1}],
        "groups": [GROUP], "index_mode": "backfill", "request_size": 50, "auto_run_compact": true,
        "postprocess": {"settle_secs": 1, "complete_settle_secs": 1, "min_size_bytes": 0},
        "predb": {"enabled": true, "sources": ["predb_club", "nope"], "late_per_second": 1}
    });
    std::fs::write(home.path().join("config.json"), config.to_string()).unwrap();
    db::create_db().unwrap();
    let main = home.path().join("atlas.db");

    // old enough for the late path, so it's busy at every stop, and already there when the release settles
    let predb_path = atlas::predb::db::path_for(&main);
    let mut titles = atlas::predb::db::create(&predb_path).unwrap();
    let mut queue = vec![atlas::predb::import::Entry { title: T.into(), ..Default::default() }];
    queue.extend((0..2_000).map(|i| atlas::predb::import::Entry {
        title: format!("Busy.Late.Title.Number.{i:05}-GRP"),
        ..Default::default()
    }));
    atlas::predb::import::import(&mut titles, 1, &queue, 5, None).unwrap();
    drop(titles);
    let predb_inode = std::os::unix::fs::MetadataExt::ino(&std::fs::metadata(&predb_path).unwrap());

    // a page of titles on every poll
    let fetch = Scripted::new();
    let rows: Vec<_> =
        (0..50).map(|i| serde_json::json!({"name": format!("Polled.Title.Number.{i:03}-GRP")})).collect();
    *fetch.always.lock().unwrap() = Some(serde_json::json!({"data": {"rows": rows}}).to_string());
    atlas::predb::set_test_fetch(fetch.clone());

    let before = shard_inodes(&main);
    let stop = Arc::new(AtomicBool::new(false));
    let cfg = atlas::config::load_config().unwrap();
    let runner = {
        let stop = stop.clone();
        std::thread::spawn(move || atlas::bg_indexer::run_until(cfg, stop))
    };

    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        let conn = shard(&main);
        let named: Option<i64> =
            conn.query_row("select name_source from releases where display_name = ?", [T], |r| r.get(0)).ok().flatten();
        let compacted = shard_inodes(&main).iter().zip(&before).all(|(now, was)| now != was);
        if named == Some(100) && compacted {
            break;
        }
        assert!(Instant::now() < deadline, "named: {named:?}, compacted: {compacted}");
        std::thread::sleep(Duration::from_millis(200));
    }
    let matches: i64 = shard(&main).query_row("select count(*) from predb_match", [], |r| r.get(0)).unwrap();
    assert!(matches >= 1, "predb_match rows are kept");
    assert_eq!(
        std::os::unix::fs::MetadataExt::ino(&std::fs::metadata(&predb_path).unwrap()),
        predb_inode,
        "a compaction leaves the titles alone"
    );

    let deadline = Instant::now() + Duration::from_secs(30);
    let stats = loop {
        let stats: serde_json::Value = std::fs::read_to_string(home.path().join("stats.json"))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or(serde_json::Value::Null);
        if stats["predb"]["titles"].as_i64().unwrap_or(0) >= 1 && stats["predb"]["sources"].is_array() {
            break stats;
        }
        assert!(Instant::now() < deadline, "no predb in stats.json");
        std::thread::sleep(Duration::from_millis(100));
    };
    assert_eq!(stats["predb"]["sources"][0]["key"], "predb_club");

    stop.store(true, Ordering::Relaxed);
    assert_eq!(runner.join().unwrap(), 0);
    assert_eq!(REAL_REQUESTS.load(Ordering::Relaxed), 0);
    let urls = fetch.urls.lock().unwrap();
    assert!(!urls.is_empty());
    assert!(urls.iter().all(|u| u.starts_with("https://predb.club/")), "{urls:?}");
    for path in store::shard_paths(&main) {
        let conn = db::open_shard(&path).unwrap();
        let busy: i64 = conn.query_row("pragma wal_checkpoint(truncate)", [], |r| r.get(0)).unwrap();
        assert_eq!(busy, 0, "no reader survived");
    }
}
