//! Sealed articles kept in the `sealed-log` service, usually on a big disk on
//! another machine. `files.blob` then holds the blob's place there instead of
//! the blob, which `crate::blob` follows, so nothing else has to know.
//!
//! Sealing stays local, and a background pass moves what's sealed: it appends a
//! batch, reads it back, compares, and only then swaps each blob for its place,
//! if the blob is still the one it read.

use std::cell::RefCell;
use std::io::{Error, ErrorKind, Result};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use rusqlite::params;
use sealed_log::{Client, Locator};

use crate::config::SealedLogConfig;
use crate::metrics::{self, names};
use crate::store;

static ADDR: RwLock<String> = RwLock::new(String::new());

/// Blobs under this size stay where they are. On a real shard the 12% of blobs
/// past it hold 80% of the bytes, and each move costs the writers a lock.
const SMALL_BLOB: usize = 1024;
/// How many times a swap waits out the writers (`db::open_at`'s 30 seconds each)
/// before its batch is tried again from the start.
const SWAP_TRIES: usize = 20;
const BATCH_FILES: usize = 10_000;
const BATCH_BYTES: usize = 128 << 20;
/// How long a finished sweep waits before looking for newly sealed files.
const SWEEP_EVERY: Duration = Duration::from_secs(10);
/// A file stays put this long after its last article. New files are what's still
/// merged, filtered and deleted, and those want their blob at hand.
const SETTLE_SECS: i64 = 60;
const RETRY: Duration = Duration::from_secs(5);

thread_local! {
    static CLIENT: RefCell<Option<(String, Client)>> = const { RefCell::new(None) };
}

/// Where the service is, for this process. Empty for nowhere.
pub fn configure(addr: &str) {
    *ADDR.write().unwrap() = addr.to_string();
}

/// A connection-using call, with one retry on a new connection, timed as
/// `function` (connecting included, which is what a caller waits for).
fn with_client<T>(function: &'static str, call: impl Fn(&mut Client) -> Result<T>) -> Result<T> {
    let span = metrics::span(function);
    let result = with_client_unmeasured(call);
    if result.is_err() {
        span.failed();
    }
    result
}

fn with_client_unmeasured<T>(call: impl Fn(&mut Client) -> Result<T>) -> Result<T> {
    let addr = ADDR.read().unwrap().clone();
    if addr.is_empty() {
        return Err(Error::new(ErrorKind::NotConnected, "article is in the sealed log, and sealed_log.addr isn't set"));
    }
    CLIENT.with_borrow_mut(|slot| {
        for attempt in 0..2 {
            if slot.as_ref().is_none_or(|(at, _)| *at != addr) {
                let connected = Client::connect(&addr);
                let result = if connected.is_ok() { "ok" } else { "failed" };
                names::SEALED_LOG_CONNECTS.inc(&[("result", result)], 1);
                *slot = Some((addr.clone(), connected?));
            }
            match call(&mut slot.as_mut().expect("just set").1) {
                // the service answered: it means this
                Err(e) if e.kind() == ErrorKind::Other => return Err(e),
                Err(e) if attempt == 0 => {
                    let _ = e;
                    *slot = None;
                }
                other => return other,
            }
        }
        unreachable!("the second attempt returns")
    })
}

/// A blob from the sealed log.
pub fn fetch(at: Locator) -> Result<Vec<u8>> {
    let mut values = with_client("sealed_log.fetch", |c| c.read(&[at]))?;
    metrics::function_bytes("sealed_log.fetch", "received", at.len as u64);
    values.pop().ok_or_else(|| Error::new(ErrorKind::InvalidData, "no value"))
}

/// Whether a shard has blobs in the log. A compaction would have to pull them
/// all back to rewrite them, so it stops there instead.
pub fn has_places(conn: &rusqlite::Connection) -> rusqlite::Result<bool> {
    conn.query_row(
        "select exists(select 1 from files where length(blob) = ? and substr(blob, 1, 5) = ?)",
        params![sealed_log::REF_BYTES as i64, &sealed_log::REF_TAG[..]],
        |r| r.get(0),
    )
}

/// One batch of `shard`'s sealed blobs past `after` moved. Returns the last file
/// looked at, none when the shard has no more, and the bytes moved.
pub fn offload_batch(shard: &Path, after: i64) -> anyhow::Result<(Option<i64>, u64)> {
    let conn = crate::db::open_at(shard)?;
    let mut stmt = conn.prepare_cached(
        "select id, blob from files
         where id > ? and blob is not null and length(blob) > ? and coalesce(touched_at, 0) < ?
         order by id limit ?",
    )?;
    let settled = chrono::Utc::now().timestamp() - SETTLE_SECS;
    let mut rows = stmt.query(params![after, SMALL_BLOB as i64, settled, BATCH_FILES as i64])?;
    let mut batch: Vec<(i64, Vec<u8>)> = Vec::new();
    let mut bytes = 0usize;
    while let Some(r) = rows.next()? {
        let blob: Vec<u8> = r.get(1)?;
        bytes += blob.len();
        batch.push((r.get(0)?, blob));
        if bytes >= BATCH_BYTES {
            break;
        }
    }
    drop(rows);
    drop(stmt);
    let Some(last) = batch.last().map(|b| b.0) else { return Ok((None, 0)) };

    let values: Vec<(u64, &[u8])> = batch.iter().map(|(id, b)| (*id as u64, &b[..])).collect();
    let places = with_client("sealed_log.append", |c| c.append(&values))?;
    metrics::function_bytes("sealed_log.append", "sent", bytes as u64);
    // in the log now, whatever happens next: a retry sends it again
    names::SEALED_LOG_OFFLOAD_BYTES.inc(&[("kind", "sent")], bytes as u64);
    // only what comes back the same replaces anything
    let back = with_client("sealed_log.verify", |c| c.read(&places))?;
    metrics::function_bytes("sealed_log.verify", "received", bytes as u64);
    for ((id, blob), value) in batch.iter().zip(&back) {
        if blob != value {
            names::SEALED_LOG_OFFLOAD_FAILURES.inc(&[("kind", "mismatch")], 1);
        }
        anyhow::ensure!(blob == value, "file {id} came back from the sealed log different");
    }

    let (swapped, changed, freed) = metrics::timed("sealed_log.swap", || swap_in(&conn, &batch, &places))?;
    names::SEALED_LOG_OFFLOAD_FILES.inc(&[("result", "moved")], swapped);
    names::SEALED_LOG_OFFLOAD_FILES.inc(&[("result", "changed")], changed);
    names::SEALED_LOG_OFFLOAD_BYTES.inc(&[("kind", "freed")], freed);
    Ok((Some(last), bytes as u64))
}

/// Each blob replaced by its place. Taking the write lock up front, and waiting
/// for it here, keeps a busy writer from wasting the appends already made.
/// Returns the files swapped, the ones that changed meanwhile, and the bytes freed.
fn swap_in(
    conn: &rusqlite::Connection,
    batch: &[(i64, Vec<u8>)],
    places: &[Locator],
) -> rusqlite::Result<(u64, u64, u64)> {
    let mut tries = 0;
    loop {
        match conn.execute_batch("begin immediate") {
            Ok(()) => break,
            Err(rusqlite::Error::SqliteFailure(e, _))
                if e.code == rusqlite::ErrorCode::DatabaseBusy && tries + 1 < SWAP_TRIES =>
            {
                tries += 1;
            }
            Err(e) => return Err(e),
        }
    }
    let swapped = (|| {
        let (mut swapped, mut changed, mut freed) = (0u64, 0u64, 0u64);
        // a blob that changed meanwhile (more articles sealed into it) keeps its
        // new self, and its copy in the log goes unused
        let mut swap = conn.prepare_cached("update files set blob = ? where id = ? and blob = ?")?;
        for ((id, blob), at) in batch.iter().zip(places) {
            if swap.execute(params![at.to_ref(), id, blob])? > 0 {
                swapped += 1;
                freed += (blob.len() - sealed_log::REF_BYTES) as u64;
            } else {
                changed += 1;
            }
        }
        Ok((swapped, changed, freed))
    })();
    match swapped {
        Ok(counts) => conn.execute_batch("commit").map(|()| counts),
        Err(e) => {
            let _ = conn.execute_batch("rollback");
            Err(e)
        }
    }
}

fn cursor_path(shard: &Path) -> PathBuf {
    let mut name = shard.as_os_str().to_owned();
    name.push(".sealed-log-cursor");
    PathBuf::from(name)
}

fn read_cursor(shard: &Path) -> i64 {
    std::fs::read_to_string(cursor_path(shard)).ok().and_then(|t| t.trim().parse().ok()).unwrap_or(0)
}

/// `atlas --sealed-log-move`: one sweep at full speed with the indexer stopped,
/// so nothing competes for the write locks. Stopping it anytime is safe.
pub fn run_move() -> i32 {
    let config = crate::config::sealed_log_config();
    if config.addr.is_empty() {
        println!("sealed_log.addr isn't set in config.toml");
        return 1;
    }
    if crate::procs::indexer_alive() {
        println!("stop the indexer first: it competes for the same locks, and it moves these anyway");
        return 1;
    }
    configure(&config.addr);
    let main = crate::paths::database();
    let started = Instant::now();
    let moved = sweep(&main, u64::MAX, &AtomicBool::new(false), true);
    println!("moved {moved} bytes in {}s", started.elapsed().as_secs());
    0
}

/// Moves sealed blobs from every shard, turn about, until `stop`.
pub fn run_offload(main: &Path, mb_per_second: u64, stop: &AtomicBool) {
    sweep(main, mb_per_second, stop, false);
}

/// The bytes moved. With `once` it returns when every shard has been through.
fn sweep(main: &Path, mb_per_second: u64, stop: &AtomicBool, once: bool) -> u64 {
    let shards = store::shard_paths(main);
    let mut cursors: Vec<Option<i64>> = shards.iter().map(|s| Some(read_cursor(s))).collect();
    let budget = mb_per_second.max(1).saturating_mul(1_000_000) as f64;
    let nap = |d: Duration| {
        let until = Instant::now() + d;
        while Instant::now() < until && !stop.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(200));
        }
    };
    let mut moved = 0u64;
    let mut reported = Instant::now();
    let mut sweep_started = Instant::now();
    // there from the start: a series that starts at its first failure doesnt show it as an increase
    names::SEALED_LOG_OFFLOAD_FAILURES.inc(&[("kind", "batch")], 0);
    names::SEALED_LOG_OFFLOAD_FAILURES.inc(&[("kind", "mismatch")], 0);
    while !stop.load(Ordering::Relaxed) {
        let left = cursors.iter().filter(|c| c.is_some()).count();
        names::SEALED_LOG_SWEEP.set(&[("kind", "shards_left")], left as f64);
        names::SEALED_LOG_SWEEP.set(&[("kind", "shards")], shards.len() as f64);
        if left == 0 {
            if once {
                return moved;
            }
            println!("sealed log: sweep done, {moved} bytes moved");
            names::SEALED_LOG_SWEEP
                .set(&[("kind", "last_done_timestamp_seconds")], chrono::Utc::now().timestamp() as f64);
            names::SEALED_LOG_SWEEP.set(&[("kind", "last_bytes")], moved as f64);
            names::SEALED_LOG_SWEEP.set(&[("kind", "last_seconds")], sweep_started.elapsed().as_secs_f64());
            set_state("resting");
            moved = 0;
            nap(SWEEP_EVERY);
            sweep_started = Instant::now();
            cursors = vec![Some(0); shards.len()];
            continue;
        }
        for (shard, cursor) in shards.iter().zip(cursors.iter_mut()) {
            let Some(after) = *cursor else { continue };
            if stop.load(Ordering::Relaxed) {
                return moved;
            }
            let started = Instant::now();
            set_state("moving");
            match metrics::timed("sealed_log.offload_batch", || offload_batch(shard, after)) {
                Ok((last, bytes)) => {
                    moved += bytes;
                    *cursor = last;
                    if once && reported.elapsed() > Duration::from_secs(30) {
                        reported = Instant::now();
                        println!("{moved} bytes moved");
                    }
                    let _ = std::fs::write(cursor_path(shard), last.unwrap_or(0).to_string());
                    let rest = Duration::from_secs_f64(bytes as f64 / budget).saturating_sub(started.elapsed());
                    if !rest.is_zero() {
                        set_state("throttled");
                        names::SEALED_LOG_THROTTLED.inc(&[], rest.as_millis() as u64);
                    }
                    nap(rest);
                }
                Err(e) => {
                    // nothing was swapped, and the rows and blobs stay as they are
                    eprintln!("sealed log: {} not moved yet: {e:#}", shard.display());
                    names::SEALED_LOG_OFFLOAD_FAILURES.inc(&[("kind", "batch")], 1);
                    set_state("retrying");
                    nap(RETRY);
                }
            }
        }
    }
    moved
}

/// What the move is doing now, as a 1 among its states.
fn set_state(now: &str) {
    names::SEALED_LOG_OFFLOAD_STATE.replace(
        ["moving", "throttled", "retrying", "resting"]
            .into_iter()
            .map(|s| (format!("state=\"{s}\""), if s == now { 1.0 } else { 0.0 })),
    );
}

/// Starts the move, when the config asks for it. The indexer doesn't wait on it.
pub fn spawn_offload(config: &SealedLogConfig, stop: Arc<AtomicBool>) {
    if !config.offload || config.addr.is_empty() {
        return;
    }
    configure(&config.addr);
    let (main, rate) = (crate::paths::database(), config.offload_mb_per_second);
    let _ =
        std::thread::Builder::new().name("sealed-log-offload".into()).spawn(move || run_offload(&main, rate, &stop));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blob::{self, Seg};
    use std::net::TcpListener;

    fn segs(n: usize) -> Vec<Seg> {
        (0..n)
            .map(|i| Seg {
                part: Some(i as i64),
                bytes: 700_000,
                domain: 1,
                local: format!("{i:04}-{}", "x7q".repeat(i % 9 + 12)).into_bytes(),
            })
            .collect()
    }

    /// A blob past `SMALL_BLOB`, whatever zstd makes of it.
    fn incompressible() -> Vec<u8> {
        let mut x = 88172645463325252u64;
        let segs: Vec<Seg> = (0..400)
            .map(|i| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                Seg { part: Some(i), bytes: 700_000, domain: 1, local: format!("{x:016x}{i}").into_bytes() }
            })
            .collect();
        blob::encode(&segs)
    }

    #[test]
    fn sealed_articles_move_and_read_the_same_and_a_lost_service_changes_nothing() {
        let dir = std::env::temp_dir().join(format!("atlas-sealed-store-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let shard = dir.join("shard.db");
        let conn = crate::db::create_at(&shard).unwrap();
        conn.execute_batch("create table files (id integer primary key, touched_at integer, blob blob)").unwrap();
        let (big, small, recent) = (incompressible(), vec![1u8; 10], incompressible());
        let insert = "insert into files (id, touched_at, blob) values (?, ?, ?)";
        conn.execute(insert, params![1, 100, big]).unwrap();
        conn.execute(insert, params![2, 100, small]).unwrap();
        conn.execute(insert, params![3, chrono::Utc::now().timestamp(), recent]).unwrap();
        conn.execute("insert into files (id, touched_at) values (4, 100)", []).unwrap();
        let read =
            |id: i64| conn.query_row("select blob from files where id = ?", [id], |r| r.get::<_, Vec<u8>>(0)).unwrap();

        // nowhere to send it: an error, and the blob stays
        configure("");
        assert!(offload_batch(&shard, 0).is_err());
        assert_eq!(read(1), big);

        let log = sealed_log::Log::open(&dir.join("log")).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        configure(&listener.local_addr().unwrap().to_string());
        std::thread::spawn(move || sealed_log::serve(log, listener));

        let (last, bytes) = offload_batch(&shard, 0).unwrap();
        assert_eq!((last, bytes), (Some(1), big.len() as u64));
        assert!(blob::is_remote(&read(1)), "swapped for its place");
        assert!(read(1).len() < big.len());
        assert_eq!(read(2), small, "too small to be worth a place");
        assert_eq!(read(3), recent, "still settling");

        // everything that reads a blob gets what it would have
        assert_eq!(blob::decode(&read(1)).unwrap(), blob::decode(&big).unwrap());
        assert_eq!(blob::decode_capped(&read(1), 400).unwrap().len(), 400);
        assert!(blob::decode_capped(&read(1), 399).is_err(), "the cap still holds");
        assert_eq!(blob::count(&read(1)).unwrap(), 400);

        // a writer holding the lock is waited out, and the swap still lands
        let writer = crate::db::create_at(&shard).unwrap();
        conn.execute("update files set touched_at = 100 where id = 3", []).unwrap();
        writer.execute_batch("begin immediate").unwrap();
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            writer.execute_batch("commit").unwrap();
        });
        assert!(offload_batch(&shard, 2).unwrap().0.is_some());
        release.join().unwrap();
        assert!(blob::is_remote(&read(3)));
        assert_eq!(blob::decode(&read(3)).unwrap(), blob::decode(&recent).unwrap());

        // the next sweep has nothing to do
        assert_eq!(offload_batch(&shard, 0).unwrap(), (None, 0));

        // and what it did is on /metrics
        let out = crate::metrics::render();
        for line in [
            "atlas_function_duration_seconds_count{function=\"sealed_log.append\"}",
            "atlas_function_duration_seconds_count{function=\"sealed_log.verify\"}",
            "atlas_function_duration_seconds_count{function=\"sealed_log.fetch\"}",
            "atlas_sealed_log_offload_files_total{result=\"moved\"}",
            "atlas_sealed_log_offload_bytes_total{kind=\"freed\"}",
            "atlas_sealed_log_connects_total{result=\"ok\"}",
        ] {
            assert!(out.contains(line), "{line} in {out}");
        }

        // the service gone: an error to report, not a wrong answer
        let place = read(1);
        configure("127.0.0.1:9");
        assert!(blob::decode(&place).is_err());
        configure("");
        assert!(blob::count(&place).is_err());
    }

    #[test]
    fn a_compaction_leaves_a_shard_with_articles_in_the_log_alone() {
        let dir = std::env::temp_dir().join(format!("atlas-sealed-compact-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let conn = crate::db::create_at(&dir.join("s.db")).unwrap();
        conn.execute_batch("create table files (id integer primary key, blob blob)").unwrap();
        conn.execute("insert into files values (1, ?)", [blob::encode(&segs(3))]).unwrap();
        assert!(!has_places(&conn).unwrap());
        let place = Locator { segment: 0, offset: 0, len: 1 }.to_ref();
        conn.execute("insert into files values (2, ?)", [place]).unwrap();
        assert!(has_places(&conn).unwrap());
    }
}
