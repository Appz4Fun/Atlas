//! `atlas --compact`, with the indexer stopped: rewrite every shard into a
//! fresh file.
//!
//! - message-ids whose domain fewer than `SHARED_AFTER` articles share are
//!   stored whole, and those domains dropped: some posting tools make up a
//!   new domain per article, which left tens of millions of single use rows
//! - the other message-ids packed the current way (locals saved before the
//!   packing by shape are re-encoded), in blobs too
//! - every file that is due sealed into a blob as it's copied
//! - the copy has no free space in it (the conversion left the space its
//!   staging table used)
//!
//! Each shard's copy is checked (row counts, and a sample of NZBs against the
//! original) before it replaces the original. The originals stay next to it
//! as `atlas.sN.precompact.db` until every shard has had its turn, then they
//! go. A shard that fails keeps its original in place and the others carry
//! on; `run` then returns an error naming each failed shard. A swap cut short
//! between moving the original aside and the copy in (a crash) is undone by
//! the next start or compaction, see `recover_cut_swaps`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use anyhow::{Context, Result, anyhow, bail};
use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension, params, params_from_iter};

use crate::blob::{self, Seg};
use crate::db;
use crate::store::{self, SHARDS, SHARED_AFTER};

/// shards rewritten at once
const PARALLEL: usize = 4;
/// releases whose NZBs are compared per shard
const CHECK_SAMPLE: i64 = 300;
/// rows per transaction
const BATCH: usize = 500_000;

/// `atlas.s3.db` -> `atlas.s3.{suffix}.db`
pub(crate) fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    path.with_file_name(format!("{stem}.{suffix}.db"))
}

fn remove_db(path: &Path) {
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}

/// Fold the WAL of `conn`'s database into it, all of it: a checkpoint held
/// back (a reader, a writer) leaves committed pages only in the WAL, soo it
/// is an error rather than taken for done.
pub(crate) fn checkpoint(conn: &Connection) -> Result<()> {
    let (busy, log, done): (i64, i64, i64) =
        conn.query_row("pragma wal_checkpoint(truncate)", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
    if busy != 0 || log != done {
        bail!("the WAL checkpoint didnt complete ({done} of {log} pages, busy {busy})");
    }
    Ok(())
}

/// Rename the database `from` to `to`, its -wal and -shm with it (those
/// there are): the WAL can hold committed pages of it.
pub(crate) fn rename_db(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::rename(from, to)?;
    for suffix in ["-wal", "-shm"] {
        let (a, b) = (format!("{}{suffix}", from.display()), format!("{}{suffix}", to.display()));
        if Path::new(&a).exists() {
            std::fs::rename(a, b)?;
        }
    }
    Ok(())
}

fn size(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

/// `atlas.compacting` next to `main`: a compaction holds it locked
/// exclusively for its whole run, the indexer holds it shared while it
/// indexes, and a write from outside the indexer for its own. The OS lets go
/// of a lock when its process ends, soo a crash leaves nothing to clean up.
/// The file itself stays (removing a file someone may be locking races them).
pub fn lock_path(main: &Path) -> PathBuf {
    main.with_extension("compacting")
}

fn open_lock(main: &Path) -> Result<std::fs::File> {
    let path = lock_path(main);
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))
}

/// A compaction couldnt start: another one, the indexer or a write from
/// outside it holds the lock.
#[derive(Debug)]
pub struct Busy;

impl std::fmt::Display for Busy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the indexer, another compaction or a database write holds the lock")
    }
}

impl std::error::Error for Busy {}

/// The indexer while it indexes, and writes from outside it (AI search saves,
/// purging) for their whole run, hold this: they would land in the original
/// after a compaction took its copy, and be lost when the copy replaces it.
/// Refused while a compaction runs; a compaction cant start while one is held.
#[must_use = "the write is only safe while the guard is held"]
pub struct WriteGuard(std::fs::File);

/// Hold off compaction while writing the shards, see `WriteGuard`. Also
/// refused while a shard and its `.precompact.db` are both there and which
/// one is whole isnt known (`unresolved_backups`): a write would go into
/// whichever is wrong. Whoever holds this is about to write; setting up the
/// database and reading go through `try_hold_off_compaction` and aren't held
/// back.
pub fn hold_off_compaction(main: &Path) -> Result<WriteGuard> {
    let held =
        try_hold_off_compaction(main)?.ok_or_else(|| anyhow!("the database is being compacted; try again later"))?;
    refuse_unresolved_backups(main)?;
    Ok(held)
}

/// `hold_off_compaction`, None while a compaction runs.
pub fn try_hold_off_compaction(main: &Path) -> Result<Option<WriteGuard>> {
    let file = open_lock(main)?;
    match file.try_lock_shared() {
        Ok(()) => Ok(Some(WriteGuard(file))),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(e)) => Err(e).context("locking the database for a write"),
    }
}

impl Drop for WriteGuard {
    fn drop(&mut self) {
        // closing the file lets go too, but Windows may take a while to
        let _ = self.0.unlock();
    }
}

/// The lock of a running compaction, let go when dropped (done, failed or stopped).
pub(crate) struct Lock(std::fs::File);

impl Lock {
    pub(crate) fn take(main: &Path) -> Result<Lock> {
        let file = open_lock(main)?;
        match file.try_lock() {
            Ok(()) => Ok(Lock(file)),
            Err(std::fs::TryLockError::WouldBlock) => Err(Busy.into()),
            Err(std::fs::TryLockError::Error(e)) => Err(e).context("locking the database for compacting"),
        }
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// Shards with a `.precompact.db` next to them, as (shard, backup). With the
/// lock held a backup next to a shard is not a compaction at work: it was
/// cut short, or an older atlas made an empty shard beside it.
pub fn unresolved_backups(main: &Path) -> Result<Vec<(PathBuf, PathBuf)>> {
    let mut found = Vec::new();
    for path in store::shard_paths(main) {
        let backup = with_suffix(&path, "precompact");
        if backup.try_exists()? && path.try_exists()? {
            found.push((path, backup));
        }
    }
    Ok(found)
}

/// What to tell the user about one `unresolved_backups` pair: both files, and
/// how to go on with either.
fn unresolved_message(shard: &Path, backup: &Path) -> String {
    let bytes = |p: &Path| crate::ui::fmt_size(Some(size(p) as i64));
    format!(
        "{shard} ({}) and {backup} ({}) both exist, and which one is whole isnt known: a compaction was cut short, \
         or an older atlas made an empty shard next to the backup. Not writing the shards till this is resolved. \
         Stop atlas, then either keep {shard} and delete {backup}, or keep {backup}: delete {shard} (and its -wal and \
         -shm files) and rename {backup} to {shard}. The bigger one usually has the releases",
        bytes(shard),
        bytes(backup),
        shard = shard.display(),
        backup = backup.display(),
    )
}

/// Err for the first of `unresolved_backups`, see `unresolved_message`.
pub fn refuse_unresolved_backups(main: &Path) -> Result<()> {
    match unresolved_backups(main)?.first() {
        Some((shard, backup)) => bail!("{}", unresolved_message(shard, backup)),
        None => Ok(()),
    }
}

/// Put back the original of every shard whose swap was cut short (a crash,
/// or its copy failing to move in): the original moved aside as
/// `.precompact.db` and nothing in its place. Nothing writes a shard while
/// it's compacted, soo the original is whole: it goes back, and the copy
/// goes. A backup next to a shard that is in place is kept (the copy may have
/// gone in, or a shard made empty in its place by an older atlas; which one
/// is whole isnt known) and said so: the indexer and saves refuse till the
/// user removes one (`hold_off_compaction`). Returns a line for each, to show.
///
/// Only with the lock held (either kind): a running compaction has a shard
/// moved aside on purpose for a moment.
pub fn recover_cut_swaps(main: &Path) -> Result<Vec<String>> {
    let mut said = Vec::new();
    for path in store::shard_paths(main) {
        let original = with_suffix(&path, "precompact");
        if !original.try_exists()? {
            continue;
        }
        if path.try_exists()? {
            said.push(unresolved_message(&path, &original));
            continue;
        }
        rename_db(&original, &path)
            .with_context(|| format!("putting {} back as {}", original.display(), path.display()))?;
        remove_db(&with_suffix(&path, "compact"));
        said.push(format!("{} was moved aside by a compaction that was cut short; it's back in place", path.display()));
    }
    Ok(said)
}

/// What compacting one shard did.
#[derive(Debug, Default, Clone, Copy)]
pub struct Shrunk {
    pub before: u64,
    pub after: u64,
    pub domains_kept: i64,
    pub domains_dropped: i64,
}

/// the rows of a copying loop between looks at the stop flag
const STOP_EVERY: usize = 65_536;

/// Stop with an error once `stop` is set. Compacting only ever leaves the
/// original shard alone until the swap, soo stopping anywhere before it loses
/// nothing: the half made copy goes and the shard stays as it was.
fn halt(stop: &AtomicBool, shard: usize) -> Result<()> {
    if stop.load(Ordering::Relaxed) {
        bail!("shard {shard}: stopped, the original was kept");
    }
    Ok(())
}

/// Have SQLite give up long statements (a bulk insert, an index build) once
/// `stop` is set: they fail with an interrupt, like any other error.
fn interruptible(conn: &Connection, stop: &Arc<AtomicBool>) -> Result<()> {
    let stop = stop.clone();
    conn.progress_handler(1000, Some(move || stop.load(Ordering::Relaxed)))?;
    Ok(())
}

/// Compact every shard of `main`. Returns the total before and after. Once
/// `stop` is set, shards not done are left as they were (and an error says so).
pub fn run(main: &Path, progress: &(dyn Fn(&str) + Sync), stop: &Arc<AtomicBool>) -> Result<Shrunk> {
    let _lock = Lock::take(main)?;
    for line in recover_cut_swaps(main)? {
        progress(&line);
    }
    if !store::exists(main) {
        bail!("{} has no shards (convert it first)", main.display());
    }
    let started = Instant::now();
    let mut total = Shrunk::default();
    let mut failed: Vec<(usize, String)> = Vec::new();
    // every shard gets its turn: one that fails keeps its original and doesnt
    // stop the others
    for chunk in (0..SHARDS).collect::<Vec<_>>().chunks(PARALLEL) {
        if stop.load(Ordering::Relaxed) {
            failed.extend(chunk.iter().map(|&shard| (shard, format!("shard {shard}: stopped"))));
            continue;
        }
        let results: Vec<Result<Shrunk>> = std::thread::scope(|s| {
            let jobs: Vec<_> =
                chunk.iter().map(|&shard| s.spawn(move || compact_shard(main, shard, progress, stop))).collect();
            jobs.into_iter().map(|j| j.join().unwrap_or_else(|_| Err(anyhow!("compacting a shard panicked")))).collect()
        });
        for (&shard, r) in chunk.iter().zip(results) {
            match r {
                Ok(r) => {
                    total.before += r.before;
                    total.after += r.after;
                    total.domains_kept += r.domains_kept;
                    total.domains_dropped += r.domains_dropped;
                }
                Err(e) => failed.push((shard, format!("shard {shard}: {e:#}"))),
            }
        }
    }
    // the originals of the shards that were swapped can go. a failed shard
    // keeps its original in place, and loses its half made copy; one whose
    // copy didnt move in gets its original back
    for shard in 0..SHARDS {
        let path = store::shard_path(main, shard);
        if !failed.iter().any(|(f, _)| *f == shard) {
            remove_db(&with_suffix(&path, "precompact"));
        } else if path.exists() {
            remove_db(&with_suffix(&path, "compact"));
        }
    }
    if failed.iter().any(|(shard, _)| !store::shard_path(main, *shard).exists()) {
        for line in recover_cut_swaps(main)? {
            progress(&line);
        }
    }
    if stop.load(Ordering::Relaxed) {
        bail!("stopped, {} of {SHARDS} shards were left as they were", failed.len());
    }
    if !failed.is_empty() {
        let each: Vec<String> = failed.into_iter().map(|(_, e)| e).collect();
        bail!("{} of {SHARDS} shards were not compacted, their originals were kept: {}", each.len(), each.join("; "));
    }
    progress(&format!(
        "compacted {:.1}GB into {:.1}GB in {}: kept {} shared domains, dropped {} single use ones",
        total.before as f64 / 1e9,
        total.after as f64 / 1e9,
        crate::dashboard::human_time(started.elapsed().as_secs() as i64),
        total.domains_kept,
        total.domains_dropped
    ));
    Ok(total)
}

fn compact_shard(
    main: &Path,
    shard: usize,
    progress: &(dyn Fn(&str) + Sync),
    stop: &Arc<AtomicBool>,
) -> Result<Shrunk> {
    let path = store::shard_path(main, shard);
    let copy = with_suffix(&path, "compact");
    let say = |msg: &str| progress(&format!("compacting shard {shard}: {msg}"));
    halt(stop, shard)?;
    // a backup left by a compaction cut short isnt known to be redundant, and
    // the swap would need its name
    let backup = with_suffix(&path, "precompact");
    if backup.try_exists()? {
        bail!("{} is left from an earlier compaction; the original was kept", backup.display());
    }

    // fold the WAL in soo the original is whole on its own
    {
        let conn = db::open_at(&path)?;
        checkpoint(&conn).context("folding the original's WAL in")?;
        // a shard from before sealing gets its (empty) seal columns
        store::migrate_shard(&conn)?;
    }
    let before = size(&path);

    // a fresh shard, filled without its indexes and triggers (built at the end)
    remove_db(&copy);
    store::create_shard(&copy)?;
    let mut conn = db::open_at(&copy)?;
    interruptible(&conn, stop)?;
    conn.query_row("pragma journal_mode = off", [], |_| Ok(()))?;
    conn.execute_batch(
        "pragma synchronous = off;
         pragma cache_size = -262144;
         pragma temp_store = memory;
         drop trigger releases_ai; drop trigger releases_ad; drop trigger releases_au;
         drop index idx_release_unique; drop index idx_release_group; drop index files_key;",
    )?;
    conn.execute("attach database ? as old", [path.to_string_lossy()])?;

    // how many articles use each domain, as rows and inside sealed blobs
    say("counting domains");
    // only the ids in use: earlier compactions leave the ids sparse
    let mut uses: HashMap<i64, u32> = HashMap::new();
    {
        let mut used = |d: i64| {
            let n = uses.entry(d).or_insert(0);
            *n = n.saturating_add(1);
        };
        let mut stmt = conn.prepare("select domain from old.segments")?;
        let mut rows = stmt.query([])?;
        let mut n = 0usize;
        while let Some(r) = rows.next()? {
            used(r.get(0)?);
            n += 1;
            if n.is_multiple_of(STOP_EVERY) {
                halt(stop, shard)?;
            }
        }
        let mut stmt = conn.prepare("select id, blob from old.files where blob is not null")?;
        let mut rows = stmt.query([])?;
        while let Some(r) = rows.next()? {
            halt(stop, shard)?;
            for seg in unseal(shard, r.get(0)?, &r.get::<_, Vec<u8>>(1)?)? {
                used(seg.domain);
            }
        }
    }
    let shared = |d: i64| d != 0 && uses.get(&d).is_some_and(|n| *n >= SHARED_AFTER);

    halt(stop, shard)?;
    say("copying releases");
    conn.execute_batch(
        "insert into releases select * from old.releases order by id;
         insert or replace into meta select * from old.meta;",
    )?;

    // the shared domains, keeping their ids
    let (mut kept, mut dropped) = (0i64, 0i64);
    {
        let tx = conn.transaction()?;
        {
            let mut insert = tx.prepare("insert into domains (id, suffix) values (?, ?)")?;
            let mut stmt = tx.prepare("select id, suffix from old.domains order by id")?;
            let mut rows = stmt.query([])?;
            while let Some(r) = rows.next()? {
                let (id, suffix): (i64, String) = (r.get(0)?, r.get(1)?);
                if shared(id) {
                    insert.execute(params![id, suffix])?;
                    kept += 1;
                } else {
                    dropped += 1;
                }
            }
        }
        tx.commit()?;
    }

    // the files and their articles, in id order: those of dropped domains
    // stored whole, the rest packed the current way, and files that are due
    // sealed into one blob on the way (sealing afterwards would leave the
    // copy full of the deleted rows' free pages)
    halt(stop, shard)?;
    say(&format!("copying files and articles ({kept} shared domains, dropping {dropped}), sealing those due"));
    let now = chrono::Utc::now().timestamp();
    // rows dropped as copies of what their file's blob already has
    let (mut copied, mut sealed, mut copies) = (0i64, 0i64, 0i64);
    {
        let open = || Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY);
        let (files_read, rows_read) = (open()?, open()?);
        let mut suffix_of = files_read.prepare("select suffix from domains where id = ?")?;
        let mut files_stmt = files_read.prepare(&format!("select {FILE_COLUMNS} from files order by id"))?;
        let mut files = files_stmt.query([])?;
        let mut rows_stmt = rows_read.prepare(
            "select s.file_id, s.local, s.domain, s.part, s.bytes, d.suffix
             from segments s left join domains d on d.id = s.domain order by s.file_id",
        )?;
        let mut rows = rows_stmt.query([])?;
        let mut pending = next_row(shard, &mut rows, &shared)?;

        let mut tx = conn.transaction()?;
        let mut in_tx = 0;
        loop {
            halt(stop, shard)?;
            let file = files.next()?;
            let id: Option<i64> = file.map(|f| f.get(0)).transpose()?;
            // rows whose file isnt there (purged, or past the last file) stay rows
            while let Some(row) = pending.take_if(|r| id.is_none_or(|id| r.file_id < id)) {
                insert_row(&tx, &row)?;
                (copied, in_tx) = (copied + 1, in_tx + 1);
                if (copied as usize).is_multiple_of(STOP_EVERY) {
                    halt(stop, shard)?;
                }
                pending = next_row(shard, &mut rows, &shared)?;
            }
            let (Some(f), Some(id)) = (file, id) else { break };
            // at most one past the most a blob takes; the rest of a bigger file
            // is streamed below, never held
            let mut file_rows = Vec::new();
            while file_rows.len() as i64 <= store::SEAL_MAX_SEGMENTS
                && let Some(row) = pending.take_if(|r| r.file_id == id)
            {
                file_rows.push(row);
                pending = next_row(shard, &mut rows, &shared)?;
            }

            // its blob, with every message-id stored the way its rows are
            let old_blob: Option<Vec<u8>> = f.get(10)?;
            let mut segs = Vec::new();
            // old blob segments whose rewritten message-id is past what a blob
            // takes (a dropped domain's suffix made whole again) become rows
            let mut spilled = Vec::new();
            for seg in old_blob.as_deref().map(|b| unseal(shard, id, b)).transpose()?.unwrap_or_default() {
                let suffix: Option<String> = if seg.domain != 0 && !shared(seg.domain) {
                    suffix_of.query_row([seg.domain], |r| r.get(0)).optional()?
                } else {
                    None
                };
                let (local, domain) = rewrite(shard, id, seg.local, seg.domain, shared(seg.domain), suffix.as_deref())?;
                if local.len() > blob::MAX_LOCAL {
                    spilled.push(Row { file_id: id, local, domain, part: seg.part, bytes: Some(seg.bytes) });
                } else {
                    segs.push(Seg { local, domain, ..seg });
                }
            }
            file_rows.extend(spilled);

            // a row the blob already has (the same message-id) isnt copied:
            // it goes before the blob is counted, the way `seal_rows` drops it
            if !segs.is_empty() {
                let held: HashSet<(&[u8], i64)> = segs.iter().map(|s| (s.local.as_slice(), s.domain)).collect();
                let (dropped, kept): (Vec<Row>, Vec<Row>) =
                    file_rows.into_iter().partition(|r| held.contains(&(r.local.as_slice(), r.domain)));
                file_rows = kept;
                if !dropped.is_empty() {
                    // out of the release's and the shard's totals too, copied as they were
                    let bytes = dropped.iter().map(|r| r.bytes.unwrap_or(0)).sum();
                    store::uncount_copies(&tx, f.get(1)?, bytes, dropped.len() as i64)?;
                    copies += dropped.len() as i64;
                }
            }

            // rows a blob cant hold exactly (a negative part, no size, a long message-id) stay rows
            let fits = file_rows
                .iter()
                .all(|r| r.part.is_none_or(|p| p >= 0) && r.bytes.is_some() && r.local.len() <= blob::MAX_LOCAL);
            let seen: Vec<u8> = f.get(8)?;
            // and files with more segments than a blob takes stay rows
            let small = (segs.len() + file_rows.len()) as i64 <= store::SEAL_MAX_SEGMENTS;
            let seal = !file_rows.is_empty()
                && fits
                && small
                && store::due(f.get(6)?, &seen, f.get(9)?, old_blob.is_some(), now);
            if seal {
                segs.extend(file_rows.drain(..).map(|r| Seg {
                    part: r.part,
                    bytes: r.bytes.unwrap_or(0),
                    domain: r.domain,
                    local: r.local,
                }));
                sealed += 1;
            }
            let blob = (old_blob.is_some() || seal).then(|| blob::encode(&segs));

            let mut values: Vec<Value> = (0..10).map(|i| f.get(i)).collect::<rusqlite::Result<_>>()?;
            values.push(blob.map_or(Value::Null, Value::Blob));
            tx.prepare_cached(&format!("insert into files ({FILE_COLUMNS}) values (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"))?
                .execute(params_from_iter(values))?;
            for row in &file_rows {
                insert_row(&tx, row)?;
            }
            copied += (segs.len() + file_rows.len()) as i64;
            in_tx += 1 + segs.len() + file_rows.len();
            // the rest of a file too big to buffer, a row at a time
            while let Some(row) = pending.take_if(|r| r.file_id == id) {
                insert_row(&tx, &row)?;
                (copied, in_tx) = (copied + 1, in_tx + 1);
                if (copied as usize).is_multiple_of(STOP_EVERY) {
                    halt(stop, shard)?;
                }
                if in_tx >= BATCH {
                    tx.commit()?;
                    tx = conn.transaction()?;
                    in_tx = 0;
                }
                pending = next_row(shard, &mut rows, &shared)?;
            }
            if in_tx >= BATCH {
                tx.commit()?;
                tx = conn.transaction()?;
                in_tx = 0;
            }
        }
        tx.commit()?;
    }
    say(&format!("sealed {sealed} files"));

    halt(stop, shard)?;
    say("building indexes and the search index");
    conn.execute_batch(
        "detach database old;
         insert into releases_fts(releases_fts) values('rebuild');",
    )?;
    drop(conn);
    {
        let conn = db::open_at(&copy)?;
        interruptible(&conn, stop)?;
        store::build_shard(&conn)?;
    }

    // the check: same rows, same NZBs
    halt(stop, shard)?;
    say("checking");
    check(&path, &copy, shard, copied, copies, stop)?;

    // swap: the last point to stop at
    halt(stop, shard)?;
    let conn = db::open_at(&copy)?;
    conn.query_row("pragma journal_mode = wal", [], |_| Ok(()))?;
    checkpoint(&conn).context("folding the copy's WAL in")?;
    drop(conn);
    // the original's -wal and -shm go aside with it: whatever they hold
    // comes back with it if the swap is cut short (`recover_cut_swaps`)
    let original = with_suffix(&path, "precompact");
    rename_db(&path, &original).context("moving the original shard aside")?;
    std::fs::rename(&copy, &path).context("putting the compacted shard in place")?;
    let after = size(&path);
    say(&format!("{:.1}GB -> {:.1}GB", before as f64 / 1e9, after as f64 / 1e9));
    Ok(Shrunk { before, after, domains_kept: kept, domains_dropped: dropped })
}

/// the files table's columns, blob last
const FILE_COLUMNS: &str =
    "id, release_id, filename, subject, subject_part, subject_mid, expected, file_total, seen, touched_at, blob";

/// A sealed file's articles. A blob that wont decode stops the shard: copying
/// on without it would lose the file's articles.
fn unseal(shard: usize, file_id: i64, blob: &[u8]) -> Result<Vec<Seg>> {
    blob::decode(blob)
        .with_context(|| format!("shard {shard}: file {file_id} has a corrupt blob; the original was kept"))
}

/// A message-id as the copy stores it, (local, domain): kept whole, packed
/// the current way under a shared domain, or made whole when its domain is
/// dropped. `suffix` is the domain's, needed for a dropped one. A rewrite
/// that doesnt give back the same message-id stops the shard.
fn rewrite(
    shard: usize,
    file_id: i64,
    local: Vec<u8>,
    domain: i64,
    shared: bool,
    suffix: Option<&str>,
) -> Result<(Vec<u8>, i64)> {
    let (new_local, new_domain) = if domain == 0 {
        return Ok((local, 0));
    } else if shared {
        (store::repack(&local), domain)
    } else {
        (store::whole(&store::decode(&local, suffix)), 0)
    };
    if new_local != local {
        let new_suffix = if new_domain == 0 { None } else { suffix };
        check_same_id(shard, file_id, (&local, suffix), (&new_local, new_suffix))?;
    }
    Ok((new_local, new_domain))
}

/// The message-id a rewritten (local, suffix) stands for is the original's:
/// a packing bug would quietly change NZBs otherwise.
fn check_same_id(
    shard: usize,
    file_id: i64,
    (old_local, old_suffix): (&[u8], Option<&str>),
    (new_local, new_suffix): (&[u8], Option<&str>),
) -> Result<()> {
    let (was, now) = (store::decode(old_local, old_suffix), store::decode(new_local, new_suffix));
    if was != now {
        bail!("shard {shard}: file {file_id}: message-id {was} came out as {now}; the original was kept");
    }
    Ok(())
}

/// one article row of the copy
struct Row {
    file_id: i64,
    local: Vec<u8>,
    domain: i64,
    part: Option<i64>,
    bytes: Option<i64>,
}

/// the next row of `select file_id, local, domain, part, bytes, suffix`, rewritten
fn next_row(shard: usize, rows: &mut rusqlite::Rows, shared: &dyn Fn(i64) -> bool) -> Result<Option<Row>> {
    let Some(r) = rows.next()? else { return Ok(None) };
    let (file_id, domain, suffix): (i64, i64, Option<String>) = (r.get(0)?, r.get(2)?, r.get(5)?);
    let (local, domain) = rewrite(shard, file_id, r.get(1)?, domain, shared(domain), suffix.as_deref())?;
    Ok(Some(Row { file_id, local, domain, part: r.get(3)?, bytes: r.get(4)? }))
}

fn insert_row(conn: &Connection, r: &Row) -> Result<()> {
    conn.prepare_cached("insert or ignore into segments (file_id, local, domain, part, bytes) values (?, ?, ?, ?, ?)")?
        .execute(params![r.file_id, r.local, r.domain, r.part, r.bytes])?;
    Ok(())
}

/// The copy holds the same releases, files and articles (rows and inside
/// blobs) as the original, less the `copies` rows that were copies of what
/// their blob already had, and a sample of releases build the same NZBs from
/// both.
fn check(original: &Path, copy: &Path, shard: usize, copied: i64, copies: i64, stop: &Arc<AtomicBool>) -> Result<()> {
    let attach = |path: &Path| -> Result<Connection> {
        let conn = Connection::open_in_memory()?;
        interruptible(&conn, stop)?;
        conn.execute("attach database ? as ?", params![path.to_string_lossy(), format!("s{shard}")])?;
        Ok(conn)
    };
    let (old, new) = (attach(original)?, attach(copy)?);
    let count = |conn: &Connection, table: &str| -> Result<i64> {
        Ok(conn.query_row(&format!("select count(*) from s{shard}.{table}"), [], |r| r.get(0))?)
    };
    for table in ["releases", "files"] {
        let (a, b) = (count(&old, table)?, count(&new, table)?);
        if a != b {
            bail!("shard {shard}: {table} has {b} rows in the copy, {a} in the original; the original was kept");
        }
    }
    let schema = format!("s{shard}");
    let (a, b) = (store::article_count(&old, &schema)?, store::article_count(&new, &schema)?);
    if a - copies != b {
        bail!(
            "shard {shard}: {b} articles in the copy, {a} in the original ({copies} copies dropped); the original was kept"
        );
    }
    if copied != b {
        bail!("shard {shard}: some articles came out as duplicates; the original was kept");
    }

    let (low, high): (Option<i64>, Option<i64>) =
        old.query_row(&format!("select min(id), max(id) from s{shard}.releases"), [], |r| Ok((r.get(0)?, r.get(1)?)))?;
    let (Some(low), Some(high)) = (low, high) else { return Ok(()) };
    let step = ((high - low) / CHECK_SAMPLE).max(1);
    for n in 0..CHECK_SAMPLE {
        halt(stop, shard)?;
        let found: Option<i64> = old
            .query_row(
                &format!("select id from s{shard}.releases where id >= ? order by id limit 1"),
                [low + n * step],
                |r| r.get(0),
            )
            .ok();
        let Some(id) = found else { continue };
        let (a, b) = (store::articles(&old, id)?, store::articles(&new, id)?);
        if a != b && !(copies > 0 && same_but_copies(&a, &b)) {
            bail!("shard {shard}: release {id} reads back differently from the copy; the original was kept");
        }
    }
    Ok(())
}

/// `copy` is `original` with only rows dropped that were a second copy of a
/// message-id (the one kept is one of the original's).
fn same_but_copies(original: &[crate::search::ArticleRow], copy: &[crate::search::ArticleRow]) -> bool {
    fn ids(rows: &[crate::search::ArticleRow]) -> HashSet<&str> {
        rows.iter().map(|r| r.message_id.as_str()).collect()
    }
    let kept = ids(copy);
    kept.len() == copy.len() && kept == ids(original) && copy.iter().all(|r| original.contains(r))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{Article, Release};

    fn no_stop() -> Arc<AtomicBool> {
        Arc::new(AtomicBool::new(false))
    }

    #[test]
    fn a_rewrite_that_changes_a_message_id_stops_the_shard() {
        let local = store::pack_local("Nyu1Q2z");
        // packed again the same way, kept whole: the same message-id
        assert_eq!(rewrite(3, 9, local.clone(), 5, true, Some("@ngPost>")).unwrap(), (local.clone(), 5));
        let (whole, domain) = rewrite(3, 9, local.clone(), 5, false, Some("@ngPost>")).unwrap();
        assert_eq!((store::decode(&whole, None), domain), ("<Nyu1Q2z@ngPost>".to_string(), 0));
        // a rewrite that came out different
        let err = check_same_id(3, 9, (&local, Some("@ngPost>")), (&store::pack_local("Nyu1Q2y"), Some("@ngPost>")))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("shard 3") && err.contains("<Nyu1Q2z@ngPost>") && err.contains("original was kept"),
            "{err}"
        );
    }

    fn busy(r: Result<Shrunk>) -> bool {
        r.is_err_and(|e| e.downcast_ref::<Busy>().is_some())
    }

    #[test]
    fn writes_from_outside_are_refused_while_compacting() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        db::create_db_at(&main).unwrap();
        drop(hold_off_compaction(&main).unwrap());

        // seen from the progress messages, while the run is going
        let refused = std::sync::Mutex::new(Vec::new());
        let progress = |_: &str| refused.lock().unwrap().push(hold_off_compaction(&main).err().map(|e| e.to_string()));
        run(&main, &progress, &no_stop()).unwrap();
        let refused = refused.into_inner().unwrap();
        assert!(
            !refused.is_empty()
                && refused.iter().all(|r| r.as_deref() == Some("the database is being compacted; try again later")),
            "refused all through the run: {refused:?}"
        );
        assert!(hold_off_compaction(&main).is_ok(), "and not after it");

        // stopped or failing: the lock goes too
        let stop = Arc::new(AtomicBool::new(true));
        assert!(run(&main, &|_| {}, &stop).is_err());
        assert!(hold_off_compaction(&main).is_ok());
    }

    #[test]
    fn a_second_compaction_at_once_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        db::create_db_at(&main).unwrap();

        let second = std::sync::Mutex::new(Vec::new());
        let progress = |_: &str| {
            let mut second = second.lock().unwrap();
            if second.is_empty() {
                second.push(run(&main, &|_| {}, &no_stop()));
            }
        };
        run(&main, &progress, &no_stop()).unwrap();
        let second = second.into_inner().unwrap().pop().expect("the second ran during the first");
        assert!(busy(second), "the second was refused, the first finished");
    }

    #[test]
    fn a_write_from_outside_holds_off_compaction() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        db::create_db_at(&main).unwrap();

        let write = hold_off_compaction(&main).unwrap();
        // writes dont hold each other off
        let other = hold_off_compaction(&main).unwrap();
        assert!(busy(run(&main, &|_| {}, &no_stop())));
        assert!(!store::shard_paths(&main).iter().any(|p| with_suffix(p, "compact").exists()), "nothing was started");
        drop((write, other));
        run(&main, &|_| {}, &no_stop()).unwrap();
        assert!(lock_path(&main).exists(), "the lock file stays, unlocked");
        assert!(hold_off_compaction(&main).is_ok());
    }

    /// every release's NZB rows, by id
    fn all_articles(main: &Path) -> Vec<(i64, Vec<crate::search::ArticleRow>)> {
        let conn = db::open_with_shards(main).unwrap();
        let ids: Vec<i64> = conn
            .prepare("select id from releases order by id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        ids.into_iter().map(|id| (id, store::articles(&conn, id).unwrap())).collect()
    }

    /// articles over every shard, loose rows and blobs
    fn article_total(main: &Path) -> i64 {
        store::shard_paths(main).iter().map(|p| store::article_count(&db::open_at(p).unwrap(), "main").unwrap()).sum()
    }

    /// segment rows over every shard (articles not sealed into a blob)
    fn loose(main: &Path) -> i64 {
        store::shard_paths(main)
            .iter()
            .map(|p| {
                db::open_at(p).unwrap().query_row("select count(*) from segments", [], |r| r.get::<_, i64>(0)).unwrap()
            })
            .sum()
    }

    fn domains(main: &Path) -> i64 {
        let conn = db::open_with_shards(main).unwrap();
        let sql = format!(
            "select sum(c) from ({})",
            store::each_shard(|i| format!("select count(*) as c from s{i}.domains"))
        );
        conn.query_row(&sql, [], |r| r.get(0)).unwrap()
    }

    /// every blob's segments, checked against the domains of their shard
    fn blob_segs_with_known_domains(main: &Path) -> Vec<Seg> {
        let mut all = Vec::new();
        for path in store::shard_paths(main) {
            let conn = db::open_at(&path).unwrap();
            let ids: std::collections::HashSet<i64> = conn
                .prepare("select id from domains")
                .unwrap()
                .query_map([], |r| r.get(0))
                .unwrap()
                .map(|r| r.unwrap())
                .collect();
            let blobs: Vec<Vec<u8>> = conn
                .prepare("select blob from files where blob is not null")
                .unwrap()
                .query_map([], |r| r.get(0))
                .unwrap()
                .map(|r| r.unwrap())
                .collect();
            for b in blobs {
                for seg in crate::blob::decode(&b).unwrap() {
                    assert!(seg.domain == 0 || ids.contains(&seg.domain), "{seg:?} points at a missing domain");
                    all.push(seg);
                }
            }
        }
        all
    }

    /// what the conversion left, from before locals were packed by shape: 40
    /// releases over several groups, 6 articles each, three with the shared
    /// ngPost domain (their locals stored as text) and three with a made up
    /// domain each, every domain in the table
    fn legacy_fixture(main: &Path) {
        db::create_db_at(main).unwrap();
        let releases: Vec<Release> = (0..40)
            .map(|r| Release {
                name: format!("Rel.{r}"),
                group: format!("alt.binaries.g{}", r % 5),
                articles: (1..=6)
                    .map(|p| Article {
                        message_id: if p % 2 == 0 {
                            format!("<Nyu{r}Q{p}z@ngPost>")
                        } else {
                            format!("<x{r}y{p}@Made{r}Up{p}>")
                        },
                        subject: format!("\"f.rar\" yEnc ({p}/6)"),
                        filename: Some("f.rar".into()),
                        part: Some(p),
                        total_parts: Some(6),
                        bytes: 10,
                        ..Default::default()
                    })
                    .collect(),
                ..Default::default()
            })
            .collect();
        store::save(main, &releases).unwrap();

        for path in store::shard_paths(main) {
            let conn = db::open_at(&path).unwrap();
            let whole: Vec<(i64, Vec<u8>)> = conn
                .prepare("select file_id, local from segments where domain = 0")
                .unwrap()
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap()
                .map(|r| r.unwrap())
                .collect();
            for (file_id, local) in whole {
                let id = String::from_utf8(local[1..].to_vec()).unwrap();
                let (part, suffix) = store::split_message_id(&id).unwrap();
                conn.execute("insert or ignore into domains (suffix) values (?)", [suffix]).unwrap();
                let d: i64 = conn.query_row("select id from domains where suffix = ?", [suffix], |r| r.get(0)).unwrap();
                // the old packing: anything but hex as text
                let mut text = vec![0u8];
                text.extend_from_slice(part.as_bytes());
                conn.execute(
                    "update segments set local = ?, domain = ? where file_id = ? and local = ? and domain = 0",
                    params![text, d, file_id, local],
                )
                .unwrap();
            }
            let packed: Vec<(i64, Vec<u8>, i64)> = conn
                .prepare("select file_id, local, domain from segments where local >= x'01'")
                .unwrap()
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .unwrap()
                .map(|r| r.unwrap())
                .collect();
            for (file_id, local, domain) in packed {
                let mut text = vec![0u8];
                text.extend_from_slice(store::unpack_local(&local).as_bytes());
                conn.execute(
                    "update segments set local = ? where file_id = ? and local = ? and domain = ?",
                    params![text, file_id, local, domain],
                )
                .unwrap();
            }
        }
        assert_eq!(domains(main), 120 + 5, "every made up domain has a row, ngPost one per shard used");
    }

    #[test]
    fn compacting_drops_single_use_domains_repacks_seals_and_keeps_every_nzb() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        legacy_fixture(&main);
        let before = all_articles(&main);
        let count = article_total(&main);
        assert_eq!(count, 240);

        let shrunk = run(&main, &|_| {}, &no_stop()).unwrap();
        assert_eq!(shrunk.domains_dropped, 120);
        assert_eq!(domains(&main), shrunk.domains_kept);
        assert!(shrunk.domains_kept <= 5);
        assert_eq!(all_articles(&main), before, "every NZB reads back the same");
        assert_eq!(article_total(&main), count);
        assert_eq!(loose(&main), 0, "every file was due (complete), so all are sealed");
        let segs = blob_segs_with_known_domains(&main);
        assert_eq!(segs.len() as i64, count);
        for seg in segs.iter().filter(|s| s.domain != 0) {
            assert_eq!(seg.local, store::repack(&seg.local), "packed the current way");
            assert_ne!(seg.local[0], 0, "not text");
        }
        for path in store::shard_paths(&main) {
            assert!(!with_suffix(&path, "precompact").exists(), "originals removed once all are done");
            let mode: String = db::open_at(&path).unwrap().query_row("pragma journal_mode", [], |r| r.get(0)).unwrap();
            assert_eq!(mode, "wal");
        }

        // saving goes on as before, without new single use domains
        let more = Release {
            name: "Rel.0".into(),
            group: "alt.binaries.g0".into(),
            articles: vec![Article {
                message_id: "<x0y1@Made0Up1>".into(),
                filename: Some("f.rar".into()),
                part: Some(1),
                total_parts: Some(6),
                bytes: 10,
                ..Default::default()
            }],
            ..Default::default()
        };
        store::save(&main, &[more]).unwrap();
        assert_eq!(all_articles(&main), before, "an article already there isnt saved twice");
    }

    #[test]
    fn a_huge_sparse_domain_id_costs_no_memory_to_count() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        legacy_fixture(&main);
        let before = all_articles(&main);
        // ids as sparse as earlier compactions can leave them
        for path in store::shard_paths(&main) {
            let conn = db::open_at(&path).unwrap();
            conn.execute_batch(
                "update segments set domain = domain + 4000000000000 where domain != 0;
                 update domains set id = id + 4000000000000;",
            )
            .unwrap();
        }
        let shrunk = run(&main, &|_| {}, &no_stop()).unwrap();
        assert_eq!(shrunk.domains_dropped, 120);
        assert_eq!(all_articles(&main), before, "every NZB reads back the same");
    }

    /// the file of release `name` in its group's shard: (shard path, file id)
    fn file_of(main: &Path, name: &str, group: &str) -> (PathBuf, i64) {
        let path = store::shard_path(main, store::shard_of(group));
        let id = db::open_at(&path)
            .unwrap()
            .query_row(
                "select f.id from files f join releases r on r.id = f.release_id where r.name = ? and r.group_name = ?",
                [name, group],
                |r| r.get(0),
            )
            .unwrap();
        (path, id)
    }

    #[test]
    fn compacting_sealed_shards_again_keeps_every_nzb() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        legacy_fixture(&main);
        run(&main, &|_| {}, &no_stop()).unwrap();
        assert_eq!(loose(&main), 0);

        // late articles for a sealed file, under a domain only one of them
        // ends up using (the first two are stored whole), the file untouched
        // long enough to seal again
        let late = Release {
            name: "Rel.1".into(),
            group: "alt.binaries.g1".into(),
            articles: (7..=9)
                .map(|p| Article {
                    message_id: format!("<late{p}@Late>"),
                    filename: Some("f.rar".into()),
                    part: Some(p),
                    total_parts: Some(6),
                    bytes: 20,
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        };
        store::save(&main, &[late]).unwrap();
        let (path, late_file) = file_of(&main, "Rel.1", "alt.binaries.g1");
        {
            let conn = db::open_at(&path).unwrap();
            let late_domain: i64 = conn
                .query_row(
                    "select count(*) from segments s join domains d on d.id = s.domain where d.suffix = '@Late>'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(late_domain, 1, "one row under the new domain, two whole");
            conn.execute("update files set touched_at = 0 where id = ?", [late_file]).unwrap();
        }

        // a domain only a sealed blob uses, once
        let (path, lonely_file) = file_of(&main, "Rel.2", "alt.binaries.g2");
        {
            let conn = db::open_at(&path).unwrap();
            conn.execute("insert into domains (suffix) values ('@Lonely>')", []).unwrap();
            let lonely = conn.last_insert_rowid();
            let blob: Vec<u8> =
                conn.query_row("select blob from files where id = ?", [lonely_file], |r| r.get(0)).unwrap();
            let mut segs = crate::blob::decode(&blob).unwrap();
            segs.push(Seg { part: Some(7), bytes: 30, domain: lonely, local: store::pack_local("solo7") });
            conn.execute("update files set blob = ? where id = ?", params![crate::blob::encode(&segs), lonely_file])
                .unwrap();
        }

        let before = all_articles(&main);
        let ids: Vec<&str> = before.iter().flat_map(|(_, rows)| rows.iter().map(|r| r.message_id.as_str())).collect();
        for id in ["<late7@Late>", "<late8@Late>", "<late9@Late>", "<solo7@Lonely>"] {
            assert!(ids.contains(&id), "{id} reads back before compacting");
        }
        let count = article_total(&main);
        assert_eq!(count, 240 + 4);

        let shrunk = run(&main, &|_| {}, &no_stop()).unwrap();
        assert_eq!(shrunk.domains_dropped, 2, "@Late> and @Lonely>");
        assert_eq!(all_articles(&main), before, "every NZB reads back the same");
        assert_eq!(article_total(&main), count);
        assert_eq!(loose(&main), 0, "the late rows are sealed in with the blob");
        assert_eq!(blob_segs_with_known_domains(&main).len() as i64, count);
    }

    /// `file`'s release's size and parts, and its shard's article total
    fn release_totals(shard: &Path, file: i64) -> (i64, i64, i64) {
        db::open_at(shard)
            .unwrap()
            .query_row(
                "select r.size, r.parts, (select value from meta where key = 'articles')
                 from releases r join files f on f.release_id = r.id where f.id = ?",
                [file],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap()
    }

    /// A loose row a sealed file's blob already has (same message-id) goes
    /// when compaction seals the file, instead of becoming a second copy in
    /// the blob for good.
    #[test]
    fn compacting_drops_a_loose_row_its_blob_already_has() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        legacy_fixture(&main);
        run(&main, &|_| {}, &no_stop()).unwrap();
        let before = all_articles(&main);
        let count = article_total(&main);

        let (path, file) = file_of(&main, "Rel.1", "alt.binaries.g1");
        let once = release_totals(&path, file);
        {
            let conn = db::open_at(&path).unwrap();
            let blob: Vec<u8> = conn.query_row("select blob from files where id = ?", [file], |r| r.get(0)).unwrap();
            let seg = crate::blob::decode(&blob).unwrap().remove(0);
            conn.execute(
                "insert into segments (file_id, local, domain, part, bytes) values (?, ?, ?, ?, ?)",
                params![file, seg.local, seg.domain, seg.part, seg.bytes],
            )
            .unwrap();
            conn.execute("update files set touched_at = 0 where id = ?", [file]).unwrap();
            // counted the way a save past its decode budget counts it
            conn.execute(
                "update releases set size = size + ?, parts = parts + 1
                 where id = (select release_id from files where id = ?)",
                params![seg.bytes, file],
            )
            .unwrap();
            store::add_totals(&conn, 0, 1).unwrap();
        }
        assert_eq!(article_total(&main), count + 1);

        run(&main, &|_| {}, &no_stop()).unwrap();
        assert_eq!(article_total(&main), count, "the copy went, not into the blob");
        assert_eq!(release_totals(&path, file), once, "and out of its release's and the shard's totals");
        assert_eq!(loose(&main), 0);
        assert_eq!(all_articles(&main), before, "every NZB reads back the same");
    }

    /// A sealed segment whose message-id grows past what a blob takes when its
    /// domain is dropped stays a row, so no blob outgrows `decode_capped`.
    #[test]
    fn a_dropped_domains_long_message_id_leaves_the_blob_as_a_row() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        legacy_fixture(&main);
        run(&main, &|_| {}, &no_stop()).unwrap();
        let (path, file) = file_of(&main, "Rel.1", "alt.binaries.g1");
        {
            let conn = db::open_at(&path).unwrap();
            let suffix = format!("@{}>", "d".repeat(240));
            conn.execute("insert into domains (suffix) values (?)", [&suffix]).unwrap();
            let d: i64 = conn.query_row("select id from domains where suffix = ?", [&suffix], |r| r.get(0)).unwrap();
            let blob: Vec<u8> = conn.query_row("select blob from files where id = ?", [file], |r| r.get(0)).unwrap();
            let mut segs = crate::blob::decode(&blob).unwrap();
            let mut local = vec![0u8];
            local.extend(std::iter::repeat_n(b'a', 300));
            segs.push(Seg { part: Some(99), bytes: 5, domain: d, local });
            conn.execute("update files set blob = ? where id = ?", params![crate::blob::encode(&segs), file]).unwrap();
        }
        let before = all_articles(&main);
        run(&main, &|_| {}, &no_stop()).unwrap();
        assert_eq!(all_articles(&main), before, "every NZB reads back the same");
        let conn = db::open_at(&path).unwrap();
        let blob: Vec<u8> = conn.query_row("select blob from files where id = ?", [file], |r| r.get(0)).unwrap();
        assert!(crate::blob::decode(&blob).unwrap().iter().all(|s| s.local.len() <= crate::blob::MAX_LOCAL));
        let rows: i64 =
            conn.query_row("select count(*) from segments where file_id = ?", [file], |r| r.get(0)).unwrap();
        assert_eq!(rows, 1, "the long one is a row");
    }

    #[test]
    fn a_file_over_the_cap_is_copied_as_rows_and_its_nzb_is_the_same() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        legacy_fixture(&main);
        // a complete file (so due) whose rows a poster padded past what a blob takes
        let (path, big) = file_of(&main, "Rel.1", "alt.binaries.g1");
        let conn = db::open_at(&path).unwrap();
        let before_rows: i64 =
            conn.query_row("select count(*) from segments where file_id = ?", [big], |r| r.get(0)).unwrap();
        let extra = store::SEAL_MAX_SEGMENTS + 1 - before_rows;
        conn.execute(
            "with recursive n(i) as (select 1 union all select i + 1 from n where i < ?2)
             insert into segments (file_id, local, domain, part, bytes)
             select ?1, cast('big' || i as blob), 0, 100 + i, 1 from n",
            params![big, extra],
        )
        .unwrap();
        drop(conn);
        let before = all_articles(&main);
        let count = article_total(&main);

        run(&main, &|_| {}, &no_stop()).unwrap();
        assert_eq!(all_articles(&main), before, "every NZB reads back the same");
        assert_eq!(article_total(&main), count);
        assert_eq!(loose(&main), store::SEAL_MAX_SEGMENTS + 1, "only the big file stays rows");
        let conn = db::open_at(&path).unwrap();
        let (rows, blob): (i64, Option<Vec<u8>>) = conn
            .query_row(
                "select (select count(*) from segments where file_id = f.id), f.blob from files f where id = ?",
                [big],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((rows, blob), (store::SEAL_MAX_SEGMENTS + 1, None));

        // and without the extra row it seals again
        conn.execute("delete from segments where file_id = ? and local = cast('big1' as blob)", [big]).unwrap();
        drop(conn);
        run(&main, &|_| {}, &no_stop()).unwrap();
        assert_eq!(loose(&main), 0, "at the cap a file seals");
    }

    #[test]
    fn a_corrupt_blob_stops_compacting_and_keeps_the_original() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        legacy_fixture(&main);
        run(&main, &|_| {}, &no_stop()).unwrap();
        let (path, file) = file_of(&main, "Rel.3", "alt.binaries.g3");
        db::open_at(&path).unwrap().execute("update files set blob = x'00' where id = ?", [file]).unwrap();

        let err = run(&main, &|_| {}, &no_stop()).unwrap_err();
        assert!(format!("{err:#}").contains("corrupt"), "{err:#}");
        let blob: Vec<u8> =
            db::open_at(&path).unwrap().query_row("select blob from files where id = ?", [file], |r| r.get(0)).unwrap();
        assert_eq!(blob, vec![0], "the original is still in place");
    }

    /// NZB rows of every release outside shard `skip`, by id
    fn articles_outside(main: &Path, skip: usize) -> Vec<(i64, Vec<crate::search::ArticleRow>)> {
        let conn = db::open_with_shards(main).unwrap();
        let sql = (0..SHARDS)
            .filter(|i| *i != skip)
            .map(|i| format!("select id from s{i}.releases"))
            .collect::<Vec<_>>()
            .join(" union all ");
        let mut ids: Vec<i64> =
            conn.prepare(&sql).unwrap().query_map([], |r| r.get(0)).unwrap().map(|r| r.unwrap()).collect();
        ids.sort();
        ids.into_iter().map(|id| (id, store::articles(&conn, id).unwrap())).collect()
    }

    #[test]
    fn a_failed_shard_doesnt_stop_the_others() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        legacy_fixture(&main);
        let (path, file) = file_of(&main, "Rel.3", "alt.binaries.g3");
        let bad = store::shard_of("alt.binaries.g3");
        let before = articles_outside(&main, bad);
        assert!(!before.is_empty());
        db::open_at(&path).unwrap().execute("update files set blob = x'00' where id = ?", [file]).unwrap();

        let err = run(&main, &|_| {}, &no_stop()).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains(&format!("shard {bad}:")) && msg.contains("corrupt"), "{msg}");

        // the others were compacted (sealed) and their backups removed
        let loose_in = |p: &Path| -> i64 {
            db::open_at(p).unwrap().query_row("select count(*) from segments", [], |r| r.get(0)).unwrap()
        };
        for (i, shard) in store::shard_paths(&main).iter().enumerate() {
            assert!(!with_suffix(shard, "precompact").exists(), "no backup left over for shard {i}");
            assert!(!with_suffix(shard, "compact").exists(), "no half made copy left over for shard {i}");
            if i == bad {
                assert!(loose_in(shard) > 0, "the failed shard still has its loose rows");
            } else {
                assert_eq!(loose_in(shard), 0, "shard {i} was compacted and sealed");
            }
        }
        assert_eq!(articles_outside(&main, bad), before, "the others read back the same");

        // the failed one is untouched
        let blob: Vec<u8> =
            db::open_at(&path).unwrap().query_row("select blob from files where id = ?", [file], |r| r.get(0)).unwrap();
        assert_eq!(blob, vec![0]);
    }

    /// no shard is half done: the files are all there, the NZBs read back the
    /// same, and nothing from the copying is left lying around
    fn assert_untouched_or_whole(main: &Path, before: &[(i64, Vec<crate::search::ArticleRow>)]) {
        for shard in store::shard_paths(main) {
            assert!(shard.exists(), "{} is still there", shard.display());
            assert!(!with_suffix(&shard, "precompact").exists(), "no backup left over");
            assert!(!with_suffix(&shard, "compact").exists(), "no half made copy left over");
        }
        assert_eq!(all_articles(main), before, "every NZB reads back the same");
    }

    #[test]
    fn a_stop_before_the_start_leaves_every_shard_as_it_was() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        legacy_fixture(&main);
        let before = all_articles(&main);
        let (rows, count) = (loose(&main), article_total(&main));

        let stop = Arc::new(AtomicBool::new(true));
        let err = run(&main, &|_| {}, &stop).unwrap_err();
        assert!(format!("{err:#}").contains("stopped"), "{err:#}");
        assert_untouched_or_whole(&main, &before);
        assert_eq!((loose(&main), article_total(&main)), (rows, count), "nothing was sealed or dropped");
    }

    #[test]
    fn a_stop_while_copying_drops_the_copies_and_keeps_the_originals() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        legacy_fixture(&main);
        let before = all_articles(&main);

        // the flag goes up as the first shard reports the copy of its files
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let progress = move |msg: &str| {
            if msg.contains("copying files and articles") {
                flag.store(true, Ordering::Relaxed);
            }
        };
        let err = run(&main, &progress, &stop).unwrap_err();
        assert!(format!("{err:#}").contains("stopped"), "{err:#}");
        assert!(stop.load(Ordering::Relaxed));
        assert_untouched_or_whole(&main, &before);
    }

    /// what a swap cut short after its first rename leaves: the shard of
    /// `group` moved aside as `.precompact.db`, its checked copy still next
    /// to it as `.compact.db`, nothing at the shard's own path
    fn cut_swap(main: &Path, group: &str) -> PathBuf {
        let path = store::shard_path(main, store::shard_of(group));
        let copy = with_suffix(&path, "compact");
        std::fs::copy(&path, &copy).unwrap();
        std::fs::rename(&path, with_suffix(&path, "precompact")).unwrap();
        for suffix in ["-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
        assert!(!path.exists() && copy.exists());
        path
    }

    #[test]
    fn a_swap_cut_short_gets_its_original_back_on_the_next_start() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        legacy_fixture(&main);
        let before = all_articles(&main);
        let path = cut_swap(&main, "alt.binaries.g3");

        db::create_db_at(&main).unwrap();
        assert!(path.exists(), "the original is back in place");
        assert!(!with_suffix(&path, "precompact").exists(), "moved back, not copied");
        assert!(!with_suffix(&path, "compact").exists(), "the copy went");
        assert_eq!(all_articles(&main), before, "every NZB reads back the same");
    }

    #[test]
    fn a_missing_shard_with_others_there_is_refused_not_made_empty() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        legacy_fixture(&main);
        let group = "alt.binaries.g3";
        let path = store::shard_path(&main, store::shard_of(group));

        // no backup at all
        remove_db(&path);
        let err = format!("{:#}", db::create_db_at(&main).unwrap_err());
        assert!(err.contains(&path.display().to_string()) && err.contains("missing"), "{err}");
        assert!(!path.exists(), "no empty shard in its place");

        // only a compacted copy, which may not have been checked: kept, still refused
        let copy = with_suffix(&path, "compact");
        std::fs::copy(store::shard_path(&main, (store::shard_of(group) + 1) % SHARDS), &copy).unwrap();
        let err = format!("{:#}", db::create_db_at(&main).unwrap_err());
        assert!(err.contains(&copy.display().to_string()), "{err}");
        assert!(!path.exists() && copy.exists(), "the copy is left for whoever puts it back");

        // writers dont make it either
        let release = Release { name: "Late".into(), group: group.into(), ..Default::default() };
        assert!(store::save(&main, &[release]).is_err());
        assert!(!path.exists(), "a save made no empty shard");
    }

    #[test]
    fn a_shard_is_left_alone_while_a_compaction_holds_the_lock() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        legacy_fixture(&main);
        let before = all_articles(&main);
        let path = cut_swap(&main, "alt.binaries.g3");

        // mid swap of a running compaction: nothing set up, nothing put back
        let compacting = Lock::take(&main).unwrap();
        assert!(db::create_db_holding(&main).unwrap().is_none());
        assert!(!path.exists() && with_suffix(&path, "precompact").exists(), "the swap is left to the compaction");
        drop(compacting);

        let held = db::create_db_holding(&main).unwrap().expect("set up once the compaction is gone");
        assert!(busy(run(&main, &|_| {}, &no_stop())), "and compaction held off while held");
        drop(held);
        assert_eq!(all_articles(&main), before);
    }

    #[test]
    fn a_compaction_puts_back_what_a_cut_swap_left_before_starting() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        legacy_fixture(&main);
        let before = all_articles(&main);
        cut_swap(&main, "alt.binaries.g3");

        run(&main, &|_| {}, &no_stop()).unwrap();
        assert_untouched_or_whole(&main, &before);
    }

    #[test]
    fn a_backup_left_next_to_a_shard_in_place_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        legacy_fixture(&main);
        let before = all_articles(&main);
        // cut after the copy went in: both there, and which is whole isnt known
        let path = store::shard_path(&main, store::shard_of("alt.binaries.g3"));
        let backup = with_suffix(&path, "precompact");
        std::fs::copy(&path, &backup).unwrap();

        db::create_db_at(&main).unwrap();
        assert!(backup.exists(), "kept on start");
        let err = format!("{:#}", run(&main, &|_| {}, &no_stop()).unwrap_err());
        assert!(err.contains(&backup.display().to_string()), "{err}");
        assert!(backup.exists(), "kept by a compaction too");
        assert_eq!(all_articles(&main), before);
    }

    #[test]
    fn a_shard_and_its_backup_together_refuse_writes_but_not_the_menu() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        legacy_fixture(&main);
        let before = all_articles(&main);

        // a normal database, no backups: untouched
        drop(hold_off_compaction(&main).unwrap());
        drop(db::create_db_holding(&main).unwrap().unwrap());

        // an older atlas made an empty shard next to the backup: which to keep isnt known
        let path = store::shard_path(&main, store::shard_of("alt.binaries.g3"));
        let backup = with_suffix(&path, "precompact");
        std::fs::copy(&path, &backup).unwrap();

        // writes are refused, with both files named and how to go on
        let err = format!("{:#}", hold_off_compaction(&main).err().expect("refused"));
        assert!(err.contains(&path.display().to_string()) && err.contains(&backup.display().to_string()), "{err}");
        assert!(err.contains("keep") && err.contains("delete"), "says how to resolve it: {err}");
        // the lock isnt kept by a refused hold: a compaction gets its own refusal, not Busy
        assert!(!busy(run(&main, &|_| {}, &no_stop())));

        // the menu still starts and reads
        drop(db::create_db_holding(&main).unwrap().expect("set up"));
        assert_eq!(all_articles(&main), before);

        // resolved: the shard is kept, the backup removed
        std::fs::remove_file(&backup).unwrap();
        drop(hold_off_compaction(&main).unwrap());
    }

    /// A checkpoint held back by a reader (its snapshot keeps WAL frames from
    /// being folded in) is an error, not taken for done.
    #[test]
    fn a_checkpoint_a_reader_holds_back_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.db");
        let writer = db::open_at(&path).unwrap();
        writer.execute_batch("pragma journal_mode = wal; pragma wal_autocheckpoint = 0; create table t (x)").unwrap();
        let reader = db::open_at(&path).unwrap();
        reader.execute_batch("begin; select count(*) from t").unwrap();
        writer.execute("insert into t values (1)", []).unwrap();

        writer.busy_timeout(std::time::Duration::ZERO).unwrap();
        let err = checkpoint(&writer).unwrap_err();
        assert!(format!("{err:#}").contains("checkpoint"), "{err:#}");
        reader.execute_batch("commit").unwrap();
        checkpoint(&writer).unwrap();
    }

    /// A shard moved aside with pages still in its WAL (committed, not
    /// folded in) gets them back with it when the swap was cut short.
    #[test]
    fn a_cut_swap_puts_the_originals_wal_back_with_it() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        legacy_fixture(&main);
        let path = store::shard_path(&main, store::shard_of("alt.binaries.g3"));
        let backup = with_suffix(&path, "precompact");
        {
            // the moved aside original: its file and its WAL, taken while a
            // connection keeps a write in the WAL
            let conn = db::open_at(&path).unwrap();
            conn.execute_batch("pragma wal_autocheckpoint = 0; create table late (x); insert into late values (7)")
                .unwrap();
            std::fs::copy(&path, &backup).unwrap();
            std::fs::copy(format!("{}-wal", path.display()), format!("{}-wal", backup.display())).unwrap();
        }
        remove_db(&path);

        recover_cut_swaps(&main).unwrap();
        let late: i64 = db::open_at(&path).unwrap().query_row("select x from late", [], |r| r.get(0)).unwrap();
        assert_eq!(late, 7, "the write in the WAL came back too");
        assert!(!Path::new(&format!("{}-wal", backup.display())).exists());
    }
}
