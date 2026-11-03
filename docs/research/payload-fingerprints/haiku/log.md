# Payload fingerprints, Haiku agents (Opus main session)

10 waves x 3 Haiku subagents, ~10 min search each, ~30 min per wave. Started 2026-11-02 06:45Z.

## Wave 1 (06:45Z), broad sweep
- A: PAR2 internals beyond set ID / MD5 / 16k MD5 (creator strings, slice sizes, IFSC packets, deobfuscation)
- B: RAR4/RAR5/7z/ZIP header fields as twin/alt/packer fingerprints
- C: posting-tool obfuscation fingerprints, indexer deobfuscation, partial hashing (CRC combine)

## Sharing (the owner 06:44Z: all groups communicate)
After each wave: message_thread a short summary to the other 9 threads, append best leads under "## Haiku research: post linking via payload fingerprints" in docs/research/linking-findings.md, and fold incoming findings into the next wave.
Others: catalog cmsg_014FRqgd95UUF8Mvov1QRBWrQRvGqGQ11Ug8Fy3joSUAvf; opus cmsg_014FRqgd95UUF8Mvov1QRBWrNjSX8S5dKnseDjkXiz1L45, cmsg_014FRqgd95UUF8Mvov1QRBWrVAsQ842SMP5j5rPUMk322k, cmsg_014FRqgd95UUF8Mvov1QRBWrJ9gfzLaZtXQCsj3gDW1s1Z; haiku10 cmsg_014FRqgd95UUF8Mvov1QRBWr32R21cxmxbYS5gZFaqvbbZ, cmsg_014FRqgd95UUF8Mvov1QRBWrH9w4Fv7jgdKWEeCawC3t8S; haiku15 cmsg_014FRqgd95UUF8Mvov1QRBWr5eCGzdHXeaFbFAKokjDkrV, cmsg_014FRqgd95UUF8Mvov1QRBWrL1M1cF5Q6PLij2VJnxMJSG, cmsg_014FRqgd95UUF8Mvov1QRBWrBQpoS35Uct66oBKpWbtLk8

## Incoming from siblings before wave 2
- Opus yEnc thread: NNTmux already joins on RAR inner CRC32, PAR2 hash16k, MKV SegmentUID; ngPost body-byte quirks (unpadded pcrc32, no crc32=); GoPostStuff uppercase pcrc32.
- Opus payload thread: crc_concat over pcrc32 gives whole-file CRC and CRCs of shared intervals (animetosho crc_manip.py); PAR2 set ID deterministic (PAR3 random); 7z NextHeaderCRC and RAR5 salts as per-creation random twin keys; ssdeep/TLSH/CDC dead on compressed media.
- Haiku15 payload thread wave 2: nZEDb/NNTmux prior art, srrDB oracle, obfuscation schemes. Avoid overlap.
- Haiku15 payload wave 2: NNTmux par_hashes/release_files.crc32 only used for PreDB renames; hash16k collides across RAR volumes of one packer; srrDB /search/ takes ArchiveCrc/RarHash/OSHash; Rust prior art scryer-media/rarpar. Their wave 3 = MKV SegmentUID byte layouts, so my W2C should lean to non-MKV media (x264 SEI, FLAC MD5, MP4, ISO/BDMV, ebooks, audio tags).
- Container-metadata thread covers MKV SegmentUID/DateUTC, MP4 mvhd, BDMV disc_id, DVD IFO MD5. So W2C: skip video containers; do elementary streams (x264/x265/AV1 encoder SEI strings), audio (FLAC STREAMINFO MD5, LAME/Xing, AccurateRip/CUETools CTDB CRCs from EAC logs), ebooks (EPUB uid, PDF /ID), software/ISO (ISO9660 PVD volume creation date + app id, ROM No-Intro/Redump CRCs).
- Stream-metadata thread covers x264/x265 SEI, DV RPU, FLAC MD5. W2C final: non-video categories only: ebooks (EPUB uid, PDF /ID, MOBI EXTH), comics (CBZ/CBR ComicInfo.xml, zip extra fields), software/ISO (ISO9660 PVD dates/app id, UDF), games (Redump/No-Intro CRC/SHA1 DATs, NSP/XCI title IDs), music (EAC/XLD log checksums, AccurateRip/CTDB, MusicBrainz DiscID from CUE).
- Archive-metadata thread covers RAR/7z/zip/PAR2 field tables and exact packer creator strings/defaults (their wave 2). So drop my W2B creator-string plan. FINAL W2: A = stored -m0 RAR / 7z -mx0 offset maps: any article of volume N -> inner-file byte offset, CRC-of-interval matching between posts packed with different volume sizes (alt). B = externally published hashes matchable to the inner CRC32/ed2k: anime [CRC32] filename tags, AnimeTosho, AniDB ed2k+size API, NFO/SFV CRCs, predb/xREL CRC fields. C = non-video category keys (ebooks, comics, ISO/software, games DATs, music logs/DiscID).

## Wave 2 (07:17Z) launched: A stored-RAR offset maps; B external hash oracles; C non-video category keys
Wave 2 results: w2a.md, w2b.md, w2c.md. Takeaway: in store mode the alt key needs no data at all (header FileCRC + size); interval-CRC matching is a fallback for CRC-less or partial cases. External oracles are weak (AniDB needs ed2k; NSRL/No-Intro/Redump offline CRC/MD5 DATs usable). Ogg serial is a random per-stream key.
## Wave 3 plan (07:45Z): A encrypted posts (-hp/-mhe): sizes/padding/salts/KDF/volume math; B cross-seed/autotorrent size-multiset matching applied to inner file sets; C verify per-creation IDs (EPUB/calibre uuid, PDF /ID, PE RSDS, Ogg serial on remux)
- CORRECTION 07:25Z: RAR FileCRC is whole-file only in the last volume; Opus payload wave 3 does -hp size math + password check values, so my W3A pivots.
- Revised W3A: repair/fill posts (extra PAR2 recovery volumes, reposted missing RAR volumes, 'fills' in request groups) linked to the original via PAR2 Recovery Set ID, RAR volume headers, and size math. Matches the owner's rule that late articles join the older release.
- Haiku15 payload wave 4 covers late fills + re-PAR'd posts + CRC interval across article sizes. Re-revised W3A: provenance of re-packs by different uploaders: what survives re-RAR (inner mtime, attributes, path prefixes -ep1, file order, name case, NFO/SFV/sample inclusion, comment), to tell 'same source copy' from independent grabs.
- 07:27Z the owner shared the Linking Signals Map (https://claude.ai/artifact/BEGjW1bU8S6Yqm3cnPMNpE, copy in reference-linking-signals-map.md) as the format example: tag every signal with Proves (twin/alt/same film/same uploader/tool) + read cost (free/overview, 1 article, few articles, every article) + Atlas status. Agents from wave 3 on must tag findings that way; final report fills the map's gaps.

## Wave 3 (07:49Z) launched: A re-pack provenance, B cross-seed size matching, C verify per-creation IDs
Wave 3 results: w3a.md, w3b.md, w3c.md. Notes: WebFetch hourly limit hit; cap agents ~20 fetches. Ogg serial is regenerated by ffmpeg on remux (mux-event key only). PE RSDS GUID and PDF /ID[0] are real per-creation keys. Cross-seed: compare per-file size lists (never totals), ignore sidecars, tolerance 0 for exact.
## Wave 4 plan (08:15Z): A mtime pipelines (torrent clients/NZBGet/SAB/rclone set mtime = download time? -> clusters re-packs by uploader); B size/CRC uniqueness statistics and collision math for thresholds; C PAR2 recovery-slice data equality across reposts (GF16 0x1100B, input slice order by File ID, ParPar vs par2cmdline determinism)
- Opus payload wave 3: RAR5 PswCheck verifies harvested passwords (same-uploader via password reuse); -hp inner size from volume sizes (~5 buckets); Anime Tosho dbexport exists. Their wave 4: RAR5 QO record file lists, side files, validation corpora. Avoid those.
- Fetch tip (haiku15 yEnc): proxy.golang.org and registry.npmjs.org serve source when GitHub blocked. Include in agent prompts.

## Wave 4 (08:18Z) launched: A download-client mtime pipelines; B size/CRC uniqueness stats; C PAR2 recovery-data equality / fills
Wave 4 results: w4a.md, w4b.md, w4c.md (+ tools/par2_recovery_compare.py). HIGHLIGHT: exponent-0 PAR2 recovery slice is rename-proof, order-proof and tool-proof (tested).
## Wave 5 plan (08:45Z): A PAR2 defaults per posting tool (slice size, redundancy, first exponent, file split; MultiPar/QuickPar constants + exp start) to know how often exp-0 / IFSC keys are comparable; B anti-linking countermeasures uploaders use (junk files, random padding, per-post passwords, re-PAR, random volume sizes) and how robust keys survive; C IFSC per-file slice-CRC lists as name-independent per-file keys across different sets (season pack vs single episode) + I'll test locally.
- Opus payload wave 4: RAR chunk/full CRC confirmed; packs vs singles by per-file (len,CRC32) byte-weighted containment; SRS match_offset. So W5C swaps to: do backbones/providers ever mutate article bodies (re-wrap, dot-stuffing, CRLF, size limits, re-encoding, binary-to-text gateways), which would break payload byte keys; I test IFSC per-file keys locally instead.
W5: w5a.md, w5c.md, w5-local-test.md done; W5B pending.
## Wave 6 plan (09:15Z): A usenet streaming/WebDAV tools' deobfuscation and file-identification heuristics (nzbdav, AltMount, StreamNZB, usenet-drive, rclone nzb?); B raw split formats without archive headers (HJSplit .001, 7z -v of raw file, split -b, .mkv.001) + sidecar .sfv/.md5/.crc; C uploader/re-packer marker files (RARBG.txt, YTS jpg, 'Downloaded from' txt/url, proof jpgs, group NFO art) as origin/re-packer fingerprints
- Opus payload wave 5 (measured): unencrypted RAR deterministic across runs/versions (mtime changes ~20 B/vol, so middle articles share pcrc32); -hp last vol ≡14 mod 16; 445k Redump/No-Intro: (size,crc32) 0 collisions, size alone 80-97% unique >=100 MB. Fills my W4B gap.

## Wave 6 (09:17Z) launched: A streaming-tool deobfuscation; B raw split files; C re-packer marker files
Wave 6 results: w6a/b/c. Agents must be told explicitly: never call mcp__hearthbot__ or coordinator tools.
## Wave 7 plan (09:45Z): A category packaging conventions outside video (music/mp3/flac scene rules, ebooks, comics, software/games, XXX?) as set-level fingerprints; B sampled-hash schemes (AltMount, imohash, SubDB, OpenSubtitles, Stremio/Plex/Jellyfin/Kodi, czkawka prehash) mapped to article reads -> cheapest twin key for unarchived posts; C server-side body-hash prior art (Cleanfeed EMP body MD5, INN filters, NoCeM, provider storage dedupe, duplicate-body rejection)
- haiku15 yEnc: X-Received-Body-CRC = crc32(body CRLF + '.\r\n'), matched 12 Highwinds/Farm articles -> HEAD-only twin key. W7C should skip it and focus on Cleanfeed EMP/INN/NoCeM/provider dedupe.

## Wave 7 (09:46Z) launched: A non-video packaging rules; B sampled-hash schemes; C server-side body-hash prior art
Wave 7 results: w7a/b/c. (W7B needed a nudge; it and W6A both touched hearthbot tools despite instructions.)
## Wave 8 plan (10:15Z): A constant marker-file hashes via VirusTotal/hybrid-analysis/forums (RARBG_DO_NOT_MIRROR.exe, YIFYStatus.com.txt, www.YTS.MX.jpg, TGx txt): sizes+hashes; B MultiPar/par2j/QuickPar exp-0 compatibility + slice defaults + ngPost PAR2_ARGS default (unverified so far); C Rust crate map for each recommended key (PAR2 parse, RAR/7z headers, crc32 combine, md5/sha256, mmh3, oshash)

## Wave 8 (10:18Z) launched: A marker-file sizes/hashes; B PAR2 tool defaults + exp-0 compatibility; C Rust crate map (agents write w8a/b/c.md themselves)
Wave 8 results: w8a (weak: only RARBG.com.txt hashed), w8b (par2cmdline+ParPar exp-0 verified; MultiPar/QuickPar open), w8c (crate map: crc32fast/RustCrypto/xxhash use; rest write own).
## Wave 9 plan (10:45Z): A PAR2 source-level verification (par2cmdline par2creator.cpp block-size rule, par2j source on GitHub Yutaka-Sawada/MultiPar, QuickPar docs via archive.org, par2cmdline-turbo identity); B srrDB as scene oracle: API fields (inner file size+CRC, RAR volume sizes, SFV), rate limits, dumps, to name obfuscated posts; C public test corpora for each key (par2cmdline/ParPar/libarchive/unrar test vectors, sanderjo/NZBs) to build Atlas unit tests
- Opus payload wave 8: encrypted reposts give password-independent volume sizes ((V,n,S_last) ~27.5 bits blocking key); disc posts: AACS Unit_Key_RO.inf SHA-1 / UDF Volume Set ID per-master keys; their W9 = size clustering, Rust CRC32 GF(2) toolkit, packer version tells. No overlap with my W9.

## Wave 9 (10:48Z) launched: A PAR2 source-level verification; B srrDB scene oracle; C public test corpora
Wave 9 results: w9a (par2cmdline slice rule from source; MultiPar still open), w9b (srrDB archive-crc search), w9c (rarfile test/files best corpus).
## Wave 10 plan (11:15Z, last): A MultiPar/par2j + QuickPar defaults via release source zips/SourceForge/archive.org; B srrDB client code (pyrescene, srrdb python/go/rust wrappers, autotorrent/cross-seed srrdb use) to confirm JSON shapes + archive-crc/archive-size syntax + usage etiquette; C does the original (pre-obfuscation) name ever leak in an obfuscated post's payload (PAR2 UniFileN vs FileDesc, RAR file comments/archive comment, NFO inside, SFV, 7z names, mkv title/segment filename tags)? Then final report.
- Opus payload wave 9: pure-Rust CRC32 GF(2) toolkit built (combine, yEnc part fold, PAR2 slice fold; opus-tools/w9b_crc32gf2/); encrypted size key collapses for fixed-cap disc images; tool tells in opus-tools/w9c_tool_tells.md. Cite in final report.

## Wave 10 (11:25Z) launched: A MultiPar/QuickPar defaults; B srrDB client code; C original-name leaks
Wave 10 results: w10a/b/c. FINAL REPORT: final-report.md + https://claude.ai/artifact/4H6dyXh6RETqWKAK8v7phC (11:40Z). Thread done.
