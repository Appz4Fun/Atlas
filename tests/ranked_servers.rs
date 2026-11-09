//! Groups by rank (`[indexing] prefer`): a server that says it doesnt carry a group is banned from it for a day and
//! the next server down the list reads it.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use atlas::config::IndexingConfig;
use atlas::indexer::{PassContext, PassSettings, RunStates, run_pass_on, shared_db};
use atlas::nntp::Pool;
use common::{GROUP, Server, mock, post, spawn_server};

fn posts(from: u64, to: u64) -> Vec<common::Post> {
    (from..=to).map(|n| post(n, &format!(r#""rel{n}.bin" yEnc (1/1)"#), 10, vec![])).collect()
}

#[test]
fn a_server_without_the_group_is_banned_from_it_and_the_next_reads_it() {
    let servers = [Server::new(posts(1, 100)), Server::new(posts(1, 100))];
    let ports: Vec<u16> = servers.iter().map(|s| spawn_server(s.clone())).collect();
    let cfg = IndexingConfig { prefer: vec![format!("127.0.0.1:{}", ports[0])], ..Default::default() };
    let pool =
        Arc::new(Pool::new(&[mock(ports[0], "secret", 2, 1), mock(ports[1], "secret", 2, 1)]).with_ranking(&cfg, None));
    assert_eq!(pool.pick_server(GROUP), 0, "the server named first");
    servers[0].dropped.store(true, Ordering::SeqCst);
    let dir = tempfile::tempdir().unwrap();
    let main = dir.path().join("atlas.db");
    atlas::db::create_db_at(&main).unwrap();
    let ctx = PassContext {
        pool: pool.clone(),
        states: RunStates::default(),
        stop: Arc::new(AtomicBool::new(false)),
        verbose: false,
    };
    let settings = PassSettings { mode: "live".into(), batch_size: 1000, request_size: 50, ..Default::default() };

    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    rt.block_on(async {
        pool.connect().await.unwrap();
        let db = shared_db(atlas::db::open_at(&main).unwrap());
        let mut ran_on = usize::MAX;
        run_pass_on(&ctx, &settings, &db, GROUP, 0, &mut |_| {}, &mut ran_on).await.unwrap();
        assert_eq!(ran_on, 1, "read from the next server");
        assert!(pool.banned(GROUP, 0), "the first is banned from the group for a day");
        assert_eq!(pool.pick_server(GROUP), 1);
        assert_eq!(pool.homes_of(GROUP), vec![1]);
    });
}
