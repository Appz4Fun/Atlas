//! Pure classifier vectors; retained mixed native oracle is privately registered in native_workers.rs.

#[test]
fn current_classifier_vectors_survive_reflag_retirement() {
    for name in ["a1B2c3D4e5F6g7H8", "kP93xQ7rL.ZMtwBfa", "Xk3Mz9PqL"] {
        assert!(atlas::parser::is_obfuscated(name), "{name}");
    }
    let identity = atlas::naming::without_collision_suffix("hQ72vB9nK [12345678]");
    assert_eq!(identity, "hQ72vB9nK");
    assert!(atlas::parser::is_obfuscated(identity));
    assert!(!atlas::parser::is_obfuscated("Good.Name.2024"));
}
