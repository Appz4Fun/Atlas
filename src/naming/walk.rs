//! What `atlas --reflag-obfuscated` and `atlas --unname` share: the same refusals, a batched
//! walk of each shard that resumes from a cursor in `meta`, and the WAL rule.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};

use rusqlite::{Connection, OptionalExtension, params};

use crate::postprocess::batch;
use crate::{compact, convert, db, paths, procs, store};

pub(crate) const BATCH: i64 = 2_000;
const SHARDS_AT_ONCE: usize = 4;
const PROGRESS_EVERY: u64 = 1_000_000;

pub(crate) struct Options {
    pub apply: bool,
    pub sample: usize,
}

pub(crate) fn parse(args: &[String], own: &str) -> Result<Options, String> {
    let mut o = Options { apply: false, sample: 0 };
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            a if a == own => {}
            "--apply" => o.apply = true,
            "--sample" => {
                let n = it.next().ok_or("--sample needs a number")?;
                o.sample = n.parse().map_err(|_| format!("--sample needs a number, not {n}"))?;
            }
            other => return Err(format!("unknown option {other}")),
        }
    }
    Ok(o)
}

pub(crate) fn open_for_reading(path: &Path) -> anyhow::Result<Connection> {
    let conn = db::open_shard(path)?;
    conn.execute_batch("pragma query_only = on")?;
    Ok(conn)
}

/// no: no build of the stage has opened this shard to write yet
pub(crate) fn has_name_source(conn: &Connection) -> rusqlite::Result<bool> {
    conn.prepare("select 1 from pragma_table_info('releases') where name = 'name_source'")?.exists([])
}

/// A dry run, with compaction held off only as long as it takes to be sure none is running.
pub(crate) fn may_read(main: &Path) -> Result<(), String> {
    match compact::try_hold_off_compaction(main) {
        Ok(Some(_guard)) => Ok(()),
        Ok(None) => Err("a compaction is running: wait for it, then run this again".into()),
        Err(e) => Err(format!("{e:#}")),
    }
}

/// What the commands refuse when they'd write. Holds the lock on success.
pub(crate) fn take_lock(main: &Path) -> Result<compact::Lock, String> {
    if procs::indexer_alive() {
        return Err("stop indexing first".into());
    }
    let lock = compact::Lock::take(main).map_err(|e| format!("{e:#}"))?;
    compact::refuse_unresolved_backups(main).map_err(|e| format!("{e:#}"))?;
    if convert::needed(main) {
        return Err("this database needs atlas --convert first".into());
    }
    Ok(lock)
}

/// test knobs. Each is read once
fn env_u64(key: &str) -> Option<u64> {
    std::env::var(key).ok().and_then(|v| v.parse().ok())
}

pub(crate) struct WalkLimits {
    /// commits between checkpoints
    pub check_every: u32,
    pub wal_limit: u64,
    /// stop after this many commits, as a crash would
    pub stop_after: Option<u64>,
}

pub(crate) fn limits(prefix: &str) -> WalkLimits {
    WalkLimits {
        check_every: env_u64(&format!("{prefix}_CHECK_EVERY")).map_or(batch::COMMITS_PER_CHECKPOINT, |n| n as u32),
        wal_limit: env_u64(&format!("{prefix}_WAL_LIMIT")).unwrap_or(batch::WAL_LIMIT),
        stop_after: env_u64(&format!("{prefix}_STOP_AFTER")),
    }
}

pub(crate) fn stop_flag() -> Arc<AtomicBool> {
    let stop = Arc::new(AtomicBool::new(false));
    let _ = signal_hook::flag::register(signal_hook::consts::SIGTERM, stop.clone());
    let _ = signal_hook::flag::register(signal_hook::consts::SIGINT, stop.clone());
    stop
}

/// Every shard, a few at once. The first error wins, the others still finish their batch.
pub(crate) fn each_shard(main: &Path, run: impl Fn(&Path) -> anyhow::Result<()> + Sync) -> anyhow::Result<()> {
    let paths: Vec<PathBuf> = store::shard_paths(main);
    let next = AtomicU64::new(0);
    let errors: Vec<anyhow::Error> = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..SHARDS_AT_ONCE)
            .map(|_| {
                scope.spawn(|| -> anyhow::Result<()> {
                    loop {
                        let i = next.fetch_add(1, Relaxed) as usize;
                        let Some(path) = paths.get(i) else { return Ok(()) };
                        run(path)?;
                    }
                })
            })
            .collect();
        workers.into_iter().filter_map(|w| w.join().ok().and_then(Result::err)).collect()
    });
    match errors.into_iter().next() {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// A cursor in `meta`, and what comes with stepping through a shard by it.
pub(crate) struct Walk<'a> {
    pub conn: &'a mut Connection,
    pub path: &'a Path,
    pub key: &'static str,
    pub limits: &'a WalkLimits,
    pub stop: &'a AtomicBool,
    pub commits: &'a AtomicU64,
    pub rows: &'a AtomicU64,
    pub label: &'static str,
}

impl Walk<'_> {
    pub fn cursor(&self) -> anyhow::Result<i64> {
        let found: Option<i64> =
            self.conn.query_row("select value from meta where key = ?", [self.key], |r| r.get(0)).optional()?;
        Ok(found.unwrap_or(i64::MIN))
    }

    /// One batch: `read` outside the transaction, then `write` with the cursor committed beside it.
    /// False when the shard is through.
    pub fn step<T>(
        &mut self,
        read: impl Fn(&Connection, i64) -> rusqlite::Result<Vec<(i64, T)>>,
        write: impl Fn(&Connection, &[(i64, T)]) -> rusqlite::Result<()>,
    ) -> anyhow::Result<bool> {
        if self.stop.load(Relaxed) {
            anyhow::bail!("stopped: run it again to go on from here");
        }
        let cursor = self.cursor()?;
        let batch = read(self.conn, cursor)?;
        let Some(last) = batch.last().map(|(id, _)| *id) else { return Ok(false) };
        let tx = self.conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        write(&tx, &batch)?;
        tx.execute("insert or replace into meta (key, value) values (?, ?)", params![self.key, last])?;
        tx.commit()?;

        let commits = self.commits.fetch_add(1, Relaxed) + 1;
        let before = self.rows.fetch_add(batch.len() as u64, Relaxed);
        if before / PROGRESS_EVERY != (before + batch.len() as u64) / PROGRESS_EVERY {
            println!("{}: {} rows read", self.label, before + batch.len() as u64);
        }
        if self.limits.stop_after.is_some_and(|n| commits >= n) {
            anyhow::bail!("stopped: run it again to go on from here");
        }
        if commits.is_multiple_of(u64::from(self.limits.check_every)) {
            batch::checkpoint_or_stop(self.conn, self.path, self.limits.wal_limit)?;
        }
        Ok(true)
    }
}

/// a shard's cursor gone, and its WAL folded in
pub(crate) fn finish_shard(conn: &Connection, keys: &str) -> anyhow::Result<()> {
    conn.execute_batch(&format!("delete from meta where key in ({keys})"))?;
    let _: i64 = conn.query_row("pragma wal_checkpoint(truncate)", [], |r| r.get(0)).unwrap_or(1);
    Ok(())
}

pub(crate) fn main_path() -> PathBuf {
    paths::database()
}
