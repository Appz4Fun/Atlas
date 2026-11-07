//! One-off seed of the shared catalog before the triggers are in (not committed). Reads each shard read-only and
//! upserts its release-level tables into Postgres with `shared_catalog::fill_shard`. A real fill after the
//! triggers are in refreshes whatever changed meanwhile.
//!
//! ATLAS_CATALOG_URL='host=... password=...' cargo run --release --example zz_catalog_seed [shards, e.g. 6,7]

use std::time::Instant;

use rusqlite::{Connection, OpenFlags};

fn main() -> anyhow::Result<()> {
    let url = std::env::var("ATLAS_CATALOG_URL")?;
    let main = atlas::paths::database();
    let wanted: Vec<usize> = std::env::args()
        .nth(1)
        .map(|a| a.split(',').filter_map(|s| s.trim().parse().ok()).collect())
        .unwrap_or_else(|| (0..atlas::store::SHARDS).collect());
    let config = atlas::config::SharedCatalogConfig { enabled: true, url, ..Default::default() };
    let mut pg = atlas::shared_catalog::connect(&config)?;
    for shard in wanted {
        let path = atlas::store::shard_path(&main, shard);
        // held off for the shard's run only, soo a compaction isn't kept waiting for hours
        let _hold = loop {
            match atlas::compact::try_hold_off_compaction(&main)? {
                Some(h) => break h,
                None => std::thread::sleep(std::time::Duration::from_secs(30)),
            }
        };
        let conn = Connection::open_with_flags(
            format!("file:{}?mode=ro", path.display()),
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        conn.busy_timeout(std::time::Duration::from_secs(30))?;
        let started = Instant::now();
        let written = atlas::shared_catalog::fill_shard(&conn, &mut pg, shard)?;
        let total: u64 = written.iter().map(|(_, n)| n).sum();
        println!("shard {shard}: {total} rows in {}s", started.elapsed().as_secs());
        for (t, n) in written.iter().filter(|(_, n)| *n > 0) {
            println!("  {t}: {n}");
        }
    }
    Ok(())
}
