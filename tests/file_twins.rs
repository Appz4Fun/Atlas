//! A file twin in another shard, over a real database.
//!
//! One test: it points ATLAS_HOME at a temp dir before any thread is spawned.

mod common;

use atlas::duplicates::{Copy, copies, file_twins_with, fill_from_copies_of, fill_from_copies_with};
use atlas::parser::Release;
use atlas::{db, store};
use common::stage::{article, release_of};

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
    let in_shard = |s: usize| {
        groups.iter().filter(|g| store::shard_of(g) == s % store::SHARDS).nth(s / store::SHARDS).unwrap().as_str()
    };
    // this posting has 2 of 3 parts of a randomly named file; another poster's has all 3 under the real name
    store::save(&main, &[Release { group: in_shard(0).into(), ..release_of("Mine", "x9k2.bin", 2, 3) }]).unwrap();
    store::save(&main, &[Release { group: in_shard(5).into(), ..release_of("Theirs", "Movie.mkv", 3, 3) }]).unwrap();
    let shard_a = db::open_shard(&store::shard_path(&main, 0)).unwrap();
    let shard_b = db::open_shard(&store::shard_path(&main, 5 % store::SHARDS)).unwrap();
    let mine: i64 = shard_a.query_row("select id from releases where name = 'Mine'", [], |r| r.get(0)).unwrap();
    let theirs: i64 = shard_b.query_row("select id from releases where name = 'Theirs'", [], |r| r.get(0)).unwrap();
    let insert = atlas::archive::store::INSERT_PAR2_FILE;
    let (md5, md5_16k, size) = (vec![1u8; 16], vec![2u8; 16], 2_100_000i64);
    shard_a
        .execute(
            insert,
            rusqlite::params![mine, vec![9u8; 16], vec![7u8; 16], md5, md5_16k, size, "Movie.mkv", 5, 1, 1, 1],
        )
        .unwrap();
    shard_b
        .execute(
            insert,
            rusqlite::params![theirs, vec![9u8; 16], vec![8u8; 16], md5, md5_16k, size, "Movie.mkv", 5, 1, 1, 1],
        )
        .unwrap();
    let conn = db::open().unwrap();
    let twins = file_twins_with(&conn, mine);
    assert_eq!(twins.len(), 1);
    assert_eq!(
        (twins[0].id, twins[0].names.as_slice()),
        (theirs, &[("Movie.mkv".to_string(), "Movie.mkv".to_string())][..])
    );
    assert!(twins[0].one_file, "each set describes one data file");
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
    // once a part both have was read on both sides and the bytes differ, the other posting is another file, whatever
    // its set says: it lends nothing
    let crc = atlas::archive::store::SET_SEGMENT_CRC;
    shard_a.execute(crc, rusqlite::params![mine, "<Mine.x9k2.bin.1@x>", 700_000, 11]).unwrap();
    shard_b.execute(crc, rusqlite::params![theirs, "<Theirs.Movie.mkv.1@x>", 700_000, 22]).unwrap();
    let mut articles = store::articles(&conn, mine).unwrap();
    assert_eq!(fill_from_copies_with(&conn, mine, &mut articles).unwrap(), 0, "other bytes in part 1");
    assert!(!atlas::nzb::build_nzb(mine).unwrap().unwrap().contains("Theirs"));
    // the same bytes, and it lends again
    shard_b.execute(crc, rusqlite::params![theirs, "<Theirs.Movie.mkv.1@x>", 700_000, 11]).unwrap();
    let mut articles = store::articles(&conn, mine).unwrap();
    assert_eq!(fill_from_copies_with(&conn, mine, &mut articles).unwrap(), 1);

    // two postings that share only an NFO hold different archives under the same name, cut up the same way: the
    // other's archive parts must not fill this one's gaps, and its NFO (not posted here) maps to nothing
    let short = Release {
        group: in_shard(2).into(),
        articles: vec![
            article("data.rar", 1, 3, "<Short.data.rar.1@x>"),
            article("data.rar", 2, 3, "<Short.data.rar.2@x>"),
        ],
        ..release_of("Short", "data.rar", 2, 3)
    };
    let stranger = Release {
        group: in_shard(4).into(),
        articles: vec![
            article("data.rar", 1, 3, "<Stranger.data.rar.1@x>"),
            article("data.rar", 2, 3, "<Stranger.data.rar.2@x>"),
            article("data.rar", 3, 3, "<Stranger.data.rar.3@x>"),
            article("shared.nfo", 1, 1, "<Stranger.shared.nfo.1@x>"),
        ],
        ..release_of("Stranger", "data.rar", 3, 3)
    };
    store::save(&main, &[short]).unwrap();
    store::save(&main, &[stranger]).unwrap();
    let shard_c = db::open_shard(&store::shard_path(&main, 2 % store::SHARDS)).unwrap();
    let shard_d = db::open_shard(&store::shard_path(&main, 4 % store::SHARDS)).unwrap();
    let short: i64 = shard_c.query_row("select id from releases where name = 'Short'", [], |r| r.get(0)).unwrap();
    let stranger: i64 = shard_d.query_row("select id from releases where name = 'Stranger'", [], |r| r.get(0)).unwrap();
    let nfo = (vec![3u8; 16], vec![3u8; 16], 900i64);
    for (shard, id, set, rar_md5) in [(&shard_c, short, 11u8, 4u8), (&shard_d, stranger, 12u8, 5u8)] {
        shard
            .execute(
                insert,
                rusqlite::params![id, vec![1u8; 16], vec![set; 16], nfo.0, nfo.1, nfo.2, "shared.nfo", 1, 1, 1, 1],
            )
            .unwrap();
        shard
            .execute(
                insert,
                rusqlite::params![
                    id,
                    vec![2u8; 16],
                    vec![set; 16],
                    vec![rar_md5; 16],
                    vec![rar_md5; 16],
                    size,
                    "data.rar",
                    2,
                    1,
                    1,
                    1
                ],
            )
            .unwrap();
    }
    let twins = file_twins_with(&conn, short);
    assert_eq!(twins.len(), 1);
    assert_eq!(
        (twins[0].id, twins[0].names.as_slice()),
        (stranger, &[("shared.nfo".to_string(), "shared.nfo".to_string())][..])
    );
    let mut articles = store::articles(&conn, short).unwrap();
    assert_eq!(fill_from_copies_with(&conn, short, &mut articles).unwrap(), 0, "nothing of the other archive");
    assert_eq!(articles.len(), 2);
    let nzb = atlas::nzb::build_nzb(short).unwrap().unwrap();
    assert!(!nzb.contains("Stranger"), "{nzb}");
    assert!(twins[0].one_file, "data.rar is the one data file either set describes");
    // a set that describes a sample beside the movie doesn't say which one a random name posted
    shard_c
        .execute(
            insert,
            rusqlite::params![
                short,
                vec![3u8; 16],
                vec![11u8; 16],
                vec![6u8; 16],
                vec![6u8; 16],
                50_000_000i64,
                "sample.mkv",
                3,
                1,
                1,
                1
            ],
        )
        .unwrap();
    assert!(!file_twins_with(&conn, short)[0].one_file);

    // an article borrowed from one donor keeps what it was read to hold: a later donor whose copy of that part
    // differs is another file, and lends nothing, even though this posting never had the part itself
    let base = Release { group: in_shard(6).into(), ..release_of("Base", "base.bin", 1, 3) };
    let donb = Release { group: in_shard(1).into(), ..release_of("DonB", "Base.mkv", 2, 3) };
    let donc = Release {
        group: in_shard(7).into(),
        articles: vec![
            article("Base.mkv", 2, 3, "<DonC.Base.mkv.2@x>"),
            article("Base.mkv", 3, 3, "<DonC.Base.mkv.3@x>"),
        ],
        ..release_of("DonC", "Base.mkv", 3, 3)
    };
    for release in [base, donb, donc] {
        store::save(&main, &[release]).unwrap();
    }
    let shard = |s: usize| db::open_shard(&store::shard_path(&main, s % store::SHARDS)).unwrap();
    let id = |s: usize, name: &str| -> i64 {
        shard(s).query_row("select id from releases where name = ?", [name], |r| r.get(0)).unwrap()
    };
    let (base, donb, donc) = (id(6, "Base"), id(1, "DonB"), id(7, "DonC"));
    assert!(donb < donc, "file twins lend in id order: B first");
    let base_md5 = (vec![8u8; 16], vec![8u8; 16], size);
    for (s, row, set) in [(6, base, 21u8), (1, donb, 22u8), (7, donc, 23u8)] {
        shard(s)
            .execute(
                insert,
                rusqlite::params![
                    row,
                    vec![4u8; 16],
                    vec![set; 16],
                    base_md5.0,
                    base_md5.1,
                    base_md5.2,
                    "Base.mkv",
                    9,
                    1,
                    1,
                    1
                ],
            )
            .unwrap();
    }
    shard(1).execute(crc, rusqlite::params![donb, "<DonB.Base.mkv.2@x>", 700_000, 5]).unwrap();
    shard(7).execute(crc, rusqlite::params![donc, "<DonC.Base.mkv.2@x>", 700_000, 6]).unwrap();
    let mut articles = store::articles(&conn, base).unwrap();
    assert_eq!(fill_from_copies_with(&conn, base, &mut articles).unwrap(), 1, "B's part 2 only");
    assert!(articles.iter().any(|a| a.message_id == "<DonB.Base.mkv.2@x>"));
    assert!(!articles.iter().any(|a| a.message_id.contains("DonC")), "C's part 2 differs from the part now held");

    // a part that came from another row of the same post (one cut across groups) is checked the same way: what that
    // row was read to hold counts against the donors, whether or not that row is a donor itself
    let split = Release {
        group: in_shard(2).into(),
        articles: vec![article("base.bin", 2, 3, "<Split.base.bin.2@x>")],
        ..release_of("Split", "base.bin", 1, 3)
    };
    store::save(&main, &[split]).unwrap();
    let split = id(2, "Split");
    shard(2).execute(crc, rusqlite::params![split, "<Split.base.bin.2@x>", 700_000, 7]).unwrap();
    let gathered = || {
        let mut all = store::articles(&conn, base).unwrap();
        all.extend(store::articles(&conn, split).unwrap());
        all
    };
    // the other row's part 2 holds other bytes than B's and C's: neither lends anything
    let mut articles = gathered();
    assert_eq!(fill_from_copies_of(&conn, base, &[base, split], &mut articles).unwrap(), 0);
    // without that row's CRCs the conflict goes unseen and C lends part 3
    let mut articles = gathered();
    assert_eq!(fill_from_copies_with(&conn, base, &mut articles).unwrap(), 1);
}
