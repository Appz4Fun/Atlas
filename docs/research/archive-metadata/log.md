# Archive and packer metadata extraction research log

Started 2026-11-02 06:52Z. 10 waves x 3 Haiku 5.5 agents (high effort, 10 min cap), Opus main session merges and steers.
Goal: a YAML field catalog of every piece of metadata extractable from Usenet packers (RAR4/RAR5, 7z, zip, split volumes, PAR2, SFV), noting what is readable from the first article / partial download, and Rust-native crates or specs (no ffmpeg/external tools).
Overlap note: the payload-fingerprints thread covers which fields make twin/alt keys. This thread covers the exhaustive field inventory, byte layouts, partial-read feasibility and Rust implementation path.

## Wave 1 (06:52Z)
- A: RAR4 + RAR5 full header/field inventory (rarlab technote, unrar source)
- B: 7z format full inventory (7zFormat.txt, sevenz-rust)
- C: zip/zip64, split volumes (.001, .zip.001, .r00), PAR2 packet inventory, SFV, Rust crate landscape

### W1C result (zip, split, PAR2, SFV) -> raw/w1c-zip-par2-sfv.md
- ZIP: local header 30 B (first article); GP bit 3 = sizes/CRC in data descriptor (4 variants). Central dir (last article): version-made-by host OS + spec = packer fingerprint, ext attrs (unix mode when host=3), comments. Zip64 via locator 0x07064b50. 0x7075 Unicode path carries CRC of CP437 name. Method 99 = WinZip AES, 0x9901 extra holds real method + strength; salt+verifier at data start.
- PAR2: 64-byte header, packet MD5 covers bytes 32..end (verify per packet in partial data). FileDesc MD5-16k at off 32 matches article 1. Main packet M derived from length. Creator strings UNVERIFIED.
- PAR3 draft: PAR3\0PKT magic, BLAKE3, near-zero in the wild.
- SFV: name + spaces + 8 hex, ';' comments; generator headers UNVERIFIED. SRR/SRS layout UNVERIFIED.
- Crates: rc-zip (sans-io, best for partial), zip 9.x (AES, seek), async_zip, crc32fast, md-5. No trusted par2 crate -> in-house parser.
- Gaps: zip extra-field layouts from memory; creator strings; SRR.

### W1A result (RAR4/RAR5) -> raw/w1a-rar.md (99 fields)
- Byte maps: RAR4 data at block_start+HEAD_SIZE, PACK_SIZE low32 right after HEAD_SIZE (LONG_BLOCK). RAR5 data at start+4+len(vint)+hdr size; data-size vint only when flag 0x0002.
- RAR5 comp_info: dict 128K<<N bits 11-15, method bits 8-10 (0=store), version bits 1-6 (1 = RAR 7.0+). RAR4 method 0x30 = store, dict in LHD_WINDOWMASK 0xE0.
- BLAKE2sp (extra 0x02) on a first split part covers only that volume -> not a dedup key there.
- Unix owner extra 0x06 / RAR4 UOW leak uploader account name + uid/gid.
- RAR5 main flag 0x0002 (volume number) absent only in vol 1 -> part1 detection without filename. ENDARC next-volume flag from last article.
- RAR5 crypt header 0x04 has 12-byte check value; RAR4 file encryption has none. TWEAKED checksums (0x0002) make CRC key-dependent.
- Store mode gives raw inner bytes at data offset; compressed needs decompressor, no verified pure-Rust RAR decompressor. Solid RAR4: later files unreachable from first article.
- Gaps: RR block layout, RAR4 Unicode name decode, EXTTIME math, RR% formula, rar-stream crate. rarlab.com blocked for curl, WebFetch works.

### W1B result (7z) -> raw/w1b-7z.md
- 32-byte signature header at offset 0 of .001 (first article); StartHeaderCRC covers 12-31. Header abs pos = 32 + NextHeaderOffset (common bug).
- Real header at END -> last volume's last article; usually kEncodedHeader (LZMA), small enough for last 1-2 articles.
- -mhe=on hides file list, but encoded header StreamsInfo is plaintext: AES NumCyclesPower, salt, IV, header packed/unpacked size + CRC.
- AES props: byte0 bits0-5 NumCyclesPower (default 19), bit6 IV, bit7 salt; byte1 sizes as nibbles.
- Copy (0x00) puts file bytes at 32+PackPos -> inner container headers in first article. Solid LZMA decodes as prefix only.
- Attributes 0x8000 = Unix extension, high 16 = st_mode (p7zip/Linux leak). FILETIME fixed 8 bytes (py7zr doc wrong).
- 7zIn recovery scans last 512 B for header if start header zeroed; signature at non-zero offset = SFX/prefixed.
- Method IDs verified (ARM64 0x0A, RISCV 0x0B, BCJ 03030103, PPMd 030401, BCJ2 0303011B). LZMA props 0x5D default; others = settings fingerprint.
- Crates: sevenz-rust2 pure Rust, header-only API UNVERIFIED -> hand-written header parser + LZMA crate.

### Wave 1 merge (06:58Z)
All three formats inventoried (~99 RAR fields, full 7z and zip/PAR2 tables). Key structural fact: RAR and zip local headers are first-article; 7z file table and zip central dir are last-article; PAR2 is separate small files.
Biggest gaps: (1) pure-Rust decompressors (RAR, LZMA/LZMA2, PPMd, deflate) and prefix decode, decides whether inner media is reachable from non-store posts; (2) packer creator strings and per-version defaults (WinRAR, 7-Zip, par2cmdline/MultiPar/ParPar/QuickPar) from source; (3) unverified layouts: zip extra fields, RAR RR, RAR4 Unicode names, EXTTIME, SRR/SRS.
Decision: LEAN IN. Wave 2 = A decompressors + partial decode, B packer version/default fingerprints from source, C verify the unverified layouts.
Peer asks: container thread wants per-volume data-start offsets (sent); stream thread wants how much inner file vol 1 exposes (sent).

## Wave 2 (06:59Z): A decompressors, B packer fingerprints, C verify layouts
### W2A result (decompressors) -> raw/w2a-decompressors.md
- RAR compressed: `rars` 0.12.3 (Apache-2.0, pure Rust, rar29+PPMd+rar50+VM filters) is the only pure option; young. `rar` 0.4 = store only. unrar/unrar-ng = FFI C++ (license OK for decode, bans compressor).
- 7z: sevenz-rust2 0.24 pure (LZMA/LZMA2/PPMd/BZip2/AES/BCJ*/BCJ2/Delta). lzma-rust2 0.25 active; lzma-rs stale. ppmd-rust (PPMd7 only). flate2/miniz_oxide, deflate64, bzip2, ruzstd. RustCrypto aes/sha2/pbkdf2.
- Prefix decode untested everywhere; LZMA/LZMA2/Deflate/CBC expected to stream; BZip2 block-granular; BCJ2 needs 4 streams. Needs a fixture test on truncated archives.
- 7z partial = header at end: decode the packed stream from offset 32 using coder props guessed (LZMA2 default) or from last article.
- Recommendation: rars (pinned + fixtures), sevenz-rust2/lzma-rust2, fallback unrar FFI. No pure RAR crate meets the "widespread" bar.

### W2B result (packer fingerprints) -> raw/w2b-packer-fingerprints.md (partial, agent ended without summary)
- par2cmdline creator: "Created by " PACKAGE " version " VERSION "." (creatorpacket.cpp:41); one Creator copy per recovery volume; defaults 2000 blocks, 5%.
- ParPar creator: "ParPar v<ver> <arch> [https://github.com/animetosho/parpar]" (parpar.js:336) -> leaks CPU arch. UniFileN only when name non-ASCII; slice multiple of 4.
- ngPost: external rar/7z + par2 binaries; defaults rar_size 0, par2 0%, name len 17, password len 13. Exact command line UNVERIFIED.
- 7-Zip 26.05 LzmaEnc dict defaults: level<=4 1<<(2L+16), 5-8 1<<(L+20).
- Blocked: rarlab.com and 7-zip.org (proxy 403 even for WebFetch this time), MultiPar paths 404. RAR per-version defaults all UNVERIFIED -> retry via mirrors (e.g. archive.org, rarlab whatsnew in unrar source tarball, Debian changelogs).

### W2C result (verified layouts) -> raw/w2c-verified-layouts.md
VERIFIED: zip 0x5455/0x7875/0x000a/0x7075/0x5855/0x000d/0x9901 (AE-2 => CRC 0), RAR4 FHD_UNICODE name decode (2-bit codes), RAR4 EXTTIME (4 bits per time), SRR blocks 0x69-0x6C, 0x71. Still UNVERIFIED: 0x6375, RAR4/RAR5 RR content + % formula, EXTTIME NS unit, SRS.

### Atlas baseline (local checkout ad111a0, src/archive/*, src/par2.rs)
Atlas parses: format identify, RAR4/RAR5/7z/zip listings -> InnerFile {name, size} only, encrypted, headers_encrypted, encryption kind, password check (RAR5 kdf+salt+check, zip), complete/damaged/need_more_tail. 7z start header (offset,size,crc). PAR2: set_id, file_id, md5/16k, size, name, block_size. Crates: flate2, lzma-rs 0.3 (stale), md-5.
NOT parsed: CRC32/BLAKE2, mtime/ctime, host OS, attributes, method/version, dict size, solid, RR, comments, volume number/first-vol flags, Unix owner, 7z coders/props, zip version-made-by/extras, PAR2 creator/IFSC, SFV.

### Wave 2 merge (07:02Z)
Decompressors: `rars` only pure-Rust RAR decoder; sevenz-rust2/lzma-rust2 for 7z. Prefix decode untested -> test it empirically (cargo 1.97 available in container). Fingerprints half done (rarlab/7-zip.org blocked by proxy). Layouts mostly verified.
Decision: LEAN IN, empirical. Wave 3 = A build fixtures + Rust test of prefix decode (flate2, lzma-rust2, sevenz-rust2, rars) on truncated archives, B finish packer table via mirrors (unrar GitHub mirrors, web.archive.org rarnew, MultiPar, par2cmdline-turbo, ngPost source), C RR/QO/service headers/SRS from unrar mirror + pyrescene.

## Wave 3 (07:03Z): A empirical prefix decode, B packer table via mirrors, C RR/QO/service/SRS
### W3C result -> raw/w3c-rr-qo-srs.md
- RAR5 service headers CMT, QO, ACL, STM, UOW, AV, RR, EA2 (unrar arcread/headers5). QO = raw area of cached full file headers (CRC32, size vint, flags, backward offset, header) -> list files from the TAIL of a RAR5 archive. Locator extra 0x01: flags 0x01 QO / 0x02 RR, vint offsets from main header (0 = absent).
- RR percent = FHEXTRA_SUBDATA vint on the RR service header (1 byte before RAR 6.10).
- RAR4 PROTECT 0x78 26 B: DataSize, Version, RecSectors, TotalBlocks, Mark. RAR4 CMT 0x75: UnpSize, UnpVer, Method (0x30 stored), CommCRC16.
- .rev: RAR3 new-style 7-byte trailer (ordinals + CRC32); RAR5 .rev header with DataCount/RecCount/RecNum/RevCRC and per-data-volume FileSize+CRC in first rev -> whole-volume CRCs of every .rar without downloading them.
- SRS/SRR remaining layout UNVERIFIED (pyrescene paths 404). unrar mirror copies in /home/claude/scratchpad/w3c/unrar/.

### W3A result (empirical prefix decode) -> raw/w3a-prefix-decode.md, probe in scratchpad/w3a/prefixdec
- Deflate (flate2 1.1.10): 10% input -> 842 KB correct prefix; 50% -> 4.2 MB. ~1x output per compressed byte on video-like data.
- LZMA1/LZMA2 (lzma-rust2 0.25): same ~1x, LZMA emits in 256 KiB steps. All correct prefixes.
- RAR (rars 0.12.3, RAR1.54/RAR3 PPMd/RAR5 fixtures): 0 bytes at 10/25/50%, "input is too short" -> parse gate checks full file, not a codec limit. Next: zero-pad to full length or call the unpacker directly.
- Implication: compressed zip/7z (LZMA/LZMA2/deflate, non-solid or first file of solid) give the inner MKV front from the first articles. RAR depends on bypassing rars' parse gate or a thin wrapper.

### W3B result (packer fingerprints via mirrors) -> raw/w3b-packer-fingerprints.md
- ngPost VERIFIED: rar a -idp -ep1 -m0 [-v<N>m -hp<pass>] (+ -x.DS_Store on macOS); 7z -mx0 -mhe=on; no split by default; archive name = NZB name (or 17-char random with gen_name). => ngPost RAR posts are store-mode (inner MKV readable) and -hp hides headers.
- unrar 7.30b1: VER_PACK 29 / VER_PACK5 50 / VER_PACK7 70.
- 7-Zip: header compression ON by default, header encryption only with password; LZMA2 default, Copy at -mx0; header dict 1<<20.
- par2cmdline creator verified format; version values conflict (0.8.1 vs turbo 1.3.0). QuickPar "QuickPar 0.9" only as test literal.
- CONFLICT: W3B says no separate PAR2 Unicode packet; W2B verified ParPar writes "PAR 2.0\0UniFileN" (spec optional packet). Trust W2B.
- Still blocked: rarlab + web.archive.org; RAR dict defaults and 7-Zip -mx table UNVERIFIED.

### Wave 3 merge (07:08Z)
Biggest result: deflate and LZMA/LZMA2 prefix-decode ~1:1, so compressed zip/7z still expose the inner MKV front from the first articles. RAR blocked by rars' whole-file parse gate. ngPost (popular) RARs are -m0 store by default. RAR5 QO + .rev headers give tail-side listings and per-volume CRCs.
Decision: LEAN IN. Wave 4 = A bypass rars gate + real .7z partial (start header + LZMA2 folder at offset 32) with EBML check; B verify add order / solid sort / QO scope / 7z ordering from unrar + 7-Zip source; C prior art: NZBGet DirectRename/RarReader + SABnzbd header parsing from first articles, real-world packer prevalence.

## Wave 4 (07:09Z): A RAR/7z partial decode, B ordering/QO, C prior art
### W4A result -> raw/w4a-rar-7z-partial.md, probe scratchpad/w4a/probe
- 7z WITHOUT end header: feed bytes from offset 32 to lzma-rust2 Lzma2Reader(64 MiB dict): 10% cut -> 786 KB correct, starts 1A45DFA3. LZMA2 first chunk (0xE0) carries lc/lp/pb in-stream, only dict missing (bigger is safe). LZMA1 with default 0x5D props also works (wrong props -> garbage, caught by magic check). => 7z first-article inner MKV read is SOLVED.
- RAR zero-padding: fails for PPMd, RAR1.5, RAR5; partial for RAR3 LZ. Direct decoder entry points exist in rars: codec::rar50::Unpack50Decoder::decode_member_from_reader_with_dictionary_to_sink (rar50.rs:518), codec::rar29::Unpack29::decode_member_from_reader (rar29.rs:162). Not yet tested -> wave 5.

### W4C result (prior art) -> raw/w4c-prior-art.md
- NZBGet/SABnzbd do NOT parse archive headers from first articles; they need whole volumes. Only partial logic = PAR2 hash16k (first 16 KiB) and magic-byte checks. => Atlas first-article archive parsing has no prior art in clients; it's novel.
- NZBGet RarReader: RAR3 volume number only from ENDARC; RAR5 VOLNR in main (absent = vol 0). Possible bug HIGH_UNP_SIZE added to pack size.
- SAB: IFSC slice CRCs -> whole-file CRC via sabctools combine; RAR5 password check via XENC check value; NEWNUMBERING flag -> partNN naming from first article; is_cloaked heuristic.
- rarinfo PHP (dariusii) and NNTmux ArchiveInfo not reached (404s) -> retry wave 5 with correct paths.

### W4B result (ordering/QO) -> raw/w4b-ordering-qo.md (agent ended without summary)
- RAR volumes: a volume holding data of a file holds a header for it (SPLITBEFORE/AFTER continuation). Headers for files without data in that volume: UNVERIFIED (archiver side).
- RAR5 QO is PER VOLUME: offsets relative to its own file, caches only that volume's headers, normally at end of volume, default QOPEN_AUTO on reader. Transparent header cache, not a global file list. (Corrects my earlier note to the container thread.)
- -ds = DisableSortSolid exists; actual rar add order/sort is archiver-side, UNVERIFIED.
- 7-Zip: type sorting (-mqs) is OFF by default for the 7z handler (7zHandlerOut.cpp:872) despite folklore. Header file order: dirs, empty files, then data files in add order. Solid block 1 = first N files.
- ZIP: add order, central dir at end; 7-Zip zip update does no sort.

### Wave 4 merge (07:13Z)
7z partial decode SOLVED (LZMA2 from offset 32 with 64 MiB dict, MKV magic check). RAR needs the direct codec entry points in rars. Clients (NZBGet/SAB) never parse first-article archive headers -> no prior art; Atlas is ahead. QO per volume only.
Decision: LEAN IN + PIVOT. Wave 5 = A rars direct codec on truncated packed data (RAR5, RAR3 LZ, PPMd); B rarinfo PHP + NNTmux ArchiveInfo + SRR/SRS tables (gh api search/code to find paths); C pivot: identity/timezone/path LEAKS in archive metadata (NTFS STM Zone.Identifier HostUrl, ACL SIDs, symlink targets, comments, RAR4 local DOS time vs RAR5 UTC vs Usenet Date -> uploader TZ offset, zip extra times, 7z attrib/st_mode, PAR2 comment packets).

## Wave 5 (07:14Z): A rars direct, B rarinfo/SRR/SRS, C leaks
### W5A result -> raw/w5a-rars-direct.md (agent ended without summary), probe scratchpad/w4a/probe/src/bin/direct.rs
- RAR5 direct codec (Unpack50Decoder::decode_member_from_reader_with_dictionary_to_sink) on cut packed data: 8 MiB LZ member, 10% packed -> 786 KB correct; 25% -> 2.1 MB; 50% -> 4.2 MB. ~1:1. => RAR5 compressed partial decode WORKS.
- Losses: StreamingOutput flushes at 64 KiB; read_compressed_block uses read_exact per block -> partial last block lost. Tiny member (9.7 KB) gives 0 below 100%.
- RAR 2.0 (rar20) eager read_to_end -> 0 on truncation (needs patch). RAR 2.9 LZ streams in 1 MiB STREAM_CHUNK (untested); PPMd not run.
- Params needed: alg version, dict, solid, unpacked size from the file header (first article) -> all available.

### W5B result (rarinfo PHP, SRR/SRS) -> raw/w5b-rarinfo-srr-srs.md, sources in scratchpad/w5b/
- zeebinz/rarinfo (PHP, used by nZEDb/newznab forks): 1 MiB max read; RAR4 full file fields; RAR5 vint parse incl. QO/hash/htime/redirect/uowner extras, no header CRC check; 7z reads start header then seeks to end header, "fragment mode" scans for 0x01 0x04 / 0x17 0x06 header markers; encoded headers need external 7za. ZIP local-only in first window. PAR2 FileDesc + Main. => indexer prior art EXISTS (PHP) for first-article parsing; Atlas Rust parsers are the analogue.
- SRR VERIFIED (srr_spec.txt): 0x69 header(app name), 0x6A stored file, 0x71 RAR file name, 0x6B OSO hash (size, hash, name), 0x6C padding.
- SRS VERIFIED (pyrescene resample/main.py): MKV EBML 0x6A75 file / 0x6B75 track; AVI/MP4 "SRSF"/"SRST"; FLAC block 0x73; FileData flags, app, sample name, size u64, CRC32; TrackData track no, data length, match_offset u64, signature bytes.
- NNTmux DB usage UNVERIFIED (gh api 403 on repos; code search works; raw works).

### W5C result (leaks) -> raw/w5c-leaks.md
- TZ: RAR4 FTIME + zip DOS = local; RAR5 mtime, zip 0x5455/0x000a, 7z FILETIME = UTC. Zip with DOS + UT/NTFS in the local header -> uploader UTC offset from article 1 (2 s precision, DST ambiguity). RAR4: offset vs Usenet Date header.
- Sub-second precision (RAR4 EXTTIME, RAR5 HTIME ns only with Unix flag, zip NTFS 100 ns) = OS fingerprint.
- MOTW: RAR STM service "Zone.Identifier" (ZoneId, ReferrerUrl, HostUrl) when packed with -os -> source download URL. Default UNVERIFIED.
- Owner: RAR5 UOWNER 0x06 (user/group names, uid/gid), RAR4 UOW, RAR5 ACL (owner SID, layout UNVERIFIED), zip 0x7875/0x5855/0x000d uid/gid; 7z only st_mode.
- Paths: RAR5 REDIR 0x05 symlink targets; names without -ep1 keep dir prefixes.
- Comments: RAR CMT, zip EOCD/entry comments (last article), PAR2 CommASCI/CommUni.
- Caution: libarchive zip local p[5] is version-needed high byte, not host OS. Archive bit 0x20 is weak.

### Wave 5 merge (07:19Z)
Partial decode now works for RAR5 (rars direct codec, ~1:1, 64 KiB flush loss), 7z LZMA/LZMA2, zip Deflate. Gaps: RAR 2.0 eager, RAR 2.9/PPMd untested. PHP rarinfo is the indexer prior art (1 MiB window). SRR/SRS layouts verified. Leak catalog done.
Decision: LEAN IN to implementation proof. Wave 6 = prototype pure-Rust extractors that emit every catalog field from partial buffers, tested on fixtures: A RAR4+RAR5, B 7z (start + tail encoded header via lzma-rust2) + zip, C PAR2 (+IFSC CRC combine) + SFV + SRR.

## Wave 6 (07:20Z): prototypes
### W6C result -> proto/par2_sfv_srr_meta.rs (697 LOC), raw/w6c-par2-proto.md
- Pure Rust (md-5, crc32fast); never panics at any prefix length. PAR2 packets validated by MD5, all packet types parsed, grouped by RSID.
- Whole-file CRC32 from IFSC slice CRCs with crc32fast combine + exact zero-unpad algebra: 0x5cb70ba3 == zlib (3 MB, 9 slices). Bit-serial unpad is O(8*slice); needs GF(2) matrix exponentiation for 100 MB slices.
- SFV + SRR parsers. Gaps: no real par2cmdline/ParPar/MultiPar samples (apt par2 unavailable), no real .srr.

### W6A result -> proto/rar_meta.rs (985 lines, crc32fast only), raw/w6a-rar-proto.md (agent ended without summary)
- Ran on 167 RAR files (162 rars fixtures) at full, 1 KB, 50%: no panics; truncation reported as truncated_at + reason.
- Works: RAR4/RAR5 main flags, locator QO/RR, RAR5 crypt header (kdf, salt, check), RAR4 -hp detection, every file field incl. Unicode names, EXTTIME (raw remainder), RAR5 extras 0x01-0x07, service headers with RR %, RAR4 RR_PROTECT estimate, stored CMT text.
- Not done: cross-check against rars parser; EXTTIME ns scaling unverified.

### W6B result -> proto/sevenz_zip_meta.rs (1,327 lines, crc32fast + lzma-rust2), raw/w6b-7z-zip-proto.md
- 7z: signature header, encoded header decoded + CRC-checked, folders/coders/substreams/files/times/attrs; AES header -> header_encrypted=true; prefix probe on folder 0 gives 1A45DFA3 Matroska for LZMA/LZMA2.
- ZIP: local headers + all extras from head, EOCD/zip64/central dir from tail, UTC offset candidate from DOS vs 0x5455.
- Limits: multi-coder chains (BCJ) not applied, solid beyond file 1, zip data-descriptor walk stops, no zip64/AES/split-7z fixtures.

### Wave 6 merge (07:24Z)
Three working prototypes (~3,000 LOC total) in proto/. All fixtures self-made or from rars/libarchive test sets; none are from real Usenet posts or Windows tools.
Decision: PIVOT to real-world validation. Wave 7 = A run prototypes over libarchive/7-Zip/other real-tool fixtures (.uu) and tabulate packer fingerprints by origin tool; B obfuscated/encrypted posts: what survives -hp/-mhe, set membership of encrypted volumes, password-check feasibility per format; C remaining decode gaps: RAR 2.9 LZ + PPMd partial, rar20 patch, 7z BCJ/Delta chains.

## Wave 7 (07:25Z): A real-tool fixtures, B encrypted posts, C RAR2.9/PPMd + 7z filters
### W7B result (encrypted) -> raw/w7b-encrypted.md
- RAR5 -hp: only HEAD_CRYPT plaintext (Lg2Count, salt 16, PswCheck 8, csum 4 = SHA-256(PswCheck)[0..4]). Later headers IV+AES with size/CRC inside ciphertext -> header count/lengths NOT recoverable. Salt shared across volumes UNVERIFIED (needs fixture).
- RAR5 check: PBKDF2-HMAC-SHA256(UTF-8 pw, salt, 2^Lg2Count + 32) XOR-folded to 8 B == PswCheck. Definitive, ~3-15 ms/guess. (Atlas already implements this: src/archive/rar.rs PasswordCheck.)
- RAR4 -hp: 8-byte salt before EACH block; SHA-1 x 262144 KDF; no check value -> decrypt next block, verify CRC.
- 7z -mhe: 7-Zip encoder writes NO salt by default; 16-byte random IV in coder props, NumCyclesPower 19 -> IV is the per-archive twin key (corrects W1B "salt"). Test = SHA-256 x 2^19 then decrypt + header CRC.
- ZipCrypto 1-byte check (1/256 FP), WinZip AES 2-byte verifier PBKDF2-SHA1 x1000.
- No-password leftovers: signatures, KDF params, 7z start header + pack sizes, volume sizes/article counts, PAR2 FileDesc of volumes. ZIP names stay clear.

### W7A result (real fixtures) -> raw/w7a-real-fixtures.md (agent ended without summary)
- 235 libarchive test files (+rars) run through the prototypes: 0 panics, 0 non-zero exits. Many are fuzz files; origin inferred from headers.
- Inferred fingerprints: RAR4 host unix + EXTTIME = Linux rar 3/4; RAR4 win32 DOS-only time = WinRAR 3.x; RAR5 host windows + HTIME + Win32 attr = WinRAR 5; RAR5 host unix + owner user/group = Linux rar 5; RAR5 multivolume has QO on every part (consistent with per-volume QO). ZIP host 3 + 0x5455/0x7875 = Info-ZIP; spec 6.3 + NTFS 0x000a + zipx methods = WinZip/7-Zip; AE-2 strength 256 = WinZip AES. 7z sig 0.3 vs 0.4 (0.4 = newer 7-Zip with ARM64/RISCV filters) vs 0.2 old.
- No creator-version field in 7z; RAR5 "version" is algorithm version only (0 almost always).
- Prototype bugs: zip host table wrong (11=MVS, 14=VFAT); no SFX scan in 7z/zip; zip garbage not rejected (needs signature + sanity checks); RAR5 hp consumers must check encryption_header record.

### W7C result (RAR 2.9 / PPMd) -> raw/w7c-rar29-filters.md, probe w4a/probe/src/bin/rar29.rs
- rars can WRITE RAR 2.9 LZ/PPMd. 8 MiB LZ: cut 10/25/50% -> 1/3/5 MiB correct prefix. PPMd 8 MiB text: 0/2/4 MiB. Output in 1 MiB steps (rar29.rs:41); input read_to_end (rar29.rs:428) = memory cost only.
- Real small RAR 2.9 fixtures (<1 MiB) give 0 below 100% because of 1 MiB granularity.
- RAR 2.0 eager (patch not built). 7z BCJ/Delta chain test not run.

### Wave 7 merge (07:30Z)
Partial decode coverage now: zip Deflate, 7z LZMA/LZMA2, RAR5 (64 KiB granularity), RAR 2.9 LZ and PPMd (1 MiB granularity). Missing: RAR 2.0 (old, rare), 7z filter chains. Encryption: what survives -hp/-mhe is documented; 7z IV (no salt) is the per-archive key; RAR5 HEAD_CRYPT salt sharing across volumes unverified. Real fixtures: 235 files, 0 panics, fingerprint table by inferred tool, 3 prototype bugs.
Decision: LEAN IN to close gaps. Wave 8 = A 7z BCJ/Delta/ARM64 prefix chains + RAR5 -hp multivolume salt test (rars writer) + rar20 patch; B inner payload sniffing + nested archives + raw .001 splits; C sweep all UNVERIFIED items across raw/ and resolve the top ones from source.

### Output format note (07:28Z)
The owner shared the "Linking Signals Map" (https://claude.ai/artifact/BEGjW1bU8S6Yqm3cnPMNpE) as the example format: each signal tagged by what it proves (twin / alt / same film / same uploader / posting tool), download cost tier (free overview -> first article -> last article -> full), and Atlas status (collected / parsed-not-stored / no). Final catalog must carry those three tags per field, plus a packer-tool fingerprint table.
Update 07:28Z: the owner's "better example" is the doc "Media metadata for releases: ideas" (ranked table: Rank | Idea | Value | Effort | Extra bandwidth | CPU; then field tables Field | Where | What it gives | Use; a "fetching just enough" step list; "what this cannot do"). Final report should follow that shape (Claude Doc), with the YAML catalog as an attached file. Note that doc assumes compressed RAR = header only; our waves 4-7 show RAR5/RAR2.9/7z/zip prefix decode works.

## Wave 8 (07:31Z): A filters/hp/rar20, B inner sniff, C resolve UNVERIFIED
### W8B result -> raw/w8b-inner-sniff.md
- Sniff table: EBML + DocType, ISO9660 PVD offsets (0x8000+), RAR4/RAR5 signatures, FLAC, TS/M2TS verified; MP4/AVI/ID3/PE/BDMV/IFO rows UNVERIFIED (overlap with container thread, which covers these).
- Nesting: store outer -> inner offset = outer data offset + inner offset; compressed/solid stops at first decoder layer; 7z-in-RAR needs end header. Raw .001 splits start with the joined file's magic -> classify by magic, never extension.
- Crates: infer 0.22 (prefix), file-format 0.29 (structural readers behind features), tree_magic_mini (GPL DB) -> hand table + infer cross-check.

### W8C result (resolved) -> raw/w8c-resolved.md
RESOLVED: RAR4 EXTTIME remainder = 24-bit LE x100 ns; RAR4 PROTECT layout; REV5_SIGN "Rar!\x1aRev"; RAR5 ACL = Windows SECURITY_DESCRIPTOR (compressed), STM name UTF-8 in SubData, RR % vint in SubData; zip 0x6375 (ver, CRC32, UTF-8); 0x9901 actual method is u16 (libarchive reads 1 byte = bug); RAR5 HTIME/file flag bits; 7z kComment 0x16 skipped, kDummy 0x19 padding; 7z AES password UTF-16LE; host OS tables RAR4 0-5, RAR5 0/1.
STILL OPEN: RAR4 RR percent formula + "Protect!" mark; rar29 SHA-1 transform variant; APPNOTE text (403).

### W8A result -> raw/w8a-filters-hp-rar20.md
- 7z BCJ x86 and Delta chains: lzma-rust2 0.25 exports filter::bcj::BcjReader / filter::delta::DeltaReader; Lzma2Reader -> filter gives correct prefix 786 KB at 10%, ~4 MB at 50%. Filters turn EOF clean, so detect truncation at the LZMA2 stage. 7-Zip analysis only picks BCJ for exe-like extensions, so MKV/MP4/ISO normally get plain LZMA2.
- RAR5 -hp multivolume (rars writer): HEAD_CRYPT byte-identical across all 7 volumes (salt + check) -> set key for encrypted volumes, even with random names. File salts per member, repeated in each volume carrying it. Not yet confirmed on WinRAR output.
- rar20 patch not attempted.

### Wave 8 merge (07:36Z)
Partial-decode matrix complete except RAR 2.0 (legacy). Encrypted RAR5 sets are groupable by HEAD_CRYPT salt (rars writer; WinRAR UNVERIFIED). Inner sniff + nesting rules done. 8/9 priority UNVERIFIED items resolved.
Decision: Wave 9 = integration and linking. A: map every catalog field onto Atlas's archive code (staged src/archive + par2.rs + schema) with storage proposal; B: draft the linking-signal table (proves twin/alt/same film/uploader/tool x cost tier x Atlas status) in the format of the owner's example docs; C: draft the YAML field catalog from raw/ files. Wave 10 = verification of the YAML + final doc.
Atlas baseline correction (07:38Z, local checkout ad111a0): src/probe/archive.rs ArchiveHead already parses the first inner file's format, encrypted, headers_encrypted, method, name, unpacked, packed, crc32, stored_at (store data offset), continues; ProbeInfo keeps sfv map, moviehash, nfo; release_probe(crc32, agent, info JSON, moviehash). archive_release(password, encryption, names_hidden, password_check). archive_inner_files(name,size,source). Atlas sources staged at /mnt/user-data/uploads/git.xbmc4lyfe/Atlas-to-Rust/src/.

## Wave 9 (07:40Z): A Atlas gaps, B signal table, C YAML catalog
### W9B result -> raw/w9b-signal-table.md
69-row signal table (proves x cost x strength), top-12 ranked list, packer fingerprint table. Caveats carried: RAR5 shared salt rars-only, RAR/7-Zip default dict tables unverified, tool mappings inferred.
### W9C result -> archive-field-catalog.yaml
259 fields (rar5 56, rar4 44, 7z 52, zip 40, par2 27, sfv 5, srr 6, srs 12, rev 8, split_raw 9); 226 VERIFIED / 33 UNVERIFIED; parses with PyYAML. Each field: where, type, read_from, use, proves, leak, status, source; per format detect, partial_decode, rust. No Atlas status yet (wave 10).
### W9A result -> raw/w9a-atlas-gaps.md (against local checkout ad111a0; main is a29ee52, re-check before PRs)
- Per-file CRC32 dropped in every format (rar.rs:265-267, zip.rs:298-333, sevenz.rs:299-304); timestamps, host OS, attrs never kept; PAR2 Creator unparsed (par2.rs:93-124) and md5/md5_16k dropped (steps.rs:72-76); SFV ';' comments skipped; no SRR parser; IFSC unparsed.
- Claimed bugs (verify): RAR4 0xFFFFFFFF unknown size stored as 4 GiB (probe/archive.rs:75, rar.rs:109); probe RAR4 Unicode names ASCII fallback; stale encryption columns (steps.rs:141-152); probe zip encrypted flag misses 0x0040/0x2000; zip names Latin-1 not CP437; 7z encoded header LZMA only, LZMA2 headers -> damaged (sevenz.rs:513-516); SFV take(3) starvation; view cap 256 rows vs 4096 stored.

### Wave 9 merge (07:47Z)
Draft deliverables exist: YAML catalog (259 fields), signal table (69 rows + top 12 + tool table), Atlas gap table with bug list.
Wave 10 = verification: A RAR4/RAR5 YAML entries vs unrar source + rar_meta.rs, add atlas status; B 7z/zip/PAR2/SFV/SRR/rev entries likewise; C verify each claimed Atlas bug at the cited lines (true/false + evidence). Each writes a fragment; Opus merges into the YAML.

## Wave 10 (07:48Z): verification
### W10C result -> raw/w10c-atlas-bugs.md (checkout ad111a0)
CONFIRMED: probe RAR4 Unicode names ASCII-only (probe/archive.rs:81-83); stale encryption columns (steps.rs:142, store.rs:70-71); probe zip encrypted ignores 0x0040/0x2000; zip non-UTF-8 names Latin-1 not CP437; 7z encoded header LZMA-only (LZMA2/filter headers -> damaged); per-file CRC dropped (RAR5 listing, RAR4 listing, zip CD, 7z substreams); SFV take(3) starvation possible; view cap 256 vs 4096 and PAR2 rows sort first so a big PAR2 listing hides archive rows.
PARTLY: RAR4 0xFFFFFFFF size (no special case; low impact); PAR2 md5 dropped from listing but used by duplicates::twin_fingerprint.
Extra: 7z header without names -> complete with zero files -> password No; zip::list merges encrypted but not encryption kind; legacy code-page RAR4 names -> U+FFFD; probe vint 10th byte; probes_per_minute cast to 0 -> Duration/0 panic (config-dependent).
### W10A result -> raw/w10a-rar.yaml (108 entries with atlas status), raw/w10a-rar-check.md
29 catalog fixes vs unrar: comp_info masks (dict 0x0f for RAR 5.0, FCI_RAR5_COMPAT 0x100000); crypt extra flag 0x02 = HASHMAC (not TWEAKED); HTIME unix+ns layout; REDIR name-length vint; UOWNER length-prefixed names, uid/gid only with flags; RAR5 PswCheck VERIFIED; RAR4 unknown size rule; RAR4 EXTTIME arctime consumes no bytes; RAR4 ENDARC DATACRC u32 then VOLNUMBER u16; RAR4 dir rule UnpVer<20 && attr&0x10; .rev5 offsets VERIFIED; RAR4 KDF VERIFIED.
Prototype rar_meta.rs bugs: 549, 373, 483-496, 552, 660, 862, 883 (listed in check file). 13 entries still UNVERIFIED.
### W10B result -> raw/w10b-other.yaml, raw/w10b-other-check.md
151 entries checked (+8 added: zip CD fields, PAR2 Main count). Fixes: 7z AES props bit layout, SWAP2/SWAP4 filters, zipcrypto check byte = CRC>>24 (or DOS time high byte with bit 3), PAR2 packet type is "PAR 2.0\0"+name, PAR2 Main file count @8, UniFileN name @16. W10B claimed 7z writes a 4-byte salt by default: FALSE, main session checked 7zAes.cpp: ResetSalt is inside a commented-out block, so default is IV only (props 0x53 0x0F). Fixed in merge.

### Wave 10 merge (07:58Z)
Merged w10a + w10b fragments into archive-field-catalog.yaml v2 (draft kept at raw/archive-field-catalog.w9-draft.yaml): 267 fields, 226 VERIFIED / 41 UNVERIFIED; Atlas: 34 stored, 67 parsed-but-dropped, 166 not read.
