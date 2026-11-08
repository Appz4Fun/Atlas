//! The header window: every overview line of the last day around where reading stands, kept compressed on local
//! disk, soo uploads posted with a random subject per article can be tied together across groups by their poster
//! and time without the rows living in a database.
//!
//! Lines go into one `.nxv` block per server, group and posting hour (`overview_archive`'s format). An hour still
//! being read is a tail file (length-prefixed lines) appended to as batches come and held nowhere else, soo memory
//! and open files stay flat however many groups and hours are being read. A tail is sealed into a block once its
//! hour has had no new line for `SEAL_IDLE` (or holds `SEGMENT_ROWS` lines), a few at a time (`SEAL_BUDGET`) soo the
//! turn of an hour doesn't stall the writer. Each block has an index file: its posting-time range and an Xor8 filter
//! of its posters. A lookup by poster reads the tails of the hours asked for and only those blocks whose filter may
//! hold that poster. An hour outside every window kept is deleted as files: nothing to vacuum; a line outside every
//! window isn't written at all.
//!
//! The window is a cache of what the servers still have: nothing is flushed to disk on purpose, and a power cut can
//! lose the last minutes, which reading again brings back. A cut between a block landing and its tail going leaves
//! those lines in both; a lookup dedups them.
//!
//! The window is in posting time, which lockstep backfill keeps aligned across groups: the last day of live reading,
//! and the days around each backfill day claimed lately (by any host, wanted days included).

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result, bail};
use nntp_xover_engine::container::{AlignedBytes, NxvBlockBuilder, NxvBlockView};
use nntp_xover_engine::date::parse_rfc2822_to_epoch;
use nntp_xover_engine::msgid_index::{Xor8Builder, Xor8View, msgid_hash};
use nntp_xover_engine::streamer::{NxvRangeCursor, XoverStreamScratch};

pub const HOUR: i64 = 3600;
/// lines in one block at most (the format's limit)
pub const SEGMENT_ROWS: usize = nntp_xover_engine::container::MAX_BLOCK_ROWS;
/// an hour with no new line for this long is sealed
pub const SEAL_IDLE: Duration = Duration::from_secs(600);
/// how long one sweep may spend sealing; the rest wait for the next
pub const SEAL_BUDGET: Duration = Duration::from_secs(5);
const IDX_MAGIC: [u8; 4] = *b"HWI1";
const CLAIMED_FILE: &str = "claimed-days";

/// The overview line's poster.
fn poster_of(line: &[u8]) -> Option<&[u8]> {
    line.split(|b| *b == b'\t').nth(2)
}

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

/// An hour still being read: its tail file, how many lines it holds, and when one last came.
struct Open {
    tail: PathBuf,
    lines: usize,
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
    /// The store at `root` for the writer: its blocks' indexes read, its tails found and cut back to their last whole
    /// line, and what a cut left half done cleared (a temp file, an index without its block or a block without its
    /// index, an index that won't read).
    pub fn open(root: &Path) -> Result<Window> {
        Window::open_with(root, true)
    }

    /// The store at `root` to read only, beside a writer: nothing repaired or cleared, what won't read passed over.
    pub fn open_read_only(root: &Path) -> Result<Window> {
        Window::open_with(root, false)
    }

    fn open_with(root: &Path, writable: bool) -> Result<Window> {
        if writable {
            fs::create_dir_all(root).with_context(|| format!("making {}", root.display()))?;
        } else if !root.exists() {
            return Ok(Window { root: root.to_path_buf(), groups: HashMap::new() });
        }
        load_claimed(root);
        let mut w = Window { root: root.to_path_buf(), groups: HashMap::new() };
        for server in read_dirs(root)? {
            for group_dir in read_dirs(&server)? {
                let (Some(s), Some(g)) = (decode(&server), decode(&group_dir)) else { continue };
                let mut group = Group { dir: group_dir.clone(), ..Group::default() };
                let names: Vec<String> = fs::read_dir(&group_dir)?
                    .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
                    .collect();
                for name in &names {
                    let path = group_dir.join(name);
                    if let Some(stem) = name.strip_suffix(".idx") {
                        let Some(hour) = stem.split('-').next().and_then(|h| h.parse::<i64>().ok()) else { continue };
                        let block = group_dir.join(format!("{stem}.nxv"));
                        match block.exists().then(|| read_idx(&path, block.clone())) {
                            Some(Ok(sealed)) => group.sealed.entry(hour).or_default().push(sealed),
                            other => {
                                if let Some(Err(e)) = other {
                                    eprintln!("header window: {} won't read ({e:#}); its hour is gone", path.display());
                                }
                                if writable {
                                    let _ = fs::remove_file(&path);
                                    let _ = fs::remove_file(&block);
                                }
                            }
                        }
                    } else if let Some(hour) = name.strip_suffix(".tail").and_then(|h| h.parse::<i64>().ok()) {
                        let lines = if writable { repair_tail(&path)? } else { read_tail(&path)?.len() };
                        group.open.insert(hour, Open { tail: path, lines, last: Instant::now() });
                    } else if writable && name.ends_with(".tmp") {
                        let _ = fs::remove_file(&path);
                    } else if writable
                        && let Some(stem) = name.strip_suffix(".nxv")
                        && !names.iter().any(|n| n.strip_suffix(".idx") == Some(stem))
                    {
                        // a block whose index went first (an expiry or a seal cut short)
                        let _ = fs::remove_file(&path);
                    }
                }
                w.groups.insert((s, g), group);
            }
        }
        Ok(w)
    }

    /// Overview lines of `group` read from `server`, each appended to its posting hour's tail. A line outside every
    /// range in `keep`, whose date won't read, or that isn't one overview line, is left out; the count of those comes
    /// back.
    pub fn record(&mut self, server: &str, group: &str, lines: &[Vec<u8>], keep: &[(i64, i64)]) -> Result<usize> {
        self.record_days(server, group, lines, keep).map(|(skipped, _)| skipped)
    }

    /// `record`, and the posting days of the lines taken in, with how many of each.
    pub fn record_days(
        &mut self,
        server: &str,
        group: &str,
        lines: &[Vec<u8>],
        keep: &[(i64, i64)],
    ) -> Result<(usize, Vec<(i64, usize)>)> {
        let mut skipped = 0;
        let mut by_hour: BTreeMap<i64, Vec<&[u8]>> = BTreeMap::new();
        for line in lines {
            match fields(line).filter(|_| usable(line)) {
                Some((_, posted)) if in_ranges(posted, keep) => {
                    by_hour.entry(posted.div_euclid(HOUR) * HOUR).or_default().push(line);
                }
                _ => skipped += 1,
            }
        }
        if by_hour.is_empty() {
            return Ok((skipped, Vec::new()));
        }
        // each posting day's lines taken in, soo the caller can tell a day being read from a stray date
        let mut days: Vec<(i64, usize)> = Vec::new();
        for (h, ls) in &by_hour {
            let d = h.div_euclid(86_400);
            match days.last_mut() {
                Some((last, n)) if *last == d => *n += ls.len(),
                _ => days.push((d, ls.len())),
            }
        }
        let g = self.groups.entry((server.to_string(), group.to_string())).or_insert_with(|| Group {
            dir: crate::overview_archive::group_dir(&self.root, server, group),
            ..Group::default()
        });
        fs::create_dir_all(&g.dir)?;
        let mut full = Vec::new();
        for (hour, lines) in by_hour {
            let tail = g.dir.join(format!("{hour}.tail"));
            let mut buf = Vec::with_capacity(lines.iter().map(|l| l.len() + 4).sum());
            for l in &lines {
                buf.extend_from_slice(&(l.len() as u32).to_le_bytes());
                buf.extend_from_slice(l);
            }
            // opened per batch: one file per group and hour stays open only as long as a write. A write that fails
            // part way is cut back off, soo what's appended later reads
            let mut f = fs::OpenOptions::new().create(true).append(true).open(&tail)?;
            let before = f.metadata()?.len();
            if let Err(e) = f.write_all(&buf) {
                let _ = f.set_len(before);
                return Err(e.into());
            }
            let open = g.open.entry(hour).or_insert_with(|| Open { tail, lines: 0, last: Instant::now() });
            open.lines += lines.len();
            open.last = Instant::now();
            if open.lines >= SEGMENT_ROWS {
                full.push(hour);
            }
        }
        for hour in full {
            seal_hour(g, hour)?;
        }
        Ok((skipped, days))
    }

    /// Seal hours that have had no new line for `idle`, the longest quiet first, for up to `budget`. Returns the
    /// blocks written. A seal that fails leaves its tail as it was, to be tried again.
    pub fn seal_idle(&mut self, idle: Duration, budget: Duration) -> Result<usize> {
        let started = Instant::now();
        let mut quiet: Vec<(Instant, (String, String), i64)> = Vec::new();
        for (key, g) in &self.groups {
            for (hour, o) in &g.open {
                if o.last.elapsed() >= idle {
                    quiet.push((o.last, key.clone(), *hour));
                }
            }
        }
        quiet.sort_by_key(|(last, _, _)| *last);
        let mut n = 0;
        for (_, key, hour) in quiet {
            if started.elapsed() >= budget {
                break;
            }
            if let Some(g) = self.groups.get_mut(&key) {
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
                    remove(&o.tail)?;
                }
                dropped += 1;
            }
        }
        Ok(dropped)
    }

    /// Every line by `poster` posted from `from` to `to` (seconds, inclusive), in any server's group, oldest first.
    pub fn by_poster(&self, poster: &[u8], from: i64, to: i64) -> Result<Vec<Found>> {
        if from > to {
            return Ok(Vec::new());
        }
        let hash = msgid_hash(poster);
        let first = from.div_euclid(HOUR) * HOUR;
        let mut out = Vec::new();
        let mut scratch = XoverStreamScratch::new();
        for ((server, group), g) in &self.groups {
            let mut keep = |l: &[u8]| {
                // the poster first: the date is the dear part
                if poster_of(l) == Some(poster)
                    && let Some((_, t)) = fields(l)
                    && t >= from
                    && t <= to
                {
                    out.push((server.clone(), group.clone(), l.to_vec()));
                }
            };
            for (_, open) in g.open.range(first..=to) {
                for l in read_tail(&open.tail)? {
                    keep(&l);
                }
            }
            for (_, blocks) in g.sealed.range(first..=to) {
                for s in blocks.iter().filter(|s| s.max >= from && s.min <= to && s.may_hold(hash)) {
                    let bytes = match fs::read(&s.path) {
                        Ok(b) => AlignedBytes::from_slice(&b),
                        Err(e) => {
                            eprintln!("header window: {} won't read: {e}", s.path.display());
                            continue;
                        }
                    };
                    // a block cut short by a power cut is passed over (it goes with its hour)
                    let Ok(view) = NxvBlockView::from_mmap_slice(&bytes) else {
                        eprintln!("header window: {} won't read", s.path.display());
                        continue;
                    };
                    // one pass over the block in order: each row decoded once (blocks here are never cold)
                    let Ok(mut cur) = NxvRangeCursor::new_for_art_range(view, 0, u64::MAX) else {
                        eprintln!("header window: {} won't read", s.path.display());
                        continue;
                    };
                    loop {
                        scratch.wire_buf.clear();
                        if !cur.format_next_row(&mut scratch) {
                            break;
                        }
                        let n = scratch.wire_buf.len().saturating_sub(2);
                        keep(&scratch.wire_buf[..n]);
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
            lines += g.open.values().map(|o| o.lines).sum::<usize>();
            lines += g.sealed.values().flatten().map(|s| s.rows).sum::<usize>();
            blocks += g.sealed.values().map(Vec::len).sum::<usize>();
            hours += g.open.len() + g.sealed.keys().filter(|h| !g.open.contains_key(h)).count();
        }
        (lines, blocks, hours)
    }
}

fn in_ranges(t: i64, keep: &[(i64, i64)]) -> bool {
    keep.iter().any(|(from, to)| t >= *from && t <= *to)
}

/// Seal `hour` of `g`: its tail's lines into a block (several when over `SEGMENT_ROWS`), each with its index, then
/// the tail goes. On any error the tail stays and so does the hour, to be tried again.
fn seal_hour(g: &mut Group, hour: i64) -> Result<usize> {
    let Some(open) = g.open.get(&hour) else { return Ok(0) };
    let tail = open.tail.clone();
    let mut lines = read_tail(&tail)?;
    lines.sort_by_key(|l| art_of(l));
    lines.dedup_by(|a, b| art_of(a) == art_of(b));
    // past the highest part there is, not the count: a part cleared after a power cut leaves a gap
    let first_part = g
        .sealed
        .get(&hour)
        .into_iter()
        .flatten()
        .filter_map(|s| s.path.file_stem()?.to_str()?.rsplit('-').next()?.parse::<usize>().ok())
        .max()
        .map_or(0, |n| n + 1);
    let mut made = Vec::new();
    for (i, chunk) in lines.chunks(SEGMENT_ROWS).enumerate() {
        let stem = format!("{hour}-{}", first_part + i);
        let block_path = g.dir.join(format!("{stem}.nxv"));
        let refs: Vec<&[u8]> = chunk.iter().map(|l| l.as_slice()).collect();
        let block = NxvBlockBuilder::build_hot_block(&refs)?;
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
        let mut idx = Vec::with_capacity(28 + filter.len());
        idx.extend_from_slice(&IDX_MAGIC);
        idx.extend_from_slice(&min.to_le_bytes());
        idx.extend_from_slice(&max.to_le_bytes());
        idx.extend_from_slice(&(chunk.len() as u64).to_le_bytes());
        idx.extend_from_slice(&filter);
        made.push((block_path, block, idx, Sealed { path: PathBuf::new(), min, max, rows: chunk.len(), filter }));
    }
    let mut sealed = Vec::new();
    for (block_path, block, idx, mut s) in made {
        write_replace(&block_path, &block)?;
        write_replace(&block_path.with_extension("idx"), &idx)?;
        s.path = block_path;
        sealed.push(s);
    }
    let n = sealed.len();
    g.sealed.entry(hour).or_default().extend(sealed);
    g.open.remove(&hour);
    remove(&tail)?;
    Ok(n)
}

fn read_idx(path: &Path, block: PathBuf) -> Result<Sealed> {
    let b = fs::read(path)?;
    if b.len() < 28 || b[..4] != IDX_MAGIC {
        bail!("{} isn't a header window index", path.display());
    }
    let num = |at: usize| i64::from_le_bytes(b[at..at + 8].try_into().expect("8 bytes"));
    Ok(Sealed { path: block, min: num(4), max: num(12), rows: num(20) as usize, filter: b[28..].to_vec() })
}

/// The whole lines of a tail and where they end: a line cut short by a crash, and anything after it, isn't one.
fn parse_tail(bytes: &[u8]) -> (Vec<Vec<u8>>, usize) {
    let mut out = Vec::new();
    let mut at = 0;
    while at + 4 <= bytes.len() {
        let n = u32::from_le_bytes(bytes[at..at + 4].try_into().expect("4 bytes")) as usize;
        if at + 4 + n > bytes.len() {
            break;
        }
        let line = &bytes[at + 4..at + 4 + n];
        if !usable(line) {
            break;
        }
        out.push(line.to_vec());
        at += 4 + n;
    }
    (out, at)
}

fn read_tail(path: &Path) -> Result<Vec<Vec<u8>>> {
    let mut bytes = Vec::new();
    match fs::File::open(path) {
        Ok(mut f) => f.read_to_end(&mut bytes)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    Ok(parse_tail(&bytes).0)
}

/// A tail cut back to its last whole line, soo what's appended next follows it. Returns its lines.
fn repair_tail(path: &Path) -> Result<usize> {
    let bytes = fs::read(path)?;
    let (lines, end) = parse_tail(&bytes);
    if end < bytes.len() {
        fs::OpenOptions::new().write(true).open(path)?.set_len(end as u64)?;
    }
    Ok(lines.len())
}

/// `bytes` at `path`, whole or not at all (written beside it, then renamed over it).
fn write_replace(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, bytes)?;
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
pub(crate) fn decode(dir: &Path) -> Option<String> {
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

/// How long a backfill day stays kept after a claim of it was last made (wall time).
pub const CLAIMED_KEEP: Duration = Duration::from_secs(6 * 3600);
/// After a start, how long nothing older than the live hours is dropped: the claims of the days being read come
/// again first.
pub const START_GRACE: Duration = Duration::from_secs(600);

/// The backfill days claimed lately (unix day, when last claimed, unix seconds), whichever host or group set claimed
/// them, wanted days included: the window takes in and keeps the days around each.
static CLAIMED: Mutex<BTreeMap<i64, i64>> = Mutex::new(BTreeMap::new());

/// A backfill day claimed (`chunks::claim`, `chunks::claim_turn`), to be kept around.
pub fn note_claimed_day(day: i64) {
    if let Ok(mut m) = CLAIMED.lock() {
        m.insert(day, now_secs());
    }
}

/// A batch was just taken in, `days` its posting days and how many lines of each: a claimed day that most of the
/// batch falls in or beside is still being read, however long its chunk takes, soo it stays kept. A stray date or
/// two in another batch doesn't keep a day read and gone. A timestamp moves only when minutes old, soo the store
/// isn't written again for every batch.
fn refresh_claimed(days: &[(i64, usize)], now: i64) {
    let total: usize = days.iter().map(|(_, n)| n).sum();
    if total == 0 {
        return;
    }
    let Ok(mut m) = CLAIMED.lock() else { return };
    for (claimed, at) in m.iter_mut() {
        let near: usize = days.iter().filter(|(d, _)| (d - claimed).abs() <= 1).map(|(_, n)| n).sum();
        if near * 2 > total && now - *at >= 300 {
            *at = now;
        }
    }
}

/// The days claimed within `CLAIMED_KEEP` of `now`; older ones are forgotten.
pub(crate) fn claimed_days(now: i64) -> Vec<i64> {
    let keep = CLAIMED_KEEP.as_secs() as i64;
    let Ok(mut m) = CLAIMED.lock() else { return Vec::new() };
    m.retain(|_, at| now - *at <= keep);
    m.keys().copied().collect()
}

/// Posting-time ranges, seconds, both ends in.
pub type Ranges = Vec<(i64, i64)>;

/// The posting-time ranges the window takes lines in and keeps at `now`: the last `keep_hours` (live reading) and
/// for each backfill day claimed lately (`days`) that day and one either side, where lockstep has the other groups
/// reading and an upload over midnight lands. While `in_grace` (just started), nothing older than the live hours is
/// dropped: (record, keep).
pub fn ranges(now: i64, keep_hours: i64, days: &[i64], in_grace: bool) -> (Ranges, Ranges) {
    let mut take = vec![(now - keep_hours * HOUR, now + HOUR)];
    take.extend(days.iter().map(|d| ((d - 1) * 86_400, (d + 2) * 86_400 - 1)));
    let keep = if in_grace { vec![(i64::MIN, now + HOUR)] } else { take.clone() };
    (take, keep)
}

/// The claimed days kept in the store, back in `CLAIMED` (a restart's first claims come after the first sweep).
fn load_claimed(root: &Path) {
    let Ok(text) = fs::read_to_string(root.join(CLAIMED_FILE)) else { return };
    let Ok(mut m) = CLAIMED.lock() else { return };
    for line in text.lines() {
        if let Some((d, at)) = line.split_once(' ')
            && let (Ok(d), Ok(at)) = (d.parse::<i64>(), at.parse::<i64>())
        {
            let e = m.entry(d).or_insert(at);
            *e = (*e).max(at);
        }
    }
}

/// The claimed days into the store, when they changed since `last` (what was written then). Returns what's there now.
fn save_claimed(root: &Path, last: &str) -> Result<String> {
    let text: String =
        CLAIMED.lock().map(|m| m.iter().map(|(d, at)| format!("{d} {at}\n")).collect()).unwrap_or_default();
    if text != last {
        write_replace(&root.join(CLAIMED_FILE), text.as_bytes())?;
    }
    Ok(text)
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
/// behind (counted, and said every sweep), rather than hold the fetch up.
pub fn record(server: &str, group: &str, lines: &[Vec<u8>]) {
    let Some(lock) = SENDER.get() else { return };
    // the sender out of the lock, soo the copy below holds no other connection up
    let Some(tx) = lock.lock().ok().and_then(|g| g.clone()) else { return };
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

fn now_secs() -> i64 {
    SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

fn run(mut window: Window, rx: Receiver<Msg>, keep_hours: i64) {
    let started = Instant::now();
    let mut last_sweep = Instant::now();
    let (mut said_dropped, mut said_failed) = (0, 0);
    // claimed-days as last written, soo an unchanged set isn't written again (each claim changes it, as a
    // timestamp, which a write per sweep carries)
    let mut saved = String::new();
    loop {
        match rx.recv_timeout(Duration::from_secs(1)) {
            Ok(Msg::Batch { server, group, lines }) => {
                // the ranges as of now: a day claimed a moment ago is taken in at once
                let now = now_secs();
                let (take, _) = ranges(now, keep_hours, &claimed_days(now), false);
                match window.record_days(&server, &group, &lines, &take) {
                    Ok((skipped, days)) => {
                        RECORDED.fetch_add((lines.len() - skipped) as u64, Ordering::Relaxed);
                        refresh_claimed(&days, now);
                    }
                    Err(e) => {
                        FAILED.fetch_add(1, Ordering::Relaxed);
                        eprintln!("header window: couldnt record {group}: {e:#}");
                    }
                }
            }
            Ok(Msg::Finish(done)) => {
                let _ = save_claimed(&window.root, &saved);
                let _ = done.send(());
                return;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
        }
        if last_sweep.elapsed() < Duration::from_secs(10) {
            continue;
        }
        last_sweep = Instant::now();
        let now = now_secs();
        let (_, keep) = ranges(now, keep_hours, &claimed_days(now), started.elapsed() < START_GRACE);
        if let Ok(now_saved) = save_claimed(&window.root, &saved) {
            saved = now_saved;
        }
        // expired first: what's going needn't be sealed
        if let Err(e) = window.expire(&keep).and_then(|_| window.seal_idle(SEAL_IDLE, SEAL_BUDGET)) {
            FAILED.fetch_add(1, Ordering::Relaxed);
            eprintln!("header window: couldnt seal or expire: {e:#}");
        }
        let (dropped, _, failed) = counters();
        if dropped > said_dropped || failed > said_failed {
            eprintln!(
                "header window: {} batches dropped (writer behind) and {} writes failed since the last word",
                dropped - said_dropped,
                failed - said_failed
            );
            (said_dropped, said_failed) = (dropped, failed);
        }
    }
}

/// `atlas --header-window [stats | census [BLOCKS] | POSTER FROM TO]`: reads the window, never a server. FROM and TO
/// are unix seconds.
pub fn run_cli(args: &[String]) -> i32 {
    let cfg = crate::config::header_window_config();
    let window = match Window::open_read_only(&cfg.root()) {
        Ok(w) => w,
        Err(e) => {
            println!("couldnt open {}: {e:#}", cfg.root().display());
            return 1;
        }
    };
    let rest: Vec<&String> = args.iter().skip_while(|a| *a != "--header-window").skip(1).collect();
    if rest.first().is_some_and(|a| *a == "census") {
        let want = rest.get(1).and_then(|n| n.parse().ok()).unwrap_or(400);
        return match crate::nxv_census::run(&cfg.root(), want) {
            Ok(()) => 0,
            Err(e) => {
                println!("census failed: {e:#}");
                1
            }
        };
    }
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
            println!("usage: atlas --header-window [stats | census [BLOCKS] | POSTER FROM TO]");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: &[(i64, i64)] = &[(i64::MIN, i64::MAX)];

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
            assert_eq!(w.record("news.example", g, &lines, ALL).unwrap(), 0);
        }
        // a line that isn't one, or outside every window, is left out
        assert_eq!(w.record("news.example", "alt.binaries.one", &[b"junk".to_vec()], ALL).unwrap(), 1);
        assert_eq!(w.record("news.example", "alt.binaries.one", &[line(7, ours, date, "o@x")], &[(0, 1)]).unwrap(), 1);
        let open = w.by_poster(ours.as_bytes(), t - 5, t + 5).unwrap();
        assert_eq!(open.len(), 8, "4 in each group while open");
        assert_eq!(w.seal_idle(Duration::ZERO, Duration::from_secs(60)).unwrap(), 2);
        let sealed = w.by_poster(ours.as_bytes(), t - 5, t + 5).unwrap();
        assert_eq!(sealed, open, "the same once sealed");
        assert!(w.by_poster(ours.as_bytes(), t + 10, t + 20).unwrap().is_empty(), "outside the time asked");
        assert!(w.by_poster(ours.as_bytes(), t + 5, t - 5).unwrap().is_empty(), "a range back to front");
        assert!(w.by_poster(b"nobody@x.y", t - 5, t + 5).unwrap().is_empty());
        // back after a restart, sealed and open alike
        w.record("news.example", "alt.binaries.one", &[line(200, ours, date, "late@x")], ALL).unwrap();
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
            w.record("s", "g", &[line(i as u64 + 1, "p@x", d, &format!("e{i}@x"))], ALL).unwrap();
        }
        w.seal_idle(Duration::ZERO, Duration::from_secs(60)).unwrap();
        w.record("s", "g", &[line(10, "p@x", dates[0], "open@x")], ALL).unwrap();
        let now = parse_rfc2822_to_epoch(dates[2].as_bytes()).unwrap();
        let backfill = parse_rfc2822_to_epoch(dates[1].as_bytes()).unwrap();
        let dropped = w.expire(&[(now - 24 * HOUR, now), (backfill - 12 * HOUR, backfill + 12 * HOUR)]).unwrap();
        assert_eq!(dropped, 2, "the old hour, sealed and open");
        assert_eq!(w.stats().2, 2);
        let files: Vec<_> = fs::read_dir(crate::overview_archive::group_dir(dir.path(), "s", "g")).unwrap().collect();
        assert_eq!(files.len(), 4, "two blocks and their indexes left");
    }

    #[test]
    fn a_tail_cut_short_is_cut_back_before_more_is_appended() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = Window::open(dir.path()).unwrap();
        let date = "Sun, 08 Nov 2026 07:00:01 +0000";
        let t = parse_rfc2822_to_epoch(date.as_bytes()).unwrap();
        w.record("s", "g", &[line(1, "p@x", date, "a@x")], ALL).unwrap();
        drop(w);
        // a crash in the middle of the next line
        let tail = crate::overview_archive::group_dir(dir.path(), "s", "g").join(format!("{}.tail", t / HOUR * HOUR));
        let mut f = fs::OpenOptions::new().append(true).open(&tail).unwrap();
        f.write_all(&500u32.to_le_bytes()).unwrap();
        f.write_all(b"12\tcut").unwrap();
        drop(f);
        let mut w = Window::open(dir.path()).unwrap();
        w.record("s", "g", &[line(2, "p@x", date, "b@x")], ALL).unwrap();
        drop(w);
        let w = Window::open(dir.path()).unwrap();
        assert_eq!(w.by_poster(b"p@x", t - 5, t + 5).unwrap().len(), 2, "both whole lines, the cut one gone");
    }

    #[test]
    fn the_window_takes_in_the_live_hours_and_the_days_around_each_day_claimed() {
        let now = 20_400 * 86_400 + 5 * HOUR;
        let live = (now - 24 * HOUR, now + HOUR);
        assert_eq!(ranges(now, 24, &[], false), (vec![live], vec![live]), "no backfill: the live hours only");
        let (take, keep) = ranges(now, 24, &[20_000, 19_990], false);
        assert_eq!(take, keep);
        assert_eq!(take[1], (19_999 * 86_400, 20_002 * 86_400 - 1));
        assert_eq!(take[2], (19_989 * 86_400, 19_992 * 86_400 - 1), "every host's day, not only the last");
        let (_, keep) = ranges(now, 24, &[], true);
        assert_eq!(keep, vec![(i64::MIN, now + HOUR)], "just started: nothing older dropped yet");
    }

    #[test]
    fn days_claimed_stay_for_a_while_and_come_back_after_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let day = 1_000_003;
        note_claimed_day(day);
        assert!(claimed_days(now_secs()).contains(&day));
        // a stray line beside it in a batch of other days doesn't keep it
        let later = now_secs() + 1000;
        refresh_claimed(&[(day + 1, 1), (day + 40, 99)], later);
        assert!(!claimed_days(now_secs() + CLAIMED_KEEP.as_secs() as i64 + 50).contains(&day), "a stray date");
        note_claimed_day(day);
        // still being read: a batch mostly of the day keeps it
        refresh_claimed(&[(day, 90), (day + 40, 10)], later);
        assert!(claimed_days(now_secs() + CLAIMED_KEEP.as_secs() as i64 + 50).contains(&day), "kept while read");
        save_claimed(dir.path(), "").unwrap();
        CLAIMED.lock().unwrap().remove(&day);
        load_claimed(dir.path());
        assert!(claimed_days(now_secs()).contains(&day));
        assert!(!claimed_days(now_secs() + 2 * CLAIMED_KEEP.as_secs() as i64).contains(&day), "then forgotten");
        CLAIMED.lock().unwrap().remove(&day);
    }

    #[test]
    fn a_torn_index_or_a_lone_block_is_cleared_and_the_window_opens() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = Window::open(dir.path()).unwrap();
        let date = "Sun, 08 Nov 2026 07:00:01 +0000";
        let t = parse_rfc2822_to_epoch(date.as_bytes()).unwrap();
        w.record("s", "g", &[line(1, "p@x", date, "a@x")], ALL).unwrap();
        w.record("s", "h", &[line(1, "p@x", date, "b@x")], ALL).unwrap();
        w.seal_idle(Duration::ZERO, Duration::from_secs(60)).unwrap();
        drop(w);
        let hour = t / HOUR * HOUR;
        let g = crate::overview_archive::group_dir(dir.path(), "s", "g");
        fs::write(g.join(format!("{hour}-0.idx")), b"HW").unwrap();
        let h = crate::overview_archive::group_dir(dir.path(), "s", "h");
        fs::remove_file(h.join(format!("{hour}-0.idx"))).unwrap();
        // read only: nothing cleared
        let r = Window::open_read_only(dir.path()).unwrap();
        assert!(g.join(format!("{hour}-0.nxv")).exists());
        assert!(r.by_poster(b"p@x", t - 5, t + 5).unwrap().is_empty());
        let w = Window::open(dir.path()).unwrap();
        assert!(!g.join(format!("{hour}-0.nxv")).exists(), "the torn pair cleared");
        assert!(!h.join(format!("{hour}-0.nxv")).exists(), "the lone block cleared");
        assert_eq!(w.stats(), (0, 0, 0));
    }

    #[test]
    fn names_decode_as_the_archive_encodes_them() {
        let root = Path::new("/r");
        for name in ["alt.binaries.x", "news.example:563#2", "a b/c", ""] {
            let d = crate::overview_archive::group_dir(root, "s", name);
            assert_eq!(decode(&d).as_deref(), Some(name), "{}", d.display());
        }
    }
}
