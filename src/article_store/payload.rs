//! Descriptor-relative immutable files. Deadlines are cooperative checks around bounded
//! synchronous operations; they cannot interrupt an arbitrary blocked Read or fsync.

#[cfg(unix)]
mod unix {
    use super::super::{
        CatalogId, Error, Identity, MAX_CHUNK_BYTES, MAX_OWNED_FDS, MAX_RANGE_BYTES, PayloadRef, Result, StoreId,
    };
    use sha2::{Digest, Sha256};
    use std::{
        cell::Cell,
        ffi::{CStr, CString, OsStr},
        fs::{File, Metadata},
        io::{self, Read, Write},
        os::{
            fd::{AsRawFd, FromRawFd, RawFd},
            unix::{
                ffi::OsStrExt,
                fs::{FileExt, MetadataExt},
            },
        },
        path::{Component, Path},
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::Instant,
    };

    /// Every owned descriptor, including path-walk/transient descriptors and returned
    /// chunks, holds a permit. Chunks keep the originating budget alive after Reader drops.
    #[derive(Default)]
    struct Budget(AtomicUsize);
    struct Permit(Arc<Budget>);
    impl Budget {
        fn acquire(self: &Arc<Self>) -> Result<Permit> {
            self.0
                .try_update(Ordering::AcqRel, Ordering::Acquire, |count| (count < MAX_OWNED_FDS).then_some(count + 1))
                .map_err(|_| Error::Limit)?;
            Ok(Permit(self.clone()))
        }
    }
    impl Drop for Permit {
        fn drop(&mut self) {
            self.0.0.fetch_sub(1, Ordering::AcqRel);
        }
    }
    struct Pinned {
        file: File,
        _permit: Permit,
    }
    impl AsRawFd for Pinned {
        fn as_raw_fd(&self) -> RawFd {
            self.file.as_raw_fd()
        }
    }

    fn check(deadline: Instant) -> Result<()> {
        if Instant::now() >= deadline { Err(Error::Deadline) } else { Ok(()) }
    }
    fn path_error(error: io::Error) -> Error {
        match error.raw_os_error() {
            Some(libc::ENOENT) => Error::Missing,
            Some(libc::ELOOP | libc::ENOTDIR) => Error::Corrupt,
            _ => Error::Io(error),
        }
    }
    fn name(value: &OsStr) -> Result<CString> {
        if value.as_bytes().len() > 255 {
            return Err(Error::Limit);
        }
        CString::new(value.as_bytes()).map_err(|_| Error::Corrupt)
    }
    fn open_at(dir: RawFd, name: &CStr, flags: i32, budget: &Arc<Budget>) -> Result<Pinned> {
        let permit = budget.acquire()?;
        // SAFETY: name is NUL-terminated; returned descriptor is exclusively owned below.
        let fd = unsafe {
            libc::openat(dir, name.as_ptr(), flags | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK, 0o600)
        };
        if fd < 0 {
            return Err(path_error(io::Error::last_os_error()));
        }
        // SAFETY: openat returned a new owned descriptor.
        let file = unsafe { File::from_raw_fd(fd) };
        let metadata = file.metadata()?;
        if (flags & libc::O_DIRECTORY != 0 && !metadata.is_dir())
            || (flags & libc::O_DIRECTORY == 0 && !metadata.is_file())
        {
            return Err(Error::Corrupt);
        }
        Ok(Pinned { file, _permit: permit })
    }
    fn directory(dir: RawFd, name: &CStr, budget: &Arc<Budget>) -> Result<Pinned> {
        open_at(dir, name, libc::O_RDONLY | libc::O_DIRECTORY, budget)
    }
    fn mkdir_at(dir: RawFd, name: &CStr, exclusive: bool) -> Result<()> {
        // SAFETY: dir is pinned by the caller; name is a single NUL-terminated component.
        if unsafe { libc::mkdirat(dir, name.as_ptr(), 0o700) } == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if !exclusive && error.raw_os_error() == Some(libc::EEXIST) { Ok(()) } else { Err(path_error(error)) }
    }
    /// Walk every component without following symbolic links. For create, only the
    /// final component is initialized. Retain the final pinned parent for writer
    /// admission to reconcile an initialization interrupted before its parent sync.
    fn root_directory(root: &Path, create: bool, budget: &Arc<Budget>) -> Result<(Pinned, Option<Pinned>)> {
        let mut current = directory(libc::AT_FDCWD, if root.is_absolute() { c"/" } else { c"." }, budget)?;
        let mut components = root
            .components()
            .filter(|component| !matches!(component, Component::RootDir | Component::CurDir))
            .peekable();
        while let Some(component) = components.next() {
            let Component::Normal(component) = component else {
                return Err(Error::Corrupt);
            };
            let component = name(component)?;
            let last = components.peek().is_none();
            if create && last {
                mkdir_at(current.as_raw_fd(), &component, true)?;
            }
            let next = directory(current.as_raw_fd(), &component, budget)?;
            if last {
                return Ok((next, Some(current)));
            }
            current = next;
        }
        Err(Error::Corrupt)
    }
    fn random<const N: usize>() -> Result<[u8; N]> {
        let mut bytes = [0; N];
        getrandom::fill(&mut bytes).map_err(|error| Error::Io(io::Error::other(error.to_string())))?;
        Ok(bytes)
    }
    fn hex(bytes: &[u8]) -> String {
        const DIGITS: &[u8; 16] = b"0123456789abcdef";
        let mut out = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            out.push(DIGITS[(byte >> 4) as usize] as char);
            out.push(DIGITS[(byte & 15) as usize] as char);
        }
        out
    }
    fn object_names(digest: &[u8; 32]) -> (CString, CString) {
        let value = hex(digest);
        (CString::new(&value[..2]).expect("hex"), CString::new(value).expect("hex"))
    }

    #[derive(Clone, Copy, Eq, PartialEq)]
    struct Stable {
        dev: u64,
        ino: u64,
        mode: u32,
        bytes: u64,
        mtime: i64,
        nanos: i64,
    }
    fn stable(file: &File) -> Result<Stable> {
        let metadata: Metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(Error::Corrupt);
        }
        // Link count/ctime change when a name is unlinked or replaced. Those are
        // permitted for a pinned snapshot; length and modification time are not.
        Ok(Stable {
            dev: metadata.dev(),
            ino: metadata.ino(),
            mode: metadata.mode(),
            bytes: metadata.len(),
            mtime: metadata.mtime(),
            nanos: metadata.mtime_nsec(),
        })
    }
    fn read_at(file: &File, out: &mut [u8], offset: u64, deadline: Instant) -> Result<usize> {
        loop {
            check(deadline)?;
            let read = file.read_at(out, offset);
            match read {
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(Error::Io(error)),
                Ok(count) => {
                    check(deadline)?;
                    return Ok(count);
                }
            }
        }
    }
    fn verify(file: &File, bytes: u32, digest: &[u8; 32], deadline: Instant) -> Result<Stable> {
        check(deadline)?;
        if bytes > MAX_CHUNK_BYTES {
            return Err(Error::Limit);
        }
        let before = stable(file)?;
        if before.bytes != bytes as u64 {
            return Err(Error::Corrupt);
        }
        let mut buffer = [0; MAX_RANGE_BYTES];
        let mut hash = Sha256::new();
        let mut offset = 0;
        while offset < bytes as u64 {
            let want = buffer.len().min(bytes as usize - offset as usize);
            let count = read_at(file, &mut buffer[..want], offset, deadline)?;
            if count == 0 {
                return Err(Error::Corrupt);
            }
            hash.update(&buffer[..count]);
            offset += count as u64;
        }
        if hash.finalize().as_slice() != digest || stable(file)? != before {
            return Err(Error::Corrupt);
        }
        check(deadline)?;
        Ok(before)
    }
    fn identity_at(root: &Pinned, budget: &Arc<Budget>) -> Result<(Identity, Pinned)> {
        let identity = open_at(root.as_raw_fd(), c"identity", libc::O_RDONLY, budget)?;
        let before = stable(&identity.file)?;
        if before.bytes != 88 {
            return Err(Error::Corrupt);
        }
        let mut bytes = [0; 88];
        // The identity is a fixed bounded record. No deadline is part of open's API.
        (&identity.file).read_exact(&mut bytes).map_err(|error| {
            if error.kind() == io::ErrorKind::UnexpectedEof { Error::Corrupt } else { Error::Io(error) }
        })?;
        if stable(&identity.file)? != before
            || &bytes[..8] != b"ATART001"
            || Sha256::digest(&bytes[..56]).as_slice() != &bytes[56..]
        {
            return Err(Error::Corrupt);
        }
        let mut label = [0; 48];
        label.copy_from_slice(&bytes[8..56]);
        Ok((Identity::from_bytes(label), identity))
    }
    fn lock_at(root: &Pinned, create: bool, budget: &Arc<Budget>) -> Result<Pinned> {
        let flags = libc::O_RDWR | if create { libc::O_CREAT | libc::O_EXCL } else { 0 };
        let lock = open_at(root.as_raw_fd(), c"writer.lock", flags, budget)?;
        // SAFETY: the descriptor is a regular file owned by lock.
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            let error = io::Error::last_os_error();
            if matches!(error.raw_os_error(), Some(libc::EWOULDBLOCK)) {
                return Err(Error::Busy);
            }
            return Err(Error::Io(error));
        }
        Ok(lock)
    }

    /// Private temporary-name cleanup only. This never scans or collects crash leftovers.
    struct TempName {
        dir: RawFd,
        name: CString,
        linked: bool,
    }
    impl TempName {
        fn create(dir: &Pinned, budget: &Arc<Budget>) -> Result<(Self, Pinned)> {
            let name = CString::new(format!(".tmp-{}", hex(&random::<16>()?))).expect("hex");
            let file = open_at(dir.as_raw_fd(), &name, libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL, budget)?;
            Ok((Self { dir: dir.as_raw_fd(), name, linked: true }, file))
        }
        fn unlink(&mut self) -> Result<()> {
            // SAFETY: the parent directory remains pinned for this temporary's lifetime.
            if unsafe { libc::unlinkat(self.dir, self.name.as_ptr(), 0) } != 0 {
                return Err(Error::Io(io::Error::last_os_error()));
            }
            self.linked = false;
            Ok(())
        }
    }
    impl Drop for TempName {
        fn drop(&mut self) {
            if self.linked {
                // SAFETY: normal lexical ownership keeps dir alive until this drop.
                unsafe {
                    libc::unlinkat(self.dir, self.name.as_ptr(), 0);
                }
            }
        }
    }
    fn install(temp: &TempName, parent: &Pinned, final_name: &CStr) -> Result<bool> {
        // SAFETY: both directories are pinned and both names are valid single components.
        if unsafe { libc::linkat(temp.dir, temp.name.as_ptr(), parent.as_raw_fd(), final_name.as_ptr(), 0) } == 0 {
            return Ok(true);
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::EEXIST) { Ok(false) } else { Err(Error::Io(error)) }
    }

    #[derive(Clone, Copy, Eq, PartialEq)]
    enum SyncPoint {
        Other,
        Selected,
        Parent,
    }
    pub(crate) struct Writer {
        root: Pinned,
        chunks: Pinned,
        _lock: Pinned,
        identity: Identity,
        budget: Arc<Budget>,
        poisoned: Cell<bool>,
    }
    pub(crate) struct Reader {
        root: Pinned,
        chunks: Pinned,
        identity: Identity,
        budget: Arc<Budget>,
    }
    /// An original verified descriptor, immune to pathname replacement/unlink.
    /// The private writer never mutates installed payload inodes in place.
    pub(crate) struct Chunk {
        file: Pinned,
        before: Stable,
        bytes: u32,
    }

    impl Writer {
        fn flush(&self, file: &File, deadline: Option<Instant>, point: SyncPoint) -> Result<()> {
            if let Some(deadline) = deadline {
                check(deadline)?;
            }
            #[cfg(not(test))]
            let _ = point;
            #[cfg(test)]
            let result = sync_primitive(file, point);
            #[cfg(not(test))]
            let result = crate::nxv_store::sync::full_flush(file);
            if let Err(error) = result {
                self.poisoned.set(true);
                return Err(Error::Io(error));
            }
            if let Some(deadline) = deadline {
                check(deadline)?;
            }
            Ok(())
        }
        pub(crate) fn create(root: &Path, catalog: CatalogId) -> Result<Self> {
            let budget = Arc::new(Budget::default());
            let (root, parent) = root_directory(root, true, &budget)?;
            let parent = parent.expect("created root has a pinned parent");
            mkdir_at(root.as_raw_fd(), c"chunks", true)?;
            let chunks = directory(root.as_raw_fd(), c"chunks", &budget)?;
            let lock = lock_at(&root, true, &budget)?;
            let identity = Identity::from_parts(catalog, StoreId::from_bytes(random()?));
            let writer = Self { root, chunks, _lock: lock, identity, budget, poisoned: Cell::new(false) };
            let (mut temp, mut file) = TempName::create(&writer.root, &writer.budget)?;
            let mut bytes = [0; 88];
            bytes[..8].copy_from_slice(b"ATART001");
            bytes[8..56].copy_from_slice(&identity.to_bytes());
            let digest = Sha256::digest(&bytes[..56]);
            bytes[56..].copy_from_slice(&digest);
            file.file.write_all(&bytes)?;
            writer.flush(&file.file, None, SyncPoint::Other)?;
            drop(file);
            if !install(&temp, &writer.root, c"identity")? {
                return Err(Error::Corrupt);
            }
            #[cfg(test)]
            crash_boundary("post-identity-install");
            temp.unlink()?;
            writer.flush(&writer._lock.file, None, SyncPoint::Other)?;
            writer.flush(&writer.chunks.file, None, SyncPoint::Other)?;
            writer.flush(&writer.root.file, None, SyncPoint::Other)?;
            writer.flush(&parent.file, None, SyncPoint::Parent)?;
            Ok(writer)
        }
        pub(crate) fn open(root: &Path, expected: Identity) -> Result<Self> {
            let budget = Arc::new(Budget::default());
            let (root, parent) = root_directory(root, false, &budget)?;
            let parent = parent.expect("opened root has a pinned parent");
            let chunks = directory(root.as_raw_fd(), c"chunks", &budget)?;
            let lock = lock_at(&root, false, &budget)?;
            let (identity, identity_file) = identity_at(&root, &budget)?;
            if identity != expected {
                return Err(Error::IdentityMismatch);
            }
            let writer = Self { root, chunks, _lock: lock, identity, budget, poisoned: Cell::new(false) };
            // Visible valid identity bytes can precede completion of initialization.
            // Admit a writer only after the actual pinned inode and complete required
            // directory chain are durable under the acquired single-writer lock.
            writer.flush(&identity_file.file, None, SyncPoint::Other)?;
            writer.flush(&writer._lock.file, None, SyncPoint::Other)?;
            writer.flush(&writer.chunks.file, None, SyncPoint::Other)?;
            writer.flush(&writer.root.file, None, SyncPoint::Other)?;
            writer.flush(&parent.file, None, SyncPoint::Parent)?;
            Ok(writer)
        }
        pub(crate) fn identity(&self) -> Identity {
            self.identity
        }
        pub(crate) fn publish(&mut self, bytes: u32, input: &mut dyn Read, deadline: Instant) -> Result<PayloadRef> {
            if self.poisoned.get() {
                return Err(Error::Busy);
            }
            if bytes > MAX_CHUNK_BYTES {
                return Err(Error::Limit);
            }
            check(deadline)?;
            // Stream before selecting the bucket, so no caller-provided path participates.
            let (mut temp, mut file) = TempName::create(&self.chunks, &self.budget)?;
            let mut buffer = [0; MAX_RANGE_BYTES];
            let mut hash = Sha256::new();
            let mut remaining = bytes as usize;
            while remaining > 0 {
                let want = remaining.min(buffer.len());
                check(deadline)?;
                let result = input.read(&mut buffer[..want]);
                check(deadline)?;
                let count = match result {
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    other => other?,
                };
                if count == 0 || count > want {
                    return Err(Error::Corrupt);
                }
                hash.update(&buffer[..count]);
                let mut written = 0;
                while written < count {
                    check(deadline)?;
                    let result = file.file.write(&buffer[written..count]);
                    check(deadline)?;
                    let n = match result {
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                        other => other?,
                    };
                    if n == 0 {
                        return Err(Error::Io(io::Error::new(
                            io::ErrorKind::WriteZero,
                            "payload write made no progress",
                        )));
                    }
                    written += n;
                }
                remaining -= count;
            }
            loop {
                check(deadline)?;
                let extra = input.read(&mut buffer[..1]);
                check(deadline)?;
                match extra {
                    Ok(0) => break,
                    Ok(_) => return Err(Error::Corrupt),
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) => return Err(Error::Io(error)),
                }
            }
            let digest: [u8; 32] = hash.finalize().into();
            self.flush(&file.file, Some(deadline), SyncPoint::Other)?;
            drop(file); // No writable descriptor remains when the final name is installed.
            let (prefix, final_name) = object_names(&digest);
            check(deadline)?;
            mkdir_at(self.chunks.as_raw_fd(), &prefix, false)?;
            check(deadline)?;
            let bucket = directory(self.chunks.as_raw_fd(), &prefix, &self.budget)?;
            check(deadline)?;
            #[cfg(test)]
            crash_boundary("pre-install");
            install(&temp, &bucket, &final_name)?;
            check(deadline)?;
            #[cfg(test)]
            crash_boundary("post-install");
            let selected = open_at(bucket.as_raw_fd(), &final_name, libc::O_RDONLY, &self.budget)?;
            verify(&selected.file, bytes, &digest, deadline)?;
            // An existing final inode may come from a child killed before directory sync.
            // Always flush the actual selected inode, never substitute the new temporary.
            self.flush(&selected.file, Some(deadline), SyncPoint::Selected)?;
            drop(selected);
            temp.unlink()?;
            check(deadline)?;
            // The temporary lived in chunks/, while the final name is in its bucket.
            // Sync the entire required parent chain, even when mkdir/link found EEXIST.
            self.flush(&bucket.file, Some(deadline), SyncPoint::Other)?;
            self.flush(&self.chunks.file, Some(deadline), SyncPoint::Other)?;
            self.flush(&self.root.file, Some(deadline), SyncPoint::Other)?;
            #[cfg(test)]
            crash_boundary("post-directory-sync");
            check(deadline)?;
            PayloadRef::from_parts(self.identity, digest, bytes)
        }
    }
    impl Reader {
        pub(crate) fn open(root: &Path, expected: Identity) -> Result<Self> {
            let budget = Arc::new(Budget::default());
            let (root, _) = root_directory(root, false, &budget)?;
            let (identity, _identity_file) = identity_at(&root, &budget)?;
            if identity != expected {
                return Err(Error::IdentityMismatch);
            }
            let chunks = directory(root.as_raw_fd(), c"chunks", &budget)?;
            Ok(Self { root, chunks, identity, budget })
        }
        pub(crate) fn open_chunk(&self, reference: PayloadRef, deadline: Instant) -> Result<Chunk> {
            check(deadline)?;
            if reference.identity != self.identity {
                return Err(Error::IdentityMismatch);
            }
            if reference.bytes > MAX_CHUNK_BYTES {
                return Err(Error::Limit);
            }
            let (prefix, final_name) = object_names(&reference.digest);
            let bucket = directory(self.chunks.as_raw_fd(), &prefix, &self.budget)?;
            check(deadline)?;
            let file = open_at(bucket.as_raw_fd(), &final_name, libc::O_RDONLY, &self.budget)?;
            drop(bucket);
            let before = verify(&file.file, reference.bytes, &reference.digest, deadline)?;
            Ok(Chunk { file, before, bytes: reference.bytes })
        }
    }
    impl Chunk {
        pub(crate) fn read_range(&self, offset: u64, out: &mut [u8], deadline: Instant) -> Result<usize> {
            check(deadline)?;
            if out.len() > MAX_RANGE_BYTES
                || offset > self.bytes as u64
                || offset.checked_add(out.len() as u64).is_none()
            {
                return Err(Error::Limit);
            }
            if stable(&self.file.file)? != self.before {
                return Err(Error::Corrupt);
            }
            let want = out.len().min((self.bytes as u64 - offset) as usize);
            let mut total = 0;
            while total < want {
                let count = read_at(&self.file.file, &mut out[total..want], offset + total as u64, deadline)?;
                if count == 0 {
                    return Err(Error::Corrupt);
                }
                total += count;
            }
            if stable(&self.file.file)? != self.before {
                return Err(Error::Corrupt);
            }
            check(deadline)?;
            Ok(total)
        }
    }

    #[cfg(test)]
    thread_local! {
        static FAIL_SYNC: Cell<Option<SyncPoint>> = const { Cell::new(None) };
        static SYNC_CALLS: Cell<usize> = const { Cell::new(0) };
    }
    #[cfg(test)]
    fn sync_primitive(file: &File, point: SyncPoint) -> io::Result<()> {
        SYNC_CALLS.with(|calls| calls.set(calls.get() + 1));
        let fail = FAIL_SYNC.with(|failure| match failure.get() {
            Some(target) if target == SyncPoint::Other || target == point => {
                failure.set(None);
                true
            }
            _ => false,
        });
        if fail {
            return Err(io::Error::from_raw_os_error(libc::EIO));
        }
        crate::nxv_store::sync::full_flush(file)
    }
    #[cfg(test)]
    pub(crate) fn test_fail_next_sync() {
        FAIL_SYNC.with(|failure| failure.set(Some(SyncPoint::Other)));
    }
    #[cfg(test)]
    pub(crate) fn test_fail_final_sync() {
        FAIL_SYNC.with(|failure| failure.set(Some(SyncPoint::Selected)));
    }
    #[cfg(test)]
    pub(crate) fn test_fail_parent_sync() {
        FAIL_SYNC.with(|failure| failure.set(Some(SyncPoint::Parent)));
    }
    #[cfg(test)]
    pub(crate) fn test_clear_sync_failure() {
        FAIL_SYNC.with(|failure| failure.set(None));
    }
    #[cfg(test)]
    pub(crate) fn test_sync_calls() -> usize {
        SYNC_CALLS.with(Cell::get)
    }
    #[cfg(test)]
    fn crash_boundary(boundary: &str) {
        if std::env::var("ATLAS_PAYLOAD_TEST_MODE").as_deref() != Ok(boundary) {
            return;
        }
        let marker = std::env::var_os("ATLAS_PAYLOAD_TEST_MARKER").expect("child marker");
        std::fs::write(marker, boundary.as_bytes()).expect("signal actual publication boundary");
        loop {
            std::thread::park();
        }
    }
}
#[cfg(unix)]
pub(crate) use unix::{Chunk, Reader, Writer};
#[cfg(all(test, unix))]
pub(super) use unix::{
    test_clear_sync_failure, test_fail_final_sync, test_fail_next_sync, test_fail_parent_sync, test_sync_calls,
};

#[cfg(not(unix))]
mod unsupported {
    use super::super::{CatalogId, Error, Identity, PayloadRef, Result};
    use std::{io::Read, path::Path, time::Instant};
    pub(crate) struct Writer;
    pub(crate) struct Reader;
    pub(crate) struct Chunk;
    impl Writer {
        pub(crate) fn create(_: &Path, _: CatalogId) -> Result<Self> {
            Err(Error::Unsupported)
        }
        pub(crate) fn open(_: &Path, _: Identity) -> Result<Self> {
            Err(Error::Unsupported)
        }
        pub(crate) fn identity(&self) -> Identity {
            unreachable!("unsupported platform cannot construct a writer")
        }
        pub(crate) fn publish(&mut self, _: u32, _: &mut dyn Read, _: Instant) -> Result<PayloadRef> {
            Err(Error::Unsupported)
        }
    }
    impl Reader {
        pub(crate) fn open(_: &Path, _: Identity) -> Result<Self> {
            Err(Error::Unsupported)
        }
        pub(crate) fn open_chunk(&self, _: PayloadRef, _: Instant) -> Result<Chunk> {
            Err(Error::Unsupported)
        }
    }
    impl Chunk {
        pub(crate) fn read_range(&self, _: u64, _: &mut [u8], _: Instant) -> Result<usize> {
            Err(Error::Unsupported)
        }
    }
}
#[cfg(not(unix))]
pub(crate) use unsupported::{Chunk, Reader, Writer};
