# Atlas map: API and CLI

Repo `/home/claude/appz4fun/atlas`, branch main at 55d2e51. Read-only pass; nothing built or run. All paths below are
under `src/` unless they start with `README`, `docs/`, `config.example.toml` or `tests/`.

Feature under study: every metadata field in the catalog must become visible in the web UI on every release page,
including existing releases (backfill). This file covers the Newznab API, the command line, config, the SABnzbd handoff,
the TUI dashboards and the metrics header, and says where those touch the web UI and the stored data.

---

## 1. Process layout: one process, one port, API and web UI

- `api::start(&mut Config)` (api.rs:486-525) starts a `tiny_http` server with `WORKERS = 4` threads (api.rs:18) on
  `config.api_host` / `config.api_port()` (config.rs:722-723, defaults `DEFAULT_API_HOST "127.0.0.1"` config.rs:14,
  `DEFAULT_API_PORT 9090` config.rs:13; `config.example.toml` ships `api_host = "0.0.0.0"`). The key comes from
  `config::get_api_key` (config.rs:1182-1202: generated as `token_urlsafe(32)` and saved when missing).
- Every request goes through `respond()` (api.rs:422-483): the `Host` header (or the configured host) becomes `base`,
  the `Cookie` header is passed along, **GET/HEAD are offered to `crate::web::get(path, query, cookies, base, key)` first
  and fall through to `api::handle` with `.unwrap_or_else(...)`**; POST is accepted only for `web::takes_post` paths
  (web.rs:93; `/login`, `/logout`, web.rs:75-90), body capped at `MAX_FORM_BODY = 4096` (api.rs:420). Metrics
  `API_REQUESTS` / `API_BYTES` are labelled by `endpoint_of()` (api.rs:402-417), a closed list so clients cannot mint
  series. Default `Content-Type` is `text/html`.
- `start_warming()` (api.rs:528-542) runs `db::warm_search_tables` every `WARM_EVERY = 3600 s` (api.rs:20). PR #57 adds
  three lines here spawning `crate::web::keep_sort_windows_warm(...)`.
- `api::stop()` (api.rs:544-554). The TUI menu starts the API at app.rs:801 (`api::start(&mut config)`) and metrics at
  app.rs:802 (`metrics::start(&config, Role::Api)`); "Change api port" in settings restarts it (app.rs:619-621).
- The API process reads config without the keyring: `config::media_config()` (config.rs:317-319) and
  `config::body_budget_set()` (config.rs:322-324) parse the TOML file directly; `load_settings()` (config.rs:1087-1089)
  is the same for the CLI (purge, reprocess). `load_config()` (config.rs:1041-1083) is the full one: `ATLAS_NNTP_HOST/
  USER/PASS/PORT/CONNECTIONS` env server at priority `i64::MIN`, `ATLAS_INDEX_MODE`, `ATLAS_API_PORT`, `ATLAS_API_HOST`,
  passwords from the keyring (`config::keyring`, config.rs:1237-1272, `ATLAS_NO_KEYRING` to skip).

## 2. The Newznab API, endpoint by endpoint

`handle(path, query, base, api_key)` (api.rs:131-167). Only `/api` is served; anything else is a 404 from the API side
(the web UI took its own paths first).

### Errors
`ApiResponse::error()` (api.rs:69-79): Newznab errors go out with **HTTP 200** and `<error code=... description=.../>`;
`failed = code >= 900`. Wrong or missing `apikey` on anything but caps: HTTP 401, `<error code="100" description="Invalid
API Key"/>`. Missing `t`: error 200 ("Missing parameter"). Unknown `t`: error 203 ("Function not available"), so
`t=details`, `t=getnfo`, `t=comments`, `t=register`, `t=cart` all answer 203 today.

### `t=caps` (no key needed)
`caps_body(base, &crate::media::query::refreshed_id_params())` (api.rs:203-209) substitutes into the `CAPS` const
(api.rs:22-36): `<limits max="100" default="100"/>`, `<retention days="0"/>`, `<searching>` with `search`/`tv-search`/
`movie-search`/`audio-search`/`book-search`, each `supportedParams="q,cat,limit,offset"`, tv-search adding `season,ep`
plus a `<!--tv-ids-->` marker and movie-search a `<!--movie-ids-->` marker that `IdSet::caps` (media/query.rs:41-57)
fills with the id params whose provider is configured (`id_params(cfg, body_budget_set)` media/query.rs:61-81, cached
30 s media/query.rs:86-96, forced refresh media/query.rs:99-103). Categories come from `category::` (placeholder
substituted in caps_body). There is no `<server>` metadata worth noting and no `<registration>`.

### `t=search`, `t=tvsearch`, `t=movie`, `t=music` / `t=audio`, `t=book`
- `search_term(kind, args)` (api.rs:174-200) builds the FTS term from `q` plus `season`/`ep` → `S01E02`, `S01`, or a daily
  `Y MM DD`. When any of `ID_PARAMS` (api.rs:170: `tvdbid, rid, tvrageid, tvmazeid, imdbid, tmdbid, traktid, doubanid`) is
  present and that id is switched on, `id_search` (api.rs:211-243) resolves through `media::query::from_args`
  (media/query.rs:158-182, `imdbid` normalised at :141, `rid`/`tvrageid` folded together at :175, `season`/`ep` parsed
  at :179) and reads `media_release`; ids present but all switched off → an empty `rss`. `doubanid` is accepted and never
  answered (docs/port/08-media-ids.md:19).
- `find_releases(q, filter, offset, limit)` (api.rs:245-257, `pub(crate)`, **shared with web.rs**) dispatches to
  `search::all_releases` (search.rs:528), `search_all_releases` (:267), `newest_in_categories` (:346) and
  `search_in_categories` (:487). All of those filter with `VISIBLE_RELEASE` (search.rs:37-40: a `display_name`, or not
  obfuscated and the name does not look random; and `not atlas_blocked(group)`).
- Per hit, `rss()` (api.rs:313-366) is called with `passwords` from `archive::store::passwords(&ids)` (api.rs:235, 274),
  `categories` from `category::categories_of`, `media` from `media::query::attrs_of(&ids)` (api.rs:234, 273) and the
  `on` id set; inside, `copies = duplicates::counts(&ids)` (api.rs:324).

### What one `<item>` carries (api.rs:326-345)
| element / attr | value | source |
|---|---|---|
| `<title>` | `ReleaseRow.name` = `coalesce(display_name, name)` (search.rs:33-34) | releases |
| `<guid isPermaLink="false">` | release id | releases.id |
| `<link>`, `<enclosure url type="application/x-nzb" length=size>` | `{base}/api?t=get&id={id}&apikey={key}` | — |
| `<size>` | `releases.size` (0 when null) | releases |
| `<pubDate>` | `dates::pub_date(posted_date)` RFC 2822, naive stored value treated as UTC (dates.rs:146-163) | releases.posted_date |
| `newznab:attr category` | `category::categories_of`, default `UNCATEGORIZED` 8000 | category step |
| media attrs via `MediaAttrs::xml(on)` (media/query.rs:378-428) | `season`; `episode` (for dailies `MM/DD` plus `tvairdate YYYY-MM-DD`, :386-387); `year` (:392); `tvdbid`, `tvmazeid`, `traktid`, `tmdbid`; `rageid` and `tvrageid` (:417-418); `imdb` and `imdbid` as 7 digits (:424-425); each id only when its param is on | media_release (media/steps.rs:25-53) via `attrs_with` (media/query.rs:331-370) |
| `release_attrs` (api.rs:285-310) | `poster`, `group`, then from `release_tags::parse(&r.name)` (api.rs:286, derived at request time, not stored): `resolution`, `video` (codec), `audio`, `hdr`, `source` (quality), `language`, `team` (release group); `twins`, `alts` from `CopyCount` | releases + name parse + release_fingerprint |
| `newznab:attr password` | `Password::code()` 0/1/2 (archive/mod.rs:98-104), **only when known** (api.rs:330-332) | archive_release / release_nfo_password via `archive::store::passwords` (archive/store.rs:146-148) |

Footer: `<newznab:response offset="{offset}" total="{offset+len}"/>` (api.rs:357) — **not a real total**, no count query
runs. Attribute values are escaped with `escape()` plus `"` → `&quot;` (api.rs:290-293).

Not emitted (versus other indexers): `size`, `files`, `grabs`, `comments`, `usenetdate`, `nfo`, `guid`, `info` as
attrs (size/guid exist as RSS elements; nothing counts grabs anywhere: `grep grab src/` finds only a comment in
indexer.rs:1577); no `t=details`, `t=getnfo`, `t=comments`, `t=register`, `t=cart`. docs/TODO.md:14 records that
`t=get&file=N` is unadvertised and that results never list a release's files ("a files attribute on results, or a
`t=files` listing").

### `t=get`
`get_nzb(args)` (api.rs:368-394): `id` → `search::get_release` (blocked groups filtered, search.rs:552-560);
optional **non-spec `file=N`** (api.rs:375-377) → `nzb::build_nzb_file`, else `nzb::build_nzb`. Response:
`content_type: Some("application/x-nzb")`, `headers = [("Content-Disposition", "attachment; filename=\"...\"")]`
(api.rs:386-393). **No `X-DNZB-*` headers of any kind.** Filename from `nzb::nzb_filename(name, Some(id))`
(nzb.rs:12-22: unsafe chars → `_`, `{safe}.{id}.nzb`).

### Metric labels
`endpoint_of` (api.rs:402-417) maps `t=` values and web paths to a fixed `Endpoint` enum; a new `t=` without a label is
counted under the catch-all. `names.rs:123-124` `API_REQUESTS "atlas_api_requests_total"`, `API_BYTES
"atlas_api_response_bytes_total"`.

## 3. NZB generation (nzb.rs)

`render_nzb_in(release, articles, groups)` (nzb.rs:66-113):
- `<?xml version="1.0" encoding="utf-8"?>` (lower-case utf-8, nzb.rs:68), DOCTYPE NZB 1.1 (nzb.rs:69-71),
  `<nzb xmlns="http://www.newzbin.com/DTD/2003/nzb">` (nzb.rs:72).
- `<head><meta type="category">{group_name}</meta></head>` (nzb.rs:73) — **the only meta**. No `title`, `password`,
  `name`, `tag`, ids, although `archive_release.password`, `release_nfo_password`, `media_release.*_id` and
  `releases.display_name` exist and go out as newznab attrs.
- One `<file poster="{first.poster}" date="{article_timestamp(first.posted_date)}" subject="{first.subject}">` per
  distinct `filename` (nzb.rs:76-89). `poster` and `posted_date` on `ArticleRow` are **the release's** values: the
  article query joins `r.poster, r.posted_date` (store.rs:1954-1957), so every `<file>` carries one From and one date.
  `date` uses `dates::article_timestamp` → `posted_timestamp` which treats a naive stored date as **Local**
  (dates.rs:167-185), while `pub_date` treats the same value as **UTC** (dates.rs:146-163): the two outputs disagree by
  the host zone.
- `<groups>` lists every group in `groups` (nzb.rs:90-94). `inventory()` (nzb.rs:119-134): for an incomplete release
  that `release_info::split_post_with` recognises as a split post, articles of all its ids and **all its groups** are
  used; otherwise `get_articles(id)` and the one group. So the catalog's "always one `<group>`" is only true for
  non-split posts.
- `<segment bytes="{bytes}" number="{part}">{message-id without <>}</segment>` (nzb.rs:97-106); `number` prints
  `None` when the part is unknown (nzb.rs:103). `bytes` is the stored XOVER `:bytes` (segments.bytes, store.rs:181-189),
  i.e. the newznab-family semantics in the catalog's writer-fingerprint sense.
- `build_nzb` (nzb.rs:115-117, metric `nzb.build`), `build_nzb_file(id, n)` (nzb.rs:147-159: file N from 1, in
  filename order via `chunk_by`), `generate_nzb(id, dir)` (nzb.rs:161-186, atomic write, used by the TUI and SAB handoff).
- Web-only variant: `web::filled_nzb` (web.rs:1204-1225) → `duplicates::filled_nzb` (duplicates.rs:527-540) fills
  gaps from a twin (`can_fill` duplicates.rs:522-524); reachable as `/ui/nzb?id=&fill=1` (web.rs:64); plain `/ui/nzb`
  calls the same `api::get_nzb` (web.rs:65), so `file=N` works there too but no UI link emits it.

## 4. What the web UI and the API share

- `web.rs:12`: `use crate::api::{ApiResponse, escape, find_releases, get_nzb, parse_query};` — the UI reuses the
  response type, the XML/HTML escaper, the result lookup and the NZB endpoint.
- Both consume `search::ReleaseRow {id, name, group_name, poster, posted_date, size, complete, parts}` (search.rs:9-18)
  selected by `COLUMNS` (search.rs:33-34) and `ArticleRow {message_id, filename, part, total_parts, bytes, subject,
  poster, posted_date}` (search.rs:22-31). Adding a column to the row type means touching `COLUMNS` and every query
  that builds a `ReleaseRow`.
- Both call `category::categories_of`, `archive::store::passwords`, `media::query::attrs_of`, `duplicates::counts`.
- Only the API parses the name with `release_tags::parse` (api.rs:286); the UI shows those as facts from its own code.
- Only the UI reads `release_probe` (probe/mod.rs:23-47), `archive_inner_files` (archive/store.rs:18-39),
  `release_info::Contents`/`Names` (release_info.rs:16-21, 39-60), `atlas.details.db` (`media::details::Details`
  media/details.rs:74-94, `for_title` :441, `kept_for` :486, `poster` :551, `backdrop` :557) and `media::resolve::Ids`
  (media/resolve.rs:16-23) through `release_html` (web.rs:1028-1201), `details_html` (web.rs:1679-1715) and `id_links`
  (web.rs:1790-1811: IMDb, TMDB, TheTVDB, TVmaze, Trakt — **no TVRage link**, no air date anywhere: `grep air_date
  src/web.rs` is empty).
- The UI row (web.rs:744-797) shows TWIN/ALT, GROUPS, PASSWORD, INCOMPLETE tags, `media_label` (S01E02 or year,
  web.rs:735-742), category, size (`ui::fmt_size` ui.rs:513), posted (`ui::fmt_date` cuts to 10 chars, ui.rs:531-533),
  group link and an NZB button `/ui/nzb?id=`; the release page adds the fill button (web.rs:1173-1180), "Download NZB"
  (web.rs:1189) and `#title-details` hx-get.

## 5. Command line

`main.rs:9-54`: `--version`, `--help` (text at main.rs:15-19), then in order `--convert`, `--compact`, `--purge`,
`--reprocess`, `--predb-import FILE`, `--reflag-obfuscated`, `--unname`, `--sealed-log-move`, `--search-indexes`,
`--selftest`, `--bg-indexer`, else the TUI menu. Every destructive command is a dry run unless `--apply`.

### `atlas --purge [classes] [--below BYTES] [--sample N] [--apply]` (postprocess/purge.rs)
- Doc line purge.rs:1-2: deletes "what no step can help". `Class` bits (purge.rs:27-34): Fragments 1, Blocked 2,
  TooSmall 4, ImageOnly 8, Rejected 16, Dead 32. `parse` (purge.rs:59-87): **default class is Fragments only**;
  `--below` defaults to `postprocess.body_min_size_bytes`.
- `classify` (purge.rs:90-129), verified text:
  - `Blocked` (group in the block list, `filters::group_blocked`) goes at once, "never indexed or served again anyway".
  - Everything else: `if now - s.last_touch < rules.config.dead_after_secs { return None; }` — nothing is removed while
    an article could still arrive (`files.touched_at`, default 172 800 s = 2 days, config.rs:636).
  - Then `Fragments` via `filters::stored_fragment` (filters.rs:624-634: one part, one file, subject claims a middle
    part, part/expected agree, random-looking name), `TooSmall` (`size < below`), `ImageOnly` (all files images),
    `Rejected` (no parts, or the rules' verdict is `Par2Only | TooFewFiles | Empty`, rules.rs:22-29, 158-184),
    `Dead` (`rules::permille(row, s) < min_completion_percent * 10`, rules.rs:187-196; default 95 %).
- **The never-purged rule**: the post-processing stage itself deletes nothing (postprocess/mod.rs:1-4); fragments and
  partial (incomplete) releases stay in the shards until a human runs `atlas --purge --apply`; even then an incomplete
  release goes only under `--dead` and only once it is below `min_completion_percent` and untouched for
  `dead_after_secs`; a rejected row is re-judged when late articles arrive (rules.rs:156-157). The only other deleter is
  the TUI "Purge broken releases" → `db::purge_broken` (app.rs:563; README:528-535: one transaction, refuses past one
  million incomplete releases in a shard, `--dead` is the batched way).
- Mechanics: resumable through shard `meta` keys `purge_victim/purge_top/purge_classes/purge_below/purge_after`
  (purge.rs:342-425); `delete_huge` for oversized rows (purge.rs:291-327); `apply` (purge.rs:431-475) walks
  `AT_ONCE = 4` shards, stops on SIGINT/SIGTERM, env knobs `ATLAS_PURGE_STOP_AFTER/CHUNK/CHECK_EVERY/WAL_LIMIT`;
  `main` (purge.rs:478-551): `--apply` needs the indexer stopped (purge.rs:516), takes `compact::Lock`,
  `refuse_unresolved_backups`, refuses when `convert::needed`. README:491-513 has the class/age table.

### `atlas --reprocess [--step NAME|all]... [--group NAME]... [--failed] [--apply]` (postprocess/reprocess.rs)
- Only **queues**: `apply(tx, redo_mask, group, now)` (reprocess.rs:25-40) inserts a `pp_walks` row
  (`kind='reprocess'`, `top_id = before_id = max(id)+1`, `redo_mask`, `group_name`), deduplicated; the indexer's stage
  drains it. Opting naming in also frees NFO (`with_what_opting_in_frees`, reprocess.rs:19-21) and writes the meta
  `naming_rename_named[:group]` (reprocess.rs:14, 28-31). `--failed` → `retry_failed` (reprocess.rs:44-65, 1000 per
  loop). `targets` (reprocess.rs:102-108) picks shards by `store::shard_of(group)`; `dry_run` (:110-135) counts;
  `apply_all` (:137-162) holds off compaction (`compact::hold_off_compaction`) and runs `migrate_shard` first. Runs
  beside a live indexer.

### `atlas --compact`, `atlas --convert`
`run_compact` (main.rs:86-107) and `run_convert` (main.rs:60-83) refuse while `procs::indexer_alive()`; compact writes
`meta last_compact` (main.rs:97-98).

### Others
`--reflag-obfuscated` (naming/reflag.rs; README:491-513: 2-10 min and ~12 GB WAL over 118 M releases), `--unname`
(naming/unname.rs: `name_source in (3,5)` → cleared; README: 15 M names, 5-10 min, 30-60 GB WAL; stops if a reader
holds the WAL past 4 GiB), `--predb-import FILE` (predb/file_import.rs, bounded), `--sealed-log-move`
(sealed_store::run_move sealed_store.rs:207-223, needs the indexer stopped), `--search-indexes` (field_search.rs:1-90:
builds `idx_release_poster`, `idx_release_size`; `Fields {group, poster, email, size, team, crc, agent}`;
`SCAN_LIMIT 200_000`), `--selftest` (app.rs:887-933), `--bg-indexer` (headless loop the menu spawns).

## 6. Config (config.rs; TOML `config.toml`, legacy JSON auto-migrated; `write_toml_at` 0600 config.rs:1007-1035)

Top level `Config` (config.rs:136-163): `servers: Vec<UsenetServer>` (config.rs:39-57: host, username, password,
port, ssl, connections, priority, compress, index, key), `index_mode`, `api_port: Option<u16>`, `api_host`,
`api_key`, plus sections. Constants config.rs:12-35: `DEFAULT_CONNECTIONS 10`, `DEFAULT_PRIORITY 99`,
`DEFAULT_BATCH_SIZE 500_000`, `CONNECTIONS_PER_GROUP 5`, `MAX_PARALLEL_GROUPS 256`, `DEFAULT_REQUEST_SIZE 10_000`,
`DEFAULT_MAX_UNSAVED_HEADERS 500_000`, `SPLIT_MIN_BACKLOG 10_000_000`.

| section | fields (default; clamp) | lines |
|---|---|---|
| `[postprocess]` | `workers 2`, `settle_secs 7200 (>=1)`, `complete_settle_secs 900 (>=1)`, `min_size_bytes 0`, `min_files 1`, `reject_par2_only true`, `group_rules`, `backlog_per_second 2000`, `body_fetches_per_minute 600` (0 = no body fetching; `body_budget_set()` keys off it), `body_connections 2`, `body_min_size_bytes 1 MiB`, `body_max_size_bytes 100 GiB`, `body_max_bytes 16 MiB`, `bodies_per_release 8`, `min_completion_percent 95 (<=100)`, `dead_after_secs 172_800`, `max_attempts 5` | config.rs:596-691 (fields :599-613, defaults :622-636, parse :651-666) |
| `[archive]` | `enabled`, `max_bodies_per_release 4 [1..16]`, `probe_files 2 [0..8]`, `probes_per_minute 60 [12..600]`, `max_names_mb 16384` | config.rs:546-584 (fields :548-550, defaults :558-560, clamps :578-580) |
| `[media]` | `parse true`, `nfo true`, `negative_ttl_days 30`, `max_titles 10 M`, `details_per_minute 120 [0..600]`, providers `tvmaze` (enabled, 90 rpm), `tmdb 240`, `omdb 30`, `trakt 120`, `tvdb 120`, `opensubtitles 60`; `-1` = no limit | config.rs:205-245, clamp :271-272, :276-277; file-only reader `media_config()` :317-319 |
| `[category]` | `enabled`, `foreign`, `web_dl` | config.rs:328-358 |
| `[predb]` | feeds, import limits | config.rs:362-429 |
| `[sealed_log]` | path, move settings | config.rs:433-468 |
| `[metrics]` | `enabled false`, `host`, `indexer_port 9092`, `api_port 9093`, `catalog_every_secs 1800 (>=60)` | config.rs:472-515 (:478, :488, :510-511) |
| `[naming]` | | config.rs:519-542 |

Budgets a new field would run into: `backlog_per_second` (walk speed over existing releases, shared by the 8 shards),
`body_fetches_per_minute` / `bodies_per_release` / `body_max_bytes` (anything that needs article bodies),
`probes_per_minute` / `probe_files` / `max_bodies_per_release` (archive/probe), `details_per_minute` and the per-provider
rpm (external lookups), `workers`. `config.example.toml` (123 lines) lists every key with its default; README:158-208
documents them.

## 7. Backfill machinery a new field plugs into

- `Step` trait (postprocess/step.rs:37-65): `id` (1..=62, bit `1 << id`), `name` (the `--step` name), `rank`,
  `needs` (Database | Bodies | External), `enabled(cfg)`, `applies(release)`, `reruns`, **`config_stamp() -> u64`
  (step.rs:58-62: "A bit that wasnt there before redoes this step on existing releases")**, `run(ctx)`.
  `ReleaseInfo` (step.rs:67-82) is what a step sees; `StepCtx` (step.rs:84-184) offers `files()` (cap 5 000),
  `segments()`, `conn()` (query_only shard), `fetch_bodies/fetch_prefix/fetch_article/article_exists` (Bodies lane only),
  `deadline` (run start + 120 s). `Outcome` (step.rs:205-223): `writes: Vec<StepWrite>` — "Your own tables only, or your
  own `releases` column. Static SQL with bound values"; `NameChange` (step.rs:239-248); `StepError` (step.rs:250-262).
- Registry `steps::all()` (postprocess/steps.rs:10-26): Par2Step, ListStep, ProbeStep, OpenSubtitlesStep, Naming,
  NfoName, PredbMatch, CategoryStep, media parse/lookup/nfo, OpenSubtitlesImdb.
- Tables: `ShardTable {name, create, indexes, release_key, added}` (postprocess/schema.rs:8-18) — migrations are
  **add-nullable-column only** (`added`, applied by `migrate` schema.rs:114-131); `PP_QUEUE` (:21-30), `PP_STATE`
  (:33-57), `PP_WALKS` (:60-76), `SHARD_TABLES` (:78-92), `RELEASE_COLUMNS` adds `name_source` (:96).
- Seeding: `engine::seed_shard` (postprocess/engine.rs:1073-1114) keeps meta `pp_walk_seeded`, `pp_steps_seen`,
  `pp_stamp_{id}`; a new step id or a new stamp bit inserts `walk("steps", bit)` (engine.rs:1106-1107) = a backfill over
  every existing release, newest first (`walk_tick` engine.rs:412-486: `where r.id < ?1 order by r.id desc limit ?2`,
  token bucket `backlog_per_second / SHARDS`). `atlas --reprocess` inserts the same kind of walk by hand.
- Cost model: a Database step re-deriving from stored rows runs at up to 2000 releases/s across shards; a Bodies step is
  bounded by 600 bodies/min (≈ 36 k releases/h at one body each, months over 118 M releases); External steps by the
  provider rpm.

## 8. SABnzbd handoff (download.rs, sab.rs)

- `download::download_release(id)` (download.rs:1-75): checks SAB's `complete_dir`, `sab::configure_servers`
  (sab.rs:183-196 → `server_section_update` sab.rs:220-299: host, port, username, password, ssl, priority, connections
  when set), starts the bundled `SABnzbd-5.0.4`, `wait_ready(90 s)`, `nzb::generate_nzb(id, Some(&watched_dir))`
  (download.rs:56), `sab::job_in_sab(..., 10 s)` (download.rs:58), opens the browser. SAB url `get_url`
  (sab.rs:440-451: `ATLAS_SAB_HOST/PORT`, default 8080); ini at `~/.sabnzbd/sabnzbd.ini`.
- `job_in_sab` (sab.rs:454-488) reads **only** queue `slots[].filename` and history `slots[].name` / `slots[].status`,
  and only to print to the terminal; nothing from SAB history (fail_message, pp/unpack result, bytes, dup detection) is
  stored. The handoff is a file in the watched folder, not the API, so no NZB headers are involved either.

## 9. Dashboards and metrics

- `app.rs` TUI: `show_results` (app.rs:36-84: #, Name, Size, Category, Parts, Date, Status `[broken]`), `show_release`
  (app.rs:86-145: poster, posted, size, parts, complete/incomplete, password text, files table present/expected max 30,
  inner files max 30), `release_actions` (app.rs:209-244: Download, Save NZB), `do_settings` (app.rs:520-629: servers,
  index mode, "Purge broken releases" :563, "Wipe db and cache" :572-603, api port :619-621), `main_menu`
  (app.rs:756-885), `selftest` (app.rs:887-933).
- `dashboard.rs` (251 lines): stats.json TUI (status, totals, headers/s, groups) — no per-release fields.
  `stats_dashboard.rs` (1691): `Content` aggregates (:133-157), `compute_content` (:177-246), `draw_content`
  (:903-985), `draw_postprocess` (:1074-1126), `draw_predb` (:1129-1146). Nothing here reaches the web UI.
- `metrics/mod.rs:1-21`: two Prometheus exporters, indexer and API processes, two ports; `metrics/names.rs:142-160`
  catalog gauges; `metrics/catalog.rs` walks the shards every `catalog_every_secs` in batches of 20 000.
  README:791-862 documents the series.

## 10. Data model this area reads or writes

| table (file) | columns | written by | read by (this area) |
|---|---|---|---|
| `s{N}.releases` (store.rs:133-163) | id, name, group_name, poster, posted_date, size, complete, parts, file_total, display_name, is_obfuscated, name_source (schema.rs:96) | indexer, naming steps | API rss/get, web rows, NZB render, purge classify |
| `s{N}.files` (store.rs:165-177) | id, release_id, filename, subject, subject_part, subject_mid, expected, file_total, seen, touched_at, blob | indexer (seal after `SEAL_AGE` 3 d store.rs:788) | NZB `<file>`; purge `last_touch`; API never reads subject |
| `s{N}.segments` + `domains` (store.rs:180-189) | file_id, local, domain, part, bytes | indexer | NZB `<segment>` via `store::articles` (store.rs:1935-1959) |
| `s{N}.meta` (store.rs:191) | key, value | compact (`last_compact`), purge (`purge_*`), reprocess (`naming_rename_named`), engine (`pp_*`) | CLI resume |
| `pp_queue`, `pp_state`, `pp_walks` (schema.rs:21-76) | see schema.rs | stage engine, `--reprocess` | stage |
| `release_fingerprint` (duplicates.rs:21-40) | release_id, posted BLOB, content BLOB, + par2_block, par2_blocks, posted_names | Par2Step | API `twins`/`alts` attrs (`counts`), web TWIN/ALT, filled NZB |
| `archive_release` (archive/store.rs:42-51) | release_id, …, encryption, names_hidden, password_check | archive probe | API `password` attr (`passwords()`), web PASSWORD |
| `archive_inner_files` (archive/store.rs:18-39), `release_nfo_password` (archive/store.rs:54-63) | inner names/sizes; nfo password | archive, NfoName | web only / `passwords()` |
| `release_probe` (probe/mod.rs:23-47) | release_id, crc32, agent, info JSON (`ProbeInfo` :53-78), + moviehash | ProbeStep | web only (API never) |
| `media_release` (media/steps.rs:25-53) | release_id, kind, title_key, season, episode, episode_last, air_date, year, quality, tvdb_id, tvmaze_id, trakt_id, tmdb_id, imdb_id, tvrage_id, imdb_from_nfo | media steps | API `MediaAttrs` (all but episode_last, quality), web `media_label` + `id_links` (no air_date, no tvrage) |
| `atlas.details.db` `Details` (media/details.rs:74-94) | title details, poster, backdrop | media details | web only |
| predb store (predb/db.rs:16 `nuked`) | size_bytes, category, pre_at, nuked, nuke_reason | feeds, `--predb-import` | neither API nor web shows `nuked` (`grep nuke src/web.rs src/api.rs` empty) |
| `~/.sabnzbd/sabnzbd.ini` | servers | `sab::configure_servers` | — |

## 11. UI surface: what the API exposes that the web UI does not (main)

| item | API | web UI (main) |
|---|---|---|
| `tvairdate` + `episode MM/DD` for dailies | media/query.rs:384-387 | never (no `air_date` in web.rs) |
| `rageid`/`tvrageid` | media/query.rs:417-418 | `id_links` web.rs:1790-1811 omits TVRage |
| per-file NZB `t=get&file=N` | api.rs:375-377 | `/ui/nzb` accepts it (web.rs:65) but no link is rendered; PR #48 adds a file list (`nzb_files_html`) without per-file links |
| `pubDate` with time of day and zone | api.rs:341, dates.rs:146-163 | `fmt_date` keeps 10 chars (ui.rs:531-533) |
| `<size>` exact bytes | api.rs:337 | row `title="{bytes} bytes"` + `fmt_size` (web.rs:790) — shown |
| `source`, `language`, `team`, `video`, `audio`, `hdr`, `resolution` from the name | api.rs:296-304 (request-time parse) | release page facts (own parse) — shown, not stored |
| `twins`/`alts` counts | api.rs:305-308 | TWIN/ALT tags + duplicates page — shown |
| `password` 0/1/2 | api.rs:330-332 | PASSWORD tag / fact — shown |
| `category` id | api.rs:333, 344 | category column — shown |
| `episode_last`, media `quality` | not emitted | not shown (stored in media_release) |
| `<newznab:response total>` | fake (offset+len) | no total either |

## 12. Catalog coverage (nzb block and derived additions)

- `nzb.xmlns`: a constant of Atlas's writer (nzb.rs:72). Nothing per release exists; Atlas reads no NZBs. → not_collected
  (as a UI item there is nothing to show).
- `head.meta`: only `type="category"` = group name (nzb.rs:73); the group is shown everywhere in the UI. The NZB side
  lacks password/title/ids although the data exists. → shown (group), NZB gap noted.
- `file.poster`: `releases.poster` per release, joined into every article (store.rs:1954-1957); shown on the release page
  and row. Per-file From is never collected. → shown.
- `file.date`: `article_timestamp(posted_date)` as Local (dates.rs:167-185) vs `pub_date` as UTC (dates.rs:146-163).
  UI shows the date only, no time, no zone (ui.rs:531-533). → stored_not_shown (time of day).
- `file.subject`: `files.subject` (store.rs:169) goes into every NZB (nzb.rs:85-88); the web UI never prints a subject
  (`grep subject src/web.rs` empty). → stored_not_shown.
- `groups.group`: all groups of a split post (nzb.rs:90-94, 121-134), one otherwise; UI GROUPS tag and release page.
  → shown. The catalog's "always one `<group>`" is wrong for split posts.
- `segment.bytes`, `segment.number`: `segments.bytes/part` (store.rs:181-189), shown only as aggregates (parts, size).
  → stored_not_shown.
- `segment.text` (message-id): `segments.local` + `domains.suffix`; never shown. → stored_not_shown.
- `nzb_identity` (segment-set hash): not computed; `release_fingerprint.posted/content` is the analogue and surfaces as
  TWIN/ALT; NZB filename `{name}.{id}.nzb` (nzb.rs:12-22) appears only on download. → not_collected (literal item).
- `external_oracles`: in this area, `--predb-import` (predb/file_import.rs) feeds the predb store that keeps `nuked`
  (predb/db.rs:16); API and web expose none of nuked/nuke_reason/pre_at. → stored_not_shown.
- `x_dnzb_response_headers`: `get_nzb` sends only `Content-Disposition` (api.rs:390) with `application/x-nzb`. The
  data for `X-DNZB-Name` (display_name), `-Category`, `-Password` (archive password), `-ProperName`, `-Details`
  (`/ui/release?id=`), `-NFO` all exist. → not_collected.
- `newznab_attrs_and_endpoints_not_emitted`: emitted set api.rs:296-308 and :330-337; `size`/`guid` exist only as RSS
  elements; `files` count is on the UI page but not an attr; `grabs`/`comments` are not counted anywhere; no
  `t=details/getnfo/comments/register/cart` (203). → not_collected (dominant), see §2.
- `downloader_history_fields`: sab.rs:454-488 reads queue filename and history name/status, prints, stores nothing.
  → parsed_not_stored.
- `nzblnk`: no occurrence in `src/`. → not_collected.
- `nzb_writer_fingerprint`: Atlas writes NZBs (lower-case `utf-8`, `poster date subject` order, `bytes number` order,
  XOVER `:bytes` family) and reads none. → not_collected.
- API-only exposures (§11): `tvairdate`/`air_date` → stored_not_shown; `rageid`/`tvrageid` → stored_not_shown;
  `file=N` → stored_not_shown (capability without UI link); `pubDate` time → stored_not_shown; `episode_last`,
  `quality` → stored_not_shown; `total` → not_collected.

## 13. Extension points

1. `release_attrs` (api.rs:285-310) — add `<newznab:attr>`s for stored facts (size, files, usenetdate, nfo, info,
   probe facts); the `add(name, value)` closure skips empties.
2. `rss` item template (api.rs:336-345) and the footer (api.rs:357) — a real `total` needs a count query.
3. `MediaAttrs` / `xml()` (media/query.rs:315-322, 378-428) — `episode_last`, `quality` are read by `attrs_with`
   but not emitted.
4. `CAPS` (api.rs:22-36) + `caps_body` (api.rs:203-209) — new `supportedParams`, `t=details`/`t=getnfo` advertising.
5. `handle` match on `t` (api.rs:131-167) + `endpoint_of` (api.rs:402-417) — new endpoints with their metric label.
6. `get_nzb` `headers` vec (api.rs:390) — `X-DNZB-Name/Category/Password/ProperName/Details/NFO` from data already
   fetched (`get_release`, `passwords`, `categories_of`); `ApiResponse.headers` is already plumbed to the client.
7. `render_nzb_in` head (nzb.rs:73) — `<meta type="password|title|name|...">`; both the API and `/ui/nzb` and the SAB
   handoff would get them for free. Also the `date` zone fix (use the same convention as `pub_date`).
8. `find_releases` (api.rs:245-257) and `ReleaseRow`/`COLUMNS` (search.rs:9-18, 33-34) — the one shared row; a new
   per-release column surfaces in both API and UI only through here or through a side table joined by id, the pattern
   used by passwords/categories/media/copies (`HashMap<i64, _>` keyed by release id).
9. A new `Step` with `config_stamp()` (step.rs:58-62) or a new `StepId` → automatic walk over existing releases
   (engine.rs:1106-1107), plus `atlas --reprocess --step NAME --apply` (reprocess.rs:74-99) for operator-driven
   backfill; `ShardTable.added` (schema.rs:8-18) for new nullable columns.
10. `config_example.toml`, README:622-790 (API docs, "Search results" :771-786) and docs/TODO.md:14 need updating with
   any new attr or endpoint.
11. `web::get` route table (web.rs:48-72) for `/ui/...` pages; `web.rs:12` for anything new to share from `api`.

## 14. Risks

- Performance: the API has 4 worker threads (api.rs:18) and recomputes name tags per item per request
  (`release_tags::parse` api.rs:286); each page does four side lookups (passwords, categories, media, copies). Adding
  probe/inner-file joins per item multiplies shard round trips (8 shards). Keep new facts in one `HashMap<i64,_>`
  lookup per page like the existing ones.
- Schema: migrations are add-nullable-column only (schema.rs:8-18, 114-131); a new table must be registered in
  `SHARD_TABLES` (schema.rs:78-92) with a `release_key` so the gc triggers purge it with the release.
- Backfill size: a walk over 118 M releases at `backlog_per_second 2000` is ≈ 16 h of pure DB work when the step needs
  no bodies; any body-fetching step is bounded by 600 bodies/min (months). `--reprocess` runs beside the indexer but
  `--purge --apply`, `--compact`, `--convert`, `--sealed-log-move`, `--search-indexes` need it stopped.
- Zone bug: NZB `<file date>` (Local) and `<pubDate>` (UTC) disagree for naive stored dates (dates.rs:146-185); a
  UI that prints the time would expose the inconsistency — pick one convention first.
- Response headers: `X-DNZB-*` values flow into SABnzbd/NZBGet job names and passwords; wrong `display_name` or a
  guessed password would mislabel downloads downstream, so emit `X-DNZB-Password` only for `Password::code() == 2`
  (known) and never for the NFO hint alone.
- PR conflicts: #48 (release page media cover) rewrites `web.rs` `release_html`/`probe_html`, adds `release_cover`
  (probe/mod.rs) and `NzbFile`/`Contents.list` (release_info.rs) — any file-list or per-file NZB link should build on
  it; #57 touches `api.rs:528-542` `start_warming` and +495 lines of `web.rs` (sort windows) — merge order matters for
  anything in `start_warming`; #51 (mid-file keyframes) and #54 (H.264 decoder) add `src/video/*` and probe output but
  do not touch api.rs; the stream-details branch is not fetched locally (unknown overlap with `probe_html`); the Finish
  web UI thread's twin grouping touches `row()`/duplicates rendering that the API's `twins`/`alts` attrs mirror.
- `t=get&file=N` is unadvertised and non-spec (docs/TODO.md:14); exposing per-file links in the UI makes it a public
  contract.
- `total` in `<newznab:response>` is fake (api.rs:357); clients paging on it stop early or loop.

## 15. Open questions

- Should NZB `<head><meta>` grow `password`/`title`/ids (nzb.rs:73) as part of "visible everywhere", given SAB reads
  them? It changes every emitted NZB and the TUI/SAB path too.
- Which zone convention for stored naive dates: UTC (`pub_date`) or Local (`posted_timestamp`)?
- Does the stream-details branch change `probe_html`/`release_html` in ways that collide with #48 (not fetched)?
- Is a true result `total` wanted (count query per request on 8 shards) or should the UI/API keep offset+len?
- Should `doubanid` be dropped from `ID_PARAMS` (api.rs:170) since it is never answered?
- Where should `episode_last` and media `quality` surface (API attr and/or UI fact)?

## 16. Files read

src/api.rs (all), src/app.rs, src/main.rs, src/config.rs (1-1308; 1309-1731 are tests), src/download.rs, src/sab.rs,
src/nzb.rs, src/dashboard.rs, src/stats_dashboard.rs, src/metrics/mod.rs (header), src/metrics/names.rs,
src/metrics/catalog.rs (head), src/postprocess/{mod.rs, purge.rs, reprocess.rs, steps.rs, step.rs, schema.rs,
rules.rs, engine.rs (parts)}, src/search.rs, src/web.rs (parts), src/media/{query.rs, steps.rs (MEDIA_RELEASE),
details.rs (Details), resolve.rs (Ids)}, src/dates.rs, src/duplicates.rs (parts), src/archive/{store.rs, mod.rs}
(parts), src/probe/mod.rs (head), src/store.rs (schema, articles), src/release_info.rs (parts), src/filters.rs
(StoredPost), src/field_search.rs (head), src/naming/{reflag.rs, unname.rs} (heads), src/predb/{file_import.rs, db.rs}
(parts), src/sealed_store.rs (run_move), src/ui.rs (fmt_size/fmt_date), README.md (config, purge/reprocess, settings,
web UI, API, metrics, env, SABnzbd), config.example.toml, docs/TODO.md, docs/port/08-media-ids.md, the other map notes
(coordination.md, web-ui.md §6-10, storage.md §6, §9-12), git diffs of remotes/pr/48, pr/57, pr/50, pr/51, pr/54,
pr/55, pr/58 against main, and the catalog `/tmp/claude-0/-home-claude/b80d34bc-6432-5cb9-a471-5c997487e01d/scratchpad/catalog-current.md`
lines 532-562 and 601-636.
