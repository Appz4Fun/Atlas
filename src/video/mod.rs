//! Decoding single keyframes, for stills of a release: Atlas's own, intra only. What's decoded comes from Usenet,
//! soo every read is bounds checked and a bad stream is an error, never a panic.

pub mod bits;
pub mod hevc;
pub mod picture;
