//! Real provider cancellation: discard the stuck connection, retain completed
//! slices, and stop the normal PostgreSQL Atlas binary without crossing its gap.

#![cfg(unix)]

use atlas as atlas_api;
#[path = "common/native_background.rs"]
mod native_background;
#[path = "common/nntp.rs"]
mod nntp;

use atlas::nntp::{BlockingPool, Pool};
use nntp::{GROUP, Server, mock, post, spawn_server};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const STALL: Duration = Duration::from_secs(20);
const PROMPT: Duration = Duration::from_secs(3);

fn stalling_server(stalls: usize) -> Arc<Server> {
    let posts =
        (1..=200).map(|n| post(n, &format!(r#""rel{}.part{}.rar" yEnc (1/1)"#, n / 20, n % 20 + 1), 10, vec![]));
    let mut s = Server::new(posts.collect());
    let fixture = Arc::get_mut(&mut s).unwrap();
    fixture.compress = true;
    fixture.stall = STALL;
    fixture.stalls_left.store(stalls, Ordering::SeqCst);
    s
}

fn stop_when_stalled(server: Arc<Server>, stop: Arc<AtomicBool>) -> std::thread::JoinHandle<Instant> {
    std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(12);
        while server.stalled.load(Ordering::SeqCst) == 0 {
            assert!(Instant::now() < deadline, "scripted NNTP request never stalled");
            std::thread::sleep(Duration::from_millis(10));
        }
        // Preserve the pool control's allowance for its other slices to finish.
        std::thread::sleep(Duration::from_millis(1500));
        stop.store(true, Ordering::SeqCst);
        Instant::now()
    })
}

#[test]
#[ignore = "requires explicit isolated ATLAS_PG_TEST_URL and actual Atlas BIN"]
fn stop_does_not_wait_for_a_stuck_request() {
    // Mandatory admission precedes even this test's provider-only control.
    let mut harness = native_background::Harness::new();
    let state = stalling_server(1);
    let port = spawn_server(state.clone());
    let pool = BlockingPool::from_pool(Pool::new(&[mock(port, "secret", 2, 1)]).with_timeout(Duration::from_secs(5)));
    pool.connect().unwrap();
    let slices: Vec<(u64, u64)> = (0..10).map(|i| (i * 20 + 1, i * 20 + 20)).collect();
    let stop = Arc::new(AtomicBool::new(false));
    let stopper = stop_when_stalled(state.clone(), stop.clone());
    let got = pool.block_on(async {
        let mut rx =
            pool.pool.stream_headers(GROUP, 0, slices.clone(), atlas::nntp::STREAM_SLICES, false, stop.clone());
        let mut got = 0;
        while let Some(slice) = rx.recv().await {
            got += slice.result.is_ok() as usize;
        }
        got
    });
    let waited = stopper.join().unwrap().elapsed();
    assert!(waited < PROMPT, "stream took {waited:?} to stop with a request stuck");
    assert!(got < slices.len(), "the stuck slice cant have come back");
    let started = Instant::now();
    let rows = pool.fetch_headers(GROUP, 1, 200).unwrap();
    assert_eq!(rows.len(), 200);
    assert!(started.elapsed() < PROMPT, "took {:?}, the stuck connection was reused", started.elapsed());
    pool.disconnect();
    drop(pool);

    let state = stalling_server(1);
    let port = spawn_server(state.clone());
    harness.configure("backfill", port, 2);
    let mut runner = harness.start();
    runner.wait_for("stalled top slice and other committed releases", |run| {
        state.stalled.load(Ordering::SeqCst) > 0 && run.saved() > 0 && run.cursors().1 == 200
    });
    assert_eq!(runner.cursors().1, 200, "cursor must not cross the stuck top slice before TERM");
    let waited = runner.stop_within(PROMPT);
    assert!(waited < PROMPT, "indexer took {waited:?} to stop with a request stuck");
    assert_eq!(runner.cursors().1, 200, "the backfill cursor moved past the stuck slice");
    assert!(runner.saved() > 0, "the slices that came back were saved");
    drop(runner);
    harness.verify_nxv(1);
}
