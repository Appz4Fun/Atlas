//! `atlas.raw.db`: permanent, schema-independent raw HTTP & probe archive next to the shards.
//!
//! Stores:
//! - Every unique PreDB feed payload (`raw_predb_log`, Zstd-compressed) so ephemeral page-1 RSS/HTML/JSON
//!   and rolling paged feeds can be re-parsed into any future schema without historical loss.
//! - Forever-cached HTTP responses (`raw_http_cache`, Zstd-compressed) for static PreDB dumps
//!   and external metadata APIs (`tmdb`, `tvdb`, `tvmaze`, `trakt`, `omdb`, `opensubtitles`).
//! - Natural-keyed Usenet & hash enrichments (`raw_opensubtitles` by `moviehash`, `raw_probes`, `raw_covers`,
//!   `raw_archives`, `raw_archive_inner_files`, `raw_nfo_passwords`, `raw_stills` by `(name, group_name)`, and
//!   `raw_backfill_deepest` / `raw_group_bounds`) so shard resets never require re-querying OpenSubtitles or
//!   re-downloading probed Usenet article bodies.

use std::path::{Path, PathBuf};
#[cfg(not(test))]
use std::sync::{Mutex, OnceLock};

use anyhow::Context;
use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};

use crate::probe::opensubtitles::Feature;

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
    name           TEXT NOT NULL,
    group_name     TEXT NOT NULL,
    password       INTEGER NOT NULL,
    encryption     INTEGER,
    names_hidden   INTEGER,
    password_check BLOB,
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

/// Looks up a forever-cached HTTP response for `source` and `url`.
pub fn get_http_cache(source: &str, url: &str) -> Option<(u16, String)> {
    let clean_url = canonical_cache_url(url);
    let cache_key = format!("GET:{source}:{clean_url}");
    with_raw(|conn| {
        let row: Option<(i64, Vec<u8>)> = conn
            .prepare_cached("SELECT status, body_zst FROM raw_http_cache WHERE cache_key = ?1")?
            .query_row(params![cache_key], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()?;
        Ok(row.and_then(|(status, zst)| Some((u16::try_from(status).ok()?, decompress(&zst)?))))
    })
    .flatten()
}

/// Saves an HTTP response in `raw_http_cache`.
pub fn put_http_cache(source: &str, url: &str, status: u16, body: &str) {
    let Some(zst) = compress(body) else { return };
    let clean_url = canonical_cache_url(url);
    let cache_key = format!("GET:{source}:{clean_url}");
    let now = chrono::Utc::now().timestamp();
    let _ = with_raw(|conn| {
        conn.prepare_cached(
            "INSERT OR REPLACE INTO raw_http_cache (cache_key, source, url, status, body_zst, fetched_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?
        .execute(params![cache_key, source, clean_url, i64::from(status), zst, now])?;
        Ok(())
    });
}

/// True if a PreDB feed URL is a static archive dump whose contents do not roll over as new releases arrive.
/// Rolling numbered pages (`predb_club`, `predb_net`, `xrel`, etc.) are always fetched live during polling
/// and archived in `raw_predb_log` for replay.
pub fn is_immutable_predb_page(source_key: &str, _page: u32) -> bool {
    matches!(source_key, "nswdb" | "3dsdb" | "nzedb_archive" | "dats_pciso_1998")
}

/// Returns a forever-cached PreDB response if `is_immutable_predb_page(source_key, page)` is true.
pub fn get_predb_page_cache(source_key: &str, url: &str, page: u32) -> Option<String> {
    if !is_immutable_predb_page(source_key, page) {
        return None;
    }
    get_http_cache(source_key, url).and_then(|(status, body)| (status == 200).then_some(body))
}

/// Archives every HTTP 200 PreDB payload in `raw_predb_log` (deduplicated by SHA-256) and, if it is a
/// static dump, also caches it in `raw_http_cache`.
pub fn record_predb_page(source_id: i64, source_key: &str, url: &str, page: u32, body: &str) {
    let Some(zst) = compress(body) else { return };
    let sha256 = Sha256::digest(body.as_bytes()).to_vec();
    let clean_url = canonical_cache_url(url);
    let now = chrono::Utc::now().timestamp();
    let cache_forever = is_immutable_predb_page(source_key, page);
    let _ = with_raw(|conn| {
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

/// Persists an OpenSubtitles `moviehash` lookup result into `raw_opensubtitles`.
pub fn put_opensubtitles(moviehash: &str, feature: Option<&Feature>, raw_json: Option<&str>) {
    let f = feature.cloned().unwrap_or_default();
    let zst = raw_json.and_then(compress);
    let now = chrono::Utc::now().timestamp();
    let _ = with_raw(|conn| {
        conn.prepare_cached(
            "INSERT INTO raw_opensubtitles
             (moviehash, imdb_id, tmdb_id, kind, title, year, season, episode, release_name, raw_json_zst, checked_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT(moviehash) DO UPDATE SET
                imdb_id = coalesce(excluded.imdb_id, main.raw_opensubtitles.imdb_id),
                tmdb_id = coalesce(excluded.tmdb_id, main.raw_opensubtitles.tmdb_id),
                kind = coalesce(excluded.kind, main.raw_opensubtitles.kind),
                title = coalesce(excluded.title, main.raw_opensubtitles.title),
                year = coalesce(excluded.year, main.raw_opensubtitles.year),
                season = coalesce(excluded.season, main.raw_opensubtitles.season),
                episode = coalesce(excluded.episode, main.raw_opensubtitles.episode),
                release_name = coalesce(excluded.release_name, main.raw_opensubtitles.release_name),
                raw_json_zst = coalesce(excluded.raw_json_zst, main.raw_opensubtitles.raw_json_zst),
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
            zst,
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

/// Persists a probed release's `ProbeInfo` JSON, completeness/stamp metadata, optional NFO password, and
/// optional cover reference into `raw_probes`, `raw_nfo_passwords`, and `raw_covers`.
pub fn put_probe(rec: &ProbeRecord<'_>) {
    let now = chrono::Utc::now().timestamp();
    let _ = with_raw(|conn| {
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

fn has_table(conn: &Connection, schema: &str, table: &str) -> rusqlite::Result<bool> {
    conn.prepare(&format!("SELECT 1 FROM {schema}.sqlite_master WHERE type = 'table' AND name = ?1"))?
        .exists([table])
}

fn harvest_attached_main(conn: &Connection) -> rusqlite::Result<()> {
    let tx = conn.unchecked_transaction()?;
    if has_table(&tx, "main_db", "backfill_deepest")? {
        tx.execute_batch(
            "INSERT OR REPLACE INTO main.raw_backfill_deepest (grp, server, oldest_at, runner_up_at)
             SELECT grp, server, oldest_at, runner_up_at FROM main_db.backfill_deepest;",
        )?;
    }
    if has_table(&tx, "main_db", "groups")? {
        tx.execute_batch(
            "INSERT OR REPLACE INTO main.raw_group_bounds (name, first_article, last_article, low_article, low_posted, high_article, high_posted)
             SELECT name, first_article, last_article, low_article, low_posted, high_article, high_posted FROM main_db.groups
             WHERE low_article IS NOT NULL OR high_article IS NOT NULL OR first_article IS NOT NULL;",
        )?;
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
    if has_table(&tx, "sh", "release_opensubtitles")? {
        tx.execute_batch(
            "INSERT INTO main.raw_opensubtitles (moviehash, imdb_id, tmdb_id, kind, title, year, season, episode, release_name, checked_at)
             SELECT moviehash, imdb_id, tmdb_id, kind, title, year, season, episode, release_name, checked_at
             FROM sh.release_opensubtitles WHERE moviehash IS NOT NULL AND moviehash != ''
             ON CONFLICT(moviehash) DO UPDATE SET
                 imdb_id = coalesce(main.raw_opensubtitles.imdb_id, excluded.imdb_id),
                 tmdb_id = coalesce(main.raw_opensubtitles.tmdb_id, excluded.tmdb_id),
                 kind = coalesce(main.raw_opensubtitles.kind, excluded.kind),
                 title = coalesce(main.raw_opensubtitles.title, excluded.title),
                 year = coalesce(main.raw_opensubtitles.year, excluded.year),
                 season = coalesce(main.raw_opensubtitles.season, excluded.season),
                 episode = coalesce(main.raw_opensubtitles.episode, excluded.episode),
                 release_name = coalesce(main.raw_opensubtitles.release_name, excluded.release_name),
                 checked_at = max(main.raw_opensubtitles.checked_at, excluded.checked_at);",
        )?;
    }
    if has_table(&tx, "sh", "release_probe")? {
        let probe_stamp: Option<i64> = if has_table(&tx, "sh", "meta")? {
            tx.query_row("SELECT value FROM sh.meta WHERE key = 'pp_stamp_4'", [], |r| r.get(0)).optional()?
        } else {
            None
        };
        tx.execute(
            "INSERT OR REPLACE INTO main.raw_probes
             (name, group_name, crc32, agent, info, moviehash, parts, complete, config_stamp)
             SELECT r.name, r.group_name, p.crc32, p.agent, p.info, p.moviehash,
                    coalesce(r.parts, 0), coalesce(r.complete, 0), ?1
             FROM sh.release_probe p JOIN sh.releases r ON r.id = p.release_id
             WHERE r.name IS NOT NULL AND r.group_name IS NOT NULL",
            params![probe_stamp],
        )?;
    }
    if has_table(&tx, "sh", "release_cover")? {
        tx.execute_batch(
            "INSERT OR REPLACE INTO main.raw_covers (name, group_name, mime, sha256)
             SELECT r.name, r.group_name, c.mime, c.sha256
             FROM sh.release_cover c JOIN sh.releases r ON r.id = c.release_id
             WHERE r.name IS NOT NULL AND r.group_name IS NOT NULL;",
        )?;
    }
    if has_table(&tx, "sh", "archive_release")? {
        tx.execute_batch(
            "INSERT OR REPLACE INTO main.raw_archives (name, group_name, password, encryption, names_hidden, password_check)
             SELECT r.name, r.group_name, a.password, a.encryption, a.names_hidden, a.password_check
             FROM sh.archive_release a JOIN sh.releases r ON r.id = a.release_id
             WHERE r.name IS NOT NULL AND r.group_name IS NOT NULL;",
        )?;
    }
    if has_table(&tx, "sh", "archive_inner_files")? {
        tx.execute_batch(
            "INSERT OR REPLACE INTO main.raw_archive_inner_files (release_name, group_name, seq, name, size, source)
             SELECT r.name, r.group_name, f.seq, f.name, f.size, f.source
             FROM sh.archive_inner_files f JOIN sh.releases r ON r.id = f.release_id
             WHERE r.name IS NOT NULL AND r.group_name IS NOT NULL;",
        )?;
    }
    if has_table(&tx, "sh", "release_nfo_password")? {
        tx.execute_batch(
            "INSERT OR REPLACE INTO main.raw_nfo_passwords (name, group_name, password)
             SELECT r.name, r.group_name, np.password
             FROM sh.release_nfo_password np JOIN sh.releases r ON r.id = np.release_id
             WHERE r.name IS NOT NULL AND r.group_name IS NOT NULL;",
        )?;
    }
    if has_table(&tx, "sh", "release_stills")? {
        tx.execute_batch(
            "INSERT OR REPLACE INTO main.raw_stills (name, group_name, position, at_ms, sha256, width, height, phash)
             SELECT r.name, r.group_name, s.position, s.at_ms, s.sha256, s.width, s.height, s.phash
             FROM sh.release_stills s JOIN sh.releases r ON r.id = s.release_id
             WHERE r.name IS NOT NULL AND r.group_name IS NOT NULL;",
        )?;
    }
    tx.commit()
}

/// Harvests natural-keyed enrichments from `main` (`atlas.db` + `atlas.s0..s7.db`) into `atlas.raw.db`.
/// Any SQL or transaction failure rolls back the failed harvest and returns `Err` so callers never delete
/// source databases after a partial or failed harvest.
pub fn harvest_from_shards(main: &Path, log: &dyn Fn(&str)) -> anyhow::Result<()> {
    let raw_path = path_for(main);
    let conn = create(&raw_path).with_context(|| format!("opening {}", raw_path.display()))?;
    if main.exists() {
        log(&format!("harvesting watermarks from {}...", main.display()));
        conn.execute("ATTACH DATABASE ?1 AS main_db", [main.to_string_lossy().as_ref()])
            .with_context(|| format!("attaching {}", main.display()))?;
        let res = harvest_attached_main(&conn);
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
pub fn run_replay_predb(args: &[String]) -> i32 {
    let apply = args.iter().any(|a| a == "--apply");
    let main = crate::paths::database();
    let raw_path = path_for(&main);
    let predb_path = crate::predb::db::path_for(&main);
    let Ok(raw_conn) = create(&raw_path) else {
        println!("couldnt open {}", raw_path.display());
        return 1;
    };
    let Ok(mut predb_conn) = crate::predb::db::create(&predb_path) else {
        println!("couldnt open {}", predb_path.display());
        return 1;
    };
    let rows: Vec<(i64, String, i64, Vec<u8>, i64)> = raw_conn
        .prepare("SELECT source_id, source_key, page, body_zst, fetched_at FROM raw_predb_log ORDER BY id")
        .and_then(|mut s| {
            s.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()
        })
        .unwrap_or_default();
    let mut parsed_entries = 0usize;
    let mut inserted = 0u64;
    let mut updated = 0u64;
    for (source_id, key, page, zst, fetched_at) in &rows {
        let Some(spec) = crate::predb::feeds::spec(key) else { continue };
        let Some(body) = decompress(zst) else { continue };
        let page_num = u32::try_from(*page).unwrap_or(1).max(1);
        let entries = match spec.parse {
            crate::predb::feeds::Parser::Json(parse) => {
                serde_json::from_str::<serde_json::Value>(&body).ok().and_then(|v| parse(&v, page_num).ok())
            }
            crate::predb::feeds::Parser::Text(parse) => parse(&body).ok(),
        };
        let Some(entries) = entries else { continue };
        parsed_entries += entries.len();
        if apply && let Ok(c) = crate::predb::import::import(&mut predb_conn, *source_id, &entries, *fetched_at, None) {
            inserted += c.inserted;
            updated += c.updated;
        }
    }
    if apply {
        println!(
            "replayed {} raw predb payloads ({} entries): {} inserted, {} updated",
            rows.len(),
            parsed_entries,
            inserted,
            updated
        );
    } else {
        println!(
            "dry run (pass --apply to write): {} raw predb payloads yield {} parsed entries",
            rows.len(),
            parsed_entries
        );
    }
    0
}

/// Safely harvests natural-keyed shard data into `atlas.raw.db` and resets only `atlas.db` and `atlas.s0..s7.db`,
/// keeping `atlas.raw.db`, `atlas.predb.db`, `atlas.media.db`, `atlas.details.db`, `posters/`, `covers/`, and `stills/`.
pub fn reset_shards_at(main: &Path, log: &dyn Fn(&str)) -> anyhow::Result<()> {
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
        for suffix in ["", "-wal", "-shm", ".sealed-log-cursor"] {
            let p = PathBuf::from(format!("{}{suffix}", base.display()));
            match std::fs::remove_file(&p) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e).with_context(|| format!("removing {}", p.display())),
            }
        }
        Ok(())
    };
    for i in 0..crate::store::SHARDS {
        remove_db_family(&crate::store::shard_path(main, i))?;
    }
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
    fn raw_store_roundtrips_http_predb_opensubtitles_and_probes() {
        let dir = tempfile::tempdir().unwrap();
        let raw_path = dir.path().join("atlas.raw.db");
        set_test_raw_path(Some(raw_path));

        put_http_cache("tmdb", "https://api.themoviedb.org/3/movie/603?api_key=SECRET", 200, r#"{"id":603}"#);
        assert_eq!(
            get_http_cache("tmdb", "https://api.themoviedb.org/3/movie/603?api_key=OTHER_KEY"),
            Some((200, r#"{"id":603}"#.to_string()))
        );

        // Rolling PreDB feeds are logged in raw_predb_log for replay, never served as immutable HTTP cache hits.
        record_predb_page(1, "predb_club", "https://predb.club/api/v1/?page=2", 2, r#"{"status":"success"}"#);
        assert_eq!(get_predb_page_cache("predb_club", "https://predb.club/api/v1/?page=2", 2), None);

        // Static archive dumps are cached forever in raw_http_cache.
        record_predb_page(30, "nswdb", "https://nswdb.com/xml", 1, "<releases/>");
        assert_eq!(get_predb_page_cache("nswdb", "https://nswdb.com/xml", 1), Some("<releases/>".to_string()));

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
        put_opensubtitles("8e245d9679d31e12", Some(&feat), Some(r#"{"data":[]}"#));
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
        // Matching completeness, parts, and stamp -> cache hit with cover and NFO password restored.
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

        set_test_raw_path(None);
    }

    #[test]
    fn reset_shards_aborts_without_deleting_when_a_shard_harvest_fails() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        crate::db::create_db_at(&main).unwrap();
        // Corrupt shard s0 by replacing its releases table so harvesting fails.
        let s0 = crate::store::shard_path(&main, 0);
        let conn = Connection::open(&s0).unwrap();
        conn.execute_batch("DROP TABLE releases;").unwrap();
        drop(conn);

        let err = reset_shards_at(&main, &|_| {}).expect_err("harvest must fail when releases table is missing");
        assert!(format!("{err:#}").contains("s0"), "{err:#}");
        assert!(main.exists(), "main db must not be deleted on failed harvest");
        assert!(s0.exists(), "shard s0 must not be deleted on failed harvest");
    }
}
