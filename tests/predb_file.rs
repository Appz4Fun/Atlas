//! The PreDB file sits next to the shards, and nothing that rewrites them touches it.

use std::path::Path;
use std::process::Command;

use atlas::config::Config;
use atlas::predb::db::{create, path_for};
use atlas::predb::import::{Entry, import};
use atlas::{db, store};

fn home() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let main = dir.path().join("atlas.db");
    db::create_db_at(&main).unwrap();
    (dir, main)
}

fn with_a_title(path: &Path) {
    let mut conn = create(path).unwrap();
    let entry = Entry { title: "Alpha.Beta.Title-GRP".into(), ..Default::default() };
    import(&mut conn, 1, &[entry], 5, None).unwrap();
    conn.query_row("pragma wal_checkpoint(truncate)", [], |_| Ok(())).unwrap();
}

#[test]
fn the_path_sits_next_to_the_shards() {
    assert_eq!(path_for(Path::new("/x/atlas.db")), Path::new("/x/atlas.predb.db"));
}

#[test]
fn create_is_idempotent() {
    let (dir, main) = home();
    let path = path_for(&main);
    with_a_title(&path);
    let conn = create(&path).unwrap();
    let titles: i64 = conn.query_row("select count(*) from titles", [], |r| r.get(0)).unwrap();
    assert_eq!(titles, 1);
    let mode: String = conn.query_row("pragma journal_mode", [], |r| r.get(0)).unwrap();
    assert_eq!(mode, "wal");
    drop(dir);
}

#[test]
fn no_file_by_default() {
    let (_dir, main) = home();
    assert!(!Config::default().predb.enabled);
    assert!(!path_for(&main).exists());
}

#[test]
fn a_wipe_keeps_the_titles() {
    let (_dir, main) = home();
    let path = path_for(&main);
    with_a_title(&path);
    let before = std::fs::read(&path).unwrap();
    db::wipe(&main).unwrap();
    assert!(store::shard_paths(&main).iter().all(|s| !s.exists()));
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[test]
fn compaction_leaves_the_predb_file_alone() {
    let (dir, main) = home();
    let path = path_for(&main);
    with_a_title(&path);
    let (before, mtime) = (std::fs::read(&path).unwrap(), std::fs::metadata(&path).unwrap().modified().unwrap());
    let out = Command::new(env!("CARGO_BIN_EXE_atlas"))
        .arg("--compact")
        .env("ATLAS_HOME", dir.path())
        .env("ATLAS_NO_KEYRING", "1")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), mtime);
    for suffix in ["atlas.predb.compact.db", "atlas.predb.precompact.db"] {
        assert!(!dir.path().join(suffix).exists(), "{suffix}");
    }
}

#[test]
fn conversion_leaves_the_predb_file_alone() {
    let dir = tempfile::tempdir().unwrap();
    let main = dir.path().join("atlas.db");
    let conn = rusqlite::Connection::open(&main).unwrap();
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
            first_article INTEGER, last_article INTEGER);
        insert into releases (id, name, group_name, size, complete, parts, file_total)
            values (3, 'Release.One', 'alt.binaries.a', 10, 1, 1, 1);
        insert into articles (release_id, message_id, subject, filename, part, total_parts, bytes, file_total)
            values (3, '<a@x>', 'a', 'x.bin', 1, 1, 10, 1);",
    )
    .unwrap();
    drop(conn);
    db::create_db_at(&main).unwrap();
    let path = path_for(&main);
    with_a_title(&path);
    let before = std::fs::read(&path).unwrap();
    atlas::convert::run(&main, &|_| {}).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), before);
}
