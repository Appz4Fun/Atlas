//! Other copies across shards, over a real database.
//!
//! One test: it points ATLAS_HOME at a temp dir before any thread is spawned.

mod common;

use atlas::duplicates::{Copy, CopyCount, SET_CONTENT, SET_POSTED, copies, counts, later_copies};
use atlas::parser::Release;
use atlas::{db, store};
use common::stage::release_of;

fn release(main: &std::path::Path, name: &str, group: &str) -> (i64, rusqlite::Connection) {
    store::save(main, &[Release { group: group.into(), ..release_of(name, "x.bin", 2, 2) }]).unwrap();
    let shard = db::open_shard(&store::shard_path(main, store::shard_of(group))).unwrap();
    let id = shard.query_row("select id from releases where name = ?", [name], |r| r.get(0)).unwrap();
    (id, shard)
}

#[test]
fn copies_are_found_across_shards_and_go_with_their_release() {
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
    let (first, shard_a) = release(&main, "Movie.Rar5-A", in_shard(0));
    let (twin, shard_b) = release(&main, "Movie.Rar5.Repost-B", in_shard(1));
    let (pair, shard_c) = release(&main, "Movie.Seven-C", in_shard(2));
    let (other, shard_d) = release(&main, "Other.Movie-D", in_shard(3));

    for (shard, id, posted, content) in [
        (&shard_a, first, b"same-posted".as_slice(), b"same-content".as_slice()),
        (&shard_b, twin, b"same-posted", b"same-content"),
        (&shard_c, pair, b"posted-as-7z", b"same-content"),
        (&shard_d, other, b"other-posted", b"other-content"),
    ] {
        shard.execute(SET_POSTED, rusqlite::params![id, posted]).unwrap();
        shard.execute(SET_CONTENT, rusqlite::params![id, content]).unwrap();
    }

    let found: Vec<(Copy, i64)> = copies(first).unwrap().into_iter().map(|(kind, r)| (kind, r.id)).collect();
    assert_eq!(found, [(Copy::Twin, twin), (Copy::Pair, pair)]);
    assert!(copies(other).unwrap().is_empty());

    let tally = counts(&[first, pair, other]).unwrap();
    assert_eq!(tally[&first], CopyCount { twins: 1, pairs: 1 });
    assert_eq!(tally[&pair], CopyCount { twins: 0, pairs: 2 });
    assert!(!tally.contains_key(&other));
    assert_eq!(later_copies(&[first, other, twin, pair]).unwrap(), [twin, pair]);

    // a purged release is no one's copy
    shard_b.execute("delete from releases where id = ?", [twin]).unwrap();
    let found: Vec<(Copy, i64)> = copies(first).unwrap().into_iter().map(|(kind, r)| (kind, r.id)).collect();
    assert_eq!(found, [(Copy::Pair, pair)]);
}
