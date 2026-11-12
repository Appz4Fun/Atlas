//! Rehearsal import of Atlas's SQLite main database into a new PostgreSQL schema.
//!
//! Cursors and release references are retained as-is. They are meaningful only with the matching
//! release and header stores, so this schema is suitable for migration checks against a paired snapshot.

use std::path::Path;

use anyhow::{Context, Result, bail, ensure};
use postgres::binary_copy::{BinaryCopyInWriter, BinaryCopyOutIter};
use postgres::fallible_iterator::FallibleIterator;
use postgres::types::{ToSql, Type};
use rusqlite::types::ValueRef;
use rusqlite::{Connection, OpenFlags};
use sha2::{Digest, Sha256};

const REQUIRED: &[&str] = &[
    "groups",
    "meta",
    "backfill_chunks",
    "backfill_deepest",
    "backfill_media",
    "backfill_speculative",
    "backfill_sweep_gen",
    "backfill_sweeps",
    "backfill_wanted",
    "spot_blacklist",
    "spot_comments",
    "spot_disposals",
    "spot_reports",
    "spots_seen",
];

// These tables are created only when their respective worker starts on SQLite.
const OPTIONAL: &[&str] = &["inferred_asked", "inferred_ids", "repair_tried", "repair_older"];
const STAGING_PREFIX: &str = "atlas_stage_";

pub(crate) fn owns_table(table: &str) -> bool {
    REQUIRED.contains(&table) || OPTIONAL.contains(&table)
}

fn check_destination(database: &str, schema: &str) -> Result<()> {
    ensure!(!database.is_empty(), "a destination database name is required");
    ensure!(
        super::valid_schema(schema) && schema.starts_with(STAGING_PREFIX) && schema.len() > STAGING_PREFIX.len(),
        "destination must be a lowercase staging schema starting with {STAGING_PREFIX}"
    );
    Ok(())
}

fn quote(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

fn prepare_source_table<'source>(
    source: &'source Connection,
    table: &str,
    names: &[&str],
) -> Result<rusqlite::Statement<'source>> {
    let mut columns = source.prepare("select name from pragma_table_xinfo(?1, 'main')")?;
    for column in columns.query_map([table], |row| row.get::<_, String>(0))? {
        let column = column?;
        ensure!(
            names.contains(&column.as_str()) && !(table == "meta" && column == "text_value"),
            "unsupported SQLite column {table}.{column} would be dropped"
        );
    }
    let fields = names
        .iter()
        .copied()
        .map(|name| match (table, name) {
            ("meta", "value") => "case when typeof(source.value) <> 'text' then source.value end".into(),
            ("meta", "text_value") => "case when typeof(source.value) = 'text' then source.value end".into(),
            _ => format!("source.{}", quote(name)),
        })
        .collect::<Vec<_>>()
        .join(", ");
    Ok(source.prepare(&format!("select {fields} from main.{} as source", quote(table)))?)
}

fn source_tables(source: &Connection) -> Result<Vec<&'static str>> {
    ensure!(!crate::db::has_old_layout(source)?, "convert the pre-shard SQLite layout before importing");
    let mut exists = source.prepare("select 1 from main.sqlite_schema where type = 'table' and name = ?1")?;
    let mut tables = Vec::with_capacity(REQUIRED.len() + OPTIONAL.len());
    for &table in REQUIRED {
        ensure!(exists.exists([table])?, "required table {table} is missing from the SQLite snapshot");
        tables.push(table);
    }
    for &table in OPTIONAL {
        if exists.exists([table])? {
            tables.push(table);
        }
    }
    let next: i64 = source
        .query_row("select value from main.meta where key = 'next_seq'", [], |r| r.get(0))
        .context("reading SQLite's next_seq counter")?;
    ensure!(next > 0, "the SQLite snapshot has an invalid next_seq counter");
    Ok(tables)
}

#[derive(Clone, Copy)]
enum Kind {
    Int,
    Text,
}

impl Kind {
    fn from_postgres(ty: &Type) -> Result<Self> {
        if *ty == Type::INT8 {
            Ok(Self::Int)
        } else if *ty == Type::TEXT {
            Ok(Self::Text)
        } else {
            bail!("unsupported PostgreSQL main-table type {ty}")
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Cell {
    Null,
    Int(i64),
    Text(String),
}

impl Cell {
    fn as_sql(&self, kind: Kind) -> &(dyn ToSql + Sync) {
        match self {
            Self::Int(value) => value,
            Self::Text(value) => value,
            Self::Null => match kind {
                Kind::Int => &None::<i64>,
                Kind::Text => &None::<String>,
            },
        }
    }
}

fn read_source_row(row: &rusqlite::Row<'_>, kinds: &[Kind], values: &mut Vec<Cell>) -> Result<()> {
    values.clear();
    for (index, kind) in kinds.iter().enumerate() {
        let value = match (row.get_ref(index)?, kind) {
            (ValueRef::Null, _) => Cell::Null,
            (ValueRef::Integer(value), Kind::Int) => Cell::Int(value),
            (ValueRef::Text(value), Kind::Text) => {
                let text = std::str::from_utf8(value).context("invalid UTF-8 in SQLite snapshot")?;
                ensure!(!text.contains('\0'), "embedded NUL in SQLite text column {}", index + 1);
                Cell::Text(text.to_owned())
            }
            (other, _) => bail!("SQLite column {} has incompatible storage type {:?}", index + 1, other.data_type()),
        };
        values.push(value);
    }
    Ok(())
}

#[derive(Debug, Default, PartialEq, Eq)]
struct Tally {
    rows: u64,
    digest: u128,
}

impl Tally {
    fn add(&mut self, cells: &[Cell]) {
        let mut hash = Sha256::new();
        for cell in cells {
            match cell {
                Cell::Null => hash.update([0]),
                Cell::Int(value) => {
                    hash.update([1]);
                    hash.update(value.to_le_bytes());
                }
                Cell::Text(value) => {
                    hash.update([2]);
                    hash.update((value.len() as u64).to_le_bytes());
                    hash.update(value.as_bytes());
                }
            }
        }
        let hash = hash.finalize();
        // Addition is order-independent and retains the multiplicity of equal rows.
        self.digest = self.digest.wrapping_add(u128::from_le_bytes(hash[..16].try_into().expect("16 digest bytes")));
        self.rows += 1;
    }
}

fn copy_table(source: &Connection, target: &mut postgres::Transaction<'_>, schema: &str, table: &str) -> Result<Tally> {
    let qualified = format!("{}.{}", quote(schema), quote(table));
    let columns = target.prepare(&format!("select * from {qualified} limit 0"))?;
    let columns = columns.columns();
    let types: Vec<_> = columns.iter().map(|col| col.type_().clone()).collect();
    let kinds = types.iter().map(Kind::from_postgres).collect::<Result<Vec<_>>>()?;
    let fields = columns.iter().map(|col| quote(col.name())).collect::<Vec<_>>().join(", ");
    let names: Vec<_> = columns.iter().map(|col| col.name()).collect();
    let mut statement = prepare_source_table(source, table, &names)?;
    let mut rows = statement.query([])?;
    let mut writer =
        BinaryCopyInWriter::new(target.copy_in(&format!("copy {qualified} ({fields}) from stdin binary"))?, &types);
    let mut cells = Vec::with_capacity(columns.len());
    let mut original = Tally::default();
    while let Some(row) = rows.next()? {
        read_source_row(row, &kinds, &mut cells).with_context(|| format!("SQLite row {}", original.rows + 1))?;
        writer.write_raw(cells.iter().zip(kinds.iter().copied()).map(|(cell, kind)| cell.as_sql(kind)))?;
        original.add(&cells);
    }
    ensure!(writer.finish()? == original.rows, "PostgreSQL COPY count did not match the SQLite snapshot");

    let reader = target.copy_out(&format!("copy (select {fields} from {qualified}) to stdout binary"))?;
    let mut reader = BinaryCopyOutIter::new(reader, &types);
    let mut restored = Tally::default();
    while let Some(row) = reader.next()? {
        cells.clear();
        for (index, kind) in kinds.iter().enumerate() {
            let value = match kind {
                Kind::Int => row.try_get::<Option<i64>>(index)?.map_or(Cell::Null, Cell::Int),
                Kind::Text => row.try_get::<Option<String>>(index)?.map_or(Cell::Null, Cell::Text),
            };
            cells.push(value);
        }
        restored.add(&cells);
    }
    ensure!(restored == original, "restored rows did not match the SQLite snapshot");
    Ok(restored)
}

/// Import a read-only snapshot and verify the copied rows before making the new schema visible.
/// A failure rolls back the entire import, including schema creation.
pub fn import_snapshot(
    snapshot: &Path,
    client: &mut postgres::Client,
    database: &str,
    schema: &str,
) -> Result<Vec<(&'static str, u64)>> {
    check_destination(database, schema)?;
    let witness = super::history_companion::Witness::capture(snapshot)?;
    let mut source = Connection::open_with_flags(snapshot, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let source = source.transaction()?;
    let tables = source_tables(&source)?;

    let mut target = client.transaction()?;
    target.batch_execute("set local synchronous_commit=on")?;
    target.query_one("select pg_advisory_xact_lock_shared(hashtext('atlas.indexer'),hashtext($1))", &[&schema])?;
    let actual: String = target.query_one("select current_database()", &[])?.get(0);
    ensure!(actual == database, "connected to database {actual:?}, expected {database:?}");
    // Use the migration lock before creating the schema. CREATE without IF NOT EXISTS protects an existing schema.
    target.execute("select pg_advisory_xact_lock(hashtext('atlas.migrate.' || $1))", &[&schema])?;
    target.batch_execute(&format!("create schema {}", quote(schema))).context("creating a new staging schema")?;
    super::migrate::run_in_transaction(&mut target, schema)?;
    // The migration inserts a default next_seq for new installs. The snapshot supplies the original allocator state.
    target.batch_execute(&format!("delete from {}.meta", quote(schema)))?;
    let mut restored = Vec::with_capacity(tables.len());
    let mut relations = Vec::new();
    for table in tables {
        let tally = copy_table(&source, &mut target, schema, table).with_context(|| format!("importing {table}"))?;
        restored.push((table, tally.rows));
        relations.push(super::history_companion::relation(table, tally.rows, tally.digest));
    }
    if let Some(witness) = witness {
        witness.check(snapshot)?;
        super::history_companion::store(
            &mut target,
            super::history_companion::Receipt {
                version: 1,
                owner: "main_import".into(),
                main_source_digest: witness.digest,
                relations,
                scope: "main_compat_v1".into(),
            },
        )?;
    }
    target.commit()?;
    Ok(restored)
}

fn run(args: &[String]) -> Result<()> {
    let value = |flag: &str| {
        args.iter().skip_while(|arg| *arg != flag).nth(1).filter(|arg| !arg.starts_with("--")).map(String::as_str)
    };
    let snapshot = Path::new(value("--snapshot").context("--snapshot FILE is required")?);
    if !args.iter().any(|arg| arg == "--apply") {
        let mut source = Connection::open_with_flags(snapshot, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let source = source.transaction()?;
        for table in source_tables(&source)? {
            let count: i64 =
                source.query_row(&format!("select count(*) from main.{}", quote(table)), [], |r| r.get(0))?;
            println!("  {table}: {count} rows");
        }
        println!("Use --apply --database NAME --schema atlas_stage_NAME for a verified PostgreSQL import.");
        return Ok(());
    }
    let database = value("--database").context("--database NAME is required with --apply")?;
    let schema = value("--schema").context("--schema atlas_stage_NAME is required with --apply")?;
    check_destination(database, schema)?;
    let mut config = crate::config::storage_config();
    if let Ok(url) = std::env::var("ATLAS_RESTORE_PG_URL") {
        config.url = url;
    }
    ensure!(!config.url.is_empty(), "set ATLAS_RESTORE_PG_URL or [storage] url for the destination");
    config.schema = schema.to_owned();
    let pool = super::Pool::new(&config)?;
    let mut client = pool.get()?;
    let restored = import_snapshot(snapshot, &mut client, database, schema)?;
    for (table, rows) in &restored {
        println!("  {table}: {rows} rows verified");
    }
    println!("Imported and verified {} tables in {database}.{schema}", restored.len());
    if let Some(receipt)=super::history_companion::digest(&mut client,schema,"main_import")? {
        println!("Main source-pairing receipt: {}",receipt.iter().map(|byte|format!("{byte:02x}")).collect::<String>());
    }
    Ok(())
}

pub fn run_cli(args: &[String]) -> i32 {
    match run(args) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("main import: {error:#}");
            1
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn staging_guard_accepts_only_named_new_schema_candidates() {
        assert!(check_destination("atlas_test", "atlas_stage_rehearsal").is_ok());
        for name in ["atlas", "public", "atlas_stage_", "atlas_stage_quoted\"", "atlas_stage_CAPS"] {
            assert!(check_destination("atlas_test", name).is_err());
        }
        assert!(check_destination("", "atlas_stage_rehearsal").is_err());
    }

    #[test]
    fn sqlite_meta_splits_integers_and_text_without_coercion() {
        let c = rusqlite::Connection::open_in_memory().unwrap();
        c.execute_batch(
            "create table meta (key text primary key, value integer);
             insert into meta values ('counter', 17), ('label', 'example'), ('missing', null), ('real', 1.25);",
        )
        .unwrap();
        let mut stmt = prepare_source_table(&c, "meta", &["key", "value", "text_value"]).unwrap();
        let mut rows = stmt.query([]).unwrap();
        let mut values = Vec::new();
        let mut by_key = std::collections::BTreeMap::new();
        while let Some(row) = rows.next().unwrap() {
            let key: String = row.get(0).unwrap();
            let result = read_source_row(row, &[Kind::Text, Kind::Int, Kind::Text], &mut values);
            by_key.insert(key, result.map(|()| values[1..].to_vec()));
        }
        assert_eq!(by_key.remove("counter").unwrap().unwrap(), [Cell::Int(17), Cell::Null]);
        assert_eq!(by_key.remove("label").unwrap().unwrap(), [Cell::Null, Cell::Text("example".into())]);
        assert_eq!(by_key.remove("missing").unwrap().unwrap(), [Cell::Null, Cell::Null]);
        assert!(by_key.remove("real").unwrap().is_err());
        assert!(by_key.is_empty());
    }

    #[test]
    fn source_columns_cannot_be_silently_dropped() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "create table groups (name text primary key, live_cursor integer, future_cursor integer);
             insert into groups values ('test.group', 700, 125);",
        )
        .unwrap();
        let error = prepare_source_table(&c, "groups", &["name", "live_cursor"]).err().unwrap();
        assert!(error.to_string().contains("future_cursor"));
    }

    pub(crate) fn sqlite_snapshot(dir: &Path) -> std::path::PathBuf {
        let path = dir.join("atlas.db");
        let c = Connection::open(&path).unwrap();
        crate::store::create_main(&c).unwrap();
        c.execute_batch(
            "create table groups (name text primary key, live_cursor integer, backfill_cursor integer,
                first_article integer, last_article integer, low_article integer, low_posted integer,
                high_article integer, high_posted integer, start_posted integer);
             insert into groups (name, live_cursor, backfill_cursor) values ('test.group', 700, 125);
             update meta set value = 91 where key = 'next_seq';
             insert into meta (key, value) values ('source_uuid', 'test-source');
             insert into spots_seen (message_id, number, release_id, at) values ('test-id', 42, 88, 100);
             create table repair_older (shard integer primary key, before integer);
             insert into repair_older values (1, null);",
        )
        .unwrap();
        path
    }

    #[test]
    fn source_requires_main_state_and_accepts_unstarted_workers() {
        let dir = tempfile::tempdir().unwrap();
        let path = sqlite_snapshot(dir.path());
        let source = Connection::open(path).unwrap();
        let tables = source_tables(&source).unwrap();
        assert_eq!(tables.len(), REQUIRED.len() + 1);
        assert!(tables.contains(&"repair_older"));
        source.execute("drop table backfill_chunks", []).unwrap();
        assert!(source_tables(&source).unwrap_err().to_string().contains("backfill_chunks"));
    }

    #[test]
    fn postgres_import_preserves_cursors_meta_references_and_nullable_repair() {
        let Some(test) = crate::pg::testing::Schema::empty() else { return };
        let dir = tempfile::tempdir().unwrap();
        let path = sqlite_snapshot(dir.path());
        let schema = format!("{STAGING_PREFIX}{}", test.name);
        let mut pg = test.pool.get().unwrap();
        let database: String = pg.query_one("select current_database()", &[]).unwrap().get(0);

        assert!(import_snapshot(&path, &mut pg, "wrong_database", &schema).is_err());
        assert!(pg.query_opt("select 1 from pg_namespace where nspname = $1", &[&schema]).unwrap().is_none());
        let restored = import_snapshot(&path, &mut pg, &database, &schema).unwrap();
        assert!(restored.contains(&("groups", 1)));
        assert!(restored.contains(&("meta", 2)));
        assert!(import_snapshot(&path, &mut pg, &database, &schema).is_err());

        let row = pg
            .query_one(
                &format!(
                    "select g.live_cursor, g.backfill_cursor, n.value, t.text_value, s.release_id, r.before
                     from {schema}.groups g, {schema}.meta n, {schema}.meta t, {schema}.spots_seen s,
                          {schema}.repair_older r where n.key = 'next_seq' and t.key = 'source_uuid'"
                ),
                &[],
            )
            .unwrap();
        pg.batch_execute(&format!("drop schema {schema} cascade")).unwrap();
        assert_eq!(row.get::<_, i64>(0), 700);
        assert_eq!(row.get::<_, i64>(1), 125);
        assert_eq!(row.get::<_, i64>(2), 91);
        assert_eq!(row.get::<_, String>(3), "test-source");
        assert_eq!(row.get::<_, i64>(4), 88);
        assert_eq!(row.get::<_, Option<i64>>(5), None);
    }

    #[test]
    fn postgres_import_rolls_back_after_a_bad_row() {
        let Some(test) = crate::pg::testing::Schema::empty() else { return };
        let dir = tempfile::tempdir().unwrap();
        let path = sqlite_snapshot(dir.path());
        let source = Connection::open(&path).unwrap();
        source.execute("insert into repair_older values (2, 'not an integer')", []).unwrap();
        let schema = format!("{STAGING_PREFIX}{}", test.name);
        let mut pg = test.pool.get().unwrap();
        let database: String = pg.query_one("select current_database()", &[]).unwrap().get(0);
        let error = import_snapshot(&path, &mut pg, &database, &schema).unwrap_err();
        assert!(format!("{error:#}").contains("repair_older"));
        assert!(pg.query_opt("select 1 from pg_namespace where nspname = $1", &[&schema]).unwrap().is_none());
    }
}
