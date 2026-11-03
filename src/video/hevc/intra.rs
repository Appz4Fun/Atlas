//! Intra sample prediction (H.265 8.4.4.2): reference filtering, planar, DC and the 33 angles.
//!
//! References come in one line, `r`, of 4N + 1 samples: from the bottom of the left column (p[-1][2N-1]) up to
//! the corner (p[-1][-1], at 2N) and on along the top row to p[2N-1][-1].

pub const PLANAR: u8 = 0;
pub const DC: u8 = 1;

const ANGLE: [i32; 35] = [
    0, 0, 32, 26, 21, 17, 13, 9, 5, 2, 0, -2, -5, -9, -13, -17, -21, -26, -32, -26, -21, -17, -13, -9, -5, -2, 0, 2, 5,
    9, 13, 17, 21, 26, 32,
];

fn inv_angle(mode: u8) -> i32 {
    match mode {
        11 | 25 => -4096,
        12 | 24 => -1638,
        13 | 23 => -910,
        14 | 22 => -630,
        15 | 21 => -482,
        16 | 20 => -390,
        17 | 19 => -315,
        _ => -256,
    }
}

/// Fills in what isn't available (8.4.4.2.2). `have[i]` says whether `r[i]` is real.
pub fn substitute(r: &mut [i32], have: &[bool], bit_depth: u8) {
    match have.iter().position(|&h| h) {
        None => r.fill(1 << (bit_depth - 1)),
        Some(first) => {
            let v = r[first];
            r[..first].fill(v);
            for i in first + 1..r.len() {
                if !have[i] {
                    r[i] = r[i - 1];
                }
            }
        }
    }
}

/// Smooths the references when the mode and size call for it (8.4.4.2.3). Luma only, in 4:2:0.
pub fn filter(r: &mut [i32], n: usize, mode: u8, strong_enabled: bool, bit_depth: u8) {
    if mode == DC || n == 4 {
        return;
    }
    let dist = (i32::from(mode) - 26).abs().min((i32::from(mode) - 10).abs());
    let threshold = match n {
        8 => 7,
        16 => 1,
        _ => 0,
    };
    if dist <= threshold {
        return;
    }
    let corner = r[2 * n];
    let (bottom, right) = (r[0], r[4 * n]);
    let strong = strong_enabled
        && n == 32
        && (corner + right - 2 * r[2 * n + n]).abs() < 1 << (bit_depth - 5)
        && (corner + bottom - 2 * r[2 * n - n]).abs() < 1 << (bit_depth - 5);
    let mut f = [0i32; 129];
    let f = &mut f[..r.len()];
    f.copy_from_slice(r);
    if strong {
        // p[-1][y] = ((63 - y) corner + (y + 1) bottom + 32) >> 6, y = 0..62: index 2N - 1 - y
        for y in 0..63 {
            f[2 * n - 1 - y] = ((63 - y as i32) * corner + (y as i32 + 1) * bottom + 32) >> 6;
            f[2 * n + 1 + y] = ((63 - y as i32) * corner + (y as i32 + 1) * right + 32) >> 6;
        }
    } else {
        for i in 1..4 * n {
            f[i] = (r[i - 1] + 2 * r[i] + r[i + 1] + 2) >> 2;
        }
    }
    r.copy_from_slice(f);
}

/// Predicts an n x n block into `out` (row major) from the references.
pub fn predict(r: &[i32], n: usize, mode: u8, luma: bool, bit_depth: u8, out: &mut [i32]) {
    let c = 2 * n;
    let top = |x: isize| r[(c as isize + 1 + x) as usize];
    let left = |y: isize| r[(c as isize - 1 - y) as usize];
    let max = (1 << bit_depth) - 1;
    match mode {
        PLANAR => {
            let shift = n.trailing_zeros() + 1;
            let (tr, bl) = (top(n as isize), left(n as isize));
            for y in 0..n {
                for x in 0..n {
                    out[y * n + x] = ((n - 1 - x) as i32 * left(y as isize)
                        + (x + 1) as i32 * tr
                        + (n - 1 - y) as i32 * top(x as isize)
                        + (y + 1) as i32 * bl
                        + n as i32)
                        >> shift;
                }
            }
        }
        DC => {
            let k = n.trailing_zeros();
            let mut sum = n as i32;
            for i in 0..n as isize {
                sum += top(i) + left(i);
            }
            let dc = sum >> (k + 1);
            out[..n * n].fill(dc);
            if luma && n < 32 {
                out[0] = (left(0) + 2 * dc + top(0) + 2) >> 2;
                for (x, v) in out.iter_mut().enumerate().take(n).skip(1) {
                    *v = (top(x as isize) + 3 * dc + 2) >> 2;
                }
                for y in 1..n {
                    out[y * n] = (left(y as isize) + 3 * dc + 2) >> 2;
                }
            }
        }
        _ => {
            let angle = ANGLE[usize::from(mode)];
            let vertical = mode >= 18;
            // ref[i] for i in -N..=2N, kept at an offset of N
            let mut refs = [0i32; 97];
            let main = |i: isize| if vertical { top(i - 1) } else { left(i - 1) };
            let side = |i: isize| if vertical { left(i - 1) } else { top(i - 1) };
            for i in 0..=n as isize {
                refs[(i + n as isize) as usize] = main(i);
            }
            if angle < 0 {
                let lowest = (n as i32 * angle) >> 5;
                if lowest < -1 {
                    let inv = inv_angle(mode);
                    for i in lowest..=-1 {
                        refs[(i + n as i32) as usize] = side(((i * inv + 128) >> 8) as isize);
                    }
                }
            } else {
                for i in n as isize + 1..=2 * n as isize {
                    refs[(i + n as isize) as usize] = main(i);
                }
            }
            let at = |i: i32| refs[(i + n as i32) as usize];
            for j in 0..n {
                let pos = (j as i32 + 1) * angle;
                let (idx, fact) = (pos >> 5, pos & 31);
                for i in 0..n {
                    let base = i as i32 + idx;
                    let v = if fact != 0 {
                        ((32 - fact) * at(base + 1) + fact * at(base + 2) + 16) >> 5
                    } else {
                        at(base + 1)
                    };
                    // vertical modes step along x within a row y = j; horizontal ones down a column x = j
                    if vertical { out[j * n + i] = v } else { out[i * n + j] = v }
                }
            }
            if luma && n < 32 {
                if mode == 26 {
                    for y in 0..n {
                        out[y * n] = (top(0) + ((left(y as isize) - top(-1)) >> 1)).clamp(0, max);
                    }
                } else if mode == 10 {
                    for (x, v) in out.iter_mut().enumerate().take(n) {
                        *v = (left(0) + ((top(x as isize) - top(-1)) >> 1)).clamp(0, max);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_available_is_mid_grey() {
        let mut r = vec![0; 17];
        substitute(&mut r, &[false; 17], 10);
        assert!(r.iter().all(|&v| v == 512));
    }

    #[test]
    fn the_first_real_sample_fills_back_and_gaps_copy_forward() {
        let mut r = vec![0, 0, 7, 0, 9];
        substitute(&mut r, &[false, false, true, false, true], 8);
        assert_eq!(r, [7, 7, 7, 7, 9]);
    }

    #[test]
    fn flat_references_predict_flat_blocks_in_every_mode() {
        for mode in 0..35u8 {
            for n in [4, 8, 16, 32] {
                let r = vec![100; 4 * n + 1];
                let mut out = vec![0; n * n];
                predict(&r, n, mode, true, 8, &mut out);
                assert!(out.iter().all(|&v| v == 100), "mode {mode} size {n}");
            }
        }
    }

    #[test]
    fn vertical_copies_the_row_above() {
        let n = 4;
        let mut r = vec![0; 17];
        for x in 0..8 {
            r[9 + x] = 10 * x as i32;
        }
        r[8] = 0;
        let mut out = vec![0; 16];
        predict(&r, n, 26, false, 8, &mut out);
        assert_eq!(&out[12..16], &[0, 10, 20, 30]);
    }
}
