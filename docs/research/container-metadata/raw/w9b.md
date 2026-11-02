# w9b: gap analysis, MediaInfo / ffprobe vs catalogs (mkv, mp4, disc_other)

Tools: MediaInfoLib v24.01 (`mediainfo --Info-Parameters`, 1799 lines, saved to scratchpad `mi_params.txt`); ffprobe on /tmp/w5a/corpus (9 MKV/MP4 files; keys saved to scratchpad `ffprobe_keys.txt`). Catalog ids were pulled from the `{id, path}` pairs in mkv.yaml (259), mp4.yaml (254), disc_other.yaml (292). Coverage checks below are by name match and are rough.

Source key: MI = MediaInfo parameter, FF = ffprobe -show_* key, "mapped" = already covered by an existing id (no new id needed).

## 1. Missing fields

| Source | Field | Container | Where to get it | Suggested catalog id |
|---|---|---|---|---|
| FF | streams.disposition.{karaoke, lyrics, captions, clean_effects, dependent, non_diegetic, still_image, timed_thumbnails, dub, metadata} | MKV, MP4 | MKV: only FlagOriginal/FlagCommentary/FlagHearingImpaired/FlagVisualImpaired/FlagTextDescriptions exist in TrackEntry. MP4: derived from track name/kind, not a flag. Most are ffmpeg-derived, not stored | derived.disposition_* (new group). Map FlagCommentary to track.flag_commentary, FlagOriginal to track.flag_original (mapped) |
| MI | Format_AdditionalFeatures (e.g. JOC, XLL) | MP4 (dec3, ddts, dac4), MKV via codec bitstream | dec3 joc_flag (catalog has mp4.dec3.joc_flag); ddts StreamConstruction; dac4 DSI | derived.audio_additional_features (new). Feed from mp4.dec3.joc_flag and mp4.ddts.stream_construction |
| MI | Format_Commercial_IfAny | all audio | MI derives it from codec ID plus features. Verified on corpus b_truehd.mkv: "Dolby TrueHD" | derived.audio_commercial_name (new) |
| MI | HDR_Format_Commercial, HDR_Format_Compatibility, HDR_Format_Settings, HDR_Format_Level | MP4 dvcC/dvvC, MKV BlockAdditionMapping (dvcC/dvvC) | DV record fields. Catalog has derived.dv_label and dvcC bl_signal_compatibility_id, so the label is present but not the compatibility name | derived.dv_format_compat (new). Source: mp4.dvcC.bl_signal_compatibility_id, mkv.block_addition.id_type |
| MI | HDR_Format for HDR10+ ("SMPTE ST 2094 App 4") | MKV (BlockAdditional type 4), bitstream SEI in MP4 | see section 2. The catalog has no explicit HDR10+ id in mkv.yaml or mp4.yaml (grep found none) | derived.hdr10plus_label (new) |
| MI | Delay_Source, Duration_Start, Duration_End (per-track start offset) | MP4 elst, MKV Cluster timestamp and track start | mp4.elst.media_time exists; MKV per-track first block timecode is not mapped | derived.track_start_offset (new) |
| MI | Compression_Mode, Compression_Ratio | all | derived from bitrate, channels, depth, not stored | derived.compression_ratio (new, low priority) |
| MI | Track/Position, Track/Sort, Original/Track | MKV, MP4 | track.number exists; total count and sort order are derived | derived.track_position (new, low priority) |
| MI | Performer, Composer, Album_Sort | MKV SimpleTag (ARTIST, COMPOSER, SORT_ALBUM), MP4 ilst (©ART, ©wrt) | mkv.tag.* lists title, credits, encoder, etc. but no ARTIST, COMPOSER or SORT_* ids. tags.simple.name covers them only generically | mkv.tag.performer, mkv.tag.composer, mkv.tag.sort_album (new). mp4.ilst.art_performer, mp4.ilst.composer (verify ©wrt mapping) |
| FF | streams.tags.VENDOR_ID | MP4 | sample entry vendor field in stsd | mp4.stsd.vendor_id (new; confirm byte offset) |
| FF | streams.codec_tag_string | MP4, MKV | fourcc of stsd entry (mkv: CodecID) | mapped per fourcc (mp4.stsd.*). Add a generic mp4.stsd.fourcc for the raw string |
| FF | chapters.chapter.N.time_base | MKV, MP4 | derived from timescale | derived.chapter_time_base (low priority) |
| FF/MI | streams.closed_captions, streams.film_grain, side_data_list (Audio Service Type) | bitstream | SEI, AAC/AC3 bsmod | streams (sibling thread) |
| FF | format.probe_score | n/a | tool-only, not container data | none (skip) |
| MI | ServiceName, ServiceProvider, ServiceChannel | TS | SDT | mapped to ts.sdt.service_descriptor_0x48 |
| MI | IsStreamable | MP4 | faststart layout | mapped to derived.mp4.faststart |
| MI | Encoded_Library_Settings | MKV, MP4 | mkv.tag.encoder_settings; mp4 ilst ©too | mapped |
| FF | streams.initial_padding | MP4, MKV | elst, dOps pre_skip, CodecDelay | mapped to derived.mp4.aac_priming_offset and track.codec_delay |
| FF | format.tags.ENCODER_SETTINGS, format.tags.IMDB, TMDB | MKV, MP4 | mkv.tag.encoder_settings, mkv.tag.imdb/tmdb; mp4 iTunMOVI | mapped |

Not container-level (send to the streams thread): HDR10+ SEI payload details, DV RPU internals, DTS:X/Atmos bitstream extension parsing, codec profile/level, closed captions, film grain.

## 2. HDR10+ at container level (Matroska)

Verified in FFmpeg source (raw.githubusercontent.com/FFmpeg/FFmpeg master, libavformat/matroska.h and matroskadec.c):

- `MATROSKA_BLOCK_ADD_ID_TYPE_ITU_T_T35 = 4` (matroska.h:363; `MATROSKA_BLOCK_ADD_ID_ITU_T_T35 4` at line 370). This is a **BlockAddIDType**, not a BlockAddID. The per-frame BlockAddID is chosen by the muxer; the type says how to read the payload.
- The header-level signal exists. A TrackEntry with a BlockAdditionMapping element whose BlockAddIDType = 4 declares that track carries T.35 metadata. Spec: ebml_matroska.xml BlockAddIDType (0x41E7, uinteger, minver 4, default 0). BlockAddIDValue (0x41F0) must be >= 2 when type is not 0. BlockAddIDExtraData (0x41ED) is optional for type 4 (mkvdec.c:2545 requires >= 4 bytes only when there are multiple type-4 mappings).
- Answer to "is there a header-level signal that a track carries HDR10+": **yes**. Track header, BlockAdditionMapping with BlockAddIDType = 4. Not a yes/no for HDR10+ specifically. Type 4 is generic T.35, so the HDR10+ identity comes from the payload (country 0xB5 US, provider 0x003C Samsung), not the header. FFmpeg mkvdec.c:4055-4090 decodes that payload to AV_PKT_DATA_DYNAMIC_HDR10_PLUS.
- Payload per FFmpeg: country byte, then provider 16-bit BE. US (0xB5) + Samsung (0x003C) = HDR10+ (ST 2094-40). US + SMPTE (0x0090) = ST 2094 App 5 (mkvdec.c:4095). UK (0xB4) + VNova (0x5000) = LCEVC.
- Codec-agnostic: the FFmpeg code attaches the side data to any packet, so it is not VP9/AV1-specific. The BlockAdditional route applies to any Matroska video track that declares the mapping. (Memory, not checked: AV1 in MKV usually carries HDR10+ in-band as T.35 OBU metadata, not as BlockAdditional. Mark as unverified.)
- mkvmerge: not checked (no mkvinfo on the box; the grep for BlockAdd in a_hdr10_mkvmerge.mkv returned nothing, so either no mapping exists or the tool was missing). Do not state mkvmerge's BlockAddID value as 4 without testing.
- Other mapping types in the same FFmpeg switch (mkvdec.c:2578-2584): dvcC 0x64766343, dvvC 0x64767643, hvcE 0x68766345. Catalog already has dv_label and codec_config_hvcc, so those are covered.
- MP4: no BlockAddition equivalent. HDR10+ in MP4 is bitstream-only (SEI in the HEVC/AV1 track). Catalog gap: ts.video.hdr10plus_sei exists for TS only, so add mp4.video.hdr10plus_sei_present as a derived flag (memory-level claim, not tested in corpus).

Catalog action: add derived.hdr10plus_label that reads BlockAdditionMapping type 4 plus the T.35 country and provider bytes from the BlockAdditional payload, with values "hdr10plus", "st2094_app5", "lcevc", or none. Keep mkv.block_addition.id_type but add a meaning entry for 4.

## 3. Atmos / DTS:X / IMAX Enhanced

| Format | Container signal | Verdict | Notes |
|---|---|---|---|
| Dolby Atmos (E-AC-3 JOC) | MP4 dec3 extension: flag_ec3_extension_type_a and complexity_index_type_a (catalog mp4.dec3.joc_flag) | container-level, yes | MKV A_EAC3: no CodecPrivate flag, so bitstream only (streams) |
| Dolby Atmos (TrueHD) | none in MKV CodecID A_TRUEHD or MP4 mlpa. Atmos is in the MLP substream extension | bitstream only (streams) | MI shows "Dolby TrueHD" for b_truehd.mkv (no Atmos in that file, so no test of Atmos naming) |
| Dolby AC-4 (IMS / immersive) | MP4 dac4 ac4_dsi_v1 | container-level, partial | DSI has presentation and IMS-related bits. The exact IMS bit is not verified here. Check TS 103 190-2 before coding |
| DTS:X | MP4 ddts StreamConstruction and ChannelLayout (catalog mp4.ddts.*); stsd fourcc dtsx/dtsh/dtsc | partial (memory) | ddts does not name DTS:X. DTS:X tiers are signalled in the stream extension (bitstream) |
| DTS-HD MA / HR | MKV A_DTS; MP4 stsd dtsh/dtsl/dtsx | container hint only | mapped |
| IMAX Enhanced | none in MKV or MP4 | not container-level | Disc-level only, possibly bdmv.index.ext_3_1_uhd_flags (unverified bit). Otherwise inferred from DV or HDR10+ plus DTS:X |

MediaInfo naming: Format_Commercial_IfAny gives "Dolby TrueHD" for TrueHD (verified on corpus). For E-AC-3 with JOC, MI usually sets Format_AdditionalFeatures to "JOC" and the commercial name to "Dolby Digital Plus with Dolby Atmos" (memory, not tested). For DTS:X, MI uses Format_Commercial_IfAny "DTS:X" from the bitstream (memory). Recommend a derived.audio_atmos_flag that is true when mp4.dec3.joc_flag is set, and a bitstream-derived check for TrueHD and DTS:X under the streams thread.

## Limits of this run
- Corpus has no Atmos, DTS:X, DV, or HDR10+ streams, so no positive tests of these signals. Everything marked "memory" is unverified.
- mkvinfo not available, so no BlockAdditionMapping check on mkvmerge output.
- Time limit reached after the FFmpeg and spec checks; disc_other was not mapped line by line beyond the ffprobe/MI name overlap.
