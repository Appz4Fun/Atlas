//! Frozen releases in the web ui: a deep search lists them, their page downloads and thaws them.
//!
//! One test: it points ATLAS_HOME at a temp dir before any thread is spawned.

mod common;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

use atlas::config::Config;
use atlas::parser::Release;
use atlas::{api, db, frozen, store};
use common::stage::release_of;

const KEY: &str = "web-frozen-key";
const GROUP: &str = "alt.binaries.g1";

fn send(port: u16, request: &str) -> (u16, String, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream.write_all(request.as_bytes()).unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).unwrap();
    let raw = String::from_utf8_lossy(&raw).into_owned();
    let (head, body) = raw.split_once("\r\n\r\n").unwrap();
    (head.split(' ').nth(1).unwrap().parse().unwrap(), head.into(), body.into())
}

fn fetch(port: u16, path: &str, cookie: &str) -> (u16, String, String) {
    send(port, &format!("GET {path} HTTP/1.1\r\nHost: atlas.test\r\nCookie: {cookie}\r\nConnection: close\r\n\r\n"))
}

fn post(port: u16, path: &str, body: &str, cookie: &str) -> (u16, String, String) {
    send(
        port,
        &format!(
            "POST {path} HTTP/1.1\r\nHost: atlas.test\r\nCookie: {cookie}\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        ),
    )
}

#[test]
fn a_frozen_release_is_found_downloaded_and_thawed_from_the_web_ui() {
    let home = tempfile::tempdir().unwrap();
    // SAFETY: only test in this binary, set before any thread is spawned
    unsafe {
        std::env::set_var("ATLAS_HOME", home.path());
        std::env::set_var("ATLAS_NO_KEYRING", "1");
    }
    db::create_db().unwrap();
    let main = home.path().join("atlas.db");
    for name in ["Some.Album.Coldword-GRP", "Live.Coldword.Show.S01E01-GRP"] {
        store::save(&main, &[Release { group: GROUP.into(), ..release_of(name, "x.bin", 2, 2) }]).unwrap();
    }
    let path = store::shard_path(&main, store::shard_of(GROUP));
    let shard = db::open_shard(&path).unwrap();
    frozen::attach(&shard, &path).unwrap();
    let id = |name: &str| -> i64 {
        shard.query_row("select id from releases where name = ?1", [name], |r| r.get(0)).unwrap()
    };
    let (album, show) = (id("Some.Album.Coldword-GRP"), id("Live.Coldword.Show.S01E01-GRP"));
    shard.execute("insert into release_category (release_id, category) values (?1, 3010)", [album]).unwrap();
    shard.execute("insert into release_category (release_id, category) values (?1, 5040)", [show]).unwrap();
    assert!(frozen::rules::evaluate(&shard, album, 1).unwrap(), "music: not movies or TV");
    assert!(!frozen::rules::evaluate(&shard, show, 1).unwrap());
    // an album still missing files: the rule takes it all the same
    shard.execute("update releases set complete = 0 where id = ?1", [album]).unwrap();
    assert_eq!(frozen::freeze_some(&shard, 10, Duration::from_secs(60), i64::MAX, 1).unwrap().frozen, 1);
    drop(shard);

    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let mut config =
        Config { api_port: Some(port), api_host: "127.0.0.1".into(), api_key: Some(KEY.into()), ..Config::default() };
    assert!(api::start(&mut config));
    let (status, head, _) = post(port, "/login", &format!("key={KEY}"), "");
    assert_eq!(status, 303);
    let cookie = head.lines().find(|l| l.to_ascii_lowercase().starts_with("set-cookie:")).unwrap();
    let cookie = cookie["set-cookie:".len()..].trim().split(';').next().unwrap().to_string();

    // the default search doesn't see it, a deep one lists it after the live ones
    let (_, _, plain) = fetch(port, "/ui/results?q=Coldword", &cookie);
    assert!(plain.contains("Live.Coldword.Show") && !plain.contains("Some.Album"), "{plain}");
    assert!(!plain.contains("Frozen releases"), "{plain}");
    assert!(plain.contains("name=\"deep\""), "the box to ask for them: {plain}");
    let (_, head, deep) = fetch(port, "/ui/results?q=Coldword&deep=1", &cookie);
    assert!(deep.contains("Frozen releases") && deep.contains("Some.Album.Coldword-GRP"), "{deep}");
    assert!(deep.find("Live.Coldword.Show").unwrap() < deep.find("Some.Album").unwrap(), "after the live ones");
    assert!(deep.contains(&format!("/ui/release?id={album}")), "{deep}");
    assert!(head.contains("deep=1"), "the address keeps it: {head}");
    let (_, _, other) = fetch(port, "/ui/results?q=Coldword&deep=1&cat=5000", &cookie);
    assert!(!other.contains("Some.Album"), "the category filters frozen ones too: {other}");
    // and so do the fields, the tags and "Hide incomplete"
    let lists = |query: &str| fetch(port, &format!("/ui/results?deep=1&{query}"), &cookie).2.contains("Some.Album");
    assert!(lists(&format!("group={GROUP}")) && !lists("group=alt.binaries.other"), "by group");
    assert!(lists("q=Coldword&team=GRP") && !lists("q=Coldword&team=NOPE"), "by release group");
    assert!(!lists("q=Coldword&filters=2160p"), "by tag");
    assert!(!lists("q=Coldword&incomplete=hide"), "hide incomplete");
    let (_, _, crc) = fetch(port, "/ui/results?q=Coldword&deep=1&crc=deadbeef", &cookie);
    assert!(!crc.contains("Some.Album") && crc.contains("Not searched"), "probe fields can't reach them: {crc}");

    // its page: what it is, its NZB, a thaw
    let (status, _, page) = fetch(port, &format!("/ui/release?id={album}"), &cookie);
    assert_eq!(status, 200);
    assert!(page.contains("FROZEN") && page.contains("Audio") && page.contains("action=\"/ui/thaw\""), "{page}");
    let (status, _, nzb) = fetch(port, &format!("/ui/nzb?id={album}"), &cookie);
    assert_eq!(status, 200);
    assert!(nzb.contains("<nzb") && nzb.contains("Some.Album.Coldword-GRP.x.bin.1@x"), "{nzb}");

    // not without signing in
    assert_eq!(post(port, "/ui/thaw", &format!("id={album}"), "").0, 303);
    assert!(fetch(port, "/ui/results?q=Coldword", &cookie).2.contains("Live.Coldword"));
    let (status, head, _) = post(port, "/ui/thaw", &format!("id={album}"), &cookie);
    assert_eq!(status, 303);
    assert!(head.contains(&format!("/ui/release?id={album}")), "{head}");
    let (_, _, back) = fetch(port, "/ui/results?q=Coldword", &cookie);
    assert!(back.contains("Some.Album.Coldword-GRP"), "live again: {back}");
    let (_, _, page) = fetch(port, &format!("/ui/release?id={album}"), &cookie);
    assert!(!page.contains("action=\"/ui/thaw\""), "{page}");
    assert_eq!(post(port, "/ui/thaw", &format!("id={album}"), &cookie).0, 404, "not frozen any more");
}
