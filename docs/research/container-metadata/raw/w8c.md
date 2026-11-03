# w8c: Atlas fetch planner and storage design

Scope: two design notes grounded in the Atlas repo at a29ee52 (read-only) and the wave 1-7 research. Repo files read: `src/probe/mod.rs`, `step.rs`, `archive.rs`, `media.rs` (header and structs), `moviehash.rs`, plus greps of `db.rs`, `store.rs`, `api.rs`, `field_search.rs`, `media/query.rs`. No repo files changed.

Labels: [measured] = run in this research on synthetic files; [spec] = from the format spec or source read; [est] = arithmetic or memory, not run on a real release.

## Note A: Fetch planner

### What the probe does today

- **Articles per release.** `ProbeStep::run` (step.rs) reads, in order: the NFO and SFV (up to 3 small files, first article each); the first article of the main file (`main_file`: the most parts, first volume by name); the middle article, as an existence check only; the last article (`tail_articles`, `SegmentPick::Last(2)`), as a body. It then reads up to `FRONT_ARTICLES = 6` front parts, one after another, while `media.header_complete` is false (`read_more_front`). Each body fetch is capped at `ARTICLE_CAP = 4 MiB` (archive/mod.rs).
- **RAR inner offsets.** Only the first volume's bytes are parsed. `archive::parse` returns `stored_at` for a store-mode first file (RAR4 `block + HEAD_SIZE`, RAR5 end of the file header block), and `inspect` parses the media from `body[stored_at..]`. Nothing maps an inner offset to a later volume or article.
- **Moviehash tail.** `moviehash_of` is the one place that already maps offsets to articles. For a single file it reads `size - 64 KiB .. size` via `file_range`. For a stored RAR set it uses `moviehash::last_volume`, reads the last volume's first article, parses its header for `stored_at` and `packed`, and reads the 64 KiB at `stored_at + packed - 64 KiB`. `moviehash::parts_for` does the offset to part arithmetic. `read_part` rejects any article whose yEnc `begin` is not `offset + 1`, so a wrong layout fails safe.
- **Gaps.** No parser can say what it needs next. `MediaInfo.header_complete` is the only signal, and it leads to sequential front reads only. MP4 needs `has_whole_moov`, so a non-faststart MP4 gets nothing from the front. 7z returns `format: "7z"` with no `stored_at`, so no inner media is probed. Compressed RAR (m1 to m5) gives headers only.

### Proposed design: needs, not more bytes

Parsers return what they need, not a flag:

```text
enum Parsed { Done(MediaInfo), Need(Vec<Need>) }
struct Need { inner_offset: u64, len: u64, purpose: Purpose, priority: u8 }
```

- MKV: the SeekHead gives the position of Chapters, Tags, Attachments and Cues (relative to Segment data). A Tags offset is a Need. Cues are never needed for metadata.
- MP4: the first top-level box walk gives `mdat` size. `moov` offset = `mdat_offset + mdat_size` (size 1 means a 64-bit largesize, size 0 means to EOF). That is a Need for `moov`.
- ISO/UDF: AVDP at 0x80000, then the VDS and partition descriptors give the metadata partition. The FSD (LBA 0 of the metadata partition) gives the BDMV/VIDEO_TS extents. Those extents become Needs for `index.bdmv`, `MovieObject.bdmv`, the PLAYLIST `.mpls` files, the CLIPINF `.clpi` files of main candidates, and the IFOs.

A planner turns Needs into articles:

1. Layout: for a single file, identity mapping with the part size from the first article's `=ypart`/`=ybegin` lines. For a store-mode RAR set, `Layout { volumes: [{ file_id, data_start, data_len, part_size }] }`, where `data_start` comes from the volume's own header (`archive::parse`) and `data_len = vol_size - header bytes`. Per-volume data is contiguous, so inner `X` maps to volume `floor(X / data_len)` and offset `data_start + X mod data_len`, then to part `floor(offset / part_size) + 1`. For 7z copy the mapping is `32 + PackPos + X` across `.001`, `.002` and so on.
2. Merge: adjacent parts are merged, parts already fetched are subtracted (the first article is always fetched), and the rest are sorted by priority: header, then Chapters, then Tags, then the first keyframe for DV and HDR10+.
3. Budget: a per-release media budget of 12 articles. That is about 8 MB at 700 KB articles and 48 MB at the 4 MiB cap. Today's worst case is about 13 article fetches (6 front, 1 last, up to 2 for the moviehash, up to 3 small text, plus the middle existence check), so the planner replaces the front loop without adding to the ceiling.
4. Verify: the `begin == offset + 1` check in `read_part` is kept. A mismatch marks the layout as unreliable for that volume and stops planning there.
5. Loop: parse, collect Needs, fetch, reparse, up to the budget. Parsers stay total, so a short read yields the partial result they already return.

### Container and case table

Article size assumed 700 KB unless stated. "Articles" counts bodies fetched beyond the NFO and SFV.

| Container / case | First | + Targeted | + Tail | What the reads yield | Basis |
| --- | --- | --- | --- | --- | --- |
| MKV, mkvmerge, header under about 1 MB | 1 | 0 | 0 | Tracks, codecs, languages, names, flags, chapters, CodecPrivate, Info (DateUTC, SegmentUID) | [measured] b_multitrack (3 audio, 4 subs, chapters) complete at 700 KB |
| MKV, large fonts before Chapters (2.2 MB of TTF) | 1 | 1 to 3, Chapters Need from SeekHead | 0 | Chapters, attachment list (font set, cover) | [measured] chapters missing at 700 KB, present at 4 MB. Targeted read skips the 6-article sequential loop |
| MKV, mkvmerge, Tags and stats at the end | 1 | 1, Tags Need from SeekHead | 1 only if no SeekHead | IMDB, TMDB, per-track BPS, NUMBER_OF_FRAMES, NUMBER_OF_BYTES, DURATION | [measured] mkvmerge puts Tags last. [spec] SeekHead holds the Tags position |
| MKV, Lavf/ffmpeg | 1 | 0 | 0 | Everything before the first Cluster, Tags at the front, per-track DURATION rewritten in place | [measured] Lavf layout |
| MKV, DV or HDR10+ | 1 | 1 to 3, from the first video keyframe in the first Cluster | 0 | DV profile, level, CM version, MEL or FEL, RPU L1, L2, L6 (MaxCLL, MaxFALL), HDR10+ SEI presence | [est] keyframe is about 40 KB at the test rate. Real 1080p IDR is 100 to 500 KB, 4K may be near 1 MB. Streams thread owns the parsers |
| MP4, faststart | 1 | 0 to 1 | 0 | Tracks, codec config (avcC, hvcC, dvcC, dvvC, colr, mdcv, clli), chpl chapters, ilst, hdlr | [measured] faststart all tracks at 700 KB. [est] moov for 2 h with two tracks is about 1 to 2 MB (stsz is 4 B per sample) |
| MP4, moov at end (ffmpeg default) | 1 (ftyp, mdat header) | 1 to 2, moov at mdat_off + mdat_size | 0 | Same as faststart, plus exact frame count, bitrate and GOP from stsz, stts, stss | [measured] nothing at 700 KB or 4 MB, full file needed today. [spec] offset rule |
| MP4, fragmented | 1 (moov with mvex) | 1, the mfra and tfra index at the end | 1 | Duration and fragment times. Exact frame count needs a moof and trun walk | [spec]. Walk is too costly for long films, see below |
| ISO, BDMV (UHD and HD) | 1 (VRS, AVDP at 0x80000, VDS) | 2 to 4: FSD and metadata partition, index.bdmv, PLAYLIST, main candidate CLPI | 0 | id.bdmv disc id, index.bdmv version (UHD 0300), initial dynamic range, MPLS durations, STN languages and codecs, MPLS chapter marks | [measured] ImgBurn sample: index.bdmv at sector 4320 (about 8.4 MB). [spec] libbluray 1.5.0 layout. Playlist offsets [est] |
| ISO, DVD-Video (genisoimage -dvd-video -udf) | 1 | 0 to 1 for VTS_01_0.IFO if it crosses the boundary; 1 per further VTS | 0 | PAL or NTSC, audio streams (AC-3 en and fr measured), chapters, title durations (BCD) | [measured] VIDEO_TS.IFO at 567,296 and VTS_01_0.IFO at 632,832. [measured] second VTS IFO at about 21 MB |
| DVDDiscID | 1 | one read per VTS IFO, whole file | 0 | MD5 over VIDEO_TS.IFO and VTS_01_0.IFO to VTS_0N_0.IFO (N from VMG) | [measured] libdvdread 6.1.3 |
| RAR m0 store, RAR4 or RAR5, multi-volume | 1 (volume 1 header plus the start of the inner MKV) | 1 per Need, mapped by the layout | 1 (last volume, first article, for moviehash) | The inner format's table above | [spec] data start and per-volume layout. [est] not run on a real release |
| RAR m1 to m5 (compressed) | 1 (headers only: name, sizes, CRC32, method) | 0 | 0 | No inner media. A pure-Rust decoder (rars 0.12.3, young) is needed | archive thread |
| 7z copy | 1 | 1 per Need, `32 + PackPos + X` across volumes | 0 | Same as RAR m0 | [spec] data at offset 32 of `.001` (archive thread) |
| 7z LZMA2 | 1, plus compressed articles up to the decode offset | not random access | 0 | Front of the inner MKV only, sequential, budget-capped | [measured] prefix decode with lzma-rust2 (64 MiB dict) works. The test gave 786 KB of MKV from 10% of a small test archive. Not measured at real archive sizes |

### What this cannot do

- Encrypted RAR (headers or data) gives the header and nothing past it. `archive::parse` already sets `encrypted` and `headers_encrypted`.
- Compressed RAR (m1 to m5) needs a decoder. Without one, only the header probe works. Compressed 7z reads a sequential prefix, so no targeted Needs.
- Solid archives: inner offsets are not a fixed mapping, so only the first file in the solid block is reachable, and only if it is store-mode.
- MP4 fragmented frame counts: exact counts need a walk of every moof and trun. At 2 s fragments a 2 h film has about 3,600 moofs, far beyond 12 articles. Use mfra for duration and accept an estimate for frames.
- BD index beyond budget: a playlist or CLPI that sits past the budget is skipped, so main-title selection falls back to the longest candidate seen. The longest-title rule needs every MPLS, which is cheap only when the PLAYLIST directory is contiguous. [est]
- DV MEL against FEL needs RPUs from the first keyframe, which costs 1 to 3 articles. It is out of reach for a release whose first keyframe is past the budget.
- Volume layouts that break the constant per-volume payload (a comment, ENDARC, or recovery records in volume 1 or the last volume) make inner mapping wrong. The `begin == offset + 1` check catches it per article, but only after the fetch.
- Articles with wrong or missing yEnc `begin` (a few posters) break mapping for that part, so that Need is dropped.
- Tags and MP4 `ilst` that a muxer writes after the data are reached only through the tail, so a release missing a tail article loses them.

### Recommended order

1. Replace the front loop with Needs: SeekHead Tags and Chapters for MKV, mdat offset for MP4, and keep `read_part`'s check. This is the biggest gain (non-faststart MP4 and font-heavy MKV).
2. Add the store-mode layout (RAR m0 and 7z copy) on top of the existing `moviehash_of` mapping, so one function serves both the hash and the probe.
3. Add the UDF and BDMV parser with the same Need type. Keep DVD IFO on the first article plus one targeted read.
4. DV and HDR10+ Needs from the first keyframe, after the streams thread lands its parsers.

## Note B: Storage and exposure

### What Atlas stores today

- `release_probe` (per shard, `probe/mod.rs` PROBE): `release_id` (PK), `crc32`, `agent`, `info` (TEXT, JSON of `ProbeInfo`), `moviehash` (added column). Indexes on `crc32`, `agent` and `moviehash`, all partial.
- `info` JSON holds `yenc`, `file_crc32`, segment and overhead checks, `health`, kept `headers` (`KEPT_HEADERS`), `crossposted_to`, `archive` (`ArchiveHead`), `media` (`MediaInfo`), `nfo` (up to 256 KB), `imdb_ids` (up to 16), `sfv`, `moviehash`. It is read whole, never queried.
- `MediaInfo` covers container, muxing and writing app, title, duration in seconds, video (codec, size, frame rate, bit depth, `hdr`, primaries, transfer, DV profile, level, EL flag, language), audio, subtitles, and chapters. Chapters are from the first edition only (the w5 bug).
- Search: `field_search.rs` builds `r.id in (select release_id from s{s}.release_probe where {column} = {param})` for `@probe:` conditions. That only compares probe columns. Probe JSON is never searched.
- Newznab attrs: `api.rs::release_attrs` adds `poster`, `group`, `resolution`, `video`, `audio`, all parsed from the release name (`release_tags::parse`). `media/query.rs::MediaAttrs::xml` adds IDs only (`season`, `episode`, `year`, `imdb`, `imdbid`, `tvdbid`, `tmdbid`, and so on), from `media_release`. The probe's media never reaches a newznab attr.
- Not in the repo: `video_data`, `audio_data`, `release_subtitles`, `release_chapters`, `release_unique`, and any Manticore field (grep of `src` found none). The reference doc's storage table assumes mediainfo tables that Atlas does not have. Those rows below are new tables, not extensions.

### Proposed mapping

Use the `added:` mechanism that `release_probe` already has for new columns. Use a `media_version` column so a parser change triggers a rerun through `config_stamp`.

| Data | Stored in | Newznab attr | Searchable token |
| --- | --- | --- | --- |
| Container, muxing app, writing app | `release_probe.info` (exists) | None | No |
| Segment title | `release_probe.info` (exists) | None | No (free text, noisy) |
| Duration in ms (Info Duration x TimestampScale; MP4 mvhd) | New `release_media.duration_ms` (int) | Custom `runtime` (minutes) | `minruntime`, `maxruntime` parameters |
| Width, height, display size | New `release_media.width`, `height` | `resolution` (standard, used only when the name has none) | `2160p` token, only when the name lacks it |
| Video codec, bit depth, frame rate | `release_probe.info` (exists) plus `release_media.bit_depth` (new; fixed to read hvcC, avcC, av1C, vpcC, not Colour) | `video` (standard, augmented) | `hevc`, `10bit` |
| Primaries, matrix, transfer | `release_media` new columns | None | No |
| HDR label (`hdr_label`: DV record, then HDR10+, HDR10, PQ, HLG, SDR) | New `release_media.hdr_label` (text) | Custom `hdr` | `hdr10`, `hdr10plus`, `dv`, `hlg` |
| MaxCLL, MaxFALL (Colour 0x55BC, 0x55BD; RPU L6; SEI 144) | New `release_media.max_cll`, `max_fall` (int) | Custom `maxcll` (no search) | No |
| Mastering display (G, B, R, WP, L) | `release_probe.info` (JSON only) | None | No |
| DV profile, level, EL type, CM version | New `release_media.dv_profile`, `dv_level`, `dv_el` (none, mel, fel), `dv_cm` | Custom `dvprofile`, `dvel` | `dvp7`, `dvp8`, `mel`, `fel` |
| Audio tracks: codec, channels, rate, language, default, forced, commentary, name | New `release_track` (release_id, track_no, kind, codec, lang, channels, rate, default, forced, commentary, name) | `audio`, `language` (standard, comma list) | `atmos`, `truehd`, `dtsx`, language codes |
| Atmos (name, then JOC or EMDF payload) | `release_track.atmos` (bool) | None (part of `audio`) | `atmos` |
| Subtitle tracks: codec, language, forced, SDH, default, name | `release_track` with `kind = sub` | `subs` (standard, comma list) | `sublang` parameter, `subs-eng` |
| ASS style block hash (same-subs key) | `release_track.codec_private_hash` (new) | None | No |
| Per-track bitrate, frames, bytes, duration (mkvmerge stats; MP4 stsz, stts) | `release_track` new columns `bitrate_bps`, `frames`, `bytes`, `duration_ms` | None | No |
| Chapters: edition, index, start, name, skip type, hidden flag | New `release_chapter` (release_id, edition, idx, start_ms, name, skip_type, hidden, default_edition) | Custom `chapters` (count) | No (names too noisy) |
| Chapter timing hash (same-disc key) | `release_probe_key` (new; kind `chapter_hash`) | None | No |
| Edition count, ordered and default flags | `release_media.edition_count` (int) | None | No |
| Linked editions (ChapterSegmentUUID) | `release_chapter` (`segment_uuid` text) | None | No |
| IMDB, TMDB, TVDB, ISRC from Tags | Evidence row in `release_probe_key` (kind, value, source `mkv_tag`, confidence); never overwrites the name | Existing `imdb`, `imdbid`, `tmdbid` (from media IDs) | `tt1234567` (IMDB) |
| Tags TMDB prefix (`movie/`, `tv/`) | Parsed into kind and id in the evidence row | `tmdbid` (movies only) | No |
| Attachments: font-set hash (sorted SHA-256 of font files), cover hash | `release_media.font_set`, `cover_hash` (text) | None | No |
| SegmentUID, DateUTC, TrackUIDs, muxing app | New `release_mux` (release_id, segment_uid, date_utc, muxing_app, writing_app) | None (internal) | No |
| Alt key (codec config hash, first frame payloads) | `release_probe_key` (kind `alt_key`, indexed) | None (internal, used to link remuxes) | No |
| Disc ids: id.bdmv disc id, DVDDiscID (MD5), UDF volume set id | `release_probe_key` (kinds `bd_disc_id`, `dvd_disc_id`, `udf_vsid`) | None | No |
| Muxer fingerprint (hdlr name, compressorname, encoder SEI string, x264 and x265 family) | `release_probe.info` (JSON); family as token | None | `x265`, `x264` (family only) |
| Privacy: iTunes apID, purd, ownr, PURCHASE_OWNER, Aegisub local video path | Not stored raw. Salted SHA-256 in `release_probe_key` (kind `private`), or dropped. The Aegisub garbage section is removed before the subs hash | None | No |
| Probe state: parser version, bytes fetched, article count, error | `release_probe.media_version` (new), plus the existing `info` | None | No |

### API exposure

- Fill the standard attrs (`video`, `resolution`, `audio`, `language`, `subs`) when `extended=1`. Keep custom attrs out of the non-extended response, so older clients see no change.
- Search: `hdr`, `dvprofile`, `dvel`, `audiolang`, `sublang`, `atmos`, `minruntime`, `maxruntime` as custom parameters on `t=search` and `t=movie`, listed in `caps`. This reuses the `@probe:` path in `field_search.rs` for typed columns, and needs no Manticore field until one is confirmed in `src`.
- Evidence path: an identification from a probe (Tags IMDB or TMDB, commentary names, OpenSubtitles hash) goes through the same evidence rows as the other sources, with source and confidence. It is never written over the release name.
- Keep the bulk of the probe JSON out of search results. Only the typed columns above are exposed.

### What this cannot do

- Nothing from the probe reaches newznab attrs today, and the typed tables above are new work. Until they exist, `extended=1` can only add what the name already gives.
- Tags written only at the tail (mkvmerge) need Note A's Tags Need. Without it, IMDB and stats are lost for those releases.
- Alt key and disc IDs match only the same encode or the same disc. Audio-only keys match the same film at a different encode (w6b), so they are not used alone.
- Stored `DateUTC` and `_STATISTICS_WRITING_DATE_UTC` change per mux (w6b), so they are kept for display and never used as a key.
- Derived frame counts and bitrates are exact only for non-fragmented MP4 and for mkvmerge stats. Fragmented frame counts are estimates.
- Privacy fields are hashed or dropped. A hash can still be compared with another hash from the same install, so it does not reveal the value but does link records.
- The `MediaInfo.chapters` bug (first edition only) must be fixed before `release_chapter` is filled, or edition 1 data gets stored as if it were the default.
