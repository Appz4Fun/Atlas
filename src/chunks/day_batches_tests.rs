//! Real PostgreSQL contracts for bounded day insertion; no elapsed-time oracle.
use super::*;
use crate::pg::sql::{Dialect, Pg, Row, Value};
use std::cell::Cell;

/// Count application statements while preserving the real connection and transaction.
/// Driver BEGIN/COMMIT and the native history gate are outside this counter.
struct CountSql<'a> {
    inner: &'a dyn Sql,
    calls: &'a Cell<usize>,
    max_params: &'a Cell<usize>,
    max_sql_bytes: &'a Cell<usize>,
}

impl CountSql<'_> {
    fn note(&self, sql: &str, params: &[Value]) {
        self.calls.set(self.calls.get() + 1);
        self.max_params.set(self.max_params.get().max(params.len()));
        self.max_sql_bytes.set(self.max_sql_bytes.get().max(sql.len()));
    }
}

impl Sql for CountSql<'_> {
    fn dialect(&self) -> Dialect {
        self.inner.dialect()
    }

    fn execute(&self, sql: &str, params: &[Value]) -> Result<usize> {
        self.note(sql, params);
        self.inner.execute(sql, params)
    }

    fn query(&self, sql: &str, params: &[Value]) -> Result<Vec<Row>> {
        self.note(sql, params);
        self.inner.query(sql, params)
    }

    fn batch(&self, sql: &str) -> Result<()> {
        self.note(sql, &[]);
        self.inner.batch(sql)
    }

    fn has_table(&self, table: &str) -> Result<bool> {
        self.calls.set(self.calls.get() + 1);
        self.inner.has_table(table)
    }

    fn transaction(&self, f: &mut dyn FnMut(&dyn Sql) -> Result<()>) -> Result<()> {
        self.inner.transaction(&mut |inner| {
            f(&CountSql { inner, calls: self.calls, max_params: self.max_params, max_sql_bytes: self.max_sql_bytes })
        })
    }
}

#[test]
#[ignore = "requires explicitly admitted isolated PostgreSQL"]
fn historical_days_use_four_bounded_statements_and_keep_existing_chunks() {
    let schema = crate::pg::testing::Schema::new().expect("explicit ATLAS_PG_TEST_URL is required");
    let db = Pg::new(schema.pool.get().unwrap());
    db.batch(
        "insert into backfill_chunks (grp,day,state,server,claimed_at,done_at,failures,failed_at,reread) values
         ('history',20000,1,'claimed-owner',101,null,3,91,1),
         ('history',19999,2,'done-owner',81,99,2,71,1),
         ('other',5,1,'other-owner',61,null,4,51,1);
         create temp table day_insert_order (ordinal bigserial,day bigint not null);
         create function pg_temp.record_day_insert() returns trigger language plpgsql as $$
         begin insert into pg_temp.day_insert_order(day) values(new.day);return new;end $$;
         create trigger test_day_insert_order before insert on backfill_chunks
         for each row execute function pg_temp.record_day_insert();
         create temp table day_insert_sizes (ordinal bigserial,inserted bigint not null);
         create function pg_temp.record_day_statement() returns trigger language plpgsql as $$
         begin insert into pg_temp.day_insert_sizes(inserted) select count(*) from inserted_days;return null;end $$;
         create trigger test_day_statement after insert on backfill_chunks referencing new table as inserted_days
         for each statement execute function pg_temp.record_day_statement();",
    )
    .unwrap();
    let existing = db.query("select * from backfill_chunks order by grp,day", vals![]).unwrap();
    let (calls, max_params, max_sql_bytes) = (Cell::new(0), Cell::new(0), Cell::new(0));
    let counted = CountSql { inner: &db, calls: &calls, max_params: &max_params, max_sql_bytes: &max_sql_bytes };
    // Exactly 6,290 inclusive days; two already exist with metadata that must survive.
    assert_eq!(add(&counted, "history", 20_289, 14_000).unwrap(), 6_288);
    let statements = calls.get();
    let preserved = db
        .query("select * from backfill_chunks where (grp='history' and day in(19999,20000)) or grp='other' order by grp,day", vals![])
        .unwrap();
    assert_eq!(preserved, existing, "conflicts must not rewrite state, claims, completion, failures or reread");
    let metric = (&db as &dyn Sql)
        .row("select total,done,oldest_day,newest_day,oldest_done_day from native_metric_chunk_groups where grp='history'", vals![])
        .unwrap()
        .unwrap();
    assert_eq!(
        metric.0,
        vec![Value::Int(6290), Value::Int(1), Value::Int(14000), Value::Int(20289), Value::Int(19999)]
    );
    let order: Vec<i64> =
        (&db as &dyn Sql).column("select day from pg_temp.day_insert_order order by ordinal", vals![]).unwrap();
    assert_eq!(order, (14_000..=20_289).rev().collect::<Vec<_>>(), "insertion attempts remain newest first");
    assert_eq!(statements, 4, "6,290 days must use four statements of at most 2,048 days, rather than one per day");
    let sizes: Vec<i64> =
        (&db as &dyn Sql).column("select inserted from pg_temp.day_insert_sizes order by ordinal", vals![]).unwrap();
    assert_eq!(sizes, [2046, 2048, 2048, 146], "conflicting days consume their place in each bounded day batch");
    assert!(
        max_params.get() <= 4 && max_sql_bytes.get() <= 1024,
        "day count must not expand the client SQL or parameter payload"
    );
    calls.set(0);
    assert_eq!(add(&counted, "history", 20_289, 14_000).unwrap(), 0, "all conflicts add nothing");
    assert_eq!(calls.get(), 4);
}

#[test]
#[ignore = "requires explicitly admitted isolated PostgreSQL"]
fn day_ranges_keep_empty_and_extreme_boundaries_without_overflow() {
    let schema = crate::pg::testing::Schema::new().expect("explicit ATLAS_PG_TEST_URL is required");
    let db = Pg::new(schema.pool.get().unwrap());
    let (calls, max_params, max_sql_bytes) = (Cell::new(0), Cell::new(0), Cell::new(0));
    let counted = CountSql { inner: &db, calls: &calls, max_params: &max_params, max_sql_bytes: &max_sql_bytes };
    let sql = &counted as &dyn Sql;
    assert_eq!(sql.tx(|tx| add_days(tx, "empty", i64::MIN, i64::MIN)).unwrap(), 0);
    assert_eq!(sql.tx(|tx| add_days(tx, "inverted", i64::MAX, i64::MIN)).unwrap(), 0);
    assert_eq!(calls.get(), 0, "empty and inverted half-open ranges issue no data statement");
    assert_eq!(add(&db, "public-inverted", 20, 21).unwrap(), 0);
    assert_eq!(add(&db, "low", i64::MIN, i64::MIN).unwrap(), 1);
    assert_eq!(
        add(&db, "high", i64::MAX, i64::MAX).unwrap(),
        1,
        "inclusive newest day must not overflow its upper bound"
    );
    assert_eq!(oldest_day(&db, "low").unwrap(), Some(i64::MIN));
    assert_eq!(newest_day(&db, "high").unwrap(), Some(i64::MAX));
}

#[test]
#[ignore = "requires explicitly admitted isolated PostgreSQL"]
fn failure_in_the_last_day_batch_rolls_back_all_earlier_batches_and_metrics() {
    let schema = crate::pg::testing::Schema::new().expect("explicit ATLAS_PG_TEST_URL is required");
    let db = Pg::new(schema.pool.get().unwrap());
    add(&db, "other", 5, 5).unwrap();
    db.batch(
        "create function pg_temp.refuse_oldest_day() returns trigger language plpgsql as $$
         begin if new.grp='rollback' and new.day=14000 then raise exception 'fixture day refused';end if;return new;end $$;
         create trigger test_refuse_oldest_day before insert on backfill_chunks
         for each row execute function pg_temp.refuse_oldest_day();",
    )
    .unwrap();
    assert!(add(&db, "rollback", 20_289, 14_000).is_err());
    assert!(!is_split(&db, "rollback").unwrap(), "earlier batches remain in the same outer transaction");
    assert_eq!(progress(&db, "other").unwrap(), (0, 1));
    let sql = &db as &dyn Sql;
    assert!(!sql.exists("select 1 from native_metric_chunk_groups where grp='rollback'", vals![]).unwrap());
    assert!(!sql.exists("select 1 from native_metric_chunk_touched", vals![]).unwrap());
}

#[test]
#[ignore = "requires explicitly admitted isolated PostgreSQL"]
fn reaching_back_with_batched_days_invalidates_the_old_claim_and_sweep_generation() {
    let schema = crate::pg::testing::Schema::new().expect("explicit ATLAS_PG_TEST_URL is required");
    let db = Pg::new(schema.pool.get().unwrap());
    add(&db, "reach", 20_290, 20_290).unwrap();
    let claim = claim(&db, &[("reach".to_string(), i64::MIN)], "owner", 1000).unwrap().unwrap();
    set_deepest(&db, "reach", "owner", 20_290 * 86_400 + 3600).unwrap();
    set_swept(&db, "reach", "sweeper", 0).unwrap();
    set_swept(&db, "other", "sweeper", 0).unwrap();
    assert_eq!(reach_back(&db, "reach", 14_000, 14_000 * 86_400).unwrap(), 6_291);
    assert_eq!(deepest(&db, "reach").unwrap(), None);
    assert!(!finish(&db, &claim, 2000).unwrap(), "the invalidated oldest claim cannot finish");
    assert!(!release(&db, &claim).unwrap());
    assert!(!swept(&db, "reach", "sweeper").unwrap());
    assert!(swept(&db, "other", "sweeper").unwrap());
    assert_eq!(sweep_generation(&db, "reach").unwrap(), 1);
    set_swept(&db, "reach", "stale-sweeper", 0).unwrap();
    assert!(!swept(&db, "reach", "stale-sweeper").unwrap());
    assert_eq!(progress(&db, "reach").unwrap(), (0, 6291));
    let row = (&db as &dyn Sql)
        .row("select state,server,claimed_at,done_at from backfill_chunks where grp='reach' and day=20290", vals![])
        .unwrap()
        .unwrap();
    assert_eq!(row.0, vec![Value::Int(PENDING), Value::Null, Value::Null, Value::Null]);
    set_speculative(&db, "reach", &Speculative { runner_up_day: 14_010, reach: 10, days: 0, articles: 0 }).unwrap();
    set_swept(&db, "reach", "current-sweeper", 1).unwrap();
    assert_eq!(reach_back(&db, "reach", 13_000, 13_000 * 86_400).unwrap(), 0, "the speculative floor fences the range");
    assert!(swept(&db, "reach", "current-sweeper").unwrap(), "a fenced no-op retains current sweeps");
}
