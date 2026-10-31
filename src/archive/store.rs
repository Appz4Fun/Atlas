//! What archive inspection stores, and how it's read back.

use std::collections::HashMap;

use rusqlite::{Connection, OptionalExtension};

use crate::db;
use crate::postprocess::schema::ShardTable;
use crate::store;

use super::Password;

pub const SOURCE_PAR2: i64 = 1;
pub const SOURCE_RAR: i64 = 2;
pub const SOURCE_7Z: i64 = 3;
pub const SOURCE_ZIP: i64 = 4;

pub const INNER_FILES: ShardTable = ShardTable {
    name: "archive_inner_files",
    create: "create table if not exists archive_inner_files (
        release_id INTEGER NOT NULL,
        seq        INTEGER NOT NULL,
        name       TEXT NOT NULL,
        size       INTEGER,
        source     INTEGER NOT NULL,
        primary key (release_id, seq)
    ) without rowid",
    indexes: &[],
    release_key: Some("release_id"),
    added: &[],
};

/// a row only when an archive was identified. No row: nothing known, no API attribute
pub const RELEASE: ShardTable = ShardTable {
    name: "archive_release",
    create: "create table if not exists archive_release (
        release_id INTEGER PRIMARY KEY,
        password   INTEGER NOT NULL
    )",
    indexes: &[],
    release_key: Some("release_id"),
    added: &[],
};

pub const DELETE_SOURCE: &str = "delete from archive_inner_files where release_id = ?1 and source = ?2";
pub const INSERT_FILE: &str =
    "insert or replace into archive_inner_files (release_id, seq, name, size, source) values (?1, ?2, ?3, ?4, ?5)";
pub const UPSERT_RELEASE: &str = "insert or replace into archive_release (release_id, password) values (?1, ?2)";
/// what a source's rows of a release count for in `archive_name_bytes`
pub const BYTES_OF_SOURCE: &str = "select coalesce(sum(length(cast(name as blob)) + 32), 0)
    from archive_inner_files where release_id = ?1 and source = ?2";
/// the running totals behind `max_names_mb`
pub const ADD_NAME_BYTES: &str = "insert into meta (key, value) values ('archive_name_bytes', ?1)
    on conflict(key) do update set value = value + excluded.value";
pub const COUNT_OVER_QUOTA: &str = "insert into meta (key, value) values ('archive_names_over_quota', 1)
    on conflict(key) do update set value = value + 1";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InnerFileRow {
    pub seq: i64,
    pub name: String,
    pub size: Option<i64>,
    pub source: i64,
}

fn rows(conn: &Connection, schema: &str, release_id: i64) -> rusqlite::Result<Vec<InnerFileRow>> {
    conn.prepare_cached(&format!(
        "select seq, name, size, source from {schema}.archive_inner_files where release_id = ? order by seq limit 256"
    ))?
    .query_map([release_id], |r| {
        Ok(InnerFileRow { seq: r.get(0)?, name: r.get(1)?, size: r.get(2)?, source: r.get(3)? })
    })?
    .collect()
}

fn read_password(conn: &Connection, schema: &str, release_id: i64) -> rusqlite::Result<Option<Password>> {
    let code: Option<i64> = conn
        .prepare_cached(&format!("select password from {schema}.archive_release where release_id = ?"))?
        .query_row([release_id], |r| r.get(0))
        .optional()?;
    Ok(code.and_then(Password::from_code))
}

/// In `seq` order: PAR2 rows first. `conn` is one shard with its tables in `main`.
pub fn inner_files(conn: &Connection, release_id: i64) -> rusqlite::Result<Vec<InnerFileRow>> {
    rows(conn, "main", release_id)
}

pub fn password(conn: &Connection, release_id: i64) -> rusqlite::Result<Option<Password>> {
    read_password(conn, "main", release_id)
}

/// `conn` has the shards attached. Any failure, such as a missing table, reads as nothing known.
pub fn passwords_with(conn: &Connection, ids: &[i64]) -> HashMap<i64, Password> {
    ids.iter()
        .filter_map(|id| {
            let schema = format!("s{}", store::shard_of_id(*id));
            read_password(conn, &schema, *id).ok().flatten().map(|p| (*id, p))
        })
        .collect()
}

pub fn passwords(ids: &[i64]) -> HashMap<i64, Password> {
    db::open().map(|conn| passwords_with(&conn, ids)).unwrap_or_default()
}

/// What the release view shows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InnerView {
    pub password: Option<Password>,
    pub files: Vec<InnerFileRow>,
}

pub fn view_with(conn: &Connection, release_id: i64) -> InnerView {
    let schema = format!("s{}", store::shard_of_id(release_id));
    InnerView {
        password: read_password(conn, &schema, release_id).ok().flatten(),
        files: rows(conn, &schema, release_id).unwrap_or_default(),
    }
}

pub fn view(release_id: i64) -> InnerView {
    db::open().map(|conn| view_with(&conn, release_id)).unwrap_or_default()
}
