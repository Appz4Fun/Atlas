//! What a title is looked up by. A key only nominates.
//!
//! Ported from newznab-tmux `app/Services/NameFixing/PredbIdentityKey.php`, `app/Services/Predb/PredbIdentityKeys.php`, GPLv3.

use std::fmt::Write;

use md5::{Digest, Md5};
use sha1::Sha1;

use crate::naming::identity_key;

fn key(d: &[u8]) -> i64 {
    i64::from_be_bytes(d[..8].try_into().unwrap())
}
fn hex(d: &[u8]) -> String {
    d.iter().fold(String::with_capacity(d.len() * 2), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}
/// lowercase hex of the exact title bytes
pub fn md5_hex(t: &str) -> String {
    hex(&Md5::digest(t.as_bytes()))
}
pub fn sha1_hex(t: &str) -> String {
    hex(&Sha1::digest(t.as_bytes()))
}
/// The lookup key of a name, whichever separators and case it uses.
pub fn name_key(name: &str) -> i64 {
    key(&Sha1::digest(identity_key(name).as_bytes()))
}
pub fn title_keys(title: &str) -> [i64; 3] {
    [name_key(title), key(&Md5::digest(title.as_bytes())), key(&Sha1::digest(title.as_bytes()))]
}
/// The lookup key of a name that is itself a 32 or 40 digit hex hash.
pub fn hash_key(name: &str) -> Option<i64> {
    if !matches!(name.len(), 32 | 40) || !name.bytes().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    u64::from_str_radix(&name[..16], 16).ok().map(|v| v as i64)
}
pub fn valid_title(t: &str) -> bool {
    let n = t.chars().count();
    (8..=255).contains(&n)
        && t == t.trim()
        && t.chars().any(char::is_alphabetic)
        && !t.chars().any(|c| c.is_control() || matches!(c, '\\' | '/' | '|' | '"'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_table() {
        assert_eq!(name_key("Foo.Bar-GRP"), name_key("foo bar-grp"));
        assert_eq!(name_key("Foo.Bar-GRP"), name_key("FOO_BAR-GRP"));
        assert_eq!(md5_hex("abc"), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(sha1_hex("abc"), "a9993e364706816aba3e25717850c26c9cd0d89d");
        let t = "Some.Title-GRP";
        let keys = title_keys(t);
        assert_eq!(hash_key(&md5_hex(t)), Some(keys[1]));
        assert_eq!(hash_key(&sha1_hex(t)), Some(keys[2]));
        assert_eq!(hash_key("zz"), None);
        assert_eq!(hash_key(&"a".repeat(31)), None);
        assert_eq!(hash_key(&"z".repeat(32)), None);
    }

    #[test]
    fn valid_title_table() {
        assert!(valid_title("Good.Title-GRP"));
        for t in [
            "short",
            "bad/slash.title-GRP",
            "quo\"te.title-GRP",
            "12345678",
            "con\u{1}trol.title",
            " padded.title-GRP ",
        ] {
            assert!(!valid_title(t), "{t}");
        }
        assert!(!valid_title(&"a".repeat(256)));
    }
}
