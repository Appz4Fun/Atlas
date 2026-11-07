//! Issue #232, end to end: the Newznab API lists a release only when it's whole, unless `incomplete=1` asks for
//! the others too, and then each of those says it isn't.
//!
//! One test: it points ATLAS_HOME at a temp dir before any thread is spawned.

use atlas::parser::{Article, Release, group_articles};
use atlas::{api, db, store};

const K: &str = "k";
const GROUP: &str = "alt.binaries.t";

fn titles(body: &str) -> Vec<String> {
    body.split("<title>").skip(2).filter_map(|t| t.split("</title>").next()).map(String::from).collect()
}

fn query(q: &str) -> String {
    let r = api::handle("/api", &format!("{q}&apikey={K}"), "http://atlas:9090", K);
    assert_eq!(r.status, 200, "{q}: {}", r.body);
    r.body
}

/// Saves a post of these subjects, an article each, as the indexer would group them.
fn post(main: &std::path::Path, tag: &str, subjects: &[&str]) {
    let articles = subjects
        .iter()
        .enumerate()
        .map(|(i, s)| Article {
            message_id: format!("<{tag}{i}@x>"),
            subject: (*s).into(),
            bytes: 100,
            ..Default::default()
        })
        .collect();
    let releases: Vec<Release> =
        group_articles(articles).into_values().map(|r| Release { group: GROUP.into(), ..r }).collect();
    assert_eq!(releases.len(), 1, "{subjects:?}");
    store::save(main, &releases).unwrap();
}

#[test]
fn incomplete_releases_are_left_out_of_the_api_unless_asked_for() {
    let home = tempfile::tempdir().unwrap();
    // SAFETY: only test in this binary, set before any thread is spawned
    unsafe {
        std::env::set_var("ATLAS_HOME", home.path());
        std::env::set_var("ATLAS_NO_KEYRING", "1");
    }
    db::create_db().unwrap();
    let main = home.path().join("atlas.db");

    // saved first, soo the newest rows a page reads are the incomplete ones
    post(
        &main,
        "w",
        &[
            "Whole.Film.2020-GRP [1/2] - \"Whole.Film.2020-GRP.rar\" yEnc (1/2)",
            "Whole.Film.2020-GRP [1/2] - \"Whole.Film.2020-GRP.rar\" yEnc (2/2)",
            "Whole.Film.2020-GRP [2/2] - \"Whole.Film.2020-GRP.par2\" yEnc (1/1)",
        ],
    );
    // every file it has is whole, but its counter names 123 and it has 2: one CD of a two CD post
    post(
        &main,
        "c",
        &[
            "Some.Show.cd2-GRP [061/123] - \"Some.Show.cd2-GRP.par2\" yEnc (1/1)",
            "Some.Show.cd2-GRP [062/123] - \"Some.Show.cd2-GRP.r00\" yEnc (1/1)",
        ],
    );
    // one article of file 17 of 58
    post(&main, "o", &["[17/58] - \"Bn3IO.8fNJB\" yEnc (27/491) 261431296"]);
    // all its files, one of them short of parts
    post(
        &main,
        "p",
        &[
            "Gappy.Film.2021-GRP [1/2] - \"Gappy.Film.2021-GRP.rar\" yEnc (1/3)",
            "Gappy.Film.2021-GRP [1/2] - \"Gappy.Film.2021-GRP.rar\" yEnc (3/3)",
            "Gappy.Film.2021-GRP [2/2] - \"Gappy.Film.2021-GRP.par2\" yEnc (1/1)",
        ],
    );

    let listed = query("t=search");
    assert_eq!(titles(&listed), ["Whole.Film.2020-GRP"], "{listed}");
    assert!(!listed.contains(r#"name="complete""#), "{listed}");
    assert!(listed.contains(r#"<newznab:response offset="0" total="1"/>"#), "{listed}");

    // a page of one reads on past the incomplete rows to the whole one
    assert_eq!(titles(&query("t=search&limit=1")), ["Whole.Film.2020-GRP"]);
    assert_eq!(titles(&query("t=search&q=Film")), ["Whole.Film.2020-GRP"]);
    assert!(titles(&query("t=search&q=Gappy")).is_empty());

    // asked for, the others are listed too, each saying it isn't whole
    let all = query("t=search&incomplete=1");
    let got = titles(&all);
    assert_eq!(got.len(), 4, "{all}");
    assert_eq!(all.matches(r#"<newznab:attr name="complete" value="0"/>"#).count(), 3, "{all}");
    for item in all.split("<item>").skip(1) {
        let whole = item.contains("<title>Whole.Film.2020-GRP</title>");
        assert_eq!(item.contains(r#"name="complete" value="0""#), !whole, "{item}");
    }
    assert_eq!(titles(&query("t=search&q=Gappy&incomplete=1")), ["Gappy.Film.2021-GRP"]);
}
