//! Inner file names from RAR4 and RAR5 headers, in one article's bytes.
#![cfg_attr(not(test), deny(clippy::indexing_slicing, clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use super::{Encryption, Listing, MAX_NAME_BYTES, crc32};

const MAX_BLOCKS: usize = 65_536;
const MARK4: &[u8] = b"Rar!\x1a\x07\x00";
const MARK5: &[u8] = b"Rar!\x1a\x07\x01\x00";

/// By its marker. No marker: damaged.
pub fn list(data: &[u8]) -> Listing {
    if data.starts_with(MARK5) {
        list5(data)
    } else if data.starts_with(MARK4) {
        list4(data)
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
    let mut l = Listing::default();
    let mut pos = 7usize;
    for _ in 0..MAX_BLOCKS {
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
            return l;
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
    let mut l = Listing::default();
    let mut pos = 8usize;
    for _ in 0..MAX_BLOCKS {
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
        let Some(block) = block5(&mut l, inner) else {
            l.damaged = true;
            return l;
        };
        if l.complete || l.headers_encrypted || l.files.len() >= super::MAX_ENTRIES {
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
fn block5(l: &mut Listing, inner: &[u8]) -> Option<u64> {
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
