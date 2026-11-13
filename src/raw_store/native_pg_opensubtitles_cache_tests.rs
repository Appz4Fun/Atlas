//! The configured natural-key cache API, isolated from providers and SQLite.

use crate as atlas;

use std::io::ErrorKind;
use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use atlas::probe::opensubtitles::Feature;

const POSITIVE: &str = "abcdef0123456789";
const NEGATIVE: &str = "abcdef012345678a";
const DIFFERENT_CASE: &str = "ABCDEF0123456789";
const ABSENT: &str = "abcdef012345678b";
const RAW_SENTINEL: &[u8] = b"SQLite must not be opened by this native cache fixture";

// These adapters observe the real public API before and after its checked
// Result transition. Legacy Option/void has no way to report a storage error.
trait CheckedRead {
    fn checked(self) -> Result<Option<Option<Feature>>, ()>;
}
impl CheckedRead for Option<Option<Feature>> {
    fn checked(self) -> Result<Option<Option<Feature>>, ()> {
        Ok(self)
    }
}
impl<E> CheckedRead for Result<Option<Option<Feature>>, E> {
    fn checked(self) -> Result<Option<Option<Feature>>, ()> {
        self.map_err(|_| ())
    }
}
trait CheckedWrite {
    fn checked(self) -> Result<(), ()>;
}
impl CheckedWrite for () {
    fn checked(self) -> Result<(), ()> {
        Ok(())
    }
}
impl<E> CheckedWrite for Result<(), E> {
    fn checked(self) -> Result<(), ()> {
        self.map_err(|_| ())
    }
}

trait ErrorText {
    fn error_text(&self) -> Option<String>;
}
impl ErrorText for Option<Option<Feature>> {
    fn error_text(&self) -> Option<String> {
        None
    }
}
impl<T, E: std::fmt::Display> ErrorText for Result<T, E> {
    fn error_text(&self) -> Option<String> {
        self.as_ref().err().map(ToString::to_string)
    }
}

fn feature() -> Feature {
    Feature {
        imdb_id: Some(101),
        tmdb_id: Some(202),
        kind: Some("Movie".into()),
        title: Some("Example Film".into()),
        year: Some(2001),
        season: None,
        episode: None,
        release_name: Some("Example.Film.2001".into()),
    }
}

#[derive(Debug, PartialEq, Eq)]
struct CacheRow {
    key: String,
    feature: Feature,
    raw: Option<Vec<u8>>,
    checked_at: i64,
}

const ROW_COLUMNS: &str = "moviehash,imdb_id,tmdb_id,kind,title,year,season,episode,release_name,raw_json_zst,checked_at";

fn sqlite_seed(path: &Path) {
    let conn = atlas::raw_store::create(path).unwrap();
    let f = feature();
    conn.execute(
        "insert into raw_opensubtitles
         (moviehash,imdb_id,tmdb_id,kind,title,year,release_name,raw_json_zst,checked_at)
         values (?1,?2,?3,?4,?5,?6,?7,?8,1)",
        rusqlite::params![POSITIVE, f.imdb_id, f.tmdb_id, f.kind, f.title, f.year, f.release_name, b"preserved raw fixture".as_slice()],
    ).unwrap();
    conn.execute("insert into raw_opensubtitles (moviehash,checked_at) values (?1,1)", [NEGATIVE]).unwrap();
    conn.execute(
        "insert into raw_opensubtitles (moviehash,imdb_id,title,checked_at) values (?1,303,'Case distinct',1)",
        [DIFFERENT_CASE],
    ).unwrap();
}

fn sqlite_rows(path: &Path) -> Vec<CacheRow> {
    let conn = rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let mut statement = conn.prepare(&format!("select {ROW_COLUMNS} from raw_opensubtitles order by moviehash")).unwrap();
    statement.query_map([], |row| Ok(CacheRow {
        key: row.get(0)?,
        feature: Feature {
            imdb_id: row.get(1)?, tmdb_id: row.get(2)?, kind: row.get(3)?, title: row.get(4)?, year: row.get(5)?,
            season: row.get(6)?, episode: row.get(7)?, release_name: row.get(8)?,
        },
        raw: row.get(9)?, checked_at: row.get(10)?,
    })).unwrap().collect::<Result<_, _>>().unwrap()
}

struct Schema {
    client: postgres::Client,
    name: String,
    url: String,
}
impl Schema {
    fn new() -> Option<Self> {
        let url = std::env::var("ATLAS_PG_TEST_URL").ok().filter(|s| !s.is_empty())?;
        let mut client = postgres::Client::connect(&url, postgres::NoTls).expect("owned test destination");
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let name = format!("t_os_cache_{}_{}_{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed), nonce);
        client.batch_execute(&format!("create schema \"{name}\"")).expect("new owned schema");
        let mut schema = Self { client, name, url };
        atlas::pg::migrate::run(&mut schema.client, &schema.name).expect("owned schema migration");
        schema.client.batch_execute(&format!("set search_path to \"{}\"", schema.name)).unwrap();
        Some(schema)
    }

    fn seed(&mut self) {
        let f = feature();
        let raw: &[u8] = b"preserved raw fixture";
        self.client
            .execute(
                &format!(
                    "insert into {}.ext_opensubtitles
                     (moviehash,imdb_id,tmdb_id,kind,title,year,release_name,raw_json_zst,checked_at)
                     values ($1,$2,$3,$4,$5,$6,$7,$8,1)",
                    self.name
                ),
                &[&POSITIVE, &f.imdb_id, &f.tmdb_id, &f.kind, &f.title, &f.year, &f.release_name, &raw],
            )
            .unwrap();
        self.client
            .execute(
                &format!("insert into {}.ext_opensubtitles (moviehash,checked_at) values ($1,1)", self.name),
                &[&NEGATIVE],
            )
            .unwrap();
        self.client
            .execute(
                &format!(
                    "insert into {}.ext_opensubtitles (moviehash,imdb_id,title,checked_at) values ($1,303,'Case distinct',1)",
                    self.name
                ),
                &[&DIFFERENT_CASE],
            )
            .unwrap();
    }

    fn rows(&mut self) -> Vec<CacheRow> {
        self.client.query(&format!("select {ROW_COLUMNS} from {}.ext_opensubtitles order by moviehash", self.name), &[])
            .unwrap().into_iter().map(|row| CacheRow {
                key: row.get(0),
                feature: Feature {
                    imdb_id: row.get(1), tmdb_id: row.get(2), kind: row.get(3), title: row.get(4), year: row.get(5),
                    season: row.get(6), episode: row.get(7), release_name: row.get(8),
                },
                raw: row.get(9), checked_at: row.get(10),
            }).collect()
    }
}
impl Drop for Schema {
    fn drop(&mut self) {
        let _ = self.client.batch_execute(&format!("drop schema \"{}\" cascade", self.name));
    }
}

fn write_config(home: &Path, url: &str, schema: &str, http_port: u16) {
    let mut config = toml_edit::DocumentMut::new();
    config["storage"]["backend"] = toml_edit::value("postgres");
    config["storage"]["url"] = toml_edit::value(url);
    config["storage"]["schema"] = toml_edit::value(schema);
    config["storage"]["max_connections"] = toml_edit::value(1);
    config["storage"]["allow_plaintext"] = toml_edit::value(true);
    config["media"]["tvmaze"]["enabled"] = toml_edit::value(false);
    config["media"]["allocine"]["enabled"] = toml_edit::value(false);
    config["media"]["details_per_minute"] = toml_edit::value(0);
    config["media"]["opensubtitles"]["api_key"] = toml_edit::value("");
    config["media"]["opensubtitles"]["base_url"] = toml_edit::value(format!("http://127.0.0.1:{http_port}"));
    config["postprocess"]["enabled"] = toml_edit::value(false);
    config["predb"]["enabled"] = toml_edit::value(false);
    config["spots"]["enabled"] = toml_edit::value(false);
    std::fs::write(home.join("config.toml"), config.to_string()).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(home.join("config.toml"), std::fs::Permissions::from_mode(0o600)).unwrap();
    }
}

fn child(url: &str, schema: &str, action: &str) -> tempfile::TempDir {
    child_with_backend(url, schema, action, false)
}

fn child_with_backend(url: &str, schema: &str, action: &str, sqlite_oracle: bool) -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    let http = TcpListener::bind("127.0.0.1:0").unwrap();
    http.set_nonblocking(true).unwrap();
    write_config(home.path(), url, schema, http.local_addr().unwrap().port());
    if sqlite_oracle {
        let path = home.path().join("config.toml");
        let mut config: toml_edit::DocumentMut = std::fs::read_to_string(&path).unwrap().parse().unwrap();
        config["storage"]["backend"] = toml_edit::value("sqlite");
        std::fs::write(path, config.to_string()).unwrap();
        sqlite_seed(&home.path().join("atlas.raw.db"));
    } else {
        std::fs::write(home.path().join("atlas.raw.db"), RAW_SENTINEL).unwrap();
    }
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "raw_store::native_pg_opensubtitles_cache_tests::configured_cache_child", "--nocapture"])
        .env("ATLAS_HOME", home.path())
        .env("ATLAS_SAB_DIR", home.path().join("no-sab"))
        .env("ATLAS_NO_KEYRING", "1")
        .env("ATLAS_CACHE_TEST_ACTION", action)
        .env_remove("ATLAS_PG_TEST_URL")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if Instant::now() >= until {
            child.kill().unwrap();
            let _ = child.wait();
            panic!("owned cache child exceeded its deadline");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "configured cache child failed: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if !sqlite_oracle {
        assert_eq!(std::fs::read(home.path().join("atlas.raw.db")).unwrap(), RAW_SENTINEL);
    }
    assert!(matches!(http.accept(), Err(error) if error.kind() == ErrorKind::WouldBlock), "cache called HTTP");
    for entry in std::fs::read_dir(home.path()).unwrap() {
        let name = entry.unwrap().file_name().into_string().unwrap();
        assert!(
            matches!(name.as_str(), "config.toml" | "atlas.raw.db")
                || (sqlite_oracle && matches!(name.as_str(), "atlas.raw.db-wal" | "atlas.raw.db-shm")),
            "cache created unexpected files: {name}"
        );
    }
    home
}

#[test]
fn configured_postgres_cache_reads_positive_negative_missing_and_case_distinct_keys() {
    let Some(mut schema) = Schema::new() else { return };
    schema.seed();
    child(&schema.url, &schema.name, "read");
}

#[test]
fn configured_postgres_upserts_preserve_fields_and_apply_raw_json_contract() {
    let Some(mut schema) = Schema::new() else { return };
    schema.seed();
    let began = chrono::Utc::now().timestamp();
    child(&schema.url, &schema.name, "write");
    let row = schema
        .client
        .query_one(
            &format!(
                "select imdb_id,tmdb_id,kind,title,year,season,episode,release_name,raw_json_zst,checked_at
                 from {}.ext_opensubtitles where moviehash=$1",
                schema.name
            ),
            &[&POSITIVE],
        )
        .unwrap();
    assert_eq!(row.get::<_, Option<i64>>(0), Some(101));
    assert_eq!(row.get::<_, Option<i64>>(1), Some(404));
    assert_eq!(row.get::<_, Option<String>>(2).as_deref(), Some("Movie"));
    assert_eq!(row.get::<_, Option<String>>(3).as_deref(), Some("Example Film"));
    assert_eq!(row.get::<_, Option<i64>>(4), Some(2001));
    assert_eq!(row.get::<_, Option<i64>>(5), None);
    assert_eq!(row.get::<_, Option<i64>>(6), None);
    assert_eq!(row.get::<_, Option<String>>(7).as_deref(), Some("Example.Film.2001"));
    assert_eq!(row.get::<_, Option<Vec<u8>>>(8), None);
    assert!(row.get::<_, i64>(9) >= began);
}

#[test]
fn paired_sqlite_oracle_matches_native_cache_fields_nulls_keys_and_refresh_time() {
    let Some(mut schema) = Schema::new() else { return };
    schema.seed();
    let began = chrono::Utc::now().timestamp();
    let sqlite = child_with_backend(&schema.url, &schema.name, "write", true);
    child(&schema.url, &schema.name, "write");
    let mut expected = sqlite_rows(&sqlite.path().join("atlas.raw.db"));
    let mut actual = schema.rows();
    let finished = chrono::Utc::now().timestamp();
    for rows in [&mut expected, &mut actual] {
        let updated = rows.iter_mut().find(|row| row.key == POSITIVE).unwrap();
        assert!((began..=finished).contains(&updated.checked_at));
        updated.checked_at = 0;
    }
    assert_eq!(actual, expected);
}

#[test]
fn mutated_cache_gets_a_fresh_ext_dump_digest_and_restores_exact_rows() {
    let Some(mut source) = Schema::new() else { return };
    source.seed();
    let Some(mut target) = Schema::new() else { return };
    let dumps = tempfile::tempdir().unwrap();
    let before = atlas::pg::ext::dump(&mut source.client, &dumps.path().join("before"), 2, false).unwrap();
    let original_manifest = std::fs::read(before.join("manifest.json")).unwrap();
    let before_manifest: atlas::pg::ext::Manifest = serde_json::from_slice(&original_manifest).unwrap();
    child(&source.url, &source.name, "write");
    let after = atlas::pg::ext::dump(&mut source.client, &dumps.path().join("after"), 2, false).unwrap();
    let after_manifest: atlas::pg::ext::Manifest = serde_json::from_slice(&std::fs::read(after.join("manifest.json")).unwrap()).unwrap();
    let old_cache = before_manifest.tables.iter().find(|row| row.table == "ext_opensubtitles").unwrap();
    let new_cache = after_manifest.tables.iter().find(|row| row.table == "ext_opensubtitles").unwrap();
    assert_eq!(new_cache.rows, 3);
    assert!(new_cache.sha256.as_ref().is_some_and(|hash| hash.len() == 64));
    assert_ne!(new_cache.sha256, old_cache.sha256, "dump still describes the cache before its mutation");
    assert_eq!(std::fs::read(before.join("manifest.json")).unwrap(), original_manifest);
    let restored = atlas::pg::ext::restore(&mut target.client, &after, false).unwrap();
    assert_eq!(restored.tables.iter().find(|row| row.table == "ext_opensubtitles").unwrap(), new_cache);
    assert_eq!(target.rows(), source.rows());
}

#[test]
fn configured_postgres_unavailable_cache_is_an_error_without_sqlite_or_http_fallback() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let url = format!("host=127.0.0.1 port={port} user=example dbname=example connect_timeout=1");
    child(&url, "cache_fixture", "unavailable");
}

#[test]
fn configured_cache_supports_the_standard_stage_thread_and_rejects_entered_tokio() {
    let Some(mut schema) = Schema::new() else { return };
    schema.seed();
    child(&schema.url, &schema.name, "context");
}

#[test]
fn configured_postgres_pin_rejects_missing_malformed_or_changed_configuration() {
    let Some(mut schema) = Schema::new() else { return };
    schema.seed();
    for action in ["config-removed", "config-malformed", "config-sqlite"] {
        child(&schema.url, &schema.name, action);
    }
}

#[test]
fn invalid_initial_cache_configuration_is_an_error_without_echoing_its_contents() {
    child("host=127.0.0.1 port=1 user=example dbname=example connect_timeout=1", "cache_fixture", "config-initial");
}

pub(crate) fn open_test_native_owner() -> anyhow::Result<crate::pg::indexer_runtime::Runtime> {
    let storage = crate::config::checked_storage_config().map_err(|_| anyhow::anyhow!("fixture storage configuration"))?;
    let mut config = crate::config::Config::default();
    config.postprocess.enabled = false;
    config.predb.enabled = false;
    config.spots.enabled = false;
    config.inferred_parts = false;
    config.media.tvmaze.enabled = false;
    config.media.allocine.enabled = false;
    config.media.details_per_minute = 0;
    let mut headers = crate::config::HeaderStoreConfig::default();
    headers.enabled = true;
    crate::pg::indexer_runtime::Runtime::open(&config, storage, headers, &[], &mut None)
}

fn bind_test_raw_store() {
    let home = std::path::PathBuf::from(std::env::var_os("ATLAS_HOME").expect("owned child home"));
    atlas::raw_store::set_test_raw_path(Some(home.join("atlas.raw.db")));
}

fn assert_cached_positive() {
    bind_test_raw_store();
    assert_eq!(atlas::raw_store::get_opensubtitles(POSITIVE).checked().unwrap(), Some(Some(feature())));
}

fn assert_cache_error() {
    assert!(atlas::raw_store::get_opensubtitles(ABSENT).checked().is_err(), "unavailable cache became a miss");
    assert!(atlas::raw_store::put_opensubtitles(ABSENT, None).checked().is_err(), "unavailable write succeeded");
}

#[test]
#[ignore = "only the fixture parent may execute this isolated configured API child"]
fn configured_cache_child() {
    bind_test_raw_store();
    let action = std::env::var("ATLAS_CACHE_TEST_ACTION").unwrap();
    let native = if !matches!(action.as_str(), "unavailable" | "config-initial")
        && crate::config::checked_storage_config().unwrap().is_postgres() {
        Some(open_test_native_owner().expect("canonical owned cache fixture"))
    } else { None };
    let scope = native.as_ref().map(|native| native.db.bind_native_cache_scope().unwrap());
    match action.as_str() {
        "read" => {
            // Production's existing processing lanes are standard threads,
            // rather than Tokio spawn_blocking tasks with an entered Handle.
            let db = native.as_ref().unwrap().db.clone();
            std::thread::Builder::new().name("cache-stage-fixture".into()).spawn(move || {
                let _scope = db.bind_native_cache_scope().unwrap();
                assert_cached_positive();
                assert_eq!(atlas::raw_store::get_opensubtitles(NEGATIVE).checked().unwrap(), Some(None));
                assert_eq!(atlas::raw_store::get_opensubtitles(ABSENT).checked().unwrap(), None);
                let distinct = atlas::raw_store::get_opensubtitles(DIFFERENT_CASE).checked().unwrap().unwrap().unwrap();
                assert_eq!(distinct.imdb_id, Some(303));
                assert_eq!(distinct.title.as_deref(), Some("Case distinct"));
                assert_eq!(distinct.tmdb_id, None);
            }).unwrap().join().unwrap();
        }
        "write" => {
            atlas::raw_store::put_opensubtitles(POSITIVE, Some(&Feature { tmdb_id: Some(404), ..Default::default() }))
                .checked()
                .unwrap();
            atlas::raw_store::put_opensubtitles(POSITIVE, None).checked().unwrap();
        }
        "unavailable" => {
            assert!(open_test_native_owner().is_err(), "unavailable destination admitted a canonical owner");
            assert_cache_error();
        }
        "context" => {
            let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            runtime.block_on(async { assert_cache_error(); });
            drop(runtime);
            let db = native.as_ref().unwrap().db.clone();
            std::thread::Builder::new().name("cache-stage-fixture".into())
                .spawn(move || { let _scope = db.bind_native_cache_scope().unwrap(); assert_cached_positive(); })
                .unwrap().join().unwrap();
        }
        action @ ("config-removed" | "config-malformed" | "config-sqlite") => {
            assert_cached_positive();
            let path = std::path::PathBuf::from(std::env::var_os("ATLAS_HOME").unwrap()).join("config.toml");
            match action {
                "config-removed" => std::fs::remove_file(path).unwrap(),
                "config-malformed" => std::fs::write(path, "invalid = [").unwrap(),
                "config-sqlite" => {
                    let mut document: toml_edit::DocumentMut = std::fs::read_to_string(&path).unwrap().parse().unwrap();
                    document["storage"]["backend"] = toml_edit::value("sqlite");
                    std::fs::write(path, document.to_string()).unwrap();
                }
                _ => unreachable!(),
            }
            assert_cache_error();
        }
        "config-initial" => {
            let marker = "fixture-private-config-marker";
            let path = std::path::PathBuf::from(std::env::var_os("ATLAS_HOME").unwrap()).join("config.toml");
            std::fs::write(path, format!("invalid = [\"{marker}\"\n")).unwrap();
            let result = atlas::raw_store::get_opensubtitles(ABSENT);
            let error = result.error_text().expect("invalid config must report an error");
            assert!(!error.contains(marker), "configuration error echoed private contents");
            assert!(result.checked().is_err());
            assert_cache_error();
        }
        _ => panic!("unknown owned fixture action"),
    }
    drop(scope);
    if let Some(native) = native { native.owner.close(); }
}
