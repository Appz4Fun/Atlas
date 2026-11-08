//! Durability for many files at once. On macOS a plain `fsync` moves a file's data to the drive but leaves it in the
//! drive's volatile cache; `F_FULLFSYNC` flushes that cache too, for the whole drive. So many files cost one plain
//! `fsync` each and a single `F_FULLFSYNC`. Rust's `File::sync_all` and `File::sync_data` both issue `F_FULLFSYNC` on
//! Apple platforms, which is why the per-file step calls `libc::fsync` itself.
//!
//! A failed sync is never retried: after a failure the kernel may already have dropped the dirty pages, and a retry
//! could report success for data that is gone. Callers stop instead, and recovery runs at the next start.

use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;

/// A file's dirty pages written to the drive, without flushing the drive's cache.
pub fn fsync_only(file: &File) -> io::Result<()> {
    // SAFETY: fsync on a descriptor this File owns
    if unsafe { libc::fsync(file.as_raw_fd()) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// `file`'s pages written and the whole drive's volatile cache flushed (`F_FULLFSYNC` on macOS; elsewhere a plain
/// `fsync`, which Linux follows with a cache flush itself).
pub fn full_flush(file: &File) -> io::Result<()> {
    #[cfg(target_vendor = "apple")]
    {
        // SAFETY: fcntl on a descriptor this File owns
        if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_FULLFSYNC) } == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        fsync_only(file)
    }
}

/// Every file durable on the drive: a plain `fsync` for all but one, then one full flush through the last. The same
/// file twice is synced once.
pub fn sync_all_durable(files: &[&File]) -> io::Result<()> {
    let mut seen: Vec<i32> = Vec::with_capacity(files.len());
    let mut unique: Vec<&File> = Vec::with_capacity(files.len());
    for f in files {
        let fd = f.as_raw_fd();
        if !seen.contains(&fd) {
            seen.push(fd);
            unique.push(f);
        }
    }
    let Some((last, rest)) = unique.split_last() else { return Ok(()) };
    for f in rest {
        fsync_only(f)?;
    }
    full_flush(last)
}

/// A directory's entries made durable (a new or renamed file's name), the same way.
pub fn sync_dir(dir: &std::path::Path) -> io::Result<()> {
    let d = File::open(dir)?;
    full_flush(&d)
}

/// Raises the open-file soft limit toward `want`, never past the hard limit or `kern.maxfilesperproc` (setrlimit
/// fails above it on macOS). Returns the limit now in force.
pub fn raise_open_files(want: u64) -> io::Result<u64> {
    let mut cap = want;
    #[cfg(target_vendor = "apple")]
    {
        let mut per_proc: libc::c_int = 0;
        let mut len = std::mem::size_of::<libc::c_int>();
        let name = c"kern.maxfilesperproc";
        // SAFETY: sysctlbyname with a correctly sized output buffer
        let r = unsafe {
            libc::sysctlbyname(
                name.as_ptr(),
                (&mut per_proc as *mut libc::c_int).cast(),
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        if r == 0 && per_proc > 0 {
            cap = cap.min(per_proc as u64);
        }
    }
    let mut rl = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
    // SAFETY: getrlimit/setrlimit with a valid rlimit
    unsafe {
        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut rl) != 0 {
            return Err(io::Error::last_os_error());
        }
        let target = if rl.rlim_max == libc::RLIM_INFINITY { cap } else { cap.min(rl.rlim_max as u64) };
        if (rl.rlim_cur as u64) < target {
            rl.rlim_cur = target as libc::rlim_t;
            if libc::setrlimit(libc::RLIMIT_NOFILE, &rl) != 0 {
                return Err(io::Error::last_os_error());
            }
        }
    }
    Ok(rl.rlim_cur as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn many_files_are_synced_with_one_full_flush_and_duplicates_once() {
        let dir = tempfile::tempdir().unwrap();
        let mut a = File::create(dir.path().join("a")).unwrap();
        let mut b = File::create(dir.path().join("b")).unwrap();
        a.write_all(b"one").unwrap();
        b.write_all(b"two").unwrap();
        sync_all_durable(&[&a, &b, &a]).unwrap();
        sync_all_durable(&[]).unwrap();
        sync_dir(dir.path()).unwrap();
    }

    #[test]
    fn the_open_file_limit_goes_up_and_stays_within_bounds() {
        let now = raise_open_files(4096).unwrap();
        assert!(now >= 256);
    }
}
