//! A password found on one posting, shown on the postings that hold the same bytes, over a real database.
//!
//! One test: it points ATLAS_HOME at a temp dir before any thread is spawned.

mod common;

use atlas::archive::store::{INSERT_PAR2_FILE, SET_NFO_PASSWORD};
use atlas::duplicates::{SET_CONTENT, SET_POSTED, first_password, own_password_hint, par2_name_password};
use atlas::parser::Release;
use atlas::{db, store};
use common::stage::release_of;

#[test]
fn a_password_carries_to_xposts_and_twins_but_not_alts() {
    let home = tempfile::tempdir().unwrap();
    // SAFETY: only test in this binary, set before any thread is spawned
    unsafe {
        std::env::set_var("ATLAS_HOME", home.path());
        std::env::set_var("ATLAS_NO_KEYRING", "1");
    }
    db::create_db().unwrap();
    let main = home.path().join("atlas.db");
    let groups: Vec<String> = (0..64).map(|n| format!("alt.binaries.g{n}")).collect();
    let in_shard = |s: usize| groups.iter().find(|g| store::shard_of(g) == s).unwrap().clone();
    let save = |name: &str, file: &str, s: usize| -> (i64, rusqlite::Connection) {
        store::save(&main, &[Release { group: in_shard(s), ..release_of(name, file, 2, 2) }]).unwrap();
        let shard = db::open_shard(&store::shard_path(&main, s)).unwrap();
        let id = shard.query_row("select id from releases where name = ?", [name], |r| r.get(0)).unwrap();
        (id, shard)
    };
    // the first upload, its crosspost to another group, a repost of the same volumes renamed, and the same film
    // packed apart
    let (first, shard_a) = save("Gone.With.The.Wind.1939", "YMFFZeoeVI6fG4iz.part1.rar", 0);
    let (xpost, shard_b) = save("Gone.With.The.Wind.1939", "YMFFZeoeVI6fG4iz.part1.rar", 1);
    let (twin, shard_c) = save("oCBvoIlhFvS0L3k1", "oCBvoIlhFvS0L3k1.part1.rar", 2);
    let (alt, shard_d) = save("Gone.With.The.Wind.1939.Other", "Qx7Lm2Pz9Rt4Vb8N.part1.rar", 3);
    for (shard, id, posted, set) in [
        (&shard_a, first, b"ymff".as_slice(), 1u8),
        (&shard_b, xpost, b"ymff", 1),
        (&shard_c, twin, b"ocbv", 1),
        (&shard_d, alt, b"other", 2),
    ] {
        shard.execute(SET_POSTED, rusqlite::params![id, posted]).unwrap();
        shard.execute(SET_CONTENT, rusqlite::params![id, b"same-film".as_slice()]).unwrap();
        // the set names the first upload's volumes, whatever the posting called them; the alt's are its own
        let (name, md5) =
            if set == 1 { ("YMFFZeoeVI6fG4iz.part1.rar", 7u8) } else { ("Qx7Lm2Pz9Rt4Vb8N.part1.rar", 8) };
        shard
            .execute(
                INSERT_PAR2_FILE,
                rusqlite::params![
                    id,
                    vec![set; 16],
                    vec![set; 16],
                    vec![md5; 16],
                    vec![md5; 16],
                    2_100_000i64,
                    name,
                    5,
                    1,
                    1,
                    1
                ],
            )
            .unwrap();
    }
    shard_a.execute(SET_NFO_PASSWORD, rusqlite::params![first, "YMFFZeoeVI6fG4iz"]).unwrap();
    let conn = db::open().unwrap();

    // the renamed repost's set still names the volumes: that name is a hint of its own
    let hint = par2_name_password(&conn, twin).unwrap();
    assert_eq!((hint.password.as_str(), hint.source), ("YMFFZeoeVI6fG4iz", "the PAR2 set's file names"));
    assert!(par2_name_password(&conn, xpost).is_none(), "posted under the set's own name: nothing renamed");
    assert!(par2_name_password(&conn, alt).is_none(), "the alt's volumes carry their set's name too");
    assert!(own_password_hint(&conn, xpost, None).is_none());

    // the xpost has no hint of its own, soo it takes the first upload's
    let rows: Vec<_> = atlas::duplicates::copies_with(&conn, xpost).unwrap();
    let same: Vec<_> = rows
        .iter()
        .filter(|(kind, _)| matches!(kind, atlas::duplicates::Copy::Xpost | atlas::duplicates::Copy::Twin))
        .map(|(_, r)| r)
        .collect();
    let carried = first_password(&conn, same.iter().copied()).unwrap();
    assert_eq!((carried.from, carried.hint.password.as_str()), (first, "YMFFZeoeVI6fG4iz"));

    // the pages say so, and the alt is left alone
    let key = "k";
    let login = atlas::web::post("/login", "key=k", None, false, "", key).unwrap();
    let cookie =
        login.headers.iter().find(|(h, _)| h == "Set-Cookie").unwrap().1.split(';').next().unwrap().to_string();
    let page = |id: i64| atlas::web::get("/ui/release", &format!("id={id}"), Some(&cookie), "", key).unwrap().body;
    let xpost_page = page(xpost);
    assert!(
        xpost_page.contains(&format!(
            "<code>YMFFZeoeVI6fG4iz</code> inherited from <a href=\"/ui/release?id={first}\">Gone.With.The.Wind.1939</a>"
        )),
        "{xpost_page}"
    );
    let alt_page = page(alt);
    assert!(!alt_page.contains("inherited from"), "an alt is packed apart: {alt_page}");
    let first_page = page(first);
    // on the first upload's page its xpost and twin cards show the password too, the alt's card doesn't
    assert_eq!(first_page.matches("inherited from").count(), 1, "the xpost's card; the twin has its own: {first_page}");
    assert!(first_page.contains("<code>YMFFZeoeVI6fG4iz</code> found in the PAR2 set's file names"), "{first_page}");
}
