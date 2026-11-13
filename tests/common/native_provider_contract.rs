//! Retained provider behavior through the original native owner and live NXV.
//!
//! Each selected test runs one child: the production NXV handle is single-start
//! per process. PostgreSQL admission is mandatory, and the child never receives
//! fixture credentials in its arguments or prints captured output.
#![allow(dead_code)]

#[path = "native.rs"]
mod native;
#[path = "nntp.rs"]
mod nntp;

use nntp::{GROUP, Server, mock, post, spawn_server};
use std::sync::Arc;

fn posts(n: u64) -> Vec<nntp::Post> {
    (1..=n).map(|i| post(i, &format!(r#""rel{}.part{}.rar" yEnc (1/1)"#, i / 20, i % 20 + 1), 10, vec![])).collect()
}

#[test]
#[ignore = "requires explicit isolated ATLAS_PG_TEST_URL and real durable NXV"]
fn a_refused_slice_is_read_again_later() {
    native::run_child("refused");
}

#[test]
#[ignore = "requires explicit isolated ATLAS_PG_TEST_URL and real durable NXV"]
fn an_empty_range_answered_420_is_just_empty() {
    native::run_child("empty-420");
}

#[test]
#[ignore = "internal isolated child; select the retained parent tests"]
fn fixture_child() {
    let case = std::env::var("ATLAS_NATIVE_PROVIDER_CASE").expect("select the retained parent test");
    match case.as_str() {
        "refused" => refused_slice(),
        "empty-420" => empty_range(),
        _ => panic!("unknown native provider fixture case"),
    }
    native::mark_finished(&case);
}

/// A 5xx fails its pass, then the same original native owner retries the slice.
fn refused_slice() {
    let state = Server::new(posts(400));
    *state.refuse_xover.lock().unwrap() = Some((120, 2));
    let port = spawn_server(state.clone());
    let mut harness = native::Harness::open(vec![mock(port, "secret", 2, 1)]);
    let mut indexer = harness.indexer("backfill");
    indexer.request_size = 50;
    let mut failed = 0;
    for _ in 0..20 {
        if indexer.index_group(GROUP).is_err() {
            failed += 1;
        }
        if indexer.is_idle(GROUP) {
            break;
        }
    }
    assert_eq!(failed, 2, "each refusal fails its pass");
    assert!(state.refuse_xover.lock().unwrap().is_none(), "both refusals were used");
    assert!(indexer.is_idle(GROUP), "the retained pass loop must reach idle");
    assert_eq!(harness.articles(), 400, "the refused slice was read once the server answered");
    assert_eq!(harness.backfill_cursor(GROUP), 0, "the backfill went all the way down");
    harness.finish(indexer, 400);
}

/// An empty range's 420 is accepted as empty, without dropping the real rows.
fn empty_range() {
    let posts = posts(400).into_iter().filter(|p| !(101..=300).contains(&p.number)).collect();
    let mut state = Server::new(posts);
    Arc::get_mut(&mut state).unwrap().empty_is_420 = true;
    let port = spawn_server(state.clone());
    let mut harness = native::Harness::open(vec![mock(port, "secret", 2, 1)]);
    let mut indexer = harness.indexer("backfill");
    indexer.request_size = 50;
    for _ in 0..20 {
        indexer.index_group(GROUP).unwrap();
        if indexer.is_idle(GROUP) {
            break;
        }
    }
    assert!(indexer.is_idle(GROUP), "indexer never went idle");
    assert_eq!(harness.articles(), 200);
    assert_eq!(harness.backfill_cursor(GROUP), 0, "the backfill went all the way down");
    harness.finish(indexer, 200);
}
