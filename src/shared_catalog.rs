//! A copy of every shard's release-level tables in one shared Postgres database, so a lookup across groups is one
//! query instead of a loop over the shards (README, "Shared catalog"). Phase 1: the shards stay what's true and the
//! copy only follows them.
//!
//! Each copied table gets three triggers that put the key of a row that changed in the shard's `catalog_outbox`.
//! A publisher thread in the indexer reads a batch of keys and the rows they point at in one read transaction,
//! writes the rows that are still there and deletes the ones that are gone in one Postgres transaction, and only
//! then takes the batch out of the outbox. It sends a row as it is when it reads it, not as the change left it, so
//! changes don't have to arrive in order, and a batch it sends twice after a crash lands the same way.
//!
//! Off unless `shared_catalog.enabled` and `shared_catalog.url` are set. Turned off, the indexer drops the triggers
//! and the outbox at its next start, so the writers pay nothing for it; a copy turned off for a while is stale until
//! it's filled again.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::time::Duration;

use rusqlite::types::Value;
use rusqlite::{Connection, params};

use crate::config::SharedCatalogConfig;
use crate::metrics::names;
use crate::store;

/// Every table copied: what a lookup across groups or posts compares. Per-file article data stays in the shards.
pub const TABLES: &[&str] = &[
    "releases",
    "release_category",
    "release_uncategorized",
    "release_keys",
    "release_fingerprint",
    "release_probe",
    "release_nfo",
    "release_nfo_password",
    "release_opensubtitles",
    "release_stills",
    "release_cover",
    "media_release",
    "predb_match",
    "predb_inner_key",
    "par2_files",
    "upload_link",
    "upload_link_tried",
    "archive_release",
    "archive_inner_files",
    "segment_crc",
    "collections",
    "junk_releases",
    "junk_collections",
    "freeze_pending",
    "freeze_exempt",
    "nzb_pointed",
    "pp_state",
    "pp_queue",
    "pp_walks",
    "sidecars",
];

/// Tables whose key only means something inside its shard: collection keys are hashed with the shard's own secret
/// (`store::IdKey`), walk ids count per shard. Their copies are keyed by the shard too.
const SHARD_KEYED: &[&str] = &["collections", "junk_collections", "pp_walks"];

pub const OUTBOX: &str = "catalog_outbox";

/// A column's Postgres type, from its declared SQLite type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PgType {
    Int,
    Real,
    Text,
    Bytes,
}

impl PgType {
    fn of(declared: &str) -> PgType {
        let d = declared.to_ascii_uppercase();
        if d.contains("INT") {
            PgType::Int
        } else if d.contains("CHAR") || d.contains("TEXT") || d.contains("CLOB") {
            PgType::Text
        } else if d.contains("REAL") || d.contains("FLOA") || d.contains("DOUB") {
            PgType::Real
        } else {
            PgType::Bytes
        }
    }

    fn sql(self) -> &'static str {
        match self {
            PgType::Int => "bigint",
            PgType::Real => "double precision",
            PgType::Text => "text",
            PgType::Bytes => "bytea",
        }
    }
}

/// One copied table as the shard has it now: columns added by a migration come along by themselves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Table {
    pub name: String,
    pub cols: Vec<(String, PgType)>,
    /// the primary key's columns, in key order (at most two in every copied table)
    pub key: Vec<String>,
    pub shard_keyed: bool,
}

impl Table {
    fn read(conn: &Connection, name: &str) -> rusqlite::Result<Option<Table>> {
        let mut cols = Vec::new();
        let mut key: Vec<(i64, String)> = Vec::new();
        let mut stmt = conn.prepare(&format!("pragma main.table_info({name})"))?;
        let mut rows = stmt.query([])?;
        while let Some(r) = rows.next()? {
            let col: String = r.get(1)?;
            let declared: String = r.get::<_, Option<String>>(2)?.unwrap_or_default();
            let pk: i64 = r.get(5)?;
            if pk > 0 {
                key.push((pk, col.clone()));
            }
            cols.push((col, PgType::of(&declared)));
        }
        if cols.is_empty() || key.is_empty() || key.len() > 2 {
            return Ok(None);
        }
        key.sort();
        Ok(Some(Table {
            name: name.to_string(),
            cols,
            key: key.into_iter().map(|(_, c)| c).collect(),
            shard_keyed: SHARD_KEYED.contains(&name),
        }))
    }

    fn key_type(&self, i: usize) -> PgType {
        let k = &self.key[i];
        self.cols.iter().find(|(c, _)| c == k).map_or(PgType::Bytes, |(_, t)| *t)
    }

    /// The Postgres statements that make the copy's table, or add what a shard migration added since.
    pub fn ddl(&self) -> Vec<String> {
        let mut cols: Vec<String> = vec!["shard smallint not null".into()];
        cols.extend(self.cols.iter().map(|(c, t)| format!("{} {}", quote(c), t.sql())));
        let mut key: Vec<String> = self.key.iter().map(|c| quote(c)).collect();
        if self.shard_keyed {
            key.insert(0, "shard".into());
        }
        let mut out = vec![format!(
            "create table if not exists {} ({}, primary key ({}))",
            quote(&self.name),
            cols.join(", "),
            key.join(", ")
        )];
        out.extend(self.cols.iter().map(|(c, t)| {
            format!("alter table {} add column if not exists {} {}", quote(&self.name), quote(c), t.sql())
        }));
        out
    }

    /// `insert ... on conflict do update` for `rows` rows.
    pub fn upsert_sql(&self, rows: usize) -> String {
        let width = self.cols.len() + 1;
        let names: Vec<String> =
            std::iter::once("shard".to_string()).chain(self.cols.iter().map(|(c, _)| quote(c))).collect();
        let values: Vec<String> = (0..rows)
            .map(|r| {
                let ps: Vec<String> = (0..width).map(|i| format!("${}", r * width + i + 1)).collect();
                format!("({})", ps.join(", "))
            })
            .collect();
        let mut conflict: Vec<String> = self.key.iter().map(|c| quote(c)).collect();
        if self.shard_keyed {
            conflict.insert(0, "shard".into());
        }
        let set: Vec<String> =
            names.iter().filter(|n| !conflict.contains(n)).map(|n| format!("{n} = excluded.{n}")).collect();
        let action =
            if set.is_empty() { "do nothing".to_string() } else { format!("do update set {}", set.join(", ")) };
        format!(
            "insert into {} ({}) values {} on conflict ({}) {}",
            quote(&self.name),
            names.join(", "),
            values.join(", "),
            conflict.join(", "),
            action
        )
    }

    /// `delete` of one row by its key, only the copy this shard sent.
    pub fn delete_sql(&self) -> String {
        let mut wh = vec!["shard = $1".to_string()];
        wh.extend(self.key.iter().enumerate().map(|(i, c)| format!("{} = ${}", quote(c), i + 2)));
        format!("delete from {} where {}", quote(&self.name), wh.join(" and "))
    }

    fn select_sql(&self) -> String {
        let cols: Vec<String> = self.cols.iter().map(|(c, _)| quote(c)).collect();
        let wh: Vec<String> = self.key.iter().enumerate().map(|(i, c)| format!("{} = ?{}", quote(c), i + 1)).collect();
        format!("select {} from {} where {}", cols.join(", "), quote(&self.name), wh.join(" and "))
    }

    fn triggers(&self) -> String {
        let t = &self.name;
        let k = |side: &str| -> String {
            let k1 = format!("{side}.{}", quote(&self.key[0]));
            let k2 = self.key.get(1).map_or("null".to_string(), |c| format!("{side}.{}", quote(c)));
            format!("'{t}', {k1}, {k2}")
        };
        let moved =
            self.key.iter().map(|c| format!("old.{0} is not new.{0}", quote(c))).collect::<Vec<_>>().join(" or ");
        format!(
            "create trigger if not exists catalog_{t}_ai after insert on {qt} begin
                insert into {OUTBOX} (tbl, k1, k2) values ({new});
             end;
             create trigger if not exists catalog_{t}_ad after delete on {qt} begin
                insert into {OUTBOX} (tbl, k1, k2) values ({old});
             end;
             create trigger if not exists catalog_{t}_au after update on {qt} begin
                insert into {OUTBOX} (tbl, k1, k2) values ({new});
                insert into {OUTBOX} (tbl, k1, k2) select {old} where {moved};
             end;",
            qt = quote(t),
            new = k("new"),
            old = k("old"),
        )
    }
}

fn quote(ident: &str) -> String {
    format!("\"{}\"", ident.replace('"', "\"\""))
}

/// The copied tables this shard has.
pub fn tables(conn: &Connection) -> rusqlite::Result<Vec<Table>> {
    let mut out = Vec::new();
    for name in TABLES {
        if let Some(t) = Table::read(conn, name)? {
            out.push(t);
        }
    }
    Ok(out)
}

/// Puts the outbox and the triggers in a shard (`on`), or takes them out. Safe to repeat.
pub fn sync_triggers(conn: &Connection, on: bool) -> rusqlite::Result<()> {
    let tables = tables(conn)?;
    if on {
        let mut sql = outbox_ddl("main");
        for t in &tables {
            sql.push_str(&t.triggers());
        }
        conn.execute_batch(&format!("begin; {sql} commit;"))
    } else {
        let present: i64 = conn.query_row(
            "select count(*) from sqlite_schema where name = ?1 or (type = 'trigger' and name like 'catalog\\_%' escape '\\')",
            [OUTBOX],
            |r| r.get(0),
        )?;
        if present == 0 {
            return Ok(());
        }
        let mut sql = String::new();
        for t in &tables {
            for kind in ["ai", "ad", "au"] {
                sql.push_str(&format!("drop trigger if exists catalog_{}_{kind};", t.name));
            }
        }
        sql.push_str(&format!("drop table if exists {OUTBOX};"));
        conn.execute_batch(&format!("begin; {sql} commit;"))
    }
}

fn outbox_ddl(schema: &str) -> String {
    format!(
        "create table if not exists {schema}.{OUTBOX} (
            seq INTEGER PRIMARY KEY, tbl TEXT NOT NULL, k1, k2, at INTEGER NOT NULL DEFAULT (unixepoch())
         );"
    )
}

/// For a compaction, which builds a shard afresh from the original attached as `old`: the original's waiting
/// changes come along, and so do the releases the copy changed in any column (dropping copies of articles takes
/// their bytes off a release's size, and its parts and completeness can change with them) and the key of every
/// copied row the copy left out. Returns whether the original had the outbox, to put the triggers back once the copy has its
/// indexes (`sync_triggers`).
pub fn carry_outbox(conn: &Connection) -> rusqlite::Result<bool> {
    let had: i64 = conn.query_row("select count(*) from old.sqlite_schema where name = ?1", [OUTBOX], |r| r.get(0))?;
    if had == 0 {
        return Ok(false);
    }
    // every column both copies have: dropping copies of articles changes size, parts and completeness, and
    // whatever else a compaction may come to rewrite is caught too
    let cols: Vec<String> = conn
        .prepare(
            "select m.name from pragma_table_info('releases', 'main') m
             join pragma_table_info('releases', 'old') o on o.name = m.name",
        )?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let changed = cols.iter().map(|c| format!("r.{0} is not o.{0}", quote(c))).collect::<Vec<_>>().join(" or ");
    conn.execute_batch(&format!(
        "{ddl}
         insert into main.{OUTBOX} (tbl, k1, k2, at) select tbl, k1, k2, at from old.{OUTBOX} order by seq;
         insert into main.{OUTBOX} (tbl, k1, k2)
         select 'releases', r.id, null from main.releases r join old.releases o on o.id = r.id
         where {changed};",
        ddl = outbox_ddl("main"),
    ))?;
    // and every row the copy left out (a collection whose release is gone, say): the publisher deletes it
    for t in tables(conn)? {
        let in_old: i64 = conn.query_row(
            "select count(*) from old.sqlite_schema where type = 'table' and name = ?1",
            [&t.name],
            |r| r.get(0),
        )?;
        if in_old == 0 {
            continue;
        }
        let k1 = quote(&t.key[0]);
        let k2 = t.key.get(1).map(|c| quote(c));
        let same = match &k2 {
            Some(k2) => format!("m.{k1} = o.{k1} and m.{k2} = o.{k2}"),
            None => format!("m.{k1} = o.{k1}"),
        };
        conn.execute_batch(&format!(
            "insert into main.{OUTBOX} (tbl, k1, k2)
             select '{name}', o.{k1}, {o2} from old.{qt} o
             where not exists (select 1 from main.{qt} m where {same});",
            name = t.name,
            qt = quote(&t.name),
            o2 = k2.as_ref().map_or("null".to_string(), |k2| format!("o.{k2}")),
        ))?;
    }
    Ok(true)
}

/// A Postgres connection. Unencrypted, so it's refused unless the host is this machine or a private network
/// address written as a number, or `allow_plaintext` says the network is trusted.
pub fn connect(config: &SharedCatalogConfig) -> anyhow::Result<postgres::Client> {
    let pg: postgres::Config = config.url.parse()?;
    if !config.allow_plaintext {
        for host in pg.get_hosts() {
            if let postgres::config::Host::Tcp(h) = host
                && !private_host(h)
            {
                anyhow::bail!(
                    "shared_catalog: {h} isn't a loopback or private network address; the connection isn't encrypted, set allow_plaintext = true only on a trusted network"
                );
            }
        }
    }
    Ok(pg.connect(postgres::NoTls)?)
}

fn private_host(h: &str) -> bool {
    if h.eq_ignore_ascii_case("localhost") {
        return true;
    }
    match h.trim_matches(|c| c == '[' || c == ']').parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(ip)) => ip.is_loopback() || ip.is_private() || ip.is_link_local(),
        Ok(std::net::IpAddr::V6(ip)) => ip.is_loopback() || (ip.segments()[0] & 0xfe00) == 0xfc00,
        Err(_) => false,
    }
}

/// One change waiting in the outbox.
#[derive(Clone, Debug, PartialEq)]
pub struct Change {
    pub seq: i64,
    pub tbl: String,
    pub k1: Value,
    pub k2: Value,
}

/// What one batch sends: per table, the rows still there and the keys of the rows gone.
#[derive(Debug, Default, PartialEq)]
pub struct Batch {
    pub max_seq: i64,
    pub changes: usize,
    /// stopped at the change limit or the byte budget: more is waiting
    pub full: bool,
    pub upserts: BTreeMap<String, Vec<Vec<Value>>>,
    pub deletes: BTreeMap<String, Vec<Vec<Value>>>,
}

impl Batch {
    pub fn is_empty(&self) -> bool {
        self.changes == 0
    }
}

/// The most row bytes one batch holds: rows can carry long texts (an NFO keeps up to 256 Ki characters), so a
/// backlog of them is sent in pieces rather than all at once.
pub const BATCH_BYTES: usize = 32 << 20;

/// Reads changes in order, up to `limit` of them or until the rows they point at come to `max_bytes`, and those
/// rows, in one read transaction so the rows are at least as new as every change read. `max_seq` is the last
/// change taken; a batch that stopped early says so in `full`.
pub fn read_batch(conn: &Connection, tables: &[Table], limit: usize, max_bytes: usize) -> rusqlite::Result<Batch> {
    let tx = conn.unchecked_transaction()?;
    let mut changes = Vec::new();
    {
        let mut stmt = tx.prepare_cached(&format!("select seq, tbl, k1, k2 from {OUTBOX} order by seq limit ?1"))?;
        let mut rows = stmt.query([limit as i64])?;
        while let Some(r) = rows.next()? {
            changes.push(Change { seq: r.get(0)?, tbl: r.get(1)?, k1: r.get(2)?, k2: r.get(3)? });
        }
    }
    let mut batch = Batch { full: changes.len() >= limit, ..Default::default() };
    // each key once: the row as it is now covers every change to it
    let mut seen: BTreeSet<(String, KeyPart, KeyPart)> = BTreeSet::new();
    let mut bytes = 0usize;
    for c in &changes {
        if bytes >= max_bytes {
            batch.full = true;
            break;
        }
        batch.changes += 1;
        batch.max_seq = c.seq;
        if !seen.insert((c.tbl.clone(), KeyPart::of(&c.k1), KeyPart::of(&c.k2))) {
            continue;
        }
        let Some(t) = tables.iter().find(|t| t.name == c.tbl) else { continue };
        let mut stmt = tx.prepare_cached(&t.select_sql())?;
        let found: Option<Vec<Value>> = {
            let mut rows = if t.key.len() == 2 { stmt.query(params![c.k1, c.k2])? } else { stmt.query(params![c.k1])? };
            match rows.next()? {
                Some(r) => Some((0..t.cols.len()).map(|i| r.get::<_, Value>(i)).collect::<rusqlite::Result<_>>()?),
                None => None,
            }
        };
        match found {
            Some(row) => {
                bytes += row.iter().map(value_bytes).sum::<usize>();
                batch.upserts.entry(t.name.clone()).or_default().push(row);
            }
            None => {
                let mut key = vec![c.k1.clone()];
                if t.key.len() == 2 {
                    key.push(c.k2.clone());
                }
                batch.deletes.entry(t.name.clone()).or_default().push(key);
            }
        }
    }
    tx.commit()?;
    Ok(batch)
}

fn value_bytes(v: &Value) -> usize {
    match v {
        Value::Text(s) => s.len(),
        Value::Blob(b) => b.len(),
        _ => 8,
    }
}

/// An outbox key, ordered and deduplicated whatever its type.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum KeyPart {
    Null,
    Int(i64),
    Real(u64),
    Text(String),
    Blob(Vec<u8>),
}

impl KeyPart {
    fn of(v: &Value) -> KeyPart {
        match v {
            Value::Null => KeyPart::Null,
            Value::Integer(i) => KeyPart::Int(*i),
            Value::Real(f) => KeyPart::Real(f.to_bits()),
            Value::Text(s) => KeyPart::Text(s.clone()),
            Value::Blob(b) => KeyPart::Blob(b.clone()),
        }
    }
}

/// A SQLite value as the Postgres column's type. SQLite lets a column hold any type, so a value is converted where
/// it can be and sent as null where it can't (a text that isn't a number, in a number column).
pub fn pg_value(v: &Value, t: PgType) -> Box<dyn postgres::types::ToSql + Sync + Send> {
    match t {
        PgType::Int => Box::new(match v {
            Value::Integer(i) => Some(*i),
            Value::Real(f) => Some(*f as i64),
            Value::Text(s) => s.trim().parse::<i64>().ok(),
            Value::Null | Value::Blob(_) => None,
        }),
        PgType::Real => Box::new(match v {
            Value::Integer(i) => Some(*i as f64),
            Value::Real(f) => Some(*f),
            Value::Text(s) => s.trim().parse::<f64>().ok(),
            Value::Null | Value::Blob(_) => None,
        }),
        PgType::Text => Box::new(match v {
            Value::Integer(i) => Some(i.to_string()),
            Value::Real(f) => Some(f.to_string()),
            // Postgres text can't hold a NUL
            Value::Text(s) => Some(s.replace('\0', "")),
            Value::Blob(b) => Some(String::from_utf8_lossy(b).replace('\0', "")),
            Value::Null => None,
        }),
        PgType::Bytes => Box::new(match v {
            Value::Integer(i) => Some(i.to_string().into_bytes()),
            Value::Real(f) => Some(f.to_string().into_bytes()),
            Value::Text(s) => Some(s.clone().into_bytes()),
            Value::Blob(b) => Some(b.clone()),
            Value::Null => None,
        }),
    }
}

/// The most rows one insert carries: Postgres takes at most 65535 parameters a statement.
fn rows_per_statement(width: usize) -> usize {
    (60_000 / width.max(1)).clamp(1, 500)
}

/// Writes one batch in one Postgres transaction.
pub fn send(
    pg: &mut postgres::Client,
    shard: usize,
    tables: &[Table],
    batch: &Batch,
) -> Result<(u64, u64), postgres::Error> {
    let shard_v: i16 = shard as i16;
    let mut tx = pg.transaction()?;
    lock_shard(&mut tx, shard)?;
    let (mut up, mut del) = (0u64, 0u64);
    for (name, rows) in &batch.upserts {
        let Some(t) = tables.iter().find(|t| &t.name == name) else { continue };
        for chunk in rows.chunks(rows_per_statement(t.cols.len() + 1)) {
            let sql = t.upsert_sql(chunk.len());
            let mut ps: Vec<Box<dyn postgres::types::ToSql + Sync + Send>> =
                Vec::with_capacity(chunk.len() * (t.cols.len() + 1));
            for row in chunk {
                ps.push(Box::new(shard_v));
                for (v, (_, ty)) in row.iter().zip(&t.cols) {
                    ps.push(pg_value(v, *ty));
                }
            }
            let refs: Vec<&(dyn postgres::types::ToSql + Sync)> = ps.iter().map(|p| &**p as _).collect();
            tx.execute(sql.as_str(), &refs)?;
            up += chunk.len() as u64;
        }
    }
    for (name, keys) in &batch.deletes {
        let Some(t) = tables.iter().find(|t| &t.name == name) else { continue };
        let stmt = tx.prepare(&t.delete_sql())?;
        for key in keys {
            let mut ps: Vec<Box<dyn postgres::types::ToSql + Sync + Send>> = vec![Box::new(shard_v)];
            for (i, v) in key.iter().enumerate() {
                ps.push(pg_value(v, t.key_type(i)));
            }
            let refs: Vec<&(dyn postgres::types::ToSql + Sync)> = ps.iter().map(|p| &**p as _).collect();
            del += tx.execute(&stmt, &refs)?;
        }
    }
    tx.commit()?;
    Ok((up, del))
}

/// The first half of the advisory lock key: a shard's writes to Postgres hold the lock (this, shard) for their
/// transaction, soo a fill that reads the shard while holding it can't be overtaken by an older batch.
const LOCK_KEY: i32 = 0x4174_6c61;

/// Holds the shard's advisory lock until the transaction ends.
pub fn lock_shard(tx: &mut postgres::Transaction<'_>, shard: usize) -> Result<(), postgres::Error> {
    tx.execute("select pg_advisory_xact_lock($1, $2)", &[&LOCK_KEY, &(shard as i32)])?;
    Ok(())
}

/// Takes a sent batch out of the outbox.
pub fn ack(conn: &Connection, max_seq: i64) -> rusqlite::Result<usize> {
    conn.execute(&format!("delete from {OUTBOX} where seq <= ?1"), [max_seq])
}

/// Makes or widens every copied table in Postgres.
pub fn ensure_schema(pg: &mut postgres::Client, tables: &[Table]) -> Result<(), postgres::Error> {
    for t in tables {
        for sql in t.ddl() {
            pg.batch_execute(&sql)?;
        }
    }
    Ok(())
}

/// Each shard's waiting changes and how long the oldest has waited, from the ends of the outbox only: a batch takes
/// out a run of the oldest seqs, soo what's left is every seq from the first to the last.
fn outbox_gauges(shard: usize, conn: &Connection) {
    let (first, last): (Option<i64>, Option<i64>) = conn
        .query_row(&format!("select min(seq), max(seq) from {OUTBOX}"), [], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap_or((None, None));
    let oldest: Option<i64> = first
        .and_then(|seq| conn.query_row(&format!("select at from {OUTBOX} where seq = ?1"), [seq], |r| r.get(0)).ok());
    let rows = match (first, last) {
        (Some(a), Some(b)) => b - a + 1,
        _ => 0,
    };
    let s = shard.to_string();
    names::SHARED_CATALOG_OUTBOX.set(&[("shard", &s), ("kind", "rows")], rows as f64);
    let age = oldest.map_or(0.0, |at| (chrono::Utc::now().timestamp() - at).max(0) as f64);
    names::SHARED_CATALOG_OUTBOX.set(&[("shard", &s), ("kind", "oldest_seconds")], age);
}

/// Puts the triggers in every shard or takes them out, then starts the publisher when it's on. The indexer calls
/// this before its writers start.
pub fn start(config: &SharedCatalogConfig, stop: Arc<AtomicBool>) {
    let main = crate::paths::database();
    let on = config.is_on();
    for path in store::shard_paths(&main) {
        if !path.exists() {
            continue;
        }
        let result = crate::db::open_shard(&path).and_then(|conn| sync_triggers(&conn, on));
        if let Err(e) = result {
            eprintln!("shared catalog: triggers in {}: {e:#}", path.display());
        }
    }
    if !on {
        return;
    }
    let config = config.clone();
    let _ = std::thread::Builder::new().name("shared-catalog".into()).spawn(move || run(&main, &config, &stop));
}

/// The publisher: a batch from each shard in turn, resting when every outbox is empty, reconnecting after a failure.
pub fn run(main: &Path, config: &SharedCatalogConfig, stop: &AtomicBool) {
    let shards: Vec<(usize, PathBuf)> =
        store::shard_paths(main).into_iter().enumerate().filter(|(_, p)| p.exists()).collect();
    let mut backoff = Duration::from_secs(1);
    while !stop.load(Relaxed) {
        let mut pg = match connect(config) {
            Ok(pg) => pg,
            Err(e) => {
                names::SHARED_CATALOG_FAILURES.inc(&[("stage", "connect")], 1);
                eprintln!("shared catalog: connect: {e}");
                rest(stop, backoff);
                backoff = (backoff * 2).min(Duration::from_secs(60));
                continue;
            }
        };
        match publish(&mut pg, main, &shards, config, stop) {
            Ok(()) => return,
            Err(e) => {
                names::SHARED_CATALOG_FAILURES.inc(&[("stage", "publish")], 1);
                eprintln!("shared catalog: {e:#}");
                rest(stop, backoff);
                backoff = (backoff * 2).min(Duration::from_secs(60));
            }
        }
    }
}

/// How long the publisher keeps the shards open while it has a backlog before it lets go, soo a compaction waiting
/// for them gets its turn.
const SESSION: Duration = Duration::from_secs(60);

/// The shards open, and the hold on compaction that keeps them from being swapped meanwhile. A compaction can't
/// rename a shard someone has open on Windows, and on Unix an open connection would go on reading the original;
/// soo the publisher opens them only while compaction is held off, and lets go when it has nothing to send or after
/// `SESSION`.
struct Session {
    // dropped before the hold
    open: Vec<(usize, Connection, Vec<Table>)>,
    _hold: crate::compact::WriteGuard,
    since: std::time::Instant,
}

fn publish(
    pg: &mut postgres::Client,
    main: &Path,
    shards: &[(usize, PathBuf)],
    config: &SharedCatalogConfig,
    stop: &AtomicBool,
) -> anyhow::Result<()> {
    // the schema each shard last made sure of in Postgres: made again only when a shard's tables change
    let mut ensured: BTreeMap<usize, Vec<Table>> = BTreeMap::new();
    let mut gauges_at: Option<std::time::Instant> = None;
    let mut session: Option<Session> = None;
    while !stop.load(Relaxed) {
        if session.is_none() {
            let Some(hold) = crate::compact::try_hold_off_compaction(main)? else {
                // a compaction runs: wait for it
                rest(stop, Duration::from_millis(config.interval_ms));
                continue;
            };
            let mut open = Vec::new();
            for (shard, path) in shards {
                let conn = crate::db::open_shard(path)?;
                let tables = tables(&conn)?;
                if ensured.get(shard) != Some(&tables) {
                    ensure_schema(pg, &tables)?;
                    ensured.insert(*shard, tables.clone());
                }
                open.push((*shard, conn, tables));
            }
            session = Some(Session { open, _hold: hold, since: std::time::Instant::now() });
        }
        let Some(open) = session.as_ref().map(|s| &s.open) else { continue };
        let mut busy = false;
        for (shard, conn, tables) in open {
            let batch = read_batch(conn, tables, config.batch, BATCH_BYTES)?;
            if !batch.is_empty() {
                let (up, del) = crate::metrics::timed("shared_catalog.send", || send(pg, *shard, tables, &batch))?;
                ack(conn, batch.max_seq)?;
                names::SHARED_CATALOG_ROWS.inc(&[("op", "upsert")], up);
                names::SHARED_CATALOG_ROWS.inc(&[("op", "delete")], del);
                busy |= batch.full;
            }
        }
        if gauges_at.is_none_or(|at| at.elapsed() >= Duration::from_secs(30)) {
            for (shard, conn, _) in open {
                outbox_gauges(*shard, conn);
            }
            gauges_at = Some(std::time::Instant::now());
        }
        if !busy || session.as_ref().is_some_and(|s| s.since.elapsed() >= SESSION) {
            session = None;
        }
        if !busy {
            rest(stop, Duration::from_millis(config.interval_ms));
        }
    }
    Ok(())
}

fn rest(stop: &AtomicBool, d: Duration) {
    let step = Duration::from_millis(100);
    let mut left = d;
    while !stop.load(Relaxed) && !left.is_zero() {
        let s = left.min(step);
        std::thread::sleep(s);
        left -= s;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// the Postgres tests share one test database's tables
    static PG: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn shard() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "create table releases (id INTEGER PRIMARY KEY, name TEXT, size INTEGER, posted_date TEXT);
             create table collections (key INTEGER PRIMARY KEY, release_id INTEGER NOT NULL) without rowid;
             create table par2_files (release_id INTEGER NOT NULL, file_id BLOB NOT NULL, name TEXT NOT NULL,
                primary key (release_id, file_id)) without rowid;
             create table nzb_pointed (release_id INTEGER PRIMARY KEY);
             create table files (id INTEGER PRIMARY KEY, release_id INTEGER, filename TEXT);",
        )
        .unwrap();
        conn
    }

    #[test]
    fn only_the_catalog_tables_a_shard_has_are_copied() {
        let conn = shard();
        let names: Vec<String> = tables(&conn).unwrap().into_iter().map(|t| t.name).collect();
        assert_eq!(names, ["releases", "par2_files", "collections", "nzb_pointed"]);
        let par2 = tables(&conn).unwrap().into_iter().find(|t| t.name == "par2_files").unwrap();
        assert_eq!(par2.key, ["release_id", "file_id"]);
        assert_eq!(par2.key_type(1), PgType::Bytes);
    }

    #[test]
    fn every_change_lands_in_the_outbox_and_a_batch_sends_each_row_once_as_it_is_now() {
        let conn = shard();
        sync_triggers(&conn, true).unwrap();
        sync_triggers(&conn, true).unwrap(); // again: nothing new
        conn.execute_batch(
            "insert into releases (id, name, size) values (8, 'a', 1), (16, 'b', 2);
             update releases set size = 5 where id = 8;
             update releases set size = 6 where id = 8;
             insert into collections (key, release_id) values (-3, 8);
             insert into par2_files values (8, x'0102', 'a.par2');
             delete from releases where id = 16;
             update releases set id = 24 where id = 8;
             insert into files (release_id, filename) values (8, 'not copied');",
        )
        .unwrap();
        let outbox: i64 = conn.query_row("select count(*) from catalog_outbox", [], |r| r.get(0)).unwrap();
        assert_eq!(outbox, 9, "files is not copied");
        let batch = read_batch(&conn, &tables(&conn).unwrap(), 100, BATCH_BYTES).unwrap();
        assert_eq!(batch.changes, 9);
        assert_eq!(
            batch.upserts["releases"],
            vec![vec![Value::Integer(24), Value::Text("a".into()), Value::Integer(6), Value::Null]],
            "the row as it is now, once"
        );
        assert_eq!(batch.deletes["releases"], vec![vec![Value::Integer(8)], vec![Value::Integer(16)]]);
        assert_eq!(batch.upserts["par2_files"][0][1], Value::Blob(vec![1, 2]));
        assert_eq!(batch.upserts["collections"].len(), 1);
        assert_eq!(ack(&conn, batch.max_seq).unwrap(), 9);
        assert!(read_batch(&conn, &tables(&conn).unwrap(), 100, BATCH_BYTES).unwrap().is_empty());
    }

    #[test]
    fn a_batch_stops_at_its_limit_and_the_rest_waits() {
        let conn = shard();
        sync_triggers(&conn, true).unwrap();
        for i in 0..5 {
            conn.execute("insert into releases (id, name) values (?1, 'x')", [i * 8]).unwrap();
        }
        let t = tables(&conn).unwrap();
        let first = read_batch(&conn, &t, 3, BATCH_BYTES).unwrap();
        assert_eq!((first.changes, first.upserts["releases"].len()), (3, 3));
        ack(&conn, first.max_seq).unwrap();
        assert_eq!(read_batch(&conn, &t, 3, BATCH_BYTES).unwrap().changes, 2);
    }

    #[test]
    fn a_batch_of_long_rows_stops_at_its_byte_budget_and_the_rest_waits() {
        let conn = shard();
        sync_triggers(&conn, true).unwrap();
        let long = "x".repeat(1000);
        for i in 1..=10 {
            conn.execute("insert into releases (id, name) values (?1, ?2)", params![i * 8, long]).unwrap();
        }
        let t = tables(&conn).unwrap();
        let first = read_batch(&conn, &t, 100, 2500).unwrap();
        assert_eq!((first.changes, first.upserts["releases"].len(), first.full), (3, 3, true));
        ack(&conn, first.max_seq).unwrap();
        let rest = read_batch(&conn, &t, 100, usize::MAX).unwrap();
        assert_eq!((rest.changes, rest.full), (7, false));
    }

    #[test]
    fn turned_off_the_triggers_and_outbox_go() {
        let conn = shard();
        sync_triggers(&conn, true).unwrap();
        sync_triggers(&conn, false).unwrap();
        let left: i64 =
            conn.query_row("select count(*) from sqlite_schema where name like 'catalog%'", [], |r| r.get(0)).unwrap();
        assert_eq!(left, 0);
        conn.execute("insert into releases (id, name) values (8, 'x')", []).unwrap();
        sync_triggers(&conn, false).unwrap(); // nothing there: fine
    }

    #[test]
    fn postgres_statements_key_shard_local_tables_by_shard() {
        let conn = shard();
        let t = tables(&conn).unwrap();
        let coll = t.iter().find(|t| t.name == "collections").unwrap();
        assert!(coll.ddl()[0].ends_with("primary key (shard, \"key\"))"), "{}", coll.ddl()[0]);
        assert_eq!(
            coll.upsert_sql(2),
            "insert into \"collections\" (shard, \"key\", \"release_id\") values ($1, $2, $3), ($4, $5, $6) \
             on conflict (shard, \"key\") do update set \"release_id\" = excluded.\"release_id\""
        );
        let rel = t.iter().find(|t| t.name == "releases").unwrap();
        assert!(rel.ddl()[0].ends_with("primary key (\"id\"))"));
        assert!(rel.upsert_sql(1).ends_with("do update set shard = excluded.shard, \"name\" = excluded.\"name\", \"size\" = excluded.\"size\", \"posted_date\" = excluded.\"posted_date\""));
        assert_eq!(rel.delete_sql(), "delete from \"releases\" where shard = $1 and \"id\" = $2");
        let nzb = t.iter().find(|t| t.name == "nzb_pointed").unwrap();
        assert!(nzb.upsert_sql(1).ends_with("do update set shard = excluded.shard"));
    }

    #[test]
    fn sqlite_values_become_the_column_type_or_null() {
        let as_dbg = |v: &Value, t| format!("{:?}", pg_value(v, t));
        assert_eq!(as_dbg(&Value::Text("12".into()), PgType::Int), "Some(12)");
        assert_eq!(as_dbg(&Value::Text("x".into()), PgType::Int), "None");
        assert_eq!(as_dbg(&Value::Integer(7), PgType::Text), "Some(\"7\")");
        assert_eq!(as_dbg(&Value::Text("a\0b".into()), PgType::Text), "Some(\"ab\")");
        assert_eq!(as_dbg(&Value::Null, PgType::Bytes), "None");
        assert_eq!(rows_per_statement(13), 500);
        assert_eq!(rows_per_statement(300), 200);
    }

    #[test]
    fn plaintext_only_goes_to_this_machine_or_a_private_address_unless_allowed() {
        for h in ["localhost", "127.0.0.1", "192.168.1.93", "10.0.0.2", "172.16.4.1", "::1", "[fd00::5]"] {
            assert!(private_host(h), "{h}");
        }
        for h in ["db.example", "8.8.8.8", "2001:db8::1", "server"] {
            assert!(!private_host(h), "{h}");
        }
        let mut c = SharedCatalogConfig {
            enabled: true,
            url: "host=db.example port=1 user=a dbname=b connect_timeout=1".into(),
            ..Default::default()
        };
        let err = connect(&c).err().unwrap().to_string();
        assert!(err.contains("isn't a loopback or private network address"), "{err}");
        c.allow_plaintext = true;
        let err = connect(&c).err().unwrap().to_string();
        assert!(!err.contains("private network"), "allowed: it tries, and fails to reach it: {err}");
    }

    #[test]
    fn a_compaction_carries_the_waiting_changes_and_the_releases_it_resized() {
        let conn = shard();
        sync_triggers(&conn, true).unwrap();
        conn.execute_batch(
            "insert into releases (id, name, size) values (8, 'a', 10), (16, 'b', 20);
             insert into collections values (-3, 99);",
        )
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let copy = dir.path().join("copy.db");
        let copy_conn = Connection::open(&copy).unwrap();
        copy_conn
            .execute_batch(
                "create table releases (id INTEGER PRIMARY KEY, name TEXT, size INTEGER, posted_date TEXT);
                 create table collections (key INTEGER PRIMARY KEY, release_id INTEGER NOT NULL) without rowid;
                 insert into releases (id, name, size, posted_date) values (8, 'a', 7, null), (16, 'b', 20, 'x');",
            )
            .unwrap();
        let orig = dir.path().join("orig.db");
        conn.execute("vacuum into ?1", [orig.to_string_lossy()]).unwrap();
        copy_conn.execute("attach database ?1 as old", [orig.to_string_lossy()]).unwrap();
        assert!(carry_outbox(&copy_conn).unwrap());
        let carried: Vec<(String, i64)> = copy_conn
            .prepare("select tbl, k1 from catalog_outbox order by seq")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(
            carried,
            [
                ("releases".into(), 8),
                ("releases".into(), 16),
                ("collections".into(), -3),
                ("releases".into(), 8),
                ("releases".into(), 16),
                ("collections".into(), -3),
            ],
            "8 for its size, 16 for another column with the size unchanged, -3 left out of the copy"
        );
        copy_conn.execute_batch("detach database old").unwrap();
        // without one in the original, nothing to carry
        let plain = dir.path().join("plain.db");
        Connection::open(&plain)
            .unwrap()
            .execute_batch("create table releases (id INTEGER PRIMARY KEY, size INTEGER)")
            .unwrap();
        copy_conn.execute("attach database ?1 as old", [plain.to_string_lossy()]).unwrap();
        assert!(!carry_outbox(&copy_conn).unwrap());
    }

    /// Against a real Postgres when `ATLAS_CATALOG_TEST_URL` names an empty database to use: the publisher sends
    /// what a real shard's triggers recorded, and waits while a compaction holds the database.
    #[test]
    fn the_publisher_sends_a_shards_changes_and_waits_out_a_compaction() {
        let Ok(url) = std::env::var("ATLAS_CATALOG_TEST_URL") else { return };
        let _one = PG.lock().unwrap_or_else(|e| e.into_inner());
        let mut pg = postgres::Client::connect(&url, postgres::NoTls).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        crate::db::create_db_at(&main).unwrap();
        let path = store::shard_path(&main, 5);
        let conn = crate::db::open_at(&path).unwrap();
        let names: Vec<String> = tables(&conn).unwrap().into_iter().map(|t| format!("\"{}\"", t.name)).collect();
        pg.batch_execute(&format!("drop table if exists {}", names.join(", "))).unwrap();
        sync_triggers(&conn, true).unwrap();
        conn.execute("insert into releases (id, name, group_name) values (13, 'a', 'g')", []).unwrap();
        let config = SharedCatalogConfig { enabled: true, url: url.clone(), interval_ms: 50, ..Default::default() };
        let stop = Arc::new(AtomicBool::new(false));
        let shards = vec![(5usize, path.clone())];
        let queued: i64 = conn.query_row("select count(*) from catalog_outbox", [], |r| r.get(0)).unwrap();
        assert!(queued >= 1);
        let held = crate::compact::Lock::take(&main).unwrap();
        let publisher = {
            let (main, config, stop) = (main.clone(), config.clone(), stop.clone());
            std::thread::spawn(move || {
                let mut pg = connect(&config).unwrap();
                publish(&mut pg, &main, &shards, &config, &stop).unwrap();
            })
        };
        std::thread::sleep(Duration::from_millis(500));
        let waiting: i64 = conn.query_row("select count(*) from catalog_outbox", [], |r| r.get(0)).unwrap();
        assert_eq!(waiting, queued, "nothing sent while a compaction holds the database");
        drop(held);
        let mut sent = 0i64;
        for _ in 0..100 {
            std::thread::sleep(Duration::from_millis(100));
            sent = pg.query_one("select count(*) from releases where id = 13 and shard = 5", &[]).unwrap().get(0);
            if sent == 1 {
                break;
            }
        }
        stop.store(true, Relaxed);
        publisher.join().unwrap();
        assert_eq!(sent, 1);
        let waiting: i64 = conn.query_row("select count(*) from catalog_outbox", [], |r| r.get(0)).unwrap();
        assert_eq!(waiting, 0);
        pg.batch_execute(&format!("drop table if exists {}", names.join(", "))).unwrap();
    }

    /// Against a real Postgres when `ATLAS_CATALOG_TEST_URL` names an empty database to use.
    #[test]
    fn a_batch_lands_in_postgres_and_lands_the_same_when_sent_twice() {
        let Ok(url) = std::env::var("ATLAS_CATALOG_TEST_URL") else { return };
        let _one = PG.lock().unwrap_or_else(|e| e.into_inner());
        let mut pg = postgres::Client::connect(&url, postgres::NoTls).unwrap();
        pg.batch_execute("drop table if exists releases, collections, par2_files, nzb_pointed").unwrap();
        let conn = shard();
        sync_triggers(&conn, true).unwrap();
        let t = tables(&conn).unwrap();
        ensure_schema(&mut pg, &t).unwrap();
        conn.execute_batch(
            "insert into releases (id, name, size, posted_date) values (8, 'a', 1, '2026-11-06'), (16, 'b', '7', null);
             insert into collections values (-3, 8);
             insert into par2_files values (8, x'00ff', 'a.par2');
             insert into nzb_pointed values (8);",
        )
        .unwrap();
        let batch = read_batch(&conn, &t, 100, BATCH_BYTES).unwrap();
        assert_eq!(send(&mut pg, 3, &t, &batch).unwrap(), (5, 0));
        assert_eq!(send(&mut pg, 3, &t, &batch).unwrap(), (5, 0), "sent again after a crash: same rows");
        ack(&conn, batch.max_seq).unwrap();
        conn.execute_batch("delete from releases where id = 16; update releases set size = 9 where id = 8;").unwrap();
        let batch = read_batch(&conn, &t, 100, BATCH_BYTES).unwrap();
        assert_eq!(send(&mut pg, 3, &t, &batch).unwrap(), (1, 1));
        let row = pg.query_one("select shard, name, size from releases", &[]).unwrap();
        assert_eq!((row.get::<_, i16>(0), row.get::<_, String>(1), row.get::<_, i64>(2)), (3, "a".into(), 9));
        let blob: Vec<u8> = pg.query_one("select file_id from par2_files", &[]).unwrap().get(0);
        assert_eq!(blob, vec![0, 255]);
        let n: i64 = pg.query_one("select count(*) from collections where shard = 3", &[]).unwrap().get(0);
        assert_eq!(n, 1);
        // a shard migration's new column reaches Postgres
        conn.execute_batch("alter table releases add column name_source INTEGER").unwrap();
        ensure_schema(&mut pg, &tables(&conn).unwrap()).unwrap();
        pg.query_one("select name_source from releases", &[]).unwrap();
        pg.batch_execute("drop table releases, collections, par2_files, nzb_pointed").unwrap();
    }
}
