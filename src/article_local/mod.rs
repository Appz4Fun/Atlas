//! The local article store: one SQLite file beside the header store holding the
//! per-article rows (message id, part, bytes) of every release. PostgreSQL stays
//! the catalogue for releases and files and hands out their ids; this store
//! only answers "which articles does this release or file have".
//!
//! The indexer is the single writer and the web process reads the same file.
//! A save writes here first and names the PostgreSQL transaction it belongs to;
//! `settle` later asks PostgreSQL how that transaction ended and takes the rows
//! back out when it rolled back, so the two never disagree after the process
//! crashes. Commits here are not synced to disk one by one (`synchronous =
//! normal` in WAL mode): an operating system crash or power loss can lose the
//! last ones while PostgreSQL kept the transactions that counted them, which
//! leaves those releases short of articles. That is the loss the store is
//! allowed (the user accepts up to a rolling day of it); every save syncing
//! here held the indexer's one PostgreSQL owner session for its fsyncs. After
//! such a loss a later save of the same articles counts them again, and the
//! rows of a release deleted meanwhile can stay behind. The catalogue's naming
//! of the store (`ensure_writer`) is synced first, so it is never lost.
//!
//! The file holds only what Usenet said, but PostgreSQL's counts are of the
//! articles in it: the catalogue names the one store it counts, and a
//! different or missing store is refused rather than counted a second time.
//!
//! The store is several SQLite files (`Store`, store.rs), each a `Shard` with its own write connection, so saves of
//! releases in different files write side by side: on prod sixteen helper writers waited about nine tenths of their
//! save time for the one write connection of a single 220 GB file. `articles.db` keeps every release it held when the
//! store was first split, and gets the later articles of those releases too; a newer release goes to one of the
//! `articles.s{n}.db` files by its id. Every row of a release, and so of each of its files, is in one file.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};

mod reads;
mod reads_meta;
mod reads_nzb;
mod reads_services;
mod reads_stage;
mod runtime;
mod store;
#[cfg(test)]
mod tests;

pub(crate) use reads::*;
pub(crate) use runtime::{ensure_reader, ensure_writer};
pub(crate) use store::Store;

pub(crate) const FILE: &str = "articles.db";

/// What a nameless article said about itself, kept only for the article that won.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Envelope {
    pub subject: Vec<u8>,
    pub poster: Vec<u8>,
    pub posted_date: Vec<u8>,
    pub nzb_timestamp: i64,
    pub expected: Option<i64>,
    pub subject_file_index: Option<i64>,
    pub subject_file_total: Option<i64>,
}

/// One accepted article on its way in.
#[derive(Clone, Debug)]
pub(crate) struct NewRow<'a> {
    pub release_id: i64,
    pub file_id: i64,
    pub message_id: &'a [u8],
    pub part: Option<i64>,
    pub bytes: i64,
    pub envelope: Option<Envelope>,
}

/// The part numbers a save added to one file that the file did not have before.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct NewParts {
    pub file_id: i64,
    pub count: i64,
    pub first: i64,
    pub last: i64,
}

/// How PostgreSQL says a transaction ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Ended {
    Committed,
    RolledBack,
    /// Still running, so its rows stay as they are.
    Running,
}

/// One SQLite file of the store, with its own write connection, readers, checkpoints, unsettled saves and `seen` fill.
pub(crate) struct Shard {
    /// A writer's background checkpoints (`Checkpointer`); stopped first when the store drops.
    checkpointer: Option<Checkpointer>,
    /// Read connections of a writer's store (`read`): in WAL mode they dont wait for the writer, and the writer's
    /// saves asked which articles a release had through the one connection their inserts held, side by side with
    /// every helper writer's. Declared before `conn`, so they close first and the writer's close checkpoints the WAL.
    readers: Mutex<Vec<Connection>>,
    conn: Mutex<Connection>,
    path: PathBuf,
    writer: bool,
    /// Transactions of this process seen to commit (`Store::confirm_later`) whose rows still wait in this file's
    /// `unsettled`: settling takes them for committed without asking, and the next save's `add` here marks them in its
    /// own transaction. Each save confirmed in a write transaction of its own, queued for the one writer behind every
    /// helper's insert, and every other save asked PostgreSQL about it meanwhile.
    confirmed: Mutex<HashSet<String>>,
    /// set by the checkpointer when the WAL should start over; the next `add` does it under the write lock
    restart_wanted: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// releases above this have every article in `seen`; below and at it `has` reads `segments` (`fill_seen`)
    seen_above: std::sync::atomic::AtomicI64,
    /// when `fill_seen` last ran
    seen_filled_at: Mutex<Option<std::time::Instant>>,
    /// a save is reading the next fill ahead (`warm_fill`): the others go straight on
    warming: std::sync::atomic::AtomicBool,
    /// saves waiting for the write lock in `add_fresh`: the fill runs only when none is (`fill_seen`)
    waiting: AtomicUsize,
}

/// Copies the WAL back into a writer store's file in the background, on a connection of its own, so the writer's
/// commits dont: with `wal_autocheckpoint` at its old 8192 pages the commit that crossed it copied pages into the 44
/// GB file and synced it holding the store's one write connection, which every helper writer's save waits for.
/// `CHECKPOINT_FALLBACK` pages still make the writer checkpoint should this fall far behind.
struct Checkpointer {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

const CHECKPOINT_EVERY: std::time::Duration = std::time::Duration::from_millis(500);
const CHECKPOINT_FALLBACK: i64 = 131_072;
/// WAL frames past which the WAL is started over (RESTART): with commits arriving all the time a PASSIVE pass never
/// catches its end, so it never starts over, and on prod it grew to `CHECKPOINT_FALLBACK`, where the writer
/// checkpointed on its own commits again. A RESTART needs the WAL's write lock free, which sixteen helpers committing
/// back to back on the one write connection almost never left it from another connection: it is run on the write
/// connection itself, by the next `add` under its lock (`restart_wanted`, `Store::restart_wal`), where no other write
/// transaction can be under way.
const CHECKPOINT_RESTART_AT: i64 = 32_768;
/// How long a RESTART waits for readers already under way (lookups hold a read transaction for `HAS_PER_READ` keys
/// at most).
const RESTART_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

impl Checkpointer {
    fn start(path: &Path, restart_wanted: std::sync::Arc<std::sync::atomic::AtomicBool>) -> Result<Self> {
        let conn = Connection::open(path).with_context(|| format!("opening {} for checkpoints", path.display()))?;
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stopping = stop.clone();
        let thread = std::thread::Builder::new().name("article-store-checkpoint".into()).spawn(move || {
            let mut failing = false;
            while !stopping.load(Ordering::Acquire) {
                std::thread::sleep(CHECKPOINT_EVERY);
                if stopping.load(Ordering::Acquire) {
                    break;
                }
                // PASSIVE: copies what no reader still needs and waits for nobody; its row says how long the WAL is
                match conn.query_row("pragma wal_checkpoint(PASSIVE)", [], |row| row.get::<_, i64>(1)) {
                    Ok(frames) => {
                        failing = false;
                        if frames > CHECKPOINT_RESTART_AT {
                            restart_wanted.store(true, Ordering::Release);
                        }
                    }
                    // said once per run of failures, not every pass
                    Err(error) if !failing => {
                        failing = true;
                        eprintln!("local article store checkpoint failed: {error}");
                    }
                    Err(_) => {}
                }
            }
        })?;
        Ok(Checkpointer { stop, thread: Some(thread) })
    }
}

impl Drop for Checkpointer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Start the WAL over (RESTART) on the write connection, under its lock: no write transaction is under way, so it
/// waits only for readers already reading (`RESTART_WAIT`). Gave up or failed: said, and asked again by the
/// checkpointer's next pass.
fn restart_wal(conn: &Connection) {
    let restarted = conn
        .busy_timeout(RESTART_WAIT)
        .and_then(|()| conn.query_row("pragma wal_checkpoint(RESTART)", [], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?, row.get::<_, i64>(2)?))));
    let _ = conn.busy_timeout(std::time::Duration::from_secs(30));
    match restarted {
        Ok((0, _, _)) => {}
        Ok((_, log, done)) => eprintln!("local article store couldnt start the WAL over: {done} of {log} frames copied, readers still on the rest"),
        Err(error) => eprintln!("local article store couldnt start the WAL over: {error}"),
    }
}

/// Keys `has` looks up in one read transaction at most: a reader holds its snapshot, and so the WAL, for as long as
/// its transaction lasts.
const HAS_PER_READ: usize = 512;

/// Read connections a writer's store keeps open when idle (`Store::read`): `has` takes up to `HAS_SPLIT` at once for
/// each of the helper writers' saves.
const READERS_KEPT: usize = 128;

/// Keys `has` looks up on one connection at most before it splits them over several, `HAS_SPLIT` at most. The store is
/// far bigger than memory, so most lookups of an existing release read the disk at random, one after another on one
/// connection: on prod `has` spent two thirds of its time in pread with the disk doing a few thousand reads a second,
/// a fraction of what it does with more requests in flight.
const HAS_ALONE: usize = 256;
const HAS_SPLIT: usize = 8;

const SCHEMA: &str = "
    create table if not exists meta (key text primary key, value text not null) without rowid;
    create table if not exists segments (
        release_id integer not null,
        message_id blob not null,
        file_id    integer not null,
        part       integer,
        bytes      integer not null,
        primary key (release_id, message_id)
    ) without rowid;
    create index if not exists segments_file_part on segments (file_id, part);
    -- What `has` asks, small enough to stay in memory: a release's articles by the first 8 bytes of their message
    -- key (`seen_hash`). Filled for the articles saved before it existed newest release first (`fill_seen`).
    create table if not exists seen (
        release_id integer not null,
        h          integer not null,
        primary key (release_id, h)
    ) without rowid;
    create table if not exists envelopes (
        release_id integer not null,
        message_id blob not null,
        subject blob not null,
        poster blob not null,
        posted_date blob not null,
        nzb_timestamp integer not null,
        expected integer,
        subject_file_index integer,
        subject_file_total integer,
        primary key (release_id, message_id)
    ) without rowid;
    -- The keys a save wrote, until PostgreSQL says how its transaction ended.
    create table if not exists unsettled (
        op integer primary key,
        xid text not null,
        keys blob not null
    );
    -- Releases a save deleted from the catalogue: their articles go once it commits.
    create table if not exists retracted (
        op integer primary key,
        xid text not null,
        release_id integer not null
    );
";

impl Shard {
    /// Open one file of the store the indexer writes, made when missing. `catalogue` names the PostgreSQL
    /// catalogue whose ids the rows carry; a file filled for another one is refused.
    fn open_writer(path: &Path, catalogue: &str) -> Result<Self> {
        let path = path.to_path_buf();
        let conn = Connection::open(&path).with_context(|| format!("opening {}", path.display()))?;
        conn.busy_timeout(std::time::Duration::from_secs(30))?;
        conn.execute_batch(&format!(
            "pragma journal_mode = wal;
             pragma synchronous = normal;
             pragma cache_size = -2097152;
             pragma wal_autocheckpoint = {CHECKPOINT_FALLBACK};
             pragma temp_store = memory;"
        ))?;
        conn.execute_batch(SCHEMA)?;
        let bound: Option<String> =
            conn.query_row("select value from meta where key = 'catalogue'", [], |row| row.get(0)).optional()?;
        match bound {
            Some(bound) => ensure!(
                bound == catalogue,
                "the local article store {} belongs to catalogue {bound}, not {catalogue}; move it away to start a new one",
                path.display()
            ),
            None => {
                conn.execute("insert into meta (key, value) values ('catalogue', ?1)", [catalogue])?;
            }
        }
        // a store from before `seen`: every release up to the newest is filled in by `fill_seen`
        let cursor: Option<i64> =
            conn.query_row("select value from meta where key = 'seen_above'", [], |row| row.get::<_, String>(0)).optional()?.map(|v| v.parse()).transpose()?;
        let seen_above = match cursor {
            Some(above) => above,
            None => {
                let newest: Option<i64> = conn.query_row("select max(release_id) from segments", [], |row| row.get(0))?;
                let above = newest.unwrap_or(i64::MIN);
                conn.execute("insert into meta (key, value) values ('seen_above', ?1)", [above.to_string()])?;
                conn.execute("delete from meta where key = 'seen_cursor'", [])?;
                above
            }
        };
        let restart_wanted = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let checkpointer = Some(Checkpointer::start(&path, restart_wanted.clone())?);

        Ok(Self {
            checkpointer,
            conn: Mutex::new(conn),
            path,
            writer: true,
            confirmed: Mutex::new(HashSet::new()),
            readers: Mutex::new(Vec::new()),
            restart_wanted,
            seen_above: std::sync::atomic::AtomicI64::new(seen_above),
            seen_filled_at: Mutex::new(None),
            warming: std::sync::atomic::AtomicBool::new(false),
            waiting: AtomicUsize::new(0),
        })
    }

    /// Open one file for reading only, as the web process does. The file must exist.
    fn open_reader(path: &Path) -> Result<Self> {
        let path = path.to_path_buf();
        let conn = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .with_context(|| format!("opening {} for reading", path.display()))?;
        conn.busy_timeout(std::time::Duration::from_secs(10))?;
        conn.execute_batch("pragma cache_size = -32768; pragma temp_store = memory;")?;
        Ok(Self {
            checkpointer: None,
            conn: Mutex::new(conn),
            path,
            writer: false,
            confirmed: Mutex::new(HashSet::new()),
            readers: Mutex::new(Vec::new()),
            restart_wanted: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            seen_above: std::sync::atomic::AtomicI64::new(i64::MAX),
            seen_filled_at: Mutex::new(None),
            warming: std::sync::atomic::AtomicBool::new(false),
            waiting: AtomicUsize::new(0),
        })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    fn catalogue(&self) -> Result<Option<String>> {
        Ok(self.lock()?.query_row("select value from meta where key = 'catalogue'", [], |row| row.get(0)).optional()?)
    }

    /// The connection, for the `reads*` modules. One statement at a time: do not hold it across PostgreSQL calls.
    /// Everything committed so far, on disk: one commit synced in full (in WAL mode that syncs every frame before it
    /// too). Commits arent otherwise synced one by one (`synchronous = normal`).
    pub(super) fn sync(&self) -> Result<()> {
        ensure!(self.writer, "the local article store is open for reading only");
        let conn = self.lock()?;
        conn.execute_batch("pragma synchronous = full;")?;
        let synced = conn.execute(
            "insert into meta (key, value) values ('synced', cast(strftime('%s', 'now') as text))
             on conflict (key) do update set value = excluded.value",
            [],
        );
        conn.execute_batch("pragma synchronous = normal;")?;
        synced?;
        Ok(())
    }

    /// Run `f` on a read connection: one of the writer store's own (see `readers`), or a reader store's connection.
    /// It sees every commit made before it starts.
    pub(super) fn read<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        if !self.writer {
            return f(&*self.lock()?);
        }
        let idle = self.readers.lock().unwrap_or_else(std::sync::PoisonError::into_inner).pop();
        let conn = match idle {
            Some(conn) => conn,
            None => {
                let conn = Connection::open_with_flags(
                    &self.path,
                    OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
                )
                .with_context(|| format!("opening {} for reading", self.path.display()))?;
                conn.busy_timeout(std::time::Duration::from_secs(30))?;
                // a reader's cache starts over whenever another connection committed, which writers do all the time
                conn.execute_batch("pragma cache_size = -4096; pragma temp_store = memory;")?;
                conn
            }
        };
        // one read transaction for the whole call: a new read after another connection's commit throws this
        // connection's page cache away, and helper writers commit all the time
        let out = match conn.unchecked_transaction() {
            Ok(tx) => {
                let out = f(&tx);
                let _ = tx.finish();
                out
            }
            Err(error) => Err(error.into()),
        };
        let mut idle = self.readers.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if idle.len() < READERS_KEPT {
            idle.push(conn);
        }
        out
    }

    pub(super) fn lock(&self) -> Result<MutexGuard<'_, Connection>> {
        self.conn.lock().map_err(|_| anyhow::anyhow!("the local article store lock is poisoned"))
    }

    /// For each `(release_id, message_id)`, whether the release already has that article.
    pub(crate) fn has(&self, keys: &[(i64, &[u8])]) -> Result<Vec<bool>> {
        if keys.is_empty() {
            return Ok(Vec::new());
        }
        let above = self.seen_above.load(Ordering::Acquire);
        let look = |keys: &[&(i64, &[u8])]| -> Result<Vec<bool>> {
            let mut found = Vec::with_capacity(keys.len());
            for keys in keys.chunks(HAS_PER_READ) {
                found.extend(self.read(|conn| {
                    keys.iter()
                        .map(|(release, id)| {
                            // prepared when used: a reader's store from before `seen` has no such table
                            Ok(if *release > above {
                                conn.prepare_cached("select 1 from seen where release_id = ?1 and h = ?2")?.exists(params![release, seen_hash(id)])?
                            } else {
                                conn.prepare_cached("select 1 from segments where release_id = ?1 and message_id = ?2")?.exists(params![release, id])?
                            })
                        })
                        .collect::<Result<Vec<bool>>>()
                })?);
            }
            Ok(found)
        };
        // in key order, so each connection walks one stretch of the index
        let mut order: Vec<usize> = (0..keys.len()).collect();
        order.sort_unstable_by(|&a, &b| keys[a].cmp(&keys[b]));
        let sorted: Vec<&(i64, &[u8])> = order.iter().map(|&i| &keys[i]).collect();
        let found: Vec<bool> = if !self.writer || keys.len() <= HAS_ALONE {
            look(&sorted)?
        } else {
            let each = keys.len().div_ceil(HAS_SPLIT.min(keys.len().div_ceil(HAS_ALONE)));
            std::thread::scope(|scope| {
                let parts: Vec<_> = sorted.chunks(each).map(|part| scope.spawn(move || look(part))).collect();
                let mut found = Vec::with_capacity(keys.len());
                for part in parts {
                    found.extend(part.join().map_err(|_| anyhow::anyhow!("a local store lookup panicked"))??);
                }
                Ok::<_, anyhow::Error>(found)
            })?
        };
        let mut answers = vec![false; keys.len()];
        for (&i, known) in order.iter().zip(found) {
            answers[i] = known;
        }
        Ok(answers)
    }

    /// Write the accepted articles of one batch, every one of a release of this file, and remember them under `xid`,
    /// the PostgreSQL transaction that counts them (see `Store::add_fresh`). Returns the parts each file gained with the
    /// place in `rows` of the row that first counted for it.
    fn add(
        &self,
        xid: &str,
        rows: &[&NewRow<'_>],
        max_part: i64,
        fresh: &HashSet<i64>,
    ) -> Result<Vec<(usize, NewParts)>> {
        ensure!(self.writer, "the local article store is open for reading only");
        if rows.is_empty() {
            return Ok(Vec::new());
        }
        // The parts the files of `rows` had already, looked up on the read connections before the write lock is
        // taken: on prod these random reads, one per article, held the lock every save waits for. Saves of one file
        // run one at a time (one release, one shard of its PostgreSQL group lock), and the save before this one
        // committed here before it let go of that lock, so a new read transaction sees its parts.
        let asked: Vec<(i64, i64)> = {
            let mut asked: Vec<(i64, i64)> = rows
                .iter()
                .filter(|row| !fresh.contains(&row.file_id))
                .filter_map(|row| row.part.filter(|part| (0..=max_part).contains(part)).map(|part| (row.file_id, part)))
                .collect();
            asked.sort_unstable();
            asked.dedup();
            asked
        };
        let mut had = HashSet::new();
        for keys in asked.chunks(HAS_PER_READ) {
            let found = self.read(|conn| {
                let mut find = conn.prepare_cached("select 1 from segments where file_id = ?1 and part = ?2 limit 1")?;
                keys.iter().map(|(file, part)| Ok(find.exists(params![file, part])?)).collect::<Result<Vec<bool>>>()
            })?;
            had.extend(keys.iter().zip(found).filter(|(_, found)| *found).map(|(key, _)| *key));
        }
        // the next fill's rows read here first, off the write lock: the fill under it then finds their pages cached
        self.warm_fill();
        self.waiting.fetch_add(1, Ordering::AcqRel);
        let locked = self.lock();
        self.waiting.fetch_sub(1, Ordering::AcqRel);
        let mut conn = locked?;
        if self.restart_wanted.swap(false, Ordering::AcqRel) {
            restart_wal(&conn);
        }
        // the fill is best effort: a save never fails for it, and it goes on from `meta` next time. Only when no other
        // save waits for this lock: it only speeds up `has`, which reads off the lock, and on prod its reads of cold
        // pages under the lock held every save back
        if self.waiting.load(Ordering::Acquire) == 0
            && let Err(error) = self.fill_seen(&mut conn)
        {
            eprintln!("local article store: couldnt fill the lookup index this time: {error:#}");
        }
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        // commits seen since the last add, marked here (`confirm_later`); should this transaction roll back they are
        // left waiting, and asked about as any unsettled transaction is
        let seen: Vec<String> = self.confirmed.lock().unwrap_or_else(std::sync::PoisonError::into_inner).iter().cloned().collect();
        for xid in &seen {
            Self::committed(&tx, xid)?;
        }
        let mut gained = HashMap::<i64, NewParts>::new();
        let mut order = Vec::new();
        {
            let mut seen = HashSet::new();
            for (at, row) in rows.iter().enumerate() {
                let Some(part) = row.part.filter(|part| (0..=max_part).contains(part)) else { continue };
                if !seen.insert((row.file_id, part)) || had.contains(&(row.file_id, part)) {
                    continue;
                }
                let entry = gained.entry(row.file_id).or_insert_with(|| {
                    order.push((at, row.file_id));
                    NewParts { file_id: row.file_id, count: 0, first: part, last: part }
                });
                entry.count += 1;
                entry.first = entry.first.min(part);
                entry.last = entry.last.max(part);
            }
            let mut insert = tx.prepare_cached(
                "insert into segments (release_id, message_id, file_id, part, bytes) values (?1, ?2, ?3, ?4, ?5)",
            )?;
            let mut mark = tx.prepare_cached("insert or ignore into seen (release_id, h) values (?1, ?2)")?;
            let mut envelope = tx.prepare_cached(
                "insert or ignore into envelopes (release_id, message_id, subject, poster, posted_date, nzb_timestamp,
                    expected, subject_file_index, subject_file_total) values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            )?;
            for row in rows {
                insert
                    .execute(params![row.release_id, row.message_id, row.file_id, row.part, row.bytes])
                    .context("an article the release already has reached the local article store")?;
                mark.execute(params![row.release_id, seen_hash(row.message_id)])?;
                if let Some(e) = &row.envelope {
                    envelope.execute(params![
                        row.release_id,
                        row.message_id,
                        e.subject,
                        e.poster,
                        e.posted_date,
                        e.nzb_timestamp,
                        e.expected,
                        e.subject_file_index,
                        e.subject_file_total
                    ])?;
                }
            }
            tx.execute("insert into unsettled (xid, keys) values (?1, ?2)", params![xid, encode_keys(rows)?])?;
        }
        tx.commit()?;
        {
            let mut confirmed = self.confirmed.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            for xid in &seen {
                confirmed.remove(xid);
            }
        }
        Ok(order.into_iter().map(|(at, file)| (at, gained[&file])).collect())
    }

    /// The caller saw PostgreSQL commit `xid`: its rows here stay and stop waiting.
    fn confirm(&self, xid: &str) -> Result<()> {
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        Self::committed(&tx, xid)?;
        Ok(tx.commit()?)
    }

    /// `xid` committed: its rows stop waiting and the releases it deleted lose their articles.
    fn committed(tx: &rusqlite::Transaction<'_>, xid: &str) -> Result<()> {
        tx.execute("delete from segments where release_id in (select release_id from retracted where xid = ?1)", [xid])?;
        tx.execute("delete from envelopes where release_id in (select release_id from retracted where xid = ?1)", [xid])?;
        tx.execute("delete from seen where release_id in (select release_id from retracted where xid = ?1)", [xid])?;
        tx.execute("delete from retracted where xid = ?1", [xid])?;
        tx.execute("delete from unsettled where xid = ?1", [xid])?;
        Ok(())
    }

    /// The save under `xid` deleted these releases, all of this file, from the catalogue. Their
    /// articles are dropped when it commits and kept if it rolls back.
    fn retract(&self, xid: &str, releases: &[i64]) -> Result<()> {
        ensure!(self.writer, "the local article store is open for reading only");
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        for release in releases {
            tx.execute("insert into retracted (xid, release_id) values (?1, ?2)", params![xid, release])?;
        }
        Ok(tx.commit()?)
    }

    /// The transactions with rows or retractions waiting in this file, but those seen to commit here
    /// (`confirmed`): the caller leaves out the ones still running.
    fn unsettled(&self) -> Result<Vec<String>> {
        let xids: Vec<String> = self.read(|conn| {
            let mut list = conn.prepare_cached("select xid from unsettled union select xid from retracted")?;
            Ok(list.query_map([], |row| row.get(0))?.collect::<rusqlite::Result<_>>()?)
        })?;
        let confirmed = self.confirmed.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        Ok(xids.into_iter().filter(|xid| !confirmed.contains(xid)).collect())
    }

    /// Settle in this file the transactions PostgreSQL said ended: rows of a committed one stay; rows of a rolled-back
    /// one are taken out again. One write transaction each.
    fn settle(&self, answers: &[(String, Ended)]) -> Result<()> {
        if answers.is_empty() {
            return Ok(());
        }
        let mut conn = self.lock()?;
        for (xid, how) in answers {
            let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            if *how == Ended::RolledBack {
                let blobs: Vec<Vec<u8>> = {
                    let mut list = tx.prepare("select keys from unsettled where xid = ?1 order by op")?;
                    list.query_map([xid], |row| row.get(0))?.collect::<rusqlite::Result<_>>()?
                };
                let mut segment = tx.prepare_cached("delete from segments where release_id = ?1 and message_id = ?2")?;
                let mut envelope = tx.prepare_cached("delete from envelopes where release_id = ?1 and message_id = ?2")?;
                let mut unmark = tx.prepare_cached("delete from seen where release_id = ?1 and h = ?2")?;
                for blob in blobs {
                    for (release, id) in decode_keys(&blob)? {
                        segment.execute(params![release, id])?;
                        envelope.execute(params![release, id])?;
                        unmark.execute(params![release, seen_hash(id)])?;
                    }
                }
                drop((segment, envelope, unmark));
                tx.execute("delete from retracted where xid = ?1", [xid])?;
                tx.execute("delete from unsettled where xid = ?1", [xid])?;
            } else {
                Self::committed(&tx, xid)?;
            }
            tx.commit()?;
        }
        Ok(())
    }

    /// Bulk load rows that PostgreSQL already counted, every one of a release of this file (see `Store::import`).
    /// Rows the file already has are skipped.
    fn import(&self, rows: &[&NewRow<'_>]) -> Result<u64> {
        ensure!(self.writer, "the local article store is open for reading only");
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let mut added = 0;
        {
            let mut insert = tx.prepare_cached(
                "insert or ignore into segments (release_id, message_id, file_id, part, bytes) values (?1, ?2, ?3, ?4, ?5)",
            )?;
            let mut mark = tx.prepare_cached("insert or ignore into seen (release_id, h) values (?1, ?2)")?;
            let mut envelope = tx.prepare_cached(
                "insert or ignore into envelopes (release_id, message_id, subject, poster, posted_date, nzb_timestamp,
                    expected, subject_file_index, subject_file_total) values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            )?;
            for row in rows {
                added += insert.execute(params![row.release_id, row.message_id, row.file_id, row.part, row.bytes])? as u64;
                mark.execute(params![row.release_id, seen_hash(row.message_id)])?;
                if let Some(e) = &row.envelope {
                    envelope.execute(params![
                        row.release_id,
                        row.message_id,
                        e.subject,
                        e.poster,
                        e.posted_date,
                        e.nzb_timestamp,
                        e.expected,
                        e.subject_file_index,
                        e.subject_file_total
                    ])?;
                }
            }
        }
        tx.commit()?;
        Ok(added)
    }

    /// Read, on a read connection and without the write lock, the rows the next `fill_seen` will copy, when one is
    /// due: on prod the fill's reads of cold `segments` pages held the write lock every save waits for (the copy itself
    /// still reads them again inside its write transaction, so what it copies is what is there). Best effort.
    fn warm_fill(&self) {
        let above = self.seen_above.load(Ordering::Acquire);
        if above == i64::MIN
            || self.seen_filled_at.lock().unwrap_or_else(std::sync::PoisonError::into_inner).is_some_and(|t| t.elapsed() < SEEN_FILL_EVERY)
        {
            return;
        }
        // one at a time, none while a WAL restart is wanted (its checkpoint waits for readers), and none while a save
        // waits for the write lock: the fill then doesnt run (`add_fresh`), and the read would be for nothing
        if self.restart_wanted.load(Ordering::Acquire)
            || self.waiting.load(Ordering::Acquire) > 0
            || self.warming.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_err()
        {
            return;
        }
        struct Done<'a>(&'a std::sync::atomic::AtomicBool);
        impl Drop for Done<'_> {
            fn drop(&mut self) {
                self.0.store(false, Ordering::Release);
            }
        }
        let _done = Done(&self.warming);
        let _ = self.read(|conn| {
            let cursor: Option<Vec<u8>> = conn
                .query_row("select value from meta where key = 'seen_cursor'", [], |row| row.get::<_, String>(0))
                .optional()?
                .map(|v| decode_hex(&v))
                .transpose()?;
            let mut read = conn.prepare_cached(
                FILL_ROWS_SQL,
            )?;
            let mut rows = read.query(params![above, cursor, SEEN_FILL_ROWS])?;
            while rows.next()?.is_some() {}
            Ok(())
        });
    }

    /// Copy into `seen` the next `SEEN_FILL_ROWS` articles saved before it existed, newest release first, on the write
    /// connection under its lock (so no save or rollback runs between reading `segments` and writing `seen`). Each
    /// release is copied whole before `seen_above` passes below it; then `has` asks `seen` for it.
    fn fill_seen(&self, conn: &mut Connection) -> Result<()> {
        let above = self.seen_above.load(Ordering::Acquire);
        if above == i64::MIN {
            return Ok(());
        }
        {
            let mut at = self.seen_filled_at.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if at.is_some_and(|t| t.elapsed() < SEEN_FILL_EVERY) {
                return Ok(());
            }
            *at = Some(std::time::Instant::now());
        }
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        // where the copy of release `above` got to: its message ids above this are in `seen`
        let cursor: Option<Vec<u8>> =
            tx.query_row("select value from meta where key = 'seen_cursor'", [], |row| row.get::<_, String>(0)).optional()?.map(|v| decode_hex(&v)).transpose()?;
        let rows: Vec<(i64, Vec<u8>)> = {
            let mut read = tx.prepare_cached(
                FILL_ROWS_SQL,
            )?;
            let mut rows = read.query(params![above, cursor, SEEN_FILL_ROWS])?;
            let mut out = Vec::new();
            while let Some(row) = rows.next()? {
                out.push((row.get(0)?, row.get(1)?));
            }
            out
        };
        {
            let mut mark = tx.prepare_cached("insert or ignore into seen (release_id, h) values (?1, ?2)")?;
            for (release, id) in &rows {
                mark.execute(params![release, seen_hash(id)])?;
            }
        }
        let next = match rows.last() {
            // the last release read may go on past this batch: it is the cursor's; every one above it is whole
            Some((release, id)) if rows.len() as i64 == SEEN_FILL_ROWS => {
                tx.execute(
                    "insert into meta (key, value) values ('seen_cursor', ?1) on conflict (key) do update set value = excluded.value",
                    [encode_hex(id)],
                )?;
                *release
            }
            // the oldest article is in: every release is
            _ => {
                tx.execute("delete from meta where key = 'seen_cursor'", [])?;
                i64::MIN
            }
        };
        tx.execute("update meta set value = ?1 where key = 'seen_above'", [next.to_string()])?;
        tx.commit()?;
        self.seen_above.store(next, Ordering::Release);
        Ok(())
    }

    fn meta(&self, key: &str) -> Result<Option<String>> {
        Ok(self.lock()?.query_row("select value from meta where key = ?1", [key], |row| row.get(0)).optional()?)
    }

    fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        ensure!(self.writer, "the local article store is open for reading only");
        self.lock()?.execute(
            "insert into meta (key, value) values (?1, ?2) on conflict (key) do update set value = excluded.value",
            [key, value],
        )?;
        Ok(())
    }

    /// A fixture's `delete from segments where release_id = ..`.
    #[cfg(test)]
    fn forget_releases(&self, releases: &[i64]) -> Result<u64> {
        let conn = self.lock()?;
        let mut gone = 0;
        for release in releases {
            gone += conn.execute("delete from segments where release_id = ?1", [release])? as u64;
            conn.execute("delete from seen where release_id = ?1", [release])?;
            conn.execute("delete from envelopes where release_id = ?1", [release])?;
        }
        Ok(gone)
    }
}

/// An article's key in `seen`: the first 8 bytes of its message key. Two message ids of one release sharing it (about
/// n²/2⁶⁵ for a release of n articles) would leave the second taken for one the release has.
pub(crate) fn seen_hash(message_id: &[u8]) -> i64 {
    i64::from_le_bytes(message_key(message_id)[..8].try_into().expect("8 bytes"))
}

fn encode_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn decode_hex(text: &str) -> Result<Vec<u8>> {
    ensure!(text.len() % 2 == 0, "odd hex length");
    (0..text.len()).step_by(2).map(|i| Ok(u8::from_str_radix(&text[i..i + 2], 16)?)).collect()
}

/// The rows `fill_seen` copies next, and `warm_fill` reads ahead of it: one text, so both read the same pages.
const FILL_ROWS_SQL: &str = "select release_id, message_id from segments
    where release_id < ?1 or (release_id = ?1 and (?2 is null or message_id < ?2))
    order by release_id desc, message_id desc limit ?3";

/// Rows `fill_seen` copies at most at once, and how often it runs at most.
const SEEN_FILL_ROWS: i64 = 20_000;
const SEEN_FILL_EVERY: std::time::Duration = std::time::Duration::from_millis(100);

/// The key PostgreSQL kept beside each article: the SHA-256 of its message id.
pub(crate) fn message_key(message_id: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    Sha256::digest(message_id).into()
}

fn encode_keys(rows: &[&NewRow<'_>]) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(rows.iter().map(|row| 12 + row.message_id.len()).sum());
    for row in rows {
        out.extend_from_slice(&row.release_id.to_le_bytes());
        out.extend_from_slice(&u32::try_from(row.message_id.len()).context("message id too long")?.to_le_bytes());
        out.extend_from_slice(row.message_id);
    }
    Ok(out)
}

fn decode_keys(mut blob: &[u8]) -> Result<Vec<(i64, &[u8])>> {
    let mut keys = Vec::new();
    while !blob.is_empty() {
        ensure!(blob.len() >= 12, "the local article store's unsettled keys are cut short");
        let release = i64::from_le_bytes(blob[..8].try_into()?);
        let len = u32::from_le_bytes(blob[8..12].try_into()?) as usize;
        ensure!(blob.len() >= 12 + len, "the local article store's unsettled keys are cut short");
        keys.push((release, &blob[12..12 + len]));
        blob = &blob[12 + len..];
    }
    Ok(keys)
}

/// The stores this process has open, by the PostgreSQL schema whose ids they
/// carry. A schema with no store keeps its articles in PostgreSQL as before.
static STORES: Mutex<Option<HashMap<String, Arc<Store>>>> = Mutex::new(None);
static OPEN: AtomicUsize = AtomicUsize::new(0);

pub(crate) fn register(schema: &str, store: Store) -> Result<Arc<Store>> {
    let store = Arc::new(store);
    let mut stores = STORES.lock().map_err(|_| anyhow::anyhow!("the local article store registry is poisoned"))?;
    let stores = stores.get_or_insert_with(HashMap::new);
    if stores.contains_key(schema) {
        bail!("this process already opened a local article store for schema {schema}");
    }
    stores.insert(schema.to_owned(), store.clone());
    OPEN.store(stores.len(), Ordering::Release);
    Ok(store)
}

pub(crate) fn unregister(schema: &str) {
    if let Ok(mut stores) = STORES.lock()
        && let Some(stores) = stores.as_mut()
    {
        stores.remove(schema);
        OPEN.store(stores.len(), Ordering::Release);
    }
}

/// The store for a schema, if this process opened one.
pub(crate) fn of_schema(schema: &str) -> Option<Arc<Store>> {
    if OPEN.load(Ordering::Acquire) == 0 {
        return None;
    }
    STORES.lock().ok()?.as_ref()?.get(schema).cloned()
}

/// The store for the schema this connection writes, if this process opened one.
/// Costs a query only when some store is open. In the writing process, saves
/// whose transaction has ended are settled first.
pub(crate) fn of(client: &mut impl postgres::GenericClient) -> Result<Option<Arc<Store>>> {
    if OPEN.load(Ordering::Acquire) == 0 {
        return Ok(None);
    }
    let schema: Option<String> = client.query_one("select current_schema()::text", &[])?.try_get(0)?;
    let Some(store) = schema.and_then(|schema| of_schema(&schema)) else { return Ok(None) };
    // A save PostgreSQL rolled back must not be read as stored. The writer
    // confirms a commit it saw, so this finds something only after a failure.
    if store.is_writer() && store.has_unsettled()? {
        store.settle(|xid| pg_ended(client, xid))?;
    }
    Ok(Some(store))
}

/// The transaction `tx` deleted these releases from the catalogue: when the
/// schema keeps its articles locally, theirs are dropped once it commits.
pub(crate) fn retract_in(tx: &mut postgres::Transaction<'_>, releases: &[i64]) -> Result<()> {
    let Some(store) = of(tx)? else { return Ok(()) };
    let xid: String = tx.query_one("select pg_current_xact_id()::text", &[])?.try_get(0)?;
    store.retract(&xid, releases)
}

/// Ask PostgreSQL how the transaction `xid` ended. One too old for PostgreSQL
/// to remember is taken as committed: its rows were visible long enough to be
/// read. One PostgreSQL says it has never handed out was lost when the server
/// crashed before writing it down, so it never committed; that answer is an
/// error, asked under a savepoint so the caller's transaction survives it.
pub(crate) fn pg_ended(client: &mut impl postgres::GenericClient, xid: &str) -> Result<Ended> {
    let mut ask = client.transaction()?;
    let status: Option<String> = match ask.query_one("select pg_xact_status($1::text::xid8)", &[&xid]) {
        Ok(row) => row.try_get(0)?,
        Err(error) if error.code() == Some(&postgres::error::SqlState::INVALID_PARAMETER_VALUE) => Some("lost".into()),
        Err(error) => return Err(error.into()),
    };
    ask.rollback()?;
    Ok(match status.as_deref() {
        Some("in progress") => Ended::Running,
        Some("aborted" | "lost") => Ended::RolledBack,
        _ => Ended::Committed,
    })
}
