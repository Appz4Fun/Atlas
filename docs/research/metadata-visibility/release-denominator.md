# Atlas map — area: release-denominator

Which releases can ever show which panels on `/ui/release?id=`: the rules, thresholds and config keys
that decide it, what `pp_state` records, how to count the denominator, whether rejected releases can be
reached, and where a "Processing" panel would go. Repo `/home/claude/appz4fun/atlas`, branch `main` at
`55d2e51`, read only. All paths below are relative to the repo root; `docs:` means
`docs/port/03-postprocess-stage.md`.

Scope note: no catalog items belong to this area (the task says so); this file maps the *denominator*
every other area's "shown" status silently assumes.

---

## 0. One-paragraph answer

A release page exists for every release whose group is not blocked (`search.rs:552-560` filters only
`group_blocked`, not `VISIBLE_RELEASE`), so hidden obfuscated releases do have pages. Every panel beyond
the basic facts is produced by a post-processing step, and a step runs only for a release that (1) passes
the rules in `src/postprocess/rules.rs` — not blocked, `parts > 0`, size ≥ `min_size_bytes`, has `files`
rows, settled for `settle_secs`/`complete_settle_secs`, not a stored fragment, `files ≥ min_files`, not
par2-only — (2) is in a shard whose stage is on (`postprocess.enabled`, `bg_indexer.rs:850`), (3) has the
step enabled by config (`Step::enabled`), (4) satisfies the step's `applies()` (name not an image), and,
for body steps, (5) passes `body_gate`: size within `body_min_size_bytes..body_max_size_bytes`, files not
all images, completion ≥ `min_completion_percent`. Rejected releases write **no** `pp_state` row
(`schema.rs:32`, `engine.rs:578-582`), settling releases only a `pp_queue` row (`engine.rs:590-594`), so
"releases with no pp_state row" = rejected ∪ settling ∪ never-visited ∪ passed-with-nothing-to-do. No
counter of that set exists today; the stage's `rejected` counters count visits since process start
(`stats.rs:27-32,48-58`). `atlas --reprocess --failed` cannot reach rejected releases (no row:
`reprocess.rs:44-65`); a plain `--reprocess --apply` re-judges them with the current rules
(`engine.rs:583-597`), so only relaxing `min_size_bytes` / `min_files` / `reject_par2_only` /
`group_rules` and then walking can widen the set; Blocked, Empty and Fragment are hard-coded.

---

## 1. Files read

- `src/postprocess/rules.rs` (all 371 lines)
- `src/postprocess/engine.rs` 1-1128 (all; key regions 557-766 `visit`, 799-1035 net lane, 1073-1112 `seed_shard`)
- `src/postprocess/apply.rs` 1-250 (write path), tests skimmed
- `src/postprocess/stats.rs` (all), `src/postprocess/schema.rs` (all), `src/postprocess/mod.rs` (all),
  `src/postprocess/step.rs` 1-200, `src/postprocess/steps.rs` (all), `src/postprocess/reprocess.rs` (all),
  `src/postprocess/purge.rs` (all), `src/postprocess/bodies.rs` 60-140
- `src/bg_indexer.rs` 30-80, 440-530, 664-680, 845-890
- `src/config.rs` 206-250, 317-322, 328-380, 519-530, 546-700, 1087-1090
- `src/probe/step.rs` (all), `src/probe/mod.rs` 23-160, `src/probe/opensubtitles.rs` 20-60, 150-270
- `src/archive/steps.rs` 1-230, 355-430, `src/archive/select.rs` 20, 40-95, 141-160
- `src/media/steps.rs` (all), `src/media/mod.rs` 146-156, 237-280
- `src/predb/step.rs` 1-200, `src/category/step.rs` (all), `src/category/rules.rs` 456-470
- `src/naming/steps.rs` 1-110, `src/naming/decide.rs` 24-80, `src/naming/mod.rs` 49-50
- `src/filters.rs` 95-135, 374-378, 585-640
- `src/store.rs` 44, 251-254, 770-783, 1525-1560, 1755-1860
- `src/web.rs` 40-80, 980-1200, 1235-1460, 1555-1580, 1657-1663
- `src/search.rs` 25-50, 320-345, 525-580
- `src/release_info.rs` 120-200, 393-436
- `src/metrics/catalog.rs` (all), `src/metrics/report.rs` 148-200, `src/metrics/names.rs` 90, 142-160
- `src/stats_dashboard.rs` 1074-1126, `src/indexer.rs` 225-235, 378-407, `src/db.rs` 20-60
- `src/duplicates.rs` 97-118, 515-535, `src/naming/unname.rs` 85-100, `src/paths.rs` 60-80
- `README.md` 190-210, 360-370, 395-410, 420-510, `docs/port/03-postprocess-stage.md` 424-521, 903-1021, 1031-1123
- `tests/pp_stage.rs` 257-292, `tests/pp_reprocess.rs` 104-150, 203-208, `tests/common/stage.rs` 171-185
- Other readers' notes: `atlas-map/probe.md` §10, `ingest-postprocess.md` §6.1, §7, `coordination.md`
- Git: `git diff --stat main...remotes/pr/{48,51,54,55,57,58}` and `origin/feat/release-stills-strip`;
  full diffs of pr/48 and pr/51 for `src/probe/step.rs`, `src/postprocess/schema.rs`, `src/postprocess/steps.rs`;
  `web.rs` hunk headers of pr/48, pr/57 and the stills-strip branch.

---

## 2. Which releases get a page at all

| Fact | Evidence |
|---|---|
| Route `/ui/release` → `release_page` when signed in; otherwise a redirect | `src/web.rs:60,69` |
| `release_page` calls `get_release(id)`; `None` → 404 "there is no such release … It may have been purged." | `src/web.rs:989-993` |
| `get_release_with` reads `s{shard}.releases` by id and filters **only** `!filters::group_blocked(group_name)`; it does not apply `VISIBLE_RELEASE` | `src/search.rs:552-560` |
| `VISIBLE_RELEASE` (display_name not null, or not obfuscated with a non-token name, and not `atlas_blocked`) is used by every list/search query, the category cursors and `all_releases`, not by `get_release` | `src/search.rs:37-40, 151, 199, 211, 237, 246, 335, 341, 519, 529` |
| So an obfuscated release with no `display_name` is hidden from lists but served by direct id (links from copies cards `web.rs:1551`, NZB, twins) | `src/search.rs:37-40` vs `:557-560` |
| `search_obfuscated`/`count_obfuscated` list exactly those hidden rows (obfuscated, no display name, not blocked) | `src/search.rs:532-549` |
| Blocked groups: always on, no setting; the SQL function `atlas_blocked` is registered on every connection | `src/filters.rs:101-131, 374-378` |
| `--purge --blocked` deletes blocked-group rows with no age wait | `src/postprocess/purge.rs:93-96` |

**Page denominator** = all `releases` rows in non-blocked groups, hidden-by-`VISIBLE_RELEASE` ones included.
Blocked-group rows are stored but have no page, run no step (`rules.rs:105`), get no body (`bodies.rs:87-88`)
and are excluded from the catalog metrics (`metrics/catalog.rs:206-213`).

---

## 3. How a release reaches the stage (and when it does not)

```
save ──► pp_queue(release_id, due_at = now + settle)        store.rs:1543-1551 (SETTLE_SECS/COMPLETE_SETTLE_SECS, store.rs:252-254,
   │                                                          set from config by engine.set_config, engine.rs:233-237)
   │  DB lane (atlas-pp-db-N, `postprocess.workers`, mod.rs:86-94)
   ├─ queue_batch: pp_queue where due_at <= now, DB_BATCH 256          engine.rs:376-410, :27
   ├─ walk_tick: pp_walks cursor, newest first, WALK_BATCH 256,
   │    token bucket backlog_per_second / SHARDS, paused while
   │    LOAD.writer_queued > 2*SHARDS                                  engine.rs:412-486, :418-431
   ▼
visit_all: judge_row filter → file_summaries for survivors             engine.rs:522-553, :534
visit:      judge_row → judge_files → steps                            engine.rs:557-766
   Net lane (atlas-pp-net-N, `body_connections`, mod.rs:95-105)
   net_claim: pp_state where net_wanted = 1 and net_next_at <= now,
              NET_BATCH 4, every 8th claim oldest first                engine.rs:799-823
   net_one:   judge_row + judge_files again → net_step                 engine.rs:825-875
   net_step:  body_gate, first remaining step, Begin counts attempt    engine.rs:877-1035
```

Stage on/off: the stage starts only if `config.postprocess.enabled` (`bg_indexer.rs:850-861`); a change
of `(enabled, workers, body_connections, predb.enabled, media.any_provider())` rebuilds the indexer
(`bg_indexer.rs:664-668, 670-675`). While off, saves still queue rows (docs:498). Config reload every 5 s
(`bg_indexer.rs:30`); `Stage::set_config` re-reads thresholds and re-seeds walks (`mod.rs:109-114`).

### 3.1 The rules (`src/postprocess/rules.rs`), in order, with config keys

| # | Test | Verdict | Threshold / key (default) | Evidence |
|---|---|---|---|---|
| 1 | `filters::group_blocked(group)` | `Reject(Blocked)` | none, always on | `rules.rs:105-106`; `filters.rs:101-131` |
| 2 | `parts <= 0` | `Reject(Empty)` | none | `rules.rs:107-108` |
| 3 | `size < max(min_size_bytes, group_rules[group].min_size_bytes)` | `Reject(TooSmall)` | `postprocess.min_size_bytes` (0 = off), `postprocess.group_rules.<group>.min_size_bytes` | `rules.rs:94-96, 109-110`; `config.rs:600, 604, 624, 680-686` |
| 4 | `files` rows read (no cap, streamed) | — | — | `rules.rs:116-154` (query :120) |
| 5 | `files == 0` | `Reject(Empty)` | none | `rules.rs:159-161` |
| 6 | `now - last_touch < quiet`, `quiet = complete ? complete_settle_secs : settle_secs` | `Wait(last_touch + quiet)` — the only Wait | `postprocess.settle_secs` (7200), `complete_settle_secs` (900), floors 1 | `rules.rs:162-165`; `config.rs:622-623, 664-665` |
| 7 | `filters::stored_fragment` (1 part, 1 file, subject claims a middle part, random-looking name) | `Reject(Fragment)` | none | `rules.rs:166-176`; `filters.rs:624-636` |
| 8 | `files < max(min_files, group_rules[group].min_files)` | `Reject(TooFewFiles)` | `postprocess.min_files` (1), `group_rules.<group>.min_files` | `rules.rs:98-100, 177-179`; `config.rs:625` |
| 9 | `reject_par2_only && all_par2` (every filename matches `\.(vol\d+\+\d+\.par2|par2)$`) | `Reject(Par2Only)` | `postprocess.reject_par2_only` (true) | `rules.rs:16, 141, 180-182`; `config.rs:626, 677-679` |
| 10 | otherwise | `Pass` | — | `rules.rs:183` |

Note: with the default `min_files = 1`, rule 8 can only fire via `group_rules` (rule 5 already rejects
`files == 0`). Size, file type and completeness never reject (`rules.rs:4-5`, tests `:305-318`).

### 3.2 What a verdict writes (`engine.rs:visit` + `apply.rs:write`)

| Verdict | Queue row? | `pp_state` row? | Evidence |
|---|---|---|---|
| Reject, release had no state row | the claimed `pp_queue` row is deleted (walk rows write nothing) | **none** (`no_state = true`) | `engine.rs:578-582`; `apply.rs:174, 205-208`; `schema.rs:32` |
| Reject, release had a state row | deleted | row kept; `net_wanted` recomputed to NULL (net_mask 0); bits untouched | `engine.rs:581`; `apply.rs:178-190` |
| Wait(until), no state row | `pp_queue.due_at = until` | none | `engine.rs:590-594`; `apply.rs:210-216` |
| Wait(until), state row | `pp_queue.due_at = until`, redo bits cleared | kept | `engine.rs:591-593` |
| Pass | queue row deleted unless a retry/wait re-queues it | written iff anything happened: set_done/set_failed/clear/rename_clears/net_mask/error/names (`no_state` rule) | `engine.rs:757-765`; `apply.rs:174-203` |
| Net lane Reject | — | `net_mask = 0` → `net_wanted` NULL | `engine.rs:862-865` |
| Net lane Wait | — | `net_wanted` stays 1, `net_next_at = until` | `engine.rs:867-870` |

Stats: every Reject increments a process-lifetime counter by reason (`stats.rs:48-58`, `engine.rs:579, 863`).

### 3.3 Step selection inside a Pass (`engine.rs:596-756`, `877-920`)

- `enabled` mask = `step.enabled(config) && (needs != Bodies || body_fetches_per_minute > 0)`
  (`engine.rs:233-250`, `:240`). `postprocess.body_fetches_per_minute` default 600 (`config.rs:629`);
  0 turns every `Needs::Bodies` step off.
- `applies(release)` is "cheap, no I/O. False: not run and not marked done, in either lane"
  (`step.rs:50-54`); checked at `engine.rs:620, 707, 900`.
- Growth: `row.info.parts > row.parts_seen` clears every step with `reruns().on_growth`
  (`engine.rs:598-601`); `parts_seen` written each DB visit (`:605`).
- Body gate for `Needs::Bodies` steps only (External steps are not gated): `Outside` → bit **set done**
  with nothing stored, counted as `unmatched` (`engine.rs:711-716`, `905-909`); `NotYet` → skipped, no
  marker (`:718`, `:910`); `Open` → `net_mask |= bit` → `net_wanted = 1` (`:720`, `apply.rs:178, 183`).
- `body_gate` (`rules.rs:199-211`): `Outside` if `size < body_min_size_bytes` (1 MiB, `config.rs:631`) or
  `body_max_size_bytes > 0 && size > body_max_size_bytes` (100 GiB, `:632`) or `all_images`
  (`IMAGE` regex `rules.rs:18-19`); else `NotYet` if `permille < min_completion_percent * 10`
  (95, `config.rs:635`, clamped ≤100 `:666`); else `Open`.
- `permille` (`rules.rs:187-196`): 1000 if `complete` (`store.rs:770-783`: every file's `seen` exactly
  `expected` and `files == file_total`); else `present*1000/wanted`, lowered to `files*1000/file_total`
  when `file_total > files`.

### 3.4 Failures, attempts, backoff

| Event | Effect | Evidence |
|---|---|---|
| DB step `Retry` | `db_attempts + 1`; `attempt >= max_attempts` → `failed` bit, attempts reset to 0; else `pp_queue.due_at = now + backoff` | `engine.rs:669-681` |
| DB step `Permanent` | `failed` bit, `last_error` | `engine.rs:683-687` |
| DB step `Transient` | re-queue `now + 300`, no attempt | `engine.rs:688-692` |
| DB step `OutOfBudget` | re-queue `now + 60` | `engine.rs:693` |
| Net step Begin | `net_attempts + 1`, `net_next_at = now + NET_CRASH_RETRY (900)` before the run | `engine.rs:938-949`, `:43` |
| Net `net_attempts >= max_attempts` on entry (crashes) | `failed` bit, `last_error = "interrupted"` | `engine.rs:924-929` |
| Net `Retry` | `attempt >= max_attempts` → `failed` bit; else `net_next_at = now + backoff` | `engine.rs:996-1006` |
| Net `Permanent` | `failed` bit | `engine.rs:1007-1012` |
| Net `Transient` | `net_next_at = now + 300`, net lane rests (doubling to 60 s after 20 in a row) | `engine.rs:1013-1018, 773-782, 30-41` |
| Net `OutOfBudget` | `net_next_at = now + 60` | `engine.rs:1021-1024` |
| Step asks to wait (provider pause) | `net_next_at = until` | `engine.rs:960-966` |
| A `StepWrite` fails on the writer | savepoint rollback; `pp_state.failed |= set_done` only when giving up; `last_error`; DB lane re-queues `now + after`, net lane `net_next_at` | `apply.rs:65-111` |
| `backoff(attempt, id)` | `min(900 * 4^(attempt-1), 604_800) + id % 60` → 15 m, 1 h, 4 h, 16 h, 64 h→7 d | `engine.rs:47-50` |
| `max_attempts` | `postprocess.max_attempts` (5, floor 1) | `config.rs:637, 656` |
| Step run deadline | `STEP_DEADLINE` 120 s | `engine.rs:44` |

### 3.5 Body refusals inside a run (`bodies.rs`)

`BodyFetcher::refusal` (`bodies.rs:85-99`) returns `Got::Refused(why)` before any bytes move:
`"blocked group"`, `"outside the size bounds"` (same `body_min/max_size_bytes`), `"an image"` (file name
matches `IMAGE`), `"past bodies_per_release"` (`postprocess.bodies_per_release`, 8, `config.rs:634`).
Budget tokens: `wait_for_token` (`:103-121`) → `StepError::OutOfBudget` when the minute's budget is gone
or the wait would pass the run's deadline. The probe maps `Refused` to `there() == None`
(`probe/step.rs:65-71`), so a health entry simply goes missing; the refusal reason is **not persisted**
anywhere (`bodies.rs:136` hands it back to the step, which drops it).

---

## 4. Decision table: release → panel possible / impossible

Panels and what they need (`src/web.rs`):

| Panel | Rendered at | Needs |
|---|---|---|
| Facts (`<dl class="facts">`): Category, Title, Size, Posted, Group(s), Poster/Email, Posted as/Name from, name tags, Parts, Encryption, Password, Files, PAR2 files, Articles, Repair, Links | `web.rs:1028-1143`, `:1191` | `releases` + `files`/segments scan (`release_info::contents`, `web.rs:1004`), `release_category` (`:1000`), `archive_release` (`:1001`), `media_release` (`:1002`), `releases.name/display_name` (`:1003`) |
| `#title-details` (htmx `/ui/details`) | `web.rs:1190`, route `:62`, `details_panel :1657-1663` | `media_release` row with provider ids |
| Other copies | `web.rs:1171-1181`, `copies_html :1555-1573` | `release_fingerprint` rows (written by Par2Step/ListStep `finish`, `archive/steps.rs:135-138`, and `SET_POSTED` etc.) |
| Probe panels: Tracks, Chapters, yEnc, Packing, How it was posted, NFO, SFV, OpenSubtitles lines | `web.rs:1163-1170`, `probe_html :1243-1452` (`<h2>NFO</h2>` :1431, `<h2>SFV</h2>` :1443, panels :1446-1451) | `release_probe.info` (+ `release_opensubtitles`) |
| Files inside | `web.rs:1149-1161` | `archive_inner_files` via `archive::store::view` (`:1001`) |

Which step produces which panel, and when the step can run:

| Step (id, rank, lane) | Panel(s) | Enabled when | `applies()` | Gate | Evidence |
|---|---|---|---|---|---|
| `archive_par2` (1, 100, Bodies) | Files inside (PAR2 list), Password/Encryption facts, copies (fingerprint), name | `archive.enabled` (true) and `body_fetches_per_minute > 0` | name not an image (`select::classify == Kind::Image`: `.jpe?g/.png/.gif/.webp/.bmp/.tiff?`) | `body_gate` Open | `archive/steps.rs:185-211, 32-34`; `select.rs:20, 74`; `config.rs:557`; `engine.rs:240` |
| `archive_list` (2, 110, Bodies) | Files inside (RAR/7z/ZIP), Password/Encryption, copies, name | same | same | `body_gate` Open; inside the run an image head ends it (`Format::Image`) | `archive/steps.rs:367-430`, `:424`; limits `max_bodies_per_release` 4, `probe_files` 2, `probes_per_minute` 60 (`config.rs:558-560`) |
| `probe` (4, 120, Bodies) | Tracks, Chapters, yEnc, Packing, How it was posted, NFO, SFV | `archive.enabled` and budget | name does **not** end `.jpg` (only `.jpg`, not `.jpeg/.png`) | `body_gate` Open; writes `release_probe` only if something was read (`empty` check) | `probe/step.rs:86-101, 196-199, 205` |
| `opensubtitles` (5, 130, External) | "OpenSubtitles match" line in Tracks | `archive.enabled && media.opensubtitles.api_key != ""` | always | not body-gated; needs a `moviehash` from the probe | `probe/opensubtitles.rs:176-192, 198-205` |
| `naming` (8, 200, Database) | Name/Name from facts, `VISIBLE_RELEASE` visibility | `naming.enabled` (true) | `worth_running`: false once `name_source >= PreDb` or the name is clean scene grade, etc. | — | `naming/steps.rs:20-41`; `naming/decide.rs:67-80`; `naming/mod.rs:49`; `config.rs:526` |
| `nfo` (9, 210, Bodies) | Name from NFO | `archive.enabled && naming.nfo_step` (true) and budget | `effective_source <= Nfo && grade < Good` | `body_gate` Open | `naming/steps.rs:75-97`; `naming/mod.rs:50` |
| `predb_match` (16, 300, Database) | (predb facts; see predb.md) | `predb.enabled` (**false** by default) | `worth_looking_up`: not already PreDb-named, not an image name, >1 part or hash-like or non-junk | — | `predb/step.rs:63-84, 159-180`; `config.rs:378` |
| `category` (24, 400, Database) | Category fact / `?cat=` links | `category.enabled` (true) | always | — ; writes a row only when `stores(&d)` (rule not `no_match/hidden/unnamed/obfuscated_name`), else the page shows the Uncategorized label | `category/step.rs:51-95`; `category/rules.rs:468-470`; `web.rs:1034, 1057`; `config.rs:336` |
| `media_parse` (32, 500, Database) | Links fact, media tag, `/ui/details` | `media.parse` (true) | always; writes a row only when the (category, name) parses as TV/movie, else deletes a stale row | — | `media/steps.rs:159-217, 113-126`; `config.rs:232` |
| `media_lookup` (33, 510, External) | provider ids → Links, details | `media.any_provider()` (tvmaze on by default) | `target_of(..).is_some_and(wants(kind, key, id, now))` (no provider asks for that kind, title already known, or store open failed → false) | not body-gated | `media/steps.rs:250-305`; `media/mod.rs:265-280`; `config.rs:237, 248` |
| `media_nfo` (34, 520, Bodies) | IMDb id from NFO → Links | `media.nfo && media.parse && body_fetches_per_minute > 0` | movie target and `!imdb_known(key)` | `body_gate` Open | `media/steps.rs:309-355`; `media/mod.rs:251-253` |
| `opensubtitles_imdb` (35, 505, Database) | IMDb id → Links | `media.parse && opensubtitles.api_key != ""` | always | — | `probe/opensubtitles.rs:241-270` |

Release classes → what is possible (✓ possible, ✗ impossible, ◐ conditional):

| Release state | Page | Facts | Category | Media/Links/Details | Files inside / Password / copies | Probe panels | Rule / threshold / key |
|---|---|---|---|---|---|---|---|
| Blocked group | ✗ 404 | — | — | — | — | — | `group_blocked`, no setting (`filters.rs:101-131`; `search.rs:560`) |
| `parts <= 0` or no `files` rows | ✓ | ✓ (sparse) | ✗ | ✗ | ✗ | ✗ | `Reason::Empty` (`rules.rs:107, 159`) |
| `size < max(min_size_bytes, group floor)` | ✓ | ✓ | ✗ | ✗ | ✗ | ✗ | `postprocess.min_size_bytes` / `group_rules` (`rules.rs:94-96, 109`), default off |
| Settling: `now - last_touch < 7200` (incomplete) / `< 900` (complete) | ✓ | ✓ | ✗ yet (queue row at `last_touch + quiet`) | ✗ yet | ✗ yet | ✗ yet | `settle_secs`, `complete_settle_secs` (`rules.rs:162-165`) |
| Stored fragment (1 part, 1 file, middle part, random name) | ✓ (hidden from lists unless named) | ✓ | ✗ | ✗ | ✗ | ✗ | `Reason::Fragment` (`filters.rs:624-636`), hard-coded |
| `files < max(min_files, group floor)` | ✓ | ✓ | ✗ | ✗ | ✗ | ✗ | `postprocess.min_files` (1) (`rules.rs:177-179`) |
| All files `.par2`/`.volN+M.par2` | ✓ | ✓ (PAR2 files fact) | ✗ | ✗ | ✗ | ✗ | `reject_par2_only` (true) (`rules.rs:180-182`) |
| Passed, stage off | ✓ | ✓ | ✗ | ✗ | ✗ | ✗ | `postprocess.enabled` (`bg_indexer.rs:850`) |
| Passed, DB steps | ✓ | ✓ | ✓ if `category.enabled` and rule stores | ◐ `media.parse` and the name parses as TV/movie; ids need a provider (`media.any_provider`) | ✗ (body steps) | ✗ (body step) | `category/step.rs:64-67`; `media/steps.rs:172-175, 263-266` |
| Passed, body budget 0 or `archive.enabled = false` | ✓ | ✓ | ✓ | ◐ | ✗ (par2/list off); names fall back to save-time lookup (README:401) | ✗ | `body_fetches_per_minute` (`engine.rs:240`), `archive.enabled` (`archive/steps.rs:175-181`, `probe/step.rs:86-89`) |
| Passed, `size < 1 MiB` or `> 100 GiB` | ✓ | ✓ | ✓ | ◐ | ✗ — bits set **done** with nothing stored (`BodyGate::Outside`) | ✗ (same) | `body_min_size_bytes`, `body_max_size_bytes` (`rules.rs:201-205`; `engine.rs:711-716`) |
| Passed, every file an image | ✓ | ✓ | ✓ | ◐ | ✗ (Outside) | ✗ (Outside) | `all_images` (`rules.rs:142, 203`) |
| Passed, incomplete `< 95 %` | ✓ | ✓ (INCOMPLETE tag, Articles fact) | ✓ | ◐ | ✗ until growth (`NotYet`, nothing recorded) | ✗ until growth | `min_completion_percent` (`rules.rs:206-207`; `engine.rs:718`) |
| Passed, Open, name ends `.jpg` | ✓ | ✓ | ✓ | ◐ | ◐ par2/list run unless name is an image (`.jpg` is) → ✗ | ✗ (`applies` false) | `probe/step.rs:90-92`; `archive/steps.rs:32-34` |
| Passed, Open, name ends `.png/.jpeg/.gif/…` | ✓ | ✓ | ✓ | ◐ | ✗ (`named_as_an_image`) | ✓ probe applies (only `.jpg` excluded) | `select.rs:20`; `probe/step.rs:91` |
| Passed, Open, probed, nothing readable (first article `TooBig`, bodies refused, no yenc/nfo/sfv/health) | ✓ | ✓ | ✓ | ◐ | ◐ | ✗ silently: bit 4 done, **no** `release_probe` row; page prints nothing (`unwrap_or_default`) | `probe/step.rs:196-199`; `web.rs:1163-1170` |
| Passed, Open, `bodies_per_release` (8) exhausted mid-run | ✓ | ✓ | ✓ | ◐ | ◐ | ◐ "On the server" health entries missing, no reason stored | `bodies.rs:95-96`; `probe/step.rs:65-71` |
| Passed, step failed after `max_attempts` (5) | ✓ | ✓ | per bit | per bit | per bit | per bit; `failed` bit + `last_error` | `engine.rs:669-681, 996-1012`; `apply.rs:95-99` |
| Incomplete `< 95 %` and quiet ≥ `dead_after_secs` (172 800) | ✓ until `--purge --dead --apply` deletes it → ✗ 404 | | | | | | `purge.rs:97, 125-127`; `config.rs:636`; `web.rs:993` |
| Rejected classes also purgeable: `--fragments` (default), `--too-small --below` (default `body_min_size_bytes`), `--image-only`, `--rejected` (Par2Only/TooFewFiles/Empty) | ✓ until purged | | | | | | `purge.rs:90-129, 54-60` |

Dependencies between panels: OpenSubtitles needs the probe's `moviehash` (`probe/step.rs:208-211`
rerun bit 5; `opensubtitles.rs:198`); `opensubtitles_imdb` needs a `media_release` row;
Links/details need `media_parse` to have produced a row; copies need the fingerprint rows that only the
two archive body steps write.

---

## 5. What `pp_state` records per release

Table (`src/postprocess/schema.rs:33-57`), comment `:32`: "only releases that passed the rules and had a
step run. Rejected rows have none."

| Column | Meaning | Written by |
|---|---|---|
| `release_id` PK | | |
| `done` | bitmask `1 << StepId` of steps finished (incl. `BodyGate::Outside` "done with nothing") | `apply.rs:176-181` (`done = (done | set_done) & ~clear`) |
| `failed` | bitmask of steps given up (`max_attempts`, `Permanent`, write failure on give-up) | `apply.rs:182, 95-99` |
| `net_wanted` | 1 while a net step is left for this release, else NULL (partial index `pp_state_net`) | `apply.rs:178, 183` |
| `db_attempts` | retries of the DB lane's current attempt; 0 when the visit had no failure | `engine.rs:669-681, 697-699`; `apply.rs:91-99` |
| `net_attempts` | counted at Begin before the net step runs | `engine.rs:938-949` |
| `net_next_at` | when the net lane may claim it again (0 = now) | `engine.rs:943, 1000-1023`; `apply.rs:106-109` |
| `parts_seen` | `releases.parts` at the last DB visit; growth beyond it clears `on_growth` steps | `engine.rs:598-605`; `apply.rs:187` |
| `last_error` | text of the last step/write error (overwritten each write, incl. with NULL on success) | `apply.rs:188, 98` |
| `rerun_gen`, `rerun_mask` | clear requests (rerun, reprocess) a running visit must not overwrite | `apply.rs:125-133, 189-190`; `mod.rs:63-71` |

Step bit ids: 1 par2, 2 list, 4 probe, 5 opensubtitles, (6 stills in PR #51/#54), 8 naming, 9 nfo,
16 predb_match, 24 category, 32 media_parse, 33 media_lookup, 34 media_nfo, 35 opensubtitles_imdb
(`archive/steps.rs:185, 369`; `probe/step.rs:19`; `probe/opensubtitles.rs:20, 22`; `naming/mod.rs:49-50`;
`predb/step.rs:63`; `category/step.rs:53`; `media/steps.rs:161, 252, 311`). Bands by rank:
`steps.rs:35-51`.

Other writers of `pp_state`/`pp_queue` outside the engine: `rerun_on` (`mod.rs:63-78`; used by predb
late matches), `reprocess::retry_failed` (`reprocess.rs:44-65`), `duplicates::forget_old_twins`
(`duplicates.rs:97-117`, clears bit 1 and queues), `naming/unname.rs:92-98`, the save
(`store.rs:1543-1551`, queue only). Readers: engine `ROW_COLUMNS` left join (`engine.rs:91-94`, used at
`:380-384, 437-439, 827-831`), `net_claim` (`:803-808`), `refresh_counts` (`:321-323`), reprocess dry run
(`reprocess.rs:121-126`), tests (`tests/common/stage.rs:171-185`). **Nothing in `src/web.rs` or
`src/api.rs` reads `pp_state` or `pp_queue`** (repo grep).

Related `meta` keys per shard: `pp_walk_seeded`, `pp_steps_seen`, `pp_stamp_{id}` (`engine.rs:1089-1110`),
`naming_rename_named[:group]` (`reprocess.rs:13-15, 28-31`), `purge_top/classes/below/after/victim`
(`purge.rs:357-363, 424-428`), `archive_names_over_quota` (`engine.rs:326`). `pp_walks`
(`schema.rs:60-76`): one row per outstanding walk (`backlog`, `steps`, `reprocess`), with `top_id`,
`before_id` (cursor), `redo_mask`, `group_name`, `seen`, `ran`.

### 5.1 Could the page print "not probed because …"? (purity and cost)

- `judge_row(&Rules, &RowFacts)` (`rules.rs:104-114`), `judge_files(&Rules, &RowFacts, &FileSummary, now)`
  (`:158-184`), `permille` (`:187-196`) and `body_gate` (`:199-211`) are pure functions of their arguments.
  `group_blocked` memoises per thread (`filters.rs:118-131`); `stored_fragment` is pure
  (`filters.rs:624-636`). All are `pub`, so `web.rs` can call them.
- `Rules::new(&PostprocessConfig)` (`rules.rs:90-92`) from `crate::config::load_settings().postprocess`
  (`config.rs:1087-1089`, reads the config file; `web.rs:1661` already does `config::media_config()` per
  `/ui/details` request, so a per-request config read is the existing pattern).
- `RowFacts` (`rules.rs:60-67`): `group`, `size`, `parts`, `complete` are on `ReleaseRow`
  (`search.rs:33-34` `COLUMNS`); `name` must be the posted `releases.name` (fragment detection uses
  `looks_random(post.name)`, `filters.rs:635`), i.e. `release_info::names(id).posted`
  (`release_info.rs:136-150`; already fetched at `web.rs:1003`), not `ReleaseRow.name` which is
  `coalesce(display_name, name)`; `file_total` is **not** in `COLUMNS` → one extra PK read or an extension
  of `COLUMNS`.
- `FileSummary` (`rules.rs:71-81`): `file_summaries(conn, &[id])` (`:118-154`) runs
  `select … from files where release_id = ?` with an **unqualified** table name, so it needs a connection
  opened on the shard (`db::open_shard(&store::shard_path(main, shard_of_id(id)))`, as the engine's `Conns`
  does, `engine.rs:149-162`), not `db::open()` whose shards are attached as `s0..s7` (`db.rs:22-25`). The
  page already reads the same `files` rows in `release_info::union_files` (`release_info.rs:393-436`:
  `filename, expected, seen, file_total, subject`); `FileSummary` additionally needs `touched_at`
  (for settling) and `subject_part` (for fragments) → either one more indexed range read of `files` (same
  pages, one statement) or extend `UnionFile` with those two columns and build the summary without a second
  read. Cost is O(files) and dwarfed by the `each_article` scan the page already pays
  (`release_info.rs:184-188`).
- `pp_state` facts: one PK lookup `select done, failed, net_wanted, net_next_at, db_attempts,
  net_attempts, parts_seen, last_error from s{n}.pp_state where release_id = ?` plus one PK lookup of
  `s{n}.pp_queue.due_at` (pattern: `probe::read`, `probe/mod.rs:142-148`). Microseconds.
- Enabled-ness must be mirrored from config booleans, not by calling `Step::enabled`, which has side
  effects (`probe/step.rs:86-89` sets a flag; `media/steps.rs:172-175` calls `set_config`;
  `category/step.rs:64-67` swaps options): `archive.enabled`, `postprocess.body_fetches_per_minute > 0`,
  `postprocess.enabled`, `naming.enabled`, `naming.nfo_step`, `predb.enabled`, `category.enabled`,
  `media.parse`, `media.nfo`, `media.any_provider()`, `media.opensubtitles.api_key`.
- Explanation ladder the page could compute from those inputs: blocked (never a page) → `judge_row`
  reason → `files == 0` → settling until `pp_queue.due_at` → fragment / too few files / par2-only →
  stage or step disabled, no body budget → `applies` false (`.jpg` / image name) → `body_gate` Outside
  (too small / too big / only images) or NotYet (show `permille`) → `pp_state`: bit done but no
  `release_probe` row ("probed, nothing readable"), `failed` bit + `last_error`, `net_wanted = 1` with
  `net_next_at` / `net_attempts` ("queued for the network"), `db_attempts` with `pp_queue.due_at`
  ("retrying at …"). Not derivable without new writes: which bodies were refused and why
  (`bodies.rs:85-99` not persisted), and `NotYet` history (nothing recorded, `engine.rs:718`).
- Caveat: a verdict recomputed on the page uses the *current* config and clock; the stage's past decision
  is not stored (rejects write nothing, `engine.rs:578-582`), so after a config change the page and the
  stage can disagree until the next walk/article.

---

## 6. Counting the denominator today

| Source | What it counts | Limits | Evidence |
|---|---|---|---|
| `Stats.rejected` by reason → `stats.json["postprocess"]["rejected"]`, TUI dashboard "rejected" panel, Prometheus `STAGE_REJECTED{reason}` | **visits** rejected since process start (a release re-judged by every queue row and every walk counts again); reset on restart | not distinct releases; never sees releases nothing visited | `stats.rs:27-32, 48-58, 72-79`; `bg_indexer.rs:486-488`; `stats_dashboard.rs:1123-1125`; `metrics/report.rs:187-190` |
| `refresh_counts` every 30 s: `queue_due` (≤100 001), `net_wanted` (≤1 000 001), `failed` (≤100 001), walk `done/total` (ids), `archive_names_over_quota`; `status.json.postprocess_backlog = due + net` | bounded `select count(*) from (… limit N)` per shard | capped; no count of `pp_state` rows at all | `engine.rs:314-336`; `mod.rs:150-158`; `stats.rs:9-18`; `bg_indexer.rs:47-57`; dashboard `stats_dashboard.rs:1087-1091` |
| Metrics catalog walk (`atlas-metrics-catalog` thread) every `metrics.catalog_every_secs`: `atlas_catalog_releases{state=total,complete,nzb_ready(parts>0),named,still_obfuscated,predb_matched}`, categories (Uncategorized for no row), groups, sizes, predb | walks every `releases` row in 20 000-row reads, left-joining `release_category` and `predb_match` only, skipping blocked groups; result cached in `metrics_catalog.json` | does **not** join `pp_state`, `release_probe`, `media_release`, `archive_release`, so it cannot say "has panel X" or "has no pp_state row"; the `has_table` guards (`:437-446`) show how a join would be added | `metrics/catalog.rs:26-28, 160-185, 202-270, 299-307, 435-479, 482-512, 588-590`; `metrics/names.rs:142` |
| `atlas --purge` dry run (no `--apply`) | per class: fragments, blocked (per group), too-small (histogram ≤64 KiB/256 KiB/1 MiB), image-only, rejected (Par2Only/TooFewFiles/Empty), dead (<95 % quiet ≥ `dead_after_secs`) — i.e. the rejected part of the denominator, by class, as distinct releases | CLI only; reads every release and its `files` (`file_summaries`), 4 shards at once; refused while a compaction runs; docs: 1–50 min at 118 M rows | `purge.rs:90-129, 187-230, 239-262, 478-500`; docs:1073 |
| `atlas --reprocess` dry run | `count(*) from pp_state … where done & mask != 0 or (--failed and failed != 0)` | counts rows **with** state only | `reprocess.rs:110-135` |
| Tests' convention | `Harness::bits(name)` returns `None` for a release with no `pp_state` row; the walk test expects exactly `(states, queued) = (300, 0)` with the fragment written nowhere | | `tests/common/stage.rs:171-178`; `tests/pp_stage.rs:98, 257-292` |

To count "releases with no `pp_state` row" exactly one must scan: per shard
`select count(*) from releases r where not exists (select 1 from pp_state s where s.release_id = r.id)`
(index on both PKs; a full `releases` scan per shard — comparable to one catalog walk pass). The natural
home is the catalog walk (`read_batch`, `metrics/catalog.rs:447-453`: add `left join pp_state s` and tally
`s.release_id is null`, plus `s.done & bit` per step for "has panel X" counts); its cost model is already
"short reads that pin no WAL" (`:1-6, 25-28`). Note `releases.id` ordering: the walk reads `where r.id > ?1
order by r.id` (`:451`), and `pp_state` rows written by the stage's newest-first walk sit at ~50 B/row
until compaction (docs:1033-1034).

Design-time sizes for reference (docs:1036-1038, 1061-1064): 118 M rows, fragments up to ~29 % (34 M),
84–100 M rows run DB steps; body candidates ≤37.6 M multi-article rows, ~15.4 M after the image share;
README:405-409: "about 15 million of 118 million releases pass the body gates … about 78 days at 300
bodies a minute".

---

## 7. Can `--reprocess --failed` or relaxed rules reach rejected releases, and at what cost?

- `--reprocess [--step NAME]… [--group]… [--failed] --apply` (`reprocess.rs:74-99`): `apply` inserts one
  `pp_walks` row of kind `reprocess` per shard with `redo_mask` (`:25-40`, deduplicated on
  `(kind, redo_mask, group_name)`); `--failed` resets `failed, db_attempts, net_attempts, net_next_at,
  last_error` and queues **only rows already in `pp_state` with `failed != 0`**, 1 000 per transaction
  (`:44-65, 150-159`). A rejected release has no row (`schema.rs:32`), so `--failed` never reaches it.
- The reprocess walk goes through `walk_tick → visit_all → visit`, where `judge_row`/`judge_files` run
  before any step (`engine.rs:534, 583-597`); a rejected release is rejected again and, with `due = None`
  and `has_state = false`, `reject` returns `None` — nothing is written (`engine.rs:578-582`). So neither
  `--reprocess` nor `--step all` can reach a release the current rules reject.
- Relaxable knobs (`config.rs:596-615, 643-690`): `min_size_bytes` → 0 (default already), `min_files` →
  0/1, `reject_par2_only` → false, `group_rules` floors. Hard-coded: Blocked (`filters.rs:101-131`
  "Always on: there is no setting for it"), Empty (`rules.rs:107, 159`), Fragment (`rules.rs:174`;
  `filters.rs:624-636`). Settling is a delay, not a rejection.
- A threshold change by itself triggers nothing: `set_config → seed → seed_shard` only queues a `steps`
  walk when a step bit becomes enabled or a `config_stamp` gains a bit (`engine.rs:1073-1112`,
  `pp_steps_seen`, `pp_stamp_{id}`); the queue only holds settling/retry rows. Newly-passing releases are
  reached by (a) `atlas --reprocess --apply` with no `--step` ("judge again only", `reprocess.rs` header
  comment docs:996: a walk with `redo = 0`; for a release with no state row every enabled, applicable step
  is "not done", so all run), or (b) a new article for that release (`store.rs:1543-1551`).
- Cost of (a): a full walk over every release at `backlog_per_second` (2 000/s default, `config.rs:628`;
  dry run prints `total / per_second`, `reprocess.rs:130-132`) → ~16.4 h for 118 M rows (docs:1061);
  each row pays `judge_row` and, if it survives, the `files` read (`rules.rs:118-154`, "0.13–0.15 page
  misses … 2–4 µs" hot, up to 3 random reads cold, docs:1074-1079); the walk pauses whenever
  `writer_queued > 2*SHARDS` (`engine.rs:418`). Newly passing par2-only/few-file posts then run every DB
  step (category, naming, media_parse …) and body steps if gated Open — e.g. Par2Step would read the PAR2
  and could produce "Files inside" for a par2-only post.
- Storage cost: each newly passing release gets a `pp_state` row, ~25 B (id order) to ~50 B (newest-first
  walk) (docs:1033-1034); 34 M fragments would add ~0.85–1.7 GB if Fragment were ever made optional.
- Interaction with purge: `--purge --rejected` uses the same `judge_files` verdict with the clock set far
  ahead (`purge.rs:117-124`, `i64::MAX / 2`), `--too-small` defaults `--below` to `body_min_size_bytes`
  (`purge.rs:60`), so relaxing `Rules` also narrows what purge takes; an install that purges `--fragments`
  (the default class, `purge.rs:85-87`) deletes the biggest rejected class outright.

---

## 8. Extension point for a "Processing" panel

Where it would go and what it would cost:

1. **Render**: a `processing_html(…)` returning `panel("Processing", &facts)` (`web.rs:1235-1241`),
   appended as a new placeholder in `release_html`'s page format string beside `{copies}{probe}{files}`
   (`web.rs:1186-1194`). Or lazily, like `#title-details`: a `<div hx-get="/ui/processing?id=…"
   hx-trigger="load">` (`web.rs:1190`) with a route in `web::get` (`web.rs:49-71`; the 401 branch at
   `:66-68` must list it too) and a handler like `details_panel` (`:1657-1663`). Lazy keeps the main page's
   cost unchanged and isolates the config read.
2. **Data**: two PK reads on `db::open()` with the `s{shard}.` prefix (`pp_state` columns of §5, `pp_queue.due_at`),
   the posted name (already read, `web.rs:1003`), `releases.file_total` (new), `Rules::new(&load_settings().postprocess)`,
   and a `FileSummary` (shard connection or an extended `union_files`, §5.1). "Next look" = `pp_queue.due_at`
   (DB lane: settling, backoff) and `pp_state.net_next_at` when `net_wanted = 1` (net lane); print them
   relative to now (both move).
3. **Step names for bits**: `steps::all()` (`steps.rs:10-26`) constructs every step, including `Media::new`
   via `crate::media::steps()` (`:11`); fine once at startup (tests do `Media::new(Path::new("/nowhere/atlas.db"))`,
   `media/steps.rs:364`), not per request. A `(StepId, name)` table derived from it once would also pick up
   `StillsStep` id 6 when PR #51/#54 land (`steps.rs` diff adds it after the OpenSubtitles step).
4. **Per-page cost**: ≈2 PK lookups + 1 `files` range read (0 if `union_files` is extended) + one config
   file read (already the pattern at `web.rs:1661`) — well under a millisecond; the page already runs
   `each_article` over every segment (`release_info.rs:184-188`) and `groups_of` across 8 shards
   (`web.rs:1069`).
5. **Tests**: `web.rs` has route tests using `get(…)` (`web.rs:1880-1990`, e.g. `:1896-1900`); stage facts
   are readable in integration tests via `Harness::bits/field` (`tests/common/stage.rs:171-185`).
6. **Counting (dashboard / metrics)**: add a `pp_state` left join and per-bit tallies to the catalog walk
   (`metrics/catalog.rs:435-479`, `Tally::add :202-270`, `publish :297-374`, `SAVED :45-64`); new families
   in `metrics/names.rs` (`CATALOG_* :142-160`). Zero extra scans.
7. **Persisting refusals** (if "why no health entry" must be shown): the only honest place is a new field
   in `ProbeInfo` (serde `#[serde(default)]`, pattern `probe/mod.rs:70-71`) written from `probe/step.rs`
   where `there()` returns `None` (`:65-71`), or a note in `pp_state.last_error`; both need a code change in
   the probe step and a re-probe/backfill for old rows.

---

## 9. Risks

1. **Thresholds are per-install, reloadable config** (`config.rs:596-690`; 5 s reload `bg_indexer.rs:30`;
   `Stage::set_config` `mod.rs:109-114`; `Rules` cloned per visit `engine.rs:530`). The stage stores no
   verdict for rejected rows (`engine.rs:578-582`), so a page-time "why" recomputed with today's config can
   differ from the stage's decision, and a relaxed threshold reaches nobody until a walk or a new article.
2. **Hidden releases have pages.** `VISIBLE_RELEASE` (`search.rs:37-40`) hides obfuscated unnamed rows
   from lists only; `/ui/release` serves them (`search.rs:552-560`). "Every release page" therefore
   includes rows search never shows; blocked-group rows are stored, counted nowhere (catalog skips them,
   `metrics/catalog.rs:211-213`) and have no page.
3. **Writer-thread budget.** Stage writes run on each shard's writer after a save with `STAGE_BUDGET_BUSY`
   10 ms / `STAGE_BUDGET_IDLE` 100 ms (`indexer.rs:230-231, 297, 347, 378-407`); walks stall while
   `writer_queued > 2*SHARDS` (`engine.rs:418`). A backfill is a walk at `backlog_per_second` (~16 h per
   118 M rows, docs:1061) and, for body steps, the body budget (600/min, `config.rs:629`; probes 60/min,
   `config.rs:560`): "15.4 M in 107 days at 300/min" (docs:1062-1064). A `steps` walk is queued
   automatically by any new enabled step bit or `config_stamp` bit (`engine.rs:1095-1110`), newest first.
4. **Schema/size.** `pp_state` at ~100 M rows is 2.5–5 GB (docs:1047-1056); widening the denominator to
   rejected rows adds ~25–50 B each (34 M fragments ≈ 0.85–1.7 GB). New tables must be in `SHARD_TABLES`
   (`schema.rs:78-92`) or compaction refuses; `RELEASE_COLUMNS` is append-only (`:94-96`); `migrate` runs on
   every shard open (`:114-130`).
5. **Rejected releases are purge targets** (`purge.rs:90-129`): fragments (default class), too-small,
   image-only, rejected, dead (<95 %, quiet 2 days). Panels promised for them vanish on `--purge --apply`
   (page → 404, `web.rs:993`). Tests assert rejected rows have no state (`tests/pp_stage.rs:98, 257-292`;
   `tests/common/stage.rs:171-178`), so giving them rows breaks the stage's contract by design.
6. **Silent probe states.** Bit 4 "done" with no `release_probe` row (`probe/step.rs:196-199`), JSON parse
   failure, `db::open` failure and "not probed" all render as nothing (`web.rs:1163-1170`
   `unwrap_or_default`). `BodyGate::Outside` marks body steps done with nothing stored (`engine.rs:711-716`).
   Refusals (`bodies.rs:85-99`) and `NotYet` leave no trace. A Processing panel can only *recompute* these.
7. **Conflicts with open work** (diffs against `main` 55d2e51):
   - PR #48 (`remotes/pr/48`, head 06847b2, "release page media cover"): `web.rs` hunks inside
     `release_html` at 1093-1157 and `probe_html` at 1282-1393, `panel()` at 1193, a new route after 1687;
     `schema.rs` adds `probe::COVER` to `SHARD_TABLES`; `probe/step.rs` changes `config_stamp` `1` → `3`
     against a base where main had `1`; main now has `1 | 2` (`probe/step.rs:99-101`, #52), so the merge
     collides and the cover bit must move to 4 (probe.md §10 is right). A new `{processing}` placeholder
     at `web.rs:1186-1194` sits inside #48's hunk range — trivial but real textual conflict.
   - `origin/feat/release-stills-strip` (2c5a0a4): `release_html` at 1113-1114 and 1141, `details_html`
     at 1663+, `schema.rs` `STILLS`, `steps.rs` `StillsStep`, `probe/step.rs` helpers `pub(super)`.
   - PR #51 / #54: `steps.rs` adds `StillsStep` (StepId 6, rank band 100-199), `schema.rs` adds
     `probe::stills::STILLS`, `probe/step.rs` makes `main_file`, `fetch_raw`, `read_part` `pub(super)`;
     no `web.rs`, no engine/rules/apply changes. A bit-name table must include id 6.
   - "stream-details" (per `coordination.md:7-8`): a not-yet-pushed branch stacked on #48 touching
     `probe_html`/`details_html` only — no rules/engine overlap.
   - PR #57 (instant sort): `web.rs:8-733` search/sort region only (+444/-51); no overlap with the
     release page.
   - Finish web UI thread's twin grouping: `copies_html`/`copy_card`/`duplicates_page`
     (`web.rs:1463-1655`) and `duplicates.rs` — adjacent to `probe_html`, not to the facts/format string;
     `forget_old_twins` (`duplicates.rs:97-117`) is a `pp_state` writer any Processing panel will see as
     "bit 1 cleared, queued".
   - None of #48/#51/#54/#55/#57/#58 touch `rules.rs`, `engine.rs`, `apply.rs`, `stats.rs`, `purge.rs`,
     `reprocess.rs` or `config.rs` (`git diff --stat` on those paths is empty).
8. **Clock semantics.** `last_touch` is `max(files.touched_at)`, NULL = 0 = "old" (`rules.rs:129`); a
   release re-touched by a late article moves `pp_queue.due_at` (`store.rs:1543-1551`) — a panel showing
   "next look at …" must re-read, not cache.
9. **Relaxed rules change purge too** (shared `Rules`, `purge.rs:117-124`): an operator who relaxes
   `reject_par2_only` to get panels for par2-only posts also stops `--rejected` from deleting them.

---

## 10. Contradictions touching this area, resolved from the code

- ingest-postprocess.md §6.1 says "releases that are blocked, empty, below `min_size_bytes`, below
  `min_files`, fragments or par2-only never get a `pp_state` row". Correct, with two additions: settling
  releases also have no row (queue row only, `engine.rs:590-594`), and a *passing* release gets no row
  until something happens (`no_state`, `engine.rs:757-765`) — in practice the always-applying
  `CategoryStep` gives nearly every passing release a row on its first visit.
- probe.md §10 "the page should say why (today it is silent, web.rs:1163-1170)": confirmed; the inputs to
  say why are all available or cheaply readable (§5.1), except body refusals and `NotYet`.
- Both readers' "no branch named stream-details exists": per `coordination.md:7-8` it is unpushed; nothing
  to diff. Treat it as touching `probe_html`/`details_html` only.
- `config_stamp` on main is `1 | 2` (`probe/step.rs:99-101`), not `1`; #48's diff is against the older
  value.
- `body_fetches_per_minute` default: docs §7 table says `0`, code says `600` (`config.rs:629`); README:369
  "Bodies are on by default". Code wins: body steps are on unless the install sets 0.

---

## 11. Open questions for the owners

1. Should the denominator for "every release page" include releases the rules reject (fragments,
   par2-only, empty, too small)? They have pages but no step ever runs; reaching them needs a page-time
   path (recompute from `releases`/`files`) or a change to the stage's "rejected rows have none"
   contract (`schema.rs:32`, tests).
2. Is a lazily loaded `/ui/processing` panel acceptable, or must the facts be in the main HTML (one more
   `files` read per page unless `union_files` is extended)?
3. Which clock/config should the "why" use — the live config (`load_settings()`) or a snapshot the stage
   writes? Today nothing records the verdict.
4. Should `BodyFetcher` refusals be persisted (new `ProbeInfo` field or `last_error` note) so the page can
   explain missing health entries?
5. Should the catalog walk (`metrics/catalog.rs`) gain a `pp_state` join to publish the denominator and
   per-step coverage, replacing the process-lifetime `rejected` visit counters as the operator's view?
6. Where does `StillsStep` (id 6) land relative to a Processing panel's bit-name table — derive from
   `steps::all()` at startup to avoid a hard-coded list?
