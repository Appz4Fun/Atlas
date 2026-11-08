//! `atlas --merge-shards`: the eight shards folded into `atlas.s0.db`, one at a time, for the move to one shard (and
//! later to Postgres). Offline: the indexer stopped, the compaction lock held.
//!
//! What collides between shards is moved out of the way rather than rewritten:
//! - file ids count from 1 in each shard: a source's are moved past the target's highest (`file_offset`);
//! - message-id domains are numbered per shard, and sealed blobs (some kept in the sealed log, out of reach) hold
//!   those numbers: a source's domains go into the target's `domains` under `shard << 32` too (a suffix may then
//!   have several ids, all reading the same: `domains` loses its unique index), its segment rows and the blobs kept
//!   in the shard take the target's id for each suffix (the lowest), and a sealed-log place is marked to be read on
//!   from that base (`blob::shifted`), soo only what's in the log keeps a source's numbers;
//! - collection keys and message-id hashes are keyed per shard, and their strings aren't kept: a source's collections
//!   go to `legacy_collections` under its key pair, and the pair is kept (`merge_shards`) for what hashes its
//!   releases' message-ids. Release ids already say which shard they came from (`id % 8`).
//!
//! Everything else is keyed by release id, unique across shards, and copied as it is. Each table goes over in key
//! ranges, one transaction each, its progress kept (`merge_progress`), soo a stop or a crash goes on from there. A
//! source is deleted only once every table's copied rows match its count, soo at most one shard's worth of space is
//! taken at a time.

use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};

/// the shards there were before the merge
pub const LEGACY_SHARDS: usize = 8;
/// rows a transaction aims at
const BATCH_ROWS: i64 = 200_000;

const SCHEMA: &str = "
create table if not exists merge_shards (
    shard         INTEGER PRIMARY KEY,
    file_offset   INTEGER NOT NULL,
    domain_offset INTEGER NOT NULL,
    key0          INTEGER,
    key1          INTEGER,
    max_release   INTEGER,
    max_file      INTEGER,
    grouping_from INTEGER,
    done          INTEGER NOT NULL DEFAULT 0
);
create table if not exists merge_progress (
    shard  INTEGER NOT NULL,
    tbl    TEXT NOT NULL,
    upto   INTEGER,
    copied INTEGER NOT NULL DEFAULT 0,
    done   INTEGER NOT NULL DEFAULT 0,
    primary key (shard, tbl)
) without rowid;
create table if not exists merge_triggers (name TEXT PRIMARY KEY, sql TEXT NOT NULL);
create table if not exists legacy_collections (
    key0 INTEGER NOT NULL, key1 INTEGER NOT NULL, key INTEGER NOT NULL, release_id INTEGER NOT NULL,
    primary key (key0, key1, key)
) without rowid;
create table if not exists legacy_junk_collections (
    key0 INTEGER NOT NULL, key1 INTEGER NOT NULL, key INTEGER NOT NULL,
    primary key (key0, key1, key)
) without rowid;
";

/// frozen files keep their collections under the same per-shard keys
const FROZEN_SCHEMA: &str = "
create table if not exists merge_progress (
    shard  INTEGER NOT NULL,
    tbl    TEXT NOT NULL,
    upto   INTEGER,
    copied INTEGER NOT NULL DEFAULT 0,
    done   INTEGER NOT NULL DEFAULT 0,
    primary key (shard, tbl)
) without rowid;
create table if not exists legacy_frozen_collections (
    key0 INTEGER NOT NULL, key1 INTEGER NOT NULL, key INTEGER NOT NULL, release_id INTEGER NOT NULL,
    primary key (key0, key1, key)
) without rowid;
";

/// tables whose `file_id` is a file of the shard's, moved by `file_offset`
const FILE_KEYED: [&str; 4] = ["segments", "held_back", "release_ids", "file_timing"];
/// tables with a `file_id` that isn't one of the shard's files
const FOREIGN_FILE_ID: [&str; 1] = ["par2_files"];
/// not copied: rebuilt (full text index shadows, kept up by triggers), started over (walks), or handled apart (meta)
fn skipped(table: &str) -> bool {
    table.starts_with("sqlite_")
        || table.starts_with("releases_fts")
        || table.starts_with("frozen_fts")
        || matches!(table, "meta" | "pp_walks" | "merge_shards" | "merge_progress" | "merge_triggers")
        // a cache of message-id hashes keyed by the source's secret: rebuilt under the target's (`store::IdKey`)
        || table == "release_ids"
        // the shared catalog is filled again after a merge (`--shared-catalog-fill`): its rows carry their shard
        || table == "catalog_outbox"
        || table.starts_with("legacy_")
}

/// One source's plan: which shard, its file, and its frozen file.
#[derive(Clone, Debug)]
pub struct Source {
    pub shard: usize,
    pub path: PathBuf,
}

/// The shards to fold into shard 0 of `main`, in shard order: each source's domains come in from `shard << 32`, soo
/// the lowest id of a suffix (the one its rows are renumbered to, `map_domains`) is the first source's that has it and
/// stays so; merged in any other order, a suffix two sources share would end with rows under two ids.
pub fn sources(main: &Path) -> Vec<Source> {
    (1..LEGACY_SHARDS)
        .map(|shard| Source { shard, path: legacy_shard_path(main, shard) })
        .filter(|s| s.path.exists())
        .collect()
}

/// The files of the shards from before one shard still beside `main`: `atlas.s1.db` .. `atlas.s7.db`, and a
/// compaction's backup of one (`.precompact.db`). None once merged.
pub fn legacy_leftovers(main: &Path) -> Vec<PathBuf> {
    (1..LEGACY_SHARDS)
        .map(|s| legacy_shard_path(main, s))
        .flat_map(|p| [crate::compact::with_suffix(&p, "precompact"), p])
        .filter(|p| p.exists())
        .collect()
}

/// `atlas.db` -> `atlas.s3.db`, whatever the shard count now
pub fn legacy_shard_path(main: &Path, shard: usize) -> PathBuf {
    let stem = main.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "atlas".into());
    main.with_file_name(format!("{stem}.s{shard}.db"))
}

/// What a merge did, for the log.
#[derive(Debug, Default)]
pub struct Merged {
    pub shards: Vec<usize>,
    pub rows: i64,
}

/// Fold `sources` into `target` (and their frozen files into its frozen file), one after another. A source is
/// deleted once its copy checks out when `delete` is set. `stop_after` stops after that many transactions, for tests
/// of going on after a stop.
pub fn merge(
    target: &Path,
    sources: &[Source],
    delete: bool,
    stop_after: Option<usize>,
    log: &dyn Fn(&str),
) -> Result<Merged> {
    let mut budget = stop_after;
    let mut merged = Merged::default();
    for source in sources {
        if source.shard == 0 || source.shard >= LEGACY_SHARDS {
            bail!("shard {} can't be folded into shard 0", source.shard);
        }
        let started = Instant::now();
        let conn = crate::db::open_at(target).with_context(|| format!("opening {}", target.display()))?;
        // every commit on disk before a source is deleted: there is nothing to copy again from after that
        conn.execute_batch("pragma synchronous = full;")?;
        conn.execute_batch(SCHEMA)?;
        refuse_an_earlier_builds_merge(&conn)?;
        // a merge begun by a build that left folded-only suffixes past `shard << 32` can't go on with small ids: its
        // rows would end under two ids. Marked when the first source starts
        let begun: i64 = conn.query_row("select count(*) from merge_shards", [], |r| r.get(0))?;
        let dense = crate::store::get_meta(&conn, DENSE_DOMAINS)?.is_some_and(|v| v != 0);
        if begun == 0 {
            conn.execute("insert or replace into meta (key, value) values (?1, 1)", [DENSE_DOMAINS])?;
        } else if !dense {
            bail!(
                "this shard was merged into by an earlier build (domains past shard << 32): start again from the shards as they were"
            );
        }
        domains_not_unique(&conn)?;
        // in shard order only (`sources`): a source after a higher one, merged or started, would take its domains'
        // lowest ids
        let later: Option<i64> =
            conn.query_row("select max(shard) from merge_shards where shard > ?1", [source.shard as i64], |r| {
                r.get(0)
            })?;
        if let Some(later) = later {
            bail!(
                "shard {} comes before shard {later}, merged or started already: shards merge in shard order",
                source.shard
            );
        }
        park_triggers(&conn)?;
        let rows = merge_one(&conn, source, &mut budget, log)?;
        let Some(rows) = rows else { return Ok(merged) };
        let frozen_src = crate::frozen::path(&source.path);
        if frozen_src.exists() {
            let frozen_target = crate::frozen::path(target);
            crate::frozen::create(&frozen_target)?;
            let frozen = crate::db::open_at(&frozen_target)?;
            frozen.execute_batch("pragma synchronous = full;")?;
            frozen.execute_batch(FROZEN_SCHEMA)?;
            let keys = keys_of(&conn, source.shard)?;
            if merge_frozen(&frozen, source.shard, &frozen_src, keys, &mut budget, log)?.is_none() {
                return Ok(merged);
            }
            frozen.query_row("pragma wal_checkpoint(truncate)", [], |_| Ok(()))?;
        }
        conn.execute("update merge_shards set done = 1 where shard = ?1", [source.shard as i64])?;
        conn.query_row("pragma wal_checkpoint(truncate)", [], |_| Ok(()))?;
        merged.shards.push(source.shard);
        merged.rows += rows;
        log(&format!("shard {}: {rows} rows in {:.0} s", source.shard, started.elapsed().as_secs_f64()));
        drop(conn);
        if delete {
            // the frozen file first: the shard's own file is what a run looks for to go on
            remove_family(&frozen_src)?;
            remove_family(&source.path)?;
            sync_dir(&source.path);
            log(&format!("shard {}: removed {}", source.shard, source.path.display()));
        }
    }
    Ok(merged)
}

/// No trigger runs while rows are copied. They would queue the copies again (word index, uncategorized list), count
/// them again (sidecar bytes), send every row to the shared catalog's outbox, where they already are, or index each
/// release's name one at a time: on a trial that took two hours for one shard's 12M releases, where `finish` rebuilds
/// the full text index in one go. Each is kept (`merge_triggers`) and made again by `finish`.
const KEPT_TRIGGERS: [&str; 0] = [];

fn park_triggers(conn: &Connection) -> Result<()> {
    let triggers: Vec<(String, String)> = conn
        .prepare("select name, sql from main.sqlite_master where type = 'trigger'")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let tx = conn.unchecked_transaction()?;
    for (name, sql) in triggers {
        if KEPT_TRIGGERS.contains(&name.as_str()) {
            continue;
        }
        tx.execute("insert or ignore into merge_triggers (name, sql) values (?1, ?2)", params![name, sql])?;
        tx.execute(&format!("drop trigger if exists main.{}", quote(&name)), [])?;
    }
    tx.commit()?;
    Ok(())
}

fn restore_triggers(conn: &Connection) -> Result<()> {
    let has: bool =
        conn.prepare("select 1 from main.sqlite_master where type = 'table' and name = 'merge_triggers'")?.exists([])?;
    if !has {
        return Ok(());
    }
    let parked: Vec<(String, String)> = conn
        .prepare("select name, sql from merge_triggers")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let tx = conn.unchecked_transaction()?;
    for (name, sql) in parked {
        let there: bool =
            tx.prepare("select 1 from main.sqlite_master where type = 'trigger' and name = ?1")?.exists([&name])?;
        if !there {
            tx.execute_batch(&sql)?;
        }
        tx.execute("delete from merge_triggers where name = ?1", [&name])?;
    }
    tx.commit()?;
    Ok(())
}

/// The target's `domains` without its unique index on the suffix: a source's domains come in under their own ids
/// (`shard << 32` on), beside the target's for the same suffix. A lookup by suffix takes the lowest id
/// (`store::Domains`), and every id reads back the same message-id.
pub(crate) fn domains_not_unique(conn: &Connection) -> Result<()> {
    let sql: Option<String> = conn
        .query_row("select sql from main.sqlite_master where type = 'table' and name = 'domains'", [], |r| r.get(0))
        .optional()?;
    if !sql.is_some_and(|s| s.to_ascii_uppercase().contains("UNIQUE")) {
        return Ok(());
    }
    conn.execute_batch(
        "begin immediate;
         create table domains_merged (id INTEGER PRIMARY KEY, suffix TEXT NOT NULL);
         insert into domains_merged (id, suffix) select id, suffix from main.domains;
         drop table main.domains;
         alter table domains_merged rename to domains;
         create index if not exists domains_suffix on domains (suffix, id);
         commit;",
    )?;
    Ok(())
}

/// The key pair of shard `shard` as the merge kept it.
fn keys_of(conn: &Connection, shard: usize) -> Result<(i64, i64)> {
    Ok(conn.query_row("select key0, key1 from merge_shards where shard = ?1", [shard as i64], |r| {
        Ok((r.get::<_, Option<i64>>(0)?.unwrap_or(0), r.get::<_, Option<i64>>(1)?.unwrap_or(0)))
    })?)
}

/// Rows copied from `source` into `conn`'s database, None when `budget` ran out first.
fn merge_one(
    conn: &Connection,
    source: &Source,
    budget: &mut Option<usize>,
    log: &dyn Fn(&str),
) -> Result<Option<i64>> {
    let shard = source.shard as i64;
    attach(conn, &source.path)?;
    let result = (|| {
        let known: Option<i64> =
            conn.query_row("select done from merge_shards where shard = ?1", [shard], |r| r.get(0)).optional()?;
        if known.is_none() {
            // past the target's files (and rows a purge left of files gone), and every other source's moved range,
            // copied yet or not
            let mut file_offset: i64 = conn.query_row(
                "select max(coalesce((select max(id) from main.files), 0),
                            coalesce((select max(file_offset + coalesce(max_file, 0)) from merge_shards), 0))",
                [],
                |r| r.get(0),
            )?;
            for t in FILE_KEYED.into_iter().filter(|t| *t != "release_ids") {
                let there: bool =
                    conn.prepare("select 1 from main.sqlite_master where type = 'table' and name = ?1")?.exists([t])?;
                if there {
                    let m: i64 =
                        conn.query_row(&format!("select coalesce(max(file_id), 0) from main.{t}"), [], |r| r.get(0))?;
                    file_offset = file_offset.max(m);
                }
            }
            let meta = |key: &str| -> Result<Option<i64>> {
                Ok(conn.query_row("select value from src.meta where key = ?1", [key], |r| r.get(0)).optional()?)
            };
            let max_release: i64 = conn.query_row("select coalesce(max(id), 0) from src.releases", [], |r| r.get(0))?;
            let mut max_file: i64 = conn.query_row("select coalesce(max(id), 0) from src.files", [], |r| r.get(0))?;
            // release_ids isn't copied (`skipped`), and has no index on file_id
            for t in FILE_KEYED.into_iter().filter(|t| *t != "release_ids") {
                let there: bool =
                    conn.prepare("select 1 from src.sqlite_master where type = 'table' and name = ?1")?.exists([t])?;
                if there {
                    let m: i64 =
                        conn.query_row(&format!("select coalesce(max(file_id), 0) from src.{t}"), [], |r| r.get(0))?;
                    max_file = max_file.max(m);
                }
            }
            conn.execute(
                "insert into merge_shards (shard, file_offset, domain_offset, key0, key1, max_release, max_file, grouping_from)
                 values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    shard,
                    file_offset,
                    shard << 32,
                    meta("id_key0")?,
                    meta("id_key1")?,
                    max_release,
                    max_file,
                    meta("grouping_from")?
                ],
            )?;
        }
        let (file_offset, domain_offset, key0, key1): (i64, i64, i64, i64) = conn.query_row(
            "select file_offset, domain_offset, coalesce(key0, 0), coalesce(key1, 0) from merge_shards where shard = ?1",
            [shard],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?;
        let moves = Moves { file_offset, domain_offset, key0, key1 };
        let mut rows = 0;
        // the source's domains first: the rest take the target's id for each suffix (`domain_map`)
        match copy_table(conn, shard, "domains", &moves, budget, log)? {
            Some(n) => rows += n,
            None => return Ok(None),
        }
        let (map, unreadable) = map_domains(conn, domain_offset)?;
        for table in tables(conn, "src")? {
            if skipped(&table) || table == "domains" {
                continue;
            }
            match copy_table(conn, shard, &table, &moves, budget, log)? {
                Some(n) => rows += n,
                None => return Ok(None),
            }
            if table == "files" {
                let n = unreadable.load(std::sync::atomic::Ordering::Relaxed);
                if n > 0 {
                    log(&format!("shard {shard}: {n} blobs that won't decode were copied as they are"));
                }
            }
        }
        drop(map);
        merge_meta(conn, shard)?;
        Ok(Some(rows))
    })();
    let _ = conn.execute("detach database src", []);
    result
}

/// A source's domain ids and the target's for the same suffix, and a count of blobs that wouldn't read.
type DomainMap = (std::sync::Arc<std::collections::HashMap<i64, i64>>, std::sync::Arc<std::sync::atomic::AtomicUsize>);

/// Each source domain id and the target's id for its suffix, as a temp table (`temp.domain_map`) for the segment rows
/// and a map for the blobs (`atlas_remap_blob`, registered on `conn`). A blob that won't decode is copied as it is,
/// and counted.
fn map_domains(conn: &Connection, base: i64) -> Result<DomainMap> {
    // a suffix the target has no small id for gets one (the next below `1 << 32`), soo the rows and blobs that move to
    // it stay short; the source's own row at `base` on stays, for its sealed-log places (`blob::shifted`). Once per
    // suffix, soo a rerun adds none
    let dense: i64 =
        conn.query_row("select coalesce(max(id), 0) from main.domains where id < 4294967296", [], |r| r.get(0))?;
    conn.execute(
        "insert into main.domains (id, suffix)
             select ?1 + row_number() over (order by s.id), s.suffix from src.domains s
             where not exists (select 1 from main.domains d where d.suffix = s.suffix and d.id < 4294967296)",
        [dense],
    )?;
    conn.execute_batch(
        "create temp table if not exists domain_map (old INTEGER PRIMARY KEY, new INTEGER NOT NULL);
         delete from temp.domain_map;
         insert into temp.domain_map (old, new)
             select s.id, (select min(d.id) from main.domains d where d.suffix = s.suffix) from src.domains s;",
    )?;
    let map: std::collections::HashMap<i64, i64> = conn
        .prepare("select old, new from temp.domain_map")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let map = std::sync::Arc::new(map);
    let unreadable = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (m, bad) = (map.clone(), unreadable.clone());
    use rusqlite::functions::FunctionFlags;
    conn.create_scalar_function(
        "atlas_remap_blob",
        1,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        move |ctx| match ctx.get_raw(0) {
            rusqlite::types::ValueRef::Blob(b) => Ok(match crate::blob::remap_blob(b, base, &m) {
                Ok(moved) => rusqlite::types::Value::Blob(moved),
                Err(_) => {
                    bad.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    rusqlite::types::Value::Blob(b.to_vec())
                }
            }),
            rusqlite::types::ValueRef::Null => Ok(rusqlite::types::Value::Null),
            rusqlite::types::ValueRef::Integer(i) => Ok(rusqlite::types::Value::Integer(i)),
            rusqlite::types::ValueRef::Real(f) => Ok(rusqlite::types::Value::Real(f)),
            rusqlite::types::ValueRef::Text(t) => {
                Ok(rusqlite::types::Value::Text(String::from_utf8_lossy(t).into_owned()))
            }
        },
    )?;
    Ok((map, unreadable))
}

/// A target an earlier build started merging (`legacy_domains`, `files.domain_base`) isn't gone on with: its sources'
/// blobs weren't renumbered. Merge from copies of the shards as they were.
fn refuse_an_earlier_builds_merge(conn: &Connection) -> Result<()> {
    let table: bool =
        conn.prepare("select 1 from main.sqlite_master where type = 'table' and name = 'legacy_domains'")?.exists([])?;
    let column = columns(conn, "main", "files")?.iter().any(|c| c == "domain_base");
    if table || column {
        bail!(
            "this shard was merged into by an earlier build (legacy_domains or files.domain_base): start again from the shards as they were"
        );
    }
    Ok(())
}

/// What a source's rows are moved by.
struct Moves {
    file_offset: i64,
    domain_offset: i64,
    key0: i64,
    key1: i64,
}

fn attach(conn: &Connection, path: &Path) -> Result<()> {
    let _ = conn.execute("detach database src", []);
    let encoded: String = path
        .display()
        .to_string()
        .chars()
        .map(|c| match c {
            '%' => "%25".to_string(),
            '?' => "%3f".to_string(),
            '#' => "%23".to_string(),
            c => c.to_string(),
        })
        .collect();
    let uri = format!("file:{encoded}?mode=ro");
    conn.execute("attach database ?1 as src", [uri]).with_context(|| format!("attaching {}", path.display()))?;
    Ok(())
}

fn tables(conn: &Connection, schema: &str) -> Result<Vec<String>> {
    let mut stmt =
        conn.prepare(&format!("select name from {schema}.sqlite_master where type = 'table' order by name"))?;
    Ok(stmt.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?)
}

fn columns(conn: &Connection, schema: &str, table: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare("select name from pragma_table_info(?1, ?2)")?;
    Ok(stmt.query_map([table, schema], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?)
}

/// Whether `column` leads the primary key or an index of `schema.table`, soo a range of it is read without a scan.
fn leads_an_index(conn: &Connection, schema: &str, table: &str, column: &str) -> Result<bool> {
    let pk_first: Option<String> = conn
        .query_row("select name from pragma_table_info(?1, ?2) where pk = 1", [table, schema], |r| r.get(0))
        .optional()?;
    if pk_first.as_deref() == Some(column) {
        return Ok(true);
    }
    let indexes: Vec<String> = conn
        .prepare("select name from pragma_index_list(?1, ?2) where partial = 0")?
        .query_map([table, schema], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    for index in indexes {
        let first: Option<String> = conn
            .query_row("select name from pragma_index_info(?1, ?2) where seqno = 0", [&index, schema], |r| r.get(0))
            .optional()?;
        if first.as_deref() == Some(column) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// How one table's rows go over: the target table, its columns and the expression each is filled from, the key the
/// copy goes in ranges of (when it leads an index), and what in the target counts the rows copied.
struct Copy {
    into: String,
    cols: Vec<String>,
    exprs: Vec<String>,
    key: Option<String>,
    /// rows of the source left out (and out of its count)
    filter: Option<String>,
}

fn plan(conn: &Connection, table: &str, m: &Moves) -> Result<Copy> {
    let src_cols = columns(conn, "src", table)?;
    let has = |c: &str| src_cols.iter().any(|s| s == c);
    let lit = |v: i64| v.to_string();
    let copy = match table {
        "domains" => Copy {
            into: "domains".into(),
            cols: vec!["id".into(), "suffix".into()],
            exprs: vec![format!("id + {}", m.domain_offset), "suffix".into()],
            key: Some("id".into()),
            filter: None,
        },
        "collections" => Copy {
            into: "legacy_collections".into(),
            cols: ["key0", "key1", "key", "release_id"].map(String::from).to_vec(),
            exprs: vec![lit(m.key0), lit(m.key1), "key".into(), "release_id".into()],
            key: Some("key".into()),
            filter: None,
        },
        "junk_collections" => Copy {
            into: "legacy_junk_collections".into(),
            cols: ["key0", "key1", "key"].map(String::from).to_vec(),
            exprs: vec![lit(m.key0), lit(m.key1), "key".into()],
            key: Some("key".into()),
            filter: None,
        },
        "frozen_collections" => Copy {
            into: "legacy_frozen_collections".into(),
            cols: ["key0", "key1", "key", "release_id"].map(String::from).to_vec(),
            exprs: vec![lit(m.key0), lit(m.key1), "key".into(), "release_id".into()],
            key: Some("key".into()),
            filter: None,
        },
        _ => {
            let target_cols = columns(conn, "main", table)?;
            if target_cols.is_empty() {
                bail!("{table} is in the source shard but not in the target: update the target's schema first");
            }
            let missing: Vec<&String> = src_cols.iter().filter(|c| !target_cols.contains(c)).collect();
            if !missing.is_empty() {
                bail!("{table}: the target has no column for {missing:?}, which would be lost");
            }
            if has("file_id") && !FILE_KEYED.contains(&table) && !FOREIGN_FILE_ID.contains(&table) {
                bail!(
                    "{table} has a file_id the merge doesn't know whether to move: add it to FILE_KEYED or FOREIGN_FILE_ID"
                );
            }
            let mut cols = Vec::new();
            let mut exprs = Vec::new();
            for c in &src_cols {
                // a fresh sequence number in the target
                if table == "catalog_outbox" && c == "seq" {
                    continue;
                }
                let q = quote(c);
                let expr = match (table, c.as_str()) {
                    ("files", "id") => format!("{q} + {}", m.file_offset),
                    ("files", "blob") => format!("atlas_remap_blob({q})"),
                    // 0 is a message-id stored whole, no domain; the target's id for the suffix (`map_domains`)
                    ("segments", "domain") => format!(
                        "case when {q} = 0 then 0 else coalesce((select new from temp.domain_map where old = {q}), {q} + {}) end",
                        m.domain_offset
                    ),
                    // a release's own row in `release_ids` has file 0, no file
                    ("release_ids", "file_id") => format!("case when {q} = 0 then 0 else {q} + {} end", m.file_offset),
                    (t, "file_id") if FILE_KEYED.contains(&t) => format!("{q} + {}", m.file_offset),
                    _ => q.clone(),
                };
                cols.push(q);
                exprs.push(expr);
            }
            // the keys a range could go by, best first: the first that leads an index is taken (`plan` below)
            let mut keys: Vec<String> = Vec::new();
            if table == "catalog_outbox" {
                keys.push("seq".into());
            } else if table == "files" || table == "releases" {
                keys.push("id".into());
            }
            if FILE_KEYED.contains(&table) {
                keys.push("file_id".into());
            }
            if has("release_id") {
                keys.push("release_id".into());
            }
            if has("id") {
                keys.push("id".into());
            }
            let mut key = None;
            for k in keys {
                if leads_an_index(conn, "src", table, &k)? {
                    key = Some(k);
                    break;
                }
            }
            // the outbox's rows for tables keyed by the shard would go out under shard 0 with another shard's keys
            let copy = Copy { into: table.to_string(), cols, exprs, key, filter: None };
            if table == "catalog_outbox" {
                return Ok(Copy {
                    filter: Some("tbl not in ('collections', 'junk_collections', 'pp_walks')".into()),
                    ..copy
                });
            }
            return Ok(copy);
        }
    };
    let key = match copy.key {
        Some(k) if leads_an_index(conn, "src", table, &k)? => Some(k),
        _ => None,
    };
    Ok(Copy { key, ..copy })
}

/// One table of the source into the target, in key ranges, going on from where its progress says. The rows copied,
/// None when `budget` ran out.
fn copy_table(
    conn: &Connection,
    shard: i64,
    table: &str,
    m: &Moves,
    budget: &mut Option<usize>,
    log: &dyn Fn(&str),
) -> Result<Option<i64>> {
    let progress: Option<(Option<i64>, i64, i64)> = conn
        .query_row(
            "select upto, copied, done from merge_progress where shard = ?1 and tbl = ?2",
            params![shard, table],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    if let Some((_, copied, 1)) = progress {
        return Ok(Some(copied));
    }
    let plan = plan(conn, table, m)?;
    let src = format!("src.{}", quote(table));
    let filter = plan.filter.as_deref().unwrap_or("1");
    let insert = format!(
        "insert into main.{} ({}) select {} from {src} where ({filter})",
        quote(&plan.into),
        plan.cols.join(", "),
        plan.exprs.join(", ")
    );
    let counted = format!("select count(*) from {src} where ({filter})");
    let (mut upto, mut copied) = progress.map_or((None, 0), |(u, c, _)| (u, c));
    match &plan.key {
        None => {
            if !spend(budget) {
                return Ok(None);
            }
            // copied, counted and noted done in one transaction: a crash leaves it all to do again, never half
            let tx = conn.unchecked_transaction()?;
            copied = tx.execute(&insert, [])? as i64;
            let count: i64 = tx.query_row(&counted, [], |r| r.get(0))?;
            if count != copied {
                bail!("shard {shard}, {table}: {copied} rows copied but the source has {count}");
            }
            tx.execute(
                "insert or replace into merge_progress (shard, tbl, upto, copied, done) values (?1, ?2, null, ?3, 1)",
                params![shard, table, copied],
            )?;
            tx.commit()?;
        }
        Some(key) => {
            let k = quote(key);
            let (lo, hi): (Option<i64>, Option<i64>) =
                conn.query_row(&format!("select min({k}), max({k}) from {src}"), [], |r| Ok((r.get(0)?, r.get(1)?)))?;
            if let (Some(lo), Some(hi)) = (lo, hi) {
                // the first key not copied yet: one past what progress says
                let mut start: i128 = upto.map_or(lo as i128, |u| u as i128 + 1);
                // the range a transaction takes, fitted to `BATCH_ROWS` as it goes
                let mut width: i128 = 1024;
                let ranged = format!("{insert} and {k} >= ?1 and {k} <= ?2");
                while start <= hi as i128 {
                    if !spend(budget) {
                        return Ok(None);
                    }
                    let to = (start + width - 1).min(hi as i128);
                    let tx = conn.unchecked_transaction()?;
                    let n = tx.execute(&ranged, params![start as i64, to as i64])? as i64;
                    copied += n;
                    tx.execute(
                        "insert or replace into merge_progress (shard, tbl, upto, copied, done) values (?1, ?2, ?3, ?4, 0)",
                        params![shard, table, to as i64, copied],
                    )?;
                    tx.commit()?;
                    upto = Some(to as i64);
                    start = to + 1;
                    let most = width.saturating_mul(4).min(i64::MAX as i128);
                    width = if n == 0 { most } else { (width * BATCH_ROWS as i128 / n as i128).clamp(1, most) };
                    let _ = conn.query_row("pragma main.wal_checkpoint(truncate)", [], |_| Ok(()));
                }
            }
            let _ = upto;
            let count: i64 = conn.query_row(&counted, [], |r| r.get(0))?;
            if count != copied {
                bail!("shard {shard}, {table}: {copied} rows copied but the source has {count}");
            }
            conn.execute("update merge_progress set done = 1 where shard = ?1 and tbl = ?2", params![shard, table])?;
        }
    }
    log(&format!("shard {shard}: {table} {copied} rows"));
    Ok(Some(copied))
}

/// One transaction more: false when `budget` is spent (tests stop a merge half way this way).
fn spend(budget: &mut Option<usize>) -> bool {
    match budget {
        Some(0) => false,
        Some(n) => {
            *n -= 1;
            true
        }
        None => true,
    }
}

/// The source's counts added to the target's, and the byte totals counted again from what the target holds.
fn merge_meta(conn: &Connection, shard: i64) -> Result<()> {
    let done: i64 = conn
        .query_row("select copied from merge_progress where shard = ?1 and tbl = 'meta'", [shard], |r| r.get(0))
        .optional()?
        .unwrap_or(0);
    if done == 1 {
        return Ok(());
    }
    let tx = conn.unchecked_transaction()?;
    for key in ["releases", "articles", "archive_name_bytes"] {
        tx.execute(
            "insert into main.meta (key, value) select ?1, coalesce((select value from src.meta where key = ?1), 0)
             on conflict(key) do update set value = coalesce(value, 0) + excluded.value",
            [key],
        )?;
    }
    // the shard's uncategorized list is filled once (`uncategorized_listed`): one source not filled leaves it to do
    tx.execute(
        "update main.meta set value = 0 where key = 'uncategorized_listed'
         and coalesce((select value from src.meta where key = 'uncategorized_listed'), 0) != 1",
        [],
    )?;
    tx.execute(
        "insert or replace into merge_progress (shard, tbl, upto, copied, done) values (?1, 'meta', null, 1, 1)",
        [shard],
    )?;
    tx.commit()?;
    Ok(())
}

/// A frozen file into the target's, its collections under the source's key pair.
fn merge_frozen(
    conn: &Connection,
    shard: usize,
    path: &Path,
    (key0, key1): (i64, i64),
    budget: &mut Option<usize>,
    log: &dyn Fn(&str),
) -> Result<Option<i64>> {
    attach(conn, path)?;
    let result = (|| {
        let m = Moves { file_offset: 0, domain_offset: 0, key0, key1 };
        let mut rows = 0;
        for table in tables(conn, "src")? {
            if skipped(&table) {
                continue;
            }
            match copy_table(conn, shard as i64, &table, &m, budget, log)? {
                Some(n) => rows += n,
                None => return Ok(None),
            }
        }
        Ok(Some(rows))
    })();
    let _ = conn.execute("detach database src", []);
    result
}

/// The directory of `path` flushed, soo a deletion in it holds after a power cut.
fn sync_dir(path: &Path) {
    if let Some(dir) = path.parent()
        && let Ok(d) = std::fs::File::open(dir)
    {
        let _ = d.sync_all();
    }
}

/// A database file and everything beside it that belongs to it.
fn remove_family(base: &Path) -> Result<()> {
    // the database last: a -wal left beside no file would be replayed into a new file of that name
    for suffix in ["-wal", "-shm", "-journal", ".sealed-log-cursor", ".catalog-cursor", ".blob-bytes-cursor", ""] {
        let p = PathBuf::from(format!("{}{suffix}", base.display()));
        match std::fs::remove_file(&p) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("removing {}", p.display())),
        }
    }
    Ok(())
}

/// Set in `atlas.db` from a merge's start to its `finish`: no indexer may start in between, as it would write to
/// shards half merged (`db::create_db_at` refuses).
pub const MERGING: &str = "merging_shards";

/// An error while a merge started on the database at `main` hasn't finished, or while the shards from before one
/// shard are still there to merge: a write then would hand out ids in one-shard numbering from the counter in
/// eight-shard numbering, which the merge's copies could meet.
pub fn refuse_if_merging(main: &Path) -> Result<()> {
    let left = legacy_leftovers(main);
    if !left.is_empty() {
        bail!(
            "the shards from before one shard are still there ({}): run `atlas --merge-shards --apply` first",
            left.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", ")
        );
    }
    if !main.exists() {
        return Ok(());
    }
    let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX;
    // a database that can't be read can't be told safe: refused, but for one with no `meta` yet (nothing merged)
    let conn = Connection::open_with_flags(main, flags).context("reading whether a shard merge is in progress")?;
    conn.busy_timeout(std::time::Duration::from_secs(30))?;
    let has_meta: bool =
        conn.prepare("select 1 from sqlite_master where type = 'table' and name = 'meta'")?.exists([])?;
    if !has_meta {
        return Ok(());
    }
    let marked: Option<i64> =
        conn.query_row("select value from meta where key = ?1", [MERGING], |r| r.get(0)).optional()?;
    if marked.is_some_and(|v| v != 0) {
        bail!("a shard merge is in progress: run `atlas --merge-shards --apply` to finish it first");
    }
    Ok(())
}

/// Set in the target when a merge into it begins with small ids for folded-only suffixes (`map_domains`).
const DENSE_DOMAINS: &str = "merge_dense_domains";

/// Set in `atlas.db` once `finish` has put the id counter in one-shard numbering.
const COUNTER_DONE: &str = "merge_counter_done";

/// Whether a merge started on `main` hasn't finished. An error when that can't be read: not taken as no.
pub fn in_progress(main: &Connection) -> Result<bool> {
    // a database with no `meta` yet (being set up) has had no merge
    let has_meta: bool =
        main.prepare("select 1 from main.sqlite_master where type = 'table' and name = 'meta'")?.exists([])?;
    Ok(has_meta && crate::store::get_meta(main, MERGING)?.is_some_and(|v| v != 0))
}

/// Whether the target `target` is left as a merge leaves it before `finish`: its triggers parked. The target's own
/// word on it, for when the marker in `atlas.db` was lost (a power cut between the two files' writes).
pub fn needs_finish(target: &Path) -> Result<bool> {
    if !target.exists() {
        return Ok(false);
    }
    let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = Connection::open_with_flags(target, flags)?;
    conn.busy_timeout(std::time::Duration::from_secs(30))?;
    let has: bool =
        conn.prepare("select 1 from sqlite_master where type = 'table' and name = 'merge_triggers'")?.exists([])?;
    Ok(has && conn.prepare("select 1 from merge_triggers limit 1")?.exists([])?)
}

/// After the last source: the id counter past every id handed out (live, frozen, a source's highest, and once, what
/// the eight-shard numbering would hand out next), as one shard numbers them (`seq` is the id); the full text index
/// made again; the triggers back; the merge marked finished, once the target's changes are on disk. Safe to run
/// again: each run raises the counter past what's there.
pub fn finish(main: &Path, target: &Path) -> Result<()> {
    let conn = crate::db::open_at(target)?;
    // on disk before the marker says the merge is finished
    conn.execute_batch("pragma synchronous = full;")?;
    conn.execute_batch(SCHEMA)?;
    let mut highest: i64 = conn.query_row(
        "select max(coalesce((select max(id) from releases), 0), coalesce((select max(max_release) from merge_shards), 0))",
        [],
        |r| r.get(0),
    )?;
    let frozen = crate::frozen::path(target);
    if frozen.exists() {
        let f = crate::db::open_at(&frozen)?;
        highest = highest.max(f.query_row("select coalesce(max(id), 0) from frozen_releases", [], |r| r.get(0))?);
    }
    let main_conn = crate::db::open_at(main)?;
    main_conn.execute_batch("pragma synchronous = full;")?;
    // the eight-shard counter counted sequences, each worth eight ids: taken into account once, with a note that it
    // was, soo a rerun doesn't take the one-shard counter for eight-shard again
    let converted = crate::store::get_meta(&main_conn, COUNTER_DONE)?.unwrap_or(0) != 0;
    if !converted {
        let next = crate::store::get_meta(&main_conn, "next_seq")?.unwrap_or(0);
        highest = highest.max(next.saturating_mul(LEGACY_SHARDS as i64));
    }
    // the first id in one-shard numbering, noted once (`one_shard_from`), before the counter's conversion is: a rerun
    // after it reckons the same `highest`, one after the conversion couldn't
    if !converted {
        conn.execute("insert or ignore into meta (key, value) values (?1, ?2)", params![ONE_SHARD_FROM, highest + 1])?;
    }
    let tx = main_conn.unchecked_transaction()?;
    crate::store::set_next_seq(&tx, highest + 1)?;
    tx.execute("insert or replace into meta (key, value) values (?1, 1)", [COUNTER_DONE])?;
    tx.commit()?;
    // the copies' names, which no trigger indexed (`KEPT_TRIGGERS`): the index made again from `releases`
    if conn.prepare("select 1 from main.merge_triggers limit 1")?.exists([])? {
        conn.execute("insert into releases_fts(releases_fts) values ('rebuild')", [])?;
    }
    restore_triggers(&conn)?;
    crate::sidecar::recount(&conn)?;
    conn.query_row("pragma wal_checkpoint(truncate)", [], |_| Ok(()))?;
    main_conn.execute("insert or replace into meta (key, value) values (?1, 0)", [MERGING])?;
    main_conn.query_row("pragma wal_checkpoint(truncate)", [], |_| Ok(()))?;
    Ok(())
}

/// Set in the merged shard by `finish`: the first id handed out in one-shard numbering.
const ONE_SHARD_FROM: &str = "one_shard_from";

/// The first release id in one-shard numbering, for the merged shard on `conn`: ids below it were handed out eight to
/// a save (`seq * 8 + shard`), ids from it on one to a save. What `finish` noted, or for a shard a build that didn't
/// note it finished, the first id past the folded shards' that isn't a multiple of eight (shard 0's own ids from
/// before all were; the cutover is within eight below it), or with none yet, no id is dense; 0 for a shard never
/// merged.
pub fn one_shard_from(conn: &Connection) -> Result<i64> {
    if let Some(v) = crate::store::get_meta(conn, ONE_SHARD_FROM)? {
        return Ok(v);
    }
    let merged: bool =
        conn.prepare("select 1 from main.sqlite_master where type = 'table' and name = 'merge_shards'")?.exists([])?;
    if !merged {
        return Ok(0);
    }
    let folded: i64 = conn.query_row("select coalesce(max(max_release), 0) from merge_shards", [], |r| r.get(0))?;
    let dense: Option<i64> =
        conn.query_row("select min(id) from releases where id > ?1 and id % 8 != 0", [folded], |r| r.get(0))?;
    Ok(dense.map_or(i64::MAX, |id| id - id % 8))
}

/// Note the first one-shard id in the merged shard beside `main` when `finish` didn't (an earlier build): the fallback
/// `one_shard_from` reckons drifts once the first dense releases freeze, soo it's kept the first time it can be told,
/// and with no dense release yet it's the counter (`main_conn`'s `next_seq`), which nothing has handed out from.
pub fn note_one_shard_from(main: &Path, main_conn: &Connection) -> Result<()> {
    let target = crate::store::shard_path(main, 0);
    if !target.exists() {
        return Ok(());
    }
    let conn = crate::db::open_at(&target)?;
    let merged: bool =
        conn.prepare("select 1 from main.sqlite_master where type = 'table' and name = 'merge_shards'")?.exists([])?;
    if !merged || crate::store::get_meta(&conn, ONE_SHARD_FROM)?.is_some() {
        return Ok(());
    }
    let from = match one_shard_from(&conn)? {
        i64::MAX => crate::store::get_meta(main_conn, "next_seq")?.unwrap_or(i64::MAX),
        found => found,
    };
    if from != i64::MAX {
        conn.execute("insert or ignore into meta (key, value) values (?1, ?2)", params![ONE_SHARD_FROM, from])?;
    }
    Ok(())
}

/// The shards a merge into `target` has begun, and whether each is copied whole.
fn started_shards(target: &Path) -> Result<Vec<(usize, bool)>> {
    let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = Connection::open_with_flags(target, flags)?;
    conn.busy_timeout(std::time::Duration::from_secs(30))?;
    let has: bool =
        conn.prepare("select 1 from sqlite_master where type = 'table' and name = 'merge_shards'")?.exists([])?;
    if !has {
        return Ok(Vec::new());
    }
    let shards = conn
        .prepare("select shard, coalesce(done, 0) from merge_shards order by shard")?
        .query_map([], |r| Ok((r.get::<_, i64>(0)? as usize, r.get::<_, i64>(1)? != 0)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(shards)
}

/// `atlas --merge-shards [--apply]`.
pub fn run(args: &[String]) -> i32 {
    let apply = args.iter().any(|a| a == "--apply");
    if crate::procs::indexer_alive() {
        println!("stop indexing first");
        return 1;
    }
    let main = crate::paths::database();
    let target = legacy_shard_path(&main, 0);
    // a compaction's swap of a shard cut short: its backup is what holds the shard (the shard itself may be gone), and the merge doesn't put it
    // back
    let backups: Vec<String> = legacy_leftovers(&main)
        .into_iter()
        .filter(|p| p.to_string_lossy().ends_with(".precompact.db"))
        .map(|p| p.display().to_string())
        .collect();
    if !backups.is_empty() {
        println!("a compaction's backup is beside a shard: put it back or remove it first: {}", backups.join(", "));
        return 1;
    }
    let todo = sources(&main);
    if todo.is_empty() {
        // the last source went but its run stopped before `finish`: the marker says so, or the target does when the
        // marker was lost with it
        let marked = match crate::db::open_at(&main).map_err(anyhow::Error::from).and_then(|c| in_progress(&c)) {
            Ok(m) => m,
            Err(e) => {
                println!("couldnt read {}: {e:#}", main.display());
                return 1;
            }
        };
        let left = match needs_finish(&target) {
            Ok(n) => n,
            Err(e) => {
                println!("couldnt read {}: {e:#}", target.display());
                return 1;
            }
        };
        if marked || left {
            if !apply {
                println!("the merge's last step is still to do (its shards are gone): pass --apply to finish it");
                return 0;
            }
            let _lock = match crate::compact::Lock::take_unchecked(&main) {
                Ok(lock) => lock,
                Err(e) => {
                    println!("couldnt take the database lock: {e:#}");
                    return 1;
                }
            };
            return match finish(&main, &target) {
                Ok(()) => {
                    println!("finished the merge that stopped after its last shard");
                    0
                }
                Err(e) => {
                    println!("couldnt finish: {e:#}");
                    1
                }
            };
        }
        println!("nothing to merge: {} is the only shard", target.display());
        return 0;
    }
    let free = crate::frozen::free_bytes(&main).unwrap_or(0);
    let size = |p: &Path| std::fs::metadata(p).map_or(0, |m| m.len());
    // a source a run copied whole and stopped before deleting needs no room; one it stopped inside is counted whole
    // (what's left of it isn't known closer than that)
    let started = started_shards(&target).unwrap_or_default();
    let copied: Vec<usize> = started.iter().filter(|(_, done)| *done).map(|(s, _)| *s).collect();
    let biggest = todo
        .iter()
        .filter(|s| !copied.contains(&s.shard))
        .map(|s| size(&s.path) + size(&crate::frozen::path(&s.path)))
        .max()
        .unwrap_or(0);
    if !started.is_empty() {
        let begun: Vec<usize> = started.iter().map(|(s, _)| *s).collect();
        println!("going on with the merge a run stopped in (shards {begun:?} begun already)");
    }
    println!("merging {} shards into {}:", todo.len(), target.display());
    for s in &todo {
        let size = std::fs::metadata(&s.path).map_or(0, |m| m.len());
        println!("  s{} {:.1} GB", s.shard, size as f64 / 1e9);
    }
    println!(
        "the largest needs about {:.0} GB free while it's copied; {:.0} GB are free",
        biggest as f64 / 1e9,
        free as f64 / 1e9
    );
    if !apply {
        println!("dry run: pass --apply to merge (each shard is deleted once its copy checks out)");
        return 0;
    }
    if free < biggest + (20 << 30) {
        println!("not enough free space: free some first");
        return 1;
    }
    let _lock = match crate::compact::Lock::take_unchecked(&main) {
        Ok(lock) => lock,
        Err(e) => {
            println!("couldnt take the database lock: {e:#}");
            return 1;
        }
    };
    if let Err(e) = crate::compact::refuse_unresolved_backups(&main) {
        println!("{e:#}");
        return 1;
    }
    // no indexer starts until `finish`, even when this run stops half way
    if let Err(e) = crate::db::open_at(&main).and_then(|c| {
        // on disk before any shard is deleted
        c.execute_batch("pragma synchronous = full;")?;
        c.execute("insert or replace into meta (key, value) values (?1, 1)", [MERGING])?;
        c.query_row("pragma wal_checkpoint(truncate)", [], |_| Ok(()))?;
        Ok(())
    }) {
        println!("couldnt mark the merge as started: {e:#}");
        return 1;
    }
    let result = merge(&target, &todo, true, None, &|m| println!("{m}")).and_then(|m| {
        finish(&main, &target)?;
        Ok(m)
    });
    match result {
        Ok(m) => {
            println!("merged shards {:?}, {} rows", m.shards, m.rows);
            0
        }
        Err(e) => {
            println!("stopped: {e:#}\nrun it again to go on from where it stopped");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{Article, Release};
    use crate::store;

    fn art(file: &str, part: i64, id: &str, domain: &str) -> Article {
        Article {
            message_id: format!("<{id}@{domain}>"),
            subject: format!("\"{file}\" yEnc ({part}/4)"),
            filename: Some(file.into()),
            part: Some(part),
            total_parts: Some(4),
            bytes: 100,
            ..Default::default()
        }
    }

    /// a group name for each shard there was
    fn groups() -> Vec<String> {
        (0..LEGACY_SHARDS).map(|s| format!("alt.binaries.g{s}")).collect()
    }

    fn count(conn: &Connection, sql: &str) -> i64 {
        conn.query_row(sql, [], |r| r.get(0)).unwrap()
    }

    /// Eight shards as there were, two releases each, their articles under a domain of their own and one rare: shard
    /// 0 is `main`'s own, each other made as a database of its own (its own secret, its ids apart) and put in place.
    fn fixture(main: &Path) -> Vec<String> {
        fixture_sharing(main, &[])
    }

    /// `fixture`, with a second file in release 1 of each of `sharing`, its articles under `both.example`: a domain
    /// those shards have and shard 0 hasn't.
    fn fixture_sharing(main: &Path, sharing: &[usize]) -> Vec<String> {
        crate::db::create_db_at(main).unwrap();
        let groups = groups();
        for (s, g) in groups.iter().enumerate() {
            let releases: Vec<Release> = (0..2)
                .map(|r| {
                    let mut articles = vec![
                        art("a.rar", 1, &format!("s{s}r{r}a1"), &format!("own{s}.example")),
                        art("a.rar", 2, &format!("s{s}r{r}a2"), &format!("own{s}.example")),
                        art("a.rar", 3, &format!("s{s}r{r}a3"), &format!("own{s}.example")),
                        art("a.rar", 4, &format!("s{s}r{r}a4"), "rare.example"),
                    ];
                    if r == 1 && sharing.contains(&s) {
                        articles.extend((1..=4).map(|n| art("b.rar", n, &format!("s{s}r{r}b{n}"), "both.example")));
                    }
                    Release {
                        name: format!("Rel.{s}.{r}"),
                        group: g.clone(),
                        collection: Some(format!("post {s} {r}")),
                        articles,
                        ..Default::default()
                    }
                })
                .collect();
            if s == 0 {
                store::save(main, &releases).unwrap();
                continue;
            }
            let other = tempfile::tempdir().unwrap();
            let other_main = other.path().join("atlas.db");
            crate::db::create_db_at(&other_main).unwrap();
            store::set_next_seq(&crate::db::open_at(&other_main).unwrap(), 1000 * s as i64).unwrap();
            store::save(&other_main, &releases).unwrap();
            let made = legacy_shard_path(&other_main, 0);
            crate::db::open_at(&made).unwrap().query_row("pragma wal_checkpoint(truncate)", [], |_| Ok(())).unwrap();
            std::fs::copy(&made, legacy_shard_path(main, s)).unwrap();
        }
        groups
    }

    fn totals(main: &Path) -> (i64, i64, i64) {
        let (mut releases, mut files, mut segments) = (0, 0, 0);
        for s in 0..LEGACY_SHARDS {
            let p = legacy_shard_path(main, s);
            if !p.exists() {
                continue;
            }
            let c = crate::db::open_at(&p).unwrap();
            releases += count(&c, "select count(*) from releases");
            files += count(&c, "select count(*) from files");
            segments += count(&c, "select count(*) from segments");
        }
        (releases, files, segments)
    }

    #[test]
    fn every_shard_folds_into_the_first_and_is_removed() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        fixture(&main);
        let before = totals(&main);
        assert_eq!(before.0, 16);
        let before_whole: i64 = (0..LEGACY_SHARDS)
            .map(|s| {
                count(
                    &crate::db::open_at(&legacy_shard_path(&main, s)).unwrap(),
                    "select count(*) from segments where domain = 0",
                )
            })
            .sum();
        let todo = sources(&main);
        assert_eq!(todo.len(), LEGACY_SHARDS - 1);
        let target = legacy_shard_path(&main, 0);
        let own_ids = count(&crate::db::open_at(&target).unwrap(), "select count(*) from release_ids");
        let merged = merge(&target, &todo, true, None, &|_| {}).unwrap();
        assert_eq!(merged.shards.len(), LEGACY_SHARDS - 1);
        finish(&main, &target).unwrap();

        for s in 1..LEGACY_SHARDS {
            assert!(!legacy_shard_path(&main, s).exists(), "s{s} removed");
        }
        assert_eq!(totals(&main), before, "every release, file and article");
        let c = crate::db::open_at(&target).unwrap();
        assert_eq!(count(&c, "select count(*) from releases_fts where releases_fts match 'Rel'"), 16, "searchable");
        // no file id twice, every article's file there
        assert_eq!(count(&c, "select count(distinct id) from files"), before.1);
        assert_eq!(count(&c, "select count(*) from segments where file_id not in (select id from files)"), 0);
        // each source's domains kept apart, its articles pointing at them; a message-id stored whole stays so
        // each source's domains kept apart under its base, its articles pointing at them; a whole message-id stays so
        assert_eq!(count(&c, "select count(*) from domains where id >= 4294967296"), LEGACY_SHARDS as i64 - 1);
        assert_eq!(
            count(
                &c,
                "select count(*) from segments s where s.domain != 0 and s.domain not in (select id from domains)"
            ),
            0
        );
        assert_eq!(count(&c, "select count(*) from segments where domain = 0"), before_whole);
        let suffix_of = |name: &str| -> String {
            c.query_row(
                "select d.suffix from segments s join files f on f.id = s.file_id join releases r on r.id = f.release_id
                 join domains d on d.id = s.domain where r.name = ?1 and s.part = 3",
                [name],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(suffix_of("Rel.5.1"), "@own5.example>");
        assert_eq!(suffix_of("Rel.0.1"), "@own0.example>");
        // collections kept under their shard's key pair, and the pairs kept
        assert_eq!(count(&c, "select count(distinct key0) from legacy_collections"), LEGACY_SHARDS as i64 - 1);
        assert_eq!(count(&c, "select count(*) from merge_shards where done = 1 and key0 is not null"), 7);
        assert_eq!(count(&c, "select value from meta where key = 'releases'"), 16);
        // release_ids, hashed with each source's secret, isn't copied: it's built again under the target's
        assert_eq!(count(&c, "select count(*) from release_ids"), own_ids);
        // ids go on past every release
        let max = count(&c, "select max(id) from releases");
        assert!(store::get_meta(&crate::db::open_at(&main).unwrap(), "next_seq").unwrap().unwrap() > max);
    }

    #[test]
    fn a_merge_stopped_half_way_goes_on_where_it_stopped() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        fixture(&main);
        let before = totals(&main);
        let target = legacy_shard_path(&main, 0);
        for stop in [3, 7, 11] {
            let merged = merge(&target, &sources(&main), true, Some(stop), &|_| {}).unwrap();
            assert!(merged.shards.len() < LEGACY_SHARDS - 1, "stopped early");
        }
        merge(&target, &sources(&main), true, None, &|_| {}).unwrap();
        assert!(sources(&main).is_empty());
        assert_eq!(totals(&main), before, "nothing twice, nothing lost");
    }

    #[test]
    fn a_table_with_an_unknown_file_id_stops_the_merge() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        fixture(&main);
        let s1 = legacy_shard_path(&main, 1);
        crate::db::open_at(&s1).unwrap().execute_batch("create table mystery (file_id INTEGER PRIMARY KEY);").unwrap();
        crate::db::open_at(&legacy_shard_path(&main, 0))
            .unwrap()
            .execute_batch("create table mystery (file_id INTEGER PRIMARY KEY);")
            .unwrap();
        let s1_only = vec![Source { shard: 1, path: s1.clone() }];
        let e = merge(&legacy_shard_path(&main, 0), &s1_only, true, None, &|_| {}).unwrap_err();
        assert!(format!("{e:#}").contains("mystery"), "{e:#}");
        assert!(s1.exists(), "left as it was");
    }

    #[test]
    fn no_indexer_starts_on_a_merge_half_done_and_a_rerun_finishes_it() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        fixture(&main);
        let m = crate::db::open_at(&main).unwrap();
        m.execute("insert or replace into meta (key, value) values (?1, 1)", [MERGING]).unwrap();
        let target = legacy_shard_path(&main, 0);
        merge(&target, &sources(&main), true, None, &|_| {}).unwrap();
        let e = crate::db::create_db_at(&main).unwrap_err();
        assert!(format!("{e:#}").contains("merge"), "{e:#}");
        assert!(crate::compact::try_hold_off_compaction(&main).is_err(), "nor any other writer");
        assert!(crate::compact::Lock::take(&main).is_err(), "nor purge, the walks or a compaction");
        // the target itself says the last step is to do, marker or no marker
        assert!(needs_finish(&target).unwrap());
        m.execute("insert or replace into meta (key, value) values (?1, 0)", [MERGING]).unwrap();
        assert!(!in_progress(&m).unwrap() && needs_finish(&target).unwrap(), "the marker lost, the target still knows");
        // the run stopped before finishing: finishing again is safe
        finish(&main, &target).unwrap();
        assert!(!needs_finish(&target).unwrap(), "finished");
        let next = |m: &Connection| store::get_meta(m, "next_seq").unwrap().unwrap();
        let once = next(&m);
        finish(&main, &target).unwrap();
        assert_eq!(next(&m), once, "the counter converted once");
        // an id handed out in between (eight-shard numbering, a split before the cutover): a rerun goes past it
        let c = crate::db::open_at(&target).unwrap();
        let high = once * 8 + 40;
        c.execute("insert into releases (id, name, group_name) values (?1, 'Late', 'g')", [high]).unwrap();
        finish(&main, &target).unwrap();
        assert!(next(&m) > high, "{} past {high}", next(&m));
        assert!(crate::compact::try_hold_off_compaction(&main).unwrap().is_some());
        // past the merge guard (the eight-shard layout's own check of its files is the next PR's)
        if let Err(e) = crate::db::create_db_at(&main) {
            assert!(!format!("{e:#}").contains("merge is in progress"), "{e:#}");
        }
        let c = crate::db::open_at(&target).unwrap();
        assert_eq!(count(&c, "select count(*) from merge_triggers"), 0, "triggers back");
        assert!(count(&c, "select count(*) from sqlite_master where type = 'trigger'") > 1);
    }

    /// A source's file sealed in a blob, the way the writer leaves a whole file: its rows packed and gone.
    fn seal_first_file(shard: &Path) {
        let c = crate::db::open_at(shard).unwrap();
        let file: i64 = c.query_row("select min(id) from files", [], |r| r.get(0)).unwrap();
        let segs: Vec<crate::blob::Seg> = c
            .prepare("select part, bytes, domain, local from segments where file_id = ?1")
            .unwrap()
            .query_map([file], |r| {
                Ok(crate::blob::Seg { part: r.get(0)?, bytes: r.get(1)?, domain: r.get(2)?, local: r.get(3)? })
            })
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert!(segs.iter().any(|s| s.domain != 0), "a domain to move");
        c.execute("update files set blob = ?1 where id = ?2", params![crate::blob::encode(&segs), file]).unwrap();
        c.execute("delete from segments where file_id = ?1", [file]).unwrap();
    }

    #[test]
    fn merged_releases_read_back_their_articles_and_take_their_posts_late_articles() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        let groups = fixture(&main);
        seal_first_file(&legacy_shard_path(&main, 5));
        let ids_of = |main: &Path, name: &str| -> Vec<String> {
            let id: i64 = (0..LEGACY_SHARDS)
                .filter(|s| legacy_shard_path(main, *s).exists())
                .find_map(|s| {
                    crate::db::open_at(&legacy_shard_path(main, s))
                        .unwrap()
                        .query_row("select id from releases where name = ?1", [name], |r| r.get(0))
                        .ok()
                })
                .unwrap();
            let c = crate::db::open_at(&legacy_shard_path(main, 0)).unwrap();
            let mut rows: Vec<String> = Vec::new();
            let blob: Option<Vec<u8>> = c
                .query_row("select blob from files where release_id = ?1 and blob is not null", [id], |r| r.get(0))
                .optional()
                .unwrap();
            for seg in blob.map(|b| crate::blob::decode(&b).unwrap()).unwrap_or_default() {
                let suffix: Option<String> = (seg.domain != 0).then(|| {
                    c.query_row("select suffix from domains where id = ?1", [seg.domain], |r| r.get(0)).unwrap()
                });
                rows.push(store::decode(&seg.local, suffix.as_deref()));
            }
            rows.sort();
            rows
        };
        let target = legacy_shard_path(&main, 0);
        merge(&target, &sources(&main), true, None, &|_| {}).unwrap();
        finish(&main, &target).unwrap();
        // the sealed file of shard 5: its blob, read through the merged domains, gives the same message-ids
        let read = ids_of(&main, "Rel.5.0");
        assert_eq!(
            read,
            ["<s5r0a1@own5.example>", "<s5r0a2@own5.example>", "<s5r0a3@own5.example>", "<s5r0a4@rare.example>"]
        );
        // and the release's articles, as an NZB lists them
        let conn = crate::db::open_with_shards(&main).unwrap();
        let id: i64 = conn.query_row("select id from s0.releases where name = 'Rel.5.0'", [], |r| r.get(0)).unwrap();
        let mut nzb: Vec<String> = store::articles(&conn, id).unwrap().into_iter().map(|a| a.message_id).collect();
        nzb.sort();
        assert_eq!(nzb, read);
        // a late article of a source's post joins its release, under the target's secret from then on
        let before = count(&crate::db::open_at(&target).unwrap(), "select count(*) from releases");
        let late = Release {
            name: "Rel.3.1".into(),
            group: groups[3].clone(),
            collection: Some("post 3 1".into()),
            articles: vec![art("b.rar", 1, "s3r1b1", "own3.example")],
            ..Default::default()
        };
        store::save(&main, &[late]).unwrap();
        let c = crate::db::open_at(&target).unwrap();
        assert_eq!(count(&c, "select count(*) from releases"), before, "no new release");
        assert_eq!(
            count(&c, "select count(*) from files f join releases r on r.id = f.release_id where r.name = 'Rel.3.1'"),
            2
        );
        assert_eq!(
            count(
                &c,
                "select count(*) from legacy_collections where release_id = (select id from releases where name = 'Rel.3.1')"
            ),
            0,
            "moved"
        );
        // the same articles again, of a file kept as rows (Rel.3.0) and of one sealed in a blob (Rel.5.0): nothing added
        let stats = |name: &str| -> (i64, i64, i64) {
            c.query_row(
                "select r.size, r.parts,
                        (select count(*) from segments s join files f on f.id = s.file_id where f.release_id = r.id)
                 from releases r where r.name = ?1",
                [name],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap()
        };
        let (rows_before, sealed_before) = (stats("Rel.3.0"), stats("Rel.5.0"));
        for (s, name) in [(3usize, "Rel.3.0"), (5, "Rel.5.0")] {
            let again = Release {
                name: name.into(),
                group: groups[s].clone(),
                collection: Some(format!("post {s} 0")),
                articles: vec![
                    art("a.rar", 1, &format!("s{s}r0a1"), &format!("own{s}.example")),
                    art("a.rar", 2, &format!("s{s}r0a2"), &format!("own{s}.example")),
                    art("a.rar", 3, &format!("s{s}r0a3"), &format!("own{s}.example")),
                    art("a.rar", 4, &format!("s{s}r0a4"), "rare.example"),
                ],
                ..Default::default()
            };
            store::save(&main, &[again]).unwrap();
        }
        assert_eq!(stats("Rel.3.0"), rows_before, "rows: seen again, not added");
        assert_eq!(stats("Rel.5.0"), sealed_before, "sealed: seen again, not added");
    }

    #[test]
    fn a_domain_two_shards_share_ends_under_one_id_whatever_their_sizes() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        let groups = fixture_sharing(&main, &[2, 5]);
        // shard 2 the bigger: merged by size, shard 5 would come first and its id for the domain stop being the lowest
        crate::db::open_at(&legacy_shard_path(&main, 2))
            .unwrap()
            .execute_batch(
                "insert into meta (key, value) values ('pad', randomblob(4000000));
                 pragma wal_checkpoint(truncate);",
            )
            .unwrap();
        let todo = sources(&main);
        assert_eq!(todo.iter().map(|s| s.shard).collect::<Vec<_>>(), (1..LEGACY_SHARDS).collect::<Vec<_>>());
        // no writer before the merge: its ids would come from the counter in eight-shard numbering
        assert!(crate::compact::try_hold_off_compaction(&main).is_err());
        assert!(crate::compact::Lock::take(&main).is_err());
        let target = legacy_shard_path(&main, 0);
        merge(&target, &todo, true, None, &|_| {}).unwrap();
        finish(&main, &target).unwrap();
        let c = crate::db::open_at(&target).unwrap();
        let shared = "select count(distinct s.domain) from segments s join domains d on d.id = s.domain
                      where d.suffix = '@both.example>'";
        assert_eq!(count(&c, shared), 1, "one id for the domain");
        // every row under a small id (a source's own rows past `1 << 32` are kept for sealed-log places only)
        assert_eq!(count(&c, "select count(*) from segments where domain >= 4294967296"), 0);
        assert_eq!(count(&c, "select count(*) from domains where id < 4294967296"), 1 + 7 + 1, "own0..7, both");
        // shard 5's article again: found under that id, not added
        let segments = "select count(*) from segments";
        let before = count(&c, segments);
        let again = Release {
            name: "Rel.5.1".into(),
            group: groups[5].clone(),
            collection: Some("post 5 1".into()),
            articles: (1..=4).map(|n| art("b.rar", n, &format!("s5r1b{n}"), "both.example")).collect(),
            ..Default::default()
        };
        store::save(&main, &[again]).unwrap();
        assert_eq!(count(&c, segments), before, "seen again, not added");
        // a domain new since takes the next small id
        let fresh = Release {
            name: "Fresh".into(),
            group: groups[0].clone(),
            collection: Some("fresh".into()),
            articles: (1..=4).map(|n| art("c.rar", n, &format!("f{n}"), "fresh.example")).collect(),
            ..Default::default()
        };
        store::save(&main, &[fresh]).unwrap();
        assert_eq!(count(&c, "select id from domains where suffix = '@fresh.example>'"), 10);
        // and a merge given its sources out of order stops
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        fixture(&main);
        let target = legacy_shard_path(&main, 0);
        let mut todo = sources(&main);
        todo.swap(0, 1);
        merge(&target, &todo[..1], true, None, &|_| {}).unwrap();
        let e = merge(&target, &todo[1..], true, None, &|_| {}).unwrap_err();
        assert!(format!("{e:#}").contains("shard order"), "{e:#}");
    }

    #[test]
    fn the_first_one_shard_id_is_noted_or_found_past_the_folded_ids() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "create table meta (key TEXT PRIMARY KEY, value INTEGER);
             create table releases (id INTEGER PRIMARY KEY, name TEXT);",
        )
        .unwrap();
        assert_eq!(one_shard_from(&c).unwrap(), 0, "never merged");
        c.execute_batch(SCHEMA).unwrap();
        c.execute(
            "insert into merge_shards (shard, file_offset, domain_offset, max_release) values (3, 0, 0, 1003)",
            [],
        )
        .unwrap();
        // shard 0's own ids past the folded ones, all multiples of eight, and none dense yet
        c.execute_batch("insert into releases (id) values (1000), (1008), (1016);").unwrap();
        assert_eq!(one_shard_from(&c).unwrap(), i64::MAX);
        c.execute_batch("insert into releases (id) values (1025), (1026);").unwrap();
        assert_eq!(one_shard_from(&c).unwrap(), 1024);
        c.execute("insert into meta (key, value) values (?1, 1017)", [ONE_SHARD_FROM]).unwrap();
        assert_eq!(one_shard_from(&c).unwrap(), 1017, "noted");
    }

    #[test]
    fn an_indexer_start_notes_the_first_one_shard_id_an_earlier_finish_didnt() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        fixture(&main);
        let target = legacy_shard_path(&main, 0);
        merge(&target, &sources(&main), true, None, &|_| {}).unwrap();
        finish(&main, &target).unwrap();
        let c = crate::db::open_at(&target).unwrap();
        let noted = store::get_meta(&c, ONE_SHARD_FROM).unwrap().unwrap();
        let next = store::get_meta(&crate::db::open_at(&main).unwrap(), "next_seq").unwrap().unwrap();
        assert_eq!(noted, next, "the counter, which nothing has handed out from");
        // as a finish of an earlier build leaves it
        c.execute("delete from meta where key = ?1", [ONE_SHARD_FROM]).unwrap();
        crate::db::create_db_at(&main).unwrap();
        assert_eq!(store::get_meta(&c, ONE_SHARD_FROM).unwrap(), Some(next));
    }
}
