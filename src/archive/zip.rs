//! Inner file names from ZIP local headers and the central directory.
#![cfg_attr(not(test), deny(clippy::indexing_slicing, clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use super::facts::{self, Twin};
use super::{Encryption, FileFacts, Listing, PasswordTest};

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
pub(crate) fn zip64_sizes(extra: &[u8], uncompressed: u32, compressed: u32) -> (u64, u64) {
    zipview::zip64_sizes(extra, uncompressed, compressed)
}

pub use zipview::PasswordCheck;

/// What a ZIP entry's extra fields say: the UTC mtime (0x5455 or NTFS 0x000a), the Unix owner's ids (0x7875), and
/// which extras there are, by id, for the packer.
#[derive(Default)]
struct Extras {
    utc: Option<i64>,
    owner: Option<facts::Private>,
    ids: Vec<u16>,
}

fn extras(extra: &[u8]) -> Extras {
    let mut out = Extras::default();
    let mut p = 0usize;
    while let (Some(id), Some(size)) = (le16(extra, p), le16(extra, p + 2)) {
        let Some(data) = extra.get(p + 4..p + 4 + size) else { break };
        let id = id as u16;
        if out.ids.len() < 8 && !out.ids.contains(&id) {
            out.ids.push(id);
        }
        match id {
            // "UT": a flags byte, then the mtime when flag 1 says so, signed seconds
            0x5455 if data.first().is_some_and(|f| f & 1 != 0) => {
                out.utc = out.utc.or_else(|| le32(data, 1).map(|t| i64::from(t as i32)));
            }
            // NTFS: 4 reserved bytes, then tagged attributes; tag 1 holds mtime, atime, ctime as FILETIMEs
            0x000a => {
                let mut at = 4usize;
                while let (Some(tag), Some(len)) = (le16(data, at), le16(data, at + 2)) {
                    if tag == 1 && len >= 8 {
                        out.utc = out.utc.or_else(|| le64(data, at + 4).and_then(facts::filetime_secs));
                    }
                    at += 4 + len;
                }
            }
            // Info-ZIP "ux": version 1, then the uid and gid, each a size byte and that many bytes
            0x7875 if data.first() == Some(&1) => {
                let id_at = |at: usize| -> Option<(u64, usize)> {
                    let n = usize::from(*data.get(at)?);
                    let bytes = data.get(at + 1..at + 1 + n).filter(|b| b.len() <= 8)?;
                    let v = bytes.iter().rev().fold(0u64, |acc, b| acc << 8 | u64::from(*b));
                    Some((v, at + 1 + n))
                };
                if let Some((uid, next)) = id_at(1) {
                    let gid = id_at(next).map(|(g, _)| g);
                    out.owner = facts::owner_text(None, None, Some(uid), gid);
                }
            }
            _ => {}
        }
        p += 4 + size;
    }
    out
}

/// A method's word in the packer.
fn method_word(method: usize) -> String {
    match method {
        0 => "store".into(),
        8 => "deflate".into(),
        9 => "deflate64".into(),
        12 => "bzip2".into(),
        14 => "lzma".into(),
        93 => "zstd".into(),
        95 => "xz".into(),
        99 => "aes".into(),
        m => format!("method {m}"),
    }
}

/// How an entry was packed: who made it (the central directory only says), the version needed, the method and the
/// extras it carries.
fn packer(made_by: Option<usize>, needs: Option<usize>, method: usize, ids: &[u16]) -> String {
    let mut out = String::from("ZIP");
    if let Some(v) = made_by {
        let (host, spec) = ((v >> 8) as u8, v & 0xff);
        out.push_str(&format!(" · made by {} {}.{}", facts::host_word(facts::zip_host(host)), spec / 10, spec % 10));
    }
    if let Some(v) = needs {
        out.push_str(&format!(" · needs {}.{}", (v & 0xff) / 10, (v & 0xff) % 10));
    }
    out.push_str(&format!(" · {}", method_word(method)));
    if !ids.is_empty() {
        let ids: Vec<String> = ids.iter().map(|id| format!("{id:04x}")).collect();
        out.push_str(&format!(" · extras {}", ids.join(",")));
    }
    out
}

/// One entry's facts into the listing: its CRC unless the header leaves it to a descriptor, its UTC time, and the
/// packer's zone from its DOS time and that UTC time; the owner, the packer and an encryption header's random bytes
/// from the first entry that has them. `dos`: the DOS time and date words.
#[allow(clippy::too_many_arguments)]
fn note(
    l: &mut Listing,
    flags: usize,
    method: usize,
    crc: Option<u32>,
    dos: (Option<usize>, Option<usize>),
    extra: &[u8],
    made_by: Option<usize>,
    needs: Option<usize>,
) -> FileFacts {
    let x = extras(extra);
    let wall = dos.0.zip(dos.1).and_then(|(t, d)| facts::dos_wall_secs(t as u16, d as u16));
    match (wall, x.utc) {
        (Some(w), Some(u)) => l.zone = l.zone.or_else(|| facts::zone_of_pair(w, u)),
        (Some(w), None) => l.saw_local(Some(w)),
        _ => {}
    }
    if l.owner.is_none() {
        l.owner = x.owner;
    }
    if l.packer.is_none() {
        l.packer = Some(packer(made_by, needs, method, &x.ids));
    }
    // AE-2 writes no CRC, and a descriptor's CRC comes after the data
    let crc = crc.filter(|c| flags & 0x0008 == 0 && !(method == 99 && *c == 0));
    let host_os = made_by.map(|v| facts::zip_host((v >> 8) as u8));
    FileFacts { crc32: crc, mtime: x.utc, host_os }
}

/// The random bytes an encrypted entry starts with: ZipCrypto's 12-byte header or WinZip AES's salt.
fn twin_of(check: &PasswordCheck) -> Twin {
    match check {
        PasswordCheck::Crypto { header, .. } => Twin::new("zipcrypto", header),
        PasswordCheck::Aes { salt, .. } => Twin::new("zipaes", salt),
    }
}

/// Decrypts a ZipCrypto entry starting at `data` (12-byte encryption header + payload) if `check` matches.
pub(crate) fn decrypt_zipcrypto(password: &str, check: u8, data: &[u8]) -> Option<Vec<u8>> {
    zipview::decrypt_zipcrypto(password, check, data)
}

/// Decrypts a WinZip AES payload. `Some(packed)` requires the complete authenticated payload.
pub(crate) fn decrypt_winzip_aes(
    password: &str,
    strength: u8,
    data: &[u8],
    full_packed: Option<u64>,
) -> Option<Vec<u8>> {
    zipview::decrypt_winzip_aes(password, strength, data, full_packed)
}

/// Decrypts an unauthenticated prefix for inspection, excluding any available authentication-tag bytes.
/// Callers must retain that provenance and must not adopt an NZB or password from this preview.
pub(crate) fn decrypt_winzip_aes_preview(
    password: &str,
    strength: u8,
    data: &[u8],
    full_packed: Option<u64>,
) -> Option<Vec<u8>> {
    if let Some(packed) = full_packed {
        if data.len() as u64 >= packed {
            return None;
        }
        let end = usize::try_from((data.len() as u64).min(packed.checked_sub(10)?)).ok()?;
        return zipview::decrypt_winzip_aes(password, strength, data.get(..end)?, None);
    }
    zipview::decrypt_winzip_aes(password, strength, data, None)
}

/// AES's own extra field, 0x9901: `(strength, actual_method)`.
pub(crate) fn aes_extra(extra: &[u8]) -> Option<(u8, u16)> {
    zipview::aes_extra(extra)
}

/// The test an encrypted file's first bytes hold, if they were read.
fn password_check(entry: &[u8], flags: usize, method: usize, extra: &[u8], data_at: usize) -> Option<PasswordCheck> {
    zipview::password_check_at(entry, flags, method, extra, data_at)
}

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
            if l.password_check.is_none() {
                let check = password_check(entry, flags, method, extra, 30 + n + e);
                l.twin = l.twin.take().or_else(|| check.as_ref().map(twin_of));
                l.password_check = check.map(PasswordTest::Zip);
            }
        }
        let (unp, packed) = zip64_sizes(extra, unp, packed);
        let name = name_of(name, flags);
        if !is_directory(&name) {
            let dos = (le16(entry, 10), le16(entry, 12));
            let facts = note(&mut l, flags, method, le32(entry, 14), dos, extra, None, le16(entry, 4));
            if !l.push_with(&name, Some(unp), facts) {
                return l;
            }
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
        if !is_directory(&name) {
            // the directory's CRC stands even when the local header left it to a descriptor
            let dos = (le16(entry, 12), le16(entry, 14));
            let facts =
                note(&mut l, flags & !0x0008, method, le32(entry, 16), dos, extra, le16(entry, 4), le16(entry, 6));
            if !l.push_with(&name, Some(unp), facts) {
                return l;
            }
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
    from_tail.password_check = from_tail.password_check.or(from_head.password_check);
    // the local headers hold what the directory doesn't: the encryption header, and often the full extras
    from_tail.twin = from_tail.twin.or(from_head.twin);
    from_tail.owner = from_tail.owner.or(from_head.owner);
    from_tail.zone = from_tail.zone.or(from_head.zone);
    from_tail.local_wall = from_tail.local_wall.or(from_head.local_wall);
    from_tail
}

#[cfg(test)]
mod tests {
    use super::*;

    fn authenticated_aes_payload() -> Vec<u8> {
        // PBKDF2/HMAC from Python hashlib; CTR keystream from OpenSSL AES-256-ECB
        // over little-endian counters starting at one. Password: secret, salt: 0..16.
        let hex = "000102030405060708090a0b0c0d0e0fa33666d7442270f4c191ec15ccc0adb7347a9401c804c4eed39914c47880c4e326a58e552d2d328ebca47dfd0f6cc0";
        (0..hex.len()).step_by(2).map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap()).collect()
    }

    #[test]
    fn a_complete_aes_payload_requires_its_authentication_tag() {
        let data = authenticated_aes_payload();
        let packed = Some(data.len() as u64);
        assert_eq!(decrypt_winzip_aes("secret", 3, &data, packed).unwrap(), b"<nzb>authenticated nested NZB</nzb>");
        for at in [18, data.len() - 1] {
            let mut damaged = data.clone();
            damaged[at] ^= 1;
            assert!(decrypt_winzip_aes("secret", 3, &damaged, packed).is_none(), "byte {at}");
        }
        assert!(decrypt_winzip_aes("wrong", 3, &data, packed).is_none());
        for end in [data.len() - 10, data.len() - 1] {
            assert!(decrypt_winzip_aes("secret", 3, &data[..end], packed).is_none());
        }
    }

    #[test]
    fn an_aes_preview_keeps_only_the_available_ciphertext() {
        let data = authenticated_aes_payload();
        let plain = b"<nzb>authenticated nested NZB</nzb>";
        // The final incomplete range reaches into the tag; none of that tag is plaintext.
        for end in [23, data.len() - 10, data.len() - 5] {
            let want = (end - 18).min(plain.len());
            assert_eq!(
                decrypt_winzip_aes_preview("secret", 3, &data[..end], Some(data.len() as u64)).unwrap(),
                plain[..want],
            );
        }
        assert!(decrypt_winzip_aes_preview("secret", 3, &data, Some(data.len() as u64)).is_none());
        assert!(decrypt_winzip_aes_preview("wrong", 3, &data[..23], Some(data.len() as u64)).is_none());
    }

    #[test]
    fn an_aes_payload_larger_than_declared_is_rejected() {
        let data = authenticated_aes_payload();
        assert!(decrypt_winzip_aes("secret", 3, &data, Some(data.len() as u64 - 1)).is_none());
    }

    #[test]
    fn a_real_zipcrypto_zip_tests_its_password() {
        // `zip -P secret -0`, for a check against another tool's work
        let hex = "504b03040a00090000005385615d8088f9e51d0000001100000005001c00612e74787455540900035dc0e76a5dc0e76a75780b000104f50100000400000000f7ab9852cf9d136c418bc8d76355e788c188185d9bab38a1e11ee61944";
        let data: Vec<u8> =
            (0..hex.len()).step_by(2).map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap()).collect();
        let test = list_head(&data).password_check.expect("a check");
        assert_eq!(test.accepts("secret"), Some(true));
        assert_eq!(test.accepts("wrong"), Some(false));
        assert!(!test.certain());
        assert_eq!(PasswordTest::from_bytes(&test.to_bytes()), Some(test));
    }

    #[test]
    fn an_aes_zip_tests_its_password() {
        // the verifier from Python's hashlib.pbkdf2_hmac, for a check against another's work
        let mut salt = [0; 16];
        for (i, b) in salt.iter_mut().enumerate() {
            *b = i as u8;
        }
        let test = PasswordTest::Zip(PasswordCheck::Aes { strength: 3, salt, verifier: [0xa3, 0x36] });
        assert_eq!(test.accepts("secret"), Some(true));
        assert_eq!(test.accepts("wrong"), Some(false));
        assert!(test.certain());
        assert_eq!(PasswordTest::from_bytes(&test.to_bytes()), Some(test));
    }

    #[test]
    fn an_aes_header_reads_from_its_extra_field() {
        let extra = [0x01, 0x99, 7, 0, 2, 0, b'A', b'E', 1, 8, 0];
        let mut entry = vec![0; 30];
        entry.extend([9; 8]);
        entry.extend([0xab, 0xcd]);
        let check = password_check(&entry, 1, 99, &extra, 30).unwrap();
        assert!(matches!(check, PasswordCheck::Aes { strength: 1, verifier: [0xab, 0xcd], .. }));
    }
}
