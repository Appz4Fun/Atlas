use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, params};

use crate::parser::Release;
use crate::paths;

pub type Result<T> = rusqlite::Result<T>;

pub fn open() -> Result<Connection> {
    open_at(&paths::database())
}

pub fn open_at(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path)?;
    conn.busy_timeout(Duration::from_secs(30))?;
    // bundled sqlite turns these on by default, python's didnt. purge deletes
    // releases before their articles and old dbs can hold orphans.
    conn.execute_batch("pragma foreign_keys = off")?;
    // with WAL this cant corrupt the db, a crash only loses the last commit,
    // which the indexer redoes anyway since cursors move after the data
    conn.execute_batch("pragma synchronous = normal")?;
    Ok(conn)
}

pub fn create_db() -> Result<()> {
    let path = paths::database();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    create_db_at(&path)
}

pub fn create_db_at(path: &Path) -> Result<()> {
    let conn = open_at(path)?;

    // wal soo the indexer can write while search reads
    conn.query_row("pragma journal_mode = wal", [], |_| Ok(()))?;

    conn.execute_batch(
        "
        create table if not exists releases (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT,
            group_name TEXT,
            poster TEXT,
            posted_date TEXT,
            size INTEGER,
            complete INTEGER,
            parts INTEGER,
            file_total INTEGER,
            display_name TEXT,
            is_obfuscated INTEGER default 0
        );

        create table if not exists articles (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            release_id INTEGER,
            message_id TEXT,
            subject TEXT,
            filename TEXT,
            part INTEGER,
            total_parts INTEGER,
            bytes INTEGER,
            file_total INTEGER,
            foreign key (release_id) references releases(id),
            unique(release_id, message_id)
        );

        create table if not exists groups(
            name TEXT PRIMARY KEY,
            live_cursor INTEGER,
            backfill_cursor INTEGER
        );
        ",
    )?;

    // old db files are missing columns, add em before indexing them
    migrate(&conn)?;

    conn.execute_batch(
        "
        create unique index if not exists idx_release_unique on releases(name, group_name);
        create index if not exists idx_release_name on releases(name);
        create index if not exists idx_release_group on releases(group_name);
        create index if not exists idx_release_date on releases(posted_date);
        create index if not exists idx_articles_release on articles(release_id);
        ",
    )?;

    // external content fts over the posted name and the real name (from par2/nfo),
    // the release data itself stays in releases
    let fts_sql: Option<String> = conn
        .query_row("select sql from sqlite_master where type = 'table' and name = 'releases_fts'", [], |r| r.get(0))
        .optional()?;

    // older dbs only indexed `name`, searching the real name then meant a full
    // table scan (seconds on a big db). rebuild the index with both columns
    let stale = fts_sql.as_deref().is_some_and(|sql| !sql.contains("display_name"));
    if stale {
        println!("updating the search index (one time, can take a minute on a big database)...");
        conn.execute_batch(
            "
            drop trigger if exists releases_ai;
            drop trigger if exists releases_ad;
            drop trigger if exists releases_au;
            drop table releases_fts;
            ",
        )?;
    }

    let fresh = fts_sql.is_none() || stale;
    if fresh {
        conn.execute_batch(
            "create virtual table releases_fts using fts5(name, display_name, content='releases', content_rowid='id')",
        )?;
    }

    // keep fts in sync with releases
    conn.execute_batch(
        "
        create trigger if not exists releases_ai after insert on releases begin
            insert into releases_fts(rowid, name, display_name) values (new.id, new.name, new.display_name);
        end;

        create trigger if not exists releases_ad after delete on releases begin
            insert into releases_fts(releases_fts, rowid, name, display_name)
                values ('delete', old.id, old.name, old.display_name);
        end;

        create trigger if not exists releases_au after update on releases begin
            insert into releases_fts(releases_fts, rowid, name, display_name)
                values ('delete', old.id, old.name, old.display_name);
            insert into releases_fts(rowid, name, display_name) values (new.id, new.name, new.display_name);
        end;
        ",
    )?;

    if fresh {
        // fill fts with whatever rows already exist
        conn.execute("insert into releases_fts(releases_fts) values ('rebuild')", [])?;
    }

    Ok(())
}

fn columns(conn: &Connection, table: &str) -> Result<BTreeSet<String>> {
    let mut stmt = conn.prepare(&format!("pragma table_info({table})"))?;
    let cols = stmt.query_map([], |r| r.get::<_, String>(1))?.collect::<Result<_>>()?;
    Ok(cols)
}

fn migrate(conn: &Connection) -> Result<()> {
    let release_columns = columns(conn, "releases")?;
    let add = |col: &str, ty: &str| -> Result<()> {
        if !release_columns.contains(col) {
            conn.execute(&format!("alter table releases add column {col} {ty}"), [])?;
        }
        Ok(())
    };

    add("group_name", "TEXT")?;
    add("poster", "TEXT")?;
    add("posted_date", "TEXT")?;

    if !release_columns.contains("parts") {
        conn.execute("alter table releases add column parts INTEGER", [])?;
        // backfill parts for releases already in the db
        conn.execute(
            "update releases set parts = (select count(*) from articles where release_id = releases.id) where parts is null",
            [],
        )?;
    }

    add("file_total", "INTEGER")?;
    add("display_name", "TEXT")?;

    if !release_columns.contains("is_obfuscated") {
        conn.execute("alter table releases add column is_obfuscated INTEGER default 0", [])?;
        if release_columns.contains("obfuscated") {
            conn.execute("update releases set is_obfuscated = obfuscated", [])?;
        }
    }

    let article_columns = columns(conn, "articles")?;
    if !article_columns.contains("subject") {
        conn.execute("alter table articles add column subject TEXT", [])?;
    }
    if !article_columns.contains("file_total") {
        conn.execute("alter table articles add column file_total INTEGER", [])?;
    }

    rebuild_articles_unique(conn)
}

/// very old dbs had no unique(release_id, message_id), rebuild the table with it
fn rebuild_articles_unique(conn: &Connection) -> Result<()> {
    let sql: Option<String> = conn
        .query_row("select sql from sqlite_master where type = 'table' and name = 'articles'", [], |r| r.get(0))
        .optional()?;

    match sql {
        None => return Ok(()),
        Some(sql) if sql.to_lowercase().contains("unique(release_id, message_id)") => return Ok(()),
        _ => {}
    }

    conn.execute_batch(
        "
        begin;
        create table articles_new (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            release_id INTEGER,
            message_id TEXT,
            subject TEXT,
            filename TEXT,
            part INTEGER,
            total_parts INTEGER,
            bytes INTEGER,
            file_total INTEGER,
            foreign key (release_id) references releases(id),
            unique(release_id, message_id)
        );
        insert or ignore into articles_new
            (id, release_id, message_id, subject, filename, part, total_parts, bytes, file_total)
            select id, release_id, message_id, subject, filename, part, total_parts, bytes, file_total from articles;
        drop table articles;
        alter table articles_new rename to articles;
        create index if not exists idx_articles_release on articles(release_id);
        commit;
        ",
    )
    .inspect_err(|_| {
        let _ = conn.execute_batch("rollback");
    })
}

pub struct ReleaseStats {
    pub size: i64,
    pub complete: bool,
    pub parts: i64,
    pub file_total: Option<i64>,
}

fn release_stats(conn: &Connection, release_id: i64) -> Result<ReleaseStats> {
    let mut stmt =
        conn.prepare("select filename, part, total_parts, bytes, file_total from articles where release_id = ?")?;
    let mut rows = stmt.query([release_id])?;

    // filename -> [(part, total_parts)]
    type Parts = Vec<(Option<i64>, Option<i64>)>;
    let mut files: HashMap<Option<String>, Parts> = HashMap::new();
    let mut size = 0;
    let mut file_total: Option<i64> = None;

    while let Some(row) = rows.next()? {
        let filename: Option<String> = row.get(0)?;
        let part: Option<i64> = row.get(1)?;
        let total: Option<i64> = row.get(2)?;
        let bytes: Option<i64> = row.get(3)?;
        let ft: Option<i64> = row.get(4)?;

        files.entry(filename).or_default().push((part, total));
        size += bytes.unwrap_or(0);

        if let Some(ft) = ft
            && file_total.is_none_or(|cur| ft > cur)
        {
            file_total = Some(ft);
        }
    }

    let parts = files.values().map(|f| f.len() as i64).sum();
    let mut complete = true;

    for file_parts in files.values() {
        let Some(expected) = file_parts.iter().filter_map(|(_, t)| *t).max() else {
            complete = false;
            break;
        };

        let have: BTreeSet<i64> = file_parts.iter().filter_map(|(p, _)| *p).collect();
        let want: BTreeSet<i64> = (1..=expected).collect();

        if have != want {
            complete = false;
            break;
        }
    }

    if complete
        && let Some(ft) = file_total
        && files.len() as i64 != ft
    {
        complete = false;
    }

    Ok(ReleaseStats { size, complete, parts, file_total })
}

pub fn save_releases_bulk(releases: &[Release]) -> Result<()> {
    if releases.is_empty() {
        return Ok(());
    }

    let mut conn = open()?;
    save_releases_bulk_with(&mut conn, releases)
}

/// Upsert releases + their articles in one transaction soo a half written
/// batch rolls back.
pub fn save_releases_bulk_with(conn: &mut Connection, releases: &[Release]) -> Result<()> {
    let tx = conn.transaction()?;

    {
        let mut upsert = tx.prepare(
            "insert into releases
                (name, size, complete, group_name, poster, posted_date, display_name, is_obfuscated)
                values (?, ?, ?, ?, ?, ?, ?, ?)
                on conflict(name, group_name) do update set
                poster = excluded.poster,
                posted_date = excluded.posted_date,
                display_name = coalesce(excluded.display_name, releases.display_name),
                is_obfuscated = excluded.is_obfuscated
                returning id",
        )?;

        let mut insert_article = tx.prepare(
            "insert or ignore into articles
                (release_id, message_id, subject, filename, part, total_parts, bytes, file_total)
                values (?, ?, ?, ?, ?, ?, ?, ?)",
        )?;

        let mut update_stats =
            tx.prepare("update releases set size = ?, complete = ?, parts = ?, file_total = ? where id = ?")?;

        for release in releases {
            let release_id: Option<i64> = upsert
                .query_row(
                    params![
                        release.name,
                        release.size,
                        release.complete as i64,
                        release.group,
                        release.poster,
                        release.date,
                        release.display_name,
                        release.is_obfuscated as i64,
                    ],
                    |r| r.get(0),
                )
                .optional()?;

            let Some(release_id) = release_id else { continue };

            let mut inserted = 0;
            for a in release.articles.iter().filter(|a| !a.message_id.is_empty()) {
                inserted += insert_article.execute(params![
                    release_id,
                    a.message_id,
                    a.subject,
                    a.filename,
                    a.part,
                    a.total_parts,
                    a.bytes,
                    a.file_total,
                ])?;
            }

            if inserted == 0 {
                continue;
            }

            let s = release_stats(&tx, release_id)?;
            update_stats.execute(params![s.size, s.complete as i64, s.parts, s.file_total, release_id])?;
        }
    }

    tx.commit()
}

/// delete incomplete releases, returns bytes freed on disk
pub fn purge_broken() -> Result<i64> {
    let path = paths::database();
    let before = fs::metadata(&path).map(|m| m.len() as i64).unwrap_or(0);

    let conn = open()?;
    conn.execute("delete from releases where complete = 0", [])?;
    conn.execute("delete from articles where release_id not in (select id from releases)", [])?;

    let _ = conn.query_row("pragma wal_checkpoint(truncate)", [], |_| Ok(()));
    let _ = conn.execute_batch("vacuum");
    drop(conn);

    let after = fs::metadata(&path).map(|m| m.len() as i64).unwrap_or(0);
    Ok(before - after)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GroupState {
    pub live_cursor: i64,
    pub backfill_cursor: i64,
}

pub fn get_group_state(conn: &Connection, group: &str) -> Result<Option<GroupState>> {
    let row: Option<(Option<i64>, Option<i64>)> = conn
        .query_row("select live_cursor, backfill_cursor from groups where name = ?", [group], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .optional()?;

    Ok(match row {
        Some((Some(live), Some(back))) => Some(GroupState { live_cursor: live, backfill_cursor: back }),
        _ => None,
    })
}

pub fn init_group_state(conn: &Connection, group: &str, cursor: i64) -> Result<()> {
    conn.execute(
        "insert into groups(name, live_cursor, backfill_cursor) values(?, ?, ?)
         on conflict(name) do update set
         live_cursor = excluded.live_cursor,
         backfill_cursor = excluded.backfill_cursor",
        params![group, cursor, cursor],
    )?;
    Ok(())
}

pub fn save_group_state(conn: &Connection, group: &str, state: GroupState) -> Result<()> {
    conn.execute(
        "insert into groups(name, live_cursor, backfill_cursor) values(?, ?, ?)
         on conflict(name) do update set
         live_cursor = excluded.live_cursor,
         backfill_cursor = excluded.backfill_cursor",
        params![group, state.live_cursor, state.backfill_cursor],
    )?;
    Ok(())
}

pub fn update_live_cursor(conn: &Connection, group: &str, article: i64) -> Result<()> {
    conn.execute(
        "insert into groups(name, live_cursor, backfill_cursor) values(?, ?, ?)
         on conflict(name) do update set live_cursor = excluded.live_cursor",
        params![group, article, article],
    )?;
    Ok(())
}

pub fn update_backfill_cursor(conn: &Connection, group: &str, article: i64) -> Result<()> {
    conn.execute(
        "insert into groups(name, live_cursor, backfill_cursor) values(?, ?, ?)
         on conflict(name) do update set backfill_cursor = excluded.backfill_cursor",
        params![group, article, article],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_name_only_index_gets_rebuilt_with_real_names() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("atlas.db");

        // a db from before display_name was indexed
        {
            let conn = open_at(&path).unwrap();
            conn.execute_batch(
                "
                create table releases (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT, group_name TEXT, poster TEXT,
                    posted_date TEXT, size INTEGER, complete INTEGER, parts INTEGER, file_total INTEGER,
                    display_name TEXT, is_obfuscated INTEGER default 0);
                create virtual table releases_fts using fts5(name, content='releases', content_rowid='id');
                create trigger releases_ai after insert on releases begin
                    insert into releases_fts(rowid, name) values (new.id, new.name);
                end;
                insert into releases(name, display_name) values ('a1B2c3D4e5F6g7H8', 'Real.Movie.Name.2024');
                ",
            )
            .unwrap();
        }

        create_db_at(&path).unwrap();
        let conn = open_at(&path).unwrap();
        let hits = |q: &str| -> i64 {
            conn.query_row("select count(*) from releases_fts where releases_fts match ?", [q], |r| r.get(0)).unwrap()
        };

        assert_eq!(hits("\"Real\"* AND \"Movie\"*"), 1, "real name searchable after the rebuild");
        assert_eq!(hits("\"a1B2c3D4e5F6g7H8\""), 1, "posted name still searchable");

        // new rows and updates keep both columns in sync
        conn.execute("insert into releases(name, display_name) values ('xyz', 'Another.Show.S01E01')", []).unwrap();
        assert_eq!(hits("\"S01E01\"*"), 1);
        conn.execute("update releases set display_name = 'Renamed.Thing' where name = 'xyz'", []).unwrap();
        assert_eq!(hits("\"S01E01\"*"), 0);
        assert_eq!(hits("\"Renamed\"*"), 1);

        // running it again leaves a current index alone
        create_db_at(&path).unwrap();
    }
}
