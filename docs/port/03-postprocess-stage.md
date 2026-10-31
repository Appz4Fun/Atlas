# Part 3: Post-processing stage

Every Atlas line number below is at base commit `de598b6`. Parts 1 and 2 merge first and shift them: find the named function, keep the intent, and record any difference in the PR. Upstream paths are at the commit pinned in `00-overview.md`. If this plan disagrees with a later part, this plan wins.

## 1. Goal

Today a release gets one chance: `save_slice` (`src/indexer.rs:1602`) parses, does its par2/nfo lookups inline, saves, and never looks again. After this part every release goes through a stage that survives restarts: it waits for the release to settle, rejects only fragments, blocked groups and empty rows, runs ordered processing steps with retries and backoff, re-runs later steps after a rename, and reports its backlog on the dashboard. Size, file type and completeness decide only whether a release gets body fetches, never whether it gets named or categorized. The 118M existing releases enter gradually, newest first. `atlas --purge` and `atlas --reprocess` clean up and revisit stored data, both dry-run by default. With an existing `config.json` the network load does not change. Part 3 itself ships no steps that do real work; Parts 4–8 plug into it.

## 2. Non-goals

- No archive, naming, PreDB, category or media logic. `steps::all()` ships empty.
- The inline lookups in `save_slice` (`src/indexer.rs:1616-1620`) stay. Part 4 or 5 may retire them.
- The stage itself never deletes or hides a release. Only `atlas --purge --apply` deletes.
- No new menu entries. The menu's "Purge broken releases" (`src/db.rs:446`, `src/store.rs:1652`) keeps its single transaction and `vacuum`. It gains the cleanup triggers, and refuses when a shard holds more than 1M incomplete rows, pointing at `atlas --purge --dead`.
- No API or search change, apart from names that steps improve.
- No body fetching or outside calls by default.
- No remote workers: the stage runs only inside `atlas --bg-indexer`.

## 3. Source map

| Upstream | Take | Leave behind |
|---|---|---|
| `app/Services/ReleaseProcessingService.php:1192-1205` | Rule: a post whose files are all par2 is rejected. Regex `\.(vol[0-9]+\+[0-9]+\.par2\|par2)` | The `collections`/`binaries` joins and deletes |
| `app/Services/ReleaseProcessingService.php:1207-1251` | Optional min size and min files; per group value and global combine as `max(group, global)` | The max size rule (off upstream by default), and deleting in batches as a side effect of a pass |
| `app/Services/Releases/Verification/CollectionReleasePolicy.php:231-247` | What "complete enough" means, 95% by parts. Here it gates body steps and `--dead` | Discarding from a pass; everything verification-specific |
| `database/support/LegacySettingsManifest.php:73-76,87-88` | Defaults: delay 2 h, timeout 48 h, completion 95%, min files 1, min size 0 | The settings tables |
| `database/support/LegacySettingsManifest.php:129-130`, `app/Services/AdditionalProcessing/AdditionalCandidateQuery.php:45-48,80,96` | 1 MiB and 100 GiB gate only upstream's body-fetching pass, so here they are `body_min_size_bytes` and `body_max_size_bytes`. Naming and categories run on every release upstream too | |
| `app/Services/Binaries/HeaderParser.php:119-120,152` | Fragment facts, through Part 1's `stored_fragment` | Slice-level shape grouping (Part 1 owns it) |
| `app/Services/AdditionalProcessing/AdditionalCandidateQuery.php:21-34` | Lesson: two copies of a predicate drifted and rows stuck forever. Atlas has one `rules` module | Claim columns and tokens (`:63-65`, `:339-443`) |
| `app/Services/AdditionalProcessing/AdditionalProcessingOrchestrator.php:245-292` | One bad release must not stall a batch: catch per release, keep draining | Re-selecting it every cycle with no backoff |
| `app/Services/AdditionalProcessing/AdditionalWorkPlanner.php:83-138` | Steps ask for specific files and segments, including the last segment for 7z (`:96-102`). Hence `SegmentPick` | The planner itself (Part 4) |
| `app/Services/AdditionalProcessing/ReleaseFileManager.php:564-577` | Count failures per release | Deleting a release after N timeouts |
| `app/Services/NameFixing/ReleaseUpdateService.php:850-873` | A rename resets later results: TV, movie, music and PreDB links. Hence `Reruns::on_rename` | The column list |
| `BUGS.md` #18 | Queue membership never depends on a config-dependent marker column | |
| `BUGS.md` #22, #28, #44 | No whole-table scan per worker or per dashboard tick; bounded counts only | |
| `BUGS.md` #16, #37, #52 | Lock order and one contended row capped throughput. Atlas sends stage writes through the shard's one writer | |
| `BUGS.md` #47, #49 | Separate pools overran the connection cap. Atlas shares `Pool` and never queues ahead of indexing | |
| `BUGS.md` #61 | Budget failures reread on every pass; backoff doubling up to 7 days | |
| `BUGS.md` #62 | Unbounded walks held passes for hours. Walks are bounded per tick, store a cursor, and stop at a fixed top id | |
| `BUGS.md` #63 | Late articles waited weeks on a cursor. Here a late article requeues the release in its save | |
| `BUGS.md` #64, #65 | Expensive work on doomed rows caused the backlog. Fragments are rejected before any step, with no write | |
| `BUGS.md` #66 | A body read stops at its byte cap, one long line included (`Conn::read_line_capped`) | |
| Root to-do file, lines 275, 281, 282, 289 | A failed connection must not mark work complete. Separate "matched", "checked, no match" and "retry". Database-only work never waits on downloads. Fairness between recent and historical rows | |

## 4. Atlas changes

**`src/store.rs`**
- `:1358` `conn.transaction()` becomes `conn.transaction_with_behavior(TransactionBehavior::Immediate)`. The save is a deferred transaction whose first statement (`IdKey::of`, `:1359`) is a read. SQLite does not call the busy handler when a connection holding a read tries to upgrade (`sqlite3BtreeBeginTrans`), so any other writer on the shard, such as purge, AI search or a reprocess command, makes the save fail at once with "database is locked" instead of waiting out the 30 s `busy_timeout` (`src/db.rs:59`). `seal_some` (`:1274`) already takes the lock up front.
- `:1362-1372` upsert: it overwrites `is_obfuscated` and lets any later save replace `display_name`, so a step's result would be undone by the next late article. Add the `name_source` guard (section 5). `poster` and `posted_date` keep their behavior.
- `:1484-1490` after `update_stats`: enqueue the release (section 5), in the same transaction. Keep `release_complete`'s result in a local for both uses. `now` is read once per save.
- `:199-240` `migrate_shard` ends with `crate::postprocess::schema::migrate(conn)?`. `build_shard` (`:126`) and therefore `create_shard` (`:120`), `db::create_shards` (`src/db.rs:314-316`), `compact_shard` (`src/compact.rs:418,424,724`) and `convert::copy` (`src/convert.rs:758,782`) all pick it up.
- New reads for steps: `release_files`, `file_segments`. `search::get_articles` (`src/search.rs:264`) is not usable by a worker: it opens the main database and attaches all 8 shards per call, and collects and sorts every article, while `each_article` (`:1559`) decodes with the uncapped `blob::decode` (`:1602`) and requires `s{N}` schemas. A loose file can hold any number of rows (`:773-776`). The new reads take a connection on one shard, hold a read snapshot (rows move into the blob when a file seals, see `:1561-1564`), use `sealed_segments` (`:794`, capped), stream rows with a bounded buffer, and dedupe by `seg_key` (`:906`).
- New `delete_release`, used by purge, and `too_many_incomplete`, used by the menu's purge.

**`src/blob.rs`**: new `pub fn count(blob: &[u8]) -> io::Result<usize>` that decompresses only the first 16 bytes and reads the version and count.

**`src/compact.rs`**
- `:445` `insert into releases select * from old.releases` is positional. A column added in a different order on an old shard would land in the wrong place. Replace with named columns from `pragma_table_info('releases','old')`.
- After `:709`: copy every table in `schema::SHARD_TABLES`, by name.
- After `:435`: refuse what would be lost. An unknown table in the original, or a column on `files`, `segments`, `domains`, `held_back`, `release_ids`, `meta` or `collections` outside the known list, stops the shard with "the original was kept". `FILE_COLUMNS` (`:820`) is read by position (`:523,624,644`), so a new `files` column would vanish silently today. `collections` (Part 2) is listed as a core table and stays out of `SHARD_TABLES`, because Part 2 copies it with its own row filter.
- `:416`: `checkpoint` becomes `checkpoint_patiently`, up to 10 s. A thread stopping as the indexer winds down can have its last read open for a moment, and today that fails the shard.
- `:944` the check's table list gains every registered table.

**`src/indexer.rs`**: `SaveJob` (`:208`) goes inside `enum WriterJob { Save(SaveJob), Stage(StageJob) }`, and `Db.saves` (`:191`) carries it. `writer` (`:240`) runs stage jobs on its own thread between saves, inside a time budget. New `Db::on_writer`.

**`src/nntp.rs`**: new `Pool::background_body`, which never joins the FIFO permit queue ahead of indexing unless it has already waited `BODY_PATIENCE`. `lease_for`'s tail (`:1314-1373`) moves into a helper both paths call. New `Conn::body_capped` over a new `Conn::read_line_capped`, because `read_raw_line` (`:411`) reads to the newline before anything can stop it, so one enormous line would pass any cap. `fetch_body` (`:2051`) and `first_names` (`:2098`) are unchanged.

**`src/db.rs`**: `purge_broken` (`:446`) refuses past `PURGE_BROKEN_MAX` incomplete rows in a shard.

**`src/bg_indexer.rs`**: `Scheduler` gains `stage: OnceLock<Arc<Stage>>`. `run_servers` starts the stage after the writers exist (after `:751`) and spawns its task into `tasks` (`:753-761`). `watch_config` (`:613`) passes reloads on, and treats `postprocess.enabled` changing like a server change (`:622-629`). `Scheduler::write_status` (`:444-452`) adds `postprocess` to `stats.json`; `write_status` (`:46`) adds `postprocess_backlog` to `status.json`.

**Compaction lock, verified**: no pause code is needed. The indexer holds the shared `WriteGuard` for all of `run_servers` (`supervise`, `src/bg_indexer.rs:849,865-879`). An outside `--compact` cannot take the exclusive lock meanwhile (`src/compact.rs:157-164`), and the indexer's own compaction runs only after `run_servers` returned (`:893-904`). The stage must end on `sched.stopping()` and hold nothing afterwards, and so must any thread Parts 6 and 8 start: a read still open fails that shard's first checkpoint, which `tests/pp_auto_compact.rs` asserts against.

**`src/config.rs`**: `PostprocessConfig`, parsed in `from_value` (`:264`). `save_config` already keeps keys it does not know (`:447-450`), so it needs no change. New `load_settings`, for the commands.

**`src/main.rs`**: `--purge` and `--reprocess` dispatch (`:22-32`) and help text (`:15-17`).

**`src/profile.rs`**: `Load` gains `writer_stage_ns` and `writer_stage_jobs`.

**`src/stats_dashboard.rs`**: `PAGES` (`:27`) gains "Post-processing"; dispatch `:552-556`, footer `:559-563` and keys `:1233` go to `1-6`.

**New `src/postprocess/`**: `mod.rs` (`Stage`, `run`), `schema.rs`, `rules.rs`, `step.rs`, `steps.rs`, `engine.rs`, `apply.rs`, `bodies.rs`, `stats.rs`, `batch.rs`, `purge.rs`, `reprocess.rs`. `src/lib.rs` gets `pub mod postprocess;`. `rules.rs` carries a GPLv3 attribution header naming the upstream files it ports.

**Part 1 consumed**: `src/filters.rs` exports what section 6 lists, defined in Part 1's section 4.4.

## 5. Schema and migration

All of it lives in the shards. `atlas.db` gains nothing.

```sql
-- what changed and wants a look: written by saves and rerun requests. Walks don't use it
create table if not exists pp_queue (
    release_id INTEGER PRIMARY KEY,
    due_at     INTEGER NOT NULL          -- unix seconds
);
create index if not exists pp_queue_due on pp_queue(due_at);

-- only releases that passed the rules and had a step run. Rejected rows have none.
create table if not exists pp_state (
    release_id   INTEGER PRIMARY KEY,
    done         INTEGER NOT NULL DEFAULT 0,  -- bit 1 << step id: step finished, match or not
    failed       INTEGER NOT NULL DEFAULT 0,  -- bit 1 << step id: gave up after max_attempts
    net_wanted   INTEGER,                     -- 1 while a body or outside step is left, else NULL
    db_attempts  INTEGER NOT NULL DEFAULT 0,  -- DB lane: consecutive failed visits
    net_attempts INTEGER NOT NULL DEFAULT 0,  -- net lane: consecutive failed runs of its step
    net_next_at  INTEGER NOT NULL DEFAULT 0,  -- net lane: not before this
    parts_seen   INTEGER NOT NULL DEFAULT 0,  -- releases.parts when steps last ran
    last_error   TEXT
);
create index if not exists pp_state_net on pp_state(release_id, net_next_at) where net_wanted = 1;
create index if not exists pp_state_failed on pp_state(release_id) where failed != 0;

-- a resumable pass over existing releases, newest first
create table if not exists pp_walks (
    id         INTEGER PRIMARY KEY,
    kind       TEXT NOT NULL,               -- 'backlog', 'steps' or 'reprocess'
    top_id     INTEGER NOT NULL,            -- fixed at creation: new rows use pp_queue
    before_id  INTEGER NOT NULL,            -- next row looked at has id < this
    redo_mask  INTEGER NOT NULL DEFAULT 0,  -- step bits to clear on the way
    group_name TEXT,
    seen       INTEGER NOT NULL DEFAULT 0,
    ran        INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL
);

-- append only, see RELEASE_COLUMNS. NULL: the name came from ingest. Otherwise
-- the priority of the step source that set it; saves leave the name alone.
alter table releases add column name_source INTEGER;

-- one per registered table with a release key, generated by schema::migrate
create trigger if not exists gc_pp_state after delete on releases
begin delete from pp_state where release_id = old.id; end;
```

Upsert (`src/store.rs:1362`):

```sql
insert into releases
    (id, name, size, complete, group_name, poster, posted_date, display_name, is_obfuscated)
    values (?, ?, ?, ?, ?, ?, ?, ?, ?)
    on conflict(name, group_name) do update set
    poster = excluded.poster,
    posted_date = excluded.posted_date,
    display_name = case when releases.name_source is null
                        then coalesce(excluded.display_name, releases.display_name)
                        else releases.display_name end,
    is_obfuscated = case when releases.name_source is null
                         then excluded.is_obfuscated
                         else releases.is_obfuscated end
    returning id, size, parts, file_total
```

Enqueue, once per release with added articles (`:1490`). `?2` is `now + COMPLETE_SETTLE_SECS` when `release_complete` is true, else `now + SETTLE_SECS`:

```sql
insert into pp_queue (release_id, due_at) values (?1, ?2)
on conflict(release_id) do update set due_at = excluded.due_at
```

The delay statics are `AtomicI64` in `store.rs` (defaults 7200 and 900), set by the stage from config. Saves have no config access, and `store::save` (`:1530`) enqueues the same way. Both delays are floored at 1 s, so a touch always moves `due_at` above any value already claimed, which is how an article arriving mid-run is never lost: apply deletes with `where due_at = :claimed` and matches nothing.

### Survival

| Path | What happens | Test |
|---|---|---|
| Open | `migrate_shard`: `create table/index/trigger if not exists` on new empty tables, `alter table add column` for a nullable column. No row is rewritten, and no index is built on an existing table. | `tests/pp_schema.rs` |
| `--compact` | `compact_shard` migrates the original (`:418`), creates the copy with the same schema (`:424`), copies each registered table by name, `build_shard` (`:724`) rebuilds indexes, and `check` compares counts. Ids are preserved (`:445`, `:646`), so rows keyed by release or file id stay valid. `meta` is copied (`:446`), so its keys survive. | `every_registered_table_and_column_survives_compaction` |
| `--convert` | Fresh shards get the empty tables from `create_shard`. Converted releases have no queue row and are reached by the backlog walk, seeded lazily at first stage start, so its `top_id` sits above them. | `tests/convert.rs` new test |
| Wipe | `db::wipe` removes whole shard files (`src/db.rs:174-201`). The next `create_db` makes the tables again. Nothing lives outside the shards. | `a_wipe_leaves_new_empty_tables` |
| `purge_incomplete` | Triggers clear stage rows on delete. | `the_menu_purge_clears_stage_rows` |

### How Parts 4–8 add data

1. **Own table (preferred).** Write a `const ShardTable` in your module and add it to `schema::SHARD_TABLES`. That one line gets it created on open, deleted with its release, copied and counted by compaction, covered by convert and wipe, and tested: the survival tests iterate `SHARD_TABLES` and fill rows with `schema::sample_row`.
2. **Nullable column on `releases`.** Append to `schema::RELEASE_COLUMNS`. Compaction copies it by name. Do not index it: building an index on 15M rows at open is what hard rule 7 forbids. A value you filter on goes in your own table with its own index (`(category, release_id)` for example).
3. **Never** add columns to `files`, `segments`, `domains`, `held_back`, `release_ids` or `meta`: compaction stops the shard. Keep per-file data in your own table keyed `(release_id, file_id)`.
4. **Never** create a virtual table in a shard. **Never** update `releases.name`: it is half of the upsert key (`src/store.rs:143`).

## 6. Interfaces

### Provided: schema (`src/postprocess/schema.rs`)

```rust
pub struct ShardTable {
    pub name: &'static str,
    /// `create table if not exists ...`
    pub create: &'static str,
    /// (index name, `create index if not exists ...`): dropped for a compaction's copy, rebuilt after
    pub indexes: &'static [(&'static str, &'static str)],
    /// column holding a release id: rows go when the release goes. None: shard wide
    pub release_key: Option<&'static str>,
    /// (column, type) added after the table first shipped, added on open
    pub added: &'static [(&'static str, &'static str)],
}
pub const SHARD_TABLES: &[&ShardTable] = &[&PP_QUEUE, &PP_STATE, &PP_WALKS /* Parts 4-8 add here */];
pub const RELEASE_COLUMNS: &[(&str, &str)] = &[("name_source", "INTEGER")]; // append only
pub fn migrate(conn: &Connection) -> rusqlite::Result<()>;
#[doc(hidden)] pub fn sample_row(conn: &Connection, table: &ShardTable, release_id: i64) -> rusqlite::Result<()>;
```

Table prefixes: `pp_` Part 3, `archive_` Part 4, `naming_` Part 5, `predb_` Part 6, `category_` Part 7, `media_` Part 8. `release_category` (Part 7) is an allowed exception: the prefix is a convention, and no test enforces it.

### Provided: steps (`src/postprocess/step.rs`)

```rust
#[derive(Clone, Copy, PartialEq, Eq, Debug)] pub struct StepId(pub u8);   // 1..=62, never reused
pub type StepMask = i64;                                                  // bit 1 << id
pub const fn mask(id: StepId) -> StepMask { 1 << id.0 }

pub enum Needs { Database, Bodies, External }

#[derive(Default, Clone, Copy)]
pub struct Reruns { pub on_rename: bool, pub on_growth: bool }

pub trait Step: Send + Sync + 'static {
    fn id(&self) -> StepId;
    fn name(&self) -> &'static str;          // the --step name, and the dashboard's
    fn rank(&self) -> u16;                   // run order, unique
    fn needs(&self) -> Needs;
    /// Is it on? Also the one way a step sees config, see "Config in a step".
    fn enabled(&self, cfg: &Config) -> bool { let _ = cfg; true }
    /// Cheap, no I/O. False: not run and not marked done, in either lane. The body gate
    /// below never sets the bit of a step whose `applies` is false.
    fn applies(&self, release: &ReleaseInfo) -> bool { let _ = release; true }
    fn reruns(&self) -> Reruns { Reruns::default() }
    /// Synchronous, on a stage thread. Safe to block on `fetch_bodies` or a `ureq` call.
    fn run(&self, ctx: &mut StepCtx<'_>) -> Result<Outcome, StepError>;
}

pub struct ReleaseInfo {
    pub id: i64, pub shard: usize, pub name: String, pub display_name: Option<String>,
    pub group: String, pub poster: Option<String>, pub posted_date: Option<String>,
    pub size: i64, pub parts: i64, pub complete: bool, pub file_total: Option<i64>,
    pub is_obfuscated: bool, pub name_source: Option<i64>,
}

pub struct StepCtx<'a> { /* private */ }
impl StepCtx<'_> {
    pub fn release(&self) -> &ReleaseInfo;   // shows a rename made earlier in this run
    pub fn files(&self) -> rusqlite::Result<Vec<store::FileInfo>>;  // capped at 5,000
    pub fn segments(&self, file_id: i64, pick: &store::SegmentPick) -> rusqlite::Result<Vec<store::SegmentRef>>;
    /// This shard, opened `query_only`. For reading your own tables. Do not keep a
    /// statement open across `fetch_bodies`: it pins the WAL.
    pub fn conn(&self) -> &Connection;
    /// A value an earlier step in this run left, see `Outcome::facts`.
    pub fn fact(&self, key: &str) -> Option<&str>;
    /// Only from `Needs::Bodies` steps. One at a time, within every budget.
    pub fn fetch_bodies(&mut self, wanted: &[BodyRequest]) -> Result<Vec<BodyResult>, StepError>;
    pub fn stopping(&self) -> bool;
    pub fn deadline(&self) -> std::time::Instant;  // run start + 120 s
}

pub struct BodyRequest { pub message_id: String, pub file: String, pub max_bytes: Option<usize> }
pub struct BodyResult { pub message_id: String, pub got: Got }
pub enum Got { Body(Vec<u8>), Missing, TooBig, Refused(&'static str) }

#[derive(Default)]
pub struct Outcome {
    /// Found something, for stats. Done with `matched: false` means "checked, no match".
    pub matched: bool,
    /// Applied in one transaction with the state change. Your own tables only, or your own
    /// `releases` column. Static SQL with bound values.
    pub writes: Vec<StepWrite>,
    pub name: Option<NameChange>,
    /// Later steps to run again: bits of steps with a higher rank. Others are ignored.
    pub rerun: StepMask,
    /// For later steps in the same run (their database writes are not visible yet).
    pub facts: Vec<(&'static str, String)>,
    /// Not finished, nothing wrong: look again then. Uses no attempt, sets no bit.
    pub wait_until: Option<i64>,
}

pub struct StepWrite { pub sql: &'static str, pub params: Vec<rusqlite::types::Value> }

/// `source` 1..=127, owned by Part 5's table: higher wins, equal replaces.
/// `display_name: None` leaves the name, `obfuscated: None` leaves the flag. A change of
/// either sets `name_source`, soo saves leave both alone from then on. Both None: nothing
/// happens. Only a `display_name` that differs counts as a rename. Never touches releases.name.
pub struct NameChange { pub display_name: Option<String>, pub obfuscated: Option<bool>, pub source: u8 }

pub enum StepError {
    Retry(String),      // a real failure: uses an attempt and backs off
    Permanent(String),  // failed bit now
    Transient(String),  // nothing wrong with this release: a server down, a file locked
    OutOfBudget,        // a budget is spent for now
    Stopped,            // shutting down
}
```

What each result does, by lane. `Begin` is the net lane's write before a step runs (Task 10). The lanes never touch each other's columns.

| Result | DB lane | Net lane |
|---|---|---|
| `Ok`, no `wait_until` | Done bit, `db_attempts = 0` | Done bit, `net_attempts = 0`, `net_next_at = 0` |
| `wait_until: Some(t)` | No bit, no attempt. Queue row `due_at = t` | No bit. The attempt `Begin` counted is given back. `net_next_at = t` |
| `Retry(e)`, or a panic | `db_attempts + 1`, `due_at = now + backoff`. At `max_attempts`: failed bit, `db_attempts = 0` | `Begin`'s attempt stands, `net_next_at = now + backoff`. At `max_attempts`: failed bit, `net_attempts = 0` |
| `Permanent(e)` | Failed bit | Failed bit, `net_attempts = 0` |
| `Transient(e)` | No attempt, `due_at = now + 300`. The lane does not pause, and the run's other steps carry on | Attempt given back, `net_next_at = now + 300`, net lane pauses 60 s |
| `OutOfBudget` | No attempt, `due_at = now + 60` | Attempt given back, `net_next_at = now + 60` |
| `Stopped` | Nothing written for this release; a queued row stays | Attempt given back, `net_next_at` now |

**Registration.** `steps::all() -> Vec<Arc<dyn Step>>` in `src/postprocess/steps.rs`, one line per step. A unit test enforces unique ids, ranks and names, and the id and rank ranges below. It does not check `needs`: a part may use any lane.

| Part | Step ids | Ranks | Needs |
|---:|---|---|---|
| 4 archive, par2 | 1–7 | 100–199 | Bodies |
| 5 naming, nfo | 8–15 | 200–299 | Database and Bodies |
| 6 predb | 16–23 | 300–399 | Database |
| 7 category | 24–31 | 400–499 | Database |
| 8 media | 32–39 | 500–599 | Database and External |

**Config in a step.** `StepCtx` has no config. `Stage::start` calls `enabled` on every step with the full `Config` before any `run`, and `Stage::set_config` calls it again on every reload. A step that needs a limit copies it there into its own `Mutex`. `enabled` may be called more than once for one config, so keep it cheap and idempotent. A step that needs the database's path reads `ctx.conn().path()`.

**Lanes.** The DB lane runs every undone `Database` step in rank order and never waits on the network. `Bodies` and `External` steps run in the net lane under their budget, lowest rank first, one per claim. So naming runs with the names it has; when archive later records inner files it sets `rerun: mask(NAMING)` and naming runs again.

**Re-running later steps.**
- A `NameChange` that really changes `display_name` clears the done bit of every higher-ranked step with `reruns().on_rename`. Nothing else is needed. In the DB lane, later `Database` steps then run in the same visit on the new name, once; net steps go back to the net lane.
- Otherwise set `Outcome.rerun` to the bits you want cleared. Only bits of higher-ranked steps take effect, so reruns cannot cycle: each one moves strictly down the ranks.
- A step never reruns itself. It returns `wait_until` or `StepError::Retry`.
- `releases.parts` growing clears steps with `reruns().on_growth` on the next visit. A `Bodies` step should set it, or a release that grows past `body_min_size_bytes` won't get it.
- From outside a step (Part 6 matching a new PreDB row): `Stage::rerun(release_id, mask)`.

**The body gate.** Before a `Bodies` step, the engine calls `rules::body_gate` for the release. `Outside` the bounds is `size < body_min_size_bytes`, `size > body_max_size_bytes`, or every filename an image: the done bit of each enabled applicable `Bodies` step is set with `matched: false`, it is left out of `net_wanted`, and it doesn't run. Completion below `min_completion_percent` is `NotYet`: no bit, no `net_wanted`, and a late article opens it again. `fetch_bodies` repeats the checks and answers `Got::Refused` for a blocked group, a release outside the bounds, any `file` matching `(?i)\.(jpe?g|png|gif|webp|bmp|tiff?)$`, and a request past `bodies_per_release`, all before any bytes move. Those refusals are the step's to judge, usually done with `matched: false`. Together with `judge` running first, that covers hard rule 9. `External` steps have no gate: a metadata call on an incomplete or small release is the part's own choice through `applies`.

Minimal step:

```rust
pub struct Naming;
impl Step for Naming {
    fn id(&self) -> StepId { StepId(8) }
    fn name(&self) -> &'static str { "naming" }
    fn rank(&self) -> u16 { 200 }
    fn needs(&self) -> Needs { Needs::Database }
    fn run(&self, ctx: &mut StepCtx<'_>) -> Result<Outcome, StepError> {
        let files = ctx.files().map_err(|e| StepError::Retry(e.to_string()))?;
        let name = best_name(ctx.release(), &files).map(|n| NameChange { display_name: Some(n), obfuscated: Some(false), source: 40 });
        Ok(Outcome { matched: name.is_some(), name, ..Outcome::default() })
    }
}
```

### Provided: reading a release (`src/store.rs`)

```rust
pub struct FileInfo {
    pub id: i64, pub filename: String, pub subject: Option<String>, pub subject_part: Option<i64>,
    pub expected: Option<i64>, pub file_total: Option<i64>,
    pub parts_seen: i64,            // popcount of files.seen
    pub touched_at: Option<i64>, pub sealed: bool,
}
pub struct SegmentRef { pub message_id: String, pub part: Option<i64>, pub bytes: i64 }
pub enum SegmentPick { First(usize), Last(usize), Parts(Vec<i64>) }

/// `conn` is one shard opened on its own (tables in `main`).
pub fn release_files(conn: &Connection, release_id: i64, limit: usize) -> Result<Vec<FileInfo>>;
/// Sealed blob plus late rows, from one snapshot, deduped by message-id, in (part,
/// message-id) order with a missing part first. Holds no more than `pick` asks for
/// beyond the blob (itself capped at SEAL_MAX_SEGMENTS). A corrupt blob is an error.
pub fn file_segments(conn: &Connection, file_id: i64, pick: &SegmentPick) -> Result<Vec<SegmentRef>>;
pub(crate) fn delete_release(tx: &Connection, release_id: i64) -> Result<Deleted>;
pub(crate) struct Deleted { pub articles: i64 }
pub fn too_many_incomplete(main: &Path, limit: i64) -> Result<bool>;
```

### Provided: the stage (`src/postprocess/mod.rs`)

```rust
pub struct StageParams {
    pub main: PathBuf, pub db: Db, pub pool: Arc<Pool>, pub config: Config,   // the full config
    pub steps: Vec<Arc<dyn Step>>, pub handle: tokio::runtime::Handle,
}
impl Stage {
    /// Calls `enabled` on every step, seeds the first walk and `pp_steps_seen`, starts the threads.
    pub fn start(p: StageParams) -> Arc<Stage>;
    pub fn set_config(&self, full: &Config);
    /// Clear these step bits and queue the release. Safe from any thread.
    pub fn rerun(&self, release_id: i64, clear: StepMask) -> anyhow::Result<()>;
    pub fn snapshot(&self) -> serde_json::Value;
    pub fn stop_and_join(&self);
}
/// Runs until `stopping` says so, then stops every stage thread.
pub async fn run(stage: Arc<Stage>, stopping: impl Fn() -> bool + Send + 'static);
/// For tests going through `run_until`: these steps instead of `steps::all()`.
#[doc(hidden)] pub fn set_test_steps(steps: Vec<Arc<dyn Step>>);
```

### Provided: long running commands

```rust
// src/postprocess/batch.rs. Part 5's --reflag-obfuscated and --unname use it too.
pub const COMMITS_PER_CHECKPOINT: u32 = 64;
pub const WAL_LIMIT: u64 = 4 << 30;
/// Call every COMMITS_PER_CHECKPOINT commits: `pragma wal_checkpoint(restart)`, up to 8 tries
/// 250 ms apart. If the -wal file is then over `limit`: Err "a reader is holding the WAL; close
/// the API or the menu and run this again". The caller's cursor is committed, soo a run resumes.
pub fn checkpoint_or_stop(conn: &Connection, path: &Path, limit: u64) -> anyhow::Result<()>;

// src/postprocess/reprocess.rs. One shard, in the caller's `BEGIN IMMEDIATE` transaction. Part 5
// adds its `meta.naming_rename_named` statement here when `mask` includes NAMING.
pub fn apply(tx: &Connection, mask: StepMask, group: Option<&str>, now: i64) -> rusqlite::Result<bool>;

// tests/common/mod.rs, cfg(unix). A process `procs::indexer_alive` takes for the indexer,
// with bg_indexer.pid written under `home`. Killed when dropped.
pub struct FakeIndexer(std::process::Child);
pub fn fake_indexer(home: &Path) -> FakeIndexer;
// also: rewriting every shard, for tests with a thread of their own to stop
pub fn shard_inodes(main: &Path) -> Vec<u64>;
```

### Provided: rules (`src/postprocess/rules.rs`)

```rust
pub enum Reason { Blocked, Empty, TooSmall, Fragment, TooFewFiles, Par2Only }
pub enum Verdict { Pass, Wait(i64), Reject(Reason) }
pub enum BodyGate { Open, Outside, NotYet }
pub struct RowFacts<'a> { pub group: &'a str, pub name: &'a str, pub size: i64, pub parts: i64, pub complete: bool, pub file_total: Option<i64> }
pub struct FileSummary { pub files: i64, pub last_touch: i64, pub present: i64, pub wanted: i64, pub all_par2: bool, pub all_images: bool,
                         pub single_part: Option<i64>, pub single_expected: Option<i64>, pub single_subject: Option<String> }
/// The one definition of who gets processed; the stage, the walk and purge all call these.
pub fn judge_row(rules: &Rules, row: &RowFacts<'_>) -> Option<Reason>;                      // steps 1-3, no `files` read
pub fn file_summaries(conn: &Connection, ids: &[i64]) -> rusqlite::Result<HashMap<i64, FileSummary>>;  // step 4
pub fn judge_files(rules: &Rules, row: &RowFacts<'_>, s: &FileSummary, now: i64) -> Verdict;  // steps 4-9
pub fn permille(row: &RowFacts<'_>, s: &FileSummary) -> i64;
pub fn body_gate(rules: &Rules, row: &RowFacts<'_>, s: &FileSummary) -> BodyGate;
```

`judge`, in this order, cheap tests first:

1. `filters::group_blocked(group)`: `Reject(Blocked)`.
2. `parts <= 0`: `Reject(Empty)`.
3. `size < max(min_size, group.min_size)`: `Reject(TooSmall)`. Off by default: both are 0.
4. Load the file summary: `select release_id, filename, subject_part, expected, seen, touched_at, subject from files where release_id = ?`, streamed with no limit. Only numbers are kept, plus the part, expected and subject of a one-file release. No files: `Reject(Empty)`. `last_touch` is the max `touched_at`; NULL means untouched since sealing, so old.
5. `quiet = complete ? complete_settle : settle`. If `now - last_touch < quiet`: `Wait(last_touch + quiet)`. This is the only `Wait`, so no queue row waits past settling.
6. `filters::stored_fragment(StoredPost { name, parts, files, part, expected, subject })`: `Reject(Fragment)`. No clock here: rejecting deletes nothing, and a late article judges it again. Purge adds `dead_after` before it deletes.
7. file count `< max(min_files, group.min_files)`: `Reject(TooFewFiles)`. Off by default: `min_files` is 1.
8. `reject_par2_only` and every filename matches `(?i)\.(vol[0-9]+\+[0-9]+\.par2|par2)$`: `Reject(Par2Only)`.
9. `Pass`. Size, images and completeness do not come into it.

`permille`: 1000 if `complete`. Otherwise `present = Σ min(popcount(seen), expected)`, `wanted = Σ expected` (a file with no `expected` counts 1 of 1), `permille = present * 1000 / wanted`, lowered to `files * 1000 / file_total` when `file_total` exceeds the file count.

`body_gate`: `Outside` when `size < body_min_size_bytes`, or `body_max_size_bytes > 0` and `size` is over it, or `all_images`. Otherwise `NotYet` when `permille < min_completion_percent * 10`. Otherwise `Open`.

### What "done" means

A release has no final state, because it can always grow. It is **settled** when no article arrived for `settle_secs` (`complete_settle_secs` if complete), **rejected** when `judge` says so (no row written, judged again if touched), and **done** when settled, passed, and every enabled applicable step has its done or failed bit, apart from `Bodies` steps waiting on `NotYet`. Dead is not a state: it is what `--purge --dead` reads, an incomplete release quiet for `dead_after_secs`. New articles requeue a release; a step turning on starts a walk that finds it again.

### Consumed

```rust
// src/filters.rs (Part 1)
pub fn group_blocked(name: &str) -> bool;
pub struct StoredPost<'a> { pub name: &'a str, pub parts: i64, pub files: usize, pub part: Option<i64>,
                            pub expected: Option<i64>, pub subject: Option<&'a str> }
pub fn stored_fragment(post: &StoredPost<'_>) -> bool;
```

`Pool` from `src/nntp.rs`, `Db` and `shared_db` from `src/indexer.rs`, `compact::Lock`, `compact::hold_off_compaction`.

### New `Pool` and `Conn` API

```rust
pub enum BackgroundBody { Body(Vec<u8>), Missing, TooBig }
/// A body for background work. Tries each server, least busy first, taking a connection
/// only when nobody waits for one and more than limit/4 are free. Past BODY_PATIENCE (20 s)
/// with none it queues normally, soo it cant starve either. Missing: every server said so.
/// Err: a server couldnt be asked.
pub async fn background_body(&self, message_id: &str, max_bytes: usize) -> Result<BackgroundBody>;

/// One line of at most about `max` bytes. None: past that, with at most one read's worth
/// more off the socket and the rest left unread, soo the connection cant take another request.
async fn read_line_capped(&mut self, max: usize) -> Result<Option<Vec<u8>>>;
/// `body` with a bound. None: over `max_bytes`, or more than one line per 16 of them. At most
/// `max_bytes` + 64 KiB off the wire, one long line included; about 6 × `max_bytes` in memory at the worst, a `Vec` per tiny line included.
pub async fn body_capped(&mut self, message_id: &str, max_bytes: usize) -> Result<Option<Vec<u8>>>;
```

`read_raw_line` (`src/nntp.rs:411`) reads to the newline before anything can stop it, so it stays out of body reads. Status lines still use it: an article's content can't reach them. Part 4's `body_prefix` can read through `read_line_capped` to get the same bound.

## 7. Config

A `postprocess` object in `config.json`, parsed into `Config.postprocess: PostprocessConfig` (one field per key, `Default` gives this table). Absent keys take these defaults, and an absent object means all defaults. No key is written back unless the user adds it. Parts 4–8 keep their own keys in their own top-level objects (`archive`, `naming`, `predb`, `category`, `media`), read in `Step::enabled`.

| Key | Default | Meaning |
|---|---|---|
| `enabled` | `true` | Run the stage. No network use on its own. While off, saves still queue rows. |
| `workers` | `2` | DB lane threads. |
| `settle_secs` | `7200` | Quiet time before an incomplete release is looked at. Floor 1. |
| `complete_settle_secs` | `900` | The same for a complete release. Floor 1. |
| `min_size_bytes` | `0` | An optional rule: smaller is rejected and runs no step. 0 is off. |
| `min_files` | `1` | An optional rule: fewer is rejected. |
| `reject_par2_only` | `true` | Reject a post whose files are all par2. |
| `group_rules` | `{}` | `{"group": {"min_size_bytes": n, "min_files": n}}`; effective value is `max(group, global)`. |
| `backlog_per_second` | `2000` | Existing releases the walks look at, over all shards. 0 stops the walks. |
| `body_fetches_per_minute` | `0` | Body budget. 0 = `Bodies` steps stay off. |
| `body_connections` | `2` | Net lane threads, and bodies in flight at once. |
| `body_min_size_bytes` | `1048576` | A release smaller gets no body. |
| `body_max_size_bytes` | `107374182400` | A release larger gets no body. 0 = no limit. |
| `body_max_bytes` | `16777216` | Largest article read. A larger one is `Got::TooBig` and its connection is dropped. |
| `bodies_per_release` | `8` | Per run of a step. |
| `min_completion_percent` | `95` | Below this a release gets no body, and `--dead` can take it. DB steps run regardless. |
| `dead_after_secs` | `172800` | Quiet time before purge may delete anything but a blocked group's rows. Read only by purge. |
| `max_attempts` | `5` | Then the step's failed bit is set. |

Backoff after attempt `n`: `min(900 * 4^(n-1), 604800)` seconds plus `release_id % 60`, so 15 m, 1 h, 4 h, 16 h.

Constants in `engine.rs`: `DB_BATCH = 256`, `WALK_BATCH = 256`, `NET_BATCH = 4`, `NET_CRASH_RETRY = 900`, `STEP_DEADLINE = 120 s`. In `indexer.rs`: `STAGE_BUDGET_BUSY = 10 ms`, `STAGE_BUDGET_IDLE = 100 ms`. In `purge.rs`: `PURGE_BATCH = 2_000`, `PURGE_TX_ARTICLES = 200_000`, `PURGE_SEGMENT_CHUNK = 20_000`.

## 8. Tasks

Each task: write the test first and see it fail for the stated reason, implement, run, commit. Append whatever attribution the executing session requires to commit messages. All tests use `tempfile::tempdir()`.

### Task 1: The save takes the write lock up front

**Files:** `src/store.rs`

- [ ] **Test first**, in `store.rs`'s tests:

```rust
/// another writer on the shard (purge, AI search, the stage's commands) makes a save wait,
/// not fail: a deferred transaction that read first never gets the busy handler
#[test]
fn a_save_waits_for_another_writer_instead_of_failing() {
    let dir = tempfile::tempdir().unwrap();
    let main = dir.path().join("atlas.db");
    db::create_db_at(&main).unwrap();
    let other = db::open_at(&shard_path(&main, shard_of("alt.binaries.t"))).unwrap();
    other.execute_batch("begin immediate; update meta set value = value where key = 'releases'").unwrap();
    let saver = std::thread::spawn({
        let main = main.clone();
        move || {
            let article = Article { message_id: "<a@x>".into(), filename: Some("a".into()), part: Some(1), total_parts: Some(1), ..Default::default() };
            save(&main, &[Release { name: "Rel".into(), group: "alt.binaries.t".into(), articles: vec![article], ..Default::default() }])
        }
    });
    std::thread::sleep(std::time::Duration::from_millis(300));
    other.execute_batch("commit").unwrap();
    saver.join().unwrap().expect("waited for the other writer");
}
```

- [ ] **Run:** `cargo test --lib a_save_waits`. **Expected before:** panics with "database is locked".
- [ ] **Implement:** `src/store.rs:1358` as in section 4.
- [ ] **Run:** `cargo test --lib a_save_waits && just test`. **Expected:** pass; `tests/save_memory.rs` and the convert tests unchanged.
- [ ] **Commit:** `Have a save wait for another writer on its shard`

### Task 2: Schema registry

**Files:** create `src/postprocess/mod.rs`, `src/postprocess/schema.rs`; modify `src/lib.rs`, `src/store.rs:199-240`; test `tests/pp_schema.rs`.

- [ ] **Tests first:**
  - `an_old_shard_gets_the_new_tables_without_a_rewrite`: `create_shard`, save 3 releases, drop `pp_*` tables and triggers and `name_source`, note the file's page count, call `create_shard` again. All tables, indexes, triggers and the column exist, rows read back the same, and `pages_after - pages_before <= 4 * (tables + indexes in SHARD_TABLES) + 8`, worked out from the registry so later parts need no change. Fails first: no tables.
  - `deleting_a_release_clears_it_from_every_registered_table`: for each `SHARD_TABLES` entry with a `release_key`, `sample_row`, then `delete from releases where id = ?`, and the row is gone.
  - `the_menu_purge_clears_stage_rows`: an incomplete release with `pp_state` and `pp_queue` rows; `store::purge_incomplete`; both empty.
  - `no_index_is_built_on_an_existing_table`: every `sql` from `sqlite_master` with `type = 'index'` created by `migrate` has its `tbl_name` in `SHARD_TABLES`.
  - `a_wipe_leaves_new_empty_tables`: `db::wipe` then `db::create_db_at`; registered tables exist with 0 rows.
- [ ] **Implement** `ShardTable`, `SHARD_TABLES`, `RELEASE_COLUMNS`, `migrate` and `sample_row` (values by declared type: INTEGER 1, TEXT `'x'`, BLOB `x'00'`, REAL 1.0; the key column gets `release_id`). Call `migrate` last in `migrate_shard`.
- [ ] **Run:** `cargo test --test pp_schema && just test`. **Expected:** pass.
- [ ] **Commit:** `Add a registry of shard tables for the processing stage`

### Task 3: Compaction carries everything, or refuses

**Files:** `src/compact.rs`; test in its `mod tests`, and `tests/convert.rs`.

- [ ] **Tests first** in `src/compact.rs`, on `legacy_fixture`:
  - `every_registered_table_and_column_survives_compaction`: in each shard with releases, `sample_row` each registered table for its first release and set `name_source = 7`. `run`. Rows compare equal, every registered index and `gc_*` trigger exists, `name_source` is 7, and the NZBs match. Fails first: rows lost.
  - `a_column_out_of_order_lands_in_the_right_place`: on each shard `alter table releases drop column poster; alter table releases add column poster TEXT; update releases set poster = 'p'`. `run` succeeds and `poster` reads `p`. Fails first: `select *` shifts columns, and the check says "reads back differently".
  - `a_table_this_build_doesnt_know_stops_the_shard`: `create table later_part (x)` in one shard. `run` errors naming `later_part` and "the original was kept", and the table is still there.
  - `collections_is_a_core_table`: `create table if not exists collections (key INTEGER PRIMARY KEY, release_id INTEGER NOT NULL) without rowid` on every shard: `run` succeeds. It is Part 2's to copy, and stays out of `SHARD_TABLES`.
  - `a_new_column_on_files_stops_the_shard`: `alter table files add column extra`. The same error shape as the unknown table.
  - `the_check_counts_every_registered_table`: run `check` on a copy with one `pp_queue` row deleted. The error names `pp_queue`. Fails first: passes.
  - `a_compaction_waits_a_moment_for_a_reader_on_its_way_out`: a reader holds a snapshot; `checkpoint_patiently` with 300 ms patience errors; with the reader letting go after 500 ms and 10 s patience it succeeds. `a_checkpoint_a_reader_holds_back_is_refused` (`:2328`) stays as it is.
- [ ] **Test first** in `tests/convert.rs`: `converted_shards_have_empty_stage_tables`: after converting the `old_database` fixture (`:12`), `pp_queue` and `pp_walks` are empty and `name_source` is NULL on every release.
- [ ] **Implement:**

```rust
// at the first checkpoint (:416), 10 s: a stopping thread may have its last read open for a moment
checkpoint_patiently(&conn, CHECKPOINT_PATIENCE).context("folding the original's WAL in")?;

// after `attach database ? as old` (:435)
refuse_what_wouldnt_be_copied(&conn, shard)?;

// replacing :444-447
let cols = copied_columns(&conn, "releases")?;
conn.execute_batch(&format!(
    "insert into releases ({cols}) select {cols} from old.releases order by id;
     insert or replace into meta select * from old.meta;"
))?;

// after `sealed {sealed} files` (:709), `conn` outside any transaction
for t in crate::postprocess::schema::SHARD_TABLES {
    halt(stop, shard)?;
    for (index, _) in t.indexes {
        conn.execute_batch(&format!("drop index if exists {index}"))?;
    }
    let cols = copied_columns(&conn, t.name)?;
    conn.execute_batch(&format!("insert into main.{0} ({cols}) select {cols} from old.{0}", t.name))?;
}

// the tables compact_shard copies its own way, with their exact columns.
const CORE_TABLES: &[(&str, &str)] = &[
    ("files", FILE_COLUMNS),
    ("segments", "file_id, local, domain, part, bytes"),
    ("domains", "id, suffix"),
    ("held_back", "file_id, message_id, subject, part, total_parts, file_total"),
    ("release_ids", "release_id, id_hash, file_id"),
    ("meta", "key, value"),
    ("collections", "key, release_id"), // Part 2 copies it, with its row filter
];
```

  `copied_columns(conn, table)`: names from `select name from pragma_table_info(?1, 'old')`, each of which must also be in `pragma_table_info(?1, 'main')`, else "`{table}` has a column `{c}` this build doesnt know; the original was kept"; comma joined. `refuse_what_wouldnt_be_copied`: tables from `old.sqlite_master` with `type = 'table'`, excluding `sqlite\_%` and `releases\_fts%` (both with `escape '\'`), must be `releases`, in `CORE_TABLES` or registered. Each core table present must have only its known columns. Add registered names to the check's list at `:944`. `build_shard` at `:724` rebuilds the dropped indexes; `interruptible` already covers the long inserts.
- [ ] **Run:** `cargo test --lib compact && cargo test --test convert && just test`. **Expected:** pass, including every existing compaction test.
- [ ] **Commit:** `Have compaction copy registered tables by name and refuse what it cant carry`

### Task 4: Queue on save, and keep a step's name

**Files:** `src/store.rs`; test in its `mod tests`, and `tests/pp_enqueue_cost.rs`.

- [ ] **Tests first:**
  - `a_save_queues_each_touched_release_and_moves_its_due_time`: save a release with 1 of 3 parts: one `pp_queue` row, `due_at` within 2 s of `now + 7200`. Set `due_at = 1` by hand and save part 2: still one row, `due_at` back near `now + 7200`. Fails first: no rows.
  - `a_complete_release_is_due_sooner`: all parts: `due_at ≈ now + 900`.
  - `a_duplicate_article_does_not_touch_the_queue`: the same article again; `due_at` unchanged.
  - `a_stage_name_survives_a_late_article`: set `display_name`, `is_obfuscated = 0`, `name_source = 40`. Save a late article with `display_name: Some("other")`, `is_obfuscated: true`. Name and flag unchanged. Fails first: both overwritten.
  - `without_a_stage_name_a_save_still_sets_the_name`: behavior at `:1369-1370` when `name_source` is NULL.
- [ ] **Bench, informational:** `#[ignore] enqueue_cost` in `tests/pp_enqueue_cost.rs`: 5 alternating pairs of 100,000 one-article releases saved in batches of 10,000, with `store::SKIP_ENQUEUE_FOR_BENCH` on and off, each on a fresh database. It prints each ratio and the median, and asserts nothing. Put the median in the PR description. By the overview, a number here never stops the part. A scratch run gave 1.017 (0.93 to 1.06).
- [ ] **Implement:** the two statements from section 5, `SETTLE_SECS`, `COMPLETE_SETTLE_SECS`, and `#[doc(hidden)] pub static SKIP_ENQUEUE_FOR_BENCH: AtomicBool`.
- [ ] **Run:** `cargo test --lib store`. **Expected:** pass.
- [ ] **Commit:** `Queue touched releases on save, and leave a step's name alone`

### Task 5: Reading a release's files and segments

**Files:** `src/store.rs`, `src/blob.rs`.

- [ ] **Tests first**, using `sealed_fixture` (`src/store.rs:1957`):
  - `file_segments_reads_a_sealed_file_and_late_rows_as_one_list`: seal `a.rar`, add a late part-2 repost and a part-9 row. `First(2)` and `Parts(vec![9])` give exact message-ids, including the domain-packed one, with no duplicate of the copy.
  - `file_segments_keeps_no_more_than_asked_of_a_huge_loose_file`: 60,000 rows, unsealed. `First(3)`, `Last(3)`; correct parts, under a `#[cfg(test)]` counter of 8 held rows.
  - `a_corrupt_blob_is_an_error_not_an_empty_list`; `release_files_reports_parts_and_sealed`; `blob::count` matches `decode(..).len()` for 0, 1 and 50,000 segments.
- [ ] **Implement** per section 6. Take the snapshot as `:1564` does.
- [ ] **Run:** `cargo test --lib store && cargo test --lib blob`. **Expected:** pass.
- [ ] **Commit:** `Read a sealed file's segments for a step, bounded`

### Task 6: Rules and config

**Files:** create `src/postprocess/rules.rs`; modify `src/config.rs`, `config.example.json`; `src/filters.rs` only if Part 1 lacks the section 6 items.

- [ ] **Tests first:** a table test over `judge_row` and `judge_files` with one row per verdict, including: `Wait` from step 5 and no other; a settled fragment rejected with no `dead_after` wait (step 5's settle is the only clock before it); a fragment whose part is first or last, and a `(1/1)` subject, which are not (Part 1's `what_is_not_a_fragment`); `touched_at` NULL treated as old; `TooSmall` only once `min_size_bytes` or a group value is set, with `max(group, global)`; a one-article 740 KB post, a 10 KB post, an image-only post and an incomplete post all `Pass`; par2-only `Reject(Par2Only)` and a mixed file list `Pass`. Over `body_gate`: below and above the bounds, 0 as no limit, all images `Outside`; 94% `NotYet` and 95% `Open`; a `file_total` above the file count lowering the permille.
  - `over_a_thousand_files_one_article_short_is_not_dead`: a real save of 1,500 files with one article missing and `file_total` 1500: `files` is 1500, `permille >= 950`. Fails with any cap on the read.
  - In `src/config.rs`: `postprocess_defaults_when_absent`, `postprocess_keys_parse`, `a_bad_value_falls_back` (matching `from_value`'s lenient style, `:631`).
- [ ] **Implement.** Add the attribution header to `rules.rs`. Add a `postprocess` block with defaults to `config.example.json`. Add `pub fn load_settings() -> Config` to `src/config.rs`: `read_file()` through `Config::from_value`, or the default. The commands use it, because they need no password and `load_config` asks the keyring (`:430-434`).
- [ ] **Run:** `cargo test --lib rules && cargo test --lib config`. **Expected:** pass.
- [ ] **Commit:** `Port the release rules, and keep size and type out of who gets a step`

### Task 7: Steps, and stage writes on the shard's writer

**Files:** create `src/postprocess/step.rs`, `steps.rs`, `apply.rs`; modify `src/indexer.rs:191,208-211,240-339,346-356`, `src/profile.rs`.

- [ ] **Tests first**, in `src/indexer.rs`:
  - `a_stage_job_runs_on_the_writer_and_answers`: `Db::on_writer(shard, work)`; the receiver gets `Ok`; the row is there.
  - `stage_jobs_dont_hold_up_saves`: queue 500 jobs of 2 ms each, then a save. The save returns with under half the jobs done, counted by the jobs themselves.
  - `queued_stage_jobs_run_before_the_writer_stops`: queue jobs, drop the `Db`, every job answered.
- [ ] **Tests first**, in `src/postprocess/apply.rs`, on a real shard:
  - `a_rename_updates_search_and_clears_later_steps_only`: `releases_fts match` finds the new name; the old `display_name` token is gone; `name_source` is set; only the `rename_clears` bits go.
  - `the_same_name_clears_nothing`; `a_lower_source_does_not_replace_a_higher_one`.
  - `obfuscated_none_leaves_the_flag_and_display_name_none_leaves_the_name`: each sets `name_source`, and a flag change clears nothing. Both None: `name_source` stays too.
  - `a_release_deleted_meanwhile_records_nothing`: `writes` are skipped, no `pp_state` row.
  - `a_bad_write_fails_that_release_alone`: a batch of 3 with bad SQL in the second; 1 and 3 applied, 2 has `db_attempts = 1` and `last_error`, and its own earlier write is undone.
  - `masks_are_deltas`: a concurrent `Stage::rerun` between claim and apply keeps its cleared bit.
  - `a_db_visit_leaves_the_net_columns_alone`: `net_attempts = 3`, `net_next_at = 999`; a DB-lane apply keeps both. The net lane's keeps `db_attempts`.
  - `net_wanted_follows_the_mask`: set while a `net_mask` bit is undone, NULL once done or failed, set again when `clear` takes it back.
  - `an_article_during_a_run_keeps_the_queue_row`: apply with an old `claimed_due` leaves the row, and a retry doesn't overwrite its newer `due_at`.
- [ ] **Tests first**, in `steps.rs`: `steps_have_unique_ids_ranks_names_within_their_ranges`.
- [ ] **Implement** the writer:

```rust
pub(crate) type StageWork = Box<dyn FnOnce(&rusqlite::Connection) -> rusqlite::Result<()> + Send>;
pub(crate) struct StageJob { pub work: StageWork, pub done: std::sync::mpsc::SyncSender<Result<(), String>> }
enum WriterJob { Save(SaveJob), Stage(StageJob) }
impl Db { pub(crate) fn on_writer(&self, shard: usize, work: StageWork) -> anyhow::Result<std::sync::mpsc::Receiver<Result<(), String>>>; }
```

  In `writer`: keep a `VecDeque<StageJob>`, and any `Stage` taken from the channel goes there, including while gathering a save batch. Wait `Duration::ZERO` instead of `idle` while it is non-empty. After a save's replies are sent, run jobs for at most `STAGE_BUDGET_BUSY`; with no save waiting, for at most `STAGE_BUDGET_IDLE`, then `housekeeping` at least once a second. Each job runs in one `BEGIN IMMEDIATE` transaction and adds to `writer_stage_ns`. When the channel closes, run all that remain. `LOAD.writer_queued` keeps counting saves only.

  In `apply.rs`, for each release inside a savepoint (`savepoint one`; on error `rollback to one`, then record the failure as `Retry` would). `Applied` carries `release_id`, `claimed_due: Option<i64>`, `set_done`, `set_failed`, `clear`, `rename_clears`, `net_mask`, `db_mask`, `writes`, `name`, `db_attempts`, `net_attempts`, `net_next_at` (each `Option`: None leaves the column), `parts_seen`, `error`, `queue_at: Option<i64>`, `no_state: bool`.

```sql
select display_name, coalesce(name_source, 0) from releases where id = ?1;   -- none: skip this release
-- each StepWrite: prepare_cached(sql).execute(params)
-- when name.source >= that source and either field is Some. A differing display_name adds rename_clears to clear
update releases set display_name = coalesce(?2, display_name), is_obfuscated = coalesce(?3, is_obfuscated), name_source = ?4
 where id = ?1;
-- unless no_state. ?2 set_done, ?3 set_failed, ?4 clear, ?5 net_mask, ?6 db_attempts, ?7 net_attempts, ?8 net_next_at
insert into pp_state (release_id, done, failed, net_wanted, db_attempts, net_attempts, net_next_at, parts_seen, last_error)
values (?1, ?2 & ~?4, ?3 & ~?4, case when (?5 & ~(?2 & ~?4) & ~(?3 & ~?4)) != 0 then 1 end,
        coalesce(?6, 0), coalesce(?7, 0), coalesce(?8, 0), ?9, ?10)
on conflict(release_id) do update set
    done = (done | ?2) & ~?4,
    failed = (failed | ?3) & ~?4,
    net_wanted = case when (?5 & ~((done | ?2) & ~?4) & ~((failed | ?3) & ~?4)) != 0 then 1 end,
    db_attempts = coalesce(?6, db_attempts),
    net_attempts = coalesce(?7, net_attempts),
    net_next_at = coalesce(?8, net_next_at),
    parts_seen = ?9,
    last_error = ?10;
-- the claimed row, when nothing wants it back
delete from pp_queue where release_id = ?1 and due_at = ?2;
-- queue_at: a wait or a retry. ?3 is claimed_due: an article since the claim keeps its later time
insert into pp_queue (release_id, due_at) values (?1, ?2)
on conflict(release_id) do update set due_at = excluded.due_at where ?3 is null or pp_queue.due_at = ?3;
-- a cleared DB step, as the net lane's rename or rerun leaves it
insert into pp_queue (release_id, due_at) values (?1, ?2) on conflict(release_id) do nothing;
```

  `pp_state` is written when a step ran, a bit changed, or something is net wanted. A visit with none of that writes no row, which is what keeps a skipped row free.
- [ ] **Run:** `cargo test --lib indexer && cargo test --lib postprocess && just test`. **Expected:** pass.
- [ ] **Commit:** `Add the step interface, and run stage writes on the shard writer`

### Task 8: The DB lane and the walks

**Files:** create `src/postprocess/engine.rs`, `src/postprocess/stats.rs`; modify `src/postprocess/mod.rs`; test `tests/pp_stage.rs` (no env, `Stage::start` takes `main`).

**Harness**, for `tests/pp_stage.rs`, `tests/pp_bodies.rs` and `tests/pp_auto_compact.rs`:
- Config: `Config { postprocess: PostprocessConfig { settle_secs: 1, complete_settle_secs: 1, ..Default::default() }, ..Default::default() }`. The body tests add `body_min_size_bytes: 0` and a `body_fetches_per_minute`.
- Steps: an explicit list holding the test's own recording steps, never `steps::all()` (through `run_until`, `set_test_steps`). Later parts add default-on steps to `steps::all()`, and an exact assertion on `done` or on a walk's rows would change meaning. Parts 4–8 do the same, or turn the other parts' steps off in the config they pass.
- Fixtures: complete releases, every part of every file, unless the test is about completeness. Growth is a second complete file added to a release with `file_total` NULL: the same release name, another filename.

- [ ] **Tests first:**
  - `a_settled_release_runs_its_steps_once_in_rank_order`.
  - `incomplete_small_and_image_releases_run_database_steps`: a 1-of-3 post, a 10 KB post and a single `.jpg`: the DB step's bit set on each. Fails on any return of those rejections.
  - `a_fragment_a_blocked_group_and_an_empty_release_run_no_step_and_leave_no_row`.
  - `applies_false_is_not_run_and_not_marked_done`: no bit, and with nothing else to do no `pp_state` row.
  - `a_growing_release_waits_and_a_late_article_requeues_it_after_done`: on growth, `on_growth` steps run again, others do not.
  - `an_article_during_a_run_is_not_lost`: the step blocks on a channel, the test saves an article, releases the step; the release runs again.
  - `a_panicking_step_is_retried_with_backoff_then_failed_and_later_steps_still_run`: `db_attempts` 1, `due_at` about now + 900; forced due; at `max_attempts` the failed bit and `last_error` are set; the rank-next step runs.
  - `a_transient_error_in_the_db_lane_uses_no_attempt_and_no_pause`: `db_attempts` 0, `due_at` about now + 300, other releases in the batch finish, the same run's next step finishes.
  - `a_rename_runs_later_steps_once_on_the_new_name`: rank 200 renames, rank 400 with `on_rename` was already done: it runs in this visit, sees the new name, and no queue row is left for it. A rank 500 net step with `on_rename` gets `net_wanted`. `facts_reach_later_steps`.
  - `a_queue_row_whose_release_is_gone_is_cleared` (the claim's left join).
  - `the_walk_goes_newest_first_writes_nothing_for_rejected_rows_and_ends`: 600 releases and 300 fragments, saved, then `pp_queue` cleared and a walk added. Steps run in descending id; `pp_queue` stays empty throughout; 600 `pp_state` rows; the walk row goes at the end.
  - `a_walk_tick_commits_its_results_with_its_cursor`: a step's write and `before_id` move together; dropping the stage between ticks repeats nothing and loses nothing.
  - `fresh_rows_are_not_held_up_by_a_walk`: a long walk running, a release saved: it finishes within two loops.
  - `a_restart_resumes_the_walk`: stop mid-walk; `before_id` kept; restart continues below it. `a_new_rows_id_above_top_id_is_left_to_the_queue`.
  - `the_first_start_seeds_one_walk`: an existing database with two enabled steps: one `'backlog'` walk, `pp_steps_seen` holds both bits, and a second start adds none. Fails with a second `'steps'` walk.
  - `a_step_turning_on_starts_one_new_walk`: a reload with a new bit adds a `'steps'` walk; the same mask again adds none.
  - `enabled_sees_the_full_config_before_the_first_run`: a step copies a limit in `enabled`; its first `run` sees it with no `set_config`.
- [ ] **Implement:** threads `atlas-pp-db-{i}`; each loops over shards and takes `shard_busy[shard]` (`AtomicBool`), so one batch per shard is in flight and no claim state is written. In a pass over a shard it runs one queue batch, then one walk tick if tokens allow, so neither holds the other up. Per shard, one `db::open_shard` connection with `pragma query_only = on`, opened lazily; every read is a short statement.

  Queue batch:

```sql
select q.release_id, q.due_at, r.id is not null, r.name, r.display_name, r.group_name, coalesce(r.size, 0), coalesce(r.complete, 0),
       coalesce(r.parts, 0), r.file_total, s.release_id is not null, coalesce(s.done, 0), coalesce(s.failed, 0),
       coalesce(s.db_attempts, 0), coalesce(s.parts_seen, 0)
from (select release_id, due_at from pp_queue where due_at <= ?1 order by due_at limit ?2) q
left join releases r on r.id = q.release_id
left join pp_state s on s.release_id = q.release_id;
```

  The other `ReleaseInfo` fields come from the same row; add them to this list. A row whose release is gone: delete it with the batch.

  Walk tick, at most `WALK_BATCH` rows from a token bucket of `backlog_per_second / SHARDS`. Skip it when `LOAD.writer_queued > 2 * SHARDS`:

```sql
select id, before_id, redo_mask, group_name from pp_walks order by id desc limit 1;
select r.id, r.name, r.display_name, r.group_name, coalesce(r.size, 0), coalesce(r.complete, 0), coalesce(r.parts, 0), r.file_total,
       s.release_id is not null, coalesce(s.done, 0), coalesce(s.failed, 0), coalesce(s.db_attempts, 0), coalesce(s.parts_seen, 0)
from releases r left join pp_state s on s.release_id = r.id
where r.id < ?1 order by r.id desc limit ?2;   -- with `and r.group_name = ?3` it uses idx_release_group
```

  A walk visits rows itself and never fills the queue. Its results and `update pp_walks set before_id = ?, seen = seen + ?, ran = ran + ? where id = ?` go in one job. No rows left: `delete from pp_walks where id = ?`.

  `visit`, for a row from either, with `redo` the walk's `redo_mask` (0 for the queue):
  1. `judge_row`, then `file_summaries` and `judge_files`.
  2. `Reject`: count it by reason. A queue row is deleted; a state row has `net_wanted` cleared; a walk row with no state writes nothing.
  3. `Wait(t)`: `queue_at = t`.
  4. `Pass`: `clear = redo`, plus every `on_growth` step when `parts > parts_seen` on a row with state. In memory, `done = row.done & !clear`.
  5. For each enabled applicable `Database` step by rank, not done or failed: run it under `catch_unwind(AssertUnwindSafe(..))`, and act on the result by section 6's table. On `Ok`: set its bit and keep its writes. `rerun` bits of higher-ranked `Database` steps come off `done` in memory, so they run now; `Bodies` and `External` ones go in `clear`. A name that differs from the current one: higher-ranked `on_rename` `Database` steps come off `done` likewise and later steps see the new name; net ones go in `rename_clears`. A bit cleared and run again in this visit is left out of the final `clear`.
  6. For each enabled applicable net step, not done or failed: `Bodies` with `body_gate` `Outside`: set its bit, counted as `matched: false`. `NotYet`: nothing. Otherwise add it to `net_mask`.
  7. Build one `Applied`. Send the batch via `Db::on_writer`, wait for the answer, then take the next.

  `Stage::rerun`: `update pp_state set done = done & ~:m, failed = failed & ~:m, db_attempts = 0, net_attempts = 0, net_next_at = 0 where release_id = :id`, then `insert into pp_queue ... on conflict(release_id) do update set due_at = excluded.due_at` with `due_at = now`, through the writer. A walk's `redo_mask` does the same through `clear`.

  At `Stage::start`, once per shard, through the writer. This is the only write when `pp_walk_seeded` is absent, because the backlog walk visits every row and needs no `'steps'` walk beside it:

```sql
insert into pp_walks (kind, top_id, before_id, created_at)
select 'backlog', t, t, ?1 from (select coalesce(max(id), -1) + 1 as t from releases)
where not exists (select 1 from meta where key = 'pp_walk_seeded');
insert or ignore into meta (key, value) values ('pp_walk_seeded', ?1);
insert or replace into meta (key, value) values ('pp_steps_seen', ?2);  -- ?2: the enabled mask, when seeding
```

  Later, compare the enabled mask (a `Bodies` step counts only with a budget) with `meta.pp_steps_seen` at start and on `set_config`: new bits insert a `'steps'` walk from `max(id) + 1`, and the mask is stored.
- [ ] **Run:** `cargo test --test pp_stage && just test`. **Expected:** pass.
- [ ] **Commit:** `Run database steps with retries, and walk the backlog newest first`

### Task 9: Background bodies from the shared pool

**Files:** `src/nntp.rs`; test `tests/background_body.rs`.

- [ ] **Tests first**, on the mock server (`tests/common/mod.rs`):
  - `it_waits_while_indexing_needs_the_connections`: 4 connections, 3 XOVERs stalled (`stall`, `stalls_left`). `background_body` sends no BODY during the stall (`bodies_sent == 0` after 400 ms) and one after.
  - `it_doesnt_take_a_connection_someone_waits_for`: a waiter queued, the same result.
  - `past_its_patience_it_queues_normally`: `with_body_patience(200 ms)`, all connections busy; it gets one once a request ends.
  - `every_server_saying_no_is_missing` (`bodies: false`); `a_server_that_cant_be_reached_is_an_error_not_missing` (a dead second server, article absent on the first).
  - `one_enormous_line_is_cut_at_the_cap_and_its_connection_goes`: a one-line body of 8 MiB with a 1 MiB cap: `TooBig`, `server_stats` `wire_bytes` under 1 MiB + 200 KiB, and the next request's `accepted` rises. Fails on `read_raw_line`.
  - `many_tiny_lines_are_too_big`: 200,000 one-byte lines with a 1 MiB cap: `TooBig`.
  - `a_small_article_comes_back_decoded_and_its_connection_stays`.
- [ ] **Implement:** `read_line_capped` over `fill_buf` and `consume`, taking up to the newline from each read and adding to `wire` and `text`; `body_capped` as in section 6. A status line saying `COMPRESS=GZIP` on a `BODY` is `None`: only listings get compressed, and a compressed one can't be bounded by its lines. Past the cap `Lease` stays busy so the connection is dropped, the pattern at `:1548-1550`. Move `lease_for`'s post-permit code into `connect_with_permit(&self, server, permit, group, force) -> Result<Option<Lease>>`, where None replaces its `continue` (`:1354`). `background_lease(i)`: `!is_down(i) && waiting == 0 && available_permits() > limit / 4`, then `try_acquire_owned`. `background_body`: order as in `fetch_body`; a round tries each server left, sleeps 250 ms if any had no room; past `body_patience` (`BODY_PATIENCE`, set by `with_body_patience`) it falls back to `lease`. A `Reply` moves to the next server and an error with no code is remembered: Err at the end if any, else `Missing`. `Load` gains `bg_bodies`, `bg_body_bytes`.
- [ ] **Run:** `cargo test --test background_body && just test`. **Expected:** pass.
- [ ] **Commit:** `Fetch bodies for background work without queuing ahead of indexing`

### Task 10: The net lane and the body budget

**Files:** create `src/postprocess/bodies.rs`; modify `engine.rs`; test `tests/pp_bodies.rs`, on Task 8's harness.

- [ ] **Tests first:**
  - `no_budget_no_bodies`: default config, a `Bodies` test step, a saved release. The stage runs; `bodies_sent == 0`; the step never runs; `net_wanted` stays NULL. This is the rule 8 test.
  - `the_budget_caps_fetches_per_minute`: 60 per minute, 10 releases wanting 8 bodies; no more than about 1 per second over 3 s.
  - `outside_the_bounds_a_body_step_is_done_and_unmatched`: `body_min_size_bytes` 1 MiB, a 10 KB release; and `body_max_size_bytes` 1 KiB, a 10 KB release: done bit set, `net_wanted` NULL, step never ran.
  - `an_image_only_release_stays_out_of_the_net_lane`: every filename an image: the same, with the DB step's bit also set.
  - `an_incomplete_release_gets_no_body_until_a_late_article_opens_it`: 1 of 3 parts: no body bit, `net_wanted` NULL, DB step ran. Save the rest: `net_wanted` 1, the step runs.
  - `the_gate_never_marks_a_step_whose_applies_is_false`: a `Bodies` step with `applies` false on a release below the bounds: no bit.
  - `fetch_bodies_refuses_before_any_bytes_move`: a step calling it directly gets `Got::Refused` for a blocked group, a release outside the bounds, an image file and a request past `bodies_per_release`; `bodies_sent == 0`.
  - `a_server_outage_uses_no_attempts`: unreachable server; `net_attempts` stays 0, `net_next_at` moves, the lane pauses; after the server returns the step finishes.
  - `wait_until_gives_back_the_attempt_begin_counted`: a step returning `wait_until` during a run: `net_attempts` is back to its old value, `net_next_at` is that time, no bit. Fails with `net_attempts` 1.
  - `a_missing_article_is_for_the_step_to_judge`: `Got::Missing`; the step returns done with `matched: false`.
  - `a_crash_mid_step_counts_one_attempt_and_retries_later`: run the `Begin` write, drop the stage without applying, restart with `net_next_at` forced past; `net_attempts` is 1, then the step finishes and it is 0. Fails without the write-ahead.
  - `a_db_visit_during_a_net_step_keeps_its_count`: `net_attempts` 2; a late article's DB visit: still 2.
  - `a_net_step_that_renames_requeues_later_database_steps`; `newest_goes_first_and_every_eighth_claim_takes_the_oldest`.
  - `a_slow_net_step_doesnt_hold_up_the_db_lane`: the net step blocks; DB-lane releases finish.
- [ ] **Implement:** `BodyFetcher { budget: Mutex<TokenBucket>, in_flight: Semaphore, cfg }`, with `handle.block_on(unless_stopped(&halt, pool.background_body(..)))`. A private `refusal(&self, release, wanted, used) -> Option<&'static str>` holds the checks in order: blocked group, release outside the bounds, image name, per-release count. Part 4's `fetch_prefix` calls it too. `fetch_bodies`: stopping, `refusal` (`Got::Refused`), then a token, waiting up to the run's deadline before `OutOfBudget`, then a permit.

  Net lane threads `atlas-pp-net-{i}`, count `body_connections` when any `Bodies` or `External` step is enabled, one batch per shard:

```sql
select release_id from pp_state where net_wanted = 1 and net_next_at <= ?1 order by release_id desc limit ?2;
-- every 8th claim: order by release_id asc
```

  For each: `judge_row`, `file_summaries`, `judge_files` again, and `body_gate`. `Reject`, or no step left: `net_wanted` cleared. `Begin` through the writer: `update pp_state set net_attempts = net_attempts + 1, net_next_at = :now + 900 where release_id = :id`. Run the lowest-rank undone enabled applicable net step, whose lane's result follows section 6's table, then `Applied`, with `db_attempts` None. `net_mask` is the undone net steps left, plus any `clear` or `rename_clears` net bits.
- [ ] **Run:** `cargo test --test pp_bodies && just test`. **Expected:** pass.
- [ ] **Commit:** `Run body and outside steps within a budget, without using up retries on an outage`

### Task 11: Run it inside the indexer

**Files:** `src/bg_indexer.rs`, `src/postprocess/mod.rs`, `stats.rs`, `tests/common/mod.rs`; test `tests/pp_auto_compact.rs` (one test, env set first, as `tests/auto_compact.rs`).

- [ ] **Test first:** `the_stage_stops_for_a_compaction_and_carries_on`: a mock server, `auto_run_compact: true`, `ATLAS_COMPACT_EVERY_SECS=2`, `postprocess.settle_secs: 1`, `complete_settle_secs: 1`, `set_test_steps(a recording DB step that sleeps 100 ms per run)`. Wait for `last_compact`, every one of the 8 shards rewritten (`common::shard_inodes`, as `tests/auto_compact.rs:67`), `pp_state` rows from before compaction still there, and new rows after it. `status.json` has `postprocess_backlog`; `stats.json` has `postprocess.queue_due` and `postprocess.walk`. Fails first: no stage, no fields. Any thread still holding a read fails this as "N of 8 shards were not compacted", so Parts 6 and 8 assert `shard_inodes` the same way with their own threads running (`predb.enabled` with a scripted fetch, `media.parse` with a fake provider).
- [ ] **Test first** in `src/bg_indexer.rs`: `a_stage_turning_on_or_off_rebuilds`: `postprocess.enabled` changing asks for a rebuild.
- [ ] **Implement:**
  - In `run_servers`, after `:751`: `Stage::start(..)` when `config.postprocess.enabled`, with the test steps if set, else `steps::all()`, and `config.clone()`. Then `tasks.spawn(postprocess::run(stage, move || s.stopping()))`.
  - `run` naps 250 ms, refreshes counts every 30 s, and on stop calls `stop_and_join` through `spawn_blocking`. All stage threads and connections are gone before `tasks` drains.
  - Bounded counts only:

```sql
select count(*) from (select 1 from pp_queue where due_at <= ?1 limit 100001);
select count(*) from (select 1 from pp_state where net_wanted = 1 limit 1000001);
select count(*) from (select 1 from pp_state where failed != 0 limit 100001);
select coalesce(sum(top_id - before_id), 0), coalesce(sum(top_id), 0) from pp_walks;
```

  - `Scheduler::write_status` inserts `stats.extra["postprocess"]`. `write_status` reads `stats::backlog()`, an atomic. The unit-test `scheduler()` (`:1053`) gets `stage: OnceLock::new()`.
- [ ] **Run:** `cargo test --test pp_auto_compact && cargo test --test auto_compact && cargo test --test compact_lock && just test`. **Expected:** pass.
- [ ] **Commit:** `Run the processing stage inside the indexer`

### Task 12: Dashboard page

**Files:** `src/stats_dashboard.rs`.

- [ ] **Test first:** extend the page loop at `:1506` with a fixture `stats.postprocess` and `5 => assert!(text.contains("walk") && text.contains("due now"))`. Fails first: no page.
- [ ] **Implement** page 6, `draw_postprocess`, reading only `stats.json`: backlog (due now, `100k+` when capped; waiting for network; failed); walks (progress % from ids, rows/s, ETA by `human_time`); rates over `LOAD_WINDOW` (steps done, matched, no match, retries); rule rejections by reason; bodies/s and bytes/s against the budget; writer time spent on stage work. Keys `1-6`.
- [ ] **Run:** `cargo test --lib stats_dashboard`. **Expected:** pass.
- [ ] **Commit:** `Show the processing stage's backlog on the stats dashboard`

### Task 13: `atlas --purge`

**Files:** create `src/postprocess/purge.rs`, `src/postprocess/batch.rs`; modify `src/main.rs`, `src/store.rs` (`delete_release`, `too_many_incomplete`), `src/blob.rs`, `src/db.rs:446`, `tests/common/mod.rs`; test `tests/pp_purge.rs` through `CARGO_BIN_EXE_atlas`, as `tests/compact_command.rs`.

Usage: `atlas --purge [--fragments] [--blocked] [--too-small [--below BYTES]] [--image-only] [--rejected] [--dead] [--sample N] [--apply]`. With no class flag: `--fragments` only. Exit 0 on success or nothing to do, 1 when refused or failed.

| Class | Takes | Age needed |
|---|---|---|
| `--fragments` | `stored_fragment` | `dead_after_secs` |
| `--blocked` | `group_blocked`, listed by group in the dry run | None: never indexed or served again anyway |
| `--too-small` | `size` under `--below`, default `body_min_size_bytes`, with a size histogram in the dry run | `dead_after_secs` |
| `--image-only` | Every filename an image | `dead_after_secs` |
| `--rejected` | `Reject(Par2Only)`, `Reject(TooFewFiles)`, `Reject(Empty)`. Neither too small nor image only | `dead_after_secs` |
| `--dead` | `permille < min_completion_percent * 10` | `dead_after_secs` |

A release counts once, in the first class above that is selected and matches.

- [ ] **Tests first**, with `common::fake_indexer` added to `tests/common/mod.rs`:

```rust
/// A process `procs::indexer_alive` takes for the indexer: `sysinfo` must see `--bg-indexer` in
/// its arguments. `sleep 60; true`, because `sh -c 'sleep 60'` execs straight into sleep and
/// loses it (tried). Killed when dropped.
#[cfg(unix)]
pub fn fake_indexer(home: &std::path::Path) -> FakeIndexer {
    let child = std::process::Command::new("sh").args(["-c", "sleep 60; true", "--bg-indexer"]).spawn().unwrap();
    std::fs::write(home.join("bg_indexer.pid"), child.id().to_string()).unwrap();
    FakeIndexer(child)
}
```

  - `a_dry_run_changes_nothing_and_says_what_it_would_do`: per class counts in stdout, `--sample 2` prints two names per class, every table's count and `meta` unchanged, no `purge_*` key, and it works while another handle holds the shared guard.
  - `no_flag_means_fragments_only`: fragment, blocked, tiny, image, par2-only, dead and good releases, some sealed. Apply takes the fragment alone. Each flag then takes its own class. `--rejected` leaves the tiny and the image.
  - `a_fresh_post_is_not_purged`: all classes but `--blocked` wait `dead_after_secs` from the last touch.
  - `the_dry_run_names_blocked_groups_and_sizes_small_ones`: `--blocked` prints each group with its release count and bytes. `--too-small` prints counts at or under 64 KiB, 256 KiB and 1 MiB.
  - `search_and_totals_are_right_after_it`: `releases_fts` finds no deleted name; `meta.releases` and `meta.articles` equal a recount by `store::article_count`; no row in `files`, `segments`, `held_back`, `release_ids`, or any registered table for a deleted id; the NZBs of survivors are unchanged.
  - `a_stopped_purge_resumes_without_counting_twice`: test env `ATLAS_PURGE_STOP_AFTER=2` commits two batches and exits; a second run finishes; totals still correct; `purge_after` key gone at the end.
  - `other_classes_start_over`: a stopped run, then different flags: the cursor resets and it says so.
  - `a_huge_release_goes_in_chunks_and_is_finished_first`: 5,000 loose rows with test env `ATLAS_PURGE_CHUNK=1000` and `ATLAS_PURGE_STOP_AFTER=2`: `purge_victim` is set, and `meta.articles` equals `article_count` at the stop. Run again with `--dead` alone: the victim is gone though `--dead` wouldn't take it, the key is gone, and totals equal a recount. Fails without the marker.
  - `big_batches_commit_in_pieces`: 6 releases of 90,000 articles each in one batch: at least 3 commits, totals 0.
  - `a_reader_holding_the_wal_stops_it_cleanly`: test env `ATLAS_PURGE_CHECK_EVERY=1`, `ATLAS_PURGE_WAL_LIMIT=1`, a second connection holding a read snapshot: exit 1, the message says a reader is holding the WAL, the committed cursor is there. With the reader gone, a second run finishes.
  - `apply_is_refused_while_anything_holds_the_lock`: `hold_off_compaction` held; exit 1; nothing changed. `apply_is_refused_with_an_indexer_alive`: `fake_indexer`; exit 1, "stop indexing first".
  - `then_compact_shrinks_it`: on 2,000 releases, `--compact` after apply succeeds, and the shards' total bytes fall.
  - In `src/db.rs` tests: `the_menu_purge_refuses_a_huge_job`: `store::too_many_incomplete(&main, 2)` with 3 incomplete rows; `purge_broken` errors naming `--purge --dead` past `PURGE_BROKEN_MAX`, which is 1,000,000.
- [ ] **Implement:**
  - `config::load_settings` for rules and classes.
  - Dry run: `try_hold_off_compaction`, `None` refuses; drop the guard; read-only connections; a `purge_victim` left behind is reported, not touched.
  - Apply: refuse on `procs::indexer_alive()` ("stop indexing first"), then `compact::Lock::take` (message on `Busy`), `refuse_unresolved_backups`, refuse when `convert::needed`, signal flag as `src/bg_indexer.rs:968-972`.
  - Per shard, 4 at once (`std::thread::scope`, as `src/compact.rs:345`). `db::open_shard`, `tune_for_writing`. First, a `purge_victim` is finished, whatever the classes. Then:

```sql
-- at the start, or when purge_classes differs (saying so)
insert or replace into meta (key, value) values ('purge_top', ?);      -- max(id) now
insert or replace into meta (key, value) values ('purge_classes', ?);
-- a batch, outside any transaction. file_summaries only when a selected class needs files
select id, name, group_name, coalesce(size, 0), coalesce(parts, 0), coalesce(complete, 0) != 0, file_total from releases
 where id > :cursor and id <= :top order by id limit 2000;
-- victims, in BEGIN IMMEDIATE transactions of at most PURGE_TX_ARTICLES by `parts`:
select id, blob from files where release_id = ?;  -- blob::count, plus what the next line deletes, is its articles
delete from segments where file_id = ?;
delete from held_back where file_id = ?;
delete from release_ids where release_id = ?;
delete from files where release_id = ?;
delete from releases where id = ?;                -- fires releases_ad (FTS) and gc_*
update meta set value = max(value - ?, 0) where key = 'releases';
update meta set value = max(value - ?, 0) where key = 'articles';
insert or replace into meta (key, value) values ('purge_after', ?);
-- a victim with parts > PURGE_SEGMENT_CHUNK: first its own commit of the marker, then chunks in
-- their own transactions, each subtracting from `articles`, and delete_release in the last
insert or replace into meta (key, value) values ('purge_victim', ?);
delete from segments where (file_id, local, domain) in
    (select file_id, local, domain from segments where file_id = ?1 limit ?2);
delete from meta where key = 'purge_victim';      -- with the release's own delete
```

  - `batch::checkpoint_or_stop` every `COMMITS_PER_CHECKPOINT` commits; progress every 1M rows; at the end delete the `purge_*` keys and `pragma wal_checkpoint(truncate)`. No vacuum. The last line says to run `atlas --compact`.
  - `src/db.rs:446` `purge_broken`: after the guard, `store::too_many_incomplete(&main, PURGE_BROKEN_MAX)` (`select count(*) from (select 1 from releases where complete = 0 limit ?)` per shard) errors with the batched command. `src/app.rs:527` already shows the error.
- [ ] **Run:** `cargo test --test pp_purge && cargo test --lib db && just test`. **Expected:** pass.
- [ ] **Commit:** `Add atlas --purge, dry run by default, resumable and in small transactions`

### Task 14: `atlas --reprocess`

**Files:** create `src/postprocess/reprocess.rs`; modify `src/main.rs`; test `tests/pp_reprocess.rs`.

Usage: `atlas --reprocess [--step NAME]... [--group NAME]... [--failed] [--apply]`. `--step all` is every step. No `--step`: judge again only, clearing nothing.

- [ ] **Tests first:**
  - `a_dry_run_prints_counts_and_changes_nothing`; `an_unknown_step_is_refused_with_the_list`.
  - `apply_adds_one_walk_per_shard_and_no_more_on_a_second_run`: same `(kind, redo_mask, group_name)` is not added twice; it says "already queued".
  - `the_walk_clears_the_mask_and_the_step_runs_again`: with a stage running a test step. `group_limits_it_to_that_groups_shard_and_rows`; `failed_clears_failed_bits_and_retries_now`.
  - `it_runs_beside_a_writer` (shared guard held by another handle: succeeds); `it_is_refused_during_a_compaction` (exclusive lock held).
- [ ] **Implement:**
  - Dry run: `select count(*) from pp_state where done & :m != 0` per shard (joined to `releases` for `--group`), printed with time at the current `backlog_per_second`.
  - Apply: `compact::hold_off_compaction`, then per shard, in one `BEGIN IMMEDIATE` transaction, `reprocess::apply`. Part 5 adds its `meta.naming_rename_named` statement to that function, inside this transaction, when `mask` includes `NAMING`:

```sql
insert into pp_walks (kind, top_id, before_id, redo_mask, group_name, created_at)
select 'reprocess', t, t, ?1, ?2, ?3 from (select coalesce(max(id), -1) + 1 as t from releases)
where not exists (select 1 from pp_walks where kind = 'reprocess' and redo_mask = ?1 and group_name is ?2);
-- --failed, by the partial index, 1,000 ids per transaction
-- (select release_id from pp_state where failed != 0 limit 1000), each:
update pp_state set failed = 0, db_attempts = 0, net_attempts = 0, net_next_at = 0, last_error = null where release_id = ?1;
insert into pp_queue (release_id, due_at) values (?1, ?2) on conflict(release_id) do update set due_at = excluded.due_at;
```

  - The DB lane then works out `net_wanted` again. Renames during the re-run go through the normal apply path, so FTS follows; `meta` totals do not change.
- [ ] **Run:** `cargo test --test pp_reprocess && just test`. **Expected:** pass.
- [ ] **Commit:** `Add atlas --reprocess, dry run by default`

### Task 15: Docs and full verification

**Files:** `README.md`, `config.example.json`, `src/main.rs` help.

- [ ] **Test first:** in `tests/compact_command.rs`'s style: `help_lists_purge_and_reprocess`. Fails until the help text changes.
- [ ] **Implement** section 12.
- [ ] **Run:** `just format-check && just lint && just test`, then the checks in section 11. **Expected:** all pass.
- [ ] **Commit:** `Document the processing stage`

## 9. Performance and load

Measured means a scratch export of `de598b6` running the code in this plan, on synthetic rows only, in a release build without LTO at `nice 15`, beside the live `atlas --compact`. Its shards were small and hot, so its rates are best cases: that run saved 170,000 articles/s where the live indexer peaks at 3,750/s per shard. Cold figures are derived. None of this touched live data.

| Measured | Result |
|---|---|
| Enqueue in a real save, median of 5 alternating pairs | 1.017 times the save without it (0.93 to 1.06) |
| `pp_queue`, row and index | 31 B/row, 0.4 µs to insert, 0.4 µs to delete |
| `pp_state` written in id order (fresh rows, or after any compaction) | 25.2 B/row |
| `pp_state` written newest first (a walk) | 50.2 B/row: inserting downward leaves pages half full |
| A one-article release, compacted | 441 B in all, with short names |
| Walk read per row: `releases`, `pp_state` join, `files` | 0.13–0.15 page misses at a 64-page cache, 2–4 µs |
| Apply, a state row / a state row and a rename with FTS | 2.2 µs / 19 µs |
| Purge: dry run / apply | 2.4 µs per row / 38,000 rows/s |
| Compaction | 400,000 releases in one shard in 33 s |

**How many rows.** Rules now reject only fragments, blocked groups, empty rows and par2-only posts. The overview's sample puts fragments at up to 29%, about 34M, and Part 1's predicate misses some, so 84–100M of the 118M rows run database steps. An earlier version of this plan had about 16M here.

**Enqueue at ingest rate.** Peak 30,000 articles/s over 8 writers is 3,750/s per writer, 266 µs of writer time per article. In the worst case every article is a new release, adding one row and one index entry near the right edge. The measured ratio puts that at about 2% of a save, and the queue's cost doesn't grow with the shard while the rest of a save does, so the share falls on a live shard. WAL: 31 B per row, at most 80,000 rows per transaction (`MAX_BATCH` 8 × `request_size` 10,000), so 2.5 MB on top of a transaction that already writes hundreds of MB.

**Queue depth.** A row waits only for settling: nothing is kept for `dead_after`, which would have held 648M rows and 20 GB per shard. Sustained peak with all incomplete releases: 3,750/s × 7,200 s = 27M rows, 840 MB per shard. All complete: 3.4M rows, 105 MB per shard. Pages are reused as it drains, and Part 1's filter removes much of this inflow. Past that the depth is whatever the stage is behind by, at the same 31 B a row.

**Keeping up with new releases.** Each fresh row now runs steps, so this is the DB lane's real limit, not its reads. A visit's writes are about 100 µs on a cold shard: a state row, a queue delete, a side row or two, and a rename's FTS update. `STAGE_BUDGET_BUSY` of 10 ms per save cycle of about 300 ms is about 100 rows per cycle, about 2,700 rows/s over 8 writers, at 3.3% of writer time. Above that during a peak, the stage falls behind and saves don't slow; it catches up on `STAGE_BUDGET_IDLE`, about 1,000 rows per 100 ms tick. Two threads at up to the 200 µs a row of steps below give at least 10,000 rows/s of CPU, so the writer budget binds first.

**`pp_state` at 84–100M rows.** A row per release stays:

| | 100M rows | Share of 300 GB |
|---|---:|---:|
| During and after the walk, until a compaction | 5.0 GB | 1.7% |
| After `--compact` | 2.5 GB | 0.8% |

It costs about what one part's own per-release table costs. Avoiding it would mean treating "no row" as "done", which loses the bits that reruns, `on_growth` and `--reprocess --step` work from, and every touch would run every step again. A row is written only when a step ran or a bit changed, so a step with `applies` false on a row costs nothing, and Part 5's skips reduce this. The `updated_at` column an earlier version had is gone: it was 5 B a row, 500 MB here, for nothing that read it. Parts' own tables filled by a walk are also inserted newest first, so they too sit at about twice their compacted size until a compaction.

**The backlog of 118M.** Rows go through the walks and never fill the queue.

| Phase | Rate | Time at 118M | Load |
|---|---|---|---:|
| Walk: judge and database steps, newest first | 2,000 rows/s (`backlog_per_second`) | 16.4 h | See below |
| Body steps at 300/min (5/s, 3.75 MB/s at 750 KB) | 3 bodies a release: 144,000 releases/day | Newest 1M in 7 days; 15.4M in 107 days; every multi-article row, 37.6M, in 261 days | 2 connections |
| Body steps at 6,000/min (100/s, 75 MB/s) | 2.9M releases/day | 15.4M in 5 days; 37.6M in 13 days | Set `body_connections` to about 20 |

Body candidates are what pass the gate: at most the 37.6M multi-article rows, and 15.4M if the sample's image share holds (Part 4 section 9). The one-article rows are under 1 MiB and get none.

Cost of the walk, per row at 2,000 rows/s:

- **Disk, measured shape.** Rows and their files are stored in id order, so a walk reads pages in order: about 0.15 pages a row at 2,000 rows/s is 1.2 MB/s, 71 GB over the walk. The shards the live indexer has written keep that order per shard, but a late second file sits apart from its release.
- **Disk, worst case, with no such order.** A `files` seek is an index page and a table page, and a `pp_state` probe one more: 3 random reads a row, 6,000 reads/s and 24 MB/s. At 100 µs a read on an SSD that is 60% of one thread and a real share of the disk's queue beside the indexer's own 8 × 256 MB of caches. A disk that can't do it slows the walk rather than the indexer: tokens are a ceiling, and the dashboard's ETA uses measured rows/s. `backlog_per_second` turns it down.
- **CPU.** Read and judge: 4 µs. Steps: Parts 5–8 put theirs at up to about 200 µs a row together, and `applies` skips most of that on many rows: at most 40% of a core for 16.4 h. Rejected rows write nothing and take only the read.
- **Writer.** 250 rows/s per writer at about 100 µs cold is 2.5% of a writer. WAL: under 1 MB/s with no renames, up to 8 MB/s if every row renames (Part 5 section 9).
- **New releases.** They use `pp_queue`, and each pass over a shard takes a queue batch before a walk tick. A walk also pauses whenever `writer_queued` exceeds `2 * SHARDS`.
- **NNTP.** If indexing holds every connection, a background body waits up to 20 s then takes its normal place, adding about one request per 10 s per net thread. Network use with the default config is zero.

**Purge at 118M.** Reads go 2,000 rows at a time, so they never pin the WAL. Dry run: 118M × 2.4 µs hot to 100 µs cold ÷ 4 shards at once = 1–50 min. Apply: the 38,000 rows/s measured is a hot small shard. Each delete touches about the pages a save of one article does, including a random page of `idx_release_unique`, so plan on 1,000–4,000 rows/s per cold 40 GB shard, 4,000–16,000 rows/s over 4.

| Class | Rows (overview's sample) | Deletes take |
|---|---:|---:|
| `--fragments`, the default | up to 34M | 35 min–2.4 h |
| `--too-small` under 1 MiB | up to 80M | 1.4–5.6 h |
| `--image-only` | up to 68M | 1.2–4.7 h |

Each transaction holds at most 2,000 deletes and 200,000 articles; a crash loses that one. Memory: 4 connections × 256 MB cache (`src/db.rs:75`) ≈ 1 GB. WAL: restarted every 64 commits, and the run stops rather than pass 4 GiB if a reader holds it.

**How space comes back.** Deletes free SQLite pages; the file does not shrink, and new inserts reuse them. Only `atlas --compact` returns space, by writing fresh copies and rebuilding the search index (`src/compact.rs:715`). At 441 B measured to about 1 KB with real names, purging 34M rows frees 15–34 GB. For time: 400,000 releases took 33 s hot, 48,000 rows/s over 4 shards, 41 min at 118M; the README's 488 GB conversion in 1.5 h (`README.md:265`) is about 90 MB/s, about an hour for 300 GB; and the check decodes every blob up to three more times (`src/compact.rs:439,951`). Plan on 1–2 h after a purge, 2–3 h at the full 118M. It needs free disk equal to the new total, because originals stay as `.precompact.db` until every shard is done (`src/compact.rs:13-16,362-372`). The indexer must be stopped for both.

**Memory while running.** Stage threads hold at most one batch per shard: about 2 MB of pending writes. Bodies: `body_connections` × 6 × `body_max_bytes` = 192 MiB at the worst, for hostile articles of tiny lines; about 1.5 MB in practice.

## 10. Failure handling

| Case | What happens |
|---|---|
| Bad input to a step (garbled subject, corrupt archive) | The step returns done with `matched: false`, or `Permanent`. A panic is caught per release and counts as `Retry`. |
| Corrupt blob | `file_segments` errors. `Retry`, then the failed bit. The release stays searchable; compaction reports the same file (`src/compact.rs:825`). |
| A `StepWrite` fails (a part's bad SQL, a constraint) | Savepoint rollback for that release only; `db_attempts + 1`, `last_error`, backoff. The rest of the batch commits. |
| Server down, refused login, timeouts | `Transient`: no attempt used, `net_next_at = now + 300`, net lane pauses 60 s. DB lane and indexing carry on. A failed connection never marks work done. |
| A database step can't reach something for now | `Transient` in the DB lane: `due_at = now + 300`, no attempt, no pause, and the run's other steps finish. |
| Article gone (430 on every server) | `Got::Missing`; the step decides. No server marked down. |
| Body over cap, or one enormous line | `Got::TooBig`, at most the cap and 64 KiB read, connection dropped. The next request logs in again, and a refused login rests that server for 30 minutes (`src/nntp.rs:128,1356`). Articles past 16 MiB are rare and the budget paces the rest; Part 4 paces its deliberate hang-ups itself. |
| Crash during a DB batch | Nothing was written: queue rows remain and run again. Steps must be idempotent (writes use `insert or replace`). |
| Crash during a walk tick | Results and cursor are one job: the tick runs again. |
| Crash during a net step | `Begin` already counted the attempt and pushed `net_next_at` 15 min out, so a poison release cannot crash-loop past `max_attempts`. A DB visit meanwhile doesn't touch that count. |
| Clean stop | `Stopped` gives the attempt back. Stage threads end, then the writers drain stage jobs, then the shards close. |
| A thread still reading as a compaction starts | `checkpoint_patiently` waits up to 10 s, then that shard keeps its original and the error names it. Safe, but `tests/pp_auto_compact.rs` exists so it doesn't happen. |
| Writer error on a stage job | The lane logs it, `error_count + 1`, naps 5 s, re-reads. The queue row is still there. |
| Release deleted while a step runs | Apply finds no row and skips all writes. |
| Article arrives during a run | `due_at` moved, apply's delete matches nothing, and the release runs again after settling. |
| Purge crash or Ctrl-C | The cursor commits with each batch. Run it again. |
| Purge stopped inside a huge release | `purge_victim` says which. The next run finishes that release first, whatever its classes, so nothing stays half deleted. |
| Purge with different classes than a stopped run | Cursor restarts from 0, and it says so. Rows already gone stay gone. |
| A reader holds the WAL during a purge | Past 4 GiB the run stops with that message and exit 1. Close the reader, run again. |
| Re-running reprocess | No duplicate walk. A walk survives restart and compaction. |
| Compaction meets a table or column it cannot carry | That shard stops, the original is kept, the error names it. |
| Stage disabled | Saves keep queuing rows. Turning it on drains them. |

## 11. Acceptance checks

Run in the part's worktree. None touch live data or a live server. The enqueue bench is not one of them: by the overview's rule its number goes in the PR description.

```bash
just format-check && just lint && just test
# expected: all pass, no warnings

! grep -n 'select \* from old.releases' src/compact.rs
# expected: no output, exit 0

! grep -nE 'TooBig|ImageOnly' src/postprocess/rules.rs
# expected: no output, exit 0: neither size nor type rejects

cargo test --lib a_save_waits
# expected: pass, a save waits out another writer

HOME_DIR=$(mktemp -d)
ATLAS_HOME=$HOME_DIR cargo run --quiet -- --help | grep -E -- '--purge|--reprocess'
# expected: both listed

cargo test --test pp_purge
# expected: pass: no flag means fragments only, a dry run changes nothing, totals equal a recount

cargo test --test pp_stage incomplete_small_and_image_releases_run_database_steps
# expected: pass

cargo test --test pp_bodies no_budget_no_bodies
# expected: pass, bodies_sent == 0 with the default config

cargo test --test background_body one_enormous_line
# expected: pass, the cap holds on one long line

cargo test --test pp_auto_compact
# expected: pass, all 8 shards rewritten, stage rows survive and the stage resumes

cargo test --test pp_schema
# expected: pass; adding a table to SHARD_TABLES needs no change to this test
```

## 12. Docs

- `README.md`, "Top-level keys" (`:127`): a `postprocess` row pointing to a new table.
- New `### Post-processing` under Usage: what settled, rejected and done mean; that size, type and completeness only decide body fetches; lanes; budget; how to turn on bodies.
- New `### Purging and reprocessing`: both commands, dry run output, each class and that `atlas --purge` alone takes fragments only, the order (stop indexing, dry run, `--apply`, `--compact`), the time and disk numbers from section 9 labelled as estimates.
- "Compacting a database" (`:269`): it refuses tables it does not know, and waits 10 s for a reader.
- "Stats dashboard" (`:301`): a Post-processing page; `1-6`.
- "Settings" (`:293`): Purge broken releases is one big transaction and refuses past 1M incomplete rows in a shard, and `atlas --purge --dead` is the batched way.
- "Credits" (`:516`): newznab-tmux, GPLv3, for the release rules.
- `src/main.rs` help: both commands. `config.example.json`: the block.

## 13. Decisions

1. **Size, type and completeness never reject.** Upstream's 1 MiB and 100 GiB gate only its body pass; naming and categories run on everything there (`AdditionalCandidateQuery.php:80,96`). Rejecting by them left most rows with no name and no category, which is the problem the overview starts from. They gate bodies alone.
2. **What is still rejected: fragments, blocked groups, empty rows, par2-only posts, and a size or file floor if the user sets one.** Each is a row no step can help. A rejection writes nothing and is judged again if touched.
3. **A row per touched release in `pp_queue`, written in the save's transaction.** A cursor-only design had no way to wake a release that grew and would need a separate time-to-id mapping. Measured at about 2%.
4. **`due_at` is rewritten on every touch.** That makes settling exact with no extra read, and apply's `where due_at = :claimed` catches an article arriving mid-run. Both delays floor at 1 s so the values can't collide.
5. **No queue row waits past settling.** A wait for `dead_after` would hold 20 GB per shard at peak and buy nothing, since a late article requeues anyway.
6. **Walks visit rows themselves and never use the queue.** With 100M rows now running steps, going through the queue meant a second judge and four more b-tree writes per row. This way results and cursor commit together, and the backlog can't flood anything.
7. **One walk at first start.** The backlog walk visits every row, so a `'steps'` walk beside it would be a second 16.4 h pass.
8. **A row per release in `pp_state`, even for database-only work.** 2.5 GB compacted, 5 GB until then. "No row means done" would lose reruns and per-step bits, and run every step on every touch. `updated_at` went: 500 MB for nothing that read it.
9. **Newest first, knowing what it costs.** Writing downward leaves pages half full, 50 B a row against 25, until the next compaction. Oldest first would save 2.5 GB for a while but name the releases clients search for last, 16 hours later. Compaction gives the space back, and the purge path runs one anyway.
10. **No leases or claim columns.** Only one process runs the stage and each shard has one batch in flight per lane, so there is nothing to lose a lease on. Upstream's stale-claim failures don't apply. A crash leaves queue rows in place.
11. **Attempts are written before a net step only.** Net steps do I/O and could kill the process. DB steps are pure CPU with bounded input, and `catch_unwind` covers panics.
12. **Separate attempt counts per lane.** A release can be in both lanes at once, and a DB visit must not reset a net step's count or backoff. Apply passes None for the other lane's columns.
13. **`wait_until` gives back what `Begin` counted.** It says nothing is wrong. Part 8 waits on provider limits this way.
14. **`Transient` in the DB lane doesn't pause.** One step's trouble shouldn't stop naming on every other release. The net lane pauses because its trouble is usually a whole server.
15. **All in-process stage writes go through the shard's writer thread.** Each collision with a second writer costs the first sleeps of up to 100 ms in SQLite's busy backoff, and the project already routes writes this way (`src/bg_indexer.rs:559-560`). The budget caps it at about 3% of a busy writer.
16. **The save takes `BEGIN IMMEDIATE`.** Needed for any outside writer; costs nothing, as the save writes anyway.
17. **Two lanes.** Database steps must never wait on NNTP (upstream to-do file, line 282).
18. **Steps are synchronous on OS threads.** Parts 4–8 write plain code, can do multi-round fetches and blocking `ureq` calls, and need no async trait. `Handle::block_on` bridges to the pool.
19. **The engine owns budgets, the block list, the body bounds and image refusal.** One place to get hard rules 8 and 9 right, not five. A part adding a read beside `fetch_bodies` shares its `refusal` function.
20. **`enabled` is how a step sees config, and `StageParams.config` is the full `Config`.** Parts 4 and 8 built on that, the first call now comes before any run, and a second path to the same values would be one more thing to keep in step.
21. **`writes` are static SQL with bound values, not closures.** They batch, log and test simply. Steps stay free of database write access (`query_only` connection).
22. **`facts` pass values between steps in one run.** The alternative, a writer round trip per step per release, would cut DB lane throughput by an order of magnitude.
23. **A rename runs later database steps in the same visit.** A requeue would be a second visit for most renamed rows. Net steps can't run there, so they go back to their lane.
24. **`NameChange` with None leaves that field.** A flag-only correction is then possible, and only a real change of name reruns anything.
25. **Registry-driven compaction that fails closed.** Discovering tables from the original and copying them blind would hide mistakes. A registry line gets everything, and anything else stops the shard with the original kept. `collections` is a listed core table because Part 2 filters its rows.
26. **Copy by name, including `releases`.** `select *` is an accident waiting for the first out-of-order column; `a_column_out_of_order_lands_in_the_right_place` shows it.
27. **No index on `releases` at open, ever.** Part 7's category filter uses its own table. Hard rule 7. Table prefixes are a convention: `release_category` (Part 7) is an allowed exception, and no test enforces them.
28. **`name_source` is a column, not a side table lookup.** The upsert reads it free in the same row; NULL costs a byte.
29. **Never update `releases.name`.** It is half the upsert key; changing it would create a duplicate release on the next article. Search and the API already prefer `display_name` (`src/search.rs:33`), and `releases_au` keeps FTS right.
30. **Rerun only clears higher ranks, and a rename is detected in the writer.** Termination is structural. An unchanged name reruns nothing.
31. **Newest first in the net lane, with every 8th claim taking the oldest.** New posts are worth most; the eighth keeps history moving (upstream to-do file, line 282; `BUGS.md` #62).
32. **Walks are LIFO per shard, with `top_id` fixed at creation.** A user's reprocess is more urgent than the backlog, which keeps its cursor and resumes. New rows belong to the queue, so a walk always ends (`BUGS.md` #62).
33. **Settle 2 h, complete 15 min, dead 48 h, completion 95%.** Upstream's defaults for the first, third and fourth. 15 min is new: Atlas can tell a complete release at save time, and one should not wait 2 h.
34. **Completeness is checked when a release is visited, not waited on.** Below 95% it gets no body now, and a late article brings it back. A 48 h wait would hold state for nothing.
35. **`judge` has no clock on fragments; purge does.** Rejecting deletes nothing and a late article undoes it, so the stage needn't wait. Deleting is different, so every purge class but `--blocked` needs `dead_after_secs` of quiet.
36. **The file read has no limit.** A cap made a 2,000-file album read as half complete, and `--dead` would have deleted it. It streams and keeps only numbers.
37. **Purge takes fragments alone unless told otherwise.** It is the one class with no real release in it. Small and image-only posts include ebooks, comics and photo sets, so each needs its own flag, and the dry run lists blocked groups by name because a false block would otherwise cost a group's rows unseen. Blocked groups aren't refused for still being in `config.json`: they're never indexed either way, and that would only make removal harder.
38. **Purge apply takes the exclusive lock and refuses a live indexer.** It runs for hours at full disk rate, the same class of job as `--compact`, and collisions would sleep the writer. Reprocess takes only the shared guard: it writes one row per shard.
39. **Purge deletes row by row in batches, not by a compaction filter.** A filter would be faster, but compaction's check would need to know purge state, and that tool has the most to lose. Deletes also make the result visible before the compaction.
40. **Purge counts articles from the rows and blobs, not `releases.parts`.** Exactly what `meta.articles` must lose; `parts` comes from old databases in converted rows. `blob::count` keeps it cheap. `parts` only sizes transactions, where being off a little costs nothing.
41. **A half-deleted release is finished first, whatever the classes.** Its class may no longer match, and it would stay short forever with the totals already reduced.
42. **A restart checkpoint every 64 commits, and a stop at 4 GiB.** A passive checkpoint never resets the WAL under a reader, and the API opens readers at any time. The cursor is safe, so stopping costs nothing.
43. **A dry run keeps no checkpoint.** It writes nothing at all, so it can run beside the indexer; an interrupted one starts over.
44. **Stage on by default, bodies off.** With no budget it costs some disk and CPU and no network. This satisfies hard rule 8. Off by default would leave the queue growing unseen.
45. **`background_body` never queues until 20 s have passed.** Indexing wins, but the stage can't starve forever. `limit/4` stays free for indexing; a server under 4 connections has none reserved and relies on `waiting == 0`.
46. **Body reads are bounded by bytes and by lines.** `read_raw_line` reads to a newline however far off, so one line could take a whole article past any cap. `read_line_capped` stops within one read. Lines are limited to one per 16 bytes of cap because a `Vec` per tiny line costs more than its bytes.
47. **No pacing of `TooBig` hang-ups here.** Each one means a new login, but articles past the cap are rare and each costs a body from the budget. Part 4's probes hang up on purpose, so it paces them itself with `archive.probes_per_minute`, and its wait holds no permit or token.
48. **Inline par2/nfo lookups stay.** Removing them changes network load and naming before Part 4 replaces them.
49. **No step versions.** New regexes or logic are picked up by `atlas --reprocess --step NAME`: explicit, and the user sees the cost first.
50. **Per-group rule overrides take `max(group, global)`.** Upstream's rule (`ReleaseProcessingService.php:1216,1240`).
51. **Bounded counts on the dashboard.** `BUGS.md` #44: exact counts at this size stalled the monitor. `100k+` is enough to read.
52. **The menu's purge stays, with a limit.** Replacing it is out of scope. Past 1M incomplete rows in a shard it would hold one transaction for hours with every trigger firing, so it refuses and names the batched command.
53. **Benchmarks are reported, not gated.** The overview's rule: they measure a machine that may be busy with the live indexer.
