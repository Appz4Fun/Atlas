//! What the release page shows about a title, from fake TMDB and OMDb servers.
//!
//! One test: it points ATLAS_HOME at a temp dir before any thread is spawned.

mod common;

use atlas::config::{MediaConfig, OmdbConfig, TmdbConfig};
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
}
