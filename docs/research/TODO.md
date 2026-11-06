# Research TODO

Findings from the 2026-11-02 research (see [README.md](README.md)) that Atlas has not acted on yet. Status was rechecked against `main` at `afef0e9` on 2026-11-05 (by reading the code, not live data). Ticked items name where they landed; a note on an open item says what part is already there. When you build one, tick it here in the same PR, with the PR number.

Rules the research sets for all of these: pure Rust, no ffmpeg; write our own parser unless a crate is widely used; no single key merges releases on its own (a twin needs size + CRC32 + a 16 KB or full MD5); twins stay separate rows; never display privacy fields.

## 1. Probe fixes (no extra download)

Source: [container-metadata/FINAL-REPORT.md](container-metadata/FINAL-REPORT.md) "Atlas probe today", [stream-metadata/FINAL-REPORT.md](stream-metadata/FINAL-REPORT.md) "What Atlas does today". Mostly built since (#48 and the MKV thread's probe work).

- [x] Chapters: pick the default, non-hidden edition (`EditionFlagHidden`, `EditionFlagDefault`). Done in `src/probe/media.rs` ("Chapter extraction with hidden edition filtering").
- [x] HDR10 shows as "PQ" (fixed: PQ + BT.2020 + 10-bit, or mastering data, now reads HDR10; `hdr_summary` in `src/web.rs`): label PQ + BT.2020 + 10-bit as HDR10 and confirm from SEI 137/144 in hvcC or the first IDR.
- [x] (hvcC and the SPS VUI; av1C and vpcC still not read) `bit_depth` is null on most 10-bit HEVC: read hvcC bytes 17/18, the avcC High10 extension, av1C byte 2, vpcC byte 6 instead of only `Colour.BitsPerChannel`.
- [x] (VUI fallback in `src/probe/media.rs`) HDR, primaries and transfer are null on mkvmerge raw-HEVC and DV files: parse the SPS VUI from the NAL arrays in CodecPrivate.
- [x] (#150) Non-faststart MP4 gives no tracks: fetch `moov` at mdat offset + mdat size (1-2 targeted articles).
- [x] (RPU presence, profile, FEL/MEL and CM version are read; L5/L6/L9/L11 values are not kept yet) Dolby Vision from the first RPU (FEL/MEL, CM 2.9/4.0, L5/L6/L9/L11), and DV in ffmpeg-made files, which carry no dvcC. Crate `dolby_vision` 3.4.0 rejects RPUs with a Level 253 block: pin git or fall back to dvcC.
- [ ] HDR10+ (today only the release name's "HDR10+" tag is used): check both the HEVC SEI (T.35 0xB5/0x003C) and the MKV BlockAdditionMapping type 4.
- [x] (TrueHD, E-AC-3 JOC via dec3 and EMDF, DTS XLL in `src/probe/media.rs`) Atmos and DTS:X from audio frames (TrueHD FBA sync + substream_info bit 7, verified on 52 files; E-AC-3 JOC; DTS XLL sync) instead of the track name.
- [x] x264/x265 settings string (SEI 5) as encoder fact (`encoder_options`, shown on the release page). Not yet used as a same-encode key.
- Integration plan: [stream-metadata/raw/w10-a-atlas-integration.md](stream-metadata/raw/w10-a-atlas-integration.md) (serde(default) fields, bump `ProbeStep::config_stamp` to 2). Prototype: [stream-metadata/code/streamprobe/](stream-metadata/code/streamprobe/). Keep Atlas's own EBML walker: `matroska-demuxer` fails on partial files.

## 2. Container facts not read yet

Source: [container-metadata/FINAL-REPORT.md](container-metadata/FINAL-REPORT.md), catalogs in [container-metadata/catalog/](container-metadata/catalog/).

- [ ] Store mux identity: SegmentUUID, DateUTC, MuxingApp/WritingApp, Duration float size (twin key and tool fingerprint). Already there: SegmentUID and MuxingApp are read and shown, DateUTC is read. Not yet: the Duration float size, and using any of them as a key.
- [x] Matroska Tags shown on the release page (#165): the Tags in front of the first Cluster, sorted, IMDB/TMDB linked, ENCODED_BY, purchase and e-mail tags never shown.
- [x] (#NNN) mkvmerge statistics tags at the file end (+1 tail article): the front's SeekHead says where the Tags are; past the first Cluster, the part holding them is read (the last part, already read, when they're in it; one more when they run into it), and each track keeps `stats` (BPS, NUMBER_OF_FRAMES, NUMBER_OF_BYTES, DURATION), shown on its line on the release page. The tail's other tags join `tags`, those that can name a person left out. Bare files only (not in an archive), and only a SeekHead that names the Tags itself (a second SeekHead at the end isn't followed). Not a key yet: same frames and bytes per track is a same-mux witness for a reader.
- [ ] Matroska Tags, the rest: IMDB/TMDB/TVDB IDs used to identify the title (another PR).
- [ ] Subtitle and attachment keys: ASS header hash (strip the Aegisub section), font-set hash, VobSub palette.
- [ ] ChapterSkipType (intro, credits) and a chapter timing signature.
- [ ] FLAC STREAMINFO MD5 as an exact audio alt key.
- [ ] MP4 `colr`/`mdcv`/`clli`, `chpl` chapters, `ilst` title and `cnID`; `stts`/`stsz` for exact frame count and bitrate. Already there: the sample tables are walked for stills (#160). Not yet: the rest.
- [x] Disc posts: ISO 9660 and UDF walk, DVD IFOs (#151), Blu-ray `index.bdmv`/MPLS/CLPI main playlist (#153).
- [x] DVD disc ID (#165): libdvdread's DVDDiscID (MD5 of VIDEO_TS.IFO and VTS_01..N_0.IFO) in `src/probe/dvd.rs`, stored as the probe's `disc_id` and shown on the page; an ISO's sets found by its directory records, a stored VIDEO_TS folder's by its entry names. Releases probed before need a reprobe to get one.
- [x] (#NNN) Blu-ray `id.bdmv` disc ID: `bluray::disc_id` finds `BDID` and its version in what was read and keeps the 4-byte org ID at byte 40 and the 16-byte disc ID at 44, in hex (40 digits), as the probe's `disc_id`, shown on the page as "Blu-ray disc ID". All-zero IDs are dropped. Only when the file is in the front read; not a merging key (per pressing or per title is still open, section 8).
- [ ] TS broadcast captures: SDT channel, EIT programme, TDT capture time. (#157 reads TS tracks; these tables are not read.)
- [x] Legacy writers: AVI ISFT and Xvid/DivX strings (#145), ASF/WMV (#148), MPEG-PS/VOB (#149).
- [ ] Ogg vendor string and FLV onMetaData.
- [ ] Fetch planner: parsers return `Need { inner_offset, len }`, a planner maps it through store-mode RAR/7z to an article, budget about 12 media articles per release.
- [ ] 29 MP4 catalog entries were reset to unverified in the last pass; recheck them.

## 3. Archive facts dropped today

Source: [archive-metadata/FINAL-REPORT.md](archive-metadata/FINAL-REPORT.md), [archive-metadata/raw/w10c-atlas-bugs.md](archive-metadata/raw/w10c-atlas-bugs.md) (checked at `ad111a0`; recheck lines). Nested and compressed decoding already shipped in #52 #56 #58 #61 #62 #63.

- [ ] (still open: `archive_inner_files` has only name, size, source) Keep per-file CRC32, unpacked size, mtime and host OS in `archive_inner_files` (RAR5 skips it at rar.rs, RAR4 never reads it, zip CD offset 16 unread, 7z substream digests discarded). Inner CRC32 + unpacked size is the exact alt key (same file, different container); the timestamps help tell a twin from a re-RAR.
- [x] Persist PAR2 FileDesc MD5, 16 KB MD5 per file (`par2_files`, every set a step reads, with the IFSC CRC32 fold; the Creator string is still not kept).
- [ ] Packer fingerprint from article 1: RAR host OS, method, dictionary, HTIME, owner record; zip version-made-by and extras; 7z version bytes and coders.
- [ ] Per-archive random values as twin keys: RAR5 `-hp` salt and check value (byte-identical across volumes with rars; check WinRAR), 7z AES IV, 7z start header NextHeaderCRC.
- [ ] RAR5 `.rev` first file: size and CRC32 of every data volume. SRR/SRS parse for scene posts.
- [ ] Bugs: RAR4 Unicode names ASCII-only in the probe; stale encryption columns on upsert; zip encrypted flag ignores 0x0040/0x2000; zip non-UTF-8 names decoded as Latin-1 not CP437; SFV probe takes only the first three small text files; 7z header without names counts as complete with zero files; view cap 256 rows vs 4096 stored, with PAR2 rows sorting first; `probes_per_minute` cast to 0 can divide by zero.
- [ ] Bug: the 7z encoded (compressed) header is decoded with `lzma-rs` as plain LZMA only (`src/archive/sevenz.rs` `decode`), so a header packed with LZMA2, or with BCJ/Delta in front, reads as no file list. Atlas already has its own decoders under `src/probe/codec/`; reuse them here, or use `lzma-rust2` (`Lzma2Reader`, `filter::bcj`, `filter::delta`) if it meets the crate rule.
- [ ] (#161 harvests, verifies and uses passwords; these facts are still not stored as keys) Encryption facts per format, stored and never shown: RAR5 `-hp` HEAD_CRYPT (KDF count, salt, check value; PswCheck is PBKDF2-HMAC-SHA256 with 2^N+32 rounds folded to 8 bytes), RAR5 file crypt extra flag 0x02 = HASHMAC, RAR4 `-hp` 8-byte salt before each block (no check value), 7z AES props (default 0x53 0x0F, no salt by default since 7-Zip's `ResetSalt` is commented out, so the 16-byte IV is the per-archive key).
- [ ] Uploader leak fields, stored for linking only and never displayed: RAR5 Unix owner extra 0x06 (user, group, uid, gid), zip DOS local time vs the 0x5455/0x000a UTC extra (gives the uploader's UTC offset), RAR STM `Zone.Identifier` stream (HostUrl), sub-second time precision (reveals the packing OS).
- [ ] Posting-tool defaults as fingerprints: ngPost runs `rar a -idp -ep1 -m0 [-v<N>m -hp<pass>]` and `7z -mx0 -mhe=on`; ParPar writes Creator `ParPar v<ver> <arch> [url]`, par2cmdline writes `Created by par2cmdline version X`. RAR5 quick open (QO) is per volume; 7-Zip does not sort solid archives by type by default (`-mqs` off).
- [ ] RAR 2.0 (`rar20`) partial decode: check that a truncated member still yields its decoded prefix (the `rars` crate's rar20 returned nothing on truncated input in the research). Check the inner output for EBML magic `1A45DFA3` before parsing it as MKV.
- [ ] Run the prototypes in [archive-metadata/proto/](archive-metadata/proto/) against real Usenet samples (only synthetic and public fixtures were used). Prototype bugs to fix first: `rar_meta.rs` lines 373, 483-496, 549, 552, 660, 862, 883; `sevenz_zip_meta.rs` maps zip host 11 to VFAT (it is MVS) and lacks 14 (VFAT).
- [ ] 13 RAR and some 7z catalog entries are still unverified; prototype extractors in [archive-metadata/proto/](archive-metadata/proto/) list known bugs in `raw/w10a-rar-check.md`.

## 4. Payload fingerprints (twins and alts)

Source: [payload-fingerprints/final-report.md](payload-fingerprints/final-report.md). Prototypes: [opus-tools/w9b_crc32gf2/](payload-fingerprints/opus-tools/w9b_crc32gf2/) (CRC32 GF(2) toolkit, std only), [opus-tools/w10b_walker/](payload-fingerprints/opus-tools/w10b_walker/) (PAR2 + RAR4/5 partial-buffer walker, 332/332 vs rarfile).

- [ ] Store yEnc `pcrc32` per part and fold it into a whole-file (size, CRC32) with the GF(2) toolkit. Stored: every article a step reads whole leaves `(decoded length, CRC32)` in `segment_crc`. Not yet: the fold to a whole-file CRC, since only a few articles of a file are ever read.
- [x] PAR2 index per set: full MD5 + length, 16 KB MD5 + length (`par2_files`, indexed on `(size, md5_16k)` and on `set_id`; it finds file twins across shards). The IFSC fold is kept per file; binding it to a pcrc32 fold waits on the item above.
- [ ] Last RAR volume's first article: inner file (name, size, CRC32) as the alt key.
- [ ] Encrypted reposts: `(V, n, S_last)` size tuple from the exact `-hp` size model, as a blocking key only.
- [ ] NZB escape-count segment-size index (Nyuu-style NZBs only).
- [ ] Encode the twin vs mtime-re-RAR rule (needs header-bearing articles) and test it against a labelled set.

## 5. Linking split and obfuscated posts

Sources: [posting-behavior/FINAL-REPORT.md](posting-behavior/FINAL-REPORT.md) (v1 pipeline spec [wave-09-A-pipeline-spec.md](posting-behavior/wave-09-A-pipeline-spec.md) with fixes in [wave-10-A-redteam.md](posting-behavior/wave-10-A-redteam.md)), [yenc-header-forensics/opus/FINAL-yenc-header-forensics.md](yenc-header-forensics/opus/FINAL-yenc-header-forensics.md) (rollout P1-P6 in [w9b-atlas-rust-spec.md](yenc-header-forensics/opus/w9b-atlas-rust-spec.md)). Open PR #64 links the files of fully obfuscated uploads; #67 keeps per-file timing.

- [x] P1: posting-tool classifier and per-file fields (`file_timing.tool`, `clock_first`/`clock_last` in ms from Nyuu or the server's id, Highwinds `hw_host`, `hw_key`, `hw_n_first`/`hw_n_last`), folded at ingest by `discovery::id_facts` and shown on the release page. Not yet: using the (host, K) key to join files across rows.
- [ ] P2: Nyuu Message-ID stream linker (Node 12+ xorshift128+, solver in [nyuu-id-solver/](yenc-header-forensics/opus/nyuu-id-solver/)); links every article of one upload from two IDs.
- [ ] P3-P6: ms-continuity placement, NewsUP drand48, size-vector twin key with entropy gate, X-Received-Body-CRC twin key, offline seed searches.
- [x] End-to-start file join by the right clock, then PAR2 file-size match and 16 KB MD5 tie-break: the link step (`archive/link.rs`) now takes candidates from every group of its shard, orders them by `merging::ReleaseAssembler`'s end-to-start window over `file_timing`'s clocks, and links on size + first-16-KiB MD5 against the stored set. Not yet: candidates in other shards (the stage's writes are per shard), and the veto DSU over more than one set at a time.
- [ ] Validate on Atlas's own XOVER/HEAD data (everything so far was measured on public NZBs). Needs the owner's go.
- [x] Run the read-only provider probe [posting-behavior/nntp-probe/](posting-behavior/nntp-probe/) once per provider account (completed 2026-11-03 in [posting-behavior/nntp-probe/results-2026-11-03.md](posting-behavior/nntp-probe/results-2026-11-03.md), PR #74): `NEWNEWS` is disabled everywhere (`400`/`500`/`502`), non-overview `XHDR`/`HDR` returns `221` with no values or `400`/`501`/`503`, `Lines` is empty/`"0"` on up to 64% of rows, 2-digit `Date` years appear on 13–68% of rows, Giganews (`super.newsgroupdirect.com`) runs **+5.74 s** ahead on `DATE`, and `XZVER` works on XSNews/Usenet.Farm while hanging on Highwinds (`XFEATURE COMPRESS GZIP` 290). Follow-up: re-run on a busy obfuscated binaries group to sample server-assigned Message-IDs.

## 6. Frames and fingerprints

Source: [stream-metadata/FINAL-REPORT.md](stream-metadata/FINAL-REPORT.md) "Frames and fingerprints". Stills shipped in #50 #51 #54 #60 and #160 (MP4).

- [ ] Remux-proof alt key: hash of codec config + first keyframe (+ first 8 video and 16 audio payloads); script [container-metadata/tools/altkey.py](container-metadata/tools/altkey.py) matched 9 remux variants and rejected a re-encode.
- [x] (stills keep a `phash` per frame in `release_stills` and look up close matches; check the recipe below against `src/probe/stills.rs`) Keyframe pHash at 25/50/75% (crop letterbox, equalise, match at 10 of 64 bits, 2 of 3 frames).
- [ ] AC-3 crc2 run as a same-track key; audio fingerprint (chromaprint) only for releases nothing else linked.

## 7. Release page

Source: [metadata-visibility/MAP.md](metadata-visibility/MAP.md) section 4 (visibility matrix). #65 and #67 built most of it.

- [ ] Recheck the matrix against `main` and list which catalog fields are still `not_collected` or `stored_not_shown`.

## 8. Open questions to settle with real data

- Is the Blu-ray `id.bdmv` disc_id per pressing or per title?
- What share of release MP4s are faststart? (`release_probe` can answer this.)
- Does mkvmerge write the T.35 HDR10+ block-addition mapping?
- Is Highwinds S/K per upload or per connection, and is K ever reused?
- ~~Does any provider allow `XHDR X-Received-Body-CRC`?~~ **Settled (2026-11-03 probe):** No — every probed provider returns `221` with zero values or `400`/`501`/`503` for non-overview headers on `XHDR`/`HDR`; reading `X-Received-Body-CRC` requires `HEAD` per article.
- Does WinRAR repeat the same `-hp` salt in every volume?
- What default dictionary size does each RAR and 7-Zip version write? (Needed for the packer-version fingerprint.)
- What Creator strings do MultiPar and QuickPar write in PAR2?
- What is the RAR4 recovery-record percent formula, and what are the RAR5 ACL and QO flag layouts?
- Real samples of E-AC-3 JOC, E-AC-3 7.1, DTS-HD MA and DTS:X were not reachable; run streamprobe on a few real releases on the Mac.
