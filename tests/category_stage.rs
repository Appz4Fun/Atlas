//! The category step, through the stage.

mod common;

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use atlas::config::{CategoryConfig, Config, PostprocessConfig};
use atlas::indexer::shared_db;
use atlas::nntp::Pool;
use atlas::parser::Release;
use atlas::postprocess::step::{NameChange, Needs, Outcome, Step, StepId, mask};
use atlas::postprocess::{Stage, StageParams, steps};
use atlas::{db, store};
use common::stage::*;
use common::{Server, mock, post, spawn_server, yenc_body};

const BIT: i64 = mask(StepId(24));

fn config() -> Config {
    Config { postprocess: PostprocessConfig { min_size_bytes: 0, ..quick_config() }, ..Config::default() }
}

fn step() -> Arc<dyn Step> {
    Arc::new(atlas::category::CategoryStep::new())
}

fn start(h: &mut Harness, config: Config, steps: Vec<Arc<dyn Step>>, pool: Pool) {
    let conn = db::open_at(&h.main).unwrap();
    h.stage = Some(Stage::start(StageParams {
        main: h.main.clone(),
        db: shared_db(conn),
        pool: Arc::new(pool),
        config,
        steps,
        handle: h.runtime.handle().clone(),
    }));
}

fn in_group(name: &str, file: &str, group: &str) -> Release {
    Release { group: group.into(), ..release_of(name, file, 2, 2) }
}

fn shard_of(h: &Harness, group: &str) -> rusqlite::Connection {
    db::open_shard(&store::shard_path(&h.main, store::shard_of(group))).unwrap()
}

fn id_in(h: &Harness, group: &str, name: &str) -> i64 {
    shard_of(h, group).query_row("select id from releases where name = ?", [name], |r| r.get(0)).unwrap()
}

fn category(h: &Harness, group: &str, id: i64) -> Option<i64> {
    shard_of(h, group).query_row("select category from release_category where release_id = ?", [id], |r| r.get(0)).ok()
}

fn done(h: &Harness, group: &str, id: i64) -> i64 {
    shard_of(h, group).query_row("select done from pp_state where release_id = ?", [id], |r| r.get(0)).unwrap_or(0)
}

const MOVIES: &str = "alt.binaries.movies";
const MISC: &str = "alt.binaries.misc";

#[test]
fn a_settled_release_gets_its_category() {
    let mut h = fresh();
    let name = "Some.Movie.2019.1080p.BluRay.x264-GRP";
    h.save(&[in_group(name, "x.bin", MOVIES)]);
    let id = id_in(&h, MOVIES, name);
    start(&mut h, config(), vec![step()], Pool::new(&[]));
    wait_for("its row", || category(&h, MOVIES, id) == Some(2040));
    wait_for("its bit", || done(&h, MOVIES, id) & BIT != 0);
}

/// a step at rank 250 that renames the release on its second run
fn renamer(log: &Log, to: &'static str) -> Arc<dyn Step> {
    let runs = Mutex::new(0);
    TestStep::new(12, "renamer", 250, Needs::Database, log)
        .reruns(false, true)
        .runs(move |_| {
            let mut n = runs.lock().unwrap();
            *n += 1;
            if *n < 2 {
                return Ok(Outcome::default());
            }
            let name = NameChange { display_name: Some(to.into()), obfuscated: Some(false), source: 5 };
            Ok(Outcome { name: Some(name), ..Outcome::default() })
        })
        .arc()
}

#[test]
fn a_rename_reruns_the_step() {
    let log = Log::default();
    let mut h = fresh();
    let name = "d41d8cd98f00b204e9800998ecf8427e";
    h.save(&[in_group(name, "x.bin", MISC)]);
    let id = id_in(&h, MISC, name);
    start(&mut h, config(), vec![renamer(&log, "Some.Show.S01E02.720p.HDTV.x264-GRP"), step()], Pool::new(&[]));
    wait_for("its bit", || done(&h, MISC, id) & BIT != 0);
    assert_eq!(category(&h, MISC, id), None, "a hash shape");
    h.stage.clone().unwrap().rerun(id, mask(StepId(12))).unwrap();
    wait_for("its row", || category(&h, MISC, id) == Some(5040));
}

#[test]
fn a_rename_to_nothing_recognizable_deletes_the_row() {
    let log = Log::default();
    let mut h = fresh();
    let name = "Some.Show.S01E02.720p.HDTV.x264-GRP";
    h.save(&[in_group(name, "x.bin", MISC)]);
    let id = id_in(&h, MISC, name);
    start(&mut h, config(), vec![renamer(&log, "Plain Name Nothing Else"), step()], Pool::new(&[]));
    wait_for("its row", || category(&h, MISC, id) == Some(5040));
    h.stage.clone().unwrap().rerun(id, mask(StepId(12))).unwrap();
    wait_for("the row to go", || category(&h, MISC, id).is_none());
}

#[test]
fn a_no_match_or_hidden_release_writes_no_row() {
    let mut h = fresh();
    let (plain, hash) = ("Plain Name Nothing Else", "d41d8cd98f00b204e9800998ecf8427e");
    h.save(&[in_group(plain, "x.bin", MISC), in_group(hash, "y.bin", MISC)]);
    let ids = [id_in(&h, MISC, plain), id_in(&h, MISC, hash)];
    start(&mut h, config(), vec![step()], Pool::new(&[]));
    for id in ids {
        wait_for("its bit", || done(&h, MISC, id) & BIT != 0);
        assert_eq!(category(&h, MISC, id), None);
    }
}

#[test]
fn a_small_release_and_an_image_post_reach_the_step() {
    let mut h = fresh();
    let book = "Some.Book.Title.epub";
    let cover = "cover.jpg";
    let mut small = in_group(book, book, "alt.binaries.e-book");
    small.size = 10_000;
    small.articles.iter_mut().for_each(|a| a.bytes = 5_000);
    let mut image = in_group(cover, cover, "alt.binaries.e-book");
    image.size = 700_000;
    h.save(&[small, image]);
    let (a, b) = (id_in(&h, "alt.binaries.e-book", book), id_in(&h, "alt.binaries.e-book", cover));
    start(&mut h, config(), vec![step()], Pool::new(&[]));
    wait_for("the book's row", || category(&h, "alt.binaries.e-book", a) == Some(7020));
    wait_for("the image's bit", || done(&h, "alt.binaries.e-book", b) & BIT != 0);
    assert_eq!(category(&h, "alt.binaries.e-book", b), None);
}

#[test]
fn a_name_from_a_late_save_reruns_it() {
    let mut h = fresh();
    h.save(&[in_group("Plain Name Nothing Else", "x.bin", MISC)]);
    let id = id_in(&h, MISC, "Plain Name Nothing Else");
    start(&mut h, config(), vec![step()], Pool::new(&[]));
    wait_for("its bit", || done(&h, MISC, id) & BIT != 0);
    assert_eq!(category(&h, MISC, id), None);
    let mut late = in_group("Plain Name Nothing Else", "x.bin", MISC);
    late.articles.iter_mut().for_each(|a| a.message_id.push('2'));
    late.articles.truncate(1);
    late.articles[0] = article("z.bin", 1, 1, "<late@x>");
    late.display_name = Some("Some.Show.S01E02.720p.HDTV.x264-GRP".into());
    h.save(&[late]);
    wait_for("the row to follow", || category(&h, MISC, id) == Some(5040));
}

#[test]
fn the_fact_reaches_a_later_step() {
    let log = Log::default();
    let seen: Arc<Mutex<Option<String>>> = Default::default();
    let s = seen.clone();
    let mut h = fresh();
    let name = "Some.Movie.2019.1080p.BluRay.x264-GRP";
    h.save(&[in_group(name, "x.bin", MOVIES)]);
    let recorder = TestStep::new(25, "recorder", 450, Needs::Database, &log)
        .runs(move |ctx| {
            *s.lock().unwrap() = ctx.fact("category").map(String::from);
            Ok(Outcome::default())
        })
        .arc();
    start(&mut h, config(), vec![step(), recorder], Pool::new(&[]));
    wait_for("the fact", || seen.lock().unwrap().as_deref() == Some("2040"));
}

#[test]
fn a_rejected_release_gets_no_row() {
    let mut h = fresh();
    let name = "Only.Par2.Files.2024-GRP";
    h.save(&[in_group(name, "x.par2", MOVIES)]);
    let id = id_in(&h, MOVIES, name);
    start(&mut h, config(), vec![step()], Pool::new(&[]));
    wait_for("its queue row to go", || {
        shard_of(&h, MOVIES)
            .query_row("select count(*) from pp_queue where release_id = ?", [id], |r| r.get::<_, i64>(0))
            .unwrap()
            == 0
    });
    assert_eq!(category(&h, MOVIES, id), None);
    assert_eq!(done(&h, MOVIES, id), 0, "no pp_state row");
}

#[test]
fn the_step_does_no_network() {
    let mut h = fresh();
    let name = "Some.Movie.2019.1080p.BluRay.x264-GRP";
    let release = in_group(name, "x.nfo", MOVIES);
    let id = release.articles[0].message_id.clone();
    h.save(&[release]);
    let row = id_in(&h, MOVIES, name);
    let body =
        common::Post { message_id: id, ..post(1, r#""x.nfo" yEnc (1/1)"#, 10, yenc_body("x.nfo", b"Title: A Name")) };
    let state = Server::new(vec![body]);
    let port = spawn_server(state.clone());
    start(&mut h, config(), steps::all(), Pool::new(&[mock(port, "secret", 2, 1)]));
    wait_for("its row", || category(&h, MOVIES, row).is_some());
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(state.bodies_sent.load(Ordering::SeqCst), 0);
}

#[test]
fn category_off_writes_nothing() {
    let mut h = fresh();
    let name = "Some.Movie.2019.1080p.BluRay.x264-GRP";
    h.save(&[in_group(name, "x.bin", MOVIES)]);
    let id = id_in(&h, MOVIES, name);
    let log = Log::default();
    let other = TestStep::new(12, "other", 250, Needs::Database, &log).arc();
    let off = Config { category: CategoryConfig { enabled: false, ..Default::default() }, ..config() };
    start(&mut h, off, vec![step(), other], Pool::new(&[]));
    wait_for("the other step", || runs_of(&log, "other") == 1);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(category(&h, MOVIES, id), None);
}
