//! `atlas --purge`, as the command runs it.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use atlas::parser::{Article, Release};
use atlas::{db, store};
use rusqlite::Connection;

const G: &str = "alt.binaries.t";
const TOKEN: &str = "a1B2c3D4e5F6g7H8i9J0k1L2m3N4o5P6";

struct Home {
    dir: tempfile::TempDir,
    main: PathBuf,
}

fn home() -> Home {
    let dir = tempfile::tempdir().unwrap();
    let main = dir.path().join("atlas.db");
    db::create_db_at(&main).unwrap();
    // nothing has to wait to be old enough, unless a test says so
    let config =
        serde_json::json!({"postprocess": {"dead_after_secs": 0, "settle_secs": 1, "complete_settle_secs": 1}});
    std::fs::write(dir.path().join("config.json"), config.to_string()).unwrap();
    Home { dir, main }
}

impl Home {
    fn run(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        let mut c = Command::new(env!("CARGO_BIN_EXE_atlas"));
        c.arg("--purge").args(args).env("ATLAS_HOME", self.dir.path()).env("ATLAS_NO_KEYRING", "1");
        for (k, v) in env {
            c.env(k, v);
        }
        c.output().unwrap()
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = self.run(args, &[]);
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(out.status.success(), "{text}");
        text
    }

    fn shard(&self) -> Connection {
        db::open_shard(&store::shard_path(&self.main, store::shard_of(G))).unwrap()
    }

    fn names(&self) -> Vec<String> {
        let mut all = Vec::new();
        for path in store::shard_paths(&self.main) {
            let conn = db::open_shard(&path).unwrap();
            let mut stmt = conn.prepare("select name from releases order by name").unwrap();
            all.extend(stmt.query_map([], |r| r.get::<_, String>(0)).unwrap().map(|r| r.unwrap()));
        }
        all.sort();
        all
    }

    fn totals_match(&self) {
        for path in store::shard_paths(&self.main) {
            let conn = db::open_shard(&path).unwrap();
            let meta = |k: &str| store::get_meta(&conn, k).unwrap().unwrap_or(0);
            let releases: i64 = conn.query_row("select count(*) from releases", [], |r| r.get(0)).unwrap();
            assert_eq!(meta("releases"), releases, "{}", path.display());
            assert_eq!(meta("articles"), store::article_count(&conn, "main").unwrap(), "{}", path.display());
        }
    }
}

fn article(file: &str, part: i64, parts: i64, id: &str) -> Article {
    Article {
        message_id: id.into(),
        subject: format!("\"{file}\" yEnc ({part}/{parts})"),
        filename: Some(file.into()),
        part: Some(part),
        total_parts: Some(parts),
        bytes: 700_000,
        ..Default::default()
    }
}

fn whole(name: &str, file: &str, have: i64, parts: i64) -> Release {
    let articles: Vec<Article> =
        (1..=have).map(|p| article(file, p, parts, &format!("<{name}.{file}.{p}@x>"))).collect();
    Release { name: name.into(), group: G.into(), articles, ..Default::default() }
}

fn fragment(n: usize) -> Release {
    let name = format!("{TOKEN}{n}");
    Release {
        name: name.clone(),
        group: G.into(),
        articles: vec![article(&name, 7, 9, &format!("<frag{n}@x>"))],
        ..Default::default()
    }
}

/// one of each class, and good releases, some sealed
fn every_class(h: &Home) {
    let mut tiny = whole("Tiny.One", "tiny.bin", 2, 2);
    tiny.articles.iter_mut().for_each(|a| a.bytes = 100);
    store::save(
        &h.main,
        &[
            fragment(1),
            tiny,
            whole("Picture.One", "pic.jpg", 2, 2),
            whole("Par2.One", "x.par2", 2, 2),
            whole("Dead.One", "dead.bin", 1, 3),
            whole("Good.One", "good.bin", 2, 2),
            whole("Good.Two", "two.bin", 2, 2),
        ],
    )
    .unwrap();
    // from before the block
    let blocked = "alt.binaries.pictures.erotica.teen";
    let conn = db::open_shard(&store::shard_path(&h.main, store::shard_of(blocked))).unwrap();
    conn.execute(
        "insert into releases (id, name, group_name, parts, size, complete) values (?, 'Blocked.One', ?, 1, 1000, 1)",
        rusqlite::params![store::global_id(900_000, store::shard_of(blocked)), blocked],
    )
    .unwrap();
    conn.execute("update meta set value = value + 1 where key = 'releases'", []).unwrap();
}

#[test]
fn a_dry_run_changes_nothing_and_says_what_it_would_do() {
    let h = home();
    every_class(&h);
    let before = h.names();
    let text = h.ok(&[
        "--blocked",
        "--too-small",
        "--below",
        "1000",
        "--image-only",
        "--rejected",
        "--dead",
        "--fragments",
        "--sample",
        "2",
    ]);
    assert!(text.contains("fragments: 1 releases"), "{text}");
    assert!(text.contains("dead: 1 releases"), "{text}");
    assert!(text.contains("Dead.One"), "samples: {text}");
    assert!(text.contains("dry run"), "{text}");
    assert_eq!(h.names(), before);
    let conn = h.shard();
    assert_eq!(store::get_meta(&conn, "purge_classes").unwrap(), None);
    let guard = atlas::compact::hold_off_compaction(&h.main).unwrap();
    h.ok(&[]);
    drop(guard);
}

#[test]
fn no_flag_means_fragments_only() {
    let h = home();
    every_class(&h);
    let start = h.names();
    h.ok(&["--apply"]);
    let without_fragment: Vec<String> = start.iter().filter(|n| !n.starts_with(TOKEN)).cloned().collect();
    assert_eq!(h.names(), without_fragment);
    h.totals_match();
    h.ok(&["--apply", "--rejected"]);
    assert!(!h.names().contains(&"Par2.One".to_string()));
    assert!(
        h.names().contains(&"Tiny.One".to_string()) && h.names().contains(&"Picture.One".to_string()),
        "it leaves the tiny and the image"
    );
    h.ok(&["--apply", "--too-small", "--below", "1000"]);
    assert!(!h.names().contains(&"Tiny.One".to_string()));
    h.ok(&["--apply", "--image-only"]);
    assert!(!h.names().contains(&"Picture.One".to_string()));
    h.ok(&["--apply", "--dead"]);
    assert!(!h.names().contains(&"Dead.One".to_string()));
    h.ok(&["--apply", "--blocked"]);
    assert_eq!(h.names(), vec!["Good.One".to_string(), "Good.Two".to_string()]);
    h.totals_match();
}

#[test]
fn a_fresh_post_is_not_purged() {
    let h = home();
    every_class(&h);
    let config = serde_json::json!({"postprocess": {"dead_after_secs": 3600}});
    std::fs::write(h.dir.path().join("config.json"), config.to_string()).unwrap();
    h.ok(&[
        "--apply",
        "--fragments",
        "--too-small",
        "--below",
        "1000",
        "--image-only",
        "--rejected",
        "--dead",
        "--blocked",
    ]);
    let names = h.names();
    assert!(!names.contains(&"Blocked.One".to_string()), "a blocked group doesnt wait");
    assert_eq!(names.len(), 7, "{names:?}");
}

#[test]
fn the_dry_run_names_blocked_groups_and_sizes_small_ones() {
    let h = home();
    every_class(&h);
    let text = h.ok(&["--blocked", "--too-small", "--below", "1000"]);
    assert!(text.contains("alt.binaries.pictures.erotica.teen: 1 releases, 1000 bytes"), "{text}");
    assert!(text.contains("at most 64 KiB: 1, 256 KiB: 1, 1 MiB: 1"), "{text}");
}

#[test]
fn search_and_totals_are_right_after_it() {
    let h = home();
    every_class(&h);
    let conn = h.shard();
    let (good, dead): (i64, i64) = conn
        .query_row(
            "select (select id from releases where name = 'Good.One'), (select id from releases where name = 'Dead.One')",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    conn.execute("insert or replace into pp_state (release_id) values (?)", [dead]).unwrap();
    let nzb = |id: i64| store::articles(&db::open_with_shards(&h.main).unwrap(), id).unwrap();
    let kept = nzb(good);
    h.ok(&["--apply", "--dead"]);
    let hits: i64 =
        conn.query_row("select count(*) from releases_fts where releases_fts match 'Dead'", [], |r| r.get(0)).unwrap();
    assert_eq!(hits, 0);
    h.totals_match();
    for table in [
        "files where release_id",
        "release_ids where release_id",
        "pp_state where release_id",
        "pp_queue where release_id",
    ] {
        let left: i64 = conn.query_row(&format!("select count(*) from {table} = ?"), [dead], |r| r.get(0)).unwrap();
        assert_eq!(left, 0, "{table}");
    }
    assert_eq!(nzb(good), kept);
}

fn many_fragments(h: &Home, n: usize) {
    let releases: Vec<Release> = (0..n).map(fragment).collect();
    store::save(&h.main, &releases).unwrap();
}

#[test]
fn a_stopped_purge_resumes_without_counting_twice() {
    let h = home();
    many_fragments(&h, 5_000);
    let out = h.run(&["--apply"], &[("ATLAS_PURGE_STOP_AFTER", "2")]);
    assert!(out.status.success());
    let left = h.names().len();
    assert!(left > 0 && left < 5_000, "{left}");
    h.totals_match();
    h.ok(&["--apply"]);
    assert_eq!(h.names().len(), 0);
    h.totals_match();
    assert_eq!(store::get_meta(&h.shard(), "purge_after").unwrap(), None);
}

#[test]
fn other_classes_start_over() {
    let h = home();
    many_fragments(&h, 5_000);
    assert!(h.run(&["--apply"], &[("ATLAS_PURGE_STOP_AFTER", "1")]).status.success());
    let text = h.ok(&["--apply", "--fragments", "--dead"]);
    assert!(text.contains("starting over"), "{text}");
    assert_eq!(h.names().len(), 0);
}

#[test]
fn a_huge_release_goes_in_chunks_and_is_finished_first() {
    let h = home();
    store::save(&h.main, &[whole("Huge.One", TOKEN, 5_000, 5_000)]).unwrap();
    // a fragment it isnt, and it wants --dead with its parts gone: take it by a class that matches, then stop
    h.shard().execute("update releases set complete = 0", []).unwrap();
    h.shard().execute("update files set seen = x''", []).unwrap();
    let out = h.run(&["--apply", "--dead"], &[("ATLAS_PURGE_CHUNK", "1000"), ("ATLAS_PURGE_STOP_AFTER", "2")]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));
    let conn = h.shard();
    assert!(store::get_meta(&conn, "purge_victim").unwrap().is_some());
    assert_eq!(store::get_meta(&conn, "articles").unwrap().unwrap(), store::article_count(&conn, "main").unwrap());
    h.ok(&["--apply", "--image-only"]);
    assert_eq!(h.names().len(), 0, "finished though --image-only wouldnt take it");
    assert_eq!(store::get_meta(&conn, "purge_victim").unwrap(), None);
    h.totals_match();
}

#[test]
fn big_batches_commit_in_pieces() {
    let h = home();
    let big: Vec<Release> = (0..6).map(|i| whole(&format!("Big.{i}"), "big.jpg", 90_000, 90_000)).collect();
    store::save(&h.main, &big).unwrap();
    let text = h.ok(&["--apply", "--image-only"]);
    let commits: i64 = text
        .lines()
        .find_map(|l| l.strip_prefix("deleted 6 releases in ")?.strip_suffix(" commits")?.parse().ok())
        .unwrap_or_else(|| panic!("{text}"));
    assert!(commits >= 3, "{commits}");
    h.totals_match();
    let conn = h.shard();
    assert_eq!(store::get_meta(&conn, "articles").unwrap().unwrap(), 0);
}

#[test]
fn a_reader_holding_the_wal_stops_it_cleanly() {
    let h = home();
    many_fragments(&h, 500);
    let reader = h.shard();
    reader.execute_batch("begin; select count(*) from releases").unwrap();
    let out = h.run(&["--apply"], &[("ATLAS_PURGE_CHECK_EVERY", "1"), ("ATLAS_PURGE_WAL_LIMIT", "1")]);
    assert!(!out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("a reader is holding the WAL"), "{text}");
    reader.execute_batch("commit").unwrap();
    assert!(store::get_meta(&h.shard(), "purge_after").unwrap().is_some(), "the committed cursor is there");
    h.ok(&["--apply"]);
    assert_eq!(h.names().len(), 0);
}

#[test]
fn apply_is_refused_while_anything_holds_the_lock() {
    let h = home();
    every_class(&h);
    let before = h.names();
    let guard = atlas::compact::hold_off_compaction(&h.main).unwrap();
    assert!(!h.run(&["--apply"], &[]).status.success());
    assert_eq!(h.names(), before);
    drop(guard);
}

#[test]
#[cfg(unix)]
fn apply_is_refused_with_an_indexer_alive() {
    let h = home();
    every_class(&h);
    let before = h.names();
    let _indexer = common::fake_indexer(h.dir.path());
    let out = h.run(&["--apply"], &[]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("stop indexing first"));
    assert_eq!(h.names(), before);
}

fn shards_bytes(main: &Path) -> u64 {
    store::shard_paths(main).iter().map(|p| std::fs::metadata(p).unwrap().len()).sum()
}

#[test]
fn then_compact_shrinks_it() {
    let h = home();
    many_fragments(&h, 2_000);
    h.ok(&["--apply"]);
    let before = shards_bytes(&h.main);
    let out = Command::new(env!("CARGO_BIN_EXE_atlas"))
        .arg("--compact")
        .env("ATLAS_HOME", h.dir.path())
        .env("ATLAS_NO_KEYRING", "1")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));
    assert!(shards_bytes(&h.main) < before);
}
