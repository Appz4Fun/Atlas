//! Native mutation gate tests and transaction entrypoint.

use anyhow::{Result, ensure};
use postgres::GenericClient;

/// The owner fence precedes the persistent gate and all group/row locks.
/// No history state currently permits native mutation, including identity_ready.
pub(crate) fn native(tx: &mut impl GenericClient) -> Result<()> {
    let row=tx.query_one("select current_schema()::text,exists(select 1 from pg_catalog.pg_class c join pg_catalog.pg_namespace n on n.oid=c.relnamespace where n.nspname=current_schema() and c.relname='history_imports' and c.relkind='r')",&[])?;
    let schema: String = row.get(0);
    let exists: bool = row.get(1);
    ensure!(super::valid_schema(&schema), "native mutation schema is not supported");
    if exists {
        ensure!(
            tx.query_opt(&format!("select 1 from \"{schema}\".history_imports limit 1"), &[])?.is_none(),
            "historical import blocks native mutations"
        );
    }
    tx.query_one(
        "select pg_catalog.pg_advisory_xact_lock_shared(pg_catalog.hashtext('atlas.indexer'),pg_catalog.hashtext($1))",
        &[&schema],
    )?;
    if exists {
        ensure!(
            tx.query_opt(&format!("select 1 from \"{schema}\".history_imports for share limit 1"), &[])?.is_none(),
            "historical import blocks native mutations"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::parser::{Article, Release};

    #[test]
    fn registered_history_blocks_direct_save_and_queue() {
        let Some(s) = crate::pg::testing::Schema::new() else { return };
        let mut c = s.pool.get().unwrap();
        c.batch_execute("insert into history_imports(import_id,source_fingerprint,contract_version,state,expected_database,expected_schema,expected_role,expected_owner,reserved_next_seq,checkpoint) values(decode(repeat('01',32),'hex'),decode(repeat('01',32),'hex'),1,'loading','test','test','test','test',900,'')").unwrap();
        let release = Release {
            name: "synthetic".into(),
            group: "test.group".into(),
            articles: vec![Article {
                filename: Some("file.rar".into()),
                message_id: "<part@example.test>".into(),
                bytes: 11,
                ..Default::default()
            }],
            ..Default::default()
        };
        let result = crate::pg::release_writer::save(&mut c, &[release], 1);
        assert!(result.is_err(), "loading history must reject a direct native save");
        assert!(
            crate::pg::release_queue::claim(&mut c, 1, 1, 2).is_err(),
            "loading history must reject queue mutation even with no work"
        );
    }
}
