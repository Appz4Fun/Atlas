//! `atlas.predb.db`: the catalogue of titles, next to the shards and never part of them.

use std::path::{Path, PathBuf};

use rusqlite::Connection;

const SCHEMA: &str = "
create table if not exists titles (
    id          INTEGER PRIMARY KEY,
    title       TEXT NOT NULL,
    source      INTEGER NOT NULL,
    category    TEXT,
    size_bytes  INTEGER,
    files       INTEGER,
    pre_at      INTEGER,
    nuked       INTEGER NOT NULL DEFAULT 0,
    nuke_reason TEXT,
    added_at    INTEGER NOT NULL
);
create table if not exists title_keys (
    key      INTEGER NOT NULL,
    title_id INTEGER NOT NULL,
    primary key (key, title_id)
) without rowid;
create table if not exists feeds (
    source       INTEGER PRIMARY KEY,
    key          TEXT NOT NULL,
    next_poll_at INTEGER NOT NULL DEFAULT 0,
    failures     INTEGER NOT NULL DEFAULT 0,
    resume_page  INTEGER NOT NULL DEFAULT 0,
    gap_floor    INTEGER NOT NULL DEFAULT 0,
    history_page INTEGER NOT NULL DEFAULT 0,
    history_read INTEGER NOT NULL DEFAULT 0,
    last_ok_at   INTEGER,
    last_error   TEXT,
    inserted     INTEGER NOT NULL DEFAULT 0,
    updated      INTEGER NOT NULL DEFAULT 0,
    -- titles the newest answer held. Beside `inserted`, it tells a page the parser can't read from one with nothing new
    last_rows    INTEGER
);
create table if not exists meta (key TEXT PRIMARY KEY, value INTEGER);
insert or ignore into meta (key, value) values ('titles', 0), ('late_after_id', 0);
";

/// `atlas.db` -> `atlas.predb.db`
pub fn path_for(main: &Path) -> PathBuf {
    crate::compact::with_suffix(main, "predb")
}

/// The file, made if it's missing.
pub fn create(path: &Path) -> rusqlite::Result<Connection> {
    let conn = crate::db::create_at(path)?;
    conn.query_row("pragma journal_mode = wal", [], |_| Ok(()))?;
    conn.execute_batch(SCHEMA)?;
    let has_rows: bool =
        conn.prepare("select 1 from pragma_table_info('feeds') where name = 'last_rows'")?.exists([])?;
    if !has_rows {
        conn.execute_batch("alter table feeds add column last_rows INTEGER")?;
    }
    Ok(conn)
}

/// The file as it is: a missing one is an error, never a new empty one.
pub fn open_reader(path: &Path) -> rusqlite::Result<Connection> {
    let conn = crate::db::open_at(path)?;
    conn.execute_batch("pragma query_only = on")?;
    Ok(conn)
}
