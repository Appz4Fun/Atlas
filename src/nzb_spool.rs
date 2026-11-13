//! Private request spools with explicit disk and concurrency admission.
//!
//! A completed body keeps its permit until HTTP sending (or cancellation) drops
//! it. PostgreSQL snapshots can therefore end before a slow consumer starts.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub requests: usize,
    pub per_request_bytes: u64,
    pub total_bytes: u64,
    pub free_bytes: u64,
}

#[derive(Default, Debug)]
struct Usage {
    requests: usize,
    bytes: u64,
    disk_credit: u64,
}

#[derive(Debug)]
pub struct Pool {
    root: PathBuf,
    limits: Limits,
    usage: Mutex<Usage>,
}

impl Pool {
    pub fn new(root: PathBuf, limits: Limits) -> io::Result<Arc<Self>> {
        if limits.requests == 0 || limits.per_request_bytes == 0 || limits.total_bytes < limits.per_request_bytes {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid NZB spool admission"));
        }
        // The caller supplies a private application or test directory; no
        // release-controlled string participates in the temporary file path.
        let metadata = std::fs::symlink_metadata(&root)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "NZB spool root is not a directory"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o077 != 0 {
                return Err(io::Error::new(io::ErrorKind::PermissionDenied, "NZB spool root must be private"));
            }
        }
        Ok(Arc::new(Self { root, limits, usage: Mutex::new(Usage::default()) }))
    }

    pub fn open(self: &Arc<Self>) -> io::Result<Spool> {
        let mut usage = self.usage.lock().unwrap_or_else(|error| error.into_inner());
        if usage.requests >= self.limits.requests {
            return Err(io::Error::new(io::ErrorKind::WouldBlock, "NZB spool requests are busy"));
        }
        self.check_disk(usage.disk_credit)?;
        let file = tempfile::tempfile_in(&self.root)?;
        usage.requests += 1;
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Ok(Spool {
            id: NEXT.fetch_add(1, Ordering::Relaxed),
            pool: self.clone(),
            file,
            bytes: 0,
            disk_credit: 0,
            complete: false,
            failed: false,
            visible: 0,
            remaining: 0,
            visible_start: 0,
        })
    }

    fn check_disk(&self, bytes: u64) -> io::Result<()> {
        let required = self.limits.free_bytes.checked_add(bytes).ok_or_else(|| io::Error::other("NZB disk admission overflow"))?;
        if available_bytes(&self.root)? < required {
            return Err(io::Error::other("insufficient free space for NZB spool"));
        }
        Ok(())
    }
}

#[derive(Debug)]
pub struct Spool {
    id: u64,
    pool: Arc<Pool>,
    file: File,
    bytes: u64,
    disk_credit: u64,
    complete: bool,
    failed: bool,
    visible: u64,
    remaining: u64,
    visible_start: u64,
}

impl PartialEq for Spool {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}
impl Eq for Spool {}

impl Spool {
    pub fn len(&self) -> u64 {
        if self.complete { self.visible } else { self.bytes }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn is_complete(&self) -> bool {
        self.complete
    }

    pub fn finish(self) -> io::Result<Self> {
        let length = self.bytes;
        self.finish_range(0, length)
    }

    /// Expose a checked range without copying a giant trimmed filename. Disk
    /// quota continues to cover the whole physical file until it is dropped.
    pub fn finish_range(mut self, start: u64, end: u64) -> io::Result<Self> {
        if start > end || end > self.bytes || self.complete || self.failed {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid NZB spool range"));
        }
        self.file.flush()?;
        self.file.seek(SeekFrom::Start(start))?;
        self.pool.usage.lock().unwrap_or_else(|error| error.into_inner()).disk_credit -= self.disk_credit;
        self.disk_credit = 0;
        self.complete = true;
        self.visible = end - start;
        self.visible_start = start;
        self.remaining = self.visible;
        Ok(self)
    }

    /// Replace an already admitted provisional field without changing its
    /// physical extent or append position. Canonical file encoders use this
    /// for fixed-size row/ID lengths discovered by streaming conversion. Any
    /// I/O failure poisons the spool so a partially patched file cannot finish.
    pub(crate) fn patch(&mut self, offset: u64, bytes: &[u8]) -> io::Result<()> {
        let end = offset.checked_add(bytes.len() as u64).ok_or_else(|| io::Error::other("NZB patch extent overflow"))?;
        if self.complete || self.failed || end > self.bytes {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid provisional NZB patch"));
        }
        self.failed = true;
        self.file.seek(SeekFrom::Start(offset))?;
        self.file.write_all(bytes)?;
        self.file.seek(SeekFrom::Start(self.bytes))?;
        self.failed = false;
        Ok(())
    }

    /// Re-read an immutable checked range without another allocation/copy.
    /// Used for shared group metadata while building many physical files.
    pub(crate) fn rewind(&mut self) -> io::Result<()> {
        if !self.complete {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "NZB spool is not complete"));
        }
        self.file.seek(SeekFrom::Start(self.visible_start))?;
        self.remaining = self.visible;
        Ok(())
    }

    /// Publish a completed body through a private temporary file on the target
    /// filesystem. The extra copy is admitted in both aggregate bytes and
    /// destination free space before writing; failure never exposes a prefix.
    pub(crate) fn publish_atomic(
        mut self,
        target: &Path,
        mut check: impl FnMut() -> io::Result<()>,
    ) -> io::Result<()> {
        if !self.complete {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "NZB body is not complete"));
        }
        let parent = target.parent().filter(|path| !path.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
        check()?;
        std::fs::create_dir_all(parent)?;
        let mut reservation = OutputReservation::new(&self.pool, parent, self.visible)?;
        let mut output = tempfile::NamedTempFile::new_in(parent)?;
        self.rewind()?;
        {
            let mut admitted = AdmittedOutput { output: output.as_file_mut(), reservation: &mut reservation };
            let mut bytes = [0u8; 8192];
            loop {
                check()?;
                let got = self.read(&mut bytes)?;
                if got == 0 { break; }
                admitted.write_all(&bytes[..got])?;
            }
            admitted.flush()?;
        }
        output.as_file().sync_all()?;
        check()?;
        output.persist(target).map_err(|error| error.error)?;
        Ok(())
    }
}

struct OutputReservation {
    pool: Arc<Pool>,
    bytes: u64,
    credit: u64,
}
impl OutputReservation {
    fn new(pool: &Arc<Pool>, parent: &Path, bytes: u64) -> io::Result<Self> {
        let mut usage = pool.usage.lock().unwrap_or_else(|error| error.into_inner());
        let total = usage.bytes.checked_add(bytes).ok_or_else(|| io::Error::other("NZB output quota overflow"))?;
        if total > pool.limits.total_bytes {
            return Err(io::Error::other("NZB atomic output exceeds aggregate spool admission"));
        }
        let credit = usage.disk_credit.checked_add(bytes).ok_or_else(|| io::Error::other("NZB output disk reservation overflow"))?;
        let floor = pool.limits.free_bytes.checked_add(credit).ok_or_else(|| io::Error::other("NZB output disk floor overflow"))?;
        if available_bytes(parent)? < floor {
            return Err(io::Error::other("insufficient destination free space for atomic NZB output"));
        }
        usage.bytes = total;
        usage.disk_credit = credit;
        Ok(Self { pool: pool.clone(), bytes, credit: bytes })
    }
    fn written(&mut self, bytes: usize) {
        let bytes = bytes as u64;
        self.pool.usage.lock().unwrap_or_else(|error| error.into_inner()).disk_credit -= bytes;
        self.credit -= bytes;
    }
}
impl Drop for OutputReservation {
    fn drop(&mut self) {
        let mut usage = self.pool.usage.lock().unwrap_or_else(|error| error.into_inner());
        usage.bytes -= self.bytes;
        usage.disk_credit -= self.credit;
    }
}

struct AdmittedOutput<'a> {
    output: &'a mut File,
    reservation: &'a mut OutputReservation,
}
impl Write for AdmittedOutput<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let written = self.output.write(bytes)?;
        self.reservation.written(written);
        Ok(written)
    }
    fn flush(&mut self) -> io::Result<()> { self.output.flush() }
}

impl Write for Spool {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if self.complete || self.failed {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "NZB spool is already complete"));
        }
        let amount = u64::try_from(buffer.len()).map_err(|_| io::Error::other("NZB spool write length"))?;
        let next = self.bytes.checked_add(amount).ok_or_else(|| io::Error::other("NZB spool size overflow"))?;
        if next > self.pool.limits.per_request_bytes {
            return Err(io::Error::other("NZB request exceeds spool admission"));
        }
        let mut usage = self.pool.usage.lock().unwrap_or_else(|error| error.into_inner());
        let total = usage.bytes.checked_add(amount).ok_or_else(|| io::Error::other("NZB spool total overflow"))?;
        if total > self.pool.limits.total_bytes {
            return Err(io::Error::other("NZB requests exceed total spool admission"));
        }
        if amount > self.disk_credit {
            let remaining = self.pool.limits.per_request_bytes - self.bytes;
            let target = amount.max(remaining.min(1024 * 1024));
            let added = target - self.disk_credit;
            let all_credit = usage.disk_credit.checked_add(added).ok_or_else(|| io::Error::other("NZB disk reservation overflow"))?;
            // Admit ALL writers' unwritten credits together. Independent free
            // checks could otherwise reserve the same remaining disk space.
            self.pool.check_disk(all_credit)?;
            usage.disk_credit = all_credit;
            self.disk_credit = target;
        }
        // Reserve before writing. Other writers cannot admit the same space.
        usage.bytes = total;
        drop(usage);
        match self.file.write(buffer) {
            Ok(written) => {
                let written = written as u64;
                let mut usage = self.pool.usage.lock().unwrap_or_else(|error| error.into_inner());
                usage.bytes -= amount - written;
                usage.disk_credit -= written;
                self.bytes += written;
                self.disk_credit -= written;
                Ok(written as usize)
            }
            Err(error) => {
                self.pool.usage.lock().unwrap_or_else(|error| error.into_inner()).bytes -= amount;
                Err(error)
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

impl Read for Spool {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if !self.complete {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "NZB spool is not complete"));
        }
        let limit = buffer.len().min(usize::try_from(self.remaining).unwrap_or(usize::MAX));
        if limit == 0 { return Ok(0) }
        let read = self.file.read(&mut buffer[..limit])?;
        if read == 0 { return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "completed NZB spool was truncated")); }
        self.remaining -= read as u64;
        Ok(read)
    }
}

impl Drop for Spool {
    fn drop(&mut self) {
        let mut usage = self.pool.usage.lock().unwrap_or_else(|error| error.into_inner());
        usage.requests -= 1;
        usage.bytes -= self.bytes;
        usage.disk_credit -= self.disk_credit;
        // The anonymous temporary file is removed by the operating system on
        // close, including incomplete writes and disconnected response bodies.
    }
}

#[cfg(unix)]
fn available_bytes(path: &Path) -> io::Result<u64> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL in spool directory"))?;
    let mut result = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: path is NUL terminated and result is writable for statvfs.
    if unsafe { libc::statvfs(path.as_ptr(), result.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a successful statvfs initialized result.
    let result = unsafe { result.assume_init() };
    #[allow(clippy::unnecessary_cast)]
    (result.f_bavail as u64).checked_mul(result.f_frsize as u64).ok_or_else(|| io::Error::other("free-space count overflow"))
}

#[cfg(not(unix))]
fn available_bytes(path: &Path) -> io::Result<u64> {
    let disks = sysinfo::Disks::new_with_refreshed_list();
    let canonical = path.canonicalize()?;
    disks.list().iter().filter(|disk| canonical.starts_with(disk.mount_point()))
        .max_by_key(|disk| disk.mount_point().as_os_str().len())
        .map(|disk| disk.available_space())
        .ok_or_else(|| io::Error::other("cannot determine NZB spool free space"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pool(root: &Path, requests: usize, bytes: u64) -> Arc<Pool> {
        Pool::new(root.to_owned(), Limits { requests, per_request_bytes: bytes, total_bytes: bytes, free_bytes: 0 }).unwrap()
    }

    // Removing the lifetime permit must fail while a completed body is retained.
    #[test]
    fn body_retains_admission_until_cancelled_or_consumed_and_dropped() {
        let root = tempfile::tempdir().unwrap();
        let pool = pool(root.path(), 1, 16);
        let mut writer = pool.open().unwrap();
        writer.write_all(b"checked XML").unwrap();
        let mut body = writer.finish().unwrap();
        assert_eq!(pool.open().unwrap_err().kind(), io::ErrorKind::WouldBlock);
        let mut content = String::new();
        body.read_to_string(&mut content).unwrap();
        assert_eq!(content, "checked XML");
        assert!(pool.open().is_err(), "EOF does not relinquish the body permit early");
        drop(body);
        assert!(pool.open().is_ok());
        assert!(std::fs::read_dir(root.path()).unwrap().next().is_none());
    }

    #[test]
    fn canonical_header_patches_preserve_admission_and_append_position() {
        let root = tempfile::tempdir().unwrap();
        let pool = pool(root.path(), 1, 12);
        let mut writer = pool.open().unwrap();
        writer.write_all(b"0000body").unwrap();
        writer.patch(0, b"0008").unwrap();
        assert_eq!(pool.usage.lock().unwrap().bytes, 8);
        assert!(writer.patch(7, b"past end").is_err());
        writer.write_all(b"tail").unwrap();
        assert!(writer.write_all(b"x").is_err());
        let mut checked = writer.finish().unwrap();
        assert!(checked.patch(0, b"0000").is_err());
        let mut bytes = Vec::new();
        checked.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"0008bodytail");
        assert_eq!(pool.usage.lock().unwrap().bytes, 12);
        drop(checked);
        assert_eq!(pool.usage.lock().unwrap().bytes, 0);
    }

    #[test]
    fn partial_writes_cannot_be_published_or_outlive_their_quota() {
        let root = tempfile::tempdir().unwrap();
        let pool = pool(root.path(), 2, 5);
        let mut first = pool.open().unwrap();
        let mut second = pool.open().unwrap();
        first.write_all(b"1234").unwrap();
        assert!(first.write_all(b"xx").is_err());
        assert!(second.write_all(b"xx").is_err());
        assert!(first.read(&mut [0u8; 1]).is_err());
        drop(first);
        second.write_all(b"12345").unwrap();
        assert_eq!(second.finish().unwrap().len(), 5);
        assert_eq!(pool.usage.lock().unwrap().bytes, 0);
    }

    #[test]
    fn disk_headroom_is_checked_before_an_output_is_created() {
        let root = tempfile::tempdir().unwrap();
        let pool = Pool::new(root.path().to_owned(), Limits { requests: 1, per_request_bytes: 5, total_bytes: 5, free_bytes: u64::MAX }).unwrap();
        assert!(pool.open().is_err());
        assert!(std::fs::read_dir(root.path()).unwrap().next().is_none());
    }

    #[test]
    fn a_trimmed_view_keeps_full_disk_quota_and_has_an_exact_eof() {
        let root = tempfile::tempdir().unwrap();
        let pool = pool(root.path(), 2, 16);
        let mut writer = pool.open().unwrap();
        writer.write_all(b" ..name.. ").unwrap();
        let mut body = writer.finish_range(3, 7).unwrap();
        assert_eq!(body.len(), 4);
        assert_eq!(pool.usage.lock().unwrap().bytes, 10);
        let mut name = String::new();
        body.read_to_string(&mut name).unwrap();
        assert_eq!(name, "name");
        assert_eq!(body.read(&mut [0u8; 1]).unwrap(), 0);
        body.rewind().unwrap();
        let mut repeated = String::new();
        body.read_to_string(&mut repeated).unwrap();
        assert_eq!(repeated, "name");
        drop(body);
        assert_eq!(pool.usage.lock().unwrap().bytes, 0);
    }

    #[test]
    fn disk_reservations_cover_all_writers_and_end_before_body_sending() {
        let root = tempfile::tempdir().unwrap();
        let pool = Pool::new(root.path().to_owned(), Limits { requests: 2, per_request_bytes: 8, total_bytes: 16, free_bytes: 0 }).unwrap();
        let mut first = pool.open().unwrap();
        let mut second = pool.open().unwrap();
        first.write_all(b"12").unwrap();
        second.write_all(b"34").unwrap();
        assert_eq!(pool.usage.lock().unwrap().disk_credit, 12, "both writers' possible remaining allocations are admitted");
        let body = first.finish().unwrap();
        assert_eq!(pool.usage.lock().unwrap().disk_credit, 6, "a completed immutable body needs no unwritten allocation");
        drop(second);
        assert_eq!(pool.usage.lock().unwrap().disk_credit, 0);
        assert_eq!(pool.usage.lock().unwrap().bytes, 2, "the slow consumer still owns its completed disk bytes");
        drop(body);
        assert_eq!(pool.usage.lock().unwrap().bytes, 0);
    }

    #[test]
    fn atomic_destination_is_complete_and_temporary_copy_is_admitted() {
        let root = tempfile::tempdir().unwrap();
        let pool = Pool::new(root.path().to_owned(), Limits {
            requests: 2, per_request_bytes: 32, total_bytes: 64, free_bytes: 0,
        }).unwrap();
        let mut body = pool.open().unwrap();
        body.write_all(b"<complete/>").unwrap();
        let target = root.path().join("finished.nzb");
        std::fs::write(&target, b"old").unwrap();
        body.finish().unwrap().publish_atomic(&target, || Ok(())).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"<complete/>");
        let usage = pool.usage.lock().unwrap();
        assert_eq!((usage.requests, usage.bytes, usage.disk_credit), (0, 0, 0));
        drop(usage);
        let tight = Pool::new(root.path().to_owned(), Limits {
            requests: 1, per_request_bytes: 32, total_bytes: 32, free_bytes: 0,
        }).unwrap();
        let mut body = tight.open().unwrap();
        body.write_all(&[b'x'; 24]).unwrap();
        assert!(body.finish().unwrap().publish_atomic(&target, || Ok(())).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"<complete/>", "quota failure cannot replace the previous destination");
    }

    #[test]
    fn cancellation_during_atomic_copy_preserves_destination_and_releases_quota() {
        let root = tempfile::tempdir().unwrap();
        let pool = Pool::new(root.path().to_owned(), Limits {
            requests: 1, per_request_bytes: 128 * 1024, total_bytes: 256 * 1024, free_bytes: 0,
        }).unwrap();
        let mut body = pool.open().unwrap();
        io::copy(&mut io::repeat(b'x').take(128 * 1024), &mut body).unwrap();
        let target = root.path().join("finished.nzb");
        std::fs::write(&target, b"old").unwrap();
        let mut checks = 0;
        assert!(body.finish().unwrap().publish_atomic(&target, || {
            checks += 1;
            if checks >= 4 { Err(io::Error::other("synthetic cancellation")) } else { Ok(()) }
        }).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"old");
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1, "partial destination temporary file must be removed");
        let usage = pool.usage.lock().unwrap();
        assert_eq!((usage.requests, usage.bytes, usage.disk_credit), (0, 0, 0));
    }

    #[cfg(unix)]
    #[test]
    fn shared_or_symlinked_roots_are_refused() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let root = tempfile::tempdir().unwrap();
        let shared = root.path().join("shared");
        std::fs::create_dir(&shared).unwrap();
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o755)).unwrap();
        let limits = Limits { requests: 1, per_request_bytes: 5, total_bytes: 5, free_bytes: 0 };
        assert!(Pool::new(shared.clone(), limits).is_err());
        let link = root.path().join("link");
        symlink(&shared, &link).unwrap();
        assert!(Pool::new(link, limits).is_err());
    }
}

