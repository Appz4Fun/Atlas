//! Intra prediction (H.264 8.3): 4x4 and 8x8 luma by direction, 16x16 luma and chroma by DC, plane and the like.

/// The samples around a block: the row above (2N: above and above right), the column left (N) and the corner.
/// Missing ones are None.
pub struct Edges {
    pub top: Option<[i32; 16]>,
    /// above right; when missing but `top` is there, the last of `top` repeated
    pub top_right: bool,
    pub left: Option<[i32; 16]>,
    pub corner: Option<i32>,
}

/// The 8x8 reference sample filter (8.3.2.2.1). `n` is 8.
pub fn filter_8x8(e: &Edges) -> Edges {
    let mut out = Edges { top: None, top_right: e.top_right, left: None, corner: None };
    if let Some(t) = e.top {
        let mut f = [0i32; 16];
        f[0] = match e.corner {
            Some(c) => (c + 2 * t[0] + t[1] + 2) >> 2,
            None => (3 * t[0] + t[1] + 2) >> 2,
        };
        for x in 1..15 {
            f[x] = (t[x - 1] + 2 * t[x] + t[x + 1] + 2) >> 2;
        }
        f[15] = (t[14] + 3 * t[15] + 2) >> 2;
        out.top = Some(f);
    }
    if let Some(c) = e.corner {
        out.corner = Some(match (e.top, e.left) {
            (Some(t), Some(l)) => (t[0] + 2 * c + l[0] + 2) >> 2,
            (Some(t), None) => (3 * c + t[0] + 2) >> 2,
            (None, Some(l)) => (3 * c + l[0] + 2) >> 2,
            (None, None) => c,
        });
    }
    if let Some(l) = e.left {
        let mut f = [0i32; 16];
        f[0] = match e.corner {
            Some(c) => (c + 2 * l[0] + l[1] + 2) >> 2,
            None => (3 * l[0] + l[1] + 2) >> 2,
        };
        for y in 1..7 {
            f[y] = (l[y - 1] + 2 * l[y] + l[y + 1] + 2) >> 2;
        }
        f[7] = (l[6] + 3 * l[7] + 2) >> 2;
        out.left = Some(f);
    }
    out
}

/// An `n`x`n` (4 or 8) prediction in `mode` 0 to 8, into `out` (row major, `n` wide). False: the mode needs
/// samples that aren't there.
pub fn directional(n: usize, mode: u8, e: &Edges, out: &mut [i32]) -> bool {
    let (top, left, corner) = (e.top.unwrap_or([0; 16]), e.left.unwrap_or([0; 16]), e.corner.unwrap_or(0));
    // p[x, -1] for x in -1..2n, p[-1, y] for y in -1..n
    let t = |x: isize| if x < 0 { corner } else { top[x as usize] };
    let l = |y: isize| if y < 0 { corner } else { left[y as usize] };
    let ni = n as isize;
    let ok = match mode {
        0 | 3 | 7 => e.top.is_some(),
        1 | 8 => e.left.is_some(),
        2 => true,
        _ => e.top.is_some() && e.left.is_some() && e.corner.is_some(),
    };
    if !ok {
        return false;
    }
    let dc = {
        let (st, sl) = ((0..ni).map(t).sum::<i32>(), (0..ni).map(l).sum::<i32>());
        let shift = if n == 4 { 2 } else { 3 };
        match (e.top.is_some(), e.left.is_some()) {
            (true, true) => (st + sl + n as i32) >> (shift + 1),
            (true, false) => (st + (n as i32 >> 1)) >> shift,
            (false, true) => (sl + (n as i32 >> 1)) >> shift,
            (false, false) => 128,
        }
    };
    for y in 0..ni {
        for x in 0..ni {
            let v = match mode {
                0 => t(x),
                1 => l(y),
                2 => dc,
                3 => {
                    if x == ni - 1 && y == ni - 1 {
                        (t(x + y) + 3 * t(x + y + 1) + 2) >> 2
                    } else {
                        (t(x + y) + 2 * t(x + y + 1) + t(x + y + 2) + 2) >> 2
                    }
                }
                4 => {
                    if x > y {
                        (t(x - y - 2) + 2 * t(x - y - 1) + t(x - y) + 2) >> 2
                    } else if x < y {
                        (l(y - x - 2) + 2 * l(y - x - 1) + l(y - x) + 2) >> 2
                    } else {
                        (t(0) + 2 * corner + l(0) + 2) >> 2
                    }
                }
                5 => {
                    let z = 2 * x - y;
                    if z >= 0 && z % 2 == 0 {
                        (t(x - (y >> 1) - 1) + t(x - (y >> 1)) + 1) >> 1
                    } else if z > 0 {
                        (t(x - (y >> 1) - 2) + 2 * t(x - (y >> 1) - 1) + t(x - (y >> 1)) + 2) >> 2
                    } else if z == -1 {
                        (l(0) + 2 * corner + t(0) + 2) >> 2
                    } else {
                        (l(y - 2 * x - 1) + 2 * l(y - 2 * x - 2) + l(y - 2 * x - 3) + 2) >> 2
                    }
                }
                6 => {
                    let z = 2 * y - x;
                    if z >= 0 && z % 2 == 0 {
                        (l(y - (x >> 1) - 1) + l(y - (x >> 1)) + 1) >> 1
                    } else if z > 0 {
                        (l(y - (x >> 1) - 2) + 2 * l(y - (x >> 1) - 1) + l(y - (x >> 1)) + 2) >> 2
                    } else if z == -1 {
                        (l(0) + 2 * corner + t(0) + 2) >> 2
                    } else {
                        (t(x - 2 * y - 1) + 2 * t(x - 2 * y - 2) + t(x - 2 * y - 3) + 2) >> 2
                    }
                }
                7 => {
                    let i = x + (y >> 1);
                    if y % 2 == 0 { (t(i) + t(i + 1) + 1) >> 1 } else { (t(i) + 2 * t(i + 1) + t(i + 2) + 2) >> 2 }
                }
                _ => {
                    let z = x + 2 * y;
                    let i = y + (x >> 1);
                    let limit = 2 * ni - 3;
                    if z > limit {
                        l(ni - 1)
                    } else if z == limit {
                        (l(ni - 2) + 3 * l(ni - 1) + 2) >> 2
                    } else if z % 2 == 0 {
                        (l(i) + l(i + 1) + 1) >> 1
                    } else {
                        (l(i) + 2 * l(i + 1) + l(i + 2) + 2) >> 2
                    }
                }
            };
            out[(y * ni + x) as usize] = v;
        }
    }
    true
}

/// A 16x16 luma (`n` 16) or 8x8 chroma (`n` 8) prediction by mode: 0 vertical, 1 horizontal, 2 DC, 3 plane. Chroma's
/// DC goes by 4x4 block, and its modes are numbered differently: map them before calling. False: samples missing.
pub fn block_mode(n: usize, mode: u8, e: &Edges, out: &mut [i32]) -> bool {
    let (top, left) = (e.top.unwrap_or([0; 16]), e.left.unwrap_or([0; 16]));
    match mode {
        0 => {
            if e.top.is_none() {
                return false;
            }
            for y in 0..n {
                out[y * n..y * n + n].copy_from_slice(&top[..n]);
            }
        }
        1 => {
            if e.left.is_none() {
                return false;
            }
            for y in 0..n {
                out[y * n..y * n + n].fill(left[y]);
            }
        }
        2 if n == 16 => {
            let (st, sl): (i32, i32) = (top.iter().sum(), left.iter().sum());
            let v = match (e.top.is_some(), e.left.is_some()) {
                (true, true) => (st + sl + 16) >> 5,
                (true, false) => (st + 8) >> 4,
                (false, true) => (sl + 8) >> 4,
                (false, false) => 128,
            };
            out[..256].fill(v);
        }
        2 => {
            // chroma: each 4x4 by itself
            for blk in 0..4 {
                let (xo, yo) = (blk % 2 * 4, blk / 2 * 4);
                let st: i32 = top[xo..xo + 4].iter().sum();
                let sl: i32 = left[yo..yo + 4].iter().sum();
                let (has_t, has_l) = (e.top.is_some(), e.left.is_some());
                let v = if xo > 0 && yo == 0 {
                    if has_t {
                        (st + 2) >> 2
                    } else if has_l {
                        (sl + 2) >> 2
                    } else {
                        128
                    }
                } else if xo == 0 && yo > 0 {
                    if has_l {
                        (sl + 2) >> 2
                    } else if has_t {
                        (st + 2) >> 2
                    } else {
                        128
                    }
                } else {
                    match (has_t, has_l) {
                        (true, true) => (st + sl + 4) >> 3,
                        (true, false) => (st + 2) >> 2,
                        (false, true) => (sl + 2) >> 2,
                        (false, false) => 128,
                    }
                };
                for y in yo..yo + 4 {
                    out[y * n + xo..y * n + xo + 4].fill(v);
                }
            }
        }
        _ => {
            let (Some(_), Some(_), Some(c)) = (e.top, e.left, e.corner) else { return false };
            let half = n as i32 / 2;
            let t = |x: i32| if x < 0 { c } else { top[x as usize] };
            let l = |y: i32| if y < 0 { c } else { left[y as usize] };
            let (mut h, mut v) = (0, 0);
            for i in 0..half {
                h += (i + 1) * (t(half + i) - t(half - 2 - i));
                v += (i + 1) * (l(half + i) - l(half - 2 - i));
            }
            let (b, cc) =
                if n == 16 { ((5 * h + 32) >> 6, (5 * v + 32) >> 6) } else { ((34 * h + 32) >> 6, (34 * v + 32) >> 6) };
            let a = 16 * (l(n as i32 - 1) + t(n as i32 - 1));
            for y in 0..n as i32 {
                for x in 0..n as i32 {
                    out[(y * n as i32 + x) as usize] =
                        ((a + b * (x - half + 1) + cc * (y - half + 1) + 16) >> 5).clamp(0, 255);
                }
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dc_with_nothing_around_is_mid_grey() {
        let e = Edges { top: None, top_right: false, left: None, corner: None };
        let mut out = [0; 16];
        assert!(directional(4, 2, &e, &mut out));
        assert_eq!(out, [128; 16]);
    }

    #[test]
    fn modes_needing_missing_samples_say_so() {
        let e = Edges { top: None, top_right: false, left: Some([1; 16]), corner: None };
        let mut out = [0; 64];
        assert!(!directional(8, 0, &e, &mut out));
        assert!(directional(8, 1, &e, &mut out));
        assert!(!block_mode(16, 3, &e, &mut out[..]));
    }
}
