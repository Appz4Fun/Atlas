//! `atlas --header-store`: tools for trying the store beside the indexer, never touching a live store.
//!
//! - `import --to DIR [--files N]`: copies the header window's lines (read only) into a store at DIR, the way the
//!   indexer would record them, and reports the write rate and the size on disk.
//! - `link --store DIR --from UNIX --to UNIX`: one linker pass over the store's obfuscated rows posted in that span
//!   (with two hours either side), grouped by poster and chained into uploads by the linker's own rule, and reports
//!   what it found next to the uploads the SQLite linker saved for the same span.
//! - `link-day --store DIR --day YYYY-MM-DD`: the store's own linker (`nxv_store::linker`) over one wall-clock day,
//!   every hour of it taken as ready and every other hour as linked already, its state in a scratch folder beside the
//!   store; reports the releases it would save (nothing is saved) and how long the day took.
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
        Some("link-day") => link_day(&rest),
        Some("stats") => stats(&rest),
        _ => {
            println!(
                "usage: atlas --header-store import --to DIR [--files N] | link --store DIR --from UNIX --to UNIX | link-day --store DIR --day YYYY-MM-DD [--from UNIX --to UNIX] | stats --store DIR"
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

/// The posting hour (unix seconds) a header window file holds: `HOUR-PART.nxv` or `HOUR.tail`.
fn file_hour(path: &Path) -> Option<i64> {
    path.file_stem()?.to_str()?.split('-').next()?.parse().ok()
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
    files.sort_by_key(|f| file_hour(&f.2));
    files.truncate(limit);
    let in_bytes: u64 = files.iter().map(|f| f.3).sum();
    let (store, _) = Store::open(StoreConfig::new(&to), keep_all())?;
    let now = chrono::Utc::now().timestamp();
    let t = Instant::now();
    let (mut rows, mut read_ns) = (0u64, 0u128);
    let mut last_ticket: Option<super::store::Ticket> = None;
    // days the import has moved two past get sealed, soo open slabs (and memory) stay a couple of days deep
    let mut open_days: BTreeSet<i64> = BTreeSet::new();
    for (k, (is_block, server, path, _)) in files.iter().enumerate() {
        if let Some(day) = file_hour(path).map(|h| h.div_euclid(DAY)) {
            open_days.insert(day);
            let done: Vec<i64> = open_days.range(..day - 1).copied().collect();
            if !done.is_empty() {
                store.seal_days(&done);
                for d in &done {
                    open_days.remove(d);
                }
            }
        }
        // the logs kept no more than a few hundred files behind
        if k % 500 == 499
            && let Some(t) = &last_ticket
        {
            t.wait(Duration::from_secs(600))?;
        }
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
    id: String,
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
                arts.push(Art { id: a.message_id, poster: a.author, posted, group: gid, bytes: a.bytes });
            }
        }
    }
    let read = t.elapsed();
    let mine = chain_summary(&arts, from, to);
    let total = t.elapsed();
    println!(
        "store pass over {}..{} (±2h): {blocks} blocks, {rows} rows read in {:.1}s; {} obfuscated articles kept ({dup} repeats dropped), {} files",
        crate::xref::iso_pub(from),
        crate::xref::iso_pub(to),
        read.as_secs_f64(),
        arts.len(),
        mine.files
    );
    println!("  {}; pass took {:.1}s", mine.line(), total.as_secs_f64());
    // the SQLite linker's own store over the same span, read only: its headers chained the same way, and the uploads
    // it saved
    let xdb = crate::xref::path_for(&crate::paths::database());
    if xdb.exists() {
        let t = Instant::now();
        let conn = rusqlite::Connection::open_with_flags(&xdb, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        conn.busy_timeout(Duration::from_secs(10))?;
        let mut stmt =
            conn.prepare("select message_id, poster, posted, grp, bytes from headers where posted between ?1 and ?2")?;
        let theirs: Vec<Art> = stmt
            .query_map([lo, hi], |r| {
                Ok(Art { id: r.get(0)?, poster: r.get(1)?, posted: r.get(2)?, group: r.get(3)?, bytes: r.get(4)? })
            })?
            .collect::<rusqlite::Result<_>>()?;
        let sum = chain_summary(&theirs, from, to);
        println!(
            "SQLite store's headers over the same span: {} articles, {} files, read in {:.1}s",
            theirs.len(),
            sum.files,
            t.elapsed().as_secs_f64()
        );
        println!("  {}", sum.line());
        // the articles both hold, each side's rows of them chained apart: the same input should link the same way
        let their_ids: HashSet<&str> = theirs.iter().map(|a| a.id.as_str()).collect();
        let common_mine: Vec<&Art> = arts.iter().filter(|a| their_ids.contains(a.id.as_str())).collect();
        let common_theirs: Vec<&Art> = theirs.iter().filter(|a| seen.contains(&a.id)).collect();
        println!(
            "articles in both: {} ({:.1}% of the SQLite store's, {:.1}% of this store's)",
            common_mine.len(),
            common_mine.len() as f64 * 100.0 / theirs.len().max(1) as f64,
            common_mine.len() as f64 * 100.0 / arts.len().max(1) as f64
        );
        let owned = |v: &[&Art]| -> Vec<Art> {
            v.iter()
                .map(|a| Art {
                    id: a.id.clone(),
                    poster: a.poster.clone(),
                    posted: a.posted,
                    group: a.group,
                    bytes: a.bytes,
                })
                .collect()
        };
        println!("  this store, those articles: {}", chain_summary(&owned(&common_mine), from, to).line());
        println!("  SQLite store, those articles: {}", chain_summary(&owned(&common_theirs), from, to).line());
        let (n, f): (i64, Option<i64>) = conn.query_row(
            "select count(*), sum(files) from uploads where first between ?1 and ?2",
            [from, to],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        println!("SQLite linker uploads saved starting in the span: {n}, {} files", f.unwrap_or(0));
    }
    store.close()
}

/// What one linker pass over a set of articles makes of the span `from..=to`.
struct Summary {
    files: usize,
    uploads: u64,
    cross: u64,
    up_files: u64,
    articles: i64,
    bytes: i64,
    noise: u64,
}

impl Summary {
    fn line(&self) -> String {
        format!(
            "uploads starting in the span: {} ({} across groups), {} files, {} articles, {:.1} GB; {} too small",
            self.uploads,
            self.cross,
            self.up_files,
            self.articles,
            self.bytes as f64 / 1e9,
            self.noise
        )
    }
}

/// Articles grouped into files by poster and the files chained into uploads by the linker's rule (`xref::chain`);
/// the uploads whose first article falls in the span are counted.
fn chain_summary(arts: &[Art], from: i64, to: i64) -> Summary {
    let mut by_poster: BTreeMap<&str, crate::xref::FileSum> = BTreeMap::new();
    let mut bytes_by_poster: HashMap<&str, i64> = HashMap::new();
    for a in arts {
        let f = by_poster.entry(a.poster.as_str()).or_insert_with(|| crate::xref::FileSum {
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
        *bytes_by_poster.entry(a.poster.as_str()).or_default() += a.bytes.max(0);
    }
    let files: Vec<crate::xref::FileSum> = by_poster.into_values().collect();
    let chains = crate::xref::chain(&files, &[]);
    let mut s = Summary { files: files.len(), uploads: 0, cross: 0, up_files: 0, articles: 0, bytes: 0, noise: 0 };
    for (_, members) in &chains {
        let articles: i64 = members.iter().map(|&i| files[i].n).sum();
        let first = members.iter().map(|&i| files[i].first).min().unwrap_or(0);
        if first < from || first > to {
            continue;
        }
        if articles < crate::xref::MIN_UPLOAD_ARTICLES {
            s.noise += 1;
            continue;
        }
        s.uploads += 1;
        s.up_files += members.len() as u64;
        s.articles += articles;
        s.bytes +=
            members.iter().map(|&i| bytes_by_poster.get(files[i].poster.as_str()).copied().unwrap_or(0)).sum::<i64>();
        let groups: BTreeSet<i64> = members.iter().flat_map(|&i| files[i].groups.iter().copied()).collect();
        if groups.len() > 1 {
            s.cross += 1;
        }
    }
    s
}

fn link_day(args: &[String]) -> Result<()> {
    use super::linker::{Linker, State};
    let dir = PathBuf::from(arg(args, "--store").context("--store DIR")?);
    let day_name = arg(args, "--day").context("--day YYYY-MM-DD")?;
    let day = super::segment::parse_day_name(day_name).context("--day YYYY-MM-DD")?;
    // only these hours of it, when given (wall clock, unix seconds)
    let from: i64 = arg(args, "--from").map_or(Ok(day * DAY), str::parse)?;
    let to: i64 = arg(args, "--to").map_or(Ok(day * DAY + DAY - 1), str::parse)?;
    let (first_hour, last_hour) = (from.div_euclid(3600), to.div_euclid(3600));
    let (store, _) = Store::open(StoreConfig::new(&dir), keep_all())?;
    let state_dir = dir.parent().unwrap_or(Path::new(".")).join(format!("linkstate-{day_name}"));
    let _ = std::fs::remove_dir_all(&state_dir);
    std::fs::create_dir_all(&state_dir)?;
    // every hour but the day's linked already, every block read already: the passes take that day alone
    let now = (day + 40) * DAY;
    let mut state = State::default();
    let segs = store.segments();
    let (first_day, last_day) =
        (segs.iter().map(|s| s.1).min().unwrap_or(day), segs.iter().map(|s| s.1).max().unwrap_or(day));
    for h in (first_day - 2) * 24..(last_day + 3) * 24 {
        if h < first_hour || h > last_hour {
            state.linked.insert(h);
        }
    }
    for (g, d, n) in &segs {
        state.counts.insert((g.clone(), *d), *n as u32);
        state.seen.insert(*d, now - 2 * DAY);
    }
    state.save(&state_dir)?;
    let mut linker = Linker::open_in(&state_dir, now)?;
    let t = Instant::now();
    let (mut releases, mut uploads, mut late, mut articles, mut files, mut blocks, mut rows, mut kept, mut read) =
        (0usize, 0u64, 0u64, 0usize, 0usize, 0u64, 0u64, 0u64, 0f64);
    let mut bytes = 0i64;
    let mut cross = 0usize;
    let mut passes = 0;
    // the uploads whose first article falls in the hours asked for, as `link` counts them
    let (mut in_span, mut in_span_files, mut in_span_articles) = (0usize, 0usize, 0usize);
    while let Some(found) = linker.pass(&store, now, &HashSet::new(), &|| false)? {
        passes += 1;
        let st = found.stats.clone();
        println!(
            "  pass {passes}: hours {}..{}: {} blocks, {} rows in {:.1}s read, {:.1}s in all; {} kept, {} uploads, {} late files",
            crate::xref::iso_pub(found.hours.first().copied().unwrap_or(0) * 3600),
            crate::xref::iso_pub(found.hours.last().copied().unwrap_or(0) * 3600 + 3599),
            st.blocks,
            st.rows,
            st.read_secs,
            st.secs,
            st.kept,
            st.uploads,
            st.late_files
        );
        releases += found.releases.len();
        uploads += st.uploads;
        late += st.late_files;
        blocks += st.blocks;
        rows += st.rows;
        kept += st.kept;
        read += st.read_secs;
        for r in &found.releases {
            articles += r.articles.len();
            bytes += r.size;
            let f: HashSet<&str> = r.articles.iter().filter_map(|a| a.filename.as_deref()).collect();
            files += f.len();
            let first = r.articles.iter().filter_map(|a| crate::dates::naive_timestamp(&a.date)).min();
            if first.is_some_and(|t| t >= from && t <= to) {
                in_span += 1;
                in_span_files += f.len();
                in_span_articles += r.articles.len();
            }
        }
        let groups: HashSet<String> = found.releases.iter().map(|r| r.group.clone()).collect();
        cross += groups.len();
        linker.ack(found)?;
    }
    let _ = cross;
    println!(
        "{day_name}: {passes} passes in {:.1}s ({blocks} blocks, {rows} rows, {:.1}s reading, {kept} obfuscated articles kept)",
        t.elapsed().as_secs_f64(),
        read
    );
    println!(
        "  releases {releases} ({uploads} uploads, {late} late files), {files} files, {articles} articles, {:.1} GB",
        bytes as f64 / 1e9
    );
    println!(
        "  of them starting in {}..{}: {in_span} uploads, {in_span_files} files, {in_span_articles} articles",
        crate::xref::iso_pub(from),
        crate::xref::iso_pub(to)
    );
    store.close()
}
