//! `/metrics`: what a scrape gets, from the NNTP pool, the catalog walk and the listener.

mod common;

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use atlas::metrics::{self, Role, catalog};
use atlas::nntp::Pool;
use atlas::{db, store};
use common::{GROUP, Server, mock, post, spawn_server, yenc_body};
use rusqlite::params;

fn get(url: &str) -> (u16, String) {
    let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).proxy(None).build().into();
    let mut reply = agent.get(url).call().unwrap();
    (reply.status().as_u16(), reply.body_mut().read_to_string().unwrap())
}

/// The value of the first line starting with `series`.
fn value(text: &str, series: &str) -> Option<f64> {
    text.lines().find_map(|l| l.strip_prefix(series)?.strip_prefix(' ')?.parse().ok())
}

/// Every line a scrape has is a comment or `name{labels} number`.
fn assert_well_formed(text: &str) {
    for line in text.lines().filter(|l| !l.starts_with('#')) {
        let (series, number) = line.rsplit_once(' ').unwrap_or_else(|| panic!("no value: {line}"));
        assert!(number == "NaN" || number.parse::<f64>().is_ok(), "{line}");
        let name = series.split('{').next().unwrap();
        assert!(name.starts_with("atlas_"), "{line}");
        assert_eq!(series.contains('{'), series.ends_with('}'), "{line}");
    }
}

#[test]
fn the_listener_answers_metrics_health_and_nothing_else() {
    let addr = metrics::serve("127.0.0.1:0", Role::Indexer).unwrap();
    let base = format!("http://{addr}");

    let (status, text) = get(&format!("{base}/metrics"));
    assert_eq!(status, 200);
    assert_well_formed(&text);
    assert_eq!(
        value(&text, &format!("atlas_build_info{{version=\"{}\",role=\"indexer\"}}", env!("CARGO_PKG_VERSION"))),
        Some(1.0)
    );
    assert!(value(&text, "atlas_process_start_time_seconds").is_some_and(|t| t > 1e9), "{text}");
    assert!(value(&text, "atlas_process_resident_memory_bytes").is_some_and(|b| b > 0.0), "{text}");

    assert_eq!(get(&format!("{base}/healthz")), (200, "ok\n".into()));
    assert_eq!(get(&format!("{base}/nope")).0, 404);
    let post = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .proxy(None)
        .build()
        .new_agent()
        .post(&format!("{base}/metrics"))
        .send("")
        .unwrap();
    assert_eq!(post.status().as_u16(), 405);
}

#[test]
fn a_port_in_use_is_an_error_not_a_panic() {
    let addr = metrics::serve("127.0.0.1:0", Role::Api).unwrap();
    assert!(metrics::serve(&addr.to_string(), Role::Api).is_err());
}

#[test]
fn a_held_port_is_taken_once_its_old_process_lets_go() {
    let held = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = held.local_addr().unwrap().to_string();
    metrics::serve_when_free(addr.clone(), Role::Indexer, Duration::from_millis(20));
    drop(held);

    let url = format!("http://{addr}/healthz");
    let agent: ureq::Agent = ureq::Agent::config_builder().proxy(None).build().into();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while agent.get(&url).call().is_err() {
        assert!(std::time::Instant::now() < deadline, "nothing came up on {addr}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn the_pool_counts_commands_bytes_connections_and_failures_per_server() {
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    let posts = vec![post(1, r#""a.bin" yEnc (1/1)"#, 10, yenc_body("a.bin", b"hello"))];
    let port = spawn_server(Server::new(posts));
    let pool = Arc::new(Pool::new(&[mock(port, "secret", 2, 1)]));
    let key = pool.server_stats()[0].key.clone();

    rt.block_on(async {
        assert_eq!(pool.fetch_headers(GROUP, 1, 1).await.unwrap().len(), 1);
        // not on the server: a miss, not an outage
        assert!(pool.fetch_body("<missing@mock>").await.is_err());
    });
    metrics::report_servers(&pool.server_stats());

    let text = metrics::render();
    assert_well_formed(&text);
    let labelled = |name: &str, rest: &str| format!("{name}{{server=\"{key}\",{rest}}}");
    assert_eq!(value(&text, &labelled("atlas_nntp_connects_total", "result=\"ok\"")).map(|n| n >= 1.0), Some(true));
    assert_eq!(value(&text, &labelled("atlas_nntp_commands_total", "command=\"XOVER\",result=\"ok\"")), Some(1.0));
    assert_eq!(value(&text, &labelled("atlas_nntp_commands_total", "command=\"LOGIN\",result=\"ok\"")), Some(1.0));
    let received = value(&text, &labelled("atlas_nntp_bytes_total", "command=\"XOVER\",direction=\"received\""));
    assert!(received.is_some_and(|b| b > 0.0), "{text}");
    let sent = value(&text, &labelled("atlas_nntp_bytes_total", "command=\"XOVER\",direction=\"sent\""));
    assert!(sent.is_some_and(|b| b > 0.0), "{text}");
    assert!(
        value(&text, "atlas_function_bytes_total{function=\"nntp.xover\",direction=\"received\"}")
            .is_some_and(|b| b > 0.0),
        "{text}"
    );
    assert_eq!(value(&text, &labelled("atlas_nntp_failures_total", "kind=\"no_such_article\"")), Some(1.0), "{text}");
    assert!(value(&text, &labelled("atlas_nntp_last_failure_timestamp_seconds", "kind=\"no_such_article\"")).is_some());
    assert_eq!(value(&text, &labelled("atlas_nntp_connections", "state=\"configured\"")), Some(2.0));
    assert_eq!(value(&text, &labelled("atlas_nntp_server_state", "state=\"ok\"")), Some(1.0));
    assert_eq!(value(&text, &labelled("atlas_nntp_server_state", "state=\"resting\"")), Some(0.0));
    assert_eq!(value(&text, &format!("atlas_nntp_waiting_requests{{server=\"{key}\"}}")), Some(0.0));

    // after a config reload: two accounts on one host and port, and the old server is gone
    let other = spawn_server(Server::new(Vec::new()));
    let pool = Pool::new(&[mock(other, "secret", 3, 1), mock(other, "secret", 7, 1)]);
    let stats = pool.server_stats();
    assert_eq!(stats[0].key, stats[1].key);
    metrics::report_servers(&stats);
    let text = metrics::render();
    let configured = format!("atlas_nntp_connections{{server=\"{}\",state=\"configured\"}}", stats[0].key);
    assert_eq!(value(&text, &configured), Some(10.0), "{text}");
    assert_eq!(value(&text, &labelled("atlas_nntp_connections", "state=\"configured\"")), None, "{text}");
    assert_eq!(value(&text, &labelled("atlas_nntp_server_state", "state=\"ok\"")), None, "{text}");
    assert!(value(&text, &format!("atlas_nntp_headers_total{{server=\"{key}\"}}")).is_some(), "counters stay: {text}");
}

#[test]
fn a_refused_connection_counts_as_a_failure_of_that_server() {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    // bound and dropped: nothing listens there
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let pool = Pool::new(&[mock(port, "secret", 1, 1)]).with_timeout(Duration::from_secs(2));
    let key = pool.server_stats()[0].key.clone();
    assert!(rt.block_on(pool.connect()).is_err());
    let text = metrics::render();
    assert_eq!(value(&text, &format!("atlas_nntp_connects_total{{server=\"{key}\",result=\"failed\"}}")), Some(1.0));
    assert_eq!(
        value(&text, &format!("atlas_nntp_failures_total{{server=\"{key}\",kind=\"io\"}}")),
        Some(1.0),
        "{text}"
    );
}

#[test]
fn the_catalog_walk_counts_every_release_once() {
    let dir = tempfile::tempdir().unwrap();
    let main = dir.path().join("atlas.db");
    db::create_db_at(&main).unwrap();

    // two groups, one blocked out of the numbers
    let (group, other) = ("alt.binaries.test", "alt.binaries.other");
    let insert = |group: &str,
                  id: i64,
                  name: &str,
                  size: i64,
                  display: Option<&str>,
                  obfuscated: i64,
                  source: Option<i64>| {
        let conn = db::open_shard(&store::shard_path(&main, store::shard_of(group))).unwrap();
        // a release id says which shard it is in
        let id = id * store::SHARDS as i64 + store::shard_of(group) as i64;
        conn.execute(
            "insert into releases (id, name, group_name, size, complete, parts, display_name, is_obfuscated, name_source)
             values (?1, ?2, ?3, ?4, 1, 3, ?5, ?6, ?7)",
            params![id, name, group, size, display, obfuscated, source],
        )
        .unwrap();
        id
    };
    let category = |group: &str, id: i64, category: i64| {
        let conn = db::open_shard(&store::shard_path(&main, store::shard_of(group))).unwrap();
        conn.execute("insert into release_category (release_id, category) values (?1, ?2)", params![id, category])
            .unwrap();
    };
    const GIB: i64 = 1 << 30;
    let movie = insert(group, 1, "Some.Movie.2024.1080p.WEB-DL.x264-GROUPA", 4 * GIB, None, 0, None);
    category(group, movie, 2040);
    let found =
        insert(group, 2, "abcdef0123456789", 8 * GIB, Some("Other.Movie.2023.2160p.WEB-DL.x265-GROUPA"), 1, Some(100));
    category(group, found, 2040);
    let tv = insert(other, 3, "Some.Show.S01E02.720p.HDTV.x264-GROUPB", GIB, None, 0, None);
    category(other, tv, 5040);
    insert(other, 4, "feedface01234567", 2 * GIB, None, 1, None);

    let conn = db::open_shard(&store::shard_path(&main, store::shard_of(group))).unwrap();
    conn.execute(
        "insert into predb_match (release_id, title_id, kind, category, matched_at) values (?1, 9, 1, 'MOVIE-HD-2160p', 1)",
        [found],
    )
    .unwrap();
    drop(conn);

    let stop = AtomicBool::new(false);
    let tally = catalog::scan(&main, &stop).unwrap().unwrap();
    assert_eq!(tally.rows, 4);
    tally.publish(Duration::from_secs(1));
    let text = metrics::render();
    assert_well_formed(&text);

    let at = |series: &str| value(&text, series).unwrap_or_else(|| panic!("no {series} in\n{text}"));
    assert_eq!(at("atlas_catalog_releases{state=\"total\"}"), 4.0);
    assert_eq!(at("atlas_catalog_releases{state=\"named\"}"), 1.0);
    assert_eq!(at("atlas_catalog_releases{state=\"still_obfuscated\"}"), 1.0);
    assert_eq!(at("atlas_catalog_releases{state=\"predb_matched\"}"), 1.0);
    assert_eq!(at("atlas_catalog_bytes"), (15 * GIB) as f64);
    assert_eq!(at("atlas_catalog_name_source_releases{source=\"predb\"}"), 1.0);
    assert_eq!(at("atlas_catalog_name_source_releases{source=\"posted\"}"), 3.0);
    assert_eq!(at("atlas_catalog_category_releases{category=\"Movies/HD\",root=\"Movies\"}"), 2.0);
    assert_eq!(at("atlas_catalog_category_releases{category=\"Uncategorized\",root=\"Uncategorized\"}"), 1.0);
    assert_eq!(at(&format!("atlas_catalog_group_releases{{group=\"{group}\"}}")), 2.0);
    assert_eq!(at(&format!("atlas_catalog_group_bytes{{group=\"{group}\"}}")), (12 * GIB) as f64);
    assert_eq!(at("atlas_catalog_release_group_releases{release_group=\"GROUPA\"}"), 2.0);
    assert_eq!(at("atlas_catalog_release_group_releases{release_group=\"GROUPB\"}"), 1.0);
    assert_eq!(at("atlas_catalog_video_releases{release_group=\"found\"}"), 3.0);
    // cumulative: both movies are at or below 8 GiB, one at or below 4
    assert_eq!(at("atlas_catalog_release_size_bytes{root=\"Movies\",le=\"4294967296\"}"), 1.0);
    assert_eq!(at("atlas_catalog_release_size_bytes{root=\"Movies\",le=\"8589934592\"}"), 2.0);
    assert_eq!(at("atlas_catalog_release_size_bytes{root=\"Movies\",le=\"+Inf\"}"), 2.0);
    assert_eq!(at("atlas_catalog_release_size_count{root=\"Movies\"}"), 2.0);
    assert_eq!(at("atlas_catalog_predb_kind_releases{kind=\"exact\"}"), 1.0);
    assert_eq!(at("atlas_catalog_predb_category_releases{category=\"MOVIE-HD-2160p\"}"), 1.0);
    assert_eq!(at("atlas_catalog_predb_root_releases{root=\"Movies\",matched=\"yes\"}"), 1.0);
    assert_eq!(at("atlas_catalog_predb_root_releases{root=\"Movies\",matched=\"no\"}"), 1.0);
    assert!(at("atlas_catalog_scan_timestamp_seconds") > 1e9);
}

#[test]
fn a_shard_out_of_the_way_fails_the_walk_instead_of_undercounting() {
    let dir = tempfile::tempdir().unwrap();
    let main = dir.path().join("atlas.db");
    db::create_db_at(&main).unwrap();
    assert!(catalog::scan(&main, &AtomicBool::new(false)).unwrap().is_some());
    std::fs::remove_file(store::shard_path(&main, 3)).unwrap();
    assert!(catalog::scan(&main, &AtomicBool::new(false)).is_err());
}

#[test]
fn a_stopped_walk_publishes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let main = dir.path().join("atlas.db");
    db::create_db_at(&main).unwrap();
    assert!(catalog::scan(&main, &AtomicBool::new(true)).unwrap().is_none());
}
