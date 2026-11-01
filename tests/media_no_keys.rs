//! A whole indexer with no provider set: no request, no cache file, and everything else works.
//!
//! One test, and the environment is set before any thread starts.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use atlas::{api, db, store};
use common::http::FixtureServer;
use common::{GROUP, Server, post, spawn_server};

#[test]
fn everything_runs_with_no_provider() {
    let home = tempfile::tempdir().unwrap();
    // SAFETY: only test in this binary, set before any thread is spawned
    unsafe {
        std::env::set_var("ATLAS_HOME", home.path());
        std::env::set_var("ATLAS_NO_KEYRING", "1");
        std::env::set_var("ATLAS_SAB_DIR", home.path().join("no-sabnzbd"));
    }
    let provider = FixtureServer::start(vec![]);
    let posts = vec![
        post(1, r#""Some.Show.S01E02.720p.HDTV.x264-GRP.bin" yEnc (1/1)"#, 2_000_000, vec![]),
        post(2, r#""Some.Movie.2019.1080p.BluRay.x264-GRP.bin" yEnc (1/1)"#, 2_000_000, vec![]),
    ];
    let port = spawn_server(Server::new(posts));
    let config = serde_json::json!({
        "usenet_servers": [{"host": "127.0.0.1", "username": "bob", "password": "secret", "port": port, "ssl": false, "connections": 2}],
        "groups": [GROUP], "index_mode": "backfill", "request_size": 50,
        "postprocess": {"settle_secs": 1, "complete_settle_secs": 1, "min_size_bytes": 0},
    });
    std::fs::write(home.path().join("config.json"), config.to_string()).unwrap();
    db::create_db().unwrap();

    let stop = Arc::new(AtomicBool::new(false));
    let cfg = atlas::config::load_config().unwrap();
    let runner = {
        let stop = stop.clone();
        std::thread::spawn(move || atlas::bg_indexer::run_until(cfg, stop))
    };
    let main = home.path().join("atlas.db");
    let shard = db::open_shard(&store::shard_path(&main, store::shard_of(GROUP))).unwrap();
    let count = |sql: &str| -> i64 { shard.query_row(sql, [], |r| r.get(0)).unwrap_or(0) };
    let deadline = Instant::now() + Duration::from_secs(60);
    while count("select count(*) from media_release") < 2 {
        assert!(Instant::now() < deadline, "no media_release rows");
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(count("select count(*) from release_category"), 2);
    let k = "k".to_string();
    let body = api::handle("/api", &format!("t=tvsearch&q=Some+Show&apikey={k}"), "http://x", &k).body;
    assert!(body.contains(r#"name="season" value="1""#), "{body}");
    assert!(body.contains(r#"name="episode" value="2""#), "{body}");
    let by_id = api::handle("/api", &format!("t=tvsearch&tvdbid=1&apikey={k}"), "http://x", &k).body;
    assert!(!by_id.contains("<item>"), "{by_id}");

    stop.store(true, Ordering::Relaxed);
    runner.join().unwrap();
    assert!(!atlas::media::store::path_for(&main).exists());
    assert!(provider.requests().is_empty());
}
