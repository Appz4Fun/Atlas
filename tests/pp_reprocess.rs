//! Current walk/worker behavior remains pending its real native Queue/StepRunner port.
//! Retired one-off reprocess command fixtures are catalogued in the retirement ledger.

mod common;

use atlas::config::PostprocessConfig;
use atlas::postprocess::reprocess;
use atlas::postprocess::step::{Needs, StepId, mask};
use common::stage::*;

#[test]
fn the_walk_clears_the_mask_and_the_step_runs_again() {
    let log: Log = Default::default();
    let mut h = fresh();
    h.save(&[complete("Rel.One")]);
    let conn = h.shard();
    let bit = mask(StepId(9));
    let id = h.id_of("Rel.One");
    conn.execute_batch(
        "delete from pp_queue; insert or replace into meta (key, value) values ('pp_walk_seeded', 1), ('pp_junk_walk', 1), ('pp_junk_queued', 1), ('pp_steps_seen', 512);",
    )
    .unwrap();
    conn.execute("insert into pp_state (release_id, done, parts_seen) values (?, ?, 2)", rusqlite::params![id, bit])
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1200));
    assert!(reprocess::apply(&conn, bit, None, 1).unwrap());
    let step = TestStep::new(9, "db", 200, Needs::Database, &log);
    h.start(vec![step.arc()], quick_config());
    wait_for("it runs again", || runs_of(&log, "db") == 1);
    wait_for("the walk ends", || {
        conn.query_row("select count(*) from pp_walks", [], |r| r.get::<_, i64>(0)).unwrap() == 0
    });
    assert_eq!(h.bits("Rel.One").unwrap().0, bit);
}

#[test]
fn a_walks_request_waits_for_a_release_still_settling() {
    let log: Log = Default::default();
    let mut h = fresh();
    h.save(&[complete("Rel.One")]);
    let conn = h.shard();
    let bit = mask(StepId(9));
    let id = h.id_of("Rel.One");
    conn.execute_batch(
        "insert or replace into meta (key, value) values ('pp_walk_seeded', 1), ('pp_junk_walk', 1), ('pp_junk_queued', 1), ('pp_steps_seen', 512);",
    )
    .unwrap();
    // done, not growing, and touched just now: the walk finds it before it settles
    conn.execute("insert into pp_state (release_id, done, parts_seen) values (?, ?, 2)", rusqlite::params![id, bit])
        .unwrap();
    assert!(reprocess::apply(&conn, bit, None, 1).unwrap());
    let step = TestStep::new(9, "db", 200, Needs::Database, &log);
    h.start(vec![step.arc()], PostprocessConfig { settle_secs: 2, complete_settle_secs: 2, ..Default::default() });
    wait_for("the step runs again", || runs_of(&log, "db") == 1);
    // a run is logged before its result is written
    wait_for("its bit", || h.bits("Rel.One").is_some_and(|b| b.0 == bit));
}
