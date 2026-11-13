//! Releases and their articles, split over `SHARDS` database files next to
//! `atlas.db` (`atlas.s0.db` .. `atlas.s7.db`) by group, each with its own
//! writer. `atlas.db` itself keeps the per group cursors and the id counter.
//!
//! Each shard stores articles compactly (about a quarter of the old layout):
//! - `releases`, its full text index and triggers: as before
//! - `files`: one row per file of a release: filename, the subject the NZB
//!   uses, which parts are there (`seen`, a bitmap) and how many there should be
//! - `segments`: one row per article, keyed (file, message-id) in a WITHOUT
//!   ROWID table soo the message-id is stored once. Message-ids are split into
//!   a local part (hex packed into bytes) and a shared domain (`domains`)
//!
//! Release ids are `seq * SHARDS + shard`: `seq` counts up across all shards
//! in the order releases are added (blocks of it come from `atlas.db`), soo
//! ordering by id is still newest first across shards, and an id says which
//! shard holds it.

use std::borrow::Cow;
use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use indexmap::IndexMap;
use rusqlite::{Connection, OptionalExtension, Result, TransactionBehavior, params};

use crate::catalog_facts::SIDE_EXTENSIONS;
pub(crate) use crate::catalog_facts::{
    ArticleFacts, ROUTE_SLACK_SECS, VIDEO_EXTENSIONS, adds_parts, due, has_part, insert_part, is_exactly, keyed_hash,
    merge_parts, one_video_named, parts_overlap, same_parts, subject_key,
};
pub use crate::catalog_facts::{
    COMPLETE_SETTLE_SECS, FileInfo, READ_MAX_SEGMENTS, SEAL_AGE, SEAL_MAX_SEGMENTS, SETTLE_SECS, SHARDS, SegmentPick,
    SegmentRef, global_id, seen_count, shard_of, shard_of_id,
};
use crate::db;
use crate::parser::{Article, Release};
use crate::postprocess::schema::ShardTable;
use crate::profile;
use crate::search::ArticleRow;

/// `atlas.db` -> `atlas.s3.db`
pub fn shard_path(main: &Path, shard: usize) -> PathBuf {
    let stem = main.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "atlas".into());
    main.with_file_name(format!("{stem}.s{shard}.db"))
}

pub fn shard_paths(main: &Path) -> Vec<PathBuf> {
    (0..SHARDS).map(|i| shard_path(main, i)).collect()
}

/// the size of the main database and its shards, in bytes (not their -wal
/// files, see `wal_bytes`)
pub fn database_bytes(main: &Path) -> u64 {
    files_bytes(main, "")
}

/// the size of the -wal files of the main database and its shards, in bytes
pub fn wal_bytes(main: &Path) -> u64 {
    files_bytes(main, "-wal")
}

fn files_bytes(main: &Path, suffix: &str) -> u64 {
    std::iter::once(main.to_path_buf())
        .chain(shard_paths(main))
        .map(|p| std::fs::metadata(format!("{}{suffix}", p.display())).map(|m| m.len()).unwrap_or(0))
        .sum()
}

/// every shard of `main` is there
pub fn exists(main: &Path) -> bool {
    shard_paths(main).iter().all(|p| p.exists())
}

/// `select ... union all select ...` over every shard: `part(i)` writes the
/// select for shard i, its tables are `s{i}.releases` and so on
pub fn each_shard(part: impl Fn(usize) -> String) -> String {
    (0..SHARDS).map(part).collect::<Vec<_>>().join("\nunion all\n")
}

/// Attach every shard of the main database `conn` (as s0 .. s7), for reads
/// across them, plus a `releases` view of all of them for simple queries.
/// Shards that dont exist are left out rather than created.
pub fn attach(conn: &Connection, main: &Path) -> Result<()> {
    let mut all = true;
    for (i, path) in shard_paths(main).iter().enumerate() {
        if path.exists() {
            conn.execute("attach database ? as ?", params![path.to_string_lossy(), format!("s{i}")])?;
        } else {
            all = false;
        }
    }
    if all {
        conn.execute_batch(&format!(
            "create temp view if not exists releases as {}",
            each_shard(|i| format!("select * from s{i}.releases"))
        ))?;
    }
    Ok(())
}

/// The main database's own tables for the store: the id counter.
pub fn create_main(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "create table if not exists meta (key TEXT PRIMARY KEY, value INTEGER);
         insert or ignore into meta (key, value) values ('next_seq', 1);",
    )?;
    crate::chunks::create(conn)?;
    crate::spotnet::store::create_main(conn)?;
    Ok(())
}

/// A shard's tables, if missing.
pub fn create_shard(path: &Path) -> Result<()> {
    build_shard(&db::create_at(path)?)
}

/// `create_shard` on a connection the caller opened (and may have set up to
/// be interrupted).
pub fn build_shard(conn: &Connection) -> Result<()> {
    conn.query_row("pragma journal_mode = wal", [], |_| Ok(()))?;
    conn.execute_batch(
        "
        create table if not exists releases (
            id INTEGER PRIMARY KEY,
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
        create unique index if not exists idx_release_unique on releases(name, group_name);
        create index if not exists idx_release_group on releases(group_name);

        create virtual table if not exists releases_fts
            using fts5(name, display_name, content='releases', content_rowid='id');
        create trigger if not exists releases_ai after insert on releases begin
            insert into releases_fts(rowid, name, display_name) values (new.id, new.name, new.display_name);
        end;
        create trigger if not exists releases_ad after delete on releases begin
            insert into releases_fts(releases_fts, rowid, name, display_name)
                values ('delete', old.id, old.name, old.display_name);
        end;
        create trigger if not exists releases_au after update of name, display_name on releases
        when old.name is not new.name or old.display_name is not new.display_name begin
            insert into releases_fts(releases_fts, rowid, name, display_name)
                values ('delete', old.id, old.name, old.display_name);
            insert into releases_fts(rowid, name, display_name) values (new.id, new.name, new.display_name);
        end;

        create table if not exists files (
            id INTEGER PRIMARY KEY,
            release_id INTEGER NOT NULL,
            filename TEXT NOT NULL,
            subject TEXT,
            subject_part INTEGER,
            subject_mid TEXT,
            expected INTEGER,
            file_total INTEGER,
            seen BLOB NOT NULL,
            touched_at INTEGER,
            blob BLOB
        );
        create unique index if not exists files_key on files(release_id, filename);

        create table if not exists domains (id INTEGER PRIMARY KEY, suffix TEXT NOT NULL UNIQUE);
        create table if not exists segments (
            file_id INTEGER NOT NULL,
            local BLOB NOT NULL,
            domain INTEGER NOT NULL,
            part INTEGER,
            bytes INTEGER,
            primary key (file_id, local, domain)
        ) without rowid;

        -- running totals for the stats pages: releases, articles
        create table if not exists meta (key TEXT PRIMARY KEY, value INTEGER);
        insert or ignore into meta (key, value) values ('releases', 0), ('articles', 0);
        ",
    )?;
    migrate_shard(conn)
}

/// What a shard made before sealing lacks: `files.touched_at` (when
/// articles were last added) and `files.blob` (a sealed file's articles), in
/// the same order `create_shard` has them, and the `held_back` table. Adding
/// a nullable column doesnt rewrite the table.
pub fn migrate_shard(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "
        -- which release a collection's articles go to, by its keyed hash
        -- (`collections.rs`): a post's files share a release whatever they are called
        create table if not exists collections (key INTEGER PRIMARY KEY, release_id INTEGER NOT NULL) without rowid;
        -- a release's keys, for `junk::remove` (a scan of the table each, minutes for a batch of junk)
        create index if not exists collections_release on collections(release_id);

        -- what a row a save couldnt tell from a copy of its sealed file's
        -- article said about the file, folded in when sealing tells (`seal_rows`)
        create table if not exists held_back (
            file_id INTEGER NOT NULL,
            message_id TEXT NOT NULL,
            subject TEXT,
            part INTEGER,
            total_parts INTEGER,
            file_total INTEGER,
            again INTEGER,
            primary key (file_id, message_id)
        ) without rowid;

        -- every message-id of a release with more than one file, by its keyed
        -- hash (`id_hash`), and the file it is in: one lookup tells whether
        -- another file of the release has it. A release is in it once its
        -- (release_id, 0, 0) row is (`index_release`); a cache, taken again for
        -- releases gone quiet (`ShardWriter::prune_ids_some`).
        create table if not exists release_ids (
            release_id INTEGER NOT NULL,
            id_hash INTEGER NOT NULL,
            file_id INTEGER NOT NULL,
            primary key (release_id, id_hash)
        ) without rowid;
        ",
    )?;
    // the hash key, secret soo posts cant be made to collide
    for key in ["id_key0", "id_key1"] {
        let mut bytes = [0u8; 8];
        getrandom::fill(&mut bytes).map_err(|e| rusqlite::Error::ToSqlConversionFailure(e.to_string().into()))?;
        conn.execute("insert or ignore into meta (key, value) values (?, ?)", params![key, i64::from_le_bytes(bytes)])?;
    }
    let cols: Vec<String> =
        conn.prepare("pragma table_info(files)")?.query_map([], |r| r.get(1))?.collect::<Result<_>>()?;
    for (col, kind) in [("touched_at", "INTEGER"), ("blob", "BLOB"), ("blob_bytes", "INTEGER")] {
        if !cols.iter().any(|c| c == col) {
            conn.execute(&format!("alter table files add column {col} {kind}"), [])?;
        }
    }
    // `blob_bytes` is the bytes of the articles in `blob`, kept by whatever writes the blob (`blob_bytes.rs`). A
    // writer that changes the blob without it leaves it NULL (unknown) rather than wrong, except the move of a blob to
    // the sealed log, which swaps the blob for its place and keeps what's in it
    conn.execute_batch(
        "create trigger if not exists files_blob_bytes_unknown after update of blob on files
         when new.blob_bytes is not null and new.blob_bytes is old.blob_bytes and new.blob is not old.blob
          and (new.blob is null or not (length(new.blob) = 21 and substr(new.blob, 1, 5) = x'FF534C4731'))
         begin update files set blob_bytes = null where id = new.id; end;",
    )?;
    // whether a held-back article's part was the file's already when it was saved (1). Kept, not read: a part
    // under another message-id isnt known to be a repost (different files can share a row). NULL: held back before
    let held: bool = conn.prepare("select 1 from pragma_table_info('held_back') where name = 'again'")?.exists([])?;
    if !held {
        conn.execute("alter table held_back add column again INTEGER", [])?;
    }
    crate::postprocess::schema::migrate(conn)?;
    crate::category::uncategorized::migrate(conn)?;
    crate::word_index::migrate(conn)
}

/// `tests/pp_enqueue_cost.rs` compares a save with and without the queue
#[doc(hidden)]
pub static SKIP_ENQUEUE_FOR_BENCH: AtomicBool = AtomicBool::new(false);

// ---------------------------------------------------------------- ids

/// Release id seqs, taken from the main database a block at a time soo every
/// process (indexer, menu) gets its own.
pub struct Ids {
    main: PathBuf,
    range: Mutex<(i64, i64)>,
}

const ID_BLOCK: i64 = 10_000;

impl Ids {
    pub fn new(main: &Path) -> Ids {
        Ids { main: main.to_path_buf(), range: Mutex::new((0, 0)) }
    }

    pub fn next(&self) -> Result<i64> {
        let mut range = self.range.lock().unwrap();
        if range.0 >= range.1 {
            let mut conn = db::open_at(&self.main)?;
            // on the disk before any of the block is saved to a shard (whose saves are, `db::with_full_flush`):
            // a power cut that kept a save and lost its block would hand its ids out again
            conn.execute_batch("pragma synchronous = full; pragma fullfsync = on")?;
            // an explicit commit: an autocommit's would happen as the RETURNING row is dropped, and a failure
            // there (disk full, a failed sync) wouldnt reach us, the block handed out yet never reserved
            let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let end: i64 = tx.query_row(
                "update meta set value = value + ? where key = 'next_seq' returning value",
                [ID_BLOCK],
                |r| r.get(0),
            )?;
            tx.commit()?;
            *range = (end - ID_BLOCK, end);
        }
        let seq = range.0;
        range.0 += 1;
        Ok(seq)
    }
}

/// Set the counter past `seq` (after converting an old database).
pub fn set_next_seq(main: &Connection, seq: i64) -> Result<()> {
    main.execute("update meta set value = max(value, ?) where key = 'next_seq'", [seq])?;
    Ok(())
}

/// A number kept in the main database's `meta` table, if it's there.
pub fn get_meta(conn: &Connection, key: &str) -> Result<Option<i64>> {
    conn.query_row("select value from main.meta where key = ?", [key], |r| r.get(0)).optional()
}

/// A number from the main database's `meta` table, on either backend.
pub fn meta(db: &dyn crate::pg::sql::Sql, key: &str) -> crate::pg::sql::Result<Option<i64>> {
    Ok(db.opt::<Option<i64>>("select value from meta where key = $1", crate::pg::sql::vals![key])?.flatten())
}

/// Keep a number in the main database's `meta` table, on either backend.
pub fn put_meta(db: &dyn crate::pg::sql::Sql, key: &str, value: i64) -> crate::pg::sql::Result<()> {
    db.execute(
        "insert into meta (key, value) values ($1, $2) on conflict (key) do update set value = excluded.value",
        crate::pg::sql::vals![key, value],
    )?;
    Ok(())
}

/// A text kept in the main database's `meta` table, on either backend: SQLite keeps it in `value` (any type goes),
/// Postgres in `text_value`.
pub fn meta_text(db: &dyn crate::pg::sql::Sql, key: &str) -> crate::pg::sql::Result<Option<String>> {
    let sql = match db.dialect() {
        crate::pg::sql::Dialect::Sqlite => "select cast(value as text) from meta where key = $1",
        crate::pg::sql::Dialect::Postgres => "select text_value from meta where key = $1",
    };
    Ok(db.opt::<Option<String>>(sql, crate::pg::sql::vals![key])?.flatten())
}

/// Keep a text in the main database's `meta` table, on either backend (see `meta_text`).
pub fn put_meta_text(db: &dyn crate::pg::sql::Sql, key: &str, value: &str) -> crate::pg::sql::Result<()> {
    let sql = match db.dialect() {
        crate::pg::sql::Dialect::Sqlite => {
            "insert into meta (key, value) values ($1, $2) on conflict (key) do update set value = excluded.value"
        }
        crate::pg::sql::Dialect::Postgres => {
            "insert into meta (key, text_value) values ($1, $2) on conflict (key) do update set text_value = excluded.text_value"
        }
    };
    db.execute(sql, crate::pg::sql::vals![key, value])?;
    Ok(())
}

/// Keep a number in the main database's `meta` table.
pub fn set_meta(conn: &Connection, key: &str, value: i64) -> Result<()> {
    conn.execute("insert or replace into main.meta (key, value) values (?, ?)", params![key, value])?;
    Ok(())
}

// ---------------------------------------------------------------- message-ids

const TEXT: u8 = 0;
const HEX_LOWER: u8 = 1;
const HEX_UPPER: u8 = 2;

const DIGITS: u8 = 3;
const BASE36_LOWER: u8 = 4;
const BASE36_UPPER: u8 = 5;
const BASE62: u8 = 6;
const BASE64_URL: u8 = 7;

/// the alphabet of each packed tag, by digit value
fn alphabet(tag: u8) -> &'static [u8] {
    match tag {
        DIGITS => b"0123456789",
        BASE36_LOWER => b"0123456789abcdefghijklmnopqrstuvwxyz",
        BASE36_UPPER => b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ",
        BASE62 => b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz",
        BASE64_URL => b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz-_",
        _ => b"",
    }
}

/// digits of base `n` that fit one u32 limb step: the most `c` with `n^c <= 2^32`, and `n^c`
fn chunk(n: u32) -> (usize, u64) {
    let (mut c, mut m) = (0usize, 1u64);
    while m * u64::from(n) <= 1 << 32 {
        m *= u64::from(n);
        c += 1;
    }
    (c, m)
}

/// big-endian base-256 bytes of a base-`n` number given as digit values, no leading zero byte (none at all for 0)
fn to_bytes(digits: &[u8], n: u32) -> Vec<u8> {
    let (c, _) = chunk(n);
    // little-endian u32 limbs, a chunk of digits folded in at a time
    let mut limbs: Vec<u32> = Vec::with_capacity(digits.len() / 4 + 1);
    for group in digits.chunks(c) {
        let (mut m, mut v) = (1u64, 0u64);
        for &d in group {
            m *= u64::from(n);
            v = v * u64::from(n) + u64::from(d);
        }
        let mut carry = v;
        for limb in limbs.iter_mut() {
            let t = u64::from(*limb) * m + carry;
            *limb = t as u32;
            carry = t >> 32;
        }
        if carry > 0 {
            limbs.push(carry as u32);
        }
    }
    let mut out = Vec::with_capacity(limbs.len() * 4);
    for limb in limbs.iter().rev() {
        out.extend_from_slice(&limb.to_be_bytes());
    }
    let zeros = out.iter().take_while(|b| **b == 0).count();
    out.drain(..zeros);
    out
}

/// `len` base-`n` digit values of a big-endian base-256 number (the lowest `len`, when it has more)
fn from_bytes(bytes: &[u8], n: u32, len: usize) -> Vec<u8> {
    let (c, m) = chunk(n);
    // big-endian u32 limbs
    let mut limbs: Vec<u32> = Vec::with_capacity(bytes.len() / 4 + 1);
    let head = bytes.len() % 4;
    if head > 0 {
        limbs.push(bytes[..head].iter().fold(0u32, |v, b| (v << 8) | u32::from(*b)));
    }
    limbs.extend(bytes[head..].chunks(4).map(|w| u32::from_be_bytes([w[0], w[1], w[2], w[3]])));
    let mut digits = vec![0u8; len];
    let mut at = len;
    let mut start = 0;
    while at > 0 {
        while start < limbs.len() && limbs[start] == 0 {
            start += 1;
        }
        if start == limbs.len() {
            // the rest are leading zeros
            break;
        }
        let mut rem = 0u64;
        for limb in &mut limbs[start..] {
            let v = (rem << 32) | u64::from(*limb);
            *limb = (v / m) as u32;
            rem = v % m;
        }
        for _ in 0..c {
            if at == 0 {
                break;
            }
            at -= 1;
            digits[at] = (rem % u64::from(n)) as u8;
            rem /= u64::from(n);
        }
    }
    digits
}

/// Which characters each packed alphabet takes: bit 0 digits, 1 lowercase, 2 uppercase, 3 `-` and `_`, 4 anything
/// else
fn char_class(b: u8) -> u8 {
    match b {
        b'0'..=b'9' => 1,
        b'a'..=b'z' => 2,
        b'A'..=b'Z' => 4,
        b'-' | b'_' => 8,
        _ => 16,
    }
}

/// a character's digit value in a packed alphabet it is in
fn digit_value(tag: u8, b: u8) -> u8 {
    match (tag, b) {
        (_, b'0'..=b'9') => b - b'0',
        (BASE36_LOWER, b'a'..=b'z') => b - b'a' + 10,
        (_, b'A'..=b'Z') => b - b'A' + 10,
        (_, b'a'..=b'z') => b - b'a' + 36,
        (_, b'-') => 62,
        _ => 63,
    }
}

/// Packs a message-id's local part by its shape: hex at half the size
/// (tags 1, 2), other single-alphabet ids as a base-N number (tags 3-7,
/// then `[len]`), anything else as text (tag 0). Deterministic, and
/// `unpack_local` gives back exactly the same string.
pub(crate) fn pack_local(local: &str) -> Vec<u8> {
    let bytes = local.as_bytes();
    let mut classes = 0u8;
    let mut hex_lower = true;
    let mut hex_upper = true;
    for &b in bytes {
        classes |= char_class(b);
        hex_lower &= b.is_ascii_digit() || (b'a'..=b'f').contains(&b);
        hex_upper &= b.is_ascii_digit() || (b'A'..=b'F').contains(&b);
    }
    let even = !bytes.is_empty() && bytes.len().is_multiple_of(2);
    if even && (hex_lower || hex_upper) {
        let nibble = |c: u8| (c as char).to_digit(16).unwrap_or(0) as u8;
        let mut out = Vec::with_capacity(1 + bytes.len() / 2);
        out.push(if hex_lower { HEX_LOWER } else { HEX_UPPER });
        out.extend(bytes.chunks(2).map(|p| (nibble(p[0]) << 4) | nibble(p[1])));
        return out;
    }
    // the first alphabet, in the order DIGITS, BASE36_LOWER, BASE36_UPPER, BASE62, BASE64_URL, with every character
    let tag = match classes {
        _ if !(1..=255).contains(&bytes.len()) || classes & 16 != 0 => TEXT,
        1 => DIGITS,
        c if c & 8 != 0 => BASE64_URL,
        c if c & 6 == 6 => BASE62,
        c if c & 2 != 0 => BASE36_LOWER,
        _ => BASE36_UPPER,
    };
    if tag == TEXT {
        let mut out = Vec::with_capacity(1 + bytes.len());
        out.push(TEXT);
        out.extend_from_slice(bytes);
        return out;
    }
    let values: Vec<u8> = bytes.iter().map(|&b| digit_value(tag, b)).collect();
    let mut out = vec![tag, bytes.len() as u8];
    out.extend(to_bytes(&values, alphabet(tag).len() as u32));
    out
}

/// A local part back from any of the packings (tags 0-7).
pub(crate) fn unpack_local(blob: &[u8]) -> String {
    const LOWER: &[u8; 16] = b"0123456789abcdef";
    const UPPER: &[u8; 16] = b"0123456789ABCDEF";
    let hex = |digits: &[u8; 16]| -> String {
        let mut out = String::with_capacity(2 * blob.len());
        for b in &blob[1..] {
            out.push(digits[usize::from(b >> 4)] as char);
            out.push(digits[usize::from(b & 15)] as char);
        }
        out
    };
    match blob.first() {
        Some(&HEX_LOWER) => hex(LOWER),
        Some(&HEX_UPPER) => hex(UPPER),
        Some(&tag) if (DIGITS..=BASE64_URL).contains(&tag) && blob.len() >= 2 => {
            let abc = alphabet(tag);
            from_bytes(&blob[2..], abc.len() as u32, blob[1] as usize)
                .iter()
                .map(|&d| abc.get(usize::from(d)).copied().unwrap_or(b'0') as char)
                .collect()
        }
        _ => String::from_utf8_lossy(blob.get(1..).unwrap_or_default()).into_owned(),
    }
}

/// a local packed the current way, whatever way it was packed before
pub(crate) fn repack(local: &[u8]) -> Vec<u8> {
    pack_local(&unpack_local(local))
}

/// `<local@domain>` -> (local, "@domain>"); anything else stays whole
pub(crate) fn split_message_id(id: &str) -> Option<(&str, &str)> {
    let inner = id.strip_prefix('<')?;
    if !id.ends_with('>') {
        return None;
    }
    let at = inner.rfind('@')?;
    Some((&inner[..at], &inner[at..]))
}

/// A new domain row, under the next id below `1 << 32`: a small id keeps segment rows and blobs short.
pub(crate) const NEW_DOMAIN: &str = "insert into domains (id, suffix) values ((select coalesce(max(id), 0) + 1 from domains where id < 4294967296), ?1)";

/// `domains` without its unique index on the suffix, for a copy that takes one suffix under several ids (a
/// restore's `frozen::Domains::Keep`). A lookup by suffix takes the lowest id (`Domains`), and every id reads back
/// the same message-id.
pub(crate) fn domains_not_unique(conn: &Connection) -> Result<()> {
    let sql: Option<String> = conn
        .query_row("select sql from main.sqlite_master where type = 'table' and name = 'domains'", [], |r| r.get(0))
        .optional()?;
    if !sql.is_some_and(|s| s.to_ascii_uppercase().contains("UNIQUE")) {
        return Ok(());
    }
    conn.execute_batch(
        "begin immediate;
         create table domains_merged (id INTEGER PRIMARY KEY, suffix TEXT NOT NULL);
         insert into domains_merged (id, suffix) select id, suffix from main.domains;
         drop table main.domains;
         alter table domains_merged rename to domains;
         create index if not exists domains_suffix on domains (suffix, id);
         commit;",
    )?;
    Ok(())
}

/// The domains a shard's writer knows. Only domains shared by several
/// articles get a row in `domains`: some posting tools make up a new domain
/// for every article, and those message-ids are stored whole instead. A
/// domain gets its row once one writer has seen it `SHARED_AFTER` times.
#[derive(Default)]
pub struct Domains {
    /// known domains (in the table), bounded
    ids: HashMap<String, i64>,
    /// domains not in the table yet: how often they came up, bounded
    seen: HashMap<String, u32>,
    /// rows this writer inserted in its open transaction: they move to `ids`
    /// once it commits, and are gone if it rolls back
    staged: HashMap<String, i64>,
    /// the table was read
    loaded: bool,
    /// `ids` has every row of the table (it had at most `DOMAIN_TABLE_MAX`): a domain not there has no row, and
    /// isnt looked up until it would get a row (a thaw adds rows outside here)
    complete: bool,
}

/// domains a writer keeps all of in memory, when the table has no more (`Domains::complete`)
const DOMAIN_TABLE_MAX: usize = 1_000_000;

/// domains a writer keeps in memory at most, known and not yet known
const DOMAIN_CACHE: usize = 100_000;
/// articles that share a domain before it gets a row
pub const SHARED_AFTER: u32 = 3;

/// a message-id stored whole (domain 0)
pub(crate) fn whole(id: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(1 + id.len());
    out.push(TEXT);
    out.extend_from_slice(id.as_bytes());
    out
}

impl Domains {
    /// (local blob, domain id) for a message-id; domain 0 = stored whole
    pub(crate) fn encode(&mut self, conn: &Connection, id: &str) -> Result<(Vec<u8>, i64)> {
        let Some((local, suffix)) = split_message_id(id) else { return Ok((whole(id), 0)) };
        if !self.loaded {
            self.load(conn)?;
        }
        if let Some(d) = self.ids.get(suffix).or_else(|| self.staged.get(suffix)) {
            return Ok((pack_local(local), *d));
        }
        let found: Option<i64> = if self.complete {
            None
        } else {
            conn.prepare_cached("select id from domains where suffix = ? order by id limit 1")?
                .query_row([suffix], |r| r.get(0))
                .optional()?
        };
        let domain = match found {
            Some(d) => d,
            None => {
                if self.seen.len() >= DOMAIN_CACHE {
                    self.seen.clear();
                }
                let count = self.seen.entry(suffix.to_string()).or_insert(0);
                *count += 1;
                if *count < SHARED_AFTER {
                    return Ok((whole(id), 0));
                }
                self.seen.remove(suffix);
                // a row added outside this writer (a thaw, by hand or of late articles) isn't in `ids`: look before
                // adding one, soo the suffix's UNIQUE never fails the save
                if self.complete
                    && let Some(d) = conn
                        .prepare_cached("select id from domains where suffix = ? order by id limit 1")?
                        .query_row([suffix], |r| r.get::<_, i64>(0))
                        .optional()?
                {
                    self.remember(suffix.to_string(), d);
                    return Ok((pack_local(local), d));
                }
                conn.prepare_cached(NEW_DOMAIN)?.execute([suffix])?;
                let domain = conn.last_insert_rowid();
                self.staged.insert(suffix.to_string(), domain);
                return Ok((pack_local(local), domain));
            }
        };
        self.remember(suffix.to_string(), domain);
        Ok((pack_local(local), domain))
    }

    /// Every row of the table into `ids`, when it has at most `DOMAIN_TABLE_MAX`; a suffix with two rows (folded
    /// shards) by its lowest id, as the lookup takes it.
    fn load(&mut self, conn: &Connection) -> Result<()> {
        self.loaded = true;
        let rows: i64 = conn.prepare_cached("select count(*) from domains")?.query_row([], |r| r.get(0))?;
        if rows as usize > DOMAIN_TABLE_MAX {
            return Ok(());
        }
        let mut stmt = conn.prepare("select suffix, min(id) from domains group by suffix")?;
        let all = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
        self.ids.clear();
        for row in all {
            let (suffix, id) = row?;
            self.ids.insert(suffix, id);
        }
        self.complete = true;
        Ok(())
    }

    fn remember(&mut self, suffix: String, domain: i64) {
        if self.complete && self.ids.len() >= DOMAIN_TABLE_MAX {
            // too many to keep all: back to looking up what isnt kept
            self.complete = false;
            self.ids.clear();
        } else if !self.complete && self.ids.len() >= DOMAIN_CACHE {
            self.ids.clear();
        }
        self.ids.insert(suffix, domain);
    }

    /// The transaction `encode` was used in committed: its new rows are known.
    pub(crate) fn committed(&mut self) {
        for (suffix, domain) in std::mem::take(&mut self.staged) {
            self.remember(suffix, domain);
        }
    }

    /// The transaction `encode` was used in rolled back: its new rows are gone.
    pub(crate) fn rolled_back(&mut self) {
        self.staged.clear();
        // what was read inside the transaction may hold rows it added (a thaw): read the table again
        self.ids.clear();
        self.loaded = false;
        self.complete = false;
    }
}

/// What decides two stored segments are the same article: the message-id as
/// text, however it is stored (whole, packed, or packed the old way under a
/// domain). A domain with no suffix row can't be told apart from another
/// id, soo it keeps its own (domain, local) as the key.
pub(crate) fn dedup_key(local: &[u8], domain: i64, suffix: Option<&str>) -> String {
    match (domain, suffix) {
        (0, _) | (_, Some(_)) => decode(local, suffix),
        _ => format!("\0{domain}\0{}", local.iter().map(|b| format!("{b:02x}")).collect::<String>()),
    }
}

/// A message-id back from its packed local part and its domain's suffix
/// (none for domain 0, kept whole).
pub(crate) fn decode(local: &[u8], suffix: Option<&str>) -> String {
    match suffix {
        None => unpack_local(local),
        Some(suffix) => format!("<{}{suffix}", unpack_local(local)),
    }
}

// ---------------------------------------------------------------- part bitmaps

/// When each release's `release_ids` were last made in full (`index_release`): a release indexed again without a
/// new article (all copies) isn't quiet for the pruning walk till SEAL_AGE after
/// (`ShardWriter::prune_ids_some`).
pub const RELEASE_INDEXED: ShardTable = ShardTable {
    name: "release_indexed",
    create: "create table if not exists release_indexed (
        release_id INTEGER PRIMARY KEY,
        at         INTEGER NOT NULL
    )",
    indexes: &[],
    release_key: Some("release_id"),
    added: &[],
};

/// What the overview said about a file's articles, folded in as they are
/// saved, for the Where and when, Health and Keys facts of the release page:
/// the first and last Date (the poster's clock, zone dropped), the first and
/// last article number, the Lines sum, how many carried References, the most
/// groups an Xref named (only servers with `Xref:full` send one), how the From
/// changed, whether a subject had an RFC 2047 word, how many articles came
/// after the file sealed, and how many articles all that covers. One row a
/// file, written by `Timing::write`; a release saved before the table has
/// none, and its facts stay off.
pub const FILE_TIMING: ShardTable = ShardTable {
    name: "file_timing",
    create: "create table if not exists file_timing (
        file_id         INTEGER PRIMARY KEY,
        release_id      INTEGER NOT NULL,
        first_date      INTEGER,
        last_date       INTEGER,
        number_first    INTEGER,
        number_last     INTEGER,
        lines           INTEGER,
        references_seen INTEGER NOT NULL DEFAULT 0,
        xref_groups_max INTEGER,
        from_hash       INTEGER,
        from_variants   INTEGER NOT NULL DEFAULT 1,
        encoded_subject INTEGER NOT NULL DEFAULT 0,
        late_articles   INTEGER NOT NULL DEFAULT 0,
        articles        INTEGER NOT NULL DEFAULT 0
    )",
    indexes: &[("file_timing_release", "create index if not exists file_timing_release on file_timing(release_id)")],
    release_key: Some("release_id"),
    // what the message-ids say (`discovery::id_facts`): the posting tool, the first and last millisecond the tool
    // or the server wrote into an id, and on Highwinds the host, session key and the first and last counter
    added: &[
        ("tool", "INTEGER"),
        ("clock_first", "INTEGER"),
        ("clock_last", "INTEGER"),
        ("hw_host", "TEXT"),
        ("hw_key", "TEXT"),
        ("hw_n_first", "INTEGER"),
        ("hw_n_last", "INTEGER"),
    ],
};

/// The rows `split_uploads` made of one release, one per upload: each row's id and name and its upload's first and
/// last Date (the poster's clock, as `file_timing` keeps them). A save sends each article of the release to the row
/// of the upload whose window its Date is in or nearest (`route_uploads`), soo a reread of a later upload's window
/// fills that upload's row, not the first's. The rows go with the first upload's row (`origin_id`).
pub const UPLOAD_ROUTES: ShardTable = ShardTable {
    name: "upload_routes",
    create: "create table if not exists upload_routes (
        origin_id  INTEGER NOT NULL,
        release_id INTEGER NOT NULL,
        name       TEXT NOT NULL,
        first_date INTEGER,
        last_date  INTEGER,
        primary key (origin_id, release_id)
    ) without rowid",
    indexes: &[],
    release_key: Some("origin_id"),
    added: &[],
};

/// How far outside every upload's window an article's Date may be and still go to the nearest upload's row
/// (`route_uploads`): half the least gap `repost_split::decide` takes between uploads, soo an article never lands
/// in a neighbour's row, and a new upload started past it isnt taken in. Further out, it goes where the save would
/// put it anyway. Less when two of the release's windows are nearer.
/// One release's `ArticleFacts`, an article each in order, and whether a file name in it is a program
/// (`junk::is_executable`).
#[derive(Debug, Default)]
pub(crate) struct PreparedRelease {
    articles: Vec<ArticleFacts>,
    executable: bool,
}

/// The prepared releases of slices, by where each release's articles are in memory: a release the save takes as it
/// came finds its facts, and one it cut into pieces (`route_uploads`, `split_old_rows`) works them out itself.
pub(crate) type Prepared = HashMap<usize, PreparedRelease>;

/// `Prepared` for a slice's releases, for `ShardWriter::save_prepared`. Off the writer's thread.
pub(crate) fn prepare(releases: &[Release]) -> Prepared {
    releases
        .iter()
        .map(|r| {
            let prepared = PreparedRelease {
                articles: r.articles.iter().map(ArticleFacts::of).collect(),
                executable: r.articles.iter().any(|a| a.filename.as_deref().is_some_and(crate::junk::is_executable)),
            };
            (r.articles.as_ptr() as usize, prepared)
        })
        .collect()
}

/// A release's prepared facts, when they are its own: the same articles, as many.
fn prepared_for<'p>(prepared: &'p Prepared, release: &Release) -> Option<&'p PreparedRelease> {
    prepared.get(&(release.articles.as_ptr() as usize)).filter(|p| p.articles.len() == release.articles.len())
}

/// One save's articles of one file, as `file_timing` keeps them. `from_variants`
/// counts runs of one From in arrival order, soo a poster that rotates
/// identities counts high and a steady one stays at 1; `from_hash` is the From
/// the run ended on, for the next save to continue the count from.
#[derive(Debug, Default)]
struct Timing {
    articles: i64,
    first_date: Option<i64>,
    last_date: Option<i64>,
    number_first: Option<u64>,
    number_last: Option<u64>,
    lines: i64,
    references: i64,
    xref_max: Option<i64>,
    from_first: Option<i64>,
    from_last: Option<i64>,
    from_runs: i64,
    encoded_subject: bool,
    tool: Option<i64>,
    clock_first: Option<i64>,
    clock_last: Option<i64>,
    hw_host: Option<String>,
    hw_key: Option<String>,
    hw_n_first: Option<i64>,
    hw_n_last: Option<i64>,
}

impl Timing {
    /// `facts`: what `prepare` read off the article already, or None to read it here
    fn fold(&mut self, a: &Article, key: &IdKey, facts: Option<&ArticleFacts>) {
        self.articles += 1;
        let here;
        let facts = match facts {
            Some(f) => f,
            None => {
                here = ArticleFacts::of(a);
                &here
            }
        };
        if let Some(ts) = facts.date {
            self.first_date = Some(self.first_date.map_or(ts, |f| f.min(ts)));
            self.last_date = Some(self.last_date.map_or(ts, |l| l.max(ts)));
        }
        let facts = &facts.id;
        if let Some(tool) = facts.tool {
            self.tool.get_or_insert(tool.code());
        }
        if let Some(ms) = facts.clock_ms {
            self.clock_first = Some(self.clock_first.map_or(ms, |f| f.min(ms)));
            self.clock_last = Some(self.clock_last.map_or(ms, |l| l.max(ms)));
        }
        if let (Some(host), Some(hw_key), Some(n)) = (&facts.hw_host, &facts.hw_key, facts.hw_n) {
            self.hw_host.get_or_insert_with(|| host.clone());
            self.hw_key.get_or_insert_with(|| hw_key.clone());
            self.hw_n_first = Some(self.hw_n_first.map_or(n, |f| f.min(n)));
            self.hw_n_last = Some(self.hw_n_last.map_or(n, |l| l.max(n)));
        }
        // 0 is what a header without a number parses to
        if a.number > 0 {
            self.number_first = Some(self.number_first.map_or(a.number, |n| n.min(a.number)));
            self.number_last = Some(self.number_last.map_or(a.number, |n| n.max(a.number)));
        }
        self.lines += a.lines.max(0);
        if !a.references.trim().is_empty() {
            self.references += 1;
        }
        if let Some(groups) = a.xref_groups {
            let groups = i64::from(groups);
            self.xref_max = Some(self.xref_max.map_or(groups, |m| m.max(groups)));
        }
        // the shard's id key: never 0, and the From itself is not what is kept
        let from = key.hash(&a.author);
        if self.from_last != Some(from) {
            self.from_runs += 1;
        }
        self.from_first.get_or_insert(from);
        self.from_last = Some(from);
        self.encoded_subject |= a.subject.contains("=?") && a.subject.contains("?=");
    }

    /// Fold this save into the file's row. `late`: the file was sealed before
    /// these arrived. The dates, numbers and the Xref count keep the extremes,
    /// the counts add up, and a From run that continues the last save's is one.
    fn write(&self, conn: &Connection, file_id: i64, release_id: i64, late: bool) -> Result<()> {
        if self.articles == 0 {
            return Ok(());
        }
        conn.prepare_cached(
            "insert into file_timing (file_id, release_id, first_date, last_date, number_first, number_last, lines,
                                      references_seen, xref_groups_max, from_hash, from_variants, encoded_subject,
                                      late_articles, articles, tool, clock_first, clock_last, hw_host, hw_key,
                                      hw_n_first, hw_n_last)
             values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?16, ?17, ?18, ?19, ?20, ?21, ?22)
             on conflict(file_id) do update set
                release_id = excluded.release_id,
                tool = coalesce(file_timing.tool, excluded.tool),
                clock_first = min(coalesce(file_timing.clock_first, excluded.clock_first),
                                  coalesce(excluded.clock_first, file_timing.clock_first)),
                clock_last = max(coalesce(file_timing.clock_last, excluded.clock_last),
                                 coalesce(excluded.clock_last, file_timing.clock_last)),
                hw_host = coalesce(file_timing.hw_host, excluded.hw_host),
                hw_key = coalesce(file_timing.hw_key, excluded.hw_key),
                hw_n_first = min(coalesce(file_timing.hw_n_first, excluded.hw_n_first),
                                 coalesce(excluded.hw_n_first, file_timing.hw_n_first)),
                hw_n_last = max(coalesce(file_timing.hw_n_last, excluded.hw_n_last),
                                coalesce(excluded.hw_n_last, file_timing.hw_n_last)),
                first_date = min(coalesce(file_timing.first_date, excluded.first_date),
                                 coalesce(excluded.first_date, file_timing.first_date)),
                last_date = max(coalesce(file_timing.last_date, excluded.last_date),
                                coalesce(excluded.last_date, file_timing.last_date)),
                number_first = min(coalesce(file_timing.number_first, excluded.number_first),
                                   coalesce(excluded.number_first, file_timing.number_first)),
                number_last = max(coalesce(file_timing.number_last, excluded.number_last),
                                  coalesce(excluded.number_last, file_timing.number_last)),
                lines = coalesce(file_timing.lines, 0) + coalesce(excluded.lines, 0),
                references_seen = file_timing.references_seen + excluded.references_seen,
                xref_groups_max = max(coalesce(file_timing.xref_groups_max, excluded.xref_groups_max),
                                      coalesce(excluded.xref_groups_max, file_timing.xref_groups_max)),
                from_variants = file_timing.from_variants + excluded.from_variants
                                - (case when file_timing.from_hash = ?15 then 1 else 0 end),
                from_hash = excluded.from_hash,
                encoded_subject = max(file_timing.encoded_subject, excluded.encoded_subject),
                late_articles = file_timing.late_articles + excluded.late_articles,
                articles = file_timing.articles + excluded.articles",
        )?
        .execute(params![
            file_id,
            release_id,
            self.first_date,
            self.last_date,
            self.number_first.map(|n| n as i64),
            self.number_last.map(|n| n as i64),
            self.lines,
            self.references,
            self.xref_max,
            self.from_last,
            self.from_runs,
            self.encoded_subject as i64,
            if late { self.articles } else { 0 },
            self.articles,
            self.from_first,
            self.tool,
            self.clock_first,
            self.clock_last,
            self.hw_host,
            self.hw_key,
            self.hw_n_first,
            self.hw_n_last,
        ])?;
        Ok(())
    }
}

/// One file of a release as the shard stores it.
#[derive(Clone, Debug, Default)]
pub(crate) struct FileState {
    pub id: i64,
    pub subject: Option<String>,
    pub subject_part: Option<i64>,
    pub subject_mid: Option<String>,
    pub expected: Option<i64>,
    pub file_total: Option<i64>,
    pub seen: Vec<u8>,
    /// its articles are in `files.blob` (plus any rows that came later)
    pub sealed: bool,
    /// made by this save, and no article stored whole in it yet: nothing in it was stored another way that
    /// `add_segment` has to look for (whole before its domain got a row, or text from before the packing)
    pub fresh: std::cell::Cell<bool>,
}

impl FileState {
    /// fold one article in
    pub fn add(&mut self, a: &Article) {
        let better = match (&self.subject, &self.subject_mid) {
            (Some(_), Some(mid)) => subject_key(a.part, &a.message_id) < subject_key(self.subject_part, mid),
            _ => true,
        };
        if better {
            self.subject = Some(a.subject.clone());
            self.subject_part = a.part;
            self.subject_mid = Some(a.message_id.clone());
        }
        if let Some(t) = a.total_parts {
            self.expected = Some(self.expected.map_or(t, |e| e.max(t)));
        }
        if let Some(p) = a.part {
            insert_part(&mut self.seen, p);
        }
        if let Some(ft) = a.file_total {
            self.file_total = Some(self.file_total.map_or(ft, |e| e.max(ft)));
        }
    }
}

/// Look up a file of a release, or make it.
pub(crate) fn file(conn: &Connection, release_id: i64, filename: &str) -> Result<FileState> {
    let found = conn
        .prepare_cached(
            "select id, subject, subject_part, subject_mid, expected, file_total, seen, blob is not null from files
             where release_id = ? and filename = ?",
        )?
        .query_row(params![release_id, filename], |r| {
            Ok(FileState {
                id: r.get(0)?,
                subject: r.get(1)?,
                subject_part: r.get(2)?,
                subject_mid: r.get(3)?,
                expected: r.get(4)?,
                file_total: r.get(5)?,
                seen: r.get(6)?,
                sealed: r.get(7)?,
                fresh: std::cell::Cell::new(false),
            })
        })
        .optional()?;
    if let Some(f) = found {
        return Ok(f);
    }
    conn.prepare_cached("insert into files (release_id, filename, seen) values (?, ?, x'')")?
        .execute(params![release_id, filename])?;
    Ok(FileState { id: conn.last_insert_rowid(), fresh: std::cell::Cell::new(true), ..FileState::default() })
}

pub(crate) fn put_file(conn: &Connection, f: &FileState) -> Result<()> {
    conn.prepare_cached(
        "update files set subject = ?, subject_part = ?, subject_mid = ?, expected = ?, file_total = ?, seen = ?,
         touched_at = unixepoch() where id = ?",
    )?
    .execute(params![f.subject, f.subject_part, f.subject_mid, f.expected, f.file_total, f.seen, f.id])?;
    Ok(())
}

/// `add_segment_with`, its statements prepared for the one article.
#[cfg(test)]
pub(crate) fn add_segment(
    conn: &Connection,
    domains: &mut Domains,
    f: &FileState,
    index: Option<(i64, &IdKey)>,
    a: &Article,
    sealed: &mut SealedCache,
) -> Result<bool> {
    add_segment_with(conn, &mut SegmentStatements::new(conn)?, domains, f, index, None, a, sealed)
}

/// A release's `release_ids` rows (id hash -> file), its own one left out: for `add_segment_with` to look in instead
/// of the table, for a release made by the save, whose rows only that save adds.
fn release_ids_of(conn: &Connection, release_id: i64) -> Result<HashMap<i64, i64>> {
    conn.prepare_cached("select id_hash, file_id from release_ids where release_id = ? and id_hash != 0")?
        .query_map([release_id], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect()
}

/// The statements `add_segment` runs for each article, prepared once for a whole save: taking each from the
/// connection's cache again for every article (hashing its SQL to find it) was a few percent of the writer's time.
pub(crate) struct SegmentStatements<'c> {
    index_lookup: rusqlite::CachedStatement<'c>,
    stored: rusqlite::CachedStatement<'c>,
    insert: rusqlite::CachedStatement<'c>,
    index_insert: rusqlite::CachedStatement<'c>,
}

impl<'c> SegmentStatements<'c> {
    pub(crate) fn new(conn: &'c Connection) -> Result<SegmentStatements<'c>> {
        Ok(SegmentStatements {
            index_lookup: conn
                .prepare_cached("select file_id from release_ids where release_id = ? and id_hash = ?")?,
            stored: conn.prepare_cached("select 1 from segments where file_id = ? and local = ? and domain = ?")?,
            insert: conn.prepare_cached(
                "insert or ignore into segments (file_id, local, domain, part, bytes) values (?, ?, ?, ?, ?)",
            )?,
            index_insert: conn
                .prepare_cached("insert or ignore into release_ids (release_id, id_hash, file_id) values (?, ?, ?)")?,
        })
    }
}

/// Add one article to a file; false when it was already there, as a row or
/// in the file's sealed blob, or in another file of its release (`index`,
/// the release and the hash key when it has more than one file: one
/// message-id saved under two parsed file names is one article). `mirror`, with `index`: every row the release
/// has in the index, looked in instead of the table and kept up with it (`release_ids_of`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn add_segment_with(
    conn: &Connection,
    stmts: &mut SegmentStatements<'_>,
    domains: &mut Domains,
    f: &FileState,
    index: Option<(i64, &IdKey)>,
    mut mirror: Option<&mut HashMap<i64, i64>>,
    a: &Article,
    sealed: &mut SealedCache,
) -> Result<bool> {
    // the release's index has every id of its files, sealed or not: no guess,
    // and no blob decoded for an article its own sealed file already has (a
    // save past its decode budget would keep that one as a row, counted twice
    // in the release's parts and size till the file seals again)
    let hash = index.map(|(_, key)| key.hash(&a.message_id));
    #[cfg(test)]
    if hash.is_some() {
        LOOKUPS.set(LOOKUPS.get() + 1);
    }
    let indexed = |stmts: &mut SegmentStatements<'_>, release_id: i64, hash: i64| -> Result<Option<i64>> {
        match mirror.as_deref() {
            Some(rows) => Ok(rows.get(&hash).copied()),
            None => stmts.index_lookup.query_row(params![release_id, hash], |r| r.get::<_, i64>(0)).optional(),
        }
    };
    if let (Some((release_id, _)), Some(hash)) = (index, hash)
        && let Some(file_id) = indexed(stmts, release_id, hash)?
        && (file_id != f.id || f.sealed)
    {
        return Ok(false);
    }
    let (local, domain) = domains.encode(conn, &a.message_id)?;
    let mut stored = |sealed: &mut SealedCache, local: &[u8], domain: i64| -> Result<bool> {
        let row = stmts.stored.exists(params![f.id, local, domain])?;
        Ok(row || (f.sealed && sealed.contains(conn, f.id, local, domain)?))
    };
    if f.sealed && sealed.contains(conn, f.id, &local, domain)? {
        return Ok(false);
    }
    // a file this save made has only what this save put in it
    let fresh = f.fresh.get();
    if domain == 0 {
        f.fresh.set(false);
    }
    // an article saved before its domain got a row is stored whole: dont save it twice
    if domain != 0 && !fresh && stored(sealed, &whole(&a.message_id), 0)? {
        return Ok(false);
    }
    // a row from before locals were packed by alphabet has the same local stored as text
    if !fresh && (DIGITS..=BASE64_URL).contains(&local[0]) {
        let mut legacy = vec![TEXT];
        legacy.extend_from_slice(unpack_local(&local).as_bytes());
        if stored(sealed, &legacy, domain)? {
            return Ok(false);
        }
    }
    let added = stmts.insert.execute(params![f.id, local, domain, a.part, a.bytes])?;
    if added > 0 && f.sealed && sealed.guessed {
        conn.prepare_cached(
            "insert or replace into held_back (file_id, message_id, subject, part, total_parts, file_total, again)
             values (?, ?, ?, ?, ?, ?, ?)",
        )?
        .execute(params![
            f.id,
            a.message_id,
            a.subject,
            a.part,
            a.total_parts,
            a.file_total,
            a.part.is_some_and(|p| has_part(&f.seen, p)) as i64
        ])?;
    }
    if added > 0
        && let (Some((release_id, _)), Some(hash)) = (index, hash)
    {
        stmts.index_insert.execute(params![release_id, hash, f.id])?;
        if let Some(rows) = mirror.as_deref_mut() {
            rows.entry(hash).or_insert(f.id);
        }
    }
    Ok(added > 0)
}

#[cfg(test)]
thread_local! {
    /// lookups in other files of a release, for counting their cost
    static LOOKUPS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The key of `release_ids`' hashes, the shard's own.
pub(crate) struct IdKey {
    k0: u64,
    k1: u64,
}

impl IdKey {
    pub(crate) fn of(conn: &Connection) -> Result<IdKey> {
        let get =
            |key: &str| -> Result<i64> { conn.query_row("select value from meta where key = ?", [key], |r| r.get(0)) };
        Ok(IdKey { k0: get("id_key0")? as u64, k1: get("id_key1")? as u64 })
    }

    /// a message-id's hash, as `dedup_key` has it; never 0 (the release's own row)
    pub(crate) fn hash(&self, id: &str) -> i64 {
        keyed_hash(self.k0, self.k1, id)
    }
}

/// Every message-id of file `file_id`, sealed or not, by its keyed hash, to `each`, till it says false.
pub(crate) fn file_id_hashes(
    conn: &Connection,
    key: &IdKey,
    file_id: i64,
    suffixes: &mut HashMap<i64, Option<String>>,
    mut each: impl FnMut(i64) -> Result<bool>,
) -> Result<()> {
    // the rows first: no blob read when they answer already
    let mut rows = conn.prepare_cached("select local, domain from segments where file_id = ?")?;
    let mut cursor = rows.query([file_id])?;
    // the rows are not bounded, so they stream from the cursor one at a time
    while let Some(r) = cursor.next()? {
        let s = crate::blob::Seg { local: r.get(0)?, domain: r.get(1)?, part: None, bytes: 0 };
        if !each(key.hash(&seg_key(conn, suffixes, &s)?))? {
            return Ok(());
        }
    }
    // the blob is bounded by READ_MAX_SEGMENTS
    for s in sealed_segments(conn, file_id)?.unwrap_or_default() {
        if !each(key.hash(&seg_key(conn, suffixes, &s)?))? {
            return Ok(());
        }
    }
    Ok(())
}

/// Whether `release_id` has its index of message-ids: its own (release_id, 0, 0) row.
pub(crate) fn is_indexed(conn: &Connection, release_id: i64) -> Result<bool> {
    conn.prepare_cached("select 1 from release_ids where release_id = ? and id_hash = 0")?.exists([release_id])
}

/// Drop `release_id`'s index of message-ids and its stamp: made again in full by the next save or spot that needs it.
pub(crate) fn unindex_release(conn: &Connection, release_id: i64) -> Result<()> {
    conn.prepare_cached("delete from release_ids where release_id = ?")?.execute([release_id])?;
    conn.prepare_cached("delete from release_indexed where release_id = ?")?.execute([release_id])?;
    Ok(())
}

/// Release `release_id` in `release_ids`, every message-id of its files
/// added the first time (once it has a second file, or after an upgrade, or
/// after the pruning walk took it). True when it was made just now.
pub(crate) fn index_release(conn: &Connection, key: &IdKey, release_id: i64) -> Result<bool> {
    if is_indexed(conn, release_id)? {
        return Ok(false);
    }
    let ids: Vec<i64> = conn
        .prepare_cached("select id from files where release_id = ?")?
        .query_map([release_id], |r| r.get(0))?
        .collect::<Result<_>>()?;
    let mut suffixes = HashMap::new();
    for file_id in ids {
        let mut insert =
            conn.prepare_cached("insert or ignore into release_ids (release_id, id_hash, file_id) values (?, ?, ?)")?;
        file_id_hashes(conn, key, file_id, &mut suffixes, |hash| {
            insert.execute(params![release_id, hash, file_id])?;
            Ok(true)
        })?;
    }
    conn.prepare_cached("insert or ignore into release_ids (release_id, id_hash, file_id) values (?, 0, 0)")?
        .execute([release_id])?;
    conn.prepare_cached("insert or replace into release_indexed (release_id, at) values (?, unixepoch())")?
        .execute([release_id])?;
    Ok(true)
}

/// a release is complete when every file has exactly its parts, and there are
/// as many files as the post said
fn release_complete(conn: &Connection, release_id: i64, file_total: Option<i64>) -> Result<bool> {
    let mut stmt = conn.prepare_cached("select expected, seen, subject from files where release_id = ?")?;
    let mut rows = stmt.query([release_id])?;
    let mut files = 0;
    let mut zero_indexed = false;
    while let Some(row) = rows.next()? {
        files += 1;
        let expected: Option<i64> = row.get(0)?;
        let seen: Vec<u8> = row.get(1)?;
        if !expected.is_some_and(|e| is_exactly(&seen, e)) {
            return Ok(false);
        }
        if !zero_indexed && let Some(subject) = row.get::<_, Option<String>>(2)? {
            zero_indexed = crate::parser::has_zero_file_index(&subject);
        }
    }
    let wanted = file_total.map(|ft| ft + i64::from(zero_indexed));
    if files == 0 {
        return Ok(false);
    }
    if wanted.is_none_or(|w| files == w) {
        return Ok(true);
    }
    // every file it has is whole, but the counter says another number of files
    Ok(own_whole_video(conn, release_id)? || own_whole_rar_set(conn, release_id)? || vouched_by_set(conn, release_id)?)
}

/// The complete PAR2 sets read for release `?1` (`archive::store::par2_inventory`) name only files it has, and name
/// every file of it but the PAR2 volumes and the small side files a set leaves out (`.nfo`, `.sfv`, ...). The sets
/// hash every file of the upload, soo with all of them there and whole the release is too, whatever the subject
/// counter says: an uploader's counter can count a file never posted (134 of "135"), or files another row got. A
/// data file no set names may belong to a set whose volumes never came, so it says nothing.
macro_rules! set_vouches_sql {
    () => {
        "exists (select 1 from par2_files where release_id = ?1 and complete = 1)
         and not exists (select 1 from par2_files p where p.release_id = ?1 and p.complete = 1
             and not exists (select 1 from files f where f.release_id = ?1 and f.filename = p.name))
         and not exists (select 1 from files f where f.release_id = ?1
             and lower(f.filename) not like '%.par2' and lower(f.filename) not like '%.nfo'
             and lower(f.filename) not like '%.sfv' and lower(f.filename) not like '%.srr'
             and lower(f.filename) not like '%.srs' and lower(f.filename) not like '%.txt'
             and lower(f.filename) not like '%.jpg' and lower(f.filename) not like '%.png'
             and lower(f.filename) not like '%.md5'
             and not exists (select 1 from par2_files p where p.release_id = ?1 and p.complete = 1
                 and p.name = f.filename))"
    };
}

/// Sets release `?1` complete when every file it has is whole and its PAR2 set vouches for them (`set_vouches_sql`):
/// the write a PAR2 step ends with, since the set is read after the articles were saved.
pub const SET_VOUCHED_COMPLETE: &str = concat!(
    "update releases set complete = 1 where id = ?1 and complete = 0
     and exists (select 1 from files where release_id = ?1)
     and not exists (select 1 from files where release_id = ?1 and not atlas_file_whole(seen, expected))
     and ",
    set_vouches_sql!()
);

/// Whether a complete PAR2 set of this release names only files it has (`set_vouches_sql`).
fn vouched_by_set(conn: &Connection, release_id: i64) -> Result<bool> {
    // a shard the postprocess tables weren't made in yet has no sets
    let has_sets =
        conn.prepare_cached("select 1 from sqlite_master where type = 'table' and name = 'par2_files'")?.exists([])?;
    if !has_sets {
        return Ok(false);
    }
    conn.prepare_cached(concat!("select ", set_vouches_sql!()))?.query_row([release_id], |r| r.get(0))
}

/// Whether the release's files are one RAR set, named as the release, there from its first volume to its last with
/// no gap, beside side files only (`SIDE_EXTENSIONS`). A set's volumes are all one size but its last, soo a last one
/// shorter than the others, which are alike, is where it ends. An episode of a season posted under one counter is a
/// row of its own: `part01.rar` to `part11.rar` of "74 files". Not a set missing its first volume, or a volume between.
fn own_whole_rar_set(conn: &Connection, release_id: i64) -> Result<bool> {
    static NEW: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        #[allow(clippy::unwrap_used)]
        regex::Regex::new(r"^(.+)\.part0*(\d{1,4})\.rar$").unwrap()
    });
    static OLD: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        #[allow(clippy::unwrap_used)]
        regex::Regex::new(r"^(.+)\.(?:rar|r(\d\d)|s(\d\d))$").unwrap()
    });
    let Some(name) = conn
        .prepare_cached("select name from releases where id = ?")?
        .query_row([release_id], |r| r.get::<_, Option<String>>(0))
        .optional()?
        .flatten()
    else {
        return Ok(false);
    };
    let files: Vec<(String, Option<i64>)> = conn
        .prepare_cached("select filename, expected from files where release_id = ?")?
        .query_map([release_id], |r| Ok((r.get::<_, String>(0)?.trim().to_lowercase(), r.get(1)?)))?
        .collect::<Result<_>>()?;
    // the volumes by their place in the set, first 0, with each one's articles
    let mut stem: Option<String> = None;
    let mut new_style: Option<bool> = None;
    let mut volumes: Vec<(i64, i64)> = Vec::new();
    for (file, expected) in &files {
        if SIDE_EXTENSIONS.iter().any(|ext| file.ends_with(ext)) {
            continue;
        }
        let Some(expected) = *expected else { return Ok(false) };
        let (this_stem, index, new) = if let Some(c) = NEW.captures(file) {
            let Ok(n) = c[2].parse::<i64>() else { return Ok(false) };
            (c[1].to_string(), n - 1, true)
        } else if let Some(c) = OLD.captures(file) {
            let at = |m: Option<regex::Match<'_>>| m.and_then(|m| m.as_str().parse::<i64>().ok());
            let index = match (at(c.get(2)), at(c.get(3))) {
                (Some(r), _) => r + 1,
                (None, Some(s)) => s + 101,
                (None, None) => 0,
            };
            (c[1].to_string(), index, false)
        } else {
            // a file of no set, or of another kind
            return Ok(false);
        };
        if stem.get_or_insert_with(|| this_stem.clone()) != &this_stem || *new_style.get_or_insert(new) != new {
            return Ok(false);
        }
        volumes.push((index, expected));
    }
    volumes.sort_unstable();
    let Some(stem) = stem else { return Ok(false) };
    let Some(((_, last), before)) = volumes.split_last() else { return Ok(false) };
    let in_order = volumes.iter().enumerate().all(|(i, (index, _))| *index == i as i64);
    let size = before.first().map(|(_, e)| *e);
    let even = size.is_some_and(|size| before.iter().all(|(_, e)| *e == size) && *last < size);
    // as the release is named, or with the post's id after it: `name [54205819]`
    let name = name.trim().to_lowercase();
    let named = name == stem
        || name
            .strip_prefix(stem.as_str())
            .and_then(|rest| rest.strip_prefix(" ["))
            .and_then(|rest| rest.strip_suffix(']'))
            .is_some_and(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()));
    Ok(in_order && even && named)
}

/// Whether the release's files are one whole video named as the release, beside side files only (`one_video_named`).
fn own_whole_video(conn: &Connection, release_id: i64) -> Result<bool> {
    let Some(name) = conn
        .prepare_cached("select name from releases where id = ?")?
        .query_row([release_id], |r| r.get::<_, Option<String>>(0))
        .optional()?
        .flatten()
    else {
        return Ok(false);
    };
    let files: Vec<String> = conn
        .prepare_cached("select filename from files where release_id = ?")?
        .query_map([release_id], |r| r.get(0))?
        .collect::<Result<_>>()?;
    Ok(one_video_named([name.as_str()], files.iter().map(String::as_str)))
}

/// The incomplete movie and TV releases that are whole after all (`release_complete`): the rows saved before a
/// whole video named as its release, or a PAR2 set's say, counted over the subject counter. Through the category
/// index: a shard has ~100,000 such rows among millions.
pub fn movie_tv_whole_again(conn: &Connection) -> Result<Vec<i64>> {
    let candidates: Vec<(i64, Option<i64>)> = conn
        .prepare(
            "select r.id, r.file_total from release_category c join releases r on r.id = c.release_id
             where (c.category between 2000 and 2999 or c.category between 5000 and 5999) and r.complete = 0",
        )?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_>>()?;
    let mut whole = Vec::new();
    for (id, file_total) in candidates {
        if release_complete(conn, id, file_total)? {
            whole.push(id);
        }
    }
    Ok(whole)
}

/// Marks the releases `movie_tv_whole_again` finds complete, once (see `postprocess::engine::seed_shard`).
pub fn recheck_movie_tv_complete(conn: &Connection) -> Result<Vec<i64>> {
    let mut update = conn.prepare_cached("update releases set complete = 1 where id = ?1 and complete = 0")?;
    let mut fixed = Vec::new();
    for id in movie_tv_whole_again(conn)? {
        if update.execute([id])? > 0 {
            fixed.push(id);
        }
    }
    Ok(fixed)
}

/// Marks `complete = 1` on existing 0-indexed `[00/N]`..`[N/N]` releases whose `N + 1` files are all complete.
pub fn repair_zero_indexed_complete(conn: &Connection) -> Result<usize> {
    let mut stmt = conn.prepare(
        "select id, file_total from releases where complete = 0 and file_total is not null and file_total > 0",
    )?;
    let candidates: Vec<(i64, i64)> = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_>>()?;
    let mut fixed = 0;
    let mut update = conn.prepare_cached("update releases set complete = 1 where id = ?1")?;
    for (id, file_total) in candidates {
        if release_complete(conn, id, Some(file_total))? {
            fixed += update.execute([id])?;
        }
    }
    Ok(fixed)
}

// ---------------------------------------------------------------- sealing

/// sql: file `f` has more than SEAL_MAX_SEGMENTS rows, counting no further
/// than one past
fn too_many_rows() -> String {
    format!(
        "(select count(*) from (select 1 from segments s where s.file_id = f.id limit {}) ) > {SEAL_MAX_SEGMENTS}",
        SEAL_MAX_SEGMENTS + 1
    )
}

fn blob_error(e: std::io::Error) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(Box::new(e))
}

/// a sealed file's blob, decoded; none when the file isnt sealed. One past
/// `READ_MAX_SEGMENTS` is an error, not something to decode into memory.
fn sealed_segments(conn: &Connection, file_id: i64) -> Result<Option<Vec<crate::blob::Seg>>> {
    conn.prepare_cached("select blob from files where id = ? and blob is not null")?
        .query_row([file_id], |r| r.get::<_, Vec<u8>>(0))
        .optional()?
        .map(|b| crate::blob::decode_capped(&b, READ_MAX_SEGMENTS as usize).map_err(blob_error))
        .transpose()
}

/// What a release on this shard holds: the bytes and the count of every article, its blobs' and rows', as saves count
/// them into `releases.size` and `parts`. None: a file with more rows than one read takes (`READ_MAX_SEGMENTS`); an
/// error for a blob that wont decode or is past that cap.
pub(crate) fn article_totals(conn: &Connection, release_id: i64) -> Result<Option<(i64, i64)>> {
    let files: Vec<i64> = conn
        .prepare_cached("select id from files where release_id = ?")?
        .query_map([release_id], |r| r.get(0))?
        .collect::<Result<_>>()?;
    let (mut bytes, mut count) = (0i64, 0i64);
    for file in files {
        for seg in sealed_segments(conn, file)?.unwrap_or_default() {
            bytes += seg.bytes;
            count += 1;
        }
        let (rows, row_bytes): (i64, i64) = conn
            .prepare_cached("select count(*), coalesce(sum(bytes), 0) from segments where file_id = ?")?
            .query_row([file], |r| Ok((r.get(0)?, r.get(1)?)))?;
        if rows > READ_MAX_SEGMENTS {
            return Ok(None);
        }
        bytes += row_bytes;
        count += rows;
    }
    Ok(Some((bytes, count)))
}

/// One stored article: its file's name, part, total (the file's `expected`), bytes and message-id (no angle brackets).
pub(crate) type StoredArticle = (String, Option<i64>, Option<i64>, i64, String);

/// Every article of release `release_id` on this shard, from its files' blobs and rows.
pub(crate) fn release_articles(conn: &Connection, release_id: i64) -> Result<Vec<StoredArticle>> {
    let files: Vec<(i64, String, Option<i64>)> = conn
        .prepare_cached("select id, filename, expected from files where release_id = ?")?
        .query_map([release_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<Result<_>>()?;
    let mut suffixes: HashMap<i64, Option<String>> = HashMap::new();
    let mut out = Vec::new();
    for (file, name, expected) in files {
        let mut segs = sealed_segments(conn, file)?.unwrap_or_default();
        let rows: Vec<crate::blob::Seg> = conn
            .prepare_cached("select part, bytes, domain, local from segments where file_id = ?")?
            .query_map([file], |r| {
                Ok(crate::blob::Seg {
                    part: r.get(0)?,
                    bytes: r.get::<_, Option<i64>>(1)?.unwrap_or(0),
                    domain: r.get(2)?,
                    local: r.get(3)?,
                })
            })?
            .collect::<Result<_>>()?;
        segs.extend(rows);
        for seg in segs {
            if seg.domain != 0 && !suffixes.contains_key(&seg.domain) {
                let suffix = conn
                    .prepare_cached("select suffix from domains where id = ?")?
                    .query_row([seg.domain], |r| r.get(0))
                    .optional()?;
                suffixes.insert(seg.domain, suffix);
            }
            let id = decode(&seg.local, suffixes.get(&seg.domain).and_then(|s| s.as_deref()));
            let id = id.trim_start_matches('<').trim_end_matches('>').to_string();
            out.push((name.clone(), seg.part, expected, seg.bytes, id));
        }
    }
    Ok(out)
}

/// Release `release_id` split into one row per upload, in the caller's transaction on its shard: `upload_of` gives
/// the upload (from 0, in time order) of each message-id (no angle brackets), as `repost_split::decide` found it.
/// Upload 0 keeps the row, its id, name, collections and everything found for it; each later upload gets a new row
/// named as a collision is (`name [NNNNNNNN]`), its own files (the same names, with that upload's articles sealed in
/// a blob), size, parts and completeness, queued for post-processing. Returns the new rows' ids. None, and nothing
/// changed: an article with no upload, a file whose articles a save held back (sealing settles them first), or an
/// upload of a file past what one blob holds. `dates`: each message-id's Date by the poster's clock (as
/// `file_timing` keeps them), for each upload's files' timing rows.
pub(crate) fn split_uploads(
    conn: &Connection,
    ids: &Ids,
    release_id: i64,
    upload_of: &HashMap<String, usize>,
    dates: &HashMap<String, i64>,
    uploads: usize,
) -> Result<Option<Vec<i64>>> {
    if uploads < 2 {
        return Ok(None);
    }
    let held: bool = conn
        .prepare_cached(
            "select exists (select 1 from held_back h join files f on f.id = h.file_id where f.release_id = ?)",
        )?
        .query_row([release_id], |r| r.get(0))?;
    if held {
        return Ok(None);
    }
    type FileRow = (i64, String, Option<String>, Option<i64>, Option<String>, Option<i64>, Option<i64>);
    let files: Vec<FileRow> = conn
        .prepare_cached(
            "select id, filename, subject, subject_part, subject_mid, expected, file_total from files where release_id = ?",
        )?
        .query_map([release_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)))?
        .collect::<Result<_>>()?;
    let mut suffixes: HashMap<i64, Option<String>> = HashMap::new();
    // each file's articles, by upload, and per upload its NZB entry's article (first by part, then message-id) and
    // its first and last Date
    let mut parts: Vec<Vec<Vec<crate::blob::Seg>>> = Vec::new();
    type Facts = (Option<(Option<i64>, String)>, Option<(i64, i64)>);
    let mut facts: Vec<Vec<Facts>> = Vec::new();
    for (file, ..) in &files {
        let mut segs = sealed_segments(conn, *file)?.unwrap_or_default();
        let rows: Vec<crate::blob::Seg> = conn
            .prepare_cached("select part, bytes, domain, local from segments where file_id = ?")?
            .query_map([file], |r| {
                Ok(crate::blob::Seg {
                    part: r.get(0)?,
                    bytes: r.get::<_, Option<i64>>(1)?.unwrap_or(0),
                    domain: r.get(2)?,
                    local: r.get(3)?,
                })
            })?
            .collect::<Result<_>>()?;
        segs.extend(rows);
        let mut by: Vec<Vec<crate::blob::Seg>> = vec![Vec::new(); uploads];
        let mut seen_facts: Vec<Facts> = vec![(None, None); uploads];
        for seg in segs {
            if seg.domain != 0 && !suffixes.contains_key(&seg.domain) {
                let suffix = conn
                    .prepare_cached("select suffix from domains where id = ?")?
                    .query_row([seg.domain], |r| r.get(0))
                    .optional()?;
                suffixes.insert(seg.domain, suffix);
            }
            let id = decode(&seg.local, suffixes.get(&seg.domain).and_then(|s| s.as_deref()));
            let id = id.trim_start_matches('<').trim_end_matches('>');
            let Some(u) = upload_of.get(id).copied() else { return Ok(None) };
            let Some(slot) = by.get_mut(u) else { return Ok(None) };
            if let Some((rep, span)) = seen_facts.get_mut(u) {
                let mid = format!("<{id}>");
                if rep.as_ref().is_none_or(|(p, m)| subject_key(seg.part, &mid) < subject_key(*p, m)) {
                    *rep = Some((seg.part, mid));
                }
                if let Some(t) = dates.get(id).copied() {
                    *span = Some(span.map_or((t, t), |(a, b)| (a.min(t), b.max(t))));
                }
            }
            slot.push(seg);
        }
        if by.iter().any(|b| b.len() as i64 > SEAL_MAX_SEGMENTS) {
            return Ok(None);
        }
        parts.push(by);
        facts.push(seen_facts);
    }
    type ReleaseRow = (String, String, Option<String>, Option<String>, Option<String>, i64, Option<i64>);
    let (name, group, poster, posted, display, obfuscated, file_total): ReleaseRow = conn
        .prepare_cached(
            "select name, group_name, poster, posted_date, display_name, is_obfuscated, file_total from releases where id = ?",
        )?
        .query_row([release_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)))?;
    let seen_of = |segs: &[crate::blob::Seg]| {
        let mut seen = Vec::new();
        for p in segs.iter().filter_map(|s| s.part) {
            insert_part(&mut seen, p);
        }
        seen
    };
    let mut rows = vec![release_id];
    let mut names = vec![name.clone()];
    let mut taken = conn.prepare_cached("select 1 from releases where name = ? and group_name = ?")?;
    for u in 1..uploads {
        let mut digits = (release_id.wrapping_mul(31).wrapping_add(u as i64)).rem_euclid(100_000_000);
        let mut twin = format!("{name} [{digits:08}]");
        while taken.exists(params![twin, group])? {
            digits = (digits + 1) % 100_000_000;
            twin = format!("{name} [{digits:08}]");
        }
        let id = global_id(ids.next()?, shard_of_id(release_id));
        conn.prepare_cached(
            "insert into releases (id, name, size, complete, group_name, poster, posted_date, display_name, is_obfuscated,
             parts, file_total) values (?, ?, 0, 0, ?, ?, ?, ?, ?, 0, ?)",
        )?
        .execute(params![id, twin, group, poster, posted, display, obfuscated, file_total])?;
        rows.push(id);
        names.push(twin);
    }
    for (((file, filename, subject, _, _, expected, ftotal), by), file_facts) in files.iter().zip(&parts).zip(&facts) {
        for (u, segs) in by.iter().enumerate() {
            let blob = (!segs.is_empty()).then(|| crate::blob::encode(segs));
            let (rep, span) = file_facts.get(u).cloned().unwrap_or((None, None));
            let (subject_part, subject_mid) = rep.map_or((None, None), |(p, m)| (p, Some(m)));
            let target = if u == 0 { Some(*file) } else { None };
            if let Some(file) = target {
                conn.prepare_cached(
                    "update files set seen = ?, blob = ?, blob_bytes = ?, subject_part = ?, subject_mid = ? where id = ?",
                )?
                .execute(params![
                    seen_of(segs),
                    blob,
                    blob.as_ref().map(|_| crate::blob_bytes::of_segs(segs)),
                    subject_part,
                    subject_mid,
                    file
                ])?;
                conn.prepare_cached("delete from segments where file_id = ?")?.execute([file])?;
                // its timing is its own upload's now: the repair rereads that window
                conn.prepare_cached(
                    "update file_timing set first_date = ?, last_date = ?, articles = ? where file_id = ?",
                )?
                .execute(params![span.map(|s| s.0), span.map(|s| s.1), segs.len() as i64, file])?;
            } else if let Some(blob) = blob {
                conn.prepare_cached(
                    "insert into files (release_id, filename, subject, subject_part, subject_mid, expected, file_total,
                     seen, touched_at, blob, blob_bytes) values (?, ?, ?, ?, ?, ?, ?, ?, unixepoch(), ?, ?)",
                )?
                .execute(params![
                    rows.get(u).copied().unwrap_or(release_id),
                    filename,
                    subject,
                    subject_part,
                    subject_mid,
                    expected,
                    ftotal,
                    seen_of(segs),
                    blob,
                    crate::blob_bytes::of_segs(segs)
                ])?;
                let new_file = conn.last_insert_rowid();
                conn.prepare_cached(
                    "insert into file_timing (file_id, release_id, first_date, last_date, articles) values (?, ?, ?, ?, ?)",
                )?
                .execute(params![
                    new_file,
                    rows.get(u).copied().unwrap_or(release_id),
                    span.map(|s| s.0),
                    span.map(|s| s.1),
                    segs.len() as i64
                ])?;
            }
        }
    }
    // a file left with nothing of upload 0's has no articles: it goes, with its timing row
    conn.prepare_cached(
        "delete from file_timing where file_id in (select id from files where release_id = ? and blob is null
         and not exists (select 1 from segments where file_id = files.id))",
    )?
    .execute([release_id])?;
    conn.prepare_cached(
        "delete from files where release_id = ? and blob is null and not exists (select 1 from segments where file_id = files.id)",
    )?
    .execute([release_id])?;
    // the index of message-ids is made again by the next save that needs it
    unindex_release(conn, release_id)?;
    for (u, id) in rows.iter().enumerate() {
        let (bytes, count) =
            parts.iter().filter_map(|by| by.get(u)).flatten().fold((0i64, 0i64), |(b, c), s| (b + s.bytes, c + 1));
        conn.prepare_cached("update releases set size = ?, parts = ? where id = ?")?
            .execute(params![bytes, count, id])?;
        let complete = release_complete(conn, *id, file_total)?;
        conn.prepare_cached("update releases set complete = ? where id = ?")?.execute(params![complete as i64, id])?;
        conn.prepare_cached(
            "insert into pp_queue (release_id, due_at) values (?1, 0)
             on conflict(release_id) do update set due_at = min(pp_queue.due_at, excluded.due_at)",
        )?
        .execute([id])?;
    }
    // each upload's posting window, soo later saves of its articles (a reread of its window) find its row
    let mut spans: Vec<Option<(i64, i64)>> = vec![None; uploads];
    for (mid, u) in upload_of {
        if let (Some(t), Some(span)) = (dates.get(mid).copied(), spans.get_mut(*u)) {
            *span = Some(span.map_or((t, t), |(a, b)| (a.min(t), b.max(t))));
        }
    }
    for ((id, row_name), span) in rows.iter().zip(&names).zip(&spans) {
        conn.prepare_cached(
            "insert or replace into upload_routes (origin_id, release_id, name, first_date, last_date)
             values (?, ?, ?, ?, ?)",
        )?
        .execute(params![release_id, id, row_name, span.map(|s| s.0), span.map(|s| s.1)])?;
    }
    conn.prepare_cached("update meta set value = value + ? where key = 'releases'")?.execute([(uploads - 1) as i64])?;
    Ok(Some(rows.split_off(1)))
}
/// Every article of a release on this shard, by the key sealing tells one message-id with (`seg_key`): the blobs'
/// and the rows'. For telling whether two rows of a shard hold the same articles.
pub(crate) fn release_article_keys(conn: &Connection, release_id: i64) -> Result<HashSet<String>> {
    let files: Vec<i64> = conn
        .prepare_cached("select id from files where release_id = ?")?
        .query_map([release_id], |r| r.get(0))?
        .collect::<Result<_>>()?;
    let mut suffixes: HashMap<i64, Option<String>> = HashMap::new();
    let mut keys = HashSet::new();
    for file in files {
        for seg in sealed_segments(conn, file)?.unwrap_or_default() {
            keys.insert(seg_key(conn, &mut suffixes, &seg)?);
        }
        let rows: Vec<crate::blob::Seg> = conn
            .prepare_cached("select part, bytes, domain, local from segments where file_id = ?")?
            .query_map([file], |r| {
                Ok(crate::blob::Seg {
                    part: r.get(0)?,
                    bytes: r.get::<_, Option<i64>>(1)?.unwrap_or(0),
                    domain: r.get(2)?,
                    local: r.get(3)?,
                })
            })?
            .collect::<Result<_>>()?;
        for seg in rows {
            keys.insert(seg_key(conn, &mut suffixes, &seg)?);
        }
    }
    Ok(keys)
}

/// Seal a file: its rows (and any blob it already has) become one blob, the
/// rows go, all or nothing. Returns the segments in the blob, 0 when there
/// is none (no rows, or rows that cant be sealed).
pub(crate) fn seal_file(conn: &Connection, file_id: i64) -> Result<usize> {
    conn.execute_batch("savepoint seal")?;
    let sealed = seal_rows(conn, file_id);
    match sealed {
        Ok(_) => conn.execute_batch("release seal")?,
        Err(_) => conn.execute_batch("rollback to seal; release seal")?,
    }
    sealed
}

fn seal_rows(conn: &Connection, file_id: i64) -> Result<usize> {
    let mut segs = sealed_segments(conn, file_id)?.unwrap_or_default();
    // more rows than one blob takes: they stay rows, and a blob stays as it
    // is (counted before any row is read)
    let counted: i64 = conn
        .prepare_cached("select count(*) from (select 1 from segments where file_id = ? limit ?)")?
        .query_row(params![file_id, SEAL_MAX_SEGMENTS + 1], |r| r.get(0))?;
    if counted > SEAL_MAX_SEGMENTS {
        settle_held_back(conn, file_id, &segs)?;
        return Ok(0);
    }
    let rows: Vec<crate::blob::Seg> = conn
        .prepare_cached("select part, bytes, domain, local from segments where file_id = ?")?
        .query_map([file_id], |r| {
            Ok(crate::blob::Seg {
                part: r.get(0)?,
                bytes: r.get::<_, Option<i64>>(1)?.unwrap_or(0),
                domain: r.get(2)?,
                local: r.get(3)?,
            })
        })?
        .collect::<Result<_>>()?;
    if rows.is_empty() {
        return Ok(segs.len());
    }
    // the blob has no room for a negative part number: such a file stays rows
    // nor one whose message-id is longer than a blob decodes
    if rows.iter().any(|r| r.part.is_some_and(|p| p < 0) || r.local.len() > crate::blob::MAX_LOCAL) {
        settle_held_back(conn, file_id, &segs)?;
        return Ok(0);
    }
    // a row the blob already has (saved past a save's decode budget, see
    // `SealedCache`) isnt added twice: it goes, before the blob is counted,
    // soo a full blob doesnt keep it a row
    // (the same message-id, even stored whole in the blob and packed in the
    // row, or the other way round, when a domain got shared in between)
    let mut suffixes: HashMap<i64, Option<String>> = HashMap::new();
    let mut key = |s: &crate::blob::Seg| seg_key(conn, &mut suffixes, s);
    let held: HashSet<String> = segs.iter().map(&mut key).collect::<Result<_>>()?;
    let (mut copies, mut fresh) = (Vec::new(), Vec::new());
    for r in rows {
        if held.contains(&key(&r)?) { copies.push(r) } else { fresh.push(r) }
    }
    let rows = fresh;
    // what the rows a save couldnt tell from copies said: the copies' goes,
    // the rest's is folded into the file now it's known to be new
    let mut held: HashMap<String, Article> =
        held_back(conn, file_id, -1)?.into_iter().map(|a| (a.message_id.clone(), a)).collect();
    let mut news = Vec::new();
    if !held.is_empty() {
        conn.prepare_cached("delete from held_back where file_id = ?")?.execute([file_id])?;
        for r in &rows {
            if let Some(a) = held.remove(&key(r)?) {
                news.push(a);
            }
        }
    }
    for copy in &copies {
        conn.prepare_cached("delete from segments where file_id = ? and local = ? and domain = ?")?
            .execute(params![file_id, copy.local, copy.domain])?;
    }
    if !copies.is_empty() {
        let release_id: i64 =
            conn.prepare_cached("select release_id from files where id = ?")?.query_row([file_id], |r| r.get(0))?;
        uncount_copies(conn, release_id, copies.iter().map(|c| c.bytes).sum(), copies.len() as i64)?;
        // a copy's part was counted in the file's parts when it was saved: they
        // are what the blob and the rows left have, and so is the release's
        // completeness
        let mut seen = Vec::new();
        for p in segs.iter().chain(&rows).filter_map(|s| s.part) {
            insert_part(&mut seen, p);
        }
        conn.prepare_cached("update files set seen = ? where id = ?")?.execute(params![seen, file_id])?;
    }
    fold_in(conn, file_id, &news)?;
    if !copies.is_empty() || !news.is_empty() {
        update_complete(conn, file_id)?;
    }
    // too much for one blob in memory: the rest stays rows, the blob as it is
    if segs.len() + rows.len() > SEAL_MAX_SEGMENTS as usize {
        return Ok(0);
    }
    segs.extend(rows);
    conn.prepare_cached("update files set blob = ?, blob_bytes = ? where id = ?")?.execute(params![
        crate::blob::encode(&segs),
        crate::blob_bytes::of_segs(&segs),
        file_id
    ])?;
    conn.prepare_cached("delete from segments where file_id = ?")?.execute([file_id])?;
    Ok(segs.len())
}

/// the message-id a stored segment stands for (`dedup_key`), its domain's
/// suffix looked up once per domain
fn seg_key(conn: &Connection, suffixes: &mut HashMap<i64, Option<String>>, s: &crate::blob::Seg) -> Result<String> {
    if s.domain != 0 && !suffixes.contains_key(&s.domain) {
        let suffix = conn
            .prepare_cached("select suffix from domains where id = ?")?
            .query_row([s.domain], |r| r.get(0))
            .optional()?;
        suffixes.insert(s.domain, suffix);
    }
    Ok(dedup_key(&s.local, s.domain, suffixes.get(&s.domain).and_then(|x| x.as_deref())))
}

/// what file `file_id`'s rows held back, `limit` at most (-1: all)
fn held_back(conn: &Connection, file_id: i64, limit: i64) -> Result<Vec<Article>> {
    conn.prepare_cached(
        "select message_id, subject, part, total_parts, file_total from held_back where file_id = ? limit ?",
    )?
    .query_map(params![file_id, limit], |r| {
        Ok(Article {
            message_id: r.get(0)?,
            subject: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
            part: r.get(2)?,
            total_parts: r.get(3)?,
            file_total: r.get(4)?,
            ..Default::default()
        })
    })?
    .collect()
}

/// What new articles held back said about file `file_id`, folded in
fn fold_in(conn: &Connection, file_id: i64, news: &[Article]) -> Result<()> {
    if news.is_empty() {
        return Ok(());
    }
    let (release_id, filename): (i64, String) = conn
        .prepare_cached("select release_id, filename from files where id = ?")?
        .query_row([file_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
    let mut f = file(conn, release_id, &filename)?;
    for a in news {
        f.add(a);
    }
    conn.prepare_cached(
        "update files set subject = ?, subject_part = ?, subject_mid = ?, expected = ?, file_total = ? where id = ?",
    )?
    .execute(params![f.subject, f.subject_part, f.subject_mid, f.expected, f.file_total, file_id])?;
    if let Some(ft) = news.iter().filter_map(|a| a.file_total).max() {
        conn.prepare_cached("update releases set file_total = max(coalesce(file_total, ?), ?) where id = ?")?
            .execute(params![ft, ft, release_id])?;
    }
    Ok(())
}

/// file `file_id`'s release's completeness worked out again
fn update_complete(conn: &Connection, file_id: i64) -> Result<()> {
    let (release_id, file_total): (i64, Option<i64>) = conn
        .prepare_cached("select r.id, r.file_total from files f join releases r on r.id = f.release_id where f.id = ?")?
        .query_row([file_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
    conn.prepare_cached("update releases set complete = ? where id = ?")?
        .execute(params![release_complete(conn, release_id, file_total)? as i64, release_id])?;
    Ok(())
}

/// The ways message-id `id` can be stored as a row: whole, packed under its
/// domain (if it has a row), and packed as text the way before locals were
/// packed by alphabet.
fn stored_forms(conn: &Connection, id: &str) -> Result<Vec<(Vec<u8>, i64)>> {
    let mut forms = vec![(whole(id), 0)];
    if let Some((local, suffix)) = split_message_id(id)
        && let Some(d) = conn
            .prepare_cached("select id from domains where suffix = ? order by id limit 1")?
            .query_row([suffix], |r| r.get::<_, i64>(0))
            .optional()?
    {
        forms.push((pack_local(local), d));
        let mut text = vec![TEXT];
        text.extend_from_slice(local.as_bytes());
        forms.push((text, d));
    }
    Ok(forms)
}

/// What a file that wont seal (too many rows, or rows a blob cant hold) held
/// back, settled without sealing it: the rows that are copies of an article
/// in its blob `segs` go, out of the totals with what they said, and the new
/// ones' totals and subject are folded in. `SEAL_MAX_SEGMENTS` entries a
/// time at most, the rest on the next walk.
fn settle_held_back(conn: &Connection, file_id: i64, segs: &[crate::blob::Seg]) -> Result<()> {
    let entries = held_back(conn, file_id, SEAL_MAX_SEGMENTS)?;
    if entries.is_empty() {
        return Ok(());
    }
    let mut suffixes = HashMap::new();
    let in_blob: HashSet<String> = segs.iter().map(|s| seg_key(conn, &mut suffixes, s)).collect::<Result<_>>()?;
    let (mut bytes, mut copies, mut news) = (0i64, 0i64, Vec::new());
    for a in entries {
        conn.prepare_cached("delete from held_back where file_id = ? and message_id = ?")?
            .execute(params![file_id, a.message_id])?;
        if !in_blob.contains(&a.message_id) {
            news.push(a);
            continue;
        }
        for (local, domain) in stored_forms(conn, &a.message_id)? {
            let gone: Option<Option<i64>> = conn
                .prepare_cached("delete from segments where file_id = ? and local = ? and domain = ? returning bytes")?
                .query_row(params![file_id, local, domain], |r| r.get(0))
                .optional()?;
            if let Some(b) = gone {
                (bytes, copies) = (bytes + b.unwrap_or(0), copies + 1);
            }
        }
    }
    if copies > 0 {
        let release_id: i64 =
            conn.prepare_cached("select release_id from files where id = ?")?.query_row([file_id], |r| r.get(0))?;
        uncount_copies(conn, release_id, bytes, copies)?;
    }
    fold_in(conn, file_id, &news)?;
    if copies > 0 {
        // the parts are what the blob and the rows left have
        recount_parts(conn, file_id)
    } else if !news.is_empty() {
        update_complete(conn, file_id)
    } else {
        Ok(())
    }
}

/// File `file_id`'s parts worked out again from its blob and rows, and its
/// release's completeness with them: after dropping copies whose parts were
/// counted (see `SealedCache`).
pub(crate) fn recount_parts(conn: &Connection, file_id: i64) -> Result<()> {
    let mut seen = Vec::new();
    for p in sealed_segments(conn, file_id)?.unwrap_or_default().iter().filter_map(|s| s.part) {
        insert_part(&mut seen, p);
    }
    {
        let mut stmt = conn.prepare_cached("select part from segments where file_id = ? and part is not null")?;
        let mut parts = stmt.query([file_id])?;
        while let Some(r) = parts.next()? {
            insert_part(&mut seen, r.get(0)?);
        }
    }
    conn.prepare_cached("update files set seen = ? where id = ?")?.execute(params![seen, file_id])?;
    update_complete(conn, file_id)
}

/// The next `limit` files after `after_id`, and which of them a writer
/// seals: ones with rows that are complete and not sealed yet, or untouched
/// for `SEAL_AGE` (a sealed file with late rows waits for this). Files never
/// touched since the upgrade are left to compaction, and files with a
/// negative part number, or more than `SEAL_MAX_SEGMENTS` rows, never seal
/// (they are taken only to settle what they held back, `settle_held_back`).
/// Returns the ids and the last id looked
/// at, for walking the table `limit` files at a time: `last == after_id`
/// means the end.
pub(crate) fn sealable(conn: &Connection, after_id: i64, limit: usize, now: i64) -> Result<(Vec<i64>, i64)> {
    let mut stmt = conn.prepare_cached(&format!(
        "select f.id, f.expected, f.seen, f.touched_at, f.blob is not null,
                exists (select 1 from segments s where s.file_id = f.id),
                exists (select 1 from segments s where s.file_id = f.id and (s.part < 0 or length(s.local) > {})) or {},
                exists (select 1 from held_back h where h.file_id = f.id)
         from files f where f.id > ? order by f.id limit ?",
        crate::blob::MAX_LOCAL,
        too_many_rows()
    ))?;
    let mut ids = Vec::new();
    let mut last = after_id;
    let mut rows = stmt.query(params![after_id, limit as i64])?;
    while let Some(r) = rows.next()? {
        let id: i64 = r.get(0)?;
        last = id;
        let (has_rows, never, held): (bool, bool, bool) = (r.get(5)?, r.get(6)?, r.get(7)?);
        // one that never seals still gets what it held back settled
        if has_rows && never && held {
            ids.push(id);
        }
        if !has_rows || never {
            continue;
        }
        let seen: Vec<u8> = r.get(2)?;
        let touched: Option<i64> = r.get(3)?;
        if touched.is_some() && due(r.get(1)?, &seen, touched, r.get(4)?, now) {
            ids.push(id);
        }
    }
    Ok((ids, last))
}

/// A file a save just completed is still complete, not sealed and has rows
/// that can go in a blob.
fn still_due(conn: &Connection, file_id: i64, now: i64) -> Result<bool> {
    conn.prepare_cached(&format!(
        "select f.expected, f.seen, f.touched_at, f.blob is not null,
                exists (select 1 from segments s where s.file_id = f.id),
                exists (select 1 from segments s where s.file_id = f.id and (s.part < 0 or length(s.local) > {})) or {}
         from files f where f.id = ?",
        crate::blob::MAX_LOCAL,
        too_many_rows()
    ))?
    .query_row([file_id], |r| {
        let (has_rows, never): (bool, bool) = (r.get(4)?, r.get(5)?);
        let (seen, touched): (Vec<u8>, Option<i64>) = (r.get(1)?, r.get(2)?);
        Ok(has_rows && !never && touched.is_some() && due(r.get(0)?, &seen, touched, r.get(3)?, now))
    })
    .optional()
    .map(|due| due.unwrap_or(false))
}

/// the segments a `SealedCache` keeps decoded at most: a save can touch a
/// sealed file per article, and each is up to SEAL_MAX_SEGMENTS
const SEALED_CACHE_MAX_SEGMENTS: usize = 200_000;

/// the segments one `SealedCache` (one save) decodes at most, over all its
/// blobs: 20 of the biggest. Late articles cycling over more sealed files
/// than the cache holds would decode a blob each otherwise
const SEALED_DECODE_BUDGET: usize = 20 * SEAL_MAX_SEGMENTS as usize;

/// a sealed file's articles as (local, domain)
/// the message-ids of a sealed file's blob, as `dedup_key` writes them: by text, soo an article stored under another
/// id of its domain (`domains_not_unique`) is the same article
type SegmentSet = HashSet<String>;

/// Blobs of sealed files, decoded once each, for checking late articles.
/// Holds at most `SEALED_CACHE_MAX_SEGMENTS` decoded segments, letting the
/// least recently used files go past that, and decodes at most
/// `SEALED_DECODE_BUDGET` segments in all. Past the budget a sealed file not
/// held is taken not to have the article: it's saved as a row, and sealing
/// the file again drops a row its blob already has (till then a repost of
/// an article could show twice in the NZB).
pub(crate) struct SealedCache {
    /// each file's segments, and when it was last used
    files: HashMap<i64, (u64, SegmentSet)>,
    /// the files by when they were last used
    by_use: std::collections::BTreeMap<u64, i64>,
    uses: u64,
    /// segments in `files`
    segments: usize,
    max_segments: usize,
    /// segments decoded so far
    decoded: usize,
    max_decoded: usize,
    /// the last `contains` was past the budget: its false only means not known
    pub(crate) guessed: bool,
    /// each domain's suffix, looked up once (a domain's suffix never changes)
    suffixes: HashMap<i64, Option<String>>,
}

impl SealedCache {
    #[cfg(test)]
    pub(crate) fn new() -> Self {
        Self::with_limits(SEALED_CACHE_MAX_SEGMENTS, SEALED_DECODE_BUDGET)
    }

    fn with_limits(max_segments: usize, max_decoded: usize) -> Self {
        SealedCache {
            files: HashMap::new(),
            by_use: Default::default(),
            uses: 0,
            segments: 0,
            max_segments,
            decoded: 0,
            max_decoded,
            guessed: false,
            suffixes: HashMap::new(),
        }
    }

    /// the article (local, domain) is already in file `file_id`'s blob
    pub(crate) fn contains(&mut self, conn: &Connection, file_id: i64, local: &[u8], domain: i64) -> Result<bool> {
        self.uses += 1;
        self.guessed = false;
        if self.suffixes.len() > 100_000 {
            self.suffixes.clear();
        }
        let key = seg_key(
            conn,
            &mut self.suffixes,
            &crate::blob::Seg { part: None, bytes: 0, domain, local: local.to_vec() },
        )?;
        if let Some((used, set)) = self.files.get_mut(&file_id) {
            self.by_use.remove(used);
            *used = self.uses;
            self.by_use.insert(self.uses, file_id);
            return Ok(set.contains(&key));
        }
        if self.decoded >= self.max_decoded {
            self.guessed = true;
            return Ok(false);
        }
        let set: SegmentSet = sealed_segments(conn, file_id)?
            .unwrap_or_default()
            .iter()
            .map(|s| seg_key(conn, &mut self.suffixes, s))
            .collect::<Result<_>>()?;
        self.decoded += set.len();
        let found = set.contains(&key);
        if set.len() > self.max_segments {
            return Ok(found);
        }
        while self.segments + set.len() > self.max_segments
            && let Some((_, oldest)) = self.by_use.pop_first()
            && let Some((_, gone)) = self.files.remove(&oldest)
        {
            self.segments -= gone.len();
        }
        self.segments += set.len();
        self.by_use.insert(self.uses, file_id);
        self.files.insert(file_id, (self.uses, set));
        Ok(found)
    }
}

// ---------------------------------------------------------------- saving

/// Saves releases into one shard: upsert the release, add its new articles,
/// update its size / parts / completeness. One per shard connection.
pub struct ShardWriter {
    pub shard: usize,
    domains: Domains,
    /// files this writer's saves completed, oldest first, waiting to be sealed
    completed: VecDeque<i64>,
    completed_ids: HashSet<i64>,
    /// where the last walk stopped in the files table
    seal_after: i64,
    /// when the last walk ran (unix seconds)
    walked_at: Option<i64>,
    /// segments of sealed files a save decodes at most (see `SealedCache`)
    decode_budget: usize,
    /// moving what the freezing rules queued into the frozen file, between saves
    pub freezer: crate::frozen::Ticker,
    /// the walk taking quiet releases' `release_ids` (`prune_ids_some`)
    ids: IdsPrune,
    /// when the walk last took a step
    ids_step: Option<Instant>,
    /// the release the step in hand is at, for going on past it if the step fails
    ids_failing: Option<i64>,
}

/// Where the walk taking quiet releases' `release_ids` is (`ShardWriter::prune_ids_some`).
#[derive(Clone, Copy, Default, Debug, PartialEq)]
struct IdsPrune {
    /// where the last step stopped
    after: i64,
    /// a quiet release whose rows are still being taken, a chunk a step
    pruning: Option<i64>,
    /// no step before this (unix seconds): the last one reached the end
    rest_until: i64,
    /// the least recently touched or indexed release this walk kept, for when the next may start
    oldest_kept: Option<i64>,
    /// a release a step failed at, passed over once
    skip: Option<i64>,
}

/// files sealed per writer tick at most
pub const SEAL_PER_TICK: usize = 2_000;

/// a pruning step of `release_ids` stops after this long while saves or stage jobs wait, so the writer gets back to them
pub const PRUNE_IDS_BUDGET: std::time::Duration = std::time::Duration::from_millis(10);

/// the same on a writer with nothing to save (a save that comes in meanwhile waits up to this, after an idle freeze tick)
pub const PRUNE_IDS_BUDGET_IDLE: std::time::Duration = std::time::Duration::from_millis(50);

/// after a step reaches the end of `release_ids`, the next starts over this much later at least (seconds)
const PRUNE_IDS_REST: i64 = 3_600;

/// between saves, a pruning step at most this often
const PRUNE_IDS_EVERY: std::time::Duration = std::time::Duration::from_secs(1);

/// the same when nothing came in meanwhile: at most a fifth of the writer's time with PRUNE_IDS_BUDGET_IDLE
const PRUNE_IDS_IDLE_EVERY: std::time::Duration = std::time::Duration::from_millis(250);

/// after a pruning step found the lock held elsewhere, the next waits this long (seconds)
const PRUNE_IDS_BUSY_REST: i64 = 30;

/// after a pruning step fails, the next waits this long (seconds)
const PRUNE_IDS_FAILED_REST: i64 = 600;

/// how long a pruning step on an idle writer waits for a write lock another connection holds (a save that comes
/// meanwhile waits as long)
const PRUNE_IDS_BUSY: std::time::Duration = std::time::Duration::from_millis(200);

/// the lock wait right after a save, or with one queued
const PRUNE_IDS_BUSY_SAVING: std::time::Duration = std::time::Duration::from_millis(20);

/// while more saves are queued, a pruning step at most this often
const PRUNE_IDS_QUEUED_EVERY: std::time::Duration = std::time::Duration::from_secs(5);

/// releases a pruning step of `release_ids` looks at most
const PRUNE_IDS_LOOKS: usize = 5_000;

/// rows of one quiet release's `release_ids` taken at a time
const PRUNE_IDS_CHUNK: i64 = 1_000;

/// the same when nothing waits for the writer
const PRUNE_IDS_CHUNK_IDLE: i64 = 5_000;

/// a seal tick stops after this long, so the writer gets back to saving
pub const SEAL_BUDGET: std::time::Duration = std::time::Duration::from_millis(100);

/// a writer walks the files table for stale files at most this often (seconds)
pub const SEAL_WALK_EVERY: i64 = 60;

/// completed files a writer keeps for sealing at most, the oldest go first
const COMPLETED_CAP: usize = 50_000;

impl ShardWriter {
    pub fn new(shard: usize) -> ShardWriter {
        ShardWriter {
            shard,
            domains: Domains::default(),
            completed: VecDeque::new(),
            completed_ids: HashSet::new(),
            seal_after: 0,
            walked_at: None,
            decode_budget: SEALED_DECODE_BUDGET,
            freezer: crate::frozen::Ticker::default(),
            ids: IdsPrune::default(),
            ids_step: None,
            ids_failing: None,
        }
    }

    fn remember_completed(&mut self, file_id: i64) {
        if !self.completed_ids.insert(file_id) {
            return;
        }
        if self.completed.len() >= COMPLETED_CAP
            && let Some(oldest) = self.completed.pop_front()
        {
            self.completed_ids.remove(&oldest);
        }
        self.completed.push_back(file_id);
    }

    /// Seal up to SEAL_PER_TICK files, for at most SEAL_BUDGET: first the
    /// files this writer's saves completed, then, at most every
    /// SEAL_WALK_EVERY seconds, the due files of a step through the files
    /// table by id from where the last walk stopped (back to the start at the
    /// end of the table). A file that fails to seal is counted and skipped.
    /// Not for use inside a save transaction. Returns the files sealed.
    pub fn seal_some(&mut self, conn: &mut Connection, now: i64) -> Result<usize> {
        let started = Instant::now();
        let out_of_time = |done: usize| done > 0 && started.elapsed() >= SEAL_BUDGET;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let seal = |id: i64| match seal_file(&tx, id) {
            Ok(segs) => Ok(usize::from(segs > 0)),
            Err(e) => {
                profile::LOAD.writer_seal_errors.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Err(e)
            }
        };
        let (mut sealed, mut done) = (0, 0);

        // what the last saves completed: oldest first, what doesnt fit waits for the next tick
        while done < SEAL_PER_TICK && !out_of_time(done) {
            let Some(id) = self.completed.pop_front() else { break };
            self.completed_ids.remove(&id);
            done += 1;
            if still_due(&tx, id, now)? {
                sealed += seal(id).unwrap_or(0);
            }
        }

        let walk = self.walked_at.is_none_or(|t| now - t >= SEAL_WALK_EVERY);
        if walk && done < SEAL_PER_TICK && !out_of_time(done) {
            self.walked_at = Some(now);
            let scan = SEAL_PER_TICK * 4;
            let (mut ids, mut last) = sealable(&tx, self.seal_after, scan, now)?;
            // nothing left after the cursor: start over from the first file
            if last == self.seal_after && self.seal_after != 0 {
                (ids, last) = sealable(&tx, 0, scan, now)?;
            }
            // past the cap, the next walk picks up after the last one sealed
            let room = SEAL_PER_TICK - done;
            if ids.len() > room {
                ids.truncate(room);
                last = ids[room - 1];
            }
            for (i, id) in ids.iter().enumerate() {
                // out of time: the next walk picks up after the last one done
                if out_of_time(done) {
                    if i > 0 {
                        last = ids[i - 1];
                    } else {
                        last = self.seal_after;
                    }
                    break;
                }
                done += 1;
                sealed += seal(*id).unwrap_or(0);
            }
            self.seal_after = last;
        }
        tx.commit()?;
        Ok(sealed)
    }

    /// Whether the pruning walk has work now: not resting after a walk, nor stopping.
    pub fn prune_ids_awake(&self, now: i64) -> bool {
        !crate::frozen::STOPPING.load(std::sync::atomic::Ordering::Relaxed) && now >= self.ids.rest_until
    }

    /// Whether a pruning step is due (`prune_ids_some`): not while resting after a walk, nor while freezing says
    /// the indexer is stopping, and no sooner than the interval for how busy the writer is.
    pub fn prune_ids_due(&self, now: i64, idle: bool, queued: bool) -> bool {
        // a step every so often between saves, and only now and then while more are queued: steady ingest neither
        // starves it nor pays for it on every batch
        let every = if queued {
            PRUNE_IDS_QUEUED_EVERY
        } else if idle {
            PRUNE_IDS_IDLE_EVERY
        } else {
            PRUNE_IDS_EVERY
        };
        self.prune_ids_awake(now) && self.ids_step.is_none_or(|t| t.elapsed() >= every)
    }

    /// Take `release_ids` rows of quiet releases, for at most PRUNE_IDS_BUDGET
    /// (PRUNE_IDS_BUDGET_IDLE when `idle`): a step through the table by release
    /// from where the last one stopped. After the end it starts over once the
    /// least recently touched release it kept could be quiet (PRUNE_IDS_REST
    /// at least, SEAL_AGE at most). Steps come every PRUNE_IDS_EVERY at most
    /// between saves (PRUNE_IDS_IDLE_EVERY when `idle`), every
    /// PRUNE_IDS_QUEUED_EVERY while more are `queued`. A release with fewer than
    /// two files loses its index whatever its age: saves dont keep it up (`others`). A release none of whose files was touched,
    /// and that wasn't indexed itself, for SEAL_AGE loses its own row first, then the rest a chunk at a time
    /// over as many steps as it takes. The index is only a cache: without its
    /// own row, the next save to the release indexes it again in full
    /// (`index_release`); a spot placed on it compares in memory, so a late article is still found in its
    /// other files. Not for use inside a save transaction. Returns the rows taken.
    /// Only when `prune_ids_due`; a write lock another connection holds past PRUNE_IDS_BUSY lets the step go.
    pub fn prune_ids_some(&mut self, conn: &mut Connection, now: i64, idle: bool, queued: bool) -> Result<usize> {
        if !self.prune_ids_due(now, idle, queued) {
            return Ok(0);
        }
        self.ids_step = Some(Instant::now());
        self.ids_failing = None;
        // with saves about, a held lock lets the step go at once nearly
        conn.busy_timeout(if idle && !queued { PRUNE_IDS_BUSY } else { PRUNE_IDS_BUSY_SAVING })?;
        let done = self.prune_ids_step(conn, now, idle);
        // put back whatever came of the step: a failure here mustnt hide the step's own
        let restored = conn.busy_timeout(crate::db::BUSY_TIMEOUT);
        let done = match done {
            // the lock is held elsewhere: not again for a while, soo the writer doesnt keep waiting on it
            Err(e) if crate::frozen::is_busy(&e) => {
                crate::metrics::names::RELEASE_IDS.inc(&[("event", "busy"), ("shard", &self.shard.to_string())], 1);
                self.ids.rest_until = now + PRUNE_IDS_BUSY_REST;
                Ok(0)
            }
            // anything else would likely fail the same way next time: the walk rests a while, then passes over the
            // release it failed at, and only that one
            Err(e) => {
                self.ids.rest_until = now + PRUNE_IDS_FAILED_REST;
                if let Some(failed) = self.ids_failing {
                    self.ids.skip = Some(failed);
                }
                Err(e)
            }
            done => done,
        };
        match (done, restored) {
            (Err(e), _) | (Ok(_), Err(e)) => Err(e),
            (Ok(n), Ok(())) => Ok(n),
        }
    }

    fn prune_ids_step(&mut self, conn: &mut Connection, now: i64, idle: bool) -> Result<usize> {
        let (budget, chunk) =
            if idle { (PRUNE_IDS_BUDGET_IDLE, PRUNE_IDS_CHUNK_IDLE) } else { (PRUNE_IDS_BUDGET, PRUNE_IDS_CHUNK) };
        let started = Instant::now();
        // worked on a copy, kept only once the transaction has committed
        let mut w = self.ids;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut taken = 0;
        let mut quiet = 0;
        let mut looked = 0;
        while started.elapsed() < budget && looked < PRUNE_IDS_LOOKS {
            if let Some(release_id) = w.pruning {
                self.ids_failing = Some(release_id);
                if w.skip == Some(release_id) {
                    (w.pruning, w.skip) = (None, None);
                    continue;
                }
                // a save indexed it again in full since: it's kept up once more
                if is_indexed(&tx, release_id)? {
                    w.pruning = None;
                    continue;
                }
                // the rows are in id_hash order under the key: up to the chunk's last one, in one range
                let last: Option<i64> = tx
                    .prepare_cached(
                        "select id_hash from release_ids where release_id = ? order by id_hash limit 1 offset ?",
                    )?
                    .query_row(params![release_id, chunk - 1], |r| r.get(0))
                    .optional()?;
                taken += match last {
                    Some(last) => tx
                        .prepare_cached("delete from release_ids where release_id = ? and id_hash <= ?")?
                        .execute(params![release_id, last])?,
                    None => {
                        w.pruning = None;
                        tx.prepare_cached("delete from release_ids where release_id = ?")?.execute([release_id])?
                    }
                };
                continue;
            }
            looked += 1;
            self.ids_failing = None;
            let next: Option<i64> = tx
                .prepare_cached("select release_id from release_ids where release_id > ? order by release_id limit 1")?
                .query_row([w.after], |r| r.get(0))
                .optional()?;
            let Some(release_id) = next else {
                // the end: start over once the oldest release kept could be quiet, PRUNE_IDS_REST at the soonest;
                // with none kept, whatever is indexed next was indexed just then (`index_release`)
                w.after = 0;
                w.rest_until = w
                    .oldest_kept
                    .map_or(now + SEAL_AGE, |t| t + SEAL_AGE + 1)
                    .clamp(now + PRUNE_IDS_REST, now + SEAL_AGE);
                w.oldest_kept = None;
                break;
            };
            w.after = release_id;
            self.ids_failing = Some(release_id);
            if w.skip == Some(release_id) {
                w.skip = None;
                continue;
            }
            // a file never touched since the upgrade counts as old, as for sealing (`due`); a release without its own
            // row is half taken already
            let (files, touched, indexed, own): (i64, i64, i64, bool) = tx
                .prepare_cached(
                    "select count(*), coalesce(max(touched_at), 0),
                            coalesce((select at from release_indexed where release_id = ?1), 0),
                            exists (select 1 from release_ids where release_id = ?1 and id_hash = 0)
                     from files where release_id = ?1",
                )?
                .query_row([release_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
            let last = touched.max(indexed);
            // without its own row it's half taken (a step cut short by a restart): no save trusts what's left
            if files < 2 || last < now - SEAL_AGE || !own {
                // its own row first: a release half taken is indexed again in full
                let own = tx
                    .prepare_cached("delete from release_ids where release_id = ? and id_hash = 0")?
                    .execute([release_id])?;
                taken += own;
                tx.prepare_cached("delete from release_indexed where release_id = ?")?.execute([release_id])?;
                w.pruning = Some(release_id);
                // one half taken before a restart was counted then
                quiet += own as u64;
            } else {
                w.oldest_kept = Some(w.oldest_kept.map_or(last, |o| o.min(last)));
            }
        }
        // past here a failure is the step's, not a release's
        self.ids_failing = None;
        tx.commit()?;
        self.ids = w;
        let shard = self.shard.to_string();
        crate::metrics::names::RELEASE_IDS.inc(&[("event", "pruned_releases"), ("shard", &shard)], quiet);
        crate::metrics::names::RELEASE_IDS.inc(&[("event", "pruned_rows"), ("shard", &shard)], taken as u64);
        Ok(taken)
    }

    /// Releases of this shard's groups, in one transaction. The same release
    /// may come up in more than one batch.
    pub fn save<'a>(
        &mut self,
        conn: &mut Connection,
        ids: &Ids,
        batches: impl IntoIterator<Item = &'a [Release]>,
    ) -> Result<()> {
        self.save_prepared(conn, ids, batches, &Prepared::new())
    }

    /// `save`, with what `prepare` worked out of the releases beforehand (none needed: what isnt there is worked out
    /// here).
    pub(crate) fn save_prepared<'a>(
        &mut self,
        conn: &mut Connection,
        ids: &Ids,
        batches: impl IntoIterator<Item = &'a [Release]>,
        prepared: &Prepared,
    ) -> Result<()> {
        let r = self.save_in_transaction(conn, ids, batches, prepared);
        // domains inserted by a save that rolled back have no row: a later
        // save must not use their ids
        match r {
            Ok(()) => self.domains.committed(),
            Err(_) => self.domains.rolled_back(),
        }
        r
    }

    fn save_in_transaction<'a>(
        &mut self,
        conn: &mut Connection,
        ids: &Ids,
        batches: impl IntoIterator<Item = &'a [Release]>,
        prepared: &Prepared,
    ) -> Result<()> {
        let shard = self.shard;
        // late articles of a frozen release thaw it first (`frozen::thaw_incoming`)
        let frozen_attached = self.freezer.attached(conn);
        let domains = &mut self.domains;
        // blobs of sealed files that get late articles, decoded once each
        let mut sealed = SealedCache::with_limits(SEALED_CACHE_MAX_SEGMENTS, self.decode_budget);
        // files this save completed, for sealing once it's committed
        let mut completed = Vec::new();
        // the write lock up front: a transaction that read first never gets
        // the busy handler when it comes to write, soo another writer on the
        // shard would fail this save at once instead of making it wait
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let key = IdKey::of(&tx)?;
        let grouping_from = grouping_from(&tx)?;
        let now = chrono::Utc::now().timestamp();
        let (mut new_releases, mut new_articles) = (0i64, 0i64);
        // inline names this save changes, for the metrics once it's committed
        let mut renames = crate::postprocess::apply::Renames::new();
        let mut junk_rows: Vec<(String, String)> = Vec::new();
        {
            let mut upsert = tx.prepare_cached(
                "insert into releases
                    (id, name, size, complete, group_name, poster, posted_date, display_name, is_obfuscated)
                    values (?, ?, ?, ?, ?, ?, ?, ?, ?)
                    on conflict(name, group_name) do update set
                    poster = excluded.poster,
                    posted_date = excluded.posted_date,
                    display_name = case when releases.name_source is null
                                        then coalesce(excluded.display_name, releases.display_name)
                                        else releases.display_name end,
                    is_obfuscated = case when releases.name_source is null
                                         then excluded.is_obfuscated
                                         else releases.is_obfuscated end
                    returning id, size, parts, file_total",
            )?;
            let mut update_stats = tx
                .prepare_cached("update releases set size = ?, complete = ?, parts = ?, file_total = ? where id = ?")?;
            let mut segment_stmts = SegmentStatements::new(&tx)?;

            // nothing is ever saved for a blocked group, whichever path it came by
            let mut releases: Vec<Cow<'_, Release>> = Vec::new();
            let frozen = frozen_attached && crate::frozen::any(&tx)?;
            // a release split into one row per upload: its articles go to their upload's row
            let mut routed = tx.prepare_cached("select 1 from upload_routes limit 1")?.exists([])?;
            for release in batches.into_iter().flatten().filter(|r| !crate::filters::group_blocked(&r.group)) {
                // a frozen first upload's row comes back, with its routes, before they are read
                if frozen {
                    let collection = release.collection.as_deref().map(|c| key.hash(c));
                    let articles: Vec<(&str, &str)> = release
                        .articles
                        .iter()
                        .filter_map(|a| a.release_name.as_deref().map(|n| (n, a.author.as_str())))
                        .collect();
                    let thawed =
                        crate::frozen::thaw_incoming(&tx, collection, &release.name, &articles, &release.group, now)?;
                    // a thawed row may bring the shard's first routes
                    if thawed > 0 && !routed {
                        routed = tx.prepare_cached("select 1 from upload_routes limit 1")?.exists([])?;
                    }
                }
                let pieces =
                    if routed { route_uploads(&tx, &key, release, frozen)? } else { vec![Cow::Borrowed(release)] };
                for piece in pieces {
                    // a later upload's row thaws by its own name
                    let twin = piece.collection.is_none() && piece.name != release.name;
                    if frozen && twin {
                        let collection = piece.collection.as_deref().map(|c| key.hash(c));
                        let articles: Vec<(&str, &str)> = piece
                            .articles
                            .iter()
                            .filter_map(|a| a.release_name.as_deref().map(|n| (n, a.author.as_str())))
                            .collect();
                        crate::frozen::thaw_incoming(&tx, collection, &piece.name, &articles, &piece.group, now)?;
                    }
                    match piece {
                        Cow::Borrowed(r) => releases.extend(split_old_rows(&tx, grouping_from, r)?),
                        Cow::Owned(r) => releases.extend(
                            split_old_rows(&tx, grouping_from, &r)?.into_iter().map(|c| Cow::Owned(c.into_owned())),
                        ),
                    }
                }
            }

            for release in releases.iter().map(Cow::as_ref) {
                let t = Instant::now();
                let ready = prepared_for(prepared, release);
                let collection =
                    release.collection.as_deref().map(|c| collection_row(&tx, &key, c, release)).transpose()?;
                let name = collection.as_ref().map_or(release.name.as_str(), |c| c.name.as_str());
                // what the row's name is, not what this slice's files happen to be called
                let is_obfuscated = collection.as_ref().map_or(release.is_obfuscated, |c| c.is_obfuscated);
                // a program in it: the release goes, with what it had, and so does all that comes for it later
                if crate::junk::is_junk(&tx, name, &release.group)?
                    || collection.as_ref().map(|c| crate::junk::is_junk_collection(&tx, c.key)).transpose()?
                        == Some(true)
                {
                    continue;
                }
                let executable = match ready {
                    Some(p) => p.executable,
                    None => {
                        release.articles.iter().any(|a| a.filename.as_deref().is_some_and(crate::junk::is_executable))
                    }
                };
                if executable {
                    crate::junk::remember(&tx, name, &release.group, crate::junk::EXECUTABLE, now)?;
                    if let Some(c) = &collection {
                        crate::junk::remember_collection(&tx, c.key)?;
                    }
                    // its row goes after the loop: ids this save still holds must not be freed for reuse under it
                    junk_rows.push((name.to_string(), release.group.clone()));
                    continue;
                }
                let id = global_id(ids.next()?, shard);
                if let Some(first) = release
                    .display_name
                    .as_deref()
                    .map(|d| inline_rename(&tx, name, &release.group, d, is_obfuscated))
                    .transpose()?
                    .flatten()
                {
                    renames.push((crate::naming::NameSource::InlineBody as u8, first));
                }
                let row: Option<Saved> = upsert
                    .query_row(
                        params![
                            id,
                            name,
                            release.size,
                            release.complete as i64,
                            release.group,
                            release.poster,
                            release.date,
                            release.display_name,
                            is_obfuscated as i64,
                        ],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                    )
                    .optional()?;
                profile::UPSERT.add_since(t);
                let Some((release_id, old_size, old_parts, old_file_total)) = row else { continue };
                // inserted, not merged: the id that came back is the one proposed
                if release_id == id {
                    new_releases += 1;
                }
                if let Some(c) = collection.as_ref().filter(|c| !c.known) {
                    tx.prepare_cached("insert or replace into collections (key, release_id) values (?, ?)")?
                        .execute(params![c.key, release_id])?;
                }

                let t = Instant::now();
                // articles by file, in arrival order
                let mut by_file: IndexMap<&str, Vec<(usize, &Article)>> = IndexMap::new();
                for (i, a) in release.articles.iter().enumerate().filter(|(_, a)| !a.message_id.is_empty()) {
                    by_file.entry(a.filename.as_deref().unwrap_or("")).or_default().push((i, a));
                }

                // and whether each might be a copy of a sealed article (see `SealedCache`). A part its file has
                // already under another message-id is counted too: it isnt known to be a repost, as different files
                // can share a file row (a name the subject doesnt give, or gives the same for each)
                let mut added: Vec<(&Article, bool)> = Vec::new();
                // a release this save made has in the index only what this save puts there: kept in memory
                let made_now = release_id == id;
                let mut mirror: Option<HashMap<i64, i64>> = None;
                for (name, articles) in by_file {
                    let mut f = file(&tx, release_id, name)?;
                    // a release with other files is checked for the same message-id
                    // in all of them by its index, one lookup an article
                    let others = tx
                        .prepare_cached("select 1 from files where release_id = ? and id != ? limit 1")?
                        .exists(params![release_id, f.id])?;
                    if others && index_release(&tx, &key, release_id)? {
                        crate::metrics::names::RELEASE_IDS
                            .inc(&[("event", "indexed"), ("shard", &shard.to_string())], 1);
                    }
                    let index = others.then_some((release_id, &key));
                    if made_now && others && mirror.is_none() {
                        mirror = Some(release_ids_of(&tx, release_id)?);
                    }
                    let before = added.len();
                    let mut timing = Timing::default();
                    for (i, a) in articles {
                        if add_segment_with(
                            &tx,
                            &mut segment_stmts,
                            domains,
                            &f,
                            index,
                            mirror.as_mut(),
                            a,
                            &mut sealed,
                        )? {
                            let unsure = f.sealed && sealed.guessed;
                            // an article that may be a copy of one in the blob isn't timed: sealing again can drop
                            // it, and nothing takes it back out of the timing row, so its counts would stay
                            if !unsure {
                                timing.fold(a, &key, ready.map(|p| &p.articles[i]));
                            }
                            if unsure {
                                // maybe a copy of what the blob has: only its part
                                // counts, the rest is held back till sealing the file
                                // again tells (`seal_rows` works the parts out again
                                // from what is left, and folds in what new rows said)
                                if let Some(p) = a.part {
                                    insert_part(&mut f.seen, p);
                                }
                            } else {
                                f.add(a);
                            }
                            added.push((a, unsure));
                        }
                    }
                    if added.len() > before {
                        put_file(&tx, &f)?;
                        timing.write(&tx, f.id, release_id, f.sealed)?;
                        if !f.sealed && f.expected.is_some_and(|e| is_exactly(&f.seen, e)) {
                            completed.push(f.id);
                        }
                    } else {
                        // a file just made whose articles all were in other files
                        let gone = tx
                            .prepare_cached(
                                "delete from files where id = ? and touched_at is null and blob is null
                                 and not exists (select 1 from segments where file_id = ?)",
                            )?
                            .execute(params![f.id, f.id])?;
                        // down to one file its index is no longer kept up (see
                        // `others` above): dropped, to be made again by the next file
                        let others = tx
                            .prepare_cached("select 1 from files where release_id = ? limit 1 offset 1")?
                            .exists([release_id])?;
                        if gone > 0 && !others {
                            unindex_release(&tx, release_id)?;
                            mirror = None;
                        }
                    }
                }
                profile::ARTICLES.add_since(t);

                if added.is_empty() {
                    continue;
                }
                new_articles += added.len() as i64;

                let t = Instant::now();
                let file_total = added
                    .iter()
                    .filter(|(_, unsure)| !unsure)
                    .filter_map(|(a, _)| a.file_total)
                    .chain(old_file_total)
                    .max();
                let old_parts = old_parts.unwrap_or(0);
                let earlier_size = if old_parts > 0 { old_size.unwrap_or(0) } else { 0 };
                let complete = release_complete(&tx, release_id, file_total)?;
                update_stats.execute(params![
                    earlier_size + added.iter().map(|(a, _)| a.bytes).sum::<i64>(),
                    complete as i64,
                    old_parts + added.len() as i64,
                    file_total,
                    release_id
                ])?;
                // it wants a look once it has been quiet. a touch moves the time
                // above any a run claimed, soo an article during a run isnt lost
                if !SKIP_ENQUEUE_FOR_BENCH.load(Ordering::Relaxed) {
                    let settle = if complete { &COMPLETE_SETTLE_SECS } else { &SETTLE_SECS };
                    tx.prepare_cached(
                        "insert into pp_queue (release_id, due_at) values (?1, ?2)
                         on conflict(release_id) do update set due_at = excluded.due_at",
                    )?
                    .execute(params![release_id, now + settle.load(Ordering::Relaxed)])?;
                }
                profile::STATS.add_since(t);
            }

            add_totals(&tx, new_releases, new_articles)?;
            for (name, group) in &junk_rows {
                crate::junk::remove_by_key(&tx, name, group, crate::junk::EXECUTABLE, now)?;
            }
        }

        let t = Instant::now();
        let r = tx.commit();
        profile::COMMIT.add_since(t);
        if r.is_ok() {
            crate::postprocess::apply::count_names(&renames);
            for id in completed {
                self.remember_completed(id);
            }
        }
        r
    }
}

/// id, size, parts and file total of the row a release is saved into
type Saved = (i64, Option<i64>, Option<i64>, Option<i64>);

/// the id collections started from in this shard, see `start_grouping`. 1, so
/// nothing is an old row, until an indexer of this build has run
pub(crate) fn grouping_from(conn: &Connection) -> Result<i64> {
    let from = conn.prepare_cached("select value from meta where key = 'grouping_from'")?.query_row([], |r| r.get(0));
    Ok(from.optional()?.unwrap_or(1))
}

/// Note where collections start in each shard, the first time an indexer of
/// this build runs: what is there by then was saved by file name, whoever
/// saved it, and its files stay in their rows (`split_old_rows`). Not on
/// open: a menu of this build can open a shard while an older indexer is
/// still writing by file name.
pub fn start_grouping(main: &Path) -> Result<()> {
    for path in shard_paths(main) {
        db::open_shard(&path)?.execute(
            "insert or ignore into meta (key, value) select 'grouping_from', coalesce(max(id), 0) + 1 from releases",
            [],
        )?;
    }
    Ok(())
}

/// A release whose row `split_uploads` made one row per upload of, in pieces: each article to the row of the upload
/// whose posting window (`upload_routes`) its Date is in, or nearest within `ROUTE_SLACK_SECS`. The first upload's
/// articles, and any with no Date or far from every window, stay in the release as it came; another upload's go in a
/// piece named as its row, with no collection, soo the upsert finds that row. A release of no split row comes back as
/// it is.
fn route_uploads<'a>(
    tx: &Connection,
    key: &IdKey,
    release: &'a Release,
    frozen: bool,
) -> Result<Vec<Cow<'a, Release>>> {
    let origin: Option<i64> = match release.collection.as_deref() {
        Some(c) => tx
            .prepare_cached("select release_id from collections where key = ?")?
            .query_row([key.hash(c)], |r| r.get(0))
            .optional()?,
        None => tx
            .prepare_cached("select id from releases where name = ? and group_name = ?")?
            .query_row(params![release.name, release.group], |r| r.get(0))
            .optional()?,
    };
    let Some(origin) = origin else { return Ok(vec![Cow::Borrowed(release)]) };
    // each upload's row by its name now (a rename keeps the id), live or frozen (its piece thaws it by that name),
    // and its window; a row gone since isnt routed to
    type Route = (i64, String, i64, i64);
    let sql = if frozen {
        "select u.release_id, coalesce(r.name, f.name), u.first_date, u.last_date from upload_routes u
         left join releases r on r.id = u.release_id left join frozen.frozen_releases f on f.id = u.release_id
         where u.origin_id = ?1 and u.first_date is not null and u.last_date is not null
         and coalesce(r.group_name, f.group_name) = ?2 order by u.first_date"
    } else {
        "select u.release_id, r.name, u.first_date, u.last_date from upload_routes u
         join releases r on r.id = u.release_id
         where u.origin_id = ?1 and u.first_date is not null and u.last_date is not null and r.group_name = ?2
         order by u.first_date"
    };
    let routes: Vec<Route> = tx
        .prepare_cached(sql)?
        .query_map(params![origin, release.group], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<Result<_>>()?;
    if routes.len() < 2 {
        return Ok(vec![Cow::Borrowed(release)]);
    }
    let slack = routes.windows(2).map(|w| (w[1].2 - w[0].3) / 2).fold(ROUTE_SLACK_SECS, i64::min).max(0);
    // each article's upload row: None, it stays
    let route_of = |a: &Article| -> Option<usize> {
        let t = crate::dates::naive_timestamp(&a.date)?;
        let (i, far) = routes
            .iter()
            .enumerate()
            .map(|(i, (_, _, first, last))| (i, if t < *first { first - t } else { (t - last).max(0) }))
            .min_by_key(|(_, far)| *far)?;
        (far <= slack && routes.get(i).is_some_and(|r| r.0 != origin)).then_some(i)
    };
    let routed: Vec<Option<usize>> = release.articles.iter().map(route_of).collect();
    if routed.iter().all(Option::is_none) {
        return Ok(vec![Cow::Borrowed(release)]);
    }
    let mut by_route: IndexMap<usize, Vec<Article>> = IndexMap::new();
    let mut stay = Vec::new();
    for (a, route) in release.articles.iter().zip(routed) {
        match route {
            Some(i) => by_route.entry(i).or_default().push(a.clone()),
            None => stay.push(a.clone()),
        }
    }
    let piece = |name: &str, collection: Option<String>, articles: Vec<Article>| Release {
        name: name.to_string(),
        size: articles.iter().map(|a| a.bytes).sum(),
        is_obfuscated: release.is_obfuscated,
        display_name: release.display_name.clone(),
        complete: false,
        group: release.group.clone(),
        poster: release.poster.clone(),
        date: release.date.clone(),
        collection,
        articles,
    };
    let mut pieces = Vec::new();
    if !stay.is_empty() {
        pieces.push(Cow::Owned(piece(&release.name, release.collection.clone(), stay)));
    }
    for (i, articles) in by_route {
        if let Some((_, name, ..)) = routes.get(i) {
            pieces.push(Cow::Owned(piece(name, None, articles)));
        }
    }
    Ok(pieces)
}

/// A collection's articles that were saved, or started, under their own file
/// name before collections: the file stays in its row, same poster, soo it
/// doesnt split and a reindexed article is still seen as a duplicate. What
/// is left, if any, goes to the collection. A release with no such file comes
/// back as it is.
fn split_old_rows<'a>(tx: &Connection, grouping_from: i64, release: &'a Release) -> Result<Vec<Cow<'a, Release>>> {
    if release.collection.is_none() || grouping_from <= 1 {
        return Ok(vec![Cow::Borrowed(release)]);
    }
    let mut old =
        tx.prepare_cached("select 1 from releases where name = ? and group_name = ? and id < ? and poster = ?")?;
    let mut known: HashMap<&str, bool> = HashMap::new();
    for a in &release.articles {
        let Some(name) = a.release_name.as_deref() else { continue };
        if !known.contains_key(name) {
            known.insert(name, old.exists(params![name, release.group, grouping_from, a.author])?);
        }
    }
    if !known.values().any(|in_old_row| *in_old_row) {
        return Ok(vec![Cow::Borrowed(release)]);
    }
    let piece = |name: &str, collection: Option<String>, articles: Vec<Article>| Release {
        name: name.to_string(),
        size: articles.iter().map(|a| a.bytes).sum(),
        is_obfuscated: crate::parser::is_obfuscated(name),
        collection,
        // its own, soo the next slice's probe still finds the row
        poster: articles.first().map_or_else(|| release.poster.clone(), |a| a.author.clone()),
        articles,
        display_name: release.display_name.clone(),
        complete: false,
        group: release.group.clone(),
        date: release.date.clone(),
    };
    let mut rest = Vec::new();
    let mut in_old: IndexMap<&str, Vec<Article>> = IndexMap::new();
    for a in &release.articles {
        match a.release_name.as_deref().filter(|name| known.get(name).copied().unwrap_or(false)) {
            Some(name) => {
                // an old row holds one file: a total read from the whole post's counter
                // would leave it incomplete for good
                let mut a = a.clone();
                if crate::collections::unread_file_total(&crate::collections::subject_base(&a.subject)).is_some() {
                    a.file_total = None;
                }
                in_old.entry(name).or_default().push(a);
            }
            None => rest.push(a.clone()),
        }
    }
    let mut pieces: Vec<Cow<'a, Release>> =
        in_old.into_iter().map(|(name, articles)| Cow::Owned(piece(name, None, articles))).collect();
    // the shortest name may have been of a file that stayed behind
    if let Some(name) = rest.iter().filter_map(|a| a.release_name.as_deref()).min_by_key(|n| n.len()) {
        pieces.push(Cow::Owned(piece(name, release.collection.clone(), rest.clone())));
    }
    Ok(pieces)
}

/// A collection in a save: its hash, the name its release is saved under,
/// and whether `collections` already points at it.
struct CollectionRow {
    key: i64,
    name: String,
    is_obfuscated: bool,
    known: bool,
}

/// The name a collection's release is saved under: its row's when it has one.
/// A new one takes the slice's name; when another release in the group has
/// that, with 8 digits of the key's hash after it, as `a [4]` has its count.
fn collection_row(tx: &Connection, key: &IdKey, collection: &str, release: &Release) -> Result<CollectionRow> {
    let hash = key.hash(collection);
    // a purge can leave a row here, whose release is gone: it finds nothing
    let known: Option<(String, bool)> = tx
        .prepare_cached(
            "select r.name, r.is_obfuscated from collections c join releases r on r.id = c.release_id where c.key = ?",
        )?
        .query_row([hash], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?;
    if let Some((name, is_obfuscated)) = known {
        // repeated posts under one key add up: past the cap it starts a new release, and the key points there
        let files: i64 = tx
            .prepare_cached(
                "select count(*) from (select 1 from collections c join files f on f.release_id = c.release_id
                 where c.key = ? limit ?)",
            )?
            .query_row(params![hash, crate::collections::MAX_COLLECTION_FILES + 1], |r| r.get(0))?;
        if files <= crate::collections::MAX_COLLECTION_FILES {
            return Ok(CollectionRow { key: hash, name, is_obfuscated, known: true });
        }
    }
    let mut taken = tx.prepare_cached("select 1 from releases where name = ? and group_name = ?")?;
    let mut name = release.name.clone();
    // the next number when 8 digits of two hashes meet: the upsert would merge the posts
    let mut digits = hash.rem_euclid(100_000_000);
    while taken.exists(params![name, release.group])? {
        name = format!("{} [{digits:08}]", release.name);
        digits = (digits + 1) % 100_000_000;
    }
    Ok(CollectionRow { key: hash, name, is_obfuscated: release.is_obfuscated, known: false })
}

pub(crate) fn add_totals(conn: &Connection, releases: i64, articles: i64) -> Result<()> {
    if releases > 0 || articles > 0 {
        let mut stmt = conn.prepare_cached("update meta set value = value + ? where key = ?")?;
        stmt.execute(params![releases, "releases"])?;
        stmt.execute(params![articles, "articles"])?;
    }
    Ok(())
}

/// Take `count` dropped copies of `bytes` in all out of release
/// `release_id`'s size and parts and the shard's article total: the save
/// that kept them as rows counted them (see `SealedCache`).
pub(crate) fn uncount_copies(conn: &Connection, release_id: i64, bytes: i64, count: i64) -> Result<()> {
    conn.prepare_cached("update releases set size = size - ?, parts = parts - ? where id = ?")?
        .execute(params![bytes, count, release_id])?;
    conn.prepare_cached("update meta set value = value - ? where key = 'articles'")?.execute([count])?;
    Ok(())
}

/// Save releases from anywhere (AI search): each to its group's shard, in one
/// transaction per shard.
/// Whether the save's upsert changes this row's name to `display`, and if soo whether
/// it's the first real name of an obfuscated post. Repeat saves of a name change nothing.
fn inline_rename(
    tx: &Connection,
    name: &str,
    group: &str,
    display: &str,
    is_obfuscated: bool,
) -> rusqlite::Result<Option<bool>> {
    let row: Option<(Option<String>, Option<i64>)> = tx
        .prepare_cached("select display_name, name_source from releases where name = ?1 and group_name = ?2")?
        .query_row(params![name, group], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?;
    Ok(match row {
        None => Some(is_obfuscated),
        // a later stage's name: the upsert leaves it
        Some((_, Some(_))) => None,
        Some((Some(old), None)) if old == display => None,
        Some((old, None)) => Some(old.is_none() && is_obfuscated),
    })
}

pub fn save(main: &Path, releases: &[Release]) -> Result<()> {
    let ids = Ids::new(main);
    let mut by_shard: Vec<Vec<Release>> = vec![Vec::new(); SHARDS];
    for r in releases {
        by_shard[shard_of(&r.group)].push(r.clone());
    }
    for (shard, list) in by_shard.iter().enumerate().filter(|(_, l)| !l.is_empty()) {
        let path = shard_path(main, shard);
        let mut conn = db::open_shard(&path)?;
        // late articles of a frozen release thaw it here too, rather than starting a second copy
        if crate::frozen::path(&path).exists() {
            crate::frozen::attach(&conn, &path)?;
        }
        ShardWriter::new(shard).save(&mut conn, &ids, [list.as_slice()])?;
    }
    Ok(())
}

// ---------------------------------------------------------------- reading

/// What deleting a release took.
pub(crate) struct Deleted {
    pub articles: i64,
}

/// `delete_release` without counting its articles: a sealed blob in the sealed log is
/// read to count it, and freezing knows the count already (`frozen::remove_live`).
pub(crate) fn delete_release_rows(tx: &Connection, release_id: i64) -> Result<()> {
    let files: Vec<i64> = tx
        .prepare_cached("select id from files where release_id = ?")?
        .query_map([release_id], |r| r.get(0))?
        .collect::<Result<_>>()?;
    for file in files {
        tx.prepare_cached("delete from segments where file_id = ?")?.execute([file])?;
        tx.prepare_cached("delete from held_back where file_id = ?")?.execute([file])?;
    }
    tx.prepare_cached("delete from release_ids where release_id = ?")?.execute([release_id])?;
    tx.prepare_cached("delete from files where release_id = ?")?.execute([release_id])?;
    tx.prepare_cached("delete from releases where id = ?")?.execute([release_id])?;
    Ok(())
}

/// A release and everything of its, in the caller's transaction: triggers take
/// its search entry and registered rows. Counts what `meta.articles` must lose.
pub(crate) fn delete_release(tx: &Connection, release_id: i64) -> Result<Deleted> {
    let mut articles = 0i64;
    let files: Vec<(i64, Option<Vec<u8>>)> = tx
        .prepare_cached("select id, blob from files where release_id = ?")?
        .query_map([release_id], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_>>()?;
    for (file, blob) in files {
        if let Some(blob) = blob {
            articles += crate::blob::count(&blob).map_err(blob_error)? as i64;
        }
        articles += tx.prepare_cached("delete from segments where file_id = ?")?.execute([file])? as i64;
        tx.prepare_cached("delete from held_back where file_id = ?")?.execute([file])?;
    }
    tx.prepare_cached("delete from release_ids where release_id = ?")?.execute([release_id])?;
    tx.prepare_cached("delete from files where release_id = ?")?.execute([release_id])?;
    tx.prepare_cached("delete from releases where id = ?")?.execute([release_id])?;
    Ok(Deleted { articles })
}

/// Whether a shard holds `limit` or more incomplete releases.
pub fn too_many_incomplete(main: &Path, limit: i64) -> Result<bool> {
    for path in shard_paths(main) {
        let conn = db::open_shard(&path)?;
        let n: i64 =
            conn.query_row("select count(*) from (select 1 from releases where complete = 0 limit ?)", [limit], |r| {
                r.get(0)
            })?;
        if n >= limit {
            return Ok(true);
        }
    }
    Ok(false)
}

/// A release's files, `limit` at most. `conn` is one shard opened on its own.
pub fn release_files(conn: &Connection, release_id: i64, limit: usize) -> Result<Vec<FileInfo>> {
    conn.prepare_cached(
        "select id, filename, subject, subject_part, expected, file_total, seen, touched_at, blob is not null
         from files where release_id = ? order by id limit ?",
    )?
    .query_map(params![release_id, limit as i64], |r| {
        Ok(FileInfo {
            id: r.get(0)?,
            filename: r.get(1)?,
            subject: r.get(2)?,
            subject_part: r.get(3)?,
            expected: r.get(4)?,
            file_total: r.get(5)?,
            parts_seen: seen_count(&r.get::<_, Vec<u8>>(6)?),
            touched_at: r.get(7)?,
            sealed: r.get(8)?,
        })
    })?
    .collect()
}

/// where a segment sorts: a missing part first
type SegmentOrder = (Option<i64>, String);

/// What a pick keeps of the segments going past: never more than it asks for.
struct Picked {
    pick: SegmentPick,
    kept: std::collections::BTreeMap<SegmentOrder, i64>,
}

impl Picked {
    /// how many one part of `Parts` keeps: a part can be reposted, not without end
    const PER_PART: usize = 16;

    fn offer(&mut self, seg: SegmentRef) {
        let key = (seg.part, seg.message_id);
        if let SegmentPick::Parts(parts) = &self.pick {
            let Some(part) = key.0.filter(|p| parts.contains(p)) else { return };
            let same = self.kept.keys().filter(|k| k.0 == Some(part)).count();
            if same < Self::PER_PART {
                self.kept.insert(key, seg.bytes);
            }
        } else {
            self.kept.insert(key, seg.bytes);
            match self.pick {
                SegmentPick::First(n) if self.kept.len() > n => {
                    self.kept.pop_last();
                }
                SegmentPick::Last(n) if self.kept.len() > n => {
                    self.kept.pop_first();
                }
                _ => {}
            }
        }
        #[cfg(test)]
        HELD_ROWS.with(|h| h.set(h.get().max(self.kept.len())));
    }
}

#[cfg(test)]
thread_local! {
    static HELD_ROWS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// A file's articles: its sealed blob and any rows that came after sealing,
/// from one snapshot, deduped by message-id, in (part, message-id) order with
/// a missing part first. Holds no more than `pick` asks for beyond the blob,
/// itself capped at `SEAL_MAX_SEGMENTS`. A corrupt blob is an error. `conn` is
/// one shard opened on its own.
pub fn file_segments(conn: &Connection, file_id: i64, pick: &SegmentPick) -> Result<Vec<SegmentRef>> {
    // a file sealed between two separate reads would show its articles twice
    // or not at all. a caller already in a transaction has its snapshot
    let _snapshot = if conn.is_autocommit() { Some(conn.unchecked_transaction()?) } else { None };
    let mut picked = Picked { pick: pick.clone(), kept: Default::default() };
    let mut suffixes: HashMap<i64, Option<String>> = HashMap::new();
    let mut sealed: HashSet<String> = HashSet::new();
    for seg in sealed_segments(conn, file_id)?.unwrap_or_default() {
        sealed.insert(seg_key(conn, &mut suffixes, &seg)?);
        let suffix = suffixes.get(&seg.domain).and_then(|s| s.as_deref());
        picked.offer(SegmentRef { message_id: decode(&seg.local, suffix), part: seg.part, bytes: seg.bytes });
    }
    let mut rows = conn.prepare_cached(
        "select s.local, s.domain, d.suffix, s.part, s.bytes
         from segments s left join domains d on d.id = s.domain where s.file_id = ?",
    )?;
    let mut rows = rows.query([file_id])?;
    while let Some(r) = rows.next()? {
        let (local, domain): (Vec<u8>, i64) = (r.get(0)?, r.get(1)?);
        let suffix: Option<String> = r.get(2)?;
        if !sealed.is_empty() && sealed.contains(&dedup_key(&local, domain, suffix.as_deref())) {
            continue;
        }
        picked.offer(SegmentRef { message_id: decode(&local, suffix.as_deref()), part: r.get(3)?, bytes: r.get(4)? });
    }
    Ok(picked.kept.into_iter().map(|((part, message_id), bytes)| SegmentRef { message_id, part, bytes }).collect())
}

/// A release's articles, ordered by filename then part (ties by message-id),
/// for building its NZB. `conn` has the shards attached.
pub fn articles(conn: &Connection, release_id: i64) -> Result<Vec<ArticleRow>> {
    let mut rows = Vec::new();
    each_article(conn, release_id, &mut |row| {
        rows.push(row);
        Ok(())
    })?;
    rows.sort_by(|a, b| (&a.filename, a.part, &a.message_id).cmp(&(&b.filename, b.part, &b.message_id)));
    Ok(rows)
}

/// A release's articles one at a time, in no particular order, none held.
/// `conn` has the shards attached.
pub fn each_article(conn: &Connection, release_id: i64, each: &mut dyn FnMut(ArticleRow) -> Result<()>) -> Result<()> {
    let s = shard_of_id(release_id);
    // rows and blobs from one snapshot: a file sealed between two separate
    // reads would show its articles twice (or not at all). a caller already
    // in a transaction has one
    let _snapshot = if conn.is_autocommit() { Some(conn.unchecked_transaction()?) } else { None };
    let mut stmt = conn.prepare(&format!(
        "select s.local, d.suffix, f.filename, s.part, f.expected, s.bytes, f.subject, r.poster, r.posted_date
         from s{s}.files f join s{s}.segments s on s.file_id = f.id join s{s}.releases r on r.id = f.release_id
         left join s{s}.domains d on d.id = s.domain
         where f.release_id = ?"
    ))?;
    let rows = stmt.query_map([release_id], |r| {
        let filename: String = r.get(2)?;
        Ok(ArticleRow {
            message_id: decode(&r.get::<_, Vec<u8>>(0)?, r.get::<_, Option<String>>(1)?.as_deref()),
            filename: (!filename.is_empty()).then_some(filename),
            part: r.get(3)?,
            total_parts: r.get(4)?,
            bytes: r.get(5)?,
            subject: r.get(6)?,
            poster: r.get(7)?,
            posted_date: r.get(8)?,
        })
    })?;
    for row in rows {
        each(row?)?;
    }

    // sealed files: their blobs, next to any rows that came after sealing
    let mut sealed = conn.prepare(&format!(
        "select f.blob, f.filename, f.expected, f.subject, r.poster, r.posted_date
         from s{s}.files f join s{s}.releases r on r.id = f.release_id
         where f.release_id = ? and f.blob is not null"
    ))?;
    let mut suffix_of = conn.prepare(&format!("select suffix from s{s}.domains where id = ?"))?;
    // per blob, soo what is held stays within what one blob takes
    let mut suffixes: HashMap<i64, Option<String>> = HashMap::new();
    let mut blob_rows = sealed.query([release_id])?;
    while let Some(r) = blob_rows.next()? {
        suffixes.clear();
        let blob: Vec<u8> = r.get(0)?;
        let filename: String = r.get(1)?;
        for seg in crate::blob::decode(&blob).map_err(blob_error)? {
            let suffix = match suffixes.entry(seg.domain) {
                Entry::Occupied(e) => e.into_mut(),
                Entry::Vacant(e) => e.insert(suffix_of.query_row([seg.domain], |r| r.get(0)).optional()?),
            };
            each(ArticleRow {
                message_id: decode(&seg.local, suffix.as_deref()),
                filename: (!filename.is_empty()).then(|| filename.clone()),
                part: seg.part,
                total_parts: r.get(2)?,
                bytes: Some(seg.bytes),
                subject: r.get(3)?,
                poster: r.get(4)?,
                posted_date: r.get(5)?,
            })?;
        }
    }
    Ok(())
}

/// articles in shard `schema` (`s3`, or `main` for a shard opened on its
/// own): loose rows plus every sealed blob's segments
pub fn article_count(conn: &Connection, schema: &str) -> Result<i64> {
    let loose: i64 = conn.query_row(&format!("select count(*) from {schema}.segments"), [], |r| r.get(0))?;
    let mut sealed = 0i64;
    let mut stmt = conn.prepare(&format!("select blob from {schema}.files where blob is not null"))?;
    let mut rows = stmt.query([])?;
    while let Some(r) = rows.next()? {
        let b: Vec<u8> = r.get(0)?;
        sealed += crate::blob::decode(&b).map_err(blob_error)?.len() as i64;
    }
    Ok(loose + sealed)
}

/// (releases, articles) over every shard, from the running totals. `conn` has
/// the shards attached.
pub fn totals(conn: &Connection) -> Result<(i64, i64)> {
    let sql = format!(
        "select coalesce(sum(case when key = 'releases' then value end), 0),
                coalesce(sum(case when key = 'articles' then value end), 0)
         from ({})",
        each_shard(|i| format!("select key, value from s{i}.meta"))
    );
    conn.query_row(&sql, [], |r| Ok((r.get(0)?, r.get(1)?)))
}

/// Delete incomplete releases (and their files and articles) from every shard.
/// What goes is counted inside the transaction that deletes it and taken off
/// the shard's totals there, soo the writers saving to the shard at the same
/// time (purge holds only the shared guard) never leave them wrong.
pub fn purge_incomplete(main: &Path) -> Result<()> {
    for path in shard_paths(main) {
        let conn = db::open_shard(&path)?;
        // the write lock from the start: the counts are of what is deleted
        let tx = rusqlite::Transaction::new_unchecked(&conn, rusqlite::TransactionBehavior::Immediate)?;
        let removed: i64 = tx.query_row("select count(*) from releases where complete = 0", [], |r| r.get(0))?;
        let doomed = "select f.id from files f join releases r on r.id = f.release_id where r.complete = 0";
        let loose: i64 =
            tx.query_row(&format!("select count(*) from segments where file_id in ({doomed})"), [], |r| r.get(0))?;
        let mut sealed = 0i64;
        {
            let mut stmt =
                tx.prepare(&format!("select blob from files where blob is not null and id in ({doomed})"))?;
            let mut rows = stmt.query([])?;
            while let Some(r) = rows.next()? {
                let b: Vec<u8> = r.get(0)?;
                sealed += crate::blob::decode(&b).map_err(blob_error)?.len() as i64;
            }
        }
        tx.execute_batch(&format!(
            "delete from segments where file_id in ({doomed});
             delete from held_back where file_id in ({doomed});
             delete from release_ids where release_id in (select id from releases where complete = 0);
             delete from collections where release_id in (select id from releases where complete = 0);
             delete from files where release_id in (select id from releases where complete = 0);
             delete from releases where complete = 0;"
        ))?;
        let mut stmt = tx.prepare("update meta set value = max(value - ?, 0) where key = ?")?;
        stmt.execute(params![removed, "releases"])?;
        stmt.execute(params![loose + sealed, "articles"])?;
        drop(stmt);
        tx.commit()?;
        let _ = conn.query_row("pragma wal_checkpoint(truncate)", [], |_| Ok(()));
        conn.execute_batch("vacuum")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires explicit isolated PostgreSQL metadata helper qualification"]
    fn native_meta_keeps_numbers_texts_and_driver_errors_distinct() {
        let schema = crate::pg::testing::Schema::new().expect("explicit isolated PostgreSQL required");
        let mut client = schema.pool.get().unwrap();
        {
            let pg = crate::pg::sql::Pg::new(&mut client);
            assert_eq!(meta_text(&pg, "owner").unwrap(), None);
            put_meta_text(&pg, "owner", "a.example").unwrap();
            put_meta_text(&pg, "owner", "b.example").unwrap();
            assert_eq!(meta_text(&pg, "owner").unwrap().as_deref(), Some("b.example"));
            assert_eq!(meta(&pg, "owner").unwrap(), None, "text is not coerced into an integer");
            put_meta(&pg, "count", 7).unwrap();
            assert_eq!(meta(&pg, "count").unwrap(), Some(7));
            put_meta(&pg, "count", i64::MIN).unwrap();
            assert_eq!(meta(&pg, "count").unwrap(), Some(i64::MIN));
            assert_eq!(meta_text(&pg, "count").unwrap(), None);
            assert_eq!(meta(&pg, "next_seq").unwrap(), Some(1));
        }
        client.batch_execute("alter table meta rename to unavailable_metadata").unwrap();
        let pg = crate::pg::sql::Pg::new(&mut client);
        let error = meta(&pg, "count").unwrap_err();
        assert_eq!(error.postgres().unwrap().code(), Some(&postgres::error::SqlState::UNDEFINED_TABLE), "missing storage must retain its original driver cause");
    }

    /// a table of sealed files for a `SealedCache`: file i holds `per` segments
    fn sealed_files(files: i64, per: i64) -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "create table files (id INTEGER PRIMARY KEY, blob BLOB);
             create table domains (id INTEGER PRIMARY KEY, suffix TEXT NOT NULL);",
        )
        .unwrap();
        for id in 1..=files {
            let segs: Vec<crate::blob::Seg> = (0..per)
                .map(|p| crate::blob::Seg {
                    part: Some(p),
                    bytes: 1,
                    domain: id,
                    local: format!("{id}-{p}").into_bytes(),
                })
                .collect();
            conn.execute("insert into files (id, blob) values (?, ?)", params![id, crate::blob::encode(&segs)])
                .unwrap();
        }
        conn
    }

    #[test]
    fn the_sealed_cache_stays_within_its_bound_and_still_finds_every_duplicate() {
        let conn = sealed_files(40, 3);
        let mut cache = SealedCache::with_limits(10, usize::MAX);
        // one late article per sealed file, twice around
        for _ in 0..2 {
            for id in 1..=40 {
                assert!(cache.contains(&conn, id, format!("{id}-1").as_bytes(), id).unwrap(), "file {id}");
                assert!(!cache.contains(&conn, id, b"new", id).unwrap());
                assert!(cache.segments <= 10, "{} segments kept", cache.segments);
                assert_eq!(cache.segments, cache.files.values().map(|(_, s)| s.len()).sum::<usize>());
            }
        }
    }

    /// The least recently used file goes when the cache is full, not all of
    /// them: a file checked over and over stays decoded.
    #[test]
    fn the_sealed_cache_lets_the_least_recently_used_file_go() {
        let conn = sealed_files(5, 4);
        let mut cache = SealedCache::with_limits(12, usize::MAX);
        for id in [1, 2, 3, 1, 4, 1, 5, 1] {
            assert!(cache.contains(&conn, id, format!("{id}-1").as_bytes(), id).unwrap(), "file {id}");
        }
        assert_eq!(cache.decoded, 5 * 4, "file 1 was decoded once");
        assert!(cache.segments <= 12);
    }

    /// Late articles cycling over more sealed files than the cache holds
    /// decode no more than the budget in one save.
    #[test]
    fn cycling_sealed_files_decodes_no_more_than_the_budget() {
        let conn = sealed_files(5, 4);
        let mut cache = SealedCache::with_limits(16, 40);
        for _ in 0..3 {
            for id in 1..=5 {
                cache.contains(&conn, id, b"late", id).unwrap();
            }
        }
        assert!(cache.decoded <= 40, "{} segments decoded", cache.decoded);
        assert!(cache.segments <= 16);
    }

    #[test]
    fn a_sealed_blob_past_the_read_cap_is_an_error_not_a_decode() {
        let conn = sealed_files(1, READ_MAX_SEGMENTS + 1);
        let mut cache = SealedCache::new();
        assert!(cache.contains(&conn, 1, b"1-1", 1).is_err());
        assert!(cache.files.is_empty());
    }

    /// Older builds left blobs past `SEAL_MAX_SEGMENTS`. They have to stay readable, or
    /// no save or NZB touching one can work, and a late article never joins its release.
    #[test]
    fn a_blob_past_the_seal_cap_still_takes_late_articles() {
        let (_dir, main) = sealed_fixture();
        let conn = db::open_shard(&shard_path(&main, shard_of("alt.binaries.t"))).unwrap();
        let (file, release_id): (i64, i64) = conn
            .query_row("select id, release_id from files where filename = 'a.rar'", [], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        let oversized: Vec<crate::blob::Seg> = (0..SEAL_MAX_SEGMENTS + 5)
            .map(|p| crate::blob::Seg {
                part: Some(p + 10),
                bytes: 1,
                domain: 0,
                local: format!("old{p}").into_bytes(),
            })
            .collect();
        conn.execute("update files set blob = ? where id = ?", params![crate::blob::encode(&oversized), file]).unwrap();
        conn.execute("delete from segments where file_id = ?", [file]).unwrap();

        let late = Article {
            message_id: "<late@x>".into(),
            subject: "\"a.rar\" yEnc (9/9)".into(),
            filename: Some("a.rar".into()),
            part: Some(9),
            total_parts: Some(9),
            bytes: 7,
            ..Default::default()
        };
        let release =
            Release { name: "Rel".into(), group: "alt.binaries.t".into(), articles: vec![late], ..Default::default() };
        save(&main, &[release]).unwrap();

        let rows: i64 =
            conn.query_row("select count(*) from segments where file_id = ?", [file], |r| r.get(0)).unwrap();
        assert_eq!(rows, 1, "the late article stays a row: the blob isn't growing");
        let read = articles(&db::open_with_shards(&main).unwrap(), release_id).unwrap();
        assert_eq!(
            read.iter().filter(|a| a.filename.as_deref() == Some("a.rar")).count() as i64,
            SEAL_MAX_SEGMENTS + 6
        );
        assert_eq!(seal_file(&conn, file).unwrap(), 0, "it doesn't seal bigger");
    }

    #[test]
    fn the_database_size_is_the_main_database_and_every_shard() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        assert_eq!(database_bytes(&main), 0, "nothing yet");
        std::fs::write(&main, [0; 10]).unwrap();
        for (i, shard) in shard_paths(&main).iter().enumerate() {
            std::fs::write(shard, vec![0; 100 * (i + 1)]).unwrap();
        }
        std::fs::write(format!("{}-wal", main.display()), [0; 7]).unwrap();
        std::fs::write(format!("{}-wal", shard_path(&main, 0).display()), [0; 3]).unwrap();
        let shards: usize = (1..=SHARDS).map(|i| 100 * i).sum();
        assert_eq!(database_bytes(&main), 10 + shards as u64);
        assert_eq!(wal_bytes(&main), 10);
    }

    #[test]
    fn message_ids_come_back_exactly() {
        let ids = [
            "<1064b678f3f54e28a5afd48a3a986076@ngPost>",
            "<DR59tDkIGMDKQS1YflogRq2MTqVgoHslO@aJQEZYp->",
            "<nnd$009a5634$43be2fd7@264e870e7f90d1fc>",
            "<ABCDEF0123@x>",
            "<abc@def@ghi>",
            "no-brackets@x",
            "<no-at-sign>",
            "<@empty-local>",
            "<odd123@x>",
            "",
        ];
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("atlas.s0.db");
        create_shard(&path).unwrap();
        let conn = db::open_at(&path).unwrap();
        let mut domains = Domains::default();
        let suffix = |d: i64| -> Option<String> {
            conn.query_row("select suffix from domains where id = ?", [d], |r| r.get(0)).ok()
        };
        for id in ids {
            let (local, domain) = domains.encode(&conn, id).unwrap();
            assert_eq!(decode(&local, suffix(domain).as_deref()), id);
            // a second writer with an empty cache finds the same domain
            assert_eq!(Domains::default().encode(&conn, id).unwrap().1, domain);
        }
        assert_eq!(pack_local("1064b678f3f54e28a5afd48a3a986076").len(), 17, "hex is stored at half the size");
    }

    #[test]
    fn locals_pack_by_shape_and_come_back_exactly() {
        let cases = [
            ("1064b678f3f54e28a5afd48a3a986076", 1u8), // ngPost hex
            ("ABCDEF0123", 2),
            ("0001234", 3), // digits (odd length, so not hex), leading zeros kept
            ("0abc9xyz", 4),
            ("0ABC9XYZ", 5),
            ("hotfTpetaZRIbOYuTuQ31", 6), // JBinUp
            ("DR59tDkIGMDKQS1YflogRq2MTqVgoHslO", 6),
            ("ZjPsQyLgOjNtQvXbHeKaXyCm1730295651235", 6), // Nyuu: letters + ms timestamp
            ("a-b_c-9", 7),
            ("abc", 4),                   // odd length hex-looking goes base36
            ("nnd$009a5634$43be2fd7", 0), // anything else stays text
            ("", 0),
        ];
        for (local, tag) in cases {
            let packed = pack_local(local);
            assert_eq!(packed[0], tag, "{local}");
            assert_eq!(unpack_local(&packed), local, "{local}");
            assert_eq!(pack_local(local), packed, "deterministic: {local}");
        }
        let long = "z".repeat(300); // not "a": that would be hex
        assert_eq!(pack_local(&long)[0], 0, "longer than 255 stays text");
        assert_eq!(pack_local("ZjPsQyLgOjNtQvXbHeKaXyCm1730295651235").len(), 30, "base62 is smaller than text (38)");
    }

    /// The packing as it was first written (one digit at a time, a byte at a time), kept to check the faster one
    /// against: what is stored must not change.
    mod first_packing {
        use super::super::{BASE64_URL, DIGITS, HEX_LOWER, HEX_UPPER, TEXT, alphabet};

        fn to_bytes(digits: &[u8], n: u32) -> Vec<u8> {
            let mut out: Vec<u8> = Vec::new();
            for &d in digits {
                let mut carry = u32::from(d);
                for b in out.iter_mut() {
                    let v = u32::from(*b) * n + carry;
                    *b = (v & 0xff) as u8;
                    carry = v >> 8;
                }
                while carry > 0 {
                    out.push((carry & 0xff) as u8);
                    carry >>= 8;
                }
            }
            out.reverse();
            out
        }

        fn from_bytes(bytes: &[u8], n: u32, len: usize) -> Vec<u8> {
            let mut num: Vec<u8> = bytes.to_vec();
            let mut digits = Vec::with_capacity(len);
            for _ in 0..len {
                let mut rem = 0u32;
                for b in num.iter_mut() {
                    let v = (rem << 8) | u32::from(*b);
                    *b = (v / n) as u8;
                    rem = v % n;
                }
                digits.push(rem as u8);
            }
            digits.reverse();
            digits
        }

        pub fn pack(local: &str) -> Vec<u8> {
            let bytes = local.as_bytes();
            let even = !bytes.is_empty() && bytes.len().is_multiple_of(2);
            let lower = even && bytes.iter().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b));
            let upper = even && !lower && bytes.iter().all(|b| b.is_ascii_digit() || (b'A'..=b'F').contains(b));
            if lower || upper {
                let nibble = |c: u8| (c as char).to_digit(16).unwrap_or(0) as u8;
                let mut out = vec![if lower { HEX_LOWER } else { HEX_UPPER }];
                out.extend(bytes.chunks(2).map(|p| (nibble(p[0]) << 4) | nibble(p[1])));
                return out;
            }
            if (1..=255).contains(&bytes.len()) {
                for tag in DIGITS..=BASE64_URL {
                    let abc = alphabet(tag);
                    let values: Option<Vec<u8>> =
                        bytes.iter().map(|b| abc.iter().position(|a| a == b).map(|p| p as u8)).collect();
                    if let Some(values) = values {
                        let mut out = vec![tag, bytes.len() as u8];
                        out.extend(to_bytes(&values, abc.len() as u32));
                        return out;
                    }
                }
            }
            [&[TEXT][..], bytes].concat()
        }

        pub fn unpack(blob: &[u8]) -> String {
            match blob.first() {
                Some(&HEX_LOWER) => blob[1..].iter().map(|b| format!("{b:02x}")).collect(),
                Some(&HEX_UPPER) => blob[1..].iter().map(|b| format!("{b:02X}")).collect(),
                Some(&tag) if (DIGITS..=BASE64_URL).contains(&tag) && blob.len() >= 2 => {
                    let abc = alphabet(tag);
                    from_bytes(&blob[2..], abc.len() as u32, blob[1] as usize)
                        .iter()
                        .map(|&d| abc[d as usize] as char)
                        .collect()
                }
                _ => String::from_utf8_lossy(blob.get(1..).unwrap_or_default()).into_owned(),
            }
        }
    }

    #[test]
    fn packing_is_byte_for_byte_what_it_always_was() {
        // a small deterministic generator: ids of every alphabet and length, and the odd character out
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let pools: [&[u8]; 8] = [
            b"0123456789",
            b"0123456789abcdef",
            b"0123456789ABCDEF",
            b"0123456789abcdefghijklmnopqrstuvwxyz",
            b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ",
            b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz",
            b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz-_",
            b"0123456789abcXYZ-_.$%+=",
        ];
        let mut cases: Vec<String> = vec![String::new(), "0".into(), "00".into(), "000".into(), "0000000a".into()];
        for len in [1usize, 2, 3, 4, 5, 7, 8, 9, 15, 16, 17, 31, 32, 33, 40, 64, 100, 254, 255, 256, 300] {
            for pool in pools {
                for _ in 0..20 {
                    cases.push((0..len).map(|_| pool[(next() % pool.len() as u64) as usize] as char).collect());
                }
                // leading zero digits
                cases.push(format!(
                    "00{}",
                    (0..len).map(|_| pool[(next() % pool.len() as u64) as usize] as char).collect::<String>()
                ));
            }
        }
        for local in &cases {
            let packed = pack_local(local);
            assert_eq!(packed, first_packing::pack(local), "packs {local:?} as before");
            assert_eq!(unpack_local(&packed), *local, "round trip of {local:?}");
            assert_eq!(unpack_local(&packed), first_packing::unpack(&packed));
        }
        // blobs no packing makes (a stored length that doesnt match the number) still decode as before
        for tag in DIGITS..=BASE64_URL {
            for blob in [vec![tag, 0], vec![tag, 3, 0xff, 0xff, 0xff, 0xff], vec![tag, 40, 1, 2], vec![tag, 1, 0, 0, 7]]
            {
                assert_eq!(unpack_local(&blob), first_packing::unpack(&blob), "{blob:?}");
            }
        }
    }

    #[test]
    fn old_packings_still_decode_and_repack_to_the_current_one() {
        // text and hex from before tags 3-7 keep their meaning
        assert_eq!(unpack_local(&[TEXT, b'h', b'i']), "hi");
        assert_eq!(unpack_local(&[HEX_LOWER, 0xab, 0x01]), "ab01");
        assert_eq!(unpack_local(&[HEX_UPPER, 0xab, 0x01]), "AB01");
        let old_text = [&[TEXT][..], b"hotfTpetaZRIbOYuTuQ31"].concat();
        let current = pack_local("hotfTpetaZRIbOYuTuQ31");
        assert_eq!(repack(&old_text), current);
        assert_eq!(repack(&current), current, "repacking is stable");
        assert_eq!(repack(&[HEX_LOWER, 0xab]), vec![HEX_LOWER, 0xab]);
        // the longest ids still round-trip
        let max = "z".repeat(255);
        assert_eq!(unpack_local(&pack_local(&max)), max);
        let max64 = format!("{}-", "Z".repeat(254));
        assert_eq!(unpack_local(&pack_local(&max64)), max64);
    }

    #[test]
    fn only_shared_domains_get_rows_and_nothing_is_saved_twice() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("atlas.s0.db");
        create_shard(&path).unwrap();
        let conn = db::open_at(&path).unwrap();
        let rows = || -> i64 { conn.query_row("select count(*) from domains", [], |r| r.get(0)).unwrap() };
        let mut domains = Domains::default();

        // made up per article: never a row
        for n in 0..10 {
            assert_eq!(domains.encode(&conn, &format!("<abc{n}@Rand{n}>")).unwrap().1, 0);
        }
        assert_eq!(rows(), 0);

        // shared: a row from the third one on, packed after that
        let ids: Vec<i64> = (0..5).map(|n| domains.encode(&conn, &format!("<{n:032x}@ngPost>")).unwrap().1).collect();
        assert_eq!(&ids[..2], &[0, 0]);
        assert!(ids[2..].iter().all(|d| *d == ids[2] && *d > 0));
        assert_eq!(rows(), 1);

        // an article stored whole before its domain got a row isnt saved again packed
        let file = file(&conn, 8, "a.rar").unwrap();
        let article = Article { message_id: format!("<{:032x}@nyuu>", 7), part: Some(1), ..Default::default() };
        let mut fresh = Domains::default();
        assert!(
            add_segment(&conn, &mut fresh, &file, None, &article, &mut SealedCache::new()).unwrap(),
            "first time: stored whole"
        );
        for n in 0..SHARED_AFTER {
            fresh.encode(&conn, &format!("<{n:032x}@nyuu>")).unwrap();
        }
        assert!(conn.prepare("select 1 from domains where suffix = '@nyuu>'").unwrap().exists([]).unwrap());
        assert!(
            !add_segment(&conn, &mut fresh, &file, None, &article, &mut SealedCache::new()).unwrap(),
            "same article again: not saved twice"
        );
        let segments: i64 = conn.query_row("select count(*) from segments", [], |r| r.get(0)).unwrap();
        assert_eq!(segments, 1);

        // the same for a row saved as text before locals were packed by alphabet
        let legacy = Article {
            message_id: "<hotfTpetaZRIbOYuTuQ31@JBinUp.local>".to_string(),
            part: Some(1),
            ..Default::default()
        };
        let (_, domain) = {
            let mut shared = Domains::default();
            let mut last = (Vec::new(), 0);
            for n in 0..SHARED_AFTER {
                last = shared.encode(&conn, &format!("<{n:032x}@JBinUp.local>")).unwrap();
            }
            last
        };
        assert!(domain > 0);
        let mut stored = vec![0u8];
        stored.extend_from_slice(b"hotfTpetaZRIbOYuTuQ31");
        conn.execute(
            "insert into segments (file_id, local, domain, part, bytes) values (?, ?, ?, 1, 0)",
            params![file.id, stored, domain],
        )
        .unwrap();
        assert!(
            !add_segment(&conn, &mut Domains::default(), &file, None, &legacy, &mut SealedCache::new()).unwrap(),
            "legacy text row found"
        );
        let segments: i64 = conn.query_row("select count(*) from segments", [], |r| r.get(0)).unwrap();
        assert_eq!(segments, 2);
    }

    #[test]
    fn a_file_this_save_made_is_fresh_till_something_is_stored_whole_in_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("atlas.s0.db");
        create_shard(&path).unwrap();
        let conn = db::open_at(&path).unwrap();
        let made = file(&conn, 9, "b.rar").unwrap();
        assert!(made.fresh.get(), "just made: nothing in it but what this save adds");
        assert!(!file(&conn, 9, "b.rar").unwrap().fresh.get(), "looked up: it may hold anything");
        let mut domains = Domains::default();
        // a shared domain's article is packed and leaves the file fresh
        for n in 0..SHARED_AFTER {
            domains.encode(&conn, &format!("<{n:032x}@ngPost>")).unwrap();
        }
        let packed = Article { message_id: format!("<{:032x}@ngPost>", 99), part: Some(1), ..Default::default() };
        assert!(add_segment(&conn, &mut domains, &made, None, &packed, &mut SealedCache::new()).unwrap());
        assert!(made.fresh.get());
        assert!(!add_segment(&conn, &mut domains, &made, None, &packed, &mut SealedCache::new()).unwrap(), "twice");
        // one stored whole makes it look again
        let whole = Article { message_id: "<abc@Rand1>".into(), part: Some(2), ..Default::default() };
        assert!(add_segment(&conn, &mut domains, &made, None, &whole, &mut SealedCache::new()).unwrap());
        assert!(!made.fresh.get());
        let segments: i64 = conn.query_row("select count(*) from segments", [], |r| r.get(0)).unwrap();
        assert_eq!(segments, 2);
    }

    #[test]
    fn a_writer_keeps_the_whole_domain_table_and_looks_nothing_up() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("atlas.s0.db");
        create_shard(&path).unwrap();
        let conn = db::open_at(&path).unwrap();
        conn.execute_batch("insert into domains (id, suffix) values (7, '@x>'), (9, '@y>')").unwrap();
        let mut domains = Domains::default();
        assert_eq!(domains.encode(&conn, "<a1@x>").unwrap().1, 7);
        assert!(domains.complete && domains.ids.len() == 2, "every row read at once");
        assert_eq!(domains.encode(&conn, "<a1@y>").unwrap().1, 9);
        // one not in the table gets its row from the third article on, as before
        let got: Vec<i64> = (0..4).map(|n| domains.encode(&conn, &format!("<{n}@z>")).unwrap().1).collect();
        assert_eq!(&got[..2], &[0, 0]);
        assert!(got[2] > 9 && got[2] < 1 << 32 && got[3] == got[2]);
        domains.committed();
        assert_eq!(domains.ids.get("@z>"), Some(&got[2]));
        // a rolled back row is forgotten, and made again
        let mut again = Domains::default();
        let tx = conn.unchecked_transaction().unwrap();
        let rolled: Vec<i64> = (0..3).map(|n| again.encode(&tx, &format!("<{n}@w>")).unwrap().1).collect();
        assert!(rolled[2] > 0);
        tx.rollback().unwrap();
        again.rolled_back();
        let redone: Vec<i64> = (0..3).map(|n| again.encode(&conn, &format!("<{n}@w>")).unwrap().1).collect();
        assert!(redone[2] > 0);
        let rows: i64 = conn.query_row("select count(*) from domains where suffix = '@w>'", [], |r| r.get(0)).unwrap();
        assert_eq!(rows, 1);
    }

    #[test]
    fn ids_say_their_shard_and_keep_order() {
        let ids: Vec<i64> = (0..SHARDS).map(|s| global_id(10, s)).collect();
        for (s, id) in ids.iter().enumerate() {
            assert_eq!(shard_of_id(*id), s);
        }
        assert!(global_id(11, 0) > global_id(10, SHARDS - 1), "a later seq sorts after, whatever the shard");
        assert_eq!(shard_path(Path::new("/x/atlas.db"), 3), PathBuf::from("/x/atlas.s3.db"));
    }

    /// a shard with one release: file a.rar (3 parts) and b.rar (2 parts)
    fn sealed_fixture() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        db::create_db_at(&main).unwrap();
        let art = |file: &str, part: i64, total: i64, id: &str| Article {
            message_id: id.into(),
            subject: format!("\"{file}\" yEnc ({part}/{total})"),
            filename: Some(file.into()),
            part: Some(part),
            total_parts: Some(total),
            bytes: 100 + part,
            ..Default::default()
        };
        let release = Release {
            name: "Rel".into(),
            group: "alt.binaries.t".into(),
            articles: vec![
                art("a.rar", 1, 3, "<a1@x>"),
                art("a.rar", 2, 3, "<0abc12def34@ngPost>"),
                art("a.rar", 3, 3, "<a3@x>"),
                art("b.rar", 1, 2, "<b1@x>"),
            ],
            ..Default::default()
        };
        save(&main, &[release]).unwrap();
        (dir, main)
    }

    /// A part saved again under another message-id is counted like any article: it isnt known to be a repost, as
    /// different files can share a file row. `article_totals` comes to what the saves counted, sealed or not.
    #[test]
    fn a_part_saved_again_is_counted_and_totals_match() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        db::create_db_at(&main).unwrap();
        let art = |part: i64, copy: u8| Article {
            message_id: format!("<p{part}c{copy}@x>"),
            subject: format!("\"a.rar\" yEnc ({part}/2)"),
            filename: Some("a.rar".into()),
            part: Some(part),
            total_parts: Some(2),
            bytes: 1_000 + part,
            ..Default::default()
        };
        let rel =
            |articles| Release { name: "Rel".into(), group: "alt.binaries.t".into(), articles, ..Default::default() };
        save(&main, &[rel(vec![art(1, 0), art(1, 1), art(2, 0)])]).unwrap();
        let c = db::open_at(&shard_path(&main, shard_of("alt.binaries.t"))).unwrap();
        let totals = || -> (i64, i64, i64) {
            c.query_row("select id, size, parts from releases where name = 'Rel'", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .unwrap()
        };
        let (id, size, parts) = totals();
        assert_eq!((size, parts), (3_004, 3));
        assert_eq!(article_totals(&c, id).unwrap(), Some((3_004, 3)));
        let file: i64 = c.query_row("select id from files where filename = 'a.rar'", [], |r| r.get(0)).unwrap();
        seal_file(&c, file).unwrap();
        save(&main, &[rel(vec![art(2, 1)])]).unwrap();
        assert_eq!(&totals().1, &4_006);
        assert_eq!(article_totals(&c, id).unwrap(), Some((4_006, 4)), "the blob and the late row");
    }

    /// Two uploads of one post in one row become two rows: the first keeps the row, the second gets its own files,
    /// each with its own articles, size, parts and completeness, both queued. Nothing changes when an article has no
    /// upload.
    #[test]
    fn a_release_of_two_uploads_splits_into_two_rows() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        db::create_db_at(&main).unwrap();
        let art = |file: &str, part: i64, upload: u8| Article {
            message_id: format!("<{file}.{part}.u{upload}@x>"),
            subject: format!("\"{file}\" yEnc ({part}/2)"),
            filename: Some(file.into()),
            part: Some(part),
            total_parts: Some(2),
            bytes: 1_000 + part,
            ..Default::default()
        };
        let articles = ["a.rar", "b.rar"]
            .iter()
            .flat_map(|f| [0u8, 1].into_iter().flat_map(move |u| (1..=2).map(move |p| art(f, p, u))))
            .collect();
        save(&main, &[Release { name: "Rel".into(), group: "alt.binaries.t".into(), articles, ..Default::default() }])
            .unwrap();
        let mut conn = db::open_at(&shard_path(&main, shard_of("alt.binaries.t"))).unwrap();
        let id: i64 = conn.query_row("select id from releases", [], |r| r.get(0)).unwrap();
        let a: i64 = conn.query_row("select id from files where filename = 'a.rar'", [], |r| r.get(0)).unwrap();
        seal_file(&conn, a).unwrap();
        let upload_of: HashMap<String, usize> = release_articles(&conn, id)
            .unwrap()
            .into_iter()
            .map(|(.., mid)| {
                let u = usize::from(mid.contains(".u1@"));
                (mid, u)
            })
            .collect();
        assert_eq!(upload_of.len(), 8);
        let releases = |c: &Connection| -> i64 {
            c.query_row("select value from meta where key = 'releases'", [], |r| r.get(0)).unwrap()
        };
        let before = releases(&conn);

        let mut short = upload_of.clone();
        short.retain(|k, _| !k.starts_with("b.rar.2.u1"));
        let tx = conn.transaction().unwrap();
        assert_eq!(
            split_uploads(&tx, &Ids::new(&main), id, &short, &HashMap::new(), 2).unwrap(),
            None,
            "an article with no upload"
        );
        drop(tx);

        let tx = conn.transaction().unwrap();
        let dates: HashMap<String, i64> =
            upload_of.iter().map(|(mid, u)| (mid.clone(), 1_000 + *u as i64 * 3_600)).collect();
        let new = split_uploads(&tx, &Ids::new(&main), id, &upload_of, &dates, 2).unwrap().unwrap();
        tx.commit().unwrap();
        assert_eq!(new.len(), 1);
        let row = |rid: i64| -> (String, i64, i64, bool) {
            conn.query_row("select name, size, parts, complete from releases where id = ?", [rid], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })
            .unwrap()
        };
        assert_eq!(row(id), ("Rel".into(), 4_006, 4, true));
        let (twin, size, parts, complete) = row(new[0]);
        assert!(twin.starts_with("Rel ["), "{twin}");
        assert_eq!((size, parts, complete), (4_006, 4, true));
        let mids = |rid: i64| -> Vec<String> {
            let mut v: Vec<String> = release_articles(&conn, rid).unwrap().into_iter().map(|a| a.4).collect();
            v.sort();
            v
        };
        assert!(mids(id).iter().all(|m| m.contains(".u0@")) && mids(id).len() == 4);
        assert!(mids(new[0]).iter().all(|m| m.contains(".u1@")) && mids(new[0]).len() == 4);
        assert_eq!(releases(&conn), before + 1);
        // each upload's files: their NZB entry's article and their timing are their own
        let facts = |rid: i64| -> Vec<(String, Option<i64>, Option<i64>)> {
            conn.prepare(
                "select f.subject_mid, t.first_date, t.last_date from files f left join file_timing t on t.file_id = f.id
                 where f.release_id = ? order by f.filename",
            )
            .unwrap()
            .query_map([rid], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
        };
        let mine = facts(id);
        assert_eq!(mine.first().map(|f| f.0.as_str()), Some("<a.rar.1.u0@x>"));
        let theirs = facts(new[0]);
        assert_eq!(theirs.first().cloned(), Some(("<a.rar.1.u1@x>".into(), Some(4_600), Some(4_600))));
        assert!(theirs.iter().all(|f| f.0.contains(".u1@")));
        let queued: i64 = conn
            .query_row("select count(*) from pp_queue where release_id in (?, ?)", [id, new[0]], |r| r.get(0))
            .unwrap();
        assert_eq!(queued, 2);

        // later saves: an article dated in (or just past) the second upload's window goes to its row, one in the
        // first's or far from both stays in the first row
        let routes: i64 =
            conn.query_row("select count(*) from upload_routes where origin_id = ?", [id], |r| r.get(0)).unwrap();
        assert_eq!(routes, 2);
        let late = |mid: &str, file: &str, at: i64| Article {
            message_id: format!("<{mid}@x>"),
            date: chrono::DateTime::from_timestamp(at, 0).unwrap().format("%a, %d %b %Y %H:%M:%S +0000").to_string(),
            ..art(file, 2, 0)
        };
        let articles = vec![
            late("b.rar.2.reread", "b.rar", 4_600 + 60),
            late("a.rar.2.first", "a.rar", 1_000),
            late("a.rar.2.far", "a.rar", 100_000),
            // past the slack after the last upload: maybe another upload starting, not this one's
            late("b.rar.2.after", "b.rar", 4_600 + ROUTE_SLACK_SECS + 1),
        ];
        save(&main, &[Release { name: "Rel".into(), group: "alt.binaries.t".into(), articles, ..Default::default() }])
            .unwrap();
        assert!(mids(new[0]).contains(&"b.rar.2.reread@x".to_string()), "{:?}", mids(new[0]));
        assert!(mids(id).contains(&"a.rar.2.first@x".to_string()) && mids(id).contains(&"a.rar.2.far@x".to_string()));
        assert!(mids(id).contains(&"b.rar.2.after@x".to_string()));
        assert_eq!((row(id).2, row(new[0]).2), (7, 5));
        // a later upload's row renamed keeps its route; one gone is routed to no more
        conn.execute("update releases set name = 'Rel renamed' where id = ?", [new[0]]).unwrap();
        save(
            &main,
            &[Release {
                name: "Rel".into(),
                group: "alt.binaries.t".into(),
                articles: vec![late("a.rar.2.renamed", "a.rar", 4_600)],
                ..Default::default()
            }],
        )
        .unwrap();
        assert!(mids(new[0]).contains(&"a.rar.2.renamed@x".to_string()));
        let n: i64 = conn.query_row("select count(*) from releases", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 2, "no fragment row under the old name");
        conn.execute("delete from releases where id = ?", [new[0]]).unwrap();
        save(
            &main,
            &[Release {
                name: "Rel".into(),
                group: "alt.binaries.t".into(),
                articles: vec![late("a.rar.2.gone", "a.rar", 4_600)],
                ..Default::default()
            }],
        )
        .unwrap();
        assert!(mids(id).contains(&"a.rar.2.gone@x".to_string()));
        let n: i64 = conn.query_row("select count(*) from releases", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1, "a gone row isnt made again");
    }
    #[test]
    fn a_prepared_save_keeps_what_an_unprepared_one_does() {
        let art = |id: &str, part: i64, file: &str, date: &str| Article {
            message_id: id.into(),
            subject: format!("\"{file}\" yEnc ({part}/3)"),
            author: "p <p@x>".into(),
            date: date.into(),
            number: part as u64 + 100,
            lines: 10,
            filename: Some(file.into()),
            part: Some(part),
            total_parts: Some(3),
            bytes: 100,
            ..Default::default()
        };
        let releases = vec![
            Release {
                name: "Rel".into(),
                group: "alt.binaries.t".into(),
                articles: vec![
                    art("<1730295651235.a1b2c3@nyuu>", 1, "a.rar", "Mon, 04 Nov 2024 10:00:00 +0000"),
                    art("<1730295651299.a1b2c4@nyuu>", 2, "a.rar", "Mon, 04 Nov 2024 10:00:05 +0000"),
                    art("<1730295652000.a1b2c5@nyuu>", 1, "a.par2", "2024-11-04 10:00:09"),
                ],
                ..Default::default()
            },
            Release {
                name: "Prog".into(),
                group: "alt.binaries.t".into(),
                articles: vec![art("<x1@y>", 1, "setup.exe", "Mon, 04 Nov 2024 10:00:00 +0000")],
                ..Default::default()
            },
        ];
        let saved = |prepared: bool| {
            let dir = tempfile::tempdir().unwrap();
            let main = dir.path().join("atlas.db");
            db::create_db_at(&main).unwrap();
            let shard = shard_of("alt.binaries.t");
            let mut conn = db::open_shard(&shard_path(&main, shard)).unwrap();
            let ready = if prepared { prepare(&releases) } else { Prepared::new() };
            assert_eq!(ready.len(), if prepared { 2 } else { 0 });
            ShardWriter::new(shard).save_prepared(&mut conn, &Ids::new(&main), [releases.as_slice()], &ready).unwrap();
            let rows: Vec<String> = conn
                .prepare(
                    "select r.name, f.filename, t.first_date, t.last_date, t.tool, t.clock_first, t.clock_last,
                            t.articles from file_timing t join files f on f.id = t.file_id
                     join releases r on r.id = t.release_id order by f.filename",
                )
                .unwrap()
                .query_map([], |r| {
                    Ok(format!(
                        "{} {} {:?} {:?} {:?} {:?} {:?} {}",
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, Option<i64>>(2)?,
                        r.get::<_, Option<i64>>(3)?,
                        r.get::<_, Option<i64>>(4)?,
                        r.get::<_, Option<i64>>(5)?,
                        r.get::<_, Option<i64>>(6)?,
                        r.get::<_, i64>(7)?
                    ))
                })
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap();
            rows
        };
        let plain = saved(false);
        assert_eq!(plain.len(), 2, "both files of Rel, and nothing of the program: {plain:?}");
        assert!(plain.iter().all(|r| r.starts_with("Rel ") && !r.contains(" None None None None ")), "{plain:?}");
        assert_eq!(saved(true), plain);
        // a release that isnt the one prepared (a piece the save cut) finds nothing, and works it out itself
        let ready = prepare(&releases);
        let piece = Release { articles: releases[0].articles[..1].to_vec(), ..releases[0].clone() };
        assert!(prepared_for(&ready, &piece).is_none());
        assert!(prepared_for(&ready, &releases[0]).is_some());
    }

    #[test]
    fn file_timing_folds_each_save() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        db::create_db_at(&main).unwrap();
        let art = |id: &str, part: i64, number: u64, date: &str, from: &str, lines: i64| Article {
            message_id: id.into(),
            subject: format!("\"a.rar\" yEnc ({part}/4)"),
            author: from.into(),
            date: date.into(),
            number,
            lines,
            filename: Some("a.rar".into()),
            part: Some(part),
            total_parts: Some(4),
            bytes: 100,
            ..Default::default()
        };
        let release = |articles: Vec<Article>| Release {
            name: "Rel".into(),
            group: "alt.binaries.t".into(),
            articles,
            ..Default::default()
        };
        #[derive(Debug, PartialEq)]
        struct Row {
            dates: (Option<i64>, Option<i64>),
            numbers: (Option<i64>, Option<i64>),
            lines: Option<i64>,
            references: i64,
            xref: Option<i64>,
            from_hash: Option<i64>,
            from_variants: i64,
            encoded: i64,
            late: i64,
            articles: i64,
        }
        let conn = db::open_at(&shard_path(&main, shard_of("alt.binaries.t"))).unwrap();
        let key = IdKey::of(&conn).unwrap();
        let row = || {
            conn.query_row(
                "select first_date, last_date, number_first, number_last, lines, references_seen, xref_groups_max,
                        from_hash, from_variants, encoded_subject, late_articles, articles
                 from file_timing",
                [],
                |r| {
                    Ok(Row {
                        dates: (r.get(0)?, r.get(1)?),
                        numbers: (r.get(2)?, r.get(3)?),
                        lines: r.get(4)?,
                        references: r.get(5)?,
                        xref: r.get(6)?,
                        from_hash: r.get(7)?,
                        from_variants: r.get(8)?,
                        encoded: r.get(9)?,
                        late: r.get(10)?,
                        articles: r.get(11)?,
                    })
                },
            )
            .unwrap()
        };
        // 2026-10-02 10:00:00 by the poster's clock
        let ten = 1_790_935_200;

        // one From, two articles, nothing threaded, no Xref
        save(
            &main,
            &[release(vec![
                art("<a1@x>", 1, 101, "2026-10-02 10:00:00", "A <a@x>", 10),
                art("<a2@x>", 2, 102, "2026-10-02 10:05:00", "A <a@x>", 20),
            ])],
        )
        .unwrap();
        assert_eq!(
            row(),
            Row {
                dates: (Some(ten), Some(ten + 300)),
                numbers: (Some(101), Some(102)),
                lines: Some(30),
                references: 0,
                xref: None,
                from_hash: Some(key.hash("A <a@x>")),
                from_variants: 1,
                encoded: 0,
                late: 0,
                articles: 2,
            }
        );

        // an earlier article from another From, threaded, crossposted, with an encoded word: the extremes
        // move, the counts add, and the From run count grows by one
        let mut from_b = art("<a3@x>", 3, 100, "2026-10-02 09:58:00", "B <b@x>", 5);
        from_b.references = "<r@x>".into();
        from_b.xref_groups = Some(2);
        from_b.subject = "=?utf-8?q?a.rar?= yEnc (3/4)".into();
        save(&main, &[release(vec![from_b])]).unwrap();
        assert_eq!(
            row(),
            Row {
                dates: (Some(ten - 120), Some(ten + 300)),
                numbers: (Some(100), Some(102)),
                lines: Some(35),
                references: 1,
                xref: Some(2),
                from_hash: Some(key.hash("B <b@x>")),
                from_variants: 2,
                encoded: 1,
                late: 0,
                articles: 3,
            }
        );

        // the same From again continues the run; a duplicate article changes nothing
        let a4 = art("<a4@x>", 4, 103, "2026-10-02 10:06:00", "B <b@x>", 0);
        save(&main, &[release(vec![a4.clone()])]).unwrap();
        save(&main, &[release(vec![a4])]).unwrap();
        let after_four = row();
        assert_eq!((after_four.from_variants, after_four.articles, after_four.numbers.1), (2, 4, Some(103)));

        // an article for a sealed file counts as late
        let file_id: i64 = conn.query_row("select id from files", [], |r| r.get(0)).unwrap();
        seal_file(&conn, file_id).unwrap();
        save(&main, &[release(vec![art("<a5@x>", 4, 104, "2026-10-02 10:07:00", "B <b@x>", 7)])]).unwrap();
        let sealed = row();
        assert_eq!((sealed.late, sealed.articles, sealed.lines, sealed.from_variants), (1, 5, Some(42), 2));
    }

    #[test]
    fn file_timing_keeps_what_the_ids_say() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        db::create_db_at(&main).unwrap();
        let art = |id: &str, part: i64| Article {
            message_id: id.into(),
            subject: format!("\"a.rar\" yEnc ({part}/4)"),
            author: "A <a@x>".into(),
            date: "2026-10-02 10:00:00".into(),
            number: 100 + part as u64,
            filename: Some("a.rar".into()),
            part: Some(part),
            total_parts: Some(4),
            bytes: 100,
            ..Default::default()
        };
        let release = |articles: Vec<Article>| Release {
            name: "Rel".into(),
            group: "alt.binaries.t".into(),
            articles,
            ..Default::default()
        };
        let conn = db::open_at(&shard_path(&main, shard_of("alt.binaries.t"))).unwrap();
        type Facts = (Option<i64>, Option<i64>, Option<i64>, Option<String>, Option<String>, Option<i64>, Option<i64>);
        let row = || -> Facts {
            conn.query_row(
                "select tool, clock_first, clock_last, hw_host, hw_key, hw_n_first, hw_n_last from file_timing",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)),
            )
            .unwrap()
        };
        // two Highwinds ids of one session: the server's clock, host, key and counters
        save(
            &main,
            &[release(vec![
                art("<EaPmB.546176$JP7.228032@fx09.am4>", 1),
                art("<EaPmC.546180$JP7.228040@fx09.am4>", 2),
            ])],
        )
        .unwrap();
        let (tool, first, last, host, key, n_first, n_last) = row();
        assert_eq!(tool, None);
        assert_eq!(first, Some(1_503_379_236_000));
        assert!(last >= first);
        assert_eq!(
            (host.as_deref(), key.as_deref(), n_first, n_last),
            (Some("fx09.am4"), Some("JP7"), Some(546_176), Some(546_180))
        );
        // a Nyuu id of the same file: the tool, and its own clock widens the span
        save(&main, &[release(vec![art("<ZjPsQyLgOjNtQvXbHeKaXyCm1730295651235@nyuu>", 3)])]).unwrap();
        let (tool, first, last, host, key, _, _) = row();
        assert_eq!(tool, Some(crate::discovery::ToolType::Nyuu.code()));
        assert_eq!((first, last), (Some(1_503_379_236_000), Some(1_730_295_651_235)));
        assert_eq!((host.as_deref(), key.as_deref()), (Some("fx09.am4"), Some("JP7")), "the session stays");
        // an id that says nothing changes nothing
        save(&main, &[release(vec![art("<a4@x>", 4)])]).unwrap();
        assert_eq!(row().0, Some(1));
    }

    #[test]
    fn sealing_keeps_every_nzb_and_late_articles_merge() {
        let (_dir, main) = sealed_fixture();
        let shard = shard_path(&main, shard_of("alt.binaries.t"));
        let read = || {
            let conn = db::open_with_shards(&main).unwrap();
            let id: i64 = conn.query_row("select id from releases", [], |r| r.get(0)).unwrap();
            articles(&conn, id).unwrap()
        };
        let before = read();

        let conn = db::open_at(&shard).unwrap();
        let ids: Vec<i64> = conn
            .prepare("select id from files")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        for id in &ids {
            seal_file(&conn, *id).unwrap();
        }
        let loose: i64 = conn.query_row("select count(*) from segments", [], |r| r.get(0)).unwrap();
        assert_eq!(loose, 0, "sealed files have no rows left");
        assert_eq!(read(), before, "same articles back from the blobs");
        assert_eq!(
            article_count(&db::open_with_shards(&main).unwrap(), &format!("s{}", shard_of("alt.binaries.t"))).unwrap(),
            4
        );

        // a late article for a sealed file is saved once, one already sealed isnt
        let late = Release {
            name: "Rel".into(),
            group: "alt.binaries.t".into(),
            articles: vec![
                Article {
                    message_id: "<b2@x>".into(),
                    filename: Some("b.rar".into()),
                    part: Some(2),
                    total_parts: Some(2),
                    bytes: 102,
                    ..Default::default()
                },
                Article {
                    message_id: "<a1@x>".into(),
                    filename: Some("a.rar".into()),
                    part: Some(1),
                    total_parts: Some(3),
                    bytes: 101,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        save(&main, &[late]).unwrap();
        let after = read();
        assert_eq!(after.len(), 5);
        assert_eq!(after.iter().filter(|a| a.message_id == "<a1@x>").count(), 1);

        // resealing folds the late row in
        let b: i64 = conn.query_row("select id from files where filename = 'b.rar'", [], |r| r.get(0)).unwrap();
        assert_eq!(seal_file(&conn, b).unwrap(), 2);
        assert_eq!(read(), after);
    }

    /// A row its blob already has (saved past a save's decode budget) is
    /// dropped when the file is sealed again, not kept twice.
    #[test]
    fn resealing_drops_a_row_the_blob_already_has() {
        let (_dir, main) = sealed_fixture();
        let conn = db::open_at(&shard_path(&main, shard_of("alt.binaries.t"))).unwrap();
        let a: i64 = conn.query_row("select id from files where filename = 'a.rar'", [], |r| r.get(0)).unwrap();
        assert_eq!(seal_file(&conn, a).unwrap(), 3);
        let first = sealed_segments(&conn, a).unwrap().unwrap().remove(0);
        conn.execute(
            "insert into segments (file_id, local, domain, part, bytes) values (?, ?, ?, ?, ?)",
            params![a, first.local, first.domain, first.part, first.bytes],
        )
        .unwrap();
        assert_eq!(seal_file(&conn, a).unwrap(), 3, "the copy isnt added");
    }

    /// An article saved whole before its domain was shared, and again packed
    /// after: the same message-id, so sealing drops the row and its counts.
    #[test]
    fn resealing_drops_a_packed_row_whose_blob_has_the_same_id_whole() {
        let (_dir, main) = sealed_fixture();
        let conn = db::open_at(&shard_path(&main, shard_of("alt.binaries.t"))).unwrap();
        let a: i64 = conn.query_row("select id from files where filename = 'a.rar'", [], |r| r.get(0)).unwrap();
        assert_eq!(seal_file(&conn, a).unwrap(), 3);
        let held = sealed_segments(&conn, a).unwrap().unwrap();
        let a1 = held.iter().find(|s| s.local == whole("<a1@x>")).expect("<a1@x> stored whole");
        let d: i64 = conn.query_row("select id from domains where suffix = '@x>'", [], |r| r.get(0)).unwrap();
        let articles_before: i64 =
            conn.query_row("select value from meta where key = 'articles'", [], |r| r.get(0)).unwrap();
        conn.execute(
            "insert into segments (file_id, local, domain, part, bytes) values (?, ?, ?, ?, ?)",
            params![a, pack_local("a1"), d, a1.part, a1.bytes],
        )
        .unwrap();
        add_totals(&conn, 0, 1).unwrap();
        assert_eq!(seal_file(&conn, a).unwrap(), 3, "the packed copy isnt added");
        let loose: i64 = conn.query_row("select count(*) from segments", [], |r| r.get(0)).unwrap();
        assert_eq!(loose, 1, "only the other file's row is left");
        let articles: i64 = conn.query_row("select value from meta where key = 'articles'", [], |r| r.get(0)).unwrap();
        assert_eq!(articles, articles_before);
    }

    /// A release saved again with no articles of its own (its `parts` still
    /// NULL) is the same release: the shard's release total counts it once.
    #[test]
    fn upserting_a_release_with_null_parts_twice_counts_it_once() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        db::create_db_at(&main).unwrap();
        let bare = || Release { name: "Bare".into(), group: "alt.binaries.t".into(), ..Default::default() };
        let releases = || -> i64 {
            let conn = db::open_at(&shard_path(&main, shard_of("alt.binaries.t"))).unwrap();
            conn.query_row("select value from meta where key = 'releases'", [], |r| r.get(0)).unwrap()
        };
        save(&main, &[bare()]).unwrap();
        let once = releases();
        save(&main, &[bare()]).unwrap();
        save(&main, &[bare()]).unwrap();
        assert_eq!(once, 1);
        assert_eq!(releases(), once);
    }

    /// A copy saved as a row counted in its release's size and parts and the
    /// shard's article total: dropping it when the file is sealed again
    /// takes it out of them too, back to what one copy gives.
    #[test]
    fn dropping_a_copy_takes_it_out_of_the_release_and_shard_totals() {
        let (_dir, main) = sealed_fixture();
        let conn = db::open_at(&shard_path(&main, shard_of("alt.binaries.t"))).unwrap();
        let a: i64 = conn.query_row("select id from files where filename = 'a.rar'", [], |r| r.get(0)).unwrap();
        seal_file(&conn, a).unwrap();
        let totals = || -> (i64, i64, i64) {
            conn.query_row(
                "select r.size, r.parts, (select value from meta where key = 'articles')
                 from releases r join files f on f.release_id = r.id where f.id = ?",
                [a],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap()
        };
        let once = totals();

        // the copy as a save past its decode budget puts it in
        let first = sealed_segments(&conn, a).unwrap().unwrap().remove(0);
        conn.execute(
            "insert into segments (file_id, local, domain, part, bytes) values (?, ?, ?, ?, ?)",
            params![a, first.local, first.domain, first.part, first.bytes],
        )
        .unwrap();
        conn.execute(
            "update releases set size = size + ?, parts = parts + 1
             where id = (select release_id from files where id = ?)",
            params![first.bytes, a],
        )
        .unwrap();
        add_totals(&conn, 0, 1).unwrap();
        assert_ne!(totals(), once);

        seal_file(&conn, a).unwrap();
        assert_eq!(totals(), once);
    }

    /// A row a full blob already has is dropped, not left a row because
    /// adding it would go past the cap.
    #[test]
    fn a_row_a_full_blob_already_has_is_dropped() {
        let (_dir, main) = sealed_fixture();
        let conn = db::open_at(&shard_path(&main, shard_of("alt.binaries.t"))).unwrap();
        let a: i64 = conn.query_row("select id from files where filename = 'a.rar'", [], |r| r.get(0)).unwrap();
        seal_file(&conn, a).unwrap();
        let mut segs = sealed_segments(&conn, a).unwrap().unwrap();
        let first = segs[0].clone();
        let pad = (segs.len() as i64..SEAL_MAX_SEGMENTS).map(|p| crate::blob::Seg {
            part: Some(100 + p),
            bytes: 1,
            domain: 0,
            local: format!("pad{p}").into_bytes(),
        });
        segs.extend(pad.collect::<Vec<_>>());
        conn.execute("update files set blob = ? where id = ?", params![crate::blob::encode(&segs), a]).unwrap();
        let insert = |local: &[u8], domain: i64| {
            conn.execute(
                "insert into segments (file_id, local, domain, part, bytes) values (?, ?, ?, ?, ?)",
                params![a, local, domain, first.part, first.bytes],
            )
            .unwrap();
        };
        let rows =
            || conn.query_row("select count(*) from segments where file_id = ?", [a], |r| r.get::<_, i64>(0)).unwrap();

        insert(&first.local, first.domain);
        assert_eq!(seal_file(&conn, a).unwrap(), SEAL_MAX_SEGMENTS as usize, "a full blob, the copy dropped");
        assert_eq!(rows(), 0);

        // with a new row too the blob is past the cap: the new one stays a row, the copy goes
        insert(&first.local, first.domain);
        insert(b"new", 0);
        assert_eq!(seal_file(&conn, a).unwrap(), 0);
        assert_eq!(rows(), 1);
        assert_eq!(sealed_segments(&conn, a).unwrap().unwrap().len() as i64, SEAL_MAX_SEGMENTS);
    }

    #[test]
    fn sealed_nzb_is_byte_for_byte_the_same() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        db::create_db_at(&main).unwrap();
        let art = |file: &str, part: Option<i64>, id: &str, bytes: i64| Article {
            message_id: id.into(),
            subject: format!("\"{file}\" yEnc"),
            filename: Some(file.into()),
            part,
            total_parts: Some(2),
            bytes,
            ..Default::default()
        };
        let release = Release {
            name: "Nzb".into(),
            group: "alt.binaries.t".into(),
            poster: "me <me@x>".into(),
            date: "2026-10-01 12:00:00".into(),
            articles: vec![
                art("a.rar", Some(2), "<00ff00ff@ngPost>", 700_000),
                art("a.rar", Some(1), "<made-up@one-off.domain>", 750_000), // stored whole
                art("a.rar", None, "<no-part@x>", 5),
                art("a.rar", Some(1), "<ABCD1234@ngPost>", 750_000), // a repost of part 1
                art("", Some(1), "<00aa@ngPost>", 9),                // no filename
            ],
            ..Default::default()
        };
        save(&main, &[release]).unwrap();
        let nzb = || {
            let conn = db::open_with_shards(&main).unwrap();
            let id: i64 = conn.query_row("select id from releases", [], |r| r.get(0)).unwrap();
            let r = crate::search::get_release_with(&conn, id).unwrap().unwrap();
            crate::nzb::render_nzb(&r, &articles(&conn, id).unwrap())
        };
        let before = nzb();
        let conn = db::open_at(&shard_path(&main, shard_of("alt.binaries.t"))).unwrap();
        let ids: Vec<i64> = conn
            .prepare("select id from files")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        for id in ids {
            assert!(seal_file(&conn, id).unwrap() > 0);
        }
        assert_eq!(nzb(), before);
    }

    #[test]
    fn a_legacy_text_local_in_a_blob_is_found() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("atlas.s0.db");
        create_shard(&path).unwrap();
        let conn = db::open_at(&path).unwrap();
        let mut domains = Domains::default();
        let mut domain = 0;
        for n in 0..SHARED_AFTER {
            domain = domains.encode(&conn, &format!("<{n:032x}@JBinUp.local>")).unwrap().1;
        }
        assert!(domain > 0);
        let f = file(&conn, 8, "a.rar").unwrap();
        let mut stored = vec![TEXT];
        stored.extend_from_slice(b"hotfTpetaZRIbOYuTuQ31");
        conn.execute(
            "insert into segments (file_id, local, domain, part, bytes) values (?, ?, ?, 1, 0)",
            params![f.id, stored, domain],
        )
        .unwrap();
        assert_eq!(seal_file(&conn, f.id).unwrap(), 1);
        let f = file(&conn, 8, "a.rar").unwrap();
        assert!(f.sealed);
        let article =
            Article { message_id: "<hotfTpetaZRIbOYuTuQ31@JBinUp.local>".into(), part: Some(1), ..Default::default() };
        assert!(!add_segment(&conn, &mut domains, &f, None, &article, &mut SealedCache::new()).unwrap());
        let other = Article { message_id: "<other1@JBinUp.local>".into(), part: Some(2), ..Default::default() };
        assert!(
            add_segment(&conn, &mut domains, &f, None, &other, &mut SealedCache::new()).unwrap(),
            "a new one is a row"
        );
        let loose: i64 = conn.query_row("select count(*) from segments", [], |r| r.get(0)).unwrap();
        assert_eq!(loose, 1);
    }

    #[test]
    fn old_shards_get_the_seal_columns() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("atlas.s0.db");
        {
            let conn = db::create_at(&path).unwrap();
            conn.execute_batch(
                "create table files (id INTEGER PRIMARY KEY, release_id INTEGER NOT NULL, filename TEXT NOT NULL,
                    subject TEXT, subject_part INTEGER, subject_mid TEXT, expected INTEGER, file_total INTEGER,
                    seen BLOB NOT NULL);
                 insert into files (release_id, filename, seen) values (8, 'a.rar', x'02');",
            )
            .unwrap();
        }
        create_shard(&path).unwrap();
        create_shard(&path).unwrap(); // again: nothing to add
        let conn = db::open_at(&path).unwrap();
        let f = file(&conn, 8, "a.rar").unwrap();
        assert!(!f.sealed);
        assert_eq!(f.seen, vec![2]);
        let touched: Option<i64> = conn.query_row("select touched_at from files", [], |r| r.get(0)).unwrap();
        assert_eq!(touched, None, "untouched since the upgrade");
        put_file(&conn, &f).unwrap();
        let touched: Option<i64> = conn.query_row("select touched_at from files", [], |r| r.get(0)).unwrap();
        assert!(touched.is_some());
    }

    #[test]
    fn sealable_walks_the_table_a_step_at_a_time() {
        let (_dir, main) = sealed_fixture();
        let conn = db::open_at(&shard_path(&main, shard_of("alt.binaries.t"))).unwrap();
        let now: i64 = conn.query_row("select unixepoch()", [], |r| r.get(0)).unwrap();
        let ids: Vec<i64> = conn
            .prepare("select id from files order by id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        for id in &ids {
            seal_file(&conn, *id).unwrap();
        }

        // all sealed: nothing to do, but every call moves on by `limit` files
        assert_eq!(sealable(&conn, 0, 1, now).unwrap(), (vec![], ids[0]));
        assert_eq!(sealable(&conn, ids[0], 1, now).unwrap(), (vec![], ids[1]));
        assert_eq!(sealable(&conn, ids[1], 1, now).unwrap(), (vec![], ids[1]), "the end: last == after_id");

        // a late row makes sealed b.rar complete, it still waits until it's stale
        let late = Release {
            name: "Rel".into(),
            group: "alt.binaries.t".into(),
            articles: vec![Article {
                message_id: "<b2@x>".into(),
                filename: Some("b.rar".into()),
                part: Some(2),
                total_parts: Some(2),
                bytes: 102,
                ..Default::default()
            }],
            ..Default::default()
        };
        save(&main, &[late]).unwrap();
        assert!(sealable(&conn, 0, 100, now).unwrap().0.is_empty());
        conn.execute("update files set touched_at = ? where filename = 'b.rar'", [now - SEAL_AGE - 1]).unwrap();
        assert_eq!(sealable(&conn, 0, 100, now).unwrap().0, vec![ids[1]]);

        // a negative part number cant go in a blob: never listed
        conn.execute(
            "insert into segments (file_id, local, domain, part, bytes) values (?, x'00', 0, -1, 0)",
            [ids[1]],
        )
        .unwrap();
        assert!(sealable(&conn, 0, 100, now).unwrap().0.is_empty());
        assert_eq!(seal_file(&conn, ids[1]).unwrap(), 0);
    }

    #[test]
    fn a_file_with_an_oversized_message_id_stays_rows_and_saves_succeed() {
        let (_dir, main) = sealed_fixture();
        let conn = db::open_at(&shard_path(&main, shard_of("alt.binaries.t"))).unwrap();
        let now: i64 = conn.query_row("select unixepoch()", [], |r| r.get(0)).unwrap();
        let b: i64 = conn.query_row("select id from files where filename = 'b.rar'", [], |r| r.get(0)).unwrap();
        conn.execute(
            "insert into segments (file_id, local, domain, part, bytes) values (?, zeroblob(5000), 0, 9, 1)",
            [b],
        )
        .unwrap();
        conn.execute("update files set touched_at = ?", [now - SEAL_AGE - 1]).unwrap();
        assert!(!sealable(&conn, 0, 100, now).unwrap().0.contains(&b));
        assert_eq!(seal_file(&conn, b).unwrap(), 0);
        let blob: Option<Vec<u8>> = conn.query_row("select blob from files where id = ?", [b], |r| r.get(0)).unwrap();
        assert!(blob.is_none() || crate::blob::decode_capped(&blob.unwrap(), SEAL_MAX_SEGMENTS as usize).is_ok());
        let again = Release {
            name: "r".into(),
            group: "alt.binaries.t".into(),
            articles: vec![Article {
                message_id: "<b9@x>".into(),
                filename: Some("b.rar".into()),
                part: Some(10),
                total_parts: Some(3),
                bytes: 5,
                ..Default::default()
            }],
            ..Default::default()
        };
        save(&main, &[again]).unwrap();
    }

    /// `n` more rows under a file, as a poster could add them
    fn add_rows(conn: &Connection, file_id: i64, n: i64) {
        conn.execute(
            "with recursive n(i) as (select 1 union all select i + 1 from n where i < ?2)
             insert into segments (file_id, local, domain, part, bytes)
             select ?1, cast('big' || i as blob), 0, 100 + i, 1 from n",
            params![file_id, n],
        )
        .unwrap();
    }

    #[test]
    fn a_file_with_more_than_the_cap_of_rows_never_seals() {
        let (_dir, main) = sealed_fixture();
        let conn = db::open_at(&shard_path(&main, shard_of("alt.binaries.t"))).unwrap();
        let now: i64 = conn.query_row("select unixepoch()", [], |r| r.get(0)).unwrap();
        let id = |name: &str| -> i64 {
            conn.query_row("select id from files where filename = ?", [name], |r| r.get(0)).unwrap()
        };
        let rows = |id: i64| -> i64 {
            conn.query_row("select count(*) from segments where file_id = ?", [id], |r| r.get(0)).unwrap()
        };
        let (a, b) = (id("a.rar"), id("b.rar"));
        // b.rar is stale (a.rar is complete): both are due, a.rar with the cap + 1 rows is not
        conn.execute("update files set touched_at = ?", [now - SEAL_AGE - 1]).unwrap();
        assert_eq!(sealable(&conn, 0, 100, now).unwrap().0, vec![a, b]);
        add_rows(&conn, a, SEAL_MAX_SEGMENTS + 1 - rows(a));
        assert_eq!(rows(a), SEAL_MAX_SEGMENTS + 1);
        assert_eq!(sealable(&conn, 0, 100, now).unwrap().0, vec![b], "the big one is skipped");
        assert!(!still_due(&conn, a, now).unwrap());
        assert!(still_due(&conn, b, now).unwrap());

        assert_eq!(seal_file(&conn, a).unwrap(), 0);
        assert_eq!(rows(a), SEAL_MAX_SEGMENTS + 1, "still rows");
        let blob: Option<Vec<u8>> = conn.query_row("select blob from files where id = ?", [a], |r| r.get(0)).unwrap();
        assert_eq!(blob, None);

        // exactly the cap seals
        conn.execute("delete from segments where file_id = ? and local = cast('big1' as blob)", [a]).unwrap();
        assert_eq!(rows(a), SEAL_MAX_SEGMENTS);
        assert_eq!(sealable(&conn, 0, 100, now).unwrap().0, vec![a, b]);
        assert_eq!(seal_file(&conn, a).unwrap() as i64, SEAL_MAX_SEGMENTS);
    }

    #[test]
    fn late_rows_that_would_take_a_blob_past_the_cap_stay_rows() {
        let (_dir, main) = sealed_fixture();
        let conn = db::open_at(&shard_path(&main, shard_of("alt.binaries.t"))).unwrap();
        let now: i64 = conn.query_row("select unixepoch()", [], |r| r.get(0)).unwrap();
        let b: i64 = conn.query_row("select id from files where filename = 'b.rar'", [], |r| r.get(0)).unwrap();
        assert_eq!(seal_file(&conn, b).unwrap(), 1);
        add_rows(&conn, b, SEAL_MAX_SEGMENTS);
        conn.execute("update files set touched_at = ?", [now - SEAL_AGE - 1]).unwrap();

        assert_eq!(seal_file(&conn, b).unwrap(), 0);
        let rows: i64 = conn.query_row("select count(*) from segments where file_id = ?", [b], |r| r.get(0)).unwrap();
        assert_eq!(rows, SEAL_MAX_SEGMENTS, "the late rows were left");
        let blob: Vec<u8> = conn.query_row("select blob from files where id = ?", [b], |r| r.get(0)).unwrap();
        assert_eq!(crate::blob::decode(&blob).unwrap().len(), 1, "and the blob is as it was");
    }

    #[test]
    fn articles_reads_inside_a_callers_transaction() {
        let (_dir, main) = sealed_fixture();
        let reader = db::open_with_shards(&main).unwrap();
        let id: i64 = reader.query_row("select id from releases", [], |r| r.get(0)).unwrap();
        let before = articles(&reader, id).unwrap();
        reader.execute_batch("begin").unwrap();
        assert_eq!(articles(&reader, id).unwrap(), before);
        // sealed by another connection meanwhile: the reader's snapshot doesnt change
        let shard = db::open_at(&shard_path(&main, shard_of("alt.binaries.t"))).unwrap();
        let files: Vec<i64> = shard
            .prepare("select id from files")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        for f in files {
            seal_file(&shard, f).unwrap();
        }
        assert_eq!(articles(&reader, id).unwrap(), before);
        reader.execute_batch("commit").unwrap();
        assert_eq!(articles(&reader, id).unwrap(), before, "and the same from the blobs");
        assert!(reader.is_autocommit(), "no transaction left open");
    }

    #[test]
    fn complete_or_stale_files_are_sealable() {
        let (_dir, main) = sealed_fixture();
        let conn = db::open_at(&shard_path(&main, shard_of("alt.binaries.t"))).unwrap();
        // the clock put_file stamped touched_at with
        let now: i64 = conn.query_row("select unixepoch()", [], |r| r.get(0)).unwrap();
        // a.rar is complete (1..=3), b.rar has 1 of 2 and was just touched
        let (ids, _) = sealable(&conn, 0, 100, now).unwrap();
        let name = |id: i64| -> String {
            conn.query_row("select filename from files where id = ?", [id], |r| r.get(0)).unwrap()
        };
        assert_eq!(ids.iter().map(|&i| name(i)).collect::<Vec<_>>(), vec!["a.rar".to_string()]);
        conn.execute("update files set touched_at = ? where filename = 'b.rar'", [now - SEAL_AGE - 1]).unwrap();
        assert_eq!(sealable(&conn, 0, 100, now).unwrap().0.len(), 2, "b.rar went stale");
    }

    #[test]
    fn a_writer_seals_what_is_due_a_tick_at_a_time() {
        let (_dir, main) = sealed_fixture();
        let shard = shard_of("alt.binaries.t");
        let mut conn = db::open_at(&shard_path(&main, shard)).unwrap();
        let mut writer = ShardWriter::new(shard);
        let now = 2_000_000_000;
        conn.execute("update files set touched_at = ?", [now]).unwrap();
        assert_eq!(writer.seal_some(&mut conn, now).unwrap(), 1, "only the complete file");
        conn.execute("update files set touched_at = ?", [now - SEAL_AGE - 1]).unwrap();
        assert_eq!(writer.seal_some(&mut conn, now + 1).unwrap(), 0, "no walk until SEAL_WALK_EVERY is up");
        let later = now + SEAL_WALK_EVERY;
        assert_eq!(writer.seal_some(&mut conn, later).unwrap(), 1, "the stale one, after wrapping around");
        assert_eq!(writer.seal_some(&mut conn, later + SEAL_WALK_EVERY).unwrap(), 0);
    }

    /// the fixture's release, saved by `writer`
    fn save_with(writer: &mut ShardWriter, main: &Path, release: Release) {
        let mut conn = db::open_at(&shard_path(main, writer.shard)).unwrap();
        writer.save(&mut conn, &Ids::new(main), [std::slice::from_ref(&release)]).unwrap();
    }

    #[test]
    fn between_walks_a_writer_seals_the_files_its_saves_completed() {
        let (_dir, main) = sealed_fixture();
        let shard = shard_of("alt.binaries.t");
        let mut conn = db::open_at(&shard_path(&main, shard)).unwrap();
        let mut writer = ShardWriter::new(shard);
        let now: i64 = conn.query_row("select unixepoch()", [], |r| r.get(0)).unwrap();
        // the walk seals a.rar (complete), b.rar has 1 of 2
        assert_eq!(writer.seal_some(&mut conn, now).unwrap(), 1);

        // another writer completes b.rar: not this writer's, and no walk yet
        let b2 = |id: &str| Release {
            name: "Rel".into(),
            group: "alt.binaries.t".into(),
            articles: vec![Article {
                message_id: id.into(),
                filename: Some("b.rar".into()),
                part: Some(2),
                total_parts: Some(2),
                bytes: 102,
                ..Default::default()
            }],
            ..Default::default()
        };
        save(&main, &[b2("<b2@x>")]).unwrap();
        assert_eq!(writer.seal_some(&mut conn, now + 1).unwrap(), 0, "not from this writer's saves");

        // a file this writer's save completes seals on the next tick
        let mut other = ShardWriter::new(shard);
        let c = Release {
            name: "Rel".into(),
            group: "alt.binaries.t".into(),
            articles: vec![Article {
                message_id: "<c1@x>".into(),
                filename: Some("c.rar".into()),
                part: Some(1),
                total_parts: Some(1),
                bytes: 5,
                ..Default::default()
            }],
            ..Default::default()
        };
        save_with(&mut other, &main, c);
        assert_eq!(other.seal_some(&mut conn, now + 1).unwrap(), 2, "c.rar from its save, b.rar from its first walk");
        save_with(&mut writer, &main, b2("<b2-again@x>"));
        let rows: i64 = conn.query_row("select count(*) from segments", [], |r| r.get(0)).unwrap();
        assert_eq!(rows, 1, "b.rar got a row, it's sealed and complete: waits to be stale");
        assert_eq!(writer.seal_some(&mut conn, now + 2).unwrap(), 0);
    }

    /// A save past its decode budget keeps a repost of a sealed article as a
    /// row, not knowing it's one: a part or total it carries that the
    /// original didnt must not make its file falsely complete or incomplete,
    /// neither while it's a row nor once sealing drops it.
    #[test]
    fn a_fallback_copy_with_another_part_or_total_leaves_completeness_as_it_was() {
        let (_dir, main) = sealed_fixture();
        let shard = shard_of("alt.binaries.t");
        let mut conn = db::open_at(&shard_path(&main, shard)).unwrap();
        for id in conn.prepare("select id from files").unwrap().query_map([], |r| r.get(0)).unwrap() {
            seal_file(&conn, id.unwrap()).unwrap();
        }
        let file = |conn: &Connection, name: &str| -> (Option<i64>, Vec<u8>) {
            conn.query_row("select expected, seen from files where filename = ?", [name], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap()
        };
        let complete =
            |conn: &Connection| -> bool { conn.query_row("select complete from releases", [], |r| r.get(0)).unwrap() };
        let (a, b) = (file(&conn, "a.rar"), file(&conn, "b.rar"));
        assert!(!complete(&conn), "b.rar has 1 of 2");

        // reposts of a1 claiming 4 parts, and of b1 claiming to be part 2
        let copy = |id: &str, name: &str, part: i64, total: i64| Article {
            message_id: id.into(),
            subject: format!("\"{name}\" yEnc ({part}/{total})"),
            filename: Some(name.into()),
            part: Some(part),
            total_parts: Some(total),
            bytes: 100 + part,
            ..Default::default()
        };
        // as for a release with one file, no index to tell them by: the save past its budget can only guess
        conn.execute("delete from release_ids where id_hash != 0", []).unwrap();
        let mut writer = ShardWriter::new(shard);
        writer.decode_budget = 0;
        let release = Release {
            name: "Rel".into(),
            group: "alt.binaries.t".into(),
            articles: vec![copy("<a1@x>", "a.rar", 1, 4), copy("<b1@x>", "b.rar", 2, 2)],
            ..Default::default()
        };
        writer.save(&mut conn, &Ids::new(&main), [std::slice::from_ref(&release)]).unwrap();
        let rows: i64 = conn.query_row("select count(*) from segments", [], |r| r.get(0)).unwrap();
        assert_eq!(rows, 2, "both kept as rows, past the budget");
        assert_eq!(file(&conn, "a.rar").0, a.0, "a.rar still takes 3 parts");

        for id in conn.prepare("select id from files").unwrap().query_map([], |r| r.get(0)).unwrap() {
            seal_file(&conn, id.unwrap()).unwrap();
        }
        assert_eq!((file(&conn, "a.rar"), file(&conn, "b.rar")), (a, b), "the copies went, and what they said");
        assert!(!complete(&conn), "b.rar still has 1 of 2");
        let held: i64 = conn.query_row("select count(*) from held_back", [], |r| r.get(0)).unwrap();
        assert_eq!(held, 0, "what the copies said is gone with them");
    }

    /// A genuinely new article a save past its decode budget keeps as a row
    /// has its total, file total and subject held back, not lost: sealing
    /// the file again, once it knows the row isnt a copy, folds them in.
    #[test]
    fn a_new_article_past_the_decode_budget_gets_its_totals_at_sealing() {
        let (_dir, main) = sealed_fixture();
        let shard = shard_of("alt.binaries.t");
        let mut conn = db::open_at(&shard_path(&main, shard)).unwrap();
        for id in conn.prepare("select id from files").unwrap().query_map([], |r| r.get(0)).unwrap() {
            seal_file(&conn, id.unwrap()).unwrap();
        }
        let expected = |conn: &Connection, name: &str| -> Option<i64> {
            conn.query_row("select expected from files where filename = ?", [name], |r| r.get(0)).unwrap()
        };
        let release = |conn: &Connection| -> (bool, Option<i64>) {
            conn.query_row("select complete, file_total from releases", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap()
        };

        // a.rar turns out to have a 4th part, and b.rar gets its 2nd (of 2 files)
        let new = |id: &str, name: &str, part: i64, total: i64| Article {
            message_id: id.into(),
            subject: format!("\"{name}\" yEnc ({part}/{total})"),
            filename: Some(name.into()),
            part: Some(part),
            total_parts: Some(total),
            file_total: Some(2),
            bytes: 100 + part,
            ..Default::default()
        };
        let mut writer = ShardWriter::new(shard);
        writer.decode_budget = 0;
        let rel = Release {
            name: "Rel".into(),
            group: "alt.binaries.t".into(),
            articles: vec![new("<a4@x>", "a.rar", 4, 4), new("<b2@x>", "b.rar", 2, 2)],
            ..Default::default()
        };
        writer.save(&mut conn, &Ids::new(&main), [std::slice::from_ref(&rel)]).unwrap();
        assert_eq!(expected(&conn, "a.rar"), Some(3), "held back while it could be a copy");
        assert_eq!(release(&conn).1, None);

        for id in conn.prepare("select id from files").unwrap().query_map([], |r| r.get(0)).unwrap() {
            seal_file(&conn, id.unwrap()).unwrap();
        }
        assert_eq!(expected(&conn, "a.rar"), Some(4), "not a copy: its total counts");
        assert_eq!(release(&conn), (true, Some(2)), "every file has its parts, and the file total");
        let held: i64 = conn.query_row("select count(*) from held_back", [], |r| r.get(0)).unwrap();
        assert_eq!(held, 0);
    }

    /// A save past its decode budget that reads a release's articles again
    /// (a pass read twice after a restart) tells them from the release's
    /// index, not by guessing: no row, nothing held back, and its parts and
    /// size stay as they were (they doubled till the files sealed again).
    #[test]
    fn a_reread_past_the_decode_budget_is_known_from_the_index() {
        let (_dir, main) = sealed_fixture();
        let shard = shard_of("alt.binaries.t");
        let mut conn = db::open_at(&shard_path(&main, shard)).unwrap();
        for id in conn.prepare("select id from files").unwrap().query_map([], |r| r.get(0)).unwrap() {
            seal_file(&conn, id.unwrap()).unwrap();
        }
        let totals = |conn: &Connection| -> (i64, i64) {
            conn.query_row("select parts, size from releases", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap()
        };
        let before = totals(&conn);
        let again = |id: &str, name: &str, part: i64, total: i64| Article {
            message_id: id.into(),
            subject: format!("\"{name}\" yEnc ({part}/{total})"),
            filename: Some(name.into()),
            part: Some(part),
            total_parts: Some(total),
            bytes: 100 + part,
            ..Default::default()
        };
        let mut writer = ShardWriter::new(shard);
        writer.decode_budget = 0;
        let release = Release {
            name: "Rel".into(),
            group: "alt.binaries.t".into(),
            articles: vec![
                again("<a1@x>", "a.rar", 1, 3),
                again("<0abc12def34@ngPost>", "a.rar", 2, 3),
                again("<a3@x>", "a.rar", 3, 3),
                again("<b1@x>", "b.rar", 1, 2),
            ],
            ..Default::default()
        };
        writer.save(&mut conn, &Ids::new(&main), [std::slice::from_ref(&release)]).unwrap();
        let count = |conn: &Connection, table: &str| -> i64 {
            conn.query_row(&format!("select count(*) from {table}"), [], |r| r.get(0)).unwrap()
        };
        assert_eq!(count(&conn, "segments"), 0, "every one is in a blob already");
        assert_eq!(count(&conn, "held_back"), 0);
        assert_eq!(totals(&conn), before, "parts and size not counted twice");
    }

    /// A file past the most a blob takes never seals, but what a save past
    /// its decode budget held back for it is still settled: a copy of a
    /// sealed article goes with what it said, a new one's totals count.
    #[test]
    fn what_a_file_too_big_to_seal_held_back_is_settled_anyway() {
        let (_dir, main) = sealed_fixture();
        let shard = shard_of("alt.binaries.t");
        let mut conn = db::open_at(&shard_path(&main, shard)).unwrap();
        for id in conn.prepare("select id from files").unwrap().query_map([], |r| r.get(0)).unwrap() {
            seal_file(&conn, id.unwrap()).unwrap();
        }
        let a: i64 = conn.query_row("select id from files where filename = 'a.rar'", [], |r| r.get(0)).unwrap();
        let state = |conn: &Connection| -> (Option<i64>, Vec<u8>) {
            conn.query_row("select expected, seen from files where id = ?", [a], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap()
        };
        let before = state(&conn);
        let article = |id: &str, part: i64, total: i64| Article {
            message_id: id.into(),
            subject: format!("\"a.rar\" yEnc ({part}/{total})"),
            filename: Some("a.rar".into()),
            part: Some(part),
            total_parts: Some(total),
            bytes: 100 + part,
            ..Default::default()
        };
        // as for a release with one file, no index to tell them by: the save past its budget can only guess
        conn.execute("delete from release_ids where id_hash != 0", []).unwrap();
        let mut writer = ShardWriter::new(shard);
        writer.decode_budget = 0;
        // a repost of a1 claiming 5 parts, and a genuinely new 4th of 4
        let rel = Release {
            name: "Rel".into(),
            group: "alt.binaries.t".into(),
            articles: vec![article("<a1@x>", 1, 5), article("<a4@x>", 4, 4)],
            ..Default::default()
        };
        writer.save(&mut conn, &Ids::new(&main), [std::slice::from_ref(&rel)]).unwrap();
        let held =
            |conn: &Connection| -> i64 { conn.query_row("select count(*) from held_back", [], |r| r.get(0)).unwrap() };
        assert_eq!(held(&conn), 2);
        let rows = |conn: &Connection| -> i64 {
            conn.query_row("select count(*) from segments where file_id = ?", [a], |r| r.get(0)).unwrap()
        };
        add_rows(&conn, a, SEAL_MAX_SEGMENTS);
        let articles: i64 = conn.query_row("select value from meta where key = 'articles'", [], |r| r.get(0)).unwrap();

        let now: i64 = conn.query_row("select unixepoch()", [], |r| r.get(0)).unwrap();
        assert!(sealable(&conn, 0, 100, now).unwrap().0.contains(&a), "walked to for what it held back");
        assert_eq!(seal_file(&conn, a).unwrap(), 0, "still too big to seal");
        assert_eq!(held(&conn), 0, "settled");
        assert_eq!(rows(&conn), SEAL_MAX_SEGMENTS + 1, "the copy went, the new one and the padding stay");
        let after: i64 = conn.query_row("select value from meta where key = 'articles'", [], |r| r.get(0)).unwrap();
        assert_eq!(after, articles - 1, "the copy out of the shard's total");
        assert_eq!(state(&conn).0, Some(4), "the new one's total counts, not the copy's");
        assert_ne!(state(&conn).1, before.1);
        assert!(!sealable(&conn, 0, 100, now).unwrap().0.contains(&a), "and it's skipped again");
    }

    /// One message-id under another file name of its release (another
    /// carrier's subject) is the same article: not saved again, neither
    /// beside a row nor beside a sealed blob, and no empty file is left.
    #[test]
    fn an_article_under_another_file_of_its_release_isnt_saved_twice() {
        let (_dir, main) = sealed_fixture();
        let shard = shard_path(&main, shard_of("alt.binaries.t"));
        let state = || {
            let conn = db::open_with_shards(&main).unwrap();
            let id: i64 = conn.query_row("select id from releases", [], |r| r.get(0)).unwrap();
            let totals: (i64, i64, bool) = conn
                .query_row("select size, parts, complete from releases", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .unwrap();
            let shard = db::open_at(&shard).unwrap();
            let files: i64 = shard.query_row("select count(*) from files", [], |r| r.get(0)).unwrap();
            let total: i64 =
                shard.query_row("select value from meta where key = 'articles'", [], |r| r.get(0)).unwrap();
            (articles(&conn, id).unwrap(), totals, files, total)
        };
        let again = |id: &str| Release {
            name: "Rel".into(),
            group: "alt.binaries.t".into(),
            articles: vec![Article {
                message_id: id.into(),
                subject: "\"c.rar\" yEnc (1/3)".into(),
                filename: Some("c.rar".into()),
                part: Some(1),
                total_parts: Some(3),
                bytes: 101,
                ..Default::default()
            }],
            ..Default::default()
        };
        let before = state();
        // a row of a.rar, and one stored packed under its domain
        save(&main, &[again("<a1@x>"), again("<0abc12def34@ngPost>")]).unwrap();
        assert_eq!(state(), before, "beside a row");

        let conn = db::open_at(&shard).unwrap();
        for id in conn.prepare("select id from files").unwrap().query_map([], |r| r.get(0)).unwrap() {
            seal_file(&conn, id.unwrap()).unwrap();
        }
        let before = state();
        save(&main, &[again("<a3@x>"), again("<b1@x>")]).unwrap();
        assert_eq!(state(), before, "beside a blob");
    }

    /// A release the save makes is checked across its files from memory (`release_ids_of`): the same copies are
    /// dropped, and the index ends up as a save of the files one at a time leaves it.
    #[test]
    fn a_release_made_by_the_save_drops_copies_across_its_files() {
        let art = |id: &str, file: &str, part: i64| Article {
            message_id: id.into(),
            subject: format!("\"{file}\" yEnc ({part}/3)"),
            filename: Some(file.into()),
            part: Some(part),
            total_parts: Some(3),
            bytes: 10,
            ..Default::default()
        };
        let all = vec![
            art("<m1@x>", "a.rar", 1),
            art("<m2@x>", "a.rar", 2),
            art("<m2@x>", "b.rar", 1),
            art("<m3@x>", "b.rar", 2),
            art("<m1@x>", "c.rar", 1),
            art("<m4@x>", "c.rar", 2),
        ];
        let rel = |articles: Vec<Article>| Release {
            name: "Rel".into(),
            group: "alt.binaries.t".into(),
            articles,
            ..Default::default()
        };
        let state = |main: &Path| {
            let conn = db::open_at(&shard_path(main, shard_of("alt.binaries.t"))).unwrap();
            let parts: i64 = conn.query_row("select parts from releases", [], |r| r.get(0)).unwrap();
            let index: Vec<(String, i64)> = conn
                .prepare(
                    "select f.filename, count(*) from release_ids i join files f on f.id = i.file_id
                     group by f.filename order by f.filename",
                )
                .unwrap()
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap();
            let segments: i64 = conn.query_row("select count(*) from segments", [], |r| r.get(0)).unwrap();
            (parts, index, segments)
        };
        let fresh = || {
            let dir = tempfile::tempdir().unwrap();
            let main = dir.path().join("atlas.db");
            db::create_db_at(&main).unwrap();
            (dir, main)
        };
        // all in the save that makes the release
        let (_one, at_once) = fresh();
        save(&at_once, &[rel(all.clone())]).unwrap();
        // a file a save, the release there already from the first
        let (_two, one_by_one) = fresh();
        for file in ["a.rar", "b.rar", "c.rar"] {
            save(&one_by_one, &[rel(all.iter().filter(|a| a.filename.as_deref() == Some(file)).cloned().collect())])
                .unwrap();
        }
        let made = state(&at_once);
        assert_eq!(made.0, 4, "m1 to m4 once each");
        assert_eq!(made.2, 4);
        assert_eq!(made, state(&one_by_one));
        // and a later save, of a release already there, still finds them
        save(&at_once, &[rel(vec![art("<m3@x>", "d.rar", 1), art("<m5@x>", "d.rar", 2)])]).unwrap();
        assert_eq!(state(&at_once).0, 5);
    }

    /// Past the decode budget a sealed file of the release is still known to
    /// have the message-id: the release's index says so, without its blob.
    #[test]
    fn a_cross_file_copy_past_the_decode_budget_isnt_saved() {
        let (_dir, main) = sealed_fixture();
        let shard = shard_path(&main, shard_of("alt.binaries.t"));
        let conn = db::open_at(&shard).unwrap();
        for id in conn.prepare("select id from files").unwrap().query_map([], |r| r.get(0)).unwrap() {
            seal_file(&conn, id.unwrap()).unwrap();
        }
        let count =
            || -> i64 { conn.query_row("select value from meta where key = 'articles'", [], |r| r.get(0)).unwrap() };
        let before = count();
        let mut writer = ShardWriter::new(shard_of("alt.binaries.t"));
        writer.decode_budget = 0;
        save_with(&mut writer, &main, one_file_release("c.rar", &["<a3@x>", "<b1@x>"]));
        assert_eq!(count(), before);
        let loose: i64 = conn.query_row("select count(*) from segments", [], |r| r.get(0)).unwrap();
        assert_eq!(loose, 0);
    }

    /// A second file made only of copies goes again, and with it the index it
    /// started: left behind, the sole file's later articles would never be put
    /// in it, and a copy of one of them in a new file would go unnoticed.
    #[test]
    fn a_copies_only_file_leaves_no_stale_index_behind() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        db::create_db_at(&main).unwrap();
        let shard = shard_path(&main, shard_of("alt.binaries.t"));
        let conn = db::open_at(&shard).unwrap();
        let count = || -> i64 { conn.query_row("select count(*) from segments", [], |r| r.get(0)).unwrap() };
        save(&main, &[one_file_release("a.rar", &["<a1@x>", "<a2@x>"])]).unwrap();
        save(&main, &[one_file_release("b.rar", &["<a1@x>"])]).unwrap();
        let files: i64 = conn.query_row("select count(*) from files", [], |r| r.get(0)).unwrap();
        assert_eq!(files, 1, "b.rar was only a copy");
        let indexed: i64 = conn.query_row("select count(*) from release_ids", [], |r| r.get(0)).unwrap();
        assert_eq!(indexed, 0, "and its index went with it");

        save(&main, &[one_file_release("a.rar", &["<a3@x>"])]).unwrap();
        let before = count();
        save(&main, &[one_file_release("b.rar", &["<a3@x>"])]).unwrap();
        assert_eq!(count(), before, "a copy of a later article is still found");
    }

    /// A release saved before the index (an upgraded shard) gets it the
    /// first time it's written to, and copies across its files are found.
    #[test]
    fn a_release_from_before_the_index_gets_it_when_saved_to() {
        let (_dir, main) = sealed_fixture();
        let shard = shard_path(&main, shard_of("alt.binaries.t"));
        let conn = db::open_at(&shard).unwrap();
        conn.execute("delete from release_ids", []).unwrap();
        let id: i64 = conn.query_row("select id from files where filename = 'a.rar'", [], |r| r.get(0)).unwrap();
        seal_file(&conn, id).unwrap();
        save(&main, &[one_file_release("c.rar", &["<a1@x>", "<0abc12def34@ngPost>", "<b1@x>"])]).unwrap();
        let files: i64 = conn.query_row("select count(*) from files", [], |r| r.get(0)).unwrap();
        assert_eq!(files, 2, "no c.rar: all of it was in a.rar and b.rar");
        let indexed: i64 = conn.query_row("select count(*) from release_ids", [], |r| r.get(0)).unwrap();
        assert_eq!(indexed, 5, "4 ids and the release's own row");
    }

    /// A file past the seal cap stays loose rows; indexing its release
    /// hashes them one by one from the cursor and indexes every one.
    #[test]
    fn indexing_a_release_with_a_huge_loose_file_indexes_every_row() {
        let (_dir, main) = sealed_fixture();
        let shard = shard_path(&main, shard_of("alt.binaries.t"));
        let conn = db::open_at(&shard).unwrap();
        conn.execute("delete from release_ids", []).unwrap();
        let id: i64 = conn.query_row("select id from files where filename = 'a.rar'", [], |r| r.get(0)).unwrap();
        let rows = SEAL_MAX_SEGMENTS + 10;
        conn.execute(
            "with recursive n(i) as (select 1 union all select i + 1 from n where i < ?)
             insert or ignore into segments (file_id, local, domain, part, bytes)
             select ?, cast('big' || i as blob), 0, i, 1 from n",
            params![rows, id],
        )
        .unwrap();
        let release_id: i64 = conn.query_row("select release_id from files where id = ?", [id], |r| r.get(0)).unwrap();
        index_release(&conn, &IdKey::of(&conn).unwrap(), release_id).unwrap();
        let indexed: i64 =
            conn.query_row("select count(*) from release_ids where file_id = ?", [id], |r| r.get(0)).unwrap();
        assert_eq!(indexed, rows + 3, "every row, and the blob's 3 ids");
    }

    /// one pruning step on an idle writer, whenever it last stepped
    fn prune_step(writer: &mut ShardWriter, conn: &mut Connection, now: i64) -> usize {
        writer.ids_step = None;
        writer.prune_ids_some(conn, now, true, false).unwrap()
    }

    /// the same right after a save
    fn prune_step_busy(writer: &mut ShardWriter, conn: &mut Connection, now: i64) -> usize {
        writer.ids_step = None;
        writer.prune_ids_some(conn, now, false, false).unwrap()
    }

    /// A release none of whose files was touched for SEAL_AGE loses its index;
    /// the next save to it indexes it again, and copies across its files are
    /// still found.
    #[test]
    fn quiet_releases_lose_their_index_and_get_it_back_when_saved_to() {
        let (_dir, main) = sealed_fixture();
        let shard = shard_path(&main, shard_of("alt.binaries.t"));
        let mut conn = db::open_at(&shard).unwrap();
        let count =
            |c: &Connection| -> i64 { c.query_row("select count(*) from release_ids", [], |r| r.get(0)).unwrap() };
        let before = count(&conn);
        assert!(before > 0, "the fixture's two files are indexed");
        let now = chrono::Utc::now().timestamp();
        let mut writer = ShardWriter::new(shard_of("alt.binaries.t"));
        assert_eq!(prune_step(&mut writer, &mut conn, now), 0, "touched just now");
        assert_eq!(count(&conn), before);

        let rest = writer.ids.rest_until - now;
        assert!(
            (SEAL_AGE - 600..=SEAL_AGE).contains(&rest),
            "the end: it rests till the release kept could be quiet ({rest})"
        );
        conn.execute("update files set touched_at = ?", [now - SEAL_AGE - 1]).unwrap();
        writer.ids.rest_until = 0;
        assert_eq!(prune_step(&mut writer, &mut conn, now), 0, "indexed just now");
        conn.execute("update release_indexed set at = ?", [now - SEAL_AGE - 1]).unwrap();
        writer.ids.rest_until = now + 1;
        assert_eq!(prune_step(&mut writer, &mut conn, now), 0, "resting");
        writer.ids.rest_until = 0;
        let mut taken = 0;
        while writer.ids.rest_until == 0 {
            taken += prune_step(&mut writer, &mut conn, now) as i64;
        }
        assert_eq!(taken, before, "every row, its own too");
        assert_eq!(count(&conn), 0);
        assert_eq!(writer.ids.rest_until, now + SEAL_AGE, "nothing kept: whatever comes next is new");

        save(&main, &[one_file_release("c.rar", &["<a1@x>", "<b1@x>"])]).unwrap();
        let files: i64 = conn.query_row("select count(*) from files", [], |r| r.get(0)).unwrap();
        assert_eq!(files, 2, "no c.rar: all of it was in a.rar and b.rar");
        assert_eq!(count(&conn), before, "indexed again in full");

        // all copies: no file was touched, but the index is new, soo it stays
        writer.ids.rest_until = 0;
        while writer.ids.rest_until == 0 {
            prune_step(&mut writer, &mut conn, now);
        }
        assert_eq!(count(&conn), before, "indexed again just now: not quiet");
    }

    /// A release down to one file loses its index whatever its age: saves dont keep a one-file release's index up.
    #[test]
    fn a_one_file_release_loses_its_index_however_new() {
        let (_dir, main) = sealed_fixture();
        let shard = shard_path(&main, shard_of("alt.binaries.t"));
        let mut conn = db::open_at(&shard).unwrap();
        conn.execute("delete from files where filename = 'b.rar'", []).unwrap();
        let count =
            |c: &Connection| -> i64 { c.query_row("select count(*) from release_ids", [], |r| r.get(0)).unwrap() };
        assert!(count(&conn) > 0);
        let now = chrono::Utc::now().timestamp();
        let mut writer = ShardWriter::new(shard_of("alt.binaries.t"));
        while writer.ids.rest_until == 0 {
            prune_step(&mut writer, &mut conn, now);
        }
        assert_eq!(count(&conn), 0, "touched just now, but one file");
        let stamps: i64 = conn.query_row("select count(*) from release_indexed", [], |r| r.get(0)).unwrap();
        assert_eq!(stamps, 0, "its stamp too");
    }

    /// A step that finds the write lock held elsewhere lets it go, counts nothing taken, and rests a while.
    #[test]
    fn a_pruning_step_lets_go_of_a_held_lock_and_rests() {
        let (_dir, main) = sealed_fixture();
        let shard = shard_path(&main, shard_of("alt.binaries.t"));
        let mut conn = db::open_at(&shard).unwrap();
        let now = chrono::Utc::now().timestamp();
        conn.execute("update files set touched_at = ?", [now - SEAL_AGE - 1]).unwrap();
        conn.execute("update release_indexed set at = ?", [now - SEAL_AGE - 1]).unwrap();
        let before: i64 = conn.query_row("select count(*) from release_ids", [], |r| r.get(0)).unwrap();
        let other = db::open_at(&shard).unwrap();
        other.execute_batch("begin immediate").unwrap();
        let mut writer = ShardWriter::new(shard_of("alt.binaries.t"));
        assert_eq!(prune_step_busy(&mut writer, &mut conn, now), 0, "the lock is held");
        assert_eq!(writer.ids.rest_until, now + PRUNE_IDS_BUSY_REST);
        other.execute_batch("rollback").unwrap();
        let after: i64 = conn.query_row("select count(*) from release_ids", [], |r| r.get(0)).unwrap();
        assert_eq!(after, before);
        writer.ids.rest_until = 0;
        assert_eq!(prune_step(&mut writer, &mut conn, now) as i64, before, "free again: all of it");
    }

    /// A big release goes a chunk at a time, its own row first; a save that
    /// indexes it again in between stops the rest going.
    #[test]
    fn pruning_a_big_index_stops_when_a_save_indexes_it_again() {
        let (_dir, main) = sealed_fixture();
        let shard = shard_path(&main, shard_of("alt.binaries.t"));
        let mut conn = db::open_at(&shard).unwrap();
        let (release_id, file_id): (i64, i64) = conn
            .query_row("select release_id, id from files where filename = 'a.rar'", [], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        let extra = PRUNE_IDS_CHUNK_IDLE * 2 + 7;
        conn.execute(
            "with recursive n(i) as (select 1 union all select i + 1 from n where i < ?)
             insert or ignore into release_ids select ?, 1000000 + i, ? from n",
            params![extra, release_id, file_id],
        )
        .unwrap();
        let count = |c: &Connection| -> i64 {
            c.query_row("select count(*) from release_ids where release_id = ?", [release_id], |r| r.get(0)).unwrap()
        };
        let marked = |c: &Connection| -> bool {
            c.query_row("select count(*) from release_ids where release_id = ? and id_hash = 0", [release_id], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap()
                > 0
        };
        let all = count(&conn);
        let now = chrono::Utc::now().timestamp();

        // a save in between touched it and indexed it again: the rest stays
        let mut writer = ShardWriter::new(shard_of("alt.binaries.t"));
        writer.ids.pruning = Some(release_id);
        assert_eq!(prune_step(&mut writer, &mut conn, now), 0, "kept up again: nothing goes");
        assert_eq!(count(&conn), all);
        assert_eq!(writer.ids.pruning, None);

        conn.execute("update files set touched_at = ?", [now - SEAL_AGE - 1]).unwrap();
        conn.execute("update release_indexed set at = ?", [now - SEAL_AGE - 1]).unwrap();

        let mut writer = ShardWriter::new(shard_of("alt.binaries.t"));
        let mut taken = 0;
        while writer.ids.rest_until == 0 {
            taken += prune_step_busy(&mut writer, &mut conn, now) as i64;
            assert!(!marked(&conn), "its own row goes first");
        }
        assert_eq!(taken, all);
        assert_eq!(count(&conn), 0);
        assert_eq!(writer.ids.pruning, None);
    }

    /// release Rel with one file `name` of the given message-ids
    fn one_file_release(name: &str, ids: &[&str]) -> Release {
        Release {
            name: "Rel".into(),
            group: "alt.binaries.t".into(),
            articles: ids
                .iter()
                .enumerate()
                .map(|(n, id)| Article {
                    message_id: (*id).into(),
                    subject: format!("\"{name}\" yEnc ({}/9)", n + 1),
                    filename: Some(name.into()),
                    part: Some(n as i64 + 1),
                    total_parts: Some(9),
                    bytes: 10,
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        }
    }

    /// Saving new articles to a release with 2,000 files looks each up in
    /// the other files once, as with 2 files: not once per file.
    #[test]
    fn a_release_with_many_files_costs_no_more_per_article() {
        let cost = |files: usize| -> usize {
            let dir = tempfile::tempdir().unwrap();
            let main = dir.path().join("atlas.db");
            db::create_db_at(&main).unwrap();
            let names: Vec<String> = (0..files).map(|n| format!("<f{n}@x>")).collect();
            let mut release = one_file_release("f", &[]);
            release.articles = names
                .iter()
                .enumerate()
                .map(|(n, id)| Article {
                    message_id: id.clone(),
                    filename: Some(format!("f{n}.rar")),
                    part: Some(1),
                    total_parts: Some(9),
                    bytes: 10,
                    ..Default::default()
                })
                .collect();
            save(&main, &[release]).unwrap();
            let ids: Vec<String> = (0..20).map(|n| format!("<new{n}@x>")).collect();
            let ids: Vec<&str> = ids.iter().map(|s| s.as_str()).collect();
            let later = one_file_release("z.rar", &ids);
            let mut conn = db::open_at(&shard_path(&main, shard_of("alt.binaries.t"))).unwrap();
            LOOKUPS.set(0);
            ShardWriter::new(shard_of("alt.binaries.t"))
                .save(&mut conn, &Ids::new(&main), [std::slice::from_ref(&later)])
                .unwrap();
            LOOKUPS.get()
        };
        assert_eq!(cost(2_000), 20, "one an article");
        assert_eq!(cost(2), 20);
    }

    /// what purging takes off the totals is what it deleted, counted in its
    /// own transaction: the rest of the totals, including what writers added
    /// in between, stays as it is
    #[test]
    fn purging_takes_only_what_it_deleted_off_the_totals() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        db::create_db_at(&main).unwrap();
        let group = "alt.binaries.t";
        let release = |name: &str, parts: i64, have: i64| Release {
            name: name.into(),
            group: group.into(),
            articles: (1..=have)
                .map(|p| Article {
                    message_id: format!("<{name}-{p}@x>"),
                    filename: Some("a.rar".into()),
                    part: Some(p),
                    total_parts: Some(parts),
                    bytes: 10,
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        };
        // two broken releases (3 + 2 articles) and a whole one
        save(&main, &[release("Broken1", 5, 3), release("Broken2", 4, 2), release("Whole1", 2, 2)]).unwrap();
        let shard = shard_path(&main, shard_of(group));
        let conn = db::open_at(&shard).unwrap();
        let totals = |c: &Connection| -> (i64, i64) {
            let get = |k: &str| c.query_row("select value from meta where key = ?", [k], |r| r.get(0)).unwrap();
            (get("releases"), get("articles"))
        };
        assert_eq!(totals(&conn), (3, 7));
        // a writer in another connection added a release and its count, and
        // another's not yet seen in the rows (3 more in the total)
        save(&main, &[release("Whole2", 3, 3)]).unwrap();
        conn.execute("update meta set value = value + 3 where key = 'articles'", []).unwrap();
        assert_eq!(totals(&conn), (4, 13));

        purge_incomplete(&main).unwrap();
        assert_eq!(totals(&conn), (2, 8), "only the 2 releases and 5 articles that were deleted went");
        let left: i64 = conn.query_row("select count(*) from releases", [], |r| r.get(0)).unwrap();
        assert_eq!(left, 2);
    }

    #[test]
    fn a_failed_save_leaves_no_domain_behind_in_the_writer() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        db::create_db_at(&main).unwrap();
        let shard = shard_of("alt.binaries.t");
        let mut conn = db::open_at(&shard_path(&main, shard)).unwrap();
        let mut writer = ShardWriter::new(shard);
        // enough articles on one new domain for it to get a row
        let release = |name: &str, first: u32| Release {
            name: name.into(),
            group: "alt.binaries.t".into(),
            articles: (first..first + SHARED_AFTER + 1)
                .map(|n| Article {
                    message_id: format!("<{n:032x}@fresh>"),
                    filename: Some("a.rar".into()),
                    part: Some(n as i64 + 1),
                    bytes: 10,
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        };

        // the save fails at its last write, after the domain's row went in
        conn.execute_batch(
            "create temp trigger no_totals before update on meta begin select raise(abort, 'forced'); end",
        )
        .unwrap();
        assert!(writer.save(&mut conn, &Ids::new(&main), [std::slice::from_ref(&release("One", 0))]).is_err());
        conn.execute_batch("drop trigger no_totals").unwrap();
        let rows: i64 = conn.query_row("select count(*) from domains", [], |r| r.get(0)).unwrap();
        assert_eq!(rows, 0, "rolled back with the save");

        let two = release("Two", 100);
        writer.save(&mut conn, &Ids::new(&main), [std::slice::from_ref(&two)]).unwrap();
        let orphans: i64 = conn
            .query_row(
                "select count(*) from segments s where s.domain != 0
                 and not exists (select 1 from domains d where d.id = s.domain)",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(orphans, 0, "every segment's domain has a row");
        let shards = db::open_with_shards(&main).unwrap();
        let id: i64 = shards.query_row("select id from releases where name = 'Two'", [], |r| r.get(0)).unwrap();
        let mut got: Vec<String> = articles(&shards, id).unwrap().into_iter().map(|a| a.message_id).collect();
        let mut want: Vec<String> = two.articles.iter().map(|a| a.message_id.clone()).collect();
        got.sort();
        want.sort();
        assert_eq!(got, want, "the NZB's message-ids are whole");
    }

    #[test]
    fn writers_leave_files_untouched_since_the_upgrade_to_compaction() {
        let (_dir, main) = sealed_fixture();
        let shard = shard_of("alt.binaries.t");
        let mut conn = db::open_at(&shard_path(&main, shard)).unwrap();
        conn.execute("update files set touched_at = null", []).unwrap();
        let now: i64 = conn.query_row("select unixepoch()", [], |r| r.get(0)).unwrap();
        let mut writer = ShardWriter::new(shard);
        assert_eq!(writer.seal_some(&mut conn, now).unwrap(), 0, "complete or not, a legacy file waits");
        assert_eq!(writer.seal_some(&mut conn, now + SEAL_WALK_EVERY).unwrap(), 0);
        // compaction still takes them
        assert!(due(Some(3), &[0b1110], None, false, now));
        assert!(due(Some(2), &[0b0010], None, false, now));
    }

    #[test]
    fn a_file_that_wont_seal_does_not_stop_the_tick() {
        let (_dir, main) = sealed_fixture();
        let shard = shard_of("alt.binaries.t");
        let mut conn = db::open_at(&shard_path(&main, shard)).unwrap();
        let mut writer = ShardWriter::new(shard);
        let now = 2_000_000_000;
        conn.execute("update files set touched_at = ?", [now - SEAL_AGE - 1]).unwrap();
        // a.rar has rows and a blob nobody can decode
        conn.execute("update files set blob = x'ff00ff' where filename = 'a.rar'", []).unwrap();
        let rows = |c: &Connection, name: &str| -> i64 {
            c.query_row(
                "select count(*) from segments where file_id = (select id from files where filename = ?)",
                [name],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(writer.seal_some(&mut conn, now).unwrap(), 1, "b.rar sealed, a.rar skipped");
        assert_eq!(rows(&conn, "a.rar"), 3, "a.rar keeps its rows");
        assert_eq!(rows(&conn, "b.rar"), 0);
        let b: i64 = conn.query_row("select id from files where filename = 'b.rar'", [], |r| r.get(0)).unwrap();
        assert_eq!(writer.seal_after, b, "the cursor went past the bad file");
    }

    /// a collection's slice: one article of file `file` in release `name`
    fn in_collection(collection: &str, name: &str, file: &str, id: &str, poster: &str) -> Release {
        Release {
            name: name.into(),
            group: "alt.binaries.t".into(),
            poster: poster.into(),
            collection: Some(collection.into()),
            articles: vec![Article {
                message_id: id.into(),
                author: poster.into(),
                subject: format!("\"{file}\" yEnc (1/1)"),
                filename: Some(file.into()),
                release_name: Some(file.into()),
                part: Some(1),
                total_parts: Some(1),
                bytes: 10,
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    fn fresh_shard_db() -> (tempfile::TempDir, PathBuf, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        db::create_db_at(&main).unwrap();
        let conn = db::open_shard(&shard_path(&main, shard_of("alt.binaries.t"))).unwrap();
        (dir, main, conn)
    }

    fn rows_of(conn: &Connection) -> Vec<(String, i64, i64)> {
        conn.prepare(
            "select r.name, r.parts, (select count(*) from files f where f.release_id = r.id) from releases r order by id",
        )
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
    }

    /// a row saved before collections: name is its file's parsed name
    fn before_collections(main: &Path, name: &str, id: &str, poster: &str) {
        let mut old = in_collection("unused", name, name, id, poster);
        old.collection = None;
        save(main, &[old]).unwrap();
        start_grouping(main).unwrap();
    }

    #[test]
    fn a_collection_stays_one_release_over_slices() {
        let (_dir, main, conn) = fresh_shard_db();
        save(&main, &[in_collection("k", "a.jpg", "a.jpg", "<a@x>", "p")]).unwrap();
        // another slice or server: its first file is another one
        save(&main, &[in_collection("k", "b.jpg", "b.jpg", "<b@x>", "p")]).unwrap();
        assert_eq!(rows_of(&conn), vec![("a.jpg".to_string(), 2, 2)]);
        // the same article again adds nothing
        save(&main, &[in_collection("k", "b.jpg", "b.jpg", "<b@x>", "p")]).unwrap();
        assert_eq!(rows_of(&conn), vec![("a.jpg".to_string(), 2, 2)]);
        let totals: i64 = conn.query_row("select value from meta where key = 'releases'", [], |r| r.get(0)).unwrap();
        assert_eq!(totals, 1, "new releases are counted once");
    }

    #[test]
    fn two_slices_of_a_collection_in_one_transaction_are_one_release() {
        let (_dir, main, conn) = fresh_shard_db();
        let (a, b) =
            (in_collection("k", "a.jpg", "a.jpg", "<a@x>", "p"), in_collection("k", "b.jpg", "b.jpg", "<b@x>", "p"));
        save(&main, &[a, b]).unwrap();
        assert_eq!(rows_of(&conn), vec![("a.jpg".to_string(), 2, 2)]);
    }

    #[test]
    fn a_name_another_release_has_gets_a_part_of_the_hash() {
        let (_dir, main, conn) = fresh_shard_db();
        save(&main, &[in_collection("one", "same.jpg", "same.jpg", "<a@x>", "p")]).unwrap();
        save(&main, &[in_collection("two", "same.jpg", "same.jpg", "<b@x>", "q")]).unwrap();
        let rows = rows_of(&conn);
        assert_eq!(rows.len(), 2, "{rows:?}");
        assert_eq!(rows[0].0, "same.jpg");
        assert!(rows[1].0.starts_with("same.jpg [") && rows[1].0.ends_with(']'), "{rows:?}");
        assert!(rows[1].0[10..rows[1].0.len() - 1].bytes().all(|b| b.is_ascii_digit()), "{rows:?}");
        // and it stays that row
        save(&main, &[in_collection("two", "other.jpg", "other.jpg", "<c@x>", "q")]).unwrap();
        assert_eq!(rows_of(&conn)[1].1, 2);
    }

    #[test]
    fn a_suffix_another_release_has_moves_to_the_next_number() {
        let (_dir, main, conn) = fresh_shard_db();
        save(&main, &[in_collection("one", "same.jpg", "same.jpg", "<a@x>", "p")]).unwrap();
        let digits = IdKey::of(&conn).unwrap().hash("two").rem_euclid(100_000_000);
        save(&main, &[in_collection("other", &format!("same.jpg [{digits:08}]"), "x.jpg", "<b@x>", "q")]).unwrap();
        save(&main, &[in_collection("two", "same.jpg", "same.jpg", "<c@x>", "r")]).unwrap();
        let rows = rows_of(&conn);
        assert_eq!(rows.len(), 3, "the posts stay apart: {rows:?}");
        assert_eq!(rows[2].0, format!("same.jpg [{:08}]", (digits + 1) % 100_000_000));
    }

    #[test]
    fn a_later_slices_file_name_doesnt_change_the_rows_flag() {
        let (_dir, main, conn) = fresh_shard_db();
        save(&main, &[in_collection("k", "Readable.Movie.2024.mkv", "Readable.Movie.2024.mkv", "<a@x>", "p")]).unwrap();
        let mut random = in_collection("k", "a1B2c3D4e5F6g7H8i9J0k1L2", "a1B2c3D4e5F6g7H8i9J0k1L2", "<b@x>", "p");
        random.is_obfuscated = true;
        save(&main, &[random]).unwrap();
        let flag: i64 = conn.query_row("select is_obfuscated from releases", [], |r| r.get(0)).unwrap();
        assert_eq!(flag, 0, "the row is still named Readable.Movie.2024.mkv");
    }

    #[test]
    fn a_full_collection_starts_a_new_release() {
        let (_dir, main, conn) = fresh_shard_db();
        let mut many = in_collection("k", "f0", "f0", "<0@x>", "p");
        many.articles = (0..=crate::collections::MAX_COLLECTION_FILES)
            .map(|i| in_collection("k", "f", &format!("f{i}"), &format!("<{i}@x>"), "p").articles.remove(0))
            .collect();
        save(&main, &[many]).unwrap();
        save(&main, &[in_collection("k", "later.jpg", "later.jpg", "<later@x>", "p")]).unwrap();
        let rows = rows_of(&conn);
        assert_eq!(rows.len(), 2, "{:?}", rows.iter().map(|r| (&r.0, r.2)).collect::<Vec<_>>());
        assert_eq!(rows[1].0, "later.jpg");
        save(&main, &[in_collection("k", "more.jpg", "more.jpg", "<more@x>", "p")]).unwrap();
        assert_eq!(rows_of(&conn)[1].2, 2, "the key points at the new one");
    }

    #[test]
    fn a_collection_whose_release_is_gone_gets_a_new_one() {
        let (_dir, main, conn) = fresh_shard_db();
        save(&main, &[in_collection("k", "a.jpg", "a.jpg", "<a@x>", "p")]).unwrap();
        // what a purge leaves: `collections` keeps its row
        conn.execute_batch("delete from segments; delete from files; delete from releases;").unwrap();
        save(&main, &[in_collection("k", "b.jpg", "b.jpg", "<b@x>", "p")]).unwrap();
        assert_eq!(rows_of(&conn), vec![("b.jpg".to_string(), 1, 1)]);
        let points: i64 = conn
            .query_row("select count(*) from collections c join releases r on r.id = c.release_id", [], |r| r.get(0))
            .unwrap();
        assert_eq!(points, 1);
    }

    #[test]
    fn a_file_started_before_collections_stays_in_its_row() {
        let (_dir, main, conn) = fresh_shard_db();
        before_collections(&main, "a.jpg", "<a@x>", "p");
        // a new part of it, a copy of what it has, and a new file of the same post
        let mut late = in_collection("k", "a.jpg", "a.jpg", "<a2@x>", "p");
        // its second part: the same part again under another message-id would be a repost, not counted
        late.articles[0].part = Some(2);
        late.articles.extend(in_collection("k", "a.jpg", "a.jpg", "<a@x>", "p").articles);
        late.articles.extend(in_collection("k", "a.jpg", "b.jpg", "<b@x>", "p").articles);
        save(&main, &[late]).unwrap();
        let rows = rows_of(&conn);
        assert_eq!(rows[0], ("a.jpg".to_string(), 2, 1), "its new part went in, its copy didnt: {rows:?}");
        assert_eq!(rows.len(), 2, "{rows:?}");
        assert_eq!(rows[1].1, 1, "the new file starts the collection: {rows:?}");
        assert_eq!(rows[1].0, "b.jpg", "named after what it holds: {rows:?}");
    }

    #[test]
    fn all_of_a_post_in_old_rows_makes_no_new_row() {
        let (_dir, main, conn) = fresh_shard_db();
        before_collections(&main, "a.jpg", "<a@x>", "p");
        save(&main, &[in_collection("k", "a.jpg", "a.jpg", "<a@x>", "p")]).unwrap();
        assert_eq!(rows_of(&conn), vec![("a.jpg".to_string(), 1, 1)]);
    }

    #[test]
    fn another_posters_file_doesnt_join_an_old_row() {
        let (_dir, main, conn) = fresh_shard_db();
        before_collections(&main, "a.jpg", "<a@x>", "p");
        save(&main, &[in_collection("k", "a.jpg", "a.jpg", "<b@x>", "q")]).unwrap();
        let rows = rows_of(&conn);
        assert_eq!(rows.len(), 2, "{rows:?}");
        assert_eq!(rows[0].1, 1);
    }

    #[test]
    fn rows_saved_since_are_not_old_rows() {
        let (_dir, main, conn) = fresh_shard_db();
        assert_eq!(grouping_from(&conn).unwrap(), 1, "a new shard");
        let mut plain = in_collection("unused", "a.jpg", "a.jpg", "<a@x>", "p");
        plain.collection = None;
        save(&main, &[plain]).unwrap();
        save(&main, &[in_collection("k", "a.jpg", "a.jpg", "<b@x>", "p")]).unwrap();
        assert_eq!(rows_of(&conn).len(), 2, "not continued: it was saved under this build");
    }

    #[test]
    fn opening_a_shard_doesnt_say_where_collections_start() {
        let (_dir, main, conn) = fresh_shard_db();
        let stored = |conn: &Connection| -> Option<i64> {
            conn.query_row("select value from meta where key = 'grouping_from'", [], |r| r.get(0)).optional().unwrap()
        };
        assert_eq!(stored(&conn), None, "made and opened by this build, no indexer yet");
        let mut plain = in_collection("unused", "a.jpg", "a.jpg", "<a@x>", "p");
        plain.collection = None;
        save(&main, &[plain]).unwrap();
        create_shard(&shard_path(&main, shard_of("alt.binaries.t"))).unwrap();
        assert_eq!(stored(&conn), None, "nor does opening it again");
        let max: i64 = conn.query_row("select max(id) from releases", [], |r| r.get(0)).unwrap();
        start_grouping(&main).unwrap();
        assert_eq!(grouping_from(&conn).unwrap(), max + 1);
        let mut more = in_collection("unused", "b.jpg", "b.jpg", "<b@x>", "p");
        more.collection = None;
        save(&main, &[more]).unwrap();
        start_grouping(&main).unwrap();
        assert_eq!(grouping_from(&conn).unwrap(), max + 1, "the next run doesnt move it");
    }

    /// `by_file` was a list searched per article: quadratic in a release's
    /// files, which a collection makes many of. `cargo test --release -- --ignored`
    #[test]
    #[ignore]
    fn many_files_in_one_release_save_in_about_linear_time() {
        let time = |files: usize| {
            let (_dir, main, _conn) = fresh_shard_db();
            let mut release = in_collection("k", "f0", "f0", "<0@x>", "p");
            release.articles = (0..files)
                .map(|i| in_collection("k", "f", &format!("f{i}"), &format!("<{i}@x>"), "p").articles.remove(0))
                .collect();
            let t = Instant::now();
            save(&main, &[release]).unwrap();
            t.elapsed().as_secs_f64()
        };
        let (small, big) = (time(5_000), time(40_000));
        println!("5,000 files {small:.2}s, 40,000 files {big:.2}s");
        assert!(big / small < 16.0, "8 times the files took {:.1} times as long", big / small);
    }

    #[test]
    fn the_menu_purge_clears_a_collections_row() {
        let (_dir, main, conn) = fresh_shard_db();
        let mut broken = in_collection("k", "a.jpg", "a.jpg", "<a@x>", "p");
        broken.articles[0].total_parts = Some(2);
        save(&main, &[broken]).unwrap();
        assert_eq!(conn.query_row("select count(*) from collections", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
        purge_incomplete(&main).unwrap();
        assert_eq!(conn.query_row("select count(*) from collections", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
    }

    /// another writer on the shard (purge, AI search, the stage's commands) makes a save wait,
    /// not fail: a deferred transaction that read first never gets the busy handler
    #[test]
    fn a_save_waits_for_another_writer_instead_of_failing() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        db::create_db_at(&main).unwrap();
        let other = db::open_at(&shard_path(&main, shard_of("alt.binaries.t"))).unwrap();
        other.execute_batch("begin immediate; update meta set value = value where key = 'releases'").unwrap();
        let saver = std::thread::spawn({
            let main = main.clone();
            move || {
                let article = Article {
                    message_id: "<a@x>".into(),
                    filename: Some("a".into()),
                    part: Some(1),
                    total_parts: Some(1),
                    ..Default::default()
                };
                save(
                    &main,
                    &[Release {
                        name: "Rel".into(),
                        group: "alt.binaries.t".into(),
                        articles: vec![article],
                        ..Default::default()
                    }],
                )
            }
        });
        std::thread::sleep(std::time::Duration::from_millis(300));
        other.execute_batch("commit").unwrap();
        saver.join().unwrap().expect("waited for the other writer");
    }

    fn queued(conn: &Connection) -> Vec<(i64, i64)> {
        conn.prepare("select release_id, due_at from pp_queue")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    }

    /// `nzb::missing_files` on what storage gives back: one subject per file, not per article
    #[test]
    fn missing_files_after_a_save() {
        let saved = |subjects: &[&str]| {
            let (_dir, main, _conn) = fresh_shard_db();
            let posted = subjects
                .iter()
                .enumerate()
                .map(|(i, s)| Article {
                    message_id: format!("<{i}@x>"),
                    subject: (*s).into(),
                    bytes: 10,
                    ..Default::default()
                })
                .collect();
            let releases: Vec<Release> = crate::parser::group_articles(posted)
                .into_values()
                .map(|r| Release { group: "alt.binaries.t".into(), ..r })
                .collect();
            assert_eq!(releases.len(), 1, "{subjects:?}");
            save(&main, &releases).unwrap();
            let conn = db::open_with_shards(&main).unwrap();
            let id: i64 = conn.query_row("select id from releases", [], |r| r.get(0)).unwrap();
            let r = crate::search::get_release_with(&conn, id).unwrap().unwrap();
            (r, articles(&conn, id).unwrap())
        };

        // bracket parts: one file as [n/3] single articles, part 3 missing. A part gap, no file gap
        let (r, rows) = saved(&["[1/3] - \"x.bin\" yEnc (1/1)", "[2/3] - \"x.bin\" yEnc (1/1)"]);
        assert!(!r.complete);
        assert_eq!(rows.iter().map(|a| a.part).collect::<Vec<_>>(), [Some(1), Some(2)]);
        assert_eq!(crate::nzb::missing_files(&rows), None);
        assert_eq!(crate::nzb::gaps(&rows)[0].missing, [3]);
        let (r, rows) = saved(&["abcdefghijklmnop [1/3]", "abcdefghijklmnop [2/3]"]);
        assert!(!r.complete);
        assert_eq!(crate::nzb::missing_files(&rows), None);

        // a file counter: file 1 of 2 whole, file 2 never came
        let (r, rows) = saved(&["[1/2] - \"a.bin\" yEnc (1/2)", "[1/2] - \"a.bin\" yEnc (2/2)"]);
        assert!(!r.complete);
        assert!(crate::nzb::gaps(&rows).is_empty());
        let gap = crate::nzb::missing_files(&rows).expect("file 2 is missing");
        assert_eq!((gap.have, gap.total, gap.missing), (1, Some(2), vec![2]));

        // both files there: complete, no gap
        let (r, rows) =
            saved(&["[1/2] - \"a.rar\" yEnc (1/2)", "[1/2] - \"a.rar\" yEnc (2/2)", "[2/2] - \"a.par2\" yEnc (1/1)"]);
        assert!(r.complete);
        assert_eq!(crate::nzb::missing_files(&rows), None);

        // a 0-based file is saved complete
        let (r, rows) = saved(&["\"z.bin\" yEnc (0/2)", "\"z.bin\" yEnc (1/2)"]);
        assert_eq!(rows.iter().map(|a| a.part).collect::<Vec<_>>(), [Some(0), Some(1)]);
        assert!(r.complete, "parts 0 and 1 of 2");
    }

    fn part_of(name: &str, part: i64, total: i64, id: &str) -> Release {
        let article = Article {
            message_id: id.into(),
            subject: format!("\"{name}\" yEnc ({part}/{total})"),
            filename: Some(name.into()),
            part: Some(part),
            total_parts: Some(total),
            bytes: 10,
            ..Default::default()
        };
        Release { name: name.into(), group: "alt.binaries.t".into(), articles: vec![article], ..Default::default() }
    }

    #[test]
    fn a_save_queues_each_touched_release_and_moves_its_due_time() {
        let (_dir, main, conn) = fresh_shard_db();
        let now = chrono::Utc::now().timestamp();
        save(&main, &[part_of("a", 1, 3, "<1@x>")]).unwrap();
        let rows = queued(&conn);
        assert_eq!(rows.len(), 1);
        assert!((rows[0].1 - (now + 7200)).abs() <= 2, "{rows:?}");
        conn.execute("update pp_queue set due_at = 1", []).unwrap();
        save(&main, &[part_of("a", 2, 3, "<2@x>")]).unwrap();
        let rows = queued(&conn);
        assert_eq!(rows.len(), 1);
        assert!((rows[0].1 - (now + 7200)).abs() <= 2, "{rows:?}");
    }

    #[test]
    fn a_complete_release_is_due_sooner() {
        let (_dir, main, conn) = fresh_shard_db();
        let now = chrono::Utc::now().timestamp();
        save(&main, &[part_of("a", 1, 1, "<1@x>")]).unwrap();
        assert!((queued(&conn)[0].1 - (now + 900)).abs() <= 2);
    }

    #[test]
    fn a_duplicate_article_does_not_touch_the_queue() {
        let (_dir, main, conn) = fresh_shard_db();
        save(&main, &[part_of("a", 1, 3, "<1@x>")]).unwrap();
        conn.execute("update pp_queue set due_at = 1", []).unwrap();
        save(&main, &[part_of("a", 1, 3, "<1@x>")]).unwrap();
        assert_eq!(queued(&conn)[0].1, 1);
    }

    #[test]
    fn a_stage_name_survives_a_late_article() {
        let (_dir, main, conn) = fresh_shard_db();
        save(&main, &[part_of("a", 1, 3, "<1@x>")]).unwrap();
        conn.execute("update releases set display_name = 'kept', is_obfuscated = 0, name_source = 40", []).unwrap();
        let mut late = part_of("a", 2, 3, "<2@x>");
        late.display_name = Some("other".into());
        late.is_obfuscated = true;
        save(&main, &[late]).unwrap();
        let (name, flag): (String, i64) = conn
            .query_row("select display_name, is_obfuscated from releases", [], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        assert_eq!((name.as_str(), flag), ("kept", 0));
    }

    #[test]
    fn without_a_stage_name_a_save_still_sets_the_name() {
        let (_dir, main, conn) = fresh_shard_db();
        save(&main, &[part_of("a", 1, 3, "<1@x>")]).unwrap();
        let mut late = part_of("a", 2, 3, "<2@x>");
        late.display_name = Some("other".into());
        late.is_obfuscated = true;
        save(&main, &[late]).unwrap();
        let (name, flag): (String, i64) = conn
            .query_row("select display_name, is_obfuscated from releases", [], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        assert_eq!((name.as_str(), flag), ("other", 1));
    }

    fn ids_of(segs: &[SegmentRef]) -> Vec<&str> {
        segs.iter().map(|s| s.message_id.as_str()).collect()
    }

    #[test]
    fn file_segments_reads_a_sealed_file_and_late_rows_as_one_list() {
        let (_dir, main) = sealed_fixture();
        let conn = db::open_shard(&shard_path(&main, shard_of("alt.binaries.t"))).unwrap();
        let file: i64 = conn.query_row("select id from files where filename = 'a.rar'", [], |r| r.get(0)).unwrap();
        seal_file(&conn, file).unwrap();
        let late = |part: i64, msg: &str| Release {
            name: "Rel".into(),
            group: "alt.binaries.t".into(),
            articles: vec![Article {
                message_id: msg.into(),
                subject: "\"a.rar\" yEnc (1/3)".into(),
                filename: Some("a.rar".into()),
                part: Some(part),
                total_parts: Some(3),
                bytes: 7,
                ..Default::default()
            }],
            ..Default::default()
        };
        save(&main, &[late(2, "<repost@x>"), late(9, "<nine@x>")]).unwrap();

        let first = file_segments(&conn, file, &SegmentPick::First(2)).unwrap();
        assert_eq!(ids_of(&first), vec!["<a1@x>", "<0abc12def34@ngPost>"]);
        let nine = file_segments(&conn, file, &SegmentPick::Parts(vec![9])).unwrap();
        assert_eq!(ids_of(&nine), vec!["<nine@x>"]);
        let two = file_segments(&conn, file, &SegmentPick::Parts(vec![2])).unwrap();
        assert_eq!(ids_of(&two), vec!["<0abc12def34@ngPost>", "<repost@x>"]);
        let last = file_segments(&conn, file, &SegmentPick::Last(1)).unwrap();
        assert_eq!(ids_of(&last), vec!["<nine@x>"]);
    }

    #[test]
    fn file_segments_keeps_no_more_than_asked_of_a_huge_loose_file() {
        let (_dir, main, conn) = fresh_shard_db();
        let mut release = part_of("big", 1, 60_000, "<1@x>");
        release.articles =
            (1..=60_000).map(|p| part_of("big", p, 60_000, &format!("<{p}@x>")).articles.remove(0)).collect();
        save(&main, &[release]).unwrap();
        let file: i64 = conn.query_row("select id from files", [], |r| r.get(0)).unwrap();
        HELD_ROWS.with(|h| h.set(0));
        let first = file_segments(&conn, file, &SegmentPick::First(3)).unwrap();
        assert_eq!(first.iter().map(|s| s.part).collect::<Vec<_>>(), vec![Some(1), Some(2), Some(3)]);
        let last = file_segments(&conn, file, &SegmentPick::Last(3)).unwrap();
        assert_eq!(last.iter().map(|s| s.part).collect::<Vec<_>>(), vec![Some(59_998), Some(59_999), Some(60_000)]);
        assert!(HELD_ROWS.with(|h| h.get()) <= 8, "{}", HELD_ROWS.with(|h| h.get()));
    }

    #[test]
    fn a_corrupt_blob_is_an_error_not_an_empty_list() {
        let (_dir, main) = sealed_fixture();
        let conn = db::open_shard(&shard_path(&main, shard_of("alt.binaries.t"))).unwrap();
        let file: i64 = conn.query_row("select id from files where filename = 'a.rar'", [], |r| r.get(0)).unwrap();
        seal_file(&conn, file).unwrap();
        conn.execute("update files set blob = x'00112233' where id = ?", [file]).unwrap();
        assert!(file_segments(&conn, file, &SegmentPick::First(1)).is_err());
    }

    #[test]
    fn release_files_reports_parts_and_sealed() {
        let (_dir, main) = sealed_fixture();
        let conn = db::open_shard(&shard_path(&main, shard_of("alt.binaries.t"))).unwrap();
        let id: i64 = conn.query_row("select id from releases", [], |r| r.get(0)).unwrap();
        let file: i64 = conn.query_row("select id from files where filename = 'a.rar'", [], |r| r.get(0)).unwrap();
        seal_file(&conn, file).unwrap();
        let files = release_files(&conn, id, 10).unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!((files[0].filename.as_str(), files[0].parts_seen, files[0].sealed), ("a.rar", 3, true));
        assert_eq!((files[1].filename.as_str(), files[1].parts_seen, files[1].sealed), ("b.rar", 1, false));
        assert_eq!(release_files(&conn, id, 1).unwrap().len(), 1);
    }

    #[test]
    fn old_rows_keep_their_own_posters() {
        let (_dir, main, conn) = fresh_shard_db();
        let mut a = in_collection("unused", "a.mkv", "a.mkv", "<a@x>", "p");
        a.collection = None;
        let mut b = in_collection("unused", "b.nfo", "b.nfo", "<b@x>", "q");
        b.collection = None;
        save(&main, &[a, b]).unwrap();
        start_grouping(&main).unwrap();

        // one collection holding both, its poster the first article's
        let mut both = in_collection("k", "a.mkv", "a.mkv", "<a2@x>", "p");
        both.articles.extend(in_collection("k", "b.nfo", "b.nfo", "<b2@x>", "q").articles);
        save(&main, &[both]).unwrap();
        let poster = |name: &str| -> String {
            conn.query_row("select poster from releases where name = ?", [name], |r| r.get(0)).unwrap()
        };
        assert_eq!((poster("a.mkv"), poster("b.nfo")), ("p".to_string(), "q".to_string()));
        // the next slice of the second file still finds its row
        save(&main, &[in_collection("k", "b.nfo", "b.nfo", "<b3@x>", "q")]).unwrap();
        assert_eq!(rows_of(&conn).len(), 2);
    }

    #[test]
    fn an_old_row_doesnt_take_a_recovered_total() {
        let (_dir, main, conn) = fresh_shard_db();
        let mut old = in_collection("unused", "a.mkv", "a.mkv", "<a@x>", "p");
        old.collection = None;
        save(&main, &[old]).unwrap();
        start_grouping(&main).unwrap();
        let mut late = in_collection("k", "a.mkv", "a.mkv", "<a2@x>", "p");
        late.articles[0].subject = "(1 of 50) \"a.mkv\" yEnc (2/2)".into();
        late.articles[0].part = Some(2);
        late.articles[0].total_parts = Some(2);
        late.articles[0].file_total = Some(50);
        save(&main, &[late]).unwrap();
        let total: Option<i64> =
            conn.query_row("select file_total from releases where name = 'a.mkv'", [], |r| r.get(0)).unwrap();
        assert_eq!(total, None);
    }

    #[test]
    fn zero_indexed_complete_repair_marks_existing_rows() {
        let (_dir, main, conn) = fresh_shard_db();
        let mut r = in_collection("k", "Show.S01E01", "a.nfo", "<a0@x>", "p");
        r.articles[0].subject = "[00/02] - \"a.nfo\" yEnc (1/1)".into();
        r.articles[0].file_total = Some(2);
        let mut r1 = in_collection("k", "Show.S01E01", "b.par2", "<a1@x>", "p");
        r1.articles[0].subject = "[01/02] - \"b.par2\" yEnc (1/1)".into();
        r1.articles[0].file_total = Some(2);
        let mut r2 = in_collection("k", "Show.S01E01", "c.mkv", "<a2@x>", "p");
        r2.articles[0].subject = "[02/02] - \"c.mkv\" yEnc (1/1)".into();
        r2.articles[0].file_total = Some(2);
        r.articles.extend(r1.articles);
        r.articles.extend(r2.articles);
        save(&main, &[r]).unwrap();
        let complete: i64 = conn.query_row("select complete from releases", [], |row| row.get(0)).unwrap();
        assert_eq!(complete, 1);
        conn.execute("update releases set complete = 0", []).unwrap();
        assert_eq!(repair_zero_indexed_complete(&conn).unwrap(), 1);
        let complete: i64 = conn.query_row("select complete from releases", [], |row| row.get(0)).unwrap();
        assert_eq!(complete, 1);
    }

    /// a file `[index/total]` of `name`, whole, in `parts` articles
    fn counted(name: &str, file: &str, index: i64, total: i64, parts: i64) -> Release {
        let mut r = in_collection(name, name, file, &format!("<{file}1@x>"), "p");
        r.articles = (1..=parts)
            .map(|part| Article {
                message_id: format!("<{file}{part}@x>"),
                author: "p".into(),
                subject: format!("[{index:02}/{total:02}] - \"{file}\" yEnc ({part}/{parts})"),
                filename: Some(file.into()),
                release_name: Some(file.into()),
                part: Some(part),
                total_parts: Some(parts),
                file_total: Some(total),
                bytes: 10,
                ..Default::default()
            })
            .collect();
        r
    }

    fn complete_of(conn: &Connection, name: &str) -> i64 {
        conn.query_row("select complete from releases where name = ?", [name], |r| r.get(0))
            .unwrap_or_else(|_| panic!("{name} not among {:?}", rows_of(conn)))
    }

    #[test]
    fn a_whole_video_named_as_its_release_is_complete_whatever_the_counter_says() {
        let (_dir, main, conn) = fresh_shard_db();
        // a season under one counter: each episode is a row of its own, "1 of 24 files"
        let episode = "Show.S01E03.1080p.BluRay.x264-G.mkv";
        save(&main, &[counted(episode, episode, 3, 24, 2)]).unwrap();
        assert_eq!(complete_of(&conn, episode), 1);
        // named without its extension
        save(&main, &[counted("Film.2020.1080p.WEB-DL-G", "Film.2020.1080p.WEB-DL-G.mp4", 1, 2, 2)]).unwrap();
        assert_eq!(complete_of(&conn, "Film.2020.1080p.WEB-DL-G"), 1);
        // a piece of a video, a sample, a file of another name, an archive, one missing a part: not
        for (name, file) in [
            ("Film.1999.CD1.avi", "Film.1999.CD1.avi"),
            ("Film.1999.Disc.2.mkv", "Film.1999.Disc.2.mkv"),
            ("Film_1999_CD1.avi", "Film_1999_CD1.avi"),
            ("Film_1999_Part_1.mkv", "Film_1999_Part_1.mkv"),
            ("grp-film-sample.mkv", "grp-film-sample.mkv"),
            ("Film.1999.DVDRip", "abc.mkv"),
            ("Film.1999.part01.rar", "Film.1999.part01.rar"),
        ] {
            save(&main, &[counted(name, file, 1, 3, 2)]).unwrap();
            assert_eq!(complete_of(&conn, name), 0, "{name}");
        }
        let mut short = counted("Other.S01E01.mkv", "Other.S01E01.mkv", 1, 24, 3);
        short.articles.remove(1);
        save(&main, &[short]).unwrap();
        assert_eq!(complete_of(&conn, "Other.S01E01.mkv"), 0, "a part missing");
        // two files of a counter of 24 are still counted as before
        let mut two = counted("Pack.S01", "Pack.S01E01.mkv", 1, 24, 1);
        two.articles.extend(counted("Pack.S01", "Pack.S01E02.mkv", 2, 24, 1).articles);
        save(&main, &[two]).unwrap();
        assert_eq!(complete_of(&conn, "Pack.S01"), 0);
    }

    /// a release `name` of these files, each `(file, parts)`, whole, under a counter of `total`
    fn files_of(name: &str, files: &[(&str, i64)], total: i64) -> Release {
        let mut r = counted(name, files[0].0, 1, total, files[0].1);
        for (i, (file, parts)) in files.iter().enumerate().skip(1) {
            r.articles.extend(counted(name, file, i as i64 + 1, total, *parts).articles);
        }
        r
    }

    #[test]
    fn a_whole_video_beside_its_par2_set_is_complete_without_the_posts_thumbnail() {
        let (_dir, main, conn) = fresh_shard_db();
        // `[9/9] - "thumbnail.jpg"` came as a row of its own, under another name and poster
        let film = "Some.Film.1997.2160p.MA.WEB-DL.DDP5.1.H.265-GRP";
        let files = [
            ("Some.Film.1997.2160p.MA.WEB-DL.DDP5.1.H.265-GRP.par2", 1),
            ("Some.Film.1997.2160p.MA.WEB-DL.DDP5.1.H.265-GRP.vol00-01.par2", 2),
            ("Some.Film.1997.2160p.MA.WEB-DL.DDP5.1.H.265-GRP.vol01-03.par2", 3),
            ("Some.Film.1997.2160p.MA.WEB-DL.DDP5.1.H.265-GRP.mkv", 4),
            ("Some.Film.1997.2160p.MA.WEB-DL.DDP5.1.H.265-GRP.nfo", 1),
        ];
        save(&main, &[files_of(film, &files, 9)]).unwrap();
        assert_eq!(complete_of(&conn, film), 1);
        // with the post's id after its name
        let named = "other.film.2001.1080p.bluray-grp [54205819]";
        let files = [("other.film.2001.1080p.bluray-grp.mkv", 3), ("other.film.2001.1080p.bluray-grp.vol0+1.par2", 1)];
        save(&main, &[files_of(named, &files, 3)]).unwrap();
        assert_eq!(complete_of(&conn, named), 1);
        // two videos, a video of another name, a sample beside the film, a piece of a film: not
        for (name, files) in [
            ("Two.2002", vec![("Two.2002.mkv", 2), ("Two.2002.Extras.mkv", 1), ("Two.2002.par2", 1)]),
            ("Abc.2003", vec![("xyz.mkv", 2), ("Abc.2003.par2", 1)]),
            ("Smp.2004", vec![("Smp.2004.mkv", 2), ("Smp.2004.sample.mkv", 1)]),
            ("Cut.2005.CD1", vec![("Cut.2005.CD1.avi", 2), ("Cut.2005.CD1.par2", 1)]),
        ] {
            save(&main, &[files_of(name, &files, 9)]).unwrap();
            assert_eq!(complete_of(&conn, name), 0, "{name}");
        }
        // a part of the film missing
        let mut short = files_of("Gap.2006", &[("Gap.2006.mkv", 3), ("Gap.2006.par2", 1)], 3);
        short.articles.remove(1);
        save(&main, &[short]).unwrap();
        assert_eq!(complete_of(&conn, "Gap.2006"), 0, "a part missing");
    }

    #[test]
    fn a_whole_rar_set_named_as_its_release_is_complete_whatever_the_counter_says() {
        let (_dir, main, conn) = fresh_shard_db();
        // an episode of a season under one counter: its volumes, first to last, and a subtitle
        let episode = "Show.S01E06.1080p.WEB-DL-G";
        let files = [
            ("Show.S01E06.1080p.WEB-DL-G.part01.rar", 3),
            ("Show.S01E06.1080p.WEB-DL-G.part02.rar", 3),
            ("Show.S01E06.1080p.WEB-DL-G.part03.rar", 1),
            ("Show.S01E06.1080p.WEB-DL-G.srt", 1),
        ];
        save(&main, &[files_of(episode, &files, 74)]).unwrap();
        assert_eq!(complete_of(&conn, episode), 1);
        // the old names, and the post's id after the release's name
        let old = "film.2009.bluray-g [71141734]";
        let files = [("film.2009.bluray-g.rar", 2), ("film.2009.bluray-g.r00", 2), ("film.2009.bluray-g.r01", 1)];
        save(&main, &[files_of(old, &[files[0], files[1], files[2], ("film.2009.bluray-g.sfv", 1)], 9)]).unwrap();
        assert_eq!(complete_of(&conn, old), 1);
        // a gap, no first volume, a last volume no shorter, volumes of uneven size, another name, a video beside, one
        // volume alone: not
        for (name, files) in [
            ("Gap.S01E01", vec![("Gap.S01E01.part01.rar", 2), ("Gap.S01E01.part03.rar", 1)]),
            ("Late.S01E01", vec![("Late.S01E01.part02.rar", 2), ("Late.S01E01.part03.rar", 1)]),
            ("Even.S01E01", vec![("Even.S01E01.part01.rar", 2), ("Even.S01E01.part02.rar", 2)]),
            (
                "Odd.S01E01",
                vec![("Odd.S01E01.part01.rar", 3), ("Odd.S01E01.part02.rar", 2), ("Odd.S01E01.part03.rar", 1)],
            ),
            ("Film.2001.DVDRip", vec![("abc.part01.rar", 2), ("abc.part02.rar", 1)]),
            ("Mix.S01E01", vec![("Mix.S01E01.part01.rar", 2), ("Mix.S01E01.part02.rar", 1), ("Mix.S01E01.mkv", 1)]),
            ("One.S01E01", vec![("One.S01E01.part01.rar", 2)]),
        ] {
            save(&main, &[files_of(name, &files, 40)]).unwrap();
            assert_eq!(complete_of(&conn, name), 0, "{name}");
        }
    }

    /// release `name`'s PAR2 set, as the PAR2 step keeps it, naming `files`
    fn set_of(conn: &Connection, name: &str, files: &[&str], complete: bool) -> i64 {
        let id: i64 = conn.query_row("select id from releases where name = ?", [name], |r| r.get(0)).unwrap();
        let inventory = crate::par2::Inventory {
            files: files
                .iter()
                .enumerate()
                .map(|(i, f)| crate::par2::Par2File {
                    set_id: [1; 16],
                    file_id: [i as u8; 16],
                    md5: [0; 16],
                    md5_16k: [0; 16],
                    size: 10,
                    name: (*f).into(),
                    bound: true,
                    crc32: None,
                })
                .collect(),
            complete,
            ..Default::default()
        };
        for w in crate::archive::store::par2_file_writes(id, &inventory) {
            conn.execute(w.sql, rusqlite::params_from_iter(w.params)).unwrap();
        }
        id
    }

    #[test]
    fn a_whole_par2_set_naming_only_files_there_says_the_release_is_complete() {
        let (_dir, main, conn) = fresh_shard_db();
        // the counter counts a file never posted: 3 of "4"
        let name = "Show.S02E06.1080p.BluRay.REMUX-G";
        let mut r = counted(name, "a.part1.rar", 1, 4, 2);
        r.articles.extend(counted(name, "a.part2.rar", 2, 4, 2).articles);
        r.articles.extend(counted(name, "a.par2", 3, 4, 1).articles);
        save(&main, &[r]).unwrap();
        assert_eq!(complete_of(&conn, name), 0);
        // a set read in part says nothing
        set_of(&conn, name, &["a.part1.rar", "a.part2.rar"], false);
        assert_eq!(complete_of(&conn, name), 0);
        // nor one naming a file the release hasn't
        set_of(&conn, name, &["a.part1.rar", "a.part2.rar", "a.part3.rar"], true);
        assert_eq!(complete_of(&conn, name), 0);
        set_of(&conn, name, &["a.part1.rar", "a.part2.rar"], true);
        assert_eq!(complete_of(&conn, name), 1, "the step's write marks it");
        // a save after the set was read works it out the same way
        let later = "Show.S02E07.1080p.BluRay.REMUX-G";
        let mut r = counted(later, "c.part1.rar", 1, 4, 2);
        r.articles.extend(counted(later, "c.par2", 3, 4, 1).articles);
        save(&main, &[r]).unwrap();
        set_of(&conn, later, &["c.part1.rar", "c.part2.rar"], true);
        assert_eq!(complete_of(&conn, later), 0, "c.part2.rar isn't there yet");
        save(&main, &[counted(later, "c.part2.rar", 2, 4, 2)]).unwrap();
        assert_eq!(complete_of(&conn, later), 1);
        // a part missing from a file the set names: not
        let other = "Film.2001.DVDR-G";
        let mut r = counted(other, "b.part1.rar", 1, 3, 3);
        r.articles.remove(2);
        r.articles.extend(counted(other, "b.par2", 2, 3, 1).articles);
        save(&main, &[r]).unwrap();
        set_of(&conn, other, &["b.part1.rar"], true);
        assert_eq!(complete_of(&conn, other), 0);
        // a data file no set names may be another set's, whose volumes never came: not; a side file it leaves out
        // says nothing either way
        let two_sets = "Film.2002.DVDR-G";
        let mut r = counted(two_sets, "f.nfo", 1, 4, 1);
        r.articles.extend(counted(two_sets, "f.part01.rar", 2, 4, 1).articles);
        r.articles.extend(counted(two_sets, "f.par2", 3, 4, 1).articles);
        save(&main, &[r]).unwrap();
        set_of(&conn, two_sets, &["f.nfo"], true);
        assert_eq!(complete_of(&conn, two_sets), 0, "f.part01.rar is in no set");
        set_of(&conn, two_sets, &["f.part01.rar"], true);
        assert_eq!(complete_of(&conn, two_sets), 1, "f.nfo is a side file");
    }

    #[test]
    fn movies_and_tv_saved_before_are_worked_out_again_once() {
        let (_dir, main, conn) = fresh_shard_db();
        let episode = "Show.S01E04.720p.HDTV.x264-G.mkv";
        save(&main, &[counted(episode, episode, 4, 24, 2)]).unwrap();
        save(&main, &[counted("Lecture.2020.mkv", "Lecture.2020.mkv", 1, 3, 2)]).unwrap();
        conn.execute("update releases set complete = 0", []).unwrap();
        let id = |name: &str| -> i64 {
            conn.query_row("select id from releases where name = ?", [name], |r| r.get(0)).unwrap()
        };
        conn.execute("insert into release_category (release_id, category) values (?1, 5040)", [id(episode)]).unwrap();
        conn.execute("insert into release_category (release_id, category) values (?1, 7020)", [id("Lecture.2020.mkv")])
            .unwrap();
        assert_eq!(movie_tv_whole_again(&conn).unwrap(), vec![id(episode)]);
        assert_eq!(recheck_movie_tv_complete(&conn).unwrap(), vec![id(episode)]);
        assert_eq!(complete_of(&conn, episode), 1);
        assert_eq!(complete_of(&conn, "Lecture.2020.mkv"), 0, "not a movie or TV: left to its next save");
        assert!(movie_tv_whole_again(&conn).unwrap().is_empty());
    }

    #[test]
    fn the_seed_queues_what_the_recheck_made_complete() {
        let (_dir, main, conn) = fresh_shard_db();
        let episode = "Show.S01E05.720p.HDTV.x264-G.mkv";
        save(&main, &[counted(episode, episode, 5, 24, 2)]).unwrap();
        let id: i64 = conn.query_row("select id from releases where name = ?", [episode], |r| r.get(0)).unwrap();
        // saved before, and visited then: complete = 0, no queue row
        conn.execute("update releases set complete = 0", []).unwrap();
        conn.execute("delete from pp_queue", []).unwrap();
        conn.execute("delete from meta where key = 'pp_complete_by_files'", []).unwrap();
        conn.execute("insert into release_category (release_id, category) values (?1, 5040)", [id]).unwrap();
        crate::postprocess::engine::seed_shard(&conn, 0, 0, &[], 1_000).unwrap();
        assert_eq!(complete_of(&conn, episode), 1);
        let due: i64 = conn.query_row("select due_at from pp_queue where release_id = ?", [id], |r| r.get(0)).unwrap();
        assert_eq!(due, 1_000, "its gated body steps are looked at again");
    }

    /// A sealed blob whose articles are under another id of a suffix this shard has an id for too: the same
    /// article, seen again under this shard's id, is in the blob.
    #[test]
    fn a_sealed_blob_under_another_id_of_the_domain_holds_the_same_article() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("atlas.s0.db");
        create_shard(&path).unwrap();
        let conn = db::open_at(&path).unwrap();
        domains_not_unique(&conn).unwrap();
        let other = (5i64 << 32) + 1;
        conn.execute_batch(&format!(
            "insert into domains (id, suffix) values (1, '@x>'), ({other}, '@x>');
             insert into releases (id, name, group_name) values (8, 'R', 'g');"
        ))
        .unwrap();
        let segs = vec![crate::blob::Seg { part: Some(1), bytes: 10, domain: other, local: pack_local("abc") }];
        conn.execute(
            "insert into files (id, release_id, filename, seen, blob) values (3, 8, 'a.rar', x'02', ?1)",
            [crate::blob::encode(&segs)],
        )
        .unwrap();
        let mut cache = SealedCache::new();
        assert!(cache.contains(&conn, 3, &pack_local("abc"), 1).unwrap(), "the same message-id, under this shard's id");
        assert!(cache.contains(&conn, 3, &pack_local("abc"), other).unwrap());
        assert!(!cache.contains(&conn, 3, &pack_local("abd"), 1).unwrap());
    }
}
