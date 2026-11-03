//! A set's recovery volumes in other shards, tied to their upload at read time, over a real database.
//!
//! One test: it points ATLAS_HOME at a temp dir before any thread is spawned.

mod common;

use atlas::archive::link::{LINK_LEAD, LINK_RECOVERY};
use atlas::archive::store::INSERT_PAR2_FILE;
use atlas::parser::Release;
use atlas::release_info::linked_post_with;
use atlas::{db, store};
use common::stage::release_of;

/// A sealed volume of the same set in another shard joins the upload when the same poster put it up. A set's
/// packets are public, soo another poster's volume sealed by a copy of its Main packet does not.
#[test]
fn a_volume_in_another_shard_joins_its_poster_s_upload_only() {
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
    let uploader = "uploader <u@x>".to_string();
    // the upload: a data file and a volume in shard 0, linked by the link step; its second volume went to shard 3
    let lead = Release { group: in_shard(0).into(), poster: uploader.clone(), ..release_of("Lead", "Movie.mkv", 3, 3) };
    let vol1 = Release {
        group: in_shard(0).into(),
        poster: uploader.clone(),
        ..release_of("Vol1", "Movie.vol00+01.par2", 1, 1)
    };
    let vol2 = Release {
        group: in_shard(3).into(),
        poster: uploader.clone(),
        ..release_of("Vol2", "Movie.vol01+02.par2", 1, 1)
    };
    // someone else's volume, sealed by a copy of the set's public Main packet, in shard 5
    let forged = Release {
        group: in_shard(5).into(),
        poster: "someone else <e@x>".into(),
        ..release_of("Forged", "Movie.vol03+04.par2", 1, 1)
    };
    for release in [lead, vol1, vol2, forged] {
        store::save(&main, &[release]).unwrap();
    }
    let shard = |s: usize| db::open_shard(&store::shard_path(&main, s)).unwrap();
    let id = |s: usize, name: &str| -> i64 {
        shard(s).query_row("select id from releases where name = ?", [name], |r| r.get(0)).unwrap()
    };
    let (lead, vol1, vol2, forged) = (id(0, "Lead"), id(0, "Vol1"), id(3, "Vol2"), id(5, "Forged"));
    let set = vec![7u8; 16];
    let shard0 = shard(0);
    // someone else's whole upload of the same set first, in the lead's shard: eight volumes linked under their own
    // lead, soo a lookup by set that stopped at the first few rows would never reach the uploader's
    let other_lead = Release {
        group: in_shard(0).into(),
        poster: "someone else <e@x>".into(),
        ..release_of("OtherLead", "Movie.mkv", 3, 3)
    };
    store::save(&main, &[other_lead]).unwrap();
    let other_lead = id(0, "OtherLead");
    shard0.execute(LINK_LEAD, rusqlite::params![other_lead]).unwrap();
    for n in 1..=8 {
        shard0.execute(LINK_RECOVERY, rusqlite::params![other_lead + 8 * 1000 * n, other_lead, 1, set]).unwrap();
    }
    shard0.execute(LINK_LEAD, rusqlite::params![lead]).unwrap();
    shard0.execute(LINK_RECOVERY, rusqlite::params![vol1, lead, 1, set]).unwrap();
    for (s, row, file_id) in [(0, vol1, 1u8), (3, vol2, 1u8), (5, forged, 1u8)] {
        shard(s)
            .execute(
                INSERT_PAR2_FILE,
                rusqlite::params![
                    row,
                    vec![file_id; 16],
                    set,
                    vec![2u8; 16],
                    vec![3u8; 16],
                    2_100_000i64,
                    "Movie.mkv",
                    5,
                    1,
                    1,
                    1
                ],
            )
            .unwrap();
    }
    let conn = db::open().unwrap();
    let post = linked_post_with(&conn, lead).unwrap().expect("the lead is linked");
    assert_eq!(post.ids, {
        let mut ids = vec![lead, vol1, vol2];
        ids.sort_unstable();
        ids
    });
    assert!(post.recovery.contains(&vol2), "the uploader's volume in another shard is recovery data of the upload");
    assert!(!post.ids.contains(&forged), "another poster's volume of the same set is not");
    // the volume finds its upload through its set, the forged one finds nothing
    assert_eq!(linked_post_with(&conn, vol2).unwrap().map(|p| p.ids), Some(post.ids.clone()));
    assert!(linked_post_with(&conn, forged).unwrap().is_none());
}
