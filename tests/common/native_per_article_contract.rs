//! Retained per-article ingestion/search/NZB behavior on the original native owner.
use crate as atlas_api;
#[path = "native.rs"]
mod native;
#[path = "nntp.rs"]
mod nntp;

use crate::indexer::Progress;
use nntp::{GROUP, Server, mock, post, spawn_server};
use std::{
    io::Read,
    time::{Duration, Instant},
};

fn token(i: usize, len: usize) -> String {
    let mut x = (i as u64 + 1).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    let abc = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    (0..len)
        .map(|_| {
            x = x.wrapping_mul(0x100_0000_01b3) ^ (x >> 7);
            abc[(x % 62) as usize] as char
        })
        .collect()
}

#[test]
#[ignore = "requires explicit isolated ATLAS_PG_TEST_URL, native word keeper and actual API spool admission"]
fn articles_obfuscated_per_article_are_gathered_and_counted() {
    native::run_child_on("per-article", "indexer::native_per_article_contract::fixture_child");
}

#[test]
#[ignore = "internal exact native child; select the retained parent oracle"]
fn fixture_child() {
    assert_eq!(std::env::var("ATLAS_NATIVE_PROVIDER_CASE").expect("select native per-article parent"), "per-article");
    per_article();
    native::mark_finished("per-article");
}

fn response(query: &str) -> crate::api::ApiResponse {
    crate::api::handle("/api", &format!("{query}&apikey=k"), "http://atlas:9090", "k")
}
fn body(mut response: crate::api::ApiResponse) -> String {
    if let Some(mut spool) = response.spool.take() {
        let mut body = String::new();
        spool.read_to_string(&mut body).expect("actual native API spool");
        body
    } else {
        response.body
    }
}
fn per_article() {
    // These are the literal original three random-subject shapes and real upload.
    let mut subjects: Vec<String> = Vec::new();
    subjects.extend((2..42).map(|p| format!(r#"[41/62] "{}" yEnc ({p}/375)"#, token(p, 32))));
    subjects.extend((2..42).map(|p| format!(r#""{}" yEnc ({p}/5747)"#, token(p + 100, 24))));
    subjects.extend(
        (2..42).map(|p| format!(r#"[17/36] - "{}.{}" yEnc ({p}/59) 2088881050"#, token(p + 200, 8), token(p + 300, 8))),
    );
    subjects.extend((1..=30).map(|p| format!(r#"[1/1] - "Real.Upload.2026.mkv" yEnc ({p}/30)"#)));
    let last = subjects.len() as u64;
    assert_eq!(last, 150);
    let posts = subjects.iter().enumerate().map(|(i, s)| post(i as u64 + 1, s, 700_000, vec![])).collect();
    let server = Server::new(posts);
    let port = spawn_server(server);
    let mut harness = native::Harness::open(vec![mock(port, "secret", 4, 1)]);
    let mut indexer = harness.indexer("backfill");
    let mut seen = Progress::default();
    for _ in 0..50 {
        indexer
            .index_group_with(GROUP, &mut |progress| {
                seen.articles += progress.articles;
                seen.skipped += progress.skipped;
            })
            .unwrap();
        if indexer.is_idle(GROUP) {
            break;
        }
    }
    assert!(indexer.is_idle(GROUP));
    assert_eq!(seen.articles, last as i64, "every article counts as received");
    assert_eq!(seen.skipped, 120, "all three shapes");
    let rows = harness.release_rows();
    let names = rows.iter().map(|row| row.name.clone()).collect::<Vec<_>>();
    assert_eq!(names.len(), 4, "the real upload and three gathered: {names:?}");
    assert!(names.contains(&"Real.Upload.2026.mkv".to_string()));
    let gathered = rows.iter().find(|row| row.name.starts_with("per-article-")).expect("actual gathered release");
    let (name, id) = (gathered.name.clone(), gathered.id);
    harness.start_words();
    // Genuine Keeper builds/ACKs the actual native word source. Only the
    // real readonly query result ends this wait; no ready/certificate SQL.
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Ok(page) = harness.with_reader(|view| view.api_query(&name, None, None, 0, 10)) {
            if page.rows.first().map(|row| row.id) == Some(id) {
                break;
            }
        }
        assert!(Instant::now() < deadline, "actual native search did not find the gathered upload");
        std::thread::sleep(Duration::from_millis(20));
    }
    let found =
        harness.with_reader(|view| view.api_query(&name, None, None, 0, 10)).expect("actual native name search");
    assert_eq!(found.rows.first().map(|row| row.id), Some(id), "a gathered upload can be found");
    let whole = response(&format!("t=get&id={id}"));
    assert_eq!(whole.status, 200);
    let whole = body(whole);
    assert_eq!(whole.matches("<segment ").count(), 40, "{whole}");
    let one = response(&format!("t=get&id={id}&file=1"));
    let status = one.status;
    let one = body(one);
    assert_eq!((status, one.matches("<file ").count()), (200, 1));
    for missing in ["2", "0", "x"] {
        assert_eq!(response(&format!("t=get&id={id}&file={missing}")).status, 404, "{missing}");
    }
    let real = harness
        .with_reader(|view| view.api_query("real upload", None, None, 0, 10))
        .expect("actual native complete-upload search");
    assert!(real.rows[0].complete);
    let cursors = harness.cursors();
    let cursor = cursors.iter().find(|cursor| cursor.key == GROUP).expect("actual native group cursor");
    assert_eq!(cursor.live, Some(last as i64));
    let backfill = cursor.backfill.expect("actual backfill cursor");
    assert!(backfill < 1, "backfill reached the first article, cursor {backfill}");
    harness.finish(indexer, 150);
}
