//! Inner file names from ZIP local headers and the central directory.
#![cfg_attr(not(test), deny(clippy::indexing_slicing, clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use super::{Encryption, Listing};

const LOCAL: &[u8] = b"PK\x03\x04";
const CENTRAL: &[u8] = b"PK\x01\x02";
const END: &[u8] = b"PK\x05\x06";
const SATURATED: u32 = u32::MAX;
/// an end record and the longest comment
const END_SEARCH: usize = 65_557;

fn le16(b: &[u8], at: usize) -> Option<usize> {
    Some(usize::from(u16::from_le_bytes(b.get(at..at.checked_add(2)?)?.try_into().ok()?)))
}

fn le32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at.checked_add(4)?)?.try_into().ok()?))
}

fn le64(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(at..at.checked_add(8)?)?.try_into().ok()?))
}

/// UTF-8 when flagged or valid, otherwise Latin-1
fn name_of(raw: &[u8], flags: usize) -> String {
    if flags & 0x0800 != 0 {
        return String::from_utf8_lossy(raw).into_owned();
    }
    match std::str::from_utf8(raw) {
        Ok(s) => s.to_string(),
        Err(_) => raw.iter().map(|b| char::from(*b)).collect(),
    }
}

fn is_encrypted(flags: usize, method: usize) -> bool {
    flags & (0x0001 | 0x0040 | 0x2000) != 0 || method == 99
}

fn zip_encryption(method: usize) -> Encryption {
    if method == 99 { Encryption::ZipAes } else { Encryption::ZipCrypto }
}

fn is_directory(name: &str) -> bool {
    name.ends_with('/') || name.ends_with('\\')
}

/// the ZIP64 extra's sizes where a 32-bit field is saturated: uncompressed, compressed
fn zip64_sizes(extra: &[u8], uncompressed: u32, compressed: u32) -> (u64, u64) {
    let (mut unp, mut packed) = (u64::from(uncompressed), u64::from(compressed));
    let mut p = 0usize;
    while let (Some(id), Some(size)) = (le16(extra, p), le16(extra, p + 2)) {
        let Some(data) = p.checked_add(4).and_then(|at| extra.get(at..at.checked_add(size)?)) else { break };
        if id == 1 {
            let mut at = 0;
            if uncompressed == SATURATED {
                if let Some(v) = le64(data, at) {
                    unp = v;
                }
                at += 8;
            }
            if compressed == SATURATED
                && let Some(v) = le64(data, at)
            {
                packed = v;
            }
            break;
        }
        p += 4 + size;
    }
    (unp, packed)
}

/// The entries at the start of an archive. `complete` stays false: only the
/// directory proves the whole list.
pub fn list_head(data: &[u8]) -> Listing {
    let mut l = Listing::default();
    let mut pos = if data.starts_with(b"PK\x07\x08") || data.starts_with(b"PK00") { 4usize } else { 0 };
    while let Some(entry) = data.get(pos..) {
        let Some(sig) = entry.get(..4) else { return l };
        if sig == CENTRAL || sig == END {
            return l;
        }
        if sig != LOCAL {
            l.damaged = true;
            return l;
        }
        let (Some(flags), Some(method), Some(unp), Some(packed), Some(n), Some(e)) =
            (le16(entry, 6), le16(entry, 8), le32(entry, 22), le32(entry, 18), le16(entry, 26), le16(entry, 28))
        else {
            return l;
        };
        let Some(name) = entry.get(30..30 + n) else { return l };
        let extra = entry.get(30 + n..30 + n + e).unwrap_or(&[]);
        if is_encrypted(flags, method) {
            l.encrypted = true;
            l.encryption = Some(zip_encryption(method));
        }
        let (unp, packed) = zip64_sizes(extra, unp, packed);
        let name = name_of(name, flags);
        if !is_directory(&name) && !l.push(&name, Some(unp)) {
            return l;
        }
        if l.files.len() >= super::MAX_ENTRIES {
            return l;
        }
        // sizes come after the data, soo where the next entry is isnt known
        if flags & 0x0008 != 0 && packed == 0 {
            return l;
        }
        let next = (pos as u64).saturating_add((30 + n + e) as u64).saturating_add(packed);
        match usize::try_from(next).ok().filter(|at| *at <= data.len()) {
            Some(at) => pos = at,
            None => return l,
        }
    }
    l
}

/// The central directory in an archive's last bytes.
pub fn list_tail(tail: &[u8]) -> Listing {
    let mut l = Listing::default();
    let from = tail.len().saturating_sub(END_SEARCH);
    let found = tail.get(from..).and_then(|window| {
        window.windows(4).enumerate().rev().find_map(|(i, w)| {
            let at = from + i;
            // a record whose comment reaches the end exactly: one inside a comment doesnt
            (w == END && le16(tail, at + 20).is_some_and(|c| at + 22 + c == tail.len())).then_some(at)
        })
    });
    let Some(eocd) = found else {
        l.damaged = true;
        return l;
    };
    let (Some(entries), Some(size)) = (le16(tail, eocd + 10), le32(tail, eocd + 12)) else {
        l.damaged = true;
        return l;
    };
    let (mut entries, mut size, mut end) = (entries as u64, u64::from(size), eocd);
    if entries == 0xffff || size == u64::from(SATURATED) {
        let record = eocd.checked_sub(76);
        let z64 = record.filter(|r| {
            tail.get(*r..*r + 4) == Some(b"PK\x06\x06") && tail.get(eocd - 20..eocd - 16) == Some(b"PK\x06\x07")
        });
        if let Some(r) = z64
            && let (Some(n), Some(s)) = (le64(tail, r + 32), le64(tail, r + 40))
        {
            entries = n;
            size = s;
            end = r;
        }
    }
    // relative to the tail: absolute offsets mean nothing in one
    let start = usize::try_from(size).ok().and_then(|s| end.checked_sub(s));
    let (mut pos, whole) = match start {
        Some(s) => (s, true),
        None => match tail.get(..end).and_then(|d| d.windows(4).position(|w| w == CENTRAL)) {
            Some(at) => (at, false),
            None => return l,
        },
    };
    let mut seen = 0u64;
    while pos < end {
        let Some(entry) = tail.get(pos..end) else { return l };
        if entry.get(..4) != Some(CENTRAL) {
            return l;
        }
        let (Some(flags), Some(method), Some(packed), Some(unp), Some(n), Some(e), Some(k)) = (
            le16(entry, 8),
            le16(entry, 10),
            le32(entry, 20),
            le32(entry, 24),
            le16(entry, 28),
            le16(entry, 30),
            le16(entry, 32),
        ) else {
            return l;
        };
        let Some(name) = entry.get(46..46 + n) else { return l };
        let Some(extra) = entry.get(46 + n..46 + n + e) else { return l };
        if entry.get(46 + n + e..46 + n + e + k).is_none() {
            return l;
        }
        if is_encrypted(flags, method) {
            l.encrypted = true;
            l.encryption = Some(zip_encryption(method));
        }
        let (unp, _) = zip64_sizes(extra, unp, packed);
        let name = name_of(name, flags);
        if !is_directory(&name) && !l.push(&name, Some(unp)) {
            return l;
        }
        if l.files.len() >= super::MAX_ENTRIES {
            return l;
        }
        seen += 1;
        pos += 46 + n + e + k;
    }
    l.complete = whole && seen == entries;
    l
}

/// The tail's read if it has files, keeping `encrypted` from either; otherwise the head's.
pub fn list(head: &[u8], tail: &[u8]) -> Listing {
    let from_head = list_head(head);
    let mut from_tail = list_tail(tail);
    if from_tail.files.is_empty() {
        return from_head;
    }
    from_tail.encrypted |= from_head.encrypted;
    from_tail
}
