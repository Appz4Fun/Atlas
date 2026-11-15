//! Startup-only admission for local recovery reads and mapped checksum chunks.
use std::fs::File;
use std::io::{self, Read};
use std::os::unix::fs::FileExt;
use std::time::{Duration, Instant};

pub(super) const READ_CHUNK: usize = 1024 * 1024;
const RATE: u128 = 64 * 1024 * 1024;

#[derive(Clone, Copy)]
pub(super) enum ReadKind {
    Index,
    Header,
    Metadata,
    Body,
    Log,
    Servers,
}

/// Lives only on the stack during one exclusive Store::open.
pub(super) struct RecoveryReadPacer {
    finish: Instant,
}
impl RecoveryReadPacer {
    pub(super) fn new() -> Self {
        Self { finish: now() }
    }

    fn admit(&mut self, bytes: usize) {
        debug_assert!(bytes <= READ_CHUNK);
        let cost = Duration::from_nanos((bytes as u128 * 1_000_000_000).div_ceil(RATE) as u64);
        let start = now().max(self.finish);
        if let Some(finish) = start.checked_add(cost) {
            self.finish = finish;
            wait_until(finish);
        } else {
            // Preserve the full wait even at an unsupported clock boundary.
            wait_until(start);
            let start = now();
            loop {
                let remaining = cost.saturating_sub(now().saturating_duration_since(start));
                if remaining.is_zero() {
                    break;
                }
                sleep(remaining);
            }
            self.finish = now();
        }
    }
}

fn now() -> Instant {
    #[cfg(test)]
    if let Some(now) = super::store::recovery_pacing_tests::clock_now() {
        return now;
    }
    Instant::now()
}
fn sleep(wait: Duration) {
    #[cfg(test)]
    if super::store::recovery_pacing_tests::clock_sleep(wait) {
        return;
    }
    std::thread::sleep(wait);
}
fn wait_until(finish: Instant) {
    loop {
        let remaining = finish.saturating_duration_since(now());
        if remaining.is_zero() {
            break;
        }
        sleep(remaining);
    }
}

pub(super) fn admit(pacer: Option<&mut RecoveryReadPacer>, kind: ReadKind, bytes: usize) {
    if let Some(pacer) = pacer {
        pacer.admit(bytes);
    }
    #[cfg(test)]
    {
        use super::store::recovery_pacing_tests::{ReadKind as Observed, observe};
        let kind = match kind {
            ReadKind::Index => Observed::Index,
            ReadKind::Header => Observed::Header,
            ReadKind::Metadata => Observed::Metadata,
            ReadKind::Body => Observed::Body,
            ReadKind::Log => Observed::Log,
            ReadKind::Servers => Observed::Servers,
        };
        observe(kind, bytes);
    }
    #[cfg(not(test))]
    let _ = kind;
}

pub(super) fn metadata_allowance(len: usize) -> usize {
    // SAFETY: sysconf takes a valid constant and no borrowed pointers.
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    let page =
        usize::try_from(page).ok().filter(|p| p.is_power_of_two() && (4096..=65536).contains(p)).unwrap_or(65536);
    len.min(8 * page)
}

pub(super) fn read_exact_at(
    file: &File,
    bytes: &mut [u8],
    offset: u64,
    mut pacer: Option<&mut RecoveryReadPacer>,
    kind: ReadKind,
) -> io::Result<()> {
    for (i, chunk) in bytes.chunks_mut(READ_CHUNK).enumerate() {
        admit(pacer.as_deref_mut(), kind, chunk.len());
        file.read_exact_at(chunk, offset + (i * READ_CHUNK) as u64)?;
    }
    Ok(())
}

/// Preserve Read's EOF, error, UTF-8 and Take behavior while bounding each read.
pub(super) struct RecoveryReader<'a, R> {
    pub(super) inner: R,
    pub(super) pacer: Option<&'a mut RecoveryReadPacer>,
    pub(super) kind: ReadKind,
}
impl<R: Read> Read for RecoveryReader<'_, R> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let len = bytes.len().min(READ_CHUNK);
        if len > 0 {
            admit(self.pacer.as_deref_mut(), self.kind, len);
        }
        self.inner.read(&mut bytes[..len])
    }
}
