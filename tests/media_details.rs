//! What the release page shows about a title, from fake TMDB and OMDb servers.
//!
//! One test: it points ATLAS_HOME at a temp dir before any thread is spawned.

mod common;

use atlas::config::{MediaConfig, OmdbConfig, TmdbConfig, TvdbConfig};
use atlas::db;
use atlas::media::details::{for_title, poster};
use atlas::media::resolve::Ids;
use common::http::{FixtureServer, route};

const MOVIE: i64 = 2;

#[test]
fn a_title_is_fetched_once_and_kept() {
    let home = tempfile::tempdir().unwrap();
    // SAFETY: only test in this binary, set before any thread is spawned
    unsafe {
        std::env::set_var("ATLAS_HOME", home.path());
        std::env::set_var("ATLAS_NO_KEYRING", "1");
    }
    db::create_db().unwrap();

    let film = r#"{"title":"The Film","overview":"A film.","poster_path":"/p.jpg","vote_average":8.0,"vote_count":3,
        "release_date":"1999-03-30","release_dates":{"results":[{"iso_3166_1":"US","release_dates":[{"type":5,"release_date":"1999-09-21T00:00:00Z"}]}]}}"#;
    let ratings = r#"{"Ratings":[{"Source":"Rotten Tomatoes","Value":"88%"}]}"#;
    let server = FixtureServer::start(vec![
        route("/movie/603", film.into()),
        route("/find/tt0133093", r#"{"movie_results":[{"id":603}],"tv_results":[]}"#.into()),
        route("/omdb/", ratings.into()),
        route("/img/p.jpg", "not a jpeg".into()),
    ]);
    let base = server.base.clone();
    let cfg = MediaConfig {
        tmdb: TmdbConfig {
            api_key: "key".into(),
            requests_per_minute: 3000,
            base_url: Some(base.clone()),
            image_base_url: Some(format!("{base}/img")),
        },
        omdb: OmdbConfig { api_key: "key".into(), requests_per_minute: 600, base_url: Some(format!("{base}/omdb")) },
        ..MediaConfig::default()
    };

    let ids = Ids { imdb: Some(133093), ..Ids::default() };
    let d = for_title(&cfg, MOVIE, &ids).unwrap();
    assert_eq!(d.tmdb_id, 603, "found through its IMDb id");
    assert_eq!(d.title.as_deref(), Some("The Film"));
    assert_eq!(d.bluray.as_deref(), Some("1999-09-21"));
    assert_eq!(d.rotten, Some(88));

    let asked = server.requests().len();
    assert_eq!(for_title(&cfg, MOVIE, &ids).unwrap().title.as_deref(), Some("The Film"));
    assert_eq!(server.requests().len(), asked, "kept, not asked again");

    // with the provider off, what's kept still shows
    let off = MediaConfig::default();
    assert_eq!(for_title(&off, MOVIE, &Ids { tmdb: Some(603), ..Ids::default() }).unwrap().rotten, Some(88));
    assert_eq!(for_title(&off, MOVIE, &Ids { tmdb: Some(1), ..Ids::default() }), None);

    // not a JPEG: not kept, not served
    assert_eq!(poster(MOVIE, 603, &cfg), None);
    assert!(!home.path().join("posters/p.jpg").exists());

    a_show_only_thetvdb_knows();
}

/// With no TMDB key, or nothing there, TheTVDB's own record shows.
fn a_show_only_thetvdb_knows() {
    const TV: i64 = 1;
    let series = |base: &str| {
        format!(
            r#"{{"data":{{"name":"The Show","overview":"A show.","averageRuntime":42,"status":{{"name":"Continuing"}},
            "originalNetwork":{{"name":"HBO"}},"firstAired":"2020-01-02","image":"{base}/img/s.jpg","score":99999,
            "genres":[{{"name":"Drama"}}],"characters":[{{"personName":"An Actor"}}]}}}}"#
        )
    };
    let server = FixtureServer::start(vec![]);
    server.set_routes(vec![
        route("/login", r#"{"data":{"token":"t"}}"#.into()),
        route("/series/77/extended", series(&server.base)),
        route("/img/s.jpg", String::from_utf8_lossy(&[0xff, 0xd8, 0xff, b'x']).into_owned()),
    ]);
    let cfg = MediaConfig {
        tvdb: TvdbConfig {
            api_key: "key".into(),
            pin: String::new(),
            requests_per_minute: 600,
            base_url: Some(server.base.clone()),
        },
        ..MediaConfig::default()
    };
    let ids = Ids { tvdb: Some(77), ..Ids::default() };
    let d = for_title(&cfg, TV, &ids).unwrap();
    assert_eq!(d.tmdb_id, -77);
    assert_eq!(d.title.as_deref(), Some("The Show"));
    assert_eq!(d.status.as_deref(), Some("Continuing"));
    assert_eq!(d.network.as_deref(), Some("HBO"));
    assert_eq!(d.cast.as_deref(), Some("An Actor"));
    assert_eq!(d.runtime, Some(42));
    assert_eq!(d.tmdb_score, None, "its score isn't a rating");

    let asked = server.requests().len();
    assert_eq!(for_title(&cfg, TV, &ids).unwrap().title.as_deref(), Some("The Show"));
    assert_eq!(server.requests().len(), asked, "kept, not asked again");
    assert_eq!(atlas::media::details::kept_for(std::iter::once((1, TV, &ids)))[&1].title.as_deref(), Some("The Show"));

    // nothing from anywhere else is fetched
    let bad = FixtureServer::start(vec![]);
    bad.set_routes(vec![
        route("/login", r#"{"data":{"token":"t"}}"#.into()),
        route("/series/78/extended", series("https://elsewhere.example")),
    ]);
    // gone from TMDB, with TheTVDB still having it
    let gone = FixtureServer::start(vec![]);
    gone.set_routes(vec![
        route("/login", r#"{"data":{"token":"t"}}"#.into()),
        route("/series/79/extended", series("https://elsewhere.example")),
        common::http::status("/tv/5", 404),
    ]);
    let both = MediaConfig {
        tvdb: TvdbConfig { base_url: Some(gone.base.clone()), ..cfg.tvdb.clone() },
        tmdb: TmdbConfig {
            api_key: "key".into(),
            requests_per_minute: 3000,
            base_url: Some(gone.base.clone()),
            image_base_url: None,
        },
        ..cfg.clone()
    };
    let d = for_title(&both, TV, &Ids { tmdb: Some(5), tvdb: Some(79), ..Ids::default() }).unwrap();
    assert_eq!(d.tmdb_id, -79);

    let other =
        MediaConfig { tvdb: TvdbConfig { base_url: Some(bad.base.clone()), ..cfg.tvdb.clone() }, ..cfg.clone() };
    let d = for_title(&other, TV, &Ids { tvdb: Some(78), ..Ids::default() }).unwrap();
    assert_eq!(d.poster, None);
}
