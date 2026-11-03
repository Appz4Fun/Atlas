//! One post stays one release over slices and servers, whatever its files are
//! called, and other posters' posts stay apart.
//!
//! One test, and the environment is set before any thread starts.

mod common;

use atlas::indexer::Indexer;
use atlas::nntp::BlockingPool;
use atlas::{db, nzb, search};
use common::{Post, Server, index_until_idle, mock, post, spawn_server};

fn index(port: u16, slice: i64) {
    let client = BlockingPool::new(&[mock(port, "secret", 2, 1)]);
    client.connect().unwrap();
    let mut indexer = Indexer::new(client, "backfill", db::open().unwrap());
    indexer.request_size = slice as u64;
    index_until_idle(&mut indexer);
}

#[test]
fn a_post_is_one_release_over_slices_and_servers() {
    let home = tempfile::tempdir().unwrap();
    // SAFETY: only test in this binary, set before any thread is spawned
    unsafe {
        std::env::set_var("ATLAS_HOME", home.path());
        std::env::set_var("ATLAS_NO_KEYRING", "1");
    }
    db::create_db().unwrap();

    // 50 images, each its own file name, from one poster
    let album = |first: u64, poster: &str| -> Vec<Post> {
        (1..=50u64)
            .map(|i| Post {
                author: poster.into(),
                // a post's message-ids are the same on every server, its numbers arent
                message_id: format!("<{i}.{}@x>", &poster[..1]),
                ..post(first + i, &format!(r#"[{i:02}/50] "DSC{i:04}.jpg" yEnc (1/1)"#), 10, vec![])
            })
            .collect()
    };
    // server A carries the first half of a's album, B all of it: same post, other numbers
    let mut on_a = album(0, "a <a@x>");
    on_a.truncate(25);
    let mut on_b = album(1000, "a <a@x>");
    // another poster's album of the same names, and a post without a counter
    on_b.extend(album(2000, "b <b@x>"));
    on_b.push(post(3000, r#""Lone.Picture.jpg" yEnc (1/1)"#, 10, vec![]));

    // small slices, soo the post is cut up
    index(spawn_server(Server::new(on_a)), 7);
    // the other server's own article numbers, from its own cursors
    db::open().unwrap().execute("delete from groups", []).unwrap();
    index(spawn_server(Server::new(on_b)), 11);

    let conn = db::open().unwrap();
    let rows: Vec<(String, i64, i64)> = conn
        .prepare("select name, parts, file_total from releases order by parts desc, name")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2).unwrap_or(0))))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert_eq!(rows.len(), 3, "{rows:?}");
    assert_eq!((rows[0].1, rows[0].2), (50, 50), "{rows:?}");
    assert_eq!((rows[1].1, rows[1].2), (50, 50), "{rows:?}");
    assert_eq!(rows[2].0, "Lone.Picture.jpg");

    // complete, and its nzb has every file
    let all = search::all_releases(0, 10).unwrap();
    let album = all.iter().find(|r| r.parts == Some(50)).unwrap();
    assert!(album.complete, "{album:?}");
    let xml = nzb::build_nzb(album.id).unwrap().unwrap();
    assert_eq!(xml.matches("<file ").count(), 50);
}
