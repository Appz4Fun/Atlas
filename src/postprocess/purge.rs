//! `atlas --purge`: what no step can help, deleted in small transactions.
//! A dry run unless `--apply`, and it can stop and carry on at any point.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};

use anyhow::bail;
use rusqlite::{Connection, params};

use crate::config::{PostprocessConfig, load_settings};
use crate::filters::{self, StoredPost};
use crate::store::{self, set_meta};
use crate::{compact, convert, db, paths, procs};

use super::batch::{COMMITS_PER_CHECKPOINT, WAL_LIMIT, checkpoint_or_stop};
use super::rules::{self, FileSummary, Reason, RowFacts, Rules, Verdict};

const PURGE_BATCH: i64 = 2_000;
const PURGE_TX_ARTICLES: i64 = 200_000;
const PURGE_SEGMENT_CHUNK: i64 = 20_000;
/// shards at once
const AT_ONCE: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Class {
    Fragments = 1,
    Blocked = 2,
    TooSmall = 4,
    ImageOnly = 8,
    Rejected = 16,
    Dead = 32,
}

const CLASSES: [Class; 6] =
    [Class::Fragments, Class::Blocked, Class::TooSmall, Class::ImageOnly, Class::Rejected, Class::Dead];

impl Class {
    fn name(self) -> &'static str {
        match self {
            Class::Fragments => "fragments",
            Class::Blocked => "blocked",
            Class::TooSmall => "too-small",
            Class::ImageOnly => "image-only",
            Class::Rejected => "rejected",
            Class::Dead => "dead",
        }
    }
}

struct Options {
    classes: i64,
    below: i64,
    sample: usize,
    apply: bool,
}

fn parse(args: &[String], config: &PostprocessConfig) -> anyhow::Result<Options> {
    let mut o = Options { classes: 0, below: config.body_min_size_bytes, sample: 0, apply: false };
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let number = |value: Option<&String>| -> anyhow::Result<i64> {
            value
                .and_then(|v| v.parse().ok())
                .filter(|n| *n >= 0)
                .ok_or_else(|| anyhow::anyhow!("{arg} needs a number"))
        };
        match arg.as_str() {
            "--purge" => {}
            "--fragments" => o.classes |= Class::Fragments as i64,
            "--blocked" => o.classes |= Class::Blocked as i64,
            "--too-small" => o.classes |= Class::TooSmall as i64,
            "--image-only" => o.classes |= Class::ImageOnly as i64,
            "--rejected" => o.classes |= Class::Rejected as i64,
            "--dead" => o.classes |= Class::Dead as i64,
            "--below" => o.below = number(it.next())?,
            "--sample" => o.sample = number(it.next())? as usize,
            "--apply" => o.apply = true,
            other => bail!("unknown option {other}"),
        }
    }
    if o.classes == 0 {
        o.classes = Class::Fragments as i64;
    }
    Ok(o)
}

/// The class a release counts in: the first selected one that takes it.
fn classify(rules: &Rules, o: &Options, row: &RowFacts<'_>, s: &FileSummary, now: i64) -> Option<Class> {
    let on = |c: Class| o.classes & c as i64 != 0;
    if on(Class::Blocked) && filters::group_blocked(row.group) {
        // never indexed or served again anyway: no wait
        return Some(Class::Blocked);
    }
    // nothing else goes until nothing can still arrive for it
    if now - s.last_touch < rules.config.dead_after_secs {
        return None;
    }
    let post = StoredPost {
        name: row.name,
        parts: row.parts,
        files: s.files as usize,
        part: s.single_part,
        expected: s.single_expected,
        subject: s.single_subject.as_deref(),
    };
    if on(Class::Fragments) && filters::stored_fragment(&post) {
        return Some(Class::Fragments);
    }
    if on(Class::TooSmall) && row.size < o.below {
        return Some(Class::TooSmall);
    }
    if on(Class::ImageOnly) && s.files > 0 && s.all_images {
        return Some(Class::ImageOnly);
    }
    if on(Class::Rejected) {
        // age was settled above: only the rules' verdict counts here
        let settled = rules::judge_files(rules, row, s, i64::MAX / 2);
        let empty = row.parts <= 0;
        if empty || matches!(settled, Verdict::Reject(Reason::Par2Only | Reason::TooFewFiles | Reason::Empty)) {
            return Some(Class::Rejected);
        }
    }
    if on(Class::Dead) && rules::permille(row, s) < rules.config.min_completion_percent * 10 {
        return Some(Class::Dead);
    }
    None
}

fn env_i64(key: &str) -> Option<i64> {
    std::env::var(key).ok().and_then(|v| v.parse().ok())
}

struct ReadRow {
    id: i64,
    name: String,
    group: String,
    size: i64,
    parts: i64,
    complete: bool,
    file_total: Option<i64>,
}

impl ReadRow {
    fn facts(&self) -> RowFacts<'_> {
        RowFacts {
            group: &self.group,
            name: &self.name,
            size: self.size,
            parts: self.parts,
            complete: self.complete,
            file_total: self.file_total,
        }
    }
}

fn read_batch(conn: &Connection, cursor: i64, top: i64) -> rusqlite::Result<Vec<ReadRow>> {
    conn.prepare_cached(
        "select id, name, group_name, coalesce(size, 0), coalesce(parts, 0), coalesce(complete, 0) != 0, file_total
         from releases where id > ? and id <= ? order by id limit ?",
    )?
    .query_map(params![cursor, top, PURGE_BATCH], |r| {
        Ok(ReadRow {
            id: r.get(0)?,
            name: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
            group: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
            size: r.get(3)?,
            parts: r.get(4)?,
            complete: r.get(5)?,
            file_total: r.get(6)?,
        })
    })?
    .collect()
}

/// What a dry run found in all shards.
#[derive(Default)]
struct Found {
    per_class: BTreeMap<Class, (i64, i64)>,
    samples: BTreeMap<Class, Vec<String>>,
    groups: BTreeMap<String, (i64, i64)>,
    sizes: [i64; 3],
    victims: Vec<i64>,
}

fn dry_run(main: &Path, rules: &Rules, o: &Options, now: i64) -> anyhow::Result<Found> {
    let mut found = Found::default();
    for path in store::shard_paths(main) {
        let conn = db::open_shard(&path)?;
        conn.execute_batch("pragma query_only = on")?;
        if let Some(victim) = store::get_meta(&conn, "purge_victim")? {
            found.victims.push(victim);
        }
        let mut cursor = i64::MIN;
        loop {
            let rows = read_batch(&conn, cursor, i64::MAX)?;
            let Some(last) = rows.last().map(|r| r.id) else { break };
            cursor = last;
            let ids: Vec<i64> = rows.iter().map(|r| r.id).collect();
            let mut summaries = rules::file_summaries(&conn, &ids)?;
            for row in &rows {
                let s = summaries.remove(&row.id).unwrap_or_default();
                let Some(class) = classify(rules, o, &row.facts(), &s, now) else { continue };
                let entry = found.per_class.entry(class).or_default();
                entry.0 += 1;
                entry.1 += row.size;
                let sample = found.samples.entry(class).or_default();
                if sample.len() < o.sample {
                    sample.push(row.name.clone());
                }
                if class == Class::Blocked {
                    let g = found.groups.entry(row.group.clone()).or_default();
                    g.0 += 1;
                    g.1 += row.size;
                }
                if class == Class::TooSmall {
                    for (n, limit) in [64 << 10, 256 << 10, 1 << 20].iter().enumerate() {
                        if row.size <= *limit {
                            found.sizes[n] += 1;
                        }
                    }
                }
            }
        }
    }
    Ok(found)
}

fn report(found: &Found, o: &Options) {
    for victim in &found.victims {
        println!("release {victim} is half deleted by a stopped purge: the next --apply finishes it first");
    }
    for class in CLASSES {
        if o.classes & class as i64 == 0 {
            continue;
        }
        let (n, bytes) = found.per_class.get(&class).copied().unwrap_or((0, 0));
        println!("{}: {n} releases, {bytes} bytes", class.name());
        for name in found.samples.get(&class).into_iter().flatten() {
            println!("  {name}");
        }
        if class == Class::Blocked {
            for (group, (n, bytes)) in &found.groups {
                println!("  {group}: {n} releases, {bytes} bytes");
            }
        }
        if class == Class::TooSmall {
            println!("  at most 64 KiB: {}, 256 KiB: {}, 1 MiB: {}", found.sizes[0], found.sizes[1], found.sizes[2]);
        }
    }
}

/// shared by the shards of one apply
struct Run {
    stop: Arc<AtomicBool>,
    commits: AtomicUsize,
    deleted: AtomicI64,
    /// stop after this many commits, for tests
    stop_after: Option<usize>,
    chunk: i64,
    check_every: u32,
    wal_limit: u64,
}

impl Run {
    fn stopping(&self) -> bool {
        self.stop.load(Ordering::Relaxed) || self.stop_after.is_some_and(|n| self.commits.load(Ordering::SeqCst) >= n)
    }

    /// One commit done: every so many, a checkpoint that can stop the run.
    fn committed(&self, conn: &Connection, path: &Path, since: &mut u32) -> anyhow::Result<()> {
        self.commits.fetch_add(1, Ordering::SeqCst);
        *since += 1;
        if *since >= self.check_every {
            *since = 0;
            checkpoint_or_stop(conn, path, self.wal_limit)?;
        }
        Ok(())
    }
}

fn subtract(tx: &Connection, key: &str, by: i64) -> rusqlite::Result<()> {
    tx.prepare_cached("update meta set value = max(value - ?, 0) where key = ?")?.execute(params![by, key])?;
    Ok(())
}

/// A release too big for one transaction: its marker first in its own commit,
/// then its rows in chunks, and the rest with its row. A stop leaves the marker,
/// and the next run finishes it whatever its classes.
fn delete_huge(conn: &mut Connection, path: &Path, run: &Run, id: i64, since: &mut u32) -> anyhow::Result<bool> {
    set_meta(conn, "purge_victim", id)?;
    run.committed(conn, path, since)?;
    let files: Vec<i64> = conn
        .prepare_cached("select id from files where release_id = ?")?
        .query_map([id], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    for file in files {
        loop {
            if run.stopping() {
                return Ok(false);
            }
            let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let gone = tx
                .prepare_cached(
                    "delete from segments where (file_id, local, domain) in
                     (select file_id, local, domain from segments where file_id = ?1 limit ?2)",
                )?
                .execute(params![file, run.chunk])? as i64;
            subtract(&tx, "articles", gone)?;
            tx.commit()?;
            run.committed(conn, path, since)?;
            if gone == 0 {
                break;
            }
        }
    }
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let deleted = store::delete_release(&tx, id)?;
    subtract(&tx, "articles", deleted.articles)?;
    subtract(&tx, "releases", 1)?;
    tx.execute("delete from meta where key = 'purge_victim'", [])?;
    tx.commit()?;
    run.deleted.fetch_add(1, Ordering::Relaxed);
    run.committed(conn, path, since)?;
    Ok(true)
}

/// One shard's apply. A stop returns with its cursor committed.
fn apply_shard(
    path: &Path,
    rules: &Rules,
    o: &Options,
    run: &Run,
    now: i64,
    say: &dyn Fn(String),
) -> anyhow::Result<()> {
    let mut conn = db::open_shard(path)?;
    db::tune_for_writing(&conn)?;
    let mut since = 0u32;

    if let Some(victim) = store::get_meta(&conn, "purge_victim")?
        && !delete_huge(&mut conn, path, run, victim, &mut since)?
    {
        return Ok(());
    }

    let stored = store::get_meta(&conn, "purge_classes")?;
    if stored != Some(o.classes) {
        if stored.is_some() {
            say(format!("{}: other classes than the stopped run: starting over", path.display()));
        }
        let top: Option<i64> = conn.query_row("select max(id) from releases", [], |r| r.get(0))?;
        let tx = conn.transaction()?;
        set_meta(&tx, "purge_top", top.unwrap_or(0))?;
        set_meta(&tx, "purge_classes", o.classes)?;
        set_meta(&tx, "purge_after", i64::MIN)?;
        tx.commit()?;
    }
    let top = store::get_meta(&conn, "purge_top")?.unwrap_or(0);
    let mut cursor = store::get_meta(&conn, "purge_after")?.unwrap_or(i64::MIN);

    loop {
        if run.stopping() {
            return Ok(());
        }
        let rows = read_batch(&conn, cursor, top)?;
        let Some(last) = rows.last().map(|r| r.id) else { break };
        let ids: Vec<i64> = rows.iter().map(|r| r.id).collect();
        let mut summaries = rules::file_summaries(&conn, &ids)?;
        let mut victims: Vec<(i64, i64)> = Vec::new();
        for row in &rows {
            let s = summaries.remove(&row.id).unwrap_or_default();
            if classify(rules, o, &row.facts(), &s, now).is_some() {
                victims.push((row.id, row.parts));
            }
        }

        // the small ones in transactions of a bounded size, a huge one by itself
        let mut at = 0;
        while at < victims.len() {
            if run.stopping() {
                return Ok(());
            }
            let (id, parts) = victims[at];
            if parts > PURGE_SEGMENT_CHUNK.min(run.chunk) {
                if !delete_huge(&mut conn, path, run, id, &mut since)? {
                    return Ok(());
                }
                at += 1;
                continue;
            }
            let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let (mut articles, mut releases, mut taken) = (0i64, 0i64, 0i64);
            let mut through = id - 1;
            while at < victims.len() {
                let (id, parts) = victims[at];
                if parts > PURGE_SEGMENT_CHUNK.min(run.chunk) || (taken > 0 && taken + parts > PURGE_TX_ARTICLES) {
                    break;
                }
                articles += store::delete_release(&tx, id)?.articles;
                releases += 1;
                taken += parts;
                through = id;
                at += 1;
            }
            subtract(&tx, "articles", articles)?;
            subtract(&tx, "releases", releases)?;
            set_meta(&tx, "purge_after", through)?;
            tx.commit()?;
            run.deleted.fetch_add(releases, Ordering::Relaxed);
            run.committed(&conn, path, &mut since)?;
        }
        set_meta(&conn, "purge_after", last)?;
        cursor = last;
    }

    let tx = conn.transaction()?;
    tx.execute("delete from meta where key in ('purge_top', 'purge_classes', 'purge_after', 'purge_victim')", [])?;
    tx.commit()?;
    let _ = conn.query_row("pragma wal_checkpoint(truncate)", [], |_| Ok(()));
    Ok(())
}

fn apply(main: &Path, rules: &Rules, o: &Options, now: i64) -> anyhow::Result<bool> {
    let stop = Arc::new(AtomicBool::new(false));
    #[cfg(unix)]
    {
        let _ = signal_hook::flag::register(signal_hook::consts::SIGTERM, stop.clone());
        let _ = signal_hook::flag::register(signal_hook::consts::SIGINT, stop.clone());
    }
    let run = Run {
        stop,
        commits: AtomicUsize::new(0),
        deleted: AtomicI64::new(0),
        stop_after: env_i64("ATLAS_PURGE_STOP_AFTER").map(|n| n as usize),
        chunk: env_i64("ATLAS_PURGE_CHUNK").unwrap_or(PURGE_SEGMENT_CHUNK),
        check_every: env_i64("ATLAS_PURGE_CHECK_EVERY").map_or(COMMITS_PER_CHECKPOINT, |n| n as u32),
        wal_limit: env_i64("ATLAS_PURGE_WAL_LIMIT").map_or(WAL_LIMIT, |n| n as u64),
    };
    let paths: Vec<PathBuf> = store::shard_paths(main);
    let next = AtomicUsize::new(0);
    let say = |msg: String| println!("{msg}");
    let results: Vec<anyhow::Result<()>> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..AT_ONCE)
            .map(|_| {
                scope.spawn(|| -> anyhow::Result<()> {
                    while let Some(path) = paths.get(next.fetch_add(1, Ordering::SeqCst)) {
                        apply_shard(path, rules, o, &run, now, &say)?;
                        if run.stopping() {
                            break;
                        }
                    }
                    Ok(())
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap_or_else(|_| Err(anyhow::anyhow!("a purge thread panicked"))))
            .collect()
    });
    for r in results {
        r?;
    }
    let (commits, deleted) = (run.commits.load(Ordering::SeqCst), run.deleted.load(Ordering::Relaxed));
    println!("deleted {deleted} releases in {commits} commits");
    Ok(!run.stopping())
}

/// `atlas --purge`. Returns the exit code.
pub fn main(args: &[String]) -> i32 {
    let config = load_settings().postprocess;
    let o = match parse(args, &config) {
        Ok(o) => o,
        Err(e) => {
            println!("{e}");
            return 1;
        }
    };
    let rules = Rules::new(&config);
    let main = paths::database();
    let now = chrono::Utc::now().timestamp();

    if !o.apply {
        match compact::try_hold_off_compaction(&main) {
            Ok(Some(guard)) => drop(guard),
            Ok(None) => {
                println!("a compaction is running, try again after it");
                return 1;
            }
            Err(e) => {
                println!("{e:#}");
                return 1;
            }
        }
        return match dry_run(&main, &rules, &o, now) {
            Ok(found) => {
                report(&found, &o);
                println!("a dry run: nothing was changed. --apply deletes them, then run atlas --compact");
                0
            }
            Err(e) => {
                println!("couldnt read the database: {e:#}");
                1
            }
        };
    }

    if procs::indexer_alive() {
        println!("stop indexing first");
        return 1;
    }
    let lock = match compact::Lock::take(&main) {
        Ok(lock) => lock,
        Err(e) => {
            println!("{e:#}");
            return 1;
        }
    };
    if let Err(e) = compact::refuse_unresolved_backups(&main) {
        println!("{e:#}");
        return 1;
    }
    if convert::needed(&main) {
        println!("this database needs converting first: atlas --convert");
        return 1;
    }
    let result = apply(&main, &rules, &o, now);
    drop(lock);
    match result {
        Ok(true) => {
            println!("done. atlas --compact gives the space back");
            0
        }
        Ok(false) => {
            println!("stopped: run it again to carry on");
            0
        }
        Err(e) => {
            println!("couldnt finish: {e:#}. what is committed stays, run it again to carry on");
            1
        }
    }
}
