# Container metadata for releases: findings

Nov 2, 2026. Exported from the Claude Doc https://claude.ai/artifact/Wb8pQ6mzroCGo38pZvLcVK

## Summary and ranking

The first article of an MKV already holds almost every container field we want: tracks, languages, flags, codec config, chapters, attachments, the Dolby Vision record and the mux identity. Atlas reads about 40 of the 231 Matroska fields today. The cheapest big wins are fixing four probe bugs and storing what the first article already gives. Tail and targeted reads come second: one extra article each gets mkvmerge bitrate and frame stats, an end-of-file MP4 moov, and Blu-ray playlists.

| Rank | Idea | Value | Effort | Extra bandwidth per release | CPU per release |
| --- | --- | --- | --- | --- | --- |
| 1 | Fix the probe: HDR label, 10-bit depth, hidden-edition chapters, SPS VUI colour fallback | High | Low | None (same first article) | Under 1 ms |
| 2 | Remux-proof alt key: hash of codec config + first video frame + first 8 video and 16 audio payloads | High for alts | Low | Codec config + first keyframe: 1 to 2 articles (keyframe 0.6 to 1 MB). Full 8+16 frame key: 2 to 3 articles at 1080p, about 7 at 2160p on grainy test content | Under 5 ms (hashing) |
| 3 | Store mux identity: SegmentUUID, DateUTC, MuxingApp/WritingApp, Duration float size | High for twins and tools | Low | None | Negligible |
| 4 | MP4 moov tail fetch: moov offset = mdat offset + mdat size | High for MP4 posts | Low | 1 to 2 targeted articles | Negligible |
| 5 | Matroska Tags: IMDB, TMDB, TVDB, ENCODER\_SETTINGS, mkvmerge stats (BPS, NUMBER\_OF\_FRAMES) | High for ID and bitrate | Low | None for Lavf files; 1 tail article for mkvmerge | Negligible |
| 6 | DVDDiscID (MD5 of the IFOs) and Blu-ray MPLS/CLPI/index.bdmv parse | High for disc posts | Medium | First article on 1-title DVDs; 1 to 2 targeted otherwise | Under 5 ms |
| 7 | Subtitle and attachment keys: ASS header hash, font-set hash, VobSub palette | Medium (fansub groups) | Low | None to 4 MB when fonts are large | Negligible |
| 8 | HDR10+ header flag (MKV T.35 block-addition mapping) and MaxCLL/MaxFALL values | Medium for 4K users | Low | None | Negligible |
| 9 | Editions, ChapterSkipType (intro and credits), chapter timing signature | Medium | Low | None to 4 MB | Negligible |
| 10 | Broadcast captures: TS SDT channel, DVB EIT programme and TDT capture time | Medium for TV | Medium | None (first article) | Negligible |
| 11 | FLAC STREAMINFO MD5 as an exact audio key | Low to medium | Low | None | Negligible |
| 12 | Legacy writers: AVI ISFT and Xvid/DivX strings, ASF File ID GUID, Ogg vendor, FLV onMetaData | Low | Low | None | Negligible |

Value scores are judgment calls from the research, not measured on live data. Costs come from local test files (mkvmerge v82, ffmpeg 6.1.1, genisoimage, libdvdread 6.1.3).

## Atlas probe today

`src/probe/media.rs` at main a29ee52 parses MKV and MP4 headers from up to 6 front articles. We ran it on a local test corpus cut to 700 KB, 4 MB and full length, with no panics in 18 runs. Tracks, subtitle names, forced, SDH and commentary flags, the Atmos name check, and the DV profile and level were right at every cut. Four things were wrong and several were missing.

| Finding | Cause | Fix | Cost of fix |
| --- | --- | --- | --- |
| Chapters come from a hidden edition | Parser takes the first EditionEntry and ignores EditionFlagHidden and EditionFlagDefault | Pick the default, non-hidden edition; keep all editions | First article |
| HDR10 shows as "PQ" | mkvmerge and Lavf write no MasteringMetadata when mastering data is only in the HEVC SEI | Label PQ + BT.2020 + 10-bit as HDR10 (unconfirmed); confirm from SEI 137/144 in hvcC (ffmpeg muxes) or the first IDR | First article |
| bit\_depth is null on 10-bit HEVC | Read from Colour.BitsPerChannel, which neither muxer writes | Read hvcC bytes 17/18 (bitDepthLumaMinus8), avcC High10 extension, av1C byte 2, vpcC byte 6 | First article |
| hdr, primaries, transfer null on mkvmerge DV and raw-HEVC files | mkvmerge writes no Colour element for raw HEVC input | Parse the SPS VUI from the hvcC/avcC NAL arrays in CodecPrivate | First article |
| Non-faststart MP4 gives no tracks | moov sits after mdat; probe reads only the front | Fetch moov at mdat offset + mdat size | 1 to 2 targeted articles |
| Chapters missing at 700 KB when fonts are large | 2.2 MB of attachments before Chapters | Already handled by reading on to 4 MB; use SeekHead to jump | First 4 MB |

Not read at all: Tags (IMDB, TMDB, stats), attachments, default flags, video track names, MaxCLL/MaxFALL and mastering values, SegmentUUID, DateUTC, MP4 chapters and title, MP4 colr/mdcv/clli, bitrates. The probe JSON lands in `release_probe`, and no newznab attr exposes probe media yet.

## Matroska (MKV, WebM)

Everything except statistics tags and Cues sits before the first Cluster in mkvmerge output. Lavf (ffmpeg) also puts Tags at the front. The full list is 231 fields plus 28 derived ones in `catalog/mkv.yaml`.

| Field | Where | What it gives | Use |
| --- | --- | --- | --- |
| `SegmentUUID`, `DateUTC` | Info, first article | Per-mux identity and mux time; both change on every mkvmerge run | Same values = same mux run (twin, or alt if re-RAR'd); different date with equal frames = remux |
| `MuxingApp`, `WritingApp`, Duration size | Info | "mkvmerge v82.0 ('I'm The President') 64-bit", "Lavf60.16.100"; Duration is float32 from mkvmerge, float64 from Lavf | Tool and version fingerprint, even with stripped strings |
| `Title`, `SegmentFilename` | Info | Often the release name; SegmentFilename can leak a local file name | Namefix; privacy scrub |
| `PrevUUID`, `NextUUID`, `SegmentFamily` | Info | Links CD1/CD2 and multi-file sets | Completeness of split posts |
| TrackEntry flags and names | Tracks | Default, forced, hearing impaired, visual impaired, commentary, original, text descriptions; BCP 47 language | Search filters; commentary names feed crew lookup |
| `CodecPrivate` | Tracks | avcC/hvcC/av1C/vpcC (profile, level, bit depth, SPS with VUI colour), ASS header, VobSub idx, FLAC STREAMINFO | Bit depth, HDR fallback, alt key, same-subs key |
| Colour, MasteringMetadata, MaxCLL, MaxFALL | Tracks/Video | Primaries, transfer, matrix, range, mastering luminance | HDR10 label and values when the muxer wrote them |
| BlockAdditionMapping | Tracks | `dvcC`/`dvvC` record (profile, level, rpu/el/bl, compat id); type 4 = ITU-T T.35 (HDR10+ metadata present) | DV profile 5/7/8.x; HDR10+ without frame reads |
| Editions and ChapterAtoms | Chapters, first 4 MB | Default/hidden/ordered editions, chapter times and names, ChapterSkipType (intro, credits, recap), linked OP/ED segments | Cut detection, chapter timing signature, skip markers |
| Attachments | Before first Cluster | Fonts and cover art with names, MIME (can be wrong), UIDs | Font-set hash (fansub group), cover hash |
| Tags: IMDB, TMDB, TVDB, TVDB2 | Front (Lavf) or tail (mkvmerge) | Embedded database IDs | Direct identification, no name match |
| Tags: BPS, NUMBER\_OF\_FRAMES, NUMBER\_OF\_BYTES, DURATION | Tail for mkvmerge | Exact per-track bitrate, frame count, subtitle event count | Bitrate attrs, same-encode checks |
| Tags: ENCODER, ENCODER\_SETTINGS, ENCODED\_BY | Front or tail | Encoder build and option line; ENCODED\_BY can name a person | Encode fingerprint; privacy scrub |
| SeekHead, Cues | Front / tail | Offsets of Tags and Cues; keyframe index | Targeted fetches and mid-file seeks |

Unknown-size Segments and Clusters (live recordings) and header-stripping compression (ContentCompAlgo 3) must be handled, or CodecPrivate is read wrong.

## MP4, MOV and M4V

All MP4 metadata lives in `moov`. ffmpeg puts it at the end by default and MP4Box at the front, so a non-faststart file needs one targeted fetch. The catalog has 234 fields plus 20 derived ones in `catalog/mp4.yaml`.

| Field | Where | What it gives | Use |
| --- | --- | --- | --- |
| `ftyp` brands, `free`, `mdat` header | First bytes | Brand set per tool; mdat size (32-bit, or 64-bit when size = 1) | moov offset = mdat offset + mdat size |
| `mvhd`, `tkhd`, `mdhd` | moov | Durations, timescales, packed ISO 639 language, creation times (0 from ffmpeg) | Runtime; non-zero creation time is a mux key |
| `hdlr` name, `©too`, compressorname | moov, udta | "VideoHandler" and "Lavf60.16.100" (ffmpeg), GPAC and L-SMASH names, HandBrake | Muxer fingerprint |
| `avcC`, `hvcC`, `av1C`, `vpcC` | stsd | Same bytes as MKV CodecPrivate; bit depth, profile, level, SPS | Bit depth, alt key across MKV and MP4 |
| `colr`, `mdcv`, `clli` | stsd entry | nclx primaries, transfer, matrix; mastering and light levels | HDR10 label and MaxCLL/MaxFALL |
| `dvcC`, `dvvC`, `dvwC` | stsd entry | DV profile, level, flags, compat id | DV, but ffmpeg 6.1 never writes these, so ffmpeg MP4s show DV only in RPUs |
| `dac3`, `dec3`, `dac4`, `ddts`, `dmlp`, `dOps`, `dfLa`, `esds` | stsd entry | Channels, bitrate, AAC object type, TrueHD format, DTS construction; dec3 JOC flag = Atmos; dfLa MD5 of decoded audio | Audio attrs, Atmos, exact FLAC alt key |
| `stts`, `ctts`, `stss`, `stsz` | stbl | Exact frame count, CFR/VFR, keyframe interval, B-frames, per-track bytes | Exact bitrate and fps without decoding |
| `elst` | edts | Start offset and AAC priming | Correct runtime and sync |
| `chpl`, chapter track (`tref chap`) | udta / trak | Chapter list (chpl in 100 ns units) | Chapters for MP4 |
| `ilst`: `©nam`, `tvsh`, `tvsn`, `tves`, `stik`, `hdvd`, `cnID`, `sfID`, iTunEXTC, iTunMOVI | udta/meta | Title, show and episode, media kind, store catalog ID, storefront, rating, cast | cnID = same film with no name match |
| `ilst`: `apID`, `purd`, `ownr` | udta/meta | Buyer's Apple ID (an email), purchase date, owner name | Privacy: hash or drop, never display |
| `mvex`, `moof`, `trun`, `sidx`, `mfra` | Fragmented files | Durations; frame counts need a walk of every moof | WEB-DL style files |
| `pssh`, `tenc` | moov | DRM leftovers | Source hint (streaming rip) |

## Blu-ray, DVD and ISO images

Disc posts need a small file-system walk, then targeted reads of the small disc files. Offsets were checked against the libbluray 1.5.0, libdvdread 7.1.1 and Linux UDF sources, and DVDDiscID was confirmed with libdvdread 6.1.3. Atlas parses none of this today. Details are in `catalog/disc_other.yaml`.

| Field | Where | What it gives | Use |
| --- | --- | --- | --- |
| ISO 9660 PVD | Byte 0x8000, first article | Volume ID, system and application ID ("GENISOIMAGE"), publisher, preparer, dates, volume size | Home-made vs pressed tell; size check; preparer is a privacy field |
| UDF AVDP, PVD, LVD, implementation IDs | Sector 256 (0x80000), first article | Volume set ID (16 hex chars), implementation ID ("\*genisoimage", mastering tools), file tree location | Walk to BDMV and VIDEO\_TS files |
| `index.bdmv` | About 8 MB into a BD ISO (targeted) | Version 0300 = UHD; initial dynamic range; extension 3.1 disc flags for 4K, HDR10, HDR10+, DV | Disc-level HDR and UHD |
| `PLAYLIST/*.mpls` | Targeted | PlayItems with IN/OUT on a 45 kHz clock (runtime), STN streams with codec, language, format, rate, HEVC dynamic\_range\_type, hdr\_plus\_flag, DV EL entries; PlayListMark chapters; static HDR extension | Main title runtime, audio and subtitle languages, chapters, HDR per stream |
| `CLIPINF/*.clpi` | Targeted | TS recording rate, source packet count, program info | Clip bitrate and duration check |
| `CERTIFICATE/id.bdmv` | Targeted | org\_id at byte 40, 16-byte disc\_id at byte 44 | Candidate same-disc key (per-pressing behaviour unconfirmed) |
| `META/DL/bdmt_*.xml` | Targeted | Disc title, thumbnails | Title hint |
| `VIDEO_TS.IFO`, `VTS_nn_0.IFO` | VIDEO\_TS.IFO at 567 KB in a genisoimage ISO; later title sets 20 MB+ in | Video standard and aspect, audio and subpicture languages and formats, PGC runtimes (BCD), chapters | DVD runtime, languages, main title (longest PGC) |
| DVDDiscID | Computed | MD5 of VIDEO\_TS.IFO then VTS\_01\_0 to VTS\_0N\_0.IFO, whole files, N = title-set count | Exact same-disc key across rips |

Main-title rule, following BDInfo: drop playlists under 20 s and looping ones, group playlists that share clips, then take the longest in the top group and check its chapter count. In RAR'd BDMV folders, volume 1 usually holds index.bdmv, MovieObject, MPLS and CLPI, because the small folders sort before STREAM. That order is not verified.

## TS, AVI and other containers

These formats keep their headers in the first article, so one read covers them. Builds with ffmpeg and dvdauthor confirmed the TS, M2TS, AVI and DVD layouts. EIT and TDT need real broadcast captures, because ffmpeg doesn't write them.

| Container | Field | What it gives | Use |
| --- | --- | --- | --- |
| TS / M2TS | Packet size (188, 192, 204), PAT/PMT stream types, ISO 639, registration (HDMV, AC-3, EAC3, DTS, HEVC, CUEI) and DOVI 0xB0 descriptors | Codecs, languages, DV config, Blu-ray origin (M2TS 4-byte arrival timestamps) | Track table for TS posts |
| TS (DVB/ATSC SI) | SDT service and provider name, EIT present/following event, TDT/TOT time, NIT network, ATSC VCT channel | Channel, programme title, capture time | Same-broadcast key; capture date |
| AVI | avih, strh, strf, strn, INFO ISFT/INAM/ICMT, odml dmlh, JUNK, codec user data in the first video chunk ("XviD0050", "DivX503b1393p", "Lavc60.31.102") | Frame count, fps, codecs, writer and encoder build | Tool fingerprint; idx1 at the tail is optional |
| ASF / WMV | File Properties (File ID GUID, size, play duration), Content Description, WM/Encoder, WM/EncodingSettings, Codec List | Per-file GUID, encoder settings | Twin key (GUID), tool fingerprint |
| Ogg / OGM | Vorbis vendor string, comment tags, stream serials; duration from the last page | Encoder, tags; per-mux serial | Tool fingerprint; tail read for duration |
| FLV | onMetaData AMF: duration, size, fps, bitrates, encoder, metadatacreator | Runtime and encoder | Tool fingerprint |
| VOB / MPEG-PS | Pack and system headers, private stream 1 substreams (AC-3 0x80, subpicture 0x20), NAV packets | Streams of a loose VOB | Rare as a standalone post |

## Linking keys from containers

The container layer gives a twin key (SegmentUUID + DateUTC), a tested remux-proof alt key, and exact same-disc keys. All 43 signals, each with what it proves, its read cost and Atlas status, are in `catalog/signals.md`, with a JSON copy for the Linking Signals Map.

| Key | Proves | Read cost | Notes |
| --- | --- | --- | --- |
| SegmentUUID + DateUTC (MKV) | Twin, or alt after a re-RAR | First article | New on every mux. mkvmerge `--engage no_variable_data` drops them |
| Codec config + first frame payload hashes | Alt (remux, MKV to MP4, re-RAR) | 1 to 3 articles | Matched all 9 remux variants and rejected a re-encode. Audio-only hashes match re-encodes too, so always include video |
| FLAC STREAMINFO MD5 | Alt for that audio track | First article (MKV), moov (MP4) | All-zero means not computed |
| ASF File ID GUID | Twin | First article | New per mux |
| MKV IMDB/TMDB/TVDB tags, iTunes cnID | Same film | First article or tail | Present on tagged files only |
| Chapter timing signature | Same disc or same cut | First 4 MB | Hash the start times and count of the default edition |
| DVDDiscID | Same disc | First article to targeted | Confirmed algorithm |
| BD id.bdmv disc\_id | Same disc (likely) | Targeted | Needs two rips of one disc to confirm |
| ASS header hash, font-set hash, VobSub palette | Same subs or fansub group | First article to 4 MB | Strip the Aegisub section before hashing |
| TS SDT + EIT + TDT | Same broadcast | First article | Channel, programme, capture time |
| MuxingApp/WritingApp, hdlr/©too, ISFT, UDF implementation ID | Tool | First article (MP4: with moov) | Version-level fingerprints |

**Privacy.** iTunes `apID` (buyer's Apple ID, usually an email), `purd` and `ownr`, the Aegisub "Video File" local path, Matroska ENCODED\_BY and PURCHASE\_OWNER, SegmentFilename and ISO preparer fields can identify a person. Store a salted hash at most for uploader linking, and never display them.

## Fetching just enough

Today the probe reads up to 6 front articles while the header is incomplete, plus a moviehash tail, and it never maps an inner offset to an article. The proposal: each parser returns `Need { inner_offset, len }`, and a planner turns that into a RAR volume and a yEnc part, with a budget of 12 media articles per release.

1. Read volume 1's first article. It holds the RAR or 7z header and the start of the inner file, so share it with the archive probe.
2. Parse incrementally. MKV stops at the first Cluster, MP4 at `moov` or the `mdat` header, ISO after the UDF anchor at 512 KB.
3. Follow pointers. MKV SeekHead gives Tags and Cues offsets, MP4 mdat size gives the moov offset, and the UDF file tree gives BDMV and IFO extents.
4. Map inner offset X in a store-mode RAR to volume floor(X / payload per volume), at the volume's header size + X mod payload. The article is that offset / yEnc part size. Confirm with the target volume's own header. For 7z Copy, use 32 + PackPos + X across the joined volumes.
5. Read the last article only when it pays: mkvmerge stats tags (about 2 KB at the very end), moviehash, or an end-of-file moov.

| Case | Articles | What they give |
| --- | --- | --- |
| MKV, mkvmerge, normal fonts | 1 | Info, Tracks, codec config, Chapters, Attachments, DV record |
| MKV with large fonts (2 to 10 MB) | Up to 15 at 10 MB fonts | Chapters sit after the fonts, so use SeekHead to jump past them |
| MKV stats tags (bitrate, frames) | +1 tail | BPS, NUMBER\_OF\_FRAMES, NUMBER\_OF\_BYTES per track |
| MKV alt key (config + first keyframe) | 1 to 2 | First keyframe measured at 576 KB (1080p) and 970 KB (2160p) |
| MP4 faststart | 1 | Whole moov |
| MP4 moov at end | 1 + 1 to 2 targeted | moov at mdat offset + mdat size |
| DVD ISO, one title set | 1 | PVD, UDF, VIDEO\_TS.IFO, VTS\_01\_0.IFO, DVDDiscID |
| DVD ISO, more title sets | 1 + 1 per extra IFO | Later IFOs measured about 21 MB in |
| Blu-ray ISO | 1 + 1 to 2 targeted | index.bdmv (about 8 MB in), MPLS, CLPI, id.bdmv |
| RAR'd BDMV folder | 1 to 2 | Small BDMV files usually sit in volume 1 |
| TS, M2TS, AVI, ASF, FLV | 1 | PSI/SI tables or headers |
| Compressed 7z (LZMA2) | Front articles, decoded forward | Works as a prefix: 10% of an archive gave 786 KB of the MKV |

Codec config + first keyframe is the practical alt key. The full 8-video + 16-audio key needed 1.6 MB at 1080p and 5 MB at 2160p on grainy test content, which is pessimistic but beyond one article.

## What this cannot do, and open questions

- Encrypted RAR or 7z gives nothing past the archive header.
- Compressed RAR needs the young `rars` crate and decodes only forward from volume 1. Mid-file seeks need store mode. Solid archives behave the same way.
- Fragmented MP4 frame counts need a walk of every moof.
- DV RPU levels, MEL vs FEL, HDR10+ payload values, Atmos in MKV and DTS:X live in the bitstream. The stream-metadata thread owns those.
- ffmpeg MP4s carry no DV box, so DV shows only in RPUs.
- Blu-ray authoring couldn't be tested here (tsMuxeR isn't packaged), so the MPLS and CLPI offsets rest on source code, not test discs.
- The proxy blocked real DV, HDR10, anime and MakeMKV samples, so every test file was synthetic.

Open questions:

- Is the id.bdmv disc\_id per pressing or per title? Check two rips of one disc and two of another.
- What share of release MP4s are faststart? The answer could come from Atlas's own `release_probe` data.
- Do pressed-disc ISOs carry a mastering-tool UDF implementation ID that separates them from home-made images?
- Does mkvmerge write the T.35 HDR10+ block-addition mapping? We only confirmed it in FFmpeg's code.
- Do BD dynamic\_range\_type enum values come from libfreemkv? libbluray doesn't define them.
- In the MP4 catalog, 29 field-level entries were reset to unverified during the last check. They need a recheck.

## Files

Everything is in `docs/research/container-metadata/`.

| File | What it holds |
| --- | --- |
| `catalog/mkv.yaml` | 231 Matroska fields + 28 derived (path, EBML ID, meaning, example, location, cost, linking, privacy, Atlas status, how verified, source) |
| `catalog/mp4.yaml` | 234 MP4/MOV fields + 20 derived |
| `catalog/disc_other.yaml` | 292 entries: ISO 9660, UDF, BDMV, DVD, TS, AVI, ASF, Ogg, FLV, VOB + derived |
| `catalog/additions.yaml` | 31 gap fills: HDR10+ T.35 mapping, dispositions, start offsets, fetch plans |
| `catalog/signals.md` | 43 linking signals with proves, cost and Atlas status, plus JSON for the Linking Signals Map |
| `tools/walk.py`, `tools/disc_ts_avi.py`, `tools/altkey.py` | Python reference walkers and the alt-key script, to port to Rust |
| `tools/probe-harness/` | Runs Atlas's current `media.rs` on files at 700 KB, 4 MB and full |
| `log.md`, `raw/w1a.md` to `raw/w10b.md` | Wave log and raw notes with sources |

Sibling threads: [archive metadata](https://claude.ai/artifact/NSkmWVebA67M95ps5NLqP1) covers RAR and 7z offsets and decoding. [Stream metadata](https://claude.ai/artifact/ByUTRbbX7sHy2H78JK9iVo) owns the SPS VUI, SEI 137/144, HDR10+ payloads, DV RPU (FEL/MEL, CM version) and bitstream Atmos/DTS:X detection. It also found that the matroska-demuxer crate fails on partial downloads, so a hand-written EBML walker is the safer base.
