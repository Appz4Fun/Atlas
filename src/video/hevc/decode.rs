//! Slice data (H.265 7.3.8): the coding tree, intra modes and residuals, decoded straight into the picture.

use super::cabac::{Cabac, Contexts, ctx};
use super::filter::chroma_qp;
use super::intra;
use super::params::{Pps, ScalingList, Sps};
use super::slice::SliceHeader;
use super::transform::{LEVEL_SCALE, Transforms, transform_skip};
use crate::video::bits::{BitReader, DecodeError, Result};
use crate::video::picture::Plane;

/// SAO for one coding tree block and one component.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Sao {
    /// 0 off, 1 band, 2 edge
    pub kind: u8,
    /// band position, or edge class
    pub param: u8,
    pub offsets: [i8; 4],
}

/// Scan positions (x, y) for blocks of 1, 2, 4 and 8 a side, in diagonal, horizontal and vertical order.
pub struct Scans(Vec<Vec<Vec<(u8, u8)>>>);

fn scans() -> &'static Scans {
    static SCANS: std::sync::OnceLock<Scans> = std::sync::OnceLock::new();
    SCANS.get_or_init(Scans::default)
}

impl Default for Scans {
    fn default() -> Scans {
        let mut all = Vec::new();
        for log2 in 0..4 {
            let n = 1usize << log2;
            let mut diag = Vec::with_capacity(n * n);
            let (mut x, mut y) = (0isize, 0isize);
            while diag.len() < n * n {
                while y >= 0 {
                    if (x as usize) < n && (y as usize) < n {
                        diag.push((x as u8, y as u8));
                    }
                    y -= 1;
                    x += 1;
                }
                y = x;
                x = 0;
            }
            let hor = (0..n * n).map(|i| ((i % n) as u8, (i / n) as u8)).collect();
            let ver = (0..n * n).map(|i| ((i / n) as u8, (i % n) as u8)).collect();
            all.push(vec![diag, hor, ver]);
        }
        Scans(all)
    }
}

/// A picture being decoded, with what later blocks and the loop filters need to know about earlier ones.
pub struct Frame {
    pub sps: Sps,
    pub pps: Pps,
    pub planes: [Plane; 3],
    /// 4x4 luma blocks a row
    pub w4: usize,
    pub h4: usize,
    /// per 4x4 luma block
    pub done: Vec<bool>,
    pub seen: Vec<bool>,
    pub mode: Vec<u8>,
    pub depth: Vec<u8>,
    pub qp: Vec<i8>,
    /// PCM with the loop filter off, or transquant bypass: the loop filters leave it alone
    pub keep: Vec<bool>,
    /// a transform edge on the block's left, and on its top
    pub edge_v: Vec<bool>,
    pub edge_h: Vec<bool>,
    /// per coding tree block: the address of the slice it's in (-1 until decoded), and that slice's header
    pub ctb_slice: Vec<i32>,
    pub ctb_header: Vec<u16>,
    pub sao: Vec<[Sao; 3]>,
    pub headers: Vec<SliceHeader>,
    /// scaling factors per size (4 to 32) and component, when the stream has scaling lists
    factors: Option<Vec<Vec<Vec<u8>>>>,
    /// contexts saved for wavefront sync, and at the end of a slice segment
    wpp: Option<Contexts>,
    end_of_segment: Option<Contexts>,
    /// QpY of the last coding unit decoded
    last_qp: i32,
    tf: Transforms,
}

fn factors(list: &ScalingList, scans: &Scans) -> Vec<Vec<Vec<u8>>> {
    let mut out = Vec::new();
    for size in 0..4usize {
        let n = 4usize << size;
        let mut per = Vec::new();
        for c in 0..3 {
            let mut m = vec![16u8; n * n];
            // 4x4 lists are 4x4; the rest are 8x8, spread over the block
            let (scan, side) = if size == 0 { (&scans.0[2][0], 4) } else { (&scans.0[3][0], 8) };
            let rep = n / side;
            let matrix = if size == 3 { 0 } else { c };
            for (i, &(sx, sy)) in scan.iter().enumerate() {
                for dy in 0..rep {
                    for dx in 0..rep {
                        let (x, y) = (usize::from(sx) * rep + dx, usize::from(sy) * rep + dy);
                        m[y * n + x] = list.lists[size][matrix][i];
                    }
                }
            }
            if size >= 2 {
                m[0] = list.dc[size][matrix];
            }
            per.push(m);
        }
        out.push(per);
    }
    out
}

impl Frame {
    pub fn new(sps: Sps, pps: Pps) -> Frame {
        let (w, h) = (sps.width as usize, sps.height as usize);
        let mid = |d: u8| 1u16 << (d - 1);
        let planes = [
            Plane::new(w, h, mid(sps.bit_depth_luma)),
            Plane::new(w / 2, h / 2, mid(sps.bit_depth_chroma)),
            Plane::new(w / 2, h / 2, mid(sps.bit_depth_chroma)),
        ];
        let (w4, h4) = (w.div_ceil(4), h.div_ceil(4));
        let ctbs = (sps.width_ctbs() * sps.height_ctbs()) as usize;
        let factors = sps.scaling_list.as_ref().map(|l| factors(pps.scaling_list.as_ref().unwrap_or(l), scans()));
        Frame {
            planes,
            w4,
            h4,
            done: vec![false; w4 * h4],
            seen: vec![false; w4 * h4],
            mode: vec![intra::DC; w4 * h4],
            depth: vec![0; w4 * h4],
            qp: vec![0; w4 * h4],
            keep: vec![false; w4 * h4],
            edge_v: vec![false; w4 * h4],
            edge_h: vec![false; w4 * h4],
            ctb_slice: vec![-1; ctbs],
            ctb_header: vec![0; ctbs],
            sao: vec![[Sao::default(); 3]; ctbs],
            headers: Vec::new(),
            factors,
            wpp: None,
            end_of_segment: None,
            last_qp: 0,
            tf: Transforms::default(),
            sps,
            pps,
        }
    }

    #[inline]
    pub fn ctb_of(&self, x: usize, y: usize) -> usize {
        let l = self.sps.log2_ctb;
        (y >> l) * self.sps.width_ctbs() as usize + (x >> l)
    }

    /// Whether the luma sample at (x, y) has been reconstructed, in the slice at `slice`.
    #[inline]
    fn available(&self, x: isize, y: isize, slice: i32) -> bool {
        self.in_slice(&self.done, x, y, slice)
    }

    /// Whether the coding unit with the luma sample at (x, y) has been read, in the slice at `slice`: what the
    /// syntax of later units may depend on, before any of it is reconstructed.
    #[inline]
    fn parsed(&self, x: isize, y: isize, slice: i32) -> bool {
        self.in_slice(&self.seen, x, y, slice)
    }

    #[inline]
    fn in_slice(&self, map: &[bool], x: isize, y: isize, slice: i32) -> bool {
        if x < 0 || y < 0 || x >= self.sps.width as isize || y >= self.sps.height as isize {
            return false;
        }
        let (x, y) = (x as usize, y as usize);
        map[(y >> 2) * self.w4 + (x >> 2)] && self.ctb_slice[self.ctb_of(x, y)] == slice
    }

    /// Marks 4x4 blocks of a luma area.
    fn fill<T: Copy>(map: &mut [T], w4: usize, x: usize, y: usize, size: usize, v: T) {
        for by in y >> 2..(y + size).div_ceil(4) {
            for bx in x >> 2..(x + size).div_ceil(4) {
                if let Some(cell) = map.get_mut(by * w4 + bx) {
                    *cell = v;
                }
            }
        }
    }

    /// Decodes one slice segment, whose header is `h`, from its RBSP.
    pub fn decode_slice(&mut self, rbsp: &[u8], h: SliceHeader) -> Result<()> {
        let index = self.headers.len() as u16;
        let slice_addr = if h.dependent {
            // a dependent segment belongs to the slice before it
            let prev = h.address.checked_sub(1).ok_or(DecodeError::Invalid("dependent slice at 0"))?;
            *self.ctb_slice.get(prev as usize).ok_or(DecodeError::Invalid("slice address"))?
        } else {
            h.address as i32
        };
        if slice_addr < 0 {
            return Err(DecodeError::Invalid("dependent slice with nothing before it"));
        }
        self.headers.push(h.clone());
        let mut s = SliceDecoder {
            cabac: Cabac::new(rbsp, h.data_offset)?,
            data: rbsp,
            ctx: Contexts::new(h.qp),
            slice_addr,
            header: index,
            h,
            qp_y: 0,
            qg_pred: 0,
            delta: 0,
            delta_coded: false,
            bypass: false,
            first_qg: true,
            coeffs: vec![0; 32 * 32],
            pred: vec![0; 32 * 32],
            f: self,
        };
        s.run()
    }
}

struct SliceDecoder<'a> {
    f: &'a mut Frame,
    data: &'a [u8],
    cabac: Cabac<'a>,
    ctx: Contexts,
    h: SliceHeader,
    header: u16,
    slice_addr: i32,
    /// QpY of the coding unit being decoded, and the quantization group's prediction
    qp_y: i32,
    qg_pred: i32,
    delta: i32,
    delta_coded: bool,
    bypass: bool,
    /// the next quantization group is the slice's first, or a wavefront row's
    first_qg: bool,
    coeffs: Vec<i32>,
    pred: Vec<i32>,
}

impl SliceDecoder<'_> {
    #[inline]
    fn bin(&mut self, i: usize) -> u32 {
        self.cabac.decision(&mut self.ctx.0[i])
    }

    fn run(&mut self) -> Result<()> {
        let sps = &self.f.sps;
        let (wc, hc) = (sps.width_ctbs() as usize, sps.height_ctbs() as usize);
        let log2_ctb = sps.log2_ctb;
        let wpp = self.f.pps.entropy_coding_sync;
        let mut addr = self.h.address as usize;
        if self.h.dependent {
            if let Some(saved) = &self.f.end_of_segment {
                self.ctx = saved.clone();
            }
            // QP prediction carries on through a dependent segment
            self.first_qg = false;
        }
        let mut first = true;
        loop {
            if addr >= wc * hc {
                return Err(DecodeError::Invalid("slice runs past the picture"));
            }
            let (cx, cy) = (addr % wc, addr / wc);
            if wpp && cx == 0 {
                // the block above and to the right, if it's in this slice, hands on its contexts
                let tr = cy > 0 && wc > 1 && self.f.ctb_slice[addr - wc + 1] == self.slice_addr;
                if tr {
                    self.ctx = self.f.wpp.clone().ok_or(DecodeError::Invalid("wavefront sync"))?;
                } else if !(first && self.h.dependent) {
                    self.ctx = Contexts::new(self.h.qp);
                }
                self.first_qg = true;
            }
            first = false;
            self.f.ctb_slice[addr] = self.slice_addr;
            self.f.ctb_header[addr] = self.header;
            let (x0, y0) = (cx << log2_ctb, cy << log2_ctb);
            if self.h.sao_luma || self.h.sao_chroma {
                self.sao(addr, cx, cy)?;
            }
            self.coding_quadtree(x0, y0, log2_ctb, 0)?;
            if self.cabac.overran() {
                return Err(DecodeError::Truncated);
            }
            if wpp && (cx == 1 || wc == 1) {
                self.f.wpp = Some(self.ctx.clone());
            }
            let end = self.cabac.terminate() == 1;
            addr += 1;
            if end {
                self.f.end_of_segment = Some(self.ctx.clone());
                return Ok(());
            }
            if wpp && addr.is_multiple_of(wc) {
                if self.cabac.terminate() != 1 {
                    return Err(DecodeError::Invalid("end of substream"));
                }
                self.cabac = Cabac::new(self.data, self.cabac.next_byte())?;
            }
        }
    }

    fn sao(&mut self, addr: usize, cx: usize, cy: usize) -> Result<()> {
        let wc = self.f.sps.width_ctbs() as usize;
        let slice = self.slice_addr as usize;
        if cx > 0 && addr > slice && self.f.ctb_slice[addr - 1] == self.slice_addr && self.bin(ctx::SAO_MERGE) == 1 {
            self.f.sao[addr] = self.f.sao[addr - 1];
            return Ok(());
        }
        if cy > 0
            && addr - wc >= slice
            && self.f.ctb_slice[addr - wc] == self.slice_addr
            && self.bin(ctx::SAO_MERGE) == 1
        {
            self.f.sao[addr] = self.f.sao[addr - wc];
            return Ok(());
        }
        let mut out = [Sao::default(); 3];
        for c in 0..3 {
            if (c == 0 && !self.h.sao_luma) || (c > 0 && !self.h.sao_chroma) {
                continue;
            }
            if c == 2 {
                out[2].kind = out[1].kind;
            } else {
                out[c].kind = if self.bin(ctx::SAO_TYPE) == 0 {
                    0
                } else if self.cabac.bypass() == 0 {
                    1
                } else {
                    2
                };
            }
            if out[c].kind == 0 {
                continue;
            }
            let depth = if c == 0 { self.f.sps.bit_depth_luma } else { self.f.sps.bit_depth_chroma };
            let max = (1u32 << (depth.min(10) - 5)) - 1;
            let mut abs = [0i32; 4];
            for a in &mut abs {
                let mut v = 0;
                while v < max && self.cabac.bypass() == 1 {
                    v += 1;
                }
                *a = v as i32;
            }
            if out[c].kind == 1 {
                for a in &mut abs {
                    if *a != 0 && self.cabac.bypass() == 1 {
                        *a = -*a;
                    }
                }
                out[c].param = self.cabac.bypass_bits(5) as u8;
            } else {
                abs[2] = -abs[2];
                abs[3] = -abs[3];
                out[c].param = if c == 2 { out[1].param } else { self.cabac.bypass_bits(2) as u8 };
            }
            for (o, a) in out[c].offsets.iter_mut().zip(abs) {
                *o = a as i8;
            }
        }
        self.f.sao[addr] = out;
        Ok(())
    }

    fn coding_quadtree(&mut self, x0: usize, y0: usize, log2: u32, depth: u8) -> Result<()> {
        let (min_cb, log2_ctb) = (self.f.sps.log2_min_cb, self.f.sps.log2_ctb);
        let size = 1usize << log2;
        let (w, h) = (self.f.sps.width as usize, self.f.sps.height as usize);
        let split = if x0 + size <= w && y0 + size <= h && log2 > min_cb {
            let mut inc = 0;
            if self.f.parsed(x0 as isize - 1, y0 as isize, self.slice_addr)
                && self.f.depth[(y0 >> 2) * self.f.w4 + ((x0 - 1) >> 2)] > depth
            {
                inc += 1;
            }
            if self.f.parsed(x0 as isize, y0 as isize - 1, self.slice_addr)
                && self.f.depth[((y0 - 1) >> 2) * self.f.w4 + (x0 >> 2)] > depth
            {
                inc += 1;
            }
            self.bin(ctx::SPLIT_CU + inc) == 1
        } else {
            log2 > min_cb
        };
        let qg_log2 = log2_ctb - self.f.pps.cu_qp_delta_depth.unwrap_or(0);
        if log2 >= qg_log2 {
            self.start_quantization_group(x0, y0);
        }
        if split {
            let half = size / 2;
            for (dx, dy) in [(0, 0), (half, 0), (0, half), (half, half)] {
                if x0 + dx < w && y0 + dy < h {
                    self.coding_quadtree(x0 + dx, y0 + dy, log2 - 1, depth + 1)?;
                }
            }
            Ok(())
        } else {
            self.coding_unit(x0, y0, log2, depth)
        }
    }

    /// 8.6.1: the prediction of QpY that a quantization group's coding units share.
    fn start_quantization_group(&mut self, x: usize, y: usize) {
        // nested groups may start here before any coding unit: the slice's first stays first until one is decoded
        let prev = if self.first_qg { self.h.qp } else { self.f.last_qp };
        let mask = (1usize << self.f.sps.log2_ctb) - 1;
        let w4 = self.f.w4;
        let a = if x & mask != 0 { i32::from(self.f.qp[(y >> 2) * w4 + ((x - 1) >> 2)]) } else { prev };
        let b = if y & mask != 0 { i32::from(self.f.qp[((y - 1) >> 2) * w4 + (x >> 2)]) } else { prev };
        self.qg_pred = (a + b + 1) >> 1;
        self.delta = 0;
        self.delta_coded = false;
    }

    fn update_qp(&mut self) {
        let offset = 6 * (i32::from(self.f.sps.bit_depth_luma) - 8);
        self.qp_y = ((self.qg_pred + self.delta + 52 + 2 * offset) % (52 + offset)) - offset;
    }

    fn coding_unit(&mut self, x0: usize, y0: usize, log2: u32, depth: u8) -> Result<()> {
        let size = 1usize << log2;
        let w4 = self.f.w4;
        self.bypass = self.f.pps.transquant_bypass && self.bin(ctx::TRANSQUANT_BYPASS) == 1;
        self.update_qp();
        Frame::fill(&mut self.f.depth, w4, x0, y0, size, depth);
        Frame::fill(&mut self.f.seen, w4, x0, y0, size, true);
        let nxn = log2 == self.f.sps.log2_min_cb && self.bin(ctx::PART_MODE) == 0;
        if nxn && log2 == self.f.sps.log2_min_tb {
            return Err(DecodeError::Invalid("NxN at the smallest transform size"));
        }
        let pcm = !nxn
            && self.f.sps.pcm.is_some_and(|p| (p.log2_min..=p.log2_max).contains(&log2))
            && self.cabac.terminate() == 1;
        if pcm {
            self.pcm(x0, y0, log2)?;
        } else {
            let parts = if nxn { 4 } else { 1 };
            let half = if nxn { size / 2 } else { size };
            let mut prev = [false; 4];
            for p in prev.iter_mut().take(parts) {
                *p = self.bin(ctx::PREV_INTRA_LUMA) == 1;
            }
            let mut modes = [0u8; 4];
            for i in 0..parts {
                let (px, py) = (x0 + (i % 2) * half, y0 + (i / 2) * half);
                let cand = self.mpm(px, py);
                modes[i] = if prev[i] {
                    let idx = if self.cabac.bypass() == 0 {
                        0
                    } else if self.cabac.bypass() == 0 {
                        1
                    } else {
                        2
                    };
                    cand[idx]
                } else {
                    let mut m = self.cabac.bypass_bits(5) as u8;
                    let mut sorted = cand;
                    sorted.sort_unstable();
                    for c in sorted {
                        if m >= c {
                            m += 1;
                        }
                    }
                    m
                };
                Frame::fill(&mut self.f.mode, w4, px, py, half, modes[i]);
            }
            let chroma = if self.bin(ctx::INTRA_CHROMA) == 0 { 4 } else { self.cabac.bypass_bits(2) };
            let chroma_mode = match chroma {
                4 => modes[0],
                c => {
                    let m = [0u8, 26, 10, 1][c as usize];
                    if m == modes[0] { 34 } else { m }
                }
            };
            let max_depth = self.f.sps.max_transform_depth_intra + u32::from(nxn);
            let cu = Cu { nxn, max_depth, chroma_mode };
            self.transform_tree(&cu, x0, y0, x0, y0, log2, 0, 0, [true, true])?;
        }
        Frame::fill(&mut self.f.qp, w4, x0, y0, size, self.qp_y as i8);
        let keep = self.bypass || (pcm && self.f.sps.pcm.is_some_and(|p| p.loop_filter_disabled));
        Frame::fill(&mut self.f.keep, w4, x0, y0, size, keep);
        self.f.last_qp = self.qp_y;
        self.first_qg = false;
        Ok(())
    }

    /// The three most probable luma modes for a prediction block (8.4.2).
    fn mpm(&self, x: usize, y: usize) -> [u8; 3] {
        let w4 = self.f.w4;
        let a = if self.f.parsed(x as isize - 1, y as isize, self.slice_addr) {
            self.f.mode[(y >> 2) * w4 + ((x - 1) >> 2)]
        } else {
            intra::DC
        };
        let ctb_top = (y >> self.f.sps.log2_ctb) << self.f.sps.log2_ctb;
        let b = if y > ctb_top && self.f.parsed(x as isize, y as isize - 1, self.slice_addr) {
            self.f.mode[((y - 1) >> 2) * w4 + (x >> 2)]
        } else {
            intra::DC
        };
        if a == b {
            if a < 2 { [intra::PLANAR, intra::DC, 26] } else { [a, 2 + ((a + 29) % 32), 2 + ((a - 2 + 1) % 32)] }
        } else {
            let c = if a != intra::PLANAR && b != intra::PLANAR {
                intra::PLANAR
            } else if a != intra::DC && b != intra::DC {
                intra::DC
            } else {
                26
            };
            [a, b, c]
        }
    }

    fn pcm(&mut self, x0: usize, y0: usize, log2: u32) -> Result<()> {
        let pcm = self.f.sps.pcm.ok_or(DecodeError::Invalid("PCM"))?;
        let start = self.cabac.next_byte();
        let mut r = BitReader::new(self.data.get(start..).ok_or(DecodeError::Truncated)?);
        let size = 1usize << log2;
        let (dl, dc) = (self.f.sps.bit_depth_luma, self.f.sps.bit_depth_chroma);
        for y in 0..size {
            for x in 0..size {
                let v = r.bits(u32::from(pcm.bit_depth_luma))? << (dl - pcm.bit_depth_luma);
                self.f.planes[0].set(x0 + x, y0 + y, v as u16);
            }
        }
        for c in 1..3 {
            for y in 0..size / 2 {
                for x in 0..size / 2 {
                    let v = r.bits(u32::from(pcm.bit_depth_chroma))? << (dc - pcm.bit_depth_chroma);
                    self.f.planes[c].set(x0 / 2 + x, y0 / 2 + y, v as u16);
                }
            }
        }
        let w4 = self.f.w4;
        Frame::fill(&mut self.f.done, w4, x0, y0, size, true);
        // a PCM block counts as DC to its neighbours' mode prediction
        Frame::fill(&mut self.f.mode, w4, x0, y0, size, intra::DC);
        self.mark_edges(x0, y0, size);
        self.cabac = Cabac::new(self.data, start + r.position().div_ceil(8))?;
        Ok(())
    }

    fn mark_edges(&mut self, x0: usize, y0: usize, size: usize) {
        let w4 = self.f.w4;
        for i in 0..size.div_ceil(4) {
            if let Some(v) = self.f.edge_v.get_mut(((y0 >> 2) + i) * w4 + (x0 >> 2)) {
                *v = true;
            }
            if (x0 >> 2) + i < w4 {
                self.f.edge_h[(y0 >> 2) * w4 + (x0 >> 2) + i] = true;
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn transform_tree(
        &mut self,
        cu: &Cu,
        x0: usize,
        y0: usize,
        x_base: usize,
        y_base: usize,
        log2: u32,
        depth: u32,
        blk: u32,
        parent_cbf: [bool; 2],
    ) -> Result<()> {
        let (min_tb, max_tb) = (self.f.sps.log2_min_tb, self.f.sps.log2_max_tb);
        let split = if log2 <= max_tb && log2 > min_tb && depth < cu.max_depth && !(cu.nxn && depth == 0) {
            self.bin(ctx::SPLIT_TRANSFORM + 5 - log2 as usize) == 1
        } else {
            log2 > max_tb || (cu.nxn && depth == 0)
        };
        let mut cbf = [false; 2];
        if log2 > 2 {
            for (c, flag) in cbf.iter_mut().enumerate() {
                if depth == 0 || parent_cbf[c] {
                    *flag = self.bin(ctx::CBF_CHROMA + depth as usize) == 1;
                }
            }
        } else {
            // 4x4 luma: chroma is coded once, with the fourth block, under the parent's flags
            cbf = parent_cbf;
        }
        if split {
            let half = 1usize << (log2 - 1);
            for (i, (dx, dy)) in [(0, 0), (half, 0), (0, half), (half, half)].into_iter().enumerate() {
                self.transform_tree(cu, x0 + dx, y0 + dy, x0, y0, log2 - 1, depth + 1, i as u32, cbf)?;
            }
            return Ok(());
        }
        let cbf_luma = self.bin(ctx::CBF_LUMA + usize::from(depth == 0)) == 1;
        if (cbf_luma || cbf[0] || cbf[1]) && self.f.pps.cu_qp_delta_depth.is_some() && !self.delta_coded {
            let mut abs = 0;
            while abs < 5 && self.bin(ctx::CU_QP_DELTA + usize::from(abs > 0)) == 1 {
                abs += 1;
            }
            if abs == 5 {
                // k = 0 Exp-Golomb suffix
                let mut k = 0;
                while k < 32 && self.cabac.bypass() == 1 {
                    abs += 1 << k;
                    k += 1;
                }
                abs += self.cabac.bypass_bits(k) as i32;
            }
            self.delta_coded = true;
            self.delta = if abs > 0 && self.cabac.bypass() == 1 { -abs } else { abs };
            let offset = 6 * (i32::from(self.f.sps.bit_depth_luma) - 8);
            if !(-(26 + offset / 2)..=25 + offset / 2).contains(&self.delta) {
                return Err(DecodeError::Invalid("cu_qp_delta"));
            }
            self.update_qp();
        }
        let size = 1usize << log2;
        let luma_mode = self.f.mode[(y0 >> 2) * self.f.w4 + (x0 >> 2)];
        self.block(0, x0, y0, log2, luma_mode, cbf_luma)?;
        let w4 = self.f.w4;
        Frame::fill(&mut self.f.done, w4, x0, y0, size, true);
        self.mark_edges(x0, y0, size);
        if log2 > 2 {
            for c in 1..3 {
                self.block(c, x0 / 2, y0 / 2, log2 - 1, cu.chroma_mode, cbf[c - 1])?;
            }
        } else if blk == 3 {
            for c in 1..3 {
                self.block(c, x_base / 2, y_base / 2, 2, cu.chroma_mode, cbf[c - 1])?;
            }
        }
        Ok(())
    }

    /// Predicts one transform block, reads its residual if it has one, and puts the sum in the picture.
    fn block(&mut self, c: usize, x0: usize, y0: usize, log2: u32, mode: u8, coded: bool) -> Result<()> {
        let n = 1usize << log2;
        let depth = if c == 0 { self.f.sps.bit_depth_luma } else { self.f.sps.bit_depth_chroma };
        self.predict(c, x0, y0, n, mode, depth);
        if coded {
            self.residual(c, log2, mode, depth)?;
            let plane = &mut self.f.planes[c];
            let max = (1i32 << depth) - 1;
            for y in 0..n {
                for x in 0..n {
                    let v = (self.pred[y * n + x] + self.coeffs[y * n + x]).clamp(0, max);
                    plane.set(x0 + x, y0 + y, v as u16);
                }
            }
        } else {
            let plane = &mut self.f.planes[c];
            for y in 0..n {
                for x in 0..n {
                    plane.set(x0 + x, y0 + y, self.pred[y * n + x] as u16);
                }
            }
        }
        Ok(())
    }

    fn predict(&mut self, c: usize, x0: usize, y0: usize, n: usize, mode: u8, depth: u8) {
        let scale = if c == 0 { 1 } else { 2 };
        let mut r = [0i32; 129];
        let mut have = [false; 129];
        let (r, have) = (&mut r[..4 * n + 1], &mut have[..4 * n + 1]);
        let plane = &self.f.planes[c];
        let (pw, ph) = (plane.width as isize, plane.height as isize);
        let mut fetch = |i: usize, x: isize, y: isize| {
            if x < pw && y < ph && self.f.available(x * scale, y * scale, self.slice_addr) {
                r[i] = i32::from(plane.get(x as usize, y as usize));
                have[i] = true;
            }
        };
        let (x0, y0) = (x0 as isize, y0 as isize);
        for k in 0..2 * n {
            fetch(2 * n - 1 - k, x0 - 1, y0 + k as isize);
            fetch(2 * n + 1 + k, x0 + k as isize, y0 - 1);
        }
        fetch(2 * n, x0 - 1, y0 - 1);
        intra::substitute(r, have, depth);
        if c == 0 {
            intra::filter(r, n, mode, self.f.sps.strong_intra_smoothing, depth);
        }
        intra::predict(r, n, mode, c == 0, depth, &mut self.pred);
    }

    /// residual_coding (7.3.8.11), then scaling and the inverse transform: leaves the residual in `coeffs`.
    fn residual(&mut self, c: usize, log2: u32, mode: u8, depth: u8) -> Result<()> {
        let n = 1usize << log2;
        self.coeffs[..n * n].fill(0);
        let luma = c == 0;
        let skip = self.f.pps.transform_skip
            && !self.bypass
            && log2 == 2
            && self.bin(ctx::TRANSFORM_SKIP + usize::from(!luma)) == 1;
        // last significant position
        let (offset, shift) = if luma { (3 * (log2 - 2) + ((log2 - 1) >> 2), (log2 + 1) >> 2) } else { (15, log2 - 2) };
        let max_prefix = (log2 << 1) - 1;
        let mut prefix = [0u32; 2];
        for (axis, base) in [ctx::LAST_X, ctx::LAST_Y].into_iter().enumerate() {
            let mut p = 0;
            while p < max_prefix && self.bin(base + (offset + (p >> shift)) as usize) == 1 {
                p += 1;
            }
            prefix[axis] = p;
        }
        let mut last = [0usize; 2];
        for axis in 0..2 {
            let p = prefix[axis];
            last[axis] = if p > 3 {
                let bits = (p >> 1) - 1;
                ((1 << bits) * (2 + (p & 1)) + self.cabac.bypass_bits(bits)) as usize
            } else {
                p as usize
            };
        }
        let scan_idx = if log2 == 2 || (log2 == 3 && luma) {
            match mode {
                6..=14 => 2,
                22..=30 => 1,
                _ => 0,
            }
        } else {
            0
        };
        if scan_idx == 2 {
            last.swap(0, 1);
        }
        if last[0] >= n || last[1] >= n {
            return Err(DecodeError::Invalid("last coefficient"));
        }
        let sub_log2 = log2 as usize - 2;
        let subs = 1usize << sub_log2;
        let scans = &scans().0;
        let sub_scan = &scans[sub_log2][scan_idx];
        let pos_scan = &scans[2][scan_idx];
        // where the last coefficient is in scan order
        let (mut last_sub, mut last_pos) = (0, 0);
        'find: for (i, &(sx, sy)) in sub_scan.iter().enumerate().rev() {
            for (p, &(px, py)) in pos_scan.iter().enumerate().rev() {
                if (usize::from(sx) << 2) + usize::from(px) == last[0]
                    && (usize::from(sy) << 2) + usize::from(py) == last[1]
                {
                    (last_sub, last_pos) = (i, p);
                    break 'find;
                }
            }
        }
        let mut csbf = [false; 64];
        let mut greater1_ctx = 1u32;
        let hide = self.f.pps.sign_data_hiding && !self.bypass;
        for i in (0..=last_sub).rev() {
            let (xs, ys) = (usize::from(sub_scan[i].0), usize::from(sub_scan[i].1));
            let mut infer_dc = false;
            if i < last_sub && i > 0 {
                let mut inc = 0;
                if xs + 1 < subs && csbf[ys * subs + xs + 1] {
                    inc = 1;
                }
                if ys + 1 < subs && csbf[(ys + 1) * subs + xs] {
                    inc = 1;
                }
                csbf[ys * subs + xs] = self.bin(ctx::CODED_SUB_BLOCK + inc + if luma { 0 } else { 2 }) == 1;
                infer_dc = true;
            } else {
                csbf[ys * subs + xs] = true;
            }
            let prev_csbf = u32::from(xs + 1 < subs && csbf[ys * subs + xs + 1])
                | u32::from(ys + 1 < subs && csbf[(ys + 1) * subs + xs]) << 1;
            let mut sig = [false; 16];
            let mut count = 0;
            let start = if i == last_sub {
                sig[last_pos] = true;
                count = 1;
                last_pos as isize - 1
            } else {
                15
            };
            if csbf[ys * subs + xs] {
                for p in (0..=start).rev() {
                    let p = p as usize;
                    let (xp, yp) = (usize::from(pos_scan[p].0), usize::from(pos_scan[p].1));
                    if p == 0 && infer_dc {
                        sig[0] = true;
                        count += 1;
                        continue;
                    }
                    let (xc, yc) = ((xs << 2) + xp, (ys << 2) + yp);
                    let sig_ctx = if log2 == 2 {
                        const MAP: [usize; 16] = [0, 1, 4, 5, 2, 3, 4, 5, 6, 6, 8, 8, 7, 7, 8, 8];
                        MAP[(yc << 2) + xc]
                    } else if xc + yc == 0 {
                        0
                    } else {
                        let mut s = match prev_csbf {
                            0 => match xp + yp {
                                0 => 2,
                                1 | 2 => 1,
                                _ => 0,
                            },
                            1 => match yp {
                                0 => 2,
                                1 => 1,
                                _ => 0,
                            },
                            2 => match xp {
                                0 => 2,
                                1 => 1,
                                _ => 0,
                            },
                            _ => 2,
                        };
                        if luma {
                            if (xs, ys) != (0, 0) {
                                s += 3;
                            }
                            s + if log2 == 3 { if scan_idx == 0 { 9 } else { 15 } } else { 21 }
                        } else {
                            s + if log2 == 3 { 9 } else { 12 }
                        }
                    };
                    let inc = if luma { sig_ctx } else { 27 + sig_ctx };
                    if self.bin(ctx::SIG_COEFF + inc) == 1 {
                        sig[p] = true;
                        count += 1;
                        infer_dc = false;
                    }
                }
            }
            if count == 0 {
                continue;
            }
            // levels
            let mut ctx_set = if i == 0 || !luma { 0 } else { 2 };
            if greater1_ctx == 0 {
                ctx_set += 1;
            }
            greater1_ctx = 1;
            let mut greater1 = [false; 16];
            let mut first_g1: Option<usize> = None;
            let mut flagged = 0;
            let (mut first_sig, mut last_sig) = (16usize, None);
            for p in (0..16).rev() {
                if !sig[p] {
                    continue;
                }
                if last_sig.is_none() {
                    last_sig = Some(p);
                }
                first_sig = p;
                if flagged < 8 {
                    let inc = ctx_set * 4 + greater1_ctx.min(3) as usize + if luma { 0 } else { 16 };
                    greater1[p] = self.bin(ctx::GREATER1 + inc) == 1;
                    flagged += 1;
                    if greater1[p] {
                        greater1_ctx = 0;
                        if first_g1.is_none() {
                            first_g1 = Some(p);
                        }
                    } else if greater1_ctx > 0 && greater1_ctx < 3 {
                        greater1_ctx += 1;
                    }
                }
            }
            let mut greater2 = [false; 16];
            if let Some(p) = first_g1 {
                greater2[p] = self.bin(ctx::GREATER2 + ctx_set + if luma { 0 } else { 4 }) == 1;
            }
            let hidden = hide && last_sig.unwrap_or(0) - first_sig > 3;
            let mut signs = [false; 16];
            for p in (0..16).rev() {
                if sig[p] && (!hidden || p != first_sig) {
                    signs[p] = self.cabac.bypass() == 1;
                }
            }
            let mut seen = 0;
            let mut sum = 0i32;
            let (mut rice, mut prev_level) = (0u32, 0i32);
            let mut first_remaining = true;
            for p in (0..16).rev() {
                if !sig[p] {
                    continue;
                }
                let base = 1 + i32::from(greater1[p]) + i32::from(greater2[p]);
                let threshold = if seen < 8 { if Some(p) == first_g1 { 3 } else { 2 } } else { 1 };
                let mut level = base;
                if base == threshold {
                    if !first_remaining {
                        rice = (rice + u32::from(prev_level > 3 * (1 << rice))).min(4);
                    }
                    first_remaining = false;
                    level += self.remaining(rice)?;
                    prev_level = level;
                }
                seen += 1;
                sum += level;
                let mut v = if signs[p] { -level } else { level };
                if hidden && p == first_sig && sum % 2 == 1 {
                    v = -v;
                }
                let (xc, yc) = ((xs << 2) + usize::from(pos_scan[p].0), (ys << 2) + usize::from(pos_scan[p].1));
                self.coeffs[yc * n + xc] = v;
            }
        }
        self.dequantize(c, log2, skip, depth);
        Ok(())
    }

    /// coeff_abs_level_remaining: a Rice prefix, Exp-Golomb past three.
    fn remaining(&mut self, rice: u32) -> Result<i32> {
        let mut prefix = 0;
        while self.cabac.bypass() == 1 {
            prefix += 1;
            if prefix > 32 {
                return Err(DecodeError::Invalid("coefficient level"));
            }
        }
        Ok(if prefix <= 3 {
            ((prefix << rice) + self.cabac.bypass_bits(rice)) as i32
        } else {
            let bits = prefix - 3 + rice;
            if bits > 24 {
                return Err(DecodeError::Invalid("coefficient level"));
            }
            ((((1 << (prefix - 3)) + 2) << rice) + self.cabac.bypass_bits(bits)) as i32
        })
    }

    fn dequantize(&mut self, c: usize, log2: u32, skip: bool, depth: u8) {
        let n = 1usize << log2;
        let block = &mut self.coeffs[..n * n];
        if self.bypass {
            return;
        }
        let qp_bd = 6 * (i32::from(depth) - 8);
        let qp = if c == 0 {
            self.qp_y + qp_bd
        } else {
            let offset = if c == 1 {
                self.f.pps.cb_qp_offset + self.h.cb_qp_offset
            } else {
                self.f.pps.cr_qp_offset + self.h.cr_qp_offset
            };
            chroma_qp((self.qp_y + offset).clamp(-qp_bd, 57)) + qp_bd
        };
        let bd_shift = i32::from(depth) + log2 as i32 - 5;
        let scale = i64::from(LEVEL_SCALE[(qp % 6) as usize]) << (qp / 6);
        let m = match &self.f.factors {
            Some(f) if !(skip && n > 4) => Some(&f[log2 as usize - 2][c]),
            _ => None,
        };
        for (i, v) in block.iter_mut().enumerate() {
            if *v != 0 {
                let mi = m.map_or(16, |m| i64::from(m[i]));
                let d = (i64::from(*v) * mi * scale + (1 << (bd_shift - 1))) >> bd_shift;
                *v = d.clamp(-32768, 32767) as i32;
            }
        }
        if skip {
            transform_skip(block, depth);
        } else {
            // the DST is for intra luma 4x4 blocks, and every block here is intra
            self.f.tf.inverse(block, n, c == 0 && n == 4, depth);
        }
    }
}

struct Cu {
    nxn: bool,
    max_depth: u32,
    chroma_mode: u8,
}
