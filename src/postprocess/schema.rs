//! The tables of the processing stage and of the parts that plug into it. A
//! line in `SHARD_TABLES` gets a table created on open, deleted with its
//! release, and copied and counted by compaction.

use rusqlite::{Connection, Result, params};

/// A table in every shard.
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

/// what changed and wants a look: written by saves and rerun requests. Walks dont use it
pub const PP_QUEUE: ShardTable = ShardTable {
    name: "pp_queue",
    create: "create table if not exists pp_queue (
        release_id INTEGER PRIMARY KEY,
        due_at     INTEGER NOT NULL
    )",
    indexes: &[("pp_queue_due", "create index if not exists pp_queue_due on pp_queue(due_at)")],
    release_key: Some("release_id"),
    added: &[],
};

/// only releases that passed the rules and had a step run. Rejected rows have none.
pub const PP_STATE: ShardTable = ShardTable {
    name: "pp_state",
    create: "create table if not exists pp_state (
        release_id   INTEGER PRIMARY KEY,
        done         INTEGER NOT NULL DEFAULT 0,
        failed       INTEGER NOT NULL DEFAULT 0,
        net_wanted   INTEGER,
        db_attempts  INTEGER NOT NULL DEFAULT 0,
        net_attempts INTEGER NOT NULL DEFAULT 0,
        net_next_at  INTEGER NOT NULL DEFAULT 0,
        parts_seen   INTEGER NOT NULL DEFAULT 0,
        last_error   TEXT,
        rerun_gen    INTEGER NOT NULL DEFAULT 0,
        rerun_mask   INTEGER NOT NULL DEFAULT 0
    )",
    indexes: &[
        (
            "pp_state_net",
            "create index if not exists pp_state_net on pp_state(release_id, net_next_at) where net_wanted = 1",
        ),
        ("pp_state_failed", "create index if not exists pp_state_failed on pp_state(release_id) where failed != 0"),
    ],
    release_key: Some("release_id"),
    added: &[],
};

/// a resumable pass over existing releases, newest first
pub const PP_WALKS: ShardTable = ShardTable {
    name: "pp_walks",
    create: "create table if not exists pp_walks (
        id         INTEGER PRIMARY KEY,
        kind       TEXT NOT NULL,
        top_id     INTEGER NOT NULL,
        before_id  INTEGER NOT NULL,
        redo_mask  INTEGER NOT NULL DEFAULT 0,
        group_name TEXT,
        seen       INTEGER NOT NULL DEFAULT 0,
        ran        INTEGER NOT NULL DEFAULT 0,
        created_at INTEGER NOT NULL
    )",
    indexes: &[],
    release_key: None,
    added: &[],
};

pub const SHARD_TABLES: &[&ShardTable] = &[&PP_QUEUE, &PP_STATE, &PP_WALKS];

/// nullable columns on `releases`, append only. `name_source`: NULL, the name came from
/// ingest, otherwise the priority of the step source that set it, and saves leave the name alone
pub const RELEASE_COLUMNS: &[(&str, &str)] = &[("name_source", "INTEGER")];

fn columns(conn: &Connection, table: &str) -> Result<Vec<String>> {
    conn.prepare("select name from pragma_table_info(?1)")?.query_map([table], |r| r.get(0))?.collect()
}

fn add_missing(conn: &Connection, table: &str, wanted: &[(&str, &str)]) -> Result<()> {
    let have = columns(conn, table)?;
    for (col, kind) in wanted {
        if !have.iter().any(|c| c == col) {
            conn.execute(&format!("alter table {table} add column {col} {kind}"), [])?;
        }
    }
    Ok(())
}

/// Every registered table, index, trigger and column, if missing: nothing
/// existing is rewritten, and no index is built on a table that has rows.
pub fn migrate(conn: &Connection) -> Result<()> {
    add_missing(conn, "releases", RELEASE_COLUMNS)?;
    for table in SHARD_TABLES {
        conn.execute_batch(table.create)?;
        add_missing(conn, table.name, table.added)?;
        for (_, create) in table.indexes {
            conn.execute_batch(create)?;
        }
        if let Some(key) = table.release_key {
            conn.execute_batch(&format!(
                "create trigger if not exists gc_{0} after delete on releases
                 begin delete from {0} where {key} = old.id; end;",
                table.name
            ))?;
        }
    }
    Ok(())
}

/// A row of `table` for a test, values by declared type, its key column `release_id`.
#[doc(hidden)]
pub fn sample_row(conn: &Connection, table: &ShardTable, release_id: i64) -> Result<()> {
    let cols: Vec<(String, String)> = conn
        .prepare("select name, type from pragma_table_info(?1)")?
        .query_map([table.name], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_>>()?;
    let values: Vec<String> = cols
        .iter()
        .map(|(name, kind)| {
            if table.release_key == Some(name.as_str()) || (table.release_key.is_none() && name == "id") {
                return release_id.to_string();
            }
            match kind.to_ascii_uppercase().as_str() {
                "TEXT" => "'x'".to_string(),
                "BLOB" => "x'00'".to_string(),
                "REAL" => "1.0".to_string(),
                _ => "1".to_string(),
            }
        })
        .collect();
    let names: Vec<&str> = cols.iter().map(|(n, _)| n.as_str()).collect();
    conn.execute(
        &format!("insert into {} ({}) values ({})", table.name, names.join(", "), values.join(", ")),
        params![],
    )?;
    Ok(())
}
