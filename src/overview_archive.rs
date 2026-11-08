//! The overview archive: every XOVER row Atlas fetches, kept byte for byte as the server sent it, outside SQLite,
//! in `nntp-xover-engine` `.nxv` blocks. A shard reset or a new parser can read a group's headers back from here
//! instead of asking the provider again, and `atlas --overview` answers XOVER ranges and OVER <message-id> from it.
//!
//! Article numbers belong to a server, soo the archive is kept per server and group:
//! `overview/<server>/<group>/`, holding
//! - `tail.nxv`: one small hot block per fetched batch, appended as batches come in
//! - `seg-<first>-<last>-<n>.nxv`: full blocks of 65,536 rows in article order, made from the tail once it holds
//!   that many. All but the `keep_hot` with the highest numbers are turned cold: zstd, with the header,
//!   checkpoints and message-id filter left uncompressed soo a lookup still skips the blocks that can't hold it.
//!
//! Writes happen on one thread fed by a bounded channel, soo a fetch never waits on compression; a batch that finds
//! the channel full is dropped (and counted) rather than slowing indexing. Sealed segments are fsynced before the
//! tail they came from is replaced, and a crash between the two leaves rows in both, which a read drops (rows are
//! deduplicated by their bytes). The tail itself isn't fsynced per batch: a crash can lose the last batches, which
//! the indexer fetched and stored in the shards already.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use memmap2::Mmap;
use nntp_xover_engine::container::DATA_START;
use nntp_xover_engine::{
    AlignedBytes, NxvBlockBuilder, NxvBlockView, NxvRangeCursor, RowCache, XoverStreamScratch, compress_block_to_cold,
    decompress_cold_block, peek_header,
};

use crate::config::OverviewArchiveConfig;

/// Rows of a full block, and when the tail is sealed into segments.
pub const SEGMENT_ROWS: usize = 65_536;
const QUEUE_BATCHES: usize = 256;
const TAIL: &str = "tail.nxv";

enum Msg {
    Batch { server: String, group: String, lines: Vec<Vec<u8>> },
    Finish(SyncSender<()>),
}

static SENDER: OnceLock<Mutex<Option<SyncSender<Msg>>>> = OnceLock::new();
static DROPPED: AtomicU64 = AtomicU64::new(0);
static WRITTEN: AtomicU64 = AtomicU64::new(0);

/// Batches dropped because the writer was behind, and rows written, since start.
pub fn counters() -> (u64, u64) {
    (DROPPED.load(Ordering::Relaxed), WRITTEN.load(Ordering::Relaxed))
}

/// Starts the writer when the archive is on. Call once, from the indexer.
pub fn spawn(cfg: &OverviewArchiveConfig) {
    if !cfg.enabled {
        return;
    }
    let root = cfg.root();
    let (tx, rx) = sync_channel(QUEUE_BATCHES);
    if SENDER.set(Mutex::new(Some(tx))).is_err() {
        return;
    }
    let mut writer = Writer { root, keep_hot: cfg.keep_hot, zstd_level: cfg.zstd_level, tails: HashMap::new() };
    let _ = std::thread::Builder::new().name("overview-archive".into()).spawn(move || writer.run(rx));
}

/// Hands one fetched XOVER batch to the writer. Does nothing when the archive is off.
pub fn record(server: &str, group: &str, lines: &[Vec<u8>]) {
    let Some(lock) = SENDER.get() else { return };
    let Ok(guard) = lock.lock() else { return };
    let Some(tx) = guard.as_ref() else { return };
    if lines.is_empty() {
        return;
    }
    let msg = Msg::Batch { server: server.to_string(), group: group.to_string(), lines: lines.to_vec() };
    if let Err(TrySendError::Full(_)) = tx.try_send(msg) {
        DROPPED.fetch_add(1, Ordering::Relaxed);
    }
}

/// Waits (up to `timeout`) for the writer to write what's queued, then stops taking batches. For shutdown.
pub fn finish(timeout: Duration) {
    let Some(lock) = SENDER.get() else { return };
    let tx = lock.lock().ok().and_then(|mut g| g.take());
    let Some(tx) = tx else { return };
    let deadline = std::time::Instant::now() + timeout;
    let (done_tx, done_rx) = sync_channel(1);
    // a full queue is waited on only until the deadline: the writer may be stuck on a slow disk
    let mut msg = Msg::Finish(done_tx);
    loop {
        match tx.try_send(msg) {
            Ok(()) => break,
            Err(TrySendError::Full(m)) if std::time::Instant::now() < deadline => {
                msg = m;
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(_) => return,
        }
    }
    let _ = done_rx.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()));
}

/// A path component from a server or group name: anything outside `[A-Za-z0-9._+-]` becomes `_`, and a name
/// that is all dots can't climb out of the archive.
fn component(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-') { c } else { '_' })
        .collect();
    if s.is_empty() || s.chars().all(|c| c == '.') { format!("_{s}") } else { s }
}

pub fn group_dir(root: &Path, server: &str, group: &str) -> PathBuf {
    root.join(component(server)).join(component(group))
}

/// Rows that can go in a block: they start with an article number (XOVER's always do; the rest is noise).
fn usable(lines: &[Vec<u8>]) -> Vec<&[u8]> {
    lines
        .iter()
        .map(|l| l.as_slice())
        .filter(|l| {
            let n = l.iter().take_while(|b| b.is_ascii_digit()).count();
            n > 0 && n <= 18 && (l.len() == n || l[n] == b'\t') && !l.contains(&b'\n') && !l.contains(&b'\r')
        })
        .collect()
}

/// The blocks of a file's bytes, in order, up to the first one that doesn't check out. Returns their (offset, len)
/// and where the good part ends.
fn scan_blocks(bytes: &[u8]) -> (Vec<(usize, usize)>, usize) {
    let mut out = Vec::new();
    let mut off = 0usize;
    while off + DATA_START <= bytes.len() {
        let Ok(h) = peek_header(&bytes[off..]) else { break };
        let len = h.total_block_bytes as usize;
        if len < DATA_START || !len.is_multiple_of(64) || off + len > bytes.len() {
            break;
        }
        out.push((off, len));
        off += len;
    }
    (out, off)
}

fn sync_dir(dir: &Path) {
    if let Ok(d) = File::open(dir) {
        let _ = d.sync_all();
    }
}

/// Writes `bytes` to `path` through a temporary file, fsynced, then renamed into place.
fn write_atomically(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("nxv.tmp");
    {
        let mut f = File::create(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

/// Every row of every block in `bytes` (a tail or a segment file), as raw lines.
fn rows_of(bytes: &[u8]) -> Result<Vec<Vec<u8>>> {
    let (blocks, _) = scan_blocks(bytes);
    let mut rows = Vec::new();
    for (off, len) in blocks {
        let al = AlignedBytes::from_slice(&bytes[off..off + len]);
        let hot = if peek_header(&al)?.flags & nntp_xover_engine::container::FLAG_COLD != 0 {
            decompress_cold_block(&al)?
        } else {
            al
        };
        let v = NxvBlockView::from_mmap_slice(&hot)?;
        let mut cache = RowCache::default();
        for r in 0..v.row_count() {
            let mut line = Vec::new();
            v.reconstruct_row(r, &mut cache, &mut line)?;
            rows.push(line);
        }
    }
    Ok(rows)
}

fn art_of(line: &[u8]) -> u64 {
    line.iter()
        .take_while(|b| b.is_ascii_digit())
        .fold(0u64, |a, &b| a.saturating_mul(10).saturating_add((b - b'0') as u64))
}

struct Writer {
    root: PathBuf,
    keep_hot: usize,
    zstd_level: i32,
    /// rows in each group's tail, once looked at
    tails: HashMap<PathBuf, usize>,
}

impl Writer {
    fn run(&mut self, rx: Receiver<Msg>) {
        loop {
            match rx.recv_timeout(Duration::from_secs(5)) {
                Ok(Msg::Batch { server, group, lines }) => {
                    if let Err(e) = self.add(&server, &group, &lines) {
                        eprintln!("overview archive: {server} {group}: {e:#}");
                    }
                }
                Ok(Msg::Finish(done)) => {
                    // write what is still queued, then stop
                    while let Ok(Msg::Batch { server, group, lines }) = rx.try_recv() {
                        let _ = self.add(&server, &group, &lines);
                    }
                    let _ = done.send(());
                    return;
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    }

    /// Appends a batch to its group's tail, and seals the tail once it holds a segment's worth.
    fn add(&mut self, server: &str, group: &str, lines: &[Vec<u8>]) -> Result<()> {
        let rows = usable(lines);
        if rows.is_empty() {
            return Ok(());
        }
        let dir = group_dir(&self.root, server, group);
        let held = match self.tails.get(&dir) {
            Some(&n) => n,
            None => {
                fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
                let n = recover_tail(&dir)?;
                self.tails.insert(dir.clone(), n);
                n
            }
        };
        let mut f = OpenOptions::new().create(true).append(true).open(dir.join(TAIL))?;
        for chunk in rows.chunks(SEGMENT_ROWS) {
            f.write_all(&NxvBlockBuilder::build_hot_block(chunk)?)?;
        }
        drop(f);
        WRITTEN.fetch_add(rows.len() as u64, Ordering::Relaxed);
        let held = held + rows.len();
        self.tails.insert(dir.clone(), held);
        if held >= SEGMENT_ROWS {
            let left = seal(&dir, self.keep_hot, self.zstd_level)?;
            self.tails.insert(dir, left);
        }
        Ok(())
    }
}

/// Cuts a tail with a block written halfway (a crash) back to its last whole block, and counts its rows.
fn recover_tail(dir: &Path) -> Result<usize> {
    let path = dir.join(TAIL);
    let Ok(bytes) = fs::read(&path) else { return Ok(0) };
    let (blocks, good) = scan_blocks(&bytes);
    if good < bytes.len() {
        OpenOptions::new().write(true).open(&path)?.set_len(good as u64)?;
    }
    let mut rows = 0;
    for (off, _) in blocks {
        rows += peek_header(&bytes[off..])?.row_count as usize;
    }
    Ok(rows)
}

fn segment_files(dir: &Path) -> Vec<(u64, PathBuf)> {
    let mut v: Vec<(u64, PathBuf)> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let first = name.strip_prefix("seg-")?.strip_suffix(".nxv")?.split('-').next()?.parse().ok()?;
            Some((first, e.path()))
        })
        .collect();
    v.sort();
    v
}

/// Turns the tail's full segments' worth of rows into segment files, keeps the rest as the new tail, and makes
/// all but the newest `keep_hot` segments cold. Returns the rows left in the tail.
pub fn seal(dir: &Path, keep_hot: usize, zstd_level: i32) -> Result<usize> {
    let tail_path = dir.join(TAIL);
    let bytes = fs::read(&tail_path)?;
    let mut rows = rows_of(&bytes)?;
    rows.sort_by(|a, b| art_of(a).cmp(&art_of(b)).then_with(|| a.cmp(b)));
    rows.dedup();
    let full = rows.len() / SEGMENT_ROWS * SEGMENT_ROWS;
    let stamp = chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0);
    for (i, chunk) in rows[..full].chunks(SEGMENT_ROWS).enumerate() {
        let refs: Vec<&[u8]> = chunk.iter().map(|l| l.as_slice()).collect();
        let block = NxvBlockBuilder::build_hot_block(&refs)?;
        let name = format!(
            "seg-{:020}-{:020}-{stamp}{i}.nxv",
            art_of(chunk[0].as_slice()),
            art_of(chunk[chunk.len() - 1].as_slice())
        );
        write_atomically(&dir.join(name), &block)?;
    }
    sync_dir(dir);
    let rest = &rows[full..];
    if rest.is_empty() {
        fs::remove_file(&tail_path)?;
    } else {
        let refs: Vec<&[u8]> = rest.iter().map(|l| l.as_slice()).collect();
        write_atomically(&tail_path, &NxvBlockBuilder::build_hot_block(&refs)?)?;
    }
    sync_dir(dir);
    cool(dir, keep_hot, zstd_level)?;
    Ok(rest.len())
}

/// Whether a segment file's blocks are all cold, from their headers (a segment holds one block, soo this
/// usually reads 320 bytes).
fn is_cold_file(path: &Path) -> Result<bool> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = File::open(path)?;
    let len = f.metadata()?.len();
    let mut off = 0u64;
    let mut head = [0u8; DATA_START];
    while off < len {
        f.seek(SeekFrom::Start(off))?;
        f.read_exact(&mut head)?;
        let h = peek_header(&head)?;
        if h.flags & nntp_xover_engine::container::FLAG_COLD == 0 {
            return Ok(false);
        }
        off += h.total_block_bytes.max(64) as u64;
    }
    Ok(true)
}

/// Makes every segment but the `keep_hot` with the highest article numbers cold.
pub fn cool(dir: &Path, keep_hot: usize, zstd_level: i32) -> Result<()> {
    let segs = segment_files(dir);
    let n = segs.len().saturating_sub(keep_hot);
    for (_, path) in &segs[..n] {
        // a cold segment stays as it is: look at its header only, not the whole file
        if is_cold_file(path)? {
            continue;
        }
        let bytes = fs::read(path)?;
        let (blocks, good) = scan_blocks(&bytes);
        if good != bytes.len() {
            bail!("{} is damaged", path.display());
        }
        let mut out = Vec::with_capacity(bytes.len() / 2);
        let mut changed = false;
        for (off, len) in blocks {
            let block = &bytes[off..off + len];
            if peek_header(block)?.flags & nntp_xover_engine::container::FLAG_COLD != 0 {
                out.extend_from_slice(block);
            } else {
                out.extend_from_slice(&compress_block_to_cold(block, zstd_level)?);
                changed = true;
            }
        }
        if changed {
            write_atomically(path, &out)?;
        }
    }
    sync_dir(dir);
    Ok(())
}

// ---------------------------------------------------------------- reading

/// One group's archive, mapped read only.
pub struct GroupArchive {
    files: Vec<(PathBuf, Mmap)>,
}

/// Sizes of one group's archive.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Stats {
    pub files: usize,
    pub blocks: usize,
    pub cold_blocks: usize,
    pub rows: u64,
    pub bytes: u64,
    pub first: Option<u64>,
    pub last: Option<u64>,
}

impl GroupArchive {
    pub fn open(root: &Path, server: &str, group: &str) -> Result<Self> {
        let dir = group_dir(root, server, group);
        let mut paths: Vec<PathBuf> = segment_files(&dir).into_iter().map(|(_, p)| p).collect();
        let tail = dir.join(TAIL);
        if tail.exists() {
            paths.push(tail);
        }
        let mut files = Vec::new();
        for p in paths {
            let f = File::open(&p).with_context(|| format!("opening {}", p.display()))?;
            if f.metadata()?.len() == 0 {
                continue;
            }
            // SAFETY: the archive's files are only ever replaced by rename or appended to, never rewritten in
            // place, soo the mapped bytes don't change under us (a block appended after mapping isn't seen).
            let map = unsafe { Mmap::map(&f)? };
            files.push((p, map));
        }
        Ok(Self { files })
    }

    /// Each block, cold or hot, read in place.
    fn blocks(&self) -> impl Iterator<Item = NxvBlockView<'_>> {
        self.files.iter().flat_map(|(_, map)| {
            let (blocks, _) = scan_blocks(map);
            blocks.into_iter().filter_map(move |(off, len)| NxvBlockView::from_mmap_slice(&map[off..off + len]).ok())
        })
    }

    pub fn stats(&self) -> Stats {
        let mut s = Stats { files: self.files.len(), ..Default::default() };
        for v in self.blocks() {
            s.blocks += 1;
            s.cold_blocks += v.is_cold() as usize;
            s.rows += v.row_count() as u64;
            s.bytes += v.data.len() as u64;
            let (a, b) = (v.header.min_art_num as u64, v.header.max_art_num as u64);
            s.first = Some(s.first.map_or(a, |f| f.min(a)));
            s.last = Some(s.last.map_or(b, |l| l.max(b)));
        }
        s
    }

    /// The rows numbered `start..=end`, in order, each once.
    pub fn range(&self, start: u64, end: u64) -> Result<Vec<Vec<u8>>> {
        let mut out = Vec::new();
        let mut scratch = XoverStreamScratch::new();
        for v in self.blocks().filter(|v| v.overlaps_art_range(start, end)) {
            let hot_owned;
            let hv = if v.is_cold() {
                hot_owned = decompress_cold_block(v.data)?;
                NxvBlockView::from_mmap_slice(&hot_owned)?
            } else {
                v
            };
            let mut cur = NxvRangeCursor::new_for_art_range(hv, start, end)?;
            loop {
                scratch.wire_buf.clear();
                if !cur.format_next_row(&mut scratch) {
                    break;
                }
                let n = scratch.wire_buf.len() - 2;
                out.push(scratch.wire_buf[..n].to_vec());
            }
        }
        out.sort_by(|a, b| art_of(a).cmp(&art_of(b)).then_with(|| a.cmp(b)));
        out.dedup();
        Ok(out)
    }

    /// The row with this message-id (angle brackets included), if any block has it.
    pub fn find(&self, msgid: &[u8]) -> Result<Option<Vec<u8>>> {
        let mut cache = RowCache::default();
        for v in self.blocks().filter(|v| v.may_contain_msgid(msgid)) {
            let hot_owned;
            let hv = if v.is_cold() {
                hot_owned = decompress_cold_block(v.data)?;
                NxvBlockView::from_mmap_slice(&hot_owned)?
            } else {
                v
            };
            if let Some(row) = hv.find_row_by_msgid_with(msgid, &mut cache) {
                let mut line = Vec::new();
                hv.reconstruct_row(row, &mut cache, &mut line)?;
                return Ok(Some(line));
            }
        }
        Ok(None)
    }
}

/// `atlas --overview SERVER GROUP [stats | START-END | <message-id>]`: reads the archive, never the provider.
pub fn run_cli(args: &[String]) -> i32 {
    let cfg = crate::config::overview_archive_config();
    let rest: Vec<&String> = args.iter().skip_while(|a| *a != "--overview").skip(1).collect();
    let (Some(server), Some(group)) = (rest.first(), rest.get(1)) else {
        eprintln!("usage: atlas --overview SERVER GROUP [stats | START-END | <message-id>]");
        return 2;
    };
    let arch = match GroupArchive::open(&cfg.root(), server, group) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e:#}");
            return 1;
        }
    };
    let what = rest.get(2).map(|s| s.as_str()).unwrap_or("stats");
    let mut stdout = std::io::stdout().lock();
    let r: Result<()> = (|| {
        if what == "stats" {
            let s = arch.stats();
            writeln!(
                stdout,
                "{} rows in {} blocks ({} cold) across {} files, {:.1} MB, {:.1} bytes a row, articles {}-{}",
                s.rows,
                s.blocks,
                s.cold_blocks,
                s.files,
                s.bytes as f64 / 1e6,
                s.bytes as f64 / s.rows.max(1) as f64,
                s.first.unwrap_or(0),
                s.last.unwrap_or(0)
            )?;
        } else if what.starts_with('<') {
            match arch.find(what.as_bytes())? {
                Some(line) => {
                    stdout.write_all(&line)?;
                    stdout.write_all(b"\n")?;
                }
                None => bail!("{what} isn't in the archive"),
            }
        } else {
            let (a, b) = what.split_once('-').context("START-END")?;
            let (a, b): (u64, u64) = (a.parse()?, if b.is_empty() { u64::MAX } else { b.parse()? });
            for line in arch.range(a, b)? {
                stdout.write_all(&line)?;
                stdout.write_all(b"\n")?;
            }
        }
        Ok(())
    })();
    match r {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("{e:#}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(n: u64) -> Vec<u8> {
        format!(
            "{n}\t[01/10] - \"Show.S01E{:02}.part{:02}.rar\" yEnc ({}/50)\tposter <p@x.y>\tSat, 07 Nov 2026 12:{:02}:{:02} +0000\t<{:016x}.{:08x}@news.example>\t\t{}\t{}\tXref: h alt.binaries.test:{n}",
            n % 30,
            n % 7,
            n % 50 + 1,
            n / 60 % 60,
            n % 60,
            n.wrapping_mul(0x9e37_79b9_7f4a_7c15),
            n,
            700_000 + n % 3,
            5_400 + n % 3
        )
        .into_bytes()
    }

    #[test]
    fn batches_seal_into_segments_and_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = Writer { root: dir.path().to_path_buf(), keep_hot: 1, zstd_level: 3, tails: HashMap::new() };
        // backfill downward and realtime upward, with a batch fetched twice
        let mut all: Vec<Vec<u8>> = Vec::new();
        let batches: Vec<std::ops::Range<u64>> = vec![
            100_000..130_000,
            70_000..100_000,
            130_000..170_000,
            40_000..70_000,
            130_000..131_000,
            170_000..175_000,
        ];
        for r in &batches {
            let lines: Vec<Vec<u8>> = r.clone().map(line).collect();
            w.add("news.example.com", "alt.binaries.test", &lines).unwrap();
        }
        for n in 40_000..175_000 {
            all.push(line(n));
        }
        let gdir = group_dir(dir.path(), "news.example.com", "alt.binaries.test");
        let segs = segment_files(&gdir);
        assert!(segs.len() >= 2, "{segs:?}");
        let a = GroupArchive::open(dir.path(), "news.example.com", "alt.binaries.test").unwrap();
        let s = a.stats();
        assert!(s.cold_blocks >= 1, "{s:?}");
        assert_eq!(s.first, Some(40_000));
        assert_eq!(s.last, Some(174_999));
        assert_eq!(a.range(0, u64::MAX).unwrap(), all);
        assert_eq!(a.range(99_990, 100_010).unwrap(), all[59_990..60_011].to_vec());
        for n in [40_000u64, 99_999, 150_123, 174_999] {
            let l = line(n);
            let m = l.split(|&b| b == b'\t').nth(4).unwrap();
            assert_eq!(a.find(m).unwrap(), Some(l));
        }
        assert_eq!(a.find(b"<nope@x>").unwrap(), None);
    }

    #[test]
    fn a_tail_cut_halfway_is_trimmed_to_its_last_whole_block() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = Writer { root: dir.path().to_path_buf(), keep_hot: 1, zstd_level: 3, tails: HashMap::new() };
        w.add("s", "g", &(1..100).map(line).collect::<Vec<_>>()).unwrap();
        w.add("s", "g", &(100..200).map(line).collect::<Vec<_>>()).unwrap();
        let tail = group_dir(dir.path(), "s", "g").join(TAIL);
        let len = fs::metadata(&tail).unwrap().len();
        OpenOptions::new().write(true).open(&tail).unwrap().set_len(len - 100).unwrap();
        let gdir = group_dir(dir.path(), "s", "g");
        assert_eq!(recover_tail(&gdir).unwrap(), 99);
        let a = GroupArchive::open(dir.path(), "s", "g").unwrap();
        assert_eq!(a.range(0, u64::MAX).unwrap(), (1..100).map(line).collect::<Vec<_>>());
    }

    #[test]
    fn names_stay_inside_the_archive() {
        assert_eq!(component(".."), "_..");
        assert_eq!(component("a/b"), "a_b");
        assert_eq!(component("alt.binaries.x"), "alt.binaries.x");
        assert!(usable(&[b"12\tx".to_vec(), b"x\t1".to_vec(), b"".to_vec(), b"7".to_vec()]).len() == 2);
    }
}
