//! The `ext_*` tables: what came from an external service (PreDB, the media providers, OpenSubtitles, the HTTP
//! cache's pinned snapshots) and releases thawed by hand. Indexing again can't bring these back, so they get what the
//! rest of the database doesn't need:
//!
//! - `copy_from_sqlite` moves them over once, at the cutover (indexing stopped, and in the same release as the
//!   providers' writes move to these tables, so nothing written after the copy is left behind in SQLite): each table copied from its SQLite file and checked
//!   against it by row count and an order-free digest of every row, then topped up from the frozen `keep.*`
//!   snapshot of 2026-11-08 (rows the SQLite raw store's caps pruned since come back; nothing there is overwritten);
//! - `dump` writes every `ext_*` table to a directory, from one snapshot, one zstd file per table and a manifest;
//!   `spawn_nightly` does it once a day and keeps the last few, away from the server's disk;
//! - `restore` puts a dump back into empty tables.
//!
//! The dump is binary COPY rather than `pg_dump`, so it needs no client tools on the indexer's machine and no
//! matching server version; a table's file is exactly what `copy ... from stdin (format binary)` reads back.

use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use postgres::Client;
use rusqlite::Connection;
use sha2::{Digest, Sha256};

use crate::keep_export::{self, Column, Tally};

/// One `ext_*` table filled from SQLite: the database beside `atlas.db` it comes from, and its table there.
struct Source {
    ext: &'static str,
    db: fn(&Path) -> PathBuf,
    table: &'static str,
}

fn sources() -> Vec<Source> {
    let predb = crate::predb::db::path_for;
    let media = crate::media::store::path_for;
    let details = crate::media::details::path_for;
    let raw = crate::raw_store::path_for;
    vec![
        Source { ext: "ext_predb_titles", db: predb, table: "titles" },
        Source { ext: "ext_predb_feeds", db: predb, table: "feeds" },
        Source { ext: "ext_predb_meta", db: predb, table: "meta" },
        Source { ext: "ext_media_titles", db: media, table: "media_titles" },
        Source { ext: "ext_media_details", db: details, table: "details" },
        Source { ext: "ext_media_details_lookups", db: details, table: "details_lookups" },
        Source { ext: "ext_opensubtitles", db: raw, table: "raw_opensubtitles" },
        Source { ext: "ext_predb_pages", db: raw, table: "raw_predb_log" },
        Source { ext: "ext_predb_page_events", db: raw, table: "raw_predb_events" },
        Source { ext: "ext_http_cache", db: raw, table: "raw_http_cache" },
    ]
}

/// The `keep.*` snapshot table each `ext_*` table is topped up from (`--export-keep` named them).
fn keep_name(ext: &str) -> Option<&'static str> {
    Some(match ext {
        "ext_predb_titles" => "predb_titles",
        "ext_predb_feeds" => "predb_feeds",
        "ext_predb_meta" => "predb_meta",
        "ext_media_titles" => "media_titles",
        "ext_media_details" => "media_details",
        "ext_media_details_lookups" => "media_details_lookups",
        "ext_opensubtitles" => "opensubtitles",
        "ext_predb_pages" => "predb_pages",
        "ext_predb_page_events" => "predb_page_events",
        "ext_http_cache" => "predb_snapshots",
        _ => return None,
    })
}

/// The `ext_*` tables in the connection's schema, by name.
pub fn tables(c: &mut Client) -> Result<Vec<String>> {
    Ok(c.query(
        "select table_name::text from information_schema.tables
          where table_schema = current_schema() and table_type = 'BASE TABLE' and table_name like 'ext\\_%'
          order by table_name",
        &[],
    )?
    .iter()
    .map(|r| r.get(0))
    .collect())
}

/// A table's columns in the connection's schema, in order.
fn pg_columns(c: &mut impl postgres::GenericClient, table: &str) -> Result<Vec<String>> {
    Ok(c.query(
        "select column_name::text from information_schema.columns
          where table_schema = current_schema() and table_name = $1 order by ordinal_position",
        &[&table],
    )?
    .iter()
    .map(|r| r.get(0))
    .collect())
}

/// What `copy_from_sqlite` did to one table.
#[derive(Debug)]
pub struct Moved {
    pub table: String,
    /// rows from SQLite, and their digest
    pub tally: Tally,
    /// rows the `keep` snapshot added on top
    pub from_keep: u64,
}

/// Every `ext_*` table filled from the SQLite files beside `main` (and the shards' hand thaws), then topped up from
/// `keep.*` when the database has it, in one transaction. A dry run (`apply` false) reads and checks the SQLite side
/// only. Refused if any `ext_*` table already has rows: the move happens once.
/// `schema` is migrated first, in the same transaction, so a dry run (rolled back) leaves Postgres as it was.
pub fn copy_from_sqlite(c: &mut Client, main: &Path, schema: &str, apply: bool) -> Result<Vec<Moved>> {
    let lite = Connection::open_in_memory()?;
    let mut tx = c.transaction()?;
    super::migrate::run_in_transaction(&mut tx, schema)?;
    for t in tables_of(&mut tx)? {
        let any: bool =
            tx.query_one(&format!("select exists (select 1 from {})", keep_export::quoted(&t)), &[])?.get(0);
        if any {
            bail!("{t} has rows already: the external data was moved before (refusing to copy over it)");
        }
    }
    let has_keep: bool = tx.query_one("select exists (select 1 from pg_namespace where nspname = 'keep')", &[])?.get(0);
    let mut moved = Vec::new();
    for s in sources() {
        let path = (s.db)(main);
        if !path.try_exists()? {
            bail!("{}: no {} (wrong directory?)", s.ext, path.display());
        }
        keep_export::attach_ro(&lite, &path, "src")?;
        let cols = keep_export::columns(&lite, "src", s.table)?;
        if cols.is_empty() {
            bail!("{}: no {} table in {}", s.ext, s.table, path.display());
        }
        let theirs = pg_columns(&mut tx, s.ext)?;
        if let Some(col) = cols.iter().find(|c| !theirs.contains(&c.name)) {
            bail!("{}: SQLite's {}.{} has no column in Postgres", s.ext, s.table, col.name);
        }
        let select = format!("select {} from src.{}", keep_export::select_list(&cols), keep_export::quoted(s.table));
        let tally = keep_export::tally_sql(&lite, &select, &cols)?;
        let mut from_keep = 0;
        if apply {
            keep_export::copy_rows(&mut tx, s.ext, &cols, &lite, &select, s.ext)?;
            let back = keep_export::pg_tally(&mut tx, s.ext, &cols)?;
            if back != tally {
                bail!("{}: Postgres holds something else than {} ({back:?} vs {tally:?})", s.ext, path.display());
            }
            if has_keep && let Some(k) = keep_name(s.ext) {
                from_keep = top_up(&mut tx, s.ext, k, &cols)?;
            }
        }
        lite.execute("detach database src", [])?;
        println!("  {}: {} rows, digest {}{}", s.ext, tally.rows, tally.hex(), added(from_keep));
        moved.push(Moved { table: s.ext.into(), tally, from_keep });
    }
    moved.push(freeze_exempt(&mut tx, &lite, main, apply, has_keep)?);
    if apply {
        advance_counters(&mut tx)?;
        tx.commit()?;
    }
    Ok(moved)
}

fn added(n: u64) -> String {
    if n == 0 { String::new() } else { format!(", {n} more from the keep snapshot") }
}

fn tables_of(tx: &mut postgres::Transaction<'_>) -> Result<Vec<String>> {
    Ok(tx
        .query(
            "select table_name::text from information_schema.tables
              where table_schema = current_schema() and table_type = 'BASE TABLE' and table_name like 'ext\\_%'",
            &[],
        )?
        .iter()
        .map(|r| r.get(0))
        .collect())
}

/// The rows of `keep.<keep>` that `ext` hasn't got (by any of its keys), added. Only the columns both have.
fn top_up(tx: &mut postgres::Transaction<'_>, ext: &str, keep: &str, cols: &[Column]) -> Result<u64> {
    let there: Vec<String> = tx
        .query(
            "select column_name::text from information_schema.columns where table_schema = 'keep' and table_name = $1",
            &[&keep],
        )?
        .iter()
        .map(|r| r.get(0))
        .collect();
    if there.is_empty() {
        return Ok(0);
    }
    let both: Vec<&Column> = cols.iter().filter(|c| there.contains(&c.name)).collect();
    let list = both.iter().map(|c| keep_export::quoted(&c.name)).collect::<Vec<_>>().join(", ");
    let total: i64 = tx.query_one(&format!("select count(*) from keep.{}", keep_export::quoted(keep)), &[])?.get(0);
    let keep_pages: bool = tx
        .query_one(
            "select exists (select 1 from information_schema.tables where table_schema = 'keep' and table_name = 'predb_pages')",
            &[],
        )?
        .get(0);
    let n = if ext == "ext_predb_page_events" && !keep_pages {
        // events can't be placed without their pages
        0
    } else if ext == "ext_predb_page_events" {
        // an event points at its page by id: a page the cap pruned and SQLite fetched again under a new id is the
        // same body, so the event goes to that page; an event already there (same page, page number and time) isn't
        // added twice, and one whose seq SQLite reused gets the next free one
        tx.execute(
            "insert into ext_predb_page_events (seq, log_id, page, fetched_at)
             select (select coalesce(max(seq), 0) from ext_predb_page_events) + row_number() over (order by e.seq),
                    p.id, e.page, e.fetched_at
               from keep.predb_page_events e
               join keep.predb_pages k on k.id = e.log_id
               join ext_predb_pages p on p.source_id = k.source_id and p.body_sha256 = k.body_sha256
              where not exists (select 1 from ext_predb_page_events x
                                 where x.log_id = p.id and x.page = e.page and x.fetched_at = e.fetched_at)",
            &[],
        )?
    } else if let Some((id, same)) = renumbered(tx, ext)? {
        // a row is the same by what it holds, whatever id it had: one already there (`same`) is left out, and one
        // whose id SQLite handed out again since the snapshot (a file made again, or the highest rows pruned) gets
        // the next free id instead of being dropped
        let picked = both
            .iter()
            .map(|c| {
                if c.name == id {
                    format!("coalesce(n.new_id, k.{id})")
                } else {
                    format!("k.{}", keep_export::quoted(&c.name))
                }
            })
            .collect::<Vec<_>>()
            .join(", ");
        tx.execute(
            &format!(
                "with missing as (
                     select k.* from keep.{keep_q} k
                      where not exists (select 1 from {ext} t where {same})
                 ),
                 n as (
                     select m.{id} as old_id,
                            greatest((select coalesce(max({id}), 0) from {ext}), (select coalesce(max({id}), 0) from missing))
                                + row_number() over (order by m.{id}) as new_id
                       from missing m where exists (select 1 from {ext} t where t.{id} = m.{id})
                 )
                 insert into {ext} ({list})
                 select {picked} from missing k left join n on n.old_id = k.{id}",
                keep_q = keep_export::quoted(keep),
            ),
            &[],
        )?
    } else {
        tx.execute(
            &format!(
                "insert into {ext} ({list}) select {list} from keep.{} on conflict do nothing",
                keep_export::quoted(keep)
            ),
            &[],
        )?
    };
    let skipped = (total as u64).saturating_sub(n);
    if skipped > 0 {
        println!("  {ext}: {skipped} rows of keep.{keep} are there already (by a key) and were left as they are");
    }
    Ok(n)
}

/// Every identity column of the ext tables set past the highest value in it, after rows came in with their own
/// (a move, a restore), so the next row Postgres numbers doesn't take one of theirs.
fn advance_counters(tx: &mut postgres::Transaction<'_>) -> Result<()> {
    let cols: Vec<(String, String)> = tx
        .query(
            "select table_name::text, column_name::text from information_schema.columns
              where table_schema = current_schema() and table_name like 'ext\\_%' and is_identity = 'YES'",
            &[],
        )?
        .iter()
        .map(|r| (r.get(0), r.get(1)))
        .collect();
    for (t, c) in cols {
        let (tq, cq) = (keep_export::quoted(&t), keep_export::quoted(&c));
        tx.execute(
            &format!(
                "select setval(pg_get_serial_sequence($1, $2), coalesce((select max({cq}) from {tq}), 0) + 1, false)"
            ),
            &[&tq, &c],
        )?;
    }
    Ok(())
}

/// The tables whose keep rows are matched by content rather than id, and how: (id column, the condition that a row
/// `t` of the ext table is keep row `k`).
/// A title is the importer's identity (`predb::import`): equal when lowercased, whichever feed brought it, soo one
/// that SQLite has again under other casing or from another feed since the snapshot isn't added twice. Lowercased
/// under ICU's root collation when the server has it (full Unicode, as Rust's `to_lowercase`), else the database's.
fn renumbered(tx: &mut postgres::Transaction<'_>, ext: &str) -> Result<Option<(&'static str, String)>> {
    Ok(match ext {
        "ext_predb_titles" => {
            let icu: bool =
                tx.query_one("select exists (select 1 from pg_collation where collname = 'und-x-icu')", &[])?.get(0);
            let c = if icu { "\"und-x-icu\"" } else { "\"default\"" };
            Some(("id", format!("lower(t.title collate {c}) = lower(k.title collate {c})")))
        }
        "ext_predb_pages" => Some(("id", "t.source_id = k.source_id and t.body_sha256 = k.body_sha256".into())),
        _ => None,
    })
}

/// Hand thaws from every shard, by the release's name and group (the id doesn't survive indexing again). A thaw
/// whose release row is gone can't be found again and is left out (counted); one release thawed in two shards keeps
/// the later time.
fn freeze_exempt(
    tx: &mut postgres::Transaction<'_>,
    lite: &Connection,
    main: &Path,
    apply: bool,
    has_keep: bool,
) -> Result<Moved> {
    let table = "ext_freeze_exempt";
    let (mut rows, mut lost) = (Vec::<(String, String, i64, i64)>::new(), 0u64);
    for shard in keep_export::shard_files(main) {
        keep_export::attach_ro(lite, &shard, "src")?;
        let has: bool = lite
            .prepare("select 1 from src.sqlite_master where type = 'table' and name = ?1")?
            .exists([keep_export::FREEZE_EXEMPT])?;
        if has {
            let mut stmt = lite.prepare(&format!(
                "select r.name, r.group_name, f.at, f.release_id from src.{} f
                 left join src.releases r on r.id = f.release_id",
                keep_export::FREEZE_EXEMPT
            ))?;
            let mut q = stmt.query([])?;
            while let Some(r) = q.next()? {
                match (r.get::<_, Option<String>>(0)?, r.get::<_, Option<String>>(1)?) {
                    (Some(n), Some(g)) => rows.push((n, g, r.get(2)?, r.get(3)?)),
                    _ => lost += 1,
                }
            }
        }
        lite.execute("detach database src", [])?;
    }
    let mut from_keep = 0;
    if apply {
        for (n, g, at, id) in &rows {
            tx.execute(
                "insert into ext_freeze_exempt (name, group_name, at, old_release_id) values ($1, $2, $3, $4)
                 on conflict (name, group_name) do update set at = greatest(ext_freeze_exempt.at, excluded.at)",
                &[n, g, at, id],
            )?;
        }
        let keep_has: bool = has_keep
            && tx
                .query_one(
                    "select exists (select 1 from information_schema.tables
                      where table_schema = 'keep' and table_name = 'freeze_exempt')",
                    &[],
                )?
                .get(0);
        if keep_has {
            from_keep = tx.execute(
                "insert into ext_freeze_exempt (name, group_name, at, old_release_id)
                 select name, group_name, at, release_id from keep.freeze_exempt
                  where name is not null and group_name is not null and at is not null
                 on conflict do nothing",
                &[],
            )?;
        }
    }
    let lost_note = if lost > 0 { format!(" ({lost} whose release row is gone left out)") } else { String::new() };
    println!("  {table}: {} rows{lost_note}{}", rows.len(), added(from_keep));
    Ok(Moved { table: table.into(), tally: Tally { rows: rows.len() as u64, digest: 0 }, from_keep })
}

/// `atlas --move-ext [--apply]`: `copy_from_sqlite` into the `[storage]` Postgres (migrated first, and only
/// committed with `--apply`).
pub fn run_move(args: &[String]) -> i32 {
    let apply = args.iter().any(|a| a == "--apply");
    let go = || -> Result<()> {
        let storage = crate::config::storage_config();
        storage.check()?;
        if storage.url.is_empty() {
            bail!("no [storage] url in config.toml");
        }
        let pool = super::Pool::new(&storage)?;
        let mut c = pool.get()?;
        let main = crate::paths::database();
        println!(
            "{} external data from beside {}",
            if apply { "moving" } else { "checking (dry run)" },
            main.display()
        );
        copy_from_sqlite(&mut c, &main, pool.schema(), apply)?;
        if !apply {
            println!("dry run: nothing written (--apply to move it)");
        }
        Ok(())
    };
    match go() {
        Ok(()) => 0,
        Err(e) => {
            crate::ui::error(&format!("move-ext: {e:#}"));
            1
        }
    }
}

// ---------------------------------------------------------------- dumps

/// The manifest of one dump: what's in it, and what each table held when it was taken.
#[derive(Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct Manifest {
    pub taken_at: String,
    pub schema_version: i32,
    pub tables: Vec<Dumped>,
    /// taken with `--allow-shrink`: what the data is meant to be from now on, so later dumps are measured against it
    /// and newer ones only, not against the dumps before it
    #[serde(default)]
    pub baseline: bool,
}

#[derive(Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct Dumped {
    pub table: String,
    pub columns: Vec<String>,
    pub rows: i64,
    pub file: String,
    pub bytes: u64,
    /// SHA-256 of the table's COPY stream as Postgres sent it (before compression), checked when the dump is
    /// written and again before a restore commits: a damaged file, or another dump's, is refused
    #[serde(default)]
    pub sha256: Option<String>,
    /// for a table whose rows come and go (`CHURNS`), how many of them never expire (`PINNED`): guarded by `shrank`
    /// as other tables' rows are, with no loss allowed
    #[serde(default)]
    pub pinned: Option<i64>,
}

/// The rows of a `CHURNS` table that are kept forever: the HTTP cache's static historical PreDB archive snapshots
/// (`raw_store::is_immutable_predb_page`).
const PINNED: &[(&str, &str)] = &[("ext_http_cache", "source in ('nzedb_archive', 'dats_pciso_1998')")];

/// A writer that hashes what goes through it.
struct Hashed<W> {
    inner: W,
    sha: Sha256,
}

impl<W: Write> Write for Hashed<W> {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(b)?;
        self.sha.update(&b[..n]);
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// `file` decompressed, its SHA-256 compared with `want`.
fn check_file(file: &Path, want: &str) -> Result<()> {
    let mut dec = zstd::Decoder::new(BufReader::new(std::fs::File::open(file)?))?;
    let mut h = Hashed { inner: std::io::sink(), sha: Sha256::new() };
    std::io::copy(&mut dec, &mut h).with_context(|| format!("reading {}", file.display()))?;
    let got = format!("{:x}", h.sha.finalize());
    if got != want {
        bail!("{}: SHA-256 {got}, the manifest says {want}", file.display());
    }
    Ok(())
}

const MANIFEST: &str = "manifest.json";

/// Every `ext_*` table written under `dir/<time>/` from one snapshot (repeatable read): a zstd-compressed binary
/// COPY per table and `manifest.json` last. It's made under a `.partial` name and renamed when whole, so a dump
/// directory without that suffix is complete. Then all but the newest `keep` dumps are removed. Returns the dump's
/// directory. A dump that looks like data was lost (`shrank`) is refused with `Shrank`, unless `allow_shrink`.
pub fn dump(c: &mut Client, dir: &Path, keep: usize, allow_shrink: bool) -> Result<PathBuf> {
    std::fs::create_dir_all(dir).with_context(|| format!("making {}", dir.display()))?;
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    let partial = dir.join(format!("{stamp}.partial"));
    let done = dir.join(&stamp);
    if done.exists() || partial.exists() {
        bail!("{} is there already", done.display());
    }
    std::fs::create_dir(&partial)?;
    let made = (|| -> Result<()> {
        let mut tx =
            c.build_transaction().isolation_level(postgres::IsolationLevel::RepeatableRead).read_only(true).start()?;
        let schema: String = tx.query_one("select current_schema()::text", &[])?.get(0);
        let version: i32 = tx.query_one("select coalesce(max(version), 0) from schema_migrations", &[])?.get(0);
        let names: Vec<String> = tables_of(&mut tx)?;
        if names.is_empty() {
            bail!("schema {schema} has no ext_* tables: nothing to dump (wrong schema, or not migrated)");
        }
        let mut tables = Vec::new();
        for t in names {
            let columns = pg_columns(&mut tx, &t)?;
            let list = columns.iter().map(|c| keep_export::quoted(c)).collect::<Vec<_>>().join(", ");
            let rows: i64 = tx.query_one(&format!("select count(*) from {}", keep_export::quoted(&t)), &[])?.get(0);
            let pinned = match PINNED.iter().find(|(p, _)| *p == t) {
                Some((_, kept)) => Some(
                    tx.query_one(&format!("select count(*) from {} where {kept}", keep_export::quoted(&t)), &[])?
                        .get::<_, i64>(0),
                ),
                None => None,
            };
            let file = format!("{t}.copy.zst");
            let path = partial.join(&file);
            let out = std::fs::File::create(&path)?;
            let enc = zstd::Encoder::new(BufWriter::new(out), 6)?;
            let mut hashed = Hashed { inner: enc, sha: Sha256::new() };
            let mut reader =
                tx.copy_out(&format!("copy {} ({list}) to stdout (format binary)", keep_export::quoted(&t)))?;
            std::io::copy(&mut reader, &mut hashed)?;
            drop(reader);
            let sha = format!("{:x}", hashed.sha.finalize());
            let w = hashed.inner.finish()?;
            let f = w.into_inner().map_err(|e| e.into_error())?;
            f.sync_all()?;
            drop(f);
            // read back: what's on disk is what Postgres sent
            check_file(&path, &sha)?;
            let bytes = std::fs::metadata(&path)?.len();
            tables.push(Dumped { table: t, columns, rows, file, bytes, sha256: Some(sha), pinned });
        }
        tx.commit()?;
        let m = Manifest { taken_at: stamp.clone(), schema_version: version, tables, baseline: allow_shrink };
        if let Some(why) = shrank(&kept_manifests(dir), &m).filter(|_| !allow_shrink) {
            return Err(Shrank(why).into());
        }
        let mf = partial.join(MANIFEST);
        let mut f = std::fs::File::create(&mf)?;
        f.write_all(serde_json::to_string_pretty(&m)?.as_bytes())?;
        f.sync_all()?;
        let total: u64 = m.tables.iter().map(|t| t.bytes).sum();
        let rows: i64 = m.tables.iter().map(|t| t.rows).sum();
        crate::ui::dim(&format!(
            "ext dump of schema {schema}: {} tables, {rows} rows, {:.1} MB",
            m.tables.len(),
            total as f64 / 1e6
        ));
        Ok(())
    })();
    if let Err(e) = made {
        let _ = std::fs::remove_dir_all(&partial);
        return Err(e);
    }
    std::fs::File::open(&partial)?.sync_all()?;
    std::fs::rename(&partial, &done)?;
    // the rename is only durable once the directory holding it is synced: until then a crash could lose the new
    // dump, soo the old ones stay
    match std::fs::File::open(dir).and_then(|d| d.sync_all()) {
        Ok(()) => {
            if let Err(e) = prune(dir, keep) {
                crate::ui::warn(&format!("ext dump: couldnt remove old dumps ({e:#})"));
            }
        }
        Err(e) => crate::ui::warn(&format!(
            "ext dump: couldnt sync {} after the rename ({e}), so no old dump was removed",
            dir.display()
        )),
    }
    Ok(done)
}

/// Why a dump was refused: the tables hold less than the dumps kept say they did (`shrank`).
#[derive(Debug)]
pub struct Shrank(pub String);

impl std::fmt::Display for Shrank {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "refusing to keep this dump in the rotation: {} (the dumps before it are kept)", self.0)
    }
}

impl std::error::Error for Shrank {}

/// Every finished dump's manifest in `dir` that can be read, from the newest baseline on (oldest first).
fn kept_manifests(dir: &Path) -> Vec<Manifest> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    since_baseline(
        entries
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| is_dump_name(n))
            .filter_map(|n| serde_json::from_slice::<Manifest>(&std::fs::read(dir.join(n).join(MANIFEST)).ok()?).ok())
            .collect::<Vec<_>>(),
    )
}

/// The newest baseline and the dumps after it, when there is one (`Manifest::baseline`), oldest first.
fn since_baseline(mut kept: Vec<Manifest>) -> Vec<Manifest> {
    kept.sort_by(|a, b| a.taken_at.cmp(&b.taken_at));
    if let Some(i) = kept.iter().rposition(|m| m.baseline) {
        kept.drain(..i);
    }
    kept
}

/// A table whose rows come and go by design (the HTTP cache's entries expire after 12 hours): left out of `shrank`,
/// all but its `PINNED` rows.
const CHURNS: &[&str] = &["ext_http_cache"];

/// Why `now` looks like a database that lost data since the dumps kept: a table they had is gone, or one is below
/// the most rows any of them had for it by more than a tenth (to none at all, for a small table). External data is
/// only ever added to, so either is a sign of a wipe, a bad restore or a wrong schema, and such a dump mustn't push
/// the good ones out of the rotation. Measured against the most any kept dump had, so a loss night after night is
/// caught too.
fn shrank(kept: &[Manifest], now: &Manifest) -> Option<String> {
    let mut most: std::collections::BTreeMap<&str, i64> = std::collections::BTreeMap::new();
    let mut most_pinned: std::collections::BTreeMap<&str, i64> = std::collections::BTreeMap::new();
    for m in kept {
        for t in &m.tables {
            let e = most.entry(t.table.as_str()).or_insert(0);
            *e = (*e).max(t.rows);
            if let Some(p) = t.pinned {
                let e = most_pinned.entry(t.table.as_str()).or_insert(0);
                *e = (*e).max(p);
            }
        }
    }
    // the rows of a churning table that never expire may not go down at all: they're static snapshots
    for (table, &pinned) in &most_pinned {
        let now_pinned = now.tables.iter().find(|t| t.table == *table).map_or(0, |t| t.pinned.unwrap_or(0));
        if now_pinned < pinned {
            return Some(format!("{table} has {now_pinned} rows kept forever, an earlier dump had {pinned}"));
        }
    }
    for (table, &rows) in &most {
        if CHURNS.contains(table) {
            continue;
        }
        match now.tables.iter().find(|t| t.table == *table) {
            None => return Some(format!("{table} was in an earlier dump and is gone")),
            Some(t) if rows > 0 && (t.rows == 0 || t.rows * 10 < rows * 9) => {
                return Some(format!("{table} has {} rows, an earlier dump had {rows}", t.rows));
            }
            _ => {}
        }
    }
    None
}

/// Whether `name` is a finished dump's directory name (`YYYYMMDDTHHMMSSZ`).
fn is_dump_name(name: &str) -> bool {
    name.len() == 16
        && name.as_bytes()[8] == b'T'
        && name.ends_with('Z')
        && name[..8].bytes().all(|b| b.is_ascii_digit())
        && name[9..15].bytes().all(|b| b.is_ascii_digit())
}

/// All but the newest `keep` finished dumps removed, and any `.partial` left by a dump that died.
fn prune(dir: &Path, keep: usize) -> Result<()> {
    let mut done = Vec::new();
    for e in std::fs::read_dir(dir)? {
        let e = e?;
        let name = e.file_name().to_string_lossy().into_owned();
        if !e.file_type()?.is_dir() {
            continue;
        }
        if is_dump_name(&name) {
            done.push(name);
        } else if name.strip_suffix(".partial").is_some_and(is_dump_name) {
            // one being made now has a stamp of this second; older ones are dead
            let age = e.metadata()?.modified()?.elapsed().unwrap_or_default();
            if age > Duration::from_secs(6 * 3600) {
                std::fs::remove_dir_all(e.path())?;
            }
        }
    }
    done.sort();
    let n = done.len().saturating_sub(keep.max(1));
    for name in &done[..n] {
        std::fs::remove_dir_all(dir.join(name))?;
    }
    Ok(())
}

/// A dump put back: each of its tables COPYed into the connection's schema in one transaction, its SHA-256 and row
/// count checked before anything commits. Tables that have rows are refused unless `replace`, which empties them first.
pub fn restore(c: &mut Client, from: &Path, replace: bool) -> Result<Manifest> {
    let m: Manifest = serde_json::from_slice(&std::fs::read(from.join(MANIFEST)).context("reading the manifest")?)?;
    let mut tx = c.transaction()?;
    let version: i32 = tx.query_one("select coalesce(max(version), 0) from schema_migrations", &[])?.get(0);
    if version < m.schema_version {
        bail!("the dump is of schema version {}, this database is at {version}: migrate it first", m.schema_version);
    }
    // all or nothing: a dump missing a table would leave that table as it is beside restored ones
    let missing: Vec<String> =
        tables_of(&mut tx)?.into_iter().filter(|t| !m.tables.iter().any(|d| &d.table == t)).collect();
    // a dump from before a migration that added a table can't have it, and is still a dump of all there was then;
    // a dump of the same version missing one is broken
    if !missing.is_empty() && m.schema_version >= version {
        bail!("the dump has no {}: refusing a partial restore", missing.join(", "));
    }
    if !missing.is_empty() {
        crate::ui::warn(&format!(
            "the dump (schema version {}) predates {}: left as they are",
            m.schema_version,
            missing.join(", ")
        ));
    }
    for t in &m.tables {
        let plain = |n: &str| n.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
        if !t.table.starts_with("ext_") || !plain(&t.table) || t.file != format!("{}.copy.zst", t.table) {
            bail!("the manifest names {:?} / {:?}: not a dump of the ext tables", t.table, t.file);
        }
        let q = keep_export::quoted(&t.table);
        let any: bool = tx.query_one(&format!("select exists (select 1 from {q})"), &[])?.get(0);
        if any {
            if !replace {
                bail!("{} has rows: refused (--replace to empty it first)", t.table);
            }
            tx.batch_execute(&format!("truncate {q}"))?;
        }
        let Some(want) = &t.sha256 else {
            bail!("{}: the dump has no checksum (made before they were recorded), so it can't be verified", t.table);
        };
        let list = t.columns.iter().map(|c| keep_export::quoted(c)).collect::<Vec<_>>().join(", ");
        let file = std::fs::File::open(from.join(&t.file))?;
        let mut dec = zstd::Decoder::new(BufReader::new(file))?;
        let mut w = tx.copy_in(&format!("copy {q} ({list}) from stdin (format binary)"))?;
        let mut buf = vec![0u8; 1 << 20];
        let mut sha = Sha256::new();
        loop {
            let n = dec.read(&mut buf)?;
            if n == 0 {
                break;
            }
            sha.update(&buf[..n]);
            w.write_all(&buf[..n])?;
        }
        w.finish()?;
        // before anything commits: a file that decodes and COPYs fine can still be the wrong one
        let got = format!("{:x}", sha.finalize());
        if &got != want {
            bail!("{}: the file's SHA-256 is {got}, the manifest says {want}: not restored", t.table);
        }
        let rows: i64 = tx.query_one(&format!("select count(*) from {q}"), &[])?.get(0);
        if rows != t.rows {
            bail!("{}: {rows} rows back, the dump says {}", t.table, t.rows);
        }
    }
    advance_counters(&mut tx)?;
    tx.commit()?;
    Ok(m)
}

/// Where the dumps go: `ext-dumps/` beside `atlas.db`.
pub fn dump_dir() -> PathBuf {
    crate::paths::database().with_file_name("ext-dumps")
}

/// When the nightly dump runs, local hour.
pub const DUMP_HOUR: u32 = 4;

/// How many dumps are kept.
pub const KEEP_DUMPS: usize = 7;

/// A thread that dumps the ext tables once a day, at `hour` (local time), until `stop`. A failed dump (Postgres
/// down) is tried again an hour later.
pub fn spawn_nightly(pool: Arc<super::Pool>, dir: PathBuf, hour: u32, stop: Arc<AtomicBool>) {
    std::thread::Builder::new()
        .name("ext-dump".into())
        .spawn(move || {
            let mut next = next_at(chrono::Local::now(), hour);
            while !stop.load(Ordering::Relaxed) {
                if chrono::Local::now() < next {
                    std::thread::sleep(Duration::from_secs(5));
                    continue;
                }
                let r =
                    pool.get().map_err(anyhow::Error::from).and_then(|mut c| dump(&mut c, &dir, KEEP_DUMPS, false));
                match r {
                    Ok(_) => next = next_at(chrono::Local::now(), hour),
                    // trying again in an hour would find the same: said, and left for the next night
                    Err(e) if e.is::<Shrank>() => {
                        crate::ui::error(&format!(
                            "ext dump: {e}; check the ext_* tables, or `atlas --dump-ext --allow-shrink` if it was meant"
                        ));
                        next = next_at(chrono::Local::now(), hour);
                    }
                    Err(e) => {
                        crate::ui::warn(&format!("ext dump failed ({e:#}); trying again in an hour"));
                        next = chrono::Local::now() + chrono::Duration::hours(1);
                    }
                }
            }
        })
        .expect("spawning the ext dump thread");
}

/// The next `hour`:00 local after `now`.
fn next_at(now: chrono::DateTime<chrono::Local>, hour: u32) -> chrono::DateTime<chrono::Local> {
    use chrono::{Duration as D, TimeZone};
    let day = now.date_naive();
    for d in 0..3 {
        if let Some(t) = (day + D::days(d)).and_hms_opt(hour.min(23), 0, 0)
            && let Some(t) = chrono::Local.from_local_datetime(&t).earliest()
            && t > now
        {
            return t;
        }
    }
    now + D::days(1)
}

/// `atlas --dump-ext` (a dump now, into `ext-dumps/`) and `atlas --restore-ext DIR [--replace]`.
pub fn run_cli(args: &[String]) -> i32 {
    let go = || -> Result<()> {
        let storage = crate::config::storage_config();
        storage.check()?;
        if storage.url.is_empty() {
            bail!("no [storage] url in config.toml");
        }
        let pool = super::Pool::new(&storage)?;
        let mut c = pool.get()?;
        if args.iter().any(|a| a == "--restore-ext") {
            let from = args
                .iter()
                .skip_while(|a| *a != "--restore-ext")
                .nth(1)
                .filter(|a| !a.starts_with("--"))
                .context("--restore-ext DIR")?;
            super::migrate::run(&mut c, pool.schema())?;
            let m = restore(&mut c, Path::new(from), args.iter().any(|a| a == "--replace"))?;
            println!("restored {} tables from the dump of {}", m.tables.len(), m.taken_at);
        } else {
            let d = dump(&mut c, &dump_dir(), KEEP_DUMPS, args.iter().any(|a| a == "--allow-shrink"))?;
            println!("dumped to {}", d.display());
        }
        Ok(())
    };
    match go() {
        Ok(()) => 0,
        Err(e) => {
            crate::ui::error(&format!("ext: {e:#}"));
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dump_names_and_next_time() {
        assert!(is_dump_name("20261109T030000Z"));
        assert!(!is_dump_name("20261109T030000Z.partial"));
        assert!(!is_dump_name("keep"));
        let now = chrono::Local::now();
        let n = next_at(now, 4);
        assert!(n > now && n - now <= chrono::Duration::hours(25));
    }

    #[test]
    fn a_dump_that_lost_rows_or_a_table_is_refused() {
        let t = |name: &str, rows| Dumped {
            table: name.into(),
            columns: vec![],
            rows,
            file: format!("{name}.copy.zst"),
            bytes: 0,
            sha256: None,
            pinned: None,
        };
        let m = |tables| Manifest { taken_at: String::new(), schema_version: 2, tables, baseline: false };
        let kept = [
            m(vec![t("ext_a", 5000), t("ext_b", 3), t("ext_http_cache", 900)]),
            m(vec![t("ext_a", 4600), t("ext_b", 3), t("ext_http_cache", 10)]),
        ];
        let ok = |a, b, h| shrank(&kept, &m(vec![t("ext_a", a), t("ext_b", b), t("ext_http_cache", h)]));
        assert_eq!(ok(5001, 3, 0), None, "the HTTP cache comes and goes");
        assert_eq!(ok(4600, 3, 5), None, "within a tenth of the most a kept dump had");
        assert!(ok(4400, 3, 5).is_some(), "measured against the most, not the newest");
        assert!(ok(5000, 0, 5).is_some(), "a small table emptied");
        assert!(shrank(&kept, &m(vec![t("ext_a", 5000), t("ext_http_cache", 1)])).is_some(), "a table gone");
        assert_eq!(shrank(&[], &m(vec![])), None);
        // a dump kept with --allow-shrink is where later ones are measured from
        let mut base = m(vec![t("ext_a", 100), t("ext_b", 3)]);
        base.baseline = true;
        base.taken_at = "20261103T040000Z".into();
        let mut old = kept.into_iter().collect::<Vec<_>>();
        old[0].taken_at = "20261101T040000Z".into();
        old[1].taken_at = "20261102T040000Z".into();
        old.push(base);
        let since = since_baseline(old);
        assert_eq!(since.len(), 1);
        assert_eq!(shrank(&since, &m(vec![t("ext_a", 101), t("ext_b", 3)])), None);
        // the HTTP cache's pinned snapshots may not go down at all, however its other rows come and go
        let cache = |rows, pinned| Dumped { pinned: Some(pinned), ..t("ext_http_cache", rows) };
        let kept = [m(vec![cache(900, 40)]), m(vec![cache(50, 41)]), m(vec![t("ext_http_cache", 7)])];
        assert_eq!(shrank(&kept, &m(vec![cache(41, 41)])), None);
        assert!(shrank(&kept, &m(vec![cache(5000, 40)])).is_some(), "one pinned row lost");
        assert!(shrank(&kept, &m(vec![t("ext_http_cache", 5000)])).is_some(), "pinned rows not counted");
    }

    #[test]
    fn pinned_rows_are_the_raw_stores_forever_ones() {
        for key in ["nzedb_archive", "dats_pciso_1998"] {
            assert!(crate::raw_store::is_immutable_predb_page(key, 0));
            assert!(PINNED[0].1.contains(&format!("'{key}'")));
        }
    }

    #[test]
    fn prune_keeps_the_newest() {
        let dir = tempfile::tempdir().unwrap();
        for s in ["20261101T040000Z", "20261102T040000Z", "20261103T040000Z", "other"] {
            std::fs::create_dir(dir.path().join(s)).unwrap();
        }
        prune(dir.path(), 2).unwrap();
        let mut left: Vec<String> =
            std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        left.sort();
        assert_eq!(left, ["20261102T040000Z", "20261103T040000Z", "other"]);
    }

    /// SQLite files made with the providers' own schemas, a row or two each, moved, dumped, emptied and restored.
    #[test]
    fn move_dump_restore_round_trip() {
        let Some(s) = super::super::testing::Schema::empty() else { return };
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        crate::db::create_at(&main).unwrap();
        {
            let p = crate::predb::db::create(&crate::predb::db::path_for(&main)).unwrap();
            p.execute(
                "insert into titles (id, title, source, added_at) values (1, 'Some.Movie.2020.1080p.BluRay-GRP', 3, 10)",
                [],
            )
            .unwrap();
            let r = crate::raw_store::create(&crate::raw_store::path_for(&main)).unwrap();
            r.execute(
                "insert into raw_http_cache (cache_key, source, url, status, body_zst, fetched_at)
                 values ('k', 'nzedb_archive', 'https://example.invalid/a', 200, x'00ff', 5)",
                [],
            )
            .unwrap();
            crate::media::store::open(&crate::media::store::path_for(&main)).unwrap();
            let d = crate::db::create_at(&crate::media::details::path_for(&main)).unwrap();
            d.execute_batch(crate::media::details::SCHEMA).unwrap();
            d.execute_batch("alter table details add column backdrop TEXT").unwrap();
        }
        let mut c = s.pool.get().unwrap();
        // a dry run writes nothing, not even the migrations it needed
        copy_from_sqlite(&mut c, &main, &s.name, false).unwrap();
        let migrated: bool = c
            .query_one("select exists (select 1 from information_schema.tables where table_schema = $1)", &[&s.name])
            .unwrap()
            .get(0);
        assert!(!migrated, "a dry run left tables behind");
        let moved = copy_from_sqlite(&mut c, &main, &s.name, true).unwrap();
        let rows = |t: &str| moved.iter().find(|m| m.table == t).unwrap().tally.rows;
        assert_eq!(rows("ext_predb_titles"), 1);
        assert_eq!(rows("ext_http_cache"), 1);
        assert!(copy_from_sqlite(&mut c, &main, &s.name, true).is_err(), "a second move is refused");
        // a new title is numbered past the moved ones, as SQLite would
        let id: i64 = c
            .query_one(
                "insert into ext_predb_titles (title, source, added_at) values ('Next', 3, 11) returning id",
                &[],
            )
            .unwrap()
            .get(0);
        assert_eq!(id, 2);

        let dumps = dir.path().join("ext-dumps");
        let d = dump(&mut c, &dumps, 3, false).unwrap();
        let all = tables(&mut c).unwrap().join(", ");
        c.batch_execute(&format!("truncate {all}")).unwrap();
        let m = restore(&mut c, &d, false).unwrap();
        assert_eq!(m.tables.iter().find(|t| t.table == "ext_predb_titles").unwrap().rows, 2);
        let title: String = c.query_one("select title from ext_predb_titles where id = 1", &[]).unwrap().get(0);
        assert_eq!(title, "Some.Movie.2020.1080p.BluRay-GRP");
        let body: Vec<u8> = c.query_one("select body_zst from ext_http_cache", &[]).unwrap().get(0);
        assert_eq!(body, [0x00, 0xff]);
        assert!(restore(&mut c, &d, false).is_err(), "tables with rows are refused without --replace");
        let pinned = m.tables.iter().find(|t| t.table == "ext_http_cache").unwrap().pinned;
        assert_eq!(pinned, Some(1), "the nzedb archive page is counted as kept forever");
        // a file that isn't the one dumped (here: another table's, decodable and the same size of rows) is
        // refused before anything commits, even with --replace
        let swapped = dir.path().join("swapped");
        std::fs::create_dir(&swapped).unwrap();
        for e in std::fs::read_dir(&d).unwrap() {
            let e = e.unwrap();
            std::fs::copy(e.path(), swapped.join(e.file_name())).unwrap();
        }
        let mut sm: Manifest = serde_json::from_slice(&std::fs::read(swapped.join(MANIFEST)).unwrap()).unwrap();
        let t = sm.tables.iter_mut().find(|t| t.table == "ext_predb_titles").unwrap();
        t.sha256 = Some("0".repeat(64));
        std::fs::write(swapped.join(MANIFEST), serde_json::to_vec(&sm).unwrap()).unwrap();
        let err = restore(&mut c, &swapped, true).unwrap_err().to_string();
        assert!(err.contains("SHA-256"), "{err}");
        let n: i64 = c.query_one("select count(*) from ext_predb_titles", &[]).unwrap().get(0);
        assert_eq!(n, 2, "the refused restore left the tables as they were");
        restore(&mut c, &d, true).unwrap();
        let id: i64 = c
            .query_one(
                "insert into ext_predb_titles (title, source, added_at) values ('After', 3, 12) returning id",
                &[],
            )
            .unwrap()
            .get(0);
        assert_eq!(id, 3, "numbered past what the restore brought back");
    }
}
