# w01-b HDR10 / HDR10+ / Dolby Vision field map (research, 2026-11-02)
# Verified = read from source/spec page this session. UNVERIFIED = from memory or failed fetch.

fields:
- field: HDR10 mastering display (SMPTE ST 2086): primaries R/G/B x,y; white point x,y; LuminanceMax, LuminanceMin
  where: MKV TrackEntry > Video > Colour (0x55B0) > MasteringMetadata (0x55D0) > PrimaryRChromaticityX 0x55D1, Y 0x55D2; G 0x55D3/0x55D4; B 0x55D5/0x55D6; WhitePointChromaticityX 0x55D7, Y 0x55D8; LuminanceMax 0x55D9; LuminanceMin 0x55DA
  parse: EBML element walk (no crate checked); Verified IDs
  use: Flags HDR10 mastering presence and peak nits (e.g. 1000 vs 4000 cd/m2 masters) for release description
  source: matroska.org/technical/elements.html

- field: MaxCLL / MaxFALL (CTA-861.3 light levels)
  where: MKV Colour 0x55BC MaxCLL, 0x55BD MaxFALL; HEVC SEI payloadType 144 (content light level) UNVERIFIED payload layout (max_cll u16, max_fall u16 assumed)
  parse: EBML; SEI 144 via libavcodec h2645_sei (not fetched)
  use: Content peak/average brightness; sometimes 0 on rips, so presence check matters
  source: https://www.matroska.org/technical/elements.html (MKV verified); SEI UNVERIFIED

- field: Mastering display SEI (payloadType 137) and MP4 mdcv / clli boxes; AV1 metadata OBU types HDR_CLL / HDR_MDCV
  where: HEVC SEI 137 (UNVERIFIED layout: 3x(x,y) u16 at 0.00002 units, white x,y u16, max lum u32 at 0.0001 cd/m2, min lum u32); MP4 mdcv/clli inside sample entry (UNVERIFIED); AV1 metadata OBU types (UNVERIFIED numeric values)
  parse: unverified; ffmpeg libavcodec/h2645_sei.c not fetched
  use: Same HDR10 signal in MP4/AV1 containers when MKV tags absent
  source: UNVERIFIED (ffmpeg libavcodec/h2645_sei.c not fetched)

- field: HDR10+ dynamic metadata (SMPTE ST 2094-40) presence and profile
  where: HEVC SEI payloadType 4 (user_data_registered_itu_t_t35): country 0xB5, provider 0x003C, provider-oriented code 0x0001, application identifier 4 (UNVERIFIED, from brief; not confirmed in source this session)
  parse: hdr10plus crate (quietvoid/hdr10plus_tool), exact parse fn UNVERIFIED
  use: Distinguish HDR10+ vs plain HDR10 on a WEB-DL; detect from first IDR access unit
  source: github.com/quietvoid/hdr10plus_tool (lib.rs fetched, metadata module not reached)

- field: HDR10+ per-window fields: targeted_system_display_maximum_luminance, maxscl, average_maxrgb, distribution percentiles, bezier curve, num_windows
  where: inside ST 2094-40 SEI payload
  parse: hdr10plus crate metadata module (UNVERIFIED names; brief only)
  use: Per-scene peak/avg brightness; bezier presence indicates tone mapping curve
  source: UNVERIFIED

- field: Dolby Vision decoder config record (dvcC / dvvC / dvwC)
  where: MP4 dvcC/dvvC box; MKV TrackEntry > BlockAdditionMapping 0x41E4 > BlockAddIDType 0x41E7 (type string UNVERIFIED, expected dvcC/dvvC/dvwC) and BlockAddIDExtraData 0x41ED (holds the record)
  parse: ffmpeg AVDOVIDecoderConfigurationRecord is the reference layout; Rust parse fn not verified
  use: Profile, level, and whether an EL is present, without decoding any video. Best single-field signal for P7 vs P8
  source: https://ffmpeg.org/doxygen/8.0/structAVDOVIDecoderConfigurationRecord.html ; github.com/FFmpeg/FFmpeg/blob/master/libavutil/dovi_meta.h

- field: dv_profile, dv_level
  where: dvcC/dvvC (bit packing UNVERIFIED; ffmpeg struct says uint8 each)
  use: Profile 5 (IPT-PQ, no BL compat), 7 (dual layer, FEL/MEL), 8.1/8.2/8.4 (single layer, compat id), 10 (AV1). Level sets max resolution/fps (level->res table UNVERIFIED)
  source: ffmpeg dovi_meta.h (no values listed); profile list partial in dovi_tool README

- field: rpu_present_flag, el_present_flag, bl_present_flag, dv_bl_signal_compatibility_id, dv_md_compression
  where: dvcC/dvvC; dv_md_compression enum verified: NONE=0, LIMITED=1, RESERVED=2, EXTENDED=3
  use: el_present_flag=1 on a profile 7 file means dual-layer remux candidate; compat id (e.g. 1 = HDR10 base, 4 = HLG, 0 = none) UNVERIFIED values
  source: raw.githubusercontent.com/FFmpeg/FFmpeg/master/libavutil/dovi_meta.h

- field: RPU header (per RPU): rpu_type, vdr_rpu_profile, bl_video_full_range_flag, el_spatial_resampling_filter_flag, disable_residual_flag, use_prev_vdr_rpu_flag, vdr_dm_metadata_present_flag
  where: HEVC NAL 62 (UNSPEC62) or AV1 T.35 metadata OBU
  parse: dolby_vision crate (MIT, v3.4.0) rpu::dovi_rpu::RpuDataHeader; DoviRpu::parse_unspec62_nalu, DoviRpu::parse_rpu, DoviRpu::parse_itu_t35_dovi_metadata_obu, DoviRpu::parse_list_of_unspec62_nalus
  use: vdr_rpu_profile and disable_residual_flag distinguish FEL/MEL-type structure
  source: raw.githubusercontent.com/quietvoid/dovi_tool/main/dolby_vision/src/rpu/dovi_rpu.rs

- field: el_type (FEL vs MEL)
  where: DoviRpu.el_type: Option<DoviELType> (set from RpuDataMapping::get_enhancement_layer_type -> RpuDataNlq::el_type)
  parse: DoviRpu::get_enhancement_layer_type(); decision logic in rpu_data_nlq.rs NOT fetched, UNVERIFIED
  use: Core FEL vs MEL distinction for P7 remux labeling. Parsing gate verified: NLQ block only when rpu_format & 0x700 == 0 and !disable_residual_flag. Profile 7 validate requires nlq_pred_pivot_value to sum to 1023; profiles 5/8 must not carry NLQ fields
  source: raw.githubusercontent.com/quietvoid/dovi_tool/main/dolby_vision/src/rpu/rpu_data_mapping.rs

- field: DM metadata blocks (ext blocks)
  where: VdrDmData.cmv40_metadata: Option<DmData>, DmData::V40(CmV40DmData) verified (CM v4.0 present); CM v2.9 variant name UNVERIFIED
  parse: dolby_vision crate rpu / extension_metadata modules
  use: CM v2.9 vs v4.0 is the main "which grading era" tag
  source: dovi_rpu.rs (above)
  block levels (dovi_tool README): CMv2.9 = L1, L2, L5, L6. CMv4.0 = those + L3, L8, L9, L10, L11. L4, L254, L255 UNVERIFIED
  L2/L8 trims (per target display): slope, offset, power, chroma, saturation, ms, mid, clip (L8 only mid and clip). Source: dovi_tool README
  L5 active area: letterbox offsets in canvas; dovi_tool needs canvas width and height to generate
  source: raw.githubusercontent.com/quietvoid/dovi_tool/main/README.md
- field: frames needed / constancy
  use: Read first 1-2 access units and cross-check the container config record. L2/L8 per-shot constancy UNVERIFIED.

leads for next wave:
- hdr10plus_tool hdr10plus/src/metadata.rs and hevc.rs (field names, SEI parse path)
- dovi_tool dolby_vision/src/rpu/rpu_data_nlq.rs (el_type rule) and extension_metadata/ (block structs)
- FFmpeg libavcodec/h2645_sei.c (T.35, SEI 137/144); FFmpeg patch 20240403154330.71585-6 (guess_hevc_profile)
- ISO/IEC 14496-12 + Dolby HLS spec for dvcC bit layout and level table

dead ends:
- hdr10plus_tool lib.rs: module declarations only
- docs.rs dolby_vision top page: module list only, rpu module only 15.5% documented
- dovi_tool README: no FEL/MEL rule, no profile 8.2 or 10
- ffmpeg dovi_meta.h and AVDOVIDecoderConfigurationRecord page: no value meanings or comments
