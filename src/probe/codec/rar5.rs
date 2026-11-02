//! RAR5's compression (RAR 5.0 and later, and RAR 7's extended distances), from the front of a file's packed data.
//! Written from the format as libarchive's reader and the unrar source describe it. Every byte decoded is kept,
//! so the whole output is the dictionary and its size never matters here; the limit bounds it instead.

use super::super::lzma::End;
use super::rarbits::{Bits, Table};

const NC: usize = 306;
const DC_BASE: usize = 64;
const DC_EXTRA: usize = 80;
const LDC: usize = 16;
const RC: usize = 44;
const BC: usize = 20;
const MAX_FILTER_BLOCK: usize = 0x40_0000;
const MAX_FILTERS: usize = 8192;

struct Tables {
    ld: Table,
    dd: Table,
    ldd: Table,
    rd: Table,
}

struct Filter {
    kind: u32,
    start: usize,
    len: usize,
    channels: usize,
}

struct Block {
    /// first byte past the block
    end: usize,
    /// valid bits in its last byte
    last_bits: u32,
    last: bool,
    tables: bool,
}

fn block_header(bits: &mut Bits<'_>) -> Option<Block> {
    bits.align();
    let flags = bits.take(8);
    let count = ((flags >> 3) & 3) + 1;
    if count == 4 {
        return None;
    }
    let check = bits.take(8);
    let mut size = 0u32;
    for i in 0..count {
        size += bits.take(8) << (i * 8);
    }
    let sum = 0x5a ^ flags ^ size ^ (size >> 8) ^ (size >> 16);
    if sum & 0xff != check || bits.past_end() {
        return None;
    }
    Some(Block {
        end: bits.addr + size as usize,
        last_bits: (flags & 7) + 1,
        last: flags & 0x40 != 0,
        tables: flags & 0x80 != 0,
    })
}

fn read_tables(bits: &mut Bits<'_>, extra_dist: bool) -> Option<Tables> {
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
    let bd = Table::new(&lengths)?;
    let dc = if extra_dist { DC_EXTRA } else { DC_BASE };
    let size = NC + dc + LDC + RC;
    let mut table = vec![0u8; size];
    let mut i = 0;
    while i < size {
        if bits.past_end() {
            return None;
        }
        let n = bd.decode(bits);
        match n {
            0..=15 => {
                table[i] = n as u8;
                i += 1;
            }
            16 | 17 => {
                let times = if n == 16 { bits.take(3) + 3 } else { bits.take(7) + 11 } as usize;
                let prev = *table.get(i.checked_sub(1)?)?;
                let end = (i + times).min(size);
                table[i..end].fill(prev);
                i = end;
            }
            _ => {
                let times = if n == 18 { bits.take(3) + 3 } else { bits.take(7) + 11 } as usize;
                let end = (i + times).min(size);
                table[i..end].fill(0);
                i = end;
            }
        }
    }
    if bits.past_end() {
        return None;
    }
    Some(Tables {
        ld: Table::new(&table[..NC])?,
        dd: Table::new(&table[NC..NC + dc])?,
        ldd: Table::new(&table[NC + dc..NC + dc + LDC])?,
        rd: Table::new(&table[NC + dc + LDC..])?,
    })
}

fn slot_to_length(bits: &mut Bits<'_>, slot: u32) -> usize {
    let (lbits, mut len) =
        if slot < 8 { (0, 2 + slot) } else { (slot / 4 - 1, 2 + ((4 | (slot & 3)) << (slot / 4 - 1))) };
    if lbits > 0 {
        len += bits.take(lbits);
    }
    len as usize
}

fn filter_number(bits: &mut Bits<'_>) -> usize {
    let count = bits.take(2) + 1;
    let mut v = 0usize;
    for i in 0..count {
        v += (bits.take(8) as usize) << (i * 8);
    }
    v
}

/// One file's packed data, from the front, to `limit` bytes. `extra_dist`: RAR 7's 80 distance codes.
pub fn rar5(data: &[u8], extra_dist: bool, size: Option<u64>, limit: usize) -> (Vec<u8>, End) {
    let target = size.and_then(|s| usize::try_from(s).ok()).unwrap_or(usize::MAX).min(limit);
    let mut window = Vec::new();
    let mut filters = Vec::new();
    let end = run(data, extra_dist, target, &mut window, &mut filters);
    let end = match end {
        End::Done | End::Limit if window.len() >= limit => End::Limit,
        e => e,
    };
    (apply_filters(window, &filters), end)
}

fn run(data: &[u8], extra_dist: bool, target: usize, out: &mut Vec<u8>, filters: &mut Vec<Filter>) -> End {
    let mut bits = Bits { data, addr: 0, bit: 0 };
    let Some(mut block) = block_header(&mut bits) else { return End::Cut };
    if !block.tables {
        return End::Cut;
    }
    let Some(mut tables) = read_tables(&mut bits, extra_dist) else { return End::Cut };
    let mut reps = [0usize; 4];
    let mut last_len = 0usize;
    // filters that don't overlap cover each byte once: more than that is work no real file asks for
    let mut filter_work = target.saturating_add(MAX_FILTER_BLOCK);
    loop {
        if out.len() >= target {
            return End::Done;
        }
        // the end of a block: on to the next, or done
        while bits.addr > block.end.saturating_sub(1) || (bits.addr + 1 == block.end && bits.bit >= block.last_bits) {
            if block.last {
                return End::Done;
            }
            let Some(next) = block_header(&mut bits) else { return End::Cut };
            block = next;
            if block.tables {
                let Some(t) = read_tables(&mut bits, extra_dist) else { return End::Cut };
                tables = t;
            }
        }
        // a block cut short: stop with room to spare for the longest symbol
        if block.end > data.len() && bits.addr + 16 > data.len() {
            return End::Cut;
        }
        let slot = u32::from(tables.ld.decode(&mut bits));
        if slot < 256 {
            out.push(slot as u8);
            continue;
        }
        if slot >= 262 {
            let mut len = slot_to_length(&mut bits, slot - 262);
            let dslot = u32::from(tables.dd.decode(&mut bits));
            let mut dist: u64 = 1;
            let dbits;
            if dslot < 4 {
                dbits = 0;
                dist += u64::from(dslot);
            } else {
                dbits = dslot / 2 - 1;
                dist += u64::from(2 | (dslot & 1)) << dbits;
            }
            if dbits > 0 {
                if dbits >= 4 {
                    if dbits > 4 {
                        if dbits > 36 {
                            dist += (bits.peek64() >> (68 - dbits)) << 4;
                        } else {
                            dist += u64::from(bits.peek32() >> (36 - dbits)) << 4;
                        }
                        bits.skip(dbits - 4);
                    }
                    dist += u64::from(tables.ldd.decode(&mut bits));
                } else {
                    dist += u64::from(bits.take(dbits));
                }
            }
            if dist > 0x100 {
                len += 1;
                if dist > 0x2000 {
                    len += 1;
                    if dist > 0x40000 {
                        len += 1;
                    }
                }
            }
            let Ok(dist) = usize::try_from(dist) else { return End::Cut };
            reps = [dist, reps[0], reps[1], reps[2]];
            last_len = len;
            if copy(out, dist, len, target).is_none() {
                return End::Cut;
            }
            continue;
        }
        if slot == 256 {
            let start = filter_number(&mut bits);
            let len = filter_number(&mut bits);
            let kind = bits.take(3);
            let channels = if kind == 0 { bits.take(5) as usize + 1 } else { 0 };
            if (4..=MAX_FILTER_BLOCK).contains(&len) {
                // past the caps a filter can't be kept, so what follows can't be trusted: stop where it is
                if filters.len() == MAX_FILTERS || len > filter_work {
                    return End::Cut;
                }
                filter_work -= len;
                filters.push(Filter { kind, start: out.len() + start, len, channels });
            }
            continue;
        }
        if slot == 257 {
            if last_len != 0 && copy(out, reps[0], last_len, target).is_none() {
                return End::Cut;
            }
            continue;
        }
        let n = (slot - 258) as usize;
        let dist = reps[n];
        reps.copy_within(0..n, 1);
        reps[0] = dist;
        let lslot = u32::from(tables.rd.decode(&mut bits));
        let len = slot_to_length(&mut bits, lslot);
        last_len = len;
        if copy(out, dist, len, target).is_none() {
            return End::Cut;
        }
    }
}

fn copy(out: &mut Vec<u8>, dist: usize, len: usize, target: usize) -> Option<()> {
    if dist == 0 || dist > out.len() {
        return None;
    }
    for _ in 0..len.min(target.saturating_sub(out.len())) {
        out.push(out[out.len() - dist]);
    }
    Some(())
}

/// The filters run on the decoded bytes, now that they're all here. One whose block runs past what was decoded
/// can't be run: the output stops where it starts.
fn apply_filters(window: Vec<u8>, filters: &[Filter]) -> Vec<u8> {
    if filters.is_empty() {
        return window;
    }
    let mut out = window.clone();
    for f in filters {
        let Some(src) = f.start.checked_add(f.len).and_then(|end| window.get(f.start..end)) else {
            out.truncate(f.start.min(out.len()));
            break;
        };
        let dst = &mut out[f.start..f.start + f.len];
        match f.kind {
            0 => {
                let mut s = 0;
                for ch in 0..f.channels {
                    let mut prev = 0u8;
                    let mut d = ch;
                    while d < f.len {
                        prev = prev.wrapping_sub(src[s]);
                        dst[d] = prev;
                        s += 1;
                        d += f.channels;
                    }
                }
            }
            1 | 2 => {
                const FILE_SIZE: u32 = 0x100_0000;
                let mut i = 0usize;
                while i + 4 < f.len {
                    let b = src[i];
                    i += 1;
                    if b == 0xe8 || (f.kind == 2 && b == 0xe9) {
                        let offset = ((i + f.start) as u32) % FILE_SIZE;
                        let addr = u32::from_le_bytes(src[i..i + 4].try_into().unwrap_or_default());
                        if addr & 0x8000_0000 != 0 {
                            if addr.wrapping_add(offset) & 0x8000_0000 == 0 {
                                dst[i..i + 4].copy_from_slice(&addr.wrapping_add(FILE_SIZE).to_le_bytes());
                            }
                        } else if addr.wrapping_sub(FILE_SIZE) & 0x8000_0000 != 0 {
                            dst[i..i + 4].copy_from_slice(&addr.wrapping_sub(offset).to_le_bytes());
                        }
                        i += 4;
                    }
                }
            }
            3 => {
                let mut i = 0usize;
                while i + 3 < f.len {
                    if src[i + 3] == 0xeb {
                        let mut offset = u32::from(src[i]) | u32::from(src[i + 1]) << 8 | u32::from(src[i + 2]) << 16;
                        offset = offset.wrapping_sub(((i + f.start) / 4) as u32);
                        dst[i..i + 3].copy_from_slice(&offset.to_le_bytes()[..3]);
                    }
                    i += 4;
                }
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests;
