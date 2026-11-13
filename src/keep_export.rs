//! `atlas --export-keep [--apply]`: what re-indexing can't bring back, before the releases and articles are wiped
//! and indexed again from scratch. That's what came from an external service (PreDB feeds, the media providers'
//! lookups and details, OpenSubtitles), costly natural-key Usenet probe caches,
//! and what an operator decided by hand (releases thawed by hand).
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
        // Preserve every stored response, including negative/API answers. The
        // legacy export table name stays compatible with older keep schemas.
        Kept {
            db: raw,
            table: "raw_http_cache",
            name: "predb_snapshots",
            filter: None,
            capped: true,
        },
        Kept { db: raw, table: "raw_probes", name: "raw_probes", filter: None, capped: true },
        Kept { db: raw, table: "raw_covers", name: "raw_covers", filter: None, capped: true },
        Kept { db: raw, table: "raw_archives", name: "raw_archives", filter: None, capped: true },
        Kept { db: raw, table: "raw_archive_inner_files", name: "raw_archive_inner_files", filter: None, capped: true },
        Kept { db: raw, table: "raw_par2", name: "raw_par2", filter: None, capped: true },
        Kept { db: raw, table: "raw_par2_files", name: "raw_par2_files", filter: None, capped: true },
        Kept { db: raw, table: "raw_rev_volumes", name: "raw_rev_volumes", filter: None, capped: true },
        Kept { db: raw, table: "raw_nfo_passwords", name: "raw_nfo_passwords", filter: None, capped: true },
        Kept { db: raw, table: "raw_stills", name: "raw_stills", filter: None, capped: true },
        Kept { db: raw, table: "raw_audio_prints", name: "raw_audio_prints", filter: None, capped: true },
    ]
}

/// Releases thawed by hand, from every shard file there is.
pub(crate) const FREEZE_EXEMPT: &str = "freeze_exempt";

pub(crate) fn shard_files(main: &Path) -> Vec<PathBuf> {
    crate::store::shard_paths(main).into_iter().filter(|p| p.exists()).collect()
}

/// A column as SQLite declares it, and the Postgres type it's copied as.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Column {
    pub(crate) name: String,
    pub(crate) decl: String,
    pub(crate) pg: PgKind,
    text_encoding: TextEncoding,
}

/// Source admission is derived from the real relation/column metadata, never
/// from query text. Only this codec changes PostgreSQL's NUL-text restriction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TextEncoding {
    Plain,
    NfoNameV1,
}

impl Column {
    pub(crate) fn plain(name: String, decl: String, pg: PgKind) -> Self {
        Self { name, decl, pg, text_encoding: TextEncoding::Plain }
    }

    fn encoded_nfo_name(&self) -> bool {
        self.pg == PgKind::Text && self.name == "name" && self.text_encoding == TextEncoding::NfoNameV1
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PgKind {
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

pub(crate) fn columns(conn: &Connection, schema: &str, table: &str) -> Result<Vec<Column>> {
    let mut stmt = conn.prepare("select name, type from pragma_table_info(?1, ?2) order by cid")?;
    let cols = stmt
        .query_map([table, schema], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(cols.into_iter().map(|(name, decl)| {
        let mut column = Column::plain(name, decl.clone(), PgKind::of(&decl));
        if table == "raw_nfo_passwords" && column.pg == PgKind::Text && column.name == "name" {
            column.text_encoding = TextEncoding::NfoNameV1;
        }
        column
    }).collect())
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

    pub(crate) fn hex(self) -> String {
        format!("{:032x}", self.digest)
    }
}

/// The tally of `sql`'s rows, each value checked to be of its column's kind (as Postgres will take it), soo a dry
/// run finds what `--apply` would refuse.
pub(crate) fn tally_sql(conn: &Connection, sql: &str, cols: &[Column]) -> Result<Tally> {
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
                && !col.encoded_nfo_name()
            {
                bail!("{}: a text with a zero byte, which Postgres's text refuses", col.name);
            }
            row.push(v);
        }
        t.add(&row, &mut buf);
    }
    Ok(t)
}

pub(crate) fn quoted(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

pub(crate) fn select_list(cols: &[Column]) -> String {
    cols.iter().map(|c| quoted(&c.name)).collect::<Vec<_>>().join(", ")
}

/// The columns of `keep.freeze_exempt`: the shard's own, and the release's name and group, by which a release
/// indexed again is found (its id won't be the same).
fn freeze_exempt_columns() -> Vec<Column> {
    [("release_id", "INTEGER"), ("at", "INTEGER"), ("name", "TEXT"), ("group_name", "TEXT")]
        .into_iter()
        .map(|(n, d)| Column::plain(n.into(), d.into(), PgKind::of(d)))
        .collect()
}

pub(crate) fn attach_ro(conn: &Connection, path: &Path, alias: &str) -> Result<()> {
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
    to_postgres_in_schema(pg, file, copied, stamp, force, "keep")
}

fn to_postgres_in_schema(
    pg: &mut postgres::Client,
    file: &Path,
    copied: &[Copied],
    stamp: &str,
    force: bool,
    schema: &str,
) -> Result<()> {
    let namespace = quoted(schema);
    let conn = Connection::open_with_flags(file, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut tx = pg.transaction()?;
    tx.batch_execute("set local synchronous_commit = on")?;
    tx.batch_execute(&format!(
        "create schema if not exists {namespace};
         create table if not exists {namespace}.runs (stamp text not null, name text not null, rows bigint not null,
             digest text not null, primary key (stamp, name));"
    ))?;
    for c in copied {
        let table = format!("{namespace}.{}", quoted(&c.name));
        let there: Option<i64> = tx
            .query_opt(
                &format!("select coalesce((select rows from {namespace}.runs where name = $1 order by stamp desc limit 1), 0)"),
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
        tx.batch_execute(&format!("drop table if exists {table}"))?;
        let back = copy_checked(&conn, &mut tx, &table, c)?;
        tx.execute(
            &format!("insert into {namespace}.runs (stamp, name, rows, digest) values ($1, $2, $3, $4)"),
            &[&stamp, &c.name, &(back.rows as i64), &back.hex()],
        )?;
        println!("  {table}: {} rows, digest {}", back.rows, back.hex());
    }
    mark_nfo_codec(&mut tx,schema,copied)?;
    tx.commit()?;
    Ok(())
}

/// Copy one table into a transaction and compare all of its rows before the caller commits.
fn copy_checked(conn: &Connection, tx: &mut postgres::Transaction<'_>, table: &str, c: &Copied) -> Result<Tally> {
    let defs = c.columns.iter().map(|col| format!("{} {}", quoted(&col.name), col.pg.sql())).collect::<Vec<_>>();
    tx.batch_execute(&format!("create table {table} ({});", defs.join(", ")))?;
    copy_rows(
        tx,
        table,
        &c.columns,
        conn,
        &format!("select {} from {}", select_list(&c.columns), quoted(&c.name)),
        &c.name,
    )?;
    let back = pg_tally(tx, table, &c.columns)?;
    if back != c.tally {
        bail!("{table}: Postgres holds something else ({back:?} vs {:?})", c.tally);
    }
    Ok(back)
}

/// The rows `select` reads from `conn` (its columns `cols`, in order) into the Postgres `table` by binary COPY, each
/// value checked to be of its column's kind. `what` names the source in errors.
pub(crate) fn copy_rows(
    tx: &mut postgres::Transaction<'_>,
    table: &str,
    cols: &[Column],
    conn: &Connection,
    select: &str,
    what: &str,
) -> Result<()> {
    let types: Vec<postgres::types::Type> = cols
        .iter()
        .map(|col| match col.pg {
            PgKind::Int => postgres::types::Type::INT8,
            PgKind::Real => postgres::types::Type::FLOAT8,
            PgKind::Text => postgres::types::Type::TEXT,
            PgKind::Blob => postgres::types::Type::BYTEA,
        })
        .collect();
    let sink = tx.copy_in(&format!("copy {table} ({}) from stdin binary", select_list(cols)))?;
    let mut writer = postgres::binary_copy::BinaryCopyInWriter::new(sink, &types);
    // Every SQLite-origin NFO name is mapped, including ordinary names and
    // literal strings beginning with the prefix. Never detect encoding from
    // input contents: that would alias a valid legacy name to a nested key.
    let mapped=nfo_table(table) && cols.iter().any(|column|column.name=="name");
    let select=if mapped {
        let projection=cols.iter().map(|column|{
            if column.name=="name" {format!("case when q.\"name\" is null then null else '{}' || lower(hex(cast(q.\"name\" as blob))) end as \"name\"",crate::raw_store::nfo_key::PREFIX)}
            else {format!("q.{}",quoted(&column.name))}
        }).collect::<Vec<_>>().join(",");
        format!("select {projection} from ({select}) q")
    } else {select.to_owned()};
    let mut stmt = conn.prepare(&select)?;
    let mut rows = stmt.query([])?;
    while let Some(r) = rows.next()? {
        let mut vals: Vec<Box<dyn postgres::types::ToSql + Sync>> = Vec::with_capacity(cols.len());
        for (i, col) in cols.iter().enumerate() {
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
                (kind, v) => bail!("{what}.{}: a {v:?} in a {kind:?} column can't go over as it is", col.name),
            });
        }
        let refs: Vec<&(dyn postgres::types::ToSql + Sync)> = vals.iter().map(|b| b.as_ref()).collect();
        writer.write(&refs)?;
    }
    writer.finish()?;
    Ok(())
}

/// The tally of a Postgres table, its rows streamed.
pub(crate) fn pg_tally(tx: &mut postgres::Transaction<'_>, table: &str, cols: &[Column]) -> Result<Tally> {
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
                PgKind::Text => match r.get::<_, Option<String>>(i) {
                    Some(text) if nfo_table(table) && col.name=="name" => Value::Text(crate::raw_store::nfo_key::decode(&text)?),
                    text=>text.map_or(Value::Null,Value::Text),
                },
                PgKind::Blob => r.get::<_, Option<Vec<u8>>>(i).map_or(Value::Null, Value::Blob),
            });
        }
        t.add(&row, &mut buf);
    }
    Ok(t)
}

fn nfo_table(table:&str)->bool {
    matches!(table.rsplit('.').next().unwrap_or(table).trim_matches('"'),"raw_nfo_passwords"|"ext_raw_nfo_passwords")
}

fn mark_nfo_codec(tx:&mut postgres::Transaction<'_>,schema:&str,copied:&[Copied])->Result<()> {
    if !copied.iter().any(|table|table.name=="raw_nfo_passwords" && table.columns.iter().any(|column|column.name=="name")) {return Ok(())}
    let namespace=quoted(schema);
    tx.batch_execute(&format!("create table if not exists {namespace}._atlas_cache_key_formats
      (table_name text not null,column_name text not null,codec text not null,primary key(table_name,column_name))"))?;
    let updated=tx.execute(&format!("insert into {namespace}._atlas_cache_key_formats as existing values('raw_nfo_passwords','name',$1)
      on conflict(table_name,column_name) do update set codec=excluded.codec where existing.codec=excluded.codec"),&[&crate::raw_store::nfo_key::PREFIX])?;
    anyhow::ensure!(updated==1,"keep NFO codec metadata conflict");Ok(())
}

pub(crate) fn keep_nfo_encoded(tx:&mut postgres::Transaction<'_>,schema:&str)->Result<bool> {
    let has:bool=tx.query_one("select exists(select 1 from information_schema.tables where table_schema=$1 and table_name='_atlas_cache_key_formats')",&[&schema])?.get(0);
    if !has {return Ok(false)}
    let row=tx.query_opt(&format!("select codec from {}._atlas_cache_key_formats where table_name='raw_nfo_passwords' and column_name='name'",quoted(schema)),&[])?;
    let Some(row)=row else {return Ok(false)};
    anyhow::ensure!(row.get::<_,String>(0)==crate::raw_store::nfo_key::PREFIX,"unsupported keep NFO codec");Ok(true)
}

fn verify_snapshot(dir: &Path, conn: &Connection) -> Result<Vec<Copied>> {
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join("manifest.json")).context("reading manifest.json")?)?;
    let manifest = manifest.as_object().context("snapshot manifest must be an object")?;
    // Old checked keep snapshots remain restorable under their original
    // narrower scope. A new-format snapshot must include every new raw family;
    // missing new tables cannot masquerade as a complete current snapshot.
    let version=manifest.get("_format").map(serde_json::Value::as_u64);
    anyhow::ensure!(matches!(version,None|Some(Some(2))),"unknown keep snapshot format");
    let tables:Vec<Kept>=kept().into_iter().filter(|table|version.is_some() || !table.name.starts_with("raw_")).collect();
    let extra=if version.is_some() {3} else {2};
    anyhow::ensure!(
        manifest.get("_left_out").and_then(serde_json::Value::as_array).is_some_and(Vec::is_empty)
            && manifest.len() == tables.len() + extra,
        "snapshot is incomplete or has an unexpected table"
    );
    let mut copied = Vec::with_capacity(tables.len() + 1);
    for name in tables.into_iter().map(|k| k.name).chain([FREEZE_EXEMPT]) {
        let entry = manifest.get(name).with_context(|| format!("{name}: missing from manifest"))?;
        let columns = columns(conn, "main", name)?;
        anyhow::ensure!(!columns.is_empty(), "{name}: table missing from keep.db");
        let tally = tally_sql(conn, &format!("select {} from {}", select_list(&columns), quoted(name)), &columns)?;
        anyhow::ensure!(
            entry["rows"].as_u64() == Some(tally.rows) && entry["digest"].as_str() == Some(tally.hex().as_str()),
            "{name}: row count or digest does not match the manifest"
        );
        copied.push(Copied { name: name.into(), columns, tally, capped: false });
    }
    Ok(copied)
}

fn restore_postgres(
    pg: &mut postgres::Client,
    snapshot: &Connection,
    copied: &[Copied],
    database: &str,
    schema: &str,
) -> Result<()> {
    anyhow::ensure!(
        crate::pg::valid_schema(schema) && schema.starts_with("keep_restore_"),
        "restore schema must start with keep_restore_ and be a plain lowercase name"
    );
    let mut tx = pg.transaction()?;
    let actual: String = tx.query_one("select current_database()", &[])?.get(0);
    anyhow::ensure!(actual == database, "connected to {actual}, expected {database}");
    restore_postgres_in_transaction(&mut tx, snapshot, copied, schema)?;
    tx.commit()?;
    Ok(())
}

/// The restore's mutation phase, shared with the durability regression so its transaction-local setting can be
/// observed before commit. The wrapper owns the transaction and checks destination identity first.
fn restore_postgres_in_transaction(
    tx: &mut postgres::Transaction<'_>,
    snapshot: &Connection,
    copied: &[Copied],
    schema: &str,
) -> Result<()> {
    tx.batch_execute("set local synchronous_commit = on")?;
    tx.batch_execute(&format!("create schema {}", quoted(schema)))?;
    for c in copied {
        let table = format!("{}.{}", quoted(schema), quoted(&c.name));
        copy_checked(snapshot, tx, &table, c).with_context(|| format!("restoring {table}"))?;
    }
    mark_nfo_codec(tx,schema,copied)?;
    Ok(())
}

pub fn run_restore(args: &[String]) -> i32 {
    let result = (|| -> Result<()> {
        let value = |flag: &str| {
            args.iter().skip_while(|a| *a != flag).nth(1).filter(|a| !a.starts_with("--")).map(String::as_str)
        };
        let dir = Path::new(value("--snapshot").context("--snapshot DIR is required")?);
        let mut snapshot = Connection::open_with_flags(dir.join("keep.db"), OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let snapshot = snapshot.transaction()?;
        let copied = verify_snapshot(dir, &snapshot)?;
        for c in &copied {
            println!("  {}: {} rows, digest {}", c.name, c.tally.rows, c.tally.hex());
        }
        if args.iter().any(|arg| arg == "--apply") {
            let database = value("--database").context("--database NAME is required with --apply")?;
            let schema = value("--schema").context("--schema keep_restore_NAME is required with --apply")?;
            let mut config = crate::config::storage_config();
            if let Ok(url) = std::env::var("ATLAS_RESTORE_PG_URL") {
                config.url = url;
            }
            anyhow::ensure!(!config.url.is_empty(), "set ATLAS_RESTORE_PG_URL or [storage] url for the destination");
            config.schema = schema.to_owned();
            let pool = crate::pg::Pool::new(&config)?;
            let mut pg = pool.get()?;
            restore_postgres(&mut pg, &snapshot, &copied, database, schema)?;
            println!("Restored and verified {} tables in {database}.{schema}", copied.len());
        } else {
            println!("Verified {} snapshot tables against the manifest", copied.len());
        }
        Ok(())
    })();
    if let Err(e) = result {
        eprintln!("keep restore: {e:#}");
        1
    } else {
        0
    }
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
    manifest.insert("_format".into(),serde_json::json!(2));
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

    fn restore_fixture(dir: &Path) -> Vec<Copied> {
        let db = Connection::open(dir.join("keep.db")).unwrap();
        let mut manifest = serde_json::Map::new();
        let mut expected = Vec::new();
        for name in kept().into_iter().map(|k| k.name).chain([FREEZE_EXEMPT]) {
            db.execute_batch(&format!(
                "create table {name} (id INTEGER, label TEXT, extra BLOB);
                 insert into {name} values (1, 'original', x'0102'), (2, null, null);"
            ))
            .unwrap();
            let cols = columns(&db, "main", name).unwrap();
            let tally = tally_sql(&db, &format!("select * from {name}"), &cols).unwrap();
            manifest.insert(name.into(), serde_json::json!({"rows": tally.rows, "digest": tally.hex()}));
            expected.push(Copied { name: name.into(), columns: cols, tally, capped: false });
        }
        manifest.insert("_left_out".into(), serde_json::json!([]));
        manifest.insert("_format".into(),serde_json::json!(2));
        std::fs::write(dir.join("manifest.json"), serde_json::Value::Object(manifest).to_string()).unwrap();
        expected
    }

    #[test]
    fn restore_refuses_changed_rows_even_when_counts_match() {
        let dir = tempfile::tempdir().unwrap();
        let expected = restore_fixture(dir.path());
        let snapshot = Connection::open(dir.path().join("keep.db")).unwrap();
        assert_eq!(verify_snapshot(dir.path(), &snapshot).unwrap().len(), expected.len());
        snapshot.execute("update predb_titles set label = 'changed' where id = 1", []).unwrap();
        assert!(verify_snapshot(dir.path(), &snapshot).unwrap_err().to_string().contains("predb_titles"));
    }

    #[test]
    fn restore_refuses_incomplete_snapshots() {
        let dir = tempfile::tempdir().unwrap();
        restore_fixture(dir.path());
        let manifest = dir.path().join("manifest.json");
        let mut json: serde_json::Value = serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
        json["_left_out"] = serde_json::json!(["predb_feeds"]);
        std::fs::write(manifest, json.to_string()).unwrap();
        let snapshot = Connection::open(dir.path().join("keep.db")).unwrap();
        assert!(verify_snapshot(dir.path(), &snapshot).is_err());
    }

    #[test]
    fn old_checked_keep_snapshots_keep_their_original_restore_scope() {
        let dir=tempfile::tempdir().unwrap();restore_fixture(dir.path());
        let path=dir.path().join("manifest.json");
        let mut manifest:serde_json::Map<String,serde_json::Value>=serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let conn=Connection::open(dir.path().join("keep.db")).unwrap();
        for table in kept().into_iter().filter(|table|table.name.starts_with("raw_")) {
            conn.execute_batch(&format!("drop table {}",quoted(table.name))).unwrap();manifest.remove(table.name);
        }
        manifest.remove("_format");std::fs::write(&path,serde_json::to_vec(&manifest).unwrap()).unwrap();
        let copied=verify_snapshot(dir.path(),&conn).unwrap();
        assert_eq!(copied.len(),11);assert!(!copied.iter().any(|table|table.name.starts_with("raw_")));
        manifest.insert("_format".into(),serde_json::json!(2));std::fs::write(&path,serde_json::to_vec(&manifest).unwrap()).unwrap();
        assert!(verify_snapshot(dir.path(),&conn).is_err(),"old scope cannot claim complete current coverage");
    }

    #[test]
    fn nfo_keep_copy_marks_encoding_and_top_up_does_not_encode_twice() {
        let Some(keep)=crate::pg::testing::Schema::empty() else {return};
        let Some(target)=crate::pg::testing::Schema::empty() else {return};
        let dir=tempfile::tempdir().unwrap();let file=dir.path().join("keep.db");
        let source=Connection::open(&file).unwrap();source.execute_batch("create table raw_nfo_passwords(name text not null,group_name text not null,content_id blob,password text not null)").unwrap();
        let names=["Kept\0nzb","atlas.nfo-name.v1:4b657074"];
        for name in names {source.execute("insert into raw_nfo_passwords values(?1,'g',x'01','')",[name]).unwrap();}
        let cols=columns(&source,"main","raw_nfo_passwords").unwrap();let tally=tally_sql(&source,"select * from raw_nfo_passwords",&cols).unwrap();
        let copied=[Copied {name:"raw_nfo_passwords".into(),columns:cols,tally,capped:false}];drop(source);
        let mut pg=keep.pool.get().unwrap();to_postgres_in_schema(&mut pg,&file,&copied,"one",false,&keep.name).unwrap();
        let mut tx=pg.transaction().unwrap();assert!(keep_nfo_encoded(&mut tx,&keep.name).unwrap());
        assert_eq!(pg_tally(&mut tx,&format!("{}.raw_nfo_passwords",quoted(&keep.name)),&copied[0].columns).unwrap(),tally);tx.rollback().unwrap();
        let home=dir.path().join("home");std::fs::create_dir(&home).unwrap();let main=home.join("atlas.db");crate::db::create_at(&main).unwrap();
        crate::predb::db::create(&crate::predb::db::path_for(&main)).unwrap();crate::media::store::open(&crate::media::store::path_for(&main)).unwrap();
        let details=crate::db::create_at(&crate::media::details::path_for(&main)).unwrap();details.execute_batch(crate::media::details::SCHEMA).unwrap();details.execute_batch("alter table details add column backdrop text").unwrap();drop(details);
        crate::raw_store::create(&crate::raw_store::path_for(&main)).unwrap();
        let mut pg=target.pool.get().unwrap();crate::pg::ext::copy_caches_for_fresh_catalog(&mut pg,&main,&target.name,true,crate::pg::ext::KeepSource::Named(&keep.name)).unwrap();
        let mut actual=pg.query("select name from ext_raw_nfo_passwords",&[]).unwrap().into_iter().map(|row|crate::raw_store::nfo_key::decode(&row.get::<_,String>(0)).unwrap()).collect::<Vec<_>>();
        actual.sort();let mut expected=names.map(str::to_owned).to_vec();expected.sort();assert_eq!(actual,expected);
    }

    #[test]
    fn restore_to_a_new_schema_checks_rows_and_refuses_existing_schema() {
        let Some(test) = crate::pg::testing::Schema::empty() else { return };
        let dir = tempfile::tempdir().unwrap();
        restore_fixture(dir.path());
        let snapshot = Connection::open(dir.path().join("keep.db")).unwrap();
        let copied = verify_snapshot(dir.path(), &snapshot).unwrap();
        let mut pg = test.pool.get().unwrap();
        let destination = format!("keep_restore_{}", test.name);
        let db: String = pg.query_one("select current_database()", &[]).unwrap().get(0);
        assert!(restore_postgres(&mut pg, &snapshot, &copied, "wrong_db", &destination).is_err());
        restore_postgres(&mut pg, &snapshot, &copied, &db, &destination).unwrap();
        assert!(restore_postgres(&mut pg, &snapshot, &copied, &db, &destination).is_err());
        let (label, extra): (String, Vec<u8>) = {
            let row = pg
                .query_one(&format!("select label, extra from {destination}.predb_titles where id = 1"), &[])
                .unwrap();
            (row.get(0), row.get(1))
        };
        pg.batch_execute(&format!("drop schema {destination} cascade")).unwrap();
        assert_eq!(label, "original");
        assert_eq!(extra, [1, 2]);
    }

    #[test]
    fn keep_conversion_commits_durably_when_the_default_is_off() {
        let Some(test) = crate::pg::testing::Schema::empty() else { return };
        let dir = tempfile::tempdir().unwrap();
        let copied = restore_fixture(dir.path());
        let file = dir.path().join("keep.db");
        let mut pg = test.pool.get().unwrap();
        to_postgres_in_schema(&mut pg, &file, &copied, "1", false, &test.name).unwrap();
        pg.batch_execute(
            "create function durable_only() returns trigger language plpgsql as $$ begin
            if current_setting('synchronous_commit') <> 'on' then raise exception 'not durable'; end if;
            return new; end $$;
            create trigger durable_only before insert on runs for each row execute function durable_only();
            set synchronous_commit=off",
        )
        .unwrap();
        assert!(pg.execute("insert into runs values ('control','control',0,'control')", &[]).is_err());
        assert!(
            to_postgres_in_schema(&mut pg, &file, &copied, "2", false, &test.name).is_ok(),
            "conversion must override asynchronous session durability"
        );
        assert_eq!(pg.query_one("select count(*) from runs where stamp='2'", &[]).unwrap().get::<_, i64>(0), 21);
        assert_eq!(pg.query_one("show synchronous_commit", &[]).unwrap().get::<_, String>(0), "off");
    }

    #[test]
    fn keep_restore_commits_durably_when_the_default_is_off() {
        let Some(test) = crate::pg::testing::Schema::empty() else { return };
        let dir = tempfile::tempdir().unwrap();
        let copied = restore_fixture(dir.path());
        let snapshot = Connection::open(dir.path().join("keep.db")).unwrap();
        let schema = format!("keep_restore_{}", test.name);
        let mut pg = test.pool.get().unwrap();
        pg.batch_execute("set synchronous_commit=off").unwrap();
        let mut tx = pg.transaction().unwrap();
        restore_postgres_in_transaction(&mut tx, &snapshot, &copied, &schema).unwrap();
        let body: Vec<u8> =
            tx.query_one(&format!("select extra from {schema}.predb_titles where id=1"), &[]).unwrap().get(0);
        assert_eq!(body, [1, 2]);
        assert_eq!(
            tx.query_one("show synchronous_commit", &[]).unwrap().get::<_, String>(0),
            "on",
            "restore must override asynchronous session durability before commit"
        );
        tx.commit().unwrap();
        assert_eq!(pg.query_one("show synchronous_commit", &[]).unwrap().get::<_, String>(0), "off");
        let label: String =
            pg.query_one(&format!("select label from {schema}.predb_titles where id=1"), &[]).unwrap().get(0);
        assert_eq!(label, "original");
        pg.batch_execute(&format!("drop schema {schema} cascade")).unwrap();
    }

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
    fn nfo_source_tally_preserves_original_zero_byte_name_digest() {
        let db=Connection::open_in_memory().unwrap();
        db.execute_batch("create table raw_nfo_passwords(name text,group_name text,content_id blob,password text)").unwrap();
        let names=["Kept\0nzb","é\0nzb","atlas.nfo-name.v1:4b657074","Kept","kept"];
        let (mut expected,mut encoded,mut scratch)=(Tally::default(),Tally::default(),Vec::new());
        for name in names {
            db.execute("insert into raw_nfo_passwords values(?1,'g',x'01','')",[name]).unwrap();
            let mut row=vec![Value::Text(name.into()),Value::Text("g".into()),Value::Blob(vec![1]),Value::Text(String::new())];
            expected.add(&row,&mut scratch);row[0]=Value::Text(crate::raw_store::nfo_key::encode(name));encoded.add(&row,&mut scratch);
        }
        let cols=columns(&db,"main","raw_nfo_passwords").unwrap();
        assert_eq!(tally_sql(&db,"select * from raw_nfo_passwords",&cols).unwrap(),expected);
        assert_ne!(expected,encoded,"source digest must hash original values, not encoded PG spelling");
        assert_eq!(db.query_row("select name from raw_nfo_passwords limit 1",[],|r|r.get::<_,String>(0)).unwrap(),names[0]);
    }

    #[test]
    fn nfo_tally_exception_never_admits_other_zero_byte_text_or_wrong_value_types() {
        let db=Connection::open_in_memory().unwrap();
        db.execute_batch("create table raw_nfo_passwords(name text,group_name text,password text); create table other_cache(name text)").unwrap();
        let nfo=columns(&db,"main","raw_nfo_passwords").unwrap();
        for (group,password) in [("bad\0group",""),("g","bad\0password")] {
            db.execute("delete from raw_nfo_passwords",[]).unwrap();
            db.execute("insert into raw_nfo_passwords values('Kept',?1,?2)",[group,password]).unwrap();
            assert!(tally_sql(&db,"select * from raw_nfo_passwords",&nfo).is_err());
        }
        db.execute("insert into other_cache values(?1)",["Kept\0nzb"]).unwrap();
        let other=columns(&db,"main","other_cache").unwrap();
        assert!(tally_sql(&db,"select name from other_cache /* raw_nfo_passwords.name */",&other).is_err(),"SQL text cannot grant codec admission");
        db.execute_batch("delete from raw_nfo_passwords; insert into raw_nfo_passwords values(x'4b00','g','')").unwrap();
        assert!(tally_sql(&db,"select * from raw_nfo_passwords",&nfo).is_err(),"encoded name remains a TEXT field");
    }

    #[test]
    fn keep_build_and_snapshot_readback_preserve_nested_nfo_names() {
        let dir=tempfile::tempdir().unwrap();let main=dir.path().join("atlas.db");crate::db::create_at(&main).unwrap();
        crate::predb::db::create(&crate::predb::db::path_for(&main)).unwrap();
        crate::media::store::open(&crate::media::store::path_for(&main)).unwrap();
        let details=crate::db::create_at(&crate::media::details::path_for(&main)).unwrap();details.execute_batch(crate::media::details::SCHEMA).unwrap();details.execute_batch("alter table details add column backdrop text").unwrap();drop(details);
        let raw=crate::raw_store::create(&crate::raw_store::path_for(&main)).unwrap();
        for name in ["Kept\0nzb","atlas.nfo-name.v1:4b657074"] {raw.execute("insert into raw_nfo_passwords(name,group_name,content_id,password) values(?1,'g',x'01','')",[name]).unwrap();}drop(raw);
        let out=dir.path().join("keep.db");let copied=build(&main,&out,false).unwrap();
        let mut manifest=serde_json::Map::new();
        for table in &copied {manifest.insert(table.name.clone(),serde_json::json!({"rows":table.tally.rows,"digest":table.tally.hex()}));}
        manifest.insert("_left_out".into(),serde_json::json!([]));manifest.insert("_format".into(),serde_json::json!(2));
        std::fs::write(dir.path().join("manifest.json"),serde_json::to_vec(&manifest).unwrap()).unwrap();
        let snapshot=Connection::open(&out).unwrap();let checked=verify_snapshot(dir.path(),&snapshot).unwrap();
        assert_eq!(checked.iter().find(|t|t.name=="raw_nfo_passwords").unwrap().tally,copied.iter().find(|t|t.name=="raw_nfo_passwords").unwrap().tally);
        assert!(snapshot.prepare("select 1 from raw_nfo_passwords where name=?1 and password='' ").unwrap().exists(["Kept\0nzb"]).unwrap());
        snapshot.execute("update raw_nfo_passwords set name=?1 where name=?2",["Changed\0nzb","Kept\0nzb"]).unwrap();
        assert!(verify_snapshot(dir.path(),&snapshot).is_err(),"manifest detects original-name changes with unchanged row count");
    }

    #[test]
    fn declared_types_map_to_postgres_ones() {
        assert_eq!(PgKind::of("INTEGER"), PgKind::Int);
        assert_eq!(PgKind::of("TEXT"), PgKind::Text);
        assert_eq!(PgKind::of("BLOB"), PgKind::Blob);
        assert_eq!(PgKind::of("REAL"), PgKind::Real);
        assert_eq!(PgKind::of(""), PgKind::Text);
    }

    /// Against an isolated Postgres schema: every table goes over, reads back with the same digest, and a second
    /// run replaces it whole.
    #[test]
    fn the_kept_tables_go_into_postgres_and_read_back_the_same() {
        let Some(test) = crate::pg::testing::Schema::empty() else { return };
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
        let mut pg = test.pool.get().unwrap();
        to_postgres_in_schema(&mut pg, &out, &copied, "1", false, &test.name).unwrap();
        to_postgres_in_schema(&mut pg, &out, &copied, "2", false, &test.name).unwrap();
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
        assert!(to_postgres_in_schema(&mut pg, &empty, &fewer, "3", false, &test.name).is_err());
        let runs: i64 = pg.query_one("select count(distinct stamp) from runs", &[]).unwrap().get(0);
        assert_eq!(runs, 2, "the refused run left nothing");
        let n: i64 = pg.query_one("select count(*) from predb_titles", &[]).unwrap().get(0);
        assert_eq!(n, 3);
    }
}
