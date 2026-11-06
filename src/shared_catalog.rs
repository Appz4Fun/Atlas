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

/// Every table copied: what a lookup across groups or posts compares. Per-file article data stays in the shards, and
/// so do the queues that only say what's waiting (`freeze_pending`, `pp_queue`): they churn all day and nothing
/// looks across groups in them.
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
    "freeze_exempt",
    "nzb_pointed",
    "pp_state",
    "pp_walks",
    "sidecars",
];

/// Tables whose key only means something inside its shard: collection keys are hashed with the shard's own secret
/// (`store::IdKey`), walk ids count per shard. Their copies are keyed by the shard too.
const SHARD_KEYED: &[&str] = &["collections", "junk_collections", "pp_walks"];

pub const OUTBOX: &str = "catalog_outbox";

/// The shard's `meta` key naming its outbox, made with it (`sync_triggers`).
const EPOCH: &str = "catalog_epoch";

/// The shard's outbox epoch, None while the catalog is off.
pub fn epoch(conn: &Connection) -> rusqlite::Result<Option<i64>> {
    use rusqlite::OptionalExtension;
    let has_meta: i64 =
        conn.query_row("select count(*) from main.sqlite_schema where type = 'table' and name = 'meta'", [], |r| {
            r.get(0)
        })?;
    if has_meta == 0 {
        return Ok(None);
    }
    conn.query_row("select value from main.meta where key = ?1", [EPOCH], |r| r.get(0)).optional()
}

/// Next to the shard, how far Postgres has taken its outbox (`epoch seq`): the shard's writer prunes up to it
/// (`prune_acked`), soo the publisher never has to wait for the shard's write lock.
fn cursor_path(shard: &Path) -> PathBuf {
    let mut p = shard.as_os_str().to_owned();
    p.push(".catalog-cursor");
    PathBuf::from(p)
}

fn write_cursor(shard: &Path, epoch: i64, seq: i64) -> std::io::Result<()> {
    let path = cursor_path(shard);
    let tmp = path.with_extension("catalog-cursor.tmp");
    std::fs::write(&tmp, format!("{epoch} {seq}\n"))?;
    std::fs::rename(tmp, path)
}

fn read_cursor(shard: &Path) -> Option<(i64, i64)> {
    let text = std::fs::read_to_string(cursor_path(shard)).ok()?;
    let mut parts = text.split_whitespace().map(|p| p.parse::<i64>().ok());
    Some((parts.next()??, parts.next()??))
}

/// The most sent changes one housekeeping tick takes out of the outbox.
const PRUNE_PER_TICK: i64 = 50_000;

/// For the shard's writer, between its transactions: takes out of the outbox what Postgres has, as far as the
/// cursor file next to the shard says, if it's about this outbox. Nothing when the catalog is off.
pub fn prune_acked(conn: &Connection) -> rusqlite::Result<usize> {
    let Some(shard) = conn.path().filter(|p| !p.is_empty()).map(PathBuf::from) else { return Ok(0) };
    let Some((epoch_sent, seq)) = read_cursor(&shard) else { return Ok(0) };
    if epoch(conn)? != Some(epoch_sent) {
        return Ok(0);
    }
    conn.execute(
        &format!("delete from {OUTBOX} where seq in (select seq from {OUTBOX} where seq <= ?1 order by seq limit ?2)"),
        params![seq, PRUNE_PER_TICK],
    )
}

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

    /// `delete` of many rows this shard sent, their keys as arrays: `$2` (and `$3`) one per key column.
    pub fn delete_many_sql(&self) -> String {
        let key: Vec<String> = self.key.iter().map(|c| quote(c)).collect();
        let types: Vec<&str> = (0..self.key.len()).map(|i| self.key_type(i).sql()).collect();
        let arrays: Vec<String> = types.iter().enumerate().map(|(i, t)| format!("${}::{t}[]", i + 2)).collect();
        format!(
            "delete from {} where shard = $1 and ({}) in (select * from unnest({}))",
            quote(&self.name),
            key.join(", "),
            arrays.join(", ")
        )
    }

    fn select_sql(&self) -> String {
        let cols: Vec<String> = self.cols.iter().map(|(c, _)| quote(c)).collect();
        let wh: Vec<String> = self.key.iter().enumerate().map(|(i, c)| format!("{} = ?{}", quote(c), i + 1)).collect();
        format!("select {} from {} where {}", cols.join(", "), quote(&self.name), wh.join(" and "))
    }

    /// A page of `?1` rows in key order: the first page, or (`first` false) the rows after the key `?2` (and `?3`)
    /// that ended the page before. Two statements, so SQLite seeks to the key instead of scanning up to it.
    fn page_sql(&self, first: bool) -> String {
        let cols: Vec<String> = self.cols.iter().map(|(c, _)| quote(c)).collect();
        let key: Vec<String> = self.key.iter().map(|c| quote(c)).collect();
        let after = match (first, key.len()) {
            (true, _) => String::new(),
            (false, 2) => format!(" where ({}, {}) > (?2, ?3)", key[0], key[1]),
            (false, _) => format!(" where {} > ?2", key[0]),
        };
        format!("select {} from {}{after} order by {} limit ?1", cols.join(", "), quote(&self.name), key.join(", "))
    }

    /// Where a key is among the columns.
    fn key_at(&self) -> Vec<usize> {
        self.key.iter().map(|k| self.cols.iter().position(|(c, _)| c == k).unwrap_or(0)).collect()
    }

    /// Postgres: the next `$3` keys this shard sent after `$4` (and `$5`), in key order.
    fn pg_keys_sql(&self, first: bool) -> String {
        let key: Vec<String> = self.key.iter().map(|c| quote(c)).collect();
        let after = match (first, key.len()) {
            (true, _) => String::new(),
            (false, 2) => format!(" and ({}, {}) > ($3, $4)", key[0], key[1]),
            (false, _) => format!(" and {} > $3", key[0]),
        };
        format!(
            "select {} from {} where shard = $1{after} order by {} limit $2",
            key.join(", "),
            quote(&self.name),
            key.join(", ")
        )
    }

    /// Postgres: `select ... for update` of one row this shard sent, by its key.
    fn lock_sql(&self) -> String {
        self.delete_sql().replacen("delete from", "select 1 from", 1) + " for update"
    }

    fn exists_sql(&self) -> String {
        let wh: Vec<String> = self.key.iter().enumerate().map(|(i, c)| format!("{} = ?{}", quote(c), i + 1)).collect();
        format!("select 1 from {} where {}", quote(&self.name), wh.join(" and "))
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
        // a table no longer copied loses its triggers; the rest are made again
        let mut sql = outbox_ddl("main");
        // which outbox this is: its seqs start again when it's made again, soo a position saved for an earlier one
        // means nothing
        sql.push_str(&format!(
            "create table if not exists meta (key TEXT PRIMARY KEY, value INTEGER);
             insert or ignore into meta (key, value) values ('{EPOCH}', abs(random()) % 9007199254740991 + 1);"
        ));
        sql.push_str(&drop_catalog_triggers(conn)?);
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
        // every catalog trigger, a retired table's too: left behind, it would write to the outbox about to go
        let mut sql = drop_catalog_triggers(conn)?;
        sql.push_str(&format!(
            "drop table if exists {OUTBOX};
             create table if not exists meta (key TEXT PRIMARY KEY, value INTEGER);
             delete from meta where key = '{EPOCH}';"
        ));
        conn.execute_batch(&format!("begin; {sql} commit;"))
    }
}

/// `drop trigger` for every catalog trigger the shard has, whatever table it's on.
fn drop_catalog_triggers(conn: &Connection) -> rusqlite::Result<String> {
    let names: Vec<String> = conn
        .prepare("select name from main.sqlite_schema where type = 'trigger' and name like 'catalog\\_%' escape '\\'")?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(names.iter().map(|n| format!("drop trigger if exists {};", quote(n))).collect())
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
/// their bytes off a release's size, and its parts and completeness can change with them), and the key of every
/// copied row the copy left out or added. Returns whether the original had the outbox, to put the triggers back once the copy has its
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
         insert into main.{OUTBOX} (seq, tbl, k1, k2, at) select seq, tbl, k1, k2, at from old.{OUTBOX} order by seq;
         insert into main.{OUTBOX} (tbl, k1, k2)
         select 'releases', r.id, null from main.releases r join old.releases o on o.id = r.id
         where {changed};",
        ddl = outbox_ddl("main"),
    ))?;
    // and every row the copy left out (a collection whose release is gone, say), which the publisher deletes, and
    // every row the copy added (a release put on the uncategorized list), which it sends
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
             where not exists (select 1 from main.{qt} m where {same});
             insert into main.{OUTBOX} (tbl, k1, k2)
             select '{name}', m.{k1}, {m2} from main.{qt} m
             where not exists (select 1 from old.{qt} o where {same});",
            name = t.name,
            qt = quote(&t.name),
            o2 = k2.as_ref().map_or("null".to_string(), |k2| format!("o.{k2}")),
            m2 = k2.as_ref().map_or("null".to_string(), |k2| format!("m.{k2}")),
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
    read_batch_after(conn, tables, limit, max_bytes, 0)
}

/// `read_batch` of the changes after seq `after`, the ones Postgres hasn't taken yet.
pub fn read_batch_after(
    conn: &Connection,
    tables: &[Table],
    limit: usize,
    max_bytes: usize,
    after: i64,
) -> rusqlite::Result<Batch> {
    let tx = conn.unchecked_transaction()?;
    let mut changes = Vec::new();
    {
        let mut stmt =
            tx.prepare_cached(&format!("select seq, tbl, k1, k2 from {OUTBOX} where seq > ?2 order by seq limit ?1"))?;
        let mut rows = stmt.query(params![limit as i64, after])?;
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
    send_with_cursor(pg, shard, tables, batch, None)
}

/// `send`, also moving the shard's position in `catalog_cursor` to the batch's last change (`epoch`, seq) in the
/// same transaction: what's past it is what's left to send.
pub fn send_with_cursor(
    pg: &mut postgres::Client,
    shard: usize,
    tables: &[Table],
    batch: &Batch,
    cursor: Option<(i64, i64)>,
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
        let sql = t.delete_many_sql();
        for chunk in keys.chunks(DELETES_PER_STATEMENT) {
            let mut ps: Vec<Box<dyn postgres::types::ToSql + Sync + Send>> = vec![Box::new(shard_v)];
            for i in 0..t.key.len() {
                ps.push(key_array(chunk.iter().map(|k| &k[i]), t.key_type(i)));
            }
            let refs: Vec<&(dyn postgres::types::ToSql + Sync)> = ps.iter().map(|p| &**p as _).collect();
            del += tx.execute(sql.as_str(), &refs)?;
        }
    }
    if let Some((epoch, seq)) = cursor {
        tx.execute(
            "insert into catalog_cursor (shard, epoch, seq) values ($1, $2, $3) on conflict (shard) do update set
                seq = case when catalog_cursor.epoch = excluded.epoch then greatest(catalog_cursor.seq, excluded.seq)
                      else excluded.seq end,
                epoch = excluded.epoch",
            &[&shard_v, &epoch, &seq],
        )?;
    }
    tx.commit()?;
    Ok((up, del))
}

/// Postgres's position in the shard's outbox: 0 when it has none for this epoch.
pub fn pg_cursor(pg: &mut postgres::Client, shard: usize, epoch: Option<i64>) -> Result<i64, postgres::Error> {
    let Some(epoch) = epoch else { return Ok(0) };
    let row = pg.query_opt("select epoch, seq from catalog_cursor where shard = $1", &[&(shard as i16)])?;
    Ok(row.filter(|r| r.get::<_, i64>(0) == epoch).map_or(0, |r| r.get(1)))
}

/// Keys one `delete` takes: they go as arrays, a parameter per key column.
const DELETES_PER_STATEMENT: usize = 5_000;

/// One key column of many rows as a Postgres array of the column's type, for `delete_many_sql`.
fn key_array<'a>(values: impl Iterator<Item = &'a Value>, t: PgType) -> Box<dyn postgres::types::ToSql + Sync + Send> {
    let values: Vec<&Value> = values.collect();
    match t {
        PgType::Int => Box::new(
            values
                .iter()
                .map(|v| match v {
                    Value::Integer(i) => Some(*i),
                    Value::Real(f) => Some(*f as i64),
                    Value::Text(s) => s.trim().parse::<i64>().ok(),
                    _ => None,
                })
                .collect::<Vec<Option<i64>>>(),
        ),
        PgType::Real => Box::new(
            values
                .iter()
                .map(|v| match v {
                    Value::Integer(i) => Some(*i as f64),
                    Value::Real(f) => Some(*f),
                    _ => None,
                })
                .collect::<Vec<Option<f64>>>(),
        ),
        PgType::Text => Box::new(
            values
                .iter()
                .map(|v| match v {
                    Value::Text(s) => Some(s.replace('\0', "")),
                    Value::Integer(i) => Some(i.to_string()),
                    Value::Real(f) => Some(f.to_string()),
                    Value::Blob(b) => Some(String::from_utf8_lossy(b).replace('\0', "")),
                    Value::Null => None,
                })
                .collect::<Vec<Option<String>>>(),
        ),
        PgType::Bytes => Box::new(
            values
                .iter()
                .map(|v| match v {
                    Value::Blob(b) => Some(b.clone()),
                    Value::Text(s) => Some(s.clone().into_bytes()),
                    Value::Integer(i) => Some(i.to_string().into_bytes()),
                    Value::Real(f) => Some(f.to_string().into_bytes()),
                    Value::Null => None,
                })
                .collect::<Vec<Option<Vec<u8>>>>(),
        ),
    }
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
    pg.batch_execute(
        "create table if not exists catalog_cursor (shard smallint primary key, epoch bigint not null, seq bigint not null)",
    )?;
    for t in tables {
        for sql in t.ddl() {
            pg.batch_execute(&sql)?;
        }
    }
    Ok(())
}

/// Each shard's waiting changes and how long the oldest has waited, from the ends of the outbox only: a batch takes
/// out a run of the oldest seqs, soo what's left is every seq from the first to the last.
fn outbox_gauges(shard: usize, conn: &Connection, acked: i64) {
    let (first, last): (Option<i64>, Option<i64>) = conn
        .query_row(&format!("select min(seq), max(seq) from {OUTBOX} where seq > ?1"), [acked], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
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
    // a thread and a Postgres connection per shard: one shard's backlog, or a fill holding its lock, doesn't hold up
    // the others
    for (shard, path) in store::shard_paths(&main).into_iter().enumerate().filter(|(_, p)| p.exists()) {
        let (main, config, stop) = (main.clone(), config.clone(), stop.clone());
        let _ = std::thread::Builder::new()
            .name(format!("shared-catalog-{shard}"))
            .spawn(move || run(&main, &[(shard, path)], &config, &stop));
    }
}

/// A publisher: a batch from each of `shards` in turn, resting when their outboxes are empty, reconnecting after a
/// failure.
pub fn run(main: &Path, shards: &[(usize, PathBuf)], config: &SharedCatalogConfig, stop: &AtomicBool) {
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
        match publish(&mut pg, main, shards, config, stop) {
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

/// How long the publisher keeps the shards open while it has a backlog before it lets go.
const SESSION: Duration = Duration::from_secs(60);

/// A compaction in this process is about to start: the publisher lets go of the shards and stays away until it's
/// done (`make_way_for_compaction`).
static COMPACTION_WANTED: AtomicBool = AtomicBool::new(false);
/// Publisher threads that have their shard open and compaction held off.
static HOLDING: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Counts a publisher thread in `HOLDING` for as long as it lives.
struct Held;

impl Held {
    fn new() -> Held {
        HOLDING.fetch_add(1, Relaxed);
        Held
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        HOLDING.fetch_sub(1, Relaxed);
    }
}

/// Clears the wish to compact when dropped, once the compaction is over.
pub struct MakingWay(());

impl Drop for MakingWay {
    fn drop(&mut self) {
        COMPACTION_WANTED.store(false, Relaxed);
    }
}

/// The indexer calls this before it compacts: the publisher closes its shards at the end of the batch it's on and
/// takes them up again only after the returned guard drops. Waits up to `wait` for it to let go, so the
/// compaction's one try at its lock isn't refused because of the publisher.
pub fn make_way_for_compaction(wait: Duration) -> MakingWay {
    COMPACTION_WANTED.store(true, Relaxed);
    let until = std::time::Instant::now() + wait;
    while HOLDING.load(Relaxed) > 0 && std::time::Instant::now() < until {
        std::thread::sleep(Duration::from_millis(50));
    }
    MakingWay(())
}

/// The shards open, and the hold on compaction that keeps them from being swapped meanwhile. A compaction can't
/// rename a shard someone has open on Windows, and on Unix an open connection would go on reading the original;
/// soo the publisher opens them only while compaction is held off, and lets go when it has nothing to send or after
/// `SESSION`.
struct Session {
    // dropped in this order: the connections, the hold, then the count
    open: Vec<Open>,
    _hold: crate::compact::WriteGuard,
    _held: Held,
    since: std::time::Instant,
}

/// A shard open in a session: its outbox epoch and how far Postgres has taken it.
struct Open {
    shard: usize,
    path: PathBuf,
    conn: Connection,
    tables: Vec<Table>,
    epoch: Option<i64>,
    acked: i64,
}

impl Drop for Session {
    fn drop(&mut self) {
        // the connections close before the hold goes (field order), and the count after both
        self.open.clear();
    }
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
            if COMPACTION_WANTED.load(Relaxed) {
                rest(stop, Duration::from_millis(config.interval_ms));
                continue;
            }
            let Some(hold) = crate::compact::try_hold_off_compaction(main)? else {
                // a compaction runs: wait for it
                rest(stop, Duration::from_millis(config.interval_ms));
                continue;
            };
            let mut open = Vec::new();
            for (shard, path) in shards {
                let conn = crate::db::open_shard(path)?;
                // its reads never wait in WAL; the one write, taking out what was sent, only tries (the shard's
                // writer takes it out anyway, `prune_acked`)
                conn.busy_timeout(Duration::from_millis(200))?;
                let tables = tables(&conn)?;
                if ensured.get(shard) != Some(&tables) {
                    ensure_schema(pg, &tables)?;
                    ensured.insert(*shard, tables.clone());
                }
                let epoch = epoch(&conn)?;
                let acked = pg_cursor(pg, *shard, epoch)?;
                open.push(Open { shard: *shard, path: path.clone(), conn, tables, epoch, acked });
            }
            session = Some(Session { open, _hold: hold, _held: Held::new(), since: std::time::Instant::now() });
        }
        let Some(open) = session.as_mut().map(|s| &mut s.open) else { continue };
        let mut busy = false;
        for o in open.iter_mut() {
            let batch = read_batch_after(&o.conn, &o.tables, config.batch, BATCH_BYTES, o.acked)?;
            if !batch.is_empty() {
                let cursor = o.epoch.map(|e| (e, batch.max_seq));
                let (up, del) = crate::metrics::timed("shared_catalog.send", || {
                    send_with_cursor(pg, o.shard, &o.tables, &batch, cursor)
                })?;
                o.acked = batch.max_seq;
                if let Some(e) = o.epoch {
                    let _ = write_cursor(&o.path, e, o.acked);
                }
                // taken out now if the lock is free; otherwise the shard's writer does it
                if ack(&o.conn, o.acked).is_err() {
                    names::SHARED_CATALOG_FAILURES.inc(&[("stage", "ack_deferred")], 1);
                }
                names::SHARED_CATALOG_ROWS.inc(&[("op", "upsert")], up);
                names::SHARED_CATALOG_ROWS.inc(&[("op", "delete")], del);
                busy |= batch.full;
            }
        }
        if gauges_at.is_none_or(|at| at.elapsed() >= Duration::from_secs(30)) {
            for o in open.iter() {
                outbox_gauges(o.shard, &o.conn, o.acked);
            }
            gauges_at = Some(std::time::Instant::now());
        }
        if !busy || COMPACTION_WANTED.load(Relaxed) || session.as_ref().is_some_and(|s| s.since.elapsed() >= SESSION) {
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

/// Rows a fill or check reads at a time.
const PAGE: usize = 5_000;

/// Copies every row of every table of one shard to Postgres, a page at a time in key order, and returns the rows
/// written per table. It writes rows that are there and refreshes rows Postgres has, even ones changed while the
/// catalog was off and no trigger saw. Each page is read from the shard while holding the shard's advisory lock
/// in Postgres (`lock_shard`), which every publisher batch holds too: a batch sent before the page is no newer than
/// it, and a change after the read stays in the outbox until a later batch sends it. Pages stop at `PAGE` rows or
/// `BATCH_BYTES` of them.
pub fn fill_shard(conn: &Connection, pg: &mut postgres::Client, shard: usize) -> anyhow::Result<Vec<(String, u64)>> {
    let tables = tables(conn)?;
    ensure_schema(pg, &tables)?;
    let mut out = Vec::new();
    for t in &tables {
        let (mut last, mut written): (Option<Vec<Value>>, u64) = (None, 0);
        let width = t.cols.len() + 1;
        let key_at = t.key_at();
        loop {
            let mut tx = pg.transaction()?;
            lock_shard(&mut tx, shard)?;
            let (rows, ended) = {
                let read = conn.unchecked_transaction()?;
                let mut stmt = read.prepare_cached(&t.page_sql(last.is_none()))?;
                let mut rows = match last.as_deref() {
                    None => stmt.query(params![PAGE as i64])?,
                    Some([k1]) => stmt.query(params![PAGE as i64, k1])?,
                    Some([k1, k2, ..]) => stmt.query(params![PAGE as i64, k1, k2])?,
                    Some([]) => unreachable!("a key has a column"),
                };
                let (mut page, mut bytes, mut ended) = (Vec::new(), 0usize, true);
                while let Some(r) = rows.next()? {
                    let row = (0..t.cols.len()).map(|i| r.get::<_, Value>(i)).collect::<rusqlite::Result<Vec<_>>>()?;
                    bytes += row.iter().map(value_bytes).sum::<usize>();
                    page.push(row);
                    if bytes >= BATCH_BYTES {
                        ended = false;
                        break;
                    }
                }
                ended &= page.len() < PAGE;
                drop(rows);
                drop(stmt);
                read.commit()?;
                (page, ended)
            };
            if let Some(tail) = rows.last() {
                last = Some(key_at.iter().map(|i| tail[*i].clone()).collect());
            }
            for chunk in rows.chunks(rows_per_statement(width)) {
                let mut ps: Vec<Box<dyn postgres::types::ToSql + Sync + Send>> =
                    Vec::with_capacity(chunk.len() * width);
                for row in chunk {
                    ps.push(Box::new(shard as i16));
                    for (v, (_, ty)) in row.iter().zip(&t.cols) {
                        ps.push(pg_value(v, *ty));
                    }
                }
                let refs: Vec<&(dyn postgres::types::ToSql + Sync)> = ps.iter().map(|p| &**p as _).collect();
                written += tx.execute(t.upsert_sql(chunk.len()).as_str(), &refs)?;
            }
            tx.commit()?;
            if ended || rows.is_empty() {
                break;
            }
        }
        names::SHARED_CATALOG_ROWS.inc(&[("op", "fill")], written);
        out.push((t.name.clone(), written));
    }
    Ok(out)
}

/// One table's rows in the shard and in Postgres, and the Postgres rows whose key the shard no longer has (with
/// `prune`, the ones deleted).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Check {
    pub table: String,
    pub shard_rows: i64,
    pub pg_rows: i64,
    pub stale: u64,
}

/// Counts each table on both sides and finds the rows Postgres keeps that the shard deleted (a fill that read a
/// row just before it was deleted puts it back after the publisher took it out). With `prune` it deletes them:
/// safe while the indexer runs, since a key gone from the shard is gone for good or comes back through the outbox.
pub fn check_shard(
    conn: &Connection,
    pg: &mut postgres::Client,
    shard: usize,
    prune: bool,
) -> anyhow::Result<Vec<Check>> {
    let tables = tables(conn)?;
    ensure_schema(pg, &tables)?;
    let shard_v = shard as i16;
    let mut out = Vec::new();
    for t in &tables {
        let shard_rows: i64 = conn.query_row(&format!("select count(*) from {}", quote(&t.name)), [], |r| r.get(0))?;
        let pg_rows: i64 =
            pg.query_one(&format!("select count(*) from {} where shard = $1", quote(&t.name)), &[&shard_v])?.get(0);
        let mut stale = 0u64;
        let mut last: Option<Vec<Value>> = None;
        let mut exists = conn.prepare_cached(&t.exists_sql())?;
        let mut in_shard = |key: &[Value]| -> rusqlite::Result<bool> {
            if key.len() == 2 { exists.exists(params![key[0], key[1]]) } else { exists.exists(params![key[0]]) }
        };
        let (lock, del) = (pg.prepare(&t.lock_sql())?, pg.prepare(&t.delete_sql())?);
        loop {
            let mut ps: Vec<Box<dyn postgres::types::ToSql + Sync + Send>> =
                vec![Box::new(shard_v), Box::new(PAGE as i64)];
            if let Some(k) = &last {
                for (i, v) in k.iter().enumerate() {
                    ps.push(pg_value(v, t.key_type(i)));
                }
            }
            let refs: Vec<&(dyn postgres::types::ToSql + Sync)> = ps.iter().map(|p| &**p as _).collect();
            let rows = pg.query(t.pg_keys_sql(last.is_none()).as_str(), &refs)?;
            let n = rows.len();
            for r in &rows {
                let key: Vec<Value> = (0..t.key.len()).map(|i| pg_to_value(r, i, t.key_type(i))).collect();
                if !in_shard(&key)? {
                    if !prune {
                        stale += 1;
                    } else {
                        // the Postgres row locked first, then the shard asked again: a publisher sending the key
                        // back waits for this transaction, and one that sent it before the lock is seen by the
                        // second look, soo a row put back meanwhile is never deleted
                        let mut kps: Vec<Box<dyn postgres::types::ToSql + Sync + Send>> = vec![Box::new(shard_v)];
                        for (i, v) in key.iter().enumerate() {
                            kps.push(pg_value(v, t.key_type(i)));
                        }
                        let krefs: Vec<&(dyn postgres::types::ToSql + Sync)> = kps.iter().map(|p| &**p as _).collect();
                        let mut tx = pg.transaction()?;
                        if !tx.query(&lock, &krefs)?.is_empty() && !in_shard(&key)? {
                            tx.execute(&del, &krefs)?;
                            stale += 1;
                        }
                        tx.commit()?;
                    }
                }
                last = Some(key);
            }
            if n < PAGE {
                break;
            }
        }
        out.push(Check { table: t.name.clone(), shard_rows, pg_rows, stale });
    }
    Ok(out)
}

/// A Postgres key column back as a SQLite value.
fn pg_to_value(row: &postgres::Row, i: usize, t: PgType) -> Value {
    match t {
        PgType::Int => row.get::<_, Option<i64>>(i).map_or(Value::Null, Value::Integer),
        PgType::Real => row.get::<_, Option<f64>>(i).map_or(Value::Null, Value::Real),
        PgType::Text => row.get::<_, Option<String>>(i).map_or(Value::Null, Value::Text),
        PgType::Bytes => row.get::<_, Option<Vec<u8>>>(i).map_or(Value::Null, Value::Blob),
    }
}

/// Each shard that has the outbox: a fill or check needs the triggers in, or a change made meanwhile is lost.
fn armed_shards() -> anyhow::Result<Vec<(usize, PathBuf)>> {
    let main = crate::paths::database();
    let mut out = Vec::new();
    for (shard, path) in store::shard_paths(&main).into_iter().enumerate() {
        if !path.exists() {
            continue;
        }
        let conn = crate::db::open_shard(&path)?;
        let armed: i64 =
            conn.query_row("select count(*) from sqlite_schema where name = ?1", [OUTBOX], |r| r.get(0))?;
        if armed == 0 {
            anyhow::bail!(
                "shard {shard} has no {OUTBOX}: turn shared_catalog on in config.toml and restart the indexer first"
            );
        }
        out.push((shard, path));
    }
    Ok(out)
}

/// `atlas --shared-catalog-fill`: copies the rows that were there before the triggers, and refreshes the copy after
/// a time with the catalog off. Runs beside the indexer, holding off compaction meanwhile.
pub fn run_fill() -> i32 {
    let config = crate::config::shared_catalog_config();
    if !config.is_on() {
        println!("shared_catalog isn't on in config.toml");
        return 1;
    }
    let result = (|| -> anyhow::Result<()> {
        // a compaction would swap a shard under the fill, which would go on copying the original
        let _hold = crate::compact::hold_off_compaction(&crate::paths::database())?;
        let shards = armed_shards()?;
        let mut pg = connect(&config)?;
        for (shard, path) in shards {
            let started = std::time::Instant::now();
            let conn = crate::db::open_shard(&path)?;
            let added = fill_shard(&conn, &mut pg, shard)?;
            let total: u64 = added.iter().map(|(_, n)| n).sum();
            println!("shard {shard}: {total} rows written in {}s", started.elapsed().as_secs());
            for (t, n) in added.iter().filter(|(_, n)| *n > 0) {
                println!("  {t}: {n}");
            }
        }
        Ok(())
    })();
    match result {
        Ok(()) => 0,
        Err(e) => {
            println!("shared catalog fill: {e:#}");
            1
        }
    }
}

/// `atlas --shared-catalog-check [--prune]`: rows per table on both sides, and the stale ones in Postgres.
pub fn run_check(prune: bool) -> i32 {
    let config = crate::config::shared_catalog_config();
    if !config.is_on() {
        println!("shared_catalog isn't on in config.toml");
        return 1;
    }
    let result = (|| -> anyhow::Result<bool> {
        // a compaction would swap a shard under the check, whose lookups would then ask the original
        let _hold = crate::compact::hold_off_compaction(&crate::paths::database())?;
        let shards = armed_shards()?;
        let mut pg = connect(&config)?;
        let mut clean = true;
        for (shard, path) in shards {
            let conn = crate::db::open_shard(&path)?;
            let acked = pg_cursor(&mut pg, shard, epoch(&conn)?)?;
            let waiting: i64 =
                conn.query_row(&format!("select count(*) from {OUTBOX} where seq > ?1"), [acked], |r| r.get(0))?;
            println!("shard {shard}: {waiting} changes waiting in the outbox");
            for c in check_shard(&conn, &mut pg, shard, prune)? {
                let mark = if c.shard_rows == c.pg_rows && c.stale == 0 { "ok" } else { "differs" };
                clean &= mark == "ok";
                println!(
                    "  {:<24} shard {:>12} postgres {:>12} stale {:>8} {}{mark}",
                    c.table,
                    c.shard_rows,
                    c.pg_rows,
                    c.stale,
                    if prune && c.stale > 0 { "pruned, " } else { "" }
                );
            }
        }
        Ok(clean)
    })();
    match result {
        Ok(true) => 0,
        Ok(false) => 2,
        Err(e) => {
            println!("shared catalog check: {e:#}");
            1
        }
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
    fn a_compaction_waits_for_the_publisher_to_let_go_and_keeps_it_away_till_done() {
        let _one = PG.lock().unwrap_or_else(|e| e.into_inner());
        let held = Held::new();
        let publisher = std::thread::spawn(move || {
            // the publisher sees the wish at the end of its batch
            while !COMPACTION_WANTED.load(Relaxed) {
                std::thread::sleep(Duration::from_millis(5));
            }
            std::thread::sleep(Duration::from_millis(100));
            drop(held);
        });
        let started = std::time::Instant::now();
        let way = make_way_for_compaction(Duration::from_secs(10));
        assert_eq!(HOLDING.load(Relaxed), 0);
        assert!(started.elapsed() >= Duration::from_millis(100), "it waited for the let-go");
        assert!(COMPACTION_WANTED.load(Relaxed), "and the publisher stays away meanwhile");
        drop(way);
        assert!(!COMPACTION_WANTED.load(Relaxed));
        publisher.join().unwrap();
        // nobody holding: no wait at all
        let started = std::time::Instant::now();
        drop(make_way_for_compaction(Duration::from_secs(10)));
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn a_table_no_longer_copied_loses_its_triggers() {
        let conn = shard();
        conn.execute_batch(
            "create table pp_queue (release_id INTEGER PRIMARY KEY, due_at INTEGER NOT NULL);
             create table catalog_outbox (seq INTEGER PRIMARY KEY, tbl TEXT NOT NULL, k1, k2, at INTEGER);
             create trigger catalog_pp_queue_ai after insert on pp_queue begin
                insert into catalog_outbox (tbl, k1) values ('pp_queue', new.release_id);
             end;",
        )
        .unwrap();
        sync_triggers(&conn, true).unwrap();
        conn.execute("insert into pp_queue values (8, 1)", []).unwrap();
        let waiting: i64 = conn.query_row("select count(*) from catalog_outbox", [], |r| r.get(0)).unwrap();
        assert_eq!(waiting, 0, "pp_queue isn't copied any more");
        conn.execute("insert into releases (id, name) values (8, 'a')", []).unwrap();
        let waiting: i64 = conn.query_row("select count(*) from catalog_outbox", [], |r| r.get(0)).unwrap();
        assert_eq!(waiting, 1, "the copied tables still are");
        // and turned off, a retired table's trigger goes too, before the outbox it writes to
        conn.execute_batch(
            "create trigger catalog_pp_queue_ai after insert on pp_queue begin
                insert into catalog_outbox (tbl, k1) values ('pp_queue', new.release_id);
             end;",
        )
        .unwrap();
        sync_triggers(&conn, false).unwrap();
        conn.execute("insert into pp_queue values (16, 1)", []).unwrap();
        let left: i64 =
            conn.query_row("select count(*) from sqlite_schema where name like 'catalog%'", [], |r| r.get(0)).unwrap();
        assert_eq!(left, 0);
        assert_eq!(
            tables(&conn).unwrap().iter().find(|t| t.name == "par2_files").unwrap().delete_many_sql(),
            "delete from \"par2_files\" where shard = $1 and (\"release_id\", \"file_id\") in (select * from unnest($2::bigint[], $3::bytea[]))"
        );
    }

    #[test]
    fn an_outbox_has_an_epoch_and_a_batch_starts_after_what_postgres_has() {
        let conn = shard();
        conn.execute_batch("create table meta (key TEXT PRIMARY KEY, value INTEGER);").unwrap();
        assert_eq!(epoch(&conn).unwrap(), None);
        sync_triggers(&conn, true).unwrap();
        let first = epoch(&conn).unwrap().unwrap();
        sync_triggers(&conn, true).unwrap();
        assert_eq!(epoch(&conn).unwrap(), Some(first), "the same outbox, the same epoch");
        for i in 1..=5 {
            conn.execute("insert into releases (id, name) values (?1, 'x')", [i * 8]).unwrap();
        }
        let t = tables(&conn).unwrap();
        let rest = read_batch_after(&conn, &t, 100, BATCH_BYTES, 3).unwrap();
        assert_eq!((rest.changes, rest.max_seq), (2, 5), "only what's past seq 3");
        sync_triggers(&conn, false).unwrap();
        assert_eq!(epoch(&conn).unwrap(), None);
        sync_triggers(&conn, true).unwrap();
        assert_ne!(epoch(&conn).unwrap(), Some(first), "a new outbox, a new epoch");
    }

    #[test]
    fn the_writer_prunes_what_the_cursor_file_says_postgres_has_for_this_outbox_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("atlas.s1.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "create table meta (key TEXT PRIMARY KEY, value INTEGER);
             create table releases (id INTEGER PRIMARY KEY, name TEXT);",
        )
        .unwrap();
        sync_triggers(&conn, true).unwrap();
        for i in 1..=5 {
            conn.execute("insert into releases (id, name) values (?1, 'x')", [i * 8]).unwrap();
        }
        assert_eq!(prune_acked(&conn).unwrap(), 0, "no cursor file yet");
        let e = epoch(&conn).unwrap().unwrap();
        write_cursor(&path, e + 1, 4).unwrap();
        assert_eq!(prune_acked(&conn).unwrap(), 0, "a cursor for another outbox");
        write_cursor(&path, e, 3).unwrap();
        assert_eq!(prune_acked(&conn).unwrap(), 3);
        let left: Vec<i64> = conn
            .prepare("select seq from catalog_outbox")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(left, [4, 5]);
        assert_eq!(Connection::open_in_memory().map(|c| prune_acked(&c).unwrap()).unwrap(), 0, "no file behind it");
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
                 create table nzb_pointed (release_id INTEGER PRIMARY KEY);
                 insert into nzb_pointed values (16);
                 insert into releases (id, name, size, posted_date) values (8, 'a', 7, null), (16, 'b', 20, 'x');",
            )
            .unwrap();
        let orig = dir.path().join("orig.db");
        conn.execute("vacuum into ?1", [orig.to_string_lossy()]).unwrap();
        copy_conn.execute("attach database ?1 as old", [orig.to_string_lossy()]).unwrap();
        assert!(carry_outbox(&copy_conn).unwrap());
        let seqs: Vec<i64> = copy_conn
            .prepare("select seq from catalog_outbox order by seq limit 3")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        let old_seqs: Vec<i64> = conn
            .prepare("select seq from catalog_outbox order by seq")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(seqs, old_seqs, "the waiting changes keep their seqs, soo a saved position still holds");
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
                ("nzb_pointed".into(), 16),
            ],
            "8 for its size, 16 for another column with the size unchanged, -3 left out of the copy, 16 added to it"
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

    #[test]
    fn pages_walk_a_table_in_key_order_one_page_after_another() {
        let conn = shard();
        conn.execute_batch(
            "insert into par2_files values (8, x'02', 'b'), (8, x'01', 'a'), (16, x'01', 'c');
             insert into releases (id, name) values (24, 'x'), (8, 'y'), (16, 'z');",
        )
        .unwrap();
        let t = tables(&conn).unwrap();
        let par2 = t.iter().find(|t| t.name == "par2_files").unwrap();
        let first: Vec<String> = conn
            .prepare(&par2.page_sql(true))
            .unwrap()
            .query_map(params![2], |r| r.get::<_, String>(2))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(first, ["a", "b"]);
        let next: Vec<String> = conn
            .prepare(&par2.page_sql(false))
            .unwrap()
            .query_map(params![2, 8, vec![2u8]], |r| r.get::<_, String>(2))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(next, ["c"]);
        for (t, first) in [(par2, false), (par2, true)] {
            let plan: String = conn
                .query_row(&format!("explain query plan {}", t.page_sql(first)), params![1, 8, vec![2u8]], |r| r.get(3))
                .unwrap_or_default();
            assert!(first || plan.starts_with("SEARCH"), "a later page seeks: {plan}");
        }
        let rel = t.iter().find(|t| t.name == "releases").unwrap();
        assert_eq!(rel.key_at(), [0]);
        let ids: Vec<i64> = conn
            .prepare(&rel.page_sql(false))
            .unwrap()
            .query_map(params![10, 8], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(ids, [16, 24]);
        assert_eq!(
            par2.lock_sql(),
            "select 1 from \"par2_files\" where shard = $1 and \"release_id\" = $2 and \"file_id\" = $3 for update"
        );
        assert!(par2.pg_keys_sql(false).contains("and (\"release_id\", \"file_id\") > ($3, $4)"));
    }

    /// Against a real Postgres when `ATLAS_CATALOG_TEST_URL` names an empty database to use: the fill writes every
    /// row and refreshes stale ones, waits for a batch holding the shard's lock, and the check finds and prunes rows
    /// the shard deleted.
    #[test]
    fn a_fill_writes_and_refreshes_every_row_under_the_shard_lock_and_a_check_prunes_what_the_shard_deleted() {
        let Ok(url) = std::env::var("ATLAS_CATALOG_TEST_URL") else { return };
        let _one = PG.lock().unwrap_or_else(|e| e.into_inner());
        let mut pg = postgres::Client::connect(&url, postgres::NoTls).unwrap();
        pg.batch_execute("drop table if exists releases, collections, par2_files, nzb_pointed").unwrap();
        let conn = shard();
        let many: Vec<String> = (1..=12_000).map(|i| format!("({}, 'r{i}')", i * 8)).collect();
        conn.execute_batch(&format!("insert into releases (id, name) values {};", many.join(", "))).unwrap();
        conn.execute_batch("insert into par2_files values (8, x'01', 'a'), (8, x'02', 'b');").unwrap();
        sync_triggers(&conn, true).unwrap();
        let t = tables(&conn).unwrap();
        ensure_schema(&mut pg, &t).unwrap();
        // Postgres holds a row the shard changed while the catalog was off
        pg.execute("insert into releases (shard, id, name) values (2, 8, 'stale')", &[]).unwrap();
        let written = fill_shard(&conn, &mut pg, 2).unwrap();
        assert_eq!(written.iter().find(|(n, _)| n == "releases").unwrap().1, 12_000, "three pages, 8 refreshed");
        assert_eq!(written.iter().find(|(n, _)| n == "par2_files").unwrap().1, 2);
        let name: String = pg.query_one("select name from releases where id = 8", &[]).unwrap().get(0);
        assert_eq!(name, "r1", "the shard's row, not the stale copy");
        // a fill waits for a batch in flight: the advisory lock is the publisher's too
        let mut other = postgres::Client::connect(&url, postgres::NoTls).unwrap();
        let mut held = other.transaction().unwrap();
        lock_shard(&mut held, 2).unwrap();
        let (url2, done) = (url.clone(), Arc::new(AtomicBool::new(false)));
        let filler = {
            let done = done.clone();
            std::thread::spawn(move || {
                let conn = shard();
                conn.execute("insert into releases (id, name) values (8, 'r1')", []).unwrap();
                let mut pg = postgres::Client::connect(&url2, postgres::NoTls).unwrap();
                fill_shard(&conn, &mut pg, 2).unwrap();
                done.store(true, Relaxed);
            })
        };
        std::thread::sleep(Duration::from_millis(300));
        assert!(!done.load(Relaxed), "the fill waits for the lock");
        held.commit().unwrap();
        filler.join().unwrap();
        assert!(done.load(Relaxed));
        // a row the shard deleted that Postgres still has
        conn.execute_batch("delete from releases where id = 16; delete from par2_files where file_id = x'02';")
            .unwrap();
        let checks = check_shard(&conn, &mut pg, 2, false).unwrap();
        let rel = checks.iter().find(|c| c.table == "releases").unwrap();
        assert_eq!((rel.shard_rows, rel.pg_rows, rel.stale), (11_999, 12_000, 1));
        assert_eq!(checks.iter().find(|c| c.table == "par2_files").unwrap().stale, 1);
        check_shard(&conn, &mut pg, 2, true).unwrap();
        let after = check_shard(&conn, &mut pg, 2, false).unwrap();
        assert!(after.iter().all(|c| c.stale == 0 && c.shard_rows == c.pg_rows), "{after:?}");
        pg.batch_execute("drop table releases, collections, par2_files, nzb_pointed").unwrap();
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
        let e = epoch(&conn).unwrap();
        assert!(pg_cursor(&mut pg, 5, e).unwrap() >= queued, "Postgres has its place in the outbox");
        assert_eq!(read_cursor(&path).map(|(epoch, _)| epoch), e, "and the writer's cursor file says so");
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
        conn.execute_batch(
            "delete from releases where id = 16; update releases set size = 9 where id = 8;
             insert into par2_files values (8, x'01', 'b.par2');",
        )
        .unwrap();
        let batch = read_batch(&conn, &t, 100, BATCH_BYTES).unwrap();
        assert_eq!(send(&mut pg, 3, &t, &batch).unwrap(), (2, 1));
        ack(&conn, batch.max_seq).unwrap();
        // many deletes go as one statement, a two-column blob key included
        conn.execute_batch("delete from par2_files;").unwrap();
        let batch = read_batch(&conn, &t, 100, BATCH_BYTES).unwrap();
        assert_eq!(send(&mut pg, 3, &t, &batch).unwrap(), (0, 2));
        let left: i64 = pg.query_one("select count(*) from par2_files", &[]).unwrap().get(0);
        assert_eq!(left, 0);
        // the position moves with a batch, only forwards for the same outbox, and starts over for a new one
        pg.batch_execute("delete from catalog_cursor where shard = 3").unwrap();
        assert_eq!(pg_cursor(&mut pg, 3, Some(77)).unwrap(), 0);
        send_with_cursor(&mut pg, 3, &t, &Batch::default(), Some((77, 50))).unwrap();
        send_with_cursor(&mut pg, 3, &t, &Batch::default(), Some((77, 40))).unwrap();
        assert_eq!(pg_cursor(&mut pg, 3, Some(77)).unwrap(), 50);
        assert_eq!(pg_cursor(&mut pg, 3, Some(78)).unwrap(), 0, "another outbox");
        send_with_cursor(&mut pg, 3, &t, &Batch::default(), Some((78, 5))).unwrap();
        assert_eq!(pg_cursor(&mut pg, 3, Some(78)).unwrap(), 5);
        assert_eq!(pg_cursor(&mut pg, 3, None).unwrap(), 0);
        let row = pg.query_one("select shard, name, size from releases", &[]).unwrap();
        assert_eq!((row.get::<_, i16>(0), row.get::<_, String>(1), row.get::<_, i64>(2)), (3, "a".into(), 9));

        let n: i64 = pg.query_one("select count(*) from collections where shard = 3", &[]).unwrap().get(0);
        assert_eq!(n, 1);
        // a shard migration's new column reaches Postgres
        conn.execute_batch("alter table releases add column name_source INTEGER").unwrap();
        ensure_schema(&mut pg, &tables(&conn).unwrap()).unwrap();
        pg.query_one("select name_source from releases", &[]).unwrap();
        pg.batch_execute("drop table releases, collections, par2_files, nzb_pointed").unwrap();
    }
}
