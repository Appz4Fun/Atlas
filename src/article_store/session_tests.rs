//! Real independent-open and subprocess controls for the inactive read/GC protocol.
use super::test_support::{OwnedChild, marker_ready, wait_exit, wait_marker};
use super::*;
use std::{
    cell::RefCell,
    fs::{self, File},
    io,
    os::{
        fd::AsRawFd,
        unix::fs::{FileExt, MetadataExt, PermissionsExt, symlink},
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(10)
}
fn binding() -> ReleaseBinding {
    ReleaseBinding { group: "alt.x".into(), is_collection: false, identity_key: [0x73; 32] }
}
fn setup() -> (tempfile::TempDir, PathBuf, Writer) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap().join("payload");
    let mut writer = Writer::create(&root, CatalogId::from_bytes([0x42; 32])).unwrap();
    writer.initialize_read_gate().expect("explicitly initialize the persistent gate");
    (temp, root, writer)
}
fn publish(writer: &mut Writer, value: &[u8]) -> PayloadRef {
    writer.publish(value.len() as u32, &mut &value[..], deadline()).unwrap()
}
fn batch(writer: &mut Writer) -> (BatchRef, PayloadRef) {
    let data = publish(writer, b"lazy article bytes");
    let mut row = AcceptedRowV1::default();
    row.fields[0] = Some(FieldRef::new(18, vec![Extent { chunk_index: 0, offset: 0, bytes: 18 }]).unwrap());
    let mut builder = BatchManifestBuilder::new(writer, binding(), vec![data], deadline()).unwrap();
    builder.push(row).unwrap();
    (builder.finish().unwrap(), data)
}
fn object(root: &Path, digest: [u8; 32]) -> PathBuf {
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    root.join("chunks").join(&hex[..2]).join(hex)
}
fn snapshot<'a>(reader: &'a SessionReader, reference: &BatchRef) -> BatchSnapshot<'a> {
    BatchSnapshot::open(reader.begin_read(deadline()).unwrap(), reference, &binding(), deadline()).unwrap()
}
fn read_literal(snapshot: &mut BatchSnapshot<'_>) {
    let row = snapshot.next_row().unwrap().unwrap();
    let mut out = [0; 18];
    assert_eq!(snapshot.read_field_range(row.fields[0].as_ref().unwrap(), 0, &mut out, deadline()).unwrap(), 18);
    assert_eq!(&out, b"lazy article bytes");
    assert!(snapshot.next_row().unwrap().is_none());
}

// A cloned open-file description would let one session convert/unlock another's flock.
#[test]
fn same_process_independent_gate_opens_block_exclusive() {
    let (_temp, root, mut writer) = setup();
    let reader = SessionReader::open(&root, writer.identity()).unwrap();
    let first = reader.begin_read(deadline()).unwrap();
    let second = reader.begin_read(deadline()).unwrap();
    assert!(matches!(reader.try_gc_exclusive(deadline()), Err(Error::Busy)));
    let independent = File::open(root.join("read-gc.lock")).unwrap();
    assert_eq!(unsafe { libc::flock(independent.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) }, -1);
    assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::EWOULDBLOCK));
    drop(first);
    assert!(matches!(reader.try_gc_exclusive(deadline()), Err(Error::Busy)));
    drop(second);
    let exclusive = reader.try_gc_exclusive(deadline()).unwrap();
    assert!(matches!(reader.begin_read(deadline()), Err(Error::Busy)));
    assert!(matches!(writer.begin_write(deadline()), Err(Error::Busy)));
    drop(exclusive);
    let writing = writer.begin_write(deadline()).unwrap();
    let shared = reader.begin_read(deadline()).unwrap();
    drop(shared);
    assert!(matches!(reader.try_gc_exclusive(deadline()), Err(Error::Busy)));
    drop(writing);
    assert!(reader.try_gc_exclusive(deadline()).is_ok());
}

fn child(mode: &str, root: &Path, reference: &BatchRef, marker: &Path) -> OwnedChild {
    OwnedChild(
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "article_store::session_tests::subprocess_entry", "--nocapture"])
            .env("ATLAS_SESSION_TEST_MODE", mode)
            .env("ATLAS_SESSION_TEST_ROOT", root)
            .env("ATLAS_SESSION_TEST_MARKER", marker)
            .env(
                "ATLAS_SESSION_TEST_IDENTITY",
                reference.identity.to_bytes().iter().map(|b| format!("{b:02x}")).collect::<String>(),
            )
            .env(
                "ATLAS_SESSION_TEST_DIGEST",
                reference.root.digest.iter().map(|b| format!("{b:02x}")).collect::<String>(),
            )
            .env("ATLAS_SESSION_TEST_BYTES", reference.root.bytes.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    )
}
fn allow_child(marker: &Path) {
    fs::write(marker.with_extension("continue"), b"continue").unwrap();
}

// No child data/page descriptor has opened at the semantic readiness boundary.
#[test]
fn cross_process_lazy_read_blocks_exclusive_gc() {
    let (temp, root, mut writer) = setup();
    let (reference, _) = batch(&mut writer);
    let reader = SessionReader::open(&root, writer.identity()).unwrap();
    let marker = temp.path().join("lazy");
    let mut process = child("lazy", &root, &reference, &marker);
    wait_marker(&mut process, &marker, b"root-only");
    assert!(
        matches!(reader.try_gc_exclusive(deadline()), Err(Error::Busy)),
        "actual child session must exclude GC before its lazy children open"
    );
    allow_child(&marker);
    assert!(wait_exit(&mut process).success());
    assert!(marker_ready(&marker.with_extension("result"), b"literal-read"));
    assert!(reader.try_gc_exclusive(deadline()).is_ok());
}

// Cancellation/error drop the original guard; process death releases its actual kernel lock.
#[test]
fn gate_release_on_cancel_error_and_process_exit() {
    for mode in ["cancel", "error", "exit"] {
        let (temp, root, mut writer) = setup();
        let (reference, data) = batch(&mut writer);
        let reader = SessionReader::open(&root, writer.identity()).unwrap();
        let marker = temp.path().join(mode);
        let mut process = child(mode, &root, &reference, &marker);
        wait_marker(&mut process, &marker, b"root-only");
        assert!(
            matches!(reader.try_gc_exclusive(deadline()), Err(Error::Busy)),
            "control must observe the original live child lock before releasing it"
        );
        if mode == "exit" {
            process.kill().unwrap();
            assert!(!wait_exit(&mut process).success());
        } else {
            if mode == "error" {
                fs::remove_file(object(&root, data.digest)).unwrap();
            }
            allow_child(&marker);
            wait_marker(&mut process, &marker.with_extension("result"), b"released");
            assert!(
                reader.try_gc_exclusive(deadline()).is_ok(),
                "guard must release while child process remains alive"
            );
            fs::write(marker.with_extension("finish"), b"finish").unwrap();
            assert!(wait_exit(&mut process).success());
        }
        assert!(reader.try_gc_exclusive(deadline()).is_ok());
    }
}

// Losing the gate name or substituting a fresh regular inode must never split a lock domain.
#[test]
fn gate_refuses_missing_symlink_replaced_inode() {
    let (temp, root, mut writer) = setup();
    let identity = writer.identity();
    let reader = SessionReader::open(&root, identity).unwrap();
    let gate = root.join("read-gc.lock");
    let saved = temp.path().join("original-gate");
    assert!(gate.is_file(), "explicit initialization must create the persistent gate before substitution controls");
    fs::rename(&gate, &saved).unwrap();
    assert!(matches!(SessionReader::open(&root, identity), Err(Error::Missing)));
    assert!(matches!(reader.begin_read(deadline()), Err(Error::Missing)));
    assert!(matches!(reader.try_gc_exclusive(deadline()), Err(Error::Missing)));
    assert!(matches!(writer.begin_write(deadline()), Err(Error::Missing)));
    symlink(&saved, &gate).unwrap();
    assert!(matches!(SessionReader::open(&root, identity), Err(Error::Corrupt)));
    assert!(matches!(reader.begin_read(deadline()), Err(Error::Corrupt)));
    assert!(matches!(writer.initialize_read_gate(), Err(Error::Corrupt)));
    fs::remove_file(&gate).unwrap();
    fs::write(&gate, []).unwrap();
    fs::set_permissions(&gate, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(matches!(reader.begin_read(deadline()), Err(Error::Corrupt)));
    assert!(matches!(reader.try_gc_exclusive(deadline()), Err(Error::Corrupt)));
    assert!(matches!(writer.begin_write(deadline()), Err(Error::Corrupt)));
    assert!(matches!(writer.initialize_read_gate(), Err(Error::Corrupt)));
    // Original physical anchor remains usable after restoring its original name.
    fs::remove_file(&gate).unwrap();
    fs::rename(&saved, &gate).unwrap();
    assert!(reader.begin_read(deadline()).is_ok());
}

// Explicit initialization must flush both inode and parent without ever replacing a persistent gate.
#[test]
fn persistent_gate_initialization_is_durable_and_poisoned_on_sync_error() {
    let (_temp, root, mut writer) = setup();
    let gate = root.join("read-gc.lock");
    assert!(gate.is_file(), "explicit initialization must durably create the persistent gate");
    let before = fs::metadata(&gate).unwrap();
    assert_eq!(before.permissions().mode() & 0o777, 0o600);
    assert_eq!(before.nlink(), 1);
    let calls = payload::test_sync_calls();
    writer.initialize_read_gate().unwrap();
    let after = fs::metadata(&gate).unwrap();
    assert_eq!((before.dev(), before.ino()), (after.dev(), after.ino()));
    assert!(payload::test_sync_calls() >= calls + 2, "reconcile gate inode and root durability");
    let calls = payload::test_sync_calls();
    payload::test_fail_next_sync();
    let result = writer.initialize_read_gate();
    payload::test_clear_sync_failure();
    assert!(matches!(result, Err(Error::Io(e)) if e.raw_os_error() == Some(libc::EIO)));
    assert!(payload::test_sync_calls() > calls);
    assert!(matches!(writer.begin_write(deadline()), Err(Error::Busy)));
    assert!(matches!(writer.publish(0, &mut &b""[..], deadline()), Err(Error::Busy)));
}

// Opening/read admission is read-only, and malformed public modes are refused.
#[test]
fn session_readonly_never_creates_gate_and_refuses_unsafe_mode() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap().join("payload");
    let mut writer = Writer::create(&root, CatalogId::from_bytes([0x42; 32])).unwrap();
    let identity = writer.identity();
    let before: Vec<_> = fs::read_dir(&root).unwrap().map(|entry| entry.unwrap().file_name()).collect();
    let calls = payload::test_sync_calls();
    assert!(matches!(SessionReader::open(&root, identity), Err(Error::Missing)));
    assert!(!root.join("read-gc.lock").exists());
    assert!(matches!(writer.begin_write(deadline()), Err(Error::Missing)));
    assert_eq!(fs::read_dir(&root).unwrap().count(), before.len());
    assert_eq!(payload::test_sync_calls(), calls);
    writer.initialize_read_gate().unwrap();
    let calls = payload::test_sync_calls();
    let reader = SessionReader::open(&root, identity).unwrap();
    drop(reader.begin_read(deadline()).unwrap());
    assert_eq!(payload::test_sync_calls(), calls);
    fs::set_permissions(root.join("read-gc.lock"), fs::Permissions::from_mode(0o666)).unwrap();
    assert!(matches!(reader.begin_read(deadline()), Err(Error::Corrupt)));
    assert!(matches!(SessionReader::open(&root, identity), Err(Error::Corrupt)));
}

// Deadline refusal is cooperative and must happen even when the uncontended flock would succeed.
#[test]
fn expired_gate_deadlines_refuse_without_consuming_descriptors() {
    let (_temp, root, mut writer) = setup();
    let reader = SessionReader::open(&root, writer.identity()).unwrap();
    let before = reader.test_descriptor_counts().0;
    assert!(matches!(reader.begin_read(Instant::now()), Err(Error::Deadline)));
    assert!(matches!(reader.try_gc_exclusive(Instant::now()), Err(Error::Deadline)));
    assert!(matches!(writer.begin_write(Instant::now()), Err(Error::Deadline)));
    assert_eq!(reader.test_descriptor_counts().0, before);
    assert!(reader.try_gc_exclusive(deadline()).is_ok());
}

// A later snapshot or range deadline cannot extend the original session admission.
#[test]
fn read_session_deadline_cannot_be_extended_by_snapshot_or_range() {
    let (_temp, root, mut writer) = setup();
    let (reference, _) = batch(&mut writer);
    let reader = SessionReader::open(&root, writer.identity()).unwrap();
    let session = reader.begin_read(Instant::now() + Duration::from_millis(20)).unwrap();
    let mut view = BatchSnapshot::open(session, &reference, &binding(), deadline()).unwrap();
    let token = view.next_row().unwrap().unwrap().fields[0].take().unwrap();
    thread::sleep(Duration::from_millis(30));
    assert!(matches!(view.next_row(), Err(Error::Deadline)));
    assert!(matches!(view.read_field_range(&token, 0, &mut [0; 18], deadline()), Err(Error::Deadline)));
    drop(view);
    let session = reader.begin_read(Instant::now() + Duration::from_millis(20)).unwrap();
    thread::sleep(Duration::from_millis(30));
    assert!(matches!(BatchSnapshot::open(session, &reference, &binding(), deadline()), Err(Error::Deadline)));
    assert!(reader.try_gc_exclusive(deadline()).is_ok());
}

// A publication guard owns its descriptor independently of the Writer borrow/lifetime.
#[test]
fn write_session_outlives_writer_until_publication_cancel() {
    let (_temp, root, writer) = setup();
    let reader = SessionReader::open(&root, writer.identity()).unwrap();
    let publishing = writer.begin_write(deadline()).unwrap();
    drop(writer);
    assert!(matches!(reader.try_gc_exclusive(deadline()), Err(Error::Busy)));
    drop(publishing);
    assert!(reader.try_gc_exclusive(deadline()).is_ok());
}

// Sessions share the actual payload permit budget, and released permits restore admission.
#[test]
fn session_descriptor_cap_counts_anchor_and_releases_permits() {
    let (_temp, root, writer) = setup();
    let reader = SessionReader::open(&root, writer.identity()).unwrap();
    assert_eq!(reader.test_descriptor_counts().0, 3, "root, chunks, persistent gate anchor");
    let mut sessions = Vec::new();
    for _ in 0..5 {
        sessions.push(reader.begin_read(deadline()).unwrap());
    }
    assert_eq!(reader.test_descriptor_counts().0, 8);
    assert!(matches!(reader.begin_read(deadline()), Err(Error::Limit)));
    assert!(matches!(reader.try_gc_exclusive(deadline()), Err(Error::Limit)));
    assert_eq!(reader.test_descriptor_counts().1, 8);
    sessions.pop();
    let replacement = reader.begin_read(deadline()).unwrap();
    drop(replacement);
    drop(sessions);
    assert_eq!(reader.test_descriptor_counts().0, 3);
    assert!(reader.try_gc_exclusive(deadline()).is_ok());
}

// Alternating page/data opens exercise the transient high-water mark, including a retained write guard.
#[test]
fn lazy_manifest_and_publication_stay_within_eight_descriptors() {
    let (_temp, root, mut writer) = setup();
    let writing = writer.begin_write(deadline()).unwrap();
    let data: Vec<_> = (0..24u16).map(|n| publish(&mut writer, &n.to_le_bytes())).collect();
    let mut builder = BatchManifestBuilder::new(&mut writer, binding(), data, deadline()).unwrap();
    for n in 0..700 {
        let mut row = AcceptedRowV1 { ordinal: n, ..Default::default() };
        row.fields[0] =
            Some(FieldRef::new(2, vec![Extent { chunk_index: (n % 24) as u16, offset: 0, bytes: 2 }]).unwrap());
        builder.push(row).unwrap();
    }
    let reference = builder.finish().unwrap();
    assert!(writer.test_descriptor_counts().1 <= 8);
    drop(writing);
    let reader = SessionReader::open(&root, writer.identity()).unwrap();
    let mut view = snapshot(&reader, &reference);
    for n in 0..700 {
        let row = view.next_row().unwrap().unwrap();
        assert_eq!(row.ordinal, n);
        let mut out = [0; 2];
        assert_eq!(view.read_field_range(row.fields[0].as_ref().unwrap(), 0, &mut out, deadline()).unwrap(), 2);
        assert_eq!(out, ((n % 24) as u16).to_le_bytes());
    }
    assert!(view.next_row().unwrap().is_none());
    assert_eq!(
        reader.test_descriptor_counts().1,
        8,
        "gate descriptors and transient bucket/file opens must be measured"
    );
    drop(view);
    assert_eq!(reader.test_descriptor_counts().0, 3);
}

// This is a closed callback sequence fixture; real PG/Owner adapter qualification is a later PR.
#[test]
fn acquire_precedes_snapshot_and_owner_callbacks_and_failure_calls_neither() {
    fn sequence(reader: &SessionReader, reference: &BatchRef, events: &RefCell<Vec<&'static str>>) -> Result<()> {
        let session = reader.begin_read(deadline())?;
        events.borrow_mut().push("gate");
        let pg_snapshot = || {
            assert!(matches!(reader.try_gc_exclusive(deadline()), Err(Error::Busy)));
            events.borrow_mut().push("snapshot");
            reference.clone()
        };
        let reference = pg_snapshot();
        let owner_admission = || {
            events.borrow_mut().push("owner");
        };
        owner_admission();
        let mut view = BatchSnapshot::open(session, &reference, &binding(), deadline())?;
        events.borrow_mut().push("manifest");
        read_literal(&mut view);
        Ok(())
    }
    let (_temp, root, mut writer) = setup();
    let (reference, _) = batch(&mut writer);
    let reader = SessionReader::open(&root, writer.identity()).unwrap();
    let events = RefCell::new(Vec::new());
    sequence(&reader, &reference, &events).unwrap();
    assert_eq!(*events.borrow(), ["gate", "snapshot", "owner", "manifest"]);
    events.borrow_mut().clear();
    let exclusive = reader.try_gc_exclusive(deadline()).unwrap();
    assert!(matches!(sequence(&reader, &reference, &events), Err(Error::Busy)));
    assert!(events.borrow().is_empty(), "gate failure cannot invoke a PG snapshot or Owner callback");
    drop(exclusive);
}

// Real NXV segment expiry cannot remove independent manifest/payload objects, even before lazy reads.
#[test]
fn independent_payload_survives_nxv_day_expiry() {
    use crate::nxv_store::segment::{Segment, parse_day_name};
    use nntp_xover_engine::container::{BlockMeta, NxvBlockBuilder};
    let (temp, root, mut writer) = setup();
    let (reference, data) = batch(&mut writer);
    let nxv = temp.path().join("nxv");
    let (segment, _) = Segment::open(&nxv, parse_day_name("2026-11-08").unwrap()).unwrap();
    let line = b"1\t[1/1] - \"x.rar\" yEnc (1/1)\tposter\tSun, 08 Nov 2026 10:00:00 +0000\t<one@x>\t\t18\t1";
    let block = NxvBlockBuilder::build_hot_block_meta(&[&line[..]], BlockMeta { server_id: 3, slab_gen: 1 }).unwrap();
    let reserved = segment.reserve(&block).unwrap().unwrap();
    reserved.arena.write_all_at(&block, reserved.entry.offset).unwrap();
    reserved.index.write_all_at(&reserved.entry.to_bytes(), 0).unwrap();
    segment.publish(&reserved).unwrap();
    assert_eq!(segment.block_count(), 1);
    let reader = SessionReader::open(&root, writer.identity()).unwrap();
    let mut view = snapshot(&reader, &reference);
    assert!(matches!(reader.try_gc_exclusive(deadline()), Err(Error::Busy)));
    assert!(segment.expire());
    assert!(!nxv.join("2026-11-08.nxv").exists());
    assert!(!nxv.join("2026-11-08.nxvi").exists());
    assert!(object(&root, data.digest).is_file());
    read_literal(&mut view);
}

// Parent unwinding must reap the original real lock owner, not reconstruct a modeled release.
#[test]
fn session_parent_unwind_kills_and_reaps_owned_child() {
    let (temp, root, mut writer) = setup();
    let (reference, _) = batch(&mut writer);
    let reader = SessionReader::open(&root, writer.identity()).unwrap();
    let marker = temp.path().join("unwind");
    let pid = std::cell::Cell::new(None);
    let reached = std::cell::Cell::new(false);
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut process = child("exit", &root, &reference, &marker);
        pid.set(Some(process.id() as libc::pid_t));
        wait_marker(&mut process, &marker, b"root-only");
        assert!(matches!(reader.try_gc_exclusive(deadline()), Err(Error::Busy)));
        reached.set(true);
        panic!("deliberate unwind with an actual child read session");
    }));
    let pid = pid.get().unwrap();
    let absent = unsafe { libc::kill(pid, 0) } == -1 && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
    if !absent {
        unsafe {
            libc::kill(pid, libc::SIGKILL);
        }
        let until = deadline();
        loop {
            let mut status = 0;
            let waited = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
            if waited == pid || (waited == -1 && io::Error::last_os_error().raw_os_error() == Some(libc::ECHILD)) {
                break;
            }
            assert!(Instant::now() < until, "bounded emergency child reap timed out");
            thread::sleep(Duration::from_millis(10));
        }
    }
    assert!(outcome.is_err() && reached.get(), "control must reach deliberate panic after proven child exclusion");
    assert!(absent, "owned child must be dead and reaped after parent unwind");
    assert!(reader.try_gc_exclusive(deadline()).is_ok());
}

#[test]
fn subprocess_entry() {
    let Ok(mode) = std::env::var("ATLAS_SESSION_TEST_MODE") else {
        return;
    };
    let root = PathBuf::from(std::env::var_os("ATLAS_SESSION_TEST_ROOT").unwrap());
    let marker = PathBuf::from(std::env::var_os("ATLAS_SESSION_TEST_MARKER").unwrap());
    fn decode<const N: usize>(value: String) -> [u8; N] {
        assert_eq!(value.len(), N * 2);
        std::array::from_fn(|i| u8::from_str_radix(&value[i * 2..i * 2 + 2], 16).unwrap())
    }
    let identity = Identity::from_bytes(decode(std::env::var("ATLAS_SESSION_TEST_IDENTITY").unwrap()));
    let digest = decode(std::env::var("ATLAS_SESSION_TEST_DIGEST").unwrap());
    let bytes = std::env::var("ATLAS_SESSION_TEST_BYTES").unwrap().parse().unwrap();
    let reference = BatchRef {
        identity,
        binding: binding(),
        generation: Generation(digest),
        root: PayloadRef::from_parts(identity, digest, bytes).unwrap(),
    };
    let reader = SessionReader::open(&root, identity).unwrap();
    let mut view = snapshot(&reader, &reference);
    fs::write(&marker, b"root-only").unwrap();
    let wait = |path: PathBuf, expected: &[u8]| {
        let until = deadline();
        while !marker_ready(&path, expected) {
            assert!(Instant::now() < until, "bounded child command timed out");
            thread::sleep(Duration::from_millis(10));
        }
    };
    wait(marker.with_extension("continue"), b"continue");
    match mode.as_str() {
        "lazy" => {
            read_literal(&mut view);
            fs::write(marker.with_extension("result"), b"literal-read").unwrap();
        }
        "cancel" | "error" => {
            if mode == "error" {
                let row = view.next_row().unwrap().unwrap();
                assert!(matches!(
                    view.read_field_range(row.fields[0].as_ref().unwrap(), 0, &mut [0; 18], deadline()),
                    Err(Error::Missing)
                ));
            }
            drop(view);
            fs::write(marker.with_extension("result"), b"released").unwrap();
            wait(marker.with_extension("finish"), b"finish");
        }
        "exit" => panic!("exit control must be killed while holding its original session"),
        _ => panic!("unknown child mode"),
    }
}
