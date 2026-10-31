//! The tables archive inspection stores in, and how they read back.

use atlas::archive::Password;
use atlas::archive::store::{
    self, DELETE_SOURCE, INSERT_FILE, InnerFileRow, SOURCE_PAR2, SOURCE_RAR, UPSERT_RELEASE, inner_files, password,
    passwords_with, view_with,
};
use atlas::postprocess::schema::SHARD_TABLES;
use atlas::{db, store as shards};
use rusqlite::{Connection, params};

fn database() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let main = dir.path().join("atlas.db");
    db::create_db_at(&main).unwrap();
    (dir, main)
}

fn shard(main: &std::path::Path, i: usize) -> Connection {
    db::open_shard(&shards::shard_path(main, i)).unwrap()
}

fn file(conn: &Connection, release: i64, seq: i64, name: &str, size: Option<i64>, source: i64) {
    conn.execute(INSERT_FILE, params![release, seq, name, size, source]).unwrap();
}

/// an id that lands in shard `i`
fn id_in(i: usize) -> i64 {
    (1..1_000).find(|id| shards::shard_of_id(*id) == i).unwrap()
}

#[test]
fn both_tables_are_registered() {
    for name in [store::INNER_FILES.name, store::RELEASE.name] {
        let table = SHARD_TABLES.iter().find(|t| t.name == name).unwrap();
        assert_eq!(table.release_key, Some("release_id"), "{name}");
    }
}

#[test]
fn reads_come_back_in_seq_order() {
    let (_dir, main) = database();
    let conn = shard(&main, 0);
    file(&conn, 7, SOURCE_RAR * 65_536, "b.mkv", Some(5), SOURCE_RAR);
    file(&conn, 7, SOURCE_PAR2 * 65_536, "a.mkv", None, SOURCE_PAR2);
    let rows = inner_files(&conn, 7).unwrap();
    assert_eq!(
        rows,
        vec![
            InnerFileRow { seq: SOURCE_PAR2 * 65_536, name: "a.mkv".into(), size: None, source: SOURCE_PAR2 },
            InnerFileRow { seq: SOURCE_RAR * 65_536, name: "b.mkv".into(), size: Some(5), source: SOURCE_RAR },
        ]
    );

    assert_eq!(password(&conn, 7).unwrap(), None);
    for (code, want) in [(0, Password::No), (1, Password::Yes), (2, Password::Maybe)] {
        conn.execute(UPSERT_RELEASE, params![7, code]).unwrap();
        assert_eq!(password(&conn, 7).unwrap(), Some(want));
    }
}

#[test]
fn a_source_replaces_only_its_own_rows() {
    let (_dir, main) = database();
    let conn = shard(&main, 0);
    file(&conn, 7, SOURCE_PAR2 * 65_536, "a.mkv", None, SOURCE_PAR2);
    file(&conn, 7, SOURCE_RAR * 65_536, "old.mkv", None, SOURCE_RAR);
    conn.execute(DELETE_SOURCE, params![7, SOURCE_RAR]).unwrap();
    file(&conn, 7, SOURCE_RAR * 65_536, "new.mkv", None, SOURCE_RAR);
    let names: Vec<String> = inner_files(&conn, 7).unwrap().into_iter().map(|r| r.name).collect();
    assert_eq!(names, ["a.mkv", "new.mkv"]);
}

#[test]
fn passwords_read_across_shards() {
    let (_dir, main) = database();
    let (a, b, none) = (id_in(0), id_in(5), id_in(3));
    shard(&main, 0).execute(UPSERT_RELEASE, params![a, 1]).unwrap();
    let five = shard(&main, 5);
    five.execute(UPSERT_RELEASE, params![b, 2]).unwrap();
    file(&five, b, 131_072, "x.mkv", Some(9), SOURCE_RAR);
    drop(five);

    let conn = db::open_with_shards(&main).unwrap();
    let got = passwords_with(&conn, &[a, b, none]);
    assert_eq!(got.len(), 2);
    assert_eq!(got[&a], Password::Yes);
    assert_eq!(got[&b], Password::Maybe);

    let view = view_with(&conn, b);
    assert_eq!(view.password, Some(Password::Maybe));
    assert_eq!(view.files.len(), 1);
    assert_eq!(view_with(&conn, none), Default::default());
}

#[test]
fn an_old_database_reads_as_nothing_known() {
    let (_dir, main) = database();
    let a = id_in(0);
    shard(&main, 0).execute_batch("drop table archive_release").unwrap();
    let conn = db::open_with_shards(&main).unwrap();
    assert!(passwords_with(&conn, &[a]).is_empty());
    assert_eq!(view_with(&conn, a).password, None);
}
