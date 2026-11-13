//! Pure sidecar limits and filename families used by native catalog rules.
//! The legacy SQLite freeze policy and backlog engine are retained in Git history.

/// What the sidecar rule takes at most, in bytes; and the file kinds it takes, as the rule spells them
pub const SIDECAR_BYTES: i64 = 4 << 20;
pub const SIDECAR_FILES: &[&str] = &["nfo", "txt", "sfv", "srr", "srs", "md5", "sha1", "diz"];
/// and the images it takes: beside those files, or one alone, a cover or poster posted on its own
pub const SIDECAR_IMAGES: &[&str] = &["jpg", "jpeg", "png", "gif", "webp"];
