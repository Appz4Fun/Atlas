//! A real native pass commits each completed slice before a later slice stalls.
//! Restart the normal Atlas binary on the same private PostgreSQL catalog/home.

#![cfg(unix)]

use atlas as atlas_api;
#[path = "common/native_background.rs"]
mod native_background;
#[path = "common/nntp.rs"]
mod nntp;

use nntp::{Server, post, spawn_server};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

const STALL: Duration = Duration::from_secs(20);
const PROMPT: Duration = Duration::from_secs(3);

fn server(stall_at: u64) -> Arc<Server> {
    let posts =
        (1..=200).map(|n| post(n, &format!(r#""rel{}.part{}.rar" yEnc (1/1)"#, n / 20, n % 20 + 1), 10, vec![]));
    let mut s = Server::new(posts.collect());
    let fixture = Arc::get_mut(&mut s).unwrap();
    fixture.stall = STALL;
    *fixture.stall_at.lock().unwrap() = Some(stall_at);
    s
}

fn run_until_stuck(
    harness: &mut native_background::Harness,
    mode: &str,
    stall_at: u64,
    expected: (i64, i64),
    expected_rows: u64,
) -> (i64, i64) {
    let state = server(stall_at);
    let port = spawn_server(state.clone());
    harness.configure(mode, port, 1);
    let mut runner = harness.start();
    runner.wait_for("a real stalled request and preceding committed slices", |run| {
        state.stalled.load(Ordering::SeqCst) > 0 && run.cursors() == expected
    });
    assert_eq!(runner.cursors(), expected, "preceding slices commit before TERM");
    assert!(runner.stop_within(PROMPT) < PROMPT, "stalled read must drain promptly");
    drop(runner);
    let cursors = harness.cursors();
    harness.verify_nxv(expected_rows);
    cursors
}

#[test]
#[ignore = "requires explicit isolated ATLAS_PG_TEST_URL and actual Atlas BIN"]
fn a_pass_stopped_partway_keeps_the_cursor_past_what_it_saved() {
    let mut harness = native_background::Harness::new();
    harness.seed_cursors(0, 200);
    assert_eq!(run_until_stuck(&mut harness, "live", 41, (40, 200), 40), (40, 200));
    assert_eq!(run_until_stuck(&mut harness, "backfill", 141, (40, 160), 80), (40, 160));
}
