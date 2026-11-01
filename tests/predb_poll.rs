//! Polling, on a `Fetch` that opens no socket.

mod common;

use std::sync::Mutex;
use std::sync::atomic::Ordering;
use std::time::Duration;

use atlas::config::PredbConfig;
use atlas::predb::db::create;
use atlas::predb::feeds::{FeedSpec, page_url, spec};
use atlas::predb::poll::{HttpReply, PollEnd, REAL_REQUESTS, poll_source, wait_seconds};
use common::Scripted;
use rusqlite::{Connection, params};
use serde_json::json;

const NOW: i64 = 1_000_000;

struct Fixture {
    _dir: tempfile::TempDir,
    path: std::path::PathBuf,
    conn: Connection,
    fetch: std::sync::Arc<Scripted>,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("atlas.predb.db");
    let conn = create(&path).unwrap();
    Fixture { _dir: dir, path, conn, fetch: Scripted::new() }
}

fn cfg() -> PredbConfig {
    PredbConfig { enabled: true, ..Default::default() }
}

fn club() -> &'static FeedSpec {
    spec("predb_club").unwrap()
}

fn page(titles: &[String]) -> String {
    let rows: Vec<_> = titles.iter().map(|t| json!({"name": t})).collect();
    json!({"data": {"rows": rows}}).to_string()
}

/// `n` titles from `from`, `T00123` style
fn titles(from: usize, n: usize) -> Vec<String> {
    (from..from + n).map(|i| format!("Title.Number.T{i:05}-GRP")).collect()
}

impl Fixture {
    fn script(&self, spec: &FeedSpec, number: u32, body: &str) {
        self.fetch.reply(&page_url(spec, number, 100), 200, body);
    }

    fn poll(&mut self, spec: &FeedSpec, cfg: &PredbConfig) -> PollEnd {
        poll_source(&mut self.conn, self.fetch.as_ref(), spec, cfg, NOW, &|_| true).unwrap()
    }

    fn field(&self, source: i64, column: &str) -> i64 {
        self.conn.query_row(&format!("select {column} from feeds where source = ?1"), [source], |r| r.get(0)).unwrap()
    }

    fn stored(&self) -> i64 {
        self.conn.query_row("select count(*) from titles", [], |r| r.get(0)).unwrap()
    }

    fn requests(&self) -> Vec<String> {
        std::mem::take(&mut *self.fetch.urls.lock().unwrap())
    }

    /// as after a success
    fn had_a_success(&self, spec: &FeedSpec) {
        self.conn
            .execute(
                "insert or ignore into feeds (source, key, last_ok_at) values (?1, ?2, 1)",
                params![spec.id, spec.key],
            )
            .unwrap();
        self.conn.execute("update feeds set last_ok_at = 1 where source = ?1", [spec.id]).unwrap();
    }

    fn import(&mut self, spec: &FeedSpec, list: &[String]) {
        let entries: Vec<_> =
            list.iter().map(|t| atlas::predb::import::Entry { title: t.clone(), ..Default::default() }).collect();
        atlas::predb::import::import(&mut self.conn, spec.id, &entries, NOW, None).unwrap();
    }
}

#[test]
fn a_first_poll_reads_its_budget_and_leaves_no_gap() {
    let mut f = fixture();
    for p in 1..=3 {
        f.script(club(), p, &page(&titles(p as usize * 100, 10)));
    }
    assert_eq!(f.poll(club(), &cfg()), PollEnd::CaughtUp);
    assert_eq!(f.requests().len(), 3);
    assert_eq!(f.field(1, "resume_page"), 0);
    assert_eq!(f.stored(), 30);
    assert_eq!(REAL_REQUESTS.load(Ordering::Relaxed), 0);
}

#[test]
fn known_titles_stop_a_poll() {
    let mut f = fixture();
    f.import(club(), &titles(200, 10));
    f.script(club(), 1, &page(&titles(100, 10)));
    f.script(club(), 2, &page(&titles(200, 10)));
    assert_eq!(f.poll(club(), &cfg()), PollEnd::CaughtUp);
    assert_eq!(f.requests().len(), 2);
    assert_eq!(f.stored(), 20);
}

#[test]
fn a_gap_resumes_from_its_page() {
    let mut f = fixture();
    let old = titles(900, 10);
    f.import(club(), &old);
    f.had_a_success(club());
    for p in 1..=3 {
        f.script(club(), p, &page(&titles(p as usize * 100, 10)));
    }
    assert_eq!(f.poll(club(), &cfg()), PollEnd::Gap);
    assert_eq!(f.field(1, "resume_page"), 4);
    f.requests();

    // a restart between polls, by a new connection
    f.conn = create(&f.path).unwrap();
    f.script(club(), 1, &page(&titles(100, 10)));
    f.script(club(), 4, &page(&old));
    assert_eq!(f.poll(club(), &cfg()), PollEnd::CaughtUp);
    let asked = f.requests();
    assert!(asked[0].contains("page=1") && asked[1].contains("page=4"), "{asked:?}");
    assert_eq!(f.field(1, "resume_page"), 0);
}

#[test]
fn the_gap_survives_new_titles_pushing_pages_down() {
    let mut f = fixture();
    let old = titles(900, 10);
    f.import(club(), &old);
    f.had_a_success(club());
    // newest first: 100.. the gap's pages 3 and 4 hold 300 and 400
    for p in 1..=3 {
        f.script(club(), p, &page(&titles(p as usize * 100, 10)));
    }
    assert_eq!(f.poll(club(), &cfg()), PollEnd::Gap);
    // 10 new titles pushed everything down by a page
    f.script(club(), 1, &page(&titles(50, 10)));
    f.script(club(), 2, &page(&titles(100, 10)));
    f.script(club(), 4, &page(&titles(300, 10)));
    f.script(club(), 5, &page(&titles(400, 10)));
    let budget = PredbConfig { max_pages_per_poll: 20, ..cfg() };
    f.script(club(), 6, &page(&old));
    assert_eq!(f.poll(club(), &budget), PollEnd::CaughtUp);
    assert_eq!(f.field(1, "resume_page"), 0);
    assert_eq!(f.stored(), 10 + 3 * 10 + 10 + 10);
}

#[test]
fn a_full_page_shift_doesnt_close_the_gap() {
    let mut f = fixture();
    let old = titles(900, 10);
    f.import(club(), &old);
    f.had_a_success(club());
    for p in 1..=3 {
        f.script(club(), p, &page(&titles(p as usize * 100, 10)));
    }
    assert_eq!(f.poll(club(), &cfg()), PollEnd::Gap);
    assert_eq!(f.field(1, "resume_page"), 4);
    f.script(club(), 1, &page(&titles(100, 10)));
    // page 4 holds only what the last poll imported: not from before the gap
    f.script(club(), 4, &page(&titles(300, 10)));
    f.script(club(), 5, &page(&titles(400, 10)));
    assert_eq!(f.poll(club(), &cfg()), PollEnd::Gap);
    assert_eq!(f.field(1, "resume_page"), 6);
    f.script(club(), 1, &page(&titles(100, 10)));
    f.script(club(), 6, &page(&old));
    assert_eq!(f.poll(club(), &cfg()), PollEnd::CaughtUp);
    assert_eq!(f.field(1, "resume_page"), 0);
}

#[test]
fn a_second_gap_keeps_the_older_floor() {
    let mut f = fixture();
    let old = titles(900, 10);
    f.import(club(), &old);
    f.had_a_success(club());
    for p in 1..=3 {
        f.script(club(), p, &page(&titles(p as usize * 100, 10)));
    }
    assert_eq!(f.poll(club(), &cfg()), PollEnd::Gap);
    let floor = f.field(1, "gap_floor");
    for p in 1..=3 {
        f.script(club(), p, &page(&titles(1000 + p as usize * 100, 10)));
    }
    assert_eq!(f.poll(club(), &cfg()), PollEnd::Gap);
    assert_eq!(f.field(1, "gap_floor"), floor);
}

#[test]
fn predb_net_never_asks_past_page_100() {
    let mut f = fixture();
    let net = spec("predb_net").unwrap();
    f.import(net, &titles(100, 1));
    f.had_a_success(net);
    f.conn.execute("update feeds set resume_page = 100 where source = 2", []).unwrap();
    let row =
        |list: &[String]| json!({"data": list.iter().map(|t| json!({"release": t})).collect::<Vec<_>>()}).to_string();
    f.script(net, 1, &row(&titles(100, 1)));
    f.script(net, 100, &row(&titles(500, 5)));
    assert_eq!(f.poll(net, &cfg()), PollEnd::CaughtUp);
    let asked = f.requests();
    assert!(!asked.iter().any(|u| u.contains("page=101")), "{asked:?}");
    assert_eq!(f.field(2, "resume_page"), 0);
}

#[test]
fn an_empty_page_ends_it() {
    let mut f = fixture();
    f.had_a_success(club());
    f.conn.execute("update feeds set resume_page = 9 where source = 1", []).unwrap();
    f.script(club(), 1, &page(&[]));
    assert_eq!(f.poll(club(), &cfg()), PollEnd::CaughtUp);
    assert_eq!(f.field(1, "resume_page"), 0);
}

#[test]
fn an_xrel_cursor_past_its_last_page_ends_cleanly() {
    let mut f = fixture();
    let xrel = spec("xrel").unwrap();
    f.import(xrel, &titles(100, 1));
    f.had_a_success(xrel);
    f.conn.execute("update feeds set resume_page = 9, history_page = 9 where source = 4", []).unwrap();
    let known = json!({"list": [{"dirname": titles(100, 1)[0]}]}).to_string();
    f.script(xrel, 1, &known);
    let past = json!({"pagination": {"total_pages": 3}}).to_string();
    f.script(xrel, 9, &past);
    f.script(xrel, 9, &past);
    let config = PredbConfig { history_pages: 20, ..cfg() };
    assert_eq!(f.poll(xrel, &config), PollEnd::CaughtUp);
    assert_eq!((f.field(4, "resume_page"), f.field(4, "history_read"), f.field(4, "failures")), (0, 20, 0));
    let error: Option<String> =
        f.conn.query_row("select last_error from feeds where source = 4", [], |r| r.get(0)).unwrap();
    assert_eq!(error, None);
}

#[test]
fn history_reads_older_pages_then_stops() {
    let mut f = fixture();
    let known = titles(100, 5);
    f.import(club(), &known);
    f.had_a_success(club());
    let config = PredbConfig { history_pages: 4, ..cfg() };
    for p in 2..=5 {
        f.script(club(), p, &page(&titles(p as usize * 100, 5)));
    }
    f.script(club(), 1, &page(&known));
    assert_eq!(f.poll(club(), &config), PollEnd::CaughtUp);
    assert_eq!((f.field(1, "history_read"), f.field(1, "history_page")), (2, 4));
    f.conn = create(&f.path).unwrap();
    f.script(club(), 1, &page(&known));
    assert_eq!(f.poll(club(), &config), PollEnd::CaughtUp);
    assert_eq!(f.field(1, "history_read"), 4);
    f.requests();
    f.script(club(), 1, &page(&known));
    assert_eq!(f.poll(club(), &config), PollEnd::CaughtUp);
    assert_eq!(f.requests().len(), 1, "it stops at 4 pages read");
    assert_eq!(f.stored(), 25);

    let mut f = fixture();
    f.import(club(), &known);
    f.had_a_success(club());
    f.script(club(), 1, &page(&known));
    assert_eq!(f.poll(club(), &cfg()), PollEnd::CaughtUp);
    assert_eq!(f.requests().len(), 1, "no history unless asked");
}

#[test]
fn a_file_over_its_quota_stops_polling() {
    let mut f = fixture();
    let config = PredbConfig { max_db_mb: 0, ..cfg() };
    assert!(matches!(f.poll(club(), &config), PollEnd::Failed(_)));
    assert!(f.requests().is_empty());
    assert_eq!(f.field(1, "failures"), 1);
    f.script(club(), 1, &page(&[]));
    assert_eq!(f.poll(club(), &cfg()), PollEnd::CaughtUp);
    assert_eq!(f.field(1, "failures"), 0);
}

#[test]
fn a_page_of_too_many_rows_imports_nothing() {
    let mut f = fixture();
    f.script(club(), 1, &page(&titles(0, 1001)));
    assert!(matches!(f.poll(club(), &cfg()), PollEnd::Failed(_)));
    assert_eq!(f.stored(), 0);
}

fn limited(retry: Option<&str>, reset: Option<&str>) -> HttpReply {
    HttpReply {
        status: 429,
        retry_after: retry.map(String::from),
        rate_reset: reset.map(String::from),
        body: String::new(),
    }
}

#[test]
fn a_429_waits_as_told() {
    let mut f = fixture();
    f.fetch.reply_with(&page_url(club(), 1, 100), limited(Some("120"), None));
    assert_eq!(f.poll(club(), &cfg()), PollEnd::RateLimited(120));
    assert_eq!(f.field(1, "next_poll_at"), NOW + 120);
    assert_eq!(f.field(1, "failures"), 0);
    assert_eq!(f.requests().len(), 1, "nothing retried in this poll");

    assert_eq!(wait_seconds(&limited(Some("Thu, 01 Jan 1970 11:48:20 GMT"), None), 42_000, 60), 500);
    assert_eq!(wait_seconds(&limited(None, Some("42100")), 42_000, 60), 100);
    assert_eq!(wait_seconds(&limited(None, None), 42_000, 60), 60);
    assert_eq!(wait_seconds(&limited(Some("172800"), None), 0, 60), 172_800, "not shortened");
}

#[test]
fn a_servers_delay_counts_from_its_answer() {
    let mut f = fixture();
    f.script(club(), 1, &page(&titles(100, 5)));
    f.fetch.reply_with(&page_url(club(), 2, 100), limited(Some("120"), None));
    let pause = |d: Duration| {
        assert!(d >= Duration::from_secs(1));
        std::thread::sleep(Duration::from_millis(1_100));
        true
    };
    let got = poll_source(&mut f.conn, f.fetch.as_ref(), club(), &cfg(), NOW, &pause).unwrap();
    assert_eq!(got, PollEnd::RateLimited(120));
    assert!(f.field(1, "next_poll_at") >= NOW + 121, "{}", f.field(1, "next_poll_at"));
}

#[test]
fn a_one_page_budget_still_closes_a_gap_and_reads_history() {
    let mut f = fixture();
    let old = titles(900, 5);
    f.import(club(), &old);
    f.had_a_success(club());
    f.conn.execute("update feeds set resume_page = 7, gap_floor = 5 where source = 1", []).unwrap();
    f.script(club(), 1, &page(&old));
    f.script(club(), 7, &page(&old));
    let one = PredbConfig { max_pages_per_poll: 1, ..cfg() };
    assert_eq!(f.poll(club(), &one), PollEnd::CaughtUp);
    assert_eq!(f.field(1, "resume_page"), 0);

    let history = PredbConfig { history_pages: 1, ..one };
    f.script(club(), 1, &page(&old));
    f.script(club(), 2, &page(&titles(100, 5)));
    assert_eq!(f.poll(club(), &history), PollEnd::CaughtUp);
    assert_eq!(f.field(1, "history_read"), 1);

    // with nothing owed, the budget is as set
    f.requests();
    f.script(club(), 1, &page(&old));
    assert_eq!(f.poll(club(), &history), PollEnd::CaughtUp);
    assert_eq!(f.requests().len(), 1);
}

#[test]
fn a_rate_limit_envelope_is_the_same() {
    let mut f = fixture();
    f.script(club(), 1, include_str!("fixtures/predb/rate_limited.json"));
    assert_eq!(f.poll(club(), &cfg()), PollEnd::RateLimited(60));
    assert_eq!(f.field(1, "failures"), 0);
}

#[test]
fn failures_back_off_to_six_hours() {
    let mut f = fixture();
    let url = page_url(club(), 1, 100);
    for (round, wait) in
        [(1, 600), (2, 1_200), (3, 2_400), (4, 4_800), (5, 9_600), (6, 19_200), (7, 21_600), (8, 21_600)]
    {
        f.fetch.reply(&url, 500, "");
        assert!(matches!(f.poll(club(), &cfg()), PollEnd::Failed(_)));
        assert_eq!((f.field(1, "failures"), f.field(1, "next_poll_at")), (round, NOW + wait));
    }
    f.script(club(), 1, &page(&[]));
    assert_eq!(f.poll(club(), &cfg()), PollEnd::CaughtUp);
    assert_eq!(f.field(1, "failures"), 0);
    let error: Option<String> =
        f.conn.query_row("select last_error from feeds where source = 1", [], |r| r.get(0)).unwrap();
    assert_eq!(error, None);
}

#[test]
fn bad_json_and_bad_shape_are_failures() {
    let mut f = fixture();
    f.script(club(), 1, "{not json");
    assert!(matches!(f.poll(club(), &cfg()), PollEnd::Failed(_)));
    f.script(club(), 1, include_str!("fixtures/predb/no_list.json"));
    assert!(matches!(f.poll(club(), &cfg()), PollEnd::Failed(_)));
    assert_eq!(f.stored(), 0);
}

#[test]
fn pages_before_a_failure_are_kept() {
    let mut f = fixture();
    let old = titles(900, 5);
    f.import(club(), &old);
    f.had_a_success(club());
    f.script(club(), 1, &page(&titles(100, 5)));
    f.fetch.reply(&page_url(club(), 2, 100), 500, "");
    assert!(matches!(f.poll(club(), &cfg()), PollEnd::Failed(_)));
    assert_eq!(f.stored(), 10);
    assert_eq!(f.field(1, "resume_page"), 2);

    f.conn = create(&f.path).unwrap();
    f.script(club(), 1, &page(&titles(100, 5)));
    f.script(club(), 2, &page(&titles(200, 5)));
    f.script(club(), 3, &page(&old));
    assert_eq!(f.poll(club(), &cfg()), PollEnd::CaughtUp);
    assert_eq!(f.field(1, "resume_page"), 0);
    assert_eq!(f.stored(), 15);
}

#[test]
fn a_clean_forward_pass_leaves_no_gap() {
    let mut f = fixture();
    let old = titles(900, 5);
    f.import(club(), &old);
    f.had_a_success(club());
    f.script(club(), 1, &page(&titles(100, 5)));
    f.script(club(), 2, &page(&old));
    assert_eq!(f.poll(club(), &cfg()), PollEnd::CaughtUp);
    assert_eq!(f.field(1, "resume_page"), 0);

    // an older gap gets its page back, and then its walk
    f.conn.execute("update feeds set resume_page = 9, gap_floor = 1 where source = 1", []).unwrap();
    f.script(club(), 1, &page(&titles(200, 5)));
    f.script(club(), 2, &page(&old));
    f.script(club(), 9, &page(&old));
    assert_eq!(f.poll(club(), &cfg()), PollEnd::CaughtUp);
    assert_eq!(f.field(1, "resume_page"), 0);
}

#[test]
fn requests_are_spaced() {
    for (key, want) in [("predb_club", 1_000), ("xrel", 4_000)] {
        let mut f = fixture();
        let source = spec(key).unwrap();
        let row = |t: &str| {
            if key == "xrel" { json!({"list": [{"dirname": t}]}) } else { json!({"data": {"rows": [{"name": t}]}}) }
                .to_string()
        };
        f.script(source, 1, &row("First.Title.Here-GRP"));
        f.script(source, 2, &row("Second.Title.Here-GRP"));
        let waits = Mutex::new(Vec::new());
        let pause = |d: Duration| {
            waits.lock().unwrap().push(d.as_millis());
            true
        };
        let config = PredbConfig { max_pages_per_poll: 2, ..cfg() };
        poll_source(&mut f.conn, f.fetch.as_ref(), source, &config, NOW, &pause).unwrap();
        assert_eq!(*waits.lock().unwrap(), [want], "{key}");
    }
}

#[test]
fn stopping_ends_a_poll() {
    let mut f = fixture();
    f.script(club(), 1, &page(&titles(100, 5)));
    f.script(club(), 2, &page(&titles(200, 5)));
    let got = poll_source(&mut f.conn, f.fetch.as_ref(), club(), &cfg(), NOW, &|_| false).unwrap();
    assert_eq!(got, PollEnd::Stopped);
    assert_eq!(f.requests().len(), 1);
}

#[test]
fn an_rss_feed_goes_through_a_poll() {
    let mut f = fixture();
    let source = spec("predb_me").unwrap();
    let doc = r#"<rss><channel><item><title>Alpha.Beta.2024.1080p.WEB-DL.H.264-TESTGRP</title></item></channel></rss>"#;
    f.script(source, 1, doc);
    assert_eq!(f.poll(source, &cfg()), PollEnd::CaughtUp);
    let count: i64 =
        f.conn.query_row("select count(*) from titles where source = ?", [source.id], |r| r.get(0)).unwrap();
    assert_eq!(count, 1);
}

#[test]
fn a_catalogue_is_read_once_a_week() {
    let mut f = fixture();
    let source = spec("nswdb").unwrap();
    let doc =
        "<releases><release><id>1</id><releasename>Alpha.Beta.2024.NSW-TESTGRP</releasename></release></releases>";
    f.script(source, 1, doc);
    assert_eq!(f.poll(source, &cfg()), PollEnd::CaughtUp);
    let asked = f.fetch.urls.lock().unwrap().len();
    assert_eq!(f.poll(source, &cfg()), PollEnd::CaughtUp);
    assert_eq!(f.fetch.urls.lock().unwrap().len(), asked, "no request inside the week");
}

#[test]
fn one_answer_can_hold_only_so_many_titles() {
    let mut f = fixture();
    let source = spec("predb_me").unwrap();
    let items: String =
        (0..5_001).map(|n| format!("<item><title>Alpha.Beta.{n}.1080p-TESTGRP</title></item>")).collect();
    f.script(source, 1, &format!("<rss><channel>{items}</channel></rss>"));
    assert!(matches!(f.poll(source, &cfg()), PollEnd::Failed(_)));
    let count: i64 = f.conn.query_row("select count(*) from titles", [], |r| r.get(0)).unwrap();
    assert_eq!(count, 0);
}
