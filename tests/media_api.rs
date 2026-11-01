//! ID searches, end to end.
//!
//! One test: it points ATLAS_HOME at a temp dir before any thread is spawned.

mod common;

use atlas::parser::Release;
use atlas::{api, db, store};
use common::stage::release_of;
use rusqlite::params;

const K: &str = "k";

fn release(name: &str, group: &str) -> Release {
    Release { group: group.into(), ..release_of(name, "x.bin", 2, 2) }
}

fn titles(body: &str) -> Vec<String> {
    body.split("<title>").skip(2).filter_map(|t| t.split("</title>").next()).map(String::from).collect()
}

fn query(q: &str) -> String {
    let r = api::handle("/api", &format!("{q}&apikey={K}"), "http://atlas:9090", K);
    assert_eq!(r.status, 200, "{q}: {}", r.body);
    r.body
}

fn sorted(q: &str) -> Vec<String> {
    let mut t = titles(&query(q));
    t.sort();
    t
}

struct Row(&'static str, &'static str, i64, i64, Option<i64>, Option<i64>, Option<i64>, Option<i64>, [Option<i64>; 4]);

#[test]
fn id_searches_end_to_end() {
    let home = tempfile::tempdir().unwrap();
    // SAFETY: only test in this binary, set before any thread is spawned
    unsafe {
        std::env::set_var("ATLAS_HOME", home.path());
        std::env::set_var("ATLAS_NO_KEYRING", "1");
    }
    db::create_db().unwrap();
    let main = home.path().join("atlas.db");
    let show = [Some(3001), Some(101), Some(100001), Some(401)];
    let movie = [None, None, Some(200002), Some(501)];
    let (ep, multi, pack, daily, next, film, hidden) = (
        "Some.Show.S01E02.720p.HDTV.x264-GRP",
        "Some.Show.S01E01-E03.720p.HDTV.x264-GRP",
        "Some.Show.S01.720p.HDTV.x264-GRP",
        "Some.Show.2025.11.10.720p.HDTV.x264-GRP",
        "Some.Show.S02E01.480p.HDTV.x264-GRP",
        "Some.Movie.2019.1080p.BluRay.x264-GRP",
        "0123456789abcdef0123456789abcdef",
    );
    let rows = [
        Row(ep, "alt.binaries.g0", 5040, 1, Some(1), Some(2), None, None, show),
        Row(multi, "alt.binaries.g1", 5040, 1, Some(1), Some(1), Some(3), None, show),
        Row(pack, "alt.binaries.g2", 5040, 1, Some(1), None, None, None, show),
        Row(daily, "alt.binaries.g3", 5030, 1, Some(2025), Some(1110), None, Some(20251110), show),
        Row(next, "alt.binaries.g4", 5030, 1, Some(2), Some(1), None, None, show),
        Row(film, "alt.binaries.g5", 2040, 2, None, None, None, None, movie),
        Row(hidden, "alt.binaries.g6", 5040, 1, Some(1), Some(2), None, None, show),
    ];
    for Row(name, group, category, kind, season, episode, last, air, [tvdb, tvmaze, imdb, tmdb]) in rows {
        store::save(&main, &[release(name, group)]).unwrap();
        let shard = db::open_shard(&store::shard_path(&main, store::shard_of(group))).unwrap();
        let id: i64 = shard.query_row("select id from releases where name = ?", [name], |r| r.get(0)).unwrap();
        shard
            .execute("insert into release_category (release_id, category) values (?, ?)", params![id, category])
            .unwrap();
        shard
            .execute(
                "insert into media_release (release_id, kind, title_key, season, episode, episode_last, air_date, quality,
                     tvdb_id, tvmaze_id, imdb_id, tmdb_id)
                 values (?, ?, 5, ?, ?, ?, ?, 0, ?, ?, ?, ?)",
                params![id, kind, season, episode, last, air, tvdb, tvmaze, imdb, tmdb],
            )
            .unwrap();
    }

    // nothing on
    let config = home.path().join("config.json");
    std::fs::write(&config, "{}").unwrap();
    assert!(!query("t=caps").contains("tvdbid"));
    assert!(titles(&query("t=tvsearch&tvdbid=3001")).is_empty());
    assert!(!titles(&query("t=tvsearch&tvdbid=3001&q=Some+Show")).is_empty(), "by name");
    assert!(titles(&query("t=movie&imdbid=0200002")).is_empty());
    assert!(!query("t=search&q=Some+Show").contains("tvdbid"), "off, so hidden");
    assert!(query("t=search&q=Some+Show").contains(r#"name="season""#));

    // TVMaze and TMDB on
    let on = serde_json::json!({"media": {"tvmaze": {"enabled": true, "base_url": "http://127.0.0.1:1"}, "tmdb": {"api_key": "x", "base_url": "http://127.0.0.1:1"}}});
    std::fs::write(&config, on.to_string()).unwrap();
    let caps = query("t=caps");
    assert!(caps.contains("tvdbid"), "{caps}");
    assert!(
        caps.contains(r#"<movie-search available="yes" supportedParams="q,cat,limit,offset,imdbid,tmdbid"/>"#),
        "{caps}"
    );

    let all = titles(&query("t=tvsearch&tvdbid=3001"));
    let mut shows = vec![ep, multi, pack, daily, next];
    shows.sort();
    let mut got = all.clone();
    got.sort();
    assert_eq!(got, shows);

    assert_eq!(sorted("t=tvsearch&tvdbid=3001&season=1&ep=2"), [multi, ep]);
    assert_eq!(sorted("t=tvsearch&tvdbid=3001&season=1"), [pack, multi, ep]);
    assert_eq!(sorted("t=tvsearch&tvdbid=3001&season=2025&ep=11/10"), [daily]);
    assert_eq!(sorted("t=tvsearch&tvdbid=3001&imdbid=tt0100001&tmdbid=401&cat=5030"), [daily, next]);
    assert!(titles(&query("t=tvsearch&tmdbid=501")).is_empty(), "a movie's id");

    let body = query("t=movie&imdbid=tt0200002&cat=2000");
    assert_eq!(titles(&body), [film]);
    assert!(body.contains(r#"name="imdb" value="0200002""#), "{body}");
    assert_eq!(titles(&query("t=movie&tmdbid=501")), [film]);
    assert!(titles(&query("t=tvsearch&tvdbid=9999")).is_empty());
    assert!(!titles(&query("t=tvsearch&tvdbid=9999&q=Some+Show")).is_empty(), "the fallback");
    assert!(titles(&query("t=tvsearch&traktid=601")).is_empty(), "Trakt is off");
    assert_eq!(titles(&query("t=tvsearch&tvdbid=3001&limit=1&offset=1")), [all[1].clone()]);

    let body = query("t=tvsearch&tvdbid=3001&season=2025&ep=11/10");
    assert!(body.contains(r#"name="tvairdate" value="2025-11-10""#), "{body}");
    assert!(body.contains(r#"name="tvdbid" value="3001""#), "{body}");
    assert!(body.contains(r#"name="tvmazeid" value="101""#), "{body}");
    assert!(!all.iter().any(|t| t == hidden), "a hidden release never shows");
}
