//! ZigZag + LEB128 varints and allocation-free ASCII integer writers.

#[inline(always)]
pub fn zigzag_encode(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}

#[inline(always)]
pub fn zigzag_decode(v: u64) -> i64 {
    ((v >> 1) as i64) ^ -((v & 1) as i64)
}

#[inline(always)]
pub fn write_varint(mut v: u64, out: &mut Vec<u8>) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

/// Reads one LEB128 varint at `*pos`, advancing it. The caller guarantees the
/// bytes were written by [`write_varint`]; a truncated stream returns what was
/// read so far instead of reading out of bounds.
#[inline(always)]
pub fn read_varint_unchecked(buf: &[u8], pos: &mut usize) -> u64 {
    let mut p = *pos;
    // fast path: one byte
    if let Some(&b) = buf.get(p)
        && b < 0x80
    {
        *pos = p + 1;
        return b as u64;
    }
    let mut v = 0u64;
    let mut shift = 0u32;
    while let Some(&b) = buf.get(p) {
        p += 1;
        if shift < 64 {
            v |= ((b & 0x7F) as u64) << shift;
        }
        if b < 0x80 {
            break;
        }
        shift += 7;
    }
    *pos = p;
    v
}

/// Checked read for untrusted input: None on truncation or overflow.
pub fn read_varint(buf: &[u8], pos: &mut usize) -> Option<u64> {
    let mut v = 0u64;
    let mut shift = 0u32;
    let mut p = *pos;
    loop {
        let b = *buf.get(p)?;
        p += 1;
        if shift >= 64 || (shift == 63 && b > 1) {
            return None;
        }
        v |= ((b & 0x7F) as u64) << shift;
        if b < 0x80 {
            *pos = p;
            return Some(v);
        }
        shift += 7;
    }
}

const DIGITS2: &[u8; 200] = b"0001020304050607080910111213141516171819\
2021222324252627282930313233343536373839\
4041424344454647484950515253545556575859\
6061626364656667686970717273747576777879\
8081828384858687888990919293949596979899";

/// Writes `v` in decimal into the tail of `buf`, returning the start index.
#[inline(always)]
fn fmt_u64(mut v: u64, buf: &mut [u8; 20]) -> usize {
    let mut i = 20;
    while v >= 100 {
        let d = ((v % 100) as usize) * 2;
        v /= 100;
        i -= 2;
        buf[i] = DIGITS2[d];
        buf[i + 1] = DIGITS2[d + 1];
    }
    if v >= 10 {
        let d = (v as usize) * 2;
        i -= 2;
        buf[i] = DIGITS2[d];
        buf[i + 1] = DIGITS2[d + 1];
    } else {
        i -= 1;
        buf[i] = b'0' + v as u8;
    }
    i
}

#[inline(always)]
pub fn write_u64_ascii(v: u64, out: &mut Vec<u8>) {
    let mut buf = [0u8; 20];
    let start = fmt_u64(v, &mut buf);
    out.extend_from_slice(&buf[start..]);
}

#[inline(always)]
pub fn write_i64_ascii(v: i64, out: &mut Vec<u8>) {
    if v < 0 {
        out.push(b'-');
    }
    write_u64_ascii(v.unsigned_abs(), out);
}

/// Decimal `val`, left-padded with zeros to `pad_width` digits (0 = natural width).
#[inline(always)]
pub fn write_padded_u32(val: u32, pad_width: usize, out: &mut Vec<u8>) {
    let mut buf = [0u8; 20];
    let start = fmt_u64(val as u64, &mut buf);
    let len = 20 - start;
    for _ in len..pad_width {
        out.push(b'0');
    }
    out.extend_from_slice(&buf[start..]);
}

/// Parses a canonical unsigned decimal (no sign, no leading zeros unless "0",
/// at most `max_digits`). Returns None for anything that would not print back
/// byte-identically.
#[inline]
pub fn parse_canonical_u64(s: &[u8], max_digits: usize) -> Option<u64> {
    if s.is_empty() || s.len() > max_digits || (s.len() > 1 && s[0] == b'0') {
        return None;
    }
    let mut v = 0u64;
    for &b in s {
        let d = b.wrapping_sub(b'0');
        if d > 9 {
            return None;
        }
        v = v.checked_mul(10)?.checked_add(d as u64)?;
    }
    Some(v)
}

/// Canonical signed decimal: optional '-' (never "-0"), then canonical digits.
#[inline]
pub fn parse_canonical_i64(s: &[u8]) -> Option<i64> {
    if let Some(rest) = s.strip_prefix(b"-") {
        let v = parse_canonical_u64(rest, 18)?;
        if v == 0 {
            return None;
        }
        Some(-(v as i64))
    } else {
        Some(parse_canonical_u64(s, 18)? as i64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zigzag_and_varint_roundtrip() {
        for v in [0i64, 1, -1, 63, -64, 64, 1 << 40, i64::MIN, i64::MAX] {
            assert_eq!(zigzag_decode(zigzag_encode(v)), v);
            let mut buf = Vec::new();
            write_varint(zigzag_encode(v), &mut buf);
            let mut p = 0;
            assert_eq!(zigzag_decode(read_varint_unchecked(&buf, &mut p)), v);
            assert_eq!(p, buf.len());
            let mut p = 0;
            assert_eq!(read_varint(&buf, &mut p).map(zigzag_decode), Some(v));
        }
    }

    #[test]
    fn ascii_writers() {
        let mut o = Vec::new();
        write_u64_ascii(0, &mut o);
        o.push(b' ');
        write_u64_ascii(u64::MAX, &mut o);
        o.push(b' ');
        write_i64_ascii(-905, &mut o);
        o.push(b' ');
        write_padded_u32(7, 3, &mut o);
        o.push(b' ');
        write_padded_u32(1234, 2, &mut o);
        assert_eq!(o, b"0 18446744073709551615 -905 007 1234");
        assert_eq!(parse_canonical_u64(b"012", 9), None);
        assert_eq!(parse_canonical_i64(b"-0"), None);
        assert_eq!(parse_canonical_i64(b"-12"), Some(-12));
    }
}
