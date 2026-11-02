# w03-a: Rust-native keyframe decode and perceptual hash (Atlas)

Read: static.crates.io tarballs, index.crates.io sparse index. crates.io API 403 via proxy, so downloads UNAVAILABLE. Unread items marked UNVERIFIED.

status: No pure-Rust HEVC pixel decoder found. AV1 keyframe decode is possible via rav1d (C-ABI style, unsafe). H.264 8-bit keyframe decode via openh264 (vendored C, static build). HEVC Main10/HDR keyframe pixel decode: no clean static path found.

items:
- field: AV1 keyframe pixels
  method: decode one KEY frame TU
  where: OBU stream in MKV SimpleBlock / MP4 mdat (first TU with sequence header)
  parse: rav1d 1.1.0 src/lib.rs: dav1d_default_settings, dav1d_open, dav1d_send_data, dav1d_get_picture (extern "C", unsafe)
  use: loop send_data / get_picture until a picture with key frame; read planes (8 or 10-bit)
  source: https://static.crates.io/crates/rav1d/rav1d-1.1.0.crate (README.md, src/lib.rs)
  note: README says a Rust API is planned (issue 1252), none present. x86 asm needs nasm. Repo pins nightly in rust-toolchain.toml; README documents `cargo +stable build --lib`.
- field: AV1 keyframe pixels (alt)
  method: dav1d-rs bindings
  where: same as above
  parse: dav1d 0.11.1 (rust-av/dav1d-rs), needs system libdav1d via system-deps
  use: DEAD END under static-build rule (dav1d-sys 0.8.3 same issue)
  source: https://static.crates.io/crates/dav1d/dav1d-0.11.1.crate (README.md)
- field: H.264 IDR pixels (8-bit)
  method: openh264 Decoder::decode on each NAL
  where: Annex B NALs (SPS, PPS, IDR slice). For AVCC/MP4/MKV convert length prefixes to start codes first (UNVERIFIED utility, about 20 lines)
  parse: openh264 0.9.9 src/decoder.rs: Decoder::new(), decode(&[u8]) -> Result<Option<DecodedYUV>>; src/utils.rs:48 nal_units(); DecodedYUV::split, write_rgb8
  use: feed SPS, PPS, IDR; take first Some(YUV); output 8-bit only (no 10-bit path seen)
  static: openh264-sys2 0.9.9 default feature "source" compiles vendored upstream/ via cc (optional nasm-rs). Verified in Cargo.toml.
  source: https://static.crates.io/crates/openh264/openh264-0.9.9.crate ; openh264-sys2-0.9.9.crate
  note: High-profile/CABAC and Main10 coverage UNVERIFIED. Main10 HDR not expected to work.
- field: HEVC keyframe pixels
  method: libde265 bindings (C libde265)
  where: Annex B NALs (VPS/SPS/PPS/IDR or CRA)
  parse: libde265 0.2.0 src/decoder.rs: Decoder::push_nal, push_data, decode(), get_next_picture() -> Image; image.rs get_bits_per_pixel
  use: UNVERIFIED end to end. Underlying libde265-sys 0.1.1 (C, upstream LGPL-3.0, UNVERIFIED). Static link triggers LGPL obligations.
  source: https://static.crates.io/crates/libde265/libde265-0.2.0.crate (Cargo.toml, README.md)
- field: HEVC NAL / IDR / CRA detection and slice header
  method: parse only, no pixels
  where: Annex B or length-prefixed NALs
  parse: hevc_parser 0.6.12 src/lib.rs split_nals; src/hevc/mod.rs NAL_IDR_W_RADL=19, NAL_IDR_N_LP=20, NAL_CRA_NUT=21, is_type_slice; src/hevc/slice.rs parse; src/hevc/sei.rs:23 parse_sei_rbsp
  use: pick IDR/CRA with first_slice_segment flag; skip RASL (types 8,9) after a CRA that starts the stream (spec knowledge, UNVERIFIED in code)
  source: https://static.crates.io/crates/hevc_parser/hevc_parser-0.6.12.crate
- field: HEVC SPS (bit depth, chroma, VUI, colour)
  method: header decode
  parse: scuffle-h265 0.2.2 src/sps/mod.rs SpsNALUnit::parse(reader), SpsRbsp::parse(reader, nuh_layer_id)
  use: get bitdepth and coded size before decode; no pixels
  source: https://static.crates.io/crates/scuffle-h265/scuffle-h265-0.2.2.crate
- field: container keyframe flags and timing (no decode)
  method: MKV block keyframe bit
  where: SimpleBlock / BlockGroup
  parse: matroska-demuxer 0.8.1 src/lib.rs:1561 MatroskaFile::next_frame(&mut Frame); Frame{track,timestamp,data,is_keyframe: Option<bool>,duration}; tracks(); TrackEntry::codec_id, codec_private (avcC/hvcC/av1C), video(), content_encodings(); seek(ts) at :1685
  use: keyframe timestamps in the first few MB give GOP cadence; codec_private gives SPS/PPS/VPS
  source: https://static.crates.io/crates/matroska-demuxer/matroska-demuxer-0.8.1.crate
- field: perceptual hash (image)
  method: image_hasher 3.1.1 HasherConfig with HashAlg
  where: 32x32 luma grid after resize
  parse: image_hasher src/alg/mod.rs HashAlg {Mean, Median, Gradient, VertGradient, DoubleGradient, ...} (DCT and Blockhash variants UNVERIFIED); HasherConfig::resize_filter (default Lanczos3)
  use: Median or Gradient on luma; compare hamming distance
  source: https://static.crates.io/crates/image_hasher/image_hasher-3.1.1.crate
- field: blockhash
  method: blockhash16/64/144/256(&impl Image)
  parse: blockhash 1.0.0 src/lib.rs:121,212,307,392; trait Image { dimensions(), brightness(x,y), MAX_BRIGHTNESS }; distance()
  use: implement Image over a luma buffer; median-thresholded blocks
  source: https://static.crates.io/crates/blockhash/blockhash-1.0.0.crate
- field: no-decode same-encode key
  method: hash the NAL/OBU payload bytes of the first IDR (after start code or length prefix removal)
  survives: remux (MKV->MP4) if NAL payload bytes are unchanged
  breaks: re-encode; MKV header stripping (ContentEncodings, check content_encodings()); AV1 temporal delimiters removed in Matroska (UNVERIFIED, spec memory)
  prior art: ffmpeg `-c copy -f framemd5` hashes packets (concept only, tool ruled out; UNVERIFIED)

normalisation (for PQ/HDR vs SDR and 1080p vs 2160p):
- Crop letterbox bars first.
- Box-average (area) downscale to 32x32 luma. Same grid for any resolution.
- For PQ, do not decode to linear for hashing. Use rank order: any monotonic tone curve preserves ordering, so median-thresholded hashes (blockhash, image_hasher Median) and gradient hashes are largely invariant to HDR vs SDR grading.
- Histogram equalise (rank transform) to 0..255 before hashing for extra robustness (small grading changes).
- Skip frames whose normalised luma is nearly flat (black or logo): e.g. >95% pixels under 16/255, or stddev below a threshold.

frame choice (no decode of more than needed):
- Use slice header first_slice_segment_in_pic_flag = 1 and slice type I, IDR or CRA.
- First IDR often a logo or black. Take the next keyframe whose frame is not flat.
- Caveat: at 10-20 Mbps, a few MB is about 1-3 s, so a second keyframe may be absent. The GOP cadence from matroska keyframe flags (Cues) tells you if a second exists.

crate table (latest non-yanked per index; downloads UNAVAILABLE):
| crate | ver | license | kind | pubtime |
| rav1d | 1.1.0 | BSD-2-Clause | Rust port of dav1d, C-ABI API | 2025-05-07 |
| dav1d | 0.11.1 | MIT | C bindings, system lib | 2025-11-25 |
| openh264 | 0.9.9 | BSD-2-Clause | C vendored, static | 2026-10-15 |
| libde265 | 0.2.0 | MIT OR Apache-2.0 | C bindings, LGPL core (UNVERIFIED) | 2025-10-31 |
| cros-codecs | 0.0.6 | BSD-3-Clause | Rust, VAAPI/V4L2 hardware only | 2025-06-18 |
| hevc_parser | 0.6.12 | MIT | pure Rust parser | 2026-08-29 |
| scuffle-h265 | 0.2.2 | MIT OR Apache-2.0 | pure Rust header parser | 2025-05-17 |
| less-avc | 0.1.5 | MIT/Apache-2.0 | pure Rust H.264 encoder only | 2023-08-29 |
| image_hasher | 3.1.1 | MIT OR Apache-2.0 | pure Rust | 2026-02-21 |
| img_hash | 3.2.0 | UNVERIFIED | pure Rust, older | 2021-05-04 |
| blockhash | 1.0.0 | MIT | pure Rust | 2024-04-08 |
| matroska-demuxer | 0.8.1 | Zlib OR MIT OR Apache-2.0 | pure Rust | 2026-08-07 |
| mp4parse | 0.17.0 | UNVERIFIED | Rust (Mozilla) | 2023-05-29 |

leads:
- rav1d Rust API when it lands (issue 1252); removes unsafe.
- mp4parse 0.17.0 for stss/stsz keyframe indices in MP4 (UNVERIFIED, not read).
- h264-reader 0.9.0 (2026-09-14) for AVC slice headers (UNVERIFIED, not read).
- phash 0.1.0, dhash 0.1.0, imagehash 0.3.0: tiny, UNVERIFIED.

dead ends:
- dav1d / dav1d-sys: system libdav1d, not static.
- cros-codecs: needs GPU/VAAPI or V4L2.
- less-avc: encoder only.
- vvdec 0.6.11: VVC (H.266), not HEVC.
- Absent from index: hevc, hevc-decoder, h265-decoder, openhevc, de265, image-hasher, perceptual-hash.
