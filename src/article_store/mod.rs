//! Inactive immutable payload primitives. Publication conveys no catalogue visibility.

use std::{fmt, io};

mod payload;
pub(crate) use payload::{Chunk, Reader, Writer};

pub(crate) const MAX_CHUNK_BYTES: u32 = 1024 * 1024;
pub(crate) const MAX_RANGE_BYTES: usize = 64 * 1024;
pub(crate) const MAX_OWNED_FDS: usize = 8;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CatalogId([u8; 32]);
impl CatalogId {
    pub(crate) const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct StoreId([u8; 16]);
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Identity {
    pub(crate) catalog: CatalogId,
    pub(crate) store: StoreId,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PayloadRef {
    pub(crate) identity: Identity,
    pub(crate) digest: [u8; 32],
    pub(crate) bytes: u32,
}
#[derive(Debug)]
pub(crate) enum Error {
    IdentityMismatch,
    Corrupt,
    Missing,
    Limit,
    Deadline,
    Busy,
    Unsupported,
    Io(io::Error),
}
pub(crate) type Result<T> = std::result::Result<T, Error>;
impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => error.fmt(f),
            other => write!(f, "{other:?}"),
        }
    }
}
impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}
#[cfg(all(test, unix))]
mod payload_tests;
