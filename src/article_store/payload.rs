use super::{CatalogId, Error, Identity, PayloadRef, Result};
use std::{io::Read, path::Path, time::Instant};

pub(crate) struct Writer;
pub(crate) struct Reader;
pub(crate) struct Chunk;

impl Writer {
    pub(crate) fn create(_root: &Path, _catalog: CatalogId) -> Result<Self> {
        Err(Error::Unsupported)
    }
    pub(crate) fn open(_root: &Path, _expected: Identity) -> Result<Self> {
        Err(Error::Unsupported)
    }
    pub(crate) fn identity(&self) -> Identity {
        unreachable!("unimplemented payload store")
    }
    pub(crate) fn publish(&mut self, _bytes: u32, _input: &mut dyn Read, _deadline: Instant) -> Result<PayloadRef> {
        Err(Error::Unsupported)
    }
}
impl Reader {
    pub(crate) fn open(_root: &Path, _expected: Identity) -> Result<Self> {
        Err(Error::Unsupported)
    }
    pub(crate) fn open_chunk(&self, _reference: PayloadRef, _deadline: Instant) -> Result<Chunk> {
        Err(Error::Unsupported)
    }
}
impl Chunk {
    pub(crate) fn read_range(&self, _offset: u64, _out: &mut [u8], _deadline: Instant) -> Result<usize> {
        Err(Error::Unsupported)
    }
}
#[cfg(test)]
pub(super) fn test_fail_next_sync() {}
#[cfg(test)]
pub(super) fn test_sync_calls() -> usize {
    0
}
