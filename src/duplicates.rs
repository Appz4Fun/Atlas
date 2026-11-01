//! Other postings of the same content. A **twin** has the same posted files, proven by PAR2 hashes, soo
//! its articles can stand in for this one's. A **pair** holds the same files packed differently, soo
//! it's another whole download, not a source of articles.
//!
//! Each release keeps two fingerprints in its shard, and copies are found by looking them up in every
//! shard, soo there are no clusters to keep in step: purging a release takes its row with it.

use rusqlite::{Connection, OptionalExtension};
use sha1::{Digest, Sha1};

use crate::db;
use crate::par2::Par2File;
use crate::postprocess::schema::ShardTable;
use crate::search::{ReleaseRow, get_release_with};
use crate::store;

pub const FINGERPRINT: ShardTable = ShardTable {
    name: "release_fingerprint",
    create: "create table if not exists release_fingerprint (
        release_id INTEGER PRIMARY KEY,
        posted     BLOB,
        content    BLOB
    )",
    indexes: &[
        (
            "release_fingerprint_posted",
            "create index if not exists release_fingerprint_posted on release_fingerprint(posted) where posted is not null",
        ),
        (
            "release_fingerprint_content",
            "create index if not exists release_fingerprint_content on release_fingerprint(content) where content is not null",
        ),
    ],
    release_key: Some("release_id"),
    added: &[],
};

pub const SET_POSTED: &str = "insert into release_fingerprint (release_id, posted) values (?1, ?2)
    on conflict(release_id) do update set posted = excluded.posted";
pub const SET_CONTENT: &str = "insert into release_fingerprint (release_id, content) values (?1, ?2)
    on conflict(release_id) do update set content = excluded.content";

/// Files smaller than this don't count: NFOs, samples' subtitles, and the like differ between postings.
const MIN_CONTENT_FILE: u64 = 1 << 20;
const MAX_COPIES: i64 = 50;

/// What the posted files are, from a PAR2 set: their sizes and MD5s. Two equal ones are byte-identical.
pub fn posted_fingerprint(files: &[Par2File]) -> Option<Vec<u8>> {
    if files.is_empty() {
        return None;
    }
    let mut hashes: Vec<(u64, [u8; 16])> = files.iter().map(|f| (f.size, f.md5)).collect();
    hashes.sort_unstable();
    hashes.dedup();
    let mut sha = Sha1::new();
    for (size, md5) in hashes {
        sha.update(size.to_le_bytes());
        sha.update(md5);
    }
    Some(sha.finalize().to_vec())
}

fn is_archive(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let ext = lower.rsplit('.').next().unwrap_or_default();
    matches!(ext, "rar" | "zip" | "7z" | "par2" | "ace" | "arj" | "cab" | "lzh" | "tar" | "gz" | "bz2" | "xz")
        || (ext.len() == 3 && ext.starts_with('r') && ext[1..].bytes().all(|b| b.is_ascii_digit()))
        || (ext.len() == 3 && ext.bytes().all(|b| b.is_ascii_digit()))
}

fn is_sample(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.contains("sample") || lower.contains("proof")
}

/// What's in a release, from what's inside its archives, or its PAR2 set when it isn't packed: the exact
/// sizes of its real files. Names don't count, because obfuscation changes them. None: a file is packed,
/// soo these aren't the real files, or there's nothing big enough to go by.
pub fn content_fingerprint<'a>(files: impl Iterator<Item = (&'a str, Option<u64>)>) -> Option<Vec<u8>> {
    let mut sizes = Vec::new();
    for (name, size) in files {
        if is_archive(name) {
            return None;
        }
        if !is_sample(name)
            && let Some(size) = size.filter(|s| *s >= MIN_CONTENT_FILE)
        {
            sizes.push(size);
        }
    }
    if sizes.is_empty() {
        return None;
    }
    sizes.sort_unstable();
    let mut sha = Sha1::new();
    for size in sizes {
        sha.update(size.to_le_bytes());
    }
    Some(sha.finalize().to_vec())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Copy {
    Twin,
    Pair,
}

fn matching(conn: &Connection, column: &str, fingerprint: &[u8], id: i64) -> db::Result<Vec<i64>> {
    let mut found = Vec::new();
    for s in 0..store::SHARDS {
        let sql = format!(
            "select release_id from s{s}.release_fingerprint where {column} = ?1 and release_id != ?2 limit {MAX_COPIES}"
        );
        match conn.prepare_cached(&sql) {
            Ok(mut stmt) => {
                let ids = stmt.query_map(rusqlite::params![fingerprint, id], |r| r.get::<_, i64>(0))?;
                found.extend(ids.collect::<db::Result<Vec<_>>>()?);
            }
            // a shard that hasn't got the table yet has no copies
            Err(_) => continue,
        }
    }
    Ok(found)
}

/// The other postings of a release's content, twins first.
pub fn copies_with(conn: &Connection, id: i64) -> db::Result<Vec<(Copy, ReleaseRow)>> {
    let s = store::shard_of_id(id);
    let sql = format!("select posted, content from s{s}.release_fingerprint where release_id = ?");
    let Ok(mut stmt) = conn.prepare(&sql) else { return Ok(Vec::new()) };
    let row =
        stmt.query_row([id], |r| Ok((r.get::<_, Option<Vec<u8>>>(0)?, r.get::<_, Option<Vec<u8>>>(1)?))).optional()?;
    let Some((posted, content)) = row else { return Ok(Vec::new()) };
    let twins = match &posted {
        Some(fingerprint) => matching(conn, "posted", fingerprint, id)?,
        None => Vec::new(),
    };
    let pairs = match &content {
        Some(fingerprint) => matching(conn, "content", fingerprint, id)?,
        None => Vec::new(),
    };
    let mut out = Vec::new();
    for twin in &twins {
        if let Some(release) = get_release_with(conn, *twin)? {
            out.push((Copy::Twin, release));
        }
    }
    for pair in pairs.iter().filter(|pair| !twins.contains(pair)) {
        if let Some(release) = get_release_with(conn, *pair)? {
            out.push((Copy::Pair, release));
        }
    }
    Ok(out)
}

pub fn copies(id: i64) -> db::Result<Vec<(Copy, ReleaseRow)>> {
    copies_with(&db::open()?, id)
}

const BACKFILL_BATCH: i64 = 2_000;

/// One shard's releases that have inner files, from what's stored. Returns how many got a fingerprint.
fn backfill_shard(path: &std::path::Path, apply: bool) -> anyhow::Result<u64> {
    let conn = db::open_shard(path)?;
    let (mut after, mut set) = (0i64, 0u64);
    loop {
        let ids: Vec<i64> = conn
            .prepare_cached(
                "select distinct release_id from archive_inner_files where release_id > ? order by release_id limit ?",
            )?
            .query_map(rusqlite::params![after, BACKFILL_BATCH], |r| r.get(0))?
            .collect::<db::Result<_>>()?;
        let Some(last) = ids.last() else { return Ok(set) };
        after = *last;
        let tx = conn.unchecked_transaction()?;
        for id in ids {
            let rows: Vec<(String, Option<i64>)> = conn
                .prepare_cached("select name, size from archive_inner_files where release_id = ? order by seq")?
                .query_map([id], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<db::Result<_>>()?;
            let files = rows.iter().map(|(name, size)| (name.as_str(), size.and_then(|s| u64::try_from(s).ok())));
            if let Some(content) = content_fingerprint(files) {
                set += 1;
                if apply {
                    conn.prepare_cached(SET_CONTENT)?.execute(rusqlite::params![id, content])?;
                }
            }
        }
        tx.commit()?;
    }
}

/// `atlas --fingerprints [--apply]`: content fingerprints for releases inspected before they existed.
/// Posted fingerprints need PAR2 hashes, which were never stored, soo those come as releases are read again.
pub fn run(args: &[String]) -> i32 {
    let apply = args.iter().any(|a| a == "--apply");
    let main = crate::naming::walk::main_path();
    let _lock = if apply {
        match crate::naming::walk::take_lock(&main) {
            Ok(lock) => Some(lock),
            Err(e) => {
                println!("{e}");
                return 1;
            }
        }
    } else {
        if let Err(e) = crate::naming::walk::may_read(&main) {
            println!("{e}");
            return 1;
        }
        None
    };
    let mut total = 0u64;
    for path in store::shard_paths(&main) {
        match backfill_shard(&path, apply) {
            Ok(set) => total += set,
            Err(e) => {
                println!("{}: {e:#}", path.display());
                return 1;
            }
        }
    }
    println!("{total} releases {}", if apply { "fingerprinted" } else { "would be fingerprinted" });
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    const GB: u64 = 1 << 30;

    fn par2(size: u64, md5: u8) -> Par2File {
        Par2File { set_id: [0; 16], file_id: [0; 16], md5: [md5; 16], md5_16k: [0; 16], size, name: "x".into() }
    }

    #[test]
    fn posted_files_match_by_hash_in_any_order() {
        let a = posted_fingerprint(&[par2(GB, 1), par2(GB, 2)]);
        assert!(a.is_some());
        assert_eq!(a, posted_fingerprint(&[par2(GB, 2), par2(GB, 1)]));
        assert_ne!(a, posted_fingerprint(&[par2(GB, 1), par2(GB, 3)]), "same sizes, other bytes");
        assert_eq!(posted_fingerprint(&[]), None);
    }

    fn content(files: &[(&str, u64)]) -> Option<Vec<u8>> {
        content_fingerprint(files.iter().map(|(name, size)| (*name, Some(*size))))
    }

    #[test]
    fn content_matches_on_sizes_whatever_the_names() {
        let a = content(&[("Movie.2024.mkv", 5 * GB), ("Movie.nfo", 2_000)]);
        assert!(a.is_some());
        assert_eq!(a, content(&[("c2f94a1b.mkv", 5 * GB)]), "obfuscated, and no NFO");
        assert_eq!(a, content(&[("Movie.2024.mkv", 5 * GB), ("Sample/movie-sample.mkv", 50 << 20)]), "plus a sample");
        assert_ne!(a, content(&[("Movie.2024.mkv", 5 * GB + 1)]), "another encode");
    }

    #[test]
    fn a_backfill_fingerprints_what_is_stored_and_only_when_applied() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shard.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "create table archive_inner_files (release_id INTEGER, seq INTEGER, name TEXT, size INTEGER, source INTEGER);
             insert into archive_inner_files values (1, 1, 'a.mkv', 5000000, 2), (2, 1, 'a.part1.rar', 5000000, 1), (3, 1, 'a.nfo', 10, 2);",
        )
        .unwrap();
        conn.execute_batch(FINGERPRINT.create).unwrap();
        assert_eq!(backfill_shard(&path, false).unwrap(), 1);
        let rows = |c: &Connection| -> i64 {
            c.query_row("select count(*) from release_fingerprint", [], |r| r.get(0)).unwrap()
        };
        assert_eq!(rows(&conn), 0, "a dry run writes nothing");
        assert_eq!(backfill_shard(&path, true).unwrap(), 1);
        assert_eq!(rows(&conn), 1);
    }

    #[test]
    fn packed_files_and_small_files_say_nothing_about_content() {
        assert_eq!(content(&[("a.part01.rar", GB), ("a.mkv", GB)]), None);
        assert_eq!(content(&[("a.r00", GB)]), None);
        assert_eq!(content(&[("a.001", GB)]), None);
        assert_eq!(content(&[("a.nfo", 2_000)]), None);
        assert_eq!(content(&[]), None);
    }
}
