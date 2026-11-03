# W1c: Disc image and broadcast container metadata (Atlas, Rust-native)

Scope: ISO 9660 / UDF, Blu-ray BDMV, DVD VIDEO_TS, MPEG-TS / M2TS, AVI, crate survey.
Time box: 10 minutes. Items marked [VERIFIED] were read in a fetched source. Items marked [SPEC-MEMORY] come from prior knowledge of the spec and need a check against the spec or a real sample before coding. Offsets in [SPEC-MEMORY] are not confirmed.

## Sources fetched
- libbluray bdid_parse.c (id.bdmv): https://git.stg.centos.org/source-git/libbluray/blob/c8/f/src/libbluray/bdnav/bdid_parse.c
- libfreemkv 0.31.1 src/mpls.rs (yanked release, read for layout): https://docs.rs/crate/libfreemkv/0.31.1/source/src/mpls.rs
- libfreemkv 0.8.0 docs/mpls.md: https://docs.rs/crate/libfreemkv/0.8.0/source/docs/mpls.md
- bdinfo-rs-core 2.0.0 src/bdrom/mpls.rs (seen in search, not fetched): https://docs.rs/crate/bdinfo-rs-core/latest/source/src/bdrom/mpls.rs
- oxideav-dvd 0.0.3 src/ifo.rs: https://docs.rs/oxideav-dvd/0.0.3/src/oxideav_dvd/ifo.rs.html
- bin2xml IFO notes: https://bin2xml.sourceforge.net/ifo.html
- libbluray BLURAY_DISC_INFO doxygen (redirected, not read): https://videolan.videolan.me/libbluray/structBLURAY__DISC__INFO.html
- lib.rs mpeg2ts: https://lib.rs/crates/mpeg2ts
- docs.rs mpeg2ts-reader: https://docs.rs/crate/mpeg2ts-reader/latest
- ETSI EN 300 468 (DVB SI, search result, not read): https://www.etsi.org/deliver/etsi_EN/300400_300499/300468/01.16.01_60/en_300468v011601p.pdf (search lists 01.16.01_60 at https://www.etsi.org/deliver/etsi_en/300400_300499/300468/01.16.01_60/en_300468v011601p.pdf)
- ECMA-119, ECMA-167 / OSTA UDF 2.60, ISO 13818-1, AACS spec 0.953 (listed, not read): https://aacsla.com/wp-content/uploads/2019/02/AACS_Spec_BD_Prepared_Final_0.953.pdf
- udflabel(8) man page: https://www.man7.org/linux/man-pages/man8/udflabel.8.html

## 1. ISO 9660 and UDF (first 1-2 MB)

Byte layout: system area sectors 0-15 (2048-byte sectors, 0x0000-0x7FFF, usually zero or boot code). Volume descriptor set starts at sector 16 (0x8000). Terminator descriptor type 255. [SPEC-MEMORY ECMA-119 8.4]

- iso.pvd.system_id: ascii[32] at PVD+8 (0x8008). Example "LINUX", "WIN32". front_of_file. link: none.
- iso.pvd.volume_id: ascii[32] at PVD+40. Disc label. front_of_file. link: same-disc (weak; labels are reused by many releases).
- iso.pvd.volume_space_size: u32 LE/BE at PVD+80. Sector count. Gives exact image size and detects truncated posts (same-film twin check). link: none.
- iso.pvd.volume_set_id: a-char[128] at PVD+190. Often empty or a tool string. link: same-disc (weak).
- iso.pvd.publisher_id: a-char[128] at PVD+318. Often empty. link: none.
- iso.pvd.data_preparer_id: a-char[128] at PVD+446. Often names the mastering tool (e.g. a vendor string). Verify on real samples. link: none.
- iso.pvd.application_id: a-char[128] at PVD+574. Mastering tool signature (for example mkisofs/genisoimage/ImgBurn strings). Verify exact strings on real samples; do not rely on them as fact. link: alt (same mastering pipeline).
- iso.pvd.creation_date / modification / expiration / effective: dec-datetime 17 bytes each (YYYYMMDDHHMMSSss + tz byte) at PVD+813, +830, +847, +864. Mastering timestamps. link: same-disc only if exact match and sub-second matches.
- iso.pvd.file_structure_version: u8 at PVD+881 (usually 1).
- iso.pvd.logical_block_size: u16 at PVD+128 (2048 almost always). [SPEC-MEMORY]
- UDF Volume Recognition Sequence (VRS): sectors 16+ hold BEA01, NSR02 or NSR03, TEA01. The VRS tells UDF images apart from plain ISO 9660. [SPEC-MEMORY ECMA-167 part 2]
- UDF AVDP (Anchor Volume Descriptor Pointer): tag identifier 2 at sector 256 (0x100000 byte offset). Points to the main and reserve VDS. [SPEC-MEMORY ECMA-167 10.2]
- UDF Primary Volume Descriptor (tag 1) inside VDS. [SPEC-MEMORY ECMA-167 10.1]
  - udf.pvd.volume_identifier: dstring[32] at PVD+24.
  - udf.pvd.volume_set_identifier: dstring[128] at PVD+72. Often a tool-written hex or GUID-like string. Test on samples before using as a unique ID; it is not a documented per-pressing key.
  - udf.pvd.recording_date_time: timestamp[12] at PVD+376. link: same-disc candidate if exact match.
  - udf.pvd.application_identifier: regid[32] at PVD+344. Format: flags byte + ID string (for example "*UDF ..."). link: alt.
  - udf.pvd.implementation_identifier: regid[32] at PVD+388. Identifies the writer. Vendor strings such as ImgBurn or AnyDVD may appear in the Implementation Use area, not in this regid. The regid normally starts with "*". The example names in the brief (ImgBurn, AnyDVD, DVD Decrypter) are unverified; check against real images. link: alt.
  - udf.pvd.implementation_use: bytes[64] at PVD+420. Vendor-defined. Test on samples. link: alt.
- UDF Logical Volume Descriptor (tag 6) [SPEC-MEMORY ECMA-167 10.6]:
  - udf.lvd.logical_volume_identifier: dstring[128] at LVD+84. Often identical to the PVD label. link: same-disc (weak).
  - udf.lvd.logical_block_size: u32 at LVD+212.
  - udf.lvd.domain_identifier: regid at LVD+216 ("*OSTA UDF Compliant" in most images). link: none.
  - udf.lvd.implementation_identifier: regid[32] at LVD+272. Writer string. link: alt.
  - udf.lvd.implementation_use: bytes[128] at LVD+304. Vendor data, possibly containing a tool string. Test on samples.
- UDF Logical Volume Integrity Descriptor (LVID, tag 9): recording time, next-unique-ID, file and directory counts in the implementation use area. Read via the integrity sequence extent in the LVD (LVD+432). Could give file-count fingerprints. link: same-film (file count) if content is identical.
- Note: UDF "volume set identifier" and LVID unique IDs are written by the mastering tool and are not guaranteed unique per pressing. Treat as a weak same-disc key until tested on several pressings.

udflabel(8) (udftools) reads and writes the LVD and PVD label fields. Useful as a cross-check of offsets on a sample image: https://www.man7.org/linux/man-pages/man8/udflabel.8.html

## 2. Blu-ray BDMV

Files live under BDMV/ on the disc image. Files for titles: BDMV/index.bdmv, BDMV/MovieObject.bdmv, BDMV/PLAYLIST/*.mpls, BDMV/CLIPINF/*.clpi, BDMV/STREAM/*.m2ts, BDMV/META/DL/bdmt_eng.xml, CERTIFICATE/id.bdmv, AACS/*.

### 2a. CERTIFICATE/id.bdmv [VERIFIED via libbluray bdid_parse.c]
- bd.id.magic: "BDID" at byte 0 (checked by bdmv_parse_header).
- bd.id.data_start (u32) and bd.id.extension_data_start (u32): header fields after the magic. Offsets not shown in the fetched source.
- bd.id.org_id: 4 bytes at offset 40 (0x28), hex-encoded. Organization ID (licensee). link: same-disc or same-pressing run (same org = same replicator/licensor family).
- bd.id.disc_id: 16 bytes at offset 44 (0x2C) through 59, hex-encoded. The source performs no hash: disc_id is the 16 raw bytes. link: same-disc candidate.
- Lookup: libbluray opens CERTIFICATE/id.bdmv and falls back to CERTIFICATE/BACKUP/id.bdmv.
- CONFIRMATION STATUS: the brief asks whether disc_id is a per-pressing ID. NOT CONFIRMED. The fetched code proves only the byte layout. Whether two copies of one pressing share the value, and whether two pressings of one title differ, needs a test on at least 2 rips of the same release and 2 of different releases. Do not state "per-pressing" as fact.
- Rule: never store or log keys or key material from AACS. The disc ID is not key material, but it sits in an AACS-related area; keep it as metadata only.

### 2b. index.bdmv and MovieObject.bdmv [SPEC-MEMORY, libbluray index_parse.c / mobj_parse.c]
- bd.index.magic: "INDX"; bd.index.version: "0200" (BD) or "0300" (UHD BD) at byte 4. link: none. UHD flag candidate: version 0300.
- bd.index.first_play: the FirstPlay title entry, plus title count (num_titles u16) and per-title object ref. link: same-film if the first_play title matches.
- bd.index.top_menu and title entries: object type (HDMV or BD-J). BD-J presence means a Java app is on disc (flag bd.bdj_present). [SPEC-MEMORY]
- bd.mobj.magic: "MOBJ"; bd.mobj.version: "0200" / "0300". Count of movie objects and navigation commands. link: none.

### 2c. PLAYLIST/*.mpls [VERIFIED via libfreemkv 0.31.1 mpls.rs and docs/mpls.md; two summaries agree once the 2-byte length prefix is accounted for]
Header (absolute offsets):
- bd.mpls.magic: "MPLS" at 0.
- bd.mpls.version: 4 bytes ASCII "0200" (BD) or "0300" (UHD) at 4.
- bd.mpls.playlist_start: u32 at 8.
- bd.mpls.playlist_mark_start: u32 at 12.
- bd.mpls.extension_data_start: u32 at 16 (libfreemkv does not parse it, so extension data is not decoded there).

PlayList section (relative to playlist_start):
- length u32 at 0; reserved u16 at 4; num_play_items u16 at 6; num_sub_paths u16 at 8.

PlayItem entry (offsets relative to the start of each PlayItem, including the 2-byte length prefix at 0):
- bd.pi.length: u16 at 0.
- bd.pi.clip_id: ascii[5] at 2 (for example "00001"). Maps to STREAM/00001.m2ts and CLIPINF/00001.clpi. link: same-film (matching clip id across encodes is not a link; clip id is per disc).
- bd.pi.codec_id: ascii[4] at 7 ("M2TS").
- bd.pi.connection_condition: low 4 bits of byte 11 (libfreemkv: byte 9 after the length prefix). Seamless vs non-seamless.
- bd.pi.stc_id: u8 at 13 (after prefix byte 12). [SPEC-MEMORY]
- bd.pi.in_time: u32 at 14 (45 kHz ticks).
- bd.pi.out_time: u32 at 18 (45 kHz ticks).
- Duration: (out_time - in_time) / 45000 seconds per play item. Sum all items for the title duration. Frame count = duration x fps (for example 23.976). Sum across play items, not per clip.
- bd.pi.uo_mask_table: 8 bytes at 22 (user operation mask, e.g. chapter-skip lock).
- bd.pi.still_mode: at 31 (after misc flags at 30). Still image playlists are detectable.

STN_table (first PlayItem only; header 16 bytes, verified by libfreemkv):
- length u16 at 0, reserved u16 at 2.
- Stream counts (u8 each): n_video at 4, n_audio at 5, n_pg at 6, n_ig at 7, n_sec_audio at 8, n_sec_video at 9, n_pip_pg at 10, n_dv at 11. (The bin2xml summary lists only four counts; it is wrong about the header size, so use libfreemkv.)
- Stream entries start after the 16-byte header.
- Stream entry: length u8, stream_type u8 (1 primary video, 2 primary audio, 3 PG, 4 IG, 5 secondary audio, 6 secondary video, 7 Dolby Vision enhancement layer), then pid u16 (offset depends on type byte).
- Stream attributes (per entry): length u8, coding_type u8, then:
  - Video: format (upper nibble) and frame rate (lower nibble) at offsets 2 in attributes. Plus dynamic_range and color_space bytes in UHD (0300) entries [verified field names in libfreemkv StreamEntry; exact offsets SPEC-MEMORY].
  - Audio: format/channel layout (upper nibble), sample rate (lower nibble), language 3 bytes ISO 639-2.
  - PG and IG: language 3 bytes.
- bd.stn.coding_type: 0x02 MPEG-2, 0x1B AVC, 0x24 HEVC, 0xEA VC-1 (video); 0x80 LPCM, 0x81 AC-3, 0x82 DTS, 0x83 TrueHD, 0x84 E-AC-3, 0x85 DTS-HD HR, 0x86 DTS-HD MA, 0xA1 secondary AC-3 Plus, 0xA2 secondary DTS-HD HR (audio); 0x90 PGS, 0x91 IG (subtitle/graphics). [VERIFIED via libfreemkv / bdinfo summary table]
- bd.stn.video_format: 1 480i, 2 576i, 3 480p, 4 1080i, 5 720p, 6 1080p, 7 576p, 8 2160p. [VERIFIED]
- bd.stn.video_frame_rate: 1 23.976, 2 24, 3 25, 4 29.97, 6 50, 7 59.94. [VERIFIED]
- bd.stn.audio_channels: 1 mono, 3 stereo, 6 5.1, 12 7.1. [VERIFIED]
- bd.stn.audio_sample_rate: 1 48 kHz, 4 96 kHz, 5 192 kHz, 12 48/192, 14 48/96. [VERIFIED]
- bd.stn.language: ISO 639-2 3-letter codes. link: none.

Secondary streams and DV:
- bd.stn.dv_enhancement_layer: stream_type 7 entries (n_dv count at STN+11). Presence means a Dolby Vision EL is referenced by this playlist. Strong DV-present signal without parsing video. [VERIFIED field names; libfreemkv]
- bd.stn.dynamic_range / color_space: per video entry in UHD. Gives HDR10 / HDR10+ / DV / SDR class at playlist level. Offset not verified. [VERIFIED field names in libfreemkv; offsets SPEC-MEMORY]

PlayListMark (at playlist_mark_start):
- length u32 at 0; num_marks u16 at 4; entries start at 6, 14 bytes each.
- Entry: reserved u8 at 0, mark_type u8 at 1 (1 = entry mark / chapter, 2 = link point), ref_to_play_item_id u16 at 2, mark_time_stamp u32 at 4 (45 kHz), entry_es_pid u16 at 8, duration u32 at 10. [VERIFIED via libfreemkv docs; entry/link semantics per the same source]
- Chapter list = all mark_type 1 entries with ref_to_play_item_id.

SubPaths: num_sub_paths gives secondary sub-paths (for example, MVC dependent view or secondary audio / commentary). Sub-path type field not parsed here. [SPEC-MEMORY] Use it for 3D / MVC detection.

Extension data (ExtensionData at header offset 16): not parsed by libfreemkv. Holds additional entries (for example UHD extension and DV EL data). Needs its own parser. [SPEC-MEMORY]

### 2d. CLIPINF/*.clpi [SPEC-MEMORY; check against libbluray clpi_parse.c]
- bd.clpi.magic: "HDMV" at 0; version "0200" or "0300".
- Sequence info, program info, CPI (EP map), clip mark, extension data start offsets in the header.
- bd.clpi.ts_recording_rate: 3 bytes after the clip info block, bits per second. Gives average bitrate of the stream.
- bd.clpi.number_of_source_packets: u32, source packet count. Duration cross-check: packets x 192 bytes x 8 / TS_recording_rate.
- bd.clpi.program_info: program_map_pid, number of streams, per stream coding_type, video format, frame rate, aspect ratio, cc flag; audio format, sample rate, language. Same fields as STN but per clip, so this is a second source for the stream table.
- bd.clpi.ep_map (CPI): entry point list with PTS and SPN. Enables seeking and frame-accurate chapter times. Can be used to count keyframes (IDR) per clip.
- link: same-film if clip packet count and recording rate match across encodes; alt if the same clip is repacked.

### 2e. BDMV/META/DL/bdmt_eng.xml [SPEC-MEMORY]
- bd.meta.title: <di:name> element (disc title). link: same-disc (weak, titles repeat across releases).
- bd.meta.thumbnail: image file reference in META/DL/thumbnails/*.jpg. link: twin if byte-identical thumbnail.

### 2f. UHD and 3D flags [SPEC-MEMORY]
- index.bdmv and MPLS version "0300" = UHD BD (pair with STN video format 8 = 2160p).
- 3D / MVC: a SubPath with a sub-path type for dependent view, and MVC coding in CLPI program info. The 0x20 coding type is the MVC dependent-view stream (SPEC-MEMORY).
- AACS directory: Unit_Key_RO.inf and AACS/ presence means encrypted disc. Keep as a flag only; do not parse or store key material.

## 3. DVD VIDEO_TS (IFO) [VERIFIED via oxideav-dvd 0.0.3 ifo.rs and bin2xml notes; offsets are the oxideav values]

VIDEO_TS.IFO (VMGI):
- dvd.vmg.magic: "DVDVIDEO-VMG" at 0.
- dvd.vmg.last_sector: u32 at 0x0C.
- dvd.vmg.version: u16 at 0x20.
- dvd.vmg.category: u32 at 0x22 (VMG category, includes karaoke/region bits per the spec). Offsets: oxideav gives 0x22.
- dvd.vmg.number_of_volumes: u16 at 0x26; volume_number u16 at 0x28; side_id u8 at 0x2A.
- dvd.vmg.number_of_title_sets: u16 at 0x3E. Count of VTS. link: same-disc.
- dvd.vmg.provider_id: string[32] at 0x40. Provider / publisher string, often the authoring tool or vendor. link: same-disc (strong if non-generic).
- dvd.vmg.region_mask: prohibited region mask (8 bits). Offset not confirmed; check spec. link: none.
- dvd.vmg.fp_pgc_addr: u32 at 0x84 (first play PGC sector pointer).
- dvd.vmg.tt_srpt_sector: u32 at 0xC4 (title table pointer).
- dvd.vmg.menu_vob_sector: u32 at 0xC0.

VTS_xx_0.IFO (VTSI):
- dvd.vts.magic: "DVDVIDEO-VTS" at 0.
- dvd.vts.category: u32 at 0x22 (0 normal, 1 karaoke).
- dvd.vts.video_attributes: 2 bytes at 0x200 (title block). Byte 0: coding (MPEG-1 / MPEG-2), standard (NTSC / PAL), aspect (4:3, 16:9), pan-scan and letterbox disallow bits. Byte 1: line-21 CC flags, resolution index (720x480/576, 704, 352, 352x240/288), letterboxed source bit, film source PAL bit. [VERIFIED bitfield map in oxideav]
- dvd.vts.audio_count: u16 at 0x202. dvd.vts.audio[0..8]: 8 bytes each at 0x204.
  - Byte 0: coding (AC-3, MPEG-1, MPEG-2 ext, LPCM, DTS), multichannel ext bit, language type bits, application mode bits.
  - Byte 1: quantization / DRC bits, sample-rate code (only 0 = 48 kHz defined for DVD-Video), channels minus 1 (bits 2-0).
  - Bytes 2-3: language code (ISO 639 2-char). Byte 5: code extension. Byte 7: application info.
- dvd.vts.subpicture_count: u16 at 0x254. dvd.vts.subpicture[0..32]: 6 bytes each at 0x256.
  - Byte 0: coding (2-bit RLE), language type. Bytes 2-3: language code. Byte 5: code extension.
- dvd.vts.title_ptt_srpt: chapter (PTT) table pointers at sector offsets stored in the VTSI header (oxideav 0x00C8 vts_ptt_srpt). Maps titles to PGCs and PGNs (chapters).

PGC (program chain) [VERIFIED oxideav]:
- pgc.number_of_programs: u8 at +0x02 (chapters).
- pgc.number_of_cells: u8 at +0x03.
- pgc.playback_time: 4 bytes BCD at +0x04. Byte 0-2 hours/minutes/seconds (BCD). Byte 3 bits 5-0 frames (BCD tens/units), bits 7-6 frame-rate code: 01 = 25 fps, 11 = 30 (29.97) fps, 00 and 10 reserved. Gives the exact runtime.
- pgc.prohibited_user_ops: u32 at +0x08.
- pgc.next_pgcn / prev_pgcn / goup_pgcn: u16 at +0x9C, +0x9E, +0xA0.
- pgc.still_time: u8 at +0xA2. pgc.playback_mode: u8 at +0xA3.
- pgc.palette: 16 x 4 bytes at +0xA4 (YUV colour table).
- Table pointers (u16): command table +0xE4, program map +0xE6, cell playback +0xE8, cell position +0xEA.
- Cell playback info: 24 bytes per cell (category, still time, command number, playback time BCD at +4, first/last VOBU sector pointers at +8, +12, +16, +20).
- Cell position info: vob_id u16 and cell_id u8 per cell.
- Chapters per title: derived from PTT table + program map (program_map lists the first cell per chapter).

Disc ID (libdvdread DVDDiscID) [SPEC-MEMORY, verify in libdvdread dvd_discid.c]: MD5 over the IFO files of the disc (VIDEO_TS.IFO and VTS_xx_0.IFO in order). Two rips of the same disc give the same ID even if the file names differ. link: same-disc (strong; this is the best DVD key).

## 4. MPEG-TS / M2TS (first 1 MB)

Packet framing:
- M2TS: 192-byte packets. Bytes 0-3 are TP_extra_header: arrival timestamp (30 bits ATS, 27 MHz ticks) and copy-permission (2 bits). Sync byte 0x47 at offset 4. link: alt (repack changes ATS).
- TS: 188-byte packets, sync byte 0x47 at 0.

PSI/SI (ISO 13818-1, ETSI EN 300 468, ETSI TS 102 808 for DVB, ATSC A/65 for ATSC) [SPEC-MEMORY]:
- PAT: PID 0x0000, table_id 0x00. program_number to PMT PID. Gives program count and channel-like grouping.
- PMT: table_id 0x02. program_info descriptors, then per stream: stream_type (0x02 MPEG-2 video, 0x1B AVC, 0x24 HEVC, 0x81 AC-3, 0x87 E-AC-3 in ATSC, 0x06 private for DTS and AC-3 in DVB), elementary_PID, ES_info descriptors.
- PMT descriptors per stream:
  - 0x0A ISO 639 language descriptor: language code + audio type. Tag 0x0A. [VERIFIED name in mpeg2ts-reader]
  - 0x05 registration descriptor: format_identifier 4 bytes ("HDMV" for BD streams, "DOVI" for Dolby Vision, "AC-3", "DTS1"). [VERIFIED in mpeg2ts-reader]
  - 0x6A AC-3 descriptor, 0x7A E-AC-3 descriptor, 0x7B DTS descriptor (DVB). [SPEC-MEMORY]
  - 0xB0 DOVI video stream descriptor (Dolby Vision config: profile, level, bl/el/rpu flags). Tag reported in DVB and in Dolby docs; verify. [SPEC-MEMORY]
  - 0x1B AVC video descriptor, 0x2A AVC timing and HRD (bitrate hints). [SPEC-MEMORY]
- SDT: PID 0x0011, table_id 0x42 (actual TS) / 0x46 (other). service_descriptor tag 0x48: service_type, service_provider_name, service_name. This is the TV channel name and provider string, the strongest "broadcast capture" identifier. link: same-film (same channel + same EIT event + same capture day).
- EIT: PID 0x0012. table_id 0x4E (present / following), 0x50-0x5F (schedule). short_event_descriptor tag 0x4D: ISO 639 language, event_name, text. Also extended_event_descriptor tag 0x4E. The event name and start time are the programme identifier. link: same-film.
- TDT: PID 0x0014, table_id 0x70. UTC time as MJD (16 bits) + BCD HHMMSS (24 bits). Capture timestamp (UTC, approximate broadcast time).
- TOT: PID 0x0014, table_id 0x73, with local_time_offset_descriptor tag 0x58 giving local time zone. Use with TDT for capture date/time.
- Duration: from EIT event duration (24-bit BCD) for the programme; frame count from video PES timing.

Video / audio parsed fields (for bitrate, resolution, frame count):
- Resolution, frame rate, bit depth come from the SPS (AVC/HEVC) or sequence header (MPEG-2). Not in PSI. Rust parsing of SPS is needed and is out of scope here.
- Bitrate: AVC HRD in the PMT AVC timing descriptor, or TS_recording_rate in CLPI (BD). Otherwise compute from PCR deltas or byte count / duration.
- Dolby Vision RPU: NAL unit type 62 (HEVC UNSPEC62) in the enhancement layer or base layer; signalled by DOVI descriptor 0xB0. Not in PSI beyond the descriptor.
- HDR10+: dynamic metadata in SEI payload (ITU-T T.35), not in PSI.

## 5. AVI (RIFF) [SPEC-MEMORY, Microsoft AVI RIFF docs]
- avi.riff.magic: "RIFF" + size + "AVI ".
- avi.hdrl.avih (56 bytes): dwMicroSecPerFrame (fps), dwMaxBytesPerSec, dwTotalFrames at offset 16, dwWidth at 32, dwHeight at 36, dwStreams at 24.
- avi.hdrl.strl.strh: fccType ("vids" / "auds"), fccHandler (codec FourCC), dwScale, dwRate (fps = rate/scale), dwLength (frames or samples).
- avi.hdrl.strl.strf: BITMAPINFOHEADER (video, incl. biCompression FourCC, biBitCount) or WAVEFORMATEX (audio: nChannels, nSamplesPerSec, nAvgBytesPerSec, wFormatTag).
- avi.info.ISFT: software name (for example VirtualDubMod, MEncoder, Lavf). link: alt (same tool).
- avi.info.INAM / IART / ICRD: title, artist, creation date.
- avi.odml.dmlh: dwTotalFrames (OpenDML large file). avi.odml.indx: super-index.
- JUNK: padding, no data.

## 6. Crate survey (crates.io / docs.rs) [VERIFIED where stated]

- mpeg2ts 0.3.1: MIT, ~92 KB, 26 downloads/month, 8 dependent crates, last release June 2023, flagged unstable. Does not state PSI/SI coverage. Recommend: do not depend on it. (https://lib.rs/crates/mpeg2ts)
- mpeg2ts-reader 0.18.2: released 2025-01-29, zero-copy TS parser. Supports PAT, PMT, registration descriptor, ISO 639 language descriptor, AVC video descriptor. Does not list SDT, EIT, TDT/TOT, AC-3/E-AC-3 descriptors, or DOVI. Usable for PAT/PMT; SI tables need hand-written code. (https://docs.rs/crate/mpeg2ts-reader/latest)
- libfreemkv (0.31.1 yanked; 0.8.0 earlier): "open source raw disc access library" with a pure-Rust MPLS parser that includes STN secondary streams, DV EL and PlayListMark. Pure-Rust status not checked. Use only as a reference for offsets; do not add as dependency (yanked, unclear maintenance). (https://docs.rs/crate/libfreemkv/0.31.1/source/src/mpls.rs)
- bdinfo-rs-core 2.0.0: a Rust BDInfo-style crate with bdrom/mpls.rs. Maintenance and downloads not checked. Reference only. (https://docs.rs/crate/bdinfo-rs-core/latest/source/src/bdrom/mpls.rs)
- ISO 9660 / UDF crates seen in search (not evaluated for downloads): isomage 2.1.0 (https://docs.rs/crate/isomage/latest), cdfs 0.2.3 (https://docs.rs/crate/cdfs/latest/source/src/parse/mod.rs), iso9660-rs 1.0.2 (https://docs.rs/crate/iso9660-rs/latest). Not checked for UDF support.
- oxideav-dvd 0.0.3: Rust IFO parser (VMGI, VTSI, PGC, cell time), depends only on optional oxideav-core / oxideav-mkv. Early version. Reference for IFO offsets. (https://docs.rs/oxideav-dvd/0.0.3/src/oxideav_dvd/ifo.rs.html)
- RIFF / AVI crates: not searched in this pass.

Project rule (widespread crates only) implies: hand-write MPLS, CLPI, IFO, id.bdmv, UDF, and SI parsers. Use mpeg2ts-reader only for PAT/PMT if the rule allows a low-download crate, otherwise hand-write.

## Linking values by field

- twin: byte-identical file (ISO hash, thumbnail bdmt xml, full IFO set).
- alt: same inner file, different packing (same MPLS/CLPI in an RAR vs ISO; same ATS-shifted TS packets).
- same-film: same content, different encode (matching title duration from MPLS sums, matching CLIPINF packet count, matching EIT event name + start time, matching VTS PGC playback time).
- same-disc: same source disc (libdvdread MD5 of IFOs; id.bdmv disc_id if confirmed; ISO PVD/UDF volume and volume set IDs as weak signal only).
- none: generic fields (system ID, publisher, region mask, language codes).

## Surprises
1. MPLS STN carries DV enhancement-layer streams (stream_type 7, count at STN+11) and in UHD per-stream dynamic_range / color_space fields. DV and HDR class can be read from the playlist without decoding video. [libfreemkv]
2. id.bdmv disc_id is the raw 16 bytes at offset 44, with no hash. [libbluray bdid_parse.c]
3. The STN table header is 16 bytes, not 8 (bin2xml summary is incomplete on secondary stream counts). [libfreemkv]
4. DVD VTS audio sample-rate code: only 0 = 48 kHz is defined for DVD-Video. The IFO rate is not a real signal; use the stream.
5. DVD PGC frame-rate code 01 = 25 fps, 11 = 30 fps (NTSC 29.97 nominal). A 30 fps PGC on an NTSC disc is not a bitrate indicator.
6. The two MPLS summaries disagree on absolute offsets because one counts from the post-length prefix. Use the prefixed offsets from this report (PlayItem IN at 14, OUT at 18).
7. mpeg2ts-reader does not parse SDT or EIT, so broadcast channel and event name require hand-written SI parsing.
8. Dolby Vision RPU has no PSI field beyond the DOVI descriptor; HDR10+ lives only in SEI. Presence needs video-layer parsing.

## Open questions
- Confirm id.bdmv disc_id uniqueness on real rips (per-pressing vs per-title).
- Confirm VMG region mask offset and PGC playback time offsets against the spec.
- Confirm DOVI descriptor tag 0xB0 in a real PMT dump.
- Check UDF implementation identifier strings on real ImgBurn / AnyDVD / DVD Decrypter images before any mapping.
