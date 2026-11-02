//! CRC-32 (IEEE 802.3, reflected, poly 0xEDB88320, init/xorout 0xFFFFFFFF)
//! arithmetic over GF(2). No dependencies.
//!
//! Representation: a 32-bit value is a polynomial of degree < 32 in
//! *reflected* order (bit 31 = x^0, bit 0 = x^31), reduced modulo P.
//! Key identity (as in zlib crc32_combine):
//!     crc(A || B) = (x^(8|B|) * crc(A)) mod P  XOR  crc(B)
//! It holds for the standard (pre/post inverted) CRC values because
//! crc(empty) = 0 and the init/xorout contributions cancel.

pub const POLY: u32 = 0xEDB8_8320;
const ONE: u32 = 1 << 31; // x^0
const X1: u32 = 1 << 30; // x^1

/// (a * b) mod P, reflected representation.
#[inline]
pub const fn mulmod(a: u32, mut b: u32) -> u32 {
    let mut m = ONE;
    let mut p = 0u32;
    loop {
        if a & m != 0 {
            p ^= b;
            if a & (m - 1) == 0 {
                break;
            }
        }
        m >>= 1;
        if m == 0 {
            break;
        }
        b = if b & 1 != 0 { (b >> 1) ^ POLY } else { b >> 1 };
    }
    p
}

/// R4[i] = (i as the x^28..x^31 coefficients) * x^4 mod P, reflected:
/// the reduction needed when shifting by four degrees.
const R4: [u32; 16] = {
    let mut t = [0u32; 16];
    let mut i = 0;
    while i < 16 {
        let mut c = i as u32;
        let mut j = 0;
        while j < 4 {
            c = if c & 1 != 0 { (c >> 1) ^ POLY } else { c >> 1 };
            j += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
};

/// (a * b) mod P using 4-bit windows (Horner over a's nibbles, from
/// x^28..x^31 down to x^0..x^3). Same result as `mulmod`; makes combine ~2x faster (measured).
#[inline]
pub fn mulmod_fast(a: u32, b: u32) -> u32 {
    // T[w] = (w as degree-<4 polynomial, w bit3 = x^0 ... bit0 = x^3) * b
    let b1 = if b & 1 != 0 { (b >> 1) ^ POLY } else { b >> 1 };
    let b2 = if b1 & 1 != 0 { (b1 >> 1) ^ POLY } else { b1 >> 1 };
    let b3 = if b2 & 1 != 0 { (b2 >> 1) ^ POLY } else { b2 >> 1 };
    let mut t = [0u32; 16];
    let mut w = 1;
    while w < 16 {
        let mut v = 0;
        if w & 8 != 0 { v ^= b; }
        if w & 4 != 0 { v ^= b1; }
        if w & 2 != 0 { v ^= b2; }
        if w & 1 != 0 { v ^= b3; }
        t[w] = v;
        w += 1;
    }
    let mut p = 0u32;
    // nibble k (bits 4k..4k+3) holds degrees 28-4k .. 31-4k; highest first.
    let mut k = 0;
    while k < 8 {
        p = (p >> 4) ^ R4[(p & 0xF) as usize];
        p ^= t[((a >> (4 * k)) & 0xF) as usize];
        k += 1;
    }
    p
}

/// X2N[k] = x^(2^k) mod P, k = 0..32. The multiplicative order of x
/// divides 2^32 - 1 (checked in tests), so x^(2^32) = x and the table
/// repeats with period 32.
const X2N: [u32; 32] = {
    let mut t = [0u32; 32];
    let mut p = X1;
    let mut k = 0;
    while k < 32 {
        t[k] = p;
        p = mulmod(p, p);
        k += 1;
    }
    t
};

/// x^-1 mod P. P = x*Q + 1 so x*Q == 1 (mod P), Q = (P - 1)/x.
/// In reflected form P without its x^32 term is POLY; (P-1)/x shifts the
/// coefficient vector down one degree, i.e. left by one in reflected bits,
/// with the x^32 term landing at x^31 (bit 0).
pub const XINV: u32 = (POLY << 1) | 1;

/// Inverse-power table: XINV2N[k] = x^-(2^k) mod P.
const XINV2N: [u32; 32] = {
    let mut t = [0u32; 32];
    let mut p = XINV;
    let mut k = 0;
    while k < 32 {
        t[k] = p;
        p = mulmod(p, p);
        k += 1;
    }
    t
};

/// x^(n * 2^k) mod P via square-and-multiply over the precomputed table.
#[inline]
fn pow_tab(tab: &[u32; 32], mut n: u64, mut k: u32) -> u32 {
    let mut p = ONE;
    while n != 0 {
        if n & 1 != 0 {
            p = mulmod_fast(tab[(k & 31) as usize], p);
        }
        n >>= 1;
        k += 1;
    }
    p
}

/// x^(8n) mod P  (shift operator for n bytes).
#[inline]
pub fn x8n(n: u64) -> u32 {
    pow_tab(&X2N, n, 3)
}

/// x^(-8n) mod P  (inverse shift for n bytes).
#[inline]
pub fn x8n_inv(n: u64) -> u32 {
    pow_tab(&XINV2N, n, 3)
}

/// crc(A || B) from crc(A), crc(B), |B|.
#[inline]
pub fn combine(crc_a: u32, crc_b: u32, len_b: u64) -> u32 {
    mulmod_fast(x8n(len_b), crc_a) ^ crc_b
}

/// crc(B) from crc(A || B), crc(A), |B|.
#[inline]
pub fn remove_prefix(crc_ab: u32, crc_a: u32, len_b: u64) -> u32 {
    mulmod_fast(x8n(len_b), crc_a) ^ crc_ab
}

/// crc(A) from crc(A || B), crc(B), |B|.
#[inline]
pub fn remove_suffix(crc_ab: u32, crc_b: u32, len_b: u64) -> u32 {
    mulmod_fast(x8n_inv(len_b), crc_ab ^ crc_b)
}

/// crc(D || 0^n) from crc(D).  crc(0^n) = ~(x^8n * ~0), hence
/// zero_extend(c, n) = ~(x^8n * ~c).
#[inline]
pub fn zero_extend(crc: u32, nzeros: u64) -> u32 {
    !mulmod_fast(x8n(nzeros), !crc)
}

/// Inverse of zero_extend: crc(D) from crc(D || 0^n).
#[inline]
pub fn zero_unpad(crc: u32, nzeros: u64) -> u32 {
    !mulmod_fast(x8n_inv(nzeros), !crc)
}

/// CRC of n zero bytes.
#[inline]
pub fn crc_zeros(n: u64) -> u32 {
    zero_extend(0, n)
}

/// Bytewise reference CRC-32 (std only; used for tests and small inputs).
pub fn crc32(data: &[u8]) -> u32 {
    crc32_update(0, data)
}

const TABLE: [u32; 256] = {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut j = 0;
        while j < 8 {
            c = if c & 1 != 0 { (c >> 1) ^ POLY } else { c >> 1 };
            j += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
};

pub fn crc32_update(crc: u32, data: &[u8]) -> u32 {
    let mut c = !crc;
    for &b in data {
        c = TABLE[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8);
    }
    !c
}

/// Memoizes x^(8n) for the last length seen: yEnc parts are almost all
/// the same article size, so folding costs one mulmod per part.
#[derive(Default)]
struct ShiftCache { len: u64, shift: u32, valid: bool }
impl ShiftCache {
    #[inline]
    fn get(&mut self, len: u64) -> u32 {
        if !self.valid || self.len != len {
            self.len = len;
            self.shift = x8n(len);
            self.valid = true;
        }
        self.shift
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FoldError {
    /// yEnc part i does not start right after part i-1 (or first != 1).
    Gap { index: usize, expected_begin: u64, got_begin: u64 },
    /// end < begin.
    BadRange { index: usize },
    /// A part straddles a PAR2 slice boundary.
    Straddle { index: usize },
    /// Wrong slice count for the file length.
    SliceCount { expected: u64, got: usize },
    ZeroSliceSize,
}

/// yEnc part: (begin, end, pcrc32), 1-based inclusive byte offsets as in
/// `=ypart begin= end=`. Returns (file_len, whole-file CRC).
pub fn fold_yenc_parts(parts: &[(u64, u64, u32)]) -> Result<(u64, u32), FoldError> {
    let mut crc = 0u32;
    let mut next = 1u64;
    let mut cache = ShiftCache::default();
    for (i, &(b, e, pc)) in parts.iter().enumerate() {
        if e < b {
            return Err(FoldError::BadRange { index: i });
        }
        if b != next {
            return Err(FoldError::Gap { index: i, expected_begin: next, got_begin: b });
        }
        let len = e - b + 1;
        crc = mulmod_fast(cache.get(len), crc) ^ pc;
        next = e + 1;
    }
    Ok((next - 1, crc))
}

/// Whole-file CRC from PAR2 IFSC slice CRCs. Every slice CRC covers
/// `slice_size` bytes; the last slice is zero-padded to `slice_size`,
/// so its CRC is unpadded first.
pub fn fold_par2_slices(file_len: u64, slice_size: u64, slice_crcs: &[u32]) -> Result<u32, FoldError> {
    if slice_size == 0 {
        return Err(FoldError::ZeroSliceSize);
    }
    let nslices = file_len.div_ceil(slice_size);
    if nslices != slice_crcs.len() as u64 {
        return Err(FoldError::SliceCount { expected: nslices, got: slice_crcs.len() });
    }
    if nslices == 0 {
        return Ok(0);
    }
    let last_len = file_len - (nslices - 1) * slice_size;
    let mut crc = 0u32;
    // Full slices: crc = crc * x^(8S) ^ c  (one shift constant reused).
    let shift = x8n(slice_size);
    for &c in &slice_crcs[..slice_crcs.len() - 1] {
        crc = mulmod_fast(shift, crc) ^ c;
    }
    let last = zero_unpad(slice_crcs[slice_crcs.len() - 1], slice_size - last_len);
    Ok(combine(crc, last, last_len))
}

/// PAR2 slice CRCs (last slice zero-padded) from yEnc part CRCs, when
/// every slice boundary coincides with a part boundary (e.g. S is a
/// multiple of the article size). Parts must be contiguous from byte 1.
pub fn slices_from_parts(parts: &[(u64, u64, u32)], slice_size: u64) -> Result<Vec<u32>, FoldError> {
    if slice_size == 0 {
        return Err(FoldError::ZeroSliceSize);
    }
    let mut out = Vec::new();
    let mut cur = 0u32; // CRC of bytes in current slice so far
    let mut fill = 0u64; // bytes in current slice so far
    let mut next = 1u64;
    let mut cache = ShiftCache::default();
    for (i, &(b, e, pc)) in parts.iter().enumerate() {
        if e < b {
            return Err(FoldError::BadRange { index: i });
        }
        if b != next {
            return Err(FoldError::Gap { index: i, expected_begin: next, got_begin: b });
        }
        let len = e - b + 1;
        if fill + len > slice_size {
            return Err(FoldError::Straddle { index: i });
        }
        cur = mulmod_fast(cache.get(len), cur) ^ pc;
        fill += len;
        if fill == slice_size {
            out.push(cur);
            cur = 0;
            fill = 0;
        }
        next = e + 1;
    }
    if fill > 0 {
        out.push(zero_extend(cur, slice_size - fill));
    }
    Ok(out)
}
