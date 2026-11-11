//! A split group with a third indexing server that doesnt carry it: probing
//! that server for the split and its idle worker trying day chunks must not
//! move the group off its home (one set of cursors), and every chunk is done
//! by the two servers that carry it.
//!
//! One test, and the environment is set before any thread starts.

mod common;

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use common::{Server, post_at, spawn_server};

/// the mock always carries common::GROUP, this one only with `any_group`.
/// its home is localhost, and GROUP on the third server falling over would
/// pick 127.0.0.1 (see Pool::pick_server_in and Pool::ranked), soo a probe
/// that falls over moves the group and leaves a second set of cursors. The
/// third server resting as down mustnt move it either (with all three up it
/// hashes to localhost, over the other two alone to 127.0.0.1)
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

    // Keep the original connection weights (which choose the group's home),
    // but one frontier worker per provider. Hold the first claimed responses
    // until both carriers have real claims, independently of mock response speed.
    let gate = Arc::new(ClaimGate::default());
    let carrier_keys = [Arc::new(Mutex::new(String::new())), Arc::new(Mutex::new(String::new()))];
    let carrier = |offset, key: Arc<Mutex<String>>| {
        let mut state = Server::new(posts(offset));
        let (main, gate) = (home.path().join("atlas.db"), gate.clone());
        let server = Arc::get_mut(&mut state).unwrap();
        server.any_group = true;
        server.before_xover = Some(Arc::new(move || {
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
    let (a, b) = (carrier(1, carrier_keys[0].clone()), carrier(500_001, carrier_keys[1].clone()));
    // answers 411 for SPLIT_GROUP
    let c = Server::new(posts(900_001));
    let (pa, pb, pc) = (spawn_server(a), spawn_server(b), spawn_server(c.clone()));
    // three host names for the loopback, soo cursors and chunk claims tell the
    // servers apart. the third has to reach its mock (and answer 411) on every
    // os, and 127.1 isnt a name every resolver takes (Windows CI couldnt reach it)
    let third = if common::has_ipv6_loopback() { "[::1]" } else { "127.1" };
    let config = serde_json::json!({
        "usenet_servers": [
            {"host": "127.0.0.1", "username": "bob", "password": "secret", "port": pa, "ssl": false, "connections": 4, "priority": 1},
            {"host": "localhost", "username": "bob", "password": "secret", "port": pb, "ssl": false, "connections": 4, "priority": 1},
            {"host": third, "username": "bob", "password": "secret", "port": pc, "ssl": false, "connections": 4, "priority": 1}
        ],
        "groups": [SPLIT_GROUP],
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
    assert_eq!(cfg.workers_per_server(&[4, 4, 4]), [1, 1, 1], "one frontier worker per provider");
    assert_eq!(atlas::nntp::Pool::new(&cfg.servers).pick_server(SPLIT_GROUP), 1, "the fixture starts on localhost");
    let keys = atlas::nntp::server_keys(&cfg.servers);
    for (slot, key) in carrier_keys.iter().zip(&keys[..2]) {
        *slot.lock().unwrap() = key.clone();
    }
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

    assert!(!gate.timed_out.load(Ordering::Relaxed), "both carriers must claim before the response gate opens");
    assert_eq!(*gate.seen.lock().unwrap(), keys[..2].iter().cloned().collect::<BTreeSet<_>>());
    let conn = atlas::db::open_with_shards(&main).unwrap();
    assert_eq!(atlas::store::totals(&conn).unwrap().1, 1200, "every post once");

    // (cursor, whether a normal pass ran on it): the home's passes save the
    // server's bounds, a carrier's sweep (its own cursor, see run_sweep) doesnt
    let cursors: Vec<(String, bool)> = conn
        .prepare("select name, last_article is not null from groups where name like ? order by name")
        .unwrap()
        .query_map([format!("{SPLIT_GROUP}%")], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let homes = cursors.iter().filter(|(_, home)| *home).count();
    assert_eq!(homes, 1, "the group stayed on one home server: {cursors:?}");
    let carriers = [format!("{SPLIT_GROUP}@{}", keys[0]), format!("{SPLIT_GROUP}@{}", keys[1])];
    assert!(cursors.iter().all(|(name, _)| carriers.contains(name)), "only carriers have cursors: {cursors:?}");

    let chunk_servers: Vec<String> = conn
        .prepare("select distinct server from backfill_chunks where state = 2 order by server")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(c.accepted.load(Ordering::SeqCst) > 0, "the third server was asked");
    assert_eq!(chunk_servers, keys[..2].to_vec(), "only the carriers did chunks");
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
