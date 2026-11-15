//! Inactive immutable payload primitives. Publication conveys no catalogue visibility.

// This module is deliberately unconnected to runtime callers.
#![allow(dead_code)]

use std::{fmt, io};

mod manifest;
mod payload;
#[allow(unused_imports)]
pub(crate) use manifest::*;
#[allow(unused_imports)] // The inactive primitive still exposes its planned return type.
pub(crate) use payload::Chunk;
pub(crate) use payload::{Reader, Writer};

pub(crate) const MAX_CHUNK_BYTES: u32 = 1024 * 1024;
pub(crate) const MAX_RANGE_BYTES: usize = 64 * 1024;
pub(crate) const MAX_OWNED_FDS: usize = 8;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CatalogId([u8; 32]);
impl CatalogId {
    pub(crate) const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
    pub(crate) const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct StoreId([u8; 16]);
impl StoreId {
    /// An untrusted identity label; only explicit store creation generates randomness.
    pub(crate) const fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }
    pub(crate) const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Identity {
    pub(crate) catalog: CatalogId,
    pub(crate) store: StoreId,
}
impl Identity {
    pub(crate) const fn from_parts(catalog: CatalogId, store: StoreId) -> Self {
        Self { catalog, store }
    }
    pub(crate) fn to_bytes(self) -> [u8; 48] {
        let mut bytes = [0; 48];
        bytes[..16].copy_from_slice(self.store.as_bytes());
        bytes[16..].copy_from_slice(self.catalog.as_bytes());
        bytes
    }
    pub(crate) fn from_bytes(bytes: [u8; 48]) -> Self {
        let mut store = [0; 16];
        let mut catalog = [0; 32];
        store.copy_from_slice(&bytes[..16]);
        catalog.copy_from_slice(&bytes[16..]);
        Self::from_parts(CatalogId::from_bytes(catalog), StoreId::from_bytes(store))
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PayloadRef {
    pub(crate) identity: Identity,
    pub(crate) digest: [u8; 32],
    pub(crate) bytes: u32,
}
impl PayloadRef {
    /// Decoding a reference conveys no verification or catalogue acceptance authority.
    pub(crate) fn from_parts(identity: Identity, digest: [u8; 32], bytes: u32) -> Result<Self> {
        if bytes > MAX_CHUNK_BYTES {
            return Err(Error::Limit);
        }
        Ok(Self { identity, digest, bytes })
    }
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

#[cfg(all(test, unix))]
mod manifest_tests;

#[cfg(all(test, unix))]
mod test_support;
