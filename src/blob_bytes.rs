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

/// Blobs counted in one batch, and so the most decoded (and fetched) between two looks at `stop`.
const BATCH: i64 = 256;
/// A rest after each batch, soo the pass takes little from the indexer's writes and the sealed log.
const REST: Duration = Duration::from_millis(200);
/// How long a write waits on the shard's writer, in tries of `busy_timeout` each.
const WRITE_TRIES: usize = 20;
/// How long after a pass of every shard the next one starts: a blob written without its count after the first is
/// rare (a writer this file doesn't know of).
const PASS_EVERY: Duration = Duration::from_secs(7 * 24 * 3600);

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

/// One batch of a shard's uncounted blobs past `after` counted. Returns the last file looked at (None: the shard has
/// no more) and how many were counted. A blob that won't decode or fetch stays uncounted, read as before.
pub fn count_batch(conn: &Connection, after: i64, stop: &AtomicBool) -> Result<(Option<i64>, usize)> {
    let batch: Vec<(i64, Vec<u8>)> = conn
        .prepare_cached(
            "select id, blob from files where id > ? and blob is not null and blob_bytes is null order by id limit ?",
        )?
        .query_map(params![after, BATCH], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_>>()?;
    let Some(last) = batch.last().map(|b| b.0) else { return Ok((None, 0)) };
    let mut counted: Vec<(i64, &[u8], i64)> = Vec::new();
    for (id, blob) in &batch {
        if stop.load(Ordering::Relaxed) {
            return Ok((Some(after), 0));
        }
        match of_blob(blob) {
            Ok(bytes) => counted.push((*id, blob, bytes)),
            Err(e) => eprintln!("blob bytes: file {id} left uncounted: {e}"),
        }
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
        Ok(n) => conn.execute_batch("commit").map(|()| (Some(last), n)),
        Err(e) => {
            let _ = conn.execute_batch("rollback");
            Err(e)
        }
    }
}

/// Every shard's uncounted blobs counted, a batch at a time, turn about, until `stop`; again every `PASS_EVERY`.
fn run(main: &Path, stop: &AtomicBool) {
    let shards = store::shard_paths(main);
    let nap = |d: Duration| {
        let until = Instant::now() + d;
        while Instant::now() < until && !stop.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(200).min(d));
        }
    };
    let mut conns: Vec<Option<Connection>> = shards.iter().map(|_| None).collect();
    let mut total = 0usize;
    while !stop.load(Ordering::Relaxed) {
        let now = chrono::Utc::now().timestamp();
        let mut busy = false;
        for (shard, conn) in shards.iter().zip(conns.iter_mut()) {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            let (after, done_at) = read_cursor(shard);
            if done_at > 0 && now - done_at < PASS_EVERY.as_secs() as i64 {
                continue;
            }
            busy = true;
            if conn.is_none() {
                match crate::db::open_at(shard) {
                    Ok(c) => *conn = Some(c),
                    Err(e) => {
                        eprintln!("blob bytes: {} not opened: {e}", shard.display());
                        continue;
                    }
                }
            }
            let Some(c) = conn.as_ref() else { continue };
            let after = if done_at > 0 { 0 } else { after };
            match count_batch(c, after, stop) {
                Ok((Some(last), n)) => {
                    total += n;
                    write_cursor(shard, last, 0);
                }
                Ok((None, _)) => {
                    println!("blob bytes: {} done, {total} blobs counted so far", shard.display());
                    write_cursor(shard, 0, now);
                }
                Err(e) => eprintln!("blob bytes: {}: {e}", shard.display()),
            }
            nap(REST);
        }
        if !busy {
            nap(Duration::from_secs(3600));
        }
    }
}

/// The pass that counts blobs from before `blob_bytes`, on a thread of its own in the indexer.
pub fn spawn(stop: Arc<AtomicBool>) {
    let main = crate::paths::database();
    let _ = std::thread::Builder::new().name("blob-bytes".into()).spawn(move || run(&main, &stop));
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
        let release = store::global_id(7, 3);
        conn.execute(
            "insert into s3.releases (id, name, group_name, size, complete, parts) values (?, 'r', 'g', 0, 0, 0)",
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
                "insert into s3.files (id, release_id, filename, seen, blob, blob_bytes) values (?, ?, ?, x'00', ?, ?)",
                params![id, release, name, encoded, bytes],
            )
            .unwrap();
            for (i, b) in rows.iter().enumerate() {
                conn.execute(
                    "insert into s3.segments (file_id, local, domain, part, bytes) values (?, ?, 0, ?, ?)",
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
        let (last, n) = count_batch(&conn, 0, &stop).unwrap();
        assert_eq!((last, n), (Some(3), 2));
        assert_eq!(count_batch(&conn, 3, &stop).unwrap(), (None, 0));
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
