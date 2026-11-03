# Media metadata for releases: ideas

Nov 2, 2026 · the owner

## Summary and ranking

The best value is reading the first 2 to 4 articles of a release's main video file and parsing its Matroska header in Rust. That one cheap fetch (about 3 MB) yields audio and subtitle languages, commentary tracks, chapter names, exact runtime, HDR and Dolby Vision profile, and a SegmentUID that identifies the exact encode. Adding the first video frames gives DV MEL or FEL. Header-only ideas (yEnc CRC32, poster signature) cost no extra bandwidth at all. Credits OCR and audio fingerprints are real but expensive, so keep them for releases nothing else could identify.

| Rank | Idea | Value | Effort | Extra bandwidth per release | CPU per release |
| --- | --- | --- | --- | --- | --- |
| 1 | Matroska header parse: tracks, languages, codecs, runtime, chapters, commentary titles, SegmentUID | High | Low | 1 to 3 articles (about 0.7 to 2.2 MB) | Under 5 ms (Rust EBML parse) |
| 2 | yEnc `crc32=` per RAR volume matched against srrDB SFV | High for ID | Low | None (last article of each volume, often already fetched) | Negligible |
| 3 | Dolby Vision profile from the track header, then MEL or FEL from the first RPUs | High for 4K users | Low to medium | 1 to 3 more articles after the header | About 10 to 50 ms (RPU parse, no decode) |
| 4 | OpenSubtitles moviehash (first and last 64 KB plus size) to IMDb ID | High for ID | Low | 1 article at the end of the file | Negligible |
| 5 | HDR10+ and HDR10 mastering metadata (MaxCLL, MaxFALL) | Medium | Low | Same articles as rank 3 | About 10 ms |
| 6 | Commentary track names to TMDB crew lookup | Medium for ID | Low | None beyond rank 1 | Negligible |
| 7 | Chapter timing signature to group remuxes of the same disc and spot cuts | Medium | Medium | None beyond rank 1 | Negligible |
| 8 | Poster signature from NNTP and yEnc headers (software, provider, line length, segment size) | Medium for grouping and trust | Low | None (HEAD or overview data) | Negligible |
| 9 | PAR2 creator, set ID and 16 KB MD5 for cross-repost dedupe | Medium | Low | None (already in the fingerprint collector) | Negligible |
| 10 | Atmos and DTS:X detection from the first audio frames | Medium | Medium | Part of rank 3's articles | About 20 ms |
| 11 | Keyframe perceptual hashes at fixed offsets to group encodes of the same title | Medium | High | 3 to 6 articles per sample point | 0.2 to 1 s per 4K frame decode |
| 12 | Credits or title-card OCR to TMDB name search | Low to medium | High | 10 to 20 MB | 1 to 3 s per frame (decode plus OCR) |
| 13 | Audio fingerprint of a 10 to 60 s window | Low | High | 60 to 400 MB on a remux | About 1 s plus your own reference database |
| 14 | Speech-to-text of commentary or dialogue | Low | Very high | 100 MB or more | GPU minutes |

The value scores are judgment calls, not measurements. I haven't checked them against the live data.

## NNTP and yEnc header fields

The yEnc trailer's `crc32=` is the strongest free identifier, because srrDB's SRR files carry the SFV with the CRC32 of every RAR volume. The NNTP headers mostly identify the uploader rather than the title, which helps group reposts and score trust. The fingerprint collector in `rust/crates/fingerprint` already samples 12 articles per release and keeps their headers, PAR2 data and the first 16 KB MD5, so most of this is a matter of keeping fields it already sees.

| Field | Where | What it gives | Use |
| --- | --- | --- | --- |
| `=yend crc32=` | Last part of each file (some posters put it on every part) | CRC32 of the whole posted file, often a RAR volume | Match the SFV in srrDB with no RAR-header probe. This complements the inner-CRC32 work in #1252. |
| `=ybegin name=` | Every part | The real file name, even when the subject is obfuscated | Feeds namefix and srrDB name lookups |
| `=ybegin size=`, `total=` | Every part | Exact file size and part count | srrDB size match, completeness checks |
| `=ybegin line=` and article size | Every part | Line length (128, 256, 4096 and so on) and segment size | Poster software signature |
| `Message-ID` shape | Overview | Random local part plus domain differs by tool (ngPost, Nyuu, GoPostStuff, newsmangler) | Poster software signature |
| `From`, `Organization`, `User-Agent`, `X-Newsposter` | HEAD | Sometimes the tool name and version; From is often randomized per post | Signature, never shown raw to users |
| `Injection-Info`, `NNTP-Posting-Host`, `Path` | HEAD (varies by provider) | Which backbone accepted the upload | Group uploads from the same origin |
| `Newsgroups` and `Xref` | Overview | The cross-post set | Group signature, retention checks |
| `Date` against arrival time | Overview | Posting cadence and the span from first to last article | Separates one bulk upload from a slow repost |
| PAR2 creator packet | PAR2 index file | `ParPar 0.4.x`, `par2cmdline`, `MultiPar` | Poster software signature |
| PAR2 file MD5 and 16 KB MD5, recovery set ID | PAR2 index file | Hashes of each inner file | Exact-file dedupe across reposts and providers |

A "poster signature" would be a hash of the software, line length, segment size, PAR2 creator, injection origin and group set. On its own it does not name the title. It does let one known-good identification carry over to every repost that shares the signature and the PAR2 16 KB MD5.

## Fetching just enough of the file

Almost every in-scope release is an MKV inside store-mode RAR volumes, so inner byte offsets map directly to article offsets and no decompression is needed. A yEnc article is usually about 700 KB decoded, so the first 1 MB of the MKV is 1 or 2 articles.

1. Pick the main video file: the largest inner file, using the NZB, the PAR2 file list and the RAR header #1252 already reads.
2. Fetch the first article of the first volume. It holds the RAR header (#1252 needs this anyway) and the start of the MKV. Share this fetch instead of fetching twice.
3. Parse the EBML header incrementally and fetch the next article only while the parser still needs bytes. Tracks, Chapters and Info normally come before the first Cluster in mkvmerge output. Tags and Cues are often at the end and can be skipped.
4. For Dolby Vision, HDR10+ and Atmos, read on into the first Cluster until the first video keyframe and a few audio frames have been parsed. That is usually 1 to 3 more articles.
5. For the OpenSubtitles hash, fetch the article that holds the last 64 KB of the inner file. It sits in the last RAR volume at a computable offset.
6. For frame or audio samples (ranks 11 to 13), seek by computing the byte offset from runtime and size. Fetch a few articles, resync on the next Cluster ID and decode from the next keyframe.

What this cannot do:

- Passworded or encrypted RARs give nothing past the RAR header.
- Compressed RARs (rare for video) need sequential decompression from the start, so only the header probe works.
- MP4 files with the `moov` box at the end need the tail first. It is still one or two articles.
- Full BDMV or ISO posts need MPLS playlist parsing instead. That is a separate small parser with the same partial-fetch pattern.

## What each probe can find

Most of these can run in pure Rust with no external process. quietvoid's `dolby_vision` and `hdr10plus` crates, the libraries behind dovi\_tool and hdr10plus\_tool, parse RPUs and HDR10+ SEI directly. A small EBML reader covers Matroska. ffprobe and mediainfo stay useful as a test oracle and a fallback, and mediainfo already runs in postprocess (#1257). Only frame decode, OCR and audio fingerprints need native libraries.

**Dolby Vision profile, MEL and FEL.** mkvmerge 51 and later write the DV configuration record (`dvcC` or `dvvC`) into the track's BlockAdditionMapping. That record gives the profile (5, 7, 8.1, 8.4), level and whether an enhancement layer is present, all from the header. MEL against FEL only matters for profile 7. It needs the RPUs from the first few frames: a FEL carries non-trivial NLQ residual data, while a MEL's enhancement layer is empty. dovi\_tool's `info` reports this per frame, so the crate can do the same on the first keyframe's RPU. The RPU also gives the CM version (2.9 or 4.0) and the L6 MaxCLL and MaxFALL. Hybrid WEB and BluRay DV remuxes show up as profile 8.1 with a CM v4.0 RPU.

**HDR format.** The Matroska Colour element gives the transfer (PQ or HLG) and MasteringMetadata. HDR10+ needs the first frames' SEI T.35 payloads. Output is a simple set: `SDR`, `HDR10`, `HDR10+`, `HLG`, `DV`, or combinations such as `DV HDR10+`.

**Chapter names.** These come from the EditionEntry and ChapterAtom elements in the header. Most discs say "Chapter 01" and the like, which isn't useful as text. The timings are the useful part: every remux of the same disc shares them to the millisecond. Named chapters ("Opening Credits", scene names) are worth storing and indexing when present. Several editions in one file reveal a seamless-branching extended cut.

**Audio codecs and languages.** Codec ID, language (BCP 47 or ISO 639-2), channels, default and forced flags, and the track name all come from the header. Atmos (TrueHD or E-AC-3 JOC) and DTS:X need a few audio frames from the first Cluster, which the rank 3 fetch already covers. This replaces and extends what `audio_data` gets from mediainfo today.

**Runtime.** Segment Info Duration is in the header, so an exact runtime costs nothing. It separates theatrical, extended and director's cuts of a movie when compared with the TMDB runtime. With chapter timings it can also match a TV episode within a season when the name doesn't.

**Subtitles.** Track language, codec (PGS, SRT, ASS, VobSub), forced flag and track name ("SDH", "Forced", "Commentary") come from the header. Reading the text of SRT subtitles would mean fetching interleaved Clusters and isn't worth it for metadata. PGS is images and would need OCR.

**Director commentary.** Commentary tracks are usually named, for example "Commentary with director Jane Doe and cinematographer John Roe". Pull the person names out of the track title and look them up in TMDB credits. One director plus the year from the release name usually narrows the movie to a single result. This is cheap because it only uses header text. Transcribing commentary audio is possible but costs GPU minutes per release, so I'd leave it out.

**Credits and title cards.** End credits are in the last 5 to 10 percent of the runtime. A probe would fetch a few articles at that offset, decode one keyframe and run OCR (Tesseract or a Rust ONNX OCR model). Names in the credits are then matched against TMDB cast and crew, and two or three distinctive names usually pin the title. The opening title card is near the start and is cheaper to reach, but many films have none. It costs 10 to 20 MB and 1 to 3 seconds of CPU per frame for 4K HEVC. Keep this for releases that ranks 1 to 9 left unidentified.

**Audio fingerprints.** Chromaprint (fpcalc) only identifies music via AcoustID, so it would catch soundtrack songs, not the film. Identifying the film needs our own reference database built from releases we have already identified. The audio is interleaved with video, so a 30 s window of a remux costs about 200 MB. The better use is grouping encodes of the same title. Keyframe perceptual hashes (rank 11) do that more cheaply.

**Free extras from the same header.** SegmentUID identifies one specific mux, so reposts of the same file match exactly. The existing `release_unique` table stores mediainfo's unique ID and is the same idea. MuxingApp and WritingApp, along with x264 or x265 settings in the codec private data, give an encoder or group signature. The Title element often holds the original release name, which helps namefix.

## Storage and API exposure

Extend the tables that mediainfo already fills, add two small ones, and copy a compact tag string into the Manticore document so free-text search finds it. NZBHydra2 parses the standard newznab attrs, and those cover video, audio, language and subs. It passes free text through to the indexer, so search tokens reach every client. Custom attrs and search parameters are mainly for direct API use.

| Data | Stored in | Newznab attr (extended=1) | Searchable |
| --- | --- | --- | --- |
| Video codec, resolution, frame rate | `video_data` (exists) | `video`, `resolution`, `framerate` (standard) | Already in the name |
| HDR format | `video_data.hdr_format` (new column) | `video` value gains it, plus custom `hdr` | Tag `hdr10`, `hdr10plus`, `dv` |
| DV profile, EL type, CM version, MaxCLL and MaxFALL | `video_data` new columns `dv_profile`, `dv_el` (none, MEL, FEL), `dv_cm`, `max_cll`, `max_fall` | Custom `dvprofile`, `dvel` | Tags `dvp7`, `fel`, `mel`, `dvp8` |
| Runtime in ms | `video_data.videoduration` (exists as text; add an integer `duration_ms`) | Custom `runtime` in minutes | `minruntime` and `maxruntime` parameters |
| Audio tracks: codec, channels, language, Atmos, DTS:X, commentary, title | `audio_data` (exists) plus `is_commentary`, `atmos`, `dtsx` | `audio` and `language` (standard, comma list) | Tags `atmos`, `truehd`, `dtsx`, language codes |
| Subtitle tracks: language, codec, forced, SDH | `release_subtitles` (exists) plus `codec`, `forced`, `sdh` | `subs` (standard, comma list) | `sublang` parameter, tag `subs-eng` |
| Chapters | New `release_chapters` (release, index, start\_ms, name) plus a chapter-timing hash on the release | Custom `chapters` (count) | Chapter names in a full-text field |
| SegmentUID, writing app, encoder settings | `release_unique` (exists) plus a new `release_mux` row | None (internal) | No |
| Poster signature, yEnc CRC32 per volume | New `release_post_signature` row; CRC32s next to the fingerprint metadata | None (internal) | No |
| Probe state | Reuse the fingerprint job queue pattern: status, bytes fetched, tool version, error | None | No |

On the API side:

- Fill the standard attrs on every item when `extended=1`, and keep unknown attrs out of the non-extended response so clients that dislike surprises aren't affected.
- Add custom search parameters `hdr`, `dvprofile`, `dvel`, `audiolang`, `sublang`, `atmos`, `minruntime` and `maxruntime` to `t=search` and `t=movie`. List them in `caps` so the parameters are discoverable.
- Put a short tag string such as `dv dvp7 fel atmos truehd eng subs-eng commentary` into its own Manticore field. Then `q=dune fel` or `q=dune atmos` works through Hydra with no client changes. This is the part users will actually feel.
- An identification found by a probe (commentary names, OpenSubtitles hash, OCR) should go through the same evidence path as srrDB and #1252, with its source and confidence recorded, never written straight over the name.

## Cost per 1,000 releases

The header and first-frames probes cost about 4 to 6 GB and a few CPU seconds per 1,000 releases, which is small next to normal header ingest. The ranking table above lists per-release costs. These totals assume 700 KB articles, all releases probed for phases 1 and 2, and 10 percent left unidentified for phase 3. I haven't checked the real daily in-scope release count, so scale by that.

| Phase | What runs | Bandwidth per 1,000 releases | CPU per 1,000 releases | New dependencies |
| --- | --- | --- | --- | --- |
| 1. Headers only | yEnc CRC32 to srrDB, poster signature, PAR2 creator | Under 50 MB (extra last-part fetches only) | Under 1 s | None |
| 2. Header and first frames | MKV header, DV, HDR10+, Atmos, OpenSubtitles hash, commentary lookup | 3 to 6 GB | 10 to 60 s | `dolby_vision` and `hdr10plus` crates (Rust), OpenSubtitles API key |
| 3. Unidentified only (about 100) | Credits OCR, keyframe hashes | 1 to 2 GB | 5 to 10 min | ffmpeg or a decoder library, an OCR model |
| 3b. Audio fingerprint (not recommended) | 30 s windows | 20 GB or more | About 2 min plus a reference database | Chromaprint |

The slow part is NNTP latency per article, not bytes. The probe should share connections and the circuit breaker with the fingerprint collector and run at low priority behind ingest.

## Suggested order and open questions

Start with phase 2's Matroska header parse, because it delivers most of what users will see. Phase 1 can ride along since it is nearly free.

1. A Rust EBML header parser plus the partial-fetch reader, sharing the first-article fetch with #1252. Fill `video_data`, `audio_data`, `release_subtitles` and `release_chapters`. Test it against mediainfo output on a sample of releases.
2. DV and HDR10+ from the first frames using the `dolby_vision` and `hdr10plus` crates, then the API attrs and the Manticore tag field.
3. yEnc CRC32 to srrDB SFV matching, folded into the identification thread's evidence path.
4. Commentary-name and OpenSubtitles-hash identification as two more evidence sources.
5. Credits OCR and keyframe hashes only if the unidentified backlog is still large after steps 1 to 4.

Open questions:

- Can we use an OpenSubtitles API account? Their hash search needs an API key and has daily limits.
- AGENTS.md requires approval for new dependencies. Are the `dolby_vision` and `hdr10plus` crates OK, and later an OCR model?
- Should custom attrs (`hdr`, `dvprofile`, `dvel`) appear in normal responses, or only with `extended=1`?
- Does #1252 already keep the yEnc `crc32=` from the trailer? If it does, rank 2 is already partly done.
- What is the real number of in-scope releases per day? The cost table scales with it.
