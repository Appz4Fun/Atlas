//! No new config keys: a release is named as it always was, and the stage stores nothing. Hard rule 8.
//!
//! One test: it points ATLAS_HOME at a temp dir before any thread is spawned.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use atlas::{db, store};
use common::*;

#[test]
fn no_budget_names_inline_and_stores_nothing() {
    let home = tempfile::tempdir().unwrap();
    // SAFETY: only test in this binary, set before any thread is spawned
    unsafe {
        std::env::set_var("ATLAS_HOME", home.path());
        std::env::set_var("ATLAS_NO_KEYRING", "1");
        std::env::set_var("ATLAS_SAB_DIR", home.path().join("no-sabnzbd"));
    }
    let par2 = par2_file_desc("Hidden.Show.S01E01.mkv", 9_000_000);
    let posts = vec![
        post(1, r#"[1/2] "a1B2c3D4e5F6g7H8i9J0.par2" yEnc (1/1)"#, 2_000_000, yenc_body("x.par2", &par2)),
        post(2, r#"[2/2] "a1B2c3D4e5F6g7H8i9J0.mkv" yEnc (1/3)"#, 2_000_000, vec![]),
        post(3, r#"[2/2] "a1B2c3D4e5F6g7H8i9J0.mkv" yEnc (2/3)"#, 2_000_000, vec![]),
        post(4, r#"[2/2] "a1B2c3D4e5F6g7H8i9J0.mkv" yEnc (3/3)"#, 2_000_000, vec![]),
    ];
    let port = spawn_server(Server::new(posts));
    let config = serde_json::json!({
        "usenet_servers": [{"host": "127.0.0.1", "username": "bob", "password": "secret", "port": port, "ssl": false, "connections": 2, "priority": 1}],
        "groups": [GROUP], "index_mode": "backfill", "request_size": 50,
        "postprocess": {"body_fetches_per_minute": 0}
    });
    let path = home.path().join("config.json");
    std::fs::write(&path, config.to_string()).unwrap();
    db::create_db().unwrap();

    let stop = Arc::new(AtomicBool::new(false));
    let cfg = atlas::config::load_config().unwrap();
    // what isnt in a user's file stays out of it
    atlas::config::save_config(&cfg).unwrap();
    assert!(!std::fs::read_to_string(&path).unwrap().contains("\"archive\""));
    let runner = {
        let stop = stop.clone();
        std::thread::spawn(move || atlas::bg_indexer::run_until(cfg, stop))
    };

    let shard = store::shard_path(&home.path().join("atlas.db"), store::shard_of(GROUP));
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let conn = db::open_shard(&shard).unwrap();
        let named: Option<(String, Option<i64>)> = conn
            .query_row("select display_name, name_source from releases where display_name is not null", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .ok();
        if let Some((name, source)) = named {
            assert_eq!(name, "Hidden.Show.S01E01.mkv");
            assert_eq!(source, None, "the save path named it");
            break;
        }
        assert!(Instant::now() < deadline, "it was never named");
        std::thread::sleep(Duration::from_millis(100));
    }
    let conn = db::open_shard(&shard).unwrap();
    let rows: i64 = conn.query_row("select count(*) from archive_inner_files", [], |r| r.get(0)).unwrap();
    assert_eq!(rows, 0);
    stop.store(true, Ordering::Relaxed);
    assert_eq!(runner.join().unwrap(), 0);
}
