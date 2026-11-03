# W1a: RAR metadata inventory (RAR4 and RAR5)

Scope: every field readable from RAR 1.5-4.x (RAR4) and RAR5 archives, for a Rust Usenet indexer working from partial downloads (first 1-6 articles of ~700 KB each, sometimes first+last article of each volume).

Conventions:
- `fmt`: rar4 | rar5 | both
- `where`: offset from start of the header block (`@hdr+N`), or from file start (`@file+N`) for the signature. Integers little-endian. `vint` = RAR5 variable int (7 bits per byte, LSB group first, bit 8 = continuation, up to 10 bytes).
- `read_from`: `first_article` = inside the first ~700 KB of that volume; `last_article` = needs the tail of that volume; `full` = needs decompression or the complete file.
- `use:` one-line reason for the indexer.
- Anything not confirmed in a primary source is tagged UNVERIFIED.

Important caveat for every "per-volume" field: a RAR4 or RAR5 volume is self-describing. Each volume begins with signature, main header, then its own file header(s), so the first article of every volume gives the header of the part that starts there.

---

## 0. Detection and overall layout

```
signature (7 or 8 bytes)  ->  main header  ->  [service headers, e.g. CMT, QO]  ->  file headers (+ their service subheaders)  ->  [RR / PROTECT]  ->  end-of-archive header
```

- signature_rar4:
  - fmt: rar4
  - where: @file+0, 7 bytes `52 61 72 21 1A 07 00` ("Rar!\x1a\x07\x00")
  - read_from: first_article
  - use: identifies RAR 1.5 through 4.x; source: unrar `rardefs`/`headers.hpp` (`HEAD3_MARK=0x72`); libarchive `RAR_SIGNATURE` in la_rar4.c L49
- signature_rar5:
  - fmt: rar5
  - where: @file+0, 8 bytes `52 61 72 21 1A 07 01 00`
  - read_from: first_article
  - use: identifies RAR5; technote section 2
- sfx_scan:
  - fmt: both
  - where: search from file start up to ~1 MB (RAR5 technote); libarchive scans with the same signature
  - read_from: first_article
  - use: self-extracting `.exe` wrappers push the signature to a non-zero offset. All later offsets are relative to the signature, not to the file.

Note: Usenet RAR sets are normally plain, so sfx_scan only matters for the odd `.exe` post.

---

## 1. RAR5 block framing (all RAR5 headers)

Common prefix of every RAR5 block (technote section 3):

- r5_hdr_crc:
  - fmt: rar5
  - where: @hdr+0, uint32 LE; CRC32 over header bytes from `Header size` through end of extra area
  - read_from: first_article
  - use: integrity check of header; lets you trust header parsing on a partial read
- r5_hdr_size:
  - fmt: rar5
  - where: @hdr+4, vint (max 3 bytes = 2 MB); counts from `Header type` through end of extra area (the CRC and this size field are excluded)
  - read_from: first_article
  - use: total header length; the next data begins at hdr+4+len(vint)+hdr_size
- r5_hdr_type:
  - fmt: rar5
  - where: vint after size. 0x00 mark (unused), 0x01 MAIN, 0x02 FILE, 0x03 SERVICE, 0x04 CRYPT (archive encryption), 0x05 ENDARC, 0xFF unknown (libarchive `HEADER_TYPE`, unrar `HEADER_TYPE`)
  - read_from: first_article
  - use: dispatch
- r5_hdr_flags:
  - fmt: rar5
  - where: vint. 0x0001 extra area present; 0x0002 data area present; 0x0004 unknown block, skip when updating; 0x0008 data split from previous volume (SPLIT_BEFORE); 0x0010 data continues in next volume (SPLIT_AFTER); 0x0020 depends on preceding file block (child); 0x0040 preserve child if host modified (INHERITED)
  - read_from: first_article
  - use: SPLIT_BEFORE/AFTER tells you a file is spread over volumes without parsing names
- r5_extra_area_size:
  - fmt: rar5
  - where: vint, present only if hdr_flags & 0x0001
  - read_from: first_article
  - use: lets you skip unknown extra records
- r5_data_size:
  - fmt: rar5
  - where: vint, present only if hdr_flags & 0x0002 (for FILE/SERVICE it is the packed size of this part)
  - read_from: first_article (for the header); the data itself follows the header
  - use: packed bytes of this block; block length = header + data
- r5_data_area_start:
  - fmt: rar5
  - where: file offset = block_start + 4 + len(hdr_size vint) + hdr_size. Data area is not covered by the CRC.
  - read_from: first_article
  - use: byte map. Gives the exact start of a compressed or stored stream inside volume 1 (for store mode this is where the inner MKV bytes start)

---

## 2. RAR5 MAIN header (type 0x01)

Fields after the common prefix:

- r5_main_extra_size:
  - fmt: rar5
  - where: vint, if flags & 0x0001
  - read_from: first_article
  - use: skip or parse extras
- r5_main_archive_flags:
  - fmt: rar5
  - where: vint. 0x0001 VOLUME (multi-volume member); 0x0002 VOLUME_NUMBER field present (every volume except the first); 0x0004 SOLID archive; 0x0008 RECOVERY record present; 0x0010 LOCKED (libarchive: "not used")
  - read_from: first_article
  - use: multi-volume set detection, solid archive, RR present. Cross-check with `-rr` leak below
- r5_main_volume_number:
  - fmt: rar5
  - where: vint, present only if archive_flags & 0x0002. First volume omits it; second is 1, third 2, and so on.
  - read_from: first_article
  - use: exact volume index for ordering; a bare first-volume file has no number, so detect part1 by its absence
- r5_main_extra_locator (extra type 0x01):
  - fmt: rar5
  - where: extra record: vint size, vint type=1, vint flags, then vint QLIST offset if flags&0x01, vint RECOVERY offset if flags&0x02
  - read_from: first_article (the offsets point into the file, usually near the end, so the target is last_article)
  - use: locates the QO (quick open) cache and the RR block without scanning. Offsets are relative to the start of the target block (technote section 6.7 / 5.1)
- r5_main_extra_metadata (extra type 0x02):
  - fmt: rar5
  - where: vint size, vint type=2, vint flags. flags 0x01 archive name present (vint length + UTF-8 bytes, no terminator); 0x02 original creation time present; 0x04 time is Unix if set else 8-byte FILETIME; 0x08 Unix time has nanoseconds (8 bytes) instead of seconds (4 bytes)
  - read_from: first_article
  - use: the original archive file name baked in at creation (often the poster's naming) and the original creation time. Low prevalence; high signal for dedup and for poster software identity
- r5_main_extra_unknown:
  - fmt: rar5
  - where: any other extra type; skip by its size
  - read_from: first_article
  - use: forward compatibility

Main header absent-field notes: RAR5 main headers carry no per-archive "packer software" string. The leaks are in §11.

---

## 3. RAR5 ARCHIVE ENCRYPTION header (type 0x04, `-hp` header encryption)

- r5_crypt_version:
  - fmt: rar5
  - where: vint, 0 = AES-256 (only defined value)
  - read_from: first_article
  - use: encrypted headers present; you cannot list files without the password
- r5_crypt_flags:
  - fmt: rar5
  - where: vint. 0x0001 password check data present
  - read_from: first_article
  - use: lets you reject a wrong password before decrypting headers
- r5_crypt_kdf_count:
  - fmt: rar5
  - where: 1 byte, binary log of PBKDF2 iteration count
  - read_from: first_article
  - use: derivation cost; a tell that the archive was made with a standard `-hp` password
- r5_crypt_salt:
  - fmt: rar5
  - where: 16 bytes, global salt for encrypted headers
  - read_from: first_article
  - use: needed for key derivation
- r5_crypt_check:
  - fmt: rar5
  - where: 12 bytes if flags & 0x0001. First 8 bytes from extra PBKDF2 rounds; last 4 bytes are a checksum (technote section 4). Algorithm details: UNVERIFIED beyond that
  - read_from: first_article
  - use: password check without decrypting content
- r5_encrypted_header_iv:
  - fmt: rar5
  - where: after an archive encryption header, each following header starts with a 16-byte AES-256 IV, then the encrypted header data padded to 16 bytes
  - read_from: first_article
  - use: you cannot parse any name, size, or time while this is on, so these fields are unreadable without the key

---

## 4. RAR5 FILE header (type 0x02) and SERVICE header (type 0x03)

Both share the same layout. Fields after the common prefix:

- r5_file_extra_size:
  - fmt: rar5
  - where: vint, if flags&0x0001
  - read_from: first_article
  - use: bounds the extra records below
- r5_file_data_size:
  - fmt: rar5
  - where: vint, if flags&0x0002. For FILE: packed size of this block's data (this volume's part only for split files)
  - read_from: first_article
  - use: packed size. For SPLIT files, sum over volumes gives the total packed size
- r5_file_flags:
  - fmt: rar5
  - where: vint. 0x0001 DIRECTORY (FILE only); 0x0002 UTIME (mtime present); 0x0004 CRC32 present; 0x0008 UNKNOWN_UNPACKED_SIZE (decompress until end-of-stream)
  - read_from: first_article
  - use: directory vs file, whether a CRC32 exists, and whether unpacked size is trusted
- r5_file_unpacked_size:
  - fmt: rar5
  - where: vint, always present. If flags&0x0008, ignore and decompress to end of stream
  - read_from: first_article (full unpacked size for the file as a whole, not just this part; UNVERIFIED for split parts, verify on a real split set)
  - use: total unpacked size, the key number for "is the MKV complete" checks
- r5_file_attributes:
  - fmt: rar5
  - where: vint, OS-specific. Windows: READONLY 0x1, HIDDEN 0x2, SYSTEM 0x4, DIRECTORY 0x10 (libarchive enums). Unix: st_mode bits (0o755 etc.). 0 for service headers
  - read_from: first_article
  - use: POSIX mode from Unix packers (leaks uploader OS and permissions)
- r5_file_mtime_unix:
  - fmt: rar5
  - where: uint32 LE, present if file_flags&0x0002 (Unix time_t, seconds). This is the basic mtime, so it is the fallback if no htime extra record
  - read_from: first_article
  - use: file modification timestamp
- r5_file_crc32:
  - fmt: rar5
  - where: uint32 LE, present if file_flags&0x0004. For split files, covers this volume's packed data for all parts except the last; for unsplit files and the last part, covers the unpacked data (technote 6.2)
  - read_from: first_article for the header. Full verify needs full (unpacked). Partial parts cannot be verified this way
  - use: content fingerprint (CRC32) only for unsplit or last-part entries; not a dedup key for first parts
- r5_file_comp_info:
  - fmt: rar5
  - where: vint bitfield (technote section 6, bits numbered from 1):
    - bits 1-6 (mask 0x003f): algorithm version, 0 (RAR 5.0+) or 1 (RAR 7.0+)
    - bit 7 (0x0040): SOLID flag (FILE only)
    - bits 8-10 (0x0380): method, 0 = store, 1-5 = compressed (`-m0` = 0)
    - bits 11-15 (0x7c00): dictionary exponent N, size = 128 KB * 2^N (0 = 128 KB, 15 = 4 GB, 19 = 64 GB, 23 = 1 TB max)
    - bits 16-20 (0xf8000): version-1 only, fractional dictionary size = dict * (value/32) added to dict
    - bit 21 (0x100000): version-1 flag, algorithm actually version 0 (appending v1 files to a v0 solid stream)
  - read_from: first_article
  - use: THE packer fingerprint field. Method 0 means store mode: inner MKV/MP4 bytes are raw at the data-area offset and can be read without a decompressor. Dict exponent and version bit reveal the -md setting and the RAR version required
- r5_file_host_os:
  - fmt: rar5
  - where: vint. 0 = Windows, 1 = Unix (libarchive HOST_UNIX=1)
  - read_from: first_article
  - use: decides whether the attribute field is POSIX mode or Windows attributes, and whether names are forward-slash separated (they always are in RAR5 per technote, but backslash may appear in Unix names)
- r5_file_name_len:
  - fmt: rar5
  - where: vint
  - read_from: first_article
  - use: length of the UTF-8 name that follows
- r5_file_name:
  - fmt: rar5
  - where: UTF-8 bytes, no trailing NUL, `/` is the separator. A Unix name with high-ASCII bytes maps to U+E080-U+E0FF (private use) and U+FFFE is inserted to flag it (technote section 8). Pre-mapped names are not portable
  - read_from: first_article
  - use: file name and path; the U+E0xx/U+FFFE pattern tells you a non-portable Unix name
- r5_file_extra (see §5 for the 7 record types)
- r5_file_data_area: starts at the value of §1 `r5_data_area_start`. Data is compressed or stored per comp_info method

Service headers (type 0x03) reuse the same layout. Names are ASCII in the name field:

- r5_service_name:
  - fmt: rar5
  - where: name field of a SERVICE header, values: `CMT` (archive comment), `QO` (quick open cache), `ACL` (NTFS ACL), `STM` (NTFS alternate data stream), `RR` (recovery record). Service data is always stored uncompressed (method 0) in CMT and QO per technote 8.1/8.2
  - read_from: first_article for CMT when it sits near the start (after the main header); QO and RR are near the end, so last_article
  - use: CMT gives the archive comment text (poster notes, group tags, scene NFOs); STM gives the NTFS stream name (Zone.Identifier = Mark-of-the-Web, see §5 note)

---

## 5. RAR5 file-header EXTRA records (the 7 types)

Extra records sit at the end of the file or service header and are read only if flags&0x0001. Each record = vint size, vint type, then type-specific data. Unknown types are skipped by size (libarchive `process_head_file_extra`, technote 6).

- r5_extra_encryption (type 0x01):
  - fmt: rar5
  - where: vint version (0 = AES-256), vint flags (0x0001 password check present; 0x0002 TWEAKED checksums, meaning the file CRC32 and the hash record depend on the key), 1-byte KDF count, 16-byte salt (per-file), 16-byte IV, 12-byte check value if flags&0x0001
  - read_from: first_article
  - use: per-file encryption detection plus parameters; if 0x0002 is set, the CRC32 in the header is not a plain CRC and you cannot use it as a content fingerprint
- r5_extra_hash (type 0x02):
  - fmt: rar5
  - where: vint hash_type (0x00 = BLAKE2sp), then 32-byte hash value for type 0x00
  - read_from: first_article for the header. For SPLIT files the hash covers this volume's packed data for all parts except the last, so it only equals a content fingerprint for unsplit files and last parts
  - use: BLAKE2sp digest of the content. For an unsplit file this is a strong content identity: identical releases from different posters produce the same hash, so it works as a dedup key. Verify requires the full content
- r5_extra_time (type 0x03):
  - fmt: rar5
  - where: vint flags. 0x0001 Unix time_t (uint32) if set, else Windows FILETIME (uint64); 0x0002 mtime present; 0x0004 ctime present; 0x0008 atime present; 0x0010 nanosecond precision. Then mtime/ctime/atime values (uint32 or uint64 by 0x0001); then mtime_ns, ctime_ns, atime_ns (uint32 each) present when 0x0001 AND 0x0010 AND the time field is present
  - read_from: first_article
  - use: high-precision mtime/ctime/atime with ns resolution. The ctime field is "change time" (Unix packers write it, Windows writes creation); useful as packer clock fingerprint
- r5_extra_version (type 0x04):
  - fmt: rar5
  - where: vint flags (0), vint version number
  - read_from: first_article
  - use: file version created by `-ver`. Name gets `;N` suffix. Rare on Usenet
- r5_extra_redirection (type 0x05):
  - fmt: rar5
  - where: vint redirect type (0x0001 Unix symlink, 0x0002 Windows symlink, 0x0003 Windows junction, 0x0004 hard link, 0x0005 file copy), vint flags (0x0001 target is directory), vint name length, UTF-8 target name
  - read_from: first_article
  - use: symlinks and hard links inside a release. A symlink entry has no data, so it breaks "the largest file is the movie" heuristics. The technote lists the name field as "vint"; libarchive reads name_len bytes (UNVERIFIED wording, the byte count is what matters)
- r5_extra_unix_owner (type 0x06):
  - fmt: rar5
  - where: vint flags (0x0001 user name present; 0x0002 group name present; 0x0004 numeric uid present; 0x0008 numeric gid present). Then if set: vint user-name-length + bytes (native encoding, not NUL-terminated); group same; vint uid; vint gid
  - read_from: first_article
  - use: uploader's Unix account name and numeric ids (e.g. `root`, uid 1000). Direct leak of packer identity on Linux/macOS builds
- r5_extra_service_data (type 0x07):
  - fmt: rar5
  - where: service-header payload (bytes); meaning depends on the service name (§4)
  - read_from: last_article for QO/RR, first_article for CMT
  - use: carries the CMT text or the QO cache

Note on STM / Zone.Identifier (applies to both formats): an NTFS alternate stream named `:Zone.Identifier` (Mark-of-the-Web, stored as an STM service in RAR5 and as a NEWSUB "STM" in RAR4) holds a small INI with `ZoneId=3`, and sometimes `HostUrl`/`ReferrerUrl`. It is stored inside the archive only when the source file was downloaded on Windows, so it can reveal the origin URL. Contents and presence: UNVERIFIED for this archive set, check on a sample.

---

## 6. RAR5 ENDARC header (type 0x05)

- r5_endarc_flags:
  - fmt: rar5
  - where: vint. 0x0001 archive is a volume and NOT the last volume in the set
  - read_from: last_article (it is the last header of each volume)
  - use: the cheapest "is there a next part?" test. Read the last article of each volume and check bit 0x0001. Then you know whether to expect a continuation without the next volume's header
- r5_endarc_trailer:
  - fmt: rar5
  - where: no fields after the ENDARC header. RAR reads nothing after it, so a digital signature or other data can be appended (technote section 7)
  - read_from: last_article
  - use: tail-append data (signatures, tags written by third-party tools) lives after this point

---

## 7. RAR5 RR and QO structures

- r5_rr_block:
  - fmt: rar5
  - where: a SERVICE header named `RR`. Its internal layout is NOT in the technote ("gives no layout for the recovery record block"). Present when main archive_flags & 0x0008 is set and locator RECOVERY offset != 0
  - read_from: last_article (located via locator offset)
  - use: recovery record presence and approximate size. Size-to-data ratio is the `-rr` percentage leak, but the exact encoding of the percentage is UNVERIFIED
- r5_qo_block:
  - fmt: rar5
  - where: SERVICE header `QO`, located via locator QLIST offset. Data is an array of cache structures: uint32 CRC32 (from structure size onward), vint structure size (max 2 MB), vint flags (=0), vint offset (from start of QO block to cached data), vint data size, bytes of cached header data (technote 8.2)
  - read_from: last_article
  - use: a full cached copy of headers. If you can reach it, you get file headers without parsing from the data area. Use the same source (QO or not) for listing and extraction (technote warning). Presence = `-qo` leak
- r5_locator_unverified_note:
  - fmt: rar5
  - where: RAR5 cannot put locator offsets on RAR4-style fields. Only the RAR5 locator (extra type 0x01) has them
  - read_from: first_article
  - use: lets you jump to QO/RR without scanning the whole volume

---

## 8. RAR4 block framing (all RAR4 headers)

Common 7-byte base for every block (`SIZEOF_SHORTBLOCKHEAD=7`, `BaseBlock` in unrar headers.hpp):

- r4_head_crc:
  - fmt: rar4
  - where: @hdr+0, uint16 LE; CRC16 (low 16 bits of CRC32) over the rest of the header
  - read_from: first_article
  - use: header integrity
- r4_head_type:
  - fmt: rar4
  - where: @hdr+2, 1 byte. 0x72 MARK (signature block), 0x73 MAIN, 0x74 FILE, 0x75 CMT (old RAR 2.x comment), 0x76 AV (authenticity verification, deprecated), 0x77 OLDSERVICE (RAR2.x SUB_HEAD), 0x78 PROTECT (recovery record), 0x79 SIGN (old), 0x7A NEWSUB (subblocks: CMT, QO, ACL, STM, UOW, AV, RR, EA2), 0x7B ENDARC
  - read_from: first_article
  - use: block dispatch
- r4_head_flags:
  - fmt: rar4
  - where: @hdr+3, uint16 LE; per-type meaning (see §9-§13). Bit 0x8000 = LONG_BLOCK (an ADD_SIZE field follows HEAD_SIZE). Bit 0x4000 = SKIP_IF_UNKNOWN
  - read_from: first_article
  - use: drives every flag test below
- r4_head_size:
  - fmt: rar4
  - where: @hdr+5, uint16 LE; total header length including base, all fixed fields, name, salt, extended time
  - read_from: first_article
  - use: data after the header starts at block_start + HEAD_SIZE
- r4_add_size:
  - fmt: rar4
  - where: @hdr+7, uint32 LE, present only if flags & 0x8000. For FILE/NEWSUB blocks this is the low 32 bits of the packed data size (libarchive / unrar `ReadHeader15`: `hd->DataSize = Raw.Get4()`, then `PackSize = High:DataSize`)
  - read_from: first_article
  - use: packed data size. NOTE: for file blocks this 4-byte field is the first byte-field after HEAD_SIZE, and the PACK_SIZE field in the fixed section is the same value (see §9). Verified against unrar arcread.cpp L272 and L336

Data-area rule (RAR4): data (if any) starts at `block_start + HEAD_SIZE` and is `PACK_SIZE` bytes long (`PACK_SIZE` plus HIGH_PACK_SIZE<<32 when LHD_LARGE is set). Next block at `block_start + HEAD_SIZE + PACK_SIZE` (unrar arcread.cpp L1333: `NextBlockPos=CurBlockPos+HeadSize+PackSize`).

---

## 9. RAR4 MAIN header (type 0x73)

Fixed fields after the base (total 13 bytes including base):

- r4_main_reserved:
  - fmt: rar4
  - where: @hdr+7, uint16 HighPosAV, @hdr+9 uint32 PosAV; base is 13 bytes total. Both are legacy AV fields
  - read_from: first_article
  - use: legacy only; ignore
- r4_main_flags (bits from unrar headers.hpp):
  - fmt: rar4
  - where: @hdr+3 (block flags)
    - 0x0001 MHD_VOLUME: member of a multi-volume set
    - 0x0002 MHD_COMMENT: old-style inline main comment (RAR 2.x; deprecated)
    - 0x0004 MHD_LOCK: locked archive
    - 0x0008 MHD_SOLID: solid archive
    - 0x0010 MHD_NEWNUMBERING: RAR 3.x+ naming (`.part01.rar`); clear = old `.rar`/`.r00` naming
    - 0x0020 MHD_AV: authenticity verification present (deprecated)
    - 0x0040 MHD_PROTECT: recovery record present (PROTECT block exists)
    - 0x0080 MHD_PASSWORD: headers encrypted (`-hp`), every later block is preceded by an 8-byte salt (unrar arcread.cpp `SIZE_SALT30` read; UNVERIFIED exact per-block layout)
    - 0x0100 MHD_FIRSTVOLUME: first volume of the set (only set for RAR 3.x+ encrypted archives per unrar comment; for others infer from name)
    - 0x0200 MHD_ENCRYPTVER: an encryption-version byte follows the main header (libarchive la_rar4.c L988; UNVERIFIED exact offset)
  - read_from: first_article
  - use: solid, locked, recovery record, multi-volume, header encryption, and naming scheme (`.partNN.rar` vs `.rNN`) all live here. MHD_NEWNUMBERING is the cleanest "RAR 3.x+ naming" signal
- r4_main_comment_inline:
  - fmt: rar4
  - where: MHD_COMMENT set, comment follows the main header (RAR 2.x; unrar `MHD_COMMENT`)
  - read_from: first_article
  - use: legacy comment, rare

RAR4 has no locator record. QO and RR offsets are not in the main header (unlike RAR5). They are found by scanning the NEWSUB headers named `QO` and the PROTECT block.

---

## 10. RAR4 FILE header (type 0x74)

Fixed section, offsets from header start, after the 7-byte base (total fixed size = 32 bytes, `SIZEOF_FILEHEAD3`):

- r4_file_pack_size_low:
  - fmt: rar4
  - where: @hdr+7, uint32 LE (= ADD_SIZE field when LONG_BLOCK, which is set on FILE headers)
  - read_from: first_article
  - use: packed size low 32 bits; add HIGH_PACK_SIZE<<32 when LHD_LARGE
- r4_file_unp_size_low:
  - fmt: rar4
  - where: @hdr+11, uint32 LE. Value 0xFFFFFFFF without LHD_LARGE means unknown size (decompress to end of stream)
  - read_from: first_article
  - use: unpacked size, with HIGH_UNP_SIZE if LHD_LARGE
- r4_file_host_os:
  - fmt: rar4
  - where: @hdr+15, 1 byte. 0 = MS-DOS, 1 = OS/2, 2 = Win32, 3 = Unix, 4 = Mac OS, 5 = BeOS (unrar `HOST_SYSTEM`)
  - read_from: first_article
  - use: OS of the packer. Unix (3) means the attribute field is POSIX mode bits (unrar arcread.cpp L302-L311)
- r4_file_crc32:
  - fmt: rar4
  - where: @hdr+16, uint32 LE, CRC32 of the unpacked file. Full file CRC, carried in each part's header (UNVERIFIED for split parts, check on a real split set)
  - read_from: first_article
  - use: content fingerprint for unsplit files, and an integrity check after full decompression
- r4_file_dos_time:
  - fmt: rar4
  - where: @hdr+20, uint32 LE. Low 16 bits = time (hour<<11 | min<<5 | sec/2), high 16 bits = date (year-1980<<9 | month<<5 | day). 2-second resolution
  - read_from: first_article
  - use: mtime fallback when no EXTTIME record; local-time, no timezone
- r4_file_unp_ver:
  - fmt: rar4
  - where: @hdr+24, 1 byte. 15 = RAR 1.5 compression, 20 = RAR 2.0, 29 = RAR 3.x / 4.x (unrar `VER_UNPACK=29`). RAR5 never uses this field, it uses comp_info version
  - read_from: first_article
  - use: the RAR version needed to extract. 29 is the common modern value; 15 or 20 means an ancient or unusual packer
- r4_file_method:
  - fmt: rar4
  - where: @hdr+25, 1 byte. 0x30 = store (`-m0`), 0x31-0x35 = compressed fastest (`-m1`) to best (`-m5`)
  - read_from: first_article
  - use: store mode means inner media bytes are raw at the data offset (readable without a decompressor). This is the single most important field for the partial-download indexer
- r4_file_name_size:
  - fmt: rar4
  - where: @hdr+26, uint16 LE
  - read_from: first_article
  - use: length of the name field
- r4_file_attr:
  - fmt: rar4
  - where: @hdr+28, uint32 LE. DOS/Windows attributes (`FILE_ATTRIBUTE_DIRECTORY` = 0x10) for host_os 0-2; for host_os 3 (Unix), the high 16 bits hold st_mode (`attr & 0xF000` is the type, e.g. 0xA000 = symlink, unrar L311)
  - read_from: first_article
  - use: directory detection, Unix mode, symlink detection on Unix builds
- r4_file_high_pack_size:
  - fmt: rar4
  - where: @hdr+32, uint32 LE, present only if flags & 0x0100 (LHD_LARGE). Followed by HIGH_UNP_SIZE uint32
  - read_from: first_article
  - use: high halves of the 64-bit sizes; needed for files over 4 GB
- r4_file_high_unp_size:
  - fmt: rar4
  - where: after HIGH_PACK_SIZE, present only if LHD_LARGE. Value 0xFFFFFFFF for both low and high = unknown size
  - read_from: first_article
  - use: 64-bit unpacked size
- r4_file_name:
  - fmt: rar4
  - where: NAME_SIZE bytes, immediately after the fixed fields (after HIGH sizes if LHD_LARGE). Without LHD_UNICODE: OEM/ANSI name. With LHD_UNICODE (0x0200): ASCII-ish name, a NUL, then an encoded Unicode stream (unrar `EncodeFileName::Decode`: a flag byte carries four 2-bit selectors that choose between plain bytes, high-byte fill, and copy runs). Exact selector mapping: UNVERIFIED here (not fetched, see Sources)
  - read_from: first_article
  - use: file name and path. Separator is `\` in RAR4 (Windows-style), normalize to `/`
- r4_file_salt:
  - fmt: rar4
  - where: 8 bytes, present only if flags & 0x0400 (LHD_SALT), after the name. libarchive la_rar4.c L1757: `memcpy(rar->salt, p, 8)`
  - read_from: first_article
  - use: per-file encryption salt (paired with the file's password). RAR4 has no password-check value per file
- r4_file_exttime:
  - fmt: rar4
  - where: present if flags & 0x1000 (LHD_EXTTIME), after salt. Starts with uint16 flags, 4 nibbles (mtime, ctime, atime, arctime) each with a present bit (0x8) and a count (low 2 bits) of extra sub-second bytes. mtime is the header DOS time, then extra bytes give sub-second fraction (100 ns units per unrar `RarTime::SetExt`) (fraction arithmetic UNVERIFIED here)
  - read_from: first_article
  - use: higher precision ctime/atime/mtime. Useful for timestamp-based packer fingerprinting (packer clock skew, timezone)
- r4_file_version_flag:
  - fmt: rar4
  - where: flags & 0x0800 (LHD_VERSION); name contains `;N` suffix
  - read_from: first_article
  - use: version-suffixed file, rare
- r4_file_flags (bits from unrar headers.hpp):
  - fmt: rar4
  - where: @hdr+3, uint16
    - 0x0001 LHD_SPLIT_BEFORE: data continues from previous volume
    - 0x0002 LHD_SPLIT_AFTER: data continues in next volume
    - 0x0004 LHD_PASSWORD: file is encrypted (AES-128 in RAR 3.x+ with PBKDF2-like SHA-1 KDF of 0x40000 rounds; key derivation details UNVERIFIED here)
    - 0x0008 LHD_COMMENT: old file comment inline (RAR 2.x)
    - 0x0010 LHD_SOLID: file is part of a solid stream with the previous file (for file blocks), or a subblock with a parent (for NEWSUB)
    - 0x00E0 LHD_WINDOWMASK: dictionary: 0x0000 = 64 KB, 0x0020 = 128 KB, 0x0040 = 256 KB, 0x0060 = 512 KB, 0x0080 = 1 MB, 0x00A0 = 2 MB, 0x00C0 = 4 MB, 0x00E0 = directory (unrar `WinSize = 0x10000 << ((Flags & 0xE0) >> 5)`)
    - 0x0100 LHD_LARGE: 64-bit sizes (HIGH fields present)
    - 0x0200 LHD_UNICODE: encoded Unicode name present
    - 0x0400 LHD_SALT: salt present
    - 0x0800 LHD_VERSION: name carries `;N` version
    - 0x1000 LHD_EXTTIME: extended times present
    - 0x4000 SKIP_IF_UNKNOWN
    - 0x8000 LONG_BLOCK: ADD_SIZE (= PACK_SIZE low) present
  - read_from: first_article
  - use: split-file detection (SPLIT_BEFORE/AFTER), encryption, solid, dictionary size (packer tuning: `-md` setting leaks here), Unicode names

Note: the RAR4 file-header layout is `base (7) + PACK_SIZE (4) + UNP_SIZE (4) + HOST_OS (1) + FILE_CRC (4) + FTIME (4) + UNP_VER (1) + METHOD (1) + NAME_SIZE (2) + ATTR (4)` = 32 bytes, then [HIGH_PACK (4), HIGH_UNP (4) if LHD_LARGE], then name, [salt 8 if LHD_SALT], [EXTTIME if LHD_EXTTIME], then data.

RAR4 SOLID: unlike RAR5 (one flag per file via comp_info bit 7), RAR4 solid is marked on the file block by LHD_SOLID (0x0010). A solid run reuses the decompressor state, so you cannot read a middle file without its predecessors (matters for partial downloads: if the first article covers only file 1 of a solid stream, file 2's data needs the full stream).

---

## 10b. RAR4 SUBBLOCK headers (type 0x7A NEWSUB; mostly with file-block layout)

NEWSUB reuses the FILE layout. The name field identifies the service:

- r4_sub_name:
  - fmt: rar4
  - where: name in the file-block layout (ASCII): `CMT` (archive comment, SUBHEAD_TYPE_CMT), `QO` (quick open cache), `ACL` (NTFS ACL), `STM` (NTFS stream, e.g. `:Zone.Identifier`), `UOW` (Unix owner), `AV` (authenticity), `RR` (recovery record), `EA2` (OS/2 EA)
  - read_from: CMT first_article if near start; QO/RR last_article
  - use: CMT text, QO cache, ACL/STM/UOW service data
- r4_sub_data:
  - fmt: rar4
  - where: after the name (and optional salt), the subheader's extra bytes = `HEAD_SIZE - NAME_SIZE - 32` (unrar arcread.cpp L372), stored as SubData. Service payload = PACK_SIZE bytes after the header
  - read_from: CMT first_article; others last_article
  - use: service payload; CMT is usually compressed with the RAR 2.x/3.x unpacker (method/unp_ver fields on the subheader apply, UNVERIFIED for CMT specifics)
- r4_sub_stm_name:
  - fmt: rar4
  - where: STM subblock: stream name, plus UnpSize/StreamCRC fields in SubData (unrar `StreamHeader`, ProtectHeader-style layout)
  - read_from: last_article
  - use: NTFS stream name and content (Zone.Identifier / MOTW, see §5 note)
- r4_sub_uow:
  - fmt: rar4
  - where: UOW subheader: owner name (256 max), group name (256 max), numeric uid/gid (unrar `UnixOwnerName`, `UnixGroupName`, `UnixOwnerID`, `UnixGroupID`)
  - read_from: last_article (service subheaders follow the file they belong to, so sometimes first_article)
  - use: Unix owner, same leak as RAR5 `unix_owner`

---

## 11. RAR4 PROTECT block (type 0x78): recovery record

- r4_protect_version:
  - fmt: rar4
  - where: after base, 1 byte (`ProtectHeader.Version`)
  - read_from: last_article (usually sits near the end of the volume set)
  - use: recovery record format version
- r4_protect_rec_sectors:
  - fmt: rar4
  - where: uint16 LE, number of recovery sectors
  - read_from: last_article
  - use: recovery record size, input to the `-rr` percentage leak
- r4_protect_total_blocks:
  - fmt: rar4
  - where: uint32 LE, total blocks in the protected set
  - read_from: last_article
  - use: denominator for the recovery percentage. Exact percentage formula UNVERIFIED
- r4_protect_mark:
  - fmt: rar4
  - where: 8 bytes Mark (`ProtectHeader.Mark`, the RAR 3.x PROTECT marker)
  - read_from: last_article
  - use: identifies the protect block

---

## 12. RAR4 ENDARC header (type 0x7B)

- r4_endarc_flags:
  - fmt: rar4
  - where: @hdr+3, uint16. 0x0001 EARC_NEXT_VOLUME (not last volume), 0x0002 EARC_DATACRC (CRC32 of archive up to here, present in volumes), 0x0004 EARC_REVSPACE (7 zero bytes reserved), 0x0008 EARC_VOLNUMBER (volume number field present)
  - read_from: last_article
  - use: "is there a next part" test, same role as RAR5 ENDARC bit 0x0001
- r4_endarc_crc_and_volnum:
  - fmt: rar4
  - where: after base, in order per unrar `EndArcHeader` (ArcDataCRC uint32 if EARC_DATACRC, then VolNumber uint16 if EARC_VOLNUMBER; exact byte order UNVERIFIED)
  - read_from: last_article
  - use: a CRC of the volume up to the end block (integrity without decompressing) and the volume number. Volume number lets you detect a mismatched or renamed part

---

## 13. RAR4 comment (CMT) and recovery details

- r4_cmt_old_block:
  - fmt: rar4
  - where: type 0x75 (HEAD3_CMT), RAR 2.x style, UnpSize uint16, UnpVer byte, Method byte, CommCRC uint16
  - read_from: first_article
  - use: legacy comment only
- r4_cmt_new:
  - fmt: rar4
  - where: NEWSUB named `CMT` (see §10b), payload compressed per method
  - read_from: first_article if early in volume
  - use: archive comment text

---

## 14. Encryption summary (both formats)

- RAR4 header encryption (`-hp`): main flag 0x0080. Blocks after the main header are encrypted with AES-128. No password-check value in RAR4 headers (UNVERIFIED, only the salt per block is documented).
- RAR4 file encryption (`-p`): file flag 0x0004 plus 8-byte salt (0x0400). The file data is AES-128 encrypted, with the key from a SHA-1 based KDF.
- RAR5 header encryption: archive encryption header (type 0x04), with KDF count, 16-byte salt, optional 12-byte check value. Each header prefixed by a 16-byte IV.
- RAR5 file encryption: file extra type 0x01, with KDF count, salt, IV, optional check value. TWEAKED checksums (flag 0x0002) make the CRC key-dependent.

Partial-download consequence: encrypted-data entries (file flag 0x0004 in RAR4, extra type 0x01 in RAR5) can be detected from the first article, but file names and sizes are unreadable when headers are encrypted (`-hp`). Only the plain-header case gives names and sizes without a password.

---

## 15. Packer leaks: what fingerprints the archiver

Read from the first article unless noted.

- leak_format_version:
  - fmt: both
  - where: signature byte 6 (RAR5 = 0x01) vs RAR4 (0x00 at byte 6). Separates `rar` 5.x from 3.x/4.x
  - read_from: first_article
  - use: `-ma4` vs `-ma5` (RAR4 vs RAR5 format)
- leak_unp_ver:
  - fmt: rar4
  - where: FILE UNP_VER (§10). 29 = RAR 3.x+ compression
  - read_from: first_article
  - use: minimum RAR version to extract
- leak_comp_version:
  - fmt: rar5
  - where: comp_info bits 1-6. 0 = RAR 5.0+, 1 = RAR 7.0+ (new algorithm variant)
  - read_from: first_article
  - use: RAR 7.x packer detection
- leak_dict_size:
  - fmt: both
  - where: RAR5 comp_info bits 11-15 (dictionary exponent, 128 KB << N); RAR4 LHD_WINDOWMASK (0x00E0)
  - read_from: first_article
  - use: `-md` setting (e.g. -md32m or RAR4 default 4 MB). Also fingerprints the packer's default
- leak_store_mode:
  - fmt: both
  - where: RAR5 comp_info method bits 8-10 = 0; RAR4 METHOD = 0x30
  - read_from: first_article
  - use: `-m0`, which also means raw inner media is accessible in partial downloads
- leak_solid:
  - fmt: both
  - where: main header solid flag (RAR5 archive_flags 0x0004; RAR4 MHD_SOLID 0x0008); file-level solid (RAR5 comp_info 0x0040; RAR4 LHD_SOLID 0x0010)
  - read_from: first_article
  - use: `-s` solid archive. Solid streams need the whole stream decompressed to reach later files
- leak_hash_algo:
  - fmt: rar5
  - where: extra type 0x02 hash type. RAR5 always stores BLAKE2sp (type 0x00) when hashing is on. RAR4 has only CRC32, no BLAKE2
  - read_from: first_article
  - use: RAR5 writer
- leak_qo:
  - fmt: both
  - where: RAR5 SERVICE `QO` and RAR5 locator QLIST flag; RAR4 NEWSUB `QO`
  - read_from: last_article
  - use: `-qo` (quick open) switch
- leak_rr:
  - fmt: both
  - where: RAR5 main archive_flags 0x0008 + RR service; RAR4 main 0x0040 + PROTECT block
  - read_from: first_article for the flag; last_article for the block
  - use: `-rr` recovery record present, size from RR sectors
- leak_os:
  - fmt: both
  - where: RAR5 file host_os (0/1); RAR4 file HOST_OS (0-5)
  - read_from: first_article
  - use: Windows vs Unix uploader; Unix suggests a Linux/macOS build of rar or a Unix-side tool
- leak_owner:
  - fmt: both
  - where: RAR5 extra type 0x06; RAR4 UOW subheader
  - read_from: first_article (RAR5), last_article or first (RAR4 depends on position)
  - use: Unix username and uid
- leak_times:
  - fmt: both
  - where: RAR5 htime extra 0x03 with ns precision bit 0x0010; RAR4 EXTTIME nibbles; plus DOS time (RAR4) and mtime (RAR5 mtime uint32)
  - read_from: first_article
  - use: ctime/atime presence and ns precision identify the packer version and timezone assumptions
- leak_name_encoding:
  - fmt: both
  - where: RAR4 LHD_UNICODE; RAR5 always UTF-8, with U+E0xx / U+FFFE for non-portable Unix names
  - read_from: first_article
  - use: packer era and Unix name handling
- leak_archive_name:
  - fmt: rar5
  - where: RAR5 main extra type 0x02 flag 0x01 (original archive name)
  - read_from: first_article
  - use: the name the archive had when first created (sometimes the poster's naming). Rare
- leak_av_flag:
  - fmt: rar4
  - where: RAR4 MHD_AV (0x0020). Legacy authenticity verification, deprecated, almost never set by modern rar
  - read_from: first_article
  - use: obsolete packer marker, low value

---

## 16. Volume and split-file mechanics (byte maps)

### 16a. Detecting continuation

- split_detect_rar5:
  - fmt: rar5
  - where: file header hdr_flags 0x0008 (SPLIT_BEFORE, data continues from previous volume) and 0x0010 (SPLIT_AFTER, continues into next volume). Volume-level: main archive_flags 0x0001 (VOLUME), ENDARC flags 0x0001 (not last volume)
  - read_from: first_article (flags), last_article (ENDARC)
  - use: complete split-file chain without reading every volume
- split_detect_rar4:
  - fmt: rar4
  - where: file flags 0x0001 (SPLIT_BEFORE) and 0x0002 (SPLIT_AFTER); main 0x0001 (VOLUME); ENDARC 0x0001 (NEXT_VOLUME)
  - read_from: first_article (file flags), last_article (ENDARC)
  - use: same role as RAR5

### 16b. Byte map for the inner media (where does the MKV start in volume 1)

RAR5 volume 1 (no SFX):
```
0      : signature (8 bytes)
8      : [archive encryption header, if -hp]
...    : main header block (CRC4 + size vint + header)
...    : [CMT service block, if commented]
...    : FILE header block (CRC + size vint + header + data-size vint)  -> data area starts here
         ... packed data (r5_file_data_size bytes)
         [service blocks for this file: ACL, STM, ...]
...    : ... next file block
[RR block]
[QO block]
ENDARC header (last ~50 bytes)
```

Data start for a file = `file_header_offset + 4 + len(hdr_size vint) + hdr_size` (see §1). Data end = start + data_size.

RAR4 volume 1 (no SFX):
```
0      : signature (7 bytes)
7      : MAIN block (HEAD_SIZE, usually 13 bytes)
...    : FILE block(s), each: fixed fields + name + [salt] + [exttime]; data starts at block_start + HEAD_SIZE
[NEWSUB blocks: CMT, QO, STM, ...]
[PROTECT block: recovery record data after its header]
ENDARC block (7 bytes base + optional CRC/volnum)
```

Data start for a file = `file_block_offset + HEAD_SIZE` (§8). Data end = start + PACK_SIZE (+ HIGH<<32).

### 16c. What an indexer needs per volume

- Volume 1, first article: signature + main + first FILE header. Gives the first part's packed size, the total unpacked size, method, dict, split flags. Enough for a store-mode MKV: MKV magic is at data_start.
- Volume N, first article: that volume's FILE header with SPLIT_BEFORE (0x0008 / 0x0001). Its data_start is the continuation point. Summing packed sizes over volumes gives the total packed size for store mode.
- Volume N, last article: ENDARC (next-volume flag) and, for RAR5, the trailing QO/RR blocks if placed last. The ENDARC is always the last header, so this is cheap.
- Volume N, first and last articles only (common Usenet case): first article gives header and early data. Last article gives ENDARC. The middle volume data is not needed for headers, so the byte map works without downloading all articles (the middle articles contain only compressed or stored payload of that file).

### 16d. Data split at article boundaries

- Usenet articles are about 700 KB. The RAR data area crosses article boundaries freely; there is no per-article header in RAR. Mapping byte offsets to articles is a pure arithmetic exercise over the volume's decoded byte stream (assuming each article decodes to exactly its segment size).
- For a store-mode MKV: the MKV EBML header sits at data_start. Its Segment, Cues, and Tags elements can sit at the end of the file, so you need the last article to read Cues/Tags if the MKV is store-mode. The file chain across volumes is contiguous in decoded order.

---

## 17. Store mode (-m0) vs compressed: what that means for inner media headers

- store (RAR4 method 0x30; RAR5 comp_info method 0): inner file bytes are identical to the original, so the MKV/MP4/ISO container headers are directly readable from the data offset in volume 1. Full inner metadata (tags, chapters, codec params) is readable from the first article plus the tail (MKV Cues/Tags/Chapters often at the end, MP4 `moov` may be at the end unless faststart).
- compressed (RAR4 0x31-0x35; RAR5 method 1-5): the stream is LZ-plus-Huffman (RAR 3.x/4.x uses LZ with PPM variant; RAR5 uses a different LZ with Huffman). A partial download yields only the decoder's first output bytes after the decoder has been started from the beginning of the stream. Without a Rust decompressor you cannot read inner headers of a compressed archive. Pure-Rust RAR decompressor availability: UNVERIFIED (see crates).
- solid: even with method 0 per file, a solid run can carry a non-store method on earlier files; store mode is per file, solid is per stream.

---

## 18. Rust crates

- name: unrar (crates.io, crate `unrar`)
  - kind: FFI wrapper around the C++ unrar library
  - maintained: UNVERIFIED (crate file at static.crates.io/crates/unrar/unrar-0.5.8.crate returned HTTP 200; not extracted or read, so version and maintenance are UNVERIFIED)
  - exposes: list of entries, names, sizes, CRC, extract, password handling; header-level details (flags, times, owner) only partially
  - note: needs the C++ build; is the reference-quality decoder; not a partial-download parser (it expects the file on disk, and it does not expose volume byte maps)
- name: compress-tools / libarchive bindings (e.g. compress-tools, libarchive crate)
  - kind: FFI to libarchive (C)
  - maintained: UNVERIFIED
  - exposes: libarchive's RAR4 and RAR5 readers (via read-support calls), entry metadata (name, size, mtime, mode, symlink targets, owner) and data stream
  - note: libarchive's RAR readers are the best open reference parser (see Sources); they need a seekable or streaming input and do not expose byte maps
- name: rar-stream
  - kind: UNVERIFIED (probable pure Rust)
  - maintained: UNVERIFIED. index.crates.io lookup for `rar-stream` returned 404 with my index path, so either the name or my index path is wrong. Check with `cargo search rar-stream` before relying on it
  - exposes: UNVERIFIED
- name: pure-Rust RAR header parser (any)
  - kind: no verified candidate. The header formats above are simple enough to write in a few hundred lines: RAR5 is vint + CRC + flags, RAR4 is fixed little-endian structs
  - recommendation: write the header parser (§1-§13) in Rust. Use libarchive la_rar5.c / la_rar4.c as the reference and unrar headers.hpp as the constant reference. Treat decompression as the separate, hard piece

## Sources

- RAR5 technote (fetched via WebFetch from https://www.rarlab.com/technote.htm). Covers RAR5 only. It has no RAR4 section, and it defers encryption and compression algorithms and the recovery record layout to UnRAR source. Some tables carry small errors noted inline (redirection name length described as "vint").
- libarchive RAR5 reader: https://raw.githubusercontent.com/libarchive/libarchive/master/libarchive/archive_read_support_format_rar5.c (downloaded and grepped: enum HEADER_FLAGS L1245, locator L1261, file extra L1700-1780, file header L1840-1870, main header L2207-2240, block dispatch L2420-2445)
- libarchive RAR4 reader: https://raw.githubusercontent.com/libarchive/libarchive/master/libarchive/archive_read_support_format_rar.c (downloaded and grepped: flag constants L64-L89, file header and salt/exttime L1690-1800, signature L49, MHD_ENCRYPTVER L988)
- UnRAR headers (mirror): https://raw.githubusercontent.com/pmachapman/unrar/master/headers.hpp (flag constants, struct layouts for MainHeader, FileHeader, EndArcHeader, SubBlockHeader, CommentHeader, ProtectHeader, StreamHeader; read in full)
- UnRAR arcread.cpp (mirror): https://raw.githubusercontent.com/pmachapman/unrar/master/arcread.cpp (read: L233-L270 flag decode, L318-L400 file header fields, L1280-L1333 RAR4 size and next-block math)
- UnRAR rardefs.hpp, rawread.hpp, archive.hpp, extinfo.cpp (mirror) (read: constants, subhead names, NTFS/UOW usage)
- Not fetched (UNVERIFIED sources): unicode.cpp (RAR4 Unicode name decode), the exact EXTTIME fraction math (unrar `RarTime`), the RAR4 per-block salt layout with `-hp`, RAR4 CMT compression specifics, the recovery record percentage formula, the RAR5 encryption KDF details.
- rarlab.com was blocked from curl (proxy 403). WebFetch returned the technote successfully.
- Not checked: forensicswiki RAR4 notes, unrar `unicode.cpp`, `rar-stream` crate source.
