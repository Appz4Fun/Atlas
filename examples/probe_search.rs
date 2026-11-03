//! What a filtered search costs, stage by stage, on the data in ATLAS_HOME.
//!
//!     ATLAS_HOME=/path/to/copy cargo run --release --locked --example probe_search -- 5000 1080p matrix
//!
//! Add `--warm` first to warm the search tables and time that too. `--plain` searches with no category. `--fresh` opens a connection per search.

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
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut flag = |name: &str| args.iter().position(|a| a == name).map(|i| args.remove(i)).is_some();
    let (warm, plain, fresh) = (flag("--warm"), flag("--plain"), flag("--fresh"));
    if warm {
        timed("warm", || atlas::db::warm_search_tables(&atlas::db::open().unwrap()).unwrap());
    }
    let (cat, queries) = args.split_first().expect("usage: probe_search [--warm] [--plain] [--fresh] CAT QUERY...");
    let filter = parse_filter(cat).expect("a category filter");
    println!("cat {cat}: {} leaves, uncategorized {}", filter.leaves.len(), filter.uncategorized);
    for q in queries {
        for round in ["first", "again"] {
            println!("{q:?} {round}");
            let rows = timed("search_in_categories", || {
                let run = |c: &rusqlite::Connection| search_in_categories_with(c, q, &filter, 0, 100, &Bounds::DEFAULT);
                if fresh { run(&atlas::db::open().unwrap()).unwrap() } else { atlas::db::with_reader(run).unwrap() }
            });
            println!("  {} rows", rows.len());
            let ids: Vec<i64> = rows.iter().map(|r| r.id).collect();
            timed("categories_of", || atlas::category::categories_of(&ids).unwrap());
            timed("attrs_of", || atlas::media::query::attrs_of(&ids).unwrap());
            timed("passwords", || atlas::archive::store::passwords(&ids));
            if plain {
                let rows = timed("search_all_releases", || atlas::search::search_all_releases(q, 0, 100).unwrap());
                println!("  {} rows, no category", rows.len());
            }
        }
    }
}
