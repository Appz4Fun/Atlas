//! `atlas --compact` notes the time like the indexer's own compaction does,
//! soo auto compaction doesnt run again right after it.

use std::process::Command;

#[test]
fn a_successful_compact_command_records_when_it_ran() {
    let home = tempfile::tempdir().unwrap();
    let main = home.path().join("atlas.db");
    atlas::db::create_db_at(&main).unwrap();
    let last = || {
        let conn = atlas::db::open_at(&main).unwrap();
        atlas::store::get_meta(&conn, "last_compact").unwrap()
    };
    assert_eq!(last(), None);

    let before = chrono::Utc::now().timestamp();
    let out = Command::new(env!("CARGO_BIN_EXE_atlas"))
        .arg("--compact")
        .env("ATLAS_HOME", home.path())
        .env("ATLAS_NO_KEYRING", "1")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));
    let at = last().expect("last_compact is set");
    assert!(at >= before && at <= chrono::Utc::now().timestamp(), "{at}");
}
