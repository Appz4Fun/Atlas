# Atlas map: ingest and postprocess

Repo `/home/claude/appz4fun/atlas`, branch main at `55d2e51`. Code read only; nothing built or run.
All paths below are relative to the repo root. Line numbers are from this checkout.

Scope of this note: how overview rows become releases, what survives into the shard
database, what each postprocess step does and when it re-runs, how existing releases get
walked again, and what of all that the release page renders. The catalog
(`/tmp/claude-0/-home-claude/b80d34bc-6432-5cb9-a471-5c997487e01d/scratchpad/catalog-current.md`,
derived/NZB items at lines 458-666) is not restated; section 7 only maps its items to code.

Status vocabulary used in section 7: `shown` = rendered on `/ui/release` (or an htmx panel of
it); `stored_not_shown` = persisted in a shard table but not rendered there (a value that only
reaches the downloadable NZB counts as not shown); `parsed_not_stored` = exists in memory during
ingest or a step and is then dropped; `not_collected` = never computed anywhere.

---

## 1. Ingest: overview rows to releases

### 1.1 What comes off the wire

- `Overview` (src/nntp.rs:91-100) has exactly eight fields: `number, subject, from, date,
  message_id, references, bytes, lines`. `parse_overview` (src/nntp.rs:2712-2727) splits the
  XOVER line on tabs and reads fields 0-7 only; anything after (the optional `Xref:` field)
  is ignored. So crossposting is invisible at ingest.
- `Overview::into_article` (src/nntp.rs:103-115) copies the eight fields into
  `parser::Article` and normalises the date with `dates::to_iso_date`
  (src/dates.rs:132-137): RFC 2822 is reformatted to `"%Y-%m-%d %H:%M:%S"` wall clock with
  the offset dropped; unparseable dates are kept as the raw string.
- Slice size is `min(request_size, pool.max_unsaved())` (src/indexer.rs:1624); defaults
  `DEFAULT_REQUEST_SIZE=10_000`, `DEFAULT_MAX_UNSAVED_HEADERS=500_000`,
  `DEFAULT_BATCH_SIZE=500_000` (src/config.rs:20-32). Chunks of a backfill overlap by
  `CHUNK_OVERLAP=3600` articles with `CHUNK_SAFETY=5000` (src/indexer.rs:1073-1077).
- The slice's representative date is the median of sampled article dates
  (`slice_date`, src/indexer.rs:1718-1730) and feeds `db::save_group_dates`
  (src/indexer.rs:1686-1689 -> src/db.rs:616-628), the only per-group time record
  (`groups.low_article/low_posted/high_article/high_posted`, src/db.rs:335-342).

### 1.2 Article and subject parsing

- `Article` (src/parser.rs:12-27): `number, subject, author, date, message_id, references,
  bytes, lines` plus the parsed `filename, release_name, part, total_parts, file_index,
  file_total`. `Release` (src/parser.rs:40-52): `name, articles, size, is_obfuscated,
  display_name, complete, group, poster, date, collection`.
- `parse_subject` (src/parser.rs:83-125): ten regexes `SUBJECT_PATTERNS`
  (src/parser.rs:55-72); filename is capture 1; `RELEASE_SUFFIX_RE` (76-77) strips trailing
  size/poster decorations in a loop (93-99); part/total default to `(1,1)` (102-105); a forged
  part number is rejected (107); `FILE_COUNT_RE` `\[(\d+)\s*/\s*(\d+)\]` (74) yields
  `file_index/file_total` (113-116); very short names get ` [total]` appended (118-122).
  `MAX_PARTS=100_000` (src/parser.rs:9). `remap_bracket_parts` (129-172) fixes the
  bracket-counter dialect.
- `group_articles_by` (src/parser.rs:182-232): key is the collection key when there is one,
  else `parsed.release_name` (197); the shortest name wins for a key (206-209); the parsed
  bits are stashed on the article (212-217); unparsable subjects are counted and logged as
  `Dropped {n}/{total} unparsable` (227-229). `is_complete` (234-270) decides
  `Release.complete`.

### 1.3 Per-article obfuscated uploads (filters)

- `filters::gather_per_article` (src/filters.rs:520-579) runs before collection grouping
  (called from src/indexer.rs:1756). It needs `PER_ARTICLE_MIN=16` articles of one shape
  (src/filters.rs:384) with `TOKEN_LEN=16` tokens (386); `SUBJECT_RE` (390), `COUNTER_RE`
  (392-393).
- `shape(base)` (src/filters.rs:408-418) is an FNV-style hash of the subject with name runs
  collapsed to `x`: effectively a subject-template hash. It exists only in memory.
- Upload key: `format!("{group}\0per-article\0{shape:x}\0{poster}\0{day}")`
  (src/filters.rs:559) with `day = article.date[..10]` (558); poster is blank unless the
  shape+poster pair is "lasting" (557). Synthetic file names
  `per-article.{n:05}of{files:05}.p{total}` / `per-article.p{total}` (566-568);
  release name from `per_article_name(&key)` (582).

### 1.4 Collections (newznab-tmux style grouping)

- `collections::group_articles` (src/collections.rs:313-326) wraps `Collections::of`
  (262-278). Regex table `data/collection_regexes.tsv` (src/collections.rs:29; 602 regexes,
  101 group regexes per the test at 350-351). `MAX_COLLECTION_FILES=10_000` (18),
  `STRONG_NAME=16` (22).
- `subject_base` (src/collections.rs:81-88) is the subject up to its part counter with
  `yEnc` appended if absent; `file_counter` (91-94); cleaners (96-134, 185-202);
  `clean` (218-223); `name_strength` (226-229).
- Key (src/collections.rs:272-277): strong name -> `"{group}\0{name}\0{files}"`;
  weak name -> `"{group}\0{name}\0{files}\0{author}\0{day}"` with `day = date[..10]`.
  The poster and the posting day are therefore part of the identity of weak collections and
  a post that crosses midnight UTC splits (also noted in docs/TODO.md).
- `unread_file_total` (302-308) and `cleaned` (280-297) decide `files`.

### 1.5 Release assembly

`parse_and_save` (src/indexer.rs:1746-1802):

```rust
let (articles, uploads, skipped) = crate::filters::gather_per_article(&group, headers_to_articles(headers));
let mut releases: Vec<Release> = crate::collections::group_articles(&group, articles).into_values().collect();
releases.extend(uploads);
...
release.display_name = names.get(i).cloned().flatten();   // inline par2/nfo lookups, only when archive::inline_lookups()
release.complete = is_complete(&release.articles);
release.group = group.to_string();
release.poster = release.articles[0].author.clone();      // first article of THIS slice
release.date = release.articles[0].date.clone();
```

(src/indexer.rs:1756-1760, 1773-1778, 1783-1790). Inline name lookups are capped at
`NAME_SOURCES_MAX=8` sources (src/indexer.rs:1806-1820) and are skipped once the stage owns
a body budget (1773).

### 1.6 What the save keeps and drops

Shard schema (`store::build_shard`, src/store.rs:129-196; 8 shards, `SHARDS` at
src/store.rs:35):

- `releases(id, name, group_name, poster, posted_date TEXT, size, complete, parts,
  file_total, display_name, is_obfuscated)` + `unique(name, group_name)`,
  `idx_release_group`, FTS `releases_fts(name, display_name)` with sync triggers
  (src/store.rs:129-163). `migrate_shard` (202-247) adds `collections(key INTEGER PRIMARY
  KEY, release_id)` (207), `held_back`, `release_ids`, meta `id_key0/id_key1` (234),
  `files.touched_at`, `files.blob`, then calls `postprocess::schema::migrate`.
- `files(id, release_id, filename, subject, subject_part, subject_mid, expected,
  file_total, seen BLOB NOT NULL, touched_at, blob)` unique `(release_id, filename)`
  (src/store.rs:165-178).
- `domains(id, suffix)` and `segments(file_id, local BLOB, domain, part, bytes) primary key
  (file_id, local, domain) without rowid` (src/store.rs:180-188).
- `meta(key, value)` running totals (191-192).

Upsert (`save_in_transaction`, src/store.rs:1367-1569; statement at 1390-1400):

```sql
insert into releases (id, name, size, complete, group_name, poster, posted_date, display_name, is_obfuscated) values (...)
on conflict(name, group_name) do update set
  poster = excluded.poster,
  posted_date = excluded.posted_date,
  display_name = case when releases.name_source is null then coalesce(excluded.display_name, releases.display_name) else releases.display_name end,
  is_obfuscated = case when releases.name_source is null then excluded.is_obfuscated else releases.is_obfuscated end
returning id, size, parts, file_total
```

Consequences:

- Per release: `poster` and `posted_date` are overwritten on every re-save with the first
  article of the latest slice (src/indexer.rs:1785-1786, src/store.rs:1394-1396). They are
  not "first seen" values and the time part is wall-clock without zone (src/dates.rs:132-137).
- Per file (`FileState::add`, src/store.rs:582-601): the subject of the lowest part number
  is kept (`subject_key`), `expected = max(total_parts)`, `file_total = max`, and the part
  is set in the `seen` bitmap. `put_file` (632-639) sets `touched_at = unixepoch()` on every
  write to the file, i.e. the last time any part arrived.
- Per article (`add_segment`, src/store.rs:645-705): only `insert or ignore into segments
  (file_id, local, domain, part, bytes)` (688-690). Message-id is split into a local blob and
  a domain id. **Dropped per article:** `number`, `author`, `date`, `references`, `lines`,
  the article's own `subject` (only one subject per file survives), and any count of
  duplicate arrivals (the `insert or ignore` neither counts nor dates them).
- Collections: when the release carries a collection key that is not yet known, the key is
  hashed into an integer (`IdKey`, src/store.rs:714-720) and stored as
  `insert or replace into collections (key, release_id)` (src/store.rs:1454-1455). The
  readable key is not persisted.
- Name collisions between collections are resolved by `collection_row` (src/store.rs:
  1668-1698) appending ` [8 digits]`.
- Stats totals (1405-1406, 1536-1542); every saved release is enqueued for the stage with the
  settle delay (`insert into pp_queue (release_id, due_at)`, src/store.rs:1545-1552).
- Sealed files pack their segments into `files.blob`; `each_article` (src/store.rs:
  1947-2008) decodes both forms and fills `search::ArticleRow` (src/search.rs:22-31) with
  `message_id, filename, part, total_parts(=files.expected), bytes, subject(=files.subject),
  poster, posted_date` where the last two come from the release row (store.rs:1954-1956,
  1967-1968). There is no per-article poster or date to read back.
- `release_files` (src/store.rs:1836-1855) returns `FileInfo` (1760-1771) and
  `seen_count` (1790-1792) counts bits.

### 1.7 Writer threads and stage budget

One writer thread per shard (src/indexer.rs:241-256; `writer()` 260-375). Stage work reaches
the shard through `StageWork`/`StageJob` closures (217-222) run between saves in one
transaction (`run_stage_jobs` 378-407, `stage_transaction` 409-418 with `begin immediate`)
under `STAGE_BUDGET_BUSY=10ms` / `STAGE_BUDGET_IDLE=100ms` (230-231). `housekeeping`
(422-445) seals files and checkpoints. `Db::on_writer` (450-458) and `Db::save` (464-474).
`MAX_BATCH=8` (185).

---

## 2. Postprocess stage

### 2.1 The step contract (src/postprocess/step.rs)

- `StepId(pub u8)` (16): "Never reused, 1..=62"; `StepMask = i64` (18); `mask()` (20-22).
- `Needs::{Database, Bodies, External}` (25-29); `Reruns { on_rename, on_growth }` (32-35).
- `trait Step` (37-65): `id`, `name`, `rank`, `needs`, `enabled(&Config)` (46),
  `applies(&ReleaseInfo)` (51), `reruns()` (55), `config_stamp() -> u64` (60-62): "Inputs
  that can change while enabled stays true, one bit each. A bit that wasnt there before
  redoes this step on existing releases".
- `ReleaseInfo` (68-82): id, name, display_name, group, poster, posted_date, size, parts,
  complete, file_total, is_obfuscated, name_source. `StepCtx` (85-93): `files()` capped at
  5,000 (102-104), `conn()` query_only (112-114), `fetch_bodies` (122-138), deadline
  120 s (180-183).
- `Outcome` (206-223): `matched, writes, name, rerun, facts, wait_until, hold`.
  `StepWrite` (225-237): static SQL + params ("Your own tables only, or your own releases
  column"). `NameChange` (244-248). `StepError` (251-262).

### 2.2 Steps, in rank order (src/postprocess/steps.rs:10-26)

| id | rank | step | needs | enabled when | reruns | config_stamp | writes |
|---|---|---|---|---|---|---|---|
| 1 | 100 | `archive::Par2Step` (src/archive/steps.rs:185-211) | Bodies | archive.enabled | - | - | `archive_inner_files`, `archive_release`, `release_fingerprint.par2_*` |
| 2 | 110 | `archive::ListStep` (src/archive/steps.rs:367-386) | Bodies | archive.enabled | - | - | `archive_inner_files(source)`, `archive_release(encryption, names_hidden, password_check)` |
| 4 | 120 | `probe::ProbeStep` (src/probe/step.rs:19, 86-101) | Bodies | archive.enabled; not `.jpg` | on_growth | `1 \| 2` (a bit per probe version) | `release_probe(crc32, agent, info JSON, moviehash)` |
| 5 | 130 | `OpenSubtitlesStep` (src/probe/opensubtitles.rs:163-192) | External | opensubtitles key | - | - | `release_opensubtitles` |
| 8 | 200 | `naming::Naming` (src/naming/steps.rs:20-41) | Database | naming.enabled | on_growth | - | `releases.display_name/is_obfuscated/name_source` |
| 9 | 210 | `naming::NfoName` (src/naming/steps.rs:75-97) | Bodies | archive.enabled && naming.nfo_step | - | - | name |
| 16 | 300 | `predb::step::PredbMatch` (src/predb/step.rs:159-180) | Database | predb on | - | - | `predb_match`, `predb_inner_key`, name (source PreDb=100) |
| 24 | 400 | `category::CategoryStep` (src/category/step.rs) | Database | always | on_rename, on_growth | - | `release_category(release_id, category)` |
| 32 | 500 | media `parse` (src/media/steps.rs) | Database | media on | - | - | `media_release` |
| 33 | 510 | `MediaLookup` (src/media/steps.rs ~270) | External | media providers | - | `provider_bits()` | `media_release` |
| 34 | 520 | media `nfo` | Bodies | - | - | - | `media_release` |
| 35 | 505 | `OpenSubtitlesImdb` (src/probe/opensubtitles.rs:241-275) | Database | - | - | - | `release_opensubtitles` |

The tests at the end of steps.rs pin id ranges to rank ranges (ids 1..=7 <-> ranks 100-199,
8..=15 <-> 200-299, 16..=23 <-> 300-399, 24..=31 <-> 400-499, 32..=39 <-> 500-599). A new
step picks the next free id in the band for its rank.

`NameSource` priorities (src/naming/mod.rs:28-41): Ingest 0, Cleared 1, PostedFile 3,
NamingRegex 5, InlineBody 9, Nfo 10, ArchiveInner 15, Par2 20, PreDb 100. The label the page
prints comes from `release_info::name_source_label` (src/release_info.rs:24-35).

### 2.3 Stage tables and the plug-in registry (src/postprocess/schema.rs)

- `ShardTable { name, create, indexes, release_key, added }` (8-18).
- `pp_queue(release_id PK, due_at)` + `pp_queue_due` (21-30): "written by saves and rerun
  requests. Walks dont use it".
- `pp_state(release_id PK, done, failed, net_wanted, db_attempts, net_attempts, net_next_at,
  parts_seen, last_error, rerun_gen, rerun_mask)` + partial indexes `pp_state_net`,
  `pp_state_failed` (33-57): "only releases that passed the rules and had a step run.
  Rejected rows have none."
- `pp_walks(id, kind, top_id, before_id, redo_mask, group_name, seen, ran, created_at)`
  (60-75): "a resumable pass over existing releases, newest first".
- `SHARD_TABLES` (78-92): the three above plus `archive::store::{INNER_FILES, RELEASE,
  NFO_PASSWORD}`, `duplicates::FINGERPRINT`, `probe::PROBE`, `opensubtitles::OPENSUBTITLES`,
  `predb::step::{MATCH, INNER_KEY}`, `category::RELEASE_CATEGORY`, `media::MEDIA_RELEASE`.
- `RELEASE_COLUMNS = [("name_source", "INTEGER")]` (96): "nullable columns on `releases`,
  append only".
- `migrate` (114-130): creates each table, adds missing `added` columns (`add_missing`
  102-112), creates indexes, and for tables with a `release_key` installs
  `create trigger if not exists gc_{table} after delete on releases begin delete from
  {table} where {key} = old.id; end;` (124-127).
- Compaction copies every registered table (src/compact.rs:745-752), rebuilds FTS (756-758)
  and `refuse_what_wouldnt_be_copied` (900-915) aborts when a shard holds a table that is
  not registered. Registering is therefore mandatory, not optional.

Plug-in table shapes: `archive_inner_files(release_id, seq, name, size, source)`
(src/archive/store.rs:18-39); `archive_release(release_id, password; added encryption,
names_hidden, password_check)` (42-51); `release_nfo_password(release_id, password)` (54-63);
`release_fingerprint(release_id, posted, content; added par2_block, par2_blocks,
posted_names)` (src/duplicates.rs:21-40); `release_probe(release_id, crc32, agent, info TEXT
JSON; added moviehash)` with indexes on crc32/agent/moviehash (src/probe/mod.rs:23-47);
`release_opensubtitles` (src/opensubtitles.rs:27-41); `predb_match` (src/predb/step.rs:25-39),
`predb_inner_key` (42-53); `release_category(release_id, category)`
(src/category/step.rs:13-22).

`ProbeInfo` JSON (src/probe/mod.rs:53-78): yenc, file_crc32, segment_size,
overhead_percent, continuity/part checks, `health` map (first/middle/last -> there/gone),
`headers` BTreeMap limited to `KEPT_HEADERS` (81-94: Injection-Date, NNTP-Posting-Date, Date,
User-Agent, X-Newsreader, X-Newsposter, Path, Injection-Info, Organization, Xref,
NNTP-Posting-Host, X-Complaints-To; values cut at 512 chars, 96-104), `crossposted_to`
(from `xref_groups`, 112-113), archive, layers, media, nfo, imdb_ids, sfv, moviehash.
This is the only place where a raw article header survives, and only for the one or few
articles the probe fetched.

### 2.4 Flow through the stage

1. `Stage::start` (src/postprocess/mod.rs:82-107): seeds walks per shard (`engine::seed`
   286-306 -> `seed_shard` 1073-1114), starts `db_workers = postprocess.workers` threads
   named `atlas-pp-db-{i}` and net threads only when a Bodies/External step is enabled
   (`has_net_steps`). It runs only inside the indexer process (mod.rs:1-4); started from
   `bg_indexer::run_servers` when `postprocess.enabled` (src/bg_indexer.rs:850-862) with
   `steps_to_run()` (mod.rs:58-60).
2. DB lane (`db_lane` src/postprocess/engine.rs:342-367): per shard, drains due `pp_queue`
   rows in `DB_BATCH=256` (27) and then `walk_tick` (412-486) for walks.
3. `walk_tick`: takes the newest walk (`order by id desc limit 1`), skips when
   `LOAD.writer_queued > 2*SHARDS` (418), takes a token from a per-shard bucket paced at
   `backlog_per_second / SHARDS`, reads
   `select {ROW_COLUMNS} from releases r left join pp_state s on ... where r.id < ?1 [and
   r.group_name = ?3] order by r.id desc limit ?2` with `WALK_BATCH=256` (28), skips rows that
   already have a future queue row unless `redo != 0`, visits them, then
   `update pp_walks set before_id = ?, seen = seen + ?, ran = ran + ?` and deletes the walk
   when exhausted. `ROW_COLUMNS` (91-94) is the `ReleaseInfo` projection.
4. `visit` (557-740): rules first (`rules::judge_row` 297-307: blocked group / empty / too
   small; `file_summaries` 311-347 reads `files.filename, subject_part, expected, seen,
   touched_at, subject`; `judge_files` 351-377: settle wait is the only `Wait`
   (`settle_secs` 7200 s, `complete_settle_secs` 900 s), reject fragment / min_files /
   par2-only). Growth rerun (599-601) clears `on_growth` steps when `parts_seen` grew. DB
   steps run in rank order; a rename clears `on_rename` steps; errors go to
   `apply::apply_one` (src/postprocess/apply.rs:65-111) which records `failed`, attempts,
   `last_error` and requeues with `backoff` (engine.rs:47-50:
   `(900*4^(min(attempt-1,16))).min(604_800) + id%60` seconds, so 15 min, 1 h, 4 h, 16 h,
   capped at 7 d; `max_attempts` 5). Net steps are gated by `rules::body_gate` (392-404)
   and `pp_state.net_wanted`.
5. Net lane (`net_lane` 763-797, `net_claim` 799-821 takes `NET_BATCH=4` (29), every 8th
   claim takes the oldest, `net_step` 877-1036). Bodies through `BodyFetcher`
   (src/postprocess/bodies.rs): token bucket `body_fetches_per_minute` (103-122), semaphore
   `body_connections` (124-177), `body_max_bytes`, `bodies_per_release`.
6. `apply::write` (src/postprocess/apply.rs:113-225): drops stale results by `rerun_gen`,
   applies `StepWrite`s, `update releases set display_name = coalesce(?2, display_name),
   is_obfuscated = coalesce(?3, is_obfuscated), name_source = ?4` for names, upserts
   `pp_state` (175-203) and handles the queue row (206-223). All of it runs on the shard
   writer thread inside the stage budget (section 1.7).
7. `refresh_counts` (engine.rs:314-336) every 30 s (mod.rs:150-162) feeds
   `postprocess_backlog` in status.json (src/bg_indexer.rs:47-66) and the snapshot in
   stats.json (463-505; keys in src/postprocess/stats.rs:309-332).

Config (`PostprocessConfig`, src/config.rs:596-639): enabled true, workers 2, settle_secs
7200, complete_settle_secs 900, min_size_bytes 0, min_files 1, reject_par2_only true,
backlog_per_second 2000, body_fetches_per_minute 600, body_connections 2,
body_min_size_bytes 1 MiB, body_max_size_bytes 100 GiB, body_max_bytes 16 MiB,
bodies_per_release 8, min_completion_percent 95, dead_after_secs 172800, max_attempts 5.
`set_config` (engine.rs:233-251) refuses to enable a Bodies step without a body budget.

### 2.5 Re-running a step on existing releases (four mechanisms)

1. **Automatic walk on enable** (`seed_shard`, src/postprocess/engine.rs:1073-1114). Meta
   keys `pp_walk_seeded`, `pp_steps_seen`, `pp_stamp_{id}`. First ever seed inserts a
   `backlog` walk with `redo_mask = 0` from `max(id)+1` downwards ("it visits every row").
   Later, any newly enabled step bit (`enabled & !seen`) inserts a `steps` walk with
   `redo 0`: rows are visited and steps whose bit is not in `done` run. So a brand new step
   with `enabled() == true` reaches every existing release once, newest first, without any
   command.
2. **`config_stamp` bit** (same function, 1104-1112): when a stored stamp gains a bit
   (`stamp & !stored != 0`) a `steps` walk with `redo_mask = bit` is inserted, which clears
   that step's `done` on each row visited (the `redo != 0` path in `walk_tick`). This is how
   the probe (`config_stamp() = 1 | 2`, src/probe/step.rs:99-101) and media lookup
   (`provider_bits()`) get redone after a version bump.
3. **Operator command** `atlas --reprocess --step <name|all> [--group G] [--failed] --apply`
   (src/postprocess/reprocess.rs:74-99). Dry run is the default (110-135): counts `pp_state`
   rows with `done & mask` and prints `about {total/per_second} s to walk them at
   backlog_per_second`. `apply` (25-40) inserts one `reprocess` walk per shard from
   `max(id)+1` with `redo_mask` and optional `group_name`, refusing a duplicate walk, and
   for naming sets meta `naming_rename_named[:group]` (28-31) so the step may replace names
   it left alone. `--failed` (`retry_failed` 44-65) clears `failed` bits 1,000 rows per
   transaction. `apply_all` (137-162) takes `hold_off_compaction`, runs `migrate_shard`, and
   `BEGIN IMMEDIATE`. The walk itself still runs inside the indexer (mod.rs:1-4), paced by
   `backlog_per_second`.
4. **Per release** `Stage::rerun(release_id, mask)` (mod.rs:117-123) -> `rerun_on`
   (63-78):

   ```sql
   insert into pp_state (release_id, rerun_gen, rerun_mask) select ?1, 1, ?2 where exists (select 1 from releases where id = ?1)
   on conflict(release_id) do update set done = done & ~?2, failed = failed & ~?2, db_attempts = 0, net_attempts = 0,
     net_next_at = 0, rerun_gen = rerun_gen + 1, rerun_mask = rerun_mask | ?2;
   insert into pp_queue (release_id, due_at) select ?1, ?2 ... on conflict(release_id) do update set due_at = excluded.due_at;
   ```

   Plus the implicit `on_growth` / `on_rename` reruns in `visit` (engine.rs:599-601 and the
   rename path), and `duplicates::forget_old_twins` (src/duplicates.rs:97-117), a startup
   raw-SQL job that clears the PAR2 bit and re-queues rows (precedent for a one-shot
   migration-style rerun).

Config reload: `bg_indexer::watch_config` (src/bg_indexer.rs:680-734) every
`CONFIG_RELOAD=5s` (30) calls `stage.set_config`, which re-seeds, so flipping a step's
`enabled` flag in the config file is enough to start its walk; a change of `StageShape`
(662-675) rebuilds workers.

### 2.6 Backlog handling and pools

- Walk pace: `backlog_per_second` (default 2000) shared across `SHARDS=8` shards
  (`walk_tick` token bucket). A full pass over N releases takes about N/2000 s of wall time
  when the writer is idle; `walk_tick` yields entirely while `LOAD.writer_queued >
  2*SHARDS` (engine.rs:418), so under ingest pressure a walk stalls rather than competing.
- Stage work shares the writer thread with saves under the 10 ms/100 ms budgets
  (src/indexer.rs:230-231), so per-release stage cost directly delays ingest commits.
- Bodies: `body_fetches_per_minute` 600, `body_connections` 2, `bodies_per_release` 8, so
  body-driven backfills are bounded at roughly 75 releases/min at best.
- Backoff on failure (engine.rs:47-50) and `max_attempts` 5; `dead_after_secs` 2 d.
- `purge` (src/postprocess/purge.rs) is the offline sibling: `PURGE_BATCH=2000`,
  `PURGE_TX_ARTICLES=200_000`, `PURGE_SEGMENT_CHUNK=20_000`, `AT_ONCE=4` (20-24), classes
  (27-34), `classify` (90-129), resumable `apply_shard` (330-429) through meta keys
  `purge_top/purge_classes/purge_below/purge_after/purge_victim`, refuses to run while the
  indexer is alive (`main` 478-551). `batch.rs` gives `COMMITS_PER_CHECKPOINT=64`,
  `WAL_LIMIT=4GiB`, `checkpoint_or_stop` (229-246).
- Process control (src/procs.rs): `BG_FLAG="--bg-indexer"`, `indexer_alive`,
  `start/stop_background_indexer`, `get_status` reads status.json and marks it `stale`.

### 2.7 Background indexer (what matters for a backfill)

`bg_indexer::run_servers` (src/bg_indexer.rs:774-905): `store::start_grouping` (823, meta
`grouping_from`, src/store.rs:1587-1595), `claim_plain_cursors` (827), `Stage::start`
(850-862), predb service (863-879), `media::run` (880-883), workers (884-888).
`bg_indexer::index` (1148-1189) also starts `media::details::start_backfill(stop)` (1172) and
runs `duplicates::forget_old_twins` (1174-1176) at startup. `supervise` (970-1046) and `run`
(1048-1120) handle restarts; `COMPACT_EVERY=24h` (33). Status every second (`write_status`
47-66, 463-505; `Stats::write` 188-228).

`media::details::start_backfill` (src/media/details.rs:673-686) is a plain std thread that
calls `backfill_pass` (640-663) paced by `details_per_minute`, resting `BACKFILL_REST=6h`
(39) with `BACKFILL_DELAY=30s` (41). This is the precedent for a paced, stage-independent
backfill thread.

---

## 3. Data model summary

| table (per shard unless noted) | columns | written by | read by |
|---|---|---|---|
| `releases` | id, name, group_name, poster, posted_date, size, complete, parts, file_total, display_name, is_obfuscated, name_source | `store::save_in_transaction` (src/store.rs:1367-1569), `apply::write` names (apply.rs:113-225) | `search::get_release*` (src/search.rs:557-583), `engine::ROW_COLUMNS`, release page |
| `releases_fts` | name, display_name | triggers (src/store.rs:150-163) | search |
| `files` | id, release_id, filename, subject, subject_part, subject_mid, expected, file_total, seen, touched_at, blob | `put_file` (632-639), sealing in `housekeeping` | `release_files`, `each_article`, `rules::file_summaries`, `release_info::union_files` (393-436), nzb |
| `domains` | id, suffix | `add_segment` | `each_article` |
| `segments` | file_id, local, domain, part, bytes | `add_segment` (645-705) | `each_article` (NZB, contents), sealing |
| `collections` | key (hashed), release_id | save (1454-1455) | collection grouping on later saves |
| `held_back`, `release_ids` | (migrate_shard 202-247) | save | save |
| `meta` | key, value | totals, grouping_from, purge_*, pp_walk_seeded, pp_steps_seen, pp_stamp_{id}, naming_rename_named | stage seed, status |
| `pp_queue` | release_id, due_at | save (1545-1552), `rerun_on`, `apply::write` | `db_lane` |
| `pp_state` | release_id, done, failed, net_wanted, db_attempts, net_attempts, net_next_at, parts_seen, last_error, rerun_gen, rerun_mask | `apply::write`, `rerun_on`, `retry_failed` | `walk_tick`, `visit`, `net_claim`, `dry_run`, `refresh_counts` |
| `pp_walks` | id, kind, top_id, before_id, redo_mask, group_name, seen, ran, created_at | `seed_shard`, `reprocess::apply` | `walk_tick` |
| `release_probe` | release_id, crc32, agent, info JSON, moviehash | ProbeStep | `web::release_html` probe panel, agent search |
| `archive_inner_files` / `archive_release` / `release_nfo_password` | see 2.3 | Par2Step, ListStep, NfoName | `archive::store::view`, release page Files inside / Encryption / Password |
| `release_fingerprint` | release_id, posted, content, par2_block, par2_blocks, posted_names | Par2Step, save path | `duplicates::copies_with/clusters_with`, `filled_nzb` |
| `release_category` | release_id, category | CategoryStep | `category::categories_of_with` (src/category/mod.rs:105-124) |
| `predb_match`, `predb_inner_key`, `media_release`, `release_opensubtitles` | see 2.3 | their steps | details panel, Links fact |
| main db `groups` | name, live_cursor, backfill_cursor, first_article, last_article, low_article, low_posted, high_article, high_posted | `db::save_group_dates` (616-628), cursors | status/ETA |
| main db `backfill_chunks`, `backfill_deepest`, `backfill_sweeps`, `backfill_sweep_gen`, `backfill_speculative` | (src/chunks.rs:18-55) | scheduler | scheduler |

---

## 4. UI surface of the release page

`release_page` (src/web.rs:989-1017) loads `search::get_release`, `release_info::names`,
`archive::store::view`, `media::query`, the category, the probe JSON and copies, then
`release_html` (1028-1201) renders:

- Tags row (1037-1045, 1054): OBFUSCATED, incomplete, passworded, etc.
- Facts `<dl>` through the closure `fact(name, value)` (1047-1052) in this order: Category
  (1055), Title (1058-1060), Size (1061-1066), **Posted** via `fmt_date` (1067; ui.rs
  531-533 keeps only the first 10 chars, so date only), **Groups** when
  `release_info::groups_of(posted_name)` has more than one posting, else **Group**
  (1069-1078), **Poster** as a `?poster=` search link and **Email** with a `MADE UP` tag
  (1079-1086; `parse_poster` src/release_info.rs:451-472, `looks_made_up` 501-508,
  `MADE_UP_DOMAINS` 476-499), **Posted as / Name from** (1087-1094), Year / Ripped from /
  Site / Video / Audio / Languages / Edition / Release group from `release_tags::parse` of
  the name (1095-1121), **Parts** (`releases.parts`, 1122-1124), Encryption (1125),
  Password (1126-1128), **Files / PAR2 files / Articles (health) / Repair** from
  `release_info::Contents` (1129-1138), Links (1139-1142).
- "Files inside" table of archive inner files (1144-1161), not NZB files.
- Probe panel `probe_html` (1163-1170; 1243-1453): yEnc panel incl. "On the server"
  FIRST/MIDDLE/LAST THERE/GONE (1277-1288); "How it was posted": agent as `?agent=` link
  (1291-1293), every kept header except User-Agent/X-Newsreader/X-Newsposter/Xref
  (1294-1298), "Crossposted to" group links (1299-1302); Packing (1304-1328); Tracks
  (1330-1396); OpenSubtitles (1398-1414); Chapters (1415-1423); NFO (1430-1432); SFV
  (1433-1444); panel order (1445-1452). `panel()` helper (1235-1240).
- Other copies (twins/alternates) and the "NZB, gaps filled from a twin" button (1171-1181;
  `copy_card` 1463-1548, `copies_html` 1555-1572, `FULL_COPIES=12` 1456, full list at
  `duplicates_page` 1575+).
- Layout (1183-1200) with `<div id="title-details" hx-get="/ui/details?id={id}"
  hx-trigger="load">` (1190) -> `details_panel` (1657-1663) / `details_html` (1679-1715) for
  media metadata.

`release_info` is explicitly "Read when the page opens, soo nothing here is stored"
(src/release_info.rs:1-2): `Contents` (39-60) via `contents_of` (160-200: `union_files` +
`each_article` byte sum + `duplicates::par2_sizes`), `missing_per_mille` (64-67),
`names_with` (136-150), `groups_of_with` (216-235: same posted name across shards, 64 per
shard), `split_post_with` (266-298; `SPLIT_POST_DAYS=1.0` 258, `SPLIT_POST_GROUPS=16` 261,
SQL at 303-308 `where name = ?1 and +poster is ?2 and not atlas_blocked(group_name)`),
`password_hint_with` (539-564).

NZB (`nzb::render_nzb_in`, src/nzb.rs:66-113): XML decl utf-8 (68), DOCTYPE (69-71), xmlns
(72), `<meta type="category">{group_name}</meta>` and nothing else in `<head>` (73); files
grouped by filename (76-79); `<file poster="{release poster}" date="{article_timestamp(release
posted_date)}" subject="{files.subject}">` (83-89); `<group>` per entry of `groups` (90-94,
from `inventory` 121-134 which lists all groups of a detected split post); `<segment
bytes="{segments.bytes}" number="{segments.part or 'None'}">{message id without <>}</segment>`
(97-106). `build_nzb_file` (148-159). The download responses carry only Content-Type
`application/x-nzb` and Content-Disposition (src/web.rs:1210-1216 `filled_nzb`,
`get_nzb` at 65; src/api.rs:388-390).

Newznab attrs on the API (src/api.rs:284-304): poster, group, resolution, video, audio, hdr,
source, language, team; `password` (330-332); `category` (337). Not part of the release
page but the same `ReleaseRow` fields.

---

## 5. Catalog coverage (assigned items)

See section 0 for the status vocabulary. "page" means `/ui/release`.

| catalog item | status | evidence |
|---|---|---|
| derived_per_article.subject_parse | stored_not_shown | Parsed into `Article.filename/release_name/part/total_parts/file_index/file_total` (src/parser.rs:83-125); persisted per file as `files.filename, subject, subject_part, expected, file_total` (src/store.rs:165-178, 582-601). Page shows only aggregates: Parts (src/web.rs:1122-1124), Files / Articles (1129-1137); no per-file subject or counter. `file_index` and the size/poster suffixes are parsed_not_stored (parser.rs:93-99, 113-116). |
| derived_per_article.subject_template | parsed_not_stored | `filters::shape` hash (src/filters.rs:408-418) and `collections::subject_base` (src/collections.rs:81-88) exist only during a slice; the collection key is stored hashed as `collections.key` (src/store.rs:207, 1454-1455; `IdKey` 714-720), unreadable. |
| derived_per_article.part_bitmap | stored_not_shown | `files.seen BLOB` (src/store.rs:174; `FileState::add` 582-601; `seen_count` 1790-1792). Rendered only as the Articles health per-mille (src/web.rs:1135-1137 via `release_info::contents_of` 160-200); gap positions are not shown. Duplicate arrivals are not counted (`insert or ignore`, src/store.rs:688-690). |
| derived_per_article.message_id_grammar | stored_not_shown | `segments.local` + `domains.suffix` (src/store.rs:180-188, 688-690); read back only by `each_article` (1947-1975) for the NZB (src/nzb.rs:97-106). Nothing analyses or shows the shape. |
| derived_per_article.message_id_timestamp | not_collected | No decoder of message-id contents exists; the only readers are the NZB path and sealing (src/store.rs:1947-1975, src/nzb.rs:97-106). |
| derived_per_article.from_identity | shown (release level only) | `releases.poster` (src/store.rs:1394) is the first article of the latest slice (src/indexer.rs:1785); shown as Poster/Email with `MADE UP` tag (src/web.rs:1079-1086; src/release_info.rs:451-508). Per-article `From` is dropped (segments has no author, src/store.rs:688-690). |
| derived_per_article.date_shape | parsed_not_stored | `to_iso_date` (src/dates.rs:132-137) drops the offset and formatting; the raw header survives only in `release_probe.info.headers.Date` for probed articles (src/probe/mod.rs:81-94) and is rendered there (src/web.rs:1294-1298). |
| posted dates (date_shape / posting_rate / timing_profile) | shown (date only, release level) | Posted fact = `fmt_date(posted_date)` first 10 chars (src/web.rs:1067; src/ui.rs:531-533). `posted_date` is overwritten on each re-save (src/store.rs:1395-1396). Per-article dates are dropped, so posting_rate and timing_profile are not_collected. Group-level ends live in `groups.low_posted/high_posted` (src/db.rs:335-342, 616-628), not per release. |
| derived_per_article.bytes_lines_ratio | parsed_not_stored | `Overview.lines` parsed (src/nntp.rs:91-100, 2712-2727), `Article.lines` (src/parser.rs:20) never written. |
| derived_per_article.article_number_adjacency | parsed_not_stored | `Article.number` used for cursors and `slice_date` (src/indexer.rs:1718-1730) only; not in `segments`. |
| derived_per_article.posting_rate | not_collected | see posted dates. |
| derived_per_article.arrival_curve | not_collected (per release) | Only `files.touched_at` last-write time (src/store.rs:175, 632-639) and group ends in `db.groups`. |
| derived_per_article.crosspost_kind | shown (probed releases only) | Ingest ignores the Xref overview field (src/nntp.rs:2712-2727). ProbeStep keeps `Xref` and `crossposted_to` (src/probe/mod.rs:81-113) and the page lists "Crossposted to" (src/web.rs:1299-1302). Same-name postings in other groups appear as Groups (src/web.rs:1069-1078; src/release_info.rs:216-235). |
| derived_per_post.contents_profile | shown (computed on open) | `release_info::contents_of` (src/release_info.rs:160-200) -> Files / PAR2 files / Articles / Repair facts (src/web.rs:1129-1138). Nothing stored. |
| derived_per_post.obfuscation_level | shown (boolean only) | `releases.is_obfuscated` (src/store.rs:1399), OBFUSCATED tag (src/web.rs:1043-1045), Name from label (1087-1094); per-article uploads carry `per-article.*` file names (src/filters.rs:566-568). No graded level is stored. |
| derived_per_post.timing_profile | not_collected | per-article dates dropped (src/store.rs:688-690). |
| derived_per_post.completeness_over_time | stored_not_shown | `files.touched_at` (src/store.rs:175, 632-639) and `pp_state.parts_seen` (src/postprocess/schema.rs:33-57) are the only traces; used by `rules::file_summaries` (src/postprocess/rules.rs:311-347) for the settle wait, never rendered. |
| derived_per_post.takedown_pattern | shown (undated snapshot) | Probe `health` first/middle/last (src/probe/mod.rs:53-78) rendered as THERE/GONE (src/web.rs:1277-1288); no history across probes (the probe row is replaced). |
| derived_per_post.group_spray / group lists | shown | Groups fact (src/web.rs:1069-1078) from `groups_of_with` (src/release_info.rs:216-235); split posts `split_post_with` (266-298); NZB `<group>` list (src/nzb.rs:90-94, 121-134). |
| poster_profile.stable_keys / habit_keys / linked_identities | not_collected | No per-poster aggregation table. Poster and agent are searchable per release (`?poster=` src/web.rs:1081; `?agent=` 1291-1293; `idx_release_poster` optional via `atlas --search-indexes`, src/field_search.rs:18, 380-381; `release_probe` agent index src/probe/mod.rs:23-47). |
| nzb.nzb_xmlns | not_collected | Constant in the writer (src/nzb.rs:72); nothing per release. |
| nzb.head_meta | stored_not_shown | Only `category = group_name` is emitted (src/nzb.rs:73); `archive_release.password` (src/archive/store.rs:42-51) is shown as a Password fact (src/web.rs:1126-1128) but never written into the NZB head; no title/name meta. |
| nzb.file_poster | shown (release level) | NZB uses the release poster for every file (src/nzb.rs:83-86; `each_article` 1954-1968); page Poster fact (src/web.rs:1079-1086). |
| nzb.file_date | shown (date only) | NZB date = `article_timestamp(release posted_date)` (src/nzb.rs:86-87; src/dates.rs:167-185 converts the naive wall clock through `Local`, so the epoch can be off by the server zone); page shows the date part (src/web.rs:1067). |
| nzb.file_subject | stored_not_shown | `files.subject` (lowest part's subject, src/store.rs:582-601) is emitted at src/nzb.rs:88 and never rendered; the "Files inside" table (src/web.rs:1144-1161) lists archive inner files, not NZB files. PR 48 adds an NZB file list (`nzb_files_html`). |
| nzb.groups_group | shown | see group lists. |
| nzb.segment_bytes / segment_number | stored_not_shown | `segments.bytes/part` (src/store.rs:688-690; nzb.rs:97-106); page shows only Size (web.rs:1061-1066), Parts (1122-1124) and health. |
| nzb.segment_text (message ids) | stored_not_shown | `segments.local/domain` decoded only for the NZB (src/store.rs:1947-1975, src/nzb.rs:103-105). |
| nzb.nzb_identity | stored_not_shown | `release_fingerprint(posted, content, par2_*)` (src/duplicates.rs:21-40); its effect (twins/alternates, gap filling) is shown as Other copies (src/web.rs:1171-1181, 1555-1572) but no identity value is. |
| nzb.external_oracles | unclear | Atlas queries no NZB oracle. Nearest: PredbMatch (src/predb/step.rs:159-180; tables 25-53) whose result surfaces only as "Name from PreDb" (src/web.rs:1087-1094; src/naming/mod.rs:28-41) and media/OpenSubtitles lookups. Whether that is what the item means needs the catalog owner. |
| derived_additions.nntmux_collection_key | stored_not_shown | `collections(key INTEGER PRIMARY KEY, release_id)` (src/store.rs:207, 1454-1455) hashed from `Collections::of` (src/collections.rs:262-278). |
| derived_additions.partrepair_window | stored_not_shown | Missing parts are only the zero bits of `files.seen`; retries happen through `CHUNK_OVERLAP=3600`/`CHUNK_SAFETY=5000` re-reads (src/indexer.rs:1073-1077); no repair table or window is kept. |
| derived_additions.last_article_bytes_key | stored_not_shown | `segments.bytes` per part (src/store.rs:688-690); not used as a key, not shown. |
| derived_additions.lines_sequence_key | parsed_not_stored | `Article.lines` dropped (src/parser.rs:20; src/store.rs:688-690). |
| derived_additions.sequential_file_tiling / session_segmentation | not_collected | needs per-article numbers/dates, which are dropped (src/store.rs:688-690). |
| derived_additions.repost_and_retry_events | shown (reposts) / not_collected (retries) | Reposts surface as twins/alternates via `release_fingerprint` (src/duplicates.rs:21-40; src/web.rs:1171-1181); re-sent articles are swallowed by `insert or ignore` (src/store.rs:688-690). |
| derived_additions.x_dnzb_response_headers | not_collected | NZB responses carry only Content-Type and Content-Disposition (src/web.rs:1210-1216; src/api.rs:388-390). |
| derived_additions.nzb_writer_fingerprint | not_collected | Atlas writes NZBs and ingests none; no generator meta (src/nzb.rs:66-74). |
| derived_additions.nzblnk | not_collected | no occurrence in src/ (grep). |
| derived_additions.newznab_attrs_and_endpoints_not_emitted | unclear (API area) | emitted attrs at src/api.rs:284-304, 330-337. |

Not assessed here (other areas): sabnzbd_obfuscation_rules, clock_skew_from_archive_times,
zero_download_inner_size, downloader_history_fields, torrent_sidecars, spotnet_ground_truth.

---

## 6. Extension points

### 6.1 Adding a postprocess step

1. Implement `Step` (src/postprocess/step.rs:37-65) with a fresh `StepId` in the band for
   its rank (steps.rs tests), `Needs::Database` for anything derivable from stored rows,
   `enabled()` tied to a config flag, `applies()` to skip unsuitable releases, `reruns()`
   `on_growth` if the result depends on parts/files.
2. Register it in `steps::all()` (src/postprocess/steps.rs:10-26).
3. Give it a table: a `ShardTable` const (schema.rs:8-18) appended to `SHARD_TABLES`
   (78-92). `migrate` (114-130) creates it, adds `added` columns, and installs the gc
   trigger; compaction copies it (src/compact.rs:745-752). Or append a nullable column to
   `RELEASE_COLUMNS` (schema.rs:96) if a single scalar per release is enough (saves then
   leave it alone only if the step's writes avoid the upsert's columns; the upsert touches
   `poster, posted_date, display_name, is_obfuscated` only, src/store.rs:1390-1400).
4. Return `StepWrite`s (step.rs:225-237) from `run`; `apply::write` executes them on the
   writer thread (apply.rs:113-225).
5. Backfill is then automatic: the first `set_config` with the new bit enabled inserts a
   `steps` walk per shard (`seed_shard`, engine.rs:1089-1092) that visits every release
   newest first at `backlog_per_second/SHARDS`. Later redo: bump `config_stamp` or run
   `atlas --reprocess --step <name> --apply`.
6. Caveat: `rules::judge_row/judge_files` (rules.rs:297-377) run before any step, so
   releases that are blocked, empty, below `min_size_bytes`, below `min_files`, fragments or
   par2-only never get a `pp_state` row and never get the step. A UI-field step that must
   cover literally every release has to bypass or relax the rules (or run as a separate job,
   6.3).

### 6.2 Adding a field that is derivable from stored rows at page time

The `release_info` pattern (src/release_info.rs:1-2, 160-200): compute in `release_page`
and add a `fact(...)` call in `release_html` (src/web.rs:1047-1142) or a `panel(...)`
(1235-1240). No schema, no backfill, no stage. Cost is one `each_article` scan per page
open (src/store.rs:1947-2008; sealed blobs decoded), already paid for `Contents`. Anything
that only needs `files`/`segments`/`releases`/`release_probe` (part bitmap gaps, per-file
subjects, message-id domains, segment sizes, group lists, poster parsing) fits here.

### 6.3 A backfill job over ALL releases outside the stage

Precedents, in order of weight:

- Startup raw SQL per shard: `duplicates::forget_old_twins` (src/duplicates.rs:97-117),
  called from `bg_indexer::index` (1174-1176). One statement per shard; fine for a mask
  flip, not for row-by-row work.
- Meta-marker gated behaviour: `store::start_grouping` (src/store.rs:1587-1595, meta
  `grouping_from`) changes how later saves treat old rows (`split_old_rows` 1602-1654)
  without touching them up front.
- Paced thread: `media::details::start_backfill` (src/media/details.rs:673-686;
  `backfill_pass` 640-663, `details_per_minute`, `BACKFILL_REST=6h`).
- Resumable cursor with checkpoints: `purge::apply_shard` (src/postprocess/purge.rs:330-429;
  meta `purge_top/..`, `batch::checkpoint_or_stop` 229-246, `WAL_LIMIT=4GiB`), offline only
  (`main` refuses while the indexer runs, 478-551).
- The stage's own `pp_walks` with `kind='backlog'`/`'steps'`/`'reprocess'` (schema.rs:60-75;
  engine.rs:412-486) is the only resumable newest-first iterator that runs inside the live
  indexer and respects writer load (`LOAD.writer_queued`, 418).

Writes from any of these must go through the shard writer (`Db::on_writer`,
src/indexer.rs:450-458) or hold the shard's `begin immediate` themselves, since the writer
thread owns the connection.

### 6.4 Keeping more per-article data for new ingest

Would need columns on `segments` (src/store.rs:181-188) or a sibling table keyed
`(file_id, local, domain)`, writes in `add_segment` (645-705), a sealed-blob format change
(`housekeeping` src/indexer.rs:422-445; `each_article` decode 1947-2008), compaction awareness
(src/compact.rs:745-752) and purge chunking (purge.rs:20-24). Existing releases cannot be
backfilled for those fields: the overview rows are gone, and re-fetching headers for old
article ranges is not something the scheduler does once a chunk is `done`
(src/chunks.rs:18-55).

---

## 7. Risks

- **Irrecoverable per-article data.** From, Date, article number, lines, references and
  per-article subjects are dropped at save (src/store.rs:688-690). Any catalog field that
  needs them cannot be backfilled for existing releases; only re-indexing from the server
  (bounded by retention) could supply them.
- **Release poster/date semantics.** Both are overwritten on each re-save with the first
  article of the latest slice (src/indexer.rs:1785-1786; src/store.rs:1394-1396) and the
  date has no zone (src/dates.rs:132-137; `posted_timestamp` guesses `Local`, 173-185).
  Showing them as "first posted" would mislead.
- **Walk cost and ingest coupling.** Stage work runs on the writer thread under
  10 ms/100 ms budgets (src/indexer.rs:230-231) and stalls while `writer_queued >
  2*SHARDS` (engine.rs:418); a full `steps` walk is N/`backlog_per_second` seconds at best
  and every visited row pays the rules' `files` read (rules.rs:311-347). Body-backed steps
  are limited to ~600 fetches/min and 8 bodies per release (config.rs:617-639).
- **Rules filter.** Rejected releases never get `pp_state` rows (schema.rs:33) so a step
  cannot reach "every release page" without relaxing `judge_row/judge_files`.
- **Schema migrations.** New tables must be in `SHARD_TABLES` or compaction refuses
  (src/compact.rs:900-915); `RELEASE_COLUMNS` is append-only and nullable (schema.rs:96);
  `migrate_shard` runs on open of every shard (src/store.rs:202-247) so a heavy `alter`
  blocks startup across 8 shards; purge and reprocess also call `migrate_shard`
  (reprocess.rs:137-162).
- **Page-time computation.** `release_info` scans all segments of a release on every open
  (src/store.rs:1947-2008) and `groups_of_with` queries all 8 shards by name (release_info.rs:
  216-235); adding more per-open derivations multiplies that. Large releases (`files()`
  capped at 5,000 in the stage, step.rs:102-104; `MAX_COLLECTION_FILES=10_000`) are the
  worst case.
- **Conflicts with open work.** PR 48 (`pr/48`, head 06847b2) edits the same `release_html`
  facts region (hunks at 1091/1106/1115/1141/1150), `probe_html`'s signature,
  `release_info::Contents` (adds `NzbFile`, `MAX_LISTED_FILES=1000`, `list`), and
  `SHARD_TABLES` (adds `probe::COVER`). PR 57 rewrites sorting in web.rs (+495). PR 58
  touches `probe/codec` and `probe/nest.rs`; PR 55 the yEnc path in nntp.rs. The twin
  grouping work in the "Finish web UI" thread lives in `duplicates.rs` cluster helpers,
  `copy_card` and `duplicates_page` (web.rs:1463-1575). PRs 51/54 (`src/video/*`) do not
  overlap. No branch named `stream-details` exists in the refs; PR 48's "every stream
  field" is the likely referent.
- **Probe is not universal.** `ProbeStep` needs `archive.enabled`, skips `.jpg`
  (src/probe/step.rs:86-92) and a body budget; "Crossposted to", agent and raw Date headers
  exist only for probed releases, and `release_probe.info` is replaced on re-probe (no history).
- **Collections split by day/poster.** Weak collection keys include author and UTC day
  (src/collections.rs:276-277), so one post can be several releases; group lists and twins
  partially paper over this on the page.

---

## 8. Open questions

- Is the "stream-details branch" PR 48, or an unpublished branch? Nothing in refs matches.
- Should "every field visible" tolerate `not_collected` fields that are gone for existing
  releases (per-article From/Date/lines/number), or does the feature imply re-indexing?
- Does the backfill need to reach releases that the rules reject (fragments, par2-only,
  blocked groups)? If yes, a stage step is the wrong vehicle and 6.2/6.3 apply.
- Is `backlog_per_second=2000` acceptable for the live database size, and should a UI
  backfill share the stage's walk table or get its own `kind`?
- Should new per-release scalars go into `RELEASE_COLUMNS` (cheap to read with
  `search::COLUMNS`, src/search.rs:33-34) or a dedicated `ShardTable` (safer for compaction
  and gc)?
- Does the time zone of `posted_date` need fixing before it is shown with a time
  (src/dates.rs:132-137 vs 173-185)?
