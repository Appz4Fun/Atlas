//! Finding the article posted at a given time by binary search over one
//! article XOVERs, with gaps in the numbering and slightly out of order dates.

mod common;

use atlas::nntp::BlockingPool;
use common::{GROUP, Server, mock, post_at, spawn_server};

/// posts 1..=1000, one per hour from 2026-01-01 00:00 UTC, with every 10th
/// number missing and number 500 dated an hour too early
fn hourly() -> Vec<common::Post> {
    let start = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00+00:00").unwrap();
    (1..=1000u64)
        .filter(|n| n % 10 != 0)
        .map(|n| {
            let hours = if n == 500 { n as i64 - 2 } else { n as i64 - 1 };
            let when = start + chrono::Duration::hours(hours);
            post_at(n, &format!(r#""p{n}.bin" yEnc (1/1)"#), &when.to_rfc2822())
        })
        .collect()
}

#[test]
fn finds_the_first_article_at_or_after_a_time() {
    let port = spawn_server(Server::new(hourly()));
    let pool = BlockingPool::new(&[mock(port, "secret", 2, 1)]);
    pool.connect().unwrap();
    let at = |rfc3339: &str| {
        let t = chrono::DateTime::parse_from_rfc3339(rfc3339).unwrap().timestamp();
        pool.block_on(pool.pool.article_at(0, GROUP, 1, 1000, t)).unwrap()
    };

    assert_eq!(at("2025-12-01T00:00:00+00:00"), 1, "before everything: the first article");
    assert_eq!(at("2026-01-01T04:00:00+00:00"), 5, "exactly on an article");
    assert_eq!(at("2026-01-01T08:30:00+00:00"), 11, "number 10 is missing, 11 is next");
    assert_eq!(at("2026-03-01T00:00:00+00:00"), 1001, "after everything: high + 1");
}

/// One day of a group indexed on one server: exactly that day's posts, the
/// chunk marked done.
#[test]
fn a_day_chunk_indexes_that_day() {
    let home = tempfile::tempdir().unwrap();
    let main = home.path().join("atlas.db");
    atlas::db::create_db_at(&main).unwrap();
    let port = spawn_server(Server::new(hourly()));
    let pool = BlockingPool::new(&[mock(port, "secret", 2, 1)]);
    pool.connect().unwrap();

    let day =
        atlas::chunks::unix_day(chrono::DateTime::parse_from_rfc3339("2026-01-02T00:00:00+00:00").unwrap().timestamp());
    let main_conn = atlas::db::open_at(&main).unwrap();
    atlas::chunks::add(&main_conn, GROUP, day, day).unwrap();

    let ctx = atlas::indexer::PassContext {
        pool: pool.pool.clone(),
        states: Default::default(),
        stop: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        verbose: false,
    };
    let db = atlas::indexer::shared_db(main_conn);
    let saved =
        pool.block_on(atlas::indexer::run_chunk(&ctx, &Default::default(), &db, GROUP, 0, day, &mut |_| {})).unwrap();

    // hours 24..47 are articles 25..48, plus an hour of overlap each side
    // (24 and 49); 30 and 40 don't exist. 24..=49 minus 2 = 24 articles
    assert_eq!(saved.articles, 24);
    let conn = atlas::db::open_at(&main).unwrap();
    assert_eq!(atlas::chunks::progress(&conn, GROUP).unwrap(), (1, 1));
}

/// posts `low..=high`, number n posted n - 1 hours after 2026-01-01 00:00 UTC,
/// minus the numbers in `gap`
fn with_gap(low: u64, high: u64, gap: std::ops::RangeInclusive<u64>) -> Vec<common::Post> {
    let start = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00+00:00").unwrap();
    (low..=high)
        .filter(|n| !gap.contains(n))
        .map(|n| {
            let when = start + chrono::Duration::hours(n as i64 - 1);
            post_at(n, &format!(r#""p{n}.bin" yEnc (1/1)"#), &when.to_rfc2822())
        })
        .collect()
}

/// unix seconds of the hour number `n` is posted at in `with_gap`
fn hour_of(n: u64) -> i64 {
    chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00+00:00").unwrap().timestamp() + (n as i64 - 1) * 3600
}

fn search(posts: Vec<common::Post>, low: u64, high: u64, when: i64) -> u64 {
    let port = spawn_server(Server::new(posts));
    let pool = BlockingPool::new(&[mock(port, "secret", 2, 1)]);
    pool.connect().unwrap();
    pool.block_on(pool.pool.article_at(0, GROUP, low, high, when)).unwrap()
}

#[test]
fn a_long_gap_at_the_midpoint_is_skipped_forward() {
    // 1..=2000 with 900..=1199 missing: the first probe (1000) lands in the gap
    let posts = || with_gap(1, 2000, 900..=1199);
    assert_eq!(search(posts(), 1, 2000, hour_of(1500)), 1500, "after the gap");
    assert_eq!(search(posts(), 1, 2000, hour_of(1200)), 1200, "the first article after the gap");
    assert_eq!(search(posts(), 1, 2000, hour_of(1000)), 1200, "inside the gap: the next article");
    assert_eq!(search(posts(), 1, 2000, hour_of(800)), 800, "before the gap");
}

#[test]
fn a_long_gap_at_the_start_settles_on_the_first_article() {
    // 1..=300 missing, the group's low says 1
    let posts = || with_gap(1, 1300, 1..=300);
    assert_eq!(search(posts(), 1, 1300, hour_of(1) - 86_400), 301, "before everything: the first article");
    assert_eq!(search(posts(), 1, 1300, hour_of(700)), 700);
}

#[test]
fn a_long_gap_at_the_end_is_high_plus_one() {
    // the group's high says 2000, nothing after 1699 exists
    let posts = || with_gap(1, 1699, 0..=0);
    assert_eq!(search(posts(), 1, 2000, hour_of(1800)), 2001, "after everything: high + 1");
    assert_eq!(search(posts(), 1, 2000, hour_of(1650)), 1650);
}

/// one post a minute: 1..=50 from 2026-01-01 00:00 UTC, nothing from 51 to
/// 3,000,000, then 3,000,001..=3,100,000 from 2026-01-03 00:00
fn holed() -> Vec<common::Post> {
    let at = |rfc3339: &str| chrono::DateTime::parse_from_rfc3339(rfc3339).unwrap();
    let (before, after) = (at("2026-01-01T00:00:00+00:00"), at("2026-01-03T00:00:00+00:00"));
    let post = |n: u64, when: chrono::DateTime<chrono::FixedOffset>| {
        post_at(n, &format!(r#""p{n}.bin" yEnc (1/1)"#), &when.to_rfc2822())
    };
    let early = (1..=50u64).map(|n| post(n, before + chrono::Duration::minutes(n as i64 - 1)));
    let late = (3_000_001..=3_100_000u64).map(|n| post(n, after + chrono::Duration::minutes((n - 3_000_001) as i64)));
    early.chain(late).collect()
}

/// unix seconds article `n` of `holed` is posted at
fn minute_of(n: u64) -> i64 {
    let at = |rfc3339: &str| chrono::DateTime::parse_from_rfc3339(rfc3339).unwrap().timestamp();
    if n <= 50 {
        at("2026-01-01T00:00:00+00:00") + (n as i64 - 1) * 60
    } else {
        at("2026-01-03T00:00:00+00:00") + (n - 3_000_001) as i64 * 60
    }
}

#[test]
fn a_hole_of_millions_is_crossed_to_its_first_article_in_few_requests() {
    let server = Server::new(holed());
    let port = spawn_server(server.clone());
    let pool = BlockingPool::new(&[mock(port, "secret", 2, 1)]);
    pool.connect().unwrap();
    let xovers = || server.xovers.load(std::sync::atomic::Ordering::SeqCst);
    let at = |low: u64, when: i64| {
        let before = xovers();
        let n = pool.block_on(pool.pool.article_at(0, GROUP, low, 3_100_000, when)).unwrap();
        let used = xovers() - before;
        assert!(used <= 150, "{used} requests to find {n}");
        n
    };
    let in_the_hole = minute_of(50) + 86_400;

    assert_eq!(at(1, minute_of(40)), 40);
    assert_eq!(at(1, in_the_hole), 3_000_001, "inside the hole: the first article after it");
    assert_eq!(at(1, minute_of(3_000_001)), 3_000_001, "the first article after the hole");
    assert_eq!(at(1, minute_of(3_000_002)), 3_000_002);
    assert_eq!(at(1, minute_of(3_050_000)), 3_050_000);
    assert_eq!(at(1, minute_of(3_100_000) + 60), 3_100_001, "after everything: high + 1");

    // a low watermark that still says 1,500,000 though nothing is there
    assert_eq!(at(1_500_000, minute_of(40)), 3_000_001, "everything from low on is newer");
    assert_eq!(at(1_500_000, in_the_hole), 3_000_001);
    assert_eq!(at(1_500_000, minute_of(3_000_001)), 3_000_001);
}

/// The first day after a hole of millions is indexed whole.
#[test]
fn a_day_chunk_right_after_a_hole_of_millions_gets_every_article() {
    let home = tempfile::tempdir().unwrap();
    let main = home.path().join("atlas.db");
    atlas::db::create_db_at(&main).unwrap();
    let port = spawn_server(Server::new(holed()));
    let pool = BlockingPool::new(&[mock(port, "secret", 2, 1)]);
    pool.connect().unwrap();

    let day = atlas::chunks::unix_day(minute_of(3_000_001));
    let main_conn = atlas::db::open_at(&main).unwrap();
    atlas::chunks::add(&main_conn, GROUP, day, day).unwrap();
    let ctx = atlas::indexer::PassContext {
        pool: pool.pool.clone(),
        states: Default::default(),
        stop: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        verbose: false,
    };
    let db = atlas::indexer::shared_db(main_conn);
    let saved =
        pool.block_on(atlas::indexer::run_chunk(&ctx, &Default::default(), &db, GROUP, 0, day, &mut |_| {})).unwrap();

    // the day's 1,440 posts and the hour after it; the hour before is the hole
    assert_eq!(saved.articles, 1_440 + 60);
    let conn = atlas::db::open_at(&main).unwrap();
    assert_eq!(atlas::chunks::progress(&conn, GROUP).unwrap(), (1, 1));
}

/// A day chunk whose day has 150 numbers missing in the middle still fetches
/// every article of that day.
#[test]
fn a_day_chunk_skips_a_gap_inside_the_day() {
    let home = tempfile::tempdir().unwrap();
    let main = home.path().join("atlas.db");
    atlas::db::create_db_at(&main).unwrap();

    // one post every 2 minutes from 2026-01-01: day 1 is 1..=720, day 2 is
    // 721..=1440, and 1050..=1199 are missing right where the search for the
    // day's end first probes
    let start = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00+00:00").unwrap();
    let posts: Vec<common::Post> = (1..=1440u64)
        .filter(|n| !(1050..=1199).contains(n))
        .map(|n| {
            let when = start + chrono::Duration::minutes((n as i64 - 1) * 2);
            post_at(n, &format!(r#""p{n}.bin" yEnc (1/1)"#), &when.to_rfc2822())
        })
        .collect();
    let port = spawn_server(Server::new(posts));
    let pool = BlockingPool::new(&[mock(port, "secret", 2, 1)]);
    pool.connect().unwrap();

    let day =
        atlas::chunks::unix_day(chrono::DateTime::parse_from_rfc3339("2026-01-02T00:00:00+00:00").unwrap().timestamp());
    let main_conn = atlas::db::open_at(&main).unwrap();
    atlas::chunks::add(&main_conn, GROUP, day, day).unwrap();

    let ctx = atlas::indexer::PassContext {
        pool: pool.pool.clone(),
        states: Default::default(),
        stop: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        verbose: false,
    };
    let db = atlas::indexer::shared_db(main_conn);
    let saved =
        pool.block_on(atlas::indexer::run_chunk(&ctx, &Default::default(), &db, GROUP, 0, day, &mut |_| {})).unwrap();

    // day 2 is 720 numbers minus the 150 missing, plus the hour before it
    // (691..=720, 30 posts); nothing comes after 1440
    assert_eq!(saved.articles, 720 - 150 + 30);
    let conn = atlas::db::open_at(&main).unwrap();
    assert_eq!(atlas::chunks::progress(&conn, GROUP).unwrap(), (1, 1));
}

/// A chunk that fails and then cant be given back still reports why it
/// failed, not the failure to give it back.
#[test]
fn a_failing_chunk_reports_its_own_error() {
    let home = tempfile::tempdir().unwrap();
    let main = home.path().join("atlas.db");
    atlas::db::create_db_at(&main).unwrap();
    let port = spawn_server(Server::new(hourly()));
    let pool = BlockingPool::new(&[mock(port, "secret", 2, 1)]);
    pool.connect().unwrap();

    // giving the chunk back fails: there is no chunk table
    let main_conn = atlas::db::open_at(&main).unwrap();
    main_conn.execute_batch("drop table backfill_chunks").unwrap();
    let ctx = atlas::indexer::PassContext {
        pool: pool.pool.clone(),
        states: Default::default(),
        stop: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        verbose: false,
    };
    let db = atlas::indexer::shared_db(main_conn);
    // the mock doesnt carry this group: GROUP answers 411
    let err = pool
        .block_on(atlas::indexer::run_chunk(
            &ctx,
            &Default::default(),
            &db,
            "alt.binaries.other",
            0,
            20_455,
            &mut |_| {},
        ))
        .unwrap_err();
    assert!(err.downcast_ref::<atlas::indexer::NotCarried>().is_some(), "{err:#}");
}

/// A day before anything the server keeps goes back unfinished (another
/// server may have it); an empty day the server does keep is finished.
#[test]
fn a_day_older_than_the_server_keeps_is_given_back() {
    let home = tempfile::tempdir().unwrap();
    let main = home.path().join("atlas.db");
    atlas::db::create_db_at(&main).unwrap();
    // posts every hour of 2026-01-01 and of 2026-01-03, none on 2026-01-02
    let start = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00+00:00").unwrap();
    let posts = (0..72u64)
        .filter(|h| !(24..48).contains(h))
        .map(|h| {
            post_at(
                h + 1,
                &format!(r#""p{h}.bin" yEnc (1/1)"#),
                &(start + chrono::Duration::hours(h as i64)).to_rfc2822(),
            )
        })
        .collect();
    let port = spawn_server(Server::new(posts));
    let pool = BlockingPool::new(&[mock(port, "secret", 2, 1)]);
    pool.connect().unwrap();

    let first_day = atlas::chunks::unix_day(start.timestamp());
    let main_conn = atlas::db::open_at(&main).unwrap();
    atlas::chunks::add(&main_conn, GROUP, first_day + 2, first_day - 1).unwrap();
    let ctx = atlas::indexer::PassContext {
        pool: pool.pool.clone(),
        states: Default::default(),
        stop: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        verbose: false,
    };
    let db = atlas::indexer::shared_db(main_conn);
    let run =
        |day: i64| pool.block_on(atlas::indexer::run_chunk(&ctx, &Default::default(), &db, GROUP, 0, day, &mut |_| {}));

    let err = run(first_day - 1).unwrap_err();
    let too_old = err.downcast_ref::<atlas::indexer::TooOld>().expect("a TooOld");
    assert_eq!(too_old.oldest_day, first_day);
    assert_eq!(ctx.states.keeps_from(GROUP, 0), first_day, "remembered for the next claims");
    let empty = run(first_day + 1).unwrap();
    assert_eq!(empty.articles, 2, "only the overlap hours");
    let conn = atlas::db::open_at(&main).unwrap();
    let state = |day: i64| -> i64 {
        conn.query_row("select state from backfill_chunks where day = ?", [day], |r| r.get(0)).unwrap()
    };
    assert_eq!(state(first_day - 1), 0, "given back, still pending");
    assert_eq!(state(first_day + 1), 2, "an empty day it keeps is done");
}
