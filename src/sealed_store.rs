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
use crate::store;

static ADDR: RwLock<String> = RwLock::new(String::new());

/// Blobs up to this size stay where they are: a place takes about as much.
const SMALL_BLOB: usize = 64;
const BATCH_FILES: usize = 2000;
const BATCH_BYTES: usize = 32 << 20;
/// How long a finished sweep waits before looking for newly sealed files.
const SWEEP_EVERY: Duration = Duration::from_secs(600);
/// A file stays put this long after its last article. New files are what's still
/// merged, filtered and deleted, and those want their blob at hand.
const SETTLE_SECS: i64 = 86_400;
const RETRY: Duration = Duration::from_secs(30);

thread_local! {
    static CLIENT: RefCell<Option<(String, Client)>> = const { RefCell::new(None) };
}

/// Where the service is, for this process. Empty for nowhere.
pub fn configure(addr: &str) {
    *ADDR.write().unwrap() = addr.to_string();
}

/// A connection-using call, with one retry on a new connection.
fn with_client<T>(call: impl Fn(&mut Client) -> Result<T>) -> Result<T> {
    let addr = ADDR.read().unwrap().clone();
    if addr.is_empty() {
        return Err(Error::new(ErrorKind::NotConnected, "article is in the sealed log, and sealed_log.addr isn't set"));
    }
    CLIENT.with_borrow_mut(|slot| {
        for attempt in 0..2 {
            if slot.as_ref().is_none_or(|(at, _)| *at != addr) {
                *slot = Some((addr.clone(), Client::connect(&addr)?));
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
    let mut values = with_client(|c| c.read(&[at]))?;
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
    let places = with_client(|c| c.append(&values))?;
    // only what comes back the same replaces anything
    let back = with_client(|c| c.read(&places))?;
    for ((id, blob), value) in batch.iter().zip(&back) {
        anyhow::ensure!(blob == value, "file {id} came back from the sealed log different");
    }

    let tx = conn.unchecked_transaction()?;
    {
        // a blob that changed meanwhile (more articles sealed into it) keeps its
        // new self, and its copy in the log goes unused
        let mut swap = tx.prepare_cached("update files set blob = ? where id = ? and blob = ?")?;
        for ((id, blob), at) in batch.iter().zip(&places) {
            swap.execute(params![at.to_ref(), id, blob])?;
        }
    }
    tx.commit()?;
    Ok((Some(last), bytes as u64))
}

fn cursor_path(shard: &Path) -> PathBuf {
    let mut name = shard.as_os_str().to_owned();
    name.push(".sealed-log-cursor");
    PathBuf::from(name)
}

fn read_cursor(shard: &Path) -> i64 {
    std::fs::read_to_string(cursor_path(shard)).ok().and_then(|t| t.trim().parse().ok()).unwrap_or(0)
}

/// Moves sealed blobs from every shard, turn about, until `stop`.
pub fn run_offload(main: &Path, mb_per_second: u64, stop: &AtomicBool) {
    let shards = store::shard_paths(main);
    let mut cursors: Vec<Option<i64>> = shards.iter().map(|s| Some(read_cursor(s))).collect();
    let budget = (mb_per_second.max(1) * 1_000_000) as f64;
    let nap = |d: Duration| {
        let until = Instant::now() + d;
        while Instant::now() < until && !stop.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(200));
        }
    };
    let mut moved = 0u64;
    while !stop.load(Ordering::Relaxed) {
        if cursors.iter().all(Option::is_none) {
            println!("sealed log: sweep done, {moved} bytes moved");
            moved = 0;
            nap(SWEEP_EVERY);
            cursors = vec![Some(0); shards.len()];
            continue;
        }
        for (shard, cursor) in shards.iter().zip(cursors.iter_mut()) {
            let Some(after) = *cursor else { continue };
            if stop.load(Ordering::Relaxed) {
                return;
            }
            let started = Instant::now();
            match offload_batch(shard, after) {
                Ok((last, bytes)) => {
                    moved += bytes;
                    *cursor = last;
                    let _ = std::fs::write(cursor_path(shard), last.unwrap_or(0).to_string());
                    nap(Duration::from_secs_f64(bytes as f64 / budget).saturating_sub(started.elapsed()));
                }
                Err(e) => {
                    // nothing was swapped, and the rows and blobs stay as they are
                    eprintln!("sealed log: {} not moved yet: {e:#}", shard.display());
                    nap(RETRY);
                }
            }
        }
    }
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

    #[test]
    fn sealed_articles_move_and_read_the_same_and_a_lost_service_changes_nothing() {
        let dir = std::env::temp_dir().join(format!("atlas-sealed-store-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let shard = dir.join("shard.db");
        let conn = crate::db::create_at(&shard).unwrap();
        conn.execute_batch("create table files (id integer primary key, touched_at integer, blob blob)").unwrap();
        let (big, small, recent) = (blob::encode(&segs(400)), vec![1u8; 10], blob::encode(&segs(300)));
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

        // the next sweep has nothing to do
        assert_eq!(offload_batch(&shard, 0).unwrap(), (None, 0));

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
