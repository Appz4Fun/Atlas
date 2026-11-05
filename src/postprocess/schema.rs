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
        // the net lane's usual order: biggest video first, newest first within a rank. The unranked are together too,
        // for `engine::rank_lane`
        (
            "pp_state_net_rank",
            "create index if not exists pp_state_net_rank on pp_state(net_rank desc, release_id desc) where net_wanted = 1",
        ),
    ],
    release_key: Some("release_id"),
    // transient net failures in a row (see `engine::transient_wait`), apart from `net_attempts`; `net_rank`, see
    // `NET_RANK`: NULL, not ranked yet
    added: &[("net_transient", "INTEGER"), ("net_rank", "INTEGER")],
};

/// How soon the net lane gets to release `?1`, higher first: what the owner wants most is big movies and TV, and a
/// fully obfuscated upload of one looks like a big release with no category or an Other one until the net lane
/// reads it. 1 from 1 GiB, 2 from 4 GiB, 3 from 15 GiB (UHD remuxes), 4 from 40 GiB. A movie or TV release of
/// `SMALL_VIDEO` or more is 1 too: an SD or 720p episode is under 1 GiB, and at 0 it waits behind every small post
/// of every kind, soo it was never probed. 0: smaller, or a category that is neither movies, TV nor Other. A release
/// with an NZB among its files is 2 at least, whatever its size or category: one small article that says what a whole
/// other post is (`archive::nzb`), the first hop to it. And a release such an NZB read in this shard points to
/// (`nzb_pointed`) is 2 at least too: the next hop.
pub const NET_RANK: &str = "coalesce((select max(case
        when c.category is not null and c.category not between 2000 and 2999 and c.category not between 5000 and 5999
            and c.category not between 8000 and 8999 then 0
        when r.size >= 42949672960 then 4
        when r.size >= 16106127360 then 3
        when r.size >= 4294967296 then 2
        when r.size >= 1073741824 then 1
        when r.size >= 52428800 and (c.category between 2000 and 2999 or c.category between 5000 and 5999) then 1
        else 0 end,
        case when exists (select 1 from files f where f.release_id = r.id and f.filename like '%.nzb')
            or exists (select 1 from nzb_pointed p where p.release_id = r.id) then 2 else 0 end)
    from releases r left join release_category c on c.release_id = r.id where r.id = ?1), 0)";

/// The smallest movie or TV release `NET_RANK` puts with the 1 GiB ones: 50 MiB, past samples and the odd file of a
/// split post.
pub const SMALL_VIDEO: i64 = 50 << 20;

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

pub const SHARD_TABLES: &[&ShardTable] = &[
    &PP_QUEUE,
    &PP_STATE,
    &PP_WALKS,
    &crate::archive::store::INNER_FILES,
    &crate::archive::store::NZB_POINTED,
    &crate::archive::store::RELEASE,
    &crate::archive::store::NFO_PASSWORD,
    &crate::archive::store::PAR2_FILES,
    &crate::archive::store::SEGMENT_CRC,
    &crate::duplicates::FINGERPRINT,
    &crate::archive::link::UPLOAD_LINK,
    &crate::archive::link::UPLOAD_LINK_TRIED,
    &crate::probe::PROBE,
    &crate::probe::COVER,
    &crate::probe::opensubtitles::OPENSUBTITLES,
    &crate::probe::stills::STILLS,
    &crate::predb::step::MATCH,
    &crate::predb::step::INNER_KEY,
    &crate::category::RELEASE_CATEGORY,
    &crate::media::MEDIA_RELEASE,
    &crate::store::FILE_TIMING,
    &crate::keys::RELEASE_KEYS,
    &crate::junk::JUNK_RELEASES,
    &crate::junk::JUNK_COLLECTIONS,
];

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
