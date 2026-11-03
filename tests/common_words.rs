//! Searching for a word that matches a lot: newest first, without reading every match.
//!
//! One test: it points ATLAS_HOME at a temp dir before any thread is spawned.

mod common;

use atlas::parser::Release;
use atlas::search::search_past;
use atlas::{db, store};
use common::stage::release_of;

#[test]
fn a_common_word_comes_back_newest_first() {
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
    let group = in_shard(0);
    for (name, group) in [
        ("Film.One.1080p-AAA", in_shard(0)),
        ("Film.Two.1080p-AAA", in_shard(1)),
        ("Film.Three.1080p-AAA", in_shard(2)),
        ("Film.Four.1080pish-AAA", in_shard(3)),
    ] {
        store::save(&main, &[Release { group, ..release_of(name, "x.bin", 2, 2) }]).unwrap();
    }
    let names = |query: &str, group: Option<&str>, page, common| -> Vec<String> {
        search_past(query, group, page, 2, common).unwrap().into_iter().map(|r| r.name).collect()
    };

    assert_eq!(names("1080p", None, 0, 2), ["Film.Three.1080p-AAA", "Film.Two.1080p-AAA"], "newest first");
    assert_eq!(names("1080p", None, 1, 2), ["Film.One.1080p-AAA"], "and its next page");
    assert_eq!(names("1080p", Some(&group), 0, 1), ["Film.One.1080p-AAA"], "within a group");
    assert_eq!(names("film 1080p", None, 0, 2), ["Film.Three.1080p-AAA", "Film.Two.1080p-AAA"], "every word");

    // few enough: ranked, and a word's start still finds longer ones
    let all: Vec<String> = search_past("1080p", None, 0, 10, 100).unwrap().into_iter().map(|r| r.name).collect();
    assert_eq!(all.len(), 4, "{all:?}");
    assert!(all.iter().any(|name| name.contains("1080pish")), "{all:?}");
    assert!(names("nothinglikethis", None, 0, 2).is_empty());
}
