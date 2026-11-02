//! What RAR's decoders since 2.0 share: big-endian bits, and Huffman tables in RAR's form.

/// Big-endian bits from a byte slice, past whose end reads give zeros: the caller checks `past_end`.
pub struct Bits<'a> {
    pub data: &'a [u8],
    pub addr: usize,
    pub bit: u32,
}

impl Bits<'_> {
    fn byte(&self, at: usize) -> u32 {
        u32::from(self.data.get(at).copied().unwrap_or(0))
    }

    /// The next 16 bits, not consumed.
    pub fn peek16(&self) -> u32 {
        let v = (self.byte(self.addr) << 16) | (self.byte(self.addr + 1) << 8) | self.byte(self.addr + 2);
        (v >> (8 - self.bit)) & 0xffff
    }

    /// The next 32 bits, not consumed.
    pub fn peek32(&self) -> u32 {
        let v = (self.byte(self.addr) << 24)
            | (self.byte(self.addr + 1) << 16)
            | (self.byte(self.addr + 2) << 8)
            | self.byte(self.addr + 3);
        (v << self.bit) | (self.byte(self.addr + 4) >> (8 - self.bit))
    }

    /// The next 64 bits, not consumed.
    pub fn peek64(&self) -> u64 {
        let mut v = 0u64;
        for i in 0..8 {
            v = (v << 8) | u64::from(self.byte(self.addr + i));
        }
        (v << self.bit) | (u64::from(self.byte(self.addr + 8)) >> (8 - self.bit))
    }

    pub fn skip(&mut self, n: u32) {
        let total = self.bit + n;
        self.addr += (total >> 3) as usize;
        self.bit = total & 7;
    }

    pub fn take(&mut self, n: u32) -> u32 {
        let v = self.peek16() >> (16 - n);
        self.skip(n);
        v
    }

    pub fn align(&mut self) {
        if self.bit != 0 {
            self.addr += 1;
            self.bit = 0;
        }
    }

    pub fn past_end(&self) -> bool {
        self.addr > self.data.len() || (self.addr == self.data.len() && self.bit > 0)
    }
}

/// A canonical Huffman table in RAR's form: thresholds per length, and the symbols in code order.
#[derive(Default)]
pub struct Table {
    decode_len: [u32; 16],
    decode_pos: [u32; 16],
    decode_num: Vec<u16>,
}

impl Table {
    pub fn new(lengths: &[u8]) -> Option<Table> {
        let mut count = [0u32; 16];
        for &l in lengths {
            count[usize::from(l & 15)] += 1;
        }
        count[0] = 0;
        let mut t = Table { decode_num: vec![0; lengths.len()], ..Table::default() };
        let mut upper = 0u32;
        for i in 1..16 {
            upper += count[i];
            t.decode_len[i] = upper << (16 - i);
            t.decode_pos[i] = t.decode_pos[i - 1] + count[i - 1];
            upper <<= 1;
        }
        if upper > 1 << 16 {
            return None;
        }
        let mut next = t.decode_pos;
        for (s, &l) in lengths.iter().enumerate() {
            let l = usize::from(l & 15);
            if l > 0 {
                t.decode_num[next[l] as usize] = s as u16;
                next[l] += 1;
            }
        }
        Some(t)
    }

    pub fn decode(&self, bits: &mut Bits<'_>) -> u16 {
        let field = bits.peek16() & 0xfffe;
        let mut len = 15;
        for i in 1..15 {
            if field < self.decode_len[i] {
                len = i;
                break;
            }
        }
        bits.skip(len as u32);
        let dist = (field - self.decode_len[len - 1]) >> (16 - len);
        let pos = (self.decode_pos[len] + dist) as usize;
        self.decode_num.get(pos).copied().unwrap_or(0)
    }
}
