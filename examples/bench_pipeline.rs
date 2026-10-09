//! Offline CPU side of indexing on captured headers (`capture_xover`): turning
//! XOVER rows into releases, per slice, timed, plus what the slice costs in
//! name lookups (BODY requests) and why headers get dropped.
//!
//!     cargo run --release --example bench_pipeline -- --in /tmp/xover.tsv [--rounds 5] [--samples 20]

use std::collections::HashMap;
use std::time::Instant;

use atlas::nntp::{Overview, headers_to_articles};
use atlas::parser::{group_articles, parse_subject};

fn arg<T: std::str::FromStr>(name: &str, default: T) -> T {
    std::env::args().skip_while(|a| a != name).nth(1).and_then(|v| v.parse().ok()).unwrap_or(default)
}

/// `(group, ms the request took, rows)` per captured slice
pub fn load(path: &str) -> Vec<(String, u64, Vec<Overview>)> {
    let text = std::fs::read_to_string(path).expect("read --in");
    let mut slices: Vec<(String, u64, Vec<Overview>)> = Vec::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("#group ") {
            let (g, ms) = rest.rsplit_once(' ').unwrap();
            slices.push((g.to_string(), ms.parse().unwrap(), Vec::new()));
            continue;
        }
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 8 {
            continue;
        }
        slices.last_mut().unwrap().2.push(Overview {
            number: f[0].parse().unwrap_or(0),
            subject: f[1].into(),
            from: f[2].into(),
            date: f[3].into(),
            message_id: f[4].into(),
            references: f[5].into(),
            bytes: f[6].parse().unwrap_or(0),
            lines: f[7].parse().unwrap_or(0),
        });
    }
    slices.retain(|s| !s.2.is_empty());
    slices
}

/// a subject with digits and long hex/alnum runs folded, to group look-alikes
fn shape(s: &str) -> String {
    let mut out = String::new();
    let mut run = 0;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            run += 1;
            if run == 1 {
                out.push(if c.is_ascii_digit() { '9' } else { 'a' });
            }
        } else {
            run = 0;
            out.push(c);
        }
    }
    out.chars().take(80).collect()
}

fn main() {
    let path: String = arg("--in", String::new());
    let rounds: usize = arg("--rounds", 5);
    let samples: usize = arg("--samples", 20);
    let slices = load(&path);
    let headers: usize = slices.iter().map(|s| s.2.len()).sum();
    println!("{} slices, {headers} headers", slices.len());

    // per group: dropped, releases, name lookups a slice would make
    let mut shapes: HashMap<String, (usize, String)> = HashMap::new();
    for (group, ms, rows) in &slices {
        let dropped = rows.iter().filter(|r| parse_subject(&r.subject).is_none()).count();
        for r in rows.iter().filter(|r| parse_subject(&r.subject).is_none()) {
            let e = shapes.entry(shape(&r.subject)).or_insert((0, r.subject.clone()));
            e.0 += 1;
        }
        let releases = group_articles(headers_to_articles(rows.clone()));
        let lookups: usize = releases
            .values()
            .map(|r| {
                r.articles
                    .iter()
                    .filter(|a| atlas::par2::is_base_par2(&a.subject) || atlas::nfo::is_nfo(&a.subject))
                    .count()
            })
            .sum();
        let with_source = releases
            .values()
            .filter(|r| {
                r.articles.iter().any(|a| atlas::par2::is_base_par2(&a.subject) || atlas::nfo::is_nfo(&a.subject))
            })
            .count();
        println!(
            "{group:34} {:>5} rows {ms:>4}ms  dropped {dropped:>5}  releases {:>5}  with a name source {with_source:>4}  bodies to try {lookups:>4}",
            rows.len(),
            releases.len()
        );
    }

    let mut shapes: Vec<_> = shapes.into_iter().collect();
    shapes.sort_by_key(|a| std::cmp::Reverse(a.1.0));
    println!("\nmost common unparsable shapes:");
    for (shape, (n, example)) in shapes.iter().take(samples) {
        println!("{n:>7}  {shape}\n         e.g. {}", example.chars().take(140).collect::<String>());
    }

    // timing: rows -> releases, as save_slice does it
    let mut best = f64::MAX;
    for _ in 0..rounds {
        let input: Vec<Vec<Overview>> = slices.iter().map(|s| s.2.clone()).collect();
        let t = Instant::now();
        let mut n = 0;
        for rows in input {
            n += group_articles(headers_to_articles(rows)).len();
        }
        std::hint::black_box(n);
        best = best.min(t.elapsed().as_secs_f64());
    }
    println!(
        "\nparse+group: {:.0} headers/s on one core ({:.1}ms per 10k)",
        headers as f64 / best,
        best * 1e4 / headers as f64 * 1e3
    );
}
