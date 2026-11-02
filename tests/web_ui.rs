//! The web ui over a real socket: sign in, search, filter, download.
//!
//! One test: it points ATLAS_HOME at a temp dir before any thread is spawned.

mod common;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};

use atlas::config::Config;
use atlas::parser::{Article, Release};
use atlas::{api, db, store};
use common::stage::{article, release_of};

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
    for (name, group) in [
        ("Tv.Findword.S01E01-GRP", "alt.binaries.g1"),
        ("<script>alert(1)</script>", "alt.binaries.g2"),
        ("Film.Findword.2024.2160p.WEB-DL.DV.H265-GRP", "alt.binaries.g3"),
    ] {
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
    assert!(filtered.body.contains("Film.Findword"), "2160p: {}", filtered.body);
    // with no words, the newest are filtered: no search for each way a tag is written
    let newest = fetch(port, "/ui/results?filters=2160p,DV", &cookie);
    assert!(newest.body.contains("Film.Findword") && !newest.body.contains("Tv.Findword"), "{}", newest.body);
    assert!(newest.body.contains("aria-current=\"true\">DV<"), "the chip shows it's on: {}", newest.body);
    let none = fetch(port, "/ui/results?q=Findword&filters=Remux", &cookie);
    assert!(none.body.contains("nothing matches"), "{}", none.body);
    assert_eq!(
        fetch(port, "/ui/results?q=Findword&filters=2160p", &cookie).header("HX-Push-Url").as_deref(),
        Some("/?q=Findword&filters=2160p")
    );
    assert!(
        fetch(port, &format!("/api?t=search&q=Findword&apikey={KEY}"), "")
            .body
            .contains(r#"name="group" value="alt.binaries.g1""#)
    );

    let sorted = fetch(port, "/ui/results?sort=name&dir=asc", &cookie);
    assert!(sorted.body.find("&lt;script&gt;").unwrap() < sorted.body.find("Tv.Findword").unwrap(), "{}", sorted.body);
    let sorted = fetch(port, "/ui/results?sort=name&dir=desc", &cookie);
    assert!(sorted.body.find("&lt;script&gt;").unwrap() > sorted.body.find("Tv.Findword").unwrap(), "{}", sorted.body);
    assert!(sorted.body.contains("sort=name&amp;dir=asc"), "the heading flips: {}", sorted.body);
    let by_group = fetch(port, "/ui/results?group=alt.binaries.g1", &cookie);
    assert!(by_group.body.contains("Findword") && by_group.body.contains("1 release"), "{}", by_group.body);
    let by_group_uhd = fetch(port, "/ui/results?group=alt.binaries.g1&filters=2160p", &cookie);
    assert!(!by_group_uhd.body.contains("1 release"), "no total that counts hidden rows: {}", by_group_uhd.body);
    assert!(!by_group.body.contains("alert(1)"), "{}", by_group.body);
    assert_eq!(by_group.header("HX-Push-Url").as_deref(), Some("/?group=alt.binaries.g1"));

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

    // one post spread over two groups: one row, whole, and an NZB with all of it
    let posted = |name: &str, group: &str, one: Article, date: &str| {
        let size = one.bytes;
        store::save(
            &main,
            &[Release {
                name: name.into(),
                group: group.into(),
                date: date.into(),
                articles: vec![one],
                size,
                complete: false,
                ..Default::default()
            }],
        )
        .unwrap();
    };
    const DAY: &str = "2026-10-03 17:41:49";
    for (group, part) in [("alt.binaries.g1", 1), ("alt.binaries.g2", 2)] {
        let one = article("split.bin", part, 2, &format!("<split.{part}@x>"));
        posted("Spread.Post.S01E01-GRP", group, one, DAY);
    }
    let spread = fetch(port, "/ui/results?q=Spread", &cookie);
    assert_eq!(spread.body.matches("Spread.Post.S01E01-GRP</a>").count(), 1, "{}", spread.body);
    assert!(spread.body.contains("2 GROUPS") && !spread.body.contains("INCOMPLETE"), "{}", spread.body);
    let both = fetch(port, "/ui/results?group=alt.binaries.g2&q=Spread", &cookie);
    assert_eq!(both.body.matches("Spread.Post.S01E01-GRP</a>").count(), 1, "{}", both.body);
    let spread_id = spread.body.split("/ui/release?id=").nth(1).unwrap().split('"').next().unwrap().to_string();
    let whole = fetch(port, &format!("/ui/nzb?id={spread_id}"), &cookie);
    assert!(whole.body.contains("split.1@x") && whole.body.contains("split.2@x"), "{}", whole.body);
    assert!(
        whole.body.contains("<group>alt.binaries.g1</group>") && whole.body.contains("<group>alt.binaries.g2</group>"),
        "{}",
        whole.body
    );
    let opened = fetch(port, &format!("/ui/release?id={spread_id}"), &cookie);
    assert!(opened.body.contains("COMPLETE") && !opened.body.contains("INCOMPLETE"), "{}", opened.body);
    assert!(opened.body.contains("2 of 2"), "{}", opened.body);
    let one_file = fetch(port, &format!("/api?t=get&id={spread_id}&file=1&apikey={KEY}"), "");
    assert!(one_file.body.contains("split.1@x") && one_file.body.contains("split.2@x"), "{}", one_file.body);

    // its rows can share a fingerprint, and hiding copies doesn't lose the post
    let spread_shards: Vec<_> = ["alt.binaries.g1", "alt.binaries.g2"]
        .map(|group| db::open_shard(&store::shard_path(&main, store::shard_of(group))).unwrap())
        .into_iter()
        .collect();
    for shard in &spread_shards {
        shard
            .execute(
                "insert into release_fingerprint (release_id, posted)
                 select id, x'01' from releases where name = 'Spread.Post.S01E01-GRP'",
                [],
            )
            .unwrap();
    }
    let hiding = fetch(port, "/ui/results?q=Spread&filters=Hide%20copies&sort=group&dir=asc", &cookie);
    assert_eq!(hiding.body.matches("Spread.Post.S01E01-GRP</a>").count(), 1, "{}", hiding.body);
    for shard in &spread_shards {
        shard.execute("delete from release_fingerprint", []).unwrap();
    }

    // it sorts by the size it shows
    for (group, part) in [("alt.binaries.g1", 1), ("alt.binaries.g2", 2)] {
        posted("Sized.Spread.S01E01-GRP", group, article("sized.bin", part, 2, &format!("<sized.{part}@x>")), DAY);
    }
    let mut bigger = article("alone.bin", 1, 2, "<alone@x>");
    bigger.bytes = 1_000_000;
    posted("Sized.Alone.S01E01-GRP", "alt.binaries.g1", bigger, DAY);
    let sized = fetch(port, "/ui/results?q=Sized&sort=size", &cookie);
    let spot = |name: &str| sized.body.find(name).unwrap();
    assert!(spot("Sized.Spread.S01E01-GRP") < spot("Sized.Alone.S01E01-GRP"), "{}", sized.body);

    // a space in the part counter is still the same subject
    for (group, part) in [("alt.binaries.g1", 1), ("alt.binaries.g2", 2)] {
        let mut one = article("spaced.bin", part, 2, &format!("<spaced.{part}@x>"));
        one.subject = format!("\"spaced.bin\" yEnc ({part} /2)");
        posted("Spaced.Post.S01E01-GRP", group, one, DAY);
    }
    let spaced = fetch(port, "/ui/results?q=Spaced", &cookie);
    assert!(spaced.body.contains("2 GROUPS") && !spaced.body.contains("INCOMPLETE"), "{}", spaced.body);

    // with no count of files, a file only one group has doesn't join
    posted("Extra.File.S01E01-GRP", "alt.binaries.g1", article("x.bin", 1, 2, "<extra.1@x>"), DAY);
    posted("Extra.File.S01E01-GRP", "alt.binaries.g2", article("x.bin", 2, 2, "<extra.2@x>"), DAY);
    posted("Extra.File.S01E01-GRP", "alt.binaries.g2", article("y.bin", 1, 1, "<extra.y@evil>"), DAY);
    let extra = fetch(port, "/ui/results?q=Extra.File", &cookie);
    assert!(extra.body.contains("INCOMPLETE") && !extra.body.contains(" GROUPS"), "{}", extra.body);

    // it isn't whole until the missing part comes
    for (group, part) in [("alt.binaries.g1", 1), ("alt.binaries.g2", 2)] {
        posted("Gapped.Post.S01E01-GRP", group, article("gap.bin", part, 3, &format!("<gap.{part}@x>")), DAY);
    }
    let gapped = fetch(port, "/ui/results?q=Gapped", &cookie);
    assert!(gapped.body.contains("INCOMPLETE") && gapped.body.contains("2 GROUPS"), "{}", gapped.body);
    let gap_id = gapped.body.split("/ui/release?id=").nth(1).unwrap().split('"').next().unwrap().to_string();
    let opened = fetch(port, &format!("/ui/release?id={gap_id}"), &cookie);
    assert!(opened.body.contains("2 of 3"), "{}", opened.body);

    // a name and poster anyone can copy aren't enough: what doesn't fit one upload stays in its own group
    let other_part = |file: &str, id: &str, subject: Option<&str>, parts: i64| {
        let mut one = article(file, 2, parts, id);
        if let Some(subject) = subject {
            one.subject = subject.into();
        }
        one
    };
    for (name, one, date) in [
        // a part the other group has too, under another message id
        ("Same.Part", article("x.bin", 1, 2, "<swap@evil>"), DAY),
        // another subject for the same file
        ("New.Subject", other_part("x.bin", "<subject@evil>", Some("\"x.bin\" yEnc (2/2) other"), 2), DAY),
        // another count of parts
        ("New.Count", other_part("x.bin", "<count@evil>", None, 3), DAY),
        // no file in common
        ("New.File", other_part("y.bin", "<file@evil>", None, 2), DAY),
        // long after
        ("Much.Later", other_part("x.bin", "<later@evil>", None, 2), "2026-10-05 17:41:49"),
        // no date to go by
        ("No.Date", other_part("x.bin", "<nodate@evil>", None, 2), ""),
    ] {
        let name = format!("{name}.Fake.S01E01-GRP");
        posted(&name, "alt.binaries.g1", article("x.bin", 1, 2, &format!("<{name}@x>")), DAY);
        posted(&name, "alt.binaries.g2", one, date);
        let found = fetch(port, &format!("/ui/results?q=%22{name}%22"), &cookie);
        assert_eq!(found.body.matches(&format!("{name}</a>")).count(), 2, "{}", found.body);
        assert!(found.body.contains("INCOMPLETE") && !found.body.contains(" GROUPS"), "{}", found.body);
        let id = found.body.split("/ui/release?id=").nth(1).unwrap().split('"').next().unwrap().to_string();
        let nzb = fetch(port, &format!("/ui/nzb?id={id}"), &cookie);
        assert_eq!(nzb.body.matches("<group>").count(), 1, "{}", nzb.body);
        assert!(!nzb.body.contains("@evil") || !nzb.body.contains(&format!("{name}@x")), "{}", nzb.body);
    }

    // copies, listed together, with what tells them apart
    assert_eq!(fetch(port, "/ui/duplicates", "").status(), 303);
    assert!(fetch(port, "/ui/duplicates", &cookie).body.contains("no twins found yet"));
    let script: i64 = {
        let shard = db::open_shard(&store::shard_path(&main, store::shard_of("alt.binaries.g2"))).unwrap();
        let script = shard.query_row("select id from releases where group_name = 'alt.binaries.g2'", [], |r| r.get(0));
        let script: i64 = script.unwrap();
        shard.execute(atlas::duplicates::SET_POSTED, rusqlite::params![script, b"same".as_slice()]).unwrap();
        script
    };
    shard.execute(atlas::duplicates::SET_POSTED, rusqlite::params![id, b"same".as_slice()]).unwrap();
    let twins = fetch(port, "/ui/duplicates?kind=twins", &cookie);
    assert!(twins.body.contains("Twins 1") && twins.body.contains("Tv.Findword"), "{}", twins.body);
    assert!(twins.body.contains("/ui/release?id=") && twins.body.contains("TWIN"), "{}", twins.body);
    assert!(!twins.body.contains("<script>alert"), "{}", twins.body);
    assert!(fetch(port, "/ui/duplicates?kind=alts", &cookie).body.contains("no alts found yet"));
    let opened = fetch(port, &format!("/ui/release?id={script}"), &cookie);
    assert!(opened.body.contains("Other copies") && opened.body.contains("Tv.Findword"), "{}", opened.body);

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
