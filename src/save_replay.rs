//! `atlas --save-replay --store DIR --db DIR [--rows N] [--slice N] [--tasks N] [--parsers N]`: replays real overview
//! lines from a header store through the indexer's parse and save path into a scratch database, for measuring and
//! profiling the release writer. It never touches the live databases: `--db` is a folder of its own (made if need be),
//! used as the app folder for the run. To exercise the native PostgreSQL release writer, also pass
//! `--pg-database NAME --pg-schema atlas_replay_NAME` and set `ATLAS_REPLAY_PG_URL`.
//!
//! Lines come group by group, round robin, in slices of `--slice` rows (the indexer's slices), parsed on `--parsers`
//! threads the way `indexer::parse_and_save` does (articles obfuscated per article go to the header store, which the
//! store's linker reads, soo they aren't saved here), and saved through the indexer's own writer (`shared_db`) by
//! `--tasks` slices at once, as many as the indexer keeps waiting on it. Every ten seconds it prints rows saved per
//! second, how busy the writer was, and the shard's size. The input row rate includes slices that produced no
//! releases. The final report also counts what actually persisted in the release and file tables.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, ensure};
use nntp_xover_engine::container::NxvBlockView;
use nntp_xover_engine::streamer::{NxvRangeCursor, XoverStreamScratch};

use crate::nxv_store::segment::Entry;
use crate::nxv_store::store::{KeepSet, Store, StoreConfig};
use crate::parser::Release;
use crate::pg::{Pool, StorageConfig};
use crate::store::Prepared;

fn arg<T: std::str::FromStr>(args: &[String], flag: &str, default: T) -> T {
    args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).and_then(|v| v.parse().ok()).unwrap_or(default)
}

fn pg_target(args: &[String]) -> Result<Option<(&str, &str)>> {
    let value = |flag: &str| -> Result<Option<&str>> {
        args.iter()
            .position(|arg| arg == flag)
            .map(|i| {
                args.get(i + 1)
                    .map(String::as_str)
                    .filter(|v| !v.starts_with("--"))
                    .with_context(|| format!("{flag} requires a value"))
            })
            .transpose()
    };
    match (value("--pg-database")?, value("--pg-schema")?) {
        (None, None) => Ok(None),
        (Some(database), Some(schema)) => {
            ensure!(
                crate::pg::valid_schema(schema) && schema.starts_with("atlas_replay_"),
                "--pg-schema must be a lowercase name starting with atlas_replay_"
            );
            ensure!(!database.is_empty(), "--pg-database must name the expected database");
            Ok(Some((database, schema)))
        }
        _ => anyhow::bail!("use --pg-database and --pg-schema together"),
    }
}

#[derive(Clone)]
enum Writer {
    Sqlite(crate::indexer::Db),
    Postgres(Arc<Pool>),
}

impl Writer {
    async fn save(&self, releases: Vec<Release>, prepared: Prepared) -> Result<()> {
        match self {
            Self::Sqlite(db) => db.save_prepared(releases, prepared).await,
            Self::Postgres(pool) => {
                let pool = pool.clone();
                tokio::task::spawn_blocking(move || {
                    let mut conn = pool.get()?;
                    let started = Instant::now();
                    crate::pg::release_writer::save(&mut conn, &releases, chrono::Utc::now().timestamp())?;
                    crate::profile::LOAD
                        .writer_busy_ns
                        .fetch_add(started.elapsed().as_nanos() as u64, Ordering::Relaxed);
                    Ok(())
                })
                .await?
            }
        }
    }
}

fn open_pg_replay(config: StorageConfig, database: &str) -> Result<Writer> {
    let pool = Pool::new(&config)?;
    let mut conn = pool.get().context("connecting to the replay database")?;
    let actual: String = conn.query_one("select current_database()", &[])?.get(0);
    ensure!(actual == database, "connected to {actual}, expected {database}");
    // Deliberately refuse to reuse a previous replay. The schema remains available for parity queries.
    conn.batch_execute(&format!("create schema \"{}\"", pool.schema())).context("creating a fresh replay schema")?;
    crate::pg::migrate::run(&mut conn, pool.schema())?;
    drop(conn);
    Ok(Writer::Postgres(Arc::new(pool)))
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
type GroupCursor = (String, Vec<(Arc<memmap2::Mmap>, Entry)>, usize);

fn limit_slice(lines: &mut Vec<Vec<u8>>, sent: u64, limit: u64) {
    lines.truncate(usize::try_from(limit - sent).unwrap_or(usize::MAX));
}

/// The store's lines, group by group round robin, in slices of `slice` rows, sent to `tx` until `rows` are sent.
fn read_slices(store: &Store, slice: usize, rows: u64, tx: std::sync::mpsc::SyncSender<Slice>, stop: &AtomicBool) {
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
        while k < cursors.len() && sent < rows {
            let (group, blocks, at) = &mut cursors[k];
            let mut lines = Vec::with_capacity(slice);
            while lines.len() < slice && *at < blocks.len() {
                let (map, e) = &blocks[*at];
                *at += 1;
                let Ok(view) = NxvBlockView::from_mmap_slice(&map[e.offset as usize..]) else { continue };
                let Ok(mut cur) = NxvRangeCursor::new_for_art_range(view, 0, u64::MAX) else { continue };
                loop {
                    scratch.wire_buf.clear();
                    if !cur.format_next_row(&mut scratch) {
                        break;
                    }
                    let n = scratch.wire_buf.len().saturating_sub(2);
                    lines.push(scratch.wire_buf[..n].to_vec());
                }
            }
            let done = *at >= blocks.len();
            if !lines.is_empty() {
                limit_slice(&mut lines, sent, rows);
                sent += lines.len() as u64;
                if tx.send((group.clone(), lines)).is_err() {
                    return;
                }
            }
            if done {
                cursors.swap_remove(k);
            } else {
                k += 1;
            }
        }
    }
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

fn files_size(main: &Path) -> u64 {
    std::iter::once(main.to_path_buf())
        .chain(crate::store::shard_paths(main))
        .flat_map(|p| {
            let wal = PathBuf::from(format!("{}-wal", p.display()));
            [p, wal]
        })
        .filter_map(|p| std::fs::metadata(p).ok())
        .map(|m| m.len())
        .sum()
}

fn persisted_counts(main: &Path) -> Result<(i64, i64, i64)> {
    let (mut releases, mut files, mut parts) = (0, 0, 0);
    for path in crate::store::shard_paths(main).into_iter().filter(|path| path.exists()) {
        let conn = rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let (shard_releases, shard_parts): (i64, i64) =
            conn.query_row("select count(*), coalesce(sum(parts), 0) from releases", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?;
        releases += shard_releases;
        parts += shard_parts;
        files += conn.query_row::<i64, _, _>("select count(*) from files", [], |row| row.get(0))?;
    }
    Ok((releases, files, parts))
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
    let pg = pg_target(args)?
        .map(|(database, schema)| {
            let mut config = crate::config::storage_config();
            if let Ok(url) = std::env::var("ATLAS_REPLAY_PG_URL") {
                config.url = url;
            }
            config.schema = schema.to_owned();
            config.backend = crate::pg::Backend::Postgres;
            config.check()?;
            Ok::<_, anyhow::Error>((config, database))
        })
        .transpose()?;
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
    // the run's app folder: the checkpointer and anything else looking for the database find the scratch one
    // SAFETY: set before any other thread starts
    unsafe { std::env::set_var("ATLAS_HOME", &db_dir) };
    let main = db_dir.join("atlas.db");
    let stop = Arc::new(AtomicBool::new(false));
    let writer = if let Some((config, database)) = pg {
        open_pg_replay(config, database)?
    } else {
        crate::db::create_db_at(&main)?;
        Writer::Sqlite(crate::indexer::shared_db(crate::db::open_at(&main)?))
    };
    let (store, _) =
        Store::open(StoreConfig::new(&store_dir), KeepSet { live_from: i64::MIN / 2, ..KeepSet::default() })?;
    let sqlite = matches!(&writer, Writer::Sqlite(_));
    let checkpointer = sqlite.then(|| crate::db::checkpointer(stop.clone()));

    let (slice_tx, slice_rx) = std::sync::mpsc::sync_channel::<Slice>(parsers * 2);
    let (parsed_tx, parsed_rx) = tokio::sync::mpsc::channel::<(usize, Vec<Release>, Prepared)>(tasks * 2);
    let reader = {
        let (store, stop) = (Arc::clone(&store), Arc::clone(&stop));
        std::thread::spawn(move || read_slices(&store, slice, rows, slice_tx, &stop))
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
                // as the indexer's parse step does: what the writer would otherwise work out
                let prepared = if sqlite { crate::store::prepare(&releases) } else { Prepared::new() };
                parse_ns.fetch_add(t.elapsed().as_nanos() as u64, Ordering::Relaxed);
                if tx.blocking_send((lines.len(), releases, prepared)).is_err() {
                    return;
                }
            }
        }));
    }
    drop(parsed_tx);

    let processed_rows = Arc::new(AtomicU64::new(0));
    let rows_in_save_slices = Arc::new(AtomicU64::new(0));
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(4).enable_all().build()?;
    let started = Instant::now();
    let report = {
        let (processed_rows, stop, main, parse_ns) =
            (Arc::clone(&processed_rows), Arc::clone(&stop), main.clone(), Arc::clone(&parse_ns));
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
                let sqlite_size =
                    if sqlite { format!("  db {:.2} GB", files_size(&main) as f64 / 1e9) } else { String::new() };
                println!(
                    "{:>6.0}s  {:>11} rows  {:>8.0} rows/s  writer busy {:>4.0}%  parse {:>5.1} cores{sqlite_size}",
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
        let mut handles = Vec::new();
        for _ in 0..tasks {
            let (rx, writer) = (Arc::clone(&parsed_rx), writer.clone());
            let (processed_rows, rows_in_save_slices) = (Arc::clone(&processed_rows), Arc::clone(&rows_in_save_slices));
            handles.push(tokio::spawn(async move {
                loop {
                    let next = rx.lock().await.recv().await;
                    let Some((n, releases, prepared)) = next else { return Ok::<(), anyhow::Error>(()) };
                    if !releases.is_empty() {
                        writer.save(releases, prepared).await?;
                        rows_in_save_slices.fetch_add(n as u64, Ordering::Relaxed);
                    }
                    processed_rows.fetch_add(n as u64, Ordering::Relaxed);
                }
            }));
        }
        for h in handles {
            h.await??;
        }
        Ok(())
    });
    let secs = started.elapsed().as_secs_f64();
    stop.store(true, Ordering::Relaxed);
    report.thread().unpark();
    let reader_result = reader.join().map_err(|_| anyhow::anyhow!("the replay reader panicked"));
    let mut parser_panicked = false;
    for p in parser_threads {
        parser_panicked |= p.join().is_err();
    }
    let _ = report.join();
    if let Some(checkpointer) = checkpointer {
        let _ = checkpointer.join();
    }
    let closed = store.close();
    result?;
    reader_result?;
    anyhow::ensure!(!parser_panicked, "a replay parser panicked");
    closed?;
    let rows = processed_rows.load(Ordering::Relaxed);
    anyhow::ensure!(rows > 0, "the replay store supplied no headers");
    let (releases, files, parts, bytes) = match writer {
        Writer::Sqlite(db) => {
            drop(db);
            let (releases, files, parts) = persisted_counts(&main)?;
            (releases, files, parts, files_size(&main) as f64)
        }
        Writer::Postgres(pool) => {
            let (releases, files, parts, bytes) = postgres_counts(&pool)?;
            (releases, files, parts, bytes as f64)
        }
    };
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
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{limit_slice, pg_target};

    #[test]
    fn a_replay_stops_at_the_requested_row_count() {
        let mut lines = vec![vec![1], vec![2], vec![3]];
        limit_slice(&mut lines, 4, 6);
        assert_eq!(lines, vec![vec![1], vec![2]]);
    }

    #[test]
    fn postgres_replay_requires_a_named_staging_destination() {
        let args = |words: &[&str]| words.iter().map(ToString::to_string).collect::<Vec<_>>();
        assert!(pg_target(&args(&["--pg-database", "atlas_test"])).is_err());
        assert!(pg_target(&args(&["--pg-database", "atlas_test", "--pg-schema", "public"])).is_err());
        assert!(pg_target(&args(&["--pg-database", "--pg-schema", "atlas_replay_test"])).is_err());
        assert_eq!(
            pg_target(&args(&["--pg-database", "atlas_test", "--pg-schema", "atlas_replay_test"])).unwrap(),
            Some(("atlas_test", "atlas_replay_test"))
        );
    }
}
