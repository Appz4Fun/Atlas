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

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::File;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use nntp_xover_engine::container::{BlockMeta, MAX_BLOCK_ROWS, NxvBlockBuilder};
use nntp_xover_engine::date::parse_rfc2822_to_epoch;

use super::segment::{ENTRY_LEN, Entry, Reserved, Segment, parse_day_name};
use super::slab::{self, Sealed, Slab};
use super::sync::{fsync_only, sync_all_durable};

pub const DAY: i64 = 86_400;

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
    /// logs replaced by replay, deleted once the slab their rows went to has synced
    replaced: Vec<PathBuf>,
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
    /// rows refused because their day isn't kept, and repeats dropped at seal
    dropped_unkept: AtomicU64,
    dropped_repeats: AtomicU64,
    blocks_committed: AtomicU64,
}

impl Shared {
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
        self.synced.notify_all();
    }
}

/// A slab on its way to the sealers.
type SealJob = (Arc<Group>, Slab);

/// A block on its way to the committer.
struct Pending {
    group: Arc<Group>,
    day: i64,
    block: Vec<u8>,
    log: PathBuf,
}

pub struct Store {
    cfg: StoreConfig,
    groups: RwLock<HashMap<String, Arc<Group>>>,
    servers: Mutex<Vec<String>>,
    keep: RwLock<KeepSet>,
    shared: Arc<Shared>,
    seal_tx: Mutex<Option<SyncSender<SealJob>>>,
    threads: Mutex<Vec<std::thread::JoinHandle<()>>>,
    /// one tick at a time: two would interleave their writes to a log
    tick_lock: Mutex<()>,
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
const SERVERS_FILE: &str = "servers";

impl Store {
    /// Opens the store at `cfg.root`: server ids read, each group's kept segments healed and others deleted, and
    /// every slab log either deleted (its day isn't kept, or its block was committed before a crash) or replayed into
    /// a fresh slab, soo rows fetched again after the crash dedup against it at seal. Then starts the batcher, the
    /// sealers and the committer.
    pub fn open(cfg: StoreConfig, keep: KeepSet) -> Result<(Arc<Store>, Opened)> {
        std::fs::create_dir_all(cfg.root.join(STATE_DIR)).with_context(|| format!("making {}", cfg.root.display()))?;
        let _ = super::sync::raise_open_files(65_536);
        let servers = read_servers(&cfg.root)?;
        let mut opened = Opened::default();
        let mut groups = HashMap::new();
        for entry in std::fs::read_dir(&cfg.root)?.flatten() {
            let dir = entry.path();
            let Some(dname) = dir.file_name().and_then(|n| n.to_str()).map(str::to_string) else { continue };
            if !dir.is_dir() || dname == STATE_DIR {
                continue;
            }
            let Some(name) = decode_name(&dname) else { continue };
            let group = open_group(&dir, &name, &keep, &mut opened)?;
            groups.insert(name, group);
        }
        let shared = Arc::new(Shared {
            failed: Mutex::new(None),
            stopping: AtomicBool::new(false),
            synced_lock: Mutex::new(()),
            synced: Condvar::new(),
            wake_lock: Mutex::new(()),
            wake: Condvar::new(),
            dropped_unkept: AtomicU64::new(0),
            dropped_repeats: AtomicU64::new(0),
            blocks_committed: AtomicU64::new(0),
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
        });
        store.start();
        Ok((store, opened))
    }

    fn start(self: &Arc<Self>) {
        let (seal_tx, seal_rx) = sync_channel::<SealJob>(64);
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
                    .spawn(move || sealer(rx, tx, shared))
                    .expect("spawning a sealer"),
            );
        }
        drop(commit_tx);
        let shared = Arc::clone(&self.shared);
        threads.push(
            std::thread::Builder::new()
                .name("nxv-committer".into())
                .spawn(move || committer(commit_rx, shared))
                .expect("spawning the committer"),
        );
        *self.seal_tx.lock().unwrap_or_else(|e| e.into_inner()) = Some(seal_tx);
        let me = Arc::clone(self);
        threads.push(
            std::thread::Builder::new()
                .name("nxv-batcher".into())
                .spawn(move || me.batcher())
                .expect("spawning the batcher"),
        );
        *self.threads.lock().unwrap_or_else(|e| e.into_inner()) = threads;
    }

    pub fn failure(&self) -> Option<String> {
        self.shared.failure()
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
        let g = Arc::new(new_group(dir, name, 1));
        gs.insert(name.to_string(), Arc::clone(&g));
        Ok(g)
    }

    /// A fetched slice of `group` from `server`: each row clamped and routed to its day's slab, rows of days not
    /// kept dropped (counted). The ticket says when the rows are durable.
    pub fn record(&self, server: &str, group: &str, lines: &[Vec<u8>], clamp: Clamp) -> Result<Ticket> {
        if let Some(e) = self.failure() {
            bail!("the header store stopped: {e}");
        }
        let sid = self.server_id(server)?;
        let g = self.group(group)?;
        let parsed: Vec<Option<(u64, Option<i64>)>> = lines
            .iter()
            .map(|l| {
                let art = slab::art_of(l)?;
                if l.contains(&b'\n') || l.contains(&b'\r') || l.len() > slab::MAX_LINE {
                    return None;
                }
                Some((art, l.split(|&b| b == b'\t').nth(3).and_then(parse_rfc2822_to_epoch)))
            })
            .collect();
        let dates: Vec<i64> = parsed.iter().filter_map(|p| p.and_then(|(_, d)| d)).collect();
        let (lo, hi) = clamp.bounds(&dates);
        let keep = self.keep.read().unwrap_or_else(|e| e.into_inner()).clone();
        let mut s = g.slabs.lock().unwrap_or_else(|e| e.into_inner());
        for (line, p) in lines.iter().zip(&parsed) {
            let Some((art, date)) = *p else { continue };
            let ts = date.unwrap_or((lo + hi) / 2).clamp(lo, hi);
            let day = ts.div_euclid(DAY);
            if !keep.keeps(day) {
                self.shared.dropped_unkept.fetch_add(1, Ordering::Relaxed);
                continue;
            }
            push_row(&g, &mut s, sid, day, art, ts, line, self.cfg.seal_rows)?;
        }
        let epoch = s.epoch + 1;
        drop(s);
        Ok(Ticket { group: g, epoch, shared: Arc::clone(&self.shared) })
    }

    /// A ticket for every row recorded for `group` so far. None when the group has none.
    pub fn sync_point(&self, group: &str) -> Option<Ticket> {
        let g = self.groups.read().unwrap_or_else(|e| e.into_inner()).get(group).cloned()?;
        let epoch = g.slabs.lock().unwrap_or_else(|e| e.into_inner()).epoch + 1;
        Some(Ticket { group: g, epoch, shared: Arc::clone(&self.shared) })
    }

    /// Asks for every slab of `days` to be sealed at the next tick (the linker's barrier before it links an hour).
    pub fn seal_days(&self, days: &[i64]) {
        for g in self.groups.read().unwrap_or_else(|e| e.into_inner()).values() {
            g.slabs.lock().unwrap_or_else(|e| e.into_inner()).seal_days.extend(days);
        }
    }

    /// One batcher tick, run by the batcher thread every `sync_every` (and by tests directly).
    pub fn tick(&self) -> Result<()> {
        let _one = self.tick_lock.lock().unwrap_or_else(|e| e.into_inner());
        let groups: Vec<Arc<Group>> = self.groups.read().unwrap_or_else(|e| e.into_inner()).values().cloned().collect();
        let mut writes: Vec<(Arc<File>, Vec<u8>)> = Vec::new();
        let mut dirs: Vec<PathBuf> = Vec::new();
        let mut synced: Vec<(Arc<Group>, u64, Vec<PathBuf>)> = Vec::new();
        let mut to_seal: Vec<(Arc<Group>, Slab)> = Vec::new();
        for g in &groups {
            let mut s = g.slabs.lock().unwrap_or_else(|e| e.into_inner());
            // the slabs ready to seal leave now, with their last bytes in this tick's writes
            let seal_days = std::mem::take(&mut s.seal_days);
            let ready: Vec<(u16, i64)> = s
                .open
                .iter()
                .filter(|(_, sl)| {
                    sl.len() >= self.cfg.seal_rows
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
            synced.push((Arc::clone(g), s.epoch, std::mem::take(&mut s.replaced)));
            to_seal.extend(leaving.into_iter().filter(|sl| !sl.is_empty()).map(|sl| (Arc::clone(g), sl)));
        }
        // outside the locks: write, then make it all durable with one full flush
        let result = (|| -> Result<()> {
            for (f, bytes) in &writes {
                slab::append(f, bytes)?;
            }
            let dir_files: Vec<File> = dirs.iter().map(File::open).collect::<std::io::Result<_>>()?;
            let mut files: Vec<&File> = writes.iter().map(|(f, _)| f.as_ref()).collect();
            files.extend(dir_files.iter());
            sync_all_durable(&files)?;
            Ok(())
        })();
        if let Err(e) = result {
            self.shared.fail(e);
            return Err(anyhow!("header store: log sync failed"));
        }
        for (g, epoch, replaced) in synced {
            g.synced_epoch.store(epoch, Ordering::Release);
            for p in replaced {
                let _ = std::fs::remove_file(p);
            }
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
                    .wait_timeout_while(g, self.cfg.sync_every, |_| !self.shared.stopping.load(Ordering::Acquire))
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
        let keep = self.keep.read().unwrap_or_else(|e| e.into_inner()).clone();
        for g in self.groups.read().unwrap_or_else(|e| e.into_inner()).values() {
            let open_days: BTreeSet<i64> = {
                let s = g.slabs.lock().unwrap_or_else(|e| e.into_inner());
                s.open.values().chain(s.full.iter()).map(|sl| sl.day).collect()
            };
            let mut segs = g.segments.write().unwrap_or_else(|e| e.into_inner());
            let gone: Vec<i64> = segs
                .iter()
                .filter(|(day, seg)| !keep.keeps(**day) && !open_days.contains(day) && seg.expire())
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

    /// Seals and commits everything, then stops the threads. What `record` got after this isn't kept.
    pub fn close(&self) -> Result<()> {
        self.shared.stopping.store(true, Ordering::Release);
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

    pub fn group_names(&self) -> Vec<String> {
        self.groups.read().unwrap_or_else(|e| e.into_inner()).keys().cloned().collect()
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
            replaced: Vec::new(),
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
fn push_row(
    g: &Group,
    s: &mut Slabs,
    sid: u16,
    day: i64,
    art: u64,
    ts: i64,
    line: &[u8],
    seal_rows: usize,
) -> Result<()> {
    let key = (sid, day);
    if s.open.get(&key).is_some_and(|sl| sl.len() >= seal_rows)
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
    Ok(())
}

/// A group at open: kept segments healed, others deleted; its logs deleted or replayed.
fn open_group(dir: &Path, name: &str, keep: &KeepSet, opened: &mut Opened) -> Result<Arc<Group>> {
    let mut segments = BTreeMap::new();
    let mut logs: Vec<(PathBuf, u16, i64, u32)> = Vec::new();
    let mut max_gen = 0u32;
    for e in std::fs::read_dir(dir)?.flatten() {
        let p = e.path();
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
    let g = new_group(dir.to_path_buf(), name, max_gen + 1);
    *g.segments.write().unwrap_or_else(|e| e.into_inner()) = segments;
    logs.sort_by_key(|l| l.3);
    {
        let mut s = g.slabs.lock().unwrap_or_else(|e| e.into_inner());
        for (path, server, day, slab_gen) in logs {
            if !keep.keeps(day) {
                let _ = std::fs::remove_file(&path);
                opened.logs_unkept += 1;
                continue;
            }
            let committed = g
                .segments
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .get(&day)
                .is_some_and(|seg| seg.is_committed(server, slab_gen));
            if committed {
                let _ = std::fs::remove_file(&path);
                opened.logs_already_committed += 1;
                continue;
            }
            let bytes = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
            for (ts, line) in slab::unframe(&bytes) {
                if let Some(art) = slab::art_of(line) {
                    push_row(&g, &mut s, server, day, art, ts, line, MAX_BLOCK_ROWS)?;
                    opened.rows_replayed += 1;
                }
            }
            s.replaced.push(path);
            opened.logs_replayed += 1;
        }
    }
    Ok(Arc::new(g))
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
        let Sealed { server, day, slab_gen, log_path, lines, dropped_repeats } = slab.seal();
        shared.dropped_repeats.fetch_add(dropped_repeats as u64, Ordering::Relaxed);
        let refs: Vec<&[u8]> = lines.iter().map(|l| l.as_slice()).collect();
        match NxvBlockBuilder::build_hot_block_meta(&refs, BlockMeta { server_id: server, slab_gen }) {
            Ok(block) => {
                if tx.send(Pending { group, day, block, log: log_path }).is_err() {
                    return;
                }
            }
            Err(e) => shared.fail(e.context(format!("building a block of {}", group.name))),
        }
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
            }
            Err(e) => shared.fail(e),
        }
    }
}

/// One batch: reserve, write, sync, write entries, sync, publish, delete logs. Returns blocks committed.
fn commit(batch: &[Pending]) -> Result<usize> {
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
            // its day expired meanwhile: the rows go, as rows of a day not kept do
            None => {
                let _ = std::fs::remove_file(&p.log);
            }
        }
    }
    for (r, p) in &reserved {
        r.arena.write_all_at(&p.block, r.entry.offset)?;
    }
    let dir_files: Vec<File> = new_dirs.iter().map(File::open).collect::<std::io::Result<_>>()?;
    let mut files: Vec<&File> = reserved.iter().map(|(r, _)| r.arena.as_ref()).collect();
    files.extend(dir_files.iter());
    sync_all_durable(&files)?;
    for (r, _) in &reserved {
        r.index.write_all_at(&r.entry.to_bytes(), r.entry.seq as u64 * ENTRY_LEN as u64)?;
    }
    let idx: Vec<&File> = reserved.iter().map(|(r, _)| r.index.as_ref()).collect();
    sync_all_durable(&idx)?;
    for (r, p) in &reserved {
        r.seg.publish(r)?;
        let _ = std::fs::remove_file(&p.log);
    }
    // the logs' names gone durably is not needed: a log left behind is found committed at the next start
    let _ = fsync_only;
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
        let lines: Vec<Vec<u8>> = (0..20).map(|k| line(k + 1, "p@x", "Sun, 01 Nov 2026 10:00:00 +0000")).collect();
        store.record("s", "g", &lines, Clamp::Live { now: NOW }).unwrap();
        assert_eq!(store.counters().0, 20);
        store.close().unwrap();
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
