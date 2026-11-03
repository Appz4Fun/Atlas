//! A movie's IMDb id from its NFO, within the body budget.

mod common;

use std::sync::Arc;
use std::sync::atomic::Ordering;

use atlas::category::CategoryStep;
use atlas::config::{Config, MediaConfig, PostprocessConfig};
use atlas::db;
use atlas::indexer::shared_db;
use atlas::media::steps::{MediaLookup, MediaNfo, MediaParse, NFO_READ, imdb_in_nfo};
use atlas::media::title::{Kind, normalize, title_key};
use atlas::media::{Media, query, store as media_store};
use atlas::nntp::Pool;
use atlas::parser::Release;
use atlas::postprocess::step::{Step, StepId, mask};
use atlas::postprocess::{Stage, StageParams};
use common::http::*;
use common::stage::*;
use common::{Post, Server, mock, spawn_server, yenc_body};

const NFO: i64 = mask(StepId(34));
const PARSE: i64 = mask(StepId(32));
const FILM: &str = "Some.Movie.2019.1080p.BluRay.x264-GRP";
const STATED: &[u8] = b"http://www.imdb.com/title/tt0200002/\n";

fn with_nfo(name: &str) -> Release {
    let mut r = release_of(name, "movie.mkv", 2, 2);
    r.articles.push(article("some.nfo", 1, 1, &format!("<{name}.nfo@x>")));
    r
}

fn posts_of(releases: &[Release], nfo: &[u8]) -> Vec<Post> {
    let mut posts = Vec::new();
    for r in releases {
        for a in &r.articles {
            let body = if a.message_id.ends_with(".nfo@x>") { nfo } else { b"payload".as_slice() };
            posts.push(Post {
                number: posts.len() as u64 + 1,
                subject: a.subject.clone(),
                message_id: a.message_id.clone(),
                bytes: 700_000,
                body: yenc_body("x", body),
                date: "Fri, 02 Oct 2026 10:11:12 +0000".into(),
                author: "poster <p@x>".into(),
            });
        }
    }
    posts
}

fn budget(per_minute: i64) -> PostprocessConfig {
    PostprocessConfig {
        min_size_bytes: 0,
        body_min_size_bytes: 0,
        body_fetches_per_minute: per_minute,
        ..quick_config()
    }
}

fn steps_of(media: &Arc<Media>) -> Vec<Arc<dyn Step>> {
    vec![
        Arc::new(CategoryStep::new()),
        Arc::new(MediaParse(media.clone())),
        Arc::new(MediaLookup(media.clone())),
        Arc::new(MediaNfo(media.clone())),
    ]
}

fn run_with(h: &mut Harness, state: &Arc<Server>, postprocess: PostprocessConfig, cfg: MediaConfig) -> Arc<Media> {
    let port = spawn_server(state.clone());
    let media = Media::new(&h.main);
    h.stage = Some(Stage::start(StageParams {
        main: h.main.clone(),
        db: shared_db(db::open_at(&h.main).unwrap()),
        pool: Arc::new(Pool::new(&[mock(port, "secret", 4, 1)])),
        config: Config { postprocess, media: cfg, ..Config::default() },
        steps: steps_of(&media),
        handle: h.runtime.handle().clone(),
    }));
    media
}

fn bodies(state: &Server) -> usize {
    state.bodies_sent.load(Ordering::Relaxed)
}

fn imdb(h: &Harness, name: &str) -> Option<i64> {
    h.shard()
        .query_row("select imdb_id from media_release where release_id = ?", [h.id_of(name)], |r| r.get(0))
        .unwrap_or(None)
}

fn done(h: &Harness, name: &str) -> i64 {
    h.bits(name).map_or(0, |b| b.0)
}

fn settle() {
    std::thread::sleep(std::time::Duration::from_millis(1_200));
}

#[test]
fn no_budget_no_body() {
    let mut h = fresh();
    let r = [with_nfo(FILM)];
    h.save(&r);
    let state = Server::new(posts_of(&r, STATED));
    run_with(&mut h, &state, budget(0), MediaConfig::default());
    wait_for("the parse", || done(&h, FILM) & PARSE != 0);
    settle();
    assert_eq!(bodies(&state), 0);
    assert_eq!(done(&h, FILM) & NFO, 0);
    assert_eq!(h.bits(FILM).unwrap().2, None, "net_wanted");
}

#[test]
fn an_nfo_gives_the_imdb_id() {
    let mut h = fresh();
    let r = [with_nfo(FILM)];
    h.save(&r);
    let state = Server::new(posts_of(&r, STATED));
    run_with(&mut h, &state, budget(600), MediaConfig::default());
    wait_for("the id", || imdb(&h, FILM) == Some(200002));
    assert_eq!(bodies(&state), 1);
    let from_nfo: i64 = h.shard().query_row("select imdb_from_nfo from media_release", [], |r| r.get(0)).unwrap();
    assert_eq!(from_nfo, 1);
}

#[test]
fn tv_never_fetches() {
    let name = "Some.Show.S01E02.720p.HDTV.x264-GRP";
    let mut h = fresh();
    let r = [with_nfo(name)];
    h.save(&r);
    let state = Server::new(posts_of(&r, STATED));
    run_with(&mut h, &state, budget(600), MediaConfig::default());
    wait_for("the parse", || done(&h, name) & PARSE != 0);
    settle();
    assert_eq!(bodies(&state), 0);
}

#[test]
fn no_nfo_no_body() {
    let mut h = fresh();
    let r = [release_of(FILM, "movie.mkv", 2, 2)];
    h.save(&r);
    let state = Server::new(posts_of(&r, STATED));
    run_with(&mut h, &state, budget(600), MediaConfig::default());
    wait_for("the step", || done(&h, FILM) & NFO != 0);
    assert_eq!(bodies(&state), 0);
}

#[test]
fn a_set_imdb_id_spends_no_body() {
    let mut h = fresh();
    let r = [with_nfo(FILM)];
    h.save(&r);
    let id = h.id_of(FILM);
    let key = title_key(Kind::Movie, &normalize("Some Movie"), Some(2019));
    h.shard()
        .execute(
            "insert into media_release (release_id, kind, title_key, quality, imdb_id) values (?, 2, ?, 0, 9)",
            rusqlite::params![id, key],
        )
        .unwrap();
    let state = Server::new(posts_of(&r, STATED));
    run_with(&mut h, &state, budget(600), MediaConfig::default());
    wait_for("the step", || done(&h, FILM) & NFO != 0);
    assert_eq!(bodies(&state), 0);
    assert_eq!(imdb(&h, FILM), Some(9));
}

#[test]
fn a_known_title_spends_no_body() {
    let mut h = fresh();
    let r = [with_nfo(FILM)];
    h.save(&r);
    let cache = media_store::open(&media_store::path_for(&h.main)).unwrap();
    let key = title_key(Kind::Movie, &normalize("Some Movie"), Some(2019));
    let mut t = media_store::TitleRow {
        key,
        kind: 2,
        title: "x".into(),
        state: 1,
        checked_mask: 2,
        fanout_done: true,
        ..Default::default()
    };
    t.ids.imdb = Some(200009);
    media_store::put(&cache, &t).unwrap();
    let s = FixtureServer::start(vec![]);
    let mut cfg = MediaConfig::default();
    cfg.tmdb.api_key = "K".into();
    cfg.tmdb.base_url = Some(s.base.clone());
    let state = Server::new(posts_of(&r, STATED));
    run_with(&mut h, &state, budget(600), cfg);
    wait_for("the ID from the cache", || imdb(&h, FILM) == Some(200009));
    settle();
    assert_eq!(bodies(&state), 0);
    assert!(s.requests().is_empty(), "{:?}", s.requests());
}

#[test]
fn a_paused_provider_doesnt_hold_up_the_nfo() {
    let mut h = fresh();
    let r = [with_nfo(FILM)];
    h.save(&r);
    let s = FixtureServer::start(vec![status("/search/movie", 401)]);
    let mut cfg = MediaConfig::default();
    cfg.tmdb.api_key = "K".into();
    cfg.tmdb.base_url = Some(s.base.clone());
    let state = Server::new(posts_of(&r, STATED));
    run_with(&mut h, &state, budget(600), cfg);
    wait_for("the id", || imdb(&h, FILM) == Some(200002));
    assert_eq!(bodies(&state), 1);
    assert_eq!(s.requests().len(), 1);
    wait_for("the step's bit", || done(&h, FILM) & NFO != 0);
    assert_eq!(done(&h, FILM) & mask(StepId(33)), 0, "the lookup is still owed");
    assert!(h.field(FILM, "net_next_at") > chrono::Utc::now().timestamp() + 60, "it waits for the provider");
}

#[test]
fn a_missing_article_is_done() {
    let mut h = fresh();
    let r = [with_nfo(FILM)];
    h.save(&r);
    let mut state = Server::new(posts_of(&r, STATED));
    Arc::get_mut(&mut state).unwrap().bodies = false;
    run_with(&mut h, &state, budget(600), MediaConfig::default());
    wait_for("the step", || done(&h, FILM) & NFO != 0);
    assert_eq!(imdb(&h, FILM), None);
    assert_eq!(h.bits(FILM).unwrap().1 & NFO, 0, "not failed");
}

#[test]
fn an_oversized_nfo_is_done() {
    let mut h = fresh();
    let r = [with_nfo(FILM)];
    h.save(&r);
    let big = [vec![b'x'; 200_000], STATED.to_vec()].concat();
    let state = Server::new(posts_of(&r, &big));
    run_with(&mut h, &state, budget(600), MediaConfig::default());
    wait_for("the step", || done(&h, FILM) & NFO != 0);
    assert_eq!(imdb(&h, FILM), None);
}

#[test]
fn regex_table() {
    assert_eq!(imdb_in_nfo(b"www.imdb.com/title/tt0133093/"), Some(133093));
    assert_eq!(imdb_in_nfo(b"IMDB: tt12345678 ok"), Some(12345678));
    assert_eq!(imdb_in_nfo(b"attt1234567"), None);
    assert_eq!(imdb_in_nfo(b"tt123456"), None);
}

#[test]
fn the_cap_is_64_kib() {
    assert_eq!(NFO_READ, 65_536);
}

#[test]
fn an_nfo_id_survives_a_fan_out_and_a_late_article() {
    let mut h = fresh();
    let r = [with_nfo(FILM)];
    h.save(&r);
    let state = Server::new(posts_of(&r, STATED));
    run_with(&mut h, &state, budget(600), MediaConfig::default());
    wait_for("the id", || imdb(&h, FILM) == Some(200002));
    let stage = h.stage.clone().unwrap();
    stage.rerun(h.id_of(FILM), PARSE).unwrap();
    settle();
    assert_eq!(imdb(&h, FILM), Some(200002), "a re-parse keeps it");
    let conn = h.shard();
    let key = title_key(Kind::Movie, &normalize("Some Movie"), Some(2019));
    conn.execute("insert into media_release (release_id, kind, title_key, quality) values (1, 2, ?, 0)", [key])
        .unwrap();
    let null = None::<i64>;
    let changed = conn
        .execute(atlas::media::fanout::UPDATE, rusqlite::params![key, 0, i64::MAX, null, null, null, 501, 200009, null])
        .unwrap();
    assert_eq!(changed, 2, "both get the title's TMDB id");
    assert_eq!(imdb(&h, FILM), Some(200002), "the NFO's stays");
    let other: i64 =
        conn.query_row("select imdb_id from media_release where release_id = 1", [], |r| r.get(0)).unwrap();
    assert_eq!(other, 200009, "the other takes the title's");
}

#[test]
fn caps_offers_imdbid_with_a_budget() {
    let mut cfg = MediaConfig::default();
    cfg.tvmaze.enabled = false;
    let on = query::id_params(&cfg, true);
    assert!(on.movie.imdb);
    assert!(!on.tv.imdb);
}
