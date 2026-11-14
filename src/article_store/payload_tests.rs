//! Contract fixtures use real files and an independently known SHA-256 vector.
use super::*;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{self, Read},
    os::unix::{
        ffi::OsStrExt,
        fs::{PermissionsExt, symlink},
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

const ABC: [u8; 32] = [
    0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d, 0xae, 0x22, 0x23, 0xb0, 0x03, 0x61,
    0xa3, 0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61, 0xf2, 0x00, 0x15, 0xad,
];
fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(10)
}
fn catalog() -> CatalogId {
    CatalogId::from_bytes([0x42; 32])
}
fn setup() -> (tempfile::TempDir, PathBuf, Writer) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap().join("payload");
    let writer = Writer::create(&root, catalog()).expect("create a physical payload store");
    (temp, root, writer)
}
fn object(root: &Path, digest: [u8; 32]) -> PathBuf {
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    root.join("chunks").join(&hex[..2]).join(hex)
}
fn abc(writer: &mut Writer) -> PayloadRef {
    let reference = writer.publish(3, &mut &b"abc"[..], deadline()).expect("publish complete bytes");
    assert_eq!(reference.digest, ABC, "digest must match independent SHA-256 vector");
    assert_eq!(reference.bytes, 3);
    reference
}
fn snapshot(path: &Path) -> Vec<(PathBuf, u32, Vec<u8>)> {
    fn visit(base: &Path, path: &Path, out: &mut Vec<(PathBuf, u32, Vec<u8>)>) {
        let mut children: Vec<_> = fs::read_dir(path).unwrap().map(|e| e.unwrap().path()).collect();
        children.sort();
        for p in children {
            let m = fs::symlink_metadata(&p).unwrap();
            out.push((
                p.strip_prefix(base).unwrap().to_owned(),
                m.permissions().mode(),
                if m.is_file() { fs::read(&p).unwrap() } else { Vec::new() },
            ));
            if m.is_dir() {
                visit(base, &p, out);
            }
        }
    }
    let mut out = Vec::new();
    visit(path, path, &mut out);
    out
}

// Catches accepting a different physical store/catalogue or malformed identity bytes.
#[test]
fn identity_binding_survives_reopen_and_refuses_mismatch() {
    let (_temp, root, mut writer) = setup();
    let identity = writer.identity();
    assert_eq!(Identity::from_bytes(identity.to_bytes()), identity);
    let reference = abc(&mut writer);
    assert_eq!(PayloadRef::from_parts(identity, ABC, 3).unwrap(), reference);
    let bytes = fs::read(root.join("identity")).unwrap();
    assert_eq!(bytes.len(), 88);
    assert_eq!(&bytes[..8], b"ATART001");
    assert_eq!(&bytes[24..56], &[0x42; 32]);
    assert_eq!(&bytes[56..], Sha256::digest(&bytes[..56]).as_slice());
    assert_eq!(identity.store.0.as_slice(), &bytes[8..24]);
    drop(writer);
    let writer = Writer::open(&root, identity).expect("reopen matching store");
    assert_eq!(writer.identity(), identity);
    drop(writer);
    let wrong = Identity { catalog: CatalogId::from_bytes([1; 32]), ..identity };
    assert!(matches!(Reader::open(&root, wrong), Err(Error::IdentityMismatch)));
    assert!(matches!(Writer::open(&root, wrong), Err(Error::IdentityMismatch)));
    let wrong_store = Identity { store: StoreId([7; 16]), ..identity };
    assert!(matches!(Reader::open(&root, wrong_store), Err(Error::IdentityMismatch)));
    let reader = Reader::open(&root, identity).unwrap();
    assert!(matches!(
        reader.open_chunk(PayloadRef { identity: wrong_store, ..reference }, deadline()),
        Err(Error::IdentityMismatch)
    ));
    drop(reader);
    for damaged in [bytes[..87].to_vec(), [bytes.clone(), vec![0]].concat(), {
        let mut b = bytes.clone();
        b[12] ^= 1;
        b
    }] {
        fs::write(root.join("identity"), damaged).unwrap();
        assert!(matches!(Reader::open(&root, identity), Err(Error::Corrupt)));
    }
}

// Catches destructive initialization, weak modes, and read-only paths that create files.
#[test]
fn readonly_open_leaves_directory_contents_unchanged() {
    let (temp, root, mut writer) = setup();
    let identity = writer.identity();
    let reference = abc(&mut writer);
    drop(writer);
    assert_eq!(fs::metadata(&root).unwrap().permissions().mode() & 0o777, 0o700);
    for (p, mode, _) in snapshot(&root) {
        assert_eq!(mode & 0o777, if root.join(p).is_dir() { 0o700 } else { 0o600 });
    }
    let before = snapshot(&root);
    let reader = Reader::open(&root, identity).unwrap();
    let chunk = reader.open_chunk(reference, deadline()).unwrap();
    let mut out = [0; 3];
    assert_eq!(chunk.read_range(0, &mut out, deadline()).unwrap(), 3);
    assert_eq!(&out, b"abc");
    drop(chunk);
    drop(reader);
    assert_eq!(snapshot(&root), before);
    assert!(Writer::create(&root, catalog()).is_err());
    assert_eq!(snapshot(&root), before);
    let missing = temp.path().join("missing");
    assert!(matches!(Reader::open(&missing, identity), Err(Error::Missing)));
    assert!(!missing.exists());
    fs::remove_file(root.join("identity")).unwrap();
    let before = snapshot(&root);
    assert!(matches!(Reader::open(&root, identity), Err(Error::Missing)));
    assert_eq!(snapshot(&root), before);
}

// Catches overwriting a digest-named corrupt object or trusting its filename.
#[test]
fn existing_object_is_verified_never_replaced() {
    let (_temp, root, mut writer) = setup();
    let identity = writer.identity();
    let path = object(&root, ABC);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, b"bad").unwrap();
    assert!(matches!(writer.publish(3, &mut &b"abc"[..], deadline()), Err(Error::Corrupt)));
    assert_eq!(fs::read(&path).unwrap(), b"bad");
    let reference = PayloadRef { identity, digest: ABC, bytes: 3 };
    let reader = Reader::open(&root, identity).unwrap();
    assert!(matches!(reader.open_chunk(reference, deadline()), Err(Error::Corrupt)));
    fs::write(&path, b"abc-extra").unwrap();
    assert!(matches!(reader.open_chunk(reference, deadline()), Err(Error::Corrupt)));
    fs::remove_file(&path).unwrap();
    let reference = abc(&mut writer);
    assert_eq!(abc(&mut writer), reference);
    fs::remove_file(&path).unwrap();
    assert!(matches!(reader.open_chunk(reference, deadline()), Err(Error::Missing)));
}

struct Failing;
impl Read for Failing {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        Err(io::Error::from_raw_os_error(libc::EIO))
    }
}
struct Delayed;
impl Read for Delayed {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        thread::sleep(Duration::from_millis(30));
        if out.is_empty() {
            return Ok(0);
        }
        out[0] = 1;
        Ok(1)
    }
}
// Catches truncation, over-reading, offset arithmetic overflow and deadline bypasses.
#[test]
fn range_and_length_limits_fail_closed() {
    let (_temp, root, mut writer) = setup();
    let identity = writer.identity();
    assert!(matches!(writer.publish(MAX_CHUNK_BYTES + 1, &mut &b""[..], deadline()), Err(Error::Limit)));
    assert!(matches!(writer.publish(4, &mut &b"abc"[..], deadline()), Err(Error::Corrupt)));
    assert!(matches!(writer.publish(2, &mut &b"abc"[..], deadline()), Err(Error::Corrupt)));
    assert!(matches!(writer.publish(1,&mut Failing,deadline()),Err(Error::Io(e)) if e.raw_os_error()==Some(libc::EIO)));
    let past = Instant::now() - Duration::from_secs(1);
    assert!(matches!(writer.publish(3, &mut &b"abc"[..], past), Err(Error::Deadline)));
    assert!(matches!(writer.publish(1, &mut Delayed, Instant::now() + Duration::from_millis(5)), Err(Error::Deadline)));
    let reference = abc(&mut writer);
    let reader = Reader::open(&root, identity).unwrap();
    assert!(matches!(reader.open_chunk(reference, past), Err(Error::Deadline)));
    assert!(matches!(
        reader.open_chunk(PayloadRef { bytes: MAX_CHUNK_BYTES + 1, ..reference }, deadline()),
        Err(Error::Limit)
    ));
    assert!(matches!(reader.open_chunk(PayloadRef { bytes: 2, ..reference }, deadline()), Err(Error::Corrupt)));
    let chunk = reader.open_chunk(reference, deadline()).unwrap();
    let mut out = [0; 5];
    assert_eq!(chunk.read_range(1, &mut out, deadline()).unwrap(), 2);
    assert_eq!(&out[..2], b"bc");
    assert_eq!(chunk.read_range(3, &mut out, deadline()).unwrap(), 0);
    assert_eq!(chunk.read_range(0, &mut [], deadline()).unwrap(), 0);
    for offset in [4, u64::MAX] {
        assert!(matches!(chunk.read_range(offset, &mut [], deadline()), Err(Error::Limit)));
        assert!(matches!(chunk.read_range(offset, &mut out, deadline()), Err(Error::Limit)));
    }
    assert!(matches!(chunk.read_range(0, &mut vec![0; MAX_RANGE_BYTES + 1], deadline()), Err(Error::Limit)));
    assert!(matches!(chunk.read_range(0, &mut out, past), Err(Error::Deadline)));
    fs::OpenOptions::new().write(true).open(object(&root, ABC)).unwrap().set_len(1).unwrap();
    assert!(matches!(chunk.read_range(0, &mut out, deadline()), Err(Error::Corrupt)));
    let empty = writer.publish(0, &mut &b""[..], deadline()).unwrap();
    assert_eq!(
        empty.digest,
        [
            0xe3, 0xb0, 0xc4, 0x42, 0x98, 0xfc, 0x1c, 0x14, 0x9a, 0xfb, 0xf4, 0xc8, 0x99, 0x6f, 0xb9, 0x24, 0x27, 0xae,
            0x41, 0xe4, 0x64, 0x9b, 0x93, 0x4c, 0xa4, 0x95, 0x99, 0x1b, 0x78, 0x52, 0xb8, 0x55
        ]
    );
}

fn pattern(offset: u64) -> u8 {
    match offset % 5 {
        0 => 0,
        1 => 255,
        2 => 128,
        _ => (offset.wrapping_mul(37) & 255) as u8,
    }
}
struct Generated {
    position: u64,
    remaining: u64,
    max_read: usize,
}
impl Read for Generated {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        self.max_read = self.max_read.max(out.len());
        let n = out.len().min(self.remaining as usize);
        for (i, b) in out[..n].iter_mut().enumerate() {
            *b = pattern(self.position + i as u64);
        }
        self.position += n as u64;
        self.remaining -= n as u64;
        Ok(n)
    }
}
// Catches whole-field allocation, UTF-8 assumptions and chunk/range boundary loss.
#[test]
fn opaque_bytes_and_long_fields_roundtrip() {
    let (_temp, root, mut writer) = setup();
    let reader = Reader::open(&root, writer.identity()).unwrap();
    let total = 3 * 1024 * 1024 + 137u64;
    let mut position = 0;
    let mut max_read = 0;
    while position < total {
        let bytes = (total - position).min(MAX_CHUNK_BYTES as u64) as u32;
        let mut source = Generated { position, remaining: bytes as u64, max_read: 0 };
        let reference = writer.publish(bytes, &mut source, deadline()).unwrap();
        max_read = max_read.max(source.max_read);
        assert_eq!(source.remaining, 0);
        let chunk = reader.open_chunk(reference, deadline()).unwrap();
        let mut offset = 0;
        let mut buffer = [0; MAX_RANGE_BYTES];
        while offset < bytes as u64 {
            let n = chunk.read_range(offset, &mut buffer, deadline()).unwrap();
            assert_eq!(n, buffer.len().min(bytes as usize - offset as usize));
            for (i, b) in buffer[..n].iter().enumerate() {
                assert_eq!(*b, pattern(position + offset + i as u64));
            }
            offset += n as u64;
        }
        position += bytes as u64;
    }
    assert!(max_read <= MAX_RANGE_BYTES, "source read request exceeded 64 KiB: {max_read}");
}

// Catches reopening a pathname instead of retaining the verified descriptor.
#[test]
fn pinned_chunk_survives_replacement_unlink_and_root_substitution() {
    let (temp, root, mut writer) = setup();
    let reference = abc(&mut writer);
    let reader = Reader::open(&root, writer.identity()).unwrap();
    let chunk = reader.open_chunk(reference, deadline()).unwrap();
    let moved = temp.path().join("moved");
    fs::rename(&root, &moved).unwrap();
    fs::create_dir(&root).unwrap();
    let path = object(&moved, ABC);
    fs::remove_file(&path).unwrap();
    fs::write(&path, b"bad").unwrap();
    fs::remove_file(&path).unwrap();
    let mut out = [0; 3];
    assert_eq!(chunk.read_range(0, &mut out, deadline()).unwrap(), 3);
    assert_eq!(&out, b"abc");
    let next = writer.publish(3, &mut &b"xyz"[..], deadline()).unwrap();
    assert!(object(&moved, next.digest).exists());
    assert!(!root.join("chunks").exists());
    let next = reader.open_chunk(next, deadline()).unwrap();
    assert_eq!(next.read_range(0, &mut out, deadline()).unwrap(), 3);
    assert_eq!(&out, b"xyz");
}

// Catches lazy snapshots bypassing the handle's eight-descriptor admission bound.
#[test]
fn retained_chunks_are_counted_in_handle_fd_limit() {
    let (_temp, root, mut writer) = setup();
    let reference = abc(&mut writer);
    let reader = Reader::open(&root, writer.identity()).unwrap();
    let mut chunks = Vec::new();
    loop {
        match reader.open_chunk(reference, deadline()) {
            Ok(chunk) => {
                chunks.push(chunk);
                assert!(chunks.len() < MAX_OWNED_FDS, "root descriptor plus chunks exceeded eight FDs");
            }
            Err(Error::Limit | Error::Busy) => break,
            Err(error) => panic!("unexpected admission error: {error}"),
        }
    }
    assert!(!chunks.is_empty());
    chunks.pop();
    assert!(reader.open_chunk(reference, deadline()).is_ok());
    drop(reader);
    let mut out = [0; 3];
    assert_eq!(chunks[0].read_range(0, &mut out, deadline()).unwrap(), 3);
    assert_eq!(&out, b"abc");
}

fn fifo(path: &Path) {
    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
}
// Catches following unsafe root/child components, FIFO blocking and path escapes.
#[test]
fn unsafe_paths_are_refused() {
    let (temp, root, mut writer) = setup();
    let identity = writer.identity();
    let reference = abc(&mut writer);
    drop(writer);
    let alias = temp.path().join("alias");
    symlink(&root, &alias).unwrap();
    assert!(Reader::open(&alias, identity).is_err());
    assert!(Writer::open(&alias, identity).is_err());
    let nested = alias.join("new");
    assert!(Writer::create(&nested, catalog()).is_err());
    assert!(!root.join("new").exists());
    let identity_path = root.join("identity");
    let original = fs::read(&identity_path).unwrap();
    fs::remove_file(&identity_path).unwrap();
    fifo(&identity_path);
    assert!(Reader::open(&root, identity).is_err());
    fs::remove_file(&identity_path).unwrap();
    fs::write(&identity_path, original).unwrap();
    let lock = root.join("writer.lock");
    fs::remove_file(&lock).unwrap();
    fifo(&lock);
    assert!(Writer::open(&root, identity).is_err());
    fs::remove_file(&lock).unwrap();
    fs::write(&lock, []).unwrap();
    let reader = Reader::open(&root, identity).unwrap();
    let path = object(&root, ABC);
    fs::remove_file(&path).unwrap();
    fifo(&path);
    assert!(reader.open_chunk(reference, deadline()).is_err());
    fs::remove_file(&path).unwrap();
    let outside = temp.path().join("outside");
    fs::write(&outside, b"abc").unwrap();
    symlink(&outside, &path).unwrap();
    assert!(reader.open_chunk(reference, deadline()).is_err());
    fs::remove_file(&path).unwrap();
    fs::remove_dir(path.parent().unwrap()).unwrap();
    symlink(temp.path(), path.parent().unwrap()).unwrap();
    assert!(reader.open_chunk(reference, deadline()).is_err());
    drop(reader);
    fs::remove_file(path.parent().unwrap()).unwrap();
    fs::remove_dir(root.join("chunks")).unwrap();
    symlink(temp.path(), root.join("chunks")).unwrap();
    assert!(Reader::open(&root, identity).is_err());
    assert_eq!(fs::read(outside).unwrap(), b"abc");
}

struct OwnedChild(Child);
impl std::ops::Deref for OwnedChild {
    type Target = Child;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl std::ops::DerefMut for OwnedChild {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}
impl Drop for OwnedChild {
    fn drop(&mut self) {}
}
fn child(mode: &str, root: &Path, identity: Identity, marker: &Path) -> OwnedChild {
    OwnedChild(
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "article_store::payload_tests::subprocess_entry", "--nocapture"])
            .env("ATLAS_PAYLOAD_TEST_MODE", mode)
            .env("ATLAS_PAYLOAD_TEST_ROOT", root)
            .env("ATLAS_PAYLOAD_TEST_MARKER", marker)
            .env("ATLAS_PAYLOAD_TEST_STORE", identity.store.0.iter().map(|b| format!("{b:02x}")).collect::<String>())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    )
}
fn wait_exit(child: &mut Child) -> std::process::ExitStatus {
    let until = deadline();
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        if Instant::now() >= until {
            let _ = child.kill();
            let _ = child.wait();
            panic!("bounded subprocess did not exit");
        }
        thread::sleep(Duration::from_millis(10));
    }
}
fn marker_ready(path: &Path, expected: &[u8]) -> bool {
    let _ = expected;
    path.exists()
}
fn wait_marker(child: &mut Child, path: &Path, expected: &[u8]) {
    let until = deadline();
    loop {
        if marker_ready(path, expected) {
            return;
        }
        if let Some(status) = child.try_wait().unwrap() {
            panic!("child exited before semantic boundary marker: {status}");
        }
        if Instant::now() >= until {
            let _ = child.kill();
            let _ = child.wait();
            panic!("bounded subprocess did not reach semantic boundary");
        }
        thread::sleep(Duration::from_millis(10));
    }
}

// Catches process-local-only writer locks; explicit child result avoids generic failure evidence.
#[test]
fn single_writer_exclusion_cross_process() {
    let (temp, root, writer) = setup();
    let identity = writer.identity();
    let marker = temp.path().join("busy");
    let mut process = child("busy", &root, identity, &marker);
    assert!(wait_exit(&mut process).success());
    assert_eq!(fs::read(&marker).unwrap(), b"Busy");
    drop(writer);
    fs::remove_file(&marker).unwrap();
    let mut process = child("acquire", &root, identity, &marker);
    assert!(wait_exit(&mut process).success());
    assert_eq!(fs::read(&marker).unwrap(), b"Acquired");
}

// Catches returning success before directory sync and exposes actual install boundaries.
#[test]
fn publish_crash_boundaries() {
    for boundary in ["pre-install", "post-install", "post-directory-sync"] {
        let (temp, root, writer) = setup();
        let identity = writer.identity();
        drop(writer);
        let marker = temp.path().join("boundary");
        let mut process = child(boundary, &root, identity, &marker);
        wait_marker(&mut process, &marker, boundary.as_bytes());
        assert_eq!(fs::read(&marker).unwrap(), boundary.as_bytes());
        assert!(!marker.with_extension("returned").exists());
        process.kill().unwrap();
        let _ = process.wait().unwrap();
        let path = object(&root, ABC);
        if boundary == "pre-install" {
            assert!(!path.exists());
        } else {
            assert_eq!(fs::read(&path).unwrap(), b"abc");
        }
        let mut writer = Writer::open(&root, identity).unwrap();
        let replay = abc(&mut writer);
        assert_eq!(replay.identity, identity);
        drop(writer);
        let reader = Reader::open(&root, identity).unwrap();
        let result = reader.open_chunk(PayloadRef { identity, digest: ABC, bytes: 3 }, deadline());
        {
            let chunk = result.unwrap();
            let mut out = [0; 3];
            assert_eq!(chunk.read_range(0, &mut out, deadline()).unwrap(), 3);
            assert_eq!(&out, b"abc");
        }
    }
}

// Catches swallowing a real sync error or retrying publication after durability uncertainty.
#[test]
fn sync_failure_stops_writer() {
    let (_temp, _root, mut writer) = setup();
    payload::test_fail_next_sync();
    let before = payload::test_sync_calls();
    assert!(
        matches!(writer.publish(3,&mut &b"abc"[..],deadline()),Err(Error::Io(e)) if e.raw_os_error()==Some(libc::EIO))
    );
    assert!(payload::test_sync_calls() > before);
    let calls = payload::test_sync_calls();
    assert!(matches!(writer.publish(3, &mut &b"abc"[..], deadline()), Err(Error::Busy)));
    assert_eq!(payload::test_sync_calls(), calls);
    drop(writer);
    let (_other_temp, other_root, mut writer) = setup();
    let reference = abc(&mut writer);
    payload::test_fail_final_sync();
    assert!(
        matches!(writer.publish(3, &mut &b"abc"[..], deadline()), Err(Error::Io(e)) if e.raw_os_error() == Some(libc::EIO))
    );
    assert_eq!(fs::read(object(&other_root, ABC)).unwrap(), b"abc");
    assert!(matches!(writer.publish(3, &mut &b"abc"[..], deadline()), Err(Error::Busy)));
    let reader = Reader::open(&other_root, reference.identity).unwrap();
    assert!(reader.open_chunk(reference, deadline()).is_ok());
}

// Catches admitting a writer without durably reconciling an interrupted root mkdir.
#[test]
fn interrupted_initialization_reconciles_parent_durability() {
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().canonicalize().unwrap();
    let root = base.join("payload");
    let marker = base.join("initialized");
    let placeholder = Identity::from_parts(catalog(), StoreId::from_bytes([0; 16]));
    let mut process = child("post-identity-install", &root, placeholder, &marker);
    wait_marker(&mut process, &marker, b"post-identity-install");
    assert_eq!(fs::read(&marker).unwrap(), b"post-identity-install");
    assert!(!marker.with_extension("returned").exists());
    process.kill().unwrap();
    let _ = process.wait().unwrap();
    let record = fs::read(root.join("identity")).unwrap();
    assert_eq!(record.len(), 88);
    assert_eq!(&record[..8], b"ATART001");
    assert_eq!(&record[24..56], &[0x42; 32]);
    assert_eq!(&record[56..], Sha256::digest(&record[..56]).as_slice());
    let mut store = [0; 16];
    store.copy_from_slice(&record[8..24]);
    let identity = Identity::from_parts(catalog(), StoreId::from_bytes(store));
    let before = snapshot(&root);
    payload::test_fail_parent_sync();
    let before_reader_syncs = payload::test_sync_calls();
    let reader_result = Reader::open(&root, identity);
    let after_reader_syncs = payload::test_sync_calls();
    let reopen_result = Writer::open(&root, identity);
    // Clear even when the missing sync leaves the targeted failure unconsumed.
    payload::test_clear_sync_failure();
    assert_eq!(after_reader_syncs, before_reader_syncs, "read-only reopening cannot invoke a sync primitive");
    assert!(reader_result.is_ok(), "read-only reopening cannot perform a durability sync");
    drop(reader_result);
    assert_eq!(snapshot(&root), before);
    assert!(
        matches!(reopen_result, Err(Error::Io(ref error)) if error.raw_os_error() == Some(libc::EIO)),
        "writer admission must preserve EIO from the actual root-parent sync"
    );
    drop(reopen_result);
    let mut writer = Writer::open(&root, identity).expect("reconcile interrupted initialization");
    let reference = abc(&mut writer);
    drop(writer);
    let reader = Reader::open(&root, identity).unwrap();
    let chunk = reader.open_chunk(reference, deadline()).unwrap();
    let mut out = [0; 3];
    assert_eq!(chunk.read_range(0, &mut out, deadline()).unwrap(), 3);
    assert_eq!(&out, b"abc");
}

// Catches accepting the pathname before its complete semantic signal is visible.
#[test]
fn marker_readiness_requires_complete_expected_bytes() {
    let temp = tempfile::tempdir().unwrap();
    let marker = temp.path().join("boundary");
    let mut observed = vec![marker_ready(&marker, b"post-install")];
    for bytes in [&b""[..], &b"post-"[..], &b"pre-install"[..], &b"post-install"[..]] {
        fs::write(&marker, bytes).unwrap();
        observed.push(marker_ready(&marker, b"post-install"));
    }
    assert_eq!(
        observed,
        [false, false, false, false, true],
        "marker readiness requires complete expected bytes, including empty/partial rejection"
    );
}

// Catches std::process::Child Drop leaving a parked lock-owning subprocess alive.
#[test]
fn parent_unwind_kills_and_reaps_owned_child() {
    let (temp, root, writer) = setup();
    let identity = writer.identity();
    drop(writer);
    let marker = temp.path().join("holding");
    let pid = std::cell::Cell::new(None);
    let reached_panic = std::cell::Cell::new(false);
    let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut process = child("park-holding-lock", &root, identity, &marker);
        pid.set(Some(process.id() as libc::pid_t));
        wait_marker(&mut process, &marker, b"Holding");
        assert_eq!(fs::read(&marker).unwrap(), b"Holding");
        assert!(matches!(Writer::open(&root, identity), Err(Error::Busy)));
        reached_panic.set(true);
        panic!("deliberate parent unwind after actual child lock admission");
    }));
    let pid = pid.get().expect("child was spawned before the deliberate unwind");
    // ESRCH proves both death and reaping; a zombie still responds to kill(pid, 0).
    let absent = unsafe { libc::kill(pid, 0) } == -1 && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
    let lock_released = Writer::open(&root, identity).is_ok();
    // Emergency cleanup keeps this regression bounded even before OwnedChild Drop
    // is implemented. It occurs only after recording the failed cleanup oracle.
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
    assert!(reached_panic.get(), "control must reach the exact lock-holding semantic boundary");
    assert!(unwound.is_err(), "control must exercise a real parent panic");
    assert!(absent && lock_released, "parent unwind must kill and reap its owned child before returning");
    let writer = Writer::open(&root, identity).expect("no child may retain the writer lock");
    drop(writer);
}

#[test]
fn subprocess_entry() {
    let Ok(mode) = std::env::var("ATLAS_PAYLOAD_TEST_MODE") else {
        return;
    };
    let root = PathBuf::from(std::env::var_os("ATLAS_PAYLOAD_TEST_ROOT").unwrap());
    let marker = PathBuf::from(std::env::var_os("ATLAS_PAYLOAD_TEST_MARKER").unwrap());
    let encoded = std::env::var("ATLAS_PAYLOAD_TEST_STORE").unwrap();
    let mut store = [0; 16];
    for (i, b) in store.iter_mut().enumerate() {
        *b = u8::from_str_radix(&encoded[i * 2..i * 2 + 2], 16).unwrap();
    }
    let identity = Identity { catalog: catalog(), store: StoreId(store) };
    if mode == "post-identity-install" {
        let _writer = Writer::create(&root, catalog()).unwrap();
        fs::write(marker.with_extension("returned"), b"returned").unwrap();
        return;
    }
    if mode == "busy" {
        assert!(matches!(Writer::open(&root, identity), Err(Error::Busy)));
        fs::write(marker, b"Busy").unwrap();
        return;
    }
    let mut writer = Writer::open(&root, identity).unwrap();
    if mode == "park-holding-lock" {
        fs::write(marker, b"Holding").unwrap();
        loop {
            thread::park();
        }
    }
    if mode == "acquire" {
        fs::write(marker, b"Acquired").unwrap();
        return;
    }
    let _reference = abc(&mut writer);
    fs::write(marker.with_extension("returned"), b"returned").unwrap();
}
