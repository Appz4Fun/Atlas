//! The deblocking filter (H.264 8.7) for a picture of intra macroblocks: bS is 4 on macroblock edges and 3 inside.

use super::decode::{Frame, Kind};
use super::tables::{ALPHA, BETA, TC0, chroma_qp};

/// Filters one line of samples across an edge: `s[i]` is the sample `i` steps from the edge on the q side, `s[-1-i]`
/// on the p side, read and written through `at`.
#[allow(clippy::too_many_arguments)]
fn filter_line(data: &mut [u16], q0: usize, step: isize, bs: u8, alpha: i32, beta: i32, tc0: i32, chroma: bool) {
    let at = |k: isize| (q0 as isize + k * step) as usize;
    let g = |data: &[u16], k: isize| i32::from(data[at(k)]);
    let (p0, p1, q0v, q1) = (g(data, -1), g(data, -2), g(data, 0), g(data, 1));
    if (p0 - q0v).abs() >= alpha || (p1 - p0).abs() >= beta || (q1 - q0v).abs() >= beta {
        return;
    }
    let (p2, q2) = if chroma { (0, 0) } else { (g(data, -3), g(data, 2)) };
    let set = |data: &mut [u16], k: isize, v: i32| data[at(k)] = v.clamp(0, 255) as u16;
    if bs < 4 {
        let (ap, aq) = ((p2 - p0).abs(), (q2 - q0v).abs());
        let tc = if chroma { tc0 + 1 } else { tc0 + i32::from(ap < beta) + i32::from(aq < beta) };
        let delta = ((((q0v - p0) << 2) + (p1 - q1) + 4) >> 3).clamp(-tc, tc);
        set(data, -1, p0 + delta);
        set(data, 0, q0v - delta);
        if !chroma {
            if ap < beta {
                set(data, -2, p1 + ((p2 + ((p0 + q0v + 1) >> 1) - (p1 << 1)) >> 1).clamp(-tc0, tc0));
            }
            if aq < beta {
                set(data, 1, q1 + ((q2 + ((p0 + q0v + 1) >> 1) - (q1 << 1)) >> 1).clamp(-tc0, tc0));
            }
        }
        return;
    }
    if chroma {
        set(data, -1, (2 * p1 + p0 + q1 + 2) >> 2);
        set(data, 0, (2 * q1 + q0v + p1 + 2) >> 2);
        return;
    }
    let (ap, aq) = ((p2 - p0).abs(), (q2 - q0v).abs());
    let strong = (p0 - q0v).abs() < (alpha >> 2) + 2;
    if ap < beta && strong {
        let p3 = g(data, -4);
        set(data, -1, (p2 + 2 * p1 + 2 * p0 + 2 * q0v + q1 + 4) >> 3);
        set(data, -2, (p2 + p1 + p0 + q0v + 2) >> 2);
        set(data, -3, (2 * p3 + 3 * p2 + p1 + p0 + q0v + 4) >> 3);
    } else {
        set(data, -1, (2 * p1 + p0 + q1 + 2) >> 2);
    }
    if aq < beta && strong {
        let q3 = g(data, 3);
        set(data, 0, (p1 + 2 * p0 + 2 * q0v + 2 * q1 + q2 + 4) >> 3);
        set(data, 1, (p0 + q0v + q1 + q2 + 2) >> 2);
        set(data, 2, (2 * q3 + 3 * q2 + q1 + q0v + p0 + 4) >> 3);
    } else {
        set(data, 0, (2 * q1 + q0v + p1 + 2) >> 2);
    }
}

impl Frame {
    /// The luma QP of a macroblock as the filter sees it: 0 for PCM.
    fn filter_qp(&self, mb: usize) -> i32 {
        if self.mbs[mb].kind == Kind::Pcm { 0 } else { self.mbs[mb].qp }
    }

    pub fn deblock(&mut self) {
        for addr in 0..self.mbs.len() {
            let slice = self.mb_slice[addr];
            if slice < 0 {
                continue;
            }
            let h = &self.headers[slice as usize];
            if h.deblocking == 1 {
                continue;
            }
            let (alpha_offset, beta_offset) = (h.alpha_offset, h.beta_offset);
            let (mbx, mby) = (addr % self.w_mbs, addr / self.w_mbs);
            let neighbour_ok = |n: usize| self.mb_slice[n] >= 0 && (h.deblocking != 2 || self.mb_slice[n] == slice);
            let left = (mbx > 0 && neighbour_ok(addr - 1)).then(|| addr - 1);
            let up = (mby > 0 && neighbour_ok(addr - self.w_mbs)).then(|| addr - self.w_mbs);
            let t8 = self.mbs[addr].t8x8;
            for vertical in [true, false] {
                let other = if vertical { left } else { up };
                for edge in 0..4 {
                    if (edge == 0 && other.is_none()) || (t8 && edge % 2 == 1) {
                        continue;
                    }
                    let p_mb = if edge == 0 { other.unwrap() } else { addr };
                    let bs = if edge == 0 { 4 } else { 3 };
                    // luma
                    let qp_av = (self.filter_qp(p_mb) + self.filter_qp(addr) + 1) >> 1;
                    self.edge(0, mbx * 16, mby * 16, 16, edge * 4, vertical, bs, qp_av, alpha_offset, beta_offset);
                    // chroma: its edges 0 and 4 go with luma's 0 and 8
                    if edge % 2 == 0 {
                        for c in 0..2 {
                            let off = self.pps.chroma_qp_offset[c];
                            let qp_c = |mb: usize| chroma_qp(self.filter_qp(mb) + off);
                            let qp_av = (qp_c(p_mb) + qp_c(addr) + 1) >> 1;
                            self.edge(
                                1 + c,
                                mbx * 8,
                                mby * 8,
                                8,
                                edge * 2,
                                vertical,
                                bs,
                                qp_av,
                                alpha_offset,
                                beta_offset,
                            );
                        }
                    }
                }
            }
        }
    }

    /// One edge `at` samples into an `n` sample block at (x, y) of plane `c`.
    #[allow(clippy::too_many_arguments)]
    fn edge(
        &mut self,
        c: usize,
        x: usize,
        y: usize,
        n: usize,
        at: usize,
        vertical: bool,
        bs: u8,
        qp_av: i32,
        alpha_offset: i32,
        beta_offset: i32,
    ) {
        let index_a = (qp_av + alpha_offset).clamp(0, 51) as usize;
        let index_b = (qp_av + beta_offset).clamp(0, 51) as usize;
        let (alpha, beta) = (i32::from(ALPHA[index_a]), i32::from(BETA[index_b]));
        let tc0 = if bs < 4 { i32::from(TC0[index_a][usize::from(bs) - 1]) } else { 0 };
        let plane = &mut self.planes[c];
        let w = plane.width;
        for k in 0..n {
            let (q0, step) = if vertical { ((y + k) * w + x + at, 1) } else { ((y + at) * w + x + k, w as isize) };
            filter_line(&mut plane.data, q0, step, bs, alpha, beta, tc0, c > 0);
        }
    }
}
