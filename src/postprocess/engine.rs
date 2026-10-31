//! The two lanes: database steps never wait on the network, and body and
//! outside steps run within their budget. All writes go through the shard's
//! writer, and a batch is one job, soo a crash repeats it whole.

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
use super::apply::{Applied, Lane, apply_one};
use super::bodies::{BodyFetcher, TokenBucket};
use super::rules::{self, BodyGate, FileSummary, RowFacts, Rules, Verdict};
use super::stats::{self, Stats};
use super::step::{NameChange, Needs, Outcome, ReleaseInfo, Step, StepCtx, StepError, StepMask, mask};

const DB_BATCH: i64 = 256;
const WALK_BATCH: i64 = 256;
const NET_BATCH: i64 = 4;
const NET_CRASH_RETRY: i64 = 900;
const STEP_DEADLINE: Duration = Duration::from_secs(120);

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
    net_busy: [AtomicBool; SHARDS],
    fetcher: BodyFetcher,
    stats: Arc<Stats>,
    /// no net claims before this
    net_paused_until: AtomicI64,
    /// per shard: every 8th claim of a shard takes its oldest
    claims: [AtomicI64; SHARDS],
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
}

const ROW_COLUMNS: &str = "r.id, r.name, r.display_name, r.group_name, r.poster, r.posted_date, coalesce(r.size, 0),
    coalesce(r.complete, 0), coalesce(r.parts, 0), r.file_total, coalesce(r.is_obfuscated, 0), r.name_source,
    s.release_id is not null, coalesce(s.done, 0), coalesce(s.failed, 0), coalesce(s.db_attempts, 0),
    coalesce(s.net_attempts, 0), coalesce(s.parts_seen, 0), coalesce(s.rerun_gen, 0)";

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
            net_busy: std::array::from_fn(|_| AtomicBool::new(false)),
            fetcher,
            stats,
            net_paused_until: AtomicI64::new(0),
            claims: std::array::from_fn(|_| AtomicI64::new(0)),
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
                db.on_writer(shard, Box::new(move |conn| seed_shard(conn, enabled, &stamps, now))).ok()
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
        self.stats.snapshot(s.config.body_fetches_per_minute, s.config.backlog_per_second)
    }

    /// Bounded counts, per shard: an exact count at this size stalls whatever asks.
    pub(crate) fn refresh_counts(&self) {
        let now = chrono::Utc::now().timestamp();
        let (mut due, mut net, mut failed, mut left, mut total) = (0, 0, 0, 0, 0);
        for path in store::shard_paths(&self.main) {
            let Ok(conn) = db::open_shard(&path) else { continue };
            let count =
                |sql: &str, p: &[&dyn rusqlite::ToSql]| -> i64 { conn.query_row(sql, p, |r| r.get(0)).unwrap_or(0) };
            due += count("select count(*) from (select 1 from pp_queue where due_at <= ?1 limit 100001)", &[&now]);
            net += count("select count(*) from (select 1 from pp_state where net_wanted = 1 limit 1000001)", &[]);
            failed += count("select count(*) from (select 1 from pp_state where failed != 0 limit 100001)", &[]);
            left += count("select coalesce(sum(top_id - before_id), 0) from pp_walks", &[]);
            total += count("select coalesce(sum(top_id), 0) from pp_walks", &[]);
        }
        let s = &self.stats;
        s.queue_due.store(due, Relaxed);
        s.net_wanted.store(net, Relaxed);
        s.failed.store(failed, Relaxed);
        s.walk_left.store(left, Relaxed);
        s.walk_total.store(total, Relaxed);
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
        let rows: Vec<(i64, i64, Option<Row>)> = conn
            .prepare_cached(&format!(
                "select q.release_id, q.due_at, r.id is not null, {ROW_COLUMNS}
                 from (select release_id, due_at from pp_queue where due_at <= ?1 order by due_at limit ?2) q
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
        let mut batch = Batch::default();
        let mut gone = Vec::new();
        let (present, missing): (Vec<_>, Vec<_>) = rows.into_iter().partition(|(_, _, row)| row.is_some());
        for (id, due, _) in missing {
            gone.push((id, due));
        }
        let visits: Vec<(Row, Option<i64>)> =
            present.into_iter().map(|(_, due, row)| (row.unwrap(), Some(due))).collect();
        self.visit_all(conn, visits, 0, now, &mut batch)?;
        self.send(db, shard, batch, now, move |tx| {
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
            self.send(db, shard, Batch::default(), now, move |tx| {
                tx.execute("delete from pp_walks where id = ?", [walk_id])?;
                Ok(())
            })?;
            return Ok(true);
        }
        let seen = rows.len() as i64;
        let next = rows.last().map_or(before, |r| r.info.id);
        let mut batch = Batch::default();
        let visits = rows.into_iter().map(|row| (row, None)).collect();
        self.visit_all(conn, visits, redo, now, &mut batch)?;
        let ran = batch.applied.len() as i64;
        self.stats.rows_walked.fetch_add(seen as u64, Relaxed);
        self.send(db, shard, batch, now, move |tx| {
            tx.prepare_cached("update pp_walks set before_id = ?, seen = seen + ?, ran = ran + ? where id = ?")?
                .execute(params![next, seen, ran, walk_id])?;
            Ok(())
        })?;
        Ok(true)
    }

    /// One job for the shard's writer: each release in its savepoint, and `also`
    /// with them. Waits for the answer, and what the outcomes hold goes with it.
    fn send(
        &self,
        db: &Db,
        shard: usize,
        batch: Batch,
        now: i64,
        also: impl FnOnce(&Connection) -> rusqlite::Result<()> + Send + 'static,
    ) -> anyhow::Result<()> {
        let Batch { applied, holds } = batch;
        let max_attempts = self.settings.read().unwrap().config.max_attempts;
        let answer = db.on_writer(
            shard,
            Box::new(move |conn| {
                for a in applied {
                    let id = a.release_id;
                    apply_one(conn, a, now, &|attempt| (backoff(attempt, id), attempt >= max_attempts))?;
                }
                also(conn)
            }),
        )?;
        let answered = answer.recv()?.map_err(anyhow::Error::msg);
        drop(holds);
        answered
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
            let summary = summaries.remove(&row.info.id);
            if let Some(a) = self.visit(conn, row, summary, due, redo, now, &rules, batch) {
                batch.applied.push(a);
            }
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
            // a queue row goes, a state row is no longer wanted by the net lane, a walk row writes nothing
            (due.is_some() || row.has_state).then(|| Applied { no_state: !row.has_state, ..base_copy(&base) })
        };
        if let Some(reason) = rules::judge_row(rules, &facts(&row)) {
            return reject(reason);
        }
        let summary = summary.unwrap_or_default();
        match rules::judge_files(rules, &facts(&row), &summary, now) {
            Verdict::Reject(reason) => return reject(reason),
            Verdict::Wait(until) => return Some(Applied { queue_at: Some(until), no_state: true, ..base_copy(&base) }),
            Verdict::Pass => {}
        }

        let enabled = self.enabled();
        let mut clear = redo;
        if row.has_state && row.info.parts > row.parts_seen {
            clear |= self.step_mask(|s| s.reruns().on_growth);
        }
        let (mut done, mut failed) = (row.done & !clear, row.failed & !clear);
        let mut a = base;
        a.parts_seen = Some(row.info.parts);
        let mut ctx = StepCtx {
            release: row.info.clone(),
            conn,
            facts: Vec::new(),
            bodies: &self.fetcher,
            used_bodies: 0,
            deadline: Instant::now() + STEP_DEADLINE,
            lane_can_fetch: false,
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
            let ran = match catch_unwind(AssertUnwindSafe(|| step.run(&mut ctx))) {
                Ok(Ok(outcome)) => Ran::Outcome(outcome),
                Ok(Err(e)) => Ran::Error(e),
                Err(panic) => Ran::Error(StepError::Retry(panic_text(panic))),
            };
            match ran {
                Ran::Outcome(mut o) => {
                    batch.holds.extend(o.hold.take());
                    if let Some(until) = o.wait_until {
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
                Ran::Error(StepError::Stopped) => return None,
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
            a.net_next_at = Some(0);
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
                    if self.net_busy[shard].swap(true, Relaxed) {
                        continue;
                    }
                    let result =
                        conns.get(shard).map_err(anyhow::Error::from).and_then(|conn| self.net_claim(conn, &db, shard));
                    self.net_busy[shard].store(false, Relaxed);
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
        // new posts are worth most, and every 8th claim keeps history moving
        let oldest = self.claims[shard].load(Relaxed) % 8 == 7;
        let order = if oldest { "asc" } else { "desc" };
        let ids: Vec<i64> = conn
            .prepare_cached(&format!(
                "select release_id from pp_state where net_wanted = 1 and net_next_at <= ?1 order by release_id {order} limit ?2"
            ))?
            .query_map(params![now, NET_BATCH], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        if ids.is_empty() {
            return Ok(false);
        }
        self.claims[shard].fetch_add(1, Relaxed);
        for id in ids {
            if self.stopping() {
                break;
            }
            self.net_one(conn, db, shard, id)?;
        }
        Ok(true)
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
            }
            // not settled: it stays wanted, and its lane comes back then
            Verdict::Wait(until) => {
                a.net_mask = net_steps;
                a.net_next_at = Some(until);
            }
            Verdict::Pass => {}
        }
        self.send(db, shard, one(a), now, |_| Ok(()))
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
            return self.send(db, shard, one(a), now, |_| Ok(()));
        };
        let bit = mask(step.id());
        a.net_mask = left.iter().fold(0, |m, s| m | mask(s.id()));

        // unfinished runs left this: the process died in them
        if row.net_attempts >= max_attempts {
            a.set_failed |= bit;
            a.net_attempts = Some(0);
            a.net_next_at = Some(0);
            a.error = Some("interrupted".into());
            return self.send(db, shard, one(a), now, |_| Ok(()));
        }

        // Begin, before anything can go wrong
        let begin_at = now + NET_CRASH_RETRY;
        let answer = db.on_writer(
            shard,
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
        };
        let ran = match catch_unwind(AssertUnwindSafe(|| step.run(&mut ctx))) {
            Ok(Ok(outcome)) => Ran::Outcome(outcome),
            Ok(Err(e)) => Ran::Error(e),
            Err(panic) => Ran::Error(StepError::Retry(panic_text(panic))),
        };
        let now = chrono::Utc::now().timestamp();
        let attempts_before = row.net_attempts;
        let mut batch = Batch::default();
        match ran {
            Ran::Outcome(mut o) => {
                batch.holds.extend(o.hold.take());
                if let Some(until) = o.wait_until {
                    a.net_attempts = Some(attempts_before);
                    a.net_next_at = Some(until);
                } else {
                    self.count_done(&o);
                    a.set_done |= bit;
                    a.net_mask &= !bit;
                    a.net_attempts = Some(0);
                    a.net_next_at = Some(0);
                    a.writes.extend(std::mem::take(&mut o.writes).into_iter().map(|w| (bit, w)));
                    // later steps run again: this lane's come back to it, the database's through the queue
                    let later = self.steps.iter().filter(|s| s.rank() > step.rank()).fold(0, |m, s| m | mask(s.id()));
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
                    a.net_next_at = Some(0);
                } else {
                    a.net_attempts = Some(attempt);
                    a.net_next_at = Some(now + backoff(attempt, id));
                }
                a.error = Some(e);
            }
            Ran::Error(StepError::Permanent(e)) => {
                a.set_failed |= bit;
                a.net_attempts = Some(0);
                a.net_next_at = Some(0);
                a.error = Some(e);
            }
            Ran::Error(StepError::Transient(e)) => {
                a.net_attempts = Some(attempts_before);
                a.net_next_at = Some(now + 300);
                a.error = Some(e);
                self.net_paused_until.store(now + 60, Relaxed);
            }
            Ran::Error(StepError::OutOfBudget) => {
                a.net_attempts = Some(attempts_before);
                a.net_next_at = Some(now + 60);
            }
        }
        batch.applied.push(a);
        self.send(db, shard, batch, now, |_| Ok(()))
    }
}

fn one(a: Applied) -> Batch {
    Batch { applied: vec![a], holds: Vec::new() }
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
