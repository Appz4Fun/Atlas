//! Decoding single keyframes, for stills of a release: Atlas's own, intra only. What's decoded comes from Usenet,
//! so every read is bounds checked and a bad stream is an error, never a panic.

pub mod bits;
pub mod h264;
pub mod hevc;
pub mod jpeg;
pub mod mkv;
pub mod picture;
