//! RAR 2.0's compression (version 20, and 26 for files past 2 GiB), from the front of a file's packed data: LZ
//! blocks, and audio blocks that predict each channel's next byte. Written from the format as unrar describes it.

use super::super::lzma::End;
use super::rar3::adapt;
use super::rarbits::{Bits, MARGIN, Table};

const NC: usize = 298;
const DC: usize = 48;
const RC: usize = 28;
const BC: usize = 19;
const MC: usize = 257;

const LDECODE: [u8; 28] =
    [0, 1, 2, 3, 4, 5, 6, 7, 8, 10, 12, 14, 16, 20, 24, 28, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224];
const LBITS: [u8; 28] = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5];
const DDECODE: [u32; DC] = [
    0, 1, 2, 3, 4, 6, 8, 12, 16, 24, 32, 48, 64, 96, 128, 192, 256, 384, 512, 768, 1024, 1536, 2048, 3072, 4096, 6144,
    8192, 12288, 16384, 24576, 32768, 49152, 65536, 98304, 131072, 196608, 262144, 327680, 393216, 458752, 524288,
    589824, 655360, 720896, 786432, 851968, 917504, 983040,
];
const DBITS: [u8; DC] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13, 14, 14, 15, 15,
    16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16,
];
const SDDECODE: [u8; 8] = [0, 4, 8, 16, 32, 64, 128, 192];
const SDBITS: [u8; 8] = [2, 2, 3, 4, 5, 6, 6, 6];

#[derive(Default, Clone, Copy)]
struct Audio {
    k: [i32; 5],
    d: [i32; 4],
    last_delta: i32,
    dif: [u32; 11],
    count: u32,
    last: u32,
}

enum Blocks {
    Lz(Box<[Table; 3]>),
    Audio(Vec<Table>),
}

struct State<'a> {
    bits: Bits<'a>,
    safe: usize,
    old_table: [u8; MC * 4],
    reps: [usize; 4],
    rep_at: usize,
    last_dist: usize,
    last_len: usize,
    audio: [Audio; 4],
    channel: usize,
    channel_delta: i32,
}

/// One file's packed data, from the front, to `limit` bytes. `complete`: `data` is all of it, not a cut front.
pub fn rar20(data: &[u8], size: Option<u64>, limit: usize, complete: bool) -> (Vec<u8>, End) {
    // a cut stream shorter than the margin has no byte that's safe to decode from
    if !complete && data.len() < MARGIN {
        return (Vec::new(), End::Cut);
    }
    let target = size.and_then(|s| usize::try_from(s).ok()).unwrap_or(usize::MAX).min(limit);
    let mut st = State {
        bits: Bits { data, addr: 0, bit: 0 },
        safe: if complete { data.len() } else { data.len() - MARGIN },
        old_table: [0; MC * 4],
        reps: [0; 4],
        rep_at: 0,
        last_dist: 0,
        last_len: 0,
        audio: [Audio::default(); 4],
        channel: 0,
        channel_delta: 0,
    };
    let mut out = Vec::new();
    let end = match run(&mut st, target, &mut out) {
        Some(()) if out.len() >= limit => End::Limit,
        Some(()) => End::Done,
        None => End::Cut,
    };
    (out, end)
}

fn run(st: &mut State<'_>, target: usize, out: &mut Vec<u8>) -> Option<()> {
    let mut blocks = read_tables(st)?;
    loop {
        if out.len() >= target {
            return Some(());
        }
        if st.bits.addr > st.safe || st.bits.past_end() {
            return None;
        }
        let bits = &mut st.bits;
        if let Blocks::Audio(md) = &blocks {
            let n = md.get(st.channel)?.decode(bits);
            if n == 256 {
                blocks = read_tables(st)?;
                continue;
            }
            let b = decode_audio(st, n as u8);
            out.push(b);
            st.channel += 1;
            if st.channel == md.len() {
                st.channel = 0;
            }
            continue;
        }
        let Blocks::Lz(lz) = &blocks else { return None };
        let [ld, dd, rd] = &**lz;
        let number = usize::from(ld.decode(bits));
        if number < 256 {
            out.push(number as u8);
            continue;
        }
        let (len, dist) = if number > 269 {
            let n = number - 270;
            let mut len = usize::from(LDECODE[n]) + 3 + bits.take(u32::from(LBITS[n])) as usize;
            let slot = usize::from(dd.decode(bits)).min(DC - 1);
            let dist = (DDECODE[slot] + 1 + bits.take(u32::from(DBITS[slot]))) as usize;
            if dist >= 0x2000 {
                len += 1;
                if dist >= 0x40000 {
                    len += 1;
                }
            }
            (len, dist)
        } else if number == 269 {
            blocks = read_tables(st)?;
            continue;
        } else if number == 256 {
            (st.last_len, st.last_dist)
        } else if number < 261 {
            let dist = st.reps[(st.rep_at.wrapping_sub(number - 256)) & 3];
            let slot = usize::from(rd.decode(bits)).min(RC - 1);
            let mut len = usize::from(LDECODE[slot]) + 2 + bits.take(u32::from(LBITS[slot])) as usize;
            if dist >= 0x101 {
                len += 1;
                if dist >= 0x2000 {
                    len += 1;
                    if dist >= 0x40000 {
                        len += 1;
                    }
                }
            }
            (len, dist)
        } else {
            let n = number - 261;
            (2, usize::from(SDDECODE[n]) + 1 + bits.take(u32::from(SDBITS[n])) as usize)
        };
        st.last_dist = dist;
        st.reps[st.rep_at] = dist;
        st.rep_at = (st.rep_at + 1) & 3;
        st.last_len = len;
        if len == 0 {
            continue;
        }
        if dist == 0 || dist > out.len() {
            return None;
        }
        for _ in 0..len.min(target - out.len()) {
            out.push(out[out.len() - dist]);
        }
    }
}

fn read_tables(st: &mut State<'_>) -> Option<Blocks> {
    let bits = &mut st.bits;
    let field = bits.peek16();
    let audio = field & 0x8000 != 0;
    if field & 0x4000 == 0 {
        st.old_table = [0; MC * 4];
    }
    bits.skip(2);
    let size = if audio {
        let channels = ((field >> 12) & 3) as usize + 1;
        if st.channel >= channels {
            st.channel = 0;
        }
        bits.skip(2);
        MC * channels
    } else {
        NC + DC + RC
    };
    let lengths: Vec<u8> = (0..BC).map(|_| bits.take(4) as u8).collect();
    let bd = Table::new(&lengths)?;
    let mut table = [0u8; MC * 4];
    let mut i = 0;
    while i < size {
        if bits.past_end() {
            return None;
        }
        let n = bd.decode(bits);
        match n {
            0..=15 => {
                table[i] = (n as u8 + st.old_table[i]) & 15;
                i += 1;
            }
            16 => {
                let times = bits.take(2) as usize + 3;
                let prev = table[i.checked_sub(1)?];
                let end = (i + times).min(size);
                table[i..end].fill(prev);
                i = end;
            }
            _ => {
                let times = if n == 17 { bits.take(3) + 3 } else { bits.take(7) + 11 } as usize;
                let end = (i + times).min(size);
                table[i..end].fill(0);
                i = end;
            }
        }
    }
    if bits.past_end() {
        return None;
    }
    st.old_table[..size].copy_from_slice(&table[..size]);
    if audio {
        let tables = table[..size].chunks(MC).map(Table::new).collect::<Option<Vec<_>>>()?;
        Some(Blocks::Audio(tables))
    } else {
        Some(Blocks::Lz(Box::new([
            Table::new(&table[..NC])?,
            Table::new(&table[NC..NC + DC])?,
            Table::new(&table[NC + DC..NC + DC + RC])?,
        ])))
    }
}

fn decode_audio(st: &mut State<'_>, delta: u8) -> u8 {
    let v = &mut st.audio[st.channel];
    v.count += 1;
    v.d[3] = v.d[2];
    v.d[2] = v.d[1];
    v.d[1] = v.last_delta - v.d[0];
    v.d[0] = v.last_delta;
    let p = (8 * v.last as i32)
        .wrapping_add(v.k[0] * v.d[0])
        .wrapping_add(v.k[1] * v.d[1])
        .wrapping_add(v.k[2] * v.d[2])
        .wrapping_add(v.k[3] * v.d[3])
        .wrapping_add(v.k[4] * st.channel_delta);
    let p = ((p >> 3) & 0xff) as u32;
    let ch = p.wrapping_sub(u32::from(delta));
    let d = i32::from(delta as i8) << 3;
    let cd = st.channel_delta;
    let [d1, d2, d3, d4] = v.d;
    for (j, x) in [d1, d2, d3, d4, cd].into_iter().enumerate() {
        v.dif[1 + 2 * j] += (d - x).unsigned_abs();
        v.dif[2 + 2 * j] += (d + x).unsigned_abs();
    }
    v.dif[0] += d.unsigned_abs();
    v.last_delta = i32::from((ch.wrapping_sub(v.last)) as u8 as i8);
    st.channel_delta = v.last_delta;
    v.last = ch & 0xff;
    if v.count & 0x1f == 0 {
        let [k1, k2, k3, k4, k5] = &mut v.k;
        adapt(&mut v.dif, &mut [k1, k2, k3, k4, k5]);
    }
    ch as u8
}

#[cfg(test)]
mod tests;
