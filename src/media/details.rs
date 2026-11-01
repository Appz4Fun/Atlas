//! What a title is: its overview, cast, dates, ratings and poster, for the release page.
//!
//! Fetched from TMDB and OMDb when someone opens a release, and kept in `atlas.details.db`, soo a page
//! costs a request only the first time, or once what's kept is old. Releases nobody opens cost nothing.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;

use crate::config::{MediaConfig, OmdbConfig, TmdbConfig, TraktConfig, TvdbConfig};

use super::http::{Client, Reply, encode, now_secs};
use super::resolve::Ids;
use super::title::Kind;
use super::tvdb::Tvdb;

const TMDB_BASE: &str = "https://api.themoviedb.org/3";
const IMAGE_BASE: &str = "https://image.tmdb.org/t/p/w342";
const BACKDROP_BASE: &str = "https://image.tmdb.org/t/p/w780";
const TVDB_IMAGES: &str = "https://artworks.thetvdb.com";
const OMDB_BASE: &str = "https://www.omdbapi.com";
const TRAKT_BASE: &str = "https://api.trakt.tv";
const KEEP_FOR: i64 = 30 * 86_400;
/// A failed look is tried again this soon.
const RETRY_AFTER: i64 = 3_600;
const MAX_CAST: usize = 8;
const MAX_POSTER: u64 = 8 << 20;
/// Past this, the oldest posters go.
const MAX_POSTER_CACHE: u64 = 512 << 20;

const SCHEMA: &str = "
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

pub fn path_for(main: &Path) -> PathBuf {
    let stem = main.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    main.with_file_name(format!("{stem}.details.db"))
}

fn open() -> rusqlite::Result<Connection> {
    let conn = crate::db::create_at(&path_for(&crate::paths::database()))?;
    conn.query_row("pragma journal_mode = wal", [], |_| Ok(()))?;
    conn.execute_batch(SCHEMA)?;
    let has_backdrop: bool =
        conn.query_row("select count(*) from pragma_table_info('details') where name = 'backdrop'", [], |r| r.get(0))?;
    if !has_backdrop {
        conn.execute_batch("alter table details add column backdrop TEXT")?;
    }
    Ok(conn)
}

fn read(conn: &Connection, kind: i64, tmdb_id: i64) -> rusqlite::Result<Option<Details>> {
    conn.query_row(
        "select title, overview, genres, runtime, status, network, cast_names, released, bluray, poster,
                tmdb_score, imdb_score, rotten, metacritic, trakt_score, fetched_at, backdrop
         from details where kind = ? and tmdb_id = ?",
        [kind, tmdb_id],
        |r| {
            Ok(Details {
                kind,
                tmdb_id,
                title: r.get(0)?,
                overview: r.get(1)?,
                genres: r.get(2)?,
                runtime: r.get(3)?,
                status: r.get(4)?,
                network: r.get(5)?,
                cast: r.get(6)?,
                released: r.get(7)?,
                bluray: r.get(8)?,
                poster: r.get(9)?,
                tmdb_score: r.get(10)?,
                imdb_score: r.get(11)?,
                rotten: r.get(12)?,
                metacritic: r.get(13)?,
                trakt_score: r.get(14)?,
                fetched_at: r.get(15)?,
                backdrop: r.get(16)?,
            })
        },
    )
    .optional()
}

fn write(conn: &Connection, d: &Details) -> rusqlite::Result<()> {
    conn.execute(
        "insert or replace into details (kind, tmdb_id, title, overview, genres, runtime, status, network, cast_names,
            released, bluray, poster, tmdb_score, imdb_score, rotten, metacritic, trakt_score, fetched_at, backdrop)
         values (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            d.kind,
            d.tmdb_id,
            d.title,
            d.overview,
            d.genres,
            d.runtime,
            d.status,
            d.network,
            d.cast,
            d.released,
            d.bluray,
            d.poster,
            d.tmdb_score,
            d.imdb_score,
            d.rotten,
            d.metacritic,
            d.trakt_score,
            d.fetched_at,
            d.backdrop
        ],
    )?;
    Ok(())
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
static SOURCES: LazyLock<Mutex<Option<std::sync::Arc<Sources>>>> = LazyLock::new(|| Mutex::new(None));

fn sources(cfg: &MediaConfig) -> Option<std::sync::Arc<Sources>> {
    if cfg.tmdb.api_key.is_empty() && cfg.tvdb.api_key.is_empty() {
        return None;
    }
    let mut held = SOURCES.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(s) = held.as_ref()
        && s.tmdb == cfg.tmdb
        && s.omdb == cfg.omdb
        && s.trakt == cfg.trakt
        && s.tvdb_config == cfg.tvdb
    {
        return Some(s.clone());
    }
    let tmdb_base = cfg.tmdb.base_url.as_deref().unwrap_or(TMDB_BASE);
    let omdb_base = cfg.omdb.base_url.as_deref().unwrap_or(OMDB_BASE);
    let made = std::sync::Arc::new(Sources {
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
    });
    *held = Some(made.clone());
    Some(made)
}

impl Sources {
    fn tmdb_base(&self) -> &str {
        self.tmdb.base_url.as_deref().unwrap_or(TMDB_BASE)
    }

    /// The TMDB id of a title known only by IMDb or TheTVDB id.
    fn find_tmdb_id(&self, kind: i64, ids: &Ids) -> Option<i64> {
        let (external, source) = match (ids.imdb, ids.tvdb) {
            (Some(imdb), _) => (format!("tt{imdb:07}"), "imdb_id"),
            (None, Some(tvdb)) => (tvdb.to_string(), "tvdb_id"),
            _ => return None,
        };
        let url = format!(
            "{}/find/{external}?external_source={source}&api_key={}",
            self.tmdb_base(),
            encode(&self.tmdb.api_key)
        );
        let Ok(Reply::Json(body)) = self.tmdb_client.get(&url, &[]) else { return None };
        let list = if kind == Kind::Tv as i64 { "tv_results" } else { "movie_results" };
        body.get(list)?.as_array()?.first()?.get("id")?.as_i64()
    }

    /// None: TMDB has no such title. An error: it couldn't say.
    fn fetch(&self, kind: i64, tmdb_id: i64, ids: &Ids, now: i64) -> Result<Option<Details>, ()> {
        let section = if kind == Kind::Tv as i64 { "tv" } else { "movie" };
        let extra = if kind == Kind::Tv as i64 { "credits" } else { "credits,release_dates" };
        let url = format!(
            "{}/{section}/{tmdb_id}?append_to_response={extra}&api_key={}",
            self.tmdb_base(),
            encode(&self.tmdb.api_key)
        );
        let body = match self.tmdb_client.get(&url, &[]) {
            Ok(Reply::Json(body)) => body,
            Ok(Reply::NotFound) => return Ok(None),
            Err(_) => return Err(()),
        };
        let mut details = from_tmdb(kind, tmdb_id, &body, now);
        self.add_ratings(&mut details, kind, ids, now);
        Ok(Some(details))
    }

    /// What a show TMDB has no record of looks like to TheTVDB, with what else rates it.
    fn fetch_tvdb(&self, tvdb_id: i64, ids: &Ids, now: i64) -> Option<Details> {
        let body = self.tvdb.as_ref()?.series(tvdb_id).ok()??;
        let images = self.tvdb_config.base_url.as_deref().unwrap_or(TVDB_IMAGES);
        let mut details = from_tvdb(tvdb_id, &body, images, now);
        self.add_ratings(&mut details, Kind::Tv as i64, ids, now);
        Some(details)
    }

    fn add_ratings(&self, details: &mut Details, kind: i64, ids: &Ids, now: i64) {
        if let Some(imdb) = ids.imdb
            && !self.omdb.api_key.is_empty()
        {
            let base = self.omdb.base_url.as_deref().unwrap_or(OMDB_BASE);
            let url = format!("{base}/?i=tt{imdb:07}&apikey={}", encode(&self.omdb.api_key));
            match self.omdb_client.get(&url, &[]) {
                Ok(Reply::Json(body)) => add_omdb(details, &body),
                Ok(Reply::NotFound) => {}
                Err(_) => try_again_soon(details, now),
            }
        }
        if let Some(trakt) = ids.trakt
            && !self.trakt.client_id.is_empty()
        {
            let base = self.trakt.base_url.as_deref().unwrap_or(TRAKT_BASE);
            let section = if kind == Kind::Tv as i64 { "shows" } else { "movies" };
            let url = format!("{base}/{section}/{trakt}?extended=full");
            let headers = [("trakt-api-version", "2"), ("trakt-api-key", self.trakt.client_id.as_str())];
            match self.trakt_client.get(&url, &headers) {
                Ok(Reply::Json(body)) => details.trakt_score = trakt_score(&body),
                Ok(Reply::NotFound) => {}
                Err(_) => try_again_soon(details, now),
            }
        }
    }
}

/// What's kept, fetched first if it's missing or old. What's kept stays in use if a fetch fails.
pub fn for_title(cfg: &MediaConfig, kind: i64, ids: &Ids) -> Option<Details> {
    let conn = open().ok()?;
    let now = now_secs();
    let sources = sources(cfg);
    let tmdb_on = !cfg.tmdb.api_key.is_empty();
    let asked = sources.as_deref().filter(|_| tmdb_on);
    let tmdb_id = ids.tmdb.or_else(|| lookup(&conn, asked, kind, ids, now));
    if let Some(tmdb_id) = tmdb_id {
        let kept = read(&conn, kind, tmdb_id).ok().flatten();
        if kept.as_ref().is_some_and(|d| now - d.fetched_at < KEEP_FOR) {
            return kept;
        }
        if tmdb_on {
            match asked?.fetch(kind, tmdb_id, ids, now) {
                Ok(Some(fresh)) => {
                    let _ = write(&conn, &fresh);
                    return Some(fresh);
                }
                // a failure says nothing about the title
                Err(()) => return kept,
                // gone from TMDB: TheTVDB may still have it
                Ok(None) => {}
            }
        }
        if kept.is_some() {
            return kept;
        }
    }
    // a show TMDB has no record of, or no TMDB key
    let tvdb_id = ids.tvdb.filter(|_| kind == Kind::Tv as i64)?;
    let kept = read(&conn, kind, -tvdb_id).ok().flatten();
    if kept.as_ref().is_some_and(|d| now - d.fetched_at < KEEP_FOR) {
        return kept;
    }
    let Some(fresh) = sources.and_then(|s| s.fetch_tvdb(tvdb_id, ids, now)) else { return kept };
    let _ = write(&conn, &fresh);
    Some(fresh)
}

/// What's already kept for these releases' titles, by release, with nothing fetched: for a page of results.
pub fn kept_for<'a>(titles: impl Iterator<Item = (i64, i64, &'a Ids)>) -> HashMap<i64, Details> {
    let mut out = HashMap::new();
    let Ok(conn) = open() else { return out };
    for (release, kind, ids) in titles {
        let tmdb_id = ids.tmdb.or_else(|| {
            conn.query_row("select tmdb_id from details_lookups where lookup = ?", [lookup_key(kind, ids)], |r| {
                r.get(0)
            })
            .ok()
            .flatten()
        });
        let own = ids.tvdb.filter(|_| kind == Kind::Tv as i64).map(|tvdb| -tvdb);
        if let Some(details) = [tmdb_id, own].into_iter().flatten().find_map(|id| read(&conn, kind, id).ok().flatten())
        {
            out.insert(release, details);
        }
    }
    out
}

fn lookup_key(kind: i64, ids: &Ids) -> String {
    format!("{kind}:{:?}:{:?}", ids.imdb, ids.tvdb)
}

/// `find_tmdb_id`, remembered, soo an id TMDB doesn't know isn't asked about on every open.
fn lookup(conn: &Connection, sources: Option<&Sources>, kind: i64, ids: &Ids, now: i64) -> Option<i64> {
    let key = lookup_key(kind, ids);
    let kept: Option<(Option<i64>, i64)> = conn
        .query_row("select tmdb_id, checked_at from details_lookups where lookup = ?", [&key], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .optional()
        .ok()?;
    if let Some((id, checked_at)) = kept
        && (id.is_some() || now - checked_at < RETRY_AFTER)
    {
        return id;
    }
    let found = sources?.find_tmdb_id(kind, ids);
    let _ = conn.execute(
        "insert or replace into details_lookups (lookup, tmdb_id, checked_at) values (?, ?, ?)",
        params![key, found, now],
    );
    found
}

fn poster_dir() -> PathBuf {
    crate::paths::app_dir().join("posters")
}

fn poster_file(path: &str) -> Option<PathBuf> {
    if path.starts_with("http") {
        let name: String =
            path.chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' }).collect();
        return Some(poster_dir().join(name));
    }
    safe_image_path(path).then(|| poster_dir().join(path.trim_start_matches('/')))
}

/// Whether a poster can be shown with nothing fetched, for a page of results.
pub fn poster_kept(d: &Details) -> bool {
    d.poster.as_deref().and_then(poster_file).is_some_and(|file| file.exists())
}

/// A poster's bytes, kept on disk, soo browsers never call out and it works once TMDB doesn't.
pub fn poster(kind: i64, tmdb_id: i64, cfg: &MediaConfig) -> Option<Vec<u8>> {
    let path = read(&open().ok()?, kind, tmdb_id).ok().flatten()?.poster?;
    image(&path, cfg.tmdb.image_base_url.as_deref().unwrap_or(IMAGE_BASE))
}

/// A title's wide image, kept the same way.
pub fn backdrop(kind: i64, tmdb_id: i64, cfg: &MediaConfig) -> Option<Vec<u8>> {
    let path = read(&open().ok()?, kind, tmdb_id).ok().flatten()?.backdrop?;
    image(&path, cfg.tmdb.image_base_url.as_deref().unwrap_or(BACKDROP_BASE))
}

fn image(path: &str, base: &str) -> Option<Vec<u8>> {
    let file = poster_file(path)?;
    if let Ok(kept) = std::fs::read(&file) {
        return Some(kept);
    }
    let url = if path.starts_with("http") { path.to_string() } else { format!("{base}{path}") };
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(15)))
        .http_status_as_error(false)
        .max_redirects(0)
        .proxy(if url.starts_with("http://127.0.0.1") { None } else { ureq::Proxy::try_from_env() })
        .build()
        .into();
    let mut reply = agent.get(&url).call().ok()?;
    if reply.status().as_u16() != 200 {
        return None;
    }
    let mut bytes = Vec::new();
    reply.body_mut().as_reader().take(MAX_POSTER).read_to_end(&mut bytes).ok()?;
    if !bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        return None;
    }
    if std::fs::create_dir_all(poster_dir()).is_ok() && std::fs::write(&file, &bytes).is_ok() {
        trim_posters(&poster_dir(), MAX_POSTER_CACHE);
    }
    Some(bytes)
}

fn trim_posters(dir: &Path, limit: u64) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut files: Vec<(std::time::SystemTime, u64, PathBuf)> = entries
        .filter_map(Result::ok)
        .filter_map(|e| {
            let meta = e.metadata().ok()?;
            Some((meta.modified().ok()?, meta.len(), e.path()))
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
        trim_posters(dir.path(), 10);
        assert!(!dir.path().join("old.jpg").exists());
        assert!(dir.path().join("new.jpg").exists());
    }
}
