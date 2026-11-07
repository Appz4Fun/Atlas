use std::collections::{HashMap, HashSet};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use rusqlite::Connection;

use crate::config::{DEFAULT_BATCH_SIZE, DEFAULT_REQUEST_SIZE};
use crate::db;
use crate::nfo;
use crate::nntp::{BlockingPool, Extract, Overview, Pool, Retention, first_names, headers_to_articles};
use crate::par2;
use crate::parser::{Release, is_complete};

/// What one finished XOVER slice added, reported as it lands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Progress {
    pub articles: i64,
    pub bytes: i64,
    pub releases: i64,
    /// articles left out as obfuscated per article, also in `articles`
    pub skipped: i64,
}

impl Progress {
    fn add(&mut self, other: &Progress) {
        self.articles += other.articles;
        self.bytes += other.bytes;
        self.releases += other.releases;
        self.skipped += other.skipped;
    }
}

/// progress callback that ignores everything
pub fn no_progress(_: &Progress) {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Backfill,
    Live,
}

/// per group phase/idle/backfilling
#[derive(Clone, Debug)]
pub struct GroupRunState {
    pub phase: Phase,
    pub idle: bool,
    pub backfilling: bool,
    /// a big group found unsplittable isnt probed again until then
    pub no_split_until: Option<Instant>,
    /// a split group's servers are looked at for older days again then
    pub reach_back_after: Option<Instant>,
    /// per server host (not its place in the pool, which a config reload can
    /// give to another), the oldest day it keeps of the group (as found by a
    /// day chunk too old for it), until it's looked at again
    pub keeps_from: HashMap<String, (i64, Instant)>,
    /// likewise the day of its first post, `i64::MAX` when it has none or
    /// doesnt carry the group (see `drop_unkept_days`); a server not carrying
    /// the group is noted for good (no expiry): the scheduler skips it for
    /// good too
    pub first_post_days: HashMap<String, (i64, Option<Instant>)>,
    /// XOVER slices a server refused with a 5xx, still being tried again (see `RunStates::refused`)
    pub refused: Vec<Refused>,
}

/// A slice of `key`'s numbers (a cursor key, soo one server's) its server
/// answered XOVER with a 5xx: when that was first, and how many times since.
#[derive(Clone, Debug)]
pub struct Refused {
    pub key: String,
    pub start: u64,
    pub end: u64,
    pub since: Instant,
    pub tries: u32,
    /// refused last at: one not asked again for `REFUSED_IDLE` is out of reach
    /// (past retention, or the group gone) and forgotten
    pub last: Instant,
    /// passed over, last asked at: kept while it is still asked, soo a day
    /// chunk read again whole passes over it again instead of starting its hour over
    pub passed: Option<Instant>,
}

/// A slice refused with a 5xx is asked again on later passes, and only once it
/// was refused at least `REFUSED_TRIES` times over at least `REFUSED_FOR` is it
/// taken for unavailable and passed over: a provider's overview having trouble
/// for a while must not cost the articles in it for good.
pub const REFUSED_FOR: Duration = Duration::from_secs(3600);
pub const REFUSED_TRIES: u32 = 3;
/// refused slices still asked again noted per group at most; past it a
/// refusal isnt noted until there is room
const REFUSED_KEPT: usize = 1024;
/// a refusal not asked again this long is forgotten: anything still being
/// read asks again well within it
pub const REFUSED_IDLE: Duration = Duration::from_secs(6 * 3600);
/// passed over ones kept per group at most, the longest not asked go first
const PASSED_KEPT: usize = 4096;

/// the days of a split only its deepest server has that are chunks at once
/// (see `chunks::Speculative`), and the articles a day those done have to
/// hold on average to reach this many further
const SPECULATIVE_WINDOW: i64 = 60;
const SPECULATIVE_YIELD: i64 = 100;

impl Default for GroupRunState {
    fn default() -> Self {
        GroupRunState {
            phase: Phase::Backfill,
            idle: false,
            backfilling: false,
            no_split_until: None,
            reach_back_after: None,
            keeps_from: HashMap::new(),
            first_post_days: HashMap::new(),
            refused: Vec::new(),
        }
    }
}

/// Run state of every group, shared by passes running at the same time, and the groups config.toml tracks now (see
/// `groups`).
#[derive(Clone, Default)]
pub struct RunStates(Arc<Mutex<HashMap<String, GroupRunState>>>, Arc<Mutex<Option<HashSet<String>>>>);

impl RunStates {
    fn with<R>(&self, group: &str, f: impl FnOnce(&mut GroupRunState) -> R) -> R {
        f(self.0.lock().unwrap().entry(group.to_string()).or_default())
    }

    pub fn is_idle(&self, group: &str) -> bool {
        self.0.lock().unwrap().get(group).is_some_and(|s| s.idle)
    }

    pub fn is_backfilling(&self, group: &str) -> bool {
        self.0.lock().unwrap().get(group).is_some_and(|s| s.backfilling)
    }

    pub fn all_idle(&self, groups: &[String]) -> bool {
        let states = self.0.lock().unwrap();
        groups.iter().all(|g| states.get(g).is_some_and(|s| s.idle))
    }

    /// Note `groups` as the ones config.toml tracks now.
    pub fn set_tracked(&self, groups: &[String]) {
        *self.1.lock().unwrap() = Some(groups.iter().cloned().collect());
    }

    /// The groups config.toml tracks now (`set_tracked`), or until those are noted the ones passes ran for: a group
    /// taken out keeps its run state, but no longer counts here.
    pub fn groups(&self) -> HashSet<String> {
        if let Some(tracked) = self.1.lock().unwrap().as_ref() {
            return tracked.clone();
        }
        self.0.lock().unwrap().keys().cloned().collect()
    }

    /// The oldest day the server `host` keeps of `group`, `i64::MIN` when not known.
    pub fn keeps_from(&self, group: &str, host: &str) -> i64 {
        let now = Instant::now();
        let states = self.0.lock().unwrap();
        let found = states.get(group).and_then(|s| s.keeps_from.get(host));
        found.filter(|(_, until)| *until > now).map_or(i64::MIN, |(day, _)| *day)
    }

    /// Note `day` as the oldest the server `host` keeps of `group`, until it's looked at again.
    fn keep_from(&self, group: &str, host: &str, day: i64) {
        self.with(group, |st| st.keeps_from.insert(host.to_string(), (day, Instant::now() + KEEPS_RECHECK)));
    }

    /// Note `day` as the day of the server `host`'s first post of `group`,
    /// until it's looked at again.
    fn first_post_on(&self, group: &str, host: &str, day: i64) {
        self.with(group, |st| st.first_post_days.insert(host.to_string(), (day, Some(Instant::now() + KEEPS_RECHECK))));
    }

    /// Note the server `host` as not carrying `group`, with no expiry.
    fn not_carrying(&self, group: &str, host: &str) {
        self.with(group, |st| st.first_post_days.insert(host.to_string(), (i64::MAX, None)));
    }

    /// The day of the server `host`'s first post of `group`, `i64::MIN` when not known.
    fn first_post_day(&self, group: &str, host: &str) -> i64 {
        let now = Instant::now();
        let states = self.0.lock().unwrap();
        let found = states.get(group).and_then(|s| s.first_post_days.get(host));
        found.filter(|(_, until)| until.is_none_or(|u| u > now)).map_or(i64::MIN, |(day, _)| *day)
    }

    /// The groups whose oldest day on the server `host` is known, with that day.
    pub fn kept_days(&self, host: &str) -> HashMap<String, i64> {
        let now = Instant::now();
        let states = self.0.lock().unwrap();
        states
            .iter()
            .filter_map(|(g, s)| s.keeps_from.get(host).filter(|(_, until)| *until > now).map(|(d, _)| (g.clone(), *d)))
            .collect()
    }

    /// `start..=end` of `key` (a cursor key of `group`) was refused with a 5xx
    /// at `now`. True: it has been, for `REFUSED_FOR` and `REFUSED_TRIES` times,
    /// soo it is passed over now. False: it is asked again later. Slice bounds
    /// move between passes, soo a refusal overlapping an earlier one of the same
    /// key counts as the same. A range passed over stays passed over while it
    /// is asked again (a day chunk is read again whole until every slice of it
    /// is done) and is forgotten once not asked for `REFUSED_FOR`.
    pub fn refused(&self, group: &str, key: &str, (start, end): (u64, u64), now: Instant) -> bool {
        self.with(group, |st| {
            st.refused.retain(|r| match r.passed {
                Some(at) => now.saturating_duration_since(at) < REFUSED_FOR,
                None => now.saturating_duration_since(r.last) < REFUSED_IDLE,
            });
            let passed = |r: &Refused| r.passed.is_some() && r.key == key && r.start <= start && end <= r.end;
            if let Some(r) = st.refused.iter_mut().find(|r| passed(r)) {
                r.passed = Some(now);
                return true;
            }
            let same = |r: &Refused| r.passed.is_none() && r.key == key && r.start <= end && start <= r.end;
            let Some(at) = st.refused.iter().position(same) else {
                // full: this one waits for room and is asked again meanwhile. Dropping
                // an older one would start its hour over, and with more refused slices
                // a pass than room none would ever reach it.
                if st.refused.iter().filter(|r| r.passed.is_none()).count() >= REFUSED_KEPT {
                    return false;
                }
                st.refused.push(Refused {
                    key: key.to_string(),
                    start,
                    end,
                    since: now,
                    tries: 1,
                    last: now,
                    passed: None,
                });
                return false;
            };
            let r = &mut st.refused[at];
            r.tries += 1;
            r.last = now;
            (r.start, r.end) = (r.start.min(start), r.end.max(end));
            let give_up = r.tries >= REFUSED_TRIES && now.saturating_duration_since(r.since) >= REFUSED_FOR;
            if give_up {
                r.passed = Some(now);
                let passed = st.refused.iter().filter(|r| r.passed.is_some()).count();
                if passed > PASSED_KEPT
                    && let Some(oldest) = (0..st.refused.len())
                        .filter(|i| st.refused[*i].passed.is_some())
                        .min_by_key(|i| st.refused[*i].passed)
                {
                    st.refused.remove(oldest);
                }
            }
            give_up
        })
    }

    /// `start..=end` of `key` was read: refusals of it are over. Only of it:
    /// slice bounds move, soo what of a refused range is left either side
    /// keeps its history.
    pub fn read(&self, group: &str, key: &str, (start, end): (u64, u64)) {
        let mut states = self.0.lock().unwrap();
        if let Some(st) = states.get_mut(group)
            && !st.refused.is_empty()
        {
            let mut left = Vec::new();
            st.refused.retain(|r| {
                if r.key != key || end < r.start || r.end < start {
                    return true;
                }
                if r.start < start {
                    left.push(Refused { end: start - 1, key: r.key.clone(), ..*r });
                }
                if end < r.end {
                    left.push(Refused { start: end + 1, key: r.key.clone(), ..*r });
                }
                false
            });
            st.refused.extend(left);
        }
    }

    /// switching modes starts every group over in the backfill phase
    pub fn reset(&self) {
        for st in self.0.lock().unwrap().values_mut() {
            *st = GroupRunState::default();
        }
    }
}

/// How a pass indexes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PassSettings {
    pub mode: String,
    /// article numbers per pass over a group
    pub batch_size: i64,
    /// article numbers per XOVER request
    pub request_size: u64,
    /// backfill left on a group's home server before it is split into day chunks
    pub split_min_backlog: i64,
    /// article numbers a day chunk reaches past its ends (`CHUNK_SAFETY`)
    pub chunk_safety: u64,
    /// a pass on a server standing in for the group's own one while that is in trouble: GROUP there only (the group's
    /// home doesnt move), and a cursor started there leaves the group's phase alone
    pub fallback: bool,
    /// with `fallback`: the first such pass on this server since the group's own one got into trouble. Its live
    /// cursor starts at the newest article, since what came before was read on the group's own server (or will be
    /// once it is back), and a cursor left from an earlier stand-in would read all of it again
    pub from_top: bool,
}

impl Default for PassSettings {
    fn default() -> Self {
        PassSettings {
            mode: "dynamic".into(),
            batch_size: DEFAULT_BATCH_SIZE as i64,
            request_size: DEFAULT_REQUEST_SIZE,
            split_min_backlog: crate::config::SPLIT_MIN_BACKLOG,
            chunk_safety: CHUNK_SAFETY,
            fallback: false,
            from_top: false,
        }
    }
}

/// slices saved together in one transaction at most.
const MAX_BATCH: usize = 32;

/// The indexer's database, shared by every pass. Small writes (cursors) run on
/// the main database's connection directly. Slices go to the writer thread of
/// their group's shard (see store.rs); each saves whatever queued up for it
/// while it was busy in one transaction, and the shards save in parallel.
#[derive(Clone)]
pub struct Db {
    conn: Arc<Mutex<Connection>>,
    saves: Vec<std::sync::mpsc::SyncSender<WriterJob>>,
    /// each shard writer's lane for small jobs someone waits on (`on_writer_first`): read between saves, ahead of
    /// both the saves and the stage work queued in `saves`
    firsts: Vec<std::sync::mpsc::Sender<StageJob>>,
    /// last field: dropping the final clone closes the queues above, then waits
    /// for the writer threads to finish what they were doing
    _writers: Arc<Writers>,
}

/// The writer threads, joined when dropped.
struct Writers(Vec<std::thread::JoinHandle<()>>);

impl Drop for Writers {
    fn drop(&mut self) {
        for writer in self.0.drain(..) {
            let _ = writer.join();
        }
    }
}

struct SaveJob {
    releases: Vec<Release>,
    done: tokio::sync::oneshot::Sender<std::result::Result<(), String>>,
}

/// a write of the processing stage's, in one transaction on the shard's writer
pub(crate) type StageWork = Box<dyn FnOnce(&Connection) -> rusqlite::Result<()> + Send>;

pub(crate) struct StageJob {
    /// what the job is, for its own `atlas_function_duration_seconds` series (`db.stage.*`)
    kind: &'static str,
    work: StageWork,
    done: std::sync::mpsc::SyncSender<std::result::Result<(), String>>,
}

enum WriterJob {
    Save(SaveJob),
    Stage(StageJob),
    /// a job came in on the writer's `firsts` lane: wakes a writer waiting for work
    Wake,
}

/// time a writer spends on stage work after each save, and with no save waiting
const STAGE_BUDGET_BUSY: Duration = Duration::from_millis(25);
const STAGE_BUDGET_IDLE: Duration = Duration::from_millis(200);
/// While saves come in, stage work gets this much of a writer's time per second it spent saving, banked up to
/// `STAGE_BANK` seconds. A stage job can take seconds and saves queue behind it: on prod the stage took about 80% of
/// the writers' time while slices waited 88 s on average to be saved, with the unsaved headers budget full, and
/// indexing waited for that budget.
const STAGE_SHARE: f64 = 0.5;
const STAGE_BANK: f64 = 10.0;
/// with no save for this long, stage work runs as much as it likes
const STAGE_QUIET: Duration = Duration::from_secs(1);

/// `conn` is the main database. The writer threads end once every clone of
/// the `Db` is gone, and the last clone to go waits for them.
pub fn shared_db(conn: Connection) -> Db {
    shared_db_idling(conn, Duration::from_secs(crate::store::SEAL_WALK_EVERY as u64))
}

/// `shared_db` whose writers, with nothing to save for `every`, look for
/// files due to be sealed.
fn shared_db_idling(conn: Connection, every: Duration) -> Db {
    let main = conn.path().map(std::path::PathBuf::from).unwrap_or_else(crate::paths::database);
    let ids = Arc::new(crate::store::Ids::new(&main));
    let mut saves = Vec::new();
    let mut firsts = Vec::new();
    let mut writers = Vec::new();
    for shard in 0..crate::store::SHARDS {
        let (save, jobs) = std::sync::mpsc::sync_channel(4096);
        let (first, first_jobs) = std::sync::mpsc::channel();
        let (path, ids) = (crate::store::shard_path(&main, shard), ids.clone());
        let handle = std::thread::Builder::new()
            .name(format!("atlas-db-writer-{shard}"))
            .spawn(move || writer(shard, &path, &ids, jobs, first_jobs, every))
            .expect("couldnt start a db writer");
        saves.push(save);
        firsts.push(first);
        writers.push(handle);
    }
    Db { conn: Arc::new(Mutex::new(conn)), saves, firsts, _writers: Arc::new(Writers(writers)) }
}

/// A shard's writer: saves what comes in, and between saves (or after
/// `idle` with nothing to save) seals what is due and checkpoints.
fn writer(
    shard: usize,
    path: &std::path::Path,
    ids: &crate::store::Ids,
    jobs: std::sync::mpsc::Receiver<WriterJob>,
    first_jobs: std::sync::mpsc::Receiver<StageJob>,
    idle: Duration,
) {
    use crate::profile::{LOAD, Load};
    use std::sync::atomic::Ordering::Relaxed;
    use std::sync::mpsc::{RecvTimeoutError, TryRecvError};

    let mut opened = db::open_shard(path).and_then(|conn| db::tune_for_writing(&conn).map(|_| conn));
    // each save on disk when committed: the WAL is no longer checkpointed (and soo synced) after every save (#228),
    // and the group cursors in atlas.db, which move once a save is committed, mustnt reach the disk before it
    if let Ok(conn) = &opened
        && let Err(e) = conn.execute_batch("pragma main.synchronous = full")
    {
        eprintln!("couldnt make the saves in {} synchronous: {e}", path.display());
    }
    // the frozen file, which only this writer has: without it nothing freezes and nothing thaws here
    if let Ok(conn) = &opened
        && let Err(e) = crate::frozen::attach(conn, path)
    {
        eprintln!("couldnt open the frozen file of {}: {e}", path.display());
    }
    let mut store = crate::store::ShardWriter::new(shard);
    // a job taken off the queue to see if more were waiting
    let mut next: Option<SaveJob> = None;
    // stage work waits here, between saves
    let mut stage: std::collections::VecDeque<StageJob> = Default::default();
    let mut last_housekeeping = std::time::Instant::now();
    // stage time owed while saves come in (see `STAGE_SHARE`), and when the last save was
    let mut bank = 0.0f64;
    let mut last_save: Option<std::time::Instant> = None;
    // jobs from the `firsts` lane waiting to run: a few at a time between saves (`STAGE_BUDGET_BUSY`), outside stage
    // work's share
    let mut firsts: std::collections::VecDeque<StageJob> = Default::default();
    // when the WAL is copied into the shard and starts over: by its size or age, not after every save (#228)
    let mut checkpoints = db::Checkpoints::for_writer();

    loop {
        run_firsts(&mut opened, path, &first_jobs, &mut firsts);
        let since_save = last_save.map_or(STAGE_QUIET, |t| t.elapsed());
        let wait = if !firsts.is_empty() {
            // the firsts lane has jobs left over from its last turn
            Duration::ZERO
        } else if stage.is_empty() {
            // a writer with nothing to do wakes for idle freezing too, and does the rest of its housekeeping every `idle`
            if crate::frozen::idle_ticks_on() { idle.min(crate::frozen::IDLE_TICK_EVERY) } else { idle }
        } else if bank > 0.0 || since_save >= STAGE_QUIET {
            Duration::ZERO
        } else {
            // saves are coming and stage work has had its share: give a save the chance to come first
            STAGE_QUIET - since_save
        };
        let first = match next.take() {
            Some(job) => job,
            None => match jobs.recv_timeout(wait) {
                Ok(WriterJob::Save(job)) => job,
                Ok(WriterJob::Stage(job)) => {
                    stage.push_back(job);
                    continue;
                }
                Ok(WriterJob::Wake) => continue,
                // nothing to save: files still come due as they go stale
                Err(RecvTimeoutError::Timeout) if stage.is_empty() && last_housekeeping.elapsed() < idle => {
                    if let Ok(conn) = &mut opened {
                        freeze_idle(conn, &mut store);
                    }
                    continue;
                }
                Err(RecvTimeoutError::Timeout) if stage.is_empty() => {
                    if let Ok(conn) = &mut opened {
                        housekeeping(conn, &mut store, &mut checkpoints, true, true);
                    }
                    last_housekeeping = std::time::Instant::now();
                    continue;
                }
                Err(RecvTimeoutError::Timeout) => {
                    let quiet = last_save.is_none_or(|t| t.elapsed() >= STAGE_QUIET);
                    let spent = run_stage_jobs(&mut opened, path, &mut stage, Some(STAGE_BUDGET_IDLE));
                    if !quiet {
                        bank -= spent;
                    }
                    if last_housekeeping.elapsed() >= Duration::from_secs(1) {
                        if let Ok(conn) = &mut opened {
                            housekeeping(conn, &mut store, &mut checkpoints, true, false);
                        }
                        last_housekeeping = std::time::Instant::now();
                    }
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => break,
            },
        };
        let mut batch = vec![first];
        while batch.len() < MAX_BATCH {
            match jobs.try_recv() {
                Ok(WriterJob::Save(job)) => batch.push(job),
                Ok(WriterJob::Stage(job)) => stage.push_back(job),
                Ok(WriterJob::Wake) => {}
                Err(_) => break,
            }
        }
        LOAD.writer_queued.fetch_sub(batch.len() as u64, Relaxed);
        // nobody waits for a slice whose pass was dropped (stopping): its
        // cursor didnt move and its headers get fetched again, soo skip it
        batch.retain(|job| !job.done.is_closed());
        if batch.is_empty() {
            continue;
        }

        let t = std::time::Instant::now();
        let span = crate::metrics::span("db.save_batch");
        let result = match &mut opened {
            Ok(conn) => {
                store.save(conn, ids, batch.iter().map(|job| job.releases.as_slice())).map_err(|e| e.to_string())
            }
            Err(e) => Err(format!("couldnt open {}: {e}", path.display())),
        };
        if result.is_err() {
            span.failed();
        }
        drop(span);
        Load::add_since(&LOAD.writer_busy_ns, t);
        LOAD.writer_batches.fetch_add(1, Relaxed);
        LOAD.writer_slices.fetch_add(batch.len() as u64, Relaxed);

        // the whole batch rolled back on an error, every slice in it failed.
        // the save is committed by now: the slices dont wait for the housekeeping
        let saved = result.is_ok();
        for job in batch {
            let _ = job.done.send(result.clone());
        }
        bank = (bank + t.elapsed().as_secs_f64() * STAGE_SHARE).min(STAGE_BANK);
        last_save = Some(std::time::Instant::now());
        run_firsts(&mut opened, path, &first_jobs, &mut firsts);
        if bank > 0.0 {
            bank -= run_stage_jobs(&mut opened, path, &mut stage, Some(STAGE_BUDGET_BUSY));
        }

        // housekeeping between transactions
        if let Ok(conn) = &mut opened {
            // more slices waiting: saving them comes first. a closed queue
            // means the indexer is stopping, it waits for this thread
            let mut seal = saved;
            loop {
                match jobs.try_recv() {
                    Ok(WriterJob::Save(job)) => {
                        next = Some(job);
                        seal = false;
                        break;
                    }
                    Ok(WriterJob::Stage(job)) => stage.push_back(job),
                    Ok(WriterJob::Wake) => {}
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        seal = false;
                        break;
                    }
                }
            }
            // nothing came in meanwhile and no stage work waits: the writer would sit idle
            let idle = next.is_none() && stage.is_empty() && saved;
            housekeeping(conn, &mut store, &mut checkpoints, seal, idle);
            last_housekeeping = std::time::Instant::now();
        }
    }
    // stopping: whoever queued these is told
    firsts.extend(first_jobs.try_iter());
    run_stage_jobs(&mut opened, path, &mut firsts, None);
    run_stage_jobs(&mut opened, path, &mut stage, None);
}

/// The `firsts` lane's jobs, in the order they came, for at most `STAGE_BUDGET_BUSY`; the rest wait for the next turn,
/// which comes between saves or when the writer next looks for work.
fn run_firsts(
    opened: &mut rusqlite::Result<Connection>,
    path: &std::path::Path,
    lane: &std::sync::mpsc::Receiver<StageJob>,
    firsts: &mut std::collections::VecDeque<StageJob>,
) {
    firsts.extend(lane.try_iter());
    if !firsts.is_empty() {
        run_stage_jobs(opened, path, firsts, Some(STAGE_BUDGET_BUSY));
    }
}

/// Stage jobs, each in one transaction, for at most `budget` (None: all of them). Returns the seconds they took.
fn run_stage_jobs(
    opened: &mut rusqlite::Result<Connection>,
    path: &std::path::Path,
    stage: &mut std::collections::VecDeque<StageJob>,
    budget: Option<Duration>,
) -> f64 {
    use crate::profile::{LOAD, Load};
    use std::sync::atomic::Ordering::Relaxed;

    let start = std::time::Instant::now();
    while let Some(job) = stage.pop_front() {
        let t = std::time::Instant::now();
        let span = crate::metrics::span("db.stage_transaction");
        let by_kind = crate::metrics::span(job.kind);
        let result = match opened {
            Ok(conn) => stage_transaction(conn, job.work).map_err(|e| e.to_string()),
            Err(e) => Err(format!("couldnt open {}: {e}", path.display())),
        };
        if result.is_err() {
            span.failed();
            by_kind.failed();
        }
        drop((span, by_kind));
        let _ = job.done.send(result);
        Load::add_since(&LOAD.writer_stage_ns, t);
        Load::add_since(&LOAD.writer_busy_ns, t);
        LOAD.writer_stage_jobs.fetch_add(1, Relaxed);
        if budget.is_some_and(|b| start.elapsed() >= b) {
            break;
        }
    }
    start.elapsed().as_secs_f64()
}

fn stage_transaction(conn: &Connection, work: StageWork) -> rusqlite::Result<()> {
    conn.execute_batch("begin immediate")?;
    match work(conn) {
        Ok(()) => conn.execute_batch("commit"),
        Err(e) => {
            let _ = conn.execute_batch("rollback");
            Err(e)
        }
    }
}

/// Only the idle freezing of `housekeeping`, for a writer woken with nothing to do between its housekeepings.
fn freeze_idle(conn: &mut Connection, store: &mut crate::store::ShardWriter) {
    let t = std::time::Instant::now();
    let span = crate::metrics::span("db.freeze");
    if let Err(e) = store.freezer.tick_idle(conn, chrono::Utc::now().timestamp()) {
        span.failed();
        eprintln!("freezing in shard {} failed: {e}", store.shard);
    }
    drop(span);
    // the writer's time like any other housekeeping's
    crate::profile::Load::add_since(&crate::profile::LOAD.writer_busy_ns, t);
}

/// A writer's work between saves: seal some of what is due (when `seal`),
/// and copy the WAL into the shard once `checkpoints` says it is due.
/// `idle`: nothing waits for the writer, soo freezing may take longer (`frozen::Ticker::tick_idle`).
fn housekeeping(
    conn: &mut Connection,
    store: &mut crate::store::ShardWriter,
    checkpoints: &mut db::Checkpoints,
    seal: bool,
    idle: bool,
) {
    use crate::profile::{LOAD, Load};
    use std::sync::atomic::Ordering::Relaxed;

    let t = std::time::Instant::now();
    if seal {
        let sealing = std::time::Instant::now();
        let span = crate::metrics::span("db.seal");
        if store.seal_some(conn, chrono::Utc::now().timestamp()).is_err() {
            span.failed();
            LOAD.writer_seal_errors.fetch_add(1, Relaxed);
        }
        drop(span);
        Load::add_since(&LOAD.writer_seal_ns, sealing);
    }
    // what the freezing rules queued, a little at a time (`frozen::Ticker`)
    let span = crate::metrics::span("db.freeze");
    let now = chrono::Utc::now().timestamp();
    let ticked = if idle { store.freezer.tick_idle(conn, now) } else { store.freezer.tick(conn, now) };
    if let Err(e) = ticked {
        span.failed();
        eprintln!("freezing in shard {} failed: {e}", store.shard);
    }
    drop(span);
    // the index of quiet releases' message-ids, a cache that is made again when needed (`ShardWriter::prune_ids_some`)
    let span = crate::metrics::span("db.prune_ids");
    if store.prune_ids_some(conn, now, idle).is_err() {
        span.failed();
    }
    drop(span);
    // the shared catalog's sent changes, as far as Postgres has them (`shared_catalog::prune_acked`)
    let span = crate::metrics::span("db.catalog_prune");
    if crate::shared_catalog::prune_acked(conn).is_err() {
        span.failed();
    }
    drop(span);
    let checkpointing = std::time::Instant::now();
    let span = crate::metrics::span("db.checkpoint");
    if checkpoints.checkpoint_if_due(conn).is_err() {
        span.failed();
    }
    drop(span);
    Load::add_since(&LOAD.writer_checkpoint_ns, checkpointing);
    Load::add_since(&LOAD.writer_busy_ns, t);
}

impl Db {
    /// Run `work` in one transaction on `shard`'s writer, between saves. The
    /// answer comes when it has committed; jobs queued when the writer stops still run.
    /// `kind` (`db.stage.*`) times it apart from the other kinds: one job can hold the writer for minutes.
    pub(crate) fn on_writer(
        &self,
        shard: usize,
        kind: &'static str,
        work: StageWork,
    ) -> Result<std::sync::mpsc::Receiver<std::result::Result<(), String>>> {
        let (done, answer) = std::sync::mpsc::sync_channel(1);
        self.saves[shard]
            .send(WriterJob::Stage(StageJob { kind, work, done }))
            .map_err(|_| anyhow!("db writer stopped"))?;
        Ok(answer)
    }

    /// `on_writer` for a small job someone waits on. It comes in on the writer's own lane, so it waits neither behind
    /// the saves queued before it nor behind the stage work: the writer runs such jobs between saves, a few at a time
    /// (`STAGE_BUDGET_BUSY`), outside stage work's share. Never blocks. Behind a backlog of processing work an
    /// ordinary job waited minutes on prod.
    pub(crate) fn on_writer_first(
        &self,
        shard: usize,
        kind: &'static str,
        work: StageWork,
    ) -> Result<std::sync::mpsc::Receiver<std::result::Result<(), String>>> {
        let (done, answer) = std::sync::mpsc::sync_channel(1);
        self.firsts[shard].send(StageJob { kind, work, done }).map_err(|_| anyhow!("db writer stopped"))?;
        // a writer waiting for work wakes; a full queue means it's busy, and looks at the lane between saves anyway
        let _ = self.saves[shard].try_send(WriterJob::Wake);
        Ok(answer)
    }

    /// Save one slice's releases. Returns once they are committed. Dropping
    /// the future before the writer gets to the slice drops the slice too
    /// (its pass didnt finish, soo the cursor didnt move past it); once the
    /// writer has it, it gets saved.
    pub(crate) async fn save(&self, releases: Vec<Release>) -> Result<()> {
        // a slice is one group, soo one shard
        let Some(shard) = releases.first().map(|r| crate::store::shard_of(&r.group)) else { return Ok(()) };
        let (done, saved) = tokio::sync::oneshot::channel();
        crate::profile::LOAD.writer_queued.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if self.saves[shard].send(WriterJob::Save(SaveJob { releases, done })).is_err() {
            crate::profile::LOAD.writer_queued.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
            return Err(anyhow!("db writer stopped"));
        }
        saved.await.map_err(|_| anyhow!("db writer stopped"))?.map_err(|e| anyhow!("saving releases: {e}"))
    }
}

pub(crate) async fn on_db<T: Send + 'static>(
    db: &Db,
    f: impl FnOnce(&mut Connection) -> Result<T> + Send + 'static,
) -> Result<T> {
    let conn = db.conn.clone();
    tokio::task::spawn_blocking(move || {
        let t = std::time::Instant::now();
        let mut conn = conn.lock().unwrap();
        crate::profile::DB_WAIT.add_since(t);
        f(&mut conn)
    })
    .await
    .map_err(|e| anyhow!("db task failed: {e}"))?
}

/// Everything a pass needs, cheap to clone into tasks.
#[derive(Clone)]
pub struct PassContext {
    pub pool: Arc<Pool>,
    pub states: RunStates,
    pub stop: Arc<AtomicBool>,
    pub verbose: bool,
}

/// Article numbers differ between providers, soo with several servers the
/// cursors are kept per server as `group@host` (`Pool::host`, see
/// `server_keys`). One server keeps the plain group name like before, unless
/// it has an explicit `#key`: that names it wherever it is in the list, and a
/// plain cursor may be another server's left from before.
fn cursor_key(pool: &Pool, server: usize, group: &str) -> String {
    // only the host (and port) is case-insensitive, an explicit `#key` isnt
    let host = pool.host(server);
    if pool.len() <= 1 && !host.contains('#') {
        return group.to_string();
    }
    match host.split_once('#') {
        Some((h, k)) => format!("{group}@{}#{k}", h.to_lowercase()),
        None => format!("{group}@{}", host.to_lowercase()),
    }
}

/// Whose the plain (identity-less) cursors are, in the main database's
/// meta: they hold article numbers of one provider, and the order of the
/// servers says nothing about which.
const PLAIN_CURSORS_OF: &str = "plain_cursors_of";

/// With one indexing server the plain cursors are its own: recorded, soo
/// once more servers are added only it adopts them. A lone server under the
/// plain name is the one writing them, soo it takes them over from whoever
/// had them before. With several servers and nothing recorded (cursors kept
/// by a build from before) they are no one's: each server starts fresh.
pub fn claim_plain_cursors(conn: &Connection, pool: &Pool) -> Result<()> {
    let indexing = pool.indexing_servers();
    let [only] = indexing.as_slice() else { return Ok(()) };
    let writes_plain = cursor_key(pool, *only, "g") == "g";
    if writes_plain || plain_cursors_owner(conn)?.is_none() {
        conn.execute(
            "insert or replace into main.meta (key, value) values (?, ?)",
            rusqlite::params![PLAIN_CURSORS_OF, pool.host(*only)],
        )?;
    }
    Ok(())
}

fn plain_cursors_owner(conn: &Connection) -> Result<Option<String>> {
    use rusqlite::OptionalExtension;
    Ok(conn
        .query_row("select cast(value as text) from main.meta where key = ?", [PLAIN_CURSORS_OF], |r| r.get(0))
        .optional()?)
}

/// whether the server named `host` (`Pool::host`) kept the plain cursors
fn owns_plain_cursors(conn: &Connection, host: &str) -> Result<bool> {
    Ok(plain_cursors_owner(conn)?.as_deref() == Some(host))
}

async fn load_cursors(
    ctx: &PassContext,
    db: &Db,
    key: &str,
    group: &str,
    server: usize,
) -> Result<Option<db::GroupState>> {
    let (key, group) = (key.to_string(), group.to_string());
    let legacy = if key == group { Vec::new() } else { ctx.pool.legacy_keys(server) };
    let host = ctx.pool.host(server);
    on_db(db, move |conn| {
        let owns_plain = key != group && owns_plain_cursors(conn, &host)?;
        cursors_or_adopted(conn, &key, &group, owns_plain, &legacy)
    })
    .await
}

/// The cursors saved under `key`, else adopted: saved under one of the
/// server's `legacy` keys (what an earlier build named it, see
/// `nntp::legacy_server_keys`) they move to `key`, once; saved under the
/// plain group name (before a second server was added) only by the server
/// that kept them (`owns_plain`, see `claim_plain_cursors`)
fn cursors_or_adopted(
    conn: &Connection,
    key: &str,
    group: &str,
    owns_plain: bool,
    legacy: &[String],
) -> Result<Option<db::GroupState>> {
    if let Some(state) = db::get_group_state(conn, key)? {
        return Ok(Some(state));
    }

    for old in legacy.iter().map(|k| format!("{group}@{k}")) {
        if db::get_group_state(conn, &old)?.is_some() {
            conn.execute("delete from groups where name = ?", [key])?;
            conn.execute("update groups set name = ? where name = ?", [key, old.as_str()])?;
            return Ok(db::get_group_state(conn, key)?);
        }
    }

    if owns_plain
        && key != group
        && let Some(state) = db::get_group_state(conn, group)?
    {
        db::save_group_state(conn, key, state)?;
        return Ok(Some(state));
    }

    Ok(None)
}

/// One pass over `group`: a batch of live or backfill articles depending on
/// the mode, on server `prefer` (or whichever server carries the group).
/// `progress` hears about every slice as it is saved. Safe to run for many
/// groups at once; the pool keeps each server within its connections.
pub async fn run_pass<P>(
    ctx: &PassContext,
    settings: &PassSettings,
    db: &Db,
    group: &str,
    prefer: usize,
    progress: &mut P,
) -> Result<Progress>
where
    P: FnMut(&Progress) + ?Sized,
{
    let mut ran_on = prefer;
    run_pass_on(ctx, settings, db, group, prefer, progress, &mut ran_on).await
}

/// `run_pass`, saying in `ran_on` which server the pass went to: `prefer`, or the one a GROUP there moved the group
/// to (see `Pool::select_group_known`), soo a failure counts against the server it happened on.
pub async fn run_pass_on<P>(
    ctx: &PassContext,
    settings: &PassSettings,
    db: &Db,
    group: &str,
    prefer: usize,
    progress: &mut P,
    ran_on: &mut usize,
) -> Result<Progress>
where
    P: FnMut(&Progress) + ?Sized,
{
    *ran_on = prefer;
    if crate::filters::group_blocked(group) {
        return Ok(Progress::default());
    }
    // a server that indexed the group before loses it only after several replies in a row that it lacks it
    // (cursors it kept under a key of an earlier build count too, see `cursors_or_adopted`)
    let (server, (_count, first, last, _name)) = if settings.fallback {
        (prefer, ctx.pool.select_group_only(prefer, group).await?)
    } else {
        let known = load_cursors(ctx, db, &cursor_key(&ctx.pool, prefer, group), group, prefer).await?.is_some();
        ctx.pool.select_group_known(prefer, group, known).await?
    };
    *ran_on = server;
    let (first, last) = (first as i64, last as i64);
    let key = cursor_key(&ctx.pool, server, group);

    let state = match load_cursors(ctx, db, &key, group, server).await? {
        Some(mut s) if settings.fallback && settings.from_top && last >= s.live_cursor => {
            let k = key.clone();
            on_db(db, move |conn| Ok(db::update_live_cursor(conn, &k, last)?)).await?;
            s.live_cursor = last;
            s
        }
        Some(s) if last >= s.live_cursor => s,
        // first time both cursors start at the top. if the server renumbered
        // (last went backwards) the group got reset soo start over the same way
        _ => {
            let k = key.clone();
            on_db(db, move |conn| Ok(db::init_group_state(conn, &k, last)?)).await?;
            if !settings.fallback {
                ctx.states.with(group, |st| st.phase = Phase::Backfill);
            }
            db::GroupState { live_cursor: last, backfill_cursor: last }
        }
    };

    // for the stats dashboard's progress and ETA
    let k = key.clone();
    on_db(db, move |conn| Ok(db::save_group_bounds(conn, &k, first, last)?)).await?;

    let mut phase = ctx.states.with(group, |st| st.phase);
    let split = settings.mode != "live" && settings.mode != "backfill" && {
        let g = group.to_string();
        on_db(db, move |conn| Ok(crate::chunks::is_split(conn, &g)?)).await?
    };
    if live_first(&settings.mode, phase, split, last - state.live_cursor, settings.request_size) {
        phase = Phase::Live;
    }
    let backfilling = settings.mode == "backfill" || (settings.mode != "live" && phase == Phase::Backfill);
    if backfilling
        && maybe_split(
            ctx,
            settings,
            db,
            group,
            server,
            (first as u64, last as u64),
            state.backfill_cursor.max(0) as u64,
        )
        .await?
    {
        // a split group's backfill is day chunks, this server takes the next one: a day asked for, or one of the
        // newest month any tracked split group still has to do (see `chunks::claim_turn`), else none
        let host = ctx.pool.host(server);
        let wanted = (group.to_string(), ctx.states.keeps_from(group, &host));
        let (kept, running) = (ctx.states.kept_days(&host), ctx.states.groups());
        // the split's generation as of the claim: a sweep noted after the split reached back isnt
        let g = group.to_string();
        let (claimed, generation) = on_db(db, move |conn| {
            let generation = crate::chunks::sweep_generation(conn, &g)?;
            let split: Vec<(String, i64)> = crate::chunks::split_groups(conn)?
                .into_iter()
                .filter(|s| *s == g || running.contains(s))
                .map(|s| {
                    let day = kept.get(&s).copied().unwrap_or(i64::MIN);
                    (s, day)
                })
                .collect();
            let now = chrono::Utc::now().timestamp();
            Ok((crate::chunks::claim_turn(conn, wanted, &split, &host, now)?, generation))
        })
        .await?;
        let r = match claimed {
            // a day this server doesnt keep is left to the others, it isnt an error
            Some(chunk) => match run_chunk(ctx, settings, db, &chunk, server, progress).await {
                Err(e) if e.downcast_ref::<TooOld>().is_some() => Ok(Progress::default()),
                r => r,
            },
            // nothing for this server: once no chunk of this group it could
            // take is left, its own backfill sweeps on from where the split
            // froze the cursor down to its first article. date searches go by
            // Date headers posters can forge, and neighbouring days done on
            // other servers can leave numbers between them; the sweep picks up
            // whatever the chunks missed (what they got is dropped as a duplicate)
            // the other carriers sweep their own numbers the same way (see `run_sweep`)
            None if sweep_due(ctx, db, group, &ctx.pool.host(server)).await? => {
                if state.backfill_cursor.min(last) < first {
                    let (g, host) = (group.to_string(), ctx.pool.host(server));
                    on_db(db, move |conn| Ok(crate::chunks::set_swept(conn, &g, &host, generation)?)).await?;
                }
                let pass = Pass { ctx, settings, db, group, server, key };
                pass.backfill(state, first, last, progress).await
            }
            None => {
                // nothing pending: in backfill mode the group rests like a finished backfill
                let idle = settings.mode == "backfill";
                ctx.states.with(group, |st| {
                    st.backfilling = false;
                    st.idle |= idle;
                });
                Ok(Progress::default())
            }
        };
        // dynamic mode takes turns, the next pass is live
        ctx.states.with(group, |st| st.phase = Phase::Live);
        return r;
    }

    let pass = Pass { ctx, settings, db, group, server, key };

    match settings.mode.as_str() {
        "live" => pass.live(state, last, progress).await,
        "backfill" => pass.backfill(state, first, last, progress).await,
        _ if phase == Phase::Live => {
            let r = pass.live(state, last, progress).await;
            ctx.states.with(group, |st| st.phase = Phase::Backfill);
            r
        }
        _ => {
            let r = pass.backfill(state, first, last, progress).await;
            ctx.states.with(group, |st| st.phase = Phase::Live);
            r
        }
    }
}

/// Whether a dynamic mode pass whose turn is backfill goes live instead: a split group's whose live side is more than
/// a slice behind. Its day chunks are taken by any server's free workers too (`take_chunk`), and one can hold its home's
/// worker for over an hour, while its new posts waited: after a restart every group's first turn is backfill, and on
/// prod alias and rail claimed a day chunk each and their live cursors stood still for hours.
fn live_first(mode: &str, phase: Phase, split: bool, behind: i64, slice: u64) -> bool {
    mode != "live" && mode != "backfill" && phase == Phase::Backfill && split && behind > slice as i64
}

/// Whether split `group`'s cursor backfill on `host` may sweep: no day
/// chunk of it is left that the server could take or that is still running.
/// Its chunks come first; other groups' chunks dont hold its sweep back
/// (idle workers take those anyway), their retention wont wait for them.
async fn sweep_due(ctx: &PassContext, db: &Db, group: &str, host: &str) -> Result<bool> {
    let wanted = [(group.to_string(), ctx.states.keeps_from(group, host))];
    on_db(db, move |conn| Ok(!crate::chunks::waiting(conn, &wanted)?)).await
}

/// how long a big group that couldnt be split waits before it is probed again
const SPLIT_RECHECK: Duration = Duration::from_secs(3600);

/// Split `group`'s backfill into day chunks when its home server still has
/// more than `split_min_backlog` article numbers to go and another indexing
/// server carries it too. Chunks run from the day at the home cursor back to
/// the oldest day any carrying server has. True when the group is split.
async fn maybe_split(
    ctx: &PassContext,
    settings: &PassSettings,
    db: &Db,
    group: &str,
    home: usize,
    (first, last): (u64, u64),
    cursor: u64,
) -> Result<bool> {
    let g = group.to_string();
    if on_db(db, move |conn| Ok(crate::chunks::is_split(conn, &g)?)).await? {
        // a server that goes back further (one added, or keeping more now)
        // is looked for every SPLIT_RECHECK: normal backfill never gets to
        // the days older than the split's
        let due = ctx.states.with(group, |st| {
            let due = st.reach_back_after.is_none_or(|t| Instant::now() >= t);
            if due {
                st.reach_back_after = Some(Instant::now() + SPLIT_RECHECK);
            }
            due
        });
        if due && let Err(e) = reach_back(ctx, db, group, home, (first, last)).await {
            println!("[SPLIT] {group}: couldnt look for servers going back further: {e:#}");
        }
        return Ok(true);
    }
    if (cursor.saturating_sub(first) as i64) < settings.split_min_backlog {
        return Ok(false);
    }
    if ctx.states.with(group, |st| st.no_split_until.is_some_and(|t| Instant::now() < t)) {
        return Ok(false);
    }

    // the newest day still to do is when the articles at the home cursor
    // were posted
    let at_cursor = ctx.pool.posted_dates(home, group, cursor).await?;
    let split = if at_cursor.is_empty() {
        None
    } else {
        split_days(ctx, group, home, (first, last), Newest::AtCursor(at_cursor)).await?
    };
    let Some(Split { newest_day, oldest_day, carriers, .. }) = split else {
        ctx.states.with(group, |st| st.no_split_until = Some(Instant::now() + SPLIT_RECHECK));
        return Ok(false);
    };

    let g = group.to_string();
    let added = on_db(db, move |conn| Ok(crate::chunks::add(conn, &g, newest_day, oldest_day)?)).await?;
    println!("[SPLIT] {group}: backfill split into {added} day chunks over {carriers} servers");
    ctx.states.with(group, |st| st.reach_back_after = Some(Instant::now() + SPLIT_RECHECK));
    Ok(true)
}

/// Days older than split `group`'s oldest that a carrier keeps now: chunks
/// for them, see `chunks::reach_back`. The split's newest day is its newest
/// chunk's, not the date at the home cursor: the cursor stays where the
/// split was made, and home's retention moves past it.
async fn reach_back(ctx: &PassContext, db: &Db, group: &str, home: usize, bounds: (u64, u64)) -> Result<()> {
    let g = group.to_string();
    let Some(newest_day) = on_db(db, move |conn| Ok(crate::chunks::newest_day(conn, &g)?)).await? else {
        return Ok(());
    };
    let Some(Split { oldest_day, oldest_at, .. }) =
        split_days(ctx, group, home, bounds, Newest::Day(newest_day)).await?
    else {
        return Ok(());
    };
    let g = group.to_string();
    let todo = on_db(db, move |conn| Ok(crate::chunks::reach_back(conn, &g, oldest_day, oldest_at)?)).await?;
    if todo > 0 {
        println!("[SPLIT] {group}: a server goes back further, {todo} day chunks to do");
    }
    Ok(())
}

/// What a split of a group covers.
struct Split {
    newest_day: i64,
    oldest_day: i64,
    /// when the oldest article any carrier keeps was posted (unix seconds)
    oldest_at: i64,
    /// servers carrying the group
    carriers: usize,
}

/// Where a split of a group starts.
enum Newest {
    /// when the articles at the home cursor were posted (unix seconds), for a new split
    AtCursor(Vec<i64>),
    /// the newest day of a split there is (unix days)
    Day(i64),
}

/// The days a split of `group` from `newest` covers and how many servers
/// carry it. None when it cant split: home's first date is unknown, no
/// other indexing server carries the group, or the dates make no sense (see
/// `split_bounds`). `first` and `last` are home's low and high marks.
async fn split_days(
    ctx: &PassContext,
    group: &str,
    home: usize,
    (first, last): (u64, u64),
    newest: Newest,
) -> Result<Option<Split>> {
    // the oldest day is at least home's retention. first articles are
    // searched for from the low marks, which can lag far behind what a
    // server still keeps. a server whose first dates disagree (forged) could
    // be deeper or shallower than it looks: no split until it can be told
    let unsure =
        || println!("[SPLIT] {group}: first post dates that disagree (forged Date headers?), not split for now");
    let home_oldest = match ctx.pool.retention(home, group, first, last).await? {
        Retention::Since(t) => t,
        Retention::Empty => return Ok(None),
        Retention::Unsure(_) => {
            unsure();
            return Ok(None);
        }
    };
    let mut oldest_at = home_oldest;

    // other servers are best effort: one that fails is left out. one that
    // carries the group counts, its oldest date only if it has one
    let mut carriers = 1;
    let total_conns = ctx.pool.indexing_connections();
    for other in ctx.pool.indexing_servers().into_iter().filter(|&s| s != home) {
        if total_conns >= 50 && ctx.pool.connections(other) * 20 < total_conns {
            continue;
        }
        let Ok((_, low, high, _)) = ctx.pool.group_on(other, group).await else { continue };
        carriers += 1;
        match ctx.pool.retention(other, group, low, high).await {
            Ok(Retention::Since(t)) => oldest_at = oldest_at.min(t),
            Ok(Retention::Unsure(_)) => {
                unsure();
                return Ok(None);
            }
            Ok(Retention::Empty) | Err(_) => {}
        }
    }
    if carriers < 2 {
        return Ok(None);
    }

    let today = crate::chunks::unix_day(chrono::Utc::now().timestamp());
    let Some((newest_day, oldest_day)) = split_bounds(&newest, home_oldest, oldest_at, today) else {
        println!("[SPLIT] {group}: post dates that make no sense (forged Date headers?), the split is left as it is");
        return Ok(None);
    };
    // a time from before its day (the split doesnt go further back than its
    // newest day) starts the day
    let oldest_at = oldest_at.clamp(oldest_day * 86_400, (oldest_day + 1) * 86_400 - 1);
    Ok(Some(Split { newest_day, oldest_day, oldest_at, carriers }))
}

/// 2000-01-01: binary retention doesnt reach further back than this
const SPLIT_OLDEST_DAY: i64 = 10_957;

/// A split's (newest, oldest) days from `newest`, home's first article
/// (`home_oldest`) and the first of any carrier (`oldest_at`, unix seconds).
/// Date headers are the posters' to forge, and a split made from a forged
/// one would leave the group's history unindexed for good: a date at the
/// cursor before 2000-01-01, after `today`, or more than a day before home's
/// first article is left out (the latest left answers), and None when none
/// is left, the oldest is before 2000-01-01 or the newest day is before the
/// oldest. Clamping such a date into range would still make a bogus split
/// (a too old chunk every carrier gives back).
fn split_bounds(newest: &Newest, home_oldest: i64, oldest_at: i64, today: i64) -> Option<(i64, i64)> {
    use crate::chunks::unix_day;
    let newest_day = match newest {
        Newest::AtCursor(times) => times
            .iter()
            .filter(|&&t| t >= home_oldest - 86_400)
            .map(|&t| unix_day(t))
            .filter(|day| (SPLIT_OLDEST_DAY..=today).contains(day))
            .max()?,
        Newest::Day(day) => *day,
    };
    let oldest_day = unix_day(oldest_at);
    (oldest_day >= SPLIT_OLDEST_DAY && newest_day >= oldest_day).then_some((newest_day, oldest_day))
}

/// Once every indexing server turned a day chunk of `group` down as too old
/// for it, drop its chunks of days before the first post of all of them:
/// no server has anything from those, and a split made from forged first
/// dates can reach back years. Left alone, those chunks wait for a server
/// forever and the split never completes (its sweeps never run). Best
/// effort: a failure is only logged.
async fn drop_unkept_days(ctx: &PassContext, db: &Db, group: &str) {
    // every server: a small one can still have the older days, and what this finds gets deleted
    let first =
        ctx.pool.indexing_servers().into_iter().map(|i| ctx.states.first_post_day(group, &ctx.pool.host(i))).min();
    // i64::MIN: a server not asked yet (or a while ago)
    let Some(first) = first.filter(|&d| d != i64::MIN) else { return };
    let g = group.to_string();
    match on_db(db, move |conn| Ok(crate::chunks::drop_before(conn, &g, first)?)).await {
        Ok(0) => {}
        Ok(n) => println!("[CHUNK] {group}: {n} days older than any server keeps, dropped (forged Date headers?)"),
        Err(e) => println!("[CHUNK] {group}: couldnt drop the days no server keeps: {e:#}"),
    }
}

/// A day chunk's server doesnt carry its group (GROUP answered 411).
#[derive(Debug)]
pub struct NotCarried {
    pub group: String,
    pub host: String,
}

impl std::fmt::Display for NotCarried {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} isnt on {}", self.group, self.host)
    }
}

impl std::error::Error for NotCarried {}

/// A day chunk older than anything its server keeps of the group: the chunk
/// goes back for a server that has the day.
#[derive(Debug)]
pub struct TooOld {
    pub group: String,
    pub host: String,
    /// the oldest day the server keeps (unix days), `i64::MAX` when it has nothing
    pub oldest_day: i64,
}

impl std::fmt::Display for TooOld {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} on {} doesnt go back that far", self.group, self.host)
    }
}

impl std::error::Error for TooOld {}

/// how long a server's oldest day of a group is trusted before a chunk older
/// than it is tried there again
const KEEPS_RECHECK: Duration = Duration::from_secs(3600);

/// Claim the newest chunk for `host` of the split groups among `groups`,
/// none older than the server's oldest day in `oldest` (by group).
pub async fn claim_chunk(
    db: &Db,
    host: String,
    groups: std::collections::HashSet<String>,
    oldest: HashMap<String, i64>,
) -> Result<Option<crate::chunks::Claim>> {
    on_db(db, move |conn| {
        let split: Vec<(String, i64)> = crate::chunks::split_groups(conn)?
            .into_iter()
            .filter(|g| groups.contains(g))
            .map(|g| {
                let day = oldest.get(&g).copied().unwrap_or(i64::MIN);
                (g, day)
            })
            .collect();
        Ok(crate::chunks::claim(conn, &split, &host, chrono::Utc::now().timestamp())?)
    })
    .await
}

/// A split group whose day chunks are all done that `host`, not its home,
/// still has to sweep, among `groups`.
pub async fn sweep_to_take(db: &Db, host: String, groups: Vec<String>) -> Result<Option<String>> {
    on_db(db, move |conn| {
        for g in groups {
            if crate::chunks::chunks_done(conn, &g)? && !crate::chunks::swept(conn, &g, &host)? {
                return Ok(Some(g));
            }
        }
        Ok(None)
    })
    .await
}

/// One batch of the sweep of split `group` on `server`, a carrier that is
/// not its home, once its day chunks are done: like the home's (see
/// `run_pass`), the server's own cursor backfill from where its backfill
/// got (its high mark when it has none, where its newest chunk ended) down
/// to its first article. The chunks go by forgeable Date headers: when the
/// home keeps less than this server, what they missed of the days only this
/// one keeps is here. Noted as swept once at its first article (or when it
/// doesnt carry the group); the split is complete when every carrier is.
pub async fn run_sweep<P>(
    ctx: &PassContext,
    settings: &PassSettings,
    db: &Db,
    group: &str,
    server: usize,
    progress: &mut P,
) -> Result<Progress>
where
    P: FnMut(&Progress) + ?Sized,
{
    if crate::filters::group_blocked(group) {
        return Ok(Progress::default());
    }
    let (g, host) = (group.to_string(), ctx.pool.host(server));
    // noted as swept only if the split hasnt reached back since (see `set_swept`)
    let generation = {
        let g = g.clone();
        on_db(db, move |conn| Ok(crate::chunks::sweep_generation(conn, &g)?)).await?
    };
    let swept = || {
        let (g, host) = (g.clone(), host.clone());
        on_db(db, move |conn| Ok(crate::chunks::set_swept(conn, &g, &host, generation)?))
    };
    // this server alone, like a day chunk
    let (_count, first, last, _name) = match ctx.pool.group_on(server, group).await {
        Ok(info) => info,
        // only 411 says the server doesnt carry it: a 403 or 430 is a failure to retry
        Err(e) if crate::nntp::carries_not(&e) => {
            swept().await?;
            return Err(NotCarried { group: g, host }.into());
        }
        Err(e) => return Err(e.into()),
    };
    let (first, last) = (first as i64, last as i64);
    let key = cursor_key(&ctx.pool, server, group);
    let cursor = match load_cursors(ctx, db, &key, group, server).await? {
        Some(s) => s.backfill_cursor.min(last),
        None => {
            let k = key.clone();
            on_db(db, move |conn| Ok(db::init_group_state(conn, &k, last)?)).await?;
            last
        }
    };
    if cursor < first {
        swept().await?;
        println!("[SWEEP] {group} on {host} done");
        return Ok(Progress::default());
    }

    // the cursor backfill's batch, without touching the group's run state:
    // that is the home's
    let start = first.max(cursor - settings.batch_size.max(1) + 1);
    let pass = Pass { ctx, settings, db, group, server, key: key.clone() };
    let ranged = pass.process_range(start, cursor, "SWEEP", progress).await;
    // done down from the cursor: the cursor moves past it, failed or not
    if let Some(high) = ranged.high_done {
        on_db(db, move |conn| Ok(db::update_backfill_cursor(conn, &key, high as i64 - 1)?)).await?;
    }
    ranged.into_result()
}

/// a day chunk also takes this much on each side: post dates are only
/// roughly in article number order, duplicates are dropped when saved
pub const CHUNK_OVERLAP: i64 = 3600;

/// article numbers a day chunk reaches past each end its date searches put
/// it at, for neighbouring days done on other servers (see `chunk_range`)
pub const CHUNK_SAFETY: u64 = 5_000;

/// Index the day chunk `chunk` claimed (its day in unix days) on `server`,
/// for a group whose backfill is split into day chunks (see chunks.rs). The
/// chunk is marked done when the whole day is in, released again if stopped
/// part way or failing, or dropped part way (stopping drops a pass stuck on
/// the network). Either is skipped once another worker took the chunk over
/// (this one ran past CLAIM_TIMEOUT): the chunk is that worker's.
pub async fn run_chunk<P>(
    ctx: &PassContext,
    settings: &PassSettings,
    db: &Db,
    chunk: &crate::chunks::Claim,
    server: usize,
    progress: &mut P,
) -> Result<Progress>
where
    P: FnMut(&Progress) + ?Sized,
{
    let mut unfinished = GiveBackOnDrop { db, chunk, armed: true };
    let r = index_chunk(ctx, settings, db, chunk, server, progress).await;
    unfinished.armed = false;
    r
}

/// Gives a claimed chunk back when its run is dropped part way, instead of
/// it staying claimed until CLAIM_TIMEOUT (a quick restart would find it
/// taken). Every way the run ends on its own finishes or gives it back already.
struct GiveBackOnDrop<'a> {
    db: &'a Db,
    chunk: &'a crate::chunks::Claim,
    armed: bool,
}

impl Drop for GiveBackOnDrop<'_> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        // best effort and quick: one update, only while this claim still owns it
        let r = match self.db.conn.lock() {
            Ok(conn) => crate::chunks::release(&conn, self.chunk).map(|_| ()).map_err(anyhow::Error::from),
            Err(_) => Err(anyhow!("the database connection is poisoned")),
        };
        if let Err(e) = r {
            let (group, day) = (&self.chunk.group, self.chunk.day);
            println!("[CHUNK] {group} day {day}: couldnt give the chunk back when stopped: {e:#}");
        }
    }
}

/// Whether a day chunk that failed with `e` rests before it is tried again: not when this server lacks the day or the
/// group, which another server takes at once.
fn chunk_failure_rests(e: &anyhow::Error) -> bool {
    e.downcast_ref::<TooOld>().is_none() && e.downcast_ref::<NotCarried>().is_none()
}

/// A day chunk of `group` that only one server claims to have (its dates
/// reach back further than any other's) came out with no article dated in
/// that day: its dates went back further than its articles do (forged), and
/// each older day would take a chunk to find that out again. They are dropped; what those days hold is
/// the sweeps' (see `run_sweep`). Best effort: a failure is only logged.
async fn drop_older_days(db: &Db, group: &str, day: i64) {
    let g = group.to_string();
    match on_db(db, move |conn| Ok(crate::chunks::drop_before(conn, &g, day)?)).await {
        Ok(0) => {}
        Ok(n) => {
            println!("[CHUNK] {group} day {day} has no article dated in it, the {n} older days are left to the sweeps")
        }
        Err(e) => println!("[CHUNK] {group}: couldnt drop the days older than an empty one: {e:#}"),
    }
}

/// A day only this server has (`day`, of the window from `floor`, see
/// `chunks::Speculative`) is done with `dated` articles in it. When it was the
/// window's oldest and the window's days held enough a day, chunks for the
/// days before it, back to the server's first post (`oldest`). Best effort:
/// a failure is only logged.
async fn reach_further(db: &Db, group: &str, (_day, floor, oldest): (i64, i64, Option<i64>), dated: i64) {
    let first = oldest.map_or(floor, crate::chunks::unix_day);
    let g = group.to_string();
    let added = on_db(db, move |conn| {
        let Some(mut s) = crate::chunks::speculative(conn, &g)? else { return Ok(0) };
        // a day of a window the reach has moved past is not the current one's
        if floor != s.runner_up_day - s.reach {
            return Ok(0);
        }
        s.days += 1;
        s.articles += dated;
        // the window is judged when its last day is done, not when its oldest is
        let running = conn
            .prepare_cached("select 1 from backfill_chunks where grp = ? and day >= ? and day < ? and state != 2")?
            .exists(rusqlite::params![g, floor, floor + SPECULATIVE_WINDOW])?;
        let further = !running && s.articles >= SPECULATIVE_YIELD * s.days && first < floor;
        if further {
            // each extension has to hold enough a day on its own: a dense
            // first window doesnt pay for the sparse ones after it
            s.reach += SPECULATIVE_WINDOW;
            s.days = 0;
            s.articles = 0;
        }
        crate::chunks::set_speculative(conn, &g, &s)?;
        if !further {
            return Ok(0);
        }
        Ok(crate::chunks::add(conn, &g, floor - 1, (floor - SPECULATIVE_WINDOW).max(first))?)
    })
    .await;
    match added {
        Ok(0) => {}
        Ok(n) => println!("[CHUNK] {group}: the days only one server has hold articles, {n} more day chunks"),
        Err(e) => println!("[CHUNK] {group}: couldnt note the days only one server has: {e:#}"),
    }
}

/// `chunk` is done with `dated` articles dated in its day. Only when this
/// claim is what made it done does what the day showed count: a claim taken
/// over (gone stale while it ran) leaves the speculative window and the older
/// days to the worker that has it now. `alone`, `floor` and `oldest` are as
/// `index_chunk` found them.
async fn finish_chunk(
    db: &Db,
    chunk: &crate::chunks::Claim,
    (alone, floor, oldest): (bool, Option<i64>, Option<i64>),
    dated: i64,
) -> Result<()> {
    let (group, day) = (chunk.group.as_str(), chunk.day);
    let c = chunk.clone();
    let now = chrono::Utc::now().timestamp();
    if !on_db(db, move |conn| Ok(crate::chunks::finish(conn, &c, now)?)).await? {
        println!("[CHUNK] {group} day {day}: taken over by another worker, leaving it to that one");
        return Ok(());
    }
    if alone && dated == 0 {
        drop_older_days(db, group, day).await;
    } else if let Some(floor) = floor {
        reach_further(db, group, (day, floor, oldest), dated).await;
    }
    Ok(())
}

async fn index_chunk<P>(
    ctx: &PassContext,
    settings: &PassSettings,
    db: &Db,
    chunk: &crate::chunks::Claim,
    server: usize,
    progress: &mut P,
) -> Result<Progress>
where
    P: FnMut(&Progress) + ?Sized,
{
    let (group, day) = (chunk.group.as_str(), chunk.day);
    // give the chunk back after `e`, and return `e`: failing to give it back
    // is only logged (the claim goes stale and gets taken over). A day this
    // server doesnt have is the others' at once; any other failure rests the
    // chunk (see `chunks::FAIL_REST`), so a day that fails every time doesnt
    // hold every older one back
    let failed = async |e: anyhow::Error| {
        let c = chunk.clone();
        let rests = chunk_failure_rests(&e);
        let gave_back = on_db(db, move |conn| {
            Ok(if rests {
                crate::chunks::fail(conn, &c, chrono::Utc::now().timestamp())?
            } else {
                crate::chunks::release(conn, &c)?
            })
        });
        if let Err(r) = gave_back.await {
            println!("[CHUNK] {group} day {day}: couldnt give the chunk back: {r:#}");
        }
        Err(e)
    };

    // GROUP on this server alone: falling over would move where the group
    // lives, and only a 411 means the server doesnt carry it
    let found = match ctx.pool.group_on(server, group).await {
        Ok(info) => info,
        Err(e) if crate::nntp::carries_not(&e) => {
            // it has no post of it (see `drop_unkept_days`)
            ctx.states.not_carrying(group, &ctx.pool.host(server));
            // if it was noted as going back furthest, the oldest day goes to the next one
            let (g, host) = (group.to_string(), ctx.pool.host(server));
            if let Err(e) = on_db(db, move |conn| Ok(crate::chunks::forget_deepest(conn, &g, &host)?)).await {
                println!("[CHUNK] {group}: couldnt forget the server going back furthest: {e:#}");
            }
            return failed(NotCarried { group: group.to_string(), host: ctx.pool.host(server) }.into()).await;
        }
        Err(e) => return failed(e.into()).await,
    };
    let (_count, first, last, _name) = found;

    // a day this server doesnt keep from its start would come out empty or
    // cut short, though another server may have all of it: give it back, and
    // the server takes no day that old for a while. the split's oldest day is
    // the exception: no server keeps it whole, the one that goes back
    // furthest on it does what there is
    // (whether its first dates agree: see `goes_back_furthest`)
    let (oldest, sure) = match ctx.pool.retention(server, group, first, last).await {
        Ok(Retention::Since(t)) => (Some(t), true),
        Ok(Retention::Unsure(t)) => (Some(t), false),
        Ok(Retention::Empty) => (None, true),
        Err(e) => return failed(e.into()).await,
    };
    // from when the server is taken to keep days whole
    let mut keeps_from = oldest;
    // every carrier's first post is noted, a day it keeps or not: a forged
    // run in the dates must not leave `drop_unkept_days` without evidence
    ctx.states.first_post_on(group, &ctx.pool.host(server), oldest.map_or(i64::MAX, crate::chunks::unix_day));
    // this day is older than any other server goes back: only this server
    // may have it, on the word of its own (forgeable) dates
    let mut alone = false;
    // and then the first day the next deepest server keeps
    let mut runner_up_day = None;
    let keeps_day = match oldest {
        // a day older than the second furthest server goes back is the
        // furthest one's alone: a server only looks deep enough by the dates
        // its posters set
        Some(t) if t <= day * 86_400 => match goes_back_furthest(ctx, db, group, server, (t, sure)).await {
            Ok((true, runner_up)) => {
                alone = runner_up.is_none_or(|r| r >= (day + 1) * 86_400);
                runner_up_day = runner_up.filter(|_| alone).map(crate::chunks::unix_day);
                true
            }
            Ok((false, runner_up)) => {
                keeps_from = keeps_from.max(runner_up);
                runner_up.is_none_or(|r| r <= day * 86_400)
            }
            Err(e) => return failed(e).await,
        },
        Some(t) if crate::chunks::unix_day(t) == day => {
            let g = group.to_string();
            let split_oldest = match on_db(db, move |conn| Ok(crate::chunks::oldest_day(conn, &g)?)).await {
                Ok(d) => d,
                Err(e) => return failed(e).await,
            };
            if split_oldest != Some(day) {
                false
            } else {
                match goes_back_furthest(ctx, db, group, server, (t, sure)).await {
                    Ok((furthest, _)) => furthest,
                    Err(e) => return failed(e).await,
                }
            }
        }
        _ => false,
    };
    if !keeps_day {
        // the first day it keeps whole
        let oldest_day = keeps_from.map_or(i64::MAX, |t| crate::chunks::unix_day(t - 1) + 1);
        let host = ctx.pool.host(server);
        ctx.states.keep_from(group, &host, oldest_day);
        ctx.states.first_post_on(group, &host, oldest.map_or(i64::MAX, crate::chunks::unix_day));
        println!("[CHUNK] {group} day {day} is older than {host} keeps, leaving it to the other servers");
        // a server that doesnt keep this day keeps none of the split's oldest
        // either: if it was noted as going back furthest, its retention moved
        // on, and the oldest day goes to the next one
        let (g, h) = (group.to_string(), host.clone());
        if let Err(e) = on_db(db, move |conn| Ok(crate::chunks::forget_deepest(conn, &g, &h)?)).await {
            println!("[CHUNK] {group}: couldnt forget the server going back furthest: {e:#}");
        }
        drop_unkept_days(ctx, db, group).await;
        return failed(TooOld { group: group.to_string(), host, oldest_day }.into()).await;
    }
    // the split's days and, for its newest, how far down this server's
    // backfill got before the split (its high mark when it has no cursor)
    let (g, key) = (group.to_string(), cursor_key(&ctx.pool, server, group));
    let bounds = on_db(db, move |conn| {
        let days = crate::chunks::oldest_day(conn, &g)?.zip(crate::chunks::newest_day(conn, &g)?);
        Ok((days, db::get_group_state(conn, &key)?))
    })
    .await;
    let (days, cursor) = match bounds {
        Ok((Some(days), state)) => (days, state.map_or(last, |s| (s.backfill_cursor.max(0) as u64).min(last))),
        Ok((None, _)) => return failed(anyhow!("{group} has no day chunks")).await,
        Err(e) => return failed(e).await,
    };
    let (start, end) =
        match chunk_range(&ctx.pool, server, group, (first, last), day, days, (cursor, settings.chunk_safety)).await {
            Ok(range) => range,
            Err(e) => return failed(e.into()).await,
        };
    // days only this server has are bounded all together, see `chunks::Speculative`
    let floor = match runner_up_day {
        Some(r) => {
            let g = group.to_string();
            let dropped = on_db(db, move |conn| {
                let s = match crate::chunks::speculative(conn, &g)? {
                    Some(s) => crate::chunks::Speculative { runner_up_day: r, ..s },
                    None => {
                        crate::chunks::Speculative { runner_up_day: r, reach: SPECULATIVE_WINDOW, days: 0, articles: 0 }
                    }
                };
                crate::chunks::set_speculative(conn, &g, &s)?;
                let floor = r - s.reach;
                Ok((floor, crate::chunks::drop_before(conn, &g, floor)?))
            })
            .await;
            let floor = match dropped {
                Ok((floor, 0)) => floor,
                Ok((floor, n)) => {
                    println!(
                        "[CHUNK] {group}: {n} days further back than {} days before the other servers go back are left to the sweeps",
                        r - floor
                    );
                    floor
                }
                Err(e) => return failed(e).await,
            };
            // this one was among them (claimed, not done): nothing to do
            if day < floor {
                return Ok(Progress::default());
            }
            Some(floor)
        }
        None => None,
    };
    if start > end {
        finish_chunk(db, chunk, (alone, floor, oldest), 0).await?;
        return Ok(Progress::default());
    }

    let pass = Pass { ctx, settings, db, group, server, key: cursor_key(&ctx.pool, server, group) };
    let window = alone.then_some((day * 86_400, (day + 1) * 86_400));
    let ranged = pass.process_range_dated(start as i64, end as i64, "CHUNK", window, progress).await;
    match ranged {
        Ranged { error: Some(e), .. } => failed(e).await,
        Ranged { saved, complete: true, in_window, .. } => {
            finish_chunk(db, chunk, (alone, floor, oldest), in_window).await?;
            Ok(saved)
        }
        Ranged { saved, .. } => {
            // stopped part way, not failed: given back as it is
            let c = chunk.clone();
            on_db(db, move |conn| Ok(crate::chunks::release(conn, &c)?)).await?;
            Ok(saved)
        }
    }
}

/// The article numbers day chunk `day` of a split over `days` (oldest,
/// newest) covers on `server`, whose low and high marks are `first` and
/// `last`: from where the day starts to where the next one does, each found
/// by the same date search an hour early (`CHUNK_OVERLAP`), and to an hour
/// past the day when that is further. The oldest day starts at `first`, the
/// newest ends at `cursor`. Neighbouring days share where one ends and the
/// next starts, soo every number from `first` to `cursor` is some chunk's
/// however forged Dates move the searches: a forged Date only moves an
/// article into another chunk. Empty (start past end) when the day's posts
/// are all in its neighbours.
///
/// That holds on one server; neighbouring days done on different servers
/// search their own numbering, and around a hole a run of forged Dates can
/// move the two searches differently, leaving articles in neither. Each end
/// also reaches `safety` (`CHUNK_SAFETY`) numbers past where its search put it (the
/// articles a neighbour has too are dropped as duplicates when saved): a
/// shift up to that many articles is covered, a forged run moving a search
/// further can still leave a gap, for the carriers' sweeps once the
/// chunks are done (see `run_pass` and `run_sweep`).
pub async fn chunk_range(
    pool: &Pool,
    server: usize,
    group: &str,
    (first, last): (u64, u64),
    day: i64,
    (oldest_day, newest_day): (i64, i64),
    (cursor, safety): (u64, u64),
) -> crate::nntp::Result<(u64, u64)> {
    let starts = |day: i64| pool.article_at(server, group, first, last, day * 86_400 - CHUNK_OVERLAP);
    if day <= oldest_day && day >= newest_day {
        return Ok((first, cursor));
    }
    let next = starts(day + 1).await?;
    // a start past the next day's (forged Dates) is moved down to it
    let start = if day <= oldest_day { first } else { starts(day).await?.min(next).saturating_sub(safety).max(first) };
    let end = if day >= newest_day {
        cursor
    } else {
        let past = pool.article_at(server, group, first, last, (day + 1) * 86_400 + CHUNK_OVERLAP).await?;
        next.max(past).saturating_sub(1).saturating_add(safety).min(cursor)
    };
    Ok((start, end))
}

/// Whether `server`, which keeps split `group` from `oldest` (see
/// `Pool::retention`; with whether its first dates agree), goes back furthest of the indexing servers: the one to
/// index the split's oldest day, and the days before the second furthest
/// goes back to (returned with it, when there is one). Found by asking each
/// server how far back it keeps the group the first time, then kept, soo it
/// doesnt move as retention rolls on (asked again once that server is no
/// longer an indexing one, no longer carries the group, or no longer keeps
/// the day: `index_chunk` forgets it on a 411 or a day older than it keeps).
/// A server whose first dates disagree goes back furthest only when every
/// one's do: a forged run could make it look deeper than it is. A server
/// that cant be asked fails it: the chunk goes back and is tried again later.
async fn goes_back_furthest(
    ctx: &PassContext,
    db: &Db,
    group: &str,
    server: usize,
    oldest: (i64, bool),
) -> Result<(bool, Option<i64>)> {
    let host = ctx.pool.host(server);
    let servers = ctx.pool.indexing_servers();
    let g = group.to_string();
    let known =
        on_db(db, move |conn| Ok((crate::chunks::deepest(conn, &g)?, crate::chunks::runner_up(conn, &g)?))).await?;
    if let (Some(deepest), runner_up) = known
        && servers.iter().any(|&i| ctx.pool.host(i) == deepest)
    {
        return Ok((deepest == host, runner_up));
    }

    let total_conns = ctx.pool.indexing_connections();
    // every carrier's (whether unsure, since, host)
    let mut found = vec![(!oldest.1, oldest.0, host.clone())];
    for other in servers.into_iter().filter(|&i| i != server) {
        if total_conns >= 50 && ctx.pool.connections(other) * 20 < total_conns {
            continue;
        }
        let (_, low, high, _) = match ctx.pool.group_on(other, group).await {
            Ok(info) => info,
            Err(e) if matches!(e.code(), Some(403 | 411 | 430 | 500 | 501)) => continue,
            Err(e) => return Err(e.into()),
        };
        match ctx.pool.retention(other, group, low, high).await? {
            Retention::Since(t) => found.push((false, t, ctx.pool.host(other))),
            Retention::Unsure(t) => found.push((true, t, ctx.pool.host(other))),
            Retention::Empty => {}
        }
    }
    // the deepest of those whose dates agree, a tie to `server` (asked
    // first); the days before the next deepest (by the same order) are its
    // alone
    found.sort_by_key(|(unsure, t, h)| (*unsure, *t, *h != host));
    let (_, since, deepest) = found.remove(0);
    let runner_up = found.first().map(|(_, t, _)| *t);
    let (g, d) = (group.to_string(), deepest.clone());
    on_db(db, move |conn| {
        crate::chunks::set_deepest(conn, &g, &d, since)?;
        if let Some(r) = runner_up {
            crate::chunks::set_runner_up(conn, &g, r)?;
        }
        Ok(())
    })
    .await?;
    println!("[CHUNK] {group}: {deepest} goes back furthest, it does the oldest day");
    Ok((deepest == host, runner_up))
}

struct Pass<'a> {
    ctx: &'a PassContext,
    settings: &'a PassSettings,
    db: &'a Db,
    group: &'a str,
    server: usize,
    key: String,
}

impl Pass<'_> {
    async fn live<P>(&self, state: db::GroupState, last: i64, progress: &mut P) -> Result<Progress>
    where
        P: FnMut(&Progress) + ?Sized,
    {
        let group = self.group;
        self.ctx.states.with(group, |st| st.backfilling = false);
        let start = state.live_cursor + 1;

        // nothing new since last check
        if start > last {
            if self.ctx.verbose {
                println!("no new articles");
            }

            let newly_idle = self.ctx.states.with(group, |st| !std::mem::replace(&mut st.idle, true));
            if newly_idle {
                println!("[LIVE] {group} no new articles, idle");
            }

            return Ok(Progress::default());
        }

        let end = last.min(start + self.settings.batch_size.max(1) - 1);
        let ranged = self.process_range(start, end, "LIVE", progress).await;

        // done up from the cursor: the cursor moves past it, failed or not
        if let Some(low) = ranged.low_done {
            let key = self.key.clone();
            on_db(self.db, move |conn| Ok(db::update_live_cursor(conn, &key, low as i64)?)).await?;
        }
        ranged.into_result()
    }

    async fn backfill<P>(&self, state: db::GroupState, first: i64, last: i64, progress: &mut P) -> Result<Progress>
    where
        P: FnMut(&Progress) + ?Sized,
    {
        let group = self.group;
        let end = state.backfill_cursor.min(last);

        if end < first {
            // only idle when the live side is caught up too, otherwise dynamic
            // mode would drain a live backlog at one batch per idle sleep
            let live_caught_up = state.live_cursor >= last || self.settings.mode == "backfill";

            let newly_idle = self.ctx.states.with(group, |st| {
                st.backfilling = false;
                st.phase = Phase::Live;
                let newly = !st.idle && live_caught_up;
                if newly {
                    st.idle = true;
                }
                newly
            });

            if newly_idle {
                println!("[BACKFILL] {group} {end} < first {first}, nothing to backfill, idle");
            }

            return Ok(Progress::default());
        }

        // grab a chunk going backwards from the cursor
        let start = first.max(end - self.settings.batch_size.max(1) + 1);
        let ranged = self.process_range(start, end, "BACKFILL", progress).await;

        // done down from the cursor: the cursor moves past it, failed or not
        if let Some(high) = ranged.high_done {
            let key = self.key.clone();
            on_db(self.db, move |conn| Ok(db::update_backfill_cursor(conn, &key, high as i64 - 1)?)).await?;
        }
        let saved = ranged.into_result()?;
        self.ctx.states.with(group, |st| st.backfilling = true);
        Ok(saved)
    }

    /// Index `start..=end`. The range is cut into `request_size` slices that
    /// stream in over the server's connections; each slice is parsed, named and
    /// saved as soon as it lands while the rest keep downloading. Backfill takes
    /// the newest slices first.
    ///
    /// A slice failing stops new ones from starting, but the ones that made it
    /// are still saved, and `Ranged` says how far the range is done from
    /// either end, soo a cursor can move past them and they arent fetched again.
    async fn process_range<P>(&self, start: i64, end: i64, kind: &str, progress: &mut P) -> Ranged
    where
        P: FnMut(&Progress) + ?Sized,
    {
        self.process_range_dated(start, end, kind, None, progress).await
    }

    /// Moves the cursor of `side` past what `finished` holds done of `start..=end` next to it (see `Finished::ends`),
    /// when that is further than `moved`, the last place this pass moved it to. One that can't be written now is
    /// moved by the next slice or once the pass ends.
    async fn advance(&self, side: Side, finished: &Finished, (start, end): (i64, i64), moved: &mut Option<u64>) {
        let (low, high) = finished.ends(start.max(0) as u64, end.max(0) as u64);
        let Some(to) = (match side {
            Side::Up => low,
            Side::Down => high,
        }) else {
            return;
        };
        if *moved == Some(to) {
            return;
        }
        let key = self.key.clone();
        let saved = on_db(self.db, move |conn| {
            match side {
                Side::Up => db::update_live_cursor(conn, &key, to as i64)?,
                Side::Down => db::update_backfill_cursor(conn, &key, to as i64 - 1)?,
            }
            Ok(())
        })
        .await;
        if saved.is_ok() {
            *moved = Some(to);
        }
    }

    /// `process_range`, also how many articles retrieved are dated within
    /// `window` (unix seconds, from up to before), 0 when there is none.
    async fn process_range_dated<P>(
        &self,
        start: i64,
        end: i64,
        kind: &str,
        window: Option<(i64, i64)>,
        progress: &mut P,
    ) -> Ranged
    where
        P: FnMut(&Progress) + ?Sized,
    {
        let mut in_window = 0i64;
        let (pool, group) = (&self.ctx.pool, self.group);
        if crate::filters::group_blocked(group) {
            let (start, end) = (start.max(0) as u64, end.max(0) as u64);
            let saved = Progress::default();
            return Ranged {
                saved,
                complete: true,
                in_window,
                low_done: Some(end),
                high_done: Some(start),
                error: None,
            };
        }
        // no slice bigger than the whole unsaved budget: it would hold more
        // headers than the cap allows
        let size = self.settings.request_size.min(pool.max_unsaved() as u64);
        let slices = make_slices(start.max(0) as u64, end.max(0) as u64, size, kind == "BACKFILL");
        let total = slices.len();

        let mut rx = pool.stream_headers(group, self.server, slices, self.ctx.stop.clone());
        let mut saved = Progress::default();
        let mut done = 0;
        let mut finished = Finished::default();
        let mut error: Option<anyhow::Error> = None;
        // a failed save stops the saving, a failed fetch only stops the fetching
        let mut save_failed = false;
        // dated ends of what this pass saved, for the stats dashboard's history numbers
        let (mut low, mut high): (Option<db::Dated>, Option<db::Dated>) = (None, None);

        // the cursor this pass moves goes on past what's done as the slices come in, not only once the pass ends: with
        // the writers busy a pass can take longer than prod runs between restarts, and one cut short kept nothing
        let side = match kind {
            "LIVE" => Some(Side::Up),
            "BACKFILL" | "SWEEP" => Some(Side::Down),
            _ => None,
        };
        let mut moved = None;
        loop {
            if let Some(side) = side {
                self.advance(side, &finished, (start, end), &mut moved).await;
            }
            let Some(slice) = rx.recv().await else { break };
            // the slice's room in the unsaved budget goes back once it's parsed
            // (or skipped)
            let unsaved = slice.unsaved;
            let headers: Vec<Overview> = match slice.result {
                Ok(h) => {
                    self.ctx.states.read(group, &self.key, (slice.start, slice.end));
                    h
                }
                // 423 (or 420) = no articles in that slice
                Err(e) if e.is_empty_range() => {
                    self.ctx.states.read(group, &self.key, (slice.start, slice.end));
                    done += 1;
                    finished.add(slice.start, slice.end);
                    continue;
                }
                // a 5xx: passed over only once refused for a while (see `REFUSED_FOR`), until then
                // the slice fails the pass like any other failure and is asked again later
                Err(e) if e.is_permanent() => {
                    let range = (slice.start, slice.end);
                    if self.ctx.states.refused(group, &self.key, range, Instant::now()) {
                        println!(
                            "[{kind}] {group} {}-{} refused for over {}m ({e}), skipping",
                            slice.start,
                            slice.end,
                            REFUSED_FOR.as_secs() / 60
                        );
                        done += 1;
                        finished.add(slice.start, slice.end);
                    } else {
                        println!("[{kind}] {group} {}-{} refused ({e}), trying it again later", slice.start, slice.end);
                        error.get_or_insert_with(|| anyhow::Error::new(e));
                    }
                    continue;
                }
                Err(e) => {
                    error.get_or_insert_with(|| anyhow::Error::new(e));
                    continue;
                }
            };

            if save_failed {
                continue;
            }

            if let Some((from, to)) = window {
                in_window += headers
                    .iter()
                    .filter(|h| crate::dates::posted_timestamp(&h.date).is_some_and(|t| (from..to).contains(&t)))
                    .count() as i64;
            }
            let dated = slice_date(&headers);
            match save_slice(pool, self.db, group, headers, unsaved).await {
                Ok(p) => {
                    if let Some(d) = dated {
                        low = low.filter(|l| l.0 <= d.0).or(Some(d));
                        high = high.filter(|h| h.0 >= d.0).or(Some(d));
                    }
                    saved.add(&p);
                    done += 1;
                    finished.add(slice.start, slice.end);
                    progress(&p);
                }
                Err(e) => {
                    save_failed = true;
                    error.get_or_insert(e);
                }
            }
        }

        if let (Some(low), Some(high)) = (low, high) {
            let key = self.key.clone();
            if let Err(e) = on_db(self.db, move |conn| Ok(db::save_group_dates(conn, &key, low, high)?)).await {
                error.get_or_insert(e);
            }
        }

        if error.is_none() && saved.articles == 0 && done == total {
            println!("[{kind}] {group} {start}-{end} empty, skipping");
        }

        if saved.skipped > 0 {
            println!("[{kind}] {group} {start}-{end} gathered {} articles obfuscated per article", saved.skipped);
        }

        if saved.articles > 0 {
            self.ctx.states.with(group, |st| st.idle = false);
        }

        if self.ctx.verbose {
            println!("[{kind}] {group} {} headers in {done}/{total} slices", saved.articles);
        }

        // failed or stopped early: what got saved stays, the cursor moves past
        // the done run next to it and waits for the rest
        let (low_done, high_done) = finished.ends(start.max(0) as u64, end.max(0) as u64);
        Ranged { saved, complete: done == total, in_window, low_done, high_done, error }
    }
}

/// `group`'s articles posted from `from` to `to` (unix seconds) on `server`, read and saved like any pass: the
/// headers of a time window another server's listing left out (see `repair`). No cursor moves. A window of more than
/// `max_articles` isn't read.
pub async fn reread_window(
    ctx: &PassContext,
    settings: &PassSettings,
    db: &Db,
    group: &str,
    server: usize,
    (from, to): (i64, i64),
    max_articles: u64,
) -> Result<Progress> {
    let (_, first, last, _) = ctx.pool.group_on(server, group).await?;
    if last < first {
        return Ok(Progress::default());
    }
    let start = ctx.pool.article_at(server, group, first, last, from).await?;
    let end = ctx.pool.article_at(server, group, first, last, to).await?.saturating_sub(1).min(last);
    if end < start {
        return Ok(Progress::default());
    }
    if end - start + 1 > max_articles {
        anyhow::bail!("{group}: {} articles from {start} to {end}, more than a window takes", end - start + 1);
    }
    // a key no cursor has: the dates this pass finds are noted nowhere
    let key = format!("repair:{group}@{}", ctx.pool.host(server));
    let pass = Pass { ctx, settings, db, group, server, key };
    let mut quiet = no_progress;
    pass.process_range(start as i64, end as i64, "repair", &mut quiet).await.into_result()
}

/// Which way a pass's cursor goes: up (live, from the cursor to the newest) or down (backfill and sweeps).
#[derive(Clone, Copy)]
enum Side {
    Up,
    Down,
}

/// How a range pass went. `low_done` / `high_done`: the range is done from
/// its start up to `low_done`, and from `high_done` up to its end (None when
/// its first, or its last, slice isnt done): what a cursor going up, or going
/// down, can move past even when the pass failed or was stopped.
struct Ranged {
    saved: Progress,
    /// every slice done: the whole range, and its cursor can move
    complete: bool,
    in_window: i64,
    low_done: Option<u64>,
    high_done: Option<u64>,
    /// the first failure; what got saved before and after it stays saved
    error: Option<anyhow::Error>,
}

impl Ranged {
    /// the progress, or the failure (once the caller moved its cursor)
    fn into_result(self) -> Result<Progress> {
        match self.error {
            Some(e) => Err(e),
            None => Ok(self.saved),
        }
    }
}

/// The slices of a range that are done (saved, empty or not available).
#[derive(Default)]
struct Finished(Vec<(u64, u64)>);

impl Finished {
    fn add(&mut self, start: u64, end: u64) {
        self.0.push((start, end));
    }

    /// How far `start..=end` is done from each end: the last number of the
    /// done run starting at `start`, the first of the one ending at `end`.
    fn ends(&self, start: u64, end: u64) -> (Option<u64>, Option<u64>) {
        let mut done = self.0.clone();
        done.sort_unstable();

        let mut low = None;
        let mut next = start;
        for &(a, b) in &done {
            if a != next {
                break;
            }
            low = Some(b);
            match b.checked_add(1) {
                Some(n) => next = n,
                None => break,
            }
        }

        let mut high = None;
        let mut prev = end;
        for &(a, b) in done.iter().rev() {
            if b != prev {
                break;
            }
            high = Some(a);
            match a.checked_sub(1) {
                Some(n) => prev = n,
                None => break,
            }
        }
        (low, high)
    }
}

/// The middle article number of a slice and when its articles were posted: the
/// median of a sample of their dates, soo a few forged or odd dates dont count.
fn slice_date(headers: &[Overview]) -> Option<db::Dated> {
    let first = headers.iter().map(|h| h.number).min()?;
    let last = headers.iter().map(|h| h.number).max()?;
    let step = (headers.len() / 64).max(1);
    let mut posted: Vec<i64> =
        headers.iter().step_by(step).filter_map(|h| crate::dates::posted_timestamp(&h.date)).collect();
    if posted.is_empty() {
        return None;
    }
    let mid = posted.len() / 2;
    let (_, median, _) = posted.select_nth_unstable(mid);
    Some((((first + last) / 2) as i64, *median))
}

/// Parse one slice into releases, look up real names, save.
/// `unsaved` is the slice's room in the unsaved budget: the parsed releases
/// stand for its headers until they are committed, soo it goes back only then.
async fn save_slice(
    pool: &Arc<Pool>,
    db: &Db,
    group: &str,
    headers: Vec<Overview>,
    unsaved: crate::nntp::Unsaved,
) -> Result<Progress> {
    let articles = headers.len() as i64;
    if crate::filters::group_blocked(group) {
        return Ok(Progress { articles, ..Progress::default() });
    }
    let span = crate::metrics::span("indexer.save_slice");
    let saved = parse_and_save(pool, db, group, headers, unsaved).await;
    if saved.is_err() {
        span.failed();
    }
    saved
}

async fn parse_and_save(
    pool: &Arc<Pool>,
    db: &Db,
    group: &str,
    headers: Vec<Overview>,
    unsaved: crate::nntp::Unsaved,
) -> Result<Progress> {
    let articles = headers.len() as i64;
    crate::profile::SLICES.add(1);
    crate::profile::HEADERS.add(articles as u64);

    // parsing is cpu work, keep it off the async threads
    let t = std::time::Instant::now();
    let parsing = {
        let group = group.to_string();
        move || {
            let _span = crate::metrics::span("indexer.parse_slice");
            let (articles, uploads, skipped) = crate::filters::gather_per_article(&group, headers_to_articles(headers));
            let mut releases: Vec<Release> =
                crate::collections::group_articles(&group, articles).into_values().collect();
            releases.extend(uploads);
            (releases, skipped)
        }
    };
    let (mut releases, skipped) =
        tokio::task::spawn_blocking(parsing).await.map_err(|e| anyhow!("parse task failed: {e}"))?;
    crate::profile::PARSE.add_since(t);
    crate::profile::Load::add_since(&crate::profile::LOAD.parse_ns, t);

    // real names from par2/nfo bodies, every release looked up concurrently,
    // a few first segments each (see `name_sources`)
    let t = std::time::Instant::now();
    // once the stage has a body budget it reads them, not the save path
    let names = if crate::archive::inline_lookups() {
        let _span = crate::metrics::span("indexer.first_names");
        first_names(pool.clone(), releases.iter().map(name_sources).collect()).await
    } else {
        Vec::new()
    };
    crate::profile::NAMES.add_since(t);

    let mut bytes = 0;
    for (i, release) in releases.iter_mut().enumerate() {
        release.display_name = names.get(i).cloned().flatten();
        release.complete = is_complete(&release.articles);
        release.group = group.to_string();
        release.poster = release.articles[0].author.clone();
        release.date = release.articles[0].date.clone();

        bytes += release.articles.iter().map(|a| a.bytes).filter(|b| *b > 0).sum::<i64>();
    }

    let count = releases.len() as i64;
    {
        let span = crate::metrics::span("indexer.wait_for_save");
        let saved = db.save(releases).await;
        if saved.is_err() {
            span.failed();
        }
        saved?;
    }
    // the parsed releases stood for these headers until they were committed: the room goes back only now, soo
    // slices waiting on a slow writer stay inside `max_unsaved_headers`
    drop(unsaved);

    Ok(Progress { articles, bytes, releases: count, skipped })
}

/// bodies a release's name lookup tries at most
const MAX_NAME_BODIES: usize = 3;

/// Bodies worth fetching for a real name: base par2 files first, then nfos,
/// only their first segment (where par2 and nfo files keep what names a
/// release, and it comes once per file, so a release is looked up once
/// however many slices it spans) and `MAX_NAME_BODIES` at most. A big
/// obfuscated par2 comes in thousands of segments of most of a MB each;
/// trying them one after another held a slice for minutes.
fn name_sources(release: &Release) -> Vec<(String, Extract)> {
    type Matches = fn(&str) -> bool;
    let sources: [(Matches, Extract); 2] = [(par2::is_base_par2, par2::display_name), (nfo::is_nfo, nfo::display_name)];

    sources
        .iter()
        .flat_map(|(matches, extract)| {
            release
                .articles
                .iter()
                .filter(|a| a.part.unwrap_or(1) == 1 && matches(&a.subject))
                .map(|a| (a.message_id.clone(), *extract))
        })
        .take(MAX_NAME_BODIES)
        .collect()
}

/// `start..=end` cut into `size` long slices, newest first when `descending`.
pub fn make_slices(start: u64, end: u64, size: u64, descending: bool) -> Vec<(u64, u64)> {
    let size = size.max(1);
    let mut slices = Vec::new();
    let mut a = start;

    while a <= end {
        let b = end.min(a.saturating_add(size - 1));
        slices.push((a, b));
        if b == u64::MAX {
            break;
        }
        a = b + 1;
    }

    if descending {
        slices.reverse();
    }
    slices
}

/// One group at a time from sync code, on the pool's active server.
/// The background indexer runs many passes at once instead (see `bg_indexer`).
pub struct Indexer {
    pub client: BlockingPool,
    pub mode: String,
    pub verbose: bool,
    pub state: RunStates,
    pub last_batch_articles: i64,
    pub last_batch_bytes: i64,
    pub last_batch_releases: i64,
    /// article numbers per pass over a group
    pub batch_size: i64,
    /// article numbers per XOVER request
    pub request_size: u64,
    /// set to stop handing out new slices, the current pass ends without moving cursors
    pub stop: Arc<AtomicBool>,
    db: Db,
}

impl Indexer {
    pub fn new(client: BlockingPool, mode: &str, conn: Connection) -> Self {
        let defaults = PassSettings::default();
        Indexer {
            client,
            mode: mode.to_string(),
            verbose: false,
            state: RunStates::default(),
            last_batch_articles: 0,
            last_batch_bytes: 0,
            last_batch_releases: 0,
            batch_size: defaults.batch_size,
            request_size: defaults.request_size,
            stop: Arc::new(AtomicBool::new(false)),
            db: shared_db(conn),
        }
    }

    pub fn is_idle(&self, group: &str) -> bool {
        self.state.is_idle(group)
    }

    pub fn is_backfilling(&self, group: &str) -> bool {
        self.state.is_backfilling(group)
    }

    pub fn all_idle(&self, groups: &[String]) -> bool {
        self.state.all_idle(groups)
    }

    /// switching modes starts every group over in the backfill phase
    pub fn set_mode(&mut self, mode: &str) {
        self.mode = mode.to_string();
        self.state.reset();
    }

    /// One pass over `group` with no progress reporting.
    pub fn index_group(&mut self, group: &str) -> Result<()> {
        self.index_group_with(group, &mut no_progress)
    }

    /// One pass over `group`: a batch of live or backfill articles depending
    /// on the mode. `progress` hears about every slice as it is saved.
    pub fn index_group_with(&mut self, group: &str, progress: &mut dyn FnMut(&Progress)) -> Result<()> {
        self.last_batch_articles = 0;
        self.last_batch_bytes = 0;
        self.last_batch_releases = 0;
        if crate::filters::group_blocked(group) {
            return Ok(());
        }

        let ctx = PassContext {
            pool: self.client.pool.clone(),
            states: self.state.clone(),
            stop: self.stop.clone(),
            verbose: self.verbose,
        };
        let settings = PassSettings {
            mode: self.mode.clone(),
            batch_size: self.batch_size,
            request_size: self.request_size,
            ..PassSettings::default()
        };
        let db = self.db.clone();

        let saved = self.client.block_on(async {
            // the active server, which moves when another server has to carry the group
            ctx.pool.select_group(group).await?;
            let server = ctx.pool.active_index();
            run_pass(&ctx, &settings, &db, group, server, progress).await
        })?;

        self.last_batch_articles = saved.articles;
        self.last_batch_bytes = saved.bytes;
        self.last_batch_releases = saved.releases;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::Article;

    #[test]
    fn a_refused_slice_is_passed_over_only_after_an_hour_and_three_refusals() {
        let states = RunStates::default();
        let t0 = Instant::now();
        let refused = |range, at| states.refused("g", "g@a", range, t0 + at);
        assert!(!refused((100, 199), Duration::ZERO), "the first refusal is tried again");
        assert!(!refused((100, 199), Duration::from_secs(10)));
        assert!(!refused((150, 249), Duration::from_secs(20)), "three refusals, but not an hour yet");
        assert!(!states.refused("g", "g@b", (100, 199), t0 + REFUSED_FOR), "another server's numbers count apart");
        assert!(!refused((300, 399), REFUSED_FOR), "a range not refused before starts over");
        assert!(refused((120, 130), REFUSED_FOR), "an hour after the first, overlapping ranges are passed over");
        assert!(refused((100, 199), REFUSED_FOR + REFUSED_FOR / 2), "and stay passed over while asked again");
        assert!(!refused((90, 199), REFUSED_FOR * 2), "but not past what was passed over");
        assert!(!refused((100, 199), REFUSED_FOR * 3), "and forgotten once not asked for the hour");

        // a read in between ends it
        assert!(!refused((300, 399), REFUSED_FOR + Duration::from_secs(1)));
        states.read("g", "g@a", (250, 449));
        assert!(!refused((300, 399), REFUSED_FOR * 3), "read since: counted from the start again");

        // a read of part of a refused range ends only that part
        assert!(!refused((1000, 1099), Duration::ZERO));
        assert!(!refused((1000, 1099), Duration::from_secs(10)));
        states.read("g", "g@a", (950, 1049));
        assert!(refused((1050, 1149), REFUSED_FOR), "the rest kept its hour");
        assert!(!refused((1000, 1049), REFUSED_FOR), "the part read starts over");

        // one never asked again (past retention) doesnt keep its room for good
        assert!(!refused((5000, 5099), REFUSED_FOR));
        assert!(!refused((5000, 5099), REFUSED_FOR * 2));
        assert!(!refused((5000, 5099), REFUSED_FOR * 2 + REFUSED_IDLE), "forgotten: starts over");
    }

    #[test]
    fn more_refused_slices_than_are_kept_still_reach_the_hour() {
        let states = RunStates::default();
        let t0 = Instant::now();
        let slices = REFUSED_KEPT as u64 * 2;
        let pass =
            |at: Duration| (0..slices).filter(|i| states.refused("g", "g@a", (i * 100, i * 100 + 99), t0 + at)).count();
        // every pass asks them all again, as a day chunk read again whole does:
        // the first ones keep their history
        assert_eq!(pass(Duration::ZERO), 0);
        assert_eq!(pass(Duration::from_secs(60)), 0);
        assert_eq!(pass(REFUSED_FOR), REFUSED_KEPT, "the ones kept are passed over after the hour");
        // the rest were noted as room freed: their hour runs from then, and
        // the ones passed over stay passed over meanwhile
        assert_eq!(pass(REFUSED_FOR + Duration::from_secs(60)), REFUSED_KEPT);
        assert_eq!(pass(REFUSED_FOR * 2), 2 * REFUSED_KEPT, "an hour later every slice is passed over");
    }

    #[test]
    fn a_name_lookup_tries_first_segments_only_and_a_few_bodies_at_most() {
        let article = |n: u64, file: &str, part: i64| Article {
            number: n,
            subject: format!(r#""{file}" yEnc ({part}/900)"#),
            message_id: format!("<{n}>"),
            filename: Some(file.into()),
            part: Some(part),
            ..Default::default()
        };
        let ids = |r: &Release| name_sources(r).into_iter().map(|(id, _)| id).collect::<Vec<_>>();

        // a big par2 with its first segment, and an nfo
        let mut big = Release {
            articles: (0..900).map(|i| article(i, "x.par2", 900 - i as i64)).collect(),
            ..Default::default()
        };
        big.articles.push(article(1000, "x.nfo", 1));
        assert_eq!(ids(&big), ["<899>", "<1000>"]);
        // seen part way, without its first segment: nothing worth a body
        big.articles.retain(|a| a.part != Some(1));
        assert!(ids(&big).is_empty());

        // many par2 files: no more than MAX_NAME_BODIES of them
        let many =
            Release { articles: (0..10).map(|i| article(i, &format!("f{i}.par2"), 1)).collect(), ..Default::default() };
        assert_eq!(ids(&many).len(), MAX_NAME_BODIES);
    }

    #[test]
    fn finished_slices_count_from_either_end_up_to_the_first_gap() {
        let mut f = Finished::default();
        assert_eq!(f.ends(1, 40), (None, None));
        f.add(31, 40);
        f.add(1, 10);
        f.add(21, 30);
        assert_eq!(f.ends(1, 40), (Some(10), Some(21)), "11-20 missing");
        f.add(11, 20);
        assert_eq!(f.ends(1, 40), (Some(40), Some(1)));
        let mut top = Finished::default();
        top.add(u64::MAX - 9, u64::MAX);
        assert_eq!(top.ends(0, u64::MAX), (None, Some(u64::MAX - 9)));
        let mut bottom = Finished::default();
        bottom.add(0, 9);
        assert_eq!(bottom.ends(0, u64::MAX), (Some(9), None));
    }

    /// `#key`s that differ by case are two servers, and the host in a key is
    /// lowercased as before
    #[test]
    fn cursor_keys_keep_the_case_of_a_servers_key() {
        let mut upper = crate::config::UsenetServer::new("A.Example", "u", "p", 563);
        upper.key = Some("Block".into());
        let mut lower = upper.clone();
        lower.key = Some("block".into());
        let plain = crate::config::UsenetServer::new("B.Example", "u", "p", 563);
        let pool = Pool::new(&[upper, lower, plain]);
        assert_eq!(cursor_key(&pool, 0, "g"), "g@a.example#Block");
        assert_eq!(cursor_key(&pool, 1, "g"), "g@a.example#block");
        assert_eq!(cursor_key(&pool, 2, "g"), "g@b.example");
    }

    /// a lone server keeps the plain group name, unless it has a `#key`
    #[test]
    fn a_lone_keyed_server_keeps_its_keyed_cursor() {
        let plain = crate::config::UsenetServer::new("A.Example", "u", "p", 563);
        let mut keyed = crate::config::UsenetServer::new("B.Example", "u", "p", 563);
        keyed.key = Some("Block".into());
        assert_eq!(cursor_key(&Pool::new(&[plain]), 0, "g"), "g");
        assert_eq!(cursor_key(&Pool::new(&[keyed]), 0, "g"), "g@b.example#Block");
    }

    #[test]
    fn each_speculative_window_has_to_hold_enough_on_its_own() {
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        rt.block_on(each_speculative_window());
    }

    async fn each_speculative_window() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        db::create_db_at(&main).unwrap();
        let db = shared_db(db::open_at(&main).unwrap());
        let (r, g) = (20_000, "g");
        let start = crate::chunks::Speculative { runner_up_day: r, reach: SPECULATIVE_WINDOW, days: 0, articles: 0 };
        on_db(&db, move |c| Ok(crate::chunks::set_speculative(c, g, &start)?)).await.unwrap();
        let floor = r - SPECULATIVE_WINDOW;
        // a dense window: its oldest day is done with plenty of articles
        reach_further(&db, g, (floor, floor, Some(0)), 100_000).await;
        let s = on_db(&db, move |c| Ok(crate::chunks::speculative(c, g)?)).await.unwrap().unwrap();
        assert_eq!((s.reach, s.days, s.articles), (2 * SPECULATIVE_WINDOW, 0, 0));
        // the next, sparse one doesnt ride on that: no further chunks
        let floor = r - s.reach;
        reach_further(&db, g, (floor, floor, Some(0)), 1).await;
        let s = on_db(&db, move |c| Ok(crate::chunks::speculative(c, g)?)).await.unwrap().unwrap();
        assert_eq!(s.reach, 2 * SPECULATIVE_WINDOW);
    }

    #[test]
    fn a_speculative_window_is_judged_once_every_day_of_it_is_done() {
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        rt.block_on(a_window_judged_when_done());
    }

    async fn a_window_judged_when_done() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        db::create_db_at(&main).unwrap();
        let db = shared_db(db::open_at(&main).unwrap());
        let (r, g) = (20_000, "g");
        let floor = r - SPECULATIVE_WINDOW;
        let start = crate::chunks::Speculative { runner_up_day: r, reach: SPECULATIVE_WINDOW, days: 0, articles: 0 };
        on_db(&db, move |c| {
            // three days of the window, one older
            crate::chunks::add(c, g, floor + 2, floor)?;
            Ok(crate::chunks::set_speculative(c, g, &start)?)
        })
        .await
        .unwrap();
        let window = || on_db(&db, move |c| Ok(crate::chunks::speculative(c, g)?.unwrap()));
        let done =
            |day: i64| on_db(&db, move |c| Ok(c.execute("update backfill_chunks set state = 2 where day = ?", [day])?));

        // the floor day is done and dense, the rest of the window is still running
        done(floor).await.unwrap();
        reach_further(&db, g, (floor, floor, Some(0)), 100_000).await;
        let s = window().await.unwrap();
        assert_eq!((s.reach, s.days, s.articles), (SPECULATIVE_WINDOW, 1, 100_000), "counted, not judged yet");

        // the last one in judges the window on all of them
        done(floor + 1).await.unwrap();
        reach_further(&db, g, (floor + 1, floor, Some(0)), 1).await;
        done(floor + 2).await.unwrap();
        reach_further(&db, g, (floor + 2, floor, Some(0)), 1).await;
        let s = window().await.unwrap();
        assert_eq!((s.reach, s.days, s.articles), (2 * SPECULATIVE_WINDOW, 0, 0));

        // a day of the window before it, finishing late, is not the new window's
        reach_further(&db, g, (floor + 2, floor, Some(0)), 100_000).await;
        let s = window().await.unwrap();
        assert_eq!((s.reach, s.days, s.articles), (2 * SPECULATIVE_WINDOW, 0, 0), "ignored");
    }

    #[test]
    fn a_chunk_taken_over_leaves_the_older_days_and_the_window_alone() {
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        rt.block_on(a_chunk_taken_over());
    }

    async fn a_chunk_taken_over() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        db::create_db_at(&main).unwrap();
        let db = shared_db(db::open_at(&main).unwrap());
        let (r, g) = (20_000, "g");
        let floor = r - SPECULATIVE_WINDOW;
        let start = crate::chunks::Speculative { runner_up_day: r, reach: SPECULATIVE_WINDOW, days: 0, articles: 0 };
        let claim = on_db(&db, move |c| {
            crate::chunks::add(c, g, floor, floor - 5)?;
            crate::chunks::set_speculative(c, g, &start)?;
            Ok(crate::chunks::claim(c, &[(g.to_string(), i64::MIN)], "a", 0)?.unwrap())
        })
        .await
        .unwrap();
        assert_eq!(claim.day, floor);
        let days = || {
            on_db(&db, move |c| Ok(c.query_row("select count(*) from backfill_chunks", [], |r| r.get::<_, i64>(0))?))
        };
        let window = || on_db(&db, move |c| Ok(crate::chunks::speculative(c, g)?.unwrap()));

        // its claim went stale and another worker has it now
        on_db(&db, move |c| Ok(c.execute("update backfill_chunks set claimed_at = 99 where day = ?", [floor])?))
            .await
            .unwrap();
        // no dated article in the day: the older days stay
        finish_chunk(&db, &claim, (true, None, Some(0)), 0).await.unwrap();
        assert_eq!(days().await.unwrap(), 6);
        // a dense day: the window doesnt count it, nor reach further
        finish_chunk(&db, &claim, (true, Some(floor), Some(0)), 100_000).await.unwrap();
        let s = window().await.unwrap();
        assert_eq!((s.reach, s.days, s.articles), (SPECULATIVE_WINDOW, 0, 0));
        assert_eq!(days().await.unwrap(), 6);

        // the claim that has it does both
        let mut now = claim.clone();
        now.claimed_at = 99;
        finish_chunk(&db, &now, (true, None, Some(0)), 0).await.unwrap();
        assert_eq!(days().await.unwrap(), 1, "the older days went to the sweeps");
    }

    /// a group taken out of config.toml keeps its run state, but stops counting among the tracked groups
    #[test]
    fn the_tracked_groups_are_the_ones_config_tracks_now() {
        let states = RunStates::default();
        states.with("gone", |st| st.idle = true);
        assert_eq!(states.groups(), HashSet::from(["gone".to_string()]), "until noted: the groups passes ran for");
        states.set_tracked(&["kept".to_string()]);
        assert_eq!(states.groups(), HashSet::from(["kept".to_string()]));
        assert!(states.is_idle("gone"));
    }

    #[test]
    fn a_split_group_behind_on_its_live_side_goes_live_first() {
        // behind by more than a slice: live, whatever the turn
        assert!(live_first("dynamic", Phase::Backfill, true, 100_001, 100_000));
        // a slice or less behind (a busy group always is a little): its turn
        assert!(!live_first("dynamic", Phase::Backfill, true, 100_000, 100_000));
        // not split: its backfill has no other takers, turns as before
        assert!(!live_first("dynamic", Phase::Backfill, false, 10_000_000, 100_000));
        // the modes that do one side only, and a live turn anyway
        assert!(!live_first("backfill", Phase::Backfill, true, 10_000_000, 100_000));
        assert!(!live_first("live", Phase::Backfill, true, 10_000_000, 100_000));
        assert!(!live_first("dynamic", Phase::Live, true, 10_000_000, 100_000));
    }

    #[test]
    fn cursors_saved_under_a_servers_old_key_are_adopted_once() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        db::create_db_at(&main).unwrap();
        let conn = db::open_at(&main).unwrap();
        let state = db::GroupState { live_cursor: 900, backfill_cursor: 500 };
        db::save_group_state(&conn, "g@x@a.example:563", state).unwrap();
        let legacy = ["a.example:563".to_string(), "x@a.example:563".to_string()];
        assert_eq!(cursors_or_adopted(&conn, "g@a.example", "g", false, &legacy).unwrap(), Some(state));
        assert_eq!(db::get_group_state(&conn, "g@x@a.example:563").unwrap(), None, "moved, not copied");
        assert_eq!(db::get_group_state(&conn, "g@a.example").unwrap(), Some(state));
        // nothing to adopt: nothing made up
        assert_eq!(cursors_or_adopted(&conn, "g@b.example", "g", false, &[]).unwrap(), None);
        // the plain group name goes to the server that kept it
        db::save_group_state(&conn, "h", state).unwrap();
        assert_eq!(cursors_or_adopted(&conn, "h@a.example", "h", true, &legacy).unwrap(), Some(state));
    }

    /// a lone server's plain cursors are its own: adding a server that sorts
    /// before it doesnt hand them to the new one
    #[test]
    fn plain_cursors_stay_with_the_server_that_kept_them() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        db::create_db_at(&main).unwrap();
        let conn = db::open_at(&main).unwrap();
        let state = db::GroupState { live_cursor: 900, backfill_cursor: 500 };
        db::save_group_state(&conn, "g", state).unwrap();

        let old = crate::config::UsenetServer::new("old.example", "u", "p", 563);
        let lone = Pool::new(std::slice::from_ref(&old));
        claim_plain_cursors(&conn, &lone).unwrap();
        assert_eq!(cursor_key(&lone, 0, "g"), "g", "still the plain name on its own");

        // a new server, ahead of it by priority
        let mut new = crate::config::UsenetServer::new("new.example", "u", "p", 563);
        new.priority = 0;
        let both = Pool::new(&[new, old]);
        claim_plain_cursors(&conn, &both).unwrap();
        assert_eq!(both.host(0), "new.example", "the new one sorts first");
        let adopted = |i: usize| {
            let key = cursor_key(&both, i, "g");
            cursors_or_adopted(&conn, &key, "g", owns_plain_cursors(&conn, &both.host(i)).unwrap(), &[]).unwrap()
        };
        assert_eq!(adopted(0), None, "the new server starts fresh");
        assert_eq!(adopted(1), Some(state), "the old one keeps its place");

        // plain cursors from before anyone was recorded, with two servers: no one's
        db::create_db_at(&dir.path().join("other.db")).unwrap();
        let conn2 = db::open_at(&dir.path().join("other.db")).unwrap();
        db::save_group_state(&conn2, "g", state).unwrap();
        claim_plain_cursors(&conn2, &both).unwrap();
        for i in 0..2 {
            assert!(!owns_plain_cursors(&conn2, &both.host(i)).unwrap(), "ambiguous, left alone");
        }
    }

    #[test]
    fn what_a_server_keeps_is_remembered_by_its_host_not_its_place_in_the_pool() {
        let states = RunStates::default();
        // index 0 was a.example when this was learned; a config reload put b.example there
        states.keep_from("alt.binaries.g", "a.example", 20_000);
        assert_eq!(states.keeps_from("alt.binaries.g", "a.example"), 20_000);
        assert_eq!(states.kept_days("a.example"), HashMap::from([("alt.binaries.g".to_string(), 20_000)]));
        assert_eq!(states.keeps_from("alt.binaries.g", "b.example"), i64::MIN, "b is not restricted");
        assert!(states.kept_days("b.example").is_empty());
    }

    #[test]
    fn a_server_not_carrying_a_group_stays_noted_but_other_first_posts_expire() {
        let states = RunStates::default();
        states.not_carrying("g", "a.example");
        states.first_post_on("g", "b.example", 20_000);
        assert_eq!(states.first_post_day("g", "b.example"), 20_000);
        let past = Instant::now() - Duration::from_secs(1);
        states.with("g", |st| {
            for (_, until) in st.first_post_days.values_mut() {
                if until.is_some() {
                    *until = Some(past);
                }
            }
        });
        assert_eq!(states.first_post_day("g", "a.example"), i64::MAX, "no expiry for a non-carrier");
        assert_eq!(states.first_post_day("g", "b.example"), i64::MIN, "an ordinary first post expires");
    }

    #[test]
    fn slices_cover_the_range() {
        assert_eq!(make_slices(1, 10, 4, false), vec![(1, 4), (5, 8), (9, 10)]);
        assert_eq!(make_slices(1, 10, 4, true), vec![(9, 10), (5, 8), (1, 4)]);
        assert_eq!(make_slices(5, 5, 100, false), vec![(5, 5)]);
        assert!(make_slices(6, 5, 100, false).is_empty());
        assert_eq!(make_slices(u64::MAX - 1, u64::MAX, 10, false), vec![(u64::MAX - 1, u64::MAX)]);
    }

    #[test]
    fn a_split_is_made_only_from_dates_that_make_sense() {
        let (today, day) = (20_400, 86_400);
        let home = 19_500 * day;
        let at = |times: &[i64]| Newest::AtCursor(times.to_vec());
        assert_eq!(split_bounds(&at(&[20_000 * day]), home, 19_000 * day, today), Some((20_000, 19_000)), "sane");
        assert_eq!(split_bounds(&at(&[0]), home, 19_000 * day, today), None, "a date from 1970 at the cursor");
        assert_eq!(split_bounds(&at(&[30_000 * day]), home, 19_000 * day, today), None, "one in the future");
        assert_eq!(split_bounds(&at(&[19_000 * day]), home, 19_000 * day, today), None, "before home's first");
        assert_eq!(split_bounds(&at(&[home - 3600]), home, home, today), None, "newest day before the oldest");
        assert_eq!(split_bounds(&at(&[home - 3600]), home, home - 7200, today), Some((19_499, 19_499)), "same day");
        assert_eq!(
            split_bounds(&at(&[19_499 * day]), home - day, 19_500 * day, today),
            None,
            "a day before the oldest"
        );
        assert_eq!(split_bounds(&Newest::Day(19_000), home, 19_100 * day, today), None, "a split's newest day too");
        assert_eq!(
            split_bounds(&at(&[0, 20_000 * day]), home, 19_000 * day, today),
            Some((20_000, 19_000)),
            "one forged date of two is left out"
        );
        assert_eq!(split_bounds(&at(&[20_000 * day]), home, 5 * day, today), None, "a carrier's first from 1970");
        assert_eq!(split_bounds(&Newest::Day(20_000), home, 19_000 * day, today), Some((20_000, 19_000)));
        assert_eq!(split_bounds(&Newest::Day(20_000), home, 5 * day, today), None);
        assert_eq!(split_bounds(&at(&[20_000 * day]), home, 19_000 * day, 0), None, "a clock from 1970");
    }

    #[test]
    fn a_slice_nobody_waits_for_isnt_saved() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        db::create_db_at(&main).unwrap();
        let shared = shared_db(db::open_at(&main).unwrap());
        let release = |name: &str| Release {
            name: name.into(),
            group: "alt.binaries.t".into(),
            articles: vec![crate::parser::Article { message_id: format!("<{name}@x>"), ..Default::default() }],
            ..Default::default()
        };

        // a pass that was dropped while stopping: nobody waits for its slice
        let (done, gone) = tokio::sync::oneshot::channel();
        drop(gone);
        crate::profile::LOAD.writer_queued.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let shard = crate::store::shard_of("alt.binaries.t");
        shared.saves[shard].send(WriterJob::Save(SaveJob { releases: vec![release("dropped")], done })).unwrap();
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        rt.block_on(shared.save(vec![release("kept")])).unwrap();
        drop(shared);

        let conn = db::open_with_shards(&main).unwrap();
        let names: Vec<String> = conn
            .prepare("select name from releases")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(names, vec!["kept".to_string()]);
    }

    #[test]
    fn an_idle_writer_still_seals_what_is_due() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        db::create_db_at(&main).unwrap();
        // one part of two, untouched for long: due though never completed
        let part = crate::parser::Article {
            message_id: "<a1@x>".into(),
            subject: "\"a.rar\" yEnc (1/2)".into(),
            filename: Some("a.rar".into()),
            part: Some(1),
            total_parts: Some(2),
            bytes: 10,
            ..Default::default()
        };
        let release =
            Release { name: "Rel".into(), group: "alt.binaries.t".into(), articles: vec![part], ..Default::default() };
        crate::store::save(&main, &[release]).unwrap();
        let shard = db::open_at(&crate::store::shard_path(&main, crate::store::shard_of("alt.binaries.t"))).unwrap();
        shard.execute("update files set touched_at = 0", []).unwrap();
        let sealed = || -> i64 {
            shard.query_row("select count(*) from files where blob is not null", [], |r| r.get(0)).unwrap()
        };
        assert_eq!(sealed(), 0);

        // no save ever comes
        let shared = shared_db_idling(db::open_at(&main).unwrap(), Duration::from_millis(50));
        let deadline = Instant::now() + Duration::from_secs(10);
        while sealed() == 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(sealed(), 1, "the idle writer sealed the stale file");
        drop(shared);
    }

    #[test]
    fn a_release_looks_at_a_few_bodies_for_its_name() {
        let article = |i: usize, name: &str| crate::parser::Article {
            subject: format!(r#""{name}" yEnc (1/1)"#),
            message_id: format!("<{i}@x>"),
            ..Default::default()
        };
        let mut articles: Vec<_> = (0..40).map(|i| article(i, &format!("n{i}.nfo"))).collect();
        articles.push(article(99, "base.par2"));
        let sources = name_sources(&Release { articles, ..Default::default() });
        assert_eq!(sources.len(), MAX_NAME_BODIES);
        assert_eq!(sources[0].0, "<99@x>", "the par2 first");
    }

    fn stage_db() -> (tempfile::TempDir, Db, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        db::create_db_at(&main).unwrap();
        let conn = db::open_at(&main).unwrap();
        let shard = crate::store::shard_path(&main, 0);
        (dir, shared_db(conn), shard)
    }

    #[test]
    fn a_job_on_the_firsts_lane_runs_ahead_of_queued_stage_work() {
        let (_dir, db, _shard) = stage_db();
        let order = Arc::new(std::sync::Mutex::new(Vec::new()));
        let job = |name: &'static str, slow: bool| -> StageWork {
            let order = order.clone();
            Box::new(move |_| {
                if slow {
                    std::thread::sleep(Duration::from_millis(20));
                }
                order.lock().unwrap().push(name);
                Ok(())
            })
        };
        // 30 jobs of 20 ms, three times the writer's idle stage budget, then one on the firsts lane
        let mut answers: Vec<_> =
            (0..30).map(|_| db.on_writer(0, "db.stage.test", job("queued", true)).unwrap()).collect();
        answers.push(db.on_writer_first(0, "db.stage.test", job("first", false)).unwrap());
        for answer in answers {
            answer.recv_timeout(Duration::from_secs(10)).unwrap().unwrap();
        }
        let order = order.lock().unwrap();
        let at = order.iter().position(|n| *n == "first").unwrap();
        assert!(at < 20, "ran {at}th: {order:?}");
    }

    #[test]
    fn jobs_on_the_firsts_lane_run_in_order_and_before_the_writer_stops() {
        let (_dir, db, _shard) = stage_db();
        let order = Arc::new(std::sync::Mutex::new(Vec::new()));
        let answers: Vec<_> = (0..20)
            .map(|n| {
                let order = order.clone();
                db.on_writer_first(
                    0,
                    "db.stage.test",
                    Box::new(move |_| {
                        order.lock().unwrap().push(n);
                        Ok(())
                    }),
                )
                .unwrap()
            })
            .collect();
        drop(db);
        for answer in answers {
            answer.recv().unwrap().unwrap();
        }
        assert_eq!(*order.lock().unwrap(), (0..20).collect::<Vec<_>>());
    }

    #[test]
    fn a_stage_job_runs_on_the_writer_and_answers() {
        let (_dir, db, shard) = stage_db();
        let answer = db
            .on_writer(
                0,
                "db.stage.test",
                Box::new(|conn| {
                    conn.execute("insert into pp_queue (release_id, due_at) values (1, 2)", [])?;
                    Ok(())
                }),
            )
            .unwrap();
        answer.recv().unwrap().unwrap();
        let rows: i64 =
            db::open_shard(&shard).unwrap().query_row("select count(*) from pp_queue", [], |r| r.get(0)).unwrap();
        assert_eq!(rows, 1);
    }

    #[test]
    fn stage_jobs_dont_hold_up_saves() {
        let (_dir, db, _shard) = stage_db();
        let group = (0..).map(|i| format!("alt.binaries.g{i}")).find(|g| crate::store::shard_of(g) == 0).unwrap();
        let ran = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let answers: Vec<_> = (0..500)
            .map(|_| {
                let ran = ran.clone();
                db.on_writer(
                    0,
                    "db.stage.test",
                    Box::new(move |_| {
                        std::thread::sleep(Duration::from_millis(2));
                        ran.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        Ok(())
                    }),
                )
                .unwrap()
            })
            .collect();
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let article = crate::parser::Article {
            message_id: "<a@x>".into(),
            filename: Some("a".into()),
            part: Some(1),
            total_parts: Some(1),
            ..Default::default()
        };
        runtime
            .block_on(db.save(vec![Release { name: "a".into(), group, articles: vec![article], ..Default::default() }]))
            .unwrap();
        assert!(ran.load(std::sync::atomic::Ordering::SeqCst) < 250);
        for answer in answers {
            answer.recv().unwrap().unwrap();
        }
    }

    /// right after a save, stage work waits its share or a quiet moment, and then runs
    #[test]
    fn stage_jobs_run_once_saves_go_quiet() {
        let (_dir, db, _shard) = stage_db();
        let group = (0..).map(|i| format!("alt.binaries.g{i}")).find(|g| crate::store::shard_of(g) == 0).unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let article = crate::parser::Article {
            message_id: "<a@x>".into(),
            filename: Some("a".into()),
            part: Some(1),
            total_parts: Some(1),
            ..Default::default()
        };
        runtime
            .block_on(db.save(vec![Release { name: "a".into(), group, articles: vec![article], ..Default::default() }]))
            .unwrap();
        let start = std::time::Instant::now();
        let answers: Vec<_> = (0..20)
            .map(|_| {
                db.on_writer(
                    0,
                    "db.stage.test",
                    Box::new(|_| {
                        std::thread::sleep(Duration::from_millis(50));
                        Ok(())
                    }),
                )
                .unwrap()
            })
            .collect();
        for answer in answers {
            answer.recv_timeout(Duration::from_secs(10)).unwrap().unwrap();
        }
        assert!(start.elapsed() < STAGE_QUIET + Duration::from_secs(3), "{:?}", start.elapsed());
    }

    #[test]
    fn queued_stage_jobs_run_before_the_writer_stops() {
        let (_dir, db, _shard) = stage_db();
        let answers: Vec<_> =
            (0..50).map(|_| db.on_writer(0, "db.stage.test", Box::new(|_| Ok(()))).unwrap()).collect();
        drop(db);
        for answer in answers {
            answer.recv().unwrap().unwrap();
        }
    }
}
