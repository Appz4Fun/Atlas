//! The Dates of a release's articles, read from one server's overview between the release's first and last Date.

use atlas as atlas_api;
#[path = "common/nntp.rs"]
mod common;

use std::collections::HashSet;

use atlas::nntp::BlockingPool;
use common::{GROUP, Server, mock, post_at, spawn_server};

/// posts 1..=300, one a minute from 2026-10-26 04:00 UTC
fn minutely() -> Vec<common::Post> {
    let start = chrono::DateTime::parse_from_rfc3339("2026-10-26T04:00:00+00:00").unwrap();
    (1..=300u64)
        .map(|n| {
            let when = start + chrono::Duration::minutes(n as i64 - 1);
            post_at(n, &format!(r#""p{n}.bin" yEnc (1/1)"#), &when.to_rfc2822())
        })
        .collect()
}

#[test]
fn the_dates_of_a_releases_articles_come_from_one_servers_overview() {
    let port = spawn_server(Server::new(minutely()));
    let pool = BlockingPool::new(&[mock(port, "secret", 2, 1)]);
    pool.connect().unwrap();
    let t = |s: &str| chrono::DateTime::parse_from_rfc3339(s).unwrap().timestamp();
    // the release's articles: numbers 100, 101 and 250, asked for with and without angle brackets, and one the
    // server doesnt have
    let ids: HashSet<String> =
        ["<msg100@mock>", "msg101@mock", "<msg250@mock>", "<gone@mock>"].iter().map(|s| s.to_string()).collect();
    let span = (t("2026-10-26T05:39:00+00:00"), t("2026-10-26T08:09:00+00:00"));
    let found = pool.block_on(atlas::repost_split::dates_on(&pool.pool, 0, GROUP, span, &ids, 1_000)).unwrap().unwrap();
    assert_eq!(found.len(), 3, "{found:?}");
    assert_eq!(found.get("msg100@mock"), Some(&t("2026-10-26T05:39:00+00:00")));
    assert_eq!(found.get("msg101@mock"), Some(&t("2026-10-26T05:40:00+00:00")));
    assert_eq!(found.get("msg250@mock"), Some(&t("2026-10-26T08:09:00+00:00")));

    // a range past the cap isnt read
    let capped = pool.block_on(atlas::repost_split::dates_on(&pool.pool, 0, GROUP, span, &ids, 50)).unwrap();
    assert_eq!(capped, None);
}
