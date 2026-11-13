//! Pure classifier vectors; retained mixed native oracle is privately registered in native_workers.rs.

#[test]
fn current_classifier_vectors_survive_unname_retirement() {
    assert!(atlas::parser::is_obfuscated("kP93xQ7rL.ZMtwBfa"));
    let identity = atlas::naming::without_collision_suffix("Xk3Mz9PqL [12345678]");
    assert_eq!(identity, "Xk3Mz9PqL");
    assert!(atlas::parser::is_obfuscated(identity));
    assert!(!atlas::parser::is_obfuscated("Good.Release.2024"));
}
