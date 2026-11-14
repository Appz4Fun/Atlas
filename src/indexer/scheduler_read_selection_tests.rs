//! Exercise real scheduler callers against the original native Owner admission.
use super::*;
use std::{
    net::TcpListener,
    sync::{atomic::Ordering, mpsc},
    thread,
};

const CACHED: &str = "example.scheduler.cached";
const SPLIT: &str = "example.scheduler.split";
const EMPTY: &str = "example.scheduler.empty";
const MUTATION: &str = "example.scheduler.mutation";

struct HeldOwner {
    release: Option<mpsc::Sender<()>>,
    job: Option<thread::JoinHandle<Result<()>>>,
}
impl HeldOwner {
    fn new(owner: &Arc<crate::pg::indexer_runtime::Owner>) -> Self {
        // A selection turn makes the next ordinary turn a service turn. The
        // active ingestion turn then lets every caller queue deterministically.
        owner.with_selection(|_| Ok(())).unwrap();
        let (started, ready) = mpsc::channel();
        let (release, wait) = mpsc::channel();
        let owner = owner.clone();
        let job = thread::spawn(move || {
            owner.with_ingest(|_| {
                started.send(())?;
                wait.recv_timeout(Duration::from_secs(10))?;
                Ok(())
            })
        });
        ready.recv_timeout(Duration::from_secs(5)).unwrap();
        Self { release: Some(release), job: Some(job) }
    }
    fn finish(mut self) {
        self.release.take().unwrap().send(()).unwrap();
        self.job.take().unwrap().join().unwrap().unwrap();
    }
}
impl Drop for HeldOwner {
    fn drop(&mut self) {
        if let Some(release) = self.release.take() {
            let _ = release.send(());
        }
        if let Some(job) = self.job.take() {
            let _ = job.join();
        }
    }
}

fn queued(owner: &crate::pg::indexer_runtime::Owner, total: usize) -> [usize; 3] {
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        let sizes = owner.admission_waiters_for_test();
        if sizes.iter().sum::<usize>() == total {
            return sizes;
        }
        assert!(Instant::now() < until, "actual scheduler queues {sizes:?}; expected {total} callbacks");
        thread::yield_now();
    }
}

struct Fixture {
    runtime: crate::pg::indexer_runtime::Runtime,
    _schema: crate::pg::testing::Schema,
    ctx: PassContext,
    unused_provider: TcpListener,
}
impl Fixture {
    fn new() -> Self {
        let schema = crate::pg::testing::Schema::new().expect("explicit isolated PostgreSQL required");
        let runtime = crate::pg::stage_queue_tests::runtime(&schema);
        assert!(runtime.owner.epoch().is_some());
        // No DNS or real provider can be reached. These early paths must not
        // open even this owned loopback listener; a wrong branch fails quickly.
        let unused_provider = TcpListener::bind("127.0.0.1:0").unwrap();
        unused_provider.set_nonblocking(true).unwrap();
        let mut server = crate::config::UsenetServer::new(
            "127.0.0.1",
            "fixture",
            "fixture",
            unused_provider.local_addr().unwrap().port(),
        );
        server.connections = Some(1);
        server.ssl = Some(false);
        server.compress = Some(false);
        let pool = Arc::new(Pool::new(&[server]).with_timeout(Duration::from_millis(200)));
        let host = pool.host(0);
        runtime
            .owner
            .with(|client| {
                let sql = &crate::pg::sql::Pg::new(&mut *client);
                assert_eq!(crate::chunks::add(sql, SPLIT, 20_000, 20_000)?, 1);
                crate::chunks::set_deepest(sql, CACHED, &host, 10)?;
                crate::chunks::set_runner_up(sql, CACHED, 100)?;
                crate::chunks::set_deepest(sql, MUTATION, &host, 10)?;
                Ok(())
            })
            .unwrap();
        let ctx = PassContext { pool, states: RunStates::default(), stop: runtime.owner.stop.clone(), verbose: false };
        ctx.states.with(SPLIT, |state| state.reach_back_after = Some(Instant::now() + SPLIT_RECHECK));
        Self { _schema: schema, runtime, ctx, unused_provider }
    }
    fn readers(&self) -> Vec<thread::JoinHandle<Result<()>>> {
        let mut jobs = Vec::new();
        for case in 0..4 {
            let ctx = self.ctx.clone();
            let db = self.runtime.db.clone();
            jobs.push(thread::spawn(move || {
                tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async move {
                    match case {
                        0 => assert_eq!(goes_back_furthest(&ctx, &db, CACHED, 0, (10, true)).await?, (true, Some(100))),
                        1 => reach_back(&ctx, &db, EMPTY, 0, (1, 10)).await?,
                        2 => assert!(maybe_split(&ctx, &PassSettings::default(), &db, SPLIT, 0, (1, 10), 5).await?),
                        3 => assert!(!sweep_due(&ctx, &db, SPLIT, &ctx.pool.host(0)).await?),
                        _ => unreachable!(),
                    }
                    Ok(())
                })
            }));
            queued(&self.runtime.owner, case + 1);
        }
        jobs
    }
    fn no_provider_io(&self) {
        assert!(
            matches!(self.unused_provider.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
            "scheduler read-only fixture contacted its owned provider listener"
        );
    }
}

#[test]
#[ignore = "explicit isolated PostgreSQL; actual scheduler callers, native observation, and original-owner mutation"]
fn scheduler_reads_leave_service_fifo_for_observation_and_mutation() {
    let fixture = Fixture::new();
    let held = HeldOwner::new(&fixture.runtime.owner);
    let readers = fixture.readers();
    let owner = fixture.runtime.owner.clone();
    let observation = thread::spawn(move || crate::pg::stage_telemetry::capture(&owner, None));
    queued(&fixture.runtime.owner, 5);
    let db = fixture.runtime.db.clone();
    let mutation = thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(
            // This is the same on_db/chunks mutation path as the scheduler's
            // retained write callbacks, not direct admission into a chosen class.
            on_db(&db, |sql| {
                assert!(sql.exists("select 1 from native_runtime_observation_current where singleton", &[])?);
                crate::chunks::set_runner_up(sql, MUTATION, 177)?;
                Ok(())
            }),
        )
    });
    let observed = queued(&fixture.runtime.owner, 6);
    // Always release and join before the intended RED assertion.
    held.finish();
    for reader in readers {
        reader.join().unwrap().unwrap();
    }
    assert!(matches!(observation.join().unwrap().unwrap(), crate::pg::stage_telemetry::Capture::Complete));
    mutation.join().unwrap().unwrap();
    assert_eq!(fixture.runtime.owner.admission_waiters_for_test(), [0; 3]);
    fixture.runtime.owner.with(|client| {
        let sql = &crate::pg::sql::Pg::new(&mut *client);
        assert_eq!(crate::chunks::runner_up(sql, MUTATION)?, Some(177));
        assert_eq!(crate::chunks::runner_up(sql, CACHED)?, Some(100));
        assert_eq!(crate::chunks::newest_day(sql, SPLIT)?, Some(20_000));
        assert_eq!(crate::chunks::newest_day(sql, EMPTY)?, None);
        let row = client.query_one("select o.epoch from native_runtime_observation_current c join native_runtime_observations o using(observation_id) where c.singleton", &[])?;
        assert_eq!(row.get::<_, Vec<u8>>(0).as_slice(), fixture.runtime.owner.epoch().unwrap().id().as_slice());
        Ok(())
    }).unwrap();
    fixture.no_provider_io();
    assert_eq!(fixture.runtime.owner.failure(), None);
    fixture.runtime.owner.close();
    assert_eq!(
        observed,
        [0, 2, 4],
        "four actual scheduler read callbacks belong to selection; observation and actual on_db mutation retain the service FIFO"
    );
}

#[test]
#[ignore = "explicit isolated PostgreSQL; operator stop cancels real queued scheduler callers"]
fn queued_scheduler_reads_stop_without_running_or_poisoning_original_owner() {
    let fixture = Fixture::new();
    let held = HeldOwner::new(&fixture.runtime.owner);
    let readers = fixture.readers();
    fixture.runtime.owner.stop.store(true, Ordering::Release);
    for reader in readers {
        assert!(crate::pg::outage::is_attempt_ended(&reader.join().unwrap().unwrap_err()));
    }
    assert_eq!(fixture.runtime.owner.admission_waiters_for_test(), [0; 3]);
    held.finish();
    fixture.no_provider_io();
    assert_eq!(fixture.runtime.owner.failure(), None);
    fixture.runtime.owner.close();
}
