//! What a title is: its overview, cast, dates, ratings and poster, for the release page.
//!
//! Fetched from TMDB and OMDb when someone opens a release, and in the background for every title with an id, and kept
//! in `atlas.details.db`, soo a page costs a request only the first time, or once what's kept is old.

pub(crate) mod native;
#[cfg(test)]
mod native_tests;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use crate::config::{MediaConfig, OmdbConfig, TmdbConfig, TraktConfig, TvdbConfig};

use super::http::{Client, Reply, encode, now_secs};
use super::resolve::Ids;
use super::title::Kind;
use super::tvdb::Tvdb;

const TMDB_BASE: &str = "https://api.themoviedb.org/3";
const IMAGE_BASE: &str = "https://image.tmdb.org/t/p/w342";
const TVDB_IMAGES: &str = "https://artworks.thetvdb.com";
const OMDB_BASE: &str = "https://www.omdbapi.com";
const TRAKT_BASE: &str = "https://api.trakt.tv";
const KEEP_FOR: i64 = 30 * 86_400;
/// A failed look is tried again this soon.
const RETRY_AFTER: i64 = 3_600;
const MAX_CAST: usize = 8;
/// Past this, the oldest posters go. Tens of thousands of titles at about 50 KB each fit.
/// How many new images between looks at the cache's size: a look reads the whole folder.
/// How long a finished pass over the titles rests.
const BACKFILL_REST: Duration = Duration::from_secs(6 * 3600);
/// Past a start, soo it doesn't add to what starting up does.
const BACKFILL_DELAY: Duration = Duration::from_secs(30);

pub(crate) const SCHEMA: &str = "
create table if not exists details (
    kind       INTEGER NOT NULL,
    tmdb_id    INTEGER NOT NULL,
    title      TEXT,
    overview   TEXT,
    genres     TEXT,
    runtime    INTEGER,
    status     TEXT,
    network    TEXT,
    cast_names TEXT,
    released   TEXT,
    bluray     TEXT,
    poster     TEXT,
    tmdb_score INTEGER,
    imdb_score INTEGER,
    rotten     INTEGER,
    metacritic INTEGER,
    trakt_score INTEGER,
    fetched_at INTEGER NOT NULL,
    primary key (kind, tmdb_id)
) without rowid;
create table if not exists details_lookups (
    lookup     TEXT PRIMARY KEY,
    tmdb_id    INTEGER,
    checked_at INTEGER NOT NULL
) without rowid;
";

/// Scores are out of 100, whatever scale the source uses.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Details {
    pub kind: i64,
    pub tmdb_id: i64,
    pub title: Option<String>,
    pub overview: Option<String>,
    pub genres: Option<String>,
    pub runtime: Option<i64>,
    pub status: Option<String>,
    pub network: Option<String>,
    pub cast: Option<String>,
    pub released: Option<String>,
    pub bluray: Option<String>,
    pub poster: Option<String>,
    pub backdrop: Option<String>,
    pub tmdb_score: Option<i64>,
    pub imdb_score: Option<i64>,
    pub rotten: Option<i64>,
    pub metacritic: Option<i64>,
    pub trakt_score: Option<i64>,
    pub fetched_at: i64,
}

fn text(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(String::from)
}

fn names(v: &Value, key: &str, limit: usize) -> Option<String> {
    let list: Vec<String> = v.get(key)?.as_array()?.iter().take(limit).filter_map(|item| text(item, "name")).collect();
    (!list.is_empty()).then(|| list.join(", "))
}

/// TMDB's own type for a physical release.
const PHYSICAL: i64 = 5;

/// The earliest physical release, the US one if there is one.
fn bluray_date(movie: &Value) -> Option<String> {
    let countries = movie.get("release_dates")?.get("results")?.as_array()?;
    let physical_in = |country: &Value| -> Option<String> {
        let mut dates: Vec<&str> = country
            .get("release_dates")?
            .as_array()?
            .iter()
            .filter(|d| d.get("type").and_then(Value::as_i64) == Some(PHYSICAL))
            .filter_map(|d| d.get("release_date").and_then(Value::as_str))
            .collect();
        dates.sort_unstable();
        dates.first().map(|d| d.chars().take(10).collect())
    };
    let us = countries.iter().find(|c| c.get("iso_3166_1").and_then(Value::as_str) == Some("US")).and_then(physical_in);
    us.or_else(|| countries.iter().filter_map(physical_in).min())
}

/// What TMDB says about a title.
fn from_tmdb(kind: i64, tmdb_id: i64, v: &Value, now: i64) -> Details {
    let is_tv = kind == Kind::Tv as i64;
    Details {
        kind,
        tmdb_id,
        title: text(v, if is_tv { "name" } else { "title" }),
        overview: text(v, "overview"),
        genres: names(v, "genres", MAX_CAST),
        runtime: if is_tv {
            v.get("episode_run_time").and_then(Value::as_array).and_then(|t| t.first()).and_then(Value::as_i64)
        } else {
            v.get("runtime").and_then(Value::as_i64)
        }
        .filter(|minutes| *minutes > 0),
        status: text(v, "status"),
        network: names(v, "networks", 3),
        cast: v.get("credits").and_then(|c| names(c, "cast", MAX_CAST)),
        released: text(v, if is_tv { "first_air_date" } else { "release_date" }),
        bluray: if is_tv { None } else { bluray_date(v) },
        poster: text(v, "poster_path").filter(|p| safe_image_path(p)),
        backdrop: text(v, "backdrop_path").filter(|p| safe_image_path(p)),
        tmdb_score: v
            .get("vote_average")
            .and_then(Value::as_f64)
            .filter(|_| v.get("vote_count").and_then(Value::as_i64).unwrap_or(0) > 0)
            .map(|score| (score * 10.0).round() as i64),
        fetched_at: now,
        ..Details::default()
    }
}

/// What TheTVDB says about a series, kept under its id made negative, as TMDB's ids are all above zero. TheTVDB's own
/// `score` is how much people follow a show, not how good it is, soo it isn't a rating here.
fn from_tvdb(tvdb_id: i64, v: &Value, images: &str, now: i64) -> Details {
    let cast: Vec<String> = v
        .get("characters")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|person| text(person, "personName"))
        .take(MAX_CAST)
        .collect();
    Details {
        kind: Kind::Tv as i64,
        tmdb_id: -tvdb_id,
        title: text(v, "name"),
        overview: text(v, "overview"),
        genres: names(v, "genres", MAX_CAST),
        runtime: v.get("averageRuntime").and_then(Value::as_i64).filter(|minutes| *minutes > 0),
        status: v.get("status").and_then(|status| text(status, "name")),
        network: v.get("originalNetwork").and_then(|network| text(network, "name")),
        cast: (!cast.is_empty()).then(|| cast.join(", ")),
        released: text(v, "firstAired"),
        poster: text(v, "image").filter(|url| safe_image_url(url, images)),
        fetched_at: now,
        ..Details::default()
    }
}

/// `7.5/10`, `93%`, `81/100`, out of 100.
fn score_of(value: &str) -> Option<i64> {
    let value = value.trim();
    if let Some(percent) = value.strip_suffix('%') {
        return percent.parse().ok();
    }
    let (score, scale) = value.split_once('/')?;
    let (score, scale): (f64, f64) = (score.parse().ok()?, scale.parse().ok()?);
    (scale > 0.0).then(|| (score * 100.0 / scale).round() as i64)
}

fn add_omdb(d: &mut Details, v: &Value) {
    let Some(ratings) = v.get("Ratings").and_then(Value::as_array) else { return };
    for rating in ratings {
        let score = rating.get("Value").and_then(Value::as_str).and_then(score_of);
        match rating.get("Source").and_then(Value::as_str) {
            Some("Internet Movie Database") => d.imdb_score = score,
            Some("Rotten Tomatoes") => d.rotten = score,
            Some("Metacritic") => d.metacritic = score,
            _ => {}
        }
    }
}

/// A ratings source failed: what's there is kept, and looked at again within the hour instead of in a month.
fn try_again_soon(details: &mut Details, now: i64) {
    details.fetched_at = now - KEEP_FOR + RETRY_AFTER;
}

fn trakt_score(v: &Value) -> Option<i64> {
    let votes = v.get("votes").and_then(Value::as_i64).unwrap_or(0);
    v.get("rating").and_then(Value::as_f64).filter(|_| votes > 0).map(|score| (score * 10.0).round() as i64)
}

/// TMDB's image paths are one segment. Anything else doesn't go in a URL of ours.
fn safe_image_path(path: &str) -> bool {
    let name = path.strip_prefix('/').unwrap_or_default();
    !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// TheTVDB gives whole addresses. Only its own image host goes in a request of ours.
fn safe_image_url(url: &str, images: &str) -> bool {
    url.strip_prefix(images).is_some_and(|rest| {
        rest.starts_with('/')
            && !rest.contains("..")
            && rest.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'/'))
    })
}

struct Sources {
    tvdb_config: TvdbConfig,
    tvdb: Option<Tvdb>,
    tmdb: TmdbConfig,
    omdb: OmdbConfig,
    trakt: TraktConfig,
    tmdb_client: Client,
    omdb_client: Client,
    trakt_client: Client,
}

/// One pace per process. Replaced if the config's keys or addresses change.

fn make_sources(cfg: &MediaConfig) -> Arc<Sources> {
    let tmdb_base = cfg.tmdb.base_url.as_deref().unwrap_or(TMDB_BASE);
    let omdb_base = cfg.omdb.base_url.as_deref().unwrap_or(OMDB_BASE);
    std::sync::Arc::new(Sources {
        tvdb_config: cfg.tvdb.clone(),
        tvdb: (!cfg.tvdb.api_key.is_empty()).then(|| Tvdb::new(&cfg.tvdb)),
        tmdb_client: Client::new("tmdb", tmdb_base, cfg.tmdb.requests_per_minute),
        omdb_client: Client::new("omdb", omdb_base, cfg.omdb.requests_per_minute),
        tmdb: cfg.tmdb.clone(),
        omdb: cfg.omdb.clone(),
        trakt: cfg.trakt.clone(),
        trakt_client: Client::new(
            "trakt",
            cfg.trakt.base_url.as_deref().unwrap_or(TRAKT_BASE),
            cfg.trakt.requests_per_minute,
        ),
    })
}

impl Sources {
    fn tmdb_base(&self) -> &str {
        self.tmdb.base_url.as_deref().unwrap_or(TMDB_BASE)
    }
}

pub(crate) fn lookup_key(kind: i64, ids: &Ids) -> String {
    format!("{kind}:{:?}:{:?}", ids.imdb, ids.tvdb)
}

/// The exact kept filename used by the legacy image cache, without opening a
/// cache database, inspecting files or fetching a provider.
pub(crate) fn kept_image_name(path: &str) -> Option<String> {
    if path.starts_with("http") {
        return Some(
            path.chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' }).collect(),
        );
    }
    safe_image_path(path).then(|| path.trim_start_matches('/').to_owned())
}

/// The most titles a pass looks at per shard, newest first: ids come from what's posted, soo there can be any number.

/// Whether the job can fetch at all.
fn backfill_on(cfg: &MediaConfig) -> bool {
    cfg.details_per_minute > 0 && !(cfg.tmdb.api_key.is_empty() && cfg.tvdb.api_key.is_empty())
}

/// What a cached image counts as against its folder's limit: its size in 4 KiB blocks, and never less than 16 KiB.
/// A tiny file still takes a whole block on disk, and the floor caps how many files a folder can hold at its
/// limit over 16 KiB, so a flood of tiny ones can't fill the disk or make every trim sort millions of names.
const MIN_CHARGE: u64 = 16 << 10;

fn charge(len: u64) -> u64 {
    len.div_ceil(4096).saturating_mul(4096).max(MIN_CHARGE)
}

pub(crate) fn trim_posters(dir: &Path, limit: u64) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut files: Vec<(std::time::SystemTime, u64, PathBuf)> = entries
        .filter_map(Result::ok)
        .filter_map(|e| {
            let meta = e.metadata().ok()?;
            Some((meta.modified().ok()?, charge(meta.len()), e.path()))
        })
        .collect();
    let mut total: u64 = files.iter().map(|(_, size, _)| size).sum();
    files.sort();
    for (_, size, path) in files {
        if total <= limit {
            break;
        }
        if std::fs::remove_file(path).is_ok() {
            total -= size;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_movie_reads_from_tmdb() {
        let body = json!({
            "title": "The Film", "overview": "A film.", "runtime": 136, "status": "Released",
            "release_date": "1999-03-30", "poster_path": "/abc.jpg", "vote_average": 8.2, "vote_count": 10,
            "genres": [{"name": "Action"}, {"name": "Sci-Fi"}],
            "credits": {"cast": [{"name": "A One"}, {"name": "B Two"}]},
            "release_dates": {"results": [
                {"iso_3166_1": "DE", "release_dates": [{"type": 5, "release_date": "1999-01-01T00:00:00.000Z"}]},
                {"iso_3166_1": "US", "release_dates": [
                    {"type": 3, "release_date": "1999-03-30T00:00:00.000Z"},
                    {"type": 5, "release_date": "1999-09-21T00:00:00.000Z"}
                ]}
            ]}
        });
        let d = from_tmdb(Kind::Movie as i64, 603, &body, 5);
        assert_eq!(d.title.as_deref(), Some("The Film"));
        assert_eq!(d.genres.as_deref(), Some("Action, Sci-Fi"));
        assert_eq!(d.cast.as_deref(), Some("A One, B Two"));
        assert_eq!(d.bluray.as_deref(), Some("1999-09-21"));
        assert_eq!(d.tmdb_score, Some(82));
        assert_eq!(d.runtime, Some(136));
    }

    #[test]
    fn a_show_reads_from_tmdb() {
        let body = json!({
            "name": "The Show", "first_air_date": "2011-04-17", "episode_run_time": [60],
            "networks": [{"name": "HBO"}], "vote_average": 0, "vote_count": 0
        });
        let d = from_tmdb(Kind::Tv as i64, 1, &body, 5);
        assert_eq!(d.title.as_deref(), Some("The Show"));
        assert_eq!(d.released.as_deref(), Some("2011-04-17"));
        assert_eq!(d.network.as_deref(), Some("HBO"));
        assert_eq!(d.runtime, Some(60));
        assert_eq!(d.tmdb_score, None, "no votes, no score");
    }

    #[test]
    fn omdb_scores_come_to_a_hundred() {
        assert_eq!(score_of("8.7/10"), Some(87));
        assert_eq!(score_of("88%"), Some(88));
        assert_eq!(score_of("73/100"), Some(73));
        assert_eq!(score_of("N/A"), None);
        let mut d = Details::default();
        add_omdb(
            &mut d,
            &json!({"Ratings": [
                {"Source": "Internet Movie Database", "Value": "8.7/10"},
                {"Source": "Rotten Tomatoes", "Value": "88%"},
                {"Source": "Metacritic", "Value": "73/100"}
            ]}),
        );
        assert_eq!((d.imdb_score, d.rotten, d.metacritic), (Some(87), Some(88), Some(73)));
    }

    #[test]
    fn trakt_scores_need_votes() {
        assert_eq!(trakt_score(&json!({"rating": 8.25, "votes": 3})), Some(83));
        assert_eq!(trakt_score(&json!({"rating": 0.0, "votes": 0})), None);
    }

    #[test]
    fn only_a_plain_file_name_can_be_a_poster() {
        assert!(safe_image_path("/abc-1_2.jpg"));
        for bad in ["abc.jpg", "/../x", "/a/b.jpg", "/", "", "/a?b.jpg"] {
            assert!(!safe_image_path(bad), "{bad}");
        }
    }

    #[test]
    fn the_oldest_posters_go_first() {
        let dir = tempfile::tempdir().unwrap();
        for (name, age) in [("old.jpg", 100), ("new.jpg", 1)] {
            let path = dir.path().join(name);
            std::fs::write(&path, [0u8; 10]).unwrap();
            let when = std::time::SystemTime::now() - Duration::from_secs(age);
            std::fs::File::options().write(true).open(&path).unwrap().set_modified(when).unwrap();
        }
        // room for one
        trim_posters(dir.path(), 2 * MIN_CHARGE - 1);
        assert!(!dir.path().join("old.jpg").exists());
        assert!(dir.path().join("new.jpg").exists());
    }

    #[test]
    fn tiny_files_count_as_whole_blocks_so_their_number_is_capped() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..40 {
            std::fs::write(dir.path().join(format!("{i}")), [i as u8; 9]).unwrap();
        }
        // 40 nine-byte files are far under 160 KiB by length, but only ten fit
        trim_posters(dir.path(), 10 * MIN_CHARGE);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 10);
        assert_eq!((charge(1), charge(20_000), charge(40_000)), (MIN_CHARGE, 20_480, 40_960));
    }
}
