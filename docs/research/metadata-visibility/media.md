# Atlas map: media (containers, codecs, HDR/DV, title metadata, NFO)

Repo: /home/claude/appz4fun/atlas, branch main at 55d2e51 ("Follow archives inside archives, up to 10 layers (#52)"). Read only; nothing built or run. All paths below are relative to the repo root; `file:line` cites main unless a PR branch is named.

Sibling notes in this folder: probe.md (the probe step in general), web-ui.md, storage.md, archive-payload.md, coordination.md. This file covers the media catalog blocks (`media`, `media_additions`) and the `nfo` block, the two "media" subsystems, and PR #48 / #51 / #54 / the stream-details branch as far as they are in the tree or described.

## 0. Two different things are called "media"

1. **Container probe** (`src/probe/media.rs`, run by `ProbeStep` in `src/probe/step.rs`): parses the first bytes of the release's main file (Matroska/WebM/MP4) into `MediaInfo` and stores it as the `media` key of the `release_probe.info` JSON. Rendered by `probe_html` in `src/web.rs` as the "Tracks" and "Chapters" panels.
2. **Title resolution** (`src/media/*.rs`): parses the release NAME into a title (TV or movie), resolves it to TVDB/TVmaze/Trakt/TMDB/IMDb ids through providers, stores per-release ids in the `media_release` shard table, caches per-title answers in `atlas.media.db`, and fetches TMDB/TVDB/OMDb/Trakt details, posters and backdrops into `atlas.details.db` + a posters folder. Rendered by `media_label`, `id_links`, `details_html` (via the htmx-loaded `/ui/details` panel) and `scores_html`.
3. **NFO**: the probe stores the NFO text, IMDb ids and a password; `MediaNfo` reads an NFO again for a movie's IMDb id; `naming::NfoName` (src/nfo.rs) takes a release name from it.

## 1. Files read (with line counts)

- src/probe/media.rs (748), src/probe/mod.rs (264), src/probe/step.rs (437), src/probe/opensubtitles.rs (parts: 1-80, 225-300), src/probe/nest.rs (Layer struct 25-47), src/probe/archive.rs (ArchiveHead 12-35), src/probe/yenc.rs (YencLines 7-18)
- src/media/mod.rs (317), details.rs (797), resolve.rs (215), query.rs (534), store.rs (143), steps.rs (369), title.rs (90), tmdb.rs (133), fanout.rs (104), parse.rs (1-120), score.rs (1-60), tvdb.rs (1-60), tvmaze.rs (1-40), trakt.rs (1-40), http.rs (1-60)
- src/web.rs: 40-80 (routes), 495-580 (results extras), 684-800 (row, media_label, name_tags), 985-1230 (release_page, release_html), 1231-1480 (probe_html, copy_card), 1481-1700 (copies, duplicates, details_panel, scores_html), 1700-1860 (details_html, poster, id_links, nested_chain)
- src/postprocess/steps.rs (53), schema.rs (160), step.rs (262), mod.rs (162), reprocess.rs (193), engine.rs (280-312, 405-475, 1070-1114)
- src/api.rs 280-312 (release_attrs), 153-170; src/release_tags.rs (139); src/nfo.rs (1-80); src/release_info.rs (1-60, 130-215, 510-565); src/archive/store.rs (1-75, 140-200); src/archive/mod.rs (44-95 identify); src/archive/steps.rs (37-50); src/duplicates.rs (21-43); src/search.rs (9-18); src/config.rs (206-245, 546-660 grep); src/bg_indexer.rs:1172; config.example.toml 60-115
- tests/web_ui.rs (grep), tests/probe_bg.rs (grep), tests/probe_moviehash.rs (grep); docs/TODO.md (grep), docs/todo-duplicate-clusters.md (1-160)
- Branches: remotes/pr/48 (diff vs main: src/probe/media.rs, mod.rs, step.rs, web.rs, release_info.rs, schema.rs, details.rs, assets/web.css, tests/web_ui.rs), pr/50, pr/51 (stat, src/video/mkv.rs head, src/probe/stills.rs grep, probe diff), pr/54 (stat), pr/55, pr/57, pr/58 (probe diff)
- Catalog: catalog-current.md lines 708-730 (nfo), 936-1141 (media, media_additions), 1143-1230 (ranked plan, storage table, open questions)

## 2. Pipeline mechanics (how media data gets made and remade)

### 2.1 Step registry
`src/postprocess/steps.rs:10-26` lists every step: Par2Step, ListStep, `probe::ProbeStep`, `opensubtitles::OpenSubtitlesStep`, Naming, NfoName, PredbMatch, CategoryStep, then the three media steps (`parse`, `lookup`, `nfo` from `crate::media::steps()`, src/media/mod.rs:92-99), then `OpenSubtitlesImdb`. Ids/ranks live in ranges per part (steps.rs:36-38): probe is StepId(4) rank 120 (step.rs:19, 80-82); OpenSubtitles StepId(5); media_parse StepId(32) rank 500 (media/steps.rs:160-168), media_lookup StepId(33) rank 510 (251-259), media_nfo StepId(34) rank 520 (310-318), opensubtitles_imdb StepId(35) rank 505 (opensubtitles.rs:22, 242-250). PR #51/#54 add `stills::StillsStep` StepId(6) (pr/51 src/probe/stills.rs:17) — the next free id in the probe range 1..=7 after that is 7.

### 2.2 The probe run (step.rs:102-212)
- Needs::Bodies (step.rs:83-85); enabled = `cfg.archive.enabled` (86-89); applies unless the name ends `.jpg` (90-92); reruns on growth, not on rename (93-95); `config_stamp() = 1 | 2` (96-101): bit 1 = chapters, commentary, multi-article headers, moviehash; bit 2 = nested archives (#52).
- Order: NFO/SFV/TXT files first (up to 3, first article each; step.rs:109-130): `nfo_text` (CP437 fallback, mod.rs:151-156), `imdb_ids` (≤16, mod.rs:163-185), `find_password` → `SET_NFO_PASSWORD` write (122-127), NFO kept to `MAX_NFO = 256 * 1024` chars (22, 128), SFV parsed (119).
- Main file = most parts, first volume by name (40-53). First article fetched RAW with `ARCHIVE::ARTICLE_CAP` 4 MiB cap (55-62; src/archive/mod.rs:28): headers kept (`keep_headers`, mod.rs:80-104), Xref groups, yEnc lines, decode, `part_crc_ok`, then `inspect(&front, &mut info)` (157).
- `inspect` (397-402): `info.archive = archive::parse(body)`; `nest::unwrap(body)` follows archive layers; **`info.media = media::parse(&nest.inner)`** (400) — so the media parser sees the innermost file's bytes (a stored RAR's inner MKV, etc.).
- Middle article STAT (161-169), last article fetched and decoded (170-181), `derive` (182), moviehash (184, 294-362), then `read_more_front` (188, 223-238): while `info.media.header_complete == false` and part ≤ min(total, `FRONT_ARTICLES = 6`) (217, 231), read the next part (`read_part` 241-250 checks yEnc begin == offset+1), append to `front`, re-run `inspect`. Only forward reads: parts 2..6. No tail read for a trailing `moov` or trailing Matroska Tags.
- Write: `SET_PROBE` = `insert or replace into release_probe (release_id, crc32, agent, info, moviehash)` (mod.rs:49-50; step.rs:200-207). Whole JSON replaced each run. Skipped entirely when nothing was found (`empty`, 196-199). A moviehash triggers a rerun of the OpenSubtitles step (209).

### 2.3 Budgets
`[archive] max_bodies_per_release = 4, probe_files = 2, probes_per_minute = 60` (src/config.rs:546-559, 578-579; config.example.toml:66-70); `[postprocess] body_fetches_per_minute = 600, backlog_per_second = 2000` (config.rs:605-606, 628-629). `[media] details_per_minute = 120` (config.rs:212, 236; clamp 0..600 at 271-272). Provider rates: tvmaze 90/min, tmdb 240, omdb 30, trakt 120, tvdb 120, opensubtitles 60 (config.rs:237-242).

### 2.4 Backfill mechanisms that already exist
1. **config_stamp re-walk** (the "#36 config_stamp pattern"): `Inner::seed` (engine.rs:286-306) sends `seed_shard` to every shard writer; `seed_shard` (1073-1114) compares each enabled step's stamp with `meta.pp_stamp_<id>`; when the stored stamp lacks a bit the step now has (`stamp & !s != 0`, 1106) it inserts a `pp_walks` row kind `steps` with `redo_mask = bit` (1081-1088, 1107). `walk_tick` (412-475) walks `releases` newest-first (`r.id < before_id ... order by r.id desc`, 435-439) at `backlog_per_second / SHARDS` per shard (421), visiting every row when `redo != 0` (465) and re-running the masked step. Bumping `ProbeStep::config_stamp` with a NEW bit is the designed way to re-probe every existing release. **The next free bit is 4** (main uses 1|2; PR #48 also uses bit 2, see §7.1).
2. **`atlas --reprocess --step probe --apply`** (src/postprocess/reprocess.rs:25-40, 74-99, 137-162): queues a `reprocess` walk with the step's mask, per shard or per group; dry run prints the count and the time at `backlog_per_second` (110-135).
3. **`Stage::rerun(release_id, mask)`** (postprocess/mod.rs:117-123, `rerun_on` 63-78) for one release.
4. **Growth reruns**: `Reruns { on_growth: true }` (step.rs:94) re-probes when a release gains articles.
5. **Details backfill thread** (details.rs:673-686 `start_backfill`, started at src/bg_indexer.rs:1172): every `BACKFILL_REST` 6 h (39) it walks `known_titles()` (598-624: newest `max(release_id)` per (kind, tmdb, imdb, tvdb, trakt) across shards, `MAX_TITLES_PER_SHARD` 100,000) and fetches details + poster at `details_per_minute` (640-663). This is a TITLE backfill, not a release backfill.
6. **Fan-out** (fanout.rs:20-24 UPDATE, 41-70 pass): when a title's ids change, every `media_release` row of that `title_key` in every shard gets them, in 1,000-row chunks on the writer.

## 3. Container parser: exactly what `src/probe/media.rs` reads

### 3.1 Output structs
- `MediaInfo` (8-24): `container`, `muxing_app`, `writing_app`, `title`, `duration_secs`, `video: Vec<VideoTrack>`, `audio: Vec<AudioTrack>`, `subtitles: Vec<SubtitleTrack>`, `chapters: Vec<Chapter>`, `header_complete` (`#[serde(skip)]`, 22-23). `#[serde(default)]` on every struct (7, 27, 34, 54, 68) so old JSON reads with new fields missing (test `old_probe_json_still_reads` 694-702).
- `VideoTrack` (33-51): `codec, width, height, display_width, display_height, frame_rate, bit_depth, hdr ("HDR10"|"HLG"|"PQ"), primaries, transfer, dv_profile, dv_level, dv_enhancement_layer, language`. No `name`.
- `AudioTrack` (53-65): `codec, channels, sample_rate, language, name, atmos (from the name), commentary`.
- `SubtitleTrack` (67-76): `codec, language, name, forced, hearing_impaired, commentary`.
- `Chapter` (26-31): `start_secs, name`.
- Caps: `MAX_CHAPTERS 256` (321), `MAX_TRACKS 128` (363), `MAX_TRACK_TEXT 256` chars (365, `text()` 157-160).

### 3.2 Matroska element ids handled
- Entry: `is_matroska` EBML magic (78-82); `matroska()` (266-282): EBML header `0x1A45DFA3` → DocType `0x4282` into `container` (270-275); Segment `0x18538067` → `segment()`.
- `segment()` (284-318): Info `0x1549A966`: `0x4D80` MuxingApp, `0x7BA9` Title (empty filtered), `0x5741` WritingApp, `0x2AD7B1` TimestampScale (default 1,000,000), `0x4489` Duration → `duration_secs = duration * scale / 1e9` (287-301). Tracks `0x1654AE6B` → each `0xAE` TrackEntry → `add_track(track_entry(v))` (302-307). Chapters `0x1043A770` only while `chapters` empty (308). **Cluster `0x1F43B675` sets `header_complete = true` and stops the walk** (310-313). Everything else (`_ => {}`) is skipped: SeekHead, Void, Cues, Tags `0x1254C367`, Attachments `0x1941A469`.
- `track_entry()` (200-264): `0x83` TrackType, `0x86` CodecID, `0x22B59C` Language (first wins), `0x22B59D` LanguageBCP47 (overrides), `0x536E` Name, `0x55AA` FlagForced, `0x55AB` FlagHearingImpaired, `0x55AF` FlagCommentary, `0x23E383` DefaultDuration; Video `0xE0`: `0xB0` PixelWidth, `0xBA` PixelHeight, `0x54B0` DisplayWidth, `0x54BA` DisplayHeight, Colour `0x55B0`: `0x55B2` BitsPerChannel, `0x55BA` TransferCharacteristics, `0x55BB` Primaries, `0x55D0` MasteringMetadata (presence only → `mastering = true`, values not read); Audio `0xE1`: `0xB5` SamplingFrequency, `0x9F` Channels; BlockAdditionMapping `0x41E4`: `0x41E7` type, `0x41ED` extra; when type is fourcc `dvcC`/`dvvC` → `dv_record` (186-189: profile = byte2>>1, level = (byte2&1)<<5 | byte3>>3, EL = byte3 & 0b10).
- `hdr_name` (191-198): transfer 16 + mastering → "HDR10"; 16 alone → "PQ"; 18 → "HLG"; else None. (Catalog's mislabel note is confirmed: HDR10 without MasteringMetadata shows as "PQ".)
- `add_track` (371-412): kind 1 video, 2 audio, 17 subtitle; `frame_rate = 1e9 / default_duration` (383); atmos = name contains "atmos" (393); commentary = flag or name (394, 403); SDH = flag or name contains "sdh" (404-405). Video track Name is parsed into `Track.name` (209) but `VideoTrack` has no field for it → dropped.
- `chapters()` (324-356): first `0x45B9` EditionEntry that yields any chapter wins, regardless of edition flags; per `0xB6` ChapterAtom: `0x91` TimeStart, `0x98` FlagHidden (hidden atoms skipped, 347), first `0x80` ChapterDisplay's `0x85` ChapString (337-342). Not read: ChapterUID, TimeEnd, ChapLanguage/Country, nested atoms, ChapProcess, EditionUID/Flags, ChapterSegmentUUID.
- **Not read anywhere in media.rs (grep by id)**: SegmentUID `0x73A4`, SegmentFamily `0x4444`, Prev/NextUID, DateUTC `0x4461`, TrackNumber `0xD7`, TrackUID `0x73C5`, FlagEnabled `0xB9`, FlagDefault `0x88`, FlagOriginal `0x55AE`, FlagVisualImpaired, CodecName, CodecPrivate `0x63A2`, CodecDelay, SeekPreRoll, ContentEncodings, PixelCrop*, DisplayUnit, AspectRatioType, FlagInterlaced, FieldOrder, StereoMode, AlphaMode, MatrixCoefficients, ChromaSubsampling, CbSubsampling, ChromaSiting, Range, MaxCLL `0x55BC`, MaxFALL `0x55BD`, MasteringMetadata children, Projection, Audio BitDepth `0x6264`, OutputSamplingFrequency, Emphasis, Tags, Attachments, Cues, SeekHead order, Void sizes.

### 3.3 MP4
- `is_mp4`: `ftyp` at bytes 4..8 (84-86); brands not kept. `boxes()` (416-436) handles 32-bit, 64-bit (`size == 1`) and to-end (`size == 0`) sizes.
- `mp4()` (438-447): only `moov` is descended; `header_complete = has_whole_moov(data)` (445, 450-470: a top-level `moov` whose end ≤ data length). `moov` after `mdat` (ffmpeg default without `+faststart`) never becomes complete from the front, so `read_more_front` spends up to 5 more articles on `mdat` and still has nothing.
- `moov()` (480-497): `mvhd` v0/v1 timescale + duration → `duration_secs` (482-493); `trak` → `trak()`. Not read: creation/modification time, rate, volume, next track id, `udta`, `meta`/`ilst`, `iods`.
- `trak()` (505-571): `mdia.hdlr` handler type at bytes 8..12 (513-517); `mdia.mdhd` language (v0 at 20, v1 at 32) decoded by `mp4_language` (499-503, "und" dropped); `minf.stbl.stsd` first sample entry (522-530); codec = fourcc at entry[4..8] (536-537); `vide`: width/height be16 at 32/34 (542-543), child boxes from offset 86: only `dvcC`/`dvvC` (547-556); `soun`: channels be16 at 24, sample rate be32 at 32 >> 16 (559-565); `subt`/`text`/`sbtl` → subtitle with codec + language only (566-568). Not read: `tkhd`, `edts/elst`, `hdlr` name, `avcC/hvcC/av1C/vpcC/esds/dac3/dec3`, `colr/pasp/clap/fiel/btrt`, `stts/stsz/stss`, `tref/chap`, `chpl`, `udta/meta/ilst/©too/covr`, `uuid`, `free/skip`, `moof/sidx`, `pssh`.

### 3.4 Everything else
`media::parse` returns `None` for anything that is not EBML or `ftyp` (88-96). `archive::identify` (src/archive/mod.rs:51-88) only sniffs magics: JPEG/PNG/GIF/WebP/BMP/TIFF → `Format::Image` (66-75); EBML, `ftyp`, RIFF/AVI, MPEG-PS `00 00 01 BA`, `fLaC`, `OggS`, `ID3` → `Format::Media` (76-84). Nothing past the magic is read for AVI/ASF/TS/PS/Ogg/FLV/RM/MP3/FLAC/WAV/APE/images/documents/subtitle sidecars. Subtitle sidecars (.srt/.ass/.idx) are small text files the probe's `is_small_text` does not include (step.rs:33-36 only .nfo/.sfv/.txt).

## 4. Data model (tables, columns, JSON shapes, writers, readers)

| Table / store | Columns | Written by | Read by |
|---|---|---|---|
| `release_probe` (shard; src/probe/mod.rs:23-47) | `release_id PK, crc32, agent, info TEXT (JSON), moviehash (added)`; indexes on crc32, agent, moviehash | `ProbeStep` `SET_PROBE` insert-or-replace (mod.rs:49-50, step.rs:204-207) | `probe::read` (mod.rs:142-148) from `release_html` (web.rs:1163-1170); `OpenSubtitlesStep` (moviehash fact); PR #51 stills (`select info from release_probe`, pr/51 stills.rs:298); tests. **Not read by src/api.rs** (grep "probe": none). |
| `release_probe.info` JSON = `ProbeInfo` (mod.rs:53-78) | `file, yenc{name,file_size,line_len,part,total,begin,end,yend_size,pcrc32,crc32}` (yenc.rs:7-18), `file_crc32, segment_size, overhead_percent, continuity_ok, part_size_ok, part_crc_ok, health{first,middle,last}, headers{12 KEPT_HEADERS ≤512 chars}, crossposted_to[], archive{format,encrypted,headers_encrypted,method,name,unpacked,packed,crc32}` (archive.rs:12-35, offsets serde-skipped), `layers[]{format,method,name,unpacked,offset,stored,encrypted,stop,capped}` (nest.rs:25-47), **`media{MediaInfo}`** (§3.1), `nfo` (≤256 Ki chars), `imdb_ids[]` (≤16), `sfv{name:crc}`, `moviehash` | same | same |
| `release_nfo_password` (src/archive/store.rs:54-63) | `release_id PK, password TEXT` | probe step (step.rs:122-127) and naming NFO step via `SET_NFO_PASSWORD` (store.rs:65) | `nfo_password` (store.rs:161-169) → `release_info::password_hint_with` (release_info.rs:539-543) → `password_fact` (web.rs:1741-1749) |
| `release_opensubtitles` (src/probe/opensubtitles.rs:27-48) | `release_id PK, moviehash, imdb_id, tmdb_id, kind, title, year, season, episode, release_name, checked_at` | `OpenSubtitlesStep` `SET_MATCH` (50-52) | `opensubtitles::read` (285-) → `probe_html` "OpenSubtitles match" (web.rs:1401-1414); `OpenSubtitlesImdb` (260-275) copies a movie's imdb into `media_release` (`SET_IMDB`, 55-56) |
| `media_release` (shard; src/media/steps.rs:25-77) | `release_id PK, kind, title_key, season, episode, episode_last, air_date, year, quality, tvdb_id, tvmaze_id, trakt_id, tmdb_id, imdb_id, tvrage_id, imdb_from_nfo`; 7 indexes (title, one per id) | `MediaParse` UPSERT with late params (80-92, 194-216), `MediaLookup` FOUND (95-97, 232-246), `MediaNfo` NFO_IMDB (98-99, 352), `OpenSubtitlesImdb` SET_IMDB, `fanout::UPDATE` (fanout.rs:20-24) | `query::attrs_with` (query.rs:331-370) → `MediaAttrs{kind,season,episode,air_date,year,ids}` for web (release_page 1002, copy_card 1465, results 570, details_panel 1659, sort 501) and API (`MediaAttrs::xml` 378-428); id searches `gather` (217-243); `details::known_titles` (598-624) |
| `atlas.media.db` `media_titles` (src/media/store.rs:9-25) | `title_key PK, kind, title, year, state, checked_mask, tvdb_id, tvmaze_id, trakt_id, tmdb_id, imdb_id, tvrage_id, found_title, checked_at, next_check, misses, fanout_done` | `Media::resolve_until` → `store::put` (resolve.rs:185, store.rs:94-120) | `Media::open` loads all into the `known` map (mod.rs:163-185); `cached_ids` (242-248); fan-out |
| `atlas.details.db` `details` (src/media/details.rs:43-64, `backdrop` added at 105-109) | `kind, tmdb_id (negative = TVDB id), title, overview, genres, runtime, status, network, cast_names, released, bluray, poster, backdrop, tmdb_score, imdb_score, rotten, metacritic, trakt_score, fetched_at`; `details_lookups(lookup, tmdb_id, checked_at)` (65-69) | `for_title` on page open (441-483) and `backfill_pass` (640-663); `lookup` (511-530) | `read` (113-144), `kept_for` (486-504) for results/copies; `poster`/`backdrop` (551-560) |
| posters folder `app_dir/posters` (details.rs:532-543) | JPEG bytes by TMDB path or sanitized URL; `MAX_POSTER_CACHE` 4 GiB (35), trimmed every 256 writes (37, 584-590, 688-707) | `image()` (562-592) | `/ui/poster` (web.rs:1717-1739) |
| `releases` (naming columns) | `name, display_name, is_obfuscated, name_source` (release_info.rs:136-150); `RELEASE_COLUMNS` appends `name_source` (schema.rs:149) | naming steps | `release_info::names_with`; `name_source_label` (release_info.rs:24-35) |
| `release_fingerprint` (src/duplicates.rs:21-40) | `release_id, posted BLOB, content BLOB, par2_block, par2_blocks, posted_names` | archive/PAR2 steps | copies panel; `content` is sizes only (docs/todo-duplicate-clusters.md "Content fingerprint") |
| PR #48 `release_cover` (pr/48 src/probe/mod.rs diff) | `release_id PK, mime, sha256`; bytes in `app_dir/covers/<sha256>`, 1 GiB cap, trimmed every 64 | probe step `SET_COVER`/`DROP_COVER` | `probe::cover`, `has_cover` → `/ui/cover` and "Cover art" panel |
| PR #51/#54 `release_stills` (pr/51 src/probe/stills.rs:29-41) | `release_id, position, at_ms, sha256, width, height` PK(release_id, position); JPEGs in `app_dir/stills/`, 2 GiB cap | `StillsStep` (StepId 6) | `stills::stills`/`still` helpers; **no web route in the PR** |

Schema evolution rules: `ShardTable.added` = append-only nullable columns applied on open by `schema::migrate` → `add_missing` (schema.rs:69-70, 155-184); new tables are registered in `SHARD_TABLES` (131-145) and get a `gc_<table>` delete trigger (175-181). JSON in `info` needs no migration (serde default).

## 5. UI surface (what renders what)

### 5.1 Release page `GET /ui/release?id=` (web.rs:60) → `release_page` (989-1017) → `release_html` (1028-1201)
- Header tags (1037-1045, 1054): `media_label` (735-742: `S01E02` / `S01` / year from `MediaAttrs`), password tag, COMPLETE/INCOMPLETE, OBFUSCATED, `name_tags` (720-732: resolution, each name HDR token, quality — all from the NAME via `release_tags::parse`).
- Facts `<dl>` (1047-1142): Category, Title, Size, Posted, Group(s), Poster, Email, Posted as, Name from (`name_source_label`, 1091-1093), Year, Ripped from, Site, **Video = name codec + bit depth + container (1104-1113)**, **Audio = name audio + channels (1114)**, Languages (name), Edition, Release group, Parts, Encryption, Password (`password_fact` 1126-1128), Files, PAR2 files, Articles, Repair, **Links = `id_links(kind, ids)`** (1139-1142; 1790-1811: IMDb, TMDB movie/tv, TheTVDB, TVmaze, Trakt).
- `<div id="title-details" hx-get="/ui/details?id=" hx-trigger="load">` (1190) → `details_panel` (1657-1663) → `media::details::for_title` → `details_html` (1679-1715): backdrop `<img>` (1702-1709), poster `<img>` (1694-1701), title, `scores_html` (1665-1677: IMDB, RT, MC, TMDB, TRAKT tags), overview, Released, Blu-ray, Genres, Runtime (min), Network, Status, Cast.
- `{copies}` = `copies_html` (1555-1572) → `copy_card` (1463-1548) per twin/alt: name tags, `scores_html`, facts incl. Video/Audio/Languages from the name and Links (`id_links`, 1534-1536), poster thumb.
- `{probe}` = `probe_html(&found, matched)` (1163-1170, 1243-1453): panels in this order (1445-1452): **Tracks**, **Chapters**, yEnc, Packing, How it was posted, then `NFO` (`<h2>NFO</h2>` + IMDb links + `<pre class="nfo">`, 1425-1432) and `SFV` table (1433-1444).
  - Tracks (1330-1396): `Container` (1332), `Written by` = writing_app (1333), `Muxed by` = muxing_app (1334), `Title` (1335), `Duration` h:mm:ss (1336-1343); one `Video` line per track (1344-1364): codec, `WxH`, `fps` (3 decimals), `N-bit`, hdr label, `Dolby Vision profile N[, enhancement layer]`; one `Audio` line per track (1365-1379): codec, `N channels`, `Hz`, language, `Atmos`, `commentary`, name; one `Subtitles` line per track (1380-1395): codec, language, `forced`, `SDH`, `commentary`, name. Then `OpenSubtitles hash` (1398-1400) and `OpenSubtitles match` (1401-1414: title, SxxEyy, year, IMDb link, release name).
  - Chapters (1415-1423): `h:mm:ss` → name or `Chapter n`.
  - **Stored in `info.media` but not rendered on main**: `video[].display_width`, `display_height`, `primaries`, `transfer` (the hdr label is derived from transfer), `dv_level`, `video[].language`. Audio and subtitle structs are fully rendered.
- `{files}` = "Files inside" from `archive_inner_files` (1144-1161).

### 5.2 Results list `GET /ui/results` (web.rs:58, 845) → `row` (744-797)
Per row: `media_label` tag (755-757), `name_tags` (758), `best_score` (712-717, one of IMDB/TMDB/TRAKT from `Details`), poster thumbnail when `poster_kept` (763-768), TWIN/ALT counts, GROUPS, password, INCOMPLETE. `RowExtras` (684-689) is filled from `attrs_of` + `kept_for` (567-575) — never fetches. Sorting by rating uses only kept details (495-508).

### 5.3 Other routes
`/ui/details`, `/ui/poster` (web.rs:62-63), `/ui/duplicates` (61, 1575-1654). PR #48 adds `/ui/cover`. All behind `signed_in`.

### 5.4 API (src/api.rs)
`release_attrs` (285-310) emits `poster, group, resolution, video, audio, hdr, source, language, team` — **all from the NAME via `release_tags::parse`** (286) — plus `twins`, `alts`; `rss` adds `category` and `password` (332-337); `MediaAttrs::xml` (query.rs:378-428) adds `season, episode, tvairdate, year, tvdbid, tvmazeid, traktid, tmdbid, rageid/tvrageid, imdb/imdbid` when the matching caps parameter is on (`id_params`, query.rs:61-81). No `t=details`/`t=getnfo`; nothing from the probe's container data.

## 6. Catalog coverage: media, media_additions, nfo

Legend: shown = on the release page (or a panel it loads); stored_not_shown = in SQLite/JSON, no UI; parsed_not_stored = computed and dropped; not_collected; unclear.

### matroska
| Item | Status | Evidence |
|---|---|---|
| EBML_header.DocType | shown | media.rs:270-275 → `container`; web.rs:1332 "Container" |
| EBML_header DocTypeVersion/ReadVersion/MaxIDLength/MaxSizeLength | not_collected | only `0x4282` handled in the `0x1A45DFA3` walk (media.rs:270-275) |
| Segment.Info Title, Duration, MuxingApp, WritingApp | shown | media.rs:291-300; web.rs:1333-1343 |
| Segment.Info TimestampScale | parsed_not_stored | media.rs:294, folded into `duration_secs` (300) |
| Segment.Info SegmentUID, SegmentFamily, Prev/NextUID | not_collected | no `0x73A4`/`0x4444` arm in media.rs:286-315; stream-details branch adds `segment_uid` per coordination.md |
| Segment.Info DateUTC | not_collected on main | PR #48 adds `0x4461` → `muxed_at` → "Muxed" dd (pr/48 media.rs diff `date_utc`, web.rs diff) |
| Tracks.TrackEntry type, codec, language/BCP47, forced, hearing_impaired, commentary, default_duration | shown | media.rs:204-213, 371-412; web.rs:1344-1395 (fps, forced, SDH, commentary, language) |
| Tracks.TrackEntry Name (audio, subtitle) | shown | media.rs:209, 399, 408; web.rs:1377, 1393 |
| Tracks.TrackEntry Name (video) | parsed_not_stored | media.rs:209 fills `Track.name`, `VideoTrack` (33-51) has no name field |
| Tracks.TrackEntry TrackNumber, TrackUID, FlagEnabled, FlagDefault, FlagOriginal, FlagVisualImpaired, CodecName, CodecPrivate, CodecDelay, SeekPreRoll, ContentEncodings | not_collected | absent from media.rs:203-259 (`_ => {}`); stream-details branch adds `default` on audio/subtitle tracks (coordination.md) |
| Tracks.TrackEntry BlockAdditionMapping dvcC/dvvC profile + EL | shown | media.rs:242-258, 186-189; web.rs:1356-1362 |
| Tracks.TrackEntry dv_level | stored_not_shown | media.rs:388 stored; no use in web.rs:1344-1364; PR #48 shows "level N" |
| Tracks.Video PixelWidth/Height | shown | media.rs:216-217; web.rs:1346-1348 |
| Tracks.Video DisplayWidth/Height | stored_not_shown | media.rs:218-219, 380-381; not in web.rs:1344-1364; PR #48 `display_aspect` |
| Tracks.Video Colour BitsPerChannel | shown | media.rs:222, 384; web.rs:1352-1354 |
| Tracks.Video Colour TransferCharacteristics, Primaries | stored_not_shown (transfer also drives the shown hdr label) | media.rs:223-224, 385-386, hdr_name 191-198; web.rs:1355 shows only `hdr`; PR #48 `primaries_name`/`transfer_name` |
| Tracks.Video Colour MasteringMetadata (presence) | parsed_not_stored | media.rs:225 `mastering = true` → only the "HDR10" vs "PQ" label (191-194); luminance/primaries values never read |
| Tracks.Video MaxCLL, MaxFALL | not_collected on main | PR #48 adds `0x55BC/0x55BD` → `max_cll/max_fall` → "MaxCLL N nits" |
| Tracks.Video PixelCrop*, DisplayUnit, AspectRatioType, FlagInterlaced, FieldOrder, StereoMode, AlphaMode, MatrixCoefficients, ChromaSubsampling, ChromaSiting, Range, Projection | not_collected | not in media.rs:214-233 |
| Tracks.Video language | stored_not_shown | media.rs:390; web.rs video line has no language; PR #48 adds it |
| Tracks.Audio SamplingFrequency, Channels | shown | media.rs:236-237; web.rs:1367-1368 |
| Tracks.Audio BitDepth, OutputSamplingFrequency, Emphasis | not_collected | media.rs:234-241 handles only `0xB5`, `0x9F` |
| Chapters first edition: starts + names | shown | media.rs:324-356; web.rs:1415-1423 |
| Chapters ChapterFlagHidden | parsed_not_stored | media.rs:335, 347 (filter only) |
| Chapters EditionUID/FlagDefault/Ordered/Hidden, ChapterUID, TimeEnd, ChapLanguage/Country, nested atoms, ChapProcess, other editions | not_collected | media.rs:325-355 (first edition with chapters wins, 354); stream-details adds `editions` count (coordination.md) |
| Tags (all SimpleTags incl. ENCODER, ENCODER_SETTINGS, IMDB/TMDB/TVDB, statistics) | not_collected | no `0x1254C367` arm (media.rs:286-315); stream-details branch adds `encoder`, `encoder_options` (coordination.md) |
| Attachments (FileName, MediaType, UID, Description, FileData/cover) | not_collected on main | PR #48 adds `0x1941A469` → `attachments[]` (name, mime, size, description; 64 × 96 chars) + `cover` → `release_cover` + "Attachments" table + "Cover art" panel |
| structure (SeekHead/Void/Cues order, first Cluster, remux tell) | parsed_not_stored | `header_complete` (media.rs:22-23 serde skip, 310-313, 445) only steers `read_more_front` (step.rs:231) |
| first_video_frame (SEI x264/x265 string, AV1 seq header, thumbnail/pHash) | not_collected on main | no NAL parsing in src/probe; PR #50/#51/#54 add `src/video/*` decoders and `stills` JPEGs (release_stills), not SEI strings |

### mp4
| Item | Status | Evidence |
|---|---|---|
| ftyp presence → container "mp4" | shown | media.rs:84-86, 439; web.rs:1332 |
| ftyp major/minor/compatible brands | not_collected | `mp4()` only descends `moov` (media.rs:440-444) |
| moov.mvhd duration | shown | media.rs:482-493; web.rs:1336-1343 |
| moov.mvhd timescale | parsed_not_stored | media.rs:483-491 |
| moov.mvhd creation/modification time, rate, volume, next track id | not_collected | media.rs:482-493 |
| moov.trak hdlr handler type | parsed_not_stored | media.rs:513-517, 538 (steers video/audio/subtitle) |
| moov.trak mdhd language (audio, subtitle) | shown | media.rs:518-521, 563, 567; web.rs:1369, 1382 |
| moov.trak mdhd language (video) | stored_not_shown | media.rs:544; not rendered on main |
| moov.trak stsd fourcc, width/height, channels, sample rate | shown | media.rs:536-565; web.rs:1345-1348, 1366-1368 |
| moov.trak dvcC/dvvC profile + EL | shown | media.rs:547-556; web.rs:1356-1362 |
| moov.trak dvcC/dvvC level | stored_not_shown | media.rs:553 |
| moov.trak tkhd, edts/elst, hdlr name, avcC/hvcC/av1C/vpcC/esds/dac3/dec3, colr/pasp/clap/fiel/btrt, stts/stsz/stss, tref/chap, chpl | not_collected | media.rs:505-571 reads only hdlr/mdhd/stsd; no MP4 chapters (chapters only from Matroska, 308) |
| moov.udta / meta.ilst (©nam, ©too, covr, tvsh/tvsn/tves, cnID, iTunMOVI…) | not_collected | no `udta` arm in `moov()` (media.rs:480-497) |
| misc: moov position/completeness (faststart) | parsed_not_stored | `has_whole_moov` (media.rs:450-470) → `header_complete` only |
| misc: uuid, free/skip sizes, moof/sidx, mdat size, meta key scheme, pssh | not_collected | media.rs:416-447 |

### avi_and_others, audio, images_and_documents, subtitles, content_identity
| Item | Status | Evidence |
|---|---|---|
| avi, asf_wmv, mpeg_ts_ps (TS/PS, DVD IFO, BDMV), ogg_flv_rm | not_collected | `archive::identify` sniffs RIFF/AVI, `00 00 01 BA`, OggS → `Format::Media` only (src/archive/mod.rs:76-84); `media::parse` → None (media.rs:88-96) so `info.media` is absent |
| audio.mp3 (ID3v2, Xing/LAME, MPEG header) | not_collected | `ID3` magic → `Format::Media` (archive/mod.rs:82); nothing else |
| audio.flac (STREAMINFO MD5, VORBIS_COMMENT, PICTURE) | not_collected | `fLaC` magic only (archive/mod.rs:80) |
| audio.aac_m4a_opus_ogg: .m4a via MP4 path (mp4a codec, channels, rate, language); Opus in MKV | shown | media.rs:559-565, 392-400; web.rs:1365-1379 |
| audio.aac_m4a_opus_ogg: ilst tags, ADTS/LATM, Opus/Vorbis vendor strings | not_collected | no `udta` in `moov()`; `OggS` magic only |
| audio.ac3_eac3_dts_truehd: codec id + channels (+ Atmos from Matroska track NAME only) | shown | media.rs:393 (`atmos` from name), 395-397; web.rs:1370-1372; `AudioTrack::default()` for MP4 → atmos always false (media.rs:559-565) |
| audio.ac3_eac3_dts_truehd: bsid, acmod, dialnorm, JOC flag, DTS ext, TrueHD major sync | not_collected | no bitstream parsing in src/probe |
| audio.wav_ape_wv_mpc, rip_sidecars (.log/.cue) | not_collected | not sniffed (archive/mod.rs:51-88); .log/.cue are not `is_small_text` (step.rs:33-36) |
| images_and_documents jpeg, png_gif_webp, pdf_epub_mobi_cbz, executables_and_discs | not_collected | `Format::Image` by magic only (archive/mod.rs:66-75); no PDF/EPUB/ISO/PE sniff |
| subtitles.srt_ass_ssa sidecar fields | not_collected | sidecars never fetched (step.rs:33-36); embedded S_TEXT tracks: codec/language/name/flags shown (web.rs:1380-1395) |
| subtitles.vobsub_pgs_dvb embedded PGS/DVB tracks | shown | media.rs:401-409 (kind 17); web.rs:1380-1395 |
| subtitles.vobsub_pgs_dvb .idx sidecar | not_collected | not fetched |
| subtitles.external_hashes moviehash | shown | step.rs:184, 294-362; mod.rs:77; web.rs:1398-1400 + `release_opensubtitles` match 1401-1414 |
| subtitles.external_hashes subtitle file MD5 | not_collected | — |
| content_identity.moviehash | shown | as above; `release_probe.moviehash` column indexed (mod.rs:40-46) |
| content_identity.perceptual (pHash, chapter signature, Chromaprint) | not_collected on main | PR #50/#51/#54 decode keyframes to JPEG stills (release_stills) but compute no hash; no audio fingerprint |
| content_identity.structural (chapters, track layout, duration as a key) | shown (as facts), not used as a key | web.rs:1330-1423; `release_fingerprint.content` is sizes only (duplicates.rs:21-40; docs/todo-duplicate-clusters.md) |

### media_additions
| Item | Status | Evidence |
|---|---|---|
| matroska_element_ids_not_read (0x73A4, 0x4444, 0x4461, 0x73C5, 0x63A2, 0x88, 0x55AE, 0x1254C367, 0x1941A469, 0x6E67, 0x6EBC, edition flags) | not_collected | none of these ids appear in media.rs; PR #48 adds 0x4461 and 0x1941A469; stream-details adds 0x73A4 and `default` flags (coordination.md) |
| muxer_layout_tell: muxing_app/writing_app | shown | web.rs:1333-1334 |
| muxer_layout_tell: element order, DateUTC, statistics tags | not_collected | `segment()` ignores order; Tags not read |
| codec_config_bit_depth (hvcC/avcC bit depth, profile/tier/level) | not_collected; Colour BitsPerChannel shown instead | media.rs:222, 384; `stsd` child boxes other than dvcC/dvvC ignored (547-556); CodecPrivate not read |
| remux_invariant_alt_key (codec config + first frame payload hashes) | not_collected | no cluster parsing (media.rs:310-313 stops at the first Cluster); `release_fingerprint.content` = sizes |
| ass_subtitle_keys (S_TEXT/ASS CodecPrivate hash, font-set hash) | not_collected | CodecPrivate `0x63A2` not read; Attachments not read on main |
| mp4_moov_at_the_end | parsed_not_stored (header_complete=false), tail moov never fetched | media.rs:450-470; step.rs:223-238 reads parts 2..6 forward only |
| mp4_codec_config_boxes: stsd fourcc, channels, rate, dvcC/dvvC profile/EL | shown | media.rs:536-565; web.rs:1344-1379 |
| mp4_codec_config_boxes: dac3, dec3 (JOC/Atmos), esds (SBR/PS), colr/pasp/clap/fiel, btrt, hvcC | not_collected | media.rs:547-556 only dvcC/dvvC |
| tagger_ids_and_buyer_leaks (SimpleTag IMDB/TMDB/TVDB, iTunes cnID; apID/purd/ownr privacy) | not_collected | Tags and ilst unread; privacy rule applies the day they are |
| dolby_vision_rpu_rules: dv_profile, EL from config record | shown | media.rs:186-189, 387-389; web.rs:1356-1362 |
| dolby_vision_rpu_rules: dv_level | stored_not_shown | media.rs:388; PR #48 shows it |
| dolby_vision_rpu_rules: RPU (MEL/FEL, CM version, L1-L11) | not_collected | no NAL parsing |
| encoder_sei_strings (x264/x265 SEI, HDR10+ T.35, captions, frame packing) | not_collected | no NAL parsing; stream-details branch adds `encoder`/`encoder_options` from Tags, not SEI (coordination.md) |
| bitstream_headers (SPS/PPS/VUI, VPS, AV1 seq header, ADTS, AC-3 BSI, TrueHD sync, DTS) | not_collected | only codec ids + track name "atmos" (media.rs:393) |
| mediainfo_and_bdinfo_text_in_nfos | shown as raw text, not parsed | NFO text stored whole (step.rs:128) and rendered `<pre>` (web.rs:1430-1432); no field parsing anywhere (src/nfo.rs parses names only) |
| bluray_dvd_identity (id.bdmv, index.bdmv, MPLS/CLPI, DVD IFO MD5, TS SDT/TDT/EIT) | not_collected | no BDMV/IFO/TS parsing; `00 00 01 BA` only sniffed |
| subtitle_sidecar_fields | not_collected | sidecars never fetched (step.rs:33-36) |
| image_fields (EXIF, XMP, DQT, PNG tEXt) | not_collected | `Format::Image` ends the look (archive/mod.rs:66-75) |
| non_video_content_keys (FLAC MD5, Ogg serial, LAME CRC, APE MD5, ISO PVD, CHD, PDF /ID, PE GUID) | not_collected | — |
| pure_rust_decode_budget (mid-file keyframe reads, decoders) | not_collected on main | PR #50 HEVC intra decoder (src/video/hevc), PR #54 H.264, PR #51 `probe/stills.rs` reads 3 keyframes through `moviehash::volumes` and the RAR map → `release_stills`; no UI in those PRs (pr/51 stat has no web.rs) |
| credits_and_title_card_ocr | not_collected | — |
| stream_thread_keys (keyframe NAL hash, AC-3 crc2 run, FLAC MD5, pHash) | not_collected | — |

### nfo
| Item | Status | Evidence |
|---|---|---|
| release_name | shown | src/nfo.rs (NfoNameExtractor port) → `releases.display_name` + `name_source`; web.rs:1087-1093 "Posted as" / "Name from: the NFO" (release_info.rs:24-35) |
| ids_and_urls: IMDb tt ids | shown | step.rs:121 `imdb_ids` → `info.imdb_ids` → links above the NFO (web.rs:1425-1432); `MediaNfo` → `media_release.imdb_id` + `imdb_from_nfo` (media/steps.rs:98-99, 332-354) → "Links" IMDb (web.rs:1793-1795) and API `imdbid` |
| ids_and_urls: TMDb/TVDB/TVmaze/AniDB/MusicBrainz/Discogs/Steam/Amazon/CSDb/srrDB URLs | not_collected | `imdb_ids` (mod.rs:166-185) and `IMDB_IN_NFO` (steps.rs:103-104) match `tt\d{7,8}` only |
| template_fields (Group, Date, Source, Video, Audio, Runtime, Size, Genre, Rating, Language, Subs, banner) | shown as raw text, not parsed | whole text in `info.nfo` (≤256 Ki chars, step.rs:22, 128) rendered `<pre class="nfo">` (web.rs:1430-1432); `MAX_NFO` truncation silent; `nfo.rs` reads only name-shaped lines |
| password | shown | `find_password` (release_info.rs:510-526) at step.rs:122-127 → `release_nfo_password`; `password_hint_with` (539-543) → `password_fact` "found in the NFO" + WORKS/WRONG/not tried (web.rs:1741-1749, 1126-1128) |
| encoding_and_shape (CP437 vs UTF-8, line width, banner hash) | parsed_not_stored | `nfo_text` (mod.rs:151-156) chooses CP437 when not UTF-8, the choice is not recorded; width/banner never computed |

## 7. Branches and threads touching the same code

### 7.1 PR #48 `remotes/pr/48` (5 commits ff8e66b…06847b2, "Show cover art, attachments, every stream field and the NZB's files on the release page")
Diff vs main: assets/web.css (+4), src/media/details.rs (`trim_posters` → `pub(crate)`), src/postprocess/schema.rs (+`COVER` in SHARD_TABLES), src/probe/media.rs (+193), src/probe/mod.rs (+67), src/probe/step.rs (+17), src/release_info.rs (+35), src/web.rs (+147), tests/web_ui.rs (+32).
- media.rs: `MediaInfo.muxed_at` (DateUTC `0x4461`, `date_utc` 2001 epoch), `attachments: Vec<Attachment{name,mime,size,description}>` (`MAX_ATTACHMENTS 64`, `MAX_ATTACHMENT_TEXT 96`), `cover: Option<Cover{mime,bytes}>` (`#[serde(skip)]`, `MAX_COVER 512 KiB`, `cover_rank` cover > cover_land > small_cover > small_cover_land > any .jpg/.png, `cover_of` checks whole JPEG/PNG markers), `VideoTrack.max_cll/max_fall` (`0x55BC/0x55BD`), `walk_sized` (declared size vs read size for cut-off attachments).
- mod.rs: `COVER` ShardTable `release_cover(release_id, mime, sha256)`, covers folder keyed by SHA-256 (`keep_cover`, atomic write, 1 GiB cap trimmed via `details::trim_posters`), `cover()`, `has_cover()`.
- step.rs: `config_stamp` **1 → 3** (its bit 2 = attachments/cover/muxed_at/MaxCLL); `SET_COVER`/`DROP_COVER` write every run (a run that finds no cover drops an earlier one).
- web.rs: `/ui/cover` route; `probe_html(p, matched, cover: Option<i64>)`; "Muxed" dd; `display_aspect` ("shown at 1.78:1"), `primaries_name` (BT.709/BT.601/BT.2020/DCI-P3/Display P3), `transfer_name` (PQ/HLG when no hdr label), DV "level N", "MaxCLL N nits"/"MaxFALL N nits", video language (≠ "und"); "Attachments" table (name, description|mime, size); "Cover art" panel first; "Files in the NZB" table from `release_info::Contents.list` (`NzbFile{name,bytes,seen,expected}`, `MAX_LISTED_FILES 1000`); "Files inside" renamed "Files inside the archives".
- **Collision**: main's `config_stamp` is already `1 | 2 = 3` (step.rs:99-101, bit 2 = nested archives from #52). PR #48's bit 2 means something else. Merged as-is, releases probed on main at stamp 3 would never be re-probed for attachments/cover/MaxCLL. The merge must move #48's bit to 4 (`1 | 2 | 4`). Also `probe_html`'s signature, the release_html body (main gained Groups/split posts #46/#53 and `nested_chain` layers #52 since #48 branched) and `release_info::Contents` will conflict textually.
- Per coordination.md, the media/frames thread owns #48 and the stream-details work; this workflow's other panels should not re-render those fields.

### 7.2 Stream-details branch (`feat/release-page-stream-details`)
Not in this clone (`git branch -a` lists only main and pr/21,48,49,50,51,54,55,57,58; `git log --all --grep stream` finds only #48's commit; no research/ folder). coordination.md (another thread's reply, data not verified against code) says it is stacked on #48 and adds to `probe_html`/`details_html`: `hdr_summary` (HDR10, HDR10 with mastering data, HLG, DV profile/level), `encoder` + `key_options` (encoder settings), `segment_uid`, `editions` count, default-track flags, `runtime_note` (runtime vs TMDB), with media.rs fields `segment_uid, editions, encoder, encoder_options, default` on audio/subtitle tracks. Any work on the Tracks panel or `MediaInfo` will conflict with it.

### 7.3 PR #51 (mid-file keyframes) and #54 (H.264 decoder), #50 (HEVC decoder)
- Add `src/video/{bits,picture,jpeg,mkv,hevc/*,h264/*}` and `src/probe/stills.rs` (`STILLS_STEP = StepId(6)`, `release_stills` table, `stills/` folder 2 GiB cap). They read `release_probe.info` to find the video track (pr/51 stills.rs:298) — a dependency on the probe's JSON shape (`media.video[0]`).
- Touch main's media code only for visibility: `media.rs` `element_id`/`element_size` → `pub(crate)` (used by `src/video/mkv.rs`), `step.rs` `main_file`/`fetch_raw`/`read_part` → `pub(super)`, `moviehash::volumes` (new), `schema.rs` SHARD_TABLES, `steps.rs` list, `details::trim_posters` pub(crate) (same one-line change as #48 → trivial conflict).
- No web.rs changes in #51/#54 → stills are stored_not_shown there; coordination.md says a later PR adds `/ui/still` and a stills strip panel.
- `src/video/mkv.rs` is a second EBML walker (cluster/SimpleBlock/BlockGroup scanning) built on media.rs's primitives; any Tags/Cues work should reuse `walk` from media.rs rather than a third walker.

### 7.4 Others
- PR #58 (compressed RAR5/bzip2/deflate64/lz4/lzw layers in `nest.rs`, new `src/probe/codec/*`): raises the share of releases whose inner MKV/MP4 header becomes readable (media::parse runs on `nest.inner`, step.rs:400). Touches probe/mod.rs (one `pub mod codec` line) and nest.rs.
- PR #57 (sort window): 495 changed lines in src/web.rs (results code); release page additions will merge-conflict in web.rs.
- PR #55 (yEnc crate in nntp.rs): the probe's `yenc_decode` path; no media conflict.
- Finish web UI thread / twin grouping: `copies_html`/`copy_card` (web.rs:1463-1572) and `duplicates_page` (1575-1654) render `scores_html` and `id_links` for every copy; docs/todo-duplicate-clusters.md tracks it (Part 2 "release details" lists tracks/HDR as done only via name tags and TMDB). New per-release media facts shown in `release_html` would need the same in `copy_card` to stay consistent, or an agreed rule that copies show name-derived facts only.

## 8. Extension points

### 8.1 Parser (src/probe/media.rs)
- New Segment-level elements: add arms in `segment()` (286-315) before the Cluster arm; Tags `0x1254C367` (SimpleTag `0x67C8`: `0x45A3` TagName, `0x4487` TagString, `0x63C0` Targets) and Attachments `0x1941A469` (PR #48 has a tested implementation) go here. DateUTC `0x4461` and SegmentUID `0x73A4` go in the Info walk (289-298).
- New track fields: `Track` (162-183) → `track_entry` arms (203-259) → `VideoTrack`/`AudioTrack`/`SubtitleTrack` (33-76) → `add_track` (371-412). CodecPrivate `0x63A2` (bit depth from hvcC/avcC; ASS script hash) belongs here; `Track` needs a `private: Option<Vec<u8>>` capped by size.
- MP4: `moov()` (480-497) for `udta`/`meta`/`ilst` and `mvhd` times; `trak()` (505-571) for `tkhd`, `hdlr` name, `stsd` child boxes (`hvcC`, `avcC`, `esds`, `dac3`, `dec3`, `colr`, `pasp`); `mp4()` (438-447) for `ftyp` brands.
- Trailing data: a "tail" read for `moov`-at-end or mkvmerge statistics Tags needs a new reader next to `read_more_front` (step.rs:223-238) using `read_part` and the last article the probe already fetches (170-181, `known_last`); `header_complete` (media.rs:22) is the signal.
- Unit test scaffold: `sample_mkv()` (585-644) builds EBML by hand; `mp4_tracks_come_out` (720-747) builds boxes.

### 8.2 Storage
- Small per-release fields: new `MediaInfo` fields land in `release_probe.info` with no migration (serde default). Searchable fields: `PROBE.added` (mod.rs:46) nullable columns + `SET_PROBE` params (49-50, step.rs:204-207); `added` columns are applied on open (schema.rs:155-163).
- New tables: a `ShardTable` const registered in `SHARD_TABLES` (schema.rs:131-145); gc trigger comes free (175-181). Pattern for blobs: files under `app_dir/<kind>/<sha256>` + a small shard table (PR #48 `release_cover`, PR #51 `release_stills`), trimmed with `details::trim_posters`.
- Per-title data (ids from Tags, encode date keyed by SegmentUID): `atlas.details.db`/`atlas.media.db` are per TITLE, not per release; per-mux keys belong in a shard table keyed by release_id (the catalog's storage table suggests `release_probe.info.media` for small fields and a `release_art` blob table).

### 8.3 Backfill job
- Bump `ProbeStep::config_stamp` (step.rs:99-101) to `1 | 2 | 4`: `seed_shard` (engine.rs:1103-1112) queues a `steps` walk with the probe's mask on the next start; `walk_tick` (412-475) replays every release newest-first. Alternatively `atlas --reprocess --step probe --apply` (reprocess.rs).
- Cost model: each re-probe refetches the NFO/SFV first articles, the main file's first and last article, STATs the middle, and up to 5 more front articles while the header is incomplete; bounded by `probes_per_minute` (60) and `body_fetches_per_minute` (600). At the default 60 probes/min: 1,000 releases ≈ 17 min, 100,000 ≈ 28 h, 1,000,000 ≈ 11.6 days, sharing the budget with archive steps and new releases. The walk itself runs at `backlog_per_second` (2000) / SHARDS, so the body budget is the binding limit.
- Re-probe writes `insert or replace` (mod.rs:49): a release whose articles have since expired (health GONE) is rewritten with a sparser JSON (or skipped when nothing at all came back, step.rs:196-199). A field-level merge (keep the old `media` when the new read has none) does not exist and would be needed to avoid regressions on retention-expired releases.
- The JSON has no version marker; "absent field" cannot be told from "not probed since the field existed" except through `meta.pp_stamp_4`/`pp_walks` progress per shard.

### 8.4 UI
- `probe_html` (web.rs:1243-1453): Tracks lines at 1344-1395 (add `display_*`, primaries/transfer names, dv_level, video language — PR #48 has them), Chapters at 1415-1423; a new panel = `panel("Title", &dl)` (1235-1240) added to the `format!` at 1445-1452; a table panel follows the NFO/SFV pattern (1430-1444).
- Facts for the summary block: `release_html` 1047-1142 (`fact` closure). Lazy panels: `hx-get` div (1190) + a handler like `details_panel` (1657-1663) registered in `get()` (48-72, `signed_in` guard, 401 → HX-Redirect at 66-68).
- Images: `/ui/poster` (1717-1739) pattern: bytes from a folder, `Cache-Control: private`, `X-Content-Type-Options: nosniff`; CSP `default-src 'self'` (41) means images must be served by Atlas.
- Copies: `copy_card` (1463-1548) if copies should show the same facts.
- API: `release_attrs` (api.rs:285-310) for newznab attrs; `t=caps` via `IdSet::caps` (query.rs:40-57); the catalog asks for `hdr`/`dvprofile` custom attrs behind `extended=1` — today `hdr` is emitted from the name for every item (api.rs:301).
- Tests: tests/web_ui.rs end-to-end (PR #48 inserts a `release_probe` JSON row and asserts panel text); media.rs unit tests.

## 9. Risks

1. **config_stamp bit collision** between main (#52: bit 2 = nested archives) and PR #48 (bit 2 = attachments/cover). Whoever merges second must renumber or the backfill silently does not run for already-probed releases. Next free bit on main: 4.
2. **Backfill scale and budget**: a full re-probe of every release is bounded by `probes_per_minute` (60) and shares `body_fetches_per_minute` (600) with the archive steps; order newest-first (engine.rs:435-439) means old releases wait days; retention may already have removed the articles of old releases, in which case the re-probe either writes nothing (step.rs:196-199) or replaces richer JSON with poorer (insert-or-replace, mod.rs:49-50). A merge-on-write or a "only when more was read" guard is needed before backfilling.
3. **JSON row growth**: `release_probe.info` already carries up to 256 Ki chars of NFO per release; attachments lists (64 × 96 chars), Tags, and ilst would add a few KB per row; every page open parses the whole JSON (`probe::read`, mod.rs:142-148). Caps per list (`MAX_*` constants) are the house pattern.
4. **No per-row version**: old rows deserialize with defaults, so the UI cannot distinguish "no chapters in the file" from "probed before chapters were read" (same for every new field). A `probe_version` field in `ProbeInfo` would fix this cheaply and is itself a backfill-visible marker.
5. **HDR labelling** (media.rs:191-198): "HDR10" needs MasteringMetadata presence; PQ-only files show "PQ"; bit depth only from Colour BitsPerChannel (often absent) rather than hvcC/avcC. The stream-details branch's `hdr_summary` reworks this — coordinate before touching.
6. **MP4 moov-at-end** wastes up to 5 extra articles (`read_more_front` reads forward only) and yields nothing; a computed tail read is the fix (catalog rank 9/10).
7. **Atmos/commentary from track names** (media.rs:393-394): never from the bitstream; always false for MP4.
8. **Merge conflicts in src/web.rs** with PR #48 (release_html/probe_html), PR #57 (results code), main's #46/#53 (split posts); in src/probe/step.rs and schema.rs/steps.rs with #48/#51/#54; in src/probe/media.rs with #48 (193 lines) and #51/#54 (visibility). PR #48's web.rs diff is against a pre-#46 release_html.
9. **Privacy** once Tags/ilst are read: iTunes `apID`/`purd`/`ownr`, RAR `UOWNER` must not be displayed (catalog open questions); `probe_html` escapes but shows strings verbatim.
10. **Twin grouping overlap**: `copy_card` duplicates the facts rendering; the Finish web UI thread's work on clusters (docs/todo-duplicate-clusters.md) touches the same page; agree where per-copy media facts show.
11. **Sealed segments**: files are sealed into `files.blob` after `SEAL_AGE` 3 days (src/store.rs:788); re-probing old releases reads segments through `store::file_segments` (1905) from blobs — slower per release than fresh rows.
12. **API drift**: `hdr`, `video`, `audio`, `resolution` attrs come from the NAME (api.rs:286-303), not from the probe; showing probe-derived values in the UI while the API reports name-derived ones will disagree on mislabeled releases.

## 10. Open questions

- Where is the `feat/release-page-stream-details` branch (not in this clone)? Its `MediaInfo` field names (`segment_uid`, `editions`, `encoder`, `encoder_options`, `default`) should be adopted verbatim to avoid two JSON spellings.
- Which `config_stamp` bit does the backfill take once #48 lands (4 if #48 is renumbered, 8 otherwise)?
- Should a re-probe be allowed to replace a richer `info` JSON with a poorer one when articles have expired?
- Should per-field visibility stay server-rendered in `probe_html` or move to a lazily loaded panel like `/ui/details` to keep the release page fast when JSON rows grow?
- Does the API gain probe-derived attrs (`hdr`, `dvprofile`, `container`, `codec`) behind `extended=1`, and from which source when name and probe disagree?
- Do copies (`copy_card`) show probe-derived facts, or only name-derived ones?
