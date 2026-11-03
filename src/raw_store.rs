//! `atlas.raw.db`: permanent, schema-independent raw HTTP & probe archive next to the shards.
//!
//! Stores:
//! - Every unique non-empty PreDB feed payload (`raw_predb_log`, Zstd-compressed and bounded by quota) so
//!   ephemeral page-1 RSS/HTML/JSON and rolling paged feeds can be re-parsed into any future schema without
//!   historical loss.
//! - Cached HTTP responses (`raw_http_cache`, Zstd-compressed) for static PreDB archive dumps
//!   and external metadata APIs (`tmdb`, `tvdb`, `tvmaze`, `trakt`, `omdb`, `opensubtitles`).
//! - Natural-keyed Usenet & hash enrichments (`raw_opensubtitles` by `moviehash`, `raw_probes`, `raw_covers`,
//!   `raw_archives`, `raw_archive_inner_files`, `raw_nfo_passwords`, `raw_stills` by `(name, group_name)`, and
//!   `raw_backfill_deepest` / `raw_group_bounds`) so shard resets never require re-querying OpenSubtitles or
//!   re-downloading probed Usenet article bodies or video stills.

use std::path::{Path, PathBuf};
#[cfg(not(test))]
use std::sync::{Mutex, OnceLock};

use anyhow::Context;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::probe::opensubtitles::Feature;

/// Maximum rows kept in `raw_predb_log` before oldest payloads are pruned.
pub const MAX_PREDB_LOG_ROWS: i64 = 10_000;
/// Maximum compressed bytes kept in `raw_predb_log` (512 MiB).
pub const MAX_PREDB_LOG_BYTES: i64 = 512 << 20;
/// Maximum rows kept in `raw_http_cache` before oldest entries are pruned.
pub const MAX_HTTP_CACHE_ROWS: i64 = 100_000;
/// Maximum compressed bytes kept in `raw_http_cache` (512 MiB).
pub const MAX_HTTP_CACHE_BYTES: i64 = 512 << 20;
/// Maximum rows kept in `raw_opensubtitles`.
pub const MAX_OPENSUBTITLES_ROWS: i64 = 200_000;
/// Maximum rows kept in `raw_probes`.
pub const MAX_RAW_PROBES_ROWS: i64 = 250_000;
/// Maximum info JSON bytes kept in `raw_probes` (512 MiB).
pub const MAX_RAW_PROBES_BYTES: i64 = 512 << 20;
/// Maximum rows kept in `raw_stills`.
pub const MAX_RAW_STILLS_ROWS: i64 = 300_000;
/// Maximum rows kept in `raw_archives` / `raw_par2`.
pub const MAX_RAW_ARCHIVES_ROWS: i64 = 250_000;
/// Hard ceiling on total inner-file name bytes in `raw_archive_inner_files` (256 MiB).
pub const MAX_RAW_ARCHIVE_NAME_BYTES: i64 = 256 << 20;
/// Freshness window for live metadata HTTP cache hits (12 hours: shorter than `media::resolve`'s 1-day
/// negative retry and `media::details`'s 30-day refresh, while covering same-day schema-test resets).
pub const LIVE_HTTP_CACHE_TTL_SECS: i64 = 12 * 3_600;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS raw_http_cache (
    cache_key   TEXT PRIMARY KEY,
    source      TEXT NOT NULL,
    url         TEXT NOT NULL,
    status      INTEGER NOT NULL,
    body_zst    BLOB NOT NULL,
    fetched_at  INTEGER NOT NULL
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS raw_predb_log (
    id          INTEGER PRIMARY KEY,
    source_id   INTEGER NOT NULL,
    source_key  TEXT NOT NULL,
    url         TEXT NOT NULL,
    page        INTEGER NOT NULL DEFAULT 1,
    body_sha256 BLOB NOT NULL,
    body_zst    BLOB NOT NULL,
    fetched_at  INTEGER NOT NULL,
    UNIQUE(source_id, body_sha256)
);

CREATE TABLE IF NOT EXISTS raw_opensubtitles (
    moviehash    TEXT PRIMARY KEY,
    imdb_id      INTEGER,
    tmdb_id      INTEGER,
    kind         TEXT,
    title        TEXT,
    year         INTEGER,
    season       INTEGER,
    episode      INTEGER,
    release_name TEXT,
    raw_json_zst BLOB,
    checked_at   INTEGER NOT NULL
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS raw_probes (
    name         TEXT NOT NULL,
    group_name   TEXT NOT NULL,
    crc32        INTEGER,
    agent        TEXT,
    info         TEXT NOT NULL,
    moviehash    TEXT,
    parts        INTEGER,
    complete     INTEGER,
    config_stamp INTEGER,
    probed_at    INTEGER NOT NULL DEFAULT ( unixepoch() ),
    PRIMARY KEY (name, group_name)
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS raw_covers (
    name       TEXT NOT NULL,
    group_name TEXT NOT NULL,
    mime       TEXT NOT NULL,
    sha256     TEXT NOT NULL,
    PRIMARY KEY (name, group_name)
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS raw_archives (
    name             TEXT NOT NULL,
    group_name       TEXT NOT NULL,
    password         INTEGER NOT NULL,
    encryption       INTEGER,
    names_hidden     INTEGER,
    password_check   BLOB,
    parts            INTEGER,
    complete         INTEGER,
    listing_complete INTEGER,
    listing_damaged  INTEGER,
    PRIMARY KEY (name, group_name)
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS raw_par2 (
    name        TEXT NOT NULL,
    group_name  TEXT NOT NULL,
    covers_all  INTEGER NOT NULL DEFAULT 0,
    posted      BLOB,
    par2_block  INTEGER,
    par2_blocks INTEGER,
    parts       INTEGER,
    complete    INTEGER,
    PRIMARY KEY (name, group_name)
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS raw_archive_inner_files (
    release_name TEXT NOT NULL,
    group_name   TEXT NOT NULL,
    seq          INTEGER NOT NULL,
    name         TEXT NOT NULL,
    size         INTEGER,
    source       INTEGER,
    PRIMARY KEY (release_name, group_name, seq)
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS raw_nfo_passwords (
    name       TEXT NOT NULL,
    group_name TEXT NOT NULL,
    password   TEXT NOT NULL,
    PRIMARY KEY (name, group_name)
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS raw_stills (
    name       TEXT NOT NULL,
    group_name TEXT NOT NULL,
    position   INTEGER NOT NULL,
    at_ms      INTEGER,
    sha256     TEXT NOT NULL,
    width      INTEGER,
    height     INTEGER,
    phash      INTEGER,
    PRIMARY KEY (name, group_name, position)
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS raw_backfill_deepest (
    grp          TEXT PRIMARY KEY,
    server       TEXT NOT NULL,
    oldest_at    INTEGER,
    runner_up_at INTEGER
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS raw_group_bounds (
    name          TEXT PRIMARY KEY,
    first_article INTEGER,
    last_article  INTEGER,
    low_article   INTEGER,
    low_posted    INTEGER,
    high_article  INTEGER,
    high_posted   INTEGER
) WITHOUT ROWID;
";

/// `atlas.db` -> `atlas.raw.db`
pub fn path_for(main: &Path) -> PathBuf {
    crate::compact::with_suffix(main, "raw")
}

fn ensure_column(conn: &Connection, table: &str, column: &str, decl: &str) -> rusqlite::Result<()> {
    let exists: bool = conn
        .prepare(&format!("SELECT 1 FROM pragma_table_info('{table}') WHERE name = ?1"))?
        .exists([column])?;
    if !exists {
        conn.execute(&format!("ALTER TABLE {table} ADD COLUMN {column} {decl}"), [])?;
    }
    Ok(())
}

/// Opens or creates `atlas.raw.db` with WAL mode and all permanent raw tables.
pub fn create(path: &Path) -> rusqlite::Result<Connection> {
    let conn = crate::db::create_at(path)?;
    conn.query_row("pragma journal_mode = wal", [], |_| Ok(()))?;
    conn.execute_batch("pragma synchronous = normal; pragma busy_timeout = 30000;")?;
    conn.execute_batch(SCHEMA)?;
    ensure_column(&conn, "raw_predb_log", "page", "INTEGER NOT NULL DEFAULT 1")?;
    ensure_column(&conn, "raw_probes", "parts", "INTEGER")?;
    ensure_column(&conn, "raw_probes", "complete", "INTEGER")?;
    ensure_column(&conn, "raw_probes", "config_stamp", "INTEGER")?;
    ensure_column(&conn, "raw_archives", "parts", "INTEGER")?;
    ensure_column(&conn, "raw_archives", "complete", "INTEGER")?;
    ensure_column(&conn, "raw_archives", "listing_complete", "INTEGER")?;
    ensure_column(&conn, "raw_archives", "listing_damaged", "INTEGER")?;
    Ok(conn)
}

#[cfg(not(test))]
static GLOBAL_CONN: OnceLock<Option<Mutex<Connection>>> = OnceLock::new();

#[cfg(test)]
thread_local! {
    static TEST_RAW_PATH: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub fn set_test_raw_path(path: Option<PathBuf>) {
    TEST_RAW_PATH.with(|cell| *cell.borrow_mut() = path);
}

fn with_raw<T>(f: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> Option<T> {
    #[cfg(test)]
    {
        let path = TEST_RAW_PATH.with(|cell| cell.borrow().clone())?;
        let conn = create(&path).ok()?;
        return f(&conn).ok();
    }
    #[cfg(not(test))]
    {
        let mutex = GLOBAL_CONN
            .get_or_init(|| {
                let path = path_for(&crate::paths::database());
                create(&path).ok().map(Mutex::new)
            })
            .as_ref()?;
        let guard = mutex.lock().unwrap_or_else(|e| e.into_inner());
        f(&guard).ok()
    }
}

fn compress(text: &str) -> Option<Vec<u8>> {
    zstd::encode_all(text.as_bytes(), 3).ok()
}

fn decompress(bytes: &[u8]) -> Option<String> {
    let raw = zstd::decode_all(bytes).ok()?;
    String::from_utf8(raw).ok()
}

/// Strips `api_key=...` and `apikey=...` query parameters so cached URLs never store or key on credentials.
pub fn canonical_cache_url(url: &str) -> String {
    let Some((base, query)) = url.split_once('?') else {
        return url.to_string();
    };
    let kept: Vec<&str> = query
        .split('&')
        .filter(|pair| {
            let key = pair.split('=').next().unwrap_or("");
            !key.eq_ignore_ascii_case("api_key") && !key.eq_ignore_ascii_case("apikey")
        })
        .collect();
    if kept.is_empty() {
        base.to_string()
    } else {
        format!("{base}?{}", kept.join("&"))
    }
}

/// True when a media JSON response represents an empty search result that should not be cached,
/// so negative-lookup retries can discover newly added titles upstream.
pub fn is_empty_media_json(json: &Value) -> bool {
    if json.as_array().is_some_and(Vec::is_empty) {
        return true;
    }
    let Some(obj) = json.as_object() else {
        return false;
    };
    if obj.get("Response").and_then(Value::as_str).is_some_and(|r| r.eq_ignore_ascii_case("false")) {
        return true;
    }
    for key in ["results", "data", "movie_results", "tv_results", "tv_episode_results"] {
        if let Some(arr) = obj.get(key).and_then(Value::as_array)
            && arr.is_empty()
            && obj.len() <= 4
        {
            return true;
        }
    }
    false
}

/// Looks up a cached HTTP response for `source` and `url` within `LIVE_HTTP_CACHE_TTL_SECS`.
pub fn get_http_cache(source: &str, url: &str) -> Option<(u16, String)> {
    get_http_cache_with_ttl(source, url, Some(LIVE_HTTP_CACHE_TTL_SECS))
}

fn get_http_cache_with_ttl(source: &str, url: &str, max_age_secs: Option<i64>) -> Option<(u16, String)> {
    let clean_url = canonical_cache_url(url);
    let cache_key = format!("GET:{source}:{clean_url}");
    let now = chrono::Utc::now().timestamp();
    with_raw(|conn| {
        let row: Option<(i64, Vec<u8>, i64)> = conn
            .prepare_cached("SELECT status, body_zst, fetched_at FROM raw_http_cache WHERE cache_key = ?1")?
            .query_row(params![cache_key], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .optional()?;
        let Some((status, zst, fetched_at)) = row else {
            return Ok(None);
        };
        if let Some(ttl) = max_age_secs
            && now.saturating_sub(fetched_at) > ttl
        {
            return Ok(None);
        }
        let (Some(code), Some(body)) = (u16::try_from(status).ok(), decompress(&zst)) else {
            return Ok(None);
        };
        Ok(Some((code, body)))
    })
    .flatten()
}

fn prune_http_cache(
    conn: &Connection,
    incoming_bytes: i64,
    max_rows: i64,
    max_bytes: i64,
    now: i64,
) -> rusqlite::Result<()> {
    let (mut count, mut bytes): (i64, i64) = conn.query_row(
        "SELECT count(*), coalesce(sum(length(body_zst)), 0) FROM raw_http_cache",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if count >= max_rows || bytes + incoming_bytes > max_bytes {
        conn.execute(
            "DELETE FROM raw_http_cache
             WHERE source NOT IN ('nzedb_archive', 'dats_pciso_1998')
               AND fetched_at < ?1",
            params![now.saturating_sub(LIVE_HTTP_CACHE_TTL_SECS)],
        )?;
        (count, bytes) = conn.query_row(
            "SELECT count(*), coalesce(sum(length(body_zst)), 0) FROM raw_http_cache",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
    }
    if count >= max_rows {
        let to_drop = (count - max_rows + 1).max(64);
        conn.execute(
            "DELETE FROM raw_http_cache WHERE cache_key IN (
                SELECT cache_key FROM raw_http_cache ORDER BY fetched_at ASC LIMIT ?1
            )",
            params![to_drop],
        )?;
    }
    if bytes + incoming_bytes > max_bytes {
        let mut total: i64 =
            conn.query_row("SELECT coalesce(sum(length(body_zst)), 0) FROM raw_http_cache", [], |r| r.get(0))?;
        while total + incoming_bytes > max_bytes {
            let deleted = conn.execute(
                "DELETE FROM raw_http_cache WHERE cache_key IN (
                    SELECT cache_key FROM raw_http_cache ORDER BY fetched_at ASC LIMIT 64
                )",
                [],
            )?;
            if deleted == 0 {
                break;
            }
            total = conn.query_row("SELECT coalesce(sum(length(body_zst)), 0) FROM raw_http_cache", [], |r| r.get(0))?;
        }
    }
    Ok(())
}

/// Saves an HTTP response in `raw_http_cache`, enforcing `MAX_HTTP_CACHE_ROWS` and `MAX_HTTP_CACHE_BYTES`.
pub fn put_http_cache(source: &str, url: &str, status: u16, body: &str) {
    let Some(zst) = compress(body) else { return };
    let incoming_bytes = zst.len() as i64;
    if incoming_bytes > MAX_HTTP_CACHE_BYTES {
        return;
    }
    let clean_url = canonical_cache_url(url);
    let cache_key = format!("GET:{source}:{clean_url}");
    let now = chrono::Utc::now().timestamp();
    let _ = with_raw(|conn| {
        prune_http_cache(conn, incoming_bytes, MAX_HTTP_CACHE_ROWS, MAX_HTTP_CACHE_BYTES, now)?;
        conn.prepare_cached(
            "INSERT OR REPLACE INTO raw_http_cache (cache_key, source, url, status, body_zst, fetched_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?
        .execute(params![cache_key, source, clean_url, i64::from(status), zst, now])?;
        Ok(())
    });
}

/// True only for static historical PreDB archive snapshots (`nzedb_archive` and `dats_pciso_1998`).
/// Live catalogues (`nswdb`, `3dsdb`) and rolling numbered feeds (`predb_club`, `predb_net`, `xrel`, etc.)
/// are fetched live according to their poll intervals and archived in `raw_predb_log` for replay.
pub fn is_immutable_predb_page(source_key: &str, _page: u32) -> bool {
    matches!(source_key, "nzedb_archive" | "dats_pciso_1998")
}

/// Returns a forever-cached PreDB response if `is_immutable_predb_page(source_key, page)` is true.
pub fn get_predb_page_cache(source_key: &str, url: &str, page: u32) -> Option<String> {
    if !is_immutable_predb_page(source_key, page) {
        return None;
    }
    get_http_cache_with_ttl(source_key, url, None).and_then(|(status, body)| (status == 200).then_some(body))
}

fn prune_predb_log(conn: &Connection, incoming_bytes: i64, max_rows: i64, max_bytes: i64) -> rusqlite::Result<()> {
    let (rows, bytes): (i64, i64) = conn.query_row(
        "SELECT count(*), coalesce(sum(length(body_zst)), 0) FROM raw_predb_log",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if rows >= max_rows {
        let drop_rows = (rows - max_rows + 1).max(1);
        conn.execute(
            "DELETE FROM raw_predb_log WHERE id IN (SELECT id FROM raw_predb_log ORDER BY id ASC LIMIT ?1)",
            params![drop_rows],
        )?;
    }
    if bytes + incoming_bytes > max_bytes {
        let mut total: i64 =
            conn.query_row("SELECT coalesce(sum(length(body_zst)), 0) FROM raw_predb_log", [], |r| r.get(0))?;
        while total + incoming_bytes > max_bytes {
            let deleted = conn.execute(
                "DELETE FROM raw_predb_log WHERE id IN (SELECT id FROM raw_predb_log ORDER BY id ASC LIMIT 16)",
                [],
            )?;
            if deleted == 0 {
                break;
            }
            total =
                conn.query_row("SELECT coalesce(sum(length(body_zst)), 0) FROM raw_predb_log", [], |r| r.get(0))?;
        }
    }
    Ok(())
}

/// Archives a validated, non-empty PreDB payload in `raw_predb_log` (deduplicated by SHA-256 and bounded
/// by `MAX_PREDB_LOG_ROWS` / `MAX_PREDB_LOG_BYTES`) and, if it is a static archive snapshot, also caches it
/// in `raw_http_cache`.
pub fn record_predb_page(source_id: i64, source_key: &str, url: &str, page: u32, body: &str) {
    let Some(zst) = compress(body) else { return };
    if zst.len() as i64 > MAX_PREDB_LOG_BYTES {
        return;
    }
    let sha256 = Sha256::digest(body.as_bytes()).to_vec();
    let clean_url = canonical_cache_url(url);
    let now = chrono::Utc::now().timestamp();
    let cache_forever = is_immutable_predb_page(source_key, page);
    let _ = with_raw(|conn| {
        prune_predb_log(conn, zst.len() as i64, MAX_PREDB_LOG_ROWS, MAX_PREDB_LOG_BYTES)?;
        conn.prepare_cached(
            "INSERT OR IGNORE INTO raw_predb_log (source_id, source_key, url, page, body_sha256, body_zst, fetched_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )?
        .execute(params![source_id, source_key, clean_url, i64::from(page), sha256, &zst, now])?;
        if cache_forever {
            let cache_key = format!("GET:{source_key}:{clean_url}");
            conn.prepare_cached(
                "INSERT OR REPLACE INTO raw_http_cache (cache_key, source, url, status, body_zst, fetched_at)
                 VALUES (?1, ?2, ?3, 200, ?4, ?5)",
            )?
            .execute(params![cache_key, source_key, clean_url, zst, now])?;
        }
        Ok(())
    });
}

/// Looks up a previously resolved OpenSubtitles `moviehash` in `raw_opensubtitles`.
/// Returns `Some(Some(Feature))` if matched, `Some(None)` if checked with no match, or `None` if unknown.
pub fn get_opensubtitles(moviehash: &str) -> Option<Option<Feature>> {
    with_raw(|conn| {
        let row: Option<Feature> = conn
            .prepare_cached(
                "SELECT imdb_id, tmdb_id, kind, title, year, season, episode, release_name
                 FROM raw_opensubtitles WHERE moviehash = ?1",
            )?
            .query_row(params![moviehash], |r| {
                Ok(Feature {
                    imdb_id: r.get(0)?,
                    tmdb_id: r.get(1)?,
                    kind: r.get(2)?,
                    title: r.get(3)?,
                    year: r.get(4)?,
                    season: r.get(5)?,
                    episode: r.get(6)?,
                    release_name: r.get(7)?,
                })
            })
            .optional()?;
        Ok(row.map(|f| if f.imdb_id.is_some() { Some(f) } else { None }))
    })
    .flatten()
}

fn prune_opensubtitles(conn: &Connection, max_rows: i64) -> rusqlite::Result<()> {
    let count: i64 = conn.query_row("SELECT count(*) FROM raw_opensubtitles", [], |r| r.get(0))?;
    if count >= max_rows {
        let to_drop = (count - max_rows + 1).max(64);
        conn.execute(
            "DELETE FROM raw_opensubtitles WHERE moviehash IN (
                SELECT moviehash FROM raw_opensubtitles ORDER BY checked_at ASC LIMIT ?1
            )",
            params![to_drop],
        )?;
    }
    Ok(())
}

/// Persists an OpenSubtitles `moviehash` lookup result into `raw_opensubtitles`, bounded by `MAX_OPENSUBTITLES_ROWS`
/// and storing only the compact structured match fields (`raw_json_zst` omitted).
pub fn put_opensubtitles(moviehash: &str, feature: Option<&Feature>) {
    let f = feature.cloned().unwrap_or_default();
    let now = chrono::Utc::now().timestamp();
    let _ = with_raw(|conn| {
        prune_opensubtitles(conn, MAX_OPENSUBTITLES_ROWS)?;
        conn.prepare_cached(
            "INSERT INTO raw_opensubtitles
             (moviehash, imdb_id, tmdb_id, kind, title, year, season, episode, release_name, raw_json_zst, checked_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, NULL, ?10)
             ON CONFLICT(moviehash) DO UPDATE SET
                imdb_id = coalesce(excluded.imdb_id, main.raw_opensubtitles.imdb_id),
                tmdb_id = coalesce(excluded.tmdb_id, main.raw_opensubtitles.tmdb_id),
                kind = coalesce(excluded.kind, main.raw_opensubtitles.kind),
                title = coalesce(excluded.title, main.raw_opensubtitles.title),
                year = coalesce(excluded.year, main.raw_opensubtitles.year),
                season = coalesce(excluded.season, main.raw_opensubtitles.season),
                episode = coalesce(excluded.episode, main.raw_opensubtitles.episode),
                release_name = coalesce(excluded.release_name, main.raw_opensubtitles.release_name),
                raw_json_zst = NULL,
                checked_at = excluded.checked_at",
        )?
        .execute(params![
            moviehash,
            f.imdb_id,
            f.tmdb_id,
            f.kind,
            f.title,
            f.year,
            f.season,
            f.episode,
            f.release_name,
            now
        ])?;
        Ok(())
    });
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedProbe {
    pub crc32: Option<i64>,
    pub agent: Option<String>,
    pub info: String,
    pub moviehash: Option<String>,
    pub nfo_password: Option<String>,
    pub cover: Option<(String, String)>,
}

/// Looks up a previously probed release by `(name, group_name)` in `raw_probes`, returning `Some` only if
/// the saved probe's `config_stamp` covers `required_stamp` and its completeness/part count is not stale
/// relative to the release's current `(parts, complete)`.
pub fn get_probe(
    name: &str,
    group_name: &str,
    current_parts: i64,
    current_complete: bool,
    required_stamp: u64,
) -> Option<SavedProbe> {
    with_raw(|conn| {
        type ProbeRow = (Option<i64>, Option<String>, String, Option<String>, Option<i64>, Option<i64>, Option<i64>);
        let probe: Option<ProbeRow> = conn
            .prepare_cached(
                "SELECT crc32, agent, info, moviehash, parts, complete, config_stamp
                 FROM raw_probes WHERE name = ?1 AND group_name = ?2",
            )?
            .query_row(params![name, group_name], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?))
            })
            .optional()?;
        let Some((crc32, agent, info, moviehash, saved_parts, saved_complete, saved_stamp)) = probe else {
            return Ok(None);
        };
        let req_stamp = required_stamp as i64;
        if saved_stamp.is_none_or(|s| req_stamp & !s != 0) {
            return Ok(None);
        }
        let was_complete = saved_complete.unwrap_or(0) != 0;
        let had_parts = saved_parts.unwrap_or(0);
        if (current_complete && !was_complete) || current_parts > had_parts {
            return Ok(None);
        }
        let nfo_password: Option<String> = conn
            .prepare_cached("SELECT password FROM raw_nfo_passwords WHERE name = ?1 AND group_name = ?2")?
            .query_row(params![name, group_name], |r| r.get(0))
            .optional()?;
        let cover: Option<(String, String)> = conn
            .prepare_cached("SELECT mime, sha256 FROM raw_covers WHERE name = ?1 AND group_name = ?2")?
            .query_row(params![name, group_name], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()?;
        Ok(Some(SavedProbe { crc32, agent, info, moviehash, nfo_password, cover }))
    })
    .flatten()
}

pub struct ProbeRecord<'a> {
    pub name: &'a str,
    pub group_name: &'a str,
    pub crc32: Option<i64>,
    pub agent: Option<&'a str>,
    pub info: &'a str,
    pub moviehash: Option<&'a str>,
    pub parts: i64,
    pub complete: bool,
    pub config_stamp: u64,
    pub nfo_password: Option<&'a str>,
    pub cover: Option<(&'a str, &'a str)>,
}

fn prune_probes(conn: &Connection, incoming_bytes: i64, max_rows: i64, max_bytes: i64) -> rusqlite::Result<()> {
    let (rows, bytes): (i64, i64) = conn.query_row(
        "SELECT count(*), coalesce(sum(length(info)), 0) FROM raw_probes",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if rows >= max_rows {
        let to_drop = (rows - max_rows + 1).max(64);
        conn.execute(
            "DELETE FROM raw_probes WHERE (name, group_name) IN (
                SELECT name, group_name FROM raw_probes ORDER BY probed_at ASC LIMIT ?1
            )",
            params![to_drop],
        )?;
    }
    if bytes + incoming_bytes > max_bytes {
        let mut total: i64 =
            conn.query_row("SELECT coalesce(sum(length(info)), 0) FROM raw_probes", [], |r| r.get(0))?;
        while total + incoming_bytes > max_bytes {
            let deleted = conn.execute(
                "DELETE FROM raw_probes WHERE (name, group_name) IN (
                    SELECT name, group_name FROM raw_probes ORDER BY probed_at ASC LIMIT 64
                )",
                [],
            )?;
            if deleted == 0 {
                break;
            }
            total = conn.query_row("SELECT coalesce(sum(length(info)), 0) FROM raw_probes", [], |r| r.get(0))?;
        }
    }
    Ok(())
}

/// Persists a probed release's `ProbeInfo` JSON, completeness/stamp metadata, optional NFO password, and
/// optional cover reference into `raw_probes`, `raw_nfo_passwords`, and `raw_covers`.
pub fn put_probe(rec: &ProbeRecord<'_>) {
    let incoming_bytes = rec.info.len() as i64;
    if incoming_bytes > MAX_RAW_PROBES_BYTES {
        return;
    }
    let now = chrono::Utc::now().timestamp();
    let _ = with_raw(|conn| {
        prune_probes(conn, incoming_bytes, MAX_RAW_PROBES_ROWS, MAX_RAW_PROBES_BYTES)?;
        conn.prepare_cached(
            "INSERT OR REPLACE INTO raw_probes
             (name, group_name, crc32, agent, info, moviehash, parts, complete, config_stamp, probed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        )?
        .execute(params![
            rec.name,
            rec.group_name,
            rec.crc32,
            rec.agent,
            rec.info,
            rec.moviehash,
            rec.parts,
            i64::from(rec.complete),
            rec.config_stamp as i64,
            now
        ])?;
        if let Some(pw) = rec.nfo_password {
            conn.prepare_cached(
                "INSERT OR REPLACE INTO raw_nfo_passwords (name, group_name, password) VALUES (?1, ?2, ?3)",
            )?
            .execute(params![rec.name, rec.group_name, pw])?;
        }
        match rec.cover {
            Some((mime, sha256)) => {
                conn.prepare_cached(
                    "INSERT OR REPLACE INTO raw_covers (name, group_name, mime, sha256) VALUES (?1, ?2, ?3, ?4)",
                )?
                .execute(params![rec.name, rec.group_name, mime, sha256])?;
            }
            None => {
                conn.prepare_cached("DELETE FROM raw_covers WHERE name = ?1 AND group_name = ?2")?
                    .execute(params![rec.name, rec.group_name])?;
            }
        }
        Ok(())
    });
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedStill {
    pub position: u8,
    pub at_ms: Option<i64>,
    pub sha256: String,
    pub width: u32,
    pub height: u32,
    pub phash: Option<i64>,
}

/// Looks up previously extracted stills for `(name, group_name)` in `raw_stills`.
pub fn get_stills(name: &str, group_name: &str) -> Vec<SavedStill> {
    with_raw(|conn| {
        let rows = conn
            .prepare_cached(
                "SELECT position, at_ms, sha256, width, height, phash
                 FROM raw_stills WHERE name = ?1 AND group_name = ?2 ORDER BY position",
            )?
            .query_map(params![name, group_name], |r| {
                Ok(SavedStill {
                    position: r.get::<_, i64>(0)?.clamp(0, 255) as u8,
                    at_ms: r.get(1)?,
                    sha256: r.get(2)?,
                    width: r.get::<_, Option<i64>>(3)?.unwrap_or(0).max(0) as u32,
                    height: r.get::<_, Option<i64>>(4)?.unwrap_or(0).max(0) as u32,
                    phash: r.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    })
    .unwrap_or_default()
}

/// Persists extracted stills for `(name, group_name)` in `raw_stills`, bounded by `MAX_RAW_STILLS_ROWS`.
pub fn put_stills(name: &str, group_name: &str, stills: &[SavedStill]) {
    let _ = with_raw(|conn| {
        conn.prepare_cached("DELETE FROM raw_stills WHERE name = ?1 AND group_name = ?2")?
            .execute(params![name, group_name])?;
        let count: i64 = conn.query_row("SELECT count(*) FROM raw_stills", [], |r| r.get(0))?;
        if count + stills.len() as i64 > MAX_RAW_STILLS_ROWS {
            let to_drop = (count + stills.len() as i64 - MAX_RAW_STILLS_ROWS).max(64);
            conn.execute(
                "DELETE FROM raw_stills WHERE (name, group_name, position) IN (
                    SELECT name, group_name, position FROM raw_stills LIMIT ?1
                )",
                params![to_drop],
            )?;
        }
        let mut insert = conn.prepare_cached(
            "INSERT OR REPLACE INTO raw_stills (name, group_name, position, at_ms, sha256, width, height, phash)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )?;
        for s in stills {
            insert.execute(params![
                name,
                group_name,
                i64::from(s.position),
                s.at_ms,
                s.sha256,
                i64::from(s.width),
                i64::from(s.height),
                s.phash
            ])?;
        }
        Ok(())
    });
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedArchiveFile {
    pub seq: i64,
    pub name: String,
    pub size: Option<u64>,
    pub source: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedArchive {
    pub password: Option<i64>,
    pub encryption: Option<i64>,
    pub names_hidden: bool,
    pub password_check: Option<Vec<u8>>,
    pub listing_complete: bool,
    pub listing_damaged: bool,
    pub files: Vec<SavedArchiveFile>,
}

/// Looks up cached inner files for `(release_name, group_name)` and `source` in `raw_archive_inner_files`.
pub fn get_archive_files(release_name: &str, group_name: &str, source: i64) -> Vec<SavedArchiveFile> {
    with_raw(|conn| {
        let rows = conn
            .prepare_cached(
                "SELECT seq, name, size, coalesce(source, ?3)
                 FROM raw_archive_inner_files
                 WHERE release_name = ?1 AND group_name = ?2 AND coalesce(source, 2) = ?3
                 ORDER BY seq",
            )?
            .query_map(params![release_name, group_name, source], |r| {
                Ok(SavedArchiveFile {
                    seq: r.get(0)?,
                    name: r.get(1)?,
                    size: r.get::<_, Option<i64>>(2)?.and_then(|s| u64::try_from(s).ok()),
                    source: r.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    })
    .unwrap_or_default()
}

/// Looks up a previously inspected archive for `(name, group_name)` in `raw_archives` and `raw_archive_inner_files`.
/// If the previous inspection was incomplete/damaged (or an inconclusive `Password::Maybe` with no listed files)
/// on a smaller/incomplete release and the release has since grown, returns `None` so `ListStep` can inspect
/// the newly arrived articles.
pub fn get_archive(
    name: &str,
    group_name: &str,
    current_parts: i64,
    current_complete: bool,
) -> Option<SavedArchive> {
    with_raw(|conn| {
        type RawArchiveRow = (
            i64,
            Option<i64>,
            Option<i64>,
            Option<Vec<u8>>,
            Option<i64>,
            Option<i64>,
            Option<i64>,
            Option<i64>,
        );
        let row: Option<RawArchiveRow> = conn
            .prepare_cached(
                "SELECT password, encryption, names_hidden, password_check, parts, complete, listing_complete, listing_damaged
                 FROM raw_archives WHERE name = ?1 AND group_name = ?2",
            )?
            .query_row(params![name, group_name], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                ))
            })
            .optional()?;
        let files = conn
            .prepare_cached(
                "SELECT seq, name, size, coalesce(source, 2)
                 FROM raw_archive_inner_files
                 WHERE release_name = ?1 AND group_name = ?2 AND coalesce(source, 2) != 1
                 ORDER BY seq",
            )?
            .query_map(params![name, group_name], |r| {
                Ok(SavedArchiveFile {
                    seq: r.get(0)?,
                    name: r.get(1)?,
                    size: r.get::<_, Option<i64>>(2)?.and_then(|s| u64::try_from(s).ok()),
                    source: r.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if row.is_none() && files.is_empty() {
            return Ok(None);
        }
        let (
            password,
            encryption,
            names_hidden,
            password_check,
            saved_parts,
            saved_complete,
            listing_complete,
            listing_damaged,
        ) = match row {
            Some((pw, enc, hidden, check, parts, complete, l_comp, l_dmg)) => (
                Some(pw),
                enc,
                hidden.unwrap_or(0) != 0,
                check,
                parts,
                complete,
                l_comp.unwrap_or(0) != 0,
                l_dmg.unwrap_or(0) != 0,
            ),
            None => (None, None, false, None, None, None, false, false),
        };
        let inconclusive = !listing_complete
            || listing_damaged
            || (files.is_empty() && password == Some(crate::archive::Password::Maybe.code()));
        if inconclusive && let Some(had_parts) = saved_parts {
            let was_complete = saved_complete.unwrap_or(0) != 0;
            if (current_complete && !was_complete) || current_parts > had_parts {
                return Ok(None);
            }
        }
        Ok(Some(SavedArchive {
            password,
            encryption,
            names_hidden,
            password_check,
            listing_complete,
            listing_damaged,
            files,
        }))
    })
    .flatten()
}

pub struct ArchiveRecord<'a> {
    pub name: &'a str,
    pub group_name: &'a str,
    pub source: i64,
    pub files: &'a [crate::archive::InnerFile],
    pub password: Option<i64>,
    pub encryption: Option<i64>,
    pub names_hidden: bool,
    pub password_check: Option<&'a [u8]>,
    pub parts: i64,
    pub complete: bool,
    pub listing_complete: bool,
    pub listing_damaged: bool,
    pub max_names_bytes: i64,
}

/// Persists an archive or PAR2 inspection result into `raw_archives` and `raw_archive_inner_files`,
/// enforcing the configured `archive.max_names_mb` ceiling (capped at `MAX_RAW_ARCHIVE_NAME_BYTES`) and
/// `MAX_RAW_ARCHIVES_ROWS`.
pub fn put_archive(rec: &ArchiveRecord<'_>) {
    let _ = with_raw(|conn| {
        conn.prepare_cached(
            "DELETE FROM raw_archive_inner_files
             WHERE release_name = ?1 AND group_name = ?2 AND coalesce(source, 2) = ?3",
        )?
        .execute(params![rec.name, rec.group_name, rec.source])?;
        if !rec.files.is_empty() {
            let incoming_bytes: i64 = rec.files.iter().map(|r| r.name.len() as i64 + 32).sum();
            let effective_quota = rec.max_names_bytes.clamp(0, MAX_RAW_ARCHIVE_NAME_BYTES);
            let used_bytes: i64 = conn.query_row(
                "SELECT coalesce(sum(length(cast(name as blob)) + 32), 0) FROM raw_archive_inner_files",
                [],
                |r| r.get(0),
            )?;
            if used_bytes + incoming_bytes <= effective_quota {
                let mut insert_file = conn.prepare_cached(
                    "INSERT OR REPLACE INTO raw_archive_inner_files (release_name, group_name, seq, name, size, source)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                )?;
                for (i, row) in rec.files.iter().enumerate() {
                    let size = row.size.and_then(|s| i64::try_from(s).ok());
                    let seq = rec.source * 65_536 + i as i64;
                    insert_file.execute(params![rec.name, rec.group_name, seq, row.name, size, rec.source])?;
                }
            }
        }
        if let Some(pw) = rec.password {
            let count: i64 = conn.query_row("SELECT count(*) FROM raw_archives", [], |r| r.get(0))?;
            if count >= MAX_RAW_ARCHIVES_ROWS {
                let to_drop = (count - MAX_RAW_ARCHIVES_ROWS + 1).max(64);
                conn.execute(
                    "DELETE FROM raw_archives WHERE (name, group_name) IN (
                        SELECT name, group_name FROM raw_archives LIMIT ?1
                    )",
                    params![to_drop],
                )?;
            }
            conn.prepare_cached(
                "INSERT OR REPLACE INTO raw_archives
                 (name, group_name, password, encryption, names_hidden, password_check, parts, complete, listing_complete, listing_damaged)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            )?
            .execute(params![
                rec.name,
                rec.group_name,
                pw,
                rec.encryption,
                i64::from(rec.names_hidden),
                rec.password_check,
                rec.parts,
                i64::from(rec.complete),
                i64::from(rec.listing_complete),
                i64::from(rec.listing_damaged),
            ])?;
        }
        Ok(())
    });
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedPar2 {
    pub covers_all: bool,
    pub posted: Option<Vec<u8>>,
    pub sizes: Option<(i64, i64)>,
}

/// Looks up cached PAR2 twin-fingerprint and repair-block metadata for `(name, group_name)` in `raw_par2`.
/// Returns `Some` only when the release's `(parts, complete)` still match the inspected snapshot so twin
/// fingerprints (which include posted article counts) and `covers_all` remain exact.
pub fn get_par2(name: &str, group_name: &str, current_parts: i64, current_complete: bool) -> Option<SavedPar2> {
    with_raw(|conn| {
        type RawPar2Row = (i64, Option<Vec<u8>>, Option<i64>, Option<i64>, Option<i64>, Option<i64>);
        let row: Option<RawPar2Row> = conn
            .prepare_cached(
                "SELECT covers_all, posted, par2_block, par2_blocks, parts, complete
                 FROM raw_par2 WHERE name = ?1 AND group_name = ?2",
            )?
            .query_row(params![name, group_name], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?))
            })
            .optional()?;
        let Some((covers_all, posted, par2_block, par2_blocks, saved_parts, saved_complete)) = row else {
            return Ok(None);
        };
        let was_complete = saved_complete.unwrap_or(0) != 0;
        let had_parts = saved_parts.unwrap_or(0);
        if was_complete != current_complete || had_parts != current_parts {
            return Ok(None);
        }
        let covers = covers_all != 0;
        if covers && (posted.is_none() || par2_block.is_none() || par2_blocks.is_none()) {
            return Ok(None);
        }
        Ok(Some(SavedPar2 { covers_all: covers, posted, sizes: par2_block.zip(par2_blocks) }))
    })
    .flatten()
}

pub struct Par2Record<'a> {
    pub name: &'a str,
    pub group_name: &'a str,
    pub covers_all: bool,
    pub posted: Option<&'a [u8]>,
    pub sizes: Option<(i64, i64)>,
    pub parts: i64,
    pub complete: bool,
}

/// Persists PAR2 twin-fingerprint and block-size metadata into `raw_par2`, bounded by `MAX_RAW_ARCHIVES_ROWS`.
pub fn put_par2(rec: &Par2Record<'_>) {
    let _ = with_raw(|conn| {
        let count: i64 = conn.query_row("SELECT count(*) FROM raw_par2", [], |r| r.get(0))?;
        if count >= MAX_RAW_ARCHIVES_ROWS {
            let to_drop = (count - MAX_RAW_ARCHIVES_ROWS + 1).max(64);
            conn.execute(
                "DELETE FROM raw_par2 WHERE (name, group_name) IN (
                    SELECT name, group_name FROM raw_par2 LIMIT ?1
                )",
                params![to_drop],
            )?;
        }
        conn.prepare_cached(
            "INSERT OR REPLACE INTO raw_par2
             (name, group_name, covers_all, posted, par2_block, par2_blocks, parts, complete)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )?
        .execute(params![
            rec.name,
            rec.group_name,
            i64::from(rec.covers_all),
            rec.posted,
            rec.sizes.map(|(b, _)| b),
            rec.sizes.map(|(_, bs)| bs),
            rec.parts,
            i64::from(rec.complete),
        ])?;
        Ok(())
    });
}

/// Restores harvested `raw_backfill_deepest` rows from `atlas.raw.db` into `main_conn`'s `backfill_deepest` table.
pub fn restore_backfill_deepest(main: &Path, main_conn: &Connection) {
    let raw_path = path_for(main);
    if !raw_path.exists() {
        return;
    }
    let Ok(raw_conn) = create(&raw_path) else { return };
    let Ok(mut stmt) = raw_conn.prepare("SELECT grp, server, oldest_at, runner_up_at FROM raw_backfill_deepest")
    else {
        return;
    };
    let Ok(rows) = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Option<i64>>(2)?,
            r.get::<_, Option<i64>>(3)?,
        ))
    }) else {
        return;
    };
    let Ok(mut insert) = main_conn.prepare_cached(
        "INSERT OR IGNORE INTO backfill_deepest (grp, server, oldest_at, runner_up_at) VALUES (?1, ?2, ?3, ?4)",
    ) else {
        return;
    };
    for row in rows.flatten() {
        let _ = insert.execute(params![row.0, row.1, row.2, row.3]);
    }
}

fn has_table(conn: &Connection, schema: &str, table: &str) -> rusqlite::Result<bool> {
    conn.prepare(&format!("SELECT 1 FROM {schema}.sqlite_master WHERE type = 'table' AND name = ?1"))?
        .exists([table])
}

fn harvest_attached_release_tables(tx: &rusqlite::Transaction<'_>, schema: &str) -> rusqlite::Result<()> {
    if has_table(tx, schema, "release_opensubtitles")? {
        tx.execute_batch(&format!(
            "INSERT INTO main.raw_opensubtitles (moviehash, imdb_id, tmdb_id, kind, title, year, season, episode, release_name, checked_at)
             SELECT moviehash, imdb_id, tmdb_id, kind, title, year, season, episode, release_name, checked_at
             FROM {schema}.release_opensubtitles WHERE moviehash IS NOT NULL AND moviehash != ''
             ON CONFLICT(moviehash) DO UPDATE SET
                 imdb_id = coalesce(main.raw_opensubtitles.imdb_id, excluded.imdb_id),
                 tmdb_id = coalesce(main.raw_opensubtitles.tmdb_id, excluded.tmdb_id),
                 kind = coalesce(main.raw_opensubtitles.kind, excluded.kind),
                 title = coalesce(main.raw_opensubtitles.title, excluded.title),
                 year = coalesce(main.raw_opensubtitles.year, excluded.year),
                 season = coalesce(main.raw_opensubtitles.season, excluded.season),
                 episode = coalesce(main.raw_opensubtitles.episode, excluded.episode),
                 release_name = coalesce(main.raw_opensubtitles.release_name, excluded.release_name),
                 checked_at = max(main.raw_opensubtitles.checked_at, excluded.checked_at);"
        ))?;
    }
    if has_table(tx, schema, "release_probe")? {
        let probe_stamp: Option<i64> = if has_table(tx, schema, "meta")? {
            tx.query_row(&format!("SELECT value FROM {schema}.meta WHERE key = 'pp_stamp_4'"), [], |r| r.get(0))
                .optional()?
        } else {
            None
        };
        tx.execute(
            &format!(
                "INSERT OR REPLACE INTO main.raw_probes
                 (name, group_name, crc32, agent, info, moviehash, parts, complete, config_stamp)
                 SELECT r.name, r.group_name, p.crc32, p.agent, p.info, p.moviehash,
                        coalesce(r.parts, 0), coalesce(r.complete, 0), ?1
                 FROM {schema}.release_probe p JOIN {schema}.releases r ON r.id = p.release_id
                 WHERE r.name IS NOT NULL AND r.group_name IS NOT NULL"
            ),
            params![probe_stamp],
        )?;
    }
    if has_table(tx, schema, "release_cover")? {
        tx.execute_batch(&format!(
            "INSERT OR REPLACE INTO main.raw_covers (name, group_name, mime, sha256)
             SELECT r.name, r.group_name, c.mime, c.sha256
             FROM {schema}.release_cover c JOIN {schema}.releases r ON r.id = c.release_id
             WHERE r.name IS NOT NULL AND r.group_name IS NOT NULL;"
        ))?;
    }
    let has_fp = has_table(tx, schema, "release_fingerprint")?;
    let has_inner = has_table(tx, schema, "archive_inner_files")?;
    if has_table(tx, schema, "archive_release")? {
        let listing_complete_expr = match (has_fp, has_inner) {
            (true, true) => format!(
                "CASE WHEN (a.password = 0 AND NOT EXISTS (SELECT 1 FROM {schema}.archive_inner_files f WHERE f.release_id = a.release_id AND coalesce(f.source, 2) != 1))
                           OR EXISTS (SELECT 1 FROM {schema}.release_fingerprint fp WHERE fp.release_id = a.release_id AND fp.content IS NOT NULL)
                      THEN 1 ELSE 0 END"
            ),
            _ => "0".to_string(),
        };
        tx.execute_batch(&format!(
            "INSERT OR REPLACE INTO main.raw_archives
                 (name, group_name, password, encryption, names_hidden, password_check, parts, complete, listing_complete, listing_damaged)
             SELECT r.name, r.group_name, a.password, a.encryption, a.names_hidden, a.password_check,
                    coalesce(r.parts, 0), coalesce(r.complete, 0), {listing_complete_expr}, 0
             FROM {schema}.archive_release a JOIN {schema}.releases r ON r.id = a.release_id
             WHERE r.name IS NOT NULL AND r.group_name IS NOT NULL;"
        ))?;
    }
    if has_inner {
        tx.execute_batch(&format!(
            "INSERT OR REPLACE INTO main.raw_archive_inner_files (release_name, group_name, seq, name, size, source)
             SELECT r.name, r.group_name, f.seq, f.name, f.size, f.source
             FROM {schema}.archive_inner_files f JOIN {schema}.releases r ON r.id = f.release_id
             WHERE r.name IS NOT NULL AND r.group_name IS NOT NULL;"
        ))?;
    }
    if has_fp {
        tx.execute_batch(&format!(
            "INSERT OR REPLACE INTO main.raw_par2
                 (name, group_name, covers_all, posted, par2_block, par2_blocks, parts, complete)
             SELECT r.name, r.group_name,
                    CASE WHEN fp.posted IS NOT NULL AND fp.par2_block IS NOT NULL THEN 1 ELSE 0 END,
                    fp.posted, fp.par2_block, fp.par2_blocks,
                    coalesce(r.parts, 0), coalesce(r.complete, 0)
             FROM {schema}.release_fingerprint fp JOIN {schema}.releases r ON r.id = fp.release_id
             WHERE r.name IS NOT NULL AND r.group_name IS NOT NULL
               AND (fp.posted IS NOT NULL OR fp.par2_block IS NOT NULL);"
        ))?;
    }
    if has_table(tx, schema, "release_nfo_password")? {
        tx.execute_batch(&format!(
            "INSERT OR REPLACE INTO main.raw_nfo_passwords (name, group_name, password)
             SELECT r.name, r.group_name, np.password
             FROM {schema}.release_nfo_password np JOIN {schema}.releases r ON r.id = np.release_id
             WHERE r.name IS NOT NULL AND r.group_name IS NOT NULL;"
        ))?;
    }
    if has_table(tx, schema, "release_stills")? {
        tx.execute_batch(&format!(
            "INSERT OR REPLACE INTO main.raw_stills (name, group_name, position, at_ms, sha256, width, height, phash)
             SELECT r.name, r.group_name, s.position, s.at_ms, s.sha256, s.width, s.height, s.phash
             FROM {schema}.release_stills s JOIN {schema}.releases r ON r.id = s.release_id
             WHERE r.name IS NOT NULL AND r.group_name IS NOT NULL;"
        ))?;
    }
    Ok(())
}

fn harvest_attached_main(conn: &Connection, schema: &str) -> rusqlite::Result<()> {
    let tx = conn.unchecked_transaction()?;
    if has_table(&tx, schema, "backfill_deepest")? {
        tx.execute_batch(&format!(
            "INSERT OR REPLACE INTO main.raw_backfill_deepest (grp, server, oldest_at, runner_up_at)
             SELECT grp, server, oldest_at, runner_up_at FROM {schema}.backfill_deepest;"
        ))?;
    }
    if has_table(&tx, schema, "groups")? {
        tx.execute_batch(&format!(
            "INSERT OR REPLACE INTO main.raw_group_bounds (name, first_article, last_article, low_article, low_posted, high_article, high_posted)
             SELECT name, first_article, last_article, low_article, low_posted, high_article, high_posted FROM {schema}.groups
             WHERE low_article IS NOT NULL OR high_article IS NOT NULL OR first_article IS NOT NULL;"
        ))?;
    }
    if has_table(&tx, schema, "releases")? {
        harvest_attached_release_tables(&tx, schema)?;
    }
    tx.commit()
}

fn harvest_attached_shard(conn: &Connection) -> rusqlite::Result<()> {
    if !has_table(conn, "sh", "releases")? {
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ERROR),
            Some("attached shard is missing releases table".into()),
        ));
    }
    let tx = conn.unchecked_transaction()?;
    harvest_attached_release_tables(&tx, "sh")?;
    tx.commit()
}

/// Harvests natural-keyed enrichments from `main` (`atlas.db` + `atlas.s0..s7.db`) into `atlas.raw.db`,
/// including legacy pre-sharding release tables when `atlas.db` itself holds `releases`.
/// Any SQL or transaction failure rolls back the failed harvest and returns `Err` so callers never delete
/// source databases after a partial or failed harvest.
pub fn harvest_from_shards(main: &Path, log: &dyn Fn(&str)) -> anyhow::Result<()> {
    let raw_path = path_for(main);
    let conn = create(&raw_path).with_context(|| format!("opening {}", raw_path.display()))?;
    if main.exists() {
        log(&format!("harvesting watermarks from {}...", main.display()));
        conn.execute("ATTACH DATABASE ?1 AS main_db", [main.to_string_lossy().as_ref()])
            .with_context(|| format!("attaching {}", main.display()))?;
        let res = harvest_attached_main(&conn, "main_db");
        let _ = conn.execute_batch("ROLLBACK");
        let detach_res = conn.execute("DETACH DATABASE main_db", []);
        res.with_context(|| format!("harvesting {}", main.display()))?;
        detach_res.with_context(|| format!("detaching {}", main.display()))?;
    }
    for shard_idx in 0..crate::store::SHARDS {
        let shard_path = crate::store::shard_path(main, shard_idx);
        if !shard_path.exists() {
            continue;
        }
        log(&format!("harvesting shard s{shard_idx} ({})...", shard_path.display()));
        conn.execute("ATTACH DATABASE ?1 AS sh", [shard_path.to_string_lossy().as_ref()])
            .with_context(|| format!("attaching {}", shard_path.display()))?;
        let res = harvest_attached_shard(&conn);
        let _ = conn.execute_batch("ROLLBACK");
        let detach_res = conn.execute("DETACH DATABASE sh", []);
        res.with_context(|| format!("harvesting {}", shard_path.display()))?;
        detach_res.with_context(|| format!("detaching {}", shard_path.display()))?;
    }
    Ok(())
}

/// Replays all raw HTTP payloads in `raw_predb_log` through current PreDB parsers into `atlas.predb.db`.
pub fn replay_predb_at(main: &Path, apply: bool, log: &dyn Fn(&str)) -> anyhow::Result<()> {
    let raw_path = path_for(main);
    let predb_path = crate::predb::db::path_for(main);
    let raw_conn = create(&raw_path).with_context(|| format!("opening {}", raw_path.display()))?;
    let mut predb_conn =
        crate::predb::db::create(&predb_path).with_context(|| format!("opening {}", predb_path.display()))?;
    let rows: Vec<(i64, i64, String, i64, Vec<u8>, i64)> = raw_conn
        .prepare("SELECT id, source_id, source_key, page, body_zst, fetched_at FROM raw_predb_log ORDER BY id")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("reading raw_predb_log")?;
    let mut parsed_entries = 0usize;
    let mut inserted = 0u64;
    let mut updated = 0u64;
    for (id, source_id, key, page, zst, fetched_at) in &rows {
        let spec = crate::predb::feeds::spec(key)
            .ok_or_else(|| anyhow::anyhow!("unknown predb source '{key}' in raw_predb_log id={id}"))?;
        let body = decompress(zst)
            .ok_or_else(|| anyhow::anyhow!("corrupt zstd payload in raw_predb_log id={id} ({key})"))?;
        let page_num = u32::try_from(*page).unwrap_or(1).max(1);
        let entries = match spec.parse {
            crate::predb::feeds::Parser::Json(parse) => {
                let val: Value = serde_json::from_str(&body)
                    .with_context(|| format!("bad JSON in raw_predb_log id={id} ({key})"))?;
                parse(&val, page_num).map_err(|e| anyhow::anyhow!("parse error in raw_predb_log id={id} ({key}): {e:?}"))?
            }
            crate::predb::feeds::Parser::Text(parse) => {
                parse(&body).map_err(|e| anyhow::anyhow!("parse error in raw_predb_log id={id} ({key}): {e:?}"))?
            }
        };
        parsed_entries += entries.len();
        if apply {
            let c = crate::predb::import::import(&mut predb_conn, *source_id, &entries, *fetched_at, None)
                .with_context(|| format!("importing raw_predb_log id={id} ({key}) into {}", predb_path.display()))?;
            inserted += c.inserted;
            updated += c.updated;
        }
    }
    if apply {
        log(&format!(
            "replayed {} raw predb payloads ({} entries): {} inserted, {} updated",
            rows.len(),
            parsed_entries,
            inserted,
            updated
        ));
    } else {
        log(&format!(
            "dry run (pass --apply to write): {} raw predb payloads yield {} parsed entries",
            rows.len(),
            parsed_entries
        ));
    }
    Ok(())
}

pub fn run_replay_predb(args: &[String]) -> i32 {
    let apply = args.iter().any(|a| a == "--apply");
    let main = crate::paths::database();
    if let Err(e) = replay_predb_at(&main, apply, &|msg| println!("{msg}")) {
        println!("couldnt replay raw predb payloads: {e:#}");
        return 1;
    }
    0
}

/// Safely holds the exclusive database lock, resolves any interrupted conversion/compaction swaps,
/// harvests natural-keyed shard data into `atlas.raw.db`, and removes `atlas.db`, `atlas.s0..s7.db`,
/// and compaction/conversion scratch files while keeping `atlas.raw.db`, `atlas.predb.db`,
/// `atlas.media.db`, `atlas.details.db`, `posters/`, `covers/`, and `stills/`.
pub fn reset_shards_at(main: &Path, log: &dyn Fn(&str)) -> anyhow::Result<()> {
    let _exclusive = crate::compact::Lock::take(main)?;
    if (!main.try_exists()? || crate::convert::is_empty_interloper(main))
        && let Some(msg) = crate::convert::recover_cut_swap(main)?
    {
        log(&msg);
    }
    for msg in crate::compact::recover_cut_swaps(main)? {
        log(&msg);
    }
    crate::compact::refuse_unresolved_backups(main)?;
    harvest_from_shards(main, log)?;
    let media_path = crate::compact::with_suffix(main, "media");
    if media_path.exists() {
        let conn = crate::db::open_at(&media_path).with_context(|| format!("opening {}", media_path.display()))?;
        if has_table(&conn, "main", "media_titles")? {
            conn.execute("UPDATE media_titles SET fanout_done = 0 WHERE state = 1", [])
                .with_context(|| format!("resetting fanout_done in {}", media_path.display()))?;
        }
    }
    let predb_path = crate::predb::db::path_for(main);
    if predb_path.exists() {
        let conn = crate::db::open_at(&predb_path).with_context(|| format!("opening {}", predb_path.display()))?;
        if has_table(&conn, "main", "meta")? {
            conn.execute("UPDATE meta SET value = 0 WHERE key = 'late_after_id'", [])
                .with_context(|| format!("resetting late_after_id in {}", predb_path.display()))?;
        }
    }
    let remove_db_family = |base: &Path| -> anyhow::Result<()> {
        for suffix in ["", "-wal", "-shm", "-journal", ".sealed-log-cursor"] {
            let p = PathBuf::from(format!("{}{suffix}", base.display()));
            match std::fs::remove_file(&p) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e).with_context(|| format!("removing {}", p.display())),
            }
        }
        Ok(())
    };
    for shard in crate::store::shard_paths(main) {
        let copy = crate::compact::with_suffix(&shard, "compact");
        remove_db_family(&crate::compact::with_suffix(&copy, "check"))?;
        remove_db_family(&copy)?;
        remove_db_family(&crate::compact::with_suffix(&shard, "domains"))?;
        remove_db_family(&crate::compact::with_suffix(&shard, "precompact"))?;
        remove_db_family(&shard)?;
    }
    remove_db_family(&main.with_file_name("atlas.old.db"))?;
    remove_db_family(&main.with_file_name("atlas.new.db"))?;
    remove_db_family(main)?;
    if let Some(dir) = main.parent()
        && let Ok(entries) = std::fs::read_dir(dir)
    {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let s = name.to_string_lossy();
            if s.starts_with("segment-") && s.ends_with(".log") {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    Ok(())
}

pub fn run_reset_shards(args: &[String]) -> i32 {
    let apply = args.iter().any(|a| a == "--apply");
    if crate::procs::indexer_alive() {
        println!("stop indexing first");
        return 1;
    }
    let main = crate::paths::database();
    if !apply {
        println!(
            "dry run (pass --apply to reset):\n  1. harvest natural-keyed data from {} and shards into {}\n  2. preserve atlas.raw.db, atlas.predb.db, atlas.media.db, atlas.details.db, posters/, covers/, stills/\n  3. delete atlas.db*, atlas.s0..s7.db*, *.sealed-log-cursor, and segment-*.log\n  4. reset media fanout_done=0 and predb late_after_id=0",
            main.display(),
            path_for(&main).display()
        );
        return 0;
    }
    let predb_path = crate::predb::db::path_for(&main);
    let media_path = crate::compact::with_suffix(&main, "media");
    if let Err(e) = reset_shards_at(&main, &|msg| println!("{msg}")) {
        println!("couldnt reset shards safely, aborting: {e:#}");
        return 1;
    }
    println!(
        "reset complete: shards removed, while {}, {}, {}, and media asset directories were preserved",
        path_for(&main).display(),
        predb_path.display(),
        media_path.display()
    );
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_url_strips_api_keys_only() {
        assert_eq!(
            canonical_cache_url("https://api.themoviedb.org/3/search/movie?api_key=SECRET123&query=Matrix&year=1999"),
            "https://api.themoviedb.org/3/search/movie?query=Matrix&year=1999"
        );
        assert_eq!(
            canonical_cache_url("https://www.omdbapi.com/?apikey=MYKEY&i=tt0133093"),
            "https://www.omdbapi.com/?i=tt0133093"
        );
        assert_eq!(
            canonical_cache_url("https://api.themoviedb.org/3/movie/603?api_key=SECRET"),
            "https://api.themoviedb.org/3/movie/603"
        );
    }

    #[test]
    fn raw_store_roundtrips_http_predb_opensubtitles_probes_and_stills() {
        let dir = tempfile::tempdir().unwrap();
        let raw_path = dir.path().join("atlas.raw.db");
        set_test_raw_path(Some(raw_path.clone()));

        put_http_cache("tmdb", "https://api.themoviedb.org/3/movie/603?api_key=SECRET", 200, r#"{"id":603}"#);
        assert_eq!(
            get_http_cache("tmdb", "https://api.themoviedb.org/3/movie/603?api_key=OTHER_KEY"),
            Some((200, r#"{"id":603}"#.to_string()))
        );
        // Expired HTTP cache entries are not served live so 24h negative retries and 30d refreshes reach upstream.
        assert_eq!(
            get_http_cache_with_ttl("tmdb", "https://api.themoviedb.org/3/movie/603", Some(-1)),
            None
        );

        // Empty media search results are detected so negative lookups aren't pinned in raw_http_cache.
        assert!(is_empty_media_json(&serde_json::from_str("[]").unwrap()));
        assert!(is_empty_media_json(&serde_json::from_str(r#"{"page":1,"results":[]}"#).unwrap()));
        assert!(is_empty_media_json(&serde_json::from_str(r#"{"Response":"False","Error":"Movie not found!"}"#).unwrap()));
        assert!(!is_empty_media_json(&serde_json::from_str(r#"{"id":603,"title":"The Matrix"}"#).unwrap()));

        // Rolling PreDB feeds and live catalogues (nswdb, 3dsdb) are logged in raw_predb_log for replay,
        // never served as immutable HTTP cache hits.
        record_predb_page(1, "predb_club", "https://predb.club/api/v1/?page=2", 2, r#"{"status":"success"}"#);
        assert_eq!(get_predb_page_cache("predb_club", "https://predb.club/api/v1/?page=2", 2), None);
        record_predb_page(30, "nswdb", "https://nswdb.com/xml", 1, "<releases/>");
        assert_eq!(get_predb_page_cache("nswdb", "https://nswdb.com/xml", 1), None);

        // Static historical archive snapshots (nzedb_archive, dats_pciso_1998) are cached in raw_http_cache.
        record_predb_page(33, "nzedb_archive", "https://example.invalid/nzedb.tsv", 1, "title\tcat\n");
        assert_eq!(
            get_predb_page_cache("nzedb_archive", "https://example.invalid/nzedb.tsv", 1),
            Some("title\tcat\n".to_string())
        );

        // Quota pruning on raw_predb_log keeps row count and byte usage bounded.
        {
            let conn = create(&raw_path).unwrap();
            prune_predb_log(&conn, 100, 1, MAX_PREDB_LOG_BYTES).unwrap();
            let count: i64 = conn.query_row("SELECT count(*) FROM raw_predb_log", [], |r| r.get(0)).unwrap();
            assert!(count <= 1);
        }

        let feat = Feature {
            imdb_id: Some(133093),
            tmdb_id: Some(603),
            kind: Some("Movie".into()),
            title: Some("The Matrix".into()),
            year: Some(1999),
            season: None,
            episode: None,
            release_name: Some("The.Matrix.1999.1080p-GRP".into()),
        };
        put_opensubtitles("8e245d9679d31e12", Some(&feat));
        assert_eq!(get_opensubtitles("8e245d9679d31e12"), Some(Some(feat)));

        let stamp = 1 | 2 | 4 | 8;
        put_probe(&ProbeRecord {
            name: "Some.Release-GRP",
            group_name: "alt.binaries.test",
            crc32: Some(12345),
            agent: Some("Nyuu"),
            info: r#"{"nfo":"hi"}"#,
            moviehash: Some("8e245d9679d31e12"),
            parts: 20,
            complete: false,
            config_stamp: stamp,
            nfo_password: Some("secretpw"),
            cover: Some(("image/jpeg", "deadbeef")),
        });
        let saved = get_probe("Some.Release-GRP", "alt.binaries.test", 20, false, stamp).unwrap();
        assert_eq!(saved.crc32, Some(12345));
        assert_eq!(saved.agent.as_deref(), Some("Nyuu"));
        assert_eq!(saved.info, r#"{"nfo":"hi"}"#);
        assert_eq!(saved.moviehash.as_deref(), Some("8e245d9679d31e12"));
        assert_eq!(saved.nfo_password.as_deref(), Some("secretpw"));
        assert_eq!(saved.cover, Some(("image/jpeg".to_string(), "deadbeef".to_string())));

        // Release grew in parts or became complete -> invalidated!
        assert_eq!(get_probe("Some.Release-GRP", "alt.binaries.test", 25, false, stamp), None);
        assert_eq!(get_probe("Some.Release-GRP", "alt.binaries.test", 20, true, stamp), None);

        // Probe step config_stamp gained a new bit -> invalidated!
        assert_eq!(get_probe("Some.Release-GRP", "alt.binaries.test", 20, false, stamp | 16), None);

        // Stills roundtrip
        let stills = vec![SavedStill {
            position: 25,
            at_ms: Some(120_000),
            sha256: "a".repeat(64),
            width: 960,
            height: 540,
            phash: Some(42),
        }];
        put_stills("Some.Release-GRP", "alt.binaries.test", &stills);
        assert_eq!(get_stills("Some.Release-GRP", "alt.binaries.test"), stills);

        // Archive & PAR2 inner-files roundtrip
        let archive_files = vec![crate::archive::InnerFile {
            name: "Movie.2026.1080p.mkv".to_string(),
            size: Some(1_500_000_000),
        }];
        put_archive(&ArchiveRecord {
            name: "Some.Release-GRP",
            group_name: "alt.binaries.test",
            source: crate::archive::store::SOURCE_RAR,
            files: &archive_files,
            password: Some(crate::archive::Password::Yes.code()),
            encryption: Some(crate::archive::Encryption::Rar5Aes256.code()),
            names_hidden: true,
            password_check: Some(&[1, 2, 3, 4]),
            parts: 20,
            complete: true,
            listing_complete: true,
            listing_damaged: false,
            max_names_bytes: 16 << 20,
        });
        let saved_arch = get_archive("Some.Release-GRP", "alt.binaries.test", 20, true).unwrap();
        assert_eq!(saved_arch.password, Some(crate::archive::Password::Yes.code()));
        assert_eq!(saved_arch.encryption, Some(crate::archive::Encryption::Rar5Aes256.code()));
        assert!(saved_arch.names_hidden);
        assert!(saved_arch.listing_complete);
        assert!(!saved_arch.listing_damaged);
        assert_eq!(saved_arch.password_check.as_deref(), Some(&[1, 2, 3, 4][..]));
        assert_eq!(saved_arch.files.len(), 1);
        assert_eq!(saved_arch.files[0].name, "Movie.2026.1080p.mkv");

        // PAR2 twin fingerprint & block metadata roundtrip
        put_par2(&Par2Record {
            name: "Some.Release-GRP",
            group_name: "alt.binaries.test",
            covers_all: true,
            posted: Some(&[2; 21]),
            sizes: Some((384_000, 4_000)),
            parts: 20,
            complete: true,
        });
        let saved_par2 = get_par2("Some.Release-GRP", "alt.binaries.test", 20, true).unwrap();
        assert!(saved_par2.covers_all);
        assert_eq!(saved_par2.posted.as_deref(), Some(&[2; 21][..]));
        assert_eq!(saved_par2.sizes, Some((384_000, 4_000)));
        assert_eq!(get_par2("Some.Release-GRP", "alt.binaries.test", 25, true), None);

        // HTTP cache byte quota pruning
        {
            let conn = create(&raw_path).unwrap();
            let now = chrono::Utc::now().timestamp();
            prune_http_cache(&conn, MAX_HTTP_CACHE_BYTES, MAX_HTTP_CACHE_ROWS, MAX_HTTP_CACHE_BYTES, now).unwrap();
            let total: i64 = conn
                .query_row("SELECT coalesce(sum(length(body_zst)), 0) FROM raw_http_cache", [], |r| r.get(0))
                .unwrap();
            assert_eq!(total, 0);
        }

        set_test_raw_path(None);
    }

    #[test]
    fn reset_shards_holds_lock_recovers_or_cleans_compaction_artifacts_and_aborts_on_failure() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        crate::db::create_db_at(&main).unwrap();

        // 1. If a writer/compaction lock is held, reset_shards_at refuses immediately.
        let guard = crate::compact::try_hold_off_compaction(&main).unwrap().unwrap();
        let busy_err = reset_shards_at(&main, &|_| {}).expect_err("must refuse while lock is held");
        assert!(busy_err.downcast_ref::<crate::compact::Busy>().is_some(), "{busy_err:#}");
        drop(guard);

        // 2. If a shard is corrupt (missing releases table), reset_shards_at aborts without deleting anything.
        let s0 = crate::store::shard_path(&main, 0);
        let conn = Connection::open(&s0).unwrap();
        conn.execute_batch("DROP TABLE releases;").unwrap();
        drop(conn);

        let err = reset_shards_at(&main, &|_| {}).expect_err("harvest must fail when releases table is missing");
        assert!(format!("{err:#}").contains("s0"), "{err:#}");
        assert!(main.exists(), "main db must not be deleted on failed harvest");
        assert!(s0.exists(), "shard s0 must not be deleted on failed harvest");

        // 3. Restore valid s0, plant compaction/conversion leftovers, and verify clean reset.
        std::fs::copy(crate::store::shard_path(&main, 1), &s0).unwrap();
        let precompact_s1 = crate::compact::with_suffix(&crate::store::shard_path(&main, 1), "precompact");
        std::fs::remove_file(crate::store::shard_path(&main, 1)).unwrap();
        std::fs::copy(crate::store::shard_path(&main, 2), &precompact_s1).unwrap();
        let old_main = main.with_file_name("atlas.old.db");
        std::fs::write(&old_main, b"old").unwrap();

        reset_shards_at(&main, &|_| {}).unwrap();
        assert!(!main.exists());
        assert!(!old_main.exists());
        assert!(!precompact_s1.exists());
        assert!(path_for(&main).exists(), "atlas.raw.db must be preserved");
    }
}
