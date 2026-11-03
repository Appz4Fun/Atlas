//! xz's filters that sit in front of LZMA2: Delta and the branch converters (BCJ) for x86, PowerPC, IA-64, ARM,
//! ARM-Thumb, SPARC and ARM64. Written from the xz format and XZ Utils' public-domain filters. Each one undoes, in
//! place, what the encoder did to the decoded bytes, from the front of the stream.

/// One filter, with what its properties say.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Filter {
    /// bytes are stored as the difference from the one `dist` back
    Delta {
        dist: usize,
    },
    Bcj {
        arch: Arch,
        start: u32,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arch {
    X86,
    PowerPc,
    Ia64,
    Arm,
    ArmThumb,
    Sparc,
    Arm64,
}

impl Filter {
    /// From an xz filter id and its properties. None for one there's no decoder for.
    pub fn from_xz(id: u64, props: &[u8]) -> Option<Filter> {
        if id == 0x03 {
            return match props {
                [d] => Some(Filter::Delta { dist: usize::from(*d) + 1 }),
                _ => None,
            };
        }
        let arch = match id {
            0x04 => Arch::X86,
            0x05 => Arch::PowerPc,
            0x06 => Arch::Ia64,
            0x07 => Arch::Arm,
            0x08 => Arch::ArmThumb,
            0x09 => Arch::Sparc,
            0x0a => Arch::Arm64,
            _ => return None,
        };
        let start = match props {
            [] => 0,
            [a, b, c, d] => u32::from_le_bytes([*a, *b, *c, *d]),
            _ => return None,
        };
        Some(Filter::Bcj { arch, start })
    }

    /// Undoes this filter on `buf`, the stream's bytes from its start. Returns how many bytes from the front are
    /// final: the rest are the last few a branch could still start in, which only the stream's end settles.
    pub fn undo(self, buf: &mut [u8]) -> usize {
        match self {
            Filter::Delta { dist } => {
                for i in dist..buf.len() {
                    buf[i] = buf[i].wrapping_add(buf[i - dist]);
                }
                buf.len()
            }
            Filter::Bcj { arch, start } => match arch {
                Arch::X86 => x86(buf, start),
                Arch::PowerPc => powerpc(buf, start),
                Arch::Ia64 => ia64(buf, start),
                Arch::Arm => arm(buf, start),
                Arch::ArmThumb => arm_thumb(buf, start),
                Arch::Sparc => sparc(buf, start),
                Arch::Arm64 => arm64(buf, start),
            },
        }
    }
}

fn x86(buf: &mut [u8], now: u32) -> usize {
    const ALLOWED: [bool; 8] = [true, true, true, false, true, false, false, false];
    const BIT: [u32; 8] = [0, 1, 2, 2, 3, 3, 3, 3];
    let ms = |b: u8| b == 0 || b == 0xff;
    if buf.len() < 5 {
        return 0;
    }
    let mut prev_mask = 0u32;
    let mut prev_pos = now.wrapping_sub(5);
    let mut i = 0usize;
    while i <= buf.len() - 5 {
        let b = buf[i];
        if b != 0xe8 && b != 0xe9 {
            i += 1;
            continue;
        }
        let pos = now.wrapping_add(i as u32);
        let offset = pos.wrapping_sub(prev_pos);
        prev_pos = pos;
        if offset > 5 {
            prev_mask = 0;
        } else {
            for _ in 0..offset {
                prev_mask &= 0x77;
                prev_mask <<= 1;
            }
        }
        let b4 = buf[i + 4];
        if ms(b4) && ALLOWED[((prev_mask >> 1) & 7) as usize] && (prev_mask >> 1) < 0x10 {
            let mut src = u32::from_le_bytes([buf[i + 1], buf[i + 2], buf[i + 3], b4]);
            let mut dest;
            loop {
                dest = src.wrapping_sub(pos.wrapping_add(5));
                if prev_mask == 0 {
                    break;
                }
                let n = BIT[(prev_mask >> 1) as usize];
                if !ms((dest >> (24 - n * 8)) as u8) {
                    break;
                }
                src = dest ^ ((1u32 << (32 - n * 8)) - 1);
            }
            buf[i + 4] = !(((dest >> 24) & 1).wrapping_sub(1)) as u8;
            buf[i + 1..i + 4].copy_from_slice(&dest.to_le_bytes()[..3]);
            i += 5;
            prev_mask = 0;
        } else {
            i += 1;
            prev_mask |= 1;
            if ms(b4) {
                prev_mask |= 0x10;
            }
        }
    }
    i
}

fn powerpc(buf: &mut [u8], now: u32) -> usize {
    let mut i = 0;
    while i + 4 <= buf.len() {
        if buf[i] >> 2 == 0x12 && buf[i + 3] & 3 == 1 {
            let src = u32::from_be_bytes([buf[i] & 3, buf[i + 1], buf[i + 2], buf[i + 3] & !3]);
            let dest = src.wrapping_sub(now.wrapping_add(i as u32));
            buf[i] = 0x48 | ((dest >> 24) & 3) as u8;
            buf[i + 1] = (dest >> 16) as u8;
            buf[i + 2] = (dest >> 8) as u8;
            buf[i + 3] = (buf[i + 3] & 3) | dest as u8;
        }
        i += 4;
    }
    i
}

fn ia64(buf: &mut [u8], now: u32) -> usize {
    const BRANCH: [u32; 32] =
        [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 4, 4, 6, 6, 0, 0, 7, 7, 4, 4, 0, 0, 4, 4, 0, 0];
    let mut i = 0;
    while i + 16 <= buf.len() {
        let mask = BRANCH[usize::from(buf[i] & 0x1f)];
        let mut bit_pos = 5u32;
        for slot in 0..3 {
            if (mask >> slot) & 1 != 0 {
                let byte_pos = i + (bit_pos >> 3) as usize;
                let bit_res = bit_pos & 7;
                let mut instruction = 0u64;
                for j in 0..6 {
                    instruction |= u64::from(buf[byte_pos + j]) << (8 * j);
                }
                let mut norm = instruction >> bit_res;
                if (norm >> 37) & 0xf == 5 && (norm >> 9) & 7 == 0 {
                    let mut src = ((norm >> 13) & 0xfffff) as u32;
                    src |= (((norm >> 36) & 1) as u32) << 20;
                    src <<= 4;
                    let dest = src.wrapping_sub(now.wrapping_add(i as u32)) >> 4;
                    norm &= !(0x8f_ffffu64 << 13);
                    norm |= u64::from(dest & 0xfffff) << 13;
                    norm |= u64::from(dest & 0x10_0000) << (36 - 20);
                    instruction &= (1u64 << bit_res) - 1;
                    instruction |= norm << bit_res;
                    for j in 0..6 {
                        buf[byte_pos + j] = (instruction >> (8 * j)) as u8;
                    }
                }
            }
            bit_pos += 41;
        }
        i += 16;
    }
    i
}

fn arm(buf: &mut [u8], now: u32) -> usize {
    let mut i = 0;
    while i + 4 <= buf.len() {
        if buf[i + 3] == 0xeb {
            let src = u32::from_le_bytes([buf[i], buf[i + 1], buf[i + 2], 0]) << 2;
            let dest = src.wrapping_sub(now.wrapping_add(i as u32 + 8)) >> 2;
            buf[i..i + 3].copy_from_slice(&dest.to_le_bytes()[..3]);
        }
        i += 4;
    }
    i
}

fn arm_thumb(buf: &mut [u8], now: u32) -> usize {
    let mut i = 0;
    while i + 4 <= buf.len() {
        if buf[i + 1] & 0xf8 == 0xf0 && buf[i + 3] & 0xf8 == 0xf8 {
            let src = (u32::from(buf[i + 1] & 7) << 19)
                | (u32::from(buf[i]) << 11)
                | (u32::from(buf[i + 3] & 7) << 8)
                | u32::from(buf[i + 2]);
            let dest = (src << 1).wrapping_sub(now.wrapping_add(i as u32 + 4)) >> 1;
            buf[i + 1] = 0xf0 | ((dest >> 19) & 7) as u8;
            buf[i] = (dest >> 11) as u8;
            buf[i + 3] = 0xf8 | ((dest >> 8) & 7) as u8;
            buf[i + 2] = dest as u8;
            i += 2;
        }
        i += 2;
    }
    i
}

fn sparc(buf: &mut [u8], now: u32) -> usize {
    let mut i = 0;
    while i + 4 <= buf.len() {
        if (buf[i] == 0x40 && buf[i + 1] & 0xc0 == 0) || (buf[i] == 0x7f && buf[i + 1] & 0xc0 == 0xc0) {
            let src = u32::from_be_bytes([buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]) << 2;
            let mut dest = src.wrapping_sub(now.wrapping_add(i as u32)) >> 2;
            dest = ((0u32.wrapping_sub((dest >> 22) & 1) << 22) & 0x3fff_ffff) | (dest & 0x3f_ffff) | 0x4000_0000;
            buf[i..i + 4].copy_from_slice(&dest.to_be_bytes());
        }
        i += 4;
    }
    i
}

fn arm64(buf: &mut [u8], now: u32) -> usize {
    let mut i = 0;
    while i + 4 <= buf.len() {
        let pc = now.wrapping_add(i as u32);
        let instr = u32::from_le_bytes([buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]);
        if instr >> 26 == 0x25 {
            // BL
            let out = 0x9400_0000 | (instr.wrapping_sub(pc >> 2) & 0x03ff_ffff);
            buf[i..i + 4].copy_from_slice(&out.to_le_bytes());
        } else if instr & 0x9f00_0000 == 0x9000_0000 {
            // ADRP
            let src = ((instr >> 29) & 3) | ((instr >> 3) & 0x001f_fffc);
            if src.wrapping_add(0x0002_0000) & 0x001c_0000 == 0 {
                let dest = src.wrapping_sub(pc >> 12);
                let mut out = instr & 0x9000_001f;
                out |= (dest & 3) << 29;
                out |= (dest & 0x0003_fffc) << 3;
                out |= 0u32.wrapping_sub(dest & 0x0002_0000) & 0x00e0_0000;
                buf[i..i + 4].copy_from_slice(&out.to_le_bytes());
            }
        }
        i += 4;
    }
    i
}

#[cfg(test)]
mod tests;
