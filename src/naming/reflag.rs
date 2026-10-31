//! `atlas --reflag-obfuscated`: the obfuscation flag on rows saved by older builds. A dry run unless `--apply`.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};

use rusqlite::{Connection, params};

use super::clean::without_collision_suffix;
use super::walk::{self, BATCH, WalkLimits};
use crate::{db, parser, store};

const CURSOR: &str = "reflag_after";

fn flags(name: &str) -> bool {
    parser::is_obfuscated(without_collision_suffix(name))
}

fn candidates(conn: &Connection, after: i64) -> rusqlite::Result<Vec<(i64, String)>> {
    read_batch(
        conn,
        "select id, name from releases where id > ? and name_source is null and is_obfuscated = 0 order by id limit ?",
        after,
    )
}

/// A dry run writes nothing, not even the column this build adds on opening a shard to write.
fn candidates_before_migrating(conn: &Connection, after: i64) -> rusqlite::Result<Vec<(i64, String)>> {
    read_batch(conn, "select id, name from releases where id > ? and is_obfuscated = 0 order by id limit ?", after)
}

fn read_batch(conn: &Connection, sql: &str, after: i64) -> rusqlite::Result<Vec<(i64, String)>> {
    conn.prepare_cached(sql)?.query_map(params![after, BATCH], |r| Ok((r.get(0)?, r.get(1)?)))?.collect()
}

fn dry_run(main: &Path, sample: usize) -> anyhow::Result<()> {
    let (mut total, mut shown) = (0u64, Vec::new());
    for path in store::shard_paths(main) {
        let conn = walk::open_for_reading(&path)?;
        let read = if walk::has_name_source(&conn)? { candidates } else { candidates_before_migrating };
        let mut after = i64::MIN;
        loop {
            let batch = read(&conn, after)?;
            let Some((last, _)) = batch.last() else { break };
            after = *last;
            for (_, name) in batch.iter().filter(|(_, n)| flags(n)) {
                total += 1;
                if shown.len() < sample {
                    shown.push(name.clone());
                }
            }
        }
    }
    println!("{total} would be flagged");
    for name in shown {
        println!("  {name}");
    }
    println!("a dry run: nothing was changed. --apply sets the flag");
    Ok(())
}

fn apply_shard(
    path: &Path,
    limits: &WalkLimits,
    stop: &AtomicBool,
    commits: &AtomicU64,
    rows: &AtomicU64,
    flagged: &AtomicU64,
) -> anyhow::Result<()> {
    let mut conn = db::open_shard(path)?;
    store::migrate_shard(&conn)?;
    db::tune_for_writing(&conn)?;
    let mut walk = walk::Walk { conn: &mut conn, path, key: CURSOR, limits, stop, commits, rows, label: "reflag" };
    let picked = |c: &Connection, after: i64| candidates(c, after);
    let write = |tx: &Connection, batch: &[(i64, String)]| -> rusqlite::Result<()> {
        let mut update = tx.prepare_cached("update releases set is_obfuscated = 1 where id = ?")?;
        for (id, name) in batch {
            if flags(name) {
                update.execute([id])?;
                flagged.fetch_add(1, Relaxed);
            }
        }
        Ok(())
    };
    while walk.step(picked, write)? {}
    walk::finish_shard(&conn, "'reflag_after'")
}

/// `atlas --reflag-obfuscated [--sample N] [--apply]`. Exit 0 on success, 1 when refused or failed.
pub fn run(args: &[String]) -> i32 {
    let o = match walk::parse(args, "--reflag-obfuscated") {
        Ok(o) => o,
        Err(e) => {
            println!("{e}");
            return 1;
        }
    };
    let main = walk::main_path();
    if !o.apply {
        if let Err(e) = walk::may_read(&main) {
            println!("{e}");
            return 1;
        }
        return match dry_run(&main, o.sample) {
            Ok(()) => 0,
            Err(e) => {
                println!("{e:#}");
                1
            }
        };
    }
    let _lock = match walk::take_lock(&main) {
        Ok(lock) => lock,
        Err(e) => {
            println!("{e}");
            return 1;
        }
    };
    let limits = walk::limits("ATLAS_REFLAG");
    let stop = walk::stop_flag();
    let (commits, rows, flagged) = (AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0));
    let result = walk::each_shard(&main, |path| apply_shard(path, &limits, &stop, &commits, &rows, &flagged));
    println!("{} flagged", flagged.load(Relaxed));
    match result {
        Ok(()) => 0,
        Err(e) => {
            println!("{e:#}");
            1
        }
    }
}
