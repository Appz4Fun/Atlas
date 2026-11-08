//! `atlas --export-keep [--apply]`: what re-indexing can't bring back, before the releases and articles are wiped
//! and indexed again from scratch. That's what came from an external service (PreDB feeds, the media providers'
//! lookups and details, OpenSubtitles) and what an operator decided by hand (releases thawed by hand), not anything
//! read from Usenet headers or article bodies.
//!
//! The tables go into one SQLite file (`keep.db`, zstd compressed beside it) under `keep-export/<time>/` and, with
//! `--apply`, into the shared Postgres's `keep` schema. Each copy is checked against its source by row count and an
//! order-free digest of every row, and the run fails on any difference. A release's hand thaw is kept by its name and
//! group, which survive the wipe, beside its old id, which doesn't.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use rusqlite::types::Value;
use rusqlite::{Connection, OpenFlags};
use sha2::{Digest, Sha256};

/// One table kept: the database beside `atlas.db` it's in, its name there, and its name in the copies.
struct Kept {
    db: fn(&Path) -> PathBuf,
    table: &'static str,
    name: &'static str,
    /// the rows kept, when not all of them
    filter: Option<&'static str>,
    /// a table its own cap prunes: fewer rows than the last run is no sign of trouble
    capped: bool,
}

fn kept() -> Vec<Kept> {
    let predb = crate::predb::db::path_for;
    let media = crate::media::store::path_for;
    let details = crate::media::details::path_for;
    let raw = crate::raw_store::path_for;
    vec![
        // PreDB titles and where each feed stands (`title_keys` is built from the titles)
        Kept { db: predb, table: "titles", name: "predb_titles", filter: None, capped: false },
        Kept { db: predb, table: "feeds", name: "predb_feeds", filter: None, capped: false },
        Kept { db: predb, table: "meta", name: "predb_meta", filter: None, capped: false },
        // the media providers' answers: ids per title, and TMDB's details
        Kept { db: media, table: "media_titles", name: "media_titles", filter: None, capped: false },
        Kept { db: details, table: "details", name: "media_details", filter: None, capped: false },
        Kept { db: details, table: "details_lookups", name: "media_details_lookups", filter: None, capped: false },
        // OpenSubtitles' answers, and the PreDB feeds' pages as fetched
        Kept { db: raw, table: "raw_opensubtitles", name: "opensubtitles", filter: None, capped: true },
        Kept { db: raw, table: "raw_predb_log", name: "predb_pages", filter: None, capped: true },
        Kept { db: raw, table: "raw_predb_events", name: "predb_page_events", filter: None, capped: true },
        // historical PreDB snapshots, downloaded once and pinned (the rest of the HTTP cache lives 12 hours)
        Kept {
            db: raw,
            table: "raw_http_cache",
            name: "predb_snapshots",
            filter: Some("source in ('nzedb_archive', 'dats_pciso_1998')"),
            capped: true,
        },
    ]
}

/// Releases thawed by hand, from every shard file there is (one shard, or the eight there were).
const FREEZE_EXEMPT: &str = "freeze_exempt";

fn shard_files(main: &Path) -> Vec<PathBuf> {
    (0..crate::merge_shards::LEGACY_SHARDS)
        .map(|s| crate::merge_shards::legacy_shard_path(main, s))
        .filter(|p| p.exists())
        .collect()
}

/// A column as SQLite declares it, and the Postgres type it's copied as.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Column {
    name: String,
    decl: String,
    pg: PgKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PgKind {
    Int,
    Real,
    Text,
    Blob,
}

impl PgKind {
    /// SQLite's affinity rules, as far as these tables need them
    fn of(decl: &str) -> PgKind {
        let d = decl.to_ascii_uppercase();
        if d.contains("INT") {
            PgKind::Int
        } else if d.contains("CHAR") || d.contains("CLOB") || d.contains("TEXT") {
            PgKind::Text
        } else if d.contains("BLOB") {
            PgKind::Blob
        } else if d.contains("REAL") || d.contains("FLOA") || d.contains("DOUB") {
            PgKind::Real
        } else {
            PgKind::Text
        }
    }

    fn sql(self) -> &'static str {
        match self {
            PgKind::Int => "bigint",
            PgKind::Real => "double precision",
            PgKind::Text => "text",
            PgKind::Blob => "bytea",
        }
    }
}

fn columns(conn: &Connection, schema: &str, table: &str) -> Result<Vec<Column>> {
    let mut stmt = conn.prepare("select name, type from pragma_table_info(?1, ?2) order by cid")?;
    let cols = stmt
        .query_map([table, schema], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(cols.into_iter().map(|(name, decl)| Column { pg: PgKind::of(&decl), name, decl }).collect())
}

/// A row's bytes for the digest: each value tagged by its kind, soo 1 and '1' differ.
fn row_bytes(row: &[Value], out: &mut Vec<u8>) {
    out.clear();
    for v in row {
        match v {
            Value::Null => out.push(0),
            Value::Integer(i) => {
                out.push(1);
                out.extend_from_slice(&i.to_le_bytes());
            }
            Value::Real(f) => {
                out.push(2);
                out.extend_from_slice(&f.to_bits().to_le_bytes());
            }
            Value::Text(t) => {
                out.push(3);
                out.extend_from_slice(&(t.len() as u64).to_le_bytes());
                out.extend_from_slice(t.as_bytes());
            }
            Value::Blob(b) => {
                out.push(4);
                out.extend_from_slice(&(b.len() as u64).to_le_bytes());
                out.extend_from_slice(b);
            }
        }
    }
}

/// Rows and an order-free digest of them: the wrapping sum of each row's SHA-256 (its first 16 bytes).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Tally {
    pub rows: u64,
    pub digest: u128,
}

impl Tally {
    fn add(&mut self, row: &[Value], buf: &mut Vec<u8>) {
        row_bytes(row, buf);
        let h = Sha256::digest(&buf[..]);
        self.rows += 1;
        self.digest = self.digest.wrapping_add(u128::from_le_bytes(h[..16].try_into().expect("16 bytes")));
    }

    fn hex(self) -> String {
        format!("{:032x}", self.digest)
    }
}

/// The tally of `sql`'s rows, each value checked to be of its column's kind (as Postgres will take it), soo a dry
/// run finds what `--apply` would refuse.
fn tally_sql(conn: &Connection, sql: &str, cols: &[Column]) -> Result<Tally> {
    let mut t = Tally::default();
    let mut buf = Vec::new();
    let mut stmt = conn.prepare(sql)?;
    let mut rows = stmt.query([])?;
    let mut row: Vec<Value> = Vec::with_capacity(cols.len());
    while let Some(r) = rows.next()? {
        row.clear();
        for (i, col) in cols.iter().enumerate() {
            let v = r.get::<_, Value>(i)?;
            let fits = matches!(
                (col.pg, &v),
                (_, Value::Null)
                    | (PgKind::Int, Value::Integer(_))
                    | (PgKind::Real, Value::Real(_))
                    | (PgKind::Text, Value::Text(_))
                    | (PgKind::Blob, Value::Blob(_))
            );
            if !fits {
                bail!("{}: a {v:?} in a {:?} column can't go to Postgres as it is", col.name, col.pg);
            }
            if let Value::Text(t) = &v
                && t.contains('\0')
            {
                bail!("{}: a text with a zero byte, which Postgres's text refuses", col.name);
            }
            row.push(v);
        }
        t.add(&row, &mut buf);
    }
    Ok(t)
}

fn quoted(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn select_list(cols: &[Column]) -> String {
    cols.iter().map(|c| quoted(&c.name)).collect::<Vec<_>>().join(", ")
}

/// The columns of `keep.freeze_exempt`: the shard's own, and the release's name and group, by which a release
/// indexed again is found (its id won't be the same).
fn freeze_exempt_columns() -> Vec<Column> {
    [("release_id", "INTEGER"), ("at", "INTEGER"), ("name", "TEXT"), ("group_name", "TEXT")]
        .into_iter()
        .map(|(n, d)| Column { name: n.into(), decl: d.into(), pg: PgKind::of(d) })
        .collect()
}

fn attach_ro(conn: &Connection, path: &Path, alias: &str) -> Result<()> {
    let uri = format!(
        "file:{}?mode=ro",
        path.display().to_string().replace('%', "%25").replace('?', "%3f").replace('#', "%23")
    );
    conn.execute(&format!("attach database ?1 as {alias}"), [uri])
        .with_context(|| format!("attaching {}", path.display()))?;
    Ok(())
}

/// One table of the copy: its columns and the tally of what went in.
#[derive(Debug)]
pub struct Copied {
    pub name: String,
    columns: Vec<Column>,
    pub tally: Tally,
    capped: bool,
}

/// `keep.db` at `out`, every kept table copied from beside `main` and checked against its source.
pub fn build(main: &Path, out: &Path, allow_missing: bool) -> Result<Vec<Copied>> {
    if out.exists() {
        bail!("{} is there already", out.display());
    }
    let conn = Connection::open(out)?;
    conn.execute_batch("pragma journal_mode = off; pragma synchronous = off;")?;
    let mut copied = Vec::new();
    for k in kept() {
        let src = (k.db)(main);
        // a database or table that isn't there is most likely the wrong directory: refused, unless told
        if !src.try_exists()? {
            if !allow_missing {
                bail!("{}: no {} (--allow-missing to go on without it)", k.name, src.display());
            }
            println!("  {}: no {} (left out)", k.name, src.display());
            continue;
        }
        attach_ro(&conn, &src, "src")?;
        let cols = columns(&conn, "src", k.table)?;
        if cols.is_empty() {
            conn.execute("detach database src", [])?;
            if !allow_missing {
                bail!("{}: no {} table in {} (--allow-missing to go on without it)", k.name, k.table, src.display());
            }
            println!("  {}: no {} table in {} (left out)", k.name, k.table, src.display());
            continue;
        }
        let filter = k.filter.map_or(String::new(), |f| format!(" where {f}"));
        let defs = cols.iter().map(|c| format!("{} {}", quoted(&c.name), c.decl)).collect::<Vec<_>>().join(", ");
        let list = select_list(&cols);
        conn.execute_batch(&format!(
            "create table main.{name} ({defs});
             insert into main.{name} ({list}) select {list} from src.{table}{filter};",
            name = quoted(k.name),
            table = quoted(k.table)
        ))?;
        let theirs = tally_sql(&conn, &format!("select {list} from src.{}{filter}", quoted(k.table)), &cols)?;
        let ours = tally_sql(&conn, &format!("select {list} from main.{}", quoted(k.name)), &cols)?;
        conn.execute("detach database src", [])?;
        if theirs != ours {
            bail!("{}: the copy differs from {} ({theirs:?} vs {ours:?})", k.name, src.display());
        }
        copied.push(Copied { name: k.name.into(), columns: cols, tally: ours, capped: k.capped });
    }
    // hand thaws, from every shard, by name and group
    let cols = freeze_exempt_columns();
    let defs = cols.iter().map(|c| format!("{} {}", quoted(&c.name), c.decl)).collect::<Vec<_>>().join(", ");
    conn.execute_batch(&format!("create table main.{FREEZE_EXEMPT} ({defs});"))?;
    let mut theirs = Tally::default();
    for shard in shard_files(main) {
        attach_ro(&conn, &shard, "src")?;
        let has: bool = conn
            .prepare("select 1 from src.sqlite_master where type = 'table' and name = ?1")?
            .exists([FREEZE_EXEMPT])?;
        if has {
            let select = format!(
                "select f.release_id, f.at, r.name, r.group_name from src.{FREEZE_EXEMPT} f
                 left join src.releases r on r.id = f.release_id"
            );
            conn.execute(&format!("insert into main.{FREEZE_EXEMPT} {select}"), [])?;
            let t = tally_sql(&conn, &select, &cols)?;
            theirs.rows += t.rows;
            theirs.digest = theirs.digest.wrapping_add(t.digest);
        }
        conn.execute("detach database src", [])?;
    }
    let ours = tally_sql(&conn, &format!("select release_id, at, name, group_name from main.{FREEZE_EXEMPT}"), &cols)?;
    if theirs != ours {
        bail!("{FREEZE_EXEMPT}: the copy differs from the shards ({theirs:?} vs {ours:?})");
    }
    copied.push(Copied { name: FREEZE_EXEMPT.into(), columns: cols, tally: ours, capped: false });
    drop(conn);
    std::fs::File::open(out)?.sync_all()?;
    Ok(copied)
}

/// Every table of `keep.db` (at `file`) into the Postgres `keep` schema in one transaction: each replaced whole,
/// read back and checked against what went in, and the run noted in `keep.runs` last, soo Postgres holds one whole
/// run or the one before. A table that would shrink (a run after a wipe, or against the wrong directory) is refused
/// unless `force`.
pub fn to_postgres(pg: &mut postgres::Client, file: &Path, copied: &[Copied], stamp: &str, force: bool) -> Result<()> {
    let conn = Connection::open_with_flags(file, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut tx = pg.transaction()?;
    tx.batch_execute(
        "create schema if not exists keep;
         create table if not exists keep.runs (stamp text not null, name text not null, rows bigint not null,
             digest text not null, primary key (stamp, name));",
    )?;
    for c in copied {
        let table = format!("keep.{}", quoted(&c.name));
        let there: Option<i64> = tx
            .query_opt(
                "select coalesce((select rows from keep.runs where name = $1 order by stamp desc limit 1), 0)",
                &[&c.name],
            )?
            .map(|r| r.get(0));
        if let Some(before) = there
            && before > c.tally.rows as i64
            && !c.capped
            && !force
        {
            bail!("{table} held {before} rows and this run has {}: refused (--force to replace it)", c.tally.rows);
        }
        let defs = c.columns.iter().map(|col| format!("{} {}", quoted(&col.name), col.pg.sql())).collect::<Vec<_>>();
        tx.batch_execute(&format!("drop table if exists {table}; create table {table} ({});", defs.join(", ")))?;
        let types: Vec<postgres::types::Type> = c
            .columns
            .iter()
            .map(|col| match col.pg {
                PgKind::Int => postgres::types::Type::INT8,
                PgKind::Real => postgres::types::Type::FLOAT8,
                PgKind::Text => postgres::types::Type::TEXT,
                PgKind::Blob => postgres::types::Type::BYTEA,
            })
            .collect();
        {
            let sink = tx.copy_in(&format!("copy {table} ({}) from stdin binary", select_list(&c.columns)))?;
            let mut writer = postgres::binary_copy::BinaryCopyInWriter::new(sink, &types);
            let mut stmt = conn.prepare(&format!("select {} from {}", select_list(&c.columns), quoted(&c.name)))?;
            let mut rows = stmt.query([])?;
            while let Some(r) = rows.next()? {
                let mut vals: Vec<Box<dyn postgres::types::ToSql + Sync>> = Vec::with_capacity(c.columns.len());
                for (i, col) in c.columns.iter().enumerate() {
                    let v: Value = r.get(i)?;
                    vals.push(match (col.pg, v) {
                        (_, Value::Null) => match col.pg {
                            PgKind::Int => Box::new(None::<i64>),
                            PgKind::Real => Box::new(None::<f64>),
                            PgKind::Text => Box::new(None::<String>),
                            PgKind::Blob => Box::new(None::<Vec<u8>>),
                        },
                        (PgKind::Int, Value::Integer(i)) => Box::new(i),
                        (PgKind::Real, Value::Real(f)) => Box::new(f),
                        (PgKind::Text, Value::Text(t)) => Box::new(t),
                        (PgKind::Blob, Value::Blob(b)) => Box::new(b),
                        (kind, v) => {
                            bail!("{}.{}: a {v:?} in a {kind:?} column can't go over as it is", c.name, col.name)
                        }
                    });
                }
                let refs: Vec<&(dyn postgres::types::ToSql + Sync)> = vals.iter().map(|b| b.as_ref()).collect();
                writer.write(&refs)?;
            }
            writer.finish()?;
        }
        let back = pg_tally(&mut tx, &table, &c.columns)?;
        if back != c.tally {
            bail!("{table}: Postgres holds something else ({back:?} vs {:?})", c.tally);
        }
        tx.execute(
            "insert into keep.runs (stamp, name, rows, digest) values ($1, $2, $3, $4)",
            &[&stamp, &c.name, &(back.rows as i64), &back.hex()],
        )?;
        println!("  {table}: {} rows, digest {}", back.rows, back.hex());
    }
    tx.commit()?;
    Ok(())
}

/// The tally of a Postgres table, its rows streamed.
fn pg_tally(tx: &mut postgres::Transaction<'_>, table: &str, cols: &[Column]) -> Result<Tally> {
    use postgres::fallible_iterator::FallibleIterator;
    let mut t = Tally::default();
    let mut buf = Vec::new();
    let mut row: Vec<Value> = Vec::with_capacity(cols.len());
    let mut rows = tx.query_raw(&format!("select {} from {table}", select_list(cols)), std::iter::empty::<i32>())?;
    while let Some(r) = rows.next()? {
        row.clear();
        for (i, col) in cols.iter().enumerate() {
            row.push(match col.pg {
                PgKind::Int => r.get::<_, Option<i64>>(i).map_or(Value::Null, Value::Integer),
                PgKind::Real => r.get::<_, Option<f64>>(i).map_or(Value::Null, Value::Real),
                PgKind::Text => r.get::<_, Option<String>>(i).map_or(Value::Null, Value::Text),
                PgKind::Blob => r.get::<_, Option<Vec<u8>>>(i).map_or(Value::Null, Value::Blob),
            });
        }
        t.add(&row, &mut buf);
    }
    Ok(t)
}

/// `path` and the directory holding it flushed to disk.
fn sync(path: &Path) -> Result<()> {
    std::fs::File::open(path)?.sync_all()?;
    if let Some(dir) = path.parent() {
        std::fs::File::open(dir)?.sync_all()?;
    }
    Ok(())
}

/// `keep.db` zstd compressed beside it, read back and checked byte for byte.
fn compress(file: &Path) -> Result<PathBuf> {
    let out = PathBuf::from(format!("{}.zst", file.display()));
    {
        let mut src = std::fs::File::open(file)?;
        let dst = std::fs::File::create(&out)?;
        let mut enc = zstd::stream::Encoder::new(dst, 19)?;
        std::io::copy(&mut src, &mut enc)?;
        enc.finish()?.sync_all()?;
    }
    let mut back = Sha256::new();
    std::io::copy(&mut zstd::stream::Decoder::new(std::fs::File::open(&out)?)?, &mut back)?;
    let mut orig = Sha256::new();
    std::io::copy(&mut std::fs::File::open(file)?, &mut orig)?;
    if back.finalize() != orig.finalize() {
        bail!("{} doesn't read back as {}", out.display(), file.display());
    }
    Ok(out)
}

/// `atlas --export-keep [--apply] [--allow-missing] [--force]`.
pub fn run(args: &[String]) -> i32 {
    let has = |f: &str| args.iter().any(|a| a == f);
    let (apply, allow_missing, force) = (has("--apply"), has("--allow-missing"), has("--force"));
    if apply && allow_missing {
        // Postgres would hold the left-out tables from an earlier run beside this one's
        println!("--allow-missing is for a dry run only: put the missing database back before --apply");
        return 1;
    }
    if crate::procs::indexer_alive() {
        println!("stop indexing first: what it writes meanwhile could be missed");
        return 1;
    }
    let main = crate::paths::database();
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    let root = main.with_file_name("keep-export");
    // built under a .partial name and renamed once checked: a directory without it is a whole copy
    let partial = root.join(format!("{stamp}.partial"));
    let dir = root.join(&stamp);
    if dir.exists() || partial.exists() {
        println!("{} is there already: run again in a second", dir.display());
        return 1;
    }
    if let Err(e) = std::fs::create_dir_all(&partial) {
        println!("couldnt make {}: {e}", partial.display());
        return 1;
    }
    let file = partial.join("keep.db");
    println!("keeping what re-indexing can't bring back:");
    let copied = match build(&main, &file, allow_missing) {
        Ok(c) => c,
        Err(e) => {
            println!("couldnt copy: {e:#}");
            return 1;
        }
    };
    let mut manifest = serde_json::Map::new();
    for c in &copied {
        println!("  {}: {} rows, digest {}", c.name, c.tally.rows, c.tally.hex());
        manifest.insert(c.name.clone(), serde_json::json!({"rows": c.tally.rows, "digest": c.tally.hex()}));
    }
    let left_out: Vec<&str> = kept().iter().map(|k| k.name).filter(|n| !copied.iter().any(|c| c.name == *n)).collect();
    manifest.insert("_left_out".into(), serde_json::json!(left_out));
    if let Err(e) = compress(&file) {
        println!("couldnt compress: {e:#}");
        return 1;
    }
    let written = std::fs::write(partial.join("manifest.json"), serde_json::Value::Object(manifest).to_string())
        .map_err(anyhow::Error::from)
        .and_then(|()| sync(&partial.join("manifest.json")))
        .and_then(|()| Ok(std::fs::rename(&partial, &dir)?))
        .and_then(|()| sync(&dir.join("manifest.json")))
        // the rename's entry is in keep-export/, and keep-export/'s own in the folder above
        .and_then(|()| sync(&dir))
        .and_then(|()| sync(&root));
    if let Err(e) = written {
        println!("couldnt finish {}: {e:#}", dir.display());
        return 1;
    }
    println!("checked copy: {} (and keep.db.zst, manifest.json)", dir.join("keep.db").display());
    println!("the poster, cover and still images stay as files beside the database");
    if !apply {
        println!("dry run: pass --apply to put these tables in the shared Postgres's keep schema too");
        return 0;
    }
    let config = crate::config::shared_catalog_config();
    if config.url.is_empty() {
        println!("no shared_catalog url in the config: nowhere to put them");
        return 1;
    }
    let mut pg = match crate::shared_catalog::connect(&config) {
        Ok(pg) => pg,
        Err(e) => {
            println!("couldnt reach Postgres: {e:#}");
            return 1;
        }
    };
    println!("into Postgres, in one transaction, each table read back and checked:");
    match to_postgres(&mut pg, &dir.join("keep.db"), &copied, &stamp, force) {
        Ok(()) => {
            println!("kept: every table checks out in {} and in Postgres (keep.runs {stamp})", dir.display());
            0
        }
        Err(e) => {
            println!("couldnt put them in Postgres (nothing there changed): {e:#}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_digest_is_the_same_in_any_order_and_tells_kinds_apart() {
        let mut buf = Vec::new();
        let rows = [
            vec![Value::Integer(1), Value::Text("a".into())],
            vec![Value::Integer(2), Value::Blob(vec![1, 2])],
            vec![Value::Null, Value::Real(1.5)],
        ];
        let (mut fwd, mut back) = (Tally::default(), Tally::default());
        rows.iter().for_each(|r| fwd.add(r, &mut buf));
        rows.iter().rev().for_each(|r| back.add(r, &mut buf));
        assert_eq!(fwd, back);
        let (mut one, mut text) = (Tally::default(), Tally::default());
        one.add(&[Value::Integer(1)], &mut buf);
        text.add(&[Value::Text("1".into())], &mut buf);
        assert_ne!(one.digest, text.digest);
    }

    #[test]
    fn the_kept_tables_and_hand_thaws_copy_whole() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        crate::db::create_db_at(&main).unwrap();
        let predb = Connection::open(crate::predb::db::path_for(&main)).unwrap();
        predb
            .execute_batch(
                "create table titles (id INTEGER PRIMARY KEY, title TEXT NOT NULL, size_bytes INTEGER, extra BLOB);
                 insert into titles values (1, 'A.Release-GRP', 100, x'0102'), (2, 'B.Release-GRP', null, null);
                 create table feeds (source INTEGER PRIMARY KEY, key TEXT NOT NULL);
                 insert into feeds values (7, 'feed');",
            )
            .unwrap();
        let shard = crate::store::shard_path(&main, 0);
        let s = crate::db::open_at(&shard).unwrap();
        s.execute("insert into releases (id, name, group_name) values (40, 'Kept.Out', 'alt.binaries.x')", []).unwrap();
        s.execute_batch(
            "create table if not exists freeze_exempt (release_id INTEGER PRIMARY KEY, at INTEGER NOT NULL);",
        )
        .unwrap();
        s.execute("insert into freeze_exempt (release_id, at) values (40, 5)", []).unwrap();
        let out = dir.path().join("keep.db");
        let copied = build(&main, &out, true).unwrap();
        let rows = |name: &str| copied.iter().find(|c| c.name == name).map(|c| c.tally.rows);
        assert_eq!(rows("predb_titles"), Some(2));
        assert_eq!(rows("predb_feeds"), Some(1));
        assert_eq!(rows("predb_meta"), None, "no table, nothing kept");
        assert_eq!(rows(FREEZE_EXEMPT), Some(1));
        let k = Connection::open(&out).unwrap();
        let (name, group): (String, String) = k
            .query_row("select name, group_name from freeze_exempt where release_id = 40", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!((name.as_str(), group.as_str()), ("Kept.Out", "alt.binaries.x"));
        let blob: Vec<u8> = k.query_row("select extra from predb_titles where id = 1", [], |r| r.get(0)).unwrap();
        assert_eq!(blob, [1, 2]);
        let z = compress(&out).unwrap();
        assert!(z.exists());
        assert!(build(&main, &out, true).is_err(), "never over an earlier copy");
        assert!(build(&main, &dir.path().join("strict.db"), false).is_err(), "a missing table stops it unless allowed");
    }

    #[test]
    fn declared_types_map_to_postgres_ones() {
        assert_eq!(PgKind::of("INTEGER"), PgKind::Int);
        assert_eq!(PgKind::of("TEXT"), PgKind::Text);
        assert_eq!(PgKind::of("BLOB"), PgKind::Blob);
        assert_eq!(PgKind::of("REAL"), PgKind::Real);
        assert_eq!(PgKind::of(""), PgKind::Text);
    }

    /// Against a real Postgres when `ATLAS_CATALOG_TEST_URL` names a database to use: every table goes over, reads
    /// back with the same digest, and a second run replaces it whole.
    #[test]
    fn the_kept_tables_go_into_postgres_and_read_back_the_same() {
        let Ok(url) = std::env::var("ATLAS_CATALOG_TEST_URL") else { return };
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        crate::db::create_db_at(&main).unwrap();
        Connection::open(crate::predb::db::path_for(&main))
            .unwrap()
            .execute_batch(
                "create table titles (id INTEGER PRIMARY KEY, title TEXT NOT NULL, score REAL, extra BLOB);
                 insert into titles values (1, 'A', 1.25, x'00ff'), (2, 'B', null, null), (3, 'C\u{00e9}', -2.5, x'');",
            )
            .unwrap();
        let out = dir.path().join("keep.db");
        let copied = build(&main, &out, true).unwrap();
        let mut pg = postgres::Client::connect(&url, postgres::NoTls).unwrap();
        pg.batch_execute("drop schema if exists keep cascade").unwrap();
        to_postgres(&mut pg, &out, &copied, "1", false).unwrap();
        to_postgres(&mut pg, &out, &copied, "2", false).unwrap();
        // fewer rows than the last run: refused, and nothing changed
        let fewer: Vec<Copied> = copied
            .iter()
            .map(|c| Copied {
                name: c.name.clone(),
                columns: c.columns.clone(),
                tally: Tally { rows: 0, digest: 0 },
                capped: false,
            })
            .collect();
        let empty = dir.path().join("empty.db");
        let e = Connection::open(&empty).unwrap();
        for c in &fewer {
            let defs = c.columns.iter().map(|col| format!("{} {}", quoted(&col.name), col.decl)).collect::<Vec<_>>();
            e.execute_batch(&format!("create table {} ({});", quoted(&c.name), defs.join(", "))).unwrap();
        }
        assert!(to_postgres(&mut pg, &empty, &fewer, "3", false).is_err());
        let runs: i64 = pg.query_one("select count(distinct stamp) from keep.runs", &[]).unwrap().get(0);
        assert_eq!(runs, 2, "the refused run left nothing");
        let n: i64 = pg.query_one("select count(*) from keep.predb_titles", &[]).unwrap().get(0);
        assert_eq!(n, 3);
        pg.batch_execute("drop schema keep cascade").unwrap();
    }
}
