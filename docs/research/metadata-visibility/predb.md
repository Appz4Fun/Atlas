# Atlas PreDB matching map (area: predb)

Repo `/home/claude/appz4fun/atlas`, branch `main` at 55d2e51 ("Follow archives inside archives, up to 10 layers (#52)"). Read-only survey, nothing built or run. Every claim cites `file:line` of that tree; paths are relative to the repo root unless they start with `/`. Catalog: `/tmp/claude-0/-home-claude/b80d34bc-6432-5cb9-a471-5c997487e01d/scratchpad/catalog-current.md`.

Files read in full: `src/predb/{mod,db,step,keys,matcher,late,import,file_import,feeds,more_feeds,poll}.rs`, `docs/port/06-predb.md`, `src/postprocess/{steps,reprocess}.rs`. Read in part: `src/postprocess/{step,engine,apply,schema,mod}.rs`, `src/naming/{mod,decide,steps}.rs`, `src/release_info.rs`, `src/web.rs`, `src/api.rs`, `src/search.rs`, `src/db.rs`, `src/store.rs`, `src/metrics/{catalog,report,mod,names}.rs`, `src/archive/{mod,steps,store}.rs`, `src/bg_indexer.rs`, `src/stats_dashboard.rs`, `src/config.rs`, `config.example.toml`, `README.md`, `tests/predb_{stage,late,import,...}.rs`, `tests/metrics.rs`, `tests/naming_stage.rs`, and the diffs `main...pr/{48,51,54,57}` and `main...origin/feat/release-stills-strip`.

**Status rule used here** (resolving the vocabulary dispute listed in the task): `shown` only when the item's own value is printed on `/ui/release` or on a panel that page loads (`/ui/details`); a Prometheus gauge, `stats.json`, the TUI dashboard or a CLI printout does not count. `stored_not_shown` = in a shard table, `atlas.predb.db` or a `releases` column with no such print. `parsed_not_stored` = computed in memory and dropped. Aggregates over many rows are separate derived facts, never "shown" for the per-row value.

---

## 1. What the subsystem is

A catalogue of scene/P2P release titles kept in its own SQLite file `atlas.predb.db` next to the shards (`src/predb/db.rs:1`, path = `compact::with_suffix(main, "predb")` `src/predb/db.rs:45-48`, `src/paths.rs:92-94`), filled by polling up to 33 public feeds (`src/predb/feeds.rs:89-297`) or by `atlas --predb-import FILE --apply` (`src/predb/file_import.rs:211-250`), and consumed by one post-processing step, `predb_match` = `StepId(16)`, rank 300, `Needs::Database` (`src/predb/step.rs:63,159-180`). A match renames the release with `NameSource::PreDb = 100` (`src/naming/mod.rs:40`), the highest name source, and writes one row to the shard table `predb_match` (`src/predb/step.rs:25-39`). Everything is off until `predb.enabled` (`src/config.rs:363,378`; `src/predb/step.rs:172-174`; `src/bg_indexer.rs:845-849,863-879`).

Threads, started from `bg_indexer::run_servers` when `config.predb.enabled` (`src/bg_indexer.rs:863-879`): `Service::start` (`src/predb/mod.rs:180-202`) creates the file (`:181-182`), spawns `atlas-predb-feed` (`:192-193`, body `feed_thread` `:87-139`) and, when a stage exists, `atlas-predb-late` (`:194-200`, body `late_thread` `:141-176`). `predb::run` (`:267-272`) sits in the indexer's `JoinSet` so `stop_and_join` (`:257-263`) runs before any compaction (`src/bg_indexer.rs:874-875`). `set_config` swaps the live `PredbConfig` on reload (`src/predb/mod.rs:204-206`; `src/bg_indexer.rs:699-700`); turning `enabled` on or off rebuilds the servers (`src/bg_indexer.rs:666`, test `:1309`).

Config (`src/config.rs:362-429`): `enabled=false`, `sources=[]`, `poll_minutes=5` (1..1440), `page_size=100` (1..100), `max_pages_per_poll=3` (1..20), `max_db_mb=8192` (>=64), `request_delay_ms=1000` (250..60000), `timeout_secs=15` (1..120), `history_pages=0`, `late_per_second=1` (0..1000). Example block `config.example.toml:76-82`. README section `README.md:433-489`.

Docs: `docs/port/06-predb.md` is the design (1041 lines, written against base `de598b6`, so its line numbers are stale); the decisions list `:1008-1041` explains the shape (separate file :1010, no schema in atlas.db :1011, nuked stored and reported only :19, `predb_match` keeps copies of category/pre_at/nuked for Parts 7-8 :1031, title_id "only for reading" :1031).

---

## 2. Data model

### 2.1 `atlas.predb.db` (separate file; never a shard; compaction, conversion and wipe leave it alone, `src/app.rs:599-600`, `README.md:533`)

Schema `src/predb/db.rs:7-43`, created WAL (`:53`), plus a one-off `alter table feeds add column last_rows` migration (`:55-59`). Opened read-only by `open_reader` (`:64-68`: `db::open_at` + `pragma query_only = on`; a missing file is an error, never created).

| table | columns | written by | read by |
|---|---|---|---|
| `titles` | `id INTEGER PK, title TEXT NOT NULL, source INTEGER NOT NULL, category TEXT, size_bytes INTEGER, files INTEGER, pre_at INTEGER, nuked INTEGER NOT NULL DEFAULT 0, nuke_reason TEXT, added_at INTEGER NOT NULL` (`db.rs:8-19`) | `import::import` (`src/predb/import.rs:47-134`): insert `:61-64,91-101`; fill-only-empty `category/size_bytes/files/pre_at` `:66-71,112`; a non-zero `nuked` replaces and `nuke_reason` is kept unless a new one comes `:72-75,113-115`; `source` and `title` never change (`:1,45`); `category` > 64 bytes becomes NULL, `nuke_reason` cut at 256 (`:29-32,82-83`); `valid_title` gate `:78-81` (`keys.rs:42-48`: 8..=255 chars, trimmed, has a letter, no control/`\`/`/`/`\|`/`"`) | `matcher::rows_for_key` (`src/predb/matcher.rs:49-65`, selects `id,title,category,size_bytes,pre_at,nuked`; NOT `files`, `source`, `nuke_reason`); `late::tick_counting` (`src/predb/late.rs:187-194`, `id,title`); `step.rs:202` and `mod.rs:227` (`max(id)`); `metrics/catalog.rs:386-390` (`source` -> `feeds.key` per matched title); `file_import.rs:193-200` (`max(id)`) |
| `title_keys` | `key INTEGER, title_id INTEGER, PK(key,title_id) WITHOUT ROWID` (`db.rs:20-24`); 3 rows per title: `name_key` (SHA-1 of `identity_key(title)`), first 8 bytes of MD5, first 8 bytes of SHA-1 (`src/predb/keys.rs:29-34`) | `import.rs:65,103-105` | `rows_for_key` (`matcher.rs:49-53`, join, `limit 17`); `import.rs:58-60,85-89` (dedupe by lowercased title) |
| `feeds` | `source PK, key, next_poll_at, failures, resume_page, gap_floor, history_page, history_read, last_ok_at, last_error, inserted, updated, last_rows` (`db.rs:25-40`) | `poll::poll_source` (`src/predb/poll.rs:362,415-438`), `set_cursor` (`:182-185`), `add_counts` (`:187-193`), `failed` (`:336-351`), history (`:293,300,307-310`); `import.rs:125-130` (cursor in the same tx) | `mod.rs:117-120` (due), `poll.rs:363-376`, `mod.rs:229-245` (`stats.json`), `metrics/catalog.rs:389` (`key` by `source`) |
| `meta` | `key TEXT PK, value INTEGER`; rows `titles`, `late_after_id` (`db.rs:41-42`) | `import.rs:124` (`titles += inserted`); `late.rs:211` and `file_import.rs:200` (`late_after_id`) | `late.rs:188-189`; `mod.rs:224-228`; `file_import.rs:190-192` |

Size: about 153 B/title, 153 MB per million titles, under 2 MB/day growth (`README.md:332`; measured table `docs/port/06-predb.md:162-170`). Polling refuses once the file passes `max_db_mb` (`poll.rs:386-388`).

### 2.2 Shard tables (registered in `SHARD_TABLES`, `src/postprocess/schema.rs:88-89`; created, `gc_*` trigger installed and `added` columns appended by `schema::migrate` `:114-130`; copied by compaction by registry)

| table | columns | written by | read by |
|---|---|---|---|
| `predb_match` | `release_id INTEGER PK, title_id INTEGER NOT NULL, kind INTEGER NOT NULL, category TEXT, pre_at INTEGER, nuked INTEGER NOT NULL DEFAULT 0, matched_at INTEGER NOT NULL` (`src/predb/step.rs:25-39`) | `MATCH_UPSERT` = `insert or replace ...` (`step.rs:55-57`), queued as a `StepWrite` from `PredbMatch::run` (`:242-254`) with `title_id = titles.id`, `kind = Kind as i64`, `category/pre_at/nuked` copied from the title row at match time, `matched_at = now` (`:237`). Rewritten on every rerun that matches, same title or not, so `matched_at` is "last confirmed", not "first matched". Applied by the shard writer in `apply::write` (`src/postprocess/apply.rs:135-144`). Deleted with the release by trigger `gc_predb_match` (`schema.rs:122-128`) | `step.rs:232-236` (`title_id` of the current row, to decide `rerun`); `metrics/catalog.rs:442-446,460-464` (`kind, category, title_id` into the catalog walk, the ONLY reader outside `src/predb/`). `src/web.rs`, `src/api.rs`, `src/release_info.rs`, `src/search.rs` read none of it (`grep predb src/web.rs src/api.rs` -> empty) |
| `predb_inner_key` | `key INTEGER, release_id INTEGER, PK(key, release_id) WITHOUT ROWID` + index `predb_inner_key_release(release_id)` (`step.rs:42-53`) | `KEYS_CLEAR`/`KEY_ADD` (`step.rs:58-59`): on no match with inner files, the `name_key` of up to `KEYS_KEPT = 4` names from `best_from_paths(inner, Grade::Weak)` (`:211-221`); on a match, cleared (`:255`) | `late::nominate` `INNER_PROBE` (`late.rs:58,146-151`) |

### 2.3 `releases` columns the step writes (through `Outcome.name`, never SQL of its own)

`NameChange { display_name: Some(title), obfuscated: Some(false), source: 100 }` (`step.rs:261-265`) is applied by `apply::write` (`src/postprocess/apply.rs:145-163`: `update releases set display_name = coalesce(?2, display_name), is_obfuscated = coalesce(?3, is_obfuscated), name_source = ?4`), only when `source >= current name_source` (`:147`). `name_source` is the one `releases` column the stage adds (`schema.rs:94-96`). The FTS index follows `display_name` by trigger (`src/store.rs:149-162`).

### 2.4 In-run facts (not stored)

`Outcome.facts` carries `("predb.title", title)` and `("predb.category", category)` (`step.rs:238-241`) for later steps of the same run via `ctx.fact()` (`src/postprocess/step.rs:116-118,215-216`; appended at `src/postprocess/engine.rs:640`). No step outside `src/predb/` reads them (`grep 'predb\.\(category\|title\)' src` -> only `step.rs`); test `facts_reach_later_steps` (`tests/predb_stage.rs:256-274`) uses a test step. Their values are stored anyway (as `display_name` and `predb_match.category`), so they add no catalog item.

### 2.5 Match row lifecycle

```
titles (atlas.predb.db)  --find()-->  Match{TitleRow, Kind}  --run()-->  predb_match row (shard) + releases.display_name/name_source=100/is_obfuscated=0
                                       |- no match, inner files --> predb_inner_key rows (<=4)
```

---

## 3. Feeds: the 33 sources and what each keeps (`src/predb/feeds.rs:89-297`; parsers `feeds.rs:377-505`, `src/predb/more_feeds.rs`)

`FeedSpec` (`feeds.rs:39-56`): `id` (stored in `titles.source`/`feeds.source`, never reused, test `:512-520` pins 33 sources, ids 1..99; 100 is the file import `file_import.rs:18`), `key` (config name), endpoint, paging (`Query`/`Fixed`/`Skip(n)` `:29-37`), `min_delay_ms`, `refresh_secs` (weekly catalogues `:58`). `Entry` = `{title, category, size_bytes, files, pre_at, nuked, nuke_reason}` (`import.rs:10-18`).

| id | key | parser | title | category | size_bytes | files | pre_at | nuked / reason |
|---:|---|---|---|---|---|---|---|---|
| 1 | predb_club | `parse_predb_club` `feeds.rs:377-399` | `name` | `cat` | `size` MB*1048576 | `files` | `preAt` | `nuke.type`: unnuke/undelpre=2, modnuke=3, renuke=4, oldnuke=5, else 1; `nuke.reason` |
| 2 | predb_net | `:401-421` (`max_page 100` `:113`) | `release` | `section` | `size` MB | `files` | `pretime` | `status` 0->0, 2/4->2, else 1; `reason` when nuked |
| 3 | predb_org | `:423-438` | `rlsname` | `section` | `size` MB | `files` | `ctime` | 1 when `status==1` or `nukereason` set |
| 4 | xrel | `:467-481` | `dirname` | `ext_info.type` upper | `size.number`*unit KB/MB/GB/TB (`:445-456`) | - | `time` | `flags.nuke_rls` truthy -> 1, no reason |
| 5 | xrel_p2p | `:483-505` | `dirname` | `META-sub` | `size_mb` MB | - | `pub_time` | never |
| 6 | predb_ovh | `more_feeds.rs:160-190` | `name` | `cat` | `size` KB*1024 | `files` | `preAt` | `nuke.typeId` 0->0, 2/5->2, 3->3, else 1, only when `nuke.preId == id`; `reason` |
| 7 | srrdb | `:193-199` (Skip(45) `feeds.rs:181`) | `release` | - | - | - | - (listing date, not pre) | - |
| 8 | predatabase | `:201-215` | `rlsname` | `section` | `size` MB | `files` | `ctime` > 0 | - |
| 9 | predb_me | `parse_rss` default `:119-158` | `<title>` | - | - | - | `<pubDate>` | - |
| 10 | scnsrc_me | rss spaced+map_category | spaces -> dots | `<category>` | - | - | pubDate | - |
| 11 | eztv | rss spaced | spaces -> dots, `EZTV` suffix stripped | - | - | - | pubDate | - |
| 12-15, 18-22 | nfodb, one_techno, one_gabba, ddlvalley, mp3scene, gload, hdworld, movieblog, scnlog | rss listed_only (+map_category for 12,13,14,22) | `<title>` | `<category>` or - | - | - | none (listing date is not a pre time `more_feeds.rs:114-115,147-149`) | - |
| 16-17 | hdencode, hdencode_tv | rss listed_only strip_size | title minus ` - N GB` | - | - | - | none | - |
| 23 | apibay | `:217-237` | `name` | 201->MOVIE, 207->MOVIE-HD, 205->TV, 208->TV-HD, else skipped | `size` bytes | `num_files` | - | - |
| 24 | knaben | `:240-254` | `title` (movie category 3000000, literal dotted) | `category` | - | - | - | - |
| 25-29 | crowdnfo, nima4k, g4u, gabber, seriousaboutsound | HTML scrapes `:257-339` | one literal name each | - | - | - | - | - |
| 30-31 | nswdb, 3dsdb | `parse_catalogue` `:342-365`, weekly | `<releasename>` | - | - | - | - | - |
| 32 | dats_pciso_1998 | `parse_dat` `:368-385`, weekly | `<game name>` with one rom | - | - | - | - | - |
| 33 | nzedb_archive | `parse_nzedb` `:422-455`, weekly | field 1 | field 8 | - | - | - | - |

So `pre_at` exists only from feeds 1-6, 8, 9-11 (RSS with dates) and the file import's third column (`file_import.rs:59-72`); `size_bytes`/`files` only from 1-3, 6, 8, 23 (xrel sizes, no files); `nuked`/`nuke_reason` only from 1-4, 6. `category` strings are free text per feed (`TV-HD`, `X264`, `MOVIE`, `MOVIES-hd`, `DVDRIP`, RSS category names, nZEDb sections), capped at 64 bytes. `nuked` codes: 0 none, 1 nuked, 2 unnuked, 3 modnuke, 4 renuke, 5 oldnuke (`docs/port/06-predb.md:132`).

Polling (`src/predb/poll.rs`): `UreqFetch` with no redirects, 8 MiB body cap, gzip unpacked within the cap (`:36-82`); `walk` pages forward/gap/history within `max_pages_per_poll` (`:205-313`); backoff `poll_secs * 2^failures` capped at 6 h (`:336-351`); 429 or a rate-limit envelope reschedules by `Retry-After`/`X-RateLimit-Reset` (`:95-109,144-149,161-166,432-437`); metrics `PREDB_FEED_POLLS` (`:414`), `function_bytes("predb.poll_feed")` (`:62`).

---

## 4. Matching (`src/predb/matcher.rs`)

`Evidence { name, display_name, size, files (posted file names), inner (archive inner names) }` (`:41-47`), built in `step.rs:193-199` from `ctx.release()`, `ctx.files()` (<= 5,000, `postprocess/step.rs:101-104`) and `archive::store::inner_files` (`archive/store.rs:128-130`).

`find` (`matcher.rs:102-171`), tiers in order, each `unique` (`:90-101`: one identity or ambiguity -> no match):
1. **Hash** (`Kind::Hash = 3`): the release name and each posted file name, cleaned by `clean_candidate`, that is a 32/40-digit hex string (`keys::hash_key` `:36-41`) nominates by its first 16 hex digits; accepted only when `md5_hex(title)` or `sha1_hex(title)` equals the whole name (`:104-118`). All hashed names must agree on one title (`:114-118`).
2. **Exact** (`Kind::Exact = 1`): `display_name` then `name`; rows by `name_key(clean)`; more than `MAX_ROWS_PER_KEY = 16` rows -> no match (`:122-125`); accepted by `literally` (`:71-82`: equal ignoring case, or title + one known file suffix) (`:126-131`).
3. **Normalized** (`Kind::Normalized = 2`): same rows, `identity_key` equality, only for `is_full_title` (15+ bytes, 3+ words, `-GROUP` `:17,67-69`) and `sizes_agree` (within a factor of 3, `:83-88`) (`:132-144`).
4. **InnerFile** (`Kind::InnerFile = 4`): every inner path and posted file name, plus its parent directory, up to `MAX_NAMES = 16` distinct keys; `literally` or `identity_key` equality, full title, sizes agree, and not an episode title when the files span >= 2 episodes (`:147-170`).

The step drops a hit that `renames_pack_to_episode(current_name, title)` (`step.rs:208`). Kind labels: `step.rs:224-229` and `metrics/catalog.rs:87-96` (`exact`, `normalized`, `hash`, `inner_file`, `other`).

---

## 5. The step `predb_match` (`src/predb/step.rs:157-272`)

- `enabled` = `cfg.predb.enabled` (`:172-174`); `applies` = `worth_looking_up` (`:69-84`): false when `name_source >= 100` (already a PreDB name), when the current name has an image extension (`naming::is_image_name` `src/naming/decide.rs:60-64`), when `parts <= 1` and the name is neither a hash nor better than `Grade::Junk`; `reruns` on rename and growth (`:178-180`).
- `run` (`:181-271`): reads files and inner files; derives the predb path from the shard's path (`db_path_from_shard` `:87-95`, `…/atlas.s3.db -> …/atlas.predb.db`); one `query_only` reader per thread per file in a `thread_local!` (`:97-116`); records `max(titles.id)` in the process-wide `IN_FLIGHT` map and returns the guard as `Outcome.hold` (`:118-155,201-206,269`) so the late path never passes an uncommitted read (`late.rs:186`).
- Metrics: `PREDB_LOOKUPS{result}` (`:209-210`), `PREDB_MATCHES{kind,category}` with the category label capped (`:230-231`, `src/metrics/mod.rs:263-267`, `src/metrics/names.rs:107-108`).
- Outcome on a match (`:257-270`): `matched: true`; writes `MATCH_UPSERT` + `KEYS_CLEAR`; `name` = the title at source 100, `obfuscated = false` (goes around `naming::improves`, comment `:260`); `rerun = LATER_PARTS` (`0xFFFF << 24`, ids 24..=39: category and media steps, `:64-65`) unless `predb_match.title_id` already equals this title (`:232-236,266`); `facts` (section 2.4).
- Outcome on no match (`:211-221`): clears and rewrites up to 4 `predb_inner_key` rows when inner files exist; nothing else (no `predb_match` row).
- A missing or unreadable predb file is `StepError::Retry` (`:182,190,206`); after `max_attempts` the failed bit is set (`engine.rs:669-683`); `--reprocess --failed` retries (`reprocess.rs:44-65`).
- Other steps ask for it: the naming step requests bit 16 whenever inner files exist, renamed or not (`src/naming/steps.rs:46-47`); the archive steps request `RERUN_ON_INNER_FILES = mask(8) | mask(16)` when inner rows were stored (`src/archive/mod.rs:34-36`, `src/archive/steps.rs:170`); any rename by an earlier step re-runs it (`engine.rs:647-660`, `Reruns.on_rename`).
- Naming never replaces a PreDB name: `worth_running` returns false at `name_source >= 100` (`src/naming/decide.rs:67-70`; test `:346`).

---

## 6. The late path (`src/predb/late.rs`): how a title that arrives after its release finds old releases

Constants `:16-19`: `LATE_DELAY_SECS = 600`, `LATE_FTS_LIMIT = 32`, `LATE_MAX_RERUNS = 16`, `LATE_TITLE_BUDGET = 2 s`. Thread `atlas-predb-late` (`mod.rs:141-176`) owns one `db::create` connection to the predb file and 8 read-only shard connections (`:143-151`), and every second takes `late_per_second` titles (`:163-168`).

`tick_counting` (`:176-216`): reads `titles` past `meta.late_after_id`, at least 600 s old, and `id <= lowest_in_flight` (`:186-194`); for each title `nominate` (`:81-162`) over every shard:
- `NAME_PROBE` `select id, name_source from releases where name = ?1 limit 17` (`:55,104-108`), on `idx_release_unique(name, group_name)` (`src/store.rs:146`);
- MD5 and SHA-1 of the title as quoted FTS tokens (`FTS_PROBE` `:56,110-120`), accepted by `accepts_hash` (`:48-53`);
- the title as one FTS phrase when `is_full_title` and within budget (`:122-141`), accepted by `accepts` on `name` or `display_name` (`:44-46`);
- `INNER_PROBE` `select release_id from predb_inner_key where key = ?1 limit 17` (`:58,146-151`);
- rows with `name_source >= 100` are dropped (`:73,108`); more than 16 nominees is `overflow`, no reruns (`:156-160`).
Each nominee gets `stage.rerun(id, mask(PREDB_MATCH))` (`mod.rs:161`; `late.rs:207-210`), which clears bit 16 in `pp_state` and queues the release (`src/postprocess/mod.rs:62-78,116-123`); the step then decides by the same rules (module doc `late.rs:1-2`). The cursor moves after each title (`:211`). Counters feed `stats.json` (`LateCounts` `:35-41`; `mod.rs:214-220`) and `atlas_predb_late_total`/`atlas_predb_late_behind` (`src/metrics/report.rs:200-205`).

Known misses (`docs/port/06-predb.md:536-545`, `README.md:477`): a short title under any form but an exact `name`; a title only in a file that isn't the row's name (`tests/predb_late.rs:100-116` pins the miss and the `rerun(id, mask(PREDB_MATCH))` remedy); inner names past the first 4 kept; overflow/budget; rows `worth_looking_up` skips. `--reprocess --step predb_match --apply` catches them.

---

## 7. Backfill paths that already exist

1. **The late path** (section 6): per new title, index-only, never scans (`tests/predb_late.rs:230`, `no_statement_scans_releases`).
2. **`atlas --reprocess --step predb_match --apply`** (`src/postprocess/reprocess.rs`): parses `--step` by step name (`:80-91`), inserts a `pp_walks` row of kind `reprocess` with `redo_mask = mask(16)` per shard (`:25-40`, `apply_all :137-162`, holds off compaction `:138`); the stage's `walk_tick` then visits releases newest-first in batches at `backlog_per_second / 8` per shard (`src/postprocess/engine.rs:412-486`, token bucket `:421-431`, `visit_all(.., redo, ..)` `:470`), clearing the bit so the step runs again on every release that `applies`. Dry run prints a count and an ETA at `backlog_per_second` (`:110-135`; default 2000/s `src/config.rs:628`). The design estimates 16.4 h for 118M rows (`docs/port/06-predb.md:896,916`).
3. **Turning `predb.enabled` on** seeds a `steps` walk of every release because a new step bit appeared (`engine.rs:1095-1099`; `README.md:445`).
4. **`--predb-import --apply`** bumps `late_after_id` to the top when the late queue was caught up and prints the `--reprocess` command (`file_import.rs:193-201`; test `tests/predb_import.rs:95`).

A rerun that matches the same title again is a no-op for names (`apply.rs:152-157` only counts a changed `display_name`) and for later parts (`step.rs:266`), but it still rewrites the `predb_match` row with a fresh `matched_at` (`step.rs:237,242-254`; `insert or replace`).

---

## 8. UI surface (what reaches a person)

| where | shows | source |
|---|---|---|
| `/ui/release` h1 and `<title>` (`src/web.rs:1184,1188,1198`) | `r.name` = `coalesce(r.display_name, r.name)` (`src/search.rs:33-34,557-559`), so for a matched release the PreDB title itself is the headline | `releases.display_name` written by the step's `NameChange` |
| `/ui/release` facts panel (`src/web.rs:1047-1142`, rendered at `:1191`) | `Posted as` = `n.posted` when it differs from the shown name (`:1088-1090`); `Name from: PreDB` (`:1091-1093`) via `release_info::name_source_label(100) = "PreDB"` (`src/release_info.rs:24-35`) | `release_info::names(id)` (`:132-150`, reads `name, display_name, is_obfuscated, name_source` from `s{shard}.releases` on a fresh `db::open()`), called at `web.rs:1003` |
| `/ui/release` tags (`web.rs:1043-1045`) | `OBFUSCATED` only when `names.obfuscated`; a PreDB match sets it false (`step.rs:263`), so the tag disappears | same |
| `/ui/duplicates` copy cards (`web.rs:1463-1548`) | names via `names_with` (`:1469`) but only the `OBFUSCATED` tag (`:1476-1478`); no `Name from` | same |
| Newznab API (`src/api.rs:285-345`) | nothing: `release_attrs` emits poster/group/resolution/video/audio/hdr/source/language/team/twins/alts (`:296-308`); the `<title>` is the coalesced name (`:338`) | `search::ReleaseRow` has no `name_source` (`src/search.rs:9-18`) |
| Stats TUI, Post-processing page, `PreDB` block (`src/stats_dashboard.rs:1077-1082,1129-1146`) | titles, file size, late queue behind, late reruns, per-feed last ok/next poll/last error | `stats.json["predb"]` = `Service::snapshot` (`mod.rs:209-254`), written at `src/bg_indexer.rs:486-488` |
| Prometheus (`src/metrics/report.rs:196-216`; `src/metrics/names.rs:106-116,154-157`) | `atlas_predb_titles`, `_file_bytes`, `_late_behind`, `_late_total{kind}`, `_feed_*`, `atlas_predb_lookups_total{result}`, `atlas_predb_matches_total{kind,category}`, and from the catalog walk `atlas_catalog_releases{state="predb_matched"}`, `atlas_catalog_predb_kind_releases{kind}`, `atlas_catalog_predb_category_releases{category}`, `atlas_catalog_predb_feed_releases{feed}`, `atlas_catalog_predb_root_releases{root,matched}` (`src/metrics/catalog.rs:168,246-256,305,356-370,376-412`; test `tests/metrics.rs:199-235`) | `predb_match` left-joined in `read_batch` (`catalog.rs:442-453`), `titles`+`feeds` in `read_feeds` (`:377-412`) |

Nothing in `src/web.rs` or `src/api.rs` reads `predb_match`, `predb_inner_key` or `atlas.predb.db` (grep empty). The only predb-derived strings on the page are the title (as the display name) and the literal `PreDB` label.

---

## 9. Catalog coverage for this area

### 9.1 `nzb.external_oracles` (catalog line 560-562), the PreDB half

| facet | status | evidence |
|---|---|---|
| PreDB name lookup (the title) | **shown** (as the page headline and `Name from: PreDB`) | `step.rs:261-265` -> `apply.rs:158-162`; `search.rs:33-34`; `web.rs:1091-1093`; `release_info.rs:31` |
| matched by name + size (`sizes_agree`) | **stored_not_shown** (how it matched = `predb_match.kind`; the size test itself is parsed_not_stored) | `matcher.rs:83-88,136,162`; `step.rs:248`; `catalog.rs:442-446` only reader |
| hash-named posts (MD5/SHA-1 of the title) | **stored_not_shown** (`kind = 3`) | `matcher.rs:104-118`; `step.rs:227` |
| nuke | **stored_not_shown** (`predb_match.nuked` copy; `titles.nuked`, `titles.nuke_reason`) | `step.rs:33,251`; `db.rs:16-17`; `import.rs:72-75`; no reader outside `catalog.rs` (which does not read `nuked`) |
| pre time | **stored_not_shown** (`predb_match.pre_at`, `titles.pre_at`) | `step.rs:32,250`; `db.rs:15` |
| PreDB section | **stored_not_shown** (`predb_match.category`, `titles.category`); reaches Prometheus only | `step.rs:31,249`; `catalog.rs:443,462,255,358-362` |
| which feed brought the title | **stored_not_shown** (`titles.source` -> `feeds.key`); Prometheus only | `db.rs:11`; `catalog.rs:386-390` |
| group (the catalog's "PreDB (name, size, group, nuke)") | **not_collected**: no feed stores a release group separately; it is only part of the title string (the page's `Release group` fact is parsed from the name, `web.rs:1119-1121`) | `import.rs:10-18` has no group field |
| srrDB hashes (ArchiveCrc, RarHash, OSHash) | **not_collected**: feed 7 keeps only `release` names (`more_feeds.rs:193-199`); no hash lookups | |

### 9.2 `derived_per_post.obfuscation_level` (catalog line 505-507), the PreDB name layer

| facet | status | evidence |
|---|---|---|
| "which layer gave the real name" = `releases.name_source` | **shown** as the `Name from` label (one of `a posted file name`, `the subject`, `the NFO`, `a file inside the archive`, `the PAR2 file`, `PreDB`) | `release_info.rs:24-35`; `web.rs:1091-1093` |
| `releases.is_obfuscated` | **shown** as the `OBFUSCATED` tag; cleared to 0 by a PreDB match | `web.rs:1043-1045`; `step.rs:263` |
| the posted name next to the real one | **shown** (`Posted as`) | `web.rs:1088-1090` |
| a "level" | **not_collected** (catalog note is right; nothing combines the layers) | |

### 9.3 Every column, for the visibility table the task asked for

| column | status | evidence |
|---|---|---|
| `predb_match.release_id` | stored_not_shown (key) | `step.rs:28` |
| `predb_match.title_id` | stored_not_shown | `step.rs:29,247`; readers `step.rs:232-236`, `catalog.rs:443` |
| `predb_match.kind` | stored_not_shown | `step.rs:30,248`; `catalog.rs:443,460` |
| `predb_match.category` | stored_not_shown | `step.rs:31,249`; `catalog.rs:443,462` |
| `predb_match.pre_at` | stored_not_shown | `step.rs:32,250`; no reader |
| `predb_match.nuked` | stored_not_shown | `step.rs:33,251`; no reader |
| `predb_match.matched_at` | stored_not_shown | `step.rs:34,252`; no reader |
| `predb_inner_key.key/release_id` | stored_not_shown (an index for the late path, not a fact) | `step.rs:42-53`; `late.rs:146-151` |
| `titles.title` | shown (as `display_name`, via the match) | section 8 |
| `titles.source` | stored_not_shown | `db.rs:11`; `catalog.rs:389` |
| `titles.category` | stored_not_shown | `db.rs:12`; `matcher.rs:51` |
| `titles.size_bytes` | stored_not_shown (used by `sizes_agree`, never printed) | `db.rs:13`; `matcher.rs:136,162` |
| `titles.files` | stored_not_shown (written, never read) | `db.rs:14`; `import.rs:62-63,68` |
| `titles.pre_at` | stored_not_shown | `db.rs:15` |
| `titles.nuked` | stored_not_shown (copied to `predb_match.nuked`) | `db.rs:16`; `matcher.rs:51` |
| `titles.nuke_reason` | stored_not_shown (never leaves the file) | `db.rs:17`; no reader |
| `titles.added_at` | stored_not_shown | `db.rs:18`; `late.rs:188` |
| `feeds.*`, `meta.*` | stored_not_shown on the release page; shown on the TUI dashboard and Prometheus (operational, not per release) | `mod.rs:229-245`; `stats_dashboard.rs:1129-1146` |
| `Outcome.facts` `predb.title/category` | parsed_not_stored (their values live elsewhere) | `step.rs:238-241`; `engine.rs:640` |
| `Evidence` (posted/inner names used, size test result, the tier that fired) | parsed_not_stored except `kind` | `step.rs:193-199`; `matcher.rs:102-171` |

---

## 10. What a "PreDB" fact group on the release page could print, and where each value comes from

| line | source | caveat |
|---|---|---|
| Title | `releases.display_name` (already the h1) | nothing new |
| Matched by | `predb_match.kind` -> "exact name" (1), "name with other separators" (2), "MD5/SHA-1 of the title" (3), "a posted or inner file" (4) | label text exists only for metrics (`catalog.rs:87-96`) |
| Pre time | `predb_match.pre_at` (shard) or `titles.pre_at` (file) | the shard copy is frozen at match time; a later feed can fill `titles.pre_at` (`import.rs:66-71`) without the copy changing, so prefer the file when it is open, else the copy; NULL for feeds 7, 12-33 |
| Section | `predb_match.category` or `titles.category` | free text per feed; not Atlas's category (`release_category`, CategoryStep rank 400s); label it "PreDB section" |
| Nuked + reason | `titles.nuked` + `titles.nuke_reason` (file); fallback `predb_match.nuked` (shard, no reason) | the copy goes stale: a nuke that arrives after the match updates `titles` (`import.rs:113-115`) but `predb_match.nuked` only on a rerun; a nuked title still renames (`docs/port/06-predb.md:19`) |
| Size vs PreDB | `titles.size_bytes` vs `releases.size` (`ReleaseRow.size`), as a ratio or delta | PreDB sizes exclude PAR2/yEnc overhead (`docs:1028`); NULL for most feeds; `sizes_agree` used a factor of 3 (`matcher.rs:83-88`) |
| Files vs PreDB | `titles.files` vs `release_info::Contents.files` | NULL for most feeds |
| From feed | `feeds.key` by `titles.source` (`catalog.rs:388-390` has the SQL); `100` = imported file (`file_import.rs:18`) | needs the file |
| Matched at | `predb_match.matched_at` | "last confirmed at" (rewritten on every matching rerun) |
| Title id | `predb_match.title_id` | dangles if the file was deleted or replaced (`docs:951`) |

Candidates for a tag: `PRE` (has a row), `NUKED` (nuked in 1,3,4,5; 2 = unnuked is clean).

---

## 11. Extension points

- **Panel placement**: `release_html` builds `facts` with the `fact(name, value)` closure (`web.rs:1047-1052`) and renders them at `:1191`; separate `<h2>` panels use `panel(title, facts)` (`:1235-1240`) and are concatenated at `:1192-1194` (`{copies}{probe}{files}`). A `predb_html(...)` call appended there, like `probe_html` (`:1163-1170`, which opens `crate::db::open()` itself), is the least invasive. The htmx-loaded `/ui/details` (`:62,1657`) is the media panel, not a general hook.
- **Reading the shard row**: `release_page` already has `id` and opens shard-attached connections several times (`release_info::names` `:1003` -> `db::open()` `release_info.rs:132-134`; `contents` `:1004`; `archive::store::view` `:1001` -> `db::open()` `archive/store.rs:183-185`; `get_release` `search.rs:552-554`; probe `web.rs:1163`). The natural shape is `predb::store::read(conn, id) -> Option<MatchRow>` doing `select title_id, kind, category, pre_at, nuked, matched_at from s{shard}.predb_match where release_id = ?` (shard = `store::shard_of_id(id)` `src/store.rs:44-46`), a PK lookup; or reuse `db::with_reader` (`src/db.rs:86-116`, kept connections with every shard attached, swept when a shard file is replaced `:73-82`).
- **Joining `title_id` to `atlas.predb.db`**: it is a separate file, never attached to the main connection (`store::attach` attaches only `s0..s7`, `src/store.rs:94-108`). Options: (a) `predb::db::open_reader(&paths::predb_database())` per request (`db.rs:64-68`, `paths.rs:92-94`): one `sqlite3_open` + `set_up` pragmas and `filters::register_sql` (`src/db.rs:167-175`) + `pragma query_only`, then `select title, source, category, size_bytes, files, pre_at, nuked, nuke_reason from titles where id = ?` and `select key from feeds where source = ?` (both PK lookups); a missing file is `Err` and must render the shard copy only. (b) `attach database ? as predb` on the kept reader: cheaper per request but the sweep in `db.rs:73-82` keys on main+shard inode ids only, so a deleted/recreated predb file would stay held open; the kept-reader cache budget (`db.rs:44-47`) does not account for it. (c) Keep one `thread_local!` reader per API worker as `step.rs:97-116` does for stage threads. The step's own `with_reader` is `fn`-private to `step.rs` (`:102`), so a web reader needs its own helper (or make that one `pub(crate)`).
- **New columns on `predb_match`**: append to `ShardTable.added` (`schema.rs:102-110,118`): nullable, added by `alter table` on open, no index built on a table with rows (`:112-113`). New shard tables: add to `SHARD_TABLES` (`schema.rs:78-92`) with `release_key: Some("release_id")` for the gc trigger; the registry tests (`tests/pp_schema.rs`, `docs:214`) and compaction copy follow from that.
- **New columns on `titles`** (file side): mirror the `last_rows` pattern in `db::create` (`db.rs:55-59`); `import.rs` `insert`/`fill` statements (`:61-71`) and `Entry` (`:10-18`) carry the value; `rows_for_key` (`matcher.rs:49-65`) and `TitleRow` (`:28-35`) if the step should copy it.
- **Copying more of the title into the shard at match time**: extend `MATCH_UPSERT` (`step.rs:55-57`) and the params (`:242-254`), e.g. `size_bytes`, `files`, `nuke_reason`, `source`; `TitleRow` would need `files`/`source`/`nuke_reason` (`matcher.rs:28-35,51`). That would make the page independent of the file, at the cost of a backfill rerun to fill old rows.
- **Backfill job for existing releases**: the registered mechanism is a `pp_walks` row (`reprocess::apply` `reprocess.rs:25-40` from the CLI, or `seed_shard`'s `steps` walk on a new stamp bit `engine.rs:1103-1111`); `Step::config_stamp` (`postprocess/step.rs:58-62`) redoes a step on existing releases when a bit it did not have before appears, which is the in-code way to force a one-time re-run of `predb_match` after a schema change (PredbMatch has no `config_stamp` today). A cheaper fill that avoids re-matching: a one-off `update predb_match set ... from titles` cannot cross files in SQLite without attaching; a maintenance thread could attach `atlas.predb.db` to a shard writer connection and copy by `title_id` in batches, but that is a new writer path (the step's writes go through `apply::write`, `apply.rs:135-144`).
- **Showing twins**: `copy_card` (`web.rs:1463-1548`) is where per-copy facts live; a PreDB badge there needs `names_with(conn, id).source` (already read `:1469`) and, for more, the same shard lookup.
- **API**: `release_attrs` (`api.rs:285-310`) is the place for a `newznab:attr name="predb"`-style attribute; the design explicitly left this out ("No new API attribute", `docs:18`).

---

## 12. Risks

- **File size and open cost**: `atlas.predb.db` grows at about 153 B/title (`README.md:332`); 10M titles is 1.5-2 GB (`docs:167`). Each per-request `open_reader` is an extra file handle and page cache with no `cache_size` tuning (`db.rs:64-68`), on top of the 3-5 connections `release_page` already opens. Cold reads cost up to a few page reads per lookup (`docs:894`).
- **The file may be absent or stale**: `enabled=false` makes no file (`bg_indexer.rs:845-849`; `tests/predb_file.rs:44`); the user may delete it (`app.rs:599-600`); `title_id` then dangles (`docs:951`). The shard copy (`kind, category, pre_at, nuked, matched_at`) must be enough to render.
- **Nuked titles are invisible today and the shard copy goes stale**: a nuke arriving after the match updates `titles.nuked/nuke_reason` (`import.rs:113-115`) but not `predb_match.nuked` until the step reruns, and a rerun of the same title does not clear later parts (`step.rs:266`). A badge from the shard copy can say "not nuked" for a title the feeds have since nuked. Codes 2 (unnuke) and 3-5 need their own words.
- **PreDB section vs Atlas category disagreement**: `predb_match.category` is a feed's free-text section (`TV-HD`, `X264`, `MOVIES-hd`, RSS category text, nZEDb section, capped 64 bytes `import.rs:30,82`) while `release_category.category` is a Newznab id chosen by CategoryStep (rank 400s) after the match; `CATALOG_PREDB_ROOT` (`catalog.rs:364-370`) exists because they differ. Two "Category" lines on one page will confuse unless one is labelled "PreDB section".
- **Reruns via `LATER_PARTS`**: a first-time match clears bits 24..=39 (`step.rs:65,266`), so a backfill that finds new matches cascades into the category and media steps (media are net steps with API budgets). A rerun that keeps the title is cheap (`step.rs:266`), but still rewrites the `predb_match` row (`insert or replace`, new `matched_at`).
- **Backfill size**: `--reprocess --step predb_match --apply` visits every release (`engine.rs:412-486`) at `backlog_per_second` (2000/s default, `config.rs:628`): about 16 h per 118M rows (`docs:896`); each visited release that `applies` costs 2-34 predb lookups plus `files`/`inner_files` reads (`docs:894`). A new `config_stamp` bit would trigger the same walk automatically at the next start (`engine.rs:1103-1111`).
- **Schema migrations**: shard tables take append-only nullable columns via `added` (`schema.rs:102-110`); the predb file has only the ad-hoc `last_rows` migration (`db.rs:55-59`); `tests/pp_schema.rs` iterates the registry (`docs:214`), and `--compact` copies registered tables only (`docs:215`), so an unregistered table is lost at compaction.
- **Concurrency**: web readers on `atlas.predb.db` are WAL readers (`db.rs:53`) beside the feed thread's writer; `--predb-import --apply` refuses to run while the indexer is alive (`file_import.rs:235`), so no conflict there. The IN_FLIGHT guard (`step.rs:118-155`) only concerns stage threads.
- **Open PRs**: see section 13; the fact list at `web.rs:1087-1094` is inside #48's hunks.

---

## 13. Conflicts with open branches (diffed locally against `main`)

| branch | touches predb? | touches the predb UI landing spots? |
|---|---|---|
| `pr/48` (head 06847b2 "Keep the attachment list small"; files: `assets/web.css`, `src/media/details.rs`, `src/postprocess/schema.rs` @84, `src/probe/media.rs`, `src/probe/mod.rs`, `src/probe/step.rs`, `src/release_info.rs`, `src/web.rs`, `tests/web_ui.rs`) | no `src/predb/*`, no `naming/*` | **yes**: `src/web.rs` hunks at 61, **1091-1092, 1106-1108, 1115-1117, 1141-1144, 1150-1154** (inside `release_html`'s fact list, right where `Name from` is printed at 1087-1094), 1190, 1280-1390 (`probe_html`), 1685; `src/release_info.rs` hunk at 33-57 adds `NzbFile`/`Contents.list` directly under `name_source_label` (the function itself is unchanged) and at 165-201 (`contents_with`); `schema.rs:84` adds a table to `SHARD_TABLES` next to the predb entries at 88-89 |
| `pr/51` (7562a82) and `pr/54` (1e1949f), stills/HEVC/H.264 | no | `schema.rs:85` (new `release_stills` entry adjacent to `MATCH`/`INNER_KEY`), `steps.rs:14` (new step in `all()`), no `web.rs` |
| `origin/feat/release-stills-strip` (2c5a0a4) | no | `web.rs` (the `/ui/still` route and strip, per `coordination.md`), `schema.rs`, `steps.rs`, `assets/web.css` |
| `pr/57` (e1ad6c3 "Sort results instantly from a window kept in memory") | no | `src/web.rs` hunks 5-730 (`Search`, `sort_rows`, results), `src/api.rs`; not `release_html` |
| Finish web UI thread (twin grouping) | unknown (no branch found locally; `coordination.md` says unresolved) | `copies_html`/`copy_card` (`web.rs:1463-1555`) is where twin cards print names |

Conclusion: nothing open touches `src/predb/`, `name_source_label`, `names_with` or `predb_match`. A PreDB panel appended after `{probe}` at `web.rs:1192-1194` and a new `SHARD_TABLES` entry will merge with #48/#51/#54 as adjacent-line conflicts; inserting lines inside the fact list at 1087-1094 conflicts with #48 textually.

---

## 14. Notes for the merge of the map

- `storage.md:81` lists `predb_match` readers as "naming (PreDb source 100), results": the naming step reads `name_source`, not `predb_match`; `results` read `coalesce(display_name, name)`. The only reader of the table outside `src/predb/` is `src/metrics/catalog.rs:442-464`. `storage.md:20,108` cite `step.rs:92-100` for the titles file; the file is `src/predb/db.rs:7-43`.
- `ingest-postprocess.md:414` says `predb_match` reaches the "details panel, Links fact": it does not; `/ui/details` is media only (`web.rs:1657-1679`).
- `api-cli.md:289,327-328` (stored_not_shown for nuked) agrees with the code; `ingest-postprocess.md:511` (unclear) can be settled: the PreDB half of `nzb.external_oracles` is `shown` for the title and `stored_not_shown` for every other facet (section 9.1).
- `docs/port/06-predb.md` line numbers are at base `de598b6` and do not match `main` (e.g. it cites `src/config.rs:134,169,264`; `PredbConfig` is at `src/config.rs:362-429` now). Its tables of feeds (`:295-315`) cover only the first five; `feeds.rs:89-297` has 33.
- `README.md:435` says "from public JSON feeds"; 20 of the 33 are RSS/HTML/XML/CSV (`feeds.rs:193-296`).

## 15. Open questions

1. Should the page read `atlas.predb.db` at request time (live nuke/size/feed, but an extra open per page and a file that may be absent) or should the step copy `size_bytes`, `files`, `nuke_reason` and `source` into `predb_match` (self-contained, but old rows need a `--reprocess --step predb_match` or a `config_stamp` walk to fill)?
2. Does the owner want `matched_at` to mean first match (then the upsert must become `insert ... on conflict do update` keeping the old value) or last confirmation (as now)?
3. Should a stale `predb_match.nuked` be refreshed by the late path when a nuke update arrives (`import.rs:113-115` knows the title id and could nominate like a new title; today `tick` only reads titles past `late_after_id`)?
4. Is a per-copy PreDB badge wanted on `/ui/duplicates` cards, given the Finish-web-UI thread's twin grouping work there?
