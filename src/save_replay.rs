//! `atlas --save-replay --store DIR --db DIR [--rows N] [--slice N] [--tasks N] [--parsers N]`: replays real overview
//! lines from a header store through the indexer's parse and save path into a scratch database, for measuring and
//! profiling the release writer. It never touches the live databases: `--db` is a folder of its own (made if need be),
//! used as the app folder for the run.
//!
//! Lines come group by group, round robin, in slices of `--slice` rows (the indexer's slices), parsed on `--parsers`
//! threads the way `indexer::parse_and_save` does (articles obfuscated per article go to the header store, which the
//! store's linker reads, soo they aren't saved here), and saved through the indexer's own writer (`shared_db`) by
//! `--tasks` slices at once, as many as the indexer keeps waiting on it. Every ten seconds it prints rows saved per
//! second, how busy the writer was, and the shard's size.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use nntp_xover_engine::container::NxvBlockView;
use nntp_xover_engine::streamer::{NxvRangeCursor, XoverStreamScratch};

use crate::nxv_store::store::{KeepSet, Store, StoreConfig};
use crate::parser::Release;

fn arg<T: std::str::FromStr>(args: &[String], flag: &str, default: T) -> T {
    args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).and_then(|v| v.parse().ok()).unwrap_or(default)
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

/// The store's lines, group by group round robin, in slices of `slice` rows, sent to `tx` until `rows` are sent.
fn read_slices(store: &Store, slice: usize, rows: u64, tx: std::sync::mpsc::SyncSender<Slice>, stop: &AtomicBool) {
    let mut groups = store.group_names();
    groups.sort();
    // each group's blocks, read one after another
    let mut cursors: Vec<(String, Vec<(Arc<memmap2::Mmap>, crate::nxv_store::segment::Entry)>, usize)> = groups
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

fn run(args: &[String]) -> Result<()> {
    let store_dir = PathBuf::from(args.iter().skip_while(|a| *a != "--store").nth(1).context("--store DIR")?);
    let db_dir = PathBuf::from(args.iter().skip_while(|a| *a != "--db").nth(1).context("--db DIR")?);
    let (rows, slice, tasks, parsers) = (
        arg(args, "--rows", u64::MAX),
        arg(args, "--slice", 20_000usize),
        arg(args, "--tasks", 12usize),
        arg(args, "--parsers", 6usize),
    );
    let live = crate::paths::app_dir();
    anyhow::ensure!(!db_dir.starts_with(&live), "--db must not be in the live app folder {}", live.display());
    std::fs::create_dir_all(&db_dir)?;
    // the run's app folder: the checkpointer and anything else looking for the database find the scratch one
    // SAFETY: set before any other thread starts
    unsafe { std::env::set_var("ATLAS_HOME", &db_dir) };
    let main = db_dir.join("atlas.db");
    crate::db::create_db_at(&main)?;
    let db = crate::indexer::shared_db(crate::db::open_at(&main)?);
    let stop = Arc::new(AtomicBool::new(false));
    let checkpointer = crate::db::checkpointer(stop.clone());

    let (store, _) =
        Store::open(StoreConfig::new(&store_dir), KeepSet { live_from: i64::MIN / 2, ..KeepSet::default() })?;
    let store = Arc::new(store);
    let (slice_tx, slice_rx) = std::sync::mpsc::sync_channel::<Slice>(parsers * 2);
    let (parsed_tx, parsed_rx) = tokio::sync::mpsc::channel::<(usize, Vec<Release>)>(tasks * 2);
    let reader = {
        let (store, stop) = (Arc::clone(&store), Arc::clone(&stop));
        std::thread::spawn(move || read_slices(&store, slice, rows, slice_tx, &stop))
    };
    let slice_rx = Arc::new(std::sync::Mutex::new(slice_rx));
    let parse_ns = Arc::new(AtomicU64::new(0));
    let mut parser_threads = Vec::new();
    for _ in 0..parsers.max(1) {
        let (rx, tx, parse_ns) = (Arc::clone(&slice_rx), parsed_tx.clone(), Arc::clone(&parse_ns));
        parser_threads.push(std::thread::spawn(move || {
            loop {
                let next = rx.lock().unwrap_or_else(|e| e.into_inner()).recv();
                let Ok((group, lines)) = next else { return };
                let t = Instant::now();
                let releases = parse(&group, &lines);
                parse_ns.fetch_add(t.elapsed().as_nanos() as u64, Ordering::Relaxed);
                if releases.is_empty() {
                    continue;
                }
                if tx.blocking_send((lines.len(), releases)).is_err() {
                    return;
                }
            }
        }));
    }
    drop(parsed_tx);

    let saved_rows = Arc::new(AtomicU64::new(0));
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(4).enable_all().build()?;
    let started = Instant::now();
    let report = {
        let (saved_rows, stop, main, parse_ns) =
            (Arc::clone(&saved_rows), Arc::clone(&stop), main.clone(), Arc::clone(&parse_ns));
        std::thread::spawn(move || {
            let (mut last_rows, mut last_busy, mut last_t) = (0u64, 0u64, Instant::now());
            while !stop.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_secs(10));
                let rows = saved_rows.load(Ordering::Relaxed);
                let busy = crate::profile::LOAD.writer_busy_ns.load(Ordering::Relaxed);
                let dt = last_t.elapsed().as_secs_f64();
                println!(
                    "{:>6.0}s  {:>11} rows  {:>8.0} rows/s  writer busy {:>4.0}%  parse {:>5.1} cores  db {:.2} GB",
                    started.elapsed().as_secs_f64(),
                    rows,
                    (rows - last_rows) as f64 / dt,
                    (busy - last_busy) as f64 / 1e9 / dt * 100.0,
                    parse_ns.swap(0, Ordering::Relaxed) as f64 / 1e9 / dt,
                    files_size(&main) as f64 / 1e9
                );
                (last_rows, last_busy, last_t) = (rows, busy, Instant::now());
            }
        })
    };
    let result: Result<()> = rt.block_on(async {
        let parsed_rx = Arc::new(tokio::sync::Mutex::new(parsed_rx));
        let mut handles = Vec::new();
        for _ in 0..tasks.max(1) {
            let (rx, db, saved_rows) = (Arc::clone(&parsed_rx), db.clone(), Arc::clone(&saved_rows));
            handles.push(tokio::spawn(async move {
                loop {
                    let next = rx.lock().await.recv().await;
                    let Some((n, releases)) = next else { return Ok::<(), anyhow::Error>(()) };
                    db.save(releases).await?;
                    saved_rows.fetch_add(n as u64, Ordering::Relaxed);
                }
            }));
        }
        for h in handles {
            h.await??;
        }
        Ok(())
    });
    stop.store(true, Ordering::Relaxed);
    let _ = reader.join();
    for p in parser_threads {
        let _ = p.join();
    }
    let _ = report.join();
    let _ = checkpointer.join();
    drop(db);
    let secs = started.elapsed().as_secs_f64();
    let rows = saved_rows.load(Ordering::Relaxed);
    println!(
        "saved {rows} rows in {secs:.1}s: {:.0} rows/s; writer busy {:.1}s; db {:.2} GB",
        rows as f64 / secs,
        crate::profile::LOAD.writer_busy_ns.load(Ordering::Relaxed) as f64 / 1e9,
        files_size(&main) as f64 / 1e9
    );
    let _ = store.close();
    result
}
