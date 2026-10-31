//! Name lookups in the save path, and turning them off.
//!
//! One test: it points ATLAS_HOME at a temp dir, and the switch is process wide.

mod common;

use std::sync::Arc;
use std::sync::atomic::Ordering;

use atlas::indexer::Indexer;
use atlas::nntp::BlockingPool;
use atlas::{archive, db, store};
use common::*;

#[test]
fn the_save_path_stops_fetching_when_told() {
    let home = tempfile::tempdir().unwrap();
    // SAFETY: only test in this binary, set before any thread is spawned
    unsafe {
        std::env::set_var("ATLAS_HOME", home.path());
        std::env::set_var("ATLAS_NO_KEYRING", "1");
    }
    let posts = vec![
        post(
            1,
            r#""Has.A.Par2.par2" yEnc (1/1)"#,
            200,
            yenc_body("x.par2", &par2_file_desc("Hidden.Show.S01E01.mkv", 9)),
        ),
        post(2, r#""Has.An.Nfo.nfo" yEnc (1/1)"#, 50, vec![b"Release Name: Cool Movie 2024".to_vec()]),
    ];
    let mut state = Server::new(posts);
    Arc::get_mut(&mut state).unwrap().any_group = true;
    let port = spawn_server(state.clone());
    db::create_db().unwrap();

    let named = |group: &str| -> (i64, i64) {
        let shard = db::open_shard(&store::shard_path(&home.path().join("atlas.db"), store::shard_of(group))).unwrap();
        shard
            .query_row("select count(*), count(display_name) from releases", [], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
    };

    let client = BlockingPool::new(&[mock(port, "secret", 4, 1)]);
    client.connect().unwrap();
    let mut indexer = Indexer::new(client, "backfill", db::open().unwrap());
    index_until_idle(&mut indexer);
    let sent = state.bodies_sent.load(Ordering::SeqCst);
    assert!(sent > 0);
    assert_eq!(named(GROUP), (2, 2));

    // the same posts in a group on another shard, with lookups off
    let other =
        (0..).map(|i| format!("alt.binaries.other{i}")).find(|g| store::shard_of(g) != store::shard_of(GROUP)).unwrap();
    archive::set_inline_lookups(false);
    for _ in 0..20 {
        indexer.index_group(&other).unwrap();
        if indexer.is_idle(&other) {
            break;
        }
    }
    assert_eq!(state.bodies_sent.load(Ordering::SeqCst), sent, "no body once the stage owns names");
    assert_eq!(named(&other), (2, 0));
    archive::set_inline_lookups(true);
}
