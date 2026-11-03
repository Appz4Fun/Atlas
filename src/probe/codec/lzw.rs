//! Unix `compress` (`.Z`): LZW with codes from 9 bits up to the header's maximum, read least significant bit first.
//! Codes come in groups of eight, and a change of width (or a clear) skips to the end of the current group, as
//! the original wrote whole groups at a time.

use super::super::lzma::End;

const CLEAR: u32 = 256;

/// A `.Z` stream, header included, from the front, to `limit` bytes.
pub fn uncompress(input: &[u8], limit: usize) -> (Vec<u8>, End) {
    let mut out = Vec::new();
    let end = run(input, &mut out, limit).unwrap_or(End::Cut);
    out.truncate(limit);
    (out, end)
}

fn run(input: &[u8], out: &mut Vec<u8>, limit: usize) -> Option<End> {
    if input.get(..2)? != [0x1f, 0x9d] {
        return None;
    }
    let flags = *input.get(2)?;
    let max_bits = u32::from(flags & 0x1f);
    let block = flags & 0x80 != 0;
    if !(9..=16).contains(&max_bits) {
        return None;
    }
    let max_max = 1u32 << max_bits;
    let mut prefix = vec![0u16; max_max as usize];
    let mut suffix = vec![0u8; max_max as usize];
    let total_bits = (input.len() as u64) * 8;
    let mut pos = 24u64;
    let mut width = 9u32;
    let mut max_code = (1u32 << width) - 1;
    let mut free = if block { 257 } else { 256 };
    let mut in_group = 0u64;
    let mut old: Option<u32> = None;
    let mut last_char = 0u8;
    let mut stack = Vec::new();
    // skips what's left of the current group of eight codes
    let skip = |pos: &mut u64, in_group: &mut u64, width: u32| {
        *pos += ((8 - *in_group % 8) % 8) * u64::from(width);
        *in_group = 0;
    };
    loop {
        if out.len() >= limit {
            return Some(End::Limit);
        }
        if free > max_code && width < max_bits {
            skip(&mut pos, &mut in_group, width);
            width += 1;
            max_code = if width == max_bits { max_max } else { (1 << width) - 1 };
        }
        if pos + u64::from(width) > total_bits {
            // the last group may be short: what's left is padding
            return Some(if total_bits - pos.min(total_bits) < 8 { End::Done } else { End::Cut });
        }
        let code = read(input, pos, width);
        pos += u64::from(width);
        in_group += 1;
        let Some(prev) = old else {
            if code >= 256 {
                return None;
            }
            last_char = code as u8;
            out.push(last_char);
            old = Some(code);
            continue;
        };
        if code == CLEAR && block {
            skip(&mut pos, &mut in_group, width);
            prefix.fill(0);
            free = 256;
            width = 9;
            max_code = (1 << width) - 1;
            continue;
        }
        let incoming = code;
        let mut c = code;
        stack.clear();
        if c >= free {
            if c > free {
                return None;
            }
            stack.push(last_char);
            c = prev;
        }
        while c >= 256 {
            stack.push(suffix[c as usize]);
            c = u32::from(prefix[c as usize]);
            if stack.len() > max_max as usize {
                return None;
            }
        }
        last_char = c as u8;
        stack.push(last_char);
        out.extend(stack.iter().rev());
        if free < max_max {
            prefix[free as usize] = prev as u16;
            suffix[free as usize] = last_char;
            free += 1;
        }
        old = Some(incoming);
    }
}

fn read(input: &[u8], pos: u64, width: u32) -> u32 {
    let mut v = 0u32;
    for i in 0..width {
        let bit = pos + u64::from(i);
        let byte = input[(bit / 8) as usize];
        v |= u32::from((byte >> (bit % 8)) & 1) << i;
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_what_ncompress_made() {
        let want = include_bytes!("../../../tests/fixtures/nest/mixed.bin");
        let packed = include_bytes!("../../../tests/fixtures/nest/mixed.Z");
        let (out, end) = uncompress(packed, usize::MAX);
        assert_eq!(end, End::Done);
        assert!(out == want, "{} bytes of {}", out.len(), want.len());
        let (front, _) = uncompress(&packed[..packed.len() / 3], usize::MAX);
        assert!(front.len() > 1000 && want.starts_with(&front));
    }

    #[test]
    fn junk_never_panics() {
        let mut seed = 11u32;
        for n in 0..2000 {
            let mut data = vec![0x1f, 0x9d, 0x90];
            data.extend((0..(n % 300)).map(|_| {
                seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
                (seed >> 16) as u8
            }));
            let _ = uncompress(&data, 1 << 16);
        }
    }
}
