//! Clicking a column heading on a category of many releases, half of them incomplete under one busy poster, which is
//! what made a sort take minutes on a real database.
//!
//! One test: it points ATLAS_HOME at a temp dir before any thread is spawned. `ATLAS_SORT_MOCK_ROWS` makes more rows:
//! `ATLAS_SORT_MOCK_ROWS=400000 cargo test --release --test web_sort_speed -- --nocapture`.

mod common;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

use atlas::config::Config;
use atlas::parser::Release;
use atlas::{api, db, store};
use common::stage::article;

const KEY: &str = "sort-speed-key";

fn fetch(port: u16, path: &str, cookie: &str) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    let request = format!("GET {path} HTTP/1.1\r\nHost: atlas.test\r\nCookie: {cookie}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).unwrap();
    // the body comes chunked, and a chunk can end inside a character
    String::from_utf8_lossy(&raw).into_owned()
}

fn login(port: u16) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    let body = format!("key={KEY}");
    let request = format!(
        "POST /login HTTP/1.1\r\nHost: atlas.test\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).unwrap();
    let mut raw = String::new();
    stream.read_to_string(&mut raw).unwrap();
    let line = raw.lines().find(|l| l.to_ascii_lowercase().starts_with("set-cookie:")).unwrap();
    line["set-cookie:".len()..].trim().split(';').next().unwrap().to_string()
}

/// `count` releases over 16 groups, half of them missing a part.
fn seed(main: &std::path::Path, count: usize) {
    for batch in (0..count).collect::<Vec<_>>().chunks(5_000) {
        let releases: Vec<Release> = batch
            .iter()
            .map(|&i| {
                let name = format!("Movie.Number.{i}.2024.1080p.WEB-DL.H264-GRP");
                let file = format!("{name}.mkv");
                let parts = if i % 2 == 0 { 2 } else { 3 };
                let articles = vec![
                    article(&file, 1, parts, &format!("<{i}.1@x>")),
                    article(&file, 2, parts, &format!("<{i}.2@x>")),
                ];
                Release {
                    name,
                    articles,
                    // no two the same, and not in id order
                    size: ((i as i64).wrapping_mul(2_654_435_761) % 90_000_000_000).abs() + 1,
                    complete: parts == 2,
                    group: format!("alt.binaries.mock{}", i % 16),
                    poster: "busy poster <p@x>".into(),
                    date: format!("2026-{:02}-{:02}T00:00:00+00:00", i % 12 + 1, i % 28 + 1),
                    ..Default::default()
                }
            })
            .collect();
        store::save(main, &releases).unwrap();
    }
    for shard in store::shard_paths(main) {
        let conn = db::open_shard(&shard).unwrap();
        conn.execute(
            "insert or replace into release_category (release_id, category) select id, 2040 from releases",
            [],
        )
        .unwrap();
    }
}

fn sizes_on(page: &str) -> Vec<i64> {
    page.split("class=\"num col-size\" title=\"")
        .skip(1)
        .filter_map(|cell| cell.split(' ').next()?.replace(',', "").parse().ok())
        .collect()
}

#[test]
fn a_warm_sort_is_instant_and_reads_no_database() {
    let rows = std::env::var("ATLAS_SORT_MOCK_ROWS").ok().and_then(|v| v.parse().ok()).unwrap_or(4_000);
    let home = tempfile::tempdir().unwrap();
    // SAFETY: only test in this binary, set before any thread is spawned
    unsafe {
        std::env::set_var("ATLAS_HOME", home.path());
        std::env::set_var("ATLAS_NO_KEYRING", "1");
    }
    db::create_db().unwrap();
    let main = home.path().join("atlas.db");
    seed(&main, rows);

    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let mut config =
        Config { api_port: Some(port), api_host: "127.0.0.1".into(), api_key: Some(KEY.into()), ..Config::default() };
    assert!(api::start(&mut config));
    let cookie = login(port);

    let click = |query: &str| {
        let started = Instant::now();
        let page = fetch(port, &format!("/ui/results?{query}"), &cookie);
        (started.elapsed(), page)
    };
    let queries: Vec<String> = ["size", "posted", "name", "category", "group", "rating"]
        .iter()
        .flat_map(|by| ["asc", "desc"].map(|dir| format!("cat=2000&sort={by}&dir={dir}")))
        .collect();

    let (first, page) = click("cat=2000&sort=size&dir=asc");
    println!("first click, size asc: {first:?}");
    let sizes = sizes_on(&page);
    assert!(!sizes.is_empty(), "{page}");
    assert!(sizes.is_sorted(), "{sizes:?}");

    // the background work settles
    let deadline = Instant::now() + Duration::from_secs(600);
    let started = Instant::now();
    while queries.iter().any(|query| click(query).0 > Duration::from_millis(10)) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    println!("warm after: {:?}", started.elapsed());

    let mut slowest = Duration::ZERO;
    for query in &queries {
        let (took, page) = click(query);
        println!("{query}: {took:?}");
        slowest = slowest.max(took);
        assert!(page.contains("<table>"), "{query}: {page}");
    }
    println!("slowest warm click: {slowest:?}");
    let (next, more) = click("cat=2000&sort=size&dir=asc&offset=50");
    println!("load more: {next:?}");
    let (_, page) = click("cat=2000&sort=size&dir=asc");
    let joined: Vec<i64> = sizes_on(&page).into_iter().chain(sizes_on(&more)).collect();
    assert!(joined.is_sorted(), "the next page carries on: {joined:?}");

    // with the database out of reach the sort still answers: nothing on this path reads it
    let moved = home.path().with_extension("moved");
    std::fs::rename(home.path(), &moved).unwrap();
    let (took, page) = click("cat=2000&sort=name&dir=desc");
    std::fs::rename(&moved, home.path()).unwrap();
    assert!(page.contains("Movie.Number."), "{page}");
    assert!(took < Duration::from_millis(250), "{took:?}");

    api::stop();
}
