//! Time cursors (`[indexing] time_cursors`): a group read from a server that hasn't been reading it (a failover, a
//! change of rank, a restart) starts there about an hour before the time it's read through on any server, found by
//! date, instead of at the newest article; and a server whose own cursor fell behind while another read the group
//! moves up to that time instead of reading it all again.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use atlas::config::IndexingConfig;
use atlas::indexer::{PassContext, PassSettings, RunStates, run_pass, shared_db};
use atlas::nntp::Pool;
use common::{GROUP, Post, Server, mock, post_at, spawn_server};

/// Posts `from..=to`, one an hour from 2026-10-20 00:00 UTC, numbered from `base + 1` on, with the same message-ids
/// on every server (another server numbers the same posts its own way).
fn hourly(from: u64, to: u64, base: u64) -> Vec<Post> {
    let start = chrono::DateTime::parse_from_rfc3339("2026-10-20T00:00:00+00:00").unwrap();
    (from..=to)
        .map(|n| {
            let when = start + chrono::Duration::hours(n as i64);
            let p = post_at(base + n, &format!(r#""rel{n}.bin" yEnc (1/1)"#), &when.to_rfc2822());
            Post { message_id: format!("<msg{n}@mock>"), ..p }
        })
        .collect()
}

fn context(pool: &Arc<Pool>) -> PassContext {
    PassContext {
        pool: pool.clone(),
        states: RunStates::default(),
        stop: Arc::new(AtomicBool::new(false)),
        verbose: false,
    }
}

fn live(fallback: bool, from_top: bool) -> PassSettings {
    PassSettings { mode: "live".into(), batch_size: 1000, request_size: 50, fallback, from_top, ..Default::default() }
}

/// Server A numbers the posts from 1, server B from 5001.
const B: u64 = 5000;

struct Setup {
    servers: [Arc<Server>; 2],
    ports: Vec<u16>,
    pool: Arc<Pool>,
    main: std::path::PathBuf,
    _dir: tempfile::TempDir,
}

fn setup(time_cursors: bool, posts: u64) -> Setup {
    let servers = [Server::new(hourly(1, posts, 0)), Server::new(hourly(1, posts, B))];
    let ports: Vec<u16> = servers.iter().map(|s| spawn_server(s.clone())).collect();
    let dir = tempfile::tempdir().unwrap();
    let main = dir.path().join("atlas.db");
    atlas::db::create_db_at(&main).unwrap();
    let pool = pool_on(&ports, time_cursors);
    Setup { servers, ports, pool, main, _dir: dir }
}

fn pool_on(ports: &[u16], time_cursors: bool) -> Arc<Pool> {
    let cfg = IndexingConfig { time_cursors, ..Default::default() };
    Arc::new(Pool::new(&[mock(ports[0], "secret", 2, 1), mock(ports[1], "secret", 2, 1)]).with_ranking(&cfg, None))
}

impl Setup {
    fn add(&self, from: u64, to: u64) {
        self.servers[0].posts.lock().unwrap().extend(hourly(from, to, 0));
        self.servers[1].posts.lock().unwrap().extend(hourly(from, to, B));
    }

    /// server `i`'s live cursor on the group
    fn cursor(&self, i: usize) -> i64 {
        let key = format!("{GROUP}@{}", self.pool.host(i).to_lowercase());
        let conn = atlas::db::open_at(&self.main).unwrap();
        atlas::db::group_progress(&conn).unwrap().into_iter().find(|p| p.key == key).unwrap().live_cursor
    }

    /// server `i`'s live cursor set back to `to`, as if it had read from there: what it began reading from is
    /// forgotten (counts as long ago)
    fn set_cursor(&self, i: usize, to: i64) {
        let key = format!("{GROUP}@{}", self.pool.host(i).to_lowercase());
        let conn = atlas::db::open_at(&self.main).unwrap();
        atlas::db::update_live_cursor(&conn, &key, to).unwrap();
        conn.execute("update groups set start_posted = null where name = ?", [&key]).unwrap();
    }

    /// server `i`'s row as if a pass cut short saved the newest slice, `article` posted at `posted`, but not the ones
    /// below it: its cursor stays where it was
    fn save_newest_slice(&self, i: usize, article: i64, posted: i64) {
        let key = format!("{GROUP}@{}", self.pool.host(i).to_lowercase());
        let conn = atlas::db::open_at(&self.main).unwrap();
        atlas::db::save_group_dates(&conn, &key, (article, posted), (article, posted)).unwrap();
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap()
}

/// A reads the group through article 100; 50 more are posted while A is down, and B, which never read it, takes
/// over. With time cursors B starts about an hour before where A is read through: none of the 50 is skipped, and
/// little is read twice. Without them B starts at its newest article and the 50 are left to A.
#[test]
fn a_server_new_to_a_group_starts_where_another_read_it_through() {
    for time_cursors in [true, false] {
        let s = setup(time_cursors, 100);
        let ctx = context(&s.pool);
        runtime().block_on(async {
            s.pool.connect().await.unwrap();
            let db = shared_db(atlas::db::open_at(&s.main).unwrap());
            // A's first pass starts at its newest article; from the start, it reads all 100
            run_pass(&ctx, &live(false, false), &db, GROUP, 0, &mut |_| {}).await.unwrap();
            s.set_cursor(0, 0);
            let a = run_pass(&ctx, &live(false, false), &db, GROUP, 0, &mut |_| {}).await.unwrap();
            assert_eq!(a.articles, 100);

            s.add(101, 150);
            s.servers[0].dropped.store(true, Ordering::SeqCst);
            let b = run_pass(&ctx, &live(true, true), &db, GROUP, 1, &mut |_| {}).await.unwrap();
            assert_eq!(s.cursor(1), (B + 150) as i64, "B read up to its newest article");
            if time_cursors {
                assert!(b.articles >= 50, "every post since A's last one is read: {}", b.articles);
                assert!(b.articles <= 80, "about an hour before where A is read through, not all 150: {}", b.articles);
            } else {
                assert_eq!(b.articles, 0, "without time cursors B starts at its newest article");
            }
        });
    }
}

/// The whole failover: A reads the group, goes down, B stands in from where A is read through while 100 more are
/// posted, and once A is back its own cursor, left behind, moves up to where B is read through. Nothing posted is
/// skipped along the way, and neither server reads again what the other read, past the margin.
#[test]
fn a_failover_mid_stream_carries_the_time_read_through_both_ways() {
    let s = setup(true, 100);
    let ctx = context(&s.pool);
    runtime().block_on(async {
        s.pool.connect().await.unwrap();
        let db = shared_db(atlas::db::open_at(&s.main).unwrap());
        run_pass(&ctx, &live(false, false), &db, GROUP, 0, &mut |_| {}).await.unwrap();
        s.set_cursor(0, 0);
        run_pass(&ctx, &live(false, false), &db, GROUP, 0, &mut |_| {}).await.unwrap();
        assert_eq!(s.cursor(0), 100);

        // A goes down; B stands in while 100 more are posted
        s.servers[0].dropped.store(true, Ordering::SeqCst);
        s.add(101, 150);
        let first = run_pass(&ctx, &live(true, true), &db, GROUP, 1, &mut |_| {}).await.unwrap();
        assert!((50..=80).contains(&first.articles), "from about an hour before 100 on: {}", first.articles);
        s.add(151, 200);
        let next = run_pass(&ctx, &live(true, false), &db, GROUP, 1, &mut |_| {}).await.unwrap();
        assert_eq!(next.articles, 50, "a stand-in's next pass reads what's new");
        assert_eq!(s.cursor(1), (B + 200) as i64);

        // A is back with its cursor at 100: it moves up to about an hour before where B is read through, rather than
        // reading 101-200 again, and reads what's new
        s.servers[0].dropped.store(false, Ordering::SeqCst);
        s.add(201, 210);
        let back = run_pass(&ctx, &live(false, false), &db, GROUP, 0, &mut |_| {}).await.unwrap();
        assert!(back.articles >= 10, "201-210 are read: {}", back.articles);
        assert!(back.articles <= 40, "not 101-200 again: {}", back.articles);
        assert_eq!(s.cursor(0), 210);

        // and a pass after that with nothing new doesnt search by date again (it would find the same place)
        let quiet = run_pass(&ctx, &live(false, false), &db, GROUP, 0, &mut |_| {}).await.unwrap();
        assert_eq!(quiet.articles, 0);
        assert_eq!(s.cursor(0), 210);
    });
}

/// Cursors saved per server before time cursors were turned on are what they go by, with no change to the
/// database: a server whose own cursor is about as far as the others reads on from it, and one left behind (the
/// group's server before) moves up to where the group is read through rather than reading it all again.
#[test]
fn cursors_saved_per_server_before_are_kept_or_moved_up() {
    let s = setup(false, 100);
    let ctx = context(&s.pool);
    runtime().block_on(async {
        s.pool.connect().await.unwrap();
        let db = shared_db(atlas::db::open_at(&s.main).unwrap());
        // without time cursors: A read through 100, B only through 40 (5040)
        for i in [0, 1] {
            run_pass(&ctx, &live(false, false), &db, GROUP, i, &mut |_| {}).await.unwrap();
        }
        s.set_cursor(0, 0);
        run_pass(&ctx, &live(false, false), &db, GROUP, 0, &mut |_| {}).await.unwrap();
        s.servers[1].posts.lock().unwrap().retain(|p| p.number <= B + 40);
        s.set_cursor(1, B as i64);
        run_pass(&ctx, &live(false, false), &db, GROUP, 1, &mut |_| {}).await.unwrap();
        s.servers[1].posts.lock().unwrap().extend(hourly(41, 100, B));
    });
    assert_eq!((s.cursor(0), s.cursor(1)), (100, (B + 40) as i64));

    // the same servers and database with time cursors on
    s.add(101, 110);
    let pool = pool_on(&s.ports, true);
    let ctx = context(&pool);
    runtime().block_on(async {
        pool.connect().await.unwrap();
        let db = shared_db(atlas::db::open_at(&s.main).unwrap());
        let a = run_pass(&ctx, &live(false, false), &db, GROUP, 0, &mut |_| {}).await.unwrap();
        assert_eq!(a.articles, 10, "A goes on from its own cursor: 101-110");
        let b = run_pass(&ctx, &live(false, false), &db, GROUP, 1, &mut |_| {}).await.unwrap();
        // A is read through about 106 now (the middle of its newest slice), soo B starts an hour before that
        assert!((1..=20).contains(&b.articles), "from about an hour before where A is, not 41-110: {}", b.articles);
        assert_eq!(s.cursor(1), (B + 110) as i64);
    });
}

/// The time an hourly post `n` is dated (see `hourly`).
fn posted(n: u64) -> i64 {
    chrono::DateTime::parse_from_rfc3339("2026-10-20T00:00:00+00:00").unwrap().timestamp() + n as i64 * 3600
}

/// A's row isn't read through (a pass cut short saved 100 but not 61-99), soo when A goes down B stands in from its
/// newest article. Once A is back its own cursor must not move up to where B is read through: B never read 61-150,
/// and nobody else did either. A goes on from its own cursor.
#[test]
fn a_home_with_a_gap_doesnt_skip_to_a_stand_in_that_started_at_the_top() {
    let s = setup(true, 60);
    let ctx = context(&s.pool);
    runtime().block_on(async {
        s.pool.connect().await.unwrap();
        let db = shared_db(atlas::db::open_at(&s.main).unwrap());
        run_pass(&ctx, &live(false, false), &db, GROUP, 0, &mut |_| {}).await.unwrap();
        s.set_cursor(0, 0);
        run_pass(&ctx, &live(false, false), &db, GROUP, 0, &mut |_| {}).await.unwrap();
        assert_eq!(s.cursor(0), 60);
        s.add(61, 100);
        s.save_newest_slice(0, 100, posted(100));

        // A goes down; B, with no row of its own and none read through to start from, starts at its newest
        s.servers[0].dropped.store(true, Ordering::SeqCst);
        s.add(101, 150);
        let first = run_pass(&ctx, &live(true, true), &db, GROUP, 1, &mut |_| {}).await.unwrap();
        assert_eq!(first.articles, 0, "B starts at its newest article");
        s.add(151, 160);
        let next = run_pass(&ctx, &live(true, false), &db, GROUP, 1, &mut |_| {}).await.unwrap();
        assert_eq!(next.articles, 10);

        // A is back: it reads on from 60, not from about where B is read through
        s.servers[0].dropped.store(false, Ordering::SeqCst);
        let back = run_pass(&ctx, &live(false, false), &db, GROUP, 0, &mut |_| {}).await.unwrap();
        assert!(back.articles >= 90, "61-150, which nobody read, are read: {}", back.articles);
        assert_eq!(s.cursor(0), 160);
    });
}

/// A's row is read through (to 60), but B, a stand-in from the top with time cursors off, began reading only after
/// 100. With time cursors on A must not move up to where B is read through: 61-100 were read by nobody.
#[test]
fn a_cursor_moves_up_only_to_a_row_that_began_reading_by_its_own_time() {
    let s = setup(false, 60);
    let ctx = context(&s.pool);
    runtime().block_on(async {
        s.pool.connect().await.unwrap();
        let db = shared_db(atlas::db::open_at(&s.main).unwrap());
        run_pass(&ctx, &live(false, false), &db, GROUP, 0, &mut |_| {}).await.unwrap();
        s.set_cursor(0, 0);
        run_pass(&ctx, &live(false, false), &db, GROUP, 0, &mut |_| {}).await.unwrap();
        assert_eq!(s.cursor(0), 60);

        s.servers[0].dropped.store(true, Ordering::SeqCst);
        s.add(61, 100);
        let first = run_pass(&ctx, &live(true, true), &db, GROUP, 1, &mut |_| {}).await.unwrap();
        assert_eq!(first.articles, 0, "without time cursors B starts at its newest article");
        s.add(101, 110);
        let next = run_pass(&ctx, &live(true, false), &db, GROUP, 1, &mut |_| {}).await.unwrap();
        assert_eq!(next.articles, 10);
        s.servers[0].dropped.store(false, Ordering::SeqCst);
    });

    let pool = pool_on(&s.ports, true);
    let ctx = context(&pool);
    runtime().block_on(async {
        pool.connect().await.unwrap();
        let db = shared_db(atlas::db::open_at(&s.main).unwrap());
        let a = run_pass(&ctx, &live(false, false), &db, GROUP, 0, &mut |_| {}).await.unwrap();
        assert_eq!(a.articles, 50, "A goes on from its own cursor: 61-110");
        assert_eq!(s.cursor(0), 110);
    });
}
