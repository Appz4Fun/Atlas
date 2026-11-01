# Part 8: TV and movie IDs

Every Atlas line number below is at base commit `de598b6`. Parts 1–7 merge first and shift them: find the named function, keep the intent, and record any difference in the PR. Upstream paths are at the commit pinned in `00-overview.md`. Part 3 wins any disagreement.

The parser and its table tests, the `similar_text` port, the score table, the upsert, the fan-out update, the NFO regex and the SQL here were prototyped in a scratch crate before this plan was written. Every row in Task 1 and the `score_table` in Task 2 is that prototype's output. The query plans were read from `EXPLAIN QUERY PLAN` on synthetic shards. How Sonarr and Radarr call an indexer was read from their source: Sonarr `43d500c`, Radarr `4d2fef5`. Provider limits are as published when this was written; section 9 says where each came from.

**Live data.** Hard rule 1 names `atlas.media.db`. Nothing may open the one in the repo root.

## 1. Goal

Sonarr and Radarr can find releases by ID. Atlas parses the title of each release categorized as TV or movie, resolves each distinct title once through documented APIs, caches the answer per title in `atlas.media.db`, and stores the IDs per release in the shards. `t=tvsearch` and `t=movie` answer `tvdbid`, `tvmazeid`, `traktid`, `rid`, `tmdbid`, `imdbid`, `season` and `ep`; results carry `tvdbid`, `imdb`, `season`, `episode` and the rest; and `caps` advertises only what is on. Every provider is off until configured. With none configured, Atlas behaves as before: an ID-only search returns empty, nothing goes out on the network, and everything still indexes and categorizes.

## 2. Non-goals

- No IMDb scraping. `app/Services/ImdbScraper.php` is not ported; IMDb IDs come from a provider, or from the release's own NFO once a body budget is set.
- No TVDB client. See decision 6.
- No episode tables, air dates from a provider, posters, covers, plots, genres, ratings or cast.
- No checking that an episode exists. `season` and `episode` come from the release name.
- No `vid`, `genre`, `t=anime`, `anidbid`, `t=details` or `t=getnfo`. `doubanid` stays in `ID_PARAMS` (`src/api.rs:160`) and stays unanswered.
- No renaming a release from a provider's title, and no re-categorizing from a provider's answer.
- No `newznab:response total` change (`src/api.rs:236`).
- No new dashboard page. Part 3's Post-processing page reports each step by name.

## 3. Source map

In this plan `TvMazeClient.php`, `TmdbClient.php`, `TraktService.php` and `MovieService.php` are in `app/Services/`. `AbstractTvProvider.php` and `TvMazeProvider.php` are in `app/Services/TvProcessing/Providers/`.

| Upstream | Take | Leave behind |
|---|---|---|
| `AbstractTvProvider.php:1125-1167` | The title cut list, `[Group]` and `sample-` prefixes, acronyms (`:1172-1181`) | The name-then-marker double parse: a marker's prefix is the title |
| `AbstractTvProvider.php:1278-1364` | Order and shapes of the season, episode and date rules | `(?P=sep)`, `(?!\d)`, two-digit year dates (decision 18) |
| `AbstractTvProvider.php:1187-1244` | Title aliases, into `data/tv_title_aliases.tsv` | |
| `AbstractTvProvider.php:1395-1415` | Country suffixes | |
| `AbstractTvProvider.php:1431-1464` | Transliteration, removed characters, language tags, trailing cleanup | Per-call `str_replace` lists |
| `AbstractTvProvider.php:49,1469-1490,1644-1688,1701-1706` (`scoreCandidatePhp`, `normalizeCountryCode`) | Scoring: exact 100, else `similar_text` at 75% or better; year within 1 adds 10; a country match adds 2; `UK` is `GB` | "Exact takes 20" for a year more than 1 off, and a country mismatch that doesn't reject (decision 14) |
| `app/Services/NfoService.php:2259` | An IMDb id stated in an NFO | The rest of the service |
| `MovieService.php:1687-1693,1893-1950` | Movie title and year regexes | The `-[A-Z0-9].*$` strip as written: with `/i` it turns `Anti-Hero` into `Anti` (decision 19) |
| `TvMazeClient.php:59,62,92-96,79-88,295-335`, `TvMazeProvider.php:349-426` | `/singlesearch/shows?embed=akas`, then `/search/shows`, 404 as no match, candidate scan including AKAs, `externals` | Episode calls, `MIN_INTERVAL_SECONDS` (Atlas has a bucket) |
| `TmdbClient.php:33,105-125,254-266,346-349,374-386,413-416` | `/search/movie`, `/search/tv`, `/{movie,tv}/{id}/external_ids`, `api_key` query parameter | Images, credits, alternative titles, `/find` |
| `TraktService.php:33,126-138,560-575` | `search/{show,movie}?query=`, `trakt-api-version`, `trakt-api-key` | OAuth, episodes, calendars |
| `app/Services/TvProcessing/TvProcessingCandidateQuery.php`, `app/Services/TvProcessing/Pipes/` | Lesson only: chained provider pipes per release are what Atlas must not do at 118M | Everything |
| `app/Services/Api/V1/ReleaseItemPayloadBuilder.php:369-490` | Attribute names: `season`, `episode`, `tvairdate`, `tvdbid`, `traktid`, `tvmazeid`, `tmdbid`, `tvrageid`, `rageid`, `imdbid`, `imdb` | Others |
| `app/Http/Controllers/Api/ApiController.php:281,330-336,426-437,1125-1140` | Params; `tt` stripped; `season` YYYY plus `ep` MM/DD is a date | Web request plumbing |
| `docs/newznab_api_specification.txt:128-142,144-155,308-316` | `tvsearch`, `movie` and the attribute lists | |

### How Sonarr and Radarr use these

Spec `:128-155` lists the parameters. The clients' source adds what the spec doesn't say:

- **Caps first, cached for 7 days** (`NewznabCapabilitiesProvider.cs`: `TimeSpan.FromDays(7)`). `supportedParams` decides what they send. A provider switched on is seen after that, or after pressing Test in the client, and the README says so.
- **All IDs in one request.** `SupportsAggregateIdSearch` is true once `tv-search` has `supportedParams`. Sonarr sends `t=tvsearch&cat=5030,5040&extended=1&offset=0&limit=100&tvdbid=N&imdbid=ttN&rid=N&tvmazeid=N&tmdbid=N&season=1&ep=2`, including only the parameters in caps. So several IDs in one request mean any of them.
- **Sonarr keeps the `tt`** on `imdbid`; **Radarr strips it** (`ImdbId.Substring(2)`). Both forms must work.
- **Sonarr falls back by tier.** An empty answer on the ID tier moves to `q` when `q` is in caps. An error does not, so an unanswerable ID must return empty.
- **Daily shows** send `season=2025&ep=11/10` (spec `:140`).
- **Pages:** `limit=100`, `offset` up to 2,900 (`MaxPages = 30`, `PageSize = 100`).
- **Result attributes** (`NewznabRssParser.cs`): `tvdbid`, `rageid` and `imdb` are read by `int.TryParse`, and `imdb` becomes `tt{imdb:D7}`, so it must be digits only. Radarr's RSS is `t=movie&cat=...` with no `q`, which Part 7 serves.

## 4. Atlas changes

**New `src/media/`**: `mod.rs` (`Media`), `parse.rs`, `title.rs`, `score.rs`, `store.rs`, `http.rs`, `tvmaze.rs`, `tmdb.rs`, `trakt.rs`, `resolve.rs`, `steps.rs`, `fanout.rs`, `query.rs`. `parse.rs`, `score.rs` and `resolve.rs` carry a GPLv3 attribution header naming the upstream files. `src/lib.rs` gets `pub mod media;`.

**New `scripts/extract_tv_aliases.py`, `data/tv_title_aliases.tsv`**: Task 2.

**`src/postprocess/schema.rs`**: `SHARD_TABLES` gains `&crate::media::MEDIA_RELEASE`. **`src/postprocess/steps.rs`**: `all()` gains `crate::media::steps()`'s three entries.

**`src/config.rs`**: `Config` (`:134`) gains `pub media: MediaConfig`, in `Default` (`:157`) and `from_value` (`:282`). New `pub fn media_config() -> MediaConfig` and `pub fn body_budget_set() -> bool`, which read the file through `read_file` (`:390`) and never touch the keyring.

**`src/db.rs`**: `wipe` (`:187-189`), before pushing `main`: `dbs.push(crate::media::store::path_for(main));`. The loop at `:190` removes it with `-wal`, `-shm` and `-journal`. A failure stops the wipe with the main database in place, as for a shard.

**`src/search.rs`**: new `pub(crate) fn visible_release(conn, id) -> Result<Option<ReleaseRow>>`, the probe Part 7 added, for one id.

**`src/api.rs`**:
- `CAPS`: `tv-search` params become `q,season,ep,cat,limit,offset<!--tv-ids-->` and `movie-search` `q,cat,limit,offset<!--movie-ids-->`. New `fn caps_body(base: &str, on: &IdParams) -> String` replaces both, for example with `,tvdbid,imdbid,rid,tvmazeid`, and the `t=caps` branch (`:135-138`) calls it with `query::id_params(&config::media_config(), config::body_budget_set())`.
- `handle` (`:149`), before `search_term`:

```rust
let on = media::query::cached_id_params();
if let Some(ids) = media::query::from_args(kind, &args, &on) {
    return id_search(kind, &ids, &args, base, api_key, &on);
}
```

- New `id_search`: reads `limit`, `offset` and `cat` as `search` does, and calls `media::query::find`. With rows it answers. With none and a non-empty `q` it goes to `search`, which keeps "an ID next to `q`" useful. With none and no `q` it answers `rss` with no items.
- `search_term` (`:164`): `ep` of the form `MM/DD` with a 4 digit `season` adds `YYYY MM DD` to the term, so a text search finds `Show.2025.11.10`. Today it adds `S2025`, which matches nothing.
- `:151` comment and `ID_PARAMS` (`:160`): the ID-only rule now applies to parameters that are off.
- `rss`: gains `media: &HashMap<i64, MediaAttrs>` and `on: &IdParams`. After the category attribute it writes `attrs.xml(on)`. Part 4's `rss_reports_a_known_password_only` and Part 7's `rss_reports_the_category_and_defaults_to_8000` gain both arguments: an empty map and `IdParams::default()`.

**`src/bg_indexer.rs`**:
- `run_servers`, after the stage starts (Part 3, after `:751`), when `config.media.any_provider()`: `tasks.spawn(media::run(media::shared(), db.clone(), { let s = sched.clone(); move || s.stopping() }))`. It holds a `Db` clone, so it ends and drops the clone before `tasks` drains (`:760-765`). Part 3's stage task works the same way.
- `Scheduler` gains `media_on: bool`. `watch_config` (`:622-629`) adds `|| config.media.any_provider() != sched.media_on` to the rebuild test, so a provider turning on starts the fan-out thread.
- The unit-test `scheduler()` (`:1053`) sets `media_on: false`.

**`config.example.json`**: the block with every provider off.

**`tests/common/mod.rs`**: `pub mod http;`, a new `tests/common/http.rs` (Task 5). **`tests/fixtures/media/`**: hand-written JSON.

**Earlier parts' stage tests.** `steps::all()` now includes `media_parse`, which runs for every release that passes the rules. `tests/archive_stage.rs` (Part 4), `tests/naming_stage.rs` (Part 5), `tests/predb_stage.rs` (Part 6) and `tests/category_stage.rs` (Part 7) use `steps::all()` or an explicit list; those using `steps::all()` set `media.parse: false` through `set_config`, as Part 5 does for naming. Part 3's `tests/pp_stage.rs`, `pp_bodies.rs` and `pp_auto_compact.rs` pass an explicit list of their recording steps and need no change; a harness there that uses `steps::all()` gets the same line.

### Title parsing (`parse.rs`)

`regex::Regex` with Unicode on, because this extracts text (`Détective`). `[0-9]` rather than `\d`, so `parse` never meets a non-ASCII digit. The first rule that matches decides. `prefix` is capture 1 in every rule.

```text
T1  (?i)^(.*?)[^a-z0-9]s([0-9]{1,2})[^a-z0-9]?e([0-9]{1,3})(?:-?e|-)([0-9]{1,3})(?:[^a-z0-9]|$)   season 2, episode 3, last 4
T2  (?i)^(.*?)[^a-z0-9]s([0-9]{2})[^a-z0-9]?e([0-9]{2})e?([0-9]{2})(?:[^a-z0-9]|$)                season 2, episode 3, last 4
T3  (?i)^(.*?)[^a-z0-9]s([0-9]{1,4})[^a-z0-9]?e([0-9]{1,3})[abr]?(?:v[0-9])?(?:[^a-z0-9]|$)       season 2, episode 3
T4  (?i)^(.*?)[^a-z0-9]s([0-9]{1,2})[^a-z0-9]?d[0-9](?:[^a-z0-9]|$)                               season 2, pack
T5  (?i)^(.*?)[^a-z0-9]s([0-9]{1,2})(?:[^a-z0-9]|$)                                               season 2, pack
T6  (?i)^(.*?)[^a-z0-9]([0-9]{1,2})x([0-9]{1,3})(?:[^a-z0-9]|$)                                   season 2, episode 3
T7  ^(.*?)[^a-zA-Z0-9]((?:19|20)[0-9]{2})[./-]([0-9]{2})[./-]([0-9]{2})(?:[^a-zA-Z0-9]|$)         date y 2, m 3, d 4
T8  ^(.*?)[^a-zA-Z0-9]([0-9]{2})([./-])([0-9]{2})([./-])((?:19|20)[0-9]{2})(?:[^a-zA-Z0-9]|$)     date, when 3 == 5; a value over 12 decides the order, else `.` is d.m and `/` `-` are m.d
T9  (?i)^(.*?)[^a-z0-9](20[0-9]{2})[^a-z0-9]E([0-9]{1,3})(?:[^0-9]|$)                             season 2, episode 3
T10 (?i)^(.*?)[^a-z0-9]((?:19|20)[0-9]{2})[^a-z0-9]Part([0-9]{1,2})(?:[^a-z0-9]|$)                season 2, episode 3
T11 (?i)^(.*?)[^a-z0-9](?:Part|Pt)[^a-z0-9]([0-9]{1,2})(?:[^a-z0-9]|$)                            season 1, episode 2
T12 (?i)^(.*?)[^a-z0-9]EP?[^a-z0-9]?([0-9]{1,3})(?:[^a-z0-9]|$)                                   season 1, episode 2
T13 ^(.*?)[_ ][-_][_ ]([0-9]{1,3})[_ ]*[\[(]                                                      season 1, episode 2
T14 (?i)^(.*?)[^a-z0-9]Seasons?[^a-z0-9]?([0-9]{1,2})(?:[^a-z0-9]|$)                              season 2, pack
```

T1 and T2 set `episode_last` only when it exceeds `episode`. A date sets `season = y`, `episode = m * 100 + d`, `air_date = y * 10000 + m * 100 + d`, and an impossible date (T7) falls through to the next rules, or fails the parse (T8). No rule matching means `None`.

`clean_title(prefix)`, in order:

```text
1. remove ^\[[^\]]+\][_\s]*
2. remove (?i)^(?:[^a-z0-9]{2,}|(?:sample|proof|repost)-)
3. cut at the first match of
   (?i)[^a-z0-9](?:(?:480|576|720|1080|2160)[ip]|AAC2?|BD-?Rip|Blu-?Ray|DD5|DiVX|DLMux|DTS|DVD(?:-?Rip)?|[HX][-_. ]?26[45]|HEVC|[HPS]DTV|PROPER|REPACK|WEB[-_. ]?(?:DL|Rip)|XViD)(?:[^a-z0-9]|$)
4. year: ^(.+?)[^a-zA-Z0-9]\(?((?:19|20)[0-9]{2})\)?$   gives title 1, year 2
5. \b((?:[A-Za-z]\.){2,}[A-Za-z]?\.?) : drop the dots inside (S.W.A.T. to SWAT)
6. \([^)]*\)|[._] : a space each
7. the language tags of AbstractTvProvider.php:1449, except the `MULTi` duplicate: removed
8. \s+-\s*$ : removed. A trailing number stays: it's part of the title in `The 100` and `Polizeiruf 110`
9. collapse whitespace; trim " and -; under 2 alphanumerics means None
10. alias: lowercase of the result found in data/tv_title_aliases.tsv gives its canonical title
```

A movie, after replacing `.` and `_` with spaces and collapsing whitespace in the whole name:

```text
FOLLOW = [^\w](?:(?:1080|480|720|2160)p|AC3D|Directors(?:[^\w]CUT)?|DD5\.1|(?:DVD|BD|BR|UHD)(?:Rip)?|BluRay|divx|HDTV|iNTERNAL|LiMiTED|(?:Real\.)?PROPER|RE(?:pack|Rip)|Sub\.?(?:fix|pack)|Unrated|WEB-?DL|WEBRip|(?:x|H|HEVC)[ ._-]?26[45]|xvid|AAC|REMUX)[^\w]
M1  (?i)([\w. '&!,:-]+)[^\w]((?:19|20)[0-9]{2})(?:[^\w]|$)     title 1, year 2. Greedy, so the last year wins
M2  (?i)^(?:[^\w]{2,})?([\w .'&!,:-]+?)FOLLOW                  on the name plus one space. Title 1
M3  the name when under 101 bytes and the original doesn't match (?i)\.(rar|zip|avi|mkv|mp4)$, with -[A-Za-z0-9]+$ removed
then: FOLLOW, \([^)]*\) and \[[^\]]*\] become spaces; collapse whitespace.
None when empty, or with no year when 2 bytes or fewer or all digits. A year makes `1984` and `12` titles.
```

Quality, as `resolution | source << 4`:

```text
resolution: 4 (?i)(?:^|[^a-z0-9])(2160p|4k|uhd)(?:[^a-z0-9]|$), 3 ...1080[pi]..., 2 ...720p..., 1 ...(480p|576p)..., else 0
source:     1 ...(blu-?ray|bdrip|brrip|remux)..., 2 ...web(?:[-._ ]?(?:dl|rip))?..., 3 ...[hp]dtv..., 4 ...dvd(?:rip|r|5|9)?..., else 0
```

### Title keys (`title.rs`)

`normalize(title)`: transliterate as `AbstractTvProvider.php:1435-1441`, `ß` to `ss`; `&` to ` and `; `$` to `s`; drop `'` and `’`; lowercase alphanumerics; everything else a space; collapse. `Détective & Privé: The $tory's End!` gives `detective and prive the storys end`.

```rust
pub enum Kind { Tv = 1, Movie = 2 }
/// FNV-1a 64 of "tv|title|year" or "movie|title|year" (year empty when unknown), shifted right once.
pub fn title_key(kind: Kind, normalized: &str, year: Option<i32>) -> i64;
/// (?i)^(.+?)\s+(US|UK|AU|NZ|CA|NL)$ : `Some Show US` gives (`Some Show`, Some("US")). AbstractTvProvider.php:1395-1415.
pub fn split_country(title: &str) -> (&str, Option<&str>);
```

The country stays in the key, so `some show us` and `some show uk` are separate titles. It leaves the query and scoring. Pinned values, which must not change:

| Input | Key |
|---|---:|
| `tv|some show|` | 2517964955805247441 |
| `tv|some show|2019` | 3994912364065961063 |
| `movie|some movie|2019` | 7250957560060540805 |
| `movie|some movie|` | 3314878267631545165 |

### Scoring (`score.rs`)

```rust
/// PHP similar_text: the longest common run, first found, plus the same on both sides of it.
pub fn similar(a: &[u8], b: &[u8]) -> usize;
/// similar * 200 / (a.len() + b.len()). Inputs cut to 200 bytes.
pub fn percent(a: &str, b: &str) -> f64;
/// `countries` empty means unknown. TMDB's `origin_country` can hold several.
pub struct Candidate<'a> { pub title: &'a str, pub year: Option<i32>, pub countries: &'a [&'a str] }
/// `UK` is `GB`, otherwise uppercase. AbstractTvProvider.php:1701-1706.
pub fn country_code(c: &str) -> String;
/// 0.0 is no match. Compared lowercased. Exact 100, else percent if >= 75.
/// A country on our side against known countries, none equal under `country_code`: 0. Equal adds 2.
/// Both years known: within 1 adds 10, otherwise 0, even for an exact title.
/// Movie only: with no year on our side, only an exact title counts.
pub fn score(kind: Kind, ours: &str, our_year: Option<i32>, our_country: Option<&str>, c: &Candidate<'_>) -> f64;
```

Each provider takes the candidate with the highest score above 0, first one on a tie, over at most 10 results and all names of each.

### Providers

All use `http.rs`:

```rust
fn agent(base: &str) -> ureq::Agent {
    let mut b = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(15)))
        .http_status_as_error(false)
        .max_redirects(0) // a reply's Location is untrusted: it could aim at a local service, or carry TMDB's key elsewhere
        .user_agent(concat!("atlas/", env!("CARGO_PKG_VERSION")));
    // a test's loopback server must not go through a proxy from the environment
    if base.starts_with("http://127.0.0.1") { b = b.proxy(None); }
    b.build().into()
}
```

A response body is read with `.body_mut().with_config().limit(2 << 20).read_json::<Value>()`. A provider's reply is untrusted: a title or alias over 255 bytes is no candidate, so nothing longer reaches `found_title`, and an array past 1,000 entries is cut there. `atlas.media.db` grows by one bounded row per title, so it needs no quota. A URL is never logged, because TMDB's key is in it. Log the provider and the status.

| Status | Meaning | Effect |
|---|---|---|
| 200 with JSON | Answered | Reset the pause |
| 404 | No such thing | A clean miss |
| 429 | Over the limit | Pause for `Retry-After` seconds, 60 when absent, at most 3,600 |
| 401, 403 | Bad key | Pause 3,600 s, log once per pause. No negative result is recorded |
| 5xx, timeout, connection error | Can't be asked | Pause 60 s, doubling to 3,600 s, reset by a success |
| Other, or a body that doesn't parse | Bad answer | `Trouble::Bad`, which uses a Part 3 attempt |

| Provider | Calls | Fields |
|---|---|---|
| TVMaze | `GET {base}/singlesearch/shows?q=T&embed=akas`; on a 404 or a score of 0, as when the one show it returns is another country's, `GET {base}/search/shows?q=T` | `id`, `name`, `premiered`, `network.country.code` or `webChannel.country.code`, `_embedded.akas[].name`, `externals.{tvrage,thetvdb,imdb}`; search: `[].show` |
| TMDB TV | `GET {base}/search/tv?query=T&page=1&include_adult=false[&first_air_date_year=Y]&api_key=K`, then for a match `GET {base}/tv/{id}/external_ids?api_key=K` | `results[].{id,name,original_name,first_air_date,origin_country}`; `{imdb_id,tvdb_id,tvrage_id}` |
| TMDB movie | `GET {base}/search/movie?query=T&page=1&include_adult=false[&year=Y]&api_key=K`, then `GET {base}/movie/{id}/external_ids?api_key=K` | `results[].{id,title,original_title,release_date}`; `{imdb_id}` |
| Trakt | `GET {base}/search/show?query=T` or `/search/movie`, headers `trakt-api-version: 2`, `trakt-api-key: ID` | `[].{show,movie}.{title,year,ids.{trakt,tvdb,imdb,tmdb,tvrage}}` |

`imdb` parses with `^(?:tt)?([0-9]{6,10})$`; anything else, or 0, is `None`. Default bases: `https://api.tvmaze.com`, `https://api.themoviedb.org/3`, `https://api.trakt.tv`.

Order: TV asks TVMaze, TMDB, Trakt. A movie asks TMDB, Trakt. Each enabled provider for the kind is asked once per title, because each fills different IDs.

### `resolve`

```rust
pub enum Resolved { Found(Ids), NotFound, Later(i64) }
impl Media { pub fn resolve(&self, q: &TitleQuery<'_>, key: i64, now: i64) -> Result<Resolved, String>; }
```

1. Read the title's row. `ask` = the enabled providers for the kind whose bit isn't in `checked_mask`. For `state = 2` with `now >= next_check`: all of them, starting from no IDs.
2. `ask` empty: `Found(ids)` for `state = 1`, otherwise `NotFound`.
3. For each in order: `find(q)`.
   - `Ok(Found)`: merge if consistent (below), set its bit.
   - `Ok(NotFound)`: set its bit.
   - `Paused(t)`, `BadKey`: `later = min(later, t)`. Its bit stays clear.
   - `Bad(e)`: keep `e`. Its bit stays clear.
4. Write the row in one statement:
   - Any ID known: `state = 1`, `next_check` NULL, `misses = 0`. `fanout_done = 0` when the IDs changed.
   - No ID and no trouble: `state = 2`, `misses + 1`, `next_check = now + min(negative_ttl_days * 2^(misses), 180) days`.
   - No ID with trouble: `state = 0`, keeping the bits set. The next run asks only the rest.
5. Update the in-memory `known` entry and clear the claim. Return `Later(later)` if any was paused, even with IDs from another, because no later release of the title may come along to retry it; those IDs reach the rows by the fan-out. Otherwise `Err(e)` if any was bad; `Found(ids)` for `state = 1`; else `NotFound`.

**Consistent:** a later provider's `tvdb`, `imdb`, `tmdb` or `tvrage`, where both are known, must equal what's there. Otherwise its whole answer is dropped and its bit is set: the first provider's identity wins. Trakt adds only when it agrees.

A positive result never expires. An ID doesn't change meaning.

### The in-memory title map

`Step::applies` may not do I/O, and sending every TV and movie row through the net lane would spend it on rows whose answer is cached, see section 9. So `Media` holds, once a provider is on:

```rust
struct Known { next_check: u32, checked: u8, found: bool, imdb: bool }   // from media_titles at first use
known: RwLock<HashMap<i64, Known>>
claims: Mutex<HashMap<i64, (i64 /*release id*/, u32 /*at*/)>>
```

`wants(kind, key, release_id, now) -> bool`, with `enabled` the bits of the providers on that ask `kind`:

1. A `known` entry with no provider left to ask (`enabled & !checked == 0`), and found or `now < next_check`: false.
2. A claim on `key` by this release: true.
3. A claim by another release under 7 days old: false.
4. A title with no `known` entry, with `media_titles` at `max_titles` rows (a count read when the file opens, kept up to date by `resolve`): false, counted in `media_titles_full`. So the map and the file stop growing, and titles already known keep their answers.
5. `claims` at `MAX_CLAIMS = 4_000_000`: drop those over 7 days old, and if it is still full, false, counted in `media_claims_full`. About 150 MB at most, a cap above section 9's 2M titles, so only hostile names reach it. A release refused here gets no lookup until another visit asks again.
6. Otherwise record the claim: true.

So each title has one release in the net lane at a time. The others keep NULL IDs until the fan-out fills them. A restart empties `claims`. A title's old representative still has `net_wanted = 1`, and runs. A second one claimed meanwhile only finds the cache filled.

### Fan-out (`fanout.rs`)

When `resolve` sets `fanout_done = 0` it pushes the key. `media::run` loops on a blocking thread until `stopping`. At start it loads every `fanout_done = 0` row, so a crash loses nothing. For each key it runs pass 1 on all 8 shards through `Db::on_writer`, one job per 1,000 rows, waiting for each answer. 60 s later it runs pass 2 the same way and sets `fanout_done = 1`. Pass 2 is a second look only: `media_parse` reads the cache at apply time, so nothing depends on it or on its delay. A pass that finds its rows unchanged writes nothing.

```sql
select max(release_id), count(*) from (select release_id from media_release
 where title_key = ?1 and release_id > ?2 order by release_id limit 1000);

update media_release indexed by media_release_title
   set tvdb_id = ?4, tvmaze_id = ?5, trakt_id = ?6, tmdb_id = ?7, imdb_id = iif(imdb_from_nfo = 1, imdb_id, ?8), tvrage_id = ?9
 where title_key = ?1 and release_id > ?2 and release_id <= ?3
   and (tvdb_id is not ?4 or tvmaze_id is not ?5 or trakt_id is not ?6 or tmdb_id is not ?7
        or (imdb_from_nfo = 0 and imdb_id is not ?8) or tvrage_id is not ?9);
```

`indexed by` is required: with a rowid range the planner could otherwise walk every release between two rows of the title. An `imdb_id` from that release's own NFO stays, since an NFO can't name the wrong film (decision 28). A provider's is replaced like the other IDs, so a corrected title corrects it too.

## 5. Schema and migration

### In each shard, registered through `schema::SHARD_TABLES`

```sql
create table if not exists media_release (
    release_id   INTEGER PRIMARY KEY,
    kind         INTEGER NOT NULL,   -- 1 tv, 2 movie
    title_key    INTEGER NOT NULL,   -- title::title_key, also media_titles.title_key
    season       INTEGER,            -- year for a daily show; NULL for a movie
    episode      INTEGER,            -- first episode; MMDD for a daily show; NULL for a pack or a movie
    episode_last INTEGER,            -- last of a multi-episode release, else NULL
    air_date     INTEGER,            -- YYYYMMDD, daily shows only
    year         INTEGER,
    quality      INTEGER NOT NULL,   -- resolution | source << 4
    tvdb_id      INTEGER,
    tvmaze_id    INTEGER,
    trakt_id     INTEGER,
    tmdb_id      INTEGER,
    imdb_id      INTEGER,
    tvrage_id    INTEGER,
    imdb_from_nfo INTEGER NOT NULL DEFAULT 0   -- 1: `imdb_id` is this release's own NFO, which a provider never overwrites
);
create index if not exists media_release_title  on media_release(title_key, release_id);
create index if not exists media_release_tvdb   on media_release(tvdb_id,   release_id, season, episode, episode_last, kind) where tvdb_id   is not null;
create index if not exists media_release_tvmaze on media_release(tvmaze_id, release_id, season, episode, episode_last, kind) where tvmaze_id is not null;
create index if not exists media_release_trakt  on media_release(trakt_id,  release_id, season, episode, episode_last, kind) where trakt_id  is not null;
create index if not exists media_release_tmdb   on media_release(tmdb_id,   release_id, season, episode, episode_last, kind) where tmdb_id   is not null;
create index if not exists media_release_imdb   on media_release(imdb_id,   release_id, season, episode, episode_last, kind) where imdb_id   is not null;
create index if not exists media_release_rage   on media_release(tvrage_id, release_id, season, episode, episode_last, kind) where tvrage_id is not null;
```

The `ShardTable` const has `release_key: Some("release_id")` and these seven in `indexes`. `sample_row` fills `kind`, `title_key` and `quality` with 1.

`media_parse` writes with one statement. The same title keeps IDs already on the row, so a late article can't lose them when a provider is off or an NFO supplied them. A new title starts clean:

```sql
insert into media_release (release_id, kind, title_key, season, episode, episode_last, air_date, year, quality,
                           tvdb_id, tvmaze_id, trakt_id, tmdb_id, imdb_id, tvrage_id)
values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
on conflict(release_id) do update set
    tvdb_id   = iif(media_release.title_key = excluded.title_key, coalesce(excluded.tvdb_id,   media_release.tvdb_id),   excluded.tvdb_id),
    tvmaze_id = iif(media_release.title_key = excluded.title_key, coalesce(excluded.tvmaze_id, media_release.tvmaze_id), excluded.tvmaze_id),
    trakt_id  = iif(media_release.title_key = excluded.title_key, coalesce(excluded.trakt_id,  media_release.trakt_id),  excluded.trakt_id),
    tmdb_id   = iif(media_release.title_key = excluded.title_key, coalesce(excluded.tmdb_id,   media_release.tmdb_id),   excluded.tmdb_id),
    tvrage_id = iif(media_release.title_key = excluded.title_key, coalesce(excluded.tvrage_id, media_release.tvrage_id), excluded.tvrage_id),
    imdb_id   = iif(media_release.title_key = excluded.title_key, iif(media_release.imdb_from_nfo = 1, media_release.imdb_id, coalesce(excluded.imdb_id, media_release.imdb_id)), excluded.imdb_id),
    imdb_from_nfo = iif(media_release.title_key = excluded.title_key, media_release.imdb_from_nfo, 0),
    kind = excluded.kind, title_key = excluded.title_key, season = excluded.season, episode = excluded.episode,
    episode_last = excluded.episode_last, air_date = excluded.air_date, year = excluded.year, quality = excluded.quality
```

### `atlas.media.db`, next to the shards

`store::path_for(main)` is `main.with_file_name("{stem}.media.db")`. It's opened only when a provider is on, through `db::create_at` with `pragma journal_mode = wal`, and created on first use. `atlas.*.db` and `atlas.*.db-*` in `.gitignore` already cover it.

```sql
create table if not exists media_titles (
    title_key    INTEGER PRIMARY KEY,
    kind         INTEGER NOT NULL,
    title        TEXT NOT NULL,       -- the query title
    year         INTEGER,
    state        INTEGER NOT NULL,    -- 0 still asking, 1 found, 2 not found
    checked_mask INTEGER NOT NULL,    -- 1 tvmaze, 2 tmdb, 4 trakt: answered for this title
    tvdb_id INTEGER, tvmaze_id INTEGER, trakt_id INTEGER, tmdb_id INTEGER, imdb_id INTEGER, tvrage_id INTEGER,
    found_title  TEXT,                -- the provider's name, for a person reading the file
    checked_at   INTEGER NOT NULL,
    next_check   INTEGER,             -- state 2 only
    misses       INTEGER NOT NULL DEFAULT 0,
    fanout_done  INTEGER NOT NULL DEFAULT 1
);
create index if not exists media_titles_fanout on media_titles(title_key) where fanout_done = 0;
```

**It's a cache.** The API reads only the shards. Deleted, Atlas loses no IDs already written: the next release of each title asks again and rewrites the same values. `atlas --reprocess --step media_lookup` asks again for titles with no `media_titles` row and a provider left to ask, and costs no request for the rest.

### Survival

| Path | What happens | Test |
|---|---|---|
| Open | `schema::migrate` creates the table and indexes empty. `media.db` isn't touched without a provider. | `tests/pp_schema.rs` iterates the registry; `no_media_file_without_a_provider` |
| `--compact` | Rows copy by name, and `build_shard` rebuilds all seven indexes. `media.db` isn't a shard and compaction leaves it alone. | Part 3's `every_registered_table_and_column_survives_compaction`; `an_id_search_works_after_a_compaction`; `a_compaction_leaves_the_media_cache_alone` |
| `--convert` | Fresh shards have the empty table. The cache is keyed by title, so it stays valid. | Part 3's convert test; `a_convert_leaves_the_media_cache_alone` |
| Wipe | The shards go, and the `db::wipe` change removes `media.db`. | `a_wipe_removes_the_media_cache` |
| Delete, purge | Part 3's generated trigger clears the row. A cached title stays. | Part 3's `deleting_a_release_clears_it_from_every_registered_table` |

## 6. Interfaces

### Provided

```rust
// src/media/parse.rs
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Parsed {
    pub title: String, pub year: Option<i32>, pub season: Option<i32>, pub episode: Option<i32>,
    pub episode_last: Option<i32>, pub air_date: Option<i32>, pub pack: bool, pub quality: i32,
}
pub fn parse_tv(name: &str) -> Option<Parsed>;
pub fn parse_movie(name: &str) -> Option<Parsed>;

// src/media/resolve.rs
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Ids { pub tvdb: Option<i64>, pub tvmaze: Option<i64>, pub trakt: Option<i64>, pub tmdb: Option<i64>, pub imdb: Option<i64>, pub tvrage: Option<i64> }
pub struct TitleQuery<'a> { pub kind: Kind, pub title: &'a str, pub year: Option<i32>, pub country: Option<&'a str> }
pub enum Answer { Found { ids: Ids, title: String }, NotFound }
pub enum Trouble { Paused(i64), BadKey(i64), Bad(String) }
pub trait Provider: Send + Sync {
    fn bit(&self) -> u8;
    fn asks(&self, kind: Kind) -> bool;
    /// Waits at most 2 s for a token, then Paused(now + 1). A lookup of two requests keeps what its first
    /// found when the second can't go yet, per query, so the next call for it goes on from there and a low
    /// rate still finishes (at most 1,024 queries, the oldest forgotten; a rebuild forgets them all).
    fn find(&self, q: &TitleQuery<'_>) -> Result<Answer, Trouble>;
}

// src/media/mod.rs
pub fn shared() -> Arc<Media>;                         // for paths::database(), one per process
pub fn steps() -> [Arc<dyn Step>; 3];
pub async fn run(media: Arc<Media>, db: Db, stopping: impl Fn() -> bool + Send + 'static);
impl Media {
    pub fn for_main(main: &Path) -> Arc<Media>;        // tests
    pub fn set_config(&self, cfg: &Config);            // rebuilds providers when `media` changed
    pub fn resolve(&self, q: &TitleQuery<'_>, key: i64, now: i64) -> Result<Resolved, String>;
    pub fn cached_ids(&self, key: i64) -> Option<Ids>; // None with no provider on
    pub fn category_options(&self) -> category::Options;
    pub fn wants(&self, kind: Kind, key: i64, release_id: i64, now: i64) -> bool;
    pub fn imdb_known(&self, key: i64) -> bool;        // in-memory, false with no provider on
}

// src/media/query.rs
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IdParams { pub tv: IdSet, pub movie: IdSet }
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IdSet { pub tvdb: bool, pub tvmaze: bool, pub trakt: bool, pub tmdb: bool, pub imdb: bool, pub rid: bool }
pub fn id_params(cfg: &MediaConfig, body_budget_set: bool) -> IdParams;
pub fn cached_id_params() -> IdParams;                 // re-read at most every 30 s; t=caps refreshes it
pub struct IdQuery { pub kind: Kind, pub ids: Vec<(IdColumn, i64)>, pub season: Option<i64>, pub episode: Option<i64> }
pub fn from_args(t: &str, args: &HashMap<String, String>, on: &IdParams) -> Option<IdQuery>;
pub fn find(q: &IdQuery, cats: Option<&CatFilter>, offset: i64, limit: i64) -> db::Result<Vec<ReleaseRow>>;
pub const ID_ROWS: i64 = 20_000;
pub const MAX_TITLE: usize = 255;
pub const ID_SCAN: i64 = 200_000;
pub struct IdPage { pub rows: Vec<ReleaseRow>, pub scanned: usize }
pub fn find_with(conn: &Connection, q: &IdQuery, cats: Option<&CatFilter>, offset: i64, limit: i64, bounds: &Bounds, id_rows: i64) -> db::Result<IdPage>;
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MediaAttrs { pub kind: i64, pub season: Option<i64>, pub episode: Option<i64>, pub air_date: Option<i64>, pub year: Option<i64>, pub ids: Ids }
pub fn attrs_of(ids: &[i64]) -> db::Result<HashMap<i64, MediaAttrs>>;
/// `on.tv` or `on.movie`, by `kind`. Season, episode and year always; an ID only when its parameter is on.
impl MediaAttrs { pub fn xml(&self, on: &IdParams) -> String; }
```

`id_params`, the IDs each provider can fill:

| Parameter | `tv-search` on with | `movie-search` on with |
|---|---|---|
| `tvdbid`, `rid` | any provider | never |
| `imdbid` | any provider | TMDB or Trakt, or `media_nfo` active |
| `tvmazeid` | TVMaze | never |
| `tmdbid` | TMDB or Trakt | TMDB or Trakt |
| `traktid` | Trakt | Trakt |

`from_args`: only `t=tvsearch` and `t=movie`. Takes parameters that are on, parsed above 0; `imdbid` with `tt` removed; `tvrageid` is read as `rid`. `None` when none remain. `season` parses as an integer. `ep`, only with a `season`, is an integer, or `MM/DD` with a 4 digit season, giving `MM * 100 + DD`.

### The steps

```rust
pub struct MediaParse(pub Arc<Media>);   // StepId(32), "media_parse", rank 500, Needs::Database
pub struct MediaLookup(pub Arc<Media>);  // StepId(33), "media_lookup", rank 510, Needs::External
pub struct MediaNfo(pub Arc<Media>);     // StepId(34), "media_nfo", rank 520, Needs::Bodies

/// What Part 8 acts on. A group hint alone isn't evidence of a title (decision 29).
pub fn media_target(category: i64, rule: &str, name: &str) -> Option<(Kind, Parsed)> {
    if rule.starts_with("group_name_") { return None; }
    let parsed = if category::is_tv(category) { parse_tv(name).map(|p| (Kind::Tv, p)) }
    else if category::is_movie(category) { parse_movie(name).map(|p| (Kind::Movie, p)) }
    else { None };
    // a poster chooses the name: nothing longer than this is a title, and none reaches a provider or the cache
    parsed.filter(|(_, p)| p.title.len() <= MAX_TITLE)
}
```

**`media_parse`.** `enabled`: calls `set_config`, returns `cfg.media.parse`. `applies`: default true, so it can drop a stale row. `reruns`: on rename and growth. `run`:

1. `category` and `rule` from the facts `category` and `category_rule`. When those are absent, because only this step was cleared or `category.enabled` is false, `category::classify_release(release, options)`. So this step doesn't depend on the category step being on.
2. `media_target(category, rule, name)`, where `name` is `display_name` or `name`.
3. `None`: if `select 1 from media_release where release_id = ?1` finds a row, a `delete` write. `matched: false`.
4. `Some`: the statement in section 5, with its parameters in `late_params`, so `media.cached_ids(key)` is read on the shard's writer as the row is written. Fan-out passes are writer jobs on the same shard and start after the cache has the title, so a parse written before a pass is fixed by it, and one written after reads the IDs itself, however long it was held. `matched: true`. No network.

**`media_lookup`.** `enabled`: `set_config`, returns `cfg.media.any_provider()`. `config_stamp` (Part 3): the bits of the providers on, so adding one to a title's already looked-up releases starts one walk that clears this step, and `wants` then lets one release per title ask only the new provider. `reruns`: on rename and growth. `applies`, pure with no I/O:

```rust
fn applies(&self, r: &ReleaseInfo) -> bool {
    let d = category::classify_release(r, &self.0.category_options());
    let name = r.display_name.as_deref().unwrap_or(&r.name);
    media_target(d.category, d.rule, name)
        .is_some_and(|(kind, p)| self.0.wants(kind, key_of(kind, &p), r.id, now_secs()))
}
```

`run`: the same `classify_release` and `media_target`. `None` is done with no call, so a release a changed rule now places elsewhere costs no request. Then `resolve`:

| `resolve` says | Outcome |
|---|---|
| `Found(ids)` | `matched: true`, a write of the six IDs on this release's row |
| `NotFound` | `matched: false`, done |
| `Later(t)` | `wait_until: Some(t + release_id % 30)`, no attempt used, no lane pause |
| `Err(e)` | `StepError::Retry(e)` |

`ctx.stopping()` before each provider: `StepError::Stopped`. The `Found` write is the fan-out's `set` clause on this one row, so an NFO's `imdb_id` stays and a provider's is replaced.

**`media_nfo`.** The one movie ID source that needs no provider and can't mismatch. `enabled`: `cfg.media.nfo && cfg.media.parse && cfg.postprocess.body_fetches_per_minute > 0`, and Part 3 keeps a `Bodies` step off with no budget in any case. So it never runs on an unchanged config. `reruns`: on rename and growth, as an NFO can arrive late. `applies`, pure: `classify_release` and `media_target` give a movie, and `self.0.imdb_known(key)` is false, which reads the in-memory map and is false with no provider on. `run`:

1. `select imdb_id from media_release where release_id = ?1` on `ctx.conn()`. No row, or `imdb_id` set: done, `matched: false`, no body.
2. `archive::select::nfo_sources(&ctx.files()?)`, first only (Part 4; Part 5's `NfoName` uses it the same way). None: done.
3. `ctx.segments(file.id, &SegmentPick::First(1))`. A row with `part > 1` means the first article was never indexed: done.
4. `ctx.fetch_bodies(&[BodyRequest { message_id, file: file.filename.clone(), max_bytes: Some(65_536) }])`. One body. `Got::Missing`, `TooBig`, `Refused`: done, `matched: false`. A `StepError` propagates, and Part 3 gives the attempt back on an outage.
5. On the bytes, `regex::bytes` with Unicode off: `(?i)(?:imdb\.com/title/)?\btt([0-9]{7,8})\b`, first match. None: done.
6. `update media_release set imdb_id = ?2, imdb_from_nfo = 1 where release_id = ?1 and imdb_id is null`. `matched: true`.

### Consumed

`category::{classify_release, is_tv, is_movie, parse_filter, CatFilter}`, the `category` and `category_rule` facts, and `release_category` for the `cat` filter (Part 7). `archive::select::nfo_sources` (Part 4). `Bounds`, `MAX_FILTER_OFFSET` (Part 7, `src/search.rs`). `Step` and its types, `SHARD_TABLES`, `Db::on_writer`, `fetch_bodies`, `store::SegmentPick` (Part 3). Part 1's block list, through `rules::judge`, which runs before any of the three in both lanes.

### The ID queries

Per `(shard, id)`, the newest `ID_SCAN = 200_000` rows of that ID are walked in descending `release_id`, and at most `ID_ROWS = 20_000` matching ids are kept, then merged in Rust. Newest first happens before either cap, so a heavily indexed title never loses its newest releases to a cap, and what the bounds can drop is the oldest. `{col}` is one of the six columns, from a fixed list. Plan in every form: `SEARCH media_release USING COVERING INDEX media_release_{x} ({col}=?)` in a co-routine. No form has a temp b-tree, since the index holds `release_id` right after the ID.

```sql
-- season and episode. A multi-episode release matches each of its episodes.
select release_id from (select release_id, kind, season, episode, episode_last from s{i}.media_release
                         where {col} = ?1 order by release_id desc limit ?5)
 where kind = ?2 and season = ?3 and episode <= ?4 and coalesce(episode_last, episode) >= ?4 limit ?6

-- season: packs and episodes
select release_id from (select release_id, kind, season from s{i}.media_release
                         where {col} = ?1 order by release_id desc limit ?4)
 where kind = ?2 and season = ?3 limit ?5

-- the id alone
select release_id from (select release_id, kind from s{i}.media_release
                         where {col} = ?1 order by release_id desc limit ?3)
 where kind = ?2 limit ?4

-- a category filter, per candidate that reaches the top
select category from s{i}.release_category where release_id = ?1
```

`find_with`: gather all ids, `sort_unstable` descending, `dedup`. Walk them, rejecting a filter miss and then a hidden row through `visible_release`, and counting `scanned`, until `offset + limit` are seen or more than `bounds.rejects` have been rejected. `offset > MAX_FILTER_OFFSET` is empty. A row with no category row passes only when `filter.uncategorized`.

## 7. Config

A `media` object in `config.json`, parsed leniently. An absent object means every provider off. `save_config` keeps it (`src/config.rs:447-450`). `media_config()` reads it for the API without the keyring.

| Key | Default | Meaning |
|---|---|---|
| `parse` | `true` | Run `media_parse`. No network. Off: no `media_release` rows and no IDs. |
| `nfo` | `true` | Run `media_nfo`. It also needs `parse` and a non-zero `postprocess.body_fetches_per_minute`, so an existing config fetches nothing. |
| `negative_ttl_days` | `30` | How long a title with no match isn't asked again. Doubles per repeat, at most 180 days, and is only acted on when a release of that title is visited. |
| `tvmaze.enabled` | `false` | TVMaze needs no key, so this is its switch. |
| `max_titles` | `10000000` | No new title is looked up past this many cached titles, about 300 MB of memory and 1.3 GB of disk. About 5 times section 9's estimate. At least 1,000 |
| `tvmaze.requests_per_minute` | `90` | Clamped to 1..=120. Published: at least 20 per 10 s per IP. |
| `tmdb.api_key` | `""` | Empty is off. |
| `tmdb.requests_per_minute` | `240` | Clamped to 1..=3000. |
| `trakt.client_id` | `""` | Empty is off. |
| `trakt.requests_per_minute` | `120` | Clamped to 1..=200. Published: 1,000 GET per 5 minutes. |
| `tvmaze.base_url`, `tmdb.base_url`, `trakt.base_url` | The provider's | For tests. Not in `config.example.json`. |

```rust
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaConfig { pub parse: bool, pub nfo: bool, pub negative_ttl_days: u32, pub tvmaze: TvmazeConfig, pub tmdb: TmdbConfig, pub trakt: TraktConfig }
impl MediaConfig { pub fn any_provider(&self) -> bool; pub fn from_value(v: Option<&Value>) -> MediaConfig; }
```

`impl Default for MediaConfig` sets `parse` and `nfo` true, `negative_ttl_days` 30, and every provider off. Keys and client ids never appear in logs. `Config` derives `Debug` and already holds passwords, so no new exposure comes from that.

**Running without keys** is the default: no `media` object, or every provider off. Indexing and categorizing run as before. `media_parse` still stores titles, seasons and episodes, so results gain `season` and `episode` attributes, and switching a provider on later needs no second parse. `media_lookup` is off, and so is `media_nfo` until a body budget is set. `caps` advertises no ID parameter. `tvdbid=N` alone returns empty. `tvdbid=N&q=Name` searches by name. No socket is opened to any provider.

## 8. Tasks

Each task: write the test first and see it fail for the stated reason, implement, run, commit. Append whatever attribution the executing session requires to commit messages. All tests use `tempfile::tempdir()`. Every HTTP test talks to `tests/common/http.rs` on loopback, serving hand-written JSON from `tests/fixtures/media/`. No test reaches the network, and none copies a name from the live database.

### Task 1: The parser

**Files:** create `src/media/mod.rs`, `src/media/parse.rs`; modify `src/lib.rs`.

- [ ] **Tests first**, in `parse.rs`. Fails first: no module. `parse_tv` table, where `-` is `None`:

| Name | Title | Year | S | E | Last | Air date | Pack | Q |
|---|---|---:|---:|---:|---:|---:|---|---:|
| `Some.Show.S01E02.720p.HDTV.x264-GRP` | Some Show | - | 1 | 2 | - | - | no | 50 |
| `Some.Show.S01E02E03.720p.WEB.x264-GRP` | Some Show | - | 1 | 2 | 3 | - | no | 34 |
| `Some.Show.S01E02-E04.1080p.WEB.x264-GRP` | Some Show | - | 1 | 2 | 4 | - | no | 35 |
| `Some.Show.S01E02-03.720p.WEB-GRP` | Some Show | - | 1 | 2 | 3 | - | no | 34 |
| `Some.Show.S01E0203.720p.WEB-GRP` | Some Show | - | 1 | 2 | 3 | - | no | 34 |
| `Some.Show.S01E02-720p.WEB-GRP` | Some Show | - | 1 | 2 | - | - | no | 34 |
| `Some.Show.S01.1080p.BluRay.x264-GRP` | Some Show | - | 1 | - | - | - | yes | 19 |
| `Some.Show.S01.COMPLETE.720p.WEB-GRP` | Some Show | - | 1 | - | - | - | yes | 34 |
| `Some.Show.Season.3.1080p.WEB-GRP` | Some Show | - | 3 | - | - | - | yes | 35 |
| `Some.Show.S03D2.DVDR-GRP` | Some Show | - | 3 | - | - | - | yes | 64 |
| `Some.Show.1x05.HDTV.XviD-GRP` | Some Show | - | 1 | 5 | - | - | no | 48 |
| `Some.Show.2024.03.15.Guest.Name.720p.WEB-GRP` | Some Show | - | 2024 | 315 | - | 20240315 | no | 34 |
| `Some.Show.2024-03-15.720p.WEB-GRP` | Some Show | - | 2024 | 315 | - | 20240315 | no | 34 |
| `Some.Show.15.03.2024.720p.WEB-GRP` | Some Show | - | 2024 | 315 | - | 20240315 | no | 34 |
| `Some.Show.03-15-2024.720p.WEB-GRP` | Some Show | - | 2024 | 315 | - | 20240315 | no | 34 |
| `Some.Show.2019.S02E03.1080p.WEB-GRP` | Some Show | 2019 | 2 | 3 | - | - | no | 35 |
| `Some.Show.US.S02E03.1080p.WEB-GRP` | Some Show US | - | 2 | 3 | - | - | no | 35 |
| `Some.Show.S2019E03.720p.WEB-GRP` | Some Show | - | 2019 | 3 | - | - | no | 34 |
| `Some.Show.2023.E05.720p.WEB-GRP` | Some Show | - | 2023 | 5 | - | - | no | 34 |
| `Some.Show.Part.3.720p.HDTV-GRP` | Some Show | - | 1 | 3 | - | - | no | 50 |
| `Some.Show.2015.Part2.720p.HDTV-GRP` | Some Show | - | 2015 | 2 | - | - | no | 50 |
| `Some.Show.EP06.Title.DVDRip.XviD-GRP` | Some Show | - | 1 | 6 | - | - | no | 64 |
| `[SubsPlease] Some Show - 08 (1080p) [ABCD1234].mkv` | Some Show | - | 1 | 8 | - | - | no | 3 |
| `[Grp]_Some_Show_-_12_[720p].mkv` | Some Show | - | 1 | 12 | - | - | no | 2 |
| `Some Show - S02E01 - Episode Title (1080p)` | Some Show | - | 2 | 1 | - | - | no | 3 |
| `S.W.A.T.S01E02.720p.HDTV-GRP` | SWAT | - | 1 | 2 | - | - | no | 50 |
| `Détective.Privé.S01E02.GERMAN.720p.WEB-GRP` | Détective Privé | - | 1 | 2 | - | - | no | 34 |
| `Some.Show.GERMAN.DL.S01E02.720p.WEB-GRP` | Some Show | - | 1 | 2 | - | - | no | 34 |
| `Some.Show.PROPER.S01E02.720p.WEB-GRP` | Some Show | - | 1 | 2 | - | - | no | 34 |
| `Some.Show.S01E02v2.720p.WEB-GRP` | Some Show | - | 1 | 2 | - | - | no | 34 |
| `sample-Some.Show.S01E02.720p.WEB-GRP` | Some Show | - | 1 | 2 | - | - | no | 34 |

  `parse_tv` gives `None` for `Some.Show.2024.13.45.720p.WEB-GRP`, `Some Movie Without Markers` and `Fast.Sixth.1080p.WEB-GRP`.

  `parse_movie` table:

| Name | Title | Year | Q |
|---|---|---:|---:|
| `Some.Movie.2019.1080p.BluRay.x264-GRP` | Some Movie | 2019 | 19 |
| `Some.Sequel.2049.2017.1080p.BluRay.x264-GRP` | Some Sequel 2049 | 2017 | 19 |
| `2001.A.Space.Story.1968.1080p.BluRay.x264-GRP` | 2001 A Space Story | 1968 | 19 |
| `Anti-Hero.2002.1080p.BluRay.x264-GRP` | Anti-Hero | 2002 | 19 |
| `Some.Movie.(2019).720p.WEB-DL` | Some Movie | 2019 | 34 |
| `Some Movie 2019 1920x1080 x264` | Some Movie | 2019 | 0 |
| `Some.Movie.1080p.BluRay.x264-GRP` | Some Movie | - | 19 |
| `Some.Movie.DVDRip.XviD-GRP` | Some Movie | - | 64 |
| `Some.Movie-GRP` | Some Movie | - | 0 |
| `Some Film [2015] [BluRay] [1080p]` | Some Film | 2015 | 19 |
| `1984.2019.2160p.UHD.BluRay.x265-GRP` | 1984 | 2019 | 20 |
| `Some_Movie_2021_WEB-DL` | Some Movie | 2021 | 32 |
| `12.2007.1080p.BluRay-GRP` | 12 | 2007 | 19 |
| `Some.Movie.2019.German.DL.1080p.BluRay.x264-GRP` | Some Movie | 2019 | 19 |
| `Director's.Cut.Story.1999.PROPER.DVDRip.XviD-GRP` | Director's Cut Story | 1999 | 64 |

- [ ] **Implement** section 4's rules.
- [ ] **Run:** `cargo test --lib media::parse`. **Expected:** pass. A failing row means the port differs from the regexes above, since these rows are a prototype's output. Do not edit a row to pass.
- [ ] **Commit:** `Parse show, season, episode, air date, movie title, year and quality from names`

### Task 2: Title keys, aliases, scoring

**Files:** create `src/media/title.rs`, `src/media/score.rs`, `scripts/extract_tv_aliases.py`, `data/tv_title_aliases.tsv`; modify `src/media/parse.rs`.

```python
#!/usr/bin/env python3
"""Writes data/tv_title_aliases.tsv from a newznab-tmux checkout. Reads text only. GPLv3, see README credits."""
import pathlib, re, sys
lines = (pathlib.Path(sys.argv[1]) / "app/Services/TvProcessing/Providers/AbstractTvProvider.php").read_text(encoding="utf-8").splitlines()[1189:1244]
rows = re.findall(r"'([^']+)' => '([^']+)'", "\n".join(lines))
assert len(rows) > 30
pathlib.Path("data/tv_title_aliases.tsv").write_text("".join(f"{a}\t{b}\n" for a, b in rows), encoding="utf-8")
```

- [ ] **Tests first:**
  - In `title.rs`: `normalize_table`: `Détective & Privé: The $tory's End!` gives `detective and prive the storys end`; `S.W.A.T` gives `s w a t`; `Some   Show` gives `some show`; `ß` gives `ss`. `title_keys_are_pinned`: the four values in section 4. `split_country_table`: `Some Show US` gives (`Some Show`, `US`). `USA Today` and `Some Show` are unchanged. `country_code_table`: `UK` and `uk` give `GB`, `GB` gives `GB`, `us` gives `US`.
  - In `parse.rs`: `an_alias_gives_the_canonical_title`: `Daily.Show.S01E02.720p.WEB-GRP` has title `The Daily Show`. `the_aliases_loaded`: more than 30.
  - In `score.rs`: `similar_text_table`: `percent("World", "Word")` is `88.88...`, `percent("some show us", "some show")` is `85.71...`, `percent("the office", "office")` is `75.0`, `percent("", "")` is `0.0`. `a_long_title_is_cut`: `percent` of two 1 MB strings equals `percent` of their first 200 bytes. `score_table`:

| Kind | Ours, year | Candidate, year | Score |
|---|---|---|---:|
| Tv | `some show`, - | `Some Show`, 2015 | 100 |
| Tv | `some show`, 2015 | `Some Show`, 2016 | 110 |
| Tv | `some show`, 2015 | `Some Show`, 1990 | 0 |
| Tv | `some shows`, 2015 | `Some Show`, 1990 | 0 |
| Tv | `some show`, - | `Another Thing`, - | 0 |
| Tv | `some show`, -, `UK` | `Some Show`, 2005, `US` | 0 |
| Tv | `some show`, -, `UK` | `Some Show`, 2005, `GB` | 102 |
| Tv | `some show`, -, `US` | `Some Show`, 2005, unknown | 100 |
| Tv | `some show`, -, none | `Some Show`, 2005, `GB` | 100 |
| Tv | `some show`, -, `US` | `Some Show`, -, `CA` and `US` | 102 |
| Movie | `some movie`, 2019 | `Some Movie`, 2020 | 110 |
| Movie | `some movie`, 2019 | `Some Movie`, 1985 | 0 |
| Movie | `some movie`, - | `Some Movie`, 1985 | 100 |
| Movie | `some movies`, - | `Some Movie`, 1985 | 0 |

- [ ] **Implement** section 4.
- [ ] **Run:** `python3 scripts/extract_tv_aliases.py "$SRC" && cargo test --lib media`. **Expected:** pass. `git status` shows the TSV written.
- [ ] **Commit:** `Normalize titles to stable keys, and score candidates as upstream does`

### Task 3: Config, the cache file, wipe

**Files:** create `src/media/store.rs`; modify `src/config.rs`, `src/db.rs`, `config.example.json`; tests in those files.

- [ ] **Tests first:**
  - In `src/config.rs`: `media_is_all_off_when_absent`: no `media` object means `any_provider()` false and `parse` true. `tvmaze_needs_its_switch`: `{"tvmaze": {}}` is off. `an_empty_key_is_off`; `a_key_turns_tmdb_on`; `a_client_id_turns_trakt_on`; `requests_per_minute_is_clamped`: 5000 on TVMaze gives 120, 0 gives 1. `the_example_keeps_every_provider_off`: `include_str!("../config.example.json")` parsed has `any_provider() == false`. This is the rule 8 test at config level.
  - In `src/media/store.rs`: `no_media_file_without_a_provider`: `Media::for_main` plus `cached_ids` with default config: no file. `path_for` gives `atlas.media.db` next to `atlas.db`. `a_row_round_trips`; `a_found_title_needs_no_new_lookup`; `a_miss_expires`: `state = 2` before and after `next_check`; `a_new_provider_needs_asking`: `checked_mask` 2 with mask 3 enabled.
  - In `src/db.rs`: `a_wipe_removes_the_media_cache`: create `atlas.media.db`, `-wal`, `-shm` next to a real database; `wipe`; all gone. Fails first: they stay.
- [ ] **Implement** sections 4, 5 and 7.
- [ ] **Run:** `cargo test --lib config && cargo test --lib media && cargo test --lib db`. **Expected:** pass.
- [ ] **Commit:** `Add media config, all providers off, and a per-title cache next to the shards`

### Task 4: The shard table and `media_parse`

**Files:** create `src/media/steps.rs`; modify `src/media/mod.rs`, `src/postprocess/schema.rs`, `src/postprocess/steps.rs`, the harnesses section 4 names; test `tests/media_stage.rs`, starting the stage as `tests/pp_stage.rs` does, settle 1 s.

Build the steps with `Media::for_main(&main)` and pass them to `Stage::start` with `CategoryStep`. Set `media.parse: false` in the harnesses section 4 names. `steps::all()` uses `paths::database()`, which these tests don't set.

Part 3's `an_old_shard_gets_the_new_tables_without_a_rewrite` bounds growth at `4 * (tables + indexes in SHARD_TABLES) + 8` pages, worked out from the registry, so this table's 8 root pages need no change to it: what the test protects is that no row is rewritten.

- [ ] **Tests first:**
  - `a_tv_release_gets_its_row_with_no_ids`: `Some.Show.S01E02.720p.HDTV.x264-GRP` of 2 MiB in `alt.binaries.teevee`, default config. Row: `kind` 1, season 1, episode 2, `title_key` pinned, all IDs NULL. No `atlas.media.db`. Fails first: no table.
  - `a_movie_release_gets_its_row`, `a_daily_show_is_stored_by_date`, `a_season_pack_has_no_episode`.
  - `with_no_fact_it_classifies_for_itself`: clear only bit 32 with `Stage::rerun`. The row is rewritten.
  - `it_runs_with_the_category_step_off`: `"category": {"enabled": false}`. The TV release still gets its row, and no `release_category` row exists. Fails if the step reads the table.
  - `a_group_hint_alone_gets_no_row`: `Plain Name Nothing Else` in `alt.binaries.movies` is category 2999 with `group_name_movie`, and `parse_movie` would accept it. No `media_release` row.
  - `a_parse_held_past_the_fan_out_gets_the_ids`: `media_parse` runs on a cache miss and its `Applied` is held, the title resolves and both fan-out passes finish, then it applies: the row has the IDs.
  - `a_late_article_keeps_ids`: a row with `imdb_id` set, a growth re-run with no provider on: `imdb_id` stays. A rename to a new title: it clears, along with `imdb_from_nfo`.
  - `a_corrected_provider_result_replaces_a_providers_imdb`: a title found with one IMDb ID and fanned out, its cache row deleted, a new `Media` as after a restart, `media_lookup` reprocessed with a corrected answer: every row of the title holds the new ID. `an_nfos_imdb_survives_the_correction`: the same with `imdb_from_nfo = 1` on one row: that row keeps its ID, through `media_parse`, the `Found` write and the fan-out.
  - `an_audio_release_gets_no_row`.
  - `a_rename_away_from_tv_drops_the_row`: a test step at rank 250 renames to `Artist-Album-WEB-2019-320kbps-MP3`; category becomes 3010; the row is gone.
  - `a_hidden_release_gets_no_row`: category 8020.
  - `parse_off_writes_nothing`: `"media": {"parse": false}`.
  - In `src/postprocess/steps.rs`: Part 3's `steps_have_unique_ids_ranks_names_within_their_ranges` with 32, 33, 34, 500, 510, 520. A test in `src/media/steps.rs`: `each_step_has_its_lane`: `media_parse` is `Database`, `media_lookup` is `External`, `media_nfo` is `Bodies`.
- [ ] **Test first** in `src/compact.rs`, on `legacy_fixture`: `an_id_search_works_after_a_compaction`: a row with `tvdb_id`, `run`, `find_with` finds it, and all seven indexes exist. `a_compaction_leaves_the_media_cache_alone`: a file with known bytes next to the shards is byte-identical after. In `tests/convert.rs`: `a_convert_leaves_the_media_cache_alone`.
- [ ] **Implement** sections 5 and 6, `media_parse` only.
- [ ] **Run:** `cargo test --test media_stage && cargo test --test pp_schema && cargo test --lib compact && cargo test --test convert`. **Expected:** pass.
- [ ] **Commit:** `Store each TV or movie release's title, season and episode`

### Task 5: The HTTP layer and TVMaze

**Files:** create `src/media/http.rs`, `src/media/tvmaze.rs`, `tests/common/http.rs`, `tests/fixtures/media/*.json`; modify `tests/common/mod.rs`; test `tests/media_providers.rs`.

```rust
// tests/common/http.rs, tiny_http on 127.0.0.1:0, a thread until dropped
pub struct Route { pub path: &'static str, pub status: u16, pub body: String, pub headers: Vec<(&'static str, String)> }
pub struct FixtureServer { pub base: String, /* requests, routes */ }
impl FixtureServer {
    pub fn start(routes: Vec<Route>) -> FixtureServer;   // first route whose path is a prefix; else 404
    pub fn requests(&self) -> Vec<String>;               // "path?query", in order
    pub fn headers_of(&self, i: usize) -> Vec<(String, String)>;
    pub fn set_routes(&self, routes: Vec<Route>);
}
pub fn fixture(name: &str) -> String;                    // tests/fixtures/media/{name}
```

Fixtures, hand-written, `tvmaze_show.json`:

```json
{"id":101,"name":"Some Show","premiered":"2015-03-02","network":{"country":{"code":"US"}},
 "externals":{"tvrage":2001,"thetvdb":3001,"imdb":"tt0100001"},
 "_embedded":{"akas":[{"name":"Some Alias"}]}}
```

`tvmaze_search.json`: `[{"score":0.9,"show":{...the same shape, id 102, name "Some Other Show"...}},{"score":0.8,"show":{...id 103, name "Some Show", `network.country.code` "GB"...}}]`.

- [ ] **Tests first:** Fails first: no client.
  - `a_hit_takes_one_call`: `find` gives `tvmaze = 101, tvdb = 3001, tvrage = 2001, imdb = 100001`. `requests()` is one: `/singlesearch/shows`, with `q` decoding to `Some Show` and `embed=akas`.
  - `an_aka_matches`: querying `Some Alias` matches.
  - `a_country_mismatch_falls_to_search`: query `Some Show`, country `UK`. The single search's US show scores 0, `/search/shows` is asked, and show 103 is chosen. Fails first: the wrong show is accepted.
  - `a_year_mismatch_falls_to_search`: query `Some Show`, year 1990, against the fixture's 2015: rejected, and search is asked.
  - `a_404_falls_to_search_then_misses_in_two_calls`: both called; `NotFound`.
  - `a_wrong_show_is_rejected`: `Another Thing` against the fixture: score 0, search called, `NotFound`.
  - `a_429_pauses_for_retry_after`: `Retry-After: 120`. `Trouble::Paused(t)` with `t` near now + 120, and the next `find` makes no request.
  - `a_500_pauses_and_doubles`: 60, then 120. A success resets it.
  - `a_connection_refused_pauses`: a closed port.
  - `bad_json_is_bad_not_a_miss`: `Trouble::Bad`.
  - `the_bucket_paces_requests`: `requests_per_minute` 120, 5 calls take at least 1.9 s, which only a broken bucket can miss. A bucket holds at most 1 token, so no burst passes the published limit.
  - `a_two_request_lookup_finishes_at_the_lowest_rate`: TMDB at 1 per minute, a bucket clock moved by hand. The search, `Paused`, then after a refill only `/external_ids`, and `Found`: 2 requests in all, with no second search. The same for TVMaze's `/singlesearch/shows` 404 followed by `/search/shows`.
  - `an_oversized_name_is_no_candidate`: a TVMaze show whose `name` is 1 MB, with a short exact alias in `_embedded.akas`: not chosen, and `found_title` is never over 255 bytes. `a_redirect_isnt_followed`: a 302 to a second loopback server is `Bad`, and that server saw nothing.
  - `no_token_within_two_seconds_gives_paused`: 1 per minute, the second call returns `Paused`, in under 5 s, a bound no loaded machine should reach. In `src/media/http.rs`, `the_bucket_refills_by_the_clock`: `Bucket::wait_for(now: Instant) -> Duration` takes its clock as an argument, so its table runs with no sleep.
  - `a_huge_body_is_refused`: 3 MiB gives `Bad`.
- [ ] **Implement** section 4.
- [ ] **Run:** `cargo test --test media_providers`. **Expected:** pass, with no connection anywhere but `127.0.0.1`.
- [ ] **Commit:** `Add a rate limited HTTP client and the TVMaze provider`

### Task 6: TMDB and Trakt

**Files:** create `src/media/tmdb.rs`, `src/media/trakt.rs`, more fixtures; test `tests/media_providers.rs`.

Fixtures: `tmdb_search_tv.json` `{"results":[{"id":401,"name":"Some Show","original_name":"Some Show","first_air_date":"2015-03-02","origin_country":["US"]}]}`; `tmdb_tv_ids.json` `{"imdb_id":"tt0100001","tvdb_id":3001,"tvrage_id":2001}`; `tmdb_search_movie.json` `{"results":[{"id":501,"title":"Some Movie","original_title":"Some Movie","release_date":"2019-05-01"}]}`; `tmdb_movie_ids.json` `{"imdb_id":"tt0200002"}`; `tmdb_empty.json` `{"results":[]}`; `trakt_show.json` `[{"type":"show","score":100.0,"show":{"title":"Some Show","year":2015,"ids":{"trakt":601,"slug":"some-show","tvdb":3001,"imdb":"tt0100001","tmdb":401,"tvrage":2001}}}]`; `trakt_movie.json`, the same shape, `trakt` 602, `imdb` `tt0200002`, `tmdb` 501.

- [ ] **Tests first:** Fails first: no clients.
  - `tmdb_tv_takes_two_calls_and_finds_external_ids`; `tmdb_movie_passes_the_year_and_finds_imdb`: the request has `year=2019`, `include_adult=false`, `api_key=K`; `tmdb_miss_takes_one_call`; `tmdb_picks_the_best_of_many`: the exact title at position 3 wins; `tmdb_null_ids_are_none`: `{"imdb_id":null,"tvdb_id":null}`.
  - `tmdb_401_is_bad_key_and_records_nothing`; `tmdb_key_never_reaches_the_log`: `Trouble` values and their `Debug` hold no `api_key`.
  - `trakt_sends_its_headers`: `trakt-api-version: 2`, `trakt-api-key: ID`; `trakt_movie_finds_all_its_ids`; `trakt_miss_is_an_empty_list`.
  - `imdb_forms`: `tt0100001`, `0100001`, `tt0`, `nm123`, `""`.
- [ ] **Implement** section 4.
- [ ] **Run:** `cargo test --test media_providers`. **Expected:** pass.
- [ ] **Commit:** `Add the TMDB and Trakt providers`

### Task 7: `resolve`, negative caching and `media_lookup`

**Files:** create `src/media/resolve.rs`; modify `src/media/mod.rs`, `src/media/steps.rs`; test `tests/media_resolve.rs`, and more of `tests/media_stage.rs`.

- [ ] **Tests first**, in `tests/media_resolve.rs`, with fixture servers and `base_url` set. Fails first: no `resolve`.
  - `a_title_is_asked_once`: `resolve` twice: one request.
  - `a_miss_is_cached`: `NotFound`, a row with `state = 2` and `next_check` 30 days out. A second `resolve` makes no request.
  - `a_miss_expires_and_doubles`: past `next_check`: asked again, `misses` 2, `next_check` 60 days out. Capped at 180.
  - `a_miss_that_becomes_a_hit_records_ids_and_asks_for_a_fan_out`: `fanout_done = 0`.
  - `tv_asks_tvmaze_then_tmdb_then_trakt_and_merges`: all three on. `tvmaze`, `tmdb`, `trakt` and `imdb` are all set; 1 + 2 + 1 requests.
  - `a_movie_never_asks_tvmaze`.
  - `a_disagreeing_provider_is_ignored`: Trakt's `tvdb` differs: its `trakt` ID isn't stored, and its bit is set.
  - `an_outage_records_no_miss`: TVMaze 500 and nothing else on: `Later(t)`, `state = 0`, no bit. On recovery: found. This is Part 3's "a failed connection must not mark work complete".
  - `one_provider_down_keeps_the_others_answer`: TVMaze 500, TMDB hit: `Found`, TMDB's bit only. A later `resolve` asks only TVMaze.
  - `bad_json_is_an_attempt`: `Err`.
  - `a_bad_key_is_later_and_records_nothing`.
  - `a_new_provider_is_asked_for_a_known_title`: found with mask 1, then Trakt on: one Trakt request, `traktid` added, `fanout_done = 0`.
  - `every_provider_off_makes_no_request`.
- [ ] **Tests first**, in `tests/media_stage.rs` with stage and fixture servers:
  - `no_config_no_requests`: the rule 8 test. Default config, a TV release, a server listening at a `base_url` that isn't enabled: `requests()` empty, `media_lookup` never runs, `net_wanted` NULL.
  - `one_release_per_title_reaches_the_net_lane`: 50 releases of one title, across seasons and groups, all saved at once: exactly one request. The other 49 have `pp_state.net_wanted` NULL.
  - `only_tv_and_movie_go_out`: an audio, a book, a PC and a hashed release: no request.
  - `a_blocked_group_makes_no_request`: a TV name in a group Part 1 blocks.
  - `a_rejected_release_makes_no_request`: every file a par2, under `reject_par2_only`.
  - `a_group_hint_alone_makes_no_request`: `Plain Name Nothing Else` in `alt.binaries.movies`.
  - `an_image_post_makes_no_request`: `Some.Movie.2019.1080p.jpg`, which Part 7 places at 8000.
  - `a_release_after_the_title_resolved_gets_ids_from_the_cache`: no new request, IDs in its row after `media_parse`.
  - `an_outage_waits_without_using_attempts`: `net_attempts` stays 0, `net_next_at` moves. This relies on Part 3's `wait_until` refund.
  - `the_representative_gets_its_ids_through_the_step`: without the fan-out thread attached.
  - `a_stale_lookup_cant_attach_the_old_titles_ids`: `media_lookup` blocked on its provider, a rename makes `media_parse` write a new title and clear the step, then the old lookup returns `Found`: the row's IDs stay NULL. The next lookup's `NotFound` for the new title leaves them NULL.
  - `a_rename_gets_a_new_title`: `media_parse` rewrites, and `media_lookup` asks for the new title.
  - `a_provider_turning_on_starts_a_walk`: Part 3's `pp_steps_seen` adds a `'steps'` walk. Old releases then go through one request per title.
  - `a_deleted_cache_row_needs_a_restart`: the same with the first `Media` still running: no request, as documented in section 10.
  - `an_oversized_title_is_no_target`: a TV-shaped name with a 1 MB show title: `media_target` is `None`, no row, no request, no cache row.
  - `titles_stop_at_their_cap`: `max_titles` at 1,000 with that many rows: a new title's `wants` is false and counted, no request and no row. A known title's answer still comes from the cache and fans out.
  - `claims_stop_at_their_cap`: through `#[doc(hidden)] with_max_claims(3)`: a fourth distinct title's `wants` is false and counted, the map stays at 3, and a claim over 7 days old makes room.
  - `a_second_provider_turning_on_starts_a_walk`: TVMaze on and a TV title and a movie looked up, then a TMDB key added: `media_lookup` stays enabled, its `config_stamp` gains a bit, one walk with its `redo_mask` runs, the movie and the TV title each make their TMDB requests and no TVMaze request, and the new IDs fan out.
- [ ] **Implement** sections 4 and 6.
- [ ] **Run:** `cargo test --test media_resolve && cargo test --test media_stage`. **Expected:** pass.
- [ ] **Commit:** `Resolve each title once, cache misses, and look up within the net lane`

### Task 8: Fan-out

**Files:** create `src/media/fanout.rs`; modify `src/bg_indexer.rs`; test `tests/media_stage.rs`, and `src/bg_indexer.rs`.

- [ ] **Tests first:**
  - `every_row_of_a_title_gets_its_ids_in_every_shard`: rows for one title in 3 shards, IDs NULL, a found title with `fanout_done = 0`. Start `media::run`. All rows hold the IDs, `fanout_done` is 1 after pass 2 (`FANOUT_SECOND_PASS` set to 1 s through `#[doc(hidden)] with_second_pass`). Fails first: NULL.
  - `a_big_title_goes_in_chunks`: 2,500 rows. A writer stage-job counter shows at least 3 jobs. All rows updated.
  - `a_row_that_slips_past_pass_one_is_caught_by_pass_two`: insert a row between the passes.
  - `a_restart_finishes_a_fan_out`: `fanout_done = 0` and no thread; start one: rows filled.
  - `an_unchanged_row_is_not_written`: count `total_changes` on a second pass: 0.
  - `the_update_uses_its_index`: `explain query plan` of the `update` holds `media_release_title`.
  - `it_stops_when_asked_and_holds_no_writer`: `stopping` true mid-title: returns. Dropping the `Db` then lets writers end. The title stays `fanout_done = 0`.
  - `a_provider_turning_on_or_off_rebuilds`, in `src/bg_indexer.rs`: `media.any_provider()` changing asks for a rebuild.
  - `tests/media_bg.rs`, one test, env set first, as `tests/auto_compact.rs`: `every_shard_is_rewritten_with_the_fan_out_running`. A mock NNTP server with TV posts, `auto_run_compact: true`, `ATLAS_COMPACT_EVERY_SECS=2`, TVMaze on at a fixture server's `base_url`, and a found title with `fanout_done = 0` whose rows sit in several shards, so `media::run` and the net lane are busy at each stop. Every compaction rewrites all 8 shards (`common::shard_inodes` before and after, every inode changed, as `tests/pp_auto_compact.rs` asserts), `media_release` rows survive, and the fixture server saw only its own address. This is what fails if `media::run` or a provider call outlives `run`.
- [ ] **Implement** sections 4 and 5.
- [ ] **Run:** `cargo test --test media_stage && cargo test --test media_bg && cargo test --lib bg_indexer && cargo test --test auto_compact`. **Expected:** pass. `media_bg` shows every shard rewritten, since `media::run` holds nothing past `stopping`.
- [ ] **Commit:** `Fill in a resolved title's IDs on all its releases`

### Task 9: `media_nfo`

**Files:** modify `src/media/steps.rs`, `src/media/mod.rs`, `src/postprocess/steps.rs`; test `tests/media_nfo.rs`, on Part 3's `tests/pp_bodies.rs` harness on the mock server, with `MediaParse`, `MediaNfo` and `CategoryStep` in the list.

- [ ] **Tests first:** Fails first: no step.
  - `no_budget_no_body`: default config. A movie with an NFO: `bodies_sent == 0`, the step never runs, `net_wanted` NULL. This is the rule 8 test.
  - `an_nfo_gives_the_imdb_id`: budget set, `Some.Movie.2019.1080p.BluRay.x264-GRP` with `some.nfo` holding `http://www.imdb.com/title/tt0200002/`: one body, with `imdb_id` 200002 and no provider on.
  - `regex_table`, on `regex::bytes`: `www.imdb.com/title/tt0133093/` gives 133093, `IMDB: tt12345678 ok` gives 12345678, `attt1234567` and `tt123456` give none.
  - `tv_never_fetches`: a TV release with an NFO: no body.
  - `no_nfo_no_body`, `a_set_imdb_id_spends_no_body`, `a_known_title_spends_no_body`: with a provider's `known` entry holding an IMDb id, `applies` is false.
  - `a_missing_article_is_done`, `an_oversized_nfo_is_done`: `Got::Missing`, `Got::TooBig`: done, no ID, no retry.
  - `the_cap_is_64_kib`: the recorded `BodyRequest` has `max_bytes: Some(65536)`.
  - `an_nfo_id_survives_a_fan_out_and_a_late_article`: the title resolves to a different IMDb id: the row keeps the NFO's. The other rows of the title get the title's.
  - `caps_offers_imdbid_with_a_budget`: `id_params(&default, true).movie.imdb` is true, `.tv.imdb` false.
- [ ] **Implement** section 6.
- [ ] **Run:** `cargo test --test media_nfo && cargo test --test media_stage`. **Expected:** pass.
- [ ] **Commit:** `Read a movie's IMDb id from its NFO once a body budget is set`

### Task 10: The API

**Files:** create `src/media/query.rs`; modify `src/api.rs`, `src/search.rs`; tests in `src/api.rs`, `src/media/query.rs`, and `tests/media_api.rs` (one test, env set first, as `tests/e2e.rs`).

- [ ] **Tests first:** Fails first: no module.
  - In `src/media/query.rs`: `from_args_table`, with all on:

| `t` | Args | Result |
|---|---|---|
| `tvsearch` | `tvdbid=3001&season=1&ep=2` | `[(Tvdb, 3001)]`, season 1, episode 2 |
| `tvsearch` | `tvdbid=3001&imdbid=tt0100001&rid=2001&tvmazeid=101&tmdbid=401&season=1&ep=2` | all five |
| `movie` | `imdbid=0200002` | `[(Imdb, 200002)]` |
| `movie` | `imdbid=tt0200002&tmdbid=501` | both |
| `tvsearch` | `tvrageid=2001` | `[(Tvrage, 2001)]` |
| `tvsearch` | `tvdbid=3001&season=2025&ep=11/10` | season 2025, episode 1110 |
| `tvsearch` | `tvdbid=3001&ep=2` | no episode |
| `tvsearch` | `tvdbid=0` | `None` |
| `tvsearch` | `q=Some+Show&season=1` | `None` |
| `movie` | `tvdbid=3001` | `None`, not a movie parameter |
| `search` | `tvdbid=3001` | `None` |
| `music` | `imdbid=1` | `None` |

  With nothing on, every row is `None`. With only TVMaze on, `tmdbid=401` alone is `None`.
  - `a_big_id_keeps_its_newest_ids`, with `ID_SCAN` and `ID_ROWS` lowered through `#[doc(hidden)]` parameters of `find_with`: 50 matching rows and a cap of 5 return ids 46–50, in the episode, season and id-alone forms. `the_id_queries_use_their_covering_index`: `explain query plan` of each form has `USING COVERING INDEX` and no temp b-tree.
  - `attrs_xml`: a full TV row, all on, gives `season`, `episode`, `year`, `tvdbid`, `tvmazeid`, `traktid`, `tmdbid`, `rageid`, `tvrageid`, `imdb value="0100001"`, `imdbid value="0100001"`. A daily row gives `tvairdate value="2024-03-15"`, `season` 2024 and `episode` `03/15`. A pack gives `season` and no `episode`. IDs that are off in `IdParams` are left out; `season` stays.
  - In `src/api.rs`: `caps_reflects_what_is_on`, on `caps_body` with explicit `IdParams`, so no test here changes the environment. Part 4's and Part 7's `rss_*` tests gain the new arguments. Nothing on: `tv-search` is `q,season,ep,cat,limit,offset` with no ID. TVMaze on: adds `tvdbid`, `imdbid`, `rid`, `tvmazeid`. All on: every one, and `movie-search` has `imdbid,tmdbid,traktid`. `search_term_finds_a_daily_show`: `q=Some+Show&season=2025&ep=11/10` gives `Some Show 2025 11 10`.
- [ ] **Test first:** `tests/media_api.rs`, `id_searches_end_to_end`. Set `ATLAS_HOME`, write `config.json`. Save hand-named releases across 3 groups in different shards, and set `release_category` and `media_release` by direct SQL. Through `api::handle`:

| Query | Provider on | Result |
|---|---|---|
| `t=caps` | none | no `tvdbid` |
| `t=tvsearch&tvdbid=3001` | none | empty, status 200 |
| `t=tvsearch&tvdbid=3001&q=Some+Show` | none | by name |
| `t=movie&imdbid=0200002` | none | empty |
| `t=caps` | TVMaze, TMDB | `tvdbid`, and `imdbid` on movie |
| `t=tvsearch&tvdbid=3001` | TVMaze, TMDB | the show's visible releases across shards, newest first |
| `t=tvsearch&tvdbid=3001&season=1&ep=2` | same | that episode, and S01E01-E03, not S01 packs |
| `t=tvsearch&tvdbid=3001&season=1` | same | packs and episodes of season 1 |
| `t=tvsearch&tvdbid=3001&season=2025&ep=11/10` | same | the daily release |
| `t=tvsearch&tvdbid=3001&imdbid=tt0100001&tmdbid=401&cat=5040` | same | any of them, once each, in 5040 only |
| `t=tvsearch&tmdbid=501` | same | empty: 501 is a movie id |
| `t=movie&imdbid=tt0200002&cat=2000` | same | the movie, attrs `imdb value="0200002"` |
| `t=movie&tmdbid=501` | same | the movie |
| `t=tvsearch&tvdbid=9999` | same | empty |
| `t=tvsearch&tvdbid=9999&q=Some+Show` | same | by name, the fallback |
| `t=tvsearch&traktid=601` | same | empty: Trakt is off |
| `t=tvsearch&tvdbid=3001&limit=1&offset=1` | same | the second newest |

  A hidden release with a matching ID never shows.
- [ ] **Test first** in `src/media/query.rs`: `find_with_is_bounded`: `Bounds { rejects: 10, ..Bounds::DEFAULT }` with 100 rows all failing `cat`: `scanned` is 11. `id_rows_are_capped_per_shard`: `id_rows` 5 with 50 rows in a shard: at most 5 of them are looked at.
- [ ] **Implement** sections 4 and 6.
- [ ] **Run:** `cargo test --lib media::query && cargo test --lib api && cargo test --test media_api && cargo test --test e2e && cargo test --test category_api`. **Expected:** pass. The two existing search tests in `src/api.rs` (`:389`) pass with no change.
- [ ] **Commit:** `Answer ID searches and report IDs in results and caps`

### Task 11: Running with no keys, docs, full verification

**Files:** `README.md`, `config.example.json`; test `tests/media_no_keys.rs`.

- [ ] **Test first:** `everything_runs_with_no_provider`: one test, env set first, as `tests/auto_compact.rs`. A mock NNTP server (`tests/common/mod.rs`) with TV and movie posts, a default-style `config.json`, a fixture server whose address appears nowhere in config. Run the indexer's stage until rows settle. Releases are indexed, `release_category` and `media_release` are filled, `atlas.media.db` doesn't exist, the fixture server has no requests, and `t=tvsearch&q=...` answers with `season` and `episode`. `t=tvsearch&tvdbid=1` is empty. Fails first: no `media_release` rows.
- [ ] **Test first:** `the_docs_describe_ids`, in `src/config.rs`, on `include_str!`: the README holds `### TV and movie IDs`, and no longer holds `searches by ID, such as TVDB or IMDb, return nothing`.
- [ ] **Implement** section 12.
- [ ] **Run:** `just format-check && just lint && just test`, then section 11. **Expected:** all pass.
- [ ] **Commit:** `Document TV and movie IDs`

## 9. Performance and load

Nothing here was measured on live data. Distinct titles can't be counted without reading it, so request counts are per title with a stated scenario. Check the real figure afterwards with `select state, count(*) from media_titles group by 1` on a copy.

**Published limits.** TheTVDB's licensing page was read directly. The TVMaze and TMDB documentation pages couldn't be fetched when this was written, so their figures come from search results quoting them. Re-read them before raising a default.

| Provider | Published | Atlas default | Clamp |
|---|---|---:|---:|
| TVMaze | At least 20 calls per 10 s per IP, 429 past it, no key | 90/min | 120/min |
| TMDB | The old 40 per 10 s was removed on 2019-12-16; about 50/s is quoted as the upper bound, 429 past it; free for non-commercial use with attribution | 240/min | 3,000/min |
| Trakt | 1,000 GET per 5 min | 120/min | 200/min |

**118M releases are not 118M requests.** Part 3's revision sends about 100M rows through the steps, so the rules no longer filter much. What stands in the way: a name-based category (TV or movie, not a group hint or an image), a title that parses, one request set per distinct title, and the cache for every later release. A title costs at most 5 calls: TVMaze 2, TMDB 2, Trakt 1.

| Calls per title | Found | Not found |
|---|---:|---:|
| TVMaze | 1 | 2 |
| TMDB | 2 | 1 |
| Trakt | 1 | 1 |

**The backlog.** The scenario is 1M distinct titles, 300,000 TV and 700,000 movie, 60% found. It scales linearly: 200,000 titles is a fifth, and 3M three times, so 14 days for TMDB at its default. Single-article posts now reach the parser, which pushes the real figure up; raise TMDB's rate if it lands near 3M.

| Providers on | Requests | At defaults | At the published limit |
|---|---:|---:|---:|
| TVMaze (TV only) | 420,000 | 3.2 days | 2.4 days |
| TMDB | 1.6M | 4.6 days | 8.9 h, with 10+ net threads |
| Trakt | 1M | 5.8 days | 3.5 days |
| TVMaze + TMDB | 2.02M | 4.6 days, TMDB's bucket decides | 2.4 days, TVMaze's decides |
| All three | 3.02M | 5.8 days, Trakt's decides | 3.5 days |

`body_connections` sets the net threads. A call takes about 200 ms, so the default 2 threads give 10 calls/s, above every default. Above about 8 calls/s of total budget, raise it. If `Bodies` steps are also on they share these threads: expect body throughput to drop by about the share of time threads spend waiting on tokens, and raise `body_connections` by 2.

**Steady state.** A new release of a known title makes no request. 2,000 new titles a day is at most 10,000 calls, minutes of budget.

**The parse walk.** Part 3's one first-start walk, 16.4 h at 2,000 rows/s, about 1,700 runs/s. `media_parse` reuses the facts, so it adds no second classify: a release that isn't a target costs under 1 µs and a primary key read under 5 µs. A target adds a parse of 10–40 µs and about 40 µs on a writer, as a row plus 2 index inserts. All of it is about 1–2% of a core. With a provider on, `media_lookup.applies` classifies and parses again, about twice per row: about 9% of a core during the walk, and nothing without a provider.

**Why not every TV and movie row through the net lane.** That design needs no claims and no fan-out, but each row costs a judge and two writer round trips (Part 3, Task 10). Derived: 2 threads manage about 600 rows/s on idle writers, and 13–30 rows/s behind writers at peak ingest. 5M rows would add 2.3 h at best and 2–4.5 days at peak, on top of the lookups, with lookups and body steps waiting in line behind rows whose answer is cached. A title that turns from a miss into a hit would also leave its old rows without IDs. One claim per title leaves at most one row per title in the lane, and the fan-out writes 1,000 rows in about 30 ms.

**Fan-out.** One write per release that existed before its title resolved: at most every TV and movie row once. A row and about 4 partial index inserts is about 30 µs, 1,000 rows a job is 30 ms, and jobs run within Part 3's writer budget, so one job per save cycle under load. At 20M rows that's 10 min of writer time spread across 8 writers and several days.

**`media_nfo`.** Off until a body budget is set. Then each movie release that has no IMDb id yet takes a net-lane claim, and one body of at most 64 KiB if it has an NFO. Bodies count against `body_fetches_per_minute`: at 300/min, 432,000 a day. Releases with no NFO cost the lane's two writer round trips and no body: for 5M movie rows, 2.3 h on idle writers to 2–4.5 days at peak ingest, shared with the other net steps. A provider that already knows a title's IMDb id takes its releases out of this.

**Disk.** Shards, about 70 B a row with no IDs, 170 B with them (derived). Rows exist only for releases whose name gives a TV or movie category and a title. That can't be counted without live data, and more rows reach the step than before, so these cases bracket it:

| Rows in `media_release` | No provider | Providers on | Share of 300 GB |
|---:|---:|---:|---:|
| 5M | 350 MB | 850 MB | 0.3% |
| 20M | 1.4 GB | 3.4 GB | 1.1% |
| 40M, more than every multi-article release | 2.8 GB | 6.8 GB | 2.3% |

These are compacted sizes. The first-start walk fills `media_release` newest first, which leaves its pages about half full (Part 3 measures about double for `pp_state`), so until the next `--compact` plan on about twice the shard figures; the seven indexes fill the same way.

`atlas.media.db`, about 130 B a title (derived):

| Titles | Size |
|---:|---:|
| 200,000 | 26 MB |
| 1M | 130 MB |
| 3M | 390 MB |

**Memory.** The title map, only with a provider on: about 30 MB per 1M titles. Claims: small, one entry per title in flight. One `Mutex<Connection>` for the cache. A response body at most 2 MiB per thread.

**API, at 118M.** A shard holds at most 4.7M rows. Read from `EXPLAIN QUERY PLAN`, on 1M rows per shard; hot figures derived from Part 7's measured 1.4 µs a probe and 2 uncached pages at 100 µs for cold.

| Query | Index seeks | Rows read | Hot | Cold |
|---|---:|---:|---:|---:|
| `tvdbid` + `season` + `ep` | 8 | Under 100 | 0.2 ms | 20 ms |
| Sonarr's aggregate, 5 IDs + `season` + `ep` | 40 | Under 100 | 0.5 ms | 30 ms |
| `tvdbid` + `season`, up to thousands of rows | 8 | Up to 160,000 ids, then `limit` probes | Under 5 ms | 100 ms |
| `tvdbid` alone, a long running show | 8 | At most 160,000 ids, then `limit` probes | Under 20 ms | 200 ms |
| `imdbid` for a movie | 8 | Under 200 | 0.2 ms | 20 ms |
| Worst: 5 IDs, all at the cap | 40 | 800,000 ids, 6.4 MB | 60 ms | 500 ms |
| `attrs_of`, 100 ids | up to 8 | 100 | 0.2 ms | 20 ms |

Scans are bounded by `ID_SCAN` index rows and `ID_ROWS` matches per shard and ID, by 2,000 rejected rows, and by `offset <= 10,000`. Without an ID parameter nothing changes, beyond one 100-id lookup for attributes.

## 10. Failure handling

| Case | What happens |
|---|---|
| Name won't parse | No row. `matched: false`. Normal for many names. |
| Parse gives junk as a title | A clean miss, cached 30 days, doubling. One or two requests lost per junk title. |
| Wrong match | Limited by the score floor and the year rules. Stays until corrected by hand: stop the indexer, delete the title's `media_titles` row, start it, and run `atlas --reprocess --step media_lookup`. A running indexer's in-memory map still says the title is known, so the restart is required. Sonarr parses the name too, so it can reject a wrong one. |
| Provider down | `Later`. No attempt, no lane pause, no miss recorded. The pause doubles to 1 h, so the queue isn't retried faster than that. Indexing, the DB lane and bodies carry on. |
| 429 | Pause for `Retry-After`. The bucket should prevent it, and the limit is shared with anything else on that IP. |
| Bad key | Pause 1 h, logged once per pause, no miss recorded. Fixing the key takes effect on the next config reload (`watch_config`, 5 s), which rebuilds providers and clears pauses. |
| Provider changes its JSON | `Trouble::Bad`: attempts, backoff (Part 3), then the failed bit on that release. Another release of the title can try after the claim's 7 days. |
| `atlas.media.db` missing | Created when a provider is on. IDs in shards stay. Titles are asked again as releases are visited. |
| `atlas.media.db` corrupt or unopenable | Logged once. `media_lookup.applies` is false and `cached_ids` is `None`. Indexing and the API carry on. Delete the file to rebuild it. |
| Crash mid-lookup | Part 3's `Begin` counted the attempt. Answers already written stay, so the retry asks only the rest. |
| Crash mid-fan-out | `fanout_done` is still 0. The next start finishes it. |
| Clean stop mid-fan-out | The same. `media::run` drops its `Db` before writers stop. |
| Rep release deleted or purged while queued | Its queue row goes with it. A later release of the title claims after 7 days, or at once after a restart. |
| Rep fails permanently | Failed bit, as above. Other releases keep NULL IDs until a later one succeeds, and then get them from the fan-out. |
| Provider turned off | Rows keep their IDs. The parameter leaves `caps`, ID searches on it return empty, and its attribute is hidden. Turning it back on restores them with no requests for titles already checked. |
| Rename | All three steps re-run. A new title clears the row's IDs, including an NFO's, and `media_nfo` reads it again. The old title's cache row stays. |
| NFO names a different film than the title resolved to | The row keeps the NFO's IMDb id and the title's other IDs. |
| Country or year on the name disagrees with the only candidate | Score 0, so it falls to `/search/shows` or to the next provider, then a clean miss. Better no ID than the wrong show's. |
| Compaction | The stage and `media::run` have stopped. `atlas.media.db` isn't involved. |
| Malformed `ep`, `season` or ID | Dropped. An ID that doesn't parse doesn't count as given, so a request with only that falls to `search_term`'s `None` path and returns empty. |
| `config.json` unreadable when `caps` is asked | `media_config` gives the defaults, so nothing is advertised. |
| Re-run of anything | `insert or replace` and unchanged-row updates are idempotent. |

## 11. Acceptance checks

Run in the part's worktree. None touch live data, a live server or a provider.

```bash
just format-check && just lint && just test
# expected: all pass, no warnings

cargo test --test media_stage no_config_no_requests
# expected: pass, zero requests with the default config. The rule 8 check.

cargo test --test media_stage one_release_per_title_reaches_the_net_lane
# expected: pass, one request for 50 releases of a title

cargo test --test media_stage only_tv_and_movie_go_out
# expected: pass, zero requests

cargo test --test media_nfo no_budget_no_body
# expected: pass, bodies_sent == 0 with the default config

cargo test --test media_resolve a_miss_is_cached
# expected: pass, one request across two lookups

cargo test --test media_no_keys
# expected: pass, everything indexes and categorizes with no atlas.media.db

cargo test --test media_api
# expected: pass, including empty answers for parameters that are off

cargo test --lib media::parse
# expected: pass, both tables

cargo test --lib db::tests::a_wipe_removes_the_media_cache
# expected: pass

! grep -rn "api.tvmaze.com\|api.themoviedb.org\|api.trakt.tv" tests/
# expected: no output: no test names a real provider

! grep -rEn "ImdbScraper|imdb\.com" src/media/
# expected: no output
```

## 12. Docs

- `README.md`, Features (`:52`): ID searches, with providers off by default.
- "Top-level keys" (`:127`): a `media` row and a new table from section 7.
- New `### TV and movie IDs` under "Newznab API": which parameters each provider enables; that `media.nfo` reads a movie's IMDb id from its NFO once `postprocess.body_fetches_per_minute` is set, with no provider; how to get a TMDB key and a Trakt client id; that TVMaze needs only `tvmaze.enabled`; that Sonarr and Radarr cache `caps` for 7 days, so press Test on the indexer after enabling one; the time and request tables from section 9, labelled as estimates for 1M titles; that a walk starts when a provider turns on.
- "Operations" (`:383-384`): `tvsearch` and `movie` take IDs when a provider is on.
- `:389`: replace with what happens with a parameter on and off.
- "Parameters" (`:397`): the ID parameters; `season` YYYY with `ep` MM/DD. "Search results" (`:435`): the attributes.
- "Settings", "Wipe DB and cache" (`:298`): names `atlas.media.db`.
- "Environment variables": no change. "Limitations" (`:514`): remove the line, and add that IDs need a provider.
- "Credits" (`:516`): newznab-tmux, GPLv3. "This product uses the TMDB API but is not endorsed or certified by TMDB." TV data from TVMaze, CC BY-SA. Trakt.
- `config.example.json`: the block with everything off.

## 13. Decisions

1. **A Database parse, an External lookup and a Bodies NFO read.** Part 3's table lists Part 8 under both lanes. A fact lives for one run, and the DB lane and net lane never share a run, so a net step can't see the `category` fact. Part 4 found the same. `media_parse` at rank 500 is in the DB lane right after category and reads the facts, or classifies for itself when they are absent, so it doesn't depend on `category.enabled`. `media_lookup` and `media_nfo` call the same pure function.
2. **`applies` uses pure functions and an in-memory map.** It may not do I/O and has only `ReleaseInfo`. Classify and parse are pure, and Part 7 exposes `classify_release` for this. Without this, every category-matching row would set `net_wanted`. See section 9 for why that fails.
3. **One release per title in the net lane.** Requests follow titles, not rows. The claim makes `applies` stateful, which is the price of Part 3's per-release lane. Claims are advisory: losing one costs at most a duplicate that finds the cache filled.
4. **IDs per release in the shard, as asked, with a fan-out.** A release stored only `title_key`, joined to the cache at read time, would be a third smaller and need no fan-out. It was rejected because it makes the cache file part of the data: deleting a "cache" would lose every ID search. It would also put a second database in the API's read path. The cost of this choice is 7 indexes, and a fan-out thread with two passes.
5. **TVMaze, TMDB and Trakt, each optional.** TVMaze: no key, gives TVDB, IMDb and TVRage ids in one call, TV only. TMDB: the only one covering movies well with a free key, and gives IMDb and TVDB ids. Trakt: adds `traktid`, and is a fallback search for both kinds. Each uses a documented API.
6. **No TVDB client.** `tvdbid` is what Sonarr wants, and all three providers supply it. TheTVDB's own API needs a licence and a token flow (`thetvdb.com/api-information`) and adds no ID the others lack. A title all three miss gets no `tvdbid`, which is the accepted cost. The schema has the column, so a client can be added later with no migration.
7. **No IMDb scraper.** The brief rules it out. IMDb IDs come from TVMaze, TMDB and Trakt.
8. **Defaults under the published limits, and clamps at them.** Headroom for anything else on that IP, and a config typo can't exceed the limit. TMDB's 240/min is far below its ceiling because its terms ask for respect, and 2 threads can't use much more anyway.
9. **`wait_until`, not `Transient` or `OutOfBudget`.** `Transient` pauses the whole net lane 60 s, which would stall body fetching because of one metadata provider. `OutOfBudget` is a fixed 60 s and would spin the queue past a 1 h pause. `wait_until` parks each release until its provider is back, uses no attempt, and sets no bit.
10. **Part 3 has no External budget, so Part 8 owns its buckets.** `body_fetches_per_minute` covers bodies only. A bucket per provider blocks a thread at most 2 s, which paces the lane to the budget with no spinning. Part 3 needs no change.
11. **Negative results expire after 30 days, doubling to 180, and only on a visit.** New shows get added to providers. A background sweep would need its own scheduler and make requests no user is waiting for.
12. **Positive results never expire.** An ID doesn't change meaning. `--reprocess` covers the rare correction.
13. **The first provider's identity wins, and a disagreeing answer is dropped entirely.** Mixing IDs from two different shows is worse than missing one.
14. **Stricter than upstream on year and country.** Upstream accepts an exact TV title with a far-off year at 80, and a country mismatch costs it nothing. But a scene name carries a year or country suffix exactly because the title clashes (`The.Flash.1990`, `Some.Show.UK`), and TVMaze's single search returns one show. So either mismatch scores 0 and falls through to search or the next provider. A wrong ID makes Sonarr file a release under the wrong series, and a missing one only costs a text search. A movie with no year on our side counts only on an exact title.
15. **Several IDs in a request mean any.** Sonarr and Radarr send all they have. An AND would drop a release whose title lacks one ID.
16. **An ID wins over `q`, and `q` is the fallback when it finds nothing.** This extends today's "an ID next to `q` searches by `q`" without losing a release that has no IDs yet, which matters during the backlog.
17. **`caps` reads the file on each call and the ID path every 30 s, with no restart.** `api::start` runs in the menu process, and the API has no config reload of its own. Only the `media` key is read, with no keyring call.
18. **No two-digit-year dates.** `AbstractTvProvider.php:1325` reads `Show.10.09.08` as a date, and needs the current year to do so. Version numbers and part numbers look the same. Dropped; a wrong season is worse than none.
19. **Parser fixes over upstream.** `-[A-Z0-9].*$` with `/i` turns `Anti-Hero` into `Anti`, so the strip applies only to the no-year fallback and only at the end. All-digit titles are allowed with a year (`1984`). Guards after each number stop `S01E02-720p` reading as episodes 2 to 720. `\w` needs Unicode here, so non-English titles parse.
20. **Daily shows use `season` = year and `episode` = MMDD in the same columns.** Sonarr asks `season=2025&ep=11/10`, which then maps straight onto the ID indexes, with no extra index. `air_date` is kept for the attribute.
21. **`media_parse` runs with no provider.** No network, and it costs 350 MB to 2.8 GB across section 9's cases. Results gain `season` and `episode`, and turning on a provider needs no second parse pass. `media.parse: false` removes it.
22. **A separate file, `atlas.media.db`.** The brief asks for it. It also keeps the title cache out of the main database, whose loss is serious, and out of the shards, so a title found in one shard serves all 8. It's opened only with a provider on, so a default install never has one.
23. **Fan-out runs twice, 60 s apart.** A release whose parse read the cache just before a title resolved can commit just after the first pass. Both are cheap because unchanged rows aren't written. A stall past 60 s can leave one row with NULL IDs until reprocessed, which is accepted.
24. **Season search returns packs and episodes; episode search returns no pack.** Sonarr's season search wants both. Its single-episode search would reject a pack anyway.
25. **IDs that are off are hidden from results, though stored.** Otherwise a client could see a `tvmazeid` it can't search by, and the "provider off" state would be only half true.
26. **`Later` adds `release_id % 30` seconds.** Parked releases then don't all wake in the same instant when a pause ends.
27. **Part 3 notes:** `facts` don't cross lanes (decision 1). There is no External budget (decision 10). `an_outage_waits_without_using_attempts` depends on Part 3's `wait_until` refund.
28. **`media_nfo`, off until a body budget is set.** An NFO that states an IMDb id needs no provider and can't resolve to the wrong film, so its ID beats a title's. It's movie only because a TV NFO's id is often the series or another episode's, and it's one body at 64 KiB because that's enough for an NFO. Reading it adds a `Bodies` step to this part, so rule 8 is held by `no_budget_no_body`.
29. **A group hint alone doesn't reach `media_release` or a provider.** Part 3's revision sends about 100M rows here, including small posts and anything in a hinted group. `parse_movie`'s fallback accepts a plain name, so `Plain Name Nothing Else` in `alt.binaries.movies` would otherwise become a title and a TMDB request, and a group of junk names would become thousands of them.
30. **ID indexes put `release_id` right after the ID.** The first form, ordered by season and episode, made a cap keep the oldest rows of a popular title, and sorting afterwards can't bring back what was cut. Newest first needs the order in the index, with a season or episode filter read from it. The covering columns cost about 2 bytes a row over the old form. A season of a title with more than `ID_SCAN` rows in one shard can miss its oldest releases, the side a client minds least.
31. **`imdb_from_nfo` records where an IMDb ID came from.** Keeping every existing IMDb ID meant a corrected provider answer changed the other IDs and left the wrong IMDb ID behind. Only an NFO's is kept.
32. **A new provider redoes `media_lookup` through `config_stamp`.** The step stays enabled, so Part 3's step mask sees nothing. The stamp walks once, and `wants` keeps it to one request per title, for the new provider only.
33. **Correcting a wrong match needs a restart, not cache invalidation.** Deleting a cache row by hand is a rare repair, and the `known` map is what keeps `applies` free of I/O. Polling for outside changes to `atlas.media.db` would add a moving part for that one case. The order is in section 10, and `a_deleted_cache_row_needs_a_restart` pins it.
34. **Distinct titles are capped, not evicted.** A poster chooses release names, so a provider on makes title count an outside input. Evicting would re-ask providers for real titles and spend their budget, where stopping at a generous cap costs only IDs for later titles, shown on the dashboard. Provider rate limits mean reaching it takes months at the defaults.
