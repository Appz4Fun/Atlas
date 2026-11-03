# W05-B: pure-Rust decoder test (one keyframe for thumbnail + perceptual hash)

Scope: one-frame IDR streams, testsrc2, encoded by ffmpeg (x265 / x264, default settings). Reference = ffmpeg `yuv420p` Y plane (raw code values). Run on the research container (release build, 1 thread per decoder, 10 timed runs each, min/median).

## Build
- rust_h265 0.4.0, rusty_h265 0.6.2, rust_h264 0.6.0: all fetched and built clean with cargo 1.97 (release, about 40 s cold including image_hasher 3 + image 0.25). No warnings that matter. No panics.
- Licences: rust_h265 MIT/Apache-2.0; rusty_h265 Apache-2.0 (no deps, `forbid(unsafe_code)`); rust_h264 MIT/Apache-2.0.

## API (entry points used)
- rust_h265: `Decoder::new().with_display_order()`, `parse_annex_b(&[u8]) -> Vec<NalUnit>`, `decode_nal(&nal)`, `next_output()`, `flush()`. Frame has `y/u/v: PixelData` (`U8` or `U16`), `width`, `height`, `bit_depth`. No stride. Also `parse_hvcc` for MP4/MKV.
- rusty_h265: `Decoder::new()`, `push_annexb(&[u8], None)`, `flush()`, `next_frame() -> Result<Frame>`. `frame.picture.planes[0]` is `Plane{width,height,stride,data:Vec<u16>}`, `picture.crop` gives the crop origin. Always u16 storage.
- rust_h264: `decoder::OrderedDecoder::<()>::new()`, `nal::parse_annex_b`, `decode_nal(&nal) -> Result<Vec<Frame>>`, `flush() -> Vec<Frame>`. Frame: `y/u/v: Vec<u8>`, `width/height: u32`, `bit_depth` (10-bit = 2 bytes LE).

## Results (first output frame, luma only)
| crate | stream | size | PSNR vs ffmpeg | ms/frame min / median | peak RSS (VmHWM) |
|---|---|---|---|---|---|
| rust_h265 0.4.0 | HEVC 8-bit Main | 1280x720 | inf (bit-exact) | 10.6 / 12.3 | 15.0 MB |
| rust_h265 0.4.0 | HEVC 10-bit Main10 | 1280x720 | inf (bit-exact) | 10.5 / 11.6 | 20.2 MB |
| rusty_h265 0.6.2 | HEVC 8-bit | 1280x720 | inf | 9.1 / 10.3 | 15.7 MB |
| rusty_h265 0.6.2 | HEVC 10-bit | 1280x720 | inf | 8.6 / 9.5 | 16.0 MB |
| rust_h264 0.6.0 | H.264 8-bit High | 1280x720 | inf | 11.3 / 17.8 (noisy) | 14.5 MB |

Timings are whole-call (NAL parse, decode, output copy) for a single IDR, so they are per-keyframe cost, not steady-state throughput. Not measured: the 2 s / 50-frame clip, multithreading, AVX2 vs baseline.

## Perceptual hash (32x32 box-average luma, then image_hasher 3)
- 10-bit input is reduced by >>2 before hashing. The 10-bit and 8-bit decodes of the same frame give identical hash strings.
- Hash bits: mean8 = 64, gradient8 = 64, blockhash16 = 256.
- 1280x720 vs 1920x1080 (separate ffmpeg encodes of testsrc2): Hamming 0 / 0 / 0.
- 1280x720 vs 640x360: Hamming 3 (mean8) / 0 (gradient8) / 1 (blockhash16).
- The three decoders produce identical hash strings (their output is bit-exact).

## Issues found
- Reference pitfall: `ffmpeg -pix_fmt gray` is NOT the Y plane. It rescales range, and the luma differed by up to 19 codes. Use `-pix_fmt yuv420p` (or yuv420p10le) and take the first w*h (x2 for 16-bit) bytes. Using gray gave a false PSNR of 28.7 dB for every decoder.
- rusty_h265 says Range Extensions are refused with `Error::Unsupported` (fine for Main/Main10). rust_h265 says 4:2:2, mono, screen content and >12-bit are refused. rust_h264 refuses 4:4:4, lossless and bit depths other than 8/10.
- Not tested: 4:2:2 / 4:4:4 HEVC, HDR (PQ) metadata, streams with B-frames at display order across more than one GOP, and corrupt-stream robustness.

## Recommendation
Use rust_h265 0.4.0 or rusty_h265 0.6.2 for HEVC Main/Main10 thumbnails. Both are bit-exact on this sample and cost about 10 ms per 720p keyframe. rust_h265 covers 12-bit range extensions and has the HVCC path; rusty_h265 is Apache-2.0 with zero dependencies. rust_h264 is also bit-exact on 8-bit High.

Code: docs/research/stream-metadata/code/w05b_main.rs (uses image 0.25 and image_hasher 3; paths point at the scratchpad sample dir).
