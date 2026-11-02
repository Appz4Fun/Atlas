//! LZ4's frame format, and the older legacy frame, from the front.

use super::super::lzma::End;

const FRAME: u32 = 0x184d_2204;
const LEGACY: u32 = 0x184c_2102;

fn le32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

/// LZ4 frames (and skippable frames between them) from the front, to `limit` bytes.
pub fn lz4(input: &[u8], limit: usize) -> (Vec<u8>, End) {
    let mut out = Vec::new();
    let end = frames(input, &mut out, limit).unwrap_or(End::Cut);
    out.truncate(limit);
    (out, end)
}

fn frames(input: &[u8], out: &mut Vec<u8>, limit: usize) -> Option<End> {
    let mut p = 0usize;
    while p < input.len() {
        let magic = le32(input, p)?;
        p += 4;
        if magic & 0xffff_fff0 == 0x184d_2a50 {
            p = p.checked_add(le32(input, p)? as usize + 4)?;
            continue;
        }
        if magic == LEGACY {
            // blocks of up to 8 MiB each, until another magic number or the end
            while let Some(size) = le32(input, p) {
                if size == FRAME || size == LEGACY {
                    break;
                }
                // a block cut short still gives what it holds
                let to = (p + 4).checked_add(size as usize)?;
                let block = input.get(p + 4..to.min(input.len()))?;
                let cut = to > input.len();
                p = to;
                let whole = sequences(block, out, limit);
                if cut {
                    return if out.len() >= limit { Some(End::Limit) } else { None };
                }
                whole?;
                if out.len() >= limit {
                    return Some(End::Limit);
                }
            }
            continue;
        }
        if magic != FRAME {
            return None;
        }
        let flg = *input.get(p)?;
        if flg >> 6 != 1 {
            return None;
        }
        let block_checksum = flg & 0x10 != 0;
        let content_size = flg & 0x08 != 0;
        let content_checksum = flg & 0x04 != 0;
        let dict = flg & 0x01 != 0;
        p += 2 + if content_size { 8 } else { 0 } + if dict { 4 } else { 0 } + 1;
        loop {
            let size = le32(input, p)?;
            p += 4;
            if size == 0 {
                break;
            }
            let raw = size & 0x8000_0000 != 0;
            let len = (size & 0x7fff_ffff) as usize;
            // a block cut short still gives what it holds
            let block = input.get(p..p.checked_add(len)?.min(input.len()))?;
            let cut = block.len() < len;
            p += len + if block_checksum { 4 } else { 0 };
            if raw {
                out.extend_from_slice(block);
            } else {
                let whole = sequences(block, out, limit);
                if whole.is_none() && !cut {
                    return None;
                }
            }
            if cut {
                return if out.len() >= limit { Some(End::Limit) } else { None };
            }
            if out.len() >= limit {
                return Some(End::Limit);
            }
        }
        if content_checksum {
            p += 4;
        }
    }
    Some(End::Done)
}

/// One compressed block: literals and matches. Matches may reach into earlier blocks, which is fine here, as every
/// byte decoded is kept.
fn sequences(block: &[u8], out: &mut Vec<u8>, limit: usize) -> Option<()> {
    let mut p = 0usize;
    let more = |p: &mut usize, mut n: usize| -> Option<usize> {
        if n == 15 {
            loop {
                let b = *block.get(*p)?;
                *p += 1;
                n += usize::from(b);
                if b != 255 {
                    break;
                }
            }
        }
        Some(n)
    };
    while p < block.len() {
        let token = block[p];
        p += 1;
        let lits = more(&mut p, usize::from(token >> 4))?;
        let Some(literal) = block.get(p..p.checked_add(lits)?) else {
            out.extend_from_slice(&block[p..]);
            return None;
        };
        out.extend_from_slice(literal);
        p += lits;
        if p == block.len() || out.len() >= limit {
            break;
        }
        let back = usize::from(u16::from_le_bytes(block.get(p..p + 2)?.try_into().ok()?));
        p += 2;
        let len = more(&mut p, usize::from(token & 15))? + 4;
        if back == 0 || back > out.len() {
            return None;
        }
        for _ in 0..len.min(limit.saturating_sub(out.len())) {
            out.push(out[out.len() - back]);
        }
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_what_python_lz4_made() {
        let want = include_bytes!("../../../tests/fixtures/nest/mixed.bin");
        for packed in [
            &include_bytes!("../../../tests/fixtures/nest/mixed.lz4")[..],
            include_bytes!("../../../tests/fixtures/nest/mixed-indep.lz4"),
        ] {
            let (out, end) = lz4(packed, usize::MAX);
            assert_eq!(end, End::Done);
            assert!(out == want, "{} bytes", out.len());
            let (front, end) = lz4(&packed[..packed.len() / 2], usize::MAX);
            assert_eq!(end, End::Cut);
            assert!(front.len() > 1000 && want.starts_with(&front));
        }
    }

    #[test]
    fn a_cut_legacy_block_gives_what_it_holds() {
        // one block of 16 literals (18 bytes), its size saying 19 and its last byte missing
        let mut data = vec![0x02, 0x21, 0x4c, 0x18, 19, 0, 0, 0, 0xf0, 1];
        data.extend(b"0123456789abcdef");
        let (out, end) = lz4(&data[..data.len() - 4], usize::MAX);
        assert_eq!(end, End::Cut);
        assert_eq!(out, b"0123456789ab");
        let (out, end) = lz4(&data, usize::MAX);
        assert_eq!(end, End::Cut);
        assert_eq!(out, b"0123456789abcdef");
        let (out, end) = lz4(&data, 5);
        assert_eq!((&out[..], end), (&b"01234"[..], End::Limit));
    }

    #[test]
    fn junk_never_panics() {
        let mut seed = 13u32;
        for n in 0..2000 {
            let mut data = vec![0x04, 0x22, 0x4d, 0x18, 0x40, 0x40, 0];
            data.extend((0..(n % 300)).map(|_| {
                seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
                (seed >> 16) as u8
            }));
            let _ = lz4(&data, 1 << 16);
        }
    }
}
