//! The retained pure PreDB decoder remains independent of HTTP polling.
//! The retired bulk-import command fixtures are recorded in the retirement ledger.

#[test]
fn the_import_path_has_no_http() {
    let source = include_str!("../src/predb/file_import.rs");
    assert!(!source.contains("ureq") && !source.contains("poll::"));
}
