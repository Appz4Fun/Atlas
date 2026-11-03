# Atlas web UI map (area: web-ui)

Repo: `/home/claude/appz4fun/atlas`, branch `main` at 55d2e51 ("Follow archives inside archives, up to 10 layers (#52)"). Read-only survey; nothing built or run. Every claim cites `file:line` of that tree. The catalog this is matched against is `/tmp/claude-0/-home-claude/b80d34bc-6432-5cb9-a471-5c997487e01d/scratchpad/catalog-current.md` (1625 lines).

GitHub could not be reached from this session (`gh pr list` → GraphQL "not available from Claude Code sessions"; `gh api repos/Appz4Fun/atlas/pulls` → HTTP 403 "GitHub access to this repository is not enabled for this session"). Only `main` exists locally, working tree clean. Everything said below about PRs #48, #51, #54, the stream-details branch and the Finish-web-UI thread is inferred from the code areas their titles imply and is marked UNVERIFIED.

---

## 1. Architecture

### 1.1 Server and routing
- One tiny_http server, `WORKERS = 4` threads (`src/api.rs:18`, `src/api.rs:498-520`), started from `src/app.rs:801` (`api::start`).
- Every GET is first offered to `web::get` and only then falls through to the Newznab API (`src/api.rs:436-445`). POST goes to `web::post` when `web::takes_post(path)` (`src/web.rs:93-95`).
- `web::get(path, query, cookies, base, api_key)` (`src/web.rs:48-72`) routes:
  - `/static/*` — bundled assets, no sign-in required.
  - `/` — `search_page` when signed in, else `login_page`.
  - `/ui/results` — results fragment (htmx target).
  - `/ui/release?id=` — release page.
  - `/ui/duplicates` — duplicate-clusters page.
  - `/ui/details?id=` — lazily loaded media-details panel.
  - `/ui/poster?kind=&tmdb=[&art=backdrop]` — cached poster/backdrop JPEG.
  - `/ui/nzb?id=[&fill=1]` — `fill=1` → `duplicates::filled_nzb`, else `api::get_nzb`.
  - Unsigned `/ui/details` and `/ui/poster` → 401 with `HX-Redirect`; unsigned `/ui/nzb|release|duplicates` → 303 to `/`.
- `web::post` (`src/web.rs:75-90`): `/login` (sets cookie, 303), `/logout` (clears cookie).
- Session cookie is derived from the API key by SHA-1 (`src/web.rs:137-153`), 30-day lifetime. All HTML gets `Cache-Control: no-store` (`src/web.rs:121-134`).

### 1.2 htmx, CSP, assets
- htmx 2.0.11 is bundled via `include_str!` (`src/web.rs:22-25`) and served under `/static/` with an asset version hash in the URL (`src/web.rs:28-34`).
- htmx config: `allowEval:false, selfRequestsOnly:true, historyCacheSize:0` (`src/web.rs:44-45`). CSP `default-src 'self'` (`src/web.rs:41`), so no inline scripts, no third-party assets.
- htmx attributes used anywhere in `web.rs`: `hx-get`, `hx-target`, `hx-sync`, `hx-indicator`, `hx-trigger` (only lines 969 and 1190), `hx-swap`, `hx-swap-oob`, `hx-include`; response headers `HX-Push-Url` and `HX-Redirect`.
- Exactly one lazy load exists: `<div id="title-details" hx-get="/ui/details?id={id}" hx-trigger="load" hx-swap="outerHTML"></div>` (`src/web.rs:1190`). An empty 200 response makes htmx swap the placeholder away, so a title with no details leaves no trace.

### 1.3 Formatting helpers (`src/ui.rs`)
- `src/ui.rs` is otherwise the terminal "rich" stand-in; the web uses only `fmt_size` (`src/ui.rs:513-529`: `None` → `?`, `<= 0` → `0 B`, one decimal, units B..PB) and `fmt_date` (`src/ui.rs:531-533`: first 10 chars of the stored date string).

---

## 2. Search / results page

- `Search` struct (`src/web.rs:187-196`): `q`, `cat`, `offset`, filters, `Fields { group, poster, email, size, team, crc, agent }`, `sort`, `descending`. `SortBy` (`src/web.rs:201-228`), `sort_rows` (`src/web.rs:231-267`), `SORT_WINDOW = 2_000` (`src/web.rs:199`).
- `FILTERS = ["2160p","1080p","DV","HDR","Remux","WEB-DL","BluRay", HIDE_COPIES]` (`src/web.rs:274`).
- `search_page` (`src/web.rs:961-986`): form with `hx-trigger="submit, input changed delay:300ms from:#q, change target:.scope-field"` (line 969), `#busy` indicator, `#failure` slot. `scope_inputs` (`src/web.rs:883-914`) renders the "More fields" inputs (group, poster, email, size, team, crc, agent). `category_nav` (`src/web.rs:917-938`). `page()` (`src/web.rs:940-947`) and `masthead()` (`src/web.rs:949-959`) wrap every page.
- `results()` (`src/web.rs:845-865`) returns the table plus `hx-swap-oob` fragments and sets `HX-Push-Url`.
- `Search::body()` (`src/web.rs:464-587`) runs `api::find_releases` and then, per row: `release_info::split_post_with` (split posts across groups), `duplicates::later_copies`, `categories_of`, `archive::store::passwords`, `media::query::attrs_of`, `duplicates::counts`, `media::details::kept_for` (no fetch).
- `row()` (`src/web.rs:744-797`), `COLUMNS = 6` (`src/web.rs:692`), `table()` (`src/web.rs:817-826`). A row shows:
  - thumbnail when `media::details::poster_kept` (`src/media/details.rs:546-548`);
  - name (display name, `coalesce(r.display_name, r.name)` from `src/search.rs:33-34`) linking to `/ui/release?id=`;
  - tags: `media_label` (S01E02 / S01 / year, `src/web.rs:735-742`), `name_tags` (resolution `tag-ref`, hdr `tag-ok`, quality plain, `src/web.rs:720-732`), `best_score` (`src/web.rs:712-717`), `N TWINS` / `N ALTS` (from `duplicates::counts`, `CopyCount { twins, alts }` `src/duplicates.rs:339-342`), `N GROUPS` (split posts), `PASSWORD` / `PASSWORD?` (`password_tag`, `src/web.rs:698-704`), `INCOMPLETE`;
  - `col-cat` (category), `col-size` with `title="{bytes} bytes"` (`thousands`, `src/web.rs:805-815`), `col-posted` (`fmt_date`), `col-group`, NZB button.
- Search links: `search_link_to` (`src/web.rs:800-802`) builds `/?q=`, `/?group=`, `/?poster=`, `/?size=`, `/?team=`, `/?crc=`, `/?agent=`; `src/field_search.rs:192-197, 211-216` search `release_probe.crc32` / `agent`; poster/email searches need `atlas --search-indexes` (`src/field_search.rs:357-380`).

---

## 3. Release page (`/ui/release?id=`)

### 3.1 Assembly (`release_page`, `src/web.rs:989-1017`)
1. `search::get_release(id)` (`src/search.rs:552-560`) → `ReleaseRow { id, name, group_name, poster, posted_date, size, complete, parts }` (`src/search.rs:9-18`). Missing → 404 "there is no such release"; DB error → 500.
2. `category::categories_of` (`src/category/mod.rs:21-45`) from `release_category(release_id, category)` (`src/category/step.rs:13-22`).
3. `archive::store::view(id)` (`src/archive/store.rs:171-185`) → `InnerView { password, encryption, names_hidden, password_check, files }` (`src/archive/store.rs:152-158`), reading `archive_release` (`src/archive/store.rs:42-51`), `archive_inner_files` (≤ 256 rows, `src/archive/store.rs:18-39, 93`), `release_nfo_password` (`src/archive/store.rs:54-63`).
4. `media::query::attrs_of` → `MediaAttrs { kind, season, episode, air_date, year, ids }` (`src/media/query.rs:315-322`; SQL at 339 reads `air_date`, which the UI never prints) from `media_release` (`src/media/steps.rs:25-49`).
5. `release_info::names(id)` → `Names { posted, display, obfuscated, source }` (`src/release_info.rs:15-21`, SQL 137-139: `name, display_name, is_obfuscated, name_source`).
6. `release_info::contents(id)` → `Contents` (`src/release_info.rs:38-60`) computed by walking the `files` table and every article with `store::each_article` (`src/release_info.rs:160-201`), with `par2_block/blocks` from `duplicates::par2_sizes` (`src/duplicates.rs:469-475`). Nothing of this is stored; it is recomputed on every page view.
7. Split-post merge (`src/web.rs:1007-1015`): when `split_post_with` finds the same post in other groups, `contents`, `size`, `complete` and `parts = articles_seen` are replaced by the merged values.

### 3.2 Layout (`release_html`, `src/web.rs:1028-1201`)
Order in the DOM (`src/web.rs:1183-1200`): `masthead`, `<div class="detail"><main>`, `<p class="path">atlas::releases::<b>{id}</b></p>` (comment at `src/web.rs:621`: "Rust-style path of where this is, as on the RustyGet site"), `<h1>{display name}</h1>`, `<p>` of tags, `<div class="actions">` (Download NZB, "NZB, gaps filled from a twin (check it with PAR2)" when `duplicates::can_fill` `src/duplicates.rs:522-524`, "More in this category"), the `#title-details` lazy div (1190), `<div class="panel"><dl class="facts">…</dl></div>`, then `{copies}`, `{probe}`, `{files}`. Page title `"{name} · Atlas"`.

Tags line (`src/web.rs:1030-1046` region): `media_label`, `password_tag`, `COMPLETE`/`INCOMPLETE` (`r.complete`), `OBFUSCATED` (`names.obfuscated`), `name_tags(parsed)` where `parsed = release_tags::parse(display name)`.

### 3.3 The facts panel: every row and where its value comes from
The closure (`src/web.rs:1048-1052`):
```rust
let mut fact = |name: &str, value: String| {
    if !value.is_empty() { let _ = write!(facts, "<dt>{name}</dt><dd>{value}</dd>"); }
};
```
A row with an empty value is simply not emitted. Rows, in order:

| dt | value | source |
|---|---|---|
| Category | link `/?cat=` | `release_category` via `categories_of` |
| Title | `parsed.title` → `/?q=` | `release_tags::Tags.title` parsed from the display name at page time (`src/release_tags.rs:7-32`) |
| Size | `fmt_size(size)` + `thousands(bytes)` → `/?size=` | `releases.size` (or merged split-post size) |
| Posted | `fmt_date` (10 chars) | `releases.posted_date` |
| Groups / Group | when `groups_of(posted_name)` finds > 1 group: each group with its article count; else one `Group` link `/?group=` | `release_info::groups_of` (`src/release_info.rs:203-235`, per-shard by name, limit 64) / `releases.group_name` |
| Poster | `parse_poster(raw)` display name → `/?poster=raw` | `releases.poster` (`release_info::Poster`, `src/release_info.rs:443-472`) |
| Email | address, plus `MADE UP` tag when `looks_made_up` (`src/release_info.rs:501-508`) | `releases.poster` |
| Posted as | only when `names.posted != r.name` | `releases.name` vs `display_name` |
| Name from | `name_source_label` (`src/release_info.rs:24-35`) | `releases.name_source` (`NameSource` enum `src/naming/mod.rs:28-41`: Ingest 0, Cleared 1, PostedFile 3, NamingRegex 5, InlineBody 9, Nfo 10, ArchiveInner 15, Par2 20, PreDb 100) |
| Year | `parsed.year` | release_tags |
| Ripped from | `parsed.network` | release_tags |
| Site | `parsed.site` | release_tags |
| Video | codec, bit_depth, container joined | release_tags |
| Audio | audio + channels | release_tags |
| Languages | `parsed.languages` | release_tags |
| Edition | `parsed.edition` | release_tags |
| Release group | `parsed.group` → `/?team=` | release_tags |
| Parts | `r.parts` | `releases.parts` (or `articles_seen` after split merge) |
| Encryption | `encryption_fact` (`src/web.rs:1763-1771`): `Encryption::label()` (`src/archive/mod.rs:148-156`: "RAR4, AES-128", "RAR5, AES-256", "7z, AES-256", "ZIP, ZipCrypto", "ZIP, AES") + ", file names hidden"; or "Encrypted, type not recorded"; or "Maybe encrypted" | `archive_release.encryption`, `names_hidden`, `password` (`Password { No, Yes, Maybe }` `src/archive/mod.rs:91-95`) |
| Password | `password_fact` (`src/web.rs:1741-1749`): `<code>pw</code> found in {source}` + `WORKS` / `PROBABLY WORKS` / `WRONG` / ", not tried" | `release_info::password_hint_with` (`src/release_info.rs:528-564`: NFO → release name → file name → subject, via `find_password` 510-526) + `archive_release.password_check` (`PasswordTest::accepts/certain` `src/archive/mod.rs:196-206`) |
| Files | `contents.files` | computed `Contents` |
| PAR2 files | "N (M recovery)" | computed `Contents` |
| Articles | `health()` (`src/web.rs:1779-1787`): "seen of expected" + `ALL THERE`, or ", X% missing" + `REPAIRABLE` / `NOT ENOUGH RECOVERY` | `Contents` (`missing_per_mille`, `repairable`, `src/release_info.rs:62-112`), `per_mille` (`src/web.rs:1774-1776`) |
| Repair | `repair_fact` (`src/web.rs:1752-1760`) | `Contents.exact_repair_per_mille / repair_per_mille` |
| Links | `id_links` (`src/web.rs:1790-1811`): IMDb, TMDB movie/tv, TheTVDB, TVmaze, Trakt (tvrage never linked) | `media_release` ids (`Ids { tvdb, tvmaze, trakt, tmdb, imdb, tvrage }` `src/media/resolve.rs:16-23`), `Kind { Tv=1, Movie=2 }` `src/media/title.rs:8-11` |

### 3.4 "Files inside" table (`src/web.rs:1144-1161`)
From `inner.files` (`archive_inner_files`): name and size only. The `source` column (`src/archive/store.rs:18-39`) is read but not displayed. Empty when the archive step has not run.

### 3.5 Copies (`src/web.rs:1171-1181`)
`duplicates::copies(id)` → `copies_html` (`src/web.rs:1555-1572`, `FULL_COPIES = 12` `src/web.rs:1456`) → one `copy_card` per copy (`src/web.rs:1463-1548`), each re-querying the DB for its own facts (`facts-wide`: Size, Group, Poster, Email, PAR2 files, Repair, Articles, Encryption, Password, Video, Audio, Languages, Ripped from, Release group, Links; `copy-poster` 72×108; `scores_html` tags). `Copy { Twin, Alt }` (`src/duplicates.rs:200-203`), `ClusterKind` (`src/duplicates.rs:259-262`), fingerprints in `release_fingerprint(release_id, posted BLOB, content BLOB, par2_block, par2_blocks, posted_names)` (`src/duplicates.rs:21-40`).

### 3.6 Probe panels (`src/web.rs:1163-1170` → `probe_html`, `src/web.rs:1243-1453`)
`db::open` (`src/db.rs:23`) then `probe::read(&conn, r.id)` (`src/probe/mod.rs:142-148`: `select info from s{s}.release_probe`, `serde_json::from_str`) and `probe::opensubtitles::read` (`src/probe/opensubtitles.rs:285-309`). `ProbeInfo` (`src/probe/mod.rs:53-78`) has `#[derive(Default)]` and serde default semantics, so old JSON without new fields still parses.

Panels are emitted in this order: **Tracks**, **Chapters**, **yEnc**, **Packing**, **How it was posted**, then **NFO** and **SFV** (not panels: `<h2>NFO</h2><p>imdb links</p><pre class="nfo">`, and an SFV table).

- **yEnc** (`src/web.rs:1251-1288`): File name (`yenc.name`), File size (`yenc.file_size`), Parts (`yenc.total`), Line length (`yenc.line_len`), Article size (`segment_size`), CRC32 (`file_crc32` `{:08x}` → `/?crc=`), Overhead (`{:.2}%` of `overhead_percent`), Checks (`verdict()` `src/web.rs:1227-1233` → `ALL BYTES COVERED`/`BYTES MISSING` from `continuity_ok`, `PART SIZE OK`/`WRONG` from `part_size_ok`, `PART CRC OK`/`WRONG` from `part_crc_ok`), On the server (`FIRST/MIDDLE/LAST THERE|GONE` from `health` map). Not shown: `YencLines.part, begin, end, yend_size, (part crc)` (`src/probe/yenc.rs:7-15`), `ProbeInfo.file`.
- **How it was posted** (`src/web.rs:1290-1302`): "Posted with" (`agent_of` `src/probe/mod.rs:107-109` → `/?agent=`), then every header in `p.headers` except `User-Agent | X-Newsreader | X-Newsposter | Xref` (line 1295), then "Crossposted to" (`crossposted_to` → `/?group=` each). Headers stored are the `KEPT_HEADERS` (`src/probe/mod.rs:81-94`: Injection-Date, NNTP-Posting-Date, Date, User-Agent, X-Newsreader, X-Newsposter, Path, Injection-Info, Organization, Xref, NNTP-Posting-Host, X-Complaints-To), values cut to 512 chars (`src/probe/mod.rs:100`). So the raw Xref is stored but hidden; it is shown only as the derived crosspost list.
- **Packing** (`src/web.rs:1304-1328`): Format, Method, encryption tag (`NAMES AND FILES ENCRYPTED` / `FILES ENCRYPTED` / `NOT ENCRYPTED`, only when `a.name` is known), First file (`a.name`), Unpacked (`fmt_size`), File CRC32 `{:08x}`. `ArchiveHead.packed` (`src/probe/archive.rs:11-35`) is stored but not shown; `stored_at, continues, data_at, dir` are `serde(skip)` so never stored. "Layers" via `nested_chain` (`src/web.rs:1840-1854`) when `layers.len() > 1 || (archive.is_none() && !layers.is_empty())`: shows format, method, `stop` reason as `tag-bad` uppercase, and the last layer's name; omits `Layer.unpacked, offset, stored, encrypted, capped` (`src/probe/nest.rs:22-51`).
- **Tracks** (`src/web.rs:1330-1396`): Container, Written by (`writing_app`), Muxed by (`muxing_app`), Title, Duration (`h:mm:ss`), one Video row per track (codec, `W×H`, `{:.3} fps`, `N-bit`, hdr, "Dolby Vision profile N[, enhancement layer]"), Audio rows (codec, "N channels", "N Hz", language, Atmos, commentary, name), Subtitles rows (codec, language, forced, SDH, commentary, name). Not shown: `VideoTrack.display_width/display_height, primaries, transfer, dv_level, language` (`src/probe/media.rs:6-76`); `MediaInfo.header_complete` is `serde(skip)`.
- **OpenSubtitles hash** (`src/web.rs:1398-1400`) from `ProbeInfo.moviehash`; **OpenSubtitles match** (`src/web.rs:1401-1414`): title, S..E.., (year) → IMDb link, release_name, from `release_opensubtitles` (`src/probe/opensubtitles.rs:27-48`; `Stored { moviehash, feature }` 280-283). `kind`, `tmdb_id`, `checked_at` of that table are not shown.
- **Chapters** (`src/web.rs:1415-1423`): `h:mm:ss` → name or "Chapter N".
- **NFO** (`src/web.rs:1425-1432`): `imdb_ids` as links, then the whole NFO text in `<pre class="nfo">` (NFO capped at `MAX_NFO = 256*1024` at probe time, `src/probe/step.rs:22`; `MAX_IMDB_IDS = 16` `src/probe/mod.rs:164`; CP437 fallback `nfo_text` `src/probe/mod.rs:150-161`).
- **SFV** (`src/web.rs:1433-1444`): name + crc table from `ProbeInfo.sfv`.

### 3.7 Lazy details panel (`/ui/details`)
`details_panel` (`src/web.rs:1657-1663`): `attrs_of` → `media::details::for_title(media_config, kind, ids)` (`src/media/details.rs:441-483`), which may call TMDB/OMDb/Trakt synchronously on the request thread when the cache (`atlas.details.db`, `KEEP_FOR = 30d` `src/media/details.rs:29`) is cold. `details_html` (`src/web.rs:1679-1715`) renders `<section class="panel title-details">` with poster 171×256 (`/ui/poster?kind&tmdb`), optional backdrop (`art=backdrop`), `<h2>` title, scores (`scores_html` `src/web.rs:1665-1677`: IMDB, RT, MC, TMDB, TRAKT), `<p>` overview, and facts Released, Blu-ray, Genres, Runtime "N min", Network, Status, Cast. `Details` struct `src/media/details.rs:74-94`; SCHEMA `src/media/details.rs:43-70` (`details(kind, tmdb_id, title, overview, genres, runtime, status, network, cast_names, released, bluray, poster, [backdrop], tmdb_score, imdb_score, rotten, metacritic, trakt_score, fetched_at)`, `details_lookups`). `fetched_at` is never shown. `poster()` (`src/web.rs:1717-1739`) serves image/jpeg with `Cache-Control: private, max-age=2592000`; `MAX_POSTER_CACHE = 4 GB` (`src/media/details.rs:35`). Background backfill `backfill_pass` / `start_backfill` (`src/media/details.rs:640-663, 673-686`) started at `src/bg_indexer.rs:1172`, budget `details_per_minute = 120` (`config.example.toml:53-90`).

### 3.8 Duplicates page
`duplicates_page` (`src/web.rs:1575-1654`): `CLUSTERS_PER_PAGE = 10`, `FULL_MEMBERS = 6` (`src/web.rs:1457-1459`), `copy_link` (`src/web.rs:1550-1552`).

---

## 4. Conventions the UI follows

1. **Panel = `<h2>` + `.panel` + `dl.facts`.** `panel(title, facts)` (`src/web.rs:1235-1240`) returns an empty string when `facts` is empty, so an entire panel disappears when nothing in it has data. Inside, `fact()`/`dd()` closures (`src/web.rs:1048`, `1488`, and the local ones in `probe_html`) skip empty values, so individual rows disappear too. There is no "unknown"/"—" placeholder anywhere: missing data is absence. Only `fmt_size(None)` prints `?` (`src/ui.rs:513-529`).
2. **Tags**: `tag(class, text)` (`src/web.rs:694-696`) → `<span class="tag {class}">TEXT</span>`; classes `tag-ok` (patina, healthy), `tag-ref` (steel, reference), `tag-warn` (amber, attention), `tag-bad` (ember, failure), plain `tag` (`assets/web.css:187-191`). Rule stated in `assets/web.css:1-5`: "Each accent keeps one job: rust for action, patina for healthy, steel for reference, amber for attention, ember for failure."
3. **Verdicts**: `verdict(Option<bool>, good, bad)` (`src/web.rs:1227-1233`) → `tag-ok` or `tag-bad`, nothing for `None`.
4. **Callouts**: `diag(level, title, body)` (`src/web.rs:829-835`) → `<div class="diag diag-{level}" role="alert">`; comment at 828: "A compiler-style callout, the way Oxide writes them". CSS `assets/web.css:193-200`.
5. **Everything searchable is a link** back into search (`search_link_to`, `src/web.rs:800-802`); README `README.md:373-385` promises "CRC32 and posting tool can be clicked or searched on, under More fields".
6. **Numbers**: `fmt_size` one decimal with full byte count in a `title` or in parentheses (`thousands`); percentages `{:.2}%` (overhead) or per-mille → `%` (`per_mille`); durations `h:mm:ss`; dates 10 chars.
7. **Long values**: `dd`, `td.name`, `h1`, `.path`, `code` have `overflow-wrap:anywhere` (`assets/web.css:205` and nearby); `td` default `white-space:nowrap`; `.table-wrap { overflow-x:auto }`; `pre.nfo { overflow-x:auto }` (`assets/web.css:225`). Header values are cut to 512 chars at probe time, NFO at 256 KiB; nothing else is truncated at render time.
8. **Escaping**: all user/Usenet strings go through `api::escape` (`src/api.rs:89`).
9. **Responsive**: `.detail { max-width:1100px }` (`assets/web.css:234`); `@media (max-width:1000px)` hides `.col-group` (`assets/web.css:240-242`); `@media (max-width:820px)` (`assets/web.css:243-258`) makes `.facts-wide` 2 columns, hides `col-cat/size/posted`, shows `.row-meta`, stacks `.title-details`.
10. **Grid**: `.facts { grid-template-columns: max-content minmax(0,1fr) }` (`assets/web.css:203`), `dt` mono uppercase muted (`assets/web.css:204`), `.facts-wide` 4 columns (`assets/web.css:214-221`).

### 4.1 Oxide design system (RustyGet) — local notes
- `assets/web.css:1-5`: "Oxide, the design system of RustyGet: https://appz4fun.github.io/RustyGet/design-system.html. The token block is copied from its site/assets/site.css." Tokens `assets/web.css:7-34`; dark theme blocks `assets/web.css:36-59` (`prefers-color-scheme: dark` guarded by `:root:not([data-theme="light"])`) and `assets/web.css:61-82` (`:root[data-theme="dark"]`).
- `README.md:606-620` "Web UI" section links Oxide at line 620.
- `src/web.rs:621` and `src/web.rs:828` reference RustyGet/Oxide conventions (the `::` path and compiler-style callouts).
- `assets/web.js` (68 lines): theme toggle persisting `localStorage['theme']` into `data-theme`; `/` focuses `#q`; folds `.navigation` at ≤ 820px; `showFailure()` injects a `diag diag-caution` into `#failure` on `htmx:sendError` / `htmx:responseError` (except 401); refolds on `htmx:afterSwap`.

---

## 5. Tooling for UI work

- `.claude/skills/run-atlas/SKILL.md` + `driver.sh` + `shot.cjs` + `mock_nntp.py`: `cargo build --release --locked` (~4.5 min), `$D up / status / api / shot OUT [PATH] [--dark] [--width N] / screen / keys / down`; `ATLAS_HOME=/tmp/atlas-run`, API key `driver-key`, port 9099. `shot` prints JSON `{out, title, url, results}` and saves a full-page PNG with Playwright. The skill says UI PRs must carry screenshots.
- Mock NNTP has 13 posts (movie RAR/NFO/PAR2, TV mkv, FLAC album, obfuscated, nested RAR5>7z>zip>tar>mkv), answers `ARTICLE` with `User-Agent: Nyuu/0.4.2`; release ids step by 8; new fixtures go in `build_posts()`.
- Tests: `tests/web_ui.rs` (387 lines) is one end-to-end test over a real socket (sign-in, results, filters, sort, fields, release page "Download NZB", 404, 303, split posts "2 GROUPS"/"2 of 2"/"2 of 3", filled NZB, duplicates page, NZB download, logout); `src/web.rs:1856-2217` has unit tests (`health_compares_what_is_missing_with_what_can_be_repaired` 2054, `media_labels_and_id_links` 2204). CI (`.github/workflows/ci.yml`): `cargo fmt --check`, `cargo clippy --all-targets --locked -- -D warnings`, `cargo test --locked` on ubuntu/windows/macos.
- Docs: `README.md:373-385` "Article probes" enumerates what the release page shows (yEnc, Checks, What's in it, How it was posted, NFO and SFV); `README.md:725-727` results tags, details/ratings/backfill. `docs/TODO.md` has no web-UI items. `docs/todo-duplicate-clusters.md` Part 2 "release details" is a checklist of what the release page shows; open items there: RAR4/7z password "not tried", prod has no archive data yet, rating filters.

---

## 6. Data model the web UI reads

| table (per shard s0..s7 unless noted) | columns | written by | read by (web) |
|---|---|---|---|
| `releases` | id, name, group_name, poster, posted_date, size, complete, parts, file_total, display_name, is_obfuscated, + `name_source` (`src/store.rs:133-193`; `src/postprocess/schema.rs:96` RELEASE_COLUMNS) | ingest/bg_indexer, naming steps | `search::get_release`, `find_releases`, `release_info::names`, `groups_of`, `split_post_with` |
| `releases_fts` | FTS over names (`src/store.rs`) | ingest | `find_releases` |
| `files` | id, release_id, filename, subject, subject_part, subject_mid, expected, file_total, seen BLOB, touched_at, blob (`src/store.rs:133-193`) | ingest; sealed into `blob` after `SEAL_AGE` 3 days (`src/store.rs:788`) | `release_info::contents_of` (aggregates only) |
| `segments` | file_id, local, domain, part, bytes (`src/store.rs:133-193`) | ingest | `contents_of` via `store::each_article`; never shown per article |
| `domains`, `meta`, `collections`, `held_back`, `release_ids` | (`src/store.rs:205-231`) | ingest | not read by web |
| `release_category` | release_id, category (`src/category/step.rs:13-22`) | category step | `categories_of` |
| `release_probe` | release_id, crc32, agent, info TEXT (JSON `ProbeInfo`), + moviehash (`src/probe/mod.rs:23-47`) | `ProbeStep` (`src/probe/step.rs:200-207`) | `probe::read` (HTML only; API never reads it), `field_search` on crc32/agent |
| `release_opensubtitles` | release_id, moviehash, imdb_id, tmdb_id, kind, title, year, season, episode, release_name, checked_at (`src/probe/opensubtitles.rs:27-48`) | opensubtitles lookup | `opensubtitles::read` |
| `archive_release` | release_id, password, + encryption, names_hidden, password_check BLOB (`src/archive/store.rs:42-51`) | archive step | `archive::store::view`, `passwords` |
| `archive_inner_files` | release_id, seq, name, size, source (`src/archive/store.rs:18-39`) | archive step | `view` (name, size only) |
| `release_nfo_password` | release_id, password (`src/archive/store.rs:54-63`) | archive/NFO step | `view` |
| `media_release` | release_id, kind, title_key, season, episode, episode_last, air_date, year, quality, tvdb_id, tvmaze_id, trakt_id, tmdb_id, imdb_id, tvrage_id, imdb_from_nfo (`src/media/steps.rs:25-49`) | media step | `attrs_of` (kind, season, episode, year, ids) |
| `media_titles` | (`src/media/store.rs:9-25`) | media resolve | not read by web |
| `release_fingerprint` | release_id, posted BLOB, content BLOB, + par2_block, par2_blocks, posted_names (`src/duplicates.rs:21-40`) | duplicates step | `copies`, `counts`, `later_copies`, `par2_sizes`, `can_fill` |
| `predb_match`, `predb_inner_key` (`src/predb/step.rs:27,44`); predb `titles.nuked, nuke_reason` (`src/predb/db.rs:16-17`) | | predb step | not shown anywhere |
| `atlas.details.db`: `details`, `details_lookups` (`src/media/details.rs:43-70`) | see §3.7 | details fetch + backfill thread | `for_title`, `kept_for`, `poster_kept` |
| `pp_walks` (`src/postprocess/engine.rs:1073-1114`) | 'steps' walks with redo mask | `seed_shard`, `--reprocess` | not read by web |

Migration mechanism: `ShardTable { name, create, indexes, release_key, added }` (`src/postprocess/schema.rs:8-18`), registry `SHARD_TABLES` (`src/postprocess/schema.rs:78-92`), `migrate` (`src/postprocess/schema.rs:114-131`) adds the `added` columns as nullable `ALTER TABLE ADD COLUMN`; `release_key` installs gc triggers. Only additive, nullable columns are possible this way.

Backfill mechanism: each `Step` has `config_stamp()` bits (`src/postprocess/step.rs:58-62`); when `seed_shard` sees a bit it has not seen before it inserts a `pp_walks('steps', redo_mask)` row and re-walks every existing release through that step (`src/postprocess/engine.rs:1073-1114`); `ProbeStep::config_stamp = 1|2` today (`src/probe/step.rs:96-101`), `applies` skips `.jpg` (`src/probe/step.rs:90-92`), `reruns on_growth` (`src/probe/step.rs:93-95`), `FRONT_ARTICLES = 6` (`src/probe/step.rs:217`). `atlas --reprocess` (`src/postprocess/reprocess.rs:25-40`) inserts the same walks on demand. Budgets: `probes_per_minute = 60`, `probe_files = 2`, `body_fetches_per_minute = 600`, `bodies_per_release = 8`, `max_bodies_per_release = 4`, `backlog_per_second = 2000` (`config.example.toml:53-90`).

---

## 7. Catalog coverage, decided from the render code

Legend: shown = printed on the release page (or results row / copy card); stored_not_shown = in a table or in `release_probe.info` but no render path; parsed_not_stored = computed in memory at probe/page time and dropped or recomputed; not_collected = catalog says `atlas: no` and no code reads it.

### shown
- From/poster display name and email (+ MADE UP) — `src/web.rs` Poster/Email facts; `releases.poster`.
- Posted date (10 chars) — `fmt_date`.
- Group(s) with article counts for multi-group posts; crossposted_to from Xref — `groups_of`, probe panel 1290-1302.
- Category — `categories_of`.
- Name tags: title, year, network, site, video codec/bit depth/container, audio/channels, languages, edition, release group, resolution, hdr, quality — `release_tags::Tags` through `name_tags` and the facts.
- Posted name vs display name, `OBFUSCATED`, name source — `Names`.
- Size with exact bytes, parts, files, PAR2 files (+ recovery), articles seen/expected, missing %, `REPAIRABLE`/`NOT ENOUGH RECOVERY`, exact repair % — `Contents`.
- Encryption type, names hidden, password (value, where it was found, verdict) — `InnerView` + `password_hint`.
- yEnc: file name, file size, total parts, line length, segment size, overhead %, CRC32, continuity/part-size/part-CRC verdicts, first/middle/last article presence — `ProbeInfo`.
- Kept headers Injection-Date, NNTP-Posting-Date, Date, Path, Injection-Info, Organization, NNTP-Posting-Host, X-Complaints-To; posting agent (User-Agent/X-Newsreader/X-Newsposter collapsed to "Posted with").
- Archive format, method, encryption flags, first file name, unpacked size, file CRC32; nested layers' format/method/stop/last name.
- MKV/MP4: container, muxing/writing app, title, duration, per-track video (codec, W×H, fps, bit depth, hdr, DV profile/EL), audio (codec, channels, sample rate, language, Atmos, commentary, name), subtitles (codec, language, forced, SDH, commentary, name), chapters.
- OpenSubtitles moviehash and match (title, S/E, year, imdb link, release_name).
- NFO text and IMDb ids; SFV name/crc table.
- Media ids as Links (IMDb, TMDB, TheTVDB, TVmaze, Trakt); season/episode/year label.
- Title details: poster, backdrop, overview, scores (IMDB/RT/MC/TMDB/TRAKT), released, Blu-ray, genres, runtime, network, status, cast.
- Twin/alt counts (results tags), copies with cards, fill-from-twin NZB.
- Inner archive file names and sizes (≤ 256).

### stored_not_shown
- `files.subject`, `files.filename`, `subject_part/mid`, `expected`, `seen`, `touched_at` per file — only aggregated into `Contents` (`src/release_info.rs:160-201`).
- `segments.local/domain/part/bytes` (message-ids and per-article sizes) — only counted.
- `releases.file_total` — never printed (file count comes from `Contents.files`).
- `ProbeInfo.file` (which file the probe is about) — `src/probe/mod.rs:55`, no render.
- `YencLines.part, begin, end, yend_size, pcrc32` — `src/probe/yenc.rs:7-15`, only `name,file_size,line_len,total` rendered.
- Raw `Xref` header — excluded at `src/web.rs:1295`; only the derived group list shows.
- `ArchiveHead.packed` — `src/probe/archive.rs:11-35`.
- `Layer.unpacked, offset, stored, encrypted, capped` — `src/probe/nest.rs:22-51`; `nested_chain` shows format/method/stop/name only.
- `VideoTrack.display_width/display_height, primaries, transfer, dv_level, language` — `src/probe/media.rs:6-76`.
- `archive_inner_files.source` — read, not displayed (`src/web.rs:1144-1161`).
- `release_fingerprint.posted/content/par2_block/par2_blocks/posted_names` — surfaced only as TWIN/ALT tags and repair %.
- `media_release.air_date, episode_last, quality, imdb_from_nfo, title_key`; `Ids.tvrage` (not linked in `id_links` 1790-1811).
- `details.fetched_at`; `release_opensubtitles.kind, tmdb_id, checked_at`.
- predb `titles.nuked, nuke_reason`, `predb_match`, `predb_inner_key` — no read path in web.rs.

### parsed_not_stored
- `release_tags::Tags.seasons, episodes, proper, repack, remastered, hardcoded, dubbed, subbed, three_d` (`src/release_tags.rs:7-32`) — parsed on each page view from the name, never rendered and never stored.
- `MediaInfo.header_complete`; `ArchiveHead.stored_at, continues, data_at, dir`; `Layer.continues` — `#[serde(skip)]`, dropped at probe time.
- `Contents` (files, PAR2 counts, articles, missing, repair) and `PasswordHint` (source, candidate) — recomputed per page view from `files`/`segments`/NFO, nothing persisted.
- Catalog "parsed" items with no persistence anywhere: CAPABILITIES / LIST ACTIVE flags, `References`, `Lines`/`:lines`, article numbers, from_shape, 7z coder details, PAR2 set_id, NFO encoding detection (CP437 fallback decided at render time of `nfo_text`).

### not_collected
- Everything the catalog marks `atlas: no` (no reader exists in `src/`): e.g. full XOVER `Lines`, `Bytes` per article kept as a column, Message-ID shown per article, `References`, NNTP `Newsgroups` header verbatim, PAR2 set id / file ids / file hashes, 7z coder chain, RAR volume count, per-file CRC from PAR2, posting rate, article-size histogram, video bitrate, audio bitrate, MKV tags beyond title, MP4 atoms beyond tracks, subtitle text, sample/proof files, image EXIF, audio album metadata (FLAC/ID3) are not in `ProbeInfo` or any table.

---

## 8. Extension points (where a new field plugs in)

1. **Facts panel**: add a `fact("Name", value)` call in `release_html` (`src/web.rs:1048-1130`) — empty strings vanish automatically; mirror it in `copy_card` (`src/web.rs:1488`) when twins should carry it.
2. **Probe panels**: add `dd()`/`fact()` lines inside the matching panel in `probe_html` (`src/web.rs:1243-1453`), or a new `panel("Title", facts)` call (`src/web.rs:1235-1240`).
3. **New probe data**: add an `Option<T>`/`Vec<T>` field to `ProbeInfo` (`src/probe/mod.rs:53-78`) — `Default` + serde default keep old rows parseable; bump a new bit into `ProbeStep::config_stamp` (`src/probe/step.rs:96-101`) and the engine re-walks all releases (`src/postprocess/engine.rs:1073-1114`) = backfill for free, paid in body fetches.
4. **Headers**: extend `KEPT_HEADERS` (`src/probe/mod.rs:81-94`); anything added shows automatically in "How it was posted" unless excluded at `src/web.rs:1295`.
5. **Searchable columns**: `ShardTable.added` on `PROBE` (`src/probe/mod.rs:46`) + partial index, as `moviehash` was added; wire to `src/field_search.rs` and `scope_inputs` (`src/web.rs:883-914`).
6. **New tables**: add a `ShardTable` to `SHARD_TABLES` (`src/postprocess/schema.rs:78-92`) with `release_key` for gc; per-article data must go in a new table, never on `segments` (sealing at `src/store.rs:788`).
7. **Slow data**: copy the `#title-details` pattern — a `hx-get … hx-trigger="load" hx-swap="outerHTML"` div (`src/web.rs:1190`) served by a new `/ui/...` route in `get()` (`src/web.rs:48-72`) returning a `panel()` or empty 200.
8. **Background backfill thread**: `media::details::start_backfill` (`src/media/details.rs:673-686`, started `src/bg_indexer.rs:1172`) is the model for a budgeted sweep over existing releases outside the step engine.
9. **Explicit re-run**: `atlas --reprocess` (`src/postprocess/reprocess.rs:25-40`).
10. **API parity**: `api::release_attrs` (`src/api.rs:285-310`) and `rss` (`src/api.rs:313-345`) if a field should also reach Newznab clients.
11. **Tests/fixtures**: `tests/web_ui.rs` assertions; `mock_nntp.py build_posts()` for fixtures; `shot.cjs` screenshots for the PR.
12. **Styling**: reuse `tag-*` classes and tokens (`assets/web.css:7-34, 187-191`); `.facts`/`.facts-wide` grids; README "Article probes" list (`README.md:373-385`) should be updated with new rows.
13. **Fields already parsed but dropped** (`Tags.seasons/episodes/proper/repack/...`) can be shown with a one-line `fact()` since they are computed per page anyway.

---

## 9. Risks

- **Release page cost**: one view runs `get_release`, `categories_of`, `view` (3 tables), `attrs_of`, `names`, `contents_of` (walks every article of every file via `each_article`, decoding sealed blobs after 3 days), `groups_of` (all 8 shards by name), `split_post_with`, `copies` + up to 12 `copy_card`s each re-querying (`src/web.rs:1463-1548`), `probe::read` + JSON parse of a possibly 256 KiB NFO, `opensubtitles::read`. With 4 worker threads (`src/api.rs:18`) a few concurrent release opens saturate the server. Any new fact that adds queries or decoding multiplies this.
- **Details panel fetches providers on the request thread** (`for_title`, `src/media/details.rs:441-483`) when cold; a slow TMDB/OMDb/Trakt stalls one of the 4 workers.
- **Backfill size**: a new `config_stamp` bit re-probes every release; at `probes_per_minute = 60` that is N/60 minutes (1M releases ≈ 11.5 days) and only runs when a body budget exists and the post-processing is enabled; `applies` filters only `.jpg`. Re-probing re-fetches up to `FRONT_ARTICLES = 6` bodies per file × `probe_files = 2`.
- **Schema**: shard migrations are add-nullable-column only (`src/postprocess/schema.rs:114-131`); no renames/drops/type changes. `release_probe.info` is page-only JSON: not indexed, not in the API, grows with each added field across 8 shards.
- **`segments` seal**: per-article columns cannot be added there (`src/store.rs:788`); a per-article table of N×parts rows is the largest possible backfill.
- **Missing-data semantics**: the UI's convention is absence, not placeholders; "every field visible on every page" conflicts with `panel()` returning nothing and `fact()` skipping empties (`src/web.rs:1048-1052, 1235-1240`). Showing "unknown" rows is a design change against Oxide's current usage and needs screenshots per the run-atlas skill.
- **Test coverage**: one e2e test (`tests/web_ui.rs`) plus a handful of unit tests; HTML is built with `format!`, no templates, so regressions in escaping/structure are only caught by string assertions.
- **UI PRs need screenshots**: `cargo build --release --locked` ~4.5 min plus Playwright; disk is scarce in this environment.
- **Merge conflicts (UNVERIFIED, GitHub blocked)**: #48 "release page media cover" most likely touches `release_html` layout (`src/web.rs:1183-1200`), `details_html` (`1679-1715`) and `.title-details/.poster/.backdrop` CSS (`assets/web.css:208-212`); the stream-details branch most likely touches the Tracks panel (`src/web.rs:1330-1396`) and `src/probe/media.rs`; #51 mid-file keyframes and #54 H.264 decoder most likely touch `src/probe/step.rs` (`FRONT_ARTICLES`, `config_stamp` bits 96-101) and `ProbeInfo` (`src/probe/mod.rs:53-78`) — a new stamp bit from this feature would collide with theirs; the Finish-web-UI thread's twin grouping touches `row()`/`body()` hide-copies, `copies_html`, `duplicates_page` and `src/duplicates.rs` (the catalog itself notes "The twin UI overlaps the Finish web UI thread's work on grouped twins, so the segment_crc and par2_files tables should be agreed there", catalog lines 1211-1227).
- **Header privacy**: showing every kept header verbatim (Path, Injection-Info, NNTP-Posting-Host) already exposes poster infrastructure; adding more headers (e.g. full Xref) widens that.

---

## 10. Open questions

1. What exactly do PRs #48, #51, #54, the stream-details branch and the Finish-web-UI thread change in `src/web.rs`, `src/probe/*` and `assets/web.css`? Could not be read (HTTP 403 from `gh api`; GraphQL unavailable).
2. Does "visible on every release page" mean a placeholder row for missing values (breaks the `fact()`/`panel()` absence convention) or only "render it whenever it exists"?
3. Backfill scope: is a new `config_stamp` bit (re-probe everything, body cost) acceptable, or should fields derivable from already-stored data (`release_probe.info`, `files`, `segments`) be surfaced without re-fetching?
4. Should new fields also reach the Newznab API (`release_attrs`), or stay HTML-only like all probe data today?
5. Which of the stored-but-hidden items (e.g. `ArchiveHead.packed`, `Layer.unpacked`, `VideoTrack.primaries/transfer/dv_level/language`, `YencLines.begin/end/yend_size`, `media_release.air_date`) are wanted — they need no backfill at all, just render lines.
6. Where should per-article data (message-ids, bytes, CRCs) live given `segments` seals into `files.blob` — a new `ShardTable` agreed with the twin-grouping thread?
