//! Compiling is per group. Its own binary: `COMPILED` counts for the whole process.

use std::sync::atomic::Ordering;

use atlas::naming::regexes::*;

#[test]
fn rules_compile_lazily_by_group() {
    assert!(names("alt.binaries.no.rules", "x").is_empty());
    assert!(group_has_rules("alt.binaries.teevee"));
    assert_eq!(COMPILED.load(Ordering::Relaxed), 0);
    names("alt.binaries.teevee", "x");
    assert_eq!(COMPILED.load(Ordering::Relaxed), 89);
    names("alt.binaries.teevee", "x");
    assert_eq!(COMPILED.load(Ordering::Relaxed), 89);
}
