//! `atlas --reflag-obfuscated`, through the binary.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use atlas::parser::Release;
use atlas::{db, store};
use common::stage::{STAGE_GROUP, release_of};
use rusqlite::{Connection, params};

struct Home {
    dir: tempfile::TempDir,
    main: PathBuf,
}

fn home() -> Home {
    let dir = tempfile::tempdir().unwrap();
    let main = dir.path().join("atlas.db");
    db::create_db_at(&main).unwrap();
    Home { dir, main }
}

fn release(name: &str) -> Release {
    release_of(name, "x.bin", 2, 2)
}

impl Home {
    fn run(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        let mut c = Command::new(env!("CARGO_BIN_EXE_atlas"));
        c.arg("--reflag-obfuscated").args(args).env("ATLAS_HOME", self.dir.path()).env("ATLAS_NO_KEYRING", "1");
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
        db::open_shard(&store::shard_path(&self.main, store::shard_of(STAGE_GROUP))).unwrap()
    }

    /// as an old build left them: flagged by the old rule only
    fn saved(&self, releases: &[Release]) {
        store::save(&self.main, releases).unwrap();
        self.shard()
            .execute(
                "update releases set is_obfuscated = name glob '*[0-9]*' and length(name) = 16 and name not like '%.%'",
                [],
            )
            .unwrap();
    }

    fn flag(&self, name: &str) -> i64 {
        self.shard().query_row("select is_obfuscated from releases where name = ?", [name], |r| r.get(0)).unwrap()
    }

    fn snapshot(&self) -> Vec<(String, i64)> {
        let conn = self.shard();
        let mut stmt = conn.prepare("select name, is_obfuscated from releases order by id").unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().map(Result::unwrap).collect()
    }

    fn meta_keys(&self) -> Vec<String> {
        let conn = self.shard();
        let mut stmt = conn.prepare("select key from meta order by key").unwrap();
        stmt.query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect()
    }
}

fn four() -> Vec<Release> {
    ["a1B2c3D4e5F6g7H8", "kP93xQ7rL.ZMtwBfa", "Xk3Mz9PqL", "Good.Name.2024"].map(release).to_vec()
}

#[test]
fn a_dry_run_changes_nothing_and_counts() {
    let h = home();
    h.saved(&four());
    let (rows, meta) = (h.snapshot(), h.meta_keys());
    let guard = atlas::compact::hold_off_compaction(&h.main).unwrap();
    let text = h.ok(&["--sample", "1"]);
    drop(guard);
    assert!(text.contains("2 would be flagged"), "{text}");
    assert!(text.contains("  kP93xQ7rL.ZMtwBfa") || text.contains("  Xk3Mz9PqL"), "{text}");
    assert_eq!(h.snapshot(), rows);
    assert_eq!(h.meta_keys(), meta);
}

#[test]
fn apply_sets_the_flag_and_search_follows() {
    let h = home();
    let mut rows = four();
    rows.push(release("hQ72vB9nK [12345678]"));
    h.saved(&rows);
    let text = h.ok(&["--apply"]);
    assert!(text.contains("3 flagged"), "{text}");
    for name in ["kP93xQ7rL.ZMtwBfa", "Xk3Mz9PqL", "hQ72vB9nK [12345678]"] {
        assert_eq!(h.flag(name), 1, "{name}");
    }
    assert_eq!(h.flag("Good.Name.2024"), 0);
    assert_eq!(h.meta_keys().iter().filter(|k| *k == "reflag_after").count(), 0);
}

#[test]
fn a_named_row_stays_visible() {
    let h = home();
    h.saved(&[Release { display_name: Some("Real.Name.2024".into()), ..release("Xk3Mz9PqL") }]);
    h.ok(&["--apply"]);
    assert_eq!(h.flag("Xk3Mz9PqL"), 1);
    let visible: i64 = h
        .shard()
        .query_row("select count(*) from releases where is_obfuscated = 1 and display_name is null", [], |r| r.get(0))
        .unwrap();
    assert_eq!(visible, 0, "search shows a flagged release that has a real name");
}

#[test]
fn a_step_named_row_is_left_alone() {
    let h = home();
    h.saved(&[release("Xk3Mz9PqL")]);
    h.shard().execute("update releases set name_source = 15, display_name = 'Real.Name.2024'", []).unwrap();
    h.ok(&["--apply"]);
    assert_eq!(h.flag("Xk3Mz9PqL"), 0);
}

#[test]
fn a_second_run_finds_nothing() {
    let h = home();
    h.saved(&four());
    h.ok(&["--apply"]);
    assert!(h.ok(&[]).contains("0 would be flagged"));
}

fn many(h: &Home, n: usize) {
    let rows: Vec<Release> = (0..n).map(|i| release(&format!("kP93xQ7rL.ZMtw{i:05}a"))).collect();
    h.saved(&rows);
}

#[test]
fn a_stopped_run_resumes() {
    let h = home();
    many(&h, 2_500);
    let out = h.run(&["--apply"], &[("ATLAS_REFLAG_STOP_AFTER", "1")]);
    assert!(!out.status.success());
    assert!(h.meta_keys().iter().any(|k| k == "reflag_after"), "the cursor is committed");
    let flagged: i64 = h.shard().query_row("select sum(is_obfuscated) from releases", [], |r| r.get(0)).unwrap();
    assert_eq!(flagged, 2_000);
    h.ok(&["--apply"]);
    assert!(!h.meta_keys().iter().any(|k| k == "reflag_after"));
    let flagged: i64 = h.shard().query_row("select sum(is_obfuscated) from releases", [], |r| r.get(0)).unwrap();
    assert_eq!(flagged, 2_500);
}

#[test]
fn apply_is_refused_while_anything_holds_the_lock() {
    let h = home();
    h.saved(&four());
    let before = h.snapshot();
    let guard = atlas::compact::hold_off_compaction(&h.main).unwrap();
    assert!(!h.run(&["--apply"], &[]).status.success());
    assert_eq!(h.snapshot(), before);
    drop(guard);
}

#[test]
#[cfg(unix)]
fn it_is_refused_with_an_indexer_alive() {
    let h = home();
    h.saved(&four());
    let before = h.snapshot();
    let _indexer = common::fake_indexer(h.dir.path());
    let out = h.run(&["--apply"], &[]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("stop indexing first"));
    assert_eq!(h.snapshot(), before);
}

#[test]
fn a_reader_holding_the_wal_stops_it_cleanly() {
    let h = home();
    many(&h, 500);
    let reader = h.shard();
    reader.execute_batch("begin; select count(*) from releases").unwrap();
    let out = h.run(&["--apply"], &[("ATLAS_REFLAG_CHECK_EVERY", "1"), ("ATLAS_REFLAG_WAL_LIMIT", "1")]);
    assert!(!out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("a reader is holding the WAL"), "{text}");
    reader.execute_batch("commit").unwrap();
    assert!(h.meta_keys().iter().any(|k| k == "reflag_after"));
    h.ok(&["--apply"]);
    assert!(!h.meta_keys().iter().any(|k| k == "reflag_after"));
}

/// the pre-shard layout `tests/convert.rs` converts
fn old_database(path: &Path) {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(
        "pragma journal_mode = wal;
        create table releases (
            id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT, group_name TEXT, poster TEXT, posted_date TEXT,
            size INTEGER, complete INTEGER, parts INTEGER, file_total INTEGER, display_name TEXT,
            is_obfuscated INTEGER default 0
        );
        create table articles (
            id INTEGER PRIMARY KEY AUTOINCREMENT, release_id INTEGER, message_id TEXT, subject TEXT, filename TEXT,
            part INTEGER, total_parts INTEGER, bytes INTEGER, file_total INTEGER,
            unique(release_id, message_id)
        );
        create table groups(name TEXT PRIMARY KEY, live_cursor INTEGER, backfill_cursor INTEGER,
            first_article INTEGER, last_article INTEGER);",
    )
    .unwrap();
    conn.execute(
        "insert into releases (id, name, group_name, poster, posted_date, size, complete, parts, file_total)
         values (3, 'kP93xQ7rL.ZMtwBfa', ?, 'p <p@x>', '2026-10-02 10:11:12', 1000, 1, 1, 1)",
        params![STAGE_GROUP],
    )
    .unwrap();
    conn.execute(
        "insert into articles (release_id, message_id, subject, filename, part, total_parts, bytes, file_total)
         values (3, '<a@x>', '\"x.bin\" yEnc (1/1)', 'x.bin', 1, 1, 1000, 1)",
        [],
    )
    .unwrap();
}

#[test]
fn reflag_runs_after_a_convert() {
    let dir = tempfile::tempdir().unwrap();
    let main = dir.path().join("atlas.db");
    old_database(&main);
    db::create_db_at(&main).unwrap();
    atlas::convert::run(&main, &|_| {}).unwrap();
    let h = Home { dir, main };
    assert_eq!(h.flag("kP93xQ7rL.ZMtwBfa"), 0, "copied as it was");
    assert!(h.ok(&[]).contains("1 would be flagged"));
    h.ok(&["--apply"]);
    assert_eq!(h.flag("kP93xQ7rL.ZMtwBfa"), 1);
}

#[test]
fn help_lists_it() {
    let out = Command::new(env!("CARGO_BIN_EXE_atlas")).arg("--help").output().unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).contains("--reflag-obfuscated"));
}
