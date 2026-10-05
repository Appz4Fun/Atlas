use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use rusqlite::Connection;

use crate::config::{DEFAULT_BATCH_SIZE, DEFAULT_REQUEST_SIZE};
use crate::db;
use crate::nfo;
use crate::nntp::{BlockingPool, Extract, Overview, Pool, first_names, headers_to_articles};
use crate::par2;
use crate::parser::{Release, group_articles, is_complete};

/// What one finished XOVER slice added, reported as it lands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Progress {
    pub articles: i64,
    pub bytes: i64,
    pub releases: i64,
}

impl Progress {
    fn add(&mut self, other: &Progress) {
        self.articles += other.articles;
        self.bytes += other.bytes;
        self.releases += other.releases;
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
    /// per server, the oldest day it keeps of the group (as found by a day
    /// chunk too old for it), until it's looked at again
    pub keeps_from: HashMap<usize, (i64, Instant)>,
}

impl Default for GroupRunState {
    fn default() -> Self {
        GroupRunState {
            phase: Phase::Backfill,
            idle: false,
            backfilling: false,
            no_split_until: None,
            keeps_from: HashMap::new(),
        }
    }
}

/// Run state of every group, shared by passes running at the same time.
#[derive(Clone, Default)]
pub struct RunStates(Arc<Mutex<HashMap<String, GroupRunState>>>);

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

    /// The oldest day `server` keeps of `group`, `i64::MIN` when not known.
    pub fn keeps_from(&self, group: &str, server: usize) -> i64 {
        let now = Instant::now();
        let states = self.0.lock().unwrap();
        let found = states.get(group).and_then(|s| s.keeps_from.get(&server));
        found.filter(|(_, until)| *until > now).map_or(i64::MIN, |(day, _)| *day)
    }

    /// The groups whose oldest day on `server` is known, with that day.
    pub fn kept_days(&self, server: usize) -> HashMap<String, i64> {
        let now = Instant::now();
        let states = self.0.lock().unwrap();
        states
            .iter()
            .filter_map(|(g, s)| {
                s.keeps_from.get(&server).filter(|(_, until)| *until > now).map(|(d, _)| (g.clone(), *d))
            })
            .collect()
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
}

impl Default for PassSettings {
    fn default() -> Self {
        PassSettings {
            mode: "dynamic".into(),
            batch_size: DEFAULT_BATCH_SIZE as i64,
            request_size: DEFAULT_REQUEST_SIZE,
            split_min_backlog: crate::config::SPLIT_MIN_BACKLOG,
        }
    }
}

/// slices saved together in one transaction at most. bigger batches measured
/// no faster on a 98GB database and risk spilling the page cache mid transaction
const MAX_BATCH: usize = 8;

/// The indexer's database, shared by every pass. Small writes (cursors) run on
/// the main database's connection directly. Slices go to the writer thread of
/// their group's shard (see store.rs); each saves whatever queued up for it
/// while it was busy in one transaction, and the shards save in parallel.
#[derive(Clone)]
pub struct Db {
    conn: Arc<Mutex<Connection>>,
    saves: Vec<std::sync::mpsc::Sender<SaveJob>>,
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

/// `conn` is the main database. The writer threads end once every clone of
/// the `Db` is gone, and the last clone to go waits for them.
pub fn shared_db(conn: Connection) -> Db {
    let main = conn.path().map(std::path::PathBuf::from).unwrap_or_else(crate::paths::database);
    let ids = Arc::new(crate::store::Ids::new(&main));
    let (saves, writers): (Vec<_>, Vec<_>) = (0..crate::store::SHARDS)
        .map(|shard| {
            let (saves, jobs) = std::sync::mpsc::channel();
            let (path, ids) = (crate::store::shard_path(&main, shard), ids.clone());
            let handle = std::thread::Builder::new()
                .name(format!("atlas-db-writer-{shard}"))
                .spawn(move || writer(shard, &path, &ids, jobs))
                .expect("couldnt start a db writer");
            (saves, handle)
        })
        .unzip();
    Db { conn: Arc::new(Mutex::new(conn)), saves, _writers: Arc::new(Writers(writers)) }
}

fn writer(shard: usize, path: &std::path::Path, ids: &crate::store::Ids, jobs: std::sync::mpsc::Receiver<SaveJob>) {
    use crate::profile::{LOAD, Load};
    use std::sync::atomic::Ordering::Relaxed;

    let mut opened = db::open_at(path).and_then(|conn| db::tune_for_writing(&conn).map(|_| conn));
    let mut store = crate::store::ShardWriter::new(shard);
    // a job taken off the queue to see if more were waiting
    let mut next: Option<SaveJob> = None;

    loop {
        let first = match next.take() {
            Some(job) => job,
            None => match jobs.recv() {
                Ok(job) => job,
                Err(_) => break,
            },
        };
        let mut batch = vec![first];
        while batch.len() < MAX_BATCH {
            match jobs.try_recv() {
                Ok(job) => batch.push(job),
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
        let result = match &mut opened {
            Ok(conn) => {
                store.save(conn, ids, batch.iter().map(|job| job.releases.as_slice())).map_err(|e| e.to_string())
            }
            Err(e) => Err(format!("couldnt open {}: {e}", path.display())),
        };
        Load::add_since(&LOAD.writer_busy_ns, t);
        LOAD.writer_batches.fetch_add(1, Relaxed);
        LOAD.writer_slices.fetch_add(batch.len() as u64, Relaxed);

        // the whole batch rolled back on an error, every slice in it failed.
        // the save is committed by now: the slices dont wait for the housekeeping
        let saved = result.is_ok();
        for job in batch {
            let _ = job.done.send(result.clone());
        }

        // housekeeping between transactions
        if let Ok(conn) = &mut opened {
            let t = std::time::Instant::now();
            // more slices waiting: saving them comes first. a closed queue
            // means the indexer is stopping, it waits for this thread
            let seal = match jobs.try_recv() {
                Ok(job) => {
                    next = Some(job);
                    false
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => saved,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => false,
            };
            if seal {
                let sealing = std::time::Instant::now();
                if store.seal_some(conn, chrono::Utc::now().timestamp()).is_err() {
                    LOAD.writer_seal_errors.fetch_add(1, Relaxed);
                }
                Load::add_since(&LOAD.writer_seal_ns, sealing);
            }
            let checkpointing = std::time::Instant::now();
            let _ = db::finish_checkpoint(conn);
            Load::add_since(&LOAD.writer_checkpoint_ns, checkpointing);
            Load::add_since(&LOAD.writer_busy_ns, t);
        }
    }
}

impl Db {
    /// Save one slice's releases. Returns once they are committed. Dropping
    /// the future before the writer gets to the slice drops the slice too
    /// (its pass didnt finish, soo the cursor didnt move past it); once the
    /// writer has it, it gets saved.
    async fn save(&self, releases: Vec<Release>) -> Result<()> {
        // a slice is one group, soo one shard
        let Some(shard) = releases.first().map(|r| crate::store::shard_of(&r.group)) else { return Ok(()) };
        let (done, saved) = tokio::sync::oneshot::channel();
        crate::profile::LOAD.writer_queued.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if self.saves[shard].send(SaveJob { releases, done }).is_err() {
            crate::profile::LOAD.writer_queued.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
            return Err(anyhow!("db writer stopped"));
        }
        saved.await.map_err(|_| anyhow!("db writer stopped"))?.map_err(|e| anyhow!("saving releases: {e}"))
    }
}

async fn on_db<T: Send + 'static>(db: &Db, f: impl FnOnce(&mut Connection) -> Result<T> + Send + 'static) -> Result<T> {
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
/// cursors are kept per server as `group@host`. One server keeps the plain
/// group name like before.
fn cursor_key(pool: &Pool, server: usize, group: &str) -> String {
    if pool.len() <= 1 { group.to_string() } else { format!("{group}@{}", pool.host(server).to_lowercase()) }
}

async fn load_cursors(db: &Db, key: &str, group: &str, legacy: bool) -> Result<Option<db::GroupState>> {
    let (key, group) = (key.to_string(), group.to_string());

    on_db(db, move |conn| {
        if let Some(state) = db::get_group_state(conn, &key)? {
            return Ok(Some(state));
        }

        // cursors saved before a second server was added belong to the first server
        if legacy
            && key != group
            && let Some(state) = db::get_group_state(conn, &group)?
        {
            db::save_group_state(conn, &key, state)?;
            return Ok(Some(state));
        }

        Ok(None)
    })
    .await
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
    let (server, (_count, first, last, _name)) = ctx.pool.select_group_on(prefer, group).await?;
    let (first, last) = (first as i64, last as i64);
    let key = cursor_key(&ctx.pool, server, group);

    let state = match load_cursors(db, &key, group, server == 0).await? {
        Some(s) if last >= s.live_cursor => s,
        // first time both cursors start at the top. if the server renumbered
        // (last went backwards) the group got reset soo start over the same way
        _ => {
            let k = key.clone();
            on_db(db, move |conn| Ok(db::init_group_state(conn, &k, last)?)).await?;
            ctx.states.with(group, |st| st.phase = Phase::Backfill);
            db::GroupState { live_cursor: last, backfill_cursor: last }
        }
    };

    // for the stats dashboard's progress and ETA
    let k = key.clone();
    on_db(db, move |conn| Ok(db::save_group_bounds(conn, &k, first, last)?)).await?;

    let phase = ctx.states.with(group, |st| st.phase);
    let backfilling = settings.mode == "backfill" || (settings.mode != "live" && phase == Phase::Backfill);
    if backfilling
        && maybe_split(ctx, settings, db, group, server, first as u64, state.backfill_cursor.max(0) as u64).await?
    {
        // a split group's backfill is day chunks, this server takes the next one
        let host = ctx.pool.host(server);
        let wanted = [(group.to_string(), ctx.states.keeps_from(group, server))];
        let claimed =
            on_db(db, move |conn| Ok(crate::chunks::claim(conn, &wanted, &host, chrono::Utc::now().timestamp())?))
                .await?;
        let r = match claimed {
            // a day this server doesnt keep is left to the others, it isnt an error
            Some(chunk) => match run_chunk(ctx, settings, db, &chunk, server, progress).await {
                Err(e) if e.downcast_ref::<TooOld>().is_some() => Ok(Progress::default()),
                r => r,
            },
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
    first: u64,
    cursor: u64,
) -> Result<bool> {
    let g = group.to_string();
    if on_db(db, move |conn| Ok(crate::chunks::is_split(conn, &g)?)).await? {
        return Ok(true);
    }
    if (cursor.saturating_sub(first) as i64) < settings.split_min_backlog {
        return Ok(false);
    }
    if ctx.states.with(group, |st| st.no_split_until.is_some_and(|t| Instant::now() < t)) {
        return Ok(false);
    }

    let Some((newest_day, oldest_day, carriers)) = split_days(ctx, group, home, first, cursor).await? else {
        ctx.states.with(group, |st| st.no_split_until = Some(Instant::now() + SPLIT_RECHECK));
        return Ok(false);
    };

    let g = group.to_string();
    let added = on_db(db, move |conn| Ok(crate::chunks::add(conn, &g, newest_day, oldest_day)?)).await?;
    println!("[SPLIT] {group}: backfill split into {added} day chunks over {carriers} servers");
    Ok(true)
}

/// The days a split of `group` covers, (newest, oldest), and how many servers
/// carry it. None when it cant split: home's dates at either end are unknown
/// or no other indexing server carries the group.
async fn split_days(
    ctx: &PassContext,
    group: &str,
    home: usize,
    first: u64,
    cursor: u64,
) -> Result<Option<(i64, i64, usize)>> {
    use crate::chunks::unix_day;

    // the newest day still to do is the post date at the home cursor, the
    // oldest at least home's first article
    let Some(newest) = ctx.pool.posted_date(home, group, cursor).await? else { return Ok(None) };
    let Some(oldest) = ctx.pool.posted_date(home, group, first).await? else { return Ok(None) };
    let newest_day = unix_day(newest);
    let mut oldest_day = unix_day(oldest).min(newest_day);

    // other servers are best effort: one that fails is left out. one that
    // carries the group counts, its oldest date only if it has one
    let mut carriers = 1;
    for other in ctx.pool.indexing_servers().into_iter().filter(|&s| s != home) {
        let Ok((_, low, _, _)) = ctx.pool.group_on(other, group).await else { continue };
        carriers += 1;
        if let Ok(Some(t)) = ctx.pool.posted_date(other, group, low).await {
            oldest_day = oldest_day.min(unix_day(t));
        }
    }

    let (newest_day, oldest_day) = clamp_days(newest_day, oldest_day, unix_day(chrono::Utc::now().timestamp()));
    Ok((carriers >= 2).then_some((newest_day, oldest_day, carriers)))
}

/// 2000-01-01: binary retention doesnt reach further back than this
const SPLIT_OLDEST_DAY: i64 = 10_957;

/// A split's (newest, oldest) days kept between 2000-01-01 and `today`, soo a
/// forged Date header cant make thousands of empty chunks.
fn clamp_days(newest_day: i64, oldest_day: i64, today: i64) -> (i64, i64) {
    // a clock set before 2000 mustnt panic the clamp
    let newest = newest_day.clamp(SPLIT_OLDEST_DAY, today.max(SPLIT_OLDEST_DAY));
    (newest, oldest_day.clamp(SPLIT_OLDEST_DAY, newest))
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

/// a day chunk also takes this much on each side: post dates are only
/// roughly in article number order, duplicates are dropped when saved
pub const CHUNK_OVERLAP: i64 = 3600;

/// Index the day chunk `chunk` claimed (its day in unix days) on `server`,
/// for a group whose backfill is split into day chunks (see chunks.rs). The
/// chunk is marked done when the whole day is in, released again if stopped
/// part way or failing. Either is skipped once another worker took the chunk
/// over (this one ran past CLAIM_TIMEOUT): the chunk is that worker's.
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
    let (group, day) = (chunk.group.as_str(), chunk.day);
    let release = || {
        let c = chunk.clone();
        on_db(db, move |conn| Ok(crate::chunks::release(conn, &c)?))
    };
    let finish = async || {
        let c = chunk.clone();
        if !on_db(db, move |conn| Ok(crate::chunks::finish(conn, &c)?)).await? {
            println!("[CHUNK] {group} day {day}: taken over by another worker, leaving it to that one");
        }
        anyhow::Ok(())
    };
    // give the chunk back after `e`, and return `e`: failing to give it back
    // is only logged (the claim goes stale and gets taken over)
    let failed = async |e: anyhow::Error| {
        if let Err(r) = release().await {
            println!("[CHUNK] {group} day {day}: couldnt give the chunk back: {r:#}");
        }
        Err(e)
    };

    // GROUP on this server alone: falling over would move where the group
    // lives, and only a 411 means the server doesnt carry it
    let found = match ctx.pool.group_on(server, group).await {
        Ok(info) => info,
        Err(e) if e.code() == Some(411) => {
            return failed(NotCarried { group: group.to_string(), host: ctx.pool.host(server) }.into()).await;
        }
        Err(e) => return failed(e.into()).await,
    };
    let (_count, first, last, _name) = found;

    let from = day * 86_400 - CHUNK_OVERLAP;
    let to = (day + 1) * 86_400 + CHUNK_OVERLAP;

    // a day this server doesnt keep from its start would come out empty or
    // cut short, though another server may have all of it: give it back, and
    // the server takes no day that old for a while. the split's oldest day is
    // the exception: no server goes back further, the one whose oldest
    // article is on it does what there is
    let oldest = match ctx.pool.first_post(server, group, first, last).await {
        Ok(oldest) => oldest,
        Err(e) => return failed(e.into()).await,
    };
    let keeps_day = match oldest {
        Some(t) if t <= day * 86_400 => true,
        Some(t) if crate::chunks::unix_day(t) == day => {
            let g = group.to_string();
            match on_db(db, move |conn| Ok(crate::chunks::oldest_day(conn, &g)?)).await {
                Ok(split_oldest) => split_oldest == Some(day),
                Err(e) => return failed(e).await,
            }
        }
        _ => false,
    };
    if !keeps_day {
        // the first day it keeps whole
        let oldest_day = oldest.map_or(i64::MAX, |t| crate::chunks::unix_day(t - 1) + 1);
        ctx.states.with(group, |st| st.keeps_from.insert(server, (oldest_day, Instant::now() + KEEPS_RECHECK)));
        let host = ctx.pool.host(server);
        println!("[CHUNK] {group} day {day} is older than {host} keeps, leaving it to the other servers");
        return failed(TooOld { group: group.to_string(), host, oldest_day }.into()).await;
    }
    let start = match ctx.pool.article_at(server, group, first, last, from).await {
        Ok(n) => n,
        Err(e) => return failed(e.into()).await,
    };
    let end = match ctx.pool.article_at(server, group, start.max(first), last, to).await {
        Ok(n) => n.saturating_sub(1),
        Err(e) => return failed(e.into()).await,
    };
    if start > end {
        finish().await?;
        return Ok(Progress::default());
    }

    let pass = Pass { ctx, settings, db, group, server, key: cursor_key(&ctx.pool, server, group) };
    match pass.process_range(start as i64, end as i64, "CHUNK", progress).await {
        Ok((saved, true)) => {
            finish().await?;
            Ok(saved)
        }
        Ok((saved, false)) => {
            release().await?;
            Ok(saved)
        }
        Err(e) => failed(e).await,
    }
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
        let (saved, complete) = self.process_range(start, end, "LIVE", progress).await?;

        if complete {
            let key = self.key.clone();
            on_db(self.db, move |conn| Ok(db::update_live_cursor(conn, &key, end)?)).await?;
        }
        Ok(saved)
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
        let (saved, complete) = self.process_range(start, end, "BACKFILL", progress).await?;

        if complete {
            let key = self.key.clone();
            on_db(self.db, move |conn| Ok(db::update_backfill_cursor(conn, &key, start - 1)?)).await?;
        }
        self.ctx.states.with(group, |st| st.backfilling = true);
        Ok(saved)
    }

    /// Index `start..=end`. The range is cut into `request_size` slices that
    /// stream in over the server's connections; each slice is parsed, named and
    /// saved as soon as it lands while the rest keep downloading. Backfill takes
    /// the newest slices first.
    ///
    /// The bool is true when the whole range is done and its cursor can move,
    /// false when `stop` cut it short (what was saved stays, the range gets redone).
    async fn process_range<P>(&self, start: i64, end: i64, kind: &str, progress: &mut P) -> Result<(Progress, bool)>
    where
        P: FnMut(&Progress) + ?Sized,
    {
        let (pool, group) = (&self.ctx.pool, self.group);
        let slices =
            make_slices(start.max(0) as u64, end.max(0) as u64, self.settings.request_size, kind == "BACKFILL");
        let total = slices.len();

        let mut rx = pool.stream_headers(group, self.server, slices, self.ctx.stop.clone());
        let mut saved = Progress::default();
        let mut done = 0;
        let mut error: Option<anyhow::Error> = None;
        // dated ends of what this pass saved, for the stats dashboard's history numbers
        let (mut low, mut high): (Option<db::Dated>, Option<db::Dated>) = (None, None);

        while let Some(slice) = rx.recv().await {
            let headers: Vec<Overview> = match slice.result {
                Ok(h) => h,
                // 423 = no articles in that slice
                Err(e) if e.code() == Some(423) => {
                    done += 1;
                    continue;
                }
                Err(e) if e.is_permanent() => {
                    let code = e.code().unwrap_or(0);
                    println!("[{kind}] {group} {}-{} not available ({code}), skipping", slice.start, slice.end);
                    done += 1;
                    continue;
                }
                Err(e) => {
                    error.get_or_insert_with(|| anyhow::Error::new(e));
                    continue;
                }
            };

            // the pass is failing anyway, dont save half of it
            if error.is_some() {
                continue;
            }

            let dated = slice_date(&headers);
            match save_slice(pool, self.db, group, headers).await {
                Ok(p) => {
                    if let Some(d) = dated {
                        low = low.filter(|l| l.0 <= d.0).or(Some(d));
                        high = high.filter(|h| h.0 >= d.0).or(Some(d));
                    }
                    saved.add(&p);
                    done += 1;
                    progress(&p);
                }
                Err(e) => {
                    error.get_or_insert(e);
                }
            }
        }

        if let (Some(low), Some(high)) = (low, high) {
            let key = self.key.clone();
            on_db(self.db, move |conn| Ok(db::save_group_dates(conn, &key, low, high)?)).await?;
        }

        if let Some(e) = error {
            return Err(e);
        }

        if saved.articles == 0 && done == total {
            println!("[{kind}] {group} {start}-{end} empty, skipping");
        }

        if saved.articles > 0 {
            self.ctx.states.with(group, |st| st.idle = false);
        }

        if self.ctx.verbose {
            println!("[{kind}] {group} {} headers in {done}/{total} slices", saved.articles);
        }

        // stopped early: what got saved stays, the cursor waits for the rest
        Ok((saved, done == total))
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
async fn save_slice(pool: &Arc<Pool>, db: &Db, group: &str, headers: Vec<Overview>) -> Result<Progress> {
    let articles = headers.len() as i64;
    crate::profile::SLICES.add(1);
    crate::profile::HEADERS.add(articles as u64);

    // parsing is cpu work, keep it off the async threads
    let t = std::time::Instant::now();
    let mut releases: Vec<Release> =
        tokio::task::spawn_blocking(move || group_articles(headers_to_articles(headers)).into_values().collect())
            .await
            .map_err(|e| anyhow!("parse task failed: {e}"))?;
    crate::profile::PARSE.add_since(t);
    crate::profile::Load::add_since(&crate::profile::LOAD.parse_ns, t);

    // real names from par2/nfo bodies, every release looked up concurrently
    let t = std::time::Instant::now();
    let jobs = releases.iter().map(name_sources).collect();
    let names = first_names(pool.clone(), jobs).await;
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
    db.save(releases).await?;

    Ok(Progress { articles, bytes, releases: count })
}

/// Bodies worth fetching for a real name: base par2 files first, then nfos.
fn name_sources(release: &Release) -> Vec<(String, Extract)> {
    type Matches = fn(&str) -> bool;
    let sources: [(Matches, Extract); 2] = [(par2::is_base_par2, par2::display_name), (nfo::is_nfo, nfo::display_name)];

    sources
        .iter()
        .flat_map(|(matches, extract)| {
            release.articles.iter().filter(|a| matches(&a.subject)).map(|a| (a.message_id.clone(), *extract))
        })
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

    #[test]
    fn slices_cover_the_range() {
        assert_eq!(make_slices(1, 10, 4, false), vec![(1, 4), (5, 8), (9, 10)]);
        assert_eq!(make_slices(1, 10, 4, true), vec![(9, 10), (5, 8), (1, 4)]);
        assert_eq!(make_slices(5, 5, 100, false), vec![(5, 5)]);
        assert!(make_slices(6, 5, 100, false).is_empty());
        assert_eq!(make_slices(u64::MAX - 1, u64::MAX, 10, false), vec![(u64::MAX - 1, u64::MAX)]);
    }

    #[test]
    fn split_days_stay_between_2000_and_today() {
        let today = 20_400;
        assert_eq!(clamp_days(20_000, 19_000, today), (20_000, 19_000), "sane dates stay");
        assert_eq!(clamp_days(30_000, 19_000, today), (today, 19_000), "a date in the future");
        assert_eq!(clamp_days(20_000, 0, today), (20_000, SPLIT_OLDEST_DAY), "a date from 1970");
        assert_eq!(clamp_days(-5, -10, today), (SPLIT_OLDEST_DAY, SPLIT_OLDEST_DAY));
        assert_eq!(clamp_days(20_000, 19_000, 0), (SPLIT_OLDEST_DAY, SPLIT_OLDEST_DAY), "a clock from 1970");
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
        shared.saves[shard].send(SaveJob { releases: vec![release("dropped")], done }).unwrap();
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
}
