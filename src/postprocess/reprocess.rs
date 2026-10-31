//! `atlas --reprocess`: look at stored releases again. A dry run unless `--apply`.

use rusqlite::{Connection, params};

use crate::config::load_settings;
use crate::store;
use crate::{compact, db, paths};

use super::step::{Step, StepMask, mask};
use super::steps;

/// One shard, in the caller's `BEGIN IMMEDIATE` transaction: a walk that clears
/// `redo`. False: the same walk is already queued.
pub fn apply(tx: &Connection, redo: StepMask, group: Option<&str>, now: i64) -> rusqlite::Result<bool> {
    // asking for naming again is what lets it replace a name it left alone so far
    if redo & mask(crate::naming::NAMING) != 0 {
        tx.prepare_cached("insert or replace into meta (key, value) values ('naming_rename_named', 1)")?.execute([])?;
    }
    let added = tx
        .prepare_cached(
            "insert into pp_walks (kind, top_id, before_id, redo_mask, group_name, created_at)
             select 'reprocess', t, t, ?1, ?2, ?3 from (select coalesce(max(id), -1) + 1 as t from releases)
             where not exists (select 1 from pp_walks where kind = 'reprocess' and redo_mask = ?1 and group_name is ?2)",
        )?
        .execute(params![redo, group, now])?;
    Ok(added > 0)
}

/// Failed steps of up to 1,000 releases try again now, those of `group` when
/// there is one. How many.
fn retry_failed(tx: &Connection, group: Option<&str>, now: i64) -> rusqlite::Result<usize> {
    let ids: Vec<i64> = tx
        .prepare_cached(
            "select s.release_id from pp_state s join releases r on r.id = s.release_id
             where s.failed != 0 and (?1 is null or r.group_name = ?1) limit 1000",
        )?
        .query_map(params![group], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    for id in &ids {
        tx.prepare_cached(
            "update pp_state set failed = 0, db_attempts = 0, net_attempts = 0, net_next_at = 0, last_error = null
             where release_id = ?1",
        )?
        .execute([id])?;
        tx.prepare_cached(
            "insert into pp_queue (release_id, due_at) values (?1, ?2)
             on conflict(release_id) do update set due_at = excluded.due_at",
        )?
        .execute(params![id, now])?;
    }
    Ok(ids.len())
}

struct Options {
    mask: StepMask,
    groups: Vec<String>,
    failed: bool,
    apply: bool,
}

fn parse(args: &[String], known: &[std::sync::Arc<dyn Step>]) -> Result<Options, String> {
    let mut o = Options { mask: 0, groups: Vec::new(), failed: false, apply: false };
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--reprocess" => {}
            "--step" => {
                let name = it.next().ok_or("--step needs a name")?;
                if name == "all" {
                    o.mask |= known.iter().fold(0, |m, s| m | mask(s.id()));
                } else {
                    let Some(step) = known.iter().find(|s| s.name() == name) else {
                        let names: Vec<&str> = known.iter().map(|s| s.name()).collect();
                        return Err(format!("no step {name}. the steps are: all {}", names.join(" ")));
                    };
                    o.mask |= mask(step.id());
                }
            }
            "--group" => o.groups.push(it.next().ok_or("--group needs a name")?.clone()),
            "--failed" => o.failed = true,
            "--apply" => o.apply = true,
            other => return Err(format!("unknown option {other}")),
        }
    }
    Ok(o)
}

/// the shards to touch, with the group to limit each to
fn targets(o: &Options) -> Vec<(usize, Option<&str>)> {
    if o.groups.is_empty() {
        (0..store::SHARDS).map(|s| (s, None)).collect()
    } else {
        o.groups.iter().map(|g| (store::shard_of(g), Some(g.as_str()))).collect()
    }
}

fn dry_run(main: &std::path::Path, o: &Options, per_second: i64) -> anyhow::Result<()> {
    let mut total = 0i64;
    for (shard, group) in targets(o) {
        let conn = db::open_shard(&store::shard_path(main, shard))?;
        conn.execute_batch("pragma query_only = on")?;
        // a shard no build of the stage has opened yet has nothing to run again
        let has_state: bool =
            conn.prepare("select 1 from sqlite_master where type = 'table' and name = 'pp_state'")?.exists([])?;
        if !has_state {
            continue;
        }
        let n: i64 = conn.query_row(
            "select count(*) from pp_state s join releases r on r.id = s.release_id
             where (s.done & ?1 != 0 or (?3 and s.failed != 0)) and (?2 is null or r.group_name = ?2)",
            params![o.mask, group, o.failed],
            |r| r.get(0),
        )?;
        total += n;
    }
    println!("{total} releases would run these steps again");
    if per_second > 0 {
        println!("about {} s to walk them at backlog_per_second", total / per_second);
    }
    println!("a dry run: nothing was changed. --apply queues it");
    Ok(())
}

fn apply_all(main: &std::path::Path, o: &Options, now: i64) -> anyhow::Result<()> {
    let _writing = compact::hold_off_compaction(main)?;
    for (shard, group) in targets(o) {
        let mut conn = db::open_shard(&store::shard_path(main, shard))?;
        // this can be the first command after an upgrade
        store::migrate_shard(&conn)?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        if apply(&tx, o.mask, group, now)? {
            println!("shard {shard}: queued");
        } else {
            println!("shard {shard}: already queued");
        }
        tx.commit()?;
        if o.failed {
            loop {
                let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
                let n = retry_failed(&tx, group, now)?;
                tx.commit()?;
                if n == 0 {
                    break;
                }
            }
        }
    }
    Ok(())
}

/// `atlas --reprocess`. Returns the exit code.
pub fn main(args: &[String]) -> i32 {
    let known = steps::all();
    main_with(args, &known)
}

#[doc(hidden)]
pub fn main_with(args: &[String], known: &[std::sync::Arc<dyn Step>]) -> i32 {
    let o = match parse(args, known) {
        Ok(o) => o,
        Err(e) => {
            println!("{e}");
            return 1;
        }
    };
    let main = paths::database();
    let now = chrono::Utc::now().timestamp();
    let result = if o.apply {
        apply_all(&main, &o, now)
    } else {
        dry_run(&main, &o, load_settings().postprocess.backlog_per_second)
    };
    match result {
        Ok(()) => 0,
        Err(e) => {
            println!("{e:#}");
            1
        }
    }
}
