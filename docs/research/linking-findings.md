
## Haiku research: post linking via posting behavior
### Wave 1 (2026-11-02 06:50Z)
- Group-independent "posting-session fingerprint": decoded part length (=ypart end-begin+1; Nyuu 700K, ngPost 716800), yEnc line length (Nyuu 128), Message-ID shape+host (Nyuu `^[A-Za-z0-9]{24}-\d+@nyuu$`, ngPost `<uuid>@ngPost`), subject template. Constant within one run.
- ngPost GROUP_POLICY has a "one group per file" mode, a documented cause of one post split across groups (Batman case). https://github.com/mbruel/ngPost
- ngPost -x obfuscation: Subject == Message-ID local part. Nyuu `--date now`: whole run shares one Date. https://raw.githubusercontent.com/animetosho/Nyuu/HEAD/help-full.txt
- Date vs NNTP-Posting-Date skew may be constant per posting session (unvalidated).
- Details: research/posting-behavior/haiku/wave01-*.md

### Wave 2 (2026-11-02 07:22Z)
- ngPost hands out files sequentially (src/PostingJob.cpp ~640-707), so in group-per-file posts the last articles of file k overlap the first of file k+1 in time. This boundary overlap stitches slices across groups.
- ngPost retry = NEW UUID Message-ID immediately, same part/subject (NntpArticle.cpp ~117-131): dedupe by (yEnc name, part, begin/end). Nyuu post-check repost keeps the SAME Message-ID and Date, >=35 s later (nyuu_config.js check.delay 5000 + recheckDelay 30000).
- Nyuu Date = article build time (lib/article.js genTime), monotone in queue order.
- nxg-upper Message-ID = SHA256("NxGHeader:type:number"): deterministic, all data+PAR2 IDs derivable from the header (github.com/Tensai75/nxg-upper README).
- Fill = same (name, size, begin, end, pcrc32), later Date, new MID.
- Uploader clock fingerprint: packer UTC offset = Date minus RAR4/zip DOS local time; Injection-Date minus Date per injecting host (RFC 5536 3.2.7; NNTP-Posting-Date is deprecated); RAR5 time flag 0x0010 ns and FILETIME-vs-Unix choice fingerprint the packer (rarlab technote).
- Details: research/posting-behavior/haiku/wave02-*.md

### Wave 3 (2026-11-02 07:56Z)
- ngPost per-file group = _grpList.at(std::rand() % nbGroups) (src/PostingJob.cpp _initPosting), seed std::srand(QUuid::createUuid().data1) (src/NgPost.cpp): uniform random, 32-bit seed per run, not round-robin. Seed recovery would predict every file's group in the run.
- ARTICLE/HEAD/STAT by <message-id> never depend on the selected group (RFC 3977 6.2.1), so predicted sibling IDs can be STATed directly. No evidence of provider STAT throttling found.
- Default group pools collide (Nyuu/Postie a.b.test; ngPost sample a.b.test,a.b.misc), so pool membership is weak except for unusual pools. Real case of RARs in a.b.misc and PAR2 in a.b.moovee: forums.newsbin.com/viewtopic.php?p=220769
- No public real binary header dumps or NZBs exist (only synthetic crate fixtures). Validate on Atlas's own overview rows and sealed log.
- Details: research/posting-behavior/haiku/wave03-*.md

### Wave 4 (2026-11-02 08:30Z)
- PAR2 bridge across groups: decode the first article of each slice, take size + md5(first 16384 bytes) = PAR2 hash16k (ParPar lib/par2gen.js:1663; par2cmdline filechecksummer.cpp), join to the PAR2 index FileDesc packets on (size, hash16k), then accept only if the matched FileIDs recompute the set ID (FileID = md5(hash16k||len||name), lib/par2.js). Works for ngPost, Postie and nxg-upper; never key on name.
- Postie default full obfuscation gives every ARTICLE a different random yEnc name (poster.go:1447), so "yEnc name constant per file" fails for Postie. Postie builds PAR2 on real names; ngPost renames before RAR so PAR2 names = posted names (PostingJob.cpp:991-1095).
- PAR2 creator strings: "ParPar v<ver> <arch> [...]" (bin/parpar.js:336), "Postie" (internal/par2/par2.go:310). Defaults: ngPost 8% (`c -l -m1024 -r8`), Postie 5%.
- Ground truth: Spotnet spots in free.pt carry RSA-signed X-XML whose NZB/Segment is the Message-ID of the zipped NZB in alt.binaries.ftd (github.com/spotnet/spotnet/wiki/Spot-Xml-format).
- Prior art: nzb-monkey-go "directsearch" builds an NZB from headers in groups g near date d for an NZBLNK (github.com/Tensai75/nzb-monkey-go).
- Same-uploader over months: Timeprints cosine on 6 period-of-day + 7 weekday + 24 hour bins (Security Informatics 2015, ~500 posts for >80%); KS on inter-post gaps (arXiv 2407.04350, >=20 events). Use arrival time, not Date.
- Details: research/posting-behavior/haiku/wave04-*.md

### Wave 5 (2026-11-02 09:00Z)
- Prior art can't handle split or fully obfuscated posts: nzb-monkey-go directsearch (12h window, Subject substring, MD5(header+poster+totalFiles)), nZEDb md5(subjectPrefix+From+groupId), pynab hash(name, group, poster, parts) all key per group; NNTmux merges groups only via Xref (true crossposts). None reads yEnc.
- Survives max obfuscation (Postie full, ngPost --obfuscate, NewsUP): per file (yEnc size=, total=, article size) + begin/end chain; per run header order (ngPost From,Newsgroups,Subject,Message-ID with no Date; Postie adds Date "UTC"), pcrc32 padding (Postie %08x, ngPost unpadded), default ngPost domains From @ngPost.com / MID @ngPost. Postie randomizes name=, ngPost and NewsUP keep the real name=.
- TRiToN case: Batman.Knightfall.Part.1...2160p.UHD.BluRay.Hybrid.REMUX...-TRiToN is one 47.8 GiB MKV (xrel.to NFO), so the split slices are RAR/PAR2 volumes of one file: use the PAR2 bridge and RAR volume chain.
- Details: research/posting-behavior/haiku/wave05-*.md

### Wave 6 (2026-11-02 09:30Z)
- Overview-only size proxy (simulated, yEnc 1.3 escapes, expansion 1.015625): :lines per non-last article = 5692±1 (716800), 5955±1 (750000), 6098±1 (768000), so :lines identifies article size; the last article's :lines gives file size to ~±300 B. Equal-size RAR volumes match last-article :lines within ±2 lines 97%, false pairs ~0.1%. Prefer :lines over :bytes (headers vary by server), but Highwinds-fed XOVER may leave :lines EMPTY (per Opus posting thread), so fall back to :bytes there.
- RAR volume chain from the first article: RAR5 main header archive flag 0x0002 + volume-number vint (absent = volume 0), header flags 0x0008/0x0010 split before/after, file header inner name + unpacked size; RAR4 keeps the volume number in the END block (flag 0x0008). Prior art: NZBGet daemon/postprocess/RarRenamer.cpp + RarReader.cpp.
- Volume size defaults: ngPost RAR_SIZE 42 MB raised to post/RAR_MAX(99); nxg-upper >=153600000 B, max 150 volumes; Postie does not RAR.
- Spotnet harvester (Spotweb source): spot <NZB><Segment> IDs are the DEFLATE-compressed NZB carriers in alt.binaries.ftd (unescape =C/=B/=A/=D then gzinflate), not the binary's segments.
- Details: research/posting-behavior/haiku/wave06-*.md

### Wave 7 (2026-11-02 10:02Z)
- Fetch scheduling as budgeted entity resolution: BODY fetch = oracle, PAR2 index = hub resolving a whole set; order by benefit ~ |C(u)|*|C(v)|*p and skip transitively implied pairs (Firmani et al., PVLDB 9(5) 2016, vldb.org/pvldb/vol9/p384-firmani.pdf); budget windows with re-ranking (Altowim et al., PVLDB 7(11) 2014); supervised meta-blocking prunes 72-98.5% of pairs (Papadakis et al., PVLDB 7(14) 2014). Estimate ~10-25 fetches per 100-volume post vs 100-200 naive (unmeasured).
- Encoder line-wrap: node-yencode/rapidyenc escape TAB/SPACE at line END too; ngPost Yenc.cpp and NewsUP only at column 0. Simulated: same :lines (5690.9±0.8 at 716800), :bytes differs ~44 B/article only for binary data -> not usable per article; use Message-ID/header order for tool ID.
- Upload sessions: ngPost jobs strictly sequential with no sleep (NgPost.cpp:970-978); shipped ngPost.conf has GEN_FROM = true; nzbup (godevindahouse/nzbup) spawns one ngPost per release with --compress --gen_name --gen_par2; Postie watcher serial (max_concurrent_uploads 1, MinFileAge 60 s, schedule windows).
- Details: research/posting-behavior/haiku/wave07-*.md

### Wave 8 (10:31Z): bulk headers, prevalence, evasion
- No shipped uploader randomizes article size, yEnc line length, posting order, jitter or junk files. Nyuu's maintainer rejects article-length changes (issue #121). The yEnc part/total/size/begin/end chain and the PAR2 FileDesc bridge survive every obfuscation mode seen.
- Postie size settled: default 750000, preset 768000 (config.go:574-575, 1110). ngPost default GROUP_POLICY is ALL; Postie default is each_file.
- NEW: Postie's PAR2 policy renames the PAR2 files but builds the PAR2 packets from the REAL basename (internal/par2/par2.go:107-113), so the FileDesc name field leaks the real filename.
- Postie full randomizes Date per article (now minus 0-6 h), which kills Date-based signals; use server order/Injection-Date instead.
- No public data on how common obfuscated or group-per-file posts are, and none on provider LIST OVERVIEW.FMT/HDR support (Eweka, Highwinds, UsenetExpress). Both need a live probe from Atlas. Rust NNTP crates are tiny (nzb-nntp 2.3k downloads), so write our own HDR/COMPRESS DEFLATE.
- Details: research/posting-behavior/haiku/wave08-merged.md

### Wave 9 (11:02Z): PAR2 real-name leak, injection headers, server ordering
- CORRECTION to wave 8: Postie's leak is in CreateSet/setInputNames (internal/par2/par2.go:223-259, existing.go:233-243), not par2.go:107-113. Confirmed: FileDesc holds the real folder-relative name under every policy while the yEnc name is random or md5(name+part). ngPost --compress --gen_name gives obfuscated FileDesc names (PAR2 over random RAR names); --gen_par2 alone leaks real names. SABnzbd and NZBGet already deobfuscate this way.
- INN (source): Injection-Info posting-host is cleartext client IP/rDNS, logging-data is the nnrpd PID (per-connection key), Path gets .POSTED.<host>!. Links one connection's articles regardless of obfuscation. Commercial backbone behaviour unverified; needs Atlas HEAD dumps.
- INN adds Injection-Date only if Message-ID or Date is missing (nnrpd/post.c ~400-405). So Nyuu/Postie posts usually lack it; use article-number order. Crosspost Xref pairs are exact simultaneous anchors between group counters on one server (innd/art.c:1450-1481).
- Details: research/posting-behavior/haiku/wave09-merged.md

### Wave 10 and final report (11:45Z)
- Final report (Claude Doc): https://claude.ai/artifact/3GbNvzUQt4Yn5Gm9WbAATY . Signal map with proves/cost/Atlas status, tool fingerprint gap fills, stitching pipeline, fetch policy, evasion matrix, validation plan.
- Wave 10: no public HEAD dump of a binary article from any commercial backbone exists; Atlas must probe its own. Nyuu adopts a server-rewritten Message-ID, so key on the served ID. NewsUP: 768000, no Message-ID by default, real yEnc name kept under obfuscation, uploads a rename PAR2 built from real names, and mimics other tools' ID shapes with near-misses. nxg-upper ObfuscateRar (default) gives obfuscated FileDesc names. GoPostStuff: 768000, UnixNano$gps@gopoststuff, no Date. Nyuu CLI default is 716800 (768000 is only a library fallback).
- Ground truth: Spotnet NZBs for split posts; Anime Tosho data dump (stopped ingesting about 2026-05-09) for twins and alts. NZBIndex is 1 request/hour.
- Details: research/posting-behavior/haiku/wave10-merged.md

## Haiku 15-min research: yEnc headers
### Wave 1 (2026-11-02 06:48Z)
- yEnc: ybegin size=/name= identical on every part; ypart begin/end are 1-based contiguous (begin=0 or 1-byte gaps flag buggy encoders). pcrc32 per part is the part-level twin key (independent of line= and escaping); crc32 whole-file + size is the file-level twin key. https://yence.sourceforge.net/docs/protocol/version1_3_draft.html
- Encoder fingerprint candidates: field order (only name= must be last), tab before name= (exists in the wild per yencoding crate), hex case/zero-padding, line=, escape set (extra TAB/SPACE/DOT escaping is decoder-invisible, only via escape-frequency stats), missing total= (legacy). Normalise headers by tokens, never by raw text. https://docs.rs/crate/yencoding/0.2.0/source/src/header.rs
- Nyuu default Message-ID `...-<unix-ms>@nyuu` clusters a run by time; Nyuu-Obfuscation fork uses `{rand(32)}@{rand(8)}.{rand(3)}` and randomises yEnc name + Date per article (shape still fingerprints). NewsUP default poster `NewsUP@somewhere.cbr`. https://github.com/animetosho/Nyuu/pull/148 , https://github.com/Antidote2151/Nyuu-Obfuscation
- NNTP: one Message-ID in Xref with several group:num = a true crosspost (one article); Xref numbers are per-server. Injection-Info logging-data = per-session id; Injection-Date minus Date = injector/client clock fingerprint. What Omicron/Abavia/UE/Eweka strip is unverified.
- Details: research/yenc-header-forensics/log.md

## Haiku 15-min research: posting behavior
### Wave 1 (2026-11-02 06:52Z)
- Nyuu Message-ID `rand24-{timestamp}@nyuu`: timestamp is per-article encode time, so one file's articles form a tight monotone run that survives subject/From/name obfuscation. Message-IDs are re-rolled on every repost, so they are never a twin key. https://raw.githubusercontent.com/animetosho/Nyuu/HEAD/help-full.txt
- Nyuu default From leaks the poster's local user@host unless -f is set, which is a cross-post uploader key.
- kkm/nzb-indexer holds incomplete releases ~4h for late parts and retries missing article numbers 5x ("partrepair"). This is a precedent for a merge window on sliced posts. https://github.com/kkm/nzb-indexer
- Segment fingerprint (size, part, begin/end, pcrc32) joins byte-identical segments across groups and reposts.
- Clustering method candidates: CopyCatch lockstep cores (WWW 2013), temporal-window sweep "Setting the clock" (arXiv 2609.21959), streaming spam-campaign clustering (Haider ICML 2009).
- Dead end: no academic paper on Usenet binary linking; no public collectionhash or Xref heuristic found via search (next: read nZEDb / newznab-tmux source).
- Log: research/posting-behavior/log.md

## Haiku 15-min research: payload fingerprints
### Wave 1 (2026-11-02 06:52Z)
- Rename-proof twin key: PAR2 FileDesc full-file MD5 + length (+16k MD5). Recovery Set ID is name-bound (File ID = MD5(md5_16k||len||name)), so a renamed repost changes RSID. https://parchive.github.io/doc/Parity%20Volume%20Set%20Specification%20v2.0.html
- Free whole-file CRC32 from headers: crc32fast `Hasher::combine` over per-part pcrc32 with =ypart lengths; then compare against PAR2/SFV/RAR inner CRC. Needs all parts.
- Alt keys from article 1: RAR4/RAR5 file header inner CRC32 + unpacked size; 7z start header (first 32 bytes) next-header offset/size/CRC. https://www.rarlab.com/technote.htm
- PAR2 IFSC per-slice MD5+CRC32 gives sub-file matching when slice sizes agree. SABnzbd's 16k-MD5 dedupe rule: a hash seen twice in one post is ambiguous, never a link.
- Details: research/payload-fingerprints/haiku15/

### Wave 2 (2026-11-02 06:58Z)
- Prior art: NNTmux `par_hashes` (PAR2 hash_16k) + `release_files.crc32`, matched in NameFixingService::hashCheck/crcCheck with ±5% size, only for renaming to PreDB names. No twin/alt index exists anywhere. 16k MD5 alone collides on RAR volumes from one packer; require exact length + full MD5. https://github.com/NNTmux/newznab-tmux
- srrDB /search/ accepts ArchiveCrc, ArchiveSize, RarHash, InternetSubtitlesDbHash (OSHash), SampleCrc: an external scene-identity oracle once inner CRC/size are known.
- RAR5 -hp leaves only the crypt header (salt, KDF count, check value) readable; the salt is a per-creation twin key.
- sabnzbd is_probably_obfuscated() rules (32 hex, 40+ hex/dots, abc.xyz) say which names to distrust. nzbget par-renamer groups PAR sets by content (issue 313).
- Rust prior art to read: github.com/scryer-media/rarpar crates/par2-rs/src/evidence.rs ("ContiguousAssembly" with hash_16k).
### Wave 3 (2026-11-02 07:27Z)
- Per-creation random fields in article 1 are perfect twin keys and separate twin from alt (same content keys + different salt = independent re-pack): RAR5 -hp salt (16 B after signature), RAR5 file-encryption salt+IV, RAR4 8-byte salt after filename (flag 0x0400), ZIP AES salt, MKV SegmentUUID. Refs: https://www.rarlab.com/technote.htm, https://raw.githubusercontent.com/nwaples/rardecode/master/archive50.go
- Rust: par2-rs `scan_packets(&[u8], off)` parses complete packets from a partial buffer; nzbdav-rar reads but discards inner CRC32/mtime/host OS; sevenz-rust2 cannot parse a 32-byte prefix.
- No system links posts across NZBs by content. usenet-dl SHA-256s the raw NZB; NZBLNK h= is a header search string.
### Wave 4 (2026-11-02 08:01Z)
- TESTED: zero-payload partial twin matching across different article sizes. A 60 MB file posted at 716,800 B (2 articles missing) and at 768,000 B: from per-part CRC32s alone both sides compute CRCs of the 10,752,000 B LCM-grid intervals; 4/4 complete intervals matched and equal zlib.crc32 of the bytes; a 1-bit change dropped exactly its interval. Script: research/payload-fingerprints/haiku15/tools/crc_interval.py (pure Python CRC combine).
- The same algebra joins PAR2 IFSC slice CRC32s to another post's yEnc pcrc32s on the LCM(slice, article) grid (1 MiB vs 716,800 = 183.5 MB), so one PAR2 index article checks a different post's data. Large files only.
- Late-article rule (proposal): joins file F iff yEnc name, size, total match, (begin,end) equals the stored range for that part, and pcrc32 equal when both present; a mismatch is a conflict (keep first, flag, never purge). Never key on subject (ngPost retries change subject and Message-ID).
- PAR2 File ID = MD5(hash16k||length||name), confirmed in par2cmdline descriptionpacket.cpp ComputeFileId; it survives a re-PAR at a new slice size, RSID does not. par2cmdline default slice = ceil(total/2000) rounded to x4.
- RAR: take the whole-file inner CRC from the LAST volume header (non-last volumes carry only their chunk CRC); explains pyrescene issue 168.
- Prior art: SABnzbd issue 2396 (order-free article-CRC combine, ~1 s precomputed); yencoding-multi Assembler places parts by byte range keyed by file size.
### Wave 5 (2026-11-02 08:36Z)
- Correction to wave 4: pcrc32 is on the =yend line of each article body, so CRC interval matching uses the body fetches Atlas already does; it is not an overview-only key. Overview :bytes/:lines sequences are the cheap pre-filter.
- Rust: crc32fast 1.5 `Hasher::new_with_initial_len(pcrc32, part_len)` plus `Hasher::combine` (both pub) cover the interval method. Faster options: crc-fast `checksum_combine`, rapidyenc `crc32_combine`/`crc32_unzero`.
- Key strength at 1e8 files: CRC32 alone ~1.2e6 chance pairs, (size, CRC32) ~1-120, so it is a candidate key; twin-grade needs full MD5 or >=4 matching part CRCs/intervals.
- Index: per-file keys in a composite (tag, fp, file_id) table are enough to start; RocksDB+bloom has the best published miss latency at 1e9 keys.
### Wave 6 (2026-11-02 09:07Z)
- Strict yEnc validation before any CRC becomes a key (spec 1.3, yenc-0.4.0 sdist doc/yenc-draft.1.3.txt): begin/end 1-based inclusive; =yend size == end-begin+1 == decoded length; part <= total; ranges contiguous and non-overlapping; whole-file crc32 (if present) == combine of the pcrc32s. Parse CRC hex numerically (1-8 digits, any case). Never use crc32 as a part CRC (bug in yencoding 0.1.0). Most decoder bugs give false rejects, which is the safe side.
- Caveat from the opus payload thread's measurements: unencrypted RAR is deterministic, so an independent re-RAR with the same name and switches shares every middle article. A middle-span CRC match therefore means "same bytes there", not twin. A twin verdict also needs the header-bearing articles or a per-creation field to match.
- Linking policy (record-linkage literature, splink): only twin-grade keys create links (full MD5+len, per-creation random field, >=4 matching part CRCs/intervals incl. headers). size+CRC32 stays a candidate until a second signal arrives. Tool, uploader and size only annotate. Union-find over links, with alarms on the largest cluster and bridge count. Measure per evidence tier against a labelled gold set.
- Tests: libarchive RAR .uu fixtures (BSD-2), par2cmdline as an oracle only (GPL), cap declared sizes before allocating (CVE-2025-11579 rardecode), cargo-fuzz per header parser, truncation at every offset.
### Wave 7 (2026-11-02 09:38Z)
- VERIFIED: pcrc32 = CRC32 of the part's DECODED bytes (NZBGet daemon/nntp/Decoder.cpp, sabctools src/yenc.cc, Nyuu lib/article.js, yencode README; example values re-checked with zlib). Nyuu puts crc32= (the combined whole-file CRC) on the last part only.
- PAR2 index costs ONE article: par2cmdline writes Main | FileDesc xN | IFSC xN (sorted by File ID) | Creator; ParPar interleaves FileDesc/IFSC per file, then Main, Creator (so packet order also identifies the tool). Even at the 32768-slice cap the index fits a 700 KB article for up to ~200 files; a 4 GiB file at par2cmdline default is ~40 KB.
- 7z (7-Zip HEAD source): the end header is LZMA-compressed by default even at -mx0 (for >=2 items) and sits at the tail, so per-file CRCs need the last article plus an LZMA prefix decode. kMTime and attributes are on by default, kCTime/kATime off.
- ZIP: TESTED Info-ZIP 3.0 `zip -0` writes the real CRC32 and sizes in the local header (flag 0), so article 1 gives the inner key. Go always uses a data descriptor, and Python does only when unseekable. The central directory (last article) is authoritative.
### Wave 8 (2026-11-02 10:12Z)
- Decode like rapidyenc (NZBGet and sabctools use it): strip CR/LF anywhere, keep space/tab, carry escape state across chunks, unstuff dots once at one layer, end on a line-start =yend. A matching part CRC is proof whatever the oddities; on a mismatch retry the known ambiguous readings, else mark the part unverified and never key it. (Proves: nothing by itself; protects every CRC-based twin key.)
- Partial-buffer parsing: write our own tri-state parser (NeedMore/Invalid/Parsed) with checked arithmetic and caps before allocation. Integrity checks that work on a prefix: RAR4 HEAD_CRC16, RAR5 per-block CRC32, 7z StartHeaderCRC, PAR2 packet MD5, EBML VINT rules. nzbdav-rar has no allocation cap, another reason to parse ourselves.
- Fetch cost: pipelined range OVER over 4-8 connections plus COMPRESS DEFLATE (RFC 8054) keeps header-tier signals cheap; body-tier keys (pcrc32, PAR2 index, article-1 archive headers) should be fetched once per file and cached.
### Wave 9 (2026-11-02 10:36Z)
- TESTED: a file's article pcrc32s folded with crc32_combine equal its PAR2 IFSC slice CRCs folded (last slice un-padded with crc_unzero), both equal to the whole-file CRC32. One PAR2 article therefore gives CRC32 + length per file to match against any upload's article CRCs with no payload download. This places equal-size RAR volumes on the right FileDesc entry, and links twins when only one side carries PAR2. (Proves: twin candidate at file level; twin-grade once MD5 or several files agree. Cost: body, one PAR2 article plus part CRCs.) Script: research/payload-fingerprints/haiku15/tools/slice_vs_article.py
- PAR2 presence by tool default: ParPar/Nyuu write an index; ngPost has PAR2 off by default. No public stat on the share of posts with PAR2, so Atlas should count posts with a parseable FileDesc. Recovery Set ID depends on the stored name; key on full MD5 + length.
- Measuring false links: synthetic corpus with our own yEnc writer (Nyuu has no dry-run) plus par2cmdline/ParPar; pairwise and cluster precision (arXiv 2404.05622); audit union-find clusters by size cap, density and bridge edges (Splink cluster metrics).
### Wave 10 / final (2026-11-02 11:05Z)
- Inner-file CRC32 is the alt key across RAR/7z/ZIP. It sits in the LAST RAR volume (non-last volumes carry only the packed-chunk CRC), the compressed 7z tail, or the ZIP central directory, and in article 1 only for single-volume RAR and zip -0. RAR5 encryption tweaks CRC32 and BLAKE2sp with the key, and ZIP AE-2 stores 0, so those can't be alt-linked. RAR4 and AE-1 keep plain CRCs.
- Payload tool tells (annotate only): ParPar Creator `ParPar v<ver> <arch> [url]`, par2cmdline writes Creator last in each file, RAR4 host_os/UNP_VER, the 7z UNIX_EXTENSION attribute flag.
- Key storage at 1e8 files: append-only sorted runs with a sparse index plus a static binary fuse filter per run. Store every edge with its evidence and keep the cluster id as a cache, because union-find can't split.
- FINAL REPORT (signal | proves | cost | Atlas | notes, plus linking policy and build order): https://claude.ai/artifact/4encPYR1gKbgmAU9vQQ76g , markdown copy research/payload-fingerprints/haiku15/final-report.md
## Haiku research: post linking via payload fingerprints
### Wave 1 (2026-11-02 07:00Z)
- PAR2 File ID = MD5(hash16k, length, ASCII name), so a rename changes the ID; the rename-proof twin key is hash16k + length (+ full MD5). UniFileN can carry a different name than the ASCII FileDesc name. https://parchive.sourceforge.net/docs/specifications/parity-volume-spec/article-spec.html
- IFSC packets hold per-slice MD5+CRC32; SABnzbd builds a full-file CRC32 from slice CRCs with crc32_combine (fixing up the last slice). So one PAR2 index article gives every file's full CRC32, comparable with yEnc =yend crc32 and RAR inner CRC. https://github.com/sabnzbd/sabnzbd/blob/develop/sabnzbd/par2file.py
- Alt key across RAR4/RAR5/7z: unpacked size + inner data CRC32 (RAR4 file header @11 and @16; RAR5 file header fields; 7z SubStreamsInfo). RAR5 extra area 0x03 has ns-precision mtime, a strong twin key when kept. https://www.rarlab.com/technote.htm
- Don't trust volume/SFV CRCs for twins: ReScene documents padding RAR volumes to a target CRC; the dictionary size, method, -mt thread count and host OS byte fingerprint the exact packer build. UNP_VER is useless (RAR 6.24 still writes 29). http://rescene.wikidot.com/collisions-queue
- Tool hints: PAR2 Creator packet string, slice size, RecvSlic exponent scheme, PkdMain subslice size (rare).
- Detail: research/payload-fingerprints/haiku/w1*.md

## Research post linking via yEnc headers (Opus 3x3)
### Wave 1 (2026-11-02 06:52Z)
- ngPost (mbruel) is identifiable from body bytes alone: pcrc32 printed with std::hex and no zero padding (~1/16 parts < 8 hex digits), =yend has no part= and never crc32=, trailing TAB/SPACE at line end never escaped (`column-1==maxwidth` can't be true), and an empty line before =yend when a part is an exact multiple of 128. Hydro74000 fork v5.6 fixed padding+ws escaping and moved to mt19937 IDs, so these are version fingerprints. https://raw.githubusercontent.com/mbruel/ngPost/master/src/utils/Yenc.cpp , https://github.com/Hydro74000/ngPost/pull/20
- Per-encoder templates: Nyuu always multipart (part=1 total=1 for single), crc32 on last part only, 8 lowercase hex; GoPostStuff pcrc32 %08X UPPERCASE; Rust `yenc` crate omits total= and never escapes TAB/SPACE; python-yenc/newsmangler and Rust crate dot-stuff instead of "=n". Leading "=n" vs raw '.' ratio and last-column whitespace stats vote ~2/256 lines.
- node-yencode issue #3: older builds missed escaping a critical char at the last byte of a segment (Nyuu posts) -> Nyuu version range fingerprint. https://github.com/animetosho/node-yencode/issues/3
- Message-ID grammars: Nyuu ^([A-Z][a-z]){12}-\d{13}@nyuu$ (strict case alternation; ms = Date header; retries keep first timestamp); ngPost v4 UUID (nibble 13 = '4', 17 in [89ab], survives custom signature); GoPostStuff <UnixNano$gps@gopoststuff>; newsmangler <epoch.5dp.partnum@NNTP-server-host> leaks the uploader's provider. NxG Upper: SHA256-derived 40hex@21hex.3hex shape. https://github.com/Tensai75/nxg-upper
- Header order (HEAD only): Nyuu Message-ID,Subject,From,Newsgroups,Date,Path(empty),User-Agent; ngPost From,Newsgroups,Subject,Message-ID (no Date, so server-made Date); GoPostStuff From,Newsgroups,Message-ID,X-Newsposter,Subject.
- Twin key: use yEnc part size + :lines (server-independent), not :bytes (includes injected headers, differs per server/crosspost copy).
- NNTmux de-obfuscates via "donor" joins on RAR inner CRC32 / PAR2 hash_16K (5% size tol) and MediaInfo UniqueID = MKV SegmentUID (10%); enhanceShortSubject() keys random subjects by punctuation skeleton. NewsGuy NNTP-Posting-Host is a per-account hash token.
- Dead ends: no public write-ups on nzbindex/binsearch/nzbking grouping; current Omicron/Abavia/UE/Eweka injected headers still unverified.
- Details: research/yenc-header-forensics/opus/w1*.md

## Research post linking via posting behavior (Opus, behavioral/temporal)
Wave 1 (full notes in research/posting-behavior/):
- TRiToN pattern = ngPost --obfuscate + group_policy=each_file (postie has it too). Every random From shares the Message-ID signature domain (xxx13@ngPost.com vs <32hex@ngPost>); ngPost sends no Date, so Date = server arrival. https://github.com/mbruel/ngPost
- Per-tool Message-ID shapes as a classifier: Nyuu rand24-msEpoch@nyuu; postie 32@8.3, From 14 <14@5.3>; nxg-upper 40@21.3 hex + shared X-Nxg; GoPostStuff UnixNano$gps; newsmangler epoch.5dp.part@host.
- Arrival-curve join: per-group article number -> wallclock from high-water-mark scans; match bursts + bytes/sec across groups.
- XOVER Bytes/Lines ratio exposes yEnc line width; default article sizes 716800 (ngPost/Nyuu), 768000 (NewsUP).
- DMCA co-removal vectors (STAT over time across providers) link a whole release after takedown: https://rp.os3.nl/2014-2015/p82/report.pdf
- Injection-Info nnrpd PID/posting-host is per connection (INN 2.6+), HEAD only, often stripped.

## Catalog all yEnc and Usenet article metadata

Doc (YAML catalog of every field, each with an `atlas:` status checked against main at a29ee52): https://claude.ai/artifact/PJsrDryQJnLBCvxKmAjLaX

A web research workflow (six sweeps, one per source type, a gap-fill critic and one synthesis pass; 269 sources) finished at 09:46Z; its merged findings and full source list are appended below under 'Research workflow: prior art and sources'.

Leads from reading the specs against the Atlas code (2026-11-02 06:50Z), with where Atlas stands:

1. **Xref:full is in the XOVER row for free** on nearly every provider (column 9 when LIST OVERVIEW.FMT says `Xref:full`): the whole crosspost set and the server hostname per article with no HEAD. `parse_overview` (src/nntp.rs) reads 8 columns and drops the rest; the probe gets Xref from one HEAD per release.
2. **Injection-Info `posting-account=`** (and legacy `NNTP-Posting-Host`, INN `X-Trace` hashed host) is a stable per-account key on providers that set it. Atlas keeps the header verbatim in `release_probe.info.headers` but never parses the parameters or indexes them.
3. **Message-ID local part and domain** fingerprint the posting tool and sometimes carry a timestamp or part number. Atlas already stores the domain separately (`domains` table, src/store.rs) but nothing surfaces or searches it.
4. **X-Received-Body-CRC / X-Received-Bytes** (Highwinds-family readers such as Newshosting, and Usenet.Farm). The CRC is verified (yEnc Haiku thread, 12 real articles, 09:28Z) as zlib.crc32 of the body text with CRLF line endings plus the final dot line, headers excluded: a HEAD-only twin key for reposts whose encoded body is byte-identical (same tool, yEnc name and part split) and a free integrity check after BODY. X-Received-Bytes is roughly the wire size of the whole article. Still open whether they are stamped at injection and carried or added by the reading backbone (one HEAD through two backbones settles it). Not read by Atlas.
5. **Header set and order** are a client fingerprint that survives a spoofed User-Agent. Atlas keeps 12 named headers (KEPT_HEADERS, src/probe/mod.rs) and loses the order and the rest.
6. **Date vs Injection-Date** skew: a poster-clock fingerprint and fake-date detector. Both strings kept, never compared.
7. **yEnc pcrc32 per part + part sizes** are the byte-level twin proof and combine into the file CRC32 (crc32 combine, no `crc32=` needed). Atlas verifies pcrc32 only for the 1 to 3 probed articles and stores none per article; `release_probe.crc32` (file CRC, indexed) is kept for the main file only.
8. **References** set on every part by a few posters (pointing at part 1): free grouping. Parsed from XOVER, dropped.
9. **:lines** from XOVER is dropped too; with :bytes it gives the mean line length (line=128 vs 256) and spots non-yEnc bodies without a BODY.

### Research workflow: prior art and sources (appended 09:52Z)

A web research workflow ran beside this catalog on 2026-11-02: six searchers, one per source type (specs and RFCs; open-source parsers; correlation and deobfuscation techniques; academic papers; community posts on reddit, forums, GitHub issues, Google Groups and IRC; archived and historic pages), a completeness critic that searched for what they missed, and one synthesis pass that merged the 269 surviving sources into the sections below. Tags like [sab-deobf] resolve to URLs in the Sources list. Match levels: twin = byte-identical; alt = same inner file, new wrapper; encode = same encode, new container; film = same title; post = same upload or NZB; uploader = same poster, tool or session.

Two cautions. The Atlas column in these tables uses the sweep's own legend (yes, part, no, ?) and was judged from the sweep's reading of the repository, not re-verified; where it disagrees with the catalog doc (https://claude.ai/artifact/PJsrDryQJnLBCvxKmAjLaX), the catalog was checked against main a29ee52 and wins. Three claims that the sibling research threads later overturned are marked [corrected] in place.

#### 1. Metadata fields found in the wild

##### 1.1 NNTP transport and overview

| Field | Sources | Atlas |
|---|---|---|
| OVER/XOVER: artnum, Subject, From, Date, Message-ID, References, `:bytes`, `:lines`, then LIST OVERVIEW.FMT extras (`Xref:full` keeps the header name) | [rfc3977] [rfc2980] [nntplib] | part: no References, `:lines`, extras |
| `:bytes`/`:lines` (server) vs `Bytes:`/`Lines:` (poster; a faked Lines is a tool tell) | [rfc3977] | part |
| Article number: per group, rising with arrival; number-vs-date binary search finds retention edges | [rfc3977] [nntmux-bin] [newznab-kop1] | ? |
| HDR/XHDR/XPAT over ranges (`LIST HEADERS` answer `:` = any header); LIST COUNTS, ACTIVE status `y n m x j =alias` | [rfc3977] [rfc2980] [rfc6048] | ? |
| Compressed overview: COMPRESS DEFLATE; XZVER/XZHDR (yEnc-wrapped raw deflate); `XFEATURE COMPRESS GZIP [TERMINATOR]` (GZIP credited to Giganews, XZVER to Astraweb). Highwinds/eweka lack COMPRESS and send zlib under the GZIP label: sniff 0x78/0x1f/raw; terminator inside the stream | [rfc8054] [willglynn] [spottarr] [pynntp] [ietf-nntp-2009] | ? |

##### 1.2 Article headers, including server-injected

| Field | Sources | Atlas |
|---|---|---|
| Injection-Info `posting-host`, `posting-account`, `logging-data`, `mail-complaints-to`. INN: posting-host `host:ip`, logging-data = nnrpd PID, posting-account off by default; eternal-september posting-host = MD5(IP) | [rfc5536] [inn] [inn-2014] [hashcat] | part: one string |
| Injection-Date; NNTP-Posting-Host/Date (obsolete) | [rfc5536] [rfc5537] | yes |
| Path diagnostics `!.POSTED[.host]`, `!!`, `!.SEEN.`, `!.MISMATCH.`; Highwinds `fx06.iad.POSTED` names the front end; some providers send only `not-for-mail` | [rfc5537] [pcreview] [leo] | part: raw |
| Xref `server group:number …` | [rfc5536] [nntmux-coll] | part: one article |
| X-Trace: INN `<host> <unix> <pid> <ip> (<date>)` (IP may be rewritten); encrypted provider tokens (`sv3-…`) | [big8] [inn-2011] [harley] | no |
| X-Received-Bytes, X-Received-Body-CRC (decimal). [corrected] The sweep found no algorithm; the yEnc research thread later verified it on 12 real Newshosting and Usenet.Farm articles as zlib.crc32 of the body text with CRLF line endings plus the final dot line, headers excluded, and X-Received-Bytes as roughly the article's wire size (see lead 4 above and the catalog doc) | [pcreview] [leo] | no |
| X-Usenet-Provider, X-Complaints-Info, X-Originating-IP, X-Original-NNTP-Posting-Host, NNTP-Posting-Path, X-User-ID, X-Auth, `Old-*` (reinjection) | [inn] [harley] [big8] | no |
| Cancel-Lock/Cancel-Key `sha256:`/`sha512:`/`sha1:`: per-message HMAC, so no linking; the scheme fingerprints software | [rfc8315] [inn] | no |
| Supersedes, Control, Approved, Archive/X-No-Archive, Distribution, Followup-To, Original-Sender, X-Face, X-PGP-Sig; obsolete Lines, Posting-Version, See-Also date the software | [iana] [rfc5536] [xnoarchive] | no |
| Non-yEnc multipart: References/See-Also, MIME `message/partial` id/number/total; RFC 2047 encoded-words; RFC 2231 `filename*N*=` | [rfc1036] [rfc2046] [rfc2231] | ? |
| Tool headers: `User-Agent: Nyuu/<ver>`, `Newspost/2.2.1 (url)`; `X-Newsposter: newsmangler <ver> (<mode>) - <repo>` (trailing extra CRLF), `gopoststuff <ver> - <repo>`, `sanguinews v<ver> (ruby <ver>)` | [nyuu] [newspost] [newsmangler] [sanguinews] | yes: one article |
| `X-Nxg`: `fileNumber:totalFiles:filename:partType:totalSize`, encrypted with the NxG header | [nxg-go] | no |
| Signed metadata: `Verify-Signature-newzNZB`, `X-Ufk`; Spotnet `X-XML`, `X-XML-Signature`, `X-User-Key`, `X-User-Signature`, and category/size/date packed into From | [newznzb-sig] [leo] [spotweb] | no |

##### 1.3 yEnc

| Field | Sources | Atlas |
|---|---|---|
| `=ybegin part total line size name` (name last; no part = single-part); `=ypart begin end` (1-based); `=yend size part pcrc32 crc32` | [yenc13] [sabctools] | yes |
| Trailer variants: ngPost/NewsUP omit `part=`/`crc32=` and don't zero-pad pcrc32; newsmangler omits `crc32=`; GoPostStuff uppercases pcrc32; sanguinews gets begin/end wrong on a short last part; Nyuu writes `crc32=` on the last part only; 0 of 500 large releases had a whole-file crc32 | [ngpost] [newsup] [newsmangler] [sanguinews] [kd476] | part |
| Body style: rapidyenc (Nyuu) lines are 128 or 129 bytes, TAB/SPACE escaped only at line edges; NewsUP/ngPost leave line-final TAB/SPACE raw; newspost escapes TAB anywhere, never SPACE | [rapidyenc] [newsup] [newspost] | no |
| Parsing: match `=ybegin ` with its space (`=ybegin2` reserved, V2 never shipped); skip stray `=y` lines; uuencode still appears | [yenc13] [yenc-notes] [sabctools] | ? |
| Encrypted yEnc: FF1-encrypted keyword lines, `=yencryption` line, XChaCha20-Poly1305 body; SABnzbd PR closed unmerged | [yenc-enc] [sab-3676] | no |

##### 1.4 NZB and indexer feeds

| Field | Sources | Atlas |
|---|---|---|
| `<meta>` title, category, tag and password (repeatable), `X-*`; NZB 1.2+ per-file meta and password; v3 file@bytes/@segments | [nzb-spec] [newzbin-specs] | ? |
| file@poster, @date (server-seen), @subject (`(xx/yy)` → `(1/yy)`); segment bytes/number/Message-ID; Nyuu can make NZB subject/poster differ from the articles | [nzb-spec] [nyuu] | yes (from XOVER) |
| Newzbin DirectNZB HTTP headers `X-DNZB-Name/Category/MoreInfo/ProperName/EpisodeName/Year/Password/Details/Failure` | [sab-urlgrabber] [hellanzb] | ? |
| Links: NZBLNK `h p t g d`; `nxglnk://?h=&p=&t=` | [nzbmonkey-go] [nxg-upper] | no |
| newznab attrs `password` 0/1/2 (2 = inner archive), `prematch` | [newznab-api] | ? |

##### 1.5 PAR2 (and PAR1, PAR3)

| Field | Sources | Atlas |
|---|---|---|
| Main, FileDesc (File ID, MD5, MD5-16k, length, name), UniFileN, set ID. File ID = MD5(MD5-16k, length, name) and set ID = MD5(Main body), so both change on rename | [par2] [sab-par2] | yes |
| IFSC: MD5 + CRC32 per slice → whole-file CRC32 of each protected file (usually the RAR volumes) | [par2] [sab-par2] [nzbget-par] | no |
| Creator: `Created by par2cmdline version X.Y.Z.`, `ParPar v<ver> <arch> [url]`, `par2j v1.3.3`; CommASCI/CommUni comments | [par2cmd] [parpar] [par2j] | no |
| Par profile: slice size (`-s2000` = par2cmdline default), `vol12-22` vs `vol12+10`, pow2 volume sizes | [parpar] | no |
| PAR1 set hash = MD5 of the file MD5s (renames don't change it); PAR3 draft: BLAKE3, CRC-64 rolling hash, owner/group names | [par1] [par3] | no |

##### 1.6 SFV, NFO, SRR, SRS, logs

| Field | Sources | Atlas |
|---|---|---|
| SFV generator line (`; Generated by WIN-SFV32 v1.1a … on <date> at <time>`), per-file size+mtime comments, CRC case | [sfv-sample] [rarinfo] | no |
| NFO: IMDb `tt\d{7,}`, TVDB, TMDb, AniDB, ISBN/ASIN/UPC; hidden NFO = one-segment `(1/1)` file without extension, 12–65535 bytes; old `(0/n)` posts carry NFO text | [nntmux-rfm] [nzedb-namefixer] [geocities-yenc] | ? |
| SRR blocks 0x69 header (app), 0x6A stored file, 0x6B OSO/ISDb hash, 0x71 RAR volume + its raw headers; SRS `SRSF` (sample name, size, CRC32), `SRST` track signatures | [rescene-go] [pyrescene] [zeebinz-srr] | no |
| EAC log `==== Log checksum <64 hex> ====` (public key: shows only no hand edits) | [eac] | no |

##### 1.7 Archives and disc images

| Field | Sources | Atlas |
|---|---|---|
| RAR4: MHD VOLUME 0x01, SOLID 0x08, NEWNUMBERING 0x10, PASSWORD 0x80 (encrypted headers), FIRSTVOLUME 0x100; LHD SPLIT_BEFORE/AFTER 0x01/0x02, PASSWORD 0x04, UNICODE 0x200, SALT 0x400; ENDARC volume number; UNP_VER 29/50/70 | [unrar] [forensics-rar] [nzbget-rar] | part: names, sizes |
| RAR5: volume number; Metadata record (original archive name, creation time); data CRC32; BLAKE2sp; ns times; Unix owner names/IDs; encryption salt/KDF and tweaked-checksum flag; CMT, QO, RR. Split files: only the last part's CRC is the inner file's | [rar5] [rarinfo] | part |
| 7z: NextHeaderCRC, kName, kCTime/kATime/kMTime, kWinAttributes, kComment, per-file CRC, kEncodedHeader (names hidden), 7zAES 06F10701 | [7z] [rarinfo] | part |
| ZIP: GP bits 0/3/11/13, version-made-by OS, extras 0x5455 (times), 0x7875 (uid/gid), 0x7075 (Unicode path), 0x9901 AES (AE-2 sets CRC to 0) | [appnote] [winzip-aes] | part |
| Inner-file byte range in each volume | [rarinfo] | ? |
| ISO 9660 PVD IDs and dates; UDF VolumeSetIdentifier (first 8 hex = time) | [pycdlib] | no |

##### 1.8 Media, subtitles, audio, images

| Field | Sources | Atlas |
|---|---|---|
| MKV Info: SegmentUUID 0x73A4 (MediaInfo "Unique ID"), Prev/NextUUID, SegmentFamily, Segment/Prev/NextFilename, DateUTC, MuxingApp, WritingApp | [rfc9559] [mkv-schema] [mediainfo] | no |
| MKV tracks: TrackUID, CodecPrivate; stats tags NUMBER_OF_FRAMES, NUMBER_OF_BYTES, DURATION, BPS (writing app/date mismatch = edited after mux); `--deterministic` drops DateUTC | [mkvmerge] [mediainfo] | part: tracks |
| MKV attachments (fonts, `cover.jpg`, FileUID); ChapterSegmentUUID 0x6E67 links another file; tags IMDB, TMDB, TVDB, ENCODER_SETTINGS, PURCHASE_OWNER | [mkv-schema] [mkv-tags] [mkv-tagging] | no |
| Encoder SEI: x264 `x264 - core N … options: …`; x265 build, OS, compiler, bit depth, options | [x264] [x265] | no |
| MP4: hdlr names (`VideoHandler`, `L-SMASH Video Handler`), ©too (`Lavf…`), compressorname; iTunes `apID` (account email), `ownr`, `purd`; `com.apple.quicktime.*` keys. AVI: INFO ISFT/INAM/ICRD | [movenc] [exiftool-qt] [exiftool-riff] | part: tracks |
| ASS headers in CodecPrivate; Aegisub `; Script generated by Aegisub <ver>`, `[Aegisub Project Garbage]` Video File/Audio File | [mkv-attach] [aegisub] | no |
| FLAC STREAMINFO MD5 of decoded audio, vendor string, CUESHEET MCN/ISRC; MP3 LAME `music_crc`, preset, delay/padding | [rfc9639] [mutagen] | no |
| EXIF Make, Model, BodySerialNumber, LensSerialNumber, OwnerName, Software, OffsetTimeOriginal, GPS (personal: store hashed) | [exiftool-exif] | no |
| Computed: OSHash (size + 64-bit sums of first/last 64 KiB; collisions trivial); ed2k (MD4 of 9,728,000-byte chunks; two variants at exact multiples) | [oshash] [ed2k] | OSHash yes, ed2k no |

#### 2. Correlation and grouping techniques

- **Subject collection keys.** NNTmux: part regex `^\s*(?!Usenet Index Post)(.+?)\s+\((\d+)\/(\d+)\)`, file counter `[[(\s](\d{1,5})(\/|[\s_]of[\s_]|-)(\d{1,5})[])[\s$:]`, `collectionhash = sha1(cleaned name . totalFiles)` with no poster or group (Xref groups merged); binary = md5(`file:`+n), else md5(subject+poster). nZEDb and old newznab also key on group (binaries on From too); nZEDb releases incomplete collections after 2 h. [nntmux-coll] [nzedb-binaries] [nzedb-wiki] [newznab-kop1] → post
- **Message-ID set fingerprints.** SABnzbd MD5 of all segment IDs; NZBGet BJ96 hash, also with ParIgnoreExt files filtered out; NNTmux order-independent `article_fingerprint`; nZEDb `nzb_guid` = md5(first segment ID). [sab-nzbparser] [nzbget-nzbfile] [nntmux-1897] [nzedb-nzb] → post
- **Whole-file CRC32.** Combine article pcrc32s in order (`crc32_combine`), or fold IFSC slices (`crc = crc32_multiply(crc, crc32_xpow8n(slice)) ^ slice_crc`, then un-pad the last); match PAR2, SFV and yEnc `crc32=` at equal size. [sab-par2] [sab-newsunpack] [nzbget-par] [node-yencode] → twin
- **PAR2 content keys.** Equal FileDesc MD5 + length = same bytes, even when File ID and set ID differ (both include names); PAR1 set hash ignores names; NZBGet groups cloaked par2 by set ID. [par2] [par1] [nzbget-rename] → twin
- **Inner-file keys.** RAR inner CRC32 (last volume) or BLAKE2sp, 7z/ZIP per-file CRC + size; void under RAR5 tweaked checksums or ZIP AE-2. NZBGet DupeMatcher: same largest inner size and same stored/compressed (`-m0`) state. [rar5] [7z] [winzip-aes] [nzbget-nzbfile] → alt
- **Archive identity.** Same encryption salt = same archive; new salt with the same inner CRC/mtime = repacked. NNTmux records 7z next-header, RAR4 main-header CRC and RAR5 locator. [rar5] [nntmux-1897] → twin vs alt
- **Media identity.** Same SegmentUUID = same mux; new SegmentUUID with the same x264/x265 options and NUMBER_OF_FRAMES/BYTES = same encode remuxed; FLAC STREAMINFO MD5 and LAME `music_crc` survive retagging; Matroska IMDB/TMDB tags name the work. [mediainfo] [x264] [rfc9639] [mutagen] → twin, encode or film
- **Upload-session clocks.** Nyuu `^(?:[A-Z][a-z]){12}-\d{13}@` (ms; shape survives custom domains like `@PRiVATE`); newsmangler `<%.5f.%d@host>` (seconds, part number); GoPostStuff `<UnixNano$gps@gopoststuff>`; INN-generated `<sec32$pid32$count@domain>` (base-32 seconds since 673416000, nnrpd PID, counter). Inference: arrival-ordered article numbers, joined via Xref, bound a session. [nyuu] [nyuu-146] [leo] [newsmangler] [inn] → post
- **Injection identity.** Same path-identity + logging-data = one nnrpd session; INN X-Trace host + PID + time; hashed posting-host with a fixed salt (mixmin) or none (MD5(IP)) is stable per IP. Caveat: Newsgroupdirect, Blocknews, Usenet.Farm, Newsdemon and theCubeNet put the server address in NNTP-Posting-Host and encrypt X-Trace. [inn] [inn-2011] [nsr-2008] [anon-server] [hashcat] [ngprovider] → uploader
- **Flood keys.** Cleanfeed: posting host + sorted groups (150/200/1800 s), host + lines (20/80/3600 s), From + subject without trailing digits + lines (20/40/1800 s), fuzzy body MD5; host from Injection-Info, then NNTP-Posting-Host, X-Trace, Path. [cleanfeed] → uploader
- **Per-article obfuscation groups.** Key = subject shape (alphanumeric runs collapsed) + part total + file counter, for names with a 16+ char token; require ≥16 articles (min 8), ≥90% distinct names, enough distinct part positions; skip first and last parts. Otherwise ~690k collections/hour. [pr274] → post
- **Tool fingerprints.** Header set (ngPost sends only From, Newsgroups, Subject, Message-ID), article size (716800 Nyuu/ngPost, 768000 GoPostStuff, NewsUP and nxg-upper; [corrected] postie defaults to 768000 and falls back to 750000), yEnc body style, PAR2 Creator and slice size. NewsUP `--obfuscate` mixes 8 imitation Message-ID formats in one file (fake `@nyuu` with a ~12-char local part), so validate shapes strictly. [ngpost] [postie] [newsup] [rapidyenc] → uploader
- **Signed identity.** Spotnet spotterid = base64(little-endian crc32(decoded user key)) minus `/+=`; self-signed spots need sha1(`<msgid>`) starting `0000`; spot XML lists NZB segment IDs. [spotweb] [spotnet-wiki] → uploader, post
- **Metadata duplicates.** NZBHydra2: same normalized title, different indexers, equal poster/group when known, ≤2 h apart, size within 1% of the mean, both doubled if only one of poster/group is known. nZEDb: same name+poster+size is a dupe within a group, a crosspost across groups. [hydra] [nzedb-wiki] → post
- **Perceptual matching.** PDQ Hamming ≤30; TMK both levels ≥0.7; vPDQ quality ≥50, distance ≤31, ≥80% frames; FFmpeg `signature` th_d 9000, th_dc 60000, th_xh 116; audio BER <0.35, failing past ~2.5% speed change (PAL ~4%); AcoustID: three random 30 s samples away from the first/last 10 min; runtime off by >10% flags fakes. [pdq] [vpdq] [ffmpeg-sig] [haitsma] [acoustid] [liang] → film
- **Behavioural attribution.** Many usernames per IP flags fake publishers; automated posters start one-message threads with no replies; spam-template inference (anchors ≥6 chars); Nilsimsa match ≥24. [cuevas] [picturing] [judo] [nilsimsa] → uploader
- **Canonical copy and survival.** Keep the earliest Date, then the lowest Message-ID; spew score Σ√groups (≤20 per 45 days). Backbones dedupe and take down only by Message-ID, so twins with new IDs expire separately; STAT/HEAD can pass while BODY is gone, so probe BODY and count 451 apart from 430. [care] [breidbart] [perfect10] [rexum] [nyuu-146] → twin selection

#### 3. Deobfuscation techniques

- **Obfuscation verdict.** SABnzbd: obfuscated if `^[a-f0-9]{32}$`, `^[a-f0-9.]{40,}$`, `[a-f0-9]{30}` with ≥2 `[\w+]`, or `abc.xyz…`; clean if (≥2 upper, ≥2 lower, a separator), ≥3 separators, (≥4 letters, ≥4 digits, a separator), or capitalized with upper/lower ≤0.25; otherwise obfuscated. NZBGet token thresholds (mixed-case alnum ≥10, single-case ≥12, ≥16 digits, vowelless ≥8) plus 11 hash regexes; Sonarr dates its shapes (`^[A-Z]{11}\d{3}$` "seen on NZBGeek"). [sab-deobf] [nzbget-deobf] [nntmux-hashed] [sonarr]
- **Name priority and subject parsing.** PAR2 hash16k, then yEnc name (if not obfuscated), then subject: first non-empty quoted string, else `\b([\w\-+()' .,]+(?:\[[\w\-/+()' .,]*][\w\-+()' .,]*)*\.[A-Za-z0-9]{2,4})\b`. Handle `[PRiVATE]-[WtFnZb]-[name]-[n/N] - "" yEnc` (one total equal to the file count) and the `[N3wZ] \token\::` prefix; parse brackets by depth. [sab-object] [sab-misc] [sab-1682] [wtfnzb] [aiostreams] → name
- **First-article probe.** Fetch segment 1 of every file first: MD5 of the first 16384 bytes (if the article is ≥16 KiB or the file has one article) matched to FileDesc; `PAR2\0PKT` exposes par2 under any name, with its set ID; rename hidden par2 to `<setid>.vol%03d+01.PAR2`. Different `=ybegin` names within one file flag mixed reposts. [nzbget-rename] [sab-object] [hellanzb] → twin-grade name
- **Size-only mapping.** NZB posted bytes vs FileDesc length: ratio 0.90–1.001, floor size − size/512, segment count, one-to-one only; ties verified from a first article. Upstream probes the 3 largest unclassified files. [pr1246] [nntmux-wiki] → name
- **CRC rename.** Combined article CRC32 equal to the PAR2 file CRC32, at equal size, gives the PAR2 name with no MD5 work. [sab-newsunpack] → twin-grade name
- **RAR set rebuild.** Volume = RAR5 `main_volume_number + 1` or RAR3 ENDARC number + 1. NZBGet chain: number + 1, same RAR version, previous has NEXTVOL, and either first file split-before matching the previous split-after file name, or neither split. SABnzbd separates interleaved sets with a staircase check. [nzbget-rar] [sab-postproc] → set
- **Donor renaming.** Take the name of a release sharing inner CRC32 (size within 5%), MediaInfo Unique ID (10%) or PAR2 hash16k (5%); donors need predb_id > 0 (UID/hash also anidbid or poster `nonscene@Ef.net (EF)`); CRC candidates ordered .srr, main .rar, .part01.rar, video, .nfo. [nntmux-namefix] [nzedb-namefixer] → alt or twin
- **PreDB matching.** Score = similarity×0.6 + token overlap×45 + 6×shared tokens (+80 exact), top 50, accept ≥60, skip names under 8 chars. nZEDb date-anchored file names need ≥93% similar_text; generic pre file names (Cover, setup) caused mass false matches. Check pre `files`, `size`, nukes. [nntmux-predb] [nzedb-namefixer] [nzedb-2568] [predb-ovh] → name
- **Dehashing.** Precompute MD5(title), MD5(MD5(title)), SHA1(title), MD5(title.requestid), MD5(title.requestid.requestid); look up `[a-fA-F0-9]{32,40}` after stripping `-postbot`/`-Obfuscated`. [nzedb-ddl] [nzedb-2494] [leo] → name
- **Request IDs.** `^\[ ?([0-9]{4,6}) ?\]`, `^REQ\s*([0-9]{4,6})`; resolve by (requestid, group) with group remaps (classic newznab queried `allfilled.newznab.com/query.php?t=[GROUP]&reqid=[REQID]`); IDs also hide in RAR names (`164822.part005.rar`). Pre bots announce `[DT][TT][SC][CT][RQ: id:group][SZ][FL][FN]`. [nzedb-ddl] [nzedb-irc] [nzedb-2359] [newznab-kop1] [prenet] → name
- **Hash oracles.** srrDB `isdbhash:` (OSHash), `archive-crc:` + `archive-size:` (verify `archived-files`, not `files`), `store-real-crc:` (samples); "use but don't scrape"; 53 searches on mixed content all missed. AniDB `FILE size=&ed2k=` returns group, episode and version (≤1 packet per 2 s); AnimeTosho lists CRC32, MD5, SHA1, SHA256, ED2K and BitTorrent v2 hashes per file. [srrdb-api] [kd476] [anidb-udp] [animetosho] → name
- **Scene packaging checks.** TV x264 rules: RAR3/4 only, ≤101 volumes, volume size a multiple of 50,000,000 bytes, no compression, recovery or encryption, one mkv, SFV + NFO. Pre tools archive SFV contents, so renamed volumes match by CRC. [scene-tv] [topclient] → twin of the scene release; rule breakers are alts
- **Reversed, ROT13 and tagged names.** Sonarr `(?:^|[-._ ])(p027|p0801|\d{2,3}E-?\d{2}S)[-._ ]` → reverse; ROT13 subjects on Newsbin; strip repost suffixes (`-Obfuscated`, `-Scrambled`, `-postbot`, `-xpost`, `-NZBGeek`). [sonarr] [newsbin] → name
- **Name maps inside posts.** rename.sh/.bat `mv` lines, rename.par2, `.srr`/`.srs` base names, MediaInfo title, MKV SegmentFilename, Aegisub Video File. [nzbtomedia] [nntmux-namefix] [aegisub] → name
- **Uploader-supplied names.** Header indexing fails on obfuscated posts, so indexers rely on uploader NZBs: nntpPoster calls `api?hash={obfuscated}&name={real}`; newzNZB logs `HH:MM -> Name[newzNZB]_[9-hex]` daily; NxG Message-IDs = sha256(`<header>:data|par2:<n>`) split `[0:40]@[40:61].[61:64]`. [reddit] [nntpposter] [newznzb-log] [nxg-upper] → name
- **Message-ID counters.** JBinUp `.+?\.(\d{1,5})-(\d{1,5})@`; PowerPost `part(\d{1,4})of(\d{1,5})` (NewsUP fakes these). [nzbmonkey] [newsup] → post
- **Per-article randomizers.** Nyuu-Obfuscation and Postie "full": random yEnc name, From, Subject per article, Date random within 6 h; Postie "partial" keeps one yEnc name and poster per post. Regroup by yEnc size/total/line and begin/end continuity (inferred), ordered by server-side times. [nyuu-obf] [postie] → post
- **Encrypted and fake posts.** Passwords from NZB meta, `{{pw}}`, `name / pw`, X-DNZB-Password; one volume tests a password (RarCRCError "cannot find volume" = right); NZBGet decrypts RAR3/RAR5 headers with it. Cloaked = inner .rar named like the outer (<8 extra chars) or an inner name containing "password"; ignore `.html` adverts in PAR2 names. [sab-misc] [sab-assembler] [nzbget-rar] [kd1088]
- **Packs without PAR2.** `^[0-9a-f]{14}\.[0-9a-f]{30}\.[0-9a-f]{4}$` per-episode names; only content (subtitle text, runtime) identifies episodes. [sab-1795]

#### 4. Sources

##### Spec

- [rfc3977] RFC 3977, Network News Transfer Protocol: <https://www.rfc-editor.org/rfc/rfc3977.html> · <https://raw.githubusercontent.com/caronc/newsreap/master/docs/rfc3977.txt> (mirror)
- [rfc2980] RFC 2980, Common NNTP Extensions: <https://www.rfc-editor.org/rfc/rfc2980.txt>
- [rfc6048] RFC 6048, NNTP Additions to LIST Command: <https://www.rfc-editor.org/rfc/rfc6048.txt>
- [rfc8054] RFC 8054, NNTP Extension for Compression: <https://www.rfc-editor.org/rfc/rfc8054.txt>
- [rfc4643] RFC 4643, NNTP Extension for Authentication: <https://www.rfc-editor.org/rfc/rfc4643.txt>
- [rfc8143] RFC 8143, Using TLS with NNTP: <https://www.rfc-editor.org/rfc/rfc8143.txt>
- [rfc5536] RFC 5536, Netnews Article Format: <https://www.rfc-editor.org/rfc/rfc5536.txt> · <https://www.rfc-editor.org/rfc/rfc5536.html>
- [rfc5537] RFC 5537, Netnews Architecture and Protocols: <https://www.rfc-editor.org/rfc/rfc5537.txt> · <https://www.rfc-editor.org/rfc/rfc5537.html>
- [rfc1036] RFC 1036, Standard for Interchange of USENET Messages: <https://www.rfc-editor.org/rfc/rfc1036.txt>
- [rfc5322] RFC 5322, Internet Message Format: <https://www.rfc-editor.org/rfc/rfc5322.txt>
- [rfc2046] RFC 2046, MIME Media Types (message/partial): <https://www.rfc-editor.org/rfc/rfc2046.txt>
- [rfc2231] RFC 2231, MIME Parameter Value and Encoded Word Extensions: <https://www.rfc-editor.org/rfc/rfc2231.txt>
- [rfc8315] RFC 8315, Cancel-Locks in Netnews Articles: <https://www.rfc-editor.org/rfc/rfc8315.txt> · <https://www.rfc-editor.org/rfc/rfc8315.html>
- [iana] IANA Message Headers registry: <https://www.iana.org/assignments/message-headers/message-headers.xhtml>
- [yenc13] yEnc 1.3 draft (mirrors): <https://raw.githubusercontent.com/sabnzbd/sabctools/master/doc/yenc-draft.1.3.txt> · <https://raw.githubusercontent.com/caronc/newsreap/master/docs/yenc-draft.1.3.txt> · <https://yence.sourceforge.net/docs/protocol/version1_3_draft.html>
- [yenc-notes] yEnc-Notes3, version 2 proposals: <https://raw.githubusercontent.com/benvanik/node-nntp/master/doc/rfcs/yEnc-Notes3.txt>
- [yenc-enc] yEnc encryption standards (Tensai75): <https://raw.githubusercontent.com/Tensai75/yenc-encryption-standards/main/README.md>
- [nzb-spec] NZB file specification (SABnzbd wiki), with its namespace and DTD: <https://sabnzbd.org/wiki/extra/nzb-spec> · <http://www.newzbin.com/DTD/2003/nzb> · <http://www.newzbin.com/DTD/nzb/nzb-1.1.dtd>
- [par2] Parity Volume Set Specification 2.0: <https://parchive.github.io/doc/Parity%20Volume%20Set%20Specification%20v2.0.html> · <https://raw.githubusercontent.com/Parchive/parchive.github.io/master/doc/Parity%20Volume%20Set%20Specification%20v2.0.html> · <https://github.com/Parchive/Parchive.github.io/blob/master/doc/Parity%20Volume%20Set%20Specification%20v2.0.html> · <https://parchive.sourceforge.net/docs/specifications/parity-volume-spec/article-spec.html>
- [par1] Parity Volume Set Specification 1.0: <https://github.com/Parchive/Parchive.github.io/blob/master/doc/Parity%20Volume%20Set%20Specification%20v1.0.html>
- [par3] Parity Volume Set Specification 3.0 (draft): <https://raw.githubusercontent.com/Parchive/parchive.github.io/master/doc/Parity_Volume_Set_Specification_v3.0.md> · <https://github.com/Parchive/Parchive.github.io/blob/master/doc/Parity_Volume_Set_Specification_v3.0.md>
- [rar5] RAR 5.0 archive format (RARLAB technote): <https://www.rarlab.com/technote.htm>
- [7z] 7z format, 7zFormat.txt: <https://raw.githubusercontent.com/ip7z/7zip/main/DOC/7zFormat.txt> · <https://android.googlesource.com/platform/external/lzma/+/refs/heads/main/DOC/7zFormat.txt>
- [appnote] PKWARE ZIP APPNOTE.TXT 6.3.10: <https://pkware.cachefly.net/webdocs/casestudies/APPNOTE.TXT>
- [winzip-aes] WinZip AES encryption: <https://www.winzip.com/en/support/aes-encryption/>
- [rfc9559] RFC 9559, Matroska Media Container Format: <https://www.rfc-editor.org/rfc/rfc9559.txt> · <https://www.rfc-editor.org/rfc/rfc9559.html>
- [mkv-schema] Matroska EBML schema, ebml_matroska.xml: <https://raw.githubusercontent.com/ietf-wg-cellar/matroska-specification/master/ebml_matroska.xml>
- [mkv-tags] Matroska tag list, matroska_tags.xml: <https://raw.githubusercontent.com/ietf-wg-cellar/matroska-specification/master/matroska_tags.xml>
- [mkv-attach] Matroska attachments and subtitles: <https://raw.githubusercontent.com/ietf-wg-cellar/matroska-specification/master/cellar-matroska/attachments.md> · <https://raw.githubusercontent.com/ietf-wg-cellar/matroska-specification/master/cellar-codec/subtitles.md>
- [mkv-tagging] Matroska tagging: <https://www.matroska.org/technical/tagging.html>
- [rfc9639] RFC 9639, FLAC: <https://www.rfc-editor.org/rfc/rfc9639.txt>
- [anidb-udp] AniDB UDP API definition: <https://wiki.anidb.net/UDP_API_Definition>
- [ed2k] AniDB wiki, ed2k hash: <https://wiki.anidb.net/Ed2k-hash>
- [srrdb-api] srrDB API v1 and search keywords: <https://api.srrdb.com/v1/docs> · <https://www.srrdb.com/help> · <https://www.srrdb.com/help#keywords> · example query <https://api.srrdb.com/v1/search/archive-crc:0053CA13>
- [predb-ovh] predb.ovh API docs; live endpoint robots-disallowed: <https://predbdotovh.github.io/pre-api/> · <https://predb.ovh/api/v1/>
- [spotnet-wiki] Spotnet wiki, Spot header format: <https://github.com/spotnet/spotnet/wiki/Spot-Header-format>
- [newznab-api] Newznab API specification (nZEDb docs): <https://raw.githubusercontent.com/nZEDb/nZEDb/0.x/docs/newznab_api_specification.txt>
- [scene-tv] Scene rules, 720p TV x264 2016: <https://raw.githubusercontent.com/scriptzteam/SCENE-SCRiPTS/master/Others/Other/scene_rules/tvx2642k16-unformatted.nfo>
- [scene-flac] Scene rules, FLAC 2016: <https://raw.githubusercontent.com/scriptzteam/SCENE-SCRiPTS/master/Others/Other/scene_rules/2016_FLAC.nfo>

##### Open source

**SABnzbd**

- [sab-nzbparser] `sabnzbd/nzbparser.py`: <https://raw.githubusercontent.com/sabnzbd/sabnzbd/develop/sabnzbd/nzbparser.py>
- [sab-deobf] `sabnzbd/deobfuscate_filenames.py`: <https://raw.githubusercontent.com/sabnzbd/sabnzbd/develop/sabnzbd/deobfuscate_filenames.py> · <https://github.com/sabnzbd/sabnzbd/blob/develop/sabnzbd/deobfuscate_filenames.py>
- [sab-par2] `sabnzbd/par2file.py`: <https://raw.githubusercontent.com/sabnzbd/sabnzbd/develop/sabnzbd/par2file.py>
- [sab-object] `sabnzbd/nzb/object.py`: <https://raw.githubusercontent.com/sabnzbd/sabnzbd/develop/sabnzbd/nzb/object.py> · <https://github.com/sabnzbd/sabnzbd/blob/develop/sabnzbd/nzb/object.py>
- [sab-newsunpack] `sabnzbd/newsunpack.py`: <https://raw.githubusercontent.com/sabnzbd/sabnzbd/develop/sabnzbd/newsunpack.py>
- [sab-postproc] `sabnzbd/postproc.py`: <https://raw.githubusercontent.com/sabnzbd/sabnzbd/develop/sabnzbd/postproc.py>
- [sab-assembler] `sabnzbd/assembler.py`: <https://raw.githubusercontent.com/sabnzbd/sabnzbd/develop/sabnzbd/assembler.py>
- [sab-misc] `sabnzbd/misc.py`: <https://raw.githubusercontent.com/sabnzbd/sabnzbd/develop/sabnzbd/misc.py>
- [sab-urlgrabber] `sabnzbd/urlgrabber.py`: <https://github.com/sabnzbd/sabnzbd/blob/develop/sabnzbd/urlgrabber.py>
- [sab-3676] SABnzbd PR #3676, encrypted yEnc (closed unmerged): <https://github.com/sabnzbd/sabnzbd/pull/3676>
- [sabctools] sabctools `src/yenc.cc`: <https://raw.githubusercontent.com/sabnzbd/sabctools/master/src/yenc.cc>

**NZBGet**

- [nzbget-deobf] `daemon/queue/Deobfuscation.cpp`: <https://raw.githubusercontent.com/nzbgetcom/nzbget/develop/daemon/queue/Deobfuscation.cpp>
- [nzbget-rename] `daemon/queue/DirectRenamer.cpp`, `nzbget.conf`: <https://raw.githubusercontent.com/nzbgetcom/nzbget/develop/daemon/queue/DirectRenamer.cpp> · <https://raw.githubusercontent.com/nzbgetcom/nzbget/develop/nzbget.conf>
- [nzbget-rar] `daemon/postprocess/RarReader.cpp`, `RarRenamer.cpp`: <https://raw.githubusercontent.com/nzbgetcom/nzbget/develop/daemon/postprocess/RarReader.cpp> · <https://raw.githubusercontent.com/nzbgetcom/nzbget/develop/daemon/postprocess/RarRenamer.cpp>
- [nzbget-nzbfile] `daemon/queue/NzbFile.cpp`: <https://raw.githubusercontent.com/nzbgetcom/nzbget/develop/daemon/queue/NzbFile.cpp>
- [nzbget-par] `daemon/postprocess/ParChecker.cpp`: <https://raw.githubusercontent.com/nzbgetcom/nzbget/develop/daemon/postprocess/ParChecker.cpp>

**newznab, nZEDb, NNTmux and scene-file parsers**

- [nntmux-namefix] NNTmux `app/Services/NameFixing/NameFixingService.php`: <https://raw.githubusercontent.com/NNTmux/newznab-tmux/master/app/Services/NameFixing/NameFixingService.php>
- [nntmux-predb] NNTmux `app/Services/NameFixing/PredbMatchSelector.php`: <https://raw.githubusercontent.com/NNTmux/newznab-tmux/master/app/Services/NameFixing/PredbMatchSelector.php>
- [nntmux-hashed] NNTmux `app/Traits/DetectsHashedNames.php`: <https://raw.githubusercontent.com/NNTmux/newznab-tmux/master/app/Traits/DetectsHashedNames.php>
- [nntmux-coll] NNTmux `app/Services/Binaries/CollectionHandler.php`: <https://raw.githubusercontent.com/NNTmux/newznab-tmux/master/app/Services/Binaries/CollectionHandler.php>
- [nntmux-bin] NNTmux `app/Services/Binaries/BinariesService.php`: <https://raw.githubusercontent.com/NNTmux/newznab-tmux/master/app/Services/Binaries/BinariesService.php>
- [nntmux-nzb] NNTmux `app/Services/Nzb/NzbParserService.php`: <https://raw.githubusercontent.com/NNTmux/newznab-tmux/master/app/Services/Nzb/NzbParserService.php>
- [nntmux-rfm] NNTmux `app/Services/AdditionalProcessing/ReleaseFileManager.php`: <https://raw.githubusercontent.com/NNTmux/newznab-tmux/master/app/Services/AdditionalProcessing/ReleaseFileManager.php>
- [nntmux-wiki] NNTmux wiki, Postprocessing: <https://github.com/NNTmux/newznab-tmux/wiki/Postprocessing>
- [nntmux-1897] NNTmux PR #1897: <https://github.com/NNTmux/newznab-tmux/pull/1897>
- [pr274] xbmc4lyfe/newznab-tmux PR #274, per-article obfuscation grouping: <https://github.com/xbmc4lyfe/newznab-tmux/pull/274>
- [pr1246] xbmc4lyfe/newznab-tmux PR #1246, size-only PAR2 mapping: <https://github.com/xbmc4lyfe/newznab-tmux/pull/1246>
- [nzedb-ddl] nZEDb `resources/db/schema/mysql-ddl.sql`: <https://raw.githubusercontent.com/nZEDb/nZEDb/dev/resources/db/schema/mysql-ddl.sql> · <https://raw.githubusercontent.com/nZEDb/nZEDb/master/resources/db/schema/mysql-ddl.sql>
- [nzedb-namefixer] nZEDb `nzedb/NameFixer.php`, `nzedb/ReleaseExtra.php`: <https://raw.githubusercontent.com/nZEDb/nZEDb/master/nzedb/NameFixer.php> · <https://raw.githubusercontent.com/nZEDb/nZEDb/0.x/nzedb/NameFixer.php> · <https://github.com/nZEDb/nZEDb/blob/0.x/nzedb/NameFixer.php> · <https://raw.githubusercontent.com/nZEDb/nZEDb/master/nzedb/ReleaseExtra.php>
- [nzedb-binaries] nZEDb `nzedb/Binaries.php`: <https://raw.githubusercontent.com/nZEDb/nZEDb/dev/nzedb/Binaries.php>
- [nzedb-nzb] nZEDb `nzedb/NZB.php`: <https://raw.githubusercontent.com/nZEDb/nZEDb/master/nzedb/NZB.php>
- [nzedb-sharing] nZEDb `nzedb/Sharing.php`: <https://github.com/nZEDb/nZEDb/blob/0.x/nzedb/Sharing.php>
- [nzedb-wiki] nZEDb wiki, Release Management: <https://github.com/nZEDb/nZEDb/wiki/Release-Management>
- [newznab-kop1] newznab 0.2.x SVN mirror (2011), `www/lib/releases.php`, `backfill.php`: <https://github.com/kop1/newznab/blob/master/www/lib/releases.php> · <https://github.com/kop1/newznab/blob/master/www/lib/backfill.php>
- [rarinfo] rarinfo `RarInfo.php`, `SrrInfo.php` (DariusIII): <https://raw.githubusercontent.com/dariusiii/rarinfo/master/RarInfo.php> · <https://raw.githubusercontent.com/DariusIII/rarinfo/master/SrrInfo.php> · <https://github.com/DariusIII/rarinfo/blob/master/SrrInfo.php>
- [zeebinz-srr] rarinfo `srrinfo.php` (zeebinz): <https://raw.githubusercontent.com/zeebinz/rarinfo/master/srrinfo.php>
- [pyrescene] pyReScene `rescene/rar.py`: <https://raw.githubusercontent.com/srrDB/pyrescene/master/rescene/rar.py> · <https://github.com/srrDB/pyrescene>
- [rescene-go] rescene (Go) `srr.go`: <https://raw.githubusercontent.com/rescene/rescene/master/srr.go>
- [scenecrc] scenecrc.py gist (dawtmaytrikx): <https://gist.github.com/dawtmaytrikx/b69c6058dd127c04ebee05025fec28db>
- [srrdb-ps] SrrDBAutomationToolkit `Search-SatRelease.ps1` (PowerShell srrDB client): <https://www.powershellgallery.com/packages/SrrDBAutomationToolkit/0.4.0/Content/Public%5CSearch-SatRelease.ps1>

**Posting tools, yEnc and PAR2 libraries**

- [nyuu] Nyuu `lib/article.js`, `config.js`, `help-full.txt`: <https://raw.githubusercontent.com/animetosho/Nyuu/master/lib/article.js> · <https://github.com/animetosho/Nyuu/blob/master/lib/article.js> · <https://raw.githubusercontent.com/animetosho/Nyuu/master/config.js> · <https://raw.githubusercontent.com/animetosho/Nyuu/HEAD/help-full.txt>
- [nyuu-obf] Nyuu-Obfuscation fork `lib/article.js`: <https://raw.githubusercontent.com/Antidote2151/Nyuu-Obfuscation/master/lib/article.js>
- [nyuu-wiki] Nyuu wiki, Usenet Uploaders: <https://github.com/animetosho/Nyuu/wiki/Usenet-Uploaders> · <https://raw.githubusercontent.com/wiki/animetosho/Nyuu/Usenet-Uploaders.md>
- [ngpost] ngPost `src/nntp/NntpArticle.cpp`, `src/utils/Yenc.cpp`, README: <https://raw.githubusercontent.com/mbruel/ngPost/master/src/nntp/NntpArticle.cpp> · <https://github.com/mbruel/ngPost/blob/master/src/nntp/NntpArticle.cpp> · <https://raw.githubusercontent.com/mbruel/ngPost/master/src/utils/Yenc.cpp> · <https://raw.githubusercontent.com/mbruel/ngPost/master/README.md> · <https://github.com/mbruel/ngPost>
- [newsmangler] newsmangler `postmangler.py`; GoPostStuff `article.go`: <https://raw.githubusercontent.com/madcowfred/newsmangler/master/newsmangler/postmangler.py> · <https://raw.githubusercontent.com/madcowfred/GoPostStuff/master/article.go> · <https://github.com/madcowfred/newsmangler> · <https://github.com/madcowfred/gopoststuff>
- [newsup] NewsUP `lib/NewsUP/yEnc.pm`, `Utils.pm`: <https://raw.githubusercontent.com/demanuel/NewsUP/master/lib/NewsUP/yEnc.pm> · <https://raw.githubusercontent.com/demanuel/NewsUP/master/lib/NewsUP/Utils.pm>
- [newspost] newspost `enc/yencode.c`: <https://raw.githubusercontent.com/PietjeBell88/newspost/master/enc/yencode.c> · <http://github.com/PietjeBell88/newspost>
- [sanguinews] sanguinews `lib/sanguinews/nntp_msg.rb`: <https://raw.githubusercontent.com/tdobrovolskij/sanguinews/master/lib/sanguinews/nntp_msg.rb> · <https://github.com/tdobrovolskij/sanguinews>
- [nntpposter] nntpPoster `Util/conf/Default.ini`: <https://raw.githubusercontent.com/boranblok/nntpPoster/master/Util/conf/Default.ini>
- [nxg-upper] NxG Upper README: <https://raw.githubusercontent.com/Tensai75/nxg-upper/main/README.md>
- [nxg-go] nxg (Go) `nxg.go`: <https://raw.githubusercontent.com/javi11/nxg/main/nxg.go>
- [postie] Postie obfuscation docs and Go package: <https://postie.kipsilabs.top/docs/obfuscation> · <https://raw.githubusercontent.com/javi11/postie/main/docs/docs/obfuscation.md> · <https://pkg.go.dev/github.com/javi11/postie@v0.0.3>
- [parpar] ParPar `help-full.txt`, `bin/parpar.js`, `lib/par2gen.js`: <https://raw.githubusercontent.com/animetosho/ParPar/master/help-full.txt> · <https://raw.githubusercontent.com/animetosho/ParPar/master/bin/parpar.js> · <https://raw.githubusercontent.com/animetosho/ParPar/master/lib/par2gen.js> · <https://github.com/animetosho/parpar>
- [par2cmd] par2cmdline `src/creatorpacket.cpp`: <https://raw.githubusercontent.com/Parchive/par2cmdline/master/src/creatorpacket.cpp>
- [par2j] MultiPar par2j `source/par2j/create.c`: <https://raw.githubusercontent.com/Yutaka-Sawada/MultiPar/master/source/par2j/create.c>
- [node-yencode] node-yencode README: <https://raw.githubusercontent.com/animetosho/node-yencode/master/README.md> · <https://github.com/animetosho/node-yencode/blob/master/README.md>
- [rapidyenc] rapidyenc `src/encoder_common.h`: <https://raw.githubusercontent.com/animetosho/rapidyenc/master/src/encoder_common.h>

**NNTP clients and servers**

- [willglynn] willglynn/nntp `nntp.go`: <https://raw.githubusercontent.com/willglynn/nntp/master/nntp.go>
- [spottarr] Spottarr ADR 0006, XZVER/XZHDR zlib overview compression: <https://raw.githubusercontent.com/Spottarr/Usenet/main/docs/adr/0006-xzver-xzhdr-zlib-overview-compression.md>
- [pynntp] pynntp `nntp/nntp.py`: <https://github.com/greenbender/pynntp/blob/main/nntp/nntp.py>
- [nntplib] CPython 3.12 `Lib/nntplib.py`: <https://raw.githubusercontent.com/python/cpython/3.12/Lib/nntplib.py>
- [inn] INN `innd/innd.c`, `nnrpd/post.c`, `lib/messageid.c`: <https://raw.githubusercontent.com/InterNetNews/inn/main/innd/innd.c> · <https://github.com/InterNetNews/inn/blob/main/nnrpd/post.c> · <https://raw.githubusercontent.com/InterNetNews/inn/main/lib/messageid.c>
- [cleanfeed] Cleanfeed filter and FAQ: <https://raw.githubusercontent.com/crooks/cleanfeed/master/cleanfeed> · <http://www.mixmin.net/cleanfeed/faq.html>
- [perl-nocem] perl-nocem(8), NoCeM-on-spool for INN: <https://manpages.debian.org/testing/inn2/perl-nocem.8.en.html>

**Downloaders, Sonarr/Radarr and other clients**

- [hellanzb] hellanzb `NewzbinDownloader.py`, `NZBLeecher/ArticleDecoder.py`: <https://github.com/pjenvey/hellanzb/blob/master/Hellanzb/NewzbinDownloader.py> · <https://github.com/pjenvey/hellanzb/blob/master/Hellanzb/NZBLeecher/ArticleDecoder.py> · DirectNZB endpoint <http://www.newzbin.com/api/dnzb/>
- [www-newzbin] WWW::Newzbin 0.03, Perl wrapper for Newzbin's FileFind and DirectNZB APIs: <https://metacpan.org/release/CHRISN/WWW-Newzbin-0.03/view/lib/WWW/Newzbin.pm>
- [nzbmonkey] NZB Monkey `src/nzbmonkey.py`: <https://raw.githubusercontent.com/nzblnk/nzb-monkey/master/src/nzbmonkey.py>
- [nzbmonkey-go] NZB Monkey Go wiki, [DIRECTSEARCH]: <https://raw.githubusercontent.com/wiki/Tensai75/nzb-monkey-go/%5BDIRECTSEARCH%5D.md>
- [wtfnzb] WtFnZb-Renamer.py gist, the three [PRiVATE]-[WtFnZb] subject layouts: <https://gist.github.com/LukeChannings/49625879cb6310d68fb4cabb4ca98766>
- [aiostreams] AIOStreams commit 06513a2, N3wZ-prefixed WtFnZb subjects: <https://github.com/Viren070/AIOStreams/commit/06513a2898e51e224ada7e4491ea62ef474a4092>
- [nzb-rs] nzb-rs (Rust) `src/parser.rs`: <https://docs.rs/crate/nzb-rs/latest/source/src/parser.rs>
- [hydra] NZBHydra2 `DuplicateDetector.java`, `baseConfig.yml`: <https://raw.githubusercontent.com/theotherp/nzbhydra2/master/core/src/main/java/org/nzbhydra/searching/DuplicateDetector.java> · <https://raw.githubusercontent.com/theotherp/nzbhydra2/master/core/src/main/resources/config/baseConfig.yml>
- [sonarr] Sonarr `Parser.cs`, `ReleaseGroupParser.cs`; Radarr `Parser.cs`: <https://raw.githubusercontent.com/Sonarr/Sonarr/develop/src/NzbDrone.Core/Parser/Parser.cs> · <https://raw.githubusercontent.com/Sonarr/Sonarr/v5-develop/src/NzbDrone.Core/Parser/Parser.cs> · <https://raw.githubusercontent.com/Sonarr/Sonarr/develop/src/NzbDrone.Core/Parser/ReleaseGroupParser.cs> · <https://raw.githubusercontent.com/Radarr/Radarr/develop/src/NzbDrone.Core/Parser/Parser.cs>
- [nzbtomedia] nzbToMedia `core/scene_exceptions.py`: <https://raw.githubusercontent.com/clinton-hall/nzbToMedia/master/core/scene_exceptions.py>
- [spotweb] Spotweb `Services_Format_Parsing.php`: <https://raw.githubusercontent.com/spotweb/spotweb/master/lib/services/Format/Services_Format_Parsing.php>

**Media**

- [mediainfo] MediaInfoLib `File_Mk.cpp`: <https://raw.githubusercontent.com/MediaArea/MediaInfoLib/master/Source/MediaInfo/Multiple/File_Mk.cpp>
- [mkvmerge] mkvmerge manual: <https://mkvtoolnix.download/doc/mkvmerge.html>
- [exiftool-qt] ExifTool `lib/Image/ExifTool/QuickTime.pm`: <https://raw.githubusercontent.com/exiftool/exiftool/master/lib/Image/ExifTool/QuickTime.pm>
- [exiftool-riff] ExifTool RIFF tags: <https://exiftool.org/TagNames/RIFF.html>
- [exiftool-exif] ExifTool EXIF tags: <https://exiftool.org/TagNames/EXIF.html>
- [x264] x264 `encoder/set.c`: <https://raw.githubusercontent.com/mirror/x264/master/encoder/set.c>
- [x265] x265 `encoder/encoder.cpp`, `common/version.cpp`: <https://raw.githubusercontent.com/videolan/x265/master/source/encoder/encoder.cpp> · <https://raw.githubusercontent.com/videolan/x265/master/source/common/version.cpp>
- [movenc] FFmpeg `libavformat/movenc.c`; L-SMASH `cli/muxer.c`: <https://raw.githubusercontent.com/FFmpeg/FFmpeg/master/libavformat/movenc.c> · <https://raw.githubusercontent.com/l-smash/l-smash/master/cli/muxer.c>
- [ffmpeg-sig] FFmpeg-devel, PATCHv2 adding the MPEG-7 video `signature` filter (April 2016): <https://ffmpeg.org/pipermail/ffmpeg-devel/2016-April/193090.html>
- [aegisub] Aegisub `src/subtitle_format_ass.cpp`: <https://raw.githubusercontent.com/Aegisub/Aegisub/master/src/subtitle_format_ass.cpp>
- [mutagen] mutagen `flac.py`, `mp3/_util.py`: <https://raw.githubusercontent.com/quodlibet/mutagen/main/mutagen/flac.py> · <https://raw.githubusercontent.com/quodlibet/mutagen/main/mutagen/mp3/_util.py>
- [eac] eac_logsigner `eac.py`: <https://raw.githubusercontent.com/puddly/eac_logsigner/master/eac.py>
- [pycdlib] pycdlib `headervd.py`, `udf.py`: <https://raw.githubusercontent.com/clalancette/pycdlib/master/pycdlib/headervd.py> · <https://raw.githubusercontent.com/clalancette/pycdlib/master/pycdlib/udf.py>
- [unrar] unrar `headers.hpp` (mirror): <https://raw.githubusercontent.com/pmachapman/unrar/master/headers.hpp>
- [oshash] OpenSubtitles oshash: <https://github.com/opensubtitles/oshash>
- [vpdq] vpdq 0.0.5 (PyPI): <https://pypi.org/project/vpdq/0.0.5/>

##### Paper

- [giac] Lachniet, A Forensic Primer for Usenet Evidence (SANS GIAC GCFA, 2006, revised 2008): <https://www.giac.org/paper/gcfa/2676/forensic-primer-usenet-evidence/114938>
- [maier] Maier, Feldmann, Paxson, Allman, On Dominant Characteristics of Residential Broadband Internet Traffic (IMC 2009): <https://www.icir.org/vern/papers/imc102-maier.pdf>
- [schneider] Schneider, Analysis of New Trends in the Web from a Network Perspective (TU Berlin PhD thesis 2010; ch. 7 on NNTP traffic): <https://depositonce.tu-berlin.de/items/ad2fc6a3-cb15-44b7-8be3-b7069c391eba>
- [surmacz] Surmacz, Remote Measurement of NNTP Internet Traffic (MEASUREMENT 2009): <https://www.measurement.sk/M2009/proceedings/214_Surmacz.pdf>
- [ingvoldstad] Ingvoldstad, Handling Information Overload on Usenet: Advanced Caching Methods for News (Oslo thesis, 2001): <https://core.ac.uk/download/30827231.pdf>
- [usenetdht] Sit, Morris, Kaashoek, UsenetDHT: A Low-Overhead Design for Usenet (NSDI 2008); Sit, Dabek, Robertson, UsenetDHT: A Low Overhead Usenet Server (IPTPS 2004): <https://static.usenix.org/event/nsdi08/tech/full_papers/sit/sit_html/index.html> · <https://pdos.csail.mit.edu/papers/usenetdht%3Aiptps04.pdf>
- [picturing] Turner, Smith, Fisher, Welser, Picturing Usenet: Mapping Computer-Mediated Collective Action (JCMC 2005): <https://academic.oup.com/jcmc/article/10/4/JCMC1048/4614533>
- [nilsimsa] Damiani et al., An Open Digest-based Technique for Spam Detection (c. 2004): <https://spdp.di.unimi.it/papers/pdcs04.pdf>
- [judo] Pitsillidis et al., Botnet Judo: Fighting Spam with Itself (NDSS 2010): <https://www.ndss-symposium.org/wp-content/uploads/2017/09/pits.pdf>
- [rao] Rao and Rohatgi, Can Pseudonymity Really Guarantee Privacy? (USENIX Security 2000): <https://static.usenix.org/events/sec00/full_papers/rao/rao_html/index.html>
- [cuevas] Cuevas et al., Is Content Publishing in BitTorrent Altruistic or Profit-Driven? (CoNEXT 2010): <https://www.kom.tu-darmstadt.de/papers/CKC+10.pdf>
- [liang] Liang, Kumar, Xi, Ross, Pollution in P2P File Sharing Systems (INFOCOM 2005): <https://cse.engineering.nyu.edu/~ross/papers/pollution.pdf>
- [pdq] Dalins, Wilson, Boudry, PDQ & TMK+PDQF: A Test Drive of Facebook's Perceptual Hashing Algorithms (arXiv 1912.07745): <https://ar5iv.labs.arxiv.org/html/1912.07745>
- [haitsma] Haitsma and Kalker, A Highly Robust Audio Fingerprinting System (ISMIR 2002): <https://archives.ismir.net/ismir2002/paper/000014.pdf>
- [lbfs] Muthitacharoen, Chen, Mazières, A Low-bandwidth Network File System (SOSP 2001): <https://www.scs.stanford.edu/nyu/02fa/sched/lbfs.pdf>
- [magnetdb] Seidenberger, Pursell, Maiti, MagnetDB: A Longitudinal Torrent Discovery Dataset with IMDb-Matched Movies and TV Shows (2025): <https://arxiv.org/html/2501.09275v1>
- [gloe] Gloe, Fischer, Kirchner, Forensic analysis of video file formats (DFRWS EU 2014): <https://dfrws.org/presentation/forensic-analysis-of-video-file-formats/>

##### Forum and reddit

- [sab-forum] SABnzbd forum, deobfuscation test cases: <https://forums.sabnzbd.org/viewtopic.php?p=122040>
- [sab-1682] SABnzbd issue #1682 (and #1768): <https://github.com/sabnzbd/sabnzbd/issues/1682>
- [sab-1795] SABnzbd issue #1795: <https://github.com/sabnzbd/sabnzbd/issues/1795>
- [nyuu-146] Nyuu issue #146, the Message-ID clock and STAT 223 with truncated bodies: <https://github.com/animetosho/Nyuu/issues/146>
- [nzedb-2359] nZEDb issue #2359: <https://github.com/nZEDb/nZEDb/issues/2359>
- [nzedb-2494] nZEDb issue #2494: <https://github.com/nZEDb/nZEDb/issues/2494>
- [nzedb-2568] nZEDb issue #2568: <https://github.com/nZEDb/nZEDb/issues/2568>
- [kd476] newznab-tmux-kd issues #476 and #481: <https://github.com/KurzonDax/newznab-tmux-kd/issues/476> · <https://github.com/KurzonDax/newznab-tmux-kd/issues/481>
- [kd1088] newznab-tmux-kd issue #1088: <https://github.com/KurzonDax/newznab-tmux-kd/issues/1088>
- [subliminal] subliminal issue #538, OpenSubtitles: don't rely on hash that much: <https://github.com/Diaoul/subliminal/issues/538>
- [newsbin] Newsbin forum, obfuscated subjects in the wild and what PAR2 recovers: <https://forums.newsbin.com/viewtopic.php?p=189700> · <https://forums.newsbin.com/viewtopic.php?p=222097>
- [reddit] r/usenet, header-based vs source-based indexing; r/newznab, per-RAR hashes in NZBs (redlib mirrors): <https://axebps-redlib.hf.space/r/usenet/comments/1emgbgp/im_trying_to_learn_morehelp_me_understand> · <https://pol1.lr.ggtyler.dev/r/newznab/comments/1dfuqi/a_worthwhile_modification/c9qa2h3/?context=3>
- [hashcat] hashcat forum, Decrypting usenet headers: <https://hashcat.net/forum/thread-5896.html>
- [pcreview] Highwinds/Easynews injection headers in a quoted article (pcreview.co.uk, 2014): <https://www.pcreview.co.uk/threads/cool-add-on-for-chrome-html5.4067674/>
- [acoustid] Video identification with AcoustID (acoustid group, 2021): <https://groups.google.com/g/acoustid/c/C0QPEqkkpxk>

##### Usenet

- [newznzb-sig] newz[NZB] uploader signature header on a real obfuscated post: <https://raw.githubusercontent.com/scriptzteam/Usenet_-_Verify-Signature-newzNZB/master/EXAMPLE_WEB.BROWSER> · <https://raw.githubusercontent.com/scriptzteam/Usenet_-_Verify-Signature-newzNZB/master/README.md>
- [anon-server] alt.privacy.anon-server, hashed NNTP-Posting-Host and posting-host: <https://groups.google.com/g/alt.privacy.anon-server/c/XI1DhRI0wr0>
- [nsr-2008] news.software.readers (January 2008), how providers encode or hash X-Trace and posting-host: <https://groups.google.com/g/news.software.readers/c/cBuXeYBT6c0>

##### IRC

No public channel logs were found; these are the pre-bot scrapers, scripts and schemas.

- [nzedb-irc] nZEDb `nzedb/IRCScraper.php`: <https://raw.githubusercontent.com/nZEDb/nZEDb/master/nzedb/IRCScraper.php>
- [nzedb-ircbots] nZEDb_IRC_Bots `IRCScraper.php`, predb schema and request-channel announce formats: <https://github.com/nZEDb/nZEDb_IRC_Bots/blob/master/PHP/Classes/IRCScraper.php>
- [prenet] prenet-4.tcl pre bot: <https://raw.githubusercontent.com/scriptzteam/SCENE-SCRiPTS/master/Scene/prenet-4.tcl>
- [topclient] TopClient and predb site SQL schemas: <https://raw.githubusercontent.com/scriptzteam/SCENE-SCRiPTS/master/TopClient-save-nfo-sfv-top-master/TopClient.sql> · <https://raw.githubusercontent.com/scriptzteam/SCENE-SCRiPTS/master/website-predb-master/table.sql>

##### Archive

- [titz] Olaf Titz, draft X-Trace protocol (inn-workers, July 2000): <https://lists.isc.org/pipermail/inn-workers/2000-July/003269.html>
- [inn-2011] inn-workers, October 2011, rewriting the IP in INN's X-Trace: <https://lists.isc.org/pipermail/inn-workers/2011-October/017940.html>
- [inn-2014] inn-workers, November 2014, INN posting-account: <https://lists.isc.org/pipermail/inn-workers/2014-November/018334.html>
- [ietf-nntp-2009] ietf-nntp, November 2009, origins of COMPRESS GZIP and XZVER: <https://lists.eyrie.org/pipermail/ietf-nntp/2009-November/006066.html>
- [newznzb-log] newzNZB Usenet Upload Logger, daily release-name logs (2017-11-17 file): <https://raw.githubusercontent.com/scriptzteam/newzNZB-Usenet-Upload-Logger/master/2017/11/17/newz%5BNZB%5D_uploads.txt>
- [geocities-yenc] yEnc FAQ for alt.binaries.sounds.music.classical (geocities.ws mirror, ca. 2002): <https://www.geocities.ws/altbinariessoundsmusicclassical/yenc.html>
- [newzbin-specs] Newzbin NZB Specs, versions 0.9 to 3.0 (copy of docs.newzbin.com): <https://www.uzantoreto.com/Newzbin:NZB_Specs>
- [fanzub] Fanzub anime indexer source and 2015-03-01 database dump (archive.org; a possible grouping test corpus, contents not inspected): <https://archive.org/metadata/fanzub> · <https://archive.org/download/fanzub/fanzub-source-code.zip/>
- [sfv-sample] Morgoth MP3 Releaser SFV (WIN-SFV32/QuickSFV header sample): <https://raw.githubusercontent.com/scriptzteam/SCENE-SCRiPTS/master/Others/Other/release-tools/Morgoth.MP3.Releaser.v3.2.0.WinALL-HiEM/Morgoth.MP3.Releaser.v3.2.0.WinALL-HiEM.sfv>

##### Other web pages

- [leo] Leo's Notes, Usenet (real header dumps of newzNZB posts): <https://leo.leung.xyz/wiki/Usenet>
- [rexum] Usenet retention and removal test (usenet.rexum.space, July 2026): <https://usenet.rexum.space/blog/usenet-retention-tests-july-2026>
- [tomsguide] Tom's Guide (2012), Usenet DMCA takedowns and retention: <https://tomsguide.com/us/Usenet-DMCA-Takedown-File-Retention-Piracy,news-16296.html>
- [chromaprint] Lalinsky, How does Chromaprint work? (2011): <https://oxygene.sk/2011/01/how-does-chromaprint-work/>
- [harley] Harley Hahn's Usenet Center, anonymous file sharing: <https://www.harley.com/usenet/file-sharing/05-anonymous-file-sharing.html>
- [ngprovider] ngprovider, Usenet anonymity: <https://www.ngprovider.com/usenet-anonymity.php>
- [big8] Big-8 wiki, Usenet headers: <https://www.big-8.org/wiki/Usenet_headers>
- [xnoarchive] Wikipedia, X-No-Archive: <https://en.wikipedia.org/wiki/X-No-Archive>
- [breidbart] Wikipedia, Breidbart Index: <https://en.wikipedia.org/wiki/Breidbart_Index>
- [care] FAQ: The Newsgroup Care Cancel Cookbook: <https://rosalind.home.xs4all.nl/faq-care.html>
- [envisional] Envisional, An Estimate of Infringing Use of the Internet (January 2011): <https://ics.uci.edu/%7Esjordan/courses/ics11/case_studies/Envisional-Internet_Usage-Jan2011-4.pdf>
- [perfect10] Perfect 10, Inc. v. Giganews, Inc. (9th Cir. 2017, No. 15-55500): <https://cases.justia.com/federal/appellate-courts/ca9/15-55500/9128711072/0.pdf>
- [newzbin-case] Twentieth Century Fox v Newzbin Ltd [2010] EWHC 608 (Ch): <https://caselaw.nationalarchives.gov.uk/ewhc/ch/2010/608>
- [patent] US 2006/0015555, Storage cluster server network: <https://uspto.report/patent/app/20060015555>
- [forensics-rar] Forensics Wiki, RAR: <https://forensics.wiki/rar/>
- [nzbindex] NZBIndex API, raw collections with per-file availableParts/totalParts: <https://nzbindex.com/api>
- [animetosho] AnimeTosho JSON feed, per-file hashes bridging torrents, NZBs and AniDB: <https://feed.animetosho.org/json?show=torrent&id=431894> · <https://animetosho.org/about/json>

#### 5. Dead ends

- Wayback Machine and its availability API unreachable (SITE_BLOCKED, proxy 403); yenc.org robots-disallowed (mirrors used).
- Reddit, old.reddit, pullpush blocked (two threads via redlib); most SABnzbd, NZBGet, Newsbin forum threads failed; forums.nzedb.com parked; no public IRC logs for #sabnzbd, #nzbget, #newznab, #nzedb.
- No academic work on correlating or deobfuscating binary posts beyond the GIAC primer [giac]; Kim et al. 2010 and Schneider ch. 7 [schneider] unreachable.
- Undocumented: Giganews `sv3-`/`DXC=` X-Trace, Postie's Message-ID domain and X-NXG payload, private indexers' hashing. [corrected] The sweep also listed the X-Received-Body-CRC algorithm here; it was verified afterwards (see 1.2).
- Last-segment yEnc crc32 → srrDB: wontfix (0/500 coverage, 0/53 hits).
- No indexer documents grouping by posting-account (INN omits it by default).
- predb.ovh live API robots-disallowed; srrDB `/details` schema undocumented.
- Moved code: SABnzbd `nzbstuff.py` → `nzb/*.py`; NNTmux `Blacklight/*` → `app/Services/*`; MKVToolNix on Codeberg; libdvdread/libbluray/libaacs paths 404. Current NNTmux lacks `nzb_guid` and predb hash tables.
- Still open: PDF/EPUB metadata, QuickPar and MP4Box strings, closed-source uploaders' Message-ID formats.

## Haiku research: elementary-stream metadata (video, HDR/DV, audio)
### Wave 1 (2026-11-02 06:56Z)
- MKV CodecPrivate (hvcC/avcC/av1C) alone gives profile/tier/level, chroma, bit depth and the SPS, so resolution, crop, SAR, fps, primaries/transfer/matrix come without reading a frame. Crates: scuffle-h265, scuffle-av1, h264-reader.
- x264/x265 put their full options string in an SEI in the first IDR frame: per-encode fingerprint (same string + same size = likely same encode; different string = different encode even with same name).
- Dolby Vision: MKV BlockAdditionMapping 0x41E4 carries dvcC/dvvC (profile, level, rpu/el/bl flags, compat id). dolby_vision crate 3.4.0 (MIT) parses the RPU: FEL vs MEL, CM v2.9 vs v4.0, L1..L11. Distinguishes P7 FEL remux from P8 WEB-DL.
- Audio: symphonia lacks AC-3/E-AC-3/DTS/TrueHD; small own parsers needed (bdinfo-rs-core is a reference). DD+ Atmos = EMDF 0x5838 payload 14; TrueHD sync 0xF8726FBA; DTS ExSS 0x64582025. FLAC STREAMINFO MD5 is a rename-proof audio content key.
- Details: research/stream-metadata/log.md and raw/

## Extract metadata from MKV, MP4 and ISO containers (haiku x3, container level)
Log + raw notes: research/container-metadata/
### Wave 1 (2026-11-02 07:02Z)
- MKV first 1-4 MB (mkvmerge layout) gives Info (Title, MuxingApp/WritingApp, Duration, SegmentUID), all Tracks (codec, CodecPrivate, language, names, flags, DefaultDuration=fps), Colour/MasteringMetadata/MaxCLL (HDR10), BlockAdditionMapping dvcC (DV profile/level/compat id), Chapters and Attachments. mkvmerge statistics tags (BPS, NUMBER_OF_FRAMES, NUMBER_OF_BYTES) and Cues sit at the END -> one tail article read gives exact per-track bitrate/frame count.
- MP4: all metadata is in moov; non-faststart files show only ftyp + mdat header up front, but mdat size gives the exact moov offset -> fetch that article. moov gives dvcC/dvvC, colr/mdcv/clli, btrt, sample tables (exact frames/fps/bitrate/GOP), hdlr name + ©too + compressorname (muxer fingerprint), iTunes ilst tags.
- Blu-ray: CERTIFICATE/id.bdmv has org_id@40 + 16-byte disc_id@44 (libbluray bdid_parse.c) = candidate same-disc key (per-pressing unverified). MPLS gives duration, per-stream languages/codecs, UHD dynamic range, chapters. DVD: libdvdread disc ID (MD5 over IFOs) = same-disc key. TS captures: SDT provider/service name + TDT/TOT time identify channel + capture date.
- Per-mux values (MKV SegmentUID, DateUTC, _STATISTICS_WRITING_DATE_UTC; MP4 mvhd creation_time when nonzero) separate byte-identical alts from remuxes.

## Extract metadata from 7z, RAR and archives (Opus + Haiku)
### Wave 1 (2026-11-02 06:58Z)
- Store-mode inner data offsets: RAR5 block_start+4+len(size vint)+hdr size; RAR4 block_start+HEAD_SIZE; 7z Copy at 32+PackPos. Lets container/stream threads map inner MKV bytes to articles.
- Part-1 detection without names: RAR5 main header flag 0x0002 (volume number) absent only in vol 1; RAR4 MHD_FIRSTVOLUME; 7z signature at offset 0.
- Uploader leaks: RAR5 extra 0x06 / RAR4 UOW (Unix owner name + uid/gid); 7z attr 0x8000 high-16 st_mode; zip version-made-by host OS.
- RAR5 BLAKE2sp on a first split part covers only that volume (not a content key). TWEAKED checksums make CRC key-dependent.
- 7z -mhe=on still leaves AES salt/IV/NumCyclesPower + header CRC plaintext = per-creation twin key from the last article.
- PAR2 packet MD5 covers bytes 32..end so packets in partial data can be validated individually.
Files: docs/research/archive-metadata/
### Wave 2 (2026-11-02 07:10Z)
- Tested mkvmerge v82 + ffmpeg 6.1.1: mkvmerge puts Cues then statistics Tags (BPS, NUMBER_OF_FRAMES, ...) at the END; Lavf puts Tags (ENCODER, per-track DURATION) at the FRONT. mkvmerge writes Duration as float32, Lavf float64 (muxer tell even with stripped WritingApp).
- Remux discriminator: MKV DateUTC and _STATISTICS_WRITING_DATE_UTC change every mux; mkvmerge "no_variable_data" WritingApp = test mode with byte-identical output. Same SegmentUID + DateUTC = same mux run (alt key across repacks).
- MP4: ffmpeg default puts moov at the end (MP4Box at the front); moov offset = mdat offset + mdat size -> one targeted article fetch. mvhd creation_time is 0 by default from ffmpeg, so only nonzero values are useful.
- ISO PVD (0x8000) and UDF AVDP (0x80000) offsets verified; both within the first 1 MB of an ISO.
### Wave 2 (2026-11-02 07:01Z) elementary-stream metadata
- x264 SEI UUID dc45e9bd-e6d9-48b7-962c-d820d923eeef and x265 UUID 2ca2de09-b517-47db-bb55-a4fe7fc2fc4e (source-verified). Both written in the first access unit only (unless --repeat-headers), so article 1 of a stored RAR has it. Options string = encode fingerprint; NVENC, libaom, SVT-AV1 write none.
- Dolby Vision from the first RPU (dolby_vision 3.4.0 crate): profile (5/7/8), FEL vs MEL (NLQ all-zero = MEL), CM v2.9 vs v4.0, L5 letterbox, L6 MaxCLL/MaxFALL, L9 primaries, L254. dvcC bytes: u8 major, u8 minor, u16 [profile 7 | level 6 | rpu | el | bl], byte4 [compat 4 | md_compression 2] (ffmpeg dovi_isom.c).
- Atmos/DTS:X rules (ffmpeg + MediaInfo source): DD+ addbsi eac3_extension_type_a + complexity index, or EMDF JOC payload 14; TrueHD num_substreams==4 && substream_info MSB; DTS:X XLL sync 0x02000850 / IMAX 0xF14000D0.
- Proxy note for other threads: docs.rs, matroska.org, gitlab, code.videolan.org return 403; static.crates.io .crate tarballs and raw.githubusercontent.com work for reading crate source.
### Wave 3 (2026-11-02 07:06Z) elementary-stream metadata
- Same-encode keys that survive a remux (MKV->MP4, re-mux with new tracks): hash of the first IDR NAL/OBU payload for video; the AC-3/E-AC-3 crc2 sequence (last 2 bytes of each syncframe; skip silent runs) for audio.
- Same-film keys: perceptual hash of a decoded keyframe (image_hasher/blockhash, crop letterbox, 32x32 luma rank hash so PQ and SDR still match) and audio fingerprints (rusty-chromaprint 0.3.0 MIT pure Rust; audiofp 0.4.5). Catch: no pure-Rust HEVC decoder exists (AV1 via rav1d, H.264 via openh264 vendored C), and a 2160p remux holds <1 s of audio in 5 MB, so fingerprints need mid-file article fetches.
- Tags from SEI (ffmpeg-verified constants): A/53 closed captions (T.35 0xB5/0x0031 "GA94"), film grain (SEI 19 / AV1 film_grain_params_present), 3D frame packing (SEI 45), HLG compat (SEI 147), interlace (PAFF/MBAFF, pic_struct).
- Proxy tip: crates.io API works with a User-Agent header.
### Wave 2-3 (2026-11-02 07:08Z)
- Empirical: truncated deflate / LZMA / LZMA2 streams decode to a correct prefix at ~1:1 (flate2, lzma-rust2 0.25). Compressed zip/7z still expose the inner MKV front from the first articles. 7z: decode from offset 32 with a big dict even without the end header (testing in wave 4).
- rars 0.12.3 (only pure-Rust compressed RAR decoder) yields 0 bytes on truncated files (whole-file parse gate); wave 4 tries to bypass it.
- ngPost defaults (verified): rar a -idp -ep1 -m0 [-v<N>m -hp<pass>]; 7z -mx0 -mhe=on. So ngPost RARs are store-mode.
- PAR2 creators: par2cmdline "Created by par2cmdline version X."; ParPar "ParPar v<ver> <arch> [https://github.com/animetosho/parpar]" (leaks CPU arch).
- RAR5 .rev first file lists FileSize + CRC32 of every data volume -> whole-volume CRCs without downloading volumes. RAR5 QO near the tail caches full file headers (listing from the last article). RR % is a vint on the RR service header.
- Atlas today parses only name/size/encryption/password-check from archives; CRC, mtime, host OS, method, dict, solid, RR, owner, volume flags are all unparsed.
### Wave 4 (2026-11-02 07:12Z) elementary-stream metadata
- Mid-file sampling plan for frames/audio fingerprints from a store-mode RAR: front 6 articles + 3-5 tail articles (Cues via front SeekHead, Tags, MP4 moov) + 3 windows at 25/50/75% (about 12 articles at 1080p, 40 at 2160p). Audio is interleaved, so a 10 s audio window costs full bitrate (2160p remux ~116 articles). Own MKV cluster resync walker needed (1F43B675 + E7 + known track number); matroska-demuxer can't start mid-file.
- Pure-Rust HEVC/H.264 decoders do exist after all: rust_h265 0.4.0 (MIT/Apache, Main10, 501k downloads), rusty_h265 0.6.2, rust_h264 0.6.0. Wave 5 is testing them on real samples.
- dec3 verified (MediaInfo): DD+ Atmos in MP4 = flag_ec3_extension_type_joc + joc_complexity_index after the substream list. MKV A_AC3/EAC3/TRUEHD/DTS CodecPrivate is usually empty, so scan frames.
### Wave 4 (2026-11-02 07:22Z)
- Same-film IDs embedded by taggers: Matroska tags IMDB, TMDB ("movie/123" | "tv/123"), TVDB/TVDB2, ISRC (IETF cellar tags draft-16); iTunes/Apple TV MP4 cnID (store catalog ID) and sfID (storefront country). When present they skip name matching entirely.
- PRIVACY: iTunes apID (buyer Apple ID, usually an email), purd (purchase date), ownr and Matroska PURCHASE_OWNER identify a buyer. Proposed rule: never display; store at most a salted hash for uploader linking.
- Disc ISOs: the BDMV small files sit just past the UDF structures (ImgBurn sample: index.bdmv at ~8.4 MB), so a 1 MB read isn't enough; the UDF tree points to them and 1-2 targeted articles get them. RAR volume 1 lists only the files whose data it holds.
### Wave 5 (2026-11-02 07:20Z) elementary-stream metadata, tested on generated samples
- Pure-Rust decode WORKS: rust_h265 0.4.0 / rusty_h265 0.6.2 decode HEVC 8/10-bit keyframes bit-exact vs ffmpeg (~10 ms/frame); rust_h264 0.6.0 bit-exact for H.264 8-bit. Perceptual hash (image_hasher, 32x32 luma) identical across 8/10-bit and 720p/1080p, 0-3/64 bits to 360p: usable "same film" frame key.
- Rust-only probe of the first 1 MiB matches ffprobe for profile/level/size/depth/colour/fps/MDCV/CLL (HEVC, AVC, AV1). x265 copies its options SEI and HDR10 SEIs into hvcC, so CodecPrivate alone gives the encode fingerprint.
- ffmpeg-muxed x265 MKVs carry no Matroska Colour primaries/transfer/MDCV: HDR detection must read SPS VUI + SEI 137/144 (Atlas currently reads only MKV Colour).
- rusty-chromaprint: FLAC vs AAC 128k score 0.992, unrelated 0. AC-3 crc2 sequence identical across MKV/MP4 remux.
### Wave 4-5 (2026-11-02 07:19Z)
- Partial decode of COMPRESSED archives works in pure Rust: 7z LZMA2 from offset 32 with lzma-rust2 (64 MiB dict, no end header needed; LZMA2 chunk carries lc/lp/pb); RAR5 via rars codec::rar50::Unpack50Decoder direct call (~1:1 output, loses <=64 KiB + partial block). RAR 2.0 eager decoder gives 0 (patchable); RAR 2.9/PPMd untested. Check EBML 1A45DFA3 at inner offset 0.
- RAR5 QO is per volume (only that volume's headers). 7-Zip does NOT sort by type by default (-mqs off).
- NZBGet/SABnzbd never parse archive headers from first articles; PHP zeebinz/rarinfo (nZEDb lineage) does, with a 1 MiB window.
- Uploader leaks from article 1: zip DOS time vs UT/NTFS UTC extra = uploader UTC offset; RAR4 local DOS time vs Usenet Date; RAR5 UOWNER names/uid; RAR STM Zone.Identifier HostUrl/ReferrerUrl when -os; RAR5 REDIR symlink targets; sub-second time precision = OS.
- SRR (0x69-0x6C, 0x71) and SRS (MKV EBML 0x6A75/0x6B75, SRSF/SRST) layouts verified in raw/w5b.
### Wave 5 (2026-11-02 07:33Z)
- FLAC audio in MP4 (dfLa) and MKV (A_FLAC CodecPrivate) carries STREAMINFO with total samples and an MD5 of the decoded audio: an exact, container-independent alt key for any post with a FLAC track, from the first article.
- MKV ChapterSkipType (v5) marks opening/end credits, recap and preview chapters; ChapterSegmentUUID links anime OP/ED segments in separate files (a post set that references external segment UIDs is incomplete without them).
- Atlas's own probe on a synthetic corpus: it picks the first (possibly hidden) edition's chapters, labels HDR10 as "PQ" when MasteringMetadata is absent (common for mkvmerge/Lavf), misses 10-bit depth without BitsPerChannel, and reads no Tags/attachments/bitrates. MP4 without faststart gives nothing until a tail fetch.

### Haiku 15-min posting behavior: Wave 2 (2026-11-02 07:20Z)
- Postie (github.com/kipsilabs/postie, docs postie.kipsilabs.top/docs/configuration) defaults to group_policy each_file + obfuscation_policy full (random filenames, subjects, dates, poster), message_id_format random, article 750000 bytes. A second documented cause of Batman-style per-file group splits besides ngPost.
- Post-check reposts: Postie re-posts a missing article after 10s-5m (max_reposts 1); juicenet-cli reposts failed articles on the next run. Same part number/size reappearing later under a new Message-ID is a session-link event, not a new post.
- Segment size per tool: Nyuu 700K, ngPost 716800, Postie 750000, NewsUP 768000 (NewsUP fixed From "NewsUP <NewsUP@somewhere.cbr>").
- Article numbers are per group per server and follow arrival (RFC 3977 s6); Diablo engines don't sync them (Gnus nntp-xref-number-is-evil). Use them only as a single-server arrival clock.
- Session splitting: BOCPD via Rust augurs-changepoint (docs.rs/augurs-changepoint), PELT offline, Kleinberg bursts.
- Newsbin dedupe = hash of first ~26K; fill check = same name + ~600K prefix (forums.newsbin.com/viewtopic.php?p=213364).
### Wave 2 (2026-11-02 07:32Z)
- Store-mode archives give the alt key with zero payload: RAR4 UnpSize in every split header, but FileCRC is the whole-file CRC only in the LAST volume's header (non-last volumes carry that chunk's CRC; unrar volume.cpp, per the Opus payload thread), RAR5 optional CRC32/BLAKE2sp, 7z SubStreamsInfo 0x0A CRCs. data_start formulas: RAR4 header_start + HEAD_SIZE; RAR5 start + 4 + len(vint) + header_size; inner offset = running sum of PackSize across SPLIT_BEFORE/AFTER chain; RR/QO service blocks must be skipped. Rust prior art: nzbdav-rar crate (no split joining). https://docs.rs/crate/nzbdav-rar/latest/source/src/rar5.rs
- Offline oracles: No-Intro/Redump DATs (CRC32/MD5/SHA1 + size) vs RAR inner CRC; NSRL RDS (MD5/SHA1) vs PAR2 full MD5. AniDB needs ed2k (full download), BEP52 needs full file: dead for front-article linking.
- Non-video early-byte keys: Ogg page serial (bytes 12-15, random per stream, RFC 3533); LAME tag music CRC16 @152; APE MD5 @36; ISO9660 PVD @0x8000 creation date (centiseconds) + application ID; CHD v5 sha1 @84; MOBI EXTH 113 ASIN.
- Detail: research/payload-fingerprints/haiku/w2*.md
### Wave 6 (2026-11-02 07:27Z) elementary-stream metadata, tested
- Dolby Vision RPU and HDR10+ parse end to end in Rust (dolby_vision 3.4.0, hdr10plus 2.1.5) on generated P8.1 CMv4.0 and HDR10+ profile B samples: profile, CM version, L2/L5/L6/L8/L9/L11/L254 and HDR10+ windows/percentiles/bezier match what was injected. ffmpeg writes no dvcC box, so detect DV from the RPU NAL (type 62), not only from container config.
- Mid-file sampling proven: from a 50% seek in 700 KB articles, a keyframe + 10 s of audio took 16 extra articles (via Cues 13-14) on a 720p file; the mid-file keyframe decodes bit-exact with rust_h264. MKV cluster validator must skip CRC-32 (0xBF) and Void (0xEC) before Timestamp.
- TrueHD major sync fully parsed in std Rust (channels/layout/rate match ffprobe).
### Wave 6 (2026-11-02 07:45Z)
- REMUX-PROOF ALT KEY (tested on 9 remux variants + a same-film re-encode control): frame payloads and codec config (avcC/hvcC) are byte-identical across mkvmerge, ffmpeg -c copy, MKV->MP4->MKV and different cluster sizes. Key = hash(codec config record) + hash(first video frame) + hash(first 8 video + first 16 audio frame payloads). ~200 KB at 1080p, fits the first article; audio-only keys collide with re-encodes so always include video. Only DateUTC and the two stats-date tags change between mkvmerge runs. Script: research/container-metadata/tools/altkey.py.
- DVD ISOs: VIDEO_TS.IFO and VTS_01_0.IFO sit inside the first 700 KB (genisoimage layout) -> DVDDiscID-style same-disc key from one article.

### Research post linking via yEnc headers (Opus 3x3), wave 2 (2026-11-02 07:25Z)
- PRNG linking: Nyuu-Obfuscation's From/Message-ID/Subject are ~111 consecutive V8 Math.random (xorshift128+) draws per article (Nyuu bin/nyuu.js rand template, fork bin/nyuu.js L841-843). xorshift128+ is GF(2)-linear, so ONE article should give the full state; then predict the session's other Message-IDs and STAT them, even unindexed ones. Solvers: https://gist.github.com/JorianWoltjer/233e4f4e9ca72be98f4b4314b338184b , d0nut v8_rand_buster. Unverified on real output.
- ngPost (mbruel) random From = std::rand()%62 seeded by 32-bit srand: brute-forceable seed links a session. Inference, unverified: Windows builds run header() on connection threads that never srand, so From strings follow MSVC seed-1 rand (predicted p1KaLmIg2kB7f@ngPost.com, Zp5Tm1qbi6dss@ngPost.com, ...), identical across users. Hydro74000 fork yEnc name uses a 32-bit-seeded thread_local mt19937.
- NxG posts self-verify: msgid sans '@'/'.' = 64-hex H; obfuscated Subject = sha256(H); From = P[10:15] <P[10:25]@P[30:45].P[50:53]> with P = sha256(Subject). https://raw.githubusercontent.com/javi11/nxg/main/nxg.go
- NewsUP (now demanuel/NewsUP) fakes 7 other tools' Message-ID grammars when obfuscating, but its yEnc layer gives it away (unpadded lowercase pcrc32, no part= in =yend, no Date header, article Subject lacks "yEnc", 768000 articles). Message-ID alone is not a tool identifier.
- postie (kipsilabs/postie) and Nyuu-Obfuscation share the {rand32}@{rand8}.{rand3} / {rand14} <{rand14}@{rand5}.{rand3}> templates; separate them by Date suffix (postie "+0000", Nyuu "GMT" backdated 0-6h), header order (postie random per article via Go map, Nyuu fixed), and yEnc name (Nyuu-Obf even-length lowercase hex 32-64).
- Reading servers (NewsDemon, XS News, NewsGuy, Easynews) add X-Received-Body-CRC + X-Received-Bytes. If the CRC is over the body only, it is a free per-part twin key from HEAD/XHDR across reposts. Needs a live check on two backbones.
- Usenet.Farm strips Path to "not-for-mail" but injects opaque X-Ufk / X-Ufhash (possible encrypted trace; check for stable prefix per account). Giganews X-Trace "sv3-<b64>!<b64>" and X-Usenet-Provider reveal the backbone behind resellers. Article numbers are per backbone family only.
- Details: research/yenc-header-forensics/opus/w2*.md

## Research post linking via payload fingerprints
### Wave 1 (2026-11-02 06:50Z)
- CRC32 algebra: crc_concat over yEnc pcrc32 + begin/end gives whole-file CRC even with no crc32=, and CRCs of shared intervals between posts split at different article sizes. https://gist.github.com/animetosho/942a96ab3299c98ee35663208e00737b
- PAR2 IFSC slice CRCs chained (+ zero-unpad tail) = whole-file CRC32 -> joins to yEnc crc32 with no payload download (SABnzbd par2file.py does this). IFSC per-slice MD5 runs = partial/repack overlap.
- PAR2 set ID is deterministic (MD5 of main packet: slice size + sorted file IDs; file ID = MD5(16kMD5||len||name)); rename changes it. PAR3 set IDs are random.
- Per-creation random values = twin keys from one decoded article: 7z start header NextHeaderOffset/Size/CRC, RAR5 -hp salt, RAR5 file encryption salt+IV.
- Tool fingerprints: PAR2 Creator string, packet ordering/redundancy, exponent ranges, slice size from vol file sizes in the NZB; Unicode filename packets and ParPar -f path leak real names.
- Dead: ssdeep/TLSH/CDC on encrypted or compressed media; eD2k/TTH/BTv2 need full downloads.
### Wave 2 (2026-11-02 07:25Z)
- One decoded article gives the inner alt key: RAR non-last volumes store the CRC32 of that volume's chunk, the LAST volume's file header stores the whole unpacked file CRC (unrar volume.cpp). So first article of the last volume = (name, size, crc32). In store mode the yEnc pcrc32s verify it via CRC prefix/suffix removal (identities checked vs zlib). No indexer does this (nzbdav-rar reads and drops the CRC). Only Nyuu writes crc32= on =yend; ngPost pcrc32 is unpadded.
- srrDB API live-tested: /v1/search/archive-size:<bytes> returned exactly one release; also archive-crc, isdbhash (=OpenSubtitles hash), store-real-crc. Unpacked size from vol 1 names scene releases even renamed/re-RARed. pyReScene usenet/srr_usenet.py builds an SRR from an NZB fetching only header segments.
- nZEDb matchPredbHash: obfuscated 32/40-hex names can be MD5/SHA1 of the predb title. Anime Tosho TSV dumps carry crc32k (CRC of first 1 KB) + size + ed2k + BTv2 per file.
- Evasion matrix: takedown bots match Message-IDs + text, not content. Per-upload random password (ngPost --auto_compress adds -hp) is the only technique that kills alt keys; RAR5 -p HMACs the inner CRC. Junk files break set-level keys, so link per file. Hardest-to-evade: ES fingerprint (SEI + first IDR), inner head MD5 + unpacked size, yEnc part crc+offset+size for twins.
- -hp fallback: inner size from volume-size arithmetic, then archive-size candidates.
### Wave 7 (2026-11-02 07:38Z) elementary-stream metadata, tested
- AC-3 audio fingerprints are pure Rust end to end: oxideav-ac3 decode + rusty-chromaprint matches the FLAC of the same audio at 0.99 with as little as 5 s, recovers a 3.2 s offset, and rejects unrelated audio.
- 2160p 10-bit HEVC keyframes decode bit-exact in pure Rust (~90 ms, ~250 MB peak); open-GOP CRA starts and MBAFF interlaced H.264 also bit-exact. MPEG-2 (DVD) via oxideav-mpeg12video is close (76 dB) but not exact.
- Dolby Vision: 8.1 and 8.4 RPUs are identical; only the dvcC compat id or HLG transfer separates them. CM v4.0 = any CMv4 level present. Per-stream DV fields are fixed from the first RPU; L1/L5/L8 change per shot.

## Research post linking via posting behavior (Opus) — wave 2
- ngPost/Nyuu post files in series: group-per-file posts TILE in time (end of file k ~ start of k+1), not overlap. Join end-to-start; total rate constant. Source: ngPost PostingJob.cpp 640-760, Nyuu lib/fileuploader.js.
- Diablo multi-engine clusters don't sync article numbers; sample HWM from one backend. INN numbers crossposts atomically. RFC 5537: server adds Date when missing (ngPost).
- Fill/retry tells: Nyuu repost keeps ms ts; same (part,pcrc32,begin) under 2 IDs = one segment; nzbrefresh same Message-ID with new Date; nzb-repair part>1 begin=1 and subject - "".
- Scoring: Fellegi-Sunter + TF, one combined tool-fingerprint field, Poisson time-closeness u, gated union-find with vetoes (Rust ena), PAR2 md5_16k chains for reposts. Notes: research/posting-behavior/wave-02-*.md

## Haiku 15-min research: yEnc headers
### Wave 2 (2026-11-02 07:33Z)
- yEnc fingerprint tuple (from decoder source: yencoding crate, Plan 9 ydec.c, Go nntp): field order, tab vs space before name=, CRC key set (pcrc32/crc32/bare crc=), hex case/padding/0x, size= on =yend, total=, begin=0, column-0 TAB/dot escaping, SPACE escaping, NNTP dot-stuffing vs yEnc-escaped dot, line ending in "=" (normal splitters never emit it; before =yend means truncated/patched), bare CR/LF/NUL, text before =ybegin, unknown extra keys. Anchor name= (it can contain "=" and spaces, e.g. name=-=DUMMY=- abc.par). Watch the yenc 0.2.2 crate bug: it never resets its hasher, giving false pcrc32 mismatches on parts 2+.
- X-Received-Body-CRC: one real sample (Usenet.Farm 2018): X-Received-Bytes 6481, X-Received-Body-CRC 2720885501 (decimal uint32, input unverified), Path just not-for-mail, Date "Thu, 28 Jun 18 15:25:10 UTC" (2-digit year, invalid zone). https://leo.leung.xyz/wiki/Usenet . Still needs a live HEAD test on two known reposts.
- Highwinds injection Path token "fxNN.iad.POSTED!not-for-mail" is a candidate backbone fingerprint; Farm strips Path. Injection-Info path-identity should match Path's injecting token. INN Bytes counts headers too, so it is not a body key.
- NxG: Message-ID_n = sha256("<NXG header>:data|par2:n") split 40@21.3, so one article's NXG header derives every other part's ID (STAT them). postie "partial" obfuscation keeps poster + yEnc name constant per post. https://github.com/Tensai75/nxg-upper , https://postie.kipsilabs.top/docs/obfuscation
- No mainstream poster sets References or Cancel-Lock (Cancel-Lock is per-article HMAC, not linkable). nZEDb collection hash includes From, so random From splits collections.
### Wave 7 (2026-11-02 08:05Z)
- DVDDiscID confirmed against libdvdread 6.1.3: MD5(VIDEO_TS.IFO || VTS_01_0.IFO || ... || VTS_0N_0.IFO), whole files, N = title-set count. Exact same-disc key; later VTS IFOs can sit 20+ MB in, so walk the ISO directory and fetch them by offset.
- Dolby Vision: mkvmerge v82 writes the DV config into BlockAdditionMapping (dvvC for P8.1, dvcC for P5), readable in the first article. ffmpeg 6.1 never writes dvcC/dvvC into MP4, so DV in ffmpeg-made MP4s can only be found from the bitstream RPUs.
- Home-made ISO tell: genisoimage writes app id "GENISOIMAGE" and UDF implementation "*genisoimage"; pressed-disc rips carry mastering-tool IDs (untested).
- Field catalog part 1 (Matroska, 231 fields + 28 derived, each with cost/linking/Atlas status): research/container-metadata/catalog/mkv.yaml.
### Wave 8 (2026-11-02 07:32Z) elementary-stream metadata
- Reaching the bitstream from article ranges works for every container: MKV cluster resync (wave 6), TS/M2TS (0x47 resync at 188 or 192, PAT/PMT, PES) gets a decodable IRAP within 1-2 articles, MP4 moov-at-end/faststart/fragmented need 2 articles. Keyframe bytes are identical across MKV/MP4/TS remuxes, so the first-IRAP hash is a cross-container alt key. Accept CRA (NAL 21) not just IDR: x265 open-GOP files only have an IDR at the start.
- MKV header stripping (ContentEncoding algo 3) must be undone before frame parsing.
- Egress note for other threads: samples.ffmpeg.org, archive.org, blender.org, Netflix Open Content are blocked; raw.githubusercontent.com and npm tarballs work.
### Wave 9 (2026-11-02 07:38Z) elementary-stream metadata
- Same-film frame key, measured: 64-bit pHash on letterbox-cropped, histogram-equalised luma. Same frame across re-encode (x265 720p 10-bit), 2.39 letterbox, brightness/gamma change, 25 fps PAL retime and PQ transfer: max 4 bits. Different shot of the same film: min 24 bits. Use <= 10/64 with a 2-of-3 vote over three frames. Without the crop a letterboxed release is 19 bits away.
- Dolby Vision on 27 real dovi_tool RPU fixtures: dolby_vision 3.4.0 gets FEL/MEL, profile and CM version right on all; it rejects RPUs with Level 253 (fixed only on dovi_tool main). L6 CLL/FALL of 0 means unknown.
### Wave 10 (2026-11-02 07:52Z) elementary-stream metadata, final
- Final report: https://claude.ai/artifact/ByUTRbbX7sHy2H78JK9iVo ; YAML catalog research/stream-metadata/catalog.yaml ; Rust prototype research/stream-metadata/code/streamprobe/ (all front fields as JSON, matches ffprobe on 12 files).
- Atlas's front articles already hold every video/HDR/DV/audio field; only the parse is missing. Atlas reads HDR/bit depth only from MKV Colour, which ffmpeg and mkvmerge leave empty for x265, and DV only from dvcC, which ffmpeg never writes.
- TrueHD Atmos = FBA sync and substream_info bit 7, verified with the CRC on 52 real files including 3 Atmos.
- Keep a hand-written EBML walker: matroska-demuxer follows SeekHead to the file end and fails on partial downloads.
### Wave 9 (2026-11-02 08:33Z)
- Container rows for the Linking Signals Map: research/container-metadata/catalog/signals.md (43 signals with proves / cost / Atlas status / notes, plus a JSON array for ingestion). 6 collected, 4 buggy, 33 not collected; 5 privacy rows (iTunes apID/purd/ownr, Aegisub local path, MKV ENCODED_BY/PURCHASE_OWNER, segment filenames, ISO owner fields) are hash-or-drop.
- HDR10+ has a header-level signal in MKV: BlockAdditionMapping BlockAddIDType 4 (ITU-T T.35) on the video track (FFmpeg matroska.h). Atmos has a container signal only in MP4 (dec3 JOC flag); DTS:X and IMAX Enhanced are bitstream-only.
### Final (2026-11-02 08:05Z)
- Report doc: https://claude.ai/artifact/NSkmWVebA67M95ps5NLqP1 ; YAML catalog (267 fields, Atlas status per field): docs/research/archive-metadata/archive-field-catalog.yaml ; prototypes in research/archive-metadata/proto/.
- RAR5 -hp: HEAD_CRYPT (salt+check) byte-identical across all volumes of a set (rars writer; WinRAR unverified) -> groups encrypted random-named volumes.
- 7z AES: 7-Zip writes NO salt by default (ResetSalt is commented out in 7zAes.cpp); the 16-byte IV is the per-archive key.
- Atlas drops per-file CRC32 in every archive listing (RAR5/RAR4/zip CD/7z substreams); confirmed bugs: probe RAR4 Unicode names ASCII-only, stale encryption columns, 7z encoded header LZMA-only, 256-row view cap hides archive rows behind PAR2 rows, SFV take(3) starvation.
### Wave 10 / final (2026-11-02 08:58Z)
- Report: https://claude.ai/artifact/Wb8pQ6mzroCGo38pZvLcVK. YAML catalog in research/container-metadata/catalog/ (mkv 231+28, mp4 234+20, disc_other 292, additions 31, signals.md 43 rows).
- Byte budgets (synthetic grainy test, pessimistic): first keyframe 576 KB at 1080p, 970 KB at 2160p; the full 8-video+16-audio alt key needs 1.6 MB / 5 MB, so the practical alt key is codec config + first keyframe hash (1-2 articles). 10 MB of MKV font attachments push the first Cluster to 10.5 MB; use SeekHead to skip.

### Haiku 15-min posting behavior: Wave 3 (2026-11-02 08:05Z)
- Correction: Postie retries KEEP the Message-ID (checkLoop; regenerating would duplicate NZB segments) and crash recovery reuses manifest IDs. Under full obfuscation its Date is now minus uniform [0,6h) per article (local zone), From is `{14} <{14}@{5}.{3}>` new per segment, Message-ID `{32}@{8}.{3}`; two files post concurrently; each_file group = rand.Intn. (internal/poster/poster.go, read via summarizer)
- NewsUP --obfuscate sends each ARTICLE to a random subset of groups with an empty From: true per-article spray (bin/newsup.pl).
- juicenet-cli reposts Nyuu dump-failed-posts raw articles (same Message-IDs) at the start of the next run.
- INN nnrpd Injection-Info logging-data = nnrpd pid, i.e. one NNTP connection; posting-host host:ip (lists.isc.org/pipermail/inn-workers/2009-December/017367.html). A per-connection session key if backbones don't strip it; worth a check on our own HEADs.
- Spotnet spots (free.pt) carry X-User-Key RSA modulus (uploader identity) and <NZB><Segment> Message-ID of the release NZB: a ground-truth label set (github.com/spotnet/spotnet/wiki/Spot-Header-format).
- Uploader throughput anecdotes: Eweka ~1.3 MB/s per connection, Tweaknews ~100 Mbit cap (r/usenet l38e4d). Clock-skew fingerprinting (Polcak JUCS 21(9)) is weak.

### Wave 3 (2026-11-02 07:58Z), tagged Proves / Cost
- Provenance of re-packs (same source copy, different uploader): inner mtime + which time fields are stored (RAR5 HTIME flags, 7z CTime/ATime) | same source copy | free (last-volume header). unrar restores the archived mtime on extract and SABnzbd keeps it by default, so re-packs of a downloaded copy inherit it. Sources: php-rar unrar extract.cpp, sabnzbd newsunpack.py.
- Machine/uploader: RAR5 UOWNER user/group/uid/gid, HOST_OS, 7z Unix mode (umask) | same uploader | free.
- ngPost packing defaults: 42 MB volumes (-v42m), -idp -ep1 -m0, 17-char random archive names, 13-char passwords, 7z -mx0 -mhe=on | tool | free. Nyuu never makes RAR. https://raw.githubusercontent.com/mbruel/ngPost/HEAD/README.md
- Set matching (cross-seed prior art): per-file exact size lists, ignore nfo/sample/txt/subs, never compare totals (a RAR set can equal the unpacked total) | alt candidate | free. https://www.cross-seed.org/docs/basics/options , https://github.com/ccf-2012/seedcross/issues/20
- Verified per-creation IDs: PE CodeView RSDS GUID (same build), PDF trailer /ID[0] (same document, /ID[1] = revision). Ogg serial is regenerated by ffmpeg on every remux (libavformat/oggenc.c), so it only marks one mux event; correct offset is bytes 14-17.
- Detail: research/payload-fingerprints/haiku/w3*.md

## Research post linking via payload fingerprints — wave 3 (2026-11-02 07:55Z)
- RAR5 encrypted archives expose Lg2Count + 16 B salt + 16 B IV + 8 B PswCheck (+4 B csum) in the clear. One PBKDF2-HMAC-SHA256 run (2^15) per candidate verifies a password with no decryption. A table of passwords harvested from NZB meta/subjects links posts that reuse a password (fixed ngPost RAR_PASS, group signature passwords; GEN_PASS is fresh per post) and unlocks inner keys. unrar ConvertHashToMAC is reversible with the password, restoring the RAR5 -p inner CRC. ZIP AES has a 2 B verifier; 7z AES has none. Source: unrar crypt5.cpp.
- Zero-download inner size for -hp/-mhe posts from yEnc volume sizes: RAR5 -hp per-volume overhead ~254-302 B (16 B steps by filename length), giving ~5 candidate buckets ±15 B; two posts of the same file with different volume counts intersect to one value. 7z -mhe: one ~300 B window per archive. Expected false matches ~0.04 / 0.15 against ~2M releases. Untested guess: RAR5 -hp volume sizes ≡ 14 mod 16. No existing tool does this.
- Oracles: srrDB (no bulk dump, slow, cache lazily; sample-crc key), Anime Tosho dbexport (full TSV), nZEDb PreDB CSV dumps (title/size/files, discontinued), NSRL RDS / CIRCL Bloom as a negative filter, No-Intro/Redump DATs for ROMs, Spotnet spots as harvestable labeled NZBs.

## Haiku research: post linking via yEnc headers
### Wave 3 (2026-11-02 07:56Z)
- Nyuu defaults VERIFIED from master source (config.js:83-108, lib/article.js:43,102-108,133,198-228, lib/nzb.js:51-99): articleSize 716800, line 128, Message-ID `^<([A-Z][a-z]){12}-\d{13}@nyuu>$` (ms = genTime = Date), Date = toUTCString (GMT), User-Agent `Nyuu/<ver>`, default Newsgroups alt.binaries.test, =ybegin always has part= and total= (even single-part), crc32= only on the last part, lowercase pcrc32, NZB utf-8 + newzBin DOCTYPE, file date in unix seconds. keepMessageId false, but reposts from a raw dump keep the ID and genTime. This settles the 700K vs 716,800 conflict in the Linking Signals Map.
- NEW: server-assigned Message-IDs from Highwinds-family injectors: `NzZUA.117908$rY1.16813@fx45.am4`, `OzZUA.117909$rY1.86560@fx45.am4` (sabnzbd.github.io tests/test_zip.nzb). The counter goes up by one per article and the host is the injecting box, so this is a same-session sequence read from overview. Decoding it in wave 4.
- STAT co-removal = same release: UvA 2014-15 measured filesets removed together and correlated across providers, with a mean of 1 to 3.6 days (rp.os3.nl/2014-2015/p82/report.pdf). Lumen keys are research-only.

### Research post linking via yEnc headers (Opus 3x3), wave 3 (2026-11-02 07:58Z)
- TESTED: V8 Math.random state recovery from floor(x*62) outputs works. Each output fixes ~4-5 bits; 32 outputs give full rank 128 with plain GF(2) elimination (0.2 s, no z3); predicted 200 later outputs exactly across 3 cache refills (Node 22). The cache-offset ambiguity is resolved by trying all 64 offsets across a block boundary. So one Nyuu-Obfuscation article (~111 draws) should recover state and predict the session's other random From/Message-ID/Subject values. Code: research/yenc-header-forensics/opus/v8-solver/. Still to do: map Nyuu's charset order and allow a draw window for interleaved Math.random calls.
- ngPost correction: in obfuscated mode the random From is made per article in NntpArticle::header() on posting threads. Seed-1 MSVC LCG From sequence: p1KaLmIg2kB7f, Zp5Tm1qbi6dss, 92C31aPGJK9Lv, k7DMPDONXrwD2, yVLZrHBio74so. Whether Windows builds hit this is still unconfirmed (compiler unknown).
- CORRECTION: X-Received-Bytes / X-Received-Body-CRC are stamped at INJECTION by Highwinds (fxNN.iad.POSTED) and Usenet.Farm and travel with the article; they are not added by the reader. They are the same everywhere for one article, mark the injection network, and only match byte-identical re-injections. Their CRC input bytes are undocumented. https://www.electronics-related.com/showthread/sci.electronics.design/795853-7.php
- newzNZB uploads: Message-ID <unixtime.SESSION.part@PRiVATE>, fixed upload hash in subject "[8d2d235457976f] [47/69]", Verify-Signature-newzNZB + X-Newsposter: newzNZB. https://raw.githubusercontent.com/scriptzteam/Usenet_-_Verify-Signature-newzNZB/master/README.md
- Legacy posters from source: the yEnc 1.3 draft examples DO have trailing spaces. yEnc32 (Blinch) copies one after name= on =ybegin, escapes EVERY '.', uses 3-digit (001/050) + byte-count subjects and "X-NoArchive" (sic). PowerPost 2004 never writes total= or crc32= (even single-part), defaults to 640000-byte parts (MaxLines*128), leaves TAB unescaped, has a blank line before the terminating dot, and sends no Message-ID/Date/UA. newspost 2.2.1: User-Agent Newspost/2.2.1, no Message-ID. Spotweb: <32 alnum>@spot.net, Subject = md5(body), X-XML signatures. Many legacy tools send no Message-ID, so server-assigned IDs (e.g. Highwinds counters) become the session key.
- Details: research/yenc-header-forensics/opus/w3*.md

### Haiku 15-min yEnc headers: Wave 3 (2026-11-02 08:00Z) source-verified via registry.npmjs.org and proxy.golang.org (both work when GitHub is blocked)
- Nyuu 0.4.2: 716800 B, line=128, MsgID <([A-Z][a-z]){12}-\d{13}@nyuu>, header order Message-ID, Subject, From, Newsgroups, Date, Path (empty), User-Agent; Date toUTCString "GMT" and Date seconds == MsgID ms/1000 (mismatch means --date); =yend size part pcrc32 lowercase padded, crc32 last part only; escapes TAB/SPACE at col 0 and line end, '.' at col 0. Retries keep the same header string.
- ngPost (mbruel): 716800, <32 hex>@ngPost, headers From, Newsgroups, Subject, Message-ID only (no Date/UA/Path), Subject "name" (p/t) with no "yEnc", =yend size pcrc32 (lowercase, unpadded), no part=, no crc32.
- postie (javi11): 768000 default (750000 fallback), rapidyenc: =ypart begin end (no size), =yend size part pcrc32 lowercase padded, no crc32; Go map so header order is random; Date "UTC" zone name in v0.0.24 but RFC1123Z "+0000" in v0.0.35 (version marker); full obfuscation Date uniform in [now-6h, now]; X-Nxg off by default.
- nxg-upper: 768000, =ypart begin end size, =yend pcrc32 UPPERCASE + crc32 last, escapes 0x7F (non-standard), LF-only headers, never emits Newsgroups (map key bug), Date RFC1123 local zone abbrev.
- NxG: javi11 MsgID = sha256hex("<28-char hdr>:data|par2:n") -> 40@21.3; nxg-upper keys on its Comment instead (IDs incompatible). X-Nxg = hex(nonce||AES-256-GCM("fileNo:totalFiles:filename:partType:totalSize", key=header)).
- X-Received-Body-CRC: 2nd sample also Usenet.Farm (Nyuu post, X-Received-Bytes 740517 on a 716800 part, i.e. yEnc-encoded size) and Farm also adds X-Ufhash. Still needs the live two-provider test.

## Research post linking via posting behavior (Opus) — wave 3
- Sibling-ID prediction: NxG mode fully derivable (sha256 of header:type:n; seed = UnixNano near first Date, ~15.5 us/seed); newsmangler <10 STATs per sibling; NewsUP imitates other tools' Message-ID formats (watch for @gpoststuff misspelling). STAT: pipeline ~100 in flight; some servers need GROUP first; 430s are negative-cached.
- Session shape: equal RAR volumes -> identical last-article :bytes across groups; ngPost order par2 index -> rars -> pow2 vols; reorder width ~2N gives connection count.
- Clock: RAR4 DOS local mtime vs MKV DateUTC in one BODY -> uploader UTC offset; INN server-assigned IDs encode time+pid+counter per connection. Notes: research/posting-behavior/wave-03-*.md
### Wave 4 (2026-11-02 08:38Z), tagged Proves / Cost
- TESTED (par2cmdline 0.8.1 vs ParPar 0.4.6): the exponent-0 PAR2 recovery slice is the plain XOR of all input slices (base^0 = 1), so it is identical across file renames, file-order changes, new set IDs and different PAR2 tools, as long as the file bytes and slice size match | twin of the file set, rename/obfuscation-proof | 1 article of the first recovery volume (prefix compare). Same test: ParPar and par2cmdline produce the same set ID and byte-identical recovery slices; only the Creator packet differs. Renames always change the set ID and change exponent>=1 data only when they flip FileId sort order. Script + detail: research/payload-fingerprints/haiku/tools/par2_recovery_compare.py, haiku/w4c.md.
- Key-strength ladder: CRC32 alone never (50% birthday at ~77k, forgeable with 4 bytes); size alone or size+hash16k/moviehash = candidate; size+CRC32 = strong candidate; size+full MD5 (PAR2 FileDesc) + CRC32 = twin for non-adversarial use. No published exact-size collision rate exists; measure it on Atlas's own catalog. https://www.nayuki.io/page/forcing-a-files-crc-to-any-value , https://opensubtitles.github.io/oshash/
- yEnc has no time field and BitTorrent has no per-file mtime (BEP 47), so a re-pack's inner mtime equals download time for P2P-sourced copies and the original mtime for unrar-extracted Usenet copies (SABnzbd default) | origin type | free (archive header).

### Haiku 15-min posting behavior: Wave 4 (2026-11-02 08:32Z)
- NewsUP (lib/NewsUP/Utils.pm) Message-IDs: own "$sec$usec.$rand@$time.newsup"; with obfuscate it also mimics JBinUp "<32>@JBinUp.local" and Nyuu "<12>-$sec$usec@nyuu". Obfuscated subject is random 1-32 lowercase but yEnc name= keeps the real filename (Article.pm TODO). Per-article group spray is UNCONFIRMED (conflicting reads).
- Merge windows with sources: newznab 4h hold for incomplete releases; NNTmux forks delaytime 2h, collection_timeout 48h, late-header grace 72h (github.com/KurzonDax/newznab-tmux-kd/issues/1020); Nyuu post-check defaults check-delay 5s, retry-delay 30s, tries 2, repost once (Nyuu help.txt). Suggest: 4h hold for cross-group slices, 30-60 min rechecks, 72h expiry.
- Takedowns name Message-IDs, remove whole filesets together, none in the first 18h, average ~25h at Eweka (2015, rp.os3.nl/2014-2015/p82/report.pdf p.24). Lumen has no Usenet fields (dead end). Atlas can STAT its own catalog over time: co-removal validates a stitch.

### Haiku 15-min yEnc headers: Wave 4 (2026-11-02 08:37Z)
- More source-verified tools: GoPostStuff v0.2.0 (From, Newsgroups, Message-ID, X-Newsposter, Subject; no Date; <UnixNano$gps@gopoststuff>; pcrc32 %08X upper, never crc32; escapes col 0 + col 127, 129-char lines, no dot-stuffing on POST). ngPost Yenc.cpp escapes TAB/SPACE only at col 0 (line-end check is dead code), 129-char lines possible. New tool nasauthunder-cli (npm): <gcid[.n]@nasauthunder-v1.invalid>, Content-Type application/octet-stream, minimal escaping, 716800.
- Experiment (scripts in research/yenc-header-forensics/tools/): same tool + same payload gives a byte-identical body. Within one tool, encoded size of different same-size payloads has sd 140-200 B (~1/400 false match); across tools there are fixed offsets (Nyuu edge escaping +~113 B, nxg 0x7F +~2900 B), so encoded size is a within-tool filter only. :lines adds nothing.
- Usenet.Farm REWRITES Date: a Nyuu 0.3.8 post whose Message-ID ms is 05:58:28.703 (Nyuu writes "Fri, 17 Apr 2020 05:58:28 GMT") is served as "Fri, 17 Apr 20 05:58:28 UTC"; the 2018 Farm sample has the same 2-digit-year UTC form, and header casing is "Message-Id". So a backbone fingerprint is readable from the XOVER Date alone. (github spoved/nntp-client.cr via gh code search)
- X-Ufhash (Farm) is variable-length base64 (171 B and 192 B decoded), so it's an encrypted trace blob, not a fixed hash. X-Received-Bytes (740517 on a 716800 Nyuu part) counts the whole received article including headers, not the body alone.
- Tip: `gh api -X GET search/code -f q='...' -H 'Accept: application/vnd.github.text-match+json'` returns code snippets even when GitHub file fetches are blocked.

### Research post linking via yEnc headers (Opus 3x3), wave 4 (2026-11-02 08:40Z)
- Overview-only twin key (modelled, untested on real data): the yEnc escape count acts as a content hash that XOVER exposes. Same bytes + same tool + same line length give identical :lines and body bytes per part. :lines sd ~0.84 per 716800-byte part (~1.76 bits), header-normalized :bytes sd ~105 B (~8.7 bits); an ordered 70-part :lines sequence carries ~120 bits. Measured escape rate 1.580% (L=128). Presets are separable from (:lines, :bytes): e.g. 716800@128 -> 5692 lines, 640000 -> 5082, 750000 -> 5955, 768000 -> 6098; 384000@128 and 768000@256 collide on lines (3051) so bytes are needed. :bytes includes injected headers, so compare within one server. NNTmux never uses :lines. Model scripts: research/yenc-header-forensics/opus/overview-model/.
- Nyuu-Obfuscation draw layout (bin/nyuu.js, lib/article.js): per article build = Message-ID 43 (discarded) + Subject 32 + From 36 + Date 1 (+1 name length on a file's first part); the WIRE Message-ID is redrawn at send time (43, +43 per retry). Subject+From = 68 consecutive outputs, more than the 32 the solver needs. Charset A-Za-z0-9.
- NewsUP's own Message-ID style base36(rand(sec.usec)) leaks almost the full 48-bit drand48 state from ONE ID, and its imitation IDs (Nyuu/JBinUp/PowerPost/...) come from the same stream, so one real NewsUP ID can expose its fakes.
- ngPost Windows release is MinGW (libgcc_s_seh/libwinpthread, Qt 5.12.5), and msvcrt rand is per-thread, so the seed-1 From sequence (p1KaLmIg2kB7f, Zp5Tm1qbi6dss, ...) likely holds; no public hits yet. ngPost before v4.15 seeded srand with the ms timestamp, which is trivially brute-forced from the Date.
- Counter-bearing Message-IDs self-check against NZB segment numbers: PowerPost A&A <partNofM.{20}@powerpost2000AA.local> (20 chars may include & $), JBinUp <{19-20}_{file}o{total}@JBinUp.local>, JBinDown <...N-M@JBinDown.local>. NewsUP's fakes draw N and M independently (real capture part52of36...@powerpost2000AA.local), and its JBinUp fake lacks _NoM. TyEncoder 0.1.3/0.1.4: trailing space after name=, escapes every '.', blank line before =yend on exact line boundaries. Known-suffix list prior art: go-while/nntp-overview rescan_overview.go:633-644.
- Details: research/yenc-header-forensics/opus/w4*.md

## Research post linking via payload fingerprints — wave 4 (2026-11-02 08:37Z)
- TESTED on public RAR test archives (libarchive, python rarfile, sharpcompress): non-last split headers store the chunk CRC32, the last volume header stores the full unpacked CRC32 (RAR5 and RAR4). RAR5 -hp single-volume files are all ≡ 14 mod 16; non-last volumes of multi-volume sets are zero-padded to exactly the -v size, so expect only the last -hp volume to show it. Parsers (incl. -hp decrypt) in research/payload-fingerprints/opus-tools/.
- RAR5 Quick Open record: located via the main-header locator (verified), but it is per volume and caches only large files by default (small files, dirs skipped). Fine for video packs, not a full file list.
- SRS (ReScene sample) stores per-track match_offset into the main file + 256 signature bytes: fetch the one candidate article at that offset and compare = near-zero-FP alt confirmation (pyReScene resample/main.py).
- SFV CRCs match each volume's yEnc crc32= (zero decode). NFO: raw CRC32 = srrDB file list; normalized SHA-256 + simhash (ANSI art excluded, kept as group feature). No indexer dedupes on NFOs.
- Music: EAC .log TOC -> MusicBrainz DiscID / AccurateRip / CTDB / CDDB (Rust cdtoc crate). .cue needs leadout from FLAC STREAMINFO. FLAC STREAMINFO MD5 and log Copy CRC hash PCM, so they match across FLAC re-encodes.
- Packs vs singles: per-file (len,CRC32) or (len,md5_16k), byte-weighted containment not Jaccard (cross-seed compareFileTreesPartial; seasonpackarr requires exact size = pack episodes byte-identical to scene singles). NNTmux rebuilds a listable 7z from first + last bytes (withSevenZipEndHeader).

## Research post linking via posting behavior (Opus) — wave 4
- Simulated the end-to-start join (scripts: research/posting-behavior/sim/): P .975 / R .942 per file link with 20 obfuscated + 40 competing uploads, using Date seconds ordered by article number within each second, window [-1.2*width/rate - 0.5 s, +0.5 s], |ln rate ratio| <= 0.25. HWM interpolation fails. Whole-upload correctness only ~2/3, so confirm chains by size totals.
- NewsUP obfuscate posts each article to a random prefix of a shuffled pool: the union over ~10 articles is the uploader's exact pool (durable ID). Uploader profiles: tiered settings hashes + FP-STALKER upgrade rules + MinHash pools.
- Takedowns: all-or-nothing per fileset in OS3 2015, but partial removal reported; BODY can be gone (451) while STAT/HEAD succeed; old marker articles ("dmca'd", blocked@article). Lumen has no Usenet Message-IDs.

### Haiku 15-min posting behavior: Wave 5 (2026-11-02 08:55Z)
- Camelsystem PowerPost fingerprint from a real article (github.com/radekstepan/nzb-grabber-js test/fixtures/articles): X-Newsposter "Camelsystem Powerpost (Modified POWER-POST http://powerpost.camelsystem.nl)", Organization Camelsystem, Message-ID <part1of1.{20 alnum}@camelsystem-powerpost.local>, Date without weekday, line=128.
- yEncBin Poster "Upload Individual Parts (Filling)" re-posts only a file's missing parts later (binaries4all.com/yencbin/): another late-part repost source.
- Timeprints (Johansson et al. 2015, doi 10.1186/s13388-015-0022-z): hour/weekday histograms + cosine; 100% top-1 at 500 aliases, 70.6% at 4,000, needs hundreds of posts per alias. Long-term uploader tie-breaker only.
- Provider exposure: no Eweka/Omicron header sample exists online; one Giganews sample has Path, Xref, NNTP-Posting-Host, X-Trace, X-Received-Bytes, X-Received-Body-CRC but no Injection-Info (pastebin.com/LxCzQDvN). Eweka claims its own backbone. Only a live CAPABILITIES / LIST OVERVIEW.FMT / HEAD from Atlas can settle what Eweka exposes.

### Wave 5 (2026-11-02 09:02Z), tagged Proves / Cost
- TESTED: a file's PAR2 IFSC packet (ordered per-slice MD5+CRC32) and FileDesc full MD5+length are identical whether it sits alone under a random name, with other files, or with a junk file added; the exponent-0 recovery block changes once a junk file joins the set. So IFSC/full MD5 = per-file twin key (packs vs singles, junk-padded and renamed reposts) | 1 article (PAR2 index); exp-0 + set ID = whole-set key. Script: haiku/tools/par2_ifsc_compare.py.
- par2cmdline's default slice size is ceil(total_bytes/2000) rounded to 4 with first exponent 0, so two default re-PARs of the same file set always share slice size and exp-0 data; ParPar has no default -s (poster must choose), Nyuu makes no PAR2, ngPost PAR2 is off unless --par2_pct and then uses par2cmdline. Sources: par2cmdline src/commandline.cpp, ParPar help-full.txt.
- Survivability matrix for uploader countermeasures (subject randomisation, random names, junk files, volume size, -p, -hp, size-preserving edits): haiku/w5b.md. Takedowns work on Message-ID lists only (UvA rp.os3.nl p82). StreamNZB documents the deobfuscation cascade: MD5-16k+length to PAR2, then yEnc name=, then RAR5 volume number, then magic bytes. https://github.com/Gaisberg/streamnzb/blob/main/docs/obfuscated-releases.md
- No source shows any backbone altering article bodies; INN rejects oversize articles rather than truncating. Normalise before hashing (dot-unstuff once, CRLF only, key CRC-verified decoded bytes) and measure by fetching the same Message-IDs from two providers. haiku/w5c.md.

## Research post linking via payload fingerprints — wave 5 (2026-11-02 09:00Z), MEASURED with real rar 6.23/7.12
- NZB-only detector: under RAR5 -hp the LAST volume size is always ≡ 14 mod 16 (8 B sig + 38 B plaintext crypt header, everything else AES blocks); non-last volumes are exactly the -v size (5m = 5,242,880). Not fixed for -p, no password, or RAR4.
- Unencrypted RAR output is deterministic: byte-identical across runs and across rar 6.23 vs 7.12. A changed mtime alters only ~20 bytes per volume (header CRC/mtime, QO, end header). So re-RARs of the same file with same name/-v/switches have identical yEnc bodies and pcrc32 for every article except the first and last of each volume = repost twin link without downloads beyond one article.
- -hp size inversion: per-volume overhead 46+16k; 16 candidates per k, k depends on name length and mtime precision; ~640 candidates with unknown name. Needs a name-length prior or a second post to pin.
- Size uniqueness measured (445k Redump/No-Intro files + 140k PyPI): (size,crc32) 0 collisions; size alone 80-97% unique for free-form files >=100 MB but collisions concentrate on same-content variants; ±300 B ok >=1 GB. Size = filter, size + one key = safe.
- Torrent bridge: Anime Tosho crc32k (first 1 KB CRC) + size from one article -> btih + full hashes + stored NZB; self-hosted bitmagnet with SavePieces gives per-file size + piece hashes (verify with 1-24 articles).
- Scripts: research/payload-fingerprints/opus-tools/ (w5a_hp_invert.py, w5b_size_uniqueness.py, RAR5 parser with -hp decrypt).

## Haiku research: post linking via yEnc headers
### Wave 5 (2026-11-02 09:01Z)
- Highwinds server-assigned Message-ID decoded (INFERRED from 658 real IDs in public NZBs: BigBuck.nzb @fx33.iad, Binsearch reftest @fx21.am4, NZBIndex earth.nzb @fx02.iad): `T1.N$K.R@fxNN.site`. N = a front-end-wide article counter that continues across all files of a post, with gaps where other users' articles were injected. K (`$8E2`) is constant for the whole post, so it is a session key. T1 = a clock-like base-64 counter that rises with date. R = random. Same K + host + tiling N ranges = one upload session, even when split across groups or obfuscated. Cost: overview.
- Nyuu adopts a server-returned Message-ID (lib/nntp.js:866-875). ngPost IDs are 32 hex with no dashes. The ngPost Hydro74000 fork pads pcrc32 %08x while mbruel's is unpadded (a version marker). postie writes headers in a random order (Go map) with RFC1123Z dates. newspost sends no Message-ID or Date.
- Rust caution: the yenc crate always dot-unstuffs per line (decode.rs:159-160), so it double-unstuffs if the NNTP layer already did, giving byte loss and a false pcrc32 mismatch. It also hard-errors on unknown =y keys. Keep the raw =ybegin/=ypart/=yend lines in Atlas's own scanner. crc32fast 1.5.4 has Hasher::combine.

### Research post linking via yEnc headers (Opus 3x3), wave 5 (2026-11-02 09:10Z)
All RNG results below are from faithful re-implementations run on real V8/C/Python, not the real posting tools. Validation on real posts is next.
- Nyuu-Obfuscation: ONE article's Subject+From (68 chars) recovered V8 state in ~0.1 s and predicted all 60/60 articles' Subject, From, wire Message-ID and Date offset, backward and forward, across queue depths 1-3 and with retries (~25 runs). Subject alone (32 chars) is not enough. Draw gaps between consecutive Subjects: 155 normal, 156 at a new file, 112 at edges, 198 after a retry. Code: research/yenc-header-forensics/opus/nyuu-obf-sim/.
- NewsUP: one own-style ID (`sec usec . base36(rand) @ base36 .newsup`, digits LSD-first) gives a unique drand48 state ~92% of the time. It linked all 29 sibling IDs in 40/40 trials with 0 false links, and regenerated the random From. Stepping back also finds the Perl srand seed.
- ngPost: brute-forcing a 32-bit glibc seed against one obfuscated From costs ~1.75 core-hours (~200 s on 32 cores) at index 0. Pre-v4.15 ms-timestamp seeds are found in seconds from the Date. glibc seed-1 Froms: pkDHTxmMR18N2, l9k88EmLgN7cC... MSVC seed-1 Froms confirmed: p1KaLmIg2kB7f, Zp5Tm1qbi6dss, 92C31aPGJK9Lv. Code: research/yenc-header-forensics/opus/rng-tools/.
- Escape-count twin key on 104 real NZBs (69k segments, mostly Nyuu test posts): Nyuu's NZB segment bytes = yEnc body + =y lines with a constant residual of 3, so per-segment escape counts are EXACTLY recoverable from the NZB. On random content the per-part sd is ~100 and per-segment false equality is 0.22% across 62,759 unrelated pairs (k parts ~0.0028^k). Identity across independent reposts is still untested (no real repost in the corpus). The key is useless on low-entropy content (zero escapes), so gate it on mean escapes > ~5000 per part. SABnzbd/indexer NZBs that include headers in bytes were not tested.

## Research post linking via posting behavior (Opus) — wave 5
- Highwinds server Message-ID <T.N$S.R@fxNN.site>: T = little-endian base-64 (0-9a-zA-Z) seconds + 876,645,500 = unix time (matched NZB dates 4/5); N = per-host counter; S = constant per upload session. XOVER-only same-upload key on every provider. Evidence: sabnzbd.github.io tests/*.nzb and 3 other public NZBs.
- Real headers: Path stripped on Highwinds/Abavia (not-for-mail) and Omicron (path-stripped); no Injection-Info. HEAD keeps X-Received-Body-CRC/Bytes, Organization/X-Complaints-To. Highwinds XOVER: Lines empty, 2-digit-year Date. Abavia nntpswitch2 synthesizes article numbers over 14 days.
- Simulation: XOVER-only file runs fail (short last article arrives early, ~N/2 stragglers); need yEnc part headers. PAR2 size-set constraint lifts whole-upload correctness .35 -> .78.
- Co-removal: 451 = DMCA (Giganews KB), STAT can lie after BODY removal, partial removal common; 4-6 canaries per file. Rust nzb-nntp breaks the connection on 451.
- (yEnc headers thread, 09:10Z correction) Highwinds T1 = little-endian base-64 `0-9a-zA-Z`, unix = value + 876645500 (decoded by the posting-behavior Opus thread; re-verified on BigBuck.nzb: 04:00:26 -> 04:01:58 across 133 articles). Ignore my wave 5 alphabet guess.

### Haiku 15-min yEnc headers: Wave 5 (2026-11-02 09:10Z)
- X-Received-Bytes + X-Received-Body-CRC also appear on Newshosting (Highwinds/Omicron) 2014-15 posts, with NNTP-Posting-Date "Thu, 13 Mar 2014 20:41:40 UTC", X-Complaints-To abuse@newshosting.com, and Organization "Newshosting.com - Highest quality at a great price! www.newshosting.com". So the pair is a Highwinds-family reader header, not Farm-only. (GerHobbelt/FontOrg-and-FontRenamer text backups, via gh code search)
- The Highwinds Path injection token is "<frontend>.POSTED!<8 hex>!not-for-mail", e.g. newsfe22.iad.POSTED!7564ea0f!not-for-mail (2009, larsbrinkhoff/its-archives). If the hex is a per-account or per-connection hash, it's a same-uploader key (unverified). Giganews: "news.giganews.com.POSTED!not-for-mail".
- XOVER twin recipe: body_i = :bytes_i - visible header length - a per-server constant, then match files on the difference vector d_i = body_i - body_1, which cancels unseen headers. INN innd counts CRLF as 1 and leaves out the blank line and the ".", while tradindexed stats the whole file, so :bytes semantics depend on the backend. :lines is a coarse fallback (~2.5 bits/part). The peer finding that Nyuu NZB segment bytes = body + y-lines + 3 makes NZBs the cleaner source.
- Prior art: GoNZB (Go, 2026) groups on canonical subject token + size + file idx/total + part/total, never From/Message-ID, and budgets BODY probes. Weaver (Rust) flags obfuscation at 30+ contiguous hex/dot chars. nzbfast "Pesto" obfuscates everything but posts a real-name PAR2 after the payload. Newsbin dedupes on RIPEMD of the first 24-26K. SABnzbd warns "Duplicate part 1, but different ID-s" for crosspost copies.

### Wave 6 (2026-11-02 09:30Z), tagged Proves / Cost
- NZBGet RarRenamer chains obfuscated RAR volumes from headers alone: same RAR version, volume number N+1, and SplitBefore filename == previous volume's SplitAfter filename | orders and groups one archive set | 1 article per file. https://raw.githubusercontent.com/nzbgetcom/nzbget/develop/daemon/postprocess/RarRenamer.cpp
- AltMount duplicate lookup key "sample-sha256-v1": SHA-256 over tag + size + three 64 KiB samples (start, middle, end), whole file if <=196608 B, with test vectors | twin key for unarchived files | 3 articles. https://raw.githubusercontent.com/javi11/altmount/HEAD/docs/docs/4.%20API/duplicate-lookup.md
- Raw split posts (HJSplit/TC/Krusader .001, split -b) have no header: part N = original bytes [N*vol,(N+1)*vol), the first .001 article starts with the original's magic bytes, and TC/Krusader .crc sidecars ("filename= size= crc32=") give the original file's CRC32+size | alt key vs unsplit/RAR posts | 1 article. https://docs.kde.org/trunk5/en/krusader/krusader/splitter.html
- Re-uploader suffix dictionary from Sonarr Parser.cs (-Obfuscated, -Scrambled, -Rakuv, -WhiteRev, -BUYMORE, -AsRequested, -postbot, -xpost, -RP, -NZBGeek, -GEROV, -Z0iDS3N, -Chamele0n, -4P, -AlteZachen, -RePACKPOST ...) and torrent tags [rartv] [rarbg] [ettv] | re-uploader identity | free. Scene allows only RAR+NFO+SFV+Sample/Proof/Subs, so any extra file (.txt/.jpg/.exe such as RARBG_DO_NOT_MIRROR.exe) marks a re-pack and may name its P2P source | free from archive name list. Detail: haiku/w6*.md

### Haiku 15-min posting behavior: Wave 6 (2026-11-02 09:30Z)
- VERIFIED from source (github.com/demanuel/NewsUP @43a4665, bin/newsup.pl:331-336 and lib/NewsUP/Utils.pm:348-360): with --obfuscate every article gets its own random non-empty subset of the configured groups (in-place Fisher-Yates shuffle, then first int(rand(n))+1 groups) and From ''. So each article is a true crosspost with a varying group set; the Xref union over ~10 articles reveals the pool. NewsUP keeps the real yEnc name=.
- NewsUP's PowerPost imitation IDs are part<N>of<M>.<22>@camelsystem-powerpost.local / @powerpost2000AA.local (Utils.pm ~774): real PowerPost IDs encode part and total.
- Anime Tosho: no-key Newznab/JSON feed (feed.animetosho.org/api) and daily dbexport (storage.animetosho.org/dbexport/: crc32, sha1, ed2k, stored_nzb) = public labelled releases for validating linkers.
- Overview-only header-length fingerprint is weak: yEnc escape noise is about ±105 bytes per 700K article (escape ratio 0.0158 at line 128, node-yencode README).

## Research post linking via payload fingerprints — wave 6 (2026-11-02 09:31Z), MEASURED
- Re-RAR of the same file (rar -m0 -ep1, same -v) with only the mtime changed: every middle part keeps its pcrc32 and its NZB-visible encoded size; only the first and last part of each volume change. rar 6.23 vs 7.12: 62/62 identical. Different -v, name or sub-second mtime shift all bytes, but CRC-interval algebra links them with 2-4 decoded articles. Encoded-size (escape count) key works on high-entropy payloads, fails on low-entropy (3/62 unique). Under the owner's definitions these are alts, not twins.
- 7z -mx0 volumes are raw splits of one stream: mtime/name change alters only the first and last part of the whole set; different -v gives an identical stream.
- RAR5 -hp predictor (≈400 real rar runs): non-last data D = floor16(V − A(L)), A = 480 + 32·steps at L ∈ {18,34,50,65,81,97,113,128}. Last volume gains 240 B once it holds >4 KiB, and ≡14 mod 16 had one exception, so treat it as strong, not absolute. Forward check against a named candidate release confirms its size to the byte (~9% FP only from same-step name lengths). Two posts at different -v still leave ~17 candidates.
- Packer defaults (source): ngPost --compress = rar -ep1 -m0 -v42m, no password unless --gen_pass/RAR_PASS -> reposts are near-identical alts; nzbup bot uses ngPost without passwords; NxG Upper -hp by default (random); postie/Nyuu/GoPostStuff post raw files; scene: no compression/passwords, decimal 15/20/50 MB volumes -> scene reposts are byte twins.
- Scripts: research/payload-fingerprints/opus-tools/w6a_*, w6b_*.

### Haiku 15-min yEnc headers: Wave 6 (2026-11-02 09:32Z) — X-Received-Body-CRC solved
- **X-Received-Body-CRC = zlib.crc32(body_with_CRLF + ".\r\n")**: the body after the blank line, headers excluded, with the NNTP terminator included. It matches 12 real articles (Newshosting posts in GerHobbelt/FontOrg-and-FontRenamer plus a Usenet.Farm doc example), and I re-verified one myself (961135873; without ".\r\n" it misses). Scripts are in research/yenc-header-forensics/tools/xrbc/. Dot-stuffing is untested.
- For a yEnc part the body is the =ybegin/=ypart lines, then the data, then =yend. So the same tool + same yEnc name= + same part split + same payload gives an identical X-Received-Body-CRC, which is a byte-level twin key from HEAD/HDR with no BODY fetch. It is written by Highwinds-family readers (Newshosting/Omicron, so likely Eweka) and Usenet.Farm. It breaks when the yEnc name is randomized per post or the encoder differs. It also gives a free integrity check after BODY.
- Open: does Eweka expose it through HDR/XHDR or LIST OVERVIEW.FMT (bulk cost)? A live check on Atlas's Eweka account would settle it.
- Highwinds Path "POSTED!<8hex>!" is a per-feed/cluster constant (one 2009 poster always got 7564ea0f), so it's weak as an uploader key. Highwinds software: Typhoon NewsServer, Cyclone NewsRouter, Tornado reader, Hurricane numbering.
- Obfuscation classifier regexes per tool (Nyuu, Nyuu rand/postie Full/Go-Post-Tools collision, postie Partial and nxg, ngPost, NewsUP fakes with tells, pesto) are in research/yenc-header-forensics/log.md, W6C.

## Haiku research: post linking via yEnc headers
### Wave 6 (2026-11-02 09:33Z)
- Highwinds `T1.N$K.R@fxNN.site` on 5 public posts (BigBuck fx33.iad, nzbunity test fx30.am4, sabnzbd basic_rar5 fx31.am4 and test_zip fx45.am4, NZBFetch earth fx02.iad): K is constant for each whole post and distinct between posts (3 chars 2016-17, 4 chars 2025), so it is a session key, not an account. The T1 alphabet has 64 symbols incl. '_' and '%'. N interleave is 0-27%. R is uniform over ~0-290k. Linking key: (host, K, T1 window, N range).
- Lineage: INN lib/messageid.c `<radix32(now-673416000)$radix32(pid)$++count@domain>`, with a per-process count, so the same pattern applies to INN-injected posts.
- node-yencode #3 (2016): Nyuu from around Oct 2016 left critical chars unescaped at the last column. That is a version marker and a decode-divergence risk.
- New Message-ID suffixes in public NZBs: `@UFK` (probably Usenet.Farm, cf. X-Ufk) and `-<ms>@PRiVATE`.

## Research post linking via posting behavior (Opus) — wave 6
- Server-assigned Message-IDs decoded (dataset: research/posting-behavior/server-ids/segs.tsv, 72k segments from 261 public NZBs): Diablo <hex unix$seq$pid$hosthash[$x]@cluster> (Astraweb, UsenetServer, free.fr, Eweka upload) = time + front-end + process; Giganews: base64 -> XOR 0x9D -> 6 LE uint32, word2 = unix time; newsmangler <float time>.<segment>@<provider>. Usenet.Farm random.
- Sim round 3: sparse yEnc header reads (1 in 5) collapse whole-upload accuracy to 0; a unique per-upload server session token equals reading every header.
- Propagation lag across backbones (seconds to >1 h) could fingerprint the injection cluster; no published measurements exist.

### Research post linking via yEnc headers (Opus 3x3), wave 6 (2026-11-02 09:45Z)
- REAL-WORLD CONFIRMED: stock Nyuu's DEFAULT Message-ID is solvable too. It is 24 Math.random draws alternating 65+x*26 / 97+x*26 (lib/article.js:199-228) + -<ms>@nyuu, so two consecutive IDs (48 draws, ~4.7 bits each) recover V8 state. Real sanderjo/NZBs posts solved: hp-passworded.nzb (2026-03, 906/1654 IDs linked into one stream in 0.1 s; window too small for the rest), reftestnzb_starrySTARnight (2025-07, 30/30), DDDD (2025-10), QUIRINIUS. Consecutive IDs are mostly 48 draws apart, not 24 (second block per article: likely send-time redraw). All 2018-2023 posts FAILED (9/9). Guess: older Node/V8 converted state to double non-linearly, which needs z3. Solver: research/yenc-header-forensics/opus/nyuu-id-solver/nyuuid_solve.py.
- REAL REPOST CONFIRMED for the escape-count key: two sanderjo uploads ~19.4 h apart with all-different Message-IDs (unique_files_with_subdir...) match 15/15 segment byte counts on 5 high-entropy 10 MB files (chance ~0.0028^14). Same name with regenerated content correctly does not match. ngPost NZBs carry bytes = 716800 flat (decoded size), so they have no escape info; gate them out. Header-inclusive NZBs (UFK/NewsUP) look like they need only a per-file constant offset (inference).
- Nyuu time-domain (101 NZBs, 93 uploads): 0/57,093 timestamp inversions within files, 0/2,651 overlapping adjacent files, and a removed middle file's [min,max] fits exactly its gap in 2,565/2,565 tests. So an orphan Nyuu file can be placed in its upload by ms-timestamp continuity. <file date> = first ID second for 87/93; deviations expose forged dates, merged NZBs and indexer re-writes (Binsearch skew 0-7 s).
- NZB writer fingerprints (decl case, DOCTYPE, attribute order, indent, EOL, comment stamps): Nyuu old "utf-8" vs current "UTF-8" (toUpperCase), CRLF/TAB, attrs poster,date,subject; ngPost 2-space LF; Binsearch/NZBIndex/newznab 0.2.3pz/nZEDb/NZBgeek comment stamps; .NET rewrites date,poster,subject + BOM; old newzbin subject,date,poster + unix-time comment. Who posted (IDs) and who wrote the NZB (format) are separable.
- Details: research/yenc-header-forensics/opus/w6*.md

### Wave 7 (2026-11-02 10:02Z), tagged Proves / Cost
- Cleanfeed's EMP filter MD5s the raw article body (headers excluded) and hashes yEnc binaries raw, rejecting the same body seen more than MD5RateCutoff times in 2 h; NewsDemon advertises ingest dedupe. Inference: raw-body identity is what servers police, which is one reason obfuscating posters vary yEnc name=/subject per article. Raw-body keys (X-Received-Body-CRC, Cleanfeed MD5) only match same-tool same-name re-injections, so cross-repost matching must key on the decoded payload CRC | twin (decoded) vs re-injection (raw) | free/1 article. https://raw.githubusercontent.com/crooks/cleanfeed/master/cleanfeed
- Sampled hashes for unarchived posts, by article cost: moviehash and SubDB (first + last article; external DBs), imohash and AltMount sample-sha256-v1 (start/mid/end, 3 articles), Shooter (4), Napiprojekt (~15). Head+tail schemes are candidate-grade; add a mid sample for twin grade. haiku/w7b.md
- Non-video set fingerprints: MP3 scene sfv+nfo+m3u; EAC log checksum / XLD signature make the log a per-rip fingerprint (1 article); scene 0day/ebook = NFO + FILE_ID.DIZ per zip, BASENAMEXX volumes; FitGirl = setup.exe + FG-NN.bin + MD5/fitgirl-bins.md5 (per-file MD5 list identifies the exact repack build). haiku/w7a.md

### Haiku 15-min posting behavior: Wave 7 (2026-11-02 10:05Z)
- INN server-assigned Message-IDs (lib/messageid.c:46-72) are `<Radix32(time-673416000)$Radix32(pid)$count@domain>`, alphabet 0-9a-v, suggested in the nnrpd 340 reply. One nnrpd process = one connection (fork mode), so pid is a connection key and count gives order. Decoded on a real 2004 ID (c02u1r$o4h$1 -> 2004-02-07 14:51:39 UTC, pid 24721, matching syslog; lists.isc.org inn-workers 2004-February/012652).
- Astraweb-style IDs `4f9a3e8a$0$14349$c3e8da3$33a0879d@news.astraweb.com` lead with hex Unix time; the middle constant may be a server/session key (nzbget tests/testdata/nzbfile/plain.nzb, 3 samples).
- PowerPost 2004 real source (SourceForge CVS snapshot) sends no Message-ID and no Date ("let the server fill it in"), crossposts one article to a comma list, subject "$F (n/N)" zero-padded to total's digits, part count size/(lines*128)+1. So server-ID session keys apply to it.
- Measured on public NZB fixtures (github.com/sanderjo/NZBs): Nyuu 716800 raw -> NZB segment bytes ~728-740K depending on content; NewsUP 768000 -> 793K; NZB <file date> non-monotone in 7/17 by up to 105 s, so date ordering needs ~2 min tolerance. Message-ID shape beats encoded size as a tool fingerprint. Scripts: research/posting-behavior/haiku15-tools/.

### Haiku 15-min yEnc headers: Wave 7 (2026-11-02 10:00Z) bulk access to X-Received-Body-CRC
- Highwinds/Eweka advertise XZVER, XZHDR, XFEATURE-COMPRESS GZIP TERMINATOR (no COMPRESS), and Eweka's "GZIP" is actually zlib 78 01 (Spottarr/Usenet docs/adr/0006). Captured reader CAPABILITIES list HDR and LIST HEADERS. Nobody has shown that XHDR X-Received-Body-CRC is accepted, since RFC 3977 allows a 503 for non-overview headers.
- Cost for 1M articles: XHDR at 1000 rows/request is ~20 MB if accepted, versus pipelined HEAD at ~1 GB and ~1000 s of RTT. Next step is a one-off probe of XHDR X-Received-Body-CRC on a small range on Eweka.

## Research post linking via payload fingerprints — wave 7 (2026-11-02 10:00Z), MEASURED
- 7z -p WITHOUT -mhe: the end header is only LZMA-compressed, so names, exact sizes and plaintext per-file CRC32s are readable without the password, identical to an unencrypted 7z of the same files -> links passworded 7z posts to plain copies. -mhe (ngPost's 7z default) still exposes packPos = exact packed total (sum of roundup16(file)) and the raw header length. 7z has no password check value; LZMA first byte 0x00 filters 255/256 wrong passwords.
- 7z -mx0 no password: data region (byte 32..packPos) = raw files concatenated; stable across 7z 16.02/24.09, mtime and -v; only start/end headers change (67-72 B).
- RAR5 -hp exact volume-size model, 33/33 real runs exact (rar 6.23 = 7.12): QO record in every volume once that volume holds >4096 data bytes; sub-second mtime adds a 7 B time record; size fields padded to fixed width for >=1 MiB. Model: research/payload-fingerprints/opus-tools/w7a_hp_model.py.
- NZB-only segment-size index (Nyuu-style NZBs only): k=8 consecutive middle-part escape counts -> 64-bit key, m>=2; ~8.6 bits/part on compressed media, ~0 false candidates/day at 1e8 files; recall 1.0 for reposts and mtime re-RARs, 0.93 with 20% missing parts; blind to byte shifts. Skip ngPost NZBs (bytes == 716800) and low-entropy files. Sketch: opus-tools/w7b_segindex.py.

### Haiku yEnc header forensics, wave 7 (10:05Z)
- Message-ID classifier over server-ids/segs.tsv: known grammars cover ~86% of 72,334 segments (nyuu 103 NZBs, @PRiVATE, ngPost, yEncBin "Part N of M.HEX32@ts.local" (new), Usenet.Farm @UFK, PowerPost, Diablo, newsmangler, Highwinds). 10,370 unknown segs are mostly one random-alnum@hex-host family. Script: research/yenc-header-forensics/tools/msgid-classifier/.
- Highwinds T1 settled: 64 symbols, `_`=62, `%`=63, LE, +876645500; 0 decreases over 862 pairs. $K is one per NZB.
- Usenet.Farm rewrites Date (2-digit year, "UTC"): normalise Date before using its form as a tool fingerprint.
- Consolidated tool and server fingerprint table: research/yenc-header-forensics/fingerprints.md.

## Research post linking via posting behavior (Opus) — wave 7
- Sim round 4: whole-upload matching to the PAR2 size list = 100% fully-correct uploads when file sizes differ; equal RAR volumes 65-80% (tie-break with PAR2 16k MD5 from the first article of each run).
- Highwinds T alphabet: 0-9a-zA-Z then '_'=62, '%'=63. S per upload 7/7 NZBs. Giganews w2 = time 20/20 within 1 s; w0 per-connection PID?; Supernews base32 time prefix; float.n@astraweb/blocknews/easyusenet IDs are client-made.
- RFC 3977: article numbers MUST be in arrival order per server. NEWNEWS single-group bisection gives exact arrival where enabled. HDR on non-overview headers blocked on Abavia (nntpswitch DenyNoIndexField).

### Haiku 15-min yEnc headers: Wave 8 (2026-11-02 10:22Z) NZB bytes families, header normalisation, prior art
- NZB `<segment bytes=>` has three families (source-verified). Wire: Nyuu (body =ybegin..\r\n.\r\n, equals X-Received-Body-CRC input length), pesto (headers+body). Raw: postie, ngPost, NZBGet NzbGenerator, newsmangler (constant per part; only the last-part remainder carries info). XOVER: newznab/NNTmux, nZEDb, nzbsearcher re-emit :bytes. Classify the family before comparing bytes across NZBs.
- TorBox's news server normalises header names (X-Received-Body-Crc, Message-Id) and forces Path: not-for-mail (debridmediamanager/notes). Parse header names case-insensitively; header casing and Path are not poster fingerprints behind resellers. Same caution as the Farm Date rewrite.
- X-Received-Body-CRC already appears on 2014-15 Newshosting/highwinds articles saved on GitHub (GerHobbelt/FontOrg-and-FontRenamer), printed as unpadded decimal. Dot-stuffing in its input is still unsettled; there is still no public XHDR/HDR evidence.
- Prior art never links across groups or across reposts: NNTmux collectionhash = sha1(subject parts + fromname + groupID + N/M), nZEDb nzb_guid = md5(first Message-ID), SABnzbd md5 = over segment Message-IDs, NZBHydra2 = normalised title + age-hours and size-% thresholds with group/poster vetoes. Atlas's body-CRC and size-vector twin keys go beyond all of them. Worth borrowing: NZBHydra's veto rules for alt candidates, and a links(a<b, kind, score, evidence) table.

### Haiku 15-min posting behavior: Wave 8
- **$-style server IDs (e.g. `4f9a3e8a$0$14349$c3e8da3$33a0879d@news.astraweb.com`)**: field 1 is hex unix time (equals the NZB file date); field 2 is "0"; field 3 is NOT monotonic (two IDs in the same second went 26527 then 23145); field 4 (c3e8da3) is constant for months, so it is a per-host value, not a session key; field 5 varies, maybe a hash. Same shape seen on news.kabelfoon.nl in 2004, so it is server software, not Astraweb-only. Use it for timestamp plus server fingerprint, not for session linking. (Source: nzbget and gonzbee test fixtures.)
- **Multi-server posting (source-verified)**: ngPost pools connections from every [server] block and all of them pull from one shared article queue, so a single post is spread across providers and front-ends in arbitrary order, and retries go back to the queue (src/Poster.cpp:85-121, PostingJob.cpp ~573). Nyuu has one --host only, so a second provider means a second run. Postie dispatches round-robin over all enabled servers via nntppool, with backup-tier failover on 430 (doc summary, not source).
- **Linking consequence**: a server-assigned session key (INN pid, Highwinds K) covers only one connection's share of a post, so a ngPost or Postie upload yields several keys at once. Treat each (host, front-end) as its own stream and stitch streams by time overlap plus subject or yEnc total. Out-of-order arrival and mixed Path/Injection hosts inside one post are normal, not evidence of a second uploader.
- **Nyuu custom domains**: real 2022 Nyuu test posts use `@nothereatall`, so match the `-\d{13}@` shape, not the `@nyuu` suffix. Compare the ID ms to each article's own Date header, not the NZB file date (segments drift up to 26 s from it).
- **Dead end**: nzbindex, binsearch and nzbking are blocked from the research container, so live multi-group spray is still unmeasured from public data. Atlas's own header log is the only way to measure it.

## Haiku research: post linking via payload fingerprints — wave 8 (2026-11-02 10:30Z)
- PAR2 exp-0 key, tool coverage (source/doc verified): par2cmdline -f first recovery block default 0, -r 5%, -b 2000 when no -b/-s; ParPar -s REQUIRED, -r default 0, recovery offset 0 (lib/par2gen.js). Exponent 0 has coefficient 1 in any GF, so poly/base differences do not matter for this key, only slice size. ngPost makes PAR2 only with --par2_pct (default 0) via embedded par2cmdline; sample ngPost.conf uses fixed slices (-s5M ParPar, -s768000 par2cmdline, /ss768000 MultiPar). Nyuu has no PAR2. MultiPar/par2j, QuickPar, turbo byte-identity: UNVERIFIED. Proves twin; cost 1-few articles (first RecvSlic of smallest vol). w8b.md
- Marker files: only RARBG.com.txt verified by hash (34 B, MD5 a7d518b719bf133ff5bd4a0fdf9c8f2f, SHA256 66f74d30...b147, gridinsoft). Others (RARBG_DO_NOT_MIRROR.exe, YIFYStatus.com.txt, www.YTS.MX.jpg, TGx) no public size/hash found. Small .txt sizes 15-280 B overlap, so size alone is not a marker; need name + CRC32 (free for single-part files via yend crc32). Proves same uploader/re-packer. w8a.md
- Rust crate map (dependency rule applied): USE crc32fast (821M dl; Hasher::combine exists -> crc32_combine of part CRCs), RustCrypto md-5/sha1/sha2, xxhash-rust or twox-hash. WRITE OWN: PAR2 packets + GF16 (~650-900 LOC), RAR4/5 headers (~600), 7z headers (~600), yEnc (~150), murmur3, oshash/imohash, MinHash/SimHash/Bloom. Note the archive-metadata thread already has proto/{rar_meta,sevenz_zip_meta,par2_sfv_srr_meta}.rs. w8c.md

## Research post linking via payload fingerprints — wave 8
- **Encrypted reposts (RAR5 -hp, 7z -mhe) link by sizes alone.** Real runs: identical volume sizes across passwords of 1/24/80 chars, and the exact size model matched. (V, n, S_last) is ~27.5 bits (p=5.3e-9/pair), so on its own it is a blocking key (~2.6e5 false pairs at 1e7 posts). Adding one more exact file size from the post (sample, nfo, 2nd episode) gives p=2.8e-17, <1 false pair per 1e8. PAR2 sizes add ~0 bits. Pitfalls: sub-second mtime (+7 B header) flips ~half of name lengths, so index both; a poster choosing a different volume size breaks it. Scripts: payload-fingerprints/opus-tools/w8a_*.
- **Full-disc posts (BD/UHD/DVD):** alt keys = hash of BDMV metadata (index.bdmv, MovieObject.bdmv, *.mpls, *.clpi) or DVD IFOs; libdvdread DVDDiscID (MD5 of the IFOs); Windows dvdid CRC64 (ISO only, DVD Profiler DBs key on it). Per-master keys = SHA-1 of AACS/Unit_Key_RO.inf (KEYDB/FindVUK key, ~190k titled entries = title oracle), Content000.cer 6-byte Content Cert ID, UDF PVD Volume Set ID (first 16 chars unique) + impl regid, id.bdmv Org+Disc ID. **AACS/ContentHash000.tbl** holds 64-bit SHA-1 per 192 KiB of encrypted m2ts (~3 articles): any single encrypted segment can be checked against another post. All 1–3 articles.
- **Crates (pure Rust):** crc32fast (copy private multiply/x8n, add x^-8n and zero-unpad), RustCrypto 0.11/0.13 generation, lzma-rust2 (pin 0.23), zip with default-features=false (default pulls C zstd), xxhash-rust for persisted keys (not ahash). Write in-house: PAR2, RAR4/5 (+ -hp KDF loop), 7z headers, yEnc with escape counts, OSDb/ed2k/BTv2/CD-TOC IDs. Avoid unrar/unrar_sys (C++), unrar-rs (UnRAR licence), xz2, discid.
- **Wave 9:** size clustering on real size lists, a pure-Rust CRC32 GF(2) toolkit prototype, packer/version tells inside the payload.

## Research post linking via yEnc headers (Opus 3x3) — wave 7 (2026-11-02 10:30Z)
Reports: research/yenc-header-forensics/opus/w7a-nyuu-old-v8.md, w7b-old-newsup-ngpost.md, w7c-weak-rng-survey.md
- Nyuu 48-draw gap explained: article.js builds a 24-draw ID that is thrown away, nntp.js doPost() calls randomizeMessageID() per send attempt and that ID is published. Gap 72 = a retry. Same in v0.3.0-v0.4.2.
- Why 2017-2022 Nyuu posts fail: V8 4.9-7.0 (Node <=11) returned the low 52 bits of state0+state1; V8 7.1+ (Node 12+, late 2018) uses state0>>12. So those posts ran on Node <=11. The add makes the solve nonlinear; z3 returned unknown on synthetic data. Next: carry-guess linearisation or CryptoMiniSat XOR. Solver stub: opus/nyuu-id-solver/nyuuid_solve_old.py.
- '[A-Za-z]{32}@UFK' IDs are server-assigned (NewsUP Feb-Jun 2017 used the ID from the 240 reply). Provider unknown; not solvable, but a provider fingerprint.
- '<32hex>@ngPost' = ngPost uuid4 without dashes (secure, not predictable). The 'Sc5dLs_...@JxB048gTyZ.qDa' From is a fixed config value from a wrapper, so it's an exact-match uploader fingerprint.
- New deterministic/structured IDs (link without any RNG attack): realNeodome/geri HMAC-SHA256(key,file,part,body) with '<crc32hex>.geri' domain (reposts with same key give same IDs); keimpema/Usenet + Spottarr fork 'part{n}of{N}.{guid}@unknown.com' (one GUID per file); isc-usenet/Upstream time.time() microseconds '@riverup'. Secure RNG (fingerprint only): pesto (32hex), Brennerofhell/usenet-posting (uuid4 + fake UA list). SuperPoster sets 'Organization: turboPoster'. No Go math/rand, java.util.Random or weak .NET Random found in any other poster.

### Haiku yEnc header forensics, wave 8 (10:32Z)
- Unknown Message-IDs are 4 custom posters, not one tool: 4 NZBs hold 97% of unknown segs. The largest is random alnum{11-20}@alnum{28-36} with a hex-run host prefix, no timestamp; no public generator found.
- postie default part size is 750000 (config.go:574-575). postie keeps the Message-ID across all retries and reposts. Nyuu (lib/nntp.js:828) and ngPost mint a new ID per send attempt, so duplicate part numbers with different IDs in one session are retries, not twins.
- Rust-ready classifier and Highwinds session-key spec with 10 real test vectors: research/yenc-header-forensics/raw/w08-c-rust-spec.md.

## Research post linking via posting behavior (Opus) — wave 8
- SIMULATION ROUND 5: the PAR2 first-16 KB MD5 tie-break fixes uploads made of equal-size RAR volumes. Fetch the PAR2 index, then one first-article body per run of files whose links are unique. With 1-8 concurrent uploads, accuracy goes from 24-72% (size chains only) to 100%, at 0.04-0.96 bodies per file. At 10% first-article fetch failures it still scores 92-99%. Link across groups, not within one group. Endpoint bisection is worse.
- SERVER SESSION KEYS ARE PER CONNECTION: one connection key holds only ~6% of a post, and 0% of posts come out whole. A stable per-file subject or yEnc name stitches the pieces perfectly. Without those, stitch by time overlap with an article-size and yEnc-total veto, per-front-end skew correction and 2 s tolerance. That gives ~0.85 whole posts at 8 concurrent posts, but only ~0.5 on busy front-ends (32 posts). Splitting low-confidence clusters with one body per session lifts that to ~0.85 (~15 bodies per post).
- ADVERSARY VIEW (ngPost, Nyuu, postie, nxg-upper source + forums): uploaders know about subjects, poster names, passwords and reposting. Nobody mentions timing, posting order, article size, the PAR2 creator string, or volume sizes, and no tool shuffles order or adds jitter. No tool can change yEnc size/part/total or PAR2 recovery set ID/hash16k, since repair depends on them. Leaks even when obfuscation is on:
  - postie "partial" uses md5(subject) and md5(filename+part), which can be reversed with a predb name dictionary.
  - Nyuu's default From is the real user@host, and `--date` makes the Message-ID timestamp constant across the whole run.
  - postie reposts reuse the same Message-IDs.
  - ngPost seeds its RNG from 32 bits (srand(QUuid.data1)), so the archive password might be brute-forced from the From strings (untested).
- PROBE KIT: research/posting-behavior/nntp-probe/ holds a read-only, stdlib-only probe (nntp_probe.py + README checklist). It was tested against fake servers (normal, legacy, hostile, TLS) with 0 errors. It refuses write verbs, takes the password only from the NNTP_PASS environment variable, and redacts it. Covers CAPABILITIES, DATE skew, OVERVIEW.FMT, HDR on non-overview headers, NEWNEWS, the Lines-empty check, Message-ID shapes, XZVER and XFEATURE.
- Peers: postie full mode randomizes Date per article (0-6 h back), so joins must use article numbers or Injection-Date. Nyuu and ngPost mint a new Message-ID on each retry, while postie keeps the same one.
Wave 9 plan: (A) Rust-ready spec: write the end-to-end linking pipeline (join, PAR2 match, hash16k, stitch, vetoes, confidence) as pseudocode + data structures against Atlas's tables; (B) check the join on real data: the end-to-start join and Nyuu Message-ID timestamp ordering, run on the 261 public NZBs in server-ids/segs.tsv (the yEnc thread already covers ngPost seed recovery, and archive-password recovery is out of scope); (C) PAR2-less uploads and yEnc pcrc32 vs PAR2 slice checksum fallback, plus how often public NZBs lack PAR2.
Details: research/posting-behavior/wave-08-*.md, probe kit in research/posting-behavior/nntp-probe/

### Haiku 15-min yEnc headers: Wave 9 (2026-11-02 10:40Z) split-independent file CRC, PAR2 binding, encoder tells
- NEW split-independent file key: fold every article's =yend pcrc32 with crc32_combine (zlib crc32_combine_op; pesto yenc/mod.rs) using =yend size=. The result is the whole-file CRC32, identical across reposts with different article sizes or tools (Nyuu 700K vs ngPost 716800). It links same-container reposts across tools; cross-check it against the =yend crc32= when present.
- NEW PAR2 binding without payload: fold PAR2 IFSC slice CRCs (unpad the last slice) == folded pcrc32 proves a PAR2 set belongs to a post (NZBGet ParQuick does this at file level after download). When slice = m × article size (postie default, juicenet-cli --slice-size-multiple=700K), each slice CRC = fold of m pcrc32, so even partial/split posts can be bound. Slices not aligned to articles can't be derived.
- Encoder tells (from source templates): ngPost pcrc32 is NOT zero-padded (std::hex); GoPostStuff, Blat and rustnzb use uppercase hex; rustnzb has no part= on multipart =yend; nzbget's test server uses part=0; xHarbour leaves a trailing space on =yend; newspost single-part uses crc32= only.
- Subject tells: NewsUP has no "yEnc" token, `[i/N] - "file" (p/P)`, and its obfuscation randomizes only the subject (name= stays real); sanguinews has the default prefix `[sanguinews] - `; ypost appends `N bytes`; newsmangler zero-pads the part index to the total width; pesto has no file index.
- No public wire captures with a User-Agent and yEnc body exist on GitHub; verifying the table needs Atlas's own article samples.

### Haiku 15-min yEnc headers: Wave 10 (2026-11-02 10:55Z) :lines vector, field trust, crossposts. Final report linked
- Overview :lines is computed by the server (RFC 3977 erratum 1932; INN innd/art.c counts body lines and ignores the client's Lines: header). No checked poster sends Lines:. So the per-part :lines vector is a header-independent twin key from the overview row: ~1.5-2 bits/part (escape-count variance), ~100 bits for a 70-part file. Unlike :bytes, it survives a repost with new Subject/From/Message-ID, given the same line length and escape rules. Other backends are unverified (check for blank or 0 values).
- Field trust (SABnzbd/NZBGet/pesto/nzbkit source): pcrc32, crc32, =yend size, =ypart begin/end and single-part size are content-bound, so they are identity evidence. name=, line=, total= presence and =yend part= presence are ignored by downloaders, so they are tool fingerprints only. Obfuscators (Nyuu ${rand}, ngPost --obfuscate) change only the subject, name=, From and Date.
- Crosspost vs multipost: same Message-ID in several groups = one article (dedupe by ID, store the Xref groups). postie's default each_file policy spreads files over random groups (multipost pattern). Xref numbers are per server and gappy, so they are not timing evidence. NNTmux merges by sha1(name + totalFiles) across groups; nZEDb only does so for a manual multigroup poster list.
- Final report (Claude Doc): https://claude.ai/artifact/ExnooEu7pBdBqoNvXeAbQT

## Haiku research: post linking via payload fingerprints — wave 9 (2026-11-02 11:00Z)
- par2cmdline default slice, from source (commandline.cpp L1192-1207, master): blocksize = (totalsize+1999)/2000, min 4, rounded up to a multiple of 4, totalsize = ALL input files; -b 2000 is help text only. par2cmdline-turbo uses the same formula (L1224-1241) and 0x1100B. ParPar gf16 0x1100B confirmed. So for default par2cmdline posts the exp-0 slice is a pure function of the input bytes, but adding/removing ANY file changes the slice size and breaks it (use IFSC/FileDesc per-file keys then). MultiPar/par2j and QuickPar still UNVERIFIED (source unreachable). w9a.md
- srrDB as scene oracle: /api/search/archive-crc:<hex> searches by inner-file CRC32 (also archive-size keyword, syntax UNVERIFIED); /api/details/<release> gives files[] name/size/crc for confirmed releases. Cost: 1 article (last RAR volume header -> inner CRC) + 1 API call -> names an obfuscated non-encrypted scene repost (proves alt/twin to the scene original). No published rate limit; /api/ is robots-disallowed, so use sparingly or ask srrDB. -hp sets hide the CRC. w9b.md
- Test corpora for Rust unit tests: rarfile test/files (106 RAR3/RAR5 files incl. multivolume, -hp, QO, with .exp oracles; ISC believed) is the best single source; par2cmdline tests/*.tar.gz (~18 PAR2 sets, GPL: fetch at test time, don't vendor); sanderjo/NZBs (no license: don't commit). Generate yEnc/PAR2/7z fixtures ourselves. w9c.md

## Research post linking via yEnc headers (Opus 3x3) — wave 8 (2026-11-02 10:58Z)
Reports: research/yenc-header-forensics/opus/w8a-old-v8-solve.md, w8b-cross-nzb-stream.md, w8c-fingerprint-table.md
- Stock Nyuu (Node 12+): all 19 solved NZBs now link 100% of IDs onto one stream each (up to 16,488 IDs). The earlier partial counts were a cache-block alignment bug, fixed by trying all 127 shifts (nyuu-id-solver/stream_link.py). Two IDs recover the state. One state predicts every article of that process; scanning an index is a hash-set lookup, false match about 2^-113.
- No cross-NZB links: each NZB came from its own Nyuu process (14 states stepped 1e9 draws, 0 hits, even for posts 3 minutes apart). Cross-post linking via RNG only works for tools that run several posts in one process.
- Old Nyuu (Node 11 or older, 2017-2022): CryptoMiniSat with native XOR clauses also fails without about 48 known state bits. Still unsolved; next try guessing top state bits or decoding carries as noise.
- Consolidated fingerprint table: 33 tool variants + 6 server ID families, each with Message-ID regex, From, Date form, header order, segment size, NZB writer tells and link method, plus a 9-step classifier order for XOVER/HEAD data. 21 conflicts listed against research/yenc-header-forensics/fingerprints.md. Notable: fingerprints.md swaps the JBinUp and JBinDown regexes; Nyuu redraws the 24 letters on every send attempt including retries (only the ms suffix survives); linking-findings line 4's "700K vs 716800" is the same size (700 KiB).

## Research post linking via payload fingerprints — wave 9
- **The size key on real, clustered sizes** (36,215 Redump disc images): the encrypted-repost key (V, n, S_last) falls to ~8 bits pooled, because 44% of images share an exact size (fixed media caps). With exact-size duplicates removed it is back to ~27 bits; clustered but not identical sizes give ~23 bits. So it works for encoded video but not for padded disc images or remuxes at fixed caps (GameCube/Wii ~2 bits). Use the disc IDs (wave 8) or a sample's exact size there. An nfo size adds only ~14 bits. Scripts: opus-tools/w9a_*.
- **Pure-Rust CRC32 GF(2) toolkit built and tested** (std only, 320 lines, 8 property tests incl. a >4 GiB direct check): combine, remove_prefix, remove_suffix (x^-8n), zero_extend/zero_unpad, fold_yenc_parts, fold_par2_slices (unpads the last slice), slices_from_parts. ~400 ns per op at n≈1e10; folding a 10 GB file takes ~0.3 ms. Ready to port: opus-tools/w9b_crc32gf2/.
- **Payload-level tool tells** (opus-tools/w9c_tool_tells.md): RAR5 has no version number (only rar ≥7.0 feature bits); host-OS byte splits Windows/Unix; 7z version bytes 00 04 vs 00 03 (libarchive) plus header property order separates 7-Zip/libarchive/py7zr; PAR2 creator strings name par2cmdline(-turbo)/ParPar(+arch)/MultiPar par2j/QuickPar; ngPost slices of exactly 5 MiB or 768000 B; NxG Upper uses one 21-char random name for rar and par2.
- **Wave 10 (last):** signal inventory draft for the final report, a Rust PAR2 + RAR header walker on the rarfile test corpus, and an adversarial critic of the top keys.

### Haiku 15-min posting behavior: Wave 9
- **`$0$` Message-IDs are Diablo dreaderd (verified in source)**: dreaderd/subs.c GenerateMessageID prints `<%08lx$%d$%d$%x@%s>` = hex time $ per-second counter $ PID $ shash(hostname) @ cluster name. A Python port of shash reproduces real IDs exactly: news.astraweb.com -> 0c3e8da3, news.kabelfoon.nl -> 58c7af7e, news.xs4all.nl -> e4fe514c. This corrects my wave 8 note: field 3 is the dreaderd process ID, so (host hash, PID, time window) is a soft session key, and the counter orders posts made by one process within a second. It is weaker than an INN nnrpd PID because one dreaderd process serves many connections. Astraweb adds a fifth 8-hex field that stock Diablo lacks; its meaning is unknown.
- **Postie header order is FIXED (verified)**: Postie pins nntppool v4.22.3, and the posting path ends in PostHeaders.WriteTo (post.go:168-236): From, Newsgroups, Subject, Message-ID, Date, then extra headers sorted. Postie's own map-ordered article.Encode is dead code. This settles the open conflict.
- **Postie dispatch (verified)**: one provider per article by weighted round-robin (weights = free connections times a speed score), with no backup tier for uploads. Status errors are not retried; transport errors move to the next provider. Retries keep Message-ID, From and Date. Postie also has an "nxg" ID format (SHA256-derived, `40@21.3`) and an encrypted per-file X-Nxg header.
- **ngPost retries change the Message-ID (verified)**: tryResend() makes a new UUID each time (up to 5 tries), and an obfuscated From is re-randomized on every write. So a retried ngPost article has no header in common with its first attempt; only the yEnc part and body tie it back. The domain after @ is configurable (default ngPost), so match the 32-hex-UUID shape, not the suffix. Failed articles return to the shared queue (the build defines __RELEASE_ARTICLES_WHEN_CON_FAILS__).
- **`@UFK` IDs are provider-assigned, not NewsUP**: NewsUP's source has no UFK. Without GENERATE_IDS or OBFUSCATE, NewsUP sends no Message-ID at all and the server fills one in. NewsUP's own ID shape is `<sec><usec>.<base36>@<base36 time>.newsup`.
- **Arrival order is never guaranteed**: Nyuu (FIFO queue, up to 25 in flight), ngPost and NewsUP all let parts finish out of order across connections, and post-check reposts arrive later still. Ordering by article number gives arrival order, not part order.

### Haiku yEnc header forensics, wave 9 (11:01Z)
- Rust classifier spec validated on 72,334 segments (labels match the Python run for all 21 labels; 4 fixes appended to raw/w08-c-rust-spec.md). Nyuu ms within ±1 day of the NZB date 99.56%; Highwinds T1 869/869 within ±1 hour.
- Nyuu and ngPost retries do not show in NZBs: 332 public NZBs had no retry pairs. Only the ID that succeeded is written; an accepted-but-retried attempt becomes an orphan article.
- Only 3 of 246 NZBs are genuinely two tools, and that itself points to a merged or repaired post.
- NZB cf32b14460 uses a fresh random From per FILE (44 volumes of 68 segments plus a PAR2 set with 2^k+1 counts). Per-file From obfuscation means one release spans many posters, so join by time and size shape, not From.

## Research post linking via posting behavior (Opus) — wave 9
- REAL DATA (246 public NZBs, 202 distinct):
  - End-to-start join holds for Nyuu uploads: files go out in series, never in parallel. Using the ms decoded from the Message-ID, the gap from one file's end to the next file's start has median 0.12 s and p90 0.67 s, and 98% of gaps are under 2 s (1,070 gaps).
  - NZB dates in whole seconds are too coarse: only 38% of gaps fall under 2 s.
  - NZB file order matches time order in only half the NZBs, so sort by time first.
  - Re-linking time-shifted real NZBs by gap only: precision .93 at 2 concurrent uploads, .60 at 10 starting together, about .98 when 10 start spread over an hour. Adding a size-like feature lifts precision by .15-.25. With whole-second dates only, precision is about chance level.
  - Highwinds: S is constant in every NZB, and N rises with article order in 861 of 862 steps. N steps of +2 or +4 are other traffic on the same front end, so N is a per-front-end counter.
- NO-PAR2 FALLBACK:
  - About 48% of public NZBs show PAR2, rising to 77% of NZBs with 3+ files. Estimate: 15-30% of multi-file uploads have no PAR2. Single-file posts usually have none.
  - ngPost's --par2_pct defaults to 0, but its sample config sets 8. Nyuu needs a separate ParPar run. postie (10%) and NewsUP (15%) make PAR2 by default.
  - Fallback order: the PAR2 index even when vol files are missing; yEnc headers; the first article of each run read as a container header (RAR volume number and flags, 7z .001); the last RAR volume's inner CRC; SFV, zip central directory or NFO; Nyuu's =yend crc32; X-Received-Body-CRC; a full pcrc32 fold, only for posts Atlas already downloads.
  - About 1 body per volume. The weak case is RAR5 -hp with no PAR2 and equal volume sizes: 65-80% correct, flagged low confidence.
- RUST PIPELINE SPEC (wave-09-A-pipeline-spec.md): 13 stages with read-cost tiers, struct sketches, thresholds from the simulations, online re-evaluation, 8 test cases including Batman Knightfall.
  - Recommends our own veto-aware union-find (about 80 lines). Crates: only crc32fast, md-5 and smallvec.
  - Biggest cost: with obfuscated subjects, file identity needs the yEnc header from every article body.
- PEERS:
  - INN assigns all Xref numbers of a crosspost in one call, so Xref pairs anchor group counters to each other exactly.
  - Injection-Date is added only when the Message-ID or Date is missing, so ngPost posts get it and Nyuu and postie posts usually do not.
  - Diablo $-IDs carry the time, a per-second counter, the PID and a hostname hash, which gives a (host, PID) session key.
  - postie's PAR2 FileDesc keeps the real file name, a free label for validation.
  - A std-only Rust CRC32 GF(2) toolkit exists in research/payload-fingerprints (opus-tools/w9b_crc32gf2/).
Wave 10 plan (last):
- (A) Red-team the pipeline spec against all 9 waves plus peer findings: contradictions, missing cases, wrong thresholds.
- (B) Atlas-side measurement plan: which header-log fields and Grafana metrics would calibrate the thresholds on live data (gap distributions, concurrency per front end, PAR2 presence, the spray rate).
- (C) Compile the signal table for the final report: each signal × what it proves × read cost × tool coverage, across all waves.
Details: research/posting-behavior/wave-09-*.md

## Research post linking via payload fingerprints — wave 10 and final report
- **Final report:** https://claude.ai/artifact/Kokmceq4dVK46C4oDHaKgD (Markdown: research/payload-fingerprints/final-report.md). About 50 payload signals by what they prove × article cost, with an Atlas coverage column, a merge policy, measured results and Rust build notes.
- **Critic verdict:** no key should merge alone. Merge twins only on size + CRC32 + 16k or full MD5; give no fold key if any part lacks pcrc32 (optional in yEnc 1.3). Key PAR2 on (16k MD5, length) without the name. Use inner CRC for alts only on main files of 50 MB or more with two matches. The encrypted size tuple is for blocking only. Use the escape-count index only on NZBs whose segment sizes vary. Use oracles to corroborate only.
- **Rust prototypes:** a std-only PAR2 + RAR4/5 partial-buffer walker (opus-tools/w10b_walker/) passed 332/332 checks against the rarfile `.exp` corpus, with the same result on 700 KB prefixes. It has a CRC32 GF(2) toolkit (opus-tools/w9b_crc32gf2/).
- **Biggest Atlas gaps** (from the archive-metadata review of a src copy): pcrc32 is not stored, PAR2 IFSC is not parsed, inner CRCs from RAR, 7z and ZIP are read then dropped, RAR has no tail read, and the PAR2 Creator and tool tells are dropped.

### Haiku 15-min posting behavior: Wave 10 and final report
- Final report (Claude Doc): https://claude.ai/artifact/SdC4pYwLdtnPpd1KjXBhDd. Signal map, tool fingerprint table, server-ID session keys (INN, Diablo, Astraweb, Highwinds), gap thresholds, merge windows, validation procedure, live checks.
- Wave 10: Highwinds K is constant per post and differs between posts on the same day; N steps +1/+2/+4. The T1.N$K.R family also appears on Usenetserver, Giganews and Easynews; Diablo's form on Usenetserver and Newshosting. Astraweb's 5th field is per-article and not derived from the other fields. Gap thresholds: split inside a file at >2 s (ngPost, Nyuu) or >5 s (Postie); between uploads >=90 s (ngPost), >=300 s (Postie). Postie overlaps two files, so never cut on file gaps. Validation: Binette et al. sampled-cluster precision (arXiv 2404.05622) plus Splink bridges.

## Research post linking via posting behavior (Opus) — wave 10 (final)
- RED TEAM of the wave 9 pipeline spec found 14 issues. The four most serious:
  - Cost tiers: with per-article random subjects, file identity needs every body. Test 1's 50-body budget only holds when subjects are stable or Nyuu ms identity is available.
  - The +0.5 s join window misses 15% of real Nyuu gaps. postie sends 2 files at once, so its files overlap. Windows must be set per tool.
  - Injection-Date is missing for Nyuu and postie posts. The Nyuu Message-ID ms is the best clock and must be used.
  - A hash16k match proves which volume a run is, not which upload it belongs to. Twins share it, so dedupe must key on (msgid, group, num) and output twin groups instead of merging them.
  - Also: veto only on content (part size, total, CRC), never on tool or :bytes. Highwinds S looked per-upload in 7/7 real NZBs. Add the container-header fallback. Build runs once per msgid for crossposts. Reopen runs on late parts and hold publication about 4 h.
  - Minimal v1: ingest+dedupe, then file runs, then the end-to-start join, then content resolve (PAR2 → sizeChain → hash16k, or the container header when there is no PAR2), then union and output with Proven/SizeMatched/TimeOnly levels plus twin and alt groups.
- LIVE CALIBRATION PLAN:
  - Fields to persist per article.
  - About 15 low-cardinality Prometheus metrics, LAN only, with no names or IDs in labels.
  - 4 ground-truth sources: clear subjects with labels masked, external NZBs, postie PAR2 real names, and Nyuu ms. A 20% holdout.
  - Rollout: instrument and run the probe, then shadow for 2 weeks, then fit thresholds, then promote per confidence level. Promotion needs precision ≥.98 for 7 days and ≤1 body per file.
  - 8 Grafana panels with queries.
- FINAL TABLES: 30 signals × proves × read cost × evidence × Atlas status, a 7-tool fingerprint table, 8 provider families, and 19 dead ends.
Final report: research/posting-behavior/FINAL-REPORT.md (doc: https://claude.ai/artifact/UADbR1wn9f5MGHs5TJRRHm)

## Research post linking via yEnc headers (Opus 3x3) — wave 10, final (2026-11-02 12:20Z)
Final report: research/yenc-header-forensics/opus/FINAL-yenc-header-forensics.md. Wave reports w10a-node4-mwc1616.md, w10c-twin-key-robustness.md.
- Node 4 Nyuu (v0.3.x binaries) solved: V8 4.5 MWC1616; letters depend only on low16(r0), ~45M candidates per ID, 0.04 s. 8 real NZBs from 2017-07 to 2019-02 link 100% (up to 1654 IDs). Still unsolved: Mar/Apr 2018 and 2019-06 to 2023-04 (~60 NZBs), likely npm Nyuu on Node 6-11.
- Size-vector twin key limits: exact match only below 6 segments; n-1 tolerance safe from ~6 segments (1e5 files), 7 (1e6), 8 (1e7); n-2 never. Add a second blocking key on segments 3-4. sd floor 40 for confirmation. Treat it as a candidate generator and confirm with size + CRC32 + MD5, as the payload thread found.
- Regrouping files without From/Subject: time + size shape gives precision only 0.27-0.42 when uploads overlap; use Nyuu ID stream continuity, with size shape only as a tie-breaker.
