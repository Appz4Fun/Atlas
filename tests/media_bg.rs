//! With providers on, every compaction still rewrites every shard: nothing here outlives the stop.
//!
//! Unix only: it tells the rewrite by the shard files' inodes.
//!
//! One test, and the environment is set before any thread starts.

#![cfg(unix)]

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use atlas::media::store as media_store;
use atlas::{db, store};
use common::http::*;
use common::{GROUP, Server, post, shard_inodes, spawn_server};

#[test]
fn every_shard_is_rewritten_with_the_fan_out_running() {
    let home = tempfile::tempdir().unwrap();
    // SAFETY: only test in this binary, set before any thread is spawned
    unsafe {
        std::env::set_var("ATLAS_HOME", home.path());
        std::env::set_var("ATLAS_NO_KEYRING", "1");
        std::env::set_var("ATLAS_SAB_DIR", home.path().join("no-sabnzbd"));
        std::env::set_var("ATLAS_COMPACT_EVERY_SECS", "2");
    }
    let provider = FixtureServer::start(vec![route("/singlesearch/shows", fixture("tvmaze_show.json"))]);
    let posts: Vec<_> = (1..=600u64)
        .map(|n| {
            post(
                n,
                &format!(r#""Some.Show.S{:02}E{:02}.720p.HDTV.x264-GRP.bin" yEnc (1/1)"#, n / 90 + 1, n % 90),
                2_000_000,
                vec![],
            )
        })
        .collect();
    let port = spawn_server(Server::new(posts));
    let config = serde_json::json!({
        "usenet_servers": [{"host": "127.0.0.1", "username": "bob", "password": "secret", "port": port, "ssl": false, "connections": 2}],
        "groups": [GROUP], "index_mode": "backfill", "request_size": 50, "auto_run_compact": true,
        "postprocess": {"settle_secs": 1, "complete_settle_secs": 1, "min_size_bytes": 0},
        "media": {"tvmaze": {"enabled": true, "base_url": provider.base, "requests_per_minute": 120}},
    });
    std::fs::write(home.path().join("config.json"), config.to_string()).unwrap();
    db::create_db().unwrap();
    let main = home.path().join("atlas.db");

    // a found title owing a fan-out, with rows in several shards
    let key = 42;
    for path in store::shard_paths(&main).iter().take(4) {
        let shard = db::open_shard(path).unwrap();
        for n in 1..=50 {
            shard
                .execute(
                    "insert into media_release (release_id, kind, title_key, quality) values (?, 1, ?, 0)",
                    rusqlite::params![n, key],
                )
                .unwrap();
        }
    }
    let cache = media_store::open(&media_store::path_for(&main)).unwrap();
    let mut t =
        media_store::TitleRow { key, kind: 1, title: "x".into(), state: 1, checked_mask: 1, ..Default::default() };
    t.ids.tvdb = Some(3001);
    media_store::put(&cache, &t).unwrap();
    drop(cache);

    let before = shard_inodes(&main);
    let stop = Arc::new(AtomicBool::new(false));
    let cfg = atlas::config::load_config().unwrap();
    let runner = {
        let stop = stop.clone();
        std::thread::spawn(move || atlas::bg_indexer::run_until(cfg, stop))
    };
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        let conn = db::open_at(&main).unwrap();
        if store::get_meta(&conn, "last_compact").unwrap().is_some() {
            break;
        }
        assert!(Instant::now() < deadline, "never compacted");
        std::thread::sleep(Duration::from_millis(200));
    }
    let after = shard_inodes(&main);
    assert!(after.iter().zip(&before).all(|(now, was)| now != was), "every shard was rewritten");
    let deadline = Instant::now() + Duration::from_secs(60);
    while provider.requests().is_empty() {
        assert!(Instant::now() < deadline, "the net lane never asked");
        std::thread::sleep(Duration::from_millis(100));
    }
    stop.store(true, Ordering::Relaxed);
    runner.join().unwrap();

    let rows: i64 = store::shard_paths(&main)
        .iter()
        .map(|p| {
            db::open_shard(p)
                .unwrap()
                .query_row("select count(*) from media_release", [], |r| r.get::<_, i64>(0))
                .unwrap()
        })
        .sum();
    assert!(rows >= 200, "the rows survived: {rows}");
    assert!(provider.requests().iter().all(|r| r.starts_with("/")), "only its own address");
}
