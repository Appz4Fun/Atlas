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
