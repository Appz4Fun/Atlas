//! `atlas --unname`: clear the names regexes and posted files gave, the two that come from subject
//! text. A dry run unless `--apply`. `name_source` says `NameSource::Cleared` after it, soo a late
//! article can't bring one back: the naming step leaves those until `--reprocess --step naming`.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};

use rusqlite::{Connection, params};

use super::clean::without_collision_suffix;
use super::walk::{self, BATCH, WalkLimits};
use crate::postprocess::step::{StepMask, mask};
use crate::postprocess::steps;
use crate::{db, parser, store};

const CURSOR: &str = "unname_after";

fn named_from_subjects(conn: &Connection, after: i64) -> rusqlite::Result<Vec<(i64, String)>> {
    conn.prepare_cached("select id, name from releases where id > ? and name_source in (3, 5) order by id limit ?")?
        .query_map(params![after, BATCH], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect()
}

/// steps past `naming` that want to see a rename
fn later_steps() -> StepMask {
    steps::all().iter().filter(|s| s.rank() > 200 && s.reruns().on_rename).fold(0, |m, s| m | mask(s.id()))
}

fn dry_run(main: &Path, sample: usize) -> anyhow::Result<()> {
    let mut by_source: BTreeMap<i64, u64> = BTreeMap::new();
    let mut shown: Vec<String> = Vec::new();
    for path in store::shard_paths(main) {
        let conn = walk::open_for_reading(&path)?;
        let has_column: bool =
            conn.prepare("select 1 from pragma_table_info('releases') where name = 'name_source'")?.exists([])?;
        if !has_column {
            continue;
        }
        let mut stmt = conn.prepare_cached(
            "select name_source, display_name from releases where name_source in (3, 5) and display_name is not null",
        )?;
        let mut rows = stmt.query([])?;
        while let Some(r) = rows.next()? {
            *by_source.entry(r.get(0)?).or_default() += 1;
            if shown.len() < sample {
                shown.push(r.get(1)?);
            }
        }
    }
    for (source, n) in &by_source {
        let what = if *source == 3 { "posted files" } else { "naming regexes" };
        println!("{n} from {what} (source {source})");
    }
    println!("{} would be cleared", by_source.values().sum::<u64>());
    for name in shown {
        println!("  {name}");
    }
    println!("a dry run: nothing was changed. --apply clears them");
    Ok(())
}

fn apply_shard(
    path: &Path,
    limits: &WalkLimits,
    stop: &AtomicBool,
    commits: &AtomicU64,
    rows: &AtomicU64,
    cleared: &AtomicU64,
    later: StepMask,
) -> anyhow::Result<()> {
    let mut conn = db::open_shard(path)?;
    store::migrate_shard(&conn)?;
    db::tune_for_writing(&conn)?;
    let mut walk = walk::Walk {
        conn: &mut conn,
        path,
        key: CURSOR,
        limits,
        stop,
        commits,
        rows,
        label: "unname",
        shard_commits: 0,
    };
    let picked = |c: &Connection, after: i64| named_from_subjects(c, after);
    let write = |tx: &Connection, batch: &[(i64, String)]| -> rusqlite::Result<()> {
        // releases_au updates FTS
        let mut clear = tx.prepare_cached(
            "update releases set display_name = null, name_source = 1, is_obfuscated = ?2 where id = ?1",
        )?;
        let mut state = tx.prepare_cached(
            "update pp_state set done = done & ~?2, failed = failed & ~?2, db_attempts = 0, net_attempts = 0, net_next_at = 0
             where release_id = ?1",
        )?;
        let mut queue = tx.prepare_cached(
            "insert into pp_queue (release_id, due_at) values (?1, 0) on conflict(release_id) do nothing",
        )?;
        for (id, name) in batch {
            // as Part 2 worked the flag out before adding its suffix
            clear.execute(params![id, parser::is_obfuscated(without_collision_suffix(name))])?;
            state.execute(params![id, later])?;
            if later != 0 {
                queue.execute([id])?;
            }
            cleared.fetch_add(1, Relaxed);
        }
        Ok(())
    };
    while walk.step(picked, write)? {}
    walk::finish_shard(&conn, "key in ('unname_after', 'naming_rename_named') or key glob 'naming_rename_named:*'")
}

/// `atlas --unname [--sample N] [--apply]`. Exit 0 on success, 1 when refused or failed.
pub fn run(args: &[String]) -> i32 {
    run_with(args, later_steps())
}

#[doc(hidden)]
pub fn run_with(args: &[String], later: StepMask) -> i32 {
    let o = match walk::parse(args, "--unname") {
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
    let limits = walk::limits("ATLAS_UNNAME");
    let stop = walk::stop_flag();
    let (commits, rows, cleared) = (AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0));
    let result = walk::each_shard(&main, |path| apply_shard(path, &limits, &stop, &commits, &rows, &cleared, later));
    println!("{} cleared", cleared.load(Relaxed));
    match result {
        Ok(()) => 0,
        Err(e) => {
            println!("{e:#}");
            1
        }
    }
}
