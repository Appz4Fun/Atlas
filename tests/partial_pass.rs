//! A backfill pass with a slice that fails part way: the slices that made it
//! are saved anyway, the cursor moves past the run of them under it, and the
//! next pass picks up at the failed slice instead of fetching the whole
//! batch again.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use atlas::indexer::{PassContext, PassSettings, RunStates, run_pass, shared_db};
use atlas::nntp::Pool;
use common::{Server, mock, post, spawn_server};

const POSTS: u64 = 100;
const REQUEST: u64 = 10;
/// the slice that fails: backfill goes newest first, soo 100-91 .. 70-61
/// are fetched before 60-51 on the one connection
const FAILS_AT: u64 = 51;

fn releases(main: &std::path::Path) -> i64 {
    let conn = atlas::db::open_with_shards(main).unwrap();
    conn.query_row("select count(*) from releases", [], |r| r.get(0)).unwrap()
}

#[test]
fn a_failed_slice_keeps_the_slices_that_made_it() {
    // a release a slice, soo releases count the slices saved
    let posts = (1..=POSTS)
        .map(|n| {
            post(n, &format!(r#""rel{}.part{}.rar" yEnc (1/1)"#, (n - 1) / REQUEST, (n - 1) % REQUEST), 10, vec![])
        })
        .collect();
    let mut server = Server::new(posts);
    let s = Arc::get_mut(&mut server).unwrap();
    s.any_group = true;
    *s.drop_xover_at.lock().unwrap() = Some(FAILS_AT);
    let port = spawn_server(server.clone());
    let pool = Arc::new(Pool::new(&[mock(port, "secret", 1, 1)]));

    let dir = tempfile::tempdir().unwrap();
    let main = dir.path().join("atlas.db");
    atlas::db::create_db_at(&main).unwrap();
    let db = shared_db(atlas::db::open_at(&main).unwrap());
    let ctx = PassContext {
        pool: pool.clone(),
        states: RunStates::default(),
        stop: Arc::new(AtomicBool::new(false)),
        verbose: false,
    };
    let settings = PassSettings {
        mode: "backfill".into(),
        batch_size: POSTS as i64,
        request_size: REQUEST,
        split_min_backlog: i64::MAX,
        ..Default::default()
    };
    let group = "alt.binaries.partial";

    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    rt.block_on(async {
        pool.connect().await.unwrap();

        let first = run_pass(&ctx, &settings, &db, group, 0, &mut |_| {}).await;
        assert!(first.is_err(), "the dropped XOVER fails the pass");
        let before = (POSTS - (FAILS_AT + REQUEST - 1)) / REQUEST;
        assert_eq!(releases(&main), before as i64, "the slices fetched before the failure are saved");

        for _ in 0..20 {
            run_pass(&ctx, &settings, &db, group, 0, &mut |_| {}).await.unwrap();
            if ctx.states.is_idle(group) {
                break;
            }
        }
        assert!(ctx.states.is_idle(group), "the group never went idle");
    });

    assert_eq!(releases(&main), (POSTS / REQUEST) as i64, "every slice saved");
    // the failed one twice, the ones before it once: the cursor moved past them
    assert_eq!(server.xovers.load(Ordering::SeqCst), (POSTS / REQUEST + 1) as usize, "slices fetched again");
}
