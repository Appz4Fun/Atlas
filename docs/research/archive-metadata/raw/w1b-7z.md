# W1b: 7z metadata inventory for a partial-download Usenet indexer (Rust)

Scope: every field readable from a 7z archive (single or .7z.001/.002 split), with byte layout, meaning, and which Usenet article the field needs. Goal: parse in pure Rust, no 7z/ffmpeg at runtime, working from the first 1-6 articles (~700 KB each) and sometimes the last article.

Verification legend: `[V]` verified in fetched source/spec text this session. `[V-partial]` verified with a caveat noted. `UNVERIFIED` = from memory or inference, not confirmed in a fetched source.

## 0. Key takeaways for the indexer

1. The 32-byte signature header is at offset 0 of volume 001, so it is always in the first article. Its StartHeaderCRC (over bytes 12-31) makes it self-validating from that one article. `[V]`
2. The real header (file list, sizes, CRCs, times, attributes) sits at the END of the logical stream: absolute offset = 32 + NextHeaderOffset. For split sets that means the last volume's last article(s). This is the main partial-download problem. `[V]`
3. The tail header is usually a kEncodedHeader: an LZMA (optionally AES) packed stream placed immediately before the plain header. The packed header is typically a few KB to a few MB, so it is inside the last article or last two articles. `[V-partial]`
4. With -mhe=on (AES header encryption) the file list is unreadable without the password, but the encoded header's StreamsInfo is plaintext. That reveals the AES coder props (NumCyclesPower, salt size, IV), the header's packed size and unpacked size, and the header's folder CRC. `[V]`
5. Store mode (-mx0, COPY coder, method 0x00): packed == unpacked, so file N's bytes sit at a computable absolute offset. Inner containers (MKV EBML header, MP4 ftyp/moov) can be read from the first article. MP4 moov at the end still needs the last article. `[V]` for COPY semantics; container layout is outside this doc.
6. Solid folders: one LZMA stream for many files. LZMA is a streaming decoder, so the first file's prefix can be decoded from the folder's first article(s). Later files need everything before them. `[V-partial]` (LzmaDec streaming behavior is inferred from the SDK API; the exact `LzmaDec` call pattern is UNVERIFIED in this session).
7. A 7z-specific trap: NextHeaderOffset is relative to byte 32, not to file start. Forgetting the +32 is the most common bug. `[V]`
8. Rust: writing a dedicated header parser (~500 lines) is cheaper than adapting a crate. Only the LZMA decoder (and optionally AES + SHA-256) is needed for the encoded header.

## 1. Byte encoding primitives

```yaml
UINT32:       where: LE, 4 bytes                                   [V]
UINT64/REAL_UINT64: where: LE, 8 bytes fixed                       [V]
NUMBER (7z "UINT64" varint):
  where: first byte b; n = count of leading 1-bits of b (0..8)
         n = 0: value = b & 0x7F                                   (0x00-0x7F)
         n = 1..6: value = (b & (0xFF >> (n+1))) << (8*n) + LE(next n bytes)
           0x80-0xBF n=1, 0xC0-0xDF n=2, 0xE0-0xEF n=3, 0xF0-0xF7 n=4,
           0xF8-0xFB n=5, 0xFC-0xFD n=6
         n = 7 (0xFE): value = LE(next 7 bytes), high bits of b unused
         n = 8 (0xFF): value = LE(next 8 bytes), no mask
  read_from: n/a (encoding)                                          [V] (py7zr read_uint64 table, 7zFormat.txt)
  use: every count, size, index, and offset in the header
BYTE, BYTEARRAY: fixed-size single byte / sequence, length from context [V]
BitField:  MSB-first, 8 items per byte, trailing bits must be 0        [V]
BooleanList (AllAreDefined): 1 byte; if nonzero -> all items true, no bitfield follows;
           if 0 -> BitField follows                                    [V]
Digests(N): AllAreDefined byte; optional BitField; then UINT32 LE CRC per defined item  [V]
```

## 2. Signature header (bytes 0-31 of volume 001)

```yaml
signature:
  where: bytes 0-5: 37 7A BC AF 27 1C ("7z\xBC\xAF\x27\x1C")
  read_from: first_article
  use: format detection. Also the byte pattern to search for in SFX/prefixed posts.  [V]

ArchiveVersion.Major:
  where: byte 6 (currently 0x00)
  read_from: first_article
  use: reader rejects if != 0 (7zIn checks major only; minor is stored, not checked)  [V]

ArchiveVersion.Minor:
  where: byte 7 (0x04 in 7-Zip 9.x+ per spec and py7zr writer)
  read_from: first_article
  use: writer-version hint. Not checked by 7-Zip reader.                  [V]

StartHeaderCRC:
  where: bytes 8-11, UINT32 LE
  covers: bytes 12-31 (20 bytes: NextHeaderOffset, NextHeaderSize, NextHeaderCRC)  [V]
  read_from: first_article
  use: validates the sig header from the first article alone. A cheap "is this a 7z?" test
       with a ~2^-32 false-positive rate. Also a per-archive fingerprint.  [V]

NextHeaderOffset:
  where: bytes 12-19, UINT64 LE (REAL_UINT64, not NUMBER)
  meaning: offset of the next header, RELATIVE TO BYTE 32 (end of sig header).
           Absolute logical offset = 32 + NextHeaderOffset.                     [V]
  read_from: first_article (value); the block it points to is last_article
  use: tells you exactly which tail article holds the header. Compute the target
       volume index and in-volume offset from it.
  sanity (7zIn): must be < 2^62                                              [V]

NextHeaderSize:
  where: bytes 20-27, UINT64 LE
  meaning: byte length of the next-header block (encoded header StreamsInfo+packed data,
           or plain header). Size of the tail you need to fetch.              [V]
  read_from: first_article
  use: exact tail-fetch length. Sanity (7zIn): must be <= 2^48. Size 0 means empty archive
       (then offset and CRC must also be 0).                                 [V]
  leak: header size roughly tracks file count and path length.

NextHeaderCRC:
  where: bytes 28-31, UINT32 LE
  meaning: CRC32 of the next-header block bytes (the encoded one if encoded).   [V]
  read_from: first_article (value); verify against tail bytes
  use: validates tail fetch, catches wrong article/volume. Also a fingerprint.

# Recovery paths (7zIn / TestSignature2, verified in summary)
recovery_zero_sig:
  where: StartHeaderCRC==0 AND offset/size/CRC all 0, version bytes nonzero
  behavior: 7zIn scans the LAST 512 BYTES for kEncodedHeader+kPackInfo or kHeader+kMainStreamsInfo
  read_from: last_article
  use: recovers headers from incompletely written/damaged posts.           [V-partial]

signature_search (SFX / prefix):
  where: if bytes 0-31 are not valid, 7zIn scans forward in ~32 KB chunks for the signature
         whose StartHeaderCRC validates; base offset shifts accordingly.
  honors searchHeaderSizeLimit (0 = no search)
  read_from: first_article (prefixed/obfuscated posts, SFX stubs)
  use: detect 7z that is not at offset 0.                                    [V-partial]
```

## 3. Next-header block: encoded vs plain

```yaml
block_first_byte:
  where: first byte of the next-header block (at absolute 32+NextHeaderOffset)
  values: 0x01 kHeader (plain header) | 0x17 kEncodedHeader (packed)
  read_from: last_article
  use: branch point. Anything else -> not a valid 7z header.                  [V]

kEncodedHeader (0x17):
  where: 1 byte, followed by a StreamsInfo describing ONE folder (PackInfo+UnPackInfo[+SubStreamsInfo])
  packed data: located at 32 + PackInfo.PackPos, immediately before the header block
  read_from: last_article (usually; the packed header is contiguous with the tail)
  use: decode with the folder's coder chain, usually LZMA (0x030101), optionally AES.
  decode rules (7zIn): decoded size must equal folder unpack size; folder CRC (if present)
       must match; decoded buffer must begin with kHeader (0x01); encoding is applied once.  [V]
  DOUBLE-ENCODING: 7zIn reads exactly one decoded buffer; a second kEncodedHeader is an error. [V]

Encrypted header (-mhe=on):
  the encoded header's StreamsInfo is PLAINTEXT, so you can read WITHOUT the password:
    - coder ID 0x06F10701 (AES) and its props: NumCyclesPower, salt size, IV
    - PackInfo: packed size, PackPos
    - UnPackInfo: unpacked header size (kCodersUnPackSize)
    - folder CRC (decompressed header CRC32)                                  [V-partial]
  Only the file list / names / sizes / attributes are hidden.
```

Note for the indexer: an -mhe=on archive is identifiable even without a password. Its header-size, folder CRC, and AES IV give a stable per-archive fingerprint and reveal "encrypted headers" as a packer setting.

## 4. Property IDs (NID values)

Verified identically in 7zFormat.txt, 7zHeader.h (decimal enum starting at 0), and py7zr/libarchive tables. `[V]`

```yaml
kEnd:                   0x00  # terminator of every record list
kHeader:                0x01  # plain header root
kArchiveProperties:     0x02  # record list, skip by size until kEnd
kAdditionalStreamsInfo: 0x03  # StreamsInfo of external data streams
kMainStreamsInfo:       0x04  # pack/unpack/substream info for file data
kFilesInfo:             0x05  # file count + property records
kPackInfo:              0x06
kUnPackInfo:            0x07  # aka CodersInfo
kSubStreamsInfo:        0x08
kSize:                  0x09
kCRC:                   0x0A  # digests
kFolder:                0x0B
kCodersUnPackSize:      0x0C
kNumUnPackStream:       0x0D  # aka kNumUnpackStream
kEmptyStream:           0x0E
kEmptyFile:             0x0F
kAnti:                  0x10
kName:                  0x11
kCTime:                 0x12
kATime:                 0x13
kMTime:                 0x14
kWinAttributes:         0x15  # 7zHeader.h: kWinAttrib; libarchive: kAttributes
kComment:               0x16  # ID only; no semantics in fetched sources     UNVERIFIED semantics
kEncodedHeader:         0x17
kStartPos:              0x18  # rarely written; raw UINT64 per defined file   [V-partial]
kDummy:                 0x19  # padding; must be zero bytes
```

Unknown IDs: skip by size (7zIn sets a warning flag, not an error). Rust parser must do the same to stay forward compatible. `[V]`

## 5. Structures in the next-header block

### 5.1 ArchiveProperties (0x02) — read_from: last_article

```yaml
record_list:
  where: repeated { type: BYTE, if 0 end; size: NUMBER; data[size] }
  read_from: last_article
  use: skip. Rarely present in 7-Zip output.                                  [V]
```

### 5.2 AdditionalStreamsInfo (0x03) — read_from: last_article

```yaml
where: a StreamsInfo; its decoded streams are the external data for properties that set External=1
use: name/time/attribute data that lives outside the header; rare. Decode like kEncodedHeader.  [V]
```

### 5.3 PackInfo (0x06) — read_from: last_article

```yaml
PackPos:
  where: NUMBER
  meaning: offset of the first packed stream, relative to byte 32 (end of sig header)  [V]
  use: absolute packed-data position = 32 + PackPos; packed streams are laid out back-to-back
NumPackStreams:
  where: NUMBER
  use: count of packed streams (zero is rejected by libarchive reader)     [V-partial]
kSize (0x09) + NUMBER[NumPackStreams]:
  meaning: packed size of each stream (compressed bytes on disk/Usenet)     [V]
  use: byte range of each folder's packed input -> which article(s) to fetch
kCRC (0x0A) + Digests(NumPackStreams):
  meaning: CRC32 of each packed stream (7zIn parses, then discards)         [V]
  use: integrity check only if you have the full packed stream. Otherwise ignore.
kEnd (0x00)
```

### 5.4 UnPackInfo / CodersInfo (0x07) — read_from: last_article

```yaml
kFolder (0x0B):
  NumFolders: NUMBER
  External: BYTE (0 = folder records inline; 1 = followed by DataStreamIndex NUMBER into
            AdditionalStreamsInfo; 7zIn rejects External=1 when folders exist)        [V]
  Folder records: see 5.5
kCodersUnPackSize (0x0C):
  meaning: NUMBER per OUTPUT stream of every coder, in folder order
  use: unpacked size of each coder output. The folder's final unpacked size is the output
       NOT consumed by a bind pair.                                                  [V]
kCRC (0x0A) + Digests(NumFolders):
  meaning: CRC32 of each folder's FINAL unpacked output (7zIn verifies after decode)  [V]
  use: integrity; needs full folder decode. Also a fingerprint.
kEnd
```

### 5.5 Folder (one per folder; coder graph)

```yaml
NumCoders: NUMBER (limits in 7zIn: <= 64 coders, <= 64 in-streams per folder)       [V]
per coder:
  flags: BYTE
    bits 0-3: CodecIdSize (bytes of method ID)
    bit 4:    complex coder (has NumInStreams, NumOutStreams NUMBERs)
    bit 5:    has properties (PropertiesSize NUMBER + bytes follow)
    bit 6:    reserved, must be 0
    bit 7:    alternative methods (unused, must be 0)                                   [V]
  CodecId: CodecIdSize bytes, big-endian value (e.g. LZMA = 03 01 01)                  [V]
  if complex: NumInStreams NUMBER, NumOutStreams NUMBER  (7zIn: each coder has exactly 1 out)
  if props: PropertiesSize NUMBER, Properties bytes (raw, method-specific; see section 6)
bind pairs: NumBindPairs = NumOutStreamsTotal - 1; each (InIndex NUMBER, OutIndex NUMBER)  [V]
packed streams: NumPackedStreams = NumInStreamsTotal - NumBindPairs
  if NumPackedStreams == 1: implicit (the single unbound in-stream)
  else: NUMBER index per packed stream                                                   [V]
read_from: last_article (inside header). Coder props of the encoded header are in plaintext.
use: determines the decode pipeline and which coder props leak packer settings.
```

### 5.6 SubStreamsInfo (0x08) — read_from: last_article

```yaml
kNumUnPackStream (0x0D):
  per folder: NUMBER (default 1 if absent)
  meaning: number of files packed in this folder (solid block size)                [V]
  note: folders with 0 substreams are skipped in file mapping (7zIn FillLinks)      [V]
kSize (0x09):
  for folders with N>1 substreams: N-1 NUMBERs; last size = folder unpack size - sum
  if N>1 and kSize absent: header is invalid                                        [V]
  use: exact per-file byte offsets/lengths inside a solid folder
kCRC (0x0A) + Digests:
  covers substreams whose CRC is not already known. A folder with exactly 1 substream
  and a folder CRC INHERITS that CRC and does not store it twice.                  [V]
  use: per-file CRC32 (verification only after full file is available)
kEnd
```

### 5.7 FilesInfo (0x05) — read_from: last_article

```yaml
NumFiles: NUMBER
property records: repeated { type: BYTE (0 = end); size: NUMBER; data[size] }
  unknown types: skipped by size (7zIn UnsupportedFeatureWarning)                  [V]
```

Per-property:

```yaml
kEmptyStream (0x0E):
  where: size NUMBER, then BitField over NumFiles
  meaning: bit set = file has NO data stream (directory or empty file)             [V]
  use: separates data-carrying entries from empty ones. Must be read first (other
       vectors index into it).

kEmptyFile (0x0F):
  where: size NUMBER, then BitField over the COUNT OF EMPTY-STREAM ENTRIES
  meaning: bit set = empty FILE (not directory)                                    [V]
  use: directory = empty-stream AND not empty-file. Attribute DIRECTORY (0x10) is a
       second signal.

kAnti (0x10):
  where: size NUMBER, BitField over empty-stream entries
  meaning: anti-item (deletion marker used in update archives)                    [V]
  use: flag as deletion; rare in Usenet posts.

kName (0x11):
  where: size NUMBER; External BYTE (0 = inline, 1 = DataIndex NUMBER into additional streams);
         then NUL-terminated UTF-16LE strings, one per file
  meaning: file path. Separator per py7zr: '/'                                    [V-partial]
  note: 7zIn reads names by scanning for each 0x0000 terminator; leftover bytes = header warning. [V]
  read_from: last_article (inside header). Names are plaintext unless -mhe=on.

kCTime (0x12), kATime (0x13), kMTime (0x14):
  where: size NUMBER; AllAreDefined BooleanList; External BYTE (+DataIndex if 1);
         then UINT64 LE FILETIME (100 ns ticks since 1601-01-01 UTC) per defined file
  note: py7zr's docs page says "FileTime NUMBERs"; its code and 7zIn both read fixed 8 bytes.
        Use FIXED 8 bytes.                                                         [V]
  presence: -mtc / -mta flags (7-Zip) control CTime/ATime; MTime is the common one.  UNVERIFIED defaults
  read_from: last_article

kWinAttributes (0x15):
  where: size NUMBER; AllAreDefined BooleanList; External BYTE (+DataIndex if 1);
         UINT32 LE per defined file
  low 16 bits: Windows attributes: READONLY 0x01, HIDDEN 0x02, SYSTEM 0x04,
               DIRECTORY 0x10, ARCHIVE 0x20, REPARSE_POINT 0x400                    [V]
  bit 0x8000: FILE_ATTRIBUTE_UNIX_EXTENSION (Unix mode present)                     [V]
  high 16 bits (>>16): Unix st_mode (type + permissions) when bit 0x8000 is set       [V-partial]
  note: the spec text puts 0x8000 inside the "Windows 0-15" range, which is inconsistent;
        treat 0x8000 as the Unix flag and the high 16 bits as st_mode.
  use: symlink detection (S_IFLNK in mode), exec bits, owner/umask leak (p7zip/Linux packers).
  mode-extraction code path: UNVERIFIED (not in fetched excerpts)

kStartPos (0x18):
  where: size NUMBER; AllAreDefined; External; UINT64 LE per defined file        [V-partial]
  use: rare. Ignore unless seen. py7zr's reader has a bug (compares bytes to int) so writers
       using it are untested there.

kDummy (0x19):
  where: size NUMBER, then size zero bytes
  meaning: alignment padding (7-Zip and py7zr writers align names to 4 bytes)     [V]
  leak: presence and length show a 7-Zip-compatible writer; non-zero bytes = header warning in 7zIn.

kComment (0x16):
  ID only; no semantics in any fetched source. Skip by size.                      UNVERIFIED
```

External-data rule (applies to Name, Times, Attributes, StartPos): External=1 means the vector lives in the AdditionalStreamsInfo stream at DataIndex. In practice 7-Zip writes External=0 for all of these. `[V]`

File-to-stream mapping (7zIn FillLinks):
- Files with EmptyStream=0 consume unpack sizes in order; NumFiles - NumEmptyStreams must equal the number of substreams total. `[V]`
- Empty-stream files get size 0, no CRC, and are dirs unless marked EmptyFile. `[V]`
- Within a folder, files appear in substream order. `[V]`

## 6. Coder method IDs and property layouts

Method IDs as read in C/7zDec.c (`[V]`), libarchive (`[V]`), py7zr table (`[V]` unless noted).

| Method | ID | Props (where) | Notes |
|---|---|---|---|
| COPY | 0x00 | none | packed == unpacked (7zDec enforces). Store mode (-mx0). `[V]` |
| Delta | 0x03 | 1 byte: distance-1 | `[V]` (7zDec: exactly 1 byte) |
| BCJ x86 | 0x03030103 | none | `[V]` (7zDec, libarchive). py7zr also lists "BCJ 0x04" and "P7Z_BCJ 0x03030103"; use 0x03030103 and treat 0x04 as an UNVERIFIED alias. |
| BCJ2 | 0x0303011B | none; 4 in-streams | `[V]` ID; layout is 4-coder folder `[V-partial]` |
| PPC | 0x03030205 | none | `[V]` |
| IA64 | 0x03030401 | none | `[V]` (not PPMd; see below) |
| ARM | 0x03030501 | none | `[V]` |
| ARMT | 0x03030701 | none | `[V]` |
| SPARC | 0x03030805 | none | `[V]` |
| ARM64 | 0x0A | 0 or 4 bytes: PC UInt32 LE, must be multiple of 4 | `[V]` (7zDec). Not in py7zr table. |
| RISC-V | 0x0B | 0 or 4 bytes: PC UInt32 LE, must be even | `[V]` (7zDec) |
| LZMA | 0x030101 | 5 bytes: byte0 = (pb*5+lp)*9+lc; bytes1-4 = dictSize UInt32 LE | `[V]` ID; props layout matches the spec example `5D 00 10 00 00` = lc3 lp0 pb2, dict 1 MiB. Byte-level meaning `[V-partial]`. |
| LZMA2 | 0x21 | 1 byte: dictionary code | `[V]` exactly 1 byte (7zDec). Dict decoding formula UNVERIFIED (standard xz-style). |
| PPMd (PPMD7/H) | 0x030401 | 5 bytes: props[0] = order; props[1..4] = memSize UInt32 LE | `[V]` (7zDec layout; the 7zDec PPMd path is behind Z7_PPMD_SUPPORT) |
| BZip2 | 0x040202 | none | from py7zr table `[V]` (ID) |
| Deflate | 0x040108 | none | from py7zr table |
| Deflate64 | 0x040109 | none | from py7zr table; py7zr's footnotes disagree on support |
| ZSTD | 0x04F71101 | ? | py7zr table; 7-Zip-zstd fork ID, not in 7zDec `[V-partial]` |
| Brotli | 0x04F71102 | ? | py7zr table; fork ID |
| LZ4 | 0x04F71104 | ? | py7zr table; fork ID |
| LZS | 0x04F71105 | ? | py7zr table; fork ID |
| Lizard | 0x04F71106 | ? | py7zr table; fork ID |
| AES-256 + SHA-256 | 0x06F10701 | see below | `[V]` ID (7zDec/libarchive/py7zr) |
| ZipCrypto | 0x06F10101 | ? | libarchive constant `[V]` |
| RAR29 crypto | 0x06F10303 | ? | libarchive constant `[V]` |

Caution: PPMd is 0x030401 and IA64 is 0x03030401. Easy to confuse in code.

### 6.1 AES-256 + SHA-256 (0x06F10701) coder properties — `[V]` (7zAes.cpp)

```yaml
where: properties blob, total length = 2 + saltSize + ivSize, or exactly 1 byte if no salt/IV
byte0:
  bits 0-5 (& 0x3F): NumCyclesPower (key stretching exponent N)
  bit 6 (0x40): IV present; contributes +1 to IV size
  bit 7 (0x80): salt present; contributes +1 to salt size
byte1 (only if bit 6 or 7 set):
  high nibble: salt size minus bit7        (saltSize = bit7 + (b1 >> 4))
  low nibble:  IV size minus bit6          (ivSize   = bit6 + (b1 & 0x0F))
then: salt bytes, then IV bytes
validation: NumCyclesPower <= 24, or exactly 0x3F (raw-key mode); else unsupported
read_from: last_article (for normal headers) / FIRST-or-LAST for folder data
use: AES param fingerprint and encryption detection without a password.

key derivation (CalcKey, non-0x3F):
  hash = SHA-256 over a single stream of 2^N blocks; block i = salt || password || 8-byte counter
         where counter field is zeroed and i is written in its first 4 bytes (SetUi32; LE assumed)
  key = final SHA-256 digest (32 bytes)                                                   [V-partial]
raw-key mode (N=0x3F): key = salt || password, truncated/zero-padded to key size            [V]
encoder default: NumCyclesPower = 19 (0x13); with IV 16 bytes, no salt, writes 0x40|19 = 0x53 then 0x0F then 16 IV bytes  [V]
password encoding: UNVERIFIED (7-Zip is believed to use UTF-16LE password bytes)
padding/CRC inside AES: delegated to MyAes.h, not fetched                              UNVERIFIED
```

### 6.2 LZMA / LZMA2 / PPMd dictionary and model fields

```yaml
LZMA props (5 bytes): byte0 = (pb*5 + lp)*9 + lc; bytes1-4 = dictSize UInt32 LE       [V-partial]
  default lc=3 lp=0 pb=2 => byte0 = 0x5D (matches spec example)                         [V]
LZMA2 props (1 byte): dictionary code (0..40)                                          [V-partial]
PPMd props (5 bytes): order (1 byte), memSize (UInt32 LE)                             [V]
```

## 7. Multi-volume (.7z.001 / .002 / ...)

```yaml
logical_stream: concatenation of volumes in order; all offsets (32 + NextHeaderOffset,
                32 + PackPos) are in LOGICAL coordinates                              UNVERIFIED (7-Zip split handler not fetched)
volume_size: all volumes except the last have the same size (the split size);
             last volume is shorter                                                   UNVERIFIED
map: volume_index = floor(logical_off / volume_size); in_volume_off = logical_off % volume_size
signature: only in volume 001 at logical 0; header sits near logical END -> last volume.
read_from: the last volume, last article. The sig header (first article of .001) gives the offset.
Usenet detail: the volume size is only known if the indexer sees volume 001 completely or the
  NZB/subject gives it. Logical end = sum of volume sizes. Without the last volume you cannot
  locate the header, and the NextHeaderOffset value from volume 001 still tells you the
  absolute target offset.
```

## 8. Solid blocks, COPY mode, and partial downloads

```yaml
store_mode_COPY:
  packed stream = raw file bytes; file i data at 32 + PackPos + sum(sizes of files before i)
  read_from: first_article for file 0 (and any file whose offset is in article 0-5)
  use: inner container headers (EBML/MP4 ftyp, moov at start) readable from the first article.
       MP4 moov-at-end still needs the last article.                                  [V] for COPY semantics
solid_LZMA_folder:
  one LZMA stream for N files. Decoding is sequential from the folder's packed start.
  read_from: first N articles of the folder's packed range give file 0's prefix;
             later files need the whole decoded prefix up to their start.              [V-partial]
  per-file CRC32 (SubStreams) needs the complete file. Partial prefix = no CRC check.
non_solid (-ms=off):
  one folder per file; the exact packed range is known from PackInfo, so you can fetch
  just the article range for one file.                                               [V]
BCJ2 folders:
  4 packed in-streams (main, call, jump, range-coder). Partial decode is harder; layout UNVERIFIED.
```

## 9. What leaks the packer and its settings

```yaml
leak_LZMA_dict_size:
  where: LZMA props bytes 1-4 (in the encoded header AND in each folder)
  use: maps to -md / -mx level. Exact level mapping: UNVERIFIED (not fetched). Note the
       encoder may shrink the dictionary to input size.
leak_lc_lp_pb:
  where: LZMA props byte0. Default 0x5D (lc3 lp0 pb2). Deviation = non-default -m0 lc/lp/pb.   [V]
leak_LZMA2_dict_byte: where: LZMA2 props byte. Level/dict hint.                          [V-partial]
leak_PPMd_order_mem: where: PPMd props. Encodes -mo / -mmem; defaults UNVERIFIED.           [V]
leak_filters:
  BCJ / BCJ2 / ARM64 / Delta presence in the coder list. 7-Zip auto-selects BCJ for exe-like
  content; ARM64 only on newer 7-Zip; reveals file type heuristics and 7-Zip version.       [V-partial]
leak_header_compression:
  presence of kEncodedHeader (0x17) = -mhc on (default); absent = -mhc=off (plain header
  readable directly in the last article).                                                  [V]
leak_header_encryption:
  -mhe=on -> AES coder in the encoded header's StreamsInfo (plaintext). Header-encrypted
  archives are a distinct packer setting.                                                   [V]
leak_attributes: 0x8000 Unix flag + st_mode high bits => Linux/p7zip packer, and uid/mode
  leakage (owner write bit, umask).                                                        [V-partial]
leak_timestamps:
  presence of kCTime/kATime records reflects -mtc / -mta flags; kMTime is common.           UNVERIFIED defaults
leak_dummy_padding:
  kDummy padding to 4-byte alignment reflects a 7-Zip-style writer.                        [V]
leak_record_order:
  7-Zip writes FilesInfo properties in a fixed order (EmptyStream, EmptyFile, Name,
  times, Attributes, Dummy). Order fingerprint                                             UNVERIFIED
leak_version_bytes: Minor 0x04 = 7-Zip 9.x+ writer.                                       [V]
```

## 10. Per-creation random values and fingerprints

```yaml
AES_IV:
  where: AES props IV bytes (16 bytes typical). Random per AES coder (per folder and per encoded header).  [V]
  use: per-encryption-session fingerprint. Same IV across archives = same encoder run / tool.
AES_salt:
  where: AES props salt bytes. The 7-Zip encoder's salt generation is commented out in the fetched
         excerpt, so salt size is often 0.                                                 [V-partial]
StartHeaderCRC / NextHeaderCRC:
  deterministic over header bytes; header bytes include times and file list, so they vary per run.  [V]
  use: stable per-archive identifier for dedup across reposts (time-free if you hash only sizes).
folder/pack/file CRCs: deterministic for the same content; cross-post dedup key without decoding
  the whole archive if you have the folder CRC in the (decoded) header.                    [V]
MTime/CTime/ATime: per-file, not random, but reveal packer clock and -mtc/-mta choices.  [V-partial]
```

## 11. Indexer field table (quick reference: where -> read_from)

| Field | where | read_from |
|---|---|---|
| signature | 0-5 | first_article |
| version major/minor | 6, 7 | first_article |
| StartHeaderCRC | 8-11 (covers 12-31) | first_article |
| NextHeaderOffset | 12-19 (+32 absolute) | first_article (value) |
| NextHeaderSize | 20-27 | first_article |
| NextHeaderCRC | 28-31 | first_article (value) |
| block first byte kHeader/kEncodedHeader | 32+NextHeaderOffset | last_article |
| encoded-header StreamsInfo | in block | last_article |
| encoded-header packed bytes | 32+PackPos (before header) | last_article |
| AES props (encoded header) | in block, plaintext | last_article |
| PackInfo sizes/CRCs | in header | last_article |
| Folder coders, props, bind pairs | in header | last_article |
| CodersUnPackSize, folder CRC | in header | last_article |
| SubStreams sizes/CRCs | in header | last_article |
| kEmptyStream/EmptyFile/Anti | in header | last_article |
| Names (UTF-16LE) | in header | last_article |
| MTime/CTime/ATime | in header | last_article |
| WinAttributes (+ Unix mode) | in header | last_article |
| kDummy / kComment / kStartPos | in header | last_article |
| file 0 data, store mode | 32+PackPos ... | first_article |
| file 0 prefix, solid LZMA | packed from 32+PackPos | first_article (prefix only) |
| later solid files | whole prefix | full (or several articles) |

## 12. Rust crates

| Crate | Pure Rust? | Maintained? | Parse headers without extracting? | Partial-buffer header parse? | Notes |
|---|---|---|---|---|---|
| sevenz-rust2 (hasenbanck/sevenz-rust2) | Yes per README; optional features pull codecs (brotli, deflate, lz4, zstd) | Yes (active fork; example pins 0.22); release date UNVERIFIED | UNVERIFIED (README shows decompress_file, decompress_file_with_password, compress_to_path, ArchiveWriter, no header-only API in README) | No. Needs Read+Seek. A sparse Read+Seek over fetched article ranges would work only if the reader never reads a byte you did not fetch. UNVERIFIED | Apache-2.0. Codecs: COPY, LZMA, LZMA2, BZIP2, PPMD always. Filters: BCJ x86/ARM/ARM64/ARM_THUMB/RISC_V/PPC/SPARC/IA64/DELTA; BCJ2 decompress only. AES via password. |
| sevenz-rust (dyz1990/sevenz-rust) | Believed yes | Superseded: sevenz-rust2 README calls it "unmaintained" `[V]`; repo fetch failed this session | UNVERIFIED | UNVERIFIED | Prefer sevenz-rust2 if using a crate. |
| py7zr | No (Python) | Yes | Yes (header parse) | No | Reference only. Doc-page errors noted in section 13. |
| libarchive (7zip reader) | No (C; Rust FFI via libarchive-sys-like bindings) | Yes | Yes | No (needs full stream) | Reference source for constants; not pure Rust. |
| lzma-rs | Yes (from memory) | UNVERIFIED | n/a (codec only) | Streaming decode yes (codec API) UNVERIFIED | Candidate for decoding the encoded header from its packed bytes. Needs props (lc/lp/pb, dict). |
| RustCrypto aes / cbc / sha2 | Yes | Yes (UNVERIFIED version) | n/a | n/a | For the AES key stretch (2^N SHA-256 blocks) and decrypt. |
| crc32fast | Yes | Yes (UNVERIFIED version) | n/a | n/a | CRC32 for StartHeaderCRC, NextHeaderCRC, folder/file CRCs. |

Recommendation: write the header parser by hand (sections 2-7 above, about 500 lines of Rust with a NUMBER reader), and use a codec crate only for the encoded header and file-data decode. A hand parser lets you require only the byte ranges you have fetched and fail cleanly on missing tail data.

## 13. Discrepancies between sources (use the right side)

1. Time records: py7zr docs say "FileTime NUMBERs"; py7zr code and 7zIn read fixed UINT64 LE. Use fixed 8 bytes.
2. Attributes: spec says Windows attrs are bits 0-15 and Unix bits are 16-31, but lists 0x8000 inside the Windows range. Use: 0x8000 = Unix flag, mode = attr >> 16.
3. BCJ x86 ID: py7zr lists both "BCJ 0x04" and "P7Z_BCJ 0x03030103". 7zDec and libarchive use 0x03030103.
4. PPMd (0x030401) vs IA64 (0x03030401): distinct IDs.
5. Coder limit: spec text says max "five" then "four" coders; 7zIn and py7zr constants allow 64 coders and 64 in-streams. Use 64.
6. py7zr's kStartPos reader compares bytes to int 0x00 (always false). Do not copy it.
7. py7zr's header decoder uses only packsizes[0] for every folder of the encoded header. Correct only for one folder.
8. ARM64 (0x0A) and RISC-V (0x0B) are absent from the py7zr table and the 7zFormat text; they come from C/7zDec.c only.
9. Property name for 0x15: kWinAttributes (7zFormat, 7zHeader.h kWinAttrib), kAttributes (py7zr, libarchive). Same ID.
10. Sig-header recovery: 7zIn accepts a zeroed start header and scans the last 512 bytes. Not in the spec text.

## 14. UNVERIFIED items (explicit list)

- kComment semantics (ID 0x16); presence in real archives.
- Exact LZMA2 dictionary-code formula; exact LZMA level-to-dict mapping; PPMd default order/memory.
- ZSTD/Brotli/LZ4/LZS/Lizard property layouts (fork IDs).
- Multi-volume: equal-size volume assumption and logical-offset semantics (no split-handler source fetched).
- AES password encoding (UTF-16LE?), padding/CRC inside AES (MyAes.h not fetched), per-folder IV rules.
- CTime/ATime default presence under -mtc/-mta; record-order fingerprint.
- Unix mode extraction code path (high 16 bits) beyond the spec wording.
- Path separator in kName (py7zr says '/').
- sevenz-rust2 header-only API and partial-buffer behavior; sevenz-rust status (repo fetch failed).
- lzma-rs and PPMd-in-Rust crate maintenance status.

## 15. Sources

- 7-Zip format spec (7zFormat.txt, version 18.06 text): https://raw.githubusercontent.com/ip7z/7zip/main/DOC/7zFormat.txt
- py7zr archive format doc: https://py7zr.readthedocs.io/en/latest/archive_format.html
- 7-Zip header constants (7zHeader.h): https://raw.githubusercontent.com/ip7z/7zip/main/CPP/7zip/Archive/7z/7zHeader.h
- 7-Zip header reader (7zIn.cpp): https://raw.githubusercontent.com/ip7z/7zip/main/CPP/7zip/Archive/7z/7zIn.cpp
- 7-Zip AES coder (7zAes.cpp): https://raw.githubusercontent.com/ip7z/7zip/main/CPP/7zip/Crypto/7zAes.cpp
- 7-Zip C decoder (7zDec.c): https://raw.githubusercontent.com/ip7z/7zip/main/C/7zDec.c
- libarchive 7zip reader: https://raw.githubusercontent.com/libarchive/libarchive/master/libarchive/archive_read_support_format_7zip.c
- py7zr header parser: https://raw.githubusercontent.com/miurahr/py7zr/master/py7zr/archiveinfo.py
- sevenz-rust2 README: https://raw.githubusercontent.com/hasenbanck/sevenz-rust2/main/README.md
- Failed this session (not used): https://raw.githubusercontent.com/dyz1990/sevenz-rust/master/README.md ; https://github.com/dyz1990/sevenz-rust (client error)
