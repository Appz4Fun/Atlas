# W02-B DoVi / HDR10+ source findings (Atlas)
Crate: dolby_vision 3.4.0 (static.crates.io tarball; src/ paths are inside it).

```yaml
- field: el_type FEL/MEL
  where: dolby_vision-3.4.0/src/rpu/rpu_data_nlq.rs (RpuDataNlq::is_mel, el_type); rpu_data_mapping.rs (nlq parse gate)
  parse: "is_mel() = nlq_offset all 0 AND vdr_in_max_int all 1 AND vdr_in_max all 0 AND linear_deadzone_slope(_int) all 0 AND linear_deadzone_threshold(_int) all 0; el_type() = MEL if is_mel else FEL. NLQ block only parsed when rpu_format & 0x700 == 0 AND !disable_residual_flag (profile 7 path); el_type None if use_prev_vdr_rpu_flag set"
  use: inferred from NLQ coefficient values, not a spec bit. With use_prev_vdr_rpu_flag=1, el_type is None: carry the last value forward.
  source: https://static.crates.io/crates/dolby_vision/dolby_vision-3.4.0.crate

- field: profile from RPU header
  where: rpu/rpu_data_header.rs get_dovi_profile
  parse: "vdr_rpu_profile==0 && bl_video_full_range -> 5; ==1 && el_spatial_resampling_filter_flag && !disable_residual_flag -> (vdr_bit_depth_minus8==4 ? 7 : 4); ==1 otherwise -> 8"
  use: profile 7/8/5 from RPU alone. Profile 4 inferred.
  source: static.crates.io dolby_vision-3.4.0.crate

- field: CMv2.9 vs CMv4.0 block levels
  where: rpu/extension_metadata/cmv29.rs (ALLOWED_BLOCK_LEVELS), cmv40.rs (ALLOWED_BLOCK_LEVELS, parse_block)
  parse: "CM v2.9 = [1,2,4,5,6,255]; CM v4.0 = [3,8,9,10,11,254]. cmv40 parse_block bails on 1,2,4,5,6,255 and errors on unknown levels (FIXME in source). Block header = ue(ext_block_length), u8 level"
  use: level-to-version map. vdr_dm_data.rs: CMv2.9 list first, CMv4.0 list if enough bits remain.
  source: static.crates.io dolby_vision-3.4.0.crate

- field: block fields and widths (all read via BsIoSliceReader)
  where: rpu/extension_metadata/blocks/level{1,2,3,4,5,6}.rs (v2.9); level{3,8,9,10,11,254}.rs (v4.0); level255.rs (v2.9)
  parse: "L1 min_pq/max_pq/avg_pq u12. L2 target_max_pq, trim_slope, trim_offset, trim_power, trim_chroma_weight, trim_saturation_gain u12, ms_weight i13. L3 min/max/avg_pq_offset u12. L4 anchor_pq u12, anchor_power u12. L5 active_area_left/right/top/bottom_offset u13. L6 max_display_mastering_luminance, min_display_mastering_luminance, max_content_light_level, max_frame_average_light_level u16. L8 length(ue), target_display_index u8, trim_slope/offset/power/chroma_weight/saturation_gain/ms_weight u12, target_mid_contrast u12, clip_trim u12, saturation_vector_field0-5 u8, hue_vector_field0-5 u8. L9 length, source_primary_index u8, 8 x u16 primaries. L10 length, target_display_index u8, target_max_pq u12, target_min_pq u12, target_primary_index u8, 8 x u16 primaries. L11 content_type u8, byte1 decoded (whitepoint, reference_mode_flag), 2 reserved u8. L254 dm_mode u8, dm_version_index u8. L255 dm_run_mode, dm_run_version, dm_debug0-3 u8"
  use: exact field list (L11 byte1 sub-fields UNVERIFIED).
  source: static.crates.io dolby_vision-3.4.0.crate

- field: dovi_tool info / summary output
  where: dovi_tool src/dovi/rpu_info.rs (info, RpusListSummary::new)
  parse: "Input = RPU .bin (extract-rpu output), read whole file by dolby_vision utils parse_rpu_file. Printed: Frames=RPU count; Profile(s) + EL type per RPU; DM version (1=CMv2.9, 2=CMv4.0, 1+2 mixed); Scene/shot count = count of RPUs with vdr scene_refresh_flag==1 (not distinct shots); RPU mastering display = unique (source_min_pq, source_max_pq); MaxCLL = pq_to_nits(max over frames of L1.max_pq); MaxFALL = pq_to_nits(max over frames of L1.avg_pq) (DM-derived, not L6); L5 top/bottom/left/right min..max; L6 unique records; L2 trims = unique target nits from target_max_pq; L8 trims (v4 only) unique target nits; L9 primary-index counts (v4 only)"
  use: first-N-frames partial: profile, EL type, DM version, L5/L6 (usually constant), L9 index, mastering display (if constant) are fine as estimates. Needs whole file for exact: frame count, scene count, MaxCLL/MaxFALL max, L2/L8 trim sets, L5 min..max range. First-N gives lower bounds only for these.
  source: https://raw.githubusercontent.com/quietvoid/dovi_tool/main/src/dovi/rpu_info.rs ; https://static.crates.io/crates/dolby_vision/dolby_vision-3.4.0.crate (rpu/utils.rs)

- field: dvcC / dvvC / dvwC config record bit layout
  where: FFmpeg libavformat/dovi_isom.c ff_isom_parse_dvcc_dvvc (read) and ff_isom_put_dvcc_dvvc
  parse: "byte0 dv_version_major u8; byte1 dv_version_minor u8; bytes2-3 big-endian u16: dv_profile = bits15..9 (7), dv_level = bits8..3 (6), rpu_present_flag bit2, el_present_flag bit1, bl_present_flag bit0; byte4: dv_bl_signal_compatibility_id = bits7..4 (4), dv_md_compression = bits3..2 (2), bits1..0 reserved. Payload size 24 (ISOM_DVCC_DVVC_SIZE, dovi_isom.h). Parser needs size>=4; compat id read only when size>=5, else 0"
  use: profile, level, flags from the box.
  source: https://raw.githubusercontent.com/FFmpeg/FFmpeg/master/libavformat/dovi_isom.c ; https://raw.githubusercontent.com/FFmpeg/FFmpeg/master/libavformat/dovi_isom.h

- field: dv_level table (level -> max pixels/fps)
  where: UNVERIFIED (not in any source read)
  parse: UNVERIFIED
  use: needs a Dolby spec or table source before use.
  source: none read

- field: compat id meaning (0 none, 1 HDR10, 2 SDR, 4 HLG, 6 BD)
  where: UNVERIFIED (ffmpeg libavutil/dovi_meta.h defines the field only, no value table)
  source: https://raw.githubusercontent.com/FFmpeg/FFmpeg/master/libavutil/dovi_meta.h

- field: HDR10+ payload fields
  where: hdr10plus_tool hdr10plus/src/metadata.rs (Hdr10PlusMetadata::parse)
  parse: "header: country u8, provider u16, provider_oriented u16, app_id u8, app_version u8, num_windows u2; then processing windows, targeted max lum u32, maxscl[3], average_maxrgb, distribution_maxrgb, bezier_curve, color_saturation_weight u6"
  use: struct field list. parse() does not check T.35 codes.
  source: https://raw.githubusercontent.com/quietvoid/hdr10plus_tool/main/hdr10plus/src/metadata.rs ; hdr10plus/src/lib.rs

- field: HDR10+ detection (T.35 in SEI)
  where: validate() in metadata.rs; SEI constants used from hevc crate (hdr10plus/src/hevc/mod.rs)
  parse: "validate(): application_identifier==4 and application_version==1 (SMPTE ST-2094 app 4 v1). Payload type 4 = USER_DATA_REGISTERED_ITU_T_35 and prefix SEI NAL: names from the hevc crate, values UNVERIFIED. T.35 constants 0xB5 / 0x003C / 0x0001: UNVERIFIED (the detection function is not in fetched files)"
  use: check app id 4 / version 1 before trusting a parse; treat the country and provider constants as unconfirmed.
  source: https://raw.githubusercontent.com/quietvoid/hdr10plus_tool/main/hdr10plus/src/hevc/mod.rs

- field: HDR10+ profile A vs B
  where: metadata.rs set_profile()
  parse: "A if tone_mapping_flag==0 AND targeted_system_display_maximum_luminance==0. B if tone_mapping_flag==1 AND targeted max luminance>0 AND bezier_curve anchors>0. else N/A"
  use: profile label for Atlas.
  source: https://raw.githubusercontent.com/quietvoid/hdr10plus_tool/main/hdr10plus/src/metadata.rs

- field: Profile 7 FEL single-track vs two-track
  where: dovi_tool README
  parse: "README: demux handles single track dual layer DV (BL+EL+RPU) in one track; extract-rpu accepts MKV. EL NALs share the track, separated by nuh_layer_id (ffmpeg hevcdec.c skips layers not active)"
  use: EL = NALs with nuh_layer_id>0 (((b0&1)<<5)|(b1>>3), HEVC spec). RPU = NAL 62 (UNSPEC62). Two-track (BlockAdditions) layout UNVERIFIED.
  source: https://raw.githubusercontent.com/quietvoid/dovi_tool/main/README.md ; libavcodec/hevc/hevcdec.c
```

## Leads
- cmv40.rs DM_DATA_PAYLOAD2 header; L11 decode_byte1.
- DV level table: Dolby Profiles and Levels spec (not in repos read).
- HDR10+ T.35 constants: likely hevc_parser crate (not read).
- dovi_tool demuxer.rs / rpu_extractor.rs for profile 7 (not read).

## Dead ends
- docs.rs: 403 from proxy. Use static.crates.io. cdn.jsdelivr gh: refused.
- raw.githubusercontent 404: dolby_vision repo src/*, dovi_tool commands.rs, hdr10plus hevc.rs, hevc/parser.rs, ffmpeg hevc_sei.c.
- FFmpeg libavcodec/hevc/sei.c and h2645_sei.c: no HDR10+ T.35 constants.
