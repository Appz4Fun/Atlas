//! PPMd variant I (revision 1), as zip's method 98 uses it: Dmitry Shkarin's model with its own memory allocator
//! and Dmitry Subbotin's carryless range coder. Written from 7-Zip's public-domain Ppmd8. The model's choices
//! depend on when its memory runs out, so the allocator is the original's, over one byte heap with 12-byte units
//! and 32-bit offsets for pointers; 0 is null. Both ways of going on when memory runs out are here: starting over,
//! and cutting the model back.

use super::super::lzma::End;
use super::heap::Heap;

const UNIT: u32 = 12;
const N_INDEXES: usize = 38;
const MAX_FREQ: u32 = 124;
const MAX_ORDER: u32 = 16;
const PERIOD_BITS: u32 = 7;
const BIN_SCALE: u32 = 1 << 14;
const TOP: u32 = 1 << 24;
const BOT: u32 = 1 << 15;
const EMPTY: u32 = 0xFFFF_FFFF;
const EXP_ESCAPE: [u8; 16] = [25, 14, 9, 7, 5, 5, 4, 4, 4, 3, 3, 3, 2, 2, 2, 2];
const INIT_BIN_ESC: [u32; 8] = [0x3CDD, 0x1F3F, 0x59BF, 0x48F3, 0x64A1, 0x5ABC, 0x6632, 0x6051];
const RESTART: u32 = 0;
const CUT_OFF: u32 = 1;

/// A secondary escape estimate.
#[derive(Clone, Copy, Default)]
struct See {
    summ: u16,
    shift: u8,
    count: u8,
}

impl See {
    fn update(&mut self) {
        if u32::from(self.shift) < PERIOD_BITS {
            self.count = self.count.wrapping_sub(1);
            if self.count == 0 {
                self.summ = self.summ.wrapping_mul(2);
                self.count = (3u32 << self.shift) as u8;
                self.shift += 1;
            }
        }
    }
}

/// Which estimate an escape used: one of the table, or the one for a context with every symbol.
#[derive(Clone, Copy)]
enum SeeAt {
    Table(usize),
    Dummy,
}

/// A symbol's statistics, read out of the heap.
#[derive(Clone, Copy)]
struct State {
    sym: u32,
    freq: u32,
    succ: u32,
}

struct Model {
    heap: Heap,
    size: u32,
    align: u32,
    glue_count: u32,
    lo_unit: u32,
    hi_unit: u32,
    text: u32,
    units_start: u32,
    indx2units: [u8; N_INDEXES],
    units2indx: [u8; 128],
    free_list: [u32; N_INDEXES],
    stamps: [u32; N_INDEXES],
    ns2bsindx: [u8; 256],
    ns2indx: [u8; 260],
    see: [See; 24 * 32],
    dummy: See,
    bin_summ: [[u16; 64]; 25],
    min_ctx: u32,
    max_ctx: u32,
    found: u32,
    order_fall: u32,
    init_esc: u32,
    prev_success: u32,
    max_order: u32,
    run_length: i32,
    init_rl: i32,
    restore: u32,
    /// damaged input sent the model somewhere it can't be: what's decoded so far stands, nothing more
    broken: bool,
    /// tests: how often the model started over, and was cut back
    #[cfg(test)]
    seen: (u32, u32),
}

struct Coder<'a> {
    data: &'a [u8],
    at: usize,
    low: u32,
    range: u32,
    code: u32,
}

impl Coder<'_> {
    fn byte(&mut self) -> u32 {
        let b = self.data.get(self.at).copied().unwrap_or(0);
        self.at += 1;
        u32::from(b)
    }

    /// Whether a byte past the input's end has been read: what's decoded after that can't be trusted.
    fn overran(&self) -> bool {
        self.at > self.data.len()
    }

    fn threshold(&mut self, total: u32) -> Option<u32> {
        self.range = self.range.checked_div(total)?;
        self.code.checked_div(self.range)
    }

    fn decode(&mut self, start: u32, size: u32) -> Option<()> {
        let start = start.wrapping_mul(self.range);
        self.low = self.low.wrapping_add(start);
        self.code = self.code.wrapping_sub(start);
        self.range = self.range.wrapping_mul(size);
        let mut steps = 0;
        loop {
            if (self.low ^ self.low.wrapping_add(self.range)) >= TOP {
                if self.range >= BOT {
                    return Some(());
                }
                self.range = 0u32.wrapping_sub(self.low) & (BOT - 1);
            }
            // a range that never grows is damage, not a stream
            steps += 1;
            if steps > 8 {
                return None;
            }
            self.code = (self.code << 8) | self.byte();
            self.range <<= 8;
            self.low <<= 8;
        }
    }
}

impl Model {
    fn new(size: u32, max_order: u32, restore: u32) -> Model {
        let align = 4 - (size & 3);
        let mut m = Model {
            heap: Heap::new((align + size) as usize),
            size,
            align,
            glue_count: 0,
            lo_unit: 0,
            hi_unit: 0,
            text: 0,
            units_start: 0,
            indx2units: [0; N_INDEXES],
            units2indx: [0; 128],
            free_list: [0; N_INDEXES],
            stamps: [0; N_INDEXES],
            ns2bsindx: [0; 256],
            ns2indx: [0; 260],
            see: [See::default(); 24 * 32],
            dummy: See { summ: 0, shift: PERIOD_BITS as u8, count: 64 },
            bin_summ: [[0; 64]; 25],
            min_ctx: 0,
            max_ctx: 0,
            found: 0,
            order_fall: 0,
            init_esc: 0,
            prev_success: 0,
            max_order,
            run_length: 0,
            init_rl: 0,
            restore,
            broken: false,
            #[cfg(test)]
            seen: (0, 0),
        };
        let mut k = 0usize;
        for i in 0..N_INDEXES {
            let step = if i >= 12 { 4 } else { (i >> 2) + 1 };
            for _ in 0..step {
                m.units2indx[k] = i as u8;
                k += 1;
            }
            m.indx2units[i] = k as u8;
        }
        m.ns2bsindx[0] = 0;
        m.ns2bsindx[1] = 2;
        m.ns2bsindx[2..11].fill(4);
        m.ns2bsindx[11..].fill(6);
        for i in 0..5 {
            m.ns2indx[i] = i as u8;
        }
        let (mut step, mut val) = (1u32, 5u32);
        for i in 5..260 {
            m.ns2indx[i] = val as u8;
            step -= 1;
            if step == 0 {
                val += 1;
                step = val - 4;
            }
        }
        m.restart();
        m
    }

    // ---- the heap: out of range reads give 0 and writes go nowhere, which only damaged data reaches

    fn b(&self, at: u32) -> u32 {
        u32::from(self.heap.get(at as usize))
    }

    fn set_b(&mut self, at: u32, v: u32) {
        self.heap.set(at as usize, v as u8);
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

    fn copy(&mut self, to: u32, from: u32, bytes: u32) {
        let (to, from, n) = (to as usize, from as usize, bytes as usize);
        let size = self.heap.size();
        if from.saturating_add(n) > size || to.saturating_add(n) > size {
            return;
        }
        // units move down to lower addresses or into free ones, but copy the safe way round either way
        if to <= from {
            for i in 0..n {
                self.heap.set(to + i, self.heap.get(from + i));
            }
        } else {
            for i in (0..n).rev() {
                self.heap.set(to + i, self.heap.get(from + i));
            }
        }
    }

    // ---- contexts: NumStats, Flags, SummFreq (or the one state from here), Stats, Suffix

    fn num_stats(&self, c: u32) -> u32 {
        self.b(c)
    }
    fn set_num_stats(&mut self, c: u32, v: u32) {
        self.set_b(c, v);
    }
    fn flags(&self, c: u32) -> u32 {
        self.b(c + 1)
    }
    fn set_flags(&mut self, c: u32, v: u32) {
        self.set_b(c + 1, v);
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

    // ---- states: Symbol, Freq, Successor (two 16-bit halves), six bytes

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
        State { sym: self.sym(s), freq: self.freq(s), succ: self.succ(s) }
    }
    fn set_state(&mut self, s: u32, st: State) {
        self.set_b(s, st.sym);
        self.set_b(s + 1, st.freq);
        self.set_d(s + 2, st.succ);
    }
    fn swap(&mut self, a: u32, b: u32) {
        let (x, y) = (self.state(a), self.state(b));
        self.set_state(a, y);
        self.set_state(b, x);
    }

    // ---- the allocator's free blocks: Stamp, Next, NU

    fn i2u(&self, i: usize) -> u32 {
        u32::from(self.indx2units[i.min(N_INDEXES - 1)])
    }
    fn u2i(&self, nu: u32) -> usize {
        usize::from(self.units2indx[(nu.clamp(1, 128) - 1) as usize])
    }

    fn insert_node(&mut self, node: u32, indx: usize) {
        let indx = indx.min(N_INDEXES - 1);
        self.set_d(node, EMPTY);
        self.set_d(node + 4, self.free_list[indx]);
        self.set_d(node + 8, self.i2u(indx));
        self.free_list[indx] = node;
        self.stamps[indx] = self.stamps[indx].wrapping_add(1);
    }

    fn remove_node(&mut self, indx: usize) -> u32 {
        let node = self.free_list[indx];
        self.free_list[indx] = self.d(node + 4);
        self.stamps[indx] = self.stamps[indx].wrapping_sub(1);
        node
    }

    fn split_block(&mut self, ptr: u32, old: usize, new: usize) {
        let nu = self.i2u(old) - self.i2u(new);
        let ptr = ptr + self.i2u(new) * UNIT;
        let mut i = self.u2i(nu);
        if self.i2u(i) != nu {
            i -= 1;
            let k = self.i2u(i);
            self.insert_node(ptr + k * UNIT, (nu - k - 1) as usize);
        }
        self.insert_node(ptr, i);
    }

    fn glue_free_blocks(&mut self) {
        // the list being built: its head, or the node whose Next is its end
        let mut head = 0u32;
        let mut prev: Option<u32> = None;
        self.glue_count = 1 << 13;
        self.stamps = [0; N_INDEXES];
        if self.lo_unit != self.hi_unit {
            self.set_d(self.lo_unit, 0);
        }
        let mut steps = 0usize;
        let max_steps = self.heap.size() / UNIT as usize + N_INDEXES;
        for i in 0..N_INDEXES {
            let mut next = self.free_list[i];
            self.free_list[i] = 0;
            while next != 0 && steps <= max_steps {
                steps += 1;
                let node = next;
                if self.d(node + 8) != 0 {
                    match prev {
                        None => head = next,
                        Some(p) => self.set_d(p + 4, next),
                    }
                    prev = Some(node);
                    loop {
                        let nu = self.d(node + 8);
                        let node2 = node.wrapping_add(nu.wrapping_mul(UNIT));
                        if nu == 0 || self.d(node2) != EMPTY {
                            break;
                        }
                        self.set_d(node + 8, nu.wrapping_add(self.d(node2 + 8)));
                        self.set_d(node2 + 8, 0);
                    }
                }
                next = self.d(node + 4);
            }
        }
        match prev {
            None => head = 0,
            Some(p) => self.set_d(p + 4, 0),
        }
        while head != 0 && steps <= 2 * max_steps {
            steps += 1;
            let mut node = head;
            head = self.d(node + 4);
            let mut nu = self.d(node + 8);
            if nu == 0 {
                continue;
            }
            while nu > 128 {
                self.insert_node(node, N_INDEXES - 1);
                nu -= 128;
                node += 128 * UNIT;
            }
            let mut i = self.u2i(nu);
            if self.i2u(i) != nu {
                i -= 1;
                let k = self.i2u(i);
                self.insert_node(node + k * UNIT, (nu - k - 1) as usize);
            }
            self.insert_node(node, i);
        }
    }

    fn alloc_units_rare(&mut self, indx: usize) -> u32 {
        if self.glue_count == 0 {
            self.glue_free_blocks();
            if self.free_list[indx] != 0 {
                return self.remove_node(indx);
            }
        }
        let mut i = indx;
        loop {
            i += 1;
            if i == N_INDEXES {
                let bytes = self.i2u(indx) * UNIT;
                self.glue_count = self.glue_count.wrapping_sub(1);
                return if self.units_start.wrapping_sub(self.text) > bytes {
                    self.units_start -= bytes;
                    self.units_start
                } else {
                    0
                };
            }
            if self.free_list[i] != 0 {
                break;
            }
        }
        let r = self.remove_node(i);
        self.split_block(r, i, indx);
        r
    }

    fn alloc_units(&mut self, indx: usize) -> u32 {
        if self.free_list[indx] != 0 {
            return self.remove_node(indx);
        }
        let bytes = self.i2u(indx) * UNIT;
        if bytes <= self.hi_unit.wrapping_sub(self.lo_unit) {
            let r = self.lo_unit;
            self.lo_unit += bytes;
            return r;
        }
        self.alloc_units_rare(indx)
    }

    fn shrink_units(&mut self, old: u32, old_nu: u32, new_nu: u32) -> u32 {
        let (i0, i1) = (self.u2i(old_nu), self.u2i(new_nu));
        if i0 == i1 {
            return old;
        }
        if self.free_list[i1] != 0 {
            let ptr = self.remove_node(i1);
            self.copy(ptr, old, new_nu * UNIT);
            self.insert_node(old, i0);
            return ptr;
        }
        self.split_block(old, i0, i1);
        old
    }

    fn free_units(&mut self, ptr: u32, nu: u32) {
        let i = self.u2i(nu);
        self.insert_node(ptr, i);
    }

    fn special_free_unit(&mut self, ptr: u32) {
        if ptr != self.units_start {
            self.insert_node(ptr, 0);
        } else {
            self.units_start += UNIT;
        }
    }

    fn move_units_up(&mut self, old: u32, nu: u32) -> u32 {
        let indx = self.u2i(nu);
        if old > self.units_start.wrapping_add(16 * 1024) || old > self.free_list[indx] {
            return old;
        }
        let ptr = self.remove_node(indx);
        self.copy(ptr, old, nu * UNIT);
        if old != self.units_start {
            self.insert_node(old, indx);
        } else {
            self.units_start += self.i2u(indx) * UNIT;
        }
        ptr
    }

    fn expand_text_area(&mut self) {
        let mut count = [0u32; N_INDEXES];
        if self.lo_unit != self.hi_unit {
            self.set_d(self.lo_unit, 0);
        }
        let mut node = self.units_start;
        while self.d(node) == EMPTY {
            let nu = self.d(node + 8);
            if nu == 0 {
                break;
            }
            self.set_d(node, 0);
            let i = self.u2i(nu);
            count[i] += 1;
            node = node.wrapping_add(nu.wrapping_mul(UNIT));
        }
        self.units_start = node;
        let max_steps = self.heap.size() / UNIT as usize;
        // `i` picks the free list as well as its count
        #[allow(clippy::needless_range_loop)]
        for i in 0..N_INDEXES {
            // where the link to the next node lives: the list's head, or a node's Next
            let mut link: Option<u32> = None;
            let mut steps = 0usize;
            while count[i] != 0 && steps <= max_steps {
                steps += 1;
                let read = |m: &Model, l: Option<u32>| match l {
                    None => m.free_list[i],
                    Some(n) => m.d(n + 4),
                };
                let mut node = read(self, link);
                while self.d(node) == 0 && steps <= max_steps {
                    steps += 1;
                    let next = self.d(node + 4);
                    match link {
                        None => self.free_list[i] = next,
                        Some(n) => self.set_d(n + 4, next),
                    }
                    node = next;
                    self.stamps[i] = self.stamps[i].wrapping_sub(1);
                    count[i] -= 1;
                    if count[i] == 0 {
                        break;
                    }
                }
                link = Some(node);
            }
        }
    }

    fn restart(&mut self) {
        #[cfg(test)]
        {
            self.seen.0 += 1;
        }
        self.free_list = [0; N_INDEXES];
        self.stamps = [0; N_INDEXES];
        self.text = self.align;
        self.hi_unit = self.text + self.size;
        self.lo_unit = self.hi_unit - self.size / 8 / UNIT * 7 * UNIT;
        self.units_start = self.lo_unit;
        self.glue_count = 0;
        self.order_fall = self.max_order;
        self.init_rl = -(self.max_order.min(12) as i32) - 1;
        self.run_length = self.init_rl;
        self.prev_success = 0;
        self.hi_unit -= UNIT;
        let c = self.hi_unit;
        self.min_ctx = c;
        self.max_ctx = c;
        self.set_suffix(c, 0);
        self.set_num_stats(c, 255);
        self.set_flags(c, 0);
        self.set_summ(c, 256 + 1);
        self.found = self.lo_unit;
        self.set_stats(c, self.lo_unit);
        for i in 0..256 {
            self.set_state(self.lo_unit + i * 6, State { sym: i, freq: 1, succ: 0 });
        }
        self.lo_unit += 128 * UNIT;
        let mut i = 0usize;
        for m in 0..25 {
            while usize::from(self.ns2indx[i]) == m {
                i += 1;
            }
            for (k, esc) in INIT_BIN_ESC.iter().enumerate() {
                let val = (BIN_SCALE - esc / (i as u32 + 1)) as u16;
                for r in (0..64).step_by(8) {
                    self.bin_summ[m][k + r] = val;
                }
            }
        }
        let mut i = 0usize;
        for m in 0..24 {
            while usize::from(self.ns2indx[i + 3]) == m + 3 {
                i += 1;
            }
            for k in 0..32 {
                let shift = (PERIOD_BITS - 4) as u8;
                self.see[m * 32 + k] = See { summ: ((2 * i as u32 + 5) << shift) as u16, shift, count: 7 };
            }
        }
    }

    fn refresh(&mut self, ctx: u32, old_nu: u32, scale: u32) {
        let i = self.num_stats(ctx);
        let mut s = self.shrink_units(self.stats(ctx), old_nu, (i + 2) >> 1);
        self.set_stats(ctx, s);
        let mut flags = (self.flags(ctx) & (0x10 + 0x04 * scale)) + 0x08 * u32::from(self.sym(s) >= 0x40);
        let mut esc = self.summ(ctx).wrapping_sub(self.freq(s));
        let f = (self.freq(s) + scale) >> scale;
        self.set_freq(s, f);
        let mut sum = f;
        for _ in 0..i {
            s += 6;
            esc = esc.wrapping_sub(self.freq(s));
            let f = (self.freq(s) + scale) >> scale;
            self.set_freq(s, f);
            sum += f;
            flags |= 0x08 * u32::from(self.sym(s) >= 0x40);
        }
        self.set_summ(ctx, sum.wrapping_add(esc.wrapping_add(scale) >> scale));
        self.set_flags(ctx, flags);
    }

    fn cut_off(&mut self, ctx: u32, order: u32) -> u32 {
        if self.num_stats(ctx) == 0 {
            let s = ctx + 2;
            if self.succ(s) >= self.units_start {
                let v = if order < self.max_order { self.cut_off(self.succ(s), order + 1) } else { 0 };
                self.set_succ(s, v);
                if self.succ(s) != 0 || order <= 9 {
                    return ctx;
                }
            }
            self.special_free_unit(ctx);
            return 0;
        }
        let tmp = (self.num_stats(ctx) + 2) >> 1;
        let moved = self.move_units_up(self.stats(ctx), tmp);
        self.set_stats(ctx, moved);
        let mut i = self.num_stats(ctx) as i32;
        let stats = self.stats(ctx);
        for j in (0..=self.num_stats(ctx)).rev() {
            let s = stats + j * 6;
            if self.succ(s) < self.units_start {
                let s2 = stats.wrapping_add((i as u32).wrapping_mul(6));
                i -= 1;
                self.set_succ(s, 0);
                self.swap(s, s2);
            } else if order < self.max_order {
                let v = self.cut_off(self.succ(s), order + 1);
                self.set_succ(s, v);
            } else {
                self.set_succ(s, 0);
            }
        }
        if i != self.num_stats(ctx) as i32 && order != 0 {
            self.set_num_stats(ctx, i as u32);
            let s = self.stats(ctx);
            if i < 0 {
                self.free_units(s, tmp);
                self.special_free_unit(ctx);
                return 0;
            }
            if i == 0 {
                let flags = (self.flags(ctx) & 0x10) + 0x08 * u32::from(self.sym(s) >= 0x40);
                self.set_flags(ctx, flags);
                let st = self.state(s);
                self.set_state(ctx + 2, st);
                self.free_units(s, tmp);
                let f = (self.freq(ctx + 2) + 11) >> 3;
                self.set_freq(ctx + 2, f);
            } else {
                let scale = u32::from(self.summ(ctx) > 16 * i as u32);
                self.refresh(ctx, tmp, scale);
            }
        }
        ctx
    }

    fn used_memory(&self) -> u32 {
        let v: u32 = (0..N_INDEXES).map(|i| self.stamps[i].wrapping_mul(self.i2u(i))).fold(0, u32::wrapping_add);
        self.size
            .wrapping_sub(self.hi_unit.wrapping_sub(self.lo_unit))
            .wrapping_sub(self.units_start.wrapping_sub(self.text))
            .wrapping_sub(v.wrapping_mul(UNIT))
    }

    fn restore_model(&mut self, c1: u32) {
        self.text = self.align;
        let mut c = self.max_ctx;
        let mut steps = 0;
        while c != c1 && c != 0 && steps < 300 {
            steps += 1;
            let ns = self.num_stats(c).wrapping_sub(1) & 0xff;
            self.set_num_stats(c, ns);
            if ns == 0 {
                let s = self.stats(c);
                let flags = (self.flags(c) & 0x10) + 0x08 * u32::from(self.sym(s) >= 0x40);
                self.set_flags(c, flags);
                let st = self.state(s);
                self.set_state(c + 2, st);
                self.special_free_unit(s);
                let f = (self.freq(c + 2) + 11) >> 3;
                self.set_freq(c + 2, f);
            } else {
                self.refresh(c, (ns + 3) >> 1, 0);
            }
            c = self.suffix(c);
        }
        while c != self.min_ctx && c != 0 && steps < 600 {
            steps += 1;
            if self.num_stats(c) == 0 {
                let f = self.freq(c + 2);
                self.set_freq(c + 2, f - (f >> 1));
            } else {
                let sf = (self.summ(c) + 4) & 0xffff;
                self.set_summ(c, sf);
                if sf > 128 + 4 * self.num_stats(c) {
                    self.refresh(c, (self.num_stats(c) + 2) >> 1, 1);
                }
            }
            c = self.suffix(c);
        }
        if self.restore == RESTART || self.used_memory() < (self.size >> 1) {
            self.restart();
            return;
        }
        #[cfg(test)]
        {
            self.seen.1 += 1;
        }
        let mut steps = 0;
        while self.suffix(self.max_ctx) != 0 && steps < 300 {
            steps += 1;
            self.max_ctx = self.suffix(self.max_ctx);
        }
        let mut rounds = 0;
        loop {
            self.cut_off(self.max_ctx, 0);
            self.expand_text_area();
            rounds += 1;
            if self.used_memory() <= 3 * (self.size >> 2) {
                break;
            }
            // cutting back that gets nowhere is damage
            if rounds > 64 {
                self.broken = true;
                break;
            }
        }
        self.glue_count = 0;
        self.order_fall = self.max_order;
    }

    fn create_successors(&mut self, skip: bool, mut s1: u32, mut c: u32) -> u32 {
        let up_branch = self.succ(self.found);
        let mut ps = [0u32; MAX_ORDER as usize + 1];
        let mut num = 0usize;
        if !skip {
            ps[num] = self.found;
            num += 1;
        }
        let found_sym = self.sym(self.found);
        while self.suffix(c) != 0 {
            c = self.suffix(c);
            let s;
            if s1 != 0 {
                s = s1;
                s1 = 0;
            } else if self.num_stats(c) != 0 {
                let Some(at) = self.find(c, found_sym) else {
                    self.broken = true;
                    return 0;
                };
                s = at;
                if self.freq(s) < MAX_FREQ - 9 {
                    self.set_freq(s, self.freq(s) + 1);
                    self.set_summ(c, self.summ(c) + 1);
                }
            } else {
                s = c + 2;
                let bump = u32::from(self.num_stats(self.suffix(c)) == 0) & u32::from(self.freq(s) < 24);
                self.set_freq(s, self.freq(s) + bump);
            }
            let successor = self.succ(s);
            if successor != up_branch {
                c = successor;
                if num == 0 {
                    return c;
                }
                break;
            }
            if num == ps.len() {
                self.broken = true;
                return 0;
            }
            ps[num] = s;
            num += 1;
        }
        if num == 0 {
            return c;
        }
        let up_sym = self.b(up_branch);
        let up_succ = up_branch.wrapping_add(1);
        let flags = 0x10 * u32::from(found_sym >= 0x40) + 0x08 * u32::from(up_sym >= 0x40);
        let up_freq = if self.num_stats(c) == 0 {
            self.freq(c + 2)
        } else {
            let Some(s) = self.find(c, up_sym) else {
                self.broken = true;
                return 0;
            };
            let cf = self.freq(s).wrapping_sub(1);
            let s0 = self.summ(c).wrapping_sub(self.num_stats(c)).wrapping_sub(cf);
            let add = if 2 * cf <= s0 {
                u32::from(5 * cf > s0)
            } else {
                (cf + 2 * s0).wrapping_sub(3).checked_div(s0).unwrap_or(0)
            };
            (1 + add) & 0xff
        };
        loop {
            let c1 = if self.hi_unit != self.lo_unit {
                self.hi_unit -= UNIT;
                self.hi_unit
            } else if self.free_list[0] != 0 {
                self.remove_node(0)
            } else {
                let r = self.alloc_units_rare(0);
                if r == 0 {
                    return 0;
                }
                r
            };
            self.set_num_stats(c1, 0);
            self.set_flags(c1, flags);
            self.set_state(c1 + 2, State { sym: up_sym, freq: up_freq, succ: up_succ });
            self.set_suffix(c1, c);
            num -= 1;
            self.set_succ(ps[num], c1);
            c = c1;
            if num == 0 {
                return c;
            }
        }
    }

    /// The state for `sym` in a context with more than one: the original walks to it unchecked.
    fn find(&self, c: u32, sym: u32) -> Option<u32> {
        let stats = self.stats(c);
        (0..=self.num_stats(c)).map(|j| stats + j * 6).find(|&s| self.sym(s) == sym)
    }

    fn reduce_order(&mut self, mut s1: u32, mut c: u32) -> u32 {
        let c1 = c;
        let up_branch = self.text;
        let found_sym = self.sym(self.found);
        self.set_succ(self.found, up_branch);
        self.order_fall = self.order_fall.wrapping_add(1);
        let mut s;
        let mut steps = 0;
        loop {
            steps += 1;
            if steps > 300 {
                self.broken = true;
                return 0;
            }
            if s1 != 0 {
                c = self.suffix(c);
                s = s1;
                s1 = 0;
            } else {
                if self.suffix(c) == 0 {
                    return c;
                }
                c = self.suffix(c);
                if self.num_stats(c) != 0 {
                    let Some(at) = self.find(c, found_sym) else {
                        self.broken = true;
                        return 0;
                    };
                    s = at;
                    if self.freq(s) < MAX_FREQ - 9 {
                        self.set_freq(s, self.freq(s) + 2);
                        self.set_summ(c, self.summ(c) + 2);
                    }
                } else {
                    s = c + 2;
                    self.set_freq(s, self.freq(s) + u32::from(self.freq(s) < 32));
                }
            }
            if self.succ(s) != 0 {
                break;
            }
            self.set_succ(s, up_branch);
            self.order_fall = self.order_fall.wrapping_add(1);
        }
        if self.succ(s) <= up_branch {
            let s2 = self.found;
            self.found = s;
            let successor = self.create_successors(false, 0, c);
            self.set_succ(s, successor);
            self.found = s2;
        }
        if self.order_fall == 1 && c1 == self.max_ctx {
            self.set_succ(self.found, self.succ(s));
            self.text = self.text.wrapping_sub(1);
        }
        self.succ(s)
    }

    fn update_model(&mut self) {
        let found = self.state(self.found);
        let (f_freq, f_sym) = (found.freq, found.sym);
        let mut f_succ = found.succ;
        let mut s = 0u32;
        if f_freq < MAX_FREQ / 4 && self.suffix(self.min_ctx) != 0 {
            let c = self.suffix(self.min_ctx);
            if self.num_stats(c) == 0 {
                s = c + 2;
                if self.freq(s) < 32 {
                    self.set_freq(s, self.freq(s) + 1);
                }
            } else {
                let Some(at) = self.find(c, f_sym) else {
                    self.broken = true;
                    return;
                };
                s = at;
                if s != self.stats(c) && self.freq(s) >= self.freq(s - 6) {
                    self.swap(s, s - 6);
                    s -= 6;
                }
                if self.freq(s) < MAX_FREQ - 9 {
                    self.set_freq(s, self.freq(s) + 2);
                    self.set_summ(c, self.summ(c) + 2);
                }
            }
        }
        let mut c = self.max_ctx;
        if self.order_fall == 0 && f_succ != 0 {
            let cs = self.create_successors(true, s, self.min_ctx);
            if cs == 0 {
                self.set_succ(self.found, 0);
                self.restore_model(c);
            } else {
                self.set_succ(self.found, cs);
                self.max_ctx = cs;
            }
            return;
        }
        self.set_b(self.text, f_sym);
        self.text += 1;
        let mut successor = self.text;
        if self.text >= self.units_start {
            self.restore_model(c);
            return;
        }
        if f_succ == 0 {
            let cs = self.reduce_order(s, self.min_ctx);
            if cs == 0 {
                self.restore_model(c);
                return;
            }
            f_succ = cs;
        } else if f_succ < self.units_start {
            let cs = self.create_successors(false, s, self.min_ctx);
            if cs == 0 {
                self.restore_model(c);
                return;
            }
            f_succ = cs;
        }
        self.order_fall = self.order_fall.wrapping_sub(1);
        if self.order_fall == 0 {
            successor = f_succ;
            self.text -= u32::from(self.max_ctx != self.min_ctx);
        }
        let ns = self.num_stats(self.min_ctx);
        let s0 = self.summ(self.min_ctx).wrapping_sub(ns).wrapping_sub(f_freq);
        let flag = 0x08 * u32::from(f_sym >= 0x40);
        let mut steps = 0;
        while c != self.min_ctx {
            steps += 1;
            if steps > 300 || c == 0 {
                self.broken = true;
                return;
            }
            let ns1 = self.num_stats(c);
            if ns1 != 0 {
                if ns1 & 1 != 0 {
                    let old_nu = (ns1 + 1) >> 1;
                    let i = self.u2i(old_nu);
                    if i != self.u2i(old_nu + 1) {
                        let ptr = self.alloc_units(i + 1);
                        if ptr == 0 {
                            self.restore_model(c);
                            return;
                        }
                        let old = self.stats(c);
                        self.copy(ptr, old, old_nu * UNIT);
                        self.insert_node(old, i);
                        self.set_stats(c, ptr);
                    }
                }
                self.set_summ(c, self.summ(c) + u32::from(3 * ns1 + 1 < ns));
            } else {
                let s2 = self.alloc_units(0);
                if s2 == 0 {
                    self.restore_model(c);
                    return;
                }
                let st = self.state(c + 2);
                self.set_state(s2, st);
                self.set_stats(c, s2);
                let f = if st.freq < MAX_FREQ / 4 - 1 { st.freq << 1 } else { MAX_FREQ - 4 };
                self.set_freq(s2, f);
                self.set_summ(c, f + self.init_esc + u32::from(ns > 2));
            }
            let summ = self.summ(c);
            let mut cf = 2 * f_freq * (summ + 6);
            let sf = s0.wrapping_add(summ);
            if cf < sf.wrapping_mul(6) {
                cf = 1 + u32::from(cf > sf) + u32::from(cf >= sf.wrapping_mul(4));
                self.set_summ(c, summ + 4);
            } else {
                cf = 4
                    + u32::from(cf > sf.wrapping_mul(9))
                    + u32::from(cf > sf.wrapping_mul(12))
                    + u32::from(cf > sf.wrapping_mul(15));
                self.set_summ(c, summ + cf);
            }
            let s2 = self.stats(c) + (ns1 + 1) * 6;
            self.set_state(s2, State { sym: f_sym, freq: cf, succ: successor });
            self.set_flags(c, self.flags(c) | flag);
            self.set_num_stats(c, ns1 + 1);
            c = self.suffix(c);
        }
        self.max_ctx = f_succ;
        self.min_ctx = f_succ;
    }

    fn rescale(&mut self) {
        let mc = self.min_ctx;
        let stats = self.stats(mc);
        let mut s = self.found;
        let tmp = self.state(s);
        while s > stats {
            let st = self.state(s - 6);
            self.set_state(s, st);
            s -= 6;
        }
        self.set_state(s, tmp);
        let mut esc = self.summ(mc).wrapping_sub(self.freq(s));
        let adder = u32::from(self.order_fall != 0);
        let f = (((self.freq(s) + 4) & 0xff) + adder) >> 1;
        self.set_freq(s, f);
        let mut sum = f;
        let mut i = self.num_stats(mc);
        for _ in 0..i {
            s += 6;
            esc = esc.wrapping_sub(self.freq(s));
            let f = (self.freq(s) + adder) >> 1;
            self.set_freq(s, f);
            sum += f;
            if f > self.freq(s - 6) {
                let mut s1 = s;
                let t = self.state(s1);
                loop {
                    let st = self.state(s1 - 6);
                    self.set_state(s1, st);
                    s1 -= 6;
                    if s1 == stats || t.freq <= self.freq(s1 - 6) {
                        break;
                    }
                }
                self.set_state(s1, t);
            }
        }
        if self.freq(s) == 0 {
            let num_stats = self.num_stats(mc);
            i = 0;
            loop {
                i += 1;
                s -= 6;
                if self.freq(s) != 0 || s <= stats {
                    break;
                }
            }
            esc = esc.wrapping_add(i);
            self.set_num_stats(mc, num_stats.wrapping_sub(i) & 0xff);
            if self.num_stats(mc) == 0 {
                let mut t = self.state(stats);
                let f = (2 * t.freq + esc).wrapping_sub(1).checked_div(esc).unwrap_or(1);
                t.freq = f.min(MAX_FREQ / 3);
                let indx = self.u2i((num_stats + 2) >> 1);
                self.insert_node(stats, indx);
                let flags = (self.flags(mc) & 0x10) + 0x08 * u32::from(t.sym >= 0x40);
                self.set_flags(mc, flags);
                self.found = mc + 2;
                self.set_state(self.found, t);
                return;
            }
            let n0 = (num_stats + 2) >> 1;
            let n1 = (self.num_stats(mc) + 2) >> 1;
            if n0 != n1 {
                let moved = self.shrink_units(stats, n0, n1);
                self.set_stats(mc, moved);
            }
            let mut s = self.stats(mc);
            let mut flags = (self.flags(mc) & !0x08) | (0x08 * u32::from(self.sym(s) >= 0x40));
            for _ in 0..self.num_stats(mc) {
                s += 6;
                flags |= 0x08 * u32::from(self.sym(s) >= 0x40);
            }
            self.set_flags(mc, flags);
        }
        self.set_summ(mc, sum.wrapping_add(esc).wrapping_sub(esc >> 1));
        self.set_flags(mc, self.flags(mc) | 0x04);
        self.found = self.stats(mc);
    }

    fn make_esc_freq(&mut self, num_masked: u32) -> (SeeAt, u32) {
        let mc = self.min_ctx;
        let ns = self.num_stats(mc);
        if ns == 0xff {
            return (SeeAt::Dummy, 1);
        }
        let row = usize::from(self.ns2indx[(ns + 2) as usize]).saturating_sub(3);
        let col = usize::from(self.summ(mc) > 11 * (ns + 1))
            + 2 * usize::from(2 * ns < self.num_stats(self.suffix(mc)) + num_masked)
            + self.flags(mc) as usize;
        let at = (row * 32 + col).min(self.see.len() - 1);
        let see = &mut self.see[at];
        let r = u32::from(see.summ >> see.shift);
        see.summ = see.summ.wrapping_sub(r as u16);
        (SeeAt::Table(at), r + u32::from(r == 0))
    }

    fn see_mut(&mut self, at: SeeAt) -> &mut See {
        match at {
            SeeAt::Table(i) => &mut self.see[i],
            SeeAt::Dummy => &mut self.dummy,
        }
    }

    fn next_context(&mut self) {
        let c = self.succ(self.found);
        if self.order_fall == 0 && c >= self.units_start {
            self.min_ctx = c;
            self.max_ctx = c;
        } else {
            self.update_model();
            self.min_ctx = self.max_ctx;
        }
    }

    fn update1(&mut self) {
        let mut s = self.found;
        self.set_freq(s, self.freq(s) + 4);
        self.set_summ(self.min_ctx, self.summ(self.min_ctx) + 4);
        if self.freq(s) > self.freq(s - 6) {
            self.swap(s, s - 6);
            s -= 6;
            self.found = s;
            if self.freq(s) > MAX_FREQ {
                self.rescale();
            }
        }
        self.next_context();
    }

    fn update1_0(&mut self) {
        self.prev_success = u32::from(2 * self.freq(self.found) >= self.summ(self.min_ctx));
        self.run_length += self.prev_success as i32;
        self.set_summ(self.min_ctx, self.summ(self.min_ctx) + 4);
        let f = (self.freq(self.found) + 4) & 0xff;
        self.set_freq(self.found, f);
        if f > MAX_FREQ {
            self.rescale();
        }
        self.next_context();
    }

    fn update_bin(&mut self) {
        let f = self.freq(self.found);
        self.set_freq(self.found, f + u32::from(f < 196));
        self.prev_success = 1;
        self.run_length += 1;
        self.next_context();
    }

    fn update2(&mut self) {
        self.set_summ(self.min_ctx, self.summ(self.min_ctx) + 4);
        let f = (self.freq(self.found) + 4) & 0xff;
        self.set_freq(self.found, f);
        if f > MAX_FREQ {
            self.rescale();
        }
        self.run_length = self.init_rl;
        self.update_model();
        self.min_ctx = self.max_ctx;
    }

    /// The next symbol; None for the end marker or damage.
    fn decode_symbol(&mut self, rc: &mut Coder<'_>) -> Option<u8> {
        let mut mask = [true; 256];
        let mc = self.min_ctx;
        if self.num_stats(mc) != 0 {
            let mut s = self.stats(mc);
            let summ = self.summ(mc);
            let count = rc.threshold(summ)?;
            let mut hi = self.freq(s);
            if count < hi {
                rc.decode(0, hi)?;
                self.found = s;
                let sym = self.sym(s) as u8;
                self.update1_0();
                return Some(sym);
            }
            self.prev_success = 0;
            for _ in 0..self.num_stats(mc) {
                s += 6;
                hi += self.freq(s);
                if hi > count {
                    rc.decode(hi - self.freq(s), self.freq(s))?;
                    self.found = s;
                    let sym = self.sym(s) as u8;
                    self.update1();
                    return Some(sym);
                }
            }
            if count >= summ {
                return None;
            }
            rc.decode(hi, summ - hi)?;
            let stats = self.stats(mc);
            for j in 0..=self.num_stats(mc) {
                mask[self.sym(stats + j * 6) as usize] = false;
            }
        } else {
            let one = mc + 2;
            let row = usize::from(self.ns2indx[(self.freq(one).max(1) - 1) as usize]);
            let col = usize::from(self.ns2bsindx[self.num_stats(self.suffix(mc)) as usize])
                + self.prev_success as usize
                + self.flags(mc) as usize
                + ((self.run_length >> 26) & 0x20) as usize;
            let (row, col) = (row.min(24), col.min(63));
            let prob = u32::from(self.bin_summ[row][col]);
            rc.range >>= 14;
            if rc.code.checked_div(rc.range)? < prob {
                rc.decode(0, prob)?;
                self.bin_summ[row][col] = (prob + 128 - ((prob + 32) >> 7)) as u16;
                self.found = one;
                let sym = self.sym(one) as u8;
                self.update_bin();
                return Some(sym);
            }
            rc.decode(prob, BIN_SCALE - prob)?;
            let prob = prob - ((prob + 32) >> 7);
            self.bin_summ[row][col] = prob as u16;
            self.init_esc = u32::from(EXP_ESCAPE[(prob >> 10) as usize & 15]);
            mask[self.sym(one) as usize] = false;
            self.prev_success = 0;
        }
        let mut ps = [0u32; 256];
        loop {
            let num_masked = self.num_stats(self.min_ctx);
            loop {
                self.order_fall = self.order_fall.wrapping_add(1);
                let suffix = self.suffix(self.min_ctx);
                if suffix == 0 {
                    return None;
                }
                self.min_ctx = suffix;
                if self.num_stats(suffix) != num_masked {
                    break;
                }
            }
            let mc = self.min_ctx;
            let num = self.num_stats(mc).checked_sub(num_masked)? as usize;
            let stats = self.stats(mc);
            let mut hi = 0u32;
            let mut n = 0usize;
            for j in 0..=self.num_stats(mc) {
                let s = stats + j * 6;
                if mask[self.sym(s) as usize] {
                    if n == num {
                        break;
                    }
                    hi += self.freq(s);
                    ps[n] = s;
                    n += 1;
                }
            }
            if n != num {
                return None;
            }
            let (see, esc) = self.make_esc_freq(num_masked);
            let total = esc + hi;
            let count = rc.threshold(total)?;
            if count < hi {
                let mut acc = 0u32;
                let mut k = 0;
                loop {
                    acc += self.freq(ps[k]);
                    if acc > count {
                        break;
                    }
                    k += 1;
                }
                let s = ps[k];
                rc.decode(acc - self.freq(s), self.freq(s))?;
                self.see_mut(see).update();
                self.found = s;
                let sym = self.sym(s) as u8;
                self.update2();
                return Some(sym);
            }
            if count >= total {
                return None;
            }
            rc.decode(hi, total - hi)?;
            let see = self.see_mut(see);
            see.summ = see.summ.wrapping_add(total as u16);
            for &s in &ps[..n] {
                mask[self.sym(s) as usize] = false;
            }
        }
    }
}

/// Zip's method 98: a two-byte header (order, memory, how to go on when memory runs out), then the range-coded
/// stream. Decodes from the front to `size` bytes, or `limit`.
pub fn zip(data: &[u8], size: Option<u64>, limit: usize) -> (Vec<u8>, End) {
    let (out, end, _) = run(data, size, limit);
    (out, end)
}

/// Like [`zip`], and also how often the model started over and was cut back (only counted in tests).
fn run(data: &[u8], size: Option<u64>, limit: usize) -> (Vec<u8>, End, (u32, u32)) {
    let mut out = Vec::new();
    let Some(head) = data.get(..2).map(|h| u32::from(u16::from_le_bytes([h[0], h[1]]))) else {
        return (out, End::Cut, (0, 0));
    };
    let order = (head & 0xf) + 1;
    let mem_mb = ((head >> 4) & 0xff) + 1;
    let restore = head >> 12;
    if order < 2 || restore > CUT_OFF {
        return (out, End::Cut, (0, 0));
    }
    let target = size.and_then(|s| usize::try_from(s).ok()).unwrap_or(usize::MAX).min(limit);
    let mut rc = Coder { data: &data[2..], at: 0, low: 0, range: 0xFFFF_FFFF, code: 0 };
    for _ in 0..4 {
        rc.code = (rc.code << 8) | rc.byte();
    }
    if rc.code == 0xFFFF_FFFF || rc.overran() {
        return (out, End::Cut, (0, 0));
    }
    let mut model = Model::new(mem_mb << 20, order, restore);
    #[cfg(test)]
    let seen = |m: &Model| m.seen;
    #[cfg(not(test))]
    let seen = |_: &Model| (0, 0);
    while out.len() < target {
        // what comes after a byte that isn't there is a guess
        // past the memory budget the model can't go on
        if rc.overran() || model.broken || model.heap.over {
            return (out, End::Cut, seen(&model));
        }
        match model.decode_symbol(&mut rc) {
            // a symbol that needed a byte past the end was decided by the zeros read in its place
            Some(_) if rc.overran() => return (out, End::Cut, seen(&model)),
            Some(b) => out.push(b),
            None => {
                let end = if size.is_none() && !model.broken && !model.heap.over { End::Done } else { End::Cut };
                return (out, end, seen(&model));
            }
        }
    }
    let reached_size = size.is_some_and(|s| out.len() as u64 >= s);
    let end = if reached_size { End::Done } else { End::Limit };
    (out, end, seen(&model))
}

#[cfg(test)]
mod tests;
