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
    let raw = String::from_utf8_lossy(&raw).into_owned();
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
    assert!(root.body.contains("class=\"site-footer\"") && root.body.contains("https://www.patreon.com/cw/xbmc4lyfe"));
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
    assert!(
        page.body.contains("class=\"site-footer\"") && page.body.contains("Atlas - Rust Edition."),
        "{}",
        page.body
    );
    assert!(page.body.contains("last git commit:"), "{}", page.body);
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
    // the database counts the group's rows, not their tags: a most, said so
    assert!(by_group_uhd.body.contains(">Up to 1 release<"), "a total that counts hidden rows: {}", by_group_uhd.body);
    assert!(by_group_uhd.body.contains("Counted before releases without the chosen tags are left out. "));

    // every page has a total, once the word index can count it
    let waited = std::time::Instant::now();
    while atlas::word_index::ready().is_none() {
        assert!(waited.elapsed() < std::time::Duration::from_secs(60), "the word index never got built");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let total = |path: &str| {
        let body = fetch(port, path, &cookie).body;
        let start = body.find("class=\"total\"").unwrap_or_else(|| panic!("no total on {path}: {body}"));
        let end = body[start..].find("</strong>").unwrap() + start;
        body[body[..end].rfind('>').unwrap() + 1..end].to_string()
    };
    assert_eq!(total("/ui/results"), "3 releases");
    assert_eq!(total("/ui/results?cat=5000"), "1 release");
    assert_eq!(total("/ui/results?cat=5000&sort=size&dir=desc"), "1 release");
    assert_eq!(total("/ui/results?cat=2000"), "0 releases");
    assert_eq!(total("/ui/results?filters=2160p"), "1 release");
    assert_eq!(total("/ui/results?filters=2160p&sort=name&dir=asc"), "1 release");
    assert_eq!(total("/ui/results?q=Findword&filters=DV"), "1 release");
    assert_eq!(total("/ui/results?filters=Hide%20copies"), "Up to 3 releases");
    // a window of its own: the pages further on keep theirs a minute
    assert_eq!(total("/ui/results?cat=5000&incomplete=hide"), "Up to 1 release");
    assert_eq!(total("/ui/results?q=Findword&incomplete=hide&filters=Hide%20copies"), "Up to 2 releases");
    let tagged = fetch(port, "/ui/results?filters=2160p", &cookie).body;
    assert!(tagged.contains("Film.Findword") && !tagged.contains("Tv.Findword"), "{tagged}");
    assert!(tagged.contains("Newest first<"), "every release filtered, not the newest few: {tagged}");
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
    assert!(release.body.contains("Files in the NZB") && release.body.contains("x.bin"), "{}", release.body);
    assert!(!release.body.contains("Cover art"), "none kept yet: {}", release.body);
    let info = r#"{"health":{},"headers":{},"crossposted_to":[],"imdb_ids":[],"sfv":{},"media":{"container":"matroska","attachments":[{"name":"cover.jpg","mime":"image/jpeg","size":4}],
        "video":[{"codec":"V_MPEGH/ISO/HEVC","width":1440,"height":1080,"display_width":1920,"display_height":1080,"primaries":9,"dv_profile":8,"dv_level":6}]}}"#;
    shard.execute("insert into release_probe (release_id, info) values (?, ?)", rusqlite::params![id, info]).unwrap();
    let jpeg = [0xff_u8, 0xd8, 0xff, 0xd9];
    let sha256 = atlas::probe::keep_cover(&jpeg).unwrap();
    assert!(home.path().join("covers").join(&sha256).exists());
    shard
        .execute(
            "insert into release_cover (release_id, mime, sha256) values (?, 'image/jpeg', ?)",
            rusqlite::params![id, sha256],
        )
        .unwrap();
    let release = fetch(port, &format!("/ui/release?id={id}"), &cookie);
    for wanted in [
        "Cover art",
        &format!("/ui/cover?id={id}"),
        "Attachments",
        "cover.jpg",
        "shown at 1.78:1",
        "BT.2020",
        "profile 8 level 6",
    ] {
        assert!(release.body.contains(wanted), "{wanted}: {}", release.body);
    }
    let cover = fetch(port, &format!("/ui/cover?id={id}"), &cookie);
    assert_eq!((cover.status(), cover.header("Content-Type").as_deref()), (200, Some("image/jpeg")));
    assert_eq!(fetch(port, &format!("/ui/cover?id={id}"), "").status(), 401);
    assert_eq!(fetch(port, "/ui/cover?id=999999999", &cookie).status(), 404);
    assert_eq!(fetch(port, "/ui/release?id=999999999", &cookie).status(), 404);
    assert_eq!(fetch(port, &format!("/ui/release?id={id}"), "").status(), 303);

    // every catalog fact: the panels, their lazy fold-outs, and the drawer under a result row
    assert!(
        release.body.contains("<h2>Keys</h2>") && release.body.contains("hx-get=\"/ui/meta?id="),
        "{}",
        release.body
    );
    let parts = fetch(port, &format!("/ui/meta?id={id}&part=parts"), &cookie);
    assert_eq!(parts.status(), 200);
    assert!(parts.body.contains("<table") && parts.body.contains("x.bin"), "{}", parts.body);
    assert_eq!(fetch(port, &format!("/ui/meta?id={id}&part=nope"), &cookie).status(), 400);
    // the fragments go through the same filtered reader as the page: no release, no fragment
    assert_eq!(fetch(port, "/ui/meta?id=999999999&part=parts", &cookie).status(), 404);
    let drawer = fetch(port, &format!("/ui/row?id={id}"), &cookie);
    assert_eq!(drawer.status(), 200);
    assert!(
        drawer.body.starts_with("<tr class=\"drawer-row\">") && drawer.body.contains("Full page"),
        "{}",
        drawer.body
    );
    let locked = fetch(port, &format!("/ui/row?id={id}"), "");
    assert_eq!(locked.status(), 401);
    assert_eq!(locked.header("HX-Redirect").as_deref(), Some("/"));
    assert!(found.body.contains("drawer-toggle"), "{}", found.body);

    // the timing facts come from the overview's dates, numbers and lines, kept at ingest
    let timed = |part: i64, number: u64, date: &str| Article {
        number,
        date: date.into(),
        lines: 4_000,
        ..article("timed.bin", part, 3, &format!("<timed{part}@x>"))
    };
    store::save(
        &main,
        &[Release {
            name: "Timed.Findword.2024-GRP".into(),
            group: "alt.binaries.g1".into(),
            size: 2_100_000,
            complete: true,
            articles: vec![
                timed(1, 500, "2026-10-02 10:00:00"),
                timed(2, 501, "2026-10-02 10:02:00"),
                timed(3, 502, "2026-10-02 10:05:00"),
            ],
            ..Default::default()
        }],
    )
    .unwrap();
    let timed_id: i64 =
        shard.query_row("select id from releases where name = 'Timed.Findword.2024-GRP'", [], |r| r.get(0)).unwrap();
    let timed_page = fetch(port, &format!("/ui/release?id={timed_id}"), &cookie);
    for fact in [
        "<dt>First and last article</dt><dd>2026-10-02 10:00 to 2026-10-02 10:05 by the poster's clock, 5 min</dd>",
        "<dt>Posting rate</dt><dd>0.6 articles a minute, 6.8 KB/s</dd>",
        "<dt>Article numbers</dt><dd>500–502 where it was read, adjacent</dd>",
        "<dt>Threaded replies</dt><dd>none</dd>",
        "<dt>From variants</dt><dd>one From throughout</dd>",
        "<dt>Lines per article</dt><dd>4,000 lines an article, 175 bytes a line</dd>",
        "<dt>Late articles</dt><dd>none</dd>",
        "<dt>Lines key</dt>",
    ] {
        assert!(timed_page.body.contains(fact), "{fact}\n{}", timed_page.body);
    }
    // the drawer keeps to its ten facts, and a release without rows says nothing about timing
    assert!(!fetch(port, &format!("/ui/row?id={timed_id}"), &cookie).body.contains("First and last article"));
    assert!(!release.body.contains("First and last article"), "{}", release.body);

    // two releases posted from one account: each page counts the other and links to the list
    let mut keyed = Vec::new();
    for name in ["Keyed.One.2024-GRP", "Keyed.Two.2024-GRP"] {
        store::save(&main, &[Release { group: "alt.binaries.g2".into(), ..release_of(name, "k.bin", 2, 2) }]).unwrap();
        let s = db::open_shard(&store::shard_path(&main, store::shard_of("alt.binaries.g2"))).unwrap();
        let id: i64 = s.query_row("select id from releases where name = ?", [name], |r| r.get(0)).unwrap();
        // one uploader, and the same first keyframe: the same video stream, posted twice
        let info = r#"{"health":{},"crossposted_to":[],"imdb_ids":[],"sfv":{},"headers":{"Injection-Info":"news.example; posting-account=\"same-uploader\""},"media":{"container":"matroska","video":[{"codec":"V_MPEGH/ISO/HEVC","keyframe_key":"0123456789abcdef"}],"audio":[{"codec":"A_EAC3","stream_key":"fedcba9876543210"}]}}"#;
        s.execute("insert into release_probe (release_id, info) values (?, ?)", rusqlite::params![id, info]).unwrap();
        // what the keys step writes, with the salt the pages use
        let salt = atlas::keys::salt(&db::open_with_shards(&main).unwrap());
        let k = atlas::keys::read_keys(&s, id, Some(2), salt).unwrap();
        s.execute(
            "insert into release_keys (release_id, account, host, layout, lines, video, audio) values (?, ?, ?, ?, ?, ?, ?)",
            rusqlite::params![id, k.account, k.host, k.layout, k.lines, k.video, k.audio],
        )
        .unwrap();
        keyed.push(id);
    }
    let (one, two) = (keyed[0], keyed[1]);
    let keyed_page = fetch(port, &format!("/ui/release?id={one}"), &cookie);
    assert!(
        keyed_page.body.contains(&format!("<a href=\"/ui/same?id={one}&amp;kind=account\">in 1 other release</a>")),
        "{}",
        keyed_page.body
    );
    let same = fetch(port, &format!("/ui/same?id={one}&kind=account"), &cookie);
    assert_eq!(same.status(), 200);
    assert!(
        same.body.contains("Same posting account")
            && same.body.contains(&format!("<a href=\"/ui/release?id={two}\">Keyed.Two.2024-GRP</a>")),
        "{}",
        same.body
    );
    assert!(
        keyed_page.body.contains("<dt>Video stream key</dt>")
            && keyed_page
                .body
                .contains(&format!("<a href=\"/ui/same?id={one}&amp;kind=video\">in 1 other release</a>")),
        "{}",
        keyed_page.body
    );
    assert!(
        keyed_page.body.contains("<dt>Audio stream key</dt>")
            && keyed_page
                .body
                .contains(&format!("<a href=\"/ui/same?id={one}&amp;kind=audio\">in 1 other release</a>")),
        "{}",
        keyed_page.body
    );
    let same_audio = fetch(port, &format!("/ui/same?id={one}&kind=audio"), &cookie);
    assert!(
        same_audio.body.contains("Same audio stream")
            && same_audio.body.contains(&format!("<a href=\"/ui/release?id={two}\">Keyed.Two.2024-GRP</a>")),
        "{}",
        same_audio.body
    );
    let same_video = fetch(port, &format!("/ui/same?id={one}&kind=video"), &cookie);
    assert!(
        same_video.body.contains("Same video stream")
            && same_video.body.contains(&format!("<a href=\"/ui/release?id={two}\">Keyed.Two.2024-GRP</a>")),
        "{}",
        same_video.body
    );
    let none = fetch(port, &format!("/ui/same?id={one}&kind=lines"), &cookie);
    assert!(none.status() == 200 && none.body.contains("this release has no such key"), "{}", none.body);
    assert_eq!(fetch(port, &format!("/ui/same?id={one}&kind=raw"), &cookie).status(), 400);
    assert_eq!(fetch(port, "/ui/same?id=999999999&kind=account", &cookie).status(), 404);
    assert_eq!(fetch(port, &format!("/ui/same?id={one}&kind=account"), "").status(), 303);

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

    // and one whose bracket counter is the part number
    for (group, part) in [("alt.binaries.g1", 1), ("alt.binaries.g2", 2)] {
        let mut one = article("bracket.bin", part, 2, &format!("<bracket.{part}@x>"));
        one.subject = format!("[{part}/2] \"bracket.bin\" yEnc (1/1)");
        posted("Bracket.Post.S01E01-GRP", group, one, DAY);
    }
    let bracket = fetch(port, "/ui/results?q=Bracket", &cookie);
    assert!(bracket.body.contains("2 GROUPS") && !bracket.body.contains("INCOMPLETE"), "{}", bracket.body);

    // but not a bracket counter that counts files
    for (group, part) in [("alt.binaries.g1", 1), ("alt.binaries.g2", 2)] {
        let mut one = article("indexed.bin", part, 2, &format!("<indexed.{part}@x>"));
        one.subject = format!("[{part}/2] \"indexed.bin\" yEnc ({part}/2)");
        posted("Indexed.Post.S01E01-GRP", group, one, DAY);
    }
    let indexed = fetch(port, "/ui/results?q=Indexed", &cookie);
    assert!(indexed.body.contains("INCOMPLETE") && !indexed.body.contains(" GROUPS"), "{}", indexed.body);

    // hiding incomplete releases goes by what a row shows: the spread post's rows are each incomplete in the database
    // but whole together, and the bracket-counted one isn't whole
    let hide = fetch(port, "/ui/results?q=Spread&incomplete=hide", &cookie);
    assert_eq!(hide.body.matches("Spread.Post.S01E01-GRP</a>").count(), 1, "{}", hide.body);
    assert!(hide.body.contains("aria-current=\"true\">Hide incomplete<"), "the chip shows it's on: {}", hide.body);
    assert_eq!(hide.header("HX-Push-Url").as_deref(), Some("/?q=Spread&incomplete=hide"));
    let kept = hide.header("Set-Cookie").unwrap();
    assert!(kept.starts_with("atlas_incomplete=hide;") && kept.contains("HttpOnly"), "{kept}");
    // the cookie keeps it on with no word of it in the url
    let remembered = format!("{cookie}; atlas_incomplete=hide");
    let gone = fetch(port, "/ui/results?q=Indexed", &remembered);
    assert!(!gone.body.contains("Indexed.Post.S01E01-GRP"), "{}", gone.body);
    assert!(gone.body.contains("Incomplete releases are hidden"), "{}", gone.body);
    assert!(gone.header("Set-Cookie").is_none(), "nothing new to keep");
    for browse in ["/ui/results", "/ui/results?sort=name&dir=asc", "/ui/results?sort=size"] {
        let page = fetch(port, browse, &remembered);
        assert!(page.body.contains("Spread.Post.S01E01-GRP</a>"), "{browse}: {}", page.body);
        assert!(
            !page.body.contains("Indexed.Post.S01E01-GRP") && !page.body.contains("INCOMPLETE"),
            "{browse}: {}",
            page.body
        );
        assert!(page.body.contains("incomplete hidden"), "{browse}: {}", page.body);
    }
    let full = fetch(port, "/", &remembered);
    assert!(full.body.contains("name=\"incomplete\" value=\"hide\""), "typing keeps it: {}", full.body);
    // and turning it off drops the cookie
    let shown = fetch(port, "/ui/results?q=Indexed&incomplete=show", &remembered);
    assert!(shown.body.contains("Indexed.Post.S01E01-GRP"), "{}", shown.body);
    assert!(shown.header("Set-Cookie").unwrap().starts_with("atlas_incomplete=; "), "{:?}", shown.header("Set-Cookie"));
    assert_eq!(shown.header("HX-Push-Url").as_deref(), Some("/?q=Indexed"));

    // a multi-file upload split by file across groups and suffix-tainted rows with per-file random posters folds into one whole release
    for (name, group, file_idx, file_name, poster) in [
        ("Heartsplit.1981.2160p-GRP", "alt.binaries.g1", 1, "Heartsplit.1981.2160p-GRP.par2", "a@rand.local"),
        (
            "Heartsplit.1981.2160p-GRP.vol001-003",
            "alt.binaries.g2",
            2,
            "Heartsplit.1981.2160p-GRP.vol001-003.par2",
            "b@rand.local",
        ),
        ("Heartsplit.1981.2160p-GRP.mkv", "alt.binaries.g3", 3, "Heartsplit.1981.2160p-GRP.mkv", "c@rand.local"),
        (
            "Heartsplit.1981.2160p-GRP.vol003-007",
            "alt.binaries.g4",
            4,
            "Heartsplit.1981.2160p-GRP.vol003-007.par2",
            "d@rand.local",
        ),
    ] {
        let mut one = article(file_name, 1, 1, &format!("<heartsplit.{file_idx}@x>"));
        one.subject = format!("[0{file_idx}/04] - \"{file_name}\" yEnc (1/1)");
        one.file_total = Some(4);
        one.author = poster.into();
        let size = one.bytes;
        store::save(
            &main,
            &[Release {
                name: name.into(),
                group: group.into(),
                poster: poster.into(),
                date: DAY.into(),
                articles: vec![one],
                size,
                complete: false,
                ..Default::default()
            }],
        )
        .unwrap();
    }
    let mut heart_ids = Vec::new();
    for (name, group) in [
        ("Heartsplit.1981.2160p-GRP", "alt.binaries.g1"),
        ("Heartsplit.1981.2160p-GRP.vol001-003", "alt.binaries.g2"),
        ("Heartsplit.1981.2160p-GRP.mkv", "alt.binaries.g3"),
        ("Heartsplit.1981.2160p-GRP.vol003-007", "alt.binaries.g4"),
    ] {
        let g_shard = db::open_shard(&store::shard_path(&main, store::shard_of(group))).unwrap();
        let row_id: i64 = g_shard.query_row("select id from releases where name = ?1", [name], |r| r.get(0)).unwrap();
        heart_ids.push((row_id, name, group));
    }
    let heart_lead = heart_ids[0].0;
    for &(row_id, name, group) in &heart_ids {
        let g_shard = db::open_shard(&store::shard_path(&main, store::shard_of(group))).unwrap();
        let recovery = i64::from(!name.ends_with(".mkv"));
        g_shard
            .execute(
                "insert into upload_link (release_id, lead_id, recovery, wanted) values (?1, ?2, ?3, 1)",
                rusqlite::params![row_id, heart_lead, recovery],
            )
            .unwrap();
    }
    let heart = fetch(port, "/ui/results?q=Heartsplit", &cookie);
    assert_eq!(heart.body.matches("Heartsplit.1981.2160p-GRP</a>").count(), 1, "{}", heart.body);
    assert!(heart.body.contains("4 GROUPS") && !heart.body.contains("INCOMPLETE"), "{}", heart.body);
    for (row_id, name, _) in heart_ids {
        let page = fetch(port, &format!("/ui/release?id={row_id}"), &cookie);
        assert!(page.body.contains("COMPLETE") && !page.body.contains("INCOMPLETE"), "{name}: {}", page.body);
        assert!(page.body.contains("<h1>Heartsplit.1981.2160p-GRP</h1>"), "{name}: {}", page.body);
        assert!(page.body.contains("Heartsplit.1981.2160p-GRP.vol001-003.par2"), "{name}: {}", page.body);
        assert!(page.body.contains("Heartsplit.1981.2160p-GRP.mkv"), "{name}: {}", page.body);
        let nzb = fetch(port, &format!("/ui/nzb?id={row_id}"), &cookie);
        for msg in ["heartsplit.1@x", "heartsplit.2@x", "heartsplit.3@x", "heartsplit.4@x"] {
            assert!(nzb.body.contains(msg), "{name} missing {msg}: {}", nzb.body);
        }
    }

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

    // the NZB with gaps filled from a twin has all of it too
    posted("Gap.Twin.S01E01-GRP", "alt.binaries.g3", article("gap.bin", 1, 3, "<twin.1@x>"), DAY);
    posted("Gap.Twin.S01E01-GRP", "alt.binaries.g3", article("gap.bin", 3, 3, "<twin.3@x>"), DAY);
    let twin_shards: Vec<_> = ["alt.binaries.g1", "alt.binaries.g2", "alt.binaries.g3"]
        .map(|group| db::open_shard(&store::shard_path(&main, store::shard_of(group))).unwrap())
        .into_iter()
        .collect();
    for shard in &twin_shards {
        shard
            .execute(
                "insert or ignore into release_fingerprint (release_id, posted)
                 select id, x'02' from releases where name in ('Gapped.Post.S01E01-GRP', 'Gap.Twin.S01E01-GRP')",
                [],
            )
            .unwrap();
    }
    let filled = fetch(port, &format!("/ui/nzb?id={gap_id}&fill=1"), &cookie);
    assert!(filled.body.contains("gap.1@x") && filled.body.contains("gap.2@x"), "{}", filled.body);
    assert!(filled.body.contains("<group>alt.binaries.g1</group>"), "{}", filled.body);
    assert!(filled.body.contains("<group>alt.binaries.g2</group>"), "{}", filled.body);
    for shard in &twin_shards {
        shard.execute("delete from release_fingerprint", []).unwrap();
    }

    // too many groups to be a real post
    for at in 1..=17 {
        let one = article("many.bin", at, 17, &format!("<many.{at}@x>"));
        posted("Many.Groups.S01E01-GRP", &format!("alt.binaries.many{at}"), one, DAY);
    }
    let many = fetch(port, "/ui/results?q=Many.Groups", &cookie);
    assert!(!many.body.contains(" GROUPS"), "{}", many.body);

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
    // every copy card has the + that opens its facts in place, as a card rather than a table row
    assert_eq!(twins.body.matches("drawer-toggle").count(), 2, "{}", twins.body);
    assert!(twins.body.contains(&format!("hx-get=\"/ui/row?id={id}&amp;as=card\"")), "{}", twins.body);
    let card = fetch(port, &format!("/ui/row?id={id}&as=card"), &cookie);
    assert_eq!(card.status(), 200);
    assert!(card.body.starts_with("<div class=\"drawer-card\">") && card.body.ends_with("</div>"), "{}", card.body);
    assert!(card.body.contains("<dl class=\"facts facts-wide\">") && card.body.contains("Full page"), "{}", card.body);
    // the drawer reads through the same filtered reader as the page: an unknown id gets no facts, in either shape
    for shape in ["", "&as=card"] {
        let none = fetch(port, &format!("/ui/row?id=999999999{shape}"), &cookie);
        assert_eq!(none.status(), 200);
        assert!(none.body.contains("There is no such release.") && !none.body.contains("<dl"), "{}", none.body);
    }
    assert!(fetch(port, "/ui/duplicates?kind=alts", &cookie).body.contains("no alts found yet"));
    let opened = fetch(port, &format!("/ui/release?id={script}"), &cookie);
    assert!(opened.body.contains("Other copies") && opened.body.contains("Tv.Findword"), "{}", opened.body);
    assert!(opened.body.contains(&format!("hx-get=\"/ui/row?id={id}&amp;as=card\"")), "{}", opened.body);

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
