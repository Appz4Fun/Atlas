//! The database lane and the walks, on a stage with steps these tests write.

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use atlas::config::{Config, PostprocessConfig};
use atlas::postprocess::step::{NameChange, Needs, Outcome, StepError, StepId, StepWrite, mask};
use common::stage::*;

fn log() -> Log {
    Default::default()
}

fn walks(h: &Harness) -> Vec<(String, i64)> {
    let conn = h.shard();
    let mut stmt = conn.prepare("select kind, redo_mask from pp_walks").unwrap();
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().map(|r| r.unwrap()).collect()
}

fn no_walks() -> PostprocessConfig {
    PostprocessConfig { backlog_per_second: 0, ..quick_config() }
}

#[test]
fn a_settled_release_runs_its_steps_once_in_rank_order() {
    let log = log();
    let mut h = fresh();
    let (first, second) = (
        TestStep::new(9, "first", 200, Needs::Database, &log),
        TestStep::new(16, "second", 300, Needs::Database, &log),
    );
    h.save(&[complete("Rel.One")]);
    h.start(vec![second.arc(), first.arc()], quick_config());
    wait_for("both ran", || runs_of(&log, "second") == 1);
    std::thread::sleep(Duration::from_millis(1500));
    let names: Vec<&str> = log.lock().unwrap().iter().map(|(s, ..)| *s).collect();
    assert_eq!(names, vec!["first", "second"], "once each, by rank");
    assert_eq!(h.bits("Rel.One").unwrap().0, mask(StepId(9)) | mask(StepId(16)));
}

#[test]
fn incomplete_small_and_image_releases_run_database_steps() {
    let log = log();
    let mut h = fresh();
    let mut small = complete("Small.One");
    small.articles.iter_mut().for_each(|a| a.bytes = 5_000);
    h.save(&[release_of("Incomplete.One", "inc.bin", 1, 3), small, release_of("Picture.One", "pic.jpg", 1, 1)]);
    let step = TestStep::new(9, "db", 200, Needs::Database, &log);
    h.start(vec![step.arc()], quick_config());
    let bit = mask(StepId(9));
    wait_for("all three", || {
        ["Incomplete.One", "Small.One", "Picture.One"].iter().all(|n| h.bits(n).is_some_and(|b| b.0 == bit))
    });
}

#[test]
fn a_fragment_a_blocked_group_and_an_empty_release_run_no_step_and_leave_no_row() {
    let log = log();
    let mut h = fresh();
    let token = "a1B2c3D4e5F6g7H8i9J0k1L2m3N4o5P6";
    let mut fragment = release_of(token, token, 0, 9);
    fragment.articles = vec![article(token, 7, 9, "<frag@x>")];
    let old = h.shard();
    h.save(&[fragment, complete("Good.One")]);
    // an empty one, and a row from before the block
    old.execute_batch(
        "insert into releases (id, name, group_name, parts) values (900000, 'Empty.One', 'alt.binaries.t', 0);
         insert into releases (id, name, group_name, parts, size) values (900001, 'Hidden.One', 'alt.binaries.erotica.teen', 1, 10);
         insert into pp_queue (release_id, due_at) values (900000, 1), (900001, 1);",
    )
    .unwrap();
    let step = TestStep::new(9, "db", 200, Needs::Database, &log);
    h.start(vec![step.arc()], quick_config());
    wait_for("the good one", || runs_of(&log, "db") == 1);
    wait_for("the queue empties", || {
        old.query_row("select count(*) from pp_queue", [], |r| r.get::<_, i64>(0)).unwrap() == 0
    });
    let states: i64 = old.query_row("select count(*) from pp_state", [], |r| r.get(0)).unwrap();
    assert_eq!(states, 1, "only the good one has a row");
    assert_eq!(log.lock().unwrap()[0].1, "Good.One");
}

#[test]
fn applies_false_is_not_run_and_not_marked_done() {
    let log = log();
    let mut h = fresh();
    h.save(&[complete("Rel.One")]);
    let step = TestStep::new(9, "db", 200, Needs::Database, &log).applies(|_| false);
    h.start(vec![step.arc()], quick_config());
    let conn = h.shard();
    wait_for("the queue empties", || {
        conn.query_row("select count(*) from pp_queue", [], |r| r.get::<_, i64>(0)).unwrap() == 0
    });
    assert_eq!(runs_of(&log, "db"), 0);
    assert_eq!(h.bits("Rel.One"), None, "no pp_state row");
}

#[test]
fn a_growing_release_waits_and_a_late_article_requeues_it_after_done() {
    let log = log();
    let mut h = fresh();
    h.save(&[complete("Rel.One")]);
    let grows = TestStep::new(9, "grows", 200, Needs::Database, &log).reruns(false, true);
    let stays = TestStep::new(16, "stays", 300, Needs::Database, &log);
    h.start(vec![grows.arc(), stays.arc()], quick_config());
    wait_for("first run", || runs_of(&log, "stays") == 1);
    wait_for("its row", || h.bits("Rel.One").is_some());
    h.save(&[release_of("Rel.One", "more.bin", 2, 2)]);
    wait_for("on_growth ran again", || runs_of(&log, "grows") == 2);
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(runs_of(&log, "stays"), 1, "others do not");
}

#[test]
fn an_article_during_a_run_is_not_lost() {
    let log = log();
    let mut h = fresh();
    h.save(&[complete("Rel.One")]);
    let (release, released) = std::sync::mpsc::channel::<()>();
    let released = Mutex::new(released);
    let runs = Arc::new(AtomicUsize::new(0));
    let r = runs.clone();
    let step = TestStep::new(9, "db", 200, Needs::Database, &log).reruns(false, true).runs(move |_| {
        if r.fetch_add(1, Ordering::SeqCst) == 0 {
            released.lock().unwrap().recv().unwrap();
        }
        Ok(Outcome::default())
    });
    h.start(vec![step.arc()], quick_config());
    wait_for("the run starts", || runs.load(Ordering::SeqCst) == 1);
    h.save(&[release_of("Rel.One", "more.bin", 2, 2)]);
    release.send(()).unwrap();
    wait_for("it runs again", || runs.load(Ordering::SeqCst) == 2);
}

#[test]
fn a_panicking_step_is_retried_with_backoff_then_failed_and_later_steps_still_run() {
    let log = log();
    let mut h = fresh();
    h.save(&[complete("Rel.One")]);
    let bad = TestStep::new(9, "bad", 200, Needs::Database, &log).runs(|_| panic!("boom"));
    let next = TestStep::new(16, "next", 300, Needs::Database, &log);
    h.start(vec![bad.arc(), next.arc()], PostprocessConfig { max_attempts: 2, ..quick_config() });
    wait_for("its first attempt", || h.bits("Rel.One").is_some());
    assert_eq!(h.field("Rel.One", "db_attempts"), 1);
    let conn = h.shard();
    let id = h.id_of("Rel.One");
    let now = chrono::Utc::now().timestamp();
    let due: i64 = conn.query_row("select due_at from pp_queue where release_id = ?", [id], |r| r.get(0)).unwrap();
    assert!((due - now - 900).abs() <= 62, "about 15 minutes off, {}", due - now);
    assert_eq!(runs_of(&log, "next"), 1, "the rank-next step ran");
    conn.execute("update pp_queue set due_at = 1", []).unwrap();
    wait_for("the failed bit", || h.bits("Rel.One").is_some_and(|b| b.1 == mask(StepId(9))));
    let error: String = conn.query_row("select last_error from pp_state", [], |r| r.get(0)).unwrap();
    assert!(error.contains("boom"), "{error}");
}

#[test]
fn a_transient_error_in_the_db_lane_uses_no_attempt_and_no_pause() {
    let log = log();
    let mut h = fresh();
    h.save(&[complete("Rel.One"), complete("Rel.Two")]);
    let flaky = TestStep::new(9, "flaky", 200, Needs::Database, &log).runs(|ctx| {
        if ctx.release().name == "Rel.One" {
            Err(StepError::Transient("locked".into()))
        } else {
            Ok(Outcome::default())
        }
    });
    let next = TestStep::new(16, "next", 300, Needs::Database, &log);
    h.start(vec![flaky.arc(), next.arc()], quick_config());
    wait_for("the other release", || h.bits("Rel.Two").is_some_and(|b| b.0 == mask(StepId(9)) | mask(StepId(16))));
    wait_for("its row", || h.bits("Rel.One").is_some());
    assert_eq!(h.field("Rel.One", "db_attempts"), 0);
    let now = chrono::Utc::now().timestamp();
    let due: i64 = h
        .shard()
        .query_row("select due_at from pp_queue where release_id = ?", [h.id_of("Rel.One")], |r| r.get(0))
        .unwrap();
    assert!((due - now - 300).abs() <= 5, "{}", due - now);
    assert_eq!(h.bits("Rel.One").unwrap().0, mask(StepId(16)), "the same run's next step finished");
}

#[test]
fn a_rename_runs_later_steps_once_on_the_new_name() {
    let log = log();
    let mut h = fresh();
    h.save(&[complete("Rel.One")]);
    let conn = h.shard();
    let id = h.id_of("Rel.One");
    conn.execute("insert into pp_state (release_id, done) values (?, ?)", rusqlite::params![id, mask(StepId(24))])
        .unwrap();
    let renames = TestStep::new(9, "renames", 200, Needs::Database, &log).runs(|_| {
        Ok(Outcome {
            matched: true,
            name: Some(NameChange { display_name: Some("New.Name".into()), obfuscated: None, source: 40 }),
            facts: vec![("seen", "yes".into())],
            ..Default::default()
        })
    });
    let fact = Arc::new(Mutex::new(None));
    let f = fact.clone();
    let later = TestStep::new(24, "later", 400, Needs::Database, &log).reruns(true, false).runs(move |ctx| {
        *f.lock().unwrap() = ctx.fact("seen").map(String::from);
        Ok(Outcome::default())
    });
    let net = TestStep::new(32, "net", 500, Needs::External, &log).reruns(true, false);
    h.start(vec![renames.arc(), later.arc(), net.arc()], quick_config());
    wait_for("the later step", || runs_of(&log, "later") == 1);
    assert_eq!(
        log.lock().unwrap().iter().find(|(s, ..)| *s == "later").unwrap().2.as_deref(),
        Some("New.Name"),
        "sees the new name"
    );
    assert_eq!(fact.lock().unwrap().as_deref(), Some("yes"), "facts_reach_later_steps");
    wait_for("the net lane", || runs_of(&log, "net") >= 1);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(runs_of(&log, "later"), 1, "once");
    let queued: i64 = conn.query_row("select count(*) from pp_queue", [], |r| r.get(0)).unwrap();
    assert_eq!(queued, 0, "no queue row is left for it");
}

#[test]
fn a_queue_row_whose_release_is_gone_is_cleared() {
    let log = log();
    let mut h = fresh();
    let conn = h.shard();
    conn.execute("insert into pp_queue (release_id, due_at) values (?, 1)", [store_id()]).unwrap();
    h.start(vec![TestStep::new(9, "db", 200, Needs::Database, &log).arc()], quick_config());
    wait_for("it goes", || conn.query_row("select count(*) from pp_queue", [], |r| r.get::<_, i64>(0)).unwrap() == 0);
}

fn store_id() -> i64 {
    atlas::store::global_id(5, atlas::store::shard_of(STAGE_GROUP))
}

/// complete releases, quiet by now, with no queue rows, and a walk over them
fn walkable(h: &Harness, n: usize) {
    let releases: Vec<_> = (0..n).map(|i| complete(&format!("Rel.{i:04}"))).collect();
    h.save(&releases);
    std::thread::sleep(Duration::from_millis(1200));
    let conn = h.shard();
    conn.execute_batch(
        "delete from pp_queue;
         insert into pp_walks (kind, top_id, before_id, created_at)
         select 'test', t, t, 1 from (select max(id) + 1 as t from releases);
         insert or replace into meta (key, value) values ('pp_walk_seeded', 1);
         insert or replace into meta (key, value) values ('pp_steps_seen', 512);",
    )
    .unwrap();
}

#[test]
fn the_walk_goes_newest_first_writes_nothing_for_rejected_rows_and_ends() {
    let log = log();
    let mut h = fresh();
    walkable(&h, 300);
    let token = "a1B2c3D4e5F6g7H8i9J0k1L2m3N4o5P6";
    let conn = h.shard();
    let top: i64 = conn.query_row("select max(id) from releases", [], |r| r.get(0)).unwrap();
    // a fragment in the walk, stored as an older build left it
    let mut fragment = release_of(token, token, 0, 9);
    fragment.articles = vec![article(token, 7, 9, "<frag@x>")];
    h.save(&[fragment]);
    std::thread::sleep(Duration::from_millis(1200));
    conn.execute_batch(
        "delete from pp_queue; update pp_walks set top_id = top_id + 1000000, before_id = before_id + 1000000",
    )
    .unwrap();
    let step = TestStep::new(9, "db", 200, Needs::Database, &log);
    h.start(vec![step.arc()], PostprocessConfig { workers: 1, ..quick_config() });
    wait_for("the walk ends", || {
        conn.query_row("select count(*) from pp_walks", [], |r| r.get::<_, i64>(0)).unwrap() == 0
    });
    let names: Vec<String> = log.lock().unwrap().iter().map(|(_, n, _)| n.clone()).collect();
    assert_eq!(names.len(), 300);
    let mut sorted = names.clone();
    sorted.sort();
    sorted.reverse();
    assert_eq!(names, sorted, "newest first");
    assert!(top > 0);
    let (states, queued): (i64, i64) = conn
        .query_row("select (select count(*) from pp_state), (select count(*) from pp_queue)", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!((states, queued), (300, 0));
}

#[test]
fn a_walk_tick_commits_its_results_with_its_cursor() {
    let log = log();
    let mut h = fresh();
    walkable(&h, 20);
    let conn = h.shard();
    let before: i64 = conn.query_row("select before_id from pp_walks", [], |r| r.get(0)).unwrap();
    conn.execute_batch("create table side (id INTEGER PRIMARY KEY)").unwrap();
    let step = TestStep::new(9, "db", 200, Needs::Database, &log).runs(|ctx| {
        Ok(Outcome {
            writes: vec![StepWrite::new("insert or ignore into side (id) values (?)", vec![ctx.release().id.into()])],
            ..Default::default()
        })
    });
    h.start(vec![step.arc()], PostprocessConfig { backlog_per_second: 8, workers: 1, ..quick_config() });
    wait_for("a tick", || {
        conn.query_row("select seen from pp_walks", [], |r| r.get::<_, i64>(0)).map(|s| s > 0).unwrap_or(true)
    });
    h.stop();
    // every row the cursor passed has its write, and only those
    let (cursor, seen): (i64, i64) =
        conn.query_row("select before_id, seen from pp_walks", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    assert!(cursor < before);
    let written: i64 = conn.query_row("select count(*) from side where id >= ?", [cursor], |r| r.get(0)).unwrap();
    assert_eq!(written, seen);
    let all: i64 = conn.query_row("select count(*) from side", [], |r| r.get(0)).unwrap();
    assert_eq!(all, seen);
}

#[test]
fn fresh_rows_are_not_held_up_by_a_walk() {
    let log = log();
    let mut h = fresh();
    walkable(&h, 200);
    let step = TestStep::new(9, "db", 200, Needs::Database, &log).runs(|_| {
        std::thread::sleep(Duration::from_millis(5));
        Ok(Outcome::default())
    });
    h.start(vec![step.arc()], PostprocessConfig { backlog_per_second: 40, ..quick_config() });
    h.save(&[complete("Fresh.One")]);
    wait_for("the fresh one", || log.lock().unwrap().iter().any(|(_, n, _)| n == "Fresh.One"));
    let walks_left: i64 = h.shard().query_row("select count(*) from pp_walks", [], |r| r.get(0)).unwrap();
    assert_eq!(walks_left, 1, "with the walk still going");
}

#[test]
fn a_restart_resumes_the_walk() {
    let log = log();
    let mut h = fresh();
    walkable(&h, 40);
    let conn = h.shard();
    let start = || TestStep::new(9, "db", 200, Needs::Database, &log).arc();
    h.start(vec![start()], PostprocessConfig { backlog_per_second: 8, ..quick_config() });
    wait_for("a tick", || conn.query_row("select seen from pp_walks", [], |r| r.get::<_, i64>(0)).unwrap() > 0);
    h.stop();
    let kept: i64 = conn.query_row("select before_id from pp_walks", [], |r| r.get(0)).unwrap();
    let ran = log.lock().unwrap().len();
    h.start(vec![start()], quick_config());
    wait_for("it ends", || conn.query_row("select count(*) from pp_walks", [], |r| r.get::<_, i64>(0)).unwrap() == 0);
    assert_eq!(log.lock().unwrap().len(), 40, "each once");
    assert!(ran > 0 && kept > 0);
}

#[test]
fn a_new_rows_id_above_top_id_is_left_to_the_queue() {
    let log = log();
    let mut h = fresh();
    walkable(&h, 5);
    let conn = h.shard();
    let top: i64 = conn.query_row("select top_id from pp_walks", [], |r| r.get(0)).unwrap();
    h.save(&[complete("Above.One")]);
    let above = h.id_of("Above.One");
    assert!(above >= top);
    assert_eq!(conn.query_row("select count(*) from pp_queue", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
    h.start(vec![TestStep::new(9, "db", 200, Needs::Database, &log).arc()], quick_config());
    wait_for("all", || runs_of(&log, "db") == 6);
}

#[test]
fn the_first_start_seeds_one_walk() {
    let log = log();
    let mut h = fresh();
    h.save(&[complete("Rel.One")]);
    let steps = || {
        vec![
            TestStep::new(9, "a", 200, Needs::Database, &log).arc(),
            TestStep::new(16, "b", 300, Needs::Database, &log).arc(),
        ]
    };
    h.start(steps(), no_walks());
    let conn = h.shard();
    assert_eq!(walks(&h), vec![("backlog".to_string(), 0)]);
    let seen: i64 = conn.query_row("select value from meta where key = 'pp_steps_seen'", [], |r| r.get(0)).unwrap();
    assert_eq!(seen, mask(StepId(9)) | mask(StepId(16)));
    h.stop();
    h.start(steps(), no_walks());
    assert_eq!(walks(&h).len(), 1, "a second start adds none");
}

#[test]
fn a_step_turning_on_starts_one_new_walk() {
    let log = log();
    let mut h = fresh();
    let second = TestStep::new(16, "b", 300, Needs::Database, &log);
    let switch = second.on.clone();
    *switch.lock().unwrap() = false;
    h.start(vec![TestStep::new(9, "a", 200, Needs::Database, &log).arc(), second.arc()], no_walks());
    assert_eq!(walks(&h).len(), 1);
    *switch.lock().unwrap() = true;
    let config = Config { postprocess: no_walks(), ..Config::default() };
    h.stage.as_ref().unwrap().set_config(&config);
    assert_eq!(walks(&h)[1], ("steps".to_string(), 0));
    h.stage.as_ref().unwrap().set_config(&config);
    assert_eq!(walks(&h).len(), 2, "the same mask again adds none");
}

#[test]
fn a_new_stamp_bit_redoes_that_step_once() {
    let log = log();
    let mut h = fresh();
    let step = TestStep::new(9, "a", 200, Needs::Database, &log);
    let stamp = step.stamp.clone();
    *stamp.lock().unwrap() = 1;
    h.start(vec![step.arc()], no_walks());
    assert_eq!(walks(&h).len(), 1, "stored with no walk at first");
    let config = Config { postprocess: no_walks(), ..Config::default() };
    let set = |v: u64| {
        *stamp.lock().unwrap() = v;
        h.stage.as_ref().unwrap().set_config(&config);
    };
    set(3);
    assert_eq!(walks(&h)[1], ("steps".to_string(), mask(StepId(9))));
    set(3);
    assert_eq!(walks(&h).len(), 2, "the same stamp again adds none");
    set(1);
    assert_eq!(walks(&h).len(), 2, "losing a bit adds none");
}

#[test]
fn enabled_sees_the_full_config_before_the_first_run() {
    struct Limit(Mutex<i64>, Arc<Mutex<Option<i64>>>);
    impl atlas::postprocess::step::Step for Limit {
        fn id(&self) -> StepId {
            StepId(9)
        }
        fn name(&self) -> &'static str {
            "limit"
        }
        fn rank(&self) -> u16 {
            200
        }
        fn needs(&self) -> Needs {
            Needs::Database
        }
        fn enabled(&self, cfg: &Config) -> bool {
            *self.0.lock().unwrap() = cfg.postprocess.max_attempts;
            true
        }
        fn run(&self, _ctx: &mut atlas::postprocess::step::StepCtx<'_>) -> Result<Outcome, StepError> {
            *self.1.lock().unwrap() = Some(*self.0.lock().unwrap());
            Ok(Outcome::default())
        }
    }
    let mut h = fresh();
    h.save(&[complete("Rel.One")]);
    let seen = Arc::new(Mutex::new(None));
    h.start(
        vec![Arc::new(Limit(Mutex::new(0), seen.clone()))],
        PostprocessConfig { max_attempts: 7, ..quick_config() },
    );
    wait_for("it ran", || seen.lock().unwrap().is_some());
    assert_eq!(*seen.lock().unwrap(), Some(7));
}

#[test]
fn a_walk_stopped_by_a_step_doesnt_pass_the_rows_it_left() {
    let log = log();
    let mut h = fresh();
    walkable(&h, 10);
    let conn = h.shard();
    let (top, stopped_at) = {
        let ids: Vec<i64> = conn
            .prepare("select id from releases order by id desc")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        (ids[0] + 1, ids[3])
    };
    let step = TestStep::new(9, "db", 200, Needs::Database, &log)
        .runs(move |ctx| if ctx.release().id == stopped_at { Err(StepError::Stopped) } else { Ok(Outcome::default()) });
    h.start(vec![step.arc()], PostprocessConfig { workers: 1, ..quick_config() });
    wait_for("three rows", || {
        conn.query_row("select before_id from pp_walks", [], |r| r.get::<_, i64>(0)).unwrap() < top
    });
    std::thread::sleep(Duration::from_millis(300));
    let before: i64 = conn.query_row("select before_id from pp_walks", [], |r| r.get(0)).unwrap();
    assert!(before > stopped_at, "the cursor stays above the row it didnt finish: {before}, {stopped_at}");
    h.stop();
}

#[test]
fn a_walk_doesnt_run_a_release_ahead_of_its_retry() {
    let log = log();
    let mut h = fresh();
    walkable(&h, 3);
    let conn = h.shard();
    let later = chrono::Utc::now().timestamp() + 900;
    let waiting = h.id_of("Rel.0001");
    conn.execute("insert into pp_queue (release_id, due_at) values (?, ?)", rusqlite::params![waiting, later]).unwrap();
    h.start(vec![TestStep::new(9, "db", 200, Needs::Database, &log).arc()], quick_config());
    wait_for("the walk ends", || {
        conn.query_row("select count(*) from pp_walks", [], |r| r.get::<_, i64>(0)).unwrap() == 0
    });
    let names: Vec<String> = log.lock().unwrap().iter().map(|(_, n, _)| n.clone()).collect();
    assert_eq!(names, vec!["Rel.0002".to_string(), "Rel.0000".to_string()], "the one waiting has the queue");
}
