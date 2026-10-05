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

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use rusqlite::{Connection, OptionalExtension, Result, params};

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
    )
}

/// A shard's tables, if missing.
pub fn create_shard(path: &Path) -> Result<()> {
    let conn = db::open_at(path)?;
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
            seen BLOB NOT NULL
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
    )
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

// ---------------------------------------------------------------- message-ids

const TEXT: u8 = 0;
const HEX_LOWER: u8 = 1;
const HEX_UPPER: u8 = 2;

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
        out
    } else {
        let mut out = Vec::with_capacity(1 + bytes.len());
        out.push(TEXT);
        out.extend_from_slice(bytes);
        out
    }
}

fn unpack_local(blob: &[u8]) -> String {
    match blob.first() {
        Some(&HEX_LOWER) => blob[1..].iter().map(|b| format!("{b:02x}")).collect(),
        Some(&HEX_UPPER) => blob[1..].iter().map(|b| format!("{b:02X}")).collect(),
        _ => String::from_utf8_lossy(blob.get(1..).unwrap_or_default()).into_owned(),
    }
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
        if let Some(d) = self.ids.get(suffix) {
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
                conn.last_insert_rowid()
            }
        };
        if self.ids.len() >= DOMAIN_CACHE {
            self.ids.clear();
        }
        self.ids.insert(suffix.to_string(), domain);
        Ok((pack_local(local), domain))
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
            "select id, subject, subject_part, subject_mid, expected, file_total, seen from files
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
        "update files set subject = ?, subject_part = ?, subject_mid = ?, expected = ?, file_total = ?, seen = ?
         where id = ?",
    )?
    .execute(params![f.subject, f.subject_part, f.subject_mid, f.expected, f.file_total, f.seen, f.id])?;
    Ok(())
}

/// Add one article to a file; false when it was already there.
pub(crate) fn add_segment(conn: &Connection, domains: &mut Domains, file_id: i64, a: &Article) -> Result<bool> {
    let (local, domain) = domains.encode(conn, &a.message_id)?;
    // an article saved before its domain got a row is stored whole: dont save it twice
    if domain != 0 {
        let stored_whole = conn
            .prepare_cached("select 1 from segments where file_id = ? and local = ? and domain = 0")?
            .exists(params![file_id, whole(&a.message_id)])?;
        if stored_whole {
            return Ok(false);
        }
    }
    let added = conn
        .prepare_cached("insert or ignore into segments (file_id, local, domain, part, bytes) values (?, ?, ?, ?, ?)")?
        .execute(params![file_id, local, domain, a.part, a.bytes])?;
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

// ---------------------------------------------------------------- saving

/// Saves releases into one shard: upsert the release, add its new articles,
/// update its size / parts / completeness. One per shard connection.
pub struct ShardWriter {
    pub shard: usize,
    domains: Domains,
}

impl ShardWriter {
    pub fn new(shard: usize) -> ShardWriter {
        ShardWriter { shard, domains: Domains::default() }
    }

    /// Releases of this shard's groups, in one transaction. The same release
    /// may come up in more than one batch.
    pub fn save<'a>(
        &mut self,
        conn: &mut Connection,
        ids: &Ids,
        batches: impl IntoIterator<Item = &'a [Release]>,
    ) -> Result<()> {
        let shard = self.shard;
        let domains = &mut self.domains;
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
                        if add_segment(&tx, domains, f.id, a)? {
                            f.add(a);
                            added.push(a);
                        }
                    }
                    if added.len() > before {
                        put_file(&tx, &f)?;
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
        let mut conn = db::open_at(&shard_path(main, shard))?;
        ShardWriter::new(shard).save(&mut conn, &ids, [list.as_slice()])?;
    }
    Ok(())
}

// ---------------------------------------------------------------- reading

/// A release's articles, ordered by filename then part (ties by message-id),
/// for building its NZB. `conn` has the shards attached.
pub fn articles(conn: &Connection, release_id: i64) -> Result<Vec<ArticleRow>> {
    let s = shard_of_id(release_id);
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
    rows.sort_by(|a, b| (&a.filename, a.part, &a.message_id).cmp(&(&b.filename, b.part, &b.message_id)));
    Ok(rows)
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
        let conn = db::open_at(&path)?;
        let removed: i64 = conn.query_row("select count(*) from releases where complete = 0", [], |r| r.get(0))?;
        conn.execute_batch(
            "begin;
             delete from segments where file_id in
                (select f.id from files f join releases r on r.id = f.release_id where r.complete = 0);
             delete from files where release_id in (select id from releases where complete = 0);
             delete from releases where complete = 0;
             commit;",
        )?;
        let articles: i64 = conn.query_row("select count(*) from segments", [], |r| r.get(0))?;
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
        assert!(add_segment(&conn, &mut fresh, file.id, &article).unwrap(), "first time: stored whole");
        for n in 0..SHARED_AFTER {
            fresh.encode(&conn, &format!("<{n:032x}@nyuu>")).unwrap();
        }
        assert!(conn.prepare("select 1 from domains where suffix = '@nyuu>'").unwrap().exists([]).unwrap());
        assert!(!add_segment(&conn, &mut fresh, file.id, &article).unwrap(), "same article again: not saved twice");
        let segments: i64 = conn.query_row("select count(*) from segments", [], |r| r.get(0)).unwrap();
        assert_eq!(segments, 1);
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
}
