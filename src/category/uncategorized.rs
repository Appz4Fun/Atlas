//! The releases with no category, listed. A release the rules match nothing for has no `release_category` row, and
//! no index can find rows that have no row elsewhere: without this list, finding them means reading the whole shard.
//! Triggers keep it to exactly the releases without a category row, soo it is read like a category.

use rusqlite::{Connection, OptionalExtension, Result};

/// Its table and the triggers that keep it, as `create ... if not exists`.
const CREATE: &str = "
    create table if not exists release_uncategorized (release_id INTEGER PRIMARY KEY);
    create trigger if not exists uncategorized_added after insert on releases begin
        insert or ignore into release_uncategorized (release_id)
        select new.id where not exists (select 1 from release_category c where c.release_id = new.id);
    end;
    create trigger if not exists uncategorized_gone after delete on releases begin
        delete from release_uncategorized where release_id = old.id;
    end;
    create trigger if not exists uncategorized_categorized after insert on release_category begin
        delete from release_uncategorized where release_id = new.release_id;
    end;
    create trigger if not exists uncategorized_uncategorized after delete on release_category begin
        insert or ignore into release_uncategorized (release_id)
        select old.release_id where exists (select 1 from releases r where r.id = old.release_id);
    end;
";

/// `meta` key: set once the list holds every release with no category, not only those since the triggers came.
const LISTED: &str = "uncategorized_listed";

/// The list and its triggers, if missing. A shard with no releases yet is listed from the start; one with releases
/// is listed by `fill`, which reads them all, soo opening a shard never does.
pub fn migrate(conn: &Connection) -> Result<()> {
    let existed: bool = conn.query_row(
        "select exists (select 1 from sqlite_master where type = 'table' and name = 'release_uncategorized')",
        [],
        |r| r.get(0),
    )?;
    conn.execute_batch(CREATE)?;
    let empty: bool = conn.query_row("select not exists (select 1 from releases)", [], |r| r.get(0))?;
    if !existed && empty {
        mark_listed(conn)?;
    }
    Ok(())
}

/// Every release with no category put on the list, in one statement soo no write lands in between, and the shard
/// marked listed. `conn` has the shard as `main`.
pub fn fill(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "begin immediate;
         insert or ignore into release_uncategorized (release_id)
         select r.id from releases r where not exists (select 1 from release_category c where c.release_id = r.id);",
    )?;
    let marked = mark_listed(conn);
    conn.execute_batch(if marked.is_ok() { "commit" } else { "rollback" })?;
    marked
}

fn mark_listed(conn: &Connection) -> Result<()> {
    conn.execute("insert or replace into meta (key, value) values (?1, 1)", [LISTED]).map(drop)
}

/// Whether shard `s`, attached as `s{s}`, lists all its releases with no category.
pub fn listed(conn: &Connection, s: usize) -> bool {
    conn.query_row(&format!("select value from s{s}.meta where key = ?1"), [LISTED], |r| r.get::<_, i64>(0))
        .optional()
        .is_ok_and(|v| v == Some(1))
}

#[cfg(test)]
mod tests {
    use crate::{db, store};

    fn listed_ids(conn: &rusqlite::Connection) -> Vec<i64> {
        conn.prepare("select release_id from release_uncategorized order by release_id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    #[test]
    fn the_list_is_the_releases_with_no_category() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.db");
        store::create_shard(&path).unwrap();
        let conn = db::open_shard(&path).unwrap();
        let listed: i64 =
            conn.query_row("select value from meta where key = 'uncategorized_listed'", [], |r| r.get(0)).unwrap();
        assert_eq!(listed, 1, "a new shard is listed from the start");
        conn.execute_batch(
            "insert into releases (id, name, group_name) values (1, 'a', 'g'), (2, 'b', 'g'), (3, 'c', 'g');",
        )
        .unwrap();
        assert_eq!(listed_ids(&conn), [1, 2, 3]);
        conn.execute("insert into release_category (release_id, category) values (2, 2040)", []).unwrap();
        assert_eq!(listed_ids(&conn), [1, 3]);
        // a category replaced keeps it off, and one taken away puts it back
        conn.execute("insert or replace into release_category (release_id, category) values (2, 2045)", []).unwrap();
        assert_eq!(listed_ids(&conn), [1, 3]);
        conn.execute("delete from release_category where release_id = 2", []).unwrap();
        assert_eq!(listed_ids(&conn), [1, 2, 3]);
        conn.execute("delete from releases where id = 3", []).unwrap();
        assert_eq!(listed_ids(&conn), [1, 2]);
    }

    #[test]
    fn a_shard_with_releases_from_before_the_list_is_listed_by_fill() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.db");
        store::create_shard(&path).unwrap();
        let conn = db::open_shard(&path).unwrap();
        // as a shard from before: releases, no list, no mark
        conn.execute_batch(
            "drop table release_uncategorized;
             drop trigger uncategorized_added; drop trigger uncategorized_gone;
             drop trigger uncategorized_categorized; drop trigger uncategorized_uncategorized;
             delete from meta where key = 'uncategorized_listed';
             insert into releases (id, name, group_name) values (1, 'a', 'g'), (2, 'b', 'g');
             insert into release_category (release_id, category) values (1, 2040);",
        )
        .unwrap();
        store::migrate_shard(&conn).unwrap();
        let mark = |conn: &rusqlite::Connection| -> Option<i64> {
            use rusqlite::OptionalExtension;
            conn.query_row("select value from meta where key = 'uncategorized_listed'", [], |r| r.get(0))
                .optional()
                .unwrap()
        };
        assert_eq!(mark(&conn), None, "opening it doesn't read every release");
        assert!(listed_ids(&conn).is_empty());
        super::fill(&conn).unwrap();
        assert_eq!(mark(&conn), Some(1));
        assert_eq!(listed_ids(&conn), [2]);
    }
}
