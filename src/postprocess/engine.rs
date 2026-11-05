//! The two lanes: database steps never wait on the network, and body and
//! outside steps run within their budget. All writes go through the shard's
//! writer, and a batch is one job, soo a crash repeats it whole.

use std::collections::HashSet;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering::Relaxed};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use rusqlite::{Connection, OptionalExtension, Row as SqlRow, params};
use serde_json::Value;

use crate::config::{Config, PostprocessConfig};
use crate::db;
use crate::indexer::Db;
use crate::profile::LOAD;
use crate::store::{self, SHARDS};

use super::StageParams;
use super::apply::{Applied, Lane, Renames, apply_one, count_names};
use super::bodies::{BodyFetcher, TokenBucket};
use super::rules::{self, BodyGate, FileSummary, RowFacts, Rules, Verdict};
use super::stats::{self, Stats};
use super::step::{NameChange, Needs, Outcome, ReleaseInfo, Step, StepCtx, StepError, StepMask, mask};

const DB_BATCH: i64 = 256;
const WALK_BATCH: i64 = 256;
pub(crate) const NET_BATCH: i64 = 4;
/// The longest the net lane rests after transient failures.
const NET_PAUSE_MAX: i64 = 60;
/// Transient failures in a row that are just bad articles, not a server in trouble.
const NET_PAUSE_AFTER: i64 = 20;

/// Seconds of rest after `in_a_row` transient failures: none at first, then
/// doubling.
fn net_pause_secs(in_a_row: i64) -> i64 {
    if in_a_row < NET_PAUSE_AFTER {
        return 0;
    }
    1i64.checked_shl((in_a_row - NET_PAUSE_AFTER).min(6) as u32).unwrap_or(NET_PAUSE_MAX).min(NET_PAUSE_MAX)
}
pub(crate) const NET_CRASH_RETRY: i64 = 900;
pub(crate) const STEP_DEADLINE: Duration = Duration::from_secs(120);

/// Transient failures in a row of a release's net step (every server timed out on its body, say) retried after 5
/// minutes; past these they back off like a retry (`backoff`). They were retried every 5 minutes for good, asking
/// every server again each time.
const TRANSIENT_FREE: i64 = 3;

/// Seconds until a net step runs again after its `in_a_row`th transient failure. They are counted apart from
/// `net_attempts` (in `net_transient`), soo they never spend the retries `max_attempts` allows, and never fail a
/// release.
fn transient_wait(in_a_row: i64, release_id: i64) -> i64 {
    if in_a_row <= TRANSIENT_FREE { 300 } else { backoff(in_a_row - TRANSIENT_FREE, release_id) }
}

/// seconds until an attempt's retry, and a little per release soo they dont bunch
fn backoff(attempt: i64, release_id: i64) -> i64 {
    let shift = (attempt.max(1) - 1).min(16) as u32;
    (900i64.saturating_mul(4i64.saturating_pow(shift))).min(604_800) + release_id.rem_euclid(60)
}

struct Settings {
    full: Config,
    config: PostprocessConfig,
    rules: Rules,
    /// enabled, and for a body step only with a budget
    enabled: StepMask,
}

pub(crate) struct Inner {
    main: PathBuf,
    /// by rank
    steps: Vec<Arc<dyn Step>>,
    settings: RwLock<Settings>,
    pub(crate) stop: Arc<AtomicBool>,
    db_busy: [AtomicBool; SHARDS],
    /// per shard: what its running net claims hold, `net_claims_per_shard` claims at most
    net_claimed: [Mutex<NetClaims>; SHARDS],
    fetcher: BodyFetcher,
    stats: Arc<Stats>,
    /// no net claims before this
    net_paused_until: AtomicI64,
    /// transient failures since the last body that worked
    net_troubles: AtomicI64,
    /// per shard: every 8th claim of a shard takes its oldest
    claims: [AtomicI64; SHARDS],
    /// per shard: queue batches taken, for `queue_order`
    queue_batches: [AtomicI64; SHARDS],
    tokens: Mutex<Vec<TokenBucket>>,
}

/// A release and its state, as read.
struct Row {
    info: ReleaseInfo,
    has_state: bool,
    done: StepMask,
    failed: StepMask,
    db_attempts: i64,
    net_attempts: i64,
    parts_seen: i64,
    rerun_gen: i64,
    /// transient net failures in a row
    net_transient: i64,
}

const ROW_COLUMNS: &str = "r.id, r.name, r.display_name, r.group_name, r.poster, r.posted_date, coalesce(r.size, 0),
    coalesce(r.complete, 0), coalesce(r.parts, 0), r.file_total, coalesce(r.is_obfuscated, 0), r.name_source,
    s.release_id is not null, coalesce(s.done, 0), coalesce(s.failed, 0), coalesce(s.db_attempts, 0),
    coalesce(s.net_attempts, 0), coalesce(s.parts_seen, 0), coalesce(s.rerun_gen, 0), coalesce(s.net_transient, 0)";

fn read_row(r: &SqlRow<'_>, at: usize, shard: usize) -> rusqlite::Result<Row> {
    let col = |i: usize| at + i;
    Ok(Row {
        info: ReleaseInfo {
            id: r.get(col(0))?,
            shard,
            name: r.get::<_, Option<String>>(col(1))?.unwrap_or_default(),
            display_name: r.get(col(2))?,
            group: r.get::<_, Option<String>>(col(3))?.unwrap_or_default(),
            poster: r.get(col(4))?,
            posted_date: r.get(col(5))?,
            size: r.get(col(6))?,
            complete: r.get::<_, i64>(col(7))? != 0,
            parts: r.get(col(8))?,
            file_total: r.get(col(9))?,
            is_obfuscated: r.get::<_, i64>(col(10))? != 0,
            name_source: r.get(col(11))?,
        },
        has_state: r.get(col(12))?,
        done: r.get(col(13))?,
        failed: r.get(col(14))?,
        db_attempts: r.get(col(15))?,
        net_attempts: r.get(col(16))?,
        parts_seen: r.get(col(17))?,
        rerun_gen: r.get(col(18))?,
        net_transient: r.get(col(19))?,
    })
}

fn facts(row: &Row) -> RowFacts<'_> {
    RowFacts {
        group: &row.info.group,
        name: &row.info.name,
        size: row.info.size,
        parts: row.info.parts,
        complete: row.info.complete,
        file_total: row.info.file_total,
    }
}

fn panic_text(payload: Box<dyn std::any::Any + Send>) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "panicked".into())
}

/// The connections of one lane thread, one per shard, opened when first needed.
struct Conns {
    main: PathBuf,
    open: Vec<Option<Connection>>,
}

impl Conns {
    fn new(main: PathBuf) -> Conns {
        Conns { main, open: (0..SHARDS).map(|_| None).collect() }
    }

    fn get(&mut self, shard: usize) -> rusqlite::Result<&Connection> {
        if self.open[shard].is_none() {
            let conn = db::open_shard(&store::shard_path(&self.main, shard))?;
            conn.execute_batch("pragma query_only = on")?;
            self.open[shard] = Some(conn);
        }
        Ok(self.open[shard].as_ref().unwrap())
    }
}

/// A batch's results, each visit holding what its outcomes asked to keep until the commit.
#[derive(Default)]
struct Batch {
    applied: Vec<Applied>,
    holds: Vec<Box<dyn std::any::Any + Send>>,
    /// the last release that got its visit: stopping can leave the rest of a batch
    through: Option<i64>,
    /// a step said it's stopping
    stopped: bool,
}

/// One run of a step, a panic being an error like any other, with its numbers.
fn run_step(step: &dyn Step, ctx: &mut StepCtx<'_>) -> Ran {
    let span = crate::metrics::span(crate::metrics::step_function(step.name()));
    let ran = match catch_unwind(AssertUnwindSafe(|| step.run(ctx))) {
        Ok(Ok(mut outcome)) => {
            // the CRCs of the articles it read go with whatever it found, waiting or done
            outcome.writes.append(&mut ctx.crcs);
            Ran::Outcome(outcome)
        }
        Ok(Err(e)) => {
            ctx.crcs.clear();
            Ran::Error(e)
        }
        Err(panic) => {
            ctx.crcs.clear();
            Ran::Error(StepError::Retry(panic_text(panic)))
        }
    };
    let result = match &ran {
        Ran::Outcome(o) if o.wait_until.is_some() => "waiting",
        Ran::Outcome(o) if o.matched => "matched",
        Ran::Outcome(_) => "unmatched",
        Ran::Error(StepError::Stopped) => "stopped",
        Ran::Error(StepError::OutOfBudget) => "out_of_budget",
        Ran::Error(StepError::Retry(_)) => "retry",
        Ran::Error(StepError::Permanent(_)) => "permanent",
        Ran::Error(StepError::Transient(_)) => "transient",
    };
    if matches!(result, "retry" | "permanent" | "transient") {
        span.failed();
    }
    crate::metrics::names::STEP_RUNS.inc(&[("step", step.name()), ("result", result)], 1);
    ran
}

/// How a step's run changed a visit.
enum Ran {
    /// a result, or an error the lane acts on
    Outcome(Outcome),
    Error(StepError),
}

impl Inner {
    pub(crate) fn new(p: &StageParams) -> Inner {
        let mut steps = p.steps.clone();
        steps.sort_by_key(|s| s.rank());
        let stop = Arc::new(AtomicBool::new(false));
        let stats = Arc::new(Stats::default());
        let config = p.config.postprocess.clone();
        let fetcher = BodyFetcher::new(p.pool.clone(), p.handle.clone(), stop.clone(), config.clone(), stats.clone());
        let inner = Inner {
            main: p.main.clone(),
            steps,
            settings: RwLock::new(Settings { full: p.config.clone(), rules: Rules::new(&config), config, enabled: 0 }),
            stop,
            db_busy: std::array::from_fn(|_| AtomicBool::new(false)),
            net_claimed: std::array::from_fn(|_| Mutex::new(NetClaims::default())),
            fetcher,
            stats,
            net_paused_until: AtomicI64::new(0),
            net_troubles: AtomicI64::new(0),
            claims: std::array::from_fn(|_| AtomicI64::new(0)),
            queue_batches: std::array::from_fn(|_| AtomicI64::new(0)),
            tokens: Mutex::new((0..SHARDS).map(|_| TokenBucket::new()).collect()),
        };
        inner.set_config(&p.config);
        inner
    }

    pub(crate) fn set_config(&self, full: &Config) {
        let config = full.postprocess.clone();
        store::SETTLE_SECS.store(config.settle_secs.max(1), Relaxed);
        store::COMPLETE_SETTLE_SECS.store(config.complete_settle_secs.max(1), Relaxed);
        let mut enabled = 0;
        for step in &self.steps {
            let on = step.enabled(full);
            let has_budget = step.needs() != Needs::Bodies || config.body_fetches_per_minute > 0;
            if on && has_budget {
                enabled |= mask(step.id());
            }
        }
        self.fetcher.set_config(&config);
        let mut s = self.settings.write().unwrap();
        s.rules = Rules::new(&config);
        s.full = full.clone();
        s.config = config;
        s.enabled = enabled;
    }

    pub(crate) fn db_workers(&self) -> usize {
        self.settings.read().unwrap().config.workers.max(1)
    }

    pub(crate) fn net_workers(&self) -> usize {
        self.settings.read().unwrap().config.body_connections.max(1)
    }

    pub(crate) fn has_net_steps(&self) -> bool {
        self.steps.iter().any(|s| s.needs() != Needs::Database)
    }

    fn stopping(&self) -> bool {
        self.stop.load(Relaxed)
    }

    fn nap(&self, how_long: Duration) {
        let until = Instant::now() + how_long;
        while !self.stopping() && Instant::now() < until {
            std::thread::sleep(how_long.min(Duration::from_millis(50)));
        }
    }

    fn enabled(&self) -> StepMask {
        self.settings.read().unwrap().enabled
    }

    fn step_mask(&self, wanted: impl Fn(&dyn Step) -> bool) -> StepMask {
        self.steps.iter().filter(|s| wanted(s.as_ref())).fold(0, |m, s| m | mask(s.id()))
    }

    /// Note which steps are on, in each shard: the first time one backlog walk,
    /// and later a walk for a step that came on or a stamp that gained a bit.
    pub(crate) fn seed(&self, db: &Db) {
        let enabled = self.enabled();
        let stamps: Vec<(u8, StepMask, i64)> = self
            .steps
            .iter()
            .filter(|s| enabled & mask(s.id()) != 0)
            .map(|s| (s.id().0, mask(s.id()), s.config_stamp() as i64))
            .collect();
        let now = chrono::Utc::now().timestamp();
        let answers: Vec<_> = (0..SHARDS)
            .filter_map(|shard| {
                let stamps = stamps.clone();
                db.on_writer(shard, "db.stage.seed", Box::new(move |conn| seed_shard(conn, enabled, &stamps, now))).ok()
            })
            .collect();
        for answer in answers {
            if !matches!(answer.recv(), Ok(Ok(()))) {
                self.stats.errors.fetch_add(1, Relaxed);
            }
        }
    }

    pub(crate) fn snapshot(&self) -> Value {
        let s = self.settings.read().unwrap();
        self.stats.snapshot(s.config.body_fetches_per_minute, s.config.backlog_per_second, s.enabled)
    }

    /// Bounded counts, per shard: an exact count at this size stalls whatever asks.
    pub(crate) fn refresh_counts(&self) {
        let now = chrono::Utc::now().timestamp();
        let (mut due, mut net, mut failed, mut walked, mut total, mut over_quota) = (0, 0, 0, 0, 0, 0);
        for path in store::shard_paths(&self.main) {
            let Ok(conn) = db::open_shard(&path) else { continue };
            let count =
                |sql: &str, p: &[&dyn rusqlite::ToSql]| -> i64 { conn.query_row(sql, p, |r| r.get(0)).unwrap_or(0) };
            due += count("select count(*) from (select 1 from pp_queue where due_at <= ?1 limit 100001)", &[&now]);
            net += count("select count(*) from (select 1 from pp_state where net_wanted = 1 limit 1000001)", &[]);
            failed += count("select count(*) from (select 1 from pp_state where failed != 0 limit 100001)", &[]);
            walked += count("select coalesce(sum(top_id - before_id), 0) from pp_walks", &[]);
            total += count("select coalesce(sum(top_id), 0) from pp_walks", &[]);
            over_quota += count("select coalesce(sum(value), 0) from meta where key = 'archive_names_over_quota'", &[]);
        }
        let s = &self.stats;
        s.queue_due.store(due, Relaxed);
        s.net_wanted.store(net, Relaxed);
        s.failed.store(failed, Relaxed);
        s.walk_done.store(walked, Relaxed);
        s.walk_total.store(total, Relaxed);
        s.names_over_quota.store(over_quota, Relaxed);
        stats::set_backlog(due + net);
    }

    // ------------------------------------------------------------ the database lane

    /// One thread: take a shard no other thread is on, one queue batch and one
    /// walk tick, and on to the next.
    pub(crate) fn db_lane(self: Arc<Self>, db: Db) {
        let mut conns = Conns::new(self.main.clone());
        while !self.stopping() {
            let mut worked = false;
            for shard in 0..SHARDS {
                if self.stopping() {
                    break;
                }
                if self.db_busy[shard].swap(true, Relaxed) {
                    continue;
                }
                match self.pass(&mut conns, &db, shard) {
                    Ok(did) => worked |= did,
                    Err(e) => {
                        self.stats.errors.fetch_add(1, Relaxed);
                        println!("processing stage, shard {shard}: {e}");
                        self.nap(Duration::from_secs(5));
                    }
                }
                self.db_busy[shard].store(false, Relaxed);
            }
            if !worked {
                self.nap(Duration::from_millis(100));
            }
        }
    }

    fn pass(&self, conns: &mut Conns, db: &Db, shard: usize) -> anyhow::Result<bool> {
        let conn = conns.get(shard)?;
        let queued = self.queue_batch(conn, db, shard)?;
        let walked = self.walk_tick(conn, db, shard)?;
        Ok(queued || walked)
    }

    fn queue_batch(&self, conn: &Connection, db: &Db, shard: usize) -> anyhow::Result<bool> {
        let now = chrono::Utc::now().timestamp();
        let order = queue_order(self.queue_batches[shard].fetch_add(1, Relaxed));
        let rows: Vec<(i64, i64, Option<Row>)> = conn
            .prepare_cached(&format!(
                "select q.release_id, q.due_at, r.id is not null, {ROW_COLUMNS}
                 from (select release_id, due_at from pp_queue where due_at <= ?1 order by {order} limit ?2) q
                 left join releases r on r.id = q.release_id
                 left join pp_state s on s.release_id = q.release_id"
            ))?
            .query_map(params![now, DB_BATCH], |r| {
                let there: bool = r.get(2)?;
                Ok((r.get(0)?, r.get(1)?, if there { Some(read_row(r, 3, shard)?) } else { None }))
            })?
            .collect::<rusqlite::Result<_>>()?;
        if rows.is_empty() {
            return Ok(false);
        }
        self.stats.lanes.queue_taken[shard].fetch_add(rows.len() as u64, Relaxed);
        let mut batch = Batch::default();
        let mut gone = Vec::new();
        let (present, missing): (Vec<_>, Vec<_>) = rows.into_iter().partition(|(_, _, row)| row.is_some());
        for (id, due, _) in missing {
            gone.push((id, due));
        }
        let visits: Vec<(Row, Option<i64>)> =
            present.into_iter().map(|(_, due, row)| (row.unwrap(), Some(due))).collect();
        self.visit_all(conn, visits, 0, now, &mut batch)?;
        self.send(db, shard, "db.stage.apply_queue", batch, now, move |tx| {
            for (id, due) in gone {
                tx.prepare_cached("delete from pp_queue where release_id = ? and due_at = ?")?
                    .execute(params![id, due])?;
            }
            Ok(())
        })?;
        Ok(true)
    }

    fn walk_tick(&self, conn: &Connection, db: &Db, shard: usize) -> anyhow::Result<bool> {
        let walk: Option<(i64, i64, i64, Option<String>)> = conn
            .prepare_cached("select id, before_id, redo_mask, group_name from pp_walks order by id desc limit 1")?
            .query_row([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .optional()?;
        let Some((walk_id, before, redo, group)) = walk else { return Ok(false) };
        if LOAD.writer_queued.load(Relaxed) > 2 * SHARDS as u64 {
            return Ok(false);
        }
        let per_second = self.settings.read().unwrap().config.backlog_per_second as f64 / SHARDS as f64;
        let mut limit = 0;
        {
            let mut tokens = self.tokens.lock().unwrap();
            while limit < WALK_BATCH {
                match tokens[shard].take(per_second, WALK_BATCH as f64, Instant::now()) {
                    Some(wait) if wait.is_zero() => limit += 1,
                    _ => break,
                }
            }
        }
        if limit == 0 {
            return Ok(false);
        }
        let sql = format!(
            "select {ROW_COLUMNS} from releases r left join pp_state s on s.release_id = r.id
             where r.id < ?1 {} order by r.id desc limit ?2",
            if group.is_some() { "and r.group_name = ?3" } else { "" }
        );
        let mut stmt = conn.prepare_cached(&sql)?;
        let rows: Vec<Row> = match &group {
            Some(g) => stmt
                .query_map(params![before, limit, g], |r| read_row(r, 0, shard))?
                .collect::<rusqlite::Result<_>>()?,
            None => {
                stmt.query_map(params![before, limit], |r| read_row(r, 0, shard))?.collect::<rusqlite::Result<_>>()?
            }
        };
        drop(stmt);
        let now = chrono::Utc::now().timestamp();
        if rows.is_empty() {
            self.send(db, shard, "db.stage.apply_walk", Batch::default(), now, move |tx| {
                tx.execute("delete from pp_walks where id = ?", [walk_id])?;
                Ok(())
            })?;
            return Ok(true);
        }
        let mut batch = Batch::default();
        let order: Vec<i64> = rows.iter().map(|row| row.info.id).collect();
        // a row with a time of its own (a retry, settling) has the queue to bring it back. Only
        // an explicit request runs it sooner: a walk mustnt use up attempts a backoff is spacing
        let mut waiting = conn.prepare_cached("select 1 from pp_queue where release_id = ? and due_at > ?")?;
        let mut visits: Vec<(Row, Option<i64>)> = Vec::with_capacity(rows.len());
        for row in rows {
            if redo != 0 || !waiting.exists(params![row.info.id, now])? {
                visits.push((row, None));
            }
        }
        drop(waiting);
        self.visit_all(conn, visits, redo, now, &mut batch)?;
        // the cursor goes no further than the visits did: nothing else brings a walked row back
        let finished = !batch.stopped && !self.stopping();
        let Some(next) = (if finished { order.last().copied() } else { batch.through }) else {
            self.send(db, shard, "db.stage.apply_walk", batch, now, |_| Ok(()))?;
            return Ok(true);
        };
        let seen = order.iter().position(|id| *id == next).map_or(0, |at| at as i64 + 1);
        let ran = batch.applied.len() as i64;
        self.stats.rows_walked.fetch_add(seen as u64, Relaxed);
        self.stats.lanes.walk_rows[shard].fetch_add(seen as u64, Relaxed);
        self.send(db, shard, "db.stage.apply_walk", batch, now, move |tx| {
            tx.prepare_cached("update pp_walks set before_id = ?, seen = seen + ?, ran = ran + ? where id = ?")?
                .execute(params![next, seen, ran, walk_id])?;
            Ok(())
        })?;
        Ok(true)
    }

    /// One job for the shard's writer: each release in its savepoint, and `also`
    /// with them. Waits for the answer, and what the outcomes hold goes with it.
    /// `kind`: which lane's results these are, for the writer's timing (`db.stage.apply_*`)
    fn send(
        &self,
        db: &Db,
        shard: usize,
        kind: &'static str,
        batch: Batch,
        now: i64,
        also: impl FnOnce(&Connection) -> rusqlite::Result<()> + Send + 'static,
    ) -> anyhow::Result<()> {
        let Batch { applied, holds, .. } = batch;
        let max_attempts = self.settings.read().unwrap().config.max_attempts;
        let written = Arc::new(Mutex::new(Renames::new()));
        let filling = written.clone();
        let answer = db.on_writer(
            shard,
            kind,
            Box::new(move |conn| {
                let mut renames = filling.lock().unwrap();
                for a in applied {
                    let id = a.release_id;
                    apply_one(conn, a, now, &|attempt| (backoff(attempt, id), attempt >= max_attempts), &mut renames)?;
                }
                also(conn)
            }),
        )?;
        let answered = answer.recv()?.map_err(anyhow::Error::msg);
        // the answer comes after the commit: a rolled back name isnt counted, nor its retry twice
        if answered.is_ok() {
            count_names(&written.lock().unwrap());
        }
        drop(holds);
        answered
    }

    /// A net lane's batch, counted for its shard's rates once it's written (see `stats::Lanes`).
    fn send_net(&self, db: &Db, shard: usize, batch: Batch, now: i64) -> anyhow::Result<()> {
        let visits = batch.applied.len() as u64;
        // a step that failed for good leaves its bit in the mask, but not in what's left (see `apply`)
        let finished = batch.applied.iter().filter(|a| a.net_mask & !(a.set_done | a.set_failed) == 0).count() as u64;
        self.send(db, shard, "db.stage.apply_net", batch, now, |_| Ok(()))?;
        let lanes = &self.stats.lanes;
        lanes.net_visits[shard].fetch_add(visits, Relaxed);
        lanes.net_finished[shard].fetch_add(finished, Relaxed);
        Ok(())
    }

    fn visit_all(
        &self,
        conn: &Connection,
        visits: Vec<(Row, Option<i64>)>,
        redo: StepMask,
        now: i64,
        batch: &mut Batch,
    ) -> anyhow::Result<()> {
        let rules = self.settings.read().unwrap().rules.clone();
        // the rows that get past the cheap tests have their files read
        let wanted: Vec<i64> = visits
            .iter()
            .filter(|(row, _)| rules::judge_row(&rules, &facts(row)).is_none())
            .map(|(row, _)| row.info.id)
            .collect();
        let mut summaries = rules::file_summaries(conn, &wanted)?;
        for (row, due) in visits {
            if self.stopping() {
                break;
            }
            let id = row.info.id;
            let summary = summaries.remove(&id);
            if let Some(a) = self.visit(conn, row, summary, due, redo, now, &rules, batch) {
                batch.applied.push(a);
            }
            if batch.stopped {
                break;
            }
            batch.through = Some(id);
        }
        Ok(())
    }

    /// Judge a release and run its database steps. None: nothing to write.
    #[allow(clippy::too_many_arguments)]
    fn visit(
        &self,
        conn: &Connection,
        row: Row,
        summary: Option<FileSummary>,
        due: Option<i64>,
        redo: StepMask,
        now: i64,
        rules: &Rules,
        batch: &mut Batch,
    ) -> Option<Applied> {
        let net_steps = self.step_mask(|s| s.needs() != Needs::Database);
        let db_mask = self.step_mask(|s| s.needs() == Needs::Database);
        let base = Applied {
            release_id: row.info.id,
            claimed_due: due,
            claimed_gen: row.rerun_gen,
            net_steps,
            db_mask,
            ..Default::default()
        };
        let reject = |reason: rules::Reason| {
            self.stats.rejected(reason);
            if reason == rules::Reason::Executable {
                return Some(Applied { junk: Some(crate::junk::EXECUTABLE), ..base_copy(&base) });
            }
            // what is stored can make a row junk whatever else rejects it (a failed read puts it off to the next visit)
            if let Ok(Some(junk)) = crate::junk::stored_reason(conn, row.info.id) {
                return Some(Applied { junk: Some(junk), ..base_copy(&base) });
            }
            // a queue row goes, a state row is no longer wanted by the net lane, a walk row writes nothing
            (due.is_some() || row.has_state).then(|| Applied { no_state: !row.has_state, ..base_copy(&base) })
        };
        if let Some(reason) = rules::judge_row(rules, &facts(&row)) {
            return reject(reason);
        }
        let summary = summary.unwrap_or_default();
        match rules::judge_files(rules, &facts(&row), &summary, now) {
            Verdict::Reject(reason) => return reject(reason),
            // a walk's request waits with it: the bits go now, soo the visit after settling runs them
            Verdict::Wait(until) => {
                let clear = if row.has_state { redo } else { 0 };
                return Some(Applied { queue_at: Some(until), clear, no_state: clear == 0, ..base_copy(&base) });
            }
            Verdict::Pass => {}
        }
        // what earlier runs stored can make it junk too: a program inside, or PC
        // (a read that fails only puts it off to the next visit)
        if let Ok(Some(reason)) = crate::junk::inner_or_pc(conn, row.info.id, summary.has_video) {
            return Some(Applied { junk: Some(reason), ..base_copy(&base) });
        }
        // a PC row from before there was no PC, on a release its video keeps: it goes uncategorized
        let stale_pc = crate::junk::stale_pc(conn, row.info.id, summary.has_video).unwrap_or(false);

        let enabled = self.enabled();
        let mut clear = redo;
        if row.has_state && row.info.parts > row.parts_seen {
            clear |= self.step_mask(|s| s.reruns().on_growth);
        }
        let (mut done, mut failed) = (row.done & !clear, row.failed & !clear);
        let mut a = base;
        if stale_pc {
            a.writes.push((0, super::step::StepWrite::new(crate::junk::DROP_CATEGORY, vec![row.info.id.into()])));
        }
        a.parts_seen = Some(row.info.parts);
        let mut ctx = StepCtx {
            release: row.info.clone(),
            conn,
            facts: Vec::new(),
            bodies: &self.fetcher,
            used_bodies: 0,
            deadline: Instant::now() + STEP_DEADLINE,
            lane_can_fetch: false,
            crcs: Vec::new(),
        };
        let mut failure = false;
        let mut queue_at: Option<i64> = None;
        let mut soonest = |at: i64| queue_at = Some(queue_at.map_or(at, |q: i64| q.min(at)));
        for (at, step) in self.steps.iter().enumerate() {
            let bit = mask(step.id());
            if step.needs() != Needs::Database
                || enabled & bit == 0
                || (done | failed) & bit != 0
                || !step.applies(&ctx.release)
            {
                continue;
            }
            ctx.deadline = Instant::now() + STEP_DEADLINE;
            let ran = run_step(step.as_ref(), &mut ctx);
            match ran {
                Ran::Outcome(mut o) => {
                    batch.holds.extend(o.hold.take());
                    if let Some(reason) = o.junk {
                        return Some(Applied { junk: Some(reason), ..base_copy(&a) });
                    }
                    if let Some(until) = o.wait_until {
                        // what it got as far as is kept, for the run that picks it up again
                        a.writes.extend(std::mem::take(&mut o.writes).into_iter().map(|w| (bit, w)));
                        soonest(until);
                        continue;
                    }
                    self.count_done(&o);
                    a.set_done |= bit;
                    done |= bit;
                    failed &= !bit;
                    a.writes.extend(std::mem::take(&mut o.writes).into_iter().map(|w| (bit, w)));
                    ctx.facts.append(&mut o.facts);
                    let later = |s: &dyn Step| s.rank() > step.rank();
                    // later database steps run now, on what this one found; net ones go back to their lane
                    let rerun = o.rerun & self.steps[at + 1..].iter().fold(0, |m, s| m | mask(s.id()));
                    clear |= rerun;
                    done &= !rerun;
                    failed &= !rerun;
                    if let Some(name) = o.name.take() {
                        if name_counts(&ctx.release, &name) {
                            let renamed = name
                                .display_name
                                .as_ref()
                                .is_some_and(|d| ctx.release.display_name.as_ref() != Some(d));
                            apply_name(&mut ctx.release, &name);
                            if renamed {
                                let on_rename = self.step_mask(|s| later(s) && s.reruns().on_rename);
                                done &= !on_rename;
                                failed &= !on_rename;
                                clear |= on_rename & db_mask;
                                a.rename_clears |= on_rename & net_steps;
                            }
                        }
                        a.names.push((bit, name));
                    }
                }
                Ran::Error(StepError::Stopped) => {
                    batch.stopped = true;
                    return None;
                }
                Ran::Error(StepError::Retry(e)) => {
                    self.stats.retries.fetch_add(1, Relaxed);
                    let attempt = row.db_attempts + 1;
                    let max = self.settings.read().unwrap().config.max_attempts;
                    if attempt >= max {
                        a.set_failed |= bit;
                        failed |= bit;
                        a.db_attempts = Some(0);
                    } else {
                        a.db_attempts = Some(attempt);
                        soonest(now + backoff(attempt, row.info.id));
                    }
                    a.error = Some(e);
                    failure = true;
                }
                Ran::Error(StepError::Permanent(e)) => {
                    a.set_failed |= bit;
                    failed |= bit;
                    a.error = Some(e);
                }
                Ran::Error(StepError::Transient(e)) => {
                    soonest(now + 300);
                    a.error = Some(e);
                }
                Ran::Error(StepError::OutOfBudget) => soonest(now + 60),
            }
        }
        if !failure {
            a.db_attempts = Some(0);
        }

        // net steps: the body gate here, the run in their lane
        for step in &self.steps {
            let bit = mask(step.id());
            if step.needs() == Needs::Database
                || enabled & bit == 0
                || (done | failed) & bit != 0
                || !step.applies(&ctx.release)
            {
                continue;
            }
            if step.needs() == Needs::Bodies {
                match rules::body_gate(rules, &facts(&row), &summary) {
                    BodyGate::Outside => {
                        a.set_done |= bit;
                        done |= bit;
                        self.stats.unmatched.fetch_add(1, Relaxed);
                        continue;
                    }
                    BodyGate::NotYet => continue,
                    BodyGate::Open => {}
                }
            }
            a.net_mask |= bit;
        }

        // a bit cleared and run again ended as its run says
        a.clear = clear & !(a.set_done | a.set_failed);
        a.queue_at = queue_at;
        if a.clear & net_steps != 0 {
            // run again from the start: the streak was the old run's
            a.net_next_at = Some(0);
            a.net_transient = Some(0);
        }
        a.no_state = !row.has_state
            && a.set_done == 0
            && a.set_failed == 0
            && a.clear == 0
            && a.rename_clears == 0
            && a.net_mask == 0
            && a.error.is_none()
            && a.names.is_empty();
        Some(a)
    }

    fn count_done(&self, o: &Outcome) {
        self.stats.steps_done.fetch_add(1, Relaxed);
        if o.matched {
            self.stats.matched.fetch_add(1, Relaxed);
        } else {
            self.stats.unmatched.fetch_add(1, Relaxed);
        }
    }

    // ------------------------------------------------------------ the net lane

    /// A server in trouble gets a rest, but one article that failed shouldn't stop
    /// every release: the rest grows with failures in a row, up to `NET_PAUSE_MAX`.
    fn pause_net_after_trouble(&self, now: i64) {
        let in_a_row = self.net_troubles.fetch_add(1, Relaxed) + 1;
        let secs = net_pause_secs(in_a_row);
        if secs > 0 {
            self.net_paused_until.store(now + secs, Relaxed);
        }
    }

    pub(crate) fn net_lane(self: Arc<Self>, db: Db) {
        let mut conns = Conns::new(self.main.clone());
        while !self.stopping() {
            if chrono::Utc::now().timestamp() < self.net_paused_until.load(Relaxed) {
                self.nap(Duration::from_millis(250));
                continue;
            }
            let net_on = self.enabled() & self.step_mask(|s| s.needs() != Needs::Database) != 0;
            let mut worked = false;
            if net_on {
                for shard in 0..SHARDS {
                    if self.stopping() {
                        break;
                    }
                    let result =
                        conns.get(shard).map_err(anyhow::Error::from).and_then(|conn| self.net_claim(conn, &db, shard));
                    match result {
                        Ok(did) => worked |= did,
                        Err(e) => {
                            self.stats.errors.fetch_add(1, Relaxed);
                            println!("processing stage, shard {shard}: {e}");
                            self.nap(Duration::from_secs(5));
                        }
                    }
                }
            }
            if !worked {
                self.nap(Duration::from_millis(100));
            }
        }
    }

    fn net_claim(&self, conn: &Connection, db: &Db, shard: usize) -> anyhow::Result<bool> {
        let now = chrono::Utc::now().timestamp();
        let order = claim_order(self.claims[shard].load(Relaxed));
        let most = self.settings.read().unwrap().config.net_claims_per_shard;
        let ids = net_take(conn, &self.net_claimed[shard], order, now, most)?;
        if ids.is_empty() {
            return Ok(false);
        }
        self.claims[shard].fetch_add(1, Relaxed);
        let mut result = Ok(());
        for id in &ids {
            if self.stopping() {
                break;
            }
            result = self.net_one(conn, db, shard, *id);
            if result.is_err() {
                break;
            }
        }
        self.net_claimed[shard].lock().unwrap().give_back(&ids);
        result.map(|()| true)
    }

    /// Gives each release of `shard` the net lane wants a `net_rank`, newest first, a slice at a time: rows from
    /// before ranks existed, and the few a rerun adds without a visit. A visit ranks its own release. One thread per
    /// shard, as each slice waits for its shard's writer and one busy writer shouldn't hold up the other shards.
    pub(crate) fn rank_lane(self: Arc<Self>, db: Db, shard: usize) {
        let mut conns = Conns::new(self.main.clone());
        while !self.stopping() {
            match conns.get(shard).map_err(anyhow::Error::from).and_then(|conn| rank_slice(conn, &db, shard)) {
                // another slice as soon as its writer is free: the writer puts saves first either way
                Ok(n) if n > 0 => {}
                Ok(_) => self.nap(Duration::from_secs(60)),
                Err(e) => {
                    println!("processing stage, ranking shard {shard}: {e}");
                    self.nap(Duration::from_secs(30));
                }
            }
        }
    }

    /// Run one net step of one release: its attempt is counted first, soo a release
    /// that kills the process cant do it past `max_attempts`.
    fn net_one(&self, conn: &Connection, db: &Db, shard: usize, id: i64) -> anyhow::Result<()> {
        let now = chrono::Utc::now().timestamp();
        let row: Option<Row> = conn
            .prepare_cached(&format!(
                "select {ROW_COLUMNS} from releases r left join pp_state s on s.release_id = r.id where r.id = ?1"
            ))?
            .query_row([id], |r| read_row(r, 0, shard))
            .optional()?;
        let Some(row) = row else { return Ok(()) };
        let (rules, max_attempts) = {
            let s = self.settings.read().unwrap();
            (s.rules.clone(), s.config.max_attempts)
        };
        let net_steps = self.step_mask(|s| s.needs() != Needs::Database);
        let db_mask = self.step_mask(|s| s.needs() == Needs::Database);
        let mut a = Applied {
            release_id: id,
            lane: Lane::Net,
            claimed_gen: row.rerun_gen,
            net_steps,
            db_mask,
            ..Default::default()
        };

        let verdict = match rules::judge_row(&rules, &facts(&row)) {
            Some(reason) => Verdict::Reject(reason),
            None => {
                let summary = rules::file_summaries(conn, &[id])?.remove(&id).unwrap_or_default();
                match rules::judge_files(&rules, &facts(&row), &summary, now) {
                    Verdict::Pass => {
                        return self.net_step(conn, db, shard, row, &rules, summary, a, max_attempts, now);
                    }
                    other => other,
                }
            }
        };
        match verdict {
            Verdict::Reject(reason) => {
                self.stats.rejected(reason);
                a.net_mask = 0;
                if reason == rules::Reason::Executable {
                    a.junk = Some(crate::junk::EXECUTABLE);
                } else if let Ok(Some(junk)) = crate::junk::stored_reason(conn, id) {
                    a.junk = Some(junk);
                }
            }
            // not settled: it stays wanted, and its lane comes back then
            Verdict::Wait(until) => {
                a.net_mask = net_steps;
                a.net_next_at = Some(until);
            }
            Verdict::Pass => {}
        }
        self.send_net(db, shard, one(a), now)
    }

    #[allow(clippy::too_many_arguments)]
    fn net_step(
        &self,
        conn: &Connection,
        db: &Db,
        shard: usize,
        row: Row,
        rules: &Rules,
        summary: FileSummary,
        mut a: Applied,
        max_attempts: i64,
        now: i64,
    ) -> anyhow::Result<()> {
        let id = row.info.id;
        let enabled = self.enabled();
        let gate = rules::body_gate(rules, &facts(&row), &summary);
        // what is left for this lane, lowest rank first
        let mut left: Vec<&Arc<dyn Step>> = Vec::new();
        for step in &self.steps {
            let bit = mask(step.id());
            if step.needs() == Needs::Database
                || enabled & bit == 0
                || (row.done | row.failed) & bit != 0
                || !step.applies(&row.info)
            {
                continue;
            }
            if step.needs() == Needs::Bodies {
                match gate {
                    BodyGate::Outside => {
                        a.set_done |= bit;
                        self.stats.unmatched.fetch_add(1, Relaxed);
                        continue;
                    }
                    BodyGate::NotYet => continue,
                    BodyGate::Open => {}
                }
            }
            left.push(step);
        }
        let Some(step) = left.first().copied() else {
            a.net_mask = 0;
            return self.send_net(db, shard, one(a), now);
        };
        let bit = mask(step.id());
        a.net_mask = left.iter().fold(0, |m, s| m | mask(s.id()));

        // unfinished runs left this: the process died in them
        if row.net_attempts >= max_attempts {
            a.set_failed |= bit;
            a.net_attempts = Some(0);
            a.net_transient = Some(0);
            a.net_next_at = Some(0);
            a.error = Some("interrupted".into());
            return self.send_net(db, shard, one(a), now);
        }

        let mut batch = Batch::default();
        let mut waiting: Option<i64> = None;
        for step in left {
            let bit = mask(step.id());
            // Begin, before anything can go wrong
            let begin_at = now + NET_CRASH_RETRY;
            let answer = db.on_writer(
                shard,
                "db.stage.net_begin",
                Box::new(move |tx| {
                    tx.prepare_cached(
                        "update pp_state set net_attempts = net_attempts + 1, net_next_at = ? where release_id = ?",
                    )?
                    .execute(params![begin_at, id])?;
                    Ok(())
                }),
            )?;
            answer.recv()?.map_err(anyhow::Error::msg)?;

            let mut ctx = StepCtx {
                release: row.info.clone(),
                conn,
                facts: Vec::new(),
                bodies: &self.fetcher,
                used_bodies: 0,
                deadline: Instant::now() + STEP_DEADLINE,
                lane_can_fetch: step.needs() == Needs::Bodies,
                crcs: Vec::new(),
            };
            let ran = run_step(step.as_ref(), &mut ctx);
            let now = chrono::Utc::now().timestamp();
            let attempts_before = row.net_attempts;
            match ran {
                Ran::Outcome(mut o) => {
                    self.net_troubles.store(0, Relaxed);
                    batch.holds.extend(o.hold.take());
                    if let Some(reason) = o.junk {
                        batch.applied.push(Applied { junk: Some(reason), ..base_copy(&a) });
                        return self.send_net(db, shard, batch, now);
                    }
                    if let Some(until) = o.wait_until {
                        a.writes.extend(std::mem::take(&mut o.writes).into_iter().map(|w| (bit, w)));
                        // what it waits for needn't hold up the steps after it
                        waiting = Some(waiting.map_or(until, |w: i64| w.min(until)));
                        a.net_attempts = Some(attempts_before);
                        a.net_next_at = Some(until);
                        continue;
                    } else {
                        self.count_done(&o);
                        a.set_done |= bit;
                        a.net_mask &= !bit;
                        a.net_attempts = Some(0);
                        a.net_transient = Some(0);
                        a.net_next_at = Some(0);
                        a.writes.extend(std::mem::take(&mut o.writes).into_iter().map(|w| (bit, w)));
                        // later steps run again: this lane's come back to it, the database's through the queue
                        let later =
                            self.steps.iter().filter(|s| s.rank() > step.rank()).fold(0, |m, s| m | mask(s.id()));
                        a.clear |= o.rerun & later;
                        a.net_mask |= o.rerun & later & a.net_steps;
                        if let Some(name) = o.name.take() {
                            let on_rename = self.step_mask(|s| s.rank() > step.rank() && s.reruns().on_rename);
                            a.rename_clears |= on_rename;
                            a.net_mask |= on_rename & a.net_steps;
                            a.names.push((bit, name));
                        }
                    }
                }
                Ran::Error(StepError::Stopped) => {
                    a.net_attempts = Some(attempts_before);
                    a.net_next_at = Some(now);
                }
                Ran::Error(StepError::Retry(e)) => {
                    self.stats.retries.fetch_add(1, Relaxed);
                    let attempt = attempts_before + 1;
                    if attempt >= max_attempts {
                        a.set_failed |= bit;
                        a.net_attempts = Some(0);
                        a.net_transient = Some(0);
                        a.net_next_at = Some(0);
                    } else {
                        a.net_attempts = Some(attempt);
                        a.net_transient = Some(0);
                        a.net_next_at = Some(now + backoff(attempt, id));
                    }
                    a.error = Some(e);
                }
                Ran::Error(StepError::Permanent(e)) => {
                    a.set_failed |= bit;
                    a.net_attempts = Some(0);
                    a.net_transient = Some(0);
                    a.net_next_at = Some(0);
                    a.error = Some(e);
                }
                Ran::Error(StepError::Transient(e)) => {
                    let in_a_row = row.net_transient + 1;
                    a.net_attempts = Some(attempts_before);
                    a.net_transient = Some(in_a_row);
                    a.net_next_at = Some(now + transient_wait(in_a_row, id));
                    a.error = Some(e);
                    self.pause_net_after_trouble(now);
                }
                Ran::Error(StepError::OutOfBudget) => {
                    a.net_attempts = Some(attempts_before);
                    a.net_next_at = Some(now + 60);
                }
            }
            break;
        }
        if let Some(until) = waiting {
            a.net_next_at = Some(match a.net_next_at {
                Some(at) if at != 0 => at.min(until),
                _ => until,
            });
        }
        batch.applied.push(a);
        self.send_net(db, shard, batch, now)
    }
}

fn one(a: Applied) -> Batch {
    Batch { applied: vec![a], ..Default::default() }
}

/// a fresh `Applied` with what identifies the visit
fn base_copy(a: &Applied) -> Applied {
    Applied {
        release_id: a.release_id,
        lane: a.lane,
        claimed_due: a.claimed_due,
        claimed_gen: a.claimed_gen,
        net_steps: a.net_steps,
        db_mask: a.db_mask,
        ..Default::default()
    }
}

/// whether this source may set the name at all
fn name_counts(release: &ReleaseInfo, name: &NameChange) -> bool {
    i64::from(name.source) >= release.name_source.unwrap_or(0)
        && (name.display_name.is_some() || name.obfuscated.is_some())
}

fn apply_name(release: &mut ReleaseInfo, name: &NameChange) {
    if let Some(d) = &name.display_name {
        release.display_name = Some(d.clone());
    }
    if let Some(o) = name.obfuscated {
        release.is_obfuscated = o;
    }
    release.name_source = Some(i64::from(name.source));
}

/// One shard's note of which steps are on, see `Inner::seed`. In the writer's transaction.
fn seed_shard(conn: &Connection, enabled: StepMask, stamps: &[(u8, StepMask, i64)], now: i64) -> rusqlite::Result<()> {
    let meta = |key: &str| -> rusqlite::Result<Option<i64>> {
        conn.prepare_cached("select value from meta where key = ?")?.query_row([key], |r| r.get(0)).optional()
    };
    let set = |key: &str, value: i64| -> rusqlite::Result<()> {
        conn.prepare_cached("insert or replace into meta (key, value) values (?, ?)")?.execute(params![key, value])?;
        Ok(())
    };
    let walk = |kind: &str, redo: StepMask| -> rusqlite::Result<()> {
        conn.prepare_cached(
            "insert into pp_walks (kind, top_id, before_id, redo_mask, created_at)
             select ?1, t, t, ?2, ?3 from (select coalesce(max(id), -1) + 1 as t from releases)",
        )?
        .execute(params![kind, redo, now])?;
        Ok(())
    };
    let seeded = meta("pp_walk_seeded")?.is_some();
    if !seeded {
        // it visits every row, soo no `steps` walk beside it
        walk("backlog", 0)?;
        set("pp_walk_seeded", now)?;
    }
    // once, for the rows from before junk was deleted: every visit looks for it (see `junk`)
    if meta("pp_junk_walk")?.is_none() {
        if seeded {
            walk("junk", 0)?;
        }
        set("pp_junk_walk", now)?;
    }
    // and the rows whose stored facts already say junk, through the queue at once: the walk takes days on a big shard
    if meta("pp_junk_queued")?.is_none() {
        let known = crate::junk::known_junk(conn)?;
        let mut queue = conn.prepare_cached(
            "insert into pp_queue (release_id, due_at) values (?1, ?2) on conflict(release_id) do update set due_at = ?2",
        )?;
        for id in known {
            queue.execute(params![id, now])?;
        }
        set("pp_junk_queued", now)?;
    }
    // once, for the movies and TV ranked before `SMALL_VIDEO` counted: ranked again, soo they leave the 0s
    if meta("pp_net_rank_small_video")?.is_none() {
        rerank_small_video(conn)?;
        set("pp_net_rank_small_video", now)?;
    }
    // once, for the probes from before a too big first article was read in part
    if meta("pp_probe_big_heads")?.is_none() {
        crate::probe::step::rerun_big_heads(conn, now)?;
        set("pp_probe_big_heads", now)?;
    }
    // once, for the AVI files probed before their header was read
    if meta("pp_probe_avi")?.is_none() {
        crate::probe::step::rerun_avi(conn, now)?;
        set("pp_probe_avi", now)?;
    }
    // once, for the WMV and ASF files probed before their header was read
    if meta("pp_probe_asf")?.is_none() {
        crate::probe::step::rerun_asf(conn, now)?;
        set("pp_probe_asf", now)?;
    }
    // once, for the MPEG program streams probed before their packs were read
    if meta("pp_probe_mpeg_ps")?.is_none() {
        crate::probe::step::rerun_mpeg_ps(conn, now)?;
        set("pp_probe_mpeg_ps", now)?;
    }
    // once, for the probes that read a set's later volume before its first was looked for
    if meta("pp_probe_first_volume")?.is_none() {
        crate::probe::step::rerun_later_volumes(conn, now)?;
        set("pp_probe_first_volume", now)?;
    }
    let seen = meta("pp_steps_seen")?.unwrap_or(0);
    let new = if seeded { enabled & !seen } else { enabled };
    if seeded && new != 0 {
        walk("steps", 0)?;
    }
    if !seeded || seen != enabled {
        set("pp_steps_seen", enabled)?;
    }
    for (id, bit, stamp) in stamps {
        let key = format!("pp_stamp_{id}");
        let stored = meta(&key)?;
        if seeded && new & bit == 0 && stored.is_some_and(|s| stamp & !s != 0) {
            walk("steps", *bit)?;
        }
        if stored != Some(*stamp) {
            set(&key, *stamp)?;
        }
    }
    Ok(())
}

/// Ranks again the movie and TV releases of `SMALL_VIDEO` or more the net lane still wants at 0, the rank they had
/// before it counted. Through the category index: a shard has tens of millions of 0s and ~150,000 movies and TV.
pub(crate) fn rerank_small_video(conn: &Connection) -> rusqlite::Result<usize> {
    let sql = format!(
        "update pp_state set net_rank = {} where net_wanted = 1 and net_rank = 0 and release_id in (
             select c.release_id from release_category c join releases r on r.id = c.release_id
             where (c.category between 2000 and 2999 or c.category between 5000 and 5999) and r.size >= {})",
        super::schema::NET_RANK.replace("?1", "pp_state.release_id"),
        super::schema::SMALL_VIDEO,
    );
    conn.execute(&sql, [])
}

/// The net lane's order for a shard's `claims`-th claim. Mostly by `net_rank`, biggest video first, as the owner
/// wants movies and TV most; every 8th claim the newest posts whatever their size, and every 8th the oldest, soo
/// neither waits on a backlog of big ones.
pub(crate) fn claim_order(claims: i64) -> &'static str {
    match claims % 8 {
        7 => "release_id asc",
        3 => "release_id desc",
        _ => "net_rank desc, release_id desc",
    }
}

/// The database lane's order for a shard's `batches`-th queue batch: mostly the latest due, soo a post just saved is
/// named, sorted and linked with its other parts within a minute however long the queue is; every 4th batch the
/// longest waiting, soo the backlog drains too.
pub(crate) fn queue_order(batches: i64) -> &'static str {
    if batches % 4 == 3 { "due_at" } else { "due_at desc" }
}

/// The share of a shard's net claims each order gets, as `claim_order` takes turns: (by rank, newest, oldest).
pub(crate) fn claim_shares() -> (f64, f64, f64) {
    let (mut rank, mut newest, mut oldest) = (0.0, 0.0, 0.0);
    for claims in 0..8 {
        match claim_order(claims) {
            "release_id asc" => oldest += 1.0,
            "release_id desc" => newest += 1.0,
            _ => rank += 1.0,
        }
    }
    (rank / 8.0, newest / 8.0, oldest / 8.0)
}

/// The share of a shard's queue batches each order gets, as `queue_order` takes turns: (latest due, longest waiting).
pub(crate) fn queue_shares() -> (f64, f64) {
    let latest = (0..4).filter(|b| queue_order(*b) == "due_at desc").count() as f64;
    (latest / 4.0, 1.0 - latest / 4.0)
}

/// A shard's running net claims, and the releases they hold.
#[derive(Default)]
pub(crate) struct NetClaims {
    held: HashSet<i64>,
    claims: usize,
}

impl NetClaims {
    /// what a claim took, given back when it's done
    pub(crate) fn give_back(&mut self, ids: &[i64]) {
        for id in ids {
            self.held.remove(id);
        }
        self.claims -= 1;
    }
}

/// The next `NET_BATCH` releases the net lane wants, in `order`, that no other claim of the shard holds, as a claim
/// of its own: none when `most` claims are running (`net_claims_per_shard`). They are held till `give_back`. They
/// were one at a time, then 8: each claim waits on its server most of the time, and 8 a shard left 448 of 512 net
/// workers asleep with the bandwidth a quarter used.
pub(crate) fn net_take(
    conn: &Connection,
    claimed: &Mutex<NetClaims>,
    order: &str,
    now: i64,
    most: usize,
) -> rusqlite::Result<Vec<i64>> {
    let mut claimed = claimed.lock().unwrap();
    if claimed.claims >= most.max(1) {
        return Ok(Vec::new());
    }
    let ids: Vec<i64> = conn
        .prepare_cached(&format!(
            "select release_id from pp_state where net_wanted = 1 and net_next_at <= ?1 order by {order} limit ?2"
        ))?
        .query_map(params![now, NET_BATCH + claimed.held.len() as i64], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let ids: Vec<i64> = ids.into_iter().filter(|id| !claimed.held.contains(id)).take(NET_BATCH as usize).collect();
    if !ids.is_empty() {
        claimed.held.extend(&ids);
        claimed.claims += 1;
    }
    Ok(ids)
}

/// Releases ranked per slice, at most.
const RANK_SLICE: i64 = 20_000;

/// Ranks up to `RANK_SLICE` of a shard's unranked releases, newest first, on its writer. Their ids are read here.
fn rank_slice(conn: &Connection, db: &Db, shard: usize) -> anyhow::Result<usize> {
    let ids: Vec<i64> = conn
        .prepare_cached(
            "select release_id from pp_state where net_wanted = 1 and net_rank is null order by release_id desc limit ?1",
        )?
        .query_map([RANK_SLICE], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let n = ids.len();
    if n == 0 {
        return Ok(0);
    }
    let answer = db.on_writer(shard, "db.stage.net_rank", Box::new(move |tx| rank_ids(tx, &ids)))?;
    answer.recv()?.map_err(anyhow::Error::msg)?;
    Ok(n)
}

/// Ranks these releases where they are still unranked.
pub(crate) fn rank_ids(conn: &Connection, ids: &[i64]) -> rusqlite::Result<()> {
    let sql = format!(
        "update pp_state set net_rank = {} where release_id = ?1 and net_rank is null",
        super::schema::NET_RANK
    );
    let mut update = conn.prepare_cached(&sql)?;
    for id in ids {
        update.execute([id])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn order_shares_follow_the_turns() {
        assert_eq!(claim_shares(), (0.75, 0.125, 0.125));
        assert_eq!(queue_shares(), (0.75, 0.25));
    }

    #[test]
    fn transient_failures_back_off_after_a_few() {
        let waits: Vec<i64> = (1..=7).map(|n| transient_wait(n, 0)).collect();
        assert_eq!(waits, [300, 300, 300, 900, 3600, 14400, 57600]);
        assert_eq!(transient_wait(40, 0), 604_800, "a week at most");
    }

    #[test]
    fn a_few_bad_articles_dont_rest_the_net_lane_but_a_run_of_them_does() {
        assert_eq!(net_pause_secs(1), 0);
        assert_eq!(net_pause_secs(NET_PAUSE_AFTER - 1), 0);
        assert_eq!(net_pause_secs(NET_PAUSE_AFTER), 1);
        assert_eq!(net_pause_secs(NET_PAUSE_AFTER + 3), 8);
        assert_eq!(net_pause_secs(10_000), NET_PAUSE_MAX);
    }
}
