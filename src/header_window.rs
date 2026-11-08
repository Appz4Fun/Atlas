//! The header window: every overview line of the last day around where reading stands, kept compressed on local
//! disk, soo uploads posted with a random subject per article can be tied together across groups by their poster
//! and time without the rows living in a database.
//!
//! Lines go into one `.nxv` block per server, group and posting hour (`overview_archive`'s format). An hour still
//! being read is held in memory and in a tail file beside it (length-prefixed lines), soo a restart loses nothing;
//! it is sealed once it has had no new line for `SEAL_IDLE` or holds `SEGMENT_ROWS` lines. Each sealed block has an
//! index file: its posting-time range and an Xor8 filter of its posters. A lookup by poster reads only the blocks
//! of the hours asked for whose filter may hold that poster. An hour outside every window the caller keeps is
//! deleted as files: nothing to vacuum.
//!
//! The window is in posting time, which lockstep backfill keeps aligned across groups.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use nntp_xover_engine::container::{NxvBlockBuilder, NxvBlockView};
use nntp_xover_engine::date::parse_rfc2822_to_epoch;
use nntp_xover_engine::msgid_index::{Xor8Builder, Xor8View, msgid_hash};
use nntp_xover_engine::streamer::RowCache;

pub const HOUR: i64 = 3600;
/// lines in one block at most (the format's limit)
pub const SEGMENT_ROWS: usize = nntp_xover_engine::container::MAX_BLOCK_ROWS;
/// an hour with no new line for this long is sealed
pub const SEAL_IDLE: Duration = Duration::from_secs(600);
const IDX_MAGIC: [u8; 4] = *b"HWI1";

/// The overview fields this store reads: poster and posting time.
fn fields(line: &[u8]) -> Option<(&[u8], i64)> {
    let mut it = line.split(|b| *b == b'\t');
    let _art = it.next()?;
    let _subject = it.next()?;
    let poster = it.next()?;
    let date = it.next()?;
    Some((poster, parse_rfc2822_to_epoch(date)?))
}

fn art_of(line: &[u8]) -> u64 {
    line.iter()
        .take_while(|b| b.is_ascii_digit())
        .fold(0u64, |n, b| n.saturating_mul(10).saturating_add((b - b'0') as u64))
}

/// A line the store can hold: it starts with an article number and is one line.
fn usable(line: &[u8]) -> bool {
    let n = line.iter().take_while(|b| b.is_ascii_digit()).count();
    n > 0 && n <= 18 && line.get(n) == Some(&b'\t') && !line.contains(&b'\n') && !line.contains(&b'\r')
}

/// A sealed block: where it is, what posting times it spans, and its posters' filter.
struct Sealed {
    path: PathBuf,
    min: i64,
    max: i64,
    rows: usize,
    filter: Vec<u8>,
}

impl Sealed {
    fn may_hold(&self, poster_hash: u64) -> bool {
        Xor8View::from_section(&self.filter).is_none_or(|f| f.contains_hash(poster_hash))
    }
}

/// An hour still being read.
struct Open {
    lines: Vec<Vec<u8>>,
    tail: BufWriter<fs::File>,
    tail_path: PathBuf,
    last: Instant,
}

/// One server's group.
#[derive(Default)]
struct Group {
    dir: PathBuf,
    open: BTreeMap<i64, Open>,
    sealed: BTreeMap<i64, Vec<Sealed>>,
}

/// The store under `root` (`header-window/` beside the database).
pub struct Window {
    root: PathBuf,
    groups: HashMap<(String, String), Group>,
}

/// What a lookup found: the server, the group, and the line.
pub type Found = (String, String, Vec<u8>);

impl Window {
    /// The store at `root`, its sealed blocks' indexes read and its tails taken back into memory.
    pub fn open(root: &Path) -> Result<Window> {
        fs::create_dir_all(root).with_context(|| format!("making {}", root.display()))?;
        let mut w = Window { root: root.to_path_buf(), groups: HashMap::new() };
        for server in read_dirs(root)? {
            for group_dir in read_dirs(&server)? {
                let (Some(s), Some(g)) = (decode(&server), decode(&group_dir)) else { continue };
                let mut group = Group { dir: group_dir.clone(), ..Group::default() };
                for entry in fs::read_dir(&group_dir)? {
                    let path = entry?.path();
                    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    if let Some(stem) = name.strip_suffix(".idx") {
                        let Some(hour) = stem.split('-').next().and_then(|h| h.parse::<i64>().ok()) else { continue };
                        let block = group_dir.join(format!("{stem}.nxv"));
                        if !block.exists() {
                            // a seal cut short before its block landed: the tail still holds the lines
                            let _ = fs::remove_file(&path);
                            continue;
                        }
                        group.sealed.entry(hour).or_default().push(read_idx(&path, block)?);
                    } else if let Some(hour) = name.strip_suffix(".tail").and_then(|h| h.parse::<i64>().ok()) {
                        let lines = read_tail(&path)?;
                        let tail = BufWriter::new(fs::OpenOptions::new().append(true).open(&path)?);
                        group.open.insert(hour, Open { lines, tail, tail_path: path, last: Instant::now() });
                    }
                }
                w.groups.insert((s, g), group);
            }
        }
        Ok(w)
    }

    /// Overview lines of `group` read from `server`, each to its posting hour. A line whose date won't read, or
    /// that isn't one overview line, is left out (the count of those comes back).
    pub fn record(&mut self, server: &str, group: &str, lines: &[Vec<u8>]) -> Result<usize> {
        let mut skipped = 0;
        let dir = crate::overview_archive::group_dir(&self.root, server, group);
        let g = self
            .groups
            .entry((server.to_string(), group.to_string()))
            .or_insert_with(|| Group { dir: dir.clone(), ..Group::default() });
        fs::create_dir_all(&g.dir)?;
        let mut full = Vec::new();
        for line in lines {
            let Some((_, posted)) = fields(line).filter(|_| usable(line)) else {
                skipped += 1;
                continue;
            };
            let hour = posted.div_euclid(HOUR) * HOUR;
            let open = match g.open.entry(hour) {
                std::collections::btree_map::Entry::Occupied(o) => o.into_mut(),
                std::collections::btree_map::Entry::Vacant(v) => {
                    let tail_path = g.dir.join(format!("{hour}.tail"));
                    let tail = BufWriter::new(fs::OpenOptions::new().create(true).append(true).open(&tail_path)?);
                    v.insert(Open { lines: Vec::new(), tail, tail_path, last: Instant::now() })
                }
            };
            open.tail.write_all(&(line.len() as u32).to_le_bytes())?;
            open.tail.write_all(line)?;
            open.lines.push(line.clone());
            open.last = Instant::now();
            if open.lines.len() >= SEGMENT_ROWS {
                full.push(hour);
            }
        }
        for open in g.open.values_mut() {
            open.tail.flush()?;
        }
        for hour in full {
            seal_hour(g, hour)?;
        }
        Ok(skipped)
    }

    /// Seal every hour that has had no new line for `idle`. Returns the blocks written.
    pub fn seal_idle(&mut self, idle: Duration) -> Result<usize> {
        let mut n = 0;
        for g in self.groups.values_mut() {
            let quiet: Vec<i64> = g.open.iter().filter(|(_, o)| o.last.elapsed() >= idle).map(|(h, _)| *h).collect();
            for hour in quiet {
                n += seal_hour(g, hour)?;
            }
        }
        Ok(n)
    }

    /// Delete every hour, open or sealed, that overlaps none of `keep` (posting-time ranges, seconds, inclusive).
    /// Returns the hours dropped.
    pub fn expire(&mut self, keep: &[(i64, i64)]) -> Result<usize> {
        let kept = |hour: i64| keep.iter().any(|(from, to)| hour + HOUR > *from && hour <= *to);
        let mut dropped = 0;
        for g in self.groups.values_mut() {
            let gone: Vec<i64> = g.sealed.keys().copied().filter(|h| !kept(*h)).collect();
            for hour in gone {
                for s in g.sealed.remove(&hour).unwrap_or_default() {
                    remove(&s.path.with_extension("idx"))?;
                    remove(&s.path)?;
                }
                dropped += 1;
            }
            let gone: Vec<i64> = g.open.keys().copied().filter(|h| !kept(*h)).collect();
            for hour in gone {
                if let Some(o) = g.open.remove(&hour) {
                    drop(o.tail);
                    remove(&o.tail_path)?;
                }
                dropped += 1;
            }
        }
        Ok(dropped)
    }

    /// Every line by `poster` posted from `from` to `to` (seconds, inclusive), in any server's group.
    pub fn by_poster(&self, poster: &[u8], from: i64, to: i64) -> Result<Vec<Found>> {
        let hash = msgid_hash(poster);
        let first = from.div_euclid(HOUR) * HOUR;
        let mut out = Vec::new();
        let mut cache = RowCache::default();
        let mut line = Vec::new();
        for ((server, group), g) in &self.groups {
            let mut keep = |l: &[u8]| {
                if let Some((p, t)) = fields(l)
                    && p == poster
                    && t >= from
                    && t <= to
                {
                    out.push((server.clone(), group.clone(), l.to_vec()));
                }
            };
            for (_, open) in g.open.range(first..=to) {
                open.lines.iter().for_each(|l| keep(l));
            }
            for (_, blocks) in g.sealed.range(first..=to) {
                for s in blocks.iter().filter(|s| s.max >= from && s.min <= to && s.may_hold(hash)) {
                    let bytes = read_aligned(&s.path)?;
                    let view = NxvBlockView::from_mmap_slice(&bytes)?;
                    for row in 0..view.row_count() {
                        line.clear();
                        view.reconstruct_row(row, &mut cache, &mut line)?;
                        keep(&line);
                    }
                }
            }
        }
        out.sort_by(|a, b| fields(&a.2).map(|f| f.1).cmp(&fields(&b.2).map(|f| f.1)).then_with(|| a.cmp(b)));
        out.dedup();
        Ok(out)
    }

    /// Lines held, blocks sealed, and hours (open and sealed) over every group.
    pub fn stats(&self) -> (usize, usize, usize) {
        let mut lines = 0;
        let mut blocks = 0;
        let mut hours = 0;
        for g in self.groups.values() {
            lines += g.open.values().map(|o| o.lines.len()).sum::<usize>();
            lines += g.sealed.values().flatten().map(|s| s.rows).sum::<usize>();
            blocks += g.sealed.values().map(Vec::len).sum::<usize>();
            hours += g.open.len() + g.sealed.keys().filter(|h| !g.open.contains_key(h)).count();
        }
        (lines, blocks, hours)
    }
}

/// Seal `hour` of `g`: its lines into a block (several when over `SEGMENT_ROWS`), each with its index, then the
/// tail goes. Written in that order, soo a cut leaves the tail to read again (`Window::open` drops an index whose
/// block isn't there; lines a cut left both in a block and in the tail are found once, `Window::by_poster` dedups).
fn seal_hour(g: &mut Group, hour: i64) -> Result<usize> {
    let Some(open) = g.open.remove(&hour) else { return Ok(0) };
    drop(open.tail);
    let mut lines = open.lines;
    lines.sort_by_key(|l| art_of(l));
    lines.dedup_by(|a, b| art_of(a) == art_of(b));
    let first_part = g.sealed.get(&hour).map_or(0, Vec::len);
    let mut written = 0;
    for (i, chunk) in lines.chunks(SEGMENT_ROWS).enumerate() {
        let stem = format!("{hour}-{}", first_part + i);
        let block_path = g.dir.join(format!("{stem}.nxv"));
        let refs: Vec<&[u8]> = chunk.iter().map(|l| l.as_slice()).collect();
        let block = NxvBlockBuilder::build_hot_block(&refs)?;
        write_synced(&block_path, &block)?;
        let mut hashes = Vec::with_capacity(chunk.len());
        let (mut min, mut max) = (i64::MAX, i64::MIN);
        for l in chunk {
            if let Some((p, t)) = fields(l) {
                hashes.push(msgid_hash(p));
                min = min.min(t);
                max = max.max(t);
            }
        }
        let filter = Xor8Builder::build(&hashes);
        let idx_path = block_path.with_extension("idx");
        let mut idx = Vec::with_capacity(28 + filter.len());
        idx.extend_from_slice(&IDX_MAGIC);
        idx.extend_from_slice(&min.to_le_bytes());
        idx.extend_from_slice(&max.to_le_bytes());
        idx.extend_from_slice(&(chunk.len() as u64).to_le_bytes());
        idx.extend_from_slice(&filter);
        write_synced(&idx_path, &idx)?;
        g.sealed.entry(hour).or_default().push(Sealed { path: block_path, min, max, rows: chunk.len(), filter });
        written += 1;
    }
    remove(&open.tail_path)?;
    Ok(written)
}

fn read_idx(path: &Path, block: PathBuf) -> Result<Sealed> {
    let b = fs::read(path)?;
    if b.len() < 28 || b[..4] != IDX_MAGIC {
        bail!("{} isn't a header window index", path.display());
    }
    let num = |at: usize| i64::from_le_bytes(b[at..at + 8].try_into().expect("8 bytes"));
    Ok(Sealed { path: block, min: num(4), max: num(12), rows: num(20) as usize, filter: b[28..].to_vec() })
}

fn read_tail(path: &Path) -> Result<Vec<Vec<u8>>> {
    let mut bytes = Vec::new();
    fs::File::open(path)?.read_to_end(&mut bytes)?;
    let mut out = Vec::new();
    let mut at = 0;
    // a line cut short by a crash ends the tail
    while at + 4 <= bytes.len() {
        let n = u32::from_le_bytes(bytes[at..at + 4].try_into().expect("4 bytes")) as usize;
        if at + 4 + n > bytes.len() {
            break;
        }
        out.push(bytes[at + 4..at + 4 + n].to_vec());
        at += 4 + n;
    }
    Ok(out)
}

/// A block read into memory aligned as the format wants it.
fn read_aligned(path: &Path) -> Result<nntp_xover_engine::container::AlignedBytes> {
    Ok(nntp_xover_engine::container::AlignedBytes::from_slice(&fs::read(path)?))
}

fn write_synced(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

fn remove(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}

fn read_dirs(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for e in fs::read_dir(dir)? {
        let p = e?.path();
        if p.is_dir() {
            out.push(p);
        }
    }
    Ok(out)
}

/// A directory's name back to the server or group it stands for (`overview_archive`'s percent-encoding).
fn decode(dir: &Path) -> Option<String> {
    let name = dir.file_name()?.to_str()?;
    let bytes = name.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 3 <= bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    let s = String::from_utf8(out).ok()?;
    // `%00` stands for an empty name
    Some(if s == "\0" { String::new() } else { s })
}

/// The newest backfill day any group still has to do (`chunks::frontier`), set as claims are made; 0 before any.
/// Lockstep reads the groups' days around it, soo the window keeps those.
pub static BACKFILL_FRONTIER_DAY: AtomicI64 = AtomicI64::new(0);

/// The posting-time ranges the window keeps at `now`: the last `keep_hours` (live reading) and the days lockstep
/// backfill reads around its frontier.
pub fn kept_ranges(now: i64, keep_hours: i64) -> Vec<(i64, i64)> {
    let mut keep = vec![(now - keep_hours * HOUR, now + HOUR)];
    let day = BACKFILL_FRONTIER_DAY.load(Ordering::Relaxed);
    if day > 0 {
        let lockstep = crate::chunks::LOCKSTEP_DAYS;
        keep.push(((day - lockstep) * 86_400, (day + 1) * 86_400));
    }
    keep
}

enum Msg {
    Batch { server: String, group: String, lines: Vec<Vec<u8>> },
    Finish(SyncSender<()>),
}

static SENDER: OnceLock<Mutex<Option<SyncSender<Msg>>>> = OnceLock::new();
static DROPPED: AtomicU64 = AtomicU64::new(0);
static RECORDED: AtomicU64 = AtomicU64::new(0);
static FAILED: AtomicU64 = AtomicU64::new(0);

/// Batches dropped (the writer behind or gone), lines recorded, and writes that failed, since start.
pub fn counters() -> (u64, u64, u64) {
    (DROPPED.load(Ordering::Relaxed), RECORDED.load(Ordering::Relaxed), FAILED.load(Ordering::Relaxed))
}

/// Whether the writer is running, soo a caller can skip copying rows for it.
pub fn enabled() -> bool {
    SENDER.get().is_some_and(|l| l.lock().is_ok_and(|g| g.is_some()))
}

/// Starts the window's writer when it's on. Call once, from the indexer.
pub fn spawn(cfg: &crate::config::HeaderWindowConfig) {
    if !cfg.enabled {
        return;
    }
    let root = cfg.root();
    let window = match Window::open(&root) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("header window: couldnt open {}: {e:#}; not recording", root.display());
            return;
        }
    };
    let (tx, rx) = sync_channel(256);
    if SENDER.set(Mutex::new(Some(tx))).is_err() {
        return;
    }
    let keep_hours = cfg.keep_hours;
    let _ = std::thread::Builder::new().name("header-window".into()).spawn(move || run(window, rx, keep_hours));
}

/// Hands one fetched XOVER batch to the window. Does nothing when it's off; drops the batch when the writer is
/// behind (counted), rather than hold the fetch up.
pub fn record(server: &str, group: &str, lines: &[Vec<u8>]) {
    let Some(lock) = SENDER.get() else { return };
    let Ok(guard) = lock.lock() else { return };
    let Some(tx) = guard.as_ref() else { return };
    if lines.is_empty() {
        return;
    }
    let msg = Msg::Batch { server: server.to_string(), group: group.to_string(), lines: lines.to_vec() };
    if let Err(e) = tx.try_send(msg) {
        DROPPED.fetch_add(1, Ordering::Relaxed);
        if matches!(e, TrySendError::Disconnected(_)) {
            eprintln!("header window: the writer stopped");
        }
    }
}

/// Waits (up to `timeout`) for what's queued to be written, then stops. For shutdown.
pub fn finish(timeout: Duration) {
    let Some(lock) = SENDER.get() else { return };
    let Some(tx) = lock.lock().ok().and_then(|mut g| g.take()) else { return };
    let (done_tx, done_rx) = sync_channel(1);
    if tx.send(Msg::Finish(done_tx)).is_ok() {
        let _ = done_rx.recv_timeout(timeout);
    }
}

fn run(mut window: Window, rx: Receiver<Msg>, keep_hours: i64) {
    let mut last_sweep = Instant::now();
    loop {
        let msg = rx.recv_timeout(Duration::from_secs(10));
        match msg {
            Ok(Msg::Batch { server, group, lines }) => match window.record(&server, &group, &lines) {
                Ok(skipped) => {
                    RECORDED.fetch_add((lines.len() - skipped) as u64, Ordering::Relaxed);
                }
                Err(e) => {
                    FAILED.fetch_add(1, Ordering::Relaxed);
                    eprintln!("header window: couldnt record {group}: {e:#}");
                }
            },
            Ok(Msg::Finish(done)) => {
                let _ = done.send(());
                return;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
        }
        if last_sweep.elapsed() >= Duration::from_secs(60) {
            last_sweep = Instant::now();
            let now = chrono::Utc::now().timestamp();
            if let Err(e) = window.seal_idle(SEAL_IDLE).and_then(|_| window.expire(&kept_ranges(now, keep_hours))) {
                FAILED.fetch_add(1, Ordering::Relaxed);
                eprintln!("header window: couldnt seal or expire: {e:#}");
            }
        }
    }
}

/// `atlas --header-window [stats | POSTER FROM TO]`: reads the window, never a server. FROM and TO are unix seconds.
pub fn run_cli(args: &[String]) -> i32 {
    let cfg = crate::config::header_window_config();
    let window = match Window::open(&cfg.root()) {
        Ok(w) => w,
        Err(e) => {
            println!("couldnt open {}: {e:#}", cfg.root().display());
            return 1;
        }
    };
    let rest: Vec<&String> = args.iter().skip_while(|a| *a != "--header-window").skip(1).collect();
    match rest.as_slice() {
        [] | [_] if rest.first().is_none_or(|a| *a == "stats") => {
            let (lines, blocks, hours) = window.stats();
            println!("{}: {lines} lines, {blocks} sealed blocks, {hours} hours", cfg.root().display());
            0
        }
        [poster, from, to] => {
            let (Ok(from), Ok(to)) = (from.parse::<i64>(), to.parse::<i64>()) else {
                println!("FROM and TO are unix seconds");
                return 1;
            };
            match window.by_poster(poster.as_bytes(), from, to) {
                Ok(found) => {
                    for (server, group, line) in found {
                        println!("{server}\t{group}\t{}", String::from_utf8_lossy(&line));
                    }
                    0
                }
                Err(e) => {
                    println!("couldnt read: {e:#}");
                    1
                }
            }
        }
        _ => {
            println!("usage: atlas --header-window [stats | POSTER FROM TO]");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(art: u64, poster: &str, date: &str, msgid: &str) -> Vec<u8> {
        format!("{art}\t[1/9] - \"x.part{art:02}.rar\" yEnc (1/50)\t{poster}\t{date}\t<{msgid}>\t\t700000\t5000")
            .into_bytes()
    }

    #[test]
    fn an_upload_across_groups_is_found_by_its_poster_and_time_sealed_or_not() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = Window::open(dir.path()).unwrap();
        let date = "Sun, 08 Nov 2026 07:00:01 +0000";
        let t = parse_rfc2822_to_epoch(date.as_bytes()).unwrap();
        let ours = "abc@x.y (abc)";
        for (g, base) in [("alt.binaries.one", 100), ("alt.binaries.two", 9000)] {
            let lines: Vec<Vec<u8>> = (0..5)
                .map(|i| line(base + i, if i == 4 { "other@x.y" } else { ours }, date, &format!("m{g}{i}@x")))
                .collect();
            assert_eq!(w.record("news.example", g, &lines).unwrap(), 0);
        }
        // a line that isn't one is left out
        assert_eq!(w.record("news.example", "alt.binaries.one", &[b"junk".to_vec()]).unwrap(), 1);
        let open = w.by_poster(ours.as_bytes(), t - 5, t + 5).unwrap();
        assert_eq!(open.len(), 8, "4 in each group while open");
        assert_eq!(w.seal_idle(Duration::ZERO).unwrap(), 2);
        let sealed = w.by_poster(ours.as_bytes(), t - 5, t + 5).unwrap();
        assert_eq!(sealed, open, "the same once sealed");
        assert!(w.by_poster(ours.as_bytes(), t + 10, t + 20).unwrap().is_empty(), "outside the time asked");
        assert!(w.by_poster(b"nobody@x.y", t - 5, t + 5).unwrap().is_empty());
        // back after a restart, sealed and open alike
        w.record("news.example", "alt.binaries.one", &[line(200, ours, date, "late@x")]).unwrap();
        drop(w);
        let w = Window::open(dir.path()).unwrap();
        assert_eq!(w.by_poster(ours.as_bytes(), t - 5, t + 5).unwrap().len(), 9);
        assert_eq!(w.stats().1, 2, "two blocks");
    }

    #[test]
    fn hours_outside_every_window_go_as_files() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = Window::open(dir.path()).unwrap();
        let dates =
            ["Sun, 01 Nov 2026 10:30:00 +0000", "Sat, 07 Nov 2026 10:30:00 +0000", "Sun, 08 Nov 2026 07:30:00 +0000"];
        for (i, d) in dates.iter().enumerate() {
            w.record("s", "g", &[line(i as u64 + 1, "p@x", d, &format!("e{i}@x"))]).unwrap();
        }
        w.seal_idle(Duration::ZERO).unwrap();
        w.record("s", "g", &[line(10, "p@x", dates[0], "open@x")]).unwrap();
        let now = parse_rfc2822_to_epoch(dates[2].as_bytes()).unwrap();
        let backfill = parse_rfc2822_to_epoch(dates[1].as_bytes()).unwrap();
        // the live day and a day around the backfill frontier
        let dropped = w.expire(&[(now - 24 * HOUR, now), (backfill - 12 * HOUR, backfill + 12 * HOUR)]).unwrap();
        assert_eq!(dropped, 2, "the old hour, sealed and open");
        assert_eq!(w.stats().2, 2);
        let files: Vec<_> = fs::read_dir(crate::overview_archive::group_dir(dir.path(), "s", "g")).unwrap().collect();
        assert_eq!(files.len(), 4, "two blocks and their indexes left");
    }

    #[test]
    fn the_window_keeps_the_live_day_and_the_days_around_the_backfill_frontier() {
        let now = 20_400 * 86_400 + 5 * HOUR;
        BACKFILL_FRONTIER_DAY.store(0, Ordering::Relaxed);
        assert_eq!(kept_ranges(now, 24), vec![(now - 24 * HOUR, now + HOUR)]);
        BACKFILL_FRONTIER_DAY.store(20_000, Ordering::Relaxed);
        let ranges = kept_ranges(now, 24);
        assert_eq!(ranges[1], ((20_000 - crate::chunks::LOCKSTEP_DAYS) * 86_400, 20_001 * 86_400));
        BACKFILL_FRONTIER_DAY.store(0, Ordering::Relaxed);
    }

    #[test]
    fn a_tail_cut_short_keeps_its_whole_lines() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("1.tail");
        let mut b = Vec::new();
        for l in [b"one".as_slice(), b"two"] {
            b.extend_from_slice(&(l.len() as u32).to_le_bytes());
            b.extend_from_slice(l);
        }
        b.extend_from_slice(&9u32.to_le_bytes());
        b.extend_from_slice(b"cut");
        fs::write(&p, b).unwrap();
        assert_eq!(read_tail(&p).unwrap(), [b"one".to_vec(), b"two".to_vec()]);
    }

    #[test]
    fn names_decode_as_the_archive_encodes_them() {
        let root = Path::new("/r");
        for name in ["alt.binaries.x", "news.example:563#2", "a b/c"] {
            let d = crate::overview_archive::group_dir(root, "s", name);
            assert_eq!(decode(&d).as_deref(), Some(name), "{}", d.display());
        }
    }
}
