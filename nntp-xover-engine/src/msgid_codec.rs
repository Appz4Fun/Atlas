//! Message-ids: `<prefix tail@domain>`. The domain and a short local prefix
//! are deduplicated per block; the random tail is bit-packed by alphabet.

use crate::varint::{read_varint_unchecked, write_varint};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum PackMode {
    Hex4Bit = 0b00,
    Base64Usenet = 0b01,
    Raw8Bit = 0b10,
    /// Hex4Bit with no dot: the dot-position byte is left out
    Hex4BitNoDot = 0b11,
}

/// Longest local prefix (delimiter included) split off into the prefix table.
pub const MAX_PREFIX_LEN: usize = 12;

const NONE: u8 = 0xFF;

/// 6-bit code of each byte in the Usenet base64 alphabet `0-9 a-z A-Z . -`,
/// NONE elsewhere. Hex digits get their hex value (same codes as 0-9a-f).
const B64_CODE: [u8; 256] = {
    let mut t = [NONE; 256];
    let mut i = 0;
    while i < 10 {
        t[b'0' as usize + i] = i as u8;
        i += 1;
    }
    let mut i = 0;
    while i < 26 {
        t[b'a' as usize + i] = 10 + i as u8;
        t[b'A' as usize + i] = 36 + i as u8;
        i += 1;
    }
    t[b'.' as usize] = 62;
    t[b'-' as usize] = 63;
    t
};

const B64_CHAR: [u8; 64] = *b"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ.-";

/// True for `0-9a-f` only.
const IS_HEX: [bool; 256] = {
    let mut t = [false; 256];
    let mut i = 0;
    while i < 16 {
        t[B64_CHAR[i] as usize] = true;
        i += 1;
    }
    t
};

/// `<local@domain>` split at the last '@'. None if it isn't shaped like that.
#[inline]
pub fn split_msgid(raw: &[u8]) -> Option<(&[u8], &[u8])> {
    let inner = raw.strip_prefix(b"<")?.strip_suffix(b">")?;
    let at = inner.iter().rposition(|&b| b == b'@')?;
    Some((&inner[..at], &inner[at + 1..]))
}

/// Splits `local` after its first '.', '-' or '_' when that prefix (delimiter
/// included) is at most [`MAX_PREFIX_LEN`] bytes; otherwise the prefix is empty.
#[inline]
pub fn split_prefix(local: &[u8]) -> (&[u8], &[u8]) {
    let lim = local.len().min(MAX_PREFIX_LEN);
    match local[..lim].iter().position(|&b| b == b'.' || b == b'-' || b == b'_') {
        Some(i) => local.split_at(i + 1),
        None => (&local[..0], local),
    }
}

pub fn classify(tail: &[u8]) -> PackMode {
    if tail.is_empty() {
        return PackMode::Raw8Bit;
    }
    let mut hex = true;
    let mut dots = 0usize;
    for (i, &b) in tail.iter().enumerate() {
        if B64_CODE[b as usize] == NONE {
            return PackMode::Raw8Bit;
        }
        if b == b'.' {
            dots += 1;
            if dots > 1 || i > 62 {
                hex = false;
            }
        } else if !IS_HEX[b as usize] {
            hex = false;
        }
    }
    // a hex tail that is only a dot packs worse than base64
    if hex && tail.len() > dots { PackMode::Hex4Bit } else { PackMode::Base64Usenet }
}

#[inline(always)]
fn write_header(mode: PackMode, len: usize, out: &mut Vec<u8>) {
    if len >= 63 {
        out.push(((mode as u8) << 6) | 63);
        write_varint(len as u64, out);
    } else {
        out.push(((mode as u8) << 6) | len as u8);
    }
}

/// Appends the packed tail to `out`.
pub fn pack_msgid_tail(tail: &[u8], out: &mut Vec<u8>) {
    let mut mode = classify(tail);
    if mode == PackMode::Hex4Bit && !tail.contains(&b'.') {
        mode = PackMode::Hex4BitNoDot;
    }
    write_header(mode, tail.len(), out);
    match mode {
        PackMode::Hex4BitNoDot => hex_digits_pack(tail, out),
        PackMode::Hex4Bit => {
            let dot = tail.iter().position(|&b| b == b'.');
            out.push(dot.map_or(NONE, |d| d as u8));
            let mut hi: Option<u8> = None;
            for &b in tail {
                if b == b'.' {
                    continue;
                }
                let v = B64_CODE[b as usize];
                match hi.take() {
                    None => hi = Some(v),
                    Some(h) => out.push((h << 4) | v),
                }
            }
            if let Some(h) = hi {
                out.push(h << 4);
            }
        }
        PackMode::Base64Usenet => {
            let mut acc = 0u32;
            let mut bits = 0u32;
            for &b in tail {
                acc = (acc << 6) | B64_CODE[b as usize] as u32;
                bits += 6;
                while bits >= 8 {
                    bits -= 8;
                    out.push((acc >> bits) as u8);
                }
            }
            if bits > 0 {
                out.push((acc << (8 - bits)) as u8);
            }
        }
        PackMode::Raw8Bit => out.extend_from_slice(tail),
    }
}

/// Packs hex digits two to a byte (high nibble first).
fn hex_digits_pack(tail: &[u8], out: &mut Vec<u8>) {
    for pair in tail.chunks(2) {
        let hi = B64_CODE[pair[0] as usize];
        let lo = pair.get(1).map_or(0, |&b| B64_CODE[b as usize]);
        out.push((hi << 4) | lo);
    }
}

/// Two hex chars per packed byte.
const HEX_PAIRS: [[u8; 2]; 256] = {
    let mut t = [[0u8; 2]; 256];
    let mut i = 0;
    while i < 256 {
        t[i] = [B64_CHAR[i >> 4], B64_CHAR[i & 15]];
        i += 1;
    }
    t
};

/// Appends hex digits `from..to` of the nibble stream `body`.
#[inline(always)]
fn hex_digits(body: &[u8], mut from: usize, to: usize, out: &mut Vec<u8>) {
    if from >= to {
        return;
    }
    if from % 2 == 1 {
        out.push(HEX_PAIRS[body.get(from / 2).copied().unwrap_or(0) as usize][1]);
        from += 1;
    }
    let pairs = (to - from) / 2;
    let start = from / 2;
    if let Some(bytes) = body.get(start..start + pairs) {
        out.reserve(pairs * 2);
        for &b in bytes {
            out.extend_from_slice(&HEX_PAIRS[b as usize]);
        }
    }
    if (to - from) % 2 == 1 {
        out.push(HEX_PAIRS[body.get(start + pairs).copied().unwrap_or(0) as usize][0]);
    }
}

/// Appends the unpacked tail to `out`. `packed` is exactly one packed tail.
#[inline(always)]
pub fn unpack_msgid_tail_into(packed: &[u8], out: &mut Vec<u8>) {
    let Some(&h) = packed.first() else { return };
    let mode = h >> 6;
    let mut pos = 1usize;
    let len = if h & 0x3F == 63 { read_varint_unchecked(packed, &mut pos) as usize } else { (h & 0x3F) as usize };
    match mode {
        0b00 => {
            let dot = packed.get(pos).copied().unwrap_or(NONE) as usize;
            pos += 1;
            let body = packed.get(pos..).unwrap_or(&[]);
            let digits = if dot != NONE as usize { len.saturating_sub(1) } else { len };
            // digits before the dot, then the dot, then the rest
            let before = if dot != NONE as usize { dot.min(digits) } else { digits };
            hex_digits(body, 0, before, out);
            if dot != NONE as usize {
                out.push(b'.');
                hex_digits(body, before, digits, out);
            }
        }
        0b11 => hex_digits(packed.get(pos..).unwrap_or(&[]), 0, len, out),
        0b01 => {
            let body = packed.get(pos..).unwrap_or(&[]);
            let mut acc = 0u32;
            let mut bits = 0u32;
            let mut bi = 0usize;
            // never more chars than the bytes there can hold: a damaged length can't run away
            for _ in 0..len.min(body.len() * 8 / 6) {
                while bits < 6 {
                    acc = (acc << 8) | body.get(bi).copied().unwrap_or(0) as u32;
                    bi += 1;
                    bits += 8;
                }
                bits -= 6;
                out.push(B64_CHAR[((acc >> bits) & 0x3F) as usize]);
            }
        }
        _ => out.extend_from_slice(packed.get(pos..pos + len).unwrap_or(&[])),
    }
}

/// `(prefix, tail, domain)` of `<prefix tail@domain>`. A string not shaped like a message-id comes back
/// whole as the tail, with an empty prefix and domain; the block keeps such a row literal.
pub fn split_msgid_parts(raw: &[u8]) -> (&[u8], &[u8], &[u8]) {
    match split_msgid(raw) {
        Some((local, domain)) => {
            let (prefix, tail) = split_prefix(local);
            (prefix, tail, domain)
        }
        None => (&raw[..0], raw, &raw[..0]),
    }
}

/// The mode a tail packs in and, for a hex tail, where its dot is (0xFF for none).
pub fn classify_tail(tail: &[u8]) -> (PackMode, u8) {
    let mode = classify(tail);
    let dot = match mode {
        PackMode::Hex4Bit => tail.iter().position(|&b| b == b'.').map_or(NONE, |d| d as u8),
        _ => NONE,
    };
    (mode, dot)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rt(tail: &[u8]) -> (PackMode, usize) {
        let mut p = Vec::new();
        pack_msgid_tail(tail, &mut p);
        let mut o = Vec::new();
        unpack_msgid_tail_into(&p, &mut o);
        assert_eq!(o, tail, "{}", String::from_utf8_lossy(tail));
        (classify(tail), p.len())
    }

    #[test]
    fn tails_roundtrip_by_mode() {
        assert_eq!(rt(b"853c49e6748fea9b.2545f491"), (PackMode::Hex4Bit, 2 + 12));
        assert_eq!(rt(b"853c49e6748fea9b2545f49"), (PackMode::Hex4Bit, 1 + 12));
        assert_eq!(rt(b".abc").0, PackMode::Hex4Bit);
        assert_eq!(rt(b"abc.").0, PackMode::Hex4Bit);
        assert_eq!(rt(b"a.b.c").0, PackMode::Base64Usenet);
        assert_eq!(rt(b"Xa9Bc-Zz.0q").0, PackMode::Base64Usenet);
        assert_eq!(rt(b"ab$cd").0, PackMode::Raw8Bit);
        assert_eq!(rt(b"").0, PackMode::Raw8Bit);
        assert_eq!(rt(b".").0, PackMode::Base64Usenet);
        let long: Vec<u8> = (0..200).map(|i| B64_CHAR[i % 64]).collect();
        assert_eq!(rt(&long).0, PackMode::Base64Usenet);
        let longhex: Vec<u8> = (0..130).map(|i| B64_CHAR[i % 16]).collect();
        assert_eq!(rt(&longhex).0, PackMode::Hex4Bit);
        let mut dotlate = longhex.clone();
        dotlate[100] = b'.';
        assert_eq!(rt(&dotlate).0, PackMode::Base64Usenet);
    }

    #[test]
    fn split() {
        let (l, d) = split_msgid(b"<part01.abc@news.example>").unwrap();
        assert_eq!((l, d), (&b"part01.abc"[..], &b"news.example"[..]));
        assert_eq!(split_prefix(l), (&b"part01."[..], &b"abc"[..]));
        assert_eq!(split_prefix(b"853c49e6748fea9b.2545f491").0, b"");
        assert!(split_msgid(b"no-brackets@x").is_none());
    }
}
