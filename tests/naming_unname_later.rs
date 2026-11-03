//! `atlas --unname` brings later steps back. Its own binary: it calls the command in this process, with a
//! step the binary doesn't know, and points ATLAS_HOME at a temp dir before any thread is spawned.

mod common;

use atlas::naming::unname;
use atlas::postprocess::step::{Needs, StepId, mask};
use atlas::{db, store};
use common::stage::*;
use rusqlite::params;

#[test]
fn later_steps_are_queued() {
    let home = tempfile::tempdir().unwrap();
    // SAFETY: only test in this binary, set before any thread is spawned
    unsafe {
        std::env::set_var("ATLAS_HOME", home.path());
        std::env::set_var("ATLAS_NO_KEYRING", "1");
    }
    db::create_db().unwrap();
    let main = home.path().join("atlas.db");
    store::save(&main, &[complete("Xk3Mz9PqL"), complete("Xk3Mz9PqA")]).unwrap();
    let conn = db::open_shard(&store::shard_path(&main, store::shard_of(STAGE_GROUP))).unwrap();
    let id_of =
        |name: &str| -> i64 { conn.query_row("select id from releases where name = ?", [name], |r| r.get(0)).unwrap() };
    let (done_one, failed_one) = (id_of("Xk3Mz9PqL"), id_of("Xk3Mz9PqA"));
    conn.execute("delete from pp_queue", []).unwrap();
    conn.execute("update releases set display_name = 'Some.Show.S01E01', name_source = 5, is_obfuscated = 0", [])
        .unwrap();
    let (naming, later) = (mask(atlas::naming::NAMING), mask(StepId(20)));
    conn.execute(
        "insert into pp_state (release_id, done, failed, db_attempts) values (?, ?, 0, 0), (?, ?, ?, 3)",
        params![done_one, naming | later, failed_one, naming, later],
    )
    .unwrap();

    assert_eq!(unname::run_with(&["--unname".to_string(), "--apply".to_string()], later), 0);

    let state = |id: i64| -> (i64, i64, i64) {
        conn.query_row("select done, failed, db_attempts from pp_state where release_id = ?", [id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .unwrap()
    };
    assert_eq!(state(done_one), (naming, 0, 0));
    assert_eq!(state(failed_one), (naming, 0, 0));
    let queued: i64 = conn.query_row("select count(*) from pp_queue", [], |r| r.get(0)).unwrap();
    assert_eq!(queued, 2);

    // and they run on the restored name, with naming leaving it unnamed
    let log = Log::default();
    let mut h = Harness {
        dir: home,
        main,
        runtime: tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap(),
        stage: None,
    };
    let step = TestStep::new(20, "later", 400, Needs::Database, &log).reruns(true, false);
    let naming_step: std::sync::Arc<dyn atlas::postprocess::step::Step> =
        std::sync::Arc::new(atlas::naming::steps::Naming);
    h.start(vec![naming_step, step.arc()], quick_config());
    wait_for("both to run", || runs_of(&log, "later") == 2);
    let names: Vec<Option<String>> = {
        let mut stmt = conn.prepare("select display_name from releases").unwrap();
        stmt.query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect()
    };
    assert!(names.iter().all(Option::is_none), "{names:?}");
    assert!(log.lock().unwrap().iter().all(|(_, _, display)| display.is_none()));
}
