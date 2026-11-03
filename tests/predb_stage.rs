//! The match step, through the stage.

mod common;

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use atlas::archive::store::{INSERT_FILE, SOURCE_PAR2, SOURCE_RAR};
use atlas::config::{CategoryConfig, Config, MediaConfig, PostprocessConfig, PredbConfig};
use atlas::indexer::shared_db;
use atlas::naming::{NAMING, NameSource};
use atlas::nntp::Pool;
use atlas::parser::Release;
use atlas::postprocess::step::{Needs, ReleaseInfo, Step, mask};
use atlas::postprocess::{Stage, StageParams, steps};
use atlas::predb::db::{create, path_for};
use atlas::predb::import::{Entry, import};
use atlas::predb::keys::md5_hex;
use atlas::predb::late::tick;
use atlas::predb::step::{PREDB_MATCH, worth_looking_up};
use atlas::{db, store};
use common::stage::*;
use rusqlite::{Connection, params};

const T: &str = "Alpha.Beta.Gamma.2024.1080p.WEB-DL.H.264-TESTGRP";
const MATCHED: i64 = mask(PREDB_MATCH);

fn config() -> Config {
    Config {
        postprocess: PostprocessConfig { min_size_bytes: 0, ..quick_config() },
        predb: PredbConfig { enabled: true, ..Default::default() },
        category: CategoryConfig { enabled: false, ..Default::default() },
        media: MediaConfig { parse: false, ..Default::default() },
        ..Config::default()
    }
}

fn titles(h: &Harness, list: &[&str]) {
    let mut conn = create(&path_for(&h.main)).unwrap();
    let entries: Vec<Entry> = list
        .iter()
        .map(|t| Entry { title: t.to_string(), category: Some("TV-HD".into()), ..Default::default() })
        .collect();
    import(&mut conn, 1, &entries, 5, None).unwrap();
}

fn start(h: &mut Harness, config: Config, steps: Vec<Arc<dyn Step>>) {
    let conn = db::open_at(&h.main).unwrap();
    h.stage = Some(Stage::start(StageParams {
        main: h.main.clone(),
        db: shared_db(conn),
        pool: Arc::new(Pool::new(&[])),
        config,
        steps,
        handle: h.runtime.handle().clone(),
    }));
}

fn row(h: &Harness, name: &str) -> (i64, Option<String>, Option<i64>, i64) {
    h.shard()
        .query_row("select id, display_name, name_source, is_obfuscated from releases where name = ?", [name], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })
        .unwrap()
}

fn matched_title(h: &Harness, id: i64) -> Option<i64> {
    h.shard().query_row("select title_id from predb_match where release_id = ?", [id], |r| r.get(0)).ok()
}

fn wait_bit(h: &Harness, name: &str, bit: i64) {
    wait_for("the step's bit", || h.bits(name).is_some_and(|(done, ..)| done & bit != 0));
}

fn with_later(id: u8, log: &Log) -> Vec<Arc<dyn Step>> {
    let mut all = steps::all();
    all.push(TestStep::new(id, "later", 400, Needs::Database, log).reruns(true, false).arc());
    all
}

#[test]
fn worth_looking_up_table() {
    let of = |name: &str, parts: i64| ReleaseInfo { name: name.into(), parts, ..Default::default() };
    let hash = md5_hex(T);
    for r in [
        ReleaseInfo { name_source: Some(100), ..of(T, 5) },
        of("DSC_0001.jpg", 1),
        of("DSC_0001.jpg", 7),
        of("Xk3Mz9PqL", 1),
    ] {
        assert!(!worth_looking_up(&r), "{r:?}");
    }
    for r in [of("Xk3Mz9PqL", 9), of(&hash, 1), of(T, 1)] {
        assert!(worth_looking_up(&r), "{r:?}");
    }
}

#[test]
fn a_picture_costs_no_lookup() {
    let mut h = fresh();
    titles(&h, &[T]);
    h.save(&[release_of("DSC_0001.jpg", "DSC_0001.jpg", 1, 1)]);
    let id = h.id_of("DSC_0001.jpg");
    start(&mut h, config(), vec![Arc::new(atlas::predb::step::PredbMatch)]);
    wait_for("its queue row to go", || {
        h.shard().query_row("select count(*) from pp_queue where release_id = ?", [id], |r| r.get::<_, i64>(0)).unwrap()
            == 0
    });
    assert_eq!(h.bits("DSC_0001.jpg"), None);
}

#[test]
fn a_match_renames_with_the_top_source() {
    let mut h = fresh();
    titles(&h, &[T]);
    let file = format!("{T}.mkv");
    h.save(&[Release { is_obfuscated: true, ..release_of(&file, &file, 2, 2) }]);
    start(&mut h, config(), steps::all());
    wait_bit(&h, &file, MATCHED);
    let (id, display, source, flag) = row(&h, &file);
    assert_eq!((display.as_deref(), source, flag), (Some(T), Some(100), 0));
    assert!(matched_title(&h, id).is_some());
    let found: i64 = h
        .shard()
        .query_row("select count(*) from releases_fts where releases_fts match ?", [format!("\"{T}\"")], |r| r.get(0))
        .unwrap();
    assert_eq!(found, 1);
}

#[test]
fn the_title_beats_a_good_name() {
    let mut h = fresh();
    titles(&h, &[T]);
    h.save(&[release_of("Xk3Mz9PqL", "x.bin", 2, 2)]);
    let (id, ..) = row(&h, "Xk3Mz9PqL");
    let conn = h.shard();
    conn.execute(
        "update releases set display_name = 'Good.Name.2024-GRP', name_source = ? where id = ?",
        params![NameSource::Par2 as i64, id],
    )
    .unwrap();
    conn.execute(INSERT_FILE, params![id, SOURCE_RAR * 65_536, format!("{T}.mkv"), 100, SOURCE_RAR]).unwrap();
    start(&mut h, config(), steps::all());
    wait_bit(&h, "Xk3Mz9PqL", MATCHED);
    assert_eq!(row(&h, "Xk3Mz9PqL").1.as_deref(), Some(T));
    assert_eq!(row(&h, "Xk3Mz9PqL").2, Some(100));
}

#[test]
fn a_rerun_is_stable() {
    let log = Log::default();
    let mut h = fresh();
    titles(&h, &[T]);
    h.save(&[release_of(T, "x.bin", 2, 2)]);
    start(&mut h, config(), with_later(20, &log));
    let id = row(&h, T).0;
    wait_bit(&h, T, MATCHED);
    wait_for("the later step", || runs_of(&log, "later") >= 1);
    std::thread::sleep(Duration::from_millis(400));
    let runs = runs_of(&log, "later");
    // once matched, a release has a title: a rerun has nothing to look up and renames nothing
    h.stage.clone().unwrap().rerun(id, MATCHED).unwrap();
    std::thread::sleep(Duration::from_millis(800));
    assert_eq!(row(&h, T).1.as_deref(), Some(T));
    assert_eq!(runs_of(&log, "later"), runs);
}

#[test]
fn a_naming_rename_reruns_it() {
    const NAME: &str = "Batman.The.Animated.Series.S04E01.DVDRiP.XviD-PyRo";
    const SUBJECT: &str = r#"[#####]-[FULL]-[#a.b.teevee@EFNet]-[Batman.The.Animated.Series.S04E01.DVDRiP.XviD-PyRo]-[00/35] "Xk3Mz9PqL.nfo" yEnc (1/1)"#;
    let mut h = fresh();
    titles(&h, &[NAME]);
    let group = "alt.binaries.teevee";
    let mut r = Release { group: group.into(), ..release_of("Xk3Mz9PqL", "Xk3Mz9PqL.nfo", 1, 1) };
    r.articles[0].subject = SUBJECT.into();
    h.save(&[r]);
    start(&mut h, config(), steps::all());
    let conn = || db::open_shard(&store::shard_path(&h.main, store::shard_of(group))).unwrap();
    wait_for("the title", || {
        conn()
            .query_row("select name_source from releases where name = 'Xk3Mz9PqL'", [], |r| r.get::<_, Option<i64>>(0))
            .unwrap()
            == Some(100)
    });
    let display: String =
        conn().query_row("select display_name from releases where name = 'Xk3Mz9PqL'", [], |r| r.get(0)).unwrap();
    assert_eq!(display, NAME);
}

#[test]
fn new_inner_files_are_seen() {
    let mut h = fresh();
    titles(&h, &[T]);
    h.save(&[complete("Xk3Mz9PqL")]);
    let id = row(&h, "Xk3Mz9PqL").0;
    start(&mut h, config(), steps::all());
    wait_bit(&h, "Xk3Mz9PqL", MATCHED);
    assert_eq!(row(&h, "Xk3Mz9PqL").2, None);
    h.shard().execute(INSERT_FILE, params![id, SOURCE_PAR2 * 65_536, format!("{T}.mkv"), 100, SOURCE_PAR2]).unwrap();
    h.stage.clone().unwrap().rerun(id, mask(NAMING)).unwrap();
    wait_for("the title", || row(&h, "Xk3Mz9PqL").2 == Some(100));
    assert_eq!(row(&h, "Xk3Mz9PqL").1.as_deref(), Some(T));
}

fn inner_keys(h: &Harness, id: i64) -> i64 {
    h.shard().query_row("select count(*) from predb_inner_key where release_id = ?", [id], |r| r.get(0)).unwrap()
}

#[test]
fn an_unmatched_release_records_its_inner_keys() {
    let mut h = fresh();
    titles(&h, &["Some.Other.Title.2024.WEB.x264-GRP"]);
    h.save(&[complete("Xk3Mz9PqL")]);
    let id = row(&h, "Xk3Mz9PqL").0;
    let conn = h.shard();
    for (i, name) in ["One.Good.Name.2024-GRP.mkv", "Two.Good.Name.2024-GRP.mkv", "Xk3Mz9PqA.mkv"].iter().enumerate() {
        conn.execute(INSERT_FILE, params![id, SOURCE_RAR * 65_536 + i as i64, name, 100, SOURCE_RAR]).unwrap();
    }
    let mut c = config();
    c.naming.enabled = false;
    start(&mut h, c, steps::all());
    wait_bit(&h, "Xk3Mz9PqL", MATCHED);
    wait_for("its keys", || inner_keys(&h, id) == 2);

    // a later match clears them
    conn.execute(
        INSERT_FILE,
        params![id, SOURCE_RAR * 65_536 + 9, "Some.Other.Title.2024.WEB.x264-GRP.mkv", 100, SOURCE_RAR],
    )
    .unwrap();
    h.stage.clone().unwrap().rerun(id, MATCHED).unwrap();
    wait_for("the match", || row(&h, "Xk3Mz9PqL").2 == Some(100));
    wait_for("its keys to go", || inner_keys(&h, id) == 0);
}

#[test]
fn a_season_pack_stays() {
    let mut h = fresh();
    let episode = "Show.Name.S02E03.1080p.WEB-DL.x264-GRP";
    titles(&h, &[episode]);
    let pack = "Show.Name.S02.1080p.WEB-DL.x264-GRP";
    h.save(&[release_of(pack, "x.bin", 2, 2)]);
    let id = row(&h, pack).0;
    h.shard()
        .execute(INSERT_FILE, params![id, SOURCE_RAR * 65_536, format!("{episode}.mkv"), 100, SOURCE_RAR])
        .unwrap();
    start(&mut h, config(), steps::all());
    wait_bit(&h, pack, MATCHED);
    assert_eq!(row(&h, pack).1, None);
    assert_eq!(row(&h, pack).2, None);
}

#[test]
fn facts_reach_later_steps() {
    let log = Log::default();
    let seen: Arc<std::sync::Mutex<Vec<String>>> = Default::default();
    let s = seen.clone();
    let mut h = fresh();
    titles(&h, &[T]);
    h.save(&[release_of(T, "x.bin", 2, 2)]);
    let mut all = steps::all();
    all.push(
        TestStep::new(20, "later", 400, Needs::Database, &log)
            .runs(move |ctx| {
                s.lock().unwrap().extend(ctx.fact("predb.category").map(String::from));
                Ok(Default::default())
            })
            .arc(),
    );
    start(&mut h, config(), all);
    wait_for("the fact", || seen.lock().unwrap().iter().any(|c| c == "TV-HD"));
}

#[test]
fn the_first_match_of_a_title_reruns_later_parts() {
    let log = Log::default();
    let mut h = fresh();
    titles(&h, &[T]);
    let file = format!("{T}.mkv");
    h.save(&[release_of(&file, &file, 2, 2)]);
    let later = TestStep::new(25, "later", 410, Needs::Database, &log);
    let mut all = steps::all();
    all.push(later.arc());
    start(&mut h, config(), all);
    wait_bit(&h, &file, MATCHED);
    wait_for("the later step", || runs_of(&log, "later") >= 1);
    let id = row(&h, &file).0;
    assert!(matched_title(&h, id).is_some());
    std::thread::sleep(Duration::from_millis(400));
    let runs = runs_of(&log, "later");
    // a release with its title gets no second lookup: the row stays, and nothing runs again
    h.stage.clone().unwrap().rerun(id, MATCHED).unwrap();
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(runs_of(&log, "later"), runs);
}

#[test]
fn off_means_no_step_and_no_file() {
    let mut h = fresh();
    h.save(&[complete("Xk3Mz9PqL")]);
    let mut c = config();
    c.predb.enabled = false;
    start(&mut h, c, steps::all());
    std::thread::sleep(Duration::from_millis(2_500));
    assert_eq!(h.bits("Xk3Mz9PqL").map_or(0, |b| b.0) & MATCHED, 0);
    assert!(!path_for(&h.main).exists());
}

#[test]
fn a_missing_file_retries() {
    let mut h = fresh();
    h.save(&[release_of(T, "x.bin", 2, 2)]);
    start(&mut h, config(), steps::all());
    let id = row(&h, T).0;
    wait_for("an attempt", || h.bits(T).is_some() && h.field(T, "db_attempts") >= 1);
    let error: Option<String> =
        h.shard().query_row("select last_error from pp_state where release_id = ?", [id], |r| r.get(0)).unwrap();
    assert!(error.is_some());

    titles(&h, &[T]);
    h.stage.clone().unwrap().rerun(id, MATCHED).unwrap();
    wait_for("the title", || row(&h, T).2 == Some(100));
}

#[test]
fn a_late_title_waits_for_an_uncommitted_read() {
    let h = fresh();
    titles(&h, &["First.Known.Title.2024.WEB.x264-GRP"]);
    let mut conn = create(&path_for(&h.main)).unwrap();
    let read: i64 = conn.query_row("select max(id) from titles", [], |r| r.get(0)).unwrap();

    // a read that hasn't committed is held by the stage: this stands for it, with what a real run registers
    let guard = atlas::predb::step::hold_for_test(&path_for(&h.main), &conn, read);
    let new = Entry { title: T.into(), ..Default::default() };
    import(&mut conn, 1, &[new], 5, None).unwrap();
    let shards: Vec<Connection> = store::shard_paths(&h.main).iter().map(|p| db::open_shard(p).unwrap()).collect();
    let stop = AtomicBool::new(false);
    let late = |c: &Connection| -> i64 {
        c.query_row("select value from meta where key = 'late_after_id'", [], |r| r.get(0)).unwrap()
    };
    tick(&conn, &shards, 10_000, 10, &stop, &|_| Ok(())).unwrap();
    assert_eq!(late(&conn), read, "the cursor stays below it");

    drop(guard);
    tick(&conn, &shards, 10_000, 10, &stop, &|_| Ok(())).unwrap();
    assert_eq!(late(&conn), read + 1);
}

#[test]
fn a_deleted_release_goes_from_both_tables() {
    let h = fresh();
    h.save(&[complete("Rel.One")]);
    let conn = h.shard();
    let id = h.id_of("Rel.One");
    conn.execute("insert into predb_inner_key (key, release_id) values (7, ?)", [id]).unwrap();
    conn.execute("insert into predb_match (release_id, title_id, kind, matched_at) values (?, 1, 1, 5)", [id]).unwrap();
    let plan: String = conn
        .query_row("explain query plan delete from predb_inner_key where release_id = 1", [], |r| r.get(3))
        .unwrap();
    assert!(plan.starts_with("SEARCH"), "{plan}");
    conn.execute("delete from releases where id = ?", [id]).unwrap();
    let left: i64 = conn
        .query_row("select (select count(*) from predb_inner_key) + (select count(*) from predb_match)", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(left, 0);
}
