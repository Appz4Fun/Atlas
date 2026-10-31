//! What the ingest filters cost: `drop_per_article_obfuscated` over slices with
//! a share of per-article obfuscation, and `group_blocked` per call.
//!
//!     cargo run --release --locked --example bench_filters

use std::time::Instant;

use atlas::filters::{drop_per_article_obfuscated, group_blocked};
use atlas::parser::Article;

const SLICES: usize = 20;
const SLICE: usize = 10_000;

fn random(i: usize, len: usize) -> String {
    let mut x = (i as u64 + 1).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    let abc = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    (0..len)
        .map(|_| {
            x = x.wrapping_mul(0x100_0000_01b3) ^ (x >> 7);
            abc[(x % 62) as usize] as char
        })
        .collect()
}

fn slice(n: usize, percent: usize) -> Vec<Article> {
    (0..SLICE)
        .map(|k| {
            let subject = if k % 100 < percent {
                format!(r#"[41/62] "{}" yEnc ({}/375)"#, random(n * SLICE + k, 32), k % 370 + 2)
            } else {
                let (part, set) = (k % 20 + 1, k / 400);
                format!(
                    r#"[{part:02}/20] - "Some.Show.S01E{:02}.1080p-GRP{set}.part{part:02}.rar" yEnc ({part}/20)"#,
                    set % 90
                )
            };
            Article { subject, ..Default::default() }
        })
        .collect()
}

fn main() {
    for percent in [0, 30, 100] {
        let slices: Vec<Vec<Article>> = (0..SLICES).map(|n| slice(n, percent)).collect();
        let t = Instant::now();
        let mut skipped = 0;
        for articles in slices {
            skipped += drop_per_article_obfuscated(articles).1;
        }
        let secs = t.elapsed().as_secs_f64();
        let articles = (SLICES * SLICE) as f64;
        println!(
            "{percent:3}% obfuscated: {:.0} articles/s, {:.2} us/article, {skipped} skipped",
            articles / secs,
            secs * 1e6 / articles
        );
    }

    let names = ["alt.binaries.pictures.erotica.teen", "alt.binaries.multimedia.teen-wolf"];
    let t = Instant::now();
    let mut blocked = 0usize;
    for i in 0..1_000_000 {
        blocked += usize::from(group_blocked(names[i % 2]));
    }
    println!("group_blocked: {:.0} ns/call, {blocked} blocked", t.elapsed().as_secs_f64() * 1e9 / 1e6);
}
