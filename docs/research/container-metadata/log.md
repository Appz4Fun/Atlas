# Container metadata extraction research log

Started 2026-11-02 06:50Z. 10 waves x 3 haiku (high effort) agents, ~30 min/wave. Goal: YAML field catalog of every metadata field in MKV/WebM, MP4/MOV, M2TS/TS, ISO/UDF/BDMV, DVD IFO, AVI, plus video bitstream metadata (DV RPU/levels, HDR10/HDR10+, SEI encoder strings) and audio stream metadata. Rust-native only (no ffmpeg/mediainfo). Note what is readable from a partial download (front articles of a RAR m0 set).

## Wave 1 (06:51Z)
- A: Matroska/WebM full element catalog (EBML header, Info, Tracks, Tags, Chapters, Attachments, Cues) + Rust crates
- B: MP4/MOV box catalog (moov/mvhd/trak/stsd, udta/ilst, edts, chpl/chapter tracks, sgpd, colr/mdcv/clli/dvcC/dvvC) + Rust crates
- C: ISO/UDF + BDMV (index.bdmv, MovieObject, mpls, clpi, META/DL) + DVD IFO + M2TS/TS PSI + Rust crates

## Sharing
Siblings: archives thread cmsg_014FRqgd95UUF8Mvov1QRBWr4Gq2fs5zanzyA8zHJofjv4 ("Extract metadata from 7z, RAR and archives"), streams thread cmsg_014FRqgd95UUF8Mvov1QRBWrCNGakS7BhDkgf7KeSYm3L3 ("Extract video, HDR and audio stream metadata": owns DV RPU/HDR10+/SEI/audio bitstream depth). This thread owns container-level fields; for DV/HDR stick to the container boxes/elements (dvcC, BlockAdditionMapping, colr/mdcv/clli, STN) and hand bitstream work to the streams thread.
Plus the 10 linking research threads listed in the coordinator note (see payload-fingerprints/log.md).

### Wave 1 merged (07:00Z)
- MKV front (first 1-4 MB, mkvmerge layout): EBML DocType, SeekHead, Info (Title, MuxingApp, WritingApp, Duration, SegmentUID), all Tracks (codec, CodecPrivate, lang, names, flags, DefaultDuration=fps), Colour/MasteringMetadata/MaxCLL (HDR10 class), BlockAdditionMapping dvcC (DV profile/level/compat id), Chapters, Attachments (fonts, cover). Back of file: Tags incl. mkvmerge statistics (BPS, NUMBER_OF_FRAMES, NUMBER_OF_BYTES, DURATION) and Cues -> tail read needed for per-track bitrate/frames.
- MP4: everything lives in moov. Faststart puts it in front; otherwise first articles show only ftyp + mdat header -> need tail fetch. From moov: dvcC/dvvC (profile 7b, level 6b, rpu/el/bl flags, compat id 4b), colr/mdcv/clli, btrt, sample tables -> exact frame count/fps/bitrate/GOP; hdlr name + ©too + compressorname = muxer fingerprint; ilst iTunes tags, chpl/chapter tracks.
- DV RPU levels and HDR10+ are bitstream (NAL 62 / T.35 SEI) -> streams thread owns.
- BD: id.bdmv "BDID" org_id@40, disc_id@44 (16 B) = candidate same-disc key; MPLS duration from IN/OUT (45 kHz), STN streams w/ language, UHD dynamic_range; PlayListMark type 1 = chapters. DVD IFO offsets verified (oxideav-dvd). TS: SDT provider/service name + TDT/TOT time = broadcast capture source+date.
- Crates: no widespread crate covers partial input well. Plan: hand-written EBML walker + ISOBMFF box reader + MPLS/CLPI/IFO/id.bdmv/UDF/SI parsers; symphonia-format-mkv / mp4parse optional for full files.
- Gaps: unverified IDs for Tags/Chapters/Attachments/Cues, ISO PVD/UDF offsets, implementation-ID strings, UHD STN offsets, DVD disc-ID algorithm, faststart prevalence.

## Wave 2 (07:01Z) — lean in: verify + tool fingerprints + tail strategy
- A: mkvmerge/MakeMKV/HandBrake/Lavf layout from source (where Tags/Cues/Chapters/Void go, statistics tags), MuxingApp/WritingApp string catalog per tool/version, verify element IDs from ebml_matroska.xml
- B: MP4 moov placement by tool (ffmpeg default, HandBrake, MP4Box, L-SMASH, Apple), tail-fetch strategy, hdlr/©too/compressorname catalog, mp4-atom/re_mp4 crates
- C: verify ISO9660/UDF offsets + implementation IDs, libbluray mpls_parse UHD STN, DVD disc ID in libdvdread, ISO layout (where BDMV metadata files sit in an ISO -> what first MBs give)

### Peer inputs (06:55Z)
- Archive thread: RAR5 store data start = block_start + 4 + len(vint hdr size) + hdr size; packed size vint only if flag 0x0002; store = method bits 8-10 == 0. RAR4 data start = block_start + HEAD_SIZE, PACK_SIZE (+HIGH_PACK_SIZE) after HEAD_SIZE, store byte 0x30. Each volume restarts sig+main+file header; inner bytes contiguous. Compressed RAR: no pure-Rust decompressor -> inner reads only on m0 posts. Notes: research/archive-metadata/raw/w1a-rar.md
- Streams thread: takes SPS/VUI/SEI/RPU/audio frame headers. hvcC/av1C in CodecPrivate give profile/level/chroma/depth + SPS without a frame; x264/x265 SEI options string in first IDR; dolby_vision crate 3.4.0 for RPU; DD+ Atmos = EMDF payload 14. research/stream-metadata/log.md

### Wave 2 merged (07:08Z)
- MKV layouts TESTED (mkvmerge v82.0, ffmpeg 6.1.1): mkvmerge = SeekHead, Void, Info, Tracks, Chapters, [Attachments], Clusters, Cues, Tags(stats) at END, no front reserve. Lavf = SeekHead, Info, Tracks, Chapters, Tags at FRONT (ENCODER, per-track DURATION rewritten in place at trailer), Cues at end unless -cues_to_front. mkvmerge Duration float32, Lavf float64 (muxer tell).
- MuxingApp/WritingApp verified strings: "libebml v1.4.5 + libmatroska v1.7.1" / "mkvmerge v82.0 ('I'm The President') 64-bit"; Lavf "Lavf60.16.100"; "no_variable_data" = mkvmerge --engage test mode (byte-identical output).
- DateUTC + _STATISTICS_WRITING_DATE_UTC change per mux -> remux discriminator; same values = same mux run (alt key with SegmentUID).
- Element IDs confirmed from ebml_matroska.xml (table in raw/w2a.md).
- MP4 TESTED: ffmpeg default moov at END (ftyp, free, mdat, moov); +faststart moves it front; MP4Box default front. Fragmented: moov(mvex, empty stbl) front, moof/mdat, mfra end -> durations need moof/trun walk. mvhd creation_time 0 by default. Tail fetch: moov_off = mdat_off + size (size 1 -> u64, 0 -> EOF/no moov) -> RAR volume -> yEnc part via =ypart begin/end.
- Crates: mp4-atom 0.16.1 (2026-09, 431k), re_mp4 0.5.1 (2.1M), mp4 0.14 (stale, no dvcC/colr). Coverage unverified -> own box reader.
- ISO: PVD @0x8000 offsets verified (libcdio); UDF AVDP @0x80000, PVD/LVD offsets verified (Linux ecma_167.h). MPLS STN counts verified (libfreemkv). BD audio/video codes listed. videolan code host 403 -> libbluray/libdvdread originals NOT read; dynamic_range nibble, CLPI, id.bdmv semantics, DVDDiscID still memory.
- Archive thread 06:58Z: rars 0.12.3 (Apache-2.0, young) is the only pure-Rust compressed-RAR decoder; sevenz-rust2 0.24/lzma-rust2 for 7z. Prefix-decoding first articles of non-solid compressed RAR/7z might give the MKV front (untested). 7z Copy mode: file 1 data at offset 32 of .001.

## Wave 3 (07:10Z)
- A: subtitles + attachments (ASS CodecPrivate headers, VobSub idx, PGS, flags/names, fonts/cover, event counts)
- B: re-verify libbluray/libdvdread from mirrors (mpls/clpi/bdid/index/meta, DVDDiscID)
- C: AVI/OpenDML, TS/M2TS SI deep (DVB/ATSC, DOVI descriptor), Ogg/OGM, FLV, ASF/WMV, VOB
- Archive thread 07:00Z, tail mapping for m0 RAR: per_vol_payload = vol_size - header bytes (same each volume except vol1 w/ comment and last w/ ENDARC/RR); inner X -> vol floor(X/per_vol_payload), offset hdr + X mod per_vol_payload -> article = offset / yEnc part size; confirm via last volume's own header. 7z Copy: inner = 32 + PackPos + X over concatenated .001/.002.

### Atlas today (main a29ee52, src/probe/media.rs 748 lines)
Already parses from front bytes: container, muxing_app, writing_app, title, duration, chapters (start+name), video (codec, w/h, display w/h, fps, bit_depth, hdr name from transfer+mastering, primaries, transfer, dv_profile/level/EL flag, language), audio (codec, channels, sample_rate, lang, name, atmos, commentary), subs (codec, lang, name, forced, HI, commentary), header_complete. MP4 only if whole moov is in the buffer (has_whole_moov). NOT parsed: SegmentUID/DateUTC, TrackUIDs, CodecPrivate hash, matrix/range/MaxCLL/MaxFALL/mastering values, crop/stereo, Tags (any), Attachments, Cues, edition flags/ordered chapters, MP4 ilst/hdlr/btrt/sample tables/tail fetch, all disc formats (ISO/UDF/BDMV/IFO), TS, AVI. Final catalog will tag each field atlas: parsed | no.
- Streams thread 07:01Z: dvcC (ffmpeg dovi_isom.c): u8 major, u8 minor, u16 [profile7|level6|rpu|el|bl], byte4 [compat4|md_compression2]. x264 SEI UUID dc45e9bd-..., x265 2ca2de09-... first AU only. AV1 HDR10+/DV in T.35 metadata OBUs. Proxy: static.crates.io + raw.githubusercontent OK; docs.rs, matroska.org, gitlab 403.

### Wave 3 merged (07:12Z)
- Subs (tested): MKV S_TEXT/ASS CodecPrivate = full [Script Info]+[V4+ Styles] (~4 KB, in Tracks -> first article). Hash minus [Aegisub Project Garbage] = same-subs key; that garbage section leaks the encoder's local video path/name ("Video File: C:\Users\...\Show.S01E01.1080p.mkv"). mkvmerge Attachments (fonts, cover) BEFORE first Cluster -> font-set hash / cover hash from front. MIME can be wrong: sniff magic. Sub NUMBER_OF_FRAMES = event count (tail). MP4 tx3g: hdlr 'sbtl', ftab font, udta/name title; stsz counts filler samples (not events).
- BD/DVD VERIFIED-ORIG (libbluray 1.5.0 + libdvdread 7.1.1 tarballs from archive.ubuntu.com): full MPLS layout; STN HEVC attr byte+2 = dynamic_range_type<<4|color_space, byte+3 bit7 cr_flag, bit6 hdr_plus_flag; CLPI attr differs (aspect/oc_flag first). MPLS ext (3,5) static HDR metadata (primaries, white point, max/min lum, MaxCLL, MaxFALL). index.bdmv version 0300 = UHD; initial_dynamic_range_type at byte 44; ext 3.1 has disc-level 4K/HDR/HDR10+/DV flags. id.bdmv org_id@40, disc_id@44 (16 B, semantics undefined; libbluray's 20-byte "Disc ID" is AACS-derived, separate). DVDDiscID = MD5 over VIDEO_TS.IFO + VTS_nn_0.IFO (up to 10). Value enums for dynamic_range_type not in source.
- Other containers: TS DOVI descriptor 0xB0 verified (ffmpeg mpegts.c) = same bit layout as dvcC; registration 0x05 values (HDMV, CUEI=SCTE-35, AC-3, EAC3, DTS1-3, HEVC...). DVB EIT p/f event name + start time = same-broadcast key, TDT/TOT capture time, usually within first 700 KB. ASF File Properties File ID GUID = per-encode twin key; WM/Encoder/EncodingSettings. AVI ISFT/ICRD/JUNK + XviD/DivX user-data in first 00dc chunk. Ogg vendor string + stream serial (per-mux); duration needs last page.
- Proxy tip: archive.ubuntu.com orig tarballs work for C sources blocked elsewhere.

## Wave 4 (07:13Z) — pivot to real-world samples + tag vocabularies
- A: download public real-world MKV/MP4 samples (DV/HDR10/Atmos/anime with fonts), dump front layouts, MuxingApp, what fits in first 700 KB / 2.8 MB
- B: tag vocabularies: Matroska official tag names, MakeMKV/tinyMediaManager/Plex/mkvpropedit tags, iTunes atoms incl. cnID/apID/purd/sfID (store IDs + purchaser leaks)
- C: real Blu-ray/DVD ISO layout + main-playlist selection (BDInfo logic, obfuscation playlists), BDMV folder order inside RARs, MakeMKV backup structure
- Archive thread 07:05Z (memory): each RAR volume has headers only for files with data in it; add order = dir enumeration (Windows ~case-insensitive alpha, recursing; Linux readdir unsorted; solid sorts by extension unless -ds). BDMV: BACKUP, CLIPINF, index.bdmv, META, MovieObject.bdmv, PLAYLIST, STREAM -> vol 1 usually holds index/MovieObject/MPLS/CLPI. RAR5 quick-open record near the tail caches headers -> fuller listing from last article. They will verify.

### Wave 4 merged (07:20Z)
- Real samples: only GitHub raw reachable (matroska-test-files, pytorch assets); DV/HDR10/anime/MakeMKV/Atmos samples unreachable through proxy. Old mkvmerge v4 (test5, 11 tracks) put Tags at tail and 1.5 MB first cluster; libebml2-era files put Tags at front. ffmpeg MP4 without faststart: moov after mdat. test4 = unknown-size (live) segment -> walker must handle unknown sizes. Walker: tools/walk.py.
- Tags: IETF cellar tags draft-16 verified: IMDB, TMDB (movie/|tv/ prefix), TVDB, TVDB2, ISRC = same-film keys; ENCODER, ENCODER_SETTINGS, ENCODED_BY, DATE_ENCODED file-side. iTunes cnID = store catalog id (same-film), sfID storefront country; apID (buyer Apple ID email), purd, ownr, PURCHASE_OWNER = PRIVACY: store hashed or not at all, never display. stik/rtng/hdvd enums verified. iTunEXTC/iTunMOVI/xid/XMP DocumentID memory only.
- Discs: libudfread verified VRS scan LBA 16-255, AVDP @256; metadata partition "*UDF Metadata Partition"; FSD at metadata LBA 0 -> full file tree in a few sectors after VDS. ImgBurn BD sample: index.bdmv at sector 4320 (~8.4 MB) -> 1 MB prefix NOT enough, plan 1-2 extra articles. BDInfo: sort by length, drop < 20 s and looping playlists, group shared clips; main title = longest non-looping in top clip group + chapter count (to test). RAR volume 1 lists only its own files.

## Wave 5 (07:21Z) — test against Atlas's real parser + deepen MKV/MP4
- A: synthetic corpus (ffmpeg libx265 HDR10, mkvmerge colour/fonts/ASS/editions, eac3/truehd/dts, MP4 faststart/not) -> run Atlas src/probe/media.rs parse() on 700 KB / 4 MB prefixes, list gaps
- B: MKV advanced: editions/ordered chapters/segment linking, unknown-size + lacing, tag targets, WebM limits, Matroska v4/v5 elements
- C: MP4 derived stats (exact bitrate/fps/VFR/GOP/B-frames/edit offset), fragmented duration, audio config boxes (dec3 JOC, dac4, ddts, dmlp, dOps, dfLa), iTunEXTC/iTunMOVI/xid via mutagen source
- Archive thread 07:11Z (correction): RAR5 quick-open is per volume (own headers only). RAR add order unverified. 7-Zip does not sort by extension by default. NEW: 7z LZMA2 prefix decode works: bytes from offset 32 into lzma-rust2 Lzma2Reader (64 MiB dict) -> correct MKV prefix (10% of archive -> 786 KB of MKV, starts 1A45DFA3). So compressed 7z posts are readable from front articles too.

### Wave 5 merged (07:30Z)
- ATLAS PROBE TEST (harness tools/probe-harness, 7-file synthetic corpus, 18 runs, no panics):
  - BUG chapters: with 2 editions (1st hidden, 2nd default+ordered) Atlas returns the hidden edition's chapters; edition flags ignored.
  - BUG HDR label: HEVC BT.2020+PQ with mastering only in SEI labelled "PQ" (Lavf/mkvmerge don't write MasteringMetadata from SEI); MaxCLL/MaxFALL/mastering values not read.
  - bit_depth None on 10-bit HEVC (no BitsPerChannel) -> fall back to hvcC in CodecPrivate.
  - Missing: Tags (IMDB/TMDB/stats), attachments, default flag, video track name, MP4 chapters/title, bitrates.
  - MP4 non-faststart: nothing at 700K/4M (needs tail fetch). Faststart: all tracks at 700K.
  - MKV with 2.2 MB fonts: chapters missing at 700K, present at 4M.
  - Correct: tracks, sub names, forced/SDH/commentary, Atmos name check.
- MKV advanced (ebml_matroska.xml verified): ChapterSkipType 0x4588 (v5) enum 0 none,1 opening credits,2 end credits,3 recap,4 next preview,5 preview,6 advert,7 intermission. ChapterSegmentUUID 0x6E67 + ChapterSegmentEditionUID 0x6EBC = linked OP/ED. Unknown-size Segment/Cluster: scan to next sibling. ContentCompAlgo 3 header stripping: prepend ContentCompSettings before probing CodecPrivate/frames. TagTrackUID 0 = whole file.
- MP4 audio boxes verified (FFmpeg/mutagen): dac3 bit layout + bitrate table; ddts 20 B (avg bitrate, depth, StreamConstruction, ChannelLayout); dOps; dfLa STREAMINFO (total samples + audio MD5 = exact alt key for FLAC tracks!); dmlp ratebits/channel assignment; esds AOT/SBR/PS. dec3 JOC bits not parsed by FFmpeg (memory only). Derived stats all from moov; fragmented frame count needs moof/trun walk. chpl start in 100 ns.

## Wave 6 (07:31Z)
- A: disc/broadcast corpus: build DVD ISO (dvdauthor+genisoimage/xorriso UDF), TS with SDT/EIT (ffmpeg -mpegts_service_name...), AVI (xvid/divx tags), validate offsets & first-MB readability with python parsers
- B: remux identity: which bytes change between two muxes of the same streams (mkvmerge x2, ffmpeg x2, MKV<->MP4) -> container-independent alt key (CodecPrivate + first-N frame payload hashes)
- C: codec config records in CodecPrivate/stsd (avcC, hvcC, av1C, vpcC) for profile/level/bit depth/chroma; HDR/DV labelling rules when Colour/Mastering missing; DV config in old mkvmerge/MakeMKV

### Wave 6 merged (07:44Z)
- Codec config (tested): hvcC bitDepthLumaMinus8/ChromaMinus8 at bytes 17/18 (low 3 bits) -> 10-bit = 2. avcC High10 ext bytes (fd fa fa 00 = 4:2:0 10-bit). av1C byte2 bit6 high_bitdepth, bit5 twelve_bit. vpcC byte6 high nibble bitDepth. => Atlas bit_depth fix: read config record, not Colour.BitsPerChannel.
- HDR SEIs 137/144 in hvcC prefix-SEI array only for ffmpeg muxes; mkvmerge strips them (keeps SEI 5). Otherwise first IDR AU (streams thread). Label precedence: DV record > HDR10+ (2094-40 SEI / BD hdr_plus_flag) > HDR10 (PQ + 137/144 or MasteringMetadata) > PQ > HLG (18) > SDR. Practical fallback: PQ + BT.2020 + 10-bit with no mastering -> "HDR10 (unconfirmed)". dvcC 24 B payload; movenc writes dvcC/dvvC/dvwC by profile, only with -strict unofficial.
- Remux identity (tested, 9 variants + crf28 control): every remux (mkvmerge x2, no_variable_data, ffmpeg copy, MKV->MP4->MKV, cluster sizes) keeps frame payloads byte-identical; avcC == MKV CodecPrivate == MP4 avcC. ALT KEY = hash(codec config) + first video payload + first 8 video + first 16 audio payloads (~200 KB at 1080p test; budget 700 KB, require coverage). Audio-only keys collide with same-film encodes. Volatile in mkvmerge: DateUTC + 2 stats date tags only. Script tools/altkey.py.
- DVD ISO (genisoimage -dvd-video -udf): VIDEO_TS.IFO @567,296, VTS_01_0.IFO @632,832 -> inside 700 KB. IFO decode OK (PAL 720x576, 2x AC-3 en/fr, 3 chapters, BCD time). TS: ffmpeg -metadata service_name/service_provider -> SDT; no EIT/TDT from ffmpeg. M2TS adds HDMV registration + 0x88. AVI: avih/strh/strf/INFO/Lavc user-data in first video chunk (@10.5 KB). Parser tools/disc_ts_avi.py.

## Catalog schema (for waves 7-10)
Each field: id, container, path, raw (ebml id | box | byte offset/bits), type, meaning, example, location (front | tail | after_moov | targeted_fetch | separate_file), linking (twin | alt | same_film | same_disc | same_subs | same_broadcast | tool | uploader | none), privacy (bool), atlas (parsed | partial | buggy | no), verified (spec | source | tested | memory), source.

## Wave 7 (07:45Z)
- A: Dolby Vision test files: cargo install dovi_tool, generate RPU + inject into HEVC, mux mkvmerge + ffmpeg (-strict unofficial) -> verify BlockAdditionMapping/dvcC/dvvC and Atlas dv_* parse
- B: Blu-ray authoring with tsMuxeR (apt/build) -> BD folder + ISO, validate mpls/clpi/index/id.bdmv + UDF tree from tools; DVDDiscID via libdvdread (C program) on the w6a ISO
- C: draft YAML catalog part 1: MKV/WebM (all fields from raw w1a,w2a,w3a,w5b + Atlas status)

### Wave 7 A/B (07:58Z)
- DV tested (dovi_tool 2.1.2 from its GitHub release tarball since it isn't on crates.io; P8.1 + P5 RPUs injected into x265 10-bit PQ): mkvmerge v82 writes BlockAdditionMapping BlockAddIDType 'dvvC' (P8.1: 01 00 10 1d 10 = profile 8, level 3, rpu1 el0 bl1, compat 1) / 'dvcC' (P5: 01 00 0a 1d 00, compat 0). mkvmerge writes NO Colour element for raw HEVC input -> HDR only in SPS VUI. ffmpeg 6.1.1 never writes dvcC/dvvC into MP4 (even -strict unofficial / dvh1 tag) -> DV in ffmpeg MP4s undetectable at container level.
- Atlas on DV MKVs: dv_profile 8/5, level 3, EL false correct at 700K; but hdr/bit_depth/primaries/transfer all null -> FIX: parse SPS VUI (colour_primaries, transfer, matrix, bit depth) from hvcC/avcC NAL arrays in CodecPrivate / stsd (first article); MP4: read colr and hvcC too.
- DVDDiscID CONFIRMED with libdvdread 6.1.3: MD5(VIDEO_TS.IFO || VTS_01_0.IFO .. VTS_0N_0.IFO), whole files, N = VMG nr_of_title_sets. 2nd VTS IFO at ~21 MB -> walk directory entries, targeted fetch. UDF volume set id = 16 hex chars. genisoimage fingerprint: app id "GENISOIMAGE", UDF impl "*genisoimage". tsmuxer not in apt -> BD authoring untested.
- 07:28Z the owner shared "Linking Signals Map" (https://claude.ai/artifact/BEGjW1bU8S6Yqm3cnPMNpE) as the output example: signals by what they prove x cost to read x Atlas coverage. Schema gets `cost:` (first_article | first_4mb | tail | targeted | full). Final report should feed that map: a signals table (proves, cost, atlas) for container signals.
- 07:28Z the owner: "better example" = doc "Media metadata for releases: ideas" (copy in ref/example-media-metadata-ideas.md). Format to follow for the final report: summary + ranked table (Rank | Idea | Value | Effort | Extra bandwidth per release | CPU per release), per-area tables (Field | Where | What it gives | Use), "Fetching just enough" step list, "What this cannot do", per-probe prose. Final = Claude Doc in that style + YAML catalog files (catalog/*.yaml).

### Wave 7 C (08:02Z)
- catalog/mkv.yaml: 231 fields + 28 derived; atlas: 182 no, 40 parsed, 5 partial, 4 buggy; cost: 145 first_article, 33 first_4mb, 52 tail, 1 full. Fixed SegmentUUID/DateUTC linking to [twin, alt].

## Wave 8 (08:03Z)
- A: catalog/mp4.yaml (MP4/MOV/M4V incl. fragmented, iTunes, audio config boxes, derived stats)
- B: catalog/disc_other.yaml (ISO9660/UDF, BDMV index/mpls/clpi/id/meta, DVD IFO + DiscID, TS/M2TS PSI/SI, AVI, ASF, Ogg, FLV, VOB)
- C: fetch planner + storage: incremental "need bytes at offset" design across containers, measured article budgets, what it cannot do; map fields to Atlas storage (probe JSON / tables) and newznab attrs

### Wave 8 merged (08:15Z)
- catalog/mp4.yaml: 234 fields + 20 derived (35 parsed, 2 partial, 197 no). Top missing: moov tail-fetch, colr/mdcv/clli, hvcC bit depth, stsz/stts frames+fps+VFR, bitrates.
- catalog/disc_other.yaml: iso9660 24, udf 25, bdmv 76, dvd 45, ts 35, avi 17, asf 15, ogg 12, flv 10, vob_ps 13, derived 20 (all atlas: no). Cost tags for UDF/DVD look too pessimistic vs w6a (AVDP @512 KB, VIDEO_TS.IFO @567 KB = first_article) -> fix in wave 9.
- raw/w8c.md: probe today = NFO+SFV, first+last article, up to 6 front parts while !header_complete, moviehash tail; no inner-offset mapping. Proposed Need{inner_offset,len} planner + 12-article media budget, 15-case cost table, cannot-do list. Storage: release_probe JSON (+crc32, agent, moviehash cols); no newznab attrs carry probe media today; ~25-item mapping table.

## Wave 9 (08:16Z) — verify + gaps
- A: validate catalogs against local corpus (walk.py/disc_ts_avi.py), fix cost/verified flags, spot-check memory entries
- B: gap analysis vs MediaInfo + ffprobe field lists; HDR10+ in container (MKV BlockAddIDType ITU-T T.35, WebM), Atmos/DTS:X signalling at container level
- C: signals table for the Linking Signals Map (container signals: proves, cost, atlas, notes) -> catalog/signals.md

### Wave 9 merged (08:30Z)
- w9a: mkv.yaml 46 entries spec->tested on corpus; cost fixes (UDF AVDP/VDS + VIDEO_TS.IFO first_article; mkv.tag.encoder front_or_tail). MP4 field checks + spot-checks not done.
- w9b: HDR10+ HAS a header signal: MKV BlockAdditionMapping BlockAddIDType 4 (ITU-T T.35) on the track (FFmpeg matroska.h:363, matroskadec.c:4055); per-frame BlockAdditional country 0xB5 + provider 0x003C = HDR10+. Atmos: only MP4 dec3 JOC flag at container level; MKV A_EAC3/A_TRUEHD bitstream only. DTS:X / IMAX Enhanced: bitstream only. Missing vs MediaInfo/ffprobe: extra dispositions (karaoke, lyrics, captions, dub), HDR_Format_Compatibility, per-track start delay, ARTIST/COMPOSER tags, stsd vendor_id.
- catalog/signals.md: 43 signal rows + JSON for the Linking Signals Map (6 collected, 4 buggy, 33 no; 5 privacy rows).

## Wave 10 (08:31Z) — close out
- A: verify mp4.yaml + disc_other.yaml entries (corpus MP4 moov/ilst/colr/btrt; rebuild DVD ISO; TS/AVI), 15 memory spot-checks
- B: measure 2160p cost numbers: first-IDR size, frames per 700 KB, alt-key coverage, header bytes with big CodecPrivate/fonts
- C: catalog/additions.yaml: fields from w9b (HDR10+ T.35 mapping, dispositions, start delay, tags), merged later

### Wave 10 A/C (08:45Z)
- w10a: mp4.yaml 158 box-level entries now tested on corpus (incl. fragmented, colr, dec3/dac3, chpl). 29 field-level entries (esds.*, ilst freeform, pssh/tenc/schm, mdcv, clli...) were reset to verified: memory by an over-broad match rule, losing their earlier tested/source marks: conservative (understates), recheck before relying on them as unverified. disc_other unchanged (TS/AVI samples confirm tools/disc_ts_avi.py).
- w10c: catalog/additions.yaml: 31 entries (HDR10+ T.35 BlockAddIDType 4 + BlockAdditional payload, stsd vendor_id, ARTIST/COMPOSER/SORT_WITH tags, derived hdr10plus_label, hdr_label_v2, dv_format_compat, atmos, dtsx, 8 dispositions, track_start_offset, 9 fetch_plan entries).
- w10b (synthetic grainy, one run): header before first Cluster 8.1 KB (2160p HEVC) / 5.8 KB (1080p) / 10.5 MB with 10 MB fonts. First IDR 970 KB / 576 KB. Full alt key (8 video + 16 E-AC-3) 4.99 MB / 1.62 MB -> use codec config + first keyframe (1-2 articles). 10 s A/V: 51.7 MB / 15.7 MB video, E-AC-3 0.8 MB, TrueHD 7.5 MB. Tail Cues+Tags ~1.8 KB.
- Archive thread done 07:49Z: report https://claude.ai/artifact/NSkmWVebA67M95ps5NLqP1, catalog research/archive-metadata/archive-field-catalog.yaml. 7-Zip writes no AES salt by default -> 16-byte IV is the per-archive key. Compressed RAR5/RAR2.9/7z decode forward only.

## FINAL (08:58Z)
Report (Claude Doc): https://claude.ai/artifact/Wb8pQ6mzroCGo38pZvLcVK "Container metadata for releases: findings". Catalog: catalog/{mkv,mp4,disc_other,additions}.yaml + signals.md.
- Streams thread final 08:04Z: owns SPS VUI, SEI 137/144, HDR10+ payloads, DV RPU (FEL/MEL, CM), bitstream Atmos/DTS:X. Report https://claude.ai/artifact/ByUTRbbX7sHy2H78JK9iVo, catalog research/stream-metadata/catalog.yaml. matroska-demuxer follows SeekHead to EOF and fails on partial downloads -> hand-written EBML walker. TrueHD Atmos = FBA sync + substream_info bit 7 (52 real files).
