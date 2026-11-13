//! `atlas --save-replay --store DIR --db DIR [--rows N] [--slice N] [--tasks N] [--parsers N]`: replays real overview
//! lines from a header store through the indexer's parse and save path into a scratch database, for measuring and
//! profiling the release writer. It never touches the live databases: `--db` is a folder of its own (made if need be),
//! used as the app folder for the run. The native PostgreSQL release writer requires
//! `--pg-database NAME --pg-schema atlas_replay_NAME` and set `ATLAS_REPLAY_PG_URL`.
//! With `--pg-header-source ID`, the replay also archives the complete sealed NXV blocks that supply its rows.
//! Release and header batches commit separately. A failed run may leave partial results in its staging schema.
//!
//! Lines come group by group, round robin, in slices of `--slice` rows (the indexer's slices), parsed on `--parsers`
//! threads the way `indexer::parse_and_save` does (articles obfuscated per article go to the header store, which the
//! store's linker reads, soo they aren't saved here), and saved through the indexer's own writer (`shared_db`) by
//! `--tasks` slices at once, as many as the indexer keeps waiting on it. Every ten seconds it prints rows saved per
//! second, how busy the writer was, and the shard's size. The input row rate includes slices that produced no
//! releases. The final report also counts what actually persisted in the release and file tables.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, ensure};
use nntp_xover_engine::container::NxvBlockView;
use nntp_xover_engine::streamer::{NxvRangeCursor, XoverStreamScratch};

use crate::nxv_store::segment::Entry;
use crate::nxv_store::store::{KeepSet, Store, StoreConfig};
use crate::parser::Release;
use crate::pg::{Pool, StorageConfig, header_blocks};

fn arg<T: std::str::FromStr>(args: &[String], flag: &str, default: T) -> T {
    args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).and_then(|v| v.parse().ok()).unwrap_or(default)
}

fn flag_value<'a>(args: &'a [String], flag: &str) -> Result<Option<&'a str>> {
    args.iter()
        .position(|arg| arg == flag)
        .map(|i| {
            args.get(i + 1)
                .map(String::as_str)
                .filter(|v| !v.starts_with("--") && !v.is_empty())
                .with_context(|| format!("{flag} requires a value"))
        })
        .transpose()
}

fn pg_target(args: &[String], replay_url: Option<String>) -> Result<(StorageConfig, &str)> {
    match (flag_value(args, "--pg-database")?, flag_value(args, "--pg-schema")?) {
        (None, None) => anyhow::bail!("use --pg-database and --pg-schema for native replay"),
        (Some(database), Some(schema)) => {
            ensure!(
                crate::pg::valid_schema(schema) && schema.starts_with("atlas_replay_"),
                "--pg-schema must be a lowercase name starting with atlas_replay_"
            );
            ensure!(!database.is_empty(), "--pg-database must name the expected database");
            let url = replay_url.filter(|url| !url.trim().is_empty()).context("set ATLAS_REPLAY_PG_URL for replay")?;
            let config = StorageConfig {
                backend: crate::pg::Backend::Postgres,
                url,
                schema: schema.to_owned(),
                ..StorageConfig::default()
            };
            config.check()?;
            Ok((config, database))
        }
        _ => anyhow::bail!("use --pg-database and --pg-schema together"),
    }
}

#[derive(Clone)]
struct Writer {
    pool: Arc<Pool>,
}

impl Writer {
    async fn save(&self, releases: Vec<Release>) -> Result<()> {
        let pool = self.pool.clone();
        tokio::task::spawn_blocking(move || {
            let mut conn = pool.get()?;
            let started = Instant::now();
            crate::pg::release_writer::save(&mut conn, &releases, chrono::Utc::now().timestamp())?;
            crate::profile::LOAD.writer_busy_ns.fetch_add(started.elapsed().as_nanos() as u64, Ordering::Relaxed);
            Ok(())
        })
        .await?
    }
}

fn open_pg_replay(config: StorageConfig, database: &str) -> Result<Writer> {
    let pool = Pool::new(&config)?;
    let mut conn = pool.get().context("connecting to the replay database")?;
    let mut tx = conn.transaction()?;
    let actual: String = tx.query_one("select current_database()", &[])?.get(0);
    ensure!(actual == database, "connected to {actual}, expected {database}");
    // Deliberately refuse to reuse a previous replay. The schema remains available for parity queries.
    tx.execute("select pg_advisory_xact_lock(hashtext('atlas.migrate.' || $1))", &[&pool.schema()])?;
    tx.batch_execute(&format!("create schema \"{}\"", pool.schema())).context("creating a fresh replay schema")?;
    crate::pg::migrate::run_in_transaction(&mut tx, pool.schema())?;
    // This transaction just created the schema (reuse is refused above).
    // Give direct replay saves the same real catalog key prerequisite as
    // Runtime::open; never derive a replacement from schema/name/input data.
    let mut key = [0; 8];
    getrandom::fill(&mut key).map_err(|e| anyhow::anyhow!("creating replay catalog key: {e}"))?;
    tx.execute("insert into meta(key,value) values('key_salt',$1)", &[&i64::from_le_bytes(key)])?;
    tx.commit()?;
    drop(conn);
    Ok(Writer { pool: Arc::new(pool) })
}

pub fn run_cli(args: &[String]) -> i32 {
    match run(args) {
        Ok(()) => 0,
        Err(e) => {
            println!("save replay: {e:#}");
            1
        }
    }
}

/// One slice: its group and its overview lines.
type Slice = (String, Vec<Vec<u8>>);
type MappedBlock = (Arc<memmap2::Mmap>, Entry);
type GroupCursor = (String, Vec<MappedBlock>, usize);

struct HeaderArchive {
    pool: Arc<Pool>,
    source: String,
    blocks: Vec<(String, MappedBlock)>,
    bytes: usize,
    rows: u64,
}

impl HeaderArchive {
    fn push(&mut self, group: &str, block: &MappedBlock) -> Result<()> {
        self.bytes += block.1.len as usize;
        self.blocks.push((group.to_owned(), block.clone()));
        if self.bytes >= 8 * 1024 * 1024 {
            self.flush()?;
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        if self.blocks.is_empty() {
            return Ok(());
        }
        let mut blocks = Vec::with_capacity(self.blocks.len());
        let mut rows = 0;
        for (group, (map, entry)) in &self.blocks {
            let start = usize::try_from(entry.offset)?;
            let end = start.checked_add(entry.len as usize).context("header block end offset overflows")?;
            let bytes = map.get(start..end).context("header block extends past its segment")?;
            let day = entry.min_ts.div_euclid(86_400);
            blocks.push(header_blocks::Block::new(group, day, *entry, bytes)?);
            rows += u64::from(entry.rows);
        }
        let mut conn = self.pool.get()?;
        let inserted = header_blocks::append(&mut conn, &self.source, &blocks)?;
        ensure!(inserted == blocks.len() as u64, "the fresh archive contains a duplicate block key");
        self.rows += rows;
        self.blocks.clear();
        self.bytes = 0;
        Ok(())
    }
}

/// The store's lines, group by group round robin, in slices of `slice` rows, sent to `tx` until `rows` are sent.
fn read_slices(
    store: &Store,
    slice: usize,
    rows: u64,
    tx: &std::sync::mpsc::SyncSender<Slice>,
    stop: &AtomicBool,
    mut visit_block: impl FnMut(&str, &MappedBlock) -> Result<()>,
) -> Result<()> {
    let mut groups = store.group_names();
    groups.sort();
    // each group's blocks, read one after another
    let mut cursors: Vec<GroupCursor> = groups
        .into_iter()
        .map(|g| {
            let blocks = store.blocks(&g, i64::MIN / 4, i64::MAX / 4);
            (g, blocks, 0)
        })
        .filter(|(_, b, _)| !b.is_empty())
        .collect();
    let mut sent = 0u64;
    let mut scratch = XoverStreamScratch::new();
    while !cursors.is_empty() && sent < rows && !stop.load(Ordering::Relaxed) {
        let mut k = 0;
        while k < cursors.len() && sent < rows && !stop.load(Ordering::Relaxed) {
            let (group, blocks, at) = &mut cursors[k];
            let remaining = usize::try_from(rows - sent).unwrap_or(usize::MAX);
            let target = slice.min(remaining);
            let mut lines = Vec::with_capacity(target);
            while lines.len() < target && *at < blocks.len() && !stop.load(Ordering::Relaxed) {
                let block = &blocks[*at];
                let (map, e) = block;
                *at += 1;
                let Ok(view) = NxvBlockView::from_mmap_slice(&map[e.offset as usize..]) else { continue };
                let Ok(mut cur) = NxvRangeCursor::new_for_art_range(view, 0, u64::MAX) else { continue };
                let before = lines.len();
                while lines.len() < remaining {
                    scratch.wire_buf.clear();
                    if !cur.format_next_row(&mut scratch) {
                        break;
                    }
                    let n = scratch.wire_buf.len().saturating_sub(2);
                    lines.push(scratch.wire_buf[..n].to_vec());
                }
                if lines.len() > before {
                    visit_block(group, block)?;
                }
            }
            if stop.load(Ordering::Relaxed) {
                return Ok(());
            }
            let done = *at >= blocks.len();
            if !lines.is_empty() {
                sent += lines.len() as u64;
                tx.send((group.clone(), lines))
                    .map_err(|_| anyhow::anyhow!("replay parsers stopped receiving slices"))?;
            }
            if done {
                cursors.swap_remove(k);
            } else {
                k += 1;
            }
        }
    }
    Ok(())
}

/// A slice's releases, parsed as `indexer::parse_and_save` does (without names from bodies, which need servers).
fn parse(group: &str, lines: &[Vec<u8>]) -> Vec<Release> {
    let headers: Vec<_> = lines.iter().filter_map(|l| crate::nntp::overview_of(l)).collect();
    let articles = crate::nntp::headers_to_articles(headers);
    // with the store's linker on, the articles obfuscated per article go to the header store only
    let (_held, kept): (Vec<_>, Vec<_>) = articles.into_iter().partition(crate::xref::keeps);
    let (articles, uploads, _skipped) = crate::filters::gather_per_article(group, kept);
    let mut releases: Vec<Release> = crate::collections::group_articles(group, articles).into_values().collect();
    releases.extend(uploads);
    for r in &mut releases {
        r.complete = crate::parser::is_complete(&r.articles);
        r.group = group.to_string();
        r.poster = r.articles[0].author.clone();
        r.date = r.articles[0].date.clone();
    }
    releases
}

fn postgres_counts(pool: &Pool) -> Result<(i64, i64, i64, i64)> {
    let row = pool.get()?.query_one(
        "select (select count(*) from releases), (select count(*) from files),
                (select count(*) from segments), (select coalesce(sum(parts), 0)::bigint from releases),
                (select coalesce(sum(pg_total_relation_size(oid)), 0)::bigint from pg_class
                 where relnamespace = current_schema()::regnamespace and relkind = 'r')",
        &[],
    )?;
    let parts: i64 = row.get(2);
    ensure!(parts == row.get::<_, i64>(3), "release part totals differ from the stored segments");
    Ok((row.get(0), row.get(1), parts, row.get(4)))
}

fn run(args: &[String]) -> Result<()> {
    let store_dir = PathBuf::from(args.iter().skip_while(|a| *a != "--store").nth(1).context("--store DIR")?);
    let db_dir = PathBuf::from(args.iter().skip_while(|a| *a != "--db").nth(1).context("--db DIR")?);
    let archive_source = flag_value(args, "--pg-header-source")?;
    let (config, database) = pg_target(args, std::env::var("ATLAS_REPLAY_PG_URL").ok())?;
    let (rows, slice, tasks, parsers) = (
        arg(args, "--rows", u64::MAX),
        arg(args, "--slice", 20_000usize),
        arg(args, "--tasks", 12usize),
        arg(args, "--parsers", 6usize),
    );
    anyhow::ensure!(rows > 0 && slice > 0 && tasks > 0 && parsers > 0, "replay limits must be positive");
    let live = crate::paths::app_dir();
    // compared resolved (relative paths, `..`, symlinks)
    let live_real = std::fs::canonicalize(&live).unwrap_or_else(|_| live.clone());
    let store_dir = std::fs::canonicalize(&store_dir).context("resolving the header-store copy")?;
    anyhow::ensure!(!store_dir.starts_with(&live_real), "--store must point to a copy outside {}", live.display());
    std::fs::create_dir_all(&db_dir)?;
    let db_real = std::fs::canonicalize(&db_dir)?;
    anyhow::ensure!(!db_real.starts_with(&live_real), "--db must not be in the live app folder {}", live.display());
    anyhow::ensure!(!db_real.starts_with(&store_dir), "--db must be outside the header-store copy");
    anyhow::ensure!(std::fs::read_dir(&db_real)?.next().is_none(), "--db must be an empty scratch folder");
    // The isolated app folder keeps auxiliary paths away from the live installation.
    // SAFETY: set before any other thread starts
    unsafe { std::env::set_var("ATLAS_HOME", &db_dir) };
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(4).enable_all().build()?;
    let stop = Arc::new(AtomicBool::new(false));
    let writer = open_pg_replay(config, database)?;
    let (store, _) =
        Store::open(StoreConfig::new(&store_dir), KeepSet { live_from: i64::MIN / 2, ..KeepSet::default() })?;
    let archive = archive_source.map(|source| HeaderArchive {
        pool: writer.pool.clone(),
        source: source.to_owned(),
        blocks: Vec::new(),
        bytes: 0,
        rows: 0,
    });

    let (slice_tx, slice_rx) = std::sync::mpsc::sync_channel::<Slice>(parsers * 2);
    let (parsed_tx, parsed_rx) = tokio::sync::mpsc::channel::<(usize, Vec<Release>)>(tasks * 2);
    let started = Instant::now();
    let reader = {
        let (store, stop) = (Arc::clone(&store), Arc::clone(&stop));
        std::thread::spawn(move || {
            let mut archive = archive;
            read_slices(&store, slice, rows, &slice_tx, &stop, |group, block| {
                if let Some(archive) = &mut archive {
                    archive.push(group, block)?;
                }
                Ok(())
            })?;
            if let Some(mut archive) = archive.filter(|_| !stop.load(Ordering::Relaxed)) {
                archive.flush()?;
                return Ok(Some(archive.rows));
            }
            Ok::<_, anyhow::Error>(None)
        })
    };
    let slice_rx = Arc::new(std::sync::Mutex::new(slice_rx));
    let parse_ns = Arc::new(AtomicU64::new(0));
    let mut parser_threads = Vec::new();
    for _ in 0..parsers {
        let (rx, tx, parse_ns) = (Arc::clone(&slice_rx), parsed_tx.clone(), Arc::clone(&parse_ns));
        parser_threads.push(std::thread::spawn(move || {
            loop {
                let next = rx.lock().unwrap_or_else(|e| e.into_inner()).recv();
                let Ok((group, lines)) = next else { return };
                let t = Instant::now();
                let releases = parse(&group, &lines);
                parse_ns.fetch_add(t.elapsed().as_nanos() as u64, Ordering::Relaxed);
                if tx.blocking_send((lines.len(), releases)).is_err() {
                    return;
                }
            }
        }));
    }
    drop(slice_rx);
    drop(parsed_tx);

    let processed_rows = Arc::new(AtomicU64::new(0));
    let rows_in_save_slices = Arc::new(AtomicU64::new(0));
    let report = {
        let (processed_rows, stop, parse_ns) = (Arc::clone(&processed_rows), Arc::clone(&stop), Arc::clone(&parse_ns));
        std::thread::spawn(move || {
            let (mut last_rows, mut last_busy, mut last_t) = (0u64, 0u64, Instant::now());
            while !stop.load(Ordering::Relaxed) {
                std::thread::park_timeout(Duration::from_secs(10));
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                let rows = processed_rows.load(Ordering::Relaxed);
                let busy = crate::profile::LOAD.writer_busy_ns.load(Ordering::Relaxed);
                let dt = last_t.elapsed().as_secs_f64();
                println!(
                    "{:>6.0}s  {:>11} rows  {:>8.0} rows/s  writer busy {:>4.0}%  parse {:>5.1} cores",
                    started.elapsed().as_secs_f64(),
                    rows,
                    (rows - last_rows) as f64 / dt,
                    (busy - last_busy) as f64 / 1e9 / dt * 100.0,
                    parse_ns.swap(0, Ordering::Relaxed) as f64 / 1e9 / dt
                );
                (last_rows, last_busy, last_t) = (rows, busy, Instant::now());
            }
        })
    };
    let result: Result<()> = rt.block_on(async {
        let parsed_rx = Arc::new(tokio::sync::Mutex::new(parsed_rx));
        let mut workers = tokio::task::JoinSet::new();
        for _ in 0..tasks {
            let (rx, writer) = (Arc::clone(&parsed_rx), writer.clone());
            let (processed_rows, rows_in_save_slices) = (Arc::clone(&processed_rows), Arc::clone(&rows_in_save_slices));
            workers.spawn(async move {
                loop {
                    let next = rx.lock().await.recv().await;
                    let Some((n, releases)) = next else { return Ok::<(), anyhow::Error>(()) };
                    if !releases.is_empty() {
                        writer.save(releases).await?;
                        rows_in_save_slices.fetch_add(n as u64, Ordering::Relaxed);
                    }
                    processed_rows.fetch_add(n as u64, Ordering::Relaxed);
                }
            });
        }
        while let Some(result) = workers.join_next().await {
            if let Err(error) = result.context("a replay writer task failed").and_then(|result| result) {
                stop.store(true, Ordering::Relaxed);
                return Err(error);
            }
        }
        Ok(())
    });
    let secs = started.elapsed().as_secs_f64();
    stop.store(true, Ordering::Relaxed);
    report.thread().unpark();
    // An error drops the JoinSet, which stops the other async workers. Wait for any already
    // running blocking saves to finish before inspecting the result or closing the replay.
    drop(rt);
    let reader_result = reader.join().map_err(|_| anyhow::anyhow!("the replay reader panicked"));
    let mut parser_panicked = false;
    for p in parser_threads {
        parser_panicked |= p.join().is_err();
    }
    let _ = report.join();
    let closed = store.close();
    let finished = result.and_then(|_| {
        ensure!(!parser_panicked, "a replay parser panicked");
        let archived_rows = reader_result??;
        closed?;
        Ok(archived_rows)
    });
    let archived_rows = finished
        .with_context(|| format!("staging schema {} may contain partial replay results", writer.pool.schema()))?;
    let rows = processed_rows.load(Ordering::Relaxed);
    anyhow::ensure!(rows > 0, "the replay store supplied no headers");
    let (releases, files, parts, bytes) = postgres_counts(&writer.pool)?;
    let bytes = bytes as f64;
    println!(
        "processed {rows} headers in {secs:.2}s: {:.0} headers/s; writer busy {:.1}s; db {:.2} GB",
        rows as f64 / secs,
        crate::profile::LOAD.writer_busy_ns.load(Ordering::Relaxed) as f64 / 1e9,
        bytes / 1e9
    );
    let save_rows = rows_in_save_slices.load(Ordering::Relaxed);
    println!(
        "{save_rows} headers in slices that produced releases; persisted {releases} releases, {files} files, {parts} parts"
    );
    if let Some(archived_rows) = archived_rows {
        println!("archived {archived_rows} headers in complete NXV blocks");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replay_visits_only_blocks_contributing_rows_to_the_limit() {
        use crate::nxv_store::store::Clamp;

        let dir = tempfile::tempdir().unwrap();
        let (store, _) =
            Store::open(StoreConfig::new(dir.path()), KeepSet { live_from: i64::MIN / 2, ..KeepSet::default() })
                .unwrap();
        let line = |id, date| format!("{id}\tExample\tPoster\t{date}\t<{id}@example.test>\t\t1024\t8").into_bytes();
        let clamp = Clamp::Live { now: 1_794_150_000 };
        let group = "alt.binaries.test";
        let first_day = "Sat, 07 Nov 2026 10:00:00 +0000";
        let second_day = "Sun, 08 Nov 2026 10:00:00 +0000";
        store.record("test", group, &[line(1, first_day), line(2, first_day), line(3, second_day)], clamp).unwrap();
        store.close().unwrap();
        assert_eq!(store.segments().len(), 2);

        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let mut archived_rows = 0;
        read_slices(&store, 20_000, 1, &tx, &AtomicBool::new(false), |_, (_, entry)| {
            archived_rows += entry.rows;
            Ok(())
        })
        .unwrap();
        drop(tx);
        assert_eq!(rx.into_iter().map(|(_, lines)| lines.len()).sum::<usize>(), 1);
        assert_eq!(archived_rows, 2, "include the full first block only");
    }

    #[test]
    fn postgres_replay_requires_a_named_staging_destination() {
        let args = |words: &[&str]| words.iter().map(ToString::to_string).collect::<Vec<_>>();
        let url = || Some("host=localhost dbname=atlas_test".to_owned());
        assert!(pg_target(&args(&[]), url()).is_err(), "replay must not select an implicit legacy writer");
        assert!(pg_target(&args(&["--pg-database", "atlas_test"]), url()).is_err());
        assert!(pg_target(&args(&["--pg-database", "atlas_test", "--pg-schema", "public"]), url()).is_err());
        assert!(pg_target(&args(&["--pg-database", "--pg-schema", "atlas_replay_test"]), url()).is_err());
        let target = args(&["--pg-database", "atlas_test", "--pg-schema", "atlas_replay_test"]);
        assert!(pg_target(&target, None).is_err());
        assert!(pg_target(&target, Some(" ".into())).is_err());
        let (config, database) = pg_target(&target, url()).unwrap();
        assert_eq!(database, "atlas_test");
        assert_eq!(config.schema, "atlas_replay_test");
        assert_eq!(config.url, url().unwrap());
    }
}
