# W3c: RAR5 service blocks (CMT/QO/RR/ACL/STM), locator, RAR4 recovery, RAR3/RAR5 .rev, RAR4 comment, ReScene SRS

Status legend: VERIFIED = read in the cited source file (line numbers from the downloaded copy). UNVERIFIED = not confirmed in the sources reached within the time cap.

Sources were read from scratchpad copies of https://raw.githubusercontent.com/pmachapman/unrar/master/ (arcread.cpp, headers.hpp, headers5.hpp, qopen.cpp, qopen.hpp, recvol.cpp, recvol3.cpp, recvol5.cpp, arccmt.cpp, encname.cpp). rar.hpp was fetched too but does not define REV5_SIGN.

read_from column: first_article = data in the first article/segment of the archive; last_article = data in the tail; full = needs whole file; separate = a different file.

---

## 1. RAR5 service headers (HEAD_SERVICE, type 3)

### 1.1 Common RAR5 service header layout

| Field | Encoding | Meaning | read_from | Status |
|---|---|---|---|---|
| HeaderCRC | 4 bytes | CRC of the header | first_article | VERIFIED (arcread.cpp ReadHeader50 reads ShortBlock, same as file headers) |
| HeaderSize | vint | size of header after this field | first_article | VERIFIED (arcread.cpp:648 BlockSize=Raw.GetV()) |
| HeaderType | vint | 3 = HEAD_SERVICE | first_article | VERIFIED (arcread.cpp:676) |
| HeaderFlags | vint | HFL_EXTRA 0x01, HFL_DATA 0x02, HFL_SKIPIFUNKNOWN 0x04, HFL_SPLITBEFORE 0x08, HFL_SPLITAFTER 0x10, HFL_CHILD 0x20, HFL_INHERITED 0x40 | first_article | VERIFIED (headers5.hpp:10-22; arcread.cpp:682) |
| ExtraSize | vint | present only if HFL_EXTRA | first_article | VERIFIED (headers5.hpp:10) |
| DataSize | vint | present only if HFL_DATA; size of the data area | first_article | VERIFIED (headers5.hpp:12) |
| FileFlags | vint | FHFL_DIRECTORY 0x01, FHFL_UTIME 0x02, FHFL_CRC32 0x04, FHFL_UNPUNKNOWN 0x08 | first_article | VERIFIED (arcread.cpp ~1247; headers5.hpp:32-35) |
| UnpSize | vint | uncompressed size of the data area (for QO this is the raw QO stream length) | first_article | VERIFIED (arcread.cpp:~1252 hd->UnpSize=Raw.GetV()) |
| FileAttr | vint | attributes | first_article | VERIFIED (arcread.cpp ~1256) |
| mtime | 4 bytes LE | only if FHFL_UTIME | first_article | VERIFIED (arcread.cpp: hd->mtime.SetUnix(Raw.Get4()) when FHFL_UTIME) |
| DataCRC32 | 4 bytes | only if FHFL_CRC32; hd->FileHash CRC32 of the unpacked data | first_article | VERIFIED (arcread.cpp:830-835) |
| CompInfo | vint | bits 0-5 UnpVer (0 = RAR5, 1 = RAR7 compat); bits 7-9 Method (0 = stored, 1-5 compressed); bit 6 solid; bit 20 RAR5 compat | first_article | VERIFIED (arcread.cpp:837-850; headers5.hpp:44-65) |
| HostOS | vint | 0 = Windows, 1 = Unix (HOST5_*) | first_article | VERIFIED (arcread.cpp:852-853) |
| NameSize | vint | byte length of name | first_article | VERIFIED (arcread.cpp:854) |
| Name | UTF-8 bytes | service name: "CMT", "QO", "ACL", "STM", "UOW", "AV", "RR", "EA2" | first_article | VERIFIED (headers.hpp:118-125 SUBHEAD_TYPE_* as L"..." constants) |
| Extra area | repeated (vint size, vint type, body) | see 1.4 | first_article | VERIFIED (arcread.cpp ProcessExtra50 ~1180-1260) |
| Data area | DataSize bytes after the header | payload for CMT (packed), QO (raw), RR (recovery sectors) | depends | VERIFIED for read path (arcread.cpp:1491-1560 ReadSubData; qopen.cpp:208-230 ReadBuffer) |

### 1.2 CMT (archive comment)

- Detection: a service block whose Name == "CMT". arcread.cpp:909-910 sets MainComment=true.
- Storage: the comment is the service block's data area, compressed or stored. ReadSubData (arcread.cpp:1491) accepts SubHead.Method 0..5 and UnpVer up to VER_UNPACK7. Method 0 = stored (CmdExtract::UnstoreFile). Other methods run Unpack.DoUnpack.
- Limit: reading to memory rejects UnpSize > 0x1000000 (16 MiB) (arcread.cpp ~1520).
- Integrity: the decoded data is CRC-checked against SubHead.FileHash (arcread.cpp:~1556, UIERROR_SUBHEADERDATABROKEN).
- Encryption: if SubHead.Encrypted, a password is required, else the read returns false.
- Flag SUBHEAD_FLAGS_CMT_UNICODE 0x00000001 is defined (headers.hpp:130) but its use was not checked; treat the text encoding as UNVERIFIED (assume UTF-8 for names).
- read_from: first_article if the comment is small and the CMT block sits right after the main header; otherwise full. Block position is found via SearchSubBlock(SUBHEAD_TYPE_CMT) after GetStartPos() (arcread.cpp: Archive::DoGetComment, ~line 38-41 in arccmt.cpp).

### 1.3 QO (quick open cache)

Service block named "QO". Its data area is NOT compressed. It is read raw.

- Data area bytes start at RawDataStart = Tell() after the header; length RawDataSize = SubHead.UnpSize (qopen.cpp:90-92). ReadBuffer reads raw bytes (qopen.cpp:208-230). If the archive is encrypted, the data is decrypted with CRYPT_RAR50 (qopen.cpp:98-112).
- Quick-open stream is a sequence of entries. Each entry (qopen.cpp:233-281 ReadRaw, 285-300 ReadNext):
  1. First 7 bytes: CRC32 (4 bytes, compared with Raw.GetCRC50() at qopen.cpp:280), then SizeBytes-encoded vint giving the size field, then BlockSize vint. SizeToRead = BlockSize - (7 - SizeBytes - 4). Entries with BlockSize 0 or SizeBytes 0 are invalid.
  2. Inside the entry body: Flags vint; Offset vint; HeaderSize vint (must be <= MAX_HEADER_SIZE_RAR5); then HeaderSize raw bytes = a cached RAR5 block header.
  3. Absolute position of the cached header: LastReadHeaderPos = QOHeaderPos - Offset (qopen.cpp:298). So Offset is a backward distance from the QO service header's start.
- Use: QuickOpen::Read/Seek serves reads of cached header bytes from memory, falling back to File::Read for everything else (qopen.cpp:124-165). QOpen.Load is invoked from ReadHeader when MainHead.Locator && QOpenOffset > 0 (arcread.cpp:788-798), and it is prohibited while loading itself (qopen.cpp:80-82).
- Consequence for listing: the cached headers are exact copies of file/service headers (name, sizes, Method, flags). Walking the QO stream gives the file list without touching the packed data. The cache is written by the archiver, so a full file list from the QO stream is possible only if the QO block is fully present.
- Where QO lives: its position is given by the locator (section 1.5). The "tail" placement is not stated in these sources; UNVERIFIED that the QO block is always near the end.
- Flags field of an entry: meaning of individual bits UNVERIFIED (not defined in headers5.hpp).
- read_from: last_article if QO is at the tail (location UNVERIFIED); full otherwise. Locator itself is first_article.

### 1.4 RR (recovery record) service block

- Name "RR" (arcread.cpp:887, SUBHEAD_TYPE_RR). SearchRR() (arcread.cpp:79-93) first tries the locator's RR offset, then scans with SearchSubBlock.
- Recovery percent: the SUBDATA extra field (FHEXTRA_SUBDATA = 0x07; headers5.hpp:88). Its body is one vint, the percentage. Older archives (up to RAR 6.02) stored one byte; RAR 6.10+ stores a vint because the max grew from 99% to 1000% (arcread.cpp:913-919 uses RawRead.GetV()). VERIFIED.
- Special case: if the service block is the last extra field and one byte short, FieldSize is corrected by +1 for RAR 5.21-era writers (arcread.cpp:1236-1247). VERIFIED.
- Recovery data: the recovery sectors occupy the RR block's data area (DataSize), which is skipped on normal header walks via NextBlockPos. Exact per-sector layout (RS16 ECC of DataCount/RecCount volumes) is in recvol5.cpp (RSCoder16, RecBufferSize). Record-size formula: RecBufferSize = TotalBufferSize / MissingVolumes, rounded to even and SSE-aligned (recvol5.cpp:359-364). This is a per-rev-volume buffer size, not a fixed record size stored in the RR block. The recovery block size in the archive itself is UNVERIFIED.
- Percentage to bytes: the percent is user input; the byte count is not computed in these sources. UNVERIFIED.
- read_from: last_article (RR is normally written at the end). Its position is the locator RR offset (section 1.5), so it can be found without reading the whole archive when the locator is present.

### 1.5 Locator record (main header extra field type 0x01)

- Main header (HEAD_MAIN) extra fields are parsed in arcread.cpp:1005-1033. MHEXTRA_LOCATOR = 0x01 (headers5.hpp:68).
- Body after the extra size and type vints:
  - Flags vint: MHEXTRA_LOCATOR_QLIST = 0x01 (QO offset present), MHEXTRA_LOCATOR_RR = 0x02 (RR offset present) (headers5.hpp:72-73).
  - If QLIST: QO offset vint. Absolute position = offset + CurBlockPos (main header start). An offset of 0 means the reserved space was too small and the value is not stored (arcread.cpp:1012-1020).
  - If RR: RR offset vint, same rule (arcread.cpp:1022-1027).
- Note: positions are relative to the main header start, not the file start. For multi-volume archives, CurBlockPos is the main header position in the current volume.
- read_from: first_article (main header is at the start). The offsets then point at last_article (QO / RR) or anywhere.

### 1.6 ACL and STM (NTFS)

- Names: "ACL" (SUBHEAD_TYPE_ACL, headers.hpp:120), "STM" (SUBHEAD_TYPE_STREAM, headers.hpp:121). arcread.cpp:1518 describes them as "the archive comment, NTFS ACL or Zone.Identifier NTFS stream" in the context of ReadSubData, which is the same decode path as CMT (compressed/stored, CRC-checked, 16 MiB memory cap).
- For a file: the "STM" service is read per file (child of a file header, HFL_CHILD 0x20). For "Zone.Identifier" the content is "[ZoneTransfer]\r\nZoneId=3..." text. The sub-field that stores the stream name is in the service name/extra; exact layout UNVERIFIED.
- read_from: first_article (child blocks follow their parent file header in the same article region).

### 1.7 Other RAR5 service names
- "UOW" (Unix owner), "AV" (authenticity verification), "EA2" (OS/2 EA). Names verified in headers.hpp:118-125. Layouts not read.

---

## 2. RAR4 recovery and service blocks

### 2.1 PROTECT_HEAD (0x78, HEAD3_PROTECT)

Source: arcread.cpp:466-473 (ReadHeader15), headers.hpp:13 (SIZEOF_PROTECTHEAD = 26), headers.hpp:331-336 (struct ProtectHeader).

| Field | Offset (after 7-byte short block header) | Size | Meaning | read_from | Status |
|---|---|---|---|---|---|
| HEAD_CRC | 0 | 2 | CRC16 of block | first_article | VERIFIED (short block header, arcread.cpp ~190-200) |
| HEAD_TYPE | 2 | 1 | 0x78 | first_article | VERIFIED (headers.hpp:82-84) |
| HEAD_FLAGS | 3 | 2 | flags (no defined bits read here) | first_article | VERIFIED read order |
| HEAD_SIZE | 5 | 2 | size of header | first_article | VERIFIED (SIZEOF_PROTECTHEAD = 26 total) |
| DataSize | 7 | 4 (LE) | size of recovery data following header; NextBlockPos += DataSize | last_article | VERIFIED (arcread.cpp:468, 473) |
| Version | 11 | 1 | version byte | first_article | VERIFIED (arcread.cpp:469) |
| RecSectors | 12 | 2 (LE) | number of recovery sectors | first_article | VERIFIED (arcread.cpp:470) |
| TotalBlocks | 14 | 4 (LE) | total data blocks in archive | first_article | VERIFIED (arcread.cpp:471) |
| Mark | 18 | 8 | signature bytes (expected "Protect!") | first_article | Field read verified (arcread.cpp:472); literal "Protect!" value UNVERIFIED (not in the sources read) |

- Field sizes check: 7 (short) + 4 + 1 + 2 + 4 + 8 = 26. VERIFIED arithmetic against SIZEOF_PROTECTHEAD.
- Recovery percentage: the sources do not compute "Recovery record x%". Candidate formula RecSectors / TotalBlocks is UNVERIFIED.
- Note: in RAR4 the PROTECT block is the recovery record; there is no NEWSUB "RR_PROTECT" name in the sources read. UNVERIFIED that a NEWSUB "RR" subheader with "RR_PROTECT" exists. The RAR4 "RR" that the sources do read is a HEAD3_SERVICE block (section 2.2).

### 2.2 RAR4 NEWSUB / service "RR" (HEAD3_SERVICE 0x7a)

- Parsed as a file-like block with a name (arcread.cpp:330-392). After the name, DataSize = HeadSize - NameSize - SIZEOF_FILEHEAD3 (minus 8 if LHD_SALT) bytes are copied to SubData (arcread.cpp:375-384).
- Whether that SubData holds a recovery percentage in RAR4: the RAR5 percent parser (arcread.cpp:913-919) is in ReadHeader50 only. RAR4 RR percent parsing: UNVERIFIED.
- CMT in RAR4 service: same path; if name == "CMT" then MainComment=true (arcread.cpp:385-386).

### 2.3 RAR4 archive comment (COMM_HEAD 0x75, HEAD3_CMT)

Source: arcread.cpp:459-465 (struct CommentHeader headers.hpp:322-328, SIZEOF_COMMHEAD = 13 at headers.hpp:12).

| Field | Offset (after 7-byte short header) | Size | Meaning | Status |
|---|---|---|---|---|
| UnpSize | 0 | 2 (LE) | uncompressed comment size | VERIFIED (arcread.cpp:460) |
| UnpVer | 2 | 1 | unpack version (must be 15..29 = VER_UNPACK) | VERIFIED (arcread.cpp:461; arccmt.cpp:60) |
| Method | 3 | 1 | 0x30 = stored; 0x31..0x35 compressed | VERIFIED (arccmt.cpp:58-60) |
| CommCRC | 4 | 2 (LE) | low 16 bits of CRC | VERIFIED (arcread.cpp:464; arccmt.cpp:92 compare with GetCRC32()&0xffff; stored case arccmt.cpp:126 uses ~CRC32(0xffffffff,data)&0xffff) |

- Comment body: CmtLength = HeadSize - SIZEOF_COMMHEAD (arccmt.cpp:54), i.e. bytes after the 13-byte header.
- Compressed: Unpack.Init(0x10000), DoUnpack(UnpVer) (arccmt.cpp:87-90).
- Output: converted from OEM to wide on Windows (arccmt.cpp:104-110).
- Note: this is the RAR 2.9-and-earlier location (MHD_COMMENT 0x0002 in the main header, headers.hpp:28; DoGetComment arccmt.cpp:27-34). RAR 3.0+ uses CMT NEWSUB (section 2.2 / 1.2 for RAR5).
- read_from: first_article for old style (comment in main header region), otherwise via SearchSubBlock anywhere (full or tail).

---

## 3. Recovery volumes (.rev)

### 3.1 RAR 3.x .rev (recvol3.cpp)

- Naming (recvol3.cpp:65-78 IsNewStyleRev): name.rev without a "_N_N" digit group is NEW style (RAR 3.10+). Two digit groups (name.arc_3_1.rev) is OLD style (RAR 3.0). VERIFIED.
- NEW style trailer (last 7 bytes, recvol3.cpp:165-189):
  - Bytes at EOF-7, EOF-6, EOF-5: three bytes read in order into P[2], P[1], P[0], each stored as (value + 1) (recvol3.cpp:179-181). Meaning of P[] (volume ordinal / counts) UNVERIFIED beyond these being per-file numbering fields.
  - Bytes at EOF-4..EOF-1: CRC32, little endian (recvol3.cpp:183-186).
  - CRC is computed over the first (Length - 4) bytes (CalcFileSum(..., Length-4), recvol3.cpp:188-189). So the 7-byte P-trailer is covered by the CRC; the CRC itself is excluded. VERIFIED.
  - Test() repeats the 4-byte CRC check with Length-4 (recvol3.cpp:520-535). VERIFIED.
  - After a restore that writes a new-style volume, the last 7 bytes are zeroed (recvol3.cpp:451-456). VERIFIED.
- OLD style (RAR 3.0 name#_#_#.rev): no trailer CRC; restore uses name parsing only. VERIFIED from recvol3.cpp:144-161.
- Encoding: RSCoder (GF(256)-style 256-symbol RS, one byte per symbol per file column; recvol3.cpp:33-44 RSEncode, RecBufferSize = TotalBufferSize/TotalFiles at recvol3.cpp:367). Per-byte column coding VERIFIED at recvol3.cpp:382-500.
- read_from: separate (the .rev file is a different file from the RAR volumes). The indexer must read the trailer (last 7 bytes) from the tail of the .rev file (last_article of .rev file).

### 3.2 RAR 5 .rev (recvol5.cpp)

- Signature: REV5_SIGN with size REV5_SIGN_SIZE (recvol.cpp:24-26, 94-95; recvol5.cpp:446-450). The byte value of REV5_SIGN is NOT in the sources read (not defined in rar.hpp or the downloaded headers). UNVERIFIED value; obtain it from the unrar headers file that defines REV5_SIGN (not located).
- Header layout (recvol5.cpp:444-505 ReadHeader, offsets relative to start of file):

| Offset | Size | Field | Notes | Status |
|---|---|---|---|---|
| 0 | REV5_SIGN_SIZE | signature | UNVERIFIED value | VERIFIED structure |
| SIGN | 4 | BlockCRC | ~CRC32 over the 4-byte HeaderSize field + header bytes (recvol5.cpp:455-467) | VERIFIED |
| SIGN+4 | 4 | HeaderSize | must be > 5 and <= 0x100000 | VERIFIED (recvol5.cpp:452, 457) |
| SIGN+8 | HeaderSize | header body | see below | VERIFIED |

Header body (recvol5.cpp:471-501):
- Get1 Version: must equal 1.
- Get2 DataCount: number of data (RAR) volumes.
- Get2 RecCount: number of recovery volumes; all rev files must agree (recvol5.cpp:478-481).
- Get2 RecNum: this rev file's index, must be < DataCount+RecCount and TotalCount <= 65535.
- Get4 RevCRC: CRC32 of this whole .rev file (recvol5.cpp:528 CalcFileSum(... INT64NDF, CALCFSUM_CURPOS) over the file). VERIFIED.
- If this is the first valid rev file: for I in 0..DataCount-1: Get8 FileSize, Get4 CRC of each data volume (recvol5.cpp:500-511). VERIFIED.
- RawRead integers: little-endian fixed width for Get2/Get4/Get8 (RAR convention). Endianness not stated explicitly in the sources read; UNVERIFIED.
- read_from: separate (.rev file). For the indexer: the header is in the first bytes of the .rev file; the RevCRC/RecCount fields give recovery counts without the rest of the file.

### 3.3 Recovery percent from rev volumes
- Recovery percent = RecCount / DataCount (RAR5 header fields); the RAR5 archive-side percent is the SUBDATA vint (section 1.4). Equality between them is UNVERIFIED.

---

## 4. ReScene SRS (pyReScene)

Attempts and results:
- https://raw.githubusercontent.com/srrDB/pyrescene/master/rescene/srs.py: client_error (404).
- https://raw.githubusercontent.com/srrDB/pyrescene/master/resample/srs.py: fetched. Contents are a CLI front end that imports SRS parsing from the `resample` and `rescene` packages, which are not in that file. No block types, struct formats, offsets, OSO (0x6B) or RAR (0x71) constants present.
- https://raw.githubusercontent.com/srrDB/pyrescene/master/usenet/srr_usenet.py: fetched. Only the RAR block-type range check (0x72..0x7B, `struct.unpack("<HBHH", ...)` for 7-byte block headers, arcread-like logic). No SRS/SRR block constants. It delegates SRR writing to the `rescene` package (rescene.rar).
- Repository root (https://github.com/srrDB/pyrescene, default branch master) top-level dirs: awescript, bin, dev-docs, images, rerar, resample, rescene, scripts, test_files, usenet. dev-docs is the likely place for format notes (not yet read). github.com tree pages are robots-disallowed for the fetch tool.
- Result: block types 0x69/0x6A/0x6B/0x71, the OSO hash block, the track CRC layout and the RAR volume header storage are all UNVERIFIED. Next step: fetch raw files under rescene/ and dev-docs/ once their exact names are known (candidate names unverified: rescene/rar.py, rescene/utility.py, dev-docs/*).

What the verified script (resample/srs.py) shows about SRS usage (VERIFIED from that file's logic):
- Per-track fields used after sample.load_srs(): track_number, signature_bytes, match_offset, data_length, track_file, check_bytes, match_length, codec, duration, fingerprint (music only).
- Signature / match-offset: a track with signature_bytes and match_offset == 0 aborts for non-MP3/STREAM samples; for MP3 and STREAM match_offset == -1 is the failure value (0 is a legal offset).
- Tracks without signature_bytes are dropped.
- Extraction succeeds when track_file is not None and track_file.tell() >= data_length.
- Rebuild CRC: rebuilt file crc32 must equal srs_data.crc32. On mismatch, find_best_educated_guesses generates candidate offsets and check_tracks tries combinations.
- RAR detection: only for .vob samples: track 1 signature_bytes.startswith(b"Rar!") marks a RAR volume sample.
- Limits: 2 GiB limit 0x80000000 unless -b. Exit codes 0-8 and 99.

Per-item status for the SRS request:
- Block types and fields (SRS header, SRR blocks): UNVERIFIED.
- Track CRC / signature bytes (match-offset technique): partly VERIFIED (usage in resample/srs.py as above); byte-level layout UNVERIFIED.
- OSO hash block (0x6B): UNVERIFIED (not present in fetched files).
- SRR RAR volume header block (0x71): UNVERIFIED (not present in fetched files). The indexer-relevant fact that RAR block headers are 7-byte blocks with types 0x72..0x7B is VERIFIED in usenet/srr_usenet.py.

---

## Summary of what is read_from-relevant for the indexer
- RAR5 locator (first_article) gives QO and RR offsets; QO (cached headers, raw data) and RR (SUBDATA percent in service block) can then be read without scanning the whole archive.
- RAR5 CMT is a service block: compressed or stored, CRC-checked, 16 MiB memory cap.
- RAR4 PROTECT fields are at fixed offsets from the block start (26-byte block); DataSize gives the recovery block to skip.
- .rev files (RAR3 and RAR5) are separate; both have fixed headers/trailers that can be read from the head (RAR5) or tail (RAR3 7-byte trailer + CRC32).

## Open items
1. REV5_SIGN byte value (recvol5.cpp uses it; defined outside the downloaded headers).
2. QO entry Flags bit meanings; QO block location (tail?) in practice.
3. RAR4 "Protect!" literal and recovery percent formula.
4. RAR4 NEWSUB "RR" subdata layout.
5. Full SRS / SRR block table (pyrescene rescene/ and dev-docs/).

## Sources
- https://raw.githubusercontent.com/pmachapman/unrar/master/arcread.cpp (ReadHeader50 service/file block fields ~808-870, 1005-1033 locator, 1236-1260 SUBDATA, 1491-1560 ReadSubData, ReadHeader15 PROTECT/CMT 455-473, RR 79-93 SearchRR, 913-919 RR percent)
- https://raw.githubusercontent.com/pmachapman/unrar/master/headers5.hpp (flags, MHEXTRA_LOCATOR 68-73, FHEXTRA 82-88)
- https://raw.githubusercontent.com/pmachapman/unrar/master/headers.hpp (RAR4 structs 322-336, SUBHEAD_TYPE_* 118-125, block type enums 82-90)
- https://raw.githubusercontent.com/pmachapman/unrar/master/qopen.cpp (QuickOpen Load/Read/ReadBuffer/ReadRaw/ReadNext 1-300)
- https://raw.githubusercontent.com/pmachapman/unrar/master/qopen.hpp (MaxBufSize 0x10000)
- https://raw.githubusercontent.com/pmachapman/unrar/master/arccmt.cpp (RAR4 comment 1-126)
- https://raw.githubusercontent.com/pmachapman/unrar/master/recvol5.cpp (ReadHeader 444-510, ProcessRS, Test 528)
- https://raw.githubusercontent.com/pmachapman/unrar/master/recvol3.cpp (IsNewStyleRev 65-78, trailer 137-190, 265-300, 440-456, Test 505-535)
- https://raw.githubusercontent.com/pmachapman/unrar/master/recvol.cpp (REV5 signature dispatch 24-26, 94-95)
- https://raw.githubusercontent.com/pmachapman/unrar/master/encname.cpp (fetched, not needed for this item)
- https://raw.githubusercontent.com/srrDB/pyrescene/master/resample/srs.py (fetched; CLI, no block constants)
- https://raw.githubusercontent.com/srrDB/pyrescene/master/usenet/srr_usenet.py (fetched; RAR block-type range 0x72..0x7B)
- https://github.com/srrDB/pyrescene (repo root, fetched for layout)
- Not reachable: https://raw.githubusercontent.com/srrDB/pyrescene/master/rescene/srs.py (404); github.com tree pages (robots disallowed); rarlab.com (blocked per task).
