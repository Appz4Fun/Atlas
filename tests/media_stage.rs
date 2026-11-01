//! TV and movie IDs, through the stage.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use atlas::category::CategoryStep;
use atlas::config::{CategoryConfig, Config, MediaConfig, PostprocessConfig};
use atlas::indexer::{Db, shared_db};
use atlas::media::steps::{MediaLookup, MediaNfo, MediaParse};
use atlas::media::title::{Kind, normalize, title_key};
use atlas::media::{Media, fanout, store as media_store};
use atlas::nntp::Pool;
use atlas::parser::Release;
use atlas::postprocess::step::{NameChange, Needs, Outcome, Step, StepId, mask};
use atlas::postprocess::{Stage, StageParams};
use atlas::{db, store};
use common::http::*;
use common::stage::*;
use rusqlite::Connection;

const PARSE: i64 = mask(StepId(32));
const LOOKUP: i64 = mask(StepId(33));
const SHOW: &str = "Some.Show.S01E02.720p.HDTV.x264-GRP";
const FAST: i64 = 600_000;

fn no_providers() -> MediaConfig {
    let mut c = MediaConfig::default();
    c.tvmaze.requests_per_minute = FAST;
    c.tmdb.requests_per_minute = FAST;
    c
}

fn maze(s: &FixtureServer) -> MediaConfig {
    let mut c = no_providers();
    c.tvmaze.enabled = true;
    c.tvmaze.base_url = Some(s.base.clone());
    c
}

fn with_tmdb(mut c: MediaConfig, s: &FixtureServer) -> MediaConfig {
    c.tmdb.api_key = "K".into();
    c.tmdb.base_url = Some(s.base.clone());
    c
}

fn config(media: MediaConfig) -> Config {
    Config { postprocess: PostprocessConfig { min_size_bytes: 0, ..quick_config() }, media, ..Config::default() }
}

fn steps_of(media: &Arc<Media>) -> Vec<Arc<dyn Step>> {
    vec![
        Arc::new(CategoryStep::new()),
        Arc::new(MediaParse(media.clone())),
        Arc::new(MediaLookup(media.clone())),
        Arc::new(MediaNfo(media.clone())),
    ]
}

fn start_with(h: &mut Harness, cfg: Config, steps: Vec<Arc<dyn Step>>) -> Db {
    let db = shared_db(db::open_at(&h.main).unwrap());
    h.stage = Some(Stage::start(StageParams {
        main: h.main.clone(),
        db: db.clone(),
        pool: Arc::new(Pool::new(&[])),
        config: cfg,
        steps,
        handle: h.runtime.handle().clone(),
    }));
    db
}

/// A stage on `cfg`, with a fresh `Media`.
fn start(h: &mut Harness, cfg: MediaConfig) -> (Arc<Media>, Db) {
    let media = Media::new(&h.main);
    media.with_second_pass(Duration::from_millis(200));
    let db = start_with(h, config(cfg), steps_of(&media));
    (media, db)
}

struct Fan {
    stop: Arc<AtomicBool>,
    _runtime: tokio::runtime::Runtime,
}

impl Drop for Fan {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        std::thread::sleep(Duration::from_millis(150));
    }
}

fn fan_out(media: &Arc<Media>, db: &Db) -> Fan {
    let stop = Arc::new(AtomicBool::new(false));
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let flag = stop.clone();
    let task = atlas::media::run(media.clone(), db.clone(), move || flag.load(Ordering::Relaxed));
    let handle = std::thread::spawn({
        let handle = runtime.handle().clone();
        move || handle.block_on(task)
    });
    drop(handle);
    Fan { stop, _runtime: runtime }
}

#[derive(Debug, Default, PartialEq)]
struct Row {
    kind: i64,
    key: i64,
    season: Option<i64>,
    episode: Option<i64>,
    last: Option<i64>,
    air: Option<i64>,
    year: Option<i64>,
    tvdb: Option<i64>,
    tvmaze: Option<i64>,
    trakt: Option<i64>,
    tmdb: Option<i64>,
    imdb: Option<i64>,
    rage: Option<i64>,
    nfo: i64,
}

fn row_on(conn: &Connection, id: i64) -> Option<Row> {
    conn.query_row(
        "select kind, title_key, season, episode, episode_last, air_date, year, tvdb_id, tvmaze_id, trakt_id, tmdb_id, imdb_id,
                tvrage_id, imdb_from_nfo from media_release where release_id = ?",
        [id],
        |r| {
            Ok(Row {
                kind: r.get(0)?,
                key: r.get(1)?,
                season: r.get(2)?,
                episode: r.get(3)?,
                last: r.get(4)?,
                air: r.get(5)?,
                year: r.get(6)?,
                tvdb: r.get(7)?,
                tvmaze: r.get(8)?,
                trakt: r.get(9)?,
                tmdb: r.get(10)?,
                imdb: r.get(11)?,
                rage: r.get(12)?,
                nfo: r.get(13)?,
            })
        },
    )
    .ok()
}

fn row(h: &Harness, name: &str) -> Option<Row> {
    row_on(&h.shard(), h.id_of(name))
}

fn key(kind: Kind, title: &str, year: Option<i32>) -> i64 {
    title_key(kind, &normalize(title), year)
}

fn done(h: &Harness, name: &str) -> i64 {
    h.bits(name).map_or(0, |b| b.0)
}

fn wait_parsed(h: &Harness, name: &str) {
    wait_for("the parse step", || done(h, name) & PARSE != 0);
}

fn single() -> Route {
    route("/singlesearch/shows", fixture("tvmaze_show.json"))
}

#[test]
fn a_tv_release_gets_its_row_with_no_ids() {
    let mut h = fresh();
    h.save(&[complete(SHOW)]);
    start(&mut h, no_providers());
    wait_parsed(&h, SHOW);
    let r = row(&h, SHOW).unwrap();
    let want =
        Row { kind: 1, key: key(Kind::Tv, "Some Show", None), season: Some(1), episode: Some(2), ..Row::default() };
    assert_eq!(r, want);
    assert!(!media_store::path_for(&h.main).exists());
}

#[test]
fn a_movie_release_gets_its_row() {
    let mut h = fresh();
    let name = "Some.Movie.2019.1080p.BluRay.x264-GRP";
    h.save(&[complete(name)]);
    start(&mut h, no_providers());
    wait_parsed(&h, name);
    let r = row(&h, name).unwrap();
    assert_eq!((r.kind, r.year, r.season), (2, Some(2019), None));
    assert_eq!(r.key, key(Kind::Movie, "Some Movie", Some(2019)));
}

#[test]
fn a_daily_show_is_stored_by_date() {
    let mut h = fresh();
    let name = "Some.Show.2024.03.15.720p.HDTV.x264-GRP";
    h.save(&[complete(name)]);
    start(&mut h, no_providers());
    wait_parsed(&h, name);
    let r = row(&h, name).unwrap();
    assert_eq!((r.season, r.episode, r.air), (Some(2024), Some(315), Some(20240315)));
}

#[test]
fn a_season_pack_has_no_episode() {
    let mut h = fresh();
    let name = "Some.Show.S03.720p.HDTV.x264-GRP";
    h.save(&[complete(name)]);
    start(&mut h, no_providers());
    wait_parsed(&h, name);
    let r = row(&h, name).unwrap();
    assert_eq!((r.season, r.episode, r.last), (Some(3), None, None));
}

#[test]
fn with_no_fact_it_classifies_for_itself() {
    let mut h = fresh();
    h.save(&[complete(SHOW)]);
    start(&mut h, no_providers());
    wait_parsed(&h, SHOW);
    let id = h.id_of(SHOW);
    h.shard().execute("delete from media_release where release_id = ?", [id]).unwrap();
    h.stage.clone().unwrap().rerun(id, PARSE).unwrap();
    wait_for("the row again", || row(&h, SHOW).is_some());
}

#[test]
fn it_runs_with_the_category_step_off() {
    let mut h = fresh();
    h.save(&[complete(SHOW)]);
    let media = Media::new(&h.main);
    let cfg = Config { category: CategoryConfig { enabled: false, ..Default::default() }, ..config(no_providers()) };
    start_with(&mut h, cfg, steps_of(&media));
    wait_parsed(&h, SHOW);
    assert_eq!(row(&h, SHOW).unwrap().season, Some(1));
    let rows: i64 = h.shard().query_row("select count(*) from release_category", [], |r| r.get(0)).unwrap();
    assert_eq!(rows, 0);
}

#[test]
fn a_group_hint_alone_gets_no_row() {
    let mut h = fresh();
    let (name, group) = ("Plain Name Nothing Else", "alt.binaries.movies");
    h.save(&[Release { group: group.into(), ..complete(name) }]);
    let s = FixtureServer::start(vec![]);
    let mut c = with_tmdb(no_providers(), &s);
    c.tvmaze.enabled = false;
    start(&mut h, c);
    let shard = db::open_shard(&store::shard_path(&h.main, store::shard_of(group))).unwrap();
    let id: i64 = shard.query_row("select id from releases where name = ?", [name], |r| r.get(0)).unwrap();
    wait_for("its bit", || {
        shard.query_row("select done from pp_state where release_id = ?", [id], |r| r.get::<_, i64>(0)).unwrap_or(0)
            & PARSE
            != 0
    });
    assert_eq!(row_on(&shard, id), None);
    std::thread::sleep(Duration::from_millis(1_200));
    assert!(s.requests().is_empty(), "{:?}", s.requests());
}

#[test]
fn a_parse_held_past_the_fan_out_gets_the_ids() {
    let mut h = fresh();
    h.save(&[complete(SHOW)]);
    let s = FixtureServer::start(vec![single()]);
    let media = Media::new(&h.main);
    media.with_second_pass(Duration::from_millis(100));
    let (entered, go) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)));
    let log: Log = Default::default();
    let holder = {
        let (entered, go) = (entered.clone(), go.clone());
        TestStep::new(35, "holder", 505, Needs::Database, &log).runs(move |_| {
            entered.store(true, Ordering::Relaxed);
            while !go.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(5));
            }
            Ok(Outcome::default())
        })
    };
    let steps: Vec<Arc<dyn Step>> =
        vec![Arc::new(CategoryStep::new()), Arc::new(MediaParse(media.clone())), holder.arc()];
    let mut c = maze(&s);
    c.nfo = false;
    let db = start_with(&mut h, config(c), steps);
    let _fan = fan_out(&media, &db);
    wait_for("the parse to run", || entered.load(Ordering::Relaxed));
    assert_eq!(row(&h, SHOW), None, "held");
    let q = atlas::media::resolve::TitleQuery { kind: Kind::Tv, title: "Some Show", year: None, country: None };
    let k = key(Kind::Tv, "Some Show", None);
    media.resolve(&q, k, 1_800_000_000).unwrap();
    let cache = media_store::open(&media_store::path_for(&h.main)).unwrap();
    wait_for("both passes", || media_store::get(&cache, k).unwrap().is_some_and(|t| t.fanout_done));
    go.store(true, Ordering::Relaxed);
    wait_for("the IDs", || row(&h, SHOW).is_some_and(|r| r.tvdb == Some(3001)));
}

#[test]
fn a_late_article_keeps_ids() {
    let mut h = fresh();
    h.save(&[complete(SHOW)]);
    start(&mut h, no_providers());
    wait_parsed(&h, SHOW);
    let id = h.id_of(SHOW);
    h.shard().execute("update media_release set imdb_id = 5, imdb_from_nfo = 1 where release_id = ?", [id]).unwrap();
    h.stage.clone().unwrap().rerun(id, PARSE).unwrap();
    wait_parsed(&h, SHOW);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(row(&h, SHOW).unwrap().imdb, Some(5));

    h.shard()
        .execute("update releases set display_name = 'Other.Show.S01E02.720p.HDTV.x264-GRP' where id = ?", [id])
        .unwrap();
    h.stage.clone().unwrap().rerun(id, PARSE).unwrap();
    let other = key(Kind::Tv, "Other Show", None);
    wait_for("a new title", || row(&h, SHOW).is_some_and(|r| r.key == other));
    let r = row(&h, SHOW).unwrap();
    assert_eq!((r.imdb, r.nfo), (None, 0));
}

/// Two releases of a title, all found and fanned out with `tt0100001`.
fn found_title(h: &mut Harness, s: &FixtureServer) -> (Arc<Media>, Db, Fan) {
    h.save(&[complete(SHOW), complete("Some.Show.S01E03.720p.HDTV.x264-GRP")]);
    let (media, db) = start(h, maze(s));
    let fan = fan_out(&media, &db);
    wait_for("both rows", || {
        [SHOW, "Some.Show.S01E03.720p.HDTV.x264-GRP"].iter().all(|n| row(h, n).is_some_and(|r| r.imdb == Some(100001)))
    });
    (media, db, fan)
}

fn correct_it(h: &mut Harness, s: &FixtureServer, rep: i64) {
    let k = key(Kind::Tv, "Some Show", None);
    h.stop();
    let cache = media_store::open(&media_store::path_for(&h.main)).unwrap();
    cache.execute("delete from media_titles where title_key = ?", [k]).unwrap();
    s.set_routes(vec![route("/singlesearch/shows", fixture("tvmaze_show.json").replace("tt0100001", "tt0100009"))]);
    let (media, db) = start(h, maze(s));
    let fan = fan_out(&media, &db);
    h.stage.clone().unwrap().rerun(rep, LOOKUP).unwrap();
    wait_for("the new answer", || {
        media_store::get(&cache, k).unwrap().is_some_and(|t| t.ids.imdb == Some(100009) && t.fanout_done)
    });
    drop(fan);
}

#[test]
fn a_corrected_provider_result_replaces_a_providers_imdb() {
    let mut h = fresh();
    let s = FixtureServer::start(vec![single()]);
    let (_media, _db, fan) = found_title(&mut h, &s);
    drop(fan);
    let rep = h.id_of(SHOW);
    correct_it(&mut h, &s, rep);
    for name in [SHOW, "Some.Show.S01E03.720p.HDTV.x264-GRP"] {
        assert_eq!(row(&h, name).unwrap().imdb, Some(100009), "{name}");
    }
}

#[test]
fn an_nfos_imdb_survives_the_correction() {
    let mut h = fresh();
    let s = FixtureServer::start(vec![single()]);
    let (_media, _db, fan) = found_title(&mut h, &s);
    drop(fan);
    let other = "Some.Show.S01E03.720p.HDTV.x264-GRP";
    h.shard()
        .execute("update media_release set imdb_id = 777, imdb_from_nfo = 1 where release_id = ?", [h.id_of(other)])
        .unwrap();
    let rep = h.id_of(SHOW);
    correct_it(&mut h, &s, rep);
    assert_eq!(row(&h, SHOW).unwrap().imdb, Some(100009));
    assert_eq!(row(&h, other).unwrap().imdb, Some(777));
    h.stage.clone().unwrap().rerun(h.id_of(other), PARSE).unwrap();
    std::thread::sleep(Duration::from_millis(1_200));
    assert_eq!(row(&h, other).unwrap().imdb, Some(777), "through the parse too");
}

fn assert_no_row(name: &str) {
    let mut h = fresh();
    h.save(&[complete(name)]);
    start(&mut h, no_providers());
    wait_parsed(&h, name);
    assert_eq!(row(&h, name), None, "{name}");
}

#[test]
fn an_audio_release_gets_no_row() {
    assert_no_row("Artist-Album-WEB-2019-320kbps-MP3");
}

#[test]
fn a_hidden_release_gets_no_row() {
    assert_no_row("0123456789abcdef0123456789abcdef");
}

#[test]
fn a_rename_away_from_tv_drops_the_row() {
    let mut h = fresh();
    h.save(&[complete(SHOW)]);
    let media = Media::new(&h.main);
    let log: Log = Default::default();
    let renamer = TestStep::new(12, "renamer", 250, Needs::Database, &log).reruns(false, true).runs(|ctx| {
        if ctx.release().display_name.is_some() || ctx.fact("go").is_none() && ctx.release().parts < 3 {
            return Ok(Outcome::default());
        }
        let name = NameChange {
            display_name: Some("Artist-Album-WEB-2019-320kbps-MP3".into()),
            obfuscated: Some(false),
            source: 5,
        };
        Ok(Outcome { name: Some(name), ..Outcome::default() })
    });
    let mut steps = steps_of(&media);
    steps.insert(0, renamer.arc());
    start_with(&mut h, config(no_providers()), steps);
    wait_parsed(&h, SHOW);
    assert!(row(&h, SHOW).is_some());
    let mut grown = release_of(SHOW, &format!("{SHOW}.bin"), 3, 3);
    grown.articles.drain(..2);
    h.save(&[grown]);
    wait_for("the row to go", || row(&h, SHOW).is_none());
}

#[test]
fn parse_off_writes_nothing() {
    let mut h = fresh();
    h.save(&[complete(SHOW)]);
    let mut c = no_providers();
    c.parse = false;
    start(&mut h, c);
    wait_for("the category step", || done(&h, SHOW) & mask(StepId(24)) != 0);
    assert_eq!(row(&h, SHOW), None);
}

#[test]
fn no_config_no_requests() {
    let mut h = fresh();
    h.save(&[complete(SHOW)]);
    let s = FixtureServer::start(vec![single()]);
    let mut c = no_providers();
    c.tvmaze.base_url = Some(s.base.clone());
    c.tmdb.base_url = Some(s.base.clone());
    start(&mut h, c);
    wait_parsed(&h, SHOW);
    std::thread::sleep(Duration::from_millis(1_200));
    assert!(s.requests().is_empty());
    assert_eq!(h.bits(SHOW).unwrap().2, None, "net_wanted");
    assert_eq!(done(&h, SHOW) & LOOKUP, 0);
    assert!(!media_store::path_for(&h.main).exists());
}

fn net_wanted(h: &Harness) -> i64 {
    store::shard_paths(&h.main)
        .iter()
        .map(|p| {
            db::open_shard(p)
                .unwrap()
                .query_row("select count(*) from pp_state where net_wanted is not null", [], |r| r.get::<_, i64>(0))
                .unwrap()
        })
        .sum()
}

fn all_ids_set(h: &Harness) -> i64 {
    store::shard_paths(&h.main)
        .iter()
        .map(|p| {
            db::open_shard(p)
                .unwrap()
                .query_row("select count(*) from media_release where tvdb_id is not null", [], |r| r.get::<_, i64>(0))
                .unwrap()
        })
        .sum()
}

fn all_rows(h: &Harness) -> i64 {
    store::shard_paths(&h.main)
        .iter()
        .map(|p| {
            db::open_shard(p)
                .unwrap()
                .query_row("select count(*) from media_release", [], |r| r.get::<_, i64>(0))
                .unwrap()
        })
        .sum()
}

#[test]
fn one_release_per_title_reaches_the_net_lane() {
    let mut h = fresh();
    let releases: Vec<Release> = (0..50)
        .map(|n| {
            let name = format!("Some.Show.S{:02}E{:02}.720p.HDTV.x264-GRP", n / 10 + 1, n % 10 + 1);
            Release { group: format!("alt.binaries.teevee{}", n % 5), ..complete(&name) }
        })
        .collect();
    h.save(&releases);
    let s = FixtureServer::start(vec![single()]);
    start(&mut h, maze(&s));
    wait_for("the rows", || all_rows(&h) == 50);
    // a release parsed after the title resolved takes its IDs from the cache, soo this can be more than one
    wait_for("the one with IDs", || all_ids_set(&h) >= 1);
    wait_for("the lane to empty", || net_wanted(&h) == 0);
    assert_eq!(s.requests().len(), 1, "{:?}", s.requests());
}

/// `names` go nowhere, while the fixture's own show proves the lane ran.
fn no_request_for(h: &mut Harness, others: &[Release]) {
    h.save(others);
    h.save(&[complete(SHOW)]);
    let s = FixtureServer::start(vec![single()]);
    start(h, maze(&s));
    wait_for("the fixture's show", || row(h, SHOW).is_some_and(|r| r.tvdb == Some(3001)));
    std::thread::sleep(Duration::from_millis(1_500));
    assert_eq!(s.requests().len(), 1, "{:?}", s.requests());
}

#[test]
fn only_tv_and_movie_go_out() {
    let mut h = fresh();
    no_request_for(
        &mut h,
        &[
            complete("Artist-Album-WEB-2019-320kbps-MP3"),
            complete("Some.Author.Some.Book.2020.Retail.EPUB-GRP"),
            complete("Some.Program.v1.2.Incl.Keygen-GRP"),
            complete("0123456789abcdef0123456789abcdef"),
        ],
    );
}

#[test]
fn a_blocked_group_makes_no_request() {
    let mut h = fresh();
    let group = "alt.binaries.pictures.erotica.teen";
    let shard = store::shard_of(group);
    let id = store::global_id(1, shard);
    let conn = db::open_shard(&store::shard_path(&h.main, shard)).unwrap();
    conn.execute(
        "insert into releases (id, name, group_name, poster, posted_date, size, complete, parts, is_obfuscated)
         values (?1, 'Blocked.Show.S01E01.720p.HDTV.x264-GRP', ?2, 'p', '2026-10-02 10:11:12', 10000000, 1, 1, 0)",
        rusqlite::params![id, group],
    )
    .unwrap();
    no_request_for(&mut h, &[]);
    assert_eq!(row_on(&conn, id).and_then(|r| r.tvdb), None);
}

#[test]
fn a_rejected_release_makes_no_request() {
    let mut h = fresh();
    let r = release_of("Rejected.Show.S01E01.720p.HDTV.x264-GRP", "rejected.show.vol00+1.par2", 2, 2);
    no_request_for(&mut h, &[r]);
}

#[test]
fn a_group_hint_alone_makes_no_request() {
    let mut h = fresh();
    no_request_for(&mut h, &[Release { group: "alt.binaries.teevee".into(), ..complete("Plain Name Nothing Else") }]);
}

#[test]
fn an_image_post_makes_no_request() {
    let mut h = fresh();
    no_request_for(&mut h, &[complete("Some.Movie.2019.1080p.jpg")]);
}

#[test]
fn a_release_after_the_title_resolved_gets_ids_from_the_cache() {
    let mut h = fresh();
    h.save(&[complete(SHOW)]);
    let s = FixtureServer::start(vec![single()]);
    start(&mut h, maze(&s));
    wait_for("the IDs", || row(&h, SHOW).is_some_and(|r| r.tvdb == Some(3001)));
    let later = "Some.Show.S02E07.720p.HDTV.x264-GRP";
    h.save(&[complete(later)]);
    wait_for("its IDs", || row(&h, later).is_some_and(|r| r.tvdb == Some(3001)));
    assert_eq!(s.requests().len(), 1);
}

#[test]
fn an_outage_waits_without_using_attempts() {
    let mut h = fresh();
    h.save(&[complete(SHOW)]);
    let s = FixtureServer::start(vec![status("/singlesearch/shows", 500)]);
    start(&mut h, maze(&s));
    wait_for("the request", || !s.requests().is_empty());
    wait_for("it to wait", || h.field(SHOW, "coalesce(net_next_at, 0)") > chrono::Utc::now().timestamp() + 30);
    assert_eq!(h.field(SHOW, "net_attempts"), 0);
    assert_eq!(done(&h, SHOW) & LOOKUP, 0);
    assert_eq!(h.bits(SHOW).unwrap().1 & LOOKUP, 0, "not failed");
}

#[test]
fn the_representative_gets_its_ids_through_the_step() {
    let mut h = fresh();
    h.save(&[complete(SHOW)]);
    let s = FixtureServer::start(vec![single()]);
    start(&mut h, maze(&s));
    wait_for("the IDs", || row(&h, SHOW).is_some_and(|r| r.tvdb == Some(3001) && r.imdb == Some(100001)));
}

#[test]
fn a_provider_still_owed_keeps_the_release_in_the_lane() {
    let mut h = fresh();
    h.save(&[complete(SHOW)]);
    let limited = Route { headers: vec![("Retry-After", "1".into())], ..status("/singlesearch/shows", 429) };
    let a = FixtureServer::start(vec![limited]);
    let b = FixtureServer::start(vec![
        route("/search/tv", fixture("tmdb_search_tv.json")),
        route("/tv/401/external_ids", fixture("tmdb_tv_ids.json")),
    ]);
    let (media, db) = start(&mut h, with_tmdb(maze(&a), &b));
    let _fan = fan_out(&media, &db);
    wait_for("TMDB's IDs, by the fan-out", || row(&h, SHOW).is_some_and(|r| r.tmdb == Some(401)));
    assert_eq!(done(&h, SHOW) & LOOKUP, 0, "TVMaze hasn't answered");
    assert_eq!(h.field(SHOW, "net_attempts"), 0);
    a.set_routes(vec![single()]);
    // it waits out the pause and up to 30 s more, by its id
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while done(&h, SHOW) & LOOKUP == 0 {
        assert!(std::time::Instant::now() < deadline, "never asked again");
        std::thread::sleep(Duration::from_millis(50));
    }
    let r = row(&h, SHOW).unwrap();
    assert_eq!((r.tvmaze, r.tmdb), (Some(101), Some(401)));
}

#[test]
fn a_stale_lookup_cant_attach_the_old_titles_ids() {
    let mut h = fresh();
    h.save(&[complete(SHOW)]);
    let s = FixtureServer::start(vec![single()]);
    s.hold();
    start(&mut h, maze(&s));
    wait_for("the lookup to start", || !s.requests().is_empty());
    let id = h.id_of(SHOW);
    h.shard()
        .execute("update releases set display_name = 'Other.Show.S01E02.720p.HDTV.x264-GRP' where id = ?", [id])
        .unwrap();
    h.stage.clone().unwrap().rerun(id, PARSE | LOOKUP).unwrap();
    let other = key(Kind::Tv, "Other Show", None);
    wait_for("the new title", || row(&h, SHOW).is_some_and(|r| r.key == other));
    s.release();
    wait_for("the new title's lookup", || s.requests().iter().any(|r| r.contains("q=Other")));
    wait_for("the lane to empty", || net_wanted(&h) == 0);
    std::thread::sleep(Duration::from_millis(300));
    let r = row(&h, SHOW).unwrap();
    assert_eq!((r.key, r.tvdb, r.imdb), (other, None, None));
}

#[test]
fn a_rename_gets_a_new_title() {
    let mut h = fresh();
    h.save(&[complete(SHOW)]);
    let s = FixtureServer::start(vec![single()]);
    start(&mut h, maze(&s));
    wait_for("the IDs", || row(&h, SHOW).is_some_and(|r| r.tvdb == Some(3001)));
    let id = h.id_of(SHOW);
    h.shard()
        .execute("update releases set display_name = 'Other.Show.S01E02.720p.HDTV.x264-GRP' where id = ?", [id])
        .unwrap();
    h.stage.clone().unwrap().rerun(id, PARSE | LOOKUP).unwrap();
    wait_for("the new title's lookup", || s.requests().iter().any(|r| r.contains("q=Other")));
    wait_for("its IDs to clear", || row(&h, SHOW).is_some_and(|r| r.tvdb.is_none()));
}

#[test]
fn a_provider_turning_on_starts_a_walk() {
    let mut h = fresh();
    h.save(&[complete(SHOW), complete("Some.Show.S01E03.720p.HDTV.x264-GRP")]);
    start(&mut h, no_providers());
    wait_parsed(&h, SHOW);
    h.stop();
    let s = FixtureServer::start(vec![single()]);
    start(&mut h, maze(&s));
    wait_for("one to get IDs", || all_ids_set(&h) >= 1);
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(s.requests().len(), 1, "{:?}", s.requests());
}

#[test]
fn a_deleted_cache_row_needs_a_restart() {
    let mut h = fresh();
    h.save(&[complete(SHOW)]);
    let s = FixtureServer::start(vec![single()]);
    start(&mut h, maze(&s));
    wait_for("the IDs", || row(&h, SHOW).is_some_and(|r| r.tvdb == Some(3001)));
    let k = key(Kind::Tv, "Some Show", None);
    let cache = media_store::open(&media_store::path_for(&h.main)).unwrap();
    cache.execute("delete from media_titles where title_key = ?", [k]).unwrap();
    h.stage.clone().unwrap().rerun(h.id_of(SHOW), LOOKUP).unwrap();
    std::thread::sleep(Duration::from_millis(1_500));
    assert_eq!(s.requests().len(), 1, "the map still says it's known");
    assert_eq!(h.bits(SHOW).unwrap().2, None);
}

#[test]
fn an_oversized_title_is_no_target() {
    let long = format!("{}.S01E02.720p.HDTV.x264-GRP", "Word.".repeat(200_000));
    assert_eq!(atlas::media::media_target(5040, "tv", &long).map(|_| ()), None);

    let name = format!("{}.S01E02.720p.HDTV.x264-GRP", "Word.".repeat(100));
    let mut h = fresh();
    h.save(&[release_of(&name, "x.bin", 2, 2)]);
    let s = FixtureServer::start(vec![single()]);
    start(&mut h, maze(&s));
    wait_parsed(&h, &name);
    std::thread::sleep(Duration::from_millis(1_200));
    assert_eq!(row(&h, &name), None);
    assert!(s.requests().is_empty());
}

#[test]
fn a_second_provider_turning_on_starts_a_walk() {
    let mut h = fresh();
    let film = "Some.Movie.2019.1080p.BluRay.x264-GRP";
    h.save(&[complete(SHOW), complete(film)]);
    let a = FixtureServer::start(vec![single()]);
    start(&mut h, maze(&a));
    wait_for("the IDs", || row(&h, SHOW).is_some_and(|r| r.tvdb == Some(3001)));
    h.stop();
    let asked = a.requests().len();
    let b = FixtureServer::start(vec![
        route("/search/tv", fixture("tmdb_search_tv.json")),
        route("/tv/401/external_ids", fixture("tmdb_tv_ids.json")),
        route("/search/movie", fixture("tmdb_search_movie.json")),
        route("/movie/501/external_ids", fixture("tmdb_movie_ids.json")),
    ]);
    start(&mut h, with_tmdb(maze(&a), &b));
    wait_for("TMDB's IDs", || {
        row(&h, SHOW).is_some_and(|r| r.tmdb == Some(401)) && row(&h, film).is_some_and(|r| r.tmdb == Some(501))
    });
    assert_eq!(b.requests().len(), 4, "{:?}", b.requests());
    assert_eq!(a.requests().len(), asked, "no more of TVMaze");
}

/// One title's rows, in 3 shards, and its cache row owing a fan-out.
fn owing(h: &Harness, rows: i64) -> (Arc<Media>, Db, Vec<Connection>) {
    let k = 42;
    let shards: Vec<Connection> =
        store::shard_paths(&h.main).iter().take(3).map(|p| db::open_shard(p).unwrap()).collect();
    for (s, conn) in shards.iter().enumerate() {
        for n in 1..=rows {
            conn.execute(
                "insert into media_release (release_id, kind, title_key, quality) values (?, 1, ?, 0)",
                rusqlite::params![n * 10 + s as i64, k],
            )
            .unwrap();
        }
    }
    let cache = media_store::open(&media_store::path_for(&h.main)).unwrap();
    let mut t =
        media_store::TitleRow { key: k, kind: 1, title: "x".into(), state: 1, checked_mask: 1, ..Default::default() };
    t.ids.tvdb = Some(3001);
    media_store::put(&cache, &t).unwrap();
    let media = Media::new(&h.main);
    media.with_second_pass(Duration::from_secs(1));
    media.set_config(&config(MediaConfig {
        tvmaze: atlas::config::TvmazeConfig {
            enabled: true,
            requests_per_minute: 1,
            base_url: Some("http://127.0.0.1:1".into()),
        },
        ..MediaConfig::default()
    }));
    let db = shared_db(db::open_at(&h.main).unwrap());
    (media, db, shards)
}

fn count(conn: &Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |r| r.get(0)).unwrap()
}

fn fan_done(h: &Harness) -> bool {
    let cache = media_store::open(&media_store::path_for(&h.main)).unwrap();
    media_store::get(&cache, 42).unwrap().is_some_and(|t| t.fanout_done)
}

#[test]
fn every_row_of_a_title_gets_its_ids_in_every_shard() {
    let h = fresh();
    let (media, db, shards) = owing(&h, 5);
    let _fan = fan_out(&media, &db);
    wait_for("both passes", || fan_done(&h));
    for conn in &shards {
        assert_eq!(count(conn, "select count(*) from media_release where tvdb_id = 3001"), 5);
    }
}

#[test]
fn a_big_title_goes_in_chunks() {
    let h = fresh();
    let (media, db, shards) = owing(&h, 2_500);
    let _fan = fan_out(&media, &db);
    wait_for("both passes", || fan_done(&h));
    assert_eq!(count(&shards[0], "select count(*) from media_release where tvdb_id = 3001"), 2_500);
    assert!(media.fanout_jobs.load(Ordering::Relaxed) >= 3 * 3 * 2);
}

#[test]
fn a_row_that_slips_past_pass_one_is_caught_by_pass_two() {
    let h = fresh();
    let (media, db, shards) = owing(&h, 1);
    let _fan = fan_out(&media, &db);
    wait_for("pass one", || count(&shards[0], "select count(*) from media_release where tvdb_id = 3001") == 1);
    shards[1]
        .execute("insert into media_release (release_id, kind, title_key, quality) values (99, 1, 42, 0)", [])
        .unwrap();
    wait_for("pass two", || fan_done(&h));
    assert_eq!(count(&shards[1], "select tvdb_id from media_release where release_id = 99"), 3001);
}

#[test]
fn it_stops_when_asked_and_holds_no_writer() {
    let h = fresh();
    let (media, db, shards) = owing(&h, 1);
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(atlas::media::run(media.clone(), db.clone(), || true));
    drop(db);
    assert!(!fan_done(&h));
    assert_eq!(count(&shards[0], "select count(*) from media_release where tvdb_id is not null"), 0);

    let (media, db) = (Media::new(&h.main), shared_db(db::open_at(&h.main).unwrap()));
    media.with_second_pass(Duration::from_millis(100));
    media.set_config(&config(maze_closed()));
    let _fan = fan_out(&media, &db);
    wait_for("a restart to finish it", || fan_done(&h));
    assert_eq!(count(&shards[0], "select count(*) from media_release where tvdb_id = 3001"), 1);
}

fn maze_closed() -> MediaConfig {
    let mut c = MediaConfig::default();
    c.tvmaze.enabled = true;
    c.tvmaze.base_url = Some("http://127.0.0.1:1".into());
    c
}

#[test]
fn an_unchanged_row_is_not_written() {
    let h = fresh();
    let conn = h.shard();
    conn.execute("insert into media_release (release_id, kind, title_key, quality) values (5, 1, 7, 0)", []).unwrap();
    let update = |tvdb: i64| {
        conn.execute(
            fanout::UPDATE,
            rusqlite::params![7, 0, 10, tvdb, None::<i64>, None::<i64>, None::<i64>, None::<i64>, None::<i64>],
        )
        .unwrap()
    };
    assert_eq!(update(9), 1);
    assert_eq!(update(9), 0);
}

#[test]
fn the_update_uses_its_index() {
    let h = fresh();
    let conn = h.shard();
    let mut stmt = conn.prepare(&format!("explain query plan {}", fanout::UPDATE)).unwrap();
    let params = rusqlite::params![7, 0, 10, 1, 1, 1, 1, 1, 1];
    let plan: Vec<String> = stmt.query_map(params, |r| r.get(3)).unwrap().map(Result::unwrap).collect();
    assert!(plan.join("\n").contains("media_release_title"), "{plan:?}");
}
