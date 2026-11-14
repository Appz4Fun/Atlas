//! Source-first controls for callback admission; PostgreSQL cases are explicitly opt-in.
use super::*;
use crate::pg::outage;
use std::{
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

fn queued(admission: &Admission, wanted: [usize; 2]) {
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        let sizes = {
            let state = admission.state.lock().unwrap();
            [state.waiting[0].len(), state.waiting[1].len()]
        };
        if sizes == wanted {
            return;
        }
        assert!(Instant::now() < until, "admission queues {sizes:?}, expected {wanted:?}");
        thread::yield_now();
    }
}

#[test]
fn fifo_classes_alternate_without_starving_ordinary_heartbeat_work() {
    let admission = Arc::new(Admission::default());
    let stop = Arc::new(AtomicBool::new(false));
    let accepting = Arc::new(AtomicBool::new(true));
    let held = admission.enter(1, &stop, &accepting).unwrap();
    let (sent, received) = mpsc::channel();
    let mut jobs = Vec::new();
    let mut sizes = [0, 0];
    // All ordinary callers are already queued before the two ingestion callers arrive.
    for (class, name) in [(1, "frontier-1"), (1, "heartbeat"), (1, "frontier-2"), (0, "save"), (0, "cursor")] {
        let (gate, stop, accepting, sent) = (admission.clone(), stop.clone(), accepting.clone(), sent.clone());
        jobs.push(thread::spawn(move || {
            let _turn = gate.enter(class, &stop, &accepting).unwrap();
            sent.send(name).unwrap();
        }));
        sizes[class] += 1;
        queued(&admission, sizes);
    }
    drop(held);
    let order: Vec<_> = (0..5).map(|_| received.recv_timeout(Duration::from_secs(5)).unwrap()).collect();
    assert_eq!(order, ["save", "frontier-1", "cursor", "heartbeat", "frontier-2"]);
    for job in jobs {
        job.join().unwrap()
    }
    queued(&admission, [0, 0]);
    assert!(!admission.state.lock().unwrap().active);
}

#[test]
fn stopped_queued_turn_and_panicked_active_turn_leave_no_queue_entries() {
    let gate = Arc::new(Admission::default());
    let stop = Arc::new(AtomicBool::new(false));
    let accepting = Arc::new(AtomicBool::new(true));
    let held = gate.enter(1, &stop, &accepting).unwrap();
    let (sent, received) = mpsc::channel();
    let (g, s, a) = (gate.clone(), stop.clone(), accepting.clone());
    let waiting = thread::spawn(move || {
        let error = g.enter(0, &s, &a).err().expect("stopped caller cannot get a turn");
        sent.send(outage::is_attempt_ended(&error)).unwrap();
    });
    queued(&gate, [1, 0]);
    stop.store(true, Ordering::Release);
    // Direct operator stop writes the existing atomic; timed condvar wake must not need the active PG call to end.
    assert!(received.recv_timeout(Duration::from_secs(2)).unwrap());
    waiting.join().unwrap();
    queued(&gate, [0, 0]);
    assert!(gate.state.lock().unwrap().active, "queued cancellation cannot release another caller's turn");
    drop(held);
    stop.store(false, Ordering::Release);
    let result = std::panic::catch_unwind(|| {
        let _turn = gate.enter(0, &stop, &accepting).unwrap();
        panic!("fixture callback panic");
    });
    assert!(result.is_err());
    let next = gate.enter(1, &stop, &accepting).unwrap();
    drop(next);
    queued(&gate, [0, 0]);
    assert!(!gate.state.lock().unwrap().active);
}

fn held_owner(owner: &Arc<Owner>) -> (mpsc::Sender<()>, thread::JoinHandle<Result<()>>) {
    let (sent, received) = mpsc::channel();
    let (release, wait) = mpsc::channel();
    let owner = owner.clone();
    let job = thread::spawn(move || {
        owner.with(|_| {
            sent.send(())?;
            wait.recv_timeout(Duration::from_secs(10))?;
            Ok(())
        })
    });
    received.recv_timeout(Duration::from_secs(5)).unwrap();
    (release, job)
}
fn input() -> Vec<crate::parser::Release> {
    vec![crate::parser::Release {
        name: "Admission.Example".into(),
        group: "example.admission".into(),
        articles: vec![crate::parser::Article {
            filename: Some("sample.bin".into()),
            message_id: "<admission@example.test>".into(),
            part: Some(1),
            total_parts: Some(1),
            bytes: 17,
            ..Default::default()
        }],
        ..Default::default()
    }]
}

#[test]
#[ignore = "requires explicit isolated PostgreSQL; same-session contention and real writer/cursor semantics"]
fn original_owner_prioritizes_save_and_checkpoint_but_serves_fifo_frontier_and_heartbeat() {
    let schema = crate::pg::testing::Schema::new().expect("explicit isolated PostgreSQL required");
    let runtime = crate::pg::stage_queue_tests::runtime(&schema);
    let owner = runtime.owner.clone();
    let pid = owner.with(|client| Ok(client.query_one("select pg_backend_pid()", &[])?.get::<_, i32>(0))).unwrap();
    let (release, held) = held_owner(&owner);
    let (sent, received) = mpsc::channel();
    let mut jobs = Vec::new();
    for (n, name) in ["frontier-1", "heartbeat", "frontier-2"].into_iter().enumerate() {
        let (owner, sent) = (owner.clone(), sent.clone());
        jobs.push(thread::spawn(move || {
            owner.with(|client| {
                assert_eq!(client.query_one("select pg_backend_pid()", &[])?.get::<_, i32>(0), pid);
                if name.starts_with("frontier") {
                    assert_eq!(
                        crate::chunks::frontier_oldest(
                            &crate::pg::sql::Pg::new(&mut *client),
                            &[("example.admission".into(), 0)],
                            100
                        )?,
                        None
                    );
                }
                sent.send(name)?;
                Ok(())
            })
        }));
        queued(&owner.admission, [0, n + 1]);
    }
    let save_owner = owner.clone();
    let save_sent = sent.clone();
    let save = thread::spawn(move || {
        save_owner.with_ingest(|client| {
            assert_eq!(client.query_one("select pg_backend_pid()", &[])?.get::<_, i32>(0), pid);
            assert_eq!(crate::pg::release_writer::save(client, &input(), 1)?.parts, 1);
            save_sent.send("save")?;
            Ok(())
        })
    });
    queued(&owner.admission, [1, 3]);
    let db = runtime.db.clone();
    let cursor_sent = sent.clone();
    let cursor = thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(
            crate::indexer::on_ingest_progress(&db, move |sql| {
                crate::db::update_backfill_cursor(sql, "example.admission", 1999)?;
                cursor_sent.send("cursor")?;
                Ok(())
            }),
        )
    });
    queued(&owner.admission, [2, 3]);
    release.send(()).unwrap();
    held.join().unwrap().unwrap();
    assert_eq!(
        (0..5).map(|_| received.recv_timeout(Duration::from_secs(5)).unwrap()).collect::<Vec<_>>(),
        ["save", "frontier-1", "cursor", "heartbeat", "frontier-2"]
    );
    save.join().unwrap().unwrap();
    cursor.join().unwrap().unwrap();
    for job in jobs {
        job.join().unwrap().unwrap();
    }
    owner
        .with(|client| {
            assert_eq!(client.query_one("select count(*),sum(bytes) from segments", &[])?.get::<_, i64>(0), 1);
            assert_eq!(
                client
                    .query_one("select backfill_cursor from groups where name='example.admission'", &[])?
                    .get::<_, i64>(0),
                1999
            );
            assert_eq!(
                crate::pg::release_writer::save(client, &input(), 2)?.parts,
                0,
                "normal replay remains deduplicated"
            );
            Ok(())
        })
        .unwrap();
    assert_eq!(owner.failure(), None);
    owner.close();
}

#[test]
#[ignore = "requires explicit isolated PostgreSQL; detached save retains original shared permits through real commit"]
fn cancelled_save_keeps_admission_and_slot_until_original_owner_commit_drains() {
    let schema = crate::pg::testing::Schema::new().expect("explicit isolated PostgreSQL required");
    let runtime = crate::pg::stage_queue_tests::runtime(&schema);
    let native = crate::pg::ingest::Native::with_owner(
        runtime.owner.clone(),
        crate::pg::ingest::PgWriteLimits { max_pending_bytes: 16384, max_job_bytes: 8192, concurrent_saves: 1 },
    )
    .unwrap();
    let (release, held) = held_owner(&runtime.owner);
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap();
    let writer = native.clone();
    let save = rt.spawn(async move { writer.save(input()).await });
    queued(&runtime.owner.admission, [1, 0]);
    save.abort();
    assert!(rt.block_on(save).unwrap_err().is_cancelled());
    rt.block_on(async {
        let mut next = Box::pin(native.reserve_native_for_owner(&runtime.owner));
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut next).await.is_err(),
            "cancelling the caller cannot release its blocking save slot"
        );
        release.send(()).unwrap();
        held.join().unwrap().unwrap();
        let permit = tokio::time::timeout(Duration::from_secs(5), next).await.unwrap().unwrap();
        drop(permit);
    });
    runtime
        .owner
        .with(|client| {
            assert_eq!(client.query_one("select count(*) from segments", &[])?.get::<_, i64>(0), 1);
            Ok(())
        })
        .unwrap();
    queued(&runtime.owner.admission, [0, 0]);
    assert_eq!(runtime.owner.failure(), None);
    runtime.owner.close();
}

#[test]
#[ignore = "requires explicit isolated PostgreSQL; fail and close wake queued callbacks without abandoning active work"]
fn fail_and_close_wake_queued_callers_preserve_failure_and_drain_active_client() {
    for failure in [None, Some(Failure::Unavailable), Some(Failure::Fatal)] {
        let schema = crate::pg::testing::Schema::new().expect("explicit isolated PostgreSQL required");
        let runtime = crate::pg::stage_queue_tests::runtime(&schema);
        let (release, held) = held_owner(&runtime.owner);
        let owner = runtime.owner.clone();
        let (sent, received) = mpsc::channel();
        let waiting = thread::spawn(move || {
            let result: Result<()> = owner.with_ingest(|_| panic!("ended queued callback ran"));
            sent.send(result.err().is_some_and(|e| outage::is_attempt_ended(&e))).unwrap();
        });
        queued(&runtime.owner.admission, [1, 0]);
        let close = if let Some(failure) = failure {
            runtime.owner.fail(failure);
            None
        } else {
            let owner = runtime.owner.clone();
            Some(thread::spawn(move || owner.close()))
        };
        assert!(received.recv_timeout(Duration::from_secs(2)).unwrap());
        waiting.join().unwrap();
        queued(&runtime.owner.admission, [0, 0]);
        assert!(runtime.owner.admission.state.lock().unwrap().active, "stop must not abandon active client");
        if let Some(close) = &close {
            assert!(!close.is_finished());
        }
        release.send(()).unwrap();
        held.join().unwrap().unwrap();
        if let Some(close) = close {
            close.join().unwrap();
        }
        assert_eq!(runtime.owner.failure(), failure);
        runtime.owner.close();
        assert!(runtime.owner.with_ingest(|_| Ok(())).is_err());
    }
}

#[test]
#[ignore = "requires explicit isolated PostgreSQL; scheduled SQL failure preserves driver cause and real rollback"]
fn ingestion_turn_sql_failure_rolls_back_and_stays_fatal() {
    let schema = crate::pg::testing::Schema::new().expect("explicit isolated PostgreSQL required");
    let runtime = crate::pg::stage_queue_tests::runtime(&schema);
    let error = runtime
        .owner
        .with_ingest(|client| {
            let mut tx = client.transaction()?;
            tx.execute("insert into meta(key,value) values('admission_rollback',17)", &[])?;
            tx.batch_execute("select 1/0")?;
            tx.commit()?;
            Ok(())
        })
        .unwrap_err();
    assert!(error.chain().any(|cause| cause.downcast_ref::<postgres::Error>().is_some()));
    assert_eq!(runtime.owner.failure(), Some(Failure::Fatal));
    let mut observer = schema.pool.get().unwrap();
    assert_eq!(
        observer.query_one("select count(*) from meta where key='admission_rollback'", &[]).unwrap().get::<_, i64>(0),
        0
    );
    queued(&runtime.owner.admission, [0, 0]);
    assert!(!runtime.owner.admission.state.lock().unwrap().active);
    runtime.owner.close();
}

#[test]
#[ignore = "requires explicit isolated PostgreSQL; real completed observation must progress between ingestion turns"]
fn ordinary_observation_heartbeat_commits_between_two_waiting_ingestion_callbacks() {
    let schema = crate::pg::testing::Schema::new().expect("explicit isolated PostgreSQL required");
    let runtime = crate::pg::stage_queue_tests::runtime(&schema);
    let owner = runtime.owner.clone();
    let (release, held) = held_owner(&owner);
    let observer = owner.clone();
    let heartbeat = thread::spawn(move || crate::pg::stage_telemetry::capture(&observer, None));
    queued(&owner.admission, [0, 1]);
    let first = owner.clone();
    let save = thread::spawn(move || {
        first.with_ingest(|client| {
            assert!(
                client
                    .query_opt("select observation_id from native_runtime_observation_current where singleton", &[])?
                    .is_none()
            );
            assert_eq!(crate::pg::release_writer::save(client, &input(), 1)?.parts, 1);
            Ok(())
        })
    });
    queued(&owner.admission, [1, 1]);
    let second = owner.clone();
    let checkpoint = thread::spawn(move || {
        second.with_ingest(|client| {
        let head = client.query_one("select o.epoch,o.catalog_nonce from native_runtime_observation_current c join native_runtime_observations o using(observation_id) where c.singleton", &[])?;
        assert_eq!(head.get::<_,Vec<u8>>(0).as_slice(), second.epoch().unwrap().id().as_slice());
        assert_eq!(head.get::<_,Vec<u8>>(1).as_slice(), second.epoch().unwrap().catalog().nonce().as_slice());
        crate::db::update_backfill_cursor(&crate::pg::sql::Pg::new(client), "example.admission", 1999)?;
        Ok(())
    })
    });
    queued(&owner.admission, [2, 1]);
    release.send(()).unwrap();
    held.join().unwrap().unwrap();
    save.join().unwrap().unwrap();
    heartbeat.join().unwrap().unwrap();
    checkpoint.join().unwrap().unwrap();
    assert_eq!(owner.failure(), None);
    owner.close();
}
