//! A split group with a third indexing server that doesnt carry it: probing
//! that server for the split and its idle worker trying day chunks must not
//! move the group off its home (one set of cursors), and every chunk is done
//! by the two servers that carry it.
//!
//! One test, and the environment is set before any thread starts.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use common::{Server, post_at, spawn_server};

/// the mock always carries common::GROUP, this one only with `any_group`.
/// its home is localhost, and GROUP on 127.1 falling over would pick
/// 127.0.0.1 (see Pool::pick_server_in and Pool::ranked), soo a probe that
/// falls over moves the group and leaves a second set of cursors
const SPLIT_GROUP: &str = "alt.binaries.split";

/// 40 posts a day for 30 days, numbered from `offset`
fn posts(offset: u64) -> Vec<common::Post> {
    let start = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00+00:00").unwrap();
    (0..1200u64)
        .map(|i| {
            let when = start + chrono::Duration::minutes(i as i64 * 36);
            let mut p = post_at(
                offset + i,
                &format!(r#""rel{}.part{}.rar" yEnc (1/1)"#, i / 10, i % 10 + 1),
                &when.to_rfc2822(),
            );
            p.message_id = format!("<post{i}@homes>");
            p
        })
        .collect()
}

#[test]
fn a_server_without_the_group_doesnt_move_it() {
    let home = tempfile::tempdir().unwrap();
    // SAFETY: only test in this binary, set before any thread is spawned
    unsafe {
        std::env::set_var("ATLAS_HOME", home.path());
        std::env::set_var("ATLAS_NO_KEYRING", "1");
        std::env::set_var("ATLAS_SAB_DIR", home.path().join("no-sabnzbd"));
    }

    let carrier = |offset| {
        let mut s = Server::new(posts(offset));
        let m = Arc::get_mut(&mut s).unwrap();
        m.any_group = true;
        m.xover_delay = Duration::from_millis(10);
        s
    };
    let (a, b) = (carrier(1), carrier(500_001));
    // answers 411 for SPLIT_GROUP
    let c = Server::new(posts(900_001));
    let (pa, pb, pc) = (spawn_server(a), spawn_server(b), spawn_server(c));
    // three host names for 127.0.0.1, soo cursors and chunk claims tell the servers apart
    let config = serde_json::json!({
        "usenet_servers": [
            {"host": "127.0.0.1", "username": "bob", "password": "secret", "port": pa, "ssl": false, "connections": 4, "priority": 1},
            {"host": "localhost", "username": "bob", "password": "secret", "port": pb, "ssl": false, "connections": 4, "priority": 1},
            {"host": "127.1", "username": "bob", "password": "secret", "port": pc, "ssl": false, "connections": 4, "priority": 1}
        ],
        "groups": [SPLIT_GROUP],
        "index_mode": "backfill",
        "batch_size": 100,
        "request_size": 50,
        "split_min_backlog": 500
    });
    std::fs::write(home.path().join("config.json"), config.to_string()).unwrap();
    atlas::db::create_db().unwrap();

    let stop = Arc::new(AtomicBool::new(false));
    let cfg = atlas::config::load_config().unwrap();
    let runner = {
        let stop = stop.clone();
        std::thread::spawn(move || atlas::bg_indexer::run_until(cfg, stop))
    };

    let main = home.path().join("atlas.db");
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let conn = atlas::db::open_with_shards(&main).unwrap();
        let (_, articles) = atlas::store::totals(&conn).unwrap();
        let (done, total) = atlas::chunks::progress(&conn, SPLIT_GROUP).unwrap_or((0, 0));
        if total > 0 && done == total && articles >= 1200 {
            break;
        }
        assert!(Instant::now() < deadline, "articles {articles}, chunks {done}/{total}");
        std::thread::sleep(Duration::from_millis(200));
    }
    stop.store(true, Ordering::Relaxed);
    assert_eq!(runner.join().unwrap(), 0);

    let conn = atlas::db::open_with_shards(&main).unwrap();
    assert_eq!(atlas::store::totals(&conn).unwrap().1, 1200, "every post once");

    let cursors: Vec<String> = conn
        .prepare("select name from groups where name like ? order by name")
        .unwrap()
        .query_map([format!("{SPLIT_GROUP}%")], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(cursors.len(), 1, "the group stayed on one home server: {cursors:?}");

    let chunk_servers: Vec<String> = conn
        .prepare("select distinct server from backfill_chunks where state = 2 order by server")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(chunk_servers, vec!["127.0.0.1".to_string(), "localhost".to_string()], "only the carriers did chunks");
}
