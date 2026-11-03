# w03-c-sei-extra: per-stream SEI/VUI/OBU flags beyond the covered set
Sources: FFmpeg master (raw.githubusercontent.com/FFmpeg/FFmpeg/master/...), crate tarballs (static.crates.io). UNVERIFIED = not confirmed in a source read here.

```yaml
- field: hdr10plus
  where: "HEVC/AVC SEI payloadType 4 (T.35). AV1 variant has no country byte."
  parse: "country u8 0xB5; provider u16 0x003C; provider_oriented u16 0x0001; application_identifier u8 == 4; application_version u8 follows (UNVERIFIED: FFmpeg does not check it)"
  use: "HDR10+"
  source: "libavcodec/itut35.c L86-94, L199-215; libavcodec/itut35.h L30,L47 (SAMSUNG 0x003C); crate hdr10plus-2.1.5 src/metadata.rs L94,L116-123,L192; src/metadata_json.rs L321-324"

- field: closed captions (A/53 cc_data, CEA-608/708)
  where: "SEI payloadType 4; country 0xB5; provider 0x0031; then be32 'GA94'; then user_data_type_code 0x03"
  parse: "byte1 bit 0x40 (process_cc_data_flag) must be 1; cc_count = byte1 & 0x1F, must be > 0; byte2 reserved; cc_count*3 bytes follow. Presence of cc_count>0 in first frames = has CC. 608 vs 708 split: UNVERIFIED (cc_type bits not read here)."
  use: "CC"
  source: "libavcodec/itut35.c L61-76, L180-190 (ATSC/GA94); libavcodec/atsc_a53.c ff_parse_a53_cc (type 0x3, flag 0x40, cc_count)"

- field: AFD (active format description)
  where: "SEI payloadType 4; country 0xB5; provider 0x0031; then be32 'DTG1'"
  parse: "byte after DTG1: bit 0x40 active_format_flag must be 1; AFD code in the low bits (UNVERIFIED layout)"
  use: "AFD (letterbox hint)"
  source: "libavcodec/itut35.c L66-72, L180-187"

- field: mastering display colour volume (static HDR10)
  where: "SEI payloadType 137"
  parse: "display primaries, white point, max/min luminance"
  use: "HDR10 MDCV"
  source: "libavcodec/sei.h L96; libavcodec/h2645_sei.c L310"

- field: content light level (MaxCLL/MaxFALL)
  where: "SEI payloadType 144"
  parse: "max_content_light_level u16, max_pic_average_light_level u16 (spec, UNVERIFIED body layout here)"
  use: "HDR10 CLL"
  source: "libavcodec/sei.h L103; libavcodec/h2645_sei.c L313"

- field: alternative transfer characteristics (HLG compatibility)
  where: "SEI payloadType 147"
  parse: "preferred_transfer_characteristics u8; value 18 = HLG (spec, UNVERIFIED here)"
  use: "HLG"
  source: "libavcodec/sei.h L106; libavcodec/h2645_sei.c L305"

- field: 3D frame packing
  where: "SEI payloadType 45 frame_packing_arrangement"
  parse: "frame_packing_arrangement_type from the SEI body (spec order, UNVERIFIED here). Values: 0 checkerboard, 1 column interleave, 2 row interleave, 3 side-by-side, 4 top-bottom, 5 temporal interleave, 6 2D"
  use: "3D SBS (type 3), 3D TAB (type 4), 3D (other types)"
  source: "libavcodec/sei.h L75,L148-154; libavcodec/h264_sei.c L308-338; libavcodec/h2645_sei.c L303, L540-563"

- field: MVC 3D (AVC base + dependent view)
  where: "AVC NAL 15 (subset SPS), NAL 20 (coded slice extension); NAL 14 (prefix) also in the stream"
  parse: "any NAL 15 or 20 in the first chunk => dependent view present"
  use: "3D MVC"
  source: "libavcodec/h264dec.c L806-824 (13 SPS_EXT and 19 AUX are ignored; 14/15/20 fall to default). NAL numbers: H.264 Table 7-1 (UNVERIFIED here)"

- field: MV-HEVC (multiview)
  where: "HEVC NAL with nuh_layer_id > 0; VPS extension"
  parse: "nuh_layer_id = ((byte0 & 1) << 5) | (byte1 >> 3) from the 2-byte NAL header (UNVERIFIED: spec H.265 7.3.1.2). VPS vps_nuh_layer_id_present_flag path: libavcodec/hevc/ps.c L580-591"
  use: "MV-HEVC / 3D"
  source: "libavcodec/hevc/ps.c L580-591"

- field: AVC interlace (MBAFF / PAFF)
  where: "AVC SPS: frame_mbs_only_flag u1; if 0 then mb_adaptive_frame_field_flag u1. Per-slice field_pic_flag (UNVERIFIED: location not read)."
  parse: "frame_mbs_only=1 => progressive. frame_mbs_only=0 and mb_adaptive=1 => MBAFF. frame_mbs_only=0 and mb_adaptive=0 => PAFF (per-picture field_pic_flag decides)."
  use: "interlaced (MBAFF) / interlaced (PAFF)"
  source: "libavcodec/h264_ps.c L451-462, L565 (log strings FRM / MB-AFF / PIC-AFF)"

- field: HEVC interlace / pulldown
  where: "HEVC SPS VUI: field_seq_flag, frame_field_info_present_flag; pic_timing SEI payloadType 1 pic_struct u(4)"
  parse: "field_seq_flag=1 => field-coded sequence. pic_struct read only when frame_field_info_present_flag. pic_struct 1/9/11 top field, 2/10/12 bottom field, 7 frame doubling, 8 frame tripling."
  use: "interlaced (field_seq_flag or pic_struct 1-2/9-12); pulldown (pic_struct 7/8, inference only, UNVERIFIED as telecine proof)"
  source: "libavcodec/hevc/ps.c L991-992; libavcodec/hevc/sei.c L60-75; libavcodec/h264_sei.c L77-86 (AVC pic_struct, max FRAME_TRIPLING)"

- field: MPEG-2 progressive / field
  where: "MPEG-2 sequence extension progressive_sequence u1; picture coding extension (repeat_first_field, top_field_first: UNVERIFIED read location)"
  parse: "progressive_sequence=0 => interlaced sequence. repeat_first_field=1 => soft 3:2 pulldown (UNVERIFIED)."
  use: "interlaced / telecine (soft)"
  source: "libavcodec/mpeg12dec.c L1038, L1104 (progressive_sequence); L687-688 (top_field_first usage)"

- field: AV1 film grain
  where: "AV1 sequence header film_grain_params_present u1; per-frame film_grain_params apply_grain u1"
  parse: "seq film_grain_params_present=1 AND frame apply_grain=1 => grain synthesis"
  use: "film grain"
  source: "crate cros-codecs-0.0.6 src/codec/av1/parser.rs L2173 (seq flag), L2214 (frame); libavcodec/av1dec.c L396, L1080 (apply_grain check)"

- field: film grain SEI (H.264/HEVC)
  where: "SEI payloadType 19 film_grain_characteristics"
  parse: "presence of the SEI => grain characteristics present (params body UNVERIFIED here)"
  use: "film grain"
  source: "libavcodec/sei.h L49; libavcodec/h2645_sei.c L297"

- field: Dolby Vision carried in T.35 (non-RPU)
  where: "SEI payloadType 4; country 0xB5; provider 0x003B; be32 provider_oriented 0x00000800"
  parse: "provider_oriented == 0x800 (be32)"
  use: "DV (SEI-carried; RPU NAL is covered elsewhere)"
  source: "libavcodec/itut35.c L95-100; libavcodec/itut35.h L45"
```

## Other encoders (item 6, source-level only, runtime UNVERIFIED)
- VideoToolbox: writes SEI type 4 (T.35) from an encoder-side ExtraSEI passthrough, not a fixed UUID. libavcodec/videotoolboxenc.c L250-253, L2168-2229.
- Intel QSV: SDK options PicTimingSEI, RecoveryPointSEI, SingleSeiNalUnit; no UUID in the wrapper. libavcodec/qsvenc.c L268-276, L961-984.
- AMD AMF: no SEI writing found in the wrapper. libavcodec/amfenc.c (grep for sei/uuid empty).

## Crates (latest versions read from index.crates.io; crates.io JSON API returned empty)
- hevc_parser 0.6.12, MIT. src/hevc/sei.rs: generic SEI header walk only (payload_type, payload_offset, payload_size). No T.35/A53/HDR decode. Caution: payload_type is u8 and is incremented with `+= 255` per 0xFF byte (UNVERIFIED whether this can overflow-panic in debug).
- hdr10plus 2.1.5, MIT. src/metadata.rs parse(data) starts at the country byte; validates application_identifier == 4 (L192). src/hevc/mod.rs is encode-only (writes a NAL).
- h264-reader 0.9.0, MIT/Apache-2.0. src/nal/sei/mod.rs maps SEI ids 0-188 to HeaderType. Bodies decoded: buffering_period, pic_timing, recovery_point, user_data_registered_itu_t_t35 (country codes incl. 0xB5 UnitedStates, L408), user_data_unregistered. No A/53 cc_data, no HDR10+, no mastering/CLL decoders.

## leads
- quietvoid/hdr10plus_tool is the reference for full HDR10+ field parsing (GitHub API blocked here; crate read instead).
- ITU-T T.35 provider 0x0031 may carry non-GA94 payloads; only GA94 was checked.
- MV-HEVC: check libavcodec/hevc/ for view_id handling (FFmpeg 7.1+ MV-HEVC) to confirm the VPS extension layout.

## dead ends
- GitHub API for quietvoid/hevc_parser: blocked ("GitHub access not enabled"). Use the crate tarball.
- crates.io JSON API: empty response. Use index.crates.io sparse index plus static.crates.io tarball.
- h264-reader: no A/53 parse at all (grep GA94/cc_data/user_data_type in src is empty).
- hdr10plus crate HEVC module: encode-only, not a parser.
