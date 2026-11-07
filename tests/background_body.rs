//! Bodies for background work: indexing comes first, and a read stops at its cap.

mod common;

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use atlas::nntp::{BackgroundBody, Pool};
use common::{GROUP, Server, mock, post, spawn_server, yenc_body};

const MIB: usize = 1 << 20;

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread().worker_threads(4).enable_all().build().unwrap()
}

/// a server whose next `stalled` XOVERs sit on their reply
fn stalling(stalled: usize, stall: Duration, body: Vec<Vec<u8>>) -> Arc<Server> {
    let mut posts = vec![post(1, r#""a.bin" yEnc (1/1)"#, 10, body)];
    posts.push(post(2, r#""b.bin" yEnc (1/1)"#, 10, vec![]));
    let mut state = Server::new(posts);
    let s = Arc::get_mut(&mut state).unwrap();
    s.stall = stall;
    s.stalls_left.store(stalled, Ordering::SeqCst);
    state
}

#[test]
fn it_waits_while_indexing_needs_the_connections() {
    let rt = runtime();
    let state = stalling(3, Duration::from_secs(2), yenc_body("a.bin", b"hello"));
    let port = spawn_server(state.clone());
    let pool = Arc::new(Pool::new(&[mock(port, "secret", 4, 1)]));
    for _ in 0..3 {
        let pool = pool.clone();
        rt.spawn(async move { pool.fetch_headers(GROUP, 1, 2).await });
    }
    common::stage::wait_for("three stalled", || state.stalled.load(Ordering::SeqCst) == 3);
    let body = {
        let pool = pool.clone();
        rt.spawn(async move { pool.background_body("<msg1@mock>", MIB).await })
    };
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(state.bodies_sent.load(Ordering::SeqCst), 0, "one of four free isnt more than a quarter");
    assert_eq!(rt.block_on(body).unwrap().unwrap(), BackgroundBody::Body(b"hello".to_vec()));
    assert_eq!(state.bodies_sent.load(Ordering::SeqCst), 1);
}

#[test]
fn it_doesnt_take_a_connection_someone_waits_for() {
    let rt = runtime();
    let state = stalling(2, Duration::from_secs(2), yenc_body("a.bin", b"hello"));
    let port = spawn_server(state.clone());
    let pool = Arc::new(Pool::new(&[mock(port, "secret", 2, 1)]));
    // two stalled, and a third waiting for one of their connections
    for _ in 0..3 {
        let pool = pool.clone();
        rt.spawn(async move { pool.fetch_headers(GROUP, 1, 2).await });
    }
    common::stage::wait_for("two stalled", || state.stalled.load(Ordering::SeqCst) == 2);
    let body = {
        let pool = pool.clone();
        rt.spawn(async move { pool.background_body("<msg1@mock>", MIB).await })
    };
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(state.bodies_sent.load(Ordering::SeqCst), 0);
    assert_eq!(rt.block_on(body).unwrap().unwrap(), BackgroundBody::Body(b"hello".to_vec()));
}

#[test]
fn past_its_patience_it_queues_normally() {
    let rt = runtime();
    let state = stalling(1, Duration::from_secs(1), yenc_body("a.bin", b"hello"));
    let port = spawn_server(state.clone());
    let pool = Arc::new(Pool::new(&[mock(port, "secret", 1, 1)]).with_body_patience(Duration::from_millis(200)));
    {
        let pool = pool.clone();
        rt.spawn(async move { pool.fetch_headers(GROUP, 1, 2).await });
    }
    common::stage::wait_for("stalled", || state.stalled.load(Ordering::SeqCst) == 1);
    let got = rt.block_on(pool.background_body("<msg1@mock>", MIB)).unwrap();
    assert_eq!(got, BackgroundBody::Body(b"hello".to_vec()));
}

#[test]
fn every_server_saying_no_is_missing() {
    let rt = runtime();
    let mut state = Server::new(vec![post(1, r#""a.bin" yEnc (1/1)"#, 10, vec![])]);
    Arc::get_mut(&mut state).unwrap().bodies = false;
    let port = spawn_server(state);
    let pool = Pool::new(&[mock(port, "secret", 2, 1)]);
    assert_eq!(rt.block_on(pool.background_body("<msg1@mock>", MIB)).unwrap(), BackgroundBody::Missing);
}

/// "430 Failed forwarding" says the server couldnt ask its backend: with the others saying no, the body isnt Missing
/// but an error, read again later; with another one having it, that one's body comes back.
#[test]
fn failed_forwarding_is_an_error_not_missing() {
    let rt = runtime();
    let servers: Vec<Arc<Server>> = (0..2)
        .map(|i| {
            let mut s = Server::new(vec![post(1, r#""a.bin" yEnc (1/1)"#, 10, vec![b"hello".to_vec()])]);
            let s2 = Arc::get_mut(&mut s).unwrap();
            if i == 0 {
                s2.forwarding_fails = true;
            } else {
                s2.bodies = false;
            }
            s
        })
        .collect();
    let ports: Vec<u16> = servers.iter().map(|s| spawn_server(s.clone())).collect();
    let pool = Pool::new(&[mock(ports[0], "secret", 2, 1), mock(ports[1], "secret", 2, 2)]);
    let got = rt.block_on(pool.background_body("<msg1@mock>", MIB));
    assert!(got.as_ref().is_err_and(|e| e.failed_forwarding()), "{got:?}");

    // a plain 430 is still just "not here"
    let pool = Pool::new(&[mock(ports[1], "secret", 2, 1)]);
    assert_eq!(rt.block_on(pool.background_body("<msg1@mock>", MIB)).unwrap(), BackgroundBody::Missing);
}

#[test]
fn failed_forwarding_on_one_server_takes_the_body_from_another() {
    let rt = runtime();
    let servers: Vec<Arc<Server>> = (0..2)
        .map(|i| {
            let mut s = Server::new(vec![post(1, r#""a.bin" yEnc (1/1)"#, 10, vec![])]);
            Arc::get_mut(&mut s).unwrap().forwarding_fails = i == 0;
            s
        })
        .collect();
    let ports: Vec<u16> = servers.iter().map(|s| spawn_server(s.clone())).collect();
    let pool = Pool::new(&[mock(ports[0], "secret", 2, 1), mock(ports[1], "secret", 2, 2)]);
    assert!(matches!(rt.block_on(pool.background_body("<msg1@mock>", MIB)).unwrap(), BackgroundBody::Body(_)));
}

/// A body that takes past its deadline on one server comes from the next: a server trickling bytes held it for good.
#[test]
fn a_body_past_its_deadline_comes_from_the_next_server() {
    let rt = runtime();
    let servers: Vec<Arc<Server>> = (0..2)
        .map(|i| {
            let mut s = Server::new(vec![post(1, r#""a.bin" yEnc (1/1)"#, 10, vec![b"hello".to_vec()])]);
            if i == 0 {
                Arc::get_mut(&mut s).unwrap().body_delay = Duration::from_millis(1500);
            }
            s
        })
        .collect();
    let ports: Vec<u16> = servers.iter().map(|s| spawn_server(s.clone())).collect();
    let pool = Pool::new(&[mock(ports[0], "secret", 2, 1), mock(ports[1], "secret", 2, 2)])
        .with_deadlines(Duration::from_secs(5), Duration::from_millis(200));
    let start = std::time::Instant::now();
    let got = rt.block_on(pool.background_body("<msg1@mock>", MIB)).unwrap();
    assert!(matches!(got, BackgroundBody::Body(_)), "{got:?}");
    assert!(start.elapsed() < Duration::from_millis(1200), "didnt wait for the slow one: {:?}", start.elapsed());
}

#[test]
fn a_server_that_cant_be_reached_is_an_error_not_missing() {
    let rt = runtime();
    let mut state = Server::new(vec![post(1, r#""a.bin" yEnc (1/1)"#, 10, vec![])]);
    Arc::get_mut(&mut state).unwrap().bodies = false;
    let port = spawn_server(state);
    let dead = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let pool = Pool::new(&[mock(port, "secret", 2, 1), mock(dead, "secret", 2, 2)]);
    assert!(rt.block_on(pool.background_body("<msg1@mock>", MIB)).is_err());
}

#[test]
fn one_enormous_line_is_cut_at_the_cap_and_its_connection_goes() {
    let rt = runtime();
    let state = Server::new(vec![post(1, r#""a.bin" yEnc (1/1)"#, 10, vec![vec![b'x'; 8 * MIB]])]);
    let port = spawn_server(state.clone());
    let pool = Pool::new(&[mock(port, "secret", 2, 1)]);
    assert_eq!(rt.block_on(pool.background_body("<msg1@mock>", MIB)).unwrap(), BackgroundBody::TooBig);
    let wire = pool.server_stats()[0].wire_bytes;
    assert!(wire < (MIB + (200 << 10)) as u64, "{wire} bytes off the wire");
    let accepted = state.accepted.load(Ordering::SeqCst);
    let _ = rt.block_on(pool.background_body("<msg9@mock>", MIB));
    assert_eq!(state.accepted.load(Ordering::SeqCst), accepted + 1, "the next request logs in again");
}

#[test]
fn many_tiny_lines_are_too_big() {
    let rt = runtime();
    let state = Server::new(vec![post(1, r#""a.bin" yEnc (1/1)"#, 10, vec![vec![b'x']; 200_000])]);
    let port = spawn_server(state);
    let pool = Pool::new(&[mock(port, "secret", 2, 1)]);
    assert_eq!(rt.block_on(pool.background_body("<msg1@mock>", MIB)).unwrap(), BackgroundBody::TooBig);
}

#[test]
fn a_small_article_comes_back_decoded_and_its_connection_stays() {
    let rt = runtime();
    let state = Server::new(vec![post(1, r#""a.bin" yEnc (1/1)"#, 10, yenc_body("a.bin", b"payload"))]);
    let port = spawn_server(state.clone());
    let pool = Pool::new(&[mock(port, "secret", 2, 1)]);
    for _ in 0..2 {
        assert_eq!(
            rt.block_on(pool.background_body("<msg1@mock>", MIB)).unwrap(),
            BackgroundBody::Body(b"payload".to_vec())
        );
    }
    assert_eq!(state.accepted.load(Ordering::SeqCst), 1);
}

#[test]
fn a_server_in_trouble_is_an_error_not_missing() {
    let rt = runtime();
    let mut state = Server::new(vec![post(1, r#""a.bin" yEnc (1/1)"#, 10, vec![])]);
    Arc::get_mut(&mut state).unwrap().body_trouble = true;
    let port = spawn_server(state);
    let pool = Pool::new(&[mock(port, "secret", 2, 1)]);
    assert!(rt.block_on(pool.background_body("<msg1@mock>", MIB)).is_err());
}

/// a port nothing listens on
fn dead_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

fn without_bodies() -> u16 {
    let mut state = Server::new(vec![post(1, r#""a.bin" yEnc (1/1)"#, 10, vec![])]);
    Arc::get_mut(&mut state).unwrap().bodies = false;
    spawn_server(state)
}

/// A fallback server that can't be reached doesn't keep an article the others don't have from being missing, neither
/// the first time nor while it rests; alone, it's still an error.
#[test]
fn a_fallback_server_that_cant_be_reached_leaves_an_article_missing() {
    let rt = runtime();
    let (port, dead) = (without_bodies(), dead_port());
    let mut fallback = mock(dead, "secret", 2, 1);
    fallback.fallback = Some(true);
    let pool = Pool::new(&[fallback.clone(), mock(port, "secret", 2, 2)]);
    for _ in 0..2 {
        let start = std::time::Instant::now();
        assert_eq!(rt.block_on(pool.background_body("<msg1@mock>", MIB)).unwrap(), BackgroundBody::Missing);
        assert!(start.elapsed() < Duration::from_secs(5), "not waited for: {:?}", start.elapsed());
    }
    let alone = Pool::new(&[fallback]);
    assert!(rt.block_on(alone.background_body("<msg1@mock>", MIB)).is_err());
}

/// A fallback server is asked after every other one, whatever its priority or how idle it is.
#[test]
fn a_fallback_server_is_asked_last() {
    let rt = runtime();
    let servers: Vec<Arc<Server>> =
        (0..2).map(|_| Server::new(vec![post(1, r#""a.bin" yEnc (1/1)"#, 10, vec![b"hello".to_vec()])])).collect();
    let ports: Vec<u16> = servers.iter().map(|s| spawn_server(s.clone())).collect();
    let mut fallback = mock(ports[0], "secret", 2, 1);
    fallback.fallback = Some(true);
    let pool = Pool::new(&[fallback, mock(ports[1], "secret", 2, 2)]);
    for _ in 0..3 {
        assert!(matches!(rt.block_on(pool.background_body("<msg1@mock>", MIB)).unwrap(), BackgroundBody::Body(_)));
    }
    assert_eq!(servers[0].bodies_sent.load(Ordering::SeqCst), 0);
    assert_eq!(servers[1].bodies_sent.load(Ordering::SeqCst), 3);
}

/// A server that times out loses health and is asked after a healthy one the next time; one that answers gains it.
#[test]
fn a_server_that_times_out_is_asked_after_a_healthy_one() {
    let rt = runtime();
    let servers: Vec<Arc<Server>> = (0..2)
        .map(|i| {
            let mut s = Server::new(vec![post(1, r#""a.bin" yEnc (1/1)"#, 10, vec![b"hello".to_vec()])]);
            if i == 0 {
                Arc::get_mut(&mut s).unwrap().body_delay = Duration::from_millis(1500);
            }
            s
        })
        .collect();
    let ports: Vec<u16> = servers.iter().map(|s| spawn_server(s.clone())).collect();
    let pool = Pool::new(&[mock(ports[0], "secret", 2, 1), mock(ports[1], "secret", 2, 1)])
        .with_deadlines(Duration::from_secs(5), Duration::from_millis(200));
    assert!(matches!(rt.block_on(pool.background_body("<msg1@mock>", MIB)).unwrap(), BackgroundBody::Body(_)));
    assert!(pool.health(0) < 0.5 && pool.health(1) > 0.5, "{} {}", pool.health(0), pool.health(1));
    let start = std::time::Instant::now();
    assert!(matches!(rt.block_on(pool.background_body("<msg1@mock>", MIB)).unwrap(), BackgroundBody::Body(_)));
    assert!(start.elapsed() < Duration::from_millis(150), "the slow one wasnt asked first: {:?}", start.elapsed());
    assert_eq!(servers[1].bodies_sent.load(Ordering::SeqCst), 2);
}

/// While a regular server has no room for a body, an idle fallback server isn't asked in its place: the body waits
/// for the regular one.
#[test]
fn a_fallback_server_waits_while_a_regular_one_has_no_room() {
    let rt = runtime();
    let regular = stalling(1, Duration::from_secs(1), yenc_body("a.bin", b"hello"));
    let spare = Server::new(vec![post(1, r#""a.bin" yEnc (1/1)"#, 10, vec![b"hello".to_vec()])]);
    let mut fallback = mock(spawn_server(spare.clone()), "secret", 2, 1);
    fallback.fallback = Some(true);
    fallback.index = Some(false);
    let pool = Arc::new(Pool::new(&[mock(spawn_server(regular.clone()), "secret", 1, 1), fallback]));
    {
        let pool = pool.clone();
        rt.spawn(async move { pool.fetch_headers(GROUP, 1, 2).await });
    }
    common::stage::wait_for("the regular one stalled", || regular.stalled.load(Ordering::SeqCst) == 1);
    assert!(matches!(rt.block_on(pool.background_body("<msg1@mock>", MIB)).unwrap(), BackgroundBody::Body(_)));
    assert_eq!(spare.bodies_sent.load(Ordering::SeqCst), 0, "the fallback was asked first");
    assert_eq!(regular.bodies_sent.load(Ordering::SeqCst), 1);
}

fn troubled(times: usize) -> Arc<Server> {
    let s = Server::new(vec![post(1, r#""a.bin" yEnc (1/1)"#, 10, vec![b"hello".to_vec()])]);
    s.body_troubles_left.store(times, Ordering::SeqCst);
    s
}

/// A server that fails a body is asked again, up to `body_retries` more times, and its body comes back.
#[test]
fn a_server_that_fails_a_body_is_asked_again() {
    let rt = runtime();
    let state = troubled(2);
    let pool = Pool::new(&[mock(spawn_server(state.clone()), "secret", 2, 1)]);
    assert!(matches!(rt.block_on(pool.background_body("<msg1@mock>", MIB)).unwrap(), BackgroundBody::Body(_)));
    assert_eq!(state.body_troubles_left.load(Ordering::SeqCst), 0);
}

/// Past its retries a failing server is an error, after one try and as many more as `body_retries` says.
#[test]
fn a_server_failing_every_try_is_an_error_after_its_retries() {
    let rt = runtime();
    let state = troubled(10);
    let pool = Pool::new(&[mock(spawn_server(state.clone()), "secret", 2, 1)]);
    assert!(rt.block_on(pool.background_body("<msg1@mock>", MIB)).is_err());
    assert_eq!(state.body_troubles_left.load(Ordering::SeqCst), 10 - 1 - atlas::nntp::BODY_RETRIES);

    let state = troubled(10);
    let pool =
        Pool::new(&[mock(spawn_server(state.clone()), "secret", 2, 1)]).with_body_tries(Duration::from_secs(5), 0);
    assert!(rt.block_on(pool.background_body("<msg1@mock>", MIB)).is_err());
    assert_eq!(state.body_troubles_left.load(Ordering::SeqCst), 9);
}

/// A server that failed once and then says it hasn't the article leaves it missing: its last answer stands.
#[test]
fn a_retry_that_says_not_here_leaves_the_article_missing() {
    let rt = runtime();
    let mut s = Server::new(vec![post(1, r#""a.bin" yEnc (1/1)"#, 10, vec![])]);
    Arc::get_mut(&mut s).unwrap().bodies = false;
    s.body_troubles_left.store(1, Ordering::SeqCst);
    let pool = Pool::new(&[mock(spawn_server(s), "secret", 2, 1)]);
    assert_eq!(rt.block_on(pool.background_body("<msg1@mock>", MIB)).unwrap(), BackgroundBody::Missing);
}
