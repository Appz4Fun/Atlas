//! Other copies across shards, over a real database.
//!
//! One test: it points ATLAS_HOME at a temp dir before any thread is spawned.

mod common;

use atlas::duplicates::{
    ClusterKind, Copy, CopyCount, SET_CONTENT, SET_POSTED, cluster_count_with, clusters_with, copies, counts,
    later_copies,
};
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

    let conn = db::open().unwrap();
    let new_form = [2u8; 21];
    shard_d.execute(SET_POSTED, rusqlite::params![other, new_form.as_slice()]).unwrap();
    atlas::duplicates::forget_old_twins(&conn);
    let kept: Option<Vec<u8>> = shard_d
        .query_row("select posted from release_fingerprint where release_id = ?", [other], |r| r.get(0))
        .unwrap();
    assert_eq!(kept.as_deref(), Some(new_form.as_slice()), "the new form stays");
    for (shard, id, posted) in [(&shard_a, first, b"same-posted".as_slice()), (&shard_b, twin, b"same-posted")] {
        let gone: Option<Vec<u8>> =
            shard.query_row("select posted from release_fingerprint where release_id = ?", [id], |r| r.get(0)).unwrap();
        assert_eq!(gone, None, "the old form goes");
        shard.execute(SET_POSTED, rusqlite::params![id, posted]).unwrap();
    }
    let ids = |kind| -> Vec<Vec<i64>> {
        clusters_with(&conn, kind, 0, 10)
            .unwrap()
            .into_iter()
            .map(|members| {
                let mut ids: Vec<i64> = members.iter().map(|r| r.id).collect();
                ids.sort_unstable();
                ids
            })
            .collect()
    };
    let sorted = |mut list: Vec<i64>| {
        list.sort_unstable();
        list
    };
    assert_eq!(cluster_count_with(&conn, ClusterKind::Twins).unwrap(), 1);
    assert_eq!(ids(ClusterKind::Twins), [sorted(vec![first, twin])], "not the pair, and not the other");
    assert_eq!(cluster_count_with(&conn, ClusterKind::Pairs).unwrap(), 1);
    assert_eq!(ids(ClusterKind::Pairs), [sorted(vec![first, twin, pair])]);
    assert!(clusters_with(&conn, ClusterKind::Twins, 1, 10).unwrap().is_empty(), "past the end");
    drop(conn);

    // a purged release is no one's copy
    shard_b.execute("delete from releases where id = ?", [twin]).unwrap();
    let found: Vec<(Copy, i64)> = copies(first).unwrap().into_iter().map(|(kind, r)| (kind, r.id)).collect();
    assert_eq!(found, [(Copy::Pair, pair)]);
    assert_eq!(cluster_count_with(&db::open().unwrap(), ClusterKind::Twins).unwrap(), 0, "its cluster goes too");
}
