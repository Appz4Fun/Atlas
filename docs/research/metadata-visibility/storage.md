# Atlas storage map (area: storage)

Repo: /home/claude/appz4fun/atlas, branch main at 55d2e51 ("Follow archives inside archives, up to 10 layers (#52)").
Read-only survey; no cargo, no edits. Every claim carries file:line (paths relative to the repo root unless absolute).
Feature this serves: every catalog field visible on every release page, including a backfill over existing releases.

Companion notes from other threads: /tmp/claude-0/-home-claude/b80d34bc-6432-5cb9-a471-5c997487e01d/scratchpad/atlas-map/coordination.md
Catalog: /tmp/claude-0/-home-claude/b80d34bc-6432-5cb9-a471-5c997487e01d/scratchpad/catalog-current.md

---

## 1. Physical layout

| File | What | Where defined |
|---|---|---|
| `app_dir()/atlas.db` (main) | `meta` (next_seq, id blocks), `groups`, `backfill_*` chunk tables, legacy `articles` for old layouts | src/paths.rs:66-68 (`app_dir()` = `ATLAS_HOME` or `~/.atlas`); src/store.rs:113-120 `create_main`; src/db.rs:318-342 `create_db_at`; src/chunks.rs:20-50 |
| `atlas.s0.db` .. `atlas.s7.db` (8 shards) | releases, files, segments, domains, every post-processing table | `SHARDS = 8` src/store.rs:35; `shard_of(group)` FNV hash src/store.rs:38-41; `shard_path` src/store.rs:53 |
| `atlas.media.db` | `media_titles` lookup cache | src/media/store.rs:9-24 |
| `atlas.details.db` | `details`, `details_lookups` (TMDB/IMDb/Trakt facts per title) | src/media/details.rs:44-69, `backdrop` column added at :108 |
| `atlas.predb.db` | PreDB titles | src/predb/step.rs:92-100; README.md:332 (153 MB per million titles) |
| `app_dir()/posters/` | poster + backdrop image cache on disk, trimmed to `MAX_POSTER_CACHE` 4 GiB | src/media/details.rs:533, :35, :589 |
| external `sealed-log` service | offloaded sealed blobs; `files.blob` holds a 21-byte locator | sealed-log/src/lib.rs:45-52 (`Locator{segment u32, offset u64, len u32}`, `REF_TAG = b"\xFFSLG1"`, `REF_BYTES = 21`); src/sealed_store.rs |
| `<shard>.sealed-log-cursor` | offload cursor file per shard | src/sealed_store.rs:200 |
| `<shard>.precompact` | compaction swap target | src/compact.rs:418-797 (compact_shard) |

Release id = `seq * SHARDS + shard` (`global_id` src/store.rs:48); `shard_of_id(id) = id % 8` (src/store.rs:44). Every reader attaches the shards as `s0..s7` and builds a temp `releases` view (`attach` src/store.rs:94-111). Readers are pooled: `MAX_READERS` 4, `READER_IDLE` 600 s, cache split per shard (src/db.rs:41-47, `with_reader` :86). Connections run busy_timeout 30 s, foreign_keys off, synchronous normal and register the SQL filter functions (`set_up` src/db.rs:167-175). WAL checkpoints come from the shard writer's housekeeping (`finish_checkpoint` src/db.rs:196; src/indexer.rs:430).

One single writer thread per shard (src/indexer.rs:258+, `ShardWriter::new` :272); stage steps hand their writes to it through `Db::on_writer` (src/indexer.rs:445-454) and `stage_transaction` begins immediate (src/indexer.rs:409-418). Budgets: `STAGE_BUDGET_BUSY` 10 ms, `STAGE_BUDGET_IDLE` 100 ms (src/indexer.rs:229-230).

Size figures (documents, not measured by me): one shard holds about 9.8M files and 221M segments, average `local` 32 bytes, shards total 138.5 GB (docs/superpowers/specs/2026-10-05-faster-backfill-smaller-storage-design.md:7); about 80 bytes per article as rows (:12); 15-27 bytes per article sealed versus 55-70 as rows, 138 GB expected to fall to ~60 GB (:137). README.md:405: about 15 million of 118 million releases pass the body gates, 1-3 bodies per release typical, at most 8. README.md:512: `--reflag-obfuscated` over 118M releases takes 2-10 minutes and writes up to ~12 GB of WAL; `--unname` of 15M names 5-10 minutes. Scaling the spec's shard numbers by 8: ~78M files and ~1.77 billion segments in total, ~15 articles per release on average, heavily skewed.

---

## 2. Shard schema, table by table

### 2.1 Core tables (built by `build_shard` src/store.rs:129-199, migrated by `migrate_shard` :202-249)

**releases** (src/store.rs:133-145)
`id INTEGER PRIMARY KEY, name TEXT, group_name TEXT, poster TEXT, posted_date TEXT, size INTEGER, complete INTEGER, parts INTEGER, file_total INTEGER, display_name TEXT, is_obfuscated INTEGER default 0`, plus `name_source INTEGER` added through `RELEASE_COLUMNS` (src/postprocess/schema.rs:96; NULL = ingest name, otherwise the `NameSource` priority src/naming/mod.rs:28-41: Ingest 0, Cleared 1, PostedFile 3, NamingRegex 5, InlineBody 9, Nfo 10, ArchiveInner 15, Par2 20, PreDb 100).
Indexes: `idx_release_unique (name, group_name)` unique :146; `idx_release_group (group_name)` :147; optional `POSTER_INDEX on releases(poster)` and `SIZE_INDEX on releases(size)` built only by `atlas --search-indexes --apply` (src/field_search.rs:380-381).
FTS: `releases_fts` fts5 over `(name, display_name)` with `content='releases'`, triggers `releases_ai/ad/au` (:149-163).
Writer: `save_in_transaction` upsert (src/store.rs:1367-1565; `on conflict(name, group_name)` at :1394 rewrites poster and posted_date on every merge, display_name/is_obfuscated only while `name_source is null`), then `update releases set size, complete, parts, file_total` (:1396); naming steps change `name/display_name/name_source` through `apply::write` (src/postprocess/apply.rs:113-250) with a source guard; `inline_rename` (src/store.rs:1723).
Readers: `search::ReleaseRow {id, name, group_name, poster, posted_date, size, complete, parts}` with `coalesce(r.display_name, r.name)` (src/search.rs:9-18, :33-34), `VISIBLE_RELEASE` (:37-40), `get_release_with` (:557-560); `release_info::names_with` reads `name, display_name, is_obfuscated, name_source` (src/release_info.rs:136); the engine's `ROW_COLUMNS` (src/postprocess/engine.rs:91).

**files** (src/store.rs:165-177)
`id INTEGER PRIMARY KEY, release_id NOT NULL, filename NOT NULL, subject, subject_part, subject_mid, expected, file_total, seen BLOB NOT NULL, touched_at, blob BLOB`; `touched_at` and `blob` are added to old shards via `pragma table_info(files)` + `alter table files add column` (:239-245). Unique `files_key (release_id, filename)` :178.
`seen` is the part bitmap (`FileState` :571-604, `insert_part`/`is_exactly` :538-559); `subject` is the lowest-numbered part's Subject, used by the NZB (src/nzb.rs:85-88); `blob` is the sealed segment list or a sealed-log locator (section 3).
Writer: `put_file` sets `touched_at = unixepoch()` (:632-640); `seal_rows` writes `blob` and deletes the rows (:836-925); `sealed_store::swap_in` replaces a blob with its locator (`update files set blob = ? where id = ? and blob = ?` src/sealed_store.rs:162-198).
Readers: `release_files` (:1836, returns `FileInfo` :1760-1771), `file_segments` (:1905), `each_article`/`articles` (:1947/:1935), `release_info::union_files` (src/release_info.rs:393), password hint reads `files.filename/subject` with limit 64 (src/release_info.rs:539).

**segments** (src/store.rs:181-188)
`file_id, local BLOB, domain, part, bytes, primary key (file_id, local, domain)` WITHOUT ROWID. One row per article still "loose" (not yet sealed). `local` is the message-id's local part packed by alphabet tag (tags :311-319, `pack_local` :373, `unpack_local` :404; `MAX_LOCAL` 512 bytes src/blob.rs:107); `domain` points at `domains.id`, 0 meaning the whole message-id sits in `local` (`whole` :455). There is **no article number, no CRC, no date, no per-article From** in this table (spec :7 confirms the five columns).
Writer: `add_segment` (:645-705) after dedup through `release_ids` (:225-230) and the `SealedCache` (:1144-1230); late articles for a sealed file become rows again and are folded in by `fold_in`/`settle_held_back` (:957, :1013).
Readers: `each_article` joins `s{s}.files/segments/releases/domains` and then decodes blobs (:1947-2010); `seen_count` (:1790); `totals` (:2026) for the dashboard.

**domains** (src/store.rs:180): `id INTEGER PRIMARY KEY, suffix TEXT NOT NULL UNIQUE`; shared suffixes after `SHARED_AFTER` 3 sightings, `DOMAIN_CACHE` 100 000 (:450-452). Never shown or searched.

**meta** per shard (src/store.rs:191-192): `key TEXT PRIMARY KEY, value INTEGER`; running totals `releases`, `articles`, the `id_key0/id_key1` SipHash keys (:233-237), seal/purge/naming cursors (`purge_top/purge_classes/purge_below/purge_after/purge_victim` src/postprocess/purge.rs:330-430; naming walk cursor src/naming/walk.rs:138-172), `pp_walk_seeded`, `pp_steps_seen`, `pp_stamp_{id}` (src/postprocess/engine.rs:1089-1107), `archive_name_bytes`, `archive_names_over_quota` (src/archive/store.rs:19-37, :63-78). `get_meta`/`set_meta` :299-307.

**collections** (src/store.rs:207): `key INTEGER PRIMARY KEY, release_id INTEGER NOT NULL` WITHOUT ROWID; the ported nntmux collection key -> release (src/collections.rs). Not shown.

**held_back** (src/store.rs:211-219): `file_id, message_id, subject, part, total_parts, file_total, PK(file_id, message_id)` WITHOUT ROWID; articles whose file key was guessed, folded in later (`held_back()` :939, `settle_held_back` :1013, `SETTLE_SECS` 7200 / `COMPLETE_SETTLE_SECS` 900 :255-257).

**release_ids** (src/store.rs:225-230): `release_id, id_hash, file_id, PK(release_id, id_hash)` WITHOUT ROWID; SipHash of each message-id (`IdKey` :713-733) for dedup; `index_release` :737.

### 2.2 Registered post-processing tables (`ShardTable` registry)

`ShardTable {name, create, indexes, release_key, added}` (src/postprocess/schema.rs:8-19). `SHARD_TABLES` (:78-92) lists, in order: `PP_QUEUE`, `PP_STATE`, `PP_WALKS`, `archive::store::INNER_FILES`, `archive::store::RELEASE`, `archive::store::NFO_PASSWORD`, `duplicates::FINGERPRINT`, `probe::PROBE`, `probe::opensubtitles::OPENSUBTITLES`, `predb::step::MATCH`, `predb::step::INNER_KEY`, `category::RELEASE_CATEGORY`, `media::MEDIA_RELEASE`. `schema::migrate` (:114-130) creates each table if missing, adds every `added` column that `pragma table_info` lacks (`add_missing` :102), creates the indexes, and installs `create trigger if not exists gc_{table} after delete on releases` (:124) so `delete_release` (src/store.rs:1801-1819) cascades. It runs at the end of `migrate_shard` (src/store.rs:248), so every open, conversion and compaction migrates.

| Table | Columns | Indexes | Written by | Read by |
|---|---|---|---|---|
| **pp_queue** (schema.rs:22-30) | `release_id PK, due_at` | `pp_queue_due` | `save_in_transaction` enqueue with settle (src/store.rs:1537-1545) | `queue_batch` `DB_BATCH` 256 (engine.rs:27, :380-385) |
| **pp_state** (schema.rs:33-58) | `release_id PK, done, failed, net_wanted, db_attempts, net_attempts, net_next_at, parts_seen, last_error, rerun_gen, rerun_mask` | `pp_state_net`, `pp_state_failed` | `apply::write` upsert (apply.rs:113-250) | `walk_tick` left join (engine.rs:437), `net_claim` `NET_BATCH` 4 (:799-808), stats |
| **pp_walks** (schema.rs:61-75) | `id, kind ('backlog'/'steps'/'reprocess'), top_id, before_id, redo_mask, group_name, seen, ran, created_at` | - | `seed_shard` (engine.rs:1073-1113), `reprocess::apply` (reprocess.rs:25-42) | `walk_tick` (engine.rs:412-500), deleted when exhausted (:453) |
| **archive_inner_files** (src/archive/store.rs:18-37) | `release_id, seq, name, size, source (1 PAR2, 2 RAR, 3 7Z, 4 ZIP; :13-16), PK(release_id, seq)` WITHOUT ROWID; trigger `archive_names_give_back` returns name bytes to meta on delete | - | archive steps `DELETE_SOURCE`/`INSERT_FILE`/`ADD_NAME_BYTES`/`COUNT_OVER_QUOTA` (src/archive/steps.rs:99-146), quota `max_names_mb` 16384 MiB (README.md:399) | `rows()` limit 256 (store.rs:90), `view_with` -> `InnerView` (:169, :148-155) |
| **archive_release** (src/archive/store.rs:42-50) | `release_id PK, password NOT NULL`; added `encryption INTEGER, names_hidden INTEGER, password_check BLOB` | - | `UPSERT_RELEASE`, `SET_ENCRYPTION` (steps.rs:99-146) | `view_with` (store.rs:169) |
| **release_nfo_password** (src/archive/store.rs:54-62) | `release_id PK, password TEXT` | - | probe step `SET_NFO_PASSWORD` (src/probe/step.rs:124), naming NFO step | `nfo_password` (store.rs:158), `password_hint_with` (release_info.rs:539) |
| **release_fingerprint** (src/duplicates.rs:21-39) | `release_id PK, posted BLOB, content BLOB`; added `par2_block INTEGER, par2_blocks INTEGER, posted_names BLOB` | posted/content indexes | `SET_POSTED`/`SET_PAR2_SIZES`/`SET_CONTENT`/`CLEAR_*` (:42-53) from the PAR2 and list steps (src/archive/steps.rs:254-268); `forget_old_twins` at start (:97, `TWIN_VERSION` 2 :92) | `copies_with` (:224), `counts_with` (:418), `par2_sizes` (:469), `filled_nzb` (:527). **`posted_names` has no writer or reader anywhere in src/** (only mention: duplicates.rs:39) |
| **release_probe** (src/probe/mod.rs:23-47) | `release_id PK, crc32 INTEGER, agent TEXT, info TEXT NOT NULL (JSON)`; added `moviehash TEXT` | `release_probe_crc32`, `release_probe_agent`, `release_probe_moviehash` (partial, not null) | `SET_PROBE` insert-or-replace (:49-50) from `ProbeStep::run` (src/probe/step.rs:102-227, :205) | `probe::read` (:142-148) decodes the whole JSON per page open; field search on crc32/agent/moviehash |
| **release_opensubtitles** (src/probe/opensubtitles.rs:27-48) | `release_id PK, moviehash NOT NULL, imdb_id, tmdb_id, kind, title, year, season, episode, release_name, checked_at` | `release_opensubtitles_imdb` | `SET_MATCH`/`SET_IMDB` (:50-55), StepId(5) and StepId(35) | `read()` -> `Stored {moviehash, feature}` (:280-309) for the probe panel |
| **predb_match** (src/predb/step.rs:25-38) | `release_id PK, title_id, kind, category, pre_at, nuked, matched_at` | - | `PREDB_MATCH = StepId(16)` (:63) | naming (PreDb source 100), results |
| **predb_inner_key** (src/predb/step.rs:42-52) | `key, release_id` PK + index | - | predb step | predb step |
| **release_category** (src/category/step.rs:13-21) | `release_id PK, category` | `category_release` | StepId(24) rank 400 | `category::categories_of` (src/category/mod.rs:21-47) for the Category fact and API |
| **media_release** (src/media/steps.rs:25-76) | `release_id PK, kind, title_key, season, episode, episode_last, air_date, year, quality, tvdb_id, tvmaze_id, trakt_id, tmdb_id, imdb_id, tvrage_id, imdb_from_nfo` | 7 indexes (:26-75) | `UPSERT` (:78-92) by StepId(32) MediaParse, StepId(33) MediaLookup (rank 510, `config_stamp = provider_bits()` :270-272), StepId(34) MediaNfo | `query::attrs_with` -> `MediaAttrs {kind, season, episode, air_date, year, ids}` (src/media/query.rs:315-331), `xml()` newznab attrs (:378); `id_links` on the page (src/web.rs:1790) |

Step id -> rank bands (src/postprocess/steps.rs:10-26 and its test): 1..=7 -> 100s (PAR2 1, List 2, Probe 4 rank 120, OpenSubtitles 5), 8..=15 -> 200s (Naming 8, NFO 9), 16..=23 -> 300s (PreDB 16), 24..=31 -> 400s (Category 24), 32..=39 -> 500s (Media 32/33/34, OpenSubtitles-imdb 35).

### 2.3 `release_probe.info` JSON shape (the one place most catalog fields live)

`ProbeInfo` (src/probe/mod.rs:52-78): `file: Option<String>` (the Subject-derived main file), `yenc: Option<YencLines>`, `file_crc32: Option<u32>`, `segment_size: Option<u64>`, `overhead_percent: Option<f64>`, `continuity_ok/part_size_ok/part_crc_ok: Option<bool>`, `health: BTreeMap<String,bool>` (first/middle/last), `headers: BTreeMap<String,String>` (the 12 `KEPT_HEADERS` :81-94, values cut to 512 chars :100), `crossposted_to: Vec<String>`, `archive: Option<ArchiveHead>`, `layers: Vec<Layer>` (`serde(default)` :70-71), `media: Option<MediaInfo>`, `nfo: Option<String>` (<= `MAX_NFO` 256 KiB src/probe/step.rs:22, :128), `imdb_ids: Vec<String>` (<= 16, :164), `sfv: BTreeMap<String,u32>`, `moviehash: Option<String>`.
- `YencLines {name, file_size, line_len, part, total, begin, end, yend_size, pcrc32, crc32}` (src/probe/yenc.rs:7-18) - first article of the main file; the last article's lines are consumed by `derive` (mod.rs:118-140) and dropped.
- `ArchiveHead {format, encrypted, headers_encrypted, method, name, unpacked, packed, crc32}` plus `serde(skip)` `stored_at, continues, data_at, dir` (src/probe/archive.rs:12-35) - never persisted.
- `Layer {format, method, name, unpacked, offset, stored, encrypted, stop, capped}` plus skipped `continues` (src/probe/nest.rs:25-50).
- `MediaInfo {container, muxing_app, writing_app, title, duration_secs, video, audio, subtitles, chapters}` plus `serde(skip)` `header_complete` (src/probe/media.rs:8-24); `Chapter {start_secs, name}` (:28-31); `VideoTrack {codec, width, height, display_width, display_height, frame_rate, bit_depth, hdr, primaries, transfer, dv_profile, dv_level, dv_enhancement_layer, language}` (:35-51); `AudioTrack {codec, channels, sample_rate, language, name, atmos, commentary}` (:55-65); `SubtitleTrack {codec, language, name, forced, hearing_impaired, commentary}` (:69-76).
`probe::read` returns `None` if the JSON fails to parse (`serde_json::from_str(&text).ok()` mod.rs:147) - a new non-`Option` field without `#[serde(default)]` would blank the probe panel for every existing row.

### 2.4 Main-database tables

- `meta (key TEXT PRIMARY KEY, value INTEGER)` with `next_seq` (src/store.rs:113-120); id blocks `ID_BLOCK` 10 000 (:263-291).
- `groups (name PK, live_cursor, backfill_cursor)` + added `first_article, last_article, low_article, low_posted, high_article, high_posted` (src/db.rs:328-342); group state functions (:585-690).
- `backfill_chunks (grp, day, state, server, claimed_at, done_at, PK(grp, day))` WITHOUT ROWID, `backfill_deepest (grp PK, server, oldest_at, runner_up_at)`, `backfill_sweeps`, `backfill_sweep_gen`, `backfill_speculative (grp, runner_up_day, reach, days, articles)` (src/chunks.rs:20-50); `add_column` helper tolerant of races (:58-69); `CLAIM_TIMEOUT` 30 min (:8).
- Legacy single-file layout: `articles` table and `migrate_old` (src/db.rs:432-534), converted into shards by src/convert.rs (`BATCH` 25 000, `READERS` 8, staging `segments_load` :467).

### 2.5 Side databases

- `atlas.details.db`: `details (kind, tmdb_id, title, overview, genres, runtime, status, network, cast_names, released, bluray, poster, tmdb_score, imdb_score, rotten, metacritic, trakt_score, fetched_at, PK(kind, tmdb_id))` WITHOUT ROWID and `details_lookups (lookup PK, tmdb_id, checked_at)` (src/media/details.rs:44-69), `backdrop` added (:108). Read by `for_title` (:441), `kept_for` (:486), `poster` (:551), `backdrop` (:557). There is **no `release_cover` table and no `posters` table** on main; posters are files under `app_dir()/posters` (:533). `release_cover` belongs to PR #48 (coordination.md:6).
- `atlas.media.db`: `media_titles` (src/media/store.rs:9-24).
- `atlas.predb.db`: titles (src/predb/step.rs:92-100).

---

## 3. Sealing, blobs and the sealed log (why per-article data cannot live on `segments`)

- A file's `segments` rows are sealed into one zstd blob in `files.blob` once the file is complete or untouched for `SEAL_AGE = 3*86_400` (src/store.rs:788, :1136-1139), by `seal_rows` (:836-925: `update files set blob = ?` then `delete from segments where file_id = ?`). Caps: `SEAL_MAX_SEGMENTS` 50 000 (:793; files with more rows never seal :800-805, :843) and `READ_MAX_SEGMENTS` 1 000 000 when decoding (:798, :819). The writer seals `SEAL_PER_TICK` 2000 under `SEAL_BUDGET` 100 ms, walking every `SEAL_WALK_EVERY` 60 s (:1250-1256, `seal_some` :1292, `sealable` :1082).
- Blob v1 (src/blob.rs:1-5 doc; `Seg {part, bytes, domain, local}` :14-19; `encode` zstd level 6 :56; `decode` :87; `count` :92; `decode_capped` :119 with `MAX_SEG_RAW` 1024 :113): per segment a varint `part+1`, zigzag `bytes` delta, `domain`, `local`. **Nothing else survives sealing.** Late articles become rows again and are merged on read (`file_segments` :1905; test :3847).
- Offload: `sealed_store::offload_batch` (src/sealed_store.rs:110-157) ships blobs >= `SMALL_BLOB` 1024 bytes in batches of `BATCH_FILES` 2000 / `BATCH_BYTES` 32 MB to the sealed-log service and `swap_in` replaces each blob with its 21-byte locator (:162-198); `blob::resolved` (src/blob.rs:74) fetches transparently; `is_remote` (:82). Config `SealedLogConfig {addr, offload, offload_mb_per_second = 50}` (src/config.rs:433-445); `spawn_offload` (src/bg_indexer.rs:1114). Compaction refuses a shard holding locators (`has_places` src/compact.rs:462).
- Consequence for the feature: a per-article field (segment CRC, per-part size check, per-article date or From) needs its own table keyed by `(file_id, part)` or a blob format v2 (new version byte in `encode`/`decode`, plus compaction's re-encode path src/compact.rs:519-678 and the sealed-log copies). The catalog's storage section says the same (catalog-current.md:1179-1194).

---

## 4. Compaction and what it refuses

`compact::run` (src/compact.rs:336) -> `compact_shard` (:418-797) per shard, `PARALLEL` 4, `CHECK_SAMPLE` 300, `BATCH` 500 000 (:36-40). It runs `migrate_shard` on the original (:442), refuses unknown tables/columns (`refuse_what_wouldnt_be_copied` :900-927), bails if sealed-log locators exist (:462), copies `releases` through `copied_columns` which bails on an unknown column (:883-897), rewrites `files` with the fixed `FILE_COLUMNS` list and an 11-placeholder insert (:866-867, :519, :678), re-seals due files, copies `release_ids`/`collections`, then **every `SHARD_TABLES` table by name** (:745), rebuilds FTS, runs `build_shard`, checks, and swaps `.precompact` in. `CORE_TABLES` (:871-879: files, segments, domains, held_back, release_ids, meta, collections) carries exact column lists: "A new column on one of them would vanish, soo it stops the shard". Writers are held off by `hold_off_compaction`/`WriteGuard`/`Lock` (:139-192). `auto_run_compact` defaults false (src/config.rs:152, :707).
=> Any new column on a core table must be added in three places: `build_shard`, `migrate_shard` and `CORE_TABLES`/`FILE_COLUMNS` (+ the insert at :678 for files). A new registered `ShardTable` needs nothing in compact.rs.

---

## 5. How a schema migration is added (recipe, from the code)

1. New per-release table: declare `pub const X: ShardTable = ShardTable { name, create: "create table if not exists ...", indexes: &[(name, "create index if not exists ...")], release_key: Some("release_id"), added: &[] }` (pattern: src/probe/mod.rs:23-47) and append it to `SHARD_TABLES` (src/postprocess/schema.rs:78-92). `migrate` creates it on the next open without rewriting the shard (tests/pp_schema.rs:42 "an_old_shard_gets_the_new_tables_without_a_rewrite", page growth bound `4*objects+8` at :80), installs `gc_<table>` (schema.rs:124; tests/pp_schema.rs:84), compaction copies it (compact.rs:745).
2. New nullable column on a registered table: append `("col", "TYPE")` to its `added` (examples: `release_probe.moviehash` probe/mod.rs:46; `release_fingerprint.par2_block/par2_blocks/posted_names` duplicates.rs:39; `archive_release.encryption/names_hidden/password_check` archive/store.rs:50). `add_missing` (schema.rs:102) runs `alter table add column`, no rewrite. Partial indexes `where col is not null` keep the index small (probe/mod.rs:34-42).
3. New column on `releases`: append to `RELEASE_COLUMNS` (schema.rs:96) and check `copied_columns` in compact.rs:883-897 accepts it (open question 4 below).
4. New column on `files`/`segments`: `build_shard` + `migrate_shard` (`pragma table_info` pattern src/store.rs:239-245) + `CORE_TABLES`/`FILE_COLUMNS` + the compaction insert (compact.rs:678). Per-article columns on `segments` are lost at sealing (section 3) - do not.
5. New per-article table: WITHOUT ROWID keyed `(file_id, part)` or `(release_id, file_seq, part)`; a `gc_` trigger only fires on `releases` deletes, so key it so the trigger (or `delete_release`) can clear it; the purge path (`purge::apply_shard` src/postprocess/purge.rs:330-430, `PURGE_SEGMENT_CHUNK` 20 000 :20-24) deletes files/segments in chunks and would need to know the table.
6. Main-db columns: `chunks::add_column` (src/chunks.rs:58-69) or the `groups` pattern (src/db.rs:337-342).
7. Writes from a step go through `StepWrite {sql: &'static str, params, late_params}` (src/postprocess/step.rs:225) inside `Outcome {matched, writes, name, rerun, facts, wait_until, hold}` (:206), applied per release in a savepoint by `apply_one`/`write` (src/postprocess/apply.rs:65-250) on the shard's writer thread.

---

## 6. The `config_stamp` re-probe / backfill pattern (how a deploy recomputes per-release data)

- `Step::config_stamp() -> u64` default 0 (src/postprocess/step.rs:60-62): "Inputs that can change while enabled stays true, one bit each. A bit that wasnt there before redoes this step on existing releases."
- `Stage::start` -> `inner.seed(&p.db)` (src/postprocess/mod.rs:84); `set_config` re-seeds (:109-112). `engine.seed` (src/postprocess/engine.rs:286-307) gathers `(id, mask, config_stamp)` of enabled steps and runs `seed_shard` on each shard writer.
- `seed_shard` (engine.rs:1073-1113): first run -> `pp_walks('backlog', redo 0)` + `pp_walk_seeded`; newly enabled step bits (`enabled & !pp_steps_seen`) -> `walk("steps", 0)`; for every step, if the stored `pp_stamp_{id}` lacks a bit the new stamp has (`stamp & !s != 0`, :1106) -> `walk("steps", bit)` with `top_id = before_id = coalesce(max(id),-1)+1` (:1084), i.e. the whole shard newest-first, then store the stamp.
- `walk_tick` (engine.rs:412-500) pops `WALK_BATCH` 256 (:28) releases per tick under a token bucket of `backlog_per_second / SHARDS` (:421-426), `select ... from releases r left join pp_state s ... where r.id < ?1 ... order by r.id desc limit ?2` (:437), advances `before_id/seen/ran`, deletes the walk when empty (:453). `redo_mask` makes `visit` re-run just those steps (`visit_all` :522).
- Examples: `ProbeStep::config_stamp() = 1 | 2` (src/probe/step.rs:99-101; bit 1 chapters/commentary, bit 2 archives inside archives), with `Reruns {on_rename: false, on_growth: true}` (:94) so a release that grows is re-probed; `MediaLookup::config_stamp = provider_bits()` (src/media/steps.rs:270-272).
- Other backfill triggers: `atlas --reprocess [--step NAME]... [--group NAME]... [--failed] [--apply]` (src/main.rs:36-37; `reprocess::apply` inserts `pp_walks kind='reprocess'` with `redo_mask`, src/postprocess/reprocess.rs:25-42); `rerun_on` (mod.rs:64-80); the naming walk (`naming::walk`, cursor in `meta`, `BATCH` 2000, `SHARDS_AT_ONCE` 4, WAL checkpoint every 64 commits with `WAL_LIMIT` 4 GiB, src/postprocess/batch.rs:8-9); `forget_old_twins` bumps `TWIN_VERSION` and queues a PAR2 re-run (src/duplicates.rs:92-123).
- Cost model: a DB-only step (no bodies) over 118M releases is bounded by `backlog_per_second` and the single writer per shard; the README's bulk passes give the order of magnitude (2-10 min and <= 12 GB WAL for a one-column flag over 118M rows, README.md:512). A step that needs bodies (`Needs::Bodies`, probe) is bounded by `body_fetches_per_minute` 600 and `bodies_per_release` 8 (src/config.rs:606-634) and the body gates; the first pass over ~15M eligible releases is quoted at ~78 days at 300 bodies/min (README.md:405), so a stamp bump that forces body re-fetches is a months-long backfill, while re-deriving new facts from the stored `release_probe.info` JSON (a DB step that parses the JSON and fills new columns/tables) is cheap and should be the backfill path wherever the raw data was already kept. `STEP_DEADLINE` 120 s per step call (engine.rs:44).

---

## 7. Where the web UI shows storage (server-rendered htmx, src/web.rs)

Routes (`get()` src/web.rs:58-69): `/ui/results`, `/ui/release`, `/ui/duplicates`, `/ui/details`, `/ui/poster`, `/ui/nzb` (`fill=1` -> `duplicates::filled_nzb`).

`release_page` (:989) -> `release_html` (:1028-1202) renders a `<dl>` of facts:
Category (:1055, from `release_category`), Title (:1059), Size (:1062, `releases.size`), Posted (:1067, `posted_date`), Groups/Group (:1075/:1077, `release_info::groups_of_with` src/release_info.rs:216), Poster + Email with MADE UP tag (:1081-1084, `parse_poster` release_info.rs:451, `looks_made_up` :501), Posted as / Name from (:1089/:1092, `Names` + `name_source_label` release_info.rs:16-37, :136), Year/Ripped from/Site (:1096-1102, `release_tags::parse` of the name), Video/Audio/Languages/Edition/Release group (:1104-1120, name tags), Parts (:1123, `releases.parts`), Encryption (:1125, `encryption_fact` :1763 from `archive_release`), Password (:1127, `password_fact` :1741 + `password_hint_with` release_info.rs:539, verdict against `password_check` computed at render), Files / PAR2 files / Articles / Repair (:1130-1138 from `release_info::Contents` computed at page open, release_info.rs:39, :152-160, `health` :1779), Links (:1141, `id_links` :1790 from `media_release` ids).
Then "Files inside" table from `archive_inner_files` (:1159), the probe panel `probe_html(&found, matched)` (:1168, data from `probe::read` + `opensubtitles::read`), the copies panel `copies_html` (:1181, `duplicates::copies_with`), and the htmx-loaded `<div id="title-details" hx-get="/ui/details?id=...">` (:1190 -> `details_panel` :1657 -> `details_html` :1679 from `atlas.details.db`; `poster` :1717 serves the image cache).

`probe_html` (:1243-1454) panels and the JSON fields they read:
- yEnc: File name `y.name` (:1252), File size `y.file_size` (:1253), Parts `y.total` (:1260), Line length `y.line_len` (:1261), Article size `p.segment_size` (:1262), CRC32 `p.file_crc32` as a search link (:1263), Overhead `p.overhead_percent` (:1266), Checks `continuity_ok/part_size_ok/part_crc_ok` verdict tags (:1272-1274), On the server `p.health` (:1279).
- How it was posted: Posted with `agent_of(&p.headers)` (:1291), every other kept header (:1294, UA/X-Newsreader/X-Newsposter/Xref excluded), Crossposted to `p.crossposted_to` (:1299-1300).
- Packing: Format (:1306), Method (:1307), lock tag from `headers_encrypted/encrypted` (:1308-1312), First file `a.name` (:1316), Unpacked (:1317), File CRC32 `a.crc32` (:1324), Layers via `nested_chain` (:1326-1327; :1840-1855 prints `format`, `method`, `stop` and the last layer's `name` only).
- Tracks: Container, Written by, Muxed by, Title, Duration (:1331-1339); video `width x height`, `frame_rate`, `bit_depth`, `hdr`, `dv_profile` + `dv_enhancement_layer` (:1346-1357); audio `codec, channels, sample_rate, language, atmos, commentary, name` (:1367-1377); subtitles `codec, language, forced, hearing_impaired, commentary, name` (:1382-1393); OpenSubtitles hash + match (:1398-1402).
- Chapters (:1416-1420), IMDb ids (:1426), NFO text (:1430), SFV (:1433-1437).
**Not rendered anywhere in :1243-1460** (stored in JSON only): `p.file`, `yenc.part`, `yenc.begin`, `yenc.end`, `yenc.yend_size`, `yenc.pcrc32`, `archive.packed`, layer `offset/stored/unpacked/encrypted/capped`, video `display_width/display_height/primaries/transfer/dv_level/language`.

Results list: `row()` (:744-797) with `RowExtras {split, copies, details}` (:684-690) built from `categories_of`, passwords and `attrs_of` (~:567-571). `duplicates_page` (:1575) and `copy_card` (:1463) show twins/alts.

API (src/api.rs): `handle` (:131), `release_attrs` (:285-310) emits newznab attrs poster, group, resolution, video, audio, hdr, source, language, team, twins, alts; `rss` (:313-366) adds category, password code and the `media_release` attrs via `MediaAttrs::xml` (:334); `get_nzb` (:368). The probe JSON is never exposed by the API.

NZB (src/nzb.rs:65-125): `<file poster date subject>` from `releases.poster/posted_date` and `files.subject` (:85-88), `<segment bytes number>` message-ids from `each_article`.

Dashboards: `stats_dashboard::load_quick` (src/stats_dashboard.rs:113-125) and `metrics/catalog.rs` read `store::totals` and per-file `DB_BYTES`; the retention horizon (`backfill_deepest`, `groups.low_posted`) is a TUI/dashboard fact, not a release-page one.

---

## 8. Catalog items whose `atlas:` note names a table or column - visibility from the code

Key = catalog section.item (catalog line). Status vocabulary: shown / stored_not_shown / parsed_not_stored / not_collected / unclear.

| Item (catalog line) | Status | Evidence |
|---|---|---|
| nntp_session.group_counts (42) | stored_not_shown | `groups.first_article/last_article` main db (src/db.rs:328-342); no release-page reader; stats TUI only |
| nntp_session.article_presence (51) | shown | `release_probe.info.health` -> "On the server" (web.rs:1279) |
| nntp_session.retention_horizon (54) | stored_not_shown | `backfill_deepest.oldest_at/runner_up_at` (src/chunks.rs:29-34), `groups.low_posted` (db.rs:337-342); dashboard/TUI only, nothing in release_html :1028-1202 |
| overview_row.number (66) | parsed_not_stored | `segments` has no article number (store.rs:181-188; spec :7); `Overview.number` drives cursors (src/nntp.rs:91-100) |
| overview_row.subject (70) | stored_not_shown | `files.subject` (store.rs:165-177) goes into the NZB (nzb.rs:85-88); the page shows the parsed "Posted as" name (web.rs:1089), not the subject |
| overview_row.from (74) | shown | `releases.poster` -> Poster/Email facts (web.rs:1081-1084); rewritten on each merge (store.rs:1394) |
| overview_row.date (78) | shown | `releases.posted_date` -> Posted (web.rs:1067); raw Date/Injection-Date in the headers panel (:1294) |
| overview_row.message_id (82) | stored_not_shown | `segments(local, domain)` + `domains` (store.rs:180-188); only emitted inside the NZB via `each_article` (store.rs:1947) |
| overview_row.bytes (90) | shown (aggregate only) | `segments.bytes` summed into `releases.size` -> Size fact (web.rs:1062) and NZB `bytes=`; per-article bytes not shown |
| overview_row.xref (98) | shown | `info.crossposted_to` -> Crossposted to (web.rs:1299-1300); `headers.Xref` itself is skipped at :1294 |
| headers_mandatory.Path (114) | shown | `info.headers` loop (web.rs:1294), value cut to 512 chars (probe/mod.rs:100) |
| headers_optional_netnews.Injection-Date (119) | shown | same loop (web.rs:1294) |
| headers_optional_netnews.User-Agent (128) | shown | `release_probe.agent` -> Posted with (web.rs:1291; `agent_of` probe/mod.rs:107) |
| headers_poster_tool.X-Newsreader (231) | shown | folded into `agent` (probe/mod.rs:107-108), Posted with (web.rs:1291) |
| ybegin.name (321) | shown | `yenc.name` -> File name (web.rs:1252) |
| ybegin.size (324) | shown | `yenc.file_size` -> File size (web.rs:1253) |
| ybegin.line (327) | shown | `yenc.line_len` -> Line length (web.rs:1261) |
| ybegin.part (330) | stored_not_shown | `yenc.part` (probe/yenc.rs:7-18) has no reader in web.rs:1243-1460; `yenc.total` is shown as Parts (:1260) |
| ypart.begin / ypart.end (338/341) | stored_not_shown | `yenc.begin/end` persisted, no reader in probe_html; derived `segment_size` shown (web.rs:1262) |
| yend.size (346) | stored_not_shown | `yenc.yend_size` persisted, unread; `part_size_ok` verdict shown (web.rs:1273) |
| yend.pcrc32 (353) | stored_not_shown | `yenc.pcrc32` (first article only) persisted, unread; `part_crc_ok` verdict shown (web.rs:1274); nothing per article (blob.rs:14-19) |
| yend.crc32 (356) | shown | `release_probe.crc32` indexed (probe/mod.rs:27, :34) and `info.file_crc32` -> CRC32 link (web.rs:1263) |
| encoding_quirks.segment_size (370) | shown | Article size (web.rs:1262) |
| encoding_quirks.overhead (373) | shown | Overhead (web.rs:1266) |
| encoding_quirks.filename_mismatch (385) | stored_not_shown | `info.file` (Subject name) is never rendered (no `p.file` in web.rs:1243-1460); `yenc.name` (:1252) and `archive.name` (:1316) are, side by side, uncompared |
| decoded_payload.magic (404) | shown | `archive.format` -> Format (web.rs:1306), `media.container` -> Container (:1331-1332) |
| decoded_payload.full_file_crc32_md5 (410) | shown (CRCs) / parsed_not_stored (MD5s) | yEnc crc32 (web.rs:1263), archive crc32 (:1324), SFV (:1433-1437); PAR2 MD5s only folded into `release_fingerprint.posted` (duplicates.rs:42) |
| decoded_payload.opensubtitles_moviehash (413) | shown | `release_probe.moviehash` + `release_opensubtitles` -> hash and match (web.rs:1398-1402) |
| derived_per_article.subject_parse (466) | stored_not_shown | `files.filename/subject/expected/file_total` (store.rs:165-177) are not listed on the page (the "Files inside" table is `archive_inner_files`, web.rs:1159); only counts via `Contents` (:1130-1138) |
| derived_per_article.part_bitmap (472) | shown (aggregate) | `files.seen/expected` -> Articles health (web.rs:1136, `health` :1779) |
| derived_per_article.message_id_grammar (475) | stored_not_shown | `segments.local/domain`, `domains.suffix` (store.rs:180-188); no reader on the page or in search |
| derived_per_article.from_identity (481) | shown | Poster/Email + MADE UP (web.rs:1081-1084) |
| derived_per_article.date_shape (484) | shown | Posted (web.rs:1067) + raw Date/Injection-Date/NNTP-Posting-Date (:1294) |
| derived_per_article.arrival_curve (496) | parsed_not_stored | `backfill_chunks` keeps grp/day/state/server only (chunks.rs:20-28); nothing per release |
| derived_per_article.crosspost_kind (499) | shown | Groups fact (web.rs:1075, `groups_of_with` release_info.rs:216) + Crossposted to (:1299) + twins (:1181) |
| derived_per_post.contents_profile (504) | shown (computed at page open) | `release_info::Contents` (release_info.rs:39, :152-160, module doc :2 "nothing here is stored") -> Files/PAR2 files/Repair (web.rs:1130-1138); `release_fingerprint.par2_block/par2_blocks` via `par2_sizes` (duplicates.rs:469) |
| derived_per_post.obfuscation_level (507) | shown | Name from (`name_source`, web.rs:1092), Encryption (:1125), Password (:1127); `is_obfuscated` feeds `Names` (release_info.rs:136) |
| derived_per_post.completeness_over_time (513) | shown (aggregate) / not_collected (history) | `files.seen/expected` -> health (web.rs:1136); `files.touched_at` is only a seal timer (store.rs:632-640, :1139) |
| derived_per_post.takedown_pattern (516) | shown | `info.health` first/middle/last (web.rs:1279); undated |
| poster_profile.stable_keys (524) | shown (headers) / stored_not_shown (domain) | headers panel (web.rs:1294); `domains` never surfaced |
| poster_profile.habit_keys (527) | shown (per release) | Article size (web.rs:1262), Repair (:1138), Password (:1127), Release group (:1120); no aggregation table exists |
| nzb.head.meta (538) | shown on page, absent in NZB | Password (web.rs:1127), Links/ids (:1141); NZB writer emits only category (nzb.rs:65-125) |
| nzb.file.poster (541) | shown | Poster fact (web.rs:1081); NZB `poster=` (nzb.rs:85) |
| nzb.file.date (544) | shown | Posted (web.rs:1067); NZB `date=` (nzb.rs:85) |
| nzb.groups.group (550) | shown | Group fact (web.rs:1077) |
| nzb.nzb_identity (559) | shown (as copies) | `release_fingerprint.posted/content` -> copies panel (web.rs:1181, `copies_with` duplicates.rs:224); the hashes themselves are not printed |
| derived_additions.repost_and_retry_events (572) | parsed_not_stored | duplicates dropped by `files.seen` (store.rs:538-559), never counted |
| derived_additions.nntmux_collection_key (584) | stored_not_shown | `collections (key, release_id)` (store.rs:207); no page reader |
| derived_additions.newznab_attrs_and_endpoints_not_emitted (608) | shown (page) | the attrs mirror page facts (web.rs:1055-1141); `release_attrs` api.rs:285-310, `rss` :313-366 |
| derived_additions.last_article_bytes_key (620) | stored_not_shown | `segments.bytes` per article (store.rs:181-188), only summed |
| par2.Main (675) | shown (block size/count) / parsed_not_stored (file ids) | `release_fingerprint.par2_block/par2_blocks` (duplicates.rs:39, :44) -> Repair fact (web.rs:1138, :1752) |
| par2.FileDesc (678) | parsed_not_stored (md5, md5_16k) | name/size stored as `archive_inner_files` source 1 (archive/store.rs:13, :18-37) and shown (web.rs:1159); MD5s only hashed into `posted` (duplicates.rs:42) |
| par2.UniFileN (681) | shown | replaces the name in `archive_inner_files` -> Files inside (web.rs:1159) |
| par2.set_id (699) | parsed_not_stored | input to `release_fingerprint.posted`, not a column (duplicates.rs:21-39) |
| sfv.entries (707) | shown | `info.sfv` -> SFV panel (web.rs:1433-1437) |
| nfo.ids_and_urls (717) | shown | `info.imdb_ids` (web.rs:1426); `media_release.imdb_from_nfo` -> Links (:1141, :1790) |
| nfo.template_fields (720) | shown (raw text) | `info.nfo` (web.rs:1430); not parsed into fields |
| nfo.password (722) | shown | `release_nfo_password` -> Password fact (web.rs:1127, :1741; release_info.rs:539) |
| rar4.main_header_0x73 (741) | shown | `archive_release.encryption/names_hidden` -> Encryption fact (web.rs:1125, :1763) |
| rar4.file_header_0x74 (744) | shown (crc/method/unpacked) / stored_not_shown (packed) | web.rs:1307, :1317, :1324; `archive.packed` (probe/archive.rs:12-35) has no reader |
| rar4.volume_scheme (753) | parsed_not_stored | `stored_at`, `continues` are `serde(skip)` (probe/archive.rs:12-35) |
| rar5.encryption_header_type_4 (761) | stored_not_shown | `archive_release.password_check` (archive/store.rs:50) read only to verify a password at render (web.rs:1741); salt/kdf never printed |
| rar5.file_header_type_2 (764) | shown (crc/method/unpacked) | web.rs:1307 (m0..m5), :1317, :1324 |
| rar5.file_extra_records (767) | stored_not_shown | same `password_check` blob |
| rar5.packer_fingerprint (776) | shown (method only) | Method (web.rs:1307) |
| zip.local_header (795) | shown | Method/File CRC32 (web.rs:1307, :1324) |
| zip.central_directory_and_end (801) | shown | names/sizes in `archive_inner_files` -> Files inside (web.rs:1159) |
| archive_common.alt_key (806) | shown (as alts) | `release_fingerprint.content` -> copies panel alts (web.rs:1181); hash not printed |
| archive_common.per_creation_random (809) | stored_not_shown | `password_check` only; 7z header offsets parsed_not_stored; MKV SegmentUID not_collected (coordination.md:7: stream-details branch adds it) |
| archive_common.inner_listing (812) | shown | Files inside (web.rs:1159), read capped at 256 rows (archive/store.rs:90); per-file CRC only for the probed first entry (:1324) |
| archive_common.password_state (815) | shown | Encryption + Password facts (web.rs:1125-1127); verdict computed at render, not stored |
| payload_additions.password_checks_as_keys (865) | stored_not_shown | `archive_release.password_check`; no password table, no index |
| payload_additions.pack_containment (905) | not_collected | `release_fingerprint.content` is per release only (duplicates.rs:47) |
| matroska.Segment.Info (947) | shown (title, duration, muxing/writing app) / not_collected (SegmentUID, DateUTC) | web.rs:1333-1339; `MediaInfo` has no uid/date field (probe/media.rs:8-24) |
| matroska.Tracks.Video (953) | stored_not_shown (display size, primaries, transfer, dv_level, language) | `VideoTrack` fields (probe/media.rs:35-51); web.rs:1346-1357 reads only width/height/frame_rate/bit_depth/hdr/dv_profile/dv_enhancement_layer; PR #48 claims these (coordination.md:6) |
| matroska.structure (968) | parsed_not_stored | `header_complete` is `serde(skip)` (probe/media.rs:8-24; probe/step.rs:231) |
| content_identity.moviehash (1052) | shown | web.rs:1398 |
| media_additions.remux_invariant_alt_key (1080) | not_collected | `release_fingerprint.content` hashes sizes only (duplicates.rs:47) |

Also relevant though the note names no table: matroska.Tracks.TrackEntry (950) - `default_duration`, flags and DV fields are in the JSON; FlagDefault is not a field (coordination.md:7 says the stream-details branch adds `default`).

---

## 9. Row counts and the cost of a per-article table

Per release today (shards): 1 `releases` row; ~0.66 `files` rows on average (78M/118M) but media releases carry 10-100 files; ~15 `segments` rows or sealed entries on average (1.77B/118M), thousands for a large release; 1 `pp_state` row once visited; 0-1 rows in each per-release step table; `archive_inner_files` up to the name quota (read cap 256, archive/store.rs:90). Per-article today: exactly one `segments` row or one blob entry (part, bytes, domain, local), 15-27 bytes sealed (spec :137).

A new `segment_crc (file_id, part, crc32, size)` WITHOUT ROWID row costs roughly 20-30 bytes with b-tree overhead. Filled for every article (~1.77B) that is 35-55 GB, about the size of the whole sealed segment store after compaction (~60 GB, spec :137) - and it needs one body fetch per article, which the fetch budget (600 bodies/min, config.rs:606-634) makes impossible (1.77B / 600 per min = 5600 years). Filled only from the articles the probe already decodes (first, last, and up to `FRONT_ARTICLES` 6 front articles for media, probe/step.rs:217, :231; `bodies_per_release` 8) over the ~15M body-eligible releases, that is 30-120M rows, 1-3 GB, and it can be back-filled from existing data only where the yEnc lines were kept (today only the first article's pcrc32 is in `release_probe.info.yenc.pcrc32`; the last article's is dropped in `derive`, probe/mod.rs:118-140), so a real backfill of two CRCs per release means re-fetching two bodies for 15M releases: ~35 days at 600/min.

A per-release JSON append (new fields on `ProbeInfo`) costs only the JSON bytes (`info` is `TEXT NOT NULL`, read whole, probe/mod.rs:22) and no migration, but it is not searchable; a column in `added` plus a partial index is the searchable form (probe/mod.rs:34-46).

---

## 10. Extension points for "every field on every release page"

1. A new fact or panel = a function in src/web.rs appended inside `release_html` (:1028-1202), reading through `with_reader` (db.rs:86) and `shard_of_id` (store.rs:44); the other threads agreed on "one call appended inside release_html, own shard tables" (coordination.md:4).
2. New per-release fields that are already parsed: add to `ProbeInfo`/`MediaInfo`/`ArchiveHead` with `#[serde(default)]` (pattern probe/mod.rs:70-71), render in `probe_html`, and bump a `config_stamp` bit on `ProbeStep` (probe/step.rs:99-101) to re-probe; or stop `serde(skip)`ping fields that are already parsed (`stored_at/continues/data_at/dir` probe/archive.rs:12-35; `header_complete` media.rs:8-24).
3. New searchable per-release columns: `added` on `release_probe`/`release_fingerprint`/`archive_release` (schema.rs:102 `add_missing`), or a new `ShardTable` in `SHARD_TABLES` (schema.rs:78-92).
4. New per-article data: a new `ShardTable` keyed `(file_id, part)` (never `segments`, section 3); writes through `StepWrite` (step.rs:225) from a `Needs::Bodies` step; `gc_` trigger needs a `release_id` column or a join through `files`.
5. Backfill without bodies: a DB-only step that re-reads `release_probe.info` and fills new columns, driven by a `config_stamp` bit (seed_shard engine.rs:1073-1113) or `atlas --reprocess --step NAME --apply` (reprocess.rs:25-42); throttled by `backlog_per_second` (engine.rs:421).
6. Backfill with bodies: the same bit on `ProbeStep`, bounded by `body_fetches_per_minute` (config.rs:606-634) and the body gates (README.md:405).
7. Per-row metadata in the results list: `row()`/`RowExtras` (web.rs:744-797, :684-690), with the Size cell owned by the download-size thread (coordination.md:16-17).
8. API: `release_attrs`/`rss` (api.rs:285-366) if the fields should also go out as newznab attrs.

---

## 11. Risks

1. **JSON compatibility**: `probe::read` returns `None` on any deserialisation error (probe/mod.rs:147), so a new non-`Option` field without `#[serde(default)]` blanks the probe panel for every existing release.
2. **Per-article tables are huge**: 1.77B articles; anything keyed per article must be probe-fed (tens of millions of rows), never store-fed (section 9); `segments` columns vanish at sealing (store.rs:836-925) and the blob format v1 keeps four fields (blob.rs:14-19).
3. **Compaction refuses unknown columns on core tables** (compact.rs:871-927). For `releases` and the registered tables, `copied_columns` (:883-897) compares the old shard's `pragma_table_info` with the freshly built, migrated destination, so a column added through `RELEASE_COLUMNS`/`added` is accepted by the build that knows it; an older binary compacting a newer shard stops with "has a column this build doesnt know". Shards with sealed-log locators cannot be compacted at all (:462).
4. **Body-fetch backfills are months long**: 15M eligible releases at 600 bodies/min; a stamp bump that re-fetches bodies competes with live indexing for the same budget (engine.rs:310 snapshot of `body_fetches_per_minute`/`backlog_per_second`).
5. **Single writer per shard**: a DB-only walk over 118M rows writes through the shard writer thread in `WALK_BATCH` 256 slices (engine.rs:28) under the token bucket; WAL growth is the main cost (README.md:512: ~12 GB WAL for one flag column; naming walk checkpoints every 64 commits with a 4 GiB WAL limit, postprocess/batch.rs:8-9).
6. **Read-time computation on the page**: `Contents`, poster parsing, password verdicts and split posts are computed per page open (release_info.rs:2, :152-160, :539); `contents_of` walks every article via `each_article` (store.rs:1947) and `union_files`, so a release with 100k articles is already slow; new panels should read stored rows, not recompute.
7. **PR overlap** (coordination.md): #48 `feat/release-nzb-files` adds `nzb_files_html`, `cover()`/cover art, attachments, and renders every stream field in `probe_html` (display aspect, primaries/transfer names, languages, Atmos/commentary, chapters) and adds `attachments`/`cover` to probe/media.rs; the stacked `feat/release-page-stream-details` adds `hdr_summary`, encoder/key_options, SegmentUID, editions, default flags, runtime_note and `segment_uid/editions/encoder/encoder_options/default` fields; #51/#54 add `src/probe/stills.rs` with shard table `release_stills (release_id, position, at_ms, sha256, width, height)` and `src/video/*`, next a `/ui/still` route. Anything touching `probe_html` :1243-1454 or `MediaInfo`/`VideoTrack` will conflict textually with #48; the agreed split is: this work renders container-level and non-stream facts, not stream/HDR/encoder/chapter/attachment/cover, nor the NZB file list.
8. **Download-size thread** edits `row()` Size cell, the size-sort block, the `fact("Size", ...)` line in `release_html`, alt list `fmt_size`, release_info download-size helper, api.rs size (coordination.md:16); do not touch those lines.
9. **Finish web UI thread's twin grouping** (coordination.md:13) overlaps the copies panel (`copies_html` web.rs:1555, `copy_card` :1463, `duplicates_page` :1575) and `release_fingerprint`; its PRs could not be read (GitHub API access denied in this session), so `segment_crc`/`par2_files` keys should be agreed there (catalog-current.md:1222).
10. **Dead column**: `release_fingerprint.posted_names BLOB` is declared (duplicates.rs:39) with no writer or reader; a new design should either use or drop it (dropping needs a rewrite - SQLite `drop column` on a WITHOUT ROWID-less table is fine, but compaction copies by name).
11. **`gc_` triggers only fire on `releases` deletes** (schema.rs:124); purge (`purge::apply_shard`) and `delete_release` (store.rs:1801-1819) must also know any table keyed by `file_id` rather than `release_id`.
12. **Catalog figure mismatch**: the catalog says `archive_inner_files` keeps at most 64 rows (catalog line 812) while the read path caps at 256 (archive/store.rs:90); the write cap lives in archive/steps.rs and was not re-verified here.

---

## 12. Open questions

1. PR #36 (config_stamp), #48, #51, #54 and the Finish web UI thread's PRs could not be fetched: `gh` returned "GitHub GraphQL is not available from Claude Code sessions" and "GitHub access to this repository is not enabled for this session". The stamp pattern above is read from main (engine.rs:1073-1113, step.rs:60-62), not from the PR.
2. `release_fingerprint.posted_names` - intended use? No code path writes or reads it.
3. `release_cover` and `posters` tables do not exist on main; `posters/` is a directory cache (details.rs:533). Confirm #48's `release_cover` DDL before adding art tables.
4. (Resolved) `copied_columns` (compact.rs:883-897) accepts any `releases`/registered-table column the compacting build itself creates through `schema::migrate`, because it diffs the old shard against the new, migrated one; only core tables need the `CORE_TABLES` list. Deploy order still matters: an old binary must not compact a shard a newer binary migrated.
5. The write cap for `archive_inner_files` (64 per catalog vs 256 read cap) - check src/archive/steps.rs before sizing a `par2_files` table.
6. Whether a blob format v2 (per-article CRC/size inside `files.blob`) is acceptable to the storage owners versus a separate `segment_crc` table; v2 touches `blob::encode/decode` (blob.rs:56, :87), `seal_rows` (store.rs:836), compaction's re-seal (compact.rs:519-678) and the sealed-log copies.
