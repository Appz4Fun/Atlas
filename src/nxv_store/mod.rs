//! The day-segment header store: every overview line Atlas fetches, from every server, kept as immutable `.nxv` blocks
//! in one file per group per UTC day, with no database. Rows wait in per-slab logs (durable within about 100 ms, soo
//! read cursors can move) until a slab is sealed into a block, which a single committer appends with one full drive
//! flush per batch. Days leave as whole files once nothing keeps them.
//!
//! The design is written up in the architecture plan ("Atlas header store without SQLite").

pub mod live;
pub mod segment;
pub mod slab;
pub mod store;
pub mod sync;
pub mod tools;

pub use store::{Clamp, KeepSet, Store, StoreConfig, Ticket};
