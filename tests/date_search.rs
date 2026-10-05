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
