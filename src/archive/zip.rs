//! Inner file names from ZIP local headers and the central directory.
#![cfg_attr(not(test), deny(clippy::indexing_slicing, clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use super::{Encryption, Listing, PasswordTest};

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
/// What a ZIP keeps at the start of an encrypted file's data to test a password.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PasswordCheck {
    /// 12 bytes that decrypt to end in `check`
    Crypto { header: [u8; 12], check: u8 },
    /// AES: `salt` is `strength * 4 + 4` long, and the key's last two bytes have to be `verifier`
    Aes { strength: u8, salt: [u8; 16], verifier: [u8; 2] },
}

impl PasswordCheck {
    pub fn to_bytes(self) -> Vec<u8> {
        match self {
            PasswordCheck::Crypto { header, check } => {
                let mut out = vec![0, check];
                out.extend(header);
                out
            }
            PasswordCheck::Aes { strength, salt, verifier } => {
                let mut out = vec![strength];
                out.extend(salt);
                out.extend(verifier);
                out
            }
        }
    }

    pub fn from_bytes(b: &[u8]) -> Option<PasswordCheck> {
        match *b.first()? {
            0 => Some(PasswordCheck::Crypto { check: *b.get(1)?, header: b.get(2..14)?.try_into().ok()? }),
            strength => Some(PasswordCheck::Aes {
                strength,
                salt: b.get(1..17)?.try_into().ok()?,
                verifier: b.get(17..19)?.try_into().ok()?,
            }),
        }
    }

    pub fn accepts(&self, password: &str) -> bool {
        match self {
            PasswordCheck::Crypto { header, check } => crypto_check(password.as_bytes(), header) == *check,
            PasswordCheck::Aes { strength, salt, verifier } => {
                let salt_len = usize::from(*strength) * 4 + 4;
                let key_len = (usize::from(*strength) + 1) * 8;
                salt.get(..salt_len)
                    .and_then(|salt| aes_verifier(password.as_bytes(), salt, key_len))
                    .is_some_and(|found| found == *verifier)
            }
        }
    }
}

/// The traditional cipher: what the last of the 12 header bytes decrypts to.
fn crypto_check(password: &[u8], header: &[u8; 12]) -> u8 {
    fn crc_byte(crc: u32, b: u8) -> u32 {
        let mut c = (crc ^ u32::from(b)) & 0xff;
        for _ in 0..8 {
            c = if c & 1 != 0 { (c >> 1) ^ 0xedb8_8320 } else { c >> 1 };
        }
        (crc >> 8) ^ c
    }
    let mut keys = [0x1234_5678u32, 0x2345_6789, 0x3456_7890];
    let update = |keys: &mut [u32; 3], b: u8| {
        keys[0] = crc_byte(keys[0], b);
        keys[1] = keys[1].wrapping_add(keys[0] & 0xff).wrapping_mul(134_775_813).wrapping_add(1);
        keys[2] = crc_byte(keys[2], (keys[1] >> 24) as u8);
    };
    for b in password {
        update(&mut keys, *b);
    }
    let mut last = 0;
    for cipher in header {
        let t = keys[2] | 2;
        last = cipher ^ (t.wrapping_mul(t ^ 1) >> 8) as u8;
        update(&mut keys, last);
    }
    last
}

/// PBKDF2 over HMAC-SHA1, 1000 rounds, as WinZip's AES has it: the two bytes after both keys.
fn aes_verifier(password: &[u8], salt: &[u8], key_len: usize) -> Option<[u8; 2]> {
    use hmac::{Hmac, Mac};
    let keyed = Hmac::<sha1::Sha1>::new_from_slice(password).ok()?;
    let wanted = key_len * 2 + 2;
    let mut derived = Vec::with_capacity(wanted + 20);
    let mut block = 1u32;
    while derived.len() < wanted {
        let mut mac = keyed.clone();
        mac.update(salt);
        mac.update(&block.to_be_bytes());
        let mut u: [u8; 20] = mac.finalize().into_bytes().into();
        let mut out = u;
        for _ in 1..1000 {
            let mut mac = keyed.clone();
            mac.update(&u);
            u = mac.finalize().into_bytes().into();
            for (slot, b) in out.iter_mut().zip(u) {
                *slot ^= b;
            }
        }
        derived.extend(out);
        block += 1;
    }
    derived.get(wanted - 2..wanted)?.try_into().ok()
}

/// AES's own extra field, 0x9901: the key strength, from 1 to 3.
fn aes_strength(extra: &[u8]) -> Option<u8> {
    let mut p = 0usize;
    while let (Some(id), Some(size)) = (le16(extra, p), le16(extra, p + 2)) {
        if id == 0x9901 {
            return extra.get(p + 8).copied().filter(|s| (1..=3).contains(s));
        }
        p = p.checked_add(4 + size)?;
    }
    None
}

/// The test an encrypted file's first bytes hold, if they were read.
fn password_check(entry: &[u8], flags: usize, method: usize, extra: &[u8], data_at: usize) -> Option<PasswordCheck> {
    let data = entry.get(data_at..)?;
    if method == 99 {
        let strength = aes_strength(extra)?;
        let salt_len = usize::from(strength) * 4 + 4;
        let mut salt = [0; 16];
        salt.get_mut(..salt_len)?.copy_from_slice(data.get(..salt_len)?);
        Some(PasswordCheck::Aes { strength, salt, verifier: data.get(salt_len..salt_len + 2)?.try_into().ok()? })
    } else {
        // with sizes after the data, the check is the time's high byte instead of the CRC's
        let check = if flags & 0x0008 != 0 { *entry.get(11)? } else { *entry.get(17)? };
        Some(PasswordCheck::Crypto { header: data.get(..12)?.try_into().ok()?, check })
    }
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
                l.password_check = password_check(entry, flags, method, extra, 30 + n + e).map(PasswordTest::Zip);
            }
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

#[cfg(test)]
mod tests {
    use super::*;

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
