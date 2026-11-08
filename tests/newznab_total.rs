//! Newznab's `total` is how many match in all, the same on every page: Sonarr and Radarr page by it.
//!
//! One test: it points ATLAS_HOME at a temp dir before any thread is spawned.

mod common;

use atlas::{api, db, store};
use common::stage::release_of;

const K: &str = "k";

fn response(q: &str) -> (String, usize) {
    let r = api::handle("/api", &format!("{q}&apikey={K}"), "http://atlas:9090", K);
    assert_eq!(r.status, 200, "{q}: {}", r.body);
    let line = r.body.lines().find(|l| l.contains("<newznab:response")).unwrap().trim().to_string();
    (line, r.body.matches("<item>").count())
}

#[test]
fn total_is_every_match_on_every_page() {
    let home = tempfile::tempdir().unwrap();
    // SAFETY: only test in this binary, set before any thread is spawned
    unsafe {
        std::env::set_var("ATLAS_HOME", home.path());
        std::env::set_var("ATLAS_NO_KEYRING", "1");
    }
    db::create_db().unwrap();
    let main = home.path().join("atlas.db");
    for i in 0..7 {
        store::save(&main, &[release_of(&format!("Paged.Show.S01E0{i}-GRP"), "x.bin", 2, 2)]).unwrap();
    }

    let (first, n) = response("t=search&q=Paged.Show&limit=3&offset=0");
    assert_eq!(n, 3);
    assert_eq!(first, r#"<newznab:response offset="0" total="7"/>"#);
    let (second, n) = response("t=search&q=Paged.Show&limit=3&offset=3");
    assert_eq!(n, 3);
    assert_eq!(second, r#"<newznab:response offset="3" total="7"/>"#);
    let (last, n) = response("t=search&q=Paged.Show&limit=3&offset=6");
    assert_eq!(n, 1);
    assert_eq!(last, r#"<newznab:response offset="6" total="7"/>"#);
}
