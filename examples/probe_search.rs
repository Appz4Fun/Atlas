//! What a filtered search costs, stage by stage, on the data in ATLAS_HOME.
//!
//!     ATLAS_HOME=/path/to/copy cargo run --release --locked --example probe_search -- 5000 1080p matrix

use std::time::Instant;

use atlas::category::parse_filter;
use atlas::search::{Bounds, search_in_categories_with};

fn timed<T>(label: &str, f: impl FnOnce() -> T) -> T {
    let start = Instant::now();
    let out = f();
    println!("  {label:<24} {:>9.1} ms", start.elapsed().as_secs_f64() * 1000.0);
    out
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (cat, queries) = args.split_first().expect("usage: probe_search CAT QUERY...");
    let filter = parse_filter(cat).expect("a category filter");
    println!("cat {cat}: {} leaves, uncategorized {}", filter.leaves.len(), filter.uncategorized);
    for q in queries {
        for round in ["first", "again"] {
            println!("{q:?} {round}");
            let conn = timed("open", || atlas::db::open().unwrap());
            let rows = timed("search_in_categories", || {
                search_in_categories_with(&conn, q, &filter, 0, 100, &Bounds::DEFAULT).unwrap()
            });
            println!("  {} rows", rows.len());
            let ids: Vec<i64> = rows.iter().map(|r| r.id).collect();
            timed("categories_of", || atlas::category::categories_of(&ids).unwrap());
            timed("attrs_of", || atlas::media::query::attrs_of(&ids).unwrap());
            timed("passwords", || atlas::archive::store::passwords(&ids));
        }
    }
}
