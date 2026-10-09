//! A pass stopped while its name lookups wait on a slow body: its cursor
//! doesnt move past them, soo the next pass fetches the slice again and the
//! release still gets its name.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use atlas::indexer::{PassContext, PassSettings, RunStates, run_pass, shared_db};
use atlas::nntp::{Pool, unless_stopped};
use common::{Server, mock, par2_file_desc, post, spawn_server, yenc_body};

#[test]
fn a_stopped_pass_keeps_its_names_for_the_next_one() {
    let par2 = par2_file_desc("Real.Name.mkv", 9);
    let posts = vec![
        post(1, r#""rel.par2" yEnc (1/1)"#, 10, yenc_body("rel.par2", &par2)),
        post(2, r#""rel.part1.rar" yEnc (1/1)"#, 10, vec![]),
    ];
    let mut server = Server::new(posts);
    let s = Arc::get_mut(&mut server).unwrap();
    s.any_group = true;
    s.body_delay = Duration::from_secs(2);
    let port = spawn_server(server.clone());
    let pool = Arc::new(Pool::new(&[mock(port, "secret", 2, 1)]));

    let dir = tempfile::tempdir().unwrap();
    let main = dir.path().join("atlas.db");
    atlas::db::create_db_at(&main).unwrap();
    let db = shared_db(atlas::db::open_at(&main).unwrap());
    let stop = Arc::new(AtomicBool::new(false));
    let ctx = PassContext { pool: pool.clone(), states: RunStates::default(), stop: stop.clone(), verbose: false };
    let settings =
        PassSettings { mode: "backfill".into(), request_size: 10, split_min_backlog: i64::MAX, ..Default::default() };
    let group = "alt.binaries.names";
    let named = || -> i64 {
        let conn = atlas::db::open_with_shards(&main).unwrap();
        conn.query_row("select count(display_name) from releases", [], |r| r.get(0)).unwrap()
    };

    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    rt.block_on(async {
        pool.connect().await.unwrap();

        // stopped while the body is on its way, the way the background indexer stops
        let stopper = stop.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(500)).await;
            stopper.store(true, Ordering::Relaxed);
        });
        let mut progress = |_: &atlas::indexer::Progress| {};
        let pass = run_pass(&ctx, &settings, &db, group, 0, &mut progress);
        assert!(unless_stopped(&stop, pass).await.is_none(), "the pass waits for its name");
        assert_eq!(named(), 0);

        stop.store(false, Ordering::Relaxed);
        for _ in 0..5 {
            run_pass(&ctx, &settings, &db, group, 0, &mut |_| {}).await.unwrap();
            if ctx.states.is_idle(group) {
                break;
            }
        }
    });

    assert_eq!(named(), 1, "the next pass named the release");
}
