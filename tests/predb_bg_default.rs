//! No `predb` key: no file, no thread, no stats. Hard rule 8.
//!
//! One test: it points ATLAS_HOME at a temp dir before any thread is spawned.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use atlas::predb::poll::REAL_REQUESTS;
use common::*;

#[test]
fn an_existing_config_gets_no_file_no_thread_and_no_stats() {
    let home = tempfile::tempdir().unwrap();
    // SAFETY: only test in this binary, set before any thread is spawned
    unsafe {
        std::env::set_var("ATLAS_HOME", home.path());
        std::env::set_var("ATLAS_NO_KEYRING", "1");
        std::env::set_var("ATLAS_SAB_DIR", home.path().join("no-sabnzbd"));
    }
    let posts = (1..=20).map(|n| post(n, &format!(r#""r{n}.bin" yEnc (1/1)"#), 10, vec![])).collect();
    let port = spawn_server(Server::new(posts));
    let config = serde_json::json!({
        "usenet_servers": [{"host": "127.0.0.1", "username": "bob", "password": "secret", "port": port, "ssl": false, "connections": 2, "priority": 1}],
        "groups": [GROUP], "index_mode": "backfill", "request_size": 50
    });
    std::fs::write(home.path().join("config.json"), config.to_string()).unwrap();
    atlas::db::create_db().unwrap();

    let stop = Arc::new(AtomicBool::new(false));
    let cfg = atlas::config::load_config().unwrap();
    let runner = {
        let stop = stop.clone();
        std::thread::spawn(move || atlas::bg_indexer::run_until(cfg, stop))
    };
    let stats_path = home.path().join("stats.json");
    let deadline = Instant::now() + Duration::from_secs(60);
    let stats = loop {
        let stats: serde_json::Value = std::fs::read_to_string(&stats_path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or(serde_json::Value::Null);
        if stats.get("servers").is_some() {
            break stats;
        }
        assert!(Instant::now() < deadline, "no stats.json");
        std::thread::sleep(Duration::from_millis(100));
    };
    assert!(stats.get("predb").is_none(), "{stats}");
    assert!(!home.path().join("atlas.predb.db").exists());
    stop.store(true, Ordering::Relaxed);
    assert_eq!(runner.join().unwrap(), 0);
    assert_eq!(REAL_REQUESTS.load(Ordering::Relaxed), 0);
}
