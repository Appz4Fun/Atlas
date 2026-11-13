//! The `ext_*` tables: what came from an external service (PreDB, the media providers, OpenSubtitles, the HTTP
//! cache's pinned snapshots) and releases thawed by hand. Indexing again can't bring these back, so they get what the
//! rest of the database doesn't need:
//!
//! - `copy_from_sqlite` moves them over once, at the cutover (indexing stopped, and in the same release as the
//!   providers' writes move to these tables, so nothing written after the copy is left behind in SQLite): each table copied from its SQLite file and checked
//!   against it by row count and an order-free digest of every row, then topped up from the `keep` schema when the
//!   database has one (an `--export-keep` snapshot, whenever it was taken: rows the SQLite raw store's caps pruned
//!   since come back, and nothing already there is overwritten; tables or columns the snapshot hasn't got are skipped);
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

use anyhow::{Context, Result, bail, ensure};
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
        Source { ext: "ext_raw_probes", db: raw, table: "raw_probes" },
        Source { ext: "ext_raw_covers", db: raw, table: "raw_covers" },
        Source { ext: "ext_raw_archives", db: raw, table: "raw_archives" },
        Source { ext: "ext_raw_archive_inner_files", db: raw, table: "raw_archive_inner_files" },
        Source { ext: "ext_raw_par2", db: raw, table: "raw_par2" },
        Source { ext: "ext_raw_par2_files", db: raw, table: "raw_par2_files" },
        Source { ext: "ext_raw_rev_volumes", db: raw, table: "raw_rev_volumes" },
        Source { ext: "ext_raw_nfo_passwords", db: raw, table: "raw_nfo_passwords" },
        Source { ext: "ext_raw_stills", db: raw, table: "raw_stills" },
        Source { ext: "ext_raw_audio_prints", db: raw, table: "raw_audio_prints" },
    ]
}

pub(crate) fn owns_table(table: &str) -> bool {
    ["ext_freeze_exempt","ext_preserved_cache_rows","ext_cache_identity_keys","ext_segment_crc_cache","ext_cached_images","ext_asset_files"].contains(&table)
        || sources().iter().any(|source| source.ext == table)
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
        "ext_raw_probes" => "raw_probes",
        "ext_raw_covers" => "raw_covers",
        "ext_raw_archives" => "raw_archives",
        "ext_raw_archive_inner_files" => "raw_archive_inner_files",
        "ext_raw_par2" => "raw_par2",
        "ext_raw_par2_files" => "raw_par2_files",
        "ext_raw_rev_volumes" => "raw_rev_volumes",
        "ext_raw_nfo_passwords" => "raw_nfo_passwords",
        "ext_raw_stills" => "raw_stills",
        "ext_raw_audio_prints" => "raw_audio_prints",
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

/// The transaction's commit waits for its WAL to be flushed, whatever the session or role defaults to: a move or
/// restore that says it's done must survive a crash right after, as the SQLite copy it replaces may be gone by then.
fn durable(tx: &mut postgres::Transaction<'_>) -> Result<()> {
    tx.batch_execute("set local synchronous_commit = on")?;
    Ok(())
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

/// Which preservation snapshot supplies facts absent from SQLite.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum KeepSource<'a> {
    /// Use the conventional `keep` schema if it exists.
    #[default]
    Optional,
    /// Require a compatible, readable snapshot in this schema.
    Named(&'a str),
    /// Import only SQLite, without consulting a snapshot.
    Disabled,
}

fn selected_keep<'a>(source: KeepSource<'a>) -> Option<(&'a str, bool)> {
    match source {
        KeepSource::Optional => Some(("keep", false)),
        KeepSource::Named(name) => Some((name, true)),
        KeepSource::Disabled => None,
    }
}

/// Resolve the source without creating or changing it. All source reads use this validated name.
fn resolve_keep<'a>(
    tx: &mut postgres::Transaction<'_>,
    destination: &str,
    source: KeepSource<'a>,
) -> Result<Option<&'a str>> {
    let Some((name, required)) = selected_keep(source) else { return Ok(None) };
    if !super::valid_schema(name) {
        bail!("keep source must be a plain lowercase schema name");
    }
    if name == destination {
        bail!("keep source must differ from the destination schema");
    }
    let exists: bool = tx.query_one("select exists (select 1 from pg_namespace where nspname = $1)", &[&name])?.get(0);
    if !exists {
        if required {
            bail!("keep source schema {name} does not exist");
        }
        return Ok(None);
    }
    check_keep_access(name, required, || {
        Ok(tx.query_one("select has_schema_privilege($1, 'USAGE')", &[&name])?.get(0))
    })?;
    Ok(Some(name))
}

/// Optional snapshots defer permission checks to their visible tables; named snapshots require schema access.
fn check_keep_access(name: &str, required: bool, readable: impl FnOnce() -> Result<bool>) -> Result<()> {
    if required && !readable()? {
        bail!("keep source schema {name} is not readable");
    }
    Ok(())
}

/// Explicit snapshots may omit entire tables and optional columns, but must not silently lose unsupported data.
/// Compare with the migrated destination, including the renamed release id of hand thaws.
fn validate_keep(tx: &mut postgres::Transaction<'_>, name: &str,include_manual:bool) -> Result<()> {
    let mut compatible = 0;
    let pairs = sources()
        .into_iter()
        .map(|s| (s.ext, keep_name(s.ext).unwrap()))
        .chain(include_manual.then_some(("ext_freeze_exempt", "freeze_exempt")));
    for (ext, keep) in pairs {
        let relation = tx.query_opt(
            "select c.relkind::text, has_table_privilege(c.oid, 'SELECT')
               from pg_class c join pg_namespace n on n.oid=c.relnamespace
              where n.nspname=$1 and c.relname=$2",
            &[&name, &keep],
        )?;
        let Some(relation) = relation else { continue };
        let kind: String = relation.get(0);
        if kind != "r" && kind != "p" {
            bail!("keep source {name}.{keep}: incompatible table kind");
        }
        if !relation.get::<_, bool>(1) {
            bail!("keep source {name}.{keep} is not readable");
        }
        let cols = tx.query(
            "select column_name::text, udt_name::text from information_schema.columns
              where table_schema=$1 and table_name=$2",
            &[&name, &keep],
        )?;
        let theirs: Vec<(String, String)> = cols.iter().map(|r| (r.get(0), r.get(1))).collect();
        let ours = tx.query(
            "select column_name::text, udt_name::text,
                    (is_nullable='NO' and column_default is null) or is_identity='YES'
               from information_schema.columns where table_schema=current_schema() and table_name=$1",
            &[&ext],
        )?;
        for r in &ours {
            let column: String = r.get(0);
            let source_column =
                if ext == "ext_freeze_exempt" && column == "old_release_id" { "release_id" } else { &column };
            let ty: String = r.get(1);
            let required = r.get::<_, bool>(2)
                || ext == "ext_freeze_exempt"
                || (ext == "ext_predb_page_events" && column == "page");
            match theirs.iter().find(|(n, _)| n == source_column) {
                Some((_, source_ty)) if source_ty != &ty => {
                    bail!("keep source {name}.{keep}: incompatible column {source_column}")
                }
                None if required => bail!("keep source {name}.{keep}: incompatible shape, missing {source_column}"),
                _ => {}
            }
        }
        for (column, _) in &theirs {
            let dest_column =
                if ext == "ext_freeze_exempt" && column == "release_id" { "old_release_id" } else { column };
            if !ours.iter().any(|r| r.get::<_, String>(0) == dest_column) {
                bail!("keep source {name}.{keep}: incompatible column {column}");
            }
        }
        // A zero-row SELECT checks privileges on every column even when the table is empty.
        tx.batch_execute(&format!(
            "select * from {}.{} limit 0",
            keep_export::quoted(name),
            keep_export::quoted(keep)
        ))?;
        compatible += 1;
    }
    if compatible == 0 {
        bail!("keep source schema {name} has no compatible keep tables");
    }
    Ok(())
}

/// Every `ext_*` table filled from the SQLite files beside `main` (and the shards' hand thaws), then topped up from
/// `keep.*` when the database has it, in one transaction. A dry run (`apply` false) reads and checks the SQLite side
/// only. Refused if any `ext_*` table already has rows: the move happens once.
/// `schema` is migrated first, in the same transaction, so a dry run (rolled back) leaves Postgres as it was.
pub fn copy_from_sqlite(c: &mut Client, main: &Path, schema: &str, apply: bool) -> Result<Vec<Moved>> {
    copy_from_sqlite_with_keep(c, main, schema, apply, KeepSource::Optional)
}

/// As `copy_from_sqlite`, with an explicit read-only preservation source policy.
pub fn copy_from_sqlite_with_keep(
    c: &mut Client,
    main: &Path,
    schema: &str,
    apply: bool,
    source: KeepSource<'_>,
) -> Result<Vec<Moved>> {
    Ok(copy_external(c,main,schema,apply,source,ImportPolicy::Paired)?.copied)
}

/// Cache-only fresh-catalog conversion. No main/header/shard operational rows,
/// hand-thaw state, full main-file witness or historical pairing receipt is
/// imported. Exact cache copy checks run before the two explicit derived-state
/// resets; `copied` reports those original source tallies, not a final receipt
/// claiming the reset fields stayed unchanged.
pub fn copy_caches_for_fresh_catalog(
    c:&mut Client,main:&Path,schema:&str,apply:bool,source:KeepSource<'_>,
) -> Result<FreshCacheImport> {
    copy_external(c,main,schema,apply,source,ImportPolicy::Fresh)
}
#[derive(Debug)]
pub struct FreshCacheImport {pub copied:Vec<Moved>,pub predb_late_reset:u64,pub media_fanout_reset:u64}
#[derive(Clone,Copy,PartialEq,Eq)] enum ImportPolicy {Paired,Fresh}
fn copy_external(c:&mut Client,main:&Path,schema:&str,apply:bool,source:KeepSource<'_>,policy:ImportPolicy)->Result<FreshCacheImport> {
    ensure!(super::valid_schema(schema),"invalid external conversion schema");
    let witness=if policy==ImportPolicy::Paired {super::history_companion::Witness::capture(main)?} else {None};
    let lite = Connection::open_in_memory()?;
    let mut tx = c.transaction()?;
    tx.query_one("select pg_advisory_xact_lock_shared(hashtext('atlas.indexer'),hashtext($1))",&[&schema])?;
    durable(&mut tx)?;
    let keep_schema = resolve_keep(&mut tx, schema, source)?;
    super::migrate::run_in_transaction(&mut tx, schema)?;
    if let (KeepSource::Named(_), Some(name)) = (source, keep_schema) {
        validate_keep(&mut tx, name,policy==ImportPolicy::Paired)?;
    }
    for t in tables_of(&mut tx)? {
        let any: bool =
            tx.query_one(&format!("select exists (select 1 from {})", keep_export::quoted(&t)), &[])?.get(0);
        if any {
            bail!("{t} has rows already: the external data was moved before (refusing to copy over it)");
        }
    }
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
            if s.ext.starts_with("ext_raw_") {
                preserve_variants(&mut tx,s.ext,&keep_export::quoted(s.ext),
                    &cols.iter().map(|column|column.name.clone()).collect::<Vec<_>>(),
                    &format!("sqlite:{}:{}:{}",s.ext,tally.rows,tally.hex()),
                    (s.ext=="ext_raw_nfo_passwords").then_some(crate::raw_store::nfo_key::PREFIX))?;
            }
            if let Some(name) = keep_schema
                && let Some(k) = keep_name(s.ext)
            {
                from_keep = top_up(&mut tx, name, s.ext, k, &cols)?;
            }
        }
        lite.execute("detach database src", [])?;
        println!("  {}: {} rows, digest {}{}", s.ext, tally.rows, tally.hex(), added(from_keep));
        moved.push(Moved { table: s.ext.into(), tally, from_keep });
    }
    if policy==ImportPolicy::Paired {moved.push(freeze_exempt(&mut tx, &lite, main, apply, keep_schema)?);}
    let (mut predb_late_reset,mut media_fanout_reset)=(0,0);
    if apply {
        if policy==ImportPolicy::Fresh {
            predb_late_reset=tx.execute("update ext_predb_meta set value=0 where key='late_after_id' and value is distinct from 0",&[])?;
            media_fanout_reset=tx.execute("update ext_media_titles set fanout_done=0 where fanout_done is distinct from 0",&[])?;
        }
        advance_counters(&mut tx)?;
        if let Some(witness) = witness {
            witness.check(main)?;
            // The existing owner verifies source copies and keep top-ups.
            // Receipt the final normalized target, including manual-thaw rows;
            // the source-only digest deliberately excludes retained top-ups.
            let mut relations = Vec::new();
            for table in tables_of(&mut tx)? {
                ensure!(owns_table(&table), "SourceMismatch: external receipt table ownership");
                let columns = tx.prepare(&format!("select * from {} limit 0",keep_export::quoted(&table)))?;
                let cols = columns.columns().iter().map(|col| {
                    use postgres::types::Type;
                    use keep_export::PgKind;
                    let pg=match *col.type_() {
                        Type::INT8=>PgKind::Int,Type::TEXT=>PgKind::Text,Type::FLOAT8=>PgKind::Real,Type::BYTEA=>PgKind::Blob,
                        _=>bail!("UnsupportedSchema: external receipt column type"),
                    };
                    Ok(Column::plain(col.name().into(),String::new(),pg))
                }).collect::<Result<Vec<_>>>()?;
                let final_tally=keep_export::pg_tally(&mut tx,&keep_export::quoted(&table),&cols)?;
                relations.push(super::history_companion::relation(&table,final_tally.rows,final_tally.digest));
            }
            super::history_companion::store(&mut tx,super::history_companion::Receipt{
                version:1,owner:"ext".into(),main_source_digest:witness.digest,relations,
                scope:"ext_normalized_and_keep_v1".into(),
            })?;
        }
        tx.commit()?;
    }
    Ok(FreshCacheImport {copied:moved,predb_late_reset,media_fanout_reset})
}

/// Inert original typed cache variants, not a reuse certificate or a source
/// completeness receipt. The origin namespace identifies the preservation
/// input; each canonical JSON row has its own SHA-256. PostgreSQL's bytea JSON
/// spelling retains every byte, signed bigint/NULL/text retain their types via
/// the descriptor. No aggregate of payloads is materialized on the client.
fn preserve_variants(
    tx:&mut postgres::Transaction<'_>,family:&str,relation:&str,columns:&[String],origin:&str,name_codec:Option<&str>,
) -> Result<u64> {
    ensure!(family.starts_with("ext_raw_") && sources().iter().any(|source|source.ext==family),"unowned cache preservation family");
    ensure!(columns.len()<=64,"cache preservation column budget");
    let list=columns.iter().map(|column|keep_export::quoted(column)).collect::<Vec<_>>().join(",");
    let statement=tx.prepare(&format!("select {list} from {relation} limit 0"))?;
    let shape=statement.columns().iter().map(|column|(column.name(),column.type_().name())).collect::<Vec<_>>();
    let schema=if family=="ext_raw_nfo_passwords" {
        serde_json::to_string(&serde_json::json!({"columns":shape,"key_codecs":name_codec.map(|codec|serde_json::json!({"name":codec})).unwrap_or_else(||serde_json::json!({}))}))?
    } else {serde_json::to_string(&shape)?};
    let name=if family=="ext_raw_archive_inner_files" {"release_name"} else {"name"};
    let content=if columns.iter().any(|column|column=="content_id") {"content_id"} else {"null::bytea"};
    Ok(tx.execute(&format!("insert into ext_preserved_cache_rows
      (family,source_digest,row_digest,name,group_name,content_id,schema_descriptor,row_format,row_bytes)
      select $1,sha256(convert_to($2,'UTF8')),sha256(convert_to('atlas.cache-row-json.v1:'||$3||':'||row_json,'UTF8')),
             {name},group_name,{content},$3,1,convert_to(row_json,'UTF8')
      from (select q.*,to_jsonb(q)::text as row_json from (select {list} from {relation}) q) preserved
      on conflict do nothing"),&[&family,&origin,&schema])?)
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

/// The rows of the selected snapshot table that `ext` hasn't got (by any of its keys), added. Only the columns both have.
fn top_up(tx: &mut postgres::Transaction<'_>, schema: &str, ext: &str, keep: &str, cols: &[Column]) -> Result<u64> {
    let there: Vec<String> = tx
        .query(
            "select column_name::text from information_schema.columns where table_schema = $2 and table_name = $1",
            &[&keep, &schema],
        )?
        .iter()
        .map(|r| r.get(0))
        .collect();
    if there.is_empty() {
        return Ok(0);
    }
    let both: Vec<&Column> = cols.iter().filter(|c| there.contains(&c.name)).collect();
    let list = both.iter().map(|c| keep_export::quoted(&c.name)).collect::<Vec<_>>().join(", ");
    let namespace = keep_export::quoted(schema);
    let nfo_encoded=ext=="ext_raw_nfo_passwords" && keep_export::keep_nfo_encoded(tx,schema)?;
    if ext.starts_with("ext_raw_") {
        let relation=format!("{namespace}.{}",keep_export::quoted(keep));
        // Save every original variant BEFORE natural-key top-up skips a key
        // occupied by a different/newer sidecar value. Never activate an old
        // variant merely because its preservation record exists.
        preserve_variants(tx,ext,&relation,&there,&format!("keep:{schema}:{keep}"),nfo_encoded.then_some(crate::raw_store::nfo_key::PREFIX))?;
    }
    let total: i64 =
        tx.query_one(&format!("select count(*) from {namespace}.{}", keep_export::quoted(keep)), &[])?.get(0);
    let keep_pages: bool = tx
        .query_one(
            "select exists (select 1 from information_schema.tables where table_schema = $1 and table_name = 'predb_pages')",
            &[&schema],
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
            &format!(
                "insert into ext_predb_page_events (seq, log_id, page, fetched_at)
             select (select coalesce(max(seq), 0) from ext_predb_page_events) + row_number() over (order by e.seq),
                    p.id, e.page, e.fetched_at
               from {namespace}.predb_page_events e
               join {namespace}.predb_pages k on k.id = e.log_id
               join ext_predb_pages p on p.source_id = k.source_id and p.body_sha256 = k.body_sha256
              where not exists (select 1 from ext_predb_page_events x
                                 where x.log_id = p.id and x.page = e.page and x.fetched_at = e.fetched_at)"
            ),
            &[],
        )?
    } else if let Some((id, there)) = renumbered(tx, schema, ext, keep)? {
        // a row is the same by what it holds, whatever id it had: one already there (`there`) is left out, and one
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
                     select k.* from {namespace}.{keep_q} k
                      where not {there}
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
        let selected=both.iter().map(|column|{
            if ext=="ext_raw_nfo_passwords" && column.name=="name" && !nfo_encoded {
                format!("case when \"name\" is null then null else '{}' || encode(convert_to(\"name\",'UTF8'),'hex') end",crate::raw_store::nfo_key::PREFIX)
            } else {keep_export::quoted(&column.name)}
        }).collect::<Vec<_>>().join(",");
        tx.execute(
            &format!(
                "insert into {ext} ({list}) select {selected} from {namespace}.{} on conflict do nothing",
                keep_export::quoted(keep)
            ),
            &[],
        )?
    };
    let skipped = (total as u64).saturating_sub(n);
    if skipped > 0 {
        println!("  {ext}: {skipped} rows of {schema}.{keep} are there already (by a key) and were left as they are");
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
/// How a keep row `k` of the tables matched by content is found already there, and the id column it renumbers.
///
/// A title is the importer's identity (`predb::import`): equal under Rust's `str::to_lowercase`, whichever feed brought
/// it, soo one that SQLite has again under other casing or from another feed since the snapshot isn't added twice.
/// The keys are made here in Rust, not with SQL's `lower()`, whose answer depends on the server's locale and ICU (a
/// Turkish locale lowercases `I` to `ı`) and could call two titles the same that the importer keeps apart.
fn renumbered(
    tx: &mut postgres::Transaction<'_>,
    schema: &str,
    ext: &str,
    keep: &str,
) -> Result<Option<(&'static str, String)>> {
    Ok(match ext {
        "ext_predb_titles" => {
            for (table, from) in [("ext_title_keys", ext.to_string()), ("keep_title_keys", format!("{}.{}", keep_export::quoted(schema), keep_export::quoted(keep)))] {
                tx.batch_execute(&format!(
                    "drop table if exists pg_temp.{table};
                     create temp table {table} (id bigint not null, key text collate \"C\" not null) on commit drop"
                ))?;
                title_keys(tx, &format!("select id, title from {from}"), table)?;
                tx.batch_execute(&format!("create index on pg_temp.{table} (key); create index on pg_temp.{table} (id); analyze pg_temp.{table}"))?;
            }
            Some((
                "id",
                "exists (select 1 from pg_temp.keep_title_keys kk join pg_temp.ext_title_keys e on e.key = kk.key \
                  where kk.id = k.id)"
                    .into(),
            ))
        }
        "ext_predb_pages" => Some((
            "id",
            "exists (select 1 from ext_predb_pages t where t.source_id = k.source_id and t.body_sha256 = k.body_sha256)"
                .into(),
        )),
        _ => None,
    })
}

/// `(id, title)` rows of `select` into the temp table `into` as `(id, title.to_lowercase())`, read through a cursor
/// a chunk at a time (one connection can't read and COPY at once).
fn title_keys(tx: &mut postgres::Transaction<'_>, select: &str, into: &str) -> Result<()> {
    use postgres::types::Type;
    tx.batch_execute(&format!("declare title_keys_cur no scroll cursor for {select}"))?;
    loop {
        let rows = tx.query("fetch 50000 from title_keys_cur", &[])?;
        if rows.is_empty() {
            break;
        }
        let sink = tx.copy_in(&format!("copy pg_temp.{into} (id, key) from stdin (format binary)"))?;
        let mut w = postgres::binary_copy::BinaryCopyInWriter::new(sink, &[Type::INT8, Type::TEXT]);
        for r in &rows {
            let (id, title): (i64, String) = (r.get(0), r.get(1));
            w.write(&[&id, &title.to_lowercase()])?;
        }
        w.finish()?;
    }
    tx.batch_execute("close title_keys_cur")?;
    Ok(())
}

/// Hand thaws from every shard, by the release's name and group (the id doesn't survive indexing again). A thaw
/// whose release row is gone can't be found again and is left out (counted); one release thawed in two shards keeps
/// the later time.
fn freeze_exempt(
    tx: &mut postgres::Transaction<'_>,
    lite: &Connection,
    main: &Path,
    apply: bool,
    keep_schema: Option<&str>,
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
        if let Some(schema) = keep_schema {
            let keep_has: bool = tx.query_one(
                "select exists (select 1 from information_schema.tables where table_schema = $1 and table_name = 'freeze_exempt')", &[&schema])?.get(0);
            if keep_has {
                from_keep = tx.execute(
                    &format!(
                        "insert into ext_freeze_exempt (name, group_name, at, old_release_id)
                     select name, group_name, at, release_id from {}.freeze_exempt
                      where name is not null and group_name is not null and at is not null
                     on conflict do nothing",
                        keep_export::quoted(schema)
                    ),
                    &[],
                )?;
            }
        }
    }
    let lost_note = if lost > 0 { format!(" ({lost} whose release row is gone left out)") } else { String::new() };
    println!("  {table}: {} rows{lost_note}{}", rows.len(), added(from_keep));
    Ok(Moved { table: table.into(), tally: Tally { rows: rows.len() as u64, digest: 0 }, from_keep })
}

/// `atlas --move-ext [--fresh-catalog] [--apply] [--keep-schema NAME]`: `copy_from_sqlite` into the `[storage]` Postgres (migrated first, and only
/// committed with `--apply`).
/// Fresh mode can read sidecars beside `--cache-source-main PATH` while keeping
/// the target config in a separate home. It never opens that main/header file.
fn cache_source(args:&[String],fresh:bool,default:&Path)->Result<PathBuf> {
    let positions=args.iter().enumerate().filter(|(_,argument)|argument.as_str()=="--cache-source-main").map(|(index,_)|index).collect::<Vec<_>>();
    ensure!(positions.len()<=1,"--cache-source-main supplied more than once");
    let Some(&index)=positions.first() else {return Ok(default.to_owned())};
    ensure!(fresh,"--cache-source-main requires --fresh-catalog");
    let path=args.get(index+1).filter(|path|!path.starts_with("--") && !path.is_empty()).context("--cache-source-main requires PATH")?;
    Ok(PathBuf::from(path))
}

pub fn run_move(args: &[String]) -> i32 {
    let apply = args.iter().any(|a| a == "--apply");
    let go = || -> Result<()> {
        let fresh=args.iter().any(|arg|arg=="--fresh-catalog");
        let main=cache_source(args,fresh,&crate::paths::database())?;
        let mut source = KeepSource::Optional;
        for (i, arg) in args.iter().enumerate() {
            if arg == "--keep-schema" {
                let name =
                    args.get(i + 1).filter(|v| !v.starts_with("--")).context("--keep-schema requires a value")?;
                if !super::valid_schema(name) {
                    bail!("keep source must be a plain lowercase schema name");
                }
                source = KeepSource::Named(name);
            }
        }
        let storage = crate::config::storage_config();
        storage.check()?;
        if storage.url.is_empty() {
            bail!("no [storage] url in config.toml");
        }
        let pool = super::Pool::new(&storage)?;
        let mut c = pool.get()?;
        println!(
            "{} external data from beside {}",
            if apply { "moving" } else { "checking (dry run)" },
            main.display()
        );
        if fresh {
            let imported=copy_caches_for_fresh_catalog(&mut c,&main,pool.schema(),apply,source)?;
            println!("fresh cache import: {} tables; derived PreDB rows reset {}, media fanout rows reset {}",imported.copied.len(),imported.predb_late_reset,imported.media_fanout_reset);
        } else {copy_from_sqlite_with_keep(&mut c, &main, pool.schema(), apply, source)?;}
        if !fresh && apply && let Some(receipt)=super::history_companion::digest(&mut c,pool.schema(),"ext")? {
            println!("External source-pairing receipt: {}",receipt.iter().map(|byte|format!("{byte:02x}")).collect::<String>());
        }
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
        match now.tables.iter().find(|t| t.table == *table) {
            // gone is refused for a churning table too: only its row count may go down
            None => return Some(format!("{table} was in an earlier dump and is gone")),
            Some(_) if CHURNS.contains(table) => {}
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
    durable(&mut tx)?;
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
    use crate::keep_export::PgKind;

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
        // a churning table with no pinned rows may empty, but not disappear
        let kept = [m(vec![cache(30, 0), t("ext_a", 5)])];
        assert_eq!(shrank(&kept, &m(vec![cache(0, 0), t("ext_a", 5)])), None);
        assert!(shrank(&kept, &m(vec![t("ext_a", 5)])).is_some(), "the HTTP cache table gone");
    }

    #[test]
    fn keep_selection_retains_the_optional_default() {
        assert_eq!(selected_keep(KeepSource::default()), Some(("keep", false)));
        assert_eq!(selected_keep(KeepSource::Named("snapshot")), Some(("snapshot", true)));
        assert_eq!(selected_keep(KeepSource::Disabled), None);
    }

    #[test]
    fn optional_keep_defers_readability_checks_while_named_requires_them() {
        let (_, required) = selected_keep(KeepSource::Optional).unwrap();
        let optional = check_keep_access("keep", required, || bail!("permission lookup must stay lazy"));
        assert!(optional.is_ok(), "optional source resolution must not query schema readability");

        let (_, required) = selected_keep(KeepSource::Named("snapshot")).unwrap();
        let named = check_keep_access("snapshot", required, || Ok(false));
        assert!(named.unwrap_err().to_string().contains("not readable"));
        assert!(check_keep_access("snapshot", required, || Ok(true)).is_ok());
    }

    #[test]
    fn named_keep_dry_run_checks_source_without_leaving_tables() {
        let Some(source) = super::super::testing::Schema::empty() else { return };
        let Some(destination) = super::super::testing::Schema::empty() else { return };
        let mut source_conn = source.pool.get().unwrap();
        source_conn
            .batch_execute(
                "create table predb_titles (id bigint, title text, source bigint, added_at bigint);
            insert into predb_titles values (1, 'Preserved.Title', 3, 5)",
            )
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let main = sqlite_files(dir.path());
        let mut c = destination.pool.get().unwrap();
        let moved =
            copy_from_sqlite_with_keep(&mut c, &main, &destination.name, false, KeepSource::Named(&source.name))
                .unwrap();
        assert!(moved.iter().all(|m| m.from_keep == 0));
        assert!(tables(&mut c).unwrap().is_empty());
        assert_eq!(
            source_conn.query_one("select title from predb_titles", &[]).unwrap().get::<_, String>(0),
            "Preserved.Title"
        );
        source_conn.batch_execute("alter table predb_titles drop column title").unwrap();
        assert!(
            copy_from_sqlite_with_keep(&mut c, &main, &destination.name, false, KeepSource::Named(&source.name))
                .is_err()
        );
        assert!(tables(&mut c).unwrap().is_empty());
    }

    /// With the session's default off (a role or database set that way), the move and the restore still commit
    /// durably: a trigger refuses any row written by a transaction that isn't.
    #[test]
    fn move_and_restore_commit_durably_when_the_default_is_off() {
        let Some(s) = super::super::testing::Schema::new() else { return };
        let dir = tempfile::tempdir().unwrap();
        let main = sqlite_files(dir.path());
        let mut c = s.pool.get().unwrap();
        c.batch_execute(
            "create function durable_only() returns trigger language plpgsql as $$ begin
               if current_setting('synchronous_commit') <> 'on' then raise exception 'not durable'; end if;
               return new; end $$;
             create trigger durable_only before insert on ext_predb_titles for each row execute function durable_only();
             set synchronous_commit = off",
        )
        .unwrap();
        copy_from_sqlite_with_keep(&mut c, &main, &s.name, true, KeepSource::Disabled).unwrap();
        let d = dump(&mut c, &dir.path().join("ext-dumps"), 3, false).unwrap();
        restore(&mut c, &d, true).unwrap();
        let after: String = c.query_one("show synchronous_commit", &[]).unwrap().get(0);
        assert_eq!(after, "off", "only the transactions' own setting changed");
        // and without it the trigger does refuse
        let refused =
            c.execute("insert into ext_predb_titles (title, source, added_at) values ('Async.Row-GRP', 3, 1)", &[]);
        assert!(refused.is_err());
    }

    /// Keep titles are matched by Rust's lowercasing, as the importer matches them, not by the server's `lower()`:
    /// other casing (ASCII or not) is the same title, a dotless `ı` is another one. The source is isolated.
    #[test]
    fn keep_titles_match_the_importers_identity() {
        let Some(source) = super::super::testing::Schema::empty() else { return };
        let Some(s) = super::super::testing::Schema::new() else { return };
        let mut c = s.pool.get().unwrap();
        let mut tx = c.transaction().unwrap();
        let namespace = keep_export::quoted(&source.name);
        tx.batch_execute(&format!(
            "create table {namespace}.predb_titles (id bigint, title text, source bigint, added_at bigint);
             insert into ext_predb_titles (id, title, source, added_at)
                 values (1, 'Some.FILE.Title-GRP', 3, 1), (2, 'école.Title-GRP', 3, 1);
             insert into {namespace}.predb_titles values
                 (1, 'some.file.title-grp', 5, 1),
                 (2, 'ÉCOLE.TITLE-GRP', 3, 1),
                 (3, 'Some.fıle.Title-GRP', 3, 1)"
        ))
        .unwrap();
        let col = |name: &str, pg| Column::plain(name.into(), String::new(), pg);
        let cols = [
            col("id", PgKind::Int),
            col("title", PgKind::Text),
            col("source", PgKind::Int),
            col("added_at", PgKind::Int),
        ];
        assert_eq!(top_up(&mut tx, &source.name, "ext_predb_titles", "predb_titles", &cols).unwrap(), 1);
        let added: Vec<(i64, String)> = tx
            .query("select id, title from ext_predb_titles where id > 2", &[])
            .unwrap()
            .iter()
            .map(|r| (r.get(0), r.get(1)))
            .collect();
        assert_eq!(added, [(3, "Some.fıle.Title-GRP".to_string())]);
        tx.rollback().unwrap();
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

    #[test]
    fn owner_receipt_covers_top_ups_and_admitted_manual_thaws() {
        let Some(keep) = super::super::testing::Schema::empty() else {return};
        let Some(target) = super::super::testing::Schema::new() else {return};
        let dir=tempfile::tempdir().unwrap();let main=sqlite_files(dir.path());
        let lite=Connection::open(&main).unwrap();lite.execute_batch("create table capture_marker(value integer);insert into capture_marker values(1);pragma wal_checkpoint(truncate);pragma journal_mode=delete").unwrap();drop(lite);
        let mut c=target.pool.get().unwrap();
        c.batch_execute(&format!("create table \"{}\".predb_titles(id bigint,title text,source bigint,added_at bigint);insert into \"{}\".predb_titles values(19,'Retained.Title-GRP',3,7);create table \"{}\".freeze_exempt(release_id bigint,at bigint,name text,group_name text);insert into \"{}\".freeze_exempt values(29,11,'Manual.Title-GRP','test.group'),(31,12,null,null)",keep.name,keep.name,keep.name,keep.name)).unwrap();
        copy_from_sqlite_with_keep(&mut c,&main,&target.name,true,KeepSource::Named(&keep.name)).unwrap();
        let data:Vec<u8>=c.query_one("select manifest from history_companion_receipts where owner_code='ext' and not dirty",&[]).unwrap().get(0);
        let receipt:super::super::history_companion::Receipt=serde_json::from_slice(&data).unwrap();
        let title=receipt.relations.iter().find(|r|r.table=="ext_predb_titles").unwrap();assert_eq!(title.rows,2);
        let cols=keep_export::columns(&Connection::open(&crate::predb::db::path_for(&main)).unwrap(),"main","titles").unwrap();
        let mut tx=c.transaction().unwrap();let tally=keep_export::pg_tally(&mut tx,"ext_predb_titles",&cols).unwrap();
        let expected=super::super::history_companion::relation("ext_predb_titles",tally.rows,tally.digest);
        assert_eq!(title.digest,expected.digest,"receipt digest includes exact post-top-up rows");
        let manual=receipt.relations.iter().find(|r|r.table=="ext_freeze_exempt").unwrap();assert_eq!(manual.rows,1);
    }

    /// SQLite files made with the providers' own schemas beside `dir/atlas.db`, a row or two each.
    fn sqlite_files(dir: &Path) -> PathBuf {
        let main = dir.join("atlas.db");
        crate::db::create_at(&main).unwrap();
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
        main
    }

    #[test]
    fn external_copy_retains_costly_natural_caches_and_negative_sentinels() {
        let Some(target)=super::super::testing::Schema::empty() else {return};
        let root=tempfile::tempdir().unwrap();let main=sqlite_files(root.path());
        let raw=Connection::open(crate::raw_store::path_for(&main)).unwrap();
        raw.execute_batch("insert into raw_probes(name,group_name,info,content_id,probed_at) values('Kept','test.group','{}',x'0102',17);
            insert into raw_covers(name,group_name,mime,sha256,content_id) values('Kept','test.group','image/jpeg','cover',x'0102');
            insert into raw_archives(name,group_name,password,content_id,owner_key,key_salt) values('Kept','test.group',0,x'0102',-9,11);
            insert into raw_archive_inner_files(release_name,group_name,seq,name,content_id) values('Kept','test.group',1,'inner',x'0102');
            insert into raw_par2(name,group_name,posted,content_id) values('Kept','test.group',zeroblob(20),x'0102');
            insert into raw_par2_files(name,group_name,content_id,file_id,set_id,md5,md5_16k,size,file_name,creator) values('Kept','test.group',x'0102',x'01',x'02',x'03',x'04',31,'inner','tool');
            insert into raw_rev_volumes(name,group_name,content_id,volume,size,crc32) values('Kept','test.group',x'0102',1,41,4294967295);
            insert into raw_nfo_passwords(name,group_name,password,content_id) values('Kept','test.group','',x'0102');
            insert into raw_stills(name,group_name,position,content_id,sha256) values('Kept','test.group',1,x'0102','still');
            insert into raw_audio_prints(name,group_name,position,content_id,at_ms,items) values('Kept','test.group',1,x'0102',23,x'0001')").unwrap();drop(raw);
        let mut client=target.pool.get().unwrap();
        let copied=copy_from_sqlite_with_keep(&mut client,&main,&target.name,true,KeepSource::Disabled).unwrap();
        for table in ["ext_raw_probes","ext_raw_covers","ext_raw_archives","ext_raw_archive_inner_files","ext_raw_par2",
            "ext_raw_par2_files","ext_raw_rev_volumes","ext_raw_nfo_passwords","ext_raw_stills","ext_raw_audio_prints"] {
            let found=copied.iter().find(|entry|entry.table==table).unwrap_or_else(||panic!("valuable cache omitted from existing external converter: {table}"));
            assert_eq!(found.tally.rows,1,"every original cache row must survive");
        }
        let row=client.query_one("select password,content_id from ext_raw_nfo_passwords",&[]).unwrap();
        assert_eq!(row.get::<_,String>(0),"");assert_eq!(row.get::<_,Option<Vec<u8>>>(1),Some(vec![1,2]));
        let row=client.query_one("select posted from ext_raw_par2",&[]).unwrap();
        assert_eq!(row.get::<_,Vec<u8>>(0),vec![0;20],"preserve old raw evidence without declaring it eligible");
        assert_eq!(client.query_one("select owner_key from ext_raw_archives",&[]).unwrap().get::<_,i64>(0),-9);
    }

    #[test]
    fn fresh_cache_copy_ignores_main_catalog_and_resets_only_derived_cache_state() {
        let Some(target)=super::super::testing::Schema::empty() else {return};
        let root=tempfile::tempdir().unwrap();let main=sqlite_files(root.path());
        let predb=Connection::open(crate::predb::db::path_for(&main)).unwrap();
        predb.execute("insert or replace into meta(key,value) values('late_after_id',99)",[]).unwrap();drop(predb);
        let media=Connection::open(crate::media::store::path_for(&main)).unwrap();
        media.execute("insert into media_titles(title_key,kind,title,state,checked_mask,checked_at,fanout_done) values(7,1,'Negative',2,31,17,1)",[]).unwrap();drop(media);
        // A trap directory proves the fresh path never opens/hashes main or
        // discovers its old release/operational tables.
        std::fs::remove_file(&main).unwrap();std::fs::create_dir(&main).unwrap();
        let mut client=target.pool.get().unwrap();
        let copied=copy_caches_for_fresh_catalog(&mut client,&main,&target.name,true,KeepSource::Disabled).unwrap();
        assert_eq!((copied.predb_late_reset,copied.media_fanout_reset),(1,1));
        assert!(!copied.copied.iter().any(|row|row.table=="ext_freeze_exempt"));
        assert_eq!(client.query_one("select value from ext_predb_meta where key='late_after_id'",&[]).unwrap().get::<_,i64>(0),0);
        let row=client.query_one("select state,checked_mask,checked_at,fanout_done from ext_media_titles where title_key=7",&[]).unwrap();
        assert_eq!((row.get::<_,i64>(0),row.get::<_,i64>(1),row.get::<_,i64>(2),row.get::<_,i64>(3)),(2,31,17,0));
        for table in ["groups","releases","ext_freeze_exempt","history_imports","history_companion_receipts"] {
            assert_eq!(client.query_one(&format!("select count(*) from {table}"),&[]).unwrap().get::<_,i64>(0),0,"fresh cache import must not seed {table}");
        }
        let predb=Connection::open_with_flags(crate::predb::db::path_for(&main),rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        assert_eq!(predb.query_row("select value from meta where key='late_after_id'",[],|row|row.get::<_,i64>(0)).unwrap(),99,"source remains unchanged");
    }

    #[test]
    fn fresh_cache_source_path_does_not_repurpose_the_target_config_home() {
        let args=vec!["--cache-source-main".into(),"/preserved/atlas.db".into()];
        let default=Path::new("/fresh/atlas.db");
        assert_eq!(cache_source(&args,true,default).unwrap(),Path::new("/preserved/atlas.db"));
        assert_eq!(cache_source(&[],true,default).unwrap(),default);
        assert!(cache_source(&args,false,default).is_err());
        assert!(cache_source(&args[..1],true,default).is_err());
        assert!(cache_source(&[args.clone(),args].concat(),true,default).is_err());
    }

    #[test]
    fn conflicting_costly_keep_variants_are_preserved_before_natural_key_top_up() {
        let Some(keep)=super::super::testing::Schema::empty() else {return};
        let Some(target)=super::super::testing::Schema::empty() else {return};
        let root=tempfile::tempdir().unwrap();let main=sqlite_files(root.path());
        let raw=Connection::open(crate::raw_store::path_for(&main)).unwrap();
        raw.execute("insert into raw_nfo_passwords(name,group_name,content_id,password) values('Kept','g',x'01','')",[]).unwrap();drop(raw);
        let mut client=target.pool.get().unwrap();
        client.batch_execute(&format!("create table \"{}\".raw_nfo_passwords(name text,group_name text,content_id bytea,password text);
          insert into \"{}\".raw_nfo_passwords values('Kept','g',decode('02','hex'),'old-secret')",keep.name,keep.name)).unwrap();
        copy_caches_for_fresh_catalog(&mut client,&main,&target.name,true,KeepSource::Named(&keep.name)).unwrap();
        assert_eq!(client.query_one("select password from ext_raw_nfo_passwords",&[]).unwrap().get::<_,String>(0),"","sidecar's selected negative cache stays selected");
        let rows=client.query("select row_bytes,content_id from ext_preserved_cache_rows where family='ext_raw_nfo_passwords' order by content_id",&[]).unwrap();
        assert_eq!(rows.len(),2,"different valuable same-key originals must not disappear through ON CONFLICT");
        let first:serde_json::Value=serde_json::from_slice(&rows[0].get::<_,Vec<u8>>(0)).unwrap();
        let second:serde_json::Value=serde_json::from_slice(&rows[1].get::<_,Vec<u8>>(0)).unwrap();
        assert_eq!(first["password"],"");assert_eq!(second["password"],"old-secret");
        assert_eq!(rows[0].get::<_,Vec<u8>>(1),[1]);assert_eq!(rows[1].get::<_,Vec<u8>>(1),[2]);
    }

    #[test]
    fn fresh_nfo_import_preserves_nested_zero_byte_names_and_literal_prefix_names() {
        let Some(target)=super::super::testing::Schema::empty() else {return};
        let root=tempfile::tempdir().unwrap();let main=sqlite_files(root.path());
        let raw=Connection::open(crate::raw_store::path_for(&main)).unwrap();
        let names=["Kept","Kept\0nzb","atlas.nfo-name.v1:4b657074","kept","é\0nzb"];
        for name in names {raw.execute("insert into raw_nfo_passwords(name,group_name,content_id,password) values(?1,'g',x'01','')",[name]).unwrap();}
        drop(raw);let mut client=target.pool.get().unwrap();
        let moved=copy_caches_for_fresh_catalog(&mut client,&main,&target.name,true,KeepSource::Disabled).unwrap();
        assert_eq!(moved.copied.iter().find(|table|table.table=="ext_raw_nfo_passwords").unwrap().tally.rows,names.len() as u64);
        let mut actual=client.query("select name,password from ext_raw_nfo_passwords",&[]).unwrap().into_iter().map(|row|{
            assert_eq!(row.get::<_,String>(1),"");crate::raw_store::nfo_key::decode(&row.get::<_,String>(0)).unwrap()
        }).collect::<Vec<_>>();actual.sort();let mut expected=names.map(str::to_owned).to_vec();expected.sort();assert_eq!(actual,expected);
        let raw=Connection::open_with_flags(crate::raw_store::path_for(&main),rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        assert!(raw.prepare("select 1 from raw_nfo_passwords where name=?1").unwrap().exists(["Kept\0nzb"]).unwrap());
    }

    /// SQLite files made with the providers' own schemas, a row or two each, moved, dumped, emptied and restored.
    #[test]
    fn move_dump_restore_round_trip() {
        let Some(s) = super::super::testing::Schema::empty() else { return };
        let dir = tempfile::tempdir().unwrap();
        let main = sqlite_files(dir.path());
        let mut c = s.pool.get().unwrap();
        // a dry run writes nothing, not even the migrations it needed
        copy_from_sqlite_with_keep(&mut c, &main, &s.name, false, KeepSource::Disabled).unwrap();
        let migrated: bool = c
            .query_one("select exists (select 1 from information_schema.tables where table_schema = $1)", &[&s.name])
            .unwrap()
            .get(0);
        assert!(!migrated, "a dry run left tables behind");
        let moved = copy_from_sqlite_with_keep(&mut c, &main, &s.name, true, KeepSource::Disabled).unwrap();
        let rows = |t: &str| moved.iter().find(|m| m.table == t).unwrap().tally.rows;
        assert_eq!(rows("ext_predb_titles"), 1);
        assert_eq!(rows("ext_http_cache"), 1);
        assert!(
            copy_from_sqlite_with_keep(&mut c, &main, &s.name, true, KeepSource::Disabled).is_err(),
            "a second move is refused"
        );
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
