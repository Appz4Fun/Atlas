//! What a rename reaches: search, listings, the API, and the obfuscated menu.
//!
//! One test: it points ATLAS_HOME at a temp dir before any thread is spawned.

mod common;

use atlas::{api, db, nzb, search, store};
use common::stage::{STAGE_GROUP, release_of};

const NEW_NAME: &str = "Real.Movie.2024.1080p.WEB-DL.x264-GRP";

#[test]
fn a_rename_reaches_everything() {
    let home = tempfile::tempdir().unwrap();
    // SAFETY: only test in this binary, set before any thread is spawned
    unsafe {
        std::env::set_var("ATLAS_HOME", home.path());
        std::env::set_var("ATLAS_NO_KEYRING", "1");
    }
    db::create_db().unwrap();
    let main = home.path().join("atlas.db");
    let obfuscated = |name: &str, display: Option<&str>| atlas::parser::Release {
        is_obfuscated: true,
        display_name: display.map(String::from),
        ..release_of(name, "a.rar", 2, 2)
    };
    store::save(
        &main,
        &[
            obfuscated("kP93xQ7rL.ZMtwBfa", None),
            release_of("Good.Name.2024", "good.mkv", 2, 2),
            obfuscated("hQ72vB9nK.XWdrLpz", Some("Inline.Name.2020")),
            obfuscated("nQzw7RtKjd.XWdrLpa", None),
        ],
    )
    .unwrap();
    let shard = db::open_shard(&store::shard_path(&main, store::shard_of(STAGE_GROUP))).unwrap();
    let id: i64 =
        shard.query_row("select id from releases where name = 'kP93xQ7rL.ZMtwBfa'", [], |r| r.get(0)).unwrap();

    // a flagged release with a recovered name is already off the menu
    let listed = |page: i64| -> Vec<String> {
        search::search_obfuscated(page, 100).unwrap().into_iter().map(|r| r.name).collect()
    };
    assert_eq!(listed(0).len(), 2, "{:?}", listed(0));
    assert_eq!(search::count_obfuscated().unwrap(), 2);

    let nzb_before = nzb::build_nzb(id).unwrap().unwrap();

    // what Part 3's apply does for a step's name
    shard
        .execute(
            "update releases set display_name = ?, is_obfuscated = 0, name_source = 15 where id = ?",
            rusqlite::params![NEW_NAME, id],
        )
        .unwrap();

    let found = search::search_all_releases("Real.Movie", 0, 10).unwrap();
    assert_eq!(found.iter().map(|r| r.id).collect::<Vec<_>>(), vec![id]);
    assert_eq!(found[0].name, NEW_NAME);
    assert_eq!(search::search_all_releases("kP93xQ7rL", 0, 10).unwrap().len(), 1, "name stays");

    let all = search::all_releases(0, 100).unwrap();
    assert!(all.iter().any(|r| r.name == NEW_NAME), "{all:?}");
    assert_eq!(search::get_release(id).unwrap().unwrap().name, NEW_NAME);

    let r = api::handle("/api", &format!("t=get&id={id}&apikey=k"), "http://atlas:9090", "k");
    let disposition = &r.headers.iter().find(|(k, _)| k == "Content-Disposition").unwrap().1;
    assert!(disposition.contains(&format!("{NEW_NAME}.{id}.nzb")), "{disposition}");
    assert_eq!(r.body, nzb_before, "the files are the same files");

    assert_eq!(search::count_obfuscated().unwrap(), 1);
    assert!(!listed(0).iter().any(|n| n == NEW_NAME));

    // flagged and unnamed stays out of search, and on the menu
    assert!(search::search_all_releases("nQzw7RtKjd", 0, 10).unwrap().is_empty());
    assert!(listed(0).iter().any(|n| n == "nQzw7RtKjd.XWdrLpa"));
}
