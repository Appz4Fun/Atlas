//! What each file's sealed blob holds in bytes, kept in `files.blob_bytes` beside the blob, soo a file's bytes are that
//! plus a sum over its loose `segments` rows, without decoding the blob or fetching it back from the sealed log.
//!
//! Whatever writes a blob writes its count (`of_segs`): sealing (`store::seal_rows`), compaction, the repost split.
//! A blob written without it gets NULL from the shard's trigger (`store::migrate_shard`), never a stale count, and
//! readers decode a blob whose count is NULL as before. Moving a blob to the sealed log keeps the count, and counts
//! one that has none. Blobs from before the column are counted by `spawn`, a slow pass in the indexer.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use rusqlite::{Connection, OptionalExtension, Result, params};

use crate::blob::Seg;
use crate::store;

/// The bytes of a blob's articles.
pub fn of_segs(segs: &[Seg]) -> i64 {
    segs.iter().map(|s| s.bytes).sum()
}

/// The bytes of a stored blob's articles, decoding it (and fetching it from the sealed log when it's a place there).
pub fn of_blob(blob: &[u8]) -> std::io::Result<i64> {
    Ok(of_segs(&crate::blob::decode(blob)?))
}

/// Each file of a release by name, and the bytes of every article it holds: its blob's and its rows', as
/// `store::each_article` would add them up. Files of one name add up, as there. A blob not counted yet is decoded.
pub fn of_release(conn: &Connection, release_id: i64) -> Result<HashMap<String, i64>> {
    let s = store::shard_of_id(release_id);
    // the rows, the counts and an uncounted blob from one snapshot: a blob sealed in between would hold rows counted
    // already. A caller already in a transaction has one
    let _snapshot = if conn.is_autocommit() { Some(conn.unchecked_transaction()?) } else { None };
    let mut stmt = conn.prepare_cached(&format!(
        "select f.id, f.filename, f.blob is not null, f.blob_bytes,
                (select coalesce(sum(g.bytes), 0) from s{s}.segments g where g.file_id = f.id)
         from s{s}.files f where f.release_id = ?"
    ))?;
    let files: Vec<(i64, String, bool, Option<i64>, i64)> = stmt
        .query_map([release_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?
        .collect::<Result<_>>()?;
    let mut out: HashMap<String, i64> = HashMap::new();
    for (id, filename, has_blob, counted, rows) in files {
        let sealed = match (has_blob, counted) {
            (false, _) => 0,
            (true, Some(bytes)) => bytes,
            (true, None) => {
                let blob: Option<Vec<u8>> = conn
                    .prepare_cached(&format!("select blob from s{s}.files where id = ?"))?
                    .query_row([id], |r| r.get(0))
                    .optional()?
                    .flatten();
                match blob {
                    Some(blob) => of_blob(&blob).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?,
                    None => 0,
                }
            }
        };
        *out.entry(filename).or_default() += sealed + rows;
    }
    Ok(out)
}

/// `of_release` without decoding anything: None when a file's sealed blob isn't counted yet, soo a caller that can't
/// wait on the sealed log (a list of results) goes without.
pub fn counted_of_release(conn: &Connection, release_id: i64) -> Result<Option<HashMap<String, i64>>> {
    let s = store::shard_of_id(release_id);
    let mut stmt = conn.prepare_cached(&format!(
        "select f.filename, f.blob is not null, f.blob_bytes,
                (select coalesce(sum(g.bytes), 0) from s{s}.segments g where g.file_id = f.id)
         from s{s}.files f where f.release_id = ?"
    ))?;
    let files: Vec<(String, bool, Option<i64>, i64)> =
        stmt.query_map([release_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?.collect::<Result<_>>()?;
    let mut out: HashMap<String, i64> = HashMap::new();
    for (filename, has_blob, counted, rows) in files {
        let sealed = match (has_blob, counted) {
            (false, _) => 0,
            (true, Some(bytes)) => bytes,
            (true, None) => return Ok(None),
        };
        *out.entry(filename).or_default() += sealed + rows;
    }
    Ok(Some(out))
}

/// Blobs counted in one batch, and so the most decoded between two looks at `stop`.
const BATCH: i64 = 256;
/// The most ids one batch looks through, soo a read where few are uncounted stays short.
const WINDOW: i64 = 50_000;
/// The most blobs one batch fetches back from the sealed log, soo the pass takes little from NZB fetches there.
const FETCHES: usize = 32;
/// The rest after a batch that couldn't fetch from the sealed log: it tries the same blob again then.
const FETCH_REST: Duration = Duration::from_secs(60);
/// The tries at fetching one blob before the pass gives up on it (the log may never give it back): it stays
/// uncounted, read as before.
const FETCH_TRIES: u32 = 5;
/// The rest after a batch that fetched from the sealed log, whichever shard: at most `FETCHES` a second or so.
const FETCHED_REST: Duration = Duration::from_secs(1);
/// A rest after each batch, soo the pass takes little from the indexer's writes and the sealed log.
const REST: Duration = Duration::from_millis(200);
/// How long a write waits on the shard's writer, in tries of `busy_timeout` each.
const WRITE_TRIES: usize = 20;
/// How long after a shard's pass the next one starts: a blob written without its count after the first is rare (a
/// writer this file doesn't know of), and a pass reads through every blob left in the shard.
const PASS_EVERY: Duration = Duration::from_secs(30 * 24 * 3600);

fn cursor_path(shard: &Path) -> PathBuf {
    let mut name = shard.as_os_str().to_owned();
    name.push(".blob-bytes-cursor");
    PathBuf::from(name)
}

/// Where a shard's pass is: the last file looked at, and when its last pass ended (0 while one is under way).
fn read_cursor(shard: &Path) -> (i64, i64) {
    let text = std::fs::read_to_string(cursor_path(shard)).unwrap_or_default();
    let mut it = text.split_whitespace().map(|v| v.parse::<i64>().unwrap_or(0));
    (it.next().unwrap_or(0), it.next().unwrap_or(0))
}

fn write_cursor(shard: &Path, after: i64, done_at: i64) {
    let _ = std::fs::write(cursor_path(shard), format!("{after} {done_at}"));
}

/// What one batch did.
#[derive(Debug, PartialEq, Eq)]
pub struct Batch {
    /// where the next batch starts; None: the shard has no more
    pub next: Option<i64>,
    pub counted: usize,
    /// a blob in the sealed log couldn't be fetched: the next batch starts at it
    pub fetch_failed: bool,
    /// blobs fetched from the sealed log
    pub fetched: usize,
    /// the failed fetch couldn't reach the sealed log at all (not counted as a try at that blob)
    pub log_down: bool,
}

/// Whether a fetch failed on the way to the sealed log (it's down, or no address is set), not on what it answered.
fn log_down(e: &std::io::Error) -> bool {
    use std::io::ErrorKind::*;
    matches!(
        e.kind(),
        NotConnected
            | ConnectionRefused
            | ConnectionReset
            | ConnectionAborted
            | TimedOut
            | BrokenPipe
            | WouldBlock
            | AddrNotAvailable
            | HostUnreachable
            | NetworkUnreachable
            | NetworkDown
    )
}

/// One batch of a shard's uncounted blobs past `after` (up to `WINDOW` ids on) counted. A blob that won't decode stays
/// uncounted and is passed over; one in the sealed log that can't be fetched is tried again by the next batch.
pub fn count_batch(conn: &Connection, after: i64, stop: &AtomicBool) -> Result<Batch> {
    count_batch_with(conn, after, stop, 0, &fetch)
}

/// A blob in the sealed log, fetched: what it stands for.
fn fetch(place: &[u8]) -> std::io::Result<Vec<u8>> {
    let at = sealed_log::Locator::from_ref(crate::blob::inner(place))
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "not a place in the sealed log"))?;
    crate::sealed_store::fetch(at)
}

/// `count_batch`, a blob in the sealed log got by `fetch`, and blobs in the shard over `skip_over` bytes (0: none)
/// left to the sealed log's offload, which counts them as it moves them.
fn count_batch_with(
    conn: &Connection,
    after: i64,
    stop: &AtomicBool,
    skip_over: usize,
    fetch: &dyn Fn(&[u8]) -> std::io::Result<Vec<u8>>,
) -> Result<Batch> {
    let batch: Vec<(i64, Vec<u8>)> = conn
        .prepare_cached(
            "select id, blob from files where id > ?1 and id <= ?1 + ?2 and blob is not null and blob_bytes is null
             and (?4 = 0 or length(blob) <= ?4) order by id limit ?3",
        )?
        .query_map(params![after, WINDOW, BATCH, skip_over as i64], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_>>()?;
    let full = batch.len() as i64 == BATCH;
    let mut next = match batch.last() {
        Some((id, _)) if full => *id,
        // none uncounted left in the window: on past it, or done past the last file
        _ => {
            let last: Option<i64> = conn.query_row("select max(id) from files", [], |r| r.get(0))?;
            if last.is_none_or(|last| after + WINDOW >= last) {
                if batch.is_empty() {
                    return Ok(Batch { next: None, counted: 0, fetch_failed: false, fetched: 0, log_down: false });
                }
                batch.last().map_or(after, |b| b.0)
            } else {
                after + WINDOW
            }
        }
    };
    let mut counted: Vec<(i64, &[u8], i64)> = Vec::new();
    let (mut fetches, mut fetch_failed, mut down) = (0usize, false, false);
    for (id, blob) in &batch {
        if stop.load(Ordering::Relaxed) {
            return Ok(Batch { next: Some(after), counted: 0, fetch_failed: false, fetched: 0, log_down: false });
        }
        let remote = crate::blob::is_remote(blob);
        if remote && fetches == FETCHES {
            next = id - 1;
            break;
        }
        fetches += usize::from(remote);
        // the sealed log out of reach: this blob again later. A blob that won't decode, fetched or not, is passed over
        let fetched = if remote {
            match fetch(blob) {
                Ok(raw) => Some(raw),
                Err(e) => {
                    next = id - 1;
                    fetch_failed = true;
                    down = log_down(&e);
                    break;
                }
            }
        } else {
            None
        };
        match of_blob(fetched.as_deref().unwrap_or(blob)) {
            Ok(bytes) => counted.push((*id, blob, bytes)),
            Err(e) => eprintln!("blob bytes: file {id} left uncounted: {e}"),
        }
    }
    if counted.is_empty() {
        return Ok(Batch { next: Some(next), counted: 0, fetch_failed, fetched: fetches, log_down: down });
    }
    let mut tries = 0;
    loop {
        match conn.execute_batch("begin immediate") {
            Ok(()) => break,
            Err(rusqlite::Error::SqliteFailure(e, _))
                if e.code == rusqlite::ErrorCode::DatabaseBusy
                    && tries + 1 < WRITE_TRIES
                    && !stop.load(Ordering::Relaxed) =>
            {
                tries += 1;
            }
            Err(e) => return Err(e),
        }
    }
    let written = (|| {
        // only a blob that's still the one counted gets the count
        let mut set =
            conn.prepare_cached("update files set blob_bytes = ? where id = ? and blob = ? and blob_bytes is null")?;
        let mut n = 0;
        for (id, blob, bytes) in &counted {
            n += set.execute(params![bytes, id, blob])?;
        }
        Ok(n)
    })();
    match written {
        Ok(n) => conn.execute_batch("commit").map(|()| Batch {
            next: Some(next),
            counted: n,
            fetch_failed,
            fetched: fetches,
            log_down: down,
        }),
        Err(e) => {
            let _ = conn.execute_batch("rollback");
            Err(e)
        }
    }
}

/// The uncounted blobs of one release's files counted, on its shard's connection, fetching from the sealed log one at
/// a time. What it counted, and the blobs it couldn't (a fetch or decode that failed: they stay uncounted).
pub fn count_release(conn: &Connection, release_id: i64, stop: &AtomicBool) -> Result<(usize, usize)> {
    count_release_with(conn, release_id, stop, &fetch)
}

fn count_release_with(
    conn: &Connection,
    release_id: i64,
    stop: &AtomicBool,
    fetch: &dyn Fn(&[u8]) -> std::io::Result<Vec<u8>>,
) -> Result<(usize, usize)> {
    let blobs: Vec<(i64, Vec<u8>)> = conn
        .prepare_cached("select id, blob from files where release_id = ?1 and blob is not null and blob_bytes is null")?
        .query_map([release_id], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_>>()?;
    let (mut counted, mut failed) = (Vec::new(), 0usize);
    for (id, blob) in &blobs {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let raw = if crate::blob::is_remote(blob) { fetch(blob).ok() } else { Some(blob.clone()) };
        match raw.as_deref().map(of_blob) {
            Some(Ok(bytes)) => counted.push((*id, blob, bytes)),
            _ => failed += 1,
        }
    }
    if counted.is_empty() {
        return Ok((0, failed));
    }
    let mut tries = 0;
    loop {
        match conn.execute_batch("begin immediate") {
            Ok(()) => break,
            Err(rusqlite::Error::SqliteFailure(e, _))
                if e.code == rusqlite::ErrorCode::DatabaseBusy && tries + 1 < WRITE_TRIES =>
            {
                tries += 1;
            }
            Err(e) => return Err(e),
        }
    }
    let written = (|| {
        // only a blob that's still the one counted gets the count
        let mut set =
            conn.prepare_cached("update files set blob_bytes = ? where id = ? and blob = ? and blob_bytes is null")?;
        let mut n = 0;
        for (id, blob, bytes) in &counted {
            n += set.execute(params![bytes, id, blob])?;
        }
        Ok(n)
    })();
    match written {
        Ok(n) => conn.execute_batch("commit").map(|()| (n, failed)),
        Err(e) => {
            let _ = conn.execute_batch("rollback");
            Err(e)
        }
    }
}

/// Every shard's uncounted blobs counted, a batch at a time, turn about, until `stop`; again every `PASS_EVERY`. A
/// shard whose batch couldn't reach the sealed log waits `FETCH_REST`; the others go on.
fn run(main: &Path, skip_over: usize, stop: &AtomicBool) {
    let shards = store::shard_paths(main);
    let nap = |d: Duration| {
        let until = Instant::now() + d;
        while Instant::now() < until && !stop.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(200).min(d));
        }
    };
    let mut waiting: HashMap<PathBuf, Instant> = HashMap::new();
    // each shard's blob the log wouldn't give back, and how many times
    let mut failing: HashMap<PathBuf, (i64, u32)> = HashMap::new();
    let mut total = 0usize;
    while !stop.load(Ordering::Relaxed) {
        let now = chrono::Utc::now().timestamp();
        let mut busy = false;
        for shard in &shards {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            let (after, done_at) = read_cursor(shard);
            if done_at > 0 && now - done_at < PASS_EVERY.as_secs() as i64 {
                continue;
            }
            busy = true;
            if waiting.get(shard).is_some_and(|until| Instant::now() < *until) {
                continue;
            }
            let after = if done_at > 0 { 0 } else { after };
            // a connection a batch, as the sealed log's offload: none is held while a compaction swaps the shard
            let batch = crate::db::open_at(shard).and_then(|conn| {
                // its commits don't copy the shard's WAL each time (see the constant)
                conn.execute_batch(&format!(
                    "pragma wal_autocheckpoint = {}",
                    crate::db::OCCASIONAL_WRITER_AUTOCHECKPOINT
                ))?;
                count_batch_with(&conn, after, stop, skip_over, &fetch)
            });
            match batch {
                Ok(Batch { next: Some(next), counted, fetch_failed, fetched, log_down }) => {
                    total += counted;
                    let mut next = next;
                    if fetch_failed && log_down {
                        // the log out of reach: no try at the blob, the same one again in a while
                        waiting.insert(shard.clone(), Instant::now() + FETCH_REST);
                    } else if fetch_failed {
                        // the blob after `next` wasn't fetched: again in a while, given up on after `FETCH_TRIES`
                        let tries = match failing.get(shard) {
                            Some((id, n)) if *id == next + 1 => n + 1,
                            _ => 1,
                        };
                        if tries >= FETCH_TRIES {
                            eprintln!("blob bytes: file {} not fetched {tries} times, left uncounted", next + 1);
                            failing.remove(shard);
                            next += 1;
                        } else {
                            failing.insert(shard.clone(), (next + 1, tries));
                            waiting.insert(shard.clone(), Instant::now() + FETCH_REST);
                        }
                    }
                    write_cursor(shard, next, 0);
                    if fetched > 0 {
                        nap(FETCHED_REST);
                    }
                }
                Ok(Batch { next: None, .. }) => {
                    println!("blob bytes: {} done, {total} blobs counted so far", shard.display());
                    write_cursor(shard, 0, now);
                }
                Err(e) => {
                    eprintln!("blob bytes: {}: {e}", shard.display());
                    waiting.insert(shard.clone(), Instant::now() + FETCH_REST);
                }
            }
        }
        nap(if busy { REST } else { Duration::from_secs(3600) });
    }
}

/// The pass that counts blobs from before `blob_bytes`, on a thread of its own in the indexer. With the sealed log's
/// offload on, it leaves the blobs the offload will move (and count) to it.
pub fn spawn(config: &crate::config::SealedLogConfig, stop: Arc<AtomicBool>) {
    let main = crate::paths::database();
    let skip_over = if config.offload && !config.addr.is_empty() { crate::sealed_store::SMALL_BLOB } else { 0 };
    let _ = std::thread::Builder::new().name("blob-bytes".into()).spawn(move || run(&main, skip_over, &stop));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segs(sizes: &[i64]) -> Vec<Seg> {
        sizes
            .iter()
            .enumerate()
            .map(|(i, b)| Seg { part: Some(i as i64 + 1), bytes: *b, domain: 0, local: format!("m{i}").into_bytes() })
            .collect()
    }

    fn shard() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("atlas.s0.db");
        store::create_shard(&path).unwrap();
        let conn = crate::db::open_at(&path).unwrap();
        (dir, conn)
    }

    #[test]
    fn a_releases_uncounted_blobs_are_counted_fetching_what_is_in_the_sealed_log() {
        let (_dir, conn) = shard();
        let place = |n: u8| {
            let mut place = sealed_log::REF_TAG.to_vec();
            place.resize(sealed_log::REF_BYTES, n);
            place
        };
        let local = crate::blob::encode(&segs(&[10, 20]));
        conn.execute("insert into files (id, release_id, filename, seen, blob) values (1, 9, 'a', x'06', ?)", [&local])
            .unwrap();
        conn.execute(
            "insert into files (id, release_id, filename, seen, blob) values (2, 9, 'b', x'02', ?)",
            [place(7)],
        )
        .unwrap();
        conn.execute(
            "insert into files (id, release_id, filename, seen, blob) values (3, 9, 'c', x'02', ?)",
            [place(8)],
        )
        .unwrap();
        conn.execute("insert into files (id, release_id, filename, seen, blob) values (4, 5, 'd', x'02', ?)", [&local])
            .unwrap();
        // the log gives back the first place and not the second
        let fetch = |p: &[u8]| {
            if p.ends_with(&[7]) {
                Ok(crate::blob::encode(&segs(&[40])))
            } else {
                Err(std::io::Error::new(std::io::ErrorKind::NotFound, "gone"))
            }
        };
        let stop = AtomicBool::new(false);
        assert_eq!(count_release_with(&conn, 9, &stop, &fetch).unwrap(), (2, 1));
        let counts: Vec<Option<i64>> = conn
            .prepare("select blob_bytes from files order by id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_>>()
            .unwrap();
        assert_eq!(counts, [Some(30), Some(40), None, None], "another release's file left alone");
        assert_eq!(count_release_with(&conn, 9, &stop, &fetch).unwrap(), (0, 1), "a run again counts nothing twice");
    }

    #[test]
    fn a_blob_written_without_its_count_has_none_rather_than_a_stale_one() {
        let (_dir, conn) = shard();
        let blob = crate::blob::encode(&segs(&[10, 20]));
        conn.execute(
            "insert into files (id, release_id, filename, seen, blob, blob_bytes) values (1, 1, 'a', x'06', ?, 30)",
            [&blob],
        )
        .unwrap();
        let count = || conn.query_row("select blob_bytes from files where id = 1", [], |r| r.get::<_, Option<i64>>(0));
        // the same blob, written again: the count stands
        conn.execute("update files set blob = ? where id = 1", [&blob]).unwrap();
        assert_eq!(count().unwrap(), Some(30));
        // written with its count
        conn.execute(
            "update files set blob = ?, blob_bytes = 60 where id = 1",
            [crate::blob::encode(&segs(&[30, 30]))],
        )
        .unwrap();
        assert_eq!(count().unwrap(), Some(60));
        // another blob, the count left as it was: unknown, not wrong
        conn.execute("update files set blob = ? where id = 1", [crate::blob::encode(&segs(&[5]))]).unwrap();
        assert_eq!(count().unwrap(), None);
        // a place in the sealed log in its stead keeps the count of what's there
        conn.execute("update files set blob_bytes = 5 where id = 1", []).unwrap();
        let mut place = sealed_log::REF_TAG.to_vec();
        place.resize(sealed_log::REF_BYTES, 7);
        conn.execute("update files set blob = ? where id = 1", [&place]).unwrap();
        assert_eq!(count().unwrap(), Some(5));
        // no blob at all: no count
        conn.execute("update files set blob = null where id = 1", []).unwrap();
        assert_eq!(count().unwrap(), None);
    }

    #[test]
    fn a_releases_file_bytes_are_what_its_articles_add_up_to() {
        let dir = tempfile::tempdir().unwrap();
        let conn = Connection::open_in_memory().unwrap();
        for s in 0..store::SHARDS {
            let path = dir.path().join(format!("atlas.s{s}.db"));
            store::create_shard(&path).unwrap();
            conn.execute(&format!("attach database ?1 as s{s}"), [path.to_string_lossy()]).unwrap();
        }
        let release = store::global_id(7, 0);
        conn.execute(
            "insert into s0.releases (id, name, group_name, size, complete, parts) values (?, 'r', 'g', 0, 0, 0)",
            [release],
        )
        .unwrap();
        // a: a counted blob and a loose row; b: an uncounted blob; c: rows only
        type File<'a> = (i64, &'a str, Option<Vec<i64>>, bool, &'a [i64]);
        let files: [File; 3] = [
            (1, "a", Some(vec![10, 20]), true, &[5]),
            (2, "b", Some(vec![100, 200]), false, &[]),
            (3, "c", None, false, &[1, 2, 3]),
        ];
        for (id, name, blob, counted, rows) in files {
            let encoded = blob.as_ref().map(|sizes| crate::blob::encode(&segs(sizes)));
            let bytes = blob.as_ref().filter(|_| counted).map(|sizes| sizes.iter().sum::<i64>());
            conn.execute(
                "insert into s0.files (id, release_id, filename, seen, blob, blob_bytes) values (?, ?, ?, x'00', ?, ?)",
                params![id, release, name, encoded, bytes],
            )
            .unwrap();
            for (i, b) in rows.iter().enumerate() {
                conn.execute(
                    "insert into s0.segments (file_id, local, domain, part, bytes) values (?, ?, 0, ?, ?)",
                    params![id, store::pack_local(&format!("<r{id}.{i}@x>")), 50 + i as i64, b],
                )
                .unwrap();
            }
        }
        let mut walked: HashMap<String, i64> = HashMap::new();
        store::each_article(&conn, release, &mut |a| {
            *walked.entry(a.filename.unwrap_or_default()).or_default() += a.bytes.unwrap_or(0);
            Ok(())
        })
        .unwrap();
        let counted = of_release(&conn, release).unwrap();
        assert_eq!(counted, walked);
        assert_eq!(counted, HashMap::from([("a".into(), 35), ("b".into(), 300), ("c".into(), 6)]));
    }

    #[test]
    fn a_blob_the_sealed_log_cant_be_reached_for_is_tried_again_and_a_bad_one_passed_over() {
        let (_dir, conn) = shard();
        let mut place = sealed_log::REF_TAG.to_vec();
        place.resize(sealed_log::REF_BYTES, 7);
        let local = |sizes: &[i64]| crate::blob::encode(&segs(sizes));
        for (id, blob) in [(1, local(&[4])), (2, place.clone()), (3, local(&[6]))] {
            conn.execute(
                "insert into files (id, release_id, filename, seen, blob) values (?, 1, ?, x'00', ?)",
                params![id, format!("f{id}"), blob],
            )
            .unwrap();
        }
        let count = |id: i64| {
            conn.query_row("select blob_bytes from files where id = ?", [id], |r| r.get::<_, Option<i64>>(0)).unwrap()
        };
        let stop = AtomicBool::new(false);
        let unreachable = |_: &[u8]| Err(std::io::Error::from(std::io::ErrorKind::NotConnected));
        // out of reach: it stops before the blob in the log, to try it again
        let batch = count_batch_with(&conn, 0, &stop, 0, &unreachable).unwrap();
        assert_eq!(batch, Batch { next: Some(1), counted: 1, fetch_failed: true, fetched: 1, log_down: true });
        assert_eq!((count(1), count(2), count(3)), (Some(4), None, None));
        // what came back is cut short and won't decode: passed over, and the pass goes on
        let whole = local(&[1, 2, 3]);
        let cut_short = move |_: &[u8]| Ok(whole[..whole.len() / 2].to_vec());
        let batch = count_batch_with(&conn, 1, &stop, 0, &cut_short).unwrap();
        assert!(!batch.fetch_failed);
        assert_eq!((count(2), count(3)), (None, Some(6)));
        assert_eq!(count_batch(&conn, batch.next.unwrap(), &stop).unwrap().next, None);
    }

    #[test]
    fn a_pass_looks_through_a_window_of_ids_at_a_time() {
        let (_dir, conn) = shard();
        for id in [5, WINDOW + 10, 3 * WINDOW] {
            conn.execute(
                "insert into files (id, release_id, filename, seen, blob) values (?, 1, ?, x'00', ?)",
                params![id, format!("f{id}"), crate::blob::encode(&segs(&[1]))],
            )
            .unwrap();
        }
        let stop = AtomicBool::new(false);
        let mut after = 0;
        let mut steps = 0;
        while let Batch { next: Some(next), .. } = count_batch(&conn, after, &stop).unwrap() {
            assert!(next > after || steps > 0 && next >= after, "it moves on");
            after = next;
            steps += 1;
            assert!(steps < 10);
        }
        let uncounted: i64 =
            conn.query_row("select count(*) from files where blob_bytes is null", [], |r| r.get(0)).unwrap();
        assert_eq!(uncounted, 0);
    }

    #[test]
    fn a_pass_counts_the_blobs_from_before_and_only_those() {
        let (_dir, conn) = shard();
        for (id, sizes) in [(1, vec![1, 2]), (2, vec![10]), (3, vec![100, 200, 300])] {
            conn.execute(
                "insert into files (id, release_id, filename, seen, blob) values (?, 1, ?, x'00', ?)",
                params![id, format!("f{id}"), crate::blob::encode(&segs(&sizes))],
            )
            .unwrap();
        }
        // no blob, and a blob already counted (wrongly, to show it isn't recounted)
        conn.execute("insert into files (id, release_id, filename, seen) values (4, 1, 'f4', x'00')", []).unwrap();
        conn.execute("update files set blob_bytes = 999 where id = 2", []).unwrap();
        let stop = AtomicBool::new(false);
        // with the offload moving blobs over a byte, it leaves every one of these to it
        assert_eq!(count_batch_with(&conn, 0, &stop, 1, &fetch).unwrap().counted, 0);
        let batch = count_batch(&conn, 0, &stop).unwrap();
        assert_eq!(batch, Batch { next: Some(3), counted: 2, fetch_failed: false, fetched: 0, log_down: false });
        assert_eq!(count_batch(&conn, 3, &stop).unwrap().next, None);
        let counts: Vec<Option<i64>> = conn
            .prepare("select blob_bytes from files order by id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_>>()
            .unwrap();
        assert_eq!(counts, vec![Some(3), Some(999), Some(600), None]);
    }
}
