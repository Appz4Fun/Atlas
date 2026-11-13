//! Storage-independent sidecar rules retained by the native catalog.
//!
//! The historical SQLite freeze/thaw engine and its offline payload sources
//! remain recoverable at revision c62d0701acd07849014cd96161a0c99f2623212e.
//! Native indexing does not attach, create, move, thaw or remove frozen files.

pub mod rules;
