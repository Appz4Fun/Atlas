//! `Xref:` fields: the per-group article numbers come out into integer slots,
//! leaving a skeleton (`host a.b:\x01 c.d:\x01`) shared by the whole post.
//! The deltas (slot 0 against the row's own article number, the other slots
//! against the previous row) are taken by the block builder.

use crate::varint::{parse_canonical_u64, write_u64_ascii};

pub const SLOT_XREF_NUM: u8 = 0x01;
pub const MAX_XREF_GROUPS: usize = 8;
pub const XREF_PREFIX: &[u8] = b"Xref: ";

/// Writes the skeleton of `field` into `skel` (cleared first) and the numbers
/// into `nums`, returning how many slots it has. None when the field already
/// holds the placeholder byte.
pub fn extract_xref_skeleton(field: &[u8], skel: &mut Vec<u8>, nums: &mut [u64; MAX_XREF_GROUPS]) -> Option<usize> {
    skel.clear();
    let mut n = 0usize;
    let mut i = 0usize;
    while i < field.len() {
        let b = field[i];
        if b == SLOT_XREF_NUM {
            skel.clear();
            return None;
        }
        skel.push(b);
        i += 1;
        if b == b':' && n < MAX_XREF_GROUPS {
            let start = i;
            while i < field.len() && field[i].is_ascii_digit() {
                i += 1;
            }
            let bounded = i == field.len() || field[i] == b' ' || field[i] == b'\t';
            match parse_canonical_u64(&field[start..i], 18) {
                Some(v) if bounded => {
                    nums[n] = v;
                    n += 1;
                    skel.push(SLOT_XREF_NUM);
                }
                // not a number we can write back as it was: keep the digits
                _ => skel.extend_from_slice(&field[start..i]),
            }
        }
    }
    Some(n)
}

#[inline]
pub fn slot_count(skel: &[u8]) -> usize {
    skel.iter().filter(|&&b| b == SLOT_XREF_NUM).count()
}

#[inline]
pub fn render_xref(skel: &[u8], nums: &[u64], out: &mut Vec<u8>) {
    let mut rest = skel;
    let mut k = 0;
    while let Some(p) = rest.iter().position(|&b| b == SLOT_XREF_NUM) {
        out.extend_from_slice(&rest[..p]);
        write_u64_ascii(nums.get(k).copied().unwrap_or(0), out);
        k += 1;
        rest = &rest[p + 1..];
    }
    out.extend_from_slice(rest);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skeleton_roundtrip() {
        for f in [
            &b"Xref: news.server.com group.a:84500001 group.b:19200400"[..],
            b"news.server.com a:1 b:2 c:3 d:4 e:5 f:6 g:7 h:8 i:9 j:10",
            b"host a:0123 b:12x c: d:5\textra\tfield",
            b"",
        ] {
            let mut skel = Vec::new();
            let mut nums = [0u64; MAX_XREF_GROUPS];
            let n = extract_xref_skeleton(f, &mut skel, &mut nums).unwrap();
            assert_eq!(slot_count(&skel), n);
            let mut out = Vec::new();
            render_xref(&skel, &nums[..n], &mut out);
            assert_eq!(out, f);
        }
        let mut skel = Vec::new();
        let mut nums = [0u64; MAX_XREF_GROUPS];
        assert_eq!(extract_xref_skeleton(b"Xref: h a:5 b:7", &mut skel, &mut nums), Some(2));
        assert_eq!(skel, b"Xref: h a:\x01 b:\x01");
        assert_eq!(&nums[..2], &[5, 7]);
    }
}
