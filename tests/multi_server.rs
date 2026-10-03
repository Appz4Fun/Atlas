//! Several usenet servers: failover by priority, parallel requests, header compression.
//! Nothing here touches the environment (each test uses its own temp db).

mod common;

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use atlas::{db, indexer::Indexer, nntp::BlockingPool};
use common::*;

/// Several servers: the primary refuses the login so the next one by
/// priority indexes, XOVER goes out in parallel slices capped at that
/// server's connections, and a body it doesnt have comes from the third.
#[test]
fn failover_and_parallel_requests() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("atlas.db");
    db::create_db_at(&db_path).unwrap();

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

    let pool =
        BlockingPool::new(&[mock(p1, "not-what-it-wants", 4, 1), mock(p2, "secret", 4, 2), mock(p3, "secret", 2, 3)]);
    let refused_pool = BlockingPool::new(&[mock(p1, "secret", 4, 1)]);
    // sanity: the primary really is up, it just rejects these creds
    assert!(refused_pool.connect().is_ok());
    drop(refused_pool);

    pool.connect().unwrap();
    assert_eq!(pool.active_index(), 1, "should fall back to the priority 2 server");

    let mut indexer = Indexer::new(pool, "backfill", db::open_at(&db_path).unwrap());
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

    let conn = db::open_at(&db_path).unwrap();
    let releases: i64 = conn.query_row("select count(*) from releases", [], |r| r.get(0)).unwrap();
    assert_eq!(releases, 3000);

    let name: Option<String> =
        conn.query_row("select display_name from releases where name = 'Hidden.Thing'", [], |r| r.get(0)).unwrap();
    assert_eq!(name.as_deref(), Some("Found Via Fallback"), "body should come from the third server");

    let peak = busy.peak.load(Ordering::SeqCst);
    assert!((2..=4).contains(&peak), "expected 2-4 parallel connections to the busy server, saw {peak}");
    assert!(backup.peak.load(Ordering::SeqCst) <= 2);

    // cursors are kept per server once there are several
    let key: String = conn.query_row("select name from groups", [], |r| r.get(0)).unwrap();
    assert_eq!(key, format!("{GROUP}@127.0.0.1"));
}

/// A server that agrees to compression but sends junk gets it turned off,
/// and the slice is fetched again uncompressed.
#[test]
fn broken_compression_falls_back_to_plain() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("atlas.db");
    db::create_db_at(&db_path).unwrap();

    let posts: Vec<Post> = (1..=40).map(|n| post(n, &format!(r#""thing{n}.bin" yEnc (1/1)"#), 10, vec![])).collect();
    let mut state = Server::new(posts);
    {
        let s = Arc::get_mut(&mut state).unwrap();
        s.compress = true;
        s.compress_broken = true;
    }
    let port = spawn_server(state.clone());

    let pool = BlockingPool::new(&[mock(port, "secret", 2, 1)]);
    pool.connect().unwrap();
    let mut indexer = Indexer::new(pool, "backfill", db::open_at(&db_path).unwrap());
    indexer.request_size = 10;
    index_until_idle(&mut indexer);

    let conn = db::open_at(&db_path).unwrap();
    let n: i64 = conn.query_row("select count(*) from articles", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 40);
}
