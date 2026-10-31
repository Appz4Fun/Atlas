//! Naming through the stage: what a run writes, and who it leaves alone.

mod common;

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use atlas::archive::store::{INSERT_FILE, SOURCE_PAR2, SOURCE_RAR};
use atlas::config::{Config, NamingConfig, PostprocessConfig};
use atlas::indexer::shared_db;
use atlas::naming::{NAMING, NFO};
use atlas::nntp::Pool;
use atlas::parser::{Article, Release};
use atlas::postprocess::step::{Needs, Step, StepId, mask};
use atlas::postprocess::{Stage, StageParams, reprocess, steps};
use atlas::{db, store};
use common::stage::*;
use common::{Post, Server, mock, par2_file_desc, post, spawn_server, yenc_body};
use rusqlite::params;

const TEEVEE: &str = "alt.binaries.teevee";
const NAME: &str = "Batman.The.Animated.Series.S04E01.DVDRiP.XviD-PyRo";
const SUBJECT: &str = r#"[#####]-[FULL]-[#a.b.teevee@EFNet]-[Batman.The.Animated.Series.S04E01.DVDRiP.XviD-PyRo]-[00/35] "Xk3Mz9PqL.nfo" yEnc (1/1)"#;
const SCENE: &str = "Real.Movie.2024.1080p.WEB-DL.x264-GRP";
const NAMED: i64 = mask(NAMING);

fn config() -> Config {
    Config { postprocess: PostprocessConfig { min_size_bytes: 0, ..quick_config() }, ..Config::default() }
}

fn with_bodies() -> Config {
    let mut c = config();
    c.postprocess.body_fetches_per_minute = 60_000;
    c.postprocess.body_min_size_bytes = 0;
    c.postprocess.min_completion_percent = 0;
    c
}

fn start(h: &mut Harness, config: Config, steps: Vec<Arc<dyn Step>>, port: Option<u16>) {
    let servers: Vec<_> = port.map(|p| mock(p, "secret", 4, 1)).into_iter().collect();
    let conn = db::open_at(&h.main).unwrap();
    h.stage = Some(Stage::start(StageParams {
        main: h.main.clone(),
        db: shared_db(conn),
        pool: Arc::new(Pool::new(&servers)),
        config,
        steps,
        handle: h.runtime.handle().clone(),
    }));
}

/// a release of the one file `file` in `group`, with this subject
fn release(name: &str, group: &str, file: &str, subject: &str) -> Release {
    let articles = vec![Article {
        subject: subject.into(),
        filename: Some(file.into()),
        ..article(file, 1, 1, &format!("<{name}@x>"))
    }];
    let size = articles.iter().map(|a| a.bytes).sum();
    Release { name: name.into(), group: group.into(), articles, size, complete: true, ..Default::default() }
}

fn conn_of(h: &Harness, group: &str) -> rusqlite::Connection {
    db::open_shard(&store::shard_path(&h.main, store::shard_of(group))).unwrap()
}

fn row(h: &Harness, group: &str, name: &str) -> (i64, Option<String>, Option<i64>, i64) {
    conn_of(h, group)
        .query_row("select id, display_name, name_source, is_obfuscated from releases where name = ?", [name], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })
        .unwrap()
}

fn done(h: &Harness, group: &str, id: i64) -> Option<i64> {
    conn_of(h, group).query_row("select done from pp_state where release_id = ?", [id], |r| r.get(0)).ok()
}

fn wait_done(h: &Harness, group: &str, id: i64, bit: i64) {
    wait_for("the step's bit", || done(h, group, id).is_some_and(|d| d & bit != 0));
}

#[test]
fn a_junk_release_gets_a_regex_name() {
    let mut h = fresh();
    h.save(&[Release { is_obfuscated: true, ..release("Xk3Mz9PqL", TEEVEE, "Xk3Mz9PqL.nfo", SUBJECT) }]);
    start(&mut h, config(), steps::all(), None);
    let id = row(&h, TEEVEE, "Xk3Mz9PqL").0;
    wait_done(&h, TEEVEE, id, NAMED);
    let (_, display, source, flag) = row(&h, TEEVEE, "Xk3Mz9PqL");
    assert_eq!(display.as_deref(), Some(NAME));
    assert_eq!(source, Some(5));
    assert_eq!(flag, 0);
}

/// three rows as an upgrade finds them: unnamed, and with an inline name graded `Good` and `Junk`
fn saved_old_rows(h: &Harness) -> (i64, i64, i64) {
    let of = |name: &str, display: Option<&str>| Release {
        display_name: display.map(String::from),
        ..release(name, TEEVEE, "Xk3Mz9PqL.nfo", SUBJECT)
    };
    h.save(&[
        of("Xk3Mz9PqL", None),
        of("Xk3Mz9PqA", Some("Inline Film 2020 720p")),
        of("Xk3Mz9PqB", Some("aB3dE9Xk.mkv")),
    ]);
    (row(h, TEEVEE, "Xk3Mz9PqL").0, row(h, TEEVEE, "Xk3Mz9PqA").0, row(h, TEEVEE, "Xk3Mz9PqB").0)
}

#[test]
fn the_upgrade_walk_leaves_named_rows_alone() {
    let mut h = fresh();
    let (none, good, junk) = saved_old_rows(&h);
    start(&mut h, config(), steps::all(), None);
    for id in [none, good, junk] {
        wait_done(&h, TEEVEE, id, NAMED);
    }
    assert_eq!(row(&h, TEEVEE, "Xk3Mz9PqL").1.as_deref(), Some(NAME));
    assert_eq!(row(&h, TEEVEE, "Xk3Mz9PqA").1.as_deref(), Some("Inline Film 2020 720p"));
    assert_eq!(row(&h, TEEVEE, "Xk3Mz9PqB").1.as_deref(), Some("aB3dE9Xk.mkv"));
    assert_eq!(row(&h, TEEVEE, "Xk3Mz9PqB").2, None);
}

#[test]
fn reprocess_opts_in() {
    let mut h = fresh();
    let (_, good, junk) = saved_old_rows(&h);
    start(&mut h, config(), steps::all(), None);
    wait_done(&h, TEEVEE, good, NAMED);
    wait_done(&h, TEEVEE, junk, NAMED);
    assert_eq!(row(&h, TEEVEE, "Xk3Mz9PqB").1.as_deref(), Some("aB3dE9Xk.mkv"));
    h.stop();

    let mut conn = conn_of(&h, TEEVEE);
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).unwrap();
    assert!(reprocess::apply(&tx, NAMED, None, chrono::Utc::now().timestamp()).unwrap());
    tx.commit().unwrap();
    assert_eq!(store::get_meta(&conn, "naming_rename_named").unwrap(), Some(1));

    let mut c = config();
    c.postprocess.backlog_per_second = 10_000;
    start(&mut h, c, steps::all(), None);
    wait_for("the junk name to go", || row(&h, TEEVEE, "Xk3Mz9PqB").1.as_deref() == Some(NAME));
    assert_eq!(row(&h, TEEVEE, "Xk3Mz9PqB").2, Some(5));
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(row(&h, TEEVEE, "Xk3Mz9PqA").1.as_deref(), Some("Inline Film 2020 720p"), "by improves");
}

#[test]
fn a_skipped_row_gets_no_state_row() {
    let mut h = fresh();
    h.save(&[release("DSC_0001.jpg", STAGE_GROUP, "DSC_0001.jpg", r#""DSC_0001.jpg" yEnc (1/1)"#)]);
    let id = h.id_of("DSC_0001.jpg");
    start(&mut h, config(), vec![Arc::new(atlas::naming::steps::Naming)], None);
    wait_for("its queue row to go", || {
        h.shard().query_row("select count(*) from pp_queue where release_id = ?", [id], |r| r.get::<_, i64>(0)).unwrap()
            == 0
    });
    assert_eq!(h.bits("DSC_0001.jpg"), None);
}

fn add_inner(h: &Harness, group: &str, id: i64, name: &str, source: i64) {
    conn_of(h, group).execute(INSERT_FILE, params![id, source * 65_536, name, 100, source]).unwrap();
}

#[test]
fn inner_files_rename_and_a_rerun_is_stable() {
    let log = Log::default();
    let mut h = fresh();
    h.save(&[complete("Xk3Mz9PqL")]);
    let (id, ..) = row(&h, STAGE_GROUP, "Xk3Mz9PqL");
    let recorder = TestStep::new(12, "later", 220, Needs::Database, &log).reruns(true, false);
    let mut all = steps::all();
    all.push(recorder.arc());
    start(&mut h, config(), all, None);
    wait_done(&h, STAGE_GROUP, id, mask(StepId(12)));
    assert_eq!(runs_of(&log, "later"), 1);

    add_inner(&h, STAGE_GROUP, id, &format!("{SCENE}.mkv"), SOURCE_RAR);
    let stage = h.stage.clone().unwrap();
    stage.rerun(id, NAMED).unwrap();
    wait_for("the rename", || row(&h, STAGE_GROUP, "Xk3Mz9PqL").2 == Some(15));
    assert_eq!(row(&h, STAGE_GROUP, "Xk3Mz9PqL").1.as_deref(), Some(SCENE));
    let found: i64 = h
        .shard()
        .query_row("select count(*) from releases_fts where releases_fts match '\"Real.Movie.2024\"'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(found, 1);
    wait_for("a later step to see it", || runs_of(&log, "later") == 2);

    // a scene name has nothing to gain: `applies` turns it away, and nothing changes
    stage.rerun(id, NAMED).unwrap();
    std::thread::sleep(Duration::from_millis(1_000));
    assert_eq!(row(&h, STAGE_GROUP, "Xk3Mz9PqL").1.as_deref(), Some(SCENE));
    assert_eq!(runs_of(&log, "later"), 2, "no rename, no rerun");
}

#[test]
fn new_inner_files_rerun_predb_matching_even_with_no_rename() {
    let log = Log::default();
    let mut h = fresh();
    h.save(&[release("Some.Film.2024.mkv", STAGE_GROUP, "x.bin", r#""x.bin" yEnc (1/1)"#)]);
    let (id, ..) = row(&h, STAGE_GROUP, "Some.Film.2024.mkv");
    // it counts as many parts, soo the step applies
    h.shard().execute("update releases set parts = 5 where id = ?", [id]).unwrap();
    let recorder = TestStep::new(16, "predb", 300, Needs::Database, &log);
    let mut all = steps::all();
    all.push(recorder.arc());
    start(&mut h, config(), all, None);
    wait_done(&h, STAGE_GROUP, id, mask(StepId(16)));
    assert_eq!(runs_of(&log, "predb"), 1);

    add_inner(&h, STAGE_GROUP, id, "short.bin", SOURCE_RAR);
    h.stage.clone().unwrap().rerun(id, NAMED).unwrap();
    wait_for("it to run again", || runs_of(&log, "predb") == 2);
    assert_eq!(row(&h, STAGE_GROUP, "Some.Film.2024.mkv").1, None);
}

#[test]
fn part_4s_steps_pass_the_same_rule() {
    let mut h = fresh();
    let mut r = release("x", STAGE_GROUP, "x.par2", r#""x.par2" yEnc (1/1)"#);
    r.articles[0].message_id = "<msg1@mock>".into();
    r.articles.push(article("x.mkv", 1, 1, "<msg2@mock>"));
    h.save(&[r]);
    let posts: Vec<Post> = vec![
        post(1, r#""x.par2" yEnc (1/1)"#, 10, yenc_body("x.par2", &par2_file_desc("Xk3Mz9PqL.mkv", 100))),
        post(2, r#""x.mkv" yEnc (1/1)"#, 10, vec![]),
    ];
    let state = Server::new(posts);
    let port = spawn_server(state);
    start(&mut h, with_bodies(), steps::all(), Some(port));
    let (id, ..) = row(&h, STAGE_GROUP, "x");
    wait_done(&h, STAGE_GROUP, id, mask(StepId(1)));
    let stored = atlas::archive::store::inner_files(&h.shard(), id).unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].source, SOURCE_PAR2);
    assert_eq!(row(&h, STAGE_GROUP, "x").1, None);
    assert_eq!(row(&h, STAGE_GROUP, "x").2, None);
}

#[test]
fn a_late_article_does_not_undo_a_name() {
    let mut h = fresh();
    h.save(&[release("Xk3Mz9PqL", TEEVEE, "Xk3Mz9PqL.nfo", SUBJECT)]);
    start(&mut h, config(), steps::all(), None);
    let id = row(&h, TEEVEE, "Xk3Mz9PqL").0;
    wait_done(&h, TEEVEE, id, NAMED);
    assert_eq!(row(&h, TEEVEE, "Xk3Mz9PqL").1.as_deref(), Some(NAME));
    let mut late = release("Xk3Mz9PqL", TEEVEE, "Xk3Mz9PqL.nfo", SUBJECT);
    late.articles[0].message_id = "<late@x>".into();
    late.display_name = Some("Another Name".into());
    h.save(&[late]);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(row(&h, TEEVEE, "Xk3Mz9PqL").1.as_deref(), Some(NAME));
}

#[test]
fn naming_off_means_no_step() {
    let mut h = fresh();
    h.save(&[release("Xk3Mz9PqL", TEEVEE, "Xk3Mz9PqL.nfo", SUBJECT)]);
    let mut c = config();
    c.naming = NamingConfig { enabled: false, ..Default::default() };
    start(&mut h, c, steps::all(), None);
    std::thread::sleep(Duration::from_millis(2_500));
    let (id, display, source, _) = row(&h, TEEVEE, "Xk3Mz9PqL");
    assert_eq!((display, source), (None, None));
    assert_eq!(done(&h, TEEVEE, id).unwrap_or(0) & NAMED, 0);
}

fn nfo_shape(h: &Harness) -> Arc<Server> {
    let mut r = release("x", STAGE_GROUP, "x.nfo", r#""x.nfo" yEnc (1/1)"#);
    r.articles[0].message_id = "<msg1@mock>".into();
    r.articles.push(article("x.mkv", 1, 1, "<msg2@mock>"));
    h.save(&[r]);
    Server::new(vec![
        post(1, r#""x.nfo" yEnc (1/1)"#, 10, yenc_body("x.nfo", b"Release Name: Real Name 2024 1080p\n")),
        post(2, r#""x.mkv" yEnc (1/1)"#, 10, vec![]),
    ])
}

#[test]
fn no_network_at_the_default() {
    let mut h = fresh();
    let state = nfo_shape(&h);
    let port = spawn_server(state.clone());
    start(&mut h, config(), steps::all(), Some(port));
    let (id, ..) = row(&h, STAGE_GROUP, "x");
    wait_done(&h, STAGE_GROUP, id, NAMED);
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(state.bodies_sent.load(Ordering::SeqCst), 0);
    assert_eq!(done(&h, STAGE_GROUP, id).unwrap() & mask(NFO), 0, "the step never ran");
}

#[test]
fn nfo_runs_with_naming_off() {
    let mut h = fresh();
    let state = nfo_shape(&h);
    let port = spawn_server(state.clone());
    let mut c = with_bodies();
    c.naming = NamingConfig { enabled: false, ..Default::default() };
    start(&mut h, c, steps::all(), Some(port));
    wait_for("the name", || row(&h, STAGE_GROUP, "x").2 == Some(10));
    assert_eq!(row(&h, STAGE_GROUP, "x").1.as_deref(), Some("Real Name 2024 1080p"));
    assert_eq!(state.bodies_sent.load(Ordering::SeqCst), 1);
}

#[test]
fn a_missing_nfo_is_done_not_failed() {
    let mut h = fresh();
    let mut state = nfo_shape(&h);
    Arc::get_mut(&mut state).unwrap().bodies = false;
    let port = spawn_server(state);
    start(&mut h, with_bodies(), steps::all(), Some(port));
    let (id, ..) = row(&h, STAGE_GROUP, "x");
    wait_done(&h, STAGE_GROUP, id, mask(NFO));
    let (_, failed, _) = h.bits("x").unwrap();
    assert_eq!(failed, 0);
    assert_eq!(h.field("x", "net_attempts"), 0);
    assert_eq!(row(&h, STAGE_GROUP, "x").1, None);
}

#[test]
fn an_nfo_never_beats_a_better_source() {
    use atlas::naming::steps::NfoName;
    use atlas::postprocess::step::ReleaseInfo;
    let r = ReleaseInfo {
        name: "x".into(),
        display_name: Some("Some Book Title".into()),
        name_source: Some(20),
        ..Default::default()
    };
    assert!(!NfoName.applies(&r));
    assert!(NfoName.applies(&ReleaseInfo { name: "x".into(), ..Default::default() }));
}

#[test]
fn archive_nfo_is_gone() {
    let all = steps::all();
    assert!(all.iter().all(|s| s.name() != "archive_nfo"));
    assert!(all.iter().any(|s| s.name() == "nfo"));
    assert!(all.iter().all(|s| s.id() != StepId(3)), "id 3 is never reused");
}
