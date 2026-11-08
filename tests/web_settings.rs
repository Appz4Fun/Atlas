//! The settings pages over a real socket: sign in, change servers, indexing and the api address, and read what
//! landed in config.toml.
//!
//! One test: it points ATLAS_HOME at a temp dir before any thread is spawned.

mod common;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

use atlas::config::{self, Config};
use atlas::{api, db};

const KEY: &str = "web-settings-key";

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

fn send(port: u16, request: &str) -> Option<Reply> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).ok()?;
    stream.write_all(request.as_bytes()).ok()?;
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).ok()?;
    let raw = String::from_utf8_lossy(&raw).into_owned();
    let (head, body) = raw.split_once("\r\n\r\n")?;
    Some(Reply { head: head.into(), body: body.into() })
}

fn fetch(port: u16, path: &str, cookie: &str) -> Reply {
    send(port, &format!("GET {path} HTTP/1.1\r\nHost: atlas.test\r\nCookie: {cookie}\r\nConnection: close\r\n\r\n"))
        .unwrap()
}

/// A form post, with the cookie and any extra header lines.
fn post(port: u16, path: &str, cookie: &str, extra: &str, body: &str) -> Reply {
    send(
        port,
        &format!(
            "POST {path} HTTP/1.1\r\nHost: atlas.test\r\nCookie: {cookie}\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\n{extra}Connection: close\r\n\r\n{body}",
            body.len()
        ),
    )
    .unwrap()
}

fn csrf_of(page: &str) -> String {
    let at = page.find("name=\"_csrf\" value=\"").expect("a token on the page") + "name=\"_csrf\" value=\"".len();
    page[at..at + 64].to_string()
}

/// config.toml as atlas reads it back
fn saved() -> Config {
    config::load_config().expect("config.toml is there")
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

#[test]
fn the_settings_pages_end_to_end() {
    let home = tempfile::tempdir().unwrap();
    // SAFETY: only test in this binary, set before any thread is spawned
    unsafe {
        std::env::set_var("ATLAS_HOME", home.path());
        std::env::set_var("ATLAS_NO_KEYRING", "1");
        std::env::remove_var("ATLAS_MANAGED_KEYS");
    }
    db::create_db().unwrap();
    let news_port = common::spawn_server(common::Server::new(vec![common::post(2, "test", 1, Vec::new())]));
    let port = free_port();
    std::fs::write(
        home.path().join("config.toml"),
        format!(
            "api_key = \"{KEY}\"\napi_host = \"127.0.0.1\"\napi_port = {port}\ngroup = \"alt.binaries.a\"\ngroups = [\"alt.binaries.a\"]\nindex_mode = \"dynamic\"\n\n[[usenet_servers]]\nhost = \"127.0.0.1\"\nusername = \"bob\"\npassword = \"secret\"\nport = {news_port}\nssl = false\nconnections = 8\npriority = 1\n"
        ),
    )
    .unwrap();
    let mut config = saved();
    assert!(api::start(&mut config));

    // signed out: the page redirects and a post is turned away before it is read
    assert_eq!(fetch(port, "/ui/settings", "").status(), 303);
    assert_eq!(post(port, "/ui/settings/servers", "", "", "do=remove&i=0&was=127.0.0.1").status(), 303);
    assert_eq!(saved().servers.len(), 1);

    let login = post(port, "/login", "", "", &format!("key={KEY}"));
    assert_eq!(login.status(), 303);
    let cookie = login.header("Set-Cookie").unwrap().split(';').next().unwrap().to_string();

    // the page: every server, never a secret
    let page = fetch(port, "/ui/settings", &cookie);
    assert_eq!(page.status(), 200);
    assert!(page.body.contains("127.0.0.1") && page.body.contains("password set"), "{}", page.body);
    assert!(!page.body.contains("value=\"secret\"") && !page.body.contains(KEY), "a secret is on the page");
    assert!(page.body.contains("class=\"site-footer\""));
    let token = csrf_of(&page.body);

    // without the token nothing changes; with it in the form or the header it does
    let stale = post(port, "/ui/settings/servers", &cookie, "", "do=remove&i=0&was=127.0.0.1");
    assert_eq!(stale.status(), 403);
    assert!(stale.body.contains("that form is stale"), "{}", stale.body);
    assert_eq!(saved().servers.len(), 1);

    // groups: provider search uses LIST, a result can be added, and the last group cannot be removed
    let found = fetch(port, "/ui/settings/groups/search?q=bin", &cookie);
    assert_eq!(found.status(), 200);
    assert!(found.body.contains(common::GROUP), "{}", found.body);
    let group_added =
        post(port, "/ui/settings/groups", &cookie, "", &format!("_csrf={token}&do=add&group={}", common::GROUP));
    assert!(group_added.body.contains("group alt.binaries.test added"), "{}", group_added.body);
    assert_eq!(saved().group, common::GROUP);
    let group_removed =
        post(port, "/ui/settings/groups", &cookie, "", &format!("_csrf={token}&do=remove&group={}", common::GROUP));
    assert!(group_removed.body.contains("group alt.binaries.test removed"), "{}", group_removed.body);
    assert_eq!(saved().group, "alt.binaries.a");

    // provider keys are accepted but never echoed; blank keeps and clear removes
    let keys = post(
        port,
        "/ui/settings/keys",
        &cookie,
        "",
        &format!("_csrf={token}&tmdb_api_key=tmdb-secret&tvdb_pin=tvdb-secret"),
    );
    assert!(!keys.body.contains("tmdb-secret") && !keys.body.contains("tvdb-secret"), "a key is on the page");
    assert_eq!(saved().media.tmdb.api_key, "tmdb-secret");
    let kept = post(port, "/ui/settings/keys", &cookie, "", &format!("_csrf={token}"));
    assert!(kept.body.contains("Nothing changed"), "{}", kept.body);
    assert_eq!(saved().media.tmdb.api_key, "tmdb-secret");
    post(port, "/ui/settings/keys", &cookie, "", &format!("_csrf={token}&clear_tmdb_api_key=on"));
    assert!(saved().media.tmdb.api_key.is_empty());

    let added = post(
        port,
        "/ui/settings/servers",
        &cookie,
        "",
        &format!(
            "_csrf={token}&do=add&host=news.b&username=me&password=pw-b&port=119&ssl=on&connections=4&priority=2&index=on"
        ),
    );
    assert_eq!(added.status(), 200);
    assert!(added.body.contains("news.b is added"), "{}", added.body);
    assert!(!added.body.contains("pw-b"), "the new password is on the page");
    let cfg = saved();
    assert_eq!(cfg.servers.iter().map(|s| s.host.as_str()).collect::<Vec<_>>(), ["127.0.0.1", "news.b"]);
    let b = &cfg.servers[1];
    assert_eq!((b.password.as_str(), b.port, b.ssl, b.connections, b.priority), ("pw-b", 119, Some(false), Some(4), 2));
    assert_eq!(cfg.servers[0].password, "secret", "the other server kept its password");
    assert_eq!(cfg.index_mode, "dynamic");
    assert_eq!(cfg.api_key.as_deref(), Some(KEY), "the key stays as it was");

    // an edit through htmx: the token in the header, an empty password keeps the saved one
    let edited = post(
        port,
        "/ui/settings/servers",
        &cookie,
        &format!("HX-Request: true\r\nX-CSRF-Token: {token}\r\n"),
        "do=edit&i=1&was=news.b&host=news.b&username=you&password=&port=563&ssl=on&connections=6&priority=0",
    );
    assert_eq!(edited.status(), 200);
    assert!(edited.body.contains("news.b is saved"), "{}", edited.body);
    let cfg = saved();
    assert_eq!(cfg.servers[0].host, "news.b", "priority 0 comes first now");
    let b = &cfg.servers[0];
    assert_eq!((b.username.as_str(), b.password.as_str(), b.ssl, b.index), ("you", "pw-b", Some(true), Some(false)));

    // remove one, never the last
    let removed = post(port, "/ui/settings/servers", &cookie, "", &format!("_csrf={token}&do=remove&i=0&was=news.b"));
    assert!(removed.body.contains("news.b is removed"), "{}", removed.body);
    assert_eq!(saved().servers.len(), 1);
    let last = post(port, "/ui/settings/servers", &cookie, "", &format!("_csrf={token}&do=remove&i=0&was=127.0.0.1"));
    assert!(last.body.contains("The last server stays"), "{}", last.body);
    assert_eq!(saved().servers.len(), 1);

    // indexing: the mode and the two numbers land in the file, a bad mode doesn't
    let indexing = post(
        port,
        "/ui/settings/indexing",
        &cookie,
        "",
        &format!("_csrf={token}&index_mode=live&parallel_groups=3&max_unsaved_headers=40000"),
    );
    assert!(indexing.body.contains("mode live"), "{}", indexing.body);
    assert!(indexing.body.contains("<option value=\"live\" selected>"), "{}", indexing.body);
    let cfg = saved();
    assert_eq!(
        (cfg.index_mode.as_str(), cfg.parallel_groups, cfg.max_unsaved_headers),
        ("live", Some(3), Some(40_000))
    );
    let bad = post(port, "/ui/settings/indexing", &cookie, "", &format!("_csrf={token}&index_mode=fast"));
    assert!(bad.body.contains("not saved"), "{}", bad.body);
    assert_eq!(saved().index_mode, "live");
    let text = std::fs::read_to_string(home.path().join("config.toml")).unwrap();
    assert!(text.contains("parallel_groups = 3") && text.contains("max_unsaved_headers = 40000"), "{text}");
    assert!(text.contains("password = \"secret\""), "no keyring here, soo the file keeps it: {text}");

    // a key something else writes is shown, not taken
    // SAFETY: the server's threads are idle between requests, and nothing reads the environment from C
    unsafe { std::env::set_var("ATLAS_MANAGED_KEYS", "index_mode,groups") };
    let page = fetch(port, "/ui/settings", &cookie);
    assert!(page.body.contains("name=\"index_mode\" disabled"), "{}", page.body);
    assert!(!page.body.contains("action=\"/ui/settings/groups\""), "{}", page.body);
    let group_kept =
        post(port, "/ui/settings/groups", &cookie, "", &format!("_csrf={token}&do=add&group=alt.binaries.managed"));
    assert!(group_kept.body.contains("Groups are managed outside Atlas"), "{}", group_kept.body);
    let kept = post(
        port,
        "/ui/settings/indexing",
        &cookie,
        "",
        &format!("_csrf={token}&index_mode=backfill&parallel_groups=2"),
    );
    assert!(kept.body.contains("2 groups at once") && !kept.body.contains("mode backfill"), "{}", kept.body);
    assert_eq!((saved().index_mode.as_str(), saved().parallel_groups), ("live", Some(2)));
    unsafe { std::env::remove_var("ATLAS_MANAGED_KEYS") };

    // the api address: the same one changes nothing, a new port moves the api there
    let same =
        post(port, "/ui/settings/api", &cookie, "", &format!("_csrf={token}&api_host=127.0.0.1&api_port={port}"));
    assert!(same.body.contains("Nothing changed"), "{}", same.body);
    let bad = post(port, "/ui/settings/api", &cookie, "", &format!("_csrf={token}&api_host=127.0.0.1&api_port=0"));
    assert!(bad.body.contains("not saved"), "{}", bad.body);
    assert_eq!(saved().api_port, Some(port));

    let moved_to = free_port();
    let moving =
        post(port, "/ui/settings/api", &cookie, "", &format!("_csrf={token}&api_host=127.0.0.1&api_port={moved_to}"));
    assert!(moving.body.contains(&format!("http://127.0.0.1:{moved_to}")), "{}", moving.body);
    assert_eq!(saved().api_port, Some(moved_to));
    let deadline = Instant::now() + Duration::from_secs(10);
    let there = loop {
        if let Some(reply) = send(moved_to, "GET / HTTP/1.1\r\nHost: atlas.test\r\nConnection: close\r\n\r\n")
            .filter(|r| r.status() == 200)
        {
            break reply;
        }
        assert!(Instant::now() < deadline, "the api did not come up on port {moved_to}");
        std::thread::sleep(Duration::from_millis(100));
    };
    assert!(there.body.contains("action=\"/login\""), "{}", there.body);
    // and the old port is closed
    let gone = Instant::now() + Duration::from_secs(5);
    while TcpStream::connect(("127.0.0.1", port)).is_ok() {
        assert!(Instant::now() < gone, "the api still answers on port {port}");
        std::thread::sleep(Duration::from_millis(100));
    }
    // the same session is signed in there: the cookie follows the key, not the port
    let page = fetch(moved_to, "/ui/settings", &cookie);
    assert_eq!(page.status(), 200);
    assert!(page.body.contains(&format!("value=\"{moved_to}\"")), "{}", page.body);

    api::stop();
}
