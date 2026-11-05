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

use std::collections::BTreeMap;
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
CREATE TABLE IF NOT EXISTS raw_meta (
    key   TEXT PRIMARY KEY,
    value INTEGER NOT NULL DEFAULT 0
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS raw_http_cache (
    cache_key   TEXT PRIMARY KEY,
    source      TEXT NOT NULL,
    url         TEXT NOT NULL,
    status      INTEGER NOT NULL,
    body_zst    BLOB NOT NULL,
    fetched_at  INTEGER NOT NULL
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS idx_raw_http_cache_fetched_at ON raw_http_cache(fetched_at);

CREATE TRIGGER IF NOT EXISTS raw_http_cache_ai AFTER INSERT ON raw_http_cache BEGIN
    INSERT INTO raw_meta (key, value) VALUES ('http_rows', 1)
        ON CONFLICT(key) DO UPDATE SET value = value + 1;
    INSERT INTO raw_meta (key, value) VALUES ('http_bytes', length(new.body_zst))
        ON CONFLICT(key) DO UPDATE SET value = value + excluded.value;
END;
CREATE TRIGGER IF NOT EXISTS raw_http_cache_ad AFTER DELETE ON raw_http_cache BEGIN
    UPDATE raw_meta SET value = max(0, value - 1) WHERE key = 'http_rows';
    UPDATE raw_meta SET value = max(0, value - length(old.body_zst)) WHERE key = 'http_bytes';
END;
CREATE TRIGGER IF NOT EXISTS raw_http_cache_au AFTER UPDATE ON raw_http_cache BEGIN
    UPDATE raw_meta SET value = max(0, value + length(new.body_zst) - length(old.body_zst)) WHERE key = 'http_bytes';
END;

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

CREATE TABLE IF NOT EXISTS raw_predb_events (
    seq        INTEGER PRIMARY KEY,
    log_id     INTEGER NOT NULL,
    page       INTEGER NOT NULL DEFAULT 1,
    fetched_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_raw_predb_events_log_id ON raw_predb_events(log_id);

CREATE TRIGGER IF NOT EXISTS raw_predb_log_ai AFTER INSERT ON raw_predb_log BEGIN
    INSERT INTO raw_meta (key, value) VALUES ('predb_rows', 1)
        ON CONFLICT(key) DO UPDATE SET value = value + 1;
    INSERT INTO raw_meta (key, value) VALUES ('predb_bytes', length(new.body_zst))
        ON CONFLICT(key) DO UPDATE SET value = value + excluded.value;
END;
CREATE TRIGGER IF NOT EXISTS raw_predb_log_ad AFTER DELETE ON raw_predb_log BEGIN
    UPDATE raw_meta SET value = max(0, value - 1) WHERE key = 'predb_rows';
    UPDATE raw_meta SET value = max(0, value - length(old.body_zst)) WHERE key = 'predb_bytes';
    DELETE FROM raw_predb_events WHERE log_id = old.id;
END;
CREATE TRIGGER IF NOT EXISTS raw_predb_log_au AFTER UPDATE ON raw_predb_log BEGIN
    UPDATE raw_meta SET value = max(0, value + length(new.body_zst) - length(old.body_zst)) WHERE key = 'predb_bytes';
END;

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
    content_id   BLOB,
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
CREATE INDEX IF NOT EXISTS idx_raw_probes_probed_at ON raw_probes(probed_at);

CREATE TRIGGER IF NOT EXISTS raw_probes_ai AFTER INSERT ON raw_probes BEGIN
    INSERT INTO raw_meta (key, value) VALUES ('probe_rows', 1)
        ON CONFLICT(key) DO UPDATE SET value = value + 1;
    INSERT INTO raw_meta (key, value) VALUES ('probe_bytes', length(cast(new.info as blob)))
        ON CONFLICT(key) DO UPDATE SET value = value + excluded.value;
END;
CREATE TRIGGER IF NOT EXISTS raw_probes_ad AFTER DELETE ON raw_probes BEGIN
    UPDATE raw_meta SET value = max(0, value - 1) WHERE key = 'probe_rows';
    UPDATE raw_meta SET value = max(0, value - length(cast(old.info as blob))) WHERE key = 'probe_bytes';
END;
CREATE TRIGGER IF NOT EXISTS raw_probes_au AFTER UPDATE ON raw_probes BEGIN
    UPDATE raw_meta SET value = max(0, value + length(cast(new.info as blob)) - length(cast(old.info as blob))) WHERE key = 'probe_bytes';
END;

CREATE TABLE IF NOT EXISTS raw_covers (
    name       TEXT NOT NULL,
    group_name TEXT NOT NULL,
    content_id BLOB,
    mime       TEXT NOT NULL,
    sha256     TEXT NOT NULL,
    PRIMARY KEY (name, group_name)
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS raw_archives (
    name             TEXT NOT NULL,
    group_name       TEXT NOT NULL,
    content_id       BLOB,
    password         INTEGER NOT NULL,
    encryption       INTEGER,
    names_hidden     INTEGER,
    password_check   BLOB,
    parts            INTEGER,
    complete         INTEGER,
    listing_complete INTEGER,
    listing_damaged  INTEGER,
    content_fp       BLOB,
    PRIMARY KEY (name, group_name)
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS raw_par2 (
    name        TEXT NOT NULL,
    group_name  TEXT NOT NULL,
    content_id  BLOB,
    covers_all  INTEGER NOT NULL DEFAULT 0,
    posted      BLOB,
    par2_block  INTEGER,
    par2_blocks INTEGER,
    parts       INTEGER,
    complete    INTEGER,
    content_fp  BLOB,
    PRIMARY KEY (name, group_name)
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS raw_par2_files (
    name       TEXT NOT NULL,
    group_name TEXT NOT NULL,
    content_id BLOB,
    file_id    BLOB NOT NULL,
    set_id     BLOB NOT NULL,
    md5        BLOB NOT NULL,
    md5_16k    BLOB NOT NULL,
    size       INTEGER NOT NULL,
    file_name  TEXT NOT NULL,
    crc32      INTEGER,
    bound      INTEGER NOT NULL DEFAULT 1,
    sealed     INTEGER NOT NULL DEFAULT 0,
    complete   INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (name, group_name, file_id)
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS raw_archive_inner_files (
    release_name TEXT NOT NULL,
    group_name   TEXT NOT NULL,
    seq          INTEGER NOT NULL,
    content_id   BLOB,
    name         TEXT NOT NULL,
    size         INTEGER,
    source       INTEGER,
    PRIMARY KEY (release_name, group_name, seq)
) WITHOUT ROWID;

CREATE TRIGGER IF NOT EXISTS raw_archive_inner_files_ai AFTER INSERT ON raw_archive_inner_files BEGIN
    INSERT INTO raw_meta (key, value) VALUES ('archive_name_bytes', length(cast(new.name as blob)) + 32)
        ON CONFLICT(key) DO UPDATE SET value = value + excluded.value;
END;
CREATE TRIGGER IF NOT EXISTS raw_archive_inner_files_ad AFTER DELETE ON raw_archive_inner_files BEGIN
    UPDATE raw_meta SET value = max(0, value - (length(cast(old.name as blob)) + 32)) WHERE key = 'archive_name_bytes';
END;
CREATE TRIGGER IF NOT EXISTS raw_archive_inner_files_au AFTER UPDATE ON raw_archive_inner_files BEGIN
    UPDATE raw_meta SET value = max(0, value + length(cast(new.name as blob)) - length(cast(old.name as blob))) WHERE key = 'archive_name_bytes';
END;

CREATE TABLE IF NOT EXISTS raw_nfo_passwords (
    name       TEXT NOT NULL,
    group_name TEXT NOT NULL,
    content_id BLOB,
    password   TEXT NOT NULL,
    PRIMARY KEY (name, group_name)
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS raw_stills (
    name       TEXT NOT NULL,
    group_name TEXT NOT NULL,
    position   INTEGER NOT NULL,
    content_id BLOB,
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

fn raw_meta_get(conn: &Connection, key: &str) -> rusqlite::Result<i64> {
    Ok(conn
        .prepare_cached("SELECT value FROM raw_meta WHERE key = ?1")?
        .query_row([key], |r| r.get(0))
        .optional()?
        .unwrap_or(0))
}

/// Opens or creates `atlas.raw.db` with WAL mode and all permanent raw tables.
pub fn create(path: &Path) -> rusqlite::Result<Connection> {
    let conn = crate::db::create_at(path)?;
    conn.query_row("pragma journal_mode = wal", [], |_| Ok(()))?;
    conn.execute_batch("pragma synchronous = normal; pragma busy_timeout = 30000; pragma recursive_triggers = on;")?;
    conn.execute_batch(SCHEMA)?;
    ensure_column(&conn, "raw_predb_log", "page", "INTEGER NOT NULL DEFAULT 1")?;
    ensure_column(&conn, "raw_probes", "content_id", "BLOB")?;
    ensure_column(&conn, "raw_probes", "parts", "INTEGER")?;
    ensure_column(&conn, "raw_probes", "complete", "INTEGER")?;
    ensure_column(&conn, "raw_probes", "config_stamp", "INTEGER")?;
    ensure_column(&conn, "raw_covers", "content_id", "BLOB")?;
    ensure_column(&conn, "raw_archives", "content_id", "BLOB")?;
    ensure_column(&conn, "raw_archives", "parts", "INTEGER")?;
    ensure_column(&conn, "raw_archives", "complete", "INTEGER")?;
    ensure_column(&conn, "raw_archives", "listing_complete", "INTEGER")?;
    ensure_column(&conn, "raw_archives", "listing_damaged", "INTEGER")?;
    ensure_column(&conn, "raw_archives", "content_fp", "BLOB")?;
    ensure_column(&conn, "raw_par2", "content_id", "BLOB")?;
    ensure_column(&conn, "raw_par2", "content_fp", "BLOB")?;
    ensure_column(&conn, "raw_archive_inner_files", "content_id", "BLOB")?;
    ensure_column(&conn, "raw_nfo_passwords", "content_id", "BLOB")?;
    ensure_column(&conn, "raw_stills", "content_id", "BLOB")?;
    let seeded = raw_meta_get(&conn, "seeded")?;
    if seeded < 2 {
        conn.execute_batch(
            "DROP TRIGGER IF EXISTS raw_predb_log_ad;
             CREATE TRIGGER raw_predb_log_ad AFTER DELETE ON raw_predb_log BEGIN
                 UPDATE raw_meta SET value = max(0, value - 1) WHERE key = 'predb_rows';
                 UPDATE raw_meta SET value = max(0, value - length(old.body_zst)) WHERE key = 'predb_bytes';
                 DELETE FROM raw_predb_events WHERE log_id = old.id;
             END;
             DROP TRIGGER IF EXISTS raw_probes_ai;
             CREATE TRIGGER raw_probes_ai AFTER INSERT ON raw_probes BEGIN
                 INSERT INTO raw_meta (key, value) VALUES ('probe_rows', 1)
                     ON CONFLICT(key) DO UPDATE SET value = value + 1;
                 INSERT INTO raw_meta (key, value) VALUES ('probe_bytes', length(cast(new.info as blob)))
                     ON CONFLICT(key) DO UPDATE SET value = value + excluded.value;
             END;
             DROP TRIGGER IF EXISTS raw_probes_ad;
             CREATE TRIGGER raw_probes_ad AFTER DELETE ON raw_probes BEGIN
                 UPDATE raw_meta SET value = max(0, value - 1) WHERE key = 'probe_rows';
                 UPDATE raw_meta SET value = max(0, value - length(cast(old.info as blob))) WHERE key = 'probe_bytes';
             END;
             DROP TRIGGER IF EXISTS raw_probes_au;
             CREATE TRIGGER raw_probes_au AFTER UPDATE ON raw_probes BEGIN
                 UPDATE raw_meta SET value = max(0, value + length(cast(new.info as blob)) - length(cast(old.info as blob))) WHERE key = 'probe_bytes';
             END;
             INSERT INTO raw_predb_events (log_id, page, fetched_at)
                 SELECT l.id, l.page, l.fetched_at FROM raw_predb_log l
                 WHERE NOT EXISTS (SELECT 1 FROM raw_predb_events e WHERE e.log_id = l.id)
                 ORDER BY l.id ASC;
             INSERT OR REPLACE INTO raw_meta (key, value)
                 SELECT 'probe_rows', count(*) FROM raw_probes;
             INSERT OR REPLACE INTO raw_meta (key, value)
                 SELECT 'probe_bytes', coalesce(sum(length(cast(info as blob))), 0) FROM raw_probes;
             INSERT OR REPLACE INTO raw_meta (key, value)
                 SELECT 'http_rows', count(*) FROM raw_http_cache;
             INSERT OR REPLACE INTO raw_meta (key, value)
                 SELECT 'http_bytes', coalesce(sum(length(body_zst)), 0) FROM raw_http_cache;
             INSERT OR REPLACE INTO raw_meta (key, value)
                 SELECT 'predb_rows', count(*) FROM raw_predb_log;
             INSERT OR REPLACE INTO raw_meta (key, value)
                 SELECT 'predb_bytes', coalesce(sum(length(body_zst)), 0) FROM raw_predb_log;
             INSERT OR REPLACE INTO raw_meta (key, value)
                 SELECT 'archive_name_bytes', coalesce(sum(length(cast(name as blob)) + 32), 0) FROM raw_archive_inner_files;
             INSERT OR REPLACE INTO raw_meta (key, value) VALUES ('seeded', 2);",
        )?;
    }
    Ok(conn)
}

/// Connections to the raw database, each for one caller at a time. It was one: every body step of every net claim
/// waited on it, ~180 threads at 24 claims a shard. WAL lets the others read while one writes.
#[cfg(not(test))]
const RAW_CONNS: usize = 16;

#[cfg(not(test))]
type RawSlot = Mutex<Option<(PathBuf, Connection)>>;

#[cfg(not(test))]
static GLOBAL_CONNS: OnceLock<Vec<RawSlot>> = OnceLock::new();

/// One open at a time, soo two connections don't both run the schema's one-time changes.
#[cfg(not(test))]
static OPENING: Mutex<()> = Mutex::new(());

/// A free connection, or when all are busy, one by turn to wait on.
#[cfg(not(test))]
fn raw_slot() -> std::sync::MutexGuard<'static, Option<(PathBuf, Connection)>> {
    use std::sync::TryLockError;
    use std::sync::atomic::{AtomicUsize, Ordering};
    static TURN: AtomicUsize = AtomicUsize::new(0);
    let slots = GLOBAL_CONNS.get_or_init(|| (0..RAW_CONNS).map(|_| Mutex::new(None)).collect());
    for slot in slots {
        match slot.try_lock() {
            Ok(guard) => return guard,
            Err(TryLockError::Poisoned(e)) => return e.into_inner(),
            Err(TryLockError::WouldBlock) => {}
        }
    }
    let slot = &slots[TURN.fetch_add(1, Ordering::Relaxed) % RAW_CONNS];
    slot.lock().unwrap_or_else(|e| e.into_inner())
}

thread_local! {
    static SHARD_RAW_PATH: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
    #[cfg(test)]
    static TEST_RAW_PATH: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub fn set_test_raw_path(path: Option<PathBuf>) {
    TEST_RAW_PATH.with(|cell| *cell.borrow_mut() = path);
}

/// Derives the sibling `atlas.raw.db` path from a shard or main database path (`.../atlas.s3.db` -> `.../atlas.raw.db`).
pub fn raw_path_from_shard(shard: &Path) -> Option<PathBuf> {
    let file = shard.file_name()?.to_str()?;
    let stem = file.strip_suffix(".db")?;
    if let Some((base, shard_no)) = stem.rsplit_once(".s")
        && !shard_no.is_empty()
        && shard_no.bytes().all(|c| c.is_ascii_digit())
    {
        return Some(shard.with_file_name(format!("{base}.raw.db")));
    }
    Some(shard.with_file_name(format!("{stem}.raw.db")))
}

// the test build returns early from its own block, which clippy reads as a needless return
#[allow(clippy::needless_return)]
fn with_raw<T>(f: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> Option<T> {
    #[cfg(test)]
    {
        let path = TEST_RAW_PATH
            .with(|cell| cell.borrow().clone())
            .or_else(|| SHARD_RAW_PATH.with(|cell| cell.borrow().clone()))?;
        let conn = create(&path).ok()?;
        return f(&conn).ok();
    }
    #[cfg(not(test))]
    {
        let path = SHARD_RAW_PATH
            .with(|cell| cell.borrow().clone())
            .unwrap_or_else(|| path_for(&crate::paths::database()));
        let mut guard = raw_slot();
        let needs_open = guard.as_ref().is_none_or(|(cached_path, _)| cached_path != &path);
        if needs_open {
            let _opening = OPENING.lock().unwrap_or_else(|e| e.into_inner());
            *guard = create(&path).ok().map(|conn| (path, conn));
        }
        let (_, conn) = guard.as_ref()?;
        f(conn).ok()
    }
}

/// `with_raw` in a transaction that takes the write lock first: with more than one connection, one that read first
/// and wrote after another's commit would fail at once instead of waiting its turn.
fn with_raw_tx<T>(f: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> Option<T> {
    with_raw(|conn| {
        let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)?;
        let out = f(&tx)?;
        tx.commit()?;
        Ok(out)
    })
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
    let mut count = raw_meta_get(conn, "http_rows")?;
    let mut bytes = raw_meta_get(conn, "http_bytes")?;
    if count >= max_rows || bytes + incoming_bytes > max_bytes {
        conn.execute(
            "DELETE FROM raw_http_cache
             WHERE source NOT IN ('nzedb_archive', 'dats_pciso_1998')
               AND fetched_at < ?1",
            params![now.saturating_sub(LIVE_HTTP_CACHE_TTL_SECS)],
        )?;
        count = raw_meta_get(conn, "http_rows")?;
        bytes = raw_meta_get(conn, "http_bytes")?;
    }
    if count >= max_rows {
        let to_drop = (count - max_rows + 1).max(64);
        conn.execute(
            "DELETE FROM raw_http_cache WHERE cache_key IN (
                SELECT cache_key FROM raw_http_cache ORDER BY fetched_at ASC LIMIT ?1
            )",
            params![to_drop],
        )?;
        bytes = raw_meta_get(conn, "http_bytes")?;
    }
    while bytes + incoming_bytes > max_bytes {
        let deleted = conn.execute(
            "DELETE FROM raw_http_cache WHERE cache_key IN (
                SELECT cache_key FROM raw_http_cache ORDER BY fetched_at ASC LIMIT 64
            )",
            [],
        )?;
        if deleted == 0 {
            break;
        }
        bytes = raw_meta_get(conn, "http_bytes")?;
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
    let _ = with_raw_tx(|conn| {
        conn.prepare_cached("DELETE FROM raw_http_cache WHERE cache_key = ?1")?
            .execute(params![&cache_key])?;
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
    let rows = raw_meta_get(conn, "predb_rows")?;
    let mut bytes = raw_meta_get(conn, "predb_bytes")?;
    if rows >= max_rows {
        let drop_rows = (rows - max_rows + 1).max(1);
        conn.execute(
            "DELETE FROM raw_predb_log WHERE id IN (SELECT id FROM raw_predb_log ORDER BY id ASC LIMIT ?1)",
            params![drop_rows],
        )?;
        bytes = raw_meta_get(conn, "predb_bytes")?;
    }
    while bytes + incoming_bytes > max_bytes {
        let deleted = conn.execute(
            "DELETE FROM raw_predb_log WHERE id IN (SELECT id FROM raw_predb_log ORDER BY id ASC LIMIT 16)",
            [],
        )?;
        if deleted == 0 {
            break;
        }
        bytes = raw_meta_get(conn, "predb_bytes")?;
    }
    Ok(())
}

/// Archives a validated, non-empty PreDB payload in `raw_predb_log` (deduplicated by SHA-256 and bounded
/// by `MAX_PREDB_LOG_ROWS` / `MAX_PREDB_LOG_BYTES`), records its polling observation order in `raw_predb_events`
/// whenever the page's payload changes, and, if it is a static archive snapshot, also caches it in `raw_http_cache`.
pub fn record_predb_page(source_id: i64, source_key: &str, url: &str, page: u32, body: &str) {
    let Some(zst) = compress(body) else { return };
    let incoming_bytes = zst.len() as i64;
    if incoming_bytes > MAX_PREDB_LOG_BYTES {
        return;
    }
    let sha256 = Sha256::digest(body.as_bytes()).to_vec();
    let clean_url = canonical_cache_url(url);
    let now = chrono::Utc::now().timestamp();
    let page_i64 = i64::from(page);
    let cache_forever = is_immutable_predb_page(source_key, page);
    let _ = with_raw_tx(|conn| {
        let existing_id: Option<i64> = conn
            .prepare_cached("SELECT id FROM raw_predb_log WHERE source_id = ?1 AND body_sha256 = ?2")?
            .query_row(params![source_id, &sha256], |r| r.get(0))
            .optional()?;
        let log_id = match existing_id {
            Some(id) => id,
            None => {
                prune_predb_log(conn, incoming_bytes, MAX_PREDB_LOG_ROWS, MAX_PREDB_LOG_BYTES)?;
                conn.prepare_cached(
                    "INSERT INTO raw_predb_log (source_id, source_key, url, page, body_sha256, body_zst, fetched_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                )?
                .execute(params![source_id, source_key, &clean_url, page_i64, &sha256, &zst, now])?;
                conn.last_insert_rowid()
            }
        };
        let last_event_log_id: Option<i64> = conn
            .prepare_cached(
                "SELECT e.log_id FROM raw_predb_events e
                 JOIN raw_predb_log l ON l.id = e.log_id
                 WHERE l.source_id = ?1 AND e.page = ?2
                 ORDER BY e.seq DESC LIMIT 1",
            )?
            .query_row(params![source_id, page_i64], |r| r.get(0))
            .optional()?;
        if last_event_log_id != Some(log_id) {
            conn.prepare_cached(
                "DELETE FROM raw_predb_events
                 WHERE log_id = ?1
                   AND seq > (SELECT MIN(e2.seq) FROM raw_predb_events e2 WHERE e2.log_id = ?1)",
            )?
            .execute(params![log_id])?;
            conn.prepare_cached("INSERT INTO raw_predb_events (log_id, page, fetched_at) VALUES (?1, ?2, ?3)")?
                .execute(params![log_id, page_i64, now])?;
        }
        if cache_forever && incoming_bytes <= MAX_HTTP_CACHE_BYTES {
            let cache_key = format!("GET:{source_key}:{clean_url}");
            conn.prepare_cached("DELETE FROM raw_http_cache WHERE cache_key = ?1")?
                .execute(params![&cache_key])?;
            prune_http_cache(conn, incoming_bytes, MAX_HTTP_CACHE_ROWS, MAX_HTTP_CACHE_BYTES, now)?;
            conn.prepare_cached(
                "INSERT OR REPLACE INTO raw_http_cache (cache_key, source, url, status, body_zst, fetched_at)
                 VALUES (?1, ?2, ?3, 200, ?4, ?5)",
            )?
            .execute(params![cache_key, source_key, clean_url, zst, now])?;
        }
        Ok(())
    });
}

fn first_segment_mid_in(
    conn: &Connection,
    schema: &str,
    file_id: i64,
    blob: Option<&[u8]>,
) -> rusqlite::Result<String> {
    let mut best: Option<(Option<i64>, String)> = None;
    let mut consider = |part: Option<i64>, mid: String| {
        if mid.is_empty() {
            return;
        }
        let replace = match &best {
            Some((bp, bm)) => crate::store::subject_key(part, &mid) < crate::store::subject_key(*bp, bm),
            None => true,
        };
        if replace {
            best = Some((part, mid));
        }
    };
    if has_table(conn, schema, "segments")? {
        let has_domains = has_table(conn, schema, "domains")?;
        let sql = if has_domains {
            format!(
                "SELECT s.part, s.local, d.suffix
                 FROM {schema}.segments s
                 LEFT JOIN {schema}.domains d ON d.id = s.domain
                 WHERE s.file_id = ?1"
            )
        } else {
            format!(
                "SELECT s.part, s.local, NULL
                 FROM {schema}.segments s
                 WHERE s.file_id = ?1"
            )
        };
        let mut stmt = conn.prepare_cached(&sql)?;
        let mut rows = stmt.query([file_id])?;
        while let Some(r) = rows.next()? {
            let part: Option<i64> = r.get(0)?;
            let local: Vec<u8> = r.get(1)?;
            let suffix: Option<String> = r.get(2)?;
            consider(part, crate::store::decode(&local, suffix.as_deref()));
        }
    }
    if let Some(bytes) = blob
        && let Ok(segs) = crate::blob::decode_capped(bytes, crate::store::READ_MAX_SEGMENTS as usize)
    {
        let has_domains = has_table(conn, schema, "domains")?;
        for seg in segs {
            let suffix: Option<String> = if seg.domain != 0 && has_domains {
                conn.prepare_cached(&format!("SELECT suffix FROM {schema}.domains WHERE id = ?1"))?
                    .query_row([seg.domain], |r| r.get(0))
                    .optional()?
            } else {
                None
            };
            consider(seg.part, crate::store::decode(&seg.local, suffix.as_deref()));
        }
    }
    Ok(best.map(|(_, mid)| mid).unwrap_or_default())
}

fn hash_file_fields_into(
    hasher: &mut Sha256,
    filename: &str,
    first_mid: &str,
    expected: Option<i64>,
    seen: &[u8],
) {
    hasher.update((filename.len() as u64).to_le_bytes());
    hasher.update(filename.as_bytes());
    hasher.update((first_mid.len() as u64).to_le_bytes());
    hasher.update(first_mid.as_bytes());
    hasher.update(expected.unwrap_or(-1).to_le_bytes());
    hasher.update((seen.len() as u64).to_le_bytes());
    hasher.update(seen);
}

#[allow(clippy::too_many_arguments)]
fn hash_file_row_into(
    hasher: &mut Sha256,
    conn: &Connection,
    schema: &str,
    file_id: i64,
    filename: &str,
    subject_mid: Option<&str>,
    expected: Option<i64>,
    seen: &[u8],
    blob: Option<&[u8]>,
) -> rusqlite::Result<()> {
    let resolved;
    let first_mid = match subject_mid.filter(|m| !m.is_empty()) {
        Some(m) => m,
        None => {
            resolved = first_segment_mid_in(conn, schema, file_id, blob)?;
            resolved.as_str()
        }
    };
    hash_file_fields_into(hasher, filename, first_mid, expected, seen);
    Ok(())
}

/// Computes a deterministic SHA-256 content identity for `release_id` in `schema` from its ordered
/// files and their first article message-IDs (`subject_mid` / first segment), expected part count,
/// and seen-parts bitmap (folded identically from `articles` in legacy pre-sharding databases).
/// Returns `None` if the release has no indexed files or articles.
pub fn release_content_id_in(conn: &Connection, schema: &str, release_id: i64) -> Option<Vec<u8>> {
    if has_table(conn, schema, "files").unwrap_or(false) {
        type FileRow = (i64, String, Option<String>, Option<i64>, Vec<u8>, Option<Vec<u8>>);
        let sql = format!(
            "SELECT id, filename, subject_mid, expected, seen, blob
             FROM {schema}.files WHERE release_id = ?1 ORDER BY filename, id"
        );
        let mut stmt = conn.prepare_cached(&sql).ok()?;
        let rows: Vec<FileRow> = stmt
            .query_map([release_id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?))
            })
            .ok()?
            .collect::<rusqlite::Result<Vec<_>>>()
            .ok()?;
        if !rows.is_empty() {
            let mut hasher = Sha256::new();
            for (file_id, filename, subject_mid, expected, seen, blob) in &rows {
                hash_file_row_into(
                    &mut hasher,
                    conn,
                    schema,
                    *file_id,
                    filename,
                    subject_mid.as_deref(),
                    *expected,
                    seen,
                    blob.as_deref(),
                )
                .ok()?;
            }
            return Some(hasher.finalize().to_vec());
        }
    }
    if has_table(conn, schema, "articles").unwrap_or(false) {
        let sql = format!(
            "SELECT coalesce(filename, ''), coalesce(message_id, ''), part, total_parts
             FROM {schema}.articles WHERE release_id = ?1"
        );
        let mut stmt = conn.prepare_cached(&sql).ok()?;
        let mut rows = stmt.query([release_id]).ok()?;
        let mut files: BTreeMap<String, crate::store::FileState> = BTreeMap::new();
        while let Some(r) = rows.next().ok()? {
            let filename: String = r.get(0).ok()?;
            let mid: String = r.get(1).ok()?;
            if mid.is_empty() {
                continue;
            }
            let part: Option<i64> = r.get(2).ok()?;
            let total_parts: Option<i64> = r.get(3).ok()?;
            files.entry(filename).or_default().add(&crate::parser::Article {
                message_id: mid,
                part,
                total_parts,
                ..crate::parser::Article::default()
            });
        }
        if !files.is_empty() {
            let mut hasher = Sha256::new();
            for (filename, f) in &files {
                hash_file_fields_into(
                    &mut hasher,
                    filename,
                    f.subject_mid.as_deref().unwrap_or(""),
                    f.expected,
                    &f.seen,
                );
            }
            return Some(hasher.finalize().to_vec());
        }
    }
    None
}

/// Computes the deterministic SHA-256 content identity for `release_id` in `main` (`conn`),
/// and binds the calling thread's raw store path to the shard's sibling `atlas.raw.db` when `conn` is on disk.
pub fn release_content_id(conn: &Connection, release_id: i64) -> Option<Vec<u8>> {
    let shard_raw = conn
        .path()
        .filter(|p| !p.is_empty())
        .and_then(|p| raw_path_from_shard(Path::new(p)));
    SHARD_RAW_PATH.with(|cell| *cell.borrow_mut() = shard_raw);
    release_content_id_in(conn, "main", release_id)
}

/// Returns `true` when `release_id` is being explicitly re-run for `step` (`pp_state.rerun_mask & mask(step) != 0`),
/// so the step re-fetches from the server instead of short-circuiting from `atlas.raw.db`.
pub fn is_explicit_rerun(conn: &Connection, release_id: i64, step: crate::postprocess::step::StepId) -> bool {
    let bit = crate::postprocess::step::mask(step);
    conn.prepare_cached("SELECT (rerun_mask & ?2) != 0 FROM pp_state WHERE release_id = ?1")
        .and_then(|mut s| s.query_row(params![release_id, bit], |r| r.get::<_, bool>(0)))
        .unwrap_or(false)
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
    let _ = with_raw_tx(|conn| {
        let exists: bool = conn
            .prepare_cached("SELECT EXISTS(SELECT 1 FROM raw_opensubtitles WHERE moviehash = ?1)")?
            .query_row(params![moviehash], |r| r.get(0))?;
        if !exists {
            prune_opensubtitles(conn, MAX_OPENSUBTITLES_ROWS)?;
        }
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

/// The group whose saved rows answer for `name` in `group_name` with `content_id`: its own when it has them, or else
/// another group's. The content id hashes every file's first message-id, parts and which of them came, soo a crosspost
/// (the same articles in several groups, likely in several shards) has the same one in each group, and what one copy's
/// articles showed holds for every copy: a post is read once, not once a group. `sql` selects `group_name` by name ?1
/// and content ?3, its own group (?2) first.
fn saved_group(
    conn: &Connection,
    sql: &str,
    name: &str,
    group_name: &str,
    content_id: &[u8],
) -> rusqlite::Result<Option<String>> {
    conn.prepare_cached(sql)?.query_row(params![name, group_name, content_id], |r| r.get(0)).optional()
}

const PROBE_GROUP: &str = "SELECT group_name FROM raw_probes WHERE name = ?1 AND content_id = ?3
     ORDER BY group_name != ?2, group_name LIMIT 1";
const NFO_PASSWORD_GROUP: &str = "SELECT group_name FROM raw_nfo_passwords WHERE name = ?1 AND content_id = ?3
     ORDER BY group_name != ?2, group_name LIMIT 1";
const STILLS_GROUP: &str = "SELECT group_name FROM raw_stills WHERE name = ?1 AND content_id = ?3
     ORDER BY group_name != ?2, group_name LIMIT 1";
const ARCHIVE_GROUP: &str = "SELECT group_name FROM raw_archives WHERE name = ?1 AND content_id = ?3
     ORDER BY group_name != ?2, group_name LIMIT 1";
const PAR2_GROUP: &str = "SELECT group_name FROM raw_par2 WHERE name = ?1 AND content_id = ?3
     ORDER BY group_name != ?2, group_name LIMIT 1";
const PAR2_FILES_GROUP: &str = "SELECT group_name FROM raw_par2_files WHERE name = ?1 AND content_id = ?3
     ORDER BY group_name != ?2, group_name LIMIT 1";

/// Looks up a previously probed release by `(name, group_name)` and `content_id` in `raw_probes`,
/// returning `Some` only if `content_id` matches, the saved probe's `config_stamp` covers `required_stamp`,
/// and its completeness/part count is not stale relative to the release's current `(parts, complete)`.
pub fn get_probe(
    name: &str,
    group_name: &str,
    content_id: &[u8],
    current_parts: i64,
    current_complete: bool,
    required_stamp: u64,
) -> Option<SavedProbe> {
    if content_id.is_empty() {
        return None;
    }
    with_raw(|conn| {
        // the probe's nfo password and cover were saved with it, under its group
        let Some(group_name) = saved_group(conn, PROBE_GROUP, name, group_name, content_id)? else {
            return Ok(None);
        };
        let group_name = group_name.as_str();
        type ProbeRow = (Option<i64>, Option<String>, String, Option<String>, Option<i64>, Option<i64>, Option<i64>);
        let probe: Option<ProbeRow> = conn
            .prepare_cached(
                "SELECT crc32, agent, info, moviehash, parts, complete, config_stamp
                 FROM raw_probes WHERE name = ?1 AND group_name = ?2 AND content_id = ?3",
            )?
            .query_row(params![name, group_name, content_id], |r| {
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
            .prepare_cached(
                "SELECT password FROM raw_nfo_passwords WHERE name = ?1 AND group_name = ?2 AND content_id = ?3",
            )?
            .query_row(params![name, group_name, content_id], |r| r.get(0))
            .optional()?;
        let cover: Option<(String, String)> = conn
            .prepare_cached(
                "SELECT mime, sha256 FROM raw_covers WHERE name = ?1 AND group_name = ?2 AND content_id = ?3",
            )?
            .query_row(params![name, group_name, content_id], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()?;
        Ok(Some(SavedProbe { crc32, agent, info, moviehash, nfo_password, cover }))
    })
    .flatten()
}

pub struct ProbeRecord<'a> {
    pub name: &'a str,
    pub group_name: &'a str,
    pub content_id: &'a [u8],
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

fn delete_oldest_probes(conn: &Connection, limit: i64) -> rusqlite::Result<usize> {
    let victims: Vec<(String, String)> = conn
        .prepare_cached("SELECT name, group_name FROM raw_probes ORDER BY probed_at ASC LIMIT ?1")?
        .query_map(params![limit], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if victims.is_empty() {
        return Ok(0);
    }
    let mut del_probe = conn.prepare_cached("DELETE FROM raw_probes WHERE name = ?1 AND group_name = ?2")?;
    let mut del_pw = conn.prepare_cached("DELETE FROM raw_nfo_passwords WHERE name = ?1 AND group_name = ?2")?;
    let mut del_cov = conn.prepare_cached("DELETE FROM raw_covers WHERE name = ?1 AND group_name = ?2")?;
    let mut deleted = 0;
    for (name, group) in &victims {
        deleted += del_probe.execute(params![name, group])?;
        del_pw.execute(params![name, group])?;
        del_cov.execute(params![name, group])?;
    }
    Ok(deleted)
}

fn prune_probes(conn: &Connection, incoming_bytes: i64, max_rows: i64, max_bytes: i64) -> rusqlite::Result<()> {
    let rows = raw_meta_get(conn, "probe_rows")?;
    let mut bytes = raw_meta_get(conn, "probe_bytes")?;
    if rows >= max_rows {
        let to_drop = (rows - max_rows + 1).max(64);
        delete_oldest_probes(conn, to_drop)?;
        bytes = raw_meta_get(conn, "probe_bytes")?;
    }
    while bytes + incoming_bytes > max_bytes {
        let deleted = delete_oldest_probes(conn, 64)?;
        if deleted == 0 {
            break;
        }
        bytes = raw_meta_get(conn, "probe_bytes")?;
    }
    Ok(())
}

fn upsert_nfo_password(
    conn: &Connection,
    name: &str,
    group_name: &str,
    content_id: &[u8],
    pw: &str,
) -> rusqlite::Result<()> {
    let exists: bool = conn
        .prepare_cached("SELECT EXISTS(SELECT 1 FROM raw_nfo_passwords WHERE name = ?1 AND group_name = ?2)")?
        .query_row(params![name, group_name], |r| r.get(0))?;
    if !exists {
        let count: i64 = conn.query_row("SELECT count(*) FROM raw_nfo_passwords", [], |r| r.get(0))?;
        if count >= MAX_RAW_PROBES_ROWS {
            let to_drop = (count - MAX_RAW_PROBES_ROWS + 1).max(64);
            conn.execute(
                "DELETE FROM raw_nfo_passwords WHERE (name, group_name) IN (
                    SELECT name, group_name FROM raw_nfo_passwords LIMIT ?1
                )",
                params![to_drop],
            )?;
        }
    }
    conn.prepare_cached(
        "INSERT OR REPLACE INTO raw_nfo_passwords (name, group_name, content_id, password) VALUES (?1, ?2, ?3, ?4)",
    )?
    .execute(params![name, group_name, content_id, pw])?;
    Ok(())
}

/// Looks up a cached NFO password for `(name, group_name)` verified against `content_id` in `raw_nfo_passwords`,
/// independent of whether `raw_probes` has a reusable probe result.
pub fn get_nfo_password(name: &str, group_name: &str, content_id: &[u8]) -> Option<String> {
    raw_nfo_password_entry(name, group_name, content_id).filter(|s| !s.is_empty())
}

/// True when `raw_nfo_passwords` holds a row (even an empty-string negative sentinel) for `(name, group_name, content_id)`.
pub fn has_nfo_password_record(name: &str, group_name: &str, content_id: &[u8]) -> bool {
    raw_nfo_password_entry(name, group_name, content_id).is_some()
}

fn raw_nfo_password_entry(name: &str, group_name: &str, content_id: &[u8]) -> Option<String> {
    if content_id.is_empty() {
        return None;
    }
    with_raw(|conn| {
        let Some(group_name) = saved_group(conn, NFO_PASSWORD_GROUP, name, group_name, content_id)? else {
            return Ok(None);
        };
        conn.prepare_cached(
            "SELECT password FROM raw_nfo_passwords WHERE name = ?1 AND group_name = ?2 AND content_id = ?3",
        )?
        .query_row(params![name, group_name, content_id], |r| r.get(0))
        .optional()
    })
    .flatten()
}

/// Persists an NFO password for `(name, group_name)` and `content_id` into `raw_nfo_passwords`.
pub fn put_nfo_password(name: &str, group_name: &str, content_id: &[u8], password: &str) {
    if content_id.is_empty() || password.len() > 256 {
        return;
    }
    let _ = with_raw_tx(|conn| upsert_nfo_password(conn, name, group_name, content_id, password));
}

fn nested_nzb_pw_key(name: &str) -> String {
    format!("{name}\0nzb")
}

/// Looks up a cached target password extracted from a nested `.nzb` inside an archive release.
pub fn get_nested_nzb_password(name: &str, group_name: &str, content_id: &[u8]) -> Option<String> {
    get_nfo_password(&nested_nzb_pw_key(name), group_name, content_id)
}

/// True when `raw_nfo_passwords` holds a nested-NZB target password record (including empty negative sentinel).
pub fn has_nested_nzb_password_record(name: &str, group_name: &str, content_id: &[u8]) -> bool {
    has_nfo_password_record(&nested_nzb_pw_key(name), group_name, content_id)
}

/// Persists a target password (or `""` negative sentinel) extracted from a nested `.nzb` inside an archive release.
pub fn put_nested_nzb_password(name: &str, group_name: &str, content_id: &[u8], password: &str) {
    put_nfo_password(&nested_nzb_pw_key(name), group_name, content_id, password);
}

/// Persists a probed release's `ProbeInfo` JSON, completeness/stamp metadata, optional NFO password, and
/// optional cover reference into `raw_probes`, `raw_nfo_passwords`, and `raw_covers`.
pub fn put_probe(rec: &ProbeRecord<'_>) {
    if rec.content_id.is_empty() {
        return;
    }
    let incoming_bytes = rec.info.len() as i64;
    if incoming_bytes > MAX_RAW_PROBES_BYTES {
        return;
    }
    let now = chrono::Utc::now().timestamp();
    let _ = with_raw_tx(|conn| {
        conn.prepare_cached("DELETE FROM raw_probes WHERE name = ?1 AND group_name = ?2")?
            .execute(params![rec.name, rec.group_name])?;
        prune_probes(conn, incoming_bytes, MAX_RAW_PROBES_ROWS, MAX_RAW_PROBES_BYTES)?;
        conn.prepare_cached(
            "INSERT OR REPLACE INTO raw_probes
             (name, group_name, content_id, crc32, agent, info, moviehash, parts, complete, config_stamp, probed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        )?
        .execute(params![
            rec.name,
            rec.group_name,
            rec.content_id,
            rec.crc32,
            rec.agent,
            rec.info,
            rec.moviehash,
            rec.parts,
            i64::from(rec.complete),
            rec.config_stamp as i64,
            now
        ])?;
        if let Some(pw) = rec.nfo_password.filter(|pw| pw.len() <= 256) {
            upsert_nfo_password(conn, rec.name, rec.group_name, rec.content_id, pw)?;
        }
        match rec.cover {
            Some((mime, sha256)) => {
                let exists: bool = conn
                    .prepare_cached("SELECT EXISTS(SELECT 1 FROM raw_covers WHERE name = ?1 AND group_name = ?2)")?
                    .query_row(params![rec.name, rec.group_name], |r| r.get(0))?;
                if !exists {
                    let count: i64 = conn.query_row("SELECT count(*) FROM raw_covers", [], |r| r.get(0))?;
                    if count >= MAX_RAW_PROBES_ROWS {
                        let to_drop = (count - MAX_RAW_PROBES_ROWS + 1).max(64);
                        conn.execute(
                            "DELETE FROM raw_covers WHERE (name, group_name) IN (
                                SELECT name, group_name FROM raw_covers LIMIT ?1
                            )",
                            params![to_drop],
                        )?;
                    }
                }
                conn.prepare_cached(
                    "INSERT OR REPLACE INTO raw_covers (name, group_name, content_id, mime, sha256) VALUES (?1, ?2, ?3, ?4, ?5)",
                )?
                .execute(params![rec.name, rec.group_name, rec.content_id, mime, sha256])?;
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

/// Looks up previously extracted stills for `(name, group_name)` and `content_id` in `raw_stills`.
pub fn get_stills(name: &str, group_name: &str, content_id: &[u8]) -> Vec<SavedStill> {
    if content_id.is_empty() {
        return Vec::new();
    }
    with_raw(|conn| {
        let Some(group_name) = saved_group(conn, STILLS_GROUP, name, group_name, content_id)? else {
            return Ok(Vec::new());
        };
        let rows = conn
            .prepare_cached(
                "SELECT position, at_ms, sha256, width, height, phash
                 FROM raw_stills WHERE name = ?1 AND group_name = ?2 AND content_id = ?3 ORDER BY position",
            )?
            .query_map(params![name, group_name, content_id], |r| {
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

/// Persists extracted stills for `(name, group_name)` and `content_id` in `raw_stills`, bounded by `MAX_RAW_STILLS_ROWS`.
pub fn put_stills(name: &str, group_name: &str, content_id: &[u8], stills: &[SavedStill]) {
    if content_id.is_empty() {
        return;
    }
    let _ = with_raw_tx(|conn| {
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
            "INSERT OR REPLACE INTO raw_stills (name, group_name, position, content_id, at_ms, sha256, width, height, phash)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        )?;
        for s in stills {
            insert.execute(params![
                name,
                group_name,
                i64::from(s.position),
                content_id,
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
    pub content_fp: Option<Vec<u8>>,
    pub files: Vec<SavedArchiveFile>,
}

/// Looks up cached inner files for `(release_name, group_name)`, `content_id`, and `source` in `raw_archive_inner_files`.
pub fn get_archive_files(
    release_name: &str,
    group_name: &str,
    content_id: &[u8],
    source: i64,
) -> Vec<SavedArchiveFile> {
    if content_id.is_empty() {
        return Vec::new();
    }
    with_raw(|conn| {
        // as `saved_group`, among the groups with files from `source`
        let group_name: Option<String> = conn
            .prepare_cached(
                "SELECT group_name FROM raw_archive_inner_files
                 WHERE release_name = ?1 AND content_id = ?3 AND coalesce(source, 2) = ?4
                 ORDER BY group_name != ?2, group_name LIMIT 1",
            )?
            .query_row(params![release_name, group_name, content_id, source], |r| r.get(0))
            .optional()?;
        let Some(group_name) = group_name else { return Ok(Vec::new()) };
        let rows = conn
            .prepare_cached(
                "SELECT seq, name, size, coalesce(source, ?4)
                 FROM raw_archive_inner_files
                 WHERE release_name = ?1 AND group_name = ?2 AND content_id = ?3 AND coalesce(source, 2) = ?4
                 ORDER BY seq",
            )?
            .query_map(params![release_name, group_name, content_id, source], |r| {
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

/// Looks up a previously inspected archive for `(name, group_name)` and `content_id` in `raw_archives`
/// and `raw_archive_inner_files`.
/// If the previous inspection was incomplete/damaged (or an inconclusive `Password::Maybe` with no listed files)
/// on a smaller/incomplete release and the release has since grown, returns `None` so `ListStep` can inspect
/// the newly arrived articles.
pub fn get_archive(
    name: &str,
    group_name: &str,
    content_id: &[u8],
    current_parts: i64,
    current_complete: bool,
) -> Option<SavedArchive> {
    if content_id.is_empty() {
        return None;
    }
    with_raw(|conn| {
        // its inner files were saved with it, under its group
        let Some(group_name) = saved_group(conn, ARCHIVE_GROUP, name, group_name, content_id)? else {
            return Ok(None);
        };
        let group_name = group_name.as_str();
        type RawArchiveRow = (
            i64,
            Option<i64>,
            Option<i64>,
            Option<Vec<u8>>,
            Option<i64>,
            Option<i64>,
            Option<i64>,
            Option<i64>,
            Option<Vec<u8>>,
        );
        let row: Option<RawArchiveRow> = conn
            .prepare_cached(
                "SELECT password, encryption, names_hidden, password_check, parts, complete, listing_complete, listing_damaged, content_fp
                 FROM raw_archives WHERE name = ?1 AND group_name = ?2 AND content_id = ?3",
            )?
            .query_row(params![name, group_name, content_id], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                ))
            })
            .optional()?;
        let Some((pw, enc, hidden, check, saved_parts, saved_complete, l_comp, l_dmg, content_fp)) = row else {
            return Ok(None);
        };
        let password = Some(pw);
        let encryption = enc;
        let names_hidden = hidden.unwrap_or(0) != 0;
        let password_check = check;
        let listing_complete = l_comp.unwrap_or(0) != 0;
        let listing_damaged = l_dmg.unwrap_or(0) != 0;
        // an archive's rows only: PAR2 (1) and posted NZB (5) lists are kept and restored on their own
        let files = conn
            .prepare_cached(
                "SELECT seq, name, size, coalesce(source, 2)
                 FROM raw_archive_inner_files
                 WHERE release_name = ?1 AND group_name = ?2 AND content_id = ?3 AND coalesce(source, 2) NOT IN (1, 5)
                 ORDER BY seq",
            )?
            .query_map(params![name, group_name, content_id], |r| {
                Ok(SavedArchiveFile {
                    seq: r.get(0)?,
                    name: r.get(1)?,
                    size: r.get::<_, Option<i64>>(2)?.and_then(|s| u64::try_from(s).ok()),
                    source: r.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if files.is_empty() && !names_hidden {
            return Ok(None);
        }
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
            content_fp,
            files,
        }))
    })
    .flatten()
}

pub struct ArchiveRecord<'a> {
    pub name: &'a str,
    pub group_name: &'a str,
    pub content_id: &'a [u8],
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
    pub content_fp: Option<&'a [u8]>,
    pub max_names_bytes: i64,
}

/// Persists an archive or PAR2 inspection result into `raw_archives` and `raw_archive_inner_files`,
/// enforcing the configured `archive.max_names_mb` ceiling (capped at `MAX_RAW_ARCHIVE_NAME_BYTES`) and
/// `MAX_RAW_ARCHIVES_ROWS`.
pub fn put_archive(rec: &ArchiveRecord<'_>) {
    if rec.content_id.is_empty() {
        return;
    }
    let _ = with_raw_tx(|conn| {
        conn.prepare_cached(
            "DELETE FROM raw_archive_inner_files
             WHERE release_name = ?1 AND group_name = ?2 AND coalesce(source, 2) = ?3",
        )?
        .execute(params![rec.name, rec.group_name, rec.source])?;
        if !rec.files.is_empty() {
            let incoming_bytes: i64 = rec.files.iter().map(|r| r.name.len() as i64 + 32).sum();
            let effective_quota = rec.max_names_bytes.clamp(0, MAX_RAW_ARCHIVE_NAME_BYTES);
            let used_bytes = raw_meta_get(conn, "archive_name_bytes")?;
            if used_bytes + incoming_bytes > effective_quota {
                if rec.password.is_some() {
                    conn.prepare_cached("DELETE FROM raw_archives WHERE name = ?1 AND group_name = ?2")?
                        .execute(params![rec.name, rec.group_name])?;
                }
                return Ok(());
            }
            let mut insert_file = conn.prepare_cached(
                "INSERT OR REPLACE INTO raw_archive_inner_files
                 (release_name, group_name, seq, content_id, name, size, source)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?;
            for (i, row) in rec.files.iter().enumerate() {
                let size = row.size.and_then(|s| i64::try_from(s).ok());
                let seq = rec.source * 65_536 + i as i64;
                insert_file.execute(params![
                    rec.name,
                    rec.group_name,
                    seq,
                    rec.content_id,
                    row.name,
                    size,
                    rec.source
                ])?;
            }
        } else if rec.password.is_some() && !rec.names_hidden {
            conn.prepare_cached("DELETE FROM raw_archives WHERE name = ?1 AND group_name = ?2")?
                .execute(params![rec.name, rec.group_name])?;
            return Ok(());
        }
        if let Some(pw) = rec.password {
            let exists: bool = conn
                .prepare_cached("SELECT EXISTS(SELECT 1 FROM raw_archives WHERE name = ?1 AND group_name = ?2)")?
                .query_row(params![rec.name, rec.group_name], |r| r.get(0))?;
            if !exists {
                let count: i64 = conn.query_row("SELECT count(*) FROM raw_archives", [], |r| r.get(0))?;
                if count >= MAX_RAW_ARCHIVES_ROWS {
                    let to_drop = (count - MAX_RAW_ARCHIVES_ROWS + 1).max(64);
                    let victims: Vec<(String, String)> = conn
                        .prepare_cached("SELECT name, group_name FROM raw_archives LIMIT ?1")?
                        .query_map(params![to_drop], |r| Ok((r.get(0)?, r.get(1)?)))?
                        .collect::<rusqlite::Result<Vec<_>>>()?;
                    let mut del_arch = conn.prepare_cached("DELETE FROM raw_archives WHERE name = ?1 AND group_name = ?2")?;
                    let mut del_files = conn.prepare_cached(
                        "DELETE FROM raw_archive_inner_files WHERE release_name = ?1 AND group_name = ?2",
                    )?;
                    for (name, group) in &victims {
                        del_arch.execute(params![name, group])?;
                        del_files.execute(params![name, group])?;
                    }
                }
            }
            conn.prepare_cached(
                "INSERT OR REPLACE INTO raw_archives
                 (name, group_name, content_id, password, encryption, names_hidden, password_check, parts, complete, listing_complete, listing_damaged, content_fp)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            )?
            .execute(params![
                rec.name,
                rec.group_name,
                rec.content_id,
                pw,
                rec.encryption,
                i64::from(rec.names_hidden),
                rec.password_check,
                rec.parts,
                i64::from(rec.complete),
                i64::from(rec.listing_complete),
                i64::from(rec.listing_damaged),
                rec.content_fp,
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
    pub content_fp: Option<Vec<u8>>,
    pub inventory: Option<crate::par2::Inventory>,
}

fn read_par2_files_in(
    conn: &Connection,
    name: &str,
    group_name: &str,
    content_id: &[u8],
) -> rusqlite::Result<Option<crate::par2::Inventory>> {
    let mut stmt = conn.prepare_cached(
        "SELECT file_id, set_id, md5, md5_16k, size, file_name, crc32, bound, sealed, complete
         FROM raw_par2_files
         WHERE name = ?1 AND group_name = ?2 AND content_id = ?3
         ORDER BY file_name, file_id",
    )?;
    let mut rows = stmt.query(params![name, group_name, content_id])?;
    let mut inv = crate::par2::Inventory::default();
    let mut any = false;
    while let Some(r) = rows.next()? {
        let Ok(file_id): Result<[u8; 16], _> = r.get::<_, Vec<u8>>(0)?.try_into() else {
            return Ok(None);
        };
        let Ok(set_id): Result<[u8; 16], _> = r.get::<_, Vec<u8>>(1)?.try_into() else {
            return Ok(None);
        };
        let Ok(md5): Result<[u8; 16], _> = r.get::<_, Vec<u8>>(2)?.try_into() else {
            return Ok(None);
        };
        let Ok(md5_16k): Result<[u8; 16], _> = r.get::<_, Vec<u8>>(3)?.try_into() else {
            return Ok(None);
        };
        let size = u64::try_from(r.get::<_, i64>(4)?).unwrap_or(0);
        let file_name: String = r.get(5)?;
        let crc32 = r.get::<_, Option<i64>>(6)?.and_then(|c| u32::try_from(c).ok());
        let bound = r.get::<_, i64>(7)? != 0;
        let sealed = r.get::<_, i64>(8)? != 0;
        let complete = r.get::<_, i64>(9)? != 0;
        if !any {
            inv.complete = complete;
            any = true;
        } else {
            inv.complete &= complete;
        }
        if sealed && bound {
            inv.sealed.push((set_id, file_id));
        }
        inv.files.push(crate::par2::Par2File {
            file_id,
            set_id,
            md5,
            md5_16k,
            size,
            name: file_name,
            bound,
            crc32,
        });
    }
    Ok(any.then_some(inv))
}

/// Looks up cached PAR2 file descriptors for `(name, group_name)` and `content_id` in `raw_par2_files`.
pub fn get_par2_files(name: &str, group_name: &str, content_id: &[u8]) -> Option<crate::par2::Inventory> {
    if content_id.is_empty() {
        return None;
    }
    with_raw(|conn| {
        let Some(group_name) = saved_group(conn, PAR2_FILES_GROUP, name, group_name, content_id)? else {
            return Ok(None);
        };
        read_par2_files_in(conn, name, &group_name, content_id)
    })
    .flatten()
}

fn upsert_par2_files_in_tx(
    conn: &Connection,
    name: &str,
    group_name: &str,
    content_id: &[u8],
    inv: &crate::par2::Inventory,
) -> rusqlite::Result<()> {
    conn.prepare_cached("DELETE FROM raw_par2_files WHERE name = ?1 AND group_name = ?2")?
        .execute(params![name, group_name])?;
    if content_id.is_empty()
        || inv.files.is_empty()
        || inv.files.len() > 4096
        || inv.files.iter().any(|f| f.name.len() > 4096)
    {
        return Ok(());
    }
    let exists_in_par2: bool = conn
        .prepare_cached("SELECT EXISTS(SELECT 1 FROM raw_par2 WHERE name = ?1 AND group_name = ?2)")?
        .query_row(params![name, group_name], |r| r.get(0))?;
    if !exists_in_par2 {
        let distinct: i64 = conn.query_row(
            "SELECT count(*) FROM (SELECT DISTINCT name, group_name FROM raw_par2_files)",
            [],
            |r| r.get(0),
        )?;
        if distinct >= MAX_RAW_ARCHIVES_ROWS {
            let to_drop = (distinct - MAX_RAW_ARCHIVES_ROWS + 1).max(64);
            conn.execute(
                "DELETE FROM raw_par2_files WHERE (name, group_name) IN (
                    SELECT DISTINCT name, group_name FROM raw_par2_files LIMIT ?1
                )",
                params![to_drop],
            )?;
        }
    }
    let mut ins = conn.prepare_cached(
        "INSERT OR REPLACE INTO raw_par2_files
         (name, group_name, content_id, file_id, set_id, md5, md5_16k, size, file_name, crc32, bound, sealed, complete)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
    )?;
    for f in &inv.files {
        let sealed = inv.sealed.contains(&(f.set_id, f.file_id)) && f.bound;
        ins.execute(params![
            name,
            group_name,
            content_id,
            &f.file_id[..],
            &f.set_id[..],
            &f.md5[..],
            &f.md5_16k[..],
            i64::try_from(f.size).unwrap_or(i64::MAX),
            &f.name,
            f.crc32.map(i64::from),
            i64::from(f.bound),
            i64::from(sealed),
            i64::from(inv.complete),
        ])?;
    }
    Ok(())
}

/// Persists a PAR2 set's file descriptors (`par2_files`) for `(name, group_name)` and `content_id`.
pub fn put_par2_files(name: &str, group_name: &str, content_id: &[u8], inv: &crate::par2::Inventory) {
    if content_id.is_empty() || inv.files.is_empty() {
        return;
    }
    let _ = with_raw_tx(|conn| upsert_par2_files_in_tx(conn, name, group_name, content_id, inv));
}

/// Looks up cached PAR2 twin-fingerprint and repair-block metadata for `(name, group_name)` and `content_id`
/// in `raw_par2`. Returns `Some` only when `content_id` and the release's `(parts, complete)` match the
/// inspected snapshot so twin fingerprints (which include posted article counts) and `covers_all` remain exact.
pub fn get_par2(
    name: &str,
    group_name: &str,
    content_id: &[u8],
    current_parts: i64,
    current_complete: bool,
) -> Option<SavedPar2> {
    if content_id.is_empty() {
        return None;
    }
    with_raw(|conn| {
        // its file descriptors were saved with it, under its group
        let Some(group_name) = saved_group(conn, PAR2_GROUP, name, group_name, content_id)? else {
            return Ok(None);
        };
        let group_name = group_name.as_str();
        type RawPar2Row =
            (i64, Option<Vec<u8>>, Option<i64>, Option<i64>, Option<i64>, Option<i64>, Option<Vec<u8>>);
        let row: Option<RawPar2Row> = conn
            .prepare_cached(
                "SELECT covers_all, posted, par2_block, par2_blocks, parts, complete, content_fp
                 FROM raw_par2 WHERE name = ?1 AND group_name = ?2 AND content_id = ?3",
            )?
            .query_row(params![name, group_name, content_id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?))
            })
            .optional()?;
        let Some((covers_all, posted, par2_block, par2_blocks, saved_parts, saved_complete, content_fp)) = row else {
            return Ok(None);
        };
        let was_complete = saved_complete.unwrap_or(0) != 0;
        let had_parts = saved_parts.unwrap_or(0);
        if was_complete != current_complete || had_parts != current_parts {
            return Ok(None);
        }
        let covers = covers_all != 0;
        if posted.as_ref().is_some_and(|p| p.len() != 21)
            || (posted.is_none() && (par2_block.is_some() || par2_blocks.is_some()))
        {
            return Ok(None);
        }
        if covers && (posted.is_none() || par2_block.is_none() || par2_blocks.is_none()) {
            return Ok(None);
        }
        let inventory = read_par2_files_in(conn, name, group_name, content_id)?;
        Ok(Some(SavedPar2 {
            covers_all: covers,
            posted,
            sizes: par2_block.zip(par2_blocks),
            content_fp,
            inventory,
        }))
    })
    .flatten()
}

pub struct Par2Record<'a> {
    pub name: &'a str,
    pub group_name: &'a str,
    pub content_id: &'a [u8],
    pub covers_all: bool,
    pub posted: Option<&'a [u8]>,
    pub sizes: Option<(i64, i64)>,
    pub parts: i64,
    pub complete: bool,
    pub content_fp: Option<&'a [u8]>,
    pub inventory: Option<&'a crate::par2::Inventory>,
}

/// Persists PAR2 twin-fingerprint, block-size, and file-descriptor metadata into `raw_par2` and
/// `raw_par2_files`, bounded by `MAX_RAW_ARCHIVES_ROWS`.
pub fn put_par2(rec: &Par2Record<'_>) {
    if rec.content_id.is_empty() {
        return;
    }
    let _ = with_raw_tx(|conn| {
        let exists: bool = conn
            .prepare_cached("SELECT EXISTS(SELECT 1 FROM raw_par2 WHERE name = ?1 AND group_name = ?2)")?
            .query_row(params![rec.name, rec.group_name], |r| r.get(0))?;
        if !exists {
            let count: i64 = conn.query_row("SELECT count(*) FROM raw_par2", [], |r| r.get(0))?;
            if count >= MAX_RAW_ARCHIVES_ROWS {
                let to_drop = (count - MAX_RAW_ARCHIVES_ROWS + 1).max(64);
                let victims: Vec<(String, String)> = conn
                    .prepare_cached("SELECT name, group_name FROM raw_par2 LIMIT ?1")?
                    .query_map(params![to_drop], |r| Ok((r.get(0)?, r.get(1)?)))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                let mut del_par2 = conn.prepare_cached("DELETE FROM raw_par2 WHERE name = ?1 AND group_name = ?2")?;
                let mut del_files =
                    conn.prepare_cached("DELETE FROM raw_par2_files WHERE name = ?1 AND group_name = ?2")?;
                for (name, group) in &victims {
                    del_par2.execute(params![name, group])?;
                    del_files.execute(params![name, group])?;
                }
            }
        }
        conn.prepare_cached(
            "INSERT OR REPLACE INTO raw_par2
             (name, group_name, content_id, covers_all, posted, par2_block, par2_blocks, parts, complete, content_fp)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        )?
        .execute(params![
            rec.name,
            rec.group_name,
            rec.content_id,
            i64::from(rec.covers_all),
            rec.posted,
            rec.sizes.map(|(b, _)| b),
            rec.sizes.map(|(_, bs)| bs),
            rec.parts,
            i64::from(rec.complete),
            rec.content_fp,
        ])?;
        if let Some(inv) = rec.inventory {
            upsert_par2_files_in_tx(conn, rec.name, rec.group_name, rec.content_id, inv)?;
        }
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

fn populate_harvest_content_ids(tx: &rusqlite::Transaction<'_>, schema: &str) -> rusqlite::Result<()> {
    tx.execute_batch(
        "CREATE TEMP TABLE IF NOT EXISTS harvest_wanted_ids (
             release_id INTEGER PRIMARY KEY
         ) WITHOUT ROWID;
         CREATE TEMP TABLE IF NOT EXISTS harvest_content_ids (
             release_id INTEGER PRIMARY KEY,
             content_id BLOB NOT NULL
         ) WITHOUT ROWID;
         DELETE FROM temp.harvest_wanted_ids;
         DELETE FROM temp.harvest_content_ids;",
    )?;
    for table in [
        "release_probe",
        "release_cover",
        "archive_release",
        "archive_inner_files",
        "par2_files",
        "release_fingerprint",
        "release_nfo_password",
        "release_stills",
    ] {
        if has_table(tx, schema, table)? {
            tx.execute_batch(&format!(
                "INSERT OR IGNORE INTO temp.harvest_wanted_ids (release_id)
                 SELECT DISTINCT release_id FROM {schema}.{table} WHERE release_id IS NOT NULL;"
            ))?;
        }
    }
    let mut insert = tx.prepare_cached(
        "INSERT OR REPLACE INTO temp.harvest_content_ids (release_id, content_id) VALUES (?1, ?2)",
    )?;
    if has_table(tx, schema, "files")? {
        let mut stmt = tx.prepare(&format!(
            "SELECT f.release_id, f.id, f.filename, f.subject_mid, f.expected, f.seen, f.blob
             FROM {schema}.files f
             JOIN temp.harvest_wanted_ids w ON w.release_id = f.release_id
             ORDER BY f.release_id, f.filename, f.id"
        ))?;
        let mut cursor = stmt.query([])?;
        let mut current_rel: Option<(i64, Sha256)> = None;
        while let Some(r) = cursor.next()? {
            let rel_id: i64 = r.get(0)?;
            let file_id: i64 = r.get(1)?;
            let filename: String = r.get(2)?;
            let subject_mid: Option<String> = r.get(3)?;
            let expected: Option<i64> = r.get(4)?;
            let seen: Vec<u8> = r.get(5)?;
            let blob: Option<Vec<u8>> = r.get(6)?;
            if current_rel.as_ref().is_some_and(|(id, _)| *id != rel_id)
                && let Some((prev_id, prev_hasher)) = current_rel.take()
            {
                insert.execute(params![prev_id, prev_hasher.finalize().to_vec()])?;
            }
            let (_, hasher) = current_rel.get_or_insert_with(|| (rel_id, Sha256::new()));
            hash_file_row_into(
                hasher,
                tx,
                schema,
                file_id,
                &filename,
                subject_mid.as_deref(),
                expected,
                &seen,
                blob.as_deref(),
            )?;
        }
        if let Some((last_id, last_hasher)) = current_rel.take() {
            insert.execute(params![last_id, last_hasher.finalize().to_vec()])?;
        }
    } else if has_table(tx, schema, "articles")? {
        let ids: Vec<i64> = tx
            .prepare("SELECT release_id FROM temp.harvest_wanted_ids")?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for rel_id in ids {
            if let Some(cid) = release_content_id_in(tx, schema, rel_id) {
                insert.execute(params![rel_id, cid])?;
            }
        }
    }
    Ok(())
}

fn verified_step_sql(schema: &str, step_bit: i64, has_pp_state: bool, has_pp_walks: bool) -> String {
    if !has_pp_state {
        return "0".to_string();
    }
    let walk_guard = if has_pp_walks {
        format!(
            " AND NOT EXISTS (
                SELECT 1 FROM {schema}.pp_walks w
                WHERE (w.redo_mask = 0 OR (w.redo_mask & {step_bit}) != 0)
                  AND r.id < w.before_id
                  AND (w.group_name IS NULL OR w.group_name = r.group_name)
            )"
        )
    } else {
        String::new()
    };
    format!(
        "(EXISTS (
            SELECT 1 FROM {schema}.pp_state ps
            WHERE ps.release_id = r.id
              AND (ps.done & {step_bit}) != 0
              AND (ps.failed & {step_bit}) = 0
              AND ps.parts_seen = coalesce(r.parts, 0)
        ){walk_guard})"
    )
}

fn prune_archive_inner_files(
    conn: &Connection,
    incoming_bytes: i64,
    keep_release: Option<(&str, &str)>,
) -> rusqlite::Result<()> {
    while raw_meta_get(conn, "archive_name_bytes")? + incoming_bytes > MAX_RAW_ARCHIVE_NAME_BYTES {
        let victim: Option<(String, String)> = match keep_release {
            Some((k_name, k_group)) => conn
                .prepare_cached(
                    "SELECT release_name, group_name FROM raw_archive_inner_files
                     WHERE release_name != ?1 OR group_name != ?2 LIMIT 1",
                )?
                .query_row(params![k_name, k_group], |r| Ok((r.get(0)?, r.get(1)?)))
                .optional()?,
            None => conn
                .prepare_cached("SELECT release_name, group_name FROM raw_archive_inner_files LIMIT 1")?
                .query_row([], |r| Ok((r.get(0)?, r.get(1)?)))
                .optional()?,
        };
        let Some((v_name, v_group)) = victim else {
            break;
        };
        conn.prepare_cached("DELETE FROM raw_archive_inner_files WHERE release_name = ?1 AND group_name = ?2")?
            .execute(params![&v_name, &v_group])?;
        conn.prepare_cached(
            "DELETE FROM raw_archives WHERE name = ?1 AND group_name = ?2 AND coalesce(names_hidden, 0) = 0",
        )?
        .execute(params![&v_name, &v_group])?;
        conn.prepare_cached("UPDATE raw_archives SET listing_complete = 0 WHERE name = ?1 AND group_name = ?2")?
            .execute(params![&v_name, &v_group])?;
    }
    Ok(())
}

fn enforce_harvest_quotas(conn: &Connection) -> rusqlite::Result<()> {
    let os_count: i64 = conn.query_row("SELECT count(*) FROM raw_opensubtitles", [], |r| r.get(0))?;
    if os_count > MAX_OPENSUBTITLES_ROWS {
        conn.execute(
            "DELETE FROM raw_opensubtitles WHERE moviehash IN (
                SELECT moviehash FROM raw_opensubtitles ORDER BY checked_at ASC LIMIT ?1
            )",
            params![os_count - MAX_OPENSUBTITLES_ROWS],
        )?;
    }
    prune_probes(conn, 0, MAX_RAW_PROBES_ROWS + 1, MAX_RAW_PROBES_BYTES)?;
    let pw_count: i64 = conn.query_row("SELECT count(*) FROM raw_nfo_passwords", [], |r| r.get(0))?;
    if pw_count > MAX_RAW_PROBES_ROWS {
        conn.execute(
            "DELETE FROM raw_nfo_passwords WHERE (name, group_name) IN (
                SELECT name, group_name FROM raw_nfo_passwords LIMIT ?1
            )",
            params![pw_count - MAX_RAW_PROBES_ROWS],
        )?;
    }
    let cov_count: i64 = conn.query_row("SELECT count(*) FROM raw_covers", [], |r| r.get(0))?;
    if cov_count > MAX_RAW_PROBES_ROWS {
        conn.execute(
            "DELETE FROM raw_covers WHERE (name, group_name) IN (
                SELECT name, group_name FROM raw_covers LIMIT ?1
            )",
            params![cov_count - MAX_RAW_PROBES_ROWS],
        )?;
    }
    let arch_count: i64 = conn.query_row("SELECT count(*) FROM raw_archives", [], |r| r.get(0))?;
    if arch_count > MAX_RAW_ARCHIVES_ROWS {
        let to_drop = arch_count - MAX_RAW_ARCHIVES_ROWS;
        let victims: Vec<(String, String)> = conn
            .prepare_cached("SELECT name, group_name FROM raw_archives LIMIT ?1")?
            .query_map(params![to_drop], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut del_arch = conn.prepare_cached("DELETE FROM raw_archives WHERE name = ?1 AND group_name = ?2")?;
        let mut del_files =
            conn.prepare_cached("DELETE FROM raw_archive_inner_files WHERE release_name = ?1 AND group_name = ?2")?;
        for (name, group) in &victims {
            del_arch.execute(params![name, group])?;
            del_files.execute(params![name, group])?;
        }
    }
    prune_archive_inner_files(conn, 0, None)?;
    let par2_count: i64 = conn.query_row("SELECT count(*) FROM raw_par2", [], |r| r.get(0))?;
    if par2_count > MAX_RAW_ARCHIVES_ROWS {
        let to_drop = par2_count - MAX_RAW_ARCHIVES_ROWS;
        let victims: Vec<(String, String)> = conn
            .prepare_cached("SELECT name, group_name FROM raw_par2 LIMIT ?1")?
            .query_map(params![to_drop], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut del_par2 = conn.prepare_cached("DELETE FROM raw_par2 WHERE name = ?1 AND group_name = ?2")?;
        let mut del_files = conn.prepare_cached("DELETE FROM raw_par2_files WHERE name = ?1 AND group_name = ?2")?;
        for (name, group) in &victims {
            del_par2.execute(params![name, group])?;
            del_files.execute(params![name, group])?;
        }
    }
    let pf_distinct: i64 = conn.query_row(
        "SELECT count(*) FROM (SELECT DISTINCT name, group_name FROM raw_par2_files)",
        [],
        |r| r.get(0),
    )?;
    if pf_distinct > MAX_RAW_ARCHIVES_ROWS {
        conn.execute(
            "DELETE FROM raw_par2_files WHERE (name, group_name) IN (
                SELECT DISTINCT name, group_name FROM raw_par2_files LIMIT ?1
            )",
            params![pf_distinct - MAX_RAW_ARCHIVES_ROWS],
        )?;
    }
    let stills_count: i64 = conn.query_row("SELECT count(*) FROM raw_stills", [], |r| r.get(0))?;
    if stills_count > MAX_RAW_STILLS_ROWS {
        conn.execute(
            "DELETE FROM raw_stills WHERE (name, group_name, position) IN (
                SELECT name, group_name, position FROM raw_stills LIMIT ?1
            )",
            params![stills_count - MAX_RAW_STILLS_ROWS],
        )?;
    }
    Ok(())
}

fn harvest_attached_release_tables(tx: &rusqlite::Transaction<'_>, schema: &str) -> rusqlite::Result<()> {
    enforce_harvest_quotas(tx)?;
    populate_harvest_content_ids(tx, schema)?;
    let has_pp_state = has_table(tx, schema, "pp_state")?;
    let has_pp_walks = has_table(tx, schema, "pp_walks")?;
    let probe_mask = crate::postprocess::step::mask(crate::probe::step::PROBE_STEP);
    let list_mask = crate::postprocess::step::mask(crate::archive::steps::LIST_STEP);
    let par2_mask = crate::postprocess::step::mask(crate::archive::steps::PAR2_STEP);
    let stills_mask = crate::postprocess::step::mask(crate::probe::stills::STILLS_STEP);
    let probe_verified = verified_step_sql(schema, probe_mask, has_pp_state, has_pp_walks);
    let list_verified = verified_step_sql(schema, list_mask, has_pp_state, has_pp_walks);
    let par2_verified = verified_step_sql(schema, par2_mask, has_pp_state, has_pp_walks);
    if has_table(tx, schema, "release_opensubtitles")? {
        tx.execute_batch(&format!(
            "INSERT INTO main.raw_opensubtitles (moviehash, imdb_id, tmdb_id, kind, title, year, season, episode, release_name, checked_at)
             SELECT moviehash, imdb_id, tmdb_id, kind, title, year, season, episode, release_name, checked_at
             FROM {schema}.release_opensubtitles WHERE moviehash IS NOT NULL AND moviehash != ''
             ORDER BY checked_at DESC LIMIT {MAX_OPENSUBTITLES_ROWS}
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
        let select_sql = format!(
            "SELECT name, group_name, cid, crc32, agent, info, moviehash, parts, complete, stamp
             FROM (
                 SELECT r.name AS name, r.group_name AS group_name,
                        CASE WHEN {probe_verified} THEN hcid.content_id ELSE NULL END AS cid,
                        p.crc32 AS crc32, p.agent AS agent, p.info AS info, p.moviehash AS moviehash,
                        CASE WHEN {probe_verified} THEN coalesce(r.parts, 0) ELSE 0 END AS parts,
                        CASE WHEN {probe_verified} THEN coalesce(r.complete, 0) ELSE 0 END AS complete,
                        CASE WHEN {probe_verified} THEN ?1 ELSE NULL END AS stamp,
                        SUM(length(cast(p.info as blob))) OVER (ORDER BY r.id DESC ROWS UNBOUNDED PRECEDING) AS cum_bytes
                 FROM {schema}.release_probe p
                 JOIN {schema}.releases r ON r.id = p.release_id
                 LEFT JOIN temp.harvest_content_ids hcid ON hcid.release_id = r.id
                 WHERE r.name IS NOT NULL AND r.group_name IS NOT NULL
                   AND length(cast(p.info as blob)) <= {MAX_RAW_PROBES_BYTES}
                 ORDER BY r.id DESC LIMIT {MAX_RAW_PROBES_ROWS}
             )
             WHERE cum_bytes <= {MAX_RAW_PROBES_BYTES}"
        );
        let mut stmt = tx.prepare(&select_sql)?;
        let mut rows = stmt.query(params![probe_stamp])?;
        let mut upsert = tx.prepare_cached(
            "INSERT INTO main.raw_probes
             (name, group_name, content_id, crc32, agent, info, moviehash, parts, complete, config_stamp)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(name, group_name) DO UPDATE SET
                content_id = excluded.content_id,
                crc32 = excluded.crc32,
                agent = excluded.agent,
                info = excluded.info,
                moviehash = excluded.moviehash,
                parts = excluded.parts,
                complete = excluded.complete,
                config_stamp = excluded.config_stamp
             WHERE excluded.content_id IS NOT NULL AND excluded.config_stamp IS NOT NULL",
        )?;
        let mut check_existing = tx.prepare_cached(
            "SELECT length(cast(info as blob)), content_id IS NOT NULL AND config_stamp IS NOT NULL
             FROM main.raw_probes WHERE name = ?1 AND group_name = ?2",
        )?;
        while let Some(r) = rows.next()? {
            let name: String = r.get(0)?;
            let group_name: String = r.get(1)?;
            let cid: Option<Vec<u8>> = r.get(2)?;
            let crc32: Option<i64> = r.get(3)?;
            let agent: Option<String> = r.get(4)?;
            let info: String = r.get(5)?;
            let moviehash: Option<String> = r.get(6)?;
            let parts: i64 = r.get(7)?;
            let complete: i64 = r.get(8)?;
            let stamp: Option<i64> = r.get(9)?;
            let existing: Option<(i64, bool)> = check_existing
                .query_row(params![&name, &group_name], |row| Ok((row.get(0)?, row.get(1)?)))
                .optional()?;
            if let Some((old_bytes, _)) = existing {
                if cid.is_none() || stamp.is_none() {
                    continue;
                }
                let net_bytes = (info.len() as i64 - old_bytes).max(0);
                prune_probes(tx, net_bytes, MAX_RAW_PROBES_ROWS + 1, MAX_RAW_PROBES_BYTES)?;
            } else {
                prune_probes(tx, info.len() as i64, MAX_RAW_PROBES_ROWS, MAX_RAW_PROBES_BYTES)?;
            }
            upsert.execute(params![name, group_name, cid, crc32, agent, info, moviehash, parts, complete, stamp])?;
        }
    }
    if has_table(tx, schema, "release_cover")? {
        tx.execute_batch(&format!(
            "INSERT INTO main.raw_covers (name, group_name, content_id, mime, sha256)
             SELECT r.name, r.group_name,
                    CASE WHEN {probe_verified} THEN hcid.content_id ELSE NULL END,
                    c.mime, c.sha256
             FROM {schema}.release_cover c
             JOIN {schema}.releases r ON r.id = c.release_id
             LEFT JOIN temp.harvest_content_ids hcid ON hcid.release_id = r.id
             WHERE r.name IS NOT NULL AND r.group_name IS NOT NULL
             ORDER BY r.id DESC LIMIT {MAX_RAW_PROBES_ROWS}
             ON CONFLICT(name, group_name) DO UPDATE SET
                content_id = excluded.content_id,
                mime = excluded.mime,
                sha256 = excluded.sha256
             WHERE excluded.content_id IS NOT NULL;"
        ))?;
    }
    let has_fp = has_table(tx, schema, "release_fingerprint")?;
    let has_inner = has_table(tx, schema, "archive_inner_files")?;
    if has_table(tx, schema, "archive_release")? {
        let listing_complete_expr = match (has_fp, has_inner) {
            (true, true) => format!(
                "CASE WHEN {list_verified} AND ((a.password = 0 AND NOT EXISTS (SELECT 1 FROM {schema}.archive_inner_files f WHERE f.release_id = a.release_id AND coalesce(f.source, 2) NOT IN (1, 5)))
                           OR EXISTS (SELECT 1 FROM {schema}.release_fingerprint fp WHERE fp.release_id = a.release_id AND fp.content IS NOT NULL))
                      THEN 1 ELSE 0 END"
            ),
            _ => "0".to_string(),
        };
        let content_fp_expr = if has_fp {
            format!("(SELECT fp.content FROM {schema}.release_fingerprint fp WHERE fp.release_id = a.release_id)")
        } else {
            "NULL".to_string()
        };
        tx.execute_batch(&format!(
            "INSERT INTO main.raw_archives
                 (name, group_name, content_id, password, encryption, names_hidden, password_check, parts, complete, listing_complete, listing_damaged, content_fp)
             SELECT r.name, r.group_name,
                    CASE WHEN {list_verified} THEN hcid.content_id ELSE NULL END,
                    a.password, a.encryption, a.names_hidden, a.password_check,
                    CASE WHEN {list_verified} THEN coalesce(r.parts, 0) ELSE 0 END,
                    CASE WHEN {list_verified} THEN coalesce(r.complete, 0) ELSE 0 END,
                    {listing_complete_expr}, 0, {content_fp_expr}
             FROM {schema}.archive_release a
             JOIN {schema}.releases r ON r.id = a.release_id
             LEFT JOIN temp.harvest_content_ids hcid ON hcid.release_id = r.id
             WHERE r.name IS NOT NULL AND r.group_name IS NOT NULL
             ORDER BY r.id DESC LIMIT {MAX_RAW_ARCHIVES_ROWS}
             ON CONFLICT(name, group_name) DO UPDATE SET
                content_id = excluded.content_id,
                password = excluded.password,
                encryption = excluded.encryption,
                names_hidden = excluded.names_hidden,
                password_check = excluded.password_check,
                parts = excluded.parts,
                complete = excluded.complete,
                listing_complete = excluded.listing_complete,
                listing_damaged = excluded.listing_damaged,
                content_fp = excluded.content_fp
             WHERE excluded.content_id IS NOT NULL;"
        ))?;
    }
    if has_inner {
        let select_inner_sql = format!(
            "SELECT r_id, r_name, group_name, seq, cid, f_name, size, source
             FROM (
                 SELECT r_id, r_name, group_name, seq, cid, f_name, size, source,
                        SUM(length(cast(f_name as blob)) + 32) OVER (ORDER BY r_id DESC RANGE UNBOUNDED PRECEDING) AS cum_bytes,
                        COUNT(*) OVER (ORDER BY r_id DESC RANGE UNBOUNDED PRECEDING) AS cum_rows
                 FROM (
                     SELECT r.id AS r_id, r.name AS r_name, r.group_name AS group_name, f.seq AS seq,
                            CASE WHEN (CASE WHEN coalesce(f.source, 2) = 1 THEN ({par2_verified} OR {list_verified}) ELSE {list_verified} END)
                                 THEN hcid.content_id ELSE NULL END AS cid,
                            f.name AS f_name, f.size AS size, f.source AS source
                     FROM {schema}.archive_inner_files f
                     JOIN {schema}.releases r ON r.id = f.release_id
                     LEFT JOIN temp.harvest_content_ids hcid ON hcid.release_id = r.id
                     WHERE r.name IS NOT NULL AND r.group_name IS NOT NULL
                       AND NOT EXISTS (
                           SELECT 1 FROM {schema}.archive_inner_files f2
                           WHERE f2.release_id = r.id AND length(cast(f2.name as blob)) > 4096
                       )
                     ORDER BY r.id DESC, f.seq ASC LIMIT ({MAX_RAW_ARCHIVES_ROWS} + 1)
                 )
             )
             WHERE cum_bytes <= {MAX_RAW_ARCHIVE_NAME_BYTES}
               AND cum_rows <= {MAX_RAW_ARCHIVES_ROWS}
             ORDER BY r_id DESC, seq ASC"
        );
        let mut stmt = tx.prepare(&select_inner_sql)?;
        let mut rows = stmt.query([])?;
        let mut clear_source = tx.prepare_cached(
            "DELETE FROM main.raw_archive_inner_files
             WHERE release_name = ?1 AND group_name = ?2
               AND coalesce(source, 2) = ?3",
        )?;
        let mut upsert_inner = tx.prepare_cached(
            "INSERT INTO main.raw_archive_inner_files (release_name, group_name, seq, content_id, name, size, source)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(release_name, group_name, seq) DO UPDATE SET
                content_id = excluded.content_id,
                name = excluded.name,
                size = excluded.size,
                source = excluded.source
             WHERE excluded.content_id IS NOT NULL",
        )?;
        let mut check_existing_inner = tx.prepare_cached(
            "SELECT length(cast(name as blob)) + 32
             FROM main.raw_archive_inner_files
             WHERE release_name = ?1 AND group_name = ?2 AND seq = ?3",
        )?;
        let mut winner_rel_id: std::collections::HashMap<(String, String, i64), i64> =
            std::collections::HashMap::new();
        while let Some(r) = rows.next()? {
            let r_id: i64 = r.get(0)?;
            let r_name: String = r.get(1)?;
            let group_name: String = r.get(2)?;
            let seq: i64 = r.get(3)?;
            let cid: Option<Vec<u8>> = r.get(4)?;
            let f_name: String = r.get(5)?;
            let size: Option<i64> = r.get(6)?;
            let source: Option<i64> = r.get(7)?;
            let src_key = source.unwrap_or(2);
            let key = (r_name.clone(), group_name.clone(), src_key);
            match winner_rel_id.get(&key) {
                Some(&win_id) if win_id != r_id => continue,
                Some(_) => {}
                None => {
                    winner_rel_id.insert(key, r_id);
                    if cid.is_some() {
                        clear_source.execute(params![&r_name, &group_name, src_key])?;
                    }
                }
            }
            let row_bytes = f_name.len() as i64 + 32;
            let existing_bytes: Option<i64> = check_existing_inner
                .query_row(params![&r_name, &group_name, seq], |row| row.get(0))
                .optional()?;
            let net_bytes = match existing_bytes {
                Some(old_bytes) => {
                    if cid.is_none() {
                        continue;
                    }
                    (row_bytes - old_bytes).max(0)
                }
                None => row_bytes,
            };
            prune_archive_inner_files(tx, net_bytes, Some((&r_name, &group_name)))?;
            upsert_inner.execute(params![r_name, group_name, seq, cid, f_name, size, source])?;
        }
        tx.execute_batch(
            "DELETE FROM main.raw_archives
             WHERE coalesce(names_hidden, 0) = 0
               AND (
                   content_id IS NULL
                   OR NOT EXISTS (
                       SELECT 1 FROM main.raw_archive_inner_files rf
                       WHERE rf.release_name = main.raw_archives.name
                         AND rf.group_name = main.raw_archives.group_name
                         AND rf.content_id = main.raw_archives.content_id
                         AND coalesce(rf.source, 2) != 1
                   )
               );",
        )?;
    }
    if has_fp {
        tx.execute_batch(&format!(
            "INSERT INTO main.raw_par2
                 (name, group_name, content_id, covers_all, posted, par2_block, par2_blocks, parts, complete, content_fp)
             SELECT r.name, r.group_name,
                    CASE WHEN {par2_verified} THEN hcid.content_id ELSE NULL END,
                    CASE WHEN length(fp.posted) = 21 AND fp.par2_block IS NOT NULL AND fp.par2_blocks IS NOT NULL THEN 1 ELSE 0 END,
                    CASE WHEN length(fp.posted) = 21 THEN fp.posted ELSE NULL END,
                    CASE WHEN length(fp.posted) = 21 THEN fp.par2_block ELSE NULL END,
                    CASE WHEN length(fp.posted) = 21 THEN fp.par2_blocks ELSE NULL END,
                    CASE WHEN {par2_verified} THEN coalesce(r.parts, 0) ELSE 0 END,
                    CASE WHEN {par2_verified} THEN coalesce(r.complete, 0) ELSE 0 END,
                    fp.content
             FROM {schema}.release_fingerprint fp
             JOIN {schema}.releases r ON r.id = fp.release_id
             LEFT JOIN temp.harvest_content_ids hcid ON hcid.release_id = r.id
             WHERE r.name IS NOT NULL AND r.group_name IS NOT NULL
               AND ((length(fp.posted) = 21 AND fp.par2_block IS NOT NULL AND fp.par2_blocks IS NOT NULL) OR fp.content IS NOT NULL)
             ORDER BY r.id DESC LIMIT {MAX_RAW_ARCHIVES_ROWS}
             ON CONFLICT(name, group_name) DO UPDATE SET
                content_id = excluded.content_id,
                covers_all = excluded.covers_all,
                posted = excluded.posted,
                par2_block = excluded.par2_block,
                par2_blocks = excluded.par2_blocks,
                parts = excluded.parts,
                complete = excluded.complete,
                content_fp = excluded.content_fp
             WHERE excluded.content_id IS NOT NULL;"
        ))?;
    }
    if has_table(tx, schema, "par2_files")? {
        let select_pf_sql = format!(
            "SELECT r_id, r_name, group_name, cid, file_id, set_id, md5, md5_16k, size, f_name, crc32, bound, sealed, complete
             FROM (
                 SELECT r_id, r_name, group_name, cid, file_id, set_id, md5, md5_16k, size, f_name, crc32, bound, sealed, complete,
                        COUNT(*) OVER (PARTITION BY r_id) AS rel_rows,
                        COUNT(*) OVER (ORDER BY r_id DESC RANGE UNBOUNDED PRECEDING) AS cum_rows
                 FROM (
                     SELECT r.id AS r_id, r.name AS r_name, r.group_name AS group_name,
                            CASE WHEN ({par2_verified} OR {list_verified}) THEN hcid.content_id ELSE NULL END AS cid,
                            pf.file_id AS file_id, pf.set_id AS set_id, pf.md5 AS md5, pf.md5_16k AS md5_16k,
                            pf.size AS size, pf.name AS f_name, pf.crc32 AS crc32,
                            coalesce(pf.bound, 1) AS bound, coalesce(pf.sealed, 0) AS sealed, coalesce(pf.complete, 0) AS complete
                     FROM {schema}.par2_files pf
                     JOIN {schema}.releases r ON r.id = pf.release_id
                     LEFT JOIN temp.harvest_content_ids hcid ON hcid.release_id = r.id
                     WHERE r.name IS NOT NULL AND r.group_name IS NOT NULL
                       AND NOT EXISTS (
                           SELECT 1 FROM {schema}.par2_files pf2
                           WHERE pf2.release_id = r.id
                             AND (length(pf2.file_id) != 16 OR length(pf2.set_id) != 16
                                  OR length(pf2.md5) != 16 OR length(pf2.md5_16k) != 16
                                  OR length(cast(pf2.name as blob)) > 4096)
                       )
                     ORDER BY r.id DESC, pf.name ASC, pf.file_id ASC LIMIT ({MAX_RAW_ARCHIVES_ROWS} * 4 + 1)
                 )
             )
             WHERE rel_rows <= 4096
               AND cum_rows <= ({MAX_RAW_ARCHIVES_ROWS} * 4)
             ORDER BY r_id DESC, f_name ASC, file_id ASC"
        );
        let mut stmt = tx.prepare(&select_pf_sql)?;
        let mut rows = stmt.query([])?;
        let mut clear_pf = tx.prepare_cached("DELETE FROM main.raw_par2_files WHERE name = ?1 AND group_name = ?2")?;
        let mut upsert_pf = tx.prepare_cached(
            "INSERT INTO main.raw_par2_files
             (name, group_name, content_id, file_id, set_id, md5, md5_16k, size, file_name, crc32, bound, sealed, complete)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
             ON CONFLICT(name, group_name, file_id) DO UPDATE SET
                content_id = excluded.content_id,
                set_id = excluded.set_id,
                md5 = excluded.md5,
                md5_16k = excluded.md5_16k,
                size = excluded.size,
                file_name = excluded.file_name,
                crc32 = excluded.crc32,
                bound = excluded.bound,
                sealed = excluded.sealed,
                complete = excluded.complete
             WHERE excluded.content_id IS NOT NULL",
        )?;
        let mut winner_pf_rel: std::collections::HashMap<(String, String), i64> =
            std::collections::HashMap::new();
        while let Some(r) = rows.next()? {
            let r_id: i64 = r.get(0)?;
            let r_name: String = r.get(1)?;
            let group_name: String = r.get(2)?;
            let cid: Option<Vec<u8>> = r.get(3)?;
            let Some(cid) = cid else { continue };
            let key = (r_name.clone(), group_name.clone());
            match winner_pf_rel.get(&key) {
                Some(&win_id) if win_id != r_id => continue,
                Some(_) => {}
                None => {
                    winner_pf_rel.insert(key, r_id);
                    clear_pf.execute(params![&r_name, &group_name])?;
                }
            }
            let file_id: Vec<u8> = r.get(4)?;
            let set_id: Vec<u8> = r.get(5)?;
            let md5: Vec<u8> = r.get(6)?;
            let md5_16k: Vec<u8> = r.get(7)?;
            let size: i64 = r.get(8)?;
            let file_name: String = r.get(9)?;
            let crc32: Option<i64> = r.get(10)?;
            let bound: i64 = r.get(11)?;
            let sealed: i64 = r.get(12)?;
            let complete: i64 = r.get(13)?;
            upsert_pf.execute(params![
                r_name, group_name, cid, file_id, set_id, md5, md5_16k, size, file_name, crc32, bound, sealed,
                complete
            ])?;
        }
        tx.execute_batch(
            "DELETE FROM main.raw_par2_files
             WHERE content_id IS NULL
                OR EXISTS (
                    SELECT 1 FROM main.raw_par2 p
                    WHERE p.name = main.raw_par2_files.name
                      AND p.group_name = main.raw_par2_files.group_name
                      AND (p.content_id IS NULL OR p.content_id != main.raw_par2_files.content_id)
                );",
        )?;
    }
    if has_table(tx, schema, "release_nfo_password")? {
        let nfo_mask = crate::postprocess::step::mask(crate::naming::NFO);
        let nfo_verified = format!("({probe_verified} OR {})", verified_step_sql(schema, nfo_mask, has_pp_state, has_pp_walks));
        tx.execute_batch(&format!(
            "INSERT INTO main.raw_nfo_passwords (name, group_name, content_id, password)
             SELECT r.name, r.group_name,
                    CASE WHEN {nfo_verified} THEN hcid.content_id ELSE NULL END,
                    np.password
             FROM {schema}.release_nfo_password np
             JOIN {schema}.releases r ON r.id = np.release_id
             LEFT JOIN temp.harvest_content_ids hcid ON hcid.release_id = r.id
             WHERE r.name IS NOT NULL AND r.group_name IS NOT NULL
               AND length(cast(np.password as blob)) <= 256
             ORDER BY r.id DESC LIMIT {MAX_RAW_PROBES_ROWS}
             ON CONFLICT(name, group_name) DO UPDATE SET
                content_id = excluded.content_id,
                password = excluded.password
             WHERE excluded.content_id IS NOT NULL;"
        ))?;
    }
    if has_table(tx, schema, "release_stills")? {
        let stills_verified = verified_step_sql(schema, stills_mask, has_pp_state, has_pp_walks);
        tx.execute_batch(&format!(
            "INSERT INTO main.raw_stills (name, group_name, position, content_id, at_ms, sha256, width, height, phash)
             SELECT r.name, r.group_name, s.position,
                    CASE WHEN {stills_verified} THEN hcid.content_id ELSE NULL END,
                    s.at_ms, s.sha256, s.width, s.height, s.phash
             FROM {schema}.release_stills s
             JOIN {schema}.releases r ON r.id = s.release_id
             LEFT JOIN temp.harvest_content_ids hcid ON hcid.release_id = r.id
             WHERE r.name IS NOT NULL AND r.group_name IS NOT NULL
             ORDER BY r.id DESC LIMIT {MAX_RAW_STILLS_ROWS}
             ON CONFLICT(name, group_name, position) DO UPDATE SET
                content_id = excluded.content_id,
                at_ms = excluded.at_ms,
                sha256 = excluded.sha256,
                width = excluded.width,
                height = excluded.height,
                phash = excluded.phash
             WHERE excluded.content_id IS NOT NULL;"
        ))?;
    }
    enforce_harvest_quotas(tx)?;
    Ok(())
}

fn harvest_attached_main(conn: &Connection, schema: &str) -> rusqlite::Result<()> {
    let tx = conn.unchecked_transaction()?;
    if has_table(&tx, schema, "backfill_deepest")? {
        tx.execute_batch(&format!(
            "DELETE FROM main.raw_backfill_deepest;
             INSERT OR REPLACE INTO main.raw_backfill_deepest (grp, server, oldest_at, runner_up_at)
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
        if shard_path.exists() {
            log(&format!("harvesting shard s{shard_idx} ({})...", shard_path.display()));
            conn.execute("ATTACH DATABASE ?1 AS sh", [shard_path.to_string_lossy().as_ref()])
                .with_context(|| format!("attaching {}", shard_path.display()))?;
            let res = harvest_attached_shard(&conn);
            let _ = conn.execute_batch("ROLLBACK");
            let detach_res = conn.execute("DETACH DATABASE sh", []);
            res.with_context(|| format!("harvesting {}", shard_path.display()))?;
            detach_res.with_context(|| format!("detaching {}", shard_path.display()))?;
        }
        // its frozen releases' rows left the shard: harvested from a shard made of them
        let Some(unfrozen) = crate::frozen::unfrozen_copy(main, shard_idx)
            .with_context(|| format!("reading shard s{shard_idx}'s frozen releases"))?
        else {
            continue;
        };
        log(&format!("harvesting shard s{shard_idx}'s frozen releases ({})...", unfrozen.display()));
        conn.execute("ATTACH DATABASE ?1 AS sh", [unfrozen.to_string_lossy().as_ref()])
            .with_context(|| format!("attaching {}", unfrozen.display()))?;
        let res = harvest_attached_shard(&conn);
        let _ = conn.execute_batch("ROLLBACK");
        let detach_res = conn.execute("DETACH DATABASE sh", []);
        res.with_context(|| format!("harvesting {}", unfrozen.display()))?;
        detach_res.with_context(|| format!("detaching {}", unfrozen.display()))?;
        crate::frozen::remove_family(&unfrozen).with_context(|| format!("removing {}", unfrozen.display()))?;
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
    let mut stmt = raw_conn.prepare(
        "SELECT l.id, l.source_id, l.source_key, e.page, l.body_zst, e.fetched_at
         FROM raw_predb_events e
         JOIN raw_predb_log l ON l.id = e.log_id
         ORDER BY e.seq ASC",
    )?;
    let mut rows = stmt.query([]).context("reading raw_predb_events")?;
    let mut replayed_payloads = 0usize;
    let mut parsed_entries = 0usize;
    let mut inserted = 0u64;
    let mut updated = 0u64;
    while let Some(r) = rows.next().context("stepping raw_predb_events")? {
        let id: i64 = r.get(0)?;
        let source_id: i64 = r.get(1)?;
        let key: String = r.get(2)?;
        let page: i64 = r.get(3)?;
        let zst: Vec<u8> = r.get(4)?;
        let fetched_at: i64 = r.get(5)?;
        let spec = crate::predb::feeds::spec(&key)
            .ok_or_else(|| anyhow::anyhow!("unknown predb source '{key}' in raw_predb_log id={id}"))?;
        let body = decompress(&zst)
            .ok_or_else(|| anyhow::anyhow!("corrupt zstd payload in raw_predb_log id={id} ({key})"))?;
        let page_num = u32::try_from(page).unwrap_or(1).max(1);
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
        replayed_payloads += 1;
        parsed_entries += entries.len();
        if apply {
            let c = crate::predb::import::import(&mut predb_conn, source_id, &entries, fetched_at, None)
                .with_context(|| format!("importing raw_predb_log id={id} ({key}) into {}", predb_path.display()))?;
            inserted += c.inserted;
            updated += c.updated;
        }
    }
    if apply {
        log(&format!(
            "replayed {} raw predb payloads ({} entries): {} inserted, {} updated",
            replayed_payloads,
            parsed_entries,
            inserted,
            updated
        ));
    } else {
        log(&format!(
            "dry run (pass --apply to write): {} raw predb payloads yield {} parsed entries",
            replayed_payloads,
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
        // frozen releases carry the old shard's ids: a new shard would hand them out again
        remove_db_family(&crate::frozen::path(&shard))?;
        remove_db_family(&crate::compact::with_suffix(&shard, "unfrozen"))?;
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
        let cid_a = b"content-identity-release-A";
        let cid_b = b"content-identity-release-B";
        put_probe(&ProbeRecord {
            name: "Some.Release-GRP",
            group_name: "alt.binaries.test",
            content_id: cid_a,
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
        let saved = get_probe("Some.Release-GRP", "alt.binaries.test", cid_a, 20, false, stamp).unwrap();
        assert_eq!(saved.crc32, Some(12345));
        assert_eq!(saved.agent.as_deref(), Some("Nyuu"));
        assert_eq!(saved.info, r#"{"nfo":"hi"}"#);
        assert_eq!(saved.moviehash.as_deref(), Some("8e245d9679d31e12"));
        assert_eq!(saved.nfo_password.as_deref(), Some("secretpw"));
        assert_eq!(saved.cover, Some(("image/jpeg".to_string(), "deadbeef".to_string())));

        // Mismatched or empty content_id -> rejected (prevents name-reuse spoofing).
        assert_eq!(get_probe("Some.Release-GRP", "alt.binaries.test", cid_b, 20, false, stamp), None);
        assert_eq!(get_probe("Some.Release-GRP", "alt.binaries.test", b"", 20, false, stamp), None);

        // Release grew in parts or became complete -> invalidated!
        assert_eq!(get_probe("Some.Release-GRP", "alt.binaries.test", cid_a, 25, false, stamp), None);
        assert_eq!(get_probe("Some.Release-GRP", "alt.binaries.test", cid_a, 20, true, stamp), None);

        // Probe step config_stamp gained a new bit -> invalidated!
        assert_eq!(get_probe("Some.Release-GRP", "alt.binaries.test", cid_a, 20, false, stamp | 16), None);

        // Standalone NFO password roundtrip (independent of raw_probes, verified by content_id)
        put_nfo_password("Nfo.Only-GRP", "alt.binaries.test", cid_a, "nfopass123");
        assert_eq!(get_nfo_password("Nfo.Only-GRP", "alt.binaries.test", cid_a).as_deref(), Some("nfopass123"));
        assert_eq!(get_nfo_password("Nfo.Only-GRP", "alt.binaries.test", cid_b), None);

        // Stills roundtrip (bound to content_id)
        let stills = vec![SavedStill {
            position: 25,
            at_ms: Some(120_000),
            sha256: "a".repeat(64),
            width: 960,
            height: 540,
            phash: Some(42),
        }];
        put_stills("Some.Release-GRP", "alt.binaries.test", cid_a, &stills);
        assert_eq!(get_stills("Some.Release-GRP", "alt.binaries.test", cid_a), stills);
        assert!(get_stills("Some.Release-GRP", "alt.binaries.test", cid_b).is_empty());

        // Archive & PAR2 inner-files roundtrip (bound to content_id and preserving full-list content_fp)
        let archive_files = vec![crate::archive::InnerFile {
            name: "Movie.2026.1080p.mkv".to_string(),
            size: Some(1_500_000_000),
        }];
        put_archive(&ArchiveRecord {
            name: "Some.Release-GRP",
            group_name: "alt.binaries.test",
            content_id: cid_a,
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
            content_fp: Some(&[9; 20]),
            max_names_bytes: 16 << 20,
        });
        let saved_arch = get_archive("Some.Release-GRP", "alt.binaries.test", cid_a, 20, true).unwrap();
        assert_eq!(saved_arch.password, Some(crate::archive::Password::Yes.code()));
        assert_eq!(saved_arch.encryption, Some(crate::archive::Encryption::Rar5Aes256.code()));
        assert!(saved_arch.names_hidden);
        assert!(saved_arch.listing_complete);
        assert!(!saved_arch.listing_damaged);
        assert_eq!(saved_arch.password_check.as_deref(), Some(&[1, 2, 3, 4][..]));
        assert_eq!(saved_arch.content_fp.as_deref(), Some(&[9; 20][..]));
        assert_eq!(saved_arch.files.len(), 1);
        assert_eq!(saved_arch.files[0].name, "Movie.2026.1080p.mkv");
        assert_eq!(get_archive("Some.Release-GRP", "alt.binaries.test", cid_b, 20, true), None);

        // Quota-dropped archive listings (max_names_bytes = 0) are treated as cache misses
        put_archive(&ArchiveRecord {
            name: "Quota.Dropped-GRP",
            group_name: "alt.binaries.test",
            content_id: cid_a,
            source: crate::archive::store::SOURCE_RAR,
            files: &archive_files,
            password: Some(crate::archive::Password::No.code()),
            encryption: None,
            names_hidden: false,
            password_check: None,
            parts: 20,
            complete: true,
            listing_complete: true,
            listing_damaged: false,
            content_fp: Some(&[9; 20]),
            max_names_bytes: 0,
        });
        assert_eq!(get_archive("Quota.Dropped-GRP", "alt.binaries.test", cid_a, 20, true), None);

        // PAR2 twin fingerprint, full-list content_fp, & block metadata + par2_files roundtrip
        let par2_inv = crate::par2::Inventory {
            files: vec![crate::par2::Par2File {
                file_id: [3; 16],
                set_id: [4; 16],
                md5: [5; 16],
                md5_16k: [6; 16],
                size: 1_500_000_000,
                name: "Movie.2026.1080p.mkv".to_string(),
                bound: true,
                crc32: Some(0x1234_5678),
            }],
            sealed: vec![([4; 16], [3; 16])],
            complete: true,
            ..Default::default()
        };
        put_par2(&Par2Record {
            name: "Some.Release-GRP",
            group_name: "alt.binaries.test",
            content_id: cid_a,
            covers_all: true,
            posted: Some(&[2; 21]),
            sizes: Some((384_000, 4_000)),
            parts: 20,
            complete: true,
            content_fp: Some(&[7; 20]),
            inventory: Some(&par2_inv),
        });
        let saved_par2 = get_par2("Some.Release-GRP", "alt.binaries.test", cid_a, 20, true).unwrap();
        assert!(saved_par2.covers_all);
        assert_eq!(saved_par2.posted.as_deref(), Some(&[2; 21][..]));
        assert_eq!(saved_par2.sizes, Some((384_000, 4_000)));
        assert_eq!(saved_par2.content_fp.as_deref(), Some(&[7; 20][..]));
        assert_eq!(saved_par2.inventory, Some(par2_inv));
        assert_eq!(get_par2("Some.Release-GRP", "alt.binaries.test", cid_a, 25, true), None);
        assert_eq!(get_par2("Some.Release-GRP", "alt.binaries.test", cid_b, 20, true), None);
        // Legacy 20-byte twin fingerprints are rejected by get_par2 (matching duplicates::forget_old_twins)
        put_par2(&Par2Record {
            name: "Legacy.Par2-GRP",
            group_name: "alt.binaries.test",
            content_id: cid_a,
            covers_all: true,
            posted: Some(&[1; 20]),
            sizes: Some((384_000, 4_000)),
            parts: 20,
            complete: true,
            content_fp: Some(&[7; 20]),
            inventory: None,
        });
        assert_eq!(get_par2("Legacy.Par2-GRP", "alt.binaries.test", cid_a, 20, true), None);

        // raw_meta counters match live table sizes (including UTF-8 byte length for CP437/box-drawing NFOs) and update on prune
        let non_ascii_info = r#"{"nfo":"██▓▒░ hi ░▒▓██"}"#;
        assert!(non_ascii_info.len() > non_ascii_info.chars().count());
        put_probe(&ProbeRecord {
            name: "Some.Release-GRP",
            group_name: "alt.binaries.test",
            content_id: cid_a,
            crc32: Some(12345),
            agent: Some("Nyuu"),
            info: non_ascii_info,
            moviehash: Some("8e245d9679d31e12"),
            parts: 20,
            complete: false,
            config_stamp: stamp,
            nfo_password: Some("secretpw"),
            cover: Some(("image/jpeg", "deadbeef")),
        });
        {
            let conn = create(&raw_path).unwrap();
            assert_eq!(raw_meta_get(&conn, "probe_rows").unwrap(), 1);
            assert_eq!(raw_meta_get(&conn, "probe_bytes").unwrap(), non_ascii_info.len() as i64);
            // Recording A -> B -> A preserves observation order in raw_predb_events while deduplicating raw_predb_log,
            // and alternating observations of B and C never orphan A's baseline event or grow raw_predb_events unboundedly
            conn.execute_batch("DELETE FROM raw_predb_log; DELETE FROM raw_predb_events;").unwrap();
            record_predb_page(1, "predb_club", "https://predb.club/api/v1/?page=1", 1, r#"{"state":"A"}"#);
            record_predb_page(1, "predb_club", "https://predb.club/api/v1/?page=1", 1, r#"{"state":"B"}"#);
            record_predb_page(1, "predb_club", "https://predb.club/api/v1/?page=1", 1, r#"{"state":"A"}"#);
            let pre_count: i64 = conn.query_row("SELECT count(*) FROM raw_predb_log", [], |r| r.get(0)).unwrap();
            let ev_count: i64 = conn.query_row("SELECT count(*) FROM raw_predb_events", [], |r| r.get(0)).unwrap();
            assert_eq!(pre_count, 2, "raw_predb_log stores unique payloads");
            assert_eq!(ev_count, 3, "raw_predb_events records A -> B -> A transitions");
            for _ in 0..10 {
                record_predb_page(1, "predb_club", "https://predb.club/api/v1/?page=1", 1, r#"{"state":"B"}"#);
                record_predb_page(1, "predb_club", "https://predb.club/api/v1/?page=1", 1, r#"{"state":"C"}"#);
            }
            let distinct_reachable: i64 = conn
                .query_row("SELECT count(DISTINCT log_id) FROM raw_predb_events", [], |r| r.get(0))
                .unwrap();
            let total_logs: i64 = conn.query_row("SELECT count(*) FROM raw_predb_log", [], |r| r.get(0)).unwrap();
            let bounded_ev_count: i64 = conn.query_row("SELECT count(*) FROM raw_predb_events", [], |r| r.get(0)).unwrap();
            assert_eq!(total_logs, 3);
            assert_eq!(distinct_reachable, 3, "all retained raw_predb_log payloads remain reachable in raw_predb_events");
            assert!(bounded_ev_count <= total_logs * 2, "raw_predb_events bounded to at most 2 events per payload");
            // Recording the same payload consecutively at MAX_PREDB_LOG_ROWS must not evict existing rows or add duplicate events
            conn.execute("UPDATE raw_meta SET value = ?1 WHERE key = 'predb_rows'", [MAX_PREDB_LOG_ROWS]).unwrap();
            record_predb_page(1, "predb_club", "https://predb.club/api/v1/?page=1", 1, r#"{"state":"C"}"#);
            let post_count: i64 = conn.query_row("SELECT count(*) FROM raw_predb_log", [], |r| r.get(0)).unwrap();
            let post_ev_count: i64 = conn.query_row("SELECT count(*) FROM raw_predb_events", [], |r| r.get(0)).unwrap();
            assert_eq!(post_count, 3, "duplicate PreDB payload must not trigger eviction");
            assert_eq!(post_ev_count, bounded_ev_count, "consecutive identical payload on same page must not append redundant event");

            prune_probes(&conn, MAX_RAW_PROBES_BYTES, MAX_RAW_PROBES_ROWS, MAX_RAW_PROBES_BYTES).unwrap();
            assert_eq!(raw_meta_get(&conn, "probe_rows").unwrap(), 0);
            assert_eq!(raw_meta_get(&conn, "probe_bytes").unwrap(), 0);
            assert_eq!(get_nfo_password("Some.Release-GRP", "alt.binaries.test", cid_a), None);
            assert_eq!(get_nfo_password("Nfo.Only-GRP", "alt.binaries.test", cid_a).as_deref(), Some("nfopass123"));
            let cov_count: i64 = conn.query_row("SELECT count(*) FROM raw_covers", [], |r| r.get(0)).unwrap();
            assert_eq!(cov_count, 0, "dependent raw_covers row must be pruned with parent probe");
            let now = chrono::Utc::now().timestamp();
            prune_http_cache(&conn, MAX_HTTP_CACHE_BYTES, MAX_HTTP_CACHE_ROWS, MAX_HTTP_CACHE_BYTES, now).unwrap();
            assert_eq!(raw_meta_get(&conn, "http_rows").unwrap(), 0);
            assert_eq!(raw_meta_get(&conn, "http_bytes").unwrap(), 0);
        }

        set_test_raw_path(None);
    }

    #[test]
    fn a_crosspost_is_read_once_for_all_its_groups() {
        let dir = tempfile::tempdir().unwrap();
        set_test_raw_path(Some(dir.path().join("atlas.raw.db")));
        let (name, a, b, c) = ("Cross.Posted.2026.1080p-GRP", "alt.binaries.a", "alt.binaries.b", "alt.binaries.c");
        let (same, other) = (&b"content-identity-of-the-post"[..], &b"content-identity-of-another"[..]);
        let stamp = 1 | 2 | 4 | 8;
        let probe = |group, content, crc32, info| ProbeRecord {
            name,
            group_name: group,
            content_id: content,
            crc32: Some(crc32),
            agent: Some("Nyuu"),
            info,
            moviehash: None,
            parts: 20,
            complete: true,
            config_stamp: stamp,
            nfo_password: Some("pw-of-a"),
            cover: Some(("image/jpeg", "cover-of-a")),
        };
        put_probe(&probe(a, same, 1, r#"{"from":"a"}"#));

        // the copy in another group has the same articles: a's probe, password and cover answer for it
        let got = get_probe(name, b, same, 20, true, stamp).unwrap();
        assert_eq!((got.crc32, got.info.as_str()), (Some(1), r#"{"from":"a"}"#));
        assert_eq!(got.nfo_password.as_deref(), Some("pw-of-a"));
        assert_eq!(got.cover, Some(("image/jpeg".to_string(), "cover-of-a".to_string())));
        // other articles under the same name are another post, and a later stamp or more parts still read again
        assert_eq!(get_probe(name, b, other, 20, true, stamp), None);
        assert_eq!(get_probe(name, b, same, 20, true, stamp | 16), None);
        assert_eq!(get_probe(name, b, same, 25, true, stamp), None);
        assert_eq!(get_probe("Another.Name-GRP", b, same, 20, true, stamp), None);

        // a group's own result comes first, and one of its own for other articles doesn't hide the post's
        put_probe(&probe(b, same, 2, r#"{"from":"b"}"#));
        assert_eq!(get_probe(name, b, same, 20, true, stamp).unwrap().crc32, Some(2));
        put_probe(&probe(c, other, 3, r#"{"from":"c"}"#));
        assert_eq!(get_probe(name, c, same, 20, true, stamp).unwrap().crc32, Some(1));
        assert_eq!(get_probe(name, c, other, 20, true, stamp).unwrap().crc32, Some(3));

        put_nfo_password(name, a, same, "nfo-pw");
        assert_eq!(get_nfo_password(name, c, same).as_deref(), Some("nfo-pw"));
        assert_eq!(get_nfo_password(name, c, b"no such post"), None);

        let stills =
            vec![SavedStill { position: 25, at_ms: Some(1), sha256: "a".repeat(64), width: 9, height: 9, phash: None }];
        put_stills(name, a, same, &stills);
        assert_eq!(get_stills(name, b, same), stills);
        assert!(get_stills(name, b, other).is_empty());

        let files = vec![crate::archive::InnerFile { name: "Movie.mkv".into(), size: Some(9) }];
        put_archive(&ArchiveRecord {
            name,
            group_name: a,
            content_id: same,
            source: crate::archive::store::SOURCE_RAR,
            files: &files,
            password: Some(crate::archive::Password::No.code()),
            encryption: None,
            names_hidden: false,
            password_check: None,
            parts: 20,
            complete: true,
            listing_complete: true,
            listing_damaged: false,
            content_fp: None,
            max_names_bytes: 16 << 20,
        });
        assert_eq!(get_archive(name, b, same, 20, true).unwrap().files[0].name, "Movie.mkv");
        assert_eq!(get_archive_files(name, b, same, crate::archive::store::SOURCE_RAR)[0].name, "Movie.mkv");
        assert_eq!(get_archive(name, b, other, 20, true), None);
        assert!(get_archive_files(name, b, same, crate::archive::store::SOURCE_PAR2).is_empty());

        let inventory = crate::par2::Inventory {
            files: vec![crate::par2::Par2File {
                file_id: [3; 16],
                set_id: [4; 16],
                md5: [5; 16],
                md5_16k: [6; 16],
                size: 9,
                name: "Movie.mkv".into(),
                bound: true,
                crc32: None,
            }],
            sealed: vec![([4; 16], [3; 16])],
            complete: true,
            ..Default::default()
        };
        put_par2(&Par2Record {
            name,
            group_name: a,
            content_id: same,
            covers_all: true,
            posted: Some(&[2; 21]),
            sizes: Some((384_000, 4_000)),
            parts: 20,
            complete: true,
            content_fp: None,
            inventory: Some(&inventory),
        });
        assert_eq!(get_par2(name, b, same, 20, true).unwrap().inventory, Some(inventory.clone()));
        assert_eq!(get_par2_files(name, b, same), Some(inventory));
        assert_eq!(get_par2(name, b, other, 20, true), None);
        assert_eq!(get_par2_files(name, b, other), None);
        set_test_raw_path(None);
    }

    #[test]
    fn legacy_articles_content_id_matches_sharded_files_content_id() {
        let legacy = Connection::open_in_memory().unwrap();
        legacy
            .execute_batch(
                "CREATE TABLE articles (
                    id INTEGER PRIMARY KEY,
                    release_id INTEGER,
                    message_id TEXT,
                    subject TEXT,
                    filename TEXT,
                    part INTEGER,
                    total_parts INTEGER,
                    bytes INTEGER,
                    file_total INTEGER
                );
                INSERT INTO articles (release_id, message_id, subject, filename, part, total_parts, bytes, file_total) VALUES
                    (1, '<seg2@news.example>', 'sub2', 'movie.mkv', 2, 2, 750000, 2),
                    (1, '<seg1@news.example>', 'sub1', 'movie.mkv', 1, 2, 750000, 2),
                    (1, '<nfo1@news.example>', 'subnfo', 'movie.nfo', 1, 1, 4096, 1);",
            )
            .unwrap();

        let sharded = Connection::open_in_memory().unwrap();
        crate::store::build_shard(&sharded).unwrap();
        let articles = [
            crate::parser::Article {
                message_id: "<seg2@news.example>".into(),
                subject: "sub2".into(),
                filename: Some("movie.mkv".into()),
                part: Some(2),
                total_parts: Some(2),
                bytes: 750000,
                file_total: Some(2),
                ..Default::default()
            },
            crate::parser::Article {
                message_id: "<seg1@news.example>".into(),
                subject: "sub1".into(),
                filename: Some("movie.mkv".into()),
                part: Some(1),
                total_parts: Some(2),
                bytes: 750000,
                file_total: Some(2),
                ..Default::default()
            },
            crate::parser::Article {
                message_id: "<nfo1@news.example>".into(),
                subject: "subnfo".into(),
                filename: Some("movie.nfo".into()),
                part: Some(1),
                total_parts: Some(1),
                bytes: 4096,
                file_total: Some(1),
                ..Default::default()
            },
        ];
        for name in ["movie.mkv", "movie.nfo"] {
            let mut f = crate::store::file(&sharded, 1, name).unwrap();
            for a in articles.iter().filter(|a| a.filename.as_deref() == Some(name)) {
                f.add(a);
            }
            crate::store::put_file(&sharded, &f).unwrap();
        }

        let legacy_cid = release_content_id(&legacy, 1).expect("legacy content_id");
        let sharded_cid = release_content_id(&sharded, 1).expect("sharded content_id");
        assert_eq!(legacy_cid, sharded_cid);
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

        let raw_conn = create(&path_for(&main)).unwrap();
        raw_conn
            .execute(
                "INSERT INTO raw_backfill_deepest (grp, server, oldest_at, runner_up_at) VALUES ('alt.binaries.stale', 's1', 100, 200)",
                [],
            )
            .unwrap();
        drop(raw_conn);

        reset_shards_at(&main, &|_| {}).unwrap();
        assert!(!main.exists());
        assert!(!old_main.exists());
        assert!(!precompact_s1.exists());
        assert!(path_for(&main).exists(), "atlas.raw.db must be preserved");
        let raw_conn = create(&path_for(&main)).unwrap();
        let bd_count: i64 =
            raw_conn.query_row("SELECT count(*) FROM raw_backfill_deepest", [], |r| r.get(0)).unwrap();
        assert_eq!(bd_count, 0, "harvest must replace stale raw_backfill_deepest snapshot");
    }
}
