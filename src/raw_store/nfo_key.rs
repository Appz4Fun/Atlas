//! Reversible PostgreSQL spelling of every legacy NFO natural name.
//!
//! Legacy nested-NZB names contain a zero byte. Encoding every name, including
//! names that already start with this prefix, keeps the namespace injective.

pub(crate) const PREFIX: &str = "atlas.nfo-name.v1:";

pub(crate) fn encode(name: &str) -> String {
    use std::fmt::Write;
    let mut encoded = String::with_capacity(PREFIX.len() + name.len() * 2);
    encoded.push_str(PREFIX);
    for byte in name.as_bytes() {
        write!(&mut encoded, "{byte:02x}").expect("writing a String");
    }
    encoded
}

pub(crate) fn decode(encoded: &str) -> anyhow::Result<String> {
    let hex = encoded.strip_prefix(PREFIX).ok_or_else(|| anyhow::anyhow!("unknown NFO natural-name encoding"))?;
    anyhow::ensure!(hex.len() % 2 == 0, "invalid NFO natural-name encoding");
    let mut bytes = Vec::with_capacity(hex.len() / 2);
    for pair in hex.as_bytes().chunks_exact(2) {
        let digit = |b: u8| match b { b'0'..=b'9' => Some(b-b'0'), b'a'..=b'f' => Some(b-b'a'+10), _ => None };
        let high = digit(pair[0]).ok_or_else(|| anyhow::anyhow!("invalid NFO natural-name encoding"))?;
        let low = digit(pair[1]).ok_or_else(|| anyhow::anyhow!("invalid NFO natural-name encoding"))?;
        bytes.push(high * 16 + low);
    }
    Ok(String::from_utf8(bytes)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_names_roundtrip_without_prefix_case_unicode_or_zero_byte_collisions() {
        let names = ["", "Kept", "kept", "Kept\0nzb", "atlas.nfo-name.v1:4b657074", "é\0nzb"];
        let encoded: std::collections::BTreeSet<_> = names.iter().map(|name| encode(name)).collect();
        assert_eq!(encoded.len(), names.len());
        for name in names {
            let encoded = encode(name);
            assert!(!encoded.contains('\0'));
            assert_eq!(decode(&encoded).unwrap(), name);
        }
        assert_eq!(encode("Kept\0nzb"), "atlas.nfo-name.v1:4b657074006e7a62");
    }

    #[test]
    fn malformed_or_unversioned_names_are_refused() {
        for name in ["Kept", "atlas.nfo-name.v1:0", "atlas.nfo-name.v1:FF", "atlas.nfo-name.v1:ff", "atlas.nfo-name.v2:00"] {
            assert!(decode(name).is_err());
        }
    }
}
