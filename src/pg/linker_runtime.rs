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

/// One bounded reducer step. Input rows, merge heads and metadata effects
/// advance durable indexed cursors; no transaction owns a whole epoch/upload.
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
                on_native_db(db, move |c| super::linker_epoch::scope_step(c, epoch, budget.rows)).await?;
            }
        }
        "select" => {
            on_native_db(db, move |c| super::linker_select::select_step(c, epoch, budget.rows).map(|_| ())).await?
        }
        "files" => {
            on_native_db(db, move |c| {
                if !super::linker_plan::prepare_step(c, epoch, budget.rows)? {
                    return Ok(());
                }
                super::linker_files::infer_step(c, epoch, budget.rows).map(|_| ())
            })
            .await?
        }
        "chain" => {
            on_native_db(db, move |c| super::linker::plan_files(c, epoch, budget.rows)).await?;
        }
        "plan" => {
            on_native_db(db, move |c| super::linker_output::step(c, epoch, budget.rows)).await?;
        }
        "emit" => {
            super::linker_emit::emit_page(db, epoch, budget).await?;
        }
        "ack" => on_native_db(db, move |c| super::linker_plan::ack_step(c, epoch, budget.rows).map(|_| ())).await?,
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
    let session_now = chrono::Utc::now().timestamp();
    let mut metadata_expired = false;
    let mut imported = false;
    let mut current = None;
    let mut source_cache = super::linker_source::Cache::default();
    while !stopping() {
        let result: Result<bool> = async {
            if !imported {
                let dir = store.root().join("_state");
                imported = on_native_db(&db, move |c| {
                    super::linker_bootstrap::step(c, &dir, chrono::Utc::now().timestamp(), Budget::default())
                })
                .await?;
                if !imported {
                    return Ok(false);
                }
            }
            if current.is_none()
                && on_native_db(&db, |c| super::linker_gc::step(c, chrono::Utc::now().timestamp(), 256)).await?
            {
                return Ok(false);
            }
            if current.is_none() && !metadata_expired {
                match on_native_db(&db, move |c| super::linker_gc::expire_metadata(c, session_now, 256)).await? {
                    super::linker_gc::MetadataExpiry::Deferred => {}
                    super::linker_gc::MetadataExpiry::Progress => return Ok(false),
                    super::linker_gc::MetadataExpiry::Complete => metadata_expired = true,
                }
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
    fn multi_file_golden_matches_indexes_group_ties_lossy_ids_and_repeated_posters() {
        let Some(schema) = crate::pg::testing::Schema::new() else { return };
        let dir = tempfile::tempdir().unwrap();
        let (store, _) = Store::open(
            StoreConfig { sealers: 1, ..StoreConfig::new(dir.path()) },
            KeepSet { live_from: i64::MIN / 2, ..KeepSet::default() },
        )
        .unwrap();
        let mut groups = std::collections::BTreeMap::<String, Vec<Vec<u8>>>::new();
        let mut number = 0;
        let mut add = |group: &str, poster: &str, posted: i64, local: Vec<u8>| {
            number += 1;
            let date = chrono::DateTime::from_timestamp(posted, 0).unwrap().format("%a, %d %b %Y %H:%M:%S +0000");
            let mut row = format!("{number}\t").into_bytes();
            row.extend_from_slice(&local);
            row.extend_from_slice(format!("\t{poster}\t{date}\t<").as_bytes());
            row.extend_from_slice(&local);
            row.extend_from_slice(b"@ngPost>\t\t700000\t5000");
            groups.entry(group.to_owned()).or_default().push(row);
        };
        add("a.example", "00-index@example.invalid", 99000, b"index0".to_vec());
        add("b.example", "01-index@example.invalid", 99000, b"index1".to_vec());
        let mut full = 0;
        for file in 0..32 {
            let poster = if file == 0 {
                "MÃ¼ller@example.invalid".to_owned()
            } else if file == 2 {
                // Valid, incompressible author text must stay a payload, never a B-tree key.
                use sha2::{Digest, Sha256};
                format!(
                    "zz-{}",
                    (0..160)
                        .map(|n| format!("{:x}", Sha256::digest(format!("generic-poster-{n}"))))
                        .collect::<String>()
                )
            } else {
                format!("file{file:02}@example.invalid")
            };
            for part in 0..if file == 0 { 64 } else { 7 } {
                let mut local = format!("file{file}part{part}").into_bytes();
                if file == 0 && part == 0 {
                    local.push(0xff);
                }
                add(if full % 2 == 0 { "a.example" } else { "b.example" }, &poster, 99000, local);
                full += 1;
            }
        }
        for (run, posted) in [(0, 150000), (1, 170000)] {
            for part in 0..12 {
                add(
                    if part % 2 == 0 { "a.example" } else { "b.example" },
                    "repeated@example.invalid",
                    posted,
                    format!("repeat{run}part{part}").into_bytes(),
                );
            }
        }
        for (group, rows) in groups {
            let ticket = store.record("server.example", &group, &rows, Clamp::Unknown { now: 250000 }).unwrap();
            store.tick().unwrap();
            ticket.wait(Duration::from_secs(5)).unwrap();
        }
        store.close().unwrap();
        let place = crate::nxv_store::linker::PlaceRec {
            place: crate::xref::Place {
                release: "xref-upload-prior".into(),
                group: "a.example".into(),
                file: 1,
                files: 1,
                parts: 12,
                leads: "1".into(),
                oldest: 250000,
            },
            firsts: Default::default(),
            added: 250000,
        };
        let upload = crate::nxv_store::linker::UploadRec {
            group: "a.example".into(),
            seed: crate::xref::Seed { groups: Default::default(), first: 0, last: 0 },
            files: 1,
            generation: 4,
            probed: 0,
            added: 250000,
        };
        let records = format!(
            "{}{}",
            crate::nxv_store::linker::Links::place_line("repeated@example.invalid", &place),
            crate::nxv_store::linker::Links::upload_line("xref-upload-prior", &upload)
        );
        let state = dir.path().join("_state");
        std::fs::write(state.join("links.rec"), &records).unwrap();
        let legacy_dir = tempfile::tempdir().unwrap();
        std::fs::write(legacy_dir.path().join("links.rec"), &records).unwrap();
        let mut legacy = crate::nxv_store::linker::Linker::open_in(legacy_dir.path(), 250000).unwrap();
        legacy.state.linked.extend(-14..=100);
        let golden = legacy.pass(&store, 250000, &Default::default(), &|| false).unwrap().unwrap().releases;
        // Each singleton index takes one full file before an established
        // chain wins ties. The second index plus its seven-part file stays
        // below MIN_UPLOAD_ARTICLES, so that eight-row chain remains pending.
        assert_eq!(golden.iter().map(|r| r.articles.len()).sum::<usize>(), 299);
        assert!(!golden.iter().flat_map(|r| &r.articles).any(|a| a.message_id.starts_with("<file1part")));
        assert!(golden.iter().flat_map(|r| &r.articles).any(|a| a.message_id.contains('�')));
        drop(store);
        let (store, _) = Store::open(
            StoreConfig { bounded_maps: true, sealers: 1, ..StoreConfig::new(dir.path()) },
            KeepSet { live_from: i64::MIN / 2, ..KeepSet::default() },
        )
        .unwrap();
        let mut c = schema.pool.get().unwrap();
        c.execute("insert into linker_linked_hours select generate_series(-14,100)", &[]).unwrap();
        c.batch_execute("create table mutation_audit(tx bigint primary key,phase text not null,mutations bigint not null);
            create function audit_linker_mutation() returns trigger language plpgsql as $$ declare p text; begin
              if current_setting('synchronous_commit')<>'on' then raise exception 'non-durable native mutation'; end if;
              select phase into p from linker_epochs where phase<>'done' order by id limit 1;
              insert into mutation_audit values(txid_current(),coalesce(p,'done'),1)
                on conflict(tx) do update set mutations=mutation_audit.mutations+1;
              return null; end $$;
            do $$ declare tab record; begin for tab in select tablename from pg_tables where schemaname=current_schema() and tablename like 'linker_%' loop
              execute format('create trigger audit_mutations after insert or update or delete on %I for each row execute function audit_linker_mutation()',tab.tablename);
            end loop;end $$").unwrap();
        c.batch_execute("set synchronous_commit=off").unwrap();
        super::super::linker_bootstrap::import(&mut c, &state, 250000).unwrap();
        let database: String = c.query_one("select current_database()", &[]).unwrap().get(0);
        drop(c);
        let db = crate::indexer::shared_postgres_db(
            &crate::pg::StorageConfig {
                backend: crate::pg::Backend::Postgres,
                url: schema.url.clone(),
                schema: schema.name.clone(),
                allow_plaintext: true,
                ..Default::default()
            },
            &database,
            crate::indexer::PgWriteLimits { max_pending_bytes: 65536, max_job_bytes: 32768, concurrent_saves: 1 },
        )
        .unwrap();
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
        let owner = Arc::new(crate::pg::indexer_runtime::Owner::new(owner_client));
        let db = crate::indexer::owned_postgres_db(
            db,
            owner,
            crate::indexer::PgWriteLimits { max_pending_bytes: 65536, max_job_bytes: 32768, concurrent_saves: 1 },
        )
        .unwrap();
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let mut actual = Vec::new();
        rt.block_on(async {
            let epoch = on_native_db(&db, |c| super::super::linker_epoch::open(c, 250000)).await.unwrap();
            let _pin = store.pin_linker_retention();
            let mut cache = super::super::linker_source::Cache::default();
            for turn in 0..3000 {
                assert!(turn < 2999, "golden fixture stopped making progress");
                let page = on_native_db(&db, move |c| {
                    if c.query_one("select phase from linker_epochs where id=$1", &[&epoch])?.get::<_, String>(0)
                        == "emit"
                    {
                        super::super::linker_emit::test_page(c, epoch, Budget { rows: 3, bytes: 1024 })
                    } else {
                        Ok(Vec::new())
                    }
                })
                .await
                .unwrap();
                assert!(page.len() <= 3);
                actual.extend(page);
                if step(&db, &store, epoch, Budget { rows: 3, bytes: 1024 }, &mut cache).await.unwrap() {
                    break;
                }
            }
            assert_eq!(cache.verifications, 2);
        });
        let mut audit = schema.pool.get().unwrap();
        let phases =
            audit.query("select phase,max(mutations) from mutation_audit group by phase order by phase", &[]).unwrap();
        for phase in ["stage", "select", "files", "chain", "plan", "emit", "ack"] {
            let row = phases.iter().find(|r| r.get::<_, String>(0) == phase).expect("every phase was exercised");
            let limit = match phase {
                "stage" => 66,
                // File merge: one output insert + consumed-input delete per unit, plus checkpoint.
                "chain" => 2 * 3 + 1,
                "ack" => 4,
                "emit" => 10,
                _ => 13,
            };
            assert!(
                row.get::<_, i64>(1) <= limit,
                "{phase} exceeded mutation bound: {} > {limit}",
                row.get::<_, i64>(1)
            );
        }
        assert_eq!(
            audit
                .query_one("select count(*) from linker_places where octet_length(poster)>5000", &[])
                .unwrap()
                .get::<_, i64>(0),
            1,
            "long poster must reach acknowledged live metadata, not merely pending selection"
        );
        drop(audit);
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
    }

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
            assert_eq!(store.segment_snapshot("a.example", 1).unwrap().unwrap().count, blocks);
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
                assert_eq!(
                    source_cache.verifications,
                    blocks as u64 + 1,
                    "completed blocks were rehashed in the next epoch"
                );
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
        let old_time = 120000 - 9 * crate::xref::KEEP_SECS;
        let old_place = crate::nxv_store::linker::PlaceRec {
            place: crate::xref::Place {
                release: "xref-upload-stale".into(),
                group: "group.example".into(),
                file: 1,
                files: 1,
                parts: 12,
                leads: "1".into(),
                oldest: old_time,
            },
            firsts: Default::default(),
            added: old_time,
        };
        let old_upload = crate::nxv_store::linker::UploadRec {
            group: "group.example".into(),
            seed: crate::xref::Seed { groups: Default::default(), first: 99000, last: 99000 },
            files: 1,
            generation: 3,
            probed: 1,
            added: old_time,
        };
        let legacy_records = format!(
            "{}{}",
            crate::nxv_store::linker::Links::place_line("current@example.invalid", &old_place),
            crate::nxv_store::linker::Links::upload_line("xref-upload-stale", &old_upload)
        );
        let state_dir = dir.path().join("_state");
        std::fs::create_dir_all(&state_dir).unwrap();
        std::fs::write(state_dir.join("links.rec"), &legacy_records).unwrap();
        let legacy_dir = tempfile::tempdir().unwrap();
        std::fs::write(legacy_dir.path().join("links.rec"), &legacy_records).unwrap();
        let mut legacy = crate::nxv_store::linker::Linker::open_in(legacy_dir.path(), 120000).unwrap();
        assert!(legacy.links.lock().unwrap().places.is_empty());
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
        super::super::linker_bootstrap::import(&mut c, &state_dir, old_time).unwrap();
        assert_eq!(c.query_one("select count(*) from linker_places", &[]).unwrap().get::<_, i64>(0), 1);
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
            let restart_dir = state_dir.clone();
            on_native_db(&db, move |c| super::super::linker_bootstrap::import(c, &restart_dir, 120000)).await.unwrap();
            assert_eq!(
                on_native_db(&db, |c| Ok(c.query_one("select count(*) from linker_places", &[])?.get::<_, i64>(0)))
                    .await
                    .unwrap(),
                1
            );
            for turn in 0..100 {
                assert!(turn < 99);
                if matches!(
                    on_native_db(&db, |c| super::super::linker_gc::expire_metadata(c, 120000, 2)).await.unwrap(),
                    super::super::linker_gc::MetadataExpiry::Complete
                ) {
                    break;
                }
            }
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
                    assert!(store.segment_snapshot("group.example", 1).unwrap().is_none());
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
        assert_eq!(std::fs::read_to_string(state_dir.join("links.rec")).unwrap(), legacy_records);
    }
}
