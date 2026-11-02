//! The in-loop filters, run once the whole picture is decoded: deblocking (8.7.2), then SAO (8.7.3).
//! In an intra picture every edge is between intra blocks, so every edge has boundary strength 2.

use super::decode::Frame;

const BETA: [i32; 52] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 20, 22, 24, 26, 28,
    30, 32, 34, 36, 38, 40, 42, 44, 46, 48, 50, 52, 54, 56, 58, 60, 62, 64,
];

const TC: [i32; 54] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4,
    5, 5, 6, 6, 7, 8, 9, 10, 11, 13, 14, 16, 18, 20, 22, 24,
];

pub fn chroma_qp(qpi: i32) -> i32 {
    const TABLE: [i32; 14] = [29, 30, 31, 32, 33, 33, 34, 34, 35, 35, 36, 36, 37, 37];
    match qpi {
        ..30 => qpi,
        30..=43 => TABLE[(qpi - 30) as usize],
        _ => qpi - 6,
    }
}

/// One edge segment's samples: `at(i, k)` is p(i) for i < 0 (p0 at -1) and q(i) for i >= 0, on line k.
struct Edge {
    q0: usize,
    across: isize,
    along: isize,
}

impl Edge {
    #[inline]
    fn idx(&self, i: isize, k: isize) -> usize {
        (self.q0 as isize + i * self.across + k * self.along) as usize
    }
}

fn filter_luma(d: &mut [u16], e: &Edge, beta: i32, tc: i32, keep_p: bool, keep_q: bool, max: i32) {
    let s = |d: &[u16], i: isize, k: isize| i32::from(d[e.idx(i, k)]);
    let p = |d: &[u16], i: isize, k: isize| s(d, -1 - i, k);
    let q = |d: &[u16], i: isize, k: isize| s(d, i, k);
    let dp0 = (p(d, 2, 0) - 2 * p(d, 1, 0) + p(d, 0, 0)).abs();
    let dp3 = (p(d, 2, 3) - 2 * p(d, 1, 3) + p(d, 0, 3)).abs();
    let dq0 = (q(d, 2, 0) - 2 * q(d, 1, 0) + q(d, 0, 0)).abs();
    let dq3 = (q(d, 2, 3) - 2 * q(d, 1, 3) + q(d, 0, 3)).abs();
    let (dpq0, dpq3) = (dp0 + dq0, dp3 + dq3);
    let (dp, dq) = (dp0 + dp3, dq0 + dq3);
    if dpq0 + dpq3 >= beta {
        return;
    }
    let strong_line = |d: &[u16], k: isize, dpq: i32| {
        2 * dpq < beta >> 2
            && (p(d, 3, k) - p(d, 0, k)).abs() + (q(d, 0, k) - q(d, 3, k)).abs() < beta >> 3
            && (p(d, 0, k) - q(d, 0, k)).abs() < (5 * tc + 1) >> 1
    };
    let strong = strong_line(d, 0, dpq0) && strong_line(d, 3, dpq3);
    let side = (beta + (beta >> 1)) >> 3;
    let (ep, eq) = (dp < side, dq < side);
    for k in 0..4 {
        let (p0, p1, p2, p3) = (p(d, 0, k), p(d, 1, k), p(d, 2, k), p(d, 3, k));
        let (q0, q1, q2, q3) = (q(d, 0, k), q(d, 1, k), q(d, 2, k), q(d, 3, k));
        let put = |d: &mut [u16], i: isize, v: i32| d[e.idx(i, k)] = v as u16;
        if strong {
            let c = |v: i32, x: i32| x.clamp(v - 2 * tc, v + 2 * tc);
            if !keep_p {
                put(d, -1, c(p0, (p2 + 2 * p1 + 2 * p0 + 2 * q0 + q1 + 4) >> 3));
                put(d, -2, c(p1, (p2 + p1 + p0 + q0 + 2) >> 2));
                put(d, -3, c(p2, (2 * p3 + 3 * p2 + p1 + p0 + q0 + 4) >> 3));
            }
            if !keep_q {
                put(d, 0, c(q0, (p1 + 2 * p0 + 2 * q0 + 2 * q1 + q2 + 4) >> 3));
                put(d, 1, c(q1, (p0 + q0 + q1 + q2 + 2) >> 2));
                put(d, 2, c(q2, (p0 + q0 + q1 + 3 * q2 + 2 * q3 + 4) >> 3));
            }
        } else {
            let mut delta = (9 * (q0 - p0) - 3 * (q1 - p1) + 8) >> 4;
            if delta.abs() >= tc * 10 {
                continue;
            }
            delta = delta.clamp(-tc, tc);
            if !keep_p {
                put(d, -1, (p0 + delta).clamp(0, max));
                if ep {
                    let dp = ((((p2 + p0 + 1) >> 1) - p1 + delta) >> 1).clamp(-(tc >> 1), tc >> 1);
                    put(d, -2, (p1 + dp).clamp(0, max));
                }
            }
            if !keep_q {
                put(d, 0, (q0 - delta).clamp(0, max));
                if eq {
                    let dq = ((((q2 + q0 + 1) >> 1) - q1 - delta) >> 1).clamp(-(tc >> 1), tc >> 1);
                    put(d, 1, (q1 + dq).clamp(0, max));
                }
            }
        }
    }
}

fn filter_chroma(d: &mut [u16], e: &Edge, lines: isize, tc: i32, keep_p: bool, keep_q: bool, max: i32) {
    for k in 0..lines {
        let s = |i: isize| i32::from(d[e.idx(i, k)]);
        let (p0, p1, q0, q1) = (s(-1), s(-2), s(0), s(1));
        let delta = ((((q0 - p0) << 2) + p1 - q1 + 4) >> 3).clamp(-tc, tc);
        if !keep_p {
            d[e.idx(-1, k)] = (p0 + delta).clamp(0, max) as u16;
        }
        if !keep_q {
            d[e.idx(0, k)] = (q0 - delta).clamp(0, max) as u16;
        }
    }
}

impl Frame {
    /// Whether an edge between 4x4 blocks p and q is filtered: q's slice decides.
    fn edge_on(&self, p: (usize, usize), q: (usize, usize)) -> bool {
        let (cp, cq) = (self.ctb_of(p.0 * 4, p.1 * 4), self.ctb_of(q.0 * 4, q.1 * 4));
        let h = &self.headers[usize::from(self.ctb_header[cq])];
        !h.deblocking_disabled && (self.ctb_slice[cp] == self.ctb_slice[cq] || h.loop_filter_across_slices)
    }

    pub fn deblock(&mut self) {
        if self.headers.iter().all(|h| h.deblocking_disabled) {
            return;
        }
        let (dl, dc) = (self.sps.bit_depth_luma, self.sps.bit_depth_chroma);
        for vertical in [true, false] {
            for by in 0..self.h4 {
                for bx in 0..self.w4 {
                    let i = by * self.w4 + bx;
                    let (on_grid, edge, p) = if vertical {
                        (bx % 2 == 0 && bx > 0, self.edge_v[i], (bx.wrapping_sub(1), by))
                    } else {
                        (by % 2 == 0 && by > 0, self.edge_h[i], (bx, by.wrapping_sub(1)))
                    };
                    if !on_grid || !edge || !self.edge_on(p, (bx, by)) {
                        continue;
                    }
                    let pi = p.1 * self.w4 + p.0;
                    let h = &self.headers[usize::from(self.ctb_header[self.ctb_of(bx * 4, by * 4)])];
                    let (beta_offset, tc_offset) = (h.beta_offset, h.tc_offset);
                    let (qp_p, qp_q) = (i32::from(self.qp[pi]), i32::from(self.qp[i]));
                    let (keep_p, keep_q) = (self.keep[pi], self.keep[i]);
                    let qpl = (qp_q + qp_p + 1) >> 1;
                    let beta = BETA[(qpl + beta_offset).clamp(0, 51) as usize] << (dl - 8);
                    let tc = TC[(qpl + 2 + tc_offset).clamp(0, 53) as usize] << (dl - 8);
                    let w = self.planes[0].width;
                    let (x, y) = (bx * 4, by * 4);
                    let e = if vertical {
                        Edge { q0: y * w + x, across: 1, along: w as isize }
                    } else {
                        Edge { q0: y * w + x, across: w as isize, along: 1 }
                    };
                    filter_luma(&mut self.planes[0].data, &e, beta, tc, keep_p, keep_q, (1 << dl) - 1);
                    // chroma edges sit on an 8 sample chroma grid: every 16 luma samples
                    let chroma_grid = if vertical { bx % 4 == 0 } else { by % 4 == 0 };
                    if !chroma_grid {
                        continue;
                    }
                    for c in 1..3 {
                        let offset = if c == 1 { self.pps.cb_qp_offset } else { self.pps.cr_qp_offset };
                        let qpc = chroma_qp(qpl + offset);
                        let tc = TC[(qpc + 2 + tc_offset).clamp(0, 53) as usize] << (dc - 8);
                        let w = self.planes[c].width;
                        let (x, y) = (bx * 2, by * 2);
                        let e = if vertical {
                            Edge { q0: y * w + x, across: 1, along: w as isize }
                        } else {
                            Edge { q0: y * w + x, across: w as isize, along: 1 }
                        };
                        filter_chroma(&mut self.planes[c].data, &e, 2, tc, keep_p, keep_q, (1 << dc) - 1);
                    }
                }
            }
        }
    }

    pub fn apply_sao(&mut self) {
        if self.headers.iter().all(|h| !h.sao_luma && !h.sao_chroma) {
            return;
        }
        let wc = self.sps.width_ctbs() as usize;
        let ctb = self.sps.ctb_size() as usize;
        for c in 0..3 {
            let src = self.planes[c].clone();
            let scale = if c == 0 { 1 } else { 2 };
            let depth = if c == 0 { self.sps.bit_depth_luma } else { self.sps.bit_depth_chroma };
            let max = (1i32 << depth) - 1;
            let (pw, ph) = (src.width as isize, src.height as isize);
            let size = ctb / scale;
            for (addr, params) in self.sao.iter().enumerate() {
                let sao = params[c];
                if sao.kind == 0 || self.ctb_slice[addr] < 0 {
                    continue;
                }
                let slice = self.ctb_slice[addr];
                let (x0, y0) = ((addr % wc) * size, (addr / wc) * size);
                let mut offsets = [0i32; 5];
                for (o, &v) in offsets[1..].iter_mut().zip(sao.offsets.iter()) {
                    *o = i32::from(v);
                }
                let (dx, dy) = match sao.param {
                    0 => ((-1, 0), (1, 0)),
                    1 => ((0, -1), (0, 1)),
                    2 => ((-1, -1), (1, 1)),
                    _ => ((1, -1), (-1, 1)),
                };
                for y in y0..(y0 + size).min(src.height) {
                    for x in x0..(x0 + size).min(src.width) {
                        let (lx, ly) = (x * scale, y * scale);
                        if self.keep[(ly >> 2) * self.w4 + (lx >> 2)] {
                            continue;
                        }
                        let v = i32::from(src.get(x, y));
                        let off = if sao.kind == 1 {
                            let band = (v >> (depth - 5)) as u8;
                            let k = band.wrapping_sub(sao.param) & 31;
                            if k < 4 { offsets[usize::from(k) + 1] } else { 0 }
                        } else {
                            let mut sum = 2;
                            let mut skip = false;
                            for (ox, oy) in [dx, dy] {
                                let (nx, ny) = (x as isize + ox, y as isize + oy);
                                if nx < 0 || ny < 0 || nx >= pw || ny >= ph {
                                    skip = true;
                                    break;
                                }
                                let n_slice = self.ctb_slice[self.ctb_of(nx as usize * scale, ny as usize * scale)];
                                if n_slice != slice {
                                    // the later slice's flag rules the boundary
                                    let later = self.ctb_header[self.ctb_of(
                                        if n_slice > slice { nx as usize * scale } else { lx },
                                        if n_slice > slice { ny as usize * scale } else { ly },
                                    )];
                                    if !self.headers[usize::from(later)].loop_filter_across_slices {
                                        skip = true;
                                        break;
                                    }
                                }
                                sum += (v - i32::from(src.get(nx as usize, ny as usize))).signum();
                            }
                            if skip {
                                continue;
                            }
                            let idx = match sum {
                                0 => 1,
                                1 => 2,
                                2 => 0,
                                s => s as usize,
                            };
                            offsets[idx]
                        };
                        if off != 0 {
                            self.planes[c].set(x, y, (v + off).clamp(0, max) as u16);
                        }
                    }
                }
            }
        }
    }
}
