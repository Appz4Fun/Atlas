//! A posting holding a file of the same size and whole-file CRC32 lends articles, over a real database.
//!
//! One test: it points ATLAS_HOME at a temp dir before any thread is spawned.

mod common;

use atlas::duplicates::fill_from_copies_with;
use atlas::keys::{FileCrc, crc_donors, named_crc_files};
use atlas::parser::Release;
use atlas::{db, store};
use common::stage::release_of;

const SIZE: u64 = 2_100_000;
const CRC: u32 = 0x5eed_c0de;

fn probe_json(file: &str, size: u64, crc: u32) -> String {
    let mut probe = serde_json::to_value(atlas::probe::ProbeInfo::default()).unwrap();
    probe["file"] = file.into();
    probe["yenc"] = serde_json::json!({ "file_size": size });
    probe["file_crc32"] = crc.into();
    probe.to_string()
}

#[test]
fn a_same_crc_file_under_another_name_fills_the_gaps() {
    let home = tempfile::tempdir().unwrap();
    // SAFETY: only test in this binary, set before any thread is spawned
    unsafe {
        std::env::set_var("ATLAS_HOME", home.path());
        std::env::set_var("ATLAS_NO_KEYRING", "1");
    }
    db::create_db().unwrap();
    let main = home.path().join("atlas.db");
    let groups: Vec<String> = (0..64).map(|n| format!("alt.binaries.g{n}")).collect();
    let in_shard = |s: usize| {
        groups.iter().filter(|g| store::shard_of(g) == s % store::SHARDS).nth(s / store::SHARDS).unwrap().as_str()
    };
    // this posting lost part 3 of Movie.mkv; a repost under another name has all 3, cut the same way; a third posting
    // has the same file cut into 4 and can't lend
    store::save(&main, &[Release { group: in_shard(0).into(), ..release_of("Mine", "Movie.mkv", 2, 3) }]).unwrap();
    store::save(&main, &[Release { group: in_shard(5).into(), ..release_of("Repost", "a8f3k.mkv", 3, 3) }]).unwrap();
    store::save(&main, &[Release { group: in_shard(3).into(), ..release_of("Recut", "Recut.mkv", 4, 4) }]).unwrap();
    let shard = |s: usize| {
        let c = db::open_shard(&store::shard_path(&main, s % store::SHARDS)).unwrap();
        atlas::postprocess::schema::migrate(&c).unwrap();
        c
    };
    let (a, b, c) = (shard(0), shard(5), shard(3));
    let id = |conn: &rusqlite::Connection, name: &str| -> i64 {
        conn.query_row("select id from releases where name = ?1", [name], |r| r.get(0)).unwrap()
    };
    let (mine, repost, recut) = (id(&a, "Mine"), id(&b, "Repost"), id(&c, "Recut"));
    let file = FileCrc::new(SIZE as i64, CRC).unwrap();
    for (conn, rid, name) in [(&a, mine, "Movie.mkv"), (&b, repost, "a8f3k.mkv"), (&c, recut, "Recut.mkv")] {
        conn.execute(
            "insert into release_probe (release_id, crc32, info) values (?1, ?2, ?3)",
            rusqlite::params![rid, i64::from(CRC), probe_json(name, SIZE, CRC)],
        )
        .unwrap();
        conn.execute(
            "insert into release_keys (release_id, file_crc) values (?1, ?2)",
            rusqlite::params![rid, file.key()],
        )
        .unwrap();
    }
    let conn = db::open().unwrap();
    let named = named_crc_files(&conn, mine);
    assert_eq!(named.len(), 1);
    assert_eq!((named[0].file, named[0].name.as_str()), (file, "Movie.mkv"));
    let donors = crc_donors(&conn, mine, file, None);
    assert_eq!(
        donors,
        vec![
            (repost.max(recut), name_of(repost.max(recut), repost)),
            (repost.min(recut), name_of(repost.min(recut), repost))
        ]
    );

    let mut articles = store::articles(&conn, mine).unwrap();
    assert_eq!(articles.len(), 2);
    assert_eq!(fill_from_copies_with(&conn, mine, &mut articles).unwrap(), 1, "part 3 from the repost only");
    assert_eq!(articles.len(), 3);
    assert!(articles.iter().all(|a| a.filename.as_deref() == Some("Movie.mkv")), "under this posting's name");
    assert!(articles.iter().any(|a| a.message_id == "<Repost.a8f3k.mkv.3@x>"));
    let nzb = atlas::nzb::build_nzb(mine).unwrap().unwrap();
    assert!(nzb.contains("Repost.a8f3k.mkv.3@x"), "{nzb}");
    assert!(!nzb.contains("Recut"), "a file cut another way lends nothing");

    // a probe that says another CRC is not the file, whatever its key says
    b.execute(
        "update release_probe set info = ?1 where release_id = ?2",
        rusqlite::params![probe_json("a8f3k.mkv", SIZE, CRC ^ 1), repost],
    )
    .unwrap();
    assert!(crc_donors(&conn, mine, file, None).iter().all(|(d, _)| *d != repost));
    let mut articles = store::articles(&conn, mine).unwrap();
    assert_eq!(fill_from_copies_with(&conn, mine, &mut articles).unwrap(), 0);

    // a PAR2 set that lists the file by size and CRC vouches for it under the set's name
    b.execute(
        atlas::archive::store::INSERT_PAR2_FILE,
        rusqlite::params![
            repost,
            vec![9u8; 16],
            vec![8u8; 16],
            vec![1u8; 16],
            vec![2u8; 16],
            SIZE as i64,
            "a8f3k.mkv",
            i64::from(CRC),
            1,
            1,
            1
        ],
    )
    .unwrap();
    let mut articles = store::articles(&conn, mine).unwrap();
    assert_eq!(fill_from_copies_with(&conn, mine, &mut articles).unwrap(), 1, "the set's word");
    // this posting's own set says another MD5 for that size and CRC: a collision or a lie, and nothing is lent
    a.execute(
        atlas::archive::store::INSERT_PAR2_FILE,
        rusqlite::params![
            mine,
            vec![9u8; 16],
            vec![7u8; 16],
            vec![6u8; 16],
            vec![2u8; 16],
            SIZE as i64,
            "Movie.mkv",
            i64::from(CRC),
            1,
            1,
            1
        ],
    )
    .unwrap();
    let named = named_crc_files(&conn, mine);
    assert!(named.iter().all(|n| n.md5.as_deref() == Some(&[6u8; 16][..])), "{named:?}");
    let mut articles = store::articles(&conn, mine).unwrap();
    assert_eq!(fill_from_copies_with(&conn, mine, &mut articles).unwrap(), 0, "the sets disagree");
}

fn name_of(id: i64, repost: i64) -> String {
    if id == repost { "a8f3k.mkv".into() } else { "Recut.mkv".into() }
}
