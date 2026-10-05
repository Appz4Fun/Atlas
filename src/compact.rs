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
//! as `atlas.sN.precompact.db` until every shard is done, then they go.

use std::path::{Path, PathBuf};
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

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    path.with_file_name(format!("{stem}.{suffix}.db"))
}

fn remove_db(path: &Path) {
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}

fn size(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

/// What compacting one shard did.
#[derive(Debug, Default, Clone, Copy)]
pub struct Shrunk {
    pub before: u64,
    pub after: u64,
    pub domains_kept: i64,
    pub domains_dropped: i64,
}

/// Compact every shard of `main`. Returns the total before and after.
pub fn run(main: &Path, progress: &(dyn Fn(&str) + Sync)) -> Result<Shrunk> {
    if !store::exists(main) {
        bail!("{} has no shards (convert it first)", main.display());
    }
    let started = Instant::now();
    let mut total = Shrunk::default();
    for chunk in (0..SHARDS).collect::<Vec<_>>().chunks(PARALLEL) {
        let results: Vec<Result<Shrunk>> = std::thread::scope(|s| {
            let jobs: Vec<_> =
                chunk.iter().map(|&shard| s.spawn(move || compact_shard(main, shard, progress))).collect();
            jobs.into_iter().map(|j| j.join().unwrap_or_else(|_| Err(anyhow!("compacting a shard panicked")))).collect()
        });
        for r in results {
            let r = r?;
            total.before += r.before;
            total.after += r.after;
            total.domains_kept += r.domains_kept;
            total.domains_dropped += r.domains_dropped;
        }
    }
    // every shard is done and checked: the originals can go
    for shard in store::shard_paths(main) {
        remove_db(&with_suffix(&shard, "precompact"));
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

fn compact_shard(main: &Path, shard: usize, progress: &(dyn Fn(&str) + Sync)) -> Result<Shrunk> {
    let path = store::shard_path(main, shard);
    let copy = with_suffix(&path, "compact");
    let say = |msg: &str| progress(&format!("compacting shard {shard}: {msg}"));

    // fold the WAL in soo the original is whole on its own
    {
        let conn = db::open_at(&path)?;
        conn.query_row("pragma wal_checkpoint(truncate)", [], |_| Ok(()))?;
        // a shard from before sealing gets its (empty) seal columns
        store::migrate_shard(&conn)?;
    }
    let before = size(&path);

    // a fresh shard, filled without its indexes and triggers (built at the end)
    remove_db(&copy);
    store::create_shard(&copy)?;
    let mut conn = db::open_at(&copy)?;
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
    let max_domain: i64 = conn.query_row("select coalesce(max(id), 0) from old.domains", [], |r| r.get(0))?;
    let mut uses = vec![0u32; max_domain as usize + 1];
    {
        let mut used = |d: i64| {
            if let Some(n) = uses.get_mut(d as usize) {
                *n = n.saturating_add(1);
            }
        };
        let mut stmt = conn.prepare("select domain from old.segments")?;
        let mut rows = stmt.query([])?;
        while let Some(r) = rows.next()? {
            used(r.get(0)?);
        }
        let mut stmt = conn.prepare("select id, blob from old.files where blob is not null")?;
        let mut rows = stmt.query([])?;
        while let Some(r) = rows.next()? {
            for seg in unseal(shard, r.get(0)?, &r.get::<_, Vec<u8>>(1)?)? {
                used(seg.domain);
            }
        }
    }
    let shared = |d: i64| d != 0 && uses.get(d as usize).is_some_and(|n| *n >= SHARED_AFTER);

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
    say(&format!("copying files and articles ({kept} shared domains, dropping {dropped}), sealing those due"));
    let now = chrono::Utc::now().timestamp();
    let (mut copied, mut sealed) = (0i64, 0i64);
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
        let mut pending = next_row(&mut rows, &shared)?;

        let mut tx = conn.transaction()?;
        let mut in_tx = 0;
        loop {
            let file = files.next()?;
            let id: Option<i64> = file.map(|f| f.get(0)).transpose()?;
            // rows whose file isnt there (purged, or past the last file) stay rows
            while let Some(row) = pending.take_if(|r| id.is_none_or(|id| r.file_id < id)) {
                insert_row(&tx, &row)?;
                (copied, in_tx) = (copied + 1, in_tx + 1);
                pending = next_row(&mut rows, &shared)?;
            }
            let (Some(f), Some(id)) = (file, id) else { break };
            let mut file_rows = Vec::new();
            while let Some(row) = pending.take_if(|r| r.file_id == id) {
                file_rows.push(row);
                pending = next_row(&mut rows, &shared)?;
            }

            // its blob, with every message-id stored the way its rows are
            let old_blob: Option<Vec<u8>> = f.get(10)?;
            let mut segs = Vec::new();
            for seg in old_blob.as_deref().map(|b| unseal(shard, id, b)).transpose()?.unwrap_or_default() {
                let suffix: Option<String> = if seg.domain != 0 && !shared(seg.domain) {
                    suffix_of.query_row([seg.domain], |r| r.get(0)).optional()?
                } else {
                    None
                };
                let (local, domain) = rewrite(seg.local, seg.domain, shared(seg.domain), suffix.as_deref());
                segs.push(Seg { local, domain, ..seg });
            }

            // rows a blob cant hold exactly (a negative part, no size) stay rows
            let fits = file_rows.iter().all(|r| r.part.is_none_or(|p| p >= 0) && r.bytes.is_some());
            let seen: Vec<u8> = f.get(8)?;
            let seal =
                !file_rows.is_empty() && fits && store::due(f.get(6)?, &seen, f.get(9)?, old_blob.is_some(), now);
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
            if in_tx >= BATCH {
                tx.commit()?;
                tx = conn.transaction()?;
                in_tx = 0;
            }
        }
        tx.commit()?;
    }
    say(&format!("sealed {sealed} files"));

    say("building indexes and the search index");
    conn.execute_batch(
        "detach database old;
         insert into releases_fts(releases_fts) values('rebuild');",
    )?;
    drop(conn);
    store::create_shard(&copy)?;

    // the check: same rows, same NZBs
    say("checking");
    check(&path, &copy, shard, copied)?;

    // swap
    let conn = db::open_at(&copy)?;
    conn.query_row("pragma journal_mode = wal", [], |_| Ok(()))?;
    conn.query_row("pragma wal_checkpoint(truncate)", [], |_| Ok(()))?;
    drop(conn);
    let original = with_suffix(&path, "precompact");
    remove_db(&original);
    std::fs::rename(&path, &original).context("moving the original shard aside")?;
    for suffix in ["-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
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
/// dropped.
fn rewrite(local: Vec<u8>, domain: i64, shared: bool, suffix: Option<&str>) -> (Vec<u8>, i64) {
    if domain == 0 {
        (local, 0)
    } else if shared {
        (store::repack(&local), domain)
    } else {
        (store::whole(&store::decode(&local, suffix)), 0)
    }
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
fn next_row(rows: &mut rusqlite::Rows, shared: &dyn Fn(i64) -> bool) -> Result<Option<Row>> {
    let Some(r) = rows.next()? else { return Ok(None) };
    let (domain, suffix): (i64, Option<String>) = (r.get(2)?, r.get(5)?);
    let (local, domain) = rewrite(r.get(1)?, domain, shared(domain), suffix.as_deref());
    Ok(Some(Row { file_id: r.get(0)?, local, domain, part: r.get(3)?, bytes: r.get(4)? }))
}

fn insert_row(conn: &Connection, r: &Row) -> Result<()> {
    conn.prepare_cached("insert or ignore into segments (file_id, local, domain, part, bytes) values (?, ?, ?, ?, ?)")?
        .execute(params![r.file_id, r.local, r.domain, r.part, r.bytes])?;
    Ok(())
}

/// The copy holds the same releases, files and articles (rows and inside
/// blobs) as the original, and a sample of releases build the same NZBs from
/// both.
fn check(original: &Path, copy: &Path, shard: usize, copied: i64) -> Result<()> {
    let attach = |path: &Path| -> Result<Connection> {
        let conn = Connection::open_in_memory()?;
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
    if a != b {
        bail!("shard {shard}: {b} articles in the copy, {a} in the original; the original was kept");
    }
    if copied != b {
        bail!("shard {shard}: some articles came out as duplicates; the original was kept");
    }

    let (low, high): (Option<i64>, Option<i64>) =
        old.query_row(&format!("select min(id), max(id) from s{shard}.releases"), [], |r| Ok((r.get(0)?, r.get(1)?)))?;
    let (Some(low), Some(high)) = (low, high) else { return Ok(()) };
    let step = ((high - low) / CHECK_SAMPLE).max(1);
    for n in 0..CHECK_SAMPLE {
        let found: Option<i64> = old
            .query_row(
                &format!("select id from s{shard}.releases where id >= ? order by id limit 1"),
                [low + n * step],
                |r| r.get(0),
            )
            .ok();
        let Some(id) = found else { continue };
        let (a, b) = (store::articles(&old, id)?, store::articles(&new, id)?);
        if a != b {
            bail!("shard {shard}: release {id} reads back differently from the copy; the original was kept");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{Article, Release};

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

        let shrunk = run(&main, &|_| {}).unwrap();
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
        run(&main, &|_| {}).unwrap();
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

        let shrunk = run(&main, &|_| {}).unwrap();
        assert_eq!(shrunk.domains_dropped, 2, "@Late> and @Lonely>");
        assert_eq!(all_articles(&main), before, "every NZB reads back the same");
        assert_eq!(article_total(&main), count);
        assert_eq!(loose(&main), 0, "the late rows are sealed in with the blob");
        assert_eq!(blob_segs_with_known_domains(&main).len() as i64, count);
    }

    #[test]
    fn a_corrupt_blob_stops_compacting_and_keeps_the_original() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        legacy_fixture(&main);
        run(&main, &|_| {}).unwrap();
        let (path, file) = file_of(&main, "Rel.3", "alt.binaries.g3");
        db::open_at(&path).unwrap().execute("update files set blob = x'00' where id = ?", [file]).unwrap();

        let err = run(&main, &|_| {}).unwrap_err();
        assert!(format!("{err:#}").contains("corrupt"), "{err:#}");
        let blob: Vec<u8> =
            db::open_at(&path).unwrap().query_row("select blob from files where id = ?", [file], |r| r.get(0)).unwrap();
        assert_eq!(blob, vec![0], "the original is still in place");
    }
}
