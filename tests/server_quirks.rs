//! Provider behaviour seen in the wild: fewer connections allowed than
//! configured, article only servers that reject GROUP, and empty ranges
//! answered with 420.

use atlas as atlas_api;
#[path = "common/nntp.rs"]
mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use atlas::nntp::BlockingPool;
use common::{GROUP, Server, mock, post, spawn_server};

fn posts(n: u64) -> Vec<common::Post> {
    (1..=n).map(|i| post(i, &format!(r#""rel{}.part{}.rar" yEnc (1/1)"#, i / 20, i % 20 + 1), 10, vec![])).collect()
}

/// Configured for 8, the provider only takes 3: atlas lowers itself to what the
/// provider allows and every slice still comes back, nothing fails.
#[test]
fn lowers_connections_to_what_the_provider_allows() {
    let mut state = Server::new(posts(400));
    {
        let s = Arc::get_mut(&mut state).unwrap();
        s.max_conns = 3;
        s.xover_delay = Duration::from_millis(50);
    }
    let port = spawn_server(state.clone());

    let pool = BlockingPool::new(&[mock(port, "secret", 8, 1)]);
    pool.connect().unwrap();

    let slices: Vec<(u64, u64)> = (0..20).map(|i| (i * 20 + 1, i * 20 + 20)).collect();
    let (ok, failed) = pool.block_on(async {
        let mut rx = pool.pool.stream_headers(
            GROUP,
            0,
            slices,
            atlas::nntp::STREAM_SLICES,
            false,
            Arc::new(AtomicBool::new(false)),
        );
        let (mut ok, mut failed) = (0, 0);
        while let Some(slice) = rx.recv().await {
            if slice.result.is_ok() { ok += 1 } else { failed += 1 }
        }
        (ok, failed)
    });

    assert_eq!((ok, failed), (20, 0), "every slice should succeed");
    assert!(state.refused.load(Ordering::SeqCst) >= 1, "the mock should have refused some logins");
    assert!(pool.pool.connections(0) <= 3, "lowered to the provider's limit, got {}", pool.pool.connections(0));
    assert!(pool.pool.connections(0) >= 1);
}

/// A limit lowered by refusals climbs back towards the configured count: a provider refuses for a while, and the
/// configured connections are what atlas should get back to.
#[test]
fn a_lowered_connection_limit_climbs_back() {
    let mut state = Server::new(posts(400));
    {
        let s = Arc::get_mut(&mut state).unwrap();
        s.max_conns = 3;
        s.xover_delay = Duration::from_millis(50);
    }
    let port = spawn_server(state.clone());
    let pool = BlockingPool::from_pool(
        atlas::nntp::Pool::new(&[mock(port, "secret", 8, 1)]).with_limit_heal(Duration::from_millis(100)),
    );
    pool.connect().unwrap();
    let slices: Vec<(u64, u64)> = (0..20).map(|i| (i * 20 + 1, i * 20 + 20)).collect();
    let ok = pool.block_on(async {
        let mut rx = pool.pool.stream_headers(
            GROUP,
            0,
            slices,
            atlas::nntp::STREAM_SLICES,
            false,
            Arc::new(AtomicBool::new(false)),
        );
        let mut ok = 0;
        while let Some(slice) = rx.recv().await {
            ok += slice.result.is_ok() as usize;
        }
        ok
    });
    assert_eq!(ok, 20);
    let lowered = pool.pool.connections(0);
    assert!(lowered <= 3, "lowered to the provider's limit, got {lowered}");

    // once due, the next request takes one more permit back
    std::thread::sleep(Duration::from_millis(250));
    assert_eq!(pool.fetch_headers(GROUP, 1, 20).unwrap().len(), 20);
    assert_eq!(pool.pool.connections(0), lowered + 1, "a step back up");
}

/// A server that answers GROUP with 501 (fill / bonus servers) is switched to
/// article lookups only and its groups go to a server that can index.
#[test]
fn article_only_servers_stop_indexing() {
    let mut fill = Server::new(posts(50));
    Arc::get_mut(&mut fill).unwrap().no_group = true;
    let normal = Server::new(posts(50));

    let (pf, pn) = (spawn_server(fill.clone()), spawn_server(normal.clone()));
    let pool = BlockingPool::new(&[mock(pf, "secret", 4, 1), mock(pn, "secret", 4, 1)]);

    assert_eq!(pool.pool.indexing_servers(), vec![0, 1]);

    let (server, info) = pool.block_on(pool.pool.select_group_on(0, GROUP)).unwrap();
    assert_eq!(server, 1, "the group should move to the server that has GROUP");
    assert_eq!(info.2, 50);
    // one refusal can be a group name a real server doesnt like
    assert_eq!(pool.pool.indexing_servers(), vec![0, 1], "one refusal isnt enough");

    // refused again and again: an article only server
    for _ in 0..2 {
        pool.block_on(pool.pool.select_group_on(0, GROUP)).unwrap();
    }
    assert_eq!(pool.pool.indexing_servers(), vec![1], "the fill server is out of indexing");
    assert_eq!(pool.pool.pick_server("alt.binaries.anything"), 1);
}

/// A real server that rejects one odd group name keeps indexing the others.
#[test]
fn one_rejected_group_doesnt_take_a_server_out() {
    let normal = Server::new(posts(50));
    let port = spawn_server(normal.clone());
    let pool = BlockingPool::new(&[mock(port, "secret", 4, 1)]);

    // the mock answers 411 for unknown groups; a 501 for one name, then GROUP works
    for _ in 0..2 {
        let _ = pool.block_on(pool.pool.select_group_on(0, "alt.binaries.missing"));
    }
    assert!(pool.block_on(pool.pool.select_group_on(0, GROUP)).is_ok());
    assert_eq!(pool.pool.indexing_servers(), vec![0]);
}

/// A home server answering GROUP with 403 or 430 (`Failed forwarding`) only
/// couldnt answer just now: the pass fails and is tried again there, the group
/// doesnt move to another server (whose cursors it doesnt have). Only 411 moves it.
#[test]
fn a_failed_group_reply_doesnt_move_the_group() {
    let home = Server::new(posts(50));
    let other = Server::new(posts(50));
    let (ph, po) = (spawn_server(home.clone()), spawn_server(other.clone()));
    let pool = BlockingPool::new(&[mock(ph, "secret", 4, 1), mock(po, "secret", 4, 1)]);

    for reply in ["430 Failed forwarding", "403 Operation failed"] {
        home.group_replies.lock().unwrap().push_back(reply.to_string());
        let code = reply[..3].parse::<u16>().unwrap();
        let err = pool.block_on(pool.pool.select_group_on(0, GROUP)).unwrap_err();
        assert_eq!(err.code(), Some(code), "{reply}: the pass fails with the home's reply");
        let (server, _) = pool.block_on(pool.pool.select_group_on(0, GROUP)).unwrap();
        assert_eq!(server, 0, "{reply}: the group stays on its home");
    }
    assert!(other.groups_seen.lock().unwrap().is_empty(), "nothing was listed on the other server");

    // a home that lacks the group still hands it over
    home.group_replies.lock().unwrap().push_back("411 no such group".to_string());
    let (server, _) = pool.block_on(pool.pool.select_group_on(0, GROUP)).unwrap();
    assert_eq!(server, 1, "411 moves the group to a server that carries it");
}

/// A home that indexed the group before and answers 411 once (a backend in trouble) keeps it: the pass fails and is
/// tried there again. Only replies in a row that it lacks the group move it.
#[test]
fn a_home_that_indexed_a_group_loses_it_only_after_misses_in_a_row() {
    let home = Server::new(posts(50));
    let other = Server::new(posts(50));
    let (ph, po) = (spawn_server(home.clone()), spawn_server(other.clone()));
    let pool = BlockingPool::new(&[mock(ph, "secret", 4, 1), mock(po, "secret", 4, 1)]);
    let select = |known| pool.block_on(pool.pool.select_group_known(0, GROUP, known));

    // one miss, then the home answers again: it stays, and the count starts over
    home.group_replies.lock().unwrap().push_back("411 no such group".to_string());
    assert_eq!(select(true).unwrap_err().code(), Some(411));
    assert_eq!(select(true).unwrap().0, 0);
    // two in a row still fail the pass on the home; the third hands it over
    for _ in 0..3 {
        home.group_replies.lock().unwrap().push_back("411 no such group".to_string());
    }
    assert_eq!(select(true).unwrap_err().code(), Some(411));
    assert_eq!(select(true).unwrap_err().code(), Some(411));
    assert_eq!(select(true).unwrap().0, 1, "the third miss in a row moves the group");
    // a group the home never indexed moves on the first
    home.group_replies.lock().unwrap().push_back("411 no such group".to_string());
    assert_eq!(select(false).unwrap().0, 1);
}

/// A group that left its home for a while, and then left that server too, still goes back to the home it left first.
#[test]
fn a_group_moved_twice_goes_back_to_the_home_it_left_first() {
    let servers: Vec<_> = (0..3).map(|_| Server::new(posts(50))).collect();
    let ports: Vec<u16> = servers.iter().map(|s| spawn_server(s.clone())).collect();
    let cfgs: Vec<_> = ports.iter().map(|&p| mock(p, "secret", 4, 1)).collect();
    let pool = BlockingPool::from_pool(atlas::nntp::Pool::new(&cfgs).with_home_retry(std::time::Duration::ZERO));
    let miss = |i: usize, n: usize| {
        for _ in 0..n {
            servers[i].group_replies.lock().unwrap().push_back("411 no such group".to_string());
        }
    };
    let select = |prefer| pool.block_on(pool.pool.select_group_known(prefer, GROUP, true));

    // the home, 0, misses 3 times in a row: the group goes to one of the others
    miss(0, 3);
    assert!(select(0).is_err() && select(0).is_err());
    let first = select(0).unwrap().0;
    assert_ne!(first, 0);
    // that one misses too; the home still can't answer, so the group goes to the third
    miss(first, 3);
    miss(0, 1);
    assert!(select(first).is_err() && select(first).is_err());
    let second = select(first).unwrap().0;
    assert_eq!(second, 3 - first, "neither the home nor the server it just left");
    // once it is due, back to the home it left first, not the one in between
    assert_eq!(pool.pool.pick_server(GROUP), 0);
}

/// One rejected login is a short rest like any failed connect: some providers word a connection limit or a hiccup
/// like a bad password. Two in a row are the long rest, and a login that works clears the count.
#[test]
fn one_rejected_login_is_only_a_short_rest() {
    let state = Server::new(posts(20));
    state.reject_logins.store(1, Ordering::SeqCst);
    let port = spawn_server(state.clone());
    let pool = BlockingPool::new(&[mock(port, "secret", 2, 1)]);
    let status = || pool.pool.server_stats()[0].state;

    assert!(pool.connect().is_err());
    assert_eq!(status(), "resting", "one rejection: a short rest");
    pool.connect().unwrap();
    assert_eq!(status(), "ok");

    // a login that worked started the count over: one more is short again, a second in a row is the long rest
    state.reject_logins.store(2, Ordering::SeqCst);
    pool.pool.disconnect();
    assert!(pool.connect().is_err());
    assert_eq!(status(), "resting");
    assert!(pool.connect().is_err());
    assert_eq!(status(), "login rejected", "two in a row: the long rest");
}

/// Compressed headers that couldnt be read are asked for again after a while, on probation: one more unreadable
/// listing turns them off again, and the slice still comes back plain.
#[test]
fn compression_is_tried_again_after_a_while() {
    let mut state = Server::new(posts(40));
    {
        let s = Arc::get_mut(&mut state).unwrap();
        s.compress = true;
        s.compress_broken = true;
    }
    let port = spawn_server(state.clone());
    let pool = BlockingPool::from_pool(
        atlas::nntp::Pool::new(&[mock(port, "secret", 2, 1)]).with_compress_retry(Duration::from_millis(200)),
    );
    pool.connect().unwrap();
    let asked = || state.compress_asked.load(Ordering::SeqCst);

    assert_eq!(pool.fetch_headers(GROUP, 1, 20).unwrap().len(), 20, "read plain after the broken listing");
    let before = asked();
    assert!(before >= 1);

    // off: new connections are plain
    pool.pool.disconnect();
    assert_eq!(pool.fetch_headers(GROUP, 1, 20).unwrap().len(), 20);
    assert_eq!(asked(), before, "still off");

    // after the rest it is asked for again; one broken listing turns it off again, and the slice still comes back
    std::thread::sleep(Duration::from_millis(300));
    pool.pool.disconnect();
    assert_eq!(pool.fetch_headers(GROUP, 21, 40).unwrap().len(), 20);
    let retried = asked();
    assert!(retried > before, "asked for compression again");
    pool.pool.disconnect();
    assert_eq!(pool.fetch_headers(GROUP, 1, 20).unwrap().len(), 20);
    assert_eq!(asked(), retried, "off again after one more broken listing");
}

/// An XOVER that takes past its deadline fails (its slice is read again later) instead of holding the slice for as
/// long as the server likes.
#[test]
fn an_xover_past_its_deadline_fails() {
    let mut state = Server::new(posts(20));
    Arc::get_mut(&mut state).unwrap().xover_delay = Duration::from_millis(1500);
    let port = spawn_server(state.clone());
    let pool = BlockingPool::from_pool(
        atlas::nntp::Pool::new(&[mock(port, "secret", 2, 1)])
            .with_deadlines(Duration::from_millis(200), Duration::from_secs(5)),
    );
    pool.connect().unwrap();
    let start = std::time::Instant::now();
    let err = pool.fetch_headers(GROUP, 1, 20).unwrap_err();
    assert!(err.to_string().contains("took longer"), "{err}");
    assert!(start.elapsed() < Duration::from_millis(1200), "{:?}", start.elapsed());
}
