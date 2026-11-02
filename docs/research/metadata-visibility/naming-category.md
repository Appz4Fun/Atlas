# Atlas map — naming, obfuscation, category rules, PTT name parser

Area: `naming-category-name-parse`. Repo `/home/claude/appz4fun/atlas`, branch `main` at `55d2e51`. Read-only pass; nothing built or run. All paths below are relative to the repo root unless absolute. Line numbers are from `cat -n` on each file individually.

Feature this map serves: every catalog metadata field visible on every release page, plus a backfill of existing releases. This area answers: which name-derived fields exist, which are stored, which are shown, what it costs to show them, and what a "Name evidence" / "Category evidence" panel would need.

---

## 0. The one-paragraph picture

Atlas parses a release name in three independent places and stores almost none of the result:

| Parser | Where | Input | Output | Stored? |
|---|---|---|---|---|
| PTT port (parse-torrent-title, 423 rules + 9 custom handlers) | `src/ptt/mod.rs:521-571`, rules `src/ptt/rules.rs` | `coalesce(display_name, name)` at render time | `Parsed{title, fields}`; narrowed to `release_tags::Tags` | No. `src/release_tags.rs:1-2`: "Names are parsed when they're shown, and nothing is stored" |
| Naming regexes (1154 newznab-tmux rules, 118 group regexes) | `src/naming/regexes.rs:12,109-116,120-126`; decided in `src/naming/decide.rs:191-233` | subjects + posted file names (stage, DB lane) | `NameChange{display_name, obfuscated, source}` `src/postprocess/step.rs:239-248` | Only `releases.display_name`, `is_obfuscated`, `name_source` (`src/postprocess/apply.rs:158-162`) |
| Category rules (~240 `regex::bytes` regexes, 8 pipes) | `src/category/rules.rs:419-453,582-1264`; step `src/category/step.rs:72-95` | `display_name.unwrap_or(name)` + group + size (`rules.rs:456-465`) | `Decision{category, confidence, rule}` `rules.rs:36-41` | Only the leaf id in `release_category` (`step.rs:24`); `rule`/`confidence` are in-run facts (`step.rs:92`) and metric labels |

Plus the ingest-time subject parser (`src/parser.rs:55-125`) that produces `releases.name` and the first `is_obfuscated` verdict (`parser.rs:79-81,200,207`), and a fourth, media-specific parser `src/media/parse.rs:226,241` (`parse_tv`/`parse_movie`, media area).

So for the feature: the fields exist (PTT emits 46 rule families), but the UI shows a subset of a subset, and the "why" behind every name and category is thrown away in the same function that computes it.

---

## 1. PTT → `Tags` → UI (deliverable a)

### 1.1 What PTT emits

`src/ptt/rules.rs` (3821 generated lines, from the Kodi add-on's `handlers.py` via `scripts/gen_ptt.py`, per `src/ptt/mod.rs:1-4`) holds 423 `Entry::Rule` and 9 `Entry::Custom` (`rules.rs:95,1185,1312,1511,2286,2287,3404,3504,3802`; custom handlers run in `ptt/mod.rs:321-430`: `is_adult_content`, `handle_bit_depth`, `handle_space_in_codec`, `handle_volumes`, `handle_episodes`, `handle_anime_eps`, `infer_language_based_on_naming`, `handle_group`, `handle_group_exclusion`). Rules run in order over a shrinking title (`ptt/mod.rs:276-305`), `clean_title` at `:477-519`, `parse` at `:521-571` (`fields.remove("title")` at `:565` moves the title out of `fields`).

46 distinct field names, with rule counts: languages 126, episodes 38, quality 33, seasons 28, complete 19, network 18, resolution 15, audio 14, trash 13, edition 11, extras 10, codec 10, dubbed 9, date 8, channels 7, title 6, group 6, site 5, 3d 5, year 4, hdr 4, bit_depth 4, upscaled 3, subbed 2, region 2, ppv 2, episode_code 2, volumes 1, unrated 1, uncensored 1, torrent 1, size 1, scene 1, retail 1, repack 1, remastered 1, proper 1, hardcoded 1, extension 1, documentary 1, country 1, convert 1, container 1, commentary 1, bitrate 1, adult 1 (+ custom).

Value shapes (`ptt/mod.rs:17-32` transforms; `Value` `:50-56`): UniqValue 161, Value 95, Boolean 66, ArrayInteger 40, Range 25, None 11, Date 8, Lowercase 6, Uppercase 4, Integer 3, RangeXOfY 2, FirstInteger 1, Resolution 1. `MAX_RANGE` 10_000 (`:140`) caps episode/season ranges. `ADULT_WORDS` from `combined-keywords.dat` (1043 lines, `:119-121`).

### 1.2 What `Tags` keeps (`src/release_tags.rs:6-32`, filled at `:88-117`)

title, resolution, quality, hdr, codec, bit_depth, audio, channels, network, edition, year, group, languages (codes mapped through a 48-entry `language_name` table `:35-86`; unknown codes are silently dropped by `filter_map` at `:104`), seasons, episodes, site, container, proper, repack, remastered, hardcoded, dubbed, subbed (`:109-114`), three_d (`:115`).

### 1.3 Field-by-field: PTT field → kept in `Tags`? → shown where

Surfaces: **Row** = results row `web.rs:744-798` (tags built by `name_tags` `:720-732`); **Rel** = release page facts `release_html` `:1028-1201`; **Copy** = `copy_card` `:1463-1548`; **Dup** = duplicates page heading `:1613-1614`; **API** = newznab attrs `api.rs:285-310`; **Filter** = tag chips `web.rs:274,281-292`.

| PTT field | In `Tags` | Row | Rel | Copy | Dup | API | Filter | Notes |
|---|---|---|---|---|---|---|---|---|
| title | yes (`Parsed.title`) | no | yes, fact "Title" → `/?q=` `:1058-1060` | no | yes, cluster heading `:1613-1614` | no | no | |
| resolution | yes | yes `tag-ref` `:722-724` | yes (via `name_tags` `:1054`) | yes `:1475` | no | `resolution` | yes `2160p/1080p` `:274,:281-292` | |
| hdr | yes (list) | yes, one `tag-ok` each `:725-728` | yes | yes | no | `hdr` | yes `DV/HDR` | |
| quality | yes | yes plain `:729-731` | yes | yes | no | `source` | yes `Remux/WEB-DL/BluRay` | |
| codec | yes | no | "Video" `:1104-1113` | "Video" `:1516-1525` | no | `video` | no | |
| bit_depth | yes | no | "Video" | "Video" | no | no | no | |
| container | yes | no | "Video" | "Video" | no | no | no | |
| audio | yes | no | "Audio" `:1114` | "Audio" `:1526` | no | `audio` | no | |
| channels | yes | no | "Audio" | "Audio" | no | no | no | |
| languages | yes (names) | no | "Languages" `:1115` | `:1527` | no | `language` | no | unknown codes dropped `:104` |
| network | yes | no | "Ripped from" `:1098-1100` | `:1528-1530` | no | no | no | |
| year | yes | no | "Year" `:1095-1097` | no | no | no | no | |
| site | yes | no | "Site" `:1101-1103` | no | no | no | no | |
| edition | yes | no | "Edition" `:1116-1118` | no | no | no | no | |
| group | yes | no | "Release group" → `/?team=` `:1119-1121` | `:1531-1533` | no | `team` | no | the `team` *filter* is a LIKE on the name (`src/field_search.rs:181-189`), not PTT |
| seasons | yes | no | no | no | no | no | no | parsed_not_stored (web-ui.md:234 agrees) |
| episodes | yes | no | no | no | no | no | no | parsed_not_stored |
| proper | yes | no | no | no | no | no | no | parsed_not_stored |
| repack | yes | no | no | no | no | no | no | parsed_not_stored |
| remastered | yes | no | no | no | no | no | no | parsed_not_stored |
| hardcoded | yes | no | no | no | no | no | no | parsed_not_stored |
| dubbed | yes | no | no | no | no | no | no | parsed_not_stored |
| subbed | yes | no | no | no | no | no | no | parsed_not_stored |
| 3d → `three_d` | yes | no | no | no | no | no | no | parsed_not_stored |
| date | **no** | — | — | — | — | — | — | 8 rules, `T::Date` |
| complete | no | | | | | | | 19 rules (year ranges, INTEGRALE, "complete series") |
| episode_code | no | | | | | | | 2 rules: 8-hex / 8-digit in brackets (CRC-style) |
| extras | no | | | | | | | 10 rules (NCED/NCOP/featurette ...) |
| ppv | no | | | | | | | 2 rules |
| trash | no | | | | | | | 13 rules (CAM/TS/TC/...) |
| upscaled | no | | | | | | | 3 rules |
| convert | no | | | | | | | 1 rule |
| retail | no | | | | | | | 1 rule |
| documentary | no | | | | | | | 1 rule |
| unrated | no | | | | | | | 1 rule |
| uncensored | no | | | | | | | 1 rule |
| commentary | no | | | | | | | 1 rule |
| region | no | | | | | | | `R\dJ?`, PAL/NTSC/SECAM |
| country | no | | | | | | | US/UK/AU/NZ/CA |
| volumes | no | | | | | | | custom `handle_volumes` |
| size | no | | | | | | | `\d+ MB|GB|TB` in the name |
| torrent | no | | | | | | | `\.torrent$` |
| scene | no | | | | | | | `rules.rs:98` |
| extension | no | | | | | | | file-extension list |
| bitrate | no | | | | | | | `\d+[kmg]bps` |
| adult | no | | | | | | | custom `is_adult_content` over `ADULT_WORDS`; category has its own adult vocabulary (`data/category_lists.tsv`, `src/category/lists.rs:3-13`) |

Summary counts: 24 PTT families reach `Tags`; 15 of those are rendered somewhere; 9 `Tags` fields are computed on every render and never printed; 22 PTT families are dropped before `Tags`. Site-style `site` rules (`rules.rs:215,224,3561,3570,3579`: `www.x.y -`, `rarbg|torrentleech|piratebay`, `[site.tld]`) are torrent-origin tells that leak into Usenet names via cross-posts.

Also dropped inside PTT itself (not recoverable from `Parsed`): which rule matched each field, the matched span, the leftover title before `clean_title` (`ptt/mod.rs:477-519`), and rule order. A "parse evidence" panel would need `run_rule`/`run_custom` (`:276-305`, `:321-430`) to record `(rule_name, pattern_index, span)`.

### 1.4 Where `is_obfuscated` and `name_source` are shown

Not PTT, but part of the same name panel: the `OBFUSCATED` tag (`web.rs:1043-1045`, copy card `:1476-1478`) reads `release_info::Names.obfuscated` ← `releases.is_obfuscated` (`src/release_info.rs:15-21,136-150`); "Name from" (`web.rs:1091-1093`) reads `name_source` through `release_info::name_source_label` `:24-35` (PostedFile→"a posted file name", NamingRegex→"the subject", Nfo→"the NFO", ArchiveInner→"a file inside the archive", Par2→"the PAR2 file", PreDb→"PreDB"; **Cleared(1) and InlineBody(9) return None → stored_not_shown**). "Posted as" (`:1087-1090`) shows `releases.name` when it differs from the display name; the `<h1>` is `coalesce(display_name, name)` (`search.rs:33-34`, template `web.rs:1183-1200`).

---

## 2. Naming evidence chain (deliverable b)

### 2.1 Ingest (indexer process)

1. `src/parser.rs:55-72` `SUBJECT_PATTERNS` (10 patterns; several accept subjects with no `yEnc` token), `FILE_COUNT_RE :74`, `RELEASE_SUFFIX_RE :76-77`, `parse_subject :83-125` → release name, part/file counts. `is_obfuscated` wrapper `:79-81` → `Release.is_obfuscated :44`, set in `group_articles_by` at `:200,:207`.
2. `src/collections.rs:81-88` `subject_base` appends `" yEnc"` when missing (erases the NewsUP "no yEnc" tell); `FILE_SIZES :96` strips `"? N bytes` (erases the ypost tell). Zero-padded counters are normalized by `FILE_COUNT_RE`, so the newsmangler zero-pad width is lost too.
3. `src/indexer.rs:1769-1782` inline `first_names` (from in-article file names, `NAME_SOURCES_MAX` 8 `:1806`, `name_sources :1809-1820`) → `display_name` with `name_source` null.
4. `src/store.rs:1390-1403` save upsert writes `display_name`/`is_obfuscated` **only while `name_source is null`** (so the stage owns them once it has spoken); inline rename metrics `:1422-1430`; `inline_rename :1723-1741`. FTS and triggers `:149-163`; releases schema `:133-145`. `src/db.rs:484-489` migrates the old `obfuscated` column into `is_obfuscated`.

### 2.2 Stage (postprocess)

- `Naming` step `src/naming/steps.rs:20-61`: StepId(8) (`naming/mod.rs:49`), rank 200 `:28`, `Needs::Database :31`, enabled by `cfg.naming.enabled :34` (`src/config.rs:517-540`), skips unless `worth_running` (`decide.rs:67-83`) `:37`, `Reruns{on_rename:false, on_growth:true} :40`, reads inner files `:45`, PreDB rerun bit StepId(16) `:47`, cleared/inline opt-in `:49-57` via `meta.naming_rename_named` (`:65-71`), passworded guard `:58`, then `decide` `:59`.
- `decide` (`src/naming/decide.rs:191-233`): PAR2 → archive inner → naming regexes (`regexes.rs`, lazy per group `:109-116`, `MAX_SUBJECT` 1024 `:13`, `SUBJECTS_TRIED` 4 `decide.rs:21`) → scene token in subjects (`scene_name_in_subjects :178-189`, rot13 subjects skipped `:156-173`) → posted file names (`FILES_TRIED` 256 `:22`, `best_from_paths :123-153`). Candidates are `Candidate{name, grade, weight, rank}` (`:114-119`, ranked `:100-112`), sorted, and only the winner survives as `NameChange { display_name: Some(name), obfuscated: Some(false), source }` (`:228-232`).
- Gate: `improves` (`decide.rs:46-58`) = source ≥ `effective_source` (`:28-34`) AND strictly better `Grade` (`src/naming/judge.rs:47-53`; `judge :56-83`: Junk if GENERIC/SEGMENT/INSTALLER or `is_obfuscated` `:62`; Scene needs ≥4 words + SCENE_SHAPE + TECH `:70-72`; Good `:73-78`; Weak `:79-81`). Season-pack guards `:87-102`.
- Cleaning: `src/naming/clean.rs` WRAPPER `:16-19`, CODEC_AFTER_WRAPPER `:20-23`, PAYLOAD `:24-27`, SAMPLE/SEGMENT tails `:28-29`, PREFIX `:30`, `COLLISION_SUFFIX` ` [\d{8}]$` `:31`, NZBSPLIT `:32`, ROT13_YENC `:34`, SKIPPED_DIR `:36`; `clean_candidate :57-72`; `identity_key :81-83`. **No Sonarr-style re-uploader suffix list** (`[rartv]`, `[eztv]`, `-Obfuscated`, `-postbot`, `-xpost`, `-BUYMORE`, `-AsRequested`, `-Scrambled`) exists anywhere in `src/naming`, `src/ptt/rules.rs`, `src/category`, `src/release_tags.rs` (grep confirmed).
- `NfoName` step `steps.rs:73-128` (rank 210, `Needs::Bodies`, enabled by `archive.enabled && naming.nfo_step :90`; `src/nfo.rs:91-124` candidates).
- Archive finish `src/archive/steps.rs:155-164`: PAR2/inner names; **`obfuscated: Some(release.is_obfuscated)`** at `:164`, i.e. an archive-named release keeps its OBFUSCATED tag even though it now has a clean name. `decide`'s own path writes `Some(false)`.
- PreDB `src/predb/step.rs:261-265`: source 100, `obfuscated: Some(false)`, bypasses `improves`.
- Engine: `src/postprocess/engine.rs:617-664` step loop, facts append `:640`, `apply_name :1062-1070`; `src/postprocess/apply.rs:113-163` writes names with `update releases set display_name = coalesce(?2, display_name), is_obfuscated = coalesce(?3, is_obfuscated), name_source = ?4 where id = ?1` (`:158-162`); metrics `NAMES_APPLIED`/`DEOBFUSCATED` (`apply.rs:52-61`, `src/metrics/names.rs:99-100`).
- Schema: `RELEASE_COLUMNS = [("name_source","INTEGER")]` (`src/postprocess/schema.rs:94-96`); no naming table. `docs/port/05-name-fixing.md:187-195` "Schema: None" means no new table; decision 22 (`:1203-1232`) explicitly rejected a `naming_previous` table. A Name-evidence table reverses that decision and should say so.

### 2.3 Stored vs dropped

| Evidence | Computed at | Persisted? | Where |
|---|---|---|---|
| Posted name | ingest | yes | `releases.name` |
| Display name | ingest (inline) / stage | yes | `releases.display_name` |
| Source of the name | stage | yes (`name_source` int; null = inline/ingest) | `releases.name_source` |
| Obfuscated verdict (bool) | ingest + stage | yes | `releases.is_obfuscated` |
| Which obfuscation sub-rule fired (LONG_ALNUM / hex / mixed / consonant run / marker ...) | `src/naming/obfuscation.rs:125-164` | **no** — returns `bool` | — |
| Rejected candidates, their `Grade`, weight, rank | `decide.rs:114-119,191-233` | **no** | — |
| Which naming regex (rule id of 1154) produced the name | `regexes.rs:54-59` has `Rule.id`; `name_from :63-67` and `names() :120-126` return only the joined captures | **no** | — |
| Which subject / which posted file the name came from | `decide.rs:156-189,123-153` | **no** | — |
| Whether `improves` rejected a better-looking name because of grade/source | `decide.rs:46-58` | **no** (no metric either) | — |
| Step outcome facts (`category`, `category_rule`, ...) | `step.rs:205-223` | **no** — "for later steps in the same run" `:215-216` | — |
| Rename history | — | **no** (05 decision 22) | — |
| Aggregates: name_source tallies, still_obfuscated | `src/metrics/catalog.rs:160-240,447-453` | metrics only | Prometheus labels `CATALOG_NAME_SOURCE/CATALOG_CATEGORY` (`metrics/names.rs:144-145`) |

### 2.4 What a "Name evidence" panel needs

Minimum content for every release page: posted name (have), display name (have), source label (have, add labels for Cleared/InlineBody), obfuscation verdict (have) **plus** the obfuscation sub-rule, the winning rule id / subject / file, the losing candidates with grades, and ideally the PTT rule hits.

Storage options (ranked by fit with existing conventions):

1. **New `ShardTable` `release_naming(release_id INTEGER PRIMARY KEY, evidence TEXT /*JSON*/)`** registered in `SHARD_TABLES` (`src/postprocess/schema.rs:78-92`) so `migrate` adds the gc trigger (`:114-131`) that deletes rows with their release. Written by `Naming`/`NfoName`/archive/predb through a widened `NameChange` (`step.rs:239-248`) or by persisting `Outcome.facts` in `apply::write` (`apply.rs:113-163`). Size: a 200-400 byte JSON per named release; ~15M named releases (README `:506-512` figure for `--unname`) → 3-6 GB; if written for all 118M → 25-50 GB.
2. **Widen `release_category`** to `(release_id, category, rule, confidence)` by changing `UPSERT` (`src/category/step.rs:24`) — the rule string is already in `Decision` (`rules.rs:36-41`) and emitted as a fact (`step.rs:92`).
3. **A `release_tags` cache table** holding the PTT `Tags`/full `Parsed` as JSON, written by a new step (new StepId, `Needs::Database`, `Reruns{on_rename:true,...}` so it follows renames). This is what makes "every field on every page" cheap; see §4 for the cost of not doing it.

Backfill precedents and costs:

- Read-only cursor walk: `src/naming/walk.rs` (BATCH 2000 `:13`, SHARDS_AT_ONCE 4 `:14`, `open_for_reading` query_only `:39-43`, `take_lock` refuses while the indexer is alive `:60-70`, `WalkLimits` env knobs `:77-91`, cursor in `meta` `:140-144`, `step :148-177` reads outside the tx and writes + cursor in one Immediate tx with `checkpoint_or_stop`). `reflag.rs` (cursor `reflag_after :12`, candidates `:21`, update `:85`) and `unname.rs` (cursor `unname_after :17`, `:20`, clear sql `:90`, pp_state `:93`, pp_queue `:97`, drops `naming_rename_named` keys `:111`) are the two existing commands built on it. README `:506-512`: `--reflag-obfuscated` over 118M releases 2-10 min and ≤~12 GB WAL; `--unname` 15M names 5-10 min, 30-60 GB WAL; stops if a reader holds the WAL past 4 GiB. Those are the right size references for a one-column backfill; a JSON-blob backfill is heavier (more bytes per row, page splits).
- Stage re-walk: `atlas --reprocess --step <x> --apply` (`src/postprocess/reprocess.rs`). **Caveat**: when the redo mask includes NAMING, `apply` sets `meta.naming_rename_named = 1` (`reprocess.rs:25-40`), which opts inline/cleared-named rows into being renamed — an evidence backfill must not reuse the naming StepId blindly. Use a new StepId (`src/postprocess/step.rs:16`) so the walk is independent. Throughput: `backlog_per_second` default 2000 (`src/config.rs:605,628`) → 118M rows ≈ 16.4 h (`docs/port/05-name-fixing.md:1090-1134`); `decide` costs 11-40 µs, compiling all naming rules 100-320 MB (same section).
- Recomputing evidence for the ~103M never-renamed rows is cheap (`worth_running` short-circuits) but the panel still needs a row saying "nothing tried / nothing better" — decide whether absence of a row is the evidence.

### 2.5 `is_obfuscated` semantics worth exposing

- Rules (`src/naming/obfuscation.rs`): doc `:1-2` ("A flagged name leaves search (src/search.rs:36)"); `LONG_ALNUM ^[A-Za-z0-9]{16,}$ :10`; `PAYLOAD_EXT :12-14`; `MARKER :16-18`; thresholds `:23-31` (MIN_TOKEN 8, MIN_MIXED_TOKEN 6, MIN_PAIRS 5, PLAIN 25 %, MIXED 45 %, MAX_CONSONANTS 6, MIN_HEX 12, MIN_JOINED_HEX 16, MIN_MIXED_CHARS 12); `classify :53-106`; `is_camel :108-123`; `is_obfuscated :125-164`.
- Applied to: release names at ingest (`parser.rs:200,207`), to candidate file names in `judge` (`judge.rs:62`), in archive naming (`archive/steps.rs:157` via `looks_random`/`stem`, `src/archive/names.rs:58-68`), and in the category `misc_pipe` (`rules.rs:551-552` → HASHED 8020, confidence 900, rule `"obfuscated_name"`; `docs/port/07-categories.md` decision 24 says it deliberately reuses this function).
- Effect: `VISIBLE_RELEASE` (`src/search.rs:37-40`) hides rows with `display_name is null and (is_obfuscated = 1 or (length(name) ≥ 16 and name has no non-alnum))`; `search_obfuscated :532-540`, `count_obfuscated :542-550`; terminal "3. Obfuscated Posts" `src/app.rs:257,288`; `src/stats_dashboard.rs:187-200` counts. **`get_release_with` (`search.rs:557-560`) does not apply `VISIBLE_RELEASE`**, so a hidden release still renders at `/ui/release?id=` (and the NZB route that uses the same getter).
- Superset decision (05 §13 item 6): the stage flag is a superset of the ingest flag; item 8: reflag is a command, not a step. Old rows keep the old verdict until `--reflag-obfuscated` runs (`05-name-fixing.md:177-185`).

---

## 3. Category (deliverable c)

- Rules: `src/category/rules.rs:1-4` doc; `Options{foreign, web_dl}` `:17-26` (defaults true/false, from `CategoryConfig` `src/config.rs:326-357`); `Input :28-34`; `Decision{category, confidence, rule} :36-41`; `regex::bytes` builder, Unicode off, size_limit 32<<20 `:53-60`; `rules!` macro → `static R: LazyLock` `:62-67` (compiled lazily, once per process, only in a process that classifies); ~240 regexes `:74-344`; `is_hidden :385-388` (negated `VISIBLE_RELEASE`); `protected`/`locks :391-401`; `classify :419-453`; `classify_release :456-465` (name = `display_name.unwrap_or(name)`); `stores :468-470` skips `no_match`/`hidden`/`unnamed`/`obfuscated_name`; `misc_pipe :545-578`; pipes `:582-1264`; `classify_is_cheap` test is `#[ignore]` (`:1485`).
- Step `src/category/step.rs`: `RELEASE_CATEGORY` ShardTable `:13-22`; `UPSERT :24`; `DELETE :25`; `has_row :27-33`; StepId(24) `:53`; rank 400 `:59`; `Needs::Database :62`; `enabled` copies options `:64-67`; **`Reruns{on_rename:true, on_growth:true}` `:69-71`**; `run :72-95` (metrics `CATEGORIZED`/`RELEASE_SIZE` `:76-80`, `src/metrics/names.rs:103-104`; writes `:82-88`; `facts: [("category", id), ("category_rule", rule)]` `:92`). `tests/category_stage.rs:168-183` proves the fact reaches a later step in the same run — and nothing else.
- Table `src/category/table.rs`: UNCATEGORIZED 8000 "read-time only, never stored" `:3`; MISC 8010 `:4`; HASHED 8020 `:5`; 70 leaves `:22-134`; `ALIASES :137`; `root_of :139`; `label :152-159`; `parse_filter :172-195`; `caps_xml :198-208` (used by `api.rs:205`). `src/category/mod.rs` `categories_of :21-26`, `categories_of_with :28-47` (fills 8000 for missing rows).
- Readers: `web.rs:751` (row), `:1000` (`release_page`), `:1055` (fact → `/?cat=`); `api.rs:263` (cat filter), `:268`, `:333-337` (rss category attr); `metrics/catalog.rs`.
- Storage facts: `07-categories.md:218-268` — no row for no_match/hidden/unnamed/obfuscated_name, 60-85M of ~100M rows reaching the step write nothing (`:259`); row sizes 15M/40M/100M → 375 MB/1 GB/2.5 GB (`:759-827`); classify 4-21 µs. Decisions (`:894-922`): 22 — a config change (`foreign`/`web_dl`) does **not** rerun categorization (`config_stamp` `step.rs:60` exists but is not used by this step to force a rerun); 26 — images skip.
- Consequences for the feature: the fired rule and confidence are not stored, so a "Category evidence" panel has nothing to read today; it needs the UPSERT widened or a facts table. Because the step reruns on rename and growth, any stored rule must be rewritten by the step (not a one-off backfill), and a backfill must run with the same `Options` the step uses or the stored rule will disagree with the stored id. A release whose category decision was `no_match` has no row and shows "Uncategorized" (8000) at read time; the panel must distinguish "never classified" (pp_state bit 24 unset) from "classified, no match" — both look identical in `release_category` today.

---

## 4. PTT parse cost per surface (deliverable d)

`src/web.rs:400-401` comment: "Reading a name's tags takes about a millisecond, soo they are read a slice at a time, on every core". No benchmark in tests; `fancy_regex` 0.17 (`Cargo.toml:27`) allows backtracking.

| Surface | Parses per request | Where |
|---|---|---|
| Results page, no tag filter | 50 (`PAGE_SIZE` `web.rs:36`) | `row :744-798` → `release_tags::parse` at `:758` |
| Results page with a tag chip | up to 2000 (`SORT_WINDOW :199`) in slices of `TAG_SLICE` 128 `:278`, parallel | `has_tags :392-398`, `keep_tagged :402-423`, `filter_matches :281-292` |
| Results page in PR #57 | `Window::read` calls `search.keep_tagged(rows, usize::MAX)` over the whole window → 2000 parses per window build, windows kept warm by `keep_sort_windows_warm` (`api.rs` +3) | pr/57 e1ad6c3, web.rs hunks 184-730 |
| Release page | 1 (`:1053`) + 1 per copy card, up to `FULL_COPIES` 12 (`:1456`, `copy_card :1464`) = 13 | `release_html`, `copy_card` |
| Duplicates page | 1 heading parse per cluster (`CLUSTERS_PER_PAGE` 10 `:1457`, `:1613-1614`) + 1 per shown member (`FULL_MEMBERS` 6 `:1459`, `copy_card`) ≈ 70 | `duplicates_page` |
| API search/rss | 1 per item in `release_attrs` (`api.rs:285-310`), up to the request limit | `api.rs` |

At ~1 ms/name: a release page is ~13 ms of parsing, a filtered results page up to ~2 s of CPU spread across cores (or amortized by #57's warm windows). Adding fields to `Tags` adds no parses (the full `Parsed` is already computed and discarded per call); adding fields to the copy card likewise. Storing `Tags` would remove the per-render cost but requires a one-time 118M-row parse ≈ 33 core-hours plus the 16.4 h stage walk envelope (§2.4).

---

## 5. Data model (this area)

| Table / key | Columns | Written by | Read by |
|---|---|---|---|
| `s{N}.releases` | `name`, `display_name`, `is_obfuscated` (schema `store.rs:133-145`; migration `db.rs:484-489`), `name_source` (`postprocess/schema.rs:94-96`) | ingest `store.rs:1390-1403` (display/obfuscated only while `name_source is null`), `inline_rename :1723-1741`; stage `apply.rs:158-162`; `reflag.rs:85`; `unname.rs:90` | `search.rs:33-34` (`COLUMNS`), `:37-40` (`VISIBLE_RELEASE`), `release_info.rs:136-150`, `web.rs` (h1, Posted as, Name from, OBFUSCATED), `category/rules.rs:456-465`, `metrics/catalog.rs:447-453` |
| `s{N}.release_category` | `release_id`, `category` (leaf id only) | `category/step.rs:24-25,82-88` | `category/mod.rs:21-47` → `web.rs:751,1000,1055`; `api.rs:263,268,333-337`; metrics |
| `releases_fts` | FTS5 over release name text with triggers | `store.rs:149-163` | search (storage area owns the detail) |
| `pp_state.done/failed` bits | StepId 8 NAMING, 9 NFO, 16 predb-rerun, 24 CATEGORY | engine | `unname.rs:93` clears; engine |
| `meta` keys | `naming_rename_named` (+ per-group `rename_named_key` `reprocess.rs:13`), `reflag_after`, `unname_after`, walk cursors | `reprocess.rs:25-40`, `reflag.rs:12`, `unname.rs:17,111`, `walk.rs:140-144` | `steps.rs:65-71` |
| (none) | PTT fields / `Tags`, naming candidates, Grade, rule id, obfuscation sub-rule, category rule/confidence | — | — |

---

## 6. UI surface (this area)

- **Results row** (`web.rs:744-798`): category label `:751`; `name_tags` `:758` → resolution `tag-ref`, each hdr `tag-ok`, quality; `media_label :735-742` (from `MediaAttrs`, media area, not PTT); `password_tag :698-704`.
- **Release page** (`release_html :1028-1201`): OBFUSCATED `:1043-1045`; name tags `:1054`; Category `:1055`; Title `:1058-1060`; Size `:1061-1066` (download-size thread); Posted `:1067`; Groups `:1068-1078`; Poster/Email `:1079-1086`; Posted as `:1087-1090`; Name from `:1091-1093`; Year `:1095-1097`; Ripped from `:1098-1100`; Site `:1101-1103`; Video `:1104-1113`; Audio `:1114`; Languages `:1115`; Edition `:1116-1118`; Release group `:1119-1121`; Parts `:1122-1124`; Encryption `:1125`; Password `:1126-1128`; Files/PAR2/Articles/Repair `:1129-1138`; Links `:1139-1142`; Files inside `:1144-1161`; probe `:1163-1170`; copies `:1171-1181`; template `:1183-1200`. `tests/web_ui.rs:161-167` asserts on this page.
- **Copy card** (`:1463-1548`): tags `:1475`, OBFUSCATED `:1476-1478`, Video `:1516-1525`, Audio `:1526`, Languages `:1527`, Ripped from `:1528-1530`, Release group `:1531-1533` — no Title/Year/Site/Edition, so the copy card and the release page already disagree on which name facts matter.
- **Duplicates page** cluster heading `:1613-1614` = PTT title of the first member (Finish-web-UI twin grouping thread).
- **Tag filter chips** `FILTERS :274` (`2160p,1080p,DV,HDR,Remux,WEB-DL,BluRay,Hide copies`), `HIDE_COPIES :275`.
- **Scope inputs** `:883-914`, "Release group" field `:898` → `field_search.rs:181-189` LIKE.
- **API** `release_attrs api.rs:285-310`: poster, group, resolution, video, audio, hdr, source, language, team, twins, alts; rss category `:333-337`; `caps` `:205`.
- **Terminal/metrics**: `app.rs:257,288`; `stats_dashboard.rs:187-200`; `metrics/catalog.rs:66-79,160-240`.

---

## 7. Catalog coverage (assigned items)

| Item | Status | Evidence |
|---|---|---|
| `derived_per_post.obfuscation_level` (a graded level) | **not_collected** | Only a boolean exists: `obfuscation.rs:125-164` returns `bool`, `releases.is_obfuscated` (`parser.rs:44,79-81,200,207`). The boolean is shown as the OBFUSCATED tag (`web.rs:1043-1045,1476-1478`). Disagrees with ingest-postprocess.md:496 "shown (boolean only)" and storage.md:220 "shown" only in vocabulary: the level is not computed; the flag is shown. |
| `derived_additions.sabnzbd_obfuscation_rules` | **shown** (verdict) / parsed_not_stored (which rule) | Atlas runs its own newznab-tmux-derived rules (`obfuscation.rs:10-31,53-164`), not SABnzbd's `deobfuscate.py`; the verdict reaches the UI as OBFUSCATED and as "Name from". Which sub-rule fired is dropped (`:125-164` returns `bool`). |
| `payload_additions.repack_and_reuploader_tells` | **parsed_not_stored** | `Tags.repack/proper/remastered` computed per render (`release_tags.rs:109-111`) and never printed (`web.rs` has no fact for them); re-uploader suffix dictionary **not_collected** (no `[rartv]`/`-Obfuscated`/`-postbot`... anywhere in `src/naming`, `src/ptt/rules.rs`, `src/category`, `src/release_tags.rs`). Agrees with archive-payload.md:563. |
| `derived_per_article.subject_parse` (name side) | **shown** | `parse_subject` (`parser.rs:83-125`) → `releases.name` → `<h1>` / "Posted as" (`web.rs:1087-1090,1183-1200`); regex names → `display_name` + "Name from: the subject" (`:1091-1093`). The filename/part side stays stored_not_shown per ingest-postprocess.md:482 / storage.md:212 — no conflict once the item is split. |
| `derived_per_article.subject_template` (name side) | **parsed_not_stored** | `subject_base` (`collections.rs:81-88`) is an in-memory grouping key; the naming rule id that matched is dropped (`regexes.rs:63-67,120-126`); `filters.rs` `shape :408-418` is the per-article side (ingest area). |
| Posting-tool Subject tells (NewsUP no `yEnc`, ypost `N bytes`, sanguinews prefix, newsmangler zero-pad) | **parsed_not_stored** (tells) / **not_collected** (tool verdict) | `SUBJECT_PATTERNS` accept both shapes (`parser.rs:55-72`); `subject_base` appends `" yEnc"` (`collections.rs:81-88`); `FILE_SIZES` strips `N bytes` (`:96`); `FILE_COUNT_RE` (`parser.rs:74`) normalizes the counter so pad width is lost. No posting-tool field anywhere. |
| PTT fields in `Tags` and rendered (title, resolution, hdr, quality, codec, bit_depth, container, audio, channels, languages, network, year, site, edition, group) | **shown** (parsed at render, never stored) | §1.3 table; `release_tags.rs:88-117`; `web.rs:720-732,1053-1121,1464-1533`; `api.rs:285-310` |
| `Tags` fields never rendered (seasons, episodes, proper, repack, remastered, hardcoded, dubbed, subbed, three_d) | **parsed_not_stored** | `release_tags.rs:108-115`; no reader in `web.rs`/`api.rs` (agrees with web-ui.md:234) |
| 22 PTT families dropped before `Tags` (date, complete, episode_code, extras, ppv, trash, upscaled, convert, retail, documentary, unrated, uncensored, commentary, region, country, volumes, size, torrent, scene, extension, bitrate, adult) | **parsed_not_stored** | computed in `ptt::parse` (`ptt/mod.rs:521-571`) on every render, not copied by `release_tags::parse :88-117` |
| Category id | **shown** | `release_category` (`step.rs:24`) → `web.rs:751,1055`, `api.rs:333-337` |
| Category fired rule / confidence | **parsed_not_stored** | `Decision` `rules.rs:36-41`; in-run fact only `step.rs:92`; metric label `step.rs:76-80` |
| `name_source` | **shown** for 3/5/10/15/20/100; **stored_not_shown** for Cleared(1) and InlineBody(9) | `release_info.rs:24-35` returns None for those two; `web.rs:1091-1093` |
| Naming rejected candidates / Grade / rule id / source subject-file | **parsed_not_stored** | `decide.rs:114-119,191-233`; `regexes.rs:63-67` |

---

## 8. Extension points

1. `release_tags::Tags` + `parse` (`src/release_tags.rs:6-32,88-117`): one struct feeds results row, release page, copy card and API; adding the 22 dropped PTT families here is a pure-Rust change with zero extra parse cost. Accessors on `Parsed` (`ptt/mod.rs:66-96`: `text`, `list`, `flag`, ...).
2. Release-page facts closure `fact(...)` in `release_html` (`web.rs:1095-1121`) and the parallel list in `copy_card` (`:1516-1533`); `name_tags :720-732` for chips. Keep the two lists in step (they already differ).
3. `release_info::name_source_label` (`release_info.rs:24-35`): add labels for Cleared/InlineBody.
4. `postprocess/schema.rs` `SHARD_TABLES :78-92` (+ gc trigger `:114-131`) and `RELEASE_COLUMNS :94-96` for a new `release_naming` / `release_tags` table or column; `category/step.rs:13-25` as the template for a `ShardTable` + UPSERT.
5. `Outcome.facts` (`step.rs:205-223`) → persist in `apply::write` (`apply.rs:113-163`); `category_rule` is already emitted (`step.rs:92`).
6. `NameChange` (`step.rs:239-248`) → carry evidence; `decide.rs:114-119` has the candidate list before it is dropped; `regexes.rs:54-67` has `Rule.id` before it is dropped.
7. Backfill: `naming/walk.rs` (+ `reflag.rs`/`unname.rs` as command templates) for a one-off; a new StepId + `atlas --reprocess --step` for a stage walk (avoid the NAMING bit: `reprocess.rs:25-40` side effect).
8. `Step::config_stamp` (`step.rs:60`) if category evidence must follow option changes (today it does not rerun; 07 decision 22).
9. `api.rs release_attrs :285-310` and `caps_xml` (`table.rs:198-208`) for API exposure of new fields.
10. `metrics/catalog.rs Tally :160-185` already walks every release and could count evidence coverage during a backfill.

---

## 9. Risks

1. **Hidden releases leak through direct links**: `get_release_with` (`search.rs:557-560`) ignores `VISIBLE_RELEASE`; `/ui/release?id=` and the NZB route show rows search hides. A "show everything" page makes this more visible, not less.
2. **OBFUSCATED tag persists after an archive rename** (`archive/steps.rs:164` writes `Some(release.is_obfuscated)`), while regex/PreDB renames clear it (`decide.rs:230`, `predb/step.rs:261-265`). A panel that prints both the flag and "Name from: a file inside the archive" will look contradictory.
3. **Render-time PTT cost scales with every surface** (§4): 13 parses per release page, 2000 per warm window in PR #57, 70 per duplicates page. `fancy_regex` backtracking has no per-name budget.
4. **Storing anything per release is a 118M-row backfill**: ≈33 core-hours of PTT at 1 ms/name; stage walk 16.4 h at 2000/s; WAL pressure (README `:506-512`: 12 GB for a 1-column flag, 30-60 GB for 15M renames); `take_lock` refuses while the indexer runs (`walk.rs:60-70`).
5. **`--reprocess --step naming --apply` has a side effect**: it flips `meta.naming_rename_named` (`reprocess.rs:25-40`), opting inline-named rows into renaming. An evidence backfill that reuses the naming StepId will rename releases.
6. **Category reruns on rename and growth** (`step.rs:69-71`): a stored rule must be maintained by the step; a config change does not rerun (07 decision 22), so stored rule ≠ current rules after an options change. `Rules` compile lazily per process (`rules.rs:62-67`, 32 MB limit `:53-60`): classifying in the web/API process (e.g. to show "would be X") adds that compile there.
7. **Three parsers, two adult vocabularies**: PTT (`ptt/mod.rs`), `media/parse.rs:226,241`, category regexes; `ADULT_WORDS` (`ptt/mod.rs:119-121`) vs `data/category_lists.tsv` (`lists.rs:3-13`). Showing all fields side by side exposes their disagreements (resolution/source/adult).
8. **`language_name` drops unknown codes silently** (`release_tags.rs:104`): a "Languages" fact can be empty though PTT found languages. PTT emits 126 language rules; the map has 48 entries (coverage check inconclusive, see open questions).
9. **Decision 22 (no history table)** in `docs/port/05-name-fixing.md:1203-1232` is reversed by any Name-evidence store; the design doc needs amending.
10. **Merge conflicts**: PR #48 (06847b2) touches `web.rs` hunks 61, 1091-1150, 1190, 1228, 1280-1390, 1685 and `release_info.rs` (+35: `NzbFile`, `Contents.list`, `MAX_LISTED_FILES` 1000) — new facts in 1095-1121 collide textually. PR #57 (e1ad6c3) rewrites `web.rs` 184-730 (Search body, `has_tags`, `keep_tagged` → `Window`) — does not touch `release_html`/`copy_card`/`name_tags` but changes where/how often `release_tags::parse` runs. `origin/feat/release-stills-strip` touches `web.rs` 61, 1110-1142, 1661 (`details_html`), stacked on #48 and unpushed (coordination.md). PRs 51/54/55/58/21/49/50 touch none of this area's files. The download-size thread owns the Size fact (`:1061-1066`); the media thread owns stream/HDR/encoder/cover; the Finish-web-UI thread's twin grouping uses the PTT title at `:1613-1614`.
11. **Tests**: `tests/web_ui.rs:161-167` asserts release-page content; `tests/ptt_parity.rs` + `tests/fixtures/ptt/{expected.jsonl,names.txt}` (16 names) pin PTT output; `tests/naming_regexes.rs` pins 1154 rules / 118 groups / 1045 samples; `tests/category_stage.rs:168-183` pins the in-run fact. Widening `Tags` is safe; changing PTT internals to record evidence must keep parity.

---

## 10. Contradictions resolved by reading code

- **api-cli.md:139** says "Only the API parses the name with `release_tags::parse`; the UI shows those as facts from its own code." Wrong: `web.rs:396` (`has_tags`), `:758` (`row`), `:1053` (`release_html`), `:1464` (`copy_card`), `:1613` (`duplicates_page`) all call `release_tags::parse`. The UI parses more often than the API.
- **obfuscation_level**: ingest-postprocess.md:496 "shown (boolean only)" and storage.md:220 "shown" vs this note's not_collected (level) + shown (boolean). Same facts, different vocabulary; the level is not computed anywhere (`obfuscation.rs:125-164` returns `bool`).
- **subject_parse**: ingest-postprocess.md:482 / storage.md:212 "stored_not_shown" is the filename/part side; the name side is shown (§7). Split the item.
- **web-ui.md:234** (9 `Tags` fields parsed_not_stored) — agrees; this note adds the 22 PTT families `Tags` never sees.
- **archive-payload.md:563** (repack_and_reuploader_tells parsed_not_stored) — agrees; adds that the re-uploader dictionary is not_collected.
- **docs/port/05 §5 "Schema: None"** vs `RELEASE_COLUMNS` `name_source` (`schema.rs:94-96`): naming adds a column through the stage schema, not a table; §4 of the same doc (`:137-152`) documents the column. Not a real contradiction, but "None" is misleading for the feature.
- **07 decision 24** (misc_pipe uses Part 5's `is_obfuscated`) confirmed at `rules.rs:551-552`.

---

## 11. Open questions

1. PTT language codes (126 rules, `UniqValue`) vs `release_tags::language_name` (48 entries, `:35-86`): the awk cross-check captured nothing, so coverage is unverified; any unmapped code is dropped at `:104`.
2. Whether `releases_fts` (`store.rs:149-163`) indexes `display_name` as well as `name` (storage area).
3. Real per-name PTT cost on production hardware: only the `web.rs:400-401` comment (~1 ms) exists; `classify_is_cheap` for category is `#[ignore]`.
4. Product decision: store naming history/candidates (reverses 05 decision 22) or recompute on demand (cheap for the ~103M never-renamed rows, impossible for the ~15M renamed ones because the losing candidates and the subjects tried are gone — unless the subjects table still has them).
5. Whether the web/API process should ever classify or run naming regexes (adds 32 MB + 100-320 MB of compiled regex per process; `rules.rs:62-67`, 05 §9).
6. Vocabulary for `sabnzbd_obfuscation_rules`: this note calls the verdict "shown" because an equivalent rule set exists; if the catalog item means SABnzbd's exact heuristics, it is not_collected.
7. How ingest-named rows (`name_source` null, inline `first_names` `indexer.rs:1769-1782`) should present evidence: nothing records which in-article file name was used.
8. Whether PR #57's warm windows run `release_tags::parse` for every window refresh (CPU budget on the search host) — depends on `keep_sort_windows_warm` cadence in `api.rs` (+3 lines in the diff).

---

## 12. Files read

`src/ptt/mod.rs`, `src/ptt/rules.rs`, `src/release_tags.rs`, `src/web.rs`, `src/api.rs`, `src/search.rs`, `src/parser.rs`, `src/collections.rs`, `src/filters.rs`, `src/naming/mod.rs`, `src/naming/obfuscation.rs`, `src/naming/judge.rs`, `src/naming/decide.rs`, `src/naming/clean.rs`, `src/naming/regexes.rs`, `src/naming/steps.rs`, `src/naming/walk.rs`, `src/naming/reflag.rs`, `src/naming/unname.rs`, `src/category/mod.rs`, `src/category/rules.rs`, `src/category/step.rs`, `src/category/table.rs`, `src/category/lists.rs`, `src/postprocess/step.rs`, `src/postprocess/engine.rs`, `src/postprocess/apply.rs`, `src/postprocess/schema.rs`, `src/postprocess/reprocess.rs`, `src/store.rs`, `src/db.rs`, `src/indexer.rs`, `src/archive/steps.rs`, `src/archive/mod.rs`, `src/archive/names.rs`, `src/predb/step.rs`, `src/nfo.rs`, `src/release_info.rs`, `src/metrics/catalog.rs`, `src/metrics/names.rs`, `src/stats_dashboard.rs`, `src/app.rs`, `src/config.rs`, `src/field_search.rs`, `src/media/parse.rs`, `Cargo.toml`, `data/naming_regexes.tsv` (counted), `data/category_lists.tsv`, `tests/fixtures/ptt/expected.jsonl`, `tests/fixtures/ptt/names.txt`, `tests/ptt_parity.rs`, `tests/fixtures/naming_samples.tsv` (counted), `tests/naming_regexes.rs`, `tests/category_stage.rs`, `tests/web_ui.rs`, `README.md` (naming `:418-432`, backfill `:506-512`, categories `:686-700`), `docs/port/05-name-fixing.md`, `docs/port/07-categories.md`; scratchpad `catalog-current.md`, `coordination.md`, `atlas-map/{ingest-postprocess,storage,archive-payload,web-ui,api-cli}.md`; `git diff main...` for `pr/48`, `pr/57`, `origin/feat/release-stills-strip`, `pr/51`, `pr/54`, `pr/55`, `pr/58`, `pr/21`, `pr/49`, `pr/50`.
