//! Actual startup I/O observations. The initial scaffold observes the existing
//! unpaced operations honestly: a whole manifest/body is one event, with no wait.
//! Clock replacement is scoped to the calling test thread and cannot affect a
//! production build, another test, or a store worker.
use std::cell::RefCell;
use std::fs::OpenOptions;
use std::io::{BufWriter, Write};
use std::rc::Rc;

use super::*;
use super::super::segment::{self, day_name};
use nntp_xover_engine::container::{AlignedBytes, DATA_START, NxvBlockView, compress_block_to_cold, crc32};

const RATE: u64 = 64 * 1024 * 1024;
const CHUNK: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::nxv_store) enum ReadKind { Index, Header, Metadata, Body, Log, Servers }
#[derive(Clone, Debug)]
struct Event { kind: ReadKind, bytes: usize, at: Duration }
#[derive(Clone, Default)]
struct Trace { events: Vec<Event>, sleeps: Vec<Duration>, early_wakes: usize }
struct Probe {
    start: Instant,
    elapsed: Duration,
    trace: Trace,
    idle_after: Option<usize>,
    wakes_left: usize,
    on_first: Option<Box<dyn FnOnce()>>,
}
thread_local! { static PROBE: RefCell<Option<Probe>> = const { RefCell::new(None) }; }
struct Scope { previous: Option<Probe> }
impl Scope {
    fn new(idle_after: Option<usize>, wakes_left: usize, on_first: Option<Box<dyn FnOnce()>>) -> Self {
        let probe = Probe { start: Instant::now(), elapsed: Duration::ZERO, trace: Trace::default(), idle_after, wakes_left, on_first };
        Self { previous: PROBE.with(|slot| slot.replace(Some(probe))) }
    }
    fn trace(&self) -> Trace { PROBE.with(|slot| slot.borrow().as_ref().unwrap().trace.clone()) }
}
impl Drop for Scope {
    fn drop(&mut self) { PROBE.with(|slot| { slot.replace(self.previous.take()); }); }
}

pub(in crate::nxv_store) fn observe(kind: ReadKind, bytes: usize) {
    let callback = PROBE.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some(probe) = slot.as_mut() else { return None };
        if probe.idle_after == Some(probe.trace.events.len()) {
            probe.elapsed += Duration::from_secs(60);
            probe.idle_after = None;
        }
        probe.trace.events.push(Event { kind, bytes, at: probe.elapsed });
        probe.on_first.take()
    });
    // Never retain a RefCell borrow across the actual recursive lock probe.
    if let Some(callback) = callback { callback(); }
}
pub(in crate::nxv_store) fn observe_metadata(len: usize) {
    // A conservative allowance for the parser's bounded metadata-page touches,
    // not a claim that an unvalidated whole mapped block was already read.
    // SAFETY: sysconf takes a valid constant and no borrowed pointers.
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    let page = usize::try_from(page).ok().filter(|p| (4096..=65536).contains(p)).unwrap_or(65536);
    observe(ReadKind::Metadata, len.min(8 * page));
}
// Scoped clock facade used by the recovery pacer's actual admission path.
// Only a probe on the calling test thread replaces wall-clock time and sleep.
#[allow(dead_code)]
pub(in crate::nxv_store) fn clock_now() -> Option<Instant> {
    PROBE.with(|slot| {
        let mut slot = slot.borrow_mut();
        let probe = slot.as_mut()?;
        if probe.idle_after == Some(probe.trace.events.len()) {
            probe.elapsed += Duration::from_secs(60);
            probe.idle_after = None;
        }
        Some(probe.start + probe.elapsed)
    })
}
#[allow(dead_code)]
pub(in crate::nxv_store) fn clock_sleep(wait: Duration) -> bool {
    PROBE.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some(probe) = slot.as_mut() else { return false };
        probe.trace.sleeps.push(wait);
        let slept = if probe.wakes_left > 0 && wait > Duration::from_nanos(1) {
            probe.wakes_left -= 1;
            probe.trace.early_wakes += 1;
            (wait / 2).max(Duration::from_nanos(1))
        } else { wait };
        probe.elapsed += slept;
        true
    })
}
fn cost(bytes: usize) -> Duration {
    Duration::from_nanos(((bytes as u128 * 1_000_000_000).div_ceil(u128::from(RATE))) as u64)
}
fn assert_paced(trace: &Trace) {
    assert!(!trace.events.is_empty(), "actual startup read events are required");
    let mut previous = Duration::ZERO;
    for event in &trace.events {
        assert!(event.bytes <= CHUNK, "unbounded actual {:?} operation: {} bytes", event.kind, event.bytes);
        assert!(event.at.saturating_sub(previous) >= cost(event.bytes),
            "unpaced actual {:?}: {} bytes at {:?}, previous {:?}", event.kind, event.bytes, event.at, previous);
        previous = event.at;
    }
    // Check arbitrary observed windows, independently of per-operation waits.
    for (start, first) in trace.events.iter().enumerate() {
        let mut bytes = 0u128;
        for last in &trace.events[start..] {
            bytes += last.bytes as u128;
            let allowance = u128::from(RATE) * last.at.saturating_sub(first.at).as_nanos() / 1_000_000_000 + CHUNK as u128;
            assert!(bytes <= allowance, "startup read window exceeded its rate plus one bounded chunk");
        }
    }
}
fn kind_bytes(trace: &Trace, kind: ReadKind) -> usize {
    trace.events.iter().filter(|e| e.kind == kind).map(|e| e.bytes).sum()
}
fn cfg(root: &Path) -> StoreConfig {
    StoreConfig { bounded_maps: true, sealers: 1, sync_every: Duration::from_secs(3600), seal_idle: Duration::from_secs(3600), ..StoreConfig::new(root) }
}
fn keep() -> KeepSet { KeepSet { live_from: i64::MIN / 2, ..Default::default() } }
fn day() -> i64 { parse_day_name("2026-11-08").unwrap() }
fn noise(seed: &mut u64, n: usize) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+-";
    (0..n).map(|_| {
        *seed ^= *seed << 13; *seed ^= *seed >> 7; *seed ^= *seed << 17;
        char::from(ALPHABET[(*seed & 63) as usize])
    }).collect()
}
fn line(art: u64, subject: &str) -> Vec<u8> {
    format!("{art}\t{subject}\tposter@x\tSun, 08 Nov 2026 10:00:00 +0000\t<m{art}@x>\t\t700000\t5000").into_bytes()
}
fn block(large: bool, generation: u32) -> Vec<u8> {
    let mut seed = 0x123456789abcdefu64;
    let lines: Vec<_> = if large {
        (1..=4096).map(|art| line(art, &noise(&mut seed, 768))).collect()
    } else { vec![line(1, "Small.Recovery.Block-GRP")] };
    let refs: Vec<_> = lines.iter().map(Vec::as_slice).collect();
    let block = NxvBlockBuilder::build_hot_block_meta(&refs, BlockMeta { server_id: 3, slab_gen: generation }).unwrap();
    let aligned = AlignedBytes::from_slice(&block);
    let view = NxvBlockView::from_mmap_slice(&aligned).unwrap();
    assert!(view.verify_body(), "real fixture checksum must pass");
    if large { assert!(block.len() - DATA_START > CHUNK, "real high-entropy fixture must exceed one hash chunk"); }
    block
}
fn paths(root: &Path, group: &str, day: i64) -> (PathBuf, PathBuf) {
    let dir = root.join(group);
    std::fs::create_dir_all(&dir).unwrap();
    (dir.join(format!("{}.nxv", day_name(day))), dir.join(format!("{}.nxvi", day_name(day))))
}
fn seed(root: &Path, group: &str, day: i64, blocks: &[&[u8]]) -> Vec<Entry> {
    let (arena, index) = paths(root, group, day);
    let mut arena = BufWriter::new(File::create(arena).unwrap());
    let mut index = BufWriter::new(File::create(index).unwrap());
    let mut offset = 0;
    let mut entries = Vec::new();
    for (seq, block) in blocks.iter().enumerate() {
        let entry = segment::entry_for(block, offset, seq as u32, day).unwrap();
        arena.write_all(block).unwrap(); index.write_all(&entry.to_bytes()).unwrap();
        offset += block.len() as u64; entries.push(entry);
    }
    arena.flush().unwrap(); index.flush().unwrap(); entries
}
fn entries(store: &Store, group: &str, day: i64) -> Vec<Entry> {
    store.groups.read().unwrap()[group].segments.read().unwrap()[&day].snapshot().1
}
fn check_block(store: &Store, group: &str, day: i64, entry: Entry, cold: bool) {
    let segment = store.groups.read().unwrap()[group].segments.read().unwrap()[&day].clone();
    assert!(segment.snapshot().0.is_none());
    let snapshot = segment.bounded_snapshot().unwrap();
    let map = snapshot.map_block(entry).unwrap();
    let view = NxvBlockView::from_mmap_slice(&map).unwrap();
    assert_eq!(view.is_cold(), cold);
    assert!(view.verify_body());
    if cold { view.verify_cold_body().unwrap(); }
}

#[test]
fn startup_recovery_paces_reads_across_groups_and_days() {
    let root = tempfile::tempdir().unwrap();
    let block = block(true, 7);
    let mut expected = Vec::new();
    for group in ["g", "h"] { for day in [day(), day() + 1] {
        expected.push((group, day, seed(root.path(), group, day, &[&block])));
    }}
    let scope = Scope::new(None, 0, None);
    let (store, opened) = Store::open(cfg(root.path()), keep()).unwrap();
    let trace = scope.trace(); drop(scope);
    assert_eq!(opened, Opened { segments: 4, blocks: 4, ..Default::default() });
    for (group, day, expected) in expected {
        assert_eq!(entries(&store, group, day), expected);
        check_block(&store, group, day, expected[0], false);
        let (arena, index) = paths(root.path(), group, day);
        assert_eq!(arena.metadata().unwrap().len(), block.len() as u64);
        assert_eq!(index.metadata().unwrap().len(), ENTRY_LEN as u64);
    }
    assert_eq!(kind_bytes(&trace, ReadKind::Index), 4 * ENTRY_LEN);
    assert_eq!(kind_bytes(&trace, ReadKind::Header), 4 * 64);
    assert_eq!(kind_bytes(&trace, ReadKind::Body), 4 * (block.len() - DATA_START));
    store.close().unwrap();
    eprintln!("NXV pacing primary: segments={} blocks={} index_bytes={} header_bytes={} body_bytes={} max_operation={} fake_elapsed_ns={}",
        opened.segments, opened.blocks, kind_bytes(&trace, ReadKind::Index), kind_bytes(&trace, ReadKind::Header), kind_bytes(&trace, ReadKind::Body),
        trace.events.iter().map(|e| e.bytes).max().unwrap(), trace.events.last().unwrap().at.as_nanos());
    assert_paced(&trace);
}

#[test]
fn startup_recovery_charges_manifest_header_and_slab_log_reads() {
    let root = tempfile::tempdir().unwrap();
    let small = block(false, 1);
    let count = CHUNK / ENTRY_LEN + 1;
    let (arena, index) = paths(root.path(), "g", day());
    let prototype = segment::entry_for(&small, 0, 0, day()).unwrap();
    let mut arena_out = BufWriter::new(File::create(&arena).unwrap());
    let mut index_out = BufWriter::new(File::create(&index).unwrap());
    for seq in 0..count {
        let entry = Entry { offset: seq as u64 * small.len() as u64, seq: seq as u32, ..prototype };
        arena_out.write_all(&small).unwrap(); index_out.write_all(&entry.to_bytes()).unwrap();
    }
    arena_out.flush().unwrap(); index_out.flush().unwrap();
    let state = root.path().join(STATE_DIR); std::fs::create_dir(&state).unwrap();
    let servers = b"first.example\nsecond.example\npartial";
    std::fs::write(state.join(SERVERS_FILE), servers).unwrap();
    let log_dir = root.path().join("h"); std::fs::create_dir(&log_dir).unwrap();
    let log = log_dir.join(slab::log_name(1, day(), 9));
    let mut seed = 7;
    let mut log_bytes = Vec::new();
    for art in [31, 32] { slab::frame(day() * DAY, &line(art, &noise(&mut seed, 600000)), &mut log_bytes); }
    log_bytes.extend_from_slice(&[1, 2, 3]);
    assert!(log_bytes.len() > CHUNK);
    std::fs::write(&log, &log_bytes).unwrap();
    let scope = Scope::new(None, 0, None);
    let (store, opened) = Store::open(cfg(root.path()), keep()).unwrap();
    let trace = scope.trace(); drop(scope);
    assert_eq!(opened, Opened { segments: 1, blocks: count, logs_replayed: 1, rows_replayed: 2, ..Default::default() });
    assert_eq!(entries(&store, "g", day()).len(), count);
    assert_eq!(*store.servers.lock().unwrap(), vec!["first.example".to_owned(), "second.example".to_owned()]);
    let h = store.groups.read().unwrap()["h"].clone();
    let slabs = h.slabs.lock().unwrap();
    let reopened = &slabs.open[&(1, day())];
    assert_eq!(reopened.slab_gen, 9); assert_eq!(reopened.len(), 2);
    drop(slabs);
    assert_eq!(std::fs::read(&log).unwrap(), log_bytes[..log_bytes.len() - 3]);
    assert_eq!(kind_bytes(&trace, ReadKind::Index), count * ENTRY_LEN);
    assert_eq!(kind_bytes(&trace, ReadKind::Header), 64);
    assert!(kind_bytes(&trace, ReadKind::Log) >= log_bytes.len());
    assert!(kind_bytes(&trace, ReadKind::Servers) >= servers.len());
    store.close().unwrap();
    // A failed server decode must release startup ownership and publish nothing.
    let bad = tempfile::tempdir().unwrap(); std::fs::create_dir(bad.path().join(STATE_DIR)).unwrap();
    std::fs::write(bad.path().join(STATE_DIR).join(SERVERS_FILE), [0xff]).unwrap();
    assert!(Store::open(cfg(bad.path()), keep()).is_err());
    std::fs::write(bad.path().join(STATE_DIR).join(SERVERS_FILE), b"fixed\n").unwrap();
    let (fixed, _) = Store::open(cfg(bad.path()), keep()).unwrap(); fixed.close().unwrap();
    assert_paced(&trace);
}

#[test]
fn startup_recovery_preserves_crc_rollforward_truncation_and_lock() {
    let root = tempfile::tempdir().unwrap();
    let (first, second) = (block(false, 1), block(false, 2));
    let expected = seed(root.path(), "g", day(), &[&first, &second]);
    let (arena, index) = paths(root.path(), "g", day());
    OpenOptions::new().write(true).open(&index).unwrap().set_len(ENTRY_LEN as u64 + 30).unwrap();
    let a = OpenOptions::new().write(true).open(&arena).unwrap();
    let good_end = (first.len() + second.len()) as u64;
    a.write_all_at(&[0xab; 350], good_end).unwrap(); drop(a);
    let lock_seen = Rc::new(std::cell::Cell::new(false));
    let seen = lock_seen.clone(); let path = root.path().to_owned();
    let scope = Scope::new(None, 0, Some(Box::new(move || {
        assert!(Store::open(cfg(&path), keep()).is_err(), "recovery must retain exclusive writer ownership"); seen.set(true);
    })));
    let (store, opened) = Store::open(cfg(root.path()), keep()).unwrap();
    let trace = scope.trace(); drop(scope);
    assert!(lock_seen.get());
    assert_eq!(opened, Opened { segments: 1, blocks: 2, ..Default::default() });
    assert_eq!(entries(&store, "g", day()), expected);
    assert_eq!(arena.metadata().unwrap().len(), good_end);
    assert_eq!(index.metadata().unwrap().len(), 2 * ENTRY_LEN as u64);
    store.close().unwrap(); drop(store);
    let corrupt = tempfile::tempdir().unwrap();
    seed(corrupt.path(), "g", day(), &[&first, &second]);
    let (arena, index) = paths(corrupt.path(), "g", day());
    let a = OpenOptions::new().read(true).write(true).open(&arena).unwrap();
    let offset = good_end - 70; let mut byte = [0]; a.read_exact_at(&mut byte, offset).unwrap();
    a.write_all_at(&[byte[0] ^ 0xff], offset).unwrap(); drop(a);
    let (store, opened) = Store::open(cfg(corrupt.path()), keep()).unwrap();
    assert_eq!(opened, Opened { segments: 1, blocks: 1, ..Default::default() });
    assert_eq!(entries(&store, "g", day()), expected[..1]);
    assert_eq!(arena.metadata().unwrap().len(), first.len() as u64);
    assert_eq!(index.metadata().unwrap().len(), ENTRY_LEN as u64);
    check_block(&store, "g", day(), expected[0], false);
    store.close().unwrap();
    let cold = compress_block_to_cold(&first, 1).unwrap();
    let cold_root = tempfile::tempdir().unwrap(); let expected = seed(cold_root.path(), "g", day(), &[&cold]);
    let scope = Scope::new(None, 0, None);
    let (store, opened) = Store::open(cfg(cold_root.path()), keep()).unwrap();
    let cold_trace = scope.trace(); drop(scope);
    assert_eq!(opened.blocks, 1); assert_eq!(entries(&store, "g", day()), expected);
    assert_eq!(kind_bytes(&cold_trace, ReadKind::Body), 0, "original cold recovery does not hash/decompress its body");
    check_block(&store, "g", day(), expected[0], true); store.close().unwrap();
    assert_paced(&trace);
}

#[test]
fn startup_recovery_rejects_forged_large_lengths_without_hashing_claimed_body() {
    let small = block(false, 1);
    let mut all_traces = Vec::new();
    for (claimed, extent) in [(u32::MAX & !63, u64::from(u32::MAX & !63)), (4096, 512), (321, 512), (64, 512)] {
        let root = tempfile::tempdir().unwrap();
        let (arena, _) = paths(root.path(), "g", day());
        let mut header = small[..DATA_START].to_vec();
        header[48..52].copy_from_slice(&claimed.to_le_bytes());
        header[52..56].fill(0);
        let checksum = crc32(&header); header[52..56].copy_from_slice(&checksum.to_le_bytes());
        let file = File::create(&arena).unwrap(); file.set_len(extent).unwrap(); file.write_all_at(&header, 0).unwrap(); drop(file);
        let scope = Scope::new(None, 0, None);
        let (store, opened) = Store::open(cfg(root.path()), keep()).unwrap();
        let trace = scope.trace(); drop(scope);
        assert_eq!(opened, Opened { segments: 1, ..Default::default() });
        assert!(entries(&store, "g", day()).is_empty());
        assert_eq!(arena.metadata().unwrap().len(), 0);
        assert_eq!(kind_bytes(&trace, ReadKind::Body), 0, "unvalidated claimed bytes must never be hashed");
        assert_eq!(kind_bytes(&trace, ReadKind::Header), 64);
        assert!(trace.events.iter().all(|e| e.bytes <= CHUNK), "a forged length must never become a giant read/admission");
        store.close().unwrap(); all_traces.push(trace);
    }
    // Independent positive control: a genuine parsed hot block produces a body event.
    let root = tempfile::tempdir().unwrap(); seed(root.path(), "g", day(), &[&small]);
    let scope = Scope::new(None, 0, None);
    let (store, opened) = Store::open(cfg(root.path()), keep()).unwrap();
    let positive = scope.trace(); drop(scope);
    assert_eq!(opened.blocks, 1); assert_eq!(kind_bytes(&positive, ReadKind::Body), small.len() - DATA_START);
    store.close().unwrap();
    for trace in all_traces { assert_paced(&trace); }
}

#[test]
fn startup_recovery_admission_discards_idle_credit_and_rechecks_early_wakes() {
    let root = tempfile::tempdir().unwrap(); let small = block(false, 1);
    let expected = seed(root.path(), "g", day(), &[&small]);
    let scope = Scope::new(Some(1), 3, None);
    let (store, opened) = Store::open(cfg(root.path()), keep()).unwrap();
    let trace = scope.trace(); drop(scope);
    assert_eq!(opened.blocks, 1); assert_eq!(entries(&store, "g", day()), expected);
    assert_eq!(kind_bytes(&trace, ReadKind::Header), 64);
    assert_eq!(kind_bytes(&trace, ReadKind::Body), small.len() - DATA_START);
    assert!(trace.events.iter().any(|e| e.at >= Duration::from_secs(60)), "fake idle gap must actually occur");
    store.close().unwrap();
    assert_paced(&trace);
    assert_eq!(trace.early_wakes, 3, "actual admission must recheck each deterministic early wake");
    assert!(trace.sleeps.len() > trace.early_wakes);
}


#[test]
fn startup_recovery_paces_orphan_manifest_opened_by_log_commit() {
    let root = tempfile::tempdir().unwrap();
    let (arena, index) = paths(root.path(), "g", day());
    let orphan_len = CHUNK + ENTRY_LEN;
    std::fs::write(&index, vec![0; orphan_len]).unwrap();
    assert!(!arena.exists());
    let log = root.path().join("g").join(slab::log_name(3, day(), 9));
    let mut random = 7;
    let lines: Vec<_> = [31, 32].into_iter().map(|art| line(art, &noise(&mut random, 600000))).collect();
    let mut log_bytes = Vec::new();
    for line in &lines { slab::frame(day() * DAY, line, &mut log_bytes); }
    assert!(log_bytes.len() > CHUNK);
    std::fs::write(&log, &log_bytes).unwrap();
    let mut config = cfg(root.path());
    config.memory_budget = 1;
    assert!(lines.iter().map(|line| row_charge(line)).sum::<usize>() > config.memory_budget);
    let scope = Scope::new(None, 0, None);
    let (store, opened) = Store::open(config, keep()).unwrap();
    let trace = scope.trace(); drop(scope);
    assert_eq!(opened, Opened { segments: 1, blocks: 1, logs_replayed: 1, rows_replayed: 2, ..Default::default() });
    let entries = entries(&store, "g", day());
    assert_eq!(entries.len(), 1);
    assert_eq!((entries[0].rows, entries[0].slab_gen, entries[0].server, entries[0].seq), (2, 9, 3, 0));
    assert_eq!(index.metadata().unwrap().len(), ENTRY_LEN as u64);
    assert_eq!(std::fs::read(&index).unwrap(), entries[0].to_bytes());
    assert_eq!(arena.metadata().unwrap().len(), u64::from(entries[0].len));
    assert!(!log.exists());
    assert!(store.groups.read().unwrap()["g"].slabs.lock().unwrap().open.is_empty());
    check_block(&store, "g", day(), entries[0], false);
    assert_eq!(kind_bytes(&trace, ReadKind::Index), orphan_len);
    assert!(kind_bytes(&trace, ReadKind::Log) >= log_bytes.len());
    let last_log = trace.events.iter().rposition(|event| event.kind == ReadKind::Log).unwrap();
    let first_index = trace.events.iter().position(|event| event.kind == ReadKind::Index).unwrap();
    assert!(last_log < first_index, "the orphan manifest must be opened by the recovered-log commit");
    store.close().unwrap();
    assert_paced(&trace);
}
