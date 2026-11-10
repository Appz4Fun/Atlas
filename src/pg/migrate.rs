//! The schema's versions, applied in order, each in its own transaction. `schema_migrations` notes each one
//! applied; an advisory lock keyed on the schema lets only one process migrate at a time, so two starting at once
//! don't both apply the same version. A migration is never edited once released: a change is a new version.

use postgres::Client;

/// Every migration, in order: (version, name, SQL).
pub const MIGRATIONS: &[(i32, &str, &str)] = &[
    (1, "main", include_str!("migrations/0001_main.sql")),
    (2, "header_blocks", include_str!("migrations/0002_header_blocks.sql")),
    (3, "release_writer", include_str!("migrations/0003_release_writer.sql")),
    (4, "main_compat", include_str!("migrations/0004_main_compat.sql")),
];

/// Bring the schema `schema` up to the last migration. Returns the versions applied now.
pub fn run(c: &mut Client, schema: &str) -> Result<Vec<i32>, postgres::Error> {
    let mut tx = c.transaction()?;
    let applied = run_in_transaction(&mut tx, schema)?;
    tx.commit()?;
    Ok(applied)
}

/// Apply migrations inside a caller's transaction so a staging import can commit schema and rows together.
pub(super) fn run_in_transaction(
    tx: &mut postgres::Transaction<'_>,
    schema: &str,
) -> Result<Vec<i32>, postgres::Error> {
    assert!(super::valid_schema(schema), "schema {schema:?}");
    // held to the end of this transaction: a second process waits here, then finds the versions applied
    tx.execute("select pg_advisory_xact_lock(hashtext('atlas.migrate.' || $1))", &[&schema])?;
    // only made when missing: `create schema if not exists` needs the right to create schemas in the database even
    // when it's there, which a role given a schema made for it by hand hasn't got
    if tx.query_opt("select 1 from pg_namespace where nspname = $1", &[&schema])?.is_none() {
        tx.batch_execute(&format!("create schema \"{schema}\""))?;
    }
    tx.batch_execute(&format!(
        "set local search_path to \"{schema}\";
         create table if not exists schema_migrations (
             version    integer primary key,
             name       text not null,
             applied_at timestamptz not null default now()
         );"
    ))?;
    let done: Vec<i32> = tx.query("select version from schema_migrations", &[])?.iter().map(|r| r.get(0)).collect();
    let mut applied = Vec::new();
    for &(version, name, sql) in MIGRATIONS {
        if done.contains(&version) {
            continue;
        }
        tx.batch_execute(sql)?;
        tx.execute("insert into schema_migrations (version, name) values ($1, $2)", &[&version, &name])?;
        applied.push(version);
    }
    Ok(applied)
}

/// The highest version applied to `schema`, 0 for none.
pub fn version(c: &mut Client, schema: &str) -> Result<i32, postgres::Error> {
    let has = c.query_one(
        "select exists (select 1 from information_schema.tables where table_schema = $1 and table_name = 'schema_migrations')",
        &[&schema],
    )?;
    if !has.get::<_, bool>(0) {
        return Ok(0);
    }
    let row = c.query_one(&format!("select coalesce(max(version), 0) from \"{schema}\".schema_migrations"), &[])?;
    Ok(row.get(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_are_in_order_and_unique() {
        for w in MIGRATIONS.windows(2) {
            assert!(w[0].0 < w[1].0, "{} then {}", w[0].0, w[1].0);
        }
        assert_eq!(MIGRATIONS[0].0, 1);
    }

    #[test]
    fn migrating_twice_applies_each_version_once() {
        let Some(t) = crate::pg::testing::Schema::empty() else { return };
        let mut c = t.pool.get().unwrap();
        assert_eq!(version(&mut c, &t.name).unwrap(), 0);
        let all: Vec<i32> = MIGRATIONS.iter().map(|m| m.0).collect();
        assert_eq!(run(&mut c, &t.name).unwrap(), all);
        assert!(run(&mut c, &t.name).unwrap().is_empty());
        assert_eq!(version(&mut c, &t.name).unwrap(), *all.last().unwrap());
        // the main tables are there, constraints included
        c.execute("insert into groups (name) values ('a.b.c')", &[]).unwrap();
        assert!(c.execute("insert into groups (name) values ('a.b.c')", &[]).is_err(), "a group's name is unique");
    }

    #[test]
    fn two_processes_migrating_at_once_both_succeed() {
        let Some(t) = crate::pg::testing::Schema::empty() else { return };
        let threads: Vec<_> = (0..4)
            .map(|_| {
                let (url, name) = (t.url.clone(), t.name.clone());
                std::thread::spawn(move || {
                    let mut c = postgres::Client::connect(&url, postgres::NoTls).unwrap();
                    run(&mut c, &name).unwrap().len()
                })
            })
            .collect();
        let applied: usize = threads.into_iter().map(|h| h.join().unwrap()).sum();
        assert_eq!(applied, MIGRATIONS.len(), "each version applied by exactly one of them");
    }
}
