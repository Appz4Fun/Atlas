# W04-C: pure-Rust video decoders for one-keyframe thumbnail + pHash
Date: 2026-11-02. Method: crates.io API (UA atlas-research/0.1), static.crates.io tarballs (extracted in mktemp dir), raw.githubusercontent READMEs, WebSearch. "[read]" = I read that source. "UNVERIFIED" = metadata or claim only, not checked in code.

## Crate / project table
| crate | codec | ver (date) | license | pure Rust | 10-bit | status | source read |
|---|---|---|---|---|---|---|---|
| rust_h265 | HEVC | 0.4.0 (2026-10-30) | MIT OR Apache-2.0 | yes; one `unsafe` module (src/shared.rs) | yes: 8/10, 12 (RExt), 4:2:0/4:4:4 | README: Main/Main10 + RExt, "passes 147 HEVC_v1 vectors", HVCC input (MP4/MKV) | github roticv/rust_h265 README [read]; tarball 27k rs lines; 501k dl (high, UNVERIFIED) |
| rusty_h265 | HEVC | 0.6.2 (2026-10-17) | Apache-2.0 | yes; core `#![forbid(unsafe_code)]` | yes: 8/10, 4:2:0 | README: 147/147 HEVC_v1 bit-exact (claim) | Remade-With-Rust/rusty_h265 README [read]; core 10.5k rs lines; 2.2k dl |
| oxideav-h265 | HEVC | 0.0.16 (2026-10-25) | MIT | yes | README: Main10 + some RExt 10-bit | CONFLICT: README says "decoder end to end", 46/61 official RExt/SCC streams byte-exact; crate description says "parser + decoder scaffold" | README master + published tarball [read]; 97k rs lines incl. tests |
| rust_h264 | H.264 | 0.6.0 (2026-10-30) | MIT OR Apache-2.0 | yes per README; NEON/SSE/AVX2 MC (unsafe status UNVERIFIED) | yes: 8/10, 4:2:0/4:2:2 | README: 199/204 JVT conformance (rest FMO/SP/SI) | github roticv/rust_h264 README [read]; 28k rs lines; 39k dl |
| rusty_h264 | H.264 | 0.17.0 (2026-10-28) | BSD-2-Clause | pure (facade over rusty_h264-decoder) | UNVERIFIED | README: Baseline+B+most High, CABAC; bit-exact vs openh264 | README [read]; decoder sub-crate not read |
| oxideav-h264 | H.264 | 0.1.10 (2026-10-25) | MIT | yes | README: 10-bit, 4:2:2, I/P/B byte-exact | CONFLICT: crate description "under spec-driven rewrite" | README master [read]; 167k rs lines |
| oxideav-mpeg12video | MPEG-1/2 | 0.0.13 (2026-09-01) | MIT | yes | 8-bit 4:2:0 likely; 4:2:2 UNVERIFIED | README: clean-room full pipeline, field pictures, fuzzed, registry-wired | README [read]; 80k rs lines incl. tests; 3k dl |
| na_mpeg2_decoder | MPEG-1/2 | 0.1.0 (2026-10-08) | MPL-2.0 | yes | UNVERIFIED | none read | tarball listing only; 6k lines; 173 dl |
| heifer-hevc-dec | HEVC intra (HEIC stills) | 0.3.17 (2026-11-02) | MIT OR Apache-2.0 | claims no unsafe | UNVERIFIED | intra-only by design | github HarpeLm/Heifer README [read] |
| heic-decoder | HEIC (HEVC intra) | 0.1.1 (2026-10-20) | MIT OR Apache-2.0 | yes | UNVERIFIED | README: "bit-exact vs HEVC reference" | github Multipad-cyber/heic-decoder-rust README [read] |
| zune-heic | HEIC + own HEVC | 0.5.0 (2026-10-11) | MIT/Apache/Zlib | yes | UNVERIFIED | crates.io metadata only | not read |
| videotoolbox | Apple VideoToolbox HEVC/H.264 HW decode | 0.21.2 (2026-10-26) | MIT OR Apache-2.0 | no: C bindings to system framework; Swift bridge only behind `frame_processor` feature | HW HEVC Main10 expected; UNVERIFIED for this crate | README: "experimental", macOS 12+ | README [read]; 11k dl |
| oxideav-videotoolbox | VT HW bridge | 0.0.4 (2026-10-17) | MIT | no (system framework via dlsym, runtime-loaded) | HW only | README [read] |
| oxideav-av1 | AV1 | 0.1.21 | MIT | yes | - | description: "orphan-rebuild scaffold" = not usable | description [read]; 196k lines |
| gamut-av1 | AV1 intra still enc+dec | 0.5.0 (2026-10-16) | UNVERIFIED | yes (claimed) | UNVERIFIED | crates.io description only | not read |
| slain-av1 | AV1 | 0.0.0 | MIT/Apache | claimed | UNVERIFIED | 86 dl; version 0.0.0 | not read |
| filmcraft-h264 / -hevc / -av1 | H.264 / HEVC / AV1 | 0.9.0 (2026-10-23) | MIT/Apache | claimed | UNVERIFIED | no repository field; ~37 dl each | not read |
| gpu-video | HEVC/H.264 Vulkan Video HW | 0.4.0 (2026-05-12) | MIT | GPU | - | needs Vulkan Video; not usable on macOS (inference) | crates.io metadata |
| cros-codecs | VAAPI/V4L2 stateless HW | 0.0.6 (2025-06-18) | BSD-3-Clause | Linux HW only | - | last release 2025-06 | crates.io metadata |

NihAV: not on crates.io; GitHub candidate repos 404 via raw; codecs.multimedia.cx 403 via proxy. Not located, UNVERIFIED.

## YAML notes
feasibility_intra_hevc:
  needed: "VPS/SPS/PPS + slice header parse; CABAC engine + context init tables; coding quadtree (intra CU/PU/TU only); residual_coding; dequant; inverse DCT/DST 4-32; 35 intra modes + reference sample substitution/filtering; PCM"
  loc_estimate: "UNVERIFIED ~6-9k LOC for intra-only Main/Main10 from scratch (own estimate). Reference points read: rusty_h265 core 10.5k lines (full Main/Main10 incl. inter and loop filters); rust_h265 27k (adds RExt, WPP, threads); oxideav-h265 97k incl. tests/fixtures."
  loop_filters: "Intra prediction in HEVC and H.264 reads reconstructed samples before in-loop deblocking/SAO (spec knowledge, not re-read). Skipping deblock+SAO therefore leaves parsing and intra reconstruction intact; output only lacks edge smoothing. Acceptable for a thumbnail and a DCT-based pHash on downscaled luma, UNVERIFIED for hash stability: measure pHash with and without filters on sample clips. rusty_h265 README mentions a filter_bypass test knob."
  keyframe_selection: "Start at first IDR or CRA. Drop RASL pictures that follow a CRA at stream start. Use decoding order (rust_h265 default) so the first picture is emitted without reorder delay."
  container: "MKV CodecPrivate / MP4 hvcC are HVCC: rust_h265 provides parse_hvcc(). Atlas still needs its own demux for the first keyframe sample."
  bitdepth: "10-bit planes are u16 (rust_h265 PixelData::U16). Hash luma only; downconvert only for the thumbnail."

## Recommendation
- HEVC: rust_h265 0.4.0 (MIT/Apache, widespread, HVCC input, README claims full Main/Main10 + RExt). Cross-check against rusty_h265 0.6.2 (Apache, forbid-unsafe core). Pin version; run a local HEVC_v1 spot check before adoption.
- H.264: rust_h264 0.6.0 (MIT/Apache, 8/10-bit, AVCC input per README).
- MPEG-2 (DVD VOB, old remux): oxideav-mpeg12video 0.0.13 first (MIT, clean-room, documented); na_mpeg2_decoder (MPL-2.0) as second. Test one I-frame from a real VOB before choosing.
- AV1: rav1d from wave 1 (not re-checked here). oxideav-av1 is a scaffold, so not usable.
- Apple prod option (decision for owner): videotoolbox 0.21.2 or oxideav-videotoolbox gives HW HEVC decode with no external tool, but it is a system-framework binding, not pure Rust.

## Leads
- heifer-hevc-dec, heic-decoder, zune-heic: intra-only HEVC decoders built for HEIC stills. Check 10-bit and whether raw planes are exposed; could be a lighter path than rust_h265.
- oxideav-h264 (Robert-Hughes/oxideav-h264, separate repo): I-slice CAVLC+CABAC, intra-pred, deblocking. Not evaluated.
- gamut-av1: AV1 intra-only still decoder, a possible keyframe path if rav1d is too heavy. License UNVERIFIED.
- NihAV (Kostya): Rust codec framework; source not located, UNVERIFIED.

## Dead ends
- rav2d, rusty_av2d: AV2, not AV1.
- oxideav-bitstream, scuffle-h264/-h265, h264-reader, h264-parser: header/parser only.
- Reserved name crates: h264 0.0.0, h265 0.0.0, av1 0.0.0.
- pith-h264 0.0.0, ps-rust_h264 (fork, 195 dl): not evaluated further.
- mpeg2ts, mpeg-ts, mpeg2-systems: demux only, no pixels.
- cros-codecs, gpu-video: hardware-only, Linux/Windows.
