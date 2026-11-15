//! Inactive session boundary for future catalogue publication and lazy reads.
//!
//! Acquire a session before opening a PG snapshot and before Owner admission.
//! Gate failure must skip both callbacks; nobody may wait for this gate while
//! holding Owner. Keep ReadSession through all lazy children, WriteSession through
//! future PG publication, and GcExclusive through future GC. No GC is implemented.
//! The gate is persistent: restore requires all participants stopped/restarted;
//! copied gates across fresh same-owner processes are outside this trust model.
use super::{Chunk, Identity, PayloadRef, Reader, Result, Writer};
use std::{path::Path, time::Instant};

pub(crate) struct SessionReader {
    reader: Reader,
}
pub(crate) struct ReadSession<'a> {
    reader: &'a Reader,
}
pub(crate) struct WriteSession;
pub(crate) struct GcExclusive;

// Tests-first checkpoint: these guards deliberately do not lock or initialize a
// gate. Root qualifies the semantic RED before the filesystem implementation.
impl SessionReader {
    pub(crate) fn open(root: &Path, expected: Identity) -> Result<Self> {
        Ok(Self { reader: Reader::open(root, expected)? })
    }
    pub(crate) fn begin_read(&self, _deadline: Instant) -> Result<ReadSession<'_>> {
        Ok(ReadSession { reader: &self.reader })
    }
    pub(crate) fn try_gc_exclusive(&self, _deadline: Instant) -> Result<GcExclusive> {
        Ok(GcExclusive)
    }
    #[cfg(all(test, unix))]
    pub(super) fn test_descriptor_counts(&self) -> (usize, usize) {
        self.reader.test_descriptor_counts()
    }
}
impl ReadSession<'_> {
    pub(super) fn open_chunk(&self, reference: PayloadRef, deadline: Instant) -> Result<Chunk> {
        self.reader.open_chunk(reference, deadline)
    }
}
impl Writer {
    pub(crate) fn initialize_read_gate(&mut self) -> Result<()> {
        Ok(())
    }
    pub(crate) fn begin_write(&self, _deadline: Instant) -> Result<WriteSession> {
        Ok(WriteSession)
    }
}
