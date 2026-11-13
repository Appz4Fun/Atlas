//! `atlas.media.db`: what each title resolved to. A cache: the API reads only the shards.

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
