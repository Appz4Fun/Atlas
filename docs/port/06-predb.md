# Part 6: PreDB

Every Atlas line number below is at base commit `de598b6`. Parts 1–5 merge first and shift them: find the named function, keep the intent, and record any difference in the PR. Upstream paths are at the commit pinned in `00-overview.md`. Part 3 wins any disagreement with this plan.

The key, import, feed-parsing and matching code was prototyped in a scratch crate, on SQLite through `rusqlite` 0.40. Numbers marked "measured" come from synthetic data there, never from live data, with an unrelated compaction taking CPU on the same machine. Every feed shape below comes from upstream's source. None was fetched, so the first real poll is the first contact with a live response (decision 18).

## 1. Goal

Atlas can keep a catalogue of scene and P2P release titles from public JSON feeds, in its own file, and use it to give a release its exact name. A release matches on its title, on the title with different separators, on a hash of the title, or on an inner file. A title that arrives after the release was indexed still finds it, through an index or the full-text index, with no scan. A match renames through Part 3 with Part 5's highest source. Everything network is off until `predb.enabled` and `predb.sources` are set. With an existing `config.json` nothing is fetched, no file is made and no step runs.

## 2. Non-goals

- No IRC client (decision 3). No WebSocket stream (decision 4).
- No RSS, HTML-scrape or XML sources in this part. No source that needs an API key.
- No fuzzy title matching. Upstream's own matcher says partial evidence never authorizes a link (`ConstrainedPredbMatcher.php:13`).
- No download of any dump. `atlas --predb-import` reads a file the user already has, dry run by default.
- No request-id matching: no feed here fills a request id.
- No new API attribute. Part 7 and Part 8 can read `predb_match`.
- Nuked titles are not hidden. `nuked` is stored and reported only.

## 3. Source map

| Upstream | Take | Leave behind |
|---|---|---|
| `config/predb_feeds.php:15,47-49` | Off by default, a list of sources | The default list: here it is empty |
| `config/predb_feeds.php:61-93` | Endpoints for the five sources ported | The others, table below |
| `config/predb_feeds.php:99-102` | History and dumps are never automatic | |
| `config/predb_feeds.php:55,114-121` | Bounded bodies, 1 s between pages, a default throttle wait of 60 s | Sleeping through a throttle: here the poll ends and is rescheduled |
| `routes/console.php:129` | A 5-minute poll | The scheduler |
| `Feeds/Sources/PredbClubSource.php:19,45-110` | Row shape, `data.rows`, `count` capped at 100, nuke types, error envelope | FFI |
| `Feeds/Sources/PredbNetSource.php:18,41-108`, `BUGS.md` #25 | Row shape, `data`, `MAX_PAGE = 100`: page 101 is HTTP 400, so never ask for it and treat it as the end | |
| `Feeds/Sources/PredbOrgSource.php:17-22,41-90` | Row shape, `values`, `q=*`, `limit` capped at 100. History only: its newest entries trail by a day | |
| `Feeds/Sources/XrelSource.php:19-21,80-160` | Scene and P2P row shapes, `list`, `per_page` 5–100, 900 requests per hour, size units. `:95-98`: a page past `pagination.total_pages` is an empty list, which ends the source | |
| `Feeds/PredbFeedImporter.php:126-210,542-575`, `BUGS.md` #34 | Dedupe by case-folded title; fill only empty fields; a later nuke updates; `source` is never relabelled | Search-index publishing, dry-run shadows |
| `Feeds/PredbFeedEntry.php:69-83` | The entry's fields | String sizes: Atlas stores bytes |
| `app/Console/Commands/PredbImportFeed.php:105-138,179-207` | Page until empty; a source failing doesn't stop the others | Retrying in a sleep loop |
| `app/Console/Commands/PredbImportFeed.php:229-300` | Wait from `Retry-After` in seconds or as an HTTP date, else epoch `X-RateLimit-Reset`, else the default; a server's delay is never shortened | |
| `app/Services/NameFixing/PredbIdentityKey.php:62-72` | `lowercase`, `[._\s]+` to `.`, as the lookup key | NFC: no ICU here, decision 9 |
| `app/Support/PredbSearchDocument.php:25-28` | Identity is lowercase equality | |
| `app/Services/Predb/PredbIdentityKeys.php:45-48` | Reject titles with control characters, `\`, `/` or `\|`, and over 255 characters; keys only nominate, they never establish identity | SHA-256 digests as text, policy hashes |
| `app/Services/NameFixing/PredbMatchSelector.php:12,98-145` | Acceptance permits only letter case; candidates from a filename must agree on one title | The similarity score: every acceptance here is literal |
| `app/Services/NameFixing/OriginalFilenameEvidence.php:139-171`, `NameFixingService.php:1668-1690` | A title plus only a known file suffix, checked by stripping the suffix | Unicode offset handling |
| `app/Services/NameFixing/ConstrainedPredbMatcher.php:233-240,270-297` | Refuse on candidate overflow; refuse short titles; refuse ambiguity | Levenshtein edits and scores: it only ever suggests (`:13`) |
| `app/Services/NameFixing/NameFixingService.php:92-94,1454-1533` | Late matching: for each title, search `name` and `searchname` with a bound, check literally, refuse 16 or more hits (`:1486`), require 15+ characters and `-GROUP` (`:1458-1459`) | Paging through the search backend |
| `app/Services/ReleaseCleaningService.php:107-130` | Subject-embedded titles need the same literal check | The per-header lookup: forward matching runs once a release settles |
| `app/Services/NameFixing/ReleaseUpdateService.php:1272-1277` | A season pack is not renamed to an episode | |
| `BUGS.md` #35, #46 | Machine names reaching a query language must be quoted | |
| `BUGS.md` #24, #26, #34, #39 | What an IRC scraper costs, decision 3 | |
| `database/migrations/2025_01_30_115835_drop_triggers.php:13-15` | Upstream dropped its `predb_hashes` triggers, so hash matching here is new, not ported | |

`rust/pipeline-ffi/src/predb.rs`, `predb_feeds.rs` and `irc.rs` are byte-parity ports behind a C interface. Nothing is lifted from them.

### What upstream reaches, and what Atlas ports

| Source | Reached by | Upstream | Atlas |
|---|---|---|---|
| `predb_club` | JSON `predb.club/api/v1/`, also WebSocket | Default | **First** |
| `predb_net` | JSON `api.predb.net`, 100 pages, also WebSocket | Default | **First** |
| `xrel` | JSON `api.xrel.to/v2/release/latest.json`, 900 requests/h | Default | **First** |
| `xrel_p2p` | JSON `api.xrel.to/v2/p2p/releases.json` | Default | **First** |
| `predb_org` | JSON `predb.org/api/search`, full history | Opt-in, history | **First, for history** |
| `predatabase` | JSON with an opaque cursor; 5 requests per run anonymously | Default | Later: its cursor differs, and the depth limit makes it weak without a key |
| `predb_me` | RSS | Default | Later: needs an XML reader |
| `srrdb` | JSON, first page only | Opt-in | No: terms say "Use but don't scrape" |
| `predb_ovh` | JSON, unverified against live responses | Opt-in | No: upstream never reached it |
| `apibay`, `knaben` | JSON, P2P or catalogue names | Opt-in | No: not PREs, more false names |
| `scnsrc_me`, `eztv` | RSS, spaced titles | Opt-in | No: titles are lossy |
| `nfodb`, `one_techno`, `one_gabba`, `ddlvalley`, `hdencode`, `hdencode_tv`, `mp3scene`, `gload`, `hdworld`, `scnlog`, `movieblog` | RSS catalogues | Opt-in | No: listing sites, not PREs |
| `nima4k`, `g4u`, `gabber`, `crowdnfo`, `seriousaboutsound` | HTML scraping | Opt-in | No: brittle |
| `nswdb`, `3dsdb`, `dats_pciso_1998`, `nzedb_archive`, `scene_archive_sql` | Manual dumps | Manual only | No download. `--predb-import` reads a local file |
| 9 IRC networks | IRC | Off except one, whose channel has gone quiet | No: decision 3 |

### How upstream guards against false positives, and what stands here

| Upstream guard | Here |
|---|---|
| Keys only nominate; a literal check accepts (`PredbIdentityKeys.php:11`, `PredbMatchSelector.php:12`) | `rows_for_key` nominates, `literally` and `identity_key` accept. A key collision can never rename |
| Overflow: too many candidates means no decision (`ConstrainedPredbMatcher.php:233`) | More than `MAX_ROWS_PER_KEY` (16) rows for a key: no match |
| Too little title to be sure (`:237`, `NameFixingService.php:1458`) | `is_full_title`: 15+ bytes and `-GROUP` after 3+ words, required for every tier except an exact name and a hash |
| Two different titles for one release: ambiguous (`:297`, `PredbMatchSelector.php`) | `unique`: more than one identity is no match, and later tiers do not get a say |
| Markers must agree (`:251-262`) | Literal equality implies it |
| Levenshtein tolerance (`:270`) | None |
| Only a known suffix may follow the title (`OriginalFilenameEvidence.php:139-171`) | `literally`, using Part 5's `strip_file_suffixes` |
| Season pack to episode (`ReleaseUpdateService.php:1272`) | `renames_pack_to_episode` in the step |
| Size checks in ranking (`ConstrainedPredbMatcher.php:613`) | `sizes_agree` within a factor of 3, for the folded and inner-file tiers |

## 4. Atlas changes

**`Cargo.toml`**: `md-5 = "0.10"`, `sha1 = "0.10"`. `Cargo.lock` gains `md-5`, and `sha1` is there already; commit it, because `just test` is `cargo test --locked`.

**`src/paths.rs`**: `pub fn predb_database() -> PathBuf { crate::predb::db::path_for(&database()) }`.

**`src/lib.rs`**: `pub mod predb;`.

**`src/db.rs:174-201`** (`wipe`): unchanged in code. It lists its files one by one, and `atlas.predb.db` is not on the list, by decision (decision 6). A test pins it.

**`src/app.rs:557`**: after a wipe, also print `kept atlas.predb.db, the PreDB titles. Delete it by hand to drop them` when that file exists.

**`src/compact.rs`, `src/convert.rs`**: unchanged. `compact::run` walks `store::shard_paths` (`src/store.rs:55`), and `check` and `refuse_what_wouldnt_be_copied` look only inside shards. Conversion only touches the main database and shards. So neither sees the new file, and a test for each pins that. The two shard tables go through Part 3's registry.

**`src/store.rs`**: unchanged. `database_bytes` and `wal_bytes` (`:61-68`) keep counting the main database and shards only, and the dashboard reports the PreDB file separately. `db::checkpointer` (`src/db.rs:97`) is not given this file: it stays on SQLite's default auto-checkpoint, because its writes are small.

**`.gitignore:50-51`**: `atlas.*.db` and `atlas.*.db-*` already cover it. An acceptance check confirms this.

**`src/postprocess/schema.rs`**: `SHARD_TABLES` gains `&crate::predb::step::MATCH, &crate::predb::step::INNER_KEY`.

**`src/postprocess/steps.rs`**: `all()` gains `Arc::new(predb::step::PredbMatch)`.

**`src/config.rs`**: `Config.predb: PredbConfig` (`:134`), default at `:169`, parsed in `from_value` (`:264`).

**`src/bg_indexer.rs`**: `Scheduler` gains `predb: OnceLock<Arc<predb::Service>>`. In `run_servers`, after Part 3's `Stage::start` (after `:751`), when `config.predb.enabled`: `Service::start(ServiceParams { fetch: predb::test_fetch(), .. })`, passing the stage if there is one, then `tasks.spawn(predb::run(service, move || s.stopping()))`. `run` is in `tasks`, so `run_servers` can't return, and the auto-compaction can't start (`:893-904`), until the late thread has been joined. `watch_config` (`:613`) treats `predb.enabled` changing like a server change (`:622-629`), and passes other reloads to `service.set_config(&config)`. `Scheduler::write_status` (`:444-452`) adds `predb` to `stats.json`. The test `scheduler()` (`:1053`) gets `predb: OnceLock::new()`.

**`src/stats_dashboard.rs`**: Part 3's `draw_postprocess` gains a PreDB block, reading only `stats.json`: titles, file size, late-queue behind, and per source last OK, last error and next poll.

**`src/main.rs`**: `--predb-import` dispatch at `:22-32`, help at `:15-17`.

**New `src/predb/`**: `mod.rs` (`Service`, `run`), `db.rs`, `keys.rs`, `import.rs`, `feeds.rs`, `poll.rs`, `matcher.rs`, `step.rs`, `late.rs`, `file_import.rs`. `keys.rs`, `import.rs`, `feeds.rs` and `matcher.rs` carry a GPLv3 attribution header naming the upstream files in section 3.

## 5. Schema and migration

### `atlas.predb.db`, next to the shards

```sql
create table if not exists titles (
    id          INTEGER PRIMARY KEY,
    title       TEXT NOT NULL,      -- as the first feed sent it, trimmed
    source      INTEGER NOT NULL,   -- first to store it; never changed (BUGS.md #34)
    category    TEXT,
    size_bytes  INTEGER,
    files       INTEGER,
    pre_at      INTEGER,            -- unix seconds
    nuked       INTEGER NOT NULL DEFAULT 0,   -- 0 no, 1 nuked, 2 unnuked, 3 modnuke, 4 renuke, 5 oldnuke
    nuke_reason TEXT,
    added_at    INTEGER NOT NULL
);
-- nominates only. 3 rows a title: the name key, the first 8 bytes of its MD5 and of its SHA-1
create table if not exists title_keys (
    key      INTEGER NOT NULL,
    title_id INTEGER NOT NULL,
    primary key (key, title_id)
) without rowid;
create table if not exists feeds (
    source       INTEGER PRIMARY KEY,   -- SourceId, never reused
    key          TEXT NOT NULL,
    next_poll_at INTEGER NOT NULL DEFAULT 0,
    failures     INTEGER NOT NULL DEFAULT 0,
    resume_page  INTEGER NOT NULL DEFAULT 0,   -- a gap to finish: continue from here. 0 = none
    gap_floor    INTEGER NOT NULL DEFAULT 0,   -- newest titles.id from before the gap opened: the gap ends at one of these
    history_page INTEGER NOT NULL DEFAULT 0,   -- next older page for history_pages
    history_read INTEGER NOT NULL DEFAULT 0,
    last_ok_at   INTEGER,
    last_error   TEXT,
    inserted     INTEGER NOT NULL DEFAULT 0,
    updated      INTEGER NOT NULL DEFAULT 0
);
create table if not exists meta (key TEXT PRIMARY KEY, value INTEGER);
insert or ignore into meta (key, value) values ('titles', 0), ('late_after_id', 0);
```

Opened by `db::create_at` (`src/db.rs:45`) and `pragma journal_mode = wal`, only when `predb.enabled` or by `--predb-import`.

**Size, measured on 1,000,000 synthetic titles** of realistic length (about 45–60 characters): 153 MB in all, `titles` 96.6 MB, `title_keys` 56.2 MB, so 153 B a title.

| Titles | File | `titles` | `title_keys` |
|---:|---:|---:|---:|
| 1M | 153 MB | 97 MB | 56 MB |
| 10M | about 1.5–2 GB | about 1 GB | about 0.6–1 GB, random keys fill pages less well at scale |
| Growth at 2,500–11,000 titles/day | 0.4–1.7 MB/day, under 620 MB/year | | |

A lookup (`rows_for_key`) took 17 µs measured warm at 1M. SQLite's depth grows by at most one level at 10M.

### Shards

```rust
pub const MATCH: ShardTable = ShardTable {
    name: "predb_match",
    create: "create table if not exists predb_match (
        release_id INTEGER PRIMARY KEY,
        title_id   INTEGER NOT NULL,    -- for reading only: the PreDB file can be deleted
        kind       INTEGER NOT NULL,    -- 1 exact, 2 separators folded, 3 hash, 4 inner or posted file
        category   TEXT,
        pre_at     INTEGER,
        nuked      INTEGER NOT NULL DEFAULT 0,
        matched_at INTEGER NOT NULL
    )",
    indexes: &[], release_key: Some("release_id"), added: &[],
};
/// Names inner files suggest, for releases with no match yet: how a late title finds them.
pub const INNER_KEY: ShardTable = ShardTable {
    name: "predb_inner_key",
    create: "create table if not exists predb_inner_key (
        key INTEGER NOT NULL, release_id INTEGER NOT NULL, primary key (key, release_id)
    ) without rowid",
    indexes: &[("predb_inner_key_release", "create index if not exists predb_inner_key_release on predb_inner_key(release_id)")],
    release_key: Some("release_id"), added: &[],
};
```

Statements in `step.rs`:

```sql
-- MATCH_UPSERT
insert or replace into predb_match (release_id, title_id, kind, category, pre_at, nuked, matched_at) values (?1, ?2, ?3, ?4, ?5, ?6, ?7)
-- KEYS_CLEAR
delete from predb_inner_key where release_id = ?1
-- KEY_ADD
insert or ignore into predb_inner_key (key, release_id) values (?1, ?2)
```

### Survival

| Path | What happens | Test |
|---|---|---|
| Open | `schema::migrate` creates both shard tables and their `gc_*` triggers. The PreDB file is made only when enabled, so an upgrade adds no file | `tests/pp_schema.rs` iterates the registry; `no_file_by_default` |
| `--compact` | Shard tables copied by name. `atlas.predb.db` is never opened, moved or counted | Part 3's registry test; `compaction_leaves_the_predb_file_alone` |
| `--convert` | Fresh shards get empty tables. The file is untouched | `conversion_leaves_the_predb_file_alone` |
| Wipe | Shard tables go with their shards. `atlas.predb.db` stays, and the wipe says so | `a_wipe_keeps_the_titles` |
| Delete a release | `gc_predb_*` triggers. `predb_inner_key` has its release index, so this is a seek | Part 3's `deleting_a_release_clears_it_from_every_registered_table` |

Part 3's rule is "never create a virtual table in a shard". This part creates none and only reads `releases_fts`.

## 6. Interfaces

### Provided (`src/predb/keys.rs`)

```rust
use md5::{Digest, Md5};
use sha1::Sha1;
fn key(d: &[u8]) -> i64 { i64::from_be_bytes(d[..8].try_into().unwrap()) }
pub fn md5_hex(t: &str) -> String;     // lowercase hex of the exact title bytes
pub fn sha1_hex(t: &str) -> String;
/// The lookup key of a name, whichever separators and case it uses.
pub fn name_key(name: &str) -> i64 { key(&Sha1::digest(identity_key(name).as_bytes())) }
pub fn title_keys(title: &str) -> [i64; 3] { [name_key(title), key(&Md5::digest(title.as_bytes())), key(&Sha1::digest(title.as_bytes()))] }
/// The lookup key of a name that is itself a 32 or 40 digit hex hash.
pub fn hash_key(name: &str) -> Option<i64> {
    if !matches!(name.len(), 32 | 40) || !name.bytes().all(|c| c.is_ascii_hexdigit()) { return None; }
    u64::from_str_radix(&name[..16], 16).ok().map(|v| v as i64)
}
pub fn valid_title(t: &str) -> bool {
    let n = t.chars().count();
    (8..=255).contains(&n) && t == t.trim() && t.chars().any(char::is_alphabetic)
        && !t.chars().any(|c| c.is_control() || matches!(c, '\\' | '/' | '|' | '"'))
}
```

`"` is refused because a title goes into an FTS5 phrase (`BUGS.md` #35).

### Provided (`src/predb/import.rs`)

```rust
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Entry { pub title: String, pub category: Option<String>, pub size_bytes: Option<i64>, pub files: Option<i64>,
                   pub pre_at: Option<i64>, pub nuked: i64, pub nuke_reason: Option<String> }
#[derive(Default, Debug, PartialEq, Clone, Copy)]
pub struct Counts {
    pub inserted: u64,
    pub updated: u64,
    pub skipped: u64,
    /// the lowest `titles.id` among these entries that were stored before this call
    pub min_known_id: Option<i64>,
}
/// One transaction. For each entry: trimmed; `valid_title` or skipped. Rows under `name_key` whose title is equal when
/// lowercased are this title. None: insert it and its three keys. Found: empty `category`, `size_bytes`, `files` and `pre_at`
/// are filled; a non-zero `nuked` replaces, with its reason when it has one; `source` and `title` never change.
/// `meta.titles` grows by the number inserted.
pub fn import(conn: &mut Connection, source: i64, entries: &[Entry], now: i64) -> rusqlite::Result<Counts>;
```

### Provided (`src/predb/feeds.rs`)

```rust
#[derive(Debug, PartialEq)] pub enum ParseError { RateLimited(String), Shape(String) }
pub struct FeedSpec {
    pub id: i64,                // SourceId, stored in titles.source and feeds.source. Never reused
    pub key: &'static str,      // the config name
    pub endpoint: &'static str,
    pub page_size_param: &'static str,
    pub min_page_size: u32, pub max_page_size: u32,
    pub extra_query: &'static str,
    pub max_page: Option<u32>,  // never ask past this
    pub min_delay_ms: u64,
    pub parse: fn(&serde_json::Value, page: u32) -> Result<Vec<Entry>, ParseError>,
}
pub const SOURCES: &[FeedSpec];
pub fn spec(key: &str) -> Option<&'static FeedSpec>;
pub fn page_url(spec: &FeedSpec, page: u32, page_size: u32) -> String;  // endpoint?extra&param=N&page=P, clamped
```

| `id` | `key` | `endpoint` | Size param, range | `extra_query` | `max_page` | `min_delay_ms` |
|---:|---|---|---|---|---:|---:|
| 1 | `predb_club` | `https://predb.club/api/v1/` | `count`, 1–100 | | | 1,000 |
| 2 | `predb_net` | `https://api.predb.net/` | `limit`, 1–100 | | 100 | 1,000 |
| 3 | `predb_org` | `https://predb.org/api/search` | `limit`, 1–100 | `q=*` | | 1,000 |
| 4 | `xrel` | `https://api.xrel.to/v2/release/latest.json` | `per_page`, 5–100 | | | 4,000 |
| 5 | `xrel_p2p` | `https://api.xrel.to/v2/p2p/releases.json` | `per_page`, 5–100 | | | 4,000 |
| 100 | (file) | `--predb-import` | | | | |

xREL's 4,000 ms keeps it at 900 requests an hour.

**Parsing.** A row with no usable title is skipped, not an error. Numbers may arrive as JSON strings. An empty string is `None`. A missing list is `ParseError::Shape`. `status: "error"` in the first two is `RateLimited` when the message contains "rate limit" in any case, otherwise `Shape`. For both xREL sources, before looking for the list: `pagination.total_pages` above 0 and `page` above it gives `Ok(vec![])`, whatever else the body holds. So a cursor past xREL's last page ends the source rather than failing for good.

| Source | List at | Title | Category | Size | Files | Pre time | Nuke |
|---|---|---|---|---|---|---|---|
| `predb_club` | `/data/rows` | `name` | `cat` | `size` MB | `files` | `preAt` | `nuke` object or null: `type` `unnuke`, `undelpre` 2, `modnuke` 3, `renuke` 4, `oldnuke` 5, else 1; `reason` |
| `predb_net` | `/data` | `release` | `section` | `size` MB | `files` | `pretime` | `status` 0 none; 2, 4 is 2; else 1; `reason` when non-zero |
| `predb_org` | `/values` | `rlsname` | `section` | `size` MB | `files` | `ctime` | 1 when `status` is 1 or `nukereason` is set; `nukereason` |
| `xrel` | `/list` | `dirname` | `ext_info.type` uppercased | `size.number` in `size.unit` KB, MB, GB, TB; another unit (`RAR`, `NFO`) is `None` | | `time` | `flags.nuke_rls` |
| `xrel_p2p` | `/list` | `dirname` | `category.meta_cat` uppercased, `-sub_cat` when set | `size_mb` MB | | `pub_time` | |

An MB is 1,048,576 bytes.

### Provided (`src/predb/poll.rs`)

```rust
pub struct HttpReply { pub status: u16, pub retry_after: Option<String>, pub rate_reset: Option<String>, pub body: String }
pub trait Fetch: Send + Sync { fn get(&self, url: &str) -> Result<HttpReply, String>; }
pub struct UreqFetch { /* ureq::Agent */ }
impl UreqFetch { pub fn new(timeout: Duration) -> UreqFetch; }
/// Counts `UreqFetch::get` calls. Every test but the one loopback test asserts this stays 0.
#[doc(hidden)] pub static REAL_REQUESTS: AtomicUsize;
#[derive(Debug, PartialEq)] pub enum PollEnd { CaughtUp, Gap, Failed(String), RateLimited(i64), Stopped }
/// `pause` naps between requests and returns false when stopping. Writes `feeds` before returning.
pub fn poll_source(conn: &mut Connection, fetch: &dyn Fetch, spec: &FeedSpec, cfg: &PredbConfig, now: i64, pause: &dyn Fn(Duration) -> bool) -> rusqlite::Result<PollEnd>;
pub fn wait_seconds(reply: &HttpReply, now: i64, default: i64) -> i64;
```

`UreqFetch::new`: `ureq::Agent::config_builder().timeout_global(Some(timeout)).http_status_as_error(false).max_redirects(3).user_agent(concat!("Atlas/", env!("CARGO_PKG_VERSION"))).build().into()`. `get` reads the body with `resp.body_mut().with_config().limit(MAX_BODY).read_to_string()`, where `MAX_BODY = 8 << 20`. Over the limit is an `Err`.

**A poll, exactly.** `budget = max_pages_per_poll`. Requests are `max(request_delay_ms, spec.min_delay_ms)` apart. Each page is imported in its own transaction, so a failure keeps what was read.

`floor = select coalesce(max(id), 0) from titles`, read before the first request.

1. **Forward.** `page = 1`. While budget is left: fetch, parse, import, spend 1. An empty list, or `page > max_page`: the source has ended, so `ended = true` and stop. `inserted == 0`: `caught = true` and stop. Else `page += 1`.
2. **The gap.** If it stopped on budget: `resume_page = page`, and end with `Gap`. The exception is a source that has never succeeded (`last_ok_at` NULL): older pages are history rather than a gap, so `resume_page = 0`, and end with `CaughtUp`. `gap_floor = floor`, unless a gap is already open, whose older floor stays. There is one cursor, so a new gap moves it up to `page`, and walking down from there passes through what the older gap had left, until a title from before the older floor.
3. If `caught` and `resume_page > 0`: go on from `page = resume_page`, fetching, parsing, importing and spending 1 each. The gap is closed, `resume_page = 0`, only by an empty list, `page > max_page`, or a page with `inserted == 0` and `min_known_id <= gap_floor`, which is a title from before the gap. Any other page, including one holding only titles imported since the gap opened, is `page += 1`. Running out of budget saves the new page. Newer titles push old ones to higher page numbers, so resuming can re-read titles but never skips any, however far they were pushed.
4. **History.** If not stopped on budget and `history_read < cfg.history_pages`: `page = max(history_page, 2)`. Fetch, parse, import, with no stopping on `inserted == 0`. Each page saves `history_page = page + 1` and `history_read + 1`. An empty list or `page > max_page` sets `history_read = cfg.history_pages`.
5. **Success:** `failures = 0`, `last_ok_at = now`, `last_error = NULL`, `next_poll_at = now + poll_secs + id * 7`, so sources don't fire together. `CaughtUp`, or `Gap` as above.

| A reply | Meaning | `next_poll_at` |
|---|---|---|
| Transport error, 5xx, other non-200, bad JSON, `ParseError::Shape` | `Failed`, `failures + 1` | `now + min(poll_secs * 2^min(failures, 16), 21,600)`, so 10 m, 20 m, up to 6 h |
| 429, or `ParseError::RateLimited` | `RateLimited(wait)`, `failures` unchanged | `now + wait_seconds(reply, now, 60)` |

`wait_seconds`: `Retry-After` as digits; else as an RFC 2822 date, by `chrono`; else `X-RateLimit-Reset` as epoch seconds; else the default. At least 1 and at most 604,800. A server's delay is never shortened.

### Provided (`src/predb/matcher.rs`)

```rust
static FULL_TITLE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"([\w()]+[\s._-]){2,}[\w()]+-\w+").unwrap());
pub const MAX_ROWS_PER_KEY: usize = 16;
pub const MAX_NAMES: usize = 16;
#[derive(Clone, Copy, PartialEq, Eq, Debug)] pub enum Kind { Exact = 1, Normalized = 2, Hash = 3, InnerFile = 4 }
#[derive(Clone, Debug)] pub struct TitleRow { pub id: i64, pub title: String, pub category: Option<String>, pub size_bytes: Option<i64>, pub pre_at: Option<i64>, pub nuked: i64 }
#[derive(Clone, Debug)] pub struct Match { pub title: TitleRow, pub kind: Kind }
pub struct Evidence<'a> { pub name: &'a str, pub display_name: Option<&'a str>, pub size: i64, pub files: Vec<&'a str>, pub inner: Vec<&'a str> }

pub fn rows_for_key(conn: &Connection, key: i64) -> rusqlite::Result<Vec<TitleRow>> {
    // select t.id, t.title, t.category, t.size_bytes, t.pre_at, t.nuked
    // from title_keys k join titles t on t.id = k.title_id where k.key = ?1 order by t.id limit 17
}
/// Strong enough that folding separators can't be a coincidence: 15+ bytes, 3+ words and a -GROUP.
pub fn is_full_title(title: &str) -> bool { title.len() >= 15 && FULL_TITLE.is_match(title) }
/// `candidate` is `title` plus only a file suffix, letter case aside.
pub fn literally(candidate: &str, title: &str) -> bool {
    let (c, t) = (candidate.to_lowercase(), title.to_lowercase());
    if c == t { return true; }
    match c.strip_prefix(t.as_str()) {
        Some(rest) if rest.starts_with('.') && rest.is_ascii() => strip_file_suffixes(&format!("title{rest}")) == "title",
        _ => false,
    }
}
pub fn sizes_agree(title: Option<i64>, release: i64) -> bool {
    match title { Some(t) if t > 0 && release > 0 => release.saturating_mul(3) >= t && t.saturating_mul(3) >= release, _ => true }
}
/// One identity or nothing. Err: two different titles.
fn unique(mut found: Vec<TitleRow>) -> Result<Option<TitleRow>, ()> {
    found.sort_by_key(|t| t.id);
    found.dedup_by_key(|t| t.id);
    match found.len() {
        0 => Ok(None),
        1 => Ok(found.pop()),
        _ => {
            let first = found[0].title.to_lowercase();
            if found.iter().all(|t| t.title.to_lowercase() == first) { Ok(found.into_iter().next()) } else { Err(()) }
        }
    }
}
pub fn find(conn: &Connection, e: &Evidence<'_>) -> rusqlite::Result<Option<Match>> {
    // hash-named posts: the full digest has to agree, a key only nominates
    for raw in std::iter::once(e.name).chain(e.files.iter().copied()).take(MAX_NAMES) {
        // cleaned, so `<hash>.mkv` and Part 2's `<hash> [12345678]` count
        let Some(name) = clean_candidate(raw) else { continue };
        let Some(key) = hash_key(&name) else { continue };
        let want = name.to_ascii_lowercase();
        let hits = rows_for_key(conn, key)?.into_iter().filter(|t| md5_hex(&t.title) == want || sha1_hex(&t.title) == want).collect();
        match unique(hits) { Ok(Some(t)) => return Ok(Some(Match { title: t, kind: Kind::Hash })), Ok(None) => {} Err(()) => return Ok(None) }
    }
    // the release's own names
    for name in e.display_name.into_iter().chain([e.name]) {
        let clean = clean_candidate(name).unwrap_or_else(|| name.to_string());
        let rows = rows_for_key(conn, name_key(&clean))?;
        if rows.len() > MAX_ROWS_PER_KEY { return Ok(None); }
        let exact = rows.iter().filter(|t| literally(name, &t.title) || literally(&clean, &t.title)).cloned().collect();
        match unique(exact) { Ok(Some(t)) => return Ok(Some(Match { title: t, kind: Kind::Exact })), Ok(None) => {} Err(()) => return Ok(None) }
        let folded = rows.into_iter().filter(|t| is_full_title(&t.title) && sizes_agree(t.size_bytes, e.size) && identity_key(&clean) == identity_key(&t.title)).collect();
        match unique(folded) { Ok(Some(t)) => return Ok(Some(Match { title: t, kind: Kind::Normalized })), Ok(None) => {} Err(()) => return Ok(None) }
    }
    // inner and posted files: all of them have to point at one title
    let many_episodes = distinct_episodes(e.inner.iter().copied().map(basename)) >= 2;
    let (mut found, mut tried) = (Vec::new(), HashSet::new());
    for path in e.inner.iter().chain(e.files.iter()).copied() {
        if tried.len() >= MAX_NAMES { break; }
        for name in clean_candidate(path).into_iter().chain(parent_dir(path).and_then(|p| clean_candidate(&p))) {
            let key = name_key(&name);
            if !tried.insert(key) { continue; }
            for t in rows_for_key(conn, key)? {
                let same = literally(&name, &t.title) || identity_key(&name) == identity_key(&t.title);
                if same && is_full_title(&t.title) && sizes_agree(t.size_bytes, e.size) && !(many_episodes && is_episode_name(&t.title)) { found.push(t); }
            }
        }
    }
    Ok(unique(found).ok().flatten().map(|t| Match { title: t, kind: Kind::InnerFile }))
}
```

### Provided (`src/predb/step.rs`)

| Step | Id | Rank | Needs | `reruns` |
|---|---:|---:|---|---|
| `predb_match` | 16 | 300 | Database | rename, growth |

Ids 17–23 and ranks 310–399 stay free. Part 5's `naming` already sets `rerun: mask(StepId(16))` when inner files exist.

```rust
pub const PREDB_MATCH: StepId = StepId(16);
/// Parts 7 and 8: ids 24..=39. A match that keeps the same name is not a rename, and they still want to look again.
const LATER_PARTS: StepMask = 0xFFFF_i64 << 24;
pub struct PredbMatch;
impl Step for PredbMatch {
    fn id(&self) -> StepId { PREDB_MATCH }
    fn name(&self) -> &'static str { "predb_match" }
    fn rank(&self) -> u16 { 300 }
    fn needs(&self) -> Needs { Needs::Database }
    fn enabled(&self, cfg: &Config) -> bool { cfg.predb.enabled }
    fn applies(&self, r: &ReleaseInfo) -> bool { worth_looking_up(r) }
    fn reruns(&self) -> Reruns { Reruns { on_rename: true, on_growth: true } }
    fn run(&self, ctx: &mut StepCtx<'_>) -> Result<Outcome, StepError>;
}
/// What no title could equal, without reading anything. Part 3's `judge` no longer rejects by size or image type, so this
/// is what keeps pictures and single-article junk from costing lookups. A title names a release directory, so a name with an
/// image extension is out. With one article there is no other file to look at, so a random name is out too, unless it is a hash.
pub fn worth_looking_up(r: &ReleaseInfo) -> bool {
    if r.name_source.is_some_and(|s| s >= NameSource::PreDb as i64) { return false; }
    let name = current_name(r);
    if is_image_name(name) { return false; }
    if r.parts > 1 { return true; }
    if clean_candidate(name).as_deref().and_then(hash_key).is_some() { return true; }
    judge(name) != Grade::Junk
}
/// `…/atlas.s3.db` -> `…/atlas.predb.db`. `StepCtx` has no main path, and `ctx.conn().path()` is the shard's.
pub fn db_path_from_shard(shard: &Path) -> Option<PathBuf>;
```

`run`:

1. `files = ctx.files()`, `inner = archive::store::inner_files(ctx.conn(), id)`. A `rusqlite` error is `StepError::Retry`.
2. `with_reader(db_path_from_shard(ctx.conn().path()), |c| find(c, &evidence))`. `with_reader` keeps one `query_only` connection per thread in a `thread_local!`, keyed by path. A missing or unreadable file is `Retry`.
3. A match where `renames_pack_to_episode(current_name(r), title)` is false:
   - `name: Some(NameChange { display_name: Some(title), obfuscated: Some(false), source: NameSource::PreDb.value() })`. This goes around `naming::improves`: a title outranks every other name.
   - `writes`: `MATCH_UPSERT`, then `KEYS_CLEAR`.
   - `facts`: `("predb.category", category)` when set, `("predb.title", title)`.
   - `rerun`: `LATER_PARTS` unless `select title_id from predb_match where release_id = ?` already gives this id.
   - `matched: true`.
4. Otherwise `matched: false`. With inner files: `KEYS_CLEAR`, then `KEY_ADD` for `name_key` of the first 4 from `naming::best_from_paths(inner.iter().map(|f| (f.name.as_str(), f.size.unwrap_or(1).max(1) as u64)), Grade::Weak)`.

### Provided (`src/predb/late.rs`)

The late path only nominates. `Stage::rerun` sends a release back through `predb_match`, which decides by the same rules as ever, so it needs no write access and no second copy of the rules.

```rust
pub const LATE_DELAY_SECS: i64 = 600;
pub const LATE_FTS_LIMIT: i64 = 32;
pub const LATE_MAX_RERUNS: usize = 16;
pub const LATE_TITLE_BUDGET: Duration = Duration::from_secs(2);
#[derive(Default, Debug, PartialEq)] pub struct Nominees { pub ids: Vec<i64>, pub overflow: bool, pub out_of_budget: bool, pub stopped: bool }
/// `shards`: read-only connections, in shard order. `stop` is checked between shards and between statements.
pub fn nominate(shards: &[Connection], title: &str, clock: &dyn Fn() -> Instant, stop: &AtomicBool) -> rusqlite::Result<Nominees>;
/// Up to `budget` titles past the cursor and at least LATE_DELAY_SECS old. Returns how many were read.
pub fn tick(predb: &Connection, shards: &[Connection], now: i64, budget: i64, stop: &AtomicBool, rerun: &dyn Fn(i64) -> anyhow::Result<()>) -> anyhow::Result<usize>;
/// A name that can be this title: the title plus a file suffix, or the same once cleaned, letter case and separators aside.
fn accepts(row_name: &str, title: &str) -> bool {
    literally(row_name, title) || clean_candidate(row_name).is_some_and(|c| identity_key(&c) == identity_key(title))
}
fn accepts_hash(row_name: &str, md5: &str, sha1: &str) -> bool {
    clean_candidate(row_name).is_some_and(|c| { let c = c.to_ascii_lowercase(); c == md5 || c == sha1 })
}
```

`nominate`, per shard, in this order:

```sql
-- the title itself, on idx_release_unique's first column
select id, name_source from releases where name = ?1 limit 17;
-- MD5, then SHA-1, each one quoted FTS token: case-folded, and it finds `<hash>.mkv` and `<hash> [12345678]`.
-- The same statement carries the title as one quoted phrase, when is_full_title(title) and the title's budget isn't
-- spent. FTS5 can't be interrupted inside a query, so the budget is checked between shards: the worst case is
-- LATE_TITLE_BUDGET plus one query
select rowid from releases_fts where releases_fts match ?1 order by rowid desc limit 32;
select name, display_name, name_source from releases where id = ?1;   -- each hit
-- inner files, on the primary key
select release_id from predb_inner_key where key = ?1 limit 17;
```

- `valid_title` has refused any `"`, so a title is always one FTS5 phrase, and `is_full_title` means 3+ words.
- A phrase hit counts when `accepts` holds for its `name` or `display_name`. A hash hit counts when `accepts_hash` holds for its `name`. Otherwise it is dropped, so `T.Extra` shares a phrase and is not nominated.
- A row with `name_source >= 100` is dropped.
- More than `LATE_MAX_RERUNS` left: `overflow`, no reruns. Upstream refuses the same (`NameFixingService.php:1486`), since a title on that many releases is too generic to trust by this route.

**What each route finds, after Part 2.** Part 2 sets `name` to the shortest parsed file name in the first slice, with ` [12345678]` when that name is taken. `T` is a full title.

| A row with | Found late by |
|---|---|
| `name` = `T` | Index |
| `name` = `T.mkv`, `t.mkv`, `T_with_other_separators` | Phrase, then `accepts` |
| `name` = `T [12345678]`, `T.mkv [12345678]` | Phrase, then `accepts`, because Part 5's `clean_candidate` strips the suffix |
| `display_name` = `T` on any `name` | Phrase |
| `name` = `<md5>`, `<sha1>`, in either case, with an extension or the suffix | Hash token |
| An inner file or directory among the release's first 4 good names | `predb_inner_key` |

**What it still misses.** These are all false negatives: the release keeps the name it had, and nothing wrong is written. None has been measured, since that needs live data. `a_title_only_in_a_later_file_is_a_known_miss` pins the second.

1. **A short title under any form but an exact `name`.** Under 15 bytes, under 3 words or with no `-GROUP`, the phrase is skipped on purpose: `Foo.Bar-GRP.mkv` and `Foo.Bar-GRP [12345678]` aren't found. This trades recall for safety and bounded cost.
2. **A title only in a file that isn't the row's `name`.** This is what Part 2 makes more of: a merged post named from `grp-t.mkv`, or from a shorter file in the first slice, with `T.nfo` beside it. FTS doesn't index `files` (Part 2 §2). If Part 5's posted-file tier adopted `T` as `display_name`, the phrase finds it. If `T` graded no better than the name shown, it didn't, and nothing here finds it.
3. **An inner name past the first 4 kept, or a release Part 4 hasn't read.** The second is caught when Part 4 reads it, since `naming` then asks for this step again.
4. **More hits than the bounds allow.** A real match older than the 32 newest phrase hits on its shard; a title on more than 16 releases; a title out of its 2 s budget, which skips the phrase on its remaining shards. All are counted in `stats.json`.
5. **Rows `worth_looking_up` skips, and rows Part 3 rejects.** By design.
6. **Differences the rules don't fold.** Composed against decomposed Unicode (decision 9), or a title in a release's subject and nowhere else.

For 2, 3 and 4 the forward step still matches when the release next runs: a late article, or `atlas --reprocess --step predb_match --apply`.

`tick` reads, then advances the cursor, committing after each title:

```sql
select id, title from titles where id > (select value from meta where key = 'late_after_id') and added_at <= ?1 order by id limit ?2;
update meta set value = ?1 where key = 'late_after_id';
```

`LATE_DELAY_SECS` closes a race. If the step read `titles` just before a title landed, then its `predb_inner_key` row commits shortly after, and a title probing at once would miss that row. The step's deadline is 120 s, so 600 s covers it. A failed `rerun`, or `stopped`, returns before moving the cursor.

**Stopping.** The thread owns its 8 shard connections. No statement stays open between queries, so it holds no read snapshot while it naps. `stop` is checked before each title and each statement, so stopping waits for at most one FTS query, about 0.5 s at 15M rows a shard. The connections drop when the thread returns. `compact_shard` starts with a truncating checkpoint (`src/compact.rs:57-64,416`), which fails while a reader holds a snapshot, so `stop_and_join` has to finish first, and `run` calling it inside `tasks` is what guarantees that.

### Provided (`src/predb/mod.rs`)

```rust
pub struct ServiceParams { pub main: PathBuf, pub config: PredbConfig, pub stage: Option<Arc<Stage>>, pub fetch: Option<Arc<dyn Fetch>> }
static TEST_FETCH: Mutex<Option<Arc<dyn Fetch>>> = Mutex::new(None);
/// For a test that runs the real indexer: `run_servers` builds the service, so this is the only way in. Mirrors Part 3's
/// `set_test_steps`. Install it before `run_until`.
#[doc(hidden)] pub fn set_test_fetch(fetch: Arc<dyn Fetch>) { *TEST_FETCH.lock().unwrap() = Some(fetch); }
pub(crate) fn test_fetch() -> Option<Arc<dyn Fetch>> { TEST_FETCH.lock().unwrap().clone() }
impl Service {
    /// Makes the file. Threads `atlas-predb-feed`, and `atlas-predb-late` when there is a stage and `late_per_second > 0`.
    pub fn start(p: ServiceParams) -> anyhow::Result<Arc<Service>>;
    pub fn set_config(&self, full: &Config);
    pub fn snapshot(&self) -> serde_json::Value;
    /// Sets the stop flag and joins both threads. The feed thread may finish one HTTP request first.
    pub fn stop_and_join(&self);
}
/// Naps 250 ms until `stopping`, then `stop_and_join` through `spawn_blocking`.
pub async fn run(service: Arc<Service>, stopping: impl Fn() -> bool + Send + 'static);
```

The feed thread naps in 1 s slices and polls each configured source whose `next_poll_at` has come. A key in `sources` that `feeds::spec` doesn't know is logged once and ignored.

### Provided (`src/predb/db.rs`, `file_import.rs`)

```rust
pub fn path_for(main: &Path) -> PathBuf { crate::compact::with_suffix(main, "predb") }   // atlas.db -> atlas.predb.db
pub fn create(path: &Path) -> rusqlite::Result<Connection>;
pub fn open_reader(path: &Path) -> rusqlite::Result<Connection>;   // db::open_at, then `pragma query_only = on`
/// `atlas --predb-import FILE [--apply]`. A local text file, gzip by its `.gz` suffix: one `title`, or
/// `title<TAB>category<TAB>pre_at`, per line. Exit 0 on success, 1 when refused or failed.
pub fn run(args: &[String]) -> i32;
```

Reading is bounded before anything is decoded, because a small `.gz` can expand to anything, and a line with no newline would otherwise grow one buffer without limit. `MAX_IMPORT_LINE = 1024`, `MAX_IMPORT_LINES = 50_000_000`, `MAX_IMPORT_BYTES = 16 << 30` of decompressed text. A longer line is skipped and counted. Past a line or byte cap the run stops with an error and exit 1, keeping the batches it committed. Test env `ATLAS_PREDB_IMPORT_MAX_LINES` and `ATLAS_PREDB_IMPORT_MAX_BYTES` lower the two totals.

```rust
let mut input = reader.take(max_bytes);
let mut buf = Vec::with_capacity(MAX_IMPORT_LINE as usize + 1);
loop {
    buf.clear();
    if (&mut input).take(MAX_IMPORT_LINE + 1).read_until(b'\n', &mut buf)? == 0 {
        return Ok(if input.limit() == 0 { Stop::TooManyBytes } else { Stop::End });
    }
    if lines == max_lines { return Ok(Stop::TooManyLines); }
    lines += 1;
    if buf.last() != Some(&b'\n') && buf.len() as u64 > MAX_IMPORT_LINE {
        too_long += 1;
        loop {   // drain the rest of this line, a piece at a time
            buf.clear();
            let n = (&mut input).take(MAX_IMPORT_LINE + 1).read_until(b'\n', &mut buf)?;
            if n == 0 || buf.last() == Some(&b'\n') { break; }
        }
        continue;
    }
    // str::from_utf8, else counted invalid; then split on tab, valid_title, into the batch
}
```

Measured: a 198 KB gzip holding a 200 MB line with no newline, then two good lines: 1 skipped, 2 read, 96 KB of memory growth.

### What Parts 7 and 8 can rely on

- **A rename:** set `Reruns { on_rename: true, .. }`. A PreDB rename clears their done bit like any other, and `ctx.release()` shows the title in the same run.
- **A match with no rename,** where the title equals the name already shown: `predb_match` clears bits 24–39 itself, the first time it matches that title.
- **In the same run:** `ctx.fact("predb.category")` and `ctx.fact("predb.title")`. **Later:** `select kind, category, pre_at, nuked from predb_match where release_id = ?` through `ctx.conn()`.
- `ctx.release().name_source == Some(100)` means the name is an exact catalogue title.
- A late title arrives through the same step, so they need nothing extra for it.

### Consumed

`Step`, `StepCtx`, `Outcome`, `StepWrite`, `NameChange`, `ShardTable`, `Stage::rerun`, `StepMask`, `mask`, `ReleaseInfo` (Part 3). `archive::store::inner_files` (Part 4). `naming::{NameSource, Grade, judge, identity_key, clean_candidate, parent_dir, basename, strip_file_suffixes, is_episode_name, is_image_name, distinct_episodes, renames_pack_to_episode, best_from_paths, current_name}` (Part 5). `Connection::path` from `rusqlite` 0.40. `compact::{with_suffix, hold_off_compaction}`, `procs::indexer_alive`, `store::shard_paths`, `db::{create_at, open_at}`.

## 7. Config

A top-level `predb` object, parsed into `Config.predb: PredbConfig`. Absent keys take these defaults, no key is written back, and a bad value falls back as `from_value` does elsewhere (`src/config.rs:631`).

| Key | Default | Meaning |
|---|---|---|
| `enabled` | `false` | Make the file, run the service and the `predb_match` step. Off: no file, no network, no step |
| `sources` | `[]` | From `predb_club`, `predb_net`, `predb_org`, `xrel`, `xrel_p2p`. Empty with `enabled: true`: matches against what is in the file, with no network |
| `poll_minutes` | `5` | Between polls of a source. Floor 1 |
| `page_size` | `100` | Clamped to each source's range |
| `max_pages_per_poll` | `3` | Requests per source per poll. Clamped to 1–20 |
| `request_delay_ms` | `1000` | Between requests to one source. Floor 250, and a source's own floor applies |
| `timeout_secs` | `15` | Whole request. Clamped to 1–120 |
| `history_pages` | `0` | Older pages to read in total per source, beyond catching up. 0: none |
| `late_per_second` | `1` | Titles the late path takes per second. 0: off |

Turning it on, with `sources: ["predb_club", "predb_net"]`: the next rebuild makes the file and starts polling, and Part 3 starts a `steps` walk, because a step turned on. No key is added to a config that lacks them.

## 8. Tasks

Each task: write the test first and see it fail for the stated reason, implement, run, commit. Append whatever attribution the executing session requires to commit messages. All tests use `tempfile::tempdir()`. Every title in a fixture is invented.

**No test opens a socket to anything but loopback.** `predb_file`, `predb_poll`, `predb_stage`, `predb_late`, `predb_bg`, `predb_bg_default` and `predb_import` use no real fetcher. Each in-process one ends with `assert_eq!(REAL_REQUESTS.load(..), 0)`. The only `UreqFetch` in the suite is `ureq_fetch_reports_status_and_headers`, in its own binary, `tests/predb_ureq.rs`, against a `tiny_http` server on `127.0.0.1`.

**`Scripted`** lives in `tests/common/mod.rs`, so `predb_poll` and `predb_bg` can both use it. `tests/common` is `#![allow(dead_code)]` already:

```rust
/// A `Fetch` that opens no socket: replies are queued per URL, every URL is recorded, and a URL with no reply left is an `Err`.
pub struct Scripted { pub urls: Mutex<Vec<String>>, replies: Mutex<HashMap<String, VecDeque<HttpReply>>> }
impl Scripted { pub fn new() -> Arc<Scripted>; pub fn reply(&self, url: &str, status: u16, body: &str); pub fn reply_with(&self, url: &str, reply: HttpReply); }
impl Fetch for Scripted { fn get(&self, url: &str) -> Result<HttpReply, String>; }
```

### Task 1: The PreDB file

**Files:** create `src/predb/mod.rs`, `src/predb/db.rs`; modify `src/lib.rs`, `src/paths.rs`, `src/app.rs:557`, `src/config.rs`, `config.example.json`; test `tests/predb_file.rs`.

- [ ] **Tests first:**
  - `the_path_sits_next_to_the_shards`: `path_for("/x/atlas.db")` is `/x/atlas.predb.db`. `paths::predb_database()` under `ATLAS_HOME`.
  - `create_is_idempotent`: twice, with a title between: the row survives. WAL mode is on.
  - `no_file_by_default`: `db::create_db_at` and a default `Config`: `atlas.predb.db` does not exist.
  - `a_wipe_keeps_the_titles`: `db::wipe`, and the file's bytes are unchanged, while every shard is gone.
  - `compaction_leaves_the_predb_file_alone`: `compact::run` on `legacy_fixture`-style shards: same bytes and mtime, no `.compact.` or `.precompact.` copy of it.
  - `conversion_leaves_the_predb_file_alone`: `convert` on the `old_database` fixture of `tests/convert.rs`: same bytes.
  - `predb_defaults_when_absent`, `predb_keys_parse`, `a_bad_predb_value_falls_back`, `values_are_clamped`.
- [ ] **Run:** `cargo test --test predb_file`. **Expected before:** does not compile.
- [ ] **Implement** section 4 and the schema.
- [ ] **Run:** `cargo test --test predb_file && cargo test --lib config && git check-ignore atlas.predb.db`. **Expected:** pass; prints `atlas.predb.db`.
- [ ] **Commit:** `Add the PreDB file next to the shards`

### Task 2: Keys and import

**Files:** create `src/predb/keys.rs`, `src/predb/import.rs`; modify `Cargo.toml`, `Cargo.lock`.

- [ ] **Tests first:**
  - `keys_table`: `name_key` agrees for `Foo.Bar-GRP`, `foo bar-grp`, `FOO_BAR-GRP`. `md5_hex("abc")` is `900150983cd24fb0d6963f7d28e17f72`, `sha1_hex("abc")` is `a9993e364706816aba3e25717850c26c9cd0d89d`. `hash_key` of a 32 and a 40 digit hex name equals the matching element of `title_keys`, and is `None` for `zz` or 31 digits.
  - `valid_title_table`: `Good.Title-GRP` yes; `short`, `bad/slash.title-GRP`, `quo"te.title-GRP`, `12345678`, a control character, ` padded.title-GRP `, 256 characters: no.
  - `import_dedupes_and_skips`: an entry twice, plus `short` and `bad/slash.title-GRP`, plus another: `Counts { inserted: 2, updated: 0, skipped: 3, min_known_id: None }`. `meta.titles` is 2. Three `title_keys` rows each.
  - `only_empty_fields_fill`: the same title lowercased, source 2, with category and size: `updated: 1`. `source` and `title` keep their first values. A later different category changes nothing.
  - `a_nuke_updates`: `nuked: 1, reason`: both stored; category kept. A later `nuked: 0` does not clear it.
  - `the_same_page_again_changes_nothing`: all `skipped`.
- [ ] **Run:** `cargo test --lib predb`. **Expected before:** does not compile.
- [ ] **Implement** section 6.
- [ ] **Run:** `cargo test --lib predb && just lint`. **Expected:** pass.
- [ ] **Commit:** `Store PreDB titles with lookup keys, deduped, and never relabel a source`

### Task 3: Feed parsers on recorded fixtures

**Files:** create `src/predb/feeds.rs`, `tests/fixtures/predb/{predb_club,predb_net,predb_org,xrel,xrel_p2p}.json`, `tests/fixtures/predb/{rate_limited,no_list}.json`.

Hand-write each fixture from section 6's table, with invented titles such as `Alpha.Beta.Gamma.2024.1080p.WEB-DL.H.264-TESTGRP`. Include in each: a normal row, a nuked row where the source has nukes, a row with an empty title, a row missing the title key, null sizes, numbers as strings.

- [ ] **Tests first**, each with `include_str!`:
  - One per source, on every column of the table. `predb_club`: `size: 12.5` gives 13,107,200; `modnuke` gives 3; an empty `cat` gives `None`. `predb_net`: `status` 3 gives 1 and 2 gives 2, `size: "100"` gives 104,857,600. `predb_org`: `status: "1"` with no reason gives 1, a reason alone gives 1. `xrel`: `{number: 700, unit: "MB"}` gives 734,003,200, `{number: 1, unit: "RAR"}` gives `None`, type `movie` gives `MOVIE`. `xrel_p2p`: `MOVIES-hd`, `size_mb: 2` gives 2,097,152.
  - `rate_limited.json` through `parse_predb_club` and `parse_predb_net`: `ParseError::RateLimited`. `no_list.json`: `ParseError::Shape` for all five.
  - `xrel_stops_at_total_pages`, for both xREL sources: page 4 of `{"pagination": {"total_pages": 3}}` gives an empty list, and so does page 4 of a body that has rows. Page 3 gives its rows. Page 2 with no list is still `ParseError::Shape`: only a page past the end is an end.
  - `page_urls_table`: `page_url(spec("predb_club"), 3, 500)` is `https://predb.club/api/v1/?count=100&page=3`; `predb_org` has `?q=*&limit=100&page=1`; `xrel` with size 2 clamps to `per_page=5`.
- [ ] **Run:** `cargo test --lib feeds`. **Expected before:** does not compile.
- [ ] **Implement** section 6.
- [ ] **Run:** `cargo test --lib feeds`. **Expected:** pass.
- [ ] **Commit:** `Parse the five JSON PreDB feeds`

### Task 4: Polling, backoff and cursors

**Files:** create `src/predb/poll.rs`; modify `tests/common/mod.rs`; tests `tests/predb_poll.rs`, on `Scripted`, and `tests/predb_ureq.rs`. `pause` records its durations and returns true.

- [ ] **Tests first:**
  - `a_first_poll_reads_its_budget_and_leaves_no_gap`: 3 pages all new, `max_pages_per_poll: 3`: 3 requests, `CaughtUp`, `resume_page = 0`.
  - `known_titles_stop_a_poll`: page 1 new, page 2 known: 2 requests, `CaughtUp`.
  - `a_gap_resumes_from_its_page`: after a success, 3 pages all new: `resume_page = 4`, `Gap`. Next poll: page 1 known, then page 4. A page 4 holding a title from before the gap: `resume_page = 0`. A restart, by a new `Connection`, between polls: same.
  - `the_gap_survives_new_titles_pushing_pages_down`: the mock shifts its pages by 10 titles between polls: every title stored, none lost.
  - `a_full_page_shift_doesnt_close_the_gap`: pages 1–3 imported with cursor 4, then a whole page of new titles, so page 4 holds only what the last poll imported: it doesn't close the gap, and the former pages 4 onward are all stored, with `history_pages: 0`. `resume_page` reaches 0 only at a title from before the gap. `a_second_gap_keeps_the_older_floor`: a new gap while one is open: `gap_floor` unchanged, and every title of both is stored.
  - `predb_net_never_asks_past_page_100`: `resume_page = 100` and all new: page 100 read, no request for 101, `resume_page = 0`. `BUGS.md` #25.
  - `an_empty_page_ends_it`: `resume_page = 0`.
  - `an_xrel_cursor_past_its_last_page_ends_cleanly`: `resume_page = 9`, then `history_page = 9` with `history_pages: 20`, and `total_pages: 3` in a body with no list: `resume_page = 0`, `history_read = 20`, `failures = 0`, and no `last_error`.
  - `history_reads_older_pages_then_stops`: `history_pages: 4`, budget 3: first poll pages 1, 2, 3 by history; next poll goes on; stops at 4 pages read; survives a restart. `history_pages: 0`: never.
  - `a_429_waits_as_told`: `Retry-After: 120`: `RateLimited(120)`, `next_poll_at = now + 120`, `failures` 0, nothing retried in this poll. An HTTP date, `X-RateLimit-Reset`, none (60), and 2 days (172,800, not shortened).
  - `a_rate_limit_envelope_is_the_same`: HTTP 200 with `rate_limited.json`.
  - `failures_back_off_to_six_hours`: 500s: `next_poll_at` is now + 600, 1,200, 2,400, and capped at 21,600. A success resets `failures` to 0 and clears `last_error`.
  - `bad_json_and_bad_shape_are_failures`: nothing imported, nothing panics.
  - `pages_before_a_failure_are_kept`: page 1 fine, page 2 is a 500: page 1's titles stored.
  - `requests_are_spaced`: `pause` got `max(request_delay_ms, min_delay_ms)` between pages: 1,000 ms, and 4,000 ms for `xrel`.
  - `stopping_ends_a_poll`: `pause` returns false: `Stopped`, no more requests.
  - `ureq_fetch_reports_status_and_headers`, in `tests/predb_ureq.rs`: a `tiny_http` server on `127.0.0.1`: a 429 with `Retry-After` comes back as a reply and not an `Err`, and a body over 8 MiB is an `Err`.
- [ ] **Implement** section 6.
- [ ] **Run:** `cargo test --test predb_poll && cargo test --test predb_ureq`. **Expected:** pass.
- [ ] **Commit:** `Poll PreDB feeds with backoff and resumable cursors`

### Task 5: Matching

**Files:** create `src/predb/matcher.rs`; tests in it, on an in-memory connection with the schema and `import`.

Titles: `Alpha.Beta.Gamma.2024.1080p.WEB-DL.H.264-TESTGRP` as T, and `Second.Title.Name.2023.720p.WEB-OTHER`.

- [ ] **Tests first:** each `find` row is `(name, display_name, size, files, inner) -> (kind, title)`:

| Case | Evidence | Result |
|---|---|---|
| `exact` | name T | Exact, T |
| `case_only` | name T uppercased | Exact, T |
| `with_a_file_suffix` | name `T.mkv` | Exact, T |
| `separators_folded` | `Alpha_Beta Gamma_2024_1080p_WEB-DL_H_264-TESTGRP` | Normalized, T |
| `folded_but_sizes_disagree` | the same, size 500, T's size 5 GB | none |
| `display_name_counts` | name `Xk3Mz9PqL`, display T | Exact, T |
| `md5_named` | name `md5_hex(T)` | Hash, T |
| `sha1_named_uppercase` | name `sha1_hex(T)` uppercased | Hash, T |
| `a_hash_named_file` | files `md5_hex(T).rar` | Hash, T |
| `a_hash_with_an_extension` | name `sha1_hex(T).mkv` | Hash, T |
| `a_suffixed_hash` | name `md5_hex(T) [12345678]` | Hash, T |
| `a_collision_suffix` | name `T [12345678]` | Exact, T |
| `a_suffix_on_a_suffix` | name `T.mkv [12345678]` | Exact, T |
| `same_prefix_wrong_digest` | MD5 with its last digit changed | none, though the key finds T |
| `an_inner_directory` | inner `T/x.mkv` | InnerFile, T |
| `an_inner_file` | inner `T.mkv` | InnerFile, T |
| `two_titles_are_no_match` | inner `T.mkv` and `Second…-OTHER.mkv` | none |
| `extra_text_after_the_title` | name `T.Extra` | none |
| `missing_the_group` | name T without `-TESTGRP` | none |
| `many_episodes_name_nothing` | inner `Show.Name.S02E03…` and `…S02E04…`, with the first a title | none |
| `one_episode_can` | inner `Show.Name.S02E03…` only | InnerFile |
| `a_short_title_needs_exact` | title `Foo.Bar-GRP`, name `foo_bar-grp` | none. Name `foo.bar-grp`: Exact |
| `too_many_rows_is_no_match` | 17 titles forced under one key | none |

  - `literally_table`: `Foo.Bar-GRP.part01.rar` over `foo.bar-grp` yes; `Foo.Bar-GRP.Extra` no; `Foo.Bar-GRP2` no.
  - `is_full_title_table`: `Foo.Bar-GRP` no (short), T yes, `Just.Words.And.More.Words` no (no group).
- [ ] **Implement** section 6.
- [ ] **Run:** `cargo test --lib matcher`. **Expected:** pass.
- [ ] **Commit:** `Match releases to PreDB titles literally, and refuse anything ambiguous`

### Task 6: The step

**Files:** create `src/predb/step.rs`; modify `src/postprocess/schema.rs`, `src/postprocess/steps.rs`; test `tests/predb_stage.rs`, on Part 3's `tests/pp_stage.rs` harness with `steps::all()`, settle 1 s, `min_size_bytes: 0`, and `predb.enabled: true` through `set_config`.

- [ ] **Tests first:**
  - `worth_looking_up_table`. False: `name_source` 100; `DSC_0001.jpg` with 1 part and with 7 parts; `Xk3Mz9PqL` with 1 part. True: `Xk3Mz9PqL` with 9 parts; `md5_hex(T)` with 1 part; T with 1 part.
  - `a_picture_costs_no_lookup`: `DSC_0001.jpg`, 1 part, with `predb_match` the only step: no `pp_state` row, and no connection to the file was opened, by a counter in `with_reader`.
  - `db_path_from_shard_table`: `/x/atlas.s3.db` gives `/x/atlas.predb.db`, `/x/other.s0.db` gives `/x/other.predb.db`, `/x/atlas.db` gives none.
  - `a_match_renames_with_the_top_source`: a release named `T.mkv`, T in the file: `display_name = T`, `name_source = 100`, `is_obfuscated = 0`, a `predb_match` row, `releases_fts` finds T.
  - `the_title_beats_a_good_name`: `display_name` at `Par2` and an inner file matching T: T wins.
  - `a_rerun_is_stable`: `Stage::rerun`: `display_name` unchanged, and a recording step at rank 400 with `on_rename` does not run again.
  - `a_naming_rename_reruns_it`: `Naming` gives a regex name equal to T: `predb_match` runs after it in the same run and matches.
  - `new_inner_files_are_seen`: `archive_inner_files` rows, then `Stage::rerun(id, mask(StepId(8)))`: `naming` asks for step 16, and T matches.
  - `an_unmatched_release_records_its_inner_keys`: `predb_inner_key` has at most 4 rows, for good names only; a later match clears them.
  - `a_season_pack_stays`: `Show.S02.1080p.WEB-DL.x264-GRP` with an inner `S02E03` file whose stem is a title: no rename.
  - `facts_reach_later_steps`: a recording step at rank 400 sees `predb.category`.
  - `the_first_match_of_a_title_reruns_later_parts`: a recording step `StepId(24)`, rank 400: runs again when T equals the name already shown; on a rerun of the same title it does not.
  - `off_means_no_step_and_no_file`: default config: no bit, no `atlas.predb.db`, and the walk doesn't start.
  - `a_missing_file_retries`: enabled with the file deleted: `db_attempts = 1`, `last_error` set, no panic. Restored, it finishes.
  - `a_deleted_release_goes_from_both_tables`: covered by `SHARD_TABLES`; assert it here for `predb_inner_key` too, by query plan that the delete is a seek.
- [ ] **Implement** section 6, and both registry lines.
- [ ] **Run:** `cargo test --test predb_stage && cargo test --test pp_schema && cargo test --lib steps_have_unique && cargo test --lib compact`. **Expected:** pass, including Part 3's registry-driven survival tests.
- [ ] **Commit:** `Add the PreDB match step`

### Task 7: Late arrivals

**Files:** create `src/predb/late.rs`; test `tests/predb_late.rs`.

- [ ] **Tests first:**
  - `each_route_nominates`, one shard each, using `store::save`, a row per line of "What each route finds": `name` = T; `T.mkv`; `t.mkv`; `T [12345678]`; `T.mkv [12345678]`; `display_name` = T on a junk `name`; `<md5>` and `<sha1>` of T in lower and upper case, each also with `.mkv` and with ` [12345678]`; a `predb_inner_key` row. Each gives that id.
  - `a_title_only_in_a_later_file_is_a_known_miss`: `name` = `grp-t.mkv`, a second file `T.nfo`, no `display_name`: `nominate` gives nothing. Then `Stage::rerun(id, mask(PREDB_MATCH))`: the step matches T through its files. This pins the miss and the remedy.
  - `an_fts_hit_needs_a_literal_agreement`: `T.Extra` shares the phrase: not nominated.
  - `a_short_title_skips_the_phrase`: `Foo.Bar-GRP` with a `Foo.Bar-GRP.mkv` release: not nominated, and no phrase ran, by a counter. The exact name still is.
  - `a_named_release_is_left_out`: `name_source = 100`.
  - `too_many_is_refused`: 17 exact names over shards: `overflow`, no ids, and `tick` moves on with no rerun.
  - `the_budget_stops_between_shards`: a `clock` that jumps 3 s after the first phrase query: the later shards' phrase is skipped, `out_of_budget`, and the other routes still ran.
  - `stop_ends_a_title_between_statements`: `stop` set after the first shard: `stopped`, and the cursor stays.
  - `the_cursor_waits_and_resumes`: a title 10 s old isn't read; at 601 s it is. A new `Connection` carries on after it. Zero titles: nothing changes.
  - `a_failed_rerun_keeps_the_title`: `rerun` errors: the cursor stays.
  - `no_statement_scans_releases`: `explain query plan` for each statement. Exactly: `SEARCH releases USING INDEX idx_release_unique (name=?)`, `SCAN releases_fts VIRTUAL TABLE INDEX …`, `SEARCH releases USING INTEGER PRIMARY KEY (rowid=?)`, `SEARCH predb_inner_key USING PRIMARY KEY (key=?)`. None is `SCAN releases`.
  - `a_title_arriving_later_renames_the_release`, end to end with `Stage`: index `T.mkv` with an empty file, step done and no match. Insert T through `import`, with `added_at` 601 s back. `tick` with `stage.rerun(id, mask(PREDB_MATCH))`: `name_source = 100`.
  - `#[ignore] late_cost_on_a_big_shard`: 2M synthetic rows; prints the time per title by route. Not a gate.
- [ ] **Implement** section 6.
- [ ] **Run:** `cargo test --test predb_late`. **Expected:** pass.
- [ ] **Commit:** `Find releases for PreDB titles that arrive late, through indexes only`

### Task 8: The service inside the indexer

**Files:** `src/predb/mod.rs`, `src/bg_indexer.rs`, `src/stats_dashboard.rs`; test `tests/predb_bg.rs`, one test, env set first, as `tests/auto_compact.rs`, and `tests/predb_bg_default.rs`, one test.

- [ ] **Tests first:**
  - `tests/predb_bg_default.rs`: a mock server and no `predb` key: no `atlas.predb.db`, `stats.json` has no `predb` object, no thread named `atlas-predb-*`. **This is the rule 8 test**, and passes before the change too.
  - `tests/predb_bg.rs`: `predb.enabled: true`, `sources: ["predb_club"]`, `late_per_second: 1`, `auto_run_compact: true`, `ATLAS_COMPACT_EVERY_SECS=2`. Before `run_until`, install a `Scripted` with `set_test_fetch`, scripted to return a page of titles on every poll, and insert enough titles that the late thread is busy at each stop. Titles arrive, and a saved release matching one is renamed. Every compaction rewrites **all 8 shards** (`common::shard_inodes` before and after, every inode changed, as `tests/pp_auto_compact.rs` asserts), with `predb_match` rows kept and `atlas.predb.db` untouched. This is what fails if the late thread outlives `run`, because `compact_shard`'s checkpoint can't finish. Polling carries on. `stats.json` has `predb.titles` and `predb.sources`. `REAL_REQUESTS` is 0, and every URL in `Scripted.urls` is `predb.club`'s scripted one.
  - In `src/bg_indexer.rs`: `predb_turning_on_or_off_rebuilds`.
  - `stop_ends_every_thread`: `stop_and_join` returns within the `Scripted` delay, and no `atlas-predb-*` thread is left. Afterward `pragma wal_checkpoint(truncate)` on each shard reports `busy = 0`, so no reader survived.
  - `a_source_it_doesnt_know_is_ignored`: `sources: ["nope"]`: one log line, no request.
  - `a_reload_takes_effect`: `set_config` with a new `sources`: polled without a restart.
  - Extend the page loop at `src/stats_dashboard.rs:1506`: page 6 with a fixture `stats.predb` shows `PreDB` and the title count.
- [ ] **Implement** section 4, and `set_test_fetch`. `run` naps 250 ms and on stop calls `stop_and_join` through `spawn_blocking`, so every connection to a shard is gone before `tasks` drains and any compaction starts. A failed `Service::start` logs and leaves indexing running.
- [ ] **Run:** `cargo test --test predb_bg && cargo test --test predb_bg_default && cargo test --test auto_compact && cargo test --lib stats_dashboard && just test`. **Expected:** pass.
- [ ] **Commit:** `Run PreDB feeds and late matching inside the indexer`

### Task 9: `atlas --predb-import`

**Files:** create `src/predb/file_import.rs`; modify `src/main.rs`; test `tests/predb_import.rs` through `CARGO_BIN_EXE_atlas`, as `tests/compact_command.rs`.

- [ ] **Tests first:**
  - `a_dry_run_counts_and_writes_nothing`: a file of 3 valid and 2 invalid lines: stdout `3 valid, 2 invalid`, and no `atlas.predb.db` appears.
  - `apply_imports_plain_and_gzip`: both, including `title<TAB>category<TAB>pre_at`. `source = 100`. A second run: 0 inserted.
  - `a_bulk_import_skips_the_late_queue_and_says_so`: with the cursor caught up before the run, `late_after_id` moves to the new top, and stdout names `atlas --reprocess --step predb_match --apply`. With the cursor behind: it stays.
  - `a_big_file_commits_in_batches`: test env `ATLAS_PREDB_IMPORT_BATCH=10`: 25 lines, 3 commits by a counter; a stopped run re-run gives the same rows.
  - `it_is_refused_with_an_indexer_alive`, using Part 3's `common::fake_indexer(home)` from `tests/common/mod.rs`, which runs a process with `--bg-indexer` in its arguments and writes `bg_indexer.pid`: exit 1, nothing changed. `a_missing_file_is_exit_1`.
  - `a_line_with_no_newline_is_skipped_and_counted`: a `.gz` holding 8 MiB of one character, a newline and a good title: stdout has `1 too long`, the title is imported, and exit 0.
  - `exactly_the_line_cap_is_kept`: 1,023 bytes and a newline is read as a line, and is then rejected by `valid_title` as invalid, not as too long. 1,025 bytes is too long.
  - `a_gzip_bomb_stops_at_the_byte_cap`: `ATLAS_PREDB_IMPORT_MAX_BYTES=65536` and 8 MiB of text: exit 1, the message names the cap, and batches already committed stay. `too_many_lines_stops`: `ATLAS_PREDB_IMPORT_MAX_LINES=10` and 50 lines: the same.
  - `a_bad_utf8_line_is_invalid`: counted, and the run goes on.
  - `the_import_path_has_no_http`: `include_str!("../src/predb/file_import.rs")` mentions neither `ureq` nor `poll::`.
  - `help_lists_it`.
- [ ] **Implement** section 6. 10,000 titles per transaction. `tune_for_writing` on this connection.
- [ ] **Run:** `cargo test --test predb_import`. **Expected:** pass.
- [ ] **Commit:** `Add atlas --predb-import for a local title list, dry run by default`

### Task 10: Docs and full verification

**Files:** `README.md`, `config.example.json`, `src/main.rs` help.

- [ ] **Implement** section 12.
- [ ] **Run:** `just format-check && just lint && just test`, then section 11. **Expected:** all pass.
- [ ] **Commit:** `Document PreDB`

## 9. Performance and load

Derived or measured on synthetic data, as the header says. No test touches live data, and nothing here is a gate.

**Network.** Default: none. With 2 sources at the defaults, 1–3 requests per source per 5 minutes: at most 0.02 requests/s. A page of 100 rows is about 30 KB, so under 20 KB/min. The worst case per response is the 8 MiB cap, with a 15 s timeout. xREL stays under 900 requests/hour by its 4 s floor. No connection is ever held open.

**Titles per day.** `BUGS.md` #25 implies about 2,500 from one source (10,000 entries over 4 days). `BUGS.md` #24 and #34 imply 5–7.5 per minute, up to about 11,000 a day. So at most 0.13/s.

**Reach.** Part 3's `judge` now passes about 100M rows. `worth_looking_up` decides which cost anything, in under 2 µs. The shares come from the overview's sample of 100 scaled to 118M, so treat them as rough.

| Rows | Share | Count | `predb_match` |
|---|---:|---:|---|
| One-article fragments | 29% | 34M | Rejected by Part 3 |
| Images, any number of articles | about 58% | 68M | Skipped |
| The rest | about 13% | 15M | Runs, unless one article with a random name |

So about 15M rows run the step. If the sample is badly off and nothing but fragments is skipped, it is 84M, and the rate below still bounds the cost.

**Forward matching.** A typical release is 2 or 3 lookups, and up to about 34 with many files. 17 µs measured warm at 1M titles, so about 50 µs typically and under 600 µs at worst, plus `files` and `inner_files` reads of about 20–50 µs. A cold 2 GB file costs up to 4 page reads a lookup until the OS has cached it, about 0.4 ms.

**Duration.** The walk's rate is Part 3's budget: 118M rows at 2,000 rows/s is 16.4 h, however many pass. At that rate a step at about 100 µs is at most 20% of a core, or 7–20% typically. 15M rows at 100 µs is 25 min of CPU over the 16.4 h. Turning `predb.enabled` on later starts its own `steps` walk of the same length.

**Late arrivals, per title.** Measured on one synthetic 5M-row shard (1.06 GB, 70% random names, 30% scene-like, with `1080p`, `web` and `dl` on 850,000 rows each):

| Route | Measured at 5M rows | Scaled to 15M rows a shard, ×8 shards |
|---|---:|---:|
| `name =` probe, hit or miss | 40–73 µs | under 1 ms |
| 2 hash tokens, absent or rare | 0.06–0.12 ms each | under 2 ms |
| `predb_inner_key` probe | under 20 µs, same structure | under 1 ms |
| Phrase, a real title, p50 | 11 ms | 0.26 s |
| Phrase, a real title, p99 | 130 ms | 3 s, cut to about 2 s by the budget |
| Phrase, all common tokens, no match | 30–165 ms | capped by the budget |
| Phrase with a token nobody uses | 0.06–0.12 ms | 3 ms |

FTS5 walks the rarest token's postings against the others, so the time follows the rarest token's count and not the table's size, which is why these scale by 3 at most. Part 3's revision changes none of this: the index holds every release's name already. On a cold 40 GB shard, expect up to about 10× until its pages are cached. That is an estimate.

Steady state: 0.13 titles/s × 0.26 s = 3.4% of a core, on one thread with read-only connections, and no writes except one rerun per real match and the cursor. At the `late_per_second` default of 1: at most about a quarter of a core during a burst. The first poll of 5 sources at the default budget brings at most 1,500 titles, or 25 minutes at 1/s.

Nothing here scans. `no_statement_scans_releases` pins the plan of every statement.

**A bulk import.** 10M lines of random-key inserts: estimated 5–30 min, at 2 GB of file and 10,000 titles a transaction. It does not use the late queue, which would need 115 days at 1/s. The walk of `atlas --reprocess --step predb_match --apply` takes 16.4 h instead. Memory stays bounded by the line cap whatever the file holds.

**Writes and growth.** There is no data on the match rate, so these use an assumed 20% of the rows that run, with 37.6M as the upper bound on those rows.

| Rows that run | Matches at 20% | Writer time, all shards | `predb_match` | FTS growth until `--compact` |
|---:|---:|---:|---:|---:|
| 15M | 3M | 2–3 min | 240 MB | 0.2–0.4 GB |
| 37.6M | 7.5M | 4–8 min | 600 MB | 0.45–0.9 GB |

A match is the same as Part 5's rename, about 30–60 µs and 2–4 KB of WAL, plus one row of about 80 B. Setting `display_name` to a title equal to `name` still adds its tokens, about 60–120 B a row. At most 2,000 applies/s over 8 writers, about 1.5% of a writer. A non-match with no inner files writes only `pp_state`, and a skipped row writes nothing.

`predb_inner_key` is about 20 B a row plus about 20 B in its index, at most 4 rows a release, and only for releases Part 4 has read: at 300 bodies per minute, about 144,000 releases a day, so at most 23 MB/day.

**Memory.** One 2 MB-cache reader on each DB-lane thread, one for the late thread, and 8 shard readers: under 30 MB. A page of JSON is at most 8 MiB, held briefly.

## 10. Failure handling

| Case | What happens |
|---|---|
| A feed is down, or sends 5xx or bad JSON | `failures + 1`, backoff to 6 h, that source only. The others and indexing carry on. A failed connection never marks anything caught up |
| A feed changes shape | `ParseError::Shape`, backoff, `last_error` on the dashboard. Nothing is imported from that page |
| Rate limited | Rescheduled for as long as the server asked. Not a failure. No sleeping thread, so the others keep their schedule |
| Banned or forever failing | At most 4 requests a day at the cap. `BUGS.md` #26 is why it backs off |
| A hostile title | `valid_title` refuses control characters, path characters and quotes. A row can only ever be a name candidate, which a release must match literally |
| A hostile feed offering a false title for a real name | Exact matching trusts the catalogue. Decision 12 |
| Key collision | `literally` or a digest check rejects it. No rename |
| Two titles for one release | `unique` gives no match, and it is checked again on the next rerun |
| `atlas.predb.db` missing or corrupt | The step returns `Retry`, then its failed bit after `max_attempts`, and `--reprocess --step predb_match --failed` retries. The service logs and naps 60 s. Indexing is unaffected. Deleting the file is safe: a new one starts empty, because nothing else depends on it |
| FTS error on a title | Logged, counted in stats, and the other routes still count for it |
| Crash during a poll | Finished pages are committed. `next_poll_at` was not moved, so it polls again at once, and duplicates are skipped |
| Crash in the late path | The cursor is committed per title. A title's reruns may repeat, and the step's answer is stable |
| Clean stop | `Stopped`; threads join. The late thread takes at most one FTS query, and the feed thread at most one HTTP timeout (15 s). No shard connection outlives `run`, so a compaction's checkpoint never meets one |
| Compaction running | Only `atlas --compact` from outside, which refuses while the indexer holds its guard. So the service never overlaps one |
| `predb.enabled` turned off | Rebuild. Step off, service gone. Names and `predb_match` rows stay |
| Wipe | The file stays and the wipe says so. New releases match forward |
| A title deleted from the file by hand | `predb_match.title_id` dangles harmlessly. The name stays |
| Re-run | Same title, same name, no rerun of later steps |
| A title the late path can't reach | The release keeps its name, and the title stays in the file. The forward step matches it on the next run, or `--reprocess --step predb_match`. See "What it still misses" |
| A huge or hostile import file | Lines over 1,024 bytes are skipped and counted. Past 50M lines or 16 GiB decompressed it stops with exit 1. Committed batches stay, and re-running skips them as duplicates |
| xREL cursor past its last page | `total_pages` makes it an empty list: the source has ended, not failed |

## 11. Acceptance checks

Run in the part's worktree. None touch live data or a live server.

```bash
just format-check && just lint && just test
# expected: all pass, no warnings

cargo test --test predb_bg_default
# expected: pass: no file, no thread, no stats, with an existing-style config

cargo test --test predb_bg
# expected: pass: all 8 shards compact with the late thread busy, and REAL_REQUESTS is 0

cargo test --test predb_import
# expected: pass, including the gzip bomb and the line with no newline

cargo test --test predb_poll a_429_waits_as_told && cargo test --test predb_poll failures_back_off_to_six_hours
# expected: pass

cargo test --lib matcher
# expected: pass, including same_prefix_wrong_digest and two_titles_are_no_match

cargo test --test predb_late no_statement_scans_releases && cargo test --test predb_late a_title_arriving_later_renames_the_release
# expected: pass

cargo test --test predb_file
# expected: pass: compaction, conversion and wipe leave the file's bytes alone

git check-ignore atlas.predb.db
# expected: prints atlas.predb.db

! grep -rn '"irc\|tungstenite\|websocket' Cargo.toml src/predb
# expected: no output, exit 0

HOME_DIR=$(mktemp -d)
ATLAS_HOME=$HOME_DIR cargo run --quiet -- --help | grep -- --predb-import
# expected: listed
```

## 12. Docs

- `README.md`, "Top-level keys" (`:127`): a `predb` row pointing to a new table.
- New `### PreDB` under Usage: what it does, off by default; the five sources and what each is for; `predb_org` and `history_pages` for history; each feed's load; the third parties a poll contacts; that a title beats any other name. `--predb-import`, and running `--reprocess --step predb_match --apply` after it.
- "How the database is stored" (`:246`): `atlas.predb.db`, what is in it, sizes from section 5.
- "Settings" (`:293-298`): Wipe keeps `atlas.predb.db`, and how to delete it.
- "Back up the database" (`:461`): add `atlas.predb.db` to the copy command. Its history can't be re-fetched past what feeds allow.
- "Stats dashboard": the PreDB block on the Post-processing page.
- "Credits" (`:516`): newznab-tmux's PreDB feeds and matching rules, GPLv3.
- "Source layout" (`:491`): `src/predb/`.

## 13. Decisions

1. **A separate file, not a shard table.** A title belongs to no release or group. Keeping it apart also means compaction never rewrites it, 8 shards don't each carry a copy, and it can be deleted or copied alone.
2. **No schema in `atlas.db`.** Its role is cursors and ids, and it is what `refuse_orphaned_shards` protects. The PreDB file can be missing with no effect on it.
3. **No IRC.** It would add a long-lived client (upstream: `IRCClient.php` 1,336 lines, `IRCScraper.php` 1,152, `PreAnnounceParser.php` 841, `IrcNetworks.php` 1,082) for one benefit: titles seconds earlier rather than within 5 minutes. A release waits 15 minutes to 2 hours to settle before any step runs, and the late path covers the rest, so those seconds buy nothing. The costs are real upstream: its one default channel fell silent (`BUGS.md` #24), a banned address reconnected twice a minute (#26), one dead address kept a network offline (#39), and repeats relabelled sources (#34). And it would keep a connection from this machine's address open to third-party networks.
4. **No WebSocket stream.** The same reasoning. The polled feeds carry the same rows, and the stream needs a client (`WebSocketClient.php`, 541 lines) and a supervisor.
5. **JSON sources first.** `serde_json` and `ureq` are already dependencies, and all five reuse one poller and differ by a row-mapping table. `predb_club` and `predb_net` are upstream's live defaults, `xrel` adds scene and P2P names, and `predb_org` is the only one with deep history. RSS needs an XML reader for weak listing sites. `srrdb` is left out because its terms say not to scrape.
6. **A wipe keeps the titles.** Feeds can't give back old titles: `predb.net` stops at about 4 days (`BUGS.md` #25). So deleting is the one irreversible choice, and keeping is undone with `rm`. Nothing in the file refers to a shard, so it can't go stale. Upstream's reset also keeps `predb` (`BUGS.md` #23). The wipe says what it kept.
7. **Compaction ignores it.** It has no sealed blobs or duplicate segments to clear. A title is never deleted, so its pages don't fragment, and SQLite's normal operation is enough.
8. **Keys are the first 8 bytes of a digest, in one table.** One `(key, title_id)` table handles the name, MD5 and SHA-1 at 19 B a row measured, against about 34–60 B for full digests and a duplicated title index. Collisions cost nothing because the literal check or the full digest always decides.
9. **No Unicode normalization.** It would add a dependency for titles that are nearly all ASCII. A composed and decomposed mismatch fails to match, which is the safe way to fail.
10. **Hash matching is new.** Upstream dropped `predb_hashes` in 2025, so its value today is unproven. It is kept because the brief asks and it costs only 2 of the 3 key rows per title, about 37 B, plus 4 index probes per late title, and it can't false-positive with a full digest check. MD5 and SHA-1 of the exact title only; not `md5(md5())` or lowercased variants.
11. **The late path nominates; the step decides.** One set of rules, one writer path, and `Stage::rerun` is the designated door. Over-nominating only costs a step run.
12. **A catalogue title outranks everything, and goes around `naming::improves`.** A literal match of a release's own name or inner file against a catalogue is the strongest evidence Atlas can have. A feed lying about a title can only affect a release already literally named that. Season packs stay protected.
13. **FTS plus indexes rather than a table of every release's keys.** A key per release would need 37.6M+ random-key inserts during the walk, a write on every step run, and 2–4 GB. FTS is free and already there, handles case and separators, and `display_name` is in it. Only inner files have no existing index, so only they get `predb_inner_key`. Their rate is limited by the body budget, so this table can't hurt the writer.
14. **`LATE_MAX_RERUNS = 16`.** Upstream's number for the same reason. A title on that many releases is reposted so often or so generic that it shouldn't move by this route. Forward matching still handles each release on its own evidence whenever it is next looked at.
15. **The title budget is checked between shards.** FTS5's search happens inside one `xFilter`, which neither a progress handler nor an interrupt can stop mid-way, so a query can't be cut short. Bounding by 2 s between shards is what can be enforced.
16. **The 600 s delay rather than re-checking.** It closes the race between a forward lookup and its inner-key row, with no extra state. Titles aren't urgent.
17. **Rate limits are obeyed by rescheduling, not sleeping.** Upstream retries within a run and gives up past 15 minutes. A rescheduled source can't hold up others, stops immediately, and never shortens a server's delay.
18. **Fixtures are written by hand from upstream's source, not recorded.** Hard rule 5 forbids contacting feeds during a port, so shapes come from upstream's parsers. A real feed may differ. That shows up as `ParseError::Shape` and backoff on the dashboard, with no bad data stored. The README tells the user what to look at on first enabling.
19. **`sizes_agree` is a factor of 3, and only for folded and file-based tiers.** PreDB sizes exclude PAR2 and yEnc overhead, and a post can hold more or less than the release. It exists to stop a small post that happens to share a name. An exact match or full hash needs no such check.
20. **`--predb-import` reads a local file only and is dry run by default.** The brief forbids a silent dump download. Feeds alone reach 10M titles only over months, so a user who has a list needs some way in. It writes to no shard.
21. **Bulk imports skip the late queue.** At 1/s, 10M titles would take 115 days. A walk over releases takes 16.4 h and finds the same matches.
22. **`predb_match` keeps a copy of `category`, `pre_at` and `nuked`.** Parts 7 and 8 read them through `ctx.conn()`, on the shard, with no second database. `title_id` is only for reading.
23. **`Step::enabled` is the single switch.** No separate "match" key. With `enabled: true` and no sources, an imported list still matches.
24. **The path comes from `ctx.conn().path()`.** Part 3 chose not to give `StepCtx` a main path or config, and it wins. The shard's own path has the same directory, which works for tests with any main database.
25. **Hash names go through FTS, not `name =` probes.** Upper and lower case each needed a probe, and neither found `<hash>.mkv` or Part 2's `<hash> [12345678]`. A hash is one rare token, which is the cheapest thing FTS can look up, it is case-folded, and it covers those forms. `accepts_hash` then checks the whole name.
26. **The late path states what it misses rather than growing to catch it.** The largest class is probably a title only in a file that isn't the row's name, which Part 2 makes more common. Catching it would mean indexing posted file names per release: a random-key insert per run on tens of millions of rows, on the writers Part 3 is careful with. A false negative costs one unnamed release until it next runs. `a_title_only_in_a_later_file_is_a_known_miss` keeps the claim honest.
27. **`worth_looking_up` skips a name with an image extension at any number of articles.** A title names a release directory. An album's `name` is its shortest file, so a title could sit beside it in `T.nfo`, and that is one more miss. It saves about 22M rows' file reads. The name judge isn't used on multi-article rows, because their files are the evidence.
28. **`set_test_fetch` is a process global, as `set_test_steps` is.** `run_servers` builds the service and takes no parameters from a test. `REAL_REQUESTS` turns "no live network" from a promise into an assertion.
29. **`parse` takes the page number.** xREL's end-of-list is in the body but relative to the page asked for. The alternative, a special case in `poll_source`, would put one source's rule in the shared loop.
30. **The import is capped on decompressed text, not on the file's size.** The compressed size says nothing: 198 KB expanded to 200 MB in the test. 16 GiB is about 50M lines at 300 B, far past any real title list, and the error says which cap it hit.
31. **A gap ends at a title older than the gap, not at a known page.** New titles can push pages the last poll imported down onto the saved cursor, and those read as known, with the real gap still below them. `titles.id` only grows and no title is deleted, so `id <= gap_floor` is proof of "from before". A title another feed stored first can make a page look older than it is for this feed, which is the same accepted miss as a known page in the forward pass.
