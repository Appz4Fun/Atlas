//! Native mutation gate tests and transaction entrypoint.

use anyhow::{Result, ensure};
use postgres::GenericClient;

/// The owner fence precedes the persistent gate and all group/row locks.
/// No history state currently permits native mutation, including identity_ready.
pub(crate) fn native(tx: &mut impl GenericClient) -> Result<()> {
    let row=tx.query_one("select current_schema()::text,exists(select 1 from pg_catalog.pg_proc p join pg_catalog.pg_namespace n on n.oid=p.pronamespace where n.nspname=current_schema() and p.proname='atlas_native_write_guard')",&[])?;
    let schema: String = row.get(0);
    ensure!(super::valid_schema(&schema), "native mutation schema is not supported");
    if row.get::<_, bool>(1) {
        tx.query_one(&format!("select \"{schema}\".atlas_native_write_guard($1)"), &[&schema])?;
    } else {
        tx.query_one("select pg_catalog.pg_advisory_xact_lock_shared(pg_catalog.hashtext('atlas.indexer'),pg_catalog.hashtext($1))",&[&schema])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::parser::{Article, Release};

    #[test]
    fn sql_transaction_checks_readiness_before_callers_can_lock_rows() {
        use crate::pg::sql::{Pg, Sql};
        let Some(s) = crate::pg::testing::Schema::new() else { return };
        let mut c = s.pool.get().unwrap();
        c.batch_execute("insert into history_imports(import_id,source_fingerprint,contract_version,state,expected_database,expected_schema,expected_role,expected_owner,reserved_next_seq,checkpoint) values(decode(repeat('01',32),'hex'),decode(repeat('01',32),'hex'),1,'loading','test','test','test','test',900,'')").unwrap();
        let pg = Pg::new(c);
        let sql: &dyn Sql = &pg;
        let mut called = false;
        let result = sql.tx(|_| {
            called = true;
            Ok(())
        });
        assert!(result.is_err() && !called, "persistent history gate must run before the transaction callback");
    }

    #[test]
    fn waiting_native_transaction_rechecks_registration_before_row_locks() {
        let Some(s) = crate::pg::testing::Schema::new() else { return };
        let mut owner = s.pool.get().unwrap();
        owner.query_one("select pg_advisory_lock(hashtext('atlas.indexer'),hashtext($1))", &[&s.name]).unwrap();
        let url = s.url.clone();
        let name = s.name.clone();
        let (pid_tx, pid_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let mut native = postgres::Client::connect(&url, postgres::NoTls).unwrap();
            native.batch_execute(&format!("set search_path to \"{name}\";set statement_timeout='5s'")).unwrap();
            let pid: i32 = native.query_one("select pg_backend_pid()", &[]).unwrap().get(0);
            pid_tx.send(pid).unwrap();
            let mut tx = native.transaction().unwrap();
            let rejected = super::native(&mut tx).is_err();
            done_tx.send(rejected).unwrap();
        });
        let pid = pid_rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        let started = std::time::Instant::now();
        loop {
            let waiting: bool = owner
                .query_one(
                    "select exists(select 1 from pg_locks where pid=$1 and locktype='advisory' and not granted)",
                    &[&pid],
                )
                .unwrap()
                .get(0);
            if waiting {
                break;
            }
            assert!(
                started.elapsed() < std::time::Duration::from_secs(3),
                "native transaction did not reach owner fence"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        owner.batch_execute("insert into history_imports(import_id,source_fingerprint,contract_version,state,expected_database,expected_schema,expected_role,expected_owner,reserved_next_seq,checkpoint) values(decode(repeat('01',32),'hex'),decode(repeat('01',32),'hex'),1,'loading','test','test','test','test',900,'')").unwrap();
        owner.query_one("select pg_advisory_unlock(hashtext('atlas.indexer'),hashtext($1))", &[&s.name]).unwrap();
        assert!(
            done_rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap(),
            "post-wait registration must be observed"
        );
        worker.join().unwrap();
    }

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
