//! GF(2) polynomial linear algebra and CRC-32 folding.
//!
//! Provides O(log n) CRC32 combination and PAR2 IFSC slice unzeroing over
//! the Galois Field GF(2) modulo the IEEE 802.3 generator polynomial
//! P(x) = 0xEDB88320 (bit-reflected representation).

use std::sync::LazyLock;

/// Generator polynomial for standard IEEE 802.3 CRC-32 (bit-reflected).
pub const GF2_POLY: u32 = 0xEDB8_8320;

/// Multiplicative group order for GF(2^32) with primitive polynomial P(x): 2^32 - 1.
pub const GF2_ORDER: u64 = (1u64 << 32) - 1;

/// Multiply polynomial `a(x)` by `b(x)` modulo `P(x) = 0xEDB88320` in bit-reflected representation.
#[inline]
pub fn multmodp(a: u32, mut b: u32) -> u32 {
    let mut m = 1u32 << 31;
    let mut p = 0u32;
    loop {
        if (a & m) != 0 {
            p ^= b;
            if (a & (m.wrapping_sub(1))) == 0 {
                break;
            }
        }
        m >>= 1;
        if m == 0 {
            break;
        }
        b = if (b & 1) != 0 { (b >> 1) ^ GF2_POLY } else { b >> 1 };
    }
    p
}

/// Table of powers x^(2^n) mod P(x) for 0 <= n < 32.
static X2N_TABLE: LazyLock<[u32; 32]> = LazyLock::new(|| {
    let mut table = [0u32; 32];
    let mut p = 1u32 << 30; // x^1
    table[0] = p;
    for slot in table.iter_mut().skip(1) {
        p = multmodp(p, p);
        *slot = p;
    }
    table
});

/// Return x^(n * 2^k) modulo P(x).
#[inline]
pub fn x2nmodp(mut n: u64, mut k: u32) -> u32 {
    let mut p = 1u32 << 31; // x^0 == 1
    while n != 0 {
        if (n & 1) != 0 {
            p = multmodp(X2N_TABLE[(k & 31) as usize], p);
        }
        n >>= 1;
        k = k.wrapping_add(1);
    }
    p
}

/// Return x^n modulo P(x) where n is in bits.
#[inline]
pub fn xpow(n_bits: u64) -> u32 {
    x2nmodp(n_bits, 0)
}

/// Combine two contiguous CRC32 checksums: CRC(A || B) where len2 = |B| in bytes.
#[inline]
pub fn crc32_combine(crc1: u32, crc2: u32, len2: usize) -> u32 {
    if len2 == 0 {
        return crc1;
    }
    let op = x2nmodp(len2 as u64, 3);
    multmodp(op, crc1) ^ crc2
}

/// Compute CRC32 of `len` consecutive zero bytes in O(log len) time without allocations.
#[inline]
pub fn crc32_zeros(len: usize) -> u32 {
    if len == 0 {
        0
    } else {
        let op = x2nmodp(len as u64, 3);
        multmodp(op, 0xFFFF_FFFF) ^ 0xFFFF_FFFF
    }
}

/// Unzero a zero-padded CRC32 value: given CRC(data || 0^zero_len), recover CRC(data).
///
/// Uses polynomial division by x^(8 * zero_len) mod P(x) via modular inverse exponentiation.
#[inline]
pub fn crc32_unzero(padded_crc: u32, zero_len: usize) -> u32 {
    if zero_len == 0 {
        return padded_crc;
    }
    let k = zero_len as u64;
    let zero_crc = crc32_zeros(zero_len);
    let inv_bits = (GF2_ORDER - ((k.wrapping_mul(8)) % GF2_ORDER)) % GF2_ORDER;
    let inv_op = x2nmodp(inv_bits, 0);
    multmodp(inv_op, padded_crc ^ zero_crc)
}

/// Alias for `crc32_unzero` matching SABnzbd nomenclature.
#[inline]
pub fn crc32_zero_unpad(padded_crc: u32, zero_len: usize) -> u32 {
    crc32_unzero(padded_crc, zero_len)
}

/// Compute standard IEEE 802.3 32-bit CRC over bytes.
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        let idx = ((crc ^ (b as u32)) & 0xFF) as usize;
        let mut p = idx as u32;
        for _ in 0..8 {
            p = if (p & 1) != 0 { (p >> 1) ^ GF2_POLY } else { p >> 1 };
        }
        crc = (crc >> 8) ^ p;
    }
    crc ^ 0xFFFF_FFFF
}

/// Errors occurring during yEnc part folding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FoldError {
    Empty,
    MissingPart(usize),
    CorruptPart(usize),
    LengthMismatch,
}

impl std::fmt::Display for FoldError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FoldError::Empty => write!(f, "No parts provided for folding"),
            FoldError::MissingPart(i) => write!(f, "Missing part {i} in sequence"),
            FoldError::CorruptPart(i) => write!(f, "Corrupted part {i} in sequence"),
            FoldError::LengthMismatch => write!(f, "Part length mismatch"),
        }
    }
}

impl std::error::Error for FoldError {}

/// Sequentially fold contiguous yEnc parts `[(pcrc32, len)]` into a whole-file CRC32.
pub fn fold_yenc_parts(parts: &[(u32, usize)]) -> Result<u32, FoldError> {
    if parts.is_empty() {
        return Err(FoldError::Empty);
    }
    let mut acc = parts[0].0;
    for &(crc, len) in &parts[1..] {
        acc = crc32_combine(acc, crc, len);
    }
    Ok(acc)
}

/// Fold indexed yEnc parts `[(part_number, pcrc32, len)]` checking for sequence completeness out of `total`.
pub fn fold_yenc_indexed_parts(total: usize, parts: &[(usize, u32, usize)]) -> Result<u32, FoldError> {
    if total == 0 || parts.is_empty() {
        return Err(FoldError::Empty);
    }
    let mut map = std::collections::BTreeMap::new();
    for &(num, crc, len) in parts {
        map.insert(num, (crc, len));
    }
    let mut ordered = Vec::with_capacity(total);
    for i in 1..=total {
        match map.get(&i) {
            Some(&entry) => ordered.push(entry),
            None => return Err(FoldError::MissingPart(i)),
        }
    }
    fold_yenc_parts(&ordered)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw_crc(data: &[u8]) -> u32 {
        let mut crc = 0xFFFF_FFFFu32;
        for &b in data {
            let idx = ((crc ^ (b as u32)) & 0xFF) as usize;
            // standard byte table entry
            let mut p = idx as u32;
            for _ in 0..8 {
                p = if (p & 1) != 0 { (p >> 1) ^ GF2_POLY } else { p >> 1 };
            }
            crc = (crc >> 8) ^ p;
        }
        crc ^ 0xFFFF_FFFF
    }

    #[test]
    fn combine_two_parts() {
        let a = b"Hello, world! 1234567890";
        let b = b" Testing crc32 combine with arbitrary length!";
        let full = [a.as_slice(), b.as_slice()].concat();

        let c_a = raw_crc(a);
        let c_b = raw_crc(b);
        let c_full = raw_crc(&full);

        assert_eq!(crc32_combine(c_a, c_b, b.len()), c_full);
    }

    #[test]
    fn zero_unpadding() {
        let data = b"This is tail data that gets padded by PAR2 IFSC.";
        let zeros = [0u8; 128];
        let padded = [data.as_slice(), &zeros].concat();

        let c_data = raw_crc(data);
        let c_padded = raw_crc(&padded);

        assert_eq!(crc32_unzero(c_padded, zeros.len()), c_data);
        assert_eq!(crc32_unzero(c_data, 0), c_data);
    }

    #[test]
    fn fold_multipart() {
        let parts: Vec<Vec<u8>> = (0..5).map(|i| vec![i as u8; 1000 + i * 200]).collect();
        let full: Vec<u8> = parts.concat();

        let crc_parts: Vec<(u32, usize)> = parts.iter().map(|p| (raw_crc(p), p.len())).collect();
        let folded = fold_yenc_parts(&crc_parts).unwrap();
        assert_eq!(folded, raw_crc(&full));
    }

    #[test]
    fn gigabyte_scale_unzero_instant() {
        let c = 0x1234_5678;
        // 1 GB zeros exponentiation in microseconds
        let unzeroed = crc32_unzero(c, 1_073_741_824);
        assert_ne!(unzeroed, 0);
    }

    #[test]
    fn polynomial_shifting_invariance() {
        let c = 0x9ABC_DEF0;
        let l1 = 1234usize;
        let l2 = 5678usize;
        let c_step = crc32_combine(crc32_combine(c, 0, l1), 0, l2);
        let c_direct = crc32_combine(c, 0, l1 + l2);
        assert_eq!(c_step, c_direct);
    }

    #[test]
    fn missing_intermediate_part() {
        let parts = vec![(1, 0x1111, 100), (2, 0x2222, 100), (4, 0x4444, 100)];
        assert_eq!(fold_yenc_indexed_parts(4, &parts), Err(FoldError::MissingPart(3)));
    }
}
