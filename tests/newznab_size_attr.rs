//! Every Newznab item carries `<newznab:attr name="size">`: Sonarr and Radarr read the size from it first.
//!
//! One test: it points ATLAS_HOME at a temp dir before any thread is spawned.

mod common;

use atlas::{api, db, store};
use common::stage::release_of;

const K: &str = "k";

fn query(q: &str) -> String {
    let r = api::handle("/api", &format!("{q}&apikey={K}"), "http://atlas:9090", K);
    assert_eq!(r.status, 200, "{q}: {}", r.body);
    r.body
}

#[test]
fn every_item_has_a_size_attr() {
    let home = tempfile::tempdir().unwrap();
    // SAFETY: only test in this binary, set before any thread is spawned
    unsafe {
        std::env::set_var("ATLAS_HOME", home.path());
        std::env::set_var("ATLAS_NO_KEYRING", "1");
    }
    db::create_db().unwrap();
    let main = home.path().join("atlas.db");
    store::save(&main, &[release_of("Size.Attr.Show.S01E01-GRP", "x.bin", 2, 2)]).unwrap();

    for q in ["t=search&q=Size.Attr", "t=tvsearch&q=Size.Attr", "t=movie&q=Size.Attr"] {
        let body = query(q);
        let items: Vec<&str> = body.split("<item>").skip(1).collect();
        assert!(!items.is_empty(), "{q}: no items in {body}");
        for item in items {
            let size = item.split("<size>").nth(1).and_then(|s| s.split("</size>").next()).unwrap();
            assert!(
                item.contains(&format!("<newznab:attr name=\"size\" value=\"{size}\"/>")),
                "{q}: no size attr matching <size>{size}</size> in {item}"
            );
        }
    }
}
