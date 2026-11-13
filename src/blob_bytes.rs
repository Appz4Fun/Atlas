//! Pure byte accounting for historical inline sealed blobs. No catalog connection,
//! background backfill, cursor writes, or implicit remote service reads live here.
//!
//! The retired six SQLite fixtures covered old shard blob_bytes migration,
//! uncounted-blob retries/windows and sealed-log backfill, not native catalog behavior.
//! Their exact source remains in the c62d070 legacy source archive and Git history.
//! Local segment accounting and every codec boundary remain covered below and in blob::tests.

use crate::blob::Seg;

/// The bytes of a blob's articles.
pub fn of_segs(segs: &[Seg]) -> i64 {
    segs.iter().map(|s| s.bytes).sum()
}

/// The bytes of a historical inline blob's articles. A remote locator requires explicit resolution.
pub fn of_blob(blob: &[u8]) -> std::io::Result<i64> {
    Ok(of_segs(&crate::blob::decode(blob)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn article_byte_counts_keep_exact_inline_and_shifted_segment_values() {
        let segments = vec![
            Seg { part: Some(1), bytes: 750_000, domain: 2, local: b"a".to_vec() },
            Seg { part: Some(1), bytes: 100, domain: 3, local: b"copy".to_vec() },
            Seg { part: None, bytes: -5, domain: 0, local: b"<whole@test>".to_vec() },
        ];
        assert_eq!(of_segs(&segments), 750_095);
        let inline = crate::blob::encode(&segments);
        assert_eq!(of_blob(&inline).unwrap(), 750_095);
        assert_eq!(of_blob(&crate::blob::shifted(&inline, 19)).unwrap(), 750_095);
        assert_eq!(of_blob(&crate::blob::encode(&[])).unwrap(), 0);
        assert!(of_blob(b"corrupt").is_err());
    }

    #[test]
    fn a_locator_cannot_trigger_legacy_network_fetch_during_byte_accounting() {
        let mut reference = sealed_log::REF_TAG.to_vec();
        reference.resize(sealed_log::REF_BYTES, 7);
        assert_eq!(of_blob(&reference).unwrap_err().kind(), std::io::ErrorKind::Unsupported);
        assert_eq!(of_blob(&crate::blob::shifted(&reference, 19)).unwrap_err().kind(), std::io::ErrorKind::Unsupported);
    }
}
