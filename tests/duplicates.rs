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
use common::stage::{article, release_of};

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
    let (alt, shard_c) = release(&main, "Movie.Seven-C", in_shard(2));
    let (other, shard_d) = release(&main, "Other.Movie-D", in_shard(3));

    for (shard, id, posted, content) in [
        (&shard_a, first, b"same-posted".as_slice(), b"same-content".as_slice()),
        (&shard_b, twin, b"same-posted", b"same-content"),
        (&shard_c, alt, b"posted-as-7z", b"same-content"),
        (&shard_d, other, b"other-posted", b"other-content"),
    ] {
        shard.execute(SET_POSTED, rusqlite::params![id, posted]).unwrap();
        shard.execute(SET_CONTENT, rusqlite::params![id, content]).unwrap();
    }

    let found: Vec<(Copy, i64)> = copies(first).unwrap().into_iter().map(|(kind, r)| (kind, r.id)).collect();
    assert_eq!(found, [(Copy::Twin, twin), (Copy::Alt, alt)]);
    assert!(copies(other).unwrap().is_empty());

    let tally = counts(&[first, alt, other]).unwrap();
    assert_eq!(tally[&first], CopyCount { xposts: 0, twins: 1, alts: 1 });
    assert_eq!(tally[&alt], CopyCount { xposts: 0, twins: 0, alts: 2 });
    assert!(!tally.contains_key(&other));
    assert_eq!(later_copies(&[first, other, twin, alt]).unwrap(), [twin, alt]);

    let conn = db::open().unwrap();
    let new_form = [2u8; 21];
    shard_d.execute(SET_POSTED, rusqlite::params![other, new_form.as_slice()]).unwrap();
    let par2_bit = atlas::postprocess::step::mask(atlas::archive::steps::PAR2_STEP);
    shard_a.execute("insert or replace into pp_state (release_id, done) values (?, ?)", [first, par2_bit | 4]).unwrap();
    atlas::duplicates::forget_old_twins(&conn);
    let done: i64 = shard_a.query_row("select done from pp_state where release_id = ?", [first], |r| r.get(0)).unwrap();
    assert_eq!(done, 4, "only the PAR2 step runs again");
    let queued: i64 =
        shard_a.query_row("select count(*) from pp_queue where release_id = ?", [first], |r| r.get(0)).unwrap();
    assert_eq!(queued, 1, "and it's queued");
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
    assert_eq!(ids(ClusterKind::Twins), [sorted(vec![first, twin])], "not the alt, and not the other");
    assert_eq!(cluster_count_with(&conn, ClusterKind::Alts).unwrap(), 1);
    assert_eq!(ids(ClusterKind::Alts), [sorted(vec![first, twin, alt])]);
    assert!(clusters_with(&conn, ClusterKind::Twins, 1, 10).unwrap().is_empty(), "past the end");
    drop(conn);

    // the first release crossposted to another group: the very same articles, the same posted files. It's that
    // upload, an xpost, not a separate upload of it, while the repost under other message-ids stays a twin
    let xpost_group = in_shard(4);
    store::save(&main, &[Release { group: xpost_group.into(), ..release_of("Movie.Rar5-A", "x.bin", 2, 2) }]).unwrap();
    let shard_e = db::open_shard(&store::shard_path(&main, 4)).unwrap();
    let xpost: i64 =
        shard_e.query_row("select id from releases where name = 'Movie.Rar5-A'", [], |r| r.get(0)).unwrap();
    shard_e.execute(SET_POSTED, rusqlite::params![xpost, b"same-posted".as_slice()]).unwrap();
    shard_e.execute(SET_CONTENT, rusqlite::params![xpost, b"same-content".as_slice()]).unwrap();
    let conn = db::open().unwrap();
    assert!(atlas::duplicates::shares_articles(&conn, first, xpost));
    assert!(atlas::duplicates::shares_articles(&conn, xpost, first));
    assert!(!atlas::duplicates::shares_articles(&conn, first, twin), "a repost has message-ids of its own");
    let found: Vec<(Copy, i64)> = copies(first).unwrap().into_iter().map(|(kind, r)| (kind, r.id)).collect();
    assert_eq!(found, [(Copy::Xpost, xpost), (Copy::Twin, twin), (Copy::Alt, alt)], "the same upload first");
    let found: Vec<(Copy, i64)> = copies(twin).unwrap().into_iter().map(|(kind, r)| (kind, r.id)).collect();
    let mut found_ids = found.clone();
    found_ids.sort_by_key(|(_, id)| *id);
    let mut want = vec![(Copy::Twin, first), (Copy::Twin, xpost), (Copy::Alt, alt)];
    want.sort_by_key(|(_, id)| *id);
    assert_eq!(found_ids, want, "to the repost, both are another upload");
    assert!(atlas::duplicates::can_fill(&copies(xpost).unwrap()));
    let tally = counts(&[first, twin, xpost]).unwrap();
    assert_eq!(tally[&first], CopyCount { xposts: 1, twins: 1, alts: 1 });
    assert_eq!(tally[&xpost], CopyCount { xposts: 1, twins: 1, alts: 1 });
    assert_eq!(tally[&twin], CopyCount { xposts: 0, twins: 2, alts: 1 });
    let cluster = clusters_with(&conn, ClusterKind::Twins, 0, 10).unwrap().remove(0);
    let marks = atlas::duplicates::xposts_of_earlier(&conn, &cluster);
    let flagged: Vec<i64> = cluster.iter().zip(&marks).filter(|(_, x)| **x).map(|(r, _)| r.id).collect();
    assert_eq!(flagged.len(), 1, "one of the two rows of the first upload is the other crossposted");
    assert!(flagged[0] == first || flagged[0] == xpost);
    shard_e.execute("delete from releases where id = ?", [xpost]).unwrap();
    shard_e.execute("delete from release_fingerprint where release_id = ?", [xpost]).unwrap();
    drop(conn);

    // three groups' rows of one crosspost, each missing other parts: the first and last share no file's first
    // message-id, but each shares one with the middle row, soo all three are one upload
    let rows = [
        ("Tri-A", 5usize, vec![article("f1", 1, 2, "<f1.1@x>"), article("f2", 2, 2, "<f2.2@x>")]),
        (
            "Tri-B",
            6,
            vec![article("f1", 1, 2, "<f1.1@x>"), article("f2", 1, 2, "<f2.1@x>"), article("f2", 2, 2, "<f2.2@x>")],
        ),
        ("Tri-C", 7, vec![article("f1", 2, 2, "<f1.2@x>"), article("f2", 1, 2, "<f2.1@x>")]),
    ];
    let mut tri = Vec::new();
    for (name, s, articles) in rows {
        store::save(&main, &[Release { group: in_shard(s).into(), articles, ..release_of(name, "f1", 0, 2) }]).unwrap();
        let shard = db::open_shard(&store::shard_path(&main, s)).unwrap();
        let id: i64 = shard.query_row("select id from releases where name = ?", [name], |r| r.get(0)).unwrap();
        shard.execute(SET_POSTED, rusqlite::params![id, b"tri-posted".as_slice()]).unwrap();
        tri.push(id);
    }
    let conn = db::open().unwrap();
    assert!(!atlas::duplicates::shares_articles(&conn, tri[0], tri[2]));
    let cluster = clusters_with(&conn, ClusterKind::Twins, 0, 10)
        .unwrap()
        .into_iter()
        .find(|members| members.iter().any(|r| r.id == tri[0]))
        .unwrap();
    assert_eq!(cluster.iter().map(|r| r.id).collect::<Vec<_>>(), tri);
    assert_eq!(atlas::duplicates::xposts_of_earlier(&conn, &cluster), [false, true, true], "C ties through B");
    for (s, id) in [(5usize, tri[0]), (6, tri[1]), (7, tri[2])] {
        let shard = db::open_shard(&store::shard_path(&main, s)).unwrap();
        shard.execute("delete from release_fingerprint where release_id = ?", [id]).unwrap();
    }
    drop(conn);

    // a purged release is no one's copy
    shard_b.execute("delete from releases where id = ?", [twin]).unwrap();
    let found: Vec<(Copy, i64)> = copies(first).unwrap().into_iter().map(|(kind, r)| (kind, r.id)).collect();
    assert_eq!(found, [(Copy::Alt, alt)]);
    assert_eq!(cluster_count_with(&db::open().unwrap(), ClusterKind::Twins).unwrap(), 0, "its cluster goes too");
}
