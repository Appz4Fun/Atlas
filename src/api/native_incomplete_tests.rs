//! Retained Issue #232 API filtering, pagination and incomplete-release attributes.
#[path = "../../tests/common/native.rs"]
mod native;

use crate::parser::{Article, Release, group_articles};
use crate::{api, nntp::BlockingPool};
use std::time::{Duration, Instant};

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
fn post(harness: &native::Harness, runtime: &tokio::runtime::Runtime, tag: &str, subjects: &[&str]) {
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
    runtime.block_on(harness.db().save(releases)).expect("commit original native fixture post");
}

#[test]
fn incomplete_releases_are_left_out_of_the_api_unless_asked_for() {
    assert!(std::env::var_os("ATLAS_NATIVE_PROVIDER_CASE").is_none(), "select the native API parent directly");
    if !cfg!(unix) || std::env::var("ATLAS_PG_TEST_URL").ok().filter(|url| !url.is_empty()).is_none() {
        eprintln!("native API completeness control requires Unix and an explicit ATLAS_PG_TEST_URL");
        return;
    }
    native::run_child_on("incomplete-api", "api::native_incomplete_tests::fixture_child");
}

#[test]
#[ignore = "internal exact native child; select the retained API parent oracle"]
fn fixture_child() {
    assert_eq!(std::env::var("ATLAS_NATIVE_PROVIDER_CASE").expect("select native API parent"), "incomplete-api");
    completeness();
    native::mark_finished("incomplete-api");
}

fn completeness() {
    let mut harness = native::Harness::open(Vec::new());
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("native save runtime");
    // saved first, soo the newest rows a page reads are the incomplete ones
    post(
        &harness,
        &runtime,
        "w",
        &[
            "Whole.Film.2020-GRP [1/2] - \"Whole.Film.2020-GRP.rar\" yEnc (1/2)",
            "Whole.Film.2020-GRP [1/2] - \"Whole.Film.2020-GRP.rar\" yEnc (2/2)",
            "Whole.Film.2020-GRP [2/2] - \"Whole.Film.2020-GRP.par2\" yEnc (1/1)",
        ],
    );
    // every file it has is whole, but its counter names 123 and it has 2: one CD of a two CD post
    post(
        &harness,
        &runtime,
        "c",
        &[
            "Some.Show.cd2-GRP [061/123] - \"Some.Show.cd2-GRP.par2\" yEnc (1/1)",
            "Some.Show.cd2-GRP [062/123] - \"Some.Show.cd2-GRP.r00\" yEnc (1/1)",
        ],
    );
    // one article of file 17 of 58
    post(&harness, &runtime, "o", &["[17/58] - \"Bn3IO.8fNJB\" yEnc (27/491) 261431296"]);
    // all its files, one of them short of parts
    post(
        &harness,
        &runtime,
        "p",
        &[
            "Gappy.Film.2021-GRP [1/2] - \"Gappy.Film.2021-GRP.rar\" yEnc (1/3)",
            "Gappy.Film.2021-GRP [1/2] - \"Gappy.Film.2021-GRP.rar\" yEnc (3/3)",
            "Gappy.Film.2021-GRP [2/2] - \"Gappy.Film.2021-GRP.par2\" yEnc (1/1)",
        ],
    );

    harness.start_words();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let ready =
            [("Whole", "Whole.Film.2020-GRP"), ("Gappy", "Gappy.Film.2021-GRP")].into_iter().all(|(query, name)| {
                harness
                    .with_reader(|view| view.api_query(query, None, None, 0, 10))
                    .is_ok_and(|page| page.rows.iter().any(|row| row.name == name))
            });
        if ready {
            break;
        }
        assert!(Instant::now() < deadline, "actual native word keeper did not publish the fixture posts");
        std::thread::sleep(Duration::from_millis(20));
    }

    let listed = query("t=search");
    assert_eq!(titles(&listed), ["Whole.Film.2020-GRP"], "{listed}");
    assert!(!listed.contains(r#"name="complete""#), "{listed}");
    assert!(listed.contains(r#"<newznab:response offset="0" total="1"/>"#), "{listed}");

    // a page of one reads on past the incomplete rows to the whole one
    assert_eq!(titles(&query("t=search&limit=1")), ["Whole.Film.2020-GRP"]);
    assert_eq!(titles(&query("t=search&q=Film")), ["Whole.Film.2020-GRP"]);
    assert!(titles(&query("t=search&q=Gappy")).is_empty());
    // The accepted deep request retains the same native completeness contract.
    assert_eq!(titles(&query("t=search&deep=1")), ["Whole.Film.2020-GRP"]);
    assert_eq!(titles(&query("t=search&deep=1&incomplete=1")).len(), 4);

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

    let indexer = harness.indexer_on(BlockingPool::new(&[]), "backfill");
    harness.finish(indexer, 0);
}
