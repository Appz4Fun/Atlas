//! Atlas's own PAR2 code (`atlas::par2`) against par-rs (`atlas::parity`), call for call, on the
//! same inputs. Each row checks the two agree, then prints the best of `BENCH_RUNS` timings.
//! `atlas::gf2` is par-rs itself, so its CRC rows are gone; only par-rs's crc32 rate is printed.
//!
//!     cargo run --release --locked --example bench_parity
//!
//! Knobs (env): BENCH_RUNS (best of, 5), BENCH_MIB (CRC buffer, 64).

use std::hint::black_box;
use std::time::{Duration, Instant};

use atlas::parity;
use par_rs::create::{SetOptions, VolumeScheme, build_set};

fn env(name: &str, default: usize) -> usize {
    std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

fn data(n: usize, seed: u32) -> Vec<u8> {
    (0..n as u32).map(|i| ((i ^ seed.wrapping_mul(0x9E37_79B9)).wrapping_mul(2_654_435_761) >> 13) as u8).collect()
}

fn best<T>(runs: usize, mut f: impl FnMut() -> T) -> Duration {
    (0..runs.max(1))
        .map(|_| {
            let t = Instant::now();
            black_box(f());
            t.elapsed()
        })
        .min()
        .unwrap()
}

fn row(what: &str, unit: &str, per: f64, atlas: Duration, par_rs: Duration) {
    let rate = |d: Duration| per / d.as_secs_f64();
    println!(
        "{what:<44} atlas {:>10.1} {unit:<6} par-rs {:>10.1} {unit:<6} {:>7.1}x",
        rate(atlas),
        rate(par_rs),
        atlas.as_secs_f64() / par_rs.as_secs_f64()
    );
}

/// par-rs's inventory with no packet MD5 checked, as Atlas reads a set.
fn unverified(bytes: &[u8]) -> usize {
    par_rs::par2::inventory_from(&par_rs::inspect_par2_with(bytes, par_rs::VerifyMode::None)).files.len()
}

fn main() {
    let runs = env("BENCH_RUNS", 5);
    let mib = env("BENCH_MIB", 64);
    println!("best of {runs}; last column = how many times faster par-rs is\n");

    // CRC-32 of article bodies (postprocess keeps one per BODY it reads). `atlas::gf2` is par-rs
    // now, so this row has nothing to compare against: it prints par-rs's own rate. The old
    // bit-at-a-time loop ran at 484 MiB/s on the same machine (docs/PARITY_REQUESTS.md).
    let buf = data(mib << 20, 1);
    let p = best(runs, || parity::crc32(&buf));
    println!("{:<44} par-rs {:>10.1} MiB/s", format!("crc32 of {mib} MiB"), mib as f64 / p.as_secs_f64());

    // Reading a PAR2 index: a big release (200 RAR volumes) and a small one.
    let opts = |slice_size| SetOptions {
        slice_size,
        recovery_blocks: 8,
        volumes: VolumeScheme::Exponential,
        kernel: par_rs::Kernel::detect(),
        ..SetOptions::default()
    };
    let vols: Vec<(String, Vec<u8>)> =
        (0..200).map(|i| (format!("Some.Movie.2026.part{:03}.rar", i + 1), data(400_000, 10 + i))).collect();
    let refs: Vec<(&str, &[u8])> = vols.iter().map(|(n, d)| (n.as_str(), d.as_slice())).collect();
    let big = build_set("Some.Movie.2026", &refs, &opts(4096)).unwrap();
    let small_files = [("Small.Show.S01E01.mkv", data(3_000_000, 7)), ("Small.Show.S01E01.nfo", data(2_000, 8))];
    let small_refs: Vec<(&str, &[u8])> = small_files.iter().map(|(n, d)| (*n, d.as_slice())).collect();
    let small = build_set("Small.Show.S01E01", &small_refs, &opts(65_536)).unwrap();
    for (label, set) in [("200 files, 20k slices", &big), ("2 files", &small)] {
        let (ai, pi) = (atlas::par2::inventory(&set.index), parity::inventory(&set.index));
        let mut an: Vec<(&str, u64)> = ai.files.iter().map(|f| (f.name.as_str(), f.size)).collect();
        let mut pn: Vec<(&str, u64)> = pi.files.iter().map(|f| (f.name.as_str(), f.size)).collect();
        an.sort();
        pn.sort();
        assert_eq!(an, pn, "{label}: both read the same files");
        for f in &ai.files {
            if let Some(c) = f.crc32 {
                assert_eq!(
                    Some(c),
                    pi.files.iter().find(|g| g.file_id == f.file_id).and_then(|g| g.crc32),
                    "{label}: {}",
                    f.name
                );
            }
        }
        let crcs = |inv: &parity::Inventory| inv.files.iter().filter(|f| f.crc32.is_some()).count();
        let atlas_crcs = ai.files.iter().filter(|f| f.crc32.is_some()).count();
        if atlas_crcs != crcs(&pi) {
            println!(
                "  ({label}: atlas folds a CRC for {atlas_crcs} of {} files, par-rs for {})",
                ai.files.len(),
                crcs(&pi)
            );
        }
        let kib = set.index.len() as f64 / 1024.0;
        let reps = if set.index.len() > 1 << 20 { 20 } else { 2_000 };
        let a =
            best(runs, || (0..reps).map(|_| atlas::par2::inventory(black_box(&set.index)).files.len()).sum::<usize>());
        let p = best(runs, || (0..reps).map(|_| parity::inventory(black_box(&set.index)).files.len()).sum::<usize>());
        row(&format!("inventory of index, {label} ({kib:.0} KiB)"), "idx/s", reps as f64, a, p);
        // Atlas checks no packet MD5; this row takes that work out of par-rs's number too.
        let p = best(runs, || (0..reps).map(|_| unverified(black_box(&set.index))).sum::<usize>());
        row("  same, par-rs without packet MD5s", "idx/s", reps as f64, a, p);
    }

    // A recovery volume: mostly bulk data the scan has to step over.
    let (vol_name, vol) = big.volumes.iter().max_by_key(|(_, v)| v.len()).unwrap();
    let vmib = vol.len() as f64 / (1 << 20) as f64;
    assert_eq!(atlas::par2::inventory(vol).files.len(), parity::inventory(vol).files.len());
    let a = best(runs, || atlas::par2::inventory(black_box(vol)).files.len());
    let p = best(runs, || parity::inventory(black_box(vol)).files.len());
    row(&format!("inventory of {vol_name} ({vmib:.1} MiB)"), "MiB/s", vmib, a, p);
    let p = best(runs, || unverified(black_box(vol)));
    row("  same, par-rs without packet MD5s", "MiB/s", vmib, a, p);

    // Base-par2 subject test over a header slice (the indexer runs it on every subject).
    let subjects: Vec<String> = (0..100_000)
        .map(|i| match i % 4 {
            0 => format!("[{i}/99] - \"Some.Movie.2026.par2\" yEnc (1/1) 1024"),
            1 => format!("[{i}/99] - \"Some.Movie.2026.vol{i:03}+04.par2\" yEnc (1/3)"),
            _ => format!("[{i}/99] - \"Some.Movie.2026.part{i:03}.rar\" yEnc (1/50) 50000000"),
        })
        .collect();
    let a = best(runs, || subjects.iter().filter(|s| atlas::par2::is_base_par2(s)).count());
    let p = best(runs, || subjects.iter().filter(|s| parity::is_base_par2(s)).count());
    row("is_base_par2 over 100k subjects", "Ms/s", 0.1, a, p);
}
