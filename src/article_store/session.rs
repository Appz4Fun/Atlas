//! Inactive session boundary for future catalogue publication and lazy reads.
//!
//! Acquire a session before both PG snapshot creation and Owner admission. Their
//! relative order is the adapter's choice. Gate failure must skip both; nobody
//! may wait for this gate while holding Owner. Keep ReadSession through all lazy
//! children, WriteSession through future PG publication, and GcExclusive through
//! future GC. No GC is implemented.
//! The gate is persistent: restore requires all participants stopped/restarted;
//! copied gates across fresh same-owner processes are outside this trust model.
use super::payload::{GateAnchor, GateGuard};
use super::{Chunk, Identity, PayloadRef, Reader, Result, Writer};
use std::{path::Path, time::Instant};

pub(crate) struct SessionReader {
    reader: Reader,
    anchor: GateAnchor,
}
pub(crate) struct ReadSession<'a> {
    reader: &'a Reader,
    _gate: GateGuard,
    deadline: Instant,
}
pub(crate) struct WriteSession {
    _gate: GateGuard,
}
pub(crate) struct GcExclusive {
    _gate: GateGuard,
}
impl SessionReader {
    pub(crate) fn open(root: &Path, expected: Identity) -> Result<Self> {
        let reader = Reader::open(root, expected)?;
        let anchor = reader.pin_read_gate()?;
        Ok(Self { reader, anchor })
    }
    pub(crate) fn begin_read(&self, deadline: Instant) -> Result<ReadSession<'_>> {
        let gate = self.reader.acquire_read_gate(&self.anchor, false, deadline)?;
        Ok(ReadSession { reader: &self.reader, _gate: gate, deadline })
    }
    pub(crate) fn try_gc_exclusive(&self, deadline: Instant) -> Result<GcExclusive> {
        Ok(GcExclusive { _gate: self.reader.acquire_read_gate(&self.anchor, true, deadline)? })
    }
    #[cfg(all(test, unix))]
    pub(super) fn test_descriptor_counts(&self) -> (usize, usize) {
        self.reader.test_descriptor_counts()
    }
}
impl ReadSession<'_> {
    pub(super) fn deadline(&self) -> Instant {
        self.deadline
    }
    pub(super) fn open_chunk(&self, reference: PayloadRef, deadline: Instant) -> Result<Chunk> {
        self.reader.open_chunk(reference, deadline.min(self.deadline))
    }
}
impl Writer {
    /// Call initialize_read_gate on this writer before session admission. The
    /// returned guard owns its permit and flock independently of the Writer.
    pub(crate) fn begin_write(&self, deadline: Instant) -> Result<WriteSession> {
        Ok(WriteSession { _gate: self.acquire_write_gate(deadline)? })
    }
}
