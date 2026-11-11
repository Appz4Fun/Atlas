//! The store: routing rows to slabs, the sync batcher, sealing, the single committer, the keep set and startup
//! recovery.
//!
//! - `record` puts a fetched slice's rows into its group's slabs under the group's lock and frames them for the slab
//!   logs; it hands back a `Ticket` the caller waits on before it moves its read cursor.
//! - The batcher wakes every `sync_every`: under each group's lock it swaps out the pending log bytes and takes the
//!   slabs ready to seal; outside the locks it writes the bytes, syncs every touched log (one full drive flush), and
//!   only then counts the group's epoch as synced, which answers the tickets. Slabs it took go to the sealers.
//! - Sealers sort and dedup a slab's rows and build its block.
//! - The committer takes every block waiting, reserves each one's space in its day segment, writes them all, syncs
//!   the arenas (one full flush), writes their manifest entries, syncs those (one more), publishes them and deletes
//!   their slabs' logs.
//! - Any write or sync error stops the store (`failed`): tickets error, nothing more is written, and recovery runs at
//!   the next start.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs::File;
use std::io::Read;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, SyncSender, channel, sync_channel};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use nntp_xover_engine::container::{BlockMeta, MAX_BLOCK_ROWS, NxvBlockBuilder};
use nntp_xover_engine::date::parse_rfc2822_to_epoch;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, TryAcquireError};

use super::segment::{ENTRY_LEN, Entry, Reserved, Segment, parse_day_name};
use super::slab::{self, Sealed, Slab};
use super::sync::sync_all_durable;

pub const DAY: i64 = 86_400;
/// A slab's lines at most, in bytes: a block stays well under the format's 1 GiB limit whatever the line lengths.
pub const SLAB_MAX_BYTES: usize = 256 << 20;
/// Small recoveries stay open for deduplication against cursor rereads. Seal excess
/// logs one at a time, so a backlog cannot be duplicated in memory at startup.
const RECOVERY_OPEN_BYTES: usize = 64 << 20;
const RECOVERY_OPEN_LOGS: usize = 1024;
const MAX_RECOVERY_LOG_BYTES: u64 = (SLAB_MAX_BYTES + slab::MAX_LINE + MAX_BLOCK_ROWS * 16) as u64;
const LIVE_MEMORY_BUDGET: usize = 512 << 20;
const FETCH_HEADROOM: usize = 16 << 20;
/// How long a day claimed here stays kept after its last claim, on top of the keep set's source.
pub const RECENT_CLAIM: Duration = Duration::from_secs(6 * 3600);

#[derive(Clone, Debug)]
pub struct StoreConfig {
    pub root: PathBuf,
    /// rows a slab holds before it's sealed (the block format's limit at most)
    pub seal_rows: usize,
    /// a slab with no new row for this long is sealed
    pub seal_idle: Duration,
    /// how often the batcher writes and syncs the slab logs
    pub sync_every: Duration,
    /// a segment's writer files close after this long without a write
    pub writer_idle: Duration,
    /// threads building blocks
    pub sealers: usize,
    /// Shared allocation budget for the live slab pipeline, in estimated bytes.
    pub memory_budget: usize,
}

impl StoreConfig {
    pub fn new(root: impl Into<PathBuf>) -> StoreConfig {
        StoreConfig {
            root: root.into(),
            seal_rows: MAX_BLOCK_ROWS,
            seal_idle: Duration::from_secs(600),
            sync_every: Duration::from_millis(100),
            writer_idle: Duration::from_secs(600),
            sealers: std::thread::available_parallelism().map_or(2, |n| n.get().clamp(1, 4)),
            memory_budget: LIVE_MEMORY_BUDGET,
        }
    }
}

/// What a fetch was reading, for clamping the dates of a slice too small to have a useful median.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Clamp {
    /// live reading at `now` (unix seconds)
    Live { now: i64 },
    /// a backfill chunk of `day` (days since the epoch)
    Backfill { day: i64, now: i64 },
    /// a listing whose read isn't known (the connection layer): the median of whatever dates it has, however few
    Unknown { now: i64 },
}

/// Slices with fewer dated rows than this are clamped by their read's context instead of their median.
pub const MEDIAN_MIN_ROWS: usize = 16;

impl Clamp {
    fn now(self) -> i64 {
        match self {
            Clamp::Live { now } | Clamp::Backfill { now, .. } | Clamp::Unknown { now } => now,
        }
    }

    /// The (from, to) every row of a slice is clamped into, from the dates it has.
    pub fn bounds(self, dates: &[i64]) -> (i64, i64) {
        let cap = self.now() + 3600;
        let median_ok = match self {
            Clamp::Unknown { .. } => !dates.is_empty(),
            _ => dates.len() >= MEDIAN_MIN_ROWS,
        };
        if median_ok {
            let mut d = dates.to_vec();
            d.sort_unstable();
            let median = d[d.len() / 2].min(cap);
            return (median - DAY, (median + DAY).min(cap));
        }
        match self {
            Clamp::Live { now } | Clamp::Unknown { now } => (now - 2 * DAY, cap),
            Clamp::Backfill { day, .. } => (day * DAY - DAY, (day * DAY + 2 * DAY - 1).min(cap)),
        }
    }
}

/// Which days the store keeps. A day stays while it's in the live window, within a day of a backfill day being read,
/// or within a day of an hour the linker hasn't linked; and, checked by expiry itself, while it has an unsealed slab.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KeepSet {
    /// the first day of the live window: the day before the oldest live cursor's
    pub live_from: i64,
    pub claimed: BTreeSet<i64>,
    pub unlinked: BTreeSet<i64>,
}

impl KeepSet {
    pub fn keeps(&self, day: i64) -> bool {
        day >= self.live_from
            || self.claimed.range(day - 1..=day + 1).next().is_some()
            || self.unlinked.range(day - 1..=day + 1).next().is_some()
    }
}

/// A slice's rows durable in the logs once `wait` returns Ok: its read cursor may move then.
pub struct Ticket {
    group: Arc<Group>,
    epoch: u64,
    shared: Arc<Shared>,
    /// the slice's rows by posting day, kept or not (empty for a `sync_point`): what tells a backfill day is still
    /// being read (`live::record`)
    pub days: Vec<(i64, usize)>,
}

impl Ticket {
    pub fn wait(&self, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        let mut g = self.shared.synced_lock.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if let Some(e) = self.shared.failure() {
                bail!("the header store stopped: {e}");
            }
            if self.group.synced_epoch.load(Ordering::Acquire) >= self.epoch {
                return Ok(());
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                bail!("the header store didnt sync {} in time", self.group.name);
            }
            g = self.shared.synced.wait_timeout(g, left).unwrap_or_else(|e| e.into_inner()).0;
        }
    }
}

struct Slabs {
    /// the slab taking rows, by (server, day)
    open: HashMap<(u16, i64), Slab>,
    /// slabs that filled up between ticks: sealed at the next, with their last bytes
    full: Vec<Slab>,
    /// swaps done: rows pushed now belong to epoch `epoch + 1`
    epoch: u64,
    /// a log was made since the last tick: the directory is synced with the next
    dir_dirty: bool,
    /// days the linker asked to have sealed at the next tick
    seal_days: BTreeSet<i64>,
}

struct Group {
    name: String,
    dir: PathBuf,
    slabs: Mutex<Slabs>,
    segments: RwLock<BTreeMap<i64, Arc<Segment>>>,
    next_gen: AtomicU32,
    synced_epoch: AtomicU64,
}

impl Group {
    fn segment(&self, day: i64) -> Result<Arc<Segment>> {
        if let Some(s) = self.segments.read().unwrap_or_else(|e| e.into_inner()).get(&day) {
            return Ok(Arc::clone(s));
        }
        let mut segs = self.segments.write().unwrap_or_else(|e| e.into_inner());
        if let Some(s) = segs.get(&day) {
            return Ok(Arc::clone(s));
        }
        let (seg, _) = Segment::open(&self.dir, day)?;
        segs.insert(day, Arc::clone(&seg));
        Ok(seg)
    }
}

/// State the store's threads share.
struct Shared {
    failed: Mutex<Option<String>>,
    stopping: AtomicBool,
    synced_lock: Mutex<()>,
    synced: Condvar,
    /// wakes the batcher early (to stop)
    wake_lock: Mutex<()>,
    wake: Condvar,
    memory: Arc<Semaphore>,
    fetches: Arc<Semaphore>,
    memory_waiters: AtomicUsize,
    drain_requested: AtomicBool,
    /// rows refused because their day isn't kept, and repeats dropped at seal
    dropped_unkept: AtomicU64,
    dropped_repeats: AtomicU64,
    blocks_committed: AtomicU64,
    /// slabs handed to the sealers and not yet committed (or let go): (group, generation)
    inflight: Mutex<HashMap<(String, u32), i64>>,
}

impl Shared {
    fn landed(&self, group: &str, slab_gen: u32) {
        self.inflight.lock().unwrap_or_else(|e| e.into_inner()).remove(&(group.to_string(), slab_gen));
    }

    fn failure(&self) -> Option<String> {
        self.failed.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn fail(&self, e: anyhow::Error) {
        let mut f = self.failed.lock().unwrap_or_else(|e| e.into_inner());
        if f.is_none() {
            eprintln!("header store: stopping after a write error: {e:#}");
            *f = Some(format!("{e:#}"));
        }
        drop(f);
        self.memory.close();
        self.fetches.close();
        let _g = self.synced_lock.lock().unwrap_or_else(|e| e.into_inner());
        self.synced.notify_all();
    }
}

/// A slab on its way to the sealers.
type SealJob = (Arc<Group>, Slab);

/// A block on its way to the committer.
struct Pending {
    group: Arc<Group>,
    day: i64,
    slab_gen: u32,
    block: Vec<u8>,
    log: PathBuf,
    _memory: Option<OwnedSemaphorePermit>,
}

struct MemoryWaiter<'a>(&'a AtomicUsize);

impl Drop for MemoryWaiter<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

// Charge the row arena, log frame, metadata and sealing scratch together.
fn row_charge(line: &[u8]) -> usize {
    if line.len() <= slab::MAX_LINE { line.len() * 4 + 256 } else { 0 }
}

fn listing_charge(lines: &[Vec<u8>]) -> usize {
    lines.iter().fold(0, |sum, line| sum.saturating_add(row_charge(line)))
}

fn parse_row(line: &[u8]) -> Option<(u64, Option<i64>)> {
    if line.len() > slab::MAX_LINE || line.contains(&b'\n') || line.contains(&b'\r') {
        return None;
    }
    Some((slab::art_of(line)?, line.split(|&b| b == b'\t').nth(3).and_then(parse_rfc2822_to_epoch)))
}

/// PostgreSQL's durable complete-coverage check. Unknown/error means retain.
/// Called outside store locks and never with an unbounded segment inventory.
pub type LinkerCoverage = Arc<dyn Fn(&str, i64, [u8; 32], usize) -> Result<bool> + Send + Sync>;

/// Blocks expiry during the source snapshot/staging boundary. The guard is
/// independent of day count and must be held before enumerating inventory.
pub struct LinkerRetentionPin {
    store: Arc<Store>,
}
impl Drop for LinkerRetentionPin {
    fn drop(&mut self) {
        self.store.linker_pins.fetch_sub(1, Ordering::AcqRel);
    }
}

pub struct Store {
    cfg: StoreConfig,
    groups: RwLock<HashMap<String, Arc<Group>>>,
    servers: Mutex<Vec<String>>,
    keep: RwLock<KeepSet>,
    shared: Arc<Shared>,
    seal_tx: Mutex<Option<Sender<SealJob>>>,
    /// expiry waits until the linker has finished a pass, soo days it hasn't linked aren't let go after a start
    expiry_armed: AtomicBool,
    linker_pins: AtomicUsize,
    retention_gate: Mutex<()>,
    linker_coverage: RwLock<Option<LinkerCoverage>>,
    coverage_cursor: Mutex<Option<(String, i64)>>,
    /// where a fresh keep set comes from (`live::keep_now`): housekeeping and a refused row ask it, soo a keep set set
    /// earlier never wins over a newer one
    keep_source: Mutex<Option<KeepSource>>,
    /// days claimed here (`add_claimed`, or a read found on a day not kept) and when: kept for `RECENT_CLAIM` past their
    /// last claim, on top of what the source says
    recent_claims: Mutex<BTreeMap<i64, Instant>>,
    /// held while the store is open: a second process opening it would replay and append to the same files
    _lock: File,
    threads: Mutex<Vec<std::thread::JoinHandle<()>>>,
    /// one tick at a time: two would interleave their writes to a log
    tick_lock: Mutex<()>,
    /// Intake in progress when close starts must reach the final tick.
    recording: RwLock<()>,
}

/// What `open` found and did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Opened {
    pub segments: usize,
    pub blocks: usize,
    pub expired_segments: usize,
    pub logs_replayed: usize,
    pub rows_replayed: usize,
    pub logs_already_committed: usize,
    pub logs_unkept: usize,
}

/// A group's directory name: the name itself when it's plain, otherwise percent-encoded.
pub fn encode_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for b in name.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'+' | b'_') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    if out.is_empty() || out.starts_with('.') || out.starts_with('_') { format!("%{}", out) } else { out }
}

pub fn decode_name(dir: &str) -> Option<String> {
    let dir = dir.strip_prefix('%').filter(|r| r.is_empty() || r.starts_with('.') || r.starts_with('_')).unwrap_or(dir);
    let b = dir.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            out.push(u8::from_str_radix(std::str::from_utf8(b.get(i + 1..i + 3)?).ok()?, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

const STATE_DIR: &str = "_state";

/// A function giving the keep set as of now.
pub type KeepSource = Arc<dyn Fn() -> KeepSet + Send + Sync>;

/// An exclusive lock on the store's `_state/lock`, refused when another process holds it.
fn lock_store(root: &Path) -> Result<File> {
    use std::os::fd::AsRawFd;
    let path = root.join(STATE_DIR).join("lock");
    let f = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(&path)?;
    // SAFETY: flock on a descriptor this File owns
    if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        bail!("{} is in use by another process (the indexer?)", root.display());
    }
    Ok(f)
}
const SERVERS_FILE: &str = "servers";

impl Store {
    /// Opens the store at `cfg.root`, heals its segments and recovers its slab logs before starting the workers.
    /// Small recoveries stay open for deduplication against rereads. Excess logs are sealed durably in place.
    pub fn open(cfg: StoreConfig, keep: KeepSet) -> Result<(Arc<Store>, Opened)> {
        anyhow::ensure!(
            (1..=Semaphore::MAX_PERMITS.min(u32::MAX as usize)).contains(&cfg.memory_budget),
            "invalid header store memory budget"
        );
        std::fs::create_dir_all(cfg.root.join(STATE_DIR)).with_context(|| format!("making {}", cfg.root.display()))?;
        let lock = lock_store(&cfg.root)?;
        let _ = super::sync::raise_open_files(65_536);
        let servers = read_servers(&cfg.root)?;
        let mut opened = Opened::default();
        let mut groups = HashMap::new();
        let memory = Arc::new(Semaphore::new(cfg.memory_budget));
        let mut recovery = RecoveryOpen {
            memory: &memory,
            bytes: RECOVERY_OPEN_BYTES.min(cfg.memory_budget),
            logs: RECOVERY_OPEN_LOGS,
        };
        for entry in std::fs::read_dir(&cfg.root)? {
            let dir = entry?.path();
            let Some(dname) = dir.file_name().and_then(|n| n.to_str()).map(str::to_string) else { continue };
            if !dir.is_dir() || dname == STATE_DIR {
                continue;
            }
            let Some(name) = decode_name(&dname) else { continue };
            // nothing is deleted at open: what the keep set no longer holds goes once expiry is armed (after the
            // linker's first pass), soo a day the linker hasn't linked survives a restart
            let everything = KeepSet { live_from: i64::MIN / 2, ..KeepSet::default() };
            let group = open_group(&dir, &name, &everything, &mut opened, &mut recovery)?;
            groups.insert(name, group);
        }
        let shared = Arc::new(Shared {
            failed: Mutex::new(None),
            stopping: AtomicBool::new(false),
            synced_lock: Mutex::new(()),
            synced: Condvar::new(),
            wake_lock: Mutex::new(()),
            wake: Condvar::new(),
            memory,
            fetches: Arc::new(Semaphore::new(cfg.memory_budget.div_ceil(FETCH_HEADROOM))),
            memory_waiters: AtomicUsize::new(0),
            drain_requested: AtomicBool::new(false),
            dropped_unkept: AtomicU64::new(0),
            dropped_repeats: AtomicU64::new(0),
            blocks_committed: AtomicU64::new(0),
            inflight: Mutex::new(HashMap::new()),
        });
        let store = Arc::new(Store {
            cfg,
            groups: RwLock::new(groups),
            servers: Mutex::new(servers),
            keep: RwLock::new(keep),
            shared,
            seal_tx: Mutex::new(None),
            threads: Mutex::new(Vec::new()),
            tick_lock: Mutex::new(()),
            recording: RwLock::new(()),
            expiry_armed: AtomicBool::new(false),
            linker_pins: AtomicUsize::new(0),
            retention_gate: Mutex::new(()),
            linker_coverage: RwLock::new(None),
            coverage_cursor: Mutex::new(None),
            keep_source: Mutex::new(None),
            recent_claims: Mutex::new(BTreeMap::new()),
            _lock: lock,
        });
        store.start();
        Ok((store, opened))
    }

    fn start(self: &Arc<Self>) {
        // Queued slabs hold their byte permits through commit, so this channel does not need a second limit.
        let (seal_tx, seal_rx) = channel::<SealJob>();
        let (commit_tx, commit_rx) = sync_channel::<Pending>(64);
        let seal_rx = Arc::new(Mutex::new(seal_rx));
        let mut threads = Vec::new();
        for i in 0..self.cfg.sealers.max(1) {
            let rx = Arc::clone(&seal_rx);
            let tx = commit_tx.clone();
            let shared = Arc::clone(&self.shared);
            threads.push(
                std::thread::Builder::new()
                    .name(format!("nxv-sealer-{i}"))
                    .spawn(move || guarded("a sealer", &Arc::clone(&shared), move || sealer(rx, tx, shared)))
                    .expect("spawning a sealer"),
            );
        }
        drop(commit_tx);
        let shared = Arc::clone(&self.shared);
        threads.push(
            std::thread::Builder::new()
                .name("nxv-committer".into())
                .spawn(move || guarded("the committer", &Arc::clone(&shared), move || committer(commit_rx, shared)))
                .expect("spawning the committer"),
        );
        *self.seal_tx.lock().unwrap_or_else(|e| e.into_inner()) = Some(seal_tx);
        let me = Arc::clone(self);
        let shared = Arc::clone(&self.shared);
        threads.push(
            std::thread::Builder::new()
                .name("nxv-batcher".into())
                .spawn(move || guarded("the batcher", &shared, move || me.batcher()))
                .expect("spawning the batcher"),
        );
        *self.threads.lock().unwrap_or_else(|e| e.into_inner()) = threads;
    }

    pub fn failure(&self) -> Option<String> {
        self.shared.failure()
    }

    /// Where fresh keep sets come from from now on.
    pub fn set_keep_source(&self, f: KeepSource) {
        *self.keep_source.lock().unwrap_or_else(|e| e.into_inner()) = Some(f);
    }

    /// The keep set as of now: from the source when there is one (kept, with the days claimed since added), else the
    /// one set last.
    fn current_keep(&self) -> KeepSet {
        let source = self.keep_source.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let Some(f) = source else { return self.keep.read().unwrap_or_else(|e| e.into_inner()).clone() };
        let fresh = f();
        // the fresh claims, and those made here lately that the source may not have caught up with (they age out,
        // soo a day claimed once isn't kept for good)
        let mut recent = self.recent_claims.lock().unwrap_or_else(|e| e.into_inner());
        recent.retain(|_, at| at.elapsed() < RECENT_CLAIM);
        let claimed: BTreeSet<i64> = fresh.claimed.iter().chain(recent.keys()).copied().collect();
        drop(recent);
        let mut k = self.keep.write().unwrap_or_else(|e| e.into_inner());
        *k = KeepSet { claimed, ..fresh };
        k.clone()
    }

    /// A backfill day just claimed, kept at once (not at the next refresh): its rows are about to come.
    pub fn add_claimed(&self, day: i64) {
        self.recent_claims.lock().unwrap_or_else(|e| e.into_inner()).insert(day, Instant::now());
        self.keep.write().unwrap_or_else(|e| e.into_inner()).claimed.insert(day);
    }

    pub fn set_keep(&self, keep: KeepSet) {
        *self.keep.write().unwrap_or_else(|e| e.into_inner()) = keep;
    }

    /// The id of server `name`, added to the servers file (synced) the first time.
    pub fn server_id(&self, name: &str) -> Result<u16> {
        let mut s = self.servers.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(i) = s.iter().position(|n| n == name) {
            return Ok(i as u16 + 1);
        }
        anyhow::ensure!(s.len() < u16::MAX as usize - 1, "too many servers");
        anyhow::ensure!(!name.contains('\n'), "a server name with a line break");
        let path = self.cfg.root.join(STATE_DIR).join(SERVERS_FILE);
        let f = std::fs::OpenOptions::new().create(true).append(true).open(&path)?;
        let mut w = &f;
        std::io::Write::write_all(&mut w, format!("{name}\n").as_bytes())?;
        sync_all_durable(&[&f, &File::open(self.cfg.root.join(STATE_DIR))?])?;
        s.push(name.to_string());
        Ok(s.len() as u16)
    }

    fn group(&self, name: &str) -> Result<Arc<Group>> {
        if let Some(g) = self.groups.read().unwrap_or_else(|e| e.into_inner()).get(name) {
            return Ok(Arc::clone(g));
        }
        let mut gs = self.groups.write().unwrap_or_else(|e| e.into_inner());
        if let Some(g) = gs.get(name) {
            return Ok(Arc::clone(g));
        }
        let dir = self.cfg.root.join(encode_name(name));
        std::fs::create_dir_all(&dir).with_context(|| format!("making {}", dir.display()))?;
        // the new folder's name durable before any row of it counts as kept
        super::sync::sync_dir(&self.cfg.root)?;
        let g = Arc::new(new_group(dir, name, 1));
        gs.insert(name.to_string(), Arc::clone(&g));
        Ok(g)
    }

    /// A fetched slice of `group` from `server`: each row clamped and routed to its day's slab, rows of days not
    /// kept dropped (counted). The ticket says when the rows are durable.
    #[cfg(test)]
    pub fn record(&self, server: &str, group: &str, lines: &[Vec<u8>], clamp: Clamp) -> Result<Ticket> {
        let bytes = listing_charge(lines);
        anyhow::ensure!(bytes <= self.cfg.memory_budget, "test listing exceeds the header store memory budget");
        let permit = Arc::clone(&self.shared.memory).try_acquire_many_owned(bytes as u32)?;
        self.record_charged(server, group, lines, clamp, None, permit)
    }

    pub async fn wait_for_fetch(&self) -> Result<OwnedSemaphorePermit> {
        let fetch = Arc::clone(&self.shared.fetches).acquire_owned().await?;
        let headroom = FETCH_HEADROOM.min(self.cfg.memory_budget);
        drop(self.reserve_memory(headroom as u32).await?);
        Ok(fetch)
    }

    fn request_drain(&self) {
        let _guard = self.shared.wake_lock.lock().unwrap_or_else(|e| e.into_inner());
        self.shared.drain_requested.store(true, Ordering::Release);
        self.shared.wake.notify_one();
    }

    async fn reserve_memory(&self, bytes: u32) -> Result<OwnedSemaphorePermit> {
        match Arc::clone(&self.shared.memory).try_acquire_many_owned(bytes) {
            Ok(permit) => Ok(permit),
            Err(TryAcquireError::Closed) => bail!("the header store stopped"),
            Err(TryAcquireError::NoPermits) => {
                let _wait = crate::metrics::span("header_store.wait_memory");
                self.shared.memory_waiters.fetch_add(1, Ordering::AcqRel);
                let _waiting = MemoryWaiter(&self.shared.memory_waiters);
                self.request_drain();
                Ok(Arc::clone(&self.shared.memory).acquire_many_owned(bytes).await?)
            }
        }
    }

    pub async fn record_live(
        self: &Arc<Self>,
        server: &str,
        group: &str,
        lines: Vec<Vec<u8>>,
        clamp: Clamp,
        fetch: Option<Arc<OwnedSemaphorePermit>>,
    ) -> Result<(Vec<Vec<u8>>, Ticket)> {
        let budget = self.cfg.memory_budget;
        let charge = listing_charge(&lines);
        if charge <= budget {
            let permit = self.reserve_memory(charge as u32).await?;
            let (store, server, group) = (Arc::clone(self), server.to_owned(), group.to_owned());
            return tokio::task::spawn_blocking(move || {
                let _fetch = fetch;
                let ticket = store.record_charged(&server, &group, &lines, clamp, None, permit)?;
                Ok((lines, ticket))
            })
            .await?;
        }

        // Oversized listings keep the whole response's date bounds and enter the slab pipeline in budgeted batches.
        let mut batches = Vec::new();
        let (mut start, mut bytes) = (0, 0);
        for (i, line) in lines.iter().enumerate() {
            let charge = row_charge(line);
            anyhow::ensure!(charge <= budget, "overview row exceeds the header store memory budget");
            if bytes + charge > budget {
                batches.push((start..i, bytes));
                start = i;
                bytes = 0;
            }
            bytes += charge;
        }
        batches.push((start..lines.len(), bytes));
        let dates: Vec<_> = lines.iter().filter_map(|line| parse_row(line)?.1).collect();
        let bounds = Some((clamp.bounds(&dates), !dates.is_empty()));
        drop(dates);

        let lines = Arc::new(lines);
        let mut days = BTreeMap::new();
        let mut last = None;
        for (range, bytes) in batches {
            let permit = self.reserve_memory(bytes as u32).await?;
            let (store, server, group) = (Arc::clone(self), server.to_owned(), group.to_owned());
            let (batch, fetch) = (Arc::clone(&lines), fetch.clone());
            let mut ticket = tokio::task::spawn_blocking(move || {
                let _fetch = fetch;
                store.record_charged(&server, &group, &batch[range], clamp, bounds, permit)
            })
            .await??;
            for (day, count) in std::mem::take(&mut ticket.days) {
                *days.entry(day).or_default() += count;
            }
            last = Some(ticket);
        }
        let mut ticket = last.expect("an oversized listing has at least one batch");
        ticket.days = days.into_iter().collect();
        let lines = Arc::try_unwrap(lines).expect("all recording batches finished");
        Ok((lines, ticket))
    }

    fn record_charged(
        &self,
        server: &str,
        group: &str,
        lines: &[Vec<u8>],
        clamp: Clamp,
        bounds: Option<((i64, i64), bool)>,
        permit: OwnedSemaphorePermit,
    ) -> Result<Ticket> {
        let _recording = self.recording.read().unwrap_or_else(|e| e.into_inner());
        anyhow::ensure!(!self.shared.stopping.load(Ordering::Acquire), "the header store stopped");
        if let Some(e) = self.failure() {
            bail!("the header store stopped: {e}");
        }
        match self.record_inner(server, group, lines, clamp, bounds, permit) {
            Ok(t) => Ok(t),
            Err(e) => {
                // rows that couldnt be kept must not let a cursor move: the store stops, and the slice's wait fails
                let msg = format!("{e:#}");
                self.shared.fail(e);
                bail!("the header store stopped: {msg}")
            }
        }
    }

    fn record_inner(
        &self,
        server: &str,
        group: &str,
        lines: &[Vec<u8>],
        clamp: Clamp,
        bounds: Option<((i64, i64), bool)>,
        mut permit: OwnedSemaphorePermit,
    ) -> Result<Ticket> {
        let sid = self.server_id(server)?;
        let g = self.group(group)?;
        let parsed: Vec<_> = lines.iter().map(|line| parse_row(line)).collect();
        let ((lo, hi), dated) = bounds.unwrap_or_else(|| {
            let dates: Vec<_> = parsed.iter().filter_map(|p| p.and_then(|(_, d)| d)).collect();
            (clamp.bounds(&dates), !dates.is_empty())
        });
        // a read is on the slice's own day (its median): that day is kept however far behind the read is (a live
        // reader catching up after an outage, a slow group), soo its rows aren't refused; stray rows a day or more
        // from it still are
        let slice_day = ((lo + hi) / 2).div_euclid(DAY);
        if dated && !self.keep.read().unwrap_or_else(|e| e.into_inner()).keeps(slice_day) {
            self.add_claimed(slice_day);
        } else if dated {
            // refreshed while the read goes on
            if let Some(at) = self.recent_claims.lock().unwrap_or_else(|e| e.into_inner()).get_mut(&slice_day) {
                *at = Instant::now();
            }
        }
        let mut keep = self.keep.read().unwrap_or_else(|e| e.into_inner()).clone();
        let mut refreshed = false;
        let mut s = g.slabs.lock().unwrap_or_else(|e| e.into_inner());
        let mut days: BTreeMap<i64, usize> = BTreeMap::new();
        for (line, p) in lines.iter().zip(&parsed) {
            let Some((art, date)) = *p else { continue };
            let ts = date.unwrap_or((lo + hi) / 2).clamp(lo, hi);
            let day = ts.div_euclid(DAY);
            *days.entry(day).or_default() += 1;
            if !keep.keeps(day) {
                // the keep set held may be older than a claim or the linker's last look: ask for a fresh one once
                if !refreshed {
                    refreshed = true;
                    drop(s);
                    keep = self.current_keep();
                    s = g.slabs.lock().unwrap_or_else(|e| e.into_inner());
                }
                if !keep.keeps(day) {
                    self.shared.dropped_unkept.fetch_add(1, Ordering::Relaxed);
                    crate::metrics::names::HEADER_STORE_DROPPED.inc(&[("reason", "unkept")], 1);
                    continue;
                }
            }
            let slab = push_row(&g, &mut s, sid, day, art, ts, line, self.cfg.seal_rows)?;
            slab.charge(permit.split(row_charge(line)).expect("listing was charged before recording"));
        }
        let epoch = s.epoch + 1;
        drop(s);
        if self.shared.memory_waiters.load(Ordering::Acquire) > 0 {
            self.request_drain();
        }
        Ok(Ticket { group: g, epoch, shared: Arc::clone(&self.shared), days: days.into_iter().collect() })
    }

    /// A ticket for every row recorded for `group` so far. None when the group has none.
    pub fn sync_point(&self, group: &str) -> Option<Ticket> {
        let g = self.groups.read().unwrap_or_else(|e| e.into_inner()).get(group).cloned()?;
        let epoch = g.slabs.lock().unwrap_or_else(|e| e.into_inner()).epoch + 1;
        Some(Ticket { group: g, epoch, shared: Arc::clone(&self.shared), days: Vec::new() })
    }

    /// Asks for every slab of `days` to be sealed at the next tick (the linker's barrier before it links an hour).
    pub fn seal_days(&self, days: &[i64]) {
        for g in self.groups.read().unwrap_or_else(|e| e.into_inner()).values() {
            g.slabs.lock().unwrap_or_else(|e| e.into_inner()).seal_days.extend(days);
        }
    }

    /// A seal barrier: every row of `days` recorded before this call sealed and committed, waiting up to `timeout`. The
    /// slabs of those days open now (and any on their way to the committer) are the ones waited for; slabs opened
    /// after, for rows that come meanwhile, aren't, soo a day still being read doesn't hold the barrier up.
    pub fn flush_days(&self, days: &[i64], timeout: Duration) -> Result<()> {
        let mut waiting: HashSet<(String, u32)> = HashSet::new();
        for g in self.groups.read().unwrap_or_else(|e| e.into_inner()).values() {
            let mut s = g.slabs.lock().unwrap_or_else(|e| e.into_inner());
            for sl in s.open.values().chain(s.full.iter()) {
                if days.contains(&sl.day) {
                    waiting.insert((g.name.clone(), sl.slab_gen));
                }
            }
            s.seal_days.extend(days);
        }
        // and those already handed to the sealers: whatever day, they're few
        waiting.extend(self.shared.inflight.lock().unwrap_or_else(|e| e.into_inner()).keys().cloned());
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(e) = self.failure() {
                bail!("the header store stopped: {e}");
            }
            // the slabs first, each group's lock on its own, then the slabs in flight: a slab moves from a group's
            // slabs to `inflight` under the group's lock, soo looking in that order never misses one on its way, and no
            // lock is held while another is taken (the batcher takes a group's slabs, then `inflight`)
            let left: usize = {
                let groups: Vec<(String, Arc<Group>)> = {
                    let gs = self.groups.read().unwrap_or_else(|e| e.into_inner());
                    let names: HashSet<&String> = waiting.iter().map(|(n, _)| n).collect();
                    names.into_iter().filter_map(|n| gs.get(n).map(|g| (n.clone(), Arc::clone(g)))).collect()
                };
                let mut still: HashSet<(String, u32)> = HashSet::new();
                for (name, g) in &groups {
                    let s = g.slabs.lock().unwrap_or_else(|e| e.into_inner());
                    for sl in s.open.values().chain(s.full.iter()) {
                        if waiting.contains(&(name.clone(), sl.slab_gen)) {
                            still.insert((name.clone(), sl.slab_gen));
                        }
                    }
                }
                let inflight = self.shared.inflight.lock().unwrap_or_else(|e| e.into_inner());
                waiting.retain(|k| still.contains(k) || inflight.contains_key(k));
                waiting.len()
            };
            if left == 0 {
                return Ok(());
            }
            if Instant::now() >= deadline {
                bail!("{left} slabs of the days asked for weren't committed in time");
            }
            // the batcher may be asleep for its whole interval (tests): a tick now cuts the seals asked for
            if self.cfg.sync_every > Duration::from_secs(1) {
                self.tick()?;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Every segment there is: (group, day, blocks published).
    pub fn segments(&self) -> Vec<(String, i64, usize)> {
        let mut out = Vec::new();
        for (name, g) in self.groups.read().unwrap_or_else(|e| e.into_inner()).iter() {
            for (day, seg) in g.segments.read().unwrap_or_else(|e| e.into_inner()).iter() {
                out.push((name.clone(), *day, seg.block_count()));
            }
        }
        out.sort();
        out
    }

    /// A keyset page of the segment inventory. Unlike `segments`, the extra
    /// memory is proportional to `limit`, regardless of historical day count.
    /// An epoch must pin retention while inventory and source staging run.
    pub fn segment_page(&self, after: Option<(&str, i64)>, limit: usize) -> Vec<(String, i64, usize)> {
        if limit == 0 {
            return Vec::new();
        }
        let mut page: BTreeMap<(String, i64), usize> = BTreeMap::new();
        use std::ops::Bound::{Excluded, Unbounded};
        for (name, g) in self.groups.read().unwrap_or_else(|e| e.into_inner()).iter() {
            if after.is_some_and(|(group, _)| name.as_str() < group) {
                continue;
            }
            let lower = match after {
                Some((group, day)) if group == name.as_str() => Excluded(day),
                _ => Unbounded,
            };
            let segments = g.segments.read().unwrap_or_else(|e| e.into_inner());
            // At most one page from each group's ordered manifest. A long
            // history does not rescan all earlier days for every next page.
            for (day, seg) in segments.range((lower, Unbounded)).take(limit) {
                if page.len() == limit
                    && page.last_key_value().is_some_and(|(k, _)| (name.as_str(), *day) >= (k.0.as_str(), k.1))
                {
                    continue;
                }
                page.insert((name.clone(), *day), seg.block_count());
                if page.len() > limit {
                    page.pop_last();
                }
            }
        }
        page.into_iter().map(|((g, d), n)| (g, d, n)).collect()
    }

    /// Pin one segment's published prefix without copying its manifest.
    pub fn segment_snapshot(&self, group: &str, day: i64) -> Option<super::segment::Snapshot> {
        let g = self.groups.read().unwrap_or_else(|e| e.into_inner()).get(group).cloned()?;
        let seg = g.segments.read().unwrap_or_else(|e| e.into_inner()).get(&day).cloned()?;
        Some(seg.bounded_snapshot())
    }

    /// Days some slab still holds rows of (not yet sealed and committed).
    pub fn open_days(&self) -> BTreeSet<i64> {
        let mut out = BTreeSet::new();
        for g in self.groups.read().unwrap_or_else(|e| e.into_inner()).values() {
            let s = g.slabs.lock().unwrap_or_else(|e| e.into_inner());
            out.extend(s.open.values().chain(s.full.iter()).filter(|sl| !sl.is_empty()).map(|sl| sl.day));
        }
        out
    }

    /// A bounded page of still-open days for a native staging barrier.
    pub fn open_days_page(&self, after: Option<i64>, limit: usize) -> Vec<i64> {
        if limit == 0 {
            return Vec::new();
        }
        let mut days = BTreeSet::new();
        for g in self.groups.read().unwrap_or_else(|e| e.into_inner()).values() {
            let slabs = g.slabs.lock().unwrap_or_else(|e| e.into_inner());
            for slab in slabs.open.values().chain(slabs.full.iter()).filter(|s| !s.is_empty()) {
                if after.is_some_and(|d| slab.day <= d) {
                    continue;
                }
                days.insert(slab.day);
                if days.len() > limit {
                    days.pop_last();
                }
            }
        }
        for &day in self.shared.inflight.lock().unwrap_or_else(|e| e.into_inner()).values() {
            if after.is_some_and(|d| day <= d) {
                continue;
            }
            days.insert(day);
            if days.len() > limit {
                days.pop_last();
            }
        }
        days.into_iter().collect()
    }

    /// The published blocks of `group`'s segment of `day`, with the map they point into.
    pub fn day_blocks(&self, group: &str, day: i64) -> (Option<Arc<memmap2::Mmap>>, Vec<Entry>) {
        let Some(g) = self.groups.read().unwrap_or_else(|e| e.into_inner()).get(group).cloned() else {
            return (None, Vec::new());
        };
        let seg = g.segments.read().unwrap_or_else(|e| e.into_inner()).get(&day).cloned();
        seg.map_or((None, Vec::new()), |s| s.snapshot())
    }

    /// Where the store lives.
    pub fn root(&self) -> &Path {
        &self.cfg.root
    }

    /// One batcher tick, run by the batcher thread every `sync_every` (and by tests directly).
    pub fn tick(&self) -> Result<()> {
        let _one = self.tick_lock.lock().unwrap_or_else(|e| e.into_inner());
        self.shared.drain_requested.store(false, Ordering::Release);
        let pressure = self.shared.memory_waiters.load(Ordering::Acquire) > 0;
        let groups: Vec<Arc<Group>> = self.groups.read().unwrap_or_else(|e| e.into_inner()).values().cloned().collect();
        let mut writes: Vec<(Arc<File>, Vec<u8>)> = Vec::new();
        let mut dirs: Vec<PathBuf> = Vec::new();
        let mut synced: Vec<(Arc<Group>, u64)> = Vec::new();
        let mut to_seal: Vec<(Arc<Group>, Slab)> = Vec::new();
        for g in &groups {
            let mut s = g.slabs.lock().unwrap_or_else(|e| e.into_inner());
            // the slabs ready to seal leave now, with their last bytes in this tick's writes
            let seal_days = std::mem::take(&mut s.seal_days);
            let ready: Vec<(u16, i64)> = s
                .open
                .iter()
                .filter(|(_, sl)| {
                    pressure
                        || sl.len() >= self.cfg.seal_rows
                        || sl.last_row.elapsed() >= self.cfg.seal_idle
                        || seal_days.contains(&sl.day)
                })
                .map(|(k, _)| *k)
                .collect();
            let mut leaving: Vec<Slab> = std::mem::take(&mut s.full);
            for k in ready {
                if let Some(sl) = s.open.remove(&k) {
                    leaving.push(sl);
                }
            }
            for sl in s.open.values_mut().chain(leaving.iter_mut()) {
                if sl.has_pending() {
                    writes.push((Arc::clone(&sl.log), sl.take_pending()));
                }
            }
            if std::mem::take(&mut s.dir_dirty) {
                dirs.push(g.dir.clone());
            }
            s.epoch += 1;
            synced.push((Arc::clone(g), s.epoch));
            let leaving: Vec<Slab> = leaving.into_iter().filter(|sl| !sl.is_empty()).collect();
            {
                let mut inflight = self.shared.inflight.lock().unwrap_or_else(|e| e.into_inner());
                inflight.extend(leaving.iter().map(|sl| ((g.name.clone(), sl.slab_gen), sl.day)));
            }
            to_seal.extend(leaving.into_iter().map(|sl| (Arc::clone(g), sl)));
        }
        // outside the locks: write, then make it all durable with one full flush
        let result = (|| -> Result<()> {
            {
                let _span = crate::metrics::span("header_store.log_append");
                for (f, bytes) in &writes {
                    slab::append(f, bytes)?;
                }
            }
            let dir_files: Vec<File> = dirs.iter().map(File::open).collect::<std::io::Result<_>>()?;
            let mut files: Vec<&File> = writes.iter().map(|(f, _)| f.as_ref()).collect();
            files.extend(dir_files.iter());
            let _span = crate::metrics::span("header_store.log_sync");
            sync_all_durable(&files)?;
            Ok(())
        })();
        if let Err(e) = result {
            self.shared.fail(e);
            return Err(anyhow!("header store: log sync failed"));
        }
        drop(writes);
        for (g, epoch) in synced {
            g.synced_epoch.store(epoch, Ordering::Release);
        }
        {
            let _g = self.shared.synced_lock.lock().unwrap_or_else(|e| e.into_inner());
            self.shared.synced.notify_all();
        }
        if !to_seal.is_empty() {
            let tx = self.seal_tx.lock().unwrap_or_else(|e| e.into_inner()).clone();
            let Some(tx) = tx else { bail!("the store is closed") };
            for item in to_seal {
                tx.send(item).map_err(|_| anyhow!("the sealers stopped"))?;
            }
        }
        Ok(())
    }

    fn batcher(self: Arc<Self>) {
        let mut last_idle_close = Instant::now();
        loop {
            {
                let g = self.shared.wake_lock.lock().unwrap_or_else(|e| e.into_inner());
                let _ = self
                    .shared
                    .wake
                    .wait_timeout_while(g, self.cfg.sync_every, |_| {
                        !self.shared.stopping.load(Ordering::Acquire)
                            && !self.shared.drain_requested.load(Ordering::Acquire)
                    })
                    .unwrap_or_else(|e| e.into_inner());
            }
            let stopping = self.shared.stopping.load(Ordering::Acquire);
            if self.failure().is_some() {
                break;
            }
            if stopping {
                // a last tick seals everything open
                self.seal_all_now();
            }
            if self.tick().is_err() || stopping {
                break;
            }
            if last_idle_close.elapsed() >= Duration::from_secs(60) {
                last_idle_close = Instant::now();
                self.housekeeping();
            }
        }
        // the sealers and then the committer drain and end once the senders go
        self.seal_tx.lock().unwrap_or_else(|e| e.into_inner()).take();
    }

    fn seal_all_now(&self) {
        for g in self.groups.read().unwrap_or_else(|e| e.into_inner()).values() {
            let mut s = g.slabs.lock().unwrap_or_else(|e| e.into_inner());
            let days: Vec<i64> = s.open.values().map(|sl| sl.day).collect();
            s.seal_days.extend(days);
        }
    }

    /// Idle writer files closed, and segments of days the keep set no longer holds (and no slab still fills)
    /// expired.
    pub fn housekeeping(&self) {
        let coverage = self.linker_coverage.read().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(coverage) = coverage {
            self.covered_housekeeping(&coverage);
            return;
        }
        // armed first, then the keep set: arming comes after the keep set took the linker's unlinked days
        let armed = self.expiry_armed.load(Ordering::Acquire);
        let keep = self.current_keep();
        for g in self.groups.read().unwrap_or_else(|e| e.into_inner()).values() {
            // the slabs first, then those on their way to the committer (a slab moves from one to the other under the
            // group's lock, soo this order never misses one in transit): both count as still filling their days
            let open_days: BTreeSet<i64> = {
                let s = g.slabs.lock().unwrap_or_else(|e| e.into_inner());
                s.open.values().chain(s.full.iter()).map(|sl| sl.day).collect()
            };
            let inflight_days: BTreeSet<i64> =
                self.shared.inflight.lock().unwrap_or_else(|e| e.into_inner()).values().copied().collect();
            let _gate = self.retention_gate.lock().unwrap_or_else(|e| e.into_inner());
            let mut segs = g.segments.write().unwrap_or_else(|e| e.into_inner());
            let gone: Vec<i64> = segs
                .iter()
                .filter(|(day, seg)| {
                    armed
                        && !keep.keeps(**day)
                        && !open_days.contains(day)
                        && !inflight_days.contains(day)
                        && self.linker_pins.load(Ordering::Acquire) == 0
                        && self.expiry_armed.load(Ordering::Acquire)
                        && self.linker_coverage.read().unwrap_or_else(|e| e.into_inner()).is_none()
                        && seg.expire()
                })
                .map(|(d, _)| *d)
                .collect();
            for d in gone {
                segs.remove(&d);
            }
            for seg in segs.values() {
                seg.close_if_idle(self.cfg.writer_idle);
            }
        }
    }

    pub fn pin_linker_retention(self: &Arc<Self>) -> LinkerRetentionPin {
        let _gate = self.retention_gate.lock().unwrap_or_else(|e| e.into_inner());
        self.linker_pins.fetch_add(1, Ordering::AcqRel);
        LinkerRetentionPin { store: Arc::clone(self) }
    }

    /// Clearing the callback also disarms expiry; shutdown/failure must not
    /// fall back to day-only expiry while native work remains unpublished.
    pub fn set_linker_coverage(&self, coverage: Option<LinkerCoverage>) {
        let _gate = self.retention_gate.lock().unwrap_or_else(|e| e.into_inner());
        if coverage.is_none() {
            self.expiry_armed.store(false, Ordering::Release);
        }
        *self.linker_coverage.write().unwrap_or_else(|e| e.into_inner()) = coverage;
    }

    fn covered_housekeeping(&self, coverage: &LinkerCoverage) {
        let after = self.coverage_cursor.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let page = self.segment_page(after.as_ref().map(|(g, d)| (g.as_str(), *d)), 64);
        *self.coverage_cursor.lock().unwrap_or_else(|e| e.into_inner()) = page.last().map(|(g, d, _)| (g.clone(), *d));
        for (name, day, _) in page {
            let Some(snapshot) = self.segment_snapshot(&name, day) else { continue };
            snapshot.close_if_idle(self.cfg.writer_idle);
            if !self.expiry_armed.load(Ordering::Acquire)
                || self.linker_pins.load(Ordering::Acquire) > 0
                || self.current_keep().keeps(day)
            {
                continue;
            }
            // No locks are held across PostgreSQL: the owner may be staging a
            // source page. A stale positive answer is rechecked at deletion.
            if !coverage(&name, day, snapshot.identity, snapshot.count).unwrap_or(false)
                || self.current_keep().keeps(day)
            {
                continue;
            }
            let Some(group) = self.groups.read().unwrap_or_else(|e| e.into_inner()).get(&name).cloned() else {
                continue;
            };
            let slabs = group.slabs.lock().unwrap_or_else(|e| e.into_inner());
            if slabs.open.values().chain(slabs.full.iter()).any(|s| s.day == day) {
                continue;
            }
            let inflight = self.shared.inflight.lock().unwrap_or_else(|e| e.into_inner());
            if inflight.values().any(|&d| d == day) {
                continue;
            }
            let _gate = self.retention_gate.lock().unwrap_or_else(|e| e.into_inner());
            if self.linker_pins.load(Ordering::Acquire) > 0 || !self.expiry_armed.load(Ordering::Acquire) {
                continue;
            }
            let mut segments = group.segments.write().unwrap_or_else(|e| e.into_inner());
            if let Some(seg) = segments.get(&day) {
                seg.close_if_idle(self.cfg.writer_idle);
                if seg.expire_covered(snapshot.identity, snapshot.count) {
                    segments.remove(&day);
                }
            }
        }
    }

    /// Lets a test reopen the store after "crashing" it (forgetting it with its lock held).
    #[cfg(test)]
    fn unlock_for_test(&self) {
        use std::os::fd::AsRawFd;
        // SAFETY: flock on a descriptor this store owns
        unsafe { libc::flock(self._lock.as_raw_fd(), libc::LOCK_UN) };
    }

    /// Lets expiry run (the linker calls it once it has finished a pass).
    pub fn arm_expiry(&self) {
        self.expiry_armed.store(true, Ordering::Release);
    }

    /// Seals and commits everything, then stops the threads. What `record` got after this isn't kept.
    pub fn close(&self) -> Result<()> {
        self.shared.memory.close();
        self.shared.fetches.close();
        {
            let _recording = self.recording.write().unwrap_or_else(|e| e.into_inner());
            self.shared.stopping.store(true, Ordering::Release);
        }
        {
            let _g = self.shared.wake_lock.lock().unwrap_or_else(|e| e.into_inner());
            self.shared.wake.notify_all();
        }
        let threads = std::mem::take(&mut *self.threads.lock().unwrap_or_else(|e| e.into_inner()));
        for t in threads {
            let _ = t.join();
        }
        match self.failure() {
            Some(e) => bail!("the header store stopped: {e}"),
            None => Ok(()),
        }
    }

    /// Every committed block of `group` whose time range meets `from..=to`: (day, entry) with the segment's map.
    pub fn blocks(&self, group: &str, from: i64, to: i64) -> Vec<(Arc<memmap2::Mmap>, Entry)> {
        let Some(g) = self.groups.read().unwrap_or_else(|e| e.into_inner()).get(group).cloned() else {
            return Vec::new();
        };
        let segs: Vec<Arc<Segment>> = g
            .segments
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .range(from.div_euclid(DAY)..=to.div_euclid(DAY))
            .map(|(_, s)| Arc::clone(s))
            .collect();
        let mut out = Vec::new();
        for seg in segs {
            let (map, entries) = seg.snapshot();
            let Some(map) = map else { continue };
            for e in entries.into_iter().filter(|e| e.min_ts <= to && e.max_ts >= from) {
                out.push((Arc::clone(&map), e));
            }
        }
        out
    }

    /// The groups the store has, sorted (soo a pass over them goes the same way every run).
    pub fn group_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.groups.read().unwrap_or_else(|e| e.into_inner()).keys().cloned().collect();
        names.sort();
        names
    }

    /// (rows refused because their day isn't kept, repeats dropped at seal, blocks committed)
    pub fn counters(&self) -> (u64, u64, u64) {
        (
            self.shared.dropped_unkept.load(Ordering::Relaxed),
            self.shared.dropped_repeats.load(Ordering::Relaxed),
            self.shared.blocks_committed.load(Ordering::Relaxed),
        )
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        self.shared.stopping.store(true, Ordering::Release);
    }
}

fn new_group(dir: PathBuf, name: &str, next_gen: u32) -> Group {
    Group {
        name: name.to_string(),
        dir,
        slabs: Mutex::new(Slabs {
            open: HashMap::new(),
            full: Vec::new(),
            epoch: 0,
            dir_dirty: false,
            seal_days: BTreeSet::new(),
        }),
        segments: RwLock::new(BTreeMap::new()),
        next_gen: AtomicU32::new(next_gen),
        synced_epoch: AtomicU64::new(0),
    }
}

/// A row into the slab of (server, day), a new slab (and log) when there's none or it's full.
#[allow(clippy::too_many_arguments)]
fn push_row<'a>(
    g: &Group,
    s: &'a mut Slabs,
    sid: u16,
    day: i64,
    art: u64,
    ts: i64,
    line: &[u8],
    seal_rows: usize,
) -> Result<&'a mut Slab> {
    let key = (sid, day);
    if s.open.get(&key).is_some_and(|sl| sl.len() >= seal_rows || sl.bytes() >= SLAB_MAX_BYTES)
        && let Some(full) = s.open.remove(&key)
    {
        s.full.push(full);
    }
    let slab = match s.open.entry(key) {
        std::collections::hash_map::Entry::Occupied(o) => o.into_mut(),
        std::collections::hash_map::Entry::Vacant(v) => {
            let slab_gen = g.next_gen.fetch_add(1, Ordering::Relaxed);
            s.dir_dirty = true;
            v.insert(Slab::create(&g.dir, sid, day, slab_gen)?)
        }
    };
    slab.push(art, ts, line);
    Ok(slab)
}

struct RecoveryOpen<'a> {
    memory: &'a Arc<Semaphore>,
    bytes: usize,
    logs: usize,
}

/// A group at open: kept segments healed, others deleted; its logs deleted or replayed.
fn open_group(
    dir: &Path,
    name: &str,
    keep: &KeepSet,
    opened: &mut Opened,
    recovery: &mut RecoveryOpen<'_>,
) -> Result<Arc<Group>> {
    let mut segments = BTreeMap::new();
    let mut logs: Vec<(PathBuf, u16, i64, u32)> = Vec::new();
    let mut max_gen = 0u32;
    for entry in std::fs::read_dir(dir)? {
        let p = entry?.path();
        let Some(fname) = p.file_name().and_then(|n| n.to_str()).map(str::to_string) else { continue };
        if let Some(stem) = fname.strip_suffix(".nxv")
            && let Some(day) = parse_day_name(stem)
        {
            if keep.keeps(day) {
                let (seg, healed) = Segment::open(dir, day)?;
                opened.segments += 1;
                opened.blocks += healed.blocks;
                max_gen = max_gen.max(seg.highest_slab_gen());
                segments.insert(day, seg);
            } else {
                let _ = std::fs::remove_file(&p);
                let _ = std::fs::remove_file(dir.join(format!("{stem}.nxvi")));
                opened.expired_segments += 1;
            }
        } else if let Some((server, day, slab_gen)) = slab::parse_log_name(&fname) {
            max_gen = max_gen.max(slab_gen);
            logs.push((p, server, day, slab_gen));
        }
    }
    let next_gen = max_gen.checked_add(1).context("header store slab generations exhausted")?;
    let g = Arc::new(new_group(dir.to_path_buf(), name, next_gen));
    *g.segments.write().unwrap_or_else(|e| e.into_inner()) = segments;
    logs.sort_by_key(|l| l.3);
    {
        let mut s = g.slabs.lock().unwrap_or_else(|e| e.into_inner());
        let mut synced_days = HashSet::new();
        for (path, server, day, slab_gen) in logs {
            if !keep.keeps(day) {
                let _ = std::fs::remove_file(&path);
                opened.logs_unkept += 1;
                continue;
            }
            let segment = g.segments.read().unwrap_or_else(|e| e.into_inner()).get(&day).cloned();
            if let Some(segment) = segment.filter(|seg| seg.is_committed(server, slab_gen)) {
                if synced_days.insert(day) {
                    // A previous process may have stopped before flushing the recovered block or its name.
                    sync_all_durable(&[
                        &File::open(&segment.arena_path)?,
                        &File::open(&segment.index_path)?,
                        &File::open(dir)?,
                    ])?;
                }
                let _ = std::fs::remove_file(&path);
                opened.logs_already_committed += 1;
                continue;
            }
            let file = File::open(&path).with_context(|| format!("opening {}", path.display()))?;
            let len = file.metadata()?.len();
            anyhow::ensure!(len <= MAX_RECOVERY_LOG_BYTES, "slab log is too large: {}", path.display());
            let mut bytes = Vec::with_capacity(len as usize);
            file.take(MAX_RECOVERY_LOG_BYTES + 1)
                .read_to_end(&mut bytes)
                .with_context(|| format!("reading {}", path.display()))?;
            anyhow::ensure!(bytes.len() as u64 <= MAX_RECOVERY_LOG_BYTES, "slab log is too large: {}", path.display());
            let mut records = slab::records(&bytes);
            let mut rows = Vec::new();
            let mut charge = 0;
            for (ts, line) in records.by_ref() {
                if let Some(art) = slab::art_of(line) {
                    anyhow::ensure!(rows.len() < MAX_BLOCK_ROWS, "too many rows in {}", path.display());
                    rows.push((art, ts, line));
                    charge += row_charge(line);
                }
            }
            opened.rows_replayed += rows.len();
            opened.logs_replayed += 1;
            if rows.is_empty() {
                std::fs::remove_file(&path)?;
            } else if recovery.logs > 0 && charge <= recovery.bytes && !s.open.contains_key(&(server, day)) {
                let permit = Arc::clone(recovery.memory).try_acquire_many_owned(charge as u32)?;
                let sl = Slab::reopen(server, day, slab_gen, path, records.valid_len(), rows, permit)?;
                s.open.insert((server, day), sl);
                recovery.bytes -= charge;
                recovery.logs -= 1;
            } else {
                recover_sealed_log(&g, path, server, day, slab_gen, rows, opened)?;
            }
        }
    }
    Ok(g)
}

fn recover_sealed_log(
    group: &Arc<Group>,
    path: PathBuf,
    server: u16,
    day: i64,
    slab_gen: u32,
    mut rows: Vec<(u64, i64, &[u8])>,
    opened: &mut Opened,
) -> Result<()> {
    rows.sort_by_key(|(art, ..)| *art);
    rows.dedup_by_key(|(art, ..)| *art);
    // Keep the original generation so recovery recognizes a committed block even if its log survived.
    let lines: Vec<_> = rows.into_iter().map(|(_, _, line)| line).collect();
    let block = NxvBlockBuilder::build_hot_block_meta(&lines, BlockMeta { server_id: server, slab_gen })?;
    let existed = group.segments.read().unwrap_or_else(|e| e.into_inner()).contains_key(&day);
    opened.blocks += commit(&[Pending { group: Arc::clone(group), day, slab_gen, block, log: path, _memory: None }])?;
    opened.segments += usize::from(!existed);
    // Sequential startup recovery can touch many segments before housekeeping first runs.
    group.segments.read().unwrap_or_else(|e| e.into_inner())[&day].close_if_idle(Duration::ZERO);
    Ok(())
}

fn read_servers(root: &Path) -> Result<Vec<String>> {
    let path = root.join(STATE_DIR).join(SERVERS_FILE);
    match std::fs::read_to_string(&path) {
        // a torn last name (no newline) is dropped: it was never handed out
        Ok(s) => Ok(s.split_inclusive('\n').filter_map(|l| l.strip_suffix('\n')).map(str::to_string).collect()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

fn sealer(rx: Arc<Mutex<Receiver<SealJob>>>, tx: SyncSender<Pending>, shared: Arc<Shared>) {
    loop {
        let next = rx.lock().unwrap_or_else(|e| e.into_inner()).recv();
        let Ok((group, slab)) = next else { return };
        if shared.failure().is_some() {
            continue;
        }
        let _span = crate::metrics::span("header_store.seal");
        let Sealed { server, day, slab_gen, log_path, lines, dropped_repeats, memory } = slab.seal();
        shared.dropped_repeats.fetch_add(dropped_repeats as u64, Ordering::Relaxed);
        if dropped_repeats > 0 {
            crate::metrics::names::HEADER_STORE_DROPPED.inc(&[("reason", "repeat")], dropped_repeats as u64);
        }
        let refs: Vec<&[u8]> = lines.iter().map(|l| l.as_slice()).collect();
        let block = NxvBlockBuilder::build_hot_block_meta(&refs, BlockMeta { server_id: server, slab_gen });
        drop(refs);
        drop(lines);
        match block {
            Ok(block) => {
                if tx.send(Pending { group, day, slab_gen, block, log: log_path, _memory: memory }).is_err() {
                    return;
                }
            }
            Err(e) => shared.fail(e.context(format!("building a block of {}", group.name))),
        }
    }
}

/// Runs a store thread; a panic in it stops the store (fail-stop) rather than leave slabs in flight or tickets
/// waiting for good.
fn guarded(what: &str, shared: &Arc<Shared>, f: impl FnOnce()) {
    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).is_err() {
        shared.fail(anyhow!("{what} panicked"));
    }
}

/// Blocks the committer takes in one batch at most.
const COMMIT_BATCH: usize = 64;

fn committer(rx: Receiver<Pending>, shared: Arc<Shared>) {
    while let Ok(first) = rx.recv() {
        let mut batch = vec![first];
        while batch.len() < COMMIT_BATCH
            && let Ok(p) = rx.try_recv()
        {
            batch.push(p);
        }
        if shared.failure().is_some() {
            continue;
        }
        match commit(&batch) {
            Ok(n) => {
                shared.blocks_committed.fetch_add(n as u64, Ordering::Relaxed);
                for p in &batch {
                    shared.landed(&p.group.name, p.slab_gen);
                }
            }
            Err(e) => shared.fail(e),
        }
    }
}

/// One batch: reserve, write, sync, write entries, sync, publish, delete logs. Returns blocks committed.
fn commit(batch: &[Pending]) -> Result<usize> {
    let _span = crate::metrics::span("header_store.commit");
    let mut reserved: Vec<(Reserved, &Pending)> = Vec::with_capacity(batch.len());
    let mut new_dirs: Vec<PathBuf> = Vec::new();
    for p in batch {
        let existed = p.group.segments.read().unwrap_or_else(|e| e.into_inner()).contains_key(&p.day);
        let seg = p.group.segment(p.day)?;
        if !existed {
            new_dirs.push(p.group.dir.clone());
        }
        match seg.reserve(&p.block)? {
            Some(r) => reserved.push((r, p)),
            // its day expired while the slab was on its way, which housekeeping is meant to prevent: stop (the log stays,
            // and the next start replays it) rather than drop rows whose cursor already moved
            None => bail!("the segment of day {} expired with a slab of it on its way", p.day),
        }
    }
    {
        let _span = crate::metrics::span("header_store.block_append");
        for (r, p) in &reserved {
            r.arena.write_all_at(&p.block, r.entry.offset)?;
        }
    }
    let dir_files: Vec<File> = new_dirs.iter().map(File::open).collect::<std::io::Result<_>>()?;
    let mut files: Vec<&File> = reserved.iter().map(|(r, _)| r.arena.as_ref()).collect();
    files.extend(dir_files.iter());
    {
        let _span = crate::metrics::span("header_store.block_sync");
        sync_all_durable(&files)?;
    }
    for (r, _) in &reserved {
        r.index.write_all_at(&r.entry.to_bytes(), r.entry.seq as u64 * ENTRY_LEN as u64)?;
    }
    let idx: Vec<&File> = reserved.iter().map(|(r, _)| r.index.as_ref()).collect();
    {
        let _span = crate::metrics::span("header_store.manifest_sync");
        sync_all_durable(&idx)?;
    }
    for (r, p) in &reserved {
        r.seg.publish(r)?;
        let _ = std::fs::remove_file(&p.log);
    }
    // the logs' names gone durably is not needed: a log left behind is found committed at the next start
    Ok(reserved.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day_of(s: &str) -> i64 {
        parse_day_name(s).unwrap()
    }

    fn line(art: u64, poster: &str, date: &str) -> Vec<u8> {
        format!(
            "{art}\t[1/9] - \"x.part{:02}.rar\" yEnc (1/50)\t{poster}\t{date}\t<m{art}@x>\t\t700000\t5000",
            art % 90
        )
        .into_bytes()
    }

    fn cfg(root: &Path) -> StoreConfig {
        StoreConfig {
            sync_every: Duration::from_secs(3600),
            seal_idle: Duration::from_secs(3600),
            sealers: 1,
            ..StoreConfig::new(root)
        }
    }

    fn keep_all() -> KeepSet {
        KeepSet { live_from: i64::MIN / 2, ..KeepSet::default() }
    }

    const NOV8: &str = "Sun, 08 Nov 2026 10:00:00 +0000";
    const NOW: i64 = 1_794_150_000; // 2026-11-08 later that day

    /// Waits until `n` blocks are committed (the committer runs on its own thread).
    fn wait_blocks(store: &Store, n: u64) {
        let t = Instant::now();
        while store.counters().2 < n {
            assert!(t.elapsed() < Duration::from_secs(20), "blocks not committed: {:?}", store.counters());
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn fetch_waits_for_header_storage_capacity() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = StoreConfig { memory_budget: 8192, ..cfg(dir.path()) };
        let (store, _) = Store::open(cfg, keep_all()).unwrap();
        let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
        rt.block_on(async {
            let held = store.reserve_memory(8192).await.unwrap();
            let mut fetch = Box::pin(store.wait_for_fetch());
            assert!(tokio::time::timeout(Duration::from_millis(30), &mut fetch).await.is_err());
            drop(held);
            drop(tokio::time::timeout(Duration::from_secs(2), fetch).await.unwrap().unwrap());
        });
        store.close().unwrap();
    }

    #[test]
    fn fetch_admission_stays_held_while_the_response_is_in_flight() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = StoreConfig { memory_budget: 8192, ..cfg(dir.path()) };
        let (store, _) = Store::open(cfg, keep_all()).unwrap();
        let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
        rt.block_on(async {
            let mut next = Box::pin(store.wait_for_fetch());
            {
                let _first = store.wait_for_fetch().await.unwrap();
                assert!(tokio::time::timeout(Duration::from_millis(30), &mut next).await.is_err());
            }
            drop(tokio::time::timeout(Duration::from_secs(2), next).await.unwrap().unwrap());
        });
        store.close().unwrap();
    }

    #[test]
    fn recovered_log_keeps_its_generation_and_accepts_rows_after_a_torn_tail() {
        let dir = tempfile::tempdir().unwrap();
        let group = dir.path().join("g");
        std::fs::create_dir(&group).unwrap();
        let day = day_of("2026-11-08");
        let path = group.join(slab::log_name(1, day, 17));
        std::fs::create_dir(dir.path().join(STATE_DIR)).unwrap();
        std::fs::write(dir.path().join(STATE_DIR).join(SERVERS_FILE), "s\n").unwrap();
        let mut bytes = Vec::new();
        slab::frame(day * DAY, &line(1, "p@x", NOV8), &mut bytes);
        bytes.extend_from_slice(b"torn");
        std::fs::write(&path, &bytes).unwrap();

        let (store, opened) = Store::open(cfg(dir.path()), keep_all()).unwrap();
        assert_eq!(opened.rows_replayed, 1);
        assert!(store.shared.memory.available_permits() < store.cfg.memory_budget);
        store.record("s", "g", &[line(2, "p@x", NOV8)], Clamp::Live { now: NOW }).unwrap();
        store.tick().unwrap();
        let appended = std::fs::read(&path).unwrap();
        assert_eq!(slab::unframe(&appended).len(), 2);
        store.close().unwrap();
        let (_, entries) = store.day_blocks("g", day);
        assert_eq!((entries.len(), entries[0].rows, entries[0].slab_gen), (1, 2, 17));
    }

    #[test]
    fn a_listing_larger_than_the_intake_budget_is_recorded_in_batches() {
        let dir = tempfile::tempdir().unwrap();
        let lines = (1..=3).map(|art| line(art, "p@x", NOV8)).collect::<Vec<_>>();
        let cfg = StoreConfig { memory_budget: row_charge(&lines[0]) * 2, ..cfg(dir.path()) };
        let (store, _) = Store::open(cfg, keep_all()).unwrap();
        let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
        let (_, ticket) = rt.block_on(async {
            tokio::time::timeout(
                Duration::from_secs(5),
                store.record_live("s", "g", lines, Clamp::Live { now: NOW }, None),
            )
            .await
            .unwrap()
            .unwrap()
        });
        assert_eq!(ticket.days, [(day_of("2026-11-08"), 3)]);
        store.close().unwrap();
        ticket.wait(Duration::ZERO).unwrap();
        assert_eq!(store.day_blocks("g", day_of("2026-11-08")).1.iter().map(|e| e.rows).sum::<u32>(), 3);
    }

    #[test]
    fn storage_pressure_seals_partial_slabs_and_returns_capacity_after_commit() {
        let dir = tempfile::tempdir().unwrap();
        let lines = vec![line(1, "p@x", NOV8)];
        let budget = row_charge(&lines[0]) * 2;
        let cfg = StoreConfig { memory_budget: budget, ..cfg(dir.path()) };
        let (store, _) = Store::open(cfg, keep_all()).unwrap();
        let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
        rt.block_on(async {
            store.record_live("s", "g", lines, Clamp::Live { now: NOW }, None).await.unwrap();
            // The log becomes durable during a normal tick, but its partial slab still holds capacity.
            store.tick().unwrap();
            assert_eq!(store.counters().2, 0);
            assert_eq!(store.shared.memory.available_permits(), budget / 2);
            // Capacity pressure seals it without waiting for the normal idle timer.
            drop(tokio::time::timeout(Duration::from_secs(5), store.wait_for_fetch()).await.unwrap().unwrap());
            assert_eq!(store.counters().2, 1);
            assert_eq!(store.shared.memory.available_permits(), budget);
        });
        store.close().unwrap();
    }

    #[test]
    fn a_second_open_of_the_same_store_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let (store, _) = Store::open(cfg(dir.path()), keep_all()).unwrap();
        assert!(Store::open(cfg(dir.path()), keep_all()).is_err());
        store.close().unwrap();
        drop(store);
        let (again, _) = Store::open(cfg(dir.path()), keep_all()).unwrap();
        again.close().unwrap();
    }

    #[test]
    fn native_expiry_requires_coverage_and_respects_inventory_pin() {
        let dir = tempfile::tempdir().unwrap();
        let (store, _) = Store::open(cfg(dir.path()), keep_all()).unwrap();
        store
            .record(
                "server.example",
                "group.example",
                &[line(1, "poster@example.invalid", NOV8)],
                Clamp::Live { now: NOW },
            )
            .unwrap();
        store.close().unwrap();
        let day = day_of("2026-11-08");
        let status = Arc::new(AtomicUsize::new(0));
        let shared = Arc::clone(&status);
        store.set_linker_coverage(Some(Arc::new(move |_, _, _, _| match shared.load(Ordering::Acquire) {
            0 => Err(anyhow!("synthetic coverage outage")),
            1 => Ok(false),
            _ => Ok(true),
        })));
        store.set_keep(KeepSet { live_from: day + 10, ..KeepSet::default() });
        store.arm_expiry();
        let sweep = || {
            for _ in 0..3 {
                store.housekeeping();
            }
        };
        sweep();
        assert!(store.segment_snapshot("group.example", day).is_some());
        status.store(1, Ordering::Release);
        sweep();
        assert!(store.segment_snapshot("group.example", day).is_some());
        let pin = store.pin_linker_retention();
        status.store(2, Ordering::Release);
        sweep();
        assert!(store.segment_snapshot("group.example", day).is_some());
        drop(pin);
        sweep();
        assert!(store.segment_snapshot("group.example", day).is_none());
        store.set_linker_coverage(None);
        assert!(!store.expiry_armed.load(Ordering::Acquire));
    }

    #[test]
    fn bounded_segment_inventory_pages_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let (store, _) = Store::open(cfg(dir.path()), keep_all()).unwrap();
        for group in ["third.example", "first.example", "second.example"] {
            store
                .record("server.example", group, &[line(1, "poster@example.invalid", NOV8)], Clamp::Live { now: NOW })
                .unwrap();
        }
        store.close().unwrap();
        let expected = store.segments();
        let mut actual = Vec::new();
        let mut after: Option<(String, i64)> = None;
        loop {
            let page = store.segment_page(after.as_ref().map(|(g, d)| (g.as_str(), *d)), 1);
            assert!(page.len() <= 1);
            let Some((g, d, _)) = page.first() else { break };
            after = Some((g.clone(), *d));
            actual.extend(page);
        }
        assert_eq!(actual, expected);
        assert!(store.segment_page(None, 0).is_empty());
    }

    #[test]
    fn names_encode_and_decode() {
        for n in ["alt.binaries.x", "a b/c", "", ".hidden", "_x", "ü"] {
            assert_eq!(decode_name(&encode_name(n)).as_deref(), Some(n), "{n}");
        }
        assert_eq!(encode_name("alt.binaries.x"), "alt.binaries.x");
    }

    #[test]
    fn clamping_uses_the_slice_median_or_the_reads_context() {
        let now = NOW;
        let dates: Vec<i64> = (0..20).map(|k| now - 5 * DAY + k).collect();
        // a live reader catching up five days late keeps its rows' own times
        let (lo, hi) = Clamp::Live { now }.bounds(&dates);
        assert!(lo <= now - 5 * DAY && hi >= now - 5 * DAY + 19);
        // a tiny slice falls back to the context
        let (lo, hi) = Clamp::Live { now }.bounds(&dates[..3]);
        assert_eq!((lo, hi), (now - 2 * DAY, now + 3600));
        let day = day_of("2026-10-01");
        let (lo, hi) = Clamp::Backfill { day, now }.bounds(&[]);
        assert_eq!((lo, hi), (day * DAY - DAY, day * DAY + 2 * DAY - 1));
        // an unknown read trusts the median of even a few dates (a small backfill slice weeks back stays on its day)
        let (lo, hi) = Clamp::Unknown { now }.bounds(&dates[..3]);
        assert!(lo <= now - 5 * DAY && hi >= now - 5 * DAY + 2);
        assert_eq!(Clamp::Unknown { now }.bounds(&[]), (now - 2 * DAY, now + 3600));
        // never past an hour from now
        let future: Vec<i64> = (0..20).map(|k| now + 10 * DAY + k).collect();
        assert_eq!(Clamp::Live { now }.bounds(&future).1, now + 3600);
    }

    #[test]
    fn the_keep_set_holds_live_claimed_and_unlinked_days_with_their_neighbours() {
        let k = KeepSet { live_from: 100, claimed: [50].into(), unlinked: [70].into() };
        assert!(k.keeps(100) && k.keeps(101));
        assert!(k.keeps(49) && k.keeps(50) && k.keeps(51) && !k.keeps(52));
        assert!(k.keeps(69) && k.keeps(71) && !k.keeps(72));
        assert!(!k.keeps(99));
    }

    #[test]
    fn rows_become_durable_then_sealed_committed_and_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let (store, opened) = Store::open(cfg(dir.path()), keep_all()).unwrap();
        assert_eq!(opened, Opened::default());
        let lines: Vec<Vec<u8>> = (0..40).map(|k| line(1000 + k, &format!("p{}@x", k % 3), NOV8)).collect();
        let t = store.record("news.example", "alt.binaries.test", &lines, Clamp::Live { now: NOW }).unwrap();
        assert!(t.wait(Duration::from_millis(10)).is_err(), "not synced before a tick");
        store.tick().unwrap();
        t.wait(Duration::from_secs(1)).unwrap();
        // the log holds every row now
        let gdir = dir.path().join("alt.binaries.test");
        let log = std::fs::read_dir(&gdir)
            .unwrap()
            .flatten()
            .find(|e| e.file_name().to_string_lossy().starts_with("slab-"))
            .unwrap();
        assert_eq!(slab::unframe(&std::fs::read(log.path()).unwrap()).len(), 40);
        store.seal_days(&[day_of("2026-11-08")]);
        store.tick().unwrap();
        wait_blocks(&store, 1);
        let blocks = store.blocks("alt.binaries.test", NOW - DAY, NOW);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].1.rows, 40);
        assert_eq!((blocks[0].1.server, blocks[0].1.posters), (1, 3));
        assert!(!log.path().exists(), "the log goes once its block is committed");
        store.close().unwrap();
    }

    #[test]
    fn rows_of_days_not_kept_are_dropped_and_counted() {
        let dir = tempfile::tempdir().unwrap();
        let keep = KeepSet { live_from: day_of("2026-11-08"), ..KeepSet::default() };
        let (store, _) = Store::open(cfg(dir.path()), keep).unwrap();
        // a slice of today with a few rows dated a week before it: clamped to a day before the median, a day nothing
        // keeps, soo dropped and counted (the slice's own day is kept)
        let mut lines: Vec<Vec<u8>> = (0..20).map(|k| line(k + 1, "p@x", NOV8)).collect();
        lines.extend((0..3).map(|k| line(100 + k, "p@x", "Sun, 01 Nov 2026 10:00:00 +0000")));
        store.record("s", "g", &lines, Clamp::Live { now: NOW }).unwrap();
        assert_eq!(store.counters().0, 3, "the strays dropped");
        store.close().unwrap();
    }

    #[test]
    fn a_read_behind_the_live_window_keeps_its_own_day_and_claims_age_out() {
        let dir = tempfile::tempdir().unwrap();
        let today = day_of("2026-11-08");
        let (store, _) = Store::open(cfg(dir.path()), KeepSet { live_from: today, ..KeepSet::default() }).unwrap();
        store.set_keep_source(Arc::new(move || KeepSet { live_from: today, ..KeepSet::default() }));
        // a live reader five days behind (after an outage): its slice's day isn't in the live window, but it's read
        let behind: Vec<Vec<u8>> = (0..20).map(|k| line(k + 1, "p@x", "Tue, 03 Nov 2026 10:00:00 +0000")).collect();
        store.record("s", "g", &behind, Clamp::Live { now: NOW }).unwrap();
        assert_eq!(store.counters().0, 0, "kept: a read is on that day");
        assert!(store.current_keep().keeps(day_of("2026-11-03")));
        // once the claim is older than RECENT_CLAIM and the source doesn't hold the day, it goes
        let old = Instant::now().checked_sub(RECENT_CLAIM + Duration::from_secs(1)).unwrap();
        store.recent_claims.lock().unwrap().insert(day_of("2026-11-03"), old);
        assert!(!store.current_keep().keeps(day_of("2026-11-03")), "a claim ages out");
        store.close().unwrap();
    }

    #[test]
    fn recovery_over_budget_commits_original_logs_before_starting_workers() {
        let dir = tempfile::tempdir().unwrap();
        let day = day_of("2026-11-08");
        let first = dir.path().join("g");
        let second = dir.path().join("h");
        let ts = day * DAY;
        let mut logs = Vec::new();
        for (group, generation) in [(&first, 1), (&second, 2)] {
            std::fs::create_dir(group).unwrap();
            let path = group.join(slab::log_name(0, day, generation));
            let mut bytes = Vec::new();
            for art in [2, 1, 2] {
                slab::frame(ts, &line(art, "p@x", NOV8), &mut bytes);
            }
            std::fs::write(&path, &bytes).unwrap();
            logs.push(path);
        }
        let mut opened = Opened::default();
        let charge = row_charge(&line(1, "p@x", NOV8)) * 3;
        let memory = Arc::new(Semaphore::new(charge));
        let mut recovery = RecoveryOpen { memory: &memory, bytes: charge, logs: 2 };
        let before_commit = std::fs::read(&logs[1]).unwrap();
        let g = open_group(&first, "g", &keep_all(), &mut opened, &mut recovery).unwrap();
        let h = open_group(&second, "h", &keep_all(), &mut opened, &mut recovery).unwrap();
        assert!(logs[0].exists());
        assert_eq!(g.slabs.lock().unwrap().open.len(), 1);
        assert!(h.slabs.lock().unwrap().open.is_empty());
        assert!(!logs[1].exists());
        let (_, entries) = h.segments.read().unwrap()[&day].snapshot();
        assert_eq!((entries.len(), entries[0].rows, entries[0].slab_gen), (1, 2, 2));
        drop(h);

        std::fs::write(&logs[1], before_commit).unwrap();
        let h = open_group(&second, "h", &keep_all(), &mut opened, &mut recovery).unwrap();
        assert_eq!(opened.logs_already_committed, 1);
        assert_eq!(h.segments.read().unwrap()[&day].block_count(), 1);
        assert!(!logs[1].exists());
    }

    #[test]
    fn a_crash_before_the_seal_replays_the_log_and_a_reread_dedups_against_it() {
        let dir = tempfile::tempdir().unwrap();
        let lines: Vec<Vec<u8>> = (0..30).map(|k| line(500 + k, "p@x", NOV8)).collect();
        {
            let (store, _) = Store::open(cfg(dir.path()), keep_all()).unwrap();
            let t = store.record("s", "g", &lines, Clamp::Live { now: NOW }).unwrap();
            store.tick().unwrap();
            t.wait(Duration::from_secs(1)).unwrap();
            // a crash: the store is dropped without close (its threads see stopping, but nothing is sealed)
            store.unlock_for_test();
            std::mem::forget(store);
        }
        let (store, opened) = Store::open(cfg(dir.path()), keep_all()).unwrap();
        assert_eq!((opened.logs_replayed, opened.rows_replayed), (1, 30));
        // the cursor's move was lost: the same rows come again, plus new ones
        let mut again = lines.clone();
        again.extend((30..35).map(|k| line(500 + k, "p@x", NOV8)));
        store.record("s", "g", &again, Clamp::Live { now: NOW }).unwrap();
        store.seal_days(&[day_of("2026-11-08")]);
        store.tick().unwrap();
        wait_blocks(&store, 1);
        let blocks = store.blocks("g", NOW - DAY, NOW);
        assert_eq!(blocks.len(), 1, "one block, not the replay apart from the reread");
        assert_eq!(blocks[0].1.rows, 35);
        assert_eq!(store.counters().1, 30, "the reread's repeats dropped at seal");
        store.close().unwrap();
        let logs = std::fs::read_dir(dir.path().join("g"))
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("slab-"))
            .count();
        assert_eq!(logs, 0, "the replayed log and the new one both gone");
    }

    #[test]
    fn a_log_whose_block_was_committed_before_a_crash_is_not_replayed() {
        let dir = tempfile::tempdir().unwrap();
        let lines: Vec<Vec<u8>> = (0..10).map(|k| line(1 + k, "p@x", NOV8)).collect();
        let kept_log;
        {
            let (store, _) = Store::open(cfg(dir.path()), keep_all()).unwrap();
            store.record("s", "g", &lines, Clamp::Live { now: NOW }).unwrap();
            store.tick().unwrap();
            let gdir = dir.path().join("g");
            let log = std::fs::read_dir(&gdir)
                .unwrap()
                .flatten()
                .find(|e| e.file_name().to_string_lossy().starts_with("slab-"))
                .unwrap()
                .path();
            kept_log = std::fs::read(&log).unwrap();
            store.seal_days(&[day_of("2026-11-08")]);
            store.tick().unwrap();
            wait_blocks(&store, 1);
            store.close().unwrap();
            // the crash came between commit and the log's delete: put the log back
            std::fs::write(&log, &kept_log).unwrap();
        }
        let (store, opened) = Store::open(cfg(dir.path()), keep_all()).unwrap();
        assert_eq!(opened.logs_already_committed, 1);
        assert_eq!(opened.logs_replayed, 0);
        assert_eq!(store.blocks("g", NOW - DAY, NOW).len(), 1);
        store.close().unwrap();
    }

    #[test]
    fn a_full_slab_seals_at_its_cap_and_close_seals_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = cfg(dir.path());
        c.seal_rows = 25;
        c.sync_every = Duration::from_millis(20);
        let (store, _) = Store::open(c, keep_all()).unwrap();
        let lines: Vec<Vec<u8>> = (0..60).map(|k| line(1 + k, "p@x", NOV8)).collect();
        let t = store.record("s", "g", &lines, Clamp::Live { now: NOW }).unwrap();
        t.wait(Duration::from_secs(5)).unwrap();
        store.close().unwrap();
        let rows: Vec<u32> = store.blocks("g", NOW - DAY, NOW).iter().map(|(_, e)| e.rows).collect();
        assert_eq!(rows, vec![25, 25, 10]);
    }

    #[test]
    fn expiry_drops_days_no_longer_kept_but_not_one_still_filling() {
        let dir = tempfile::tempdir().unwrap();
        let (store, _) = Store::open(cfg(dir.path()), keep_all()).unwrap();
        let old: Vec<Vec<u8>> = (0..20).map(|k| line(1 + k, "p@x", "Sun, 01 Nov 2026 10:00:00 +0000")).collect();
        store.record("s", "g", &old, Clamp::Live { now: NOW - 7 * DAY }).unwrap();
        store.seal_days(&[day_of("2026-11-01")]);
        store.tick().unwrap();
        wait_blocks(&store, 1);
        let filling: Vec<Vec<u8>> = (0..20).map(|k| line(100 + k, "p@x", NOV8)).collect();
        store.record("s", "g", &filling, Clamp::Live { now: NOW }).unwrap();
        store.tick().unwrap();
        store.set_keep(KeepSet { live_from: day_of("2026-11-09"), ..KeepSet::default() });
        store.housekeeping();
        assert!(dir.path().join("g/2026-11-01.nxv").exists(), "nothing expires before the linker has run");
        store.arm_expiry();
        store.housekeeping();
        assert!(!dir.path().join("g/2026-11-01.nxv").exists(), "an old day goes");
        assert_eq!(
            std::fs::read_dir(dir.path().join("g"))
                .unwrap()
                .flatten()
                .filter(|e| e.file_name().to_string_lossy().starts_with("slab-"))
                .count(),
            1,
            "the day still filling stays"
        );
        store.set_keep(keep_all());
        store.close().unwrap();
    }
}
