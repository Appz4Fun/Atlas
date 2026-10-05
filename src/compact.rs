//! `atlas --compact`, with the indexer stopped: rewrite every shard into a
//! fresh file.
//!
//! - message-ids whose domain fewer than `SHARED_AFTER` articles share are
//!   stored whole, and those domains dropped: some posting tools make up a
//!   new domain per article, which left tens of millions of single use rows
//! - the copy has no free space in it (the conversion left the space its
//!   staging table used)
//!
//! Each shard's copy is checked (row counts, and a sample of NZBs against the
//! original) before it replaces the original. The originals stay next to it
//! as `atlas.sN.precompact.db` until every shard is done, then they go.

use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, anyhow, bail};
use rusqlite::{Connection, params};

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

    // how many articles use each domain
    say("counting domains");
    let max_domain: i64 = conn.query_row("select coalesce(max(id), 0) from old.domains", [], |r| r.get(0))?;
    let mut uses = vec![0u32; max_domain as usize + 1];
    {
        let mut stmt = conn.prepare("select domain from old.segments")?;
        let mut rows = stmt.query([])?;
        while let Some(r) = rows.next()? {
            let d: i64 = r.get(0)?;
            if let Some(n) = uses.get_mut(d as usize) {
                *n = n.saturating_add(1);
            }
        }
    }
    let shared = |d: i64| d != 0 && uses.get(d as usize).is_some_and(|n| *n >= SHARED_AFTER);

    say("copying releases and files");
    conn.execute_batch(
        "insert into releases select * from old.releases order by id;
         insert into files select * from old.files order by id;
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

    // the articles, in key order: those of dropped domains stored whole
    say(&format!("copying articles ({kept} shared domains, dropping {dropped})"));
    let mut copied = 0i64;
    {
        let read = Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let mut stmt = read.prepare(
            "select s.file_id, s.local, s.domain, s.part, s.bytes, d.suffix
             from segments s left join domains d on d.id = s.domain",
        )?;
        let mut rows = stmt.query([])?;
        let mut tx = conn.transaction()?;
        let mut in_tx = 0;
        loop {
            let Some(r) = rows.next()? else { break };
            let (file_id, local, domain): (i64, Vec<u8>, i64) = (r.get(0)?, r.get(1)?, r.get(2)?);
            let (part, bytes): (Option<i64>, Option<i64>) = (r.get(3)?, r.get(4)?);
            let (local, domain) = if domain == 0 || shared(domain) {
                (local, domain)
            } else {
                let suffix: Option<String> = r.get(5)?;
                (store::whole(&store::decode(&local, suffix.as_deref())), 0)
            };
            tx.prepare_cached(
                "insert or ignore into segments (file_id, local, domain, part, bytes) values (?, ?, ?, ?, ?)",
            )?
            .execute(params![file_id, local, domain, part, bytes])?;
            copied += 1;
            in_tx += 1;
            if in_tx >= BATCH {
                tx.commit()?;
                tx = conn.transaction()?;
                in_tx = 0;
            }
        }
        tx.commit()?;
    }

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

/// The copy holds the same releases, files and articles as the original, and
/// a sample of releases build the same NZBs from both.
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
    for table in ["releases", "files", "segments"] {
        let (a, b) = (count(&old, table)?, count(&new, table)?);
        if a != b {
            bail!("shard {shard}: {table} has {b} rows in the copy, {a} in the original; the original was kept");
        }
    }
    if copied != count(&new, "segments")? {
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

    #[test]
    fn compacting_drops_single_use_domains_and_keeps_every_nzb() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        db::create_db_at(&main).unwrap();

        // releases over several groups: a shared domain, and made up ones per article
        let releases: Vec<Release> = (0..40)
            .map(|r| Release {
                name: format!("Rel.{r}"),
                group: format!("alt.binaries.g{}", r % 5),
                articles: (1..=6)
                    .map(|p| Article {
                        message_id: if p % 2 == 0 {
                            format!("<{:032x}@ngPost>", r * 10 + p)
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
        store::save(&main, &releases).unwrap();

        // what the conversion left: every domain in the table, made up ones too
        for path in store::shard_paths(&main) {
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
                conn.execute(
                    "update segments set local = ?, domain = ? where file_id = ? and local = ? and domain = 0",
                    params![store::pack_local(part), d, file_id, local],
                )
                .unwrap();
            }
        }
        let domains = |main: &Path| -> i64 {
            let conn = db::open_with_shards(main).unwrap();
            let sql = format!(
                "select sum(c) from ({})",
                store::each_shard(|i| format!("select count(*) as c from s{i}.domains"))
            );
            conn.query_row(&sql, [], |r| r.get(0)).unwrap()
        };
        assert_eq!(domains(&main), 120 + 5, "every made up domain has a row, ngPost one per shard used");
        let before = all_articles(&main);

        let shrunk = run(&main, &|_| {}).unwrap();
        assert_eq!(shrunk.domains_dropped, 120);
        assert_eq!(domains(&main), shrunk.domains_kept);
        assert!(shrunk.domains_kept <= 5);
        assert_eq!(all_articles(&main), before, "every NZB reads back the same");
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
}
