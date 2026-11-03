# W2c: Verified byte layouts (ZIP extra fields, RAR4/RAR5, ReScene SRR/SRS)

Status key: VERIFIED = read from source text in this pass (URL + line/function given). PARTIAL = some fields verified, rest not. UNVERIFIED = not confirmed from source in this pass.

Fetched sources (raw, master branch), saved under scratchpad `w2c/`:
- https://raw.githubusercontent.com/LuaDist/zip/master/proginfo/extrafld.txt (1372 lines, Info-ZIP extra-field spec)
- https://raw.githubusercontent.com/libarchive/libarchive/master/libarchive/archive_read_support_format_zip.c (4835 lines)
- https://raw.githubusercontent.com/libarchive/libarchive/master/libarchive/archive_read_support_format_rar.c (3943 lines)
- https://raw.githubusercontent.com/libarchive/libarchive/master/libarchive/archive_read_support_format_rar5.c (4690 lines)
- https://raw.githubusercontent.com/srrDB/pyrescene/master/rescene/rar.py (1680 lines)

All integers are little-endian unless stated. Offsets in extra-field tables are relative to the start of that extra block's data (after the 4-byte tag+size header).

---

## 1. ZIP extra fields

### 0x5455 "UT" extended timestamp: VERIFIED
- Spec: extrafld.txt "Extended Timestamp Extra Field" (Last Revision 19970118), approx lines 817-870.
  - Local header: TSize (2) | Flags (1) | ModTime (4, signed Unix UTC) | AcTime (4) | CrTime (4).
  - Central header: TSize (2) | Flags (1, refers to local header) | ModTime (4) only, or no timestamp (TSize says which).
  - Flags bit0 = mtime present, bit1 = atime present, bit2 = creation (ctime) present, bits 3-7 reserved (zero). Times appear in order mtime, atime, ctime. TSize should equal 1 + 4*popcount(flags) (local).
  - Rule: if the local Flags says mtime is present, it MUST also be present in the central field.
- Parser: libarchive archive_read_support_format_zip.c, case 0x5455, approx lines 907-963. Flags = p[offset] (1 byte), then mtime if bit0 (4 bytes), atime if bit1, ctime if bit2, each 4 bytes LE, read with archive_le32dec. Loop breaks if fewer than 4 bytes remain.
- Note: libarchive does not distinguish local vs central layout; it reads by flags bits, so a central record with only mtime works.

### 0x7875 "ux" Info-ZIP New Unix (UID/GID): VERIFIED
- Spec: extrafld.txt "Info-ZIP New Unix Extra Field" (Last Revision 20080509), approx lines 1339-1372.
  - Version (1, must be 1) | UIDSize (1) | UID (UIDSize bytes, LE) | GIDSize (1) | GID (GIDSize bytes, LE).
  - Supersedes 0x7855 if both present.
- Parser: archive_read_support_format_zip.c, case 0x7875, approx lines 1172-1206. Requires version byte == 1. Only uidsize/gidsize of 2 or 4 are decoded (TODO in source for others). GID offset = 2 + uidsize + 1 (the GIDSize byte) + 1.

### 0x000a NTFS: VERIFIED
- Spec: extrafld.txt, NTFS section, approx lines 194-232.
  - Reserved (4 bytes) | Tag1 (2, = 0x0001) | Size1 (2, = 24) | Mtime (8, FILETIME) | Atime (8, FILETIME) | Ctime (8, FILETIME = Windows creation time). Total data 32 bytes.
  - FILETIME = 64-bit unsigned 100 ns ticks since 1601-01-01 UTC.
- Parser: archive_read_support_format_zip.c, case 0x000a, approx lines 790-864. Skips 4 reserved bytes, walks Tag(2)/Size(2) sub-blocks, decodes only tag 1 with size 24. A raw FILETIME of 0 is treated as absent. The "Ctime" maps to birth/creation time, not POSIX ctime.

### 0x7075 "up" Info-ZIP Unicode Path: VERIFIED
- Spec: extrafld.txt, "(UPath) 0x7075", approx lines 1262-1290.
  - Version (1, currently 1) | NameCRC32 (4, standard zip CRC32 of the header File Name field) | UnicodeName (TSize - 5 bytes, UTF-8, no BOM).
  - Use only if version is recognized. If NameCRC32 mismatches the header name, ignore the field and use the header name.
- Parser: archive_read_support_format_zip.c, case 0x7075, approx lines 1106-1171. Requires datasize >= 5; offset += 5 (version + CRC). Compares crc32 of the current pathname (zip->crc32func(0, cp, strlen(cp))) against the stored CRC; mismatch -> ignore (approx lines 1130-1141). Converter is UTF-8.
- Note: libarchive checks the version byte only implicitly (it skips it, does not reject other versions). Spec says reject unknown versions.

### 0x6375 "uc" Info-ZIP Unicode Comment: PARTIAL
- Spec tag and purpose verified (extrafld.txt "Info-ZIP Unicode Comment Extra Field", approx line 1305 region, "(UCom) 0x6375 ... tag ("uc")", stores UTF-8 comment as in central directory). Exact field-by-field layout (Version, CRC, text) was not printed in this pass. Expected to mirror 0x7075 (Version 1 + CRC32 of the header comment + UTF-8 text), but this is UNVERIFIED until lines ~1305-1335 are read.

### 0x9901 WinZip AES: PARTIAL (libarchive layout VERIFIED, AE-2 CRC rule VERIFIED, WinZip spec itself not fetched)
- Layout as parsed by libarchive, archive_read_support_format_zip.c case 0x9901, approx lines 1208-1225:
  - Data size must be >= 6 (else error "Incomplete AES field").
  - Vendor version (2 bytes, LE) at offset 0: 0x0001 = AE-1, 0x0002 = AE-2 (defines at approx lines 119-120).
  - Vendor ID (2 bytes) at offset 2 must be ASCII 'A' 'E' (0x41 0x45).
  - AES strength (1 byte) at offset 4: 1 = AES-128, 2 = AES-192, 3 = AES-256. Key/salt sizes (approx lines 3457-3460): strength 1 -> salt 8, key 16; 2 -> salt 12, key 24; 3 -> salt 16, key 32. (Source comment at line ~114 has a typo "2 - 256"; the code switch is authoritative.)
  - Actual compression method (2 bytes in the WinZip spec; libarchive reads 1 byte at offset 5): stored into aes_extra.compression, and when the outer method is 99 (0x63) the entry compression is replaced by this value (approx line 3523).
- AE-2 CRC rule: libarchive approx lines 1781-1792: for AE-2 the stored CRC32 is always 0, so actual CRC is set to 0 and not checked. AE-1 may write CRC 0 sometimes, so zero CRC is tolerated (crc32_may_be_zero). Also approx line 1997 and 3649 treat AE-2 specially for the CRC check.
- The WinZip AES spec (http://www.winzip.com/aes_info.htm, cited at libarchive line ~154) was not fetched; the 2-byte "actual method" width is per WinZip spec, libarchive reads only one byte. UNVERIFIED against WinZip spec text.

### 0x5855 "UX" Info-ZIP Unix (old): VERIFIED
- Spec: extrafld.txt "Info-ZIP Unix Extra Field (type 1)", approx lines 874-913. Atime (4) | Mtime (4) | UID (2, optional) | GID (2, optional).
- Parser: archive_read_support_format_zip.c case 0x5855, approx lines 964-986. atime at offset 0, mtime at offset 4 (archive_le32dec), uid LE16 at offset 8 and gid LE16 at offset 10 only if datasize >= 12.

### 0x000d PKWARE Unix: VERIFIED
- Spec: extrafld.txt "PKWARE Unix Extra Field", approx lines 583-610. Local header only. Minimum data 12 bytes. AcTime (4) | ModTime (4) | UID (2) | GID (2) | variable (link target / device numbers).
- Parser: archive_read_support_format_zip.c case 0x000d, approx lines 865-886. atime = le32 at offset 0, mtime = le32 at offset 4, uid = le16 at offset 8, gid = le16 at offset 10. Variable tail not parsed.

---

## 2. RAR4 Unicode FILE_NAME decoding: VERIFIED
- Flag FHD_UNICODE = 0x0200 (libarchive archive_read_support_format_rar.c, defines approx lines 80-89).
- Name field: NUL-terminated ASCII name, then (if FHD_UNICODE) the encoded stream. Field length is NAME_SIZE (LE16 in file header).
- libarchive read_header(), approx lines 1555-1660:
  1. `end = name_size`; `fn_end = name_size * 2` (output byte limit, UTF-16BE).
  2. `offset = strlen(name) + 1` (skip ASCII part and its NUL). `highbyte = p[offset++]` (if offset < end, else 0).
  3. Loop while offset < end and output < fn_end. Flag byte read when flagbits==0: `flagbyte = p[offset++]; flagbits = 8`. Each step: `flagbits -= 2; code = (flagbyte >> flagbits) & 3` (2-bit codes consumed MSB first).
  4. Codes:
     - 0: write 0x00, then p[offset++] (plain byte, high 0).
     - 1: write highbyte, then p[offset++].
     - 2: two bytes: write p[offset+1], then p[offset]; offset += 2. (word pair, output is UTF-16BE char = (p[offset+1]<<8)|p[offset]).
     - 3: length byte L = p[offset++]. If L & 0x80: extra = p[offset++] and high = highbyte, else extra = high = 0. count = (L & 0x7f) + 2. Repeat count times: write high, then (p[cp] + extra) where cp = output_bytes >> 1 (char index into ASCII name).
  5. Output stream is UTF-16BE, converted with sconv_utf16be. Backslash path separators converted to '/' in UTF-16 pairs ("\x00\\" -> "/"). Approx lines 1640-1660.
- If FHD_UNICODE is clear, name is ANSI/OEM and only '\' -> '/' replacement applies (approx lines 1670-1690).

## 3. RAR4 EXTTIME (FHD_EXTTIME = 0x1000): VERIFIED for libarchive layout, NS_UNIT not verified
- Flag define: archive_read_support_format_rar.c line 89. read_exttime() call approx line 1768.
- Layout (read_exttime, approx lines 1886-1975):
  - 2 bytes: flags (LE16). Must have 2 bytes remaining.
  - For i = 3, 2, 1, 0 (in that order: mtime, ctime, atime, arctime): rmode = flags >> (i*4) (4 bits per time).
    - bit 3 (0x8) = time present.
    - bit 2 (0x4) = "add one second" (libarchive: tm_sec++ before mktime; odd-second rounding).
    - bits 0-1 (0x3) = count of extra remainder bytes (0-3).
  - Base time: for mtime (i==3) the base is the file header's DOS FTIME (rar->mtime). For the others, if not already set, a 4-byte LE32 DOS time follows (approx lines 1912-1920) then `count` remainder bytes.
  - Remainder: accumulated little-endian: rem = (byte << 16) | (rem >> 8) for each of count bytes (so 3-byte max). Sub-second = rem / NS_UNIT added to tm_sec as nsec. NS_UNIT value not printed in this pass (expected 100 ns ticks, UNVERIFIED). Read the `#define NS_UNIT` in archive_read_support_format_rar.c to confirm.
- Ordering bug to watch: the 4-bit fields are read from the flags word, but the 4-byte base time and the remainder bytes are consumed from the stream in the same order as the loop (mtime first). Parse order matters.

## 4. RAR recovery records

### RAR4 PROTECT_HEAD (0x78) and NEWSUB (0x7A): PARTIAL
- Header type defines: PROTECT_HEAD = 0x78, NEWSUB_HEAD = 0x7a (archive_read_support_format_rar.c lines 58, 60).
- libarchive handling of PROTECT_HEAD (approx lines 1028-1070): flags = LE16 at p+3; HEAD_SIZE = LE16 at p+5 (must be >= 7). If flags & HD_ADD_SIZE_PRESENT (0x8000), ADD_SIZE = LE32 at p+7 and skip += ADD_SIZE (must have >= 11 bytes). CRC16 is the first 2 bytes of the header; the rest is CRC'd with crc32 & 0xFFFF.
- The RR-specific fields after the 7-byte common header (version byte, RecSectors, TotalBlocks, Mark) are NOT parsed by libarchive (it only skips). Layout from memory, UNVERIFIED: after HEAD_SIZE, [ADD_SIZE], VERSION (1), RECSECTORS (2), TOTALBLOCKS (4), MARK (8, "RR" signature bytes). Need unrar `recvol.cpp` / `rs16` to confirm.
- NEWSUB (0x7A) in libarchive read_header() approx lines 1515-1535: treated as a file-like header; packed_size + header_size are skipped; metadata-only, not parsed.
- Recovery percentage: computed from RECSECTORS / TOTALBLOCKS in unrar; NOT VERIFIED here (libarchive does not compute it).

### RAR5 service header "RR": PARTIAL
- Header type HEAD_SERVICE = 0x03 (libarchive rar5.c approx line 2436). Service blocks are parsed like FILE headers (process_head_service, approx lines 2177-2195), data is skipped. No RR content parsed.
- Main-archive locator extra block: LOCATOR flags enum QLIST = 0x01, RECOVERY = 0x02 (approx line 1262). rr_offset (uint64) stored in struct but "not supported by this unpacker" (approx lines 359-361).
- The internal "RR" service record name and its data layout (recovery percentage, RR data count, etc.) are NOT verified here. Need unrar `recvol5.cpp` or libarchive's rar5 spec to confirm. UNVERIFIED.

---

## 5. ReScene SRR block layouts (pyrescene rar.py): VERIFIED for listed blocks

Common header (7 bytes), rar.py class RarBlock, approx lines 311-378:
- HEAD_CRC (2) | HEAD_TYPE (1) | HEAD_FLAGS (2) | HEAD_SIZE (2) (struct "<HBHH" at offset 0; line 362).
- If flags & LONG_BLOCK (0x8000): ADD_SIZE (4, "<I" at offset 7) (line 368). Total header span = HEAD_SIZE; ADD_SIZE gives data size.
- HEAD_CRC: CRC of header bytes (RAR), lower 16 bits of CRC32 (per rar.py docstring line ~311-322). SRR blocks use fixed magic CRCs (see below), not computed.

Block type table (rar.py BlockType, approx lines 212-236; BLOCK_NAME approx lines 246-258):
- 0x69 SRR Volume Header. Magic CRC 0x6969 (not a real CRC, rar.py line ~534). FLAG 0x0001 = application name present. If present: APNS (2, LE) then APNS bytes of app name (approx lines 475-545; name at line ~517). Max APNS 0xFFF6.
- 0x6A SRR Stored File (e.g. .nfo, .sfv). CRC 0x6A6A. FLAG must include LONG_BLOCK 0x8000. HL = HEAD_SIZE (2). SIZE = file size (4, "<I", rar.py approx line 605). NL = name length (2, LE). Name = path with '\' separators (approx lines 564-650; _unpack_file_name approx lines 440-463).
- 0x6B SRR ISDb/OSO hash block (previously named OSO). CRC 0x6B6B. FLAG = 0. HL (2). SIZE = file size (8, "<Q", approx line 781). OSO HASH = 64-bit (8, "<Q"), formatted %016x (approx line 785). NL (2). File name. OSO hash = 64-bit checksum of first and last 64 KiB (per docstring line ~756-760, referencing opensubtitles hash).
- 0x6C SRR Padding. CRC 0x6C6C. FLAG = LONG_BLOCK. HL = always 11 (7 + 4). Padding size 4 bytes "<I" (approx lines 819-855). Used for bytes after the RAR end block.
- 0x71 SRR RAR subblock (one per RAR file). CRC 0x7171. FLAG 0x0001 = recovery records were removed from this RAR. HEAD_SIZE (2). NL (2). RAR file name (approx lines 699-730). Max name 0xFFF6.
- 0x72 RAR marker, 0x73 RAR archive header, 0x74 RAR file, 0x7A NEWSUB (RR, CMT, AV): normal RAR block types (rar.py 0x72-0x7B, approx lines 226-236 and 246-258).
- SRR block numbers 0x69-0x71 are chosen to be below the RAR range 0x72-0x7B.
- NEWSUB (0x7A) CRC note: rar.py docstring at approx line 1222-1230 says "crc = crc32(data, ~0x0fffffff)" with a link to a StackOverflow answer. Unverified as normative; treat as hint only.

### SRS (MKV/MP4/AVI sample tracks): UNVERIFIED
- Not present in the fetched files. Needs rescene/srs.py or the ReScene .NET source (srr format doc). NOT VERIFIED in this pass.

---

## Summary of status
- VERIFIED: UT 0x5455, UX 0x5855, 0x000a NTFS, 0x000d PKWARE, 0x7075 UPath, 0x7875 ux, 0x9901 layout + AE-2 CRC rule, RAR4 FHD_UNICODE name decode, RAR4 EXTTIME layout (except NS_UNIT), RAR4 PROTECT/NEWSUB header skip, RAR5 service and locator flags (as handled by libarchive), pyrescene SRR blocks 0x69, 0x6A, 0x6B, 0x6C, 0x71.
- PARTIAL: 0x6375 (tag only), 0x9901 (WinZip spec not fetched), RAR4 PROTECT/RR fields, RAR5 RR content.
- UNVERIFIED: recovery percentage formula for RAR4 and RAR5, RAR5 RR internal layout, RAR EXTTIME NS_UNIT constant, SRS format.

## Next steps (not done, time cap)
- unrar: encname.cpp (for an independent check of the RAR4 decoder), arcread.cpp ReadHeader14/15 (EXTTIME), recvol.cpp / recvol5.cpp (RR layout and percentage).
- pyrescene rescene/srs.py (SRS block types).
- WinZip AES spec http://www.winzip.com/aes_info.htm (2-byte vs 1-byte compression field).

## Sources
- https://raw.githubusercontent.com/LuaDist/zip/master/proginfo/extrafld.txt
- https://raw.githubusercontent.com/libarchive/libarchive/master/libarchive/archive_read_support_format_zip.c
- https://raw.githubusercontent.com/libarchive/libarchive/master/libarchive/archive_read_support_format_rar.c
- https://raw.githubusercontent.com/libarchive/libarchive/master/libarchive/archive_read_support_format_rar5.c
- https://raw.githubusercontent.com/srrDB/pyrescene/master/rescene/rar.py
- http://www.winzip.com/aes_info.htm (cited in libarchive, NOT fetched)
