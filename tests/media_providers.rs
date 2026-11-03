//! The providers, each against a loopback server answering from fixtures.

mod common;

use std::time::{Duration, Instant};

use atlas::config::{TmdbConfig, TraktConfig, TvdbConfig, TvmazeConfig};
use atlas::media::http::Client;
use atlas::media::resolve::{Answer, Ids, Provider, TitleQuery, Trouble};
use atlas::media::title::Kind;
use atlas::media::tmdb::Tmdb;
use atlas::media::trakt::Trakt;
use atlas::media::tvdb::Tvdb;
use atlas::media::tvmaze::Tvmaze;
use common::http::*;

const FAST: i64 = 600_000;

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

fn maze(s: &FixtureServer, rate: i64) -> Tvmaze {
    Tvmaze::new(&TvmazeConfig { enabled: true, requests_per_minute: rate, base_url: Some(s.base.clone()) })
}

fn tmdb(s: &FixtureServer, rate: i64) -> Tmdb {
    Tmdb::new(&TmdbConfig {
        api_key: "SECRETK".into(),
        requests_per_minute: rate,
        base_url: Some(s.base.clone()),
        image_base_url: None,
    })
}

fn trakt(s: &FixtureServer) -> Trakt {
    Trakt::new(&TraktConfig { client_id: "ID".into(), requests_per_minute: FAST, base_url: Some(s.base.clone()) })
}

fn tv(title: &str) -> TitleQuery<'_> {
    TitleQuery { kind: Kind::Tv, title, year: None, country: None }
}

fn movie(title: &str, year: Option<i32>) -> TitleQuery<'_> {
    TitleQuery { kind: Kind::Movie, title, year, country: None }
}

fn found(answer: Result<Answer, Trouble>) -> Ids {
    match answer {
        Ok(Answer::Found { ids, .. }) => ids,
        other => panic!("not found: {other:?}"),
    }
}

fn near(t: i64, secs: i64) {
    assert!((t - (now() + secs)).abs() <= 5, "{t} isnt about now + {secs}");
}

fn single() -> Route {
    route("/singlesearch/shows", fixture("tvmaze_show.json"))
}

fn search() -> Route {
    route("/search/shows", fixture("tvmaze_search.json"))
}

#[test]
fn a_hit_takes_one_call() {
    let s = FixtureServer::start(vec![single()]);
    let ids = found(maze(&s, FAST).find(&tv("Some Show")));
    assert_eq!(ids.tvmaze, Some(101));
    assert_eq!(ids.tvdb, Some(3001));
    assert_eq!(ids.tvrage, Some(2001));
    assert_eq!(ids.imdb, Some(100001));
    let requests = s.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0], "/singlesearch/shows?q=Some%20Show&embed=akas");
}

#[test]
fn an_aka_matches() {
    let s = FixtureServer::start(vec![single()]);
    assert_eq!(found(maze(&s, FAST).find(&tv("Some Alias"))).tvmaze, Some(101));
}

#[test]
fn a_country_mismatch_falls_to_search() {
    let s = FixtureServer::start(vec![single(), search()]);
    let q = TitleQuery { country: Some("UK"), ..tv("Some Show") };
    assert_eq!(found(maze(&s, FAST).find(&q)).tvmaze, Some(103));
    assert_eq!(s.requests().len(), 2);
}

#[test]
fn a_year_mismatch_falls_to_search() {
    let s = FixtureServer::start(vec![single()]);
    let q = TitleQuery { year: Some(1990), ..tv("Some Show") };
    assert_eq!(maze(&s, FAST).find(&q), Ok(Answer::NotFound));
    assert_eq!(s.requests().len(), 2);
    assert!(s.requests()[1].starts_with("/search/shows"));
}

#[test]
fn a_404_falls_to_search_then_misses_in_two_calls() {
    let s = FixtureServer::start(vec![]);
    assert_eq!(maze(&s, FAST).find(&tv("Some Show")), Ok(Answer::NotFound));
    assert_eq!(s.requests().len(), 2);
}

#[test]
fn a_wrong_show_is_rejected() {
    let s = FixtureServer::start(vec![single(), search()]);
    assert_eq!(maze(&s, FAST).find(&tv("Another Thing")), Ok(Answer::NotFound));
    assert_eq!(s.requests().len(), 2);
}

#[test]
fn a_429_pauses_for_retry_after() {
    let limited = Route { headers: vec![("Retry-After", "120".into())], ..status("/singlesearch/shows", 429) };
    let s = FixtureServer::start(vec![limited]);
    let p = maze(&s, FAST);
    let Err(Trouble::Paused(t)) = p.find(&tv("Some Show")) else { panic!("not paused") };
    near(t, 120);
    assert!(matches!(p.find(&tv("Some Show")), Err(Trouble::Paused(_))));
    assert_eq!(s.requests().len(), 1, "no request while paused");
}

#[test]
fn a_500_pauses_and_doubles() {
    let s = FixtureServer::start(vec![status("/singlesearch/shows", 500)]);
    let p = maze(&s, FAST);
    let Err(Trouble::Paused(t)) = p.find(&tv("Some Show")) else { panic!("not paused") };
    near(t, 60);
    p.client().advance();
    let Err(Trouble::Paused(t)) = p.find(&tv("Some Show")) else { panic!("not paused") };
    near(t, 120);
    p.client().advance();
    s.set_routes(vec![single()]);
    found(p.find(&tv("Some Show")));
    s.set_routes(vec![status("/singlesearch/shows", 500)]);
    let Err(Trouble::Paused(t)) = p.find(&tv("Some Show")) else { panic!("not paused") };
    near(t, 60);
}

#[test]
fn a_connection_refused_pauses() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let cfg =
        TvmazeConfig { enabled: true, requests_per_minute: FAST, base_url: Some(format!("http://127.0.0.1:{port}")) };
    assert!(matches!(Tvmaze::new(&cfg).find(&tv("Some Show")), Err(Trouble::Paused(_))));
}

#[test]
fn bad_json_is_bad_not_a_miss() {
    let s = FixtureServer::start(vec![route("/singlesearch/shows", "<html>".into())]);
    assert!(matches!(maze(&s, FAST).find(&tv("Some Show")), Err(Trouble::Bad(_))));
}

#[test]
fn the_bucket_paces_requests() {
    let s = FixtureServer::start(vec![single()]);
    let p = maze(&s, 120);
    let start = Instant::now();
    for _ in 0..5 {
        found(p.find(&tv("Some Show")));
    }
    assert!(start.elapsed() >= Duration::from_millis(1_900), "{:?}", start.elapsed());
}

#[test]
fn a_two_request_lookup_finishes_at_the_lowest_rate() {
    let s = FixtureServer::start(vec![
        route("/search/tv", fixture("tmdb_search_tv.json")),
        route("/tv/401/external_ids", fixture("tmdb_tv_ids.json")),
    ]);
    let p = tmdb(&s, 1);
    assert!(matches!(p.find(&tv("Some Show")), Err(Trouble::Paused(_))));
    p.client().advance();
    assert_eq!(found(p.find(&tv("Some Show"))).tvdb, Some(3001));
    let requests = s.requests();
    assert_eq!(requests.len(), 2, "{requests:?}");
    assert!(requests[1].starts_with("/tv/401/external_ids"));

    let s = FixtureServer::start(vec![search()]);
    let p = maze(&s, 1);
    assert!(matches!(p.find(&tv("Some Show")), Err(Trouble::Paused(_))));
    p.client().advance();
    assert_eq!(found(p.find(&tv("Some Show"))).tvmaze, Some(103));
    let requests = s.requests();
    assert_eq!(requests.len(), 2, "{requests:?}");
    assert!(requests[1].starts_with("/search/shows"));
}

#[test]
fn an_oversized_name_is_no_candidate() {
    let name = "Some Show ".repeat(100_000);
    let show = serde_json::json!({"id": 101, "name": name, "premiered": "2015-03-02", "_embedded": {"akas": [{"name": "Some Alias"}]}});
    let s = FixtureServer::start(vec![route("/singlesearch/shows", show.to_string())]);
    assert_eq!(maze(&s, FAST).find(&tv("Some Show")), Ok(Answer::NotFound));
    let Ok(Answer::Found { title, .. }) = maze(&s, FAST).find(&tv("Some Alias")) else { panic!("the alias") };
    assert!(title.len() <= 255, "{}", title.len());
}

#[test]
fn a_redirect_isnt_followed() {
    let elsewhere = FixtureServer::start(vec![single()]);
    let redirect = Route {
        headers: vec![("Location", format!("{}/singlesearch/shows", elsewhere.base))],
        ..status("/singlesearch/shows", 302)
    };
    let s = FixtureServer::start(vec![redirect]);
    assert!(matches!(maze(&s, FAST).find(&tv("Some Show")), Err(Trouble::Bad(_))));
    assert!(elsewhere.requests().is_empty());
}

#[test]
fn no_token_within_two_seconds_gives_paused() {
    let s = FixtureServer::start(vec![single()]);
    let p = maze(&s, 1);
    found(p.find(&tv("Some Show")));
    let start = Instant::now();
    assert!(matches!(p.find(&tv("Some Show")), Err(Trouble::Paused(_))));
    assert!(start.elapsed() < Duration::from_secs(5));
    assert_eq!(s.requests().len(), 1);
}

#[test]
fn a_huge_body_is_refused() {
    let huge = format!("[{}]", " ".repeat(3 << 20));
    let s = FixtureServer::start(vec![route("/singlesearch/shows", huge)]);
    assert!(matches!(maze(&s, FAST).find(&tv("Some Show")), Err(Trouble::Bad(_))));
}

#[test]
fn tmdb_tv_takes_two_calls_and_finds_external_ids() {
    let s = FixtureServer::start(vec![
        route("/search/tv", fixture("tmdb_search_tv.json")),
        route("/tv/401/external_ids", fixture("tmdb_tv_ids.json")),
    ]);
    let ids = found(tmdb(&s, FAST).find(&tv("Some Show")));
    assert_eq!((ids.tmdb, ids.imdb, ids.tvdb, ids.tvrage), (Some(401), Some(100001), Some(3001), Some(2001)));
    assert_eq!(s.requests().len(), 2);
}

#[test]
fn tmdb_movie_passes_the_year_and_finds_imdb() {
    let s = FixtureServer::start(vec![
        route("/search/movie", fixture("tmdb_search_movie.json")),
        route("/movie/501/external_ids", fixture("tmdb_movie_ids.json")),
    ]);
    let ids = found(tmdb(&s, FAST).find(&movie("Some Movie", Some(2019))));
    assert_eq!((ids.tmdb, ids.imdb), (Some(501), Some(200002)));
    let first = &s.requests()[0];
    for want in ["year=2019", "include_adult=false", "api_key=SECRETK"] {
        assert!(first.contains(want), "{first}");
    }
}

#[test]
fn tmdb_miss_takes_one_call() {
    let s = FixtureServer::start(vec![route("/search/tv", fixture("tmdb_empty.json"))]);
    assert_eq!(tmdb(&s, FAST).find(&tv("Some Show")), Ok(Answer::NotFound));
    assert_eq!(s.requests().len(), 1);
}

#[test]
fn tmdb_picks_the_best_of_many() {
    let results = serde_json::json!({"results": [
        {"id": 1, "name": "Some Show Again", "first_air_date": "2015-01-01"},
        {"id": 2, "name": "Some Shows", "first_air_date": "2015-01-01"},
        {"id": 3, "name": "Some Show", "first_air_date": "2015-01-01"},
    ]});
    let s = FixtureServer::start(vec![
        route("/search/tv", results.to_string()),
        route("/tv/3/external_ids", fixture("tmdb_tv_ids.json")),
    ]);
    assert_eq!(found(tmdb(&s, FAST).find(&tv("Some Show"))).tmdb, Some(3));
}

#[test]
fn tmdb_null_ids_are_none() {
    let s = FixtureServer::start(vec![
        route("/search/tv", fixture("tmdb_search_tv.json")),
        route("/tv/401/external_ids", r#"{"imdb_id":null,"tvdb_id":null}"#.into()),
    ]);
    let ids = found(tmdb(&s, FAST).find(&tv("Some Show")));
    assert_eq!((ids.tmdb, ids.imdb, ids.tvdb), (Some(401), None, None));
}

#[test]
fn tmdb_401_is_bad_key_and_records_nothing() {
    let s = FixtureServer::start(vec![status("/search/tv", 401)]);
    let Err(Trouble::BadKey(t)) = tmdb(&s, FAST).find(&tv("Some Show")) else { panic!("not a bad key") };
    near(t, 3_600);
}

#[test]
fn tmdb_key_never_reaches_the_log() {
    let mut answers = Vec::new();
    for route in [
        status("/search/tv", 401),
        status("/search/tv", 500),
        status("/search/tv", 418),
        self::route("/search/tv", "x".into()),
    ] {
        let s = FixtureServer::start(vec![route]);
        answers.push(tmdb(&s, FAST).find(&tv("Some Show")));
    }
    for answer in answers {
        let text = format!("{answer:?}");
        assert!(answer.is_err() && !text.contains("SECRETK") && !text.contains("api_key"), "{text}");
    }
}

#[test]
fn trakt_sends_its_headers() {
    let s = FixtureServer::start(vec![route("/search/show", fixture("trakt_show.json"))]);
    let ids = found(trakt(&s).find(&tv("Some Show")));
    assert_eq!(
        (ids.trakt, ids.tvdb, ids.imdb, ids.tmdb, ids.tvrage),
        (Some(601), Some(3001), Some(100001), Some(401), Some(2001))
    );
    let headers = s.headers_of(0);
    let of = |name: &str| headers.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str());
    assert_eq!(of("trakt-api-version"), Some("2"));
    assert_eq!(of("trakt-api-key"), Some("ID"));
}

#[test]
fn trakt_movie_finds_all_its_ids() {
    let s = FixtureServer::start(vec![route("/search/movie", fixture("trakt_movie.json"))]);
    let ids = found(trakt(&s).find(&movie("Some Movie", Some(2019))));
    assert_eq!((ids.trakt, ids.imdb, ids.tmdb), (Some(602), Some(200002), Some(501)));
}

#[test]
fn trakt_miss_is_an_empty_list() {
    let s = FixtureServer::start(vec![route("/search/show", "[]".into())]);
    assert_eq!(trakt(&s).find(&tv("Some Show")), Ok(Answer::NotFound));
}

#[test]
fn a_client_is_a_public_type() {
    // the two hooks a test needs are reachable, and nothing else about a client is
    let _ = Client::new("x", "http://127.0.0.1:1", 1);
}

fn tvdb(s: &FixtureServer) -> Tvdb {
    Tvdb::new(&TvdbConfig {
        api_key: "SECRETK".into(),
        pin: String::new(),
        requests_per_minute: FAST,
        base_url: Some(s.base.clone()),
    })
}

fn tvdb_routes() -> Vec<Route> {
    vec![route("/login", fixture("tvdb_login.json")), route("/search", fixture("tvdb_search.json"))]
}

#[test]
fn tvdb_logs_in_once_and_finds_its_ids() {
    let s = FixtureServer::start(tvdb_routes());
    let p = tvdb(&s);
    let ids = found(p.find(&tv("Some Show")));
    assert_eq!((ids.tvdb, ids.imdb, ids.tmdb, ids.tvmaze), (Some(3001), Some(100001), Some(401), Some(101)));
    found(p.find(&tv("Some Show")));
    let requests = s.requests();
    assert_eq!(requests.iter().filter(|r| r.starts_with("/login")).count(), 1, "{requests:?}");
    assert!(requests[1].contains("query=Some%20Show") && requests[1].contains("type=series"), "{requests:?}");
    let headers = s.headers_of(1);
    let auth = headers.iter().find(|(n, _)| n.eq_ignore_ascii_case("authorization")).map(|(_, v)| v.as_str());
    assert_eq!(auth, Some("Bearer T0KEN"));
}

#[test]
fn tvdb_matches_an_alias_a_country_and_a_year() {
    let s = FixtureServer::start(tvdb_routes());
    let p = tvdb(&s);
    assert_eq!(found(p.find(&tv("Some Alias"))).tvdb, Some(3001));
    let uk = TitleQuery { country: Some("UK"), ..tv("Some Show") };
    assert_eq!(found(p.find(&uk)).tvdb, Some(3003), "gbr is UK");
    let old = TitleQuery { year: Some(1990), ..tv("Some Show") };
    assert_eq!(p.find(&old), Ok(Answer::NotFound));
    assert_eq!(p.find(&tv("Another Thing")), Ok(Answer::NotFound));
}

#[test]
fn tvdb_asks_only_for_tv() {
    let s = FixtureServer::start(tvdb_routes());
    assert!(tvdb(&s).asks(Kind::Tv) && !tvdb(&s).asks(Kind::Movie));
}

#[test]
fn a_refused_tvdb_key_pauses_and_says_nothing_of_it() {
    let s = FixtureServer::start(vec![status("/login", 401)]);
    let answer = tvdb(&s).find(&tv("Some Show"));
    let Err(Trouble::BadKey(t)) = &answer else { panic!("{answer:?}") };
    near(*t, 3_600);
    assert!(!format!("{answer:?}").contains("SECRETK"));
}

#[test]
fn a_token_it_stops_taking_logs_in_again() {
    let s = FixtureServer::start(tvdb_routes());
    let p = tvdb(&s);
    found(p.find(&tv("Some Show")));
    s.set_routes(vec![route("/login", fixture("tvdb_login.json")), status("/search", 401)]);
    assert!(matches!(p.find(&tv("Some Show")), Err(Trouble::BadKey(_))));
    p.client().advance();
    s.set_routes(tvdb_routes());
    found(p.find(&tv("Some Show")));
    assert_eq!(s.requests().iter().filter(|r| r.starts_with("/login")).count(), 2);
}

#[test]
fn a_login_with_no_token_is_bad_not_a_miss() {
    let s = FixtureServer::start(vec![route("/login", r#"{"status":"failure"}"#.into())]);
    assert!(matches!(tvdb(&s).find(&tv("Some Show")), Err(Trouble::Bad(_))));
}

#[test]
fn an_empty_tvdb_search_is_a_miss() {
    let s = FixtureServer::start(vec![
        route("/login", fixture("tvdb_login.json")),
        route("/search", r#"{"data":[]}"#.into()),
    ]);
    assert_eq!(tvdb(&s).find(&tv("Some Show")), Ok(Answer::NotFound));
}

#[test]
fn a_translated_name_matches() {
    let item = serde_json::json!({"data": [{"name": "Originaltitel", "tvdb_id": "3001", "translations": {"eng": "Some Show", "fra": "Une Serie"}, "name_translated": "Another Name"}]});
    let s = FixtureServer::start(vec![route("/login", fixture("tvdb_login.json")), route("/search", item.to_string())]);
    let p = tvdb(&s);
    assert_eq!(found(p.find(&tv("Some Show"))).tvdb, Some(3001));
    assert_eq!(found(p.find(&tv("Une Serie"))).tvdb, Some(3001));
    assert_eq!(found(p.find(&tv("Another Name"))).tvdb, Some(3001));
}

#[test]
fn a_pin_goes_with_the_key_when_there_is_one() {
    let s = FixtureServer::start(tvdb_routes());
    found(tvdb(&s).find(&tv("Some Show")));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&s.body_of(0)).unwrap(),
        serde_json::json!({"apikey": "SECRETK"})
    );

    let s = FixtureServer::start(tvdb_routes());
    let p = Tvdb::new(&TvdbConfig {
        api_key: "SECRETK".into(),
        pin: "1234".into(),
        requests_per_minute: FAST,
        base_url: Some(s.base.clone()),
    });
    found(p.find(&tv("Some Show")));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&s.body_of(0)).unwrap(),
        serde_json::json!({"apikey": "SECRETK", "pin": "1234"})
    );
}
