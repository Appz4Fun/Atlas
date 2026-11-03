# Atlas map: media-title-resolution (src/media, the title → ids → details half)

Repo /home/claude/appz4fun/atlas, branch main at 55d2e51 ("Follow archives inside archives, up to 10 layers (#52)"). Read only; nothing built or run. Every line number below is at that commit unless a branch is named.

Companion notes: media.md (containers/probe half, same thread family), storage.md, web-ui.md, api-cli.md, ingest-postprocess.md, coordination.md. This file covers only what those left thin: the name → title → provider → ids → details pipeline, what each stage keeps or drops, and what of it the web UI prints.

## 0. One paragraph

A release's display name is classified (TV root 5000 / movie root 2000, src/category/table.rs:143-149) and parsed by `media::parse` into `Parsed { title, year, season, episode, episode_last, air_date, pack, quality }` (src/media/parse.rs:14-23). `MediaParse` (StepId 32, rank 500, DB lane; steps.rs:157-218) writes one `media_release` row per release in the shard: the FNV title_key, season/episode/episode_last/air_date/year/quality and whatever ids the title cache already has (UPSERT steps.rs:80-92, late params 194-215). `MediaLookup` (StepId 33, rank 510, External lane, `config_stamp = provider_bits()`; steps.rs:248-305) asks TVmaze/TMDB/Trakt/TVDB once per distinct title through `Media::resolve_until` (resolve.rs:115-200), scores candidates (score.rs:72-102), keeps only ids + the first provider's title in `atlas.media.db` `media_titles` (store.rs:9-25), and the fan-out copies the ids onto every row of the title in 1,000-row chunks (fanout.rs:17, 20-24, 27-70). `MediaNfo` (StepId 34, rank 520, Bodies lane) and `OpenSubtitlesImdb` (rank 505) put a movie's own IMDb id on the row with `imdb_from_nfo = 1` (steps.rs:98-99, 332-354; src/probe/opensubtitles.rs:55-56, 239-276). The web UI reads `media_release` through `query::attrs_with` into `MediaAttrs { kind, season, episode, air_date, year, ids }` (query.rs:314-370) and prints exactly three things from it: the `S01E02`/`S01`/year tag (`media_label`, web.rs:735-742), the "Links" fact (`id_links`, web.rs:1139-1142, 1790-1811: IMDb, TMDB, TheTVDB, TVmaze, Trakt; TVRage omitted) and the lazily loaded details panel (`/ui/details` → `details_panel` → `details::for_title` → `details_html`, web.rs:62, 1657-1715), which is TMDB/TVDB/OMDb/Trakt facts keyed by tmdb id from `atlas.details.db`. Everything about HOW a title resolved (query title, canonical `found_title`, state, which providers answered, misses, timestamps, match score) is either stored in `media_titles` with no reader in the UI, or computed and dropped.

## 1. Files read

| File | Lines | What |
|---|---|---|
| src/media/parse.rs | 381 | Parsed struct 14-23, rules T1-T14 31-44, cut/clean 46-69, quality regexes 71-78, `quality()` 89-113, `clean_title` 116-139, `find_marker` 176-224, `parse_tv` 226-239, `parse_movie` 241-263, tests 265-381 |
| src/media/title.rs | 90 | Kind 7-11, `normalize` 17-36, `title_key` 39-51, `split_country` 54-62 |
| src/media/resolve.rs | 215 | Ids 15-23, consistent/merge 30-43, TitleQuery 46-51, Answer 53-57, Trouble 59-66, Provider trait 68-73, Resolved 75-80, MAX_NEGATIVE_DAYS 106, `resolve_until` 115-200 |
| src/media/score.rs | 166 | similar 11-28, percent 31-37, Candidate 40-44, comparable 53-66, `score` 72-102 |
| src/media/fanout.rs | 104 | SECOND_PASS 16, CHUNK 17, UPDATE 20-24, chunk 27-38, pass 41-70, run 73-104 |
| src/media/store.rs | 143 | SCHEMA 9-25, STATE_* 27-29, path_for 32-35, open 37-42, TitleRow 44-58, COLUMNS 60-62, get 88-92, put 94-120, all 123-129, fanout_owed/again/finished 131-143 |
| src/media/steps.rs | 369 | MEDIA_RELEASE 25-77, UPSERT 80-92, DELETE 93, FOUND 95-97, NFO_IMDB 98-99, imdb_in_nfo 107-110, media_target 113-126, key_of 128-130, MediaParse 157-218, stale_ids 221-230, MediaLookup 248-305, MediaNfo 307-355 |
| src/media/query.rs | 534 | IdSet/IdParams 23-37, caps 40-57, id_params 61-81, from_args 158-182, episode/season/id_sql 190-214, gather 217-243, find_* 259-312, MediaAttrs 314-322, attrs_of/attrs_with 324-370, xml 376-428, tests 431-534 |
| src/media/mod.rs | 317 | Known 40-45, Media 48-65, build_providers 67-82, shared/steps/run 87-104, set_config 146-160, open 163-185, with_store 187-192, provider_bits 199-201, remember 212-220, cached_ids 242-248, imdb_known 251-253, snapshot 256-262, wants 265-307, known_of 310-317 |
| src/media/tmdb.rs | 133 | BIT 2 at 13, search 36-92, find 111-132 |
| src/media/tvdb.rs | 170 | BIT 8 at 15, series 43-57, bearer 64-84, candidate_score 100-115, answer_of 117-131, find 143-169 |
| src/media/tvmaze.rs | 111 | BIT 1 at 12, show_score 36-49, answer_of 51-65, find 83-110 |
| src/media/trakt.rs | 79 | BIT 4 at 13, find 46-78 |
| src/media/http.rs | 298 | Bucket 19-45, agent 47-59, Client 68-73, get/post_json 133-150, counted 152-170, answer 182-214, Memo 231-258 |
| src/media/details.rs | 797 | constants 23-41, SCHEMA 43-70, Details 73-94, open 101-111, read/write 113-174, from_tmdb 207-236, from_tvdb 240-264, add_omdb 277-288, trakt_score 295-298, Sources 315-438, for_title 441-483, kept_for 486-504, lookup 506-530, poster/backdrop/image 546-592, known_titles 598-624, backfill 627-686, trim_posters 688-707 |
| src/config.rs | 195-335 | TraktConfig 197-202, MediaConfig 205-219, defaults 229-245, any_provider 248-253, from_value 255-313, media_config/body_budget_set 317-324 |
| src/web.rs | 48-72, 495-580, 684-797, 989-1201, 1463-1548, 1657-1739, 1790-1811, 2204-2216 | routes, results extras, row/media_label, release_page/release_html, copy_card, details_panel/details_html/poster, id_links, test |
| src/api.rs | 195-345 | caps_body 203-208, id_search 211-242, release_attrs 285-310, rss 313-345 |
| src/postprocess/step.rs 1-80, engine.rs 280-300 + 1073-1114, schema.rs 1-160, reprocess.rs 1-60 | step trait, stamp re-walk, ShardTable.added, --reprocess |
| src/probe/opensubtitles.rs 55-56, 225-276 | SET_IMDB and OpenSubtitlesImdb |
| src/category/table.rs 143-149, src/release_tags.rs 7-32, src/search.rs 9-18, src/db.rs 20-100, src/bg_indexer.rs 875-890 + 1165-1180, src/metrics/report.rs 150-160, src/metrics/catalog.rs 534-544, src/metrics/names.rs 119 | supporting |
| docs/port/08-media-ids.md | 1124 | design doc: parser 99-155, keys 156-176, scoring 177-196, providers 197-235, resolve 236-259, title map 260-280, fan-out 281-297, schema 298-383, config 547-574, load 927-1002, decisions 1085-1124 |
| README.md 179, 720-757; config.example.toml 89-110; data/tv_title_aliases.tsv (50 lines) | docs and data |
| tests/media_*.rs (names only), tests/common/http.rs head | conventions |
| Branch diffs: pr/48, pr/51, pr/54, pr/57, pr/58, pr/55, origin/feat/release-stills-strip against their merge bases, restricted to src/media, src/web.rs, src/api.rs, src/config.rs, src/postprocess/{schema,step}.rs | conflicts |

## 2. Pipeline, stage by stage

### 2.1 Target selection (steps.rs:113-139)

`media_target(category, rule, name)` returns `(Kind, Parsed)` only when the category is TV (`is_tv`, table.rs:143-145: root 5000) or movie (`is_movie`, 147-149: root 2000) AND the category rule is not a group hint (`rule.starts_with("group_name_")` → None, steps.rs:114-116; decision 29, 08-media-ids.md:1115) AND the parsed title is ≤ `MAX_TITLE` 255 bytes (steps.rs:124-125; query.rs:19). The name used is `display_name` else `name` (`name_of`, steps.rs:132-134). `MediaParse::run` takes category and rule from the run's facts when the category step ran (182-184) else classifies for itself (`target_of`, 136-139, 185); `MediaLookup::applies` and `MediaNfo::applies` always classify for themselves (274, 330) because `applies` may not do I/O and facts do not cross lanes (decision 1-2, 08-media-ids.md:1087-1088).

### 2.2 Parsing (parse.rs)

`Parsed` (14-23): `title: String`, `year: Option<i32>`, `season`, `episode`, `episode_last`, `air_date: Option<i32>` (YYYYMMDD), `pack: bool`, `quality: i32`.

TV (`parse_tv`, 226-239): the first of T1-T14 (31-44) that matches decides (`find_marker`, 176-224). Capture 1 is the title prefix; `clean_title` (116-139) strips a `[Group]` prefix (46), junk/`sample-`/`proof-`/`repost-` (47), cuts at the first quality token (CUT, 48-51), takes a trailing `(YYYY)`/`YYYY` as the year (YEAR 52, 123-126), joins acronyms (53, 127), replaces parentheses/`.`/`_` with spaces (54, 128), removes language words incl. `DL`, `Dubbed`, `MULTI` (55-58, 129), trims, requires ≥ 2 alphanumerics (133-135) and maps the lowercased result through `data/tv_title_aliases.tsv` (11, 80-81, 136-137; 50 rows, e.g. `shield → Agents of S.H.I.E.L.D.`). Shapes:

| Rule | Shape | season | episode | episode_last | air_date | pack |
|---|---|---|---|---|---|---|
| T1/T2 (31-32, 177-183) | S01E02-E04, S01E0203 | 1 | 2 | 4 / 3 only when > episode (180) | - | false |
| T3 (33, 184-186) | S01E02, S2019E03, E02a, v2 | 1 | 2 | - | - | false |
| T4/T5 (34-35, 187-189) | S03D2, S01 | 3 | None | - | - | true (168) |
| T6 (36, 190-192) | 1x05 | 1 | 5 | - | - | false |
| T7 (37, 193-198) | 2024.03.15 (real date only, 145-150) | 2024 | 315 | - | 20240315 (172-174) | false |
| T8 (38, 199-213) | 15.03.2024 / 03-15-2024; separators must match; >12 decides order else `.` is d.m | 2024 | 315 | - | 20240315 | false |
| T9/T10 (39-40, 214-216) | 2023.E05, 2015.Part2 | 2023 / 2015 | 5 / 2 | - | - | false |
| T11/T12/T13 (41-43, 217-219) | Part.3, EP06, `[Grp]_Show_-_12_[720p]` | 1 | 3 / 6 / 12 | - | - | false |
| T14 (44, 220-222) | Season.3 | 3 | None | - | - | true |

`pack` is simply `episode.is_none()` (168). Movie (`parse_movie`, 241-263): M1 `title YYYY` (63, 243-244), else M2 title before a quality token (62, 64-65, 245-246), else for names under 101 bytes without a file extension the whole name minus `-GROUP` (247-248); never sets season/episode/pack (262).

Quality code (`quality`, 89-113; `resolution | source << 4`, 112):

| resolution (low nibble) | regex | source (high nibble) | regex |
|---|---|---|---|
| 4 | 2160p / 4k / uhd (71) | 1 | blu-ray / bluray / bdrip / brrip / remux (75) |
| 3 | 1080p / 1080i (72) | 2 | web / web-dl / webrip (76) |
| 2 | 720p (73) | 3 | hdtv / pdtv (77) |
| 1 | 480p / 576p (74) | 4 | dvd / dvdrip / dvdr / dvd5 / dvd9 (78) |
| 0 | none | 0 | none |

So 50 = 720p HDTV, 34 = 720p WEB, 35 = 1080p WEB, 19 = 1080p BluRay, 20 = 2160p BluRay, 64 = DVD with no resolution, 48 = HDTV with no resolution, 3 = 1080p with no source (test table 274-317, 345-359). Nothing in the repo decodes this integer back to words (grep `quality` in web.rs: only `release_tags::Tags.quality`, web.rs:282-289, 728-730). The UI's resolution/quality tags come from a SECOND name parser, `release_tags::parse` (web.rs:720-732, 1053-1054; Tags struct src/release_tags.rs:7-32), which also supplies the "Title" fact (web.rs:1058-1060) and "Year" fact (1095-1097). The two parsers can disagree (aliases, `Détective.Privé` handling, `Some.Show.US`), and only `release_tags` is visible.

### 2.3 Title key (title.rs)

`normalize` (17-36) transliterates, `&` → ` and `, `$` → `s`, drops apostrophes, lowercases alphanumerics, collapses. `title_key(kind, normalized, year)` (39-51) = FNV-1a 64 of `"tv|title|year"` / `"movie|title|year"` shifted right once (pinned values 77-82). `key_of` (steps.rs:128-130) uses `p.year`, so `Some.Show.2019.S02E03` and `Some.Show.S02E03` are DIFFERENT titles (different keys, separate lookups and separate `media_titles` rows). `split_country` (53-62) peels `US|UK|AU|NZ|CA|NL` off the title for the provider query only (steps.rs:283-284) — the key keeps the suffix.

### 2.4 MediaParse (steps.rs:157-218)

Reruns on rename and growth (176-178). No target: deletes a stale row (187-191, `has_row` 149-155, DELETE 93). Target: the UPSERT (80-92) with `late_params` (194-215) evaluated on the shard writer as the row is written, so `media.cached_ids(key)` (mod.rs:242-248; requires a provider on and `Known.found`) fills the six id columns from the cache at write time. The conflict clause keeps existing ids when the title_key is unchanged (`coalesce(excluded.x, media_release.x)`, 84-89), keeps an NFO/hash IMDb (`imdb_from_nfo = 1`, 89-90), and resets everything when the title changed (91-92). `matched: true` (216). Enabled by `media.parse` (172-175, default true config.rs:232).

### 2.5 MediaLookup (steps.rs:248-305) and resolve

`enabled` = any provider (263-266). `config_stamp` = `provider_bits()` (270-272; mod.rs:199-201; bits TVmaze 1, TMDB 2, Trakt 4, TVDB 8 — tvmaze.rs:12, tmdb.rs:13, trakt.rs:13, tvdb.rs:15). A new bit makes `seed_shard` queue a `steps` walk over every release (engine.rs:1103-1112; decision 32, 08-media-ids.md:1118). `applies` = `Media::wants` (274; mod.rs:265-307): false when the title is known with nothing left to ask (276-281), one claim per title for 7 days (`CLAIM_DAYS`, 37; 282-291), refuses new titles past `max_titles` (292-297; `titles_full` counter) and past `MAX_CLAIMS` 4,000,000 (36; 298-304). `run` builds `TitleQuery { kind, title (country split off), year, country }` (283-284) and calls `resolve_until` (287).

`resolve_until` (resolve.rs:115-200): loads the row (123), or a fresh `STATE_ASKING` row with `fanout_done: true` (124-132); a `STATE_NOT_FOUND` row past `next_check` is reset to no ids and mask 0 (133-137); asks each enabled provider for the kind whose bit is not in `checked_mask` (138-139); a `Found` sets the bit and merges ids only if consistent with what is already known (`consistent`, 30-34: tvdb/imdb/tmdb/tvrage must agree where both known; first provider's identity wins, decision 13) and takes `found_title` only from the FIRST provider that found it (157-159); `NotFound` sets the bit (162); `Paused`/`BadKey` records the earliest resume time (163); `Bad` records the error (164). Outcome (168-184): any id → `STATE_FOUND`, `next_check = None`, `misses = 0`, `fanout_done = false` when ids changed or the row is new; trouble and no id → stays `STATE_ASKING`; otherwise `STATE_NOT_FOUND`, `next_check = now + min(negative_ttl_days << misses, 180) days` (181-183; MAX_NEGATIVE_DAYS 106; negative_ttl_days default 30 config.rs:234, clamp 1..180 config.rs:266), `misses += 1`. Row written (185), `known` map updated and fan-out queued (186; mod.rs:212-220), claim cleared (187). Return `Later` if stopped/paused even with ids (190-195: "a provider still owed an answer keeps this release in the lane"), `Err` if bad, else Found/NotFound (196-199).

Step outcome (steps.rs:289-303): `Found` → `FOUND` write on this one row, guarded by `title_key` so a rename mid-lookup can't take the old title's ids (95-97, 232-246); `NotFound` with ids still on the newest 100 rows of the title (`stale_ids`, 221-230: "from a row deleted by hand: a wrong match") → `fan_out_again` to clear them (296-298; mod.rs:223-227; decision 35); `Later(t)` → `wait_until = t + id % 30` (302; decision 26).

### 2.6 MediaNfo (steps.rs:307-355) and OpenSubtitlesImdb (src/probe/opensubtitles.rs:236-276)

MediaNfo: enabled when `nfo && parse && body_fetches_per_minute > 0` (322-325); movies only, and only when the in-memory map says the title has no IMDb yet (`imdb_known`, 329-331; mod.rs:251-253); skips unless the row exists with `imdb_id IS NULL` (335-342); reads the first NFO file's first segment up to `NFO_READ` 65,536 bytes (101, 343-349) and the first `tt\d{7,8}` (103-110); writes `NFO_IMDB` = `imdb_id = ?, imdb_from_nfo = 1 where imdb_id is null` (98-99, 352). OpenSubtitlesImdb (rank 505, DB lane, enabled by `media.parse && opensubtitles.api_key`) copies a MOVIE's hash-matched `release_opensubtitles.imdb_id` with the SAME `SET_IMDB` shape `imdb_from_nfo = 1` (opensubtitles.rs:55-56, 262-274). So `imdb_from_nfo = 1` means "from this release's own evidence (NFO text or moviehash), not from a title lookup" — the column name is narrower than its meaning. The fan-out and FOUND writes both preserve such an id (fanout.rs:21, 24; steps.rs:96).

### 2.7 Fan-out (fanout.rs)

`run` (73-104) on a blocking thread (mod.rs:102-104; spawned bg_indexer.rs:880-883 only when postprocess is enabled and a provider is on): at start requeues every `fanout_done = 0` title (74-78; store.rs:131-133); for each key, `pass` (41-70) runs over all 8 shards (`store::SHARDS`, src/store.rs:35) one writer job per `CHUNK` 1,000 rows (17, 27-38: a `select max(release_id), count(*)` window then the `UPDATE ... indexed by media_release_title` 20-24 that writes only rows whose ids differ), then a second pass after `SECOND_PASS` 60 s (16, 98) and `fanout_finished` (88; store.rs:140-143). `fanout_jobs` counts jobs (61). Load figure: ~30 ms per 1,000 rows (08-media-ids.md:965).

### 2.8 Details (details.rs) — the second half of the "oracle"

Separate file `atlas.details.db` (96-99), opened fresh on EVERY call (`open`, 101-111: create_at + WAL pragma + schema + a `pragma_table_info` check for the `backdrop` column added later at 105-109 — the house pattern for adding a column to a non-shard table). Keyed by `(kind, tmdb_id)`; a TVDB-only show is stored under `-tvdb_id` (238-251). `for_title(cfg, kind, ids)` (441-483): finds the tmdb id from `ids.tmdb` or `lookup` (`/find/{imdb|tvdb}` remembered in `details_lookups` for `RETRY_AFTER` 1 h on a miss, 506-530, 367-382); returns the kept row when younger than `KEEP_FOR` 30 days (29, 452-458); otherwise fetches TMDB `/{tv|movie}/{id}?append_to_response=credits[,release_dates]` (385-401), adds OMDb ratings by IMDb id when `omdb.api_key` is set (412-423; only the `Ratings` array is read, 277-288) and Trakt's rating when Trakt is on (424-436, `extended=full`, only `rating`/`votes` read, 295-298); a ratings failure keeps the row but makes it stale within the hour (`try_again_soon`, 290-293). No tmdb id → TVDB `series/{id}/extended` when `tvdb.api_key` is set (475-482, 403-410, 240-264). `kept_for` (486-504) reads only, for result rows. `known_titles` (598-624) + `backfill_pass` (640-663) + `start_backfill` (673-686; started unconditionally at bg_indexer.rs:1172, `BACKFILL_DELAY` 30 s, `BACKFILL_REST` 6 h, pace `details_per_minute` 120 default config.rs:236, clamp 0..600 config.rs:272, `MAX_TITLES_PER_SHARD` 100,000 newest per shard, 595) pre-fetch details + poster for every title with an id so results show thumbnails and scores without a page open.

## 3. Providers: what is asked, what is read, what is kept

| Provider (bit, kinds) | Requests per title | Fields read | Kept | Dropped |
|---|---|---|---|---|
| TVmaze (1, TV; tvmaze.rs) | `/singlesearch/shows?q=&embed=akas` then, on 404 or score 0, `/search/shows?q=` (83-110; Memo `single_done` so a paused second call resumes, 20-21, 86-97) | `id`, `name`, `premiered`, `network.country.code`/`webChannel.country.code`, `_embedded.akas[].name` (≤1,000), `externals.{tvrage,thetvdb,imdb}` (31-65) | ids tvmaze/tvrage/tvdb/imdb + `name` as the found title (51-65) | premiered year, country, akas, which name matched, whether single or full search answered, the score |
| TMDB (2, both; tmdb.rs) | `/search/{tv|movie}?query=&...[&first_air_date_year|year]` then `/{tv|movie}/{id}/external_ids` (36-92, 111-132; Memo `searched` 21-22 holds the first answer when the second can't go) | up to 10 results: `id`, `name|title`, `original_name|original_title`, `first_air_date|release_date`, `origin_country[]` (68-91); then `imdb_id`, `tvdb_id`, `tvrage_id` (126-130) | tmdb id + imdb/tvdb/tvrage + the matching name (131) | overview/popularity/vote data in search results, original vs localized name distinction, year, countries, score |
| Trakt (4, both; trakt.rs) | `/search/{show|movie}?query=` with `trakt-api-version: 2` (46-53) | up to 10 `{show|movie}.title`, `.year`, `.ids.{trakt,tvdb,tmdb,tvrage,imdb}` (55-73) | all five ids + title (67-77) | year, slug, overview, score |
| TVDB (8, TV only; tvdb.rs) | `/login` (token 25 days, 20, 64-84) then `/search?query=&type=series&limit=10` with Bearer (143-157) | `tvdb_id`, `name`, `name_translated`, `aliases[]` (≤1,000), `translations{}`, `year`, `country` (3-letter → 2, 87-98), `remote_ids[] {id, sourceName}` for IMDB / TheMovieDB.com / TV Maze (100-131) | tvdb + imdb/tmdb/tvmaze ids + `name` (117-131) | year, country, aliases, translations, other remote ids (e.g. Facebook, Official Website), the score |

Common: `Client` (http.rs:68-73) with a token bucket (`per_minute`, one token max, 19-45; -1 = unlimited), 2 s max token wait → `Paused(now+1)` (11, 96-107), 15 s timeout, no redirects, `atlas/<ver>` UA (47-59), 2 MiB body cap (10, 186), 429 → pause for `retry-after` else 60 s (197-204), 401/403 → `BadKey` and a 1 h pause (206-210), no answer/5xx → outage backoff 60 s doubling to 1 h (123-130, 211), other status → `Bad` (212). Every request increments `atlas_media_requests_total{provider,result}` (152-170; src/metrics/names.rs:119) — the only place a per-provider outcome is observable, and only on `/metrics` (src/metrics/server.rs:79) when metrics are enabled.

Scoring (score.rs:72-102): both titles pass `comparable` (53-66: `normalize` plus acronym letters joined, so `S.W.A.T.` = `SWAT`); exact 100, else PHP `similar_text` percent if ≥ 75 (11-37, cut at 200 bytes, 8); movies without a year need an exact title (77-79); a country on our side against known candidate countries must match (`UK` = `GB`, 46-50) or 0, match +2 (86-94); both years known must be within 1 or 0, +10 (95-100). Each provider keeps the best candidate above 0 (first on a tie) and forgets the number. Nothing stores the score: `Answer::Found { ids, title }` (resolve.rs:54-57) is all that leaves a provider; `TitleRow` (store.rs:45-58) has no score column.

## 4. Data model

### 4.1 `media_release` — one row per TV/movie release, in each shard (steps.rs:25-77)

Columns: `release_id INTEGER PK`, `kind` (1 TV, 2 movie), `title_key` (FNV), `season`, `episode` (MMDD for a daily), `episode_last`, `air_date` (YYYYMMDD, dailies), `year`, `quality NOT NULL` (resolution | source<<4), `tvdb_id`, `tvmaze_id`, `trakt_id`, `tmdb_id`, `imdb_id`, `tvrage_id`, `imdb_from_nfo NOT NULL DEFAULT 0`. Indexes (45-74): `media_release_title (title_key, release_id)` and one covering index per id `(x_id, release_id, season, episode, episode_last, kind) where x_id is not null`. `release_key: Some("release_id")` → gc trigger on release delete (schema.rs:122-128). `added: &[]` (76) — no post-ship columns yet; the `ShardTable.added` mechanism (schema.rs:16-17, 102-110) adds nullable columns on open with no rewrite. Registered in `SHARD_TABLES` (schema.rs:91).

Writers: `MediaParse` UPSERT (steps.rs:80-92, params 197-213) and DELETE (93, 189); `MediaLookup` FOUND (95-97, 232-246, 291); `MediaNfo` NFO_IMDB (98-99, 352); `OpenSubtitlesImdb` SET_IMDB (probe/opensubtitles.rs:55-56, 273); fan-out UPDATE (fanout.rs:20-24, 35-36).

Readers: `query::attrs_with` (query.rs:331-370) selects `release_id, kind, season, episode, air_date, year, tvdb_id, tvmaze_id, trakt_id, tmdb_id, imdb_id, tvrage_id` (339) — NOT `episode_last`, `quality`, `title_key`, `imdb_from_nfo`; id searches `gather` → `episode_sql/season_sql/id_sql` (190-243); `has_row`/`stale_ids` (steps.rs:149-155, 221-230); `MediaNfo` current id (335-339); `details::known_titles` (598-624, `group by kind, tmdb_id, imdb_id, tvdb_id, trakt_id`); fan-out window (fanout.rs:28-33).

### 4.2 `media_titles` — `atlas.media.db`, next to the shards (store.rs:9-25, path 31-35)

Columns: `title_key INTEGER PK`, `kind`, `title` (the query title as parsed, country suffix included), `year`, `state` (0 asking, 1 found, 2 not found; 27-29), `checked_mask` (provider bits answered), six id columns, `found_title` (first provider's canonical name, ≤255 bytes), `checked_at`, `next_check` (state 2 only), `misses`, `fanout_done`. Index `media_titles_fanout` partial on `fanout_done = 0` (24). WAL (39). Created only when a provider is on (`set_config` → `open`, mod.rs:157-159, 163-185). NOTE: TVmaze is enabled by default (config.rs:237 `enabled: true`; config.example.toml:95-96; query.rs:526 "tvmaze is on unless turned off"), so a default install DOES have this file — the port doc's "every provider off" (08-media-ids.md:556) is stale.

Writers: `store::put` from `resolve_until` (resolve.rs:185; store.rs:94-120, insert-or-replace of the whole row); `fanout_owed_again` / `fanout_finished` (135-143). Readers: `store::get` (88-92) from resolve (123), `cached_ids` (mod.rs:246), fan-out (fanout.rs:83, 93); `store::all` → the `known` map at open (123-129; mod.rs:171-174); `fanout_owed` (131-133). No reader in src/web.rs or src/api.rs (grep `media_titles|found_title|checked_mask`: none). The README calls it a cache: deleting it loses no search (README.md:757; decision 4, 08-media-ids.md:1090).

### 4.3 In-memory (src/media/mod.rs)

`known: HashMap<i64, Known { next_check: u32, checked: u8, found: bool, imdb: bool }>` (40-45, 55; 310-317) loaded whole at open (171-174), updated by `remember` (212-220); `claims: HashMap<i64, (release_id, since)>` (56; 282-306); `titles` count (57) → `snapshot()` `{titles, titles_full, claims_full}` (255-262) → stats.json `extra.media` (bg_indexer.rs:490) → Prometheus gauge `MEDIA_TITLES` (src/metrics/report.rs:157-159). Per-provider `Memo` of 1,024 in-flight two-step lookups (http.rs:231-258). Sizing: ~30 MB per 1M titles, ~300 MB at the 10M cap (08-media-ids.md:557, 987; `max_titles` default 10,000,000 config.rs:235, floor 1,000 config.rs:269).

### 4.4 `atlas.details.db` (details.rs:43-70, 96-111)

`details (kind, tmdb_id, title, overview, genres, runtime, status, network, cast_names, released, bluray, poster, tmdb_score, imdb_score, rotten, metacritic, trakt_score, fetched_at, backdrop)` PK `(kind, tmdb_id)` WITHOUT ROWID; negative `tmdb_id` = a TVDB-only show (251). `details_lookups (lookup TEXT PK = "{kind}:{imdb:?}:{tvdb:?}" (506-508), tmdb_id, checked_at)`. Writers: `write` (146-174) from `for_title` (462, 481); `lookup` (525-528). Readers: `read` (113-144) via `for_title` (449, 451), `kept_for` (498), `poster`/`backdrop` (552, 558); `lookup` (513-518), `kept_for` (491-495). Scores are ints out of 100 (72, 228-232, 267-275, 295-298). Posters/backdrops are JPEG files under `app_dir()/posters` (532-543), trimmed to 4 GiB (35, 688-707).

### 4.5 Shard meta for the stamp

`meta.pp_stamp_33` holds the last seen `provider_bits` per shard (engine.rs:1103-1112); a stamp with a new bit inserts a `pp_walks` row kind `steps`, `redo_mask = 1<<33` (1081-1088, 1107).

## 5. UI surface (what prints what)

| Where | Function | Shows | Source |
|---|---|---|---|
| Release page `/ui/release?id=` (web.rs:60 → `release_page` 989-1017 → `release_html` 1028-1201) | header tags, 1038-1040 | `S01E02` / `S01` / `2019` tag (`media_label`, 735-742: season+episode → SxxExx; season only → Sxx; else year) | `MediaAttrs` from `attrs_of(&[id])` (1002) → `media_release.season/episode/year` |
| same | "Links" fact, 1139-1142 → `id_links` 1790-1811 | `IMDb · TMDB · TheTVDB · TVmaze · Trakt` links (`tt{imdb:07}`; TMDB `/tv/` or `/movie/` by `kind`; Trakt a search URL by id_type); TVRage never linked | `media_release.{imdb,tmdb,tvdb,tvmaze,trakt}_id`, `kind` |
| same, lazily | `<div id="title-details" hx-get="/ui/details?id=">` (1190) → route 62 → `details_panel` 1657-1663 → `details::for_title` → `details_html` 1679-1715 | backdrop + poster images (`/ui/poster`, 1717-1739), `<h2>` TMDB/TVDB title, score tags IMDB/RT/MC/TMDB/TRAKT (`scores_html` 1665-1677), overview, facts Released, Blu-ray, Genres, Runtime "N min", Network, Status, Cast. EMPTY when the title has no tmdb/imdb/tvdb id or no TMDB/TVDB key (details.rs:447-449, 476; web.rs:1662 `unwrap_or_default`) — no "unresolved" text | `atlas.details.db`, fetched synchronously on the request thread when cold (details.rs:441-483) |
| Results list `/ui/results` (web.rs:58 → `Search::body` 464-587 → `row` 744-797) | 755-757, 760-768 | `media_label` tag, one `best_score` tag (712-717: IMDB else TMDB else TRAKT), poster thumb when `poster_kept` | `attrs_of(&ids)` (570) + `kept_for` (574; never fetches) |
| Sort "Best rated" | 500-508, `score_value` 707-709 | ordering only | kept details |
| Copies panel (`copies_html` 1555-1572 → `copy_card` 1463-1548) | 1483-1485, 1534-1536, 1537-1542 | all score tags, "Links" fact, poster thumb per twin/alt | `attrs_with(conn, &[id])` (1465) + `kept_for` (1466-1468) |
| Newznab API (src/api.rs) | `rss` 313-345 → `MediaAttrs::xml(on)` (query.rs:378-428) | `season`; `episode` (for a daily `MM/DD` + `tvairdate YYYY-MM-DD`, 384-387); `year`; `tvdbid`, `tvmazeid`, `traktid`, `tmdbid`, `rageid` + `tvrageid` (414-419), `imdb` + `imdbid` 7 digits (420-426), each id only when its caps parameter is on (`id_params`, 61-81); `caps` (api.rs:203-208; `IdSet::caps` 40-57); `id_search` (211-242) | `media_release` |
| `/metrics` (metrics/server.rs:79) | — | `atlas_media_requests_total{provider,result}` (http.rs:168), `MEDIA_TITLES` gauge (report.rs:157-159) | in-memory |

Not read anywhere in web.rs: `air_date` (grep: only the `MediaAttrs` import at 16), `episode_last`, `quality`, `title_key`, `imdb_from_nfo`, `tvrage` (id_links omits it), anything from `media_titles`, `details.fetched_at`, `details_lookups`. Test pins: `media_labels_and_id_links` (web.rs:2204-2216), `attrs_xml` (query.rs:491-521), `id_searches_end_to_end` asserts `tvairdate` (tests/media_api.rs:130); tests/web_ui.rs asserts nothing about media labels or links.

A rendering defect worth fixing with this feature: a daily show has `season = 2024, episode = 315` (parse.rs:172-174; query.rs:503-513), so `media_label` prints `S2024E315` (web.rs:737) while the API prints `episode 03/15` + `tvairdate 2024-03-15` (query.rs:384-387); `episode_last` is stored (steps.rs:205) but the label never prints a range (`S01E02-E04` shows as `S01E02`).

## 6. Catalog coverage for this area

Rule used (proposed for the merged map, see §12): **shown** = the item's own value is printed on the release page or on a panel it loads (`/ui/details`); API-only, metrics-only and aggregates are NOT shown; **stored_not_shown** = in a table/JSON with no UI reader; **parsed_not_stored** = exists in memory during a step or request and is dropped; **not_collected** = never read from the source.

### 6.1 Catalog items

| Catalog item | Status | Evidence |
|---|---|---|
| `nzb.external_oracles` (catalog-current.md:560-562), the TMDb/TVDB/TVmaze/Trakt/OMDb half | **shown** for the oracles' ANSWERS: ids as "Links" (web.rs:1139-1142, 1790-1811), TMDB/TVDB facts + OMDb/Trakt ratings in the details panel (web.rs:1657-1715; details.rs:207-298). **stored_not_shown** for the oracle PROVENANCE: query title, `found_title`, `state`, `checked_mask`, `misses`, `checked_at`, `next_check` (store.rs:9-25; no reader in web.rs). **parsed_not_stored** for the match score and candidate facts (score.rs:72-102; resolve.rs:54-57). | see §3, §4.2 |
| Storage row "Media extras … tag ids … ids feed the existing imdbid, tvdbid, tmdbid attrs" (catalog-current.md:1190), as far as ids go | **not_collected** on main: src/probe/media.rs reads no Matroska Tags/SimpleTag (grep `Tags|SimpleTag|TMDB|IMDB` in src/probe/media.rs: nothing); the only probe → `media_release` writer is `OpenSubtitlesImdb` (moviehash, probe/opensubtitles.rs:239-276). The attrs they would feed exist (`MediaAttrs::xml`, query.rs:394-426) and the id columns exist (steps.rs:37-42), so a tag-id writer would be a new step writing `imdb_id/tmdb_id/tvdb_id` with a provenance flag (today only `imdb_from_nfo`, steps.rs:43). | |

### 6.2 `media_release` columns

| Column | Status | Evidence |
|---|---|---|
| `kind` | shown (as a selector, never printed as text) | picks the TMDB/Trakt link section (web.rs:1797, 1807) and the API attribute set (query.rs:379); the "Category" fact is `release_category`, not this (web.rs:1055) |
| `season`, `episode` | shown | `media_label` web.rs:735-742 at 1038-1040 (release page) and 755-757 (results); API `season`/`episode` query.rs:381-390 |
| `year` | shown for movies / no-season titles (web.rs:739); for TV with a season the stored year is not printed (the "Year" fact, web.rs:1095-1097, is `release_tags::parse`'s) | web.rs:736-740; API `year` query.rs:391-393 |
| `episode_last` | stored_not_shown | column steps.rs:33, written 205; read only by `episode_sql` (query.rs:192-194) and `stale_ids`; not in `attrs_with` (query.rs:339) nor `xml` |
| `air_date` | stored_not_shown (API-only) | steps.rs:34, 204; `attrs_with` reads it (query.rs:339, 351) and `xml` emits `tvairdate` + `episode MM/DD` (384-387); web.rs never reads `air_date` (grep) |
| `quality` | stored_not_shown | steps.rs:36, 206; no reader outside the UPSERT; the UI's quality tags are `release_tags` (web.rs:720-732) |
| `title_key` | stored_not_shown | steps.rs:30, 200; used by UPSERT guard (84-92), FOUND (97), fan-out (fanout.rs:22), `stale_ids` (steps.rs:224) |
| `imdb_from_nfo` | stored_not_shown | steps.rs:43, 90, 99; probe/opensubtitles.rs:56; read only inside the SQL guards (steps.rs:89, 96, 226; fanout.rs:21, 24) |
| `tvrage_id` | stored_not_shown (API-only `rageid`/`tvrageid`, query.rs:414-419) | `id_links` has no TVRage branch (web.rs:1790-1811) |
| `tvdb_id`, `tvmaze_id`, `trakt_id`, `tmdb_id`, `imdb_id` | shown | "Links" web.rs:1139-1142, 1790-1811; copy_card 1534-1536; API query.rs:394-426 |

### 6.3 `media_titles` columns (resolution provenance) — all stored_not_shown

| Column | Meaning | Evidence |
|---|---|---|
| `title`, `year`, `kind` | the query as parsed (country suffix kept) | resolve.rs:124-128; store.rs:12-14; no UI reader |
| `found_title` | the FIRST consistent provider's canonical name (TVmaze before TMDB before Trakt before TVDB, build order mod.rs:67-82) | resolve.rs:157-159; store.rs:18, 113; `known_of` ignores it (mod.rs:310-317); 08-media-ids.md:363 "for a person reading the file". The details panel's `<h2>` is `Details.title` from TMDB/TVDB (details.rs:212, 252; web.rs:1711-1712) — a canonical title IS shown, but only for titles with a tmdb/tvdb id and from a different fetch |
| `state` | 0 asking / 1 found / 2 not found | store.rs:15, 27-29; in memory as `Known.found` (mod.rs:314); only the aggregate `titles` count reaches stats.json/metrics (mod.rs:255-262; bg_indexer.rs:490; report.rs:157-159) |
| `checked_mask` | which providers answered (bits 1/2/4/8) | store.rs:16; resolve.rs:139, 154, 162; `Known.checked` (mod.rs:313) |
| `misses`, `checked_at`, `next_check` | negative-cache state | store.rs:19-21; resolve.rs:169, 173, 181-183 |
| `fanout_done` | operational | store.rs:22; fanout.rs:74-78, 88 |

### 6.4 `atlas.details.db`

| Item | Status | Evidence |
|---|---|---|
| `details.title, overview, genres, runtime, status, network, cast_names, released, bluray, poster, backdrop, tmdb_score, imdb_score, rotten, metacritic, trakt_score` | shown | details_html web.rs:1686-1713; scores_html 1665-1677; results `best_score` 712-717 |
| `details.fetched_at` | stored_not_shown | details.rs:62, 93, 138, 169; used only for staleness (452, 456, 477, 635); web.rs has no `fetched_at` |
| `details_lookups.{lookup, tmdb_id, checked_at}` | stored_not_shown | details.rs:65-69, 506-530 |
| TMDB body fields not copied (tagline, original_title/name, original_language, production_companies/countries, spoken_languages, popularity, vote_count, homepage, imdb_id, budget/revenue, number_of_seasons/episodes, seasons, created_by, crew, last/next_episode_to_air, in_production, type) | parsed_not_stored | `from_tmdb` copies only the listed keys (details.rs:207-236); the request appends only `credits[,release_dates]` (387) |
| TVDB extended fields not copied (year, aliases, airsDays/Time, remoteIds, seasons, artworks, companies, originalCountry/Language, contentRatings, trailers; `score` deliberately excluded) | parsed_not_stored | `from_tvdb` details.rs:240-264, comment 238-239 |
| OMDb fields other than `Ratings` (Plot, Director, Writer, Awards, BoxOffice, imdbVotes, Rated…) | not_collected (body fetched, only `Ratings` read) | `add_omdb` details.rs:277-288 |
| Trakt `extended=full` fields other than rating/votes (overview, certification, trailer, status, runtime, aired_episodes…) | parsed_not_stored | details.rs:429-432, 295-298 |

### 6.5 What `media::parse` extracts that neither `media_release` nor `release_tags` shows

| Fact | Status | Evidence |
|---|---|---|
| parsed `title` text (post alias, post cleanup) | parsed_not_stored in the shards: only the hash goes to `media_release` (steps.rs:200); the text reaches `media_titles.title` only when a provider asks (resolve.rs:124-132). The page's "Title" fact is `release_tags::parse` (web.rs:1053, 1058-1060), a different parser without the alias table | parse.rs:11, 80-81, 136-137 |
| `pack` flag | parsed_not_stored (parse.rs:21, 168; not in UPSERT 197-213). Derivable: `kind = 1 and season is not null and episode is null`; `media_label` prints `S03` for it (web.rs:738) so the fact is visible without the word | |
| `air_date` for dailies | stored_not_shown (§6.2) | |
| `episode_last` (multi-episode range) | stored_not_shown (§6.2) | |
| quality code meanings (resolution 1-4, source 1-4) | stored_not_shown as an undecoded integer (§2.2); no decoder exists | parse.rs:89-113 |
| TV `year` | stored; shown only through `release_tags`' Year fact, not from this column | web.rs:1095-1097 |
| country suffix (`US|UK|AU|NZ|CA|NL`) | parsed_not_stored (query only) | title.rs:53-62; steps.rs:283 |
| provider candidate facts (TVmaze premiered/country/akas; TMDB first_air_date/origin_country/original name; Trakt year; TVDB year/country/aliases/translations) | parsed_not_stored (scoring only) | tvmaze.rs:36-49; tmdb.rs:73-91; trakt.rs:55-64; tvdb.rs:100-115 |
| match score | parsed_not_stored | score.rs:72-102; best-pick loops tmdb.rs:84-91, tvmaze.rs:102-109, trakt.rs:60-65, tvdb.rs:161-168; `Answer::Found { ids, title }` resolve.rs:54-57; `TitleRow` store.rs:45-58 has no score |
| which provider supplied each id (TVmaze externals vs TMDB external_ids vs Trakt ids vs TVDB remote_ids) | parsed_not_stored: `merge` keeps the first non-None per id (resolve.rs:36-43) with no origin | |

## 7. Extension points

1. **Render-only, zero backfill** for `media_release` columns already on every row: extend the SELECT (query.rs:339-340) and `MediaAttrs` (314-322, `Default` derive so callers keep compiling) with `episode_last`, `quality`, `imdb_from_nfo`, `title_key`; print in `release_html` next to the "Links" fact (web.rs:1139-1142) through the `fact` closure (1048-1052), fix `media_label` (735-742) for dailies (`air_date` → `2024-03-15`) and ranges (`S01E02-E04`), add a TVRage link in `id_links` (1790-1811), decode `quality` with a small table mirroring parse.rs:89-113. `MediaAttrs` is shared with `copy_card` (1465), results (570), sort (501), details_panel (1659) and the API `xml` (query.rs:378) — new fields are opt-in per renderer. Unit tests: web.rs:2204-2216, query.rs:491-521.
2. **Provenance from `media_titles`** (what was asked, who answered, canonical title, state, misses, timestamps): join by `media_release.title_key` → `media_titles.title_key` (PK lookup, `store::get` store.rs:88-92) in `atlas.media.db` (`store::path_for(&paths::database())`, store.rs:31-35). Do NOT reach it through `media::shared()` in the web process: `Media::open` loads the whole table into the `known` map (mod.rs:163-185) and would duplicate up to ~300 MB; use a read-only `rusqlite` open per request or a kept reader like `db::with_reader` (db.rs:27-47, 86-100). The file may not exist (no provider on, mod.rs:157-159) or the row may not exist (title never visited) — render nothing then. Best placed in a lazily loaded panel like `details_panel` (web.rs:62, 1657-1663, slot at 1190) or appended to `details_html` (1679-1715), so a cold `atlas.media.db` open never blocks the page. Needs a new route in `get()` (web.rs:48-72, `signed_in` guard pattern 62/66-68).
3. **Parsed title without a schema change**: recompute at render with `media::media_target(category, rule, name)` (steps.rs:113-126, exported mod.rs:34) — pure and ~10-40 µs (08-media-ids.md:961). `release_page` already has the category (web.rs:1000); the rule string is only in step facts, so call `category::classify_release(r, &options)` as `target_of` does (steps.rs:136-139) or accept `media_target(category, "", name)`. This also exposes `pack`, the alias canonicalization and the two-parser disagreement with `release_tags` (web.rs:1058-1060).
4. **New `media_release` columns** (e.g. `title TEXT`, `pack`, `id_source` bits): `MEDIA_RELEASE.added` (steps.rs:76; schema.rs:16-17, 102-110 adds them on open, no rewrite, no index) + UPSERT (steps.rs:80-92) + late params (197-213) + the `DO UPDATE SET` list (91-92). Backfill = re-run `MediaParse` over all releases: `atlas --reprocess --step media_parse --apply` (reprocess.rs:25-40, 74-99; README.md:757 pattern) or give `MediaParse` a `config_stamp` (it has none; `Step::config_stamp` default 0 step.rs:58-62) so `seed_shard` (engine.rs:1103-1112) walks every shard newest-first on the next start. DB lane only, no network: 118M rows ≈ 16.4 h at 2,000 rows/s (08-media-ids.md:961). The UPSERT keeps ids on a same-title rewrite (84-89), so this is safe for existing rows. Note `sample_row` (schema.rs:135-159) fills new columns by declared type in the compaction survival test.
5. **New provider facts** (score, per-id origin, candidate year/country): widen `Answer::Found` (resolve.rs:54-57), the merge (153-161), `TitleRow`/COLUMNS/`row_of`/`put` (store.rs:45-120) and the SCHEMA (9-25). `media_titles` has NO `added` mechanism — SCHEMA is `create table if not exists` (store.rs:40), so a new column needs the `pragma_table_info` + `alter table` pattern of details.rs:105-109. Existing rows only gain the value on a re-ask: positive results never expire (decision 12, 08-media-ids.md:1098), so a provenance backfill means either a new stamp bit on `MediaLookup` (steps.rs:270-272; `wants` dedups to one request per title, mod.rs:265-307, but `resolve_until` asks only providers whose bit is unset, resolve.rs:138-143 — a title with `checked_mask` full is NOT re-asked; a real re-ask needs `checked_mask` reset or a row delete + restart per decision 33, 08-media-ids.md:1119, README.md:757).
6. **Details**: `Details` struct (details.rs:73-94), SCHEMA (43-70), `read`/`write` (113-174), `from_tmdb` (207-236), `from_tvdb` (240-264), `details_html` (web.rs:1679-1715); add a column with the `backdrop` ALTER pattern (105-109). Existing rows refresh only when older than `KEEP_FOR` 30 days (452-458) or via the backfill thread, which skips kept titles (632-636, 647-649); to backfill a new field, add a `version`/`fields` column and treat an old version as stale in `for_title`/`title_is_kept`, or temporarily lower `KEEP_FOR`.
7. **API**: `MediaAttrs::xml` (query.rs:376-428) for new attrs; `IdSet::caps` (40-57) and `id_params` (61-81) for new search parameters; `release_attrs` (api.rs:285-310) for name-derived ones.
8. **Tests and conventions**: parse/score/xml tables (parse.rs:271-370, score.rs:135-157, query.rs:491-521); integration tests tests/media_*.rs use a loopback `FixtureServer` (tests/common/http.rs) and never a real provider; `tests/media_stage.rs` names (e.g. `a_daily_show_is_stored_by_date` 200, `a_rename_gets_a_new_title` 685) show the harness; CI runs fmt, clippy -D warnings, tests (web-ui.md:160).

## 8. Backfill options and cost for THIS area

| Lever | Mechanism | Scope | Cost / pace |
|---|---|---|---|
| Render-only exposure (§7.1-7.3) | none | every release instantly | one PK read per page (already done at web.rs:1002) |
| `MediaParse` re-walk (§7.4) | `--reprocess --step media_parse --apply` (reprocess.rs:25-40) or a new `config_stamp` on MediaParse (engine.rs:1103-1112) | every TV/movie release | DB lane, `backlog_per_second` (2000) / 8 shards; ~16 h for 118M rows (08-media-ids.md:961); writer time ~40 µs per target row |
| Provider re-lookup (§7.5) | new bit in `provider_bits` (steps.rs:270-272) → `steps` walk; `wants` keeps it to one request per title (mod.rs:265-307) | titles with that provider's bit unset | per-title request pacing: TVmaze 90/min, TMDB 240/min (2 calls per hit), Trakt 120/min, TVDB 120/min defaults (config.rs:237-241); 1M titles ≈ 3-6 days (08-media-ids.md:947-955) |
| Fan-out | automatic after a title's ids change (resolve.rs:174-176; fanout.rs) | all rows of the title, all shards | `CHUNK` 1,000 rows per writer job (fanout.rs:17), ~30 ms per chunk, second pass +60 s (16); crash-safe via `fanout_done = 0` (store.rs:131-133) |
| Details backfill thread | `start_backfill` (details.rs:673-686, bg_indexer.rs:1172); pass every 6 h (39), 30 s after start (41) | every title with a tmdb/imdb/tvdb id, newest 100,000 per shard (595, 598-624) | `details_per_minute` 120 (config.rs:236; 0 off, ≤600, config.rs:272): 1,000 titles ≈ 8 min, 100,000 ≈ 14 h; each title = 1-2 TMDB calls (+ `/find` without a tmdb id, 368-382) + OMDb (30/min default, config.rs:239 — the binding limit when on: a `Paused` OMDb makes the row stale again within the hour, 290-293, 421) + Trakt + one image fetch (562-592) |
| Correction of a wrong match | delete the `media_titles` row, restart, `--reprocess --step media_lookup` (README.md:757; decision 33) | one title | manual; no UI action exists |

## 9. Risks

1. **Provider rate limits and pauses** (config.rs:237-242 defaults; clamps 287-310: TVmaze ≤120, TMDB ≤3,000, OMDb ≤600, Trakt ≤200, TVDB ≤600, -1 unlimited). Any backfill that re-asks providers competes with new titles for the same buckets (http.rs:19-45, one token held); 429/401/5xx pause a provider for up to an hour (http.rs:12, 197-211) and park every release of the lane on `wait_until` (steps.rs:302).
2. **Negative cache**: `negative_ttl_days` 30 doubling to 180 (config.rs:234; resolve.rs:106, 181-183), re-asked only on a visit (`wants`, mod.rs:276-281): old "not found" titles will stay id-less (and provenance-less) for weeks after a backfill starts; a provenance panel will show `state = 2` honestly, which is a feature.
3. **`max_titles` 10M in memory** (config.rs:235; mod.rs:292-297; ~300 MB, 08-media-ids.md:557). A web-process `Media::shared()` would load a second copy (mod.rs:163-185) — read `atlas.media.db` directly instead (§7.2).
4. **Three readings of "quality"**: `media_release.quality` (name, parse.rs:89-113), `release_tags` resolution/quality tags (name, web.rs:720-732) and the probe's measured width/height (media.md risk 12). Showing more than one invites contradictions on mislabeled releases; pick one source of truth per fact and label name-derived ones as such.
5. **Two title parsers**: `media::parse` (with `data/tv_title_aliases.tsv`, parse.rs:11, 136-137) vs `release_tags::parse` (web.rs:1053-1060). Showing the media title next to the existing "Title" fact will expose disagreements (`Daily Show` → `The Daily Show`, country suffixes, acronyms); decide which is canonical before rendering both.
6. **`imdb_from_nfo` is overloaded**: set by `MediaNfo` (NFO text) and `OpenSubtitlesImdb` (moviehash) alike (steps.rs:98-99; probe/opensubtitles.rs:55-56). A UI line "IMDb from NFO" would be wrong for hash matches; consult `release_opensubtitles` or split the flag (an `added` column) first.
7. **Positive ids never expire** (decision 12) and corrections are manual (decision 33; README.md:757). Making provenance visible makes wrong matches visible with no UI path to fix them; the `stale_ids` + `fan_out_again` path (steps.rs:221-230, 293-299) only runs after a row delete and a restart (`a_deleted_cache_row_needs_a_restart`, tests/media_stage.rs:715).
8. **Request-thread fetches**: `details_panel` → `for_title` calls TMDB/OMDb/Trakt/TVDB synchronously when the cache is cold (details.rs:441-483; web.rs:1657-1663); each `details::open()` also re-runs the schema and a `pragma_table_info` (101-111). Any new per-page provider call adds latency; keep new facts to what the backfill thread fills.
9. **Schema**: `media_release` already carries 7 indexes (steps.rs:45-74); new columns via `added` are free, but a new index on a populated shard is against the house rule (schema.rs:112-113). `media_titles` has no `added` mechanism (store.rs:9-25, 40) — a column there needs an ALTER guard (details.rs:105-109 pattern) and is a cache that can be deleted at any time (README.md:757), so nothing the UI shows may REQUIRE it.
10. **Default install has TVmaze on** (config.rs:237; config.example.toml:95-96) — `atlas.media.db`, the fan-out thread (bg_indexer.rs:880-883) and net-lane lookups run by default; docs/port says otherwise (08-media-ids.md:556). Plans that assume "no provider by default" are wrong.
11. **Daily shows render as `S2024E315`** (web.rs:737 with season = year, episode = MMDD, parse.rs:172-174); fixing `media_label` changes a tag that tests pin (web.rs:2204-2208) and results rows.
12. **Shared `MediaAttrs`**: it feeds the API `xml` (query.rs:378) — a field added for the UI must not change the attribute set Sonarr/Radarr parse (`imdb` must stay 7 digits, 08-media-ids.md:56).

## 10. Conflicts with open PRs and branches (diffed against each merge base, restricted to src/media, src/web.rs, src/api.rs, src/config.rs, src/postprocess/{schema,step}.rs)

| Head | Touches in this area | Collision with a title-resolution change |
|---|---|---|
| `pr/48` 06847b2 (5 commits ff8e66b..06847b2, "Show cover art, attachments, every stream field and the NZB's files on the release page"; merge-base a29ee52) | src/media/details.rs: ONLY `fn trim_posters` → `pub(crate)` (688); src/postprocess/schema.rs +1 (a new table); src/web.rs +143/-7: route at 61, `release_html` hunks right AFTER the "Links" fact (adds `let nzb_files = nzb_files_html(contents)` at 1092-1096), "Files inside" heading (1106-1111), `probe_html` signature gains a `cover` arg (1115-1121), page template gains `{nzb_files}` (1141-1147), new `nzb_files_html` after `release_html` (1154-1188), `probe_html` hunks 1228-1480, new routes/functions after `poster` (1685-1770). Does not touch `media_label`, `id_links`, `details_panel`, `details_html`, `MediaAttrs`. | Textual: any fact inserted at web.rs:1139-1142 or any change to the page template (1183-1200) conflicts; semantic: none |
| `pr/51` 7562a82, `pr/54` 1e1949f (merge-base ea5a7fc) | src/media/details.rs: the same one-line `trim_posters` visibility; schema.rs +1 (`release_stills`). Nothing in web.rs/api.rs/config.rs. | none |
| `origin/feat/release-stills-strip` 2c5a0a4 (merge-base ea5a7fc; the #51/#54 follow-up per coordination.md:8) | details.rs same line; schema.rs +1; web.rs +67/-4: route at 61, `release_html` 1110-1121 (`conn` reused, `stills_html` call) and the template gains `{stills}` before `{copies}` (1139-1147), new `stills_html` inserted right after `details_html` (1661-1712), tests 1816-1890 | Textual with the page template and with anything appended after `details_html` (1715) |
| `pr/57` e1ad6c3 "Sort results instantly from a window kept in memory" (merge-base 55d2e51 = main) | src/web.rs +447/-51 all in the Search/results code (hunks 5-730): moves the `attrs_of` + `kept_for` calls into `Search::scores()` and a new `rows_html(found, split)` (diff lines 118-128, 318-328, 572-580); api.rs +3 (`keep_sort_windows_warm`). Release page, `media_label`, `id_links`, details untouched. | Only if the feature also changes results rows (`row`, `RowExtras` web.rs:684-689, 744-797) |
| `pr/58` 44c336c, `pr/55` 6a0ff6d, `pr/49`, `pr/50` | nothing in these files | none |
| `pr/21` cec2f5e | no merge base with main (unrelated history); ignore | — |
| #64 (coordination.md:34-37, not in this clone) | `release_html` gains a LAST parameter `recovery`, a `{warning}` slot before the title-details div (web.rs:1190), `release_page` uses `whole_post_with`, new StepId(7) at rank 150 | Any change to `release_html`'s signature or template; a new step must not take id 7 (nor 32/33/34 steps.rs:161, 252, 311; ranks 500/505/510/520 taken) |
| Finish web UI thread, twin grouping (coordination.md:13; media.md §7.4) | `copy_card`/`copies_html` (web.rs:1463-1572) which duplicate the "Links" rendering (1534-1536) and score tags (1483-1485) | A new per-release media fact shown in `release_html` needs the same in `copy_card` or an agreed rule that copies show name-derived facts only |

Stream-details branch (coordination.md:7, stacked on #48, not pushed): touches `probe_html` and `details_html` (`runtime_note` = runtime vs TMDB). A provenance block appended inside `details_html` (web.rs:1679-1715) would collide with it; prefer a separate panel/route.

## 11. Open questions

1. Should the parsed title (media::parse, aliases applied) be shown beside `release_tags`' "Title" when they differ, and which one is canonical for search links?
2. Is a provenance panel wanted when the only provider on is TVmaze (the default) and the details panel is empty for lack of a TMDB key? Today such a page shows just `S01E02` + Links.
3. Split `imdb_from_nfo` into a source (nfo / moviehash / provider) before printing "IMDb from NFO"?
4. Fix `media_label` for dailies (`2024-03-15`) and ranges (`S01E02-E04`) as part of this feature (changes a pinned tag, web.rs:2204-2208)?
5. New `media_release` columns (parsed title text, pack) with a cheap DB-only re-walk, or re-parse at render time (§7.3)?
6. How does the web/API process read `media_titles`: a per-request read-only open of `atlas.media.db`, or attach it to the kept readers (db.rs:27-47)? The port doc rejected a read-time join for the API because the cache can be deleted (decision 4, 08-media-ids.md:1090) — for a UI provenance line that degrades to "unknown" this is acceptable, but it must be stated.
7. The #48 branch name: coordination.md:6 says `feat/release-nzb-files`, media.md/probe.md/tests-and-conventions.md say `feat/release-page-media-cover`; the local remote is `pr/48` at 06847b2 whose first commit is ff8e66b "Show cover art, attachments, every stream field and the NZB's files on the release page" — confirm with the owner before a plan cites a branch.

## 12. Contradictions from other readers that touch these files, resolved from the code

- **Status vocabulary for `nzb.external_oracles`** (ingest unclear / api-cli stored_not_shown (predb nuked) / storage absent): for the provider half the ANSWER (ids, ratings, TMDB facts) is shown on the release page (web.rs:1139-1142, 1657-1715) and the PROVENANCE is stored_not_shown (store.rs:9-25). Proposed rule for the merge: a composite catalog item takes the status of its primary value; aggregates (`titles` count), metrics (`atlas_media_requests_total`) and API-only emissions (`tvairdate`, `rageid`) do not make a value "shown"; per-title provenance rows are "stored_not_shown" and are listed separately. Under that rule api-cli.md's `tvairdate`/`rageid` → stored_not_shown (api-cli.md:296-297, 340) and web-ui.md:229 agree with this file.
- **`overview_row.subject`** (probe.md shown vs others stored_not_shown): web.rs:1087-1093 prints `n.posted` from `release_info::Names`, not `files.subject`; nothing in web.rs reads a subject. stored_not_shown is right (web-ui.md, storage.md, ingest-postprocess.md, api-cli.md); probe.md needs the fix.
- **Open-PR impact (web-ui.md §9 UNVERIFIED)**: confirmed by diff here: #48 touches src/media/details.rs ONLY at `trim_posters` visibility (688), not `details_html`; #51/#54 touch details.rs the same single line and schema.rs (+1) and nothing else in this area; neither changes `MediaAttrs`, `config_stamp` of the media steps, or `provider_bits`. web-ui.md §9 is superseded (media.md §7, probe.md §10 and tests-and-conventions.md §10 were right).
- **`found_title` / provenance columns**: web-ui.md:181 ("`media_titles` not read by web") and storage.md:85 are confirmed; this file adds the per-column statuses (§6.3).
- **Line drift**: media.md and storage.md line numbers for src/media/* and the web.rs media functions (735-742, 1139-1142, 1657-1715, 1790-1811) match main at 55d2e51; no correction needed for this area.
