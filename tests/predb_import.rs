//! `atlas --predb-import`, through the binary.

mod common;

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output};

use atlas::predb::db::{create, path_for};
use atlas::predb::file_import::{ImportLimits, import_lines};
use rusqlite::Connection;

struct Home {
    dir: tempfile::TempDir,
    main: PathBuf,
}

fn home() -> Home {
    let dir = tempfile::tempdir().unwrap();
    let main = dir.path().join("atlas.db");
    atlas::db::create_db_at(&main).unwrap();
    Home { dir, main }
}

impl Home {
    fn run(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        let mut c = Command::new(env!("CARGO_BIN_EXE_atlas"));
        c.arg("--predb-import").args(args).env("ATLAS_HOME", self.dir.path()).env("ATLAS_NO_KEYRING", "1");
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

    fn file(&self, name: &str, bytes: &[u8]) -> String {
        let path = self.dir.path().join(name);
        std::fs::write(&path, bytes).unwrap();
        path.to_string_lossy().into_owned()
    }

    fn gz(&self, name: &str, bytes: &[u8]) -> String {
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        enc.write_all(bytes).unwrap();
        self.file(name, &enc.finish().unwrap())
    }

    fn predb(&self) -> Connection {
        create(&path_for(&self.main)).unwrap()
    }

    fn titles(&self) -> i64 {
        self.predb().query_row("select count(*) from titles", [], |r| r.get(0)).unwrap()
    }
}

const THREE: &str =
    "Alpha.Beta.Title-GRP\nGamma.Delta.Title-GRP\tTV\t1700000000\nshort\nbad/slash.title-GRP\nEpsilon.Zeta.Title-GRP\n";

#[test]
fn a_dry_run_counts_and_writes_nothing() {
    let h = home();
    let file = h.file("list.txt", THREE.as_bytes());
    let text = h.ok(&[&file]);
    assert!(text.contains("3 valid, 2 invalid"), "{text}");
    assert!(!path_for(&h.main).exists());
}

#[test]
fn apply_imports_plain_and_gzip() {
    let h = home();
    let plain = h.file("list.txt", THREE.as_bytes());
    assert!(h.ok(&[&plain, "--apply"]).contains("3 inserted"));
    let row: (i64, Option<String>, Option<i64>) = h
        .predb()
        .query_row("select source, category, pre_at from titles where title = 'Gamma.Delta.Title-GRP'", [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .unwrap();
    assert_eq!(row, (100, Some("TV".into()), Some(1_700_000_000)));
    assert!(h.ok(&[&plain, "--apply"]).contains("0 inserted"));

    let h = home();
    let zipped = h.gz("list.txt.gz", THREE.as_bytes());
    assert!(h.ok(&[&zipped, "--apply"]).contains("3 inserted"));
}

#[test]
fn a_bulk_import_skips_the_late_queue_and_says_so() {
    let h = home();
    let file = h.file("list.txt", THREE.as_bytes());
    let text = h.ok(&[&file, "--apply"]);
    assert!(text.contains("atlas --reprocess --step predb_match --apply"), "{text}");
    let cursor = |c: &Connection| -> i64 {
        c.query_row("select value from meta where key = 'late_after_id'", [], |r| r.get(0)).unwrap()
    };
    assert_eq!(cursor(&h.predb()), 3);

    // behind: it stays
    h.predb().execute("update meta set value = 1 where key = 'late_after_id'", []).unwrap();
    let more = h.file("more.txt", b"Another.New.Title-GRP\n");
    h.ok(&[&more, "--apply"]);
    assert_eq!(cursor(&h.predb()), 1);
}

#[test]
fn a_big_file_commits_in_batches() {
    let h = home();
    let lines: String = (0..25).map(|i| format!("Numbered.Title.N{i:03}-GRP\n")).collect();
    let file = h.file("list.txt", lines.as_bytes());
    let out = h.run(&[&file, "--apply"], &[("ATLAS_PREDB_IMPORT_BATCH", "10")]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("25 inserted, 0 updated, 3 batches"), "{text}");
    assert!(h.ok(&[&file, "--apply"]).contains("0 inserted"));
    assert_eq!(h.titles(), 25);
}

#[test]
#[cfg(unix)]
fn it_is_refused_with_an_indexer_alive() {
    let h = home();
    let file = h.file("list.txt", THREE.as_bytes());
    let _indexer = common::fake_indexer(h.dir.path());
    let out = h.run(&[&file, "--apply"], &[]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("stop indexing first"));
    assert!(!path_for(&h.main).exists());
}

#[test]
fn a_missing_file_is_exit_1() {
    let h = home();
    assert!(!h.run(&["/no/such/file.txt"], &[]).status.success());
}

#[test]
fn a_line_with_no_newline_is_skipped_and_counted() {
    let h = home();
    let mut text = vec![b'x'; 8 << 20];
    text.extend_from_slice(b"\nGood.Title.Here-GRP\n");
    let file = h.gz("big.txt.gz", &text);
    let out = h.ok(&[&file, "--apply"]);
    assert!(out.contains("1 too long"), "{out}");
    assert_eq!(h.titles(), 1);
}

#[test]
fn exactly_the_line_cap_is_kept() {
    let h = home();
    let mut text = vec![b'a'; 1_023];
    text.push(b'\n');
    text.extend(vec![b'a'; 1_025]);
    text.push(b'\n');
    let file = h.file("cap.txt", &text);
    let out = h.ok(&[&file]);
    assert!(out.contains("0 valid, 1 invalid, 1 too long"), "{out}");
}

#[test]
fn a_gzip_bomb_stops_at_the_byte_cap() {
    let h = home();
    let mut text = b"First.Good.Title-GRP\n".to_vec();
    text.extend(vec![b'x'; 8 << 20]);
    let file = h.gz("bomb.txt.gz", &text);
    let out =
        h.run(&[&file, "--apply"], &[("ATLAS_PREDB_IMPORT_MAX_BYTES", "65536"), ("ATLAS_PREDB_IMPORT_BATCH", "1")]);
    assert!(!out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("the cap of 65536 decompressed bytes"), "{text}");
    assert_eq!(h.titles(), 1, "batches already committed stay");
}

#[test]
fn the_byte_cap_inside_a_line_stores_no_part_of_it() {
    let h = home();
    let file = h.file("cut.txt", b"First.Whole.Title-GRP\nAlpha.Beta.Gamma-GRP\n");
    let out = h.run(&[&file, "--apply"], &[("ATLAS_PREDB_IMPORT_MAX_BYTES", "32"), ("ATLAS_PREDB_IMPORT_BATCH", "1")]);
    assert!(!out.status.success());
    let stored: Vec<String> = {
        let conn = h.predb();
        let mut stmt = conn.prepare("select title from titles").unwrap();
        stmt.query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect()
    };
    assert_eq!(stored, ["First.Whole.Title-GRP"]);
}

#[test]
fn too_many_lines_stops() {
    let h = home();
    let lines: String = (0..50).map(|i| format!("Numbered.Title.N{i:03}-GRP\n")).collect();
    let file = h.file("list.txt", lines.as_bytes());
    let out = h.run(&[&file, "--apply"], &[("ATLAS_PREDB_IMPORT_MAX_LINES", "10")]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("the cap of 10 lines"));
    assert_eq!(h.titles(), 10);
}

#[test]
fn a_big_import_keeps_its_wal_small() {
    let h = home();
    let path = path_for(&h.main);
    let mut conn = create(&path).unwrap();
    let autocheckpoint: i64 = conn.query_row("pragma wal_autocheckpoint", [], |r| r.get(0)).unwrap();
    assert!(autocheckpoint > 0);
    let lines: String = (0..100_000).map(|i| format!("Numbered.Title.N{i:06}-GRP\n")).collect();
    let wal = format!("{}-wal", path.display());
    let limits = ImportLimits { max_bytes: u64::MAX, max_lines: u64::MAX, batch: 10_000 };
    let mut biggest = 0;
    let (_, stats) = import_lines(lines.as_bytes(), Some(&mut conn), &limits, 5, &mut |_| {
        biggest = biggest.max(std::fs::metadata(&wal).map(|m| m.len()).unwrap_or(0));
    })
    .unwrap();
    assert_eq!(stats.counts.inserted, 100_000);
    assert!(biggest < 64 << 20, "{biggest}");
}

#[test]
fn a_bad_utf8_line_is_invalid() {
    let h = home();
    let file = h.file("bad.txt", b"\xff\xfe.bad.bytes-GRP\nGood.Title.Here-GRP\n");
    let out = h.ok(&[&file]);
    assert!(out.contains("1 valid, 1 invalid"), "{out}");
}

#[test]
fn the_import_path_has_no_http() {
    let source = include_str!("../src/predb/file_import.rs");
    assert!(!source.contains("ureq") && !source.contains("poll::"));
}

#[test]
fn help_lists_it() {
    let out = Command::new(env!("CARGO_BIN_EXE_atlas")).arg("--help").output().unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).contains("--predb-import"));
}
