//! Subject templates: the per-article counters of a binary post's subject
//! (`[14/40]`, `(01/85)`, `.vol01.par2`) are lifted out into integer columns
//! so every article of a file shares one template string.

use crate::varint::write_padded_u32;

pub const SLOT_BRACKET_FRAC: u8 = 0x01;
pub const SLOT_PAREN_FRAC: u8 = 0x02;
pub const SLOT_FILE_PART: u8 = 0x03;

/// Slot indices into [`SubjectNums::vals`] and [`PadMask`].
pub const BRK_NUM: usize = 0;
pub const BRK_DEN: usize = 1;
pub const PAR_NUM: usize = 2;
pub const PAR_DEN: usize = 3;
pub const FILE_PART: usize = 4;

/// Leading-zero widths of the five counters, 3 bits each (`slot * 3`):
/// 0 = no leading zero, 2..=7 = zero-padded to that many digits. Bit 15 is
/// not used by the subject codec; the container marks literal rows with it.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
#[repr(transparent)]
pub struct PadMask(pub u16);

impl PadMask {
    #[inline(always)]
    pub fn width(self, slot: usize) -> usize {
        ((self.0 >> (slot * 3)) & 0b111) as usize
    }
    #[inline(always)]
    pub fn set_width(&mut self, slot: usize, w: u8) {
        debug_assert!(w == 0 || (2..=7).contains(&w));
        self.0 = (self.0 & !(0b111 << (slot * 3))) | (((w as u16) & 0b111) << (slot * 3));
    }
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct SubjectNums {
    pub vals: [u32; 5],
    pub pad: PadMask,
}

/// One counter: value and leading-zero width code, from a run of 1..=9
/// digits. None when the run can't be written back exactly (a zero-padded
/// run longer than 7 digits).
#[inline(always)]
fn counter(digits: &[u8]) -> Option<(u32, u8)> {
    let n = digits.len();
    if n == 0 || n > 9 {
        return None;
    }
    let width = if n > 1 && digits[0] == b'0' {
        if n > 7 {
            return None;
        }
        n as u8
    } else {
        0
    };
    let mut v = 0u32;
    for &d in digits {
        v = v * 10 + (d - b'0') as u32;
    }
    Some((v, width))
}

#[inline(always)]
fn digit_run(s: &[u8], from: usize) -> usize {
    let mut j = from;
    while j < s.len() && s[j].is_ascii_digit() {
        j += 1;
    }
    j - from
}

/// `NUM/DEN` then `close` at `s[i..]` (i just past the opening bracket).
/// Returns (num, num_w, den, den_w, index just past `close`).
#[inline(always)]
fn fraction(s: &[u8], i: usize, close: u8) -> Option<(u32, u8, u32, u8, usize)> {
    let a = digit_run(s, i);
    let slash = i + a;
    if a == 0 || s.get(slash) != Some(&b'/') {
        return None;
    }
    let b = digit_run(s, slash + 1);
    let end = slash + 1 + b;
    if b == 0 || s.get(end) != Some(&close) {
        return None;
    }
    let (n, nw) = counter(&s[i..slash])?;
    let (d, dw) = counter(&s[slash + 1..end])?;
    Some((n, nw, d, dw, end + 1))
}

/// Single pass over `subj`: writes the template into `tpl` (cleared first) and
/// returns the counters. Returns None (and leaves `tpl` empty) when the
/// subject already holds a placeholder byte, so the row must be kept whole.
pub fn extract_subject_template(subj: &[u8], tpl: &mut Vec<u8>) -> Option<SubjectNums> {
    tpl.clear();
    let mut nums = SubjectNums::default();
    let (mut brk, mut par, mut file) = (false, false, false);
    let mut i = 0;
    while i < subj.len() {
        let b = subj[i];
        match b {
            SLOT_BRACKET_FRAC | SLOT_PAREN_FRAC | SLOT_FILE_PART => {
                tpl.clear();
                return None;
            }
            b'[' if !brk => {
                if let Some((n, nw, d, dw, next)) = fraction(subj, i + 1, b']') {
                    brk = true;
                    nums.vals[BRK_NUM] = n;
                    nums.vals[BRK_DEN] = d;
                    nums.pad.set_width(BRK_NUM, nw);
                    nums.pad.set_width(BRK_DEN, dw);
                    tpl.extend_from_slice(&[b'[', SLOT_BRACKET_FRAC, b']']);
                    i = next;
                    continue;
                }
            }
            b'(' if !par => {
                if let Some((n, nw, d, dw, next)) = fraction(subj, i + 1, b')') {
                    par = true;
                    nums.vals[PAR_NUM] = n;
                    nums.vals[PAR_DEN] = d;
                    nums.pad.set_width(PAR_NUM, nw);
                    nums.pad.set_width(PAR_DEN, dw);
                    tpl.extend_from_slice(&[b'(', SLOT_PAREN_FRAC, b')']);
                    i = next;
                    continue;
                }
            }
            b'.' if !file => {
                let n = digit_run(subj, i + 1);
                if (1..=5).contains(&n)
                    && subj.get(i + 1 + n) == Some(&b'.')
                    && let Some((v, w)) = counter(&subj[i + 1..i + 1 + n])
                {
                    file = true;
                    nums.vals[FILE_PART] = v;
                    nums.pad.set_width(FILE_PART, w);
                    tpl.extend_from_slice(&[b'.', SLOT_FILE_PART]);
                    // the closing dot is the next literal byte
                    i += 1 + n;
                    continue;
                }
            }
            _ => {}
        }
        tpl.push(b);
        i += 1;
    }
    Some(nums)
}

/// Writes the subject back from its template and counters.
#[inline]
pub fn render_subject(tpl: &[u8], nums: &SubjectNums, out: &mut Vec<u8>) {
    let mut rest = tpl;
    while let Some(p) = rest.iter().position(|&b| b <= SLOT_FILE_PART && b != 0) {
        out.extend_from_slice(&rest[..p]);
        let pad = nums.pad;
        match rest[p] {
            SLOT_BRACKET_FRAC => {
                write_padded_u32(nums.vals[BRK_NUM], pad.width(BRK_NUM), out);
                out.push(b'/');
                write_padded_u32(nums.vals[BRK_DEN], pad.width(BRK_DEN), out);
            }
            SLOT_PAREN_FRAC => {
                write_padded_u32(nums.vals[PAR_NUM], pad.width(PAR_NUM), out);
                out.push(b'/');
                write_padded_u32(nums.vals[PAR_DEN], pad.width(PAR_DEN), out);
            }
            _ => write_padded_u32(nums.vals[FILE_PART], pad.width(FILE_PART), out),
        }
        rest = &rest[p + 1..];
    }
    out.extend_from_slice(rest);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rt(s: &[u8]) -> (Vec<u8>, SubjectNums) {
        let mut tpl = Vec::new();
        let n = extract_subject_template(s, &mut tpl).expect("template");
        let mut out = Vec::new();
        render_subject(&tpl, &n, &mut out);
        assert_eq!(out, s, "{}", String::from_utf8_lossy(s));
        (tpl, n)
    }

    #[test]
    fn counters_come_out_and_go_back() {
        let (tpl, n) = rt(b"[14/40] - \"Release.Name.vol01.par2\" yEnc (01/85)");
        assert_eq!(tpl, b"[\x01] - \"Release.Name.vol01.par2\" yEnc (\x02)");
        assert_eq!(n.vals, [14, 40, 1, 85, 0]);
        let (_, n) = rt(b"Movie.2024.part001.rar (001/120)");
        assert_eq!(n.vals[PAR_NUM], 1);
        assert_eq!(n.pad.width(PAR_NUM), 3);
        let (tpl, n) = rt(b"x.r00.01.par2 [1/85] [2/3] (0/0)");
        assert_eq!(n.vals[FILE_PART], 1);
        assert!(tpl.starts_with(b"x.r00.\x03.par2 [\x01] [2/3] (\x02)"));
        rt(b"[0000001/9] (00000001/2)");
        rt(b"no counters at all");
        rt(b"");
        rt(b"[/] (1/) [12345678901/2]");
        let mut tpl = Vec::new();
        assert!(extract_subject_template(b"bad\x02byte", &mut tpl).is_none());
    }
}
