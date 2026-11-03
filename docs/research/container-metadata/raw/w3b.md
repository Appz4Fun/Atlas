# W3b: Blu-ray / DVD structure layouts from original source

Status key: **VERIFIED-ORIG** = read in the upstream release tarball or upstream repo file (URL and function given). Anything else is marked NOT VERIFIED.

## Sources used

| Source | Location | Result |
|---|---|---|
| libbluray 1.5.0 upstream tarball | http://archive.ubuntu.com/ubuntu/pool/universe/libb/libbluray/libbluray_1.5.0.orig.tar.xz (extracted to scratchpad `w3b/bluray/libbluray-1.5.0/`) | OK. Upstream release tarball, not a fork. |
| libdvdread 7.1.1 upstream tarball | http://archive.ubuntu.com/ubuntu/pool/universe/libd/libdvdread/libdvdread_7.1.1.orig.tar.xz (extracted to `w3b/dvdread/libdvdread-7.1.1/`) | OK. |
| BDInfo (C#) | https://raw.githubusercontent.com/UniqProject/BDInfo/master/BDInfo/BDROM/TSPlaylistFile.cs and TSStreamClipFile.cs | OK (raw fetch, 200). Only partly read. |
| Blocked | code.videolan.org, sources.debian.org, android.googlesource.com (CONNECT 403); github.com/videolan/libbluray (403); `gh api` on ShiftMediaProject/libbluray ("GitHub access not enabled for this session"); raw.githubusercontent for videolan/* and rokibhasansagar/BDInfo (404) | Not usable. |

Binary .debs (libbluray2, libdvdread8t64, libbluray-dev) also downloaded via apt. They contain no source.

Paths below are relative to `src/libbluray/bdnav/` (libbluray) or `src/` (libdvdread).

## 1. MPLS (PlayList, `PLAYLIST/*.mpls`) — mpls_parse.c

**VERIFIED-ORIG** (`_mpls_parse`, `_parse_header`, `_parse_appinfo`, `_parse_stream`, `_parse_stn`, `_parse_playitem`, `_parse_subplayitem`, `_parse_subpath`, `_parse_playlistmark`, `_parse_playlist`, `_parse_mpls_extension`, `_parse_static_metadata(_extension)`)

Header (file offsets, big-endian):
- 0: "MPLS" (`MPLS_SIG1`), 4: version (`type_indicator2`)
- 8: u32 list_pos (PlayList), 12: u32 mark_pos (PlayListMark), 16: u32 ext_pos (extension data)
- 20..39: 160 bits reserved
- 40: AppInfoPlayList

AppInfoPlayList (`_parse_appinfo`), starting at 40:
- 40: u32 len (not seeked past)
- 44: reserved
- 45: u8 playback_type
- 46: u16 playback_count (only if playback_type 2 or 3, else reserved)
- 48: 8-byte UO mask (`uo_mask_parse`)
- 56 bit7 random_access_flag, bit6 audio_mix_flag, bit5 lossless_bypass_flag, bit4 mvc_base_view_r_flag, bit3 sdr_conversion_notification_flag (bits 2..0 unread)

PlayList (at list_pos): u32 len, u16 reserved, u16 list_count, u16 sub_count, then PlayItems (u16 len, then clip_id[5]+"M2TS"/"FMTS"[4] at +2..+10, then 11 reserved bits + is_multi_angle (1 bit) + connection_condition (4 bits, values 1/5/6), u8 stc_id, u32 in_time, u32 out_time, 8-byte UO mask, random_access (1 bit)+7 reserved, u8 still_mode, u16 still_time if still_mode==1, if multi-angle: u8 angle_count, 6 reserved, is_different_audio, is_seamless_angle).

PlayListMark (at mark_pos): u32 len, u16 mark_count, then 14-byte entries: u8 reserved, u8 mark_type, u16 play_item_ref, u32 time (45 kHz), u16 entry_es_pid, u32 duration.

SubPath (`_parse_subpath`): u32 len, u8 reserved, u8 sub_path_type, 15 reserved bits + is_repeat (1 bit), u8 reserved, u8 sub_playitem_count, then SubPlayItems.
SubPlayItem (`_parse_subplayitem`): u16 len (min 24), clip_id[5]+codec[4], 27 reserved bits + connection_condition (4 bits) + is_multi_clip (1 bit), u8 stc_id, u32 in_time, u32 out_time, u16 sync_play_item_id, u32 sync_pts, [if multi-clip: u8 clip_count + per-extra-clip clip_id/codec/stc_id].

**STN table (`_parse_stn`), relative to STN start:**
- 0: u16 len, 2: u16 reserved
- 4: u8 num_video, 5: num_audio, 6: num_pg, 7: num_ig, 8: num_secondary_audio, 9: num_secondary_video, 10: num_pip_pg, 11: **num_dv**
- 12..15: 4 reserved
- 16: entries, in order: video[num_video], audio[num_audio], pg[num_pg + num_pip_pg], ig[num_ig], secondary audio (each followed by u8 num_primary_audio_ref, u8 reserved, refs, pad to even), secondary video (each followed by secondary-audio refs, u8 num_pip_pg_ref, pad), **dv[num_dv]** (dolby vision enhancement-layer streams).

**Stream entry (`_parse_stream`)**: u8 len; u8 stream_type; then
- type 1: u16 pid
- type 2: u8 subpath_id, u8 subclip_id, u16 pid
- type 3 or 4: u8 subpath_id, u16 pid
Then u8 len2 and stream coding info:
- u8 coding_type
- 0x01, 0x02, 0xEA, 0x1B, 0x24: byte+1 = format<<4 | rate; if coding_type==0x24 (HEVC): byte+2 = dynamic_range_type<<4 | color_space; byte+3 bit7 = cr_flag, bit6 = hdr_plus_flag
- 0x03, 0x04, 0x80-0x86, 0xA1, 0xA2: format|rate byte, then 3-byte lang
- 0x90, 0x91: 3-byte lang
- 0x92: u8 char_code, 3-byte lang

**MPLS extension data (`bdmv_parse_extension_data`, extdata_parse.c; VERIFIED-ORIG)**: at ext_pos: u32 block length, u32 relative start, 3 padding, u8 num_entries at +11, entries at +12, 12 bytes each: u16 id1, u16 id2, u32 ext_start (relative to block start), u32 ext_len.
Handlers (`_parse_mpls_extension`):
- (1,1) PiP metadata
- (2,2) SubPath entries (`_parse_subpath_extension`: u32 len, u16 sub_count, then SubPaths)
- (3,5) **Static HDR metadata** (`_parse_static_metadata_extension`): u32 len (>=32), u8 sm_count at +4, 3 reserved, then 28-byte entries: 4 bits dynamic_range_type + 4 reserved + 3 reserved bytes; display primaries x/y x3 (u16 each); white point x,y; u16 max_display_mastering_luminance; u16 min_display_mastering_luminance; u16 max_CLL; u16 max_FALL.

**Not in source**: the meaning of dynamic_range_type / color_space values, the DV RPU, and HDR10+ metadata. The libbluray parser does not decode RPU or HDR10+ payloads. Those need the Dolby or ITU specs. NOT VERIFIED.

## 2. CLPI (ClipInfo, `CLIPINF/*.clpi`) — clpi_parse.c

**VERIFIED-ORIG** (`_parse_header`, `_parse_clipinfo`, `_parse_sequence`, `_parse_program`, `_parse_stream_attr`, `_parse_cpi`, `_parse_ep_map_stream`, `_parse_extent_start_points`, `_parse_clpi_extension`)

Header: 0 "HDMV" (`CLPI_SIG1`), 4 version, 8 u32 sequence_info_start, 12 u32 program_info_start, 16 u32 cpi_start, 20 u32 clip_mark_start, 24 u32 ext_data_start.

ClipInfo at 40: u32 len, 2 reserved, 46 u8 clip_stream_type, 47 u8 application_type, 48..51: 31 reserved bits + is_atc_delta (bit 0 of byte 51), 52 u32 ts_recording_rate, 56 u32 num_source_packets, 60..187 reserved (128 bytes), 188 u16 ts_type_info len (if nonzero: u8 validity, 4-byte format_id at 190), then if is_atc_delta: reserved, u8 atc_delta_count, entries of (u32 delta, file_id[5], file_code[4], pad). If application_type==6: font_info.

SequenceInfo: +0 u32 len, +4 reserved, +5 u8 num_atc_seq; each ATC seq: u32 spn_atc_start, u8 num_stc_seq, u8 offset_stc_id; each STC seq: u16 pcr_pid, u32 spn_stc_start, u32 presentation_start_time, u32 presentation_end_time.

ProgramInfo: +0 u32 len, +4 reserved, +5 u8 num_prog; each program: u32 spn_program_sequence_start, u16 program_map_pid, u8 num_streams, u8 num_groups, then streams: u16 pid + StreamAttr.

**StreamAttr (`_parse_stream_attr`)**: u8 len, u8 coding_type, then:
- 0x01, 0x02, 0xEA, 0x1B, 0x20, 0x24 (c = coding_type byte): c+1 = format<<4 | rate; c+2 = aspect(4) | 2 reserved | oc_flag(1) | 1 more bit. For 0x24 that last bit is cr_flag; c+3 = dynamic_range_type<<4 | color_space; c+4 bit7 = hdr_plus_flag, bits 6..0 reserved. Total 32 bits from c+1. For non-HEVC codings, that last bit and the following 17 bits are reserved.
- 0x03, 0x04, 0x80-0x86, 0xA1, 0xA2: format|rate, 3-byte lang
- 0x90, 0x91, 0xA0: 3-byte lang, u8 reserved
- 0x92: u8 char_code, lang
- then isrc[12] (bytes ahead of the seek to pos+len)

Note the CLPI 0x24 layout differs from MPLS 0x24 (CLPI adds aspect/oc_flag and has cr_flag first). Both are VERIFIED-ORIG.

CPI (`_parse_cpi`, at cpi_start): u32 len (0 = none), 12 reserved bits + 4-bit type (bytes 4..5), ep_map_pos = cpi_start+6, u8 reserved at 6, u8 num_stream_pid at 7, then 12-byte entries from 8: u16 pid, 10 reserved bits + 4-bit ep_stream_type, u16 num_ep_coarse, 18-bit num_ep_fine, u32 ep_map_stream_start_addr (relative to ep_map_pos).

EP map stream: at ep_map_start: u32 fine_start (relative), then coarse[num_ep_coarse], each 8 bytes: 18-bit ref_ep_fine_id, 14-bit pts_ep, u32 spn_ep. Fine table at ep_map_start+fine_start: 4-byte entries: 1-bit is_angle_change_point, 3-bit i_end_position_offset, 11-bit pts_ep, 17-bit spn_ep.

Extension data ID2 values (`_parse_clpi_extension`): (1,2) LPCM downmix (not parsed); (2,4) ExtentStartPoints (u32 len, u32 num_point, u32 each); (2,5) ProgramInfo SS; (2,6) CPI SS.

No HDR/DV-specific clip extension is parsed. HDR and DV are only visible through StreamAttr 0x24 fields and the MPLS STN.

## 3. id.bdmv (CERTIFICATE/id.bdmv) — bdid_parse.c and bluray.h

**VERIFIED-ORIG** (`_bdid_parse`, `bdid_get`)
- 0: "BDID" magic, 4: version, 8: u32 data_start, 12: u32 extension_data_start
- 40..43: org_id (4 raw bytes, hex-printed as 8 chars)
- 44..59: disc_id (16 raw bytes, hex-printed as 32 chars)
- Backup: CERTIFICATE/BACKUP/id.bdmv.

Disc ID semantics: the source does not define the meaning of the id.bdmv disc_id.
- `bluray.h` declares `uint8_t disc_id[20]` ("Disc ID"). It is filled in `disc/dec.c` (~line 191) from `bdpriv_aacs_get_aacs_data(BD_AACS_DISC_ID)` with `memcpy(..., 20)`. So the 20-byte libbluray Disc ID is AACS-derived, and it is not the id.bdmv field. VERIFIED-ORIG.
- `bluray.h` also has `udf_volume_id` and `disc_name`. Those come from the BD-J/meta paths.

## 4. index.bdmv — index_parse.c, bdmv_parse.h

**VERIFIED-ORIG** (`_parse_header`, `_parse_app_info`, `_parse_indx_extension_hevc`, `_parse_indx_extension`)

- 0: "INDX" (`INDX_SIG1`), 4: version (`bdmv_parse_header`, which accepts `0100`, `0200`, `0240`, `0300`; BDMV_VERSION_* defines are in bdmv_parse.h lines 28..31), 8: u32 indexes_start, 12: u32 extension_data_start.
- Version `0300` is the UHD marker. `0240` is also accepted.

AppInfoBDMV at 40:
- 40: u32 len (expected 34)
- 44: bit7 skip, bit6 initial_output_mode_preference (0=2D, 1=3D), bit5 content_exist_flag, bit4 skip, bits3..0 initial_dynamic_range_type
- 45: bits7..4 video_format, bits3..0 frame_rate
- 46..77: user_data[32]

Titles: index.bdmv u32 len, then u16 num_titles, and each title is 12 bytes (object_type 2 bits, access_type 2 bits, 28 reserved). Title access_type bit0 = prohibited, bit1 = hidden (index_data.h lines 68..69).

Extension ID 3.1 (`_parse_indx_extension_hevc`, len >= 8): disc_type (4 bits), 3 reserved, exist_4k_flag (1), 8 reserved, 3 reserved, hdrplus_flag (1), 1 reserved, dv_flag (1), hdr_flags (2 bits), 8 reserved, 32 reserved. This is the UHD flag set: 4K, HDR, HDR10+, and DV presence at the disc level.

## 5. META (META/DL/bdmt_*.xml) — meta_parse.c

**VERIFIED-ORIG** (`meta_parse` xmlNode loop, lines ~77..121)
- XML element `name` (di:name) and `alternative` (di:alternative)
- `numSets`, `setNumber`
- `titleName` with attribute `titleNumber`
- `thumbnail` with attribute `href` (path) and `size` (WxH, else -1)

The META/TN thumbnails and title names are read by libbluray only in the element/attribute forms above. No other META fields are parsed.

## 6. libdvdread — dvd_reader.c, dvdread/ifo_types.h, dvd_udf.c

**VERIFIED-ORIG**

### DVDDiscID (dvd_reader.c, line 1904)
- Opens VMG IFO (`ifoOpen(dvd, 0)`) and reads `vmgi_mat->vmg_nr_of_title_sets + 1`, capped to 10.
- For title in 0..title_sets-1: `DVDOpenFile(dvd, title, DVD_READ_INFO_FILE)`. Title 0 is VIDEO_TS.IFO, and title n is VTS_0n_0.IFO.
- Each file is read in full (filesize * 2048 bytes) and fed into the MD5 context, in order, with no separators.
- Output: 16 bytes of MD5 (`memcpy(discid, ctx.buf, 16)`).
- Returns -1 if no IFO could be opened.

### DVDISOVolumeInfo / DVDUDFVolumeInfo (dvd_reader.c 1971, 2033)
- ISO9660 PVD read via `InternalUDFReadBlocksRaw(ctx, 16, 1, ...)` (sector 16).
- volid = bytes 40..71 (32 bytes, `memcpy(volid, &buffer[40], ...)`).
- volsetid = bytes 190..317 (128 bytes, `memcpy(volsetid, &buffer[190], ...)`).
- UDF variant uses `UDFGetVolumeIdentifier` and `UDFGetVolumeSetIdentifier` (dvd_udf.c:989 and nearby).
- Volume set ID is used as a stable disc ID by some players, but libdvdread itself only exposes it. The 16-byte DVDDiscID is the MD5 described above. The source does not say it is the same as any player's ID.

### VMGI (ifo_types.h ~line 439, struct is ATTRIBUTE_PACKED, so offsets are literal)
- 0: vmg_identifier[12]
- 12: u32 vmg_last_sector
- 28: u32 vmgi_last_sector
- 33: u8 specification_version
- 34: u32 vmg_category
- 38: u16 vmg_nr_of_volumes, 40: u16 vmg_this_volume_nr, 42: u8 disc_side
- 62: u16 vmg_nr_of_title_sets
- 64: provider_identifier[32]
- 96: u64 vmg_pos_code

### audio_attr_t (ifo_types.h line 113..141, 8 bytes, packed)
Bitfield declaration order in the struct:
- byte0: audio_format:3, multichannel_extension:1, lang_type:2, application_mode:2, [then byte1] quantization:2, sample_frequency:2, unknown1:1, channels:3
- byte2-3: lang_code (u16), byte4: lang_extension, byte5: code_extension, byte6: unknown3, byte7: app_info union (karaoke or surround, with dolby_encoded bit)

**Caveat**: these are C bitfields. Their bit order is set by the compiler (GCC on little-endian packs from LSB). So the struct order is not the DVD-Video spec bit numbering (MSB-first). Use the struct as a guide to field grouping only. The bit positions must be checked against the DVD-Video spec, which is NOT in source. NOT VERIFIED for exact bit offsets.

## 7. BDInfo (C#) — TSPlaylistFile.cs, TSStreamClipFile.cs (partial)

**VERIFIED-ORIG (partial)**, from UniqProject/BDInfo raw files:
- MPLS header: `playlistOffset`, `chaptersOffset`, `extensionsOffset` read as u32 after the 'MPLS' type and version fields (TSPlaylistFile.cs ~251..253). Order is list, mark, extension, matching libbluray.
- Chapters: `pos = chaptersOffset + 4`, then u16 count. Each entry: data[pos+1] type (1 = chapter), u16 stream file index (+2), u32 time at +4 (45 kHz). Matches the PlayListMark layout above.
- Stream entries and coding type switch: TSPlaylistFile.cs ~623..674 (audio format u8, audio language 3 bytes, graphics language 3 bytes, text code u8 TODO, text language 3 bytes). Consistent with libbluray.
- TSStreamClipFile.cs (247 lines) was fetched but not read in detail. NOT VERIFIED beyond the file existing.

## Top findings (summary for Atlas)

1. HDR/DV signaling is in two places. MPLS STN coding_type 0x24 (HEVC) carries `dynamic_range_type` (4 bits), `color_space` (4 bits), `cr_flag`, and `hdr_plus_flag`. CLPI StreamAttr 0x24 carries the same fields, shifted by aspect and oc_flag. VERIFIED-ORIG.
2. DV enhancement-layer streams are a separate STN array of `num_dv` entries at STN byte offset 11, after the secondary video block. VERIFIED-ORIG.
3. HDR10 static metadata (MaxCLL, MaxFALL, mastering luminance, primaries) is in the MPLS extension (3,5) as 28-byte entries. VERIFIED-ORIG.
4. The UHD disc-level flags (4K, HDR, HDR10+, DV, disc_type) are in index.bdmv extension 3.1. VERIFIED-ORIG.
5. id.bdmv: org_id at 40 (4 bytes), disc_id at 44 (16 bytes). The AACS Disc ID that libbluray exposes is a separate 20-byte value. VERIFIED-ORIG.
6. DVDDiscID is MD5 over the full VIDEO_TS.IFO plus VTS_nn_0.IFO files, up to 10 IFOs. VERIFIED-ORIG.
7. ISO PVD volume id at byte 40 (32 bytes) and volume-set id at byte 190 (128 bytes). VERIFIED-ORIG.
8. Libbluray's Mpls AppInfo UO mask is 8 bytes at offset 48 and the flags byte is at offset 56. VERIFIED-ORIG.

## NOT VERIFIED

- DV RPU (Dolby Vision metadata) layout: not in these sources.
- HDR10+ metadata: not in these sources.
- Value enumerations for dynamic_range_type and color_space: not documented in the parser sources.
- DVD audio_attr_t exact bit positions: see the caveat above.
- Full ISO 9660 and UDF structures beyond the volume id offsets.
