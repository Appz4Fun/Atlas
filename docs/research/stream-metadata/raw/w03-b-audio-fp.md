# W03-B audio fingerprint research (Atlas) - 2026-11-02
Method: crates.io API (needs a User-Agent header, plain curl gets 403); static.crates.io tarballs extracted in scratch and read; FFmpeg sources via raw.githubusercontent. Anything not read is UNVERIFIED.

```yaml
- method: chromaprint-style fingerprint
  field: u32 per item, item = 1365 samples at 11025 Hz (0.1238 s)
  where: any-rate interleaved i16; crate resamples internally (rubato) to 11025
  parse: rusty_chromaprint::Fingerprinter::new(&Configuration::preset_test2()); start(sr,ch); consume(&[i16]); finish(); fingerprint()
  use: one fingerprint per track/window; ALL windows must use the same Configuration
  source: rusty-chromaprint-0.3.0 src/fingerprinter.rs, src/audio_processor.rs (read)

- method: segment compare (Chromaprint-style)
  field: Vec<Segment{offset1,offset2,items_count,score}>
  where: rusty_chromaprint::match_fingerprints(fp1, fp2, cfg)
  parse: rusty_chromaprint::fingerprint_matcher::match_fingerprints
  use: score = popcount(XOR) per aligned u32, smoothed; 0..32, lower = more similar. Returns only the single best offset peak (histogram of equal 12-bit hashes), so it is a 1-alignment test, not multi-hit search.
  source: src/fingerprint_matcher.rs (read)

- method: Haitsma-Kalker 32-bit/frame, 5 kHz input
  field: u32 per frame, 78.125 fps (hop 64, n_fft 2048, 33 bands 300-2000 Hz)
  where: caller resamples to 5 kHz mono
  parse: audiofp::classical::Haitsma::new(HaitsmaConfig).extract_with_progress(..)
  use: match if BER <= max_ber 0.35 AND overlap >= min_overlap_frames 256 (~3.3 s). informative_frames() rejects silence/steady tones.
  source: audiofp-0.4.5 src/classical/haitsma.rs, src/matching/haitsma.rs (read)

- method: AC-3 syncframe CRC (bit-exact identity key)
  field: crc1 = bytes[2..4] (after 0x0B77 syncword); crc2 = last 2 bytes of syncframe
  where: frame length from fscod+frmsizecod (oxideav_ac3 tables::frame_length_bytes)
  parse: oxideav_ac3::crc::verify_ac3_syncframe(frame, len) (doc-hidden); FFmpeg ac3_parser.c:315 reads crc1 with get_bits(16)
  use: crc values depend only on frame bytes, not file offset -> position-independent. Key = crc2 sequence over N consecutive frames. Digital-silence frames repeat, so skip low-energy runs.
  source: oxideav-ac3-0.0.11 src/crc.rs (read); FFmpeg libavcodec/ac3_parser.c (read)

- method: E-AC-3 crc2 only
  field: last 2 bytes of syncframe; no crc1
  parse: oxideav_ac3::crc::verify_eac3_syncframe
  use: same as AC-3 crc2; dependent substreams need separate check (UNVERIFIED)
  source: oxideav-ac3 src/crc.rs (read)

- method: DTS core header CRC (OPTIONAL)
  field: 16-bit CRC right after Predictor History bit, present only if crc_present flag (bit after Deficit Sample Count) is 1
  parse: revelo-parsers-audio dts.rs parse_dts (header layout); FFmpeg dca_core.c:525 side-info CRC if crc_present
  use: many DTS streams likely have crc_present=0 (UNVERIFIED frequency). Not a universal key.
  source: revelo-parsers-audio-0.5.6 src/dts.rs (read); FFmpeg dca_core.c (read)

- method: TrueHD/MLP check data
  field: major-sync CRC-16 (sync AUs only, over major sync info bytes); per-AU 4-bit parity nibble (non-sync AUs)
  where: AU header bytes after 4-byte sync word
  parse: truehd-0.7.2 src/process/extract.rs crc16_major_sync_info (~l.234-311) and parity (~l.363-417); FFmpeg libavcodec/mlp_parser.c:139-156
  use: NOT a content key. Major sync info is format-only (same across many titles). For identity hash full AU payloads of first K AUs (UNVERIFIED that AU bytes are stable across remux).
  source: truehd-0.7.2 (read); FFmpeg mlp_parser.c (read)

- method: decode to PCM (TrueHD)
  parse: truehd crate process/decode.rs decode_presentation (l.27)
  use: research-grade; only if PCM needed for chromaprint path
  source: truehd-0.7.2 src/process/decode.rs (read, signatures only)

- method: alignment budget (audio seconds in first N bytes)
  field: audio_seconds ~= N_bytes / total_bitrate (audio is time-interleaved)
  examples: 2160p remux ~50 Mb/s -> 5 MB ~ 0.8 s (10 s needs ~60 MB). 1080p encode ~3 Mb/s -> 5 MB ~ 13 s. AC-3 5.1 @640 kb/s = 2560 B per 1536-sample frame (32 ms).
  use: a chromaprint window of 10-30 s does NOT fit in 5 MB for high-bitrate remuxes. Use compressed-domain keys there. Estimates, UNVERIFIED against real files.
  t=0 silence: UNVERIFIED. Sample several offsets and skip silent runs.
```

## Crate table
| crate | version | license | downloads (total / version) | pure Rust | notes |
|---|---|---|---|---|---|
| rusty-chromaprint | 0.3.0 | MIT | 275,259 / 213,290 | yes | rubato 0.16, rustfft 6.2; last update 2024-10 |
| audiofp | 0.4.5 | MIT | 1,602 / 13 | yes (Symphonia opt-in) | Wang, Panako, Haitsma; rust 1.93; 2026-04 |
| symphonia | 0.6.1 | MPL-2.0 | 19.9M / 2.0M | yes | codecs: aac adpcm alac flac mp1-3 pcm vorbis. NO ac3/eac3/dts/truehd/opus. containers isomp4, mkv, ogg, wav |
| oxideav-ac3 | 0.0.11 | MIT | 8,646 / 1,686 | yes | AC-3/E-AC-3 decoder with CRC verify; created 2026-04 |
| oxideav-mkv / oxideav-mp4 | 0.0.11 / 0.0.10 | MIT | 9,473 / 3,708 | yes | containers, NOT inspected |
| truehd | 0.7.2 | Apache-2.0 | 3,691 / 597 | yes | research TrueHD/MLP parser+decoder, CRC+parity |
| revelo-parsers-audio | 0.5.6 | BSD-2-Clause | 1,924 / 325 | yes | header parsers AC-3, DTS, DTS:UHD, ADTS; no decode |
| dolby_vision | 3.4.0 | MIT | 64,889 / 16,271 | yes | RPU parse (other worker) |
| chromaprint-next | 0.1.0 | MIT AND LGPL-2.1+ | 3,496 | yes | LGPL part, license caution |
| chromaprint-sys-next | 1.6.0 | not checked | 17,836 | NO (FFI) | libchromaprint binding |
| songrec | 0.8.0 | GPL-3.0+ | 36,369 | n/a | do not depend on it |

## Leads
- AC-3 exponents (oxideav_ac3 audblk.rs decode_exponents, l.1227, pub(crate)) give a per-block spectral envelope without mantissas. Haitsma-style band sign bits from exponents might work at much lower cost. UNVERIFIED, needs a fork.
- Build a per-file key list: sequence of AC-3 crc2 over the first K frames, plus a 10-30 s chroma window where bytes allow.
- crates.io search for "dts" and "truehd" is noisy. Leads on DTS core decoding were not found. UNVERIFIED.

## Dead ends
- Crate names not on crates.io: ac3, dtsx, dolby-ac3, aac-decoder, soundprint, panako, olaf-rs, fpcalc, audio-fingerprint.
- No pure-Rust DTS core decoder found (dts-core 0.2.7 last updated 2022, not inspected).
- Symphonia 0.6.1 has no AC-3, E-AC-3, DTS, TrueHD, or Opus decoder.
- Chromaprint duration guidance (AcoustID ~120 s, min ~10 s) not confirmed from the README; UNVERIFIED.
