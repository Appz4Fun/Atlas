//! What collection grouping costs against grouping by file name.
//!
//!     cargo run --release --locked --example bench_collections

use std::time::Instant;

use atlas::parser::Article;
use atlas::{collections, parser};

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

fn article(k: usize, subject: String) -> Article {
    Article {
        subject,
        author: "poster <p@x>".into(),
        date: "2026-10-03 10:00:00".into(),
        message_id: format!("<{k}@bench>"),
        bytes: 700_000,
        ..Default::default()
    }
}

fn mixed() -> Vec<Vec<Article>> {
    (0..SLICES)
        .map(|n| {
            (n * SLICE..(n + 1) * SLICE)
                .map(|k| {
                    let subject = if k % 2 == 0 {
                        let (part, set) = (k % 20 + 1, k / 400);
                        format!(
                            r#"[{part:02}/20] - "Some.Show.S01E{:02}.1080p-GRP{set}.part{part:02}.rar" yEnc ({part}/20)"#,
                            set % 90
                        )
                    } else {
                        format!(r#"[{:03}/200] "Album{}_{:04}.jpg" yEnc (1/1)"#, k % 200 + 1, k / 2000, k % 200 + 1)
                    };
                    article(k, subject)
                })
                .collect()
        })
        .collect()
}

fn worst() -> Vec<Vec<Article>> {
    (0..SLICES)
        .map(|n| {
            (n * SLICE..(n + 1) * SLICE)
                .map(|i| {
                    let subject =
                        format!(r#"[{}/9000] "Some.Name.{}.{i}.bin" yEnc (1/1)"#, i % 9000 + 1, random(i, 12));
                    article(i, subject)
                })
                .collect()
        })
        .collect()
}

fn rate(label: &str, slices: Vec<Vec<Article>>, mut group: impl FnMut(Vec<Article>) -> usize) {
    let articles = (SLICES * SLICE) as f64;
    let t = Instant::now();
    let releases: usize = slices.into_iter().map(&mut group).sum();
    let secs = t.elapsed().as_secs_f64();
    println!(
        "  {label}: {:.0} articles/s, {:.2} us/article, {releases} releases",
        articles / secs,
        secs * 1e6 / articles
    );
}

fn main() {
    for group in ["alt.binaries.test", "alt.binaries.boneless", "alt.binaries.e-book", "alt.binaries.sounds.mp3"] {
        println!("{group}");
        rate("by name", mixed(), |a| parser::group_articles(a).len());
        let t = Instant::now();
        collections::group_articles(group, Vec::new());
        println!("  first use of its regexes: {:.2} ms", t.elapsed().as_secs_f64() * 1e3);
        rate("by collection", mixed(), |a| collections::group_articles(group, a).len());
    }
    for group in ["alt.binaries.test", "alt.binaries.e-book"] {
        println!("{group}, every article its own file");
        rate("by collection", worst(), |a| collections::group_articles(group, a).len());
    }
}
