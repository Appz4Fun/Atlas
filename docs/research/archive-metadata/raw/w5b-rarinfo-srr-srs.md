# W5b: rarinfo (RAR/ZIP/7z/PAR2/SFV), SRR and SRS format research

Status legend: VERIFIED = read in fetched source (URL + line). UNVERIFIED = not confirmed from source in this run.

Access note: `gh api` returned HTTP 403 ("GitHub access to this repository is not enabled for this session") for every candidate repo (zeebinz/rarinfo, KurzonDax/nZEDbetter, kop1/newznab, srrDB/pyrescene, EchterAgo/pyrescene). Code search (`gh api search/code`) still worked and gave the paths. Source was fetched from raw.githubusercontent.com (branch `master`), which worked. Local copies are in the scratchpad under `w5b/`. Line numbers below refer to the raw files as fetched.

## 0. Which copy of rarinfo

| Copy | Path | Notes |
|---|---|---|
| zeebinz/rarinfo | https://raw.githubusercontent.com/zeebinz/rarinfo/master/rarinfo.php (44,991 B) | Full set: rarinfo, szipinfo, zipinfo, par2info, sfvinfo, srrinfo, archiveinfo, archivereader. VERIFIED present. |
| KurzonDax/nZEDbetter | https://raw.githubusercontent.com/KurzonDax/nZEDbetter/master/www/lib/rarinfo/rarinfo.php (44,633 B) | Same family. No `szipinfo.php` (404). VERIFIED. |
| kop1/newznab | https://raw.githubusercontent.com/kop1/newznab/master/www/lib/rarinfo.php (15,657 B) | Older, single-file, RAR only. Its class header says "RarInfo class... pure PHP". VERIFIED. |

Code search also listed kop1/nnplus, maschmann/newznab (www/lib/rarinfo.php) as copies. Not fetched.

## 1. Shared base: ArchiveReader and partial-data strategy

- Default read window: `protected $maxReadBytes = 1048576;` (1 MiB). https://raw.githubusercontent.com/zeebinz/rarinfo/master/archivereader.php line 416. VERIFIED.
- Filename cap: `protected $maxFilenameLength = 256;` line 422. VERIFIED.
- `$isFragment` flag set by `open($file, $isFragment)` / `setData($data, $isFragment)` (lines 246-292). VERIFIED.
- `setData` truncates input to `maxReadBytes` (line 292: `$data = ($dsize > $this->maxReadBytes) ? substr($data, 0, $this->maxReadBytes) : $data;`). VERIFIED. So for partial data, only the first 1 MiB is ever parsed, unless the caller calls `setMaxReadBytes` (line 322).
- Facade `archiveinfo.php` detects type by extension regex `'rar|r[0-9]+|zip|srr|par2|sfv|7z|[0-9]+'` (zeebinz/archiveinfo.php line 707) and maps type flags TYPE_RAR=0x0002, TYPE_ZIP=0x0004, TYPE_SRR=0x0008, TYPE_SFV=0x0010, TYPE_PAR2=0x0020 (nZEDbetter/archiveinfo.php lines 82-87) to classes RarInfo, SrrInfo, Par2Info, ZipInfo, SfvInfo (lines 101-105). VERIFIED (nZEDbetter copy). TYPE_SZIP/7z is not in this constant list (UNVERIFIED whether 7z is wired into the facade; zeebinz copy has the 7z extension in the regex but the nZEDbetter map shows no 7z entry).

## 2. RAR (rarinfo.php)

Signatures:
- RAR 1.5 marker `"\x52\x61\x72\x21\x1a\x07\x00"` (line 255); RAR 5.0 marker `"\x52\x61\x72\x21\x1a\x07\x01\x00"` (line 261). VERIFIED.
- Format detection: `findMarker()` reads min(length, maxReadBytes), strpos for each marker, sets `$this->format` (lines 820-838). VERIFIED.

Block header (RAR 1.5 / 4.x): 7 bytes `vhead_crc/Chead_type/vhead_flags/vhead_size` (`FORMAT_BLOCK_HEADER`, ~line 157); `add_size` (4 bytes) present only if `LONG_BLOCK` (0x8000) and type is not FILE or SUB (getNextBlock, lines 924-945). VERIFIED.

Block types (lines 64-74): MARK 0x72, MAIN 0x73, FILE 0x74, OLD_COMMENT 0x75, OLD_EXTRA 0x76, OLD_SUB 0x77, OLD_RECOVERY 0x78, OLD_AUTH 0x79, SUB 0x7A, ENDARC 0x7B, NULL 0x00. VERIFIED.

Main-header flags (lines 77-86): VOLUME 0x1, COMMENT 0x2, LOCK 0x4, SOLID 0x8, NEWNUMBERING 0x10, AUTH 0x20, RECOVERY 0x40, PASSWORD 0x80, FIRSTVOLUME 0x100, ENCRYPTVER 0x200. VERIFIED. Main block reads 6 more bytes (`vreserved1/Vreserved2`, line ~964-970).

File-header flags (lines 89-108): SPLIT_BEFORE 0x1, SPLIT_AFTER 0x2, PASSWORD 0x4, COMMENT 0x8, SOLID 0x10, DICTMASK 0xE0 (DIRECTORY = 0xE0), LARGE 0x100, UNICODE 0x200, SALT 0x400, VERSION 0x800, EXTTIME 0x1000, EXTFLAGS 0x2000. VERIFIED.

File block fields (`FORMAT_FILE_HEADER`, line ~138-140, read 25 bytes at line 1009): `Vpack_size, Vunp_size, Chost_os, Vfile_crc, Vftime, Cunp_ver, Cmethod, vname_size, Vattr`. Then if LARGE: high_pack_size, high_unp_size (8 bytes, lines 1011-1014). Then filename: if UNICODE, the name field is split on NUL and decoded via `RarUnicodeFilename` (the RAR unicode-compression scheme) into UTF-16LE then UTF-8; else raw. Optional SALT (8 bytes), `ext_time`, `has_password`, `split_before`, `split_after`. VERIFIED (lines ~1005-1085).
- Method codes: STORE 0x30 ... BEST 0x35 (lines 131-136). Note the sanity check rejects method > 0x35 (line 806). VERIFIED.
- Directory = `(flags & DICTMASK) == 0xE0`. VERIFIED.
- Subblock AUTHVER (`av_file_name`, `av_created_by`) parsed at ~line 1085-1095 (labelled "guesswork" in source). VERIFIED.

RAR 5.0 (`getNextBlockR50` line 1101, `processBlockR50` line 1142): header is `Vhead_crc` (4 bytes), then vint head_size, vint head_type, vint head_flags, optional vint extra_size (flag 0x1), vint data_size (flag 0x2). VERIFIED.
- Block types: MAIN 1, FILE 2, SERVICE 3, CRYPT 4, ENDARC 5 (lines 175-179). VERIFIED.
- FILE fields: vint flags (DIRECTORY 0x1, UTIME 0x2, CRC32 0x4, UNPKNOWN 0x8), vint unp_size, vint attr, optional `Vutime`, optional `Vfile_crc`, vint comp_info (version = &0x3f, method = (>>7)&7), vint host_os, vint name_size, name. pack_size = data_size. VERIFIED (lines 1151-1200).
- Quick-open service records (`processQuickOpenRecords`, line 1242) and comment service (`R50_SERVICE_COMMENT`). VERIFIED that these exist; field details not read.
- Extra records: locator (0x01), encryption (0x01 CRYPT), hash (0x02), htime (0x03), version (0x04), redirect (0x05), uowner (0x06), subdata (0x07) (lines 214-227). VERIFIED.
- RAR5 crypt block sets `isEncrypted`. VERIFIED (line ~1180).
- RAR5 skips header CRC checks (`checkFileHeaderCRC` returns false for RAR50, line 780). VERIFIED.

Partial data: `findFileHeader()` (line 735) for fragments scans the buffer for a valid file header and seeks to it; `sanityCheckFileHeader` (line 800) checks name_size <= maxFilenameLength and method <= 0x35; both bail for RAR50. VERIFIED. `analyze()` (lines 844-890) loops blocks until EOF; a read exception after at least one good block is not fatal. VERIFIED.

Encrypted archives: loop breaks on first encrypted block (line ~885). VERIFIED.

## 3. 7z (szipinfo.php, zeebinz copy)

Source: https://raw.githubusercontent.com/zeebinz/rarinfo/master/szipinfo.php. The class comment (lines 12-24) says the class is "quite flakey", that headers are at start and end, and that for fragments "aim for the end of the file". VERIFIED.

- Signature `"7z\xbc\xaf\x27\x1c"` (MARKER_SIGNATURE, line 105). VERIFIED.
- Start header: `Cversion_major/Cversion_minor/Vhead_crc/Vnext_head_offset/Vnext_head_offset_high/Vnext_head_size/Vnext_head_size_high/Vnext_head_crc`, 26 bytes (lines 111-112). VERIFIED.
- `analyze()` (~line 470): reads start header, computes `next_offset = offset + next_head_offset`, seeks there (so it reads the END header, i.e. the header at the end of the archive, not the start). Then loops `readNextHeader()` (~608). VERIFIED.
- Fragment search: `findHeader()` (~547) buffers min(length, maxReadBytes) and looks for `0x01 0x04` (PROPERTY_HEADER + MAIN_STREAMS_INFO, unencoded) or `0x17 0x06` (ENCODED_HEADER + PACK_INFO). It then runs `sanityCheckStreamsInfo`. VERIFIED.
- Property IDs (lines 50-100): HEADER 0x01, ARCHIVE_PROPERTIES 0x02, ADDITIONAL_STREAMS 0x03, MAIN_STREAMS 0x04, FILES_INFO 0x05, ENCODED_HEADER 0x17, PACK_INFO 0x06, UNPACK_INFO 0x07, SUBSTREAMS_INFO 0x08, SIZE 0x09, CRC 0x0A, FOLDER 0x0B, CODERS_UNPACK_SIZE 0x0C, NUM_UNPACK_STREAM 0x0D, EMPTY_STREAM 0x0E, EMPTY_FILE 0x0F, ANTI 0x10, NAME 0x11, CREATION_TIME 0x12, LAST_ACCESS_TIME 0x13, LAST_WRITE_TIME 0x14, ATTRIBUTES 0x15, COMMENT 0x16, START_POSITION 0x18, DUMMY 0x19. VERIFIED.
- Coder methods: COPY '00', LZMA '03', CRYPTO '06', 7z-AES '06f10701' (lines 94-98). VERIFIED.
- FilesInfo parse (`processFilesInfo`, line 939): `readNumber()` = num_files; loop of (type, size) pairs. NAME (0x11): UTF-16LE NUL-terminated per file, converted to UTF-8 (lines 963-975). LAST_WRITE/CREATION/ACCESS times via `processFileTimes` (lines 979-988, 1059). ATTRIBUTES (0x15): boolean vector then 4-byte attr each (lines 990-1000). EMPTY_STREAM/EMPTY_FILE at lines 1017-1021. VERIFIED.
- Encoded headers (the usual case): `getEncodedHeader()` (line 1336) and `extractHeaders()` (line 1411) require the external 7za client. Without it, `analyze()` returns the parsed header list only. VERIFIED from the `analyze` tail and the `extractHeaders` name. Exact external-client argv: `e -so -bd -y -t7z {$pass} --` (line 378). VERIFIED.
- Result: `getFileList` gives name (truncated to 256), and per-file attributes/times. CRC of files comes from the SubStreams digests (`processSubstreamsInfo`, line 860; CRC at 906). VERIFIED that the code is present; field-level detail not read.
- AES-encrypted folder detected via `METHOD_7Z_AES` (line ~810). VERIFIED.

## 4. ZIP (zipinfo.php)

Source: https://raw.githubusercontent.com/zeebinz/rarinfo/master/zipinfo.php (24,701 B).
- Record signatures (lines 65-72): CENTRAL_FILE 0x02014B50, LOCAL_FILE 0x04034B50, SIGNATURE 0x05054B50, ENDCENTRAL 0x06054B50, Z64_ENDCENTRAL 0x06064B50, Z64_ENDCENTRAL_LOC 0x07064B50, ARCHIVE_EXTRA 0x08064B50, DATA_DESCR 0x08074B50. VERIFIED.
- Flags (75-79): ENCRYPTED 0x1, DESCRIPTOR_USED 0x8, STRONG_ENCRYPTED 0x40, EFS_UTF8 0x800, CDR_ENCRYPTED 0x2000. VERIFIED.
- Extra IDs (82-91): ZIP64 0x1, NTFS 0x000A, UNIX 0x000D, STRONG_ENCR 0x17, POSZIP 0x4690, UNIXTIME 0x5455, IZUNIX 0x5855, IZUNIX2 0x7855, IZUNIX3 0x7875, WZ_AES 0x9901. VERIFIED.
- Partial strategy: `findMarker()` (line 504) searches the first maxReadBytes for LOCAL_FILE, else CENTRAL_FILE, else ENDCENTRAL (empty zip). VERIFIED. Note: this only sees the start of the file. Central directory at the tail is not read in `findMarker`. UNVERIFIED whether `analyze()` seeks to the tail for ENDCENTRAL.

## 5. PAR2 (par2info.php)

Source: https://raw.githubusercontent.com/zeebinz/rarinfo/master/par2info.php (13,335 B).
- Packet marker `"PAR2\x00PKT"` (line 53). `findMarker` = strpos in first maxReadBytes (lines 243-256). VERIFIED.
- Packet header (line 77): `A8head_marker / Vhead_length / Vhead_length_high / H32head_hash (MD5 of packet) / H32head_set_id / A16head_type`. VERIFIED.
- Main packet (line 83): `Vblock_size / Vblock_size_high / Vrec_file_count`. VERIFIED.
- File Description packet (line 88): `H32file_id / H32file_hash / H32file_hash_16K / Vfile_length / Vfile_length_high`, i.e. file name hash (MD5 of the 16K-prefix and full), first-16KB MD5, and length. VERIFIED. Filename is stored after this struct in the packet (not read in this pass; UNVERIFIED exact read).

## 6. SFV (sfvinfo.php)

Source: https://raw.githubusercontent.com/zeebinz/rarinfo/master/sfvinfo.php (3,682 B).
- `findMarker()` line 95; `analyze()` line 111 reads min(length, maxReadBytes) (line 115). VERIFIED. SFV is text, so parsing is line-based "name CRC32". Field-level parse UNVERIFIED (not read).

## 7. SRR (srrinfo.php and dev-docs/srr_spec.txt)

Sources:
- https://raw.githubusercontent.com/zeebinz/rarinfo/master/srrinfo.php (8,822 B). Same as nZEDbetter copy (8,882 B; minor diff).
- Authoritative spec: https://raw.githubusercontent.com/srrDB/pyrescene/master/dev-docs/srr_spec.txt (38,126 B). Block definitions at lines 553-700.

Constants (srrinfo.php lines 58-61, 64, 71): SRR_BLOCK_MARK 0x69, SRR_STORED_FILE 0x6A, SRR_OSO_HASH 0x6B, SRR_RAR_FILE 0x71; APP_NAME_PRESENT 0x0001; FORMAT_SRR_OSO_HASH `Vfile_size/Vfile_size_high/h16file_hash/vname_size`. VERIFIED.

Block table (spec, lines ~553-700; common header HEAD_CRC 2 / HEAD_TYPE 1 / HEAD_FLAGS 2 / HEAD_SIZE 2 / ADD_SIZE 4 only if flags&0x8000):

| Type | Name | HEAD_CRC | Flags | Fields | Source |
|---|---|---|---|---|---|
| 0x69 | SRR Header | 0x6969 | 0x0001 = app name present | APP_NAME_SIZE (2), APP_NAME (generator name, UTF-8) | spec ~571-600 VERIFIED |
| 0x6A | SRR Stored File | 0x6A6A | 0x8000 must be set | ADD_SIZE = file size (4, max 4 GiB), NAME_SIZE (2), NAME (path with "/", UTF-8), then file data (e.g. .nfo, .sfv) | spec ~601-628 VERIFIED |
| 0x71 | SRR RAR File | 0x7171 | 0x0001 = recovery data removed | NAME_SIZE (2), NAME (RAR volume path, "/", UTF-8) | spec ~629-650 VERIFIED |
| 0x6B | SRR OSO Hash | 0x6B6B | 0x0001 = recovery removed | FILE_SIZE (8), OSO_HASH (8, stored as number), NAME_SIZE (2), FILE_NAME (no path, UTF-8) | spec ~651-670 VERIFIED |
| 0x6C | SRR RAR Padding | 0x6C6C | 0x8000 = ADD_SIZE present | ADD_SIZE = padding bytes after the RAR end-of-archive block | spec ~690-700 VERIFIED |

Parser (srrinfo.php ~lines 262-300): handles 0x69 (app name), 0x6A (`vname_size`, name, then `add_size` bytes of data), 0x6B (18 bytes: size, size_high, hash, name_size; hash is `strrev`'d; size = int64), 0x71 (name). Anything else falls to `parent::processBlock` (RAR block parser). VERIFIED.
- Consequence: the zeebinz/nZEDbetter SRR parser has no branch for 0x6C padding. A padding block would be read as a RAR block (UNVERIFIED impact; it has `add_size` so `next_offset` may still land correctly).
- `getStoredFiles` returns stored-file name, size (`add_size`), and data (srrinfo.php ~166-172). VERIFIED. OSO list ~194-198. RAR list ~221-222. VERIFIED.
- Marker check: flags > 1 or offset mismatch = invalid (line ~268). VERIFIED.
- Spec: each SRR RAR block is followed by the RAR blocks of that volume (spec ~474-490). VERIFIED.

## 8. SRS (pyReScene sample format)

Sources (srrDB/pyrescene, master):
- https://raw.githubusercontent.com/srrDB/pyrescene/master/resample/main.py (126,513 B) - all container readers and writers.
- https://raw.githubusercontent.com/srrDB/pyrescene/master/resample/ebml.py (16,913 B) - EBML IDs.
- https://raw.githubusercontent.com/srrDB/pyrescene/master/resample/srs.py (23,792 B) - CLI only (`srs sample.mkv --dd`, `srs sample.srs full.mkv`, `-l` to list SRS info). VERIFIED.
- resample/mkv.py and resample/mp4.py DO NOT EXIST in this repo (HTTP 404). The container logic is inside main.py (MkvReSample at line 607, Mp4ReSample line 623, AviReSample line 590, WmvReSample 639, FlacReSample 655, Mp3ReSample 671, StreamSample 687, M2tsReSample 703). VERIFIED.
- `bin/srs_*.py`, `bin/pyrescene.py` exist (from code search). Not fetched.

Container markers:
- Detection (main.py ~203-204): a file starting with `b"SRSF"` is treated as MP3-type. VERIFIED.
- MKV: EBML element IDs (ebml.py line 244-245): `RESAMPLE_FILE = b"\x6A\x75"`, `RESAMPLE_TRACK = b"\x6B\x75"`. Element type enums ReSampleFile / ReSampleTrack (ebml.py 161-167, 279-280). VERIFIED.
- AVI (RIFF) / ASF / FLAC / MP3 / M2TS / MOV (MP4): main.py serialize functions:
  - RIFF: chunk `SRSF` + LE32 length + data (serialize_as_riff, ~line 331-335). VERIFIED.
  - MOV/MP4: atom = BE32 (len+8) + `SRSF` + data (serialize_as_mov ~338-341). VERIFIED.
  - ASF: GUID_SRS_FILE object, LE64 length (~343-349). VERIFIED (name only).
  - FLAC: block type 0x73 ('s') + 3-byte BE length + data (~352-355). VERIFIED.
  - MP3 / stream / m2ts: `SRSF` + LE32 (4+4+len) + data (~359-365). VERIFIED.
  - Track chunks: `SRST` (main.py lines 477-485 mov/atom, 514-520 mp3 with `SRSP` fingerprint sub-block, 530 stream). VERIFIED.
  - Readers that look for these: lines 728-731 (`SRSF`/`SRST` in RIFF reader), 810-812 (MOV), 865-881 (block types incl. "SRSP"), 3418/3554/3807 (CRC skipping of SRS blocks when rebuilding). VERIFIED.

FileData (main.py lines 259-369). Binary layout (`serialize`, ~306-322):
- flags: uint16 LE (bit 0x1 SIMPLE_BLOCK_FIX; bit 0x2 ATTACHMENTS_REMOVED; supported mask 0x3).
- app name length uint16 + app name UTF-8 (APPNAME of pyrescene).
- sample name length uint16 + basename UTF-8.
- size: uint64 LE.
- CRC32: uint32 LE (sample CRC used to validate the rebuild).
Parse path (`__init__` buff branch, lines ~280-295) matches this order. VERIFIED.

TrackData (main.py lines 371-440, serialize ~line 421-440):
- flags: uint16 LE. BIG_FILE 0x4 (data_length and match_offset as 8 bytes instead of 4 for data length); BIG_TRACK_NUMBER 0x8 (track number uint32 instead of uint16, MP4 case). Mask 0xC.
- track_number: uint16 (or uint32 if 0x8).
- data_length: uint32 or uint64 (if 0x4).
- match_offset: uint64 LE, offset in the full file where track data begins.
- signature length: uint16, then signature bytes (first bytes of the track, used to find the track in the full file when offset is unknown).
- Parser (lines 383-404): `struct.unpack_from("<%sQH" ...)` for data_length, match_offset, sig_length. VERIFIED.
- Not serialized: codec (MKV ASCII), compression_algorithm, compression_settings, check_bytes, track_file. VERIFIED (lines 410-420).

Fingerprint (`SRSP`) and `read_fingerprint_data` (main.py line 538). Format not read in detail. UNVERIFIED.

SRS app name / info printed by `-l`: SRS Type, SRS App, sample name, Sample Size, Sample CRC32 (srs.py lines 383-388). VERIFIED.

## 9. How NNTmux / nZEDb store these

UNVERIFIED. The NNTmux and nZEDb repos were not fetched in this pass (gh access denied, raw not tried for those paths). Code search hits for "rarinfo" were:
- kop1/newznab www/lib/rarinfo.php, maschmann/newznab www/lib/rarinfo.php, kop1/nnplus www/lib/rarinfo.php (RAR-only copies).
- KurzonDax/nZEDbetter www/lib/rarinfo/ (full family, fetched; the consuming code was not read).

Table names (release_files, par_hashes etc.) from the task were not confirmed in any fetched source.

## Sources

- https://raw.githubusercontent.com/zeebinz/rarinfo/master/rarinfo.php (fetched; 44,991 B; RAR parser)
- https://raw.githubusercontent.com/zeebinz/rarinfo/master/szipinfo.php (fetched; 44,789 B; 7z)
- https://raw.githubusercontent.com/zeebinz/rarinfo/master/zipinfo.php (fetched; 24,701 B; ZIP)
- https://raw.githubusercontent.com/zeebinz/rarinfo/master/par2info.php (fetched; 13,335 B; PAR2)
- https://raw.githubusercontent.com/zeebinz/rarinfo/master/sfvinfo.php (fetched; 3,682 B; SFV)
- https://raw.githubusercontent.com/zeebinz/rarinfo/master/srrinfo.php (fetched; 8,822 B; SRR)
- https://raw.githubusercontent.com/zeebinz/rarinfo/master/archiveinfo.php (fetched; 26,706 B; facade)
- https://raw.githubusercontent.com/zeebinz/rarinfo/master/archivereader.php (fetched; 20,036 B; base class, maxReadBytes/maxFilenameLength)
- https://raw.githubusercontent.com/KurzonDax/nZEDbetter/master/www/lib/rarinfo/rarinfo.php (fetched; 44,633 B; copy)
- https://raw.githubusercontent.com/KurzonDax/nZEDbetter/master/www/lib/rarinfo/archiveinfo.php (fetched; 25,103 B; type map lines 82-105)
- https://raw.githubusercontent.com/kop1/newznab/master/www/lib/rarinfo.php (fetched; 15,657 B; RAR-only copy)
- https://raw.githubusercontent.com/srrDB/pyrescene/master/dev-docs/srr_spec.txt (fetched; 38,126 B; SRR block spec)
- https://raw.githubusercontent.com/srrDB/pyrescene/master/resample/main.py (fetched; 126,513 B; SRS containers, FileData, TrackData)
- https://raw.githubusercontent.com/srrDB/pyrescene/master/resample/ebml.py (fetched; 16,913 B; EBML IDs)
- https://raw.githubusercontent.com/srrDB/pyrescene/master/resample/srs.py (fetched; 23,792 B; CLI)
- https://raw.githubusercontent.com/srrDB/pyrescene/master/resample/fpcalc.py (fetched; 9,734 B; not analysed)
- https://raw.githubusercontent.com/srrDB/pyrescene/master/README (fetched; 9,334 B)
- Code search index (gh api search/code): filename:RarInfo.php, filename:SrrInfo.php, filename:SzipInfo.php, "pyrescene resample srs"
