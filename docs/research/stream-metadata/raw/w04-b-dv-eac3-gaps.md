# W04-B: DV levels, DV compat IDs, E-AC-3 / AC-3 / DTS box layouts (research)
Sources read this run are listed per item. Anything not read is marked UNVERIFIED.

```yaml
- field: dv_level (item 1, level -> max resolution@fps table)
  where: dvcC/dvvC config record (dv_level), RPU header vdr_rpu_level (u4)
  parse: not parsed for semantics in any source read
  values: UNVERIFIED. No level table found. Example "level 6 = 3840x2160@24" and "level 9 = 3840x2160@60" NOT confirmed.
  use: do not hardcode a level->resolution table until Dolby spec is read.
  source: https://static.crates.io/crates/dolby_vision/dolby_vision-3.4.0.crate (src/rpu/rpu_data_header.rs: vdr_rpu_level field only, no table); https://raw.githubusercontent.com/MediaArea/MediaInfoLib/master/Source/MediaInfo/Multiple/File_Mpeg4_Elements.cpp (dvcC atom at line ~8451 delegates to dvcC(), body not in file); Dolby PDF NOT read (time cap).

- field: profile from RPU header (crate heuristic, not spec-checked)
  where: RPU NAL header, after rpu_type (u6 == 2)
  parse: rpu_format u11, vdr_rpu_profile u4, vdr_rpu_level u4, vdr_seq_info_present_flag u1; when seq_info: chroma_resampling_explicit_filter_flag u1, coefficient_data_type u2, bl/el bit depth if (rpu_format & 0x700)==0, el_spatial_resampling_filter_flag, disable_residual_flag.
  values: get_dovi_profile(): vdr_rpu_profile 0 and bl_video_full_range_flag -> 5; vdr_rpu_profile 1 -> if el_spatial_resampling_filter_flag && !disable_residual_flag then (vdr_bit_depth_minus8==4 ? 7 : 4) else 8; otherwise 0.
  use: usable for 4/5/7/8 detection from RPU alone. 8.1 vs 8.4 is NOT derivable here (crate has profile81/profile84 generators but no parser split). 7 vs 4 heuristic is the crate's, UNVERIFIED against spec.
  source: src/rpu/rpu_data_header.rs lines 61-191 (crate dolby_vision-3.4.0 from static.crates.io); src/rpu/profiles/mod.rs (DoviProfile trait: backwards_compatible() default true, profile5 false).

- field: L11 content metadata (ext block level 11, 4 bytes)
  where: DV ext metadata block, level 11
  parse: byte0 content_type u8; byte1 low nibble whitepoint, bit4 reference_mode_flag; byte2 reserved; byte3 reserved (validate: must be 0).
  values: content_type 0..15 and whitepoint 0..15 are the only constraints the crate enforces. Crate default_cinema uses content_type 1, whitepoint 0, comment "Cinema, D65 whitepoint". Crate XML test fixture uses content_type 2, whitepoint 0 (tests.rs:288-290). Named labels (0 default, 1 movies, 2 games, 3 sport, 4 UGC): UNVERIFIED.
  use: parse the 4 bytes as above; treat content_type labels as UNVERIFIED.
  source: https://static.crates.io/crates/dolby_vision/dolby_vision-3.4.0.crate, src/rpu/extension_metadata/blocks/level11.rs; src/xml/tests.rs.

- field: dv_bl_signal_compatibility_id and profile strings (8.1/8.2/8.4/7.6, dvhe.08.06 "BL+RPU, HDR10 compatible")
  where: dvcC/dvvC box (DOVIDecoderConfigurationRecord)
  values: UNVERIFIED. Not in crate (grep for signal_compat/compatib: no hit). MediaInfo File_DolbyVisionConfig.cpp has field names only (line 1626 "BackwardCompatible", V_Empty). 0/1/2/4/6 meanings NOT confirmed.
  use: none yet.
  source: https://raw.githubusercontent.com/MediaArea/MediaInfoLib/master/Source/MediaInfo/Video/File_DolbyVisionConfig.cpp (grep only; dvcC() body not located).

- field: dec3 EAC3SpecificBox bit layout (MP4 ec-3 / dec3)
  where: MP4 stsd > ec-3 > dec3 payload (atom ID 0x64656333)
  parse: bits from payload start: data_rate 13 (kbps, per ETSI; MediaInfo skips it); num_ind_sub 3; then per independent substream (num_ind_sub+1 times): fscod 2, bsid 5, reserved 1, asvc 1, bsmod 3, acmod 3, lfeon 1, reserved 3, num_dep_sub 4, then chan_loc 9 if num_dep_sub>0 else reserved 1. Each substream = 24 bits (no dep) or 32 bits (dep). After loop, if bits remain: reserved 7, flag_ec3_extension_type_joc 1, if set joc_complexity_index 8. Remainder skipped.
  values: as parsed above. CORRECTION to the brief: the optional tail is 7 reserved + 1 flag (flag_ec3_extension_type_joc) + 8 joc_complexity_index, NOT "flag_ec3_extension_type_a 8 + complexity_index_type_a 8".
  use: channel layout from acmod (3 bits) + lfeon; bsmod; JOC complexity if present. Bitrate: data_rate kbps (ETSI unit UNVERIFIED, not read).
  source: https://raw.githubusercontent.com/MediaArea/MediaInfoLib/master/Source/MediaInfo/Audio/File_Ac3.cpp lines 4722-4767 (File_Ac3::dec3); dec3 atom dispatch in File_Mpeg4_Elements.cpp lines 8143-8170 (hands payload to File_Ac3 with MustParse_dec3). ETSI TS 102 366 PDF NOT read.

- field: ffmpeg dec3 parse (cross-check and bug note)
  where: mov.c mov_read_dec3
  parse: eac3info = AV_RB24(extradata + 2); bsmod = (eac3info >> 12) & 0x1f; acmod = (eac3info >> 9) & 0x7; lfeon = (eac3info >> 8) & 0x1.
  values: bit positions agree with MediaInfo for substream 0. BUG NOTE: mask 0x1f on a 3-bit bsmod also picks up bit 16 (reserved) and bit 15 (asvc). Harmless when those are 0. Correct mask is 0x7. Only first substream is read.
  use: copy the MediaInfo layout, not the mask.
  source: https://raw.githubusercontent.com/FFmpeg/FFmpeg/master/libavformat/mov.c lines 1129-1170.

- field: dac3 (AC-3 AC3SpecificBox, F.4) and ddts (DTS)
  where: MP4 ac-3/dac3, dts/ddts (atom IDs confirmed in source)
  parse dac3: UNVERIFIED (from memory: fscod 2, bsid 5, bsmod 3, acmod 3, lfeon 1, bit_rate_code 5, reserved 5 = 24 bits). Not read in this run: MediaInfo File_Ac3::dac3 exists (File_Ac3.cpp, MustParse_dac3 path) and FFmpeg mov_read_dac3 at mov.c:871.
  parse ddts (partial, read): DTSSamplingFrequency u32, maxBitrate u32, avgBitrate u32, ChannelLayout u16, then CoreLayout u8 (not verified further).
  source: https://raw.githubusercontent.com/MediaArea/MediaInfoLib/master/Source/MediaInfo/Multiple/File_Mpeg4_Elements.cpp lines 1036-1043 (atom IDs) and ddts function (~line 8170-8200); https://raw.githubusercontent.com/FFmpeg/FFmpeg/master/libavformat/mov.c lines 10363-10365 (dispatch).

- field: MKV CodecPrivate for A_AC3, A_EAC3, A_TRUEHD, A_DTS
  where: Matroska TrackEntry > CodecPrivate
  values: ffmpeg muxer writes CodecPrivate for these codec IDs ONLY IF extradata is non-empty (default branch: "else if (extradata_size && codec_id != TTA) write extradata"). Raw AC-3/E-AC-3/TrueHD/DTS packets typically carry no extradata, so CodecPrivate is usually empty in ffmpeg-muxed MKVs. Matroska spec statement for these IDs: UNVERIFIED (codec_specs spec not reachable).
  use: do not rely on CodecPrivate for E-AC-3/AC-3/TrueHD/DTS. Parse the first sync frame (syncword 0x0B77) instead.
  source: https://raw.githubusercontent.com/FFmpeg/FFmpeg/master/libavformat/matroskaenc.c lines ~1190-1225 (mkv_write_codecprivate default branch); codec ID table https://raw.githubusercontent.com/FFmpeg/FFmpeg/master/libavformat/matroska.c lines 29-60.
```

## Leads
- ffmpeg libavutil/dovi_meta.h (AVDOVIDecoderConfigurationRecord): should give dvcC field order (dv_profile, dv_level, rpu/el/bl flags, dv_bl_signal_compatibility_id). From memory only; UNVERIFIED.
- MediaInfo dvcC() body: not in fetched files; find via File_Mpeg4.h or File_DolbyVisionConfig.cpp dvcC parser.
- File_DolbyVisionConfig.cpp field list (line ~1600-1630) names the metadata fields.
- Dolby "Dolby Vision Profiles and Levels" PDF (professional.dolby.com): needed for item 1 and item 2 values.
- ETSI TS 102 366 Annex F (dec3, dac3): confirm the bit layout and data_rate unit against the PDF.
- crates.io versions of dolby_vision are 1.0.0 to 3.4.0; 3.4.0 used here.

## Dead ends
- matroska-specification codec_specs.md and codec/*.md: 404 on master and main branches of raw.githubusercontent.com.
- api.github.com contents for matroska-specification: GitHub access not enabled for this session.
- MediaInfo Multiple/File_Matroska.cpp: 404 at that path.
- dolby_vision crate: no dv_level table and no dv_bl_signal_compatibility_id parse.
- Not attempted (time cap): ETSI PDFs, Dolby PDFs, matroska.org (proxy-blocked).
