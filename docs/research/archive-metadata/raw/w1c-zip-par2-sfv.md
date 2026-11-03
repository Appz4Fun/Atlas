# W1c: ZIP / ZIP64, split volumes, PAR2 / PAR3, SFV, SRR/SRS, Rust crates

Scope: metadata fields for a Rust Usenet indexer working from partial downloads (first 1-6 articles of ~700 KB, sometimes the last). Rust-native parsing only. RAR and 7z are covered by another researcher.

Legend: `read_from` = `first_article` (local headers / start of file), `last_article` (end of file: EOCD, central directory, Zip64 EOCD, PAR2 packets scattered), `full`, `separate_small_file` (a sidecar such as .par2, .sfv, .srr). Items marked UNVERIFIED were not confirmed against a fetched primary source.

Verification status of this file: APPNOTE structures (sections 1-7 below) were read from the PKWARE APPNOTE via WebFetch. PAR2 packet layouts were read from the parchive.github.io v2.0 spec page. PAR3 was read from the alpha draft (2022-01-28). Creator strings for individual PAR2 clients, SRR block IDs and most ZIP extra-field internals beyond the header IDs were NOT confirmed (see UNVERIFIED tags). The pyReScene README and par2cmdline source files were not retrievable (client_error on raw.githubusercontent.com).

---

## 1. ZIP / ZIP64

### 1.1 Local file header (30-byte fixed part, then name, extra)

- `where:` offset 0, signature `0x04034B50` (LE bytes `50 4B 03 04`)
- `read_from: first_article`
- `use:` anchor for scanning when no EOCD exists (truncated/first-article archive); carries name and sizes unless bit 3 is set.

Fields (offset, size, meaning):
- 0 / 4: signature
- 4 / 2: version needed to extract (e.g. 20 = 2.0, 45 = 4.5 Zip64, 46 = BZIP2, 51 = AES, 63 = LZMA/PPMd era). `read_from: first_article`. `use:` minimum-feature check, detects zip64 need and AES (51+).
- 6 / 2: general purpose bit flag (see 1.8). `read_from: first_article`. `use:` encrypted (bit 0), data-descriptor follows (bit 3), UTF-8 names (bit 11), strong encryption (bit 6).
- 8 / 2: compression method (see 1.9). `read_from: first_article`. `use:` codec identification; 99 means AE-x (real method in 0x9901 extra).
- 10 / 2: last mod file time (MS-DOS: hour<<11 | min<<5 | sec/2). `read_from: first_article`. `use:` fallback mtime if no extended timestamp.
- 12 / 2: last mod file date (MS-DOS: (year-1980)<<9 | month<<5 | day). `read_from: first_article`.
- 14 / 4: CRC-32 of uncompressed data (0 when bit 3 set). `read_from: first_article` (if not bit 3) else last_article/data descriptor. `use:` integrity fingerprint for dedupe and for matching a sample/full-file.
- 18 / 4: compressed size (0xFFFFFFFF = see zip64 extra). `read_from: first_article`.
- 22 / 4: uncompressed size (0xFFFFFFFF = see zip64 extra). `read_from: first_article`. `use:` file size without downloading the entry.
- 26 / 2: file name length n. `read_from: first_article`.
- 28 / 2: extra field length m. `read_from: first_article`.
- 30 / n: file name (CP437 unless bit 11 set, or 0x7075 present). `read_from: first_article`.
- 30+n / m: extra fields (see 1.5). `read_from: first_article`.
- After extra: encryption header if encrypted (traditional: 12 bytes; AE-x: salt + password verifier at start of data, see 1.6). `read_from: first_article`.

Data-directory notes: bit 3 means local CRC and sizes are zero here and real values sit in the data descriptor after the data. A streaming writer (Usenet posting tool) often sets bit 3, so local headers alone may not reveal sizes. `read_from: first_article` for the header, `last_article` or next-entry boundary for the real CRC/size.

### 1.2 Data descriptor (follows the compressed data when bit 3 is set)

- `where:` immediately after compressed data. Two variants:
  - with signature `0x08074B50` (16 bytes non-zip64): CRC-32 @4, comp size @8 (4 B), uncomp size @12 (4 B)
  - without signature (12 bytes): CRC-32 @0, comp size @4, uncomp size @8. Readers must accept both.
  - Zip64 variants: 24 bytes with signature (sizes 8 B each, @8 and @16); 20 bytes without (sizes @4 and @12). The Zip64 form is used when the entry has a Zip64 extra. (Layout derived from the spec text; UNVERIFIED byte-for-byte.)
- `read_from: first_article` only if the entry is entirely within the first article; otherwise `last_article` (the descriptor is at the end of each entry, so it lands in the middle of the archive, not at EOF). For a partial download, the descriptor for entry k is found by scanning from entry k's data end, which requires the sizes from the central directory.
- `use:` real CRC-32 and sizes for bit-3 entries.

### 1.3 Central directory file header (46-byte fixed part, then name, extra, comment)

- `where:` located at EOCD `cd_offset`. Signature `0x02014B50`.
- `read_from: last_article` (CD normally sits at the end). Mandatory for listing entries in a split archive.

Fields:
- 0 / 4: signature
- 4 / 2: version made by. Upper byte = host OS, lower byte = spec version (see 1.10). `read_from: last_article`. `use:` packer fingerprint (host OS + spec version).
- 6 / 2: version needed to extract.
- 8 / 2: general purpose flag (may differ from local header, notably bit 13 CD encryption masking).
- 10 / 2: compression method.
- 12 / 2: last mod time (DOS).
- 14 / 2: last mod date (DOS).
- 16 / 4: CRC-32.
- 20 / 4: compressed size (zip64 may override).
- 24 / 4: uncompressed size (zip64 may override).
- 28 / 2: file name length n.
- 30 / 2: extra field length m.
- 32 / 2: file comment length k.
- 34 / 2: disk number start (0xFFFF = see zip64 extra). `read_from: last_article`. `use:` which volume holds this entry (spanned/split zips).
- 36 / 2: internal file attributes. bit 0 = text (ASCII), bit 2 = control field records. `read_from: last_article`.
- 38 / 4: external file attributes. Low byte DOS attribs (0x10 directory, 0x01 read-only, 0x20 archive). If host OS = 3 (UNIX) the high 16 bits are st_mode (e.g. 0x81ED << 16 = 0755 regular file, 0x41ED = dir, 0xA1FF = symlink). If host OS = 10 (NTFS) low byte is Windows attributes. `read_from: last_article`. `use:` unix permissions and symlink detection.
- 42 / 4: relative offset of local header (0xFFFFFFFF = see zip64 extra). `read_from: last_article`.
- 46 / n: file name. `read_from: last_article`.
- 46+n / m: extra fields. `read_from: last_article`.
- 46+n+m / k: file comment. `read_from: last_article`. `use:` packer comments occasionally used as notes/fingerprint.

Note: central-directory entries may be in an order different from local-header order.

### 1.4 End of central directory record (EOCD, 22-byte fixed part, then comment)

- `where:` the last 22 bytes of the file (plus up to 65535 bytes of comment). Scan backwards for signature `0x06054B50`.
- `read_from: last_article`. For a truncated file the EOCD is missing: fall back to scanning local headers in the first article.

Fields:
- 0 / 4: signature
- 4 / 2: number of this disk (0xFFFF = zip64)
- 6 / 2: disk with start of central directory
- 8 / 2: entries in CD on this disk
- 10 / 2: total entries in CD
- 12 / 4: size of central directory
- 16 / 4: offset of start of central directory (relative to the start of the archive, which for split archives is disk-relative)
- 20 / 2: ZIP comment length k
- 22 / k: ZIP comment. `use:` often holds packer name or a release note. `read_from: last_article`.

Sentinel values: 0xFFFF / 0xFFFFFFFF in any of these fields mean "look in the Zip64 EOCD".

### 1.5 Zip64 records

- Zip64 EOCD record: signature `0x06064B50`, 56-byte fixed part. `read_from: last_article`.
  - 0/4 signature; 4/8 size of record (total-12); 12/2 version made by; 14/2 version needed; 16/4 disk number; 20/4 disk with CD start; 24/8 entries on disk; 32/8 total entries; 40/8 CD size; 48/8 CD offset; 56/var extensible data.
- Zip64 EOCD locator: signature `0x07064B50`, 20 bytes, sits immediately before the EOCD. 0/4 sig; 4/4 disk with Zip64 EOCD; 8/8 relative offset of Zip64 EOCD; 16/4 total disks. `read_from: last_article`. `use:` the single, cheapest way to find the Zip64 EOCD in a tail buffer.
- Zip64 extended information extra (0x0001): 0/2 ID, 2/2 data size, then in this order, present only when the matching base field is 0xFFFFFFFF or 0xFFFF: uncompressed size (8), compressed size (8), relative header offset (8), disk start (4). `read_from`: in local header (first_article) for sizes; in CD (last_article) for all four. `use:` true sizes and offsets for files over 4 GiB.

### 1.6 Extra fields (ID, size, payload). Known IDs

Verified from the APPNOTE ID registry (fetched):
- 0x0001 Zip64 extended information
- 0x0007 AV info
- 0x0008 reserved for extended language encoding
- 0x0009 OS/2
- 0x000A NTFS
- 0x000C OpenVMS
- 0x000D UNIX
- 0x000E / 0x000F file stream descriptors / patch descriptor
- 0x0014 PKCS#7 store for X.509 certs
- 0x0015 X.509 cert ID and signature (individual file)
- 0x0016 X.509 cert ID (central directory)
- 0x0017 strong encryption header
- 0x0018 record management controls
- 0x0019 PKCS#7 encryption recipient cert list
- 0x0020 reserved timestamp record
- 0x0021 policy decryption key record
- 0x0022 Smartcrypt key provider record
- 0x0023 Smartcrypt policy key data record
- 0x0065 IBM S/390 (Z390) / AS/400 (I400) attributes, uncompressed
- 0x0066 IBM S/390 / AS/400 attributes, compressed
- 0x4690 POSZIP 4690 (reserved)
- 0x07C8 Macintosh
- 0x1986 Pixar USD
- 0x334D Info-ZIP Macintosh
- 0x4154 Tandem
- 0x4341 Acorn / SparkFS
- 0x4453 Windows NT security descriptor
- 0x4704 VM/CMS
- 0x470F MVS
- 0x4B46 FWKCS MD5
- 0x4C41 OS/2 text ACL
- 0x4D49 Info-ZIP OpenVMS
- 0x5455 extended timestamp
- 0x5855 Info-ZIP UNIX (original)
- 0x6375 Info-ZIP Unicode comment
- 0x7075 Info-ZIP Unicode path
- 0x7855 Info-ZIP UNIX (new)
- 0x7875 Info-ZIP UNIX (newer, UID/GID)
- 0x9901 AE-x encryption structure (WinZip AES)
- 0x9902 unknown
- 0xA11E data stream alignment
- 0xA220 Microsoft Open Packaging growth hint
- 0xCAFE Java JAR
- 0xD935 Android ZIP alignment
- 0xE57A Korean ZIP code page info
- 0xFD4A SMS/QDOS

Field-level layouts for the most useful ones (from Info-ZIP / WinZip AES definitions; the APPNOTE body for these sections was not retrievable in this run, so they are UNVERIFIED at byte level):

- 0x9901 WinZip AES (AE-x): data size 7. Bytes: vendor version (2) = `0x0001` (AE-1, CRC present) or `0x0002` (AE-2, CRC omitted); vendor ID (2) = ASCII `AE`; AES strength (1) = 1 (128-bit), 2 (192), 3 (256); actual compression method (2). The main header method is 99. `read_from: first_article` (local) and `last_article` (CD). `use:` AES key strength and the real codec. UNVERIFIED layout details.
- 0x5455 extended timestamp: data size 1 + 4*k. Flags byte: bit 0 = mtime present, bit 1 = atime, bit 2 = ctime. Each time is 4-byte Unix epoch seconds, in that order. Local header may carry all three; central directory commonly carries only mtime (flag byte + 4 bytes). `read_from: first_article` (local) and `last_article` (CD). `use:` the most reliable mtime; more precise than DOS time (2-second resolution). UNVERIFIED on flag details.
- 0x7875 Info-ZIP new UNIX: version (1) = 1; UID size (1); UID (variable, little-endian); GID size (1); GID (variable). `read_from` as above. `use:` packer's uid/gid, a fingerprint for the original host. UNVERIFIED.
- 0x5855 Info-ZIP old UNIX: atime (4), mtime (4), optional UID (2), GID (2). UNVERIFIED.
- 0x000D UNIX (PKWARE): atime, mtime, UID (2), GID (2), optional device/link data. UNVERIFIED.
- 0x000A NTFS: reserved (4), then tag 0x0001 (2), size 24 (2), mtime (8), atime (8), ctime (8), all FILETIME (100-ns intervals since 1601-01-01). `read_from: first_article`. `use:` high-precision Windows times; a strong fingerprint for Windows-packed archives. UNVERIFIED layout.
- 0x7075 Info-ZIP Unicode path: version (1) = 1, CRC-32 of the standard name bytes (4), UTF-8 name (rest). `read_from` as above. `use:` true UTF-8 names; the CRC lets a reader confirm the Unicode name matches the CP437 name. UNVERIFIED.
- 0x6375 Info-ZIP Unicode comment: version (1), CRC-32 of the standard comment (4), UTF-8 comment (rest). `read_from: last_article`. UNVERIFIED.
- 0x0017 strong encryption header: format, algorithm ID, bit length, flags, hash (APPNOTE section 7 structure). `read_from: first_article`. `use:` detects strong encryption (flag bit 6). UNVERIFIED layout.
- 0x0008 archive extra data record (record signature `0x08064B50`, then 4-byte length, then data; used with central directory encryption). `read_from: last_article`.

### 1.7 General purpose bit flags (2 bytes)

Verified (APPNOTE section 4.4.4, fetched):
- bit 0: encrypted
- bits 1-2: method-dependent. Deflate: 00 normal, 01 maximum, 10 fast, 11 superfast. Implode: dictionary size and tree count. LZMA: bit 1 = end-of-stream marker present.
- bit 3: data descriptor follows (CRC and sizes zeroed locally)
- bit 4: enhanced deflate (method 8 reserved)
- bit 5: compressed patched data
- bit 6: strong encryption (requires version needed >= 50 and bit 0 set; AES needs >= 51)
- bits 7-10: unused
- bit 11: UTF-8 names and comments (EFS)
- bit 12: reserved (enhanced compression)
- bit 13: central directory encryption masking (local header values hidden)
- bit 14: reserved (alternate streams)
- bit 15: reserved

Scene relevance: bit 3 is set by streaming writers and by many Usenet posting tools that write to a pipe; bit 0 with method 99 indicates AE-x.

### 1.8 Compression method IDs

Verified (APPNOTE section 4.4.5, fetched):
- 0 stored
- 1 shrunk (legacy)
- 2-5 reduced (legacy, factors 1-4)
- 6 imploded (legacy)
- 7 reserved for tokenizing
- 8 deflated
- 9 Deflate64
- 10 PKWARE DCL implode (IBM TERSE old)
- 12 BZIP2
- 14 LZMA
- 16 IBM z/OS CMPSC
- 18 IBM TERSE (new)
- 19 IBM LZ77 z architecture
- 20 deprecated (use 93)
- 93 Zstandard
- 94 MP3
- 95 XZ
- 96 JPEG variant
- 97 WavPack
- 98 PPMd version I rev 1
- 99 AE-x encryption marker (real method in 0x9901)

Note for PPMd (98): the two bytes before compressed data hold the PPMd properties (verified from the pyReScene/zip-method excerpt, a second-hand source for the rc-zip/PKWARE spec: order-1 + (suballoc-1)<<4 + restoration<<12). Mostly irrelevant for Usenet.

### 1.9 Version made by (2 bytes)

- Upper byte = host OS (verified, fetched):
  0 MS-DOS / OS/2 FAT, 1 Amiga, 2 OpenVMS, 3 UNIX, 4 VM/CMS, 5 Atari ST, 6 OS/2 HPFS, 7 Macintosh, 8 Z-System, 9 CP/M, 10 Windows NTFS, 11 MVS, 12 VSE, 13 Acorn RISC, 14 VFAT, 15 alternate MVS, 16 BeOS, 17 Tandem, 18 OS/400, 19 OS X (Darwin). 20-255 unused.
- Lower byte = spec version (value/10 major, value%10 minor; 63 = 6.3). Verified.
- Packer fingerprint: host 10 (NTFS) plus spec 20 is typical of Windows tools (WinRAR-created zips, 7-Zip). Host 3 (UNIX) plus spec 30 or 63 is typical of Info-ZIP / Python zipfile on Linux (Python writes 20 on Linux, 20 is also common). Host 0 plus spec 20 is typical of older Windows zip tools (PKZIP-lineage). The exact per-tool mapping is UNVERIFIED and must be built empirically from sample archives.

### 1.10 Version needed to extract (verified, fetched)

1.0 default, 1.1 volume label, 2.0 folder / deflate / traditional encryption, 2.1 Deflate64, 2.5 DCL implode, 2.7 patch data, 4.5 Zip64, 4.6 BZIP2 (7-Zip wrote 50 erroneously), 5.0 DES/RC2/RC4, 5.1 AES, 5.2 corrected RC2-64, 6.1 non-OAEP key wrapping, 6.2 central directory encryption, 6.3 LZMA / PPMd / Blowfish / Twofish. Version values for zstd, xz, MP3, JPEG, WavPack, AE-x are not listed in the fetched text; use the feature-specific value from the packer's own output (UNVERIFIED).

### 1.11 Encryption signals

- Traditional PKWARE (ZipCrypto): bit 0 set, method unchanged, 12-byte encryption header at start of data. Can be attacked with known plaintext, not a metadata source.
- AE-x: method 99, 0x9901 extra with strength; salt (8/12/16 bytes by strength) + 2-byte password verifier at start of data, then data, then 10-byte HMAC-SHA1 authentication code at end. `read_from: first_article` (salt, verifier present) and `last_article` (auth code). UNVERIFIED sizes.
- Strong encryption: bit 6 plus 0x0017 header. Rare.
- Usenet relevance: encrypted archives are common in scene/private posts. Encryption is detectable from the first article alone (bit 0, method 99, 0x9901). Contents cannot be listed.

### 1.12 Split / spanned ZIP

- Spanned zips: EOCD disk number 0 and CD disk-start fields tell which disk holds the CD. `read_from: last_article`.
- The `.zip.001` / `.z01` / `.z02` ... / `.zip` naming: `.z01`, `.z02`, final `.zip` (PKWARE split). Spec-level naming is not in APPNOTE (it is a PKWARE tool convention; UNVERIFIED). In a split archive, the local header offsets in the central directory are disk-relative. The Zip64 EOCD locator and EOCD must be on the same (last) disk (APPNOTE 4.4.1.5, fetched).
- Usenet practice: plain byte splits (see section 2), so a bare .zip split with 7-Zip `-v` is the common case. Each part is a raw byte slice; reconstruct by concatenation before parsing.
- Detection of volume count: the EOCD disk fields (total disks in the Zip64 locator, "number of this disk" in EOCD) give the expected number of disks; for PKWARE spanned zips the last disk's EOCD gives total. Read from last_article.

### 1.13 Comments and other tail fields

- EOCD comment (`read_from: last_article`): common for packer name ("Created by ...") and release notes.
- Per-entry comment (CD, `read_from: last_article`).

### 1.14 What is readable from the first article only (ZIP)

- Local headers for every entry fully inside the first article: name (or 0x7075 unicode name), method, flags (encryption, bit 3, UTF-8), DOS mtime, version needed, local CRC/sizes if bit 3 clear, 0x5455 and 0x000A local timestamps, AE-x strength, local Zip64 sizes.
- Not readable without the tail: central-directory-only fields (version made by, external attributes / unix mode, disk start, relative offsets, CD comments, EOCD comment), entry count, and the final list of entries if CD is at the end.
- Data-descriptor CRC for bit-3 entries (unless the descriptor is within the first article).

---

## 2. Split volumes

### 2.1 Naming schemes (conventions, not normative specs; UNVERIFIED for exact behavior unless noted)

- HJSplit: `file.001`, `file.002` ... raw byte slices, no header, no magic. Sizes are fixed (e.g. 700 KB) except the last.
- 7-Zip volume splitting (`7z a -v700k`): produces `name.7z.001`, `name.7z.002`, ... Raw slices of a single 7z (or .zip if the archive format is zip).
- `split` (coreutils) / generic `file.part01.rar` style: `.partNN.rar` for RAR (not in scope), `.rNN` old-style RAR volumes (`.r00`, `.r01`), `.zip.001` for zip volumes produced by 7-Zip `-tzip -v`. PKWARE spanned: `.z01`..`.zNN` with final `.zip`.
- Note: 7-Zip `.zip.001` is a raw slice of a zip, so the inner format is ZIP (first 4 bytes `50 4B 03 04`), not the `.001` suffix.
- Usenet posts often name split parts `.part01.rar` (RAR), `.001` (raw), or use `[filename].vol01+02.par2` PAR2 volumes.

### 2.2 Detecting inner format from the first bytes of `.001`

Magic numbers (well-known, from APPNOTE for ZIP and public format docs for others; ZIP verified):
- ZIP local header: `50 4B 03 04` (also `50 4B 05 06` for an empty zip EOCD-only archive, and `50 4B 07 08` for a spanned-archive marker)
- 7z: `37 7A BC AF 27 1C` (out of scope; for cross-reference)
- RAR 4/5: `52 61 72 21 1A 07 00` / `52 61 72 21 1A 07 01 00` (out of scope)
- ISO 9660: "CD001" at offset 0x8001, 0x8801, 0x9001 (volume descriptor). Offset 32769 decimal. UNVERIFIED offset in this run.
- MKV / WebM: EBML magic `1A 45 DF A3` at offset 0. Doc type string "matroska" or "webm" at offset ~ 0x20.
- MP4 / MOV: `ftyp` box at offset 4 (size at 0).
- GZIP: `1F 8B`. BZIP2: `42 5A 68`. XZ: `FD 37 7A 58 5A 00`. Zstd: `28 B5 2F FD`.
- PAR2: `50 41 52 32 00 50 4B 54` ("PAR2\0PKT") at offset 0 of every packet (verified).

Procedure: read the first 32 bytes of `.001`; check ZIP local header magic at 0; if absent, check 7z, RAR, EBML and ftyp; if all fail, treat as unknown and do not guess.

### 2.3 Volume order and total count

- Raw `.NNN` splits: order is the numeric suffix. Total count is the number of parts; the last part is shorter than the nominal size (size < nominal indicates the final part). With partial downloads, volumes missing from the middle are unrecoverable except by PAR2.
- Zip spanned (`.z01`...`.zip`): the final `.zip` holds the EOCD; the Zip64 locator "total disks" gives the total number. `read_from: last_article`.
- RAR volumes: out of scope.

---

## 3. PAR2 (Parity Volume Set Specification v2.0)

### 3.1 Packet header (64 bytes, verified fetched)

| offset | size | field | notes |
|---|---|---|---|
| 0 | 8 | magic `PAR2\0PKT` (ASCII 50 41 52 32 00 50 4B 54) | locate packets by scanning |
| 8 | 8 | packet length (uint64 LE), multiple of 4, includes header | |
| 16 | 16 | MD5 of packet | MD5 over bytes 32..end of packet (recovery set ID through body) |
| 32 | 16 | Recovery Set ID | MD5 of the Main packet body |
| 48 | 16 | type (ASCII, zero-padded to 16) | see 3.2 |
| 64 | len-64 | body | multiple of 4 bytes |

`read_from:` any packet can be found in any article. Packets are small except Recovery Slice and IFSC. Packets are typically stored in the `.par2` index file and in `vol` files. For a partial download, packets are only valid if their MD5 verifies. Use the packet MD5 to validate every packet independently.

### 3.2 Packet types (16-byte type field, verified fetched)

| name | type string |
|---|---|
| Main | `PAR 2.0\0Main\0\0\0\0` |
| File Description | `PAR 2.0\0FileDesc` |
| Input File Slice Checksum (IFSC) | `PAR 2.0\0IFSC\0\0\0\0` |
| Recovery Slice | `PAR 2.0\0RecvSlic` |
| Creator | `PAR 2.0\0Creator\0` |
| Unicode Filename (optional) | `PAR 2.0\0UniFileN` |
| ASCII Comment (optional) | `PAR 2.0\0CommASCI` |
| Unicode Comment (optional) | `PAR 2.0\0CommUni` (15 chars; pad with NUL to 16) |
| Input File Slice (optional) | `PAR 2.0\0FileSlic` |
| Recovery File Slice Checksum (optional, "may never be used") | `PAR 2.0\0RFSC\0\0\0\0` |
| Packed Main (optional) | `PAR 2.0\0PkdMain\0` |
| Packed Recovery Slice (optional) | `PAR 2.0\0PkdRecvS` |

### 3.3 Packet bodies (verified fetched)

Main (body):
- 0 / 8: slice size (multiple of 4). `use:` block size; needed for every other packet.
- 8 / 4: number of recovery-set files N.
- 12 / 16*N: recovery-set File IDs, sorted ascending (unsigned 128-bit).
- 12+16N / 16*M: non-recovery File IDs, sorted ascending. M = (packet_len - 64 - 12 - 16N) / 16. M is not stored.

File Description (body):
- 0 / 16: File ID. Computed as MD5(MD5-16k || file length (8 bytes LE) || ASCII file name as stored in body). `use:` joins files across packets.
- 16 / 16: MD5 of entire file. `use:` whole-file verification.
- 32 / 16: MD5-16k (MD5 of first 16 KiB). `use:` cheap identity check against the first article of the file. This is the single most useful PAR2 field for partial downloads: the first 16 KiB of a file is in article 1.
- 48 / 8: file length (bytes). `use:` file size without the file.
- 56 / var (multiple of 4): file name, ASCII, not NUL-terminated, zero-padded. Subdirectories use `/`. Case-sensitive, unique.

Input File Slice Checksum (IFSC) (body):
- 0 / 16: File ID.
- 16 / 20*K: K entries in slice order, each MD5 (16 bytes) + CRC-32 (4 bytes). Last slice zero-filled to full slice length before hashing. `use:` per-slice identity; lets an indexer match an incomplete file slice-by-slice.

Recovery Slice (body):
- 0 / 4: exponent. 4 / slice-size: recovery data. `use:` the recovery block count; the exponent gives the recovery block index. Large; don't need to parse the data for indexing.

Creator (body):
- 0 / var (multiple of 4): ASCII text identifying the client and a contact URL or email. Not NUL-terminated. `use:` packer fingerprint. Exact strings per client are UNVERIFIED (see 3.6).

Unicode Filename (body): 0 / 16 File ID; 16 / var: Unicode name (not NUL-terminated). Overrides ASCII name. `use:` non-ASCII names.

ASCII Comment (body): 0 / var: comment text.

Unicode Comment (body): 0 / 16 MD5 of matching ASCII comment packet (zeros if none); 16 / var: Unicode comment text.

Input File Slice (body): 0 / 16 File ID; 16 / 8 slice index; 24 / up to slice size: slice data. `use:` raw data for a slice. Large.

Recovery File Slice Checksum (body): 0 / 16 File ID; 16 / 24*K entries of MD5 (16), CRC-32 (4), exponent (4). Spec says never used.

Packed Main (body): 0 / 8 subslice size; 8 / 8 slice size; 16 / 4 N; 20 / 16*N recovery-set IDs (ascending); 20+16N / 16*M non-recovery IDs. `use:` rarely produced; treat as Main equivalent.

Packed Recovery Slice (body): 0 / 4 exponent; 4 / slice size: data (spec text says "recovery slice packet" under PkdRecvS; likely copy error).

### 3.4 Computation rules (verified fetched)

- Recovery Set ID = MD5 of the Main packet body.
- File ID = MD5(MD5-16k || length LE 8 || name bytes as stored).
- Slice checksums: MD5 + CRC-32 (IEEE / PKZIP polynomial, `crc32fast` compatible).
- Recovery data: GF(2^16), generator 0x1100B. Input constants are powers of 2 with exponents not divisible by 3, 5, 17, 257: 2, 4, 16, 128, 256, 2048, 8192, 16384, 4107, 32856, 17132, ...

### 3.5 PAR2 creator packet and version strings

- Verified: every PAR2 set must contain one Creator packet; clients that can't process a set show the Creator text to the user.
- par2cmdline string format: UNVERIFIED. The fetch of par2cmdline source failed (client_error). Expected pattern is `Created by par2cmdline version X.Y.Z` (UNVERIFIED).
- MultiPar / par2j: UNVERIFIED string. MultiPar (Yutaka Sawada) uses par2j as its backend (verified via Wikipedia).
- QuickPar: UNVERIFIED string.
- ParPar (animetosho): the README does not mention a creator string (verified absence). The packet is written by ParPar; exact text UNVERIFIED.
- phpar2 (Paul Houle): UNVERIFIED string.
- par2cmdline-turbo / par2cmdline-mt / par2cmdline-tbb: forks of par2cmdline (verified by README), creator text UNVERIFIED.
- Recommendation: build a creator-string corpus from real `.par2` samples; do not hard-code any string.

### 3.6 PAR2 index vs volume files (naming)

- Index: `filename.par2` (verified via Wikipedia, `filename.PAR2`). Contains no recovery data, only Main, FileDesc, IFSC, Creator and optional packets.
- Volumes: `filename.volNNN+MM.par2` (three-digit, verified from Wikipedia example `filename.vol000+01.PAR2`), where MM is the block count, NNN is the first recovery block index.
- `read_from: separate_small_file` for index (typically a few KB to tens of KB). Volumes are large (up to ~ block size * count).

### 3.7 PAR3 (alpha draft 2022-01-28, verified fetched; stated as DRAFT)

- Packet magic: `PAR3\0PKT` (8 bytes, the only file-level magic; no separate file signature).
- Packet header (48 bytes, from the draft): magic (8), fingerprint = lower 16 bytes of BLAKE3 over bytes 24..end of packet (16), packet length (8, includes header, unsigned, not required to be 4-aligned), InputSetID (8), type (8).
- Differences vs PAR2: MD5 replaced by BLAKE3 (lower 16 B for fingerprint, first 8 B for InputSetID). Type IDs are 8 bytes (`PAR CRE\0`, `PAR COM\0`, `PAR STA\0`, `PAR DAT\0`, `PAR EXT\0`, `PAR CAU\0`, `PAR SPA\0`, `PAR EXP\0`, `PAR REC\0`, `PAR ERD\0`, `PAR FIL\0`, `PAR DIR\0`, `PAR ROO\0`, `PAR LNK\0`, `PAR UNX\0`, `PAR FAT\0`). Application packet types need a 4-byte prefix. Main and Index packets do not exist in this draft.
- Block-based (not slice-based), block size multiple of GF size, rolling hash CRC-64-ISO used to find moved blocks.
- File packet: filename length (2), UTF-8 name, rolling hash of first 16 KiB (8), option count (1), chunk descriptions (length 8, fingerprint 16, optional first-block index, tail fields).
- Draft ambiguities flagged by the spec: directory name length (table says 2 B, prose says 1 B); Cauchy formula sign; hash TODOs.
- Usenet relevance: almost no PAR3 sets in the wild; assume PAR2 first. PAR3 detection: scan for `PAR3\0PKT`. UNVERIFIED that any production client writes PAR3.

---

## 4. SFV / MD5 / SHA1 / comment headers / NFO / SRR / SRS

### 4.1 SFV (Simple File Verification)

- `where:` plain text, one entry per line: `FILENAME<whitespace>HEX8` (CRC-32, 8 hex digits). Verified via Wikipedia: "one or several spaces", tabs never used. Lines beginning with `;` are comments.
- `read_from: separate_small_file`. `use:` file names, sizes unknown, CRC-32 for each file (matches ZIP CRC-32 and IFSC CRC-32 in PAR2).
- Comment headers: the Wikipedia article gives only `; This is a comment`. Generator-style headers (`; Generated by QuickSFV ... on ...`, `; Generated by cksfv ...`, `; Generated by RapidCRC ...`, `; Generated by pyReScene ...`) are UNVERIFIED. The `; Generated by ... on <date>` pattern matches common community practice; verify with real samples.
- Other formats: `.md5` (`md5sum` style: `HEX32  filename`, two spaces, optional `*` for binary mode) and `.sha1` (40 hex). UNVERIFIED in this run; RekSFV handles SFV/MD5/SHA1 per Wikipedia.

### 4.2 NFO

- `where:` plain text, often CP437 with ANSI art and a release header. UNVERIFIED format spec; no primary spec consulted.
- `read_from: separate_small_file`. `use:` release metadata (group, date, tool). Parse as text; treat as untrusted.

### 4.3 SRR (ReScene)

- The rescene.wikidot.com homepage (fetched) describes SRR as a small (typically 10-20 KB) file backing up release metadata (copy of SFV, file names, dates, attributes, comments, recovery records) to rebuild original RAR files. Store-mode RAR reconstruction supported; compressed reconstruction since 0.4 via WinRAR executables.
- Block-level layout (CRC16 header, block types such as `0x69` SRR header, `0x6A` stored file, `0x71` RAR file, `0x72` archived file, app-name block): NOT confirmed. The fetched page has no block specification. All block IDs are UNVERIFIED.
- The pyReScene README fetch failed (client_error). The pyReScene source and the rescene.wikidot.com format pages (tutorials, FAQ, downloads) were not read.
- `read_from: separate_small_file`. `use:` if an SRR exists, the set's names, sizes and SFV CRC-32 can be recovered without the RAR bodies.

### 4.4 SRS (ReScene sample)

- Fetched text: SRS holds sample metadata plus per-track signatures so a sample can be rebuilt from the full MKV/AVI and checked with a CRC. For music, SRS backs up tags (ID3v1, ID3v2, Lyrics3, Lyrics3v2, APE), an AcoustID fingerprint, file name and CRC-32.
- Byte layout: UNVERIFIED.
- `read_from: separate_small_file`.

---

## 5. Rust crate landscape (crates.io API, fetched 2026-11-02)

| crate | latest | license | repo | updated | downloads | notes |
|---|---|---|---|---|---|---|
| `zip` (zip-rs/zip2) | 9.2.2 | MIT | github.com/zip-rs/zip2 | 2026-11-02 | 326.5M total | Pure Rust. Read/write. Deflate, bzip2, zstd, xz, LZMA read-only, PPMd, Deflate64 read-only, Implode/Shrink/Reduce read-only, ZipCrypto, AES (feature flags). Central-directory based; needs seekable reader for normal open; `read_zipfile_from_stream_*` functions exist for streams. No partial/sans-io API documented. Strong candidate for full-file parsing, not partial buffers. |
| `rc-zip` (bearcove/rc-zip) | 5.4.1 | Apache-2.0 OR MIT | github.com/bearcove/rc-zip | 2025-11-19 | 2.35M | Pure Rust, sans-I/O state machine. Requests reads at offsets, never blocks, works with sync/async I/O. Only central directory is authoritative (local headers ignored unless asked). Zip64 supported. Reads trailing-zip (SFX). Normalises DOS/NTFS/extended/unix times to chrono UTC. CP-437 / Shift-JIS detection via encoding_rs. Parsing via winnow. Reading only. Compression support not listed in the README (UNVERIFIED). AES not mentioned. Best fit for "I have only some bytes, tell me what range you need next". |
| `async_zip` (Majored/rs-async-zip) | 0.0.19 | MIT | github.com/Majored/rs-async-zip | 2026-08-22 | 7.69M | Pure Rust, async (futures and tokio). Read and write, streaming writer with data descriptors, ZIP64 partial, deflate/bzip2/lzma/zstd/xz features, encryption not mentioned in README (UNVERIFIED). Seekable reads are the main path. |
| `crc32fast` | 1.5.3 | MIT OR Apache-2.0 | github.com/srijs/rust-crc32fast | 2026-10-20 | 811.8M | Pure Rust with SIMD (PCLMULQDQ etc). IEEE CRC-32 = PKZIP CRC-32 = PAR2 slice CRC-32. Use for ZIP and SFV and PAR2 IFSC. |
| `md-5` (RustCrypto/hashes) | 0.11.0 | MIT OR Apache-2.0 | github.com/RustCrypto/hashes | 2026-03-27 | 397.8M (all versions) | Pure Rust MD5. Use for PAR2 packets, file MD5, MD5-16k, IFSC MD5, SFV-MD5 variants. |
| `par2` | UNVERIFIED | UNVERIFIED | UNVERIFIED | UNVERIFIED | UNVERIFIED | crates.io fetch failed (client_error). Not verified in this run. Check crates.io page, Parchive org repos, and whether any pure-Rust PAR2 parser exists before relying on one. A parser for packet headers and bodies (sections 3.1-3.3) is ~300 lines and is safer to write than to depend on an unknown crate. |
| `blake3` | UNVERIFIED | UNVERIFIED | UNVERIFIED | UNVERIFIED | UNVERIFIED | Needed only for PAR3 fingerprints. Not fetched. |

Assessment for the indexer:
- ZIP parsing: prefer rc-zip for partial buffers (it accepts ranged reads and the sans-io model maps to "give me the next byte range"). Use `zip` only if full-file access is available. async_zip is the choice if the pipeline is async-first and seekable.
- PAR2 parsing: write a small packet parser on top of md-5 and crc32fast. Verify packet MD5 on each packet, check Main packet FileIDs, use FileDesc MD5-16k to match first-article files.
- SFV/SRR/SRS/NFO: hand-written text parsers and a small block reader. No crate needed.

---

## Sources

- APPNOTE.TXT (PKWARE, current): https://pkware.cachefly.net/webdocs/casestudies/APPNOTE.TXT (fetched; sections 4.3-4.5 incl. local header, CD, EOCD, Zip64, data descriptor, extra ID registry, GP flags, method IDs, version made by, version needed. Extra-field byte-level layouts were not retrievable in this run.)
- Parity Volume Set Specification v2.0: https://parchive.github.io/doc/Parity%20Volume%20Set%20Specification%20v2.0.html (fetched; all packet layouts above)
- Parity Volume Set Specification v3.0 (alpha draft 2022-01-28): https://parchive.github.io/doc/Parity_Volume_Set_Specification_v3.0.html (fetched; partial, see DRAFT flags)
- Wikipedia, Parchive: https://en.wikipedia.org/wiki/Parchive (fetched; naming and client list)
- Wikipedia, Simple file verification: https://en.wikipedia.org/wiki/Simple_file_verification (fetched; SFV line format and comments)
- ReScene: https://rescene.wikidot.com/ (fetched; high-level SRR/SRS only)
- par2cmdline README: https://raw.githubusercontent.com/parchive/par2cmdline/master/README.md (fetched; feature list, fork list)
- ParPar README: https://raw.githubusercontent.com/animetosho/ParPar/master/README.md (fetched)
- zip-rs/zip2 README: https://raw.githubusercontent.com/zip-rs/zip2/master/README.md (fetched)
- rc-zip README: https://raw.githubusercontent.com/bearcove/rc-zip/main/README.md (fetched)
- rs-async-zip README: https://raw.githubusercontent.com/Majored/rs-async-zip/main/README.md (fetched)
- crates.io API: https://crates.io/api/v1/crates/{rc-zip,zip,async_zip,crc32fast,md-5} (fetched; par2 failed)
- Failed fetches (not used): par2cmdline src/par2creatorpacket.{h,cpp} (raw.githubusercontent client_error); pyReScene README (client_error); crates.io par2 (client_error).
