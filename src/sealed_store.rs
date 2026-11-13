//! Historical SQLite sealed-log offload is retired from the native shipping graph.
//!
//! The original offline source is preserved at c62d0701acd07849014cd96161a0c99f2623212e.
//! Native history uses explicitly admitted streaming readers; the pure historical
//! blob codec never connects to a service or substitutes a storage backend.
