//! RAR 1.5's compression (version 15, RAR 1.5 to 2.0's -m1 to -m5 before 2.0's own), from the front of a file's
//! packed data: adaptive codes that reorder themselves by use, with a flags byte choosing literal or match. Written
//! from the format as unrar describes it.

use super::super::lzma::End;
use super::rarbits::{Bits, MARGIN};

const DEC_L1: &[u32] = &[0x8000, 0xa000, 0xc000, 0xd000, 0xe000, 0xea00, 0xee00, 0xf000, 0xf200, 0xf200, 0xffff];
const POS_L1: [u32; 13] = [0, 0, 0, 2, 3, 5, 7, 11, 16, 20, 24, 32, 32];
const DEC_L2: &[u32] = &[0xa000, 0xc000, 0xd000, 0xe000, 0xea00, 0xee00, 0xf000, 0xf200, 0xf240, 0xffff];
const POS_L2: [u32; 13] = [0, 0, 0, 0, 5, 7, 9, 13, 18, 22, 26, 34, 36];
const DEC_HF0: &[u32] = &[0x8000, 0xc000, 0xe000, 0xf200, 0xf200, 0xf200, 0xf200, 0xf200, 0xffff];
const POS_HF0: [u32; 13] = [0, 0, 0, 0, 0, 8, 16, 24, 33, 33, 33, 33, 33];
const DEC_HF1: &[u32] = &[0x2000, 0xc000, 0xe000, 0xf000, 0xf200, 0xf200, 0xf7e0, 0xffff];
const POS_HF1: [u32; 13] = [0, 0, 0, 0, 0, 0, 4, 44, 60, 76, 80, 80, 127];
const DEC_HF2: &[u32] = &[0x1000, 0x2400, 0x8000, 0xc000, 0xfa00, 0xffff, 0xffff, 0xffff];
const POS_HF2: [u32; 13] = [0, 0, 0, 0, 0, 0, 2, 7, 53, 117, 233, 0, 0];
const DEC_HF3: &[u32] = &[0x800, 0x2400, 0xee00, 0xfe80, 0xffff, 0xffff, 0xffff];
const POS_HF3: [u32; 13] = [0, 0, 0, 0, 0, 0, 0, 2, 16, 218, 251, 0, 0];
const DEC_HF4: &[u32] = &[0xff00, 0xffff, 0xffff, 0xffff, 0xffff, 0xffff];
const POS_HF4: [u32; 13] = [0, 0, 0, 0, 0, 0, 0, 0, 0, 255, 0, 0, 0];

type Code = (u32, &'static [u32], &'static [u32; 13]);
const L1: Code = (2, DEC_L1, &POS_L1);
const L2: Code = (3, DEC_L2, &POS_L2);
const HF0: Code = (4, DEC_HF0, &POS_HF0);
const HF1: Code = (5, DEC_HF1, &POS_HF1);
const HF2: Code = (5, DEC_HF2, &POS_HF2);
const HF3: Code = (6, DEC_HF3, &POS_HF3);
const HF4: Code = (8, DEC_HF4, &POS_HF4);

const SHORT_LEN1: [u32; 16] = [1, 3, 4, 4, 5, 6, 7, 8, 8, 4, 4, 5, 6, 6, 4, 0];
const SHORT_XOR1: [u32; 15] = [0, 0xa0, 0xd0, 0xe0, 0xf0, 0xf8, 0xfc, 0xfe, 0xff, 0xc0, 0x80, 0x90, 0x98, 0x9c, 0xb0];
const SHORT_LEN2: [u32; 16] = [2, 3, 3, 3, 4, 4, 5, 6, 6, 4, 4, 5, 6, 6, 4, 0];
const SHORT_XOR2: [u32; 15] = [0, 0x40, 0x60, 0xa0, 0xd0, 0xe0, 0xf0, 0xf8, 0xfc, 0xc0, 0x80, 0x90, 0x98, 0x9c, 0xb0];

struct State<'a> {
    bits: Bits<'a>,
    out: Vec<u8>,
    target: usize,
    ch_set: [u16; 256],
    ch_set_a: [u16; 256],
    ch_set_b: [u16; 256],
    ch_set_c: [u16; 256],
    n_to_pl: [u8; 256],
    n_to_pl_b: [u8; 256],
    n_to_pl_c: [u8; 256],
    avr_plc: u32,
    avr_plc_b: u32,
    avr_ln1: u32,
    avr_ln2: u32,
    avr_ln3: u32,
    num_huf: u32,
    buf60: u32,
    max_dist3: u32,
    nhfb: u32,
    nlzb: u32,
    flags_cnt: i32,
    flag_buf: u32,
    st_mode: bool,
    l_count: u32,
    old_dist: [u32; 4],
    old_dist_ptr: usize,
    last_dist: u32,
    last_len: u32,
}

/// One file's packed data, from the front, to `limit` bytes. `complete`: `data` is all of it, not a cut front.
pub fn rar15(data: &[u8], size: Option<u64>, limit: usize, complete: bool) -> (Vec<u8>, End) {
    // a cut stream shorter than the margin has no byte that's safe to decode from
    if !complete && data.len() < MARGIN {
        return (Vec::new(), End::Cut);
    }
    let target = size.and_then(|s| usize::try_from(s).ok()).unwrap_or(usize::MAX).min(limit);
    let mut st = State {
        bits: Bits { data, addr: 0, bit: 0 },
        out: Vec::new(),
        target,
        ch_set: [0; 256],
        ch_set_a: [0; 256],
        ch_set_b: [0; 256],
        ch_set_c: [0; 256],
        n_to_pl: [0; 256],
        n_to_pl_b: [0; 256],
        n_to_pl_c: [0; 256],
        avr_plc: 0x3500,
        avr_plc_b: 0,
        avr_ln1: 0,
        avr_ln2: 0,
        avr_ln3: 0,
        num_huf: 0,
        buf60: 0,
        max_dist3: 0x2001,
        nhfb: 0x80,
        nlzb: 0x80,
        flags_cnt: 0,
        flag_buf: 0,
        st_mode: false,
        l_count: 0,
        old_dist: [0; 4],
        old_dist_ptr: 0,
        last_dist: 0,
        last_len: 0,
    };
    for i in 0..256u16 {
        st.ch_set[usize::from(i)] = i << 8;
        st.ch_set_b[usize::from(i)] = i << 8;
        st.ch_set_a[usize::from(i)] = i;
        st.ch_set_c[usize::from(i)] = ((256 - i) & 0xff) << 8;
    }
    corr_huff(&mut st.ch_set_b, &mut st.n_to_pl_b);
    let safe = if complete { data.len() } else { data.len() - MARGIN };
    let mut end = End::Done;
    if target > 0 {
        st.get_flags();
        st.flags_cnt = 8;
    }
    while st.out.len() < target {
        if st.bits.addr > safe || st.bits.past_end() {
            end = End::Cut;
            break;
        }
        if st.st_mode {
            st.huff_decode();
            continue;
        }
        st.flags_cnt -= 1;
        if st.flags_cnt < 0 {
            st.get_flags();
            st.flags_cnt = 7;
        }
        if st.flag_buf & 0x80 != 0 {
            st.flag_buf <<= 1;
            if st.nlzb > st.nhfb {
                st.long_lz();
            } else {
                st.huff_decode();
            }
        } else {
            st.flag_buf <<= 1;
            st.flags_cnt -= 1;
            if st.flags_cnt < 0 {
                st.get_flags();
                st.flags_cnt = 7;
            }
            if st.flag_buf & 0x80 != 0 {
                st.flag_buf <<= 1;
                if st.nlzb > st.nhfb {
                    st.huff_decode();
                } else {
                    st.long_lz();
                }
            } else {
                st.flag_buf <<= 1;
                st.short_lz();
            }
        }
    }
    if matches!(end, End::Done) && st.out.len() >= limit {
        end = End::Limit;
    }
    st.out.truncate(target);
    (st.out, end)
}

fn corr_huff(set: &mut [u16; 256], to_place: &mut [u8; 256]) {
    for (n, c) in set.iter_mut().enumerate() {
        *c = (*c & !0xff) | (7 - (n / 32) as u16);
    }
    to_place.fill(0);
    for (i, p) in to_place.iter_mut().take(7).enumerate() {
        *p = ((7 - i) * 32) as u8;
    }
}

impl State<'_> {
    fn decode_num(&mut self, num: u32, code: Code) -> u32 {
        let (mut start, dec, pos) = code;
        let num = num & 0xfff0;
        let mut i = 0;
        while i < dec.len() && dec[i] <= num {
            start += 1;
            i += 1;
        }
        self.bits.skip(start);
        let base = if i > 0 { dec[i - 1] } else { 0 };
        ((num - base) >> (16 - start)) + pos.get(start as usize).copied().unwrap_or(0)
    }

    fn copy(&mut self, dist: u32, len: u32) {
        let dist = dist as usize;
        let n = (len as usize).min(self.target.saturating_sub(self.out.len()));
        if dist == 0 || dist > self.out.len() {
            self.out.extend(std::iter::repeat_n(0, n));
            return;
        }
        for _ in 0..n {
            self.out.push(self.out[self.out.len() - dist]);
        }
    }

    fn remember(&mut self, dist: u32, len: u32) {
        self.old_dist[self.old_dist_ptr] = dist;
        self.old_dist_ptr = (self.old_dist_ptr + 1) & 3;
        self.last_len = len;
        self.last_dist = dist;
    }

    fn short_lz(&mut self) {
        self.num_huf = 0;
        let mut field = self.bits.peek16();
        if self.l_count == 2 {
            self.bits.skip(1);
            if field >= 0x8000 {
                self.copy(self.last_dist, self.last_len);
                return;
            }
            field <<= 1;
            self.l_count = 0;
        }
        let field = (field >> 8) & 0xff;
        let (lens, xors) = if self.avr_ln1 < 37 { (SHORT_LEN1, SHORT_XOR1) } else { (SHORT_LEN2, SHORT_XOR2) };
        let special = if self.avr_ln1 < 37 { 1 } else { 3 };
        let len_of = |i: usize| if i == special { self.buf60 + 3 } else { lens[i] };
        let mut length = 0usize;
        while length < xors.len() && ((field ^ xors[length]) & !(0xff >> len_of(length)) & 0xff) != 0 {
            length += 1;
        }
        if length == xors.len() {
            // no code matches: only damaged data gets here
            self.bits.addr = usize::MAX / 2;
            return;
        }
        let n = len_of(length);
        self.bits.skip(n);
        let mut length = length as u32;
        if length >= 9 {
            if length == 9 {
                self.l_count += 1;
                self.copy(self.last_dist, self.last_len);
                return;
            }
            if length == 14 {
                self.l_count = 0;
                let field = self.bits.peek16();
                let len = self.decode_num(field, L2) + 5;
                let dist = (self.bits.peek16() >> 1) | 0x8000;
                self.bits.skip(15);
                self.last_len = len;
                self.last_dist = dist;
                self.copy(dist, len);
                return;
            }
            self.l_count = 0;
            let saved = length;
            let dist = self.old_dist[self.old_dist_ptr.wrapping_sub((length - 9) as usize) & 3];
            let field = self.bits.peek16();
            length = self.decode_num(field, L1) + 2;
            if length == 0x101 && saved == 10 {
                self.buf60 ^= 1;
                return;
            }
            if dist > 256 {
                length += 1;
            }
            if dist >= self.max_dist3 {
                length += 1;
            }
            self.remember(dist, length);
            self.copy(dist, length);
            return;
        }
        self.l_count = 0;
        self.avr_ln1 += length;
        self.avr_ln1 -= self.avr_ln1 >> 4;
        let field = self.bits.peek16();
        let place = (self.decode_num(field, HF2) & 0xff) as usize;
        let mut dist = u32::from(self.ch_set_a[place]);
        if place > 0 {
            let last = self.ch_set_a[place - 1];
            self.ch_set_a[place] = last;
            self.ch_set_a[place - 1] = dist as u16;
        }
        length += 2;
        dist += 1;
        self.remember(dist, length);
        self.copy(dist, length);
    }

    fn long_lz(&mut self) {
        self.num_huf = 0;
        self.nlzb += 16;
        if self.nlzb > 0xff {
            self.nlzb = 0x90;
            self.nhfb >>= 1;
        }
        let old_avr2 = self.avr_ln2;
        let field = self.bits.peek16();
        let mut length = if self.avr_ln2 >= 122 {
            self.decode_num(field, L2)
        } else if self.avr_ln2 >= 64 {
            self.decode_num(field, L1)
        } else if field < 0x100 {
            self.bits.skip(16);
            field
        } else {
            let n = (field << 16).leading_zeros();
            self.bits.skip(n + 1);
            n
        };
        self.avr_ln2 += length;
        self.avr_ln2 -= self.avr_ln2 >> 5;
        let field = self.bits.peek16();
        let place = if self.avr_plc_b > 0x28ff {
            self.decode_num(field, HF2)
        } else if self.avr_plc_b > 0x6ff {
            self.decode_num(field, HF1)
        } else {
            self.decode_num(field, HF0)
        };
        self.avr_plc_b += place;
        self.avr_plc_b -= self.avr_plc_b >> 8;
        let place = (place & 0xff) as usize;
        let (mut dist, mut new_place);
        loop {
            dist = u32::from(self.ch_set_b[place]);
            let slot = &mut self.n_to_pl_b[(dist & 0xff) as usize];
            new_place = usize::from(*slot);
            *slot = slot.wrapping_add(1);
            dist += 1;
            if dist & 0xff == 0 {
                corr_huff(&mut self.ch_set_b, &mut self.n_to_pl_b);
            } else {
                break;
            }
        }
        self.ch_set_b[place] = self.ch_set_b[new_place];
        self.ch_set_b[new_place] = dist as u16;
        let dist = ((dist & 0xff00) | (self.bits.peek16() >> 8)) >> 1;
        self.bits.skip(7);
        let old_avr3 = self.avr_ln3;
        if length != 1 && length != 4 {
            if length == 0 && dist <= self.max_dist3 {
                self.avr_ln3 += 1;
                self.avr_ln3 -= self.avr_ln3 >> 8;
            } else if self.avr_ln3 > 0 {
                self.avr_ln3 -= 1;
            }
        }
        length += 3;
        if dist >= self.max_dist3 {
            length += 1;
        }
        if dist <= 256 {
            length += 8;
        }
        self.max_dist3 = if old_avr3 > 0xb0 || (self.avr_plc >= 0x2a00 && old_avr2 < 0x40) { 0x7f00 } else { 0x2001 };
        self.remember(dist, length);
        self.copy(dist, length);
    }

    fn huff_decode(&mut self) {
        let field = self.bits.peek16();
        let code = match self.avr_plc {
            0x7600.. => HF4,
            0x5e00.. => HF3,
            0x3600.. => HF2,
            0x0e00.. => HF1,
            _ => HF0,
        };
        let mut place = (self.decode_num(field, code) & 0xff) as i32;
        if self.st_mode {
            if place == 0 && field > 0xfff {
                place = 0x100;
            }
            place -= 1;
            if place == -1 {
                let field = self.bits.peek16();
                self.bits.skip(1);
                if field & 0x8000 != 0 {
                    self.num_huf = 0;
                    self.st_mode = false;
                    return;
                }
                let len = if field & 0x4000 != 0 { 4 } else { 3 };
                self.bits.skip(1);
                let f = self.bits.peek16();
                let dist = self.decode_num(f, HF2);
                let dist = (dist << 5) | (self.bits.peek16() >> 11);
                self.bits.skip(5);
                self.copy(dist, len);
                return;
            }
        } else {
            let n = self.num_huf;
            self.num_huf += 1;
            if n >= 16 && self.flags_cnt == 0 {
                self.st_mode = true;
            }
        }
        let place = place as usize & 0xff;
        self.avr_plc += place as u32;
        self.avr_plc -= self.avr_plc >> 8;
        self.nhfb += 16;
        if self.nhfb > 0xff {
            self.nhfb = 0x90;
            self.nlzb >>= 1;
        }
        self.out.push((self.ch_set[place] >> 8) as u8);
        let (mut cur, mut new_place);
        loop {
            cur = u32::from(self.ch_set[place]);
            let slot = &mut self.n_to_pl[(cur & 0xff) as usize];
            new_place = usize::from(*slot);
            *slot = slot.wrapping_add(1);
            cur += 1;
            if cur & 0xff > 0xa1 {
                corr_huff(&mut self.ch_set, &mut self.n_to_pl);
            } else {
                break;
            }
        }
        self.ch_set[place] = self.ch_set[new_place];
        self.ch_set[new_place] = cur as u16;
    }

    fn get_flags(&mut self) {
        let field = self.bits.peek16();
        let place = self.decode_num(field, HF2) as usize;
        if place >= 256 {
            return;
        }
        let (mut flags, mut new_place);
        loop {
            flags = u32::from(self.ch_set_c[place]);
            self.flag_buf = flags >> 8;
            let slot = &mut self.n_to_pl_c[(flags & 0xff) as usize];
            new_place = usize::from(*slot);
            *slot = slot.wrapping_add(1);
            flags += 1;
            if flags & 0xff != 0 {
                break;
            }
            corr_huff(&mut self.ch_set_c, &mut self.n_to_pl_c);
        }
        self.ch_set_c[place] = self.ch_set_c[new_place];
        self.ch_set_c[new_place] = flags as u16;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_rar_1_5() {
        for (name, data) in [
            ("readme_normal", &include_bytes!("../../../tests/fixtures/rar3/readme_154_normal.rar")[..]),
            ("doc_best", include_bytes!("../../../tests/fixtures/rar3/doc_154_best.rar")),
        ] {
            assert!(super::super::rar3::tests::check(name, data, &[15], rar15) > 0, "{name}: nothing checked");
        }
    }
}
