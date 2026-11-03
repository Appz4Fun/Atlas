# Usenet metadata, correlation and deobfuscation: merged findings for Atlas

Tags like [sab-deobf] resolve to URLs in section 4. **Atlas**: yes; part (partly kept); no (not in Atlas's feature list); ? (can't tell). **Match level**: twin = byte-identical; alt = same inner file, new wrapper; encode = same encode, new container; film = same title; post = same upload or NZB; uploader = same poster, tool or session.

## 1. Metadata fields found in the wild

### 1.1 NNTP transport and overview

| Field | Sources | Atlas |
|---|---|---|
| OVER/XOVER: artnum, Subject, From, Date, Message-ID, References, `:bytes`, `:lines`, then LIST OVERVIEW.FMT extras (`Xref:full` keeps the header name) | [rfc3977] [rfc2980] [nntplib] | part: no References, `:lines`, extras |
| `:bytes`/`:lines` (server) vs `Bytes:`/`Lines:` (poster; a faked Lines is a tool tell) | [rfc3977] | part |
| Article number: per group, rising with arrival; number-vs-date binary search finds retention edges | [rfc3977] [nntmux-bin] [newznab-kop1] | ? |
| HDR/XHDR/XPAT over ranges (`LIST HEADERS` answer `:` = any header); LIST COUNTS, ACTIVE status `y n m x j =alias` | [rfc3977] [rfc2980] [rfc6048] | ? |
| Compressed overview: COMPRESS DEFLATE; XZVER/XZHDR (yEnc-wrapped raw deflate); `XFEATURE COMPRESS GZIP [TERMINATOR]` (GZIP credited to Giganews, XZVER to Astraweb). Highwinds/eweka lack COMPRESS and send zlib under the GZIP label: sniff 0x78/0x1f/raw; terminator inside the stream | [rfc8054] [willglynn] [spottarr] [pynntp] [ietf-nntp-2009] | ? |

### 1.2 Article headers, including server-injected

| Field | Sources | Atlas |
|---|---|---|
| Injection-Info `posting-host`, `posting-account`, `logging-data`, `mail-complaints-to`. INN: posting-host `host:ip`, logging-data = nnrpd PID, posting-account off by default; eternal-september posting-host = MD5(IP) | [rfc5536] [inn] [inn-2014] [hashcat] | part: one string |
| Injection-Date; NNTP-Posting-Host/Date (obsolete) | [rfc5536] [rfc5537] | yes |
| Path diagnostics `!.POSTED[.host]`, `!!`, `!.SEEN.`, `!.MISMATCH.`; Highwinds `fx06.iad.POSTED` names the front end; some providers send only `not-for-mail` | [rfc5537] [pcreview] [leo] | part: raw |
| Xref `server group:number …` | [rfc5536] [nntmux-coll] | part: one article |
| X-Trace: INN `<host> <unix> <pid> <ip> (<date>)` (IP may be rewritten); encrypted provider tokens (`sv3-…`) | [big8] [inn-2011] [harley] | no |
| X-Received-Bytes, X-Received-Body-CRC (decimal; algorithm undocumented) | [pcreview] [leo] | no |
| X-Usenet-Provider, X-Complaints-Info, X-Originating-IP, X-Original-NNTP-Posting-Host, NNTP-Posting-Path, X-User-ID, X-Auth, `Old-*` (reinjection) | [inn] [harley] [big8] | no |
| Cancel-Lock/Cancel-Key `sha256:`/`sha512:`/`sha1:`: per-message HMAC, so no linking; the scheme fingerprints software | [rfc8315] [inn] | no |
| Supersedes, Control, Approved, Archive/X-No-Archive, Distribution, Followup-To, Original-Sender, X-Face, X-PGP-Sig; obsolete Lines, Posting-Version, See-Also date the software | [iana] [rfc5536] [xnoarchive] | no |
| Non-yEnc multipart: References/See-Also, MIME `message/partial` id/number/total; RFC 2047 encoded-words; RFC 2231 `filename*N*=` | [rfc1036] [rfc2046] [rfc2231] | ? |
| Tool headers: `User-Agent: Nyuu/<ver>`, `Newspost/2.2.1 (url)`; `X-Newsposter: newsmangler <ver> (<mode>) - <repo>` (trailing extra CRLF), `gopoststuff <ver> - <repo>`, `sanguinews v<ver> (ruby <ver>)` | [nyuu] [newspost] [newsmangler] [sanguinews] | yes: one article |
| `X-Nxg`: `fileNumber:totalFiles:filename:partType:totalSize`, encrypted with the NxG header | [nxg-go] | no |
| Signed metadata: `Verify-Signature-newzNZB`, `X-Ufk`; Spotnet `X-XML`, `X-XML-Signature`, `X-User-Key`, `X-User-Signature`, and category/size/date packed into From | [newznzb-sig] [leo] [spotweb] | no |

### 1.3 yEnc

| Field | Sources | Atlas |
|---|---|---|
| `=ybegin part total line size name` (name last; no part = single-part); `=ypart begin end` (1-based); `=yend size part pcrc32 crc32` | [yenc13] [sabctools] | yes |
| Trailer variants: ngPost/NewsUP omit `part=`/`crc32=` and don't zero-pad pcrc32; newsmangler omits `crc32=`; GoPostStuff uppercases pcrc32; sanguinews gets begin/end wrong on a short last part; Nyuu writes `crc32=` on the last part only; 0 of 500 large releases had a whole-file crc32 | [ngpost] [newsup] [newsmangler] [sanguinews] [kd476] | part |
| Body style: rapidyenc (Nyuu) lines are 128 or 129 bytes, TAB/SPACE escaped only at line edges; NewsUP/ngPost leave line-final TAB/SPACE raw; newspost escapes TAB anywhere, never SPACE | [rapidyenc] [newsup] [newspost] | no |
| Parsing: match `=ybegin ` with its space (`=ybegin2` reserved, V2 never shipped); skip stray `=y` lines; uuencode still appears | [yenc13] [yenc-notes] [sabctools] | ? |
| Encrypted yEnc: FF1-encrypted keyword lines, `=yencryption` line, XChaCha20-Poly1305 body; SABnzbd PR closed unmerged | [yenc-enc] [sab-3676] | no |

### 1.4 NZB and indexer feeds

| Field | Sources | Atlas |
|---|---|---|
| `<meta>` title, category, tag and password (repeatable), `X-*`; NZB 1.2+ per-file meta and password; v3 file@bytes/@segments | [nzb-spec] [newzbin-specs] | ? |
| file@poster, @date (server-seen), @subject (`(xx/yy)` → `(1/yy)`); segment bytes/number/Message-ID; Nyuu can make NZB subject/poster differ from the articles | [nzb-spec] [nyuu] | yes (from XOVER) |
| Newzbin DirectNZB HTTP headers `X-DNZB-Name/Category/MoreInfo/ProperName/EpisodeName/Year/Password/Details/Failure` | [sab-urlgrabber] [hellanzb] | ? |
| Links: NZBLNK `h p t g d`; `nxglnk://?h=&p=&t=` | [nzbmonkey-go] [nxg-upper] | no |
| newznab attrs `password` 0/1/2 (2 = inner archive), `prematch` | [newznab-api] | ? |

### 1.5 PAR2 (and PAR1, PAR3)

| Field | Sources | Atlas |
|---|---|---|
| Main, FileDesc (File ID, MD5, MD5-16k, length, name), UniFileN, set ID. File ID = MD5(MD5-16k, length, name) and set ID = MD5(Main body), so both change on rename | [par2] [sab-par2] | yes |
| IFSC: MD5 + CRC32 per slice → whole-file CRC32 of each protected file (usually the RAR volumes) | [par2] [sab-par2] [nzbget-par] | no |
| Creator: `Created by par2cmdline version X.Y.Z.`, `ParPar v<ver> <arch> [url]`, `par2j v1.3.3`; CommASCI/CommUni comments | [par2cmd] [parpar] [par2j] | no |
| Par profile: slice size (`-s2000` = par2cmdline default), `vol12-22` vs `vol12+10`, pow2 volume sizes | [parpar] | no |
| PAR1 set hash = MD5 of the file MD5s (renames don't change it); PAR3 draft: BLAKE3, CRC-64 rolling hash, owner/group names | [par1] [par3] | no |

### 1.6 SFV, NFO, SRR, SRS, logs

| Field | Sources | Atlas |
|---|---|---|
| SFV generator line (`; Generated by WIN-SFV32 v1.1a … on <date> at <time>`), per-file size+mtime comments, CRC case | [sfv-sample] [rarinfo] | no |
| NFO: IMDb `tt\d{7,}`, TVDB, TMDb, AniDB, ISBN/ASIN/UPC; hidden NFO = one-segment `(1/1)` file without extension, 12–65535 bytes; old `(0/n)` posts carry NFO text | [nntmux-rfm] [nzedb-namefixer] [geocities-yenc] | ? |
| SRR blocks 0x69 header (app), 0x6A stored file, 0x6B OSO/ISDb hash, 0x71 RAR volume + its raw headers; SRS `SRSF` (sample name, size, CRC32), `SRST` track signatures | [rescene-go] [pyrescene] [zeebinz-srr] | no |
| EAC log `==== Log checksum <64 hex> ====` (public key: shows only no hand edits) | [eac] | no |

### 1.7 Archives and disc images

| Field | Sources | Atlas |
|---|---|---|
| RAR4: MHD VOLUME 0x01, SOLID 0x08, NEWNUMBERING 0x10, PASSWORD 0x80 (encrypted headers), FIRSTVOLUME 0x100; LHD SPLIT_BEFORE/AFTER 0x01/0x02, PASSWORD 0x04, UNICODE 0x200, SALT 0x400; ENDARC volume number; UNP_VER 29/50/70 | [unrar] [forensics-rar] [nzbget-rar] | part: names, sizes |
| RAR5: volume number; Metadata record (original archive name, creation time); data CRC32; BLAKE2sp; ns times; Unix owner names/IDs; encryption salt/KDF and tweaked-checksum flag; CMT, QO, RR. Split files: only the last part's CRC is the inner file's | [rar5] [rarinfo] | part |
| 7z: NextHeaderCRC, kName, kCTime/kATime/kMTime, kWinAttributes, kComment, per-file CRC, kEncodedHeader (names hidden), 7zAES 06F10701 | [7z] [rarinfo] | part |
| ZIP: GP bits 0/3/11/13, version-made-by OS, extras 0x5455 (times), 0x7875 (uid/gid), 0x7075 (Unicode path), 0x9901 AES (AE-2 sets CRC to 0) | [appnote] [winzip-aes] | part |
| Inner-file byte range in each volume | [rarinfo] | ? |
| ISO 9660 PVD IDs and dates; UDF VolumeSetIdentifier (first 8 hex = time) | [pycdlib] | no |

### 1.8 Media, subtitles, audio, images

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

## 2. Correlation and grouping techniques

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
- **Tool fingerprints.** Header set (ngPost sends only From, Newsgroups, Subject, Message-ID), article size (716800 Nyuu/ngPost, 768000 GoPostStuff, 750000 Postie), yEnc body style, PAR2 Creator and slice size. NewsUP `--obfuscate` mixes 8 imitation Message-ID formats in one file (fake `@nyuu` with a ~12-char local part), so validate shapes strictly. [ngpost] [postie] [newsup] [rapidyenc] → uploader
- **Signed identity.** Spotnet spotterid = base64(little-endian crc32(decoded user key)) minus `/+=`; self-signed spots need sha1(`<msgid>`) starting `0000`; spot XML lists NZB segment IDs. [spotweb] [spotnet-wiki] → uploader, post
- **Metadata duplicates.** NZBHydra2: same normalized title, different indexers, equal poster/group when known, ≤2 h apart, size within 1% of the mean, both doubled if only one of poster/group is known. nZEDb: same name+poster+size is a dupe within a group, a crosspost across groups. [hydra] [nzedb-wiki] → post
- **Perceptual matching.** PDQ Hamming ≤30; TMK both levels ≥0.7; vPDQ quality ≥50, distance ≤31, ≥80% frames; FFmpeg `signature` th_d 9000, th_dc 60000, th_xh 116; audio BER <0.35, failing past ~2.5% speed change (PAL ~4%); AcoustID: three random 30 s samples away from the first/last 10 min; runtime off by >10% flags fakes. [pdq] [vpdq] [ffmpeg-sig] [haitsma] [acoustid] [liang] → film
- **Behavioural attribution.** Many usernames per IP flags fake publishers; automated posters start one-message threads with no replies; spam-template inference (anchors ≥6 chars); Nilsimsa match ≥24. [cuevas] [picturing] [judo] [nilsimsa] → uploader
- **Canonical copy and survival.** Keep the earliest Date, then the lowest Message-ID; spew score Σ√groups (≤20 per 45 days). Backbones dedupe and take down only by Message-ID, so twins with new IDs expire separately; STAT/HEAD can pass while BODY is gone, so probe BODY and count 451 apart from 430. [care] [breidbart] [perfect10] [rexum] [nyuu-146] → twin selection

## 3. Deobfuscation techniques

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

## 4. Sources

### Spec

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

### Open source

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

### Paper

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

### Forum and reddit

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

### Usenet

- [newznzb-sig] newz[NZB] uploader signature header on a real obfuscated post: <https://raw.githubusercontent.com/scriptzteam/Usenet_-_Verify-Signature-newzNZB/master/EXAMPLE_WEB.BROWSER> · <https://raw.githubusercontent.com/scriptzteam/Usenet_-_Verify-Signature-newzNZB/master/README.md>
- [anon-server] alt.privacy.anon-server, hashed NNTP-Posting-Host and posting-host: <https://groups.google.com/g/alt.privacy.anon-server/c/XI1DhRI0wr0>
- [nsr-2008] news.software.readers (January 2008), how providers encode or hash X-Trace and posting-host: <https://groups.google.com/g/news.software.readers/c/cBuXeYBT6c0>

### IRC

No public channel logs were found; these are the pre-bot scrapers, scripts and schemas.

- [nzedb-irc] nZEDb `nzedb/IRCScraper.php`: <https://raw.githubusercontent.com/nZEDb/nZEDb/master/nzedb/IRCScraper.php>
- [nzedb-ircbots] nZEDb_IRC_Bots `IRCScraper.php`, predb schema and request-channel announce formats: <https://github.com/nZEDb/nZEDb_IRC_Bots/blob/master/PHP/Classes/IRCScraper.php>
- [prenet] prenet-4.tcl pre bot: <https://raw.githubusercontent.com/scriptzteam/SCENE-SCRiPTS/master/Scene/prenet-4.tcl>
- [topclient] TopClient and predb site SQL schemas: <https://raw.githubusercontent.com/scriptzteam/SCENE-SCRiPTS/master/TopClient-save-nfo-sfv-top-master/TopClient.sql> · <https://raw.githubusercontent.com/scriptzteam/SCENE-SCRiPTS/master/website-predb-master/table.sql>

### Archive

- [titz] Olaf Titz, draft X-Trace protocol (inn-workers, July 2000): <https://lists.isc.org/pipermail/inn-workers/2000-July/003269.html>
- [inn-2011] inn-workers, October 2011, rewriting the IP in INN's X-Trace: <https://lists.isc.org/pipermail/inn-workers/2011-October/017940.html>
- [inn-2014] inn-workers, November 2014, INN posting-account: <https://lists.isc.org/pipermail/inn-workers/2014-November/018334.html>
- [ietf-nntp-2009] ietf-nntp, November 2009, origins of COMPRESS GZIP and XZVER: <https://lists.eyrie.org/pipermail/ietf-nntp/2009-November/006066.html>
- [newznzb-log] newzNZB Usenet Upload Logger, daily release-name logs (2017-11-17 file): <https://raw.githubusercontent.com/scriptzteam/newzNZB-Usenet-Upload-Logger/master/2017/11/17/newz%5BNZB%5D_uploads.txt>
- [geocities-yenc] yEnc FAQ for alt.binaries.sounds.music.classical (geocities.ws mirror, ca. 2002): <https://www.geocities.ws/altbinariessoundsmusicclassical/yenc.html>
- [newzbin-specs] Newzbin NZB Specs, versions 0.9 to 3.0 (copy of docs.newzbin.com): <https://www.uzantoreto.com/Newzbin:NZB_Specs>
- [fanzub] Fanzub anime indexer source and 2015-03-01 database dump (archive.org; a possible grouping test corpus, contents not inspected): <https://archive.org/metadata/fanzub> · <https://archive.org/download/fanzub/fanzub-source-code.zip/>
- [sfv-sample] Morgoth MP3 Releaser SFV (WIN-SFV32/QuickSFV header sample): <https://raw.githubusercontent.com/scriptzteam/SCENE-SCRiPTS/master/Others/Other/release-tools/Morgoth.MP3.Releaser.v3.2.0.WinALL-HiEM/Morgoth.MP3.Releaser.v3.2.0.WinALL-HiEM.sfv>

### Other web pages

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

## 5. Dead ends

- Wayback Machine and its availability API unreachable (SITE_BLOCKED, proxy 403); yenc.org robots-disallowed (mirrors used).
- Reddit, old.reddit, pullpush blocked (two threads via redlib); most SABnzbd, NZBGet, Newsbin forum threads failed; forums.nzedb.com parked; no public IRC logs for #sabnzbd, #nzbget, #newznab, #nzedb.
- No academic work on correlating or deobfuscating binary posts beyond the GIAC primer [giac]; Kim et al. 2010 and Schneider ch. 7 [schneider] unreachable.
- Undocumented: X-Received-Body-CRC algorithm, Giganews `sv3-`/`DXC=` X-Trace, Postie's Message-ID domain and X-NXG payload, private indexers' hashing.
- Last-segment yEnc crc32 → srrDB: wontfix (0/500 coverage, 0/53 hits).
- No indexer documents grouping by posting-account (INN omits it by default).
- predb.ovh live API robots-disallowed; srrDB `/details` schema undocumented.
- Moved code: SABnzbd `nzbstuff.py` → `nzb/*.py`; NNTmux `Blacklight/*` → `app/Services/*`; MKVToolNix on Codeberg; libdvdread/libbluray/libaacs paths 404. Current NNTmux lacks `nzb_guid` and predb hash tables.
- Still open: PDF/EPUB metadata, QuickPar and MP4Box strings, closed-source uploaders' Message-ID formats.