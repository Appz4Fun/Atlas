//! With auto_run_compact on, the indexer stops for a compaction once the
//! interval has passed, records it, and goes back to indexing.
//!
//! One test, and the environment is set before any thread starts.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use common::{GROUP, Server, post, spawn_server};

#[test]
fn compacts_on_schedule_and_keeps_indexing() {
    let home = tempfile::tempdir().unwrap();
    // SAFETY: only test in this binary, set before any thread is spawned
    unsafe {
        std::env::set_var("ATLAS_HOME", home.path());
        std::env::set_var("ATLAS_NO_KEYRING", "1");
        std::env::set_var("ATLAS_SAB_DIR", home.path().join("no-sabnzbd"));
        std::env::set_var("ATLAS_COMPACT_EVERY_SECS", "2");
    }
    let posts: Vec<common::Post> = (1..=200)
        .map(|n| post(n, &format!(r#""r{}.part{}.rar" yEnc (1/1)"#, n / 20, n % 20 + 1), 10, vec![]))
        .collect();
    let port = spawn_server(Server::new(posts));
    let config = serde_json::json!({
        "usenet_servers": [{"host": "127.0.0.1", "username": "bob", "password": "secret", "port": port, "ssl": false, "connections": 2, "priority": 1}],
        "groups": [GROUP], "index_mode": "backfill", "request_size": 50, "auto_run_compact": true
    });
    std::fs::write(home.path().join("config.json"), config.to_string()).unwrap();
    atlas::db::create_db().unwrap();

    let stop = Arc::new(AtomicBool::new(false));
    let cfg = atlas::config::load_config().unwrap();
    assert!(cfg.auto_run_compact);
    let runner = {
        let stop = stop.clone();
        std::thread::spawn(move || atlas::bg_indexer::run_until(cfg, stop))
    };

    let main = home.path().join("atlas.db");
    let deadline = Instant::now() + Duration::from_secs(60);
    let compacted = loop {
        let conn = atlas::db::open_at(&main).unwrap();
        if let Some(t) = atlas::store::get_meta(&conn, "last_compact").unwrap() {
            break t;
        }
        assert!(Instant::now() < deadline, "never compacted");
        std::thread::sleep(Duration::from_millis(200));
    };
    assert!(compacted > 0);

    // indexing goes on after it: everything gets indexed
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let conn = atlas::db::open_with_shards(&main).unwrap();
        if atlas::store::totals(&conn).unwrap().1 >= 200 {
            break;
        }
        assert!(Instant::now() < deadline, "indexing didnt resume");
        std::thread::sleep(Duration::from_millis(200));
    }
    stop.store(true, Ordering::Relaxed);
    assert_eq!(runner.join().unwrap(), 0);
}
