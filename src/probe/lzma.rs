//! LZMA and LZMA2, decoded from the front only: as much output as the bytes read so far give, up to a limit.
//! Ours rather than a crate's, because the probe holds just a prefix of the stream, and a decoder that gives up on
//! a cut-off stream without handing back what it had decoded is no use for that.

/// lc, lp and pb, the literal context, literal position and position bits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Props {
    pub lc: u32,
    pub lp: u32,
    pub pb: u32,
}

impl Props {
    /// The one byte form, `(pb * 5 + lp) * 9 + lc`.
    pub fn from_byte(b: u8) -> Option<Props> {
        if b >= 9 * 5 * 5 {
            return None;
        }
        let b = u32::from(b);
        Some(Props { lc: b % 9, lp: (b / 9) % 5, pb: b / 45 })
    }
}

/// How a decode ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum End {
    /// The stream said it ended, by an end marker, its size, or LZMA2's last chunk.
    Done,
    /// The output reached the limit asked for.
    Limit,
    /// The input ran out, or what came next made no sense: the output so far still stands.
    Cut,
}

struct Rc<'a> {
    data: &'a [u8],
    pos: usize,
    range: u32,
    code: u32,
}

impl<'a> Rc<'a> {
    fn new(data: &'a [u8]) -> Option<Rc<'a>> {
        // the first byte is always zero
        if *data.first()? != 0 {
            return None;
        }
        let code = u32::from_be_bytes(data.get(1..5)?.try_into().ok()?);
        Some(Rc { data, pos: 5, range: u32::MAX, code })
    }

    fn normalize(&mut self) -> Option<()> {
        if self.range < 1 << 24 {
            let b = *self.data.get(self.pos)?;
            self.pos += 1;
            self.range <<= 8;
            self.code = (self.code << 8) | u32::from(b);
        }
        Some(())
    }

    fn bit(&mut self, prob: &mut u16) -> Option<u32> {
        let bound = (self.range >> 11) * u32::from(*prob);
        let bit = if self.code < bound {
            self.range = bound;
            *prob += (2048 - *prob) >> 5;
            0
        } else {
            self.range -= bound;
            self.code -= bound;
            *prob -= *prob >> 5;
            1
        };
        self.normalize()?;
        Some(bit)
    }

    fn direct(&mut self, count: u32) -> Option<u32> {
        let mut out = 0u32;
        for _ in 0..count {
            self.range >>= 1;
            let bit = if self.code >= self.range {
                self.code -= self.range;
                1
            } else {
                0
            };
            out = (out << 1) | bit;
            self.normalize()?;
        }
        Some(out)
    }

    fn tree(&mut self, probs: &mut [u16], bits: u32) -> Option<u32> {
        let mut m = 1usize;
        for _ in 0..bits {
            m = (m << 1) | self.bit(probs.get_mut(m)?)? as usize;
        }
        Some(m as u32 - (1 << bits))
    }

    fn reverse(&mut self, probs: &mut [u16], bits: u32) -> Option<u32> {
        let mut m = 1usize;
        let mut out = 0;
        for i in 0..bits {
            let bit = self.bit(probs.get_mut(m)?)?;
            m = (m << 1) | bit as usize;
            out |= bit << i;
        }
        Some(out)
    }
}

const INIT: u16 = 1024;

struct LenDecoder {
    choice: u16,
    choice2: u16,
    low: [[u16; 8]; 16],
    mid: [[u16; 8]; 16],
    high: [u16; 256],
}

impl LenDecoder {
    fn new() -> LenDecoder {
        LenDecoder { choice: INIT, choice2: INIT, low: [[INIT; 8]; 16], mid: [[INIT; 8]; 16], high: [INIT; 256] }
    }

    fn decode(&mut self, rc: &mut Rc<'_>, pos_state: usize) -> Option<u32> {
        if rc.bit(&mut self.choice)? == 0 {
            return rc.tree(&mut self.low[pos_state], 3);
        }
        if rc.bit(&mut self.choice2)? == 0 {
            return Some(8 + rc.tree(&mut self.mid[pos_state], 3)?);
        }
        Some(16 + rc.tree(&mut self.high, 8)?)
    }
}

/// An LZMA decoder's state, which LZMA2 keeps from one chunk to the next.
pub struct Decoder {
    props: Props,
    literal: Vec<u16>,
    is_match: [u16; 192],
    is_rep: [u16; 12],
    is_rep0: [u16; 12],
    is_rep1: [u16; 12],
    is_rep2: [u16; 12],
    is_rep0_long: [u16; 192],
    pos_slot: [[u16; 64]; 4],
    pos_special: [u16; 115],
    align: [u16; 16],
    len: LenDecoder,
    rep_len: LenDecoder,
    state: usize,
    reps: [u32; 4],
}

impl Decoder {
    pub fn new(props: Props) -> Decoder {
        Decoder {
            props,
            literal: vec![INIT; 0x300 << (props.lc + props.lp)],
            is_match: [INIT; 192],
            is_rep: [INIT; 12],
            is_rep0: [INIT; 12],
            is_rep1: [INIT; 12],
            is_rep2: [INIT; 12],
            is_rep0_long: [INIT; 192],
            pos_slot: [[INIT; 64]; 4],
            pos_special: [INIT; 115],
            align: [INIT; 16],
            len: LenDecoder::new(),
            rep_len: LenDecoder::new(),
            state: 0,
            reps: [0; 4],
        }
    }

    fn distance(&mut self, rc: &mut Rc<'_>, len: u32) -> Option<u32> {
        let slot = rc.tree(&mut self.pos_slot[len.min(3) as usize], 6)?;
        if slot < 4 {
            return Some(slot);
        }
        let direct = (slot >> 1) - 1;
        let mut dist = (2 | (slot & 1)) << direct;
        if slot < 14 {
            let base = (dist - slot) as usize;
            dist += rc.reverse(self.pos_special.get_mut(base..)?, direct)?;
        } else {
            dist += rc.direct(direct - 4)? << 4;
            dist += rc.reverse(&mut self.align, 4)?;
        }
        Some(dist)
    }

    /// Decodes into `out` until it holds `target` bytes, the stream's end marker, or `limit`. Distances reach back
    /// no further than `dict_start`, where LZMA2 last emptied the dictionary.
    fn run(&mut self, rc: &mut Rc<'_>, out: &mut Vec<u8>, dict_start: usize, target: usize, limit: usize) -> End {
        let stop = target.min(limit);
        let pb_mask = (1usize << self.props.pb) - 1;
        let lp_mask = (1usize << self.props.lp) - 1;
        let lc = self.props.lc;
        while out.len() < stop {
            match self.one(rc, out, dict_start, stop, pb_mask, lp_mask, lc) {
                Some(true) => {}
                Some(false) => return End::Done,
                None => return End::Cut,
            }
        }
        if out.len() >= target { End::Done } else { End::Limit }
    }

    /// One literal or match. Some(false): the end marker.
    #[allow(clippy::too_many_arguments)]
    fn one(
        &mut self,
        rc: &mut Rc<'_>,
        out: &mut Vec<u8>,
        dict_start: usize,
        stop: usize,
        pb_mask: usize,
        lp_mask: usize,
        lc: u32,
    ) -> Option<bool> {
        let pos = out.len() - dict_start;
        let pos_state = pos & pb_mask;
        let s = self.state;
        if rc.bit(&mut self.is_match[(s << 4) + pos_state])? == 0 {
            let prev = if pos > 0 { u32::from(out[out.len() - 1]) } else { 0 };
            let lit_state = ((pos & lp_mask) << lc) + (prev >> (8 - lc)) as usize;
            let probs = self.literal.get_mut(0x300 * lit_state..0x300 * (lit_state + 1))?;
            let mut symbol = 1usize;
            if s >= 7 {
                let back = self.reps[0] as usize + 1;
                let mut match_byte = if back <= pos { u32::from(out[out.len() - back]) } else { 0 };
                while symbol < 0x100 {
                    let match_bit = ((match_byte >> 7) & 1) as usize;
                    match_byte <<= 1;
                    let bit = rc.bit(&mut probs[((1 + match_bit) << 8) + symbol])? as usize;
                    symbol = (symbol << 1) | bit;
                    if match_bit != bit {
                        break;
                    }
                }
            }
            while symbol < 0x100 {
                symbol = (symbol << 1) | rc.bit(&mut probs[symbol])? as usize;
            }
            out.push(symbol as u8);
            self.state = if s < 4 {
                0
            } else if s < 10 {
                s - 3
            } else {
                s - 6
            };
            return Some(true);
        }
        let len;
        if rc.bit(&mut self.is_rep[s])? == 0 {
            len = self.len.decode(rc, pos_state)?;
            self.state = if s < 7 { 7 } else { 10 };
            let dist = self.distance(rc, len)?;
            if dist == u32::MAX {
                return Some(false);
            }
            self.reps = [dist, self.reps[0], self.reps[1], self.reps[2]];
        } else {
            if rc.bit(&mut self.is_rep0[s])? == 0 {
                if rc.bit(&mut self.is_rep0_long[(s << 4) + pos_state])? == 0 {
                    self.state = if s < 7 { 9 } else { 11 };
                    copy(out, dict_start, self.reps[0], 1)?;
                    return Some(true);
                }
            } else {
                let dist;
                if rc.bit(&mut self.is_rep1[s])? == 0 {
                    dist = self.reps[1];
                } else {
                    if rc.bit(&mut self.is_rep2[s])? == 0 {
                        dist = self.reps[2];
                    } else {
                        dist = self.reps[3];
                        self.reps[3] = self.reps[2];
                    }
                    self.reps[2] = self.reps[1];
                }
                self.reps[1] = self.reps[0];
                self.reps[0] = dist;
            }
            len = self.rep_len.decode(rc, pos_state)?;
            self.state = if s < 7 { 8 } else { 11 };
        }
        let want = (len as usize + 2).min(stop - out.len());
        copy(out, dict_start, self.reps[0], want)?;
        Some(true)
    }
}

/// `count` bytes from `dist + 1` back. None: further back than the dictionary holds, a broken stream.
fn copy(out: &mut Vec<u8>, dict_start: usize, dist: u32, count: usize) -> Option<()> {
    let back = dist as usize + 1;
    if back > out.len() - dict_start {
        return None;
    }
    for _ in 0..count {
        out.push(out[out.len() - back]);
    }
    Some(())
}

/// A raw LZMA stream, as 7z, zip, `.lzma` and lzip hold it. `size` is the unpacked size, when known.
pub fn lzma(data: &[u8], props: Props, size: Option<u64>, limit: usize) -> (Vec<u8>, End) {
    let mut out = Vec::new();
    let Some(mut rc) = Rc::new(data) else { return (out, End::Cut) };
    let target = size.and_then(|s| usize::try_from(s).ok()).unwrap_or(usize::MAX);
    if props.lc + props.lp > 12 {
        return (out, End::Cut);
    }
    let mut dec = Decoder::new(props);
    let end = dec.run(&mut rc, &mut out, 0, target, limit);
    (out, end)
}

/// An LZMA2 stream, its chunks one after another.
pub fn lzma2(data: &[u8], limit: usize) -> (Vec<u8>, End) {
    let mut out = Vec::new();
    let mut dec: Option<Decoder> = None;
    let mut dict_start = 0;
    let mut p = 0usize;
    loop {
        if out.len() >= limit {
            out.truncate(limit);
            return (out, End::Limit);
        }
        let Some(&control) = data.get(p) else { return (out, End::Cut) };
        p += 1;
        match control {
            0 => return (out, End::Done),
            1 | 2 => {
                let Some(size) = be16(data, p) else { return (out, End::Cut) };
                p += 2;
                if control == 1 {
                    dict_start = out.len();
                }
                let size = usize::from(size) + 1;
                let take = size.min(data.len() - p).min(limit - out.len());
                out.extend_from_slice(&data[p..p + take]);
                if take < size {
                    let end = if out.len() >= limit { End::Limit } else { End::Cut };
                    return (out, end);
                }
                p += size;
            }
            0x80..=0xff => {
                let (Some(low), Some(packed)) = (be16(data, p), be16(data, p + 2)) else { return (out, End::Cut) };
                p += 4;
                let unpacked = ((usize::from(control & 0x1f)) << 16) + usize::from(low) + 1;
                let packed = usize::from(packed) + 1;
                let mode = (control >> 5) & 3;
                if mode >= 2 {
                    let Some(props) = data.get(p).and_then(|b| Props::from_byte(*b)) else { return (out, End::Cut) };
                    p += 1;
                    if props.lc + props.lp > 4 {
                        return (out, End::Cut);
                    }
                    dec = Some(Decoder::new(props));
                } else if mode == 1 {
                    let Some(props) = dec.as_ref().map(|d| d.props) else { return (out, End::Cut) };
                    dec = Some(Decoder::new(props));
                }
                if mode == 3 {
                    dict_start = out.len();
                }
                let Some(d) = dec.as_mut() else { return (out, End::Cut) };
                let chunk = &data[p.min(data.len())..(p + packed).min(data.len())];
                let Some(mut rc) = Rc::new(chunk) else { return (out, End::Cut) };
                let target = out.len() + unpacked;
                match d.run(&mut rc, &mut out, dict_start, target, limit) {
                    End::Done if out.len() == target => {}
                    End::Limit => return (out, End::Limit),
                    _ => return (out, End::Cut),
                }
                p += packed;
            }
            _ => return (out, End::Cut),
        }
    }
}

fn be16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEXT: &[u8] = b"the quick brown fox jumps over the lazy dog, again and again and again and again. ";

    fn sample() -> Vec<u8> {
        let mut v = Vec::new();
        for i in 0..400u32 {
            v.extend_from_slice(TEXT);
            v.extend_from_slice(&i.to_le_bytes());
        }
        v
    }

    #[test]
    fn decodes_what_xz_made_and_its_prefix() {
        // `.lzma` files made by xz from `sample()`, with real matches in them
        let alone = include_bytes!("../../tests/fixtures/nest/sample.lzma");
        let props = Props::from_byte(alone[0]).unwrap();
        let (out, end) = lzma(&alone[13..], props, None, usize::MAX);
        assert_eq!(end, End::Done);
        assert_eq!(out, sample());
        // cut in half: the front still comes out right
        let (front, end) = lzma(&alone[13..alone.len() / 2], props, None, usize::MAX);
        assert_eq!(end, End::Cut);
        assert!(front.len() > 100 && sample().starts_with(&front));
        // and a limit stops it
        let (front, end) = lzma(&alone[13..], props, None, 1000);
        assert_eq!((front.len(), end), (1000, End::Limit));
    }

    #[test]
    fn decodes_lzma2_and_its_prefix() {
        let raw = include_bytes!("../../tests/fixtures/nest/sample.lzma2");
        let (out, end) = lzma2(raw, usize::MAX);
        assert_eq!(end, End::Done);
        assert_eq!(out, sample());
        let (front, end) = lzma2(&raw[..raw.len() / 2], usize::MAX);
        assert_eq!(end, End::Cut);
        assert!(sample().starts_with(&front));
    }

    #[test]
    fn junk_never_panics() {
        let mut seed = 0x1234_5678u32;
        for n in 0..2000 {
            let data: Vec<u8> = (0..(n % 300))
                .map(|i| {
                    seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
                    if i == 0 { 0 } else { (seed >> 16) as u8 }
                })
                .collect();
            let _ = lzma(&data, Props { lc: 3, lp: 0, pb: 2 }, None, 1 << 16);
            let _ = lzma2(&data, 1 << 16);
        }
    }
}
