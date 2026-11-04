//! A pass standing in for a group's own server while that one is in trouble (`PassSettings::fallback`): it reads
//! only from the server it was given, so the group's home stays put, and the first one starts at the newest article.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use atlas::indexer::{PassContext, PassSettings, RunStates, run_pass, shared_db};
use atlas::nntp::Pool;
use common::{GROUP, Server, mock, post, spawn_server};

fn posts(from: u64, to: u64) -> Vec<common::Post> {
    (from..=to).map(|n| post(n, &format!(r#""rel{n}.bin" yEnc (1/1)"#), 10, vec![])).collect()
}

fn context(pool: &Arc<Pool>) -> PassContext {
    PassContext {
        pool: pool.clone(),
        states: RunStates::default(),
        stop: Arc::new(AtomicBool::new(false)),
        verbose: false,
    }
}

fn live(fallback: bool, from_top: bool) -> PassSettings {
    PassSettings { mode: "live".into(), batch_size: 1000, request_size: 50, fallback, from_top, ..Default::default() }
}

/// The first stand-in pass starts the server's cursor at the newest article, even when an earlier stand-in left one
/// far behind; the ones after it read what is new.
#[test]
fn a_stand_in_starts_at_the_newest_article() {
    let server = Server::new(posts(1, 100));
    let port = spawn_server(server.clone());
    let pool = Arc::new(Pool::new(&[mock(port, "secret", 2, 1)]));
    let dir = tempfile::tempdir().unwrap();
    let main = dir.path().join("atlas.db");
    atlas::db::create_db_at(&main).unwrap();
    let conn = atlas::db::open_at(&main).unwrap();
    let ctx = context(&pool);

    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    rt.block_on(async {
        pool.connect().await.unwrap();
        let db = shared_db(atlas::db::open_at(&main).unwrap());
        // a cursor an earlier stand-in left far behind
        run_pass(&ctx, &live(false, false), &db, GROUP, 0, &mut |_| {}).await.unwrap();
        let key = atlas::db::group_progress(&conn).unwrap().remove(0).key;
        atlas::db::update_live_cursor(&conn, &key, 10).unwrap();

        let first = run_pass(&ctx, &live(true, true), &db, GROUP, 0, &mut |_| {}).await.unwrap();
        assert_eq!(first.articles, 0, "1-100 are left to the group's own server");
        assert_eq!(atlas::db::group_progress(&conn).unwrap()[0].live_cursor, 100);

        server.posts.lock().unwrap().extend(posts(101, 110));
        let next = run_pass(&ctx, &live(true, false), &db, GROUP, 0, &mut |_| {}).await.unwrap();
        assert_eq!(next.articles, 10, "what was posted since");
    });
}

/// A stand-in that doesnt carry the group fails its pass and leaves the group's home where it is: a normal pass
/// would have moved it.
#[test]
fn a_stand_in_doesnt_move_the_groups_home() {
    let servers = [Server::new(posts(1, 100)), Server::new(posts(1, 100))];
    let ports: Vec<u16> = servers.iter().map(|s| spawn_server(s.clone())).collect();
    let pool = Arc::new(Pool::new(&[mock(ports[0], "secret", 2, 1), mock(ports[1], "secret", 2, 1)]));
    let home = pool.pick_server(GROUP);
    let other = 1 - home;
    servers[other].dropped.store(true, Ordering::SeqCst);
    let dir = tempfile::tempdir().unwrap();
    let main = dir.path().join("atlas.db");
    atlas::db::create_db_at(&main).unwrap();
    let ctx = context(&pool);

    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    rt.block_on(async {
        pool.connect().await.unwrap();
        let db = shared_db(atlas::db::open_at(&main).unwrap());
        assert!(run_pass(&ctx, &live(true, true), &db, GROUP, other, &mut |_| {}).await.is_err());
        assert_eq!(pool.pick_server(GROUP), home, "the group's home stays");
        assert!(atlas::db::group_progress(&atlas::db::open_at(&main).unwrap()).unwrap().is_empty(), "no cursor");
    });
}

/// A normal pass on a server without the group moves it, and says where it ran: a failure there counts against the
/// server it moved to, not the one it was handed.
#[test]
fn a_pass_says_which_server_it_ran_on() {
    let servers = [Server::new(posts(1, 100)), Server::new(posts(1, 100))];
    let ports: Vec<u16> = servers.iter().map(|s| spawn_server(s.clone())).collect();
    let pool = Arc::new(Pool::new(&[mock(ports[0], "secret", 2, 1), mock(ports[1], "secret", 2, 1)]));
    let other = 1 - pool.pick_server(GROUP);
    servers[1 - other].dropped.store(true, Ordering::SeqCst);
    let dir = tempfile::tempdir().unwrap();
    let main = dir.path().join("atlas.db");
    atlas::db::create_db_at(&main).unwrap();
    let ctx = context(&pool);

    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    rt.block_on(async {
        pool.connect().await.unwrap();
        let db = shared_db(atlas::db::open_at(&main).unwrap());
        let mut ran_on = usize::MAX;
        let home = 1 - other;
        atlas::indexer::run_pass_on(&ctx, &live(false, false), &db, GROUP, home, &mut |_| {}, &mut ran_on)
            .await
            .unwrap();
        assert_eq!(ran_on, other, "moved to the server that carries it");
    });
}
