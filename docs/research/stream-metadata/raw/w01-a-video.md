# w01-a: video ES metadata (HEVC, AVC, AV1) without decode
Verified = seen in fetched source/doc text. UNVERIFIED = memory or not confirmed.

```yaml
- field: hvcC configurationVersion, general_profile_space, general_tier_flag, general_profile_idc
  where: hvcC box / MKV CodecPrivate (ISO 14496-15, HEVCDecoderConfigurationRecord)
  parse: manual bit read
  use: profile (Main/Main10), tier
  source: https://ffmpeg.org/doxygen/7.1/structHEVCDecoderConfigurationRecord.html (field list verified)

- field: hvcC general_profile_compatibility_flags, general_constraint_indicator_flags, general_level_idc
  where: hvcC
  parse: manual
  use: level (153 = 5.1), compat flags
  source: same (verified)

- field: hvcC chromaFormat, bitDepthLumaMinus8, bitDepthChromaMinus8
  where: hvcC
  parse: manual
  use: 4:2:0/4:2:2/4:4:4, 8/10/12-bit
  source: same (verified)

- field: hvcC avgFrameRate, constantFrameRate, numTemporalLayers, temporalIdNested, lengthSizeMinusOne
  where: hvcC
  parse: manual
  use: fps (often 0, fall back to VUI), CFR flag, NAL length size for walking NALs
  source: same (verified)

- field: hvcC arrays[] (VPS/SPS/PPS/SEI, array_completeness, NAL_unit_type, numNalus)
  where: hvcC after numOfArrays
  parse: manual; feed SPS/VPS parser
  use: parameter sets from the header alone, same in MKV CodecPrivate
  source: same (verified)

- field: av1C seq_profile, seq_level_idx_0, seq_tier_0, high_bitdepth, twelve_bit, monochrome, chroma_subsampling_x/y, chroma_sample_position
  where: av1C box / MKV CodecPrivate, 4-byte fixed header (marker=1, version=1)
  parse: manual, or scuffle-av1 demux() (fields verified)
  use: AV1 profile/level/tier, depth, mono, subsampling
  source: https://cdn.jsdelivr.net/gh/AOMediaCodec/av1-isobmff@1.1.0/index.html ; https://docs.rs/scuffle-av1/0.1.4/src/scuffle_av1/config.rs.html

- field: av1C configOBUs (Sequence Header OBU; hdr_wcg_idc; initial_presentation_delay_minus_one)
  where: av1C tail; sequence header must be first and not repeated
  parse: leb128 OBU walk; scuffle-av1 exposes hdr_wcg_idc, config_obu (verified)
  use: HDR/WCG hint, full color_config and timing_info
  source: av1-isobmff spec (verified); full color_config parse UNVERIFIED

- field: SPS pic size, conformance window (conf_win_*_offset)
  where: HEVC SPS (NAL 33); AVC SPS (NAL 7); AV1 Sequence Header frame size
  parse: scuffle-h265 SPS / ConformanceWindow (names verified; SpsRbsp not read)
  use: coded vs display res after crop (1920x1088 -> 1920x1080)
  source: https://docs.rs/crate/scuffle-h265/latest/source/src/sps/vui_parameters/mod.rs

- field: VUI aspect_ratio_info (aspect_ratio_idc, sar), overscan, chroma_loc_info, field_seq_flag, frame_field_info_present_flag, default_display_window
  where: SPS VUI
  parse: scuffle-h265 VuiParameters (names verified)
  use: SAR/DAR, interlace hint (field_seq_flag), chroma siting
  source: same as above

- field: VUI video_signal_type: video_full_range_flag, colour_primaries, transfer_characteristics, matrix_coeffs
  where: SPS VUI
  parse: scuffle-h265 VideoSignalType (colour_primaries, transfer_characteristics, matrix_coeffs verified)
  use: HDR10 (PQ/BT.2020) vs SDR (BT.709), full/limited range
  source: same as above

- field: VUI vui_timing_info num_units_in_tick, time_scale (fps = time_scale / num_units_in_tick)
  where: SPS VUI
  parse: scuffle-h265 VuiTimingInfo (NonZero<u32>, verified)
  use: exact fps (24000/1001 = 23.976)
  source: same as above

- field: x264 SEI user_data_unregistered, payload "x264 - core NNN rNNNN ... options: ..."
  where: first IDR AU, SEI NAL (AVC type 6, HEVC type 39/40). Usually not in hvcC/avcC arrays
  parse: manual SEI payload type 5, 16-byte UUID then ASCII
  use: encoder build + all options (crf, ref, bframes, aq, psy-rd): strongest release tag
  source: UNVERIFIED (UUID dc45e9bd-e6d9-48b7-962c-d820d923eeef from memory; x264 base.c fetch had no SEI writer)

- field: x265 SEI user_data_unregistered, payload "x265 (build NNN) ... options: ..."
  where: first IDR AU, HEVC SEI
  parse: manual, as x264
  use: x265 build + options (crf, preset, aq-mode, hdr10-opt)
  source: UNVERIFIED (ffmpeg-devel 2021 mail mentions the SEI; page fetch not read)

- field: NVENC / SVT-AV1 / aomenc / rav1e tags
  where: unknown
  parse: manual
  use: encoder family
  source: UNVERIFIED

- field: average bitrate
  where: MKV Tags BPS (mkvmerge statistics, at end of file, not in first MBs); MP4 btrt (avgBitrate, maxBitrate) and esds avgBitrate
  parse: manual, or fallback file_size*8/duration
  use: avg kbps
  source: mkvtoolnix track_statistics.cpp listing (not read in full)
```

## Crates
| crate | version | license | parses | notes |
|---|---|---|---|---|
| scuffle-h265 | 0.2.2 | MIT/Apache-2.0 | SPS, VUI, timing, conf window, HRD | "unstable", last update Apr 2025 (lib.rs); VPS/PPS/SEI not confirmed |
| scuffle-av1 | 0.1.4 | not checked | av1C record, config OBU | fields verified |
| h264-reader | 0.8.0 | MIT/Apache | SPS, PPS, SEI (pic timing, recovery, film grain, mastering display), Annex B and AVCC | ~2.3k downloads/month (Apr 2025); VUI not listed |
| hevc_parser (quietvoid) | ? | not stated | unknown | no parse scope listed |

## Leads for next wave
- Confirm x264/x265 SEI UUIDs in encoder source (x264 encoder/ SEI file; x265 source/encoder/sei.cpp).
- Check h264-reader and scuffle-h264 for VUI; SpsRbsp in scuffle-h265.
- MKV tag names (BPS, DURATION) in the ebml crate; MP4 btrt/esds in mp4 / re_mp4.
- DV RPU and HDR10+ are in SEI / UNSPEC62, out of scope here.

## Dead ends
- FFmpeg-devel x265 patch page: access-denied, no content.
- x264 base.c: param2string only. x265 encoder.cpp: truncated before NAL writes.
- lib.rs hevc_parser page: no scope listed.
