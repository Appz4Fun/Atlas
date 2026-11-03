//! Saves real XOVER rows for offline benchmarks (`bench_pipeline`): a few
//! slices from the low, middle and high end of each group, one connection on
//! one server, and how long each request took.
//!
//!     ATLAS_HOME=. cargo run --release --example capture_xover -- --out /tmp/xover.tsv \
//!         --groups a.b.one,a.b.two [--only host] [--size 10000] [--at 0.05,0.5,0.95]
//!
//! Read only (GROUP, XOVER). Rows are written tab separated, like XOVER
//! itself, after a `#group <name> <ms>` line per slice. Prints no passwords.

use std::io::Write;
use std::time::{Duration, Instant};

use atlas::config::load_config;
use atlas::nntp::Conn;

fn arg(name: &str) -> Option<String> {
    std::env::args().skip_while(|a| a != name).nth(1)
}

fn main() {
    tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap().block_on(run());
}

async fn run() {
    let out = arg("--out").expect("--out file");
    let groups: Vec<String> = arg("--groups").expect("--groups a,b").split(',').map(str::to_string).collect();
    let only = arg("--only");
    let size: u64 = arg("--size").and_then(|v| v.parse().ok()).unwrap_or(10_000);
    let at: Vec<f64> =
        arg("--at").unwrap_or_else(|| "0.05,0.5,0.95".into()).split(',').filter_map(|s| s.parse().ok()).collect();

    let cfg = load_config().expect("no config.json (set ATLAS_HOME)");
    let server = cfg
        .servers
        .iter()
        .filter(|s| s.indexes())
        .find(|s| only.as_ref().is_none_or(|o| s.host.contains(o.as_str())))
        .expect("no indexing server");
    let mut conn =
        Conn::open(server, Duration::from_secs(120), server.compress.unwrap_or(true)).await.expect("connect");
    let mut file = std::io::BufWriter::new(std::fs::File::create(&out).unwrap());

    for group in &groups {
        let Ok((_, first, last, _)) = conn.select_group(group).await else {
            eprintln!("{} has no {group}", server.host);
            continue;
        };
        for &f in &at {
            let start = first + ((last - first) as f64 * f) as u64;
            let end = (start + size - 1).min(last);
            let t = Instant::now();
            let rows = match conn.xover(start, end).await {
                Ok(rows) => rows,
                Err(e) => {
                    eprintln!("{group} {start}-{end}: {e}");
                    continue;
                }
            };
            let ms = t.elapsed().as_millis();
            eprintln!("{group} {start}-{end}: {} rows in {ms}ms", rows.len());
            writeln!(file, "#group {group} {ms}").unwrap();
            for r in rows {
                writeln!(
                    file,
                    "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                    r.number, r.subject, r.from, r.date, r.message_id, r.references, r.bytes, r.lines
                )
                .unwrap();
            }
        }
    }
    file.flush().unwrap();
    conn.quit().await;
}
