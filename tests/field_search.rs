//! Searching on group, poster, email, size and release group, with and without the indexes.
//!
//! One test: it points ATLAS_HOME at a temp dir before any thread is spawned.

mod common;

use atlas::field_search::{Fields, count, find};
use atlas::parser::Release;
use atlas::release_info::groups_of;
use atlas::{db, store};
use common::stage::release_of;

#[test]
fn every_field_finds_its_releases_and_counts_them() {
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
    let save = |name: &str, group: &str, poster: &str| {
        store::save(
            &main,
            &[Release { group: group.into(), poster: poster.into(), ..release_of(name, "x.bin", 3, 3) }],
        )
        .unwrap();
    };
    let (a, b) = (in_shard(0), in_shard(1));
    save("Film.One.2024.1080p.WEB-DL-AAA", &a, "Sam <sam@example.org>");
    save("Film.Two.2024.1080p.WEB-DL-AAA", &b, "Sam <sam@example.org>");
    save("Film.Three.2024.1080p.WEB-DL-BBB", &a, "Pat <pat@example.net>");
    save("Film.One.2024.1080p.WEB-DL-AAA", &b, "Sam <sam@example.org>");

    let names = |fields: &Fields, words: &str| {
        let mut found: Vec<String> = find(fields, words, None, 0, 50)
            .unwrap()
            .into_iter()
            .map(|r| format!("{}@{}", r.name, r.group_name))
            .collect();
        found.sort();
        found
    };
    let check = |exact: bool| {
        let email = Fields { email: "sam@example.org".into(), ..Fields::default() };
        assert_eq!(names(&email, "").len(), 3);
        let total = count(&email, "", None).unwrap();
        assert_eq!((total.count, total.exact), (3, exact));

        let poster = Fields { poster: "Pat <pat@example.net>".into(), ..Fields::default() };
        assert_eq!(names(&poster, ""), [format!("Film.Three.2024.1080p.WEB-DL-BBB@{a}")]);

        let group = Fields { group: a.clone(), ..Fields::default() };
        assert_eq!(names(&group, "").len(), 2);
        assert!(count(&group, "", None).unwrap().exact, "the group has its own index");
        assert_eq!(names(&group, "Three").len(), 1, "with words too");

        let team = Fields { team: "BBB".into(), ..Fields::default() };
        assert_eq!(names(&team, "").len(), 1);
        let part = Fields { team: "BB".into(), ..Fields::default() };
        assert!(names(&part, "").is_empty(), "the whole group, not the start of one");

        // each counted once, whichever spellings it matches
        let group = Fields { group: a.clone(), ..Fields::default() };
        let any = |words: &[&str]| {
            let choices: Vec<String> = words.iter().map(|w| w.to_string()).collect();
            atlas::field_search::count_any(&group, &choices, None).unwrap().count
        };
        assert_eq!(any(&["One", "Three"]), 2);
        assert_eq!(any(&["One", "Film.One"]), 1);
        assert_eq!(any(&["Nothing"]), 0);

        let size = find(&group, "", None, 0, 1).unwrap()[0].size.unwrap();
        let bytes = Fields { size: size.to_string(), ..Fields::default() };
        assert_eq!(names(&bytes, "").len(), 4);
        let none = Fields { size: "12345".into(), ..Fields::default() };
        assert!(names(&none, "").is_empty());
    };
    check(false);
    assert_eq!(atlas::field_search::run(&["--apply".to_string()]), 0);
    check(true);

    let postings = groups_of("Film.One.2024.1080p.WEB-DL-AAA").unwrap();
    assert_eq!(postings.len(), 2, "the same name in two groups");
    assert!(postings.iter().all(|p| p.articles == 3), "{postings:?}");
}
