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
