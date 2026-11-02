//! bzip2, a block at a time from the front. A block only comes out whole (it's one Burrows-Wheeler transform), soo a
//! stream cut short gives every block before the cut.

use super::super::lzma::End;

struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
    buf: u64,
    count: u32,
}

impl Bits<'_> {
    fn bits(&mut self, n: u32) -> Option<u32> {
        while self.count < n {
            let b = *self.data.get(self.pos)?;
            self.pos += 1;
            self.buf = (self.buf << 8) | u64::from(b);
            self.count += 8;
        }
        self.count -= n;
        Some(((self.buf >> self.count) & ((1u64 << n) - 1)) as u32)
    }

    fn bit(&mut self) -> Option<bool> {
        Some(self.bits(1)? == 1)
    }
}

const BLOCK_MAGIC: u64 = 0x3141_5926_5359;
const END_MAGIC: u64 = 0x1772_4538_5090;
const MAX_SELECTORS: usize = 18_002;

/// A `.bz2` stream (or several, one after another) from the front, to `limit` bytes.
pub fn bzip2(input: &[u8], limit: usize) -> (Vec<u8>, End) {
    let mut out = Vec::new();
    let mut bits = Bits { data: input, pos: 0, buf: 0, count: 0 };
    let end = streams(&mut bits, &mut out, limit).unwrap_or(End::Cut);
    out.truncate(limit);
    (out, end)
}

fn streams(bits: &mut Bits<'_>, out: &mut Vec<u8>, limit: usize) -> Option<End> {
    loop {
        if bits.bits(24)? != 0x42_5a_68 {
            return None;
        }
        let level = bits.bits(8)?;
        if !(u32::from(b'1')..=u32::from(b'9')).contains(&level) {
            return None;
        }
        let max = (level - u32::from(b'0')) as usize * 100_000;
        loop {
            let magic = (u64::from(bits.bits(24)?) << 24) | u64::from(bits.bits(24)?);
            let _crc = bits.bits(32)?;
            if magic == END_MAGIC {
                break;
            }
            if magic != BLOCK_MAGIC {
                return None;
            }
            block(bits, out, max)?;
            if out.len() >= limit {
                return Some(End::Limit);
            }
        }
        // a stream ends on a byte; another may follow
        bits.count -= bits.count % 8;
        if bits.pos >= bits.data.len() && bits.count == 0 {
            return Some(End::Done);
        }
    }
}

/// Canonical Huffman for one table: codes up to 20 bits.
struct Table {
    limit: [i32; 21],
    base: [i32; 21],
    perm: Vec<u16>,
    min: u32,
}

impl Table {
    fn new(lengths: &[u8]) -> Option<Table> {
        let min = u32::from(*lengths.iter().min()?);
        let max = u32::from(*lengths.iter().max()?);
        let mut perm = Vec::with_capacity(lengths.len());
        for len in min..=max {
            for (s, &l) in lengths.iter().enumerate() {
                if u32::from(l) == len {
                    perm.push(s as u16);
                }
            }
        }
        let mut limit = [i32::MAX; 21];
        let mut base = [0i32; 21];
        let (mut code, mut index) = (0i32, 0i32);
        for len in min..=max {
            let n = lengths.iter().filter(|l| u32::from(**l) == len).count() as i32;
            base[len as usize] = index - code;
            code += n;
            index += n;
            limit[len as usize] = code - 1;
            code <<= 1;
        }
        Some(Table { limit, base, perm, min })
    }

    fn decode(&self, bits: &mut Bits<'_>) -> Option<u16> {
        let mut len = self.min;
        let mut code = bits.bits(len)? as i32;
        while len <= 20 {
            if code <= self.limit[len as usize] {
                return self.perm.get((self.base[len as usize] + code) as usize).copied();
            }
            code = (code << 1) | bits.bits(1)? as i32;
            len += 1;
        }
        None
    }
}

fn block(bits: &mut Bits<'_>, out: &mut Vec<u8>, max: usize) -> Option<()> {
    if bits.bit()? {
        // randomised blocks: bzip2 stopped writing these in 0.9.5
        return None;
    }
    let orig_ptr = bits.bits(24)? as usize;
    let used_groups = bits.bits(16)?;
    let mut symbols = Vec::with_capacity(256);
    for i in 0..16 {
        if used_groups & (0x8000 >> i) != 0 {
            let used = bits.bits(16)?;
            for j in 0..16 {
                if used & (0x8000 >> j) != 0 {
                    symbols.push((i * 16 + j) as u8);
                }
            }
        }
    }
    if symbols.is_empty() {
        return None;
    }
    let alpha = symbols.len() + 2;
    let groups = bits.bits(3)? as usize;
    if !(2..=6).contains(&groups) {
        return None;
    }
    let count = bits.bits(15)? as usize;
    if count == 0 {
        return None;
    }
    let mut order: Vec<u8> = (0..groups as u8).collect();
    let mut selectors = Vec::with_capacity(count.min(MAX_SELECTORS));
    for _ in 0..count {
        let mut j = 0usize;
        while bits.bit()? {
            j += 1;
            if j >= groups {
                return None;
            }
        }
        let g = order.remove(j);
        order.insert(0, g);
        if selectors.len() < MAX_SELECTORS {
            selectors.push(g);
        }
    }
    let mut tables = Vec::with_capacity(groups);
    for _ in 0..groups {
        let mut len = bits.bits(5)? as i32;
        let mut lengths = vec![0u8; alpha];
        for l in &mut lengths {
            loop {
                if !(1..=20).contains(&len) {
                    return None;
                }
                if !bits.bit()? {
                    break;
                }
                len += if bits.bit()? { -1 } else { 1 };
            }
            *l = len as u8;
        }
        tables.push(Table::new(&lengths)?);
    }

    // Huffman, then runs of the front symbol, then move-to-front
    let eob = (alpha - 1) as u16;
    let mut mtf: Vec<u8> = (0..symbols.len()).map(|i| i as u8).collect();
    let mut data: Vec<u8> = Vec::new();
    let (mut run, mut weight) = (0usize, 1usize);
    let mut decoded = 0usize;
    loop {
        let table = &tables[usize::from(*selectors.get(decoded / 50)?)];
        decoded += 1;
        let sym = table.decode(bits)?;
        if sym <= 1 {
            run += weight << sym;
            weight <<= 1;
            if run > max {
                return None;
            }
            continue;
        }
        if run > 0 {
            if data.len() + run > max {
                return None;
            }
            let b = symbols[usize::from(mtf[0])];
            data.extend(std::iter::repeat_n(b, run));
            (run, weight) = (0, 1);
        }
        if sym == eob {
            break;
        }
        let at = usize::from(sym - 1);
        let v = *mtf.get(at)?;
        mtf.copy_within(0..at, 1);
        mtf[0] = v;
        if data.len() >= max {
            return None;
        }
        data.push(symbols[usize::from(v)]);
    }
    if orig_ptr >= data.len() {
        return None;
    }

    // the Burrows-Wheeler transform, undone
    let mut starts = [0usize; 256];
    for &b in &data {
        starts[usize::from(b)] += 1;
    }
    let mut sum = 0;
    for s in &mut starts {
        let n = *s;
        *s = sum;
        sum += n;
    }
    let mut next = vec![0u32; data.len()];
    for (i, &b) in data.iter().enumerate() {
        next[starts[usize::from(b)]] = i as u32;
        starts[usize::from(b)] += 1;
    }
    // then the first run-length step: four alike, then a count of more
    let mut p = next[orig_ptr] as usize;
    let (mut last, mut same) = (None, 0u8);
    for _ in 0..data.len() {
        let b = data[p];
        p = next[p] as usize;
        if same == 4 {
            out.extend(std::iter::repeat_n(last.unwrap_or(0), usize::from(b)));
            same = 0;
            last = None;
            continue;
        }
        if Some(b) == last {
            same += 1;
        } else {
            last = Some(b);
            same = 1;
        }
        out.push(b);
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_what_python_made() {
        let want = include_bytes!("../../../tests/fixtures/nest/mixed.bin");
        let packed = include_bytes!("../../../tests/fixtures/nest/mixed.bz2");
        let (out, end) = bzip2(packed, usize::MAX);
        assert_eq!(end, End::Done);
        assert!(out == want, "{} bytes", out.len());
        let (_, end) = bzip2(&packed[..packed.len() / 2], usize::MAX);
        assert_eq!(end, End::Cut);
        let (out, end) = bzip2(packed, 100);
        assert_eq!((out.len(), end), (100, End::Limit));
    }

    #[test]
    fn junk_never_panics() {
        let packed = include_bytes!("../../../tests/fixtures/nest/mixed.bz2");
        let mut seed = 5u32;
        for _ in 0..300 {
            let mut d = packed.to_vec();
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
            let at = 4 + (seed as usize >> 8) % (d.len() - 4);
            d[at] ^= (seed >> 3) as u8 | 1;
            let _ = bzip2(&d, 1 << 20);
        }
    }
}
