//! A title's IDs: asked once, and what each answer leaves in the cache.

mod common;

use std::path::PathBuf;
use std::sync::Arc;

use atlas::config::{Config, MediaConfig};
use atlas::media::Media;
use atlas::media::resolve::{Resolved, TitleQuery};
use atlas::media::store::{self, TitleRow};
use atlas::media::title::Kind;
use common::http::*;

const NOW: i64 = 1_800_000_000;
const DAY: i64 = 86_400;
const FAST: i64 = 600_000;
const K: i64 = 77;

fn none() -> MediaConfig {
    let mut c = MediaConfig::default();
    c.tvmaze.requests_per_minute = FAST;
    c.tmdb.requests_per_minute = FAST;
    c.trakt.requests_per_minute = FAST;
    c
}

fn maze_on(mut c: MediaConfig, s: &FixtureServer) -> MediaConfig {
    c.tvmaze.enabled = true;
    c.tvmaze.base_url = Some(s.base.clone());
    c
}

fn tmdb_on(mut c: MediaConfig, s: &FixtureServer) -> MediaConfig {
    c.tmdb.api_key = "K".into();
    c.tmdb.base_url = Some(s.base.clone());
    c
}

fn trakt_on(mut c: MediaConfig, s: &FixtureServer) -> MediaConfig {
    c.trakt.client_id = "ID".into();
    c.trakt.base_url = Some(s.base.clone());
    c
}

struct Case {
    _dir: tempfile::TempDir,
    main: PathBuf,
    media: Arc<Media>,
}

fn case(cfg: MediaConfig) -> Case {
    let dir = tempfile::tempdir().unwrap();
    let main = dir.path().join("atlas.db");
    let media = Media::new(&main);
    media.set_config(&Config { media: cfg, ..Config::default() });
    Case { _dir: dir, main, media }
}

impl Case {
    fn row(&self) -> TitleRow {
        let conn = store::open(&store::path_for(&self.main)).unwrap();
        store::get(&conn, K).unwrap().expect("a row")
    }

    fn set(&self, cfg: MediaConfig) {
        self.media.set_config(&Config { media: cfg, ..Config::default() });
    }

    fn tv(&self, now: i64) -> Result<Resolved, String> {
        let q = TitleQuery { kind: Kind::Tv, title: "Some Show", year: None, country: None };
        self.media.resolve(&q, K, now)
    }

    fn movie(&self, now: i64) -> Result<Resolved, String> {
        let q = TitleQuery { kind: Kind::Movie, title: "Some Movie", year: Some(2019), country: None };
        self.media.resolve(&q, K, now)
    }
}

fn single() -> Route {
    route("/singlesearch/shows", fixture("tvmaze_show.json"))
}

fn tmdb_tv() -> Vec<Route> {
    vec![
        route("/search/tv", fixture("tmdb_search_tv.json")),
        route("/tv/401/external_ids", fixture("tmdb_tv_ids.json")),
    ]
}

fn trakt_show() -> Vec<Route> {
    vec![route("/search/show", fixture("trakt_show.json"))]
}

#[test]
fn a_title_is_asked_once() {
    let s = FixtureServer::start(vec![single()]);
    let c = case(maze_on(none(), &s));
    assert!(matches!(c.tv(NOW), Ok(Resolved::Found(_))));
    assert!(matches!(c.tv(NOW), Ok(Resolved::Found(_))));
    assert_eq!(s.requests().len(), 1);
}

#[test]
fn a_miss_is_cached() {
    let s = FixtureServer::start(vec![]);
    let c = case(maze_on(none(), &s));
    assert_eq!(c.tv(NOW), Ok(Resolved::NotFound));
    let row = c.row();
    assert_eq!((row.state, row.next_check), (store::STATE_NOT_FOUND, Some(NOW + 30 * DAY)));
    let asked = s.requests().len();
    assert_eq!(c.tv(NOW + DAY), Ok(Resolved::NotFound));
    assert_eq!(s.requests().len(), asked);
}

#[test]
fn a_miss_expires_and_doubles() {
    let s = FixtureServer::start(vec![]);
    let c = case(maze_on(none(), &s));
    assert_eq!(c.tv(NOW), Ok(Resolved::NotFound));
    let asked = s.requests().len();
    let later = NOW + 31 * DAY;
    assert_eq!(c.tv(later), Ok(Resolved::NotFound));
    assert_eq!(s.requests().len(), asked * 2);
    let row = c.row();
    assert_eq!((row.misses, row.next_check), (2, Some(later + 60 * DAY)));
    let mut at = later;
    for _ in 0..4 {
        at = c.row().next_check.unwrap();
        c.tv(at).unwrap();
    }
    assert_eq!(c.row().next_check, Some(at + 180 * DAY), "capped");
}

#[test]
fn a_miss_that_becomes_a_hit_records_ids_and_asks_for_a_fan_out() {
    let s = FixtureServer::start(vec![]);
    let c = case(maze_on(none(), &s));
    assert_eq!(c.tv(NOW), Ok(Resolved::NotFound));
    assert!(c.row().fanout_done);
    s.set_routes(vec![single()]);
    let Ok(Resolved::Found(ids)) = c.tv(NOW + 31 * DAY) else { panic!("not found") };
    assert_eq!(ids.tvmaze, Some(101));
    let row = c.row();
    assert_eq!((row.state, row.fanout_done), (store::STATE_FOUND, false));
}

#[test]
fn tv_asks_tvmaze_then_tmdb_then_trakt_and_merges() {
    let (a, b, t) =
        (FixtureServer::start(vec![single()]), FixtureServer::start(tmdb_tv()), FixtureServer::start(trakt_show()));
    let c = case(trakt_on(tmdb_on(maze_on(none(), &a), &b), &t));
    let Ok(Resolved::Found(ids)) = c.tv(NOW) else { panic!("not found") };
    assert_eq!((ids.tvmaze, ids.tmdb, ids.trakt, ids.imdb), (Some(101), Some(401), Some(601), Some(100001)));
    assert_eq!((a.requests().len(), b.requests().len(), t.requests().len()), (1, 2, 1));
    assert_eq!(c.row().checked_mask, 7);
}

#[test]
fn a_movie_never_asks_tvmaze() {
    let a = FixtureServer::start(vec![single()]);
    let b = FixtureServer::start(vec![
        route("/search/movie", fixture("tmdb_search_movie.json")),
        route("/movie/501/external_ids", fixture("tmdb_movie_ids.json")),
    ]);
    let c = case(tmdb_on(maze_on(none(), &a), &b));
    assert!(matches!(c.movie(NOW), Ok(Resolved::Found(_))));
    assert!(a.requests().is_empty());
}

#[test]
fn a_disagreeing_provider_is_ignored() {
    let a = FixtureServer::start(vec![single()]);
    let disagree = fixture("trakt_show.json").replace("\"tvdb\":3001", "\"tvdb\":9999");
    let t = FixtureServer::start(vec![route("/search/show", disagree)]);
    let c = case(trakt_on(maze_on(none(), &a), &t));
    let Ok(Resolved::Found(ids)) = c.tv(NOW) else { panic!("not found") };
    assert_eq!((ids.tvdb, ids.trakt), (Some(3001), None));
    assert_eq!(c.row().checked_mask, 5, "its bit is set");
}

#[test]
fn an_outage_records_no_miss() {
    let s = FixtureServer::start(vec![status("/singlesearch/shows", 500)]);
    let c = case(maze_on(none(), &s));
    let Ok(Resolved::Later(_)) = c.tv(NOW) else { panic!("not later") };
    let row = c.row();
    assert_eq!((row.state, row.checked_mask), (store::STATE_ASKING, 0));
    // the pause lasts a minute, and a new config clears it as a fixed key would
    s.set_routes(vec![single()]);
    c.set(none());
    c.set(maze_on(none(), &s));
    assert!(matches!(c.tv(NOW + 1), Ok(Resolved::Found(_))));
}

#[test]
fn one_provider_down_keeps_the_others_answer() {
    let a = FixtureServer::start(vec![status("/singlesearch/shows", 500)]);
    let b = FixtureServer::start(tmdb_tv());
    let c = case(tmdb_on(maze_on(none(), &a), &b));
    let Ok(Resolved::Found(ids)) = c.tv(NOW) else { panic!("not found") };
    assert_eq!(ids.tmdb, Some(401));
    assert_eq!(c.row().checked_mask, 2);
    let tmdb_asked = b.requests().len();
    a.set_routes(vec![single()]);
    c.set(none());
    c.set(tmdb_on(maze_on(none(), &a), &b));
    let Ok(Resolved::Found(ids)) = c.tv(NOW + 1) else { panic!("not found") };
    assert_eq!(ids.tvmaze, Some(101));
    assert_eq!(b.requests().len(), tmdb_asked, "only TVMaze was asked");
    assert_eq!(c.row().checked_mask, 3);
}

#[test]
fn bad_json_is_an_attempt() {
    let s = FixtureServer::start(vec![route("/singlesearch/shows", "<html>".into())]);
    let c = case(maze_on(none(), &s));
    assert!(c.tv(NOW).is_err());
    assert_eq!(c.row().checked_mask, 0);
}

#[test]
fn a_bad_key_is_later_and_records_nothing() {
    let s = FixtureServer::start(vec![status("/search/tv", 401)]);
    let c = case(tmdb_on(none(), &s));
    assert!(matches!(c.tv(NOW), Ok(Resolved::Later(_))));
    let row = c.row();
    assert_eq!((row.state, row.checked_mask), (store::STATE_ASKING, 0));
}

#[test]
fn a_new_provider_is_asked_for_a_known_title() {
    let (a, t) = (FixtureServer::start(vec![single()]), FixtureServer::start(trakt_show()));
    let c = case(maze_on(none(), &a));
    assert!(matches!(c.tv(NOW), Ok(Resolved::Found(_))));
    let conn = store::open(&store::path_for(&c.main)).unwrap();
    store::fanout_finished(&conn, K).unwrap();
    c.set(trakt_on(maze_on(none(), &a), &t));
    let Ok(Resolved::Found(ids)) = c.tv(NOW + 1) else { panic!("not found") };
    assert_eq!(ids.trakt, Some(601));
    assert_eq!((a.requests().len(), t.requests().len()), (1, 1));
    assert!(!c.row().fanout_done);
}

#[test]
fn every_provider_off_makes_no_request() {
    let s = FixtureServer::start(vec![single()]);
    let c = case(none());
    assert_eq!(c.tv(NOW), Ok(Resolved::NotFound));
    assert!(s.requests().is_empty());
}

#[test]
fn no_media_file_without_a_provider() {
    let c = case(none());
    assert_eq!(c.media.cached_ids(K), None);
    assert!(!store::path_for(&c.main).exists());
}

#[test]
fn a_row_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let conn = store::open(&dir.path().join("atlas.media.db")).unwrap();
    let mut row =
        TitleRow { key: 5, kind: 1, title: "x".into(), year: Some(2019), checked_mask: 3, ..TitleRow::default() };
    row.ids.tvdb = Some(9);
    row.found_title = Some("y".into());
    row.fanout_done = true;
    store::put(&conn, &row).unwrap();
    assert_eq!(store::get(&conn, 5).unwrap(), Some(row));
}

#[test]
fn claims_stop_at_their_cap() {
    let s = FixtureServer::start(vec![]);
    let c = case(maze_on(none(), &s));
    c.media.with_max_claims(3);
    for key in 1..=3 {
        assert!(c.media.wants(Kind::Tv, key, key * 10, NOW));
    }
    assert!(c.media.wants(Kind::Tv, 1, 10, NOW), "its holder keeps it");
    assert!(!c.media.wants(Kind::Tv, 1, 11, NOW), "another release waits");
    assert!(!c.media.wants(Kind::Tv, 4, 40, NOW));
    assert_eq!(c.media.claims_full.load(std::sync::atomic::Ordering::Relaxed), 1);
    assert!(c.media.wants(Kind::Tv, 4, 40, NOW + 8 * DAY), "an old claim makes room");
}

#[test]
fn a_found_title_needs_no_new_lookup_and_a_new_provider_does() {
    let (a, t) = (FixtureServer::start(vec![single()]), FixtureServer::start(trakt_show()));
    let c = case(maze_on(none(), &a));
    assert!(c.media.wants(Kind::Tv, K, 1, NOW));
    assert!(matches!(c.tv(NOW), Ok(Resolved::Found(_))));
    assert!(!c.media.wants(Kind::Tv, K, 2, NOW));
    c.set(trakt_on(maze_on(none(), &a), &t));
    assert!(c.media.wants(Kind::Tv, K, 2, NOW));
}

#[test]
fn a_miss_expires_for_wants() {
    let s = FixtureServer::start(vec![]);
    let c = case(maze_on(none(), &s));
    c.tv(NOW).unwrap();
    assert!(!c.media.wants(Kind::Tv, K, 1, NOW + DAY));
    assert!(c.media.wants(Kind::Tv, K, 1, NOW + 31 * DAY));
}

#[test]
fn titles_stop_at_their_cap() {
    let s = FixtureServer::start(vec![]);
    let mut cfg = maze_on(none(), &s);
    cfg.max_titles = 1_000;
    let dir = tempfile::tempdir().unwrap();
    let main = dir.path().join("atlas.db");
    let conn = store::open(&store::path_for(&main)).unwrap();
    for key in 1..=1_000 {
        let mut t = TitleRow {
            key,
            kind: 1,
            title: "x".into(),
            state: store::STATE_FOUND,
            checked_mask: 1,
            fanout_done: true,
            ..TitleRow::default()
        };
        t.ids.tvdb = Some(key);
        store::put(&conn, &t).unwrap();
    }
    let media = Media::new(&main);
    media.set_config(&Config { media: cfg, ..Config::default() });
    assert!(!media.wants(Kind::Tv, 5_000, 1, NOW));
    assert_eq!(media.titles_full.load(std::sync::atomic::Ordering::Relaxed), 1);
    assert_eq!(media.cached_ids(7).and_then(|i| i.tvdb), Some(7), "a known title still answers");
    assert!(s.requests().is_empty());
}

#[test]
fn a_corrupt_cache_stops_only_the_lookups() {
    let dir = tempfile::tempdir().unwrap();
    let main = dir.path().join("atlas.db");
    std::fs::write(store::path_for(&main), vec![b'x'; 8_192]).unwrap();
    let s = FixtureServer::start(vec![]);
    let media = Media::new(&main);
    media.set_config(&Config { media: maze_on(none(), &s), ..Config::default() });
    assert!(!media.wants(Kind::Tv, K, 1, NOW));
    assert_eq!(media.cached_ids(K), None);
}
