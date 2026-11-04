//! A pass's cursor moves on past each slice saved, not only once the pass is over: prod's passes can take longer than
//! it runs between restarts, and one stopped partway kept none of its progress.
//!
//! One test, and the environment is set before any thread starts.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use common::{GROUP, Server, post, spawn_server};

/// way longer than the test waits
const STALL: Duration = Duration::from_secs(20);

/// 200 posts, the XOVER of the slice starting at `stall_at` stuck
fn server(stall_at: u64) -> Arc<Server> {
    let posts =
        (1..=200).map(|n| post(n, &format!(r#""rel{}.part{}.rar" yEnc (1/1)"#, n / 20, n % 20 + 1), 10, vec![]));
    let mut s = Server::new(posts.collect());
    let s_mut = Arc::get_mut(&mut s).unwrap();
    s_mut.stall = STALL;
    *s_mut.stall_at.lock().unwrap() = Some(stall_at);
    s
}

/// index `mode` against a server stuck at `stall_at` until a request is stuck and the slices before it are saved,
/// then stop; the group's (live, backfill) cursors
fn run_until_stuck(home: &std::path::Path, mode: &str, stall_at: u64) -> (i64, i64) {
    let state = server(stall_at);
    let port = spawn_server(state.clone());
    let config = serde_json::json!({
        "usenet_servers": [{"host": "127.0.0.1", "username": "bob", "password": "secret", "port": port, "ssl": false, "connections": 1, "priority": 1}],
        "group": GROUP,
        "groups": [GROUP],
        "index_mode": mode,
        "batch_size": 200,
        "request_size": 20
    });
    std::fs::write(home.join("config.json"), config.to_string()).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let stopper = {
        let stop = stop.clone();
        std::thread::spawn(move || {
            while state.stalled.load(Ordering::SeqCst) == 0 {
                std::thread::sleep(Duration::from_millis(10));
            }
            // what came before is saved meanwhile
            std::thread::sleep(Duration::from_millis(1500));
            stop.store(true, Ordering::SeqCst);
        })
    };
    let cfg = atlas::config::load_config().unwrap();
    let runner = std::thread::spawn(move || atlas::bg_indexer::run_until(cfg, stop));
    stopper.join().unwrap();
    assert_eq!(runner.join().unwrap(), 0);
    let db = atlas::db::open().unwrap();
    db.query_row("select live_cursor, backfill_cursor from groups where name = ?", [GROUP], |r| {
        Ok((r.get(0)?, r.get(1)?))
    })
    .unwrap()
}

#[test]
fn a_pass_stopped_partway_keeps_the_cursor_past_what_it_saved() {
    let home = tempfile::tempdir().unwrap();
    // SAFETY: only test in this binary, set before any thread is spawned
    unsafe {
        std::env::set_var("ATLAS_HOME", home.path());
        std::env::set_var("ATLAS_NO_KEYRING", "1");
        std::env::set_var("ATLAS_SAB_DIR", home.path().join("no-sabnzbd"));
    }
    atlas::db::create_db().unwrap();
    let db = atlas::db::open().unwrap();
    // live from the start, backfill from the top
    atlas::db::save_group_state(&db, GROUP, atlas::db::GroupState { live_cursor: 0, backfill_cursor: 200 }).unwrap();
    drop(db);

    // live goes up a slice at a time: 1-20 and 21-40 are saved, 41-60 is stuck
    assert_eq!(run_until_stuck(home.path(), "live", 41), (40, 200));
    // backfill goes down, newest first: 181-200 and 161-180 are saved, 141-160 is stuck
    assert_eq!(run_until_stuck(home.path(), "backfill", 141), (40, 160));
}
