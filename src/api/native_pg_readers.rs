//! Test-only real released8 import -> actual HTTP dispatcher boundaries.
//! No reader9 generation is fabricated. Positive9 tests require its real
//! production producer/validator and separately admitted capacity/source input.
use crate as subject;
#[path = "../../tests/native_pg_web_fixture/mod.rs"]
mod fixture;

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;
use fixture::{ChildContext, KEY};

#[test]
#[cfg(target_os = "linux")]
#[ignore = "requires explicit isolated PostgreSQL + metadata observer qualification"]
fn metadata_audit_detects_real_failed_sqlite_and_owned_connection_controls() {
    let fixture = fixture::Fixture::new();
    fixture::audited_child(&fixture, "sqlite_control", false, fixture::AuditExpectation::SqliteControl);
    fixture::audited_child(&fixture, "connect_control", false, fixture::AuditExpectation::ConnectControl);
    fixture.finish();
}

#[test]
#[cfg(target_os = "linux")]
#[ignore = "requires explicit isolated PostgreSQL + audited HTTP child admission"]
fn released_identity_current_frozen_and_aliases_do_not_publish_public_readers() {
    let fixture = fixture::Fixture::new();
    assert!(fixture.expected_current_nzb.contains("part1-id3@example.invalid"));
    assert!(fixture.expected_frozen_nzb.contains("alt.binaries.payload"));
    assert!(!fixture.expected_frozen_nzb.contains("alt.binaries.header"));
    fixture::audited_child(&fixture, "refusal", true, fixture::AuditExpectation::Clean);
    fixture.finish();
}

#[test]
#[cfg(target_os = "linux")]
#[ignore = "requires explicit isolated PostgreSQL + audited HTTP child admission"]
fn all_unauthenticated_readers_precede_storage_and_keep_the_real_footer() {
    let fixture = fixture::Fixture::new();
    fixture::audited_child(&fixture, "auth", false, fixture::AuditExpectation::Clean);
    fixture.finish();
}

#[test]
#[cfg(target_os = "linux")]
#[ignore = "semantic RED candidate: remaining native web readers are not implemented"]
fn remaining_authenticated_readers_refuse_before_sqlite_provider_or_state_mutation() {
    let fixture = fixture::Fixture::new();
    fixture::audited_child(&fixture, "remaining", true, fixture::AuditExpectation::Clean);
    fixture.finish();
}

#[test]
#[cfg(target_os = "linux")]
#[ignore = "bootstrap CONTROL: separately admit real companion converters and released history8"]
fn real_companion_receipts_bootstrap_released_identity_before_reader9() {
    let mut fixture = fixture::Fixture::with_real_companions();
    fixture.assert_real_companion_control();
    fixture.finish();
}

#[test]
#[cfg(target_os = "linux")]
#[ignore = "positive semantic RED checkpoint: qualify companion CONTROL first; no reader9 publisher exists"]
fn real_companion_identity_requires_positive_current_and_frozen_http_nzb() {
    let mut fixture = fixture::Fixture::with_real_companions();
    fixture.assert_real_companion_control();
    fixture::audited_child(&fixture, "positive_nzb", true, fixture::AuditExpectation::Clean);
    fixture.finish();
}

const REDIRECTS: &[&str] = &["/ui/release?id=3", "/ui/duplicates?id=3", "/ui/stats", "/ui/same?id=3&kind=file",
    "/ui/settings", "/ui/settings/groups/search?q=fixture", "/ui/nzb?id=3"];
const FRAGMENTS: &[&str] = &["/ui/results?q=fixture", "/ui/total?q=fixture", "/ui/details?id=3", "/ui/pending?id=3",
    "/ui/poster?kind=2&tmdb=603", "/ui/cover?id=3", "/ui/still?id=3&at=25", "/ui/image?id=3&at=0",
    "/ui/samepicture?id=3", "/ui/picture?id=3&name=picture.jpg", "/ui/samefilm?id=3", "/ui/meta?id=3",
    "/ui/row?id=3", "/ui/stats/live", "/ui/stats/stored"];
const REFUSAL_API: &[&str] = &["/api?t=get&id=3", "/api?t=get&id=19", "/api?t=get&id=899", "/api?t=get&id=3&file=1",
    "/api?t=get&id=3&name=payload-3.bin", "/api?t=files&id=3", "/api?t=search&q=fixture", "/api?t=search",
    "/api?t=search&cat=2000", "/api?t=tvsearch&tvdbid=1&season=1&ep=2", "/api?t=movie&imdbid=0133093",
    "/api?t=search&deep=1", "/api?t=search&q=%21%3F", "/api?t=caps"];
const REMAINING: &[&str] = &["/", "/ui/results?q=fixture", "/ui/total?q=fixture", "/ui/results?q=fixture&sort=size&dir=desc",
    "/ui/results?q=fixture&filters=2160p&rated=60", "/ui/release?id=3", "/ui/release?id=19", "/ui/duplicates?id=3",
    "/ui/same?id=3&kind=file", "/ui/samepicture?id=3", "/ui/picture?id=3&name=picture.jpg", "/ui/samefilm?id=3",
    "/ui/pending?id=3", "/ui/stats", "/ui/stats/live", "/ui/stats/stored", "/ui/meta?id=3", "/ui/row?id=3",
    "/ui/settings", "/ui/settings/groups/search?q=fixture"];

struct Wire { status: u16, headers: Vec<(String, String)>, body: Vec<u8> }
impl Wire {
    fn header(&self, name: &str) -> Option<&str> { self.headers.iter().find(|(field, _)| field.eq_ignore_ascii_case(name)).map(|(_, value)| value.as_str()) }
    fn text(&self) -> &str { std::str::from_utf8(&self.body).unwrap() }
}
fn request(port: u16, method: &str, path: &str, cookie: Option<&str>, form: &str) -> Wire {
    let mut socket = TcpStream::connect(("127.0.0.1", port)).unwrap();
    socket.set_read_timeout(Some(Duration::from_secs(3))).unwrap(); socket.set_write_timeout(Some(Duration::from_secs(3))).unwrap();
    write!(socket, "{method} {path} HTTP/1.1\r\nHost: fixture.invalid\r\nConnection: close\r\n").unwrap();
    if let Some(cookie) = cookie { write!(socket, "Cookie: {cookie}\r\n").unwrap(); }
    if method == "POST" { write!(socket, "Content-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\n", form.len()).unwrap(); }
    write!(socket, "\r\n{form}").unwrap();
    let mut bytes = Vec::new(); socket.take(256 * 1024 + 1).read_to_end(&mut bytes).unwrap();
    assert!(bytes.len() <= 256 * 1024, "fixed response unexpectedly grew");
    let split = bytes.windows(4).position(|bytes| bytes == b"\r\n\r\n").expect("real HTTP header terminator");
    let head = std::str::from_utf8(&bytes[..split]).unwrap(); let mut lines = head.split("\r\n");
    let status = lines.next().unwrap(); assert!(status.starts_with("HTTP/1.1 "));
    let status = status.split_whitespace().nth(1).unwrap().parse().unwrap();
    let headers: Vec<(String, String)> = lines.map(|line| { let (field, value) = line.split_once(':').unwrap(); (field.into(), value.trim().into()) }).collect();
    let wire = Wire { status, headers, body: bytes[split + 4..].to_vec() };
    assert!(wire.header("Date").is_some() && wire.header("Server").is_some());
    if method != "HEAD" { assert_eq!(wire.header("Content-Length").unwrap().parse::<usize>().unwrap(), wire.body.len()); }
    wire
}
fn login(port: u16) -> String {
    let response = request(port, "POST", "/login", None, &format!("key={KEY}"));
    assert_eq!(response.status, 303); assert_eq!(response.header("Location"), Some("/"));
    let cookie = response.header("Set-Cookie").unwrap().split(';').next().unwrap().to_string();
    assert!(cookie.starts_with("atlas_session=") && !cookie.contains(KEY)); cookie
}
fn nzb_error(response: &Wire) {
    assert_eq!(response.status, 200, "retain Newznab status convention");
    assert!(response.header("Content-Type").unwrap().starts_with("application/xml"));
    assert!(response.text().contains("<error code=\"900\""));
    assert!(!response.text().contains("<nzb ") && response.header("Content-Disposition").is_none());
}

#[test]
fn reader_http_child() {
    let Some(path) = std::env::var_os("ATLAS_WEB_READER_FIXTURE") else { return; };
    let context: ChildContext = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    #[cfg(unix)] fixture::install_sqlite_observer(&context.sqlite_events);
    let storage = subject::config::storage_config();
    assert_eq!(storage.schema, context.schema); assert_eq!(subject::paths::app_dir(), context.home);
    if matches!(context.group.as_str(), "refusal" | "remaining" | "positive_nzb") {
        let mut client = postgres::Client::connect(&storage.url, postgres::NoTls).unwrap();
        assert_eq!(client.query_one("show default_transaction_read_only", &[]).unwrap().get::<_, String>(0), "on");
        assert_eq!(client.query_one("select oid from pg_namespace where nspname=$1", &[&context.schema]).unwrap().get::<_, u32>(0), context.schema_oid);
        assert!(context.schema.bytes().all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'));
        client.batch_execute(&format!("set search_path to \"{}\"", context.schema)).unwrap();
        let mut catalog = subject::pg::history_catalog::Snapshot::open(&mut client, context.import).unwrap();
        assert!(matches!(catalog.lookup(3, subject::pg::history_catalog::LookupPurpose::Raw, Default::default()).unwrap(), subject::pg::history_catalog::Lookup::Found(_)));
    }
    let media = subject::config::media_config();
    assert!(media.any_provider() && !media.tmdb.api_key.is_empty() && media.tvmaze.enabled,
        "configured providers cannot be replaced by disabled-key proof");
    assert_eq!(media.tmdb.image_base_url.as_deref(), Some("http://127.0.0.1:9"));
    assert_eq!(subject::config::load_settings().servers[0].host, "127.0.0.1");
    if context.group == "sqlite_control" {
        assert!(rusqlite::Connection::open(context.home.join("atlas.db")).is_err());
        assert!(rusqlite::Connection::open_in_memory().is_err()); return;
    }
    if context.group == "connect_control" {
        let reserved = std::net::TcpListener::bind(("127.0.0.1", context.http_port)).unwrap();
        let witness = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let socket = TcpStream::connect(witness.local_addr().unwrap()).unwrap();
        drop(socket); drop(witness); drop(reserved); return;
    }
    let server = tiny_http::Server::http(("127.0.0.1", context.http_port)).unwrap();
    let count = match context.group.as_str() { "auth" => 1 + REDIRECTS.len() + FRAGMENTS.len() + 2,
        "refusal" => 1 + REFUSAL_API.len() + 5 + 2, "remaining" => 1 + REMAINING.len(), "positive_nzb" => 3, _ => panic!("unknown fixed reader group") };
    let worker = std::thread::spawn(move || {
        for _ in 0..count {
            let request = server.recv_timeout(Duration::from_secs(3)).unwrap().expect("owned bounded HTTP request");
            super::respond(request, KEY, "fixture.invalid", &storage);
        }
    });
    match context.group.as_str() {
        "auth" => {
            let page = request(context.http_port, "GET", "/", None, ""); assert_eq!(page.status, 200);
            for token in ["class=\"site-footer\"", "Atlas - Rust Edition.", "https://github.com/Eraxty", "https://github.com/xbmc4lyfe",
                "Version:", "last git commit:", "License:", "https://www.gnu.org/licenses/gpl-3.0.txt", "Rust Version:", "Compilation Date:", "https://www.patreon.com/cw/xbmc4lyfe"] {
                assert!(page.text().contains(token), "full login page must keep {token}");
            }
            assert!(!page.text().contains(KEY));
            for path in REDIRECTS { let response = request(context.http_port, "GET", path, None, ""); assert_eq!(response.status, 303, "{path}"); assert_eq!(response.header("Location"), Some("/")); }
            for path in FRAGMENTS { let response = request(context.http_port, "GET", path, None, ""); assert_eq!(response.status, 401, "{path}"); assert_eq!(response.header("HX-Redirect"), Some("/")); }
            for path in ["/api?t=get&id=3", "/api?t=search&q=fixture"] { let response = request(context.http_port, "GET", path, None, ""); assert_eq!(response.status, 401); assert!(response.text().contains("code=\"100\"")); }
        }
        "refusal" => {
            let cookie = login(context.http_port);
            for path in REFUSAL_API {
                let path = if *path == "/api?t=caps" { path.to_string() } else { format!("{path}&apikey={KEY}") };
                nzb_error(&request(context.http_port, "GET", &path, Some(&cookie), ""));
            }
            for path in ["/ui/poster?kind=2&tmdb=603", "/ui/cover?id=3", "/ui/still?id=3&at=25", "/ui/image?id=3&at=0", "/ui/details?id=3"] {
                let response = request(context.http_port, "GET", path, Some(&cookie), ""); assert_eq!(response.status, 500, "identity-ready is not reader-ready: {path}");
            }
            let path = format!("/api?t=get&id=3&apikey={KEY}");
            let get = request(context.http_port, "GET", &path, Some(&cookie), "");
            let head = request(context.http_port, "HEAD", &path, Some(&cookie), "");
            assert_eq!(head.status, get.status); assert_eq!(head.header("Content-Length"), get.header("Content-Length")); assert!(head.body.is_empty());
            let mut config = subject::config::load_settings(); config.api_key = Some(KEY.into()); config.api_port = Some(0);
            let started = subject::api::start(&mut config); if started { subject::api::stop(); }
            assert!(!started, "actual FullWeb startup must remain closed after real identity publication");
        }
        "positive_nzb" => {
            let cookie = login(context.http_port);
            // Finish both fixed real HTTP exchanges before asserting, so an
            // expected current-response RED cannot strand a server request.
            let current = request(context.http_port, "GET", &format!("/api?t=get&id=3&apikey={KEY}"), Some(&cookie), "");
            let frozen = request(context.http_port, "GET", &format!("/api?t=get&id=19&apikey={KEY}"), Some(&cookie), "");
            for (kind, response, expected) in [("current", current, context.expected_current_nzb.as_str()), ("frozen", frozen, context.expected_frozen_nzb.as_str())] {
                assert_eq!(response.status, 200, "positive {kind} download status");
                assert!(!response.text().contains("<error"), "positive {kind} NZB was refused: {}", response.text());
                assert_eq!(response.text(), expected, "positive {kind} NZB must match the legacy oracle");
                assert!(response.header("Content-Disposition").is_some_and(|header| header.contains("attachment")), "positive {kind} attachment header");
                assert!(response.header("Content-Type").unwrap().contains("nzb"), "positive {kind} MIME type");
            }
        }
        "remaining" => {
            let cookie = login(context.http_port);
            let mut violations = Vec::new();
            for path in REMAINING {
                let response = request(context.http_port, "GET", path, Some(&cookie), "");
                if !matches!(response.status, 500 | 503) { violations.push(format!("{path}: {}", response.status)); }
            }
            assert!(violations.is_empty(), "unpublished native surfaces must refuse before legacy loaders: {violations:?}");
        }
        _ => unreachable!(),
    }
    worker.join().unwrap();
}
