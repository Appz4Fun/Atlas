//! `cargo run --release --example nxv_recompress -- [ROOT] [BLOCKS]`: reads a spread sample of `.nxv` blocks under
//! ROOT (`<server>/<group>/<hour>-<n>.nxv`, default `~/.atlas/header-window`, 300 blocks), decodes every row,
//! builds each block again with the current writer, checks every row of the new block comes back byte for byte (by
//! the cursor, by row, by article number and by message-id), and prints bytes per row before and after, overall,
//! by section and by server. Read only: nothing is written anywhere.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result, ensure};
use nntp_xover_engine::container::{AlignedBytes, NxvBlockBuilder, NxvBlockView, msgid_field_pub};
use nntp_xover_engine::corpus::Rng;
use nntp_xover_engine::*;

const SECTIONS: [&str; 16] = [
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
];
const DICTS: [&str; 6] = ["subject", "xref", "author", "domain", "prefix", "literal"];

#[derive(Default)]
struct Tally {
    blocks: u64,
    rows: u64,
    raw: u64,
    old: u64,
    new: u64,
    old_sec: [u64; 16],
    new_sec: [u64; 16],
    old_dict: [(u64, u64); 6],
    new_dict: [(u64, u64); 6],
    token_rows: u64,
    servers: BTreeMap<String, (u64, u64, u64, u64)>,
    old_versions: BTreeMap<u16, u64>,
    new_versions: BTreeMap<u16, u64>,
    failures: Vec<String>,
}

fn rows_of(view: NxvBlockView) -> Result<Vec<Vec<u8>>> {
    let mut cur = NxvRangeCursor::new_for_art_range(view, 0, u64::MAX)?;
    let mut scratch = XoverStreamScratch::new();
    let mut rows = Vec::with_capacity(view.row_count());
    loop {
        scratch.wire_buf.clear();
        if !cur.format_next_row(&mut scratch) {
            break;
        }
        rows.push(scratch.wire_buf.strip_suffix(b"\r\n").context("row without CRLF")?.to_vec());
    }
    ensure!(rows.len() == view.row_count(), "cursor gave {} of {} rows", rows.len(), view.row_count());
    Ok(rows)
}

fn art_of(l: &[u8]) -> u64 {
    std::str::from_utf8(l.split(|&b| b == b'\t').next().unwrap_or_default())
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

/// Every row of `new` against `rows`: the cursor over all of them, then by row, by article number and by message-id
/// for a sample.
fn check(new: NxvBlockView, rows: &[Vec<u8>], seed: u64) -> Result<()> {
    let back = rows_of(new)?;
    for (i, (a, b)) in rows.iter().zip(&back).enumerate() {
        ensure!(a == b, "row {i} differs:\n  was {}\n  now {}", String::from_utf8_lossy(a), String::from_utf8_lossy(b));
    }
    let mut r = Rng::new(seed | 1);
    let mut cache = RowCache::default();
    let mut out = Vec::new();
    let picks = rows.len().min(400);
    for k in 0..picks {
        let i = if k < 4 {
            [0, rows.len() - 1, rows.len() / 2, 127.min(rows.len() - 1)][k]
        } else {
            r.below(rows.len() as u64) as usize
        };
        out.clear();
        new.reconstruct_row(i, &mut cache, &mut out)?;
        ensure!(out == rows[i], "random row {i} differs");
        let a = art_of(&rows[i]);
        let first = rows.partition_point(|l| art_of(l) < a);
        ensure!(new.find_row_by_art_num(a) == Some(first), "article {a} not found at row {first}");
        if let Some(m) = msgid_field_pub(&rows[i]) {
            let got = new.find_row_by_msgid_with(m, &mut cache).context("message-id not found")?;
            ensure!(msgid_field_pub(&rows[got]) == Some(m), "message-id found at the wrong row");
        }
    }
    Ok(())
}

fn one(path: &Path, t: &mut Tally) -> Result<()> {
    let file = std::fs::read(path)?;
    let al = AlignedBytes::from_slice(&file);
    let first = NxvBlockView::from_mmap_slice(&al)?;
    let hot = if first.is_cold() { decompress_cold_block(&al)? } else { al.clone() };
    let view = NxvBlockView::from_mmap_slice(&hot)?;
    let rows = rows_of(view)?;
    let refs: Vec<&[u8]> = rows.iter().map(|l| l.as_slice()).collect();
    let built = NxvBlockBuilder::build_hot_block_meta(&refs, view.store_meta())?;
    let nb = AlignedBytes::from_slice(&built);
    let new = NxvBlockView::from_mmap_slice(&nb)?;
    ensure!(new.store_meta() == view.store_meta(), "store fields changed");
    ensure!(
        new.poster_count() == view.poster_count(),
        "poster count {} was {}",
        new.poster_count(),
        view.poster_count()
    );
    ensure!(new.verify_body(), "new block fails its body hash");
    check(new, &rows, file.len() as u64)?;
    let cold = compress_block_to_cold(&built, 3)?;
    ensure!(decompress_cold_block(&cold)?[..] == built[..], "cold round trip differs");
    t.blocks += 1;
    t.rows += rows.len() as u64;
    t.raw += rows.iter().map(|l| l.len() as u64).sum::<u64>();
    t.old += hot.len() as u64;
    t.new += built.len() as u64;
    for (i, (_, n)) in view.section_sizes().iter().enumerate() {
        t.old_sec[i] += *n as u64;
    }
    for (i, (_, n)) in new.section_sizes().iter().enumerate() {
        t.new_sec[i] += *n as u64;
    }
    for (i, (n, b, _)) in view.dict_stats().iter().enumerate() {
        t.old_dict[i].0 += *n as u64;
        t.old_dict[i].1 += *b as u64;
    }
    for (i, (n, b, _)) in new.dict_stats().iter().enumerate() {
        t.new_dict[i].0 += *n as u64;
        t.new_dict[i].1 += *b as u64;
    }
    t.token_rows += new.token_subject_rows() as u64;
    *t.old_versions.entry(view.header.version).or_default() += 1;
    *t.new_versions.entry(new.header.version).or_default() += 1;
    Ok(())
}

fn walk(root: &Path) -> Result<Vec<(String, PathBuf)>> {
    let mut out = Vec::new();
    for server in std::fs::read_dir(root).with_context(|| format!("reading {}", root.display()))?.flatten() {
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
                if p.extension().and_then(|e| e.to_str()) == Some("nxv") {
                    out.push((sname.clone(), p));
                }
            }
        }
    }
    out.sort();
    Ok(out)
}

fn per_row(b: u64, rows: u64) -> f64 {
    b as f64 / rows.max(1) as f64
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let root = match args.first() {
        Some(r) => PathBuf::from(r),
        None => PathBuf::from(std::env::var("HOME")?).join(".atlas/header-window"),
    };
    let want: usize = args.get(1).map(|s| s.parse()).transpose()?.unwrap_or(300);
    let all = walk(&root)?;
    let step = (all.len() / want.max(1)).max(1);
    let sample: Vec<(String, PathBuf)> = all.iter().step_by(step).take(want).cloned().collect();
    let work = Mutex::new(sample);
    let total = Mutex::new(Tally::default());
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).min(10);
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                loop {
                    let Some((server, path)) = work.lock().unwrap().pop() else { break };
                    let mut t = Tally::default();
                    let res = one(&path, &mut t);
                    let mut g = total.lock().unwrap();
                    match res {
                        Ok(()) => {
                            let e = g.servers.entry(server).or_default();
                            e.0 += t.rows;
                            e.1 += t.raw;
                            e.2 += t.old;
                            e.3 += t.new;
                            g.blocks += t.blocks;
                            g.rows += t.rows;
                            g.raw += t.raw;
                            g.old += t.old;
                            g.new += t.new;
                            for i in 0..16 {
                                g.old_sec[i] += t.old_sec[i];
                                g.new_sec[i] += t.new_sec[i];
                            }
                            for i in 0..6 {
                                g.old_dict[i].0 += t.old_dict[i].0;
                                g.old_dict[i].1 += t.old_dict[i].1;
                                g.new_dict[i].0 += t.new_dict[i].0;
                                g.new_dict[i].1 += t.new_dict[i].1;
                            }
                            g.token_rows += t.token_rows;
                            for (k, v) in t.old_versions {
                                *g.old_versions.entry(k).or_default() += v;
                            }
                            for (k, v) in t.new_versions {
                                *g.new_versions.entry(k).or_default() += v;
                            }
                        }
                        // a file the window rotated away while sampling is not a failure
                        Err(_) if !path.exists() => {}
                        Err(e) => g.failures.push(format!("{}: {e:#}", path.display())),
                    }
                }
            });
        }
    });
    let t = total.into_inner().unwrap();
    println!(
        "{} blocks, {} rows, {:.1} raw bytes/row; block versions before {:?}, after {:?}",
        t.blocks,
        t.rows,
        per_row(t.raw, t.rows),
        t.old_versions,
        t.new_versions
    );
    println!(
        "every row round-trips: {} ({} failures)",
        if t.failures.is_empty() { "yes" } else { "NO" },
        t.failures.len()
    );
    for f in t.failures.iter().take(10) {
        println!("  FAIL {f}");
    }
    println!(
        "bytes/row: before {:.2} ({:.2}x), after {:.2} ({:.2}x), {:+.2} ({:+.1}%)",
        per_row(t.old, t.rows),
        t.raw as f64 / t.old.max(1) as f64,
        per_row(t.new, t.rows),
        t.raw as f64 / t.new.max(1) as f64,
        per_row(t.new, t.rows) - per_row(t.old, t.rows),
        100.0 * (t.new as f64 - t.old as f64) / t.old.max(1) as f64
    );
    println!("\n| section | before B/row | after B/row | change |");
    println!("|---|---:|---:|---:|");
    for (name, (old, new)) in SECTIONS.iter().zip(t.old_sec.iter().zip(&t.new_sec)) {
        let (a, b) = (per_row(*old, t.rows), per_row(*new, t.rows));
        println!("| {name} | {a:.2} | {b:.2} | {:+.2} |", b - a);
    }
    let pad = |sec: &[u64; 16], all: u64| all.saturating_sub(sec.iter().sum::<u64>());
    let (a, b) = (per_row(pad(&t.old_sec, t.old), t.rows), per_row(pad(&t.new_sec, t.new), t.rows));
    println!("| header, table, alignment | {a:.2} | {b:.2} | {:+.2} |", b - a);
    println!("\n| dictionary | entries/row before | after | pool B/row before | after |");
    println!("|---|---:|---:|---:|---:|");
    for (name, (old, new)) in DICTS.iter().zip(t.old_dict.iter().zip(&t.new_dict)) {
        println!(
            "| {name} | {:.3} | {:.3} | {:.2} | {:.2} |",
            per_row(old.0, t.rows),
            per_row(new.0, t.rows),
            per_row(old.1, t.rows),
            per_row(new.1, t.rows)
        );
    }
    println!(
        "rows with the subject kept per row (token subjects): {:.1}%",
        100.0 * t.token_rows as f64 / t.rows.max(1) as f64
    );
    println!("\n| server | rows | raw B/row | before B/row | after B/row |");
    println!("|---|---:|---:|---:|---:|");
    for (s, (rows, raw, old, new)) in &t.servers {
        println!(
            "| {s} | {rows} | {:.1} | {:.2} | {:.2} |",
            per_row(*raw, *rows),
            per_row(*old, *rows),
            per_row(*new, *rows)
        );
    }
    Ok(())
}
