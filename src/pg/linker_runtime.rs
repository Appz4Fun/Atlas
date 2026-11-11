//! Native linker phase driver. Every database operation uses the background
//! owner's existing session; no auxiliary pool or unfenced writer is created.
use super::linker_stage::Budget;
use crate::indexer::{Db, on_native_db, on_native_db_sync};
use crate::nxv_store::{Store, store::LinkerRetentionPin};
use anyhow::{Result, bail};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

static ACTIVE: AtomicBool = AtomicBool::new(false);
pub(crate) fn active() -> bool {
    ACTIVE.load(Ordering::Acquire)
}
struct Guard(Arc<Store>);
impl Drop for Guard {
    fn drop(&mut self) {
        // Clear the closure too: it owns a Db clone and must not retain the
        // advisory-lock session after the background owner stops.
        self.0.set_linker_coverage(None);
        ACTIVE.store(false, Ordering::Release);
    }
}

/// One bounded client step. SQL may externally sort arbitrarily large input;
/// only scalar IDs and the configured row/byte page reach this process.
pub(crate) async fn step(
    db: &Db,
    store: &Arc<Store>,
    epoch: i64,
    budget: Budget,
    cache: &mut super::linker_source::Cache,
) -> Result<bool> {
    let phase = on_native_db(db, move |c| {
        Ok(c.query_one("select phase from linker_epochs where id=$1", &[&epoch])?.get::<_, String>(0))
    })
    .await?;
    match phase.as_str() {
        "stage" => {
            if super::linker_source::step(db, store, epoch, budget, cache).await? {
                on_native_db(db, move |c| super::linker_epoch::scope(c, epoch)).await?;
            }
        }
        "select" => on_native_db(db, move |c| super::linker_select::select(c, epoch)).await?,
        "files" => {
            on_native_db(db, move |c| {
                super::linker_plan::prepare(c, epoch)?;
                super::linker_files::infer(c, epoch)
            })
            .await?
        }
        "chain" => {
            on_native_db(db, move |c| super::linker::plan_files(c, epoch, 32)).await?;
        }
        "plan" => {
            on_native_db(db, move |c| {
                if !super::linker_plan::late_one(c, epoch)? {
                    super::linker_plan::chain_one(c, epoch)?;
                }
                Ok(())
            })
            .await?;
        }
        "emit" => {
            super::linker_emit::emit_page(db, epoch, budget).await?;
        }
        "ack" => on_native_db(db, move |c| super::linker_plan::ack(c, epoch)).await?,
        "done" => return Ok(true),
        _ => bail!("unknown native linker phase"),
    }
    Ok(false)
}

async fn pause(stopping: &impl Fn() -> bool) {
    for _ in 0..60 {
        if stopping() {
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

pub(crate) async fn run(db: Db, store: Arc<Store>, stopping: impl Fn() -> bool) {
    let mut pin: Option<LinkerRetentionPin> = Some(store.pin_linker_retention());
    ACTIVE.store(true, Ordering::Release);
    let _guard = Guard(Arc::clone(&store));
    let coverage_db = db.clone();
    store.set_linker_coverage(Some(Arc::new(move |group, day, generation, count| {
        on_native_db_sync(&coverage_db, |c| super::linker_stage::covered(c, group, day, generation, count))
    })));
    let mut imported = false;
    let mut current = None;
    let mut source_cache = super::linker_source::Cache::default();
    while !stopping() {
        let result: Result<bool> = async {
            if !imported {
                let dir = store.root().join("_state");
                on_native_db(&db, move |c| super::linker_bootstrap::import(c, &dir, chrono::Utc::now().timestamp()))
                    .await?;
                imported = true;
            }
            if current.is_none()
                && on_native_db(&db, |c| super::linker_gc::step(c, chrono::Utc::now().timestamp(), 256)).await?
            {
                return Ok(false);
            }
            let epoch = match current {
                Some(epoch) => epoch,
                None => {
                    if pin.is_none() {
                        pin = Some(store.pin_linker_retention());
                    }
                    let epoch =
                        on_native_db(&db, move |c| super::linker_epoch::open(c, chrono::Utc::now().timestamp()))
                            .await?;
                    current = Some(epoch);
                    epoch
                }
            };
            let done = step(&db, &store, epoch, Budget::default(), &mut source_cache).await?;
            let staged = on_native_db(&db, move |c| {
                Ok(c.query_one("select phase<>'stage' from linker_epochs where id=$1", &[&epoch])?.get::<_, bool>(0))
            })
            .await?;
            if staged {
                // Every captured block is durable, including future/delayed
                // rows. Expiry now consults that exact generation's coverage.
                pin.take();
                store.arm_expiry();
            }
            Ok(done)
        }
        .await;
        match result {
            Ok(true) => {
                source_cache = super::linker_source::Cache::default();
                let completed = current.take().expect("completed native epoch");
                let progressed=on_native_db(&db,move |c| Ok(c.query_one(
                    "select planned_rows>0 or exists(select 1 from linker_epoch_hours where epoch=$1) from linker_epochs where id=$1",
                    &[&completed])?.get::<_,bool>(0))).await;
                match progressed {
                    Ok(true) => tokio::task::yield_now().await,
                    Ok(false) => pause(&stopping).await,
                    Err(error) => {
                        eprintln!("native header linker: could not read completed progress: {error:#}");
                        pause(&stopping).await;
                    }
                }
            }
            Ok(false) => tokio::task::yield_now().await,
            Err(error) => {
                eprintln!("native header linker: {error:#}; durable phase retained for retry");
                pause(&stopping).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nxv_store::store::{Clamp, KeepSet, StoreConfig};

    #[test]
    fn unknown_generation_same_and_larger_counts_reconcile_unpublished_ids_once() {
        for blocks in [1, 2] {
            let Some(schema) = crate::pg::testing::Schema::new() else { return };
            let dir = tempfile::tempdir().unwrap();
            let (store, _) = Store::open(
                StoreConfig { sealers: 1, ..StoreConfig::new(dir.path()) },
                KeepSet { live_from: i64::MIN / 2, ..KeepSet::default() },
            )
            .unwrap();
            let date = chrono::DateTime::from_timestamp(99000, 0).unwrap().format("%a, %d %b %Y %H:%M:%S +0000");
            let lines: Vec<_> = (1..=13)
                .map(|n| {
                    format!("{n}\tpart{n}\tposter@example.invalid\t{date}\t<part{n}@ngPost>\t\t700000\t5000")
                        .into_bytes()
                })
                .collect();
            let ticket = store.record("server.example", "a.example", &lines, Clamp::Unknown { now: 200000 }).unwrap();
            store.tick().unwrap();
            ticket.wait(Duration::from_secs(5)).unwrap();
            store.flush_days(&[1], Duration::from_secs(5)).unwrap();
            if blocks == 2 {
                let row =
                    format!("100\tordinary text\tother@example.invalid\t{date}\t<ordinary@example.invalid>\t\t100\t1")
                        .into_bytes();
                let ticket =
                    store.record("server.example", "a.example", &[row], Clamp::Unknown { now: 200000 }).unwrap();
                store.tick().unwrap();
                ticket.wait(Duration::from_secs(5)).unwrap();
            }
            let duplicate: Vec<_> = lines
                .iter()
                .map(|line| String::from_utf8(line.clone()).unwrap().replace("700000", "800000").into_bytes())
                .collect();
            let ticket =
                store.record("second.example", "b.example", &duplicate, Clamp::Unknown { now: 200000 }).unwrap();
            store.tick().unwrap();
            ticket.wait(Duration::from_secs(5)).unwrap();
            store.close().unwrap();
            assert_eq!(store.segment_snapshot("a.example", 1).unwrap().count, blocks);
            let state = dir.path().join("_state");
            std::fs::create_dir_all(&state).unwrap();
            std::fs::write(state.join("linker.rec"), "L\t-14\t100\nC\t1\t1\ta.example\nE\n").unwrap();
            let hash = nntp_xover_engine::msgid_index::msgid_hash(b"<part1@ngPost>");
            std::fs::write(state.join("links.rec"), format!("I\t120000\t{hash:x}\n")).unwrap();
            let database: String =
                schema.pool.get().unwrap().query_one("select current_database()", &[]).unwrap().get(0);
            let db = crate::indexer::shared_postgres_db(
                &crate::pg::StorageConfig {
                    backend: crate::pg::Backend::Postgres,
                    url: schema.url.clone(),
                    schema: schema.name.clone(),
                    allow_plaintext: true,
                    ..Default::default()
                },
                &database,
                crate::indexer::PgWriteLimits::default(),
            )
            .unwrap();
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            rt.block_on(async {
                let mut source_cache = super::super::linker_source::Cache::default();
                let source_dir = state.clone();
                on_native_db(&db, move |c| super::super::linker_bootstrap::import(c, &source_dir, 120000))
                    .await
                    .unwrap();
                for _ in 0..2 {
                    let epoch = on_native_db(&db, |c| super::super::linker_epoch::open(c, 120000)).await.unwrap();
                    let _pin = store.pin_linker_retention();
                    for turn in 0..200 {
                        assert!(turn < 199, "reconciliation stopped making progress");
                        let resumed = on_native_db(&db, |c| super::super::linker_epoch::open(c, 130000)).await.unwrap();
                        assert_eq!(resumed, epoch);
                        step(&db, &store, epoch, Budget { rows: 2, bytes: 1024 }, &mut source_cache).await.unwrap();
                        let done = on_native_db(&db, move |c| {
                            Ok(c.query_one("select phase='done' from linker_epochs where id=$1", &[&epoch])?
                                .get::<_, bool>(0))
                        })
                        .await
                        .unwrap();
                        if done {
                            break;
                        }
                    }
                }
            });
            let mut c = schema.pool.get().unwrap();
            assert_eq!(c.query_one("select count(*) from segments", &[]).unwrap().get::<_, i64>(0), 12);
            assert_eq!(c.query_one("select count(*) from linker_staged_rows s join linker_source_blocks b on b.id=s.source where s.published and b.group_name='a.example'",&[]).unwrap().get::<_,i64>(0),12);
            assert_eq!(c.query_one("select count(*) from linker_linked_ids", &[]).unwrap().get::<_, i64>(0), 12);
            let generation = c.query_one("select min(generation),max(generation) from linker_uploads", &[]).unwrap();
            assert_eq!((generation.get::<_, i64>(0), generation.get::<_, i64>(1)), (0, 0));
        }
    }

    #[test]
    fn tiny_pages_resume_every_phase_and_publish_mixed_block_future_rows_later() {
        let Some(schema) = crate::pg::testing::Schema::new() else { return };
        let dir = tempfile::tempdir().unwrap();
        let (store, _) = Store::open(
            StoreConfig { sealers: 1, ..StoreConfig::new(dir.path()) },
            KeepSet { live_from: i64::MIN / 2, ..KeepSet::default() },
        )
        .unwrap();
        let mut lines = Vec::new();
        for n in 1..=24 {
            let (posted, poster) =
                if n <= 12 { (99000, "current@example.invalid") } else { (144000, "future@example.invalid") };
            let date = chrono::DateTime::from_timestamp(posted, 0).unwrap().format("%a, %d %b %Y %H:%M:%S +0000");
            lines.push(format!("{n}\tpart{n}\t{poster}\t{date}\t<part{n}@ngPost>\t\t740000\t5692").into_bytes());
        }
        let ticket = store.record("server.example", "group.example", &lines, Clamp::Unknown { now: 250000 }).unwrap();
        store.tick().unwrap();
        ticket.wait(Duration::from_secs(5)).unwrap();
        store.close().unwrap();
        let legacy_dir = tempfile::tempdir().unwrap();
        let mut legacy = crate::nxv_store::linker::Linker::open_in(legacy_dir.path(), 120000).unwrap();
        legacy.state.linked.extend((-14..=100).filter(|h| ![27, 40].contains(h)));
        let mut golden = Vec::new();
        for now in [120000, 250000] {
            let found = legacy.pass(&store, now, &Default::default(), &|| false).unwrap().unwrap();
            golden.extend(found.releases.iter().cloned());
            legacy.ack(found).unwrap();
        }
        let mut actual = Vec::new();
        let database: String = schema.pool.get().unwrap().query_one("select current_database()", &[]).unwrap().get(0);
        let db = crate::indexer::shared_postgres_db(
            &crate::pg::StorageConfig {
                backend: crate::pg::Backend::Postgres,
                url: schema.url.clone(),
                schema: schema.name.clone(),
                allow_plaintext: true,
                ..Default::default()
            },
            &database,
            crate::indexer::PgWriteLimits { max_pending_bytes: 4096, max_job_bytes: 4096, concurrent_saves: 1 },
        )
        .unwrap();
        let mut c = schema.pool.get().unwrap();
        super::super::linker_bootstrap::import(&mut c, &dir.path().join("_state"), 120000).unwrap();
        c.execute(
            "insert into linker_linked_hours select h from generate_series(-14,100) h where h not in(27,40)",
            &[],
        )
        .unwrap();
        c.batch_execute("create function require_durable_linker_write() returns trigger language plpgsql as $$ begin
          if current_setting('synchronous_commit') <> 'on' then raise exception 'asynchronous native linker write'; end if;
          if TG_OP='DELETE' then return old; else return new; end if; end $$;
          do $$ declare tab record; begin for tab in select tablename from pg_tables where schemaname=current_schema() and tablename like 'linker_%' loop
          execute format('create trigger require_durable_linker before insert or update or delete on %I for each row execute function require_durable_linker_write()',tab.tablename);
          end loop; end $$").unwrap();
        drop(c);
        let mut owner_client = postgres::Client::connect(&schema.url, postgres::NoTls).unwrap();
        owner_client
            .batch_execute(&format!("set search_path to \"{}\"; set synchronous_commit=off", schema.name))
            .unwrap();
        assert!(
            owner_client
                .query_one("select pg_try_advisory_lock(hashtext('atlas.indexer'),hashtext($1))", &[&schema.name])
                .unwrap()
                .get::<_, bool>(0)
        );
        let db = crate::indexer::owned_postgres_db(
            db,
            Arc::new(crate::pg::indexer_runtime::Owner::new(owner_client)),
            crate::indexer::PgWriteLimits { max_pending_bytes: 4096, max_job_bytes: 4096, concurrent_saves: 1 },
        )
        .unwrap();
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            let mut source_cache = super::super::linker_source::Cache::default();
            for (now, expected) in [(120000i64, 12i64), (250000, 24)] {
                let epoch = on_native_db(&db, move |c| super::super::linker_epoch::open(c, now)).await.unwrap();
                let pin = store.pin_linker_retention();
                let mut done = false;
                for _ in 0..200 {
                    // Recovery reopens the epoch with a different wall clock:
                    // source identities, phase and logical time remain frozen.
                    let resumed =
                        on_native_db(&db, move |c| super::super::linker_epoch::open(c, now + 1000)).await.unwrap();
                    assert_eq!(resumed, epoch);
                    let emitted = on_native_db(&db, move |c| {
                        let phase: String =
                            c.query_one("select phase from linker_epochs where id=$1", &[&epoch])?.get(0);
                        if phase == "emit" {
                            super::super::linker_emit::test_page(c, epoch, Budget { rows: 2, bytes: 1024 })
                        } else {
                            Ok(Vec::new())
                        }
                    })
                    .await
                    .unwrap();
                    actual.extend(emitted);
                    if step(&db, &store, epoch, Budget { rows: 2, bytes: 1024 }, &mut source_cache).await.unwrap() {
                        done = true;
                        break;
                    }
                    let phase = on_native_db(&db, move |c| {
                        Ok(c.query_one("select phase from linker_epochs where id=$1", &[&epoch])?.get::<_, String>(0))
                    })
                    .await
                    .unwrap();
                    if phase == "done" {
                        done = true;
                        break;
                    }
                }
                assert!(done, "bounded phase driver failed to progress");
                drop(pin);
                let count =
                    on_native_db(&db, |c| Ok(c.query_one("select count(*) from segments", &[])?.get::<_, i64>(0)))
                        .await
                        .unwrap();
                assert_eq!(count, expected);
                if expected == 12 {
                    assert_eq!(source_cache.verifications, 1, "normal row pages rehashed the same raw block");
                    let callback_db = db.clone();
                    store.set_linker_coverage(Some(Arc::new(move |group, day, generation, count| {
                        on_native_db_sync(&callback_db, |c| {
                            super::super::linker_stage::covered(c, group, day, generation, count)
                        })
                    })));
                    store.set_keep(KeepSet { live_from: 100, ..KeepSet::default() });
                    store.arm_expiry();
                    for _ in 0..3 {
                        store.housekeeping();
                    }
                    assert!(store.segment_snapshot("group.example", 1).is_none());
                    store.set_linker_coverage(None);
                    for turn in 0..2000 {
                        assert!(turn < 1999, "completed epoch cleanup did not progress");
                        if !on_native_db(&db, move |c| super::super::linker_gc::step(c, now, 2)).await.unwrap() {
                            break;
                        }
                    }
                }
            }
        });
        let flatten = |releases: Vec<crate::parser::Release>| {
            let mut rows: Vec<_> = releases
                .into_iter()
                .flat_map(|r| {
                    r.articles
                        .into_iter()
                        .map(move |a| ((r.name.clone(), r.group.clone(), r.poster.clone(), r.date.clone()), a))
                })
                .collect();
            rows.sort_by(|a, b| a.1.message_id.cmp(&b.1.message_id));
            rows
        };
        assert_eq!(flatten(actual), flatten(golden));
        let mut c = schema.pool.get().unwrap();
        assert_eq!(c.query_one("select count(*) from linker_linked_ids", &[]).unwrap().get::<_, i64>(0), 24);
        assert_eq!(
            c.query_one("select count(*) from linker_staged_rows where published", &[]).unwrap().get::<_, i64>(0),
            12
        );
        assert!(!dir.path().join("_state/links.rec").exists());
    }
}
