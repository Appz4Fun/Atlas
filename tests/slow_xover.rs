//! A server that takes longer than the read timeout to answer a wide XOVER: the range is asked for in halves.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use atlas::nntp::{BlockingPool, Pool};
use common::{GROUP, Server, mock, post, spawn_server};

/// On prod news.newshosting.com took 11 to 105 s to start answering each 100,000 number XOVER of
/// alt.binaries.mom, past the 30 s read timeout, and the same day chunk timed out on every try.
#[test]
fn a_range_too_wide_to_answer_in_time_is_asked_for_in_halves() {
    let posts: Vec<_> = (1..=2000).map(|n| post(n, &format!(r#""thing{n}.bin" yEnc (1/1)"#), 10, vec![])).collect();
    let mut state = Server::new(posts);
    Arc::get_mut(&mut state).unwrap().slow_over = Some((600, Duration::from_secs(3)));
    let port = spawn_server(state.clone());
    let pool = BlockingPool::from_pool(Pool::new(&[mock(port, "secret", 2, 1)]).with_timeout(Duration::from_secs(1)));
    pool.connect().unwrap();

    let stop = Arc::new(AtomicBool::new(false));
    let (rows, errors) = pool.block_on(async {
        let mut rx = pool.pool.stream_headers(GROUP, 0, vec![(1, 2000)], stop.clone());
        let (mut rows, mut errors) = (0, Vec::new());
        while let Some(slice) = rx.recv().await {
            match slice.result {
                Ok(h) => rows += h.len(),
                Err(e) => errors.push(e.to_string()),
            }
        }
        (rows, errors)
    });
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(rows, 2000);
    // 2000 and 1000 time out, the 500s answer: 1 + 2 + 4
    assert_eq!(state.xovers.load(Ordering::SeqCst), 7);
}

/// A server that answers no range in time still fails the slice, after halving down to the smallest one.
#[test]
fn a_server_that_never_answers_still_fails() {
    let posts: Vec<_> = (1..=2000).map(|n| post(n, &format!(r#""thing{n}.bin" yEnc (1/1)"#), 10, vec![])).collect();
    let mut state = Server::new(posts);
    Arc::get_mut(&mut state).unwrap().slow_over = Some((0, Duration::from_secs(3)));
    let port = spawn_server(state.clone());
    let pool = BlockingPool::from_pool(Pool::new(&[mock(port, "secret", 2, 1)]).with_timeout(Duration::from_secs(1)));
    pool.connect().unwrap();

    let stop = Arc::new(AtomicBool::new(false));
    let failed = pool.block_on(async {
        let mut rx = pool.pool.stream_headers(GROUP, 0, vec![(1, 2000)], stop.clone());
        let mut failed = 0;
        while let Some(slice) = rx.recv().await {
            failed += slice.result.is_err() as usize;
        }
        failed
    });
    assert_eq!(failed, 1);
    // 2000, 1000, 500 and 250 numbers, the left half each time, then it gives up
    assert_eq!(state.xovers.load(Ordering::SeqCst), 4);
}
