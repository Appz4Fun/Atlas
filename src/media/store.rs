//! `atlas.media.db`: what each title resolved to. A cache: the API reads only the shards.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, params};

use super::resolve::Ids;

const SCHEMA: &str = "
create table if not exists media_titles (
    title_key    INTEGER PRIMARY KEY,
    kind         INTEGER NOT NULL,
    title        TEXT NOT NULL,
    year         INTEGER,
    state        INTEGER NOT NULL,
    checked_mask INTEGER NOT NULL,
    tvdb_id INTEGER, tvmaze_id INTEGER, trakt_id INTEGER, tmdb_id INTEGER, imdb_id INTEGER, tvrage_id INTEGER,
    found_title  TEXT,
    checked_at   INTEGER NOT NULL,
    next_check   INTEGER,
    misses       INTEGER NOT NULL DEFAULT 0,
    fanout_done  INTEGER NOT NULL DEFAULT 1
);
create index if not exists media_titles_fanout on media_titles(title_key) where fanout_done = 0;
";

pub const STATE_ASKING: i64 = 0;
pub const STATE_FOUND: i64 = 1;
pub const STATE_NOT_FOUND: i64 = 2;

/// `atlas.db` -> `atlas.media.db`
pub fn path_for(main: &Path) -> PathBuf {
    let stem = main.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    main.with_file_name(format!("{stem}.media.db"))
}

pub fn open(path: &Path) -> rusqlite::Result<Connection> {
    let conn = crate::db::create_at(path)?;
    conn.query_row("pragma journal_mode = wal", [], |_| Ok(()))?;
    conn.execute_batch(SCHEMA)?;
    Ok(conn)
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TitleRow {
    pub key: i64,
    pub kind: i64,
    pub title: String,
    pub year: Option<i32>,
    pub state: i64,
    pub checked_mask: i64,
    pub ids: Ids,
    pub found_title: Option<String>,
    pub checked_at: i64,
    pub next_check: Option<i64>,
    pub misses: i64,
    pub fanout_done: bool,
}

const COLUMNS: &str =
    "title_key, kind, title, year, state, checked_mask, tvdb_id, tvmaze_id, trakt_id, tmdb_id, imdb_id,
    tvrage_id, found_title, checked_at, next_check, misses, fanout_done";

fn row_of(r: &rusqlite::Row<'_>) -> rusqlite::Result<TitleRow> {
    Ok(TitleRow {
        key: r.get(0)?,
        kind: r.get(1)?,
        title: r.get(2)?,
        year: r.get(3)?,
        state: r.get(4)?,
        checked_mask: r.get(5)?,
        ids: Ids {
            tvdb: r.get(6)?,
            tvmaze: r.get(7)?,
            trakt: r.get(8)?,
            tmdb: r.get(9)?,
            imdb: r.get(10)?,
            tvrage: r.get(11)?,
        },
        found_title: r.get(12)?,
        checked_at: r.get(13)?,
        next_check: r.get(14)?,
        misses: r.get(15)?,
        fanout_done: r.get::<_, i64>(16)? != 0,
    })
}

pub fn get(conn: &Connection, key: i64) -> rusqlite::Result<Option<TitleRow>> {
    conn.prepare_cached(&format!("select {COLUMNS} from media_titles where title_key = ?1"))?
        .query_row([key], row_of)
        .optional()
}

pub fn put(conn: &Connection, t: &TitleRow) -> rusqlite::Result<()> {
    conn.prepare_cached(
        "insert or replace into media_titles (title_key, kind, title, year, state, checked_mask, tvdb_id, tvmaze_id, trakt_id,
            tmdb_id, imdb_id, tvrage_id, found_title, checked_at, next_check, misses, fanout_done)
         values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
    )?
    .execute(params![
        t.key,
        t.kind,
        t.title,
        t.year,
        t.state,
        t.checked_mask,
        t.ids.tvdb,
        t.ids.tvmaze,
        t.ids.trakt,
        t.ids.tmdb,
        t.ids.imdb,
        t.ids.tvrage,
        t.found_title,
        t.checked_at,
        t.next_check,
        t.misses,
        t.fanout_done,
    ])?;
    Ok(())
}

/// What the in-memory map keeps of every title.
pub fn all(conn: &Connection, mut each: impl FnMut(TitleRow)) -> rusqlite::Result<()> {
    let mut stmt = conn.prepare(&format!("select {COLUMNS} from media_titles"))?;
    for row in stmt.query_map([], row_of)? {
        each(row?);
    }
    Ok(())
}

pub fn fanout_owed(conn: &Connection) -> rusqlite::Result<Vec<i64>> {
    conn.prepare("select title_key from media_titles where fanout_done = 0")?.query_map([], |r| r.get(0))?.collect()
}

pub fn fanout_owed_again(conn: &Connection, key: i64) -> rusqlite::Result<()> {
    conn.execute("update media_titles set fanout_done = 0 where title_key = ?1", [key])?;
    Ok(())
}

pub fn fanout_finished(conn: &Connection, key: i64) -> rusqlite::Result<()> {
    conn.execute("update media_titles set fanout_done = 1 where title_key = ?1", [key])?;
    Ok(())
}
