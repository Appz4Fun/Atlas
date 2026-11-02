//! Decoders of our own for what the probe meets inside archives, each from the front of its stream to a limit.

pub mod bcj;
pub mod bzip2;
pub mod inflate;
pub mod lz4;
pub mod lzw;
mod ppmd_h;
pub mod rar15;
pub mod rar20;
pub mod rar3;
pub mod rar5;
mod rarbits;
