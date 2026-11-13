//! Real rejected/waiting Runner results retain observed own-category cleanup.
use super::*;
use crate::pg::{
    self,
    indexer_runtime::Runtime,
    stage_queue::{Queue, Settlement},
};
use crate::postprocess::step::{Outcome, StepCtx, StepError, StepId};
use std::sync::atomic::{AtomicUsize, Ordering};

const NOW: i64 = 10_000;
const BIT: i64 = 1 << 24;

struct CountStep(Arc<AtomicUsize>);
impl Step for CountStep {
    fn id(&self) -> StepId {
        StepId(24)
    }
    fn name(&self) -> &'static str {
        "cleanup_control"
    }
    fn rank(&self) -> u16 {
        400
    }
    fn needs(&self) -> Needs {
        Needs::Database
    }
    fn run(&self, _: &mut StepCtx<'_>) -> Result<Outcome, StepError> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Ok(Outcome::default())
    }
}

fn runner(runtime: &Runtime, min_size: i64, settle: i64) -> (Runner, Arc<AtomicUsize>, tokio::runtime::Runtime) {
    let mut config = crate::config::Config::default();
    config.postprocess.min_size_bytes = min_size;
    config.postprocess.settle_secs = settle;
    config.postprocess.complete_settle_secs = settle;
    config.predb.enabled = false;
    config.spots.enabled = false;
    config.inferred_parts = false;
    config.media.tvmaze.enabled = false;
    config.media.allocine.enabled = false;
    config.media.details_per_minute = 0;
    let handle = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let mut runner =
        Runner::new(&runtime.owner, &config, Arc::new(crate::nntp::Pool::new(&[])), handle.handle().clone()).unwrap();
    let runs = Arc::new(AtomicUsize::new(0));
    runner.steps = Steps {
        ordered: vec![Arc::new(CountStep(runs.clone()))],
        enabled: BIT,
        database: BIT,
        network: 0,
        on_growth: 0,
        stamps: Vec::new(),
    };
    (runner, runs, handle)
}

fn seed(runtime: &Runtime, filename: &str, category: i64, state: bool) -> (i64, i64) {
    runtime
        .owner
        .with(|client| {
            pg::release_writer::save(
                client,
                &[crate::parser::Release {
                    name: "Native.PC.Cleanup".into(),
                    group: "example.group".into(),
                    articles: vec![crate::parser::Article {
                        filename: Some(filename.into()),
                        subject: format!("{filename} yEnc (1/1)"),
                        message_id: "<pc-cleanup@example.test>".into(),
                        part: Some(1),
                        total_parts: Some(1),
                        bytes: 4,
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
                1,
            )?;
            let id: i64 = client.query_one("select id from releases", &[])?.try_get(0)?;
            client.execute("insert into release_category(release_id,category) values($1,$2)", &[&id, &category])?;
            if state {
                client.execute(
                    "insert into pp_state(release_id,done,failed,net_wanted,net_left) values($1,4,8,true,32)",
                    &[&id],
                )?;
            }
            let revision =
                client.query_one("select native_work_revision from releases where id=$1", &[&id])?.try_get(0)?;
            Ok((id, revision))
        })
        .unwrap()
}

#[test]
#[ignore = "requires isolated PostgreSQL; genuine Runner rejected cleanup and receipt replay"]
fn rejected_video_cleans_stale_pc_without_running_or_marking_a_step() {
    for state in [false, true] {
        let schema = pg::testing::Schema::new().expect("explicit isolated ATLAS_PG_TEST_URL required");
        let runtime = pg::stage_queue_tests::runtime(&schema);
        let (id, revision) = seed(&runtime, "video.mkv", 4000, state);
        let (runner, runs, _handle) = runner(&runtime, 8, 1);
        let queue = Queue::for_owner(&runtime.owner).unwrap();
        let claim = queue.claim(NOW, 1, 60).unwrap().pop().expect("real writer-created due claim");
        let prepared = runner.database(&claim, 0, NOW).unwrap();
        assert_eq!(runner.stats.too_small.load(Ordering::Relaxed), 1);
        let replay = runner.database(&claim, 0, NOW).unwrap();
        assert_eq!(prepared.settle(&queue, &claim, NOW + 1).unwrap(), Settlement::Applied);
        assert_eq!(replay.settle(&queue, &claim, NOW + 2).unwrap(), Settlement::Replayed);
        runtime
            .owner
            .with(|client| {
                assert_eq!(
                    client
                        .query_one("select count(*) from release_category where release_id=$1", &[&id])?
                        .get::<_, i64>(0),
                    0
                );
                assert_eq!(
                    client.query_one("select native_work_revision from releases where id=$1", &[&id])?.get::<_, i64>(0),
                    revision + 1
                );
                assert_eq!(client.query_one("select count(*) from native_operations", &[])?.get::<_, i64>(0), 1);
                assert_eq!(client.query_one("select count(*) from pp_queue", &[])?.get::<_, i64>(0), 0);
                if state {
                    let row =
                        client.query_one("select done,failed,net_wanted from pp_state where release_id=$1", &[&id])?;
                    assert_eq!(
                        (row.get::<_, i64>(0), row.get::<_, i64>(1), row.get::<_, Option<bool>>(2)),
                        (4, 8, None)
                    );
                } else {
                    assert_eq!(client.query_one("select count(*) from pp_state", &[])?.get::<_, i64>(0), 0);
                }
                Ok(())
            })
            .unwrap();
        assert_eq!(runs.load(Ordering::Relaxed), 0);
        assert!(runtime.owner.failure().is_none());
        runtime.owner.close();
    }
}

#[test]
#[ignore = "requires isolated PostgreSQL; genuine settling wait retains category cleanup"]
fn settling_video_cleans_stale_pc_and_keeps_its_real_wait() {
    let schema = pg::testing::Schema::new().expect("explicit isolated ATLAS_PG_TEST_URL required");
    let runtime = pg::stage_queue_tests::runtime(&schema);
    let (id, _) = seed(&runtime, "video.mkv", 4000, false);
    let (runner, runs, _handle) = runner(&runtime, 0, 20_000);
    let queue = Queue::for_owner(&runtime.owner).unwrap();
    let claim = queue.claim(NOW, 1, 60).unwrap().pop().unwrap();
    let prepared = runner.database(&claim, 0, NOW).unwrap();
    let until: i64 = runtime
        .owner
        .with(|client| {
            Ok(client.query_one("select max(touched_at)+20000 from files where release_id=$1", &[&id])?.try_get(0)?)
        })
        .unwrap();
    assert!(until > NOW);
    assert_eq!(prepared.settle(&queue, &claim, NOW + 1).unwrap(), Settlement::Applied);
    runtime
        .owner
        .with(|client| {
            assert_eq!(client.query_one("select count(*) from release_category", &[])?.get::<_, i64>(0), 0);
            assert_eq!(
                client.query_one("select due_at from pp_queue where release_id=$1", &[&id])?.get::<_, i64>(0),
                until
            );
            assert_eq!(client.query_one("select count(*) from pp_state", &[])?.get::<_, i64>(0), 0);
            Ok(())
        })
        .unwrap();
    assert_eq!(runs.load(Ordering::Relaxed), 0);
    runtime.owner.close();
}

#[test]
#[ignore = "requires isolated PostgreSQL; stale original input cannot commit cleanup or receipt"]
fn superseded_video_cleanup_cannot_delete_a_newer_category() {
    let schema = pg::testing::Schema::new().expect("explicit isolated ATLAS_PG_TEST_URL required");
    let runtime = pg::stage_queue_tests::runtime(&schema);
    let (id, _) = seed(&runtime, "video.mkv", 4000, false);
    let (runner, _, _handle) = runner(&runtime, 8, 1);
    let queue = Queue::for_owner(&runtime.owner).unwrap();
    let claim = queue.claim(NOW, 1, 60).unwrap().pop().unwrap();
    let prepared = runner.database(&claim, 0, NOW).unwrap();
    // Actual newer accepted header input invalidates the prepared observation.
    runtime
        .owner
        .with(|client| {
            pg::release_writer::save(
                client,
                &[crate::parser::Release {
                    name: "Native.PC.Cleanup".into(),
                    group: "example.group".into(),
                    articles: vec![crate::parser::Article {
                        filename: Some("video.mkv".into()),
                        subject: "video.mkv yEnc (2/2)".into(),
                        message_id: "<pc-cleanup-new@example.test>".into(),
                        part: Some(2),
                        total_parts: Some(2),
                        bytes: 4,
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
                NOW,
            )
            .map(|_| ())
        })
        .unwrap();
    assert_eq!(prepared.settle(&queue, &claim, NOW + 1).unwrap(), Settlement::Superseded);
    runtime
        .owner
        .with(|client| {
            assert_eq!(
                client.query_one("select category from release_category where release_id=$1", &[&id])?.get::<_, i64>(0),
                4000
            );
            assert_eq!(client.query_one("select count(*) from native_operations", &[])?.get::<_, i64>(0), 0);
            Ok(())
        })
        .unwrap();
    assert!(runtime.owner.failure().is_none());
    runtime.owner.close();
}

#[test]
#[ignore = "requires isolated PostgreSQL; actual mutation SQL failure rolls back cleanup and ACK"]
fn cleanup_sql_failure_preserves_category_and_has_no_receipt() {
    let schema = pg::testing::Schema::new().expect("explicit isolated ATLAS_PG_TEST_URL required");
    let runtime = pg::stage_queue_tests::runtime(&schema);
    let (id, revision) = seed(&runtime, "video.mkv", 4000, false);
    let (runner, _, _handle) = runner(&runtime, 8, 1);
    let queue = Queue::for_owner(&runtime.owner).unwrap();
    let claim = queue.claim(NOW, 1, 60).unwrap().pop().unwrap();
    let prepared = runner.database(&claim, 0, NOW).unwrap();
    runtime
        .owner
        .with(|client| {
            client.batch_execute("alter table release_category rename to unavailable_cleanup_category")?;
            Ok(())
        })
        .unwrap();
    let error = prepared.settle(&queue, &claim, NOW + 1).unwrap_err();
    assert!(error.chain().any(|cause| cause.downcast_ref::<postgres::Error>().is_some()));
    assert_eq!(runtime.owner.failure(), Some(pg::outage::Failure::Fatal));
    let mut client = schema.pool.get().unwrap();
    assert_eq!(
        client
            .query_one("select category from unavailable_cleanup_category where release_id=$1", &[&id])
            .unwrap()
            .get::<_, i64>(0),
        4000
    );
    assert_eq!(
        client.query_one("select native_work_revision from releases where id=$1", &[&id]).unwrap().get::<_, i64>(0),
        revision
    );
    assert_eq!(client.query_one("select count(*) from native_operations", &[]).unwrap().get::<_, i64>(0), 0);
    assert_eq!(
        client.query_one("select count(*) from pp_queue where release_id=$1", &[&id]).unwrap().get::<_, i64>(0),
        1
    );
    drop(client);
    runtime.owner.close();
}

#[test]
#[ignore = "requires isolated PostgreSQL; passing video and no-video junk policies remain real"]
fn passing_video_cleanup_and_no_video_pc_junk_keep_their_policies() {
    for video in [true, false] {
        let schema = pg::testing::Schema::new().expect("explicit isolated ATLAS_PG_TEST_URL required");
        let runtime = pg::stage_queue_tests::runtime(&schema);
        let (id, _) = seed(&runtime, if video { "video.mkv" } else { "payload.bin" }, 4000, false);
        let (runner, runs, _handle) = runner(&runtime, 0, 1);
        let queue = Queue::for_owner(&runtime.owner).unwrap();
        let claim = queue.claim(NOW, 1, 60).unwrap().pop().unwrap();
        let prepared = runner.database(&claim, 0, NOW).unwrap();
        assert_eq!(prepared.settle(&queue, &claim, NOW + 1).unwrap(), Settlement::Applied);
        runtime
            .owner
            .with(|client| {
                assert_eq!(
                    client.query_one("select count(*) from releases where id=$1", &[&id])?.get::<_, i64>(0),
                    i64::from(video)
                );
                assert_eq!(client.query_one("select count(*) from release_category", &[])?.get::<_, i64>(0), 0);
                if video {
                    assert_eq!(
                        client.query_one("select done from pp_state where release_id=$1", &[&id])?.get::<_, i64>(0),
                        BIT
                    )
                }
                Ok(())
            })
            .unwrap();
        assert_eq!(runs.load(Ordering::Relaxed), usize::from(video));
        runtime.owner.close();
    }
}
