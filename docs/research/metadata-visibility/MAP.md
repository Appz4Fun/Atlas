# Atlas code map: every catalog field, visible on every release page

Scope: the Atlas Usenet indexer at `/home/claude/appz4fun/atlas`, branch `main` at 55d2e51 ("#52"); `origin/main` is one squash ahead at 67504d6 ("Decode bzip2, deflate64, compress and lz4 layers (#56)"), which touches only the nest/decoder side. The feature: every metadata field in the catalog `/tmp/claude-0/-home-claude/b80d34bc-6432-5cb9-a471-5c997487e01d/scratchpad/catalog-current.md` (1624 lines, 293 YAML items checked by its author against a29ee52 on 2026-11-02, plus the posting-tool table) becomes visible in the web UI on every release page, backfilled for existing releases.

Sources: the 14 reader notes and `coordination.md` under `/tmp/claude-0/-home-claude/b80d34bc-6432-5cb9-a471-5c997487e01d/scratchpad/atlas-map/` (api-cli, archive-payload, ingest-postprocess, media, media-title-resolution, naming-category, posting-tools, predb, probe, release-denominator, storage, tests-and-conventions, twin-grouping, web-ui), re-checked against the code on main for every line cited below. `file:line` means `src/<file>` at 55d2e51 unless the path says otherwise; catalog lines are `catalog L<n>`. GitHub is unreachable from this session (HTTP 403), so every PR fact comes from the local refs `pr/48`, `pr/51`, `pr/54`, `pr/55`, `pr/57`, `pr/58`, `origin/feat/release-stills-strip` and `origin/claude/project-thread-lgvcpf` (PR #64), diffed against `main`.

Status vocabulary used in section 4: **shown** (on the release page today), **stored_not_shown** (in SQLite or the probe JSON, no UI), **parsed_not_stored** (read in Rust for a decision, then dropped), **not_collected** (never read), **unclear** (none remained after reconciliation). A composite item takes the status of its primary value; the evidence column names the parts that differ.

---

## 1. How a release page is rendered today

### 1.1 Process, routing, security

- One `tiny_http` process serves the Newznab API and the htmx web UI with `WORKERS = 4` threads (api.rs:18, 509). Every request goes to `crate::web::get` first (api.rs:440); the web router is `web::get` (web.rs:48-72), the API handles the rest.
- Routes (web.rs:52-70): `/static/htmx.min.js` (htmx 2.0.11 bundled, web.rs:22), `/static/atlas.css` (web.rs:23 includes `assets/web.css`), `/static/atlas.js`, `/static/atlas.svg`, `/` (search page or login), `/ui/results`, `/ui/release`, `/ui/duplicates`, `/ui/details`, `/ui/poster`, `/ui/nzb` (plain, or `fill=1` for a twin-filled NZB). Signed-out `/ui/details` and `/ui/poster` answer 401 with `HX-Redirect`, the others redirect (web.rs:66-69).
- Sign-in is a cookie checked by `signed_in` (web.rs:142); the response carries a `Content-Security-Policy` header (web.rs:127, constant `CSP` near :41) and an htmx config constant (web.rs:44). No inline scripts: anything interactive must be htmx attributes or `assets/atlas.js`.

### 1.2 `release_page` (web.rs:989-1017): what it loads and from where

| Call | Source | Table / data | Notes |
|---|---|---|---|
| `get_release(id)` (search.rs:552-560) | shard `s{id%8}.releases` | `ReleaseRow {id, name, group_name, poster, posted_date, size, complete, parts}` (search.rs:9-18) | filters only `filters::group_blocked` (search.rs:559); 404 and 500 pages via `release_problem` (web.rs:1019-1026, `diag` web.rs:829) |
| `category::categories_of(&[id])` (category/mod.rs:21-28) | `release_category` (category/step.rs:13-21) | one i64 per release | `UNCATEGORIZED` fallback (web.rs:1036) |
| `archive::store::view(id)` (archive/store.rs:183 -> `view_with` :171-185) | `archive_release`, `archive_inner_files`, `release_nfo_password` | `InnerView {password, encryption, names_hidden, password_check, files}` (archive/store.rs:152-158) | inner files read with `limit 256` (archive/store.rs:93); at most `MAX_STORED = 64` are ever written (archive/mod.rs:27) |
| `media::query::attrs_of(&[id])` (media/query.rs:324-335) | `media_release` (media/steps.rs:25-77) | `MediaAttrs {kind, season, episode, air_date, year, ids}` (media/query.rs:315-322) | the same struct feeds the API `xml` attrs (media/query.rs:44-49), so its shape is an API contract |
| `release_info::names(id)` (release_info.rs:132-139) | `releases.name/display_name/is_obfuscated/name_source` | `Names {posted, display, obfuscated, source}` (release_info.rs:16-21) | `name_source_label` (release_info.rs:24-35) maps `NameSource` (naming/mod.rs:28-41: Ingest 0, Cleared 1, PostedFile 3, NamingRegex 5, InlineBody 9, Nfo 10, ArchiveInner 15, Par2 20, PreDb 100) to "a posted file name", "the subject", "the NFO", "a file inside the archive", "the PAR2 file", "PreDB"; Ingest/Cleared/InlineBody print nothing |
| `release_info::contents(id)` (release_info.rs:152-157) | `files`, `segments`/`files.blob` via `store::each_article` (store.rs:1947), `release_fingerprint.par2_block/par2_blocks` | `Contents` (release_info.rs:39-60): files, par2_files, recovery_volumes, data_bytes, recovery_bytes, articles seen/expected (total, data, recovery), recovery_blocks, par2_block, par2_blocks | recomputed on every page view, never stored |
| split-post fold (web.rs:1007-1015) | `release_info::split_post_with` (release_info.rs:266), `contents_of_split` (:439) | for an incomplete release spread over groups: size, complete, parts replaced by the union | PR #64 replaces this with `whole_post_with` and makes `inner` mutable (coordination.md:35; branch release_info.rs `whole_post_with`) |

### 1.3 `release_html` (web.rs:1028-1201)

Signature on main: `release_html(r: &ReleaseRow, category: Option<i64>, inner: &InnerView, media: Option<&MediaAttrs>, names: Option<&Names>, contents: &Contents) -> String` (web.rs:1028-1035). PR #64 appends a seventh parameter `recovery: Option<&release_info::RecoveryOnly>` (coordination.md:35).

Tags line (web.rs:1037-1045, 1054): `media_label` (S01E02 / S01 / year, web.rs:735-742, class `tag-ref`), `password_tag` (PASSWORD `tag-bad` / PASSWORD? `tag-warn`, web.rs:698-704), COMPLETE `tag-ok` or INCOMPLETE `tag-warn`, OBFUSCATED `tag-warn` when `names.obfuscated`, then `name_tags(&parsed)` (web.rs:720-732: resolution `tag-ref`, each HDR label `tag-ok`, quality with no colour class).

The fact closure (web.rs:1048-1052) writes `<dt>{name}</dt><dd>{value}</dd>` and skips empty values; nothing prints a placeholder. The name is parsed once per view with `release_tags::parse(&r.name)` (web.rs:1053; release_tags.rs:88; `Tags` struct release_tags.rs:7-32 over the PTT port ptt/mod.rs:521). Facts in order:

| Fact | Lines | Source |
|---|---|---|
| Category (link `/?cat=`) | 1055 | `release_category` |
| Title (search link `q=`) | 1057-1060 | `parsed.title` |
| Size (`fmt_size` and a `/?size=` link) | 1061-1066 | `r.size`; owned by the download-size thread / PR #64 (coordination.md:16-17) |
| Posted | 1067 | `r.posted_date` via `fmt_date` (ui.rs:531) |
| Groups (list with article counts) or Group | 1068-1078 | `release_info::groups_of(posted_name)` (release_info.rs:212-236, `limit 64` per shard, sorted by articles) |
| Poster, Email (+ MADE UP tag) | 1079-1085 | `parse_poster` (release_info.rs:451), `looks_made_up` (:501); `search_link_to("poster", raw, name)` (web.rs:800) |
| Posted as (only when it differs from the name) | 1087-1090 | `names.posted` |
| Name from | 1091-1093 | `name_source_label(names.source)` |
| Year, Ripped from (network), Site | 1095-1103 | `parsed` |
| Video (codec, bit depth, container), Audio (codecs + channels), Languages | 1104-1115 | `parsed` |
| Edition, Release group (search link `team=`) | 1116-1121 | `parsed` |
| Parts | 1122-1124 | `r.parts` |
| Encryption | 1125 | `encryption_fact(inner)` (web.rs:1763-1771): encryption label + ", file names hidden", or "Encrypted, type not recorded" / "Maybe encrypted" |
| Password | 1126-1128 | `release_info::password_hint(r.id, names)` (release_info.rs:539 `password_hint_with`) + `password_fact` (web.rs:1741-1749): `<code>pw</code> found in <source>` + WORKS / PROBABLY WORKS / WRONG / ", not tried" from `inner.password_check.accepts` |
| Files, PAR2 files "(n recovery)" | 1129-1134 | `contents` |
| Articles | 1135-1137 | `health(contents, missing)` (web.rs:1779-1787): "x of y" + ALL THERE / REPAIRABLE / NOT ENOUGH RECOVERY |
| Repair | 1138 | `repair_fact` (web.rs:1752-1760): exact percentage from the PAR2 set, else "about N% (size)" |
| Links | 1139-1142 | `id_links(kind, ids)` (web.rs:1790-1811): IMDb, TMDB, TheTVDB, TVmaze, Trakt with `rel="noreferrer"`; no TVRage |

Then: **Files inside** (web.rs:1144-1161) as an `<h2>` plus a `table-wrap` table of `inner.files` (name, size) when any; the **probe** block (web.rs:1163-1170): `probe::read(&conn, r.id)` (probe/mod.rs:142-148, `serde_json::from_str(...).ok()`, so an unreadable JSON silently drops the whole block) and `opensubtitles::read`, passed to `probe_html`; **copies** (web.rs:1171-1181): `duplicates::copies(r.id)` (duplicates.rs:253), `can_fill` (:522) adds the "NZB, gaps filled from a twin" button, `copies_html` (web.rs:1555-1572) renders at most `FULL_COPIES = 12` cards (web.rs:1456) via `copy_card` (web.rs:1463-1548) and links for the rest.

Page template (web.rs:1183-1200): masthead, breadcrumb `atlas::releases::<id>`, `<h1>` name, tags, the actions row (Download NZB, fill button, "More in this category"), then
```
<div id="title-details" hx-get="/ui/details?id={id}" hx-trigger="load" hx-swap="outerHTML"></div>   (web.rs:1190)
<div class="panel"><dl class="facts">{facts}</dl></div>
{copies}
{probe}
{files}
```
so the order on the page is: details (lazy) -> facts -> copies -> probe panels -> files inside. PR #64 adds a `{warning}` slot before the title-details div (coordination.md:35); PR #48 adds `{nzb_files}` (pr/48 web.rs hunk at 1141-1147); the stills branch adds `{stills}` before `{copies}` (origin/feat/release-stills-strip web.rs hunk 1139-1147).

### 1.4 `probe_html` (web.rs:1243-1453): the panels built from `release_probe.info`

`dd` (web.rs:1244-1248) is the same skip-empty writer; panels are emitted at web.rs:1445-1452 in the order **Tracks, Chapters, yEnc, Packing, How it was posted**, followed by the NFO and SFV blocks (which use `<h2>` + `<pre>`/table rather than `panel`).

| Panel | Lines | Facts (label: JSON field) |
|---|---|---|
| yEnc | 1250-1289 | File name: `yenc.name`; File size: `yenc.file_size`; Parts: `yenc.total`; Line length: `yenc.line_len`; Article size: `segment_size`; CRC32 (search link `crc=`): `file_crc32`; Overhead: `overhead_percent`; Checks: `verdict(continuity_ok, "ALL BYTES COVERED", "BYTES MISSING")`, `verdict(part_size_ok, ...)`, `verdict(part_crc_ok, ...)` (web.rs:1270-1275; `verdict` 1227-1233); On the server: FIRST/MIDDLE/LAST THERE (`tag-ok`) or GONE (`tag-bad`) from `health` (1278-1289) |
| How it was posted | 1291-1300 | Posted with (search link `agent=`): `agent_of(headers)` (probe/mod.rs:107-109); then every kept header except User-Agent, X-Newsreader, X-Newsposter and Xref, printed raw (1294-1297), i.e. Injection-Date, NNTP-Posting-Date, Date, Path, Injection-Info, Organization, NNTP-Posting-Host, X-Complaints-To (KEPT_HEADERS probe/mod.rs:81-94, 512 chars each :100); Crossposted to: `crossposted_to` as group links (1297-1300) |
| Packing | 1302-1325 | Format: `archive.format`; Method: `archive.method`; Encryption tag: NAMES AND FILES ENCRYPTED / FILES ENCRYPTED / NOT ENCRYPTED (1307-1313); First file: `archive.name`; Unpacked: `archive.unpacked`; File CRC32: `archive.crc32`; Layers: `nested_chain(layers)` (1322-1325; web.rs:1840-1854 prints `format method` per layer, a STOP reason as `tag-bad`, and the innermost name) |
| Tracks | 1327-1414 | Container, Written by (`writing_app`), Muxed by (`muxing_app`), Title, Duration (h:mm:ss); one Video line per track: codec, WxH, fps, N-bit, hdr label, "Dolby Vision profile N[, enhancement layer]" (1342-1361); one Audio line per track: codec, channels, Hz, language, Atmos, commentary, name (1362-1377); one Subtitles line per track: codec, language, forced, SDH, commentary, name (1378-1396); OpenSubtitles hash: `moviehash`; OpenSubtitles match: title + SxxEyy + (year) linked to IMDb, plus the matched release name (1398-1414) |
| Chapters | 1415-1423 | `h:mm:ss` -> chapter name or "Chapter n" |
| NFO | 1425-1432 | `<h2>NFO</h2>`, IMDb links from `imdb_ids`, `<pre class="nfo">` with the text |
| SFV | 1433-1444 | `<h2>SFV</h2>` + table of name -> crc32 |

Fields of `ProbeInfo` that no panel prints: `file` (the main file's Subject name), `yenc.part`, `yenc.begin`, `yenc.end`, `yenc.yend_size`, `yenc.pcrc32`, `yenc.crc32` (the struct's own field; the shown CRC is `file_crc32`), `archive.packed`, `VideoTrack.display_width/display_height/primaries/transfer/dv_level/language` (probe/media.rs:39-50). PR #48 renders the video extras (coordination.md:6).

### 1.5 The lazy panel: `/ui/details`

`details_panel` (web.rs:1657-1663) loads `attrs_of(&[id])`, then `media::details::for_title(&media_config(), kind, &ids)` (media/details.rs:441), which calls TMDB/OMDb/Trakt/TVDB on the request thread when the `details` cache (`atlas.details.db`, media/details.rs:43-70) is cold, and returns `details_html` or an empty body. `details_html` (web.rs:1679-1715) renders the backdrop `<img>`, a `section.panel.title-details` with the poster `<img src="/ui/poster?kind=&tmdb=">` (served by `poster`, web.rs:1717-1739), the title, `scores_html` (IMDB / RT / MC / TMDB / TRAKT `tag-ref` tags, web.rs:1665-1677), the overview, and a `dl.facts` with Released, Blu-ray, Genres, Runtime, Network, Status, Cast. Because the response replaces the div with `hx-swap="outerHTML"`, an empty response removes the placeholder: the pattern to copy for any new lazy panel.

### 1.6 Copies and clusters

`copy_card` (web.rs:1463-1548) re-reads per copy: `attrs_with`, `details::kept_for`, `names_with`, `contents_with`, `archive::store::view_with`, `password_hint_with`; tags (badge, name_tags, OBFUSCATED, password, INCOMPLETE, scores) and a `dl.facts.facts-wide` with Size, Group, Poster/Email, PAR2 files, Repair, Articles, Encryption, Password, Video, Audio, Languages, Ripped from, Release group, Links, plus a 72x108 poster thumb. `duplicates_page` (web.rs:1575-1654) pages clusters (`CLUSTERS_PER_PAGE = 10`, `FULL_MEMBERS = 6`, web.rs:1457-1459). Any per-release fact added to `release_html` has to be mirrored here or an agreed rule must say copies show name-derived facts only (media-title-resolution.md §10 last row; the twin-grouping thread owns this surface, coordination.md:13).

### 1.7 UI conventions to reuse

- **Panels**: `panel(title, facts)` (web.rs:1235-1240) returns `""` when `facts` is empty, else `<h2>title</h2><div class="panel"><dl class="facts">...</dl></div>`. Facts panels are `dl.facts` two-column grids (`assets/web.css:203-205`: `dt` in mono uppercase muted, `dd` with `overflow-wrap: anywhere`); `.facts-wide` is the four-column variant for cards (web.css:221, collapsing to two columns under 820px, web.css:241-258). `.panel` is the card surface (web.css:202). `.detail` caps the page at 1100px (web.css:234). `.table-wrap` + `table` (web.css:171-172) for tabular lists, `pre.nfo` (web.css:225) for monospace text.
- **Absence, not placeholders**: `fact`/`dd` skip empties; the only placeholder in the UI is `fmt_size(None)` printing "?" (ui.rs:513).
- **Tags**: `tag(class, text)` (web.rs:694-696) -> `<span class="tag {class}">`; classes `tag-ok` (patina, healthy), `tag-ref` (steel, reference), `tag-warn` (amber, attention), `tag-bad` (ember, failure) (web.css:187-191); the token block is Oxide's (web.css:1-5 comment, tokens 7-34, dark theme 36-59 under `prefers-color-scheme` with a `data-theme` override). `verdict(Option<bool>, good, bad)` (web.rs:1227-1233) prints nothing for `None`. `diag(level, title, body)` (web.rs:829-835; `.diag`, `.diag-help`, `.diag-caution` web.css:193-199) is the notice block.
- **Escaping**: every value through `api::escape` (api.rs:89) or `attr` (web.rs:156) for attributes and `url_encode` (web.rs:160) for query values; external links carry `rel="noreferrer"` (web.rs:1403, 1428, 1791). `search_link_to(param, value, shown)` (web.rs:800-802) and the local `search_link` closure (web.rs:1056) are the two ways to link into the search page; `thousands`, `fmt_size` (ui.rs:513), `fmt_date` (ui.rs:531) format numbers.
- **Lazy panels**: the `#title-details` pattern (web.rs:1190) with a route in `web::get` guarded by `signed_in` and an empty body when nothing applies.
- **Results rows**: `row` (web.rs:744-797) with `RowExtras` (web.rs:684-690), `results` (web.rs:845) and `scope_inputs` (web.rs:883) are the search surfaces; PR #57 rewrites the sort path (web.rs hunks 5-730) and PR #64 adds `size_note`/`recovery_tag` to `row` (branch web.rs:699-760, 820); coordination.md:17 asks that any per-row drawer be a separate htmx-loaded row and leave the Size cell alone.
- **Tests and screenshots**: web.rs unit tests at 1856-2217 pin tag texts (e.g. `media_label` at 2204-2208); UI PRs carry screenshots made with `.claude/skills/run-atlas/{shot.cjs, mock_nntp.py, driver.sh}` under `pr-shots/<name>/{dark,light,phone}.png` (tests-and-conventions.md); `rustfmt.toml` `max_width = 120`, toolchain 1.99.0 (`rust-toolchain.toml:4`); comments lowercase with the deliberate "soo" spelling (see store.rs:233, media/steps.rs:79).

---

## 2. Data model

### 2.1 Files on disk

| File | Holds | Opened by |
|---|---|---|
| `atlas.db` (main) | `groups` (db.rs:328-342: cursors, `first_article/last_article`, `low_posted`), `articles` (db.rs:446), `backfill_chunks/deepest/sweeps/sweep_gen/speculative` (chunks.rs:20-50), `meta` (store.rs:115) | indexer, API/web readers (`db::open`, `db::with_reader`) |
| `atlas.s0.db` .. `atlas.s7.db` | the shards: `SHARDS = 8` (store.rs:35); a release id is `seq * 8 + shard` so `shard_of_id(id) = id % 8` (store.rs:44-48); each shard holds the core tables (2.2) plus every registered plug-in table (2.3) | attached as `s0..s7` on every connection |
| `atlas.media.db` | `media_titles` (media/store.rs:9-25): provider answers per title key, deletable cache (README.md:757) | media steps, fan-out thread |
| `atlas.details.db` | `details` (media/details.rs:43-64), `details_lookups` (:65-70): TMDB/OMDb/Trakt/TVDB facts per title, poster/backdrop paths | `details_panel`, backfill thread (`start_backfill` media/details.rs:673) |
| `atlas.predb.db` | `titles`, `title_keys`, `feeds`, `meta` (predb/db.rs:8-43) | predb poller and `predb_match` step |
| `posters/` | TMDB poster and backdrop images, trimmed by `trim_posters` (media/details.rs:688) | `/ui/poster` |
| sealed-log service | the external article log; shards keep 21-byte locators (storage.md) | ingest |

### 2.2 Core shard tables (store.rs:129-249, `build_shard` and `migrate_shard`)

| Table | Columns | Written by | Read by |
|---|---|---|---|
| `releases` (store.rs:133-144) | `id, name, group_name, poster, posted_date, size, complete, parts, file_total, display_name, is_obfuscated`; plus the append-only `RELEASE_COLUMNS` = `name_source INTEGER` (postprocess/schema.rs:96, added by `migrate` :115); unique `(name, group_name)`; FTS5 `releases_fts` over name/display_name with insert/delete/update triggers (store.rs:148-161) | ingest `upsert` (store.rs:1367-1569; `on conflict ... poster = excluded.poster, posted_date = excluded.posted_date` :1394-1396 overwrites poster and date on every merge), naming steps (display_name, name_source), `--reflag-obfuscated` | search, API, web, every step |
| `files` (store.rs:165-177) | `id, release_id, filename, subject, subject_part, subject_mid, expected, file_total, seen BLOB, touched_at, blob BLOB`; unique `(release_id, filename)` | ingest; sealing writes `blob` (store.rs:826-860) | `each_article` (store.rs:1947), `Contents`, NZB |
| `domains` + `segments` (store.rs:180-188) | `domains(id, suffix)`; `segments(file_id, local BLOB, domain, part, bytes)` primary key `(file_id, local, domain)`, `without rowid`; `local` is alphabet-packed with a class byte (`pack_local`/`unpack_local` store.rs:311-415) | ingest | `each_article`, NZB, moviehash tail selection |
| `meta` (store.rs:191) | running totals (`releases`, `articles`), `id_key0/1` (secret hash key, store.rs:233-239), `pp_walk_seeded`, `pp_steps_seen`, `pp_stamp_<id>` (engine.rs:1073-1114), `archive_name_bytes` (archive/store.rs:30-36) | everyone | everyone |
| `collections` (store.rs:207) | `(key, release_id)`: the collection hash -> release | ingest (collections.rs) | ingest |
| `held_back` (store.rs:211-219) | rows a save could not place while a file was sealed, folded in when sealing | ingest | sealing |
| `release_ids` (store.rs:225-230) | `(release_id, id_hash, file_id)`: every message-id hash of a multi-file release | ingest | ingest (`index_release`) |

### 2.3 Plug-in shard tables: `ShardTable` and `SHARD_TABLES`

`ShardTable { name, create, indexes, release_key, added }` (postprocess/schema.rs:8-19). `migrate(conn)` (schema.rs:114-130) runs on every shard open (store.rs:248 from `migrate_shard`): `add_missing` for `RELEASE_COLUMNS`, then for each registered table `create table if not exists`, `add_missing` for the `added` nullable columns (`alter table add column`, schema.rs:102-110), `create index if not exists` for each index, and a `gc_{table} after delete on releases` trigger when `release_key` is set. The doc comment states the house rule: nothing existing is rewritten and no index is built on a table that has rows (schema.rs:112-113). Compaction copies registered tables (compact.rs:745) and refuses unregistered ones (compact.rs:901, 1049); `convert.rs` and "Wipe DB and cache" must also carry them (docs/port/00-overview.md:57 rule 7, with a test per table: `tests/archive_schema.rs` is the pattern).

Registry order on main (schema.rs:78-92): `PP_QUEUE, PP_STATE, PP_WALKS, archive INNER_FILES, archive RELEASE, archive NFO_PASSWORD, duplicates FINGERPRINT, probe PROBE, opensubtitles OPENSUBTITLES, predb MATCH, predb INNER_KEY, category RELEASE_CATEGORY, media MEDIA_RELEASE`. PR #64 inserts `archive::link::UPLOAD_LINK` after FINGERPRINT (branch schema.rs:85); PR #48 appends `probe::COVER` (pr/48 schema.rs:87); PR #51 appends `probe::stills::STILLS` (pr/51 schema.rs:88).

| Table (definition) | Columns | `added` | Indexes | Writers | Readers |
|---|---|---|---|---|---|
| `pp_queue` (schema.rs:22-31) | `release_id PK, due_at` | - | `pp_queue_due(due_at)` | saves (store.rs:1549 `on conflict ... due_at`), `--reprocess` (reprocess.rs:59-60) | engine queue tick |
| `pp_state` (schema.rs:34-59) | `release_id PK, done, failed, net_wanted, db_attempts, net_attempts, net_next_at, parts_seen, last_error, rerun_gen, rerun_mask` | - | `pp_state_net` (partial, net_wanted = 1), `pp_state_failed` | engine `apply` (apply.rs:65-250; rerun at :397), `--reprocess --failed` (reprocess.rs:54) | engine, dashboard |
| `pp_walks` (schema.rs:62-75) | `id PK, kind, top_id, before_id, redo_mask, group_name, seen, ran, created_at` | - | - | `seed_shard` (engine.rs:1073-1114), `--reprocess` (reprocess.rs:34-36) | `walk_tick` (engine.rs:412-486) |
| `archive_inner_files` (archive/store.rs:18-40) | `release_id, seq, name, size, source` PK `(release_id, seq)` without rowid; a `before delete on releases` trigger gives names back to `meta.archive_name_bytes` | - | - | Par2Step (source PAR2), ListStep (RAR4/RAR5/7z/ZIP), capped at `MAX_STORED = 64` rows (archive/mod.rs:27) and `max_names_mb` (config.rs:551, 561) | `view_with` (limit 256), content fingerprint |
| `archive_release` (archive/store.rs:43-51) | `release_id PK, password` | `encryption INTEGER, names_hidden INTEGER, password_check BLOB` | - | ListStep (`PasswordTest` serialised into `password_check`, RAR5 and zip only) | `view_with`, API password attr (api.rs:330-332), `encryption_fact`, `password_fact` |
| `release_nfo_password` (archive/store.rs:54-62) | `release_id PK, password TEXT` | - | - | probe and NFO naming steps (`SET_NFO_PASSWORD` :64) | `password_hint_with` (release_info.rs:539) |
| `release_fingerprint` (duplicates.rs:21-40) | `release_id PK, posted BLOB, content BLOB` | `par2_block INTEGER, par2_blocks INTEGER, posted_names BLOB` (dead column, never written on main) | partial indexes on `posted`, `content` | Par2Step (`twin_fingerprint` duplicates.rs:64-92, `TWIN_VERSION = 2` :92; `SET_POSTED` :42), ListStep finish (`content_fingerprint` :173), `forget_old_twins` (:97), `CLEAR_CONTENT` (:53) | `copies_with` (:224), `counts_with` (:418), `Contents.exact_repair_per_mille` |
| `release_probe` (probe/mod.rs:23-49) | `release_id PK, crc32, agent, info TEXT NOT NULL` | `moviehash TEXT` | partial indexes `release_probe_crc32`, `_agent`, `_moviehash` | ProbeStep `SET_PROBE` (probe/mod.rs:51-52; insert or replace) | `probe::read` (:142-148), search by `crc`, `agent`, `moviehash` |
| `release_opensubtitles` (probe/opensubtitles.rs:27-48) | `release_id PK, moviehash, imdb_id, tmdb_id, kind, title, year, season, episode, release_name, checked_at` | - | partial `imdb_id` | OpenSubtitlesStep (StepId 5, rank 130, External) | `opensubtitles::read` -> "OpenSubtitles match"; OpenSubtitlesImdb step (StepId 35, rank 505) copies the id into `media_release` |
| `predb_match` (predb/step.rs:25-43), `predb_inner_key` (:44-53) | match per release (title key, size, kind...), inner-file keys for hash-named posts | - | see file | `PredbMatch` (StepId 16, rank 300, Database; off by default `predb.enabled: false` config.rs:378) | naming (PreDb source), `metrics/catalog.rs` only (storage.md:81 corrected in section 6) |
| `release_category` (category/step.rs:13-21) | `release_id PK, category` | - | - | CategoryStep (StepId 24, rank 400, Database; `category.enabled: true` config.rs:336) | `categories_of`, API, web |
| `media_release` (media/steps.rs:25-77) | `release_id PK, kind, title_key, season, episode, episode_last, air_date, year, quality, tvdb_id, tvmaze_id, trakt_id, tmdb_id, imdb_id, tvrage_id, imdb_from_nfo` | - | 7 indexes (title, and one per id with season/episode) | media_parse (StepId 32, rank 500), media_lookup (33, 510, External, `config_stamp = provider_bits`), media_nfo (34, 520, Bodies), OpenSubtitlesImdb (35, 505) | `attrs_with` (media/query.rs:331), API id search (`episode_sql` media/query.rs:190) |

Side-DB tables without the `added` mechanism: `media_titles` (media/store.rs:9-25) needs an explicit `pragma_table_info` guard (the `details` pattern at media/details.rs:101-111) and is a deletable cache, so nothing the UI shows may require it (README.md:757).

### 2.4 JSON shapes in `release_probe.info`

`ProbeInfo` (probe/mod.rs:53-78): `file: Option<String>`, `yenc: Option<YencLines>`, `file_crc32: Option<u32>`, `segment_size: Option<u64>`, `overhead_percent: Option<f64>`, `continuity_ok / part_size_ok / part_crc_ok: Option<bool>`, `health: BTreeMap<String, bool>` (keys first/middle/last), `headers: BTreeMap<String, String>` (KEPT_HEADERS only, 512 chars), `crossposted_to: Vec<String>`, `archive: Option<ArchiveHead>`, `layers: Vec<nest::Layer>` (`#[serde(default, skip_serializing_if = "Vec::is_empty")]`), `media: Option<MediaInfo>`, `nfo: Option<String>` (`MAX_NFO = 256 KiB`, probe/step.rs:21), `imdb_ids: Vec<String>`, `sfv: BTreeMap<String, u32>`, `moviehash: Option<String>`.

- `YencLines` (probe/yenc.rs:7-18): `name, file_size, line_len, part, total, begin, end, yend_size, pcrc32, crc32`, all `Option`.
- `ArchiveHead` (probe/archive.rs:12-35): `format` ("RAR4", "RAR5", "7z", "ZIP"), `encrypted`, `headers_encrypted`, `method: Option<String>` (m0-m5 for RAR, the zip method's name, None for 7z), `name`, `unpacked`, `packed`, `crc32`; `stored_at`, `continues`, `data_at`, `dir` are `#[serde(skip)]` and never reach the JSON.
- `nest::Layer` (probe/nest.rs:25-39+): `format` (RAR4, RAR5, 7z, ZIP, TAR, GZIP, XZ, LZMA, LZIP, ZSTD, BZIP2, COMPRESS, LZ4, LZOP), `method`, `name`, `unpacked` (skipped when None), `offset`, `stored`, `stop` (why the descent stopped).
- `MediaInfo` (probe/media.rs:8-24): `container, muxing_app, writing_app, title, duration_secs, video: Vec<VideoTrack>, audio: Vec<AudioTrack>, subtitles: Vec<SubtitleTrack>, chapters: Vec<Chapter>`; `header_complete` is `#[serde(skip)]`. `VideoTrack` (:35-51): `codec, width, height, display_width, display_height, frame_rate, bit_depth, hdr ("HDR10"|"HLG"|"PQ"), primaries, transfer, dv_profile, dv_level, dv_enhancement_layer, language`. `AudioTrack` (:55-65): `codec, channels, sample_rate, language, name, atmos (from the name), commentary`. `SubtitleTrack` (:69-76): `codec, language, name, forced, hearing_impaired, commentary`. `Chapter` (:28-31): `start_secs, name`. The track structs carry `#[serde(default)]`, so new `Option`/`bool` fields are read from old rows as None/false; `ProbeInfo` itself has no `#[serde(default)]`, so a new field there must be `Option` or carry `#[serde(default)]`, and a renamed or removed field makes `probe::read` return None and the whole probe block vanish for every old row (probe/mod.rs:147).

### 2.5 How a new table, column or step is added

1. Declare a `const X: ShardTable` next to the step (pattern probe/mod.rs:23-49): `create table if not exists`, partial indexes only where searched, `release_key: Some("release_id")` for the gc trigger, new columns on an existing table through `added` (archive/store.rs:50 pattern). Append to `SHARD_TABLES` (schema.rs:78-92); compaction and migration pick it up; add the compact/convert/wipe tests (docs/port/00-overview.md:57).
2. For a per-release flag on `releases`, append to `RELEASE_COLUMNS` (schema.rs:96); the README warns a one-column rewrite of 118M rows costs about 12 GB of WAL (README.md:512).
3. A step implements `Step` (postprocess/step.rs:37-65): unique `StepId(u8)` and `rank` within its part's range (`steps.rs:36-38` test: ids 1..=7 ranks 100..=199, 8..=15 / 200..=299, 16..=23 / 300..=399, 24..=31 / 400..=499, 32..=39 / 500..=599), `needs` (Database / Bodies / External), `enabled(cfg)`, `applies(release)`, `reruns {on_rename, on_growth}`, `config_stamp` bits, `run(ctx)`. Register in `steps::all()` (postprocess/steps.rs:10-26). Taken on main: ids 1 (archive_par2, 100), 2 (archive_list, 110), 4 (probe, 120), 5 (opensubtitles, 130), 8 (naming, 200), 9 (nfo, 210), 16 (predb_match, 300), 24 (category, 400), 32 (media_parse, 500), 35 (opensubtitles_imdb, 505), 33 (media_lookup, 510), 34 (media_nfo, 520); #51 takes 6 (stills), #64 takes 7 (upload_link, rank 150, Needs::Bodies; branch archive/link.rs:26, 293-295). Free in the body lane: id 3 and ranks 140, 160-199.
4. The `added` columns and new tables are nullable and lazily filled; nothing rewrites existing rows.

### 2.6 What shards and the sealed log forbid

- Per-article data cannot live on `segments`: a file's rows seal into `files.blob` (zstd, `LEVEL = 6`, blob.rs:10) once the file is complete or untouched for `SEAL_AGE = 3 days` (store.rs:788, `sealable` :1082, rule :1136-1139, `seal_file` :826); blob v1 keeps only `Seg {part, bytes, domain, local}` (blob.rs:13-18) and the rows are deleted. A new per-article column would be lost at sealing, so per-article facts (pcrc32, header hashes, arrival numbers) need their own table keyed by `(file_id, part)` or by the message-id hash (`release_ids.id_hash` pattern, store.rs:225-230), or a blob v2 with a format version.
- Reads of sealed files decode the blob (`sealed_segments`, store.rs:815-819; capped by `READ_MAX_SEGMENTS`), which `each_article` already does for `Contents` on every page view, so a render-only derivation over articles costs no more than today's page.
- The id space is fixed (`id % 8`), so every new table is per shard and every cross-release query runs eight times (`groups_of` pattern, release_info.rs:218-227).
- `releases.poster` and `posted_date` are overwritten by later merges (store.rs:1394-1396): nothing derived from them is stable across re-saves.
- Fragments are never purged by the stage; only `atlas --purge` deletes (catalog L1226), so every derived table needs a cheap re-derive when `files.seen` changes (`Reruns.on_growth`).

---

## 3. Pipelines

### 3.1 Ingest

- Per group, `XOVER` ranges are listed (nntp.rs `parse_overview`, fixed 8 columns: number, subject, from, date, message-id, references, bytes, lines; column 9+ ignored; `XFEATURE COMPRESS GZIP` negotiated at login, nntp.rs:297, 397). Kept per article: subject -> `files.subject/filename/part/expected/file_total` via `src/parser.rs` and the ported collection regexes (`src/collections.rs`); from -> `releases.poster`; date -> `releases.posted_date` with the offset stripped (dates.rs `to_iso_date`); message-id -> `segments.local/domain`; bytes -> `segments.bytes` and `releases.size`. Dropped: number (after cursors), references, lines, any extra column, encoded-word decoding (`from_utf8_lossy`).
- Grouping: `Collections::of` keys by group + cleaned subject (store.rs:207 `collections`); a post spread over groups is folded at view time by `split_post_with` (release_info.rs:266), which PR #64 replaces with `upload_link`-based `whole_post_with`.
- A save enqueues `pp_queue(release_id, due_at)` (store.rs:1549); the stage waits `settle_secs = 7200` (or `complete_settle_secs = 900` when complete) before looking (config.rs:622-623).

### 3.2 The postprocess stage

- Steps run in rank order in two lanes: `Needs::Database` steps on the shard writer, `Needs::Bodies`/`External` on the net lane with its own attempts (`pp_state.net_attempts`, `max_attempts = 5`, config.rs:637). `rules::judge_row` rejects blocked groups (rules.rs:104-105), `judge_files` applies completeness, and `body_gate` (rules.rs:199-211) gates bodies by `body_min_size_bytes = 1 MiB`, `body_max_size_bytes = 100 GiB` and `min_completion_percent = 95` (config.rs:631-635). Budgets: `backlog_per_second = 2000`, `body_fetches_per_minute = 600`, `body_connections = 2`, `bodies_per_release = 8` (config.rs:628-634). Archive and probe budgets: `max_bodies_per_release = 4`, `probe_files = 2`, `probes_per_minute = 60`, `max_names_mb = 16384` (config.rs:557-561; clamps :578-581). Media: `details_per_minute = 120`, `negative_ttl_days = 30`, `max_titles = 10M`, TVmaze **on by default** at 90 rpm, TMDB 240, OMDb 30, Trakt 120, TVDB 120, OpenSubtitles 60 (config.rs:234-242; clamps :287-310).
- Walks: `walk_tick` (engine.rs:412-486) takes the newest `pp_walks` row and visits releases `order by r.id desc` (engine.rs:437) below `before_id`, at `backlog_per_second / 8` per shard in batches of `WALK_BATCH = 256` (engine.rs:28, 421-427), running only the steps in `redo_mask` (0 = every applicable step not yet done). `seed_shard` (engine.rs:1073-1114) runs at start: a one-time `backlog` walk, a `steps` walk whenever an enabled step bit was never seen (`pp_steps_seen`), and a `steps` walk with that step's bit whenever a step's `config_stamp` gained a bit (`pp_stamp_<id>`). So a new StepId or a new stamp bit re-runs over every existing release automatically, newest first.
- Manual: `atlas --reprocess --step NAME [--group G] [--failed]` (reprocess.rs:80-93) inserts a `reprocess` walk with the step's mask (reprocess.rs:34-36), resets failed `pp_state` rows (:54) and re-queues (:59-60); it only queues, the indexer does the work (README.md "Reprocess" section). Steps can also request reruns of others (`naming/steps.rs:47` reruns predb), and `Reruns {on_rename, on_growth}` re-run a step when the name changes or more articles arrive.

| Step (file) | StepId / rank | Needs | enabled | applies | reruns | config_stamp |
|---|---|---|---|---|---|---|
| archive_par2 `Par2Step` (archive/steps.rs:185-270) | 1 / 100 | Bodies | `archive.enabled` | not named as an image | on_growth | 0 |
| archive_list `ListStep` (archive/steps.rs:367-430) | 2 / 110 | Bodies | `archive.enabled` | not named as an image | on_growth | 0 |
| probe `ProbeStep` (probe/step.rs:74-101) | 4 / 120 | Bodies | `archive.enabled` | name not ending `.jpg` | on_growth | `1 \| 2` (bit 1: chapters, commentary, more front articles, moviehash; bit 2: nested archives; comment at probe/step.rs:96-98) |
| opensubtitles (probe/opensubtitles.rs:165-191) | 5 / 130 | External | `archive.enabled` and an API key | all | - | 0 |
| naming (naming/steps.rs:22-40) | 8 / 200 | Database | `naming.enabled` | `worth_running` | on_growth | 0 |
| nfo `NfoName` (naming/steps.rs:77-96) | 9 / 210 | Bodies | `archive.enabled && naming.nfo_step` | source <= Nfo and grade < Good | on_growth | 0 |
| predb_match (predb/step.rs:161-179) | 16 / 300 | Database | `predb.enabled` (false by default) | `worth_looking_up` | rename + growth | 0 |
| category (category/step.rs:53-70) | 24 / 400 | Database | `category.enabled` | all | rename + growth | 0 |
| media_parse (media/steps.rs:161-177) | 32 / 500 | Database | `media.parse` | all | rename + growth | 0 |
| opensubtitles_imdb (probe/opensubtitles.rs:243-258) | 35 / 505 | Database | `media.parse` and an API key | - | rename + growth | 0 |
| media_lookup (media/steps.rs:252-274) | 33 / 510 | External | any provider on | title wanted now | rename + growth | `provider_bits` |
| media_nfo (media/steps.rs:311-330) | 34 / 520 | Bodies | `media.nfo && media.parse && body_fetches_per_minute > 0` | movies without a known IMDb id | rename + growth | 0 |

### 3.3 The probe (probe/step.rs:102-215)

- Which release: `applies` excludes `.jpg`; the archive step's `PROBE_MIN_PARTS = 3` (archive/mod.rs:30) and the body gate decide eligibility; `main_file` (probe/step.rs:39-46) is the file with the most parts that is neither PAR2 nor NFO/SFV/TXT, first volume by name (`first_volume_key` :49-53).
- Fetches, in order: the main file's **first article** whole with `ARTICLE` (headers + body; `fetch_raw` :55-62 capped at `ARTICLE_CAP = 4 MiB`, archive/mod.rs:28; the archive head is parsed from the first `PROBE_PREFIX = 64 KiB`, archive/mod.rs:29), **middle** article by `STAT` (nntp.rs:638), **last** article decoded (for `crc32=` and the moviehash tail), `derive` (continuity, sizes, CRC verdicts, overhead), `moviehash_of` (:294-361, through the stored-RAR byte map when needed), `read_more_front` while the Matroska/MP4 header is incomplete up to `FRONT_ARTICLES = 6` (:217, 223-231), then one small text file (NFO/SFV/TXT) and the PAR2 index, each first article, within `probe_files` and `max_bodies_per_release`.
- Keeps: everything in `ProbeInfo` (2.4) plus the indexed `crc32`, `agent`, `moviehash` columns. Drops: every header not in `KEPT_HEADERS` and the header order (probe/mod.rs:96-104), the last article's `pcrc32` and `=yend` fields, yEnc syntax and escape statistics, the `Xref` host token, the response code text, the archive fields past `ArchiveHead` (mtime, host OS, dictionary, salts beyond the RAR5 check), Matroska elements outside Info/Tracks/Chapters/first Cluster, MP4 `udta`, payload bytes.
- Re-run for existing releases: `on_growth`; any new `config_stamp` bit triggers a `steps` walk over every release (3.2); `atlas --reprocess --step probe`. `insert or replace` (probe/mod.rs:51-52) rewrites the whole row, so a re-probe refreshes every field at once. Cost: at the default `probes_per_minute = 60` a release's 2-4 bodies plus STAT take 1-4 slots, so 15M eligible releases need about 174 days at 60 probes a minute and about 17 days at the clamp's 600; README.md:405 gives 78 days at 300 bodies a minute for 1-3 bodies per release and about 14 TB of traffic for the archive steps.

### 3.4 Archive steps (archive/steps.rs; select.rs; par2.rs; rar.rs; sevenz.rs; zip.rs)

- `Par2Step` fetches the PAR2 index's first article, parses Main, FileDesc, UniFileN (par2.rs:61-135; non-recoverable ids merged at par2.rs:96), writes `archive_inner_files` rows (source PAR2), `release_fingerprint.posted` (`twin_fingerprint`: SHA-1 over set id, block size, the posted (name, parts) list and the sorted (name, size, md5) list, duplicates.rs:64-92) and `par2_block/par2_blocks`; MD5s, md5_16k, IFSC, RecvSlic, Creator are dropped.
- `ListStep` picks the first and, when the listing is incomplete, the last volume (`select.rs:45-137`; `Kind::Numbered` for `.001` :36, 70, 104-110; the `NotArchive` extension list :21 covers media, ISO/IMG/BIN, EXE, documents, SFV/SRR/SRS/NZB, subtitles, tar/gz/bz2/xz/ace/cab), reads headers from `PROBE_PREFIX`, writes `archive_inner_files` (up to 64), `archive_release.password/encryption/names_hidden/password_check`, and `release_fingerprint.content` (`content_fingerprint`: SHA-1 of sorted sizes >= 1 MiB, samples skipped, duplicates.rs:173). Budget: `bodies_per_release = 8` across the body lane, `body_fetches_per_minute = 600`.
- Twins and alts: `copies_with` (duplicates.rs:224) joins `posted` (TWIN) and `content` (ALT); `forget_old_twins` (:97) clears fingerprints older than `TWIN_VERSION`; `CLEAR_CONTENT` (:53) is reused by #64.
- Re-run: `on_growth`; `--reprocess --step archive_par2|archive_list`; no stamp bits today, so a new PAR2 field for existing releases needs either a stamp bit on `Par2Step` (automatic walk) or the manual reprocess. About 48% of posts carry PAR2 (coordination.md:27), so a PAR2-only backfill is roughly 7M single-article fetches (about 8 days at 600 a minute).

### 3.5 Media title resolution and details (media/*)

`media_parse` (Database) parses the name with `media::parse` (parse.rs; aliases from `data/tv_title_aliases.tsv`) into `media_release` (kind, title_key, season, episode, year, quality); `media_lookup` (External) asks TVmaze/TMDB/OMDb/Trakt/TVDB through `resolve.rs` with a shared token per provider (http.rs), caches answers in `media_titles` (negative TTL 30 days doubling to 180, resolve.rs:106, 181-183), and copies ids onto `media_release`; `media_nfo` fetches an NFO for movies without an IMDb id; `opensubtitles_imdb` copies the moviehash match. `details::start_backfill` (media/details.rs:673) fills `details` for known titles in the background; `details_panel` fills on demand. Re-run: `config_stamp = provider_bits` re-runs `media_lookup` when a provider is switched on; a wrong match is corrected by deleting the `media_titles` row and `--reprocess --step media_lookup` (README.md:757). Provider budgets make a full re-lookup of 1M titles take 3-6 days (media-title-resolution.md §8-9).

### 3.6 Naming, category, PreDB

Naming (Database, rank 200) picks a display name by evidence priority (`NameSource`), the NFO step (Bodies, 210) reads an NFO when the name is still poor; `release_tags`/PTT parse happens at render only and stores nothing (release_tags.rs:1-3). Category (Database, 400) writes `release_category`. PreDB (Database, 300, off by default) matches title + size and md5/sha1-named posts against the 33 feeds in `atlas.predb.db` and writes `predb_match`; nuke state and pre time are stored but never shown.

### 3.7 Re-running anything for existing releases: the menu

| Mechanism | Trigger | Order and rate | Cost over the catalog |
|---|---|---|---|
| New `StepId` registered | automatic `steps` walk at start (engine.rs:1096-1098) | newest first, `backlog_per_second/8` per shard | Database lane: 118M rows at 2000/s = 16.4 h of walking plus the step's writes; body lane: bounded by `body_fetches_per_minute` and `probes_per_minute` |
| New `config_stamp` bit | automatic `steps` walk with that step's mask (engine.rs:1103-1110) | same | same; the probe's two bits already set by #52 mean a full re-probe pass is already queued or running on a live deploy |
| `atlas --reprocess --step X [--group]` | manual; dry-run by default convention (docs/port/00-overview.md:56) | same walk machinery | same |
| `Reruns.on_growth` / `on_rename` | per release, when articles or the name change | queue, not walk | marginal |
| Render-only change | none | - | zero |
| Side-DB cache delete + restart | manual (README.md:757) | provider rate | 1M titles 3-6 days |

---

## 4. Visibility matrix

Every YAML item of the catalog (293) plus one row for the posting-tool table (catalog L639-665), in catalog order with the catalog's own ids and line numbers. Layer = the catalog block the item sits in.

Legend for the last column: **N0** nothing (already on the page for every release that has the data); **N1** render only (read what the shards or the probe JSON already hold; no backfill); **N2** store then render (a Needs::Database step or walk: 118M releases at backlog_per_second 2000 over 8 shards is about 16.4 h, src/postprocess/engine.rs:421, README.md:405 counts; about 12 GB of WAL per flag column, README.md:512); **N3** new fetch then store then render, riding on the articles the probe or archive steps already read (a ProbeInfo field or KEPT_HEADERS name plus a config_stamp bit; backfill = re-probe of every probed release: about 15M body-gate releases, README.md:405 gives 78 days at 300 bodies a minute and 4 days at 6,000, so 17-70 days at the default 600 and about 174 days at probes_per_minute 60); **N4** new articles beyond today's probe set (volume tails, mid-file, first cluster): new releases only, the backlog is out of reach (35 GB per 1,000 releases for volume tails, catalog L1208); **N5** an XOVER column that is already on the wire but dropped: store for new ingest only, no backfill (sealed blob v1 keeps part, bytes, domain, local only, src/blob.rs:13-18, and expired articles cannot be re-listed); **N6** a new NNTP command or external service; **NA** not a per-release fact (server, group, tool, downloader or prior-art note).

| # | Catalog item (block.id, catalog line) | Layer | Status | Evidence | What showing it on every release needs |
|---|---|---|---|---|---|
| 1 | `capabilities` (L30) | nntp_session | **parsed_not_stored** | CAPABILITIES is never sent; XFEATURE COMPRESS GZIP goes out at login and a 290 sets a per-connection flag (src/nntp.rs:297, 397); the only commands sent are AUTHINFO, XFEATURE, GROUP, XOVER, LIST, ARTICLE, BODY, STAT (src/nntp.rs:586, 615, 638; catalog L33, L273) | NA: a server fact, not a release fact; a status page could show it after a CAPABILITIES per connection (N6) |
| 2 | `greeting` (L34) | nntp_session | **not_collected** | banner text is not kept anywhere (catalog L36; no reader found a store) | NA (server fact); N6 then a status page |
| 3 | `server_clock` (L37) | nntp_session | **not_collected** | DATE is never sent (catalog L39; src/nntp.rs command list) | NA (server fact); N6 |
| 4 | `group_counts` (L40) | nntp_session | **stored_not_shown** | GROUP low/high drive cursors and are stored per group and server as groups.first_article/last_article (src/db.rs:328-342 groups table, save_group_bounds); the count is read and dropped, LISTGROUP unused (catalog L42) | NA for a release page (group fact); N1 on a group page: join releases.group_name to groups |
| 5 | `group_metadata` (L43) | nntp_session | **parsed_not_stored** | LIST ACTIVE names and high/low feed the group picker's 10-minute cache (src/groups_menu.rs); the status flag column is dropped; LIST NEWSGROUPS and ACTIVE.TIMES never sent (catalog L45) | NA (group fact); N2 for the flag (a groups column), N6 for descriptions |
| 6 | `overview_format` (L46) | nntp_session | **not_collected** | parse_overview assumes the fixed 8 columns and ignores column 9+ (src/nntp.rs parse_overview; catalog L48) | NA (server fact); N6 (LIST OVERVIEW.FMT once per server) |
| 7 | `article_presence` (L49) | nntp_session | **shown** | release_probe.info.health {first, middle, last} -> 'On the server' FIRST THERE / GONE tags (src/web.rs:1278-1289; src/probe/step.rs:65-72 there(), 161-181); one server, undated, re-run only on growth or a stamp bit | N0 for probed releases; per provider and dated history = N6 (a release_health table, catalog L1192) with STATs only |
| 8 | `retention_horizon` (L52) | nntp_session | **stored_not_shown** | Pool::retention samples 3 windows of 100 overview rows per server and stores backfill_deepest.oldest_at/runner_up_at and groups.low_posted (src/chunks.rs:29-33; src/db.rs); the stats TUI shows 'backfill reached', the web UI nothing (catalog L54) | NA for a release page; N1 on a group or status page |
| 9 | `new_news` (L55) | nntp_session | **not_collected** | NEWNEWS and NEWGROUPS are never sent (catalog L57) | NA; N6 |
| 10 | `transfer_facts` (L58) | nntp_session | **parsed_not_stored** | in-memory Prometheus counters only: commands by outcome, seconds, bytes, failures by kind (src/metrics/names.rs); no TLS details, nothing in SQLite (catalog L60) | NA (connection fact); the /metrics endpoint already exposes the counters |
| 11 | `number` (L63) | overview_row | **parsed_not_stored** | article numbers drive cursors and day chunks (src/indexer.rs xover_ends, src/chunks.rs); segments has no per-article number column (src/store.rs:181-188; catalog L66) | N5: keep (server, group, number) per segment for new ingest in a side table; backfill impossible (sealed blob v1 holds part, bytes, domain, local only: src/blob.rs:13-18) |
| 12 | `subject` (L67) | overview_row | **stored_not_shown** | files.subject stores the first article's Subject per file (src/store.rs:165-177); it is written only into the NZB (src/nzb.rs:83-88); the page prints names.posted (the release name as posted), never files.subject (src/web.rs:1087-1090) | N1: a per-file list (name, subject, parts) read from files; PR #48 adds an NZB file list (name, size, parts) that this must not duplicate (coordination.md:6) |
| 13 | `from` (L71) | overview_row | **shown** | releases.poster (src/store.rs:133-144, overwritten by every merge: on conflict ... poster = excluded.poster src/store.rs:1394-1396) -> Poster and Email facts with the MADE UP tag (src/web.rs:1079-1085; parse_poster src/release_info.rs:451, looks_made_up :501); searchable by poster and email | N0; per-article Froms would be N5 (new ingest) + a side table |
| 14 | `date` (L75) | overview_row | **shown** | Posted fact from releases.posted_date (src/web.rs:1067; fmt_date src/ui.rs:531), the saved batch's first article with the TZ offset stripped (src/dates.rs to_iso_date); the probed article's raw Date, Injection-Date and NNTP-Posting-Date are printed verbatim under 'How it was posted' (src/web.rs:1294-1297; KEPT_HEADERS src/probe/mod.rs:81-94) | N0; first/last Date per file = N5 (new ingest only) |
| 15 | `message_id` (L79) | overview_row | **stored_not_shown** | segments(local BLOB, domain) with a shared domains table (src/store.rs:180-188; pack_local alphabet tag src/store.rs:311-404); read back only for the NZB and each_article (src/store.rs:1947); the domain is never surfaced or searched | N1: print the main file's first id or its domain from each_article (sealed files decode from files.blob: src/store.rs:815-819) |
| 16 | `references` (L83) | overview_row | **parsed_not_stored** | parsed into Overview and Article and dropped before store (src/nntp.rs parse_overview; catalog L86) | N5: column already on the wire; store for new ingest only; backfill impossible for sealed and expired articles |
| 17 | `bytes` (L87) | overview_row | **shown** | segments.bytes per article (src/store.rs:181-188), summed into releases.size -> Size fact (src/web.rs:1061-1066, owned by the download-size thread PR #64 coordination.md:16-17); per-article bytes only in the NZB (src/nzb.rs) | N0 for the total; per-article values N1 (each_article) |
| 18 | `lines` (L91) | overview_row | **parsed_not_stored** | parse_overview drops :lines (src/nntp.rs; catalog L94, L632) | N5: new ingest only (a column on a new per-article table); backfill impossible |
| 19 | `xref` (L95) | overview_row | **shown** | ignored in XOVER; taken from the probe's first article fetched whole with ARTICLE (Atlas never sends HEAD): release_probe.info.crossposted_to -> 'Crossposted to' group links (src/web.rs:1297-1300; xref_groups src/probe/mod.rs:112-114); the raw Xref header is kept but filtered from the header list (src/web.rs:1295); the server host token is dropped | N0 for probed releases; per-article Xref from XOVER column 9 = N5 after LIST OVERVIEW.FMT (catalog rank 2, L1150) |
| 20 | `extra_columns` (L99) | overview_row | **not_collected** | columns past 8 are ignored (src/nntp.rs parse_overview; catalog L102) | N5 plus LIST OVERVIEW.FMT (N6) |
| 21 | `Date (header)` (L105) | headers_mandatory | **shown** | see overview_row.date: raw header printed verbatim from release_probe.info.headers (src/web.rs:1294-1297) | N0 |
| 22 | `From (header)` (L106) | headers_mandatory | **shown** | see overview_row.from (src/web.rs:1079-1085) | N0 |
| 23 | `Message-ID (header)` (L107) | headers_mandatory | **stored_not_shown** | see overview_row.message_id (src/store.rs:181-188) | N1 |
| 24 | `Subject (header)` (L108) | headers_mandatory | **stored_not_shown** | see overview_row.subject (files.subject; src/nzb.rs:83-88) | N1 |
| 25 | `Newsgroups` (L109) | headers_mandatory | **not_collected** | not in KEPT_HEADERS (src/probe/mod.rs:81-94), so dropped from the probe's ARTICLE read (catalog L111) | N3: add to KEPT_HEADERS + a config_stamp bit, render under 'How it was posted'; backfill = re-probe |
| 26 | `Path` (L112) | headers_mandatory | **shown** | kept verbatim (512 chars) in release_probe.info.headers and printed raw (src/probe/mod.rs:88, :100; src/web.rs:1294-1297); not parsed into hops (catalog L114) | N0 raw; parsed hops and the injecting host = N1 (parse at render) |
| 27 | `Injection-Date` (L117) | headers_optional_netnews | **shown** | KEPT_HEADERS (src/probe/mod.rs:82); printed raw (src/web.rs:1294-1297); never compared with Date (catalog L119) | N0 raw; the Date minus Injection-Date skew = N1 |
| 28 | `Injection-Info` (L120) | headers_optional_netnews | **shown** | KEPT_HEADERS (src/probe/mod.rs:89); printed raw, unparsed (src/web.rs:1294-1297; catalog L122) | N0 raw; posting-account / posting-host / logging-data split out = N1 (parse at render) or N2 for indexed columns (catalog rank 5) |
| 29 | `Organization` (L123) | headers_optional_netnews | **shown** | KEPT_HEADERS (src/probe/mod.rs:90); printed raw (src/web.rs:1294-1297) | N0 |
| 30 | `User-Agent` (L126) | headers_optional_netnews | **shown** | release_probe.agent = User-Agent, else X-Newsreader, else X-Newsposter (agent_of src/probe/mod.rs:107-109) -> 'Posted with' search link (src/web.rs:1292-1294); indexed (release_probe_agent src/probe/mod.rs:39-42) | N0 |
| 31 | `Xref (header)` (L129) | headers_optional_netnews | **shown** | see overview_row.xref (crossposted_to; the raw header line is suppressed at src/web.rs:1295) | N0 |
| 32 | `Approved` (L131) | headers_optional_netnews | **not_collected** | not in KEPT_HEADERS (src/probe/mod.rs:81-94) | N3 |
| 33 | `Archive` (L134) | headers_optional_netnews | **not_collected** | not in KEPT_HEADERS | N3 |
| 34 | `Control` (L137) | headers_optional_netnews | **not_collected** | not in KEPT_HEADERS; cancels never seen (binaries are read by XOVER only) | N3 (one article) or N6 for cancel tracking |
| 35 | `Supersedes` (L140) | headers_optional_netnews | **not_collected** | not in KEPT_HEADERS | N3 |
| 36 | `Distribution` (L143) | headers_optional_netnews | **not_collected** | not in KEPT_HEADERS | N3 |
| 37 | `Expires` (L146) | headers_optional_netnews | **not_collected** | not in KEPT_HEADERS | N3 |
| 38 | `Followup-To` (L149) | headers_optional_netnews | **not_collected** | not in KEPT_HEADERS | N3 |
| 39 | `Summary` (L152) | headers_optional_netnews | **not_collected** | not in KEPT_HEADERS | N3 |
| 40 | `Lines (header)` (L155) | headers_optional_netnews | **not_collected** | not in KEPT_HEADERS; the overview :lines is dropped too (src/nntp.rs) | N3 for the header, N5 for the overview column |
| 41 | `Bytes (header)` (L158) | headers_optional_netnews | **not_collected** | not in KEPT_HEADERS | N3 |
| 42 | `Cancel-Lock` (L161) | headers_optional_netnews | **not_collected** | not in KEPT_HEADERS | N3 |
| 43 | `Cancel-Key` (L164) | headers_optional_netnews | **not_collected** | not in KEPT_HEADERS (only on control articles, which Atlas never reads) | N6 |
| 44 | `References (header)` (L169) | headers_mail_and_mime | **parsed_not_stored** | see overview_row.references (dropped before store) | N5 |
| 45 | `In-Reply-To` (L171) | headers_mail_and_mime | **not_collected** | not in KEPT_HEADERS (src/probe/mod.rs:81-94) | N3 |
| 46 | `Reply-To` (L174) | headers_mail_and_mime | **not_collected** | not in KEPT_HEADERS | N3 |
| 47 | `Sender` (L177) | headers_mail_and_mime | **not_collected** | not in KEPT_HEADERS | N3 |
| 48 | `Comments, Keywords` (L180) | headers_mail_and_mime | **not_collected** | not in KEPT_HEADERS | N3 |
| 49 | `MIME-Version` (L183) | headers_mail_and_mime | **not_collected** | not in KEPT_HEADERS; no MIME body decoder (catalog L389-396) | N3 |
| 50 | `Content-Type` (L186) | headers_mail_and_mime | **not_collected** | not in KEPT_HEADERS | N3 |
| 51 | `Content-Transfer-Encoding` (L189) | headers_mail_and_mime | **not_collected** | not in KEPT_HEADERS | N3 |
| 52 | `Content-Disposition` (L192) | headers_mail_and_mime | **not_collected** | not in KEPT_HEADERS | N3 |
| 53 | `Content-Description, Content-ID, Content-Length` (L195) | headers_mail_and_mime | **not_collected** | not in KEPT_HEADERS | N3 |
| 54 | `NNTP-Posting-Host` (L200) | headers_server_injected | **shown** | KEPT_HEADERS (src/probe/mod.rs:92); printed raw (src/web.rs:1294-1297) | N0 (privacy: an IP or its hash; keep raw only, never index without hashing) |
| 55 | `NNTP-Posting-Date` (L203) | headers_server_injected | **shown** | KEPT_HEADERS (src/probe/mod.rs:83); printed raw | N0 |
| 56 | `X-Trace` (L206) | headers_server_injected | **not_collected** | not in KEPT_HEADERS (catalog L208) | N3 |
| 57 | `X-Complaints-To` (L209) | headers_server_injected | **shown** | KEPT_HEADERS (src/probe/mod.rs:93); printed raw | N0 |
| 58 | `X-Abuse-Info, X-Abuse-and-DMCA-Info, X-DMCA-Notifications, X-DMCA-Complaints-To` (L212) | headers_server_injected | **not_collected** | not in KEPT_HEADERS | N3 |
| 59 | `X-Received-Bytes, X-Received-Body-CRC` (L215) | headers_server_injected | **not_collected** | not in KEPT_HEADERS (catalog L217); the body CRC would be a free integrity check after BODY | N3; as a twin key N2 after storing (indexed column) |
| 60 | `X-Original-Bytes, X-Original-Lines` (L218) | headers_server_injected | **not_collected** | not in KEPT_HEADERS | N3 |
| 61 | `X-Postfilter, X-Spam-Status, X-Spam-Score, X-Antivirus, X-Antivirus-Status` (L221) | headers_server_injected | **not_collected** | not in KEPT_HEADERS | N3 |
| 62 | `X-Ref, X-Server-Date` (L224) | headers_server_injected | **not_collected** | not in KEPT_HEADERS | N3 |
| 63 | `X-Newsreader, X-Newsposter, X-Mailer` (L229) | headers_poster_tool | **shown** | X-Newsreader and X-Newsposter fold into release_probe.agent -> 'Posted with' (src/probe/mod.rs:85-86, 107-109; src/web.rs:1292-1294); X-Mailer is not kept (catalog L231) | N0; X-Mailer = N3 |
| 64 | `X-No-Archive` (L232) | headers_poster_tool | **not_collected** | not in KEPT_HEADERS | N3 |
| 65 | `X-Face, Face` (L235) | headers_poster_tool | **not_collected** | not in KEPT_HEADERS | N3 |
| 66 | `X-Priority, X-MSMail-Priority, Importance, Precedence` (L238) | headers_poster_tool | **not_collected** | not in KEPT_HEADERS | N3 |
| 67 | `X-XML, X-XML-Signature, X-User-Key, X-User-Signature, X-User-Avatar` (L241) | headers_poster_tool | **not_collected** | no 'spot', 'Spotnet' or 'X-XML' anywhere in src/ (catalog L243, L305) | N3 for the headers of the one probed article; Spotnet spots live in other groups and need N6 (a spot reader) |
| 68 | `any other X-*` (L244) | headers_poster_tool | **not_collected** | keep_headers keeps only the 12 names in KEPT_HEADERS; every other name, and the order (BTreeMap), is dropped (src/probe/mod.rs:81-104; catalog L247) | N3: keep the whole header block (a text column or JSON list) + stamp bit |
| 69 | `header_order` (L249) | header_shape | **not_collected** | headers land in a BTreeMap, so the arrival order is lost (src/probe/mod.rs:96-104) | N3: store the ordered name list or its hash |
| 70 | `header_count_and_bytes` (L252) | header_shape | **not_collected** | nothing counts headers or header bytes (catalog L254) | N3 |
| 71 | `folding_and_case` (L255) | header_shape | **not_collected** | keep_headers matches names case-insensitively and normalises to the KEPT_HEADERS spelling (src/probe/mod.rs:99-100), so casing is lost | N3 |
| 72 | `date_format_quirks` (L258) | header_shape | **not_collected** | no property is derived; the raw Date string is visible under 'How it was posted' (src/web.rs:1294-1297) so a reader can see the quirks | N1: derive day name, 2-digit year, zone form from the stored raw header at render |
| 73 | `from_shape` (L261) | header_shape | **parsed_not_stored** | parse_poster splits name and address when a page renders (src/release_info.rs:451-499; src/web.rs:1079-1085); nothing about the shape is stored (catalog L263) | N1: classify the shape at render from releases.poster |
| 74 | `hdr_xhdr_bulk_headers` (L270) | nntp_additions | **not_collected** | HDR/XHDR/XPAT are never sent (src/nntp.rs commands; catalog L273) | N6: a new ingest command; per-article header columns in a side table |
| 75 | `response_code_signals` (L274) | nntp_additions | **parsed_not_stored** | 420/423/430 collapse into one metric label (no_such_article) and the text is dropped (src/metrics/names.rs; catalog L277); per release only the health bool survives (src/probe/step.rs:65-72) | N3: keep the code and text per STAT in release_probe.info.health |
| 76 | `article_number_portability` (L278) | nntp_additions | **not_collected** | numbers are used for cursors and dropped (catalog L281) | N5 (new ingest) into a per-article side table |
| 77 | `server_assigned_message_ids` (L282) | nntp_additions | **stored_not_shown** | stored like any message-id in segments (src/store.rs:181-188); not decoded (catalog L285) | N1: decode the grammar at render (INN, Highwinds, Diablo shapes) from each_article |
| 78 | `encoded_words_and_charset` (L286) | nntp_additions | **not_collected** | parse_overview takes every column with from_utf8_lossy and nothing decodes RFC 2047 words (src/nntp.rs; catalog L289) | N5 for the charset flag; N1 to decode encoded words in the stored subject/poster at render |
| 79 | `mime_part_fields` (L290) | nntp_additions | **not_collected** | not in KEPT_HEADERS (src/probe/mod.rs:81-94) | N3 |
| 80 | `x_nxg` (L294) | nntp_additions | **not_collected** | not in KEPT_HEADERS | N3 |
| 81 | `x_usenet_provider_and_backbone_tokens` (L298) | nntp_additions | **not_collected** | X-Ufk, X-Ufhash, X-Usenet-Provider are not in KEPT_HEADERS; the Highwinds Path token is visible only inside the raw Path string (src/web.rs:1294-1297) | N3 for the headers; N1 to name the backbone from the stored Path |
| 82 | `spotnet_spot` (L302) | nntp_additions | **not_collected** | no spot parsing anywhere in src/ (catalog L305) | N6: a spot reader for alt.binaries.ftd / free.pt, joined to releases by the NZB message-ids |
| 83 | `availability_prediction` (L306) | nntp_additions | **not_collected** | the three probe STATs are shown once as 'On the server' (src/web.rs:1278-1289) and never repeated (catalog L309); no co-removal signal is derived | N6: a scheduled STAT history (release_health) then N1 |
| 84 | `name` (L318) | ybegin | **shown** | release_probe.info.yenc.name -> 'File name' in the yEnc panel (src/web.rs:1252; YencLines src/probe/yenc.rs:8); main file's first article only; from_utf8_lossy, so non-UTF-8 bytes are already U+FFFD (catalog L321) | N0 for the main file; every file = N4 (first article per file) |
| 85 | `size` (L322) | ybegin | **shown** | yenc.file_size -> 'File size' (src/web.rs:1253-1259) | N0 |
| 86 | `line` (L325) | ybegin | **shown** | yenc.line_len -> 'Line length' (src/web.rs:1261) | N0 |
| 87 | `part` (L328) | ybegin | **stored_not_shown** | yenc.part is in the JSON (src/probe/yenc.rs:11) and not rendered; only total is printed as 'Parts' (src/web.rs:1260); segments.part comes from the Subject, not from yEnc (catalog L330) | N1 |
| 88 | `total` (L331) | ybegin | **shown** | yenc.total -> 'Parts' (src/web.rs:1260); compared with the Subject total in probe::derive -> continuity_ok verdict (src/web.rs:1270-1274) | N0 |
| 89 | `begin` (L336) | ypart | **stored_not_shown** | yenc.begin in the JSON (src/probe/yenc.rs:13); derives segment_size ('Article size' src/web.rs:1262) and continuity_ok | N1 |
| 90 | `end` (L339) | ypart | **stored_not_shown** | yenc.end in the JSON (src/probe/yenc.rs:14); end - begin + 1 is the shown 'Article size' | N1 |
| 91 | `size` (L344) | yend | **stored_not_shown** | yenc.yend_size in the JSON (src/probe/yenc.rs:15); the part_size_ok verdict ('PART SIZE OK') is shown (src/web.rs:1272); the last article's value is parsed and dropped | N1 |
| 92 | `part` (L347) | yend | **not_collected** | yenc::parse reads only size=, pcrc32= and crc32= from =yend; the part number comes from =ybegin (src/probe/yenc.rs:20-23; catalog L349) | N3: parse one more key; a stamp bit re-probes |
| 93 | `pcrc32` (L350) | yend | **stored_not_shown** | the first article's pcrc32 sits in yenc.pcrc32 (src/probe/yenc.rs:16) and is verified as part_crc_ok ('PART CRC OK' src/web.rs:1273); the value is not printed; the last article is decoded but unchecked; nothing per article (catalog L353) | N1 for the first article; per-article CRCs = a segment_crc table fed by the articles Atlas already decodes (catalog rank 1): N3 for probed releases, N4 for every article |
| 94 | `crc32` (L354) | yend | **shown** | release_probe.crc32 (indexed, src/probe/mod.rs:31-34) from the last part else the first -> 'CRC32' search link (src/web.rs:1263-1265); main file only | N0; other files = N4 (their last article) |
| 95 | `escape_set` (L359) | encoding_quirks | **not_collected** | yenc_decode decodes correctly and records nothing about the escapes seen (src/nntp.rs yenc_decode; catalog L361) | N3 (statistics from the articles the probe already decodes) |
| 96 | `line_length_variance` (L362) | encoding_quirks | **not_collected** | not measured (catalog L364) | N3 |
| 97 | `line_endings` (L365) | encoding_quirks | **not_collected** | not measured (catalog L367) | N3 |
| 98 | `segment_size` (L368) | encoding_quirks | **shown** | release_probe.info.segment_size -> 'Article size' (src/web.rs:1262; ProbeInfo src/probe/mod.rs:59) | N0 |
| 99 | `overhead` (L371) | encoding_quirks | **shown** | release_probe.info.overhead_percent -> 'Overhead' (src/web.rs:1266-1268) | N0 |
| 100 | `preamble_and_trailer` (L374) | encoding_quirks | **not_collected** | decoders skip text outside the =y lines (catalog L376) | N3 |
| 101 | `control_line_syntax` (L377) | encoding_quirks | **not_collected** | yenc::parse normalises to values; the raw control lines are not kept (src/probe/yenc.rs) | N3 |
| 102 | `part_numbering` (L380) | encoding_quirks | **shown** | continuity_ok compares Subject and yEnc totals and offsets -> 'ALL BYTES COVERED' / 'BYTES MISSING' (src/web.rs:1270-1274; ProbeInfo.continuity_ok src/probe/mod.rs:61) | N0 (as a verdict); the raw disagreement = N1 from the stored fields |
| 103 | `filename_mismatch` (L383) | encoding_quirks | **stored_not_shown** | the three names are stored side by side: info.file (Subject name, not rendered), yenc.name ('File name'), archive.name ('First file') (src/probe/mod.rs:55-56, 68; src/web.rs:1252, 1319); never compared (catalog L385) | N1: compare the three stored names at render and print an obfuscation-layer line |
| 104 | `uuencode` (L388) | non_yenc_bodies | **not_collected** | yenc_decode returns None without =ybegin and callers fall back to the raw lines; no uu decoder (catalog L390) | N6-class work: a decoder in src/nntp.rs, then N3 |
| 105 | `base64_mime` (L391) | non_yenc_bodies | **not_collected** | no MIME decoder; MIME headers not kept | same as uuencode |
| 106 | `xxencode_binhex` (L394) | non_yenc_bodies | **not_collected** | no decoder (catalog L396) | same, low value |
| 107 | `plain_text` (L397) | non_yenc_bodies | **shown** | NFO/SFV/TXT bodies are read whether yEnc or bare text (src/probe/step.rs is_small_text :34-37; nfo_text src/probe/mod.rs:150) -> NFO <pre> and SFV table (src/web.rs:1425-1444) | N0 for NFO/SFV/TXT; other text files = N4 |
| 108 | `magic` (L402) | decoded_payload | **shown** | archive formats -> 'Format' in Packing (src/web.rs:1304-1306; ArchiveHead.format src/probe/archive.rs:14), containers -> 'Container' in Tracks (src/web.rs:1330), compressed layers -> 'Layers' (src/web.rs:1322-1325; nest::Layer src/probe/nest.rs:25-39); image, AVI, MPEG-PS, FLAC, Ogg, ID3 magics only steer the archive step (src/archive/mod.rs identify; catalog L404) | N0 for archives, MKV, MP4 and layers; the steering-only magics = N3 (record the detected type) |
| 109 | `first_16k_md5` (L405) | decoded_payload | **parsed_not_stored** | src/par2.rs reads md5_16k into Par2File and nothing stores it (twin_fingerprint hashes the full md5 list only: src/duplicates.rs:64-92); nothing hashes the payload's first 16 KiB (catalog L407) | N2-class for the PAR2 value (a par2_files table filled by the Par2Step's existing read = N3 re-run of the PAR2 step); payload hash = N3 in the probe |
| 110 | `full_file_crc32_md5` (L408) | decoded_payload | **shown** | yEnc crc32 ('CRC32' src/web.rs:1263-1265), the first inner file's RAR/ZIP CRC ('File CRC32' src/web.rs:1321) and every SFV entry (src/web.rs:1433-1444) are shown; PAR2 MD5s are folded into release_fingerprint.posted and not stored (src/duplicates.rs:64-92); nothing reconciles them (catalog L410) | N0 for what is shown; per-file MD5 and CRC = par2_files (N3)  |
| 111 | `opensubtitles_moviehash` (L411) | decoded_payload | **shown** | release_probe.moviehash (indexed, src/probe/mod.rs:43-46) -> 'OpenSubtitles hash' and the match line (src/web.rs:1398-1414; release_opensubtitles src/probe/opensubtitles.rs:27-48) | N0 |
| 112 | `encoder_fingerprint_tuple` (L420) | yenc_additions | **not_collected** | yenc_decode records nothing about the control-line syntax (catalog L423) | N3 |
| 113 | `first_article_file_key` (L424) | yenc_additions | **stored_not_shown** | its inputs are all in the probe JSON for the main file: yenc.file_size, yenc.pcrc32, segment_size (src/probe/yenc.rs:9, 16; src/probe/mod.rs:59); the tuple itself is never built or indexed (catalog L427) | N1 to print; N2 (indexed column) to use as a key |
| 114 | `crc32_interval_algebra` (L428) | yenc_additions | **not_collected** | a plain crc32 table in src/archive/mod.rs, no combine (catalog L431) | N3 plus per-part CRCs (segment_crc) and a crc32fast combine |
| 115 | `node_yencode_issue_3` (L432) | yenc_additions | **not_collected** | no escape statistics (catalog L435) | N3 |
| 116 | `uuencode_frame` (L436) | yenc_additions | **not_collected** | no uu/xx/BinHex decoder exists (catalog L439) | decoder first, then N3 |
| 117 | `decoder_caveats` (L440) | yenc_additions | **not_collected** | a note about third-party decoders; Atlas has its own (src/nntp.rs yenc_decode); nothing per release exists or is wanted | NA |
| 118 | `late_article_join_key` (L444) | yenc_additions | **not_collected** | late articles join only through the Subject-derived file key (src/collections.rs; src/store.rs held_back :211-219; catalog L447) | N5-class ingest change; not a display item |
| 119 | `v8_random_state_recovery` (L448) | yenc_additions | **not_collected** | nothing of the kind in src/ (catalog L451) | N1 in principle (ids are stored) but needs a solver; not a display item |
| 120 | `encoder_tells` (L452) | yenc_additions | **not_collected** | yenc_decode verifies pcrc32 and drops the formatting (catalog L455) | N3 |
| 121 | `subject_parse` (L464) | derived_per_article | **shown** | the release name is parsed at render by release_tags::parse (src/release_tags.rs:88; src/web.rs:1053) into Title, Year, Ripped from, Site, Video, Audio, Languages, Edition, Release group (src/web.rs:1057-1121) and tags (name_tags src/web.rs:720-732); per-file parse (filename, part, total, file_index) feeds files.filename/expected/file_total (src/parser.rs; src/store.rs:165-177) and the Files / Parts / Articles counts; size and poster tags are not parsed (catalog L466) | N0; the posted file list = PR #48 (coordination.md:6) |
| 122 | `subject_template` (L467) | derived_per_article | **parsed_not_stored** | subject_base strips counters for the collection key (src/collections.rs); the template is not stored (catalog L469) | N1 to show a skeleton of files.subject at render; N2 to search on it |
| 123 | `part_bitmap` (L470) | derived_per_article | **shown** | files.seen bitmap and files.expected (src/store.rs:165-177) -> Articles 'x of y' with ALL THERE / REPAIRABLE / NOT ENOUGH RECOVERY (src/web.rs:1135-1137; health :1779-1787); duplicate parts and parts past total are dropped at parse and not counted (catalog L472) | N0 for the counts; a per-part map = N1 (each_article) |
| 124 | `message_id_grammar` (L473) | derived_per_article | **stored_not_shown** | local and domain stored (segments, domains src/store.rs:180-188) with an alphabet class byte from pack_local (src/store.rs:311-404); no grammar classification, no search on domain (catalog L475) | N1 to classify the main file's ids at render; N2 for a searchable tool column (catalog rank 6) |
| 125 | `message_id_timestamp` (L476) | derived_per_article | **not_collected** | not decoded (catalog L478); the local part is stored losslessly, so Nyuu / GoPostStuff / newsmangler timestamps are recoverable | N1 (decode at render from stored ids) |
| 126 | `from_identity` (L479) | derived_per_article | **shown** | releases.poster -> Poster, Email, MADE UP (src/web.rs:1079-1085; src/release_info.rs:451-508); one From per release (the last save's first article, src/store.rs:1394-1396); constant-vs-random per article is not knowable without per-article Froms | N0 for the raw identity; per-article variation = N5 |
| 127 | `date_shape` (L482) | derived_per_article | **shown** | the probed article's raw Date, Injection-Date and NNTP-Posting-Date are printed verbatim (src/web.rs:1294-1297); no property is derived (catalog L484) | N0 raw; derived properties N1 |
| 128 | `bytes_lines_ratio` (L485) | derived_per_article | **not_collected** | :lines is dropped (src/nntp.rs parse_overview) | N5 |
| 129 | `article_number_adjacency` (L488) | derived_per_article | **not_collected** | numbers are used for cursors only (catalog L490) | N5 |
| 130 | `posting_rate` (L491) | derived_per_article | **not_collected** | no per-article dates or numbers survive (catalog L493) | N5 then N1 |
| 131 | `arrival_curve` (L494) | derived_per_article | **parsed_not_stored** | xover_ends dates a number range to cut day chunks and drops the result; backfill_chunks keeps group, day, state and server (src/chunks.rs:20-28); nothing joins groups by time (catalog L496) | NA for a release page; N2 (store the number-to-time map per group) |
| 132 | `crosspost_kind` (L497) | derived_per_article | **shown** | groups_of lists every release with the same posted name per group with article counts -> 'Groups' fact (src/release_info.rs:212-236; src/web.rs:1069-1078); 'Crossposted to' from the probe's Xref (src/web.rs:1297-1300); twins via release_fingerprint.posted in the copies panel (src/web.rs:1171-1181) | N0 |
| 133 | `contents_profile` (L502) | derived_per_post | **shown** | release_info::Contents is computed per view from each_article (src/release_info.rs:39-60, 152-201) -> Files, PAR2 files (n recovery), Articles, Repair (src/web.rs:1129-1138); PAR2 block size and count stored in release_fingerprint.par2_block/par2_blocks (src/duplicates.rs:40) feed exact_repair_per_mille; volume size, file-type mix and posting order are not kept (catalog L504) | N0 for the counts; volume size and type mix = N1 (from files.filename and segments.bytes); posting order = N5 |
| 134 | `obfuscation_level` (L505) | derived_per_post | **shown** | OBFUSCATED tag from names.obfuscated (releases.is_obfuscated, src/naming/obfuscation.rs; src/web.rs:1043-1045), 'Name from' (name_source_label src/release_info.rs:24-35), Encryption and Password facts (src/web.rs:1125-1128); no graded level (catalog L507) | N1: derive a level from is_obfuscated, name_source, archive_release.names_hidden/encryption and the three stored names |
| 135 | `timing_profile` (L508) | derived_per_post | **not_collected** | posted_date is the first article of the latest slice saved and moves on every re-save (src/store.rs:1394-1396); no first/last Date (catalog L510) | N5 (first and last Date per file at ingest), then N1 |
| 136 | `completeness_over_time` (L511) | derived_per_post | **not_collected** | files.seen/expected and touched_at (src/store.rs:165-177) hold the current state only; no history (catalog L513) | N2-class: an append-only history table written by saves; N1 for touched_at |
| 137 | `takedown_pattern` (L514) | derived_per_post | **not_collected** | three STAT bools per probe, undated, shown as 'On the server' (src/web.rs:1278-1289); re-run only on growth or a stamp bit (src/probe/step.rs:94-101) | N6: a scheduled STAT history per provider (release_health) |
| 138 | `group_spray` (L517) | derived_per_post | **shown** | the 'Groups' fact lists each group's release of the same posted name with article counts (src/web.rs:1069-1078; groups_of src/release_info.rs:212-236, limit 64 per shard); order and run-level record not kept (catalog L519) | N0 for the set; order = N5 |
| 139 | `stable_keys` (L522) | poster_profile | **shown** | poster, email and agent are shown and searchable (src/web.rs:1079-1085, 1292-1294; scope_inputs :883); Path, Injection-Info, NNTP-Posting-Host, X-Complaints-To and Organization are shown raw for one article (src/web.rs:1294-1297), not searchable; the id domain is stored, not shown (segments.domain); header order is lost (catalog L524) | N0 per release; indexed columns for posting-account/host = N2 after parsing (catalog rank 5); aggregation = a poster page (new route) |
| 140 | `habit_keys` (L525) | poster_profile | **shown** | per release: Article size, Line length (src/web.rs:1261-1262), Repair percentage from the PAR2 block size (src/web.rs:1138), Encryption and Password (1125-1128), Release group (1119-1121), Group (1078); nothing is aggregated per poster (catalog L527) | N0 per release; aggregation needs a poster page |
| 141 | `linked_identities` (L528) | poster_profile | **not_collected** | no linking of Froms to accounts or grammars (catalog L530) | N2 after rank-5/6 columns exist; not a release-page item |
| 142 | `nzb.xmlns` (L533) | nzb | **stored_not_shown** | a constant render_nzb writes into every NZB (src/nzb.rs); not a per-release value | NA |
| 143 | `head.meta` (L536) | nzb | **shown** | on the page: Category (src/web.rs:1055), Password (1126-1128), Title (1058-1060) and the id Links (1139-1142) are all shown; the NZB itself carries only type=category (src/nzb.rs; catalog L538) | N0 for the page; the NZB needs password/title/id metas (api side, not UI) |
| 144 | `file.poster` (L539) | nzb | **shown** | Poster fact (src/web.rs:1079-1081); every <file poster> is releases.poster (src/nzb.rs:83-88) | N0 |
| 145 | `file.date` (L542) | nzb | **shown** | Posted fact (src/web.rs:1067); the NZB writes posted_date as unix time with the zone already lost (src/nzb.rs:83-88; src/dates.rs; catalog L544) | N0 (the zone caveat is an ingest fix, not a display one) |
| 146 | `file.subject` (L545) | nzb | **stored_not_shown** | files.subject, written only into the NZB (src/nzb.rs:83-88) | N1 (see overview_row.subject) |
| 147 | `groups.group` (L548) | nzb | **shown** | Group / Groups facts (src/web.rs:1069-1078); the NZB writes the one group of the release (src/nzb.rs:89-92) | N0 |
| 148 | `segment.bytes, segment.number` (L551) | nzb | **stored_not_shown** | segments.bytes and segments.part (src/store.rs:181-188), written into the NZB (src/nzb.rs); only their sums (Size, Parts) are shown (src/web.rs:1061-1066, 1122-1124) | N1 |
| 149 | `segment.text` (L554) | nzb | **stored_not_shown** | segments.local + domains (src/store.rs:180-188); NZB only | N1 |
| 150 | `nzb_identity` (L557) | nzb | **shown** | the TWIN / ALT relation is shown in the copies panel (src/web.rs:1171-1181, 1555-1572) from release_fingerprint.posted (PAR2-based, src/duplicates.rs:64-92) and content (sorted sizes, :173); no hash of the segment-id set exists (catalog L559) | N0 for the relation; a literal segment-set digest = N2 (DB-only walk over segments, heavy) |
| 151 | `external_oracles` (L560) | nzb | **shown** | PreDB: 'Name from PreDB' (src/release_info.rs:24-35) and the predb_match row (src/predb/step.rs:25-43); OpenSubtitles match line (src/web.rs:1401-1414); TMDb/TVDB/TVmaze/Trakt ids as Links (src/web.rs:1139-1142, id_links :1790-1811) and the details panel (:1657-1715); stored but not shown: predb kind, pre_at, nuked, section, feed (src/predb/db.rs:8-43; src/metrics/catalog.rs reads them), media_titles provenance (src/media/store.rs:9-25); no srrDB or hash oracle (catalog L562) | N0 for the answers; provenance and nuke = N1 (read predb and media side DBs at render) |
| 152 | `repost_and_retry_events` (L569) | derived_additions | **not_collected** | same-id repeats are dropped by the primary key (src/store.rs:181-188) and the files.seen bitmap; a repost under a new id becomes a second segments row for the same part and nothing counts it (src/store.rs:645-705; catalog L572) | N1: count parts with more than one segments row from each_article (blob v1 keeps part: src/blob.rs:13-18); N5 for the ids themselves |
| 153 | `sequential_file_tiling` (L573) | derived_additions | **not_collected** | only one article's Date survives per release (catalog L576) | N5 then N1 |
| 154 | `session_segmentation` (L577) | derived_additions | **not_collected** | nothing of the kind in src/ (catalog L580) | N5 plus new code; not a display item |
| 155 | `nntmux_collection_key` (L581) | derived_additions | **stored_not_shown** | Collections::of builds a per-group keyed hash from the ported regexes and stores collections(key, release_id) (src/collections.rs; src/store.rs:207); no skeleton for random subjects (catalog L584) | N1 (print the key) or NA; it is an ingest mechanism |
| 156 | `partrepair_window` (L585) | derived_additions | **not_collected** | late articles always join; missing numbers are never retried (catalog L588) | NA (ingest behaviour, not a display item) |
| 157 | `sabnzbd_obfuscation_rules` (L589) | derived_additions | **shown** | releases.is_obfuscated from newznab-tmux-derived rules on release names (src/naming/obfuscation.rs:125-164) -> OBFUSCATED tag (src/web.rs:1043-1045); not applied to file names (catalog L592) | N0; per-file verdicts = N1 (run the rules on files.filename at render) |
| 158 | `clock_skew_from_archive_times` (L593) | derived_additions | **not_collected** | RAR FTIME/mtime and zip DOS times are parsed past and dropped (src/archive/rar.rs, zip.rs; catalog L596) | N3 (the fields are in the first article already read); backfill = re-list + re-probe |
| 159 | `zero_download_inner_size` (L597) | derived_additions | **not_collected** | not computed (catalog L600); files.filename plus per-file sizes are stored, so the volume-size triple is derivable | N1 (compute from stored volume sizes at render) |
| 160 | `x_dnzb_response_headers` (L601) | derived_additions | **not_collected** | get_nzb sets only Content-Type and Content-Disposition (src/api.rs:368-391, :390) | NA for the page; an API change (the values exist: name, category, password) |
| 161 | `newznab_attrs_and_endpoints_not_emitted` (L605) | derived_additions | **shown** | the page shows Files, NFO, Size and Posted (src/web.rs:1061-1067, 1129-1131, 1425-1432); the API emits category, size, pubDate, password, poster, group, resolution, video, audio, hdr, source, language, team, twins, alts and the media ids (src/api.rs:285-310, 313-366; src/media/query.rs:44-49) but no files/nfo/usenetdate attrs and no t=details/t=getnfo | N0 for the page; API work otherwise |
| 162 | `downloader_history_fields` (L609) | derived_additions | **parsed_not_stored** | src/sab.rs reads only the queue filename and history status (catalog L612) | NA (downstream fact); N6 to store per release |
| 163 | `torrent_sidecars` (L613) | derived_additions | **not_collected** | .torrent inside a release is listed by name only; no parser (catalog L616) | N4 (the sidecar's article) then store |
| 164 | `last_article_bytes_key` (L617) | derived_additions | **stored_not_shown** | segments.bytes is stored per article (src/store.rs:181-188); nothing compares tails across releases (catalog L620) | N1 to print the tail size; N2 (indexed column) to use as a key |
| 165 | `spotnet_ground_truth` (L621) | derived_additions | **not_collected** | no spot reader (catalog L624) | N6 |
| 166 | `nzblnk` (L625) | derived_additions | **not_collected** | Atlas serves NZBs only (catalog L628) | NA (an outbound link format); N1 to emit one from stored fields |
| 167 | `lines_sequence_key` (L629) | derived_additions | **not_collected** | :lines is dropped by parse_overview (catalog L632) | N5 |
| 168 | `nzb_writer_fingerprint` (L633) | derived_additions | **not_collected** | Atlas builds its own NZBs from headers and reads none (catalog L636) | NA |
| 169 | `posting_tool_fingerprints (table): tool classification per release` (L639) | posting_tool_fingerprints | **not_collected** | only the agent header is kept and shown ('Posted with', src/web.rs:1292-1294); segment size, line length and the message-id alphabet class are stored (src/probe/mod.rs:59; src/probe/yenc.rs:10; src/store.rs:311-404) but no classifier combines them (catalog L639-665) | N1 for a first classifier over stored fields (agent, segment_size, line_len, id grammar); N3 for the yEnc syntax tells |
| 170 | `Main` (L673) | par2 | **stored_not_shown** | block_size -> release_fingerprint.par2_block, data block count -> par2_blocks (src/duplicates.rs:40; src/archive/steps.rs Par2Step); both feed the Repair percentage (exact_repair_per_mille, src/web.rs:1138) but the slice size itself is not printed; file-id order and the non-recoverable list are dropped (catalog L675) | N1 to print the slice size; recoverable/non-recoverable lists = N3 (re-run the PAR2 step) |
| 171 | `FileDesc` (L676) | par2 | **shown** | name and size become archive_inner_files rows (source PAR2, src/archive/store.rs:18-40) -> 'Files inside' (src/web.rs:1144-1161); md5 and md5_16k are read (src/par2.rs Par2File) and only folded into release_fingerprint.posted (src/duplicates.rs:64-92), not stored (catalog L678) | N0 for names and sizes; per-file md5/md5_16k/length = par2_files table filled by re-running the PAR2 step (N3) |
| 172 | `UniFileN` (L679) | par2 | **shown** | a clean UTF-16 name replaces the ASCII one in Par2File.name, so 'Files inside' carries it; the disagreement is not noted (catalog L681) | N0; the ASCII/UTF-16 disagreement = N3 |
| 173 | `IFSC` (L682) | par2 | **not_collected** | the packet is never read (catalog L684) | N3 (same index article) + crc32 combine -> per-file CRC32 into par2_files |
| 174 | `RecvSlic` (L685) | par2 | **not_collected** | never read; recovery volumes and blocks are counted from file names (vol000+01) at page time (src/release_info.rs:39-60; catalog L687) -> 'PAR2 files (n recovery)' and Repair | N4 (a recovery-volume article) for exponents; counts are N0 already |
| 175 | `Creator` (L688) | par2 | **not_collected** | not read (catalog L690) | N3 (same index article), a text column on par2_files or release_fingerprint |
| 176 | `Comment / UniComnt` (L691) | par2 | **not_collected** | not read | N3 |
| 177 | `PkdMain / PkdRecv / PkdIFSC` (L694) | par2 | **not_collected** | not read | N3, rare |
| 178 | `set_id` (L697) | par2 | **parsed_not_stored** | Par2File.set_id is read; it is the first input of release_fingerprint.posted and the one-set check; not stored on its own (src/duplicates.rs:64-92; catalog L699) | N3 (store it as a column) or N1 impossible since it is hashed away |
| 179 | `par2_layout` (L700) | par2 | **shown** | counts only: PAR2 files, recovery volumes, Repair percentage (src/web.rs:1132-1138; repair_fact :1752-1760); slice-size choice and coverage of nfo/sfv not shown (catalog L703) | N0 for counts; slice size N1 (stored in par2_block) |
| 180 | `entries` (L705) | sfv | **shown** | release_probe.info.sfv (name -> crc32, src/probe/mod.rs:75) -> SFV table (src/web.rs:1433-1444) | N0 |
| 181 | `comments` (L708) | sfv | **not_collected** | comment lines are skipped in parse_sfv (src/probe/mod.rs; catalog L710) | N3 |
| 182 | `release_name` (L713) | nfo | **shown** | the ported NfoNameExtractor (src/nfo.rs) feeds naming (NfoName step src/naming/steps.rs:77-96) -> display name and 'Name from the NFO' (src/release_info.rs:24-35; src/web.rs:1091-1093) | N0 |
| 183 | `ids_and_urls` (L715) | nfo | **shown** | imdb_ids (up to 16) in release_probe.info.imdb_ids -> links above the NFO text (src/web.rs:1425-1432); media_release.imdb_from_nfo is stored, not shown (src/media/steps.rs:42); other ids ignored (catalog L717) | N0 for IMDb; TMDb/TVDB/AniDB/MusicBrainz URLs = N1 (regex over the stored NFO text) |
| 184 | `template_fields` (L718) | nfo | **shown** | the whole text (256 KiB max, MAX_NFO src/probe/step.rs:21) is shown as <pre class=nfo> (src/web.rs:1425-1432); fields are not parsed (catalog L720) | N0 as text; parsed fields = N1 |
| 185 | `password` (L721) | nfo | **shown** | release_nfo_password (src/archive/store.rs:54-62) -> Password fact with WORKS / PROBABLY WORKS / WRONG / not tried (src/web.rs:1126-1128; password_fact :1741-1749; password_hint_with src/release_info.rs:539); the verdict is computed at render against archive_release.password_check and not stored | N0 |
| 186 | `encoding_and_shape` (L723) | nfo | **parsed_not_stored** | UTF-8 vs CP437 is decided per NFO in probe::nfo_text and dropped (src/probe/mod.rs:150); line width and banner hash not read (catalog L725) | N1 for width (from the stored text); the original encoding = N3 |
| 187 | `srr` (L728) | rescene | **not_collected** | .srr is in the NotArchive list (src/archive/select.rs:21) | N4 (the .srr's article) + a parser |
| 188 | `srs` (L731) | rescene | **not_collected** | .srs is NotArchive (src/archive/select.rs:21) | N4 + a parser |
| 189 | `srrdb_lookup` (L734) | rescene | **not_collected** | no srrDB client (catalog L736; coordination.md:21 notes the endpoint and its robots policy) | N6 (an external step keyed on inner CRC + size) |
| 190 | `main_header_0x73` (L739) | rar4 | **shown** | encrypted headers (0x0080) -> archive_release.encryption and names_hidden (src/archive/store.rs:43-51) -> Encryption fact '..., file names hidden' (encryption_fact src/web.rs:1763-1771) and NAMES AND FILES ENCRYPTED in Packing (src/web.rs:1307-1313); no RAR4 password check (PasswordTest covers RAR5 and zip); solid, recovery, comment flags not kept (catalog L741) | N0 for the flag; solid/recovery flags = N3 |
| 191 | `file_header_0x74` (L742) | rar4 | **shown** | name and unpacked size -> 'Files inside' (archive_inner_files); the first file's CRC, METHOD, UNP_SIZE -> 'File CRC32', 'Method', 'Unpacked' (src/web.rs:1304-1321; ArchiveHead src/probe/archive.rs:12-35); PACK_SIZE stored (archive.packed), not shown; HOST_OS, FTIME, UNP_VER, ATTR, SALT, EXT_TIME dropped (catalog L744) | N0; packed = N1; mtime, host OS, dictionary = N3 (same article) |
| 192 | `newsub_0x7a` (L745) | rar4 | **not_collected** | skipped (catalog L747) | N3 |
| 193 | `end_0x7b` (L748) | rar4 | **parsed_not_stored** | stops the walk (catalog L750) | N3 for the volume number |
| 194 | `volume_scheme` (L751) | rar4 | **parsed_not_stored** | stored_at and continues exist on ArchiveHead but are serde(skip) (src/probe/archive.rs:23-28); the volume naming and sizes are visible only through files.filename and per-file sizes; volume size not kept (catalog L753) | N1 (derive volume count and size from files and segments at render) |
| 195 | `main_header_type_1` (L756) | rar5 | **not_collected** | type 1 falls through the generic block header; archive flags and the locator are never read (catalog L758) | N3 |
| 196 | `encryption_header_type_4` (L759) | rar5 | **shown** | its presence -> headers_encrypted -> NAMES AND FILES ENCRYPTED / Encryption fact (src/web.rs:1307-1313, 1125); kdf, salt and check are stored inside archive_release.password_check (src/archive/store.rs:50) and drive the Password verdict, but are not printed or indexed (catalog L761) | N0 for the state; salt/check as a twin key = N2 (indexed column from the stored blob) |
| 197 | `file_header_type_2` (L762) | rar5 | **shown** | name, unpacked size -> 'Files inside'; first file's data CRC32, method (m0..m5), packed and unpacked -> Packing (src/web.rs:1304-1321; src/archive/rar.rs rar5); mtime, version/solid/dictionary bits, host OS, attributes dropped (catalog L764) | N0; dropped fields = N3 |
| 198 | `file_extra_records` (L765) | rar5 | **shown** | the 0x01 encryption record sets encrypted and fills password_check -> FILES ENCRYPTED tag and Password verdict (src/web.rs:1307-1313, 1741-1749); 0x02 to 0x07 not read (catalog L767) | N0 for encryption; hash, htime, owner = N3 (owner is a privacy leak: store hashed or not at all) |
| 199 | `service_headers_type_3` (L768) | rar5 | **not_collected** | skipped (catalog L770) | N3 |
| 200 | `end_type_5` (L771) | rar5 | **parsed_not_stored** | stops the walk | N3 |
| 201 | `packer_fingerprint` (L774) | rar5 | **shown** | only the method is stored and shown ('Method' src/web.rs:1305; archive.method); dictionary, solid, host OS, htime, quick open, recovery record not read (catalog L777) | N0 for method; the rest N3 |
| 202 | `start_header` (L779) | sevenz | **parsed_not_stored** | offset/size/crc are used to find the end header and not stored (src/archive/sevenz.rs; catalog L781) | N3 (per-creation twin key) |
| 203 | `streams_info` (L782) | sevenz | **shown** | coders are read to detect AES/LZMA -> Encryption fact; names and sizes -> 'Files inside'; per-file CRCs are read (Folder.crc) and dropped; the 7z ArchiveHead.method is None (src/probe/archive.rs:17; catalog L784) | N0 for names/sizes/encryption; per-file CRCs and coder details = N3 (needs the end header: a tail article when the start header points past the first article) |
| 204 | `files_info` (L785) | sevenz | **shown** | names and sizes -> 'Files inside'; times and attributes dropped (catalog L787) | N0; times = N3 |
| 205 | `encrypted_header` (L788) | sevenz | **shown** | headers_encrypted -> NAMES AND FILES ENCRYPTED and the Encryption fact (src/web.rs:1307-1313; 1763-1771); salt/IV not kept (catalog L791) | N0; salt/IV = N3 |
| 206 | `local_header` (L793) | zip | **shown** | flags, method, sizes, name, zip64 and AES extras are read; the first entry's CRC32 and method name -> Packing (src/web.rs:1305, 1321); the CRC is blanked when bit 3 is set (src/probe/archive.rs:285-286, a fix after a29ee52); DOS time, version made by and attributes dropped (catalog L795) | N0; dropped fields = N3 |
| 207 | `extra_fields` (L796) | zip | **shown** | zip64 and 0x9901 AES only -> sizes and the Encryption fact; UT/unix/NTFS/unicode-path extras not read (catalog L798) | N0 for AES; other extras = N3 |
| 208 | `central_directory_and_end` (L799) | zip | **shown** | the central directory is preferred when it lists files (zip::list) -> names and sizes in 'Files inside'; comments, versions and attributes dropped (catalog L802) | N0; dropped fields = N3 |
| 209 | `alt_key` (L804) | archive_common | **shown** | ALT copies in the copies panel from release_fingerprint.content = SHA-1 over sorted sizes of posted files >= 1 MiB (src/duplicates.rs:173; src/web.rs:1171-1181, 1555-1572); no CRCs (catalog L806) | N0 for the relation; a size + inner CRC key = N3 (store inner CRCs) then N2 (index) |
| 210 | `per_creation_random` (L807) | archive_common | **stored_not_shown** | the RAR5 salt (with kdf and check) sits in archive_release.password_check (src/archive/store.rs:50); the 7z next-header trio is parsed only; the MKV SegmentUID is not read; none is used as a key (catalog L809) | N1 to print the RAR5 salt; N2 to index; 7z/MKV values N3 |
| 211 | `inner_listing` (L810) | archive_common | **shown** | archive_inner_files (name, size, source; at most MAX_STORED 64 rows written src/archive/mod.rs:27, 256 read src/archive/store.rs:93) -> 'Files inside' (src/web.rs:1144-1161); the first entry's CRC32 in Packing; no times, no per-file CRC (catalog L812) | N0; per-file CRC and mtime = N3 (added columns on archive_inner_files) |
| 212 | `password_state` (L813) | archive_common | **shown** | archive_release.password/encryption/names_hidden/password_check and release_nfo_password -> PASSWORD / PASSWORD? tags (password_tag src/web.rs:698-704), Encryption and Password facts (1125-1128); the source and verdict are computed at render, not stored (catalog L815) | N0 |
| 213 | `par2_recvslic_twin` (L822) | payload_additions | **not_collected** | recovery volumes are never read (catalog L825) | N4 (first recovery volume's article) |
| 214 | `par1_and_par3` (L826) | payload_additions | **not_collected** | identify() knows only PAR2 (catalog L829) | N3 after a parser |
| 215 | `rar_data_offset_and_volume_map` (L830) | payload_additions | **parsed_not_stored** | stored_at is computed for volume 1 only and is serde(skip) (src/probe/archive.rs:23-25; catalog L833) | N1 is impossible (not serialised); N3 to store the map |
| 216 | `rar5_rev_and_quick_open` (L834) | payload_additions | **not_collected** | .rev is not a recognised format; service payloads are skipped (catalog L837) | N4 |
| 217 | `recovery_record_and_comment` (L838) | payload_additions | **not_collected** | not read (catalog L841) | N3 |
| 218 | `uploader_leaks_in_archives` (L842) | payload_additions | **not_collected** | STM/UOWNER/REDIR records not read (catalog L845); the catalog says store hashed or not at all | N3 only as hashes; never display |
| 219 | `sevenz_coder_details` (L846) | payload_additions | **parsed_not_stored** | coders and CRCs are read to detect AES/LZMA and dropped (catalog L849) | N3 |
| 220 | `compressed_prefix_decode` (L850) | payload_additions | **shown** | main (#52) follows archives and compressed streams down: ProbeInfo.layers (src/probe/mod.rs:69-71) with format, method, stored flag and stop reason (src/probe/nest.rs:25-39) -> 'Layers' chain (src/web.rs:1322-1325; nested_chain :1840-1854); compressed RAR5 decode is PR #58, bzip2/deflate64/compress/lz4 decode is #56 on origin/main; the catalog's 'no' (L853) is stale | N0 on main for deflate/LZMA/xz/zstd layers; RAR5 = merge #58; backfill rides on probe stamp bit 2 (already set, so every probed release re-probes once) |
| 221 | `zip_fingerprint_details` (L854) | payload_additions | **not_collected** | version made by, extra-field order, descriptor use not kept (catalog L857) | N3 |
| 222 | `repack_provenance` (L858) | payload_additions | **not_collected** | inner mtimes are dropped (catalog L861) | N3 |
| 223 | `password_checks_as_keys` (L862) | payload_additions | **stored_not_shown** | archive_release.password_check keeps the RAR5/zip check bytes and the Password verdict is shown (src/web.rs:1741-1749); no password table, no key use (catalog L865) | N1 to print; a harvested-password table = new table + N2 |
| 224 | `ngpost_pack_defaults` (L866) | payload_additions | **shown** | method m0 is parsed and shown ('Method'); the rest of the ngPost signature (-v42m volumes, 17-char names, -hp) is not fingerprinted (catalog L869) | N0 for method; the rest N1 (volume size and name shape from stored files) and N3 |
| 225 | `srr_srs_layout` (L870) | payload_additions | **not_collected** | no SRR/SRS parser (catalog L873) | N4 + parser |
| 226 | `indexer_archive_prior_art` (L874) | payload_additions | **not_collected** | prior art, not a field; src/archive is the Rust analogue with a 64 KiB probe prefix (PROBE_PREFIX src/archive/mod.rs:29) | NA |
| 227 | `listing_scope_per_volume` (L878) | payload_additions | **parsed_not_stored** | Listing.complete and need_more_tail steer the ListStep to read the last volume (src/archive/steps.rs:367-430); whether the stored listing is complete is not stored or shown (catalog L881) | N2 (a completeness flag as an added column) then N1 |
| 228 | `legacy_and_unix_archives` (L882) | payload_additions | **shown** | main (#52) recognises TAR, GZIP, XZ, LZMA, LZIP, ZSTD, BZIP2, COMPRESS, LZ4, LZOP as layers (src/probe/nest.rs:26) -> 'Layers' chain; ACE, ARJ, LZH stay unknown (.ace, .cab, .tar, .gz, .bz2, .xz are NotArchive for the archive step: src/archive/select.rs:21); the catalog's 'no' (L885) is stale | N0 for the recognised layers; ACE/ARJ/LZH = N3 after parsers; per-entry tar/gzip fields = N3 |
| 229 | `disc_images` (L886) | payload_additions | **not_collected** | .iso, .img, .bin are NotArchive (src/archive/select.rs:21; catalog L889) | N4 (targeted articles: PVD at 0x8000 is in the first article, UDF/BDMV later) + parsers |
| 230 | `documents_and_comics` (L890) | payload_additions | **shown** | .cbz is read as zip and .cbr as RAR for names -> 'Files inside'; epub/mobi/pdf are NotArchive (src/archive/select.rs:21; catalog L893); ComicInfo/OPF/Info-dict fields not read | N0 for cbz/cbr names; metadata = N3 (first article) + parsers |
| 231 | `games_and_software` (L894) | payload_additions | **not_collected** | console and PC releases are categorised from the name only (src/category; catalog L897) | N3 + ROM/PE parsers |
| 232 | `par2_exponent0_recovery_slice` (L898) | payload_additions | **not_collected** | recovery volumes are never read, only counted from names (catalog L901) | N4 |
| 233 | `pack_containment` (L902) | payload_additions | **not_collected** | alts are judged per release (release_fingerprint.content), never per file across releases (catalog L905) | needs per-file CRCs (N3) then a cross-release query (N2) |
| 234 | `rar_determinism` (L906) | payload_additions | **not_collected** | no per-article CRCs are kept (catalog L909) | segment_crc (N3/N4) then N2 |
| 235 | `volume_order_from_headers` (L910) | payload_additions | **not_collected** | archive::names collapse_volumes orders by name only (catalog L913) | N3 (volume numbers from the headers already read) |
| 236 | `sampled_sha256_key` (L914) | payload_additions | **not_collected** | moviehash (first and last 64 KiB) is the only sampled key (src/probe/step.rs:294-361; catalog L917) | N4 (one middle article) |
| 237 | `raw_split_and_crc_sidecars` (L918) | payload_additions | **shown** | main treats .001 as Kind::Numbered and opens it (src/archive/select.rs:36, 70, 104-110), so a numbered split's inner header shows in Packing; the .crc sidecar is not read; the catalog's 'no' (L921) is stale for .001 | N0 for .001 headers; the .crc sidecar = N4 (its one article) |
| 238 | `repack_and_reuploader_tells` (L922) | payload_additions | **not_collected** | src/naming strips some re-uploader suffixes while naming; no re-pack flag is kept (catalog L925); the posted name with its suffix is visible as 'Posted as' (src/web.rs:1087-1090) | N1 (match the suffix dictionary against names.posted at render; flag files outside the scene set from archive_inner_files) |
| 239 | `non_video_set_fingerprints` (L926) | payload_additions | **not_collected** | no set-level key from sidecar files (catalog L929) | N4 (sidecar articles) + parsers |
| 240 | `marker_file_hashes` (L930) | payload_additions | **not_collected** | tiny companion files are listed by name and size only ('Files inside'); no hash (catalog L933) | N4 (the tiny file's article) then store |
| 241 | `EBML_header` (L942) | matroska | **shown** | container name ('Matroska' / 'WebM') -> 'Container' (src/web.rs:1330; MediaInfo.container src/probe/media.rs:9); DocTypeVersion not kept (catalog L944) | N0; versions N3 |
| 242 | `Segment.Info` (L945) | matroska | **shown** | title, duration_secs, muxing_app, writing_app -> Title, Duration, Muxed by, Written by (src/web.rs:1331-1341; src/probe/media.rs:10-14); SegmentUID, DateUTC, SegmentFamily, Prev/NextUID not read (catalog L947; coordination.md:9 leaves these to this feature) | N0 for the four; SegmentUID / DateUTC = N3 (same front article, catalog rank 9) |
| 243 | `Tracks.TrackEntry` (L948) | matroska | **shown** | type, codec, language, forced, hearing_impaired, commentary, frame rate (DefaultDuration), DV profile/EL and track name are shown per track (src/web.rs:1342-1396; src/probe/media.rs:35-76); FlagDefault, TrackUID, CodecPrivate, Original/VisualImpaired flags and ContentEncodings not read; dv_level and video language are in the JSON only (catalog L950) | N0; dv_level/video language = N1 (PR #48 and the stream-details branch render them: coordination.md:6-7); the rest N3 |
| 244 | `Tracks.Video` (L951) | matroska | **shown** | width x height, frame rate, bit depth, hdr label, DV profile and enhancement layer -> 'Video' (src/web.rs:1342-1361); display size, primaries, transfer, dv_level, language stored in the JSON only (src/probe/media.rs:39-50); interlace, stereo, crop, chroma, MaxCLL/FALL not read; the hdr label says 'HDR10' when MasteringMetadata is merely present (catalog L953) | N0; display/primaries/transfer = N1 (PR #48 renders them, coordination.md:6); the rest N3 |
| 245 | `Tracks.Audio` (L954) | matroska | **shown** | channels, sample rate, codec, language, name, Atmos (from the track name only), commentary -> 'Audio' (src/web.rs:1362-1377; src/probe/media.rs:55-65); bit depth dropped (catalog L956) | N0; bit depth N3 |
| 246 | `Chapters` (L957) | matroska | **shown** | the first EditionEntry with chapters: start and name -> 'Chapters' panel (src/web.rs:1415-1423, 1448; src/probe/media.rs:28-31); UIDs, ends, languages, other editions dropped (catalog L959) | N0; editions and flags = N3 (stream-details branch adds an edition count, coordination.md:7) |
| 247 | `Tags` (L960) | matroska | **not_collected** | the walker handles Info, Tracks, Chapters and the first Cluster only (src/probe/media.rs; catalog L962, L1068) | N3 for Lavf front Tags (same articles); N4 (one tail article) for mkvmerge's statistics Tags at the end |
| 248 | `Attachments` (L963) | matroska | **not_collected** | not read on main; PR #48 adds attachments and cover art (coordination.md:6; pr/48 src/probe/mod.rs release_cover table) | merge #48 (N3 under its stamp bit); do not duplicate |
| 249 | `structure` (L966) | matroska | **parsed_not_stored** | header_complete (first cluster reached or whole moov) only decides whether to read more front articles and is serde(skip) (src/probe/media.rs:20-23; src/probe/step.rs:223-231; catalog L968) | N3 (store the layout tell) |
| 250 | `first_video_frame` (L969) | matroska | **not_collected** | no bitstream parsing; ffmpeg is ruled out (catalog L971); PRs #51/#54 bring an H.264/HEVC intra decoder for stills | N4 (first cluster, 1-3 articles) + a NAL/SEI walker |
| 251 | `ftyp` (L974) | mp4 | **shown** | detected -> 'Container' MP4 (src/web.rs:1330; src/probe/media.rs:84-86); brands not kept (catalog L976) | N0; brands N3 |
| 252 | `moov.mvhd` (L977) | mp4 | **shown** | duration -> 'Duration' (src/web.rs:1335-1341); creation/modification times not kept (catalog L979) | N0; times N3 |
| 253 | `moov.trak` (L980) | mp4 | **shown** | hdlr type, mdhd language, stsd fourcc, width/height, channels, sample rate, dvcC/dvvC -> Video/Audio/Subtitles lines (src/web.rs:1342-1396); times, matrix, edit lists, handler names, frame counts dropped; no MP4 chapters (catalog L982) | N0; dropped fields N3 |
| 254 | `moov.udta` (L983) | mp4 | **not_collected** | ilst / iTunes tags not read (catalog L985); apID, purd, ownr are buyer leaks and must never be displayed (catalog L1096) | N3 (front articles when faststart) or N4 (tail moov); privacy filter required |
| 255 | `misc` (L986) | mp4 | **parsed_not_stored** | moov position and completeness are read by has_whole_moov for header_complete and dropped (catalog L989) | N3 (store faststart yes/no) |
| 256 | `avi` (L991) | avi_and_others | **parsed_not_stored** | the RIFF/AVI magic is sniffed by archive::identify to end the archive step; nothing past the magic (src/archive/mod.rs identify; catalog L990-993) | N3 + an AVI header parser (hdrl, INFO) |
| 257 | `asf_wmv` (L994) | avi_and_others | **not_collected** | the ASF GUID is not sniffed (catalog L990 lists the sniffed magics) | N3 + parser |
| 258 | `mpeg_ts_ps` (L997) | avi_and_others | **parsed_not_stored** | the MPEG-PS magic is sniffed only (catalog L990); TS, DVD IFO and BDMV not read | N3 (PS/TS tables) or N4 (IFO/BDMV targeted articles) |
| 259 | `ogg_flv_rm` (L1000) | avi_and_others | **parsed_not_stored** | the OggS magic is sniffed only; FLV and RM not (catalog L990) | N3 + parsers |
| 260 | `mp3` (L1005) | audio | **parsed_not_stored** | the ID3 / 0xFFFB magic is sniffed only (catalog L1004) | N3 + ID3/LAME parser |
| 261 | `flac` (L1008) | audio | **parsed_not_stored** | the fLaC magic is sniffed only (catalog L1004) | N3 + STREAMINFO/VORBIS_COMMENT parser |
| 262 | `aac_m4a_opus_ogg` (L1011) | audio | **shown** | .m4a goes down the MP4 path: mp4a codec, channels, sample rate, language -> 'Audio' (src/web.rs:1362-1377); Opus in .mka/.webm via Matroska; ilst, ADTS and vendor strings not read (catalog L1013) | N0 for container-level facts; tags N3 |
| 263 | `ac3_eac3_dts_truehd` (L1014) | audio | **shown** | codec id and channels from the container; Atmos only when a Matroska track name contains 'atmos' (src/probe/media.rs:61-62), never from the bitstream (catalog L1016) | N0 for codec/channels; bitstream flags (JOC/Atmos, dialnorm, DTS:X) = N4 (first cluster) + parser |
| 264 | `wav_ape_wv_mpc` (L1017) | audio | **not_collected** | not sniffed (catalog L1019) | N3 + parsers |
| 265 | `rip_sidecars` (L1020) | audio | **not_collected** | .log/.cue/.m3u never fetched (catalog L1022) | N4 (sidecar articles) + parsers |
| 266 | `jpeg` (L1025) | images_and_documents | **parsed_not_stored** | the JPEG magic ends the archive step; nothing past it (src/archive/mod.rs identify; catalog L1024) | N3 + EXIF/JFIF parser (privacy: Make/Model/serial/GPS are personal) |
| 267 | `png_gif_webp` (L1028) | images_and_documents | **parsed_not_stored** | magics only (catalog L1024) | N3 + parser |
| 268 | `pdf_epub_mobi_cbz` (L1031) | images_and_documents | **not_collected** | PDF, EPUB, MOBI are NotArchive (src/archive/select.rs:21); CBZ names only via zip (catalog L1033) | N3 + parsers |
| 269 | `executables_and_discs` (L1034) | images_and_documents | **not_collected** | .exe, .msi, .dmg, .iso, .img, .bin are NotArchive (src/archive/select.rs:21) | N3 (PE header is in the first article) / N4 (ISO PVD) + parsers |
| 270 | `srt_ass_ssa` (L1039) | subtitles | **shown** | embedded subtitle tracks: codec, language, forced, SDH, commentary, name -> 'Subtitles' (src/web.rs:1378-1396; src/probe/media.rs:69-76); ASS script info / styles and sidecar .srt/.ass are never read (.srt, .ass, .sub, .idx are NotArchive: src/archive/select.rs:21; catalog L1041) | N0 for track facts; ASS CodecPrivate = N3; sidecars = N4 |
| 271 | `vobsub_pgs_dvb` (L1042) | subtitles | **shown** | PGS and DVB tracks are stored as subtitle codec plus language and shown (src/web.rs:1378-1396); .idx sidecars not read (catalog L1044) | N0; .idx = N4 |
| 272 | `external_hashes` (L1045) | subtitles | **shown** | moviehash stored and shown (src/web.rs:1398-1400); subtitle files are not hashed (catalog L1048) | N0; sub MD5 = N4 |
| 273 | `moviehash` (L1050) | content_identity | **shown** | release_probe.moviehash and release_opensubtitles -> 'OpenSubtitles hash' / 'OpenSubtitles match' (src/web.rs:1398-1414) | N0 |
| 274 | `perceptual` (L1053) | content_identity | **not_collected** | no decoded frames on main (catalog L1055); PRs #51/#54 add stills (release_stills: pr/51 src/probe/stills.rs:17, 29-) and origin/feat/release-stills-strip renders them (coordination.md:8) | merge #51/#54/#60; a pHash = N4 (mid-file articles) on top |
| 275 | `structural` (L1056) | content_identity | **shown** | chapters, tracks and duration are shown (src/web.rs:1327-1423); not used as a key (catalog L1058) | N0; key use = N2 |
| 276 | `matroska_element_ids_not_read` (L1065) | media_additions | **not_collected** | the walker handles Info, Tracks, Chapters and the first Cluster only (src/probe/media.rs; catalog L1068) | N3 (same front articles) for SegmentUID, DateUTC, TrackUID, FlagDefault, FlagOriginal; N4 for Tags/Attachments at the end |
| 277 | `muxer_layout_tell` (L1069) | media_additions | **shown** | muxing_app and writing_app -> 'Muxed by' / 'Written by' (src/web.rs:1332-1333); the element layout is not recorded (catalog L1072) | N0 for the app strings; layout = N3 |
| 278 | `codec_config_bit_depth` (L1073) | media_additions | **shown** | bit depth from Colour BitsPerChannel only -> 'N-bit' (src/web.rs:1352-1354; src/probe/media.rs:42); hvcC/avcC not parsed, so files without Colour show none (catalog L1076) | N0 when present; reliable value = N3 (parse hvcC/avcC from CodecPrivate / stsd in the same article) |
| 279 | `remux_invariant_alt_key` (L1077) | media_additions | **not_collected** | release_fingerprint.content hashes sizes only (src/duplicates.rs:173; catalog L1080) | N4 (first cluster) + hashing, then N2 |
| 280 | `ass_subtitle_keys` (L1081) | media_additions | **not_collected** | CodecPrivate is not read (catalog L1084) | N3 |
| 281 | `mp4_moov_at_the_end` (L1085) | media_additions | **not_collected** | has_whole_moov parses the front only; a tail moov is never fetched, so such files show Container only (src/probe/media.rs; catalog L1088) | N4 (one computed tail article) |
| 282 | `mp4_codec_config_boxes` (L1089) | media_additions | **shown** | stsd fourcc, channels and rate, hvc1 and the DV config partly -> Video/Audio lines (src/web.rs:1342-1377); dac3/dec3/esds/colr/pasp/btrt not read (catalog L1092) | N0; the rest N3 |
| 283 | `tagger_ids_and_buyer_leaks` (L1093) | media_additions | **not_collected** | Tags and ilst are not read (catalog L1096); apID/purd/ownr must never be displayed | N3/N4 with a privacy filter; only IMDB/TMDB/TVDB tags are showable |
| 284 | `dolby_vision_rpu_rules` (L1097) | media_additions | **shown** | dv_profile and dv_enhancement_layer -> 'Dolby Vision profile N, enhancement layer' (src/web.rs:1355-1361); dv_level is in the JSON only; the RPU is not parsed (catalog L1100) | N0; dv_level N1 (PR #48 renders it); MEL/FEL and CM version = N4 (first RPU) + the dolby_vision crate |
| 285 | `encoder_sei_strings` (L1101) | media_additions | **not_collected** | no NAL or SEI parsing (catalog L1104); the stream-details branch reads encoder and settings from Matroska Tags, not from SEI (coordination.md:7) | N4 (first cluster) + a SEI walker; Tags route = that branch |
| 286 | `bitstream_headers` (L1105) | media_additions | **not_collected** | only codec ids and the track name today (catalog L1108) | N4 (first cluster) + parsers |
| 287 | `mediainfo_and_bdinfo_text_in_nfos` (L1109) | media_additions | **shown** | the NFO text is stored whole and shown as <pre> (src/web.rs:1425-1432); the 'Unique ID', 'Encoded date', 'Writing application' lines are not parsed (catalog L1112) | N1 (regex over the stored NFO at render) |
| 288 | `bluray_dvd_identity` (L1113) | media_additions | **not_collected** | ISO/IMG are NotArchive; no IFO/BDMV reader (catalog L1116) | N4 + parsers |
| 289 | `subtitle_sidecar_fields` (L1117) | media_additions | **not_collected** | .srt/.sub/.idx/.ass are NotArchive and never fetched (src/archive/select.rs:21; catalog L1120) | N4 + parsers |
| 290 | `image_fields` (L1121) | media_additions | **not_collected** | images are recognised by magic only (catalog L1124) | N3 + EXIF/PNG parsers (privacy filter) |
| 291 | `non_video_content_keys` (L1125) | media_additions | **not_collected** | FLAC MD5, Ogg serial, LAME CRC, APE MD5, PDF /ID, PE GUID not read (catalog L1128) | N3 + parsers |
| 292 | `pure_rust_decode_budget` (L1129) | media_additions | **not_collected** | a budget note, not a field; PRs #51/#54 carry the decoders (pr/54 1e1949f merges feat/h264-intra-decoder) | NA |
| 293 | `credits_and_title_card_ocr` (L1133) | media_additions | **not_collected** | nothing of the kind (catalog L1136) | N4 (file-level) + OCR; out of scope |
| 294 | `stream_thread_keys` (L1137) | media_additions | **not_collected** | no keyframe/AC-3/FLAC keys (catalog L1140) | N4 + parsers, then N2 |

### 4.1 Counts per status

| Status | Items | Share |
|---|---|---|
| shown | 96 | 32.7% |
| stored_not_shown | 24 | 8.2% |
| parsed_not_stored | 31 | 10.5% |
| not_collected | 143 | 48.6% |
| unclear | 0 | 0.0% |
| **total** | **294** | 100% |

Of the 294 rows, 19 are not per-release facts (needs column starts with NA): server, group, tool, downloader or prior-art notes. The other 275 are release facts that a release page could carry.

Rows by the first needs code named (the cheapest way to show the primary value): N0 94, N1 35, N2 4, N3 90, N4 22, N5 15, N6 8, NA 19, other 7 (rows whose cheapest path is merging an open PR or writing a decoder first).

### 4.2 Counts per layer

| Layer (catalog block) | Items | shown | stored_not_shown | parsed_not_stored | not_collected | unclear |
|---|---|---|---|---|---|---|
| nntp_session | 10 | 1 | 2 | 3 | 4 | 0 |
| overview_row | 10 | 4 | 2 | 3 | 1 | 0 |
| headers_mandatory | 6 | 3 | 2 | 0 | 1 | 0 |
| headers_optional_netnews | 17 | 5 | 0 | 0 | 12 | 0 |
| headers_mail_and_mime | 10 | 0 | 0 | 1 | 9 | 0 |
| headers_server_injected | 9 | 3 | 0 | 0 | 6 | 0 |
| headers_poster_tool | 6 | 1 | 0 | 0 | 5 | 0 |
| header_shape | 5 | 0 | 0 | 1 | 4 | 0 |
| nntp_additions | 10 | 0 | 1 | 1 | 8 | 0 |
| ybegin | 5 | 4 | 1 | 0 | 0 | 0 |
| ypart | 2 | 0 | 2 | 0 | 0 | 0 |
| yend | 4 | 1 | 2 | 0 | 1 | 0 |
| encoding_quirks | 9 | 3 | 1 | 0 | 5 | 0 |
| non_yenc_bodies | 4 | 1 | 0 | 0 | 3 | 0 |
| decoded_payload | 4 | 3 | 0 | 1 | 0 | 0 |
| yenc_additions | 9 | 0 | 1 | 0 | 8 | 0 |
| derived_per_article | 12 | 5 | 1 | 2 | 4 | 0 |
| derived_per_post | 6 | 3 | 0 | 0 | 3 | 0 |
| poster_profile | 3 | 2 | 0 | 0 | 1 | 0 |
| nzb | 10 | 6 | 4 | 0 | 0 | 0 |
| derived_additions | 17 | 2 | 2 | 1 | 12 | 0 |
| posting_tool_fingerprints | 1 | 0 | 0 | 0 | 1 | 0 |
| par2 | 10 | 3 | 1 | 1 | 5 | 0 |
| sfv | 2 | 1 | 0 | 0 | 1 | 0 |
| nfo | 5 | 4 | 0 | 1 | 0 | 0 |
| rescene | 3 | 0 | 0 | 0 | 3 | 0 |
| rar4 | 5 | 2 | 0 | 2 | 1 | 0 |
| rar5 | 7 | 4 | 0 | 1 | 2 | 0 |
| sevenz | 4 | 3 | 0 | 1 | 0 | 0 |
| zip | 3 | 3 | 0 | 0 | 0 | 0 |
| archive_common | 4 | 3 | 1 | 0 | 0 | 0 |
| payload_additions | 28 | 5 | 1 | 3 | 19 | 0 |
| matroska | 10 | 6 | 0 | 1 | 3 | 0 |
| mp4 | 5 | 3 | 0 | 1 | 1 | 0 |
| avi_and_others | 4 | 0 | 0 | 3 | 1 | 0 |
| audio | 6 | 2 | 0 | 2 | 2 | 0 |
| images_and_documents | 4 | 0 | 0 | 2 | 2 | 0 |
| subtitles | 3 | 3 | 0 | 0 | 0 | 0 |
| content_identity | 3 | 2 | 0 | 0 | 1 | 0 |
| media_additions | 19 | 5 | 0 | 0 | 14 | 0 |
| **all** | **294** | **96** | **24** | **31** | **143** | **0** |

### 4.3 Counts per catalog section

| Catalog section | Items | shown | stored_not_shown | parsed_not_stored | not_collected |
|---|---|---|---|---|---|
| NNTP transport, overview and article headers (catalog L24-310) | 83 | 17 | 7 | 9 | 50 |
| yEnc framing and encoding quirks (L312-456) | 37 | 12 | 7 | 1 | 17 |
| Derived signals, poster habits, timing and NZB (L458-665) | 49 | 18 | 7 | 3 | 21 |
| Payload contents: PAR2, SFV, NFO, SRR and archives (L667-934) | 71 | 28 | 3 | 9 | 31 |
| Container and media metadata (L936-1141) | 54 | 21 | 0 | 9 | 24 |

---

## 5. Extension points and risks

### 5.1 Where the feature plugs in

1. **Rendering**: a new module (for example `src/web_meta.rs`, name to agree) exposing one function that returns HTML panels built with `panel()`/`fact`-style writers, appended inside `release_html` after `{files}` or between `{probe}` and `{files}` (web.rs:1192-1194), per coordination.md:4 ("our rendering lives in a separate module, one call appended inside release_html, own shard tables"). Inputs it already has in scope: `r`, `inner`, `names`, `contents`, `media`, and the `ProbeInfo` it can read itself (as web.rs:1163-1170 does). For derivations over articles (per-part repeats, volume sizes, id grammar, tail bytes) reuse `store::each_article` (store.rs:1947), which `Contents` already walks once per view; or make the heavy part a lazy route (`/ui/meta?id=`) on the `#title-details` pattern (web.rs:62, 1190, 1657-1663) so the page never waits on blob decoding.
2. **Results rows**: a per-row drawer as its own htmx-loaded row; do not edit the Size cell or `release_html`'s Size fact (coordination.md:16-17; PR #64 adds `size_note`/`recovery_tag` there).
3. **Storage**: new `ShardTable`s `segment_crc (file_id, part, crc32, size)`, `par2_files (release_id, seq, name, size, md5, md5_16k, crc32, source)`, `article_headers` (coordination.md:10; catalog L1183-1192) registered in `SHARD_TABLES`; `added` columns on `archive_inner_files` (crc32, mtime, method, host_os) and `archive_release` (salt, iv, header_crc); indexed columns on `release_probe` for posting-account/host/tool (catalog rank 5-6). Each needs the compact/convert/wipe tests.
4. **Fetching**: extend `ProbeInfo` (new `Option`/`#[serde(default)]` fields only), extend `KEPT_HEADERS` or keep the full header list, and bump `ProbeStep::config_stamp` with a **new** bit; extend `Par2Step` to write `par2_files` and give it a stamp bit. Keep every new read inside the articles already fetched (first/last of the main file, PAR2 index, NFO) so the backfill cost is one re-probe, not new traffic (catalog L1164-1177).
5. **Ids and bits**: StepId 3 is the only free body-lane id once #51 (6) and #64 (7) merge; a Database-lane derivation step fits 8..=15 (ranks 220-299 free) or 16..=23. Probe stamp bits: main `1 | 2`; pr/48 sets `3` (branch probe/step.rs:99-101), which is the same value as main's `1 | 2`, so after a rebase #48 must move cover to bit 4 or its cover backfill never triggers; this feature should take bit 8 (or 4 if #48 does not take it) and say so in coordination.md.
6. **API**: anything added to `MediaAttrs`/`release_attrs` changes what Sonarr/Radarr parse (api.rs:285-310; media/query.rs:44-49); new attrs belong under `extended=1` and `caps` (catalog L1194).
7. **Screenshots and tests**: `.claude/skills/run-atlas` for `pr-shots`; unit tests next to `probe_html`'s (web.rs:1856-2217); fixtures hand-written, no live names (docs/port/00-overview.md:55, 60).

### 5.2 Conflicts with open PRs and threads (diffs against main 55d2e51)

| Branch (head) | Merge base | Touches | Collision with this feature |
|---|---|---|---|
| `pr/48` 06847b2 "Show cover art, attachments, every stream field and the NZB's files on the release page" (branch `feat/release-page-media-cover`; coordination.md:6 calls it `feat/release-nzb-files`) | a29ee52 | web.rs +147/-7: route at 61 (`/ui/cover`), `release_html` hunks after the Links fact (1091-1150), `probe_html` signature gains `cover`, hunks 1228-1480 (every stream field: display aspect, primaries, transfer names, audio rate, subtitles, chapters), new `nzb_files_html`, new routes after `poster` (1685-1770); release_info.rs `NzbFile`/`Contents.list`; probe/mod.rs `release_cover` table (:50-60) and `COVER` in schema.rs:87; probe/media.rs attachments + cover; probe/step.rs stamp `3`; tests/web_ui.rs | Textual: any fact inserted at web.rs:1139-1142, the page template (1183-1200), and `probe_html`'s signature/body. Semantic: it owns stream/HDR/attachment/cover facts and the NZB file list; this feature must not render those (coordination.md:6, 9). Stamp-bit collision as in 5.1 item 5 |
| stream-details branch (stacked on #48, unpushed, no PR) | - | `probe_html`, `details_html`: HDR summary, encoder + key options from Matroska Tags, SegmentUID, edition count, default-track flags, runtime vs TMDB (coordination.md:7) | Owns SegmentUID, encoder strings, editions, default flags; this feature keeps the rest of Segment.Info (DateUTC, family), muxer apps, Cues/Tags presence, MP4 atoms, TS/DVD/BD, audio-file tags (coordination.md:9). A provenance or metadata block appended inside `details_html` collides; use a separate panel |
| `pr/51` 7562a82, `pr/54` 1e1949f (stills + H.264/HEVC intra decoder; 57 files, +7069) | ea5a7fc | `src/probe/stills.rs` (StepId 6, `release_stills` table, schema.rs:88), `src/video/*`, `probe/step.rs` makes `main_file`/`fetch_raw`/`read_part` `pub(super)`, `probe/media.rs` `element_id/element_size` `pub(crate)`, `details.rs` `trim_posters` visibility; no web.rs | StepId 6 taken; helper visibility changes in `probe/step.rs` may conflict textually with probe edits |
| `origin/feat/release-stills-strip` 2c5a0a4 (the #60 follow-up) | ea5a7fc | web.rs +67/-4: route at 61 (`/ui/still`), `release_html` 1110-1121 (`stills_html` call), template gains `{stills}` before `{copies}`, `stills_html` after `details_html` (1661-1712), tests 1816-1890 | Page template and anything appended right after `details_html` |
| `pr/55` 6a0ff6d "Decode yEnc with the crate shared with newznab-tmux" | 55d2e51 | nntp.rs +205 (`yenc_decode` rewrite, nntmux-yenc dep), Cargo.toml | Any yEnc syntax/escape statistics must be taken from the new decoder's output; coordinate before touching `yenc_decode` |
| `pr/57` e1ad6c3 "Sort results instantly from a window kept in memory" | 55d2e51 | web.rs +495/-51 in the Search/results code (hunks 5-730), api.rs `keep_sort_windows_warm`, tests/web_sort_speed.rs | Only the results-row drawer (row(), RowExtras, results()) |
| `pr/58` 44c336c "Decode compressed RAR5 layers" | 55d2e51 | probe/archive.rs `rar_version`/`solid` `serde(skip)`, nest.rs `Codec::Rar5`, 26 files | `ArchiveHead` gains fields this feature might want to *store* (rar_version, solid are `serde(skip)` there): agree on un-skipping them instead of adding parallel fields |
| `origin/claude/project-thread-lgvcpf` 7bb0788 = PR #64 (download size / upload links) | 55d2e51 | 10 files, +858/-44: `src/archive/link.rs` (451 lines: `LINK_STEP = StepId(7)` :26, rank 150 :293-295, Needs::Bodies, `upload_link (release_id, lead_id, recovery)` + index `upload_link_lead` :29-36, reuses `CLEAR_CONTENT`), schema.rs +1 (:85), steps.rs +1, release_info.rs +206 (`contents_of(ids, recovery)`, `RecoveryOnly`, `recovery_only_with/of`, `whole_post_with`, `SplitPost.recovery`), web.rs +142 (`size_note`, `recovery_warning`, `recovery_tag` at 699-760, `row` at 820, `release_html` gains the last parameter `recovery` and a `{warning}` slot, `release_page` calls `whole_post_with`), nzb.rs +20, api.rs +5, mock_nntp.py +58 | `release_html`'s signature and template; `release_page`'s split fold (web.rs:1007-1015); `Contents` construction; StepId 7 and rank 150 taken; rebase whichever merges second (coordination.md:34-37) |
| Finish web UI / twin grouping thread (offline since 07:25Z, name-grouping step unwritten) | - | `copy_card`/`copies_html`/`duplicates_page` (web.rs:1463-1654) and twin keys | `segment_crc` and `par2_files` are twin keys (catalog L1224): agree the tables there before either side builds on them; copies cards must mirror any new per-release fact or an agreed rule limits them to name-derived facts |
| `pr/21` cec2f5e | none (unrelated history) | - | ignore |

### 5.3 Risks

1. **Probe JSON evolution** (probe/mod.rs:142-148): `probe::read` returns None on any deserialisation error; a renamed or non-default new field in `ProbeInfo` silently removes every probe panel for old rows until the re-probe reaches them (newest first, months for the tail). Only add `Option`/`#[serde(default)]` fields; never rename.
2. **Backfill size**: render-only = zero; a Database walk = 16.4 h of walking over 118M rows at 2000/s (engine.rs:421) plus writer serialisation and up to 12 GB of WAL per `releases` column (README.md:512); a re-probe = 15M body-gate releases (README.md:405) at 60-600 bodies a minute = 17-174 days; per-article CRCs for all 1.77B articles are out of reach (storage.md), the probe-fed subset is 2 CRCs per probed release = about 30M rows (1-3 GB); PAR2 per-file rows = about 7M index articles (about 8 days at 600/min); a provider re-lookup of 1M titles = 3-6 days; details backfill about 14 h per 100k titles (media-title-resolution.md §8). New releases get everything at ingest time once the steps carry the fields.
3. **Request-thread latency**: `details_panel` already calls providers synchronously when cold (media/details.rs:441-483); `Contents` decodes sealed blobs per view; a new panel must not add a network call or a second blob decode on the request thread.
4. **Compaction and conversion**: every new table or column must survive `--compact` (compact.rs:745, 901, 1049 only know `SHARD_TABLES`), `convert.rs` and the wipe, with tests (docs/port/00-overview.md:57). No new index on a populated table (schema.rs:112-113); partial indexes only.
5. **API stability**: `MediaAttrs` and `release_attrs` are what Sonarr/Radarr/Hydra parse (api.rs:285-310); `imdbid` stays 7 digits; new attrs only behind `extended=1`.
6. **Privacy**: RAR5 `UOWNER`, RAR4 `UO`, iTunes `apID/purd/ownr`, EXIF serials/GPS, `NNTP-Posting-Host` and Nyuu's default From leak accounts and hosts (catalog L842-845, L1093-1096, L1222); store hashes or nothing, never display, never index raw.
7. **Two name parsers**: `release_tags`/PTT (render) and `media::parse` (stored) disagree on titles (media-title-resolution.md §9 items 4-5); showing both invites contradictions. The daily-show label `S2024E315` (web.rs:737) is pinned by a test (web.rs:2204-2208).
8. **Defaults**: TVmaze is on by default (config.rs:237), so provider traffic exists on a default install; `predb.enabled` is false (config.rs:378); `archive.enabled` gates the probe, archive and NFO steps (probe/step.rs:87-88) and `body_fetches_per_minute = 600` (config.rs:629) is the code default even where docs say 0. Rule 8 (docs/port/00-overview.md:58): new fetches must not change a configured install's network load.
9. **Caps**: `archive_inner_files` is written to 64 rows and read to 256 (archive/mod.rs:27; archive/store.rs:93); `KEPT_HEADERS` values are cut at 512 chars (probe/mod.rs:100); `MAX_NFO` 256 KiB; `groups_of` 64 rows per shard; `FULL_COPIES` 12. A "every field" panel must state what was cut.
10. **The hdr label** is "HDR10" whenever MasteringMetadata exists and "PQ" otherwise (catalog L953); showing more HDR detail belongs to the stream-details branch.
11. **Coordination debt**: the #48 stamp-bit collision (5.1 item 5), the StepId/rank budget (one free body-lane id), and the `release_html` signature churn (#64 and #48 both edit it) mean the merge order decides who rebases; agree the order before writing the appended call.
12. **Open test failures** (tests-and-conventions.md): `an_nfo_is_skipped_once_named` fails by reading (net-lane `applies`/ctx staleness around engine.rs:899/951) and `a_set_with_only_volumes_reads_first_and_last` could not be reproduced by reading; CI runs fmt, clippy `-D warnings` and `test --locked` on three OSes, so a flaky baseline must be checked before adding tests.

---

## 6. Contradictions found and how they were resolved; open questions

### 6.1 Contradictions between readers, catalog and code

| # | Claim(s) | Resolution (what main says) |
|---|---|---|
| 1 | api-cli.md:139: only the API parses the release name | Wrong: `release_tags::parse` runs in web.rs:1053 (`release_html`), 1464 (`copy_card`) and in the results/duplicates code (web.rs:396, 758, 1613 per web-ui.md); the API does it at api.rs:286 |
| 2 | probe.md: `overview_row.subject` is shown; others: stored_not_shown | stored_not_shown: web.rs:1087-1090 prints `names.posted` (the release name as posted), nothing reads `files.subject` except nzb.rs:83-88 |
| 3 | `obfuscation_level` shown vs partial | The boolean (OBFUSCATED tag, `names.obfuscated`) and the layer source ("Name from") are shown; a graded level is not computed: shown (primary), level not_collected |
| 4 | `subject_parse` statuses differed by side | Name side shown (facts from `release_tags`), filename side stored (files.filename) and only counted; one row, status shown with the split in evidence |
| 5 | `nzb_identity` | The twin/alt relation is shown (copies panel); a literal segment-id digest does not exist; shown with the caveat |
| 6 | `repost_and_retry_events` partial (catalog L572) | Nothing counts repeats: same-id repeats are dropped by the primary key, new-id reposts become extra rows nobody counts (store.rs:181-188, 645-705): not_collected, but derivable at render from stored rows |
| 7 | storage.md: `release_fingerprint` written by store.rs | Writers are `Par2Step` (archive/steps.rs:256-268), the ListStep finish (:134-138) and `forget_old_twins` (duplicates.rs:97); store.rs never touches it |
| 8 | `from_identity`/`date_shape` shown vs parsed | The raw strings are shown (web.rs:1079-1085, 1294-1297); the derived properties are not computed: shown (raw) with "render only" for the properties |
| 9 | `external_oracles`: unclear / stored_not_shown / absent | Composite rule (media-title-resolution.md §12): the primary values (ids, titles, PreDB name) are shown; provenance, pre time, nuke are stored_not_shown; `tvairdate`/`rageid` go out in the API only and do not make a value "shown" |
| 10 | storage.md:81: `predb_match` read by naming and web | Only `metrics/catalog.rs:442-464` reads it on main (predb.md) |
| 11 | ingest-postprocess.md:414: `/ui/details` is the generic details panel | It is the media details panel only (web.rs:1657-1663, 1679-1715) |
| 12 | Probe stamp: notes said main `1`, #48 `3` adds a bit | main is `1 \| 2` (probe/step.rs:99-101, #52 added bit 2); pr/48 is literally `3`, the same value, so #48's cover bit collides with #52's nested-archive bit and must move to 4 after rebase; this feature takes the next free bit |
| 13 | docs: `body_fetches_per_minute` default 0 | config.rs:629: 600 |
| 14 | coordination.md:6: #48 is `feat/release-nzb-files` | The local ref `pr/48` is `feat/release-page-media-cover`, first commit ff8e66b "Show cover art, attachments, every stream field and the NZB's files on the release page" (media-title-resolution.md §11 item 7); confirm with the owner before citing a branch |
| 15 | stream-details "PR after #48" | Not pushed; no ref in the clone; treat as a design intent, not a diff |
| 16 | "#60" | The local `origin/feat/release-stills-strip` 2c5a0a4 is the stills strip; there is no `pr/60` ref; #60's number comes from the coordination notes |
| 17 | media-title-resolution.md:276: #64 "not in this clone" | It is now: `origin/claude/project-thread-lgvcpf` 7bb0788, merge base 55d2e51, verified in 5.2 |
| 18 | Catalog `atlas:` notes checked at a29ee52 | Drift on main: `.001` is `Kind::Numbered` and probed (select.rs:36, 70, 104-110; catalog L921 says NotArchive); `compressed_prefix_decode` (L853) and tar/gzip/xz (L885) are recognised as Layers by #52 (nest.rs:26; shown at web.rs:1322-1325); zip CRC is blanked when bit 3 is set (probe/archive.rs:285-286; L795 describes the old bogus 00000000); Main-packet non-recoverable ids are merged (par2.rs:96); the catalog's `parts_agree` (L382) does not exist as a field, only `continuity_ok` (probe/mod.rs:61); the catalog header's "rendered on the HTML release page only, never in the API" for the probe JSON holds |
| 19 | docs/port/08-media-ids.md:556: no provider by default | config.rs:237: TVmaze enabled by default |
| 20 | `archive_inner_files` cap 64 vs 256 | Both real: 64 written (archive/mod.rs:27), 256 read (archive/store.rs:93) |
| 21 | docs/port/06-predb.md line numbers; README.md:435 "JSON feeds" | Stale line numbers; 33 mixed-format feeds (predb.md) |
| 22 | web-ui.md §9 "UNVERIFIED" PR impact | Superseded by the diffs in 5.2 (#48 touches `details.rs` only at `trim_posters`; #51/#54 touch schema.rs +1 and `details.rs` one line; none changes `MediaAttrs` or media stamps) |
| 23 | probe panel order | Notes and code agree: Tracks, Chapters, yEnc, Packing, How it was posted, NFO, SFV (web.rs:1445-1452) |
| 24 | KEPT_HEADERS count "12" and which are printed | 12 names (probe/mod.rs:81-94); 8 are printed raw, 3 fold into Posted with, Xref becomes Crossposted to (web.rs:1292-1300) |

### 6.2 Open questions

1. Panel placement and granularity: one "Everything Atlas knows" panel appended after `{files}`, or several panels per catalog layer (Headers, yEnc, Archive, Media, Keys), or a lazy `/ui/meta` route? The page already has seven panel kinds; the twin thread and #48/#60 add more.
2. Which name is canonical where `release_tags` and `media::parse` disagree, and should both be shown (media-title-resolution.md §11 item 1)?
3. Should provenance (which provider answered, when, from which cache state; PreDB kind/pre time/nuke) be shown when the only provider on is TVmaze and the details panel is empty (item 2)?
4. `imdb_from_nfo` is set by both the NFO and the moviehash path (steps.rs:98-99; opensubtitles.rs:55-56): split before printing "IMDb from NFO" (item 3)?
5. How does the web process read `media_titles` and `atlas.predb.db`: per-request read-only opens, or attached to the kept readers (db.rs:27-47)? The port doc rejects a read-time join for the API (08-media-ids.md:1090); for a UI line that degrades to "unknown" it is acceptable if stated.
6. Per-article tables: `segment_crc` keyed by `(file_id, part)` vs by message-id hash; blob v2 vs side table; who owns them (twin thread) and the agreed `TWIN` rule "size + CRC32 + 16k or full MD5" (coordination.md:29).
7. Stamp bits and ids: confirm #48 moves to bit 4 and whether this feature takes 8; confirm StepId 3 / rank 140 for a body-lane step and the Database-lane id for derivations.
8. Which crates are acceptable under the widely-used-crates rule for CRC combine (`crc32fast`), decoders (`flate2`, `lzma-rust2`), `dolby_vision`, `hdr10plus` (catalog L1218-1219)?
9. Privacy policy for header and archive leaks (NNTP-Posting-Host raw is already printed; `UOWNER`, `apID`): display rules and hashing before any indexing.
10. The results-row drawer: separate htmx row (coordination.md:17) versus nothing on rows at all; PR #57's rewrite of the results code decides the merge order.
11. Whether to show the per-file posted name list ourselves or wait for #48's `nzb_files_html` (coordination.md:6).
12. Whether `X-Received-*` headers are injection-time or reader-side (catalog L1217); the posting-behavior probe kit answers per provider before header keys are built.
13. Showing catalog items whose value Atlas has for the main file only (yEnc name/size/CRC, archive head): label as "main file" or extend to every file (N4)?
14. The two failing tests (5.3 item 12) and whether the baseline must be fixed before the UI PR lands.
