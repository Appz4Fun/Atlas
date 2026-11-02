# W2c: Disc image and Blu-ray/DVD metadata byte layouts (Atlas, Rust-native)

Time box: 10 minutes. Status tags: [VERIFIED-SRC] = read in a fetched or local source file this session. [VERIFIED-SUMMARY] = read in a WebFetch model summary of a source, not checked line by line. [MEMORY] = prior knowledge, not checked; confirm before coding.

Blocked this session: code.videolan.org (CONNECT 403), github.com/videolan clones (no auth in sandbox), raw.githubusercontent.com videolan paths (404). So libbluray mpls_parse.c, clpi_parse.c, bdid_parse.c and libdvdread dvd_reader.c were NOT read directly. Their content below comes from third-party parsers and must be checked against the originals.

Local sources read: libcdio include/cdio/iso9660.h (fetched from raw.githubusercontent.com/libcdio), Linux fs/udf/ecma_167.h (fetched from raw.githubusercontent.com/torvalds/linux).
Remote summaries: docs.rs libfreemkv 0.31.1 src/mpls.rs (yanked crate, source) [VERIFIED-SRC]; docs.rs libfreemkv 0.8.0 docs/mpls.md [VERIFIED-SUMMARY]; docs.rs oxideav-dvd 0.0.3 src/ifo.rs [VERIFIED-SUMMARY].

## 1. ISO 9660 Primary Volume Descriptor (sector 16 = byte 0x8000)

Field offsets are relative to the PVD start (0x8000 + offset). Widths and names match libcdio iso9660.h struct iso9660_pvd_s [VERIFIED-SRC for the field list and string sizes: volume_id 32, system_id 32, volume_set_id 128, publisher_id 128, preparer_id 128, application_id 128, and four 17-byte dates]. Offsets are computed from ECMA-119 field widths [MEMORY, consistent with the list]:

| Offset | Len | Field | Notes |
|---|---|---|---|
| 0 | 1 | type (1 = PVD) | |
| 1 | 5 | "CD001" | standard identifier |
| 6 | 1 | version (1) | |
| 8 | 32 | system_id | a-chars |
| 40 | 32 | volume_id | d-chars, space padded |
| 80 | 8 | volume_space_size | both-endian u32 (LE then BE) |
| 120 | 4 | volume_set_size | both-endian u16 |
| 128 | 4 | logical_block_size | both-endian, 2048 |
| 156 | 34 | root directory record | |
| 190 | 128 | volume_set_id | d-chars, often empty or a tool string |
| 318 | 128 | publisher_id | a-chars |
| 446 | 128 | data_preparer_id | a-chars, often the mastering tool |
| 574 | 128 | application_id | a-chars, mastering tool signature |
| 702 | 37 | copyright file id | |
| 739 | 37 | abstract file id | |
| 776 | 37 | bibliographic file id | |
| 813 | 17 | volume creation date/time | ASCII YYYYMMDDHHMMSScc + 1 byte GMT offset (signed, 15-min units) |
| 830 | 17 | modification date/time | |
| 847 | 17 | expiration date/time | |
| 864 | 17 | effective date/time | |
| 881 | 1 | file structure version (1) | |

Date format: 16 ASCII digits (year, month, day, hour, minute, second, hundredths) plus one tz byte. All-zero/space-filled means "not set". [MEMORY]

## 2. UDF (ECMA-167 / OSTA UDF) descriptors

Verified against Linux ecma_167.h [VERIFIED-SRC]:

- Descriptor tag (16 bytes, struct tag): tagIdentifier u16 @0, descriptorVersion u16 @2, tagChecksum u8 @4, reserved u8 @5, tagSerialNumber u16 @6, descriptorCRC u16 @8, descriptorCRCLength u16 @10, tagLocation u32 @12 (LE). Tag IDs (1 PVD, 2 AVDP, 4 IUVD, 6 LVD, 8 TD, 256 FSD, 261 FE) [MEMORY].
- Timestamp (12 bytes, struct timestamp): typeAndTimezone u16 @0 (type nibble 0=CUT, 1=local, 2=agreement; low 12 bits = tz minutes), year u16 @2, month @4, day @5, hour @6, minute @7, second @8, centiseconds @9, hundreds of µs @10, µs @11. [VERIFIED-SRC]
- regid / EntityID (32 bytes, struct regid): flags u8 @0 (bit 0 dirty, bit 1 protected), ident[23] @1 (e.g. "*ImgBurn", "*Linux UDFFS", "*Sony", "*Panasonic", "*Pioneer"), identSuffix[8] @24 (OS identifier + UDF revision bytes). [VERIFIED-SRC]
- AVDP (tag 2, 512 bytes, struct anchorVolDescPtr): tag @0; mainVolDescSeqExt extent_ad (len u32, loc u32) @16 (8 bytes); reserveVolDescSeqExt @24; reserved[480] @32. AVDP lives at sector 256 (byte 0x80000) and at the last sector (N-256). [VERIFIED-SRC for struct; sector 256 location MEMORY]
- PVD (tag 1, 512 bytes, struct primaryVolDesc) [VERIFIED-SRC, offsets computed from the struct]:
  - volDescSeqNum @16 (4), primaryVolDescNum @20 (4)
  - volIdent dstring[32] @24 (last byte = used length)
  - volSeqNum @56, maxVolSeqNum @58, interchangeLvl @60, maxInterchangeLvl @62, charSetList @64, maxCharSetList @68
  - volSetIdent dstring[128] @72 (end 200). Note: the "first 16 chars are a hex timestamp" pattern is a tool convention, not in ECMA-167 [MEMORY].
  - descCharSet charspec[64] @200, explanatoryCharSet @264
  - volAbstract extent_ad @328, volCopyright extent_ad @336
  - appIdent regid @344 (32) [ImgBurn / mkudffs / authoring tool ID goes here]
  - recordingDateAndTime timestamp @376 (12)
  - impIdent regid @388 (32)
  - impUse[64] @420, predecessorVolDescSeqLocation @484, flags @488, reserved[22] @490
- LVD (tag 6, struct logicalVolDesc) [VERIFIED-SRC field list; offsets computed]: volDescSeqNum @16, descCharSet @20 (64), logicalVolIdent dstring[128] @84, logicalBlockSize @212, domainIdent regid @216, logicalVolContentsUse[16] @248 (FSD long_ad), mapTableLength @264, numPartitionMaps @268, impIdent regid @272, impUse[128] @304, integritySeqExt @432, partitionMaps @440.
- IUVD / LVInfo (tag 4, Implementation Use Volume Descriptor, OSTA UDF 2.60 2.2.7) [MEMORY, layout derived to total 512 bytes]: implementationIdentifier regid @20 (32, e.g. "*UDF LV Info"); then LVICharset charspec @52 (64), LogicalVolumeIdentifier dstring @116 (128), LVInfo1 dstring @244 (36, commonly owner/name), LVInfo2 @280 (36, commonly organization), LVInfo3 @316 (36, commonly contact), implementationID regid @352 (32), impUse @384 (128). Exact field usage by pressing tools is unverified.
- Logical Volume Integrity Descriptor (struct logicalVolIntegrityDesc) has recordingDateAndTime timestamp near @16. [VERIFIED-SRC for presence; offset MEMORY]
- FSD (tag 256, struct fileSetDesc) has recordingDateAndTime timestamp near @16, domainIdent regid, fileSetIdent dstring. [VERIFIED-SRC for presence; offsets MEMORY]

Which identifiers mark a source: appIdent / impIdent EntityIDs in PVD, and "*ImgBurn", "*Linux UDFFS", "*Sony", "*Panasonic", "*Pioneer", "*AnyDVD HD" (strings from prior wave, not verified in a source here). A commercial pressing usually shows a studio/replicator tool ID; a home rip usually shows ImgBurn, AnyDVD, DVD Decrypter or a Linux mkudffs/mkisofs pass. Do not treat these as proof.

## 3. Placement of BDMV/VIDEO_TS metadata inside the image

- VRS starts at 32 KB (0x8000). AVDP at sector 256 = 0x80000 (512 KB). Both are inside the first 700 KB article only if the article starts at the image start; the AVDP at 512 KB is inside the first 1 MB. [MEMORY for the sector numbers; computed from sector size 2048]
- Observed layout of authored BD ISOs: UDF structures, then BDMV/ small files, then STREAM/*.m2ts [MEMORY, not verified on a sample]. The DVD order is VIDEO_TS.IFO, VIDEO_TS.BUP, VTS_01_0.IFO, then VOBs [MEMORY]. Atlas must not assume this. Verify with a real sample ISO before relying on it.
- If the UDF file entries sit in the first few MB, a 2 MB read yields the full directory tree with m2ts sizes. This is the single most useful check to make on a real ISO. [UNVERIFIED]

## 4. Blu-ray playlist and clip layouts

### MPLS (verified only in a third-party parser, libfreemkv 0.31.1, read from source)

Header (BE): "MPLS" @0, version @4 ("0200" BD, "0300" UHD) [VERIFIED-SUMMARY: "0300" for UHD], PlayList_start @8 (u32), PlayListMark_start @12 (u32), ExtensionData_start @16 (u32) [libfreemkv 0.31.1 reads only magic, version, playlist_start @8, mark_start @12; extension is not parsed].

PlayList section at PlayList_start: length u32 @0, reserved u16 @4, number_of_PlayItems u16 @6, number_of_SubPaths u16 @8, PlayItems from @10. [VERIFIED-SUMMARY]

PlayItem (offsets relative to the start of the item's 2-byte length field, item[0..1] = length) [VERIFIED-SUMMARY for the table; libfreemkv 0.31.1 counts from after the length, so add 2]:

| Off (from length) | Len | Field |
|---|---|---|
| 0 | 2 | PlayItem length |
| 2 | 5 | Clip_Information_file_name (e.g. "00001") -> BDMV/STREAM/00001.m2ts and BDMV/CLIPINF/00001.clpi |
| 7 | 4 | Clip_codec_identifier ("M2TS") |
| 11 | 2 | reserved 11 bits, is_multi_angle 1 bit, connection_condition 4 bits (exact bit position disputed between sources) |
| 13 | 1 | ref_to_STC_id |
| 14 | 4 | IN_time (45 kHz) |
| 18 | 4 | OUT_time (45 kHz) |
| 22 | 8 | UO_mask_table |
| 30 | 1 | PlayItem_random_access_flag (bit 7) + reserved |
| 31 | 1 | still_mode |
| 32 | 2 | still_time (only when still_mode = 1 per spec) [MEMORY] |
| 34 | var | STN_table (parsed only for the first PlayItem in libfreemkv) |

Duration = (OUT - IN) / 45000 seconds. The two sources disagree on connection_condition offset (libfreemkv: item[9] low nibble with item counted after length; other summary: offset 11). Confirm with libbluray mpls_parse.c.

STN_table (relative to STN start, verified in libfreemkv 0.31.1 and consistent with 0.8.0 summary):
- +0 length u16, +2 reserved u16
- +4 n_video, +5 n_audio, +6 n_pg, +7 n_ig, +8 n_sec_audio, +9 n_sec_video, +10 n_pip_pg, +11 n_dv (one byte each) [VERIFIED-SRC]
- +16 first stream entry (libfreemkv: "item offset 48" with item counted after length)

Stream entry: se_len u8 @0; stream type u8 @1 (1 = PlayItem clip, 2 = SubPath SubClip, 3 = InMux, 4 = DV EL); PID u16 BE at +2 for type 1, +4 for type 2 (after subpath_id and subclip_id), +3 for types 3 and 4 (libfreemkv, untested by its own tests). Then sa_len u8 at se_end; coding_type u8 at sa[0]; format-specific attributes after that. [VERIFIED-SRC, type 2-4 offsets not validated by libfreemkv tests]

Stream attributes:
- Video (0x02, 0x1B, 0x24, 0xEA): sa[1] high nibble = video_format, low nibble = frame_rate. For HEVC 0x24 with sa_len >= 3: sa[2] high nibble = dynamic_range_type, low nibble = color_space. Further HDR bits (cr_flag, hdr_plus_flag) are not in libfreemkv; they are documented in the AACS/BD spec (not read here) [MEMORY]. [VERIFIED-SRC for libfreemkv bit use]
- Audio (0x80-0x86, 0xA1, 0xA2): sa[1] high nibble = channel layout, low nibble = sample rate; language = sa[2..4] (3 bytes ASCII ISO-639-2) [VERIFIED-SUMMARY].
- PG (0x90) and IG (0x91), text subtitle (0x92): language sa[1..3] [VERIFIED-SUMMARY].

Code tables:

| Table | Values | Source |
|---|---|---|
| Coding type | 0x02 MPEG-2, 0x1B AVC, 0x24 HEVC, 0xEA VC-1 | summary + MEMORY |
| Coding type audio | 0x80 LPCM, 0x81 AC-3, 0x82 DTS, 0x83 TrueHD, 0x84 E-AC-3, 0x85 DTS-HD HR, 0x86 DTS-HD MA, 0xA1 E-AC-3 secondary, 0xA2 DTS-HD HR secondary | summary + MEMORY |
| Coding type subtitle | 0x90 PGS, 0x91 IG (interactive), 0x92 text subtitle | summary (0.8.0 lists 0x91 as PGS; MEMORY says IG) |
| video_format | 1 480i, 2 576i, 3 480p, 4 1080i, 5 720p, 6 1080p, 7 576p, 8 2160p | summary + MEMORY |
| frame_rate | 1 23.976, 2 24, 3 25, 4 29.97, 6 50, 7 59.94 (5 undefined in sources) | VERIFIED-SUMMARY |
| audio channel layout | 1 mono, 3 stereo, 6 5.1, 12 7.1 | VERIFIED-SUMMARY |
| audio sample rate | 1 48 kHz, 4 96 kHz, 5 192 kHz, 12 48/192 combo, 14 48/96 combo | VERIFIED-SUMMARY |
| dynamic_range_type (HEVC) | 0 SDR, 1 HDR10, 2 Dolby Vision | libfreemkv 0.31.1 only, UNVERIFIED; HDR10+ and other values not given |
| color_space (HEVC) | 0 unknown, 1 BT.709, 2 BT.2020 | libfreemkv only, UNVERIFIED |

### CLPI (clip info)

Not read. The 0.8.0 summary says CLPI holds EP map and sector extents and points to a separate clpi.md that was not fetched. [UNVERIFIED]

## 5. id.bdmv and BD disc ID

- id.bdmv (BDMV/ROOT ID file): the libbluray bdid_parse.c content was not read. Header is expected to start with "DISCINFO" and version "0100" [MEMORY]. The disc_id field is 16 bytes at a fixed offset [MEMORY, offset not confirmed].
- Whether disc_id is per pressing or per title: it is the AACS Disc ID, which is derived from the disc's Media Key Block and Volume ID [MEMORY]. Treat it as unique per disc content set, not per pressing. Verify with AACS spec 0.953 (listed in prior wave, not fetched) and the libbluray bdid_parse.c source.
- Atlas use: id.bdmv disc_id can dedupe rips of the same disc. It is not a reliable pressing ID. [MEMORY]

## 6. DVD IFO layouts

From oxideav-dvd ifo.rs summary [VERIFIED-SUMMARY]; all multi-byte fields are BE.

VMG (VIDEO_TS.IFO):
- 0x000 "DVDVIDEO-VMG" (12); 0x00C last sector of VMG set (4); 0x01C last sector of IFO (4); 0x020 version (2); 0x022 VMG category (4)
- 0x026 number of volumes (2); 0x028 volume number (2); 0x02A side ID (1); 0x03E number of title sets (2); 0x040 provider ID (32)
- 0x0C4 TT_SRPT sector (4) (absolute LBA per summary); 0x0C8 VMGM_PGCI_UT; 0x0D8 VMGM_C_ADT; 0x0DC VMGM_VOBU_ADMAP

VTS (VTS_xx_0.IFO):
- 0x000 "DVDVIDEO-VTS"; 0x00C last sector; 0x01C last sector of IFO; 0x0C0 menu VOB sector; 0x0C4 title VOB start sector; 0x0C8 VTS_PTT_SRPT; 0x0CC VTS_PGCI

Attribute blocks in VTSI_MAT (title block; menu block in parentheses):
- 0x200 (0x100) video attributes, 2 bytes
- 0x202 (0x102) audio stream count u16
- 0x204 (0x104) audio attributes, 8 x 8 bytes
- 0x254 (0x154) subpicture stream count u16
- 0x256 (0x156) subpicture attributes, 32 x 6 bytes (menu: 1 x 6)
- 0x318 multichannel extension, 24 x 8 bytes to 0x3D8

Video attribute (2 bytes):
- byte0: bits 7-6 coding (00 MPEG-1, 01 MPEG-2) = codec; bits 5-4 standard (00 NTSC, 01 PAL); bits 3-2 aspect (00 4:3, 11 16:9)
- byte1: bits 5-3 resolution index (0 720x480/576, 1 704x480/576, 2 352x480/576, 3 SIF)

Audio attribute (8 bytes):
- byte0: bits 7-5 coding (0 AC-3, 2 MPEG-1, 3 MPEG-2 ext, 4 LPCM, 6 DTS); bits 3-2 language type; bits 1-0 app mode
- byte1: bits 7-6 quantisation (LPCM 0/1/2 = 16/20/24 bit); bits 5-4 sample-rate code (0 = 48 kHz); bits 2-0 channels - 1
- bytes 2-3 ISO-639 language; byte 5 code extension

Subpicture attribute (6 bytes): byte0 bits 7-5 coding (0 = RLE); bits 1-0 language type; bytes 2-3 language; byte 5 code extension.

No bitrate field exists in IFO. DVD bitrate must come from VOB/MPEG PES scan. [VERIFIED-SUMMARY: no bitrate in the layouts]

DVD disc ID (libdvdread DVDDiscID): not read (dvd_reader.c blocked). MEMORY: an MD5 over the IFO files, with the exact file set and byte range unconfirmed. Do not implement from this note.

## 7. META (bdmt_eng.xml)

BDMV/META/DL/bdmt_eng.xml with di:discinfo and di:name, numSets, thumbnails (META/DL/thumb) [MEMORY]. Present on most commercial BD releases, not all. Not verified on a sample.

## Gaps and conflicts

1. libbluray mpls_parse.c, clpi_parse.c, bdid_parse.c and libdvdread dvd_reader.c could not be read (code.videolan.org blocked; GitHub clone blocked). These are the authoritative sources for items 4-6.
2. The two libfreemkv summaries disagree on the connection_condition offset and on the STN start offset convention. Only libfreemkv 0.31.1 code was read directly.
3. HDR fields beyond dynamic_range_type and color_space (cr_flag, hdr_plus_flag) are not confirmed by any source read.
4. ECMA-167 tag IDs, LVInfo layout and FSD offsets are from memory.
5. No real ISO sample was downloaded, so the placement question (item 3) is unverified.

## Source URLs
- https://docs.rs/crate/libfreemkv/0.31.1/source/src/mpls.rs (read)
- https://docs.rs/crate/libfreemkv/0.8.0/source/docs/mpls.md (read, summary)
- https://docs.rs/oxideav-dvd/0.0.3/src/oxideav_dvd/ifo.rs.html (read, summary)
- https://raw.githubusercontent.com/torvalds/linux/master/fs/udf/ecma_167.h (read)
- https://raw.githubusercontent.com/libcdio/libcdio/master/include/cdio/iso9660.h (read)
- https://code.videolan.org/videolan/libbluray (blocked, 403)
- https://www.videolan.org/developers/libbluray.html (read, no codes)
