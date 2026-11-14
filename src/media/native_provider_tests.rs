//! The 36 provider contracts on their real original-owner cache backend.
//! Each ignored LIB parent owns a fresh schema and a single isolated child.

use std::{
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::Arc,
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

#[path = "../../tests/common/http.rs"]
mod fixture_http;

const CHILD_CASE: &str = "ATLAS_NATIVE_PROVIDER_CASE";
const CHILD_SCHEMA: &str = "ATLAS_NATIVE_PROVIDER_SCHEMA";
const SELECTOR_PREFIX: &str = "media::native_provider_tests::";
const LOG_BYTES: usize = 64 * 1024;
const CHILD_DEADLINE: Duration = Duration::from_secs(120);

// This is the only case registry: it supplies registration and child dispatch.
macro_rules! provider_cases {
    ($register:ident) => {
        $register! {
            a_hit_takes_one_call,
            an_aka_matches,
            a_country_mismatch_falls_to_search,
            a_year_mismatch_falls_to_search,
            a_404_falls_to_search_then_misses_in_two_calls,
            a_wrong_show_is_rejected,
            a_429_pauses_for_retry_after,
            a_500_pauses_and_doubles,
            a_connection_refused_pauses,
            bad_json_is_bad_not_a_miss,
            the_bucket_paces_requests,
            a_two_request_lookup_finishes_at_the_lowest_rate,
            an_oversized_name_is_no_candidate,
            a_redirect_isnt_followed,
            no_token_within_two_seconds_gives_paused,
            a_huge_body_is_refused,
            tmdb_tv_takes_two_calls_and_finds_external_ids,
            tmdb_movie_passes_the_year_and_finds_imdb,
            tmdb_miss_takes_one_call,
            tmdb_picks_the_best_of_many,
            tmdb_null_ids_are_none,
            tmdb_401_is_bad_key_and_records_nothing,
            tmdb_key_never_reaches_the_log,
            trakt_sends_its_headers,
            trakt_movie_finds_all_its_ids,
            trakt_miss_is_an_empty_list,
            a_client_is_a_public_type,
            tvdb_logs_in_once_and_finds_its_ids,
            tvdb_matches_an_alias_a_country_and_a_year,
            tvdb_asks_only_for_tv,
            a_refused_tvdb_key_pauses_and_says_nothing_of_it,
            a_token_it_stops_taking_logs_in_again,
            a_login_with_no_token_is_bad_not_a_miss,
            an_empty_tvdb_search_is_a_miss,
            a_translated_name_matches,
            a_pin_goes_with_the_key_when_there_is_one,
        }
    };
}

macro_rules! register_cases {
    ($($case:ident),+ $(,)?) => {
        fn case_body(name: &str) -> Option<fn()> {
            match name {
                $(stringify!($case) => Some(cases::$case),)+
                _ => None,
            }
        }
        $(
            #[test]
            #[ignore = "requires explicit isolated PostgreSQL and the owned native provider child"]
            fn $case() {
                run_case(stringify!($case));
            }
        )+
    };
}
provider_cases!(register_cases);

struct Log {
    kept: Vec<u8>,
    overflow: bool,
}

// Retain a bounded prefix but keep draining: an overlong child cannot fill a pipe.
fn drain_log(mut stream: impl Read) -> io::Result<Log> {
    let mut kept = Vec::new();
    let mut overflow = false;
    let mut chunk = [0u8; 4096];
    loop {
        let count = match stream.read(&mut chunk) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            result => result?,
        };
        if count == 0 {
            break;
        }
        let admitted = count.min(LOG_BYTES - kept.len());
        kept.extend_from_slice(&chunk[..admitted]);
        overflow |= admitted != count;
    }
    Ok(Log { kept, overflow })
}

// Declared after Schema/home in run_case, so cleanup runs before either drops.
struct OwnedChild {
    child: Child,
    reaped: bool,
    out: Option<JoinHandle<io::Result<Log>>>,
    err: Option<JoinHandle<io::Result<Log>>>,
}

impl OwnedChild {
    fn new(child: Child) -> Self {
        let mut owned = Self {
            child,
            reaped: false,
            out: None,
            err: None,
        };
        let out = owned.child.stdout.take().expect("owned child stdout pipe");
        let err = owned.child.stderr.take().expect("owned child stderr pipe");
        owned.out = Some(thread::spawn(move || drain_log(out)));
        owned.err = Some(thread::spawn(move || drain_log(err)));
        owned
    }

    fn terminate_if_unreaped(&mut self) {
        if !self.reaped {
            let _ = self.child.kill();
            if self.child.wait().is_ok() {
                self.reaped = true;
            }
        }
    }

    fn wait(&mut self) -> io::Result<(ExitStatus, bool)> {
        let until = Instant::now() + CHILD_DEADLINE;
        loop {
            if let Some(status) = self.child.try_wait()? {
                self.reaped = true;
                return Ok((status, false));
            }
            if Instant::now() >= until {
                // The child has not been reaped; terminate before waiting.
                let _ = self.child.kill();
                let status = self.child.wait()?;
                self.reaped = true;
                return Ok((status, true));
            }
            thread::sleep(Duration::from_millis(20));
        }
    }

    fn logs(&mut self) -> (io::Result<Log>, io::Result<Log>) {
        fn join(reader: JoinHandle<io::Result<Log>>) -> io::Result<Log> {
            reader.join().map_err(|_| io::Error::other("owned log reader panicked"))?
        }
        // Join both even when either reader reports an I/O failure.
        let out = join(self.out.take().expect("owned stdout reader"));
        let err = join(self.err.take().expect("owned stderr reader"));
        (out, err)
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        self.terminate_if_unreaped();
        if let Some(reader) = self.out.take() {
            let _ = reader.join();
        }
        if let Some(reader) = self.err.take() {
            let _ = reader.join();
        }
    }
}

fn private_directory(path: &Path) {
    std::fs::create_dir(path).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
}

fn write_config(home: &Path, schema: &crate::pg::testing::Schema) {
    let mut config = toml_edit::DocumentMut::new();
    config["storage"]["backend"] = toml_edit::value("postgres");
    config["storage"]["url"] = toml_edit::value(schema.url.as_str());
    config["storage"]["schema"] = toml_edit::value(schema.name.as_str());
    config["storage"]["max_connections"] = toml_edit::value(1);
    config["storage"]["allow_plaintext"] = toml_edit::value(true);
    config["header_store"]["enabled"] = toml_edit::value(true);
    config["header_store"]["dir"] = toml_edit::value(home.join("headers").to_str().unwrap());
    config["postprocess"]["enabled"] = toml_edit::value(false);
    config["predb"]["enabled"] = toml_edit::value(false);
    config["spots"]["enabled"] = toml_edit::value(false);
    config["media"]["tvmaze"]["enabled"] = toml_edit::value(false);
    config["media"]["allocine"]["enabled"] = toml_edit::value(false);
    config["media"]["details_per_minute"] = toml_edit::value(0);
    config["inferred_parts"] = toml_edit::value(false);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let pending = home.join("config.toml.new");
    let mut file = options.open(&pending).unwrap();
    file.write_all(config.to_string().as_bytes()).unwrap();
    file.sync_all().unwrap();
    drop(file);
    std::fs::rename(pending, home.join("config.toml")).unwrap();
}

struct OwnedRuntime(crate::pg::indexer_runtime::Runtime);
impl Drop for OwnedRuntime {
    fn drop(&mut self) {
        self.0.owner.close();
    }
}

fn completion(case: &str) -> String {
    format!("owned native provider case completed: {case}")
}

fn configured_case(case: &str, body: impl FnOnce(&crate::pg::indexer_runtime::Runtime)) {
    let home = PathBuf::from(std::env::var_os("ATLAS_HOME").expect("private child home"));
    let config = crate::config::load_config().expect("selected private fixture configuration");
    let storage = crate::config::checked_storage_config().expect("checked private PostgreSQL destination");
    assert_eq!(storage.schema, std::env::var(CHILD_SCHEMA).expect("parent-owned schema"));
    let headers = crate::config::header_store_config();
    assert!(headers.enabled);
    assert_eq!(headers.root(), home.join("headers"));
    let runtime = OwnedRuntime(
        crate::pg::indexer_runtime::Runtime::open(&config, storage, headers.clone(), &[], &mut None)
            .expect("real original native owner admission"),
    );
    assert_eq!(runtime.0.configured_headers(), headers);
    let owned_headers = home.join("headers").canonicalize().expect("canonical owned header directory");
    assert!(runtime.0.header_config().unwrap().root().starts_with(owned_headers));
    let cache = Arc::new(crate::pg::service_cache::Cache::new(&runtime.0.owner).unwrap());
    cache.check().expect("actual owner/config/epoch/cache preflight");
    let retained = cache.clone();
    // A constructor context, NOT an Owner::with callback or transaction over HTTP.
    super::cache::native(cache, || body(&runtime.0));
    retained.check().expect("same original owner remains valid after the case");
    assert_eq!(runtime.0.owner.failure(), None, "provider case must not fail its native owner");
    drop(retained);
    drop(runtime);
    // Every provider/server fixture has returned and the owner has closed.
    println!("{}", completion(case));
}

fn run_case(case: &str) {
    let body = case_body(case).expect("closed provider case registry");
    let selector = format!("{SELECTOR_PREFIX}{case}");
    run_owned_case(&selector, case, true, |_| body());
}

// These existing default selectors share the reviewed provider fixture's exact
// selected-config/original-owner admission. They remain unignored parent tests.
pub(super) fn run_runtime_case(selector: &'static str, body: impl FnOnce(&crate::pg::indexer_runtime::Runtime)) {
    assert!(matches!(selector,
        "media::native_cache_tests::native_title_claims_keep_capacity_and_release_on_guard_drop"
        | "media::steps::tests::a_spots_kind_gives_its_title_to_look_up"
        | "media::steps::tests::each_step_has_its_lane"
        | "postprocess::native::media_settlement_tests::a_files_own_tags_pin_its_ids_against_a_lookup_by_name"
    ), "closed native runtime fixture selector");
    run_owned_case(selector, selector, false, body);
}

fn run_owned_case(
    selector: &str,
    case: &str,
    ignored: bool,
    body: impl FnOnce(&crate::pg::indexer_runtime::Runtime),
) {
    if let Some(selected) = std::env::var_os(CHILD_CASE) {
        assert_eq!(selected.to_str(), Some(case), "mismatched owned child case");
        configured_case(case, body);
        return;
    }
    assert!(cfg!(unix), "native provider fixture requires private Unix file permissions");
    let schema = crate::pg::testing::Schema::new().expect("ATLAS_PG_TEST_URL must select isolated PostgreSQL");
    let home = tempfile::tempdir().unwrap();
    private_directory(&home.path().join("tmp"));
    private_directory(&home.path().join("headers"));
    write_config(home.path(), &schema);
    let mut command = Command::new(std::env::current_exe().unwrap());
    command.args(["--exact", selector, "--nocapture", "--test-threads=1"]);
    if ignored {
        command.arg("--ignored");
    }
    let child = command
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", home.path())
        .env("ATLAS_HOME", home.path())
        .env("TMPDIR", home.path().join("tmp"))
        .env("TMP", home.path().join("tmp"))
        .env("ATLAS_SAB_DIR", home.path().join("no-sabnzbd"))
        .env("ATLAS_NO_KEYRING", "1")
        .env(CHILD_CASE, case)
        .env(CHILD_SCHEMA, &schema.name)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("owned provider child spawn");
    let mut child = OwnedChild::new(child);
    let waited = child.wait();
    if waited.is_err() {
        child.terminate_if_unreaped();
    }
    let (out, err) = child.logs();
    // Drop reaps on a wait error and joins any remaining readers before Schema.
    drop(child);
    let (status, timed_out) = waited.expect("owned provider child wait");
    let out = out.expect("owned provider stdout drain");
    let err = err.expect("owned provider stderr drain");
    assert!(!timed_out, "owned provider case {case} exceeded its 120-second deadline");
    assert!(!out.overflow && !err.overflow, "owned provider case {case} exceeded its per-pipe 64-KiB log cap");
    // Do not copy raw child output into parent failures: it can contain a driver
    // diagnostic about the private selected destination. Status/case identifies
    // the exact isolated assertion for Root's separately owned diagnostics.
    assert!(status.success(), "owned native provider case {case} failed: {status}; child output kept private");
    let marker = completion(case);
    assert_eq!(String::from_utf8_lossy(&out.kept).matches(&marker).count(), 1, "missing owned case completion");
    // stderr is drained and admitted even when successful libtest writes none.
    drop(err);
    drop(home);
    drop(schema);
}

mod cases {
//! The providers, each against a loopback server answering from fixtures.

use crate as atlas;

use std::time::{Duration, Instant};

use atlas::config::{TmdbConfig, TraktConfig, TvdbConfig, TvmazeConfig};
use atlas::media::http::Client;
use atlas::media::resolve::{Answer, Ids, Provider, TitleQuery, Trouble};
use atlas::media::title::Kind;
use atlas::media::tmdb::Tmdb;
use atlas::media::trakt::Trakt;
use atlas::media::tvdb::Tvdb;
use atlas::media::tvmaze::Tvmaze;
use super::fixture_http::*;

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

pub(super) fn a_hit_takes_one_call() {
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

pub(super) fn an_aka_matches() {
    let s = FixtureServer::start(vec![single()]);
    assert_eq!(found(maze(&s, FAST).find(&tv("Some Alias"))).tvmaze, Some(101));
}

pub(super) fn a_country_mismatch_falls_to_search() {
    let s = FixtureServer::start(vec![single(), search()]);
    let q = TitleQuery { country: Some("UK"), ..tv("Some Show") };
    assert_eq!(found(maze(&s, FAST).find(&q)).tvmaze, Some(103));
    assert_eq!(s.requests().len(), 2);
}

pub(super) fn a_year_mismatch_falls_to_search() {
    let s = FixtureServer::start(vec![single()]);
    let q = TitleQuery { year: Some(1990), ..tv("Some Show") };
    assert_eq!(maze(&s, FAST).find(&q), Ok(Answer::NotFound));
    assert_eq!(s.requests().len(), 2);
    assert!(s.requests()[1].starts_with("/search/shows"));
}

pub(super) fn a_404_falls_to_search_then_misses_in_two_calls() {
    let s = FixtureServer::start(vec![]);
    assert_eq!(maze(&s, FAST).find(&tv("Some Show")), Ok(Answer::NotFound));
    assert_eq!(s.requests().len(), 2);
}

pub(super) fn a_wrong_show_is_rejected() {
    let s = FixtureServer::start(vec![single(), search()]);
    assert_eq!(maze(&s, FAST).find(&tv("Another Thing")), Ok(Answer::NotFound));
    assert_eq!(s.requests().len(), 2);
}

pub(super) fn a_429_pauses_for_retry_after() {
    let limited = Route { headers: vec![("Retry-After", "120".into())], ..status("/singlesearch/shows", 429) };
    let s = FixtureServer::start(vec![limited]);
    let p = maze(&s, FAST);
    let Err(Trouble::Paused(t)) = p.find(&tv("Some Show")) else { panic!("not paused") };
    near(t, 120);
    assert!(matches!(p.find(&tv("Some Show")), Err(Trouble::Paused(_))));
    assert_eq!(s.requests().len(), 1, "no request while paused");
}

pub(super) fn a_500_pauses_and_doubles() {
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

pub(super) fn a_connection_refused_pauses() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let cfg =
        TvmazeConfig { enabled: true, requests_per_minute: FAST, base_url: Some(format!("http://127.0.0.1:{port}")) };
    assert!(matches!(Tvmaze::new(&cfg).find(&tv("Some Show")), Err(Trouble::Paused(_))));
}

pub(super) fn bad_json_is_bad_not_a_miss() {
    let s = FixtureServer::start(vec![route("/singlesearch/shows", "<html>".into())]);
    assert!(matches!(maze(&s, FAST).find(&tv("Some Show")), Err(Trouble::Bad(_))));
}

pub(super) fn the_bucket_paces_requests() {
    let s = FixtureServer::start(vec![single()]);
    let p = maze(&s, 120);
    let start = Instant::now();
    for _ in 0..5 {
        found(p.find(&tv("Some Show")));
    }
    assert!(start.elapsed() >= Duration::from_millis(1_900), "{:?}", start.elapsed());
}

pub(super) fn a_two_request_lookup_finishes_at_the_lowest_rate() {
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

pub(super) fn an_oversized_name_is_no_candidate() {
    let name = "Some Show ".repeat(100_000);
    let show = serde_json::json!({"id": 101, "name": name, "premiered": "2015-03-02", "_embedded": {"akas": [{"name": "Some Alias"}]}});
    let s = FixtureServer::start(vec![route("/singlesearch/shows", show.to_string())]);
    assert_eq!(maze(&s, FAST).find(&tv("Some Show")), Ok(Answer::NotFound));
    let Ok(Answer::Found { title, .. }) = maze(&s, FAST).find(&tv("Some Alias")) else { panic!("the alias") };
    assert!(title.len() <= 255, "{}", title.len());
}

pub(super) fn a_redirect_isnt_followed() {
    let elsewhere = FixtureServer::start(vec![single()]);
    let redirect = Route {
        headers: vec![("Location", format!("{}/singlesearch/shows", elsewhere.base))],
        ..status("/singlesearch/shows", 302)
    };
    let s = FixtureServer::start(vec![redirect]);
    assert!(matches!(maze(&s, FAST).find(&tv("Some Show")), Err(Trouble::Bad(_))));
    assert!(elsewhere.requests().is_empty());
}

pub(super) fn no_token_within_two_seconds_gives_paused() {
    let s = FixtureServer::start(vec![single()]);
    let p = maze(&s, 1);
    found(p.find(&tv("Some Show")));
    let start = Instant::now();
    assert!(matches!(p.find(&tv("Some Show")), Err(Trouble::Paused(_))));
    assert!(start.elapsed() < Duration::from_secs(5));
    assert_eq!(s.requests().len(), 1);
}

pub(super) fn a_huge_body_is_refused() {
    let huge = format!("[{}]", " ".repeat(3 << 20));
    let s = FixtureServer::start(vec![route("/singlesearch/shows", huge)]);
    assert!(matches!(maze(&s, FAST).find(&tv("Some Show")), Err(Trouble::Bad(_))));
}

pub(super) fn tmdb_tv_takes_two_calls_and_finds_external_ids() {
    let s = FixtureServer::start(vec![
        route("/search/tv", fixture("tmdb_search_tv.json")),
        route("/tv/401/external_ids", fixture("tmdb_tv_ids.json")),
    ]);
    let ids = found(tmdb(&s, FAST).find(&tv("Some Show")));
    assert_eq!((ids.tmdb, ids.imdb, ids.tvdb, ids.tvrage), (Some(401), Some(100001), Some(3001), Some(2001)));
    assert_eq!(s.requests().len(), 2);
}

pub(super) fn tmdb_movie_passes_the_year_and_finds_imdb() {
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

pub(super) fn tmdb_miss_takes_one_call() {
    let s = FixtureServer::start(vec![route("/search/tv", fixture("tmdb_empty.json"))]);
    assert_eq!(tmdb(&s, FAST).find(&tv("Some Show")), Ok(Answer::NotFound));
    assert_eq!(s.requests().len(), 1);
}

pub(super) fn tmdb_picks_the_best_of_many() {
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

pub(super) fn tmdb_null_ids_are_none() {
    let s = FixtureServer::start(vec![
        route("/search/tv", fixture("tmdb_search_tv.json")),
        route("/tv/401/external_ids", r#"{"imdb_id":null,"tvdb_id":null}"#.into()),
    ]);
    let ids = found(tmdb(&s, FAST).find(&tv("Some Show")));
    assert_eq!((ids.tmdb, ids.imdb, ids.tvdb), (Some(401), None, None));
}

pub(super) fn tmdb_401_is_bad_key_and_records_nothing() {
    let s = FixtureServer::start(vec![status("/search/tv", 401)]);
    let Err(Trouble::BadKey(t)) = tmdb(&s, FAST).find(&tv("Some Show")) else { panic!("not a bad key") };
    near(t, 3_600);
}

pub(super) fn tmdb_key_never_reaches_the_log() {
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

pub(super) fn trakt_sends_its_headers() {
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

pub(super) fn trakt_movie_finds_all_its_ids() {
    let s = FixtureServer::start(vec![route("/search/movie", fixture("trakt_movie.json"))]);
    let ids = found(trakt(&s).find(&movie("Some Movie", Some(2019))));
    assert_eq!((ids.trakt, ids.imdb, ids.tmdb), (Some(602), Some(200002), Some(501)));
}

pub(super) fn trakt_miss_is_an_empty_list() {
    let s = FixtureServer::start(vec![route("/search/show", "[]".into())]);
    assert_eq!(trakt(&s).find(&tv("Some Show")), Ok(Answer::NotFound));
}

pub(super) fn a_client_is_a_public_type() {
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

pub(super) fn tvdb_logs_in_once_and_finds_its_ids() {
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

pub(super) fn tvdb_matches_an_alias_a_country_and_a_year() {
    let s = FixtureServer::start(tvdb_routes());
    let p = tvdb(&s);
    assert_eq!(found(p.find(&tv("Some Alias"))).tvdb, Some(3001));
    let uk = TitleQuery { country: Some("UK"), ..tv("Some Show") };
    assert_eq!(found(p.find(&uk)).tvdb, Some(3003), "gbr is UK");
    let old = TitleQuery { year: Some(1990), ..tv("Some Show") };
    assert_eq!(p.find(&old), Ok(Answer::NotFound));
    assert_eq!(p.find(&tv("Another Thing")), Ok(Answer::NotFound));
}

pub(super) fn tvdb_asks_only_for_tv() {
    let s = FixtureServer::start(tvdb_routes());
    assert!(tvdb(&s).asks(Kind::Tv) && !tvdb(&s).asks(Kind::Movie));
}

pub(super) fn a_refused_tvdb_key_pauses_and_says_nothing_of_it() {
    let s = FixtureServer::start(vec![status("/login", 401)]);
    let answer = tvdb(&s).find(&tv("Some Show"));
    let Err(Trouble::BadKey(t)) = &answer else { panic!("{answer:?}") };
    near(*t, 3_600);
    assert!(!format!("{answer:?}").contains("SECRETK"));
}

pub(super) fn a_token_it_stops_taking_logs_in_again() {
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

pub(super) fn a_login_with_no_token_is_bad_not_a_miss() {
    let s = FixtureServer::start(vec![route("/login", r#"{"status":"failure"}"#.into())]);
    assert!(matches!(tvdb(&s).find(&tv("Some Show")), Err(Trouble::Bad(_))));
}

pub(super) fn an_empty_tvdb_search_is_a_miss() {
    let s = FixtureServer::start(vec![
        route("/login", fixture("tvdb_login.json")),
        route("/search", r#"{"data":[]}"#.into()),
    ]);
    assert_eq!(tvdb(&s).find(&tv("Some Show")), Ok(Answer::NotFound));
}

pub(super) fn a_translated_name_matches() {
    let item = serde_json::json!({"data": [{"name": "Originaltitel", "tvdb_id": "3001", "translations": {"eng": "Some Show", "fra": "Une Serie"}, "name_translated": "Another Name"}]});
    let s = FixtureServer::start(vec![route("/login", fixture("tvdb_login.json")), route("/search", item.to_string())]);
    let p = tvdb(&s);
    assert_eq!(found(p.find(&tv("Some Show"))).tvdb, Some(3001));
    assert_eq!(found(p.find(&tv("Une Serie"))).tvdb, Some(3001));
    assert_eq!(found(p.find(&tv("Another Name"))).tvdb, Some(3001));
}

pub(super) fn a_pin_goes_with_the_key_when_there_is_one() {
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

}
