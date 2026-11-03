//! Reading an article's start, and hanging up on the rest.

mod common;

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use atlas::archive::PROBE_PREFIX;
use atlas::config::PostprocessConfig;
use atlas::nntp::{BackgroundBody, Pool};
use atlas::postprocess::step::{BodyRequest, Got, Needs, Outcome};
use common::stage::*;
use common::{Server, mock, post, spawn_server, yenc_lined};

const MIB: usize = 1 << 20;

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap()
}

fn data(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}

fn serving(body: Vec<Vec<u8>>) -> (Arc<Server>, Arc<Pool>) {
    // a second post, for "the next request" on the connection
    let state = Server::new(vec![
        post(1, r#""a.bin" yEnc (1/1)"#, 10, body),
        post(2, r#""b.bin" yEnc (1/1)"#, 10, yenc_lined("b.bin", b"next")),
    ]);
    let port = spawn_server(state.clone());
    (state, Arc::new(Pool::new(&[mock(port, "secret", 4, 1)])))
}

#[test]
fn a_long_body_is_cut_and_its_connection_goes() {
    let rt = runtime();
    let whole = data(MIB);
    let (state, pool) = serving(yenc_lined("a.bin", &whole));
    let got = rt.block_on(pool.background_prefix("<msg1@mock>", PROBE_PREFIX)).unwrap();
    let BackgroundBody::Body(b) = got else { panic!("{got:?}") };
    assert!(!b.is_empty() && b.len() <= PROBE_PREFIX, "{}", b.len());
    assert_eq!(b, whole[..b.len()]);

    let before = state.accepted.load(Ordering::SeqCst);
    let next = rt.block_on(pool.background_prefix("<msg2@mock>", PROBE_PREFIX)).unwrap();
    assert_eq!(next, BackgroundBody::Body(b"next".to_vec()));
    assert_eq!(state.accepted.load(Ordering::SeqCst), before + 1, "the cut connection was thrown away");
}

#[test]
fn a_short_body_comes_back_whole_and_the_connection_stays() {
    let rt = runtime();
    let whole = data(10_000);
    let (state, pool) = serving(yenc_lined("a.bin", &whole));
    let got = rt.block_on(pool.background_prefix("<msg1@mock>", PROBE_PREFIX)).unwrap();
    assert_eq!(got, BackgroundBody::Body(whole));
    let before = state.accepted.load(Ordering::SeqCst);
    let next = rt.block_on(pool.background_prefix("<msg2@mock>", PROBE_PREFIX)).unwrap();
    assert_eq!(next, BackgroundBody::Body(b"next".to_vec()));
    assert_eq!(state.accepted.load(Ordering::SeqCst), before, "it kept its connection");
}

#[test]
fn many_empty_lines_stop_a_prefix() {
    let rt = runtime();
    let (state, pool) = serving(vec![Vec::new(); 1_000_000]);
    let got = rt.block_on(pool.background_prefix("<msg1@mock>", 64 * 1024)).unwrap();
    let BackgroundBody::Body(b) = got else { panic!("{got:?}") };
    assert!(b.len() < 32_768, "{}", b.len());
    let before = state.accepted.load(Ordering::SeqCst);
    rt.block_on(pool.background_prefix("<msg2@mock>", 64 * 1024)).unwrap();
    assert_eq!(state.accepted.load(Ordering::SeqCst), before + 1, "the cut connection was thrown away");
}

#[test]
fn every_server_saying_no_is_missing() {
    let rt = runtime();
    let mut state = Server::new(vec![post(1, r#""a.bin" yEnc (1/1)"#, 10, yenc_lined("a.bin", b"x"))]);
    Arc::get_mut(&mut state).unwrap().bodies = false;
    let port = spawn_server(state);
    let pool = Pool::new(&[mock(port, "secret", 2, 1)]);
    assert_eq!(rt.block_on(pool.background_prefix("<msg1@mock>", PROBE_PREFIX)).unwrap(), BackgroundBody::Missing);
}

#[test]
fn a_server_that_cant_be_reached_is_an_error_not_missing() {
    let rt = runtime();
    let dead = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let pool = Pool::new(&[mock(dead, "secret", 2, 1)]);
    assert!(rt.block_on(pool.background_prefix("<msg1@mock>", PROBE_PREFIX)).is_err());
}

#[test]
fn fetch_prefix_makes_every_check() {
    let log = Log::default();
    let mut h = fresh();
    let release = complete("Rel.One");
    let id = release.articles[0].message_id.clone();
    h.save(&[release]);
    let state = Server::new(vec![atlas_post(&id)]);
    let port = spawn_server(state.clone());

    let answers: Arc<Mutex<Vec<Got>>> = Default::default();
    let a = answers.clone();
    let step = TestStep::new(1, "body", 100, Needs::Bodies, &log).runs(move |ctx| {
        let ask = |file: &str, id: &str| BodyRequest { message_id: id.into(), file: file.into(), max_bytes: Some(64) };
        let image = ctx.fetch_prefix(&ask("a.jpg", "<no@x>"))?.got;
        let first = ctx.fetch_prefix(&ask("a.bin", &id))?.got;
        let past = ctx.fetch_prefix(&ask("a.bin", &id))?.got;
        a.lock().unwrap().extend([image, first, past]);
        Ok(Outcome::default())
    });
    let config = PostprocessConfig {
        body_min_size_bytes: 0,
        body_fetches_per_minute: 6000,
        bodies_per_release: 1,
        ..quick_config()
    };
    h.start_with_pool(vec![step.arc()], config, Arc::new(Pool::new(&[mock(port, "secret", 2, 1)])));
    wait_for("its answers", || answers.lock().unwrap().len() == 3);
    let answers = answers.lock().unwrap();
    assert!(matches!(answers[0], Got::Refused(_)), "an image: {:?}", answers[0]);
    assert!(matches!(answers[1], Got::Body(_)), "{:?}", answers[1]);
    assert!(matches!(answers[2], Got::Refused(_)), "one call uses one of the release's bodies: {:?}", answers[2]);
    assert_eq!(state.bodies_sent.load(Ordering::SeqCst), 1);
}

fn atlas_post(message_id: &str) -> common::Post {
    common::Post {
        message_id: message_id.into(),
        ..post(1, r#""a.bin" yEnc (1/2)"#, 10, yenc_lined("a.bin", b"hello"))
    }
}
