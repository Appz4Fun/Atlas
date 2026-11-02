//! Deflate64, PKWARE's deflate with a 64 KiB window: distance codes 30 and 31, and length code 285 taking 16 extra
//! bits. Written the way zlib's `puff` is, decoding a symbol a bit at a time against the canonical code's counts.
//! Plain deflate goes through flate2; this is only for what it can't read.

use super::super::lzma::End;

struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
    buf: u64,
    count: u32,
}

impl Bits<'_> {
    fn need(&mut self, n: u32) -> Option<()> {
        while self.count < n {
            let b = *self.data.get(self.pos)?;
            self.pos += 1;
            self.buf |= u64::from(b) << self.count;
            self.count += 8;
        }
        Some(())
    }

    fn bits(&mut self, n: u32) -> Option<u32> {
        if n == 0 {
            return Some(0);
        }
        self.need(n)?;
        let v = (self.buf & ((1u64 << n) - 1)) as u32;
        self.buf >>= n;
        self.count -= n;
        Some(v)
    }

    fn align(&mut self) {
        let drop = self.count % 8;
        self.buf >>= drop;
        self.count -= drop;
    }
}

/// A canonical Huffman code: how many codes of each length, and the symbols in code order.
struct Huffman {
    count: [u16; 16],
    symbol: Vec<u16>,
}

impl Huffman {
    /// None: more codes than the lengths have room for.
    fn new(lengths: &[u8]) -> Option<Huffman> {
        let mut count = [0u16; 16];
        for &l in lengths {
            count[usize::from(l)] += 1;
        }
        let mut left = 1i32;
        for c in &count[1..] {
            left = (left << 1) - i32::from(*c);
            if left < 0 {
                return None;
            }
        }
        let mut offs = [0u16; 16];
        for len in 1..15 {
            offs[len + 1] = offs[len] + count[len];
        }
        let mut symbol = vec![0u16; lengths.len()];
        for (s, &l) in lengths.iter().enumerate() {
            if l != 0 {
                symbol[usize::from(offs[usize::from(l)])] = s as u16;
                offs[usize::from(l)] += 1;
            }
        }
        Some(Huffman { count, symbol })
    }

    fn decode(&self, bits: &mut Bits<'_>) -> Option<u16> {
        let (mut code, mut first, mut index) = (0i32, 0i32, 0i32);
        for len in 1..16 {
            code |= bits.bits(1)? as i32;
            let count = i32::from(self.count[len]);
            if code - count < first {
                return self.symbol.get((index + code - first) as usize).copied();
            }
            index += count;
            first = (first + count) << 1;
            code <<= 1;
        }
        None
    }
}

const LEN_BASE: [u16; 29] =
    [3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 3];
const LEN_EXTRA: [u8; 29] = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 16];
const DIST_BASE: [u32; 32] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537, 2049, 3073, 4097, 6145,
    8193, 12289, 16385, 24577, 32769, 49153,
];
const DIST_EXTRA: [u8; 32] =
    [0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13, 14, 14];

/// A deflate64 stream from the front, to `limit` bytes.
pub fn deflate64(input: &[u8], limit: usize) -> (Vec<u8>, End) {
    let mut out = Vec::new();
    let mut bits = Bits { data: input, pos: 0, buf: 0, count: 0 };
    let end = run(&mut bits, &mut out, limit).unwrap_or(End::Cut);
    out.truncate(limit);
    (out, end)
}

fn run(bits: &mut Bits<'_>, out: &mut Vec<u8>, limit: usize) -> Option<End> {
    loop {
        let last = bits.bits(1)?;
        match bits.bits(2)? {
            0 => {
                bits.align();
                let len = bits.bits(16)?;
                let nlen = bits.bits(16)?;
                if len != !nlen & 0xffff {
                    return None;
                }
                for _ in 0..len {
                    out.push(bits.bits(8)? as u8);
                }
            }
            1 => {
                let mut lengths = [0u8; 288 + 32];
                lengths[..144].fill(8);
                lengths[144..256].fill(9);
                lengths[256..280].fill(7);
                lengths[280..288].fill(8);
                lengths[288..].fill(5);
                let lit = Huffman::new(&lengths[..288])?;
                let dist = Huffman::new(&lengths[288..])?;
                if codes(bits, out, &lit, &dist, limit)? {
                    return Some(End::Limit);
                }
            }
            2 => {
                let (lit, dist) = dynamic(bits)?;
                if codes(bits, out, &lit, &dist, limit)? {
                    return Some(End::Limit);
                }
            }
            _ => return None,
        }
        if out.len() >= limit {
            return Some(End::Limit);
        }
        if last == 1 {
            return Some(End::Done);
        }
    }
}

fn dynamic(bits: &mut Bits<'_>) -> Option<(Huffman, Huffman)> {
    const ORDER: [usize; 19] = [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];
    let nlen = bits.bits(5)? as usize + 257;
    let ndist = bits.bits(5)? as usize + 1;
    let ncode = bits.bits(4)? as usize + 4;
    if nlen > 288 || ndist > 32 {
        return None;
    }
    let mut lengths = [0u8; 288 + 32];
    for &i in &ORDER[..ncode] {
        lengths[i] = bits.bits(3)? as u8;
    }
    let lencode = Huffman::new(&lengths[..19])?;
    lengths = [0u8; 288 + 32];
    let mut i = 0;
    while i < nlen + ndist {
        let sym = lencode.decode(bits)?;
        if sym < 16 {
            lengths[i] = sym as u8;
            i += 1;
            continue;
        }
        let (value, times) = match sym {
            16 => (*lengths.get(i.checked_sub(1)?)?, 3 + bits.bits(2)?),
            17 => (0, 3 + bits.bits(3)?),
            _ => (0, 11 + bits.bits(7)?),
        };
        for _ in 0..times {
            *lengths.get_mut(i).filter(|_| i < nlen + ndist)? = value;
            i += 1;
        }
    }
    if lengths[256] == 0 {
        return None;
    }
    Some((Huffman::new(&lengths[..nlen])?, Huffman::new(&lengths[nlen..nlen + ndist])?))
}

/// One block's symbols. Some(true): the limit was reached.
fn codes(bits: &mut Bits<'_>, out: &mut Vec<u8>, lit: &Huffman, dist: &Huffman, limit: usize) -> Option<bool> {
    loop {
        if out.len() >= limit {
            return Some(true);
        }
        let sym = usize::from(lit.decode(bits)?);
        if sym < 256 {
            out.push(sym as u8);
            continue;
        }
        if sym == 256 {
            return Some(false);
        }
        let s = sym.checked_sub(257).filter(|s| *s < 29)?;
        let len = usize::from(LEN_BASE[s]) + bits.bits(u32::from(LEN_EXTRA[s]))? as usize;
        let d = usize::from(dist.decode(bits)?);
        let back = (*DIST_BASE.get(d)? + bits.bits(u32::from(DIST_EXTRA[d]))?) as usize;
        if back > out.len() {
            return None;
        }
        for _ in 0..len.min(limit - out.len()) {
            out.push(out[out.len() - back]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_what_the_inflate64_package_made() {
        let want = include_bytes!("../../../tests/fixtures/nest/mixed.bin");
        let packed = include_bytes!("../../../tests/fixtures/nest/mixed.deflate64");
        let (out, end) = deflate64(packed, usize::MAX);
        assert_eq!(end, End::Done);
        assert!(out == want, "{} bytes", out.len());
        let (front, end) = deflate64(&packed[..packed.len() / 2], usize::MAX);
        assert_eq!(end, End::Cut);
        assert!(front.len() > 1000 && want.starts_with(&front));
    }

    #[test]
    fn length_285_is_sixteen_extra_bits() {
        // fixed block: literal 'a', then length code 285 with extra 997 (length 1000), distance code 0 (1)
        let mut w = Writer::default();
        w.put(1, 1);
        w.put(1, 2);
        w.code(0x30 + u32::from(b'a'), 8);
        w.code(0xc0 + (285 - 280), 8);
        w.put(997, 16);
        w.code(0, 5);
        w.code(0, 7);
        let (out, end) = deflate64(&w.bytes(), usize::MAX);
        assert_eq!((out.len(), end), (1001, End::Done));
        assert!(out.iter().all(|b| *b == b'a'));
    }

    #[derive(Default)]
    struct Writer {
        out: Vec<u8>,
        bit: u32,
    }

    impl Writer {
        fn put(&mut self, v: u32, n: u32) {
            for i in 0..n {
                if self.bit.is_multiple_of(8) {
                    self.out.push(0);
                }
                *self.out.last_mut().unwrap() |= (((v >> i) & 1) as u8) << (self.bit % 8);
                self.bit += 1;
            }
        }
        /// Huffman codes go most significant bit first.
        fn code(&mut self, v: u32, n: u32) {
            for i in (0..n).rev() {
                self.put((v >> i) & 1, 1);
            }
        }
        fn bytes(self) -> Vec<u8> {
            self.out
        }
    }

    #[test]
    fn junk_never_panics() {
        let mut seed = 3u32;
        for n in 0..3000 {
            let data: Vec<u8> = (0..(n % 200))
                .map(|_| {
                    seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
                    (seed >> 16) as u8
                })
                .collect();
            let _ = deflate64(&data, 1 << 16);
        }
    }
}
