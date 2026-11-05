//! Inner file names from RAR4 and RAR5 headers, in one article's bytes.
#![cfg_attr(not(test), deny(clippy::indexing_slicing, clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use super::{Encryption, Listing, MAX_NAME_BYTES, PasswordTest, crc32};

const MAX_BLOCKS: usize = 65_536;
const MARK4: &[u8] = b"Rar!\x1a\x07\x00";
const MARK5: &[u8] = b"Rar!\x1a\x07\x01\x00";

/// By its marker. No marker: damaged.
pub fn list(data: &[u8]) -> Listing {
    list_with_password(data, None)
}

/// By its marker, decrypting `-hp` encrypted headers when `password` is given and matches.
pub fn list_with_password(data: &[u8], password: Option<&str>) -> Listing {
    if data.starts_with(MARK5) {
        list5_with_password(data, password)
    } else if data.starts_with(MARK4) {
        list4_with_password(data, password)
    } else {
        Listing { damaged: true, ..Default::default() }
    }
}

fn le16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at.checked_add(2)?)?.try_into().ok()?))
}

fn le32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at.checked_add(4)?)?.try_into().ok()?))
}

pub fn list4(data: &[u8]) -> Listing {
    list4_with_password(data, None)
}

pub fn list4_with_password(data: &[u8], password: Option<&str>) -> Listing {
    let mut l = Listing::default();
    let mut pos = 7usize;
    let mut encrypted_headers = false;
    let mut salt_cache: Option<([u8; 8], [u8; 16], [u8; 16])> = None;
    for _ in 0..MAX_BLOCKS {
        if encrypted_headers {
            let Some(pw) = password else { return l };
            let Some((hdr_bytes, header, _)) = decrypt_rar4_header(data, pos, pw, &mut salt_cache) else {
                return l;
            };
            let (Some(ty), Some(flags)) = (header.get(2).copied(), le16(&header, 3)) else { return l };
            if ty == 0x7b {
                l.complete = true;
                return l;
            }
            let mut add = 0u64;
            if flags & 0x8000 != 0 {
                let Some(low) = le32(&header, 7) else { return l };
                add = u64::from(low);
            }
            if ty == 0x74 {
                let Some(high_pack) = file4(&mut l, &header, flags) else { return l };
                add |= high_pack << 32;
                if l.files.len() >= super::MAX_ENTRIES {
                    return l;
                }
            }
            let next = (pos as u64).saturating_add(hdr_bytes as u64).saturating_add(add);
            match usize::try_from(next).ok().filter(|n| *n <= data.len()) {
                Some(n) => pos = n,
                None => return l,
            }
            continue;
        }
        // what isnt there yet is truncation, not damage
        let Some(start) = data.get(pos..).filter(|rest| rest.len() >= 7) else { return l };
        let (Some(ty), Some(flags), Some(size)) = (start.get(2).copied(), le16(start, 3), le16(start, 5)) else {
            return l;
        };
        let size = usize::from(size);
        let Some(header) = start.get(..size) else { return l };
        if size < 7 {
            l.damaged = true;
            return l;
        }
        if matches!(ty, 0x73 | 0x74 | 0x7a) {
            // an old main header with a comment inside has only its first 13 bytes covered
            let covered = if ty == 0x73 && flags & 0x0002 != 0 { header.get(2..13) } else { header.get(2..) };
            let stored = le16(header, 0);
            if covered.is_none_or(|c| Some(crc32(c) as u16) != stored) {
                l.damaged = true;
                return l;
            }
        }
        if ty == 0x73 && flags & 0x0080 != 0 {
            // what follows is salt and AES data
            l.encrypted = true;
            l.encryption = Some(Encryption::Rar4Aes128);
            l.headers_encrypted = true;
            let Some(pw) = password.filter(|pw| !pw.is_empty()) else { return l };
            let _ = pw;
            let Some(next) = pos.checked_add(size).filter(|n| *n <= data.len()) else { return l };
            pos = next;
            encrypted_headers = true;
            continue;
        }
        if ty == 0x7b {
            l.complete = true;
            return l;
        }
        let mut add = 0u64;
        if flags & 0x8000 != 0 {
            let Some(low) = le32(header, 7) else {
                l.damaged = true;
                return l;
            };
            add = u64::from(low);
        }
        if ty == 0x74 {
            let Some(high_pack) = file4(&mut l, header, flags) else { return l };
            add |= high_pack << 32;
            if l.files.len() >= super::MAX_ENTRIES {
                return l;
            }
        }
        let next = (pos as u64).saturating_add(size as u64).saturating_add(add);
        match usize::try_from(next).ok().filter(|n| *n <= data.len()) {
            Some(n) => pos = n,
            // inside one article this is the normal end
            None => return l,
        }
    }
    l
}

/// A file block's name and size, and the high half of its packed size. None: damaged.
fn file4(l: &mut Listing, header: &[u8], flags: u16) -> Option<u64> {
    let fields = || -> Option<(u32, u32, u32, &[u8])> {
        let (unp, name_len) = (le32(header, 11)?, usize::from(le16(header, 26)?));
        let (high_pack, high_unp, name_at): (u32, u32, usize) =
            if flags & 0x0100 != 0 { (le32(header, 32)?, le32(header, 36)?, 40) } else { (0, 0, 32) };
        if header.len() < 32 {
            return None;
        }
        Some((unp, high_pack, high_unp, header.get(name_at..name_at.checked_add(name_len)?)?))
    };
    let Some((unp, high_pack, high_unp, name)) = fields() else {
        l.damaged = true;
        return None;
    };
    if flags & 0x0004 != 0 {
        l.encrypted = true;
        l.encryption = Some(Encryption::Rar4Aes128);
    }
    if flags & 0x00e0 != 0x00e0 {
        let size = u64::from(unp) | u64::from(high_unp) << 32;
        l.push(&name4(name, flags), Some(size));
    }
    Some(u64::from(high_pack))
}

fn name4(raw: &[u8], flags: u16) -> String {
    if flags & 0x0200 != 0
        && let Some(nul) = raw.iter().position(|b| *b == 0)
        && let (Some(name), Some(enc)) = (raw.get(..nul), raw.get(nul + 1..))
    {
        return unicode4(name, enc);
    }
    String::from_utf8_lossy(raw).into_owned()
}

/// The packed unicode form: commands of 2 bits, high bits first, from flag bytes in the stream.
fn unicode4(name: &[u8], enc: &[u8]) -> String {
    let mut bytes = enc.iter().copied();
    let Some(high) = bytes.next() else { return String::from_utf8_lossy(name).into_owned() };
    let high = u16::from(high) << 8;
    let mut out: Vec<u16> = Vec::new();
    let (mut flags, mut bits) = (0u8, 0u32);
    while out.len() < MAX_NAME_BYTES {
        if bits == 0 {
            let Some(f) = bytes.next() else { break };
            flags = f;
            bits = 8;
        }
        let command = flags >> 6;
        flags = flags.wrapping_shl(2);
        bits -= 2;
        match command {
            0 => {
                let Some(b) = bytes.next() else { break };
                out.push(u16::from(b));
            }
            1 => {
                let Some(b) = bytes.next() else { break };
                out.push(u16::from(b) | high);
            }
            2 => {
                let (Some(a), Some(b)) = (bytes.next(), bytes.next()) else { break };
                out.push(u16::from_le_bytes([a, b]));
            }
            _ => {
                let Some(len) = bytes.next() else { break };
                let correction = if len & 0x80 != 0 {
                    let Some(k) = bytes.next() else { break };
                    Some(k)
                } else {
                    None
                };
                for _ in 0..usize::from(len & 0x7f) + 2 {
                    let Some(c) = name.get(out.len()).copied() else { break };
                    out.push(match correction {
                        Some(k) => u16::from(c.wrapping_add(k)) | high,
                        None => u16::from(c),
                    });
                }
            }
        }
    }
    String::from_utf16_lossy(&out)
}

/// little-endian base 128, 10 bytes at most, a 10th above 1 rejected
fn vint(b: &[u8], pos: &mut usize) -> Option<u64> {
    let mut value = 0u64;
    for i in 0..10u32 {
        let byte = *b.get(*pos)?;
        *pos += 1;
        if i == 9 && byte > 1 {
            return None;
        }
        value |= u64::from(byte & 0x7f) << (7 * i);
        if byte & 0x80 == 0 {
            return Some(value);
        }
    }
    None
}

pub fn list5(data: &[u8]) -> Listing {
    list5_with_password(data, None)
}

pub fn list5_with_password(data: &[u8], password: Option<&str>) -> Listing {
    let mut l = Listing::default();
    let mut pos = 8usize;
    let mut hdr_key: Option<[u8; 32]> = None;
    for _ in 0..MAX_BLOCKS {
        if let Some(key) = &hdr_key {
            let (hdr_bytes, inner) = match decrypt_rar5_header(data, pos, key) {
                Ok(Some(v)) => v,
                Ok(None) => return l,
                Err(()) => {
                    if l.password_check.is_some() {
                        l.damaged = true;
                    }
                    return l;
                }
            };
            let Some(block) = block5(&mut l, &inner, password, &mut hdr_key) else {
                l.damaged = true;
                return l;
            };
            if l.complete || l.files.len() >= super::MAX_ENTRIES {
                return l;
            }
            let next = (pos as u64).saturating_add(hdr_bytes as u64).saturating_add(block);
            match usize::try_from(next).ok().filter(|n| *n <= data.len()) {
                Some(n) => pos = n,
                None => return l,
            }
            continue;
        }
        let Some(rest) = data.get(pos..).filter(|rest| rest.len() >= 4) else { return l };
        // the header's size: 3 bytes at most, and running out is truncation
        let (mut size, mut size_len) = (0usize, 0usize);
        loop {
            let Some(byte) = rest.get(4 + size_len).copied() else { return l };
            size |= usize::from(byte & 0x7f) << (7 * size_len);
            size_len += 1;
            if byte & 0x80 == 0 {
                break;
            }
            if size_len == 3 {
                l.damaged = true;
                return l;
            }
        }
        if size == 0 {
            l.damaged = true;
            return l;
        }
        let Some(body) = rest.get(4..4 + size_len + size) else { return l };
        if le32(rest, 0) != Some(crc32(body)) {
            l.damaged = true;
            return l;
        }
        let Some(inner) = body.get(size_len..) else { return l };
        let Some(block) = block5(&mut l, inner, password, &mut hdr_key) else {
            l.damaged = true;
            return l;
        };
        if l.complete || (l.headers_encrypted && hdr_key.is_none()) || l.files.len() >= super::MAX_ENTRIES {
            return l;
        }
        let next = (pos as u64).saturating_add((4 + size_len + size) as u64).saturating_add(block);
        match usize::try_from(next).ok().filter(|n| *n <= data.len()) {
            Some(n) => pos = n,
            None => return l,
        }
    }
    l
}

/// One verified header: what it says goes in `l`, and its data size comes back.
/// None: a read failed inside a header whose CRC was right, soo it's damaged.
fn block5(
    l: &mut Listing,
    inner: &[u8],
    password: Option<&str>,
    hdr_key: &mut Option<[u8; 32]>,
) -> Option<u64> {
    let mut p = 0usize;
    let ty = vint(inner, &mut p)?;
    let flags = vint(inner, &mut p)?;
    let extra = if flags & 1 != 0 { usize::try_from(vint(inner, &mut p)?).ok()? } else { 0 };
    let data = if flags & 2 != 0 { vint(inner, &mut p)? } else { 0 };
    let end = inner.len().checked_sub(extra)?;
    if end < p {
        return None;
    }
    match ty {
        4 => {
            l.encrypted = true;
            l.encryption = Some(Encryption::Rar5Aes256);
            l.headers_encrypted = true;
            let body = inner.get(p..end)?;
            // the header's own: version, flags, kdf, salt, check
            let check = password_check(body, false);
            l.password_check = check.map(PasswordTest::Rar5).or(l.password_check);
            if let Some(pw) = password.filter(|pw| !pw.is_empty())
                && let Some((kdf, salt, _)) = crypt_kdf_salt_iv(body, false)
            {
                *hdr_key = derive_rar5_key(pw, kdf, &salt, check);
            }
        }
        5 => l.complete = true,
        2 | 3 => {
            let file_flags = vint(inner, &mut p)?;
            let unpacked = vint(inner, &mut p)?;
            let _attributes = vint(inner, &mut p)?;
            if file_flags & 2 != 0 {
                p = p.checked_add(4)?;
            }
            if file_flags & 4 != 0 {
                p = p.checked_add(4)?;
            }
            let _compression = vint(inner, &mut p)?;
            let _host = vint(inner, &mut p)?;
            let name_len = usize::try_from(vint(inner, &mut p)?).ok()?;
            let name_end = p.checked_add(name_len)?;
            if name_end > end {
                return None;
            }
            let name = inner.get(p..name_end)?;
            if ty == 2 {
                if encrypted_record(inner.get(end..)?) {
                    l.encrypted = true;
                    l.encryption = Some(Encryption::Rar5Aes256);
                    l.password_check =
                        l.password_check.or_else(|| record_check(inner.get(end..)?).map(PasswordTest::Rar5));
                }
                if file_flags & 1 == 0 {
                    let size = (file_flags & 8 == 0).then_some(unpacked);
                    l.push(&String::from_utf8_lossy(name), size);
                }
            }
        }
        _ => {}
    }
    Some(data)
}

/// What RAR5 keeps to test a password: `2^kdf` rounds over `salt` have to fold down to `check`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PasswordCheck {
    pub kdf: u8,
    pub salt: [u8; 16],
    pub check: [u8; 8],
}

pub type Rar5PswCheck = PasswordCheck;

/// More rounds than this take too long to run while a page waits. Capped at 2^16 (65,536 rounds) to prevent DoS.
pub const MAX_KDF: u8 = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum EncryptionMode {
    None,
    HeaderEncryptedHp,
    CandidateHp,
    IndeterminateIncomplete,
    Indeterminate,
}

impl EncryptionMode {
    pub fn is_encrypted(self) -> bool {
        matches!(self, EncryptionMode::HeaderEncryptedHp | EncryptionMode::CandidateHp)
    }

    pub fn is_candidate(self) -> bool {
        matches!(self, EncryptionMode::HeaderEncryptedHp | EncryptionMode::CandidateHp)
    }
}

pub struct Rar5Header;

impl Rar5Header {
    /// Zero-download RAR5 -hp archive encryption detection candidate heuristic from NZB/overview volume sizes.
    pub fn detect_encryption(volume_sizes: &[i64]) -> EncryptionMode {
        let valid: Vec<i64> = volume_sizes.iter().copied().filter(|&s| s > 0).collect();
        if valid.is_empty() {
            return EncryptionMode::None;
        }
        if valid.len() == 1 {
            let s = match valid.first() {
                Some(v) => *v,
                None => return EncryptionMode::None,
            };
            let rem = s.rem_euclid(16);
            if rem == 14 || rem == 10 {
                return EncryptionMode::CandidateHp;
            }
            return EncryptionMode::None;
        }

        let n = valid.len();
        let nominal = match valid.first() {
            Some(v) => *v,
            None => return EncryptionMode::None,
        };
        let nominal_slice = match valid.get(..n.saturating_sub(1)) {
            Some(slice) => slice,
            None => return EncryptionMode::None,
        };
        let nominal_match = nominal_slice.iter().all(|&v| v == nominal);
        let tail = match valid.get(n.saturating_sub(1)) {
            Some(v) => *v,
            None => return EncryptionMode::None,
        };

        if !nominal_match {
            return EncryptionMode::Indeterminate;
        }

        if tail == nominal {
            return EncryptionMode::IndeterminateIncomplete;
        }

        let rem = tail.rem_euclid(16);
        if rem == 14 || rem == 10 {
            EncryptionMode::HeaderEncryptedHp
        } else {
            EncryptionMode::None
        }
    }

    /// Article 1 Block Type 4 inspection:
    /// Inspects article 1 prefix for RAR5 magic and checks if the following block header type is 4 (Encryption Header).
    pub fn verify_article1(bytes: &[u8]) -> bool {
        if !bytes.starts_with(MARK5) {
            return false;
        }
        let pos = MARK5.len();
        let Some(rest) = bytes.get(pos..) else { return false };
        if rest.len() < 4 {
            return false;
        }
        let mut size_len = 0usize;
        let mut size = 0usize;
        loop {
            let Some(byte) = rest.get(4 + size_len).copied() else { return false };
            size |= usize::from(byte & 0x7f) << (7 * size_len);
            size_len += 1;
            if byte & 0x80 == 0 {
                break;
            }
            if size_len == 3 {
                return false;
            }
        }
        if size == 0 {
            return false;
        }
        let Some(end) = (4 + size_len).checked_add(size) else { return false };
        let Some(body) = rest.get(4..end) else { return false };
        if le32(rest, 0) != Some(crc32(body)) {
            return false;
        }
        let Some(inner) = body.get(size_len..) else { return false };
        let mut p = 0usize;
        let Some(ty) = vint(inner, &mut p) else { return false };
        ty == 4
    }

    pub fn inspect_article1_block4(bytes: &[u8]) -> bool {
        Self::verify_article1(bytes)
    }
}

impl PasswordCheck {
    pub fn new(kdf: u8, salt: [u8; 16], check: [u8; 8]) -> Self {
        PasswordCheck { kdf, salt, check }
    }

    pub fn verify(&self, candidate: &str) -> bool {
        self.accepts(candidate).unwrap_or(false)
    }

    pub async fn verify_async(&self, candidate: &str) -> bool {
        let check = *self;
        let cand = candidate.to_string();
        tokio::task::spawn_blocking(move || check.verify(&cand))
            .await
            .unwrap_or(false)
    }

    /// Stored as kdf, salt, check.
    pub fn to_bytes(self) -> Vec<u8> {
        let mut out = vec![self.kdf];
        out.extend(self.salt);
        out.extend(self.check);
        out
    }

    pub fn from_bytes(b: &[u8]) -> Option<PasswordCheck> {
        if b.len() < 25 {
            return None;
        }
        Some(PasswordCheck {
            kdf: *b.first()?,
            salt: b.get(1..17)?.try_into().ok()?,
            check: b.get(17..25)?.try_into().ok()?,
        })
    }

    /// Whether this is the password. None: too many rounds to find out here.
    pub fn accepts(&self, password: &str) -> Option<bool> {
        if self.kdf > MAX_KDF {
            return None;
        }
        let hash = pbkdf2_sha256(password.as_bytes(), &self.salt, (1u32 << self.kdf) + 32)?;
        let mut folded = [0u8; 8];
        for (i, b) in hash.iter().enumerate() {
            if let Some(slot) = folded.get_mut(i % 8) {
                *slot ^= b;
            }
        }
        Some(folded == self.check)
    }
}

/// PBKDF2's first block, which is all RAR asks of it.
fn pbkdf2_sha256(password: &[u8], salt: &[u8], rounds: u32) -> Option<[u8; 32]> {
    use hmac::{Hmac, Mac};
    let keyed = Hmac::<sha2::Sha256>::new_from_slice(password).ok()?;
    let mut mac = keyed.clone();
    mac.update(salt);
    mac.update(&1u32.to_be_bytes());
    let mut u: [u8; 32] = mac.finalize().into_bytes().into();
    let mut out = u;
    for _ in 1..rounds {
        let mut mac = keyed.clone();
        mac.update(&u);
        u = mac.finalize().into_bytes().into();
        for (o, b) in out.iter_mut().zip(u) {
            *o ^= b;
        }
    }
    Some(out)
}

/// `version, flags, kdf, salt, [iv,] check`. A check is only there with flag 1, and its sum has to be right.
fn password_check(body: &[u8], has_iv: bool) -> Option<PasswordCheck> {
    use sha2::Digest;
    let mut p = 0usize;
    let _version = vint(body, &mut p)?;
    let flags = vint(body, &mut p)?;
    if flags & 1 == 0 {
        return None;
    }
    let kdf = *body.get(p)?;
    p += 1;
    let salt: [u8; 16] = body.get(p..p + 16)?.try_into().ok()?;
    p += if has_iv { 32 } else { 16 };
    let check: [u8; 8] = body.get(p..p + 8)?.try_into().ok()?;
    let sum = body.get(p + 8..p + 12)?;
    (sha2::Sha256::digest(check).get(..4) == Some(sum)).then_some(PasswordCheck { kdf, salt, check })
}

/// The check in a file's encryption record.
fn record_check(area: &[u8]) -> Option<PasswordCheck> {
    let mut p = 0usize;
    while p < area.len() {
        let size = usize::try_from(vint(area, &mut p)?).ok().filter(|s| *s > 0)?;
        let end = p.checked_add(size).filter(|e| *e <= area.len())?;
        let mut at = p;
        if vint(area, &mut at) == Some(1) {
            return password_check(area.get(at..end)?, true);
        }
        p = end;
    }
    None
}

/// an extra area holding a file encryption record. A record of size 0 or past the area ends the walk
fn encrypted_record(area: &[u8]) -> bool {
    let mut p = 0usize;
    while p < area.len() {
        let Some(size) = vint(area, &mut p).and_then(|s| usize::try_from(s).ok()).filter(|s| *s > 0) else {
            return false;
        };
        let Some(end) = p.checked_add(size).filter(|e| *e <= area.len()) else { return false };
        let mut at = p;
        if vint(area, &mut at) == Some(1) {
            return true;
        }
        p = end;
    }
    false
}

/// Parses `(kdf, salt, iv)` from a RAR5 encryption header (`has_iv = false`) or file crypt record (`has_iv = true`).
pub(crate) fn crypt_kdf_salt_iv(body: &[u8], has_iv: bool) -> Option<(u8, [u8; 16], Option<[u8; 16]>)> {
    let mut p = 0usize;
    let version = vint(body, &mut p)?;
    if version != 0 {
        return None;
    }
    let _flags = vint(body, &mut p)?;
    let kdf = *body.get(p)?;
    p += 1;
    let salt: [u8; 16] = body.get(p..p + 16)?.try_into().ok()?;
    p += 16;
    let iv = if has_iv { Some(body.get(p..p + 16)?.try_into().ok()?) } else { None };
    Some((kdf, salt, iv))
}

/// `(kdf, salt, iv, check)` from a RAR5 file block's encryption record.
pub type Rar5CryptParams = (u8, [u8; 16], [u8; 16], Option<PasswordCheck>);

/// Extracts `(kdf, salt, iv, check)` from a RAR5 file block's extra area (record type 1).
pub(crate) fn rar5_record_params(area: &[u8]) -> Option<Rar5CryptParams> {
    let mut p = 0usize;
    while p < area.len() {
        let size = usize::try_from(vint(area, &mut p)?).ok().filter(|s| *s > 0)?;
        let end = p.checked_add(size).filter(|e| *e <= area.len())?;
        let mut at = p;
        if vint(area, &mut at) == Some(1) {
            let body = area.get(at..end)?;
            let (kdf, salt, Some(iv)) = crypt_kdf_salt_iv(body, true)? else { return None };
            let check = password_check(body, true);
            return Some((kdf, salt, iv, check));
        }
        p = end;
    }
    None
}

/// Derives RAR5's 32-byte AES-256 key, verifying `check` in the same PBKDF2 pass when present.
pub(crate) fn derive_rar5_key(
    password: &str,
    kdf: u8,
    salt: &[u8; 16],
    check: Option<PasswordCheck>,
) -> Option<[u8; 32]> {
    use hmac::{Hmac, Mac};
    if kdf > MAX_KDF {
        return None;
    }
    let rounds = 1u32 << kdf;
    let keyed = Hmac::<sha2::Sha256>::new_from_slice(password.as_bytes()).ok()?;
    let mut mac = keyed.clone();
    mac.update(salt);
    mac.update(&1u32.to_be_bytes());
    let mut u: [u8; 32] = mac.finalize().into_bytes().into();
    let mut out = u;
    for _ in 1..rounds {
        let mut mac = keyed.clone();
        mac.update(&u);
        u = mac.finalize().into_bytes().into();
        for (o, b) in out.iter_mut().zip(u) {
            *o ^= b;
        }
    }
    let aes_key = out;
    if let Some(chk) = check {
        for _ in 0..32 {
            let mut mac = keyed.clone();
            mac.update(&u);
            u = mac.finalize().into_bytes().into();
            for (o, b) in out.iter_mut().zip(u) {
                *o ^= b;
            }
        }
        let mut folded = [0u8; 8];
        for (i, b) in out.iter().enumerate() {
            if let Some(slot) = folded.get_mut(i % 8) {
                *slot ^= b;
            }
        }
        if folded != chk.check {
            return None;
        }
    }
    Some(aes_key)
}

/// Decrypts one RAR5 `-hp` encrypted block header at `pos` (where the 16-byte IV sits).
/// Returns `Ok(Some((16 + enc_hdr_len, inner_header_bytes)))`, `Ok(None)` on truncation, or `Err(())` on CRC/format failure.
pub(crate) fn decrypt_rar5_header(
    data: &[u8],
    pos: usize,
    key: &[u8; 32],
) -> Result<Option<(usize, Vec<u8>)>, ()> {
    let Some(first_end) = pos.checked_add(32) else { return Ok(None) };
    let (Some(iv_slice), Some(first_ct)) = (data.get(pos..pos + 16), data.get(pos + 16..first_end)) else {
        return Ok(None);
    };
    let Ok(iv): Result<[u8; 16], _> = iv_slice.try_into() else { return Ok(None) };
    let Some(first_dec) = super::aes::decrypt_cbc(key, &iv, first_ct) else { return Err(()) };
    let (mut size, mut size_len) = (0usize, 0usize);
    loop {
        let Some(byte) = first_dec.get(4 + size_len).copied() else { return Err(()) };
        size |= usize::from(byte & 0x7f) << (7 * size_len);
        size_len += 1;
        if byte & 0x80 == 0 {
            break;
        }
        if size_len == 3 {
            return Err(());
        }
    }
    if size == 0 {
        return Err(());
    }
    let Some(raw_len) = (4 + size_len).checked_add(size) else { return Err(()) };
    let Some(enc_len) = raw_len.checked_add(15).map(|n| n & !15) else { return Err(()) };
    let Some(total_hdr) = 16usize.checked_add(enc_len) else { return Err(()) };
    let Some(hdr_end) = pos.checked_add(total_hdr) else { return Ok(None) };
    let Some(enc_hdr) = data.get(pos + 16..hdr_end) else { return Ok(None) };
    let Some(dec) = super::aes::decrypt_cbc(key, &iv, enc_hdr) else { return Err(()) };
    let Some(body) = dec.get(4..raw_len) else { return Err(()) };
    if le32(&dec, 0) != Some(crc32(body)) {
        return Err(());
    }
    let Some(inner) = body.get(size_len..) else { return Err(()) };
    Ok(Some((total_hdr, inner.to_vec())))
}

/// RAR3 / RAR4 key and IV derivation (`SetKey30`: 262,144 SHA-1 rounds over UTF-16LE password + 8-byte salt).
pub(crate) fn derive_rar4_key_iv(password: &str, salt: &[u8; 8]) -> ([u8; 16], [u8; 16]) {
    use sha1::{Digest, Sha1};
    let mut raw = Vec::with_capacity(password.len() * 2 + 8);
    for ch in password.encode_utf16() {
        raw.extend_from_slice(&ch.to_le_bytes());
    }
    raw.extend_from_slice(salt);
    let mut sha = Sha1::new();
    let mut iv = [0u8; 16];
    const ROUNDS: u32 = 0x40000;
    const STEP: u32 = ROUNDS / 16;
    for i in 0..ROUNDS {
        sha.update(&raw);
        sha.update([i as u8, (i >> 8) as u8, (i >> 16) as u8]);
        if i % STEP == 0 {
            let d = sha.clone().finalize();
            if let (Some(slot), Some(b)) = (iv.get_mut((i / STEP) as usize), d.get(19).copied()) {
                *slot = b;
            }
        }
    }
    let final_d = sha.finalize();
    let mut key = [0u8; 16];
    for i in 0..4 {
        for j in 0..4 {
            if let (Some(dst), Some(src)) = (key.get_mut(i * 4 + j), final_d.get(i * 4 + (3 - j)).copied()) {
                *dst = src;
            }
        }
    }
    (key, iv)
}

/// Decrypts one RAR4 `-hp` encrypted block header at `pos` (where the 8-byte salt sits).
pub(crate) fn decrypt_rar4_header(
    data: &[u8],
    pos: usize,
    password: &str,
    cache: &mut Option<([u8; 8], [u8; 16], [u8; 16])>,
) -> Option<(usize, Vec<u8>, [u8; 8])> {
    let first_end = pos.checked_add(24)?;
    let salt: [u8; 8] = data.get(pos..pos + 8)?.try_into().ok()?;
    let first_ct = data.get(pos + 8..first_end)?;
    let (key, iv) = match cache {
        Some((s, k, i)) if *s == salt => (*k, *i),
        _ => {
            let (k, i) = derive_rar4_key_iv(password, &salt);
            *cache = Some((salt, k, i));
            (k, i)
        }
    };
    let first_dec = super::aes::decrypt_cbc(&key, &iv, first_ct)?;
    let (ty, flags, size) = (*first_dec.get(2)?, le16(&first_dec, 3)?, usize::from(le16(&first_dec, 5)?));
    if size < 7 || !matches!(ty, 0x72..=0x7b) {
        return None;
    }
    let enc_len = size.checked_add(15)? & !15;
    let total_hdr = 8usize.checked_add(enc_len)?;
    let enc_hdr = data.get(pos + 8..pos.checked_add(total_hdr)?)?;
    let dec = super::aes::decrypt_cbc(&key, &iv, enc_hdr)?;
    let header = dec.get(..size)?;
    if matches!(ty, 0x73 | 0x74 | 0x7a | 0x7b) {
        let covered = if ty == 0x73 && flags & 0x0002 != 0 { header.get(2..13) } else { header.get(2..) };
        if covered.is_none_or(|c| Some(crc32(c) as u16) != le16(header, 0)) {
            return None;
        }
    }
    Some((total_hdr, header.to_vec(), salt))
}
