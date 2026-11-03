# W9a: Atlas archive/PAR2/SFV/SRR metadata gap table

Scope: read-only review of the Atlas copy at `/mnt/user-data/uploads/git.xbmc4lyfe/Atlas-to-Rust/src/` (only `src/` is present: `probe/archive.rs`, `probe/mod.rs`, `probe/step.rs`, `archive/mod.rs`, `archive/rar.rs`, `archive/sevenz.rs`, `archive/zip.rs`, `archive/steps.rs`, `archive/store.rs`, `par2.rs`). Not in the copy, so not verified here: `archive/names.rs` (`clean_name`, `best_name`), `archive/select.rs`, `archive/testkit.rs`, `media` module, `duplicates` module (`SET_CONTENT`, `SET_PAR2_SIZES`, `twin_fingerprint`), `is_small_text`. Line numbers below are from the copy as read.

Status legend:
- **stored**: persisted. Named as a column of `archive_release` / `archive_inner_files` / `release_nfo_password`, or a field of `ProbeInfo` / `ArchiveHead` serialized as JSON into `release_probe.info` (via `SET_PROBE`, `probe/mod.rs`).
- **parsed**: Atlas code reads it and then drops it. Function and file:line given.
- **no**: not read at all.

Note on persistence: `ArchiveHead` derives `Serialize`, so `format`, `encrypted`, `headers_encrypted`, `method`, `name`, `unpacked`, `packed`, `crc32` are in `release_probe.info` JSON. `stored_at` and `continues` are `#[serde(skip)]` (probe/archive.rs:23-28), so they are not persisted. The probe `ArchiveHead` only describes the FIRST article of the release's main file, never a whole listing.

---

## 0. Summary of the big gaps

1. **Per-file CRC32 is thrown away for all four container formats.** RAR5 reads it and skips it in the listing path (archive/rar.rs:265-267). RAR4 never reads it in the listing path. ZIP central-directory CRC is never read (zip.rs:298-333). 7z substream digests are read and discarded (sevenz.rs:299-304). PAR2 IFSC is not parsed at all.
2. **Timestamps, host OS and attributes are never kept.** RAR4 DOS time/host/attr, RAR5 mtime/attributes/host (skipped at rar.rs:262-269), ZIP DOS time, version-made-by, external attributes, and 7z kMTime/kCTime/kATime/kWinAttributes (skipped at sevenz.rs:363) are all unstored.
3. **Packer-fingerprint fields are dropped.** PAR2 Creator packet (no branch in par2.rs:93-124), SFV `;` generator comments (probe/mod.rs:189 skips them), ZIP EOCD comment, RAR5 main/extra archive metadata, RAR5 `version`/`owner`/`redirection` extras, SRR app name. None are stored.
4. **SRR has no parser at all.** No `srr` string or block code exists in the copy.
5. **7z probe gives almost nothing.** `probe/archive.rs:37` returns only `format: "7z"`. Names, sizes, encryption and method come only from the listing path (archive/sevenz.rs), and only for the listing path's header.
6. **7z encoded headers: LZMA only.** sevenz.rs:513-516 requires coder id `030101`. A header packed with LZMA2 (`21`), BCJ chains, or other coders returns None, which marks the archive damaged.
7. **RAR4 unknown-size bug.** `0xFFFFFFFF` without LHD_LARGE means "size unknown" (per unrar). Atlas stores it as 4 GiB: probe/archive.rs:75 and rar.rs:109 have no filter. RAR5 does filter (rar.rs:284, probe/archive.rs:152).
8. **RAR4 unicode name in the probe is the ASCII fallback only.** probe/archive.rs:79-83 splits at the NUL and keeps the ASCII half. The listing path decodes it (rar.rs:115-173). The two names can differ.
9. **Stale encryption columns.** steps.rs:141-152 upserts only `password` and writes `encryption` / `names_hidden` / `password_check` only when `listing.encryption` is Some (store.rs:70-73). A later listing with no encryption leaves the old values in place.
10. **SFV starvation.** The probe's small-file loop takes only the first 3 small text files (probe/step.rs run(), the `.take(3)` loop, about line 108). A release with several `.nfo`/`.txt`/`.srr` files can push the `.sfv` out.
11. **Listing view capped at 256 rows.** `rows()` (store.rs:93) reads `limit 256`, though up to `MAX_ENTRIES` = 4096 are stored.
12. **Listing is bounded by the bytes fetched.** RAR has no tail read. Its walk stops at the end of the buffer (rar.rs:79-84 for RAR4, rar.rs:229-232 for RAR5), so only headers inside the first article (or 64 KiB in probe mode) are listed.

---

## 1. Byte budgets and truncation per parser

| Parser | Bytes read | Where | Truncation behaviour |
|---|---|---|---|
| `probe::archive::parse` (ArchiveHead) | The whole first article of the main file, up to `ARTICLE_CAP` = 4 MiB | probe/step.rs `fetch_raw` (~line 53-61, uses `archive::ARTICLE_CAP`), `inspect()` at probe/step.rs:354-364 | Fields become None. No CRC check on RAR headers. Walks only to the first file block (RAR4 `break` at probe/archive.rs:89; RAR5 `return` at probe/archive.rs:162). |
| `archive::rar::list` (RAR4/RAR5 listing) | Probe-flag file: 64 KiB prefix (`PROBE_PREFIX`, steps.rs:63-70). Other: whole first article, up to 4 MiB (steps.rs:54-61). No tail read. | steps.rs:405-406 | Walk ends silently at buffer end (rar.rs:79-84, rar.rs:229-232). `complete` only with the end marker (RAR4 `0x7b` rar.rs:60-63; RAR5 kind 5 rar.rs:257). `damaged` only on CRC or structure failure. |
| `archive::zip::list_head` | Same as above (first article or prefix) | zip.rs:209-254 | Stops on a central-directory signature or a bit-3 entry (zip.rs:244-246: packed==0 stops the walk). |
| `archive::zip::list_tail` | Last article of the file (`tail_articles`), plus a budgeted read via `budgeted()`. Window for EOCD search: `END_SEARCH` = 65,557 bytes (zip.rs:11, 259). | steps.rs:317-327 | If the CD start is outside the tail, falls back to scanning the tail for `PK\x01\x02` (zip.rs:291-297) with `complete = false` (zip.rs:335). |
| `archive::sevenz::list` (7z) | Start header (32 bytes). Next header up to `MAX_NEXT` = 16 MiB (sevenz.rs:7, 472). Encoded header decoded up to `MAX_DECODED` = 16 MiB (sevenz.rs:8, 439). Tail: last article, then the previous adjacent article (2 max). | steps.rs:330-365 | `need_more_tail` set when the window is shorter than the header (sevenz.rs:482-485). Bad header CRC gives damaged. Tail window assumes the header ends at the window end (sevenz.rs:482, `window.len() - size`). |
| `par2::inventory` | First article, plus the second article (base file) or the set's last article (volume) (steps.rs:216-234). Resync on `PAR2\0PKT` (par2.rs:70-77). | steps.rs:212-234 | `truncated` flag set only for an oversize packet (par2.rs:83). Caps: `MAX_FILES` = 4096 (par2.rs:24). |
| `parse_sfv` (SFV) | First article of up to 3 small text files, yEnc-decoded (probe/step.rs run loop). | probe/step.rs `take(3)` | Only the first article is read, so an SFV longer than one article is cut silently. |

### 1.1 Format detection
- `probe::archive::parse` (probe/archive.rs:31-43) checks RAR5, RAR4, 7z and ZIP by magic. `archive::identify` (archive/mod.rs:51-88) also handles PAR2, images and media.
- RAR4 and RAR5 are distinguished by marker length (7 vs 8 bytes), which is correct.
- SFX RARs (marker not at offset 0) are not recognised. The prototype `find_sig` (rar_meta.rs:219) scans for the marker.

### 1.2 Truncation handling, per parser
- ArchiveHead: returns partial fields as `None`. Safe.
- RAR list: "what isn't there yet is truncation, not damage" (rar.rs:33). Good.
- ZIP: `list_tail` uses `checked_*` everywhere. Good.
- 7z: several `checked_sub` and `checked_add` guards (sevenz.rs:288, 475, 523). Good. The header-size guard is applied before allocation (sevenz.rs:338, 212, 236).
- PAR2: explicit `is_multiple_of(4)` and `len >= 64` check before using the length (par2.rs:79-86). Good.

---

## 2. RAR4 (w1a §8-13)

Atlas code: probe `rar4()` probe/archive.rs:53-100; listing `list4()` / `file4()` / `name4()` / `unicode4()` archive/rar.rs:29-173.

| Field (research id) | Atlas status | Where / note |
|---|---|---|
| r4_head_crc (@0) | parsed | rar.rs:44-51 checks CRC16 for 0x73/0x74/0x7a, then dropped. probe does not check. |
| r4_head_type (@2) | parsed | probe/archive.rs:61; rar.rs:35. Dropped. |
| r4_head_flags (@3) | parsed, partly stored | Only bits used: 0x0080 stored as `headers_encrypted` (probe/archive.rs:63-66; rar.rs:53-59); 0x0004 stored as `encrypted` (probe/archive.rs:84; rar.rs:104-107); 0x0002 → `continues` (probe/archive.rs:85, serde-skip, not stored); 0x8000 ADD_SIZE used for skipping. 0x0001, 0x0008, 0x0010, 0x0100, 0x0200, 0x0400, 0x1000 = no. |
| r4_head_size (@5) | parsed | Used as block length (probe/archive.rs:56-57; rar.rs:38-39). Dropped. |
| r4_add_size (@7 for LONG_BLOCK) | parsed | probe/archive.rs:93-94; rar.rs:65-71. Dropped. |
| r4_main_reserved | no | Legacy. Acceptable. |
| r4_main_flags: MHD_VOLUME 0x0001 | no | Not read. |
| MHD_SOLID 0x0008 | no | Not read. Needed for the "solid" flag. |
| MHD_NEWNUMBERING 0x0010 | no | Not read. Naming scheme. |
| MHD_PROTECT 0x0040 | no | Not read. RR present. |
| MHD_FIRSTVOLUME 0x0100 | no | Not read. |
| MHD_ENCRYPTVER 0x0200 | no | Not read. |
| MHD_PASSWORD 0x0080 | stored | `headers_encrypted` (archive_release.names_hidden via steps.rs:148). |
| r4_main_comment_inline (MHD_COMMENT 0x0002) | no | Legacy. |
| r4_file_pack_size_low (@7) | stored (probe), parsed (listing) | probe/archive.rs:74 → `ArchiveHead.packed` in release_probe.info. Listing: rar.rs:73-74, used only to skip. |
| r4_file_high_pack_size (@32, LARGE) | stored (probe), parsed (listing) | probe/archive.rs:74 `high_word`; rar.rs:93-94. |
| r4_file_unp_size_low (@11) | stored | probe/archive.rs:75 → `ArchiveHead.unpacked`; listing rar.rs:109-110 → `archive_inner_files.size`. **Bug: no 0xFFFFFFFF unknown-size filter.** |
| r4_file_high_unp_size (@36, LARGE) | stored | probe/archive.rs:75; rar.rs:109. |
| r4_file_host_os (@15) | no | Not read. |
| r4_file_crc32 (@16) | stored (probe, first file only) | probe/archive.rs:76 → `ArchiveHead.crc32`. Listing path: **no** (rar.rs `file4` never reads @16). |
| r4_file_dos_time (@20) | no | Not read. |
| r4_file_unp_ver (@24) | no | Not read. |
| r4_file_method (@25) | stored (probe) | probe/archive.rs:77-78 → `ArchiveHead.method` "m0".."m5". Listing: no. |
| r4_file_name_size (@26) | parsed | probe/archive.rs:79; rar.rs:92. Dropped. |
| r4_file_attr (@28) | no | Not read. |
| r4_file_name (@32/@40) | stored | probe: ASCII part only (probe/archive.rs:79-83). Listing: `name4` (rar.rs:115-123) and `unicode4` (rar.rs:126-173) decode the unicode form; stored to `archive_inner_files.name`. Unicode decoder matches unrar's `DecodeFileName` command set (0/1/2/3). |
| r4_file_salt (LHD_SALT 0x0400) | no | Not read. |
| r4_file_exttime (LHD_EXTTIME 0x1000) | no | Not read. Prototype `rar4_exttime` (rar_meta.rs:364) handles it. |
| r4_file_version_flag | no | Not read. |
| r4_sub_name / sub_data / sub_stm_name / sub_uow (NEWSUB 0x7a) | no | Skipped by ADD_SIZE (rar.rs:65-71). Zone.Identifier and QO/RR not read. |
| r4_protect (PROTECT 0x78): version, rec_sectors, total_blocks | no | Skipped by ADD_SIZE. Recovery percent not stored. |
| r4_endarc (0x7b): flags, volnum/CRC | parsed | rar.rs:60-63 sets `complete = true`. Used by steps.rs:129 (content fingerprint). Flags and volume number dropped. |
| r4_cmt_old_block (0x75), r4_cmt_new | no | Not read. |
| Encryption (AES-128 for RAR4) | stored | `archive_release.encryption` = 1 (archive/mod.rs:129; steps.rs:142-152). |
| Password state | stored | `archive_release.password` (store.rs:70). |

### 2.1 RAR4 bugs and limits
- **Unknown size** (probe/archive.rs:75, rar.rs:109): `0xFFFFFFFF` without flag 0x0100 must become None. RAR5 already does this at rar.rs:284 and probe/archive.rs:152.
- **Unicode name in the probe** (probe/archive.rs:79-83): use the same decoder as the listing (rar.rs `name4`). Make `name4`/`unicode4` `pub(crate)` and call them from `probe::archive::rar4`.
- **Listing CRC** (rar.rs `file4`, line 90-113): read `le32(header, 16)` and carry it on `InnerFile` (see §8).
- **Main header**: `list4` only checks 0x0080 at rar.rs:53. Flags 0x0008 SOLID, 0x0010 NEWNUMBERING, 0x0100 FIRSTVOLUME, 0x0040 PROTECT are not stored.
- **Host OS, DOS time, attributes** at @15, @20, @28: skipped entirely.

---

## 3. RAR5 (w1a §1-7)

Atlas code: probe `rar5()` probe/archive.rs:115-172, `has_crypt_record()` probe/archive.rs:174-185; listing `list5()` / `block5()` / `password_check()` archive/rar.rs:192-406.

| Field (research id) | Atlas status | Where / note |
|---|---|---|
| r5_hdr_crc | parsed | Listing checks CRC32 (rar.rs:216-219). Probe reads and skips (probe/archive.rs:119). |
| r5_hdr_size (vint, ≤3 bytes) | parsed | rar.rs:197-214 (listing). probe/archive.rs:121. Dropped. |
| r5_hdr_type | parsed | Dispatch only. |
| r5_hdr_flags: 0x0001 extra, 0x0002 data | parsed | probe/archive.rs:125-126; rar.rs:242-244. Dropped. |
| r5_hdr_flags: 0x0010 SPLIT_AFTER | parsed, not stored | probe/archive.rs:138 → `continues` (serde-skip). |
| r5_hdr_flags: 0x0008 SPLIT_BEFORE, 0x0004 skip-if-unknown, 0x0020 CHILD, 0x0040 INHERITED | no | Not read. |
| r5_extra_area_size | parsed | probe/archive.rs:125; rar.rs:243. Dropped. |
| r5_data_size | stored (probe) | probe/archive.rs:136 → `ArchiveHead.packed`. Listing: rar.rs:244 returned as skip only. |
| r5_data_area_start | parsed | probe/archive.rs:160 → `stored_at` (serde-skip). |
| Main (kind 1): r5_main_extra_size | no | Probe never handles kind 1 (probe/archive.rs:129-166). Listing: `block5` has no ty 1 arm (rar.rs:249-290). |
| r5_main_archive_flags: 0x0001 VOLUME, 0x0002 VOLNUM, 0x0004 SOLID, 0x0008 RR | no | Not read. |
| r5_main_volume_number | no | Not read. |
| r5_main_extra_locator (type 1: QO/RR offsets) | no | Not read. |
| r5_main_extra_metadata (type 2: archive name, ctime) | no | Not read. |
| r5_crypt_version (kind 4) | parsed | `_version` discarded (rar.rs:362). |
| r5_crypt_flags (check present bit) | parsed | rar.rs:363-365 gates the check. |
| r5_crypt_kdf_count | stored | `PasswordCheck.kdf` in `archive_release.password_check` BLOB (rar.rs:294-320; store.rs:72-73). |
| r5_crypt_salt | stored | Same BLOB. |
| r5_crypt_check (8 bytes + 4 sum) | stored | Same BLOB. Sum verified (rar.rs:372-373). |
| r5_encrypted_header_iv | parsed | Used for file records only (rar.rs:370). Header IV not kept (not needed). |
| r5_file_extra_size | parsed | probe/archive.rs:125; rar.rs:243. |
| r5_file_data_size | stored (probe) | probe/archive.rs:136. |
| r5_file_flags: 0x0001 dir | used | rar.rs:283 skips dirs. Not stored. |
| r5_file_flags: 0x0002 mtime present | parsed | Skipped (probe/archive.rs:142-144, rar.rs:262-264). |
| r5_file_flags: 0x0004 CRC present | parsed (listing skipped) | probe/archive.rs:145-148 stores crc (stored, probe only). Listing: rar.rs:265-267 skips it. |
| r5_file_flags: 0x0008 unpacked unknown | parsed | Used to null the size: probe/archive.rs:152; rar.rs:284. Not stored. |
| r5_file_unpacked_size | stored | `archive_inner_files.size` (rar.rs:284-285 → steps.rs INSERT_FILE); probe `ArchiveHead.unpacked` (probe/archive.rs:140, 152). |
| r5_file_attributes | parsed | `_attributes` (rar.rs:261; probe/archive.rs:141). Dropped. |
| r5_file_mtime_unix (uint32) | no | Skipped by `p += 4` (probe/archive.rs:143; rar.rs:262-264). |
| r5_file_crc32 | stored (probe), no (listing) | probe/archive.rs:146 → `ArchiveHead.crc32`. Listing: skipped (rar.rs:265-267). |
| r5_file_comp_info: version (bits 0-5) | no | Not stored. probe/archive.rs:149 reads `info`, uses only the method. |
| r5_file_comp_info: solid (bit 6) | no | Not stored. |
| r5_file_comp_info: method (bits 7-9) | stored (probe) | probe/archive.rs:155-156 → `ArchiveHead.method` "m0".."m5". Listing: `_compression` dropped (rar.rs:268). |
| r5_file_comp_info: dict (bits 10-14) | no | Not stored. |
| r5_file_host_os | parsed | `_host` (rar.rs:269; probe/archive.rs:150). Dropped. |
| r5_file_name_len / name | stored | probe/archive.rs:151-154 → `ArchiveHead.name`; listing rar.rs:270-286 → `archive_inner_files.name`. |
| r5_service_name (kind 3) | no | Service blocks skipped (probe/archive.rs:164-166; rar.rs:258-288 treat kind 3 for the crypt record only). |
| r5_endarc (kind 5) | parsed | Sets `complete` (rar.rs:257). Flags and trailer not read. |
| r5_rr_block / r5_qo_block | no | Not read. |
| Extra 0x01 crypt: version | no | `has_crypt_record` (probe/archive.rs:174-185) only checks the type id. |
| Extra 0x01 crypt: flags 0x0001 check present | stored | rar.rs:296-304 (`password_check`) → BLOB. |
| Extra 0x01 crypt: flags 0x0002 TWEAKED | no | Not read. The CRC is then not a content fingerprint. |
| Extra 0x01 encryption (presence) | stored | rar.rs:277-282 → `encryption` = Rar5Aes256 (archive_release.encryption = 2). probe/archive.rs:157-158 → `ArchiveHead.encrypted`. |
| Extra 0x02 hash (BLAKE2sp) | no | Not read. Strong content identity. |
| Extra 0x03 time (mtime/ctime/atime, ns) | no | Not read. |
| Extra 0x04 version | no | Not read. |
| Extra 0x05 redirection (symlink/hardlink) | no | Not read. Matters for "largest file" heuristics. |
| Extra 0x06 unix owner (name, uid, gid) | no | Not read. Packer identity leak. |
| Extra 0x07 service data | no | Not read. |
| Any extra type other than crypt | no | Ignored. |

### 3.1 RAR5 bugs and limits
- The probe's extra walk only tests for type 1 (probe/archive.rs:174-185). Other extras are never parsed. `has_crypt_record` also treats the first matching record as the answer and ignores the 0x0002 TWEAKED flag.
- Main header (kind 1) is not handled in either path. Volume, solid and RR flags are therefore never read, so `archive_release` cannot say whether the release is solid or multi-volume.
- Listing does not keep the CRC (rar.rs:265-267), so `archive_inner_files` has no per-file checksum even though the header has one.
- `list5` size check: `size_len == 3` damaged (rar.rs:206-209). Correct for a 3-byte header-size vint.

---

## 4. 7z (w1b §3-11)

Atlas code: probe `parse()` probe/archive.rs:36-37 (signature only). Listing: archive/sevenz.rs (`start`, `number`, `Reader`, `folder`, `streams`, `names`, `files`, `header`, `decode`, `list`, `run`, `nested`). Steps: archive/steps.rs `seven_list` (330-365).

| Field (research id) | Atlas status | Where / note |
|---|---|---|
| Signature (0-5) | stored (format) | probe/archive.rs:36-37 → `ArchiveHead.format` "7z". |
| Version major (6) | parsed | Checked == 0 (sevenz.rs:37). Dropped. |
| Version minor (7) | no | Not read. |
| StartHeaderCRC (8-11) | parsed | Checked over bytes 12-31 (sevenz.rs:40). Dropped. |
| NextHeaderOffset (12-19) | parsed | `Start.offset` (sevenz.rs:43, 472-479). Dropped. |
| NextHeaderSize (20-27) | parsed | `Start.size`. Dropped. |
| NextHeaderCRC (28-31) | parsed | `Start.crc`, checked against the header (sevenz.rs:487). Dropped. |
| kEncodedHeader 0x17 (first byte of block) | parsed | sevenz.rs:500-504 (`nested`). Dropped. |
| Encoded header AES (header encrypted) | stored | `archive_release.names_hidden` = 1, `encryption` = 3 (steps.rs:142-152 via `listing.headers_encrypted`). Not in probe `ArchiveHead`. |
| Encoded header coder: LZMA `030101` | parsed | Used to decode (sevenz.rs:513-535). Dropped. |
| Encoded header coder: LZMA2 `21` | no (fails) | sevenz.rs:513-516 requires `LZMA` id, else None (damaged). |
| Encoded header coder: BCJ, Delta, PPMd, Copy, others | no (fails) | Same as above. |
| PackInfo pack_pos, sizes (0x06) | parsed | sevenz.rs:207-230 (used for decode, dropped). |
| PackInfo CRCs (0x0a) | parsed | `r.digests(count)?` return dropped (sevenz.rs:224). |
| UnpackInfo folder count, folder coders (id, props) (0x07/0x0b) | parsed | `Folder.coders` (sevenz.rs:126, 184). Used only for the AES flag (sevenz.rs:181-183) and the LZMA check (513). Not persisted. |
| Coder props: LZMA 5 bytes (dict, lc/lp/pb) | no | Only `props.len()==5` checked. Passed to the decoder, not kept. |
| Coder props: AES (salt, iv, cycles) | no | Not parsed. AES flag only. |
| Folder bind pairs, packed-stream indices | parsed | sevenz.rs:191-201. Dropped after the output size is computed. |
| Folder unpack size (0x0c) | parsed | sevenz.rs:245-249 → `Folder::size` (sevenz.rs:135-139). Used for sizes. |
| Folder CRC (0x0a inside 0x07) | parsed | sevenz.rs:254-257 → `Folder.crc`. Used only to verify the decoded header (sevenz.rs:534-536). |
| SubStreams NumUnpackStream (0x0d) | parsed | sevenz.rs:267-274 → `per_folder`. Dropped. |
| SubStreams sizes (0x09) | stored (via listing) | sevenz.rs:279-292 → `Streams.sizes` → `archive_inner_files.size`. |
| SubStreams digests (0x0a): per-file CRC32 | parsed, discarded | sevenz.rs:299-304 `r.digests(count)?` return unused. **Free per-file CRC32 is thrown away.** |
| kEmptyStream 0x0e | parsed | sevenz.rs:350, 369. Dropped after use. |
| kEmptyFile 0x0f | parsed | sevenz.rs:352-353, 370. Dropped after use. |
| kAnti 0x10 | no | Skipped at sevenz.rs:363 (`_ => {}`). Anti-items (deletions in update archives) are then listed as files. |
| kName 0x11 | stored | sevenz.rs:355-362 → `archive_inner_files.name`. |
| kCTime 0x12 | no | Skipped (sevenz.rs:363). |
| kATime 0x13 | no | Skipped. |
| kMTime 0x14 | no | Skipped. |
| kWinAttributes 0x15 (+ unix mode in high 16 bits) | no | Skipped. Needed for dir and symlink detection. |
| kComment 0x16 | no | Skipped. |
| kStartPos 0x18 | no | Skipped. |
| kDummy 0x19 | no | Skipped (correct). |
| Empty-stream file size (0 for empty files) | stored | sevenz.rs:379 pushes `Some(0)`. |
| Solid block info (files per folder) | no | Per-folder counts parsed (`per_folder`) but no solid flag stored. |
| Multi-volume `.7z.001` | not applicable | Handled by `seven_list` joining articles (steps.rs:353-364). |

### 4.1 7z bugs and limits
- **Probe is signature-only** (probe/archive.rs:37). `ArchiveHead` for a 7z has no name, size, method, encryption. The listing path has all of this but never writes the probe fields.
- **Substream CRC discarded** (sevenz.rs:299-304). Keep the returned digests and attach them to the files (see §8).
- **kAnti treated as a file** (sevenz.rs:363 falls through to `l.push` at sevenz.rs:375-389 for non-empty streams and for empty files). Skip anti-items.
- **Encoded header: LZMA only** (sevenz.rs:513-516). LZMA2 (`21`) and filter chains fail. Several writers use LZMA2 for headers, so this is a real loss, not a corner case. `lzma_rs` provides an LZMA2 decoder (verify the API in the pinned version).
- **Tail window assumption** (sevenz.rs:479-482): `next_at = window.len() - size` assumes the window ends at the header end. This holds for the last article only. Compute `len - at` instead.
- **Start header minor version** not kept (sevenz.rs:37).
- **Method chain not kept.** `Folder.coders` holds the ids and props; persist a short method string such as `LZMA2+BCJ` (see §8).

---

## 5. ZIP (w1c §1)

Atlas code: probe `zip()` probe/archive.rs:187-222. Listing: archive/zip.rs: `list_head` (209-254), `list_tail` (257-337), `list` (340-349), `zip64_sizes` (49-72), `aes_strength` (182-191), `password_check` (194-207), `name_of` (26-34), `is_encrypted` (36-38).

### 5.1 Local file header

| Field (research id) | Atlas status | Where / note |
|---|---|---|
| Signature (0) | parsed | Dispatch. |
| Version needed (4) | no | Not read. |
| GP flag bit 0 encrypted (6) | stored | probe/archive.rs:191 → `ArchiveHead.encrypted`. Listing: zip.rs:36-38, 228. |
| GP flag bit 0x0040 strong, 0x2000 masked | parsed (listing only) | zip.rs:36-38 (`is_encrypted`). Probe does NOT check them (probe/archive.rs:191). |
| GP flag bit 3 (sizes after data) | parsed | zip.rs:206 and 244-246. Probe: probe/archive.rs:207 drops the CRC. |
| GP flag bit 11 (UTF-8) | parsed | zip.rs:27-29 (`name_of`). Not stored. |
| Compression method (8) | stored | probe/archive.rs:189-204 → `ArchiveHead.method` text ("store", "deflate", "deflate64", "bzip2", "lzma", "zstd", "aes", else "other"). **Numeric method lost for "other".** Listing: only method 99 used (zip.rs:40-42). |
| Last mod time (10) | no | Not read. |
| Last mod date (12) | no | Not read. |
| CRC-32 (14) | stored (probe, first entry, when no bit 3) | probe/archive.rs:207 → `ArchiveHead.crc32`. Listing: no (zip.rs:217-237 never reads it). |
| Compressed size (18) | stored (probe, zip64-aware) | probe/archive.rs:210-216 → `ArchiveHead.packed`. Listing: used to skip (zip.rs:235, 247). |
| Uncompressed size (22) | stored | probe/archive.rs:210-216; listing zip.rs:235-237 → `archive_inner_files.size`. |
| Name length (26) | parsed | Used to slice name (zip.rs:226; probe/archive.rs:217). |
| Extra length (28) | parsed | zip.rs:227. |
| Name (30) | stored | probe/archive.rs:217 (lossy UTF-8, no CP437). Listing: zip.rs:236 via `name_of` → `archive_inner_files.name`. |
| Extra: 0x0001 zip64 | parsed, used | zip.rs:49-72 → sizes (stored via listing and probe). |
| Extra: 0x9901 AES | parsed, partly stored | zip.rs:182-191 (strength) → used in `password_check` (zip.rs:197-201) → stored as BLOB in `archive_release.password_check`. Vendor version and actual method (+5) not read. |
| Extra: 0x7075 unicode path, 0x5455 extended timestamp, 0x000a NTFS, 0x000d UNIX, 0x5855 timestamps, 0x6375 unicode comment, 0x0017 strong encryption | no | Not read. |
| Encryption header: traditional 12 bytes (ZipCrypto) | stored | `PasswordCheck::Crypto` (zip.rs:80, 194-206) → `archive_release.password_check` BLOB; encryption code 4 (archive/mod.rs:132). |
| Encryption header: AES salt + verifier | stored | `PasswordCheck::Aes` (zip.rs:82) → BLOB; encryption code 5 (archive/mod.rs:133). |
| Data descriptor (after data, bit 3) | no | Not read. CRC and sizes unavailable when the descriptor is in a later article. |

### 5.2 Central directory and EOCD

| Field (research id) | Atlas status | Where / note |
|---|---|---|
| CD signature | parsed | zip.rs:301-303. |
| Version made by (4): host OS, spec version | no | Not read. Needed for attribute interpretation. |
| Version needed (6) | no | Not read. |
| GP flag (8) | parsed | zip.rs:304-306 → `is_encrypted`. |
| Compression method (10) | parsed, dropped | zip.rs:305. Only used for `is_encrypted` (zip.rs:320). |
| Last mod time/date (12/14) | no | Not read. |
| CRC-32 (16) | no | **Not read.** Per-entry CRC from CD is free. |
| Compressed size (20) | parsed, dropped | zip.rs:306, then `let (unp, _)` discards it at zip.rs:324. |
| Uncompressed size (24) | stored | zip.rs:307 → `zip64_sizes` (zip.rs:324) → `archive_inner_files.size`. |
| Name length (28) | parsed | zip.rs:309 → name (zip.rs:315). |
| Extra length (30) | parsed | zip.rs:310 → extra (zip.rs:316). |
| Comment length (32) | parsed, dropped | zip.rs:311, 317-319 (length check only). |
| Disk number start (34) | no | Not read. Split ZIP unsupported in the walk. |
| Internal attributes (36) | no | Not read. |
| External attributes (38): DOS dir bit, unix mode | no | Not read. **Directory detection uses the name suffix only** (zip.rs:44-46, 326). |
| Local header offset (42) | no | Not read. |
| CD file name (46) | stored | zip.rs:315, 325 → `archive_inner_files.name`. |
| CD extra (46+n) | parsed for zip64 only | zip.rs:316, 324. |
| CD file comment (46+n+m) | no | Length checked (zip.rs:317-319); content not read. |
| EOCD: entries (10), CD size (12), CD offset (16) | parsed | zip.rs:271-297. Used for walk, not stored. |
| EOCD: comment (22..) | no | Not read. The comment often names the packer. |
| Zip64 EOCD record (sizes, entries) | parsed | zip.rs:276-288. |
| Zip64 EOCD locator | parsed (position only) | zip.rs:279 checks the signature. |
| Zip64 EOCD: disk numbers, version fields | no | Not read. |

### 5.3 ZIP bugs and limits
- **Probe encrypted flag incomplete** (probe/archive.rs:191 vs zip.rs:36-38). Strong encryption (0x0040) and masked headers (0x2000) show `encrypted: false` in the probe.
- **Name decoding wrong for CP437.** `name_of` (zip.rs:26-34) falls back to Latin-1 when bit 11 is clear and the bytes are not UTF-8. The spec default is CP437. Non-ASCII names in CP437 come out wrong.
- **Probe name uses lossy UTF-8** (probe/archive.rs:217), with no bit-11 or CP437 handling. Route it through `name_of`.
- **`method` text loses the number** for "other" (probe/archive.rs:201). Store the numeric value too.
- **CD CRC and sizes**: `packed` is parsed then discarded (zip.rs:324). The CD CRC (16) is never read. Both are free.
- **Streamed ZIP** (bit 3, `packed == 0`): the walk stops after the first entry (zip.rs:244-246). By design, but the rest of the listing comes only from the CD tail.
- **EOCD comment** is ignored (zip.rs:257-270 searches for the record but drops the comment).
- **Zip64 central-directory size** (zip.rs:276-288): the `eocd - 76` assumption holds only when the locator and record are immediately before the EOCD. Fine for normal writers; note it.

---

## 6. PAR2 (w1c §3; par2.rs)

Atlas code: `inventory()` par2.rs:61-135; `Par2File` 28-37; `Inventory` 39-49. Steps: `Par2Step::run` steps.rs:212-270; `listing_of` steps.rs:72-76.

| Field (research id) | Atlas status | Where / note |
|---|---|---|
| Packet magic `PAR2\0PKT` (0) | parsed | par2.rs:67-77 (resync scan). |
| Packet length (8) | parsed | par2.rs:78-87. Dropped. |
| Packet MD5 (16) | no | Not verified. |
| Recovery set ID (32) | parsed | `Par2File.set_id` (par2.rs:89, 113); used to split sets (par2.rs:111, 132-134). Not stored. |
| Packet type (48) | parsed | Dispatch (par2.rs:93-124). |
| Main: slice size (body 0) | stored | `Inventory.block_size` (par2.rs:95) → steps.rs:249-253 → `SET_PAR2_SIZES` (duplicates table; column names not in this copy). |
| Main: recovery-set file count N (body 8) | no | Not read. Ids start at body 12 (par2.rs:96). |
| Main: recovery-set file ids (16N) and non-recovery ids | parsed | par2.rs:96-101 → `mains`. Used for `complete` (par2.rs:132-134). Not stored. |
| FileDesc: file ID (body 0) | parsed | par2.rs:103-113. Not stored. |
| FileDesc: MD5 of file (body 16) | parsed | `Par2File.md5` (par2.rs:103-113). Persistence not visible in this copy. |
| FileDesc: MD5-16k (body 32) | parsed | `Par2File.md5_16k`. Dropped by `listing_of` (steps.rs:72-76). Not stored. |
| FileDesc: file length (body 48) | stored | `archive_inner_files.size` (source=1) via steps.rs:74. |
| FileDesc: file name (body 56) | stored | `archive_inner_files.name` (par2.rs:108-110 via `clean_name`). Also used for naming (steps.rs:156-164). |
| Unicode filename (UniFileN) | parsed, applied | par2.rs:115-131 overrides the ASCII name. Not separately stored. |
| IFSC (per-slice MD5 + CRC32) | no | No branch in par2.rs:93-124. |
| Recovery slice (RecvSlic: exponent) | no | No branch. Block count not counted. |
| Creator (`PAR 2.0\0Creator\0`) | no | No branch. **Packer fingerprint dropped.** |
| ASCII comment / Unicode comment | no | Not read. |
| Input file slice (FileSlic) | no | Correctly ignored (data). |
| RFSC (RecvFileSliceChecksum) | no | Spec says unused. |
| PkdMain / PkdRecvS | no | Not read. |
| Main-set completeness | parsed | `inv.complete` (par2.rs:132-134) → `content` (steps.rs:129-131) → `duplicates::SET_CONTENT`. |
| Set covers all files | parsed, stored (indirectly) | steps.rs:243 → `SET_POSTED`, `CLEAR_POSTED`, `SET_PAR2_SIZES` (duplicates). |

### 6.1 PAR2 bugs and limits
- The Main packet's `complete` test includes non-recovery IDs (par2.rs:96-101, 132-134). Confirm this is intended.
- The listing reads only the first one or two articles of the set (steps.rs:216-234). A base file with more FileDesc packets than that gives a partial list. `complete` covers this, so the outcome is safe.
- `MAX_FILES` = 4096 (par2.rs:24) silently caps sets.
- Creator and IFSC are the two highest-value PAR2 fields (research), and both are absent.

---

## 7. SFV, SRR and SRS (w1c §4)

Atlas code: `parse_sfv` probe/mod.rs:183-198; the SFV/NFO loop in probe/step.rs run() (`take(3)`).

| Field (research id) | Atlas status | Where / note |
|---|---|---|
| SFV entry: filename | stored | `ProbeInfo.sfv` keys (probe/mod.rs:70) → `release_probe.info` JSON. |
| SFV entry: CRC-32 (hex8) | stored | `ProbeInfo.sfv` values (u32). |
| SFV: `;` comment lines (generator header, e.g. "Generated by ...") | parsed, dropped | probe/mod.rs:189 `starts_with(';')` skip. **Packer fingerprint dropped.** |
| SFV: whitespace separator | parsed | probe/mod.rs:190-193 (`rsplit_once(char::is_whitespace)`). |
| SFV file size | not applicable | SFV has none. |
| `.md5` / `.sha1` sidecar | no | Only `.sfv` extension is handled (probe/step.rs run loop). |
| SFV source selection | parsed | `take(3)` (probe/step.rs run loop) can exclude the SFV. |
| NFO text | stored | `ProbeInfo.nfo` (truncated to `MAX_NFO`) → `release_probe.info`. |
| NFO imdb ids | stored | `ProbeInfo.imdb_ids` → `release_probe.info`. |
| NFO password | stored | `release_nfo_password.password` (store.rs:54-65). |
| SRR (any field: app name, stored files, RAR file blocks, dates, comments, RR) | no | No parser exists in the copy (grep for `srr`, `0x69`, `0x6a`, `0x71`, `0x72`, `rescene` finds nothing). |
| SRS | no | No parser. |

### 7.1 SFV, SRR bugs and limits
- **SFV starvation** (probe/step.rs run loop, `take(3)`): pick the `.sfv` explicitly, not by position.
- **Generator comment dropped** (probe/mod.rs:189). Keep the first `;` comment.
- **SRR risk to check**: if `is_small_text` matches `.srr`, the NFO branch (probe/step.rs run loop, `info.nfo.is_none()`) would decode a binary SRR as text and store it in `ProbeInfo.nfo`. `is_small_text` is not in this copy, so verify it.
- SFV parse keeps last-wins duplicates (BTreeMap insert, probe/mod.rs:194).

---

## 8. Proposed minimal changes, per format

Principle: every change below reads bytes already fetched (no extra article fetches), except the 7z LZMA2 path and the PAR2 IFSC phase, which are flagged.

### 8.1 Schema (shared)
Use the existing `ShardTable.added` mechanism (store.rs:50 for `archive_release`):
- `archive_inner_files`: add `crc32 INTEGER`, `mtime INTEGER` (unix seconds), `attr INTEGER` (raw attributes or unix mode), `host_os INTEGER`.
- `archive_release`: add `format TEXT` (RAR4/RAR5/7Z/ZIP/PAR2; not stored anywhere today), `solid INTEGER`, `volume INTEGER`, `host_os INTEGER`, `method TEXT`, `creator TEXT` (PAR2 Creator or SFV generator), `comment TEXT` (ZIP EOCD comment, RAR5 CMT), `hash BLOB` (RAR5 BLAKE2sp when unsplit).
- New table `archive_par2_files` (release_id, file_id BLOB, md5 BLOB, md5_16k BLOB, size, name), PK (release_id, file_id). Source for the cheap identity check the research calls the most useful PAR2 field.
- `SOURCE_SRR = 5` in archive/store.rs:13-16, for SRR-stored names.

### 8.2 RAR4 (archive/rar.rs, probe/archive.rs)
1. **Fix unknown size** in `file4` (rar.rs:108-111) and `rar4()` (probe/archive.rs:75): if `flags & 0x0100 == 0 && unp == 0xFFFF_FFFF`, use None.
2. **Unicode name in probe**: make `name4` (rar.rs:115) `pub(crate)`; in `rar4()` (probe/archive.rs:79-83) call it on the raw name and NUL-separated encoding.
3. **CRC and mtime in listing**: in `file4` (rar.rs:90-113) read `le32(header, 16)` (CRC) and `le32(header, 20)` (DOS time, convert with a `dos_to_unix` helper from rar_meta.rs:173). Extend `InnerFile` (archive/mod.rs:159-163) with `crc32: Option<u32>`, `mtime: Option<i64>`, `attr: Option<u32>`. Extend `ArchiveHead` (probe/archive.rs:12-29) the same way plus `host_os`.
4. **Host OS**: `header[15]` (rar.rs `file4`), store in `ArchiveHead.host_os`.
5. **Main flags**: in `list4` (rar.rs:53-59) and `rar4()` (probe/archive.rs:62-66) store `volume` (0x0001), `solid` (0x0008), `newnumbering` (0x0010), `first_volume` (0x0100), `protect` (0x0040) into `ArchiveHead` and `archive_release.solid` / `volume`.
6. **EXTTIME** (later, optional): port `rar4_exttime` (prototype rar_meta.rs:364) for ns mtime.

### 8.3 RAR5 (archive/rar.rs, probe/archive.rs)
1. **CRC in listing**: rar.rs:265-267 replace `p += 4` with `le32(inner, p)` and put it on `InnerFile.crc32`.
2. **Mtime, attributes, host** in listing and probe: rar.rs:261-269 and probe/archive.rs:141-150. Read `attributes` (`_attributes`), mtime (`le32` at the skipped position), host (`_host`), keep them.
3. **Compression info**: rar.rs:268 and probe/archive.rs:149-156. Keep version (`info & 0x3f`), solid (`info >> 6 & 1`), dict (`info >> 10 & 0x1f`). Store `solid` and a `method` numeric.
4. **Main header (kind 1)**: add a `1 =>` arm in `block5` (rar.rs:249-257) and in `rar5()` (probe/archive.rs:129-166): read archive flags (`0x0001` volume, `0x0004` solid, `0x0008` RR present), volume number if `0x0002`. Store as `archive_release.volume`, `solid`.
5. **Extras**: replace `has_crypt_record` (probe/archive.rs:174-185) and the crypt-only walk with one `rar5_extras(extra_area) -> Extras` in archive/rar.rs. Keep crypt (type 1, with its TWEAKED flag 0x0002). Add time (type 3 → mtime, ctime, atime, with ns), hash (type 2 → BLAKE2sp 32 bytes into `archive_release.hash` when the file is not split), redirection (type 5 → `InnerFile.redirect`, so symlinks and hard links do not count as the movie), owner (type 6 → `archive_release.owner`, optional), version (type 4, optional).
6. **Locator** (main extra type 1): record QO and RR offsets only if the RR percent is needed later (see §8.7 for PAR2/RR).

### 8.4 ZIP (archive/zip.rs, probe/archive.rs)
1. **Central directory fields** in `list_tail` (zip.rs:304-332): read CRC32 (`le32(entry, 16)`), DOS time/date (`le16` at 12 and 14), version made by (`le16` at 4, host = high byte), external attributes (`le32` at 38), numeric method (already read at zip.rs:305, keep it). Put on `InnerFile` as `crc32`, `mtime`, `attr`, `host_os`. Use the external attribute's DOS dir bit (0x10) and unix mode (`attr >> 16`, S_IFDIR/S_IFLNK) instead of the name suffix (zip.rs:44-46).
2. **Compressed size** (zip.rs:324): stop discarding it; keep for `archive_release` ratio if needed.
3. **Encrypted flags in probe** (probe/archive.rs:191): use `is_encrypted(flags, method)` from zip.rs:36-38 (make `pub(crate)`).
4. **Name decoding**: `name_of` (zip.rs:26-34): add CP437 decoding for bit-11-clear non-UTF-8 names (table of 128 entries). Use `name_of` in the probe (probe/archive.rs:217).
5. **Extra fields**: add `zip_extras(extra)` next to `aes_strength` (zip.rs:182-191), iterating ids as `aes_strength` does. Handle 0x7075 (Info-ZIP unicode path: overrides the name, version 1, CRC of the ASCII name), 0x5455 (extended timestamp: mtime), 0x000a (NTFS times), 0x000d (UNIX: uid, gid, mode), 0x0017 (strong encryption header). 0x7075 matters most: it is the real name for many non-ASCII releases.
6. **EOCD comment** (zip.rs:257-270 and its `tail.get(at+22..)` slice): store as `archive_release.comment`. It is often a packer name or release note.
7. **Numeric method** in `ArchiveHead.method` (probe/archive.rs:189-204): keep the number in a new `method_id: Option<u16>` so "other" is recoverable.

### 8.5 7z (archive/sevenz.rs, probe/archive.rs)
1. **Substream CRCs**: at sevenz.rs:299-304 keep the return of `r.digests(count)` and return it inside `Streams` (new field `crcs: Vec<Option<u32>>`, one per substream, mapping like `sizes` at sevenz.rs:281-292). Thread it into `files()` (sevenz.rs:335-396) and set `InnerFile.crc32` on each non-empty-stream entry. Empty files get `Some(0)` size and no CRC.
2. **kMTime / kCTime / kATime / kWinAttributes** in `files()` (sevenz.rs:355-364): add arms `0x12`, `0x13`, `0x14`, `0x15`. Each has a defined-bits vector (like `bits()` at sevenz.rs:90-101 and `digests` at sevenz.rs:103-119), an `external` byte (must be 0), then 8-byte FILETIME (0x14) or u32 attrs (0x15). Attrs with bit 0x8000 carry the unix mode in the high 16 bits. Store `InnerFile.mtime` and `attr`.
3. **kAnti 0x10**: add an arm that records a per-entry bit and skips that entry in the `for (i, name)` loop (sevenz.rs:368-389). Anti-items are not files.
4. **Method chain**: in `folder()` (sevenz.rs:151-204) keep the coder ids as a short string, e.g. `LZMA2+BCJ`, plus LZMA dict size from props (props[1..5] LE). Store as `ArchiveHead.method` in the probe and `archive_release.method` in the listing.
5. **Probe for 7z**: replace the signature-only arm (probe/archive.rs:36-37) with a call to `sevenz::list(data, &[])`, mapping `encrypted`, `headers_encrypted`, first name, first size. Honest limit: a 64 KiB or 4 MiB head will often set `need_more_tail`; the probe then reports the signature plus what it could read.
6. **LZMA2 for encoded header**: in `decode()` (sevenz.rs:433-449) add a branch on `id == [0x21]` using `lzma_rs`' LZMA2 decoder (verify the API in the pinned crate), with dict from the 1-byte prop (sevenz.rs:513-516 widened to allow `21`). Keep `MAX_DECODED` and `catch_unwind`.
7. **Tail window**: sevenz.rs:479-482 compute `header_at = len - s.size` relative to the absolute offset, rather than `window.len() - size`.
8. Keep start-header minor version (sevenz.rs:37) into `ArchiveHead` only if version reporting is wanted. Low priority.

### 8.6 PAR2 (par2.rs, archive/steps.rs)
1. **Keep MD5 and MD5-16k** (par2.rs:103-113 already parses them). Stop dropping them at `listing_of` (steps.rs:72-76): carry `md5`, `md5_16k`, `file_id` on `InnerFile` (or a separate `Par2File` list) and write to the new `archive_par2_files` table in `finish()` (steps.rs:84-173).
2. **Creator packet**: add a branch in `inventory()` (par2.rs:93-124) for `PAR 2.0\0Creator\0`. Store the text (not NUL-terminated, trim NULs) in `Inventory.creator` and into `archive_release.creator`. Prototype `classify_creator` (par2_sfv_srr_meta.rs:323) gives the fingerprint.
3. **Recovery block count**: count `PAR 2.0\0RecvSlic` packets per set in `inventory()` (one per recovery block; the exponent at body 0 is the block number). Store as `recovery_blocks` next to `SET_PAR2_SIZES` (steps.rs:265-268). The percentage is then `recovery_blocks / data_blocks`.
4. **IFSC** (phase 2): parse `PAR 2.0\0IFSC\0\0\0\0` into per-slice (MD5, CRC32) entries. The prototype `file_crc_from_slices` (par2_sfv_srr_meta.rs:87) derives a file CRC32 without the file. Needs CRC-combine (prototype `crc_shift_bits` / `crc_unshift_bits`, par2_sfv_srr_meta.rs:58-80). Store per file as `crc32`.
5. **Main recovery-set N** (body 8): read it and compare to the count of FileDesc entries in the set (cheap check for incomplete sets).

### 8.7 SFV, SRR, SRS (probe/mod.rs, probe/step.rs, new archive/srr.rs)
1. **SFV source selection**: in probe/step.rs run(), replace the `take(3)` loop with explicit selection: at most one `.sfv` (the first by name) plus up to two NFOs.
2. **Generator comment**: in `parse_sfv` (probe/mod.rs:183-198), keep the first `;` comment line as a new `ProbeInfo.sfv_generator: Option<String>`. Store via JSON (no schema change).
3. **`.md5` and `.sha1` sidecars**: new `parse_hash_list` for `HEX32  name` and `HEX40  name` lines, selected in the same loop (probe/step.rs). Stored as `ProbeInfo.sfv` for MD5 (same map is fine if the key is the name; store the hash kind alongside).
4. **SRR parser** (new `archive/srr.rs`): port `parse_srr` (par2_sfv_srr_meta.rs:489-593). Block layout per the prototype: CRC16 header, type 0x69 (SRR header with app name), 0x6A (stored file: name and size), 0x71 (RAR file block: name, size, dates), 0x72 (archived file info). Store app name in `archive_release.creator`, stored-file names in `archive_inner_files` with `source = 5` (`SOURCE_SRR`). Mark the byte layout as "to verify against a real SRR", because w1c §4.3 lists it as UNVERIFIED.
5. **SRR guard**: confirm `is_small_text` excludes binary `.srr` from the NFO branch, or skip `.srr` explicitly before `nfo_text`.

### 8.8 Shared bug fixes (no schema change)
- **Stale encryption columns** (steps.rs:141-152, store.rs:70-73): when `listing.encryption` is None, clear `encryption`, `names_hidden`, `password_check` for this release (one `UPDATE ... SET NULL`), or make the UPSERT write all four columns every time.
- **View cap** (store.rs:91-99): `limit 256` is a display choice. If the view should show the whole list, raise it or paginate. Document the choice if intentional.
- **Probe-vs-listing name** for RAR4 Unicode (see §8.2 item 2).

---

## 9. Priorities

| Priority | Change | Why | Effort |
|---|---|---|---|
| P1 | Keep 7z substream CRCs and ZIP CD CRC and RAR CRC on `InnerFile` | Free per-file checksums, no extra bytes | Small |
| P1 | RAR4 unknown-size fix (rar.rs:109, probe/archive.rs:75) | Wrong sizes stored today | Tiny |
| P1 | ZIP CD: mtime, external attrs, version-made-by, numeric method | Directory and symlink detection, host OS | Small |
| P1 | PAR2 Creator packet and RecvSlic count | Packer fingerprint and recovery percentage | Small |
| P1 | Stale encryption columns fix (steps.rs:141-152) | Wrong state after reruns | Tiny |
| P1 | SFV `take(3)` fix and generator comment | SFV loss, fingerprint | Small |
| P2 | RAR5 extras: time, BLAKE2sp hash, redirection | Content identity, symlink-safe sizes | Medium |
| P2 | 7z kMTime / kWinAttributes / kAnti | Time, dir detection, correctness | Medium |
| P2 | 7z LZMA2 encoded header | Headers that fail today | Small (verify lzma_rs API) |
| P2 | 7z probe via `sevenz::list` | Probe currently blank for 7z | Small |
| P2 | RAR main-header flags (solid, volume, RR) | Multi-volume and solid detection | Small |
| P2 | ZIP 0x7075 unicode path | Correct names for non-ASCII releases | Small |
| P2 | PAR2 MD5-16k and MD5 persisted (`archive_par2_files`) | Cheap identity check the research ranks highest | Medium |
| P3 | SRR parser | Whole format absent | Medium (block layout to verify) |
| P3 | PAR2 IFSC and file CRC derivation | Per-file CRC without the file | Larger (CRC combine) |
| P3 | SFX scan (`find_sig`) | SFX RARs unidentified | Medium |
| P3 | ZIP EOCD comment, RAR5 CMT, PAR2 comments | Packer notes | Small |

---

## 10. Items not verified here
- `archive/names.rs`, `archive/select.rs`, `archive/testkit.rs`, `media`, `duplicates`, `is_small_text`: not in the copy. Statuses that depend on them (e.g. `clean_name` behaviour, `SET_PAR2_SIZES` column names, whether `.srr` is "small text") are marked as unverified.
- Persistence of PAR2 `md5` and `md5_16k` through `duplicates::twin_fingerprint` is not visible. Marked "parsed" here.
- `lzma_rs` LZMA2 API and the exact behaviour of `UseProvided` with the 5-byte props header were not run. The LZMA path (sevenz.rs:433-449) matches lzma_rs' documented `.lzma` header layout (props then dict size, no size field under `UseProvided`), which is what the code assumes.
- The SRR block layout and the RAR4 EXTTIME ns remainder are as stated in the research (w1c §4.3, w8c §a); not re-verified against bytes.
