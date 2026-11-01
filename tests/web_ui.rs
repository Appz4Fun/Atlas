//! The web ui over a real socket: sign in, search, filter, download.
//!
//! One test: it points ATLAS_HOME at a temp dir before any thread is spawned.

mod common;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};

use atlas::config::Config;
use atlas::parser::Release;
use atlas::{api, db, store};
use common::stage::release_of;

const KEY: &str = "web-ui-key";

struct Reply {
    head: String,
    body: String,
}

impl Reply {
    fn status(&self) -> u16 {
        self.head.split(' ').nth(1).unwrap().parse().unwrap()
    }

    fn header(&self, name: &str) -> Option<String> {
        let prefix = format!("{}:", name.to_ascii_lowercase());
        self.head
            .lines()
            .find(|l| l.to_ascii_lowercase().starts_with(&prefix))
            .map(|l| l[prefix.len()..].trim().to_string())
    }
}

fn send(port: u16, request: &str) -> Reply {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream.write_all(request.as_bytes()).unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).unwrap();
    let raw = String::from_utf8(raw).unwrap();
    let (head, body) = raw.split_once("\r\n\r\n").unwrap();
    Reply { head: head.into(), body: body.into() }
}

fn fetch(port: u16, path: &str, cookie: &str) -> Reply {
    send(port, &format!("GET {path} HTTP/1.1\r\nHost: atlas.test\r\nCookie: {cookie}\r\nConnection: close\r\n\r\n"))
}

fn post(port: u16, path: &str, body: &str) -> Reply {
    send(
        port,
        &format!(
            "POST {path} HTTP/1.1\r\nHost: atlas.test\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        ),
    )
}

#[test]
fn the_web_ui_end_to_end() {
    let home = tempfile::tempdir().unwrap();
    // SAFETY: only test in this binary, set before any thread is spawned
    unsafe {
        std::env::set_var("ATLAS_HOME", home.path());
        std::env::set_var("ATLAS_NO_KEYRING", "1");
    }
    db::create_db().unwrap();
    let main = home.path().join("atlas.db");
    for (name, group) in
        [("Tv.Findword.S01E01-GRP", "alt.binaries.g1"), ("<script>alert(1)</script>", "alt.binaries.g2")]
    {
        store::save(&main, &[Release { group: group.into(), ..release_of(name, "x.bin", 2, 2) }]).unwrap();
    }
    let shard = db::open_shard(&store::shard_path(&main, store::shard_of("alt.binaries.g1"))).unwrap();
    let id: i64 =
        shard.query_row("select id from releases where name = 'Tv.Findword.S01E01-GRP'", [], |r| r.get(0)).unwrap();
    shard
        .execute("insert into release_category (release_id, category) values (?, 5040)", rusqlite::params![id])
        .unwrap();

    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let mut config =
        Config { api_port: Some(port), api_host: "127.0.0.1".into(), api_key: Some(KEY.into()), ..Config::default() };
    assert!(api::start(&mut config));

    // nothing but the sign in page and the assets without signing in
    let root = fetch(port, "/", "");
    assert_eq!(root.status(), 200);
    assert!(root.body.contains("action=\"/login\""));
    assert!(!root.body.contains("Findword"));
    assert_eq!(fetch(port, "/ui/results?q=Findword", "").status(), 401);
    assert_eq!(fetch(port, &format!("/ui/nzb?id={id}"), "").status(), 303);
    assert_eq!(fetch(port, "/static/htmx.min.js", "").status(), 200);
    assert_eq!(post(port, "/login", "key=wrong").status(), 401);
    assert_eq!(post(port, "/api", "t=search").status(), 405);

    let login = post(port, "/login", &format!("key={KEY}"));
    assert_eq!(login.status(), 303);
    let cookie = login.header("Set-Cookie").unwrap();
    assert!(!cookie.contains(KEY), "{cookie}");
    let cookie = cookie.split(';').next().unwrap().to_string();

    // the full page, and never the key
    let page = fetch(port, "/", &cookie);
    assert!(page.body.contains("Findword"), "{}", page.body);
    assert!(page.body.contains("TV/HD"), "{}", page.body);
    assert!(!page.body.contains(KEY));
    assert!(page.body.contains("&lt;script&gt;alert(1)"), "{}", page.body);
    assert!(!page.body.contains("<script>alert"), "{}", page.body);
    assert!(page.header("Content-Security-Policy").unwrap().contains("default-src 'self'"));

    // what htmx swaps in
    let found = fetch(port, "/ui/results?q=Findword&cat=", &cookie);
    assert_eq!(found.status(), 200);
    assert!(found.body.contains("<table>") && found.body.contains("Findword"), "{}", found.body);
    assert!(found.body.contains(r#"id="categories""#) && found.body.contains("hx-swap-oob"), "{}", found.body);
    assert!(!found.body.contains("alert(1)"), "{}", found.body);
    assert_eq!(found.header("HX-Push-Url").as_deref(), Some("/?q=Findword"));

    assert!(found.body.contains("class=\"chip\""), "{}", found.body);
    let filtered = fetch(port, "/ui/results?q=Findword&filters=2160p", &cookie);
    assert!(!filtered.body.contains("Tv.Findword"), "not 2160p: {}", filtered.body);
    assert_eq!(
        fetch(port, "/ui/results?q=Findword&filters=2160p", &cookie).header("HX-Push-Url").as_deref(),
        Some("/?q=Findword&filters=2160p")
    );
    assert!(
        fetch(port, &format!("/api?t=search&q=Findword&apikey={KEY}"), "")
            .body
            .contains(r#"name="group" value="alt.binaries.g1""#)
    );

    let by_cat = fetch(port, "/ui/results?cat=5000", &cookie);
    assert!(by_cat.body.contains("Findword") && !by_cat.body.contains("alert(1)"), "{}", by_cat.body);
    assert!(fetch(port, "/ui/results?q=nothinglikethis", &cookie).body.contains("nothing matches"));
    assert!(fetch(port, "/ui/results?cat=2040", &cookie).body.contains("nothing is in Movies / HD yet"));
    assert!(fetch(port, "/?q=Findword", &cookie).body.contains(r#"value="Findword""#));

    let release = fetch(port, &format!("/ui/release?id={id}"), &cookie);
    assert_eq!(release.status(), 200);
    assert!(
        release.body.contains("Tv.Findword.S01E01-GRP") && release.body.contains("Download NZB"),
        "{}",
        release.body
    );
    assert_eq!(fetch(port, "/ui/release?id=999999999", &cookie).status(), 404);
    assert_eq!(fetch(port, &format!("/ui/release?id={id}"), "").status(), 303);

    let nzb = fetch(port, &format!("/ui/nzb?id={id}"), &cookie);
    assert_eq!(nzb.status(), 200);
    assert!(nzb.header("Content-Type").unwrap().contains("x-nzb"));
    assert!(nzb.header("Content-Disposition").unwrap().contains("attachment"));
    assert!(nzb.body.contains("<nzb"), "{}", nzb.body);

    // the api is as it was
    assert_eq!(fetch(port, "/api?t=search", &cookie).status(), 401, "the cookie is not an api key");
    assert!(fetch(port, &format!("/api?t=search&q=Findword&apikey={KEY}"), "").body.contains("Findword"));

    let out = post(port, "/logout", "");
    assert_eq!(out.status(), 303);
    assert!(out.header("Set-Cookie").unwrap().contains("Max-Age=0"));

    api::stop();
}
