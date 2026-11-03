# W8b: Inner-payload sniff catalog (Usenet indexer)

Scope: magic bytes, offsets, and first-64-KB metadata for inner payloads; nested archives; raw splits; Rust sniffing crates.
Tag convention: **VERIFIED (url)** = read from the cited page in this session. **UNVERIFIED** = from working knowledge, not confirmed by a fetched source; check before relying on it.
Fetch note: several raw.githubusercontent.com paths I guessed returned client_error (ietf-wg-cellar ebml_lite.xml, gpac mp4box box-parser.js, file/Magdir/iso9660, file/Magdir/flac, nwerner/rar-rs, infi-rs/infer README, Richard-Lu/file-format README). Those rows are UNVERIFIED.

Offsets: "abs" = absolute file offset. 2048-byte sectors unless stated.

## 1. Magic / sniff table

### 1a. Containers and video

| Payload | Offset | Bytes (hex) | Read in first 64 KB | Tag |
|---|---|---|---|---|
| Matroska / WebM | 0 | `1A 45 DF A3` (EBML header ID) | DocType element `42 82` inside EBML header, value `matroska` or `webm`. Then Segment `18 53 80 67`, Info `15 49 A9 66` (Duration `44 89`, TimestampScale `2A D7 B1`, Title `7B A9`, MuxingApp `4D 80`, WritingApp `57 41`), Tracks `16 54 AE 6B` > TrackEntry `AE` (CodecID `86`, Video `E0` with PixelWidth `B0` / PixelHeight `BA`, Audio `E1` with SamplingFrequency `B5` / Channels `9F`, Language `22 B5 9C`, Name `53 6E`). Chapters `10 43 A7 70`, Tags `12 54 C3 67`, Attachments `19 41 A4 69` (may sit past 64 KB). | VERIFIED (https://www.matroska.org/technical/elements.html) for element IDs; VERIFIED (https://raw.githubusercontent.com/file/file/master/magic/Magdir/matroska) for the 4-byte magic and DocType rule (`search/4096` for `42 82`, then `webm`/`matroska` string). |
| MP4 / MOV / M4V | 4 | `66 74 79 70` (`ftyp`); major brand at 8 (4 B), minor version at 12, compatible brands from 16 in 4-byte steps | Brands: `isom`, `iso2..6`, `mp41`, `mp42`, `M4V `, `M4A `, `qt  `, `dash`, `avc1`, `3gp*`. Then `moov` > `mvhd` (duration, timescale), `trak` > `tkhd` (dimensions), `hdlr`, `mdia` > `minf` > `stsd` (codec fourcc such as `avc1`, `hvc1`, `dvh1`, `dvhe`, `mp4a`, `ac-3`, `ec-3`), `udta` > `meta` > `ilst` (tags: `©nam`, `©too` encoder, `©day`), `dvcC`/`dvvC` (Dolby Vision config), `colr` (HDR signalling). Large `moov` at end of file = not readable from a prefix (faststart needed). | UNVERIFIED (ISO/IEC 14496-12 box layout from memory; fetch of gpac box-parser failed). |
| AVI | 0 | `52 49 46 46` (`RIFF`), size at 4, `41 56 49 20` (`AVI `) at 8 | `LIST` `hdrl` > `avih` (frame count, width, height, microseconds/frame gives fps). `strl` > `strh` / `strf` (codec fourcc, audio WAVEFORMATEX). `LIST` `INFO` for `ISFT` (software). | UNVERIFIED |
| MPEG-TS | 0 | `47` at 0, 188, 376... (sync every 188 bytes) | PID table (PAT/PMT), stream_type per PID (H.264 `0x1B`, HEVC `0x24`, AC-3 `0x81`, E-AC-3 `0x87`, DTS `0x82`/`0x8A`, TrueHD `0x83`), descriptors for language and Dolby. Checking 3-4 consecutive sync bytes gives confidence. | VERIFIED sync byte 0x47 and 188-byte packets (https://en.wikipedia.org/wiki/MPEG_transport_stream); stream_type values UNVERIFIED. |
| M2TS (BDAV) | 0 | `47` at 4, 196, 388... (192-byte packets, 4-byte timecode prefix before each TS packet) | Same as TS. Blu-ray `.m2ts` in `BDMV/STREAM/`. | VERIFIED 192-byte packet with 4-byte timecode (https://en.wikipedia.org/wiki/MPEG_transport_stream); sync offset 4 UNVERIFIED (derived from the prefix). |
| MPEG-PS / VOB | 0 | `00 00 01 BA` (pack start code) | Program stream map `00 00 01 BC`; stream IDs `E0..EF` video, `C0..DF` MPEG audio, `BD` private (AC-3/DTS/LPCM inside VOB). | UNVERIFIED |
| ISO 9660 | 0x8001 (sector 16 + 1) | `43 44 30 30 31` (`CD001`) | PVD (descriptor type `01` at 0x8000). Absolute = 0x8000 + field offset. System ID 0x08 (32 B), Volume ID 0x28 (32 B, strD), Volume Set ID 0xBE (128 B), Publisher 0x13E (128 B), Data Preparer 0x1BE (128 B), Application ID 0x23E (128 B), creation date 0x32D (17 B, dec-datetime, abs 0x832D). Root directory record at PVD offset 156. | VERIFIED (https://wiki.osdev.org/ISO_9660) for offsets and lengths. Type byte `01` for PVD UNVERIFIED. |
| UDF | 32 KiB+ (VRS, one per 2 KiB sector) | `BEA01` (`NSR02` or `NSR03`, `TEA01`) at sector 16 + 1 | Anchor Volume Descriptor Pointer (tag identifier 2) at sector 256 (abs 0x80000 for 2048-byte sectors), then Partition / Logical Volume Descriptor for volume id. | UNVERIFIED (ECMA-167 / ISO 13346 from memory; fetch of the Wikipedia page did not confirm sector numbers). |
| Blu-ray BDMV | file-level | `index.bdmv`: `INDX0200` / `INDX0300`; `MovieObject.bdmv`: `MOBJ0200`; `PLAYLIST/*.mpls`: `MPLS0200` / `MPLS0300`; `CLIPINF/*.clpi`: `HDMV0200` | Playlist and clip info (duration, PIDs, codec). Use with `BDMV/STREAM/*.m2ts`. | UNVERIFIED |
| DVD IFO | 0 | `44 56 44 56 49 44 45 4F 56 49 44 45 4F` family: `DVDVIDEO-VMG` or `DVDVIDEO-VTS` (12 B ASCII at 0) | VTS: video attributes, audio streams, subpicture count, PGC/title info. | UNVERIFIED |
| AV1 / HEVC / H.264 raw | 0 | Annex-B start codes `00 00 00 01` or `00 00 01` | Codec sniff only; no duration from raw. Often inside MKV/MP4 instead. | UNVERIFIED |
| DV RPU / HDR10+ SEI | inside the stream | Dolby Vision RPU is NAL-embedded (HEVC unspecified-type NAL with RPU header); HDR10+ is T.35 SEI | Needs demux, so only from MKV/MP4 sample data (the first access unit of the first cluster / mdat). Not readable from container headers alone. | UNVERIFIED |

### 1b. Audio

| Payload | Offset | Bytes (hex) | Read in first 64 KB | Tag |
|---|---|---|---|---|
| FLAC | 0 | `66 4C 61 43` (`fLaC`) | STREAMINFO: 4-byte block header (last-flag bit, type 0, length 34), min/max block size (16+16 bits), min/max frame size (24+24), sample rate (20 bits), channels-1 (3), bps-1 (5), total samples (36), MD5 (128). Then VORBIS_COMMENT (type 4) for tags, PICTURE (type 6) for covers. | Magic VERIFIED (https://en.wikipedia.org/wiki/FLAC); STREAMINFO bit widths UNVERIFIED (RFC 9639 / xiph.org not fetched). |
| MP3 | 0 | `49 44 33` (`ID3`) then major version (1 B), revision (1 B), flags (1 B), size (4 B syncsafe) | ID3v2 frames: `TIT2`, `TPE1`, `TALB`, `TCON`, `TENC` (encoder), `TSSE` (encoder settings), `TLEN` (length), `APIC` (cover), `COMM`. After tag: first MPEG frame sync `FF Ex`. Xing/Info header inside first frame (after side info): frame count, bytes, TOC; LAME tag 120 bytes after `Xing` gives encoder string, delay, padding. | UNVERIFIED |
| AAC ADTS | 0 | sync 12 bits `FF Fx` (`FFF` with layer bits 00) | Profile, sample rate index, channel config from header. | UNVERIFIED |
| AC-3 | 0 | `0B 77` | Sample rate and frmsizecod in BSI. | UNVERIFIED |
| DTS (core) | 0 | `7F FE 80 01` | Sample rate, channels, bit rate index. | UNVERIFIED |
| WAV | 0 | `52 49 46 46` ... `57 41 56 45` (`WAVE`) at 8 | `fmt ` chunk: format tag, channels, sample rate, byte rate, bits. `data` chunk size gives duration. | UNVERIFIED |

### 1c. Documents, images, misc

| Payload | Offset | Bytes (hex) | Read in first 64 KB | Tag |
|---|---|---|---|---|
| ZIP (generic) | 0 | `50 4B 03 04` (local file header) | Local header per entry: name at 30 (name length at 26, extra at 28). Central directory at end (needs tail). EPUB/CBZ/DOCX: first entry `mimetype` stored (no compression), so its data starts at 30 + 8 = 38 (name length 8 for `mimetype`). | Signature UNVERIFIED (from memory, ZIP APPNOTE). |
| EPUB | 0 | ZIP as above; `mimetype` first, content `application/epub+zip` | OPF metadata in META-INF/container.xml to *.opf. | UNVERIFIED |
| CBZ | 0 | ZIP | Image list. | UNVERIFIED |
| DOCX | 0 | ZIP; `[Content_Types].xml`, `docProps/core.xml` | Creator, title. | UNVERIFIED |
| PDF | 0 | `25 50 44 46 2D 31 2E` (`%PDF-1.`) | Info dict `/Producer`, `/Creator`, `/CreationDate`, `/Title`. Trailer at end (may be past 64 KB). XMP may be inline. | UNVERIFIED |
| CBR (rar of images) | 0 | RAR signature, then JPEG/PNG inside | See section 2. | UNVERIFIED |
| JPEG | 0 | `FF D8 FF` | EXIF APP1 (`FF E1` ... `45 78 69 66 00 00`), dimensions in SOF0 `FF C0`. | UNVERIFIED |
| PNG | 0 | `89 50 4E 47 0D 0A 1A 0A` | IHDR at 8 (width 4 B, height 4 B, bit depth, colour type), `tEXt`/`iTXt` chunks. | UNVERIFIED |
| NFO | none | No magic. Plain text, often CP437 (box-drawing bytes 0xB0-0xDF). | Heuristic: printable CP437 text, `=` / `-` banners, scene group tags. | UNVERIFIED |
| SRR / SRS | 0 | Not verified here | SRR is a scene RAR-like block container; not verified. | UNVERIFIED (name only) |
| Windows PE | 0 | `4D 5A` (`MZ`), `e_lfanew` at 0x3C (4 B) points to `50 45 00 00` (`PE\0\0`) | COFF header `TimeDateStamp` at PE + 8 (4 B, Unix time). Rich header between DOS stub and PE header (XOR key). Version resource strings. | UNVERIFIED |
| ELF | 0 | `7F 45 4C 46` | Class (byte 4), data (byte 5), machine (e_machine). | UNVERIFIED |
| APK | 0 | ZIP; `AndroidManifest.xml` (binary XML), `META-INF/` | Parse binary AXML for package name. | UNVERIFIED |
| DMG (Apple disk image) | end of file | `koly` in the last 512-byte trailer | UDIF trailer: data fork offset/length, XML plist offset. | UNVERIFIED |
| VHD | 0 (dynamic: header copy at 0), trailer at end-512 | `conectix` at 0 (footer copy at end), `cxsparse` in dynamic header | Disk size, geometry, type (fixed/dynamic/diff). | UNVERIFIED |

## 2. Nested archives

- **Outer archive in store mode** (RAR method `0x30`, 7z copy, ZIP method 0): inner file bytes are contiguous in the outer data area. Inner offset in the decoded stream = outer data offset (end of outer file header, plus any extra area) + inner offset. So a 700 KB prefix from the first article covers the inner head if the inner file starts within that prefix. UNVERIFIED (general format behaviour; RAR5 data area location from rarlab technote).
- **Compressed outer**: inner stream needs the decompressor, so only the first decoded bytes are available. Cost: decoder state from block start. Solid RAR (solid flag in archive header) makes later files depend on earlier ones.
- **Layers readable from a partial download**: store-mode RAR-in-RAR gives typically 2-3 layers within 700 KB if each header is small. Each layer costs one header parse plus the inner magic. Stop at the first compressed layer unless the indexer decodes it.
- **RAR inside RAR** (scene re-RAR): outer RAR5 file header names the inner `.rar`. Check the inner signature at outer data offset.
- **ZIP inside RAR** (scene "dist" zips with `.diz` and inner `.rar`): outer RAR (store) > inner `PK\x03\x04` at data offset > `FILE_ID.DIZ` entry (stored, 30 + name length). UNVERIFIED.
- **FILE_ID.DIZ in zips**: stored or deflated; name `FILE_ID.DIZ` in local header at 30. Deflate needs a decoder; store mode readable directly. UNVERIFIED.
- **ISO inside RAR**: ISO's `CD001` at outer data offset + 0x8001. Requires the whole ISO data to be in store mode.
- **7z inside RAR**: `37 7A BC AF 27 1C` at outer data offset. The 7z start header holds next-header offset/size; the header is at the end of the file, so a partial prefix often cannot reach it. UNVERIFIED.
- **7z layout**: signature 6 B, version 2 B, start header CRC 4 B, next header offset 8 B, size 8 B, CRC 4 B; header at 32 + offset. UNVERIFIED.
- **RAR4 vs RAR5 block headers**: RAR4 and RAR5 signatures VERIFIED via rarlab (https://www.rarlab.com/technote.htm). RAR4 block headers UNVERIFIED (CRC16, type byte, 16-bit flags, 16-bit size; file block type `0x74`).

## 3. Raw split volumes

- **HJSplit `.001/.002`**: raw byte slices. `.001` begins with the magic of the joined file, so sniff `.001` directly. UNVERIFIED.
- **7-Zip `-v` on a `.7z`**: produces `name.7z.001`, `.002`, ... The joined stream is a valid `.7z`, so `.001` starts with `37 7A BC AF 27 1C`. UNVERIFIED.
- **7-Zip `-v` on a raw file**: produces a 7z container or a split of the raw file depending on switches. Sniff the joined content, not the extension. UNVERIFIED.
- **Raw MKV split** (`.mkv.001`): `.001` starts with `1A 45 DF A3`; later volumes have no header. Joined length = sum of parts. Confirm with EBML parse on the joined stream. UNVERIFIED.
- **Obfuscated posts** (random extension): classify solely by magic from section 1. Ignore the name. The sniff order should be: container magics first (EBML, ftyp, RAR, 7z, ZIP, PE), then fallback to text heuristics (NFO).

## 4. Rust crates for sniffing

| Crate | Version (fetched) | Facts | Pros | Cons |
|---|---|---|---|---|
| `infer` | 0.22.0 (VERIFIED https://docs.rs/infer/latest/infer/) | `infer::get(&[u8])` and `get_from_path`. Category modules (archive, video, audio, book, doc, image, font, text, app, odf). MIT. Optional `cfb` dep. Custom matchers via `Infer::add`. | Small, prefix-based, fast; many formats; easy to extend. | Format coverage per module not confirmed on the page (MP4, MKV, ISO, FLAC, MP3 need checking in source). No deep metadata parse. |
| `file-format` | 0.29.0 (VERIFIED https://docs.rs/file-format/latest/file_format/) | `FileFormat::from_bytes`, `from_file`. Feature readers: `reader-ebml` (Matroska/WebM), `reader-mp4`, `reader-id3v2` (FLAC/MP3), `reader-zip` (DOCX/EPUB/JAR), `reader-exe` (PE/DLL), `reader-pdf`, `reader-cfb`, `reader-asf`, `reader-rm`, `reader-sqlite3`, `reader-xml`, `reader-txt`. Dual MIT/Apache-2.0. | Structural readers (not just magic) for several families; good for the nested classification step. | Feature-gated; reader depth per format not verified here; bigger dependency tree. |
| `tree_magic_mini` | 3.2.2 (VERIFIED https://docs.rs/tree_magic_mini/latest/tree_magic_mini/) | `from_u8`, `from_file`, `from_filepath`, `match_*`. Loads shared-mime-info DB at runtime (or `TREE_MAGIC_DIR`); `tree_magic_db` feature embeds it. ~150 ns per check. | MIME-tree based, broad type coverage. | Runtime DB dependency; embedded DB is GPL, license check needed; slower on first hit (5-100 µs per type lookup per docs). |
| Hand-written table | n/a | Exactly the table in section 1, ~40 rules, first-match by offset, plus a second pass for ZIP/RAR/7z contents. | No deps, exact control over offsets, reads only the prefix we fetch, handles nested layers. | Must maintain it; no structural parsing unless written. |

Recommendation (UNVERIFIED judgement): hand-written table for the outer and first-inner classification (it is the only path that handles prefixes and nested offsets cleanly), and `infer` as a cross-check for the long tail. Use `file-format` readers (`reader-ebml`, `reader-mp4`) only if a full structural parse is wanted and the prefix holds the headers.

## Sources

Fetched in this session (VERIFIED):
- https://www.matroska.org/technical/elements.html (EBML element IDs: Segment, Info, Duration, TimestampScale, Title, MuxingApp, WritingApp, Tracks, TrackEntry, CodecID, Video, PixelWidth, PixelHeight, Audio, SamplingFrequency, Channels, Language, Name, Tags, Chapters, Attachments)
- https://raw.githubusercontent.com/file/file/master/magic/Magdir/matroska (EBML magic `1a 45 df a3`, DocType `42 82`, `webm`/`matroska` strings)
- https://wiki.osdev.org/ISO_9660 (CD001 at 0x8001, PVD field offsets and lengths)
- https://www.rarlab.com/technote.htm (RAR4 `52 61 72 21 1A 07 00`, RAR5 `52 61 72 21 1A 07 01 00`, RAR5 header fields and block types 1-5)
- https://en.wikipedia.org/wiki/FLAC (magic `fLaC`; STREAMINFO bit widths not on the page)
- https://en.wikipedia.org/wiki/MPEG_transport_stream (0x47 sync, 188-byte packets, 192-byte M2TS with 4-byte timecode)
- https://docs.rs/infer/latest/infer/ (infer 0.22.0 API)
- https://docs.rs/file-format/latest/file_format/ (file-format 0.29.0 API and feature flags)
- https://docs.rs/tree_magic_mini/latest/tree_magic_mini/ (tree_magic_mini 3.2.2 API, DB and license caveats)
- https://en.wikipedia.org/wiki/ISO/IEC_13346 (fetched; UDF sector numbers and VRS identifiers not on the page)

Attempted, failed (client_error): raw.githubusercontent.com ietf-wg-cellar ebml_lite.xml; gpac/mp4box.js box-parser.js; file/file Magdir/iso9660; file/file Magdir/flac; nwerner/rar-rs README; infi-rs/infer README; Richard-Lu/file-format README; github.com/Matroska-Org/matroska-specification (fetched only the README; EBML header and DocType IDs not confirmed there).

Not fetched (UNVERIFIED from working knowledge): MP4 ftyp/box layout, AVI avih, ID3v2 / Xing / LAME, ADTS, AC-3, DTS, WAV, ZIP/EPUB/DOCX, PDF, JPEG, PNG, PE/Rich header, ELF, APK, DMG koly, VHD, MPEG-PS, BDMV/IFO magics, 7z header layout, RAR4 block header, UDF sector numbers, HJSplit and 7-Zip split behaviour.
