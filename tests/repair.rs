//! A post whose one indexing server's listing left parts out gets them from another server's listing.

mod common;

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use atlas::indexer::{Indexer, PassContext, PassSettings, RunStates, reread_window, shared_db};
use atlas::nntp::{BlockingPool, Pool};
use atlas::{db, repair};
use common::*;

const PART: u64 = 200 << 20;

fn rfc2822(t: i64) -> String {
    chrono::DateTime::from_timestamp(t, 0).unwrap().to_rfc2822()
}

/// The film's 10 parts, a second apart from `t`, numbered from `first`; `skip` left out.
fn film(first: u64, t: i64, skip: Option<u64>) -> Vec<Post> {
    (1..=10u64)
        .filter(|k| Some(*k) != skip)
        .map(|k| Post {
            message_id: format!("<part{k}@nyuu>"),
            bytes: PART,
            ..post_at(first + k, &format!(r#"[1/1] - "Some.Film.2160p.DV.mkv" yEnc ({k}/10)"#), &rfc2822(t + k as i64))
        })
        .collect()
}

#[test]
fn a_part_one_listing_lacks_comes_from_the_other_server() {
    let dir = tempfile::tempdir().unwrap();
    let main = dir.path().join("atlas.db");
    db::create_db_at(&main).unwrap();
    let posted = chrono::Utc::now().timestamp() - 3 * 3600;
    // around the post, other articles, soo the window is found by date
    let around = |first: u64, t: i64| -> Vec<Post> {
        (0..20)
            .map(|n| post_at(first + n, &format!(r#""other{first}-{n}.bin" yEnc (1/1)"#), &rfc2822(t + n as i64)))
            .collect()
    };
    let mut on_a = around(100, posted - 4000);
    on_a.extend(film(200, posted, Some(5)));
    on_a.extend(around(300, posted + 4000));
    let mut on_b = around(1100, posted - 4000);
    on_b.extend(film(1200, posted, None));
    on_b.extend(around(1300, posted + 4000));
    let (a, b) = (spawn_server(Server::new(on_a)), spawn_server(Server::new(on_b)));

    // indexed from server a alone
    let mut indexer =
        Indexer::new(BlockingPool::new(&[mock(a, "secret", 2, 1)]), "backfill", db::open_at(&main).unwrap());
    for _ in 0..10 {
        indexer.index_group_with(GROUP, &mut |_| {}).unwrap();
        if indexer.is_idle(GROUP) {
            break;
        }
    }
    drop(indexer);
    let conn = db::open_with_shards(&main).unwrap();
    let parts = || -> i64 {
        conn.query_row("select parts from releases where name like 'Some.Film%'", [], |r| r.get(0)).unwrap()
    };
    assert_eq!(parts(), 9);

    // the round finds it: big, nearly whole, settled
    conn.execute_batch(
        "create table if not exists repair_tried (release_id INTEGER PRIMARY KEY, at INTEGER NOT NULL, permille INTEGER)",
    )
    .unwrap();
    let found = repair::find(&conn, chrono::Utc::now().timestamp()).unwrap();
    assert_eq!(found.windows.len(), 1, "{found:?}");
    let w = &found.windows[0];
    assert_eq!(w.group, GROUP);
    assert!(w.from <= posted + 1 && w.to >= posted + 10);

    // and server b's listing of that window brings the part
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let pool = Arc::new(Pool::new(&[mock(a, "secret", 2, 1), mock(b, "secret", 2, 2)]));
    let ctx =
        PassContext { pool, states: RunStates::default(), stop: Arc::new(AtomicBool::new(false)), verbose: false };
    let db_handle = shared_db(db::open_at(&main).unwrap());
    let read = runtime
        .block_on(reread_window(&ctx, &PassSettings::default(), &db_handle, GROUP, 1, (w.from, w.to), 1_000_000))
        .unwrap();
    assert!(read.articles >= 10 && read.articles < 30, "only the window: {}", read.articles);
    drop(db_handle);
    assert_eq!(parts(), 10);
}
