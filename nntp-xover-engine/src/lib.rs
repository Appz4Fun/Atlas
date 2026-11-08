//! `nntp-xover-engine`: columnar, compressed, mmap-able storage for NNTP
//! overview lines (RFC 3977 `OVER`/`XOVER`), with O(1)-bounded row access by
//! article number and by message-id, and an allocation-free streaming path.
//!
//! A block is built from raw overview lines with
//! [`NxvBlockBuilder::build_hot_block`], read in place with
//! [`NxvBlockView::from_mmap_slice`], and optionally turned cold
//! ([`compress_block_to_cold`]) with its routing data left uncompressed.

pub mod container;
pub mod corpus;
pub mod date;
pub mod msgid_codec;
pub mod msgid_index;
pub mod streamer;
pub mod subject_codec;
pub mod varint;
pub mod xref_codec;

pub use container::{
    AlignedBytes, NxvBlockBuilder, NxvBlockHeader, NxvBlockView, SectionEntry, SectionId, SkipCheckpoint,
    compress_block_to_cold, decompress_cold_block, peek_header,
};
pub use corpus::generate_realistic_xover_corpus;
pub use msgid_index::{Xor8Builder, Xor8Header, msgid_hash};
pub use streamer::{NxvRangeCursor, RowCache, XoverStreamScratch, stream_xover_blocks_tokio, stream_xover_range_tokio};
