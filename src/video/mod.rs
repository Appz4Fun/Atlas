//! Decoding single keyframes, for stills of a release: Atlas's own, intra only. What's decoded comes from Usenet,
//! so every read is bounds checked and a bad stream is an error, never a panic.

pub mod bits;

/// The most luma samples a picture may have: 4096x2304, room for any 2160p video. Stills never need more, and a
/// damaged or hostile header asking for more is refused before anything is allocated.
pub const MAX_LUMA_SAMPLES: u64 = 4096 * 2304;
/// The most NAL units one frame may hold. Real ones have a few dozen slices at most.
pub const MAX_UNITS: usize = 4096;
pub mod hevc;
pub mod jpeg;
pub mod mkv;
pub mod picture;
