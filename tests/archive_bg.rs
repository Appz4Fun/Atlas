//! With a body budget, the background indexer's stage names a release.
//!
//! One test: it points ATLAS_HOME at a temp dir before any thread is spawned.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use atlas::{db, store};
use common::*;

#[test]
fn the_stage_names_a_release_when_it_has_a_budget() {
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
        "postprocess": {"body_fetches_per_minute": 6000, "settle_secs": 1, "complete_settle_secs": 1, "body_min_size_bytes": 0}
    });
    std::fs::write(home.path().join("config.json"), config.to_string()).unwrap();
    db::create_db().unwrap();

    let stop = Arc::new(AtomicBool::new(false));
    let cfg = atlas::config::load_config().unwrap();
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
            // naming may tidy the suffix off at the same source
            assert!(name.starts_with("Hidden.Show.S01E01"), "{name}");
            assert_eq!(source, Some(20), "only the stage writes this");
            break;
        }
        assert!(Instant::now() < deadline, "the stage never named it");
        std::thread::sleep(Duration::from_millis(100));
    }
    stop.store(true, Ordering::Relaxed);
    assert_eq!(runner.join().unwrap(), 0);
}
