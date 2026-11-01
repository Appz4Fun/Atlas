//! `atlas --unname`, through the binary.

mod common;

use std::path::PathBuf;
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
        c.arg("--unname").args(args).env("ATLAS_HOME", self.dir.path()).env("ATLAS_NO_KEYRING", "1");
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

    fn name_of(&self, name: &str) -> (Option<String>, Option<i64>, i64) {
        self.shard()
            .query_row("select display_name, name_source, is_obfuscated from releases where name = ?", [name], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .unwrap()
    }

    fn meta_keys(&self) -> Vec<String> {
        let conn = self.shard();
        let mut stmt = conn.prepare("select key from meta order by key").unwrap();
        stmt.query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect()
    }

    fn snapshot(&self) -> Vec<(String, Option<String>, Option<i64>)> {
        let conn = self.shard();
        let mut stmt = conn.prepare("select name, display_name, name_source from releases order by id").unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap().map(Result::unwrap).collect()
    }
}

/// rows named by each source, one inline, and one unnamed
fn every_source(h: &Home) {
    let names = ["src3", "src5", "src10", "src15", "src20", "src100", "inline", "plain"];
    let rows: Vec<Release> = names.map(release).to_vec();
    store::save(&h.main, &rows).unwrap();
    let conn = h.shard();
    for (name, source) in [("src3", 3), ("src5", 5), ("src10", 10), ("src15", 15), ("src20", 20), ("src100", 100)] {
        conn.execute(
            "update releases set display_name = ?, name_source = ? where name = ?",
            params![format!("Named.By.{source}.2024"), source, name],
        )
        .unwrap();
    }
    conn.execute("update releases set display_name = 'Inline.Named.2020' where name = 'inline'", []).unwrap();
}

#[test]
fn a_dry_run_changes_nothing_and_counts() {
    let h = home();
    every_source(&h);
    h.shard().execute("insert into meta (key, value) values ('naming_rename_named', 1)", []).unwrap();
    let (rows, meta) = (h.snapshot(), h.meta_keys());
    let guard = atlas::compact::hold_off_compaction(&h.main).unwrap();
    let text = h.ok(&["--sample", "1"]);
    drop(guard);
    assert!(text.contains("2 would be cleared"), "{text}");
    assert!(text.contains("1 from posted files (source 3)"), "{text}");
    assert!(text.contains("1 from naming regexes (source 5)"), "{text}");
    assert!(text.contains("  Named.By."), "{text}");
    assert_eq!(h.snapshot(), rows);
    assert_eq!(h.meta_keys(), meta);
}

#[test]
fn apply_clears_only_regex_and_posted_names() {
    let h = home();
    every_source(&h);
    assert!(h.ok(&["--apply"]).contains("2 cleared"));
    for name in ["src3", "src5"] {
        let (display, source, _) = h.name_of(name);
        assert_eq!((display, source), (None, Some(1)), "{name}");
    }
    for (name, source) in [("src10", 10), ("src15", 15), ("src20", 20), ("src100", 100)] {
        assert_eq!(h.name_of(name).1, Some(source));
    }
    assert_eq!(h.name_of("inline").0.as_deref(), Some("Inline.Named.2020"));
    assert_eq!(h.name_of("plain").0, None);
}

#[test]
fn search_follows() {
    let h = home();
    store::save(&h.main, &["kP93xQ7rL.ZMtwBfa", "Good.Release.2024", "Xk3Mz9PqL [12345678]"].map(release)).unwrap();
    let conn = h.shard();
    conn.execute("update releases set display_name = 'Clearedword.Name.2024', name_source = 5, is_obfuscated = 0", [])
        .unwrap();
    h.ok(&["--apply"]);
    let fts = |word: &str| -> i64 {
        conn.query_row("select count(*) from releases_fts where releases_fts match ?", [word], |r| r.get(0)).unwrap()
    };
    assert_eq!(fts("Clearedword"), 0);
    assert_eq!(fts("Good"), 1);
    assert_eq!(h.name_of("kP93xQ7rL.ZMtwBfa").2, 1, "a random name leaves search");
    assert_eq!(h.name_of("Xk3Mz9PqL [12345678]").2, 1, "with Part 2's suffix too");
    assert_eq!(h.name_of("Good.Release.2024").2, 0, "a real one stays");
}

#[test]
fn it_removes_the_opt_in() {
    let h = home();
    every_source(&h);
    let conn = h.shard();
    conn.execute("insert into meta (key, value) values ('naming_rename_named', 1)", []).unwrap();
    conn.execute("insert into meta (key, value) values ('naming_rename_named:alt.binaries.t', 1)", []).unwrap();
    h.ok(&["--apply"]);
    assert!(!h.meta_keys().iter().any(|k| k.starts_with("naming_rename_named")));
}

#[test]
fn naming_keeps_its_bit_and_failed_later_steps_clear() {
    let h = home();
    every_source(&h);
    let conn = h.shard();
    let id: i64 = conn.query_row("select id from releases where name = 'src5'", [], |r| r.get(0)).unwrap();
    let naming = 1i64 << 8;
    conn.execute(
        "insert into pp_state (release_id, done, failed, db_attempts) values (?, ?, 0, 2)",
        params![id, naming],
    )
    .unwrap();
    h.ok(&["--apply"]);
    let (done, attempts): (i64, i64) = conn
        .query_row("select done, db_attempts from pp_state where release_id = ?", [id], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap();
    assert_ne!(done & naming, 0, "the name stays cleared");
    assert_eq!(attempts, 0);
}

#[test]
fn a_stopped_run_resumes() {
    let h = home();
    let rows: Vec<Release> = (0..2_500).map(|i| release(&format!("Named{i:05}"))).collect();
    store::save(&h.main, &rows).unwrap();
    h.shard().execute("update releases set display_name = 'A.Name.2024', name_source = 5", []).unwrap();
    let out = h.run(&["--apply"], &[("ATLAS_UNNAME_STOP_AFTER", "1")]);
    assert!(!out.status.success());
    assert!(h.meta_keys().iter().any(|k| k == "unname_after"));
    h.ok(&["--apply"]);
    assert!(!h.meta_keys().iter().any(|k| k == "unname_after"));
    let left: i64 = h.shard().query_row("select count(display_name) from releases", [], |r| r.get(0)).unwrap();
    assert_eq!(left, 0);
}

#[test]
fn apply_is_refused_while_anything_holds_the_lock() {
    let h = home();
    every_source(&h);
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
    every_source(&h);
    let before = h.snapshot();
    let _indexer = common::fake_indexer(h.dir.path());
    let out = h.run(&["--apply"], &[]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("stop indexing first"));
    assert_eq!(h.snapshot(), before);
}

#[test]
fn a_second_run_finds_nothing() {
    let h = home();
    every_source(&h);
    h.ok(&["--apply"]);
    assert!(h.ok(&[]).contains("0 would be cleared"));
}

#[test]
fn help_lists_it() {
    let out = Command::new(env!("CARGO_BIN_EXE_atlas")).arg("--help").output().unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).contains("--unname"));
}

#[test]
fn archive_nfo_is_refused_with_the_list() {
    let h = home();
    let out = Command::new(env!("CARGO_BIN_EXE_atlas"))
        .args(["--reprocess", "--step", "archive_nfo"])
        .env("ATLAS_HOME", h.dir.path())
        .env("ATLAS_NO_KEYRING", "1")
        .output()
        .unwrap();
    assert!(!out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("the steps are") && text.contains(" nfo"), "{text}");
}
