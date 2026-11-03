//! Where collections start in a shard: only an indexer of this build says so.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use atlas::parser::{Article, Release};
use atlas::{bg_indexer, config, db, paths, store};
use common::{GROUP, Server, post, spawn_server};
use rusqlite::OptionalExtension;

#[test]
fn only_the_indexer_says_where_collections_start() {
    let home = tempfile::tempdir().unwrap();
    // SAFETY: only test in this binary, set before any thread is spawned
    unsafe {
        std::env::set_var("ATLAS_HOME", home.path());
        std::env::set_var("ATLAS_NO_KEYRING", "1");
        std::env::set_var("ATLAS_SAB_DIR", home.path().join("no-sabnzbd"));
    }
    let port = spawn_server(Server::new(vec![post(1, r#"[1/2] "Straddling.Post.bin" yEnc (2/2)"#, 10, vec![])]));
    let config = serde_json::json!({
        "usenet_servers": [{"host": "127.0.0.1", "username": "bob", "password": "secret", "port": port, "ssl": false, "connections": 2}],
        "groups": [GROUP],
        "index_mode": "backfill",
    });
    std::fs::write(home.path().join("config.json"), config.to_string()).unwrap();

    db::create_db().unwrap();
    let main = paths::database();
    let shard = db::open_shard(&store::shard_path(&main, store::shard_of(GROUP))).unwrap();
    let from = || -> Option<i64> {
        shard.query_row("select value from meta where key = 'grouping_from'", [], |r| r.get(0)).optional().unwrap()
    };
    assert_eq!(from(), None, "opening a shard doesnt start it");

    // an older indexer goes on writing, with this build's menu open
    let old = Release {
        name: "Straddling.Post.bin".into(),
        group: GROUP.into(),
        poster: "poster <p@mock>".into(),
        articles: vec![Article {
            subject: r#"[1/2] "Straddling.Post.bin" yEnc (1/2)"#.into(),
            filename: Some("Straddling.Post.bin".into()),
            part: Some(1),
            total_parts: Some(2),
            file_index: Some(1),
            file_total: Some(2),
            message_id: "<old@x>".into(),
            bytes: 10,
            ..Default::default()
        }],
        ..Default::default()
    };
    store::save(&main, &[old]).unwrap();
    db::create_db().unwrap();
    assert_eq!(from(), None, "nor does opening it again");
    let max: i64 = shard.query_row("select max(id) from releases", [], |r| r.get(0)).unwrap();

    let stop = Arc::new(AtomicBool::new(false));
    let cfg = config::load_config().unwrap();
    let runner = {
        let stop = stop.clone();
        std::thread::spawn(move || bg_indexer::run_until(cfg, stop))
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    let parts = || -> i64 { shard.query_row("select parts from releases", [], |r| r.get(0)).unwrap_or(0) };
    while parts() < 2 {
        assert!(Instant::now() < deadline, "the post never arrived");
        std::thread::sleep(Duration::from_millis(50));
    }
    stop.store(true, Ordering::Relaxed);
    assert_eq!(runner.join().unwrap(), 0);

    assert_eq!(from(), Some(max + 1), "what the older indexer saved is below it");
    let rows: i64 = shard.query_row("select count(*) from releases", [], |r| r.get(0)).unwrap();
    assert_eq!(rows, 1, "the file went on in the row it started in");
    let collections: i64 = shard.query_row("select count(*) from collections", [], |r| r.get(0)).unwrap();
    assert_eq!(collections, 0);

    // a later run leaves it where it is
    store::start_grouping(&main).unwrap();
    assert_eq!(from(), Some(max + 1));
}
