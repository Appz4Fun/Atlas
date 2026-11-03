# W02-A encoder and mux fingerprints (source-verified where marked)
Sources read: x264 mirror (github.com/mirror/x264, raw.githubusercontent.com); x265 (raw.githubusercontent.com/videolan/x265/master/source); FFmpeg (raw.githubusercontent.com/FFmpeg/FFmpeg/master/libavcodec, libavformat); libebml/libmatroska headers (raw.githubusercontent.com/Matroska-Org). code.videolan.org and gitlab.com returned 403 via proxy; mkvtoolnix GitHub mirror paths 404.

```yaml
- field: x264_sei_uuid
  where: H.264 SEI NAL, payloadType user_data_unregistered (5), first 16 bytes of payload
  parse: bytes DC 45 E9 BD E6 D9 48 B7 96 2C D8 20 D9 23 EE EF
  use: strongest x264 per-encode fingerprint; scan Annex-B NALs in first block / CodecPrivate
  source: raw.githubusercontent.com/mirror/x264/master/encoder/set.c (x264_sei_version_write, L593-615)
- field: x264_sei_string
  where: payload bytes 16 onward, NUL-terminated (length = strlen+1)
  parse: "x264 - core %d%s - H.264/MPEG-4 AVC codec - Copy%s 2003-2024 - http://www.videolan.org/x264.html - options: %s"; %d=X264_BUILD, %s=X264_VERSION (e.g. " r3095 baee400"); Copy%s = "left" if HAVE_GPL else "right"
  use: x264 core build number and build hash; options follow after "options: "
  source: same as above, set.c L610-612 (year literal 2003-2024 is this source version only, UNVERIFIED for older builds)
- field: x264_options_string
  where: text after "options: " in x264_sei_string
  parse: x264_param2string(p, b_res=0) in common/base.c L1428; space-separated key=value; SEI omits width/height/fps (b_res=0). Keys include cabac ref deblock analyse me subme psy psy_rd trellis 8x8dct cqm threads lookahead_threads sliced_threads slices nr decimate interlaced bframes b_pyramid b_adapt b_bias direct weightb open_gop weightp keyint keyint_min scenecut intra_refresh rc_lookahead rc= mbtree crf= bitrate= ratetol= qcomp qpmin qpmax qpstep vbv_maxrate vbv_bufsize nal_hrd filler crop_rect mastering-display= cll= ip_ratio aq
  use: encoder settings fingerprint (preset/tune class), rate-control mode (rc=crf|abr|cbr|2pass|cqp) and target bitrate or CRF, HDR mastering values if present
  source: raw.githubusercontent.com/mirror/x264/master/common/base.c L1426-1500 and L1500-1560
- field: x264_sei_emit_rule
  where: NAL SEI in access units
  parse: once in headers (encoder.c L2112-2119, x264_encoder_headers); repeated only if b_repeat_headers and fenc->i_frame==0 (encoder.c L3730-3736), i.e. first frame only, not every IDR
  use: search first frame / first block only; not present on later keyframes unless headers are re-sent
  source: raw.githubusercontent.com/mirror/x264/master/encoder/encoder.c
- field: x265_sei_uuid
  where: HEVC prefix SEI NAL, payloadType 5, first 16 bytes
  parse: bytes 2C A2 DE 09 B5 17 47 DB BB 55 A4 FE 7F C2 FC 4E (string 2CA2DE09-B517-47DB-BB55-A4FE7FC2FC4E)
  use: x265 fingerprint, HEVC
  source: raw.githubusercontent.com/videolan/x265/master/source/encoder/sei.cpp L32-35
- field: x265_sei_string
  where: payload after UUID, SEIuserDataUnregistered
  parse: "x265 (build %d) - %s:%s - H.265/HEVC codec - Copyright 2013-2018 (c) Multicoreware, Inc - http://x265.org - options: %s" with X265_BUILD, version_str, build_info_str (e.g. "3.5+...") and opts=x265_param2string()
  use: x265 build version plus full settings string
  source: raw.githubusercontent.com/videolan/x265/master/source/encoder/encoder.cpp L3308-3324 (gated by bEmitInfoSEI)
- field: x265_sei_emit_rule
  where: getStreamHeaders (encoder.cpp L3258)
  parse: called from x265_encoder_headers (encoder/api.cpp L284) at stream start; and from frameencoder.cpp L478-496 on EVERY keyframe only when repeat-headers is on. --info (param.cpp L149, default 1, option L1042) controls the SEI
  use: default x265: one occurrence in headers; with repeat-headers: one per keyframe
  source: raw.githubusercontent.com/videolan/x265/master/source/encoder/frameencoder.cpp
- field: av1_t35_metadata_obu
  where: AV1 OBU_METADATA (type 5), metadata_type ITU-T T.35
  parse: T.35 country 0xB5 (US); provider 0x003C = Samsung (HDR10+); provider 0x0090 = SMPTE (2094 app5); Dolby DV RPU wrapped T.35 uses provider 0x003B (Dolby, itut35.h L45)
  use: AV1 HDR10+ and Dolby Vision signalling detection without HEVC SEI
  source: raw.githubusercontent.com/FFmpeg/FFmpeg/master/libavcodec/libaomenc.c L355-368, L402-413, L1369-1374; itut35.h L30-48
- field: mp4_ctoo_atom
  where: moov/udta/meta/ilst, atom "\xa9too" (encoding tool)
  parse: UTF-8 string; default LIBAVFORMAT_IDENT (e.g. "Lavf<ver>") unless metadata set
  use: muxer fingerprint for MP4
  source: raw.githubusercontent.com/FFmpeg/FFmpeg/master/libavformat/movenc.c L4970-4972
- field: mp4_btrt_box
  where: sample entry child box "btrt"
  parse: written by mov_write_btrt_tag (movenc.c L1351); only when write_btrt enabled (auto = some encoders); fields bufferSizeDB, maxBitrate, avgBitrate (u32 BE each, from ISO 14496-12 memory, UNVERIFIED for layout)
  use: declared bitrate without decoding frames
  source: movenc.c L1351-1361, L1624-1625
- field: mkv_segment_ids
  where: EBML Segment > SegmentInfo (0x1549A966)
  parse: MuxingApp 0x4D80 (UTF-8), WritingApp 0x5741 (UTF-8), DateUTC 0x4461 (int64 ns since 2001-01-01), SegmentUUID 0x73A4 (16 bytes, random per mux), Title 0x7BA9, Duration 0x4489 (float). Tags 0x1254C367, Tag 0x7373, Targets 0x63C0, SimpleTag 0x67C8, TagName 0x45A3, TagString 0x4487. SeekHead 0x114D9B74
  use: MuxingApp/WritingApp give libebml/libmatroska or mkvmerge version; SegmentUUID is a per-mux twin key
  source: IDs from memory, UNVERIFIED (libmatroska KaxDefines.h fetched but has no hex IDs; matroska.org not reachable)
- field: mkvmerge_stats_tags
  where: EBML Tags element, SimpleTags per track (TagTrackUID 0x63C5)
  parse: names _STATISTICS_TAGS, BPS, DURATION, NUMBER_OF_FRAMES, NUMBER_OF_BYTES, _STATISTICS_WRITING_APP, _STATISTICS_WRITING_DATE_UTC
  use: bitrate, frame count and byte count per track, mkvmerge writing app and date
  source: UNVERIFIED (mkvtoolnix source not retrievable)
```

## Leads
- Is the x264 SEI kept in MKV block data or dropped into CodecPrivate by muxers? Check a real sample.
- Whether mkvmerge Tags sit at file end (SeekHead 0x114D9B74 at front would point there; then a tail range fetch works). UNVERIFIED.
- HEVC DV RPU T.35 wrapper (provider 0x003B, country 0xB5) in the HEVC path: not checked in ffmpeg hevc code.

## Dead ends
- NVENC (ffmpeg nvenc.c): no vendor UUID SEI. Writes only standard recovery-point and picture-timing SEIs (nvenc.c L1203-1228) and passes frame side-data SEIs such as A53 CC (L2770). Not a fingerprint.
- libaom version string (libaomenc.c L1018) is only logged, never written to the bitstream.
- SVT-AV1 (libsvtav1.c, ffmpeg wrapper): no bitstream identity string found. EbSvtAv1Metadata.h not fetched: UNVERIFIED.
- HandBrake, Apple/AVFoundation, Netflix/Amazon vendor SEI UUIDs: UNVERIFIED, not in fetched source.
