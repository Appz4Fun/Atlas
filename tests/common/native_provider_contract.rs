//! Retained provider behavior through the original native owner and live NXV.
//!
//! Each selected test runs one child: the production NXV handle is single-start
//! per process. PostgreSQL admission is mandatory, and the child never receives
//! fixture credentials in its arguments or prints captured output.
use crate as atlas_api;

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
        "ranked" => ranked_server(),
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

#[test]
#[ignore = "requires explicit isolated ATLAS_PG_TEST_URL and real durable NXV"]
fn a_server_without_the_group_is_banned_from_it_and_the_next_reads_it() {
    native::run_child("ranked");
}

fn ranked_server() {
    use crate::config::IndexingConfig;
    use crate::indexer::{PassSettings, run_pass_on};
    use crate::nntp::{BlockingPool, Pool};
    use std::sync::atomic::Ordering;
    let posts = |from, to| (from..=to).map(|n| post(n, &format!(r#""rel{n}.bin" yEnc (1/1)"#), 10, vec![])).collect();
    let servers = [Server::new(posts(1, 100)), Server::new(posts(1, 100))];
    let ports: Vec<u16> = servers.iter().map(|s| spawn_server(s.clone())).collect();
    let cfg = IndexingConfig { prefer: vec![format!("127.0.0.1:{}", ports[0])], ..Default::default() };
    let configs = vec![mock(ports[0], "secret", 2, 1), mock(ports[1], "secret", 2, 1)];
    let pool = BlockingPool::from_pool(Pool::new(&configs).with_ranking(&cfg, None));
    assert_eq!(pool.pool.pick_server(GROUP), 0, "the server named first");
    servers[0].dropped.store(true, Ordering::SeqCst);
    let mut harness = native::Harness::open(configs);
    let ctx = harness.context(pool.pool.clone());
    let settings = PassSettings { mode: "live".into(), batch_size: 1000, request_size: 50, ..Default::default() };
    let db = harness.db();
    pool.block_on(async {
        pool.pool.connect().await.unwrap();
        let mut ran_on = usize::MAX;
        run_pass_on(&ctx, &settings, &db, GROUP, 0, &mut |_| {}, &mut ran_on).await.unwrap();
        assert_eq!(ran_on, 1, "read from the next server");
        assert!(pool.pool.banned(GROUP, 0), "the first is banned from the group for a day");
        assert_eq!(pool.pool.pick_server(GROUP), 1);
        assert_eq!(pool.pool.homes_of(GROUP), vec![1]);
        // The original first live pass starts at the newest article. A genuine
        // later post additionally proves the chosen provider's native commit.
        servers[1].posts.lock().unwrap().extend(posts(101, 101));
        let progress = run_pass_on(&ctx, &settings, &db, GROUP, 1, &mut |_| {}, &mut ran_on).await.unwrap();
        assert_eq!(progress.articles, 1, "new post from the selected provider is read");
    });
    assert_eq!(harness.articles(), 1, "chosen provider's actual native commit");
    assert!(servers[1].xovers.load(Ordering::SeqCst) > 0, "chosen provider supplied real overview input");
    let indexer = harness.indexer_on(pool, "live");
    harness.finish(indexer, 1);
}
