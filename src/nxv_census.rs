//! `atlas --header-window census [BLOCKS]`: a read-only survey of the header window, for sizing the day-segment
//! store's block format. It walks every file for totals and decodes a spread sample of sealed blocks (and some open
//! tails) row by row: how subjects would split over the subject modes, how many posters a block holds, how far dates
//! stray from their block's median, and where a block's bytes go. Nothing is written.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use nntp_xover_engine::container::{AlignedBytes, NxvBlockView};
use nntp_xover_engine::date::parse_rfc2822_to_epoch;
use nntp_xover_engine::msgid_codec::split_msgid;
use nntp_xover_engine::streamer::{NxvRangeCursor, XoverStreamScratch};
use nntp_xover_engine::subject_codec::{SLOT_BRACKET_FRAC, SLOT_FILE_PART, SLOT_PAREN_FRAC, extract_subject_template};

const DAY: i64 = 86_400;

/// Totals of one kind of file.
#[derive(Default, Clone, Copy)]
pub(crate) struct Files {
    count: u64,
    bytes: u64,
}

/// What the sample's rows and blocks add up to.
#[derive(Default)]
struct Tally {
    blocks: u64,
    tails: u64,
    rows: u64,
    /// bytes of the overview lines themselves (no CR/LF)
    raw_bytes: u64,
    /// bytes of the sampled blocks on disk
    block_bytes: u64,
    block_rows: u64,
    literal_rows: u64,
    // subject modes
    same_local: u64,
    same_full: u64,
    template: u64,
    token: u64,
    token_fallback: u64,
    plain: u64,
    control_bytes: u64,
    // posters
    max_posters_in_block: u64,
    poster_ratio_sum: f64,
    blocks_over_half_unique: u64,
    // dates
    no_date: u64,
    outside_median_day: u64,
    future: u64,
    // section bytes, by section name
    sections: HashMap<&'static str, u64>,
    // by server: rows, raw bytes, block bytes
    servers: HashMap<String, (u64, u64, u64)>,
    rows_per_block: Vec<u64>,
}

fn is_token_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'.' || b == b'-' || b == b'_'
}

fn in_b64_alphabet(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'.' || b == b'-'
}

/// The subject mode a row would get: 0 same as local, 1 same as full, 2 template, 3 token (4 when it needs the raw
/// fallback), 5 plain.
fn subject_mode(subject: &[u8], msgid: &[u8], tpl: &mut Vec<u8>) -> u8 {
    if let Some((local, _)) = split_msgid(msgid)
        && !local.is_empty()
        && subject == local
    {
        return 0;
    }
    if !msgid.is_empty() {
        let bare = msgid.strip_prefix(b"<").and_then(|s| s.strip_suffix(b">")).unwrap_or(msgid);
        if subject == msgid || subject == bare {
            return 1;
        }
    }
    let counters = extract_subject_template(subject, tpl)
        .is_some_and(|_| tpl.iter().any(|&b| matches!(b, SLOT_BRACKET_FRAC | SLOT_PAREN_FRAC | SLOT_FILE_PART)));
    if counters {
        return 2;
    }
    if subject.len() >= 12 && subject.iter().all(|&b| is_token_char(b)) {
        return if subject.iter().all(|&b| in_b64_alphabet(b)) { 3 } else { 4 };
    }
    5
}

/// One block's or tail's lines, tallied.
fn tally_lines(server: Option<&str>, lines: &[Vec<u8>], t: &mut Tally, now: i64) {
    let mut posters: HashSet<&[u8]> = HashSet::new();
    let mut dates = Vec::with_capacity(lines.len());
    let mut tpl = Vec::new();
    for l in lines {
        let f: Vec<&[u8]> = l.splitn(9, |&b| b == b'\t').collect();
        t.rows += 1;
        t.raw_bytes += l.len() as u64;
        let subject = f.get(1).copied().unwrap_or_default();
        let poster = f.get(2).copied().unwrap_or_default();
        let msgid = f.get(4).copied().unwrap_or_default();
        if subject.iter().any(|&b| (1..=3).contains(&b)) {
            t.control_bytes += 1;
        }
        match subject_mode(subject, msgid, &mut tpl) {
            0 => t.same_local += 1,
            1 => t.same_full += 1,
            2 => t.template += 1,
            3 => t.token += 1,
            4 => {
                t.token += 1;
                t.token_fallback += 1;
            }
            _ => t.plain += 1,
        }
        posters.insert(poster);
        match f.get(3).and_then(|d| parse_rfc2822_to_epoch(d)) {
            Some(ts) => {
                dates.push(ts);
                if ts > now + 3600 {
                    t.future += 1;
                }
            }
            None => t.no_date += 1,
        }
    }
    if !dates.is_empty() {
        let mut sorted = dates.clone();
        sorted.sort_unstable();
        let median = sorted[sorted.len() / 2];
        t.outside_median_day += dates.iter().filter(|&&d| (d - median).abs() > DAY).count() as u64;
    }
    let n = lines.len().max(1) as f64;
    let unique = posters.len() as u64;
    t.max_posters_in_block = t.max_posters_in_block.max(unique);
    t.poster_ratio_sum += unique as f64 / n;
    if unique as f64 > n / 2.0 {
        t.blocks_over_half_unique += 1;
    }
    // by server, sealed blocks only, soo stored bytes per row compare like with like
    if let Some(server) = server {
        let s = t.servers.entry(server.to_string()).or_default();
        s.0 += lines.len() as u64;
        s.1 += lines.iter().map(|l| l.len() as u64).sum::<u64>();
    }
}

/// The rows of a sealed block, and its section sizes and literal count.
pub(crate) fn read_block(path: &Path) -> anyhow::Result<(Vec<Vec<u8>>, Vec<(&'static str, u32)>, usize, u64)> {
    let raw = std::fs::read(path)?;
    let bytes = AlignedBytes::from_slice(&raw);
    let view = NxvBlockView::from_mmap_slice(&bytes)?;
    let sections: Vec<(&'static str, u32)> =
        view.section_sizes().iter().map(|(id, n)| (section_name(*id as u8), *n)).collect();
    let literal = view.literal_rows();
    let mut cur = NxvRangeCursor::new_for_art_range(view, 0, u64::MAX)?;
    let mut scratch = XoverStreamScratch::new();
    let mut rows = Vec::new();
    loop {
        scratch.wire_buf.clear();
        if !cur.format_next_row(&mut scratch) {
            break;
        }
        let n = scratch.wire_buf.len().saturating_sub(2);
        rows.push(scratch.wire_buf[..n].to_vec());
    }
    Ok((rows, sections, literal, raw.len() as u64))
}

fn section_name(id: u8) -> &'static str {
    [
        "fsst tables",
        "checkpoints",
        "msgid filter",
        "msgid index",
        "subject template ids",
        "subject pad masks",
        "xref skeleton ids",
        "author ids",
        "domain ids",
        "msgid prefix ids",
        "msgid tail offsets",
        "string pool offsets",
        "string pool",
        "msgid tails",
        "numbers",
        "references",
    ]
    .get(id as usize)
    .copied()
    .unwrap_or("?")
}

/// Length-prefixed lines of a tail file (a torn last line is left out).
pub(crate) fn read_tail(path: &Path) -> anyhow::Result<Vec<Vec<u8>>> {
    let b = std::fs::read(path)?;
    let mut out = Vec::new();
    let mut at = 0;
    while at + 4 <= b.len() {
        let n = u32::from_le_bytes(b[at..at + 4].try_into().expect("4 bytes")) as usize;
        if at + 4 + n > b.len() {
            break;
        }
        out.push(b[at + 4..at + 4 + n].to_vec());
        at += 4 + n;
    }
    Ok(out)
}

/// Every file under the window: (server dir name, path, size), split into blocks and tails.
pub(crate) fn walk(root: &Path) -> anyhow::Result<(Vec<(String, PathBuf, u64)>, Vec<(String, PathBuf, u64)>, Files, Files)> {
    let (mut blocks, mut tails) = (Vec::new(), Vec::new());
    let (mut idx, mut other) = (Files::default(), Files::default());
    for server in std::fs::read_dir(root)?.flatten() {
        if !server.path().is_dir() {
            continue;
        }
        let sname = server.file_name().to_string_lossy().into_owned();
        for group in std::fs::read_dir(server.path())?.flatten() {
            if !group.path().is_dir() {
                continue;
            }
            for f in std::fs::read_dir(group.path())?.flatten() {
                let p = f.path();
                let len = f.metadata().map(|m| m.len()).unwrap_or(0);
                match p.extension().and_then(|e| e.to_str()) {
                    Some("nxv") => blocks.push((sname.clone(), p, len)),
                    Some("tail") => tails.push((sname.clone(), p, len)),
                    Some("idx") => {
                        idx.count += 1;
                        idx.bytes += len;
                    }
                    _ => {
                        other.count += 1;
                        other.bytes += len;
                    }
                }
            }
        }
    }
    blocks.sort_by(|a, b| a.1.cmp(&b.1));
    tails.sort_by(|a, b| a.1.cmp(&b.1));
    Ok((blocks, tails, idx, other))
}

/// Every `step`-th item, soo the sample spreads over servers, groups and hours.
fn spread<T: Clone>(all: &[T], want: usize) -> Vec<T> {
    if want == 0 || all.is_empty() {
        return Vec::new();
    }
    let step = (all.len() / want).max(1);
    all.iter().step_by(step).take(want).cloned().collect()
}

fn pct(a: u64, b: u64) -> String {
    if b == 0 { "-".into() } else { format!("{:.2}%", a as f64 * 100.0 / b as f64) }
}

fn gb(b: u64) -> String {
    format!("{:.2} GB", b as f64 / 1e9)
}

/// Runs the census over `root`, sampling `want_blocks` sealed blocks and a tenth as many tails.
pub fn run(root: &Path, want_blocks: usize) -> anyhow::Result<()> {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
    let (blocks, tails, idx, other) = walk(root)?;
    let block_total = Files { count: blocks.len() as u64, bytes: blocks.iter().map(|b| b.2).sum() };
    let tail_total = Files { count: tails.len() as u64, bytes: tails.iter().map(|b| b.2).sum() };
    let sample_blocks = spread(&blocks, want_blocks);
    let sample_tails = spread(&tails, (want_blocks / 10).max(1));
    let tally = Mutex::new(Tally::default());
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).min(12);
    let work: Mutex<Vec<(bool, String, PathBuf)>> = Mutex::new(
        sample_blocks
            .iter()
            .map(|(s, p, _)| (true, s.clone(), p.clone()))
            .chain(sample_tails.iter().map(|(s, p, _)| (false, s.clone(), p.clone())))
            .collect(),
    );
    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| {
                loop {
                    let Some((is_block, server, path)) = work.lock().expect("work").pop() else { break };
                    let mut local = Tally::default();
                    if is_block {
                        match read_block(&path) {
                            Ok((rows, sections, literal, size)) => {
                                tally_lines(Some(&server), &rows, &mut local, now);
                                local.blocks = 1;
                                local.block_rows = rows.len() as u64;
                                local.block_bytes = size;
                                local.literal_rows = literal as u64;
                                for (name, n) in sections {
                                    *local.sections.entry(name).or_default() += n as u64;
                                }
                                local.servers.entry(server.clone()).or_default().2 += size;
                                local.rows_per_block.push(rows.len() as u64);
                            }
                            Err(e) => eprintln!("census: {} won't read: {e:#}", path.display()),
                        }
                    } else {
                        match read_tail(&path) {
                            Ok(rows) => {
                                tally_lines(None, &rows, &mut local, now);
                                local.tails = 1;
                            }
                            Err(e) => eprintln!("census: {} won't read: {e:#}", path.display()),
                        }
                    }
                    let mut t = tally.lock().expect("tally");
                    merge(&mut t, local);
                }
            });
        }
    });
    let t = tally.into_inner().expect("tally");
    print_report(root, block_total, tail_total, idx, other, &t);
    Ok(())
}

fn merge(t: &mut Tally, l: Tally) {
    t.blocks += l.blocks;
    t.tails += l.tails;
    t.rows += l.rows;
    t.raw_bytes += l.raw_bytes;
    t.block_bytes += l.block_bytes;
    t.block_rows += l.block_rows;
    t.literal_rows += l.literal_rows;
    t.same_local += l.same_local;
    t.same_full += l.same_full;
    t.template += l.template;
    t.token += l.token;
    t.token_fallback += l.token_fallback;
    t.plain += l.plain;
    t.control_bytes += l.control_bytes;
    t.max_posters_in_block = t.max_posters_in_block.max(l.max_posters_in_block);
    t.poster_ratio_sum += l.poster_ratio_sum;
    t.blocks_over_half_unique += l.blocks_over_half_unique;
    t.no_date += l.no_date;
    t.outside_median_day += l.outside_median_day;
    t.future += l.future;
    for (k, v) in l.sections {
        *t.sections.entry(k).or_default() += v;
    }
    for (k, v) in l.servers {
        let s = t.servers.entry(k).or_default();
        s.0 += v.0;
        s.1 += v.1;
        s.2 += v.2;
    }
    t.rows_per_block.extend(l.rows_per_block);
}

fn print_report(root: &Path, blocks: Files, tails: Files, idx: Files, other: Files, t: &Tally) {
    println!("header window census: {}", root.display());
    println!(
        "files: {} blocks ({}), {} tails ({}), {} indexes ({}), {} other ({})",
        blocks.count,
        gb(blocks.bytes),
        tails.count,
        gb(tails.bytes),
        idx.count,
        gb(idx.bytes),
        other.count,
        gb(other.bytes)
    );
    println!("sample: {} blocks, {} tails, {} rows", t.blocks, t.tails, t.rows);
    let mut rpb = t.rows_per_block.clone();
    rpb.sort_unstable();
    if !rpb.is_empty() {
        println!(
            "rows per block: median {}, p10 {}, p90 {}, max {} ({} full at 65,536)",
            rpb[rpb.len() / 2],
            rpb[rpb.len() / 10],
            rpb[rpb.len() * 9 / 10],
            rpb[rpb.len() - 1],
            rpb.iter().filter(|&&r| r >= 65_536).count()
        );
    }
    if t.block_rows > 0 {
        println!(
            "compression (sampled blocks): {:.1} raw bytes/row, {:.1} stored bytes/row, {:.1}x; literal rows {}",
            t.raw_bytes as f64 / t.rows as f64,
            t.block_bytes as f64 / t.block_rows as f64,
            t.raw_bytes as f64 / t.block_bytes.max(1) as f64 * (t.block_rows as f64 / t.rows.max(1) as f64),
            pct(t.literal_rows, t.block_rows)
        );
    }
    println!("subject modes:");
    println!("  same as message-id local part  {}", pct(t.same_local, t.rows));
    println!("  same as full message-id        {}", pct(t.same_full, t.rows));
    println!("  template with counters         {}", pct(t.template, t.rows));
    println!(
        "  random token (>=12, no spaces) {} (of which needing raw fallback: {})",
        pct(t.token, t.rows),
        pct(t.token_fallback, t.token)
    );
    println!("  plain text, no counters        {}", pct(t.plain, t.rows));
    println!("  subjects with bytes 0x01-0x03  {}", t.control_bytes);
    println!(
        "posters: max {} unique in one block; mean unique/rows {:.3}; {} of {} blocks over half unique",
        t.max_posters_in_block,
        t.poster_ratio_sum / (t.blocks + t.tails).max(1) as f64,
        t.blocks_over_half_unique,
        t.blocks + t.tails
    );
    println!(
        "dates: {} unparseable, {} more than a day from their block's median, {} over an hour in the future",
        pct(t.no_date, t.rows),
        pct(t.outside_median_day, t.rows),
        pct(t.future, t.rows)
    );
    let mut secs: Vec<_> = t.sections.iter().collect();
    secs.sort_by(|a, b| b.1.cmp(a.1));
    let total: u64 = secs.iter().map(|(_, n)| **n).sum();
    println!("where block bytes go:");
    for (name, n) in secs.iter().take(10) {
        println!("  {:<22} {:>7}  {:.1} B/row", name, pct(**n, total), **n as f64 / t.block_rows.max(1) as f64);
    }
    println!("by server (rows in sampled blocks, raw bytes/row, stored bytes/row):");
    let mut servers: Vec<_> = t.servers.iter().collect();
    servers.sort_by(|a, b| b.1.0.cmp(&a.1.0));
    for (s, (rows, raw, stored)) in servers {
        println!(
            "  {:<28} {:>11} rows  {:>6.1}  {:>6.1}",
            s,
            rows,
            *raw as f64 / (*rows).max(1) as f64,
            *stored as f64 / (*rows).max(1) as f64
        );
    }
}
