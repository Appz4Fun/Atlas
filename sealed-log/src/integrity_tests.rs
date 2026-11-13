//! Tiny synthetic integrity oracles; no protected archive or endpoint is used.
pub(crate) mod test_io {
    use super::*;
    use std::cell::RefCell;
    use std::marker::PhantomData;
    use std::rc::Rc;

    #[derive(Clone, Copy, PartialEq, Eq)]
    pub(crate) enum Fault {
        FinalWrite,
        FinalSync,
        RolloverOpen,
    }

    struct State {
        dir: PathBuf,
        allocations: Vec<usize>,
        fault: Option<Fault>,
        fired: bool,
    }

    thread_local! {
        static ACTIVE: RefCell<Option<State>> = const { RefCell::new(None) };
    }

    pub(crate) struct Scope(PhantomData<Rc<()>>);

    impl Scope {
        pub(crate) fn enter(dir: &Path, fault: Option<Fault>) -> Self {
            ACTIVE.with_borrow_mut(|active| {
                assert!(active.is_none(), "I/O fixture scopes cannot overlap");
                *active = Some(State { dir: dir.to_path_buf(), allocations: Vec::new(), fault, fired: false });
            });
            Self(PhantomData)
        }

        pub(crate) fn allocations(&self) -> Vec<usize> {
            ACTIVE.with_borrow(|active| active.as_ref().unwrap().allocations.clone())
        }

        pub(crate) fn fault_fired(&self) -> bool {
            ACTIVE.with_borrow(|active| active.as_ref().unwrap().fired)
        }
    }

    impl Drop for Scope {
        fn drop(&mut self) {
            ACTIVE.with_borrow_mut(|active| *active = None);
        }
    }

    pub(crate) fn before_payload_allocation(dir: &Path, bytes: usize) -> io::Result<()> {
        ACTIVE.with_borrow_mut(|active| {
            if let Some(state) = active.as_mut().filter(|state| state.dir == dir) {
                state.allocations.push(bytes);
                if bytes > 4096 {
                    return Err(Error::other("fixture stopped an oversized payload allocation"));
                }
            }
            Ok(())
        })
    }

    fn take_fault(dir: &Path, expected: Fault) -> bool {
        ACTIVE.with_borrow_mut(|active| {
            if let Some(state) = active.as_mut().filter(|state| state.dir == dir && state.fault == Some(expected)) {
                state.fault = None;
                state.fired = true;
                return true;
            }
            false
        })
    }

    pub(crate) fn before_final_write(dir: &Path, file: &File, bytes: &[u8], offset: u64) -> io::Result<()> {
        if take_fault(dir, Fault::FinalWrite) {
            file.write_all_at(&bytes[..bytes.len().min(7)], offset)?;
            return Err(Error::other("injected partial final write"));
        }
        Ok(())
    }

    pub(crate) fn before_final_sync(dir: &Path) -> io::Result<()> {
        if take_fault(dir, Fault::FinalSync) {
            return Err(Error::other("injected final sync failure"));
        }
        Ok(())
    }

    pub(crate) fn before_rollover_open(dir: &Path) -> io::Result<()> {
        if take_fault(dir, Fault::RolloverOpen) {
            return Err(Error::other("injected rollover open failure"));
        }
        Ok(())
    }

    pub(crate) fn registered_fault(dir: &Path) -> Option<Fault> {
        ACTIVE.with_borrow(|active| active.as_ref().filter(|state| state.dir == dir).and_then(|state| state.fault))
    }

    pub(crate) fn skip_reservation(dir: &Path) -> bool {
        ACTIVE.with_borrow(|active| active.as_ref().is_some_and(|state| state.dir == dir))
    }
}

use super::*;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::AtomicU64;

const FIXTURE_LIMIT: usize = 8 << 20;
static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    dir: PathBuf,
}

impl Fixture {
    fn new(segments: &[Vec<u8>]) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "sealed-integrity-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Relaxed)
        ));
        fs::create_dir(&dir).unwrap();
        let fixture = Self { dir };
        for (segment, bytes) in segments.iter().enumerate() {
            assert!(bytes.len() <= FIXTURE_LIMIT);
            fs::write(segment_path(&fixture.dir, segment as u32), bytes).unwrap();
        }
        fixture
    }

    fn open(&self) -> io::Result<Arc<Log>> {
        Log::open_with(&self.dir, FIXTURE_LIMIT as u64)
    }

    fn checkpoint(&self, offset: u64) {
        fs::write(self.dir.join("tail"), format!("0 {offset}")).unwrap();
    }

    fn bytes(&self, segment: u32) -> Vec<u8> {
        let path = segment_path(&self.dir, segment);
        assert!(fs::metadata(&path).unwrap().len() <= FIXTURE_LIMIT as u64);
        fs::read(path).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn record(key: u64, value: &[u8]) -> Vec<u8> {
    let mut body = key.to_be_bytes().to_vec();
    body.extend_from_slice(&(value.len() as u32).to_be_bytes());
    body.extend_from_slice(value);
    let mut bytes = b"SLG1".to_vec();
    bytes.extend_from_slice(&crc32fast::hash(&body).to_be_bytes());
    bytes.extend_from_slice(&body);
    bytes
}

fn reject_without_rewriting(fixture: &Fixture) {
    let before = fixture.bytes(0);
    for _ in 0..2 {
        let result = fixture.open();
        let error = result.err().expect("ambiguous or corrupt bytes must not become a writable tail");
        assert_eq!(error.kind(), ErrorKind::InvalidData, "recovery must identify invalid source bytes");
        assert_eq!(fixture.bytes(0), before, "failed reopen must preserve the complete segment");
    }
}

#[test]
fn corrupt_record_with_valid_suffix_is_not_a_writable_tail() {
    let first = record(1, b"first");
    let mut middle = record(2, b"middle");
    middle[HEADER] ^= 1;
    let bytes = [first.clone(), middle, record(3, b"last")].concat();
    for checkpoint in [None, Some(first.len() as u64)] {
        let fixture = Fixture::new(&[bytes.clone()]);
        if let Some(offset) = checkpoint {
            fixture.checkpoint(offset);
        }
        reject_without_rewriting(&fixture);
    }
}

#[test]
fn a_complete_bad_crc_tail_is_not_overwritten() {
    let mut bytes = record(1, b"damaged");
    bytes[HEADER + 1] ^= 1;
    reject_without_rewriting(&Fixture::new(&[bytes]));
}

#[test]
fn ambiguous_nonzero_malformed_tails_are_rejected() {
    let mut oversized = record(2, b"");
    oversized[16..20].copy_from_slice(&u32::MAX.to_be_bytes());
    let mut truncated = record(3, b"payload");
    truncated.truncate(HEADER + 2);
    for tail in [vec![1; HEADER], b"SLG".to_vec(), oversized, truncated] {
        let bytes = [record(1, b"keep"), tail].concat();
        reject_without_rewriting(&Fixture::new(&[bytes]));
    }
}

#[test]
fn zeros_before_nonzero_suffix_are_not_unused_reservation() {
    let mut bytes = record(1, b"keep");
    bytes.resize(64 << 10, 0);
    bytes[(64 << 10) - 1] = 1;
    reject_without_rewriting(&Fixture::new(&[bytes]));
}

#[test]
fn verified_zero_reservation_keeps_the_valid_record_boundary() {
    let valid = record(1, b"keep");
    let mut bytes = valid.clone();
    bytes.resize(64 << 10, 0);
    let fixture = Fixture::new(&[bytes.clone()]);
    let log = fixture.open().unwrap();
    assert_eq!(log.tail().0, 0);
    assert_eq!(log.tail().1, valid.len() as u64);
    assert_eq!(log.read(Locator { segment: 0, offset: 0, len: 4 }).unwrap(), b"keep");
    assert_eq!(fixture.bytes(0), bytes, "opening must not truncate the reserved suffix");
}

#[test]
fn a_checkpoint_cannot_hide_corruption_before_its_offset() {
    let mut first = record(1, b"damaged");
    first[HEADER] ^= 1;
    let checkpoint = first.len() as u64;
    let fixture = Fixture::new(&[[first, record(2, b"valid")].concat()]);
    fixture.checkpoint(checkpoint);
    reject_without_rewriting(&fixture);
}

#[test]
fn invalid_checkpoint_offsets_never_become_the_writable_tail() {
    let bytes = record(1, b"keep");
    for offset in [1, bytes.len() as u64 + 17, u64::MAX] {
        let fixture = Fixture::new(&[bytes.clone()]);
        fixture.checkpoint(offset);
        let opened = catch_unwind(AssertUnwindSafe(|| fixture.open()));
        assert!(opened.is_ok(), "invalid checkpoint arithmetic must not panic: {offset}");
        if let Ok(log) = opened.unwrap() {
            assert_eq!(log.tail().1, bytes.len() as u64, "a rejected hint must be rescanned safely");
        }
        assert_eq!(fixture.bytes(0), bytes);
    }
}

fn reject_before_payload_allocation(fixture: &Fixture, at: Locator) {
    let log = fixture.open().unwrap();
    let scope = test_io::Scope::enter(&fixture.dir, None);
    let result = catch_unwind(AssertUnwindSafe(|| log.read(at)));
    assert!(result.is_ok(), "untrusted locator arithmetic must not panic: {at:?}");
    let error = result.unwrap().unwrap_err();
    assert_eq!(error.kind(), ErrorKind::InvalidData, "invalid locator must fail bounds/header admission: {at:?}");
    assert!(scope.allocations().is_empty(), "rejection reached payload allocation: {:?}", scope.allocations());
}

#[test]
fn old_segment_lengths_are_rejected_before_payload_allocation() {
    for len in [1024, u32::MAX] {
        let fixture = Fixture::new(&[record(1, b"old"), record(2, b"new")]);
        reject_before_payload_allocation(&fixture, Locator { segment: 0, offset: 0, len });
    }
}

#[test]
fn current_segment_lengths_are_rejected_before_payload_allocation() {
    for len in [1024, u32::MAX] {
        let fixture = Fixture::new(&[record(1, b"old"), record(2, b"new")]);
        reject_before_payload_allocation(&fixture, Locator { segment: 1, offset: 0, len });
    }
}

#[test]
fn overflowing_locator_offsets_are_rejected_without_panics() {
    for segment in [0, 1] {
        let fixture = Fixture::new(&[record(1, b"old"), record(2, b"new")]);
        reject_before_payload_allocation(&fixture, Locator { segment, offset: u64::MAX, len: 3 });
    }
}

#[test]
fn mismatched_record_lengths_are_rejected_before_payload_allocation() {
    let mut older = record(1, b"old");
    older.resize(128, 0);
    let fixture = Fixture::new(&[older, record(2, b"new")]);
    reject_before_payload_allocation(&fixture, Locator { segment: 0, offset: 0, len: 12 });
}

#[test]
fn valid_old_and_current_locators_keep_their_values() {
    let fixture = Fixture::new(&[record(1, b"old"), record(2, b"")]);
    let log = fixture.open().unwrap();
    let _scope = test_io::Scope::enter(&fixture.dir, None);
    assert_eq!(log.read(Locator { segment: 0, offset: 0, len: 3 }).unwrap(), b"old");
    assert!(log.read(Locator { segment: 1, offset: 0, len: 0 }).unwrap().is_empty());
}

struct Writer {
    send: Option<mpsc::Sender<Group>>,
    thread: Option<std::thread::JoinHandle<bool>>,
    tail: Arc<RwLock<(u32, u64)>>,
    failure: Arc<Mutex<Option<Arc<io::Error>>>>,
}

impl Writer {
    fn start(fixture: &Fixture, fault: test_io::Fault) -> Self {
        let file = Arc::new(open_segment(&fixture.dir, 0).unwrap());
        let pos = file.metadata().unwrap().len();
        let tail = Arc::new(RwLock::new((0, pos)));
        let shared = tail.clone();
        let dir = fixture.dir.clone();
        let failure = Arc::new(Mutex::new(None));
        let failed = failure.clone();
        let (send, recv) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            let scope = test_io::Scope::enter(&dir, Some(fault));
            let state = Tail { segment: 0, file, pos, reserved: pos };
            write_loop(dir.clone(), 64, state, shared, failed, recv);
            scope.fault_fired()
        });
        Self { send: Some(send), thread: Some(thread), tail, failure }
    }

    fn append(&self, value: &[u8]) -> io::Result<Vec<Locator>> {
        let (done, reply) = mpsc::channel();
        self.send
            .as_ref()
            .unwrap()
            .send(Group { values: vec![(2, value.to_vec())], queued: Instant::now(), done })
            .map_err(|_| writer_error(&self.failure))?;
        match reply.recv_timeout(Duration::from_secs(2)) {
            Ok(result) => result,
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(writer_error(&self.failure)),
            Err(mpsc::RecvTimeoutError::Timeout) => panic!("fixture writer did not answer"),
        }
    }

    fn finish(&mut self) {
        self.send.take();
        assert!(self.thread.take().unwrap().join().unwrap(), "the requested fault was not injected");
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        self.send.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn failure_fences_writer(fault: test_io::Fault) {
    let committed = record(1, b"ack");
    let fixture = Fixture::new(&[committed.clone()]);
    fixture.checkpoint(committed.len() as u64);
    let checkpoint = fs::read(fixture.dir.join("tail")).unwrap();
    let mut writer = Writer::start(&fixture, fault);
    let failed = writer.append(&[7; 60]);
    let later = writer.append(b"later");
    writer.finish();
    assert!(failed.is_err(), "the injected I/O failure must reject its group");
    assert!(later.is_err(), "an I/O failure must permanently fence subsequent appends");
    let first = failed.unwrap_err();
    let later = later.unwrap_err();
    let first = first.get_ref().unwrap().downcast_ref::<WriterFenced>().expect("typed first I/O fence");
    let later = later.get_ref().unwrap().downcast_ref::<WriterFenced>().expect("typed later I/O fence");
    assert!(Arc::ptr_eq(&first.cause, &later.cause), "all callers retain the original I/O cause");
    assert!(first.cause.to_string().contains("injected"));
    assert_eq!(*writer.tail.read().unwrap(), (0, committed.len() as u64));
    assert_eq!(fixture.bytes(0), committed, "the previously committed prefix must remain unchanged");
    assert_eq!(fs::read(fixture.dir.join("tail")).unwrap(), checkpoint, "failed positions are not checkpoints");
    for entry in fs::read_dir(&fixture.dir).unwrap() {
        assert!(entry.unwrap().metadata().unwrap().len() <= FIXTURE_LIMIT as u64);
    }
}

#[test]
fn a_partial_final_write_after_rollover_permanently_fences_the_writer() {
    failure_fences_writer(test_io::Fault::FinalWrite);
}

#[test]
fn a_final_sync_failure_after_rollover_permanently_fences_the_writer() {
    failure_fences_writer(test_io::Fault::FinalSync);
}

#[test]
fn a_rollover_open_failure_permanently_fences_the_writer() {
    failure_fences_writer(test_io::Fault::RolloverOpen);
}

#[test]
fn public_append_keeps_prior_acknowledgements_and_original_failure_for_future_callers() {
    for fault in [test_io::Fault::FinalWrite, test_io::Fault::FinalSync, test_io::Fault::RolloverOpen] {
        let committed = record(1, b"ack");
        let fixture = Fixture::new(&[committed.clone()]);
        fixture.checkpoint(committed.len() as u64);
        let checkpoint = fs::read(fixture.dir.join("tail")).unwrap();
        let _scope = test_io::Scope::enter(&fixture.dir, Some(fault));
        let log = Log::open_with(&fixture.dir, 64).unwrap();
        let before = log.tail();
        let failed = log.append(vec![(2, vec![7; 60])]).unwrap_err();
        let later = log.append(vec![(3, b"later".to_vec())]).unwrap_err();
        let first = failed.get_ref().unwrap().downcast_ref::<WriterFenced>().expect("actual public append fence");
        let next = later.get_ref().unwrap().downcast_ref::<WriterFenced>().expect("actual future public append fence");
        assert!(Arc::ptr_eq(&first.cause, &next.cause));
        assert!(first.cause.to_string().contains("injected"));
        assert_eq!(log.tail(), before);
        assert_eq!(log.read(Locator { segment: 0, offset: 0, len: 3 }).unwrap(), b"ack");
        drop(log);
        assert_eq!(fixture.bytes(0), committed);
        assert_eq!(fs::read(fixture.dir.join("tail")).unwrap(), checkpoint);
    }
}

#[test]
fn missing_or_noncanonical_segment_inventory_cannot_select_a_new_writable_tail() {
    for bad in ["segment-00000002.log", "segment-1.log"] {
        let first = record(1, b"ack");
        let last = record(2, b"later");
        let fixture = Fixture::new(&[first.clone(), last.clone()]);
        fs::rename(segment_path(&fixture.dir, 1), fixture.dir.join(bad)).unwrap();
        assert!(matches!(fixture.open(),Err(error) if error.kind()==ErrorKind::InvalidData));
        assert_eq!(fixture.bytes(0), first);
        assert_eq!(fs::read(fixture.dir.join(bad)).unwrap(), last);
        assert!(!segment_path(&fixture.dir, 1).exists());
    }
}

#[test]
fn an_orphan_checkpoint_cannot_create_an_empty_replacement_log() {
    let fixture = Fixture::new(&[]);
    fixture.checkpoint(23);
    let before = fs::read(fixture.dir.join("tail")).unwrap();
    assert!(matches!(fixture.open(),Err(error) if error.kind()==ErrorKind::InvalidData));
    assert!(!segment_path(&fixture.dir, 0).exists());
    assert_eq!(fs::read(fixture.dir.join("tail")).unwrap(), before);
}

#[test]
fn append_reply_extent_is_admitted_before_any_commit() {
    let fixture = Fixture::new(&[]);
    let scope = test_io::Scope::enter(&fixture.dir, None);
    let log = fixture.open().unwrap();
    let before = log.tail();
    let body: Vec<u8> =
        (0u64..6).flat_map(|key| [key.to_be_bytes().as_slice(), &0u32.to_be_bytes()].concat()).collect();
    assert_eq!(body.len(), 72);
    let error = handle_append(&log, &body, 100).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::InvalidData);
    assert!(error.to_string().contains("append response"));
    assert_eq!(log.tail(), before, "rejected response extent must not publish a batch");
    assert!(fixture.bytes(0).is_empty(), "rejected batch must not enter the writer");
    assert!(scope.allocations().is_empty());
    let reply = handle_append(&log, &body[..60], 105).unwrap();
    assert_eq!(reply.len(), 105, "the exact five-locator boundary remains writable");
    assert_ne!(log.tail(), before);
    drop(log);
}

#[test]
fn append_extent_checks_both_directions_and_overflow_without_large_allocations() {
    let extent = append_extent([0; 5], 105).unwrap();
    assert_eq!((extent.request, extent.response, extent.count), (60, 105, 5));
    for (lengths, limit) in [(vec![0; 6], 100), (vec![94], 105), (vec![usize::MAX], usize::MAX)] {
        assert_eq!(append_extent(lengths, limit).err().unwrap().kind(), ErrorKind::InvalidData);
    }
    let count = MAX_FRAME / REF_BYTES + 1;
    assert_eq!(count, 25_565_282);
    assert_eq!(count * 12, 306_783_384);
    assert_eq!(count * REF_BYTES, 536_870_922);
    let mut extent = AppendExtent { request: 0, response: 0, count: usize::MAX / REF_BYTES };
    assert_eq!(extent.include(0, usize::MAX).unwrap_err().kind(), ErrorKind::InvalidData);
}
