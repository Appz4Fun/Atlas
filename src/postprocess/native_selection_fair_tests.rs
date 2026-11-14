//! Root runs this control alone in a fresh configured PG/home child; no provider service starts.
use super::*;
use std::{
    sync::{atomic::AtomicUsize, mpsc},
    thread,
};

fn waiters(owner: &Owner, wanted: [usize; 3]) {
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        let actual = owner.admission_waiters_for_test();
        if actual == wanted {
            return;
        }
        assert!(Instant::now() < until, "admission {actual:?}, expected {wanted:?}");
        thread::yield_now();
    }
}
fn input() -> Vec<crate::parser::Release> {
    vec![crate::parser::Release {
        name: "Publication.Example".into(),
        group: "example.publication".into(),
        articles: vec![crate::parser::Article {
            filename: Some("sample.bin".into()),
            message_id: "<publication@example.test>".into(),
            part: Some(1),
            total_parts: Some(1),
            bytes: 17,
            ..Default::default()
        }],
        ..Default::default()
    }]
}
fn hold_selection(owner: &Arc<Owner>) -> (mpsc::Sender<()>, thread::JoinHandle<Result<()>>) {
    let (entered, ready) = mpsc::channel();
    let (release, wait) = mpsc::channel();
    let owner = owner.clone();
    let job = thread::spawn(move || {
        owner.with_selection(|_| {
            entered.send(())?;
            wait.recv_timeout(Duration::from_secs(10))?;
            Ok(())
        })
    });
    ready.recv_timeout(Duration::from_secs(5)).unwrap();
    (release, job)
}

#[test]
#[ignore = "requires fresh Root-owned ATLAS_NATIVE_TELEMETRY_OWNED PG/home child, real publication under queued selections"]
fn real_refresh_and_current_capture_progress_between_selection_turns_without_partial_stop_ack() {
    assert_eq!(std::env::var("ATLAS_NATIVE_TELEMETRY_OWNED").as_deref(), Ok("1"));
    assert!(std::env::var_os("ATLAS_HOME").is_some());
    let storage = crate::config::checked_storage_config().expect("explicit isolated owned storage");
    let mut config = crate::config::Config::default();
    config.postprocess.enabled = false;
    config.predb.enabled = false;
    config.spots.enabled = false;
    config.inferred_parts = false;
    config.media.tvmaze.enabled = false;
    config.media.allocine.enabled = false;
    config.media.details_per_minute = 0;
    let runtime = crate::pg::indexer_runtime::Runtime::open(
        &config,
        storage,
        crate::config::HeaderStoreConfig { enabled: true, ..Default::default() },
        &[],
        &mut None,
    )
    .unwrap();
    let owner = runtime.owner.clone();
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    config.postprocess.enabled = true;
    let runner =
        Arc::new(Runner::new(&owner, &config, Arc::new(crate::nntp::Pool::new(&[])), rt.handle().clone()).unwrap());
    let lanes = Arc::new(LaneCounters::new(runner.steps.enabled as u64).unwrap());
    let producer = Producer::new(&runner, lanes.clone()).unwrap();
    let (release, held) = hold_selection(&owner);
    let (started, enter) = mpsc::channel();
    let done = Arc::new(AtomicUsize::new(0));
    let mut releases = Vec::new();
    let mut jobs = Vec::new();
    for n in 0..8 {
        let (release, wait) = mpsc::channel();
        releases.push(release);
        let (worker, started, done) = (owner.clone(), started.clone(), done.clone());
        jobs.push(thread::spawn(move || {
            worker.with_selection(|client| {
                assert_eq!(
                    crate::chunks::frontier_oldest(
                        &crate::pg::sql::Pg::new(client),
                        &[("example.publication".into(), 0)],
                        100
                    )?,
                    None
                );
                started.send(n)?;
                wait.recv_timeout(Duration::from_secs(10))?;
                done.fetch_add(1, Ordering::Release);
                Ok(())
            })
        }));
        waiters(&owner, [0, 0, n + 1]);
    }
    let first = owner.clone();
    let save = thread::spawn(move || {
        first.with_ingest(|client| {
            assert_eq!(crate::pg::release_writer::save(client, &input(), 1)?.parts, 1);
            Ok(())
        })
    });
    waiters(&owner, [1, 0, 8]);
    let next = owner.clone();
    let cursor = thread::spawn(move || {
        next.with_ingest(|client| {
            crate::db::update_backfill_cursor(&crate::pg::sql::Pg::new(client), "example.publication", 1999)?;
            Ok(())
        })
    });
    waiters(&owner, [2, 0, 8]);
    let (refreshed, refresh_done) = mpsc::channel();
    let (capture_now, may_capture) = mpsc::channel();
    let (captured, capture_done) = mpsc::channel();
    let (finish_cache, cache_may_finish) = mpsc::channel();
    let cache_owner = owner.clone();
    let cache = thread::spawn(move || -> Result<()> {
        assert!(matches!(producer.refresh()?, Refresh::Published));
        refreshed.send(())?;
        may_capture.recv_timeout(Duration::from_secs(10))?;
        crate::pg::stage_telemetry::capture(&cache_owner, Some(&lanes))?;
        captured.send(())?;
        // Keep the actual producer installed through the parent's commit assertions.
        cache_may_finish.recv_timeout(Duration::from_secs(10))?;
        Ok(())
    });
    waiters(&owner, [2, 1, 8]);
    release.send(()).unwrap();
    held.join().unwrap().unwrap();
    assert_eq!(enter.recv_timeout(Duration::from_secs(5)).unwrap(), 0);
    refresh_done.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(done.load(Ordering::Acquire), 0, "real refresh completed before the queued selections drained");
    save.join().unwrap().unwrap();
    cursor.join().unwrap().unwrap();
    // Keep two real ingestion callbacks pending for the SECOND service admission too.
    let replay_owner = owner.clone();
    let replay = thread::spawn(move || {
        replay_owner.with_ingest(|client| {
            assert_eq!(crate::pg::release_writer::save(client, &input(), 2)?.parts, 0);
            Ok(())
        })
    });
    waiters(&owner, [1, 0, 7]);
    let checkpoint_owner = owner.clone();
    let checkpoint = thread::spawn(move || {
        checkpoint_owner.with_ingest(|client| {
            crate::db::update_live_cursor(&crate::pg::sql::Pg::new(client), "example.publication", 2000)?;
            Ok(())
        })
    });
    waiters(&owner, [2, 0, 7]);
    capture_now.send(()).unwrap();
    waiters(&owner, [2, 1, 7]);
    releases[0].send(()).unwrap();
    assert_eq!(enter.recv_timeout(Duration::from_secs(5)).unwrap(), 1);
    capture_done.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(done.load(Ordering::Acquire), 1, "real capture committed before all selected callbacks finished");
    replay.join().unwrap().unwrap();
    checkpoint.join().unwrap().unwrap();
    releases[1].send(()).unwrap();
    for n in 2..8 {
        assert_eq!(enter.recv_timeout(Duration::from_secs(5)).unwrap(), n);
        releases[n].send(()).unwrap();
    }
    for job in jobs {
        job.join().unwrap().unwrap();
    }
    waiters(&owner, [0; 3]);
    let head=owner.with(|client| {
        let row=client.query_one("select o.observation_id,o.epoch,o.catalog_nonce,o.observed_at_ms,(select count(*) from native_metric_samples s where s.observation_id=o.observation_id) from native_runtime_observation_current h join native_runtime_observations o using(observation_id) where h.singleton",&[])?;
        assert_eq!(row.get::<_,Vec<u8>>(1).as_slice(),owner.epoch().unwrap().id().as_slice());
        assert_eq!(row.get::<_,Vec<u8>>(2).as_slice(),owner.epoch().unwrap().catalog().nonce().as_slice());
        assert!(row.get::<_,i64>(3)>0&&row.get::<_,i64>(3)<=chrono::Utc::now().timestamp_millis());assert!(row.get::<_,i64>(4)>0);
        assert_eq!(client.query_one("select count(*) from segments",&[])?.get::<_,i64>(0),1);Ok(row.get::<_,i64>(0))
    }).unwrap();
    finish_cache.send(()).unwrap();
    cache.join().unwrap().unwrap();
    let (release, held) = hold_selection(&owner);
    let stopped_owner = owner.clone();
    let stopped = thread::spawn(move || crate::pg::stage_telemetry::capture(&stopped_owner, None));
    waiters(&owner, [0, 1, 0]);
    owner.stop.store(true, Ordering::Release);
    assert!(crate::pg::outage::is_attempt_ended(&stopped.join().unwrap().unwrap_err()));
    waiters(&owner, [0; 3]);
    release.send(()).unwrap();
    held.join().unwrap().unwrap();
    assert_eq!(owner.failure(), None);
    owner.close();
    // A queued stop did not publish a new head or acknowledge a partial frame.
    let storage = crate::config::checked_storage_config().unwrap();
    let mut check = postgres::Client::connect(&storage.url, postgres::NoTls).unwrap();
    check.batch_execute(&format!("set search_path to \"{}\"", storage.schema)).unwrap();
    assert_eq!(
        check
            .query_one("select observation_id from native_runtime_observation_current where singleton", &[])
            .unwrap()
            .get::<_, i64>(0),
        head
    );
}
