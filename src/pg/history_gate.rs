//! Native mutation gate tests and transaction entrypoint.

use crate::history_identity::{ErrorCode, HistoryError};
use anyhow::{Result, ensure};
use postgres::GenericClient;

/// The owner fence precedes the persistent gate and all group/row locks.
/// No history state currently permits native mutation, including identity_ready.
pub(crate) fn native(tx: &mut impl GenericClient) -> Result<()> {
    let row=tx.query_typed_one("select current_schema()::text,exists(select 1 from pg_catalog.pg_proc p join pg_catalog.pg_namespace n on n.oid=p.pronamespace where n.nspname=current_schema() and p.proname='atlas_native_write_guard'),to_regclass(format('%I.history_imports',current_schema())) is not null,current_setting('transaction_isolation')",&[])?;
    let schema: String = row.get(0);
    ensure!(super::valid_schema(&schema), "native mutation schema is not supported");
    // A fixed transaction snapshot cannot observe registration after waiting
    // for the owner fence, including when the guard runs through a savepoint.
    ensure!(row.get::<_, String>(3) == "read committed", "native mutation requires READ COMMITTED isolation");
    if row.get::<_, bool>(1) {
        tx.query_typed_one(
            &format!("select \"{schema}\".atlas_native_write_guard($1)"),
            &[(&schema, postgres::types::Type::TEXT)],
        )?;
    } else {
        ensure!(
            !row.get::<_, bool>(2),
            HistoryError::new(ErrorCode::IncompleteImport, "history schema is missing its native guard")
        );
        tx.query_typed_one("select pg_catalog.pg_advisory_xact_lock_shared(pg_catalog.hashtext('atlas.indexer'),pg_catalog.hashtext($1))",&[(&schema,postgres::types::Type::TEXT)])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::parser::{Article, Release};

    fn review_writer(
        client: &mut postgres::Client,
        operation: &str,
        entered: &std::sync::atomic::AtomicBool,
    ) -> std::result::Result<(), String> {
        use crate::pg::sql::{Pg, Sql};
        use std::sync::atomic::Ordering::SeqCst;
        let write = "insert into meta(key,value) values('review_native_mutation',1)";
        match operation {
            "guard" => {
                let mut tx = client.transaction().map_err(|error| format!("{error:?}"))?;
                super::native(&mut tx).map_err(|error| format!("{error:?}"))?;
                entered.store(true, SeqCst);
                tx.batch_execute(write).map_err(|error| format!("{error:?}"))?;
                tx.commit().map_err(|error| format!("{error:?}"))
            }
            "sql_callback" => {
                let pg = Pg::new(client);
                let sql: &dyn Sql = &pg;
                sql.tx(|db| {
                    entered.store(true, SeqCst);
                    db.execute(write, &[])?;
                    Ok(())
                })
                .map_err(|error| format!("{error:?}"))
            }
            "queue_begin" => {
                let mut tx = crate::pg::release_queue::begin(client).map_err(|error| format!("{error:?}"))?;
                entered.store(true, SeqCst);
                tx.batch_execute(write).map_err(|error| format!("{error:?}"))?;
                tx.commit().map_err(|error| format!("{error:?}"))
            }
            "queue_claim" => crate::pg::release_queue::claim(client, 1, 1, 30)
                .map(|_| entered.store(true, SeqCst))
                .map_err(|error| format!("{error:?}")),
            "direct_dml" => {
                let mut tx = client.transaction().map_err(|error| format!("{error:?}"))?;
                tx.query_one("select count(*) from history_imports", &[]).map_err(|error| format!("{error:?}"))?;
                tx.batch_execute(write).map_err(|error| format!("{error:?}"))?;
                entered.store(true, SeqCst);
                tx.commit().map_err(|error| format!("{error:?}"))
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn review_regression_repeatable_read_registration_cannot_admit_writers() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering::SeqCst},
            mpsc,
        };
        let mut failures = Vec::new();
        for state in ["loading", "identity_ready"] {
            for operation in ["guard", "sql_callback", "queue_begin", "queue_claim", "direct_dml"] {
                let Some(schema) = crate::pg::testing::Schema::new() else { return };
                let mut registrar = schema.pool.get().unwrap();
                registrar
                    .query_one("select pg_advisory_lock(hashtext('atlas.indexer'),hashtext($1))", &[&schema.name])
                    .unwrap();
                let (url, name) = (schema.url.clone(), schema.name.clone());
                let (pid_tx, pid_rx) = mpsc::channel();
                let (done_tx, done_rx) = mpsc::channel();
                let entered = Arc::new(AtomicBool::new(false));
                let callback = entered.clone();
                let worker = std::thread::spawn(move || {
                    let mut client = postgres::Client::connect(&url, postgres::NoTls).unwrap();
                    client.batch_execute(&format!("set search_path to \"{name}\";set default_transaction_isolation='repeatable read';set statement_timeout='5s'")).unwrap();
                    let isolation: String = client.query_one("show default_transaction_isolation", &[]).unwrap().get(0);
                    assert_eq!(isolation, "repeatable read");
                    let pid: i32 = client.query_one("select pg_backend_pid()", &[]).unwrap().get(0);
                    pid_tx.send(pid).unwrap();
                    done_tx.send(review_writer(&mut client, operation, &callback)).unwrap();
                });
                let pid = pid_rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
                let started = std::time::Instant::now();
                let mut early = None;
                let waited = loop {
                    if let Ok(done) = done_rx.try_recv() {
                        early = Some(done);
                        break false;
                    }
                    let blocked: bool = registrar.query_one(
                        "select exists(select 1 from pg_locks where pid=$1 and locktype='advisory' and not granted)", &[&pid],
                    ).unwrap().get(0);
                    if blocked {
                        break true;
                    }
                    assert!(
                        started.elapsed() < std::time::Duration::from_secs(3),
                        "writer neither rejected nor reached the fence"
                    );
                    std::thread::sleep(std::time::Duration::from_millis(10));
                };
                let id = [17u8; 32];
                registrar.execute("insert into history_imports(import_id,source_fingerprint,contract_version,state,expected_database,expected_schema,expected_role,expected_owner,reserved_next_seq,checkpoint,identity_digest) values($1,$1,1,$2,current_database(),$3,current_user,current_user,900,'',case when $2='identity_ready' then $1::bytea else null::bytea end)", &[&&id[..], &state, &schema.name]).unwrap();
                registrar
                    .query_one("select pg_advisory_unlock(hashtext('atlas.indexer'),hashtext($1))", &[&schema.name])
                    .unwrap();
                let outcome = early.unwrap_or_else(|| done_rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap());
                worker.join().unwrap();
                let rows: i64 = registrar
                    .query_one("select count(*) from meta where key='review_native_mutation'", &[])
                    .unwrap()
                    .get(0);
                println!(
                    "{}",
                    serde_json::json!({"case":"repeatable_registration_race","state":state,"operation":operation,"waited":waited,"admitted":outcome.is_ok(),"entered":entered.load(SeqCst),"native_rows":rows,"error":outcome.as_ref().err()})
                );
                if outcome.is_ok() || entered.load(SeqCst) || rows != 0 {
                    failures.push(format!(
                        "{state}/{operation}: admitted={},entered={},rows={rows}",
                        outcome.is_ok(),
                        entered.load(SeqCst)
                    ));
                } else {
                    assert!(
                        outcome.unwrap_err().contains("READ COMMITTED"),
                        "rejection must identify unsupported writer isolation"
                    );
                }
            }
        }
        assert!(failures.is_empty(), "fixed-snapshot writers crossed registration: {failures:?}");
    }

    #[test]
    fn review_regression_nested_fixed_snapshot_callback_is_rejected() {
        use crate::pg::sql::{Pg, Sql};
        for isolation in [postgres::IsolationLevel::RepeatableRead, postgres::IsolationLevel::Serializable] {
            let Some(schema) = crate::pg::testing::Schema::new() else { return };
            let mut client = schema.pool.get().unwrap();
            let mut outer = client.build_transaction().isolation_level(isolation).start().unwrap();
            outer.query_one("select count(*) from history_imports", &[]).unwrap();
            let pg = Pg::new(outer);
            let sql: &dyn Sql = &pg;
            let mut entered = false;
            let result = sql.tx(|_| {
                entered = true;
                Ok(())
            });
            assert!(result.is_err() && !entered, "nested fixed-snapshot transaction admitted a writer callback");
            assert!(format!("{:?}", result.unwrap_err()).contains("READ COMMITTED"));
            pg.into_inner().rollback().unwrap();
        }
    }

    #[test]
    fn review_regression_native_only_fallback_rejects_fixed_snapshot_callback() {
        use crate::pg::sql::{Pg, Sql};
        let Some(schema) = crate::pg::testing::Schema::new() else { return };
        let mut client = schema.pool.get().unwrap();
        client.batch_execute("drop function atlas_native_write_guard(text) cascade;drop table history_imports cascade;set default_transaction_isolation='repeatable read'").unwrap();
        let pg = Pg::new(client);
        let sql: &dyn Sql = &pg;
        let mut entered = false;
        let result = sql.tx(|_| {
            entered = true;
            Ok(())
        });
        assert!(result.is_err() && !entered, "native-only fallback bypassed writer isolation admission");
    }

    #[test]
    fn a_missing_gate_function_is_never_native_only_fallback() {
        let Some(s) = crate::pg::testing::Schema::new() else { return };
        let mut c = s.pool.get().unwrap();
        c.batch_execute("drop function atlas_native_write_guard(text)").unwrap();
        let mut tx = c.transaction().unwrap();
        assert!(
            super::native(&mut tx).is_err(),
            "a history schema missing its gate must fail before callers lock rows"
        );
    }

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
