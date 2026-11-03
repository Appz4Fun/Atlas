//! The arithmetic decoder (H.265 9.3.4.3), bit by bit as the standard writes it, and the contexts an intra slice
//! uses with their initial values (9.3.2.2, init type 0).

use crate::video::bits::{DecodeError, Result};

const RANGE_TAB_LPS: [[u8; 4]; 64] = [
    [128, 176, 208, 240],
    [128, 167, 197, 227],
    [128, 158, 187, 216],
    [123, 150, 178, 205],
    [116, 142, 169, 195],
    [111, 135, 160, 185],
    [105, 128, 152, 175],
    [100, 122, 144, 166],
    [95, 116, 137, 158],
    [90, 110, 130, 150],
    [85, 104, 123, 142],
    [81, 99, 117, 135],
    [77, 94, 111, 128],
    [73, 89, 105, 122],
    [69, 85, 100, 116],
    [66, 80, 95, 110],
    [62, 76, 90, 104],
    [59, 72, 86, 99],
    [56, 69, 81, 94],
    [53, 65, 77, 89],
    [51, 62, 73, 85],
    [48, 59, 69, 80],
    [46, 56, 66, 76],
    [43, 53, 63, 72],
    [41, 50, 59, 69],
    [39, 48, 56, 65],
    [37, 45, 54, 62],
    [35, 43, 51, 59],
    [33, 41, 48, 56],
    [32, 39, 46, 53],
    [30, 37, 43, 50],
    [29, 35, 41, 48],
    [27, 33, 39, 45],
    [26, 31, 37, 43],
    [24, 30, 35, 41],
    [23, 28, 33, 39],
    [22, 27, 32, 37],
    [21, 26, 30, 35],
    [20, 24, 29, 33],
    [19, 23, 27, 31],
    [18, 22, 26, 30],
    [17, 21, 25, 28],
    [16, 20, 23, 27],
    [15, 19, 22, 25],
    [14, 18, 21, 24],
    [14, 17, 20, 23],
    [13, 16, 19, 22],
    [12, 15, 18, 21],
    [12, 14, 17, 20],
    [11, 14, 16, 19],
    [11, 13, 15, 18],
    [10, 12, 15, 17],
    [10, 12, 14, 16],
    [9, 11, 13, 15],
    [9, 11, 12, 14],
    [8, 10, 12, 14],
    [8, 9, 11, 13],
    [7, 9, 11, 12],
    [7, 9, 10, 12],
    [7, 8, 10, 11],
    [6, 8, 9, 11],
    [6, 7, 9, 10],
    [6, 7, 8, 9],
    [2, 2, 2, 2],
];

const TRANS_IDX_LPS: [u8; 64] = [
    0, 0, 1, 2, 2, 4, 4, 5, 6, 7, 8, 9, 9, 11, 11, 12, 13, 13, 15, 15, 16, 16, 18, 18, 19, 19, 21, 21, 22, 22, 23, 24,
    24, 25, 26, 26, 27, 27, 28, 29, 29, 30, 30, 30, 31, 32, 32, 33, 33, 33, 34, 34, 35, 35, 35, 36, 36, 36, 37, 37, 37,
    38, 38, 63,
];

/// Where each syntax element's contexts start in `Contexts`, and how many it has.
pub mod ctx {
    pub const SAO_MERGE: usize = 0;
    pub const SAO_TYPE: usize = 1;
    pub const SPLIT_CU: usize = 2; // 3
    pub const TRANSQUANT_BYPASS: usize = 5;
    pub const CU_QP_DELTA: usize = 6; // 2
    pub const PART_MODE: usize = 8;
    pub const PREV_INTRA_LUMA: usize = 9;
    pub const INTRA_CHROMA: usize = 10;
    pub const SPLIT_TRANSFORM: usize = 11; // 3
    pub const CBF_LUMA: usize = 14; // 2
    pub const CBF_CHROMA: usize = 16; // 5
    pub const TRANSFORM_SKIP: usize = 21; // 2: luma, chroma
    pub const LAST_X: usize = 23; // 18
    pub const LAST_Y: usize = 41; // 18
    pub const CODED_SUB_BLOCK: usize = 59; // 4
    pub const SIG_COEFF: usize = 63; // 44
    pub const GREATER1: usize = 107; // 24
    pub const GREATER2: usize = 131; // 6
    pub const COUNT: usize = 137;
}

/// Init type 0's values, in the order of `ctx`.
const INIT_VALUES: [u8; ctx::COUNT] = [
    153, // sao_merge
    200, // sao_type_idx
    139, 141, 157, // split_cu_flag
    154, // cu_transquant_bypass_flag
    154, 154, // cu_qp_delta_abs
    184, // part_mode
    184, // prev_intra_luma_pred_flag
    63,  // intra_chroma_pred_mode
    153, 138, 138, // split_transform_flag
    111, 141, // cbf_luma
    94, 138, 182, 154, 154, // cbf_cb, cbf_cr
    139, 139, // transform_skip_flag
    110, 110, 124, 125, 140, 153, 125, 127, 140, 109, 111, 143, 127, 111, 79, 108, 123, 63, // last x
    110, 110, 124, 125, 140, 153, 125, 127, 140, 109, 111, 143, 127, 111, 79, 108, 123, 63, // last y
    91, 171, 134, 141, // coded_sub_block_flag
    111, 111, 125, 110, 110, 94, 124, 108, 124, 107, 125, 141, 179, 153, 125, 107, 125, 141, 179, 153, 125, 107, 125,
    141, 179, 153, 125, 140, 139, 182, 182, 152, 136, 152, 136, 153, 136, 139, 111, 136, 139, 111, 141,
    111, // sig_coeff_flag
    140, 92, 137, 138, 140, 152, 138, 139, 153, 74, 149, 92, 139, 107, 122, 152, 140, 179, 166, 182, 140, 227, 122,
    197, // coeff_abs_level_greater1_flag
    138, 153, 136, 167, 152, 152, // coeff_abs_level_greater2_flag
];

/// One context: its probability state and which value is the more probable.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct State {
    pub p: u8,
    pub mps: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Contexts(pub [State; ctx::COUNT]);

impl Contexts {
    pub fn new(slice_qp: i32) -> Contexts {
        let qp = slice_qp.clamp(0, 51);
        let mut states = [State::default(); ctx::COUNT];
        for (state, &init) in states.iter_mut().zip(INIT_VALUES.iter()) {
            let m = i32::from(init >> 4) * 5 - 45;
            let n = (i32::from(init & 15) << 3) - 16;
            let pre = (((m * qp) >> 4) + n).clamp(1, 126);
            *state =
                if pre <= 63 { State { p: (63 - pre) as u8, mps: 0 } } else { State { p: (pre - 64) as u8, mps: 1 } };
        }
        Contexts(states)
    }
}

/// The decoding engine over a slice's data (emulation prevention bytes already out).
pub struct Cabac<'a> {
    data: &'a [u8],
    /// bits read
    pos: usize,
    range: u32,
    offset: u32,
}

impl<'a> Cabac<'a> {
    /// Starts at byte `start` of `data`.
    pub fn new(data: &'a [u8], start: usize) -> Result<Cabac<'a>> {
        let mut c = Cabac { data, pos: start * 8, range: 510, offset: 0 };
        c.offset = c.read_bits(9)?;
        if c.offset >= 510 {
            return Err(DecodeError::Invalid("CABAC start"));
        }
        Ok(c)
    }

    #[inline]
    fn read_bit(&mut self) -> u32 {
        // past the end reads zeros: a slice that runs out fails on its own checks, never here
        let bit = self.data.get(self.pos >> 3).map_or(0, |b| (b >> (7 - (self.pos & 7))) & 1);
        self.pos += 1;
        u32::from(bit)
    }

    fn read_bits(&mut self, n: u32) -> Result<u32> {
        if self.pos + n as usize > self.data.len() * 8 {
            return Err(DecodeError::Truncated);
        }
        let mut v = 0;
        for _ in 0..n {
            v = v << 1 | self.read_bit();
        }
        Ok(v)
    }

    /// Whether it has read past the slice's end: what it decoded since is garbage.
    pub fn overran(&self) -> bool {
        self.pos > self.data.len() * 8 + 16
    }

    /// The byte after the one the last bin's bits end in: where the next substream or PCM samples start.
    pub fn next_byte(&self) -> usize {
        self.pos.div_ceil(8)
    }

    #[inline]
    pub fn decision(&mut self, s: &mut State) -> u32 {
        let q = ((self.range >> 6) & 3) as usize;
        let lps = u32::from(RANGE_TAB_LPS[usize::from(s.p)][q]);
        self.range -= lps;
        let bin;
        if self.offset >= self.range {
            bin = u32::from(1 - s.mps);
            self.offset -= self.range;
            self.range = lps;
            if s.p == 0 {
                s.mps = 1 - s.mps;
            }
            s.p = TRANS_IDX_LPS[usize::from(s.p)];
        } else {
            bin = u32::from(s.mps);
            if s.p < 62 {
                s.p += 1;
            }
        }
        while self.range < 256 {
            self.range <<= 1;
            self.offset = self.offset << 1 | self.read_bit();
        }
        bin
    }

    #[inline]
    pub fn bypass(&mut self) -> u32 {
        self.offset = self.offset << 1 | self.read_bit();
        if self.offset >= self.range {
            self.offset -= self.range;
            1
        } else {
            0
        }
    }

    pub fn bypass_bits(&mut self, n: u32) -> u32 {
        let mut v = 0;
        for _ in 0..n {
            v = v << 1 | self.bypass();
        }
        v
    }

    pub fn terminate(&mut self) -> u32 {
        self.range -= 2;
        if self.offset >= self.range {
            1
        } else {
            while self.range < 256 {
                self.range <<= 1;
                self.offset = self.offset << 1 | self.read_bit();
            }
            0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contexts_start_where_the_standard_says() {
        // initValue 154 is the equiprobable one: m 0, n 64
        let c = Contexts::new(26);
        assert_eq!(c.0[ctx::TRANSQUANT_BYPASS], State { p: 0, mps: 1 });
        assert_eq!(INIT_VALUES.len(), ctx::COUNT);
    }
}
