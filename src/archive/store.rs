//! What archive inspection stores, and how it's read back.

use std::collections::HashMap;

use rusqlite::{Connection, OptionalExtension};

use crate::db;
use crate::postprocess::schema::ShardTable;
use crate::store;

use super::{Encryption, Password};

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
    ) without rowid;
    -- a deleted release gives its names back to `max_names_mb`. Before: the gc trigger takes these rows after
    create trigger if not exists archive_names_give_back before delete on releases
    when exists (select 1 from archive_inner_files where release_id = old.id)
    begin
        update meta set value = max(0, value - (
            select coalesce(sum(length(cast(name as blob)) + 32), 0) from archive_inner_files where release_id = old.id
        )) where key = 'archive_name_bytes';
    end",
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
    added: &[("encryption", "INTEGER"), ("names_hidden", "INTEGER"), ("password_check", "BLOB")],
};

/// A password an NFO named. Unverified: nothing has tried it.
pub const NFO_PASSWORD: ShardTable = ShardTable {
    name: "release_nfo_password",
    create: "create table if not exists release_nfo_password (
        release_id INTEGER PRIMARY KEY,
        password   TEXT NOT NULL
    )",
    indexes: &[],
    release_key: Some("release_id"),
    added: &[],
};

pub const SET_NFO_PASSWORD: &str = "insert or replace into release_nfo_password (release_id, password) values (?1, ?2)";

/// Every file a release's PAR2 set describes, as the set says it: its id in the set, its whole-file MD5, the MD5 of
/// its first 16 KiB, its size and name, and its CRC32 when the slice checksums folded to one. `sealed`: the set's Main
/// packet hashes to its set id, soo nobody added this file to someone else's set. `complete`: the whole set was read.
/// Kept soo a file is known by its hashes without reading the set again, and soo the same file in another posting, under
/// any name and from any poster, is found by `(size, md5_16k, md5)`.
pub const PAR2_FILES: ShardTable = ShardTable {
    name: "par2_files",
    create: "create table if not exists par2_files (
        release_id INTEGER NOT NULL,
        file_id    BLOB NOT NULL,
        set_id     BLOB NOT NULL,
        md5        BLOB NOT NULL,
        md5_16k    BLOB NOT NULL,
        size       INTEGER NOT NULL,
        name       TEXT NOT NULL,
        crc32      INTEGER,
        sealed     INTEGER NOT NULL DEFAULT 0,
        complete   INTEGER NOT NULL DEFAULT 0,
        primary key (release_id, file_id)
    ) without rowid",
    indexes: &[
        ("par2_files_hash", "create index if not exists par2_files_hash on par2_files(size, md5_16k)"),
        ("par2_files_set", "create index if not exists par2_files_set on par2_files(set_id)"),
    ],
    release_key: Some("release_id"),
    added: &[],
};

pub const DELETE_PAR2_FILES: &str = "delete from par2_files where release_id = ?1";
pub const INSERT_PAR2_FILE: &str = "insert or replace into par2_files
    (release_id, file_id, set_id, md5, md5_16k, size, name, crc32, sealed, complete)
    values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)";

/// The CRC32 of each article body a step read whole, by the article's message-id: what yEnc's `pcrc32` says, or the
/// CRC of the decoded bytes when the trailer wasn't seen. Two articles with the same decoded length and CRC hold the
/// same bytes, whoever posted them, and a file's articles fold into its whole-file CRC32 (`gf2::crc32_combine`) once
/// they're all known.
pub const SEGMENT_CRC: ShardTable = ShardTable {
    name: "segment_crc",
    create: "create table if not exists segment_crc (
        release_id INTEGER NOT NULL,
        message_id TEXT NOT NULL,
        decoded    INTEGER NOT NULL,
        crc32      INTEGER NOT NULL,
        primary key (release_id, message_id)
    ) without rowid",
    indexes: &[("segment_crc_value", "create index if not exists segment_crc_value on segment_crc(decoded, crc32)")],
    release_key: Some("release_id"),
    added: &[],
};

pub const SET_SEGMENT_CRC: &str =
    "insert or replace into segment_crc (release_id, message_id, decoded, crc32) values (?1, ?2, ?3, ?4)";

/// The writes that keep a set's files, every one of them: the small ones and the ones alike too, soo the set reads
/// back whole.
pub fn par2_file_writes(release_id: i64, inventory: &crate::par2::Inventory) -> Vec<crate::postprocess::step::StepWrite> {
    use crate::postprocess::step::StepWrite;
    let mut writes = vec![StepWrite::new(DELETE_PAR2_FILES, vec![release_id.into()])];
    for f in &inventory.files {
        let sealed = inventory.sealed.contains(&(f.set_id, f.file_id)) && f.bound;
        writes.push(StepWrite::new(
            INSERT_PAR2_FILE,
            vec![
                release_id.into(),
                f.file_id.to_vec().into(),
                f.set_id.to_vec().into(),
                f.md5.to_vec().into(),
                f.md5_16k.to_vec().into(),
                i64::try_from(f.size).unwrap_or(i64::MAX).into(),
                f.name.clone().into(),
                f.crc32.map(i64::from).into(),
                i64::from(sealed).into(),
                i64::from(inventory.complete).into(),
            ],
        ));
    }
    writes
}

/// A set's files as the shard keeps them, in `schema`. Only a complete set is read back, soo what it says is all of
/// it. None: the release holds no complete set, or the shard has no table yet.
pub fn par2_inventory(conn: &Connection, schema: &str, release_id: i64) -> Option<crate::par2::Inventory> {
    let sql = format!(
        "select file_id, set_id, md5, md5_16k, size, name, crc32, sealed from {schema}.par2_files
         where release_id = ? and complete = 1 order by name"
    );
    let mut stmt = conn.prepare_cached(&sql).ok()?;
    let mut inventory = crate::par2::Inventory { complete: true, ..Default::default() };
    let rows = stmt
        .query_map([release_id], |r| {
            let sixteen = |at: usize| -> rusqlite::Result<[u8; 16]> {
                let bytes: Vec<u8> = r.get(at)?;
                bytes.try_into().map_err(|_| rusqlite::Error::InvalidQuery)
            };
            Ok((
                crate::par2::Par2File {
                    file_id: sixteen(0)?,
                    set_id: sixteen(1)?,
                    md5: sixteen(2)?,
                    md5_16k: sixteen(3)?,
                    size: u64::try_from(r.get::<_, i64>(4)?).unwrap_or(0),
                    name: r.get(5)?,
                    bound: true,
                    crc32: r.get::<_, Option<i64>>(6)?.and_then(|c| u32::try_from(c).ok()),
                },
                r.get::<_, i64>(7)? != 0,
            ))
        })
        .ok()?;
    for row in rows {
        let (file, sealed) = row.ok()?;
        if sealed {
            inventory.sealed.push((file.set_id, file.file_id));
        }
        inventory.files.push(file);
    }
    (!inventory.files.is_empty()).then_some(inventory)
}

/// A file known by its set, found in other releases' sets, in every shard: the same size, first 16 KiB and whole MD5.
/// Each is `(release id, the file's name there)`. `conn` has the shards attached.
pub fn same_par2_file_with(conn: &Connection, size: u64, md5_16k: &[u8], md5: &[u8], not: i64) -> Vec<(i64, String)> {
    let mut out = Vec::new();
    for s in 0..store::SHARDS {
        let sql = format!(
            "select release_id, name from s{s}.par2_files
             where size = ?1 and md5_16k = ?2 and md5 = ?3 and release_id != ?4 limit 64"
        );
        let Ok(mut stmt) = conn.prepare_cached(&sql) else { continue };
        let Ok(rows) = stmt.query_map(
            rusqlite::params![i64::try_from(size).unwrap_or(i64::MAX), md5_16k, md5, not],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
        ) else {
            continue;
        };
        out.extend(rows.flatten());
    }
    out
}

/// A release's own set's files, with their hashes, from any shard. `conn` has the shards attached.
pub fn par2_files_with(conn: &Connection, release_id: i64) -> Vec<crate::par2::Par2File> {
    let schema = format!("s{}", store::shard_of_id(release_id));
    let sql = format!(
        "select file_id, set_id, md5, md5_16k, size, name, crc32 from {schema}.par2_files where release_id = ?"
    );
    let Ok(mut stmt) = conn.prepare_cached(&sql) else { return Vec::new() };
    let Ok(rows) = stmt.query_map([release_id], |r| {
        let sixteen = |at: usize| -> rusqlite::Result<[u8; 16]> {
            let bytes: Vec<u8> = r.get(at)?;
            bytes.try_into().map_err(|_| rusqlite::Error::InvalidQuery)
        };
        Ok(crate::par2::Par2File {
            file_id: sixteen(0)?,
            set_id: sixteen(1)?,
            md5: sixteen(2)?,
            md5_16k: sixteen(3)?,
            size: u64::try_from(r.get::<_, i64>(4)?).unwrap_or(0),
            name: r.get(5)?,
            bound: true,
            crc32: r.get::<_, Option<i64>>(6)?.and_then(|c| u32::try_from(c).ok()),
        })
    }) else {
        return Vec::new();
    };
    rows.flatten().collect()
}

/// The releases in every shard whose sealed set is this one: the recovery volumes of one upload, each posted on its own,
/// wherever its group's shard is. `conn` has the shards attached.
pub fn releases_of_set_with(conn: &Connection, set_id: &[u8], not: i64) -> Vec<i64> {
    let mut out = Vec::new();
    for s in 0..store::SHARDS {
        let sql = format!(
            "select distinct release_id from s{s}.par2_files where set_id = ?1 and sealed = 1 and release_id != ?2
             limit 1024"
        );
        let Ok(mut stmt) = conn.prepare_cached(&sql) else { continue };
        let Ok(rows) = stmt.query_map(rusqlite::params![set_id, not], |r| r.get::<_, i64>(0)) else { continue };
        out.extend(rows.flatten());
    }
    out.sort_unstable();
    out.dedup();
    out
}

pub const DELETE_SOURCE: &str = "delete from archive_inner_files where release_id = ?1 and source = ?2";
pub const INSERT_FILE: &str =
    "insert or replace into archive_inner_files (release_id, seq, name, size, source) values (?1, ?2, ?3, ?4, ?5)";
pub const UPSERT_RELEASE: &str = "insert into archive_release (release_id, password) values (?1, ?2)
    on conflict(release_id) do update set password = excluded.password";
pub const SET_ENCRYPTION: &str =
    "update archive_release set encryption = ?2, names_hidden = ?3, password_check = ?4 where release_id = ?1";
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

/// Any failure, such as a shard that hasn't got the columns yet, reads as nothing known.
fn read_encryption(
    conn: &Connection,
    schema: &str,
    release_id: i64,
) -> (Option<Encryption>, bool, Option<super::PasswordTest>) {
    let sql =
        format!("select encryption, names_hidden, password_check from {schema}.archive_release where release_id = ?");
    conn.query_row(&sql, [release_id], |r| {
        Ok((
            r.get::<_, Option<i64>>(0)?.and_then(Encryption::from_code),
            r.get::<_, Option<i64>>(1)?.unwrap_or(0) != 0,
            r.get::<_, Option<Vec<u8>>>(2)?.as_deref().and_then(super::PasswordTest::from_bytes),
        ))
    })
    .unwrap_or((None, false, None))
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
    db::with_reader(|conn| Ok(passwords_with(conn, ids))).unwrap_or_default()
}

/// What the release view shows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InnerView {
    pub password: Option<Password>,
    pub encryption: Option<Encryption>,
    pub names_hidden: bool,
    pub password_check: Option<super::PasswordTest>,
    pub files: Vec<InnerFileRow>,
}

/// Any failure, such as a shard without the table yet, reads as nothing known.
pub fn nfo_password(conn: &Connection, release_id: i64) -> Option<String> {
    let schema = format!("s{}", store::shard_of_id(release_id));
    conn.query_row(
        &format!("select password from {schema}.release_nfo_password where release_id = ?"),
        [release_id],
        |r| r.get(0),
    )
    .ok()
}

pub fn view_with(conn: &Connection, release_id: i64) -> InnerView {
    let schema = format!("s{}", store::shard_of_id(release_id));
    let (encryption, names_hidden, password_check) = read_encryption(conn, &schema, release_id);
    InnerView {
        password: read_password(conn, &schema, release_id).ok().flatten(),
        encryption,
        names_hidden,
        password_check,
        files: rows(conn, &schema, release_id).unwrap_or_default(),
    }
}

pub fn view(release_id: i64) -> InnerView {
    db::open().map(|conn| view_with(&conn, release_id)).unwrap_or_default()
}
