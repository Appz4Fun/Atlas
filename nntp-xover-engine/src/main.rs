//! `nxv-bench`: the benchmark table. With no arguments it runs the 10,000-row
//! synthetic corpus; `nxv-bench FILE...` runs each file of raw overview lines
//! (one per line, as XOVER sends them) on its own.

use std::hint::black_box;
use std::time::Instant;

use anyhow::{Context, Result};
use nntp_xover_engine::container::msgid_field_pub;
use nntp_xover_engine::corpus::{Rng, generate_realistic_xover_corpus};
use nntp_xover_engine::*;

const BLOCK_ROWS: usize = 65_536;

struct Row {
    name: &'static str,
    bytes: usize,
    ingest_mb_s: f64,
    art_ns: f64,
    msgid_ns: Option<f64>,
    stream_mb_s: f64,
}

fn mb(b: usize, secs: f64) -> f64 {
    b as f64 / 1e6 / secs
}

fn time<T>(mut f: impl FnMut() -> T) -> (T, f64) {
    let t = Instant::now();
    let v = f();
    (v, t.elapsed().as_secs_f64())
}

/// Repeats `f` until about `budget` seconds pass; seconds per call.
fn per_call(budget: f64, mut f: impl FnMut()) -> f64 {
    let mut n = 0u64;
    let t = Instant::now();
    loop {
        f();
        n += 1;
        if t.elapsed().as_secs_f64() > budget {
            break;
        }
    }
    t.elapsed().as_secs_f64() / n as f64
}

fn run(label: &str, lines: &[Vec<u8>]) -> Result<()> {
    let raw: usize = lines.iter().map(|l| l.len() + 2).sum();
    let refs: Vec<&[u8]> = lines.iter().map(|l| l.as_slice()).collect();
    let n = lines.len();
    let mut rng = Rng::new(42);
    let picks: Vec<usize> = (0..20_000).map(|_| rng.below(n as u64) as usize).collect();
    let mut rows = Vec::new();

    // ---- row by row zstd -3, without and with a 64 KB dictionary
    let (z, zt) = time(|| refs.iter().map(|l| zstd::bulk::compress(l, 3).unwrap()).collect::<Vec<_>>());
    let zsize: usize = z.iter().map(|c| c.len()).sum();
    let art = per_call(0.3, || {
        for &i in &picks[..1000] {
            black_box(zstd::bulk::decompress(&z[i], 4096).unwrap());
        }
    }) / 1000.0;
    let (_, st) = time(|| {
        for c in &z {
            black_box(zstd::bulk::decompress(c, 4096).unwrap());
        }
    });
    rows.push(Row {
        name: "Row-by-row Zstd -3",
        bytes: zsize,
        ingest_mb_s: mb(raw, zt),
        art_ns: art * 1e9,
        msgid_ns: None,
        stream_mb_s: mb(raw, st),
    });

    let sample: Vec<&[u8]> = refs.iter().step_by((n / 20_000).max(1)).copied().collect();
    let (dict, dt) = time(|| zstd::dict::from_samples(&sample, 64 * 1024).unwrap());
    let mut comp = zstd::bulk::Compressor::with_dictionary(3, &dict)?;
    let (zd, zdt) = time(|| refs.iter().map(|l| comp.compress(l).unwrap()).collect::<Vec<_>>());
    let zdsize: usize = zd.iter().map(|c| c.len()).sum::<usize>() + dict.len();
    let mut dec = zstd::bulk::Decompressor::with_dictionary(&dict)?;
    let art = per_call(0.3, || {
        for &i in &picks[..1000] {
            black_box(dec.decompress(&zd[i], 4096).unwrap());
        }
    }) / 1000.0;
    let (_, st) = time(|| {
        for c in &zd {
            black_box(dec.decompress(c, 4096).unwrap());
        }
    });
    rows.push(Row {
        name: "Row-by-row Zstd -3 + 64 KB ZDICT",
        bytes: zdsize,
        ingest_mb_s: mb(raw, dt + zdt),
        art_ns: art * 1e9,
        msgid_ns: None,
        stream_mb_s: mb(raw, st),
    });

    // whole-batch zstd, for reference: what plain compression of the same bytes gets
    let mut joined = Vec::with_capacity(raw);
    for l in &refs {
        joined.extend_from_slice(l);
        joined.extend_from_slice(b"\r\n");
    }
    for lvl in [3, 19] {
        let (c, ct) = time(|| zstd::bulk::compress(&joined, lvl).unwrap());
        let (_, dtt) = time(|| black_box(zstd::bulk::decompress(&c, raw).unwrap()));
        rows.push(Row {
            name: if lvl == 3 {
                "Whole-batch Zstd -3 (no random access)"
            } else {
                "Whole-batch Zstd -19 (no random access)"
            },
            bytes: c.len(),
            ingest_mb_s: mb(raw, ct),
            art_ns: dtt * 1e9,
            msgid_ns: None,
            stream_mb_s: mb(raw, dtt),
        });
    }

    // ---- .nxv hot
    let (blocks, bt) =
        time(|| refs.chunks(BLOCK_ROWS).map(|c| NxvBlockBuilder::build_hot_block(c).unwrap()).collect::<Vec<_>>());
    let hot: Vec<AlignedBytes> = blocks.iter().map(|b| AlignedBytes::from_slice(b)).collect();
    let views: Vec<NxvBlockView> = hot.iter().map(|b| NxvBlockView::from_mmap_slice(b).unwrap()).collect();
    let hot_size: usize = hot.iter().map(|b| b.len()).sum();
    let mut cache = RowCache::default();
    let mut out = Vec::with_capacity(4096);
    let art = per_call(0.5, || {
        for &i in &picks {
            out.clear();
            views[i / BLOCK_ROWS].reconstruct_row(i % BLOCK_ROWS, &mut cache, &mut out).unwrap();
            black_box(&out);
        }
    }) / picks.len() as f64;
    let ids: Vec<&[u8]> = picks.iter().map(|&i| msgid_field_pub(&lines[i]).unwrap()).collect();
    let msgid = per_call(0.5, || {
        for (k, m) in ids.iter().enumerate() {
            // route across every block, as a server would without an article number
            let hit =
                views.iter().enumerate().find_map(|(b, v)| v.find_row_by_msgid_with(m, &mut cache).map(|r| (b, r)));
            let (b, r) = hit.expect("message-id not found");
            debug_assert_eq!(b * BLOCK_ROWS + r, picks[k]);
            out.clear();
            views[b].reconstruct_row(r, &mut cache, &mut out).unwrap();
            black_box(&out);
        }
    }) / ids.len() as f64;
    let rt = tokio::runtime::Builder::new_current_thread().build()?;
    let mut scratch = XoverStreamScratch::new();
    let stream = per_call(1.0, || {
        let mut sink = tokio::io::sink();
        let sent = rt.block_on(stream_xover_blocks_tokio(&views, 0, u64::MAX, &mut scratch, &mut sink)).unwrap();
        assert_eq!(sent, n);
    });
    rows.push(Row {
        name: ".nxv Hot (columnar + delta + FSST + msgid packer + Xor8/CSR)",
        bytes: hot_size,
        ingest_mb_s: mb(raw, bt),
        art_ns: art * 1e9,
        msgid_ns: Some(msgid * 1e9),
        stream_mb_s: mb(raw, stream),
    });

    // ---- .nxv cold
    let (cold, ct) = time(|| blocks.iter().map(|b| compress_block_to_cold(b, 9).unwrap()).collect::<Vec<_>>());
    let cold_al: Vec<AlignedBytes> = cold.iter().map(|b| AlignedBytes::from_slice(b)).collect();
    let cold_size: usize = cold.iter().map(|b| b.len()).sum();
    let cold_views: Vec<NxvBlockView> = cold_al.iter().map(|b| NxvBlockView::from_mmap_slice(b).unwrap()).collect();
    // cold random access: route by header, decompress the block, read the row
    let art_cold = per_call(0.5, || {
        for &i in &picks[..50] {
            let hotb = decompress_cold_block(&cold_al[i / BLOCK_ROWS]).unwrap();
            let v = NxvBlockView::from_mmap_slice(&hotb).unwrap();
            out.clear();
            v.reconstruct_row(i % BLOCK_ROWS, &mut cache, &mut out).unwrap();
            black_box(&out);
        }
    }) / 50.0;
    let msgid_cold = per_call(0.5, || {
        for m in &ids[..50] {
            // every block whose filter says maybe, until one really has it (filters give false positives)
            let found = cold_views.iter().enumerate().filter(|(_, v)| v.may_contain_msgid(m)).find_map(|(b, _)| {
                let hotb = decompress_cold_block(&cold_al[b]).unwrap();
                let v = NxvBlockView::from_mmap_slice(&hotb).unwrap();
                v.find_row_by_msgid_with(m, &mut cache)
            });
            black_box(found.expect("message-id not found"));
        }
    }) / 50.0;
    let stream_cold = per_call(1.0, || {
        let hb: Vec<AlignedBytes> = cold_al.iter().map(|c| decompress_cold_block(c).unwrap()).collect();
        let vs: Vec<NxvBlockView> = hb.iter().map(|b| NxvBlockView::from_mmap_slice(b).unwrap()).collect();
        let mut sink = tokio::io::sink();
        rt.block_on(stream_xover_blocks_tokio(&vs, 0, u64::MAX, &mut scratch, &mut sink)).unwrap();
    });
    rows.push(Row {
        name: ".nxv Cold (hot + Zstd -9, routing data uncompressed)",
        bytes: cold_size,
        ingest_mb_s: mb(raw, bt + ct),
        art_ns: art_cold * 1e9,
        msgid_ns: Some(msgid_cold * 1e9),
        stream_mb_s: mb(raw, stream_cold),
    });

    // ---- table
    println!("\n### {label}: {n} rows, {:.1} KB raw XOVER\n", raw as f64 / 1024.0);
    println!(
        "| Format | Size (KB) | Ratio | Ingest (MB/s) | Random ArtNum (ns/row) | OVER <msgid> (ns/lookup) | Stream XOVER (MB/s) | Stream (Gbps) |"
    );
    println!("|---|---:|---:|---:|---:|---:|---:|---:|");
    for r in &rows {
        println!(
            "| {} | {:.1} | {:.2}x | {:.0} | {:.0} | {} | {:.0} | {:.2} |",
            r.name,
            r.bytes as f64 / 1024.0,
            raw as f64 / r.bytes as f64,
            r.ingest_mb_s,
            r.art_ns,
            r.msgid_ns.map_or("n/a".into(), |v| format!("{v:.0}")),
            r.stream_mb_s,
            r.stream_mb_s * 8.0 / 1000.0
        );
    }
    let literal: usize = views.iter().map(|v| v.literal_rows()).sum();
    let mut why: std::collections::BTreeMap<&str, usize> = Default::default();
    let mut example: std::collections::BTreeMap<&str, String> = Default::default();
    for l in lines {
        if let Some(r) = nntp_xover_engine::container::literal_reason(l) {
            *why.entry(r).or_default() += 1;
            example.entry(r).or_insert_with(|| String::from_utf8_lossy(&l[..l.len().min(300)]).into_owned());
        }
    }
    for (r, c) in &why {
        println!("literal because {r}: {c}, e.g. {:?}", example[r]);
    }
    println!(
        "\nRows kept literal: {literal} ({:.2}%). Template cache hits while streaming: {:.1}%.",
        100.0 * literal as f64 / n as f64,
        100.0 * scratch.cache.tpl_hits as f64 / (scratch.cache.tpl_hits + scratch.cache.tpl_misses).max(1) as f64
    );
    let mut by = [0u64; 16];
    for v in &views {
        for (i, (_, b)) in v.section_sizes().iter().enumerate() {
            by[i] += *b as u64;
        }
    }
    let names = [
        "FsstTables",
        "SkipCheckpoints",
        "MsgIdXor8",
        "MsgIdBucketIndex",
        "SubjTplIds",
        "SubjPadMasks",
        "XrefSkelIds",
        "AuthorIds",
        "DomainIds",
        "MsgIdPfxIds",
        "MsgIdTailOffsets",
        "StringPoolOffsets",
        "StringPoolData",
        "MsgIdTailData",
        "NumericVarints",
        "ReferencesData",
    ];
    let parts: Vec<String> =
        names.iter().zip(by).map(|(nm, b)| format!("{nm} {:.1}%", 100.0 * b as f64 / hot_size as f64)).collect();
    println!(
        "Hot block bytes by section: {}. Dictionary entries (subject, xref, author, domain, prefix, literal): {:?}.",
        parts.join(", "),
        views[0].dict_sizes()
    );
    Ok(())
}

fn main() -> Result<()> {
    let files: Vec<String> = std::env::args().skip(1).collect();
    if files.is_empty() {
        run("Synthetic corpus", &generate_realistic_xover_corpus(10_000))?;
    }
    for f in files {
        let data = std::fs::read(&f).with_context(|| format!("reading {f}"))?;
        let lines: Vec<Vec<u8>> = data
            .split(|&b| b == b'\n')
            .map(|l| l.strip_suffix(b"\r").unwrap_or(l).to_vec())
            .filter(|l| !l.is_empty())
            .collect();
        let name = std::path::Path::new(&f).file_name().unwrap().to_string_lossy().into_owned();
        run(Box::leak(name.into_boxed_str()), &lines)?;
    }
    Ok(())
}
