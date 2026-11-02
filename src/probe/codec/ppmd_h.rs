//! PPMd variant H, as RAR 2.9 uses it for its PPM blocks: Dmitry Shkarin's model with its own memory allocator,
//! and Dmitry Subbotin's carryless range coder. Written from unrar's port of the public-domain original. The
//! model's choices depend on when its memory runs out, soo the allocator is the original's, over one byte heap
//! with 12-byte units and 32-bit offsets for pointers; 0 is null.

use super::rarbits::Bits;

const UNIT: u32 = 12;
const N_INDEXES: usize = 38;
const MAX_O: usize = 64;
const MAX_FREQ: u32 = 124;
const PERIOD_BITS: u32 = 7;
const TOT_BITS: u32 = 14;
const INTERVAL: u32 = 1 << 7;
const BIN_SCALE: u32 = 1 << TOT_BITS;
const TOP: u32 = 1 << 24;
const BOT: u32 = 1 << 15;
/// The model's memory is 1 to 256 MiB, as the stream asks
const MAX_MB: u32 = 256;
const EXP_ESCAPE: [u8; 16] = [25, 14, 9, 7, 5, 5, 4, 4, 4, 3, 3, 3, 2, 2, 2, 2];
const INIT_BIN_ESC: [u32; 8] = [0x3CDD, 0x1F3F, 0x59BF, 0x48F3, 0x64A1, 0x5ABC, 0x6632, 0x6051];

/// A secondary escape estimate.
#[derive(Clone, Copy, Default)]
struct See2 {
    summ: u16,
    shift: u8,
    count: u8,
}

impl See2 {
    fn init(&mut self, v: u32) {
        self.shift = (PERIOD_BITS - 4) as u8;
        self.summ = (v << self.shift) as u16;
        self.count = 4;
    }

    fn mean(&mut self) -> u32 {
        let r = (self.summ as i16) >> self.shift;
        self.summ = self.summ.wrapping_sub(r as u16);
        (i32::from(r) + i32::from(r == 0)) as u32
    }

    fn update(&mut self) {
        if u32::from(self.shift) < PERIOD_BITS {
            self.count = self.count.wrapping_sub(1);
            if self.count == 0 {
                self.summ = self.summ.wrapping_add(self.summ);
                self.count = (3u32 << self.shift) as u8;
                self.shift += 1;
            }
        }
    }
}

#[derive(Clone, Copy)]
struct State {
    sym: u8,
    freq: u8,
    succ: u32,
}

pub struct Ppm {
    heap: Vec<u8>,
    /// the model's memory in bytes; 0 before the first start
    size: u32,
    indx2units: [u8; N_INDEXES],
    units2indx: [u8; 128],
    glue_count: u8,
    free_list: [u32; N_INDEXES],
    heap_start: u32,
    heap_end: u32,
    lo_unit: u32,
    hi_unit: u32,
    units_start: u32,
    fake_units_start: u32,
    p_text: u32,

    see2: [[See2; 16]; 25],
    min_ctx: u32,
    max_ctx: u32,
    found: u32,
    num_masked: u32,
    init_esc: u32,
    order_fall: i32,
    max_order: i32,
    run_length: i32,
    init_rl: i32,
    char_mask: [u8; 256],
    ns2indx: [u8; 256],
    ns2bsindx: [u8; 256],
    hb2flag: [u8; 256],
    esc_count: u8,
    prev_success: u8,
    hi_bits_flag: u8,
    bin_summ: [[u16; 64]; 128],
    /// a step that needed memory found none, even after a restart
    broken: bool,

    low: u32,
    code: u32,
    range: u32,
    lo_count: u32,
    hi_count: u32,
    scale: u32,
    /// tests: counts from this generator instead of the stream, soo any model run is a valid one
    #[cfg(test)]
    fake: Option<u32>,
    /// tests: how often the model started over, and the allocator glued its free blocks
    #[cfg(test)]
    seen: (u32, u32),
}

fn get_byte(bits: &mut Bits<'_>) -> u32 {
    let b = bits.data.get(bits.addr).copied().unwrap_or(0);
    bits.addr += 1;
    u32::from(b)
}

impl Default for Ppm {
    fn default() -> Ppm {
        Ppm {
            heap: Vec::new(),
            size: 0,
            indx2units: [0; N_INDEXES],
            units2indx: [0; 128],
            glue_count: 0,
            free_list: [0; N_INDEXES],
            heap_start: UNIT,
            heap_end: 0,
            lo_unit: 0,
            hi_unit: 0,
            units_start: 0,
            fake_units_start: 0,
            p_text: 0,
            see2: [[See2::default(); 16]; 25],
            min_ctx: 0,
            max_ctx: 0,
            found: 0,
            num_masked: 0,
            init_esc: 0,
            order_fall: 0,
            max_order: 0,
            run_length: 0,
            init_rl: 0,
            char_mask: [0; 256],
            ns2indx: [0; 256],
            ns2bsindx: [0; 256],
            hb2flag: [0; 256],
            esc_count: 0,
            prev_success: 0,
            hi_bits_flag: 0,
            bin_summ: [[0; 64]; 128],
            broken: false,
            low: 0,
            code: 0,
            range: 0,
            lo_count: 0,
            hi_count: 0,
            scale: 0,
            #[cfg(test)]
            fake: None,
            #[cfg(test)]
            seen: (0, 0),
        }
    }
}

impl Ppm {
    // ---- the heap: out of range reads give 0 and writes go nowhere, which only damaged data reaches

    fn b(&self, at: u32) -> u32 {
        u32::from(self.heap.get(at as usize).copied().unwrap_or(0))
    }

    fn set_b(&mut self, at: u32, v: u32) {
        if let Some(x) = self.heap.get_mut(at as usize) {
            *x = v as u8;
        }
    }

    fn w(&self, at: u32) -> u32 {
        self.b(at) | (self.b(at.wrapping_add(1)) << 8)
    }

    fn set_w(&mut self, at: u32, v: u32) {
        self.set_b(at, v);
        self.set_b(at.wrapping_add(1), v >> 8);
    }

    fn d(&self, at: u32) -> u32 {
        self.w(at) | (self.w(at.wrapping_add(2)) << 16)
    }

    fn set_d(&mut self, at: u32, v: u32) {
        self.set_w(at, v);
        self.set_w(at.wrapping_add(2), v >> 16);
    }

    fn copy(&mut self, to: u32, from: u32, n: u32) {
        for i in 0..n {
            let v = self.b(from.wrapping_add(i));
            self.set_b(to.wrapping_add(i), v);
        }
    }

    // a context: NumStats, then SummFreq and Stats or one State, then Suffix
    fn ns(&self, c: u32) -> u32 {
        self.w(c)
    }
    fn set_ns(&mut self, c: u32, v: u32) {
        self.set_w(c, v);
    }
    fn summ(&self, c: u32) -> u32 {
        self.w(c + 2)
    }
    fn set_summ(&mut self, c: u32, v: u32) {
        self.set_w(c + 2, v);
    }
    fn stats(&self, c: u32) -> u32 {
        self.d(c + 4)
    }
    fn set_stats(&mut self, c: u32, v: u32) {
        self.set_d(c + 4, v);
    }
    fn suffix(&self, c: u32) -> u32 {
        self.d(c + 8)
    }
    fn set_suffix(&mut self, c: u32, v: u32) {
        self.set_d(c + 8, v);
    }

    // a state: Symbol, Freq, Successor; 6 bytes
    fn sym(&self, s: u32) -> u32 {
        self.b(s)
    }
    fn freq(&self, s: u32) -> u32 {
        self.b(s + 1)
    }
    fn set_freq(&mut self, s: u32, v: u32) {
        self.set_b(s + 1, v);
    }
    fn succ(&self, s: u32) -> u32 {
        self.d(s + 2)
    }
    fn set_succ(&mut self, s: u32, v: u32) {
        self.set_d(s + 2, v);
    }
    fn state(&self, s: u32) -> State {
        State { sym: self.b(s) as u8, freq: self.b(s + 1) as u8, succ: self.d(s + 2) }
    }
    fn set_state(&mut self, s: u32, v: State) {
        self.set_b(s, u32::from(v.sym));
        self.set_b(s + 1, u32::from(v.freq));
        self.set_d(s + 2, v.succ);
    }
    fn swap(&mut self, a: u32, b: u32) {
        let (x, y) = (self.state(a), self.state(b));
        self.set_state(a, y);
        self.set_state(b, x);
    }

    // ---- the allocator

    fn u2b(nu: u32) -> u32 {
        UNIT * nu
    }

    fn insert_node(&mut self, p: u32, indx: usize) {
        let head = self.free_list[indx];
        self.set_d(p, head);
        self.free_list[indx] = p;
    }

    fn remove_node(&mut self, indx: usize) -> u32 {
        let r = self.free_list[indx];
        self.free_list[indx] = self.d(r);
        r
    }

    fn split_block(&mut self, pv: u32, old: usize, new: usize) {
        let mut udiff = u32::from(self.indx2units[old]) - u32::from(self.indx2units[new]);
        let mut p = pv + Self::u2b(u32::from(self.indx2units[new]));
        let mut i = usize::from(self.units2indx[udiff as usize - 1]);
        if u32::from(self.indx2units[i]) != udiff {
            i -= 1;
            self.insert_node(p, i);
            let units = u32::from(self.indx2units[i]);
            p += Self::u2b(units);
            udiff -= units;
        }
        let i = usize::from(self.units2indx[udiff as usize - 1]);
        self.insert_node(p, i);
    }

    fn start_sub_allocator(&mut self, mb: u32) {
        let t = mb << 20;
        if self.size == t {
            return;
        }
        let alloc = t / UNIT * UNIT + 2 * UNIT;
        self.heap = vec![0; (self.heap_start + alloc) as usize];
        self.heap_end = self.heap_start + alloc - UNIT;
        self.size = t;
    }

    fn init_sub_allocator(&mut self) {
        self.free_list = [0; N_INDEXES];
        self.p_text = self.heap_start;
        let size2 = UNIT * (self.size / 8 / UNIT * 7);
        let size1 = self.size - size2;
        let real1 = size1 / UNIT * UNIT + UNIT;
        self.lo_unit = self.heap_start + real1;
        self.units_start = self.lo_unit;
        self.fake_units_start = self.heap_start + size1;
        self.hi_unit = self.lo_unit + size2;
        let mut k = 1u32;
        let mut i = 0;
        for (count, step) in [(4, 1), (4, 2), (4, 3), (26, 4)] {
            if i > 0 {
                k += 1;
            }
            for _ in 0..count {
                self.indx2units[i] = k as u8;
                i += 1;
                k += step;
            }
        }
        let mut i = 0usize;
        for k in 0..128usize {
            if usize::from(self.indx2units[i]) < k + 1 {
                i += 1;
            }
            self.units2indx[k] = i as u8;
        }
        self.glue_count = 0;
    }

    // a free block while gluing: Stamp, NU, next, prev; the list's head sits at offset 0, below the heap
    fn blk_next(&self, p: u32) -> u32 {
        self.d(p + 4)
    }
    fn blk_prev(&self, p: u32) -> u32 {
        self.d(p + 8)
    }
    fn blk_insert_at(&mut self, this: u32, p: u32) {
        let next = self.blk_next(p);
        self.set_d(this + 8, p);
        self.set_d(this + 4, next);
        self.set_d(next + 8, this);
        self.set_d(p + 4, this);
    }
    fn blk_remove(&mut self, this: u32) {
        let (prev, next) = (self.blk_prev(this), self.blk_next(this));
        self.set_d(prev + 4, next);
        self.set_d(next + 8, prev);
    }

    fn glue_free_blocks(&mut self) {
        #[cfg(test)]
        {
            self.seen.1 += 1;
        }
        const S0: u32 = 0;
        if self.lo_unit != self.hi_unit {
            self.set_b(self.lo_unit, 0);
        }
        self.set_d(S0 + 4, S0);
        self.set_d(S0 + 8, S0);
        for i in 0..N_INDEXES {
            while self.free_list[i] != 0 {
                let p = self.remove_node(i);
                self.blk_insert_at(p, S0);
                self.set_w(p, 0xffff);
                self.set_w(p + 2, u32::from(self.indx2units[i]));
            }
        }
        let mut p = self.blk_next(S0);
        while p != S0 {
            loop {
                let p1 = p.wrapping_add(Self::u2b(self.w(p + 2)));
                if self.w(p1) == 0xffff && self.w(p + 2) + self.w(p1 + 2) < 0x10000 {
                    self.blk_remove(p1);
                    let nu = self.w(p + 2) + self.w(p1 + 2);
                    self.set_w(p + 2, nu);
                } else {
                    break;
                }
            }
            p = self.blk_next(p);
        }
        loop {
            let mut p = self.blk_next(S0);
            if p == S0 {
                break;
            }
            self.blk_remove(p);
            let mut sz = self.w(p + 2);
            if sz == 0 {
                continue;
            }
            while sz > 128 {
                self.insert_node(p, N_INDEXES - 1);
                sz -= 128;
                p += Self::u2b(128);
            }
            let mut i = usize::from(self.units2indx[sz as usize - 1]);
            if u32::from(self.indx2units[i]) != sz {
                i -= 1;
                let k = sz - u32::from(self.indx2units[i]);
                self.insert_node(p + Self::u2b(sz - k), k as usize - 1);
            }
            self.insert_node(p, i);
        }
    }

    fn alloc_units_rare(&mut self, indx: usize) -> u32 {
        if self.glue_count == 0 {
            self.glue_count = 255;
            self.glue_free_blocks();
            if self.free_list[indx] != 0 {
                return self.remove_node(indx);
            }
        }
        let mut i = indx;
        loop {
            i += 1;
            if i == N_INDEXES {
                self.glue_count = self.glue_count.wrapping_sub(1);
                let bytes = Self::u2b(u32::from(self.indx2units[indx]));
                let j = UNIT * u32::from(self.indx2units[indx]);
                if i64::from(self.fake_units_start) - i64::from(self.p_text) > i64::from(j) {
                    self.fake_units_start -= j;
                    self.units_start -= bytes;
                    return self.units_start;
                }
                return 0;
            }
            if self.free_list[i] != 0 {
                break;
            }
        }
        let r = self.remove_node(i);
        self.split_block(r, i, indx);
        r
    }

    fn alloc_units(&mut self, nu: u32) -> u32 {
        let indx = usize::from(self.units2indx[nu as usize - 1]);
        if self.free_list[indx] != 0 {
            return self.remove_node(indx);
        }
        let r = self.lo_unit;
        let bytes = Self::u2b(u32::from(self.indx2units[indx]));
        if self.lo_unit + bytes <= self.hi_unit {
            self.lo_unit += bytes;
            return r;
        }
        self.alloc_units_rare(indx)
    }

    fn alloc_context(&mut self) -> u32 {
        if self.hi_unit != self.lo_unit {
            self.hi_unit -= UNIT;
            return self.hi_unit;
        }
        if self.free_list[0] != 0 {
            return self.remove_node(0);
        }
        self.alloc_units_rare(0)
    }

    fn expand_units(&mut self, old: u32, old_nu: u32) -> u32 {
        let i0 = usize::from(self.units2indx[old_nu as usize - 1]);
        let i1 = usize::from(self.units2indx[old_nu as usize]);
        if i0 == i1 {
            return old;
        }
        let p = self.alloc_units(old_nu + 1);
        if p != 0 {
            self.copy(p, old, Self::u2b(old_nu));
            self.insert_node(old, i0);
        }
        p
    }

    fn shrink_units(&mut self, old: u32, old_nu: u32, new_nu: u32) -> u32 {
        let i0 = usize::from(self.units2indx[old_nu as usize - 1]);
        let i1 = usize::from(self.units2indx[new_nu as usize - 1]);
        if i0 == i1 {
            return old;
        }
        if self.free_list[i1] != 0 {
            let p = self.remove_node(i1);
            self.copy(p, old, Self::u2b(new_nu));
            self.insert_node(old, i0);
            p
        } else {
            self.split_block(old, i0, i1);
            old
        }
    }

    fn free_units(&mut self, p: u32, nu: u32) {
        let i = usize::from(self.units2indx[nu as usize - 1]);
        self.insert_node(p, i);
    }

    // ---- the range coder

    fn coder_init(&mut self, bits: &mut Bits<'_>) {
        self.low = 0;
        self.code = 0;
        self.range = u32::MAX;
        for _ in 0..4 {
            self.code = (self.code << 8) | get_byte(bits);
        }
    }

    /// None: the range has gone to nothing, which only damaged data does
    fn normalize(&mut self, bits: &mut Bits<'_>) -> Option<()> {
        if self.fake_count(1).is_some() {
            return Some(());
        }
        if self.range == 0 {
            return None;
        }
        loop {
            if (self.low ^ self.low.wrapping_add(self.range)) >= TOP {
                if self.range >= BOT {
                    break;
                }
                self.range = self.low.wrapping_neg() & (BOT - 1);
            }
            self.code = (self.code << 8) | get_byte(bits);
            self.range <<= 8;
            self.low <<= 8;
        }
        Some(())
    }

    #[cfg(test)]
    fn fake_count(&mut self, below: u32) -> Option<u32> {
        let seed = self.fake.as_mut()?;
        *seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
        Some((*seed >> 8) % below.max(1))
    }

    #[cfg(not(test))]
    fn fake_count(&mut self, _: u32) -> Option<u32> {
        None
    }

    /// tests: a real stream never escapes the root context, which holds every byte; soo neither do fake counts
    fn fake_root_count(&mut self, ctx: u32, stats: u32, ns: u32) -> Option<u32> {
        if self.ns(ctx) != 256 || self.fake_count(1).is_none() {
            return None;
        }
        let total = (0..ns).map(|i| self.freq(stats + 6 * i)).sum::<u32>();
        self.fake_count(total.max(1))
    }

    fn current_count(&mut self) -> Option<u32> {
        if let Some(c) = self.fake_count(self.scale) {
            return Some(c);
        }
        self.range = self.range.checked_div(self.scale)?;
        self.code.wrapping_sub(self.low).checked_div(self.range)
    }

    fn current_shift_count(&mut self, shift: u32) -> Option<u32> {
        if let Some(c) = self.fake_count(1 << shift) {
            return Some(c);
        }
        self.range >>= shift;
        self.code.wrapping_sub(self.low).checked_div(self.range)
    }

    fn coder_decode(&mut self) {
        if self.fake_count(1).is_some() {
            return;
        }
        self.low = self.low.wrapping_add(self.range.wrapping_mul(self.lo_count));
        self.range = self.range.wrapping_mul(self.hi_count.wrapping_sub(self.lo_count));
    }

    // ---- the model

    fn restart_model_rare(&mut self) {
        #[cfg(test)]
        {
            self.seen.0 += 1;
        }
        self.char_mask = [0; 256];
        self.init_sub_allocator();
        self.init_rl = -self.max_order.min(12) - 1;
        let ctx = self.alloc_context();
        self.min_ctx = ctx;
        self.max_ctx = ctx;
        if ctx == 0 {
            self.broken = true;
            return;
        }
        self.set_suffix(ctx, 0);
        self.order_fall = self.max_order;
        self.set_ns(ctx, 256);
        self.set_summ(ctx, 257);
        let stats = self.alloc_units(256 / 2);
        self.found = stats;
        self.set_stats(ctx, stats);
        if stats == 0 {
            self.broken = true;
            return;
        }
        self.run_length = self.init_rl;
        self.prev_success = 0;
        for i in 0..256u32 {
            self.set_state(stats + 6 * i, State { sym: i as u8, freq: 1, succ: 0 });
        }
        for (i, row) in self.bin_summ.iter_mut().enumerate() {
            for (j, b) in row.iter_mut().enumerate() {
                *b = (BIN_SCALE - INIT_BIN_ESC[j % 8] / (i as u32 + 2)) as u16;
            }
        }
        for i in 0..25 {
            for k in 0..16 {
                self.see2[i][k].init(5 * i as u32 + 10);
            }
        }
    }

    fn start_model_rare(&mut self, max_order: i32) {
        self.esc_count = 1;
        self.max_order = max_order;
        self.restart_model_rare();
        self.ns2bsindx[0] = 0;
        self.ns2bsindx[1] = 2;
        self.ns2bsindx[2..11].fill(4);
        self.ns2bsindx[11..].fill(6);
        for i in 0..3 {
            self.ns2indx[i] = i as u8;
        }
        let (mut m, mut k, mut step) = (3u32, 1u32, 1u32);
        for i in 3..256 {
            self.ns2indx[i] = m as u8;
            k -= 1;
            if k == 0 {
                step += 1;
                k = step;
                m += 1;
            }
        }
        self.hb2flag[..0x40].fill(0);
        self.hb2flag[0x40..].fill(8);
    }

    fn rescale(&mut self, ctx: u32) {
        let old_ns = self.ns(ctx);
        let stats = self.stats(ctx);
        let mut p = self.found;
        while p > stats {
            self.swap(p, p - 6);
            p -= 6;
        }
        p = stats;
        self.set_freq(stats, self.freq(stats) + 4);
        let mut summ = self.summ(ctx) + 4;
        let mut esc = summ as i32 - self.freq(p) as i32;
        let adder = u32::from(self.order_fall != 0);
        let f = (self.freq(p) + adder) >> 1;
        self.set_freq(p, f);
        summ = f;
        let mut i = old_ns as i32 - 1;
        while i > 0 {
            p += 6;
            esc -= self.freq(p) as i32;
            let f = (self.freq(p) + adder) >> 1;
            self.set_freq(p, f);
            summ += f;
            if self.freq(p) > self.freq(p - 6) {
                let tmp = self.state(p);
                let mut p1 = p;
                loop {
                    let prev = self.state(p1 - 6);
                    self.set_state(p1, prev);
                    p1 -= 6;
                    if p1 == stats || u32::from(tmp.freq) <= self.freq(p1 - 6) {
                        break;
                    }
                }
                self.set_state(p1, tmp);
            }
            i -= 1;
        }
        let mut ns = old_ns;
        if self.freq(p) == 0 {
            let mut i = 0;
            loop {
                i += 1;
                p -= 6;
                if self.freq(p) != 0 || p <= stats {
                    break;
                }
            }
            esc += i;
            ns = ns.saturating_sub(i as u32);
            self.set_ns(ctx, ns);
            if ns == 1 {
                let mut tmp = self.state(stats);
                loop {
                    tmp.freq -= tmp.freq >> 1;
                    esc >>= 1;
                    if esc <= 1 {
                        break;
                    }
                }
                self.free_units(stats, (old_ns + 1) >> 1);
                self.found = ctx + 2;
                self.set_state(ctx + 2, tmp);
                return;
            }
        }
        esc -= esc >> 1;
        summ = summ.wrapping_add(esc as u32);
        self.set_summ(ctx, summ);
        let (n0, n1) = ((old_ns + 1) >> 1, (ns + 1) >> 1);
        if n0 != n1 && n1 > 0 {
            let s = self.shrink_units(stats, n0, n1);
            self.set_stats(ctx, s);
        }
        self.found = self.stats(ctx);
    }

    fn create_child(&mut self, ctx: u32, ps: u32, first: State) -> u32 {
        let pc = self.alloc_context();
        if pc != 0 {
            self.set_ns(pc, 1);
            self.set_state(pc + 2, first);
            self.set_suffix(pc, ctx);
            self.set_succ(ps, pc);
        }
        pc
    }

    /// The state for `sym` among a context's stats, or None when it isn't there.
    fn find(&self, ctx: u32, sym: u32) -> Option<u32> {
        let stats = self.stats(ctx);
        (0..self.ns(ctx)).map(|i| stats + 6 * i).find(|p| self.sym(*p) == sym)
    }

    fn create_successors(&mut self, skip: bool, p1: u32) -> u32 {
        let mut pc = self.min_ctx;
        let up_branch = self.succ(self.found);
        let fsym = self.sym(self.found);
        let mut ps: Vec<u32> = Vec::with_capacity(MAX_O);
        let mut walk = true;
        if !skip {
            ps.push(self.found);
            if self.suffix(pc) == 0 {
                walk = false;
            }
        }
        if walk {
            let mut entry = p1 != 0;
            loop {
                let p;
                pc = self.suffix(pc);
                if entry {
                    p = p1;
                    entry = false;
                } else if self.ns(pc) != 1 {
                    let Some(found) = self.find(pc, fsym) else { return 0 };
                    p = found;
                } else {
                    p = pc + 2;
                }
                if self.succ(p) != up_branch {
                    pc = self.succ(p);
                    break;
                }
                if ps.len() >= MAX_O {
                    return 0;
                }
                ps.push(p);
                if self.suffix(pc) == 0 {
                    break;
                }
            }
        }
        if ps.is_empty() {
            return pc;
        }
        let up_sym = self.b(up_branch);
        let up_succ = up_branch.wrapping_add(1);
        let up_freq = if self.ns(pc) != 1 {
            if pc <= self.p_text {
                return 0;
            }
            let Some(p) = self.find(pc, up_sym) else { return 0 };
            let cf = self.freq(p).wrapping_sub(1);
            let s0 = self.summ(pc).wrapping_sub(self.ns(pc)).wrapping_sub(cf);
            if cf.wrapping_mul(2) <= s0 {
                1 + u32::from(cf.wrapping_mul(5) > s0)
            } else {
                let top = cf.wrapping_mul(2).wrapping_add(s0.wrapping_mul(3)).wrapping_sub(1);
                let Some(q) = top.checked_div(s0.wrapping_mul(2)) else { return 0 };
                1u32.wrapping_add(q)
            }
        } else {
            self.freq(pc + 2)
        };
        let up = State { sym: up_sym as u8, freq: up_freq as u8, succ: up_succ };
        while let Some(p) = ps.pop() {
            pc = self.create_child(pc, p, up);
            if pc == 0 {
                return 0;
            }
        }
        pc
    }

    fn update_model(&mut self) {
        if !self.update_model_or_restart() {
            self.restart_model_rare();
            self.esc_count = 0;
        }
    }

    /// false: the memory ran out, and the model starts over
    fn update_model_or_restart(&mut self) -> bool {
        let mut fs = self.state(self.found);
        let mut p = 0u32;
        let pc = self.suffix(self.min_ctx);
        if u32::from(fs.freq) < MAX_FREQ / 4 && pc != 0 {
            if self.ns(pc) != 1 {
                p = self.stats(pc);
                if self.sym(p) != u32::from(fs.sym) {
                    let Some(found) = self.find(pc, u32::from(fs.sym)) else { return false };
                    p = found;
                    if self.freq(p) >= self.freq(p - 6) {
                        self.swap(p, p - 6);
                        p -= 6;
                    }
                }
                if self.freq(p) < MAX_FREQ - 9 {
                    self.set_freq(p, self.freq(p) + 2);
                    self.set_summ(pc, self.summ(pc) + 2);
                }
            } else {
                p = pc + 2;
                self.set_freq(p, self.freq(p) + u32::from(self.freq(p) < 32));
            }
        }
        if self.order_fall == 0 {
            let s = self.create_successors(true, p);
            self.min_ctx = s;
            self.max_ctx = s;
            self.set_succ(self.found, s);
            return s != 0;
        }
        self.set_b(self.p_text, u32::from(fs.sym));
        self.p_text += 1;
        let mut successor = self.p_text;
        if self.p_text >= self.fake_units_start {
            return false;
        }
        if fs.succ != 0 {
            if fs.succ <= self.p_text {
                fs.succ = self.create_successors(false, p);
                if fs.succ == 0 {
                    return false;
                }
            }
            self.order_fall -= 1;
            if self.order_fall == 0 {
                successor = fs.succ;
                if self.max_ctx != self.min_ctx {
                    self.p_text -= 1;
                }
            }
        } else {
            self.set_succ(self.found, successor);
            fs.succ = self.min_ctx;
        }
        let ns = self.ns(self.min_ctx);
        let s0 = self.summ(self.min_ctx).wrapping_sub(ns).wrapping_sub(u32::from(fs.freq).wrapping_sub(1));
        let mut pc = self.max_ctx;
        let mut guard = 0;
        while pc != self.min_ctx {
            guard += 1;
            if pc == 0 || guard > MAX_O + 1 {
                return false;
            }
            let ns1 = self.ns(pc);
            if ns1 != 1 {
                if ns1 & 1 == 0 {
                    let s = self.expand_units(self.stats(pc), ns1 >> 1);
                    self.set_stats(pc, s);
                    if s == 0 {
                        return false;
                    }
                }
                let add = u32::from(2 * ns1 < ns) + 2 * u32::from((4 * ns1 <= ns) & (self.summ(pc) <= 8 * ns1));
                self.set_summ(pc, self.summ(pc) + add);
            } else {
                let p = self.alloc_units(1);
                if p == 0 {
                    return false;
                }
                let one = self.state(pc + 2);
                self.set_state(p, one);
                self.set_stats(pc, p);
                if self.freq(p) < MAX_FREQ / 4 - 1 {
                    self.set_freq(p, self.freq(p) * 2);
                } else {
                    self.set_freq(p, MAX_FREQ - 4);
                }
                self.set_summ(pc, self.freq(p) + self.init_esc + u32::from(ns > 3));
            }
            let mut cf = 2 * u32::from(fs.freq) * (self.summ(pc) + 6);
            let sf = s0.wrapping_add(self.summ(pc));
            if cf < 6u32.wrapping_mul(sf) {
                cf = 1 + u32::from(cf > sf) + u32::from(cf >= 4u32.wrapping_mul(sf));
                self.set_summ(pc, self.summ(pc) + 3);
            } else {
                cf = 4
                    + u32::from(cf >= 9u32.wrapping_mul(sf))
                    + u32::from(cf >= 12u32.wrapping_mul(sf))
                    + u32::from(cf >= 15u32.wrapping_mul(sf));
                self.set_summ(pc, self.summ(pc) + cf);
            }
            let p = self.stats(pc) + 6 * ns1;
            self.set_state(p, State { sym: fs.sym, freq: cf as u8, succ: successor });
            self.set_ns(pc, ns1 + 1);
            pc = self.suffix(pc);
        }
        self.max_ctx = fs.succ;
        self.min_ctx = fs.succ;
        true
    }

    fn decode_bin_symbol(&mut self, ctx: u32) -> Option<()> {
        let rs = ctx + 2;
        self.hi_bits_flag = self.hb2flag[self.sym(self.found) as usize];
        let i = (self.freq(rs) as usize).checked_sub(1)?.min(127);
        let j = usize::from(self.prev_success)
            + usize::from(self.ns2bsindx[(self.ns(self.suffix(ctx)) as usize).wrapping_sub(1) & 0xff])
            + usize::from(self.hi_bits_flag)
            + 2 * usize::from(self.hb2flag[self.sym(rs) as usize])
            + ((self.run_length >> 26) & 0x20) as usize;
        let bs = u32::from(*self.bin_summ.get(i)?.get(j)?);
        let mean = |bs: u32| (bs + (1 << (PERIOD_BITS - 2))) >> PERIOD_BITS;
        if self.current_shift_count(TOT_BITS)? < bs {
            self.found = rs;
            self.set_freq(rs, self.freq(rs) + u32::from(self.freq(rs) < 128));
            self.lo_count = 0;
            self.hi_count = bs;
            self.bin_summ[i][j] = (bs + INTERVAL - mean(bs)) as u16;
            self.prev_success = 1;
            self.run_length += 1;
        } else {
            self.lo_count = bs;
            let bs = bs.wrapping_sub(mean(bs)) as u16;
            self.bin_summ[i][j] = bs;
            self.hi_count = BIN_SCALE;
            self.init_esc = u32::from(EXP_ESCAPE[usize::from(bs >> 10).min(15)]);
            self.num_masked = 1;
            self.char_mask[self.sym(rs) as usize] = self.esc_count;
            self.prev_success = 0;
            self.found = 0;
        }
        Some(())
    }

    fn update1(&mut self, ctx: u32, mut p: u32) {
        self.found = p;
        self.set_freq(p, self.freq(p) + 4);
        self.set_summ(ctx, self.summ(ctx) + 4);
        if self.freq(p) > self.freq(p - 6) {
            self.swap(p, p - 6);
            p -= 6;
            self.found = p;
            if self.freq(p) > MAX_FREQ {
                self.rescale(ctx);
            }
        }
    }

    fn decode_symbol1(&mut self, ctx: u32) -> Option<()> {
        self.scale = self.summ(ctx);
        let mut p = self.stats(ctx);
        let count = match self.fake_root_count(ctx, self.stats(ctx), self.ns(ctx)) {
            Some(c) => c,
            None => self.current_count()?,
        };
        if count >= self.scale {
            return None;
        }
        let mut hi = self.freq(p);
        if count < hi {
            self.hi_count = hi;
            self.prev_success = u8::from(2 * hi > self.scale);
            self.run_length += i32::from(self.prev_success);
            hi += 4;
            self.found = p;
            self.set_freq(p, hi);
            self.set_summ(ctx, self.summ(ctx) + 4);
            if hi > MAX_FREQ {
                self.rescale(ctx);
            }
            self.lo_count = 0;
            return Some(());
        }
        if self.found == 0 {
            return None;
        }
        self.prev_success = 0;
        let ns = self.ns(ctx);
        let mut i = ns.checked_sub(1).filter(|i| *i > 0)?;
        loop {
            p += 6;
            hi += self.freq(p);
            if hi > count {
                break;
            }
            i -= 1;
            if i == 0 {
                self.hi_bits_flag = self.hb2flag[self.sym(self.found) as usize];
                self.lo_count = hi;
                self.char_mask[self.sym(p) as usize] = self.esc_count;
                self.num_masked = ns;
                self.found = 0;
                for _ in 0..ns - 1 {
                    p -= 6;
                    self.char_mask[self.sym(p) as usize] = self.esc_count;
                }
                self.hi_count = self.scale;
                return Some(());
            }
        }
        self.hi_count = hi;
        self.lo_count = hi - self.freq(p);
        self.update1(ctx, p);
        Some(())
    }

    fn update2(&mut self, ctx: u32, p: u32) {
        self.found = p;
        self.set_freq(p, self.freq(p) + 4);
        self.set_summ(ctx, self.summ(ctx) + 4);
        if self.freq(p) > MAX_FREQ {
            self.rescale(ctx);
        }
        self.esc_count = self.esc_count.wrapping_add(1);
        self.run_length = self.init_rl;
    }

    fn decode_symbol2(&mut self, ctx: u32) -> Option<()> {
        let ns = self.ns(ctx);
        if self.num_masked >= ns {
            return None;
        }
        let diff = ns - self.num_masked;
        // the escape's estimate
        let see = if ns != 256 {
            let i = usize::from(self.ns2indx[diff as usize - 1]);
            let j = usize::from((diff as i32) < self.ns(self.suffix(ctx)) as i32 - ns as i32)
                + 2 * usize::from(self.summ(ctx) < 11 * ns)
                + 4 * usize::from(self.num_masked > diff)
                + usize::from(self.hi_bits_flag);
            self.scale = self.see2.get_mut(i)?.get_mut(j)?.mean();
            Some((i, j))
        } else {
            self.scale = 1;
            None
        };
        let stats = self.stats(ctx);
        let end = stats + 6 * ns;
        let mut ps = Vec::with_capacity(diff as usize);
        let mut p = stats;
        let mut hi = 0u32;
        while ps.len() < diff as usize {
            while self.char_mask[self.sym(p) as usize] == self.esc_count {
                p += 6;
                if p >= end {
                    return None;
                }
            }
            hi += self.freq(p);
            ps.push(p);
            p += 6;
            if ps.len() < diff as usize && p >= end {
                return None;
            }
        }
        self.scale = self.scale.wrapping_add(hi);
        let count = match self.fake_root_count(ctx, 0, 0).map(|_| self.fake_count(hi)) {
            Some(c) => c?,
            None => self.current_count()?,
        };
        if count >= self.scale {
            return None;
        }
        if count < hi {
            let mut hi = 0;
            let mut k = 0;
            let p = loop {
                let p = *ps.get(k)?;
                hi += self.freq(p);
                if hi > count {
                    break p;
                }
                k += 1;
            };
            self.hi_count = hi;
            self.lo_count = hi - self.freq(p);
            if let Some((i, j)) = see {
                self.see2[i][j].update();
            }
            self.update2(ctx, p);
        } else {
            self.lo_count = hi;
            self.hi_count = self.scale;
            for p in ps {
                self.char_mask[self.sym(p) as usize] = self.esc_count;
            }
            if let Some((i, j)) = see {
                let s = &mut self.see2[i][j];
                s.summ = s.summ.wrapping_add(self.scale as u16);
            }
            self.num_masked = ns;
        }
        Some(())
    }

    fn clear_mask(&mut self) {
        self.esc_count = 1;
        self.char_mask = [0; 256];
    }

    fn ctx_ok(&self, c: u32) -> bool {
        c > self.p_text && c <= self.heap_end
    }

    /// A PPM block's header: its order and memory when it starts a new model, and maybe a new escape byte. False:
    /// it can't be decoded.
    pub fn init(&mut self, bits: &mut Bits<'_>, esc: &mut u8) -> bool {
        let max_order = get_byte(bits);
        let reset = max_order & 0x20 != 0;
        let mut max_mb = 0;
        if reset {
            max_mb = get_byte(bits);
        } else if self.size == 0 {
            return false;
        }
        if max_order & 0x40 != 0 {
            *esc = get_byte(bits) as u8;
        }
        self.coder_init(bits);
        if reset {
            let mut order = (max_order & 0x1f) as i32 + 1;
            if order > 16 {
                order = 16 + (order - 16) * 3;
            }
            if order == 1 || max_mb + 1 > MAX_MB {
                self.size = 0;
                return false;
            }
            self.broken = false;
            self.start_sub_allocator(max_mb + 1);
            self.start_model_rare(order);
        }
        self.min_ctx != 0 && !self.broken
    }

    /// The next byte, or None for damaged data.
    pub fn decode_char(&mut self, bits: &mut Bits<'_>) -> Option<u8> {
        if self.broken || !self.ctx_ok(self.min_ctx) {
            return None;
        }
        let ctx = self.min_ctx;
        if self.ns(ctx) != 1 {
            if !self.ctx_ok(self.stats(ctx)) {
                return None;
            }
            self.decode_symbol1(ctx)?;
        } else {
            self.decode_bin_symbol(ctx)?;
        }
        self.coder_decode();
        while self.found == 0 {
            self.normalize(bits)?;
            loop {
                self.order_fall += 1;
                self.min_ctx = self.suffix(self.min_ctx);
                if !self.ctx_ok(self.min_ctx) {
                    return None;
                }
                if self.ns(self.min_ctx) != self.num_masked {
                    break;
                }
            }
            self.decode_symbol2(self.min_ctx)?;
            self.coder_decode();
        }
        let sym = self.sym(self.found) as u8;
        if self.order_fall == 0 && self.succ(self.found) > self.p_text {
            self.min_ctx = self.succ(self.found);
            self.max_ctx = self.min_ctx;
        } else {
            self.update_model();
            if self.esc_count == 0 {
                self.clear_mask();
            }
        }
        self.normalize(bits)?;
        Some(sym)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_model_that_runs_out_of_memory_starts_over_without_panicking() {
        // a model of 1 MiB fed random choices makes random text, which fills its memory again and again: its
        // allocator glues free blocks, takes units from the text area, and restarts
        for order in [3u8, 15] {
            let data = [0x20 | order, 0, 0, 0, 0, 0];
            let mut bits = Bits { data: &data, addr: 0, bit: 0 };
            let mut ppm = Ppm::default();
            let mut esc = 2;
            assert!(ppm.init(&mut bits, &mut esc));
            ppm.fake = Some(u32::from(order));
            let mut n = 0;
            while n < 300_000 && ppm.decode_char(&mut bits).is_some() {
                n += 1;
            }
            assert!(n == 300_000, "order {order}: {n} bytes");
            assert!(ppm.seen.0 > 2 && ppm.seen.1 > 0, "order {order}: restarts and glues {:?}", ppm.seen);
        }
    }
}
