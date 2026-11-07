//! `atlas --merge-shards`: the eight shards folded into `atlas.s0.db`, one at a time, for the move to one shard (and
//! later to Postgres). Offline: the indexer stopped, the compaction lock held.
//!
//! What collides between shards is moved out of the way rather than rewritten:
//! - file ids count from 1 in each shard: a source's are moved past the target's highest (`file_offset`);
//! - message-id domains are numbered per shard, and sealed blobs (some kept in the sealed log, out of reach) hold
//!   those numbers: a source's domains go to `legacy_domains` under `shard << 32`, its segments' domains move the
//!   same (0, a message-id stored whole, stays 0), and each of its files records the move (`files.domain_base`) for
//!   its blob's numbers;
//! - collection keys and message-id hashes are keyed per shard, and their strings aren't kept: a source's collections
//!   go to `legacy_collections` under its key pair, and the pair is kept (`merge_shards`) for what hashes its
//!   releases' message-ids. Release ids already say which shard they came from (`id % 8`).
//!
//! Everything else is keyed by release id, unique across shards, and copied as it is. Each table goes over in key
//! ranges, one transaction each, its progress kept (`merge_progress`), soo a stop or a crash goes on from there. A
//! source is deleted only once every table's copied rows match its count, soo at most one shard's worth of space is
//! taken at a time.

use std::collections::BTreeSet;
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
create table if not exists legacy_domains (id INTEGER PRIMARY KEY, suffix TEXT NOT NULL);
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
        || table.starts_with("legacy_")
}

/// One source's plan: which shard, its file, and its frozen file.
#[derive(Clone, Debug)]
pub struct Source {
    pub shard: usize,
    pub path: PathBuf,
}

/// The shards to fold into shard 0 of `main`, smallest first.
pub fn sources(main: &Path) -> Vec<Source> {
    let mut found: Vec<(u64, Source)> = (1..LEGACY_SHARDS)
        .map(|shard| (shard, legacy_shard_path(main, shard)))
        .filter(|(_, p)| p.exists())
        .map(|(shard, path)| (std::fs::metadata(&path).map_or(0, |m| m.len()), Source { shard, path }))
        .collect();
    found.sort_by_key(|(size, s)| (*size, s.shard));
    found.into_iter().map(|(_, s)| s).collect()
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
        conn.execute_batch(SCHEMA)?;
        add_column(&conn, "files", "domain_base", "INTEGER")?;
        park_triggers(&conn)?;
        let rows = merge_one(&conn, source, &mut budget, log)?;
        let Some(rows) = rows else { return Ok(merged) };
        let frozen_src = crate::frozen::path(&source.path);
        if frozen_src.exists() {
            let frozen_target = crate::frozen::path(target);
            crate::frozen::create(&frozen_target)?;
            let frozen = crate::db::open_at(&frozen_target)?;
            frozen.execute_batch(FROZEN_SCHEMA)?;
            let keys = keys_of(&conn, source.shard)?;
            if merge_frozen(&frozen, source.shard, &frozen_src, keys, &mut budget, log)?.is_none() {
                return Ok(merged);
            }
        }
        conn.execute("update merge_shards set done = 1 where shard = ?1", [source.shard as i64])?;
        merged.shards.push(source.shard);
        merged.rows += rows;
        log(&format!("shard {}: {rows} rows in {:.0} s", source.shard, started.elapsed().as_secs_f64()));
        drop(conn);
        if delete {
            remove_family(&source.path)?;
            remove_family(&frozen_src)?;
            log(&format!("shard {}: removed {}", source.shard, source.path.display()));
        }
    }
    Ok(merged)
}

/// The full text index's insert trigger is all that runs while rows are copied. The rest would queue the copies again
/// (word index, uncategorized list), count them again (sidecar bytes) or send every row to the shared catalog's
/// outbox, where they already are. Each is kept (`merge_triggers`) and made again by `finish`.
const KEPT_TRIGGERS: [&str; 1] = ["releases_ai"];

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
            let file_offset: i64 = conn.query_row("select coalesce(max(id), 0) from main.files", [], |r| r.get(0))?;
            let meta = |key: &str| -> Result<Option<i64>> {
                Ok(conn.query_row("select value from src.meta where key = ?1", [key], |r| r.get(0)).optional()?)
            };
            let max_release: i64 = conn.query_row("select coalesce(max(id), 0) from src.releases", [], |r| r.get(0))?;
            conn.execute(
                "insert into merge_shards (shard, file_offset, domain_offset, key0, key1, max_release)
                 values (?1, ?2, ?3, ?4, ?5, ?6)",
                params![shard, file_offset, shard << 32, meta("id_key0")?, meta("id_key1")?, max_release],
            )?;
        }
        let (file_offset, domain_offset, key0, key1): (i64, i64, i64, i64) = conn.query_row(
            "select file_offset, domain_offset, coalesce(key0, 0), coalesce(key1, 0) from merge_shards where shard = ?1",
            [shard],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?;
        let moves = Moves { file_offset, domain_offset, key0, key1 };
        let mut rows = 0;
        for table in tables(conn, "src")? {
            if skipped(&table) {
                continue;
            }
            match copy_table(conn, shard, &table, &moves, budget, log)? {
                Some(n) => rows += n,
                None => return Ok(None),
            }
        }
        merge_meta(conn, shard)?;
        Ok(Some(rows))
    })();
    let _ = conn.execute("detach database src", []);
    result
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
    let uri = format!("file:{}?mode=ro", path.display());
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
        .prepare("select name from pragma_index_list(?1, ?2)")?
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
}

fn plan(conn: &Connection, table: &str, m: &Moves) -> Result<Copy> {
    let src_cols = columns(conn, "src", table)?;
    let has = |c: &str| src_cols.iter().any(|s| s == c);
    let lit = |v: i64| v.to_string();
    let copy = match table {
        "domains" => Copy {
            into: "legacy_domains".into(),
            cols: vec!["id".into(), "suffix".into()],
            exprs: vec![format!("id + {}", m.domain_offset), "suffix".into()],
            key: Some("id".into()),
        },
        "collections" => Copy {
            into: "legacy_collections".into(),
            cols: ["key0", "key1", "key", "release_id"].map(String::from).to_vec(),
            exprs: vec![lit(m.key0), lit(m.key1), "key".into(), "release_id".into()],
            key: Some("key".into()),
        },
        "junk_collections" => Copy {
            into: "legacy_junk_collections".into(),
            cols: ["key0", "key1", "key"].map(String::from).to_vec(),
            exprs: vec![lit(m.key0), lit(m.key1), "key".into()],
            key: Some("key".into()),
        },
        "frozen_collections" => Copy {
            into: "legacy_frozen_collections".into(),
            cols: ["key0", "key1", "key", "release_id"].map(String::from).to_vec(),
            exprs: vec![lit(m.key0), lit(m.key1), "key".into(), "release_id".into()],
            key: Some("key".into()),
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
                    ("files", "domain_base") => continue,
                    // 0 is a message-id stored whole, no domain
                    ("segments", "domain") => format!("case when {q} = 0 then 0 else {q} + {} end", m.domain_offset),
                    (t, "file_id") if FILE_KEYED.contains(&t) => format!("{q} + {}", m.file_offset),
                    _ => q.clone(),
                };
                cols.push(q);
                exprs.push(expr);
            }
            if table == "files" {
                cols.push("domain_base".into());
                exprs.push(lit(m.domain_offset));
            }
            let key = if table == "files" || table == "releases" || table == "catalog_outbox" {
                Some(if table == "catalog_outbox" { "seq" } else { "id" }.to_string())
            } else if FILE_KEYED.contains(&table) {
                Some("file_id".into())
            } else if has("release_id") {
                Some("release_id".into())
            } else if has("id") {
                Some("id".into())
            } else {
                None
            };
            Copy { into: table.to_string(), cols, exprs, key }
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
    let insert = format!(
        "insert into main.{} ({}) select {} from {src}",
        quote(&plan.into),
        plan.cols.join(", "),
        plan.exprs.join(", ")
    );
    let (mut upto, mut copied) = progress.map_or((None, 0), |(u, c, _)| (u, c));
    match &plan.key {
        None => {
            if !spend(budget) {
                return Ok(None);
            }
            let tx = conn.unchecked_transaction()?;
            copied = tx.execute(&insert, [])? as i64;
            tx.execute(
                "insert or replace into merge_progress (shard, tbl, upto, copied, done) values (?1, ?2, null, ?3, 0)",
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
                let mut width: i128 = BATCH_ROWS as i128;
                let ranged = format!("{insert} where {k} >= ?1 and {k} <= ?2");
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
        }
    }
    let _ = upto;
    let count: i64 = conn.query_row(&format!("select count(*) from {src}"), [], |r| r.get(0))?;
    if count != copied {
        bail!("shard {shard}, {table}: {copied} rows copied but the source has {count}");
    }
    conn.execute("update merge_progress set done = 1 where shard = ?1 and tbl = ?2", params![shard, table])?;
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

fn add_column(conn: &Connection, table: &str, column: &str, kind: &str) -> Result<()> {
    let have: BTreeSet<String> = columns(conn, "main", table)?.into_iter().collect();
    if !have.contains(column) {
        conn.execute(&format!("alter table {table} add column {column} {kind}"), [])?;
    }
    Ok(())
}

/// A database file and everything beside it that belongs to it.
fn remove_family(base: &Path) -> Result<()> {
    for suffix in ["", "-wal", "-shm", "-journal", ".sealed-log-cursor", ".catalog-cursor"] {
        let p = PathBuf::from(format!("{}{suffix}", base.display()));
        match std::fs::remove_file(&p) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("removing {}", p.display())),
        }
    }
    Ok(())
}

/// After the last source: the id counter past every release id, as one shard numbers them (`seq` is the id).
pub fn finish(main: &Path, target: &Path) -> Result<()> {
    let conn = crate::db::open_at(target)?;
    let max: i64 = conn.query_row("select coalesce(max(id), 0) from releases", [], |r| r.get(0))?;
    crate::store::set_next_seq(&crate::db::open_at(main)?, max + 1)?;
    restore_triggers(&conn)?;
    crate::sidecar::recount(&conn)?;
    Ok(())
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
    let todo = sources(&main);
    if todo.is_empty() {
        println!("nothing to merge: {} is the only shard", target.display());
        return 0;
    }
    let free = crate::frozen::free_bytes(&main).unwrap_or(0);
    let biggest = todo.iter().filter_map(|s| std::fs::metadata(&s.path).ok()).map(|m| m.len()).max().unwrap_or(0);
    println!("merging {} shards into {}, smallest first:", todo.len(), target.display());
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
    let _lock = match crate::compact::Lock::take(&main) {
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

    /// a group name for each shard
    fn groups() -> Vec<String> {
        let mut by_shard: Vec<Option<String>> = vec![None; LEGACY_SHARDS];
        for i in 0.. {
            let g = format!("alt.binaries.g{i}");
            let s = store::shard_of(&g);
            if by_shard[s].is_none() {
                by_shard[s] = Some(g);
            }
            if by_shard.iter().all(Option::is_some) {
                break;
            }
        }
        by_shard.into_iter().map(Option::unwrap).collect()
    }

    fn count(conn: &Connection, sql: &str) -> i64 {
        conn.query_row(sql, [], |r| r.get(0)).unwrap()
    }

    /// Eight shards with two releases each, their articles under a domain of their own and one shared by all.
    fn fixture(main: &Path) -> Vec<String> {
        crate::db::create_db_at(main).unwrap();
        let groups = groups();
        for (s, g) in groups.iter().enumerate() {
            let releases: Vec<Release> = (0..2)
                .map(|r| Release {
                    name: format!("Rel.{s}.{r}"),
                    group: g.clone(),
                    collection: Some(format!("post {s} {r}")),
                    articles: vec![
                        art("a.rar", 1, &format!("s{s}r{r}a1"), &format!("own{s}.example")),
                        art("a.rar", 2, &format!("s{s}r{r}a2"), &format!("own{s}.example")),
                        art("a.rar", 3, &format!("s{s}r{r}a3"), &format!("own{s}.example")),
                        art("a.rar", 4, &format!("s{s}r{r}a4"), "rare.example"),
                    ],
                    ..Default::default()
                })
                .collect();
            store::save(main, &releases).unwrap();
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
        assert_eq!(count(&c, "select count(*) from legacy_domains"), LEGACY_SHARDS as i64 - 1);
        assert_eq!(
            count(
                &c,
                "select count(*) from segments s join files f on f.id = s.file_id
                 where coalesce(f.domain_base, 0) != 0 and s.domain != 0
                   and s.domain not in (select id from legacy_domains)"
            ),
            0
        );
        assert_eq!(count(&c, "select count(*) from segments where domain = 0"), before_whole);
        let suffix_of = |name: &str| -> String {
            c.query_row(
                "select coalesce((select suffix from domains where id = s.domain),
                                 (select suffix from legacy_domains where id = s.domain))
                 from segments s join files f on f.id = s.file_id join releases r on r.id = f.release_id
                 where r.name = ?1 and s.part = 3",
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
}
