//! RAR 2.9's compression (RAR 3.x and 4.x, version 29), from the front of a file's packed data: LZ blocks with
//! Huffman tables coded against the last ones, and the standard filters RAR's VM runs on the output. Written from
//! the format as unrar describes it. Every byte decoded is kept, soo the whole output is the dictionary; the limit
//! bounds it. PPM blocks go through our PPMd variant H.

use super::super::lzma::End;
use super::ppmd_h::Ppm;
use super::rarbits::{Bits, MARGIN, Table};
use crate::archive::crc32;

const NC: usize = 299;
const DC: usize = 60;
const LDC: usize = 17;
const RC: usize = 28;
const BC: usize = 20;
const TABLE: usize = NC + DC + LDC + RC;
const LOW_DIST_REP: u32 = 16;
const MAX_FILTERS: usize = 8192;
/// The VM's memory: no filter's block is bigger
const VM_MEM: usize = 0x40000;

const LDECODE: [u8; 28] =
    [0, 1, 2, 3, 4, 5, 6, 7, 8, 10, 12, 14, 16, 20, 24, 28, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224];
const LBITS: [u8; 28] = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5];
const SDDECODE: [u8; 8] = [0, 4, 8, 16, 32, 64, 128, 192];
const SDBITS: [u8; 8] = [2, 2, 3, 4, 5, 6, 6, 6];

/// The distance slots: how many slots of each bit length.
fn distances() -> ([u32; DC], [u8; DC]) {
    const COUNTS: [usize; 19] = [4, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 14, 0, 12];
    let (mut base, mut bits) = ([0u32; DC], [0u8; DC]);
    let (mut dist, mut slot) = (0u32, 0);
    for (len, &count) in COUNTS.iter().enumerate() {
        for _ in 0..count {
            base[slot] = dist;
            bits[slot] = len as u8;
            dist += 1 << len;
            slot += 1;
        }
    }
    (base, bits)
}

/// The VM's standard filters, told apart by their code's length and CRC.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    E8,
    E8E9,
    Itanium,
    Delta,
    Rgb,
    Audio,
    /// a program of its own: it would need the VM itself
    Other,
}

fn kind_of(code: &[u8]) -> Kind {
    let xor = code.iter().skip(1).fold(0u8, |x, b| x ^ b);
    if code.first() != Some(&xor) {
        return Kind::Other;
    }
    let crc = crc32(code);
    match (code.len(), crc) {
        (53, 0xad57_6887) => Kind::E8,
        (57, 0x3cd7_e57e) => Kind::E8E9,
        (120, 0x3769_893f) => Kind::Itanium,
        (29, 0x0e06_077d) => Kind::Delta,
        (149, 0x1c2c_5dc8) => Kind::Rgb,
        (216, 0xbc85_e701) => Kind::Audio,
        _ => Kind::Other,
    }
}

/// A filter to run on one block of the output.
struct Pending {
    kind: Kind,
    start: usize,
    len: usize,
    regs: [u32; 7],
}

#[derive(Default)]
struct Filters {
    /// the kind of each filter defined so far
    kinds: Vec<Kind>,
    old_lengths: Vec<u32>,
    last: usize,
    pending: Vec<Pending>,
    /// where the furthest pending block ends: past the limit, decoding goes on to here
    reach: usize,
    /// filter work still allowed, block bytes and program bytes alike: filters that don't overlap cover each byte
    /// once, so more than the output and one filter's memory is work no real file asks for
    work: usize,
}

struct Tables {
    ld: Table,
    dd: Table,
    ldd: Table,
    rd: Table,
}

struct State<'a> {
    bits: Bits<'a>,
    /// past here, a cut stream may be reading zeros it doesn't have
    safe: usize,
    old_table: [u8; TABLE],
    reps: [usize; 4],
    last_len: usize,
    low_dist: u32,
    low_reps: u32,
    filters: Filters,
    /// the PPM model, once a PPM block has started one
    ppm: Option<Box<Ppm>>,
    /// in PPM blocks, the byte that says what follows isn't a literal
    esc: u8,
    /// new tables, or a PPM model's start, still allowed: each is work that makes no output, so a stream of
    /// nothing else would run as long as its input
    tables_left: usize,
}

/// Why decoding stopped short of the end.
enum Stop {
    Cut,
}

/// One file's packed data, from the front, to `limit` bytes. `complete`: `data` is all of it, not a cut front.
pub fn rar29(data: &[u8], size: Option<u64>, limit: usize, complete: bool) -> (Vec<u8>, End) {
    // a cut stream shorter than the margin has no byte that's safe to decode from
    if !complete && data.len() < MARGIN {
        return (Vec::new(), End::Cut);
    }
    let size = size.and_then(|s| usize::try_from(s).ok()).unwrap_or(usize::MAX);
    let mut st = State {
        bits: Bits { data, addr: 0, bit: 0 },
        safe: if complete { data.len() } else { data.len() - MARGIN },
        old_table: [0; TABLE],
        reps: [0; 4],
        last_len: 0,
        low_dist: 0,
        low_reps: 0,
        filters: Filters::default(),
        ppm: None,
        esc: 2,
        tables_left: 0,
    };
    let mut window = Vec::new();
    let end = match run(&mut st, size, limit, &mut window) {
        Ok(()) if window.len() >= limit => End::Limit,
        Ok(()) => End::Done,
        Err(Stop::Cut) => End::Cut,
    };
    let mut out = apply_filters(window, &st.filters.pending);
    out.truncate(limit);
    (out, end)
}

/// Decodes to `size` or `limit`, and when the limit falls inside a filter's block, on to the block's end, soo
/// the filter can run on all of it.
fn run(st: &mut State<'_>, size: usize, limit: usize, out: &mut Vec<u8>) -> Result<(), Stop> {
    let (dbase, dbits) = distances();
    let goal = size.min(limit);
    let target = size.min(limit.saturating_add(VM_MEM));
    st.filters.work = target.saturating_add(VM_MEM);
    // real blocks run far longer than 4 KiB
    st.tables_left = 1024 + target / 4096;
    let mut tables = read_tables(st)?;
    loop {
        if out.len() >= target {
            return Ok(());
        }
        if out.len() >= goal {
            // kept as filters come, not scanned for each byte: thousands of filters would make that slow
            if out.len() >= st.filters.reach {
                return Ok(());
            }
        }
        if st.bits.addr > st.safe || st.bits.past_end() {
            return Err(Stop::Cut);
        }
        let Some(lz) = &tables else {
            match ppm_step(st, out, target)? {
                PpmStep::Next => {}
                PpmStep::Tables => tables = read_tables(st)?,
                PpmStep::FileEnd => return Ok(()),
            }
            continue;
        };
        let bits = &mut st.bits;
        let number = usize::from(lz.ld.decode(bits));
        if number < 256 {
            out.push(number as u8);
            continue;
        }
        if number >= 271 {
            let n = number - 271;
            let mut len = usize::from(LDECODE[n]) + 3 + bits.take(u32::from(LBITS[n])) as usize;
            let slot = usize::from(lz.dd.decode(bits)).min(DC - 1);
            let mut dist = dbase[slot] + 1;
            let b = u32::from(dbits[slot]);
            if b > 0 {
                if slot > 9 {
                    if b > 4 {
                        dist += (bits.peek16() >> (20 - b)) << 4;
                        bits.skip(b - 4);
                    }
                    if st.low_reps > 0 {
                        st.low_reps -= 1;
                        dist += st.low_dist;
                    } else {
                        let low = u32::from(lz.ldd.decode(bits));
                        if low == 16 {
                            st.low_reps = LOW_DIST_REP - 1;
                            dist += st.low_dist;
                        } else {
                            dist += low;
                            st.low_dist = low;
                        }
                    }
                } else {
                    dist += bits.take(b);
                }
            }
            if dist >= 0x2000 {
                len += 1;
                if dist >= 0x40000 {
                    len += 1;
                }
            }
            let dist = dist as usize;
            st.reps = [dist, st.reps[0], st.reps[1], st.reps[2]];
            st.last_len = len;
            copy(out, dist, len, target)?;
            continue;
        }
        match number {
            256 => {
                // the end of a block: "1" new tables here, "0x" the file ends
                let field = bits.peek16();
                if field & 0x8000 == 0 {
                    return Ok(());
                }
                bits.skip(1);
                tables = read_tables(st)?;
            }
            257 => read_vm_code(st, out.len())?,
            258 => {
                if st.last_len != 0 {
                    copy(out, st.reps[0], st.last_len, target)?;
                }
            }
            259..=262 => {
                let n = number - 259;
                let dist = st.reps[n];
                st.reps.copy_within(0..n, 1);
                st.reps[0] = dist;
                let slot = usize::from(lz.rd.decode(bits)).min(RC - 1);
                let len = usize::from(LDECODE[slot]) + 2 + bits.take(u32::from(LBITS[slot])) as usize;
                st.last_len = len;
                copy(out, dist, len, target)?;
            }
            _ => {
                let n = number - 263;
                let dist = usize::from(SDDECODE[n]) + 1 + bits.take(u32::from(SDBITS[n])) as usize;
                st.reps = [dist, st.reps[0], st.reps[1], st.reps[2]];
                st.last_len = 2;
                copy(out, dist, 2, target)?;
            }
        }
    }
}

enum PpmStep {
    Next,
    /// the PPM block ended: new tables follow
    Tables,
    FileEnd,
}

/// One step of a PPM block: a byte, a match, or a filter, as the escape byte says.
fn ppm_step(st: &mut State<'_>, out: &mut Vec<u8>, target: usize) -> Result<PpmStep, Stop> {
    let ppm = st.ppm.as_mut().ok_or(Stop::Cut)?;
    let bits = &mut st.bits;
    let mut next = || ppm.decode_char(bits).ok_or(Stop::Cut);
    let ch = next()?;
    if ch != st.esc {
        out.push(ch);
        return Ok(PpmStep::Next);
    }
    match next()? {
        0 => return Ok(PpmStep::Tables),
        2 => return Ok(PpmStep::FileEnd),
        3 => {
            let first = next()?;
            let len = match (first & 7) + 1 {
                7 => usize::from(next()?) + 7,
                8 => usize::from(next()?) * 256 + usize::from(next()?),
                n => usize::from(n),
            };
            if len == 0 {
                return Err(Stop::Cut);
            }
            // decoding the program is work too, and none of it reaches the output
            st.filters.work = st.filters.work.checked_sub(len).ok_or(Stop::Cut)?;
            let code = (0..len).map(|_| next()).collect::<Result<Vec<u8>, Stop>>()?;
            add_vm_code(&mut st.filters, u32::from(first), &code, out.len())?;
        }
        4 => {
            let mut dist = 0usize;
            for _ in 0..3 {
                dist = (dist << 8) | usize::from(next()?);
            }
            let len = usize::from(next()?);
            copy(out, dist + 2, len + 32, target)?;
        }
        5 => {
            let len = usize::from(next()?);
            copy(out, 1, len + 4, target)?;
        }
        // 1: the escape byte itself
        _ => out.push(ch),
    }
    Ok(PpmStep::Next)
}

fn copy(out: &mut Vec<u8>, dist: usize, len: usize, target: usize) -> Result<(), Stop> {
    if dist == 0 || dist > out.len() {
        return Err(Stop::Cut);
    }
    for _ in 0..len.min(target.saturating_sub(out.len())) {
        out.push(out[out.len() - dist]);
    }
    Ok(())
}

/// The tables for the next block: their code lengths, then each length as a change from the last tables'. None:
/// a PPM block, its model ready.
fn read_tables(st: &mut State<'_>) -> Result<Option<Tables>, Stop> {
    st.tables_left = st.tables_left.checked_sub(1).ok_or(Stop::Cut)?;
    let bits = &mut st.bits;
    bits.align();
    let field = bits.peek16();
    if field & 0x8000 != 0 {
        let ppm = st.ppm.get_or_insert_with(Box::default);
        return if ppm.init(bits, &mut st.esc) { Ok(None) } else { Err(Stop::Cut) };
    }
    st.low_dist = 0;
    st.low_reps = 0;
    if field & 0x4000 == 0 {
        st.old_table = [0; TABLE];
    }
    bits.skip(2);
    let mut lengths = [0u8; BC];
    let mut i = 0;
    while i < BC {
        let l = bits.take(4) as u8;
        if l == 15 {
            let zeros = bits.take(4) as usize;
            if zeros == 0 {
                lengths[i] = 15;
                i += 1;
            } else {
                let end = (i + zeros + 2).min(BC);
                lengths[i..end].fill(0);
                i = end;
            }
        } else {
            lengths[i] = l;
            i += 1;
        }
    }
    let bd = Table::new(&lengths).ok_or(Stop::Cut)?;
    let mut table = [0u8; TABLE];
    let mut i = 0;
    while i < TABLE {
        if bits.past_end() {
            return Err(Stop::Cut);
        }
        let n = bd.decode(bits);
        match n {
            0..=15 => {
                table[i] = (n as u8 + st.old_table[i]) & 15;
                i += 1;
            }
            16 | 17 => {
                let times = if n == 16 { bits.take(3) + 3 } else { bits.take(7) + 11 } as usize;
                let prev = *table.get(i.checked_sub(1).ok_or(Stop::Cut)?).ok_or(Stop::Cut)?;
                let end = (i + times).min(TABLE);
                table[i..end].fill(prev);
                i = end;
            }
            _ => {
                let times = if n == 18 { bits.take(3) + 3 } else { bits.take(7) + 11 } as usize;
                let end = (i + times).min(TABLE);
                table[i..end].fill(0);
                i = end;
            }
        }
    }
    if bits.past_end() {
        return Err(Stop::Cut);
    }
    st.old_table = table;
    let t = |r: std::ops::Range<usize>| Table::new(&table[r]).ok_or(Stop::Cut);
    Ok(Some(Tables {
        ld: t(0..NC)?,
        dd: t(NC..NC + DC)?,
        ldd: t(NC + DC..NC + DC + LDC)?,
        rd: t(NC + DC + LDC..TABLE)?,
    }))
}

/// A number in the VM's code: 4, 8, 16 or 32 bits, or a negative byte.
fn vm_number(bits: &mut Bits<'_>) -> u32 {
    let data = bits.peek16();
    match data & 0xc000 {
        0 => {
            bits.skip(6);
            (data >> 10) & 15
        }
        0x4000 => {
            if data & 0x3c00 == 0 {
                bits.skip(14);
                0xffff_ff00 | ((data >> 2) & 0xff)
            } else {
                bits.skip(10);
                (data >> 6) & 0xff
            }
        }
        0x8000 => {
            bits.skip(2);
            let v = bits.peek16();
            bits.skip(16);
            v
        }
        _ => {
            bits.skip(2);
            let high = bits.peek16() << 16;
            bits.skip(16);
            let v = high | bits.peek16();
            bits.skip(16);
            v
        }
    }
}

fn read_vm_code(st: &mut State<'_>, at: usize) -> Result<(), Stop> {
    let bits = &mut st.bits;
    let first = bits.take(8);
    let mut len = (first & 7) + 1;
    if len == 7 {
        len = bits.take(8) + 7;
    } else if len == 8 {
        len = bits.take(16);
    }
    if len == 0 {
        return Err(Stop::Cut);
    }
    st.filters.work = st.filters.work.checked_sub(len as usize).ok_or(Stop::Cut)?;
    let bits = &mut st.bits;
    let code: Vec<u8> = (0..len).map(|_| bits.take(8) as u8).collect();
    if bits.past_end() {
        return Err(Stop::Cut);
    }
    add_vm_code(&mut st.filters, first, &code, at)
}

fn add_vm_code(f: &mut Filters, first: u32, code: &[u8], at: usize) -> Result<(), Stop> {
    let mut bits = Bits { data: code, addr: 0, bit: 0 };
    let mut pos = if first & 0x80 != 0 {
        let n = vm_number(&mut bits) as usize;
        if n == 0 {
            f.kinds.clear();
            f.old_lengths.clear();
            f.last = 0;
            0
        } else {
            n - 1
        }
    } else {
        f.last
    };
    if pos > f.kinds.len() || pos > f.old_lengths.len() || f.pending.len() >= MAX_FILTERS {
        return Err(Stop::Cut);
    }
    f.last = pos;
    let new = pos == f.kinds.len();
    if new {
        if pos > MAX_FILTERS {
            return Err(Stop::Cut);
        }
        f.kinds.push(Kind::Other);
        f.old_lengths.push(0);
    }
    let mut start = vm_number(&mut bits) as usize;
    if first & 0x40 != 0 {
        start += 258;
    }
    let len = if first & 0x20 != 0 {
        let l = vm_number(&mut bits);
        f.old_lengths[pos] = l;
        l
    } else {
        f.old_lengths[pos]
    };
    let mut regs = [0u32; 7];
    regs[4] = len;
    if first & 0x10 != 0 {
        let mask = bits.peek16() >> 9;
        bits.skip(7);
        for (i, r) in regs.iter_mut().enumerate() {
            if mask & (1 << i) != 0 {
                *r = vm_number(&mut bits);
            }
        }
    }
    if new {
        let size = vm_number(&mut bits) as usize;
        if size == 0 || size >= 0x10000 || bits.addr + size > code.len() {
            return Err(Stop::Cut);
        }
        let prg: Vec<u8> = (0..size).map(|_| bits.take(8) as u8).collect();
        f.kinds[pos] = kind_of(&prg);
    }
    pos = pos.min(f.kinds.len() - 1);
    f.work = f.work.checked_sub(len as usize).ok_or(Stop::Cut)?;
    f.reach = f.reach.max((at + start).saturating_add(len as usize));
    f.pending.push(Pending { kind: f.kinds[pos], start: at + start, len: len as usize, regs });
    Ok(())
}

/// The filters run on the decoded bytes, now that they're all here, each on its block as decoded or, when it
/// follows another on the same block, on what that one made. One that can't be run stops the output where its
/// block starts.
fn apply_filters(window: Vec<u8>, pending: &[Pending]) -> Vec<u8> {
    if pending.is_empty() {
        return window;
    }
    let mut out = window.clone();
    let mut i = 0;
    while i < pending.len() {
        let f = &pending[i];
        let src = f.start.checked_add(f.len).and_then(|end| window.get(f.start..end));
        let Some(mut data) = src.and_then(|src| filter(f, src.to_vec())) else {
            out.truncate(f.start.min(out.len()));
            break;
        };
        while let Some(next) = pending.get(i + 1).filter(|n| n.start == f.start && n.len == data.len()) {
            let Some(again) = filter(next, data) else {
                out.truncate(f.start.min(out.len()));
                return out;
            };
            data = again;
            i += 1;
        }
        if data.len() != f.len {
            out.truncate(f.start.min(out.len()));
            break;
        }
        out[f.start..f.start + f.len].copy_from_slice(&data);
        i += 1;
    }
    out
}

/// One standard filter on one block. None: it can't be run here.
fn filter(f: &Pending, mut data: Vec<u8>) -> Option<Vec<u8>> {
    let size = f.regs[4] as usize;
    if f.kind == Kind::Other || size != data.len() || size > VM_MEM {
        return None;
    }
    // what the VM's register 6 holds: how much of the file was written before this block
    let offset = f.start as u32;
    match f.kind {
        Kind::E8 | Kind::E8E9 => {
            if size < 4 {
                return Some(data);
            }
            const FILE_SIZE: u32 = 0x100_0000;
            let mut i = 0usize;
            while i < size - 4 {
                let b = data[i];
                i += 1;
                if b == 0xe8 || (f.kind == Kind::E8E9 && b == 0xe9) {
                    let at = (i as u32).wrapping_add(offset);
                    let addr = u32::from_le_bytes(data[i..i + 4].try_into().ok()?);
                    if addr & 0x8000_0000 != 0 {
                        if addr.wrapping_add(at) & 0x8000_0000 == 0 {
                            data[i..i + 4].copy_from_slice(&addr.wrapping_add(FILE_SIZE).to_le_bytes());
                        }
                    } else if addr.wrapping_sub(FILE_SIZE) & 0x8000_0000 != 0 {
                        data[i..i + 4].copy_from_slice(&addr.wrapping_sub(at).to_le_bytes());
                    }
                    i += 4;
                }
            }
            Some(data)
        }
        Kind::Itanium => {
            if size < 21 {
                return Some(data);
            }
            const MASKS: [u8; 16] = [4, 4, 6, 6, 0, 0, 7, 7, 4, 4, 0, 0, 4, 4, 0, 0];
            let mut file = offset >> 4;
            let mut at = 0;
            while at < size - 21 {
                let b = i32::from(data[at] & 0x1f) - 0x10;
                if b >= 0 {
                    let mask = MASKS[b as usize];
                    for slot in 0..3 {
                        if mask & (1 << slot) != 0 {
                            let start = slot * 41 + 5;
                            if bundle_bits(&data[at..], start + 37, 4) == 5 {
                                let v = bundle_bits(&data[at..], start + 13, 20);
                                set_bundle_bits(&mut data[at..], v.wrapping_sub(file) & 0xfffff, start + 13, 20);
                            }
                        }
                    }
                }
                at += 16;
                file = file.wrapping_add(1);
            }
            Some(data)
        }
        Kind::Delta => {
            let channels = f.regs[0] as usize;
            if size > VM_MEM / 2 || channels == 0 || channels > 1024 {
                return Some(data);
            }
            let mut out = vec![0u8; size];
            let mut src = 0;
            for ch in 0..channels {
                let mut prev = 0u8;
                let mut d = ch;
                while d < size {
                    prev = prev.wrapping_sub(data[src]);
                    out[d] = prev;
                    src += 1;
                    d += channels;
                }
            }
            Some(out)
        }
        Kind::Rgb => {
            let width = (f.regs[0] as usize).wrapping_sub(3);
            let pos_r = f.regs[1] as usize;
            if !(3..=VM_MEM / 2).contains(&size) || width > size || pos_r > 2 {
                return Some(data);
            }
            let mut out = vec![0u8; size];
            let mut src = 0;
            for ch in 0..3 {
                let mut prev = 0u32;
                let mut i = ch;
                while i < size {
                    let predicted = if i >= width + 3 {
                        let upper = u32::from(out[i - width]);
                        let upper_left = u32::from(out[i - width - 3]);
                        let p = prev.wrapping_add(upper).wrapping_sub(upper_left);
                        let pa = (p.wrapping_sub(prev) as i32).unsigned_abs();
                        let pb = (p.wrapping_sub(upper) as i32).unsigned_abs();
                        let pc = (p.wrapping_sub(upper_left) as i32).unsigned_abs();
                        if pa <= pb && pa <= pc {
                            prev
                        } else if pb <= pc {
                            upper
                        } else {
                            upper_left
                        }
                    } else {
                        prev
                    };
                    let b = (predicted as u8).wrapping_sub(data[src]);
                    src += 1;
                    out[i] = b;
                    prev = u32::from(b);
                    i += 3;
                }
            }
            let mut i = pos_r;
            while i + 2 < size {
                let g = out[i + 1];
                out[i] = out[i].wrapping_add(g);
                out[i + 2] = out[i + 2].wrapping_add(g);
                i += 3;
            }
            Some(out)
        }
        Kind::Audio => {
            let channels = f.regs[0] as usize;
            if size > VM_MEM / 2 || channels == 0 || channels > 128 {
                return Some(data);
            }
            let mut out = vec![0u8; size];
            let mut src = 0;
            for ch in 0..channels {
                let (mut prev_byte, mut prev_delta) = (0u32, 0i32);
                let mut dif = [0u32; 7];
                let (mut d1, mut d2) = (0i32, 0i32);
                let (mut k1, mut k2, mut k3) = (0i32, 0i32, 0i32);
                let mut i = ch;
                let mut count = 0u32;
                while i < size {
                    let d3 = d2;
                    d2 = prev_delta - d1;
                    d1 = prev_delta;
                    let predicted = (8 * prev_byte as i32 + k1 * d1 + k2 * d2 + k3 * d3) as u32;
                    let predicted = (predicted >> 3) & 0xff;
                    let cur = data[src];
                    src += 1;
                    let b = predicted.wrapping_sub(u32::from(cur)) as u8;
                    out[i] = b;
                    prev_delta = i32::from(b.wrapping_sub(prev_byte as u8) as i8);
                    prev_byte = u32::from(b);
                    let d = i32::from(cur as i8) << 3;
                    dif[0] += d.unsigned_abs();
                    dif[1] += (d - d1).unsigned_abs();
                    dif[2] += (d + d1).unsigned_abs();
                    dif[3] += (d - d2).unsigned_abs();
                    dif[4] += (d + d2).unsigned_abs();
                    dif[5] += (d - d3).unsigned_abs();
                    dif[6] += (d + d3).unsigned_abs();
                    if count & 0x1f == 0 {
                        adapt(&mut dif, &mut [&mut k1, &mut k2, &mut k3]);
                    }
                    count += 1;
                    i += channels;
                }
            }
            Some(out)
        }
        Kind::Other => None,
    }
}

/// Every 32 bytes the audio predictors lean toward whichever coefficient would have erred least.
pub(super) fn adapt(dif: &mut [u32], k: &mut [&mut i32]) {
    let mut min = dif[0];
    let mut which = 0;
    dif[0] = 0;
    for (j, d) in dif.iter_mut().enumerate().skip(1) {
        if *d < min {
            min = *d;
            which = j;
        }
        *d = 0;
    }
    if which > 0 {
        let c = &mut *k[(which - 1) / 2];
        if which % 2 == 1 {
            if *c >= -16 {
                *c -= 1;
            }
        } else if *c < 16 {
            *c += 1;
        }
    }
}

fn bundle_bits(data: &[u8], pos: usize, count: u32) -> u32 {
    let at = pos / 8;
    let v = u32::from_le_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]]) >> (pos & 7);
    v & (u32::MAX >> (32 - count))
}

fn set_bundle_bits(data: &mut [u8], value: u32, pos: usize, count: u32) {
    let at = pos / 8;
    let bit = pos & 7;
    let mut and = !((u32::MAX >> (32 - count)) << bit);
    let mut value = value << bit;
    for b in &mut data[at..at + 4] {
        *b = (u32::from(*b) & and) as u8 | value as u8;
        and = (and >> 8) | 0xff00_0000;
        value >>= 8;
    }
}

#[cfg(test)]
pub(crate) mod tests;
