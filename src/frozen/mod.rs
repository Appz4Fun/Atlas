//! Releases set aside: small complete ones and those outside Movies, TV and Other (`rules`), moved whole out of their shard into the shard's frozen file
//! (`atlas.s3.frozen.db` next to `atlas.s3.db`), where search, the word index and both processing lanes never look.
//! Nothing is lost: a frozen release is one row holding every row it had (`payload`), a deep search still finds it,
//! its NZB still builds, and a thaw puts it back as it was.
//!
//! Only a shard's writer has the frozen file attached (as `frozen`). Freezing is two transactions, the frozen copy
//! committed before the live rows go: SQLite commits an attached file after the main one, soo a single transaction
//! could lose a release to a crash in between. A crash between the two leaves both copies, and the next tick copies
//! again and deletes. A thaw is one transaction, live rows first: a crash leaves both, and the live one wins.
//!
//! A post's late articles join the release they belong to: a save looks every incoming name and collection key up in
//! the frozen file and thaws what it finds before saving (`thaw_incoming`).

pub mod payload;
pub mod rules;

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering::Relaxed;
use std::sync::atomic::{AtomicBool, AtomicI64};
use std::time::{Duration, Instant};

use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension, Result, params};

use crate::postprocess::schema::{SHARD_TABLES, ShardTable};
use crate::search::ReleaseRow;
use payload::{Payload, Table};

/// releases queued for freezing by `rules`, moved by the writer's `Ticker`
pub const FREEZE_PENDING: ShardTable = ShardTable {
    name: "freeze_pending",
    create: "create table if not exists freeze_pending (
        release_id INTEGER PRIMARY KEY,
        reason     TEXT NOT NULL,
        at         INTEGER NOT NULL
    )",
    indexes: &[("freeze_pending_at", "create index if not exists freeze_pending_at on freeze_pending(at)")],
    release_key: Some("release_id"),
    added: &[],
};

/// releases thawed by hand: no rule freezes them again
pub const FREEZE_EXEMPT: ShardTable = ShardTable {
    name: "freeze_exempt",
    create: "create table if not exists freeze_exempt (release_id INTEGER PRIMARY KEY, at INTEGER NOT NULL)",
    indexes: &[],
    release_key: Some("release_id"),
    added: &[],
};

/// `[freeze] enabled`: rules queue and the writer moves. Off, nothing new freezes; late articles still thaw.
pub static ENABLED: AtomicBool = AtomicBool::new(true);
/// `[freeze] small_bytes`: a complete release under this freezes, 0 turns the rule off
pub static SMALL_BYTES: AtomicI64 = AtomicI64::new(1 << 30);
/// `[freeze] small_keeps_tv_movies`: the small rule leaves movies and TV alone
pub static SMALL_KEEPS_TV_MOVIES: AtomicBool = AtomicBool::new(false);
/// `[freeze] sidecar`: a post of only an NFO, mediainfo or other text, SFV or SRR files freezes (`rules::SIDECAR`)
pub static SIDECAR: AtomicBool = AtomicBool::new(true);
/// `[freeze] not_tv_movies`: a release categorized outside Movies, TV and Other freezes, complete or not
pub static NOT_TV_MOVIES: AtomicBool = AtomicBool::new(true);
/// `[freeze] per_tick`: releases a writer moves at most a tick
pub static PER_TICK: AtomicI64 = AtomicI64::new(200);
/// `[freeze] backlog_per_tick`: releases the backlog walk looks at a tick, 0 turns it off
pub static BACKLOG_PER_TICK: AtomicI64 = AtomicI64::new(2_000);
/// `[freeze] tick_ms`: how long a tick may hold the writer
pub static TICK_MS: AtomicI64 = AtomicI64::new(50);
/// `[freeze] idle_tick_ms`: how long a tick may take when the writer has nothing waiting (`Ticker::tick_idle`), 0 for
/// no such ticks
pub static IDLE_TICK_MS: AtomicI64 = AtomicI64::new(200);
/// `[freeze] min_free_gb`, in bytes: below this much free disk nothing is moved
pub static MIN_FREE_BYTES: AtomicI64 = AtomicI64::new(100 << 30);
/// `[freeze] wait_hours`, in seconds: how long a release waits queued before it moves, soo late articles and PreDB
/// titles have their time. A visit that finds it matching for the same reason keeps the time it was queued (see
/// `rules::evaluate`); a new reason starts the wait over.
pub static WAIT_SECS: AtomicI64 = AtomicI64::new(12 * 3600);

/// releases the backlog walk looks at in a tick whose moves took all its budget
const WALK_AT_LEAST: usize = 25;
/// a writer ticks at most this often, soo saves keep the writer
const TICK_EVERY: Duration = Duration::from_secs(1);
/// and at most this often when it has nothing waiting: a save that comes meanwhile waits `IDLE_TICK_MS` at most
pub const IDLE_TICK_EVERY: Duration = Duration::from_millis(250);

/// Whether idle ticks are on: a writer with nothing to do wakes for them (`IDLE_TICK_EVERY`).
pub fn idle_ticks_on() -> bool {
    ENABLED.load(Relaxed) && IDLE_TICK_MS.load(Relaxed) > 0 && !STOPPING.load(Relaxed)
}
/// How long a tick waits for another connection's write lock on the shard before it lets the tick go: the writer's
/// own wait is far longer, and freezing can always come back next tick
const TICK_BUSY: Duration = Duration::from_secs(2);
/// what the writer's connection waits otherwise (`db::open_shard`)
const WRITER_BUSY: Duration = Duration::from_secs(30);
/// The indexer is stopping: writers do no freezing, soo their queues of saves drain as fast as they did before it.
pub static STOPPING: AtomicBool = AtomicBool::new(false);
/// how long a free disk reading is trusted
const DISK_EVERY: Duration = Duration::from_secs(60);

pub fn configure(cfg: &crate::config::FreezeConfig) {
    ENABLED.store(cfg.enabled, Relaxed);
    SMALL_BYTES.store(cfg.small_bytes.max(0), Relaxed);
    SMALL_KEEPS_TV_MOVIES.store(cfg.small_keeps_tv_movies, Relaxed);
    SIDECAR.store(cfg.sidecar, Relaxed);
    NOT_TV_MOVIES.store(cfg.not_tv_movies, Relaxed);
    PER_TICK.store(cfg.per_tick.max(1), Relaxed);
    BACKLOG_PER_TICK.store(cfg.backlog_per_tick.max(0), Relaxed);
    TICK_MS.store(cfg.tick_ms.max(1), Relaxed);
    IDLE_TICK_MS.store(cfg.idle_tick_ms.max(0), Relaxed);
    MIN_FREE_BYTES.store(cfg.min_free_gb.max(0).saturating_mul(1 << 30), Relaxed);
    WAIT_SECS.store(cfg.wait_hours.max(0).saturating_mul(3600), Relaxed);
}

/// `atlas.s3.db` -> `atlas.s3.frozen.db`
pub fn path(shard: &Path) -> PathBuf {
    crate::compact::with_suffix(shard, "frozen")
}

/// shard `shard`'s frozen file, from the main database's path
pub fn path_of(main: &Path, shard: usize) -> PathBuf {
    path(&crate::store::shard_path(main, shard))
}

const SCHEMA: &str = "
    create table if not exists frozen_releases (
        id           INTEGER PRIMARY KEY,
        name         TEXT,
        group_name   TEXT,
        display_name TEXT,
        poster       TEXT,
        posted_date  TEXT,
        size         INTEGER,
        parts        INTEGER,
        complete     INTEGER,
        category     INTEGER,
        reason       TEXT NOT NULL,
        frozen_at    INTEGER NOT NULL,
        payload      BLOB NOT NULL
    );
    create unique index if not exists frozen_key on frozen_releases(name, group_name);
    create table if not exists frozen_collections (key INTEGER PRIMARY KEY, release_id INTEGER NOT NULL) without rowid;
    create index if not exists frozen_collections_release on frozen_collections(release_id);
    create virtual table if not exists frozen_fts
        using fts5(name, display_name, content='frozen_releases', content_rowid='id');
    create trigger if not exists frozen_ai after insert on frozen_releases begin
        insert into frozen_fts(rowid, name, display_name) values (new.id, new.name, new.display_name);
    end;
    create trigger if not exists frozen_ad after delete on frozen_releases begin
        insert into frozen_fts(frozen_fts, rowid, name, display_name)
            values ('delete', old.id, old.name, old.display_name);
    end;";

/// The frozen file at `path` and its tables, if missing.
pub fn create(path: &Path) -> Result<()> {
    let conn = crate::db::create_at(path)?;
    conn.query_row("pragma journal_mode = wal", [], |_| Ok(()))?;
    conn.execute_batch(SCHEMA)?;
    // the articles it held (`Summary::articles`), for the shard's total on thawing. None: frozen before
    // reposts were counted once, when `parts` was the same
    let has: bool =
        conn.prepare("select 1 from pragma_table_info('frozen_releases') where name = 'articles'")?.exists([])?;
    if !has {
        conn.execute_batch("alter table frozen_releases add column articles INTEGER")?;
    }
    Ok(())
}

/// Attach shard `shard_path`'s frozen file to `conn` (the shard's writer) as `frozen`, made if missing.
pub fn attach(conn: &Connection, shard_path: &Path) -> Result<()> {
    if attached(conn)? {
        return Ok(());
    }
    let frozen = path(shard_path);
    create(&frozen)?;
    conn.execute("attach database ?1 as frozen", [frozen.to_string_lossy()])?;
    Ok(())
}

/// Whether `conn` has a frozen file attached.
pub fn attached(conn: &Connection) -> Result<bool> {
    conn.prepare_cached("select exists (select 1 from pragma_database_list where name = 'frozen')")?
        .query_row([], |r| r.get(0))
}

/// Whether the attached frozen file holds any release.
pub fn any(conn: &Connection) -> Result<bool> {
    conn.prepare_cached("select exists (select 1 from frozen.frozen_releases)")?.query_row([], |r| r.get(0))
}

// ---------------------------------------------------------------- freezing

/// What a frozen row says of its release, beside its payload.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Summary {
    pub id: i64,
    pub name: Option<String>,
    pub group_name: Option<String>,
    pub display_name: Option<String>,
    pub poster: Option<String>,
    pub posted_date: Option<String>,
    pub size: Option<i64>,
    pub parts: Option<i64>,
    pub complete: Option<i64>,
    pub category: Option<i64>,
    /// the articles it holds, its blobs' and rows': the shard's article total counts each, where `parts` counts a
    /// part posted again under another message-id once
    pub articles: i64,
}

/// The tables a release's rows are in, besides `releases`, `files` and what hangs off a file: every registered table
/// keyed by release, but for what only makes sense live (the queue, freezing's own tables).
fn release_tables() -> impl Iterator<Item = (&'static str, &'static str)> {
    const LIVE_ONLY: &[&str] = &["pp_queue", "freeze_pending", "freeze_exempt"];
    SHARD_TABLES.iter().filter(|t| !LIVE_ONLY.contains(&t.name)).filter_map(|t| t.release_key.map(|key| (t.name, key)))
}

/// the tables keyed by a file: their `file_id` is the file's id in `files`
const FILE_TABLES: &[&str] = &["segments", "held_back"];

/// Every table a payload may bring back. Anything else in one is left out.
fn restorable(name: &str) -> bool {
    matches!(name, "releases" | "files" | "collections")
        || FILE_TABLES.contains(&name)
        || release_tables().any(|(t, _)| t == name)
}

fn read_table(conn: &Connection, name: &str, sql: &str, key: i64, into: &mut Option<Table>) -> Result<()> {
    let mut stmt = conn.prepare_cached(sql)?;
    let columns: Vec<String> = stmt.column_names().iter().map(|c| c.to_string()).collect();
    let n = columns.len();
    let table = into.get_or_insert_with(|| Table::new(name, columns));
    let mut rows = stmt.query([key])?;
    while let Some(r) = rows.next()? {
        table.rows.push((0..n).map(|i| r.get::<_, Value>(i)).collect::<Result<_>>()?);
    }
    Ok(())
}

fn int(v: Option<&Value>) -> Option<i64> {
    match v {
        Some(Value::Integer(i)) => Some(*i),
        _ => None,
    }
}

fn text(v: Option<&Value>) -> Option<String> {
    match v {
        Some(Value::Text(s)) => Some(s.clone()),
        _ => None,
    }
}

/// Every row release `id` has in the shard `conn` has open as main, or None when there is no such release.
pub fn collect(conn: &Connection, id: i64) -> Result<Option<(Summary, Payload)>> {
    let mut release = None;
    read_table(conn, "releases", "select * from main.releases where id = ?1", id, &mut release)?;
    let Some(release) = release.filter(|t| !t.rows.is_empty()) else { return Ok(None) };
    let mut files = None;
    read_table(conn, "files", "select * from main.files where release_id = ?1 order by id", id, &mut files)?;
    let files = files.expect("read_table always fills it");
    let file_ids: Vec<i64> = files.rows.iter().filter_map(|r| int(files.column("id").map(|c| &r[c]))).collect();

    let mut tables = vec![release, files];
    for name in FILE_TABLES {
        let mut t = None;
        let sql = format!("select * from main.{name} where file_id = ?1");
        for file in &file_ids {
            read_table(conn, name, &sql, *file, &mut t)?;
        }
        tables.extend(t.filter(|t| !t.rows.is_empty()));
    }
    let mut t = None;
    read_table(conn, "collections", "select * from main.collections where release_id = ?1", id, &mut t)?;
    tables.extend(t.filter(|t| !t.rows.is_empty()));
    for (name, key) in release_tables() {
        let mut t = None;
        read_table(conn, name, &format!("select * from main.{name} where {key} = ?1"), id, &mut t)?;
        tables.extend(t.filter(|t| !t.rows.is_empty()));
    }

    // the domains its articles use, by their text: loose rows, and blobs, fetched from the sealed log for one kept
    // there (its segments hold the shard's domain ids, which the log knows nothing of)
    let mut used: BTreeSet<i64> = BTreeSet::new();
    if let Some(segments) = tables.iter().find(|t| t.name == "segments")
        && let Some(c) = segments.column("domain")
    {
        used.extend(segments.rows.iter().filter_map(|r| int(Some(&r[c]))));
    }
    let mut articles = tables.iter().find(|t| t.name == "segments").map_or(0, |t| t.rows.len() as i64);
    let blob_col = tables[1].column("blob");
    for row in &tables[1].rows {
        if let Some(Value::Blob(b)) = blob_col.map(|c| &row[c]) {
            let segs = crate::blob::decode(b).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
            articles += segs.len() as i64;
            used.extend(segs.iter().map(|s| s.domain));
        }
    }
    let mut domains = Vec::new();
    let mut suffix = conn.prepare_cached("select suffix from main.domains where id = ?1")?;
    for d in used.into_iter().filter(|d| *d != 0) {
        if let Some(s) = suffix.query_row([d], |r| r.get::<_, String>(0)).optional()? {
            domains.push((d, s));
        }
    }

    let r = &tables[0];
    let row = &r.rows[0];
    let get = |c: &str| r.column(c).map(|i| &row[i]);
    let category = tables
        .iter()
        .find(|t| t.name == "release_category")
        .and_then(|t| t.rows.first().and_then(|row| int(t.column("category").map(|c| &row[c]))));
    let summary = Summary {
        id,
        name: text(get("name")),
        group_name: text(get("group_name")),
        display_name: text(get("display_name")),
        poster: text(get("poster")),
        posted_date: text(get("posted_date")),
        size: int(get("size")),
        parts: int(get("parts")),
        complete: int(get("complete")),
        category,
        articles,
    };
    Ok(Some((summary, Payload { domains, tables })))
}

/// What writing a frozen row came to.
#[derive(Debug, PartialEq, Eq)]
pub enum Written {
    Written,
    /// another frozen release has the name in the group: this one stays live
    NameTaken,
    /// its rows come to more than a payload holds (`payload::MAX_RAW`): it stays live
    TooBig,
}

/// The frozen row for a collected release, in place of any older copy of it. In the caller's transaction.
pub fn write(conn: &Connection, s: &Summary, p: &Payload, reason: &str, now: i64) -> Result<Written> {
    let other: Option<i64> = conn
        .prepare_cached("select id from frozen.frozen_releases where name = ?1 and group_name = ?2")?
        .query_row(params![s.name, s.group_name], |r| r.get(0))
        .optional()?;
    if other.is_some_and(|o| o != s.id) {
        return Ok(Written::NameTaken);
    }
    let Some(packed) = payload::encode(p) else { return Ok(Written::TooBig) };
    // a delete then an insert, not a replace: a replace doesnt fire the delete trigger the full text index needs
    conn.prepare_cached("delete from frozen.frozen_releases where id = ?1")?.execute([s.id])?;
    conn.prepare_cached("delete from frozen.frozen_collections where release_id = ?1")?.execute([s.id])?;
    conn.prepare_cached(
        "insert into frozen.frozen_releases (id, name, group_name, display_name, poster, posted_date, size, parts,
             complete, category, reason, frozen_at, payload, articles)
         values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
    )?
    .execute(params![
        s.id,
        s.name,
        s.group_name,
        s.display_name,
        s.poster,
        s.posted_date,
        s.size,
        s.parts,
        s.complete,
        s.category,
        reason,
        now,
        packed,
        s.articles
    ])?;
    if let Some(c) = p.table("collections")
        && let Some(k) = c.column("key")
    {
        let mut put =
            conn.prepare_cached("insert or replace into frozen.frozen_collections (key, release_id) values (?1, ?2)")?;
        for row in &c.rows {
            if let Some(key) = int(Some(&row[k])) {
                put.execute(params![key, s.id])?;
            }
        }
    }
    Ok(Written::Written)
}

/// A frozen release's live rows gone, in the caller's transaction on the shard: its articles, files, collection keys
/// and every registered row (their triggers). The shard's totals lose it.
pub fn remove_live(conn: &Connection, id: i64, articles: i64) -> Result<()> {
    conn.prepare_cached("delete from collections where release_id = ?1")?.execute([id])?;
    crate::store::delete_release_rows(conn, id)?;
    let mut less = conn.prepare_cached("update meta set value = max(value - ?1, 0) where key = ?2")?;
    less.execute(params![articles.max(0), "articles"])?;
    less.execute(params![1, "releases"])?;
    Ok(())
}

/// What one tick did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Moved {
    pub frozen: usize,
    /// gone from the shard before it was copied, or its name taken in the frozen file
    pub dropped: usize,
    /// changed between the copy and the delete: copied again next tick
    pub changed: usize,
    /// queued by the backlog walk
    pub backlog: usize,
    /// another connection held the shard's write lock: nothing done, the next tick tries again
    pub busy: bool,
}

/// A release a tick copied into the frozen file: its id, its size and parts as copied, and the articles it holds.
pub type Copied = (i64, Option<i64>, Option<i64>, i64);

/// Move up to `limit` releases queued at `ready` or before, longest queued first, for about `budget`. Two
/// transactions on `conn` (the shard's writer with its frozen file attached): the frozen rows (`copy`), then the live
/// rows gone (`drop_copied`).
pub fn freeze_some(conn: &Connection, limit: usize, budget: Duration, ready: i64, now: i64) -> Result<Moved> {
    let queued: Vec<(i64, String)> = conn
        .prepare_cached("select release_id, reason from freeze_pending where at <= ?1 order by at limit ?2")?
        .query_map(params![ready, limit as i64], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_>>()?;
    if queued.is_empty() {
        return Ok(Moved::default());
    }
    // the write lock first: a deferred transaction that reads, then writes, can't wait for it and fails as locked
    let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)?;
    let (copied, dropped) = copy(&tx, &queued, budget, now)?;
    tx.commit()?;
    // taking the write lock first: a thaw from outside the indexer (`--thaw`) can't come in between the checks and
    // the delete
    let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)?;
    let moved = drop_copied(&tx, &copied, &dropped)?;
    tx.commit()?;
    crate::metrics::names::FROZEN.inc(&[("event", "frozen")], moved.frozen as u64);
    Ok(moved)
}

/// The first step: frozen rows for `queued` (release, reason), for about `budget`. Returns what was copied, and
/// what is to come off the queue (gone from the shard, or its name taken in the frozen file).
pub fn copy(
    conn: &Connection,
    queued: &[(i64, String)],
    budget: Duration,
    now: i64,
) -> Result<(Vec<Copied>, Vec<i64>)> {
    let started = Instant::now();
    let (mut copied, mut dropped) = (Vec::new(), Vec::new());
    for (id, reason) in queued {
        if (!copied.is_empty() || !dropped.is_empty()) && started.elapsed() >= budget {
            break;
        }
        // what it is now: late articles since it was queued may have taken it out of the rules
        if !rules::still(conn, *id)? {
            dropped.push(*id);
            continue;
        }
        let collected = match collect(conn, *id) {
            Ok(c) => c,
            // a sealed log that can't be read now: it waits its turn again rather than holding up the rest
            Err(e) => {
                eprintln!("couldnt collect release {id} for freezing: {e}");
                conn.prepare_cached("update freeze_pending set at = ?2 where release_id = ?1")?
                    .execute(params![id, now])?;
                continue;
            }
        };
        match collected {
            None => dropped.push(*id),
            Some((s, p)) => match write(conn, &s, &p, reason, now)? {
                Written::Written => copied.push((*id, s.size, s.parts, s.articles)),
                Written::NameTaken => {
                    crate::metrics::names::FROZEN.inc(&[("event", "name_taken")], 1);
                    dropped.push(*id);
                }
                Written::TooBig => {
                    // it only grows: kept off the rules, soo no visit collects it again
                    conn.prepare_cached("insert or replace into main.freeze_exempt (release_id, at) values (?1, ?2)")?
                        .execute(params![id, now])?;
                    crate::metrics::names::FROZEN.inc(&[("event", "too_big")], 1);
                    dropped.push(*id);
                }
            },
        }
    }
    Ok((copied, dropped))
}

/// The second step, once the copies are committed: the live rows of each copied release gone, unless it changed
/// since (it stays queued, and the next tick copies it again), or a thaw by hand came in between (its frozen copy is
/// gone, and it stays live).
pub fn drop_copied(conn: &Connection, copied: &[Copied], dropped: &[i64]) -> Result<Moved> {
    let mut moved = Moved::default();
    for (id, size, parts, articles) in copied {
        let still_frozen: bool = conn
            .prepare_cached(
                "select exists (select 1 from frozen.frozen_releases where id = ?1)
                    and not exists (select 1 from main.freeze_exempt where release_id = ?1)",
            )?
            .query_row([id], |r| r.get(0))?;
        if !still_frozen {
            conn.prepare_cached("delete from freeze_pending where release_id = ?1")?.execute([id])?;
            moved.dropped += 1;
            continue;
        }
        let now_is: Option<(Option<i64>, Option<i64>)> = conn
            .prepare_cached("select size, parts from main.releases where id = ?1")?
            .query_row([id], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()?;
        match now_is {
            Some(n) if n == (*size, *parts) => {
                remove_live(conn, *id, *articles)?;
                moved.frozen += 1;
            }
            // gone meanwhile: the frozen copy is all there is
            None => {
                conn.prepare_cached("delete from freeze_pending where release_id = ?1")?.execute([id])?;
                moved.frozen += 1;
            }
            Some(_) => moved.changed += 1,
        }
    }
    for id in dropped {
        conn.prepare_cached("delete from freeze_pending where release_id = ?1")?.execute([id])?;
    }
    moved.dropped += dropped.len();
    Ok(moved)
}

/// How much is free on the disk holding `path`, None when that can't be told.
pub fn free_bytes(path: &Path) -> Option<u64> {
    let dir = path.parent().unwrap_or(path).canonicalize().ok()?;
    let disks = sysinfo::Disks::new_with_refreshed_list();
    disks
        .list()
        .iter()
        .filter(|d| dir.starts_with(d.mount_point()))
        .max_by_key(|d| d.mount_point().as_os_str().len())
        .map(|d| d.available_space())
}

/// What `free_bytes` last read for the disk under the database, -1 before the first reading and -2 when it couldnt
/// be read. Read on a thread of its own every `FREE_EVERY`: `sysinfo` lists every mount, network ones too, and on
/// prod that took over 4 s, which a writer used to spend every minute before its tick could go on.
static FREE: AtomicI64 = AtomicI64::new(-1);
/// how often the free disk is read, off the writers: a disk filling up stops the moves within this
const FREE_EVERY: Duration = Duration::from_secs(30);
static DISK_WATCH: std::sync::Once = std::sync::Once::new();

/// The free bytes under `path` as last read, None until there is a reading or when it couldnt be read. The first
/// call starts the reading thread for that disk.
fn free_now(path: &Path) -> Option<u64> {
    DISK_WATCH.call_once(|| {
        let path = path.to_path_buf();
        let _ = std::thread::Builder::new().name("atlas-disk-free".into()).spawn(move || {
            loop {
                FREE.store(free_bytes(&path).map_or(-2, |b| b.min(i64::MAX as u64) as i64), Relaxed);
                std::thread::sleep(FREE_EVERY);
            }
        });
    });
    u64::try_from(FREE.load(Relaxed)).ok()
}

/// A shard writer's freezing: at most a tick a second, nothing while the disk is short.
#[derive(Default)]
pub struct Ticker {
    attached: Option<bool>,
    last: Option<Instant>,
    disk: Option<(Instant, bool)>,
}

impl Ticker {
    /// Whether `conn` has its frozen file, asked once.
    pub fn attached(&mut self, conn: &Connection) -> bool {
        *self.attached.get_or_insert_with(|| attached(conn).unwrap_or(false))
    }

    fn disk_ok(&mut self, conn: &Connection) -> bool {
        if let Some((at, ok)) = self.disk
            && at.elapsed() < DISK_EVERY
        {
            return ok;
        }
        let floor = MIN_FREE_BYTES.load(Relaxed).max(0) as u64;
        if floor > 0 && FREE.load(Relaxed) == -1 {
            // the first reading isnt in yet: nothing moves, and the disk isnt short either
            if let Some(p) = conn.path() {
                free_now(Path::new(p));
            }
            return false;
        }
        // a disk that can't be read is taken as full: moving makes the files bigger before ingest refills the shard
        let ok = floor == 0 || conn.path().map(Path::new).and_then(free_now).is_some_and(|free| free >= floor);
        if !ok {
            crate::metrics::names::FROZEN.inc(&[("event", "disk_short")], 1);
        }
        // the reading is read anew each tick (it costs an atomic load); `disk` stays for a set reading in tests
        ok
    }

    /// One tick on the shard's writer, between its transactions. Another connection holding the shard's write lock
    /// past `TICK_BUSY` lets the tick go (`Moved::busy`): what is queued stays queued, the walk keeps its place, and
    /// the next tick tries again.
    pub fn tick(&mut self, conn: &Connection, now: i64) -> Result<Moved> {
        self.tick_for(conn, now, TICK_EVERY, TICK_MS.load(Relaxed).max(1))
    }

    /// A tick when the writer has nothing waiting: up to `IDLE_TICK_MS`, as often as `IDLE_TICK_EVERY`. On prod the
    /// writers sat idle over a third of the time while freezing, a 50 ms tick a second, used 5% of it: 4,500 releases a
    /// minute against a queue of 48 million, and a longer `tick_ms` would have come out of the saves.
    pub fn tick_idle(&mut self, conn: &Connection, now: i64) -> Result<Moved> {
        let ms = IDLE_TICK_MS.load(Relaxed);
        if ms <= 0 {
            return self.tick(conn, now);
        }
        self.tick_for(conn, now, IDLE_TICK_EVERY, ms.max(TICK_MS.load(Relaxed)))
    }

    fn tick_for(&mut self, conn: &Connection, now: i64, every: Duration, ms: i64) -> Result<Moved> {
        if !ENABLED.load(Relaxed)
            || STOPPING.load(Relaxed)
            || !self.attached(conn)
            || self.last.is_some_and(|t| t.elapsed() < every)
        {
            return Ok(Moved::default());
        }
        self.last = Some(Instant::now());
        conn.busy_timeout(TICK_BUSY)?;
        let done = self.tick_now(conn, now, Duration::from_millis(ms as u64));
        conn.busy_timeout(WRITER_BUSY)?;
        match done {
            Err(e) if is_busy(&e) => {
                crate::metrics::names::FROZEN.inc(&[("event", "busy")], 1);
                Ok(Moved { busy: true, ..Moved::default() })
            }
            done => done,
        }
    }

    /// Moves first, then the backlog walk with what is left of `budget`: the walk queues releases that only move 12
    /// hours on, and while millions are ready to move it went first and took the tick. On prod, with the walk started
    /// over (#188), a tick averaged 1.1 s, mostly walking, and moves fell from 4,500 a minute to 200.
    fn tick_now(&mut self, conn: &Connection, now: i64, budget: Duration) -> Result<Moved> {
        let started = Instant::now();
        let mut moved = Moved::default();
        if self.disk_ok(conn) {
            let limit = PER_TICK.load(Relaxed).max(1) as usize;
            moved = freeze_some(conn, limit, budget, now.saturating_sub(WAIT_SECS.load(Relaxed)), now)?;
        }
        // queueing takes no room: the walk goes on while the disk is short. It gets what the moves left, and a short
        // step of its own when they left nothing, soo it never stands still
        let walk = BACKLOG_PER_TICK.load(Relaxed).max(0) as usize;
        let left = budget.saturating_sub(started.elapsed());
        let (walk, walk_budget) = if left.is_zero() { (walk.min(WALK_AT_LEAST), Duration::ZERO) } else { (walk, left) };
        let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)?;
        let backlog = rules::backlog_some(&tx, walk, walk_budget, now)?.queued;
        tx.commit()?;
        crate::metrics::names::FROZEN.inc(&[("event", "backlog_queued")], backlog as u64);
        Ok(Moved { backlog, ..moved })
    }
}

/// Whether `e` is another connection holding the lock: worth trying again, not a failure.
fn is_busy(e: &rusqlite::Error) -> bool {
    matches!(e.sqlite_error_code(), Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked))
}

// ---------------------------------------------------------------- thawing

/// What a thaw came to.
#[derive(Debug, PartialEq, Eq)]
pub enum Thawed {
    Restored,
    /// the shard has it already (a crash between a freeze's two steps): the frozen copy went
    AlreadyLive,
    /// another live release has its name in its group: it stays frozen
    NameTaken,
    NotFrozen,
}

/// The column names `schema.table` has, none when there is no such table. `table` is one `restorable` allows.
fn target_columns(conn: &Connection, schema: &str, table: &str) -> Result<Vec<String>> {
    conn.prepare_cached(&format!("select name from pragma_table_info('{table}', '{schema}')"))?
        .query_map([], |r| r.get(0))?
        .collect()
}

fn quoted(cols: &[&str]) -> String {
    cols.iter().map(|c| format!("\"{}\"", c.replace('"', "\"\""))).collect::<Vec<_>>().join(", ")
}

/// Rows of `t` into `schema`, the columns both have; `fix` may change a value by its column first.
fn insert_rows(
    conn: &Connection,
    schema: &str,
    t: &Table,
    verb: &str,
    skip: &[&str],
    fix: &mut dyn FnMut(&str, Value) -> Result<Value>,
    mut after: impl FnMut(&Connection, &[Value]) -> Result<()>,
) -> Result<()> {
    let have = target_columns(conn, schema, &t.name)?;
    if have.is_empty() {
        return Ok(());
    }
    let picked: Vec<(usize, &str)> = t
        .columns
        .iter()
        .enumerate()
        .filter(|(_, c)| !skip.contains(&c.as_str()) && have.iter().any(|h| h == *c))
        .map(|(i, c)| (i, c.as_str()))
        .collect();
    if picked.is_empty() {
        return Ok(());
    }
    let names: Vec<&str> = picked.iter().map(|(_, c)| *c).collect();
    let marks = (1..=names.len()).map(|n| format!("?{n}")).collect::<Vec<_>>().join(", ");
    let sql = format!("{verb} into {schema}.{} ({}) values ({marks})", t.name, quoted(&names));
    for row in &t.rows {
        let values: Vec<Value> = picked.iter().map(|(i, c)| fix(c, row[*i].clone())).collect::<Result<_>>()?;
        conn.prepare_cached(&sql)?.execute(rusqlite::params_from_iter(values))?;
        after(conn, row)?;
    }
    Ok(())
}

/// How a restore takes a payload's message-id domains.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Domains {
    /// into a live shard: looked up by their text, and the ids in blobs changed to match (a blob in the sealed log is
    /// fetched and kept in the shard when they differ)
    Map,
    /// into a scratch copy of its own shard: with the ids they had, blobs as they are
    Keep,
}

/// A payload's rows into `schema` (`main`, the live shard, or a scratch copy that may have only some of the tables):
/// files get new ids, soo they can't meet files saved since. Returns the release's parts.
pub fn restore(conn: &Connection, schema: &str, p: &Payload, how: Domains) -> Result<i64> {
    let mut domain_of: HashMap<i64, i64> = HashMap::new();
    for (old, suffix) in &p.domains {
        if how == Domains::Keep {
            conn.prepare_cached(&format!("insert or ignore into {schema}.domains (id, suffix) values (?1, ?2)"))?
                .execute(params![old, suffix])?;
            domain_of.insert(*old, *old);
            continue;
        }
        let found: Option<i64> = conn
            .prepare_cached(&format!("select id from {schema}.domains where suffix = ?1"))?
            .query_row([suffix], |r| r.get(0))
            .optional()?;
        let new = match found {
            Some(d) => d,
            None => {
                conn.prepare_cached(&format!("insert into {schema}.domains (suffix) values (?1)"))?
                    .execute([suffix])?;
                conn.last_insert_rowid()
            }
        };
        domain_of.insert(*old, new);
    }
    let same_domains = domain_of.iter().all(|(a, b)| a == b);
    let domain = |d: i64| if d == 0 { 0 } else { domain_of.get(&d).copied().unwrap_or(d) };

    let mut parts = 0;
    if let Some(r) = p.table("releases") {
        parts = r.rows.first().and_then(|row| int(r.column("parts").map(|c| &row[c]))).unwrap_or(0);
        insert_rows(conn, schema, r, "insert", &[], &mut |_, v| Ok(v), |_, _| Ok(()))?;
    }
    let mut file_of: HashMap<i64, i64> = HashMap::new();
    if let Some(f) = p.table("files") {
        let id_col = f.column("id");
        insert_rows(
            conn,
            schema,
            f,
            "insert",
            &["id"],
            &mut |c, v| match (c, v) {
                ("blob", Value::Blob(b)) if !same_domains => {
                    let mut segs =
                        crate::blob::decode(&b).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
                    for s in &mut segs {
                        s.domain = domain(s.domain);
                    }
                    Ok(Value::Blob(crate::blob::encode(&segs)))
                }
                (_, v) => Ok(v),
            },
            |conn, row| {
                if let Some(old) = int(id_col.map(|c| &row[c])) {
                    file_of.insert(old, conn.last_insert_rowid());
                }
                Ok(())
            },
        )?;
    }
    let file = |v: Value| match v {
        Value::Integer(old) => Value::Integer(file_of.get(&old).copied().unwrap_or(old)),
        v => v,
    };
    for t in p.tables.iter().filter(|t| restorable(&t.name) && t.name != "releases" && t.name != "files") {
        let verb = if t.name == "segments" { "insert or ignore" } else { "insert or replace" };
        insert_rows(
            conn,
            schema,
            t,
            verb,
            &[],
            &mut |c, v| {
                Ok(match (t.name.as_str(), c) {
                    ("segments", "domain") => match v {
                        Value::Integer(d) => Value::Integer(domain(d)),
                        v => v,
                    },
                    (_, "file_id") if t.name != "par2_files" => file(v),
                    _ => v,
                })
            },
            |_, _| Ok(()),
        )?;
    }
    Ok(parts)
}

/// Put frozen release `id` back into the live shard, in the caller's transaction on the shard's writer (live rows
/// first, then the frozen row goes). `exempt`: thawed by hand, soo no rule freezes it again.
pub fn thaw(conn: &Connection, id: i64, exempt: bool, now: i64) -> Result<Thawed> {
    type Row = (Option<String>, Option<String>, Vec<u8>, Option<i64>);
    let row: Option<Row> = conn
        .prepare_cached("select name, group_name, payload, articles from frozen.frozen_releases where id = ?1")?
        .query_row([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .optional()?;
    let Some((name, group, packed, articles)) = row else { return Ok(Thawed::NotFrozen) };
    let live: bool = conn
        .prepare_cached("select exists (select 1 from main.releases where id = ?1)")?
        .query_row([id], |r| r.get(0))?;
    let outcome = if live {
        Thawed::AlreadyLive
    } else {
        let taken: bool = conn
            .prepare_cached("select exists (select 1 from main.releases where name = ?1 and group_name = ?2)")?
            .query_row(params![name, group], |r| r.get(0))?;
        if taken {
            return Ok(Thawed::NameTaken);
        }
        let p = payload::decode(&packed).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        let parts = restore(conn, "main", &p, Domains::Map)?;
        crate::store::add_totals(conn, 1, articles.unwrap_or(parts).max(0))?;
        Thawed::Restored
    };
    if exempt {
        conn.prepare_cached("insert or replace into main.freeze_exempt (release_id, at) values (?1, ?2)")?
            .execute(params![id, now])?;
    }
    conn.prepare_cached("delete from frozen.frozen_releases where id = ?1")?.execute([id])?;
    conn.prepare_cached("delete from frozen.frozen_collections where release_id = ?1")?.execute([id])?;
    crate::metrics::names::FROZEN.inc(&[("event", "thawed")], 1);
    Ok(outcome)
}

/// the most frozen releases one incoming release thaws by the names its articles carry: a save joins articles to an
/// older row only by its own name and poster, and a post naming many can't unfreeze many
const THAW_BY_ARTICLES: usize = 8;

/// The frozen releases an incoming post belongs to, the ones its save will update: by its collection key, its name,
/// and the names its articles were grouped under where the frozen row has the article's poster too (as a save joins
/// them to an older row), `THAW_BY_ARTICLES` of those at most. Each is thawed, in a savepoint of its own: one that
/// can't be (a payload that wont read) stays frozen, and the post is saved as new rather than the save failing. Each
/// thawed one is queued again if a rule still matches it, soo one the save leaves as it was goes back after its wait.
/// In the save's transaction.
pub fn thaw_incoming(
    conn: &Connection,
    key: Option<i64>,
    name: &str,
    articles: &[(&str, &str)],
    group: &str,
    now: i64,
) -> Result<usize> {
    let mut ids: BTreeSet<i64> = BTreeSet::new();
    if let Some(k) = key
        && let Some(id) = conn
            .prepare_cached("select release_id from frozen.frozen_collections where key = ?1")?
            .query_row([k], |r| r.get(0))
            .optional()?
    {
        ids.insert(id);
    }
    if let Some(id) = conn
        .prepare_cached("select id from frozen.frozen_releases where name = ?1 and group_name = ?2")?
        .query_row(params![name, group], |r| r.get(0))
        .optional()?
    {
        ids.insert(id);
    }
    let mut seen: HashSet<(&str, &str)> = HashSet::new();
    let mut by_article = conn
        .prepare_cached("select id from frozen.frozen_releases where name = ?1 and group_name = ?2 and poster = ?3")?;
    let mut by_articles = 0;
    for (name, poster) in articles.iter().filter(|a| seen.insert(**a)) {
        if by_articles >= THAW_BY_ARTICLES {
            break;
        }
        if let Some(id) = by_article.query_row(params![name, group, poster], |r| r.get(0)).optional()?
            && ids.insert(id)
        {
            by_articles += 1;
        }
    }
    let mut thawed = 0;
    for id in ids {
        conn.execute_batch("savepoint thaw")?;
        match thaw(conn, id, false, now).and_then(|t| rules::evaluate(conn, id, now).map(|_| t)) {
            Ok(_) => {
                conn.execute_batch("release thaw")?;
                thawed += 1;
            }
            Err(e) => {
                conn.execute_batch("rollback to thaw; release thaw")?;
                eprintln!("couldnt thaw frozen release {id} for its late articles: {e}");
                crate::metrics::names::FROZEN.inc(&[("event", "thaw_failed")], 1);
            }
        }
    }
    Ok(thawed)
}

// ---------------------------------------------------------------- reading, from any process

/// Shard `shard`'s frozen file, read only; None when there is none.
pub fn open_read(main: &Path, shard: usize) -> Option<Connection> {
    use rusqlite::OpenFlags;
    let p = path_of(main, shard);
    if !p.exists() {
        return None;
    }
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = Connection::open_with_flags(&p, flags).ok()?;
    conn.busy_timeout(Duration::from_secs(10)).ok()?;
    Some(conn)
}

/// A frozen release as a search lists it, with its category.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    pub row: ReleaseRow,
    pub category: Option<i64>,
    pub reason: String,
}

/// Frozen release `id` as a search would list it, and when it was frozen. None: it isn't frozen, or its group is one
/// the filters hide (as a search and a live release's page leave it out).
pub fn get(main: &Path, id: i64) -> Result<Option<(Hit, i64)>> {
    let Some(conn) = open_read(main, crate::store::shard_of_id(id)) else { return Ok(None) };
    let found = conn
        .query_row(
            "select f.id, coalesce(f.display_name, f.name), f.group_name, f.poster, f.posted_date, f.size, f.complete,
             f.parts, f.category, f.reason, f.frozen_at from frozen_releases f where f.id = ?1",
            [id],
            |r| Ok((Hit { row: crate::search::release_row(r)?, category: r.get(8)?, reason: r.get(9)? }, r.get(10)?)),
        )
        .optional()?;
    Ok(found.filter(|(h, _)| !crate::filters::group_blocked(&h.row.group_name)))
}

/// What a deep search narrows frozen releases to besides its words and category: the search page's fields and its
/// "Hide incomplete". Empty: no narrowing.
#[derive(Clone, Debug, Default)]
pub struct Narrow {
    pub group: String,
    pub poster: String,
    /// part of the poster
    pub email: String,
    /// smallest and largest size, bytes
    pub size: Option<(i64, i64)>,
    /// the release group at the end of the name
    pub team: String,
    pub complete_only: bool,
}

/// The newest frozen releases matching `words` (all of them when empty) in `filter`, narrowed by `narrow`, `limit` at
/// most over every shard, newest first.
pub fn search(
    main: &Path,
    words: &str,
    filter: Option<&crate::category::CatFilter>,
    narrow: &Narrow,
    limit: usize,
) -> Result<Vec<Hit>> {
    use rusqlite::types::Value;
    let mut hits = Vec::new();
    let fts = crate::search::fts_query(words);
    let mut values: Vec<Value> = vec![Value::Integer(limit as i64)];
    if !fts.trim().is_empty() {
        values.push(Value::Text(fts.clone()));
    }
    let mut cond = String::new();
    if let Some(f) = filter {
        let mut parts: Vec<String> = Vec::new();
        if !f.leaves.is_empty() {
            let leaves: Vec<String> = f.leaves.iter().map(|l| l.to_string()).collect();
            parts.push(format!("f.category in ({})", leaves.join(",")));
        }
        if f.uncategorized {
            parts.push("f.category is null".into());
        }
        cond.push_str(&if parts.is_empty() { " and 0".to_string() } else { format!(" and ({})", parts.join(" or ")) });
    }
    let mut add = |condition: String, value: Value| {
        values.push(value);
        cond.push_str(&format!(" and {}", condition.replace("{}", &format!("?{}", values.len()))));
    };
    if !narrow.group.is_empty() {
        add("f.group_name = {}".into(), Value::Text(narrow.group.clone()));
    }
    if !narrow.poster.is_empty() {
        add("f.poster = {}".into(), Value::Text(narrow.poster.clone()));
    }
    if !narrow.email.is_empty() {
        add(r"f.poster like {} escape '\'".into(), Value::Text(crate::search::like_query(&narrow.email)));
    }
    if let Some((low, high)) = narrow.size {
        add("f.size >= {}".into(), Value::Integer(low));
        add("f.size <= {}".into(), Value::Integer(high));
    }
    if !narrow.team.is_empty() {
        add(
            crate::field_search::team_condition("coalesce(f.display_name, f.name)", "{}"),
            Value::Text(crate::field_search::team_pattern(&narrow.team)),
        );
    }
    if narrow.complete_only {
        cond.push_str(" and f.complete = 1");
    }
    let cols = "f.id, coalesce(f.display_name, f.name), f.group_name, f.poster, f.posted_date, f.size, f.complete,
                f.parts, f.category, f.reason";
    let sql = if fts.trim().is_empty() {
        format!("select {cols} from frozen_releases f where 1 {cond} order by f.id desc limit ?1")
    } else {
        format!(
            "select {cols} from frozen_fts join frozen_releases f on f.id = frozen_fts.rowid
             where frozen_fts match ?2 {cond} order by f.id desc limit ?1"
        )
    };
    for shard in 0..crate::store::SHARDS {
        let Some(conn) = open_read(main, shard) else { continue };
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(values.iter()), |r| {
                Ok(Hit { row: crate::search::release_row(r)?, category: r.get(8)?, reason: r.get(9)? })
            })?
            .collect::<Result<Vec<_>>>()?;
        hits.extend(rows.into_iter().filter(|h| !crate::filters::group_blocked(&h.row.group_name)));
    }
    hits.sort_by_key(|h| std::cmp::Reverse(h.row.id));
    hits.truncate(limit);
    Ok(hits)
}

/// A frozen release's payload, from its shard's frozen file.
fn read_payload(main: &Path, id: i64) -> Result<Option<Payload>> {
    let Some(conn) = open_read(main, crate::store::shard_of_id(id)) else { return Ok(None) };
    let packed: Option<Vec<u8>> =
        conn.query_row("select payload from frozen_releases where id = ?1", [id], |r| r.get(0)).optional()?;
    packed.map(|b| payload::decode(&b).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))).transpose()
}

/// The tables a scratch copy needs to read a release's articles back the usual way (`store::articles`).
const SCRATCH: &str = "
    create table {s}.releases (id INTEGER PRIMARY KEY, name TEXT, group_name TEXT, poster TEXT, posted_date TEXT,
        size INTEGER, complete INTEGER, parts INTEGER, file_total INTEGER, display_name TEXT, is_obfuscated INTEGER);
    create table {s}.files (id INTEGER PRIMARY KEY, release_id INTEGER NOT NULL, filename TEXT NOT NULL, subject TEXT,
        subject_part INTEGER, subject_mid TEXT, expected INTEGER, file_total INTEGER, seen BLOB NOT NULL,
        touched_at INTEGER, blob BLOB);
    create table {s}.domains (id INTEGER PRIMARY KEY, suffix TEXT NOT NULL UNIQUE);
    create table {s}.segments (file_id INTEGER NOT NULL, local BLOB NOT NULL, domain INTEGER NOT NULL, part INTEGER,
        bytes INTEGER, primary key (file_id, local, domain)) without rowid;";

/// A frozen release and its NZB, built from its frozen row without thawing it. None: no such frozen release.
pub fn nzb(main: &Path, id: i64) -> anyhow::Result<Option<(ReleaseRow, crate::nzb::NzbBuild)>> {
    let Some(p) = read_payload(main, id)? else { return Ok(None) };
    let s = format!("s{}", crate::store::shard_of_id(id));
    let conn = Connection::open_in_memory()?;
    conn.execute("attach database ':memory:' as ?1", [&s])?;
    conn.execute_batch(&SCRATCH.replace("{s}", &s))?;
    restore(&conn, &s, &p, Domains::Keep)?;
    let Some(release) = crate::search::get_release_with(&conn, id)? else { return Ok(None) };
    let articles = crate::store::articles(&conn, id)?;
    if articles.is_empty() {
        return Ok(None);
    }
    // checked like a live release's: freezing keeps incomplete releases too
    let built = crate::nzb::render_checked(&release, &articles, std::slice::from_ref(&release.group_name), true);
    crate::nzb::note_gaps(&release, &built);
    Ok(Some((release, built)))
}

/// Shard `shard`'s frozen releases back in a shard of their own (`atlas.s3.unfrozen.db`, made afresh), for what reads
/// a shard's tables: a reset harvests their enrichments from it. Blobs stay as they are. None: no frozen file. A
/// payload that won't read is an error, soo a reset stops rather than lose what it holds.
pub fn unfrozen_copy(main: &Path, shard: usize) -> anyhow::Result<Option<PathBuf>> {
    // only a file that isn't there is nothing to harvest: one that won't open stops the reset
    let p = path_of(main, shard);
    if !p.try_exists()? {
        return Ok(None);
    }
    let frozen = Connection::open_with_flags(&p, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    frozen.busy_timeout(Duration::from_secs(10))?;
    let copy = crate::compact::with_suffix(&crate::store::shard_path(main, shard), "unfrozen");
    remove_family(&copy)?;
    crate::store::create_shard(&copy)?;
    let conn = crate::db::open_shard(&copy)?;
    crate::postprocess::schema::migrate(&conn)?;
    let tx = conn.unchecked_transaction()?;
    let mut stmt = frozen.prepare("select id, payload from frozen_releases order by id")?;
    let mut rows = stmt.query([])?;
    while let Some(r) = rows.next()? {
        let (id, packed): (i64, Vec<u8>) = (r.get(0)?, r.get(1)?);
        let p = payload::decode(&packed).map_err(|e| anyhow::anyhow!("frozen release {id}: {e}"))?;
        restore(&tx, "main", &p, Domains::Keep)?;
    }
    tx.commit()?;
    Ok(Some(copy))
}

/// A database file and its journals gone.
pub fn remove_family(base: &Path) -> std::io::Result<()> {
    for suffix in ["", "-wal", "-shm", "-journal"] {
        match std::fs::remove_file(format!("{}{suffix}", base.display())) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
    }
    Ok(())
}

/// Frozen releases per shard and reason.
pub fn stats(main: &Path) -> Result<Vec<(usize, String, i64, i64)>> {
    let mut out = Vec::new();
    for shard in 0..crate::store::SHARDS {
        let Some(conn) = open_read(main, shard) else { continue };
        let mut stmt = conn.prepare(
            "select reason, count(*), coalesce(sum(size), 0) from frozen_releases group by reason order by reason",
        )?;
        let rows = stmt.query_map([], |r| Ok((shard, r.get(0)?, r.get(1)?, r.get(2)?)))?;
        out.extend(rows.collect::<Result<Vec<_>>>()?);
    }
    Ok(out)
}

/// Thaw frozen release `id` by hand, from outside the indexer (the CLI, the web UI): kept off the rules after. The
/// caller holds off compaction (`compact::hold_off_compaction`), soo a shard swapped in under it can't lose it.
pub fn thaw_by_hand(main: &Path, id: i64) -> Result<Thawed> {
    let shard = crate::store::shard_path(main, crate::store::shard_of_id(id));
    let conn = crate::db::open_shard(&shard)?;
    attach(&conn, &shard)?;
    // like the shard's writer: the live row synced at commit like the frozen file's delete, and the commit not
    // copying whatever waits in the shard's WAL (`db::OCCASIONAL_WRITER_AUTOCHECKPOINT`)
    conn.execute_batch(&format!(
        "pragma main.synchronous = full; pragma wal_autocheckpoint = {}",
        crate::db::OCCASIONAL_WRITER_AUTOCHECKPOINT
    ))?;
    conn.execute_batch("begin immediate")?;
    match thaw(&conn, id, true, chrono::Utc::now().timestamp()) {
        Ok(t) => {
            conn.execute_batch("commit")?;
            Ok(t)
        }
        Err(e) => {
            let _ = conn.execute_batch("rollback");
            Err(e)
        }
    }
}

/// `--thaw ID...`: put frozen releases back, each in one transaction on its shard alongside a running indexer.
pub fn run_thaw(args: &[String]) -> i32 {
    let main = crate::paths::database();
    let ids: Vec<i64> = args.iter().filter_map(|a| a.parse().ok()).collect();
    if ids.is_empty() {
        println!("usage: atlas --thaw RELEASE_ID...");
        return 1;
    }
    // a compaction swapping a shard in under a thaw would lose what it restored
    let _writing = match crate::compact::hold_off_compaction(&main) {
        Ok(held) => held,
        Err(e) => {
            println!("{e}");
            return 1;
        }
    };
    let mut code = 0;
    for id in ids {
        match thaw_by_hand(&main, id) {
            Ok(Thawed::Restored) => println!("{id}: thawed"),
            Ok(Thawed::AlreadyLive) => println!("{id}: was live already, its frozen copy is gone"),
            Ok(Thawed::NameTaken) => {
                println!("{id}: a live release has its name in its group, it stays frozen");
                code = 1;
            }
            Ok(Thawed::NotFrozen) => {
                println!("{id}: not frozen");
                code = 1;
            }
            Err(e) => {
                println!("{id}: couldnt thaw: {e}");
                code = 1;
            }
        }
    }
    code
}

/// `--frozen-stats`: what is frozen, per shard and reason.
pub fn run_stats() -> i32 {
    match stats(&crate::paths::database()) {
        Ok(rows) if rows.is_empty() => {
            println!("nothing is frozen");
            0
        }
        Ok(rows) => {
            let (mut n, mut bytes) = (0, 0);
            for (shard, reason, count, size) in rows {
                println!("shard {shard}  {reason:<16} {count:>12} releases  {:>10.1} GB", size as f64 / 1e9);
                n += count;
                bytes += size;
            }
            println!("all              {:<16} {n:>12} releases  {:>10.1} GB", "", bytes as f64 / 1e9);
            0
        }
        Err(e) => {
            println!("couldnt read the frozen files: {e}");
            1
        }
    }
}

#[cfg(test)]
mod tests;
