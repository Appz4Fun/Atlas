# Payload fingerprints: Haiku 15-min thread log

(Moved here 07:27Z; ../log.md is shared with another thread.)

### Wave 1 merged (06:50Z)
- Best twin key without names: PAR2 FileDesc full MD5 + length (rename-proof). RSID is name-bound (File IDs hash the name), so a renamed repost gets a new RSID. Use RSID as "same post set", MD5 as "same bytes".
- Free whole-file CRC: crc32fast Hasher::combine over per-part pcrc32 using =ypart lengths. Needs every part header (we already have them from XOVER/bodies?) Needs sampling of how often pcrc32/crc32 are filled.
- Inner-file alt keys from the first article: RAR5/RAR4 file header CRC32 + unpacked size; 7z start header next-header offset/size/CRC (first 32 bytes) is a per-archive fingerprint.
- Packer/poster fingerprints: RAR dict size/method/host OS/RR flag; ngPost 716800-byte articles and uuid@ngPost Message-IDs.
- IFSC per-slice MD5+CRC32 allows partial matching when slice sizes agree.
- Gaps: creator strings, Nyuu/newsmangler defaults, line length.
- Direction: lean in. Wave 2 = exact tool strings from source, indexer prior art (nZEDb/newznab par hashes, srrDB/ReScene), obfuscation schemes.

### Wave 2 merged (06:58Z)
- Prior art: NNTmux stores PAR2 hash_16k in par_hashes and release_files.crc32, but only to rename obfuscated releases (±5% size). Nobody builds a twin/alt index. Risk: 16k MD5 alone collides for RAR volumes from one packer (identical headers), so require exact length + full MD5.
- srrDB search takes ArchiveCrc, ArchiveSize, RarHash, OSHash, SampleCrc: an external scene-identity oracle for re-RAR'd posts once we have inner CRC/size.
- Obfuscation: the classifier rules (sabnzbd is_probably_obfuscated) say which names to distrust; content keys survive. RAR5 -hp leaves only crypt header (salt + KDF + check value), which is itself a per-creation twin key.
- Peers: per-creation randomness (7z NextHeaderCRC, RAR5 salts/IV) = perfect twin keys; interval CRCs via crc_concat link posts split at different article sizes.
- Direction for wave 3: lean into byte-exact layouts of per-creation random fields, Rust prior art (scryer rarpar), and external NZB search engines as cross-group oracles.
- Opus header thread: NNTmux already uses RAR inner CRC32, PAR2 hash16k, MKV SegmentUID as donor joins (prior art). W2-C should go past SegmentUID: encoder SEI strings, Tracks UIDs, MP4 mvhd, BD/DVD IDs, and PRNG-free per-mux values.
- Haiku15 payload W3 takes byte layouts of per-creation random fields + Rust crate survey + NZB engines as oracles. So my W2-B switches to: obfuscation/deobfuscation schemes (random-name generators, NZB meta password, nzbget/sab deobfuscate code, RAR name-in-header vs subject) + ReScene/srrDB SRR internals (stored RAR headers let you recompute exact scene volumes -> link scene release and its reposts).
- Stream-metadata thread covers x264/x265 SEI, DV RPU, FLAC MD5, MKV SegmentUID. So W2-C switches to: anti-dedupe/DMCA-evasion repost techniques (random padding, junk files, nested archives, re-RAR, re-PAR, per-reupload salts) and which keys survive each.
- Archive-metadata thread covers full RAR/7z/zip/PAR2 field tables incl SRR/SRS layouts in W2. My W2-B keeps obfuscation/deobfuscation + srrDB as linking oracle, not layouts.
- Archive W2-3: ngPost packs rar -m0 (store) and 7z -mx0 -mhe=on -> inner bytes contiguous, supports W2-A CRC-algebra alt key. RAR5 .rev file lists size+CRC32 of every data volume. ParPar creator includes CPU arch.

### Wave 3 merged (07:27Z)
- Per-creation random fields readable from article 1 are perfect twin keys and separate twin from alt (same content keys + different salt = independent re-pack): RAR5 -hp salt (16 B after signature), RAR5 file-encryption salt+IV, RAR4 8-byte salt after filename (flag 0x0400), ZIP AES salt, MKV SegmentUUID (random per mkvmerge output). Unencrypted 7z: NextHeaderCRC is creation-unique via mtimes (weaker).
- RAR5 BLAKE2sp record = rename-proof content key when the file is not split.
- Encryption is rare on Usenet (anecdote ~1%); obfuscation dominates.
- Rust: par2-rs scan_packets(&[u8]) handles partial buffers; nzbdav-rar discards inner CRC/mtime/host OS; sevenz-rust2 can't parse a 32-byte prefix, so hand-parse. Go reference: nwaples/rardecode.
- No prior system links posts across NZBs by content. usenet-dl hashes raw NZB; a sorted Message-ID set hash only dedupes the same post re-shared.
- Wave 4: CRC interval algebra across different article sizes, fills/re-posted articles as part-level twins, re-PAR'd posts.

Note 07:28Z: the owner shared "Linking Signals Map" as the example format (copy in example-signals-map.md; live https://claude.ai/artifact/BEGjW1bU8S6Yqm3cnPMNpE). Final report should use: signal | proves (twin/alt/same film/uploader/tool) | cost tier (free/HEAD/article 1/last article/all headers/full) | Atlas status | notes, and fill the map's gaps.
- peer (opus posting w2): same (part,pcrc32,begin) under two MIDs = one segment; nzb-repair fills part>1 begin=1 Date=orig; nzbrefresh keeps MID; repost chains by md5_16k+length (arXiv 2307.07005).

### Wave 4 merged (07:33Z)
- TESTED (tools/crc_interval.py, pure Python, no zlib combine): 60 MB random file posted at 716,800 B (2 articles missing) and at 768,000 B. From per-part CRC32s alone, both sides compute CRCs of the 10,752,000-byte LCM-grid intervals; 4 of 4 complete intervals matched and all equal zlib.crc32 of the real bytes. A 1-bit change dropped exactly the interval containing it (3 of 4). So partial twin matching across different article sizes works with zero payload, even with missing articles.
- Same algebra joins PAR2 IFSC slice CRC32s to another post's yEnc pcrc32s on the LCM(slice, article) grid (e.g. 1 MiB vs 716,800 = 183.5 MB grid): one PAR2 index article checks a different post's data with zero payload, for big files.
- Fill/late-article rule (proposal): article joins file F iff yEnc name, size, total equal F, (begin,end) equals F's stored range for that part, and pcrc32 equal when both present; mismatch = conflict (some posters supersede parts with corrupt copies), keep first, never purge. Key (name,size,total), never subject (ngPost retries change subject + MID; Nyuu re-randomizes MID unless --keep-message-id). Obfuscated cross-posts with differing names fall back to (size, begin/end, pcrc32) sequence equality over >=2 parts.
- PAR2: File ID = MD5(hash16k||length||name) confirmed in par2cmdline ComputeFileId; File ID survives re-PAR with a new slice size, RSID does not. par2cmdline default slice = ceil(total/2000) rounded to x4 (deterministic from size), redundancy 5%. No tool aligns slices to articles.
- RAR: read the inner whole-file CRC from the LAST volume header (non-last volumes carry only their chunk CRC), which explains pyrescene issue 168.
- Prior art: SABnzbd #2396 designed article-CRC combine (order-free, precomputed coefficient, ~1 s); NNTmux collectionHash = sha1(name+totalFiles); yencoding-multi Assembler identifies files by size and places parts by byte range.

### Wave 5 merged (08:08Z)
- Rust CRC: crc32fast 1.5 `Hasher::new_with_initial_len(pcrc32, part_len)` + `Hasher::combine` (both pub) are all the interval algorithm needs, since it only combines contiguous parts forward. No port required. For speed later: crc32fast's internal shift_table(len) (4x256 tables per fixed stride) or crc-fast `checksum_combine` (PCLMULQDQ). PAR2 last-slice zero padding needs rapidyenc-style `crc32_unzero`, or skip the last slice. SABnzbd's combine = sabctools wrappers over rapidyenc (crc32_combine, crc32_xpow8n, crc32_zero_unpad).
- Key strength (birthday at 1e8 files): CRC32 alone ~1.2e6 false pairs; (size, CRC32) ~1-120 pairs; so single (size, crc32) is a CANDIDATE key only. Twin-grade = full MD5, or >=4 matching per-part pcrc32s / LCM intervals (>=128 bits). Literature (BEP 52 per-file roots, ed2k/AICH, Liang/Kumar/Ross pollution and index-poisoning papers) agrees: exact hash for identity, size as hint, and add structural checks (decodable, complete) because fakes can carry valid hashes; one poster pushing many variants is a decoy signal. Fuzzy hashes (ssdeep) no good for twins.
- Index: keys are per file/interval, not per article, so tens of millions of rows; a composite-key table (tag, fp, file_id) in Atlas's existing DB is enough to start. At 1e9 keys RocksDB + bloom has the best published miss latency (3.4 us vs redb 95 us). Optional per-sealed-segment binary fuse16 filter (xorf) + fastbloom tail.
- Wave 6 direction: pivot to adversarial/edge cases in my lane: (A) corrupted/partial/superseded parts and how they poison CRC keys; (B) alt detection across containers using CRC algebra on store-mode inner data offsets; (C) validation corpora that are legal/public (Linux ISOs, public-domain posts, test groups like alt.binaries.test) for measuring precision/recall.

CORRECTION 08:33Z: pcrc32 sits on the =yend line inside each article BODY, so the CRC interval method needs every article's body (or its tail), not just overview. "Zero payload" means no extra download beyond the body fetches Atlas already does; it is not a HEAD/XOVER-only key. Cheap overview pre-filter (opus yEnc peer): same bytes+tool+line length give identical per-part :lines and :bytes (~10 bits/part) before fetching bodies.

### Wave 6 merged (08:42Z)
- Strict yEnc validation checklist (spec 1.3 quoted from yenc-0.4.0 sdist): begin/end 1-based inclusive, =yend size == end-begin+1 == decoded length, pcrc32 of decoded bytes, part<=total, contiguous non-overlapping ranges, crc32 (if any) == combine of pcrc32s. Parse CRC hex numerically (1-8 digits, any case). Never use whole-file crc32 as a part CRC (bug in yencoding 0.1.0). Only verified parts produce keys; most bugs cause false rejects, which is the safe side.
- Tests: libarchive RAR .uu fixtures (BSD-2), sabctools decoder tests, par2cmdline as GPL oracle only (don't vendor), zlib-rs combine.rs as reference; cap declared sizes before allocating (cf. CVE-2025-11579 rardecode dictionary DoS); fuzz each header parser; truncate fixtures at every offset; combine property test vs zlib.
- Clustering policy: LINK edges only from twin-grade keys (full MD5+len, per-creation random field, >=4 part CRCs/intervals); size+CRC32 = candidate until a 2nd independent signal; tool/uploader/size = annotate only, never edges. Union-find over link edges; alarm on largest cluster and bridge count (splink style). Evaluate per evidence tier with a labelled gold set (pairwise P/R, closest-cluster F1). Consistent with "group twins, don't collapse".

### Wave 7 merged (09:20Z)
- VERIFIED pcrc32 = CRC32 of the part's DECODED bytes (NZBGet Decoder.cpp, sabctools yenc.cc, Nyuu lib/article.js, yencode README; I re-checked yencode's example values with zlib). Nyuu writes crc32= (whole file, combined) only on the last part. The 1.3 draft's "encoded part" wording is just loose.
- PAR2 index cost = ONE article. par2cmdline index: Main | FileDesc xN | IFSC xN (sorted by File ID) | Creator. ParPar: FileDesc/IFSC interleaved per file, then Main, Creator. The packet order itself fingerprints the tool. Even at the 32768-slice cap the whole index (~655 KB IFSC) fits one 700 KB article for up to ~200 files; a 4 GiB file at par2cmdline default is ~40 KB.
- 7z (7-Zip HEAD): header LZMA-compressed by default even at -mx0 (when >=2 items) and sits at the tail, so 7z per-file CRCs need the LAST article plus an LZMA prefix decode (peers: lzma-rust2 works on prefixes). kMTime and attributes on by default, kCTime/kATime off.
- ZIP: TESTED Info-ZIP 3.0 `zip -0` (file and stdout) writes real CRC32+sizes in the LOCAL header (flag 0), so article 1 gives the inner alt key. Go always uses a data descriptor; Python only when unseekable; Java STORED requires CRC up front. The central directory (last article) is always authoritative.

### Wave 8 merged (09:50Z)
- Decoding: follow rapidyenc (used by both NZBGet and sabctools): strip CR/LF anywhere, keep space/tab, carry the escape state across chunks, unstuff dots once at exactly one layer, end on a line-start =yend. If the CRC matches, the part is trustworthy whatever its oddities. If it fails, retry the known ambiguous interpretations (=CR at line end, dot-unstuff on/off, old Nyuu unescaped last column) and otherwise mark the part unverified. Never key unverified parts.
- Partial-buffer parsing: hand-written tri-state parser (NeedMore/Invalid/Parsed), checked arithmetic, caps before allocation, vint max 10 bytes. Per-format integrity checks usable on partial data: RAR4 HEAD_CRC16, RAR5 per-block CRC32, 7z StartHeaderCRC (bytes 12..32), PAR2 packet MD5 (bytes 32..end, length %4, >=64), EBML VINT rules (RFC 8794). nzbdav-rar has no allocation cap and mixes EOF with invalid, another reason to parse ourselves.
- NNTP: RFC 3977 allows pipelining; range OVER in big batches across 4-8 connections; COMPRESS DEFLATE (RFC 8054) after STARTTLS+AUTHINFO, opt-in under TLS; XZVER is Astraweb-origin. Rust crates: nntp-rs 0.3 (COMPRESS + XFEATURE GZIP, fetch_xover), nzb-nntp 0.2.26 (pipelined ARTICLE/STAT). Per-provider HDR support for X-Received-Body-CRC is the yEnc-headers (Haiku 15-min) thread's wave 7.

### Wave 9 merged (10:36Z)
- TESTED (tools/slice_vs_article.py): folding a file's article pcrc32s and folding its PAR2 IFSC slice CRCs (last slice un-padded) give the same whole-file CRC32. So one PAR2 article gives a CRC32+length per file that can be matched to an upload's article CRCs with no payload: places equal-size RAR volumes onto the right FileDesc entry, and links twins whose PAR2 is missing on one side. Shared interior LCM boundaries are usually too far apart to matter.
- PAR2 presence: ParPar and Nyuu examples write an index by default; ngPost has PAR2 off by default (article size 716800). par2cmdline puts critical packets in the index file and interleaves copies into recovery volumes. No tool sets slice size = article size by default. No published share of posts carrying PAR2, so Atlas must measure it itself (count posts with a parseable FileDesc).
- Recovery Set ID changes with the stored name; key on full MD5 + length (+ hash16k).
- Evaluation: build a synthetic corpus with our own yEnc writer (Nyuu has no dry-run; only --dump-failed-posts) plus par2cmdline/ParPar (-s slice size). Measure pairwise and cluster precision (arXiv 2404.05622 ratio estimator; Wilson interval for a sampled pair set). Audit union-find with size caps, low density and bridge edges (Splink cluster metrics). CRC32 alone = blocking key only.

### Wave 10 merged (11:05Z) + final
- Tool tells: ParPar Creator has arch token; par2cmdline Creator last; RAR4 host_os/UNP_VER; 7z UNIX_EXTENSION hint. Inner-file CRC: RAR5 last part only (non-last = packed chunk CRC), tweaked under encryption; RAR4 plain; 7z in compressed tail; ZIP CD (AE-2 = 0). Storage: sorted runs + sparse index + binary fuse filter per run; edges with evidence, cluster id cache.
- Final report: Claude Doc https://claude.ai/artifact/4encPYR1gKbgmAU9vQQ76g ; markdown copy final-report.md.
