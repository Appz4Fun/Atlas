//! Stable native group opt-in metadata key.

pub fn rename_named_key(group: &str) -> String {
    format!("naming_rename_named:{group}")
}
