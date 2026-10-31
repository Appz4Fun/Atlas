//! What opening a shard does for the processing stage's tables, read from the registry.

use atlas::parser::{Article, Release};
use atlas::postprocess::schema::{self, SHARD_TABLES};
use atlas::{db, store};
use rusqlite::Connection;

const GROUP: &str = "alt.binaries.t";

fn release(name: &str, parts: i64, have: i64) -> Release {
    Release {
        name: name.into(),
        group: GROUP.into(),
        articles: (1..=have)
            .map(|p| Article {
                message_id: format!("<{name}{p}@x>"),
                subject: format!("\"{name}\" yEnc ({p}/{parts})"),
                filename: Some(name.into()),
                part: Some(p),
                total_parts: Some(parts),
                bytes: 10,
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    }
}

fn fixture() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let main = dir.path().join("atlas.db");
    db::create_db_at(&main).unwrap();
    let shard = store::shard_path(&main, store::shard_of(GROUP));
    (dir, main, shard)
}

fn in_master(conn: &Connection, kind: &str, name: &str) -> bool {
    conn.prepare("select 1 from sqlite_master where type = ? and name = ?").unwrap().exists([kind, name]).unwrap()
}

#[test]
fn an_old_shard_gets_the_new_tables_without_a_rewrite() {
    let (_dir, main, shard) = fixture();
    store::save(&main, &[release("a", 1, 1), release("b", 1, 1), release("c", 1, 1)]).unwrap();
    let conn = db::open_shard(&shard).unwrap();
    for table in SHARD_TABLES {
        conn.execute_batch(&format!("drop trigger if exists gc_{0}; drop table {0};", table.name)).unwrap();
    }
    conn.execute_batch("alter table releases drop column name_source; pragma wal_checkpoint(truncate);").unwrap();
    let names = |conn: &Connection| -> Vec<String> {
        conn.prepare("select name from releases order by id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    };
    let before_names = names(&conn);
    let pages = |conn: &Connection| -> i64 { conn.query_row("pragma page_count", [], |r| r.get(0)).unwrap() };
    let before = pages(&conn);
    drop(conn);

    store::create_shard(&shard).unwrap();

    let conn = db::open_shard(&shard).unwrap();
    let mut objects = 0;
    for table in SHARD_TABLES {
        objects += 1 + table.indexes.len() as i64;
        assert!(in_master(&conn, "table", table.name), "{}", table.name);
        for (index, _) in table.indexes {
            assert!(in_master(&conn, "index", index), "{index}");
        }
        if table.release_key.is_some() {
            assert!(in_master(&conn, "trigger", &format!("gc_{}", table.name)));
        }
    }
    conn.query_row("select name_source from releases limit 1", [], |r| r.get::<_, Option<i64>>(0)).unwrap();
    assert_eq!(names(&conn), before_names);
    assert!(pages(&conn) - before <= 4 * objects + 8, "{} pages grew to {}", before, pages(&conn));
}

#[test]
fn deleting_a_release_clears_it_from_every_registered_table() {
    let (_dir, main, shard) = fixture();
    store::save(&main, &[release("a", 1, 1)]).unwrap();
    let conn = db::open_shard(&shard).unwrap();
    let id: i64 = conn.query_row("select id from releases", [], |r| r.get(0)).unwrap();
    for table in SHARD_TABLES.iter().filter(|t| t.release_key.is_some()) {
        conn.execute(&format!("delete from {}", table.name), []).unwrap();
        schema::sample_row(&conn, table, id).unwrap();
    }
    conn.execute("delete from releases where id = ?", [id]).unwrap();
    for table in SHARD_TABLES.iter().filter(|t| t.release_key.is_some()) {
        let n: i64 = conn.query_row(&format!("select count(*) from {}", table.name), [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0, "{}", table.name);
    }
}

#[test]
fn the_menu_purge_clears_stage_rows() {
    let (_dir, main, shard) = fixture();
    store::save(&main, &[release("a", 3, 1)]).unwrap();
    let conn = db::open_shard(&shard).unwrap();
    let id: i64 = conn.query_row("select id from releases", [], |r| r.get(0)).unwrap();
    conn.execute("insert or replace into pp_state (release_id) values (?)", [id]).unwrap();
    conn.execute("insert or replace into pp_queue (release_id, due_at) values (?, 1)", [id]).unwrap();
    drop(conn);
    store::purge_incomplete(&main).unwrap();
    let conn = db::open_shard(&shard).unwrap();
    let rows: i64 = conn
        .query_row("select (select count(*) from pp_state) + (select count(*) from pp_queue)", [], |r| r.get(0))
        .unwrap();
    assert_eq!(rows, 0);
}

#[test]
fn no_index_is_built_on_an_existing_table() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("create table releases (id INTEGER PRIMARY KEY, name TEXT)").unwrap();
    schema::migrate(&conn).unwrap();
    let on: Vec<String> = conn
        .prepare("select tbl_name from sqlite_master where type = 'index' and sql is not null")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert!(!on.is_empty());
    for table in on {
        assert!(SHARD_TABLES.iter().any(|t| t.name == table), "an index on {table}");
    }
}

#[test]
fn a_wipe_leaves_new_empty_tables() {
    let (_dir, main, _) = fixture();
    store::save(&main, &[release("a", 1, 1)]).unwrap();
    db::wipe(&main).unwrap();
    db::create_db_at(&main).unwrap();
    for path in store::shard_paths(&main) {
        let conn = db::open_shard(&path).unwrap();
        for table in SHARD_TABLES {
            let n: i64 = conn.query_row(&format!("select count(*) from {}", table.name), [], |r| r.get(0)).unwrap();
            assert_eq!(n, 0, "{}", table.name);
        }
    }
}
