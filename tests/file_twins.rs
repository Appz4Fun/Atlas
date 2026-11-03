//! A file twin in another shard, over a real database.
//!
//! One test: it points ATLAS_HOME at a temp dir before any thread is spawned.

mod common;

use atlas::duplicates::{Copy, copies, file_twins_with, fill_from_copies_with};
use atlas::parser::Release;
use atlas::{db, store};
use common::stage::release_of;

/// A file another poster's set describes the same way lends its articles, under either posting's name for it.
#[test]
fn a_file_twin_in_another_shard_fills_the_gaps() {
    let home = tempfile::tempdir().unwrap();
    // SAFETY: only test in this binary, set before any thread is spawned
    unsafe {
        std::env::set_var("ATLAS_HOME", home.path());
        std::env::set_var("ATLAS_NO_KEYRING", "1");
    }
    db::create_db().unwrap();
    let main = home.path().join("atlas.db");
    let groups: Vec<String> = (0..64).map(|n| format!("alt.binaries.g{n}")).collect();
    let in_shard = |s: usize| groups.iter().find(|g| store::shard_of(g) == s).unwrap().as_str();
    // this posting has 2 of 3 parts of a randomly named file; another poster's has all 3 under the real name
    store::save(&main, &[Release { group: in_shard(0).into(), ..release_of("Mine", "x9k2.bin", 2, 3) }]).unwrap();
    store::save(&main, &[Release { group: in_shard(5).into(), ..release_of("Theirs", "Movie.mkv", 3, 3) }]).unwrap();
    let shard_a = db::open_shard(&store::shard_path(&main, 0)).unwrap();
    let shard_b = db::open_shard(&store::shard_path(&main, 5)).unwrap();
    let mine: i64 = shard_a.query_row("select id from releases where name = 'Mine'", [], |r| r.get(0)).unwrap();
    let theirs: i64 = shard_b.query_row("select id from releases where name = 'Theirs'", [], |r| r.get(0)).unwrap();
    let insert = atlas::archive::store::INSERT_PAR2_FILE;
    let (md5, md5_16k, size) = (vec![1u8; 16], vec![2u8; 16], 2_100_000i64);
    shard_a
        .execute(insert, rusqlite::params![mine, vec![9u8; 16], vec![7u8; 16], md5, md5_16k, size, "Movie.mkv", 5, 1, 1])
        .unwrap();
    shard_b
        .execute(insert, rusqlite::params![theirs, vec![9u8; 16], vec![8u8; 16], md5, md5_16k, size, "Movie.mkv", 5, 1, 1])
        .unwrap();
    let conn = db::open().unwrap();
    let twins = file_twins_with(&conn, mine);
    assert_eq!(twins.len(), 1);
    assert_eq!((twins[0].id, twins[0].names.as_slice()), (theirs, &[("Movie.mkv".to_string(), "Movie.mkv".to_string())][..]));
    let found: Vec<(Copy, i64)> = copies(mine).unwrap().into_iter().map(|(kind, r)| (kind, r.id)).collect();
    assert_eq!(found, [(Copy::Twin, theirs)], "a file twin is a twin on the page");
    let mut articles = store::articles(&conn, mine).unwrap();
    assert_eq!(articles.len(), 2);
    assert_eq!(fill_from_copies_with(&conn, mine, &mut articles).unwrap(), 1);
    assert_eq!(articles.len(), 3);
    assert!(articles.iter().all(|a| a.filename.as_deref() == Some("x9k2.bin")), "under this posting's name");
    assert!(articles.iter().any(|a| a.message_id == "<Theirs.Movie.mkv.3@x>"));
    // the NZB the API serves has the borrowed article in it
    let nzb = atlas::nzb::build_nzb(mine).unwrap().unwrap();
    assert!(nzb.contains("Theirs.Movie.mkv.3@x"), "{nzb}");
}
