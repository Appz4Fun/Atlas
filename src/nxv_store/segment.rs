//! One group's headers of one UTC day: `<day>.nxv`, `.nxv` blocks back to back on 64-byte boundaries, and
//! `<day>.nxvi`, a 64-byte entry per block in commit order. Blocks are only ever appended, by the store's single
//! committer, which reserves each block's space under the segment's writer lock before writing it, soo two blocks
//! never overlap and a crash never leaves a hole. Readers map the arena and see a block once its entry is published.

use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use memmap2::{Mmap, MmapOptions};
use nntp_xover_engine::container::NxvBlockView;

use super::sync::{fsync_only, full_flush};

pub const ENTRY_LEN: usize = 64;
const DAY: i64 = 86_400;

/// A block's manifest entry. Laid out by hand, little-endian:
/// `offset u64 | len u32 | rows u32 | min_art i64 | max_art i64 | min_ts i64 | max_ts i64 | seq u32 | slab_gen u32 |
/// server u16 | flags u16 | posters u32`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Entry {
    pub offset: u64,
    pub len: u32,
    pub rows: u32,
    pub min_art: i64,
    pub max_art: i64,
    pub min_ts: i64,
    pub max_ts: i64,
    /// its position in the manifest, checked at recovery
    pub seq: u32,
    pub slab_gen: u32,
    pub server: u16,
    pub flags: u16,
    pub posters: u32,
}

impl Entry {
    pub fn to_bytes(&self) -> [u8; ENTRY_LEN] {
        let mut b = [0u8; ENTRY_LEN];
        b[0..8].copy_from_slice(&self.offset.to_le_bytes());
        b[8..12].copy_from_slice(&self.len.to_le_bytes());
        b[12..16].copy_from_slice(&self.rows.to_le_bytes());
        b[16..24].copy_from_slice(&self.min_art.to_le_bytes());
        b[24..32].copy_from_slice(&self.max_art.to_le_bytes());
        b[32..40].copy_from_slice(&self.min_ts.to_le_bytes());
        b[40..48].copy_from_slice(&self.max_ts.to_le_bytes());
        b[48..52].copy_from_slice(&self.seq.to_le_bytes());
        b[52..56].copy_from_slice(&self.slab_gen.to_le_bytes());
        b[56..58].copy_from_slice(&self.server.to_le_bytes());
        b[58..60].copy_from_slice(&self.flags.to_le_bytes());
        b[60..64].copy_from_slice(&self.posters.to_le_bytes());
        b
    }

    pub fn from_bytes(b: &[u8]) -> Entry {
        let u32_at = |o: usize| u32::from_le_bytes(b[o..o + 4].try_into().expect("4 bytes"));
        let i64_at = |o: usize| i64::from_le_bytes(b[o..o + 8].try_into().expect("8 bytes"));
        Entry {
            offset: u64::from_le_bytes(b[0..8].try_into().expect("8 bytes")),
            len: u32_at(8),
            rows: u32_at(12),
            min_art: i64_at(16),
            max_art: i64_at(24),
            min_ts: i64_at(32),
            max_ts: i64_at(40),
            seq: u32_at(48),
            slab_gen: u32_at(52),
            server: u16::from_le_bytes([b[56], b[57]]),
            flags: u16::from_le_bytes([b[58], b[59]]),
            posters: u32_at(60),
        }
    }

    /// The entry for a block found at `offset` of a mapped arena, if a whole, intact block is there. Its time range is
    /// the block's clamped into `day`: a stray date stays in the row (lossless), and readers clamp a row's time into its
    /// entry's range.
    fn of_block(map: &[u8], offset: u64, seq: u32, day: i64) -> Option<Entry> {
        let at = usize::try_from(offset).ok()?;
        let view = NxvBlockView::from_mmap_slice(map.get(at..)?).ok()?;
        if !view.verify_body() {
            return None;
        }
        Some(Self::from_view(view, offset, seq, day))
    }

    fn from_view(view: NxvBlockView<'_>, offset: u64, seq: u32, day: i64) -> Entry {
        let h = view.header;
        let meta = view.store_meta();
        Entry {
            offset,
            len: h.total_block_bytes,
            rows: h.row_count,
            min_art: h.min_art_num,
            max_art: h.max_art_num,
            min_ts: h.min_timestamp.clamp(day * DAY, day * DAY + DAY - 1),
            max_ts: h.max_timestamp.clamp(day * DAY, day * DAY + DAY - 1),
            seq,
            slab_gen: meta.slab_gen,
            server: meta.server_id,
            flags: h.flags,
            posters: view.poster_count(),
        }
    }
}

/// The entry a new block gets, given where it goes (`offset`, `seq`); None when it isn't a block.
pub fn entry_for(block: &[u8], offset: u64, seq: u32, day: i64) -> Option<Entry> {
    let aligned = nntp_xover_engine::container::AlignedBytes::from_slice(block);
    let view = NxvBlockView::from_mmap_slice(&aligned).ok()?;
    if !view.verify_body() || (view.is_cold() && view.verify_cold_body().is_err()) {
        return None;
    }
    Some(Entry::from_view(view, offset, seq, day))
}

struct Writer {
    files: Option<(Arc<File>, Arc<File>)>,
    arena_tail: u64,
    index_tail: u64,
    next_seq: u32,
    /// commits holding space here: the idle closer and expiry leave the segment alone while any are
    in_flight: usize,
    last_write: Instant,
    expired: bool,
}

/// What readers see: the arena's map and the published entries.
pub struct View {
    pub map: Option<Arc<Mmap>>,
    pub entries: Vec<Entry>,
    /// (server, slab generation) of every committed block, soo recovery can tell a leftover log was committed
    pub committed: HashSet<(u16, u32)>,
}

pub struct Segment {
    /// days since the epoch (UTC)
    pub day: i64,
    pub arena_path: PathBuf,
    pub index_path: PathBuf,
    writer: Mutex<Writer>,
    view: RwLock<View>,
}

/// Space a commit reserved in a segment, with its own handles on the files.
pub struct Reserved {
    pub seg: Arc<Segment>,
    pub arena: Arc<File>,
    pub index: Arc<File>,
    pub entry: Entry,
}

/// What recovery found and did at open.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Healed {
    pub blocks: usize,
    pub rolled_forward: usize,
    pub dropped_entries: usize,
    pub cut_arena_bytes: u64,
    pub rewrote_manifest: bool,
}

pub fn day_name(day: i64) -> String {
    let (y, m, d) = nntp_xover_engine::date::civil_from_days(day);
    format!("{y:04}-{m:02}-{d:02}")
}

pub fn parse_day_name(name: &str) -> Option<i64> {
    let mut it = name.split('-');
    let y: i64 = it.next()?.parse().ok()?;
    let m: u32 = it.next()?.parse().ok()?;
    let d: u32 = it.next()?.parse().ok()?;
    if it.next().is_some() || !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    Some(nntp_xover_engine::date::days_from_civil(y, m, d))
}

impl Segment {
    /// Opens (making if need be) the segment of `day` in `dir`, healing what a crash left: a torn manifest tail, entries
    /// past the last intact block, intact blocks the manifest never got, and a torn arena tail.
    pub fn open(dir: &Path, day: i64) -> Result<(Arc<Segment>, Healed)> {
        std::fs::create_dir_all(dir).with_context(|| format!("making {}", dir.display()))?;
        let name = day_name(day);
        let arena_path = dir.join(format!("{name}.nxv"));
        let index_path = dir.join(format!("{name}.nxvi"));
        let rw = |p: &Path| OpenOptions::new().create(true).read(true).write(true).truncate(false).open(p);
        let arena = rw(&arena_path).with_context(|| format!("opening {}", arena_path.display()))?;
        let index = rw(&index_path).with_context(|| format!("opening {}", index_path.display()))?;
        let (entries, arena_len, healed) = heal(&arena, &index, day)?;
        let map = map_arena(&arena, arena_len)?;
        let committed = entries.iter().map(|e| (e.server, e.slab_gen)).collect();
        let seg = Segment {
            day,
            arena_path,
            index_path,
            writer: Mutex::new(Writer {
                files: None,
                arena_tail: arena_len,
                index_tail: (entries.len() * ENTRY_LEN) as u64,
                next_seq: entries.len() as u32,
                in_flight: 0,
                last_write: Instant::now(),
                expired: false,
            }),
            view: RwLock::new(View { map, entries, committed }),
        };
        Ok((Arc::new(seg), healed))
    }

    /// The published entries and the map they point into.
    pub fn snapshot(&self) -> (Option<Arc<Mmap>>, Vec<Entry>) {
        let v = self.view.read().unwrap_or_else(|e| e.into_inner());
        (v.map.clone(), v.entries.clone())
    }

    pub fn block_count(&self) -> usize {
        self.view.read().unwrap_or_else(|e| e.into_inner()).entries.len()
    }

    pub fn is_committed(&self, server: u16, slab_gen: u32) -> bool {
        self.view.read().unwrap_or_else(|e| e.into_inner()).committed.contains(&(server, slab_gen))
    }

    pub fn highest_slab_gen(&self) -> u32 {
        self.view.read().unwrap_or_else(|e| e.into_inner()).entries.iter().map(|e| e.slab_gen).max().unwrap_or(0)
    }

    /// Space for a block of `len` bytes at the end, and its entry, reserved at once (the tails move now, soo a second
    /// block for this segment in the same batch goes after it). None when the segment has expired.
    pub fn reserve(self: &Arc<Self>, block: &[u8]) -> Result<Option<Reserved>> {
        let mut w = self.writer.lock().unwrap_or_else(|e| e.into_inner());
        if w.expired {
            return Ok(None);
        }
        if w.files.is_none() {
            let open = |p: &Path| OpenOptions::new().read(true).write(true).open(p);
            let a = open(&self.arena_path).with_context(|| format!("opening {}", self.arena_path.display()))?;
            let i = open(&self.index_path).with_context(|| format!("opening {}", self.index_path.display()))?;
            w.files = Some((Arc::new(a), Arc::new(i)));
        }
        let offset = w.arena_tail;
        let seq = w.next_seq;
        let entry = entry_for(block, offset, seq, self.day).context("not an intact .nxv block")?;
        w.arena_tail += block.len() as u64;
        w.index_tail += ENTRY_LEN as u64;
        w.next_seq += 1;
        w.in_flight += 1;
        w.last_write = Instant::now();
        let (a, i) = w.files.clone().expect("opened above");
        Ok(Some(Reserved { seg: Arc::clone(self), arena: a, index: i, entry }))
    }

    /// A committed block made visible: the arena mapped again and its entry published. Blocks are published in the
    /// order they were reserved, soo the entries readers see are always a prefix of the manifest (the linker counts
    /// them).
    pub fn publish(&self, r: &Reserved) -> Result<()> {
        let end = r.entry.offset + r.entry.len as u64;
        let map = map_arena(&r.arena, end)?;
        {
            let mut v = self.view.write().unwrap_or_else(|e| e.into_inner());
            anyhow::ensure!(
                r.entry.seq as usize == v.entries.len(),
                "block {} of {} published out of order",
                r.entry.seq,
                self.arena_path.display()
            );
            v.map = map;
            v.committed.insert((r.entry.server, r.entry.slab_gen));
            v.entries.push(r.entry);
        }
        let mut w = self.writer.lock().unwrap_or_else(|e| e.into_inner());
        w.in_flight = w.in_flight.saturating_sub(1);
        w.last_write = Instant::now();
        Ok(())
    }

    /// Closes the writer's files when the segment hasn't been written for `idle` and no commit holds it.
    pub fn close_if_idle(&self, idle: Duration) -> bool {
        let mut w = self.writer.lock().unwrap_or_else(|e| e.into_inner());
        if w.in_flight == 0 && w.files.is_some() && w.last_write.elapsed() >= idle {
            w.files = None;
            return true;
        }
        false
    }

    /// Marks the segment expired and deletes its files, unless a commit holds it (then false: try again later).
    pub fn expire(&self) -> bool {
        let mut w = self.writer.lock().unwrap_or_else(|e| e.into_inner());
        if w.in_flight > 0 {
            return false;
        }
        w.expired = true;
        w.files = None;
        let _ = std::fs::remove_file(&self.arena_path);
        let _ = std::fs::remove_file(&self.index_path);
        true
    }
}

fn map_arena(arena: &File, len: u64) -> Result<Option<Arc<Mmap>>> {
    if len == 0 {
        return Ok(None);
    }
    // SAFETY: the arena is only appended to (by this process's committer) and never cut while mapped: recovery cuts
    // it before mapping, and expiry deletes rather than truncates
    let map = unsafe { MmapOptions::new().len(len as usize).map(arena)? };
    Ok(Some(Arc::new(map)))
}

/// Recovery at open: the entries to keep, the arena's good length, and what was done.
fn heal(arena: &File, index: &File, day: i64) -> Result<(Vec<Entry>, u64, Healed)> {
    let arena_len = arena.metadata()?.len();
    let index_len = index.metadata()?.len();
    let mut healed = Healed::default();
    let mut dirty = index_len % ENTRY_LEN as u64 != 0;
    let count = (index_len / ENTRY_LEN as u64) as usize;
    let mut raw = vec![0u8; count * ENTRY_LEN];
    if count > 0 {
        index.read_exact_at(&mut raw, 0)?;
    }
    let mut entries: Vec<Entry> = Vec::with_capacity(count);
    let mut end = 0u64;
    for (seq, chunk) in raw.as_chunks::<ENTRY_LEN>().0.iter().enumerate() {
        let e = Entry::from_bytes(chunk);
        let ok = e.offset == end
            && e.seq == seq as u32
            && e.len as usize >= nntp_xover_engine::container::DATA_START
            && e.len.is_multiple_of(64)
            && e.offset + e.len as u64 <= arena_len;
        if !ok {
            break;
        }
        end += e.len as u64;
        entries.push(e);
    }
    if entries.len() < count {
        healed.dropped_entries = count - entries.len();
        dirty = true;
    }
    let map = map_arena(arena, arena_len)?;
    let bytes: &[u8] = map.as_deref().map_or(&[], |m| &m[..]);
    // the last kept block whole and intact, or dropped
    if let Some(last) = entries.last().copied()
        && Entry::of_block(bytes, last.offset, last.seq, day).is_none()
    {
        entries.pop();
        healed.dropped_entries += 1;
        dirty = true;
        end = last.offset;
    }
    // blocks written whole that the manifest never got
    while end < arena_len {
        match Entry::of_block(bytes, end, entries.len() as u32, day) {
            Some(e) => {
                end += e.len as u64;
                entries.push(e);
                healed.rolled_forward += 1;
                dirty = true;
            }
            None => break,
        }
    }
    drop(map);
    if end < arena_len {
        healed.cut_arena_bytes = arena_len - end;
        arena.set_len(end)?;
        fsync_only(arena)?;
        full_flush(arena)?;
    }
    if dirty {
        // written over in place first, then cut: a crash between leaves a manifest with its old tail, never empty
        let mut out = Vec::with_capacity(entries.len() * ENTRY_LEN);
        for e in &entries {
            out.extend_from_slice(&e.to_bytes());
        }
        index.write_all_at(&out, 0)?;
        index.set_len(out.len() as u64)?;
        fsync_only(index)?;
        full_flush(index)?;
        healed.rewrote_manifest = true;
    }
    healed.blocks = entries.len();
    Ok((entries, end, healed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use nntp_xover_engine::container::{BlockMeta, NxvBlockBuilder};

    fn block(first_art: u64, n: u64, slab_gen: u32) -> Vec<u8> {
        let lines: Vec<Vec<u8>> = (0..n)
            .map(|k| {
                let a = first_art + k;
                format!(
                    "{a}\t[1/9] - \"x.part{k:02}.rar\" yEnc (1/50)\tposter{k}@x\tSun, 08 Nov 2026 10:00:{:02} +0000\t<m{a}@x>\t\t700000\t5000",
                    k % 60
                )
                .into_bytes()
            })
            .collect();
        let refs: Vec<&[u8]> = lines.iter().map(|l| l.as_slice()).collect();
        NxvBlockBuilder::build_hot_block_meta(&refs, BlockMeta { server_id: 3, slab_gen }).unwrap()
    }

    /// Commits one block the way the committer does, without the batching.
    fn commit(seg: &Arc<Segment>, b: &[u8]) -> Entry {
        let r = seg.reserve(b).unwrap().unwrap();
        r.arena.write_all_at(b, r.entry.offset).unwrap();
        r.index.write_all_at(&r.entry.to_bytes(), r.entry.seq as u64 * ENTRY_LEN as u64).unwrap();
        seg.publish(&r).unwrap();
        r.entry
    }

    #[test]
    fn day_names_round_trip() {
        let d = parse_day_name("2026-11-08").unwrap();
        assert_eq!(day_name(d), "2026-11-08");
        assert_eq!(parse_day_name("2026-13-01"), None);
        assert_eq!(parse_day_name("x"), None);
    }

    #[test]
    fn entries_round_trip() {
        let e = Entry {
            offset: 64,
            len: 640,
            rows: 9,
            min_art: -1,
            max_art: 7,
            min_ts: 1,
            max_ts: 2,
            seq: 4,
            slab_gen: 5,
            server: 6,
            flags: 7,
            posters: 8,
        };
        assert_eq!(Entry::from_bytes(&e.to_bytes()), e);
    }

    #[test]
    fn two_blocks_reserved_together_get_their_own_space_and_reopen_intact() {
        let dir = tempfile::tempdir().unwrap();
        let day = parse_day_name("2026-11-08").unwrap();
        let (seg, healed) = Segment::open(dir.path(), day).unwrap();
        assert_eq!(healed, Healed::default());
        let (b1, b2) = (block(100, 50, 1), block(200, 70, 2));
        // both reserved before either is written, as one batch does
        let r1 = seg.reserve(&b1).unwrap().unwrap();
        let r2 = seg.reserve(&b2).unwrap().unwrap();
        assert_eq!(r2.entry.offset, b1.len() as u64);
        assert_eq!((r1.entry.seq, r2.entry.seq), (0, 1));
        for (r, b) in [(&r1, &b1), (&r2, &b2)] {
            r.arena.write_all_at(b, r.entry.offset).unwrap();
            r.index.write_all_at(&r.entry.to_bytes(), r.entry.seq as u64 * 64).unwrap();
        }
        assert!(seg.publish(&r2).is_err(), "out of order");
        seg.publish(&r1).unwrap();
        seg.publish(&r2).unwrap();
        let (_, entries) = seg.snapshot();
        assert_eq!(entries.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![0, 1]);
        assert_eq!((entries[1].server, entries[1].slab_gen, entries[1].rows), (3, 2, 70));
        assert!(seg.is_committed(3, 1) && !seg.is_committed(3, 9));
        drop(seg);
        let (seg, healed) = Segment::open(dir.path(), day).unwrap();
        assert_eq!(healed, Healed { blocks: 2, ..Healed::default() });
        assert_eq!(seg.block_count(), 2);
        assert_eq!(seg.highest_slab_gen(), 2);
    }

    #[test]
    fn a_block_the_manifest_missed_is_rolled_forward_and_a_torn_tail_cut() {
        let dir = tempfile::tempdir().unwrap();
        let day = parse_day_name("2026-11-08").unwrap();
        let (seg, _) = Segment::open(dir.path(), day).unwrap();
        commit(&seg, &block(1, 10, 1));
        let e2 = commit(&seg, &block(20, 10, 2));
        drop(seg);
        let index = dir.path().join("2026-11-08.nxvi");
        let arena = dir.path().join("2026-11-08.nxv");
        // the second entry lost, half an entry left, and garbage after the last block
        let f = OpenOptions::new().write(true).open(&index).unwrap();
        f.set_len(64 + 30).unwrap();
        let a = OpenOptions::new().write(true).open(&arena).unwrap();
        let len = a.metadata().unwrap().len();
        a.write_all_at(&[0xAB; 350], len).unwrap();
        let (seg, healed) = Segment::open(dir.path(), day).unwrap();
        assert_eq!(healed.rolled_forward, 1);
        assert_eq!(healed.cut_arena_bytes, 350);
        assert!(healed.rewrote_manifest);
        let (_, entries) = seg.snapshot();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[1], e2);
        assert_eq!(std::fs::metadata(&index).unwrap().len(), 128);
    }

    #[test]
    fn a_damaged_last_block_is_dropped_with_its_entry() {
        let dir = tempfile::tempdir().unwrap();
        let day = parse_day_name("2026-11-08").unwrap();
        let (seg, _) = Segment::open(dir.path(), day).unwrap();
        commit(&seg, &block(1, 10, 1));
        let e2 = commit(&seg, &block(20, 10, 2));
        drop(seg);
        // a byte of the second block's body flipped
        let a = OpenOptions::new().read(true).write(true).open(dir.path().join("2026-11-08.nxv")).unwrap();
        let at = e2.offset + e2.len as u64 - 70;
        let mut b = [0u8; 1];
        a.read_exact_at(&mut b, at).unwrap();
        a.write_all_at(&[b[0] ^ 0xFF], at).unwrap();
        let (seg, healed) = Segment::open(dir.path(), day).unwrap();
        assert_eq!(healed.blocks, 1);
        assert_eq!(healed.dropped_entries, 1);
        assert_eq!(healed.cut_arena_bytes, e2.len as u64);
        assert_eq!(seg.block_count(), 1);
    }

    #[test]
    fn expiry_waits_for_a_commit_in_flight() {
        let dir = tempfile::tempdir().unwrap();
        let day = parse_day_name("2026-11-08").unwrap();
        let (seg, _) = Segment::open(dir.path(), day).unwrap();
        let b = block(1, 10, 1);
        let r = seg.reserve(&b).unwrap().unwrap();
        assert!(!seg.expire(), "a commit holds it");
        assert!(!seg.close_if_idle(Duration::ZERO), "a commit holds its files");
        r.arena.write_all_at(&b, r.entry.offset).unwrap();
        seg.publish(&r).unwrap();
        assert!(seg.expire());
        assert!(seg.reserve(&b).unwrap().is_none(), "expired: refused");
        assert!(!dir.path().join("2026-11-08.nxv").exists());
    }
}
