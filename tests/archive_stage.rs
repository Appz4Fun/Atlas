//! Archive inspection through the stage, on the mock server. Every body count is exact.

mod common;

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use atlas::archive::Password;
use atlas::archive::steps::next_probe_slot;
use atlas::archive::store::{InnerFileRow, inner_files, password};
use atlas::archive::testkit::*;
use atlas::config::{ArchiveConfig, Config, NamingConfig, PostprocessConfig};
use atlas::indexer::shared_db;
use atlas::nntp::Pool;
use atlas::parser::{Article, Release};
use atlas::postprocess::step::{Needs, Step, StepId, mask};
use atlas::postprocess::{Stage, StageParams, steps};
use atlas::{db, store};
use common::stage::*;
use common::{Post, Server, mock, par2_file_desc, post, spawn_server, yenc_body, yenc_lined};

const ARTICLE: i64 = 700_000;
const PAR2_DONE: i64 = mask(StepId(1));
const LIST_DONE: i64 = mask(StepId(2));
const NFO_DONE: i64 = mask(StepId(9));

/// A release and what the server holds for it, numbered in the order they're added.
#[derive(Default)]
struct Shape {
    articles: Vec<Article>,
    posts: Vec<Post>,
    name: String,
    obfuscated: bool,
    display_name: Option<String>,
}

fn shape(name: &str) -> Shape {
    Shape { name: name.into(), ..Default::default() }
}

impl Shape {
    fn obfuscated(mut self) -> Self {
        self.obfuscated = true;
        self
    }

    fn named(mut self, display_name: &str) -> Self {
        self.display_name = Some(display_name.into());
        self
    }

    fn sized(mut self, file: &str, part: i64, parts: i64, bytes: i64, body: Vec<Vec<u8>>) -> Self {
        let number = self.posts.len() as u64 + 1;
        let id = format!("<msg{number}@mock>");
        self.articles.push(Article { bytes, ..article(file, part, parts, &id) });
        self.posts.push(post(number, &format!("\"{file}\" yEnc ({part}/{parts})"), bytes as u64, body));
        self
    }

    fn add(self, file: &str, part: i64, parts: i64, body: Vec<Vec<u8>>) -> Self {
        self.sized(file, part, parts, ARTICLE, body)
    }

    /// one article, whose body the test means to read
    fn one(self, file: &str, data: &[u8]) -> Self {
        self.add(file, 1, 1, yenc_body(file, data))
    }

    /// an article nothing should fetch: as an empty body it would list nothing
    fn dark(self, file: &str, part: i64, parts: i64) -> Self {
        self.add(file, part, parts, vec![])
    }

    fn release(&self) -> Release {
        Release {
            name: self.name.clone(),
            group: STAGE_GROUP.into(),
            articles: self.articles.clone(),
            size: self.articles.iter().map(|a| a.bytes).sum(),
            is_obfuscated: self.obfuscated,
            display_name: self.display_name.clone(),
            complete: false,
            ..Default::default()
        }
    }
}

fn config() -> Config {
    Config {
        postprocess: PostprocessConfig {
            body_min_size_bytes: 0,
            body_fetches_per_minute: 60_000,
            // posts stand for their sets here: a 900 part file with one article is enough
            min_completion_percent: 0,
            ..quick_config()
        },
        // the fixtures' names are for these steps to judge, see tests/naming_stage.rs
        naming: NamingConfig { enabled: false, ..Default::default() },
        ..Config::default()
    }
}

fn start_with(h: &mut Harness, port: u16, config: Config, steps: Vec<Arc<dyn Step>>) {
    let pool = Arc::new(Pool::new(&[mock(port, "secret", 4, 1)]));
    let conn = db::open_at(&h.main).unwrap();
    h.stage = Some(Stage::start(StageParams {
        main: h.main.clone(),
        db: shared_db(conn),
        pool,
        config,
        steps,
        handle: h.runtime.handle().clone(),
    }));
}

/// the release saved and the stage started on it, with the server holding its posts
fn serve(h: &mut Harness, shape: &Shape, config: Config) -> Arc<Server> {
    h.save(&[shape.release()]);
    let state = Server::new(shape.posts.clone());
    let port = spawn_server(state.clone());
    start_with(h, port, config, steps::all());
    state
}

/// through the stage: its queue row gone, and nothing it waits on the network for
fn settled(h: &Harness, name: &str) {
    let id = h.id_of(name);
    wait_for("the release to settle", || {
        let conn = h.shard();
        let queued: i64 =
            conn.query_row("select count(*) from pp_queue where release_id = ?", [id], |r| r.get(0)).unwrap();
        let waiting = h.bits(name).is_some_and(|(_, _, net)| net == Some(1));
        queued == 0 && !waiting
    });
}

fn bodies(state: &Server) -> usize {
    state.bodies_sent.load(Ordering::SeqCst)
}

fn rows(h: &Harness, name: &str) -> Vec<InnerFileRow> {
    inner_files(&h.shard(), h.id_of(name)).unwrap()
}

fn names(h: &Harness, name: &str) -> Vec<String> {
    rows(h, name).into_iter().map(|r| r.name).collect()
}

fn pass(h: &Harness, name: &str) -> Option<Password> {
    password(&h.shard(), h.id_of(name)).unwrap()
}

fn naming(h: &Harness, name: &str) -> (Option<String>, Option<i64>) {
    h.shard()
        .query_row("select display_name, name_source from releases where name = ?", [name], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap()
}

fn a_rar5(name: &str) -> Vec<u8> {
    rar5(&[r5_main(), r5_file(2, 4, name.as_bytes(), &[], b"hello"), r5_end()])
}

fn a_rar4(name: &str) -> Vec<u8> {
    rar4(&[r4_main(0), r4_file(0, name.as_bytes(), b"hello"), r4_end()])
}

/// a 7z of `payload` zeros, cut after its start header
fn a_7z(payload: usize) -> (Vec<u8>, Vec<u8>) {
    let whole = sz_encoded(&vec![0u8; payload], &["Inner.Movie.2024.mkv", "Inner.Movie.2024.nfo"]);
    let (head, tail) = whole.split_at(40);
    (head.to_vec(), tail.to_vec())
}

fn par2_of(names: &[&str]) -> Vec<u8> {
    names.iter().flat_map(|n| par2_file_desc(n, 1000)).collect()
}

#[test]
fn a_named_rar_set_reads_only_the_first_article_of_the_first_volume() {
    let mut h = fresh();
    let s = shape("x")
        .dark("x.part03.rar", 1, 3)
        .dark("x.part03.rar", 2, 3)
        .dark("x.part03.rar", 3, 3)
        .add("x.part01.rar", 1, 3, yenc_body("x.part01.rar", &a_rar5("Inner.mkv")))
        .dark("x.part01.rar", 2, 3)
        .dark("x.part01.rar", 3, 3)
        .dark("x.part02.rar", 1, 3)
        .dark("x.part02.rar", 2, 3)
        .dark("x.part02.rar", 3, 3);
    let state = serve(&mut h, &s, config());
    settled(&h, "x");
    assert_eq!(bodies(&state), 1);
    let found = rows(&h, "x");
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].source, 2);
    assert_eq!(pass(&h, "x"), Some(Password::No));
}

#[test]
fn rar4_records_its_source() {
    let mut h = fresh();
    let s = shape("x").one("x.rar", &a_rar4("Inner.mkv")).dark("x.r00", 1, 2);
    let state = serve(&mut h, &s, config());
    settled(&h, "x");
    assert_eq!(bodies(&state), 1);
    assert_eq!(rows(&h, "x")[0].source, 2);
}

#[test]
fn a_zip_reads_its_head_and_last_article() {
    let mut h = fresh();
    let whole = zip_of(&[("Inner.nfo", b"note"), ("Inner.mkv", b"hello world")]);
    let (head, tail) = whole.split_at(40);
    let s = shape("x").add("x.zip", 1, 2, yenc_body("x.zip", head)).add("x.zip", 2, 2, yenc_body("x.zip", tail));
    let state = serve(&mut h, &s, config());
    settled(&h, "x");
    assert_eq!(bodies(&state), 2);
    let found = rows(&h, "x");
    assert_eq!(found.len(), 2, "{found:?}");
    assert!(found.iter().all(|r| r.source == 4));
}

#[test]
fn a_one_article_zip_is_one_body() {
    let mut h = fresh();
    let whole = zip_of(&[("Inner.nfo", b"note"), ("Inner.mkv", b"hello world")]);
    let s = shape("x").one("x.zip", &whole);
    let state = serve(&mut h, &s, config());
    settled(&h, "x");
    assert_eq!(bodies(&state), 1);
    assert_eq!(rows(&h, "x").len(), 2);
}

#[test]
fn an_encoded_7z_reads_its_head_and_tail() {
    let mut h = fresh();
    let (head, tail) = a_7z(1_000);
    let s = shape("x").add("x.7z", 1, 2, yenc_body("x.7z", &head)).add("x.7z", 2, 2, yenc_body("x.7z", &tail));
    let state = serve(&mut h, &s, config());
    settled(&h, "x");
    assert_eq!(bodies(&state), 2);
    let found = rows(&h, "x");
    assert_eq!(found.len(), 2, "{found:?}");
    assert!(found.iter().all(|r| r.source == 3));
}

#[test]
fn encrypted_headers_are_passworded_not_failed() {
    let packed = vec![0xaa; 64];
    let sz = sz_wrap(&packed, &sz_encrypted_header(0, packed.len() as u64));
    let rar4_locked = [rar4(&[r4_main(0x0080)]), vec![0xaa; 64]].concat();
    let rar5_locked = [rar5(&[r5_crypt_header()]), vec![0xaa; 64]].concat();
    for (file, body) in [("x.rar", rar5_locked), ("x.rar", rar4_locked), ("x.7z", sz)] {
        let mut h = fresh();
        let s = shape("x").one(file, &body);
        serve(&mut h, &s, config());
        settled(&h, "x");
        assert_eq!(pass(&h, "x"), Some(Password::Yes), "{file}");
        assert!(rows(&h, "x").is_empty());
        let (done, failed, _) = h.bits("x").unwrap();
        assert_ne!(done & LIST_DONE, 0);
        assert_eq!(failed, 0);
        assert_eq!(h.field("x", "net_attempts"), 0);
    }
}

#[test]
fn a_missing_first_article_moves_to_the_next_rar_volume() {
    let mut h = fresh();
    let s = shape("x")
        .dark("x.part01.rar", 2, 3)
        .dark("x.part01.rar", 3, 3)
        .add("x.part02.rar", 1, 3, yenc_body("x.part02.rar", &a_rar5("Inner.mkv")))
        .dark("x.part02.rar", 2, 3)
        .dark("x.part02.rar", 3, 3);
    let state = serve(&mut h, &s, config());
    settled(&h, "x");
    assert_eq!(bodies(&state), 1);
    assert_eq!(rows(&h, "x").len(), 1);
}

#[test]
fn a_7z_header_split_across_two_articles_takes_a_second_tail() {
    let mut h = fresh();
    let (head, tail) = a_7z(1_000);
    let (second, last) = tail.split_at(tail.len() - 20);
    let s = shape("x").add("x.7z", 1, 3, yenc_body("x.7z", &head)).add("x.7z", 2, 3, yenc_body("x.7z", second)).add(
        "x.7z",
        3,
        3,
        yenc_body("x.7z", last),
    );
    let state = serve(&mut h, &s, config());
    settled(&h, "x");
    assert_eq!(bodies(&state), 3);
    assert_eq!(rows(&h, "x").len(), 2);
}

#[test]
fn a_7z_whose_last_article_isnt_indexed_is_maybe() {
    let mut h = fresh();
    let (head, tail) = a_7z(1_000);
    let s = shape("x").add("x.7z", 1, 3, yenc_body("x.7z", &head)).add("x.7z", 2, 3, yenc_body("x.7z", &tail));
    let state = serve(&mut h, &s, config());
    settled(&h, "x");
    assert_eq!(bodies(&state), 1);
    assert_eq!(pass(&h, "x"), Some(Password::Maybe));
    assert!(rows(&h, "x").is_empty());
}

#[test]
fn a_random_named_set_is_probed_by_magic() {
    let mut h = fresh();
    let big = rar5(&[r5_main(), r5_file(2, 4, b"Real.Movie.2024.mkv", &[], &vec![7u8; 1 << 20]), r5_end()]);
    let s = shape("a1B2c3D4e5F6g7H8i9J0").obfuscated().add("a1B2c3D4e5F6g7H8i9J0", 1, 900, yenc_lined("x", &big)).dark(
        "k9L8m7N6o5P4q3R2s1T0",
        1,
        500,
    );
    let state = serve(&mut h, &s, config());
    settled(&h, &s.name);
    assert_eq!(bodies(&state), 1);
    let found = rows(&h, &s.name);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].source, 2);
    assert_eq!(found[0].name, "Real.Movie.2024.mkv");
    assert_eq!(naming(&h, &s.name), (Some("Real.Movie.2024.mkv".into()), Some(15)));
    assert!(state.accepted.load(Ordering::SeqCst) >= 1);
}

#[test]
fn probing_stops_at_its_cap() {
    let mut h = fresh();
    let mut s = shape("a1B2c3D4e5F6g7H8i9J0");
    for i in 0..5 {
        s = s.add(&format!("r{i}a8Z3q9W7"), 1, 900 - i, yenc_lined("x", b"no magic here"));
    }
    let state = serve(&mut h, &s, config());
    settled(&h, &s.name);
    assert_eq!(bodies(&state), 2);
    assert!(rows(&h, &s.name).is_empty());
    assert_ne!(h.bits(&s.name).unwrap().0 & LIST_DONE, 0);
}

#[test]
fn probe_slots_are_spaced() {
    let now = Instant::now();
    let gap = Duration::from_secs(1);
    let mut next = None;
    assert_eq!(next_probe_slot(&mut next, now, gap), now);
    assert_eq!(next, Some(now + gap));
    assert_eq!(next_probe_slot(&mut next, now, gap), now + gap);
    let later = now + Duration::from_secs(60);
    assert_eq!(next_probe_slot(&mut next, later, gap), later);
}

#[test]
fn a_7z_found_by_a_probe_gets_its_own_tail() {
    let mut h = fresh();
    let (head, tail) = a_7z(1_000);
    let s = shape("a1B2c3D4e5F6g7H8i9J0")
        .add("a1B2c3D4e5F6g7H8i9J0", 1, 3, yenc_lined("x", &head))
        .dark("a1B2c3D4e5F6g7H8i9J0", 2, 3)
        .add("a1B2c3D4e5F6g7H8i9J0", 3, 3, yenc_body("x", &tail));
    let state = serve(&mut h, &s, config());
    settled(&h, &s.name);
    assert_eq!(bodies(&state), 2);
    assert_eq!(rows(&h, &s.name).len(), 2);
}

#[test]
fn a_probed_7z_longer_than_its_file_doesnt_fetch_a_tail() {
    let mut h = fresh();
    let (head, _) = a_7z(100_000);
    let s = shape("a1B2c3D4e5F6g7H8i9J0").sized("a1B2c3D4e5F6g7H8i9J0", 1, 3, 100, yenc_lined("x", &head)).sized(
        "a1B2c3D4e5F6g7H8i9J0",
        3,
        3,
        100,
        vec![],
    );
    let state = serve(&mut h, &s, config());
    settled(&h, &s.name);
    assert_eq!(bodies(&state), 1);
    assert_eq!(pass(&h, &s.name), Some(Password::Maybe));
}

#[test]
fn an_image_found_by_a_probe_stops_the_release() {
    let mut h = fresh();
    let jpeg = [&[0xff, 0xd8, 0xff, 0xe0][..], &vec![1u8; 1 << 20]].concat();
    let s = shape("a1B2c3D4e5F6g7H8i9J0").add("a1B2c3D4e5F6g7H8i9J0", 1, 900, yenc_lined("x", &jpeg)).add(
        "k9L8m7N6o5P4q3R2s1T0",
        1,
        500,
        yenc_lined("x", &a_rar5("Inner.mkv")),
    );
    let state = serve(&mut h, &s, config());
    settled(&h, &s.name);
    assert_eq!(bodies(&state), 1);
    assert!(rows(&h, &s.name).is_empty());
}

#[test]
fn random_named_single_articles_are_never_probed() {
    for parts in [1, 2] {
        let mut h = fresh();
        let s =
            shape("a1B2c3D4e5F6g7H8i9J0").add("a1B2c3D4e5F6g7H8i9J0", 1, parts, yenc_lined("x", &a_rar5("Inner.mkv")));
        let state = serve(&mut h, &s, config());
        settled(&h, &s.name);
        assert_eq!(bodies(&state), 0, "{parts}");
        assert!(rows(&h, &s.name).is_empty());
    }
}

#[test]
fn image_named_files_are_never_fetched() {
    for mkv in [true, false] {
        let mut h = fresh();
        let mut s = shape("x").add("x.jpg", 1, 3, yenc_lined("x", &a_rar5("Inner.mkv")));
        if mkv {
            s = s.dark("x.mkv", 1, 3);
        }
        let state = serve(&mut h, &s, config());
        settled(&h, "x");
        assert_eq!(bodies(&state), 0);
    }
}

#[test]
fn a_release_named_as_an_image_runs_no_step() {
    let mut h = fresh();
    let s = shape("IMG_0001.jpg").add("a1B2c3D4e5F6g7H8i9J0", 1, 500, yenc_lined("x", &a_rar5("Inner.mkv"))).dark(
        "a1B2c3D4e5F6g7H8i9J0",
        2,
        500,
    );
    let state = serve(&mut h, &s, config());
    settled(&h, &s.name);
    assert_eq!(bodies(&state), 0);
    // no step applies, soo it may have no row at all
    let (done, net) = h.bits(&s.name).map_or((0, None), |(done, _, net)| (done, net));
    assert_eq!(net, None);
    assert_eq!(done & (PAR2_DONE | LIST_DONE | NFO_DONE), 0);
}

#[test]
fn a_release_under_the_body_minimum_gets_no_body() {
    let mut h = fresh();
    let mut under = config();
    under.postprocess.body_min_size_bytes = PostprocessConfig::default().body_min_size_bytes;
    let s = shape("x").one("x.rar", &a_rar5("Inner.mkv"));
    let state = serve(&mut h, &s, under);
    settled(&h, "x");
    assert_eq!(bodies(&state), 0);
    assert!(rows(&h, "x").is_empty());
}

#[test]
fn a_named_rar_that_is_really_7z_goes_by_its_bytes() {
    let mut h = fresh();
    let (head, tail) = a_7z(10);
    let s = shape("x").one("x.rar", &[head, tail].concat());
    serve(&mut h, &s, config());
    settled(&h, "x");
    assert_eq!(rows(&h, "x")[0].source, 3);
}

#[test]
fn a_named_rar_with_junk_records_nothing() {
    let mut h = fresh();
    let s = shape("x").one("x.rar", b"nothing a parser knows");
    serve(&mut h, &s, config());
    settled(&h, "x");
    assert!(rows(&h, "x").is_empty());
    assert_eq!(pass(&h, "x"), None);
}

#[test]
fn a_par2_set_is_listed_and_named() {
    let mut h = fresh();
    let body =
        par2_of(&["Real.Show.part01.rar", "Real.Show.part02.rar", "Real.Show.part03.rar", "Real.Show.S01E01.mkv"]);
    let s = shape("x").one("x.par2", &body).dark("x.mkv", 1, 3);
    let state = serve(&mut h, &s, config());
    settled(&h, "x");
    assert_eq!(bodies(&state), 1);
    let found = rows(&h, "x");
    assert_eq!(found.len(), 2, "volumes collapse: {found:?}");
    assert!(found.iter().all(|r| r.source == 1));
    let (display, source) = naming(&h, "x");
    assert!(display.is_some());
    assert_eq!(source, Some(20));
    assert_eq!(pass(&h, "x"), None, "a PAR2 says nothing about passwords");
}

#[test]
fn a_set_with_only_volumes_reads_first_and_last() {
    let mut h = fresh();
    let s = shape("x")
        .add("x.vol000+01.par2", 1, 3, yenc_body("x", &[9u8; 64]))
        .dark("x.vol000+01.par2", 2, 3)
        .add("x.vol000+01.par2", 3, 3, yenc_body("x", &par2_of(&["Real.Show.S01E01.mkv"])))
        .dark("x.mkv", 1, 3);
    let state = serve(&mut h, &s, config());
    settled(&h, "x");
    assert_eq!(bodies(&state), 2);
    assert_eq!(names(&h, "x"), ["Real.Show.S01E01.mkv"]);
}

#[test]
fn an_existing_name_is_left_alone() {
    let mut h = fresh();
    let s = shape("x").named("Keep.Me").one("x.par2", &par2_of(&["Real.Show.S01E01.mkv"])).dark("x.mkv", 1, 3);
    serve(&mut h, &s, config());
    settled(&h, "x");
    assert_eq!(rows(&h, "x").len(), 1);
    assert_eq!(naming(&h, "x"), (Some("Keep.Me".into()), None));
}

#[test]
fn inner_names_only_replace_junk() {
    let mut h = fresh();
    let s = shape("Good.Release.Name.2024").one("x.rar", &a_rar5("Inner.Movie.2024.mkv"));
    serve(&mut h, &s, config());
    settled(&h, &s.name);
    assert_eq!(rows(&h, &s.name).len(), 1);
    assert_eq!(naming(&h, &s.name), (None, None));

    let mut h = fresh();
    let s = shape("Xk3Mz9PqL").obfuscated().one("x.rar", &a_rar5("Inner.Movie.2024.mkv"));
    serve(&mut h, &s, config());
    settled(&h, &s.name);
    assert_eq!(naming(&h, &s.name), (Some("Inner.Movie.2024.mkv".into()), Some(15)));
}

#[test]
fn an_nfo_names_a_release_nothing_else_named() {
    let mut h = fresh();
    let s = shape("x").one("x.nfo", b"Release Name: Real Name 2024\n").dark("x.mkv", 1, 3);
    let state = serve(&mut h, &s, config());
    settled(&h, "x");
    assert_eq!(bodies(&state), 1);
    assert_eq!(naming(&h, "x"), (Some("Real Name 2024".into()), Some(10)));
}

#[test]
fn an_nfo_is_skipped_once_named() {
    let mut h = fresh();
    let s = shape("x")
        .one("x.par2", &par2_of(&["Real.Show.S01E01.mkv"]))
        .one("x.nfo", b"Release Name: Real Name 2024\n")
        .dark("x.mkv", 1, 3);
    let state = serve(&mut h, &s, config());
    settled(&h, "x");
    assert_eq!(bodies(&state), 1);
    assert_eq!(naming(&h, "x").1, Some(20));
}

fn runs_with_naming(body: &[u8]) -> usize {
    let log = Log::default();
    let mut h = fresh();
    let s = shape("x").one("x.rar", body);
    h.save(&[s.release()]);
    let state = Server::new(s.posts.clone());
    let port = spawn_server(state);
    let naming = TestStep::new(8, "naming", 200, Needs::Database, &log).arc();
    let mut steps = steps::all();
    steps.push(naming);
    start_with(&mut h, port, config(), steps);
    settled(&h, "x");
    wait_for("the naming step", || runs_of(&log, "naming") >= 1);
    std::thread::sleep(Duration::from_millis(300));
    settled(&h, "x");
    runs_of(&log, "naming")
}

#[test]
fn inner_files_rerun_naming() {
    assert_eq!(runs_with_naming(&a_rar5("Inner.mkv")), 2);
    assert_eq!(runs_with_naming(b"nothing a parser knows"), 1);
}

#[test]
fn a_late_article_lists_again() {
    let mut h = fresh();
    let s = shape("x").dark("x.rar", 2, 3).dark("x.rar", 3, 3);
    let mut state = Server::new(
        s.posts
            .iter()
            .cloned()
            .chain([Post {
                message_id: "<late@mock>".into(),
                ..post(3, "\"x.rar\" yEnc (1/3)", 10, yenc_body("x.rar", &a_rar5("Inner.mkv")))
            }])
            .collect(),
    );
    Arc::get_mut(&mut state).unwrap();
    h.save(&[s.release()]);
    let port = spawn_server(state);
    start_with(&mut h, port, config(), steps::all());
    settled(&h, "x");
    assert!(rows(&h, "x").is_empty());
    let late = Release { articles: vec![article("x.rar", 1, 3, "<late@mock>")], ..s.release() };
    h.save(&[late]);
    wait_for("the rows", || !rows(&h, "x").is_empty());
}

#[test]
fn a_rerun_replaces_rows_without_duplicates() {
    let mut h = fresh();
    let s = shape("x").one("x.rar", &a_rar5("Inner.mkv"));
    let state = serve(&mut h, &s, config());
    settled(&h, "x");
    let before = rows(&h, "x");
    assert_eq!(before.len(), 1);
    let id = h.id_of("x");
    h.stage.as_ref().unwrap().rerun(id, LIST_DONE).unwrap();
    wait_for("a second read", || bodies(&state) == 2);
    settled(&h, "x");
    assert_eq!(rows(&h, "x"), before);
}

#[test]
fn an_outage_costs_no_attempt() {
    let mut h = fresh();
    let s = shape("x").one("x.rar", &a_rar5("Inner.mkv"));
    h.save(&[s.release()]);
    let dead = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    start_with(&mut h, dead, config(), steps::all());
    // a run counts an attempt ahead of itself, and the outage gives it back with the pause: read them together
    wait_for("it moved on with no attempt used", || {
        h.bits("x").is_some()
            && h.field("x", "net_next_at") > chrono::Utc::now().timestamp()
            && h.field("x", "net_attempts") == 0
    });
    assert_eq!(h.bits("x").unwrap().0 & LIST_DONE, 0, "it isnt done");
    assert!(rows(&h, "x").is_empty());
}

#[test]
fn limits_follow_the_config() {
    let mut h = fresh();
    let mut s = shape("a1B2c3D4e5F6g7H8i9J0");
    for i in 0..3 {
        s = s.add(&format!("r{i}a8Z3q9W7"), 1, 900 - i, yenc_lined("x", b"no magic here"));
    }
    let mut limited = config();
    limited.archive = ArchiveConfig { max_bodies_per_release: 1, ..Default::default() };
    let state = serve(&mut h, &s, limited);
    settled(&h, &s.name);
    assert_eq!(bodies(&state), 1);
}

#[test]
fn no_budget_no_bodies() {
    let mut h = fresh();
    let s = shape("x").one("x.rar", &a_rar5("Inner.mkv"));
    let mut default = config();
    default.postprocess.body_fetches_per_minute = 0;
    let state = serve(&mut h, &s, default);
    settled(&h, "x");
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(bodies(&state), 0);
    assert!(rows(&h, "x").is_empty());
}

fn long_names() -> Vec<u8> {
    let mut blocks = vec![r5_main()];
    for i in 0..64 {
        blocks.push(r5_file(2, 4, format!("{i:03}{}.mkv", "n".repeat(505)).as_bytes(), &[], b"x"));
    }
    blocks.push(r5_end());
    rar5(&blocks)
}

fn meta(h: &Harness, key: &str) -> Option<i64> {
    store::get_meta(&h.shard(), key).unwrap()
}

#[test]
fn past_the_quota_no_names_are_stored() {
    let mut h = fresh();
    h.shard().execute("insert into meta (key, value) values ('archive_name_bytes', 99000000)", []).unwrap();
    let mut small = config();
    small.archive.max_names_mb = 64;
    let s = shape("x").one("x.rar", &long_names());
    serve(&mut h, &s, small);
    settled(&h, "x");
    assert!(rows(&h, "x").is_empty());
    assert_eq!(pass(&h, "x"), Some(Password::No));
    assert_eq!(meta(&h, "archive_names_over_quota"), Some(1));
    assert_eq!(meta(&h, "archive_name_bytes"), Some(99_000_000));
}

#[test]
fn under_the_quota_the_total_grows_by_what_was_stored() {
    let mut h = fresh();
    let s = shape("x").one("x.rar", &a_rar5("Inner.mkv"));
    serve(&mut h, &s, config());
    settled(&h, "x");
    assert_eq!(meta(&h, "archive_name_bytes"), Some("Inner.mkv".len() as i64 + 32));
    assert_eq!(meta(&h, "archive_names_over_quota"), None);
}

#[test]
fn a_rerun_doesnt_spend_the_quota() {
    let mut h = fresh();
    let s = shape("x").one("x.rar", &a_rar5("Inner.mkv"));
    let state = serve(&mut h, &s, config());
    settled(&h, "x");
    let before = meta(&h, "archive_name_bytes");
    assert!(before.is_some());
    let id = h.id_of("x");
    h.stage.as_ref().unwrap().rerun(id, LIST_DONE).unwrap();
    wait_for("a second read", || bodies(&state) == 2);
    settled(&h, "x");
    assert_eq!(meta(&h, "archive_name_bytes"), before);
}

#[test]
fn a_rerun_past_the_quota_keeps_what_was_stored() {
    let mut h = fresh();
    let mut small = config();
    small.archive.max_names_mb = 64;
    let s = shape("x").one("x.rar", &a_rar5("Inner.mkv"));
    let state = serve(&mut h, &s, small);
    settled(&h, "x");
    let before = rows(&h, "x");
    assert_eq!(before.len(), 1);
    h.shard().execute("update meta set value = 99000000 where key = 'archive_name_bytes'", []).unwrap();
    let id = h.id_of("x");
    h.stage.as_ref().unwrap().rerun(id, LIST_DONE).unwrap();
    wait_for("a second read", || bodies(&state) == 2);
    settled(&h, "x");
    assert_eq!(rows(&h, "x"), before);
    assert_eq!(meta(&h, "archive_names_over_quota"), Some(1));
}
