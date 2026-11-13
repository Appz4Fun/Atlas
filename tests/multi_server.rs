//! Pure NNTP controls retained alongside the privately registered native multi-server catalog tests.
use atlas as atlas_api;
#[path = "common/nntp.rs"]
mod nntp;

use atlas::nntp::BlockingPool;
use nntp::*;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

#[test]
fn name_lookups_spread_over_servers() {
    let posts: Vec<Post> = (1..=40)
        .map(|n| {
            post(n, &format!(r#""thing{n}.nfo" yEnc (1/1)"#), 10, vec![format!("Release Name: Thing {n}").into_bytes()])
        })
        .collect();
    let servers: Vec<Arc<Server>> = (0..2)
        .map(|_| {
            let mut s = Server::new(posts.clone());
            Arc::get_mut(&mut s).unwrap().body_delay = Duration::from_millis(20);
            s
        })
        .collect();
    let (p1, p2) = (spawn_server(servers[0].clone()), spawn_server(servers[1].clone()));

    let pool = BlockingPool::new(&[mock(p1, "secret", 2, 1), mock(p2, "secret", 2, 2)]);
    pool.connect().unwrap();

    let extract: atlas::nntp::Extract = atlas::nfo::display_name;
    let jobs = posts.iter().map(|p| vec![(p.message_id.clone(), extract)]).collect();
    let names = pool.first_names(jobs).unwrap();
    assert!(names.iter().all(Option::is_some), "{names:?}");

    let sent: Vec<usize> = servers.iter().map(|s| s.bodies_sent.load(Ordering::SeqCst)).collect();
    assert_eq!(sent.iter().sum::<usize>(), 40);
    assert!(sent.iter().all(|&n| n >= 10), "the lookups should be shared, got {sent:?}");
}

#[test]
fn name_lookups_fall_back_on_a_missing_article() {
    let posts: Vec<Post> = (1..=10)
        .map(|n| {
            post(n, &format!(r#""thing{n}.nfo" yEnc (1/1)"#), 10, vec![format!("Release Name: Thing {n}").into_bytes()])
        })
        .collect();
    let mut missing = Server::new(posts.clone());
    Arc::get_mut(&mut missing).unwrap().bodies = false;
    let has = Server::new(posts.clone());
    let (p1, p2) = (spawn_server(missing.clone()), spawn_server(has.clone()));

    let pool = BlockingPool::new(&[mock(p1, "secret", 2, 1), mock(p2, "secret", 2, 2)]);
    pool.connect().unwrap();

    let extract: atlas::nntp::Extract = atlas::nfo::display_name;
    let jobs = posts.iter().map(|p| vec![(p.message_id.clone(), extract)]).collect();
    assert!(pool.first_names(jobs).unwrap().iter().all(Option::is_some));
    assert_eq!(has.bodies_sent.load(Ordering::SeqCst), 10);
}

#[test]
fn a_group_never_falls_over_to_a_server_kept_out_of_indexing() {
    let posts: Vec<Post> = (1..=10).map(|n| post(n, &format!(r#""thing{n}.rar" yEnc (1/1)"#), 10, vec![])).collect();
    let indexing = Server::new(posts.clone());
    indexing.dropped.store(true, Ordering::SeqCst);
    let block = Server::new(posts);
    let (p1, p2) = (spawn_server(indexing), spawn_server(block));

    let mut kept_out = mock(p2, "secret", 2, 2);
    kept_out.index = Some(false);
    let pool = BlockingPool::new(&[mock(p1, "secret", 2, 1), kept_out]);
    pool.connect().unwrap();

    let err = pool.select_group(GROUP).expect_err("only the block account carries it");
    assert_eq!(err.code(), Some(411), "{err}");
    assert_eq!(pool.active_index(), 0, "still on the indexing server");
}
