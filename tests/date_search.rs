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

#[test]
fn a_hole_of_millions_is_crossed_in_few_requests() {
    let server = Server::new(with_gap(1, 3_000_050, 51..=3_000_000));
    let port = spawn_server(server.clone());
    let pool = BlockingPool::new(&[mock(port, "secret", 2, 1)]);
    pool.connect().unwrap();
    let at = |n: u64| pool.block_on(pool.pool.article_at(0, GROUP, 1, 3_000_050, hour_of(n))).unwrap();

    assert_eq!(at(40), 40);
    assert_eq!(at(60), 3_000_001, "inside the hole: the first article after it");
    assert_eq!(at(3_000_020), 3_000_020);
    let xovers = server.xovers.load(std::sync::atomic::Ordering::SeqCst);
    assert!(xovers < 400, "{xovers} requests for three searches");
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
