//! Releases and their articles, split over `SHARDS` database files next to
//! `atlas.db` (`atlas.s0.db` .. `atlas.s7.db`) by group, each with its own
//! writer. `atlas.db` itself keeps the per group cursors and the id counter.
//!
//! Each shard stores articles compactly (about a quarter of the old layout,
//! see poc_sqlite_2):
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

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use rusqlite::{Connection, OptionalExtension, Result, TransactionBehavior, params};

use crate::db;
use crate::parser::{Article, Release};
use crate::profile;
use crate::search::ArticleRow;

pub const SHARDS: usize = 8;

/// the shard a group's releases live in
pub fn shard_of(group: &str) -> usize {
    let hash = group.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3));
    (hash % SHARDS as u64) as usize
}

/// the shard a release id lives in
pub fn shard_of_id(id: i64) -> usize {
    id.rem_euclid(SHARDS as i64) as usize
}

pub fn global_id(seq: i64, shard: usize) -> i64 {
    seq * SHARDS as i64 + shard as i64
}

/// `atlas.db` -> `atlas.s3.db`
pub fn shard_path(main: &Path, shard: usize) -> PathBuf {
    let stem = main.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "atlas".into());
    main.with_file_name(format!("{stem}.s{shard}.db"))
}

pub fn shard_paths(main: &Path) -> Vec<PathBuf> {
    (0..SHARDS).map(|i| shard_path(main, i)).collect()
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
    Ok(())
}

/// A shard's tables, if missing.
pub fn create_shard(path: &Path) -> Result<()> {
    build_shard(&db::open_at(path)?)
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

/// Columns a shard made before sealing lacks: `files.touched_at` (when
/// articles were last added) and `files.blob` (a sealed file's articles), in
/// the same order `create_shard` has them. Adding a nullable column doesnt
/// rewrite the table.
pub fn migrate_shard(conn: &Connection) -> Result<()> {
    let cols: Vec<String> =
        conn.prepare("pragma table_info(files)")?.query_map([], |r| r.get(1))?.collect::<Result<_>>()?;
    for (col, kind) in [("touched_at", "INTEGER"), ("blob", "BLOB")] {
        if !cols.iter().any(|c| c == col) {
            conn.execute(&format!("alter table files add column {col} {kind}"), [])?;
        }
    }
    Ok(())
}

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
            let conn = db::open_at(&self.main)?;
            let end: i64 = conn.query_row(
                "update meta set value = value + ? where key = 'next_seq' returning value",
                [ID_BLOCK],
                |r| r.get(0),
            )?;
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

/// big-endian base-256 bytes of a base-`n` number given as digit values
fn to_bytes(digits: &[u8], n: u32) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new(); // little endian while building
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

/// `len` base-`n` digit values of a big-endian base-256 number
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

/// Packs a message-id's local part by its shape: hex at half the size
/// (tags 1, 2), other single-alphabet ids as a base-N number (tags 3-7,
/// then `[len]`), anything else as text (tag 0). Deterministic, and
/// `unpack_local` gives back exactly the same string.
pub(crate) fn pack_local(local: &str) -> Vec<u8> {
    let bytes = local.as_bytes();
    let even = !bytes.is_empty() && bytes.len().is_multiple_of(2);
    let lower = even && bytes.iter().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b));
    let upper = even && !lower && bytes.iter().all(|b| b.is_ascii_digit() || (b'A'..=b'F').contains(b));
    if lower || upper {
        let nibble = |c: u8| (c as char).to_digit(16).unwrap_or(0) as u8;
        let mut out = Vec::with_capacity(1 + bytes.len() / 2);
        out.push(if lower { HEX_LOWER } else { HEX_UPPER });
        out.extend(bytes.chunks(2).map(|p| (nibble(p[0]) << 4) | nibble(p[1])));
        return out;
    }
    if (1..=255).contains(&bytes.len()) {
        for tag in [DIGITS, BASE36_LOWER, BASE36_UPPER, BASE62, BASE64_URL] {
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
    let mut out = Vec::with_capacity(1 + bytes.len());
    out.push(TEXT);
    out.extend_from_slice(bytes);
    out
}

/// A local part back from any of the packings (tags 0-7).
pub(crate) fn unpack_local(blob: &[u8]) -> String {
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
}

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
        if let Some(d) = self.ids.get(suffix).or_else(|| self.staged.get(suffix)) {
            return Ok((pack_local(local), *d));
        }
        let found: Option<i64> = conn
            .prepare_cached("select id from domains where suffix = ?")?
            .query_row([suffix], |r| r.get(0))
            .optional()?;
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
                conn.prepare_cached("insert into domains (suffix) values (?)")?.execute([suffix])?;
                let domain = conn.last_insert_rowid();
                self.staged.insert(suffix.to_string(), domain);
                return Ok((pack_local(local), domain));
            }
        };
        self.remember(suffix.to_string(), domain);
        Ok((pack_local(local), domain))
    }

    fn remember(&mut self, suffix: String, domain: i64) {
        if self.ids.len() >= DOMAIN_CACHE {
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

/// one bit per part number present
pub(crate) fn insert_part(bits: &mut Vec<u8>, part: i64) {
    // a bad part number from a broken subject shouldnt allocate a huge bitmap
    if !(0..=1_000_000).contains(&part) {
        return;
    }
    let (byte, bit) = ((part / 8) as usize, part % 8);
    if bits.len() <= byte {
        bits.resize(byte + 1, 0);
    }
    bits[byte] |= 1 << bit;
}

/// exactly parts 1..=expected, nothing else
pub(crate) fn is_exactly(bits: &[u8], expected: i64) -> bool {
    if expected <= 0 || bits.first().is_some_and(|b| b & 1 == 1) {
        return false;
    }
    let count: i64 = bits.iter().map(|b| i64::from(b.count_ones())).sum();
    let highest = bits.iter().rposition(|b| *b != 0).map(|i| i as i64 * 8 + 7 - i64::from(bits[i].leading_zeros()));
    count == expected && highest == Some(expected)
}

/// the article whose subject a file's NZB entry uses comes first in (part, a
/// missing part first, then message-id) order: how the NZB lists them
pub(crate) fn subject_key(part: Option<i64>, message_id: &str) -> (bool, i64, &str) {
    (part.is_some(), part.unwrap_or(0), message_id)
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
            })
        })
        .optional()?;
    if let Some(f) = found {
        return Ok(f);
    }
    conn.prepare_cached("insert into files (release_id, filename, seen) values (?, ?, x'')")?
        .execute(params![release_id, filename])?;
    Ok(FileState { id: conn.last_insert_rowid(), ..FileState::default() })
}

pub(crate) fn put_file(conn: &Connection, f: &FileState) -> Result<()> {
    conn.prepare_cached(
        "update files set subject = ?, subject_part = ?, subject_mid = ?, expected = ?, file_total = ?, seen = ?,
         touched_at = unixepoch() where id = ?",
    )?
    .execute(params![f.subject, f.subject_part, f.subject_mid, f.expected, f.file_total, f.seen, f.id])?;
    Ok(())
}

/// Add one article to a file; false when it was already there, as a row or
/// in the file's sealed blob.
pub(crate) fn add_segment(
    conn: &Connection,
    domains: &mut Domains,
    f: &FileState,
    a: &Article,
    sealed: &mut SealedCache,
) -> Result<bool> {
    let (local, domain) = domains.encode(conn, &a.message_id)?;
    let stored = |sealed: &mut SealedCache, local: &[u8], domain: i64| -> Result<bool> {
        let row = conn
            .prepare_cached("select 1 from segments where file_id = ? and local = ? and domain = ?")?
            .exists(params![f.id, local, domain])?;
        Ok(row || (f.sealed && sealed.contains(conn, f.id, local, domain)?))
    };
    if f.sealed && sealed.contains(conn, f.id, &local, domain)? {
        return Ok(false);
    }
    // an article saved before its domain got a row is stored whole: dont save it twice
    if domain != 0 && stored(sealed, &whole(&a.message_id), 0)? {
        return Ok(false);
    }
    // a row from before locals were packed by alphabet has the same local stored as text
    if (DIGITS..=BASE64_URL).contains(&local[0]) {
        let mut legacy = vec![TEXT];
        legacy.extend_from_slice(unpack_local(&local).as_bytes());
        if stored(sealed, &legacy, domain)? {
            return Ok(false);
        }
    }
    let added = conn
        .prepare_cached("insert or ignore into segments (file_id, local, domain, part, bytes) values (?, ?, ?, ?, ?)")?
        .execute(params![f.id, local, domain, a.part, a.bytes])?;
    Ok(added > 0)
}

/// a release is complete when every file has exactly its parts, and there are
/// as many files as the post said
fn release_complete(conn: &Connection, release_id: i64, file_total: Option<i64>) -> Result<bool> {
    let mut stmt = conn.prepare_cached("select expected, seen from files where release_id = ?")?;
    let mut rows = stmt.query([release_id])?;
    let mut files = 0;
    while let Some(row) = rows.next()? {
        files += 1;
        let expected: Option<i64> = row.get(0)?;
        let seen: Vec<u8> = row.get(1)?;
        if !expected.is_some_and(|e| is_exactly(&seen, e)) {
            return Ok(false);
        }
    }
    Ok(files > 0 && file_total.is_none_or(|ft| files == ft))
}

// ---------------------------------------------------------------- sealing

/// files untouched this long get sealed even when incomplete
pub const SEAL_AGE: i64 = 3 * 86_400;

fn blob_error(e: std::io::Error) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(Box::new(e))
}

/// a sealed file's blob, decoded; none when the file isnt sealed
fn sealed_segments(conn: &Connection, file_id: i64) -> Result<Option<Vec<crate::blob::Seg>>> {
    conn.prepare_cached("select blob from files where id = ? and blob is not null")?
        .query_row([file_id], |r| r.get::<_, Vec<u8>>(0))
        .optional()?
        .map(|b| crate::blob::decode(&b).map_err(blob_error))
        .transpose()
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
    if rows.iter().any(|r| r.part.is_some_and(|p| p < 0)) {
        return Ok(0);
    }
    segs.extend(rows);
    conn.prepare_cached("update files set blob = ? where id = ?")?
        .execute(params![crate::blob::encode(&segs), file_id])?;
    conn.prepare_cached("delete from segments where file_id = ?")?.execute([file_id])?;
    Ok(segs.len())
}

/// The next `limit` files after `after_id`, and which of them a writer
/// seals: ones with rows that are complete and not sealed yet, or untouched
/// for `SEAL_AGE` (a sealed file with late rows waits for this). Files never
/// touched since the upgrade are left to compaction, and files with a
/// negative part number never seal. Returns the ids and the last id looked
/// at, for walking the table `limit` files at a time: `last == after_id`
/// means the end.
pub(crate) fn sealable(conn: &Connection, after_id: i64, limit: usize, now: i64) -> Result<(Vec<i64>, i64)> {
    let mut stmt = conn.prepare_cached(
        "select f.id, f.expected, f.seen, f.touched_at, f.blob is not null,
                exists (select 1 from segments s where s.file_id = f.id),
                exists (select 1 from segments s where s.file_id = f.id and s.part < 0)
         from files f where f.id > ? order by f.id limit ?",
    )?;
    let mut ids = Vec::new();
    let mut last = after_id;
    let mut rows = stmt.query(params![after_id, limit as i64])?;
    while let Some(r) = rows.next()? {
        let id: i64 = r.get(0)?;
        last = id;
        let (has_rows, negative): (bool, bool) = (r.get(5)?, r.get(6)?);
        if !has_rows || negative {
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
    conn.prepare_cached(
        "select f.expected, f.seen, f.touched_at, f.blob is not null,
                exists (select 1 from segments s where s.file_id = f.id),
                exists (select 1 from segments s where s.file_id = f.id and s.part < 0)
         from files f where f.id = ?",
    )?
    .query_row([file_id], |r| {
        let (has_rows, negative): (bool, bool) = (r.get(4)?, r.get(5)?);
        let (seen, touched): (Vec<u8>, Option<i64>) = (r.get(1)?, r.get(2)?);
        Ok(has_rows && !negative && touched.is_some() && due(r.get(0)?, &seen, touched, r.get(3)?, now))
    })
    .optional()
    .map(|due| due.unwrap_or(false))
}

/// A file with rows is due to seal when it's complete and not sealed yet, or
/// untouched for `SEAL_AGE` (never touched since the upgrade counts as old).
pub(crate) fn due(expected: Option<i64>, seen: &[u8], touched: Option<i64>, sealed: bool, now: i64) -> bool {
    let complete = !sealed && expected.is_some_and(|e| is_exactly(seen, e));
    complete || touched.is_none_or(|t| t < now - SEAL_AGE)
}

/// Blobs of sealed files, decoded once each, for checking late articles.
#[derive(Default)]
pub(crate) struct SealedCache {
    files: HashMap<i64, HashSet<(Vec<u8>, i64)>>,
}

impl SealedCache {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// the article (local, domain) is already in file `file_id`'s blob
    pub(crate) fn contains(&mut self, conn: &Connection, file_id: i64, local: &[u8], domain: i64) -> Result<bool> {
        let set = match self.files.entry(file_id) {
            Entry::Occupied(e) => e.into_mut(),
            Entry::Vacant(e) => e.insert(
                sealed_segments(conn, file_id)?.unwrap_or_default().into_iter().map(|s| (s.local, s.domain)).collect(),
            ),
        };
        Ok(set.contains(&(local.to_vec(), domain)))
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
}

/// files sealed per writer tick at most
pub const SEAL_PER_TICK: usize = 2_000;

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

    /// Releases of this shard's groups, in one transaction. The same release
    /// may come up in more than one batch.
    pub fn save<'a>(
        &mut self,
        conn: &mut Connection,
        ids: &Ids,
        batches: impl IntoIterator<Item = &'a [Release]>,
    ) -> Result<()> {
        let r = self.save_in_transaction(conn, ids, batches);
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
    ) -> Result<()> {
        let shard = self.shard;
        let domains = &mut self.domains;
        // blobs of sealed files that get late articles, decoded once each
        let mut sealed = SealedCache::new();
        // files this save completed, for sealing once it's committed
        let mut completed = Vec::new();
        let tx = conn.transaction()?;
        let (mut new_releases, mut new_articles) = (0i64, 0i64);
        {
            let mut upsert = tx.prepare_cached(
                "insert into releases
                    (id, name, size, complete, group_name, poster, posted_date, display_name, is_obfuscated)
                    values (?, ?, ?, ?, ?, ?, ?, ?, ?)
                    on conflict(name, group_name) do update set
                    poster = excluded.poster,
                    posted_date = excluded.posted_date,
                    display_name = coalesce(excluded.display_name, releases.display_name),
                    is_obfuscated = excluded.is_obfuscated
                    returning id, size, parts, file_total",
            )?;
            let mut update_stats = tx
                .prepare_cached("update releases set size = ?, complete = ?, parts = ?, file_total = ? where id = ?")?;

            for release in batches.into_iter().flatten() {
                let t = Instant::now();
                let id = global_id(ids.next()?, shard);
                type Saved = (i64, Option<i64>, Option<i64>, Option<i64>);
                let row: Option<Saved> = upsert
                    .query_row(
                        params![
                            id,
                            release.name,
                            release.size,
                            release.complete as i64,
                            release.group,
                            release.poster,
                            release.date,
                            release.display_name,
                            release.is_obfuscated as i64,
                        ],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                    )
                    .optional()?;
                profile::UPSERT.add_since(t);
                let Some((release_id, old_size, old_parts, old_file_total)) = row else { continue };
                if old_parts.is_none() {
                    new_releases += 1;
                }

                let t = Instant::now();
                // articles by file, in arrival order
                let mut by_file: Vec<(&str, Vec<&Article>)> = Vec::new();
                for a in release.articles.iter().filter(|a| !a.message_id.is_empty()) {
                    let name = a.filename.as_deref().unwrap_or("");
                    match by_file.iter_mut().find(|(n, _)| *n == name) {
                        Some((_, list)) => list.push(a),
                        None => by_file.push((name, vec![a])),
                    }
                }

                let mut added: Vec<&Article> = Vec::new();
                for (name, articles) in by_file {
                    let mut f = file(&tx, release_id, name)?;
                    let before = added.len();
                    for a in articles {
                        if add_segment(&tx, domains, &f, a, &mut sealed)? {
                            f.add(a);
                            added.push(a);
                        }
                    }
                    if added.len() > before {
                        put_file(&tx, &f)?;
                        if !f.sealed && f.expected.is_some_and(|e| is_exactly(&f.seen, e)) {
                            completed.push(f.id);
                        }
                    }
                }
                profile::ARTICLES.add_since(t);

                if added.is_empty() {
                    continue;
                }
                new_articles += added.len() as i64;

                let t = Instant::now();
                let file_total = added.iter().filter_map(|a| a.file_total).chain(old_file_total).max();
                let old_parts = old_parts.unwrap_or(0);
                let earlier_size = if old_parts > 0 { old_size.unwrap_or(0) } else { 0 };
                update_stats.execute(params![
                    earlier_size + added.iter().map(|a| a.bytes).sum::<i64>(),
                    release_complete(&tx, release_id, file_total)? as i64,
                    old_parts + added.len() as i64,
                    file_total,
                    release_id
                ])?;
                profile::STATS.add_since(t);
            }

            add_totals(&tx, new_releases, new_articles)?;
        }

        let t = Instant::now();
        let r = tx.commit();
        profile::COMMIT.add_since(t);
        if r.is_ok() {
            for id in completed {
                self.remember_completed(id);
            }
        }
        r
    }
}

pub(crate) fn add_totals(conn: &Connection, releases: i64, articles: i64) -> Result<()> {
    if releases > 0 || articles > 0 {
        let mut stmt = conn.prepare_cached("update meta set value = value + ? where key = ?")?;
        stmt.execute(params![releases, "releases"])?;
        stmt.execute(params![articles, "articles"])?;
    }
    Ok(())
}

/// Save releases from anywhere (AI search): each to its group's shard, in one
/// transaction per shard.
pub fn save(main: &Path, releases: &[Release]) -> Result<()> {
    let ids = Ids::new(main);
    let mut by_shard: Vec<Vec<Release>> = vec![Vec::new(); SHARDS];
    for r in releases {
        by_shard[shard_of(&r.group)].push(r.clone());
    }
    for (shard, list) in by_shard.iter().enumerate().filter(|(_, l)| !l.is_empty()) {
        let mut conn = db::open_shard(&shard_path(main, shard))?;
        ShardWriter::new(shard).save(&mut conn, &ids, [list.as_slice()])?;
    }
    Ok(())
}

// ---------------------------------------------------------------- reading

/// A release's articles, ordered by filename then part (ties by message-id),
/// for building its NZB. `conn` has the shards attached.
pub fn articles(conn: &Connection, release_id: i64) -> Result<Vec<ArticleRow>> {
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
    let mut rows: Vec<ArticleRow> = rows.collect::<Result<_>>()?;

    // sealed files: their blobs, next to any rows that came after sealing
    let mut sealed = conn.prepare(&format!(
        "select f.blob, f.filename, f.expected, f.subject, r.poster, r.posted_date
         from s{s}.files f join s{s}.releases r on r.id = f.release_id
         where f.release_id = ? and f.blob is not null"
    ))?;
    let mut suffixes: HashMap<i64, Option<String>> = HashMap::new();
    let mut blob_rows = sealed.query([release_id])?;
    while let Some(r) = blob_rows.next()? {
        let blob: Vec<u8> = r.get(0)?;
        let filename: String = r.get(1)?;
        for seg in crate::blob::decode(&blob).map_err(blob_error)? {
            let suffix = match suffixes.entry(seg.domain) {
                Entry::Occupied(e) => e.into_mut(),
                Entry::Vacant(e) => e.insert(
                    conn.query_row(&format!("select suffix from s{s}.domains where id = ?"), [seg.domain], |r| {
                        r.get(0)
                    })
                    .optional()?,
                ),
            };
            rows.push(ArticleRow {
                message_id: decode(&seg.local, suffix.as_deref()),
                filename: (!filename.is_empty()).then(|| filename.clone()),
                part: seg.part,
                total_parts: r.get(2)?,
                bytes: Some(seg.bytes),
                subject: r.get(3)?,
                poster: r.get(4)?,
                posted_date: r.get(5)?,
            });
        }
    }
    rows.sort_by(|a, b| (&a.filename, a.part, &a.message_id).cmp(&(&b.filename, b.part, &b.message_id)));
    Ok(rows)
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
pub fn purge_incomplete(main: &Path) -> Result<()> {
    for path in shard_paths(main) {
        let conn = db::open_shard(&path)?;
        let removed: i64 = conn.query_row("select count(*) from releases where complete = 0", [], |r| r.get(0))?;
        conn.execute_batch(
            "begin;
             delete from segments where file_id in
                (select f.id from files f join releases r on r.id = f.release_id where r.complete = 0);
             delete from files where release_id in (select id from releases where complete = 0);
             delete from releases where complete = 0;
             commit;",
        )?;
        let articles = article_count(&conn, "main")?;
        conn.execute("update meta set value = max(value - ?, 0) where key = 'releases'", [removed])?;
        conn.execute("update meta set value = ? where key = 'articles'", [articles])?;
        let _ = conn.query_row("pragma wal_checkpoint(truncate)", [], |_| Ok(()));
        conn.execute_batch("vacuum")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
            add_segment(&conn, &mut fresh, &file, &article, &mut SealedCache::new()).unwrap(),
            "first time: stored whole"
        );
        for n in 0..SHARED_AFTER {
            fresh.encode(&conn, &format!("<{n:032x}@nyuu>")).unwrap();
        }
        assert!(conn.prepare("select 1 from domains where suffix = '@nyuu>'").unwrap().exists([]).unwrap());
        assert!(
            !add_segment(&conn, &mut fresh, &file, &article, &mut SealedCache::new()).unwrap(),
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
            !add_segment(&conn, &mut Domains::default(), &file, &legacy, &mut SealedCache::new()).unwrap(),
            "legacy text row found"
        );
        let segments: i64 = conn.query_row("select count(*) from segments", [], |r| r.get(0)).unwrap();
        assert_eq!(segments, 2);
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

    #[test]
    fn part_sets() {
        let mut bits = Vec::new();
        for i in [1, 2, 3] {
            insert_part(&mut bits, i);
        }
        assert!(is_exactly(&bits, 3));
        assert!(!is_exactly(&bits, 4));
        insert_part(&mut bits, 5);
        assert!(!is_exactly(&bits, 4), "part 4 missing");
        let mut zero = Vec::new();
        insert_part(&mut zero, 0);
        insert_part(&mut zero, 1);
        assert!(!is_exactly(&zero, 1), "a part 0 doesnt belong");
        let mut huge = Vec::new();
        insert_part(&mut huge, 5_000_000_000);
        assert!(huge.is_empty());
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
        assert!(!add_segment(&conn, &mut domains, &f, &article, &mut SealedCache::new()).unwrap());
        let other = Article { message_id: "<other1@JBinUp.local>".into(), part: Some(2), ..Default::default() };
        assert!(add_segment(&conn, &mut domains, &f, &other, &mut SealedCache::new()).unwrap(), "a new one is a row");
        let loose: i64 = conn.query_row("select count(*) from segments", [], |r| r.get(0)).unwrap();
        assert_eq!(loose, 1);
    }

    #[test]
    fn old_shards_get_the_seal_columns() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("atlas.s0.db");
        {
            let conn = db::open_at(&path).unwrap();
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
}
