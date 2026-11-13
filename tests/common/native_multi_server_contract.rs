//! Original catalog-backed multi-server controls on the real native owner.
use crate as atlas_api;
#[path = "native.rs"]
mod native;
#[path = "nntp.rs"]
mod nntp;

use crate::nntp::BlockingPool;
use nntp::*;
use std::{
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

#[test]
#[ignore = "requires explicit isolated ATLAS_PG_TEST_URL and real native provider/NXV fixture"]
fn failover_and_parallel_requests() {
    native::run_child_on("failover", "indexer::native_multi_server_contract::fixture_child");
}
#[test]
#[ignore = "requires explicit isolated ATLAS_PG_TEST_URL and real native provider/NXV fixture"]
fn broken_compression_falls_back_to_plain() {
    native::run_child_on("broken", "indexer::native_multi_server_contract::fixture_child");
}
#[test]
#[ignore = "requires explicit isolated ATLAS_PG_TEST_URL and real native provider/NXV fixture"]
fn one_range_that_wont_inflate_is_fetched_plain() {
    native::run_child_on("one-range", "indexer::native_multi_server_contract::fixture_child");
}
#[test]
#[ignore = "internal exact child; select a retained native parent oracle"]
fn fixture_child() {
    let case = std::env::var("ATLAS_NATIVE_PROVIDER_CASE").expect("select retained native multi-server parent");
    match case.as_str() {
        "failover" => failover(),
        "broken" => broken(),
        "one-range" => one_range(),
        _ => panic!("unknown native multi-server fixture"),
    }
    native::mark_finished(&case);
}
fn index_until_idle(indexer: &mut crate::indexer::Indexer) {
    for _ in 0..20 {
        indexer.index_group(GROUP).unwrap();
        if indexer.is_idle(GROUP) {
            return;
        }
    }
    panic!("indexer never went idle");
}

fn failover() {
    let nfo_body = vec![b"Release Name: Found Via Fallback".to_vec()];
    let mut posts: Vec<Post> =
        (1..3000).map(|n| post(n, &format!(r#""filler{n}.bin" yEnc (1/1)"#), 10, vec![])).collect();
    posts.push(post(3000, r#""Hidden.Thing.nfo" yEnc (1/1)"#, 10, nfo_body.clone()));

    let refuses = Server::new(posts.clone());
    let mut busy = Server::new(posts.clone());
    {
        let b = Arc::get_mut(&mut busy).unwrap();
        b.bodies = false;
        b.xover_delay = Duration::from_millis(150);
        b.compress = true;
    }
    let backup = Server::new(vec![post(3000, r#""Hidden.Thing.nfo" yEnc (1/1)"#, 10, nfo_body)]);

    let (p1, p2, p3) = (spawn_server(refuses.clone()), spawn_server(busy.clone()), spawn_server(backup.clone()));

    let configs = vec![mock(p1, "not-what-it-wants", 4, 1), mock(p2, "secret", 4, 2), mock(p3, "secret", 2, 3)];
    let pool = BlockingPool::new(&configs);
    let mut harness = native::Harness::open(configs);
    let refused_pool = BlockingPool::new(&[mock(p1, "secret", 4, 1)]);
    // sanity: the primary really is up, it just rejects these creds
    assert!(refused_pool.connect().is_ok());
    drop(refused_pool);

    pool.connect().unwrap();
    assert_eq!(pool.active_index(), 1, "should fall back to the priority 2 server");

    let mut indexer = harness.indexer_on(pool, "backfill");
    // 12 slices for 4 connections
    indexer.request_size = 250;
    let mut reported = 0;
    for _ in 0..20 {
        indexer.index_group_with(GROUP, &mut |p| reported += p.articles).unwrap();
        if indexer.is_idle(GROUP) {
            break;
        }
    }
    assert!(indexer.is_idle(GROUP));
    assert_eq!(reported, 3000, "progress hears about every slice");
    assert_eq!(busy.compressed_sent.load(Ordering::SeqCst), 12, "every XOVER should come back compressed");

    let releases = harness.with_reader(|view| view.count()).expect("actual native release count");
    assert_eq!(releases, 3000);
    let name = harness.display_name("Hidden.Thing");
    assert_eq!(name.as_deref(), Some("Found Via Fallback"), "body should come from the third server");

    let peak = busy.peak.load(Ordering::SeqCst);
    assert!((2..=4).contains(&peak), "expected 2-4 parallel connections to the busy server, saw {peak}");
    assert!(backup.peak.load(Ordering::SeqCst) <= 2);

    // cursors are kept per server once there are several, by host:port as
    // these share a host
    let cursors = harness.cursors();
    assert_eq!(cursors.len(), 1, "only the actual indexing provider has a cursor");
    assert_eq!(cursors[0].key, format!("{GROUP}@127.0.0.1:{p2}"));
    assert_eq!(cursors[0].live, Some(3000));
    assert_eq!(cursors[0].backfill, Some(0));
    harness.finish(indexer, 3000);
}

fn broken() {
    let posts: Vec<Post> = (1..=40).map(|n| post(n, &format!(r#""thing{n}.bin" yEnc (1/1)"#), 10, vec![])).collect();
    let mut state = Server::new(posts);
    {
        let s = Arc::get_mut(&mut state).unwrap();
        s.compress = true;
        s.compress_broken = true;
    }
    let port = spawn_server(state.clone());

    let configs = vec![mock(port, "secret", 2, 1)];
    let pool = BlockingPool::new(&configs);
    let mut harness = native::Harness::open(configs);
    pool.connect().unwrap();
    let mut indexer = harness.indexer_on(pool, "backfill");
    indexer.request_size = 10;
    index_until_idle(&mut indexer);

    let articles = harness.articles();
    assert_eq!(articles, 40);
    harness.finish(indexer, 40);
}

fn one_range() {
    let posts: Vec<Post> = (1..=40).map(|n| post(n, &format!(r#""thing{n}.bin" yEnc (1/1)"#), 10, vec![])).collect();
    let mut state = Server::new(posts);
    {
        let s = Arc::get_mut(&mut state).unwrap();
        s.compress = true;
        s.compress_broken_at = Some(21..=30);
        // the provider allows one connection: the plain one has to take the compressed one's place
        s.max_conns = 1;
    }
    let port = spawn_server(state.clone());

    let configs = vec![mock(port, "secret", 1, 1)];
    let pool = BlockingPool::new(&configs);
    let mut harness = native::Harness::open(configs);
    pool.connect().unwrap();
    let mut indexer = harness.indexer_on(pool, "backfill");
    indexer.request_size = 10;
    index_until_idle(&mut indexer);

    let articles = harness.articles();
    assert_eq!(articles, 40);
    assert_eq!(state.compressed_sent.load(Ordering::SeqCst), 3, "every other slice still comes compressed");
    harness.finish(indexer, 40);
}
