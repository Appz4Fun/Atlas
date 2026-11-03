//! The macroblock layer of I slices with CABAC (H.264 7.3.5, 9.3), and putting each macroblock together: prediction,
//! dequantisation and the inverse transforms (8.3, 8.5).

use super::intra::{self, Edges};
use super::params::{Pps, Sps, scaling_of};
use super::slice::SliceHeader;
use super::tables::{CABAC_INIT_I, LAST_8X8, NORM_4X4, NORM_8X8, SIG_8X8, ZIGZAG_4X4, ZIGZAG_8X8, chroma_qp, ctx};
use crate::video::bits::{DecodeError, Result};
use crate::video::hevc::cabac::{Cabac, State};
use crate::video::picture::Plane;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Kind {
    #[default]
    None,
    I4x4,
    I8x8,
    I16x16,
    Pcm,
}

/// What later macroblocks and the deblocking filter need of one. 4x4 blocks are indexed in raster order in the
/// macroblock: x + 4y.
#[derive(Clone, Copy, Debug, Default)]
pub struct Mb {
    pub kind: Kind,
    pub t8x8: bool,
    pub cbp_luma: u8,
    pub cbp_chroma: u8,
    pub chroma_mode: u8,
    /// Intra4x4PredMode (or the 8x8 one, for each of its four), 2 for the other kinds
    pub modes: [u8; 16],
    /// coded_block_flag of each luma 4x4 (an 8x8 sets its four)
    pub cbf_luma: u16,
    /// bit 0 luma DC, 1 Cb DC, 2 Cr DC
    pub cbf_dc: u8,
    /// chroma AC coded_block_flags, raster in the 2x2, Cb then Cr
    pub cbf_ac: [u8; 2],
    pub qp: i32,
}

/// The decoding order of 4x4 blocks: raster index of each blkIdx.
const BLK_RASTER: [usize; 16] = [0, 1, 4, 5, 2, 3, 6, 7, 8, 9, 12, 13, 10, 11, 14, 15];

pub struct Frame {
    pub sps: Sps,
    pub pps: Pps,
    pub planes: [Plane; 3],
    pub w_mbs: usize,
    pub h_mbs: usize,
    pub mbs: Vec<Mb>,
    /// the slice each macroblock is in (an index into `headers`), -1 for none yet
    pub mb_slice: Vec<i32>,
    pub headers: Vec<SliceHeader>,
    /// luma 4x4 blocks put together, picture wide
    done: Vec<bool>,
    /// LevelScale4x4 by list (Y, Cb, Cr), qP % 6 and raster position
    scale4: [[[i32; 16]; 6]; 3],
    scale8: [[i32; 64]; 6],
}

fn init_contexts(qp: i32) -> Vec<State> {
    let qp = qp.clamp(0, 51);
    CABAC_INIT_I
        .iter()
        .map(|&(m, n)| {
            let pre = (((i32::from(m) * qp) >> 4) + i32::from(n)).clamp(1, 126);
            if pre <= 63 { State { p: (63 - pre) as u8, mps: 0 } } else { State { p: (pre - 64) as u8, mps: 1 } }
        })
        .collect()
}

/// Coefficients of one macroblock, dequantised, in raster order per block.
struct Coeffs {
    /// by blkIdx; an 8x8 block is in `luma8`
    luma: [[i32; 16]; 16],
    luma8: [[i32; 64]; 4],
    /// which blocks have anything
    luma_any: u16,
    chroma: [[[i32; 16]; 4]; 2],
    chroma_any: [u8; 2],
}

impl Frame {
    pub fn new(sps: Sps, pps: Pps) -> Frame {
        let (w_mbs, h_mbs) = (sps.width_mbs as usize, sps.height_mbs as usize);
        let (w, h) = (w_mbs * 16, h_mbs * 16);
        let lists = scaling_of(&sps, &pps);
        let mut scale4 = [[[0; 16]; 6]; 3];
        for (list, s) in scale4.iter_mut().enumerate() {
            for (m, s) in s.iter_mut().enumerate() {
                for k in 0..16 {
                    let p = usize::from(ZIGZAG_4X4[k]);
                    let (i, j) = (p / 4, p % 4);
                    let norm = if i % 2 == 0 && j % 2 == 0 {
                        NORM_4X4[m][0]
                    } else if i % 2 == 1 && j % 2 == 1 {
                        NORM_4X4[m][1]
                    } else {
                        NORM_4X4[m][2]
                    };
                    s[p] = i32::from(lists.list4[list][k]) * norm;
                }
            }
        }
        let mut scale8 = [[0; 64]; 6];
        for (m, s) in scale8.iter_mut().enumerate() {
            for k in 0..64 {
                let p = usize::from(ZIGZAG_8X8[k]);
                let (i, j) = (p / 8, p % 8);
                let class = if i % 4 == 0 && j % 4 == 0 {
                    0
                } else if i % 2 == 1 && j % 2 == 1 {
                    1
                } else if i % 4 == 2 && j % 4 == 2 {
                    2
                } else if (i % 4 == 0 && j % 2 == 1) || (i % 2 == 1 && j % 4 == 0) {
                    3
                } else if (i % 4 == 0 && j % 4 == 2) || (i % 4 == 2 && j % 4 == 0) {
                    4
                } else {
                    5
                };
                s[p] = i32::from(lists.list8[0][k]) * NORM_8X8[m][class];
            }
        }
        Frame {
            sps,
            pps,
            planes: [Plane::new(w, h, 128), Plane::new(w / 2, h / 2, 128), Plane::new(w / 2, h / 2, 128)],
            w_mbs,
            h_mbs,
            mbs: vec![Mb::default(); w_mbs * h_mbs],
            mb_slice: vec![-1; w_mbs * h_mbs],
            headers: Vec::new(),
            done: vec![false; w_mbs * h_mbs * 16],
            scale4,
            scale8,
        }
    }

    /// Decodes one slice's macroblocks from `data` (its RBSP after the NAL header).
    pub fn decode_slice(&mut self, data: &[u8], header: SliceHeader) -> Result<()> {
        let slice = self.headers.len() as i32;
        let mut s = SliceDecoder {
            cabac: Cabac::new(data, header.data_start)?,
            ctxs: init_contexts(header.qp),
            data,
            slice,
            qp: header.qp,
            last_dqp: false,
        };
        let mut addr = header.first_mb as usize;
        self.headers.push(header);
        loop {
            if addr >= self.mbs.len() || self.mb_slice[addr] != -1 {
                return Err(DecodeError::Invalid("macroblock address"));
            }
            self.mb_slice[addr] = slice;
            s.macroblock(self, addr)?;
            if s.cabac.overran() {
                return Err(DecodeError::Truncated);
            }
            if s.cabac.terminate() == 1 {
                return Ok(());
            }
            addr += 1;
        }
    }

    /// Macroblock `n` of the current slice: it's there and in the same slice.
    fn mb_in(&self, addr: Option<usize>, slice: i32) -> Option<usize> {
        addr.filter(|a| self.mb_slice[*a] == slice)
    }

    fn left_of(&self, addr: usize, slice: i32) -> Option<usize> {
        self.mb_in((!addr.is_multiple_of(self.w_mbs)).then(|| addr - 1), slice)
    }

    fn above(&self, addr: usize, slice: i32) -> Option<usize> {
        self.mb_in(addr.checked_sub(self.w_mbs), slice)
    }

    /// A luma sample (x, y) is there for prediction: in the picture, in the slice, and put together already.
    fn sample_ready(&self, x: isize, y: isize, slice: i32) -> bool {
        let w = self.w_mbs as isize * 16;
        if x < 0 || y < 0 || x >= w || y >= self.h_mbs as isize * 16 {
            return false;
        }
        let (x, y) = (x as usize, y as usize);
        let mb = y / 16 * self.w_mbs + x / 16;
        self.mb_slice[mb] == slice && self.done[y / 4 * (self.w_mbs * 4) + x / 4]
    }

    fn mark_done(&mut self, x: usize, y: usize, size: usize) {
        let stride = self.w_mbs * 4;
        for by in y / 4..(y + size) / 4 {
            for bx in x / 4..(x + size) / 4 {
                self.done[by * stride + bx] = true;
            }
        }
    }

    /// The samples around an `n`x`n` luma block at (x, y), with `right` samples above right (0, `n`).
    fn luma_edges(&self, x: usize, y: usize, n: usize, right: usize, slice: i32) -> Edges {
        let p = &self.planes[0];
        let (xi, yi) = (x as isize, y as isize);
        let mut e = Edges { top: None, top_right: false, left: None, corner: None };
        if self.sample_ready(xi, yi - 1, slice) {
            let mut t = [0i32; 16];
            for i in 0..n {
                t[i] = i32::from(p.get(x + i, y - 1));
            }
            e.top_right = right > 0 && self.sample_ready(xi + n as isize, yi - 1, slice);
            for i in n..n + right {
                t[i] = if e.top_right { i32::from(p.get(x + i, y - 1)) } else { t[n - 1] };
            }
            e.top = Some(t);
        }
        if self.sample_ready(xi - 1, yi, slice) {
            let mut l = [0i32; 16];
            for (i, v) in l.iter_mut().enumerate().take(n) {
                *v = i32::from(p.get(x - 1, y + i));
            }
            e.left = Some(l);
        }
        if self.sample_ready(xi - 1, yi - 1, slice) {
            e.corner = Some(i32::from(p.get(x - 1, y - 1)));
        }
        e
    }

    /// The samples around a macroblock's 8x8 chroma block in plane `c`.
    fn chroma_edges(&self, mbx: usize, mby: usize, c: usize, slice: i32) -> Edges {
        let p = &self.planes[c];
        let (x, y) = (mbx * 8, mby * 8);
        let (lx, ly) = (mbx as isize * 16, mby as isize * 16);
        let mut e = Edges { top: None, top_right: false, left: None, corner: None };
        if self.sample_ready(lx, ly - 1, slice) {
            let mut t = [0i32; 16];
            for (i, v) in t.iter_mut().enumerate().take(8) {
                *v = i32::from(p.get(x + i, y - 1));
            }
            e.top = Some(t);
        }
        if self.sample_ready(lx - 1, ly, slice) {
            let mut l = [0i32; 16];
            for (i, v) in l.iter_mut().enumerate().take(8) {
                *v = i32::from(p.get(x - 1, y + i));
            }
            e.left = Some(l);
        }
        if self.sample_ready(lx - 1, ly - 1, slice) {
            e.corner = Some(i32::from(p.get(x - 1, y - 1)));
        }
        e
    }

    fn put(&mut self, c: usize, x: usize, y: usize, n: usize, pred: &[i32], residual: Option<&[i32]>) {
        let plane = &mut self.planes[c];
        for j in 0..n {
            for i in 0..n {
                let r = residual.map_or(0, |r| r[j * n + i]);
                plane.set(x + i, y + j, (pred[j * n + i] + r).clamp(0, 255) as u16);
            }
        }
    }
}

/// 8.5.12.2: rows, then columns, then (x + 32) >> 6.
fn idct4(d: &mut [i32; 16]) {
    for row in d.as_chunks_mut::<4>().0 {
        let e = [row[0] + row[2], row[0] - row[2], (row[1] >> 1) - row[3], row[1] + (row[3] >> 1)];
        row[0] = e[0] + e[3];
        row[1] = e[1] + e[2];
        row[2] = e[1] - e[2];
        row[3] = e[0] - e[3];
    }
    for i in 0..4 {
        let (a, b, c, dd) = (d[i], d[4 + i], d[8 + i], d[12 + i]);
        let e = [a + c, a - c, (b >> 1) - dd, b + (dd >> 1)];
        d[i] = (e[0] + e[3] + 32) >> 6;
        d[4 + i] = (e[1] + e[2] + 32) >> 6;
        d[8 + i] = (e[1] - e[2] + 32) >> 6;
        d[12 + i] = (e[0] - e[3] + 32) >> 6;
    }
}

fn idct8_line(d: [i32; 8]) -> [i32; 8] {
    let a0 = d[0] + d[4];
    let a4 = d[0] - d[4];
    let a2 = (d[2] >> 1) - d[6];
    let a6 = d[2] + (d[6] >> 1);
    let b0 = a0 + a6;
    let b2 = a4 + a2;
    let b4 = a4 - a2;
    let b6 = a0 - a6;
    let a1 = -d[3] + d[5] - d[7] - (d[7] >> 1);
    let a3 = d[1] + d[7] - d[3] - (d[3] >> 1);
    let a5 = -d[1] + d[7] + d[5] + (d[5] >> 1);
    let a7 = d[3] + d[5] + d[1] + (d[1] >> 1);
    let b1 = a1 + (a7 >> 2);
    let b7 = a7 - (a1 >> 2);
    let b3 = a3 + (a5 >> 2);
    let b5 = (a3 >> 2) - a5;
    [b0 + b7, b2 + b5, b4 + b3, b6 + b1, b6 - b1, b4 - b3, b2 - b5, b0 - b7]
}

/// 8.5.13.2
fn idct8(d: &mut [i32; 64]) {
    for row in d.as_chunks_mut::<8>().0 {
        let out = idct8_line(*row);
        row.copy_from_slice(&out);
    }
    for i in 0..8 {
        let col = std::array::from_fn(|k| d[k * 8 + i]);
        let out = idct8_line(col);
        for k in 0..8 {
            d[k * 8 + i] = (out[k] + 32) >> 6;
        }
    }
}

/// Past what a valid stream can hold: a damaged one is kept from overflowing.
const LIMIT: i64 = 1 << 20;

/// `c * scale`, shifted by `shift` (left when positive, right with rounding when not), within [`LIMIT`].
fn scaled(c: i32, scale: i32, shift: i32) -> i32 {
    let v = i64::from(c) * i64::from(scale);
    let v = if shift >= 0 { v << shift } else { (v + (1 << (-shift - 1))) >> -shift };
    v.clamp(-LIMIT, LIMIT) as i32
}

fn dequant4(c: i32, scale: i32, qp: i32) -> i32 {
    scaled(c, scale, qp / 6 - 4)
}

/// Luma DC and 8x8 coefficients are scaled by six more bits (8-326, 8-336).
fn dequant6(c: i32, scale: i32, qp: i32) -> i32 {
    scaled(c, scale, qp / 6 - 6)
}

struct SliceDecoder<'a> {
    cabac: Cabac<'a>,
    ctxs: Vec<State>,
    data: &'a [u8],
    slice: i32,
    qp: i32,
    /// the previous macroblock in the slice had a non-zero mb_qp_delta
    last_dqp: bool,
}

impl SliceDecoder<'_> {
    fn bin(&mut self, ctx: usize) -> u32 {
        self.cabac.decision(&mut self.ctxs[ctx])
    }

    fn macroblock(&mut self, f: &mut Frame, addr: usize) -> Result<()> {
        let slice = self.slice;
        let (a, b) = (f.left_of(addr, slice), f.above(addr, slice));
        let (ma, mb) = (a.map(|a| f.mbs[a]), b.map(|b| f.mbs[b]));
        let mut cur = Mb { qp: self.qp, modes: [2; 16], ..Mb::default() };

        // mb_type
        let not_nxn = |m: Option<Mb>| m.is_some_and(|m| !matches!(m.kind, Kind::I4x4 | Kind::I8x8)) as usize;
        let mb_type = if self.bin(ctx::MB_TYPE + not_nxn(ma) + not_nxn(mb)) == 0 {
            0
        } else if self.cabac.terminate() == 1 {
            25
        } else {
            let luma = self.bin(ctx::MB_TYPE + 3);
            let chroma = if self.bin(ctx::MB_TYPE + 4) == 0 { 0 } else { 1 + self.bin(ctx::MB_TYPE + 5) };
            let mode = self.bin(ctx::MB_TYPE + 6) << 1 | self.bin(ctx::MB_TYPE + 7);
            1 + mode + 4 * chroma + 12 * luma
        };
        let (mbx, mby) = (addr % f.w_mbs, addr / f.w_mbs);
        if mb_type == 25 {
            return self.pcm(f, addr, cur);
        }

        let mut i16_mode = 0;
        if mb_type == 0 {
            cur.kind = Kind::I4x4;
            if f.pps.transform_8x8 {
                let t = |m: Option<Mb>| m.is_some_and(|m| m.t8x8) as usize;
                cur.t8x8 = self.bin(ctx::TRANSFORM_8X8 + t(ma) + t(mb)) == 1;
                if cur.t8x8 {
                    cur.kind = Kind::I8x8;
                }
            }
            self.pred_modes(&mut cur, ma, mb);
        } else {
            cur.kind = Kind::I16x16;
            i16_mode = ((mb_type - 1) % 4) as u8;
            cur.cbp_chroma = ((mb_type - 1) / 4 % 3) as u8;
            cur.cbp_luma = if mb_type >= 13 { 15 } else { 0 };
        }

        // intra_chroma_pred_mode
        let c = |m: Option<Mb>| m.is_some_and(|m| m.kind != Kind::Pcm && m.chroma_mode != 0) as usize;
        cur.chroma_mode = if self.bin(ctx::CHROMA_PRED + c(ma) + c(mb)) == 0 {
            0
        } else if self.bin(ctx::CHROMA_PRED + 3) == 0 {
            1
        } else {
            2 + self.bin(ctx::CHROMA_PRED + 3) as u8
        };

        if cur.kind != Kind::I16x16 {
            self.coded_block_pattern(&mut cur, ma, mb);
        }

        let coded = cur.cbp_luma > 0 || cur.cbp_chroma > 0 || cur.kind == Kind::I16x16;
        let mut dqp = 0;
        if coded {
            let mut k = 0u32;
            let mut ctx_i = ctx::MB_QP_DELTA + usize::from(self.last_dqp);
            while self.bin(ctx_i) == 1 {
                k += 1;
                if k > 52 {
                    return Err(DecodeError::Invalid("mb_qp_delta"));
                }
                ctx_i = ctx::MB_QP_DELTA + if k == 1 { 2 } else { 3 };
            }
            dqp = if k % 2 == 1 { k.div_ceil(2) as i32 } else { -((k / 2) as i32) };
            if !(-26..=25).contains(&dqp) {
                return Err(DecodeError::Invalid("mb_qp_delta"));
            }
            self.qp = (self.qp + dqp + 52) % 52;
        }
        self.last_dqp = dqp != 0;
        cur.qp = self.qp;

        let mut co = Coeffs {
            luma: [[0; 16]; 16],
            luma8: [[0; 64]; 4],
            luma_any: 0,
            chroma: [[[0; 16]; 4]; 2],
            chroma_any: [0; 2],
        };
        if coded {
            self.residual(f, &mut cur, ma, mb, &mut co);
        }
        if self.cabac.overran() {
            return Err(DecodeError::Truncated);
        }
        f.mbs[addr] = cur;
        self.rebuild(f, mbx, mby, &cur, i16_mode, &mut co);
        Ok(())
    }

    fn pcm(&mut self, f: &mut Frame, addr: usize, mut cur: Mb) -> Result<()> {
        let at = self.cabac.next_byte();
        let bytes = self.data.get(at..at + 384).ok_or(DecodeError::Truncated)?;
        let (mbx, mby) = (addr % f.w_mbs, addr / f.w_mbs);
        for (i, &v) in bytes[..256].iter().enumerate() {
            f.planes[0].set(mbx * 16 + i % 16, mby * 16 + i / 16, u16::from(v));
        }
        for c in 0..2 {
            for (i, &v) in bytes[256 + c * 64..320 + c * 64].iter().enumerate() {
                f.planes[1 + c].set(mbx * 8 + i % 8, mby * 8 + i / 8, u16::from(v));
            }
        }
        cur.kind = Kind::Pcm;
        cur.cbp_luma = 15;
        cur.cbp_chroma = 2;
        cur.cbf_luma = 0xFFFF;
        cur.cbf_dc = 7;
        cur.cbf_ac = [15, 15];
        f.mbs[addr] = cur;
        f.mark_done(mbx * 16, mby * 16, 16);
        self.last_dqp = false;
        self.cabac = Cabac::new(self.data, at + 384)?;
        Ok(())
    }

    /// The predicted mode of a 4x4 or 8x8 block at raster 4x4 (bx, by) of the macroblock, from the blocks left and
    /// above its top left 4x4.
    fn predicted_mode(cur: &Mb, bx: usize, by: usize, ma: Option<Mb>, mb: Option<Mb>) -> u8 {
        let left = if bx > 0 { Some(cur.modes[by * 4 + bx - 1]) } else { ma.map(|m| m.modes[by * 4 + 3]) };
        let up = if by > 0 { Some(cur.modes[(by - 1) * 4 + bx]) } else { mb.map(|m| m.modes[12 + bx]) };
        match (left, up) {
            (Some(l), Some(u)) => l.min(u),
            _ => 2,
        }
    }

    fn pred_modes(&mut self, cur: &mut Mb, ma: Option<Mb>, mb: Option<Mb>) {
        let (count, size) = if cur.t8x8 { (4, 2) } else { (16, 1) };
        for blk in 0..count {
            let (bx, by) = if cur.t8x8 {
                (blk % 2 * 2, blk / 2 * 2)
            } else {
                let r = BLK_RASTER[blk];
                (r % 4, r / 4)
            };
            let predicted = Self::predicted_mode(cur, bx, by, ma, mb);
            let mode = if self.bin(ctx::PREV_INTRA) == 1 {
                predicted
            } else {
                let rem =
                    (self.bin(ctx::REM_INTRA) | self.bin(ctx::REM_INTRA) << 1 | self.bin(ctx::REM_INTRA) << 2) as u8;
                if rem < predicted { rem } else { rem + 1 }
            };
            for y in by..by + size {
                for x in bx..bx + size {
                    cur.modes[y * 4 + x] = mode;
                }
            }
        }
    }

    fn coded_block_pattern(&mut self, cur: &mut Mb, ma: Option<Mb>, mb: Option<Mb>) {
        let mut luma = 0u8;
        for b8 in 0..4 {
            let (x8, y8) = (b8 % 2, b8 / 2);
            // 1 when the neighbouring 8x8 has no coefficients
            let cond = |m: Option<Mb>, n: usize| match m {
                None => 0,
                Some(m) if m.kind == Kind::Pcm => 0,
                Some(m) => usize::from(m.cbp_luma >> n & 1 == 0),
            };
            let a = if x8 == 1 { usize::from(luma >> (b8 - 1) & 1 == 0) } else { cond(ma, b8 + 1) };
            let b = if y8 == 1 { usize::from(luma >> (b8 - 2) & 1 == 0) } else { cond(mb, b8 + 2) };
            luma |= (self.bin(ctx::CBP_LUMA + a + 2 * b) as u8) << b8;
        }
        cur.cbp_luma = luma;
        let cond = |m: Option<Mb>, two: bool| {
            m.is_some_and(|m| m.kind == Kind::Pcm || if two { m.cbp_chroma == 2 } else { m.cbp_chroma != 0 }) as usize
        };
        cur.cbp_chroma = if self.bin(ctx::CBP_CHROMA + cond(ma, false) + 2 * cond(mb, false)) == 0 {
            0
        } else {
            1 + self.bin(ctx::CBP_CHROMA + 4 + cond(ma, true) + 2 * cond(mb, true)) as u8
        };
    }

    /// coded_block_flag's ctxIdxInc part for one neighbour: an unavailable one counts as coded (we're intra).
    fn cbf_cond(m: Option<Mb>, flag: impl Fn(&Mb) -> bool) -> usize {
        match m {
            None => 1,
            Some(m) if m.kind == Kind::Pcm => 1,
            Some(m) => usize::from(flag(&m)),
        }
    }

    /// residual_block_cabac: the coefficients in scan order into `out`. `cat` 0 to 5; `cbf_inc` for all but 5.
    fn block(&mut self, cat: usize, cbf_inc: usize, max: usize, out: &mut [i32; 64]) -> bool {
        if cat != 5 && self.bin(ctx::CODED_BLOCK + ctx::CBF_CAT[cat] + cbf_inc) == 0 {
            return false;
        }
        let mut sig = [false; 64];
        let mut last = max - 1;
        for (i, s) in sig.iter_mut().enumerate().take(max - 1) {
            let (sc, lc) = match cat {
                5 => (ctx::SIG_8X8 + usize::from(SIG_8X8[i]), ctx::LAST_8X8 + usize::from(LAST_8X8[i])),
                3 => (ctx::SIG + 44 + i.min(2), ctx::LAST + 44 + i.min(2)),
                _ => (ctx::SIG + ctx::SIG_CAT[cat] + i, ctx::LAST + ctx::SIG_CAT[cat] + i),
            };
            if self.bin(sc) == 1 {
                *s = true;
                if self.bin(lc) == 1 {
                    last = i;
                    break;
                }
            }
        }
        sig[last] = true;
        let base = if cat == 5 { ctx::ABS_8X8 } else { ctx::ABS + ctx::ABS_CAT[cat] };
        let (mut gt1, mut eq1) = (0usize, 0usize);
        out[..max].fill(0);
        for i in (0..=last).rev() {
            if !sig[i] {
                continue;
            }
            let first = if gt1 != 0 { 0 } else { (1 + eq1).min(4) };
            let mut prefix = 0;
            if self.bin(base + first) == 1 {
                prefix = 1;
                let rest = base + 5 + gt1.min(4 - usize::from(cat == 3));
                while prefix < 14 && self.bin(rest) == 1 {
                    prefix += 1;
                }
            }
            let mut level = prefix;
            if prefix == 14 {
                let mut k = 0;
                while self.cabac.bypass() == 1 {
                    level += 1 << k;
                    k += 1;
                    if k > 24 {
                        break;
                    }
                }
                while k > 0 {
                    k -= 1;
                    level += (self.cabac.bypass() as i32) << k;
                }
            }
            // a valid level fits in 16 bits
            let abs = (level + 1).min(1 << 15);
            if abs == 1 {
                eq1 += 1;
            } else {
                gt1 += 1;
            }
            out[i] = if self.cabac.bypass() == 1 { -abs } else { abs };
        }
        true
    }

    #[allow(clippy::too_many_arguments)]
    fn residual(&mut self, f: &Frame, cur: &mut Mb, ma: Option<Mb>, mb: Option<Mb>, co: &mut Coeffs) {
        let mut buf = [0i32; 64];
        let qp = cur.qp;
        // luma DC of a 16x16
        let mut dc = [0i32; 16];
        if cur.kind == Kind::I16x16 {
            let inc = Self::cbf_cond(ma, |m| m.kind == Kind::I16x16 && m.cbf_dc & 1 != 0)
                + 2 * Self::cbf_cond(mb, |m| m.kind == Kind::I16x16 && m.cbf_dc & 1 != 0);
            if self.block(0, inc, 16, &mut buf) {
                cur.cbf_dc |= 1;
                let mut c = [0i32; 16];
                for k in 0..16 {
                    c[usize::from(ZIGZAG_4X4[k])] = buf[k];
                }
                // 8.5.10: the Hadamard transform, then scaling
                let mut t = [0i32; 16];
                for i in 0..4 {
                    let r = &c[i * 4..i * 4 + 4];
                    t[i * 4] = r[0] + r[1] + r[2] + r[3];
                    t[i * 4 + 1] = r[0] + r[1] - r[2] - r[3];
                    t[i * 4 + 2] = r[0] - r[1] - r[2] + r[3];
                    t[i * 4 + 3] = r[0] - r[1] + r[2] - r[3];
                }
                let scale = f.scale4[0][(qp % 6) as usize][0];
                for j in 0..4 {
                    let (a, b, cc, d) = (t[j], t[4 + j], t[8 + j], t[12 + j]);
                    let col = [a + b + cc + d, a + b - cc - d, a - b - cc + d, a - b + cc - d];
                    for i in 0..4 {
                        dc[i * 4 + j] = dequant6(col[i], scale, qp);
                    }
                }
            }
        }
        // the coded_block_flag of luma 4x4 (bx, by) of the macroblock, or a neighbour's
        let luma_cond = |cur: &Mb, bx: isize, by: isize| -> usize {
            if bx >= 0 && by >= 0 {
                usize::from(cur.cbf_luma >> (by * 4 + bx) & 1 != 0)
            } else if bx < 0 {
                Self::cbf_cond(ma, |m| m.cbf_luma >> (by * 4 + 3) & 1 != 0)
            } else {
                Self::cbf_cond(mb, |m| m.cbf_luma >> (12 + bx) & 1 != 0)
            }
        };
        let level_scale = &f.scale4[0][(qp % 6) as usize];
        if cur.t8x8 {
            for b8 in 0..4 {
                if cur.cbp_luma >> b8 & 1 == 0 {
                    continue;
                }
                self.block(5, 0, 64, &mut buf);
                let s = &f.scale8[(qp % 6) as usize];
                let out = &mut co.luma8[b8];
                for k in 0..64 {
                    let p = usize::from(ZIGZAG_8X8[k]);
                    out[p] = dequant6(buf[k], s[p], qp);
                }
                co.luma_any |= 1 << b8;
                let (bx, by) = (b8 % 2 * 2, b8 / 2 * 2);
                for (x, y) in [(bx, by), (bx + 1, by), (bx, by + 1), (bx + 1, by + 1)] {
                    cur.cbf_luma |= 1 << (y * 4 + x);
                }
            }
        } else {
            let (cat, max) = if cur.kind == Kind::I16x16 { (1, 15) } else { (2, 16) };
            for blk in 0..16 {
                let r = BLK_RASTER[blk];
                let (bx, by) = ((r % 4) as isize, (r / 4) as isize);
                let out = &mut co.luma[blk];
                if cur.kind == Kind::I16x16 {
                    out[0] = dc[r];
                }
                if cur.cbp_luma >> (blk / 4) & 1 != 0 {
                    let inc = luma_cond(cur, bx - 1, by) + 2 * luma_cond(cur, bx, by - 1);
                    if self.block(cat, inc, max, &mut buf) {
                        cur.cbf_luma |= 1 << r;
                        let skip = usize::from(cat == 1);
                        for k in 0..max {
                            let p = usize::from(ZIGZAG_4X4[k + skip]);
                            out[p] = dequant4(buf[k], level_scale[p], qp);
                        }
                    }
                }
                if out.iter().any(|v| *v != 0) {
                    co.luma_any |= 1 << blk;
                }
            }
        }
        // chroma
        if cur.cbp_chroma == 0 {
            return;
        }
        let mut chroma_dc = [[0i32; 4]; 2];
        for c in 0..2 {
            let flag = |m: &Mb| m.cbf_dc >> (1 + c) & 1 != 0;
            let inc = Self::cbf_cond(ma, flag) + 2 * Self::cbf_cond(mb, flag);
            if self.block(3, inc, 4, &mut buf) {
                cur.cbf_dc |= 2 << c;
                let qpc = chroma_qp(qp + f.pps.chroma_qp_offset[c]);
                let scale = f.scale4[1 + c][(qpc % 6) as usize][0];
                let (c0, c1, c2, c3) = (buf[0], buf[1], buf[2], buf[3]);
                let t = [c0 + c1 + c2 + c3, c0 - c1 + c2 - c3, c0 + c1 - c2 - c3, c0 - c1 - c2 + c3];
                for k in 0..4 {
                    chroma_dc[c][k] = scaled(t[k], scale, qpc / 6) >> 5;
                }
            }
        }
        for c in 0..2 {
            let qpc = chroma_qp(qp + f.pps.chroma_qp_offset[c]);
            let level_scale = f.scale4[1 + c][(qpc % 6) as usize];
            for blk in 0..4 {
                let out = &mut co.chroma[c][blk];
                out[0] = chroma_dc[c][blk];
                if cur.cbp_chroma == 2 {
                    let (bx, by) = (blk % 2, blk / 2);
                    let a = if bx == 1 {
                        usize::from(cur.cbf_ac[c] >> (blk - 1) & 1 != 0)
                    } else {
                        Self::cbf_cond(ma, |m| m.cbp_chroma == 2 && m.cbf_ac[c] >> (blk + 1) & 1 != 0)
                    };
                    let b = if by == 1 {
                        usize::from(cur.cbf_ac[c] >> (blk - 2) & 1 != 0)
                    } else {
                        Self::cbf_cond(mb, |m| m.cbp_chroma == 2 && m.cbf_ac[c] >> (blk + 2) & 1 != 0)
                    };
                    if self.block(4, a + 2 * b, 15, &mut buf) {
                        cur.cbf_ac[c] |= 1 << blk;
                        for k in 0..15 {
                            let p = usize::from(ZIGZAG_4X4[k + 1]);
                            out[p] = dequant4(buf[k], level_scale[p], qpc);
                        }
                    }
                }
                if out.iter().any(|v| *v != 0) {
                    co.chroma_any[c] |= 1 << blk;
                }
            }
        }
    }

    /// Prediction plus residual, block by block.
    fn rebuild(&self, f: &mut Frame, mbx: usize, mby: usize, cur: &Mb, i16_mode: u8, co: &mut Coeffs) {
        let slice = self.slice;
        let (x0, y0) = (mbx * 16, mby * 16);
        let mut pred = [0i32; 256];
        match cur.kind {
            Kind::I4x4 => {
                for blk in 0..16 {
                    let r = BLK_RASTER[blk];
                    let (x, y) = (x0 + r % 4 * 4, y0 + r / 4 * 4);
                    let e = f.luma_edges(x, y, 4, 4, slice);
                    intra::directional(4, cur.modes[r], &e, &mut pred[..16]);
                    let res = (co.luma_any >> blk & 1 != 0).then(|| {
                        idct4(&mut co.luma[blk]);
                        &co.luma[blk][..]
                    });
                    f.put(0, x, y, 4, &pred[..16], res);
                    f.mark_done(x, y, 4);
                }
            }
            Kind::I8x8 => {
                for b8 in 0..4 {
                    let (x, y) = (x0 + b8 % 2 * 8, y0 + b8 / 2 * 8);
                    let e = intra::filter_8x8(&f.luma_edges(x, y, 8, 8, slice));
                    intra::directional(8, cur.modes[b8 / 2 * 8 + b8 % 2 * 2], &e, &mut pred[..64]);
                    let res = (co.luma_any >> b8 & 1 != 0).then(|| {
                        idct8(&mut co.luma8[b8]);
                        &co.luma8[b8][..]
                    });
                    f.put(0, x, y, 8, &pred[..64], res);
                    f.mark_done(x, y, 8);
                }
            }
            _ => {
                let e = f.luma_edges(x0, y0, 16, 0, slice);
                if !intra::block_mode(16, i16_mode, &e, &mut pred) {
                    intra::block_mode(16, 2, &e, &mut pred);
                }
                let mut res = [0i32; 256];
                for blk in 0..16 {
                    if co.luma_any >> blk & 1 == 0 {
                        continue;
                    }
                    let r = BLK_RASTER[blk];
                    idct4(&mut co.luma[blk]);
                    for j in 0..4 {
                        for i in 0..4 {
                            res[(r / 4 * 4 + j) * 16 + r % 4 * 4 + i] = co.luma[blk][j * 4 + i];
                        }
                    }
                }
                f.put(0, x0, y0, 16, &pred, Some(&res));
                f.mark_done(x0, y0, 16);
            }
        }
        // chroma: intra_chroma_pred_mode 0 DC, 1 horizontal, 2 vertical, 3 plane
        let mode = [2, 1, 0, 3][usize::from(cur.chroma_mode)];
        for c in 0..2 {
            let e = f.chroma_edges(mbx, mby, 1 + c, slice);
            if !intra::block_mode(8, mode, &e, &mut pred[..64]) {
                intra::block_mode(8, 2, &e, &mut pred[..64]);
            }
            let mut res = [0i32; 64];
            for blk in 0..4 {
                if co.chroma_any[c] >> blk & 1 == 0 {
                    continue;
                }
                let block = &mut co.chroma[c][blk];
                idct4(block);
                for j in 0..4 {
                    for i in 0..4 {
                        res[(blk / 2 * 4 + j) * 8 + blk % 2 * 4 + i] = block[j * 4 + i];
                    }
                }
            }
            f.put(1 + c, mbx * 8, mby * 8, 8, &pred[..64], Some(&res));
        }
    }
}
