//! The `cat` parameter, end to end.
//!
//! One test: it points ATLAS_HOME at a temp dir before any thread is spawned.

mod common;

use atlas::parser::Release;
use atlas::{api, db, store};
use common::stage::release_of;

const K: &str = "k";

fn release(name: &str, group: &str) -> Release {
    Release { group: group.into(), ..release_of(name, "x.bin", 2, 2) }
}

fn titles(body: &str) -> Vec<String> {
    body.split("<title>").skip(2).filter_map(|t| t.split("</title>").next()).map(String::from).collect()
}

fn query(q: &str) -> String {
    let r = api::handle("/api", &format!("{q}&apikey={K}"), "http://atlas:9090", K);
    assert_eq!(r.status, 200, "{q}: {}", r.body);
    r.body
}

#[test]
fn the_cat_filter_end_to_end() {
    let home = tempfile::tempdir().unwrap();
    // SAFETY: only test in this binary, set before any thread is spawned
    unsafe {
        std::env::set_var("ATLAS_HOME", home.path());
        std::env::set_var("ATLAS_NO_KEYRING", "1");
    }
    db::create_db().unwrap();
    let main = home.path().join("atlas.db");
    let rows = [
        ("Movie.Name.2019-GRP", "alt.binaries.g0", Some(2040)),
        ("Tv.Hd.Findword.S01E01-GRP", "alt.binaries.g1", Some(5040)),
        ("Tv.Sd.Name.S01E02-GRP", "alt.binaries.g2", Some(5030)),
        ("Book.Name.2020-GRP", "alt.binaries.g3", Some(7020)),
        ("No.Row.Name.2021-GRP", "alt.binaries.g4", None),
        ("abcdefghij0123456789", "alt.binaries.g5", None),
        ("Other.Name.2022-GRP", "alt.binaries.g6", Some(8010)),
    ];
    for (name, group, category) in rows {
        store::save(&main, &[release(name, group)]).unwrap();
        let shard = db::open_shard(&store::shard_path(&main, store::shard_of(group))).unwrap();
        let id: i64 = shard.query_row("select id from releases where name = ?", [name], |r| r.get(0)).unwrap();
        if let Some(category) = category {
            shard
                .execute(
                    "insert into release_category (release_id, category) values (?, ?)",
                    rusqlite::params![id, category],
                )
                .unwrap();
        }
    }

    let all = query("t=search");
    let visible = ["Other", "No.Row", "Book", "Tv.Sd", "Tv.Hd", "Movie"];
    let got = titles(&all);
    assert_eq!(got.len(), visible.len(), "{got:?}");
    for (title, start) in got.iter().zip(visible) {
        assert!(title.starts_with(start), "{got:?}");
    }

    let one = |q: &str| titles(&query(q));
    let movie = query("t=search&cat=2000");
    assert_eq!(titles(&movie), ["Movie.Name.2019-GRP"]);
    assert!(movie.contains(r#"name="category" value="2040""#));
    assert_eq!(one("t=search&cat=5040"), ["Tv.Hd.Findword.S01E01-GRP"]);
    assert_eq!(one("t=search&cat=5000"), ["Tv.Sd.Name.S01E02-GRP", "Tv.Hd.Findword.S01E01-GRP"]);
    assert_eq!(one("t=search&cat=5030,5040"), ["Tv.Sd.Name.S01E02-GRP", "Tv.Hd.Findword.S01E01-GRP"]);
    assert_eq!(one("t=tvsearch&cat=2040"), ["Movie.Name.2019-GRP"], "cat decides, not t");
    let none = query("t=search&cat=9999");
    assert!(titles(&none).is_empty());
    assert!(none.contains(r#"<newznab:response offset="0" total="0"/>"#));
    assert_eq!(one("t=search&cat=abc"), got);
    assert_eq!(one("t=search&cat=-1"), got);
    let book = query("t=search&cat=7000");
    assert_eq!(titles(&book), ["Book.Name.2020-GRP"]);
    assert!(book.contains(r#"name="category" value="7020""#));
    assert_eq!(one("t=search&cat=7020"), ["Book.Name.2020-GRP"]);
    let other = query("t=search&cat=8000");
    assert_eq!(titles(&other), ["Other.Name.2022-GRP", "No.Row.Name.2021-GRP"]);
    assert!(other.contains(r#"name="category" value="8000""#));
    assert_eq!(one("t=search&cat=8010"), ["Other.Name.2022-GRP"]);
    assert_eq!(one("t=search&q=Findword&cat=5040"), ["Tv.Hd.Findword.S01E01-GRP"]);
    assert!(one("t=search&q=Findword&cat=7000").is_empty());
    assert_eq!(one("t=search&cat=5000&limit=1&offset=1"), ["Tv.Hd.Findword.S01E01-GRP"]);
    let newest = query("t=search&limit=1");
    assert_eq!(titles(&newest), ["Other.Name.2022-GRP"]);
    assert!(newest.contains(r#"name="category" value="8010""#));
}
