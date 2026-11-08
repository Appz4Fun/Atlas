//! `atlas --header-store`: tools for trying the store beside the indexer, never touching a live store.
//!
//! - `import --to DIR [--files N]`: copies the header window's lines (read only) into a store at DIR, the way the
//!   indexer would record them, and reports the write rate and the size on disk.
//! - `link --store DIR --from UNIX --to UNIX`: one linker pass over the store's obfuscated rows posted in that span
//!   (with two hours either side), grouped by poster and chained into uploads by the linker's own rule, and reports
//!   what it found next to the uploads the SQLite linker saved for the same span.
//! - `stats --store DIR`: segments, blocks, rows and bytes.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use nntp_xover_engine::container::NxvBlockView;
use nntp_xover_engine::streamer::{NxvRangeCursor, XoverStreamScratch};

use super::store::{Clamp, DAY, KeepSet, Store, StoreConfig};

fn arg<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).map(String::as_str)
}

pub fn run_cli(args: &[String]) -> i32 {
    let rest: Vec<String> = args.iter().skip_while(|a| *a != "--header-store").skip(1).cloned().collect();
    let r = match rest.first().map(String::as_str) {
        Some("import") => import(&rest),
        Some("link") => link(&rest),
        Some("stats") => stats(&rest),
        _ => {
            println!(
                "usage: atlas --header-store import --to DIR [--files N] | link --store DIR --from UNIX --to UNIX | stats --store DIR"
            );
            return 1;
        }
    };
    match r {
        Ok(()) => 0,
        Err(e) => {
            println!("header store: {e:#}");
            1
        }
    }
}

fn keep_all() -> KeepSet {
    KeepSet { live_from: i64::MIN / 2, ..KeepSet::default() }
}

fn dir_bytes(dir: &Path) -> u64 {
    let mut n = 0;
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                n += dir_bytes(&p);
            } else {
                n += e.metadata().map(|m| m.len()).unwrap_or(0);
            }
        }
    }
    n
}

fn import(args: &[String]) -> Result<()> {
    let to = PathBuf::from(arg(args, "--to").context("--to DIR")?);
    let limit: usize = arg(args, "--files").and_then(|n| n.parse().ok()).unwrap_or(usize::MAX);
    let window = crate::config::header_window_config().root();
    anyhow::ensure!(to != window && !to.starts_with(&window), "--to must be outside the header window");
    let (blocks, tails, _, _) = crate::nxv_census::walk(&window)?;
    let mut files: Vec<(bool, String, PathBuf, u64)> = blocks
        .into_iter()
        .map(|(s, p, n)| (true, s, p, n))
        .chain(tails.into_iter().map(|(s, p, n)| (false, s, p, n)))
        .collect();
    // posting hour order, the way reading goes
    files.sort_by_key(|f| {
        f.2.file_stem().and_then(|s| s.to_str()).and_then(|s| s.split('-').next()?.parse::<i64>().ok())
    });
    files.truncate(limit);
    let in_bytes: u64 = files.iter().map(|f| f.3).sum();
    let (store, _) = Store::open(StoreConfig::new(&to), keep_all())?;
    let now = chrono::Utc::now().timestamp();
    let t = Instant::now();
    let (mut rows, mut read_ns) = (0u64, 0u128);
    let mut last_ticket = None;
    for (k, (is_block, server, path, _)) in files.iter().enumerate() {
        let Some(group) = path.parent().and_then(crate::header_window::decode) else { continue };
        let Some(server) = crate::header_window::decode(Path::new(server)) else { continue };
        let r = Instant::now();
        let lines = if *is_block {
            crate::nxv_census::read_block(path).map(|b| b.0)
        } else {
            crate::nxv_census::read_tail(path)
        };
        read_ns += r.elapsed().as_nanos();
        let Ok(lines) = lines else { continue };
        rows += lines.len() as u64;
        last_ticket = Some(store.record(&server, &group, &lines, Clamp::Unknown { now })?);
        if k % 2000 == 1999 {
            let s = t.elapsed().as_secs_f64();
            println!("  {}/{} files, {rows} rows, {:.0} rows/s", k + 1, files.len(), rows as f64 / s);
        }
    }
    if let Some(t) = last_ticket {
        t.wait(Duration::from_secs(600))?;
    }
    let recorded = t.elapsed();
    store.close()?;
    let total = t.elapsed();
    let (unkept, repeats, blocks) = store.counters();
    let out_bytes = dir_bytes(&to);
    println!("imported {} window files ({:.2} GB) into {}", files.len(), in_bytes as f64 / 1e9, to.display());
    println!(
        "rows {rows}: recorded and durable in {:.1}s ({:.0} rows/s, of which reading the window {:.1}s); sealed and committed by {:.1}s ({:.0} rows/s)",
        recorded.as_secs_f64(),
        rows as f64 / recorded.as_secs_f64(),
        read_ns as f64 / 1e9,
        total.as_secs_f64(),
        rows as f64 / total.as_secs_f64()
    );
    println!("blocks {blocks}, repeats dropped {repeats}, rows of days not kept {unkept}");
    println!(
        "store on disk {:.2} GB ({:.1} bytes/row) vs window {:.2} GB",
        out_bytes as f64 / 1e9,
        out_bytes as f64 / rows.max(1) as f64,
        in_bytes as f64 / 1e9
    );
    Ok(())
}

fn stats(args: &[String]) -> Result<()> {
    let dir = PathBuf::from(arg(args, "--store").context("--store DIR")?);
    let (store, opened) = Store::open(StoreConfig::new(&dir), keep_all())?;
    println!(
        "{}: {} groups, {} segments, {} blocks, {:.2} GB",
        dir.display(),
        store.group_names().len(),
        opened.segments,
        opened.blocks,
        dir_bytes(&dir) as f64 / 1e9
    );
    let mut rows = 0u64;
    let mut sizes: Vec<u32> = Vec::new();
    for g in store.group_names() {
        for (_, e) in store.blocks(&g, i64::MIN / 4, i64::MAX / 4) {
            rows += e.rows as u64;
            sizes.push(e.rows);
        }
    }
    sizes.sort_unstable();
    if !sizes.is_empty() {
        println!(
            "rows {rows}; rows per block median {}, p10 {}, p90 {}",
            sizes[sizes.len() / 2],
            sizes[sizes.len() / 10],
            sizes[sizes.len() * 9 / 10]
        );
    }
    store.close()
}

/// One article the linker considers.
struct Art {
    poster: String,
    posted: i64,
    group: i64,
    bytes: i64,
}

fn link(args: &[String]) -> Result<()> {
    let dir = PathBuf::from(arg(args, "--store").context("--store DIR")?);
    let from: i64 = arg(args, "--from").context("--from UNIX")?.parse()?;
    let to: i64 = arg(args, "--to").context("--to UNIX")?.parse()?;
    let (store, _) = Store::open(StoreConfig::new(&dir), keep_all())?;
    let t = Instant::now();
    let (lo, hi) = (from - 2 * 3600, to + 2 * 3600);
    let mut group_ids: HashMap<String, i64> = HashMap::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut arts: Vec<Art> = Vec::new();
    let (mut blocks, mut rows, mut dup) = (0u64, 0u64, 0u64);
    let mut scratch = XoverStreamScratch::new();
    for g in store.group_names() {
        let gid = {
            let n = group_ids.len() as i64;
            *group_ids.entry(g.clone()).or_insert(n)
        };
        // block ranges are UTC and the linker's times the poster's wall clock: a margin for any time zone
        for (map, e) in store.blocks(&g, lo - 14 * 3600, hi + 14 * 3600) {
            blocks += 1;
            let view = NxvBlockView::from_mmap_slice(&map[e.offset as usize..])?;
            let mut cur = NxvRangeCursor::new_for_art_range(view, 0, u64::MAX)?;
            loop {
                scratch.wire_buf.clear();
                if !cur.format_next_row(&mut scratch) {
                    break;
                }
                rows += 1;
                let n = scratch.wire_buf.len().saturating_sub(2);
                let Some(ov) = crate::nntp::overview_of(&scratch.wire_buf[..n]) else { continue };
                let a = ov.into_article();
                if !crate::xref::keeps(&a) {
                    continue;
                }
                // the poster's wall clock, as the SQLite linker keeps it (`Store::record_chunked`), soo the two compare
                let Some(posted) = crate::dates::naive_timestamp(&a.date) else { continue };
                if posted < lo || posted > hi {
                    continue;
                }
                if !seen.insert(a.message_id.clone()) {
                    dup += 1;
                    continue;
                }
                arts.push(Art { poster: a.author, posted, group: gid, bytes: a.bytes });
            }
        }
    }
    let read = t.elapsed();
    let mut by_poster: BTreeMap<String, crate::xref::FileSum> = BTreeMap::new();
    for a in &arts {
        let f = by_poster.entry(a.poster.clone()).or_insert_with(|| crate::xref::FileSum {
            poster: a.poster.clone(),
            n: 0,
            first: i64::MAX,
            last: i64::MIN,
            added: 0,
            groups: BTreeSet::new(),
        });
        f.n += 1;
        f.first = f.first.min(a.posted);
        f.last = f.last.max(a.posted);
        f.groups.insert(a.group);
    }
    let files: Vec<crate::xref::FileSum> = by_poster.into_values().collect();
    let chains = crate::xref::chain(&files, &[]);
    let mut bytes_by_poster: HashMap<&str, i64> = HashMap::new();
    for a in &arts {
        *bytes_by_poster.entry(a.poster.as_str()).or_default() += a.bytes.max(0);
    }
    let (mut uploads, mut up_files, mut up_articles, mut up_bytes, mut cross) = (0u64, 0u64, 0i64, 0i64, 0u64);
    let mut noise = 0u64;
    for (_, members) in &chains {
        let articles: i64 = members.iter().map(|&i| files[i].n).sum();
        let first = members.iter().map(|&i| files[i].first).min().unwrap_or(0);
        if first < from || first > to {
            continue;
        }
        if articles < crate::xref::MIN_UPLOAD_ARTICLES {
            noise += 1;
            continue;
        }
        uploads += 1;
        up_files += members.len() as u64;
        up_articles += articles;
        up_bytes +=
            members.iter().map(|&i| bytes_by_poster.get(files[i].poster.as_str()).copied().unwrap_or(0)).sum::<i64>();
        let groups: BTreeSet<i64> = members.iter().flat_map(|&i| files[i].groups.iter().copied()).collect();
        if groups.len() > 1 {
            cross += 1;
        }
    }
    let total = t.elapsed();
    println!(
        "store pass over {}..{} (±2h): {blocks} blocks, {rows} rows read in {:.1}s; {} obfuscated articles kept ({dup} repeats across servers dropped), {} files",
        crate::xref::iso_pub(from),
        crate::xref::iso_pub(to),
        read.as_secs_f64(),
        arts.len(),
        files.len()
    );
    println!(
        "uploads starting in the span: {uploads} ({cross} across groups), {up_files} files, {up_articles} articles, {:.1} GB; {noise} too small; pass took {:.1}s",
        up_bytes as f64 / 1e9,
        total.as_secs_f64()
    );
    // the SQLite linker's uploads for the same span, read only
    let xdb = crate::xref::path_for(&crate::paths::database());
    if xdb.exists() {
        let conn = rusqlite::Connection::open_with_flags(&xdb, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        conn.busy_timeout(Duration::from_secs(10))?;
        let (n, f): (i64, Option<i64>) = conn.query_row(
            "select count(*), sum(files) from uploads where first between ?1 and ?2",
            [from, to],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        println!("SQLite linker uploads starting in the span: {n}, {} files", f.unwrap_or(0));
    }
    store.close()?;
    let _ = DAY;
    Ok(())
}
