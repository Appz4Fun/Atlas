use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

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
}

impl Default for GroupRunState {
    fn default() -> Self {
        GroupRunState { phase: Phase::Backfill, idle: false, backfilling: false }
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
}

impl Default for PassSettings {
    fn default() -> Self {
        PassSettings {
            mode: "dynamic".into(),
            batch_size: DEFAULT_BATCH_SIZE as i64,
            request_size: DEFAULT_REQUEST_SIZE,
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
}

struct SaveJob {
    releases: Vec<Release>,
    done: tokio::sync::oneshot::Sender<std::result::Result<(), String>>,
}

/// `conn` is the main database. The writer threads end once every clone of
/// the `Db` is gone.
pub fn shared_db(conn: Connection) -> Db {
    let main = conn.path().map(std::path::PathBuf::from).unwrap_or_else(crate::paths::database);
    let ids = Arc::new(crate::store::Ids::new(&main));
    let saves = (0..crate::store::SHARDS)
        .map(|shard| {
            let (saves, jobs) = std::sync::mpsc::channel();
            let (path, ids) = (crate::store::shard_path(&main, shard), ids.clone());
            std::thread::Builder::new()
                .name(format!("atlas-db-writer-{shard}"))
                .spawn(move || writer(shard, &path, &ids, jobs))
                .expect("couldnt start a db writer");
            saves
        })
        .collect();
    Db { conn: Arc::new(Mutex::new(conn)), saves }
}

fn writer(shard: usize, path: &std::path::Path, ids: &crate::store::Ids, jobs: std::sync::mpsc::Receiver<SaveJob>) {
    use crate::profile::{LOAD, Load};
    use std::sync::atomic::Ordering::Relaxed;

    let mut opened = db::open_at(path).and_then(|conn| db::tune_for_writing(&conn).map(|_| conn));
    let mut store = crate::store::ShardWriter::new(shard);

    while let Ok(first) = jobs.recv() {
        let mut batch = vec![first];
        while batch.len() < MAX_BATCH {
            match jobs.try_recv() {
                Ok(job) => batch.push(job),
                Err(_) => break,
            }
        }
        LOAD.writer_queued.fetch_sub(batch.len() as u64, Relaxed);

        let t = std::time::Instant::now();
        let result = match &mut opened {
            Ok(conn) => {
                let saved = store.save(conn, ids, batch.iter().map(|job| job.releases.as_slice()));
                let t = std::time::Instant::now();
                let _ = db::finish_checkpoint(conn);
                Load::add_since(&LOAD.writer_checkpoint_ns, t);
                saved.map_err(|e| e.to_string())
            }
            Err(e) => Err(format!("couldnt open {}: {e}", path.display())),
        };
        Load::add_since(&LOAD.writer_busy_ns, t);
        LOAD.writer_batches.fetch_add(1, Relaxed);
        LOAD.writer_slices.fetch_add(batch.len() as u64, Relaxed);

        // the whole batch rolled back on an error, every slice in it failed
        for job in batch {
            let _ = job.done.send(result.clone());
        }
    }
}

impl Db {
    /// Save one slice's releases. Returns once they are committed. Dropping
    /// the future early leaves the slice queued: it still gets saved.
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

    let pass = Pass { ctx, settings, db, group, server, key };
    let phase = ctx.states.with(group, |st| st.phase);

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
        let settings =
            PassSettings { mode: self.mode.clone(), batch_size: self.batch_size, request_size: self.request_size };
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
}
