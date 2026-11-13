//! The headless indexing loop, run as `atlas --bg-indexer`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::json;

use crate::atomic::write_atomic;
use crate::compact::WriteGuard;
use crate::config::{Config, UsenetServer, load_config};
use crate::db;
use crate::indexer::{Db, NotCarried, PassContext, PassSettings, Progress, RunStates, TooOld, run_pass_on, shared_db};
use crate::nntp::{Pool, RankView, unless_stopped};
use crate::paths;
use crate::pg::indexer_runtime::{Profile as NativeProfile, Runtime as NativeRuntime};
use crate::postprocess::{Stage, StageParams};
use crate::sab;
use crate::ui;

const HISTORY_LEN: f64 = 60.0;
/// a group that failed 3 times in a row on its server gets another go there after this long; its new posts are read
/// from the next server meanwhile (see `Parked`)
const FAILED_RETRY: Duration = Duration::from_secs(300);
/// an idle group (nothing new) is looked at again after this long
const IDLE_RECHECK: Duration = Duration::from_secs(10);
/// how often config.toml is re-read while indexing
const CONFIG_RELOAD: Duration = Duration::from_secs(5);
/// how often the groups big releases come from are read again (`follow_media`)
const MEDIA_EVERY: Duration = Duration::from_secs(600);

/// how often auto_run_compact compacts the database
pub const COMPACT_EVERY: Duration = Duration::from_secs(24 * 3600);
/// a compaction that couldnt take the lock (a `--compact` or a menu write
/// holds it) is tried again after this long
const COMPACT_RETRY: Duration = Duration::from_secs(10 * 60);
static NATIVE_PAUSED: AtomicBool = AtomicBool::new(false);
static STATUS_WRITE: Mutex<()> = Mutex::new(());

/// servers with a host and password, in priority order
fn usable_servers(config: &crate::config::Config) -> Vec<UsenetServer> {
    config.servers.iter().filter(|s| !s.host.is_empty() && !s.password.is_empty()).cloned().collect()
}

fn now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

fn write_status(running: bool, group: &str, mode: &str, idle: bool, status: &str, error: bool, errors: u32) {
    let _write = STATUS_WRITE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let (group, idle, status, error, errors) = if running && NATIVE_PAUSED.load(Ordering::Acquire) {
        ("PostgreSQL unavailable", true, "postgres_paused", false, 0)
    } else {
        (group, idle, status, error, errors)
    };
    let data = json!({
        "running": running,
        "group": group,
        "error": error,
        "idle": idle,
        "mode": mode,
        "status": status,
        "error_count": errors,
        "pid": std::process::id(),
        "postprocess_backlog": crate::postprocess::stats::backlog(),
    });

    if let Err(e) = write_atomic(&paths::status_file(), data.to_string()) {
        // windows can refuse the rename while the menu reads it, write in place
        if let Err(e2) = fs::write(paths::status_file(), data.to_string()) {
            println!("couldnt write status: {e} / {e2}");
        }
    }
}

#[derive(Serialize, Clone)]
struct HistoryPoint {
    t: f64,
    a: i64,
    b: i64,
}

#[derive(Default)]
struct GroupStats {
    articles: i64,
    releases: i64,
    skipped: i64,
    last_indexed: f64,
}

struct Stats {
    history: Vec<HistoryPoint>,
    total_articles: i64,
    total_bytes: i64,
    total_releases: i64,
    total_skipped: i64,
    start_time: f64,
    groups_indexed: HashSet<String>,
    error_count: u32,
    group_stats: BTreeMap<String, GroupStats>,
    /// headers and post bytes per RATE_BIN seconds, oldest first, RATE_KEEP long
    rate: std::collections::VecDeque<(i64, i64, i64)>,
    /// extra fields for stats.json (servers, groups configured, workers)
    extra: serde_json::Map<String, serde_json::Value>,
}

/// headers/s is kept in buckets this many seconds wide...
pub const RATE_BIN: i64 = 10;
/// ...for this long
const RATE_KEEP: i64 = 6 * 3600;

impl Stats {
    fn new() -> Self {
        Stats {
            history: Vec::new(),
            total_articles: 0,
            total_bytes: 0,
            total_releases: 0,
            total_skipped: 0,
            start_time: now(),
            groups_indexed: HashSet::new(),
            error_count: 0,
            group_stats: BTreeMap::new(),
            rate: std::collections::VecDeque::new(),
            extra: serde_json::Map::new(),
        }
    }

    fn tick(&mut self, p: &Progress, group: &str) {
        let Progress { articles, bytes, releases, skipped } = *p;
        let t = now();

        let bin = t as i64 / RATE_BIN * RATE_BIN;
        match self.rate.back_mut() {
            Some(last) if last.0 == bin => {
                last.1 += articles;
                last.2 += bytes;
            }
            _ => self.rate.push_back((bin, articles, bytes)),
        }
        while self.rate.front().is_some_and(|f| f.0 < bin - RATE_KEEP) {
            self.rate.pop_front();
        }

        self.total_articles += articles;
        self.total_bytes += bytes;
        self.total_releases += releases;
        self.total_skipped += skipped;
        self.history.push(HistoryPoint { t, a: articles, b: bytes });

        if !group.is_empty() {
            use crate::metrics::names::{ARTICLE_BYTES, ARTICLES, ARTICLES_SKIPPED, GROUP_LAST_INDEXED, RELEASES_SEEN};
            // a blocked group's name never becomes a series
            if !crate::filters::group_blocked(group) {
                let labels = [("group", group)];
                ARTICLES.inc(&labels, articles.max(0) as u64);
                ARTICLE_BYTES.inc(&labels, bytes.max(0) as u64);
                RELEASES_SEEN.inc(&labels, releases.max(0) as u64);
                ARTICLES_SKIPPED.inc(&labels, skipped.max(0) as u64);
                GROUP_LAST_INDEXED.set(&labels, t);
            }
            self.groups_indexed.insert(group.to_string());
            let gs = self.group_stats.entry(group.to_string()).or_default();
            gs.articles += articles;
            gs.releases += releases;
            gs.skipped += skipped;
            gs.last_indexed = t;
        }

        let cutoff = t - HISTORY_LEN;
        self.history.retain(|h| h.t >= cutoff);
    }

    /// (peak articles/s, peak bytes/s, avg articles/s, avg bytes/s)
    fn speeds(&self) -> (f64, f64, f64, f64) {
        let mut a_speeds = Vec::new();
        let mut b_speeds = Vec::new();

        for w in self.history.windows(2) {
            let dt = w[1].t - w[0].t;
            if dt > 0.0 {
                a_speeds.push(w[1].a as f64 / dt);
                b_speeds.push(w[1].b as f64 / dt);
            }
        }

        if a_speeds.is_empty() {
            return (0.0, 0.0, 0.0, 0.0);
        }

        let max = |v: &[f64]| v.iter().cloned().fold(f64::MIN, f64::max);
        let avg = |v: &[f64]| v.iter().sum::<f64>() / v.len() as f64;
        (max(&a_speeds), max(&b_speeds), avg(&a_speeds), avg(&b_speeds))
    }

    fn write(&self, group: &str, mode: &str, running: bool, idle: bool) {
        let (peak_a, peak_b, avg_a, avg_b) = self.speeds();
        let db_size = crate::store::database_bytes(&paths::database());

        let groups: serde_json::Map<String, serde_json::Value> = self
            .group_stats
            .iter()
            .map(|(name, g)| {
                (name.clone(), json!({"articles": g.articles, "releases": g.releases, "skipped": g.skipped, "last_indexed": g.last_indexed}))
            })
            .collect();

        let data = json!({
            "running": running,
            "idle": idle,
            "group": group,
            "mode": mode,
            "uptime": (now() - self.start_time) as i64,
            "total_articles": self.total_articles,
            "total_bytes": self.total_bytes,
            "total_releases": self.total_releases,
            "total_skipped": self.total_skipped,
            "history": self.history,
            "groups_indexed": self.groups_indexed.len(),
            "error_count": self.error_count,
            "peak_art_speed": peak_a,
            "peak_byte_speed": peak_b,
            "avg_art_speed": avg_a,
            "avg_byte_speed": avg_b,
            "db_size": db_size,
            "groups": groups,
            "rate_bin": RATE_BIN,
            "rate": self.rate.iter().map(|(t, a, b)| [*t, *a, *b]).collect::<Vec<_>>(),
        });
        let mut data = data;
        if let Some(map) = data.as_object_mut() {
            map.extend(self.extra.clone());
        }

        let _ = write_atomic(&paths::stats_file(), data.to_string());
    }
}

fn backoff(errors: u32) -> Duration {
    Duration::from_secs(2u64.saturating_pow(errors).min(30))
}

/// sleep up to `d`, waking early once `done` says so
pub(crate) async fn nap(d: Duration, done: impl Fn() -> bool) {
    let deadline = Instant::now() + d;
    while !done() && Instant::now() < deadline {
        tokio::time::sleep((deadline - Instant::now()).min(Duration::from_millis(250))).await;
    }
}

/// how often auto_run_compact compacts (ATLAS_COMPACT_EVERY_SECS for tests)
fn compact_every() -> Duration {
    std::env::var("ATLAS_COMPACT_EVERY_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .map(Duration::from_secs)
        .unwrap_or(COMPACT_EVERY)
}

fn settings_of(config: &Config) -> PassSettings {
    PassSettings {
        mode: config.index_mode.clone(),
        batch_size: config.batch_size() as i64,
        request_size: config.request_size(),
        split_min_backlog: config.split_min_backlog(),
        chunk_safety: crate::indexer::CHUNK_SAFETY,
        fallback: false,
        from_top: false,
    }
}

/// `alt.binaries.a, alt.binaries.b` or `3197 groups, 20 at once`
/// Groups indexed at once on each indexing server, as (server index, workers),
/// from config.toml alone (pool indexes follow `usable_servers` order). Runtime
/// changes, like a server found to be article only or a lowered connection
/// limit, dont change the plan, soo they never trigger a rebuild.
fn plan_workers(config: &Config) -> Vec<(usize, usize)> {
    let servers = usable_servers(config);
    let mut indexing: Vec<usize> = (0..servers.len()).filter(|&i| servers[i].indexes()).collect();
    if indexing.is_empty() {
        indexing = (0..servers.len()).collect();
    }
    let conns: Vec<usize> = indexing.iter().map(|&i| servers[i].connections() as usize).collect();
    indexing.into_iter().zip(config.workers_per_server(&conns)).collect()
}

fn groups_label(groups: &[String], workers: usize) -> String {
    if groups.len() <= 3 { groups.join(", ") } else { format!("{} groups, {workers} at once", groups.len()) }
}

/// Say which configured groups wont be indexed (`filters::group_blocked`).
fn say_blocked(blocked: &[String]) {
    if !blocked.is_empty() {
        println!("blocked and not indexed, their names label them for minors: {}", blocked.join(", "));
    }
}

/// State shared by the scheduler's workers for one set of servers.
struct Scheduler {
    native: Option<NativeProfile>,
    native_owner: Option<Arc<crate::pg::indexer_runtime::Owner>>,
    native_workers: Option<crate::postprocess::native::ConfigPin>,
    fatal: AtomicBool,
    ctx: PassContext,
    settings: RwLock<PassSettings>,
    groups: RwLock<Vec<String>>,
    /// configured groups left out of `groups`, see `filters::group_blocked`
    blocked: Mutex<Vec<String>>,
    /// the processing stage, when it's on
    stage: OnceLock<Arc<Stage>>,
    predb: OnceLock<Arc<crate::predb::Service>>,
    /// what the stage was started with: its threads and body permits are fixed then
    stage_shape: StageShape,
    /// (server, groups at once) for every indexing server
    plan: Vec<(usize, usize)>,
    workers: usize,
    /// round robin position in `groups`, per server
    next: Mutex<HashMap<usize, usize>>,
    /// the groups big releases come from (`chunks::media_groups`), taken before the round robin's next
    media: RwLock<Vec<String>>,
    /// groups a worker is on right now, and whose worker (the server it was taken for)
    busy: Mutex<HashMap<String, usize>>,
    /// (group, server) pairs whose day chunks the server leaves alone: for
    /// good (None) when it doesnt carry the group, until then after chunk errors
    skip: Mutex<HashMap<(String, usize), Option<Instant>>>,
    /// consecutive day chunk errors per (group, server)
    chunk_errors: Mutex<HashMap<(String, usize), u32>>,
    /// servers one of whose workers sweeps a split group (see `take_sweep`)
    sweeping: Mutex<HashSet<usize>>,
    /// per server, how many of its workers run a day chunk now (see `next_work`)
    chunking: Mutex<HashMap<usize, usize>>,
    /// groups resting after going idle or after an error
    wait_until: Mutex<HashMap<String, Instant>>,
    /// when `wait_until` was last rid of groups done resting
    wait_pruned: Mutex<Option<Instant>>,
    /// goes up when `groups` changes (with its write lock held)
    groups_changed: AtomicU64,
    /// by rank: every group's servers in order, see `RankCache`
    rank_cache: Mutex<Option<Arc<RankCache>>>,
    /// consecutive errors per group, on each server
    errors: Mutex<HashMap<(String, usize), u32>>,
    /// groups parked after 3 errors in a row, retried after FAILED_RETRY
    failed: Mutex<HashMap<String, Parked>>,
    stats: Arc<Mutex<Stats>>,
    /// wind down (servers changed or stopping): finish current passes, start no new ones
    wind_down: AtomicBool,
    /// the compaction interval passed (set together with `wind_down`)
    compact_due: AtomicBool,
    /// no compaction before this (one couldnt take the lock)
    compact_after: Option<SystemTime>,
    /// auto_run_compact as config.toml has it now
    auto_compact: AtomicBool,
    passes: AtomicUsize,
}

/// A group whose passes failed 3 times in a row: since when, and on which servers. Until `FAILED_RETRY` is up its
/// live passes run on the next server that `Pool::fallback_in` gives, on that server's own cursor, soo posts that
/// land while the home is in trouble arent left unread for hours; the home's cursor stays where it was and picks up
/// from there when the group goes back.
#[derive(Debug)]
struct Parked {
    since: Instant,
    servers: Vec<usize>,
    /// servers that ran a pass standing in for these to its end, soo the first starts at the newest article (see
    /// `PassSettings::from_top`); one that failed before its cursor was set doesnt count
    stood_in: Vec<usize>,
}

/// With groups going by rank: every group's servers in order (`Pool::rank_order` over the tier), worked out once for
/// each change of the groups, the tier or the bans instead of for each group on every `take_group`. Prod's 3087 groups
/// on 9 servers took 9 bans each per look, and the workers of the servers below the best, which by rank have no
/// group of their own, looked at every group on every call with the scheduler's locks held: ~50 headers/s.
struct RankCache {
    /// (`Scheduler::groups_changed`, `Pool::bans_version`) it was worked out for
    version: (u64, u64),
    tier: Vec<usize>,
    /// a learned ban ends then (unix seconds), and with it some group's order
    until: i64,
    /// a group's place in the groups
    idx: HashMap<String, u32>,
    /// each group's order, in `orders`
    of: Vec<u32>,
    /// the orders groups have, each once: groups with no bans all share the first
    orders: Vec<Ranked>,
    /// the groups whose order puts them on each server while none is short of connections, in the groups' order
    home: HashMap<usize, Vec<u32>>,
}

/// One order of servers (`Pool::rank_order`) and the groups that have it.
struct Ranked {
    servers: Vec<usize>,
    /// with `servers` empty (config bans the groups from every server left): where they go, if anywhere
    none_left: Option<usize>,
    /// it's the order of groups banned from every server, `servers` banned too
    all_banned: bool,
    /// in the groups' order
    groups: Vec<u32>,
}

impl Ranked {
    /// the server its groups go to unless one was given another (`RankView::pick`)
    fn target(&self, view: &RankView) -> Option<usize> {
        if self.servers.is_empty() {
            return self.none_left;
        }
        self.servers.iter().copied().find(|&i| !view.saturated(i)).or(self.servers.first().copied())
    }
}

impl RankCache {
    fn new(pool: &Pool, groups: &[String], tier: &[usize], version: (u64, u64)) -> Self {
        let until = pool.next_ban_end();
        let mut index: HashMap<(Vec<usize>, Option<usize>), u32> = HashMap::new();
        let mut orders: Vec<Ranked> = Vec::new();
        let mut of = Vec::with_capacity(groups.len());
        let mut idx = HashMap::with_capacity(groups.len());
        for (i, g) in groups.iter().enumerate() {
            let (servers, all_banned) = pool.rank_order_why(tier, g);
            let none_left =
                if servers.is_empty() { tier.first().copied().filter(|&s| !pool.banned_for_good(g, s)) } else { None };
            let n = *index.entry((servers.clone(), none_left)).or_insert_with(|| {
                orders.push(Ranked { servers, none_left, all_banned, groups: Vec::new() });
                orders.len() as u32 - 1
            });
            orders[n as usize].groups.push(i as u32);
            of.push(n);
            idx.insert(g.clone(), i as u32);
        }
        let mut home: HashMap<usize, Vec<u32>> = HashMap::new();
        for o in &orders {
            if let Some(s) = o.servers.first().copied().or(o.none_left) {
                home.entry(s).or_default().extend(&o.groups);
            }
        }
        for list in home.values_mut() {
            list.sort_unstable();
        }
        RankCache { version, tier: tier.to_vec(), until, idx, of, orders, home }
    }

    fn order_at(&self, i: usize) -> &[usize] {
        &self.orders[self.of[i] as usize].servers
    }

    /// `group`'s order, none for one not among the groups
    fn order(&self, group: &str) -> &[usize] {
        self.idx.get(group).map_or(&[], |&i| self.order_at(i as usize))
    }

    /// the server the passes of the group at `i` go to (not parked), as `Pool::server_for_in` gives it
    fn on(&self, i: usize, group: &str, view: &RankView) -> Option<usize> {
        let o = &self.orders[self.of[i] as usize];
        if o.servers.is_empty() { o.none_left } else { view.pick(&o.servers, group) }
    }

    /// whether `group` isn't read from `server`, one in the tier (`Pool::banned`)
    fn banned(&self, group: &str, server: usize) -> bool {
        self.idx.get(group).is_some_and(|&i| {
            let o = &self.orders[self.of[i as usize] as usize];
            o.all_banned || !o.servers.contains(&server)
        })
    }

    /// `home` of `server` and `extra`, in the groups' order
    fn home_of(&self, server: usize, len: usize, extra: &[u32]) -> std::borrow::Cow<'_, [u32]> {
        let home = self.home.get(&server).map_or(&[][..], Vec::as_slice);
        if extra.is_empty() {
            return std::borrow::Cow::Borrowed(home);
        }
        let mut mark = vec![false; len];
        for &i in home.iter().chain(extra) {
            mark[i as usize] = true;
        }
        std::borrow::Cow::Owned((0..len as u32).filter(|&i| mark[i as usize]).collect())
    }

    /// the groups of the orders `pick` takes and `extra`, in the groups' order
    fn groups_of(&self, len: usize, pick: impl Fn(&Ranked) -> bool, extra: &[u32]) -> std::borrow::Cow<'_, [u32]> {
        let picked: Vec<bool> = self.orders.iter().map(pick).collect();
        if extra.is_empty() {
            let mut which = picked.iter().enumerate().filter(|(_, p)| **p).map(|(n, _)| n);
            match (which.next(), which.next()) {
                (None, _) => return std::borrow::Cow::Borrowed(&[]),
                (Some(n), None) => return std::borrow::Cow::Borrowed(&self.orders[n].groups),
                _ => {}
            }
        }
        let mut mark = vec![false; len];
        for &i in extra {
            mark[i as usize] = true;
        }
        std::borrow::Cow::Owned(
            (0..len as u32).filter(|&i| mark[i as usize] || picked[self.of[i as usize] as usize]).collect(),
        )
    }
}

impl Scheduler {
    fn fail(&self) {
        self.fatal.store(true,Ordering::Release);
        // Record the reason before publishing cancellation. Native NET drain
        // must not refund an attempt ended by configuration or a task panic.
        if let Some(owner)=&self.native_owner {owner.fail(crate::pg::outage::Failure::Fatal);}
        self.ctx.stop.store(true,Ordering::Release);
    }
    fn stopping(&self) -> bool {
        self.wind_down.load(Ordering::Relaxed) || self.ctx.stop.load(Ordering::Relaxed)
    }

    /// Next group due on `server`: one that maps to this server and isnt being
    /// indexed, resting or parked. Each server pulls its own groups soo every
    /// server's connections stay busy.
    /// How many of `server`'s workers may be on the groups big releases come from at once: half, and one at least.
    fn media_share(&self, server: usize) -> usize {
        (self.workers_of(server) / 2).max(1)
    }

    /// workers of `server` that run day chunks ahead of its due groups at once (`next_work`): one per
    /// `STREAM_SLICES` of its connections, at most half its workers. Forward backfill holds every group to the slowest
    /// one's day (`chunks::LOCKSTEP_DAYS`): with one a server, on prod about nine chunks ran at a time and some 450
    /// connections sat idle while the biggest groups' first day waited.
    fn chunk_share(&self, server: usize) -> usize {
        (self.ctx.pool.connections(server) / crate::nntp::STREAM_SLICES).clamp(1, self.chunk_most(server))
    }

    /// workers of `server` that run day chunks at once while it has due groups: half its workers, the other half
    /// keep its groups' live sides going. A worker with nothing else to do takes one past it (`next_work`).
    fn chunk_most(&self, server: usize) -> usize {
        (self.workers_of(server) / 2).max(1)
    }

    /// A day chunk worker's place on `server`, while fewer than `most` of its workers run one. Given back with
    /// `chunk_done`.
    fn start_chunk(&self, server: usize, most: usize) -> bool {
        let mut chunking = self.chunking.lock().unwrap();
        let n = chunking.entry(server).or_insert(0);
        if *n >= most {
            return false;
        }
        *n += 1;
        true
    }

    fn chunk_done(&self, server: usize) {
        let mut chunking = self.chunking.lock().unwrap();
        if let Some(n) = chunking.get_mut(&server) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                chunking.remove(&server);
            }
        }
    }

    /// `take_chunk` in a day chunk worker's place on `server` (`start_chunk`), given back when there is none.
    async fn take_chunk_within(&self, server: usize, db: &Db, most: usize) -> Option<crate::chunks::Claim> {
        if !self.start_chunk(server, most) {
            return None;
        }
        let chunk = take_chunk(self, server, db).await;
        if chunk.is_none() {
            self.chunk_done(server);
        }
        chunk
    }

    fn take_group(&self, server: usize) -> Option<String> {
        let groups = self.groups.read().unwrap();
        if groups.is_empty() {
            return None;
        }

        let now = Instant::now();
        let pool = &self.ctx.pool;
        let tier = pool.indexing_tier();
        if !tier.contains(&server) {
            // this server is resting after a failure, its groups went elsewhere
            return None;
        }
        // by rank: every group's servers in order, worked out again only when something they depend on changed, and
        // before the locks below are taken
        let ranked = pool.ranks().then(|| self.rank_cache(&groups, &tier));

        let mut busy = self.busy.lock().unwrap();
        let mut wait = self.wait_until.lock().unwrap();
        let mut failed = self.failed.lock().unwrap();
        let mut next = self.next.lock().unwrap();
        let next = next.entry(server).or_insert(0);

        failed.retain(|_, parked| parked.since.elapsed() < FAILED_RETRY);
        // thousands of groups can be resting: the ones done resting are let go of once a second, not on every call
        {
            let mut pruned = self.wait_pruned.lock().unwrap();
            if pruned.is_none_or(|t| now.duration_since(t) >= Duration::from_secs(1)) {
                wait.retain(|_, until| *until > now);
                *pruned = Some(now);
            }
        }
        let resting = |g: &str| wait.get(g).is_some_and(|until| *until > now);
        if let Some(cache) = ranked {
            return self.take_ranked(server, &groups, &tier, &cache, &mut busy, &resting, &failed, next);
        }

        // the server a group's passes go to: a parked one's stand-in. none when config bans it from every server left
        let on = |g: &String| match failed.get(g) {
            None => pool.server_for_in(&tier, g),
            Some(parked) => pool.fallback_in(&tier, g, &parked.servers),
        };
        let free = |g: &String| !busy.contains_key(g) && !resting(g) && on(g) == Some(server);

        // a group big releases come from goes as soon as it's due, so it keeps up with what's posted while the round
        // robin goes through all the others; one worker at a time on each, like any group, and at most half of the
        // server's workers on them, so the round robin always moves
        let media = self.media.read().unwrap();
        let on_media = media.iter().filter(|g| busy.contains_key(*g) && on(g) == Some(server)).count();
        if on_media < self.media_share(server)
            && let Some(g) = media.iter().find(|g| groups.contains(g) && free(g))
        {
            busy.insert(g.clone(), server);
            return Some(g.clone());
        }

        for step in 0..groups.len() {
            let idx = (*next + step) % groups.len();
            let g = &groups[idx];

            if !free(g) {
                continue;
            }

            *next = idx + 1;
            busy.insert(g.clone(), server);
            return Some(g.clone());
        }

        None
    }

    /// The groups' `RankCache` for `groups` over `tier`, worked out again when the groups, the tier or the bans
    /// changed, or a learned ban ended.
    fn rank_cache(&self, groups: &[String], tier: &[usize]) -> Arc<RankCache> {
        let pool = &self.ctx.pool;
        let version = (self.groups_changed.load(Ordering::Relaxed), pool.bans_version());
        let mut cache = self.rank_cache.lock().unwrap();
        if let Some(c) = cache.as_ref()
            && c.version == version
            && c.tier == tier
            && c.of.len() == groups.len()
            && (now() as i64) < c.until
        {
            return c.clone();
        }
        let built = Arc::new(RankCache::new(pool, groups, tier, version));
        *cache = Some(built.clone());
        built
    }

    /// `take_group` when groups go by rank, with the scheduler's locks held: the same picks as looking at every group
    /// in turn would give, from only the groups that can be on `server` (see `RankCache`), soo a server no group is
    /// on finds that out without a look at each.
    #[allow(clippy::too_many_arguments)]
    fn take_ranked(
        &self,
        server: usize,
        groups: &[String],
        tier: &[usize],
        cache: &RankCache,
        busy: &mut HashMap<String, usize>,
        resting: &dyn Fn(&str) -> bool,
        failed: &HashMap<String, Parked>,
        next: &mut usize,
    ) -> Option<String> {
        let pool = &self.ctx.pool;
        let mut view = pool.rank_view();
        // the server a group's passes go to: a parked one's stand-in. none when config bans it from every server left
        let on = |g: &str, view: &RankView| match failed.get(g) {
            Some(parked) => pool.fallback_in(tier, g, &parked.servers),
            None => match cache.idx.get(g) {
                Some(&i) => cache.on(i as usize, g, view),
                // not one of the groups (a group big releases come from that config.toml left out)
                None => {
                    let order = pool.rank_order(tier, g);
                    view.pick(&order, g).or_else(|| tier.first().copied().filter(|&i| !pool.banned_for_good(g, i)))
                }
            },
        };
        let free = |g: &str, view: &RankView| !busy.contains_key(g) && !resting(g) && on(g, view) == Some(server);

        // a group big releases come from goes as soon as it's due, like `take_group` says
        let media = self.media.read().unwrap();
        let on_media = media.iter().filter(|g| busy.contains_key(*g) && on(g, &view) == Some(server)).count();
        if on_media < self.media_share(server)
            && let Some(g) = media.iter().find(|g| cache.idx.contains_key(*g) && free(g, &view))
        {
            if !failed.contains_key(g) {
                view.keep(cache.order(g), g, server);
            }
            busy.insert(g.clone(), server);
            return Some(g.clone());
        }

        // the round robin, over the groups that can be on this server: those whose order has it first among the
        // servers with connections to spare, those given to it, and parked ones standing in on it
        let mut extra: Vec<u32> = view.given_to(server).filter_map(|g| cache.idx.get(g).copied()).collect();
        for (g, parked) in failed {
            if pool.fallback_in(tier, g, &parked.servers) == Some(server)
                && let Some(&i) = cache.idx.get(g.as_str())
            {
                extra.push(i);
            }
        }
        let candidates = if view.any_saturated() {
            cache.groups_of(groups.len(), |o| o.target(&view) == Some(server), &extra)
        } else {
            cache.home_of(server, groups.len(), &extra)
        };
        let start = candidates.partition_point(|&i| (i as usize) < *next % groups.len());
        for &i in candidates[start..].iter().chain(&candidates[..start]) {
            let g = &groups[i as usize];
            if !free(g, &view) {
                continue;
            }
            *next = i as usize + 1;
            if !failed.contains_key(g) {
                view.keep(cache.order_at(i as usize), g, server);
            }
            busy.insert(g.clone(), server);
            return Some(g.clone());
        }

        // a group whose server has every worker busy goes to this one when it's next down the group's list and every
        // server above it is just as full, and stays a while (`Pool::assign`). Only while another server is full, and
        // only among the groups that can be on a full one
        let mut taken: HashMap<usize, usize> = HashMap::new();
        for &s in busy.values() {
            *taken.entry(s).or_insert(0) += 1;
        }
        let full = |s: usize| taken.get(&s).copied().unwrap_or(0) >= self.workers_of(s);
        if !tier.iter().chain(taken.keys()).any(|&s| s != server && full(s)) {
            return None;
        }
        let mut extra: Vec<u32> = Vec::new();
        for &s in tier.iter().chain(taken.keys()) {
            if s != server && full(s) {
                extra.extend(view.given_to(s).filter_map(|g| cache.idx.get(g).copied()));
            }
        }
        let candidates = cache.groups_of(
            groups.len(),
            |o| {
                o.servers.iter().position(|&s| s == server).is_some_and(|at| o.servers[..at].iter().all(|&s| full(s)))
                    && o.target(&view).is_some_and(|t| t != server && full(t))
            },
            &extra,
        );
        let start = candidates.partition_point(|&i| (i as usize) < *next % groups.len());
        for &i in candidates[start..].iter().chain(&candidates[..start]) {
            let g = &groups[i as usize];
            if busy.contains_key(g) || resting(g) || failed.contains_key(g) {
                continue;
            }
            let Some(now_on) = on(g, &view).filter(|&s| s != server && full(s)) else { continue };
            let order = cache.order_at(i as usize);
            let Some(at) = order.iter().position(|&s| s == server) else { continue };
            if !order[..at].iter().all(|&s| s == now_on || full(s)) {
                continue;
            }
            *next = i as usize + 1;
            view.assign(g, server);
            busy.insert(g.clone(), server);
            return Some(g.clone());
        }

        None
    }

    /// groups `server`'s workers take at once
    fn workers_of(&self, server: usize) -> usize {
        self.plan.iter().find(|(s, _)| *s == server).map_or(self.workers, |(_, n)| *n)
    }

    #[cfg(test)]
    fn parked(&self, group: &str) -> bool {
        self.failed.lock().unwrap().contains_key(group)
    }

    /// For a pass of `group` on `server`: whether it stands in for the group's own server (parked, see `Parked`), and
    /// whether none on `server` finished since. Never on one of the group's homes (`Pool::homes_of`): one can be the
    /// one to take a group parked on a server it was only on for a while (its home down, or moved there until it goes
    /// back), and that pass is the home's own, on its own cursor, which a stand-in's start at the newest article would
    /// skip past.
    fn stand_in(&self, group: &str, server: usize) -> Option<bool> {
        let failed = self.failed.lock().unwrap();
        let parked = failed.get(group)?;
        if self.ctx.pool.homes_of(group).contains(&server) {
            return None;
        }
        Some(!parked.stood_in.contains(&server))
    }

    /// After a pass of `group` that ran on `server` (see `run_pass_on`); `stood_in`: it stood in for the group's own
    /// server (see `stand_in`).
    fn finish(&self, group: &str, server: usize, stood_in: bool, result: anyhow::Result<Progress>) {
        let states = &self.ctx.states;
        // the group went back home while a stand-in ran: its failure says nothing about the home, nor parks it
        let stand_in_late = stood_in && !self.failed.lock().unwrap().contains_key(group);

        match result {
            Ok(_) => {
                if stood_in
                    && let Some(parked) = self.failed.lock().unwrap().get_mut(group)
                    && !parked.stood_in.contains(&server)
                {
                    parked.stood_in.push(server);
                }
                self.errors.lock().unwrap().remove(&(group.to_string(), server));
                // caught up: give it a rest instead of hammering GROUP every loop
                if states.is_idle(group) && !states.is_backfilling(group) {
                    self.wait_until.lock().unwrap().insert(group.to_string(), Instant::now() + IDLE_RECHECK);
                }
            }
            Err(_) if self.ctx.stop.load(Ordering::Relaxed) => {}
            Err(e) if stand_in_late => {
                self.stats.lock().unwrap().error_count += 1;
                ui::error(&format!("Indexing error ({group} on {}, standing in): {e}", self.ctx.pool.host(server)));
            }
            Err(e) => {
                self.stats.lock().unwrap().error_count += 1;
                let count = {
                    let mut errors = self.errors.lock().unwrap();
                    let c = errors.entry((group.to_string(), server)).or_insert(0);
                    *c += 1;
                    *c
                };

                if count >= 3 {
                    self.errors.lock().unwrap().remove(&(group.to_string(), server));
                    let pool = &self.ctx.pool;
                    let mut failed = self.failed.lock().unwrap();
                    let parked = failed.entry(group.to_string()).or_insert_with(|| Parked {
                        since: Instant::now(),
                        servers: vec![],
                        stood_in: vec![],
                    });
                    if !parked.servers.contains(&server) {
                        parked.servers.push(server);
                    }
                    // by rank, a server that keeps failing a group is left out of it for a day (`Pool::strike`)
                    pool.strike(group, server, "failed three passes in a row");
                    let instead = match pool.fallback_in(&pool.indexing_tier(), group, &parked.servers) {
                        Some(i) => format!(", reading its new posts from {} meanwhile", pool.host(i)),
                        None => String::new(),
                    };
                    ui::error(&format!(
                        "Too many errors on {group} on {}, parking it there for {}m{instead}: {e}",
                        pool.host(server),
                        FAILED_RETRY.saturating_sub(parked.since.elapsed()).as_secs().div_ceil(60)
                    ));
                } else {
                    ui::error(&format!("Indexing error ({group}): {e}"));
                    self.wait_until.lock().unwrap().insert(group.to_string(), Instant::now() + backoff(count));
                }
            }
        }

        self.busy.lock().unwrap().remove(group);
        self.passes.fetch_add(1, Ordering::Relaxed);
    }

    /// After a day chunk of `group` on `server`. Errors count like a pass error
    /// and rest the pair; a server that doesnt carry the group skips its chunks for good.
    fn finish_chunk(&self, group: String, server: usize, result: anyhow::Result<Progress>) {
        let key = (group, server);
        match result {
            Ok(_) => {
                self.chunk_errors.lock().unwrap().remove(&key);
            }
            Err(_) if self.ctx.stop.load(Ordering::Relaxed) => {}
            // the day went back for another server, and this one wont take one that old again soon
            Err(e) if e.downcast_ref::<TooOld>().is_some() => {}
            Err(e) if e.downcast_ref::<NotCarried>().is_some() => {
                println!("{e}, leaving its day chunks to the other servers");
                self.skip.lock().unwrap().insert(key, None);
            }
            Err(e) => {
                self.stats.lock().unwrap().error_count += 1;
                let count = {
                    let mut errors = self.chunk_errors.lock().unwrap();
                    let c = errors.entry(key.clone()).or_insert(0);
                    *c += 1;
                    *c
                };
                ui::error(&format!("Indexing error ({}, day chunk on {}): {e}", key.0, self.ctx.pool.host(server)));
                self.skip.lock().unwrap().insert(key, Some(Instant::now() + backoff(count)));
            }
        }
    }

    /// groups not parked
    fn active_groups(&self) -> Vec<String> {
        let failed = self.failed.lock().unwrap();
        self.groups.read().unwrap().iter().filter(|g| !failed.contains_key(*g)).cloned().collect()
    }

    fn idle(&self) -> bool {
        let active = self.active_groups();
        let states = &self.ctx.states;
        states.all_idle(&active) && !active.iter().any(|g| states.is_backfilling(g))
    }

    fn write_status(&self) {
        let groups = self.groups.read().unwrap().clone();
        let mode = self.settings.read().unwrap().mode.clone();
        let idle = self.idle();
        let errors: u32 = self.errors.lock().unwrap().values().sum();
        let status = if errors > 0 {
            "warning"
        } else if idle {
            "idle"
        } else {
            "running"
        };

        write_status(true, &groups_label(&groups, self.workers), &mode, idle, status, false, errors);
        let mut stats = self.stats.lock().unwrap();
        let servers = self.ctx.pool.server_stats();
        crate::metrics::report_servers(&servers);
        stats.extra.insert("servers".into(), json!(servers));
        stats.extra.insert("groups_configured".into(), json!(groups.len()));
        stats.extra.insert("groups_blocked".into(), json!(self.blocked.lock().unwrap().len()));
        if let Some(stage) = self.stage.get() {
            stats.extra.insert("postprocess".into(), stage.snapshot());
        }
        if let Some(predb) = self.predb.get() {
            stats.extra.insert("predb".into(), predb.snapshot());
        }
        if self.stage_shape.4 {
            stats.extra.insert("media".into(), crate::media::shared().snapshot());
        }
        stats.extra.insert("workers".into(), json!(self.workers));
        stats.extra.insert("pid".into(), json!(std::process::id()));
        let mut load = crate::profile::LOAD.snapshot();
        load["unsaved_headers"] = json!(self.ctx.pool.unsaved_headers());
        load["max_unsaved_headers"] = json!(self.ctx.pool.max_unsaved());
        stats.extra.insert("load".into(), load);
        let live = crate::metrics::Live {
            pool: self.ctx.pool.id(),
            stage: self.stage.get().is_some(),
            predb: self.predb.get().is_some(),
        };
        crate::metrics::report_indexer(&stats.extra, stats.error_count, idle, live);
        stats.write("", &mode, true, idle);
    }
}

/// The groups whose day chunks a worker may take: ones still in config.toml,
/// except `skip`, and none when the mode never backfills.
fn chunk_groups(mode: &str, groups: &[String], skip: &HashSet<String>) -> HashSet<String> {
    if mode == "live" {
        return HashSet::new();
    }
    groups.iter().filter(|g| !skip.contains(*g)).cloned().collect()
}

/// A day chunk of a split group for an idle worker on `server`: the newest
/// one pending, among the tracked groups this server hasnt said it lacks.
async fn take_chunk(sched: &Scheduler, server: usize, db: &Db) -> Option<crate::chunks::Claim> {
    // a server resting after a failure takes no chunks either
    if !sched.ctx.pool.indexing_tier().contains(&server) {
        return None;
    }

    let host = sched.ctx.pool.host(server);
    let skip: HashSet<String> = {
        let now = Instant::now();
        let mut skip = sched.skip.lock().unwrap();
        skip.retain(|_, until| until.is_none_or(|t| t > now));
        skip.keys().filter(|(_, s)| *s == server).map(|(g, _)| g.clone()).collect()
    };
    let mode = sched.settings.read().unwrap().mode.clone();
    let groups = {
        let all = sched.groups.read().unwrap();
        let mut groups = chunk_groups(&mode, &all, &skip);
        // a server a group is banned from takes none of its day chunks either (`Pool::banned`): by config, ranked or
        // not, and by rank for a while, which the groups' orders already worked out say
        if sched.ctx.pool.ranks() && !groups.is_empty() {
            let tier = sched.ctx.pool.indexing_tier();
            let cache = sched.rank_cache(&all, &tier);
            groups.retain(|g| !cache.banned(g, server));
        } else {
            groups.retain(|g| !sched.ctx.pool.banned(g, server));
        }
        groups
    };
    if groups.is_empty() {
        return None;
    }

    let oldest = sched.ctx.states.kept_days(&host);
    match crate::indexer::claim_chunk(db, host, groups, oldest).await {
        Ok(chunk) => chunk,
        Err(e) => {
            ui::error(&format!("couldnt claim a day chunk: {e}"));
            None
        }
    }
}

/// A split group whose day chunks are done for an idle worker on `server`
/// to sweep (see `indexer::run_sweep`): one this server is not the home of
/// and hasnt swept, none while another of its workers sweeps.
async fn take_sweep(sched: &Scheduler, server: usize, db: &Db) -> Option<String> {
    let tier = sched.ctx.pool.indexing_tier();
    if !tier.contains(&server) || !sched.sweeping.lock().unwrap().insert(server) {
        return None;
    }

    let skip: HashSet<String> = {
        let now = Instant::now();
        let mut skip = sched.skip.lock().unwrap();
        skip.retain(|_, until| until.is_none_or(|t| t > now));
        skip.keys().filter(|(_, s)| *s == server).map(|(g, _)| g.clone()).collect()
    };
    let mode = sched.settings.read().unwrap().mode.clone();
    let groups: Vec<String> = {
        let all = sched.groups.read().unwrap();
        let groups = chunk_groups(&mode, &all, &skip);
        let pool = &sched.ctx.pool;
        if pool.ranks() && !groups.is_empty() {
            // by rank, from the groups' orders worked out already, and without writing where each goes (`RankView`)
            let cache = sched.rank_cache(&all, &tier);
            let view = pool.rank_view();
            groups
                .into_iter()
                .filter(|g| {
                    let on = match cache.idx.get(g.as_str()) {
                        Some(&i) => cache.on(i as usize, g, &view),
                        None => Some(server),
                    };
                    on != Some(server) && !pool.banned_for_good(g, server)
                })
                .collect()
        } else {
            groups
                .into_iter()
                .filter(|g| pool.pick_server_in(&tier, g) != server && !pool.banned_for_good(g, server))
                .collect()
        }
    };
    let found = match crate::indexer::sweep_to_take(db, sched.ctx.pool.host(server), groups).await {
        Ok(found) => found,
        Err(e) => {
            ui::error(&format!("couldnt look for a split group to sweep: {e}"));
            None
        }
    };
    if found.is_none() {
        sched.sweeping.lock().unwrap().remove(&server);
    }
    found
}

/// What a worker does next.
enum Work {
    Group(String),
    /// a day chunk, in one of the server's day chunk worker places (`Scheduler::start_chunk`), and whether it went
    /// ahead of the server's due groups
    Chunk(crate::chunks::Claim, bool),
    Sweep(String),
}

/// Next work for a worker on `server`. A due sweep of a split group whose
/// chunks are done (see `take_sweep`) comes first, before its normal groups,
/// soo home groups that keep every worker busy dont hold it off until what
/// it would find ages out. One worker per server sweeps at a time, the
/// others index their groups or take day chunks: up to `chunk_share` workers of a server run a day chunk ahead of
/// its groups, up to `chunk_most` when the worker's last pass found nothing (`found_nothing`), and a worker that
/// would go idle takes one whatever the count. `after_sweep`: the worker's last pick was a sweep or a day chunk ahead
/// of its groups, soo a due group of its own goes before the next.
async fn next_work(sched: &Scheduler, server: usize, db: &Db, after_sweep: bool, found_nothing: bool) -> Option<Work> {
    // right after a sweep (or a chunk ahead of its groups) a due normal group goes
    // first, soo a worker that is alone on its server alternates instead of sweeping forever
    if after_sweep && let Some(group) = sched.take_group(server) {
        return Some(Work::Group(group));
    }
    if let Some(group) = take_sweep(sched, server, db).await {
        return Some(Work::Sweep(group));
    }
    // workers run day chunks ahead of their due groups: with thousands of groups a worker is never idle, soo one
    // waiting for none to be due never took a chunk, and the lockstep (`chunks::LOCKSTEP_DAYS`) holds every group's
    // backfill to the slowest day. A worker whose pass found nothing (a live side with nothing new, a backfill turn
    // waiting on the lockstep) is better put on the frontier days than on the next GROUP
    let most = if found_nothing { sched.chunk_most(server) } else { sched.chunk_share(server) };
    if let Some(chunk) = sched.take_chunk_within(server, db, most).await {
        return Some(Work::Chunk(chunk, true));
    }
    if let Some(group) = sched.take_group(server) {
        return Some(Work::Group(group));
    }
    // nothing due: a chunk rather than idle, however many run
    sched.take_chunk_within(server, db, usize::MAX).await.map(|chunk| Work::Chunk(chunk, false))
}

/// longest an idle worker naps when groups go by rank
const RANKED_IDLE_MAX: Duration = Duration::from_secs(5);

/// How long a worker on `server` that found nothing to do naps, its `idle`th time in a row. A second, but by rank
/// every group is the best server's and the workers of the others mostly find nothing (~375 of prod's ~450): those
/// nap twice as long each time up to `RANKED_IDLE_MAX`, a bit apart soo they dont all wake at once. With that many,
/// one still looks every few hundredths of a second, for a group the best server has no worker free for.
fn idle_nap(ranked: bool, idle: u32, server: usize) -> Duration {
    let base = Duration::from_secs(1);
    if !ranked {
        return base;
    }
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().subsec_nanos();
    let jitter = Duration::from_millis((u64::from(nanos) + server as u64 * 97) % 250);
    (base * 2u32.saturating_pow(idle.min(8))).min(RANKED_IDLE_MAX) + jitter
}

/// One worker on `server`: index whichever of its groups is due next, again and again.
/// All workers share one db connection, soo their writes queue up in order
/// instead of racing for sqlite's write lock (and timing out on a busy db).
async fn worker(sched: Arc<Scheduler>, server: usize, db: Db) {
    let mut after_sweep = false;
    // the last group pass saved no article: the next pick may be a day chunk (see `next_work`)
    let mut found_nothing = false;
    // naps in a row with nothing to do
    let mut idle = 0u32;
    while !sched.stopping() {
        let work = next_work(&sched, server, &db, after_sweep, found_nothing).await;
        found_nothing = false;
        after_sweep = matches!(work, Some(Work::Sweep(_) | Work::Chunk(_, true)));
        if work.is_some() {
            idle = 0;
        }
        let group = match work {
            Some(Work::Group(group)) => group,
            // help with a split group's day chunks. not marked busy, a
            // group's chunks run on several servers at once
            Some(Work::Chunk(chunk, _)) => {
                let settings = sched.settings.read().unwrap().clone();
                let stats = sched.stats.clone();
                let group = chunk.group.clone();
                let mut progress = |p: &Progress| stats.lock().unwrap().tick(p, &group);
                let run = crate::indexer::run_chunk(&sched.ctx, &settings, &db, &chunk, server, &mut progress);
                let result = unless_stopped(&sched.ctx.stop, run).await;
                sched.chunk_done(server);
                let Some(result) = result else { break };
                sched.finish_chunk(chunk.group, server, result);
                continue;
            }
            Some(Work::Sweep(group)) => {
                let settings = sched.settings.read().unwrap().clone();
                let stats = sched.stats.clone();
                let mut progress = |p: &Progress| stats.lock().unwrap().tick(p, &group);
                let run = crate::indexer::run_sweep(&sched.ctx, &settings, &db, &group, server, &mut progress);
                let result = unless_stopped(&sched.ctx.stop, run).await;
                sched.sweeping.lock().unwrap().remove(&server);
                let Some(result) = result else { break };
                sched.finish_chunk(group, server, result);
                continue;
            }
            None => {
                nap(idle_nap(sched.ctx.pool.ranks(), idle, server), || sched.stopping()).await;
                idle += 1;
                continue;
            }
        };

        let mut settings = sched.settings.read().unwrap().clone();
        let stood_in = sched.stand_in(&group, server);
        if let Some(first) = stood_in {
            // its own server is in trouble: this one reads what is new, on its own cursor, and nothing more
            settings.mode = "live".into();
            settings.fallback = true;
            settings.from_top = first;
        }
        let stats = sched.stats.clone();
        let mut progress = |p: &Progress| stats.lock().unwrap().tick(p, &group);

        // stopping drops a pass stuck on the network too (GROUP, a login, a name
        // lookup). safe: db writes run whole on their own thread and the cursor
        // only moves past slices that are saved
        let mut ran_on = server;
        let pass = run_pass_on(&sched.ctx, &settings, &db, &group, server, &mut progress, &mut ran_on);
        let Some(result) = unless_stopped(&sched.ctx.stop, pass).await else { break };
        found_nothing = matches!(&result, Ok(p) if p.articles == 0);
        sched.finish(&group, ran_on, stood_in.is_some(), result);
    }
}

/// `fut` as a task named `name`: one that ends more than a second after the stop was asked for (`since`) says so,
/// soo a slow stop shows which task held it.
async fn named_task(name: String, since: Arc<OnceLock<Instant>>, fut: impl std::future::Future<Output = ()>) {
    fut.await;
    if let Some(at) = since.get()
        && at.elapsed() > Duration::from_secs(1)
    {
        println!("[STOP] {name} ended {:.1}s after the stop", at.elapsed().as_secs_f64());
    }
}

/// Keeps `Scheduler::media` to what the counts say (see `repair::count_media_every`).
async fn follow_media(sched: Arc<Scheduler>, db: Db) {
    while !sched.stopping() {
        match crate::indexer::on_db(&db, |conn| Ok(crate::chunks::media_groups(conn)?)).await {
            Ok(media) => *sched.media.write().unwrap() = media,
            Err(e) => ui::warn(&format!("couldnt read which groups big releases come from: {e:#}")),
        }
        nap(MEDIA_EVERY, || sched.stopping()).await;
    }
}

/// on, database threads, net threads, and whether PreDB's service and the media fan-out run
type StageShape = (bool, usize, usize, bool, bool);

fn stage_shape(config: &Config) -> StageShape {
    let p = &config.postprocess;
    (p.enabled, p.workers, p.body_connections, config.predb.enabled, config.media.any_provider())
}

/// a config the running indexer cant take on as it is
fn needs_rebuild(sched: &Scheduler, config: &Config, servers: &[UsenetServer]) -> bool {
    usable_servers(config) != servers
        || plan_workers(config) != sched.plan
        || config.max_unsaved_headers() != sched.ctx.pool.max_unsaved()
        || stage_shape(config) != sched.stage_shape
}

/// Re-read config.toml while indexing: new groups / mode / batch settings /
/// auto compaction apply right away. Returns when servers, the parallelism or
/// the unsaved headers cap changed (the caller rebuilds) or when stopping.
async fn watch_config(sched: Arc<Scheduler>, servers: Vec<UsenetServer>) {
    loop {
        nap(CONFIG_RELOAD, || sched.stopping()).await;
        if sched.stopping() {
            return;
        }

        let Some(config) = load_config() else {
            if sched.native.is_some() {
                ui::error("native indexing configuration unavailable");
                sched.fail();
                return;
            }
            continue;
        };
        if let Some(profile) = &sched.native
            && let Err(error) = profile.check_reload(&config)
        {
            ui::error(&format!("native indexing configuration refused: {error}"));
            sched.fail();
            return;
        }
        // Native Steps/providers capture these settings on real standard
        // threads. Refuse before any legacy/global reconfiguration instead of
        // continuing with stale worker objects or silently changing their mask.
        if let Some(pin)=&sched.native_workers && let Err(error)=pin.check(&config,&usable_servers(&config),&plan_workers(&config)) {
            ui::error(&error.to_string());sched.fail();return;
        }

        if needs_rebuild(&sched, &config, &servers) {
            println!("config changed, restarting the indexer");
            sched.wind_down.store(true, Ordering::Relaxed);
            return;
        }
        if sched.native.is_some() {
            crate::archive::set_inline_lookups(false);
        } else {
            crate::archive::sync_inline_lookups(&config);
        }
        if let Some(stage) = sched.stage.get() {
            let (stage, config) = (stage.clone(), config.clone());
            let _ = tokio::task::spawn_blocking(move || stage.set_config(&config)).await;
        }
        if let Some(predb) = sched.predb.get() {
            predb.set_config(&config);
        }

        let auto = config.auto_run_compact;
        if sched.auto_compact.swap(auto, Ordering::Relaxed) != auto {
            println!("auto compaction turned {}", if auto { "on" } else { "off" });
        }

        let settings = settings_of(&config);
        {
            let mut current = sched.settings.write().unwrap();
            if settings.mode != current.mode {
                // switching modes starts every group over in the backfill phase
                sched.ctx.states.reset();
                println!("indexer mode set to {}", settings.mode);
            }
            *current = settings;
        }

        let blocked = config.blocked_groups();
        let mut known = sched.blocked.lock().unwrap();
        if *known != blocked {
            say_blocked(&blocked);
            *known = blocked;
        }
        drop(known);

        let groups = config.tracked_groups();
        let mut current = sched.groups.write().unwrap();
        if *current != groups {
            println!("groups changed: {} -> {}", current.len(), groups.len());
            sched.ctx.states.set_tracked(&groups);
            *current = groups;
            sched.groups_changed.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// While auto_run_compact is on (looked at every second, config.toml can
/// change it): once the interval has passed since the last compaction (or
/// since the indexer started, when there was none), wind down so `supervise`
/// can compact.
async fn compact_timer(sched: Arc<Scheduler>) {
    let every = compact_every();
    let last = db::open_at(&paths::database()).ok().and_then(|conn| crate::store::get_meta(&conn, "last_compact").ok());
    // no stored time: count from when the indexer started (rebuilds dont reset it)
    let last = last.flatten().unwrap_or_else(|| sched.stats.lock().unwrap().start_time as i64);
    let due =
        (UNIX_EPOCH + Duration::from_secs(last.max(0) as u64) + every).max(sched.compact_after.unwrap_or(UNIX_EPOCH));
    while !sched.stopping() {
        if sched.auto_compact.load(Ordering::Relaxed) && SystemTime::now() >= due {
            println!("compacting the database, indexing resumes after");
            sched.compact_due.store(true, Ordering::Relaxed);
            sched.wind_down.store(true, Ordering::Relaxed);
            return;
        }
        nap(Duration::from_secs(1), || sched.stopping()).await;
    }
}

/// status.json / stats.json once a second for the menu and dashboard
/// (plus a timing breakdown every 30s with ATLAS_PROFILE)
async fn report(sched: Arc<Scheduler>) {
    let mut last_profile = Instant::now();
    while !sched.stopping() {
        sched.write_status();
        if crate::profile::enabled() && last_profile.elapsed() >= Duration::from_secs(30) {
            println!("{}", crate::profile::report(last_profile.elapsed().as_secs_f64()));
            last_profile = Instant::now();
        }
        nap(Duration::from_secs(1), || sched.stopping()).await;
    }
}

/// Index with this set of servers until stopping, the config needs a rebuild
/// or a compaction is due (true), not before `compact_after`.
async fn run_servers(
    config: &Config,
    pool: Arc<Pool>,
    states: RunStates,
    stats: Arc<Mutex<Stats>>,
    stop: Arc<AtomicBool>,
    compact_after: Option<SystemTime>,
    native: Option<&NativeRuntime>,
) -> anyhow::Result<bool> {
    let stop = native.map_or(stop, |runtime| runtime.owner.stop.clone());
    let servers = usable_servers(config);
    let groups = config.tracked_groups();
    say_blocked(&config.blocked_groups());
    let plan = plan_workers(config);
    let workers: usize = plan.iter().map(|(_, w)| w).sum();

    let per_server: Vec<String> = plan
        .iter()
        .map(|&(i, w)| format!("{} ({} connections, {w} groups at once)", pool.host(i), pool.connections(i)))
        .collect();
    println!("indexing {} groups on {}", groups.len(), per_server.join(" + "));
    states.set_tracked(&groups);

    let sched = Arc::new(Scheduler {
        native: native.map(|runtime| runtime.profile.clone()),
        native_owner: native.map(|runtime|runtime.owner.clone()),
        native_workers: (native.is_some() && (config.postprocess.enabled || config.predb.enabled)).then(||crate::postprocess::native::ConfigPin::capture(config,&servers,&plan)),
        fatal: AtomicBool::new(false),
        ctx: PassContext { pool, states, stop, verbose: false },
        settings: RwLock::new(settings_of(config)),
        groups: RwLock::new(groups),
        blocked: Mutex::new(config.blocked_groups()),
        stage: OnceLock::new(),
        predb: OnceLock::new(),
        stage_shape: stage_shape(config),
        plan: plan.clone(),
        workers,
        next: Mutex::new(HashMap::new()),
        media: RwLock::new(Vec::new()),
        busy: Mutex::new(HashMap::new()),
        skip: Mutex::new(HashMap::new()),
        chunk_errors: Mutex::new(HashMap::new()),
        sweeping: Mutex::new(HashSet::new()),
        chunking: Mutex::new(HashMap::new()),
        wait_until: Mutex::new(HashMap::new()),
        wait_pruned: Mutex::new(None),
        groups_changed: AtomicU64::new(0),
        rank_cache: Mutex::new(None),
        errors: Mutex::new(HashMap::new()),
        failed: Mutex::new(HashMap::new()),
        stats,
        wind_down: AtomicBool::new(false),
        compact_due: AtomicBool::new(false),
        compact_after,
        auto_compact: AtomicBool::new(config.auto_run_compact),
        passes: AtomicUsize::new(0),
    });

    // a rebuild or a compaction stopped the last run: these writers freeze again
    crate::frozen::STOPPING.store(false, Ordering::Relaxed);
    let db = if let Some(runtime) = native {
        let db = runtime.db.clone();
        let pool = sched.ctx.pool.clone();
        crate::indexer::on_db(&db, move |sql| {
            crate::indexer::claim_plain_cursors(sql, &pool)?;
            Ok(())
        })
        .await?;
        db
    } else {
        match db::open().and_then(|conn| db::tune_for_writing(&conn).map(|_| conn)) {
            Ok(conn) => {
                // before any pass: what is there so far was saved by file name
                if let Err(e) = crate::store::start_grouping(&paths::database()) {
                    ui::warn(&format!("couldnt note where collections start: {e:#}"));
                }
                // before any pass: whose the plain cursors are (see claim_plain_cursors)
                if let Err(e) = crate::indexer::claim_plain_cursors(&conn, &sched.ctx.pool) {
                    ui::warn(&format!("couldnt record whose the group cursors are: {e:#}"));
                }
                shared_db(conn)
            }
            Err(e) => {
                ui::error(&format!("couldnt open database: {e}"));
                return Err(e.into());
            }
        }
    };

    // checkpoints off the writer's path, see db::checkpointer
    let checkpoint_stop = Arc::new(AtomicBool::new(false));
    let checkpointer = native.is_none().then(|| db::checkpointer(checkpoint_stop.clone()));

    if native.is_some() {
        crate::archive::set_inline_lookups(false);
    } else {
        crate::archive::sync_inline_lookups(config);
    }
    let mut tasks = tokio::task::JoinSet::new();
    // when the stop was asked for: each task that ends past it says when (`named`)
    let stopping_since: Arc<OnceLock<Instant>> = Arc::new(OnceLock::new());
    macro_rules! named {
        ($name:expr, $fut:expr $(,)?) => {
            named_task($name, stopping_since.clone(), $fut)
        };
    }
    // before the stage: its step reads this file, and a missing one is an error it would spend attempts on
    if native.is_none() && config.predb.enabled
        && let Err(e) = crate::predb::db::create(&paths::predb_database())
    {
        eprintln!("predb: {e}");
    }
    if let Some(runtime)=native {
        let s=sched.clone();
        tasks.spawn(named!("native stage".into(),crate::postprocess::native::run(
            runtime.owner.clone(),config.clone(),sched.ctx.pool.clone(),move||s.stopping())));
    } else if config.postprocess.enabled {
        let stage = Stage::start(StageParams {
            main: paths::database(),
            db: db.clone(),
            pool: sched.ctx.pool.clone(),
            config: config.clone(),
            steps: crate::postprocess::steps_to_run(),
            handle: tokio::runtime::Handle::current(),
        });
        let _ = sched.stage.set(stage.clone());
        let s = sched.clone();
        tasks.spawn(named!("the stage".into(), crate::postprocess::run(stage, move || s.stopping())));
    }
    if native.is_none() && config.predb.enabled {
        let started = crate::predb::Service::start(crate::predb::ServiceParams {
            main: paths::database(),
            config: config.predb.clone(),
            stage: sched.stage.get().cloned(),
            fetch: crate::predb::test_fetch(),
        });
        match started {
            Ok(service) => {
                let _ = sched.predb.set(service.clone());
                let s = sched.clone();
                // in `tasks`: no connection of its outlives this, soo a compaction's checkpoint never meets one
                tasks.spawn(named!("predb".into(), crate::predb::run(service, move || s.stopping())));
            }
            Err(e) => eprintln!("predb: {e:#}"),
        }
    }
    if native.is_none() && config.postprocess.enabled && config.media.any_provider() {
        let s = sched.clone();
        tasks
            .spawn(named!("media".into(), crate::media::run(crate::media::shared(), db.clone(), move || s.stopping())));
    }
    for &(server, count) in &plan {
        for _ in 0..count {
            tasks.spawn(named!(
                format!("a worker on {}", sched.ctx.pool.host(server)),
                worker(sched.clone(), server, db.clone()),
            ));
        }
    }
    // set once stopping: the long scans of every shard (`repair::reading_until`) give up instead of holding the stop
    let scans_stop = Arc::new(AtomicBool::new(false));
    // which groups the big releases come from, for the backfill to take first, and the live passes
    if native.is_none() {
        let s = sched.clone();
        tasks.spawn(named!(
            "the count of big releases".into(),
            crate::repair::count_media_every(db.clone(), move || s.stopping(), scans_stop.clone()),
        ));
        tasks.spawn(named!("follow_media".into(), follow_media(sched.clone(), db.clone())));
    }
    // Spotnet spots of movies and TV, saved as releases (see `spotnet::ingest`)
    if config.spots.enabled {
        let s = sched.clone();
        let (pool, spots) = (sched.ctx.pool.clone(), config.spots.clone());
        tasks.spawn(named!("spots".into(), crate::spotnet::ingest::run(pool, db.clone(), spots, move || s.stopping())));
        // what's said about the spots kept, in a task of its own so a long spot pass doesn't hold it back
        if config.spots.comments {
            let s = sched.clone();
            let (pool, spots) = (sched.ctx.pool.clone(), config.spots.clone());
            tasks.spawn(named!(
                "spot comments".into(),
                crate::spotnet::ingest::run_comments(pool, db.clone(), spots, move || s.stopping())
            ));
        }
    }
    // parts one server's listing left out, read again from another (see `repair`)
    if native.is_none() && sched.ctx.pool.indexing_servers().len() > 1 {
        let s = sched.clone();
        let settings = settings_of(config);
        tasks.spawn(named!(
            "repair".into(),
            crate::repair::run(sched.ctx.clone(), settings, db.clone(), move || s.stopping(), scans_stop.clone()),
        ));
    }
    // parts no listing showed, asked for on every server by the ids their files' other parts name (see `inferred`)
    if config.inferred_parts {
        let s = sched.clone();
        tasks.spawn(named!(
            "inferred parts".into(),
            crate::inferred::run(sched.ctx.clone(), db.clone(), move || s.stopping(), scans_stop.clone())
        ));
    }
    // once stopping, the writers do no freezing: their queues of saves drain as fast as they did before it
    let s = sched.clone();
    let since = stopping_since.clone();
    tasks.spawn(async move {
        while !s.stopping() {
            nap(Duration::from_secs(1), || s.stopping()).await;
        }
        let _ = since.set(Instant::now());
        scans_stop.store(true, Ordering::Relaxed);
        crate::frozen::STOPPING.store(true, Ordering::Relaxed);
    });
    // uploads whose every article has a random subject, linked across groups from the header store's blocks
    // (`nxv_store::linker`), when the store is on
    if let Some(store) = crate::nxv_store::live::handle() {
        let s = sched.clone();
        tasks.spawn(named!(
            "the header store linker".into(),
            crate::nxv_store::linker::run(db.clone(), store, move || s.stopping())
        ));
    }
    tasks.spawn(named!("the config watcher".into(), watch_config(sched.clone(), servers)));
    tasks.spawn(named!("the status report".into(), report(sched.clone())));
    if let Some(runtime) = native {
        let (db, s, owner) = (db.clone(), sched.clone(), runtime.owner.clone());
        tasks.spawn(named!("native storage health".into(), async move {
            while !s.stopping() {
                if let Err(error) = crate::indexer::on_db(&db, |sql| {
                    let _: i64 = sql.one("select 1", &[])?;
                    Ok(())
                })
                .await
                {
                    if !s.ctx.stop.load(Ordering::Acquire) {
                        owner.fail(crate::pg::outage::classify(&error));
                    }
                    return;
                }
                nap(Duration::from_secs(1), || s.stopping()).await;
            }
        }));
    } else {
        tasks.spawn(named!("the compaction timer".into(), compact_timer(sched.clone())));
    }

    // the workers and the config watcher all stop on `stopping()`
    while let Some(result) = tasks.join_next().await {
        if result.is_err() || !sched.stopping() {
            ui::error("an indexer task ended unexpectedly; draining and stopping");
            sched.fail();
        }
        if sched.stopping() {
            sched.wind_down.store(true, Ordering::Relaxed);
        }
    }
    // how long each part of a stop takes, from when it was asked for
    let stop_log = |what: &str| {
        if let Some(since) = stopping_since.get() {
            println!("[STOP] {what} {:.1}s after the stop", since.elapsed().as_secs_f64());
        }
    };
    stop_log("workers, the stage and the other tasks done");

    // the last clone of the db waits for the writers (one may be sealing); the checkpointer goes on meanwhile
    let _ = tokio::task::spawn_blocking(move || drop(db)).await;
    stop_log("writers drained");
    // its last checkpoints after the writers: with them still writing, each waited out its busy timeout
    checkpoint_stop.store(true, Ordering::Relaxed);
    if let Some(checkpointer) = checkpointer {
        let _ = tokio::task::spawn_blocking(move || checkpointer.join()).await;
    }
    stop_log("checkpointer done");
    if let Some(runtime) = native {
        if sched.fatal.load(Ordering::Relaxed) {
            runtime.owner.fail(crate::pg::outage::Failure::Fatal);
        }
        anyhow::ensure!(runtime.owner.failure().is_none(), "native PostgreSQL attempt ended");
    }
    anyhow::ensure!(!sched.fatal.load(Ordering::Relaxed), "indexer task or storage ownership failed");
    Ok(sched.compact_due.load(Ordering::Relaxed))
}

/// How an auto compaction went, for what the indexer does next.
enum Compacted {
    /// done, failed or stopped: indexing goes on
    Ran,
    /// another compaction or a menu write held the lock, nothing was touched
    Busy,
    /// a shard is missing afterwards: the indexer stops
    ShardMissing,
}

/// Compact every shard in this process (the indexer is wound down and its
/// writers are gone, soo nothing else writes), then note the time. Noted on
/// failure too, soo a failing compaction is tried again in 24 hours, not every
/// minute. Not noted when it was stopped: that wasnt a failure, and it runs
/// again on the next start. Nor when it couldnt take the lock: it's tried
/// again after COMPACT_RETRY.
async fn compact(config: &Config, stats: &Arc<Mutex<Stats>>, stop: &Arc<AtomicBool>) -> Compacted {
    write_status(true, "compacting the database", &config.index_mode, false, "running", false, 0);
    let main = paths::database();
    let started = Instant::now();
    let (path, flag) = (main.clone(), stop.clone());
    let report = |msg: &str| println!("{msg}");
    let result = tokio::task::spawn_blocking(move || crate::compact::run(&path, &report, &flag)).await;
    let stopped = stop.load(Ordering::Relaxed);
    match result {
        Ok(Ok(_)) => println!("compacted in {}", crate::dashboard::human_time(started.elapsed().as_secs() as i64)),
        Ok(Err(e)) if e.downcast_ref::<crate::compact::Busy>().is_some() => {
            ui::warn(&format!("couldnt compact the database: {e}. trying again in {}m", COMPACT_RETRY.as_secs() / 60));
            return Compacted::Busy;
        }
        Ok(Err(e)) if stopped => println!("compaction stopped, the shards left are as they were: {e:#}"),
        Ok(Err(e)) => {
            ui::error(&format!("auto compaction failed, the originals were kept: {e:#}"));
            stats.lock().unwrap().error_count += 1;
        }
        Err(e) => {
            ui::error(&format!("auto compaction stopped: {e}"));
            stats.lock().unwrap().error_count += 1;
        }
    }

    // a swap that was cut half way leaves its shard missing, and a writer
    // would make an empty one there
    let missing: Vec<String> =
        crate::store::shard_paths(&main).iter().filter(|p| !p.exists()).map(|p| p.display().to_string()).collect();
    if !missing.is_empty() {
        ui::error(&format!(
            "shards missing after compacting, stopping the indexer. restore from the .precompact.db / .compact.db files next to them: {}",
            missing.join(", ")
        ));
        stats.lock().unwrap().error_count += 1;
        return Compacted::ShardMissing;
    }

    if !stopped && let Ok(conn) = db::open_at(&main) {
        let _ = crate::store::set_meta(&conn, "last_compact", chrono::Utc::now().timestamp());
    }
    Compacted::Ran
}

/// Only typed rejection before work may turn a preflight error into operator stop.
fn native_preflight_exit(
    result: Result<anyhow::Result<()>, tokio::task::JoinError>,
    operator_stopped: bool,
) -> Option<i32> {
    match result {
        Ok(Ok(())) => None,
        Ok(Err(error)) if operator_stopped && crate::pg::outage::is_attempt_ended(&error) => Some(0),
        _ => Some(1),
    }
}

fn run_servers_error_exit(error: &anyhow::Error, native: Option<&NativeRuntime>, stop: &AtomicBool) -> i32 {
    if native.is_some() && stop.load(Ordering::Acquire) && crate::pg::outage::is_attempt_ended(error) {
        return 0;
    }
    ui::error(&format!("indexing stopped: {error}"));
    if native.is_none() {
        stop.store(true, Ordering::Relaxed);
    }
    1
}

/// Index until stopping. Returns the exit code: not 0 when a compaction held
/// the database at the start. `writing` is the hold on compaction taken
/// before setting up the database, if there was one.
async fn supervise(
    stop: Arc<AtomicBool>,
    stats: Arc<Mutex<Stats>>,
    writing: Option<WriteGuard>,
    native: Option<&NativeRuntime>,
) -> i32 {
    let states = RunStates::default();
    let stopping = || {
        stop.load(Ordering::Acquire)
            || native
                .is_some_and(|runtime| runtime.owner.stop.load(Ordering::Acquire) || runtime.owner.failure().is_some())
    };
    let main = paths::database();
    // a compaction that couldnt take the lock waits till then
    let mut compact_after = None;
    // indexing writes the shards: no compaction from elsewhere while it runs
    // (its copy would miss what's written meanwhile). let go only for this
    // indexer's own compaction, taken again after it
    let mut writing = writing;
    let mut started = false;

    while !stopping() {
        let Some(config) = load_config() else {
            let why = crate::config::file_problem().unwrap_or_else(|| "config.toml not found".into());
            ui::error(&format!("error with config, stopped: {why}"));
            if native.is_some() {
                return 1;
            }
            break;
        };

        if let Some(native) = native
            && let Err(error) = native.profile.check_reload(&config)
        {
            ui::error(&format!("native indexing configuration refused: {error}"));
            return 1;
        }
        if let Some(native) = native {
            let owner = native.owner.clone();
            let result = tokio::task::spawn_blocking(move || {
                owner.with(|client| {
                    client.simple_query("select 1")?;
                    Ok(())
                })
            })
            .await;
            if let Some(code) = native_preflight_exit(result, stop.load(Ordering::Acquire)) {
                return code;
            }
            if stopping() {
                // Only a successful reply may end cleanly on operator stop.
                // Recorded owner failures still take precedence after drain.
                return i32::from(!stop.load(Ordering::Acquire));
            }
        }
        let servers = usable_servers(&config);
        if servers.is_empty() {
            ui::error("config missing required fields");
            if native.is_some() {
                return 1;
            }
            break;
        }

        if native.is_none() && writing.is_none() {
            match crate::compact::hold_off_compaction(&main) {
                Ok(guard) => writing = Some(guard),
                Err(e) if !started => {
                    ui::error(&format!("not indexing: {e:#}"));
                    write_status(false, "", &config.index_mode, false, "stopped", true, 1);
                    return 1;
                }
                Err(e) => {
                    ui::warn(&format!("cant index yet: {e:#}"));
                    nap(Duration::from_secs(30), stopping).await;
                    continue;
                }
            }
        }
        started = true;

        let pp = &config.postprocess;
        let pool = Arc::new(
            Pool::new(&servers)
                .with_max_unsaved(config.max_unsaved_headers())
                .with_body_tries(
                    Duration::from_secs(pp.body_timeout_secs.max(1) as u64),
                    usize::try_from(pp.body_retries).unwrap_or(0),
                )
                // by rank, best server first, when `[indexing]` says soo; bans learned are kept beside the databases
                .with_ranking(&crate::config::indexing_config(), Some(crate::paths::app_dir().join("server-bans"))),
        );

        // at least one server has to answer before spinning everything up
        let connected = if let Some(native) = native {
            match unless_stopped(&native.owner.stop, pool.connect()).await {
                Some(result) => result,
                None => {
                    pool.close().await;
                    return i32::from(!stop.load(Ordering::Acquire));
                }
            }
        } else {
            pool.connect().await
        };
        if let Err(e) = connected {
            ui::error(&format!("couldnt reach any usenet server: {e}"));
            let errors = {
                let mut stats = stats.lock().unwrap();
                stats.error_count += 1;
                stats.error_count
            };
            // no scheduler to report them: this is when they matter most
            crate::metrics::report_servers(&pool.server_stats());
            crate::metrics::report_errors(errors);
            write_status(true, "", &config.index_mode, false, "warning", false, 1);
            nap(Duration::from_secs(30), stopping).await;
            continue;
        }

        let result =
            run_servers(&config, pool.clone(), states.clone(), stats.clone(), stop.clone(), compact_after, native)
                .await;
        pool.close().await;
        // its last totals, and no connections while there's no pool
        crate::metrics::report_servers(&pool.server_stats());
        let compact_due = match result {
            Ok(compact_due) => compact_due,
            Err(error) => {
                return run_servers_error_exit(&error, native, &stop);
            }
        };

        if compact_due && !stopping() {
            writing = None;
            // the shared catalog's publisher lets go of the shards first, and stays away till the compaction is done
            let _making_way =
                tokio::task::spawn_blocking(|| crate::shared_catalog::make_way_for_compaction(Duration::from_secs(30)))
                    .await
                    .ok();
            match compact(&config, &stats, &stop).await {
                Compacted::Ran => compact_after = None,
                Compacted::Busy => compact_after = Some(SystemTime::now() + COMPACT_RETRY),
                Compacted::ShardMissing => break,
            }
        }
    }
    0
}

pub fn run() -> i32 {
    // Resolve the selected file before environment credentials can produce a
    // usable Config. A parse/read failure is never a SQLite backend choice.
    let storage=match crate::config::checked_storage_config() {
        Ok(storage)=>storage,
        Err(_)=>{ui::error("storage configuration unavailable");return 1;},
    };
    let Some(config) = load_config().filter(|c| !c.servers.is_empty()) else {
        match crate::config::file_problem() {
            Some(problem) => println!("{problem}"),
            None => println!("no valid config"),
        }
        return 1;
    };

    if usable_servers(&config).is_empty() {
        ui::warn("no password stored for any server, run atlas to set it up first");
        return 1;
    }

    if crate::config::checked_storage_config().ok().as_ref()!=Some(&storage) {
        ui::error("storage configuration changed or unavailable");
        return 1;
    }
    if storage.check().is_err() {
        ui::error("invalid storage configuration");
        return 1;
    }
    if storage.is_postgres() {
        return run_native(config, storage);
    }
    if let Err(e) = write_atomic(&paths::pid_file(), std::process::id().to_string()) {
        ui::error(&format!("couldnt write pid file: {e}"));
        return 1;
    }

    // compaction held off from before setting up: a shard it's swapping
    // would look missing. kept for indexing
    let writing = match db::create_db() {
        Ok(Some(held)) => match crate::compact::refuse_unresolved_backups(&paths::database()) {
            Ok(()) => held,
            Err(e) => {
                ui::error(&format!("not indexing: {e:#}"));
                write_status(false, "", &config.index_mode, false, "stopped", true, 1);
                return 1;
            }
        },
        Ok(None) => {
            ui::error("not indexing: the database is being compacted; try again later");
            write_status(false, "", &config.index_mode, false, "stopped", true, 1);
            return 1;
        }
        Err(e) => {
            ui::error(&format!("couldnt open database: {e:#}"));
            return 1;
        }
    };

    // one time move of an older database into the shards, here and not in the
    // menu because it takes a while on a big one. the menu shows the progress
    let main = paths::database();
    let report = |msg: &str| {
        println!("{msg}");
        write_status(true, msg, &config.index_mode, false, "running", false, 0);
    };
    let writing = match convert_at_start(&main, writing, &report) {
        Ok(held) => held,
        Err(e) => {
            ui::error(&format!("not indexing: {e:#}"));
            write_status(false, "", &config.index_mode, false, "stopped", true, 1);
            return 1;
        }
    };
    {
        // forward backfill's first day, fixed once, in the main database it indexes into (after any conversion); a
        // change in the config needs a restart
        let now = chrono::Utc::now().timestamp();
        match db::open_at(&main)
            .map_err(anyhow::Error::from)
            .and_then(|c| Ok(crate::chunks::set_forward_start(&c, config.backfill_start_days_back, now)?))
        {
            Ok(day) if day > 0 => ui::print(&format!("backfill reads forward from day {day} to live")),
            Ok(_) => {}
            Err(e) => ui::warn(&format!("couldnt set the forward backfill's start: {e:#}")),
        }
        // one indexer runs on a database (the pid file): day chunks its servers still hold claimed are a killed run's,
        // which would hold their days, and the forward backfill's frontier, for `CLAIM_TIMEOUT`
        let hosts = crate::nntp::server_keys(&usable_servers(&config));
        match db::open_at(&main)
            .map_err(anyhow::Error::from)
            .and_then(|c| Ok(crate::chunks::release_left_over(&c, &hosts)?))
        {
            Ok(n) if n > 0 => ui::print(&format!("{n} day chunks left claimed by the last run given back")),
            Ok(_) => {}
            Err(e) => ui::warn(&format!("couldnt give back the day chunks the last run left claimed: {e:#}")),
        }
    }

    let stop = Arc::new(AtomicBool::new(false));

    #[cfg(unix)]
    {
        let _ = signal_hook::flag::register(signal_hook::consts::SIGTERM, stop.clone());
        let _ = signal_hook::flag::register(signal_hook::consts::SIGINT, stop.clone());
    }

    if let Err(e) = crate::nxv_store::live::spawn(&crate::config::header_store_config()) {
        ui::error(&format!("not indexing: {e:#}"));
        write_status(false, "", &config.index_mode, false, "stopped", true, 1);
        return 1;
    }
    crate::metrics::start(&config, crate::metrics::Role::Indexer);
    crate::sealed_store::spawn_offload(&config.sealed_log, stop.clone());
    crate::blob_bytes::spawn(&config.sealed_log, stop.clone());
    crate::shared_catalog::start(&config.shared_catalog, stop.clone());
    crate::overview_archive::spawn(&config.overview_archive);
    if config.metrics.enabled {
        crate::metrics::catalog::spawn(Duration::from_secs(config.metrics.catalog_every_secs), stop.clone());
    }

    let code = index(config, stop, Some(writing), None);
    // the batches still queued for the overview archive
    crate::overview_archive::finish(Duration::from_secs(30));
    // the header store's open slabs sealed and committed
    crate::nxv_store::live::finish();
    code
}

/// Start the explicitly limited native ingestion profile without opening SQLite.
fn run_native(config: Config, storage: crate::pg::StorageConfig) -> i32 {
    use crate::pg::outage::{AttemptOutcome, Backoff, Failure, classify};
    let headers = crate::config::header_store_config();
    if NativeProfile::check(&config, &headers).is_err() || crate::pg::Pool::new(&storage).is_err() {
        ui::error("native PostgreSQL configuration refused");
        return 1;
    }
    if write_atomic(&paths::pid_file(), std::process::id().to_string()).is_err() {
        ui::error("could not write the indexer PID");
        return 1;
    }
    let stop = Arc::new(AtomicBool::new(false));
    #[cfg(unix)]
    {
        let _ = signal_hook::flag::register(signal_hook::consts::SIGTERM, stop.clone());
        let _ = signal_hook::flag::register(signal_hook::consts::SIGINT, stop.clone());
    }
    let final_stop = Arc::new(AtomicBool::new(false));
    let mut identity = None;
    let mut catalog = None;
    let mut initialized = false;
    let mut backoff = Backoff::default();
    let mut paused = false;
    let code = loop {
        if stop.load(Ordering::Acquire) {
            break 0;
        }
        let Some(current) = load_config() else {
            ui::error("native PostgreSQL configuration unavailable");
            break 1;
        };
        if crate::config::checked_storage_config().ok().as_ref()!=Some(&storage)
            || crate::config::header_store_config() != headers
            || NativeProfile::check(&current, &headers).is_err()
        {
            ui::error("native PostgreSQL configuration changed or refused");
            break 1;
        }
        let hosts = crate::nntp::server_keys(&usable_servers(&current));
        NATIVE_PAUSED.store(true, Ordering::Release);
        write_status(true, "Validating PostgreSQL", &current.index_mode, true, "postgres_paused", false, 0);
        let native = match NativeRuntime::open(&current, storage.clone(), headers.clone(), &hosts, &mut identity) {
            Ok(native) => native,
            Err(error) if classify(&error) == Failure::Unavailable => {
                native_pause(&current, &stop, &mut backoff, &mut paused);
                continue;
            }
            Err(_) => {
                ui::error("native PostgreSQL destination refused");
                break 1;
            }
        };
        if stop.load(Ordering::Acquire) {
            native.owner.close();
            break 0;
        }
        identity = Some(native.identity.clone());
        let Some(epoch)=native.owner.epoch() else {
            native.owner.close();
            ui::error("native catalog identity is unavailable");
            break 1;
        };
        let current_catalog=*epoch.catalog().nonce();
        if catalog.is_some_and(|expected|expected!=current_catalog) {
            native.owner.close();
            ui::error("native catalog changed; restart with the intended catalog");
            break 1;
        }
        catalog=Some(current_catalog);
        if !initialized {
            let effective_headers=match native.header_config() {
                Ok(headers)=>headers,
                Err(_)=>{
                    ui::error("native catalog header root refused");
                    native.owner.close();
                    break 1;
                }
            };
            if crate::nxv_store::live::spawn_bounded(&effective_headers).is_err() {
                ui::error("could not start the durable NXV header store");
                native.owner.close();
                break 1;
            }
            crate::metrics::start(&current, crate::metrics::Role::Indexer);
            if let Ok(pool) = crate::pg::Pool::new(&storage) {
                crate::pg::ext::spawn_nightly(
                    Arc::new(pool),
                    crate::pg::ext::dump_dir(),
                    crate::pg::ext::DUMP_HOUR,
                    final_stop.clone(),
                );
            }
            initialized = true;
        }
        if paused {
            ui::success("native PostgreSQL recovered; resuming with a fresh owner");
        }
        paused = false;
        NATIVE_PAUSED.store(false, Ordering::Release);
        backoff = Backoff::default();
        let owner = native.owner.clone();
        let monitor_done = Arc::new(AtomicBool::new(false));
        let (finished, watched, operator, mode) =
            (monitor_done.clone(), owner.clone(), stop.clone(), current.index_mode.clone());
        // Outside the attempt runtime: this also watches run-local cursor
        // claims and pre-provider health work before run_servers starts.
        let monitor = thread::spawn(move || {
            while !finished.load(Ordering::Acquire) {
                if operator.load(Ordering::Acquire) {
                    watched.stop.store(true, Ordering::Release);
                }
                if watched.overdue() && watched.failure().is_none() {
                    watched.fail(Failure::Unavailable);
                    NATIVE_PAUSED.store(true, Ordering::Release);
                    ui::warn("native PostgreSQL reply overdue; indexing paused until the old session drains");
                    write_status(true, "PostgreSQL reply overdue", &mode, true, "postgres_paused", false, 0);
                }
                thread::sleep(Duration::from_millis(50));
            }
        });
        // index owns and drops its Tokio runtime, joining detached blocking work
        // before returning. No old runtime or session is reused by the next run.
        let keeper=crate::pg::stage_words::Keeper::start(&owner,headers.clone());
        let result=match keeper {
            Ok(keeper)=>{
                let result=index(current.clone(),stop.clone(),None,Some(native));
                if let Err(error)=keeper.finish() {owner.fail(classify(&error));}
                result
            },
            Err(error) if crate::pg::outage::is_attempt_ended(&error)
                && (stop.load(Ordering::Acquire)||owner.stop.load(Ordering::Acquire))
                && owner.failure().is_none()=>{drop(native);0},
            Err(error)=>{owner.fail(classify(&error));drop(native);1},
        };
        monitor_done.store(true, Ordering::Release);
        if monitor.join().is_err() {
            owner.fail(Failure::Fatal);
        }
        owner.close();
        match AttemptOutcome::from_exit(result, owner.failure()) {
            AttemptOutcome::Stopped => break result,
            AttemptOutcome::Fatal => {
                ui::error("native PostgreSQL indexing failed");
                break 1;
            }
            AttemptOutcome::Unavailable => native_pause(&current, &stop, &mut backoff, &mut paused),
        }
    };
    final_stop.store(true, Ordering::Release);
    crate::nxv_store::live::finish();
    write_status(false, "", &config.index_mode, false, "stopped", code != 0, u32::from(code != 0));
    NATIVE_PAUSED.store(false, Ordering::Release);
    code
}

fn native_pause(config: &Config, stop: &AtomicBool, backoff: &mut crate::pg::outage::Backoff, paused: &mut bool) {
    if !*paused {
        ui::warn("native PostgreSQL unavailable; indexing paused");
    }
    *paused = true;
    NATIVE_PAUSED.store(true, Ordering::Release);
    write_status(true, "PostgreSQL unavailable", &config.index_mode, true, "postgres_paused", false, 0);
    let until = Instant::now() + backoff.next();
    let Ok(destination)=crate::config::checked_storage_config() else {return;};
    let headers = crate::config::header_store_config();
    let mut checked = Instant::now();
    while Instant::now() < until && !stop.load(Ordering::Acquire) {
        thread::sleep(Duration::from_millis(50));
        if checked.elapsed() >= Duration::from_secs(1) {
            checked = Instant::now();
            if load_config().is_none_or(|current| NativeProfile::check(&current, &headers).is_err())
                || crate::config::checked_storage_config().ok().as_ref()!=Some(&destination)
                || crate::config::header_store_config() != headers
            {
                break;
            }
        }
    }
}

/// The one time move into the shards at start, if `main` needs it, with
/// `setup` the hold taken for setting up. Returns the hold to index under.
/// The conversion runs alone, under the exclusive lock (`convert::run_alone`):
/// another indexer starting, or a `--convert`, mustnt convert alongside it.
/// The setup hold is let go for it, and taken again after, setting up the
/// converted database: refused if a compaction got in between.
pub fn convert_at_start(
    main: &std::path::Path,
    setup: WriteGuard,
    report: &dyn Fn(&str),
) -> anyhow::Result<WriteGuard> {
    use anyhow::Context as _;
    if !crate::convert::needed(main) {
        return Ok(setup);
    }
    drop(setup);
    crate::convert::run_alone(main, report).context("couldnt convert the database, nothing was changed")?;
    db::create_db_holding(main)?.ok_or_else(|| anyhow::anyhow!("the database is being compacted; try again later"))
}

/// The indexing loop from `run`, stopping once `stop` is set (tests set it
/// directly, `run` wires it to SIGTERM / SIGINT).
pub fn run_until(config: Config, stop: Arc<AtomicBool>) -> i32 {
    index(config, stop, None, None)
}

fn index(config: Config, stop: Arc<AtomicBool>, writing: Option<WriteGuard>, native: Option<NativeRuntime>) -> i32 {
    // start sabnzbd soo its ready when you wanna download. on its own thread
    // soo indexing doesnt sit around for up to 90s waiting on it
    if native.is_none() && sab::available() && !sab::is_running() {
        thread::spawn(|| {
            if sab::start() {
                sab::wait_ready(Duration::from_secs(90));
            }
        });
    }

    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(10).max(8);
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(threads)
        .thread_name("atlas-indexer")
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            ui::error(&format!("couldnt start the indexer runtime: {e}"));
            return 1;
        }
    };

    if native.is_none() {
        crate::media::details::start_backfill(stop.clone());
        if let Ok(conn) = crate::db::open() {
            crate::duplicates::forget_old_twins(&conn);
        }
    }

    let stats = Arc::new(Mutex::new(Stats::new()));
    write_status(true, &groups_label(&config.tracked_groups(), 0), &config.index_mode, false, "running", false, 0);

    let code = runtime.block_on(supervise(stop, stats.clone(), writing, native.as_ref()));
    write_status(false, "", &config.index_mode, false, "stopped", code != 0, u32::from(code != 0));
    stats.lock().unwrap().write("", "", false, false);
    code
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checked_background_startup_child() {
        if std::env::var("ATLAS_BG_STARTUP_CHILD").as_deref()!=Ok("1") {return;}
        assert_eq!(super::run(),1,"invalid selected configuration must refuse startup");
    }

    fn refused_selected_startup(dangling:bool) {
        let home=tempfile::tempdir().unwrap();
        fs::write(home.path().join("config.json"),r#"{"storage":{"backend":"sqlite"}}"#).unwrap();
        let toml=home.path().join("config.toml");
        if dangling {
            #[cfg(unix)]
            std::os::unix::fs::symlink(home.path().join("missing.toml"),&toml).unwrap();
            #[cfg(not(unix))]
            return;
        } else {
            fs::write(&toml,"[storage]\nbackend='postgres'\nurl='fixture-secret-sentinel'\n[broken").unwrap();
        }
        let provider=std::net::TcpListener::bind("127.0.0.1:0").unwrap();provider.set_nonblocking(true).unwrap();
        let mut child=std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact","bg_indexer::tests::checked_background_startup_child","--nocapture","--test-threads=1"])
            .env_clear().env("ATLAS_HOME",home.path()).env("ATLAS_BG_STARTUP_CHILD","1")
            .env("ATLAS_NNTP_HOST","127.0.0.1").env("ATLAS_NNTP_PORT",provider.local_addr().unwrap().port().to_string())
            .env("ATLAS_NNTP_USER","fixture").env("ATLAS_NNTP_PASS","fixture")
            .stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).spawn().unwrap();
        let deadline=Instant::now()+Duration::from_secs(3);
        let status=loop {
            if let Some(status)=child.try_wait().unwrap() {break Some(status);}
            if Instant::now()>=deadline {child.kill().unwrap();break None;}
            thread::sleep(Duration::from_millis(10));
        };
        let output=child.wait_with_output().unwrap();
        assert!(status.is_some_and(|status|status.success()),"selected-file refusal must finish before database/provider startup");
        assert!(matches!(provider.accept(),Err(error) if error.kind()==std::io::ErrorKind::WouldBlock),"no provider connection before selector refusal");
        let names=fs::read_dir(home.path()).unwrap().map(|entry|entry.unwrap().file_name().to_string_lossy().into_owned()).collect::<Vec<_>>();
        assert!(names.iter().all(|name|matches!(name.as_str(),"config.toml"|"config.json")),"no database, PID, status or NXV startup artifacts: {names:?}");
        let log=String::from_utf8_lossy(&output.stdout).to_string()+&String::from_utf8_lossy(&output.stderr);
        assert!(!log.contains("fixture-secret-sentinel"),"configuration failure must not print source values");
    }

    #[test]
    fn checked_background_startup_rejects_malformed_selected_toml_before_side_effects() {
        refused_selected_startup(false);
    }

    #[cfg(unix)]
    #[test]
    fn checked_background_startup_rejects_dangling_selected_toml_before_side_effects() {
        refused_selected_startup(true);
    }

    fn native_cursor_fixture() -> (crate::pg::testing::Schema, Config, NativeRuntime, std::net::TcpListener) {
        // These focused regressions require an admitted PostgreSQL fixture;
        // an absent URL must never look like a successful cancellation test.
        let schema = crate::pg::testing::Schema::new().expect("ATLAS_PG_TEST_URL required for cursor admission test");
        let provider = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        provider.set_nonblocking(true).unwrap();
        let mut server = UsenetServer::new("127.0.0.1", "example", "fixture", provider.local_addr().unwrap().port());
        server.ssl = Some(false);
        server.connections = Some(1);
        let mut config = Config {
            servers: vec![server],
            groups: vec!["alt.binaries.fixture".into()],
            inferred_parts: false,
            ..Config::default()
        };
        config.postprocess.enabled = false;
        config.predb.enabled = false;
        config.spots.enabled = false;
        config.media.tvmaze.enabled = false;
        config.media.allocine.enabled = false;
        config.media.details_per_minute = 0;
        let storage = crate::pg::StorageConfig {
            backend: crate::pg::Backend::Postgres,
            url: schema.url.clone(),
            schema: schema.name.clone(),
            max_connections: 4,
            allow_plaintext: true,
        };
        let headers = crate::config::HeaderStoreConfig { enabled: true, ..Default::default() };
        let hosts = crate::nntp::server_keys(&config.servers);
        let native = NativeRuntime::open(&config, storage, headers, &hosts, &mut None).unwrap();
        (schema, config, native, provider)
    }

    fn canceled_native_cursor_admission(failure: Option<crate::pg::outage::Failure>, operator_stopped: bool) {
        use crate::pg::outage::{AttemptOutcome, Failure, is_attempt_ended};
        use std::future::Future;
        use std::task::Poll;

        let (_schema, config, native, provider) = native_cursor_fixture();
        let operator = Arc::new(AtomicBool::new(false));
        let pool = Arc::new(Pool::new(&config.servers));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .max_blocking_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let (entered, ready) = std::sync::mpsc::channel();
        let (release, proceed) = std::sync::mpsc::channel();
        struct Release(Option<std::sync::mpsc::Sender<()>>);
        impl Drop for Release {
            fn drop(&mut self) {
                if let Some(release) = self.0.take() {
                    let _ = release.send(());
                }
            }
        }
        let release = Release(Some(release));
        let blocker = runtime.spawn_blocking(move || {
            entered.send(()).unwrap();
            proceed.recv_timeout(Duration::from_secs(5)).unwrap();
        });
        ready.recv_timeout(Duration::from_secs(2)).unwrap();
        let result = runtime.block_on(async {
            let admission = run_servers(
                &config,
                pool,
                RunStates::default(),
                Arc::new(Mutex::new(Stats::new())),
                operator.clone(),
                None,
                Some(&native),
            );
            let mut admission = std::pin::pin!(admission);
            // run_servers has reached its first await: the actual on_db
            // claim_plain_cursors job is queued behind the occupied lane.
            std::future::poll_fn(|cx| {
                assert!(matches!(admission.as_mut().poll(cx), Poll::Pending), "cursor job did not queue");
                Poll::Ready(())
            })
            .await;
            if let Some(failure) = failure {
                native.owner.fail(failure);
            }
            operator.store(operator_stopped, Ordering::Release);
            native.owner.stop.store(true, Ordering::Release);
            drop(release);
            blocker.await.unwrap();
            tokio::time::timeout(Duration::from_secs(2), admission).await.unwrap()
        });
        let error = result.unwrap_err();
        assert!(is_attempt_ended(&error), "expected typed queued rejection: {error:#}");
        let code = run_servers_error_exit(&error, Some(&native), &operator);
        assert_eq!(native.owner.failure(), failure, "a rejected callback did no storage work");
        match failure {
            Some(Failure::Fatal) => {
                assert!(matches!(AttemptOutcome::from_exit(code, native.owner.failure()), AttemptOutcome::Fatal));
            }
            Some(Failure::Unavailable) => {
                assert!(matches!(AttemptOutcome::from_exit(code, native.owner.failure()), AttemptOutcome::Unavailable));
            }
            None => {
                assert_eq!(code, 0, "operator cancellation at actual cursor admission must exit cleanly");
                assert!(matches!(AttemptOutcome::from_exit(code, native.owner.failure()), AttemptOutcome::Stopped));
            }
        }
        assert!(matches!(provider.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock),
            "cursor admission contacted the provider");
        native.owner.close();
    }

    #[test]
    #[ignore = "requires an admitted isolated PostgreSQL fixture"]
    fn native_cursor_admission_queued_operator_cancellation_is_clean() {
        canceled_native_cursor_admission(None, true);
    }

    #[test]
    #[ignore = "requires an admitted isolated PostgreSQL fixture"]
    fn native_cursor_admission_fatal_before_term_stays_fatal() {
        canceled_native_cursor_admission(Some(crate::pg::outage::Failure::Fatal), true);
    }

    #[test]
    #[ignore = "requires an admitted isolated PostgreSQL fixture"]
    fn native_cursor_admission_lost_owner_keeps_recovery_outcome() {
        canceled_native_cursor_admission(Some(crate::pg::outage::Failure::Unavailable), false);
    }

    #[test]
    #[ignore = "requires an admitted isolated PostgreSQL fixture"]
    fn native_cursor_admission_unexpected_sql_error_stays_fatal_after_term() {
        use crate::pg::outage::{AttemptOutcome, Failure, is_attempt_ended};
        let (schema, config, native, provider) = native_cursor_fixture();
        schema.pool.get().unwrap().batch_execute("drop table meta").unwrap();
        let operator = Arc::new(AtomicBool::new(false));
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let error = runtime
            .block_on(run_servers(
                &config,
                Arc::new(Pool::new(&config.servers)),
                RunStates::default(),
                Arc::new(Mutex::new(Stats::new())),
                operator.clone(),
                None,
                Some(&native),
            ))
            .unwrap_err();
        assert!(!is_attempt_ended(&error), "unexpected SQL failure became cancellation");
        operator.store(true, Ordering::Release);
        native.owner.stop.store(true, Ordering::Release);
        assert_eq!(native.owner.failure(), Some(Failure::Fatal));
        let code = run_servers_error_exit(&error, Some(&native), &operator);
        assert_eq!(code, 1, "unrelated SQL errors must remain fatal after operator stop");
        assert!(matches!(AttemptOutcome::from_exit(code, native.owner.failure()), AttemptOutcome::Fatal));
        assert!(matches!(provider.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock),
            "cursor admission contacted the provider");
        native.owner.close();
    }

    #[test]
    fn native_preflight_queued_operator_cancellation_is_clean() {
        use crate::pg::{
            indexer_runtime::Owner,
            outage::{AttemptOutcome, Failure, is_attempt_ended},
        };
        let Some(schema) = crate::pg::testing::Schema::new() else { return };
        let owner = Arc::new(Owner::new(postgres::Client::connect(&schema.url, postgres::NoTls).unwrap()));
        let (entered, ready) = std::sync::mpsc::channel();
        let (release, proceed) = std::sync::mpsc::channel();
        let current = owner.clone();
        let work = thread::spawn(move || {
            current.with(|_| {
                entered.send(()).unwrap();
                proceed.recv().unwrap();
                Ok(())
            })
        });
        ready.recv_timeout(Duration::from_secs(2)).unwrap();
        let (queued, waiting) = std::sync::mpsc::channel();
        let stale = owner.clone();
        let preflight = thread::spawn(move || {
            queued.send(()).unwrap();
            stale.with::<()>(|_| panic!("canceled preflight executed"))
        });
        waiting.recv_timeout(Duration::from_secs(2)).unwrap();
        // The callback was admitted before TERM, but cannot enter its work
        // until the current callback drains and observes operator cancellation.
        owner.stop.store(true, Ordering::Release);
        release.send(()).unwrap();
        work.join().unwrap().unwrap();
        let error = preflight.join().unwrap().unwrap_err().context("queued native preflight");
        assert!(is_attempt_ended(&error));
        assert_eq!(owner.failure(), None);
        let code = native_preflight_exit(Ok(Err(error)), true).unwrap();
        assert_eq!(code, 0, "typed operator rejection must stop cleanly");
        assert!(matches!(AttemptOutcome::from_exit(code, owner.failure()), AttemptOutcome::Stopped));
        owner.fail(Failure::Unavailable);
        assert!(matches!(AttemptOutcome::from_exit(code, owner.failure()), AttemptOutcome::Unavailable));
        owner.fail(Failure::Fatal);
        assert!(matches!(AttemptOutcome::from_exit(code, owner.failure()), AttemptOutcome::Fatal));
        owner.close();
    }

    #[test]
    fn native_preflight_independent_errors_and_join_failures_remain_failed() {
        use crate::pg::outage::AttemptEnded;
        assert_eq!(native_preflight_exit(Ok(Ok(())), true), None);
        assert_eq!(native_preflight_exit(Ok(Err(AttemptEnded.into())), false), Some(1));
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        for operator_stopped in [false, true] {
            for message in ["native attempt has ended", "native configuration invariant"] {
                assert_eq!(native_preflight_exit(Ok(Err(anyhow::anyhow!(message))), operator_stopped), Some(1));
            }
            let panic = runtime.spawn(async { panic!("owned preflight panic fixture") });
            let result = runtime.block_on(panic);
            assert!(result.as_ref().unwrap_err().is_panic());
            assert_eq!(native_preflight_exit(result, operator_stopped), Some(1));
            let canceled = runtime.spawn(std::future::pending::<anyhow::Result<()>>());
            canceled.abort();
            let result = runtime.block_on(canceled);
            assert!(result.as_ref().unwrap_err().is_cancelled());
            assert_eq!(native_preflight_exit(result, operator_stopped), Some(1));
        }
    }

    #[test]
    fn skipped_articles_are_counted_per_group_and_in_all() {
        let mut stats = Stats::new();
        stats.tick(&Progress { articles: 10, bytes: 5, releases: 2, skipped: 4 }, "alt.binaries.a");
        stats.tick(&Progress { articles: 10, bytes: 5, releases: 2, skipped: 1 }, "alt.binaries.a");
        assert_eq!(stats.total_skipped, 5);
        assert_eq!(stats.group_stats["alt.binaries.a"].skipped, 5);
        assert_eq!(stats.total_articles, 20, "they still count as received");
    }

    #[test]
    fn chunks_only_for_tracked_groups_in_a_backfilling_mode() {
        let groups = vec!["a".to_string(), "b".to_string()];
        let skip = HashSet::from(["b".to_string()]);
        assert_eq!(chunk_groups("backfill", &groups, &skip), HashSet::from(["a".to_string()]));
        assert_eq!(chunk_groups("dynamic", &groups, &HashSet::new()).len(), 2);
        assert!(chunk_groups("live", &groups, &HashSet::new()).is_empty(), "live never backfills");
    }

    fn scheduler(pool: Pool, groups: Vec<String>) -> Scheduler {
        Scheduler {
            native: None,
            native_owner: None,
            native_workers: None,
            fatal: AtomicBool::new(false),
            ctx: PassContext {
                pool: Arc::new(pool),
                states: RunStates::default(),
                stop: Default::default(),
                verbose: false,
            },
            settings: RwLock::new(PassSettings { mode: "backfill".into(), ..PassSettings::default() }),
            groups: RwLock::new(groups),
            blocked: Mutex::new(Vec::new()),
            stage: OnceLock::new(),
            predb: OnceLock::new(),
            stage_shape: stage_shape(&Config::default()),
            plan: vec![],
            workers: 2,
            next: Default::default(),
            media: Default::default(),
            busy: Default::default(),
            skip: Default::default(),
            chunk_errors: Default::default(),
            sweeping: Default::default(),
            chunking: Default::default(),
            wait_until: Default::default(),
            wait_pruned: Default::default(),
            groups_changed: Default::default(),
            rank_cache: Default::default(),
            errors: Default::default(),
            failed: Default::default(),
            stats: Arc::new(Mutex::new(Stats::new())),
            wind_down: AtomicBool::new(false),
            compact_due: AtomicBool::new(false),
            compact_after: None,
            auto_compact: AtomicBool::new(false),
            passes: AtomicUsize::new(0),
        }
    }

    #[test]
    fn native_scheduler_failure_is_recorded_before_worker_cancellation() {
        let Some(schema)=crate::pg::testing::Schema::new() else {return};
        let runtime=crate::pg::stage_queue_tests::runtime(&schema);
        let mut sched=scheduler(Pool::new(&[]),Vec::new());sched.native_owner=Some(runtime.owner.clone());
        sched.fail();
        assert_eq!(runtime.owner.failure(),Some(crate::pg::outage::Failure::Fatal));
        assert!(runtime.owner.stop.load(Ordering::Acquire));assert!(sched.stopping());
        assert!(matches!(crate::pg::outage::AttemptOutcome::from_exit(0,runtime.owner.failure()),crate::pg::outage::AttemptOutcome::Fatal));
        runtime.owner.close();
    }

    /// by rank every group is the best server's; one due while all of that server's workers are busy goes to the next
    /// server down its list, and stays there a while
    #[test]
    fn by_rank_a_group_overflows_when_the_best_servers_workers_are_busy() {
        let pool = Pool::new(&[UsenetServer::new("a.test", "u", "p", 119), UsenetServer::new("b.test", "u", "p", 119)])
            .with_ranking(&crate::config::IndexingConfig { prefer: vec!["a.test".into()], ..Default::default() }, None);
        let groups: Vec<String> = ["g1", "g2", "g3"].iter().map(|g| g.to_string()).collect();
        let mut sched = scheduler(pool, groups);
        sched.plan = vec![(0, 1), (1, 1)];
        assert_eq!(sched.take_group(0).as_deref(), Some("g1"));
        assert_eq!(sched.take_group(1).as_deref(), Some("g2"), "a's one worker is busy: b takes the next");
        assert_eq!(sched.ctx.pool.pick_server("g2"), 1, "and keeps it a while");
        sched.busy.lock().unwrap().remove("g1");
        assert_eq!(sched.take_group(1), None, "a has a worker free again: nothing more for b");
        assert_eq!(sched.take_group(0).as_deref(), Some("g3"));
    }

    /// by rank, a worker of a server down the list that has nothing to take finds that out fast: prod has 3087 groups
    /// on 9 servers and ~375 workers that aren't the best server's, and a scan of every group per call held the
    /// scheduler's locks soo long the indexer read ~50 headers/s
    #[test]
    fn by_rank_a_take_on_a_server_down_the_list_is_quick() {
        let servers: Vec<UsenetServer> =
            (0..9).map(|i| UsenetServer::new(&format!("s{i}.test"), "u", "p", 119)).collect();
        let pool = Pool::new(&servers)
            .with_ranking(&crate::config::IndexingConfig { ranked: true, ..Default::default() }, None);
        let groups: Vec<String> = (0..3000).map(|i| format!("alt.binaries.g{i}")).collect();
        let mut sched = scheduler(pool, groups);
        sched.plan = (0..9).map(|i| (i, 50)).collect();
        let top = sched.ctx.pool.rank_order(&sched.ctx.pool.indexing_tier(), "alt.binaries.g0")[0];
        let other = (0..9).find(|&s| s != top).unwrap();
        // the best server busy with some of its groups, not all of its workers
        for _ in 0..10 {
            assert!(sched.take_group(top).is_some());
        }
        assert_eq!(sched.take_group(other), None, "nothing for a server down the list while the best has room");
        let calls = 200;
        let start = Instant::now();
        for _ in 0..calls {
            assert_eq!(sched.take_group(other), None);
        }
        let each = start.elapsed() / calls;
        println!("take_group on a server down the list: {each:?} a call");
        assert!(each < Duration::from_micros(200), "{each:?} a call");

        // the worst: the best server full and every other group resting, soo the overflow looks at each in vain
        for _ in 10..50 {
            assert!(sched.take_group(top).is_some());
        }
        let later = Instant::now() + Duration::from_secs(600);
        for g in sched.groups.read().unwrap().iter() {
            if !sched.busy.lock().unwrap().contains_key(g) {
                sched.wait_until.lock().unwrap().insert(g.clone(), later);
            }
        }
        let start = Instant::now();
        for _ in 0..calls {
            assert_eq!(sched.take_group(other), None);
        }
        let each = start.elapsed() / calls;
        println!("take_group finding nothing while the best server is full: {each:?} a call");
        // ~0.1 ms in a release build, ~1.7 ms in a debug one
        assert!(each < Duration::from_millis(10), "{each:?} a call");
    }

    /// by rank, `take_group` picks what a look at every group in turn picks (the scan it had before `RankCache`), with
    /// bans, parked groups, groups handed down the list, resting and busy ones, and full servers
    #[test]
    fn by_rank_a_take_picks_what_a_look_at_every_group_does() {
        let servers: Vec<UsenetServer> =
            (0..4).map(|i| UsenetServer::new(&format!("s{i}.test"), "u", "p", 119)).collect();
        let groups: Vec<String> = (0..60).map(|i| format!("g{i}")).collect();
        let cfg = crate::config::IndexingConfig {
            ranked: true,
            ban: vec![("g3".into(), vec!["s0.test".into()]), ("g7".into(), vec!["s0.test".into(), "s1.test".into()])],
            ..Default::default()
        };
        let pool = Pool::new(&servers).with_ranking(&cfg, None);
        for _ in 0..3 {
            pool.strike("g5", 0, "test");
        }
        let mut sched = scheduler(pool, groups.clone());
        sched.plan = vec![(0, 6), (1, 3), (2, 2), (3, 2)];
        *sched.media.write().unwrap() = vec!["g9".into(), "g11".into()];
        let mut seed = 7u64;
        let mut rand = move |n: usize| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (seed >> 33) as usize % n
        };

        // the scan before: every group in turn, nothing written while looking
        let expect = |sched: &Scheduler, server: usize| -> Option<String> {
            let pool = &sched.ctx.pool;
            let tier = pool.indexing_tier();
            let busy = sched.busy.lock().unwrap();
            let wait = sched.wait_until.lock().unwrap();
            let failed = sched.failed.lock().unwrap();
            let next = sched.next.lock().unwrap().get(&server).copied().unwrap_or(0);
            let view = pool.rank_view();
            let now = Instant::now();
            let resting = |g: &String| wait.get(g).is_some_and(|u| *u > now);
            let on = |g: &String| match failed.get(g) {
                Some(p) => pool.fallback_in(&tier, g, &p.servers),
                None => view
                    .pick(&pool.rank_order(&tier, g), g)
                    .or_else(|| tier.first().copied().filter(|&i| !pool.banned_for_good(g, i))),
            };
            let free = |g: &String| !busy.contains_key(g) && !resting(g) && on(g) == Some(server);
            let media = sched.media.read().unwrap();
            let on_media = media.iter().filter(|g| busy.contains_key(*g) && on(g) == Some(server)).count();
            if on_media < sched.media_share(server)
                && let Some(g) = media.iter().find(|g| groups.contains(g) && free(g))
            {
                return Some(g.clone());
            }
            for step in 0..groups.len() {
                let g = &groups[(next + step) % groups.len()];
                if free(g) {
                    return Some(g.clone());
                }
            }
            let mut taken: HashMap<usize, usize> = HashMap::new();
            for &s in busy.values() {
                *taken.entry(s).or_insert(0) += 1;
            }
            let full = |s: usize| taken.get(&s).copied().unwrap_or(0) >= sched.workers_of(s);
            for step in 0..groups.len() {
                let g = &groups[(next + step) % groups.len()];
                if busy.contains_key(g) || resting(g) || failed.contains_key(g) {
                    continue;
                }
                let Some(now_on) = on(g).filter(|&s| s != server && full(s)) else { continue };
                let order = pool.rank_order(&tier, g);
                let Some(at) = order.iter().position(|&s| s == server) else { continue };
                if order[..at].iter().all(|&s| s == now_on || full(s)) {
                    return Some(g.clone());
                }
            }
            None
        };

        let (mut took, mut nothing) = ([0; 4], 0);
        for round in 0..400 {
            match rand(10) {
                // a pass ends
                0..=2 => {
                    let busy: Vec<String> = sched.busy.lock().unwrap().keys().cloned().collect();
                    if !busy.is_empty() {
                        let g = &busy[rand(busy.len())];
                        sched.busy.lock().unwrap().remove(g);
                        if rand(3) == 0 {
                            let until = Instant::now() + Duration::from_secs(60);
                            sched.wait_until.lock().unwrap().insert(g.clone(), until);
                        }
                    }
                }
                3 if rand(4) == 0 => {
                    let g = groups[rand(groups.len())].clone();
                    let server = rand(4);
                    sched
                        .failed
                        .lock()
                        .unwrap()
                        .insert(g, Parked { since: Instant::now(), servers: vec![server], stood_in: vec![] });
                }
                4 if rand(3) == 0 => sched.ctx.pool.assign(&groups[rand(groups.len())], rand(4)),
                5 if rand(20) == 0 => {
                    sched.wait_until.lock().unwrap().clear();
                    sched.failed.lock().unwrap().clear();
                }
                _ => {}
            }
            let server = rand(4);
            let want = expect(&sched, server);
            assert_eq!(sched.take_group(server), want, "round {round}, server {server}");
            if want.is_some() {
                took[server] += 1;
            } else {
                nothing += 1;
            }
        }
        println!("taken per server {took:?}, nothing {nothing}");
        assert!(took.iter().all(|&n| n > 0) && nothing > 0, "every server took some: {took:?}, {nothing}");
    }

    /// by rank, a server parked on a group three times in a day is banned from it for a day
    #[test]
    fn by_rank_a_server_that_keeps_failing_a_group_is_banned_from_it() {
        let pool = Pool::new(&[UsenetServer::new("a.test", "u", "p", 119), UsenetServer::new("b.test", "u", "p", 119)])
            .with_ranking(&crate::config::IndexingConfig { prefer: vec!["a.test".into()], ..Default::default() }, None);
        let sched = scheduler(pool, vec!["g".into()]);
        for _ in 0..9 {
            sched.finish("g", 0, false, Err(anyhow::anyhow!("broken listing")));
        }
        assert!(sched.ctx.pool.banned("g", 0));
        assert_eq!(sched.ctx.pool.homes_of("g"), vec![1]);
    }

    /// a group big releases come from is taken whenever it's free, the others in turn
    #[test]
    fn groups_big_releases_come_from_go_first() {
        let pool = Pool::new(&[UsenetServer::new("a.test", "u", "p", 119)]);
        let groups: Vec<String> = ["a", "b", "c", "m"].iter().map(|g| g.to_string()).collect();
        let mut sched = scheduler(pool, groups);
        sched.plan = vec![(0, 3)];
        *sched.media.write().unwrap() = vec!["m".into(), "gone".into()];
        let take = || sched.take_group(0);
        assert_eq!(take().as_deref(), Some("m"));
        // busy now: the round robin has the rest
        assert_eq!(take().as_deref(), Some("a"));
        sched.busy.lock().unwrap().remove("m");
        assert_eq!(take().as_deref(), Some("m"), "free again, before b");
        assert_eq!(take().as_deref(), Some("b"));
        assert_eq!(take().as_deref(), Some("c"));
        assert_eq!(take(), None, "a group no longer indexed is never taken");
    }

    /// a group that failed 3 times in a row on its server has its passes on the next one meanwhile, the first of them
    /// from the newest article, and goes back once `FAILED_RETRY` is up
    #[test]
    fn a_parked_groups_passes_go_to_the_next_server() {
        let pool = Pool::new(&[UsenetServer::new("a.test", "u", "p", 119), UsenetServer::new("b.test", "u", "p", 119)]);
        let home = pool.pick_server("g");
        let other = 1 - home;
        let sched = scheduler(pool, vec!["g".into()]);
        for _ in 0..3 {
            assert_eq!(sched.take_group(home).as_deref(), Some("g"));
            sched.finish("g", home, false, Err(anyhow::anyhow!("430 Failed forwarding")));
            sched.wait_until.lock().unwrap().clear();
        }
        assert_eq!(sched.take_group(home), None, "parked on its own server");
        assert_eq!(sched.take_group(other).as_deref(), Some("g"), "the next server stands in");
        assert_eq!(sched.stand_in("g", other), Some(true), "its first pass starts at the newest article");
        sched.finish("g", other, true, Err(anyhow::anyhow!("timed out")));
        sched.wait_until.lock().unwrap().clear();
        assert_eq!(sched.take_group(other).as_deref(), Some("g"));
        assert_eq!(sched.stand_in("g", other), Some(true), "a failed one doesnt count");
        sched.finish("g", other, true, Ok(Progress::default()));
        assert_eq!(sched.stand_in("g", other), Some(false), "once one finished");
        sched.wait_until.lock().unwrap().clear();

        // the stand-in fails too: nothing left to try
        for _ in 0..3 {
            assert_eq!(sched.take_group(other).as_deref(), Some("g"));
            sched.finish("g", other, true, Err(anyhow::anyhow!("timed out")));
            sched.wait_until.lock().unwrap().clear();
        }
        assert_eq!((sched.take_group(home), sched.take_group(other)), (None, None));

        // the rest is up: back home, and passes there arent stand-ins
        sched.failed.lock().unwrap().get_mut("g").unwrap().since -= FAILED_RETRY;
        assert_eq!(sched.take_group(home).as_deref(), Some("g"));
        assert_eq!(sched.stand_in("g", home), None);

        // parked on a server it was on only while its home was down: the home takes it back as its own, no stand-in
        sched
            .failed
            .lock()
            .unwrap()
            .insert("g".into(), Parked { since: Instant::now(), servers: vec![other], stood_in: vec![] });
        assert_eq!(sched.stand_in("g", home), None);
        sched.failed.lock().unwrap().clear();

        // a stand-in still running then fails: the home isnt blamed or parked for it
        for _ in 0..3 {
            sched.finish("g", other, true, Err(anyhow::anyhow!("timed out")));
        }
        assert!(!sched.parked("g"));
    }

    /// with more media groups than its share of workers, the round robin still moves
    #[test]
    fn the_groups_big_releases_come_from_take_half_the_workers() {
        let pool = Pool::new(&[UsenetServer::new("a.test", "u", "p", 119)]);
        let groups: Vec<String> = ["m1", "m2", "m3", "a", "b"].iter().map(|g| g.to_string()).collect();
        let mut sched = scheduler(pool, groups);
        sched.plan = vec![(0, 4)];
        *sched.media.write().unwrap() = vec!["m1".into(), "m2".into(), "m3".into()];
        // two jump the queue; past that m3 waits for the round robin, which goes on from where it was to a and b
        let taken: Vec<String> = (0..5).filter_map(|_| sched.take_group(0)).collect();
        assert_eq!(taken, ["m1", "m2", "m3", "a", "b"]);
        // with the round robin past them, a media group done goes straight back to the front
        sched.busy.lock().unwrap().retain(|g, _| g != "m1" && g != "m3");
        assert_eq!(sched.take_group(0).as_deref(), Some("m1"));
    }

    /// a sweep due on a server comes before another group's day chunks
    #[test]
    fn a_due_sweep_comes_before_other_groups_chunks() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        db::create_db_at(&main).unwrap();
        let pool = Pool::new(&[UsenetServer::new("a.test", "u", "p", 119), UsenetServer::new("b.test", "u", "p", 119)]);
        // "done" split with its chunks all done, "busy" with many to go
        let conn = db::open_at(&main).unwrap();
        crate::chunks::add(&conn, "done", 20_000, 19_990).unwrap();
        conn.execute("update backfill_chunks set state = 2 where grp = 'done'", []).unwrap();
        crate::chunks::add(&conn, "busy", 20_000, 10_000).unwrap();
        // the server that isnt done's home sweeps it
        let server = 1 - pool.pick_server("done");
        let sched = scheduler(pool, vec!["busy".into(), "done".into()]);
        let db = shared_db(conn);
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            assert_eq!(take_sweep(&sched, server, &db).await.as_deref(), Some("done"));
            // one sweep per server at a time: the next worker takes a chunk
            assert!(take_sweep(&sched, server, &db).await.is_none());
            assert!(matches!(take_chunk(&sched, server, &db).await, Some(c) if c.group == "busy"));
        });
    }

    /// a config `ban` alone, without ranking: the banned server takes none of the group's live passes or day chunks,
    /// and banned from every server the group isn't read at all
    #[test]
    fn a_config_ban_alone_keeps_a_group_off_a_server() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        db::create_db_at(&main).unwrap();
        let ban = |hosts: &[&str]| crate::config::IndexingConfig {
            ban: vec![("g".into(), hosts.iter().map(|h| h.to_string()).collect())],
            ..Default::default()
        };
        let servers = [UsenetServer::new("a.test", "u", "p", 119), UsenetServer::new("b.test", "u", "p", 119)];
        let home = Pool::new(&servers).pick_server("g");
        let pool = Pool::new(&servers).with_ranking(&ban(&[servers[home].host.as_str()]), None);
        assert!(!pool.ranks());
        let conn = db::open_at(&main).unwrap();
        crate::chunks::add(&conn, "g", 20_000, 10_000).unwrap();
        let mut sched = scheduler(pool, vec!["g".into()]);
        sched.plan = vec![(0, 1), (1, 1)];
        assert_eq!(sched.take_group(home), None, "not on the server it's banned from");
        assert_eq!(sched.take_group(1 - home).as_deref(), Some("g"));
        let db = shared_db(conn);
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            assert!(take_chunk(&sched, home, &db).await.is_none(), "none of its day chunks either");
            assert!(matches!(take_chunk(&sched, 1 - home, &db).await, Some(c) if c.group == "g"));
        });

        let pool = Pool::new(&servers).with_ranking(&ban(&["a.test", "b.test"]), None);
        let mut sched = scheduler(pool, vec!["g".into()]);
        sched.plan = vec![(0, 1), (1, 1)];
        assert_eq!((sched.take_group(0), sched.take_group(1)), (None, None), "banned from both: not read");
    }

    /// home groups that keep every worker busy dont hold a due sweep off
    #[test]
    fn a_due_sweep_runs_while_home_groups_keep_workers_busy() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        db::create_db_at(&main).unwrap();
        let pool = Pool::new(&[UsenetServer::new("a.test", "u", "p", 119), UsenetServer::new("b.test", "u", "p", 119)]);
        let conn = db::open_at(&main).unwrap();
        crate::chunks::add(&conn, "done", 20_000, 19_990).unwrap();
        conn.execute("update backfill_chunks set state = 2 where grp = 'done'", []).unwrap();
        let server = 1 - pool.pick_server("done");
        // more home groups on that server than it has workers
        let homes: Vec<String> =
            (0..).map(|i| format!("home{i}")).filter(|g| pool.pick_server(g) == server).take(4).collect();
        let mut groups = homes.clone();
        groups.push("done".into());
        let sched = scheduler(pool, groups);
        let db = shared_db(conn);
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            assert!(matches!(next_work(&sched, server, &db, false, false).await, Some(Work::Sweep(g)) if g == "done"));
            // one sweeper per server: the others keep to their groups
            assert!(matches!(next_work(&sched, server, &db, false, false).await, Some(Work::Group(_))));
            assert!(matches!(next_work(&sched, server, &db, false, false).await, Some(Work::Group(_))));
        });
    }

    /// a server of two workers runs one day chunk ahead of its due groups (`chunk_share`), the other keeps to its
    /// groups, and that worker takes a due group of its own next
    #[test]
    fn one_worker_a_server_runs_a_day_chunk_ahead_of_due_groups() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        db::create_db_at(&main).unwrap();
        let pool = Pool::new(&[UsenetServer::new("a.test", "u", "p", 119), UsenetServer::new("b.test", "u", "p", 119)]);
        let conn = db::open_at(&main).unwrap();
        crate::chunks::add(&conn, "split", 20_000, 19_990).unwrap();
        let server = pool.pick_server("split");
        let homes: Vec<String> =
            (0..).map(|i| format!("home{i}")).filter(|g| pool.pick_server(g) == server).take(4).collect();
        let mut groups = homes.clone();
        groups.push("split".into());
        let sched = scheduler(pool, groups);
        let db = shared_db(conn);
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            let first = next_work(&sched, server, &db, false, false).await;
            assert!(matches!(&first, Some(Work::Chunk(c, true)) if c.group == "split" && c.day == 20_000));
            // the slot is taken: the next workers keep to their groups
            assert!(matches!(next_work(&sched, server, &db, false, false).await, Some(Work::Group(_))));
            // the slot given back, that worker takes a group of its own first
            sched.chunk_done(server);
            assert!(matches!(next_work(&sched, server, &db, true, false).await, Some(Work::Group(_))));
            assert!(matches!(next_work(&sched, server, &db, false, false).await, Some(Work::Chunk(_, true))));
        });
    }

    /// Day chunk workers scale with a server's connections: up to one per `STREAM_SLICES` of them run frontier days
    /// ahead of due groups, a worker whose pass found nothing joins them up to half the server's workers, and one with
    /// nothing due takes a chunk whatever the count
    #[test]
    fn day_chunk_workers_scale_with_a_servers_connections() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        db::create_db_at(&main).unwrap();
        let mut servers = [UsenetServer::new("a.test", "u", "p", 119), UsenetServer::new("b.test", "u", "p", 119)];
        for s in &mut servers {
            s.connections = Some(64);
        }
        let pool = Pool::new(&servers);
        let conn = db::open_at(&main).unwrap();
        // big groups' frontier day, one chunk each
        let splits: Vec<String> = (0..40).map(|i| format!("big{i}")).collect();
        for g in &splits {
            crate::chunks::add(&conn, g, 20_000, 20_000).unwrap();
        }
        let server = 0;
        let homes: Vec<String> =
            (0..).map(|i| format!("home{i}")).filter(|g| pool.pick_server(g) == server).take(64).collect();
        let mut groups = homes.clone();
        groups.extend(splits.iter().cloned());
        let mut sched = scheduler(pool, groups);
        sched.plan = vec![(0, 64), (1, 64)];
        assert_eq!(sched.chunk_share(server), 64 / crate::nntp::STREAM_SLICES);
        assert_eq!(sched.chunk_most(server), 32);
        let db = shared_db(conn);
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            let mut chunks = 0;
            for _ in 0..sched.chunk_share(server) {
                let w = next_work(&sched, server, &db, false, false).await;
                assert!(matches!(w, Some(Work::Chunk(_, true))));
                chunks += 1;
            }
            // the share is taken: a worker with due groups keeps to them
            assert!(matches!(next_work(&sched, server, &db, false, false).await, Some(Work::Group(_))));
            // one whose pass found nothing takes a frontier day, up to half the workers
            while chunks < sched.chunk_most(server) {
                assert!(matches!(next_work(&sched, server, &db, false, true).await, Some(Work::Chunk(_, true))));
                chunks += 1;
            }
            assert!(matches!(next_work(&sched, server, &db, false, true).await, Some(Work::Group(_))));
            assert_eq!(sched.chunking.lock().unwrap().get(&server), Some(&32));
            // a place given back is taken again
            sched.chunk_done(server);
            assert!(matches!(next_work(&sched, server, &db, false, true).await, Some(Work::Chunk(_, true))));
            // nothing due on this server: a chunk past the count rather than idle
            sched.busy.lock().unwrap().extend(sched.groups.read().unwrap().iter().map(|g| (g.clone(), 1)));
            assert!(matches!(next_work(&sched, server, &db, false, false).await, Some(Work::Chunk(_, false))));
            assert_eq!(sched.chunking.lock().unwrap().get(&server), Some(&33));
        });
    }

    #[test]
    fn a_provider_turning_on_or_off_rebuilds() {
        let mut config = Config::default();
        config.media.tvmaze.enabled = false;
        let before = stage_shape(&config);
        config.media.tvmaze.enabled = true;
        assert_ne!(stage_shape(&config), before);
    }

    #[test]
    fn predb_turning_on_or_off_rebuilds() {
        let mut config = Config::default();
        let before = stage_shape(&config);
        config.predb.enabled = true;
        assert_ne!(stage_shape(&config), before);
    }

    /// a worker alone on its server alternates a due sweep with its home group
    #[test]
    fn a_lone_worker_alternates_a_due_sweep_with_its_home_group() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        db::create_db_at(&main).unwrap();
        let pool = Pool::new(&[UsenetServer::new("a.test", "u", "p", 119), UsenetServer::new("b.test", "u", "p", 119)]);
        let conn = db::open_at(&main).unwrap();
        crate::chunks::add(&conn, "done", 20_000, 19_990).unwrap();
        conn.execute("update backfill_chunks set state = 2 where grp = 'done'", []).unwrap();
        let server = 1 - pool.pick_server("done");
        let home = (0..).map(|i| format!("home{i}")).find(|g| pool.pick_server(g) == server).unwrap();
        let sched = scheduler(pool, vec![home.clone(), "done".into()]);
        let db = shared_db(conn);
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            // the sweep first, as before
            assert!(matches!(next_work(&sched, server, &db, false, false).await, Some(Work::Sweep(g)) if g == "done"));
            sched.sweeping.lock().unwrap().remove(&server);
            // then the due home group, though the sweep is still due
            assert!(matches!(next_work(&sched, server, &db, true, false).await, Some(Work::Group(g)) if g == home));
            // and the sweep again after the group
            assert!(matches!(next_work(&sched, server, &db, false, false).await, Some(Work::Sweep(g)) if g == "done"));
        });
    }

    #[test]
    fn a_stage_turning_on_or_off_rebuilds() {
        let sched = scheduler(Pool::new(&[]), vec![]);
        let mut config = Config::default();
        let servers = usable_servers(&config);
        config.max_unsaved_headers = Some(sched.ctx.pool.max_unsaved() as u64);
        let plan_same = plan_workers(&config) == sched.plan;
        assert!(plan_same);
        assert!(!needs_rebuild(&sched, &config, &servers));
        config.postprocess.enabled = false;
        assert!(needs_rebuild(&sched, &config, &servers));
        config.postprocess.enabled = true;
        config.postprocess.workers += 1;
        assert!(needs_rebuild(&sched, &config, &servers), "its thread counts are fixed when it starts");
        config.postprocess.workers -= 1;
        config.postprocess.body_connections += 1;
        assert!(needs_rebuild(&sched, &config, &servers));
    }
}
