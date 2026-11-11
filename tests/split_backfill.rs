//! A big group's backfill split across two servers that number the same
//! posts differently: both servers index day chunks, every post is saved
//! exactly once, and every chunk ends done.
//!
//! One test, and the environment is set before any thread starts.

mod common;

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use common::{GROUP, Server, post_at, spawn_server};

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
            p.message_id = format!("<post{i}@split>");
            p
        })
        .collect()
}

#[test]
fn a_split_group_is_shared_by_both_servers() {
    let home = tempfile::tempdir().unwrap();
    // SAFETY: only test in this binary, set before any thread is spawned
    unsafe {
        std::env::set_var("ATLAS_HOME", home.path());
        std::env::set_var("ATLAS_NO_KEYRING", "1");
        std::env::set_var("ATLAS_SAB_DIR", home.path().join("no-sabnzbd"));
    }

    // One worker per provider leaves one frontier day for each. The first chunk
    // response waits until both have claimed work, independently of wall-clock speed.
    let gate = Arc::new(ClaimGate::default());
    let keys = [Arc::new(Mutex::new(String::new())), Arc::new(Mutex::new(String::new()))];
    let server = |offset, key: Arc<Mutex<String>>| {
        let mut state = Server::new(posts(offset));
        let (main, gate) = (home.path().join("atlas.db"), gate.clone());
        Arc::get_mut(&mut state).unwrap().before_xover = Some(Arc::new(move || {
            if gate.ready.load(Ordering::Acquire) {
                return;
            }
            let key = key.lock().unwrap().clone();
            let connection =
                rusqlite::Connection::open_with_flags(&main, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
            connection.busy_timeout(Duration::from_secs(5)).unwrap();
            let exists: bool = connection
                .query_row("select exists(select 1 from sqlite_master where name='backfill_chunks')", [], |r| r.get(0))
                .unwrap();
            if !exists {
                return;
            }
            let claimed: bool = connection
                .query_row("select exists(select 1 from backfill_chunks where state=1 and server=?1)", [&key], |r| {
                    r.get(0)
                })
                .unwrap();
            drop(connection);
            if claimed {
                gate.wait(key);
            }
        }));
        state
    };
    let a = server(1, keys[0].clone());
    let b = server(500_001, keys[1].clone());
    let (pa, pb) = (spawn_server(a), spawn_server(b));
    *keys[0].lock().unwrap() = format!("127.0.0.1:{pa}");
    *keys[1].lock().unwrap() = format!("localhost:{pb}");
    let config = serde_json::json!({
        "usenet_servers": [
            {"host": "127.0.0.1", "username": "bob", "password": "secret", "port": pa, "ssl": false, "connections": 1, "priority": 1},
            {"host": "localhost", "username": "bob", "password": "secret", "port": pb, "ssl": false, "connections": 1, "priority": 1}
        ],
        "groups": [GROUP],
        "index_mode": "backfill",
        "batch_size": 100,
        "parallel_groups": 1,
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
        let split = atlas::chunks::is_split(&conn, GROUP).unwrap_or(false);
        let (done, total) = atlas::chunks::progress(&conn, GROUP).unwrap_or((0, 0));
        if split && total > 0 && done == total && articles >= 1200 {
            break;
        }
        assert!(Instant::now() < deadline, "articles {articles}, chunks {done}/{total}, split {split}");
        std::thread::sleep(Duration::from_millis(200));
    }
    stop.store(true, Ordering::Relaxed);
    assert_eq!(runner.join().unwrap(), 0);

    assert!(!gate.timed_out.load(Ordering::Relaxed), "both providers must claim work before the response gate opens");
    assert_eq!(gate.seen.lock().unwrap().len(), 2);
    let conn = atlas::db::open_with_shards(&main).unwrap();
    assert_eq!(atlas::store::totals(&conn).unwrap().1, 1200, "every post once, overlaps deduplicated");
    let servers: i64 =
        conn.query_row("select count(distinct server) from backfill_chunks where state = 2", [], |r| r.get(0)).unwrap();
    assert_eq!(servers, 2, "both servers indexed day chunks");
}

#[derive(Default)]
struct ClaimGate {
    seen: Mutex<BTreeSet<String>>,
    changed: Condvar,
    ready: AtomicBool,
    timed_out: AtomicBool,
}

impl ClaimGate {
    fn wait(&self, key: String) {
        let mut seen = self.seen.lock().unwrap();
        seen.insert(key);
        if seen.len() == 2 {
            self.ready.store(true, Ordering::Release);
            self.changed.notify_all();
            return;
        }
        let deadline = Instant::now() + Duration::from_secs(30);
        while !self.ready.load(Ordering::Acquire) {
            let (held, waited) =
                self.changed.wait_timeout(seen, deadline.saturating_duration_since(Instant::now())).unwrap();
            seen = held;
            if waited.timed_out() && !self.ready.load(Ordering::Acquire) {
                self.timed_out.store(true, Ordering::Relaxed);
                self.ready.store(true, Ordering::Release);
                self.changed.notify_all();
            }
        }
    }
}
