//! The six retained mixed oracles, through original native authorities.
//! Register only as a test child of native_workers.rs; no public runtime hook.

#[path = "native_mixed_fixture.rs"]
mod fixture;
use crate::pg::{
    fresh_reader::{Budget, ReadView},
    stage_queue::WalkProgress,
};
use crate::postprocess::step::{NameChange, Needs, Outcome, Reruns, Step, StepCtx, StepError, StepId, mask};
use fixture::{Fixture, now, wait};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicU8, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

type Log = Arc<Mutex<Vec<(String, Option<String>)>>>;
const NAMING: i64 = 1 << 8;
const LATER: i64 = 1 << 20;
const WALK: i64 = 1 << 9;

#[test]
#[ignore = "requires explicit isolated ATLAS_PG_TEST_URL and original native owner"]
fn a_named_row_stays_visible() {
    fixture::run_child("visible");
}
#[test]
#[ignore = "requires explicit isolated ATLAS_PG_TEST_URL and original native owner"]
fn naming_keeps_its_bit_and_failed_later_steps_clear() {
    fixture::run_child("name-bits");
}
#[test]
#[ignore = "requires explicit isolated ATLAS_PG_TEST_URL and original native owner"]
fn later_steps_are_queued() {
    fixture::run_child("name-later");
}
#[test]
#[ignore = "requires explicit isolated ATLAS_PG_TEST_URL and original native worker stop/recovery"]
fn the_stage_stops_for_a_compaction_and_carries_on() {
    fixture::run_child("worker-restart");
}
#[test]
#[ignore = "requires explicit isolated ATLAS_PG_TEST_URL and original native owner"]
fn the_walk_clears_the_mask_and_the_step_runs_again() {
    fixture::run_child("walk");
}
#[test]
#[ignore = "requires explicit isolated ATLAS_PG_TEST_URL and actual native settling"]
fn a_walks_request_waits_for_a_release_still_settling() {
    fixture::run_child("settle");
}
#[test]
#[ignore = "internal exact isolated child; select a retained parent oracle"]
fn fixture_child() {
    let case = std::env::var("ATLAS_NATIVE_MIXED_CASE").expect("select retained native parent oracle");
    match case.as_str() {
        "visible" => visible(),
        "name-bits" => name_bits(),
        "name-later" => name_later(),
        "walk" => walk(),
        "settle" => settle(),
        "worker-restart" => worker_restart(),
        _ => panic!("unknown native mixed case"),
    }
    fixture::mark_finished(&case);
}

#[repr(u8)]
#[derive(Clone, Copy)]
enum Mode {
    Done,
    Retry,
    Permanent,
    Cleared,
    Hold,
    Recover,
}
struct Record {
    id: u8,
    mode: AtomicU8,
    runs: AtomicUsize,
    log: Log,
}
impl Record {
    fn new(id: u8, mode: Mode, log: &Log) -> Arc<Self> {
        Arc::new(Self { id, mode: AtomicU8::new(mode as u8), runs: AtomicUsize::new(0), log: log.clone() })
    }
    fn mode(&self, mode: Mode) {
        self.mode.store(mode as u8, Ordering::Release);
    }
}
impl Step for Record {
    fn id(&self) -> StepId {
        StepId(self.id)
    }
    fn name(&self) -> &'static str {
        "retained-native-record"
    }
    fn rank(&self) -> u16 {
        if self.id == 8 { 200 } else { 400 }
    }
    fn needs(&self) -> Needs {
        Needs::Database
    }
    fn reruns(&self) -> Reruns {
        Reruns { on_rename: true, on_growth: false }
    }
    fn run(&self, ctx: &mut StepCtx<'_>) -> Result<Outcome, StepError> {
        let count = self.runs.fetch_add(1, Ordering::AcqRel);
        self.log.lock().unwrap().push((ctx.release().name.clone(), ctx.release().display_name.clone()));
        let mode = self.mode.load(Ordering::Acquire);
        if mode == Mode::Hold as u8 || (mode == Mode::Recover as u8 && count > 0) {
            let deadline = Instant::now() + Duration::from_secs(20);
            while !ctx.stopping() {
                assert!(Instant::now() < deadline, "held original native Step was not stopped");
                std::thread::sleep(Duration::from_millis(10));
            }
            return Err(StepError::Stopped);
        }
        if mode == Mode::Retry as u8 {
            return Err(StepError::Retry("retained actual retry predecessor".into()));
        }
        if mode == Mode::Permanent as u8 {
            return Err(StepError::Permanent("retained actual failed predecessor".into()));
        }
        if mode == Mode::Cleared as u8 {
            return Ok(Outcome {
                name: Some(NameChange {
                    display_name: None,
                    obfuscated: Some(true),
                    source: crate::naming::NameSource::Cleared.value(),
                }),
                ..Default::default()
            });
        }
        Ok(Outcome::default())
    }
}
fn actual_naming() -> Arc<dyn Step> {
    Arc::new(crate::naming::steps::Naming)
}
fn done(f: &Fixture, id: i64, bit: i64) -> bool {
    f.observer
        .get()
        .expect("native state observer")
        .query_opt("select done from pp_state where release_id=$1", &[&id])
        .expect("actual native state")
        .is_some_and(|row| row.get::<_, i64>(0) & bit == bit)
}
fn source(f: &Fixture, id: i64) -> Option<i64> {
    f.observer.get().unwrap().query_one("select name_source from releases where id=$1", &[&id]).unwrap().get(0)
}
fn close_walk(f: &Fixture) {
    wait("actual native walk EOF", || match f.queue().walk_tick(now()).expect("actual walk EOF") {
        WalkProgress::Idle => true,
        WalkProgress::Finished => false,
        other => panic!("unexpected unfinished walk: {other:?}"),
    });
}
fn run_group(f: &Fixture, runner: &crate::postprocess::native::Runner, id: i64, group: &str, bit: i64) {
    f.walk(group, bit);
    // A redo request does not clear old done bits until its actual leased
    // visit runs. Drive that nomination before observing terminal state.
    assert!(f.visit(runner), "the newly nominated native walk is actually visited");
    f.run_until(runner, || done(f, id, bit));
    close_walk(f);
}
/// Cleared is a supported retained marker; the retired unname command is not
/// resurrected. Commit it through a real leased NameChange on a raw unnamed
/// row after actual Naming has executed, never a SQL metadata/state insertion.
fn cleared(f: &Fixture, id: i64, group: &str) {
    let naming = f.runner(vec![actual_naming()]);
    run_group(f, &naming, id, group, NAMING);
    let marker = Record::new(8, Mode::Cleared, &Log::default());
    let runner = f.runner(vec![marker]);
    f.walk(group, NAMING);
    f.run_until(&runner, || source(f, id) == Some(1));
    close_walk(f);
    assert_eq!(f.bits(id), (NAMING, 0, 0));
}
fn visible() {
    let f = Fixture::open(1);
    let id = f.save("Xk3Mz9PqL", "alt.binaries.mixed.visible", Some("Real.Name.2024"));
    let mut observer = f.observer.get().expect("native visibility observer");
    assert!(observer.query_one("select is_obfuscated from releases where id=$1", &[&id]).unwrap().get::<_, bool>(0));
    assert_eq!(
        observer
            .query_one("select count(*) from releases where is_obfuscated and display_name is null", &[])
            .unwrap()
            .get::<_, i64>(0),
        0,
        "search shows a flagged release that has a real name"
    );
    let view = ReadView::open(
        &mut observer,
        &std::env::var("ATLAS_NATIVE_MIXED_SCHEMA").unwrap(),
        Budget::default(),
        Instant::now() + Duration::from_secs(5),
    )
    .unwrap();
    assert!(view.visible(id).unwrap(), "actual native visibility includes a resolved obfuscated name");
    assert_eq!(view.release(id).unwrap().unwrap().name, "Real.Name.2024");
}
fn name_bits() {
    let f = Fixture::open(1);
    let group = "alt.binaries.mixed.name-bits";
    let id = f.save("Xk3Mz9PqL", group, None);
    cleared(&f, id, group);
    let log = Log::default();
    let later = Record::new(20, Mode::Retry, &log);
    let runner = f.runner(vec![actual_naming(), later.clone()]);
    f.walk(group, LATER);
    f.run_until(&runner, || {
        f.observer
            .get()
            .unwrap()
            .query_one("select coalesce((select db_attempts from pp_state where release_id=$1),0)", &[&id])
            .unwrap()
            .get::<_, i64>(0)
            > 0
    });
    close_walk(&f);
    assert!(f.bits(id).2 > 0, "nonzero attempts come from a genuine Step retry");
    f.rerun(id, LATER);
    assert_eq!(f.bits(id), (NAMING, 0, 0), "Naming stays done; later retry state clears");
    later.mode(Mode::Done);
    f.run_until(&runner, || done(&f, id, LATER));
    assert_eq!(f.bits(id), (NAMING | LATER, 0, 0));
}
fn name_later() {
    let f = Fixture::open(1);
    let done_group = "alt.binaries.mixed.later-done";
    let failed_group = "alt.binaries.mixed.later-failed";
    let a = f.save("Xk3Mz9PqL", done_group, None);
    cleared(&f, a, done_group);
    let b = f.save("Xk3Mz9PqA", failed_group, None);
    cleared(&f, b, failed_group);
    let setup_log = Log::default();
    let later = Record::new(20, Mode::Done, &setup_log);
    let setup = f.runner(vec![actual_naming(), later.clone()]);
    run_group(&f, &setup, a, done_group, LATER);
    later.mode(Mode::Permanent);
    f.walk(failed_group, LATER);
    f.run_until(&setup, || {
        f.observer
            .get()
            .unwrap()
            .query_one("select failed & $2<>0 from pp_state where release_id=$1", &[&b, &LATER])
            .unwrap()
            .get::<_, bool>(0)
    });
    close_walk(&f);
    assert_eq!(f.bits(a), (NAMING | LATER, 0, 0));
    assert_eq!(f.bits(b), (NAMING, LATER, 0));
    f.rerun(a, LATER);
    f.rerun(b, LATER);
    assert_eq!(f.bits(a), (NAMING, 0, 0));
    assert_eq!(f.bits(b), (NAMING, 0, 0));
    assert_eq!(f.observer.get().unwrap().query_one("select count(*) from pp_queue", &[]).unwrap().get::<_, i64>(0), 2);
    let log = Log::default();
    let actual = Record::new(20, Mode::Done, &log);
    let runner = f.runner(vec![actual_naming(), actual]);
    f.run_until(&runner, || done(&f, a, LATER) && done(&f, b, LATER));
    assert_eq!(log.lock().unwrap().len(), 2, "both actual later steps run once");
    let names = f
        .observer
        .get()
        .unwrap()
        .query("select display_name from releases order by id", &[])
        .unwrap()
        .into_iter()
        .map(|row| row.get::<_, Option<String>>(0))
        .collect::<Vec<_>>();
    assert!(names.iter().all(Option::is_none));
    assert!(log.lock().unwrap().iter().all(|(_, display)| display.is_none()));
}
fn walk() {
    let f = Fixture::open(1);
    let group = "alt.binaries.mixed.walk";
    let id = f.save("Rel.One", group, None);
    let log = Log::default();
    let step = Record::new(9, Mode::Done, &log);
    let runner = f.runner(vec![step]);
    run_group(&f, &runner, id, group, WALK);
    let before = log.lock().unwrap().len();
    run_group(&f, &runner, id, group, WALK);
    assert_eq!(log.lock().unwrap().len() - before, 1, "the real step runs exactly once again");
    assert_eq!(f.bits(id).0, WALK);
    assert_eq!(f.observer.get().unwrap().query_one("select count(*) from pp_walks", &[]).unwrap().get::<_, i64>(0), 0);
}
fn settle() {
    let f = Fixture::open(2);
    let group = "alt.binaries.mixed.settle";
    let id = f.save("Rel.One", group, None);
    let log = Log::default();
    let step = Record::new(9, Mode::Done, &log);
    let runner = f.runner(vec![step]);
    f.walk(group, WALK);
    assert!(f.visit(&runner), "fresh actual walk was claimed");
    assert!(log.lock().unwrap().is_empty(), "actual fresh input still settling cannot execute the Step");
    let until: i64 =
        f.observer.get().unwrap().query_one("select due_at from pp_queue where release_id=$1", &[&id]).unwrap().get(0);
    assert!(until > now(), "original Runner preserves its genuine settle deadline");
    close_walk(&f);
    f.run_until(&runner, || done(&f, id, WALK));
    assert_eq!(log.lock().unwrap().len(), 1);
    assert_eq!(f.bits(id).0, WALK);
}
/// The obsolete compaction trigger maps to real native worker/owner drain and
/// restart. Genuine pending work survives; actual native counters must resume.
/// Current source lacks its native Stats/backlog refresh: the final value
/// assertions intentionally expose that publisher gap, never accept defaults.
fn worker_restart() {
    let mut f = Fixture::open(1);
    let a = f.save("Resume.One", "alt.binaries.mixed.resume-a", None);
    let b = f.save("Resume.Two", "alt.binaries.mixed.resume-b", None);
    let c = f.save("Resume.Three", "alt.binaries.mixed.resume-c", None);
    f.rerun(a, WALK);
    f.rerun(b, WALK);
    f.rerun(c, WALK);
    let log = Log::default();
    let held = Record::new(9, Mode::Hold, &log);
    let first = f.runner(vec![held.clone()]);
    let stage =
        super::Workers::start_runner(first.clone(), f.runtime().db.clone(), f.runtime().configured_headers()).unwrap();
    wait("actual original native worker enters its Step", || held.runs.load(Ordering::Acquire) > 0);
    let before = f
        .observer
        .get()
        .unwrap()
        .query_one("select count(*) from pp_state where done & $1<>0", &[&WALK])
        .unwrap()
        .get::<_, i64>(0);
    stage.finish().unwrap();
    assert_eq!(f.runtime().owner.failure(), None);
    f.reopen();
    let resumed = Record::new(9, Mode::Recover, &Log::default());
    let runner = f.runner(vec![resumed.clone()]);
    let stage =
        super::Workers::start_runner(runner.clone(), f.runtime().db.clone(), f.runtime().configured_headers()).unwrap();
    wait("actual recovered worker commits and reaches next pending Step", || resumed.runs.load(Ordering::Acquire) >= 2);
    let mut observer = f.observer.get().unwrap();
    assert!(
        observer.query_one("select count(*) from pp_state where done & $1<>0", &[&WALK]).unwrap().get::<_, i64>(0)
            > before,
        "stage carries on after real native owner recovery"
    );
    crate::pg::stage_telemetry::capture(&f.runtime().owner, Some(&stage.lanes)).unwrap();
    let view = ReadView::open(
        &mut observer,
        &std::env::var("ATLAS_NATIVE_MIXED_SCHEMA").unwrap(),
        Budget { rows: 64, bytes: 64 * 1024 * 1024 },
        Instant::now() + Duration::from_secs(5),
    )
    .unwrap();
    let native = view.live_statistics().unwrap();
    assert_eq!(native.catalog.total, 3);
    match native.pace {
        crate::web_pending::NativePace::Warming { enabled } => assert_eq!(enabled, WALK),
        crate::web_pending::NativePace::Measured(pace) => assert_eq!(pace.enabled, Some(WALK)),
        crate::web_pending::NativePace::Stopped { .. } => panic!("actual recovered Stage was reported stopped"),
    }
    drop(view);
    let queue_due = observer
        .query_one(
            "select count(*) from pp_queue q where due_at<=$1 and (lease_until is null or lease_until<=$1)
         and not exists(select 1 from pp_state s where s.release_id=q.release_id and s.native_net_until>$1)",
            &[&now()],
        )
        .unwrap()
        .get::<_, i64>(0);
    assert_eq!(queue_due, 1, "one genuinely unclaimed actionable release remains beside the actual held Step");
    // Observe the actual one-second producer's completed payload on its normal
    // standard worker lane, never a manual counter or file injection.
    wait("actual native producer observes stable actionable work", || {
        runner.stats.snapshot(0, 100, WALK)["queue_due"].as_i64() == Some(queue_due)
            && crate::postprocess::stats::backlog() == queue_due
    });
    let snapshot = runner.stats.snapshot(0, 100, WALK);
    assert!(snapshot["queue_due"].is_i64());
    assert!(snapshot["walk"].is_object());
    assert_eq!(
        snapshot["queue_due"].as_i64(),
        Some(queue_due),
        "native stats must publish actual queue work, not defaults"
    );
    assert_eq!(
        crate::postprocess::stats::backlog(),
        queue_due,
        "status backlog must reflect actual native pending work"
    );
    stage.finish().unwrap();
    assert_eq!(f.runtime().owner.failure(), None);
}
