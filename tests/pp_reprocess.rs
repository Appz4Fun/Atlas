//! `atlas --reprocess`.

mod common;

use std::process::{Command, Output};

use atlas::config::PostprocessConfig;
use atlas::postprocess::reprocess;
use atlas::postprocess::step::{Needs, StepId, mask};
use atlas::{db, store};
use common::stage::*;

fn run(dir: &std::path::Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_atlas"))
        .arg("--reprocess")
        .args(args)
        .env("ATLAS_HOME", dir)
        .env("ATLAS_NO_KEYRING", "1")
        .output()
        .unwrap()
}

fn fixture() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let main = dir.path().join("atlas.db");
    db::create_db_at(&main).unwrap();
    (dir, main)
}

fn walks(main: &std::path::Path) -> i64 {
    store::shard_paths(main)
        .iter()
        .map(|p| {
            db::open_shard(p).unwrap().query_row("select count(*) from pp_walks", [], |r| r.get::<_, i64>(0)).unwrap()
        })
        .sum()
}

#[test]
fn a_dry_run_prints_counts_and_changes_nothing() {
    let (dir, main) = fixture();
    let out = run(dir.path(), &[]);
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("dry run"));
    assert_eq!(walks(&main), 0);
}

#[test]
fn an_unknown_step_is_refused_with_the_list() {
    let (dir, _) = fixture();
    let out = run(dir.path(), &["--step", "nope"]);
    assert!(!out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("nope") && text.contains("all"), "{text}");
}

#[test]
fn apply_adds_one_walk_per_shard_and_no_more_on_a_second_run() {
    let (dir, main) = fixture();
    let first = run(dir.path(), &["--apply"]);
    assert!(first.status.success());
    assert_eq!(walks(&main), store::SHARDS as i64);
    let second = run(dir.path(), &["--apply"]);
    assert!(String::from_utf8_lossy(&second.stdout).contains("already queued"));
    assert_eq!(walks(&main), store::SHARDS as i64);
}

#[test]
fn the_walk_clears_the_mask_and_the_step_runs_again() {
    let log: Log = Default::default();
    let mut h = fresh();
    h.save(&[complete("Rel.One")]);
    let conn = h.shard();
    let bit = mask(StepId(9));
    let id = h.id_of("Rel.One");
    conn.execute_batch(
        "delete from pp_queue; insert or replace into meta (key, value) values ('pp_walk_seeded', 1), ('pp_steps_seen', 512);",
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
fn group_limits_it_to_that_groups_shard_and_rows() {
    let (dir, main) = fixture();
    let out = run(dir.path(), &["--apply", "--group", STAGE_GROUP]);
    assert!(out.status.success());
    assert_eq!(walks(&main), 1);
    let conn = db::open_shard(&store::shard_path(&main, store::shard_of(STAGE_GROUP))).unwrap();
    let group: String = conn.query_row("select group_name from pp_walks", [], |r| r.get(0)).unwrap();
    assert_eq!(group, STAGE_GROUP);
}

fn two_failed_groups(main: &std::path::Path) -> (rusqlite::Connection, i64, i64, String) {
    let other = (0..)
        .map(|i| format!("alt.binaries.other{i}"))
        .find(|g| store::shard_of(g) == store::shard_of(STAGE_GROUP))
        .unwrap();
    let mut b = release_of("Other.One", "o.bin", 2, 2);
    b.group = other.clone();
    store::save(main, &[release_of("Mine.One", "m.bin", 2, 2), b]).unwrap();
    let conn = db::open_shard(&store::shard_path(main, store::shard_of(STAGE_GROUP))).unwrap();
    conn.execute_batch(
        "delete from pp_queue; insert into pp_state (release_id, failed, net_attempts, last_error) select id, 2, 1, 'x' from releases;",
    )
    .unwrap();
    let id = |name: &str| conn.query_row("select id from releases where name = ?", [name], |r| r.get(0)).unwrap();
    let (mine, others) = (id("Mine.One"), id("Other.One"));
    (conn, mine, others, other)
}

#[test]
fn failed_clears_failed_bits_and_retries_now() {
    let (dir, main) = fixture();
    let (conn, mine, others, _) = two_failed_groups(&main);
    assert!(run(dir.path(), &["--apply", "--failed"]).status.success());
    for id in [mine, others] {
        let (failed, attempts): (i64, i64) = conn
            .query_row("select failed, net_attempts from pp_state where release_id = ?", [id], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!((failed, attempts), (0, 0));
    }
    assert_eq!(conn.query_row("select count(*) from pp_queue", [], |r| r.get::<_, i64>(0)).unwrap(), 2);
}

#[test]
fn failed_with_a_group_leaves_other_groups_failures_alone() {
    let (dir, main) = fixture();
    let (conn, mine, others, _) = two_failed_groups(&main);
    assert!(run(dir.path(), &["--apply", "--failed", "--group", STAGE_GROUP]).status.success());
    let failed = |id: i64| -> (i64, i64) {
        conn.query_row("select failed, net_attempts from pp_state where release_id = ?", [id], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap()
    };
    assert_eq!(failed(mine), (0, 0));
    assert_eq!(failed(others), (2, 1));
    let queued: Vec<i64> = conn
        .prepare("select release_id from pp_queue")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert_eq!(queued, vec![mine]);
}

#[test]
fn it_runs_beside_a_writer() {
    let (dir, main) = fixture();
    let guard = atlas::compact::hold_off_compaction(&main).unwrap();
    assert!(run(dir.path(), &["--apply"]).status.success());
    drop(guard);
}

#[test]
fn it_is_refused_during_a_compaction() {
    let (dir, main) = fixture();
    let lock = std::fs::File::create(atlas::compact::lock_path(&main)).unwrap();
    lock.lock().unwrap();
    assert!(!run(dir.path(), &["--apply"]).status.success());
    assert_eq!(walks(&main), 0);
    drop(lock);
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
        "insert or replace into meta (key, value) values ('pp_walk_seeded', 1), ('pp_steps_seen', 512);",
    )
    .unwrap();
    // done, not growing, and touched just now: the walk finds it before it settles
    conn.execute("insert into pp_state (release_id, done, parts_seen) values (?, ?, 2)", rusqlite::params![id, bit])
        .unwrap();
    assert!(reprocess::apply(&conn, bit, None, 1).unwrap());
    let step = TestStep::new(9, "db", 200, Needs::Database, &log);
    h.start(vec![step.arc()], PostprocessConfig { settle_secs: 2, complete_settle_secs: 2, ..Default::default() });
    wait_for("the step runs again", || runs_of(&log, "db") == 1);
    assert_eq!(h.bits("Rel.One").unwrap().0, bit);
}

#[test]
fn the_dry_run_counts_failed_releases() {
    let (dir, main) = fixture();
    two_failed_groups(&main);
    let text = String::from_utf8_lossy(&run(dir.path(), &["--failed"]).stdout).into_owned();
    assert!(text.contains("2 releases"), "{text}");
    let text = String::from_utf8_lossy(&run(dir.path(), &["--failed", "--group", STAGE_GROUP]).stdout).into_owned();
    assert!(text.contains("1 releases"), "{text}");
}
