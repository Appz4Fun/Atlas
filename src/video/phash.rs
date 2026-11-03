//! A 64-bit perceptual hash of a still, for "the same film" across releases: two encodes of one source,
//! a remux and a re-encode, a different cut. Close hashes are only ever a "same film" link, never a twin.
//!
//! The recipe, from the research: crop rows and columns whose mean luma is under 20 (letterbox and pillarbox
//! bars), shrink to 32x32, histogram-equalise (so a brighter or darker grade still matches), take the 2-D DCT, and
//! set one bit for each of the 8x8 lowest frequencies that is above their median.

use super::picture::Plane8;

/// Rows and columns darker than this on average are bars.
const BAR_LUMA: f64 = 20.0;
const N: usize = 32;
/// Two stills this many bits apart or fewer show the same picture.
pub const MATCH_BITS: u32 = 10;

/// None for a picture with nothing to go on: all bars, or one flat colour.
pub fn phash(y: &Plane8) -> Option<u64> {
    let (w, h) = (y.width, y.height);
    if w < N || h < N || y.data.len() < w * h {
        return None;
    }
    let row_mean = |r: usize| y.data[r * w..(r + 1) * w].iter().map(|&v| f64::from(v)).sum::<f64>() / w as f64;
    let top = (0..h).find(|&r| row_mean(r) >= BAR_LUMA)?;
    let bottom = (0..h).rev().find(|&r| row_mean(r) >= BAR_LUMA)? + 1;
    let col_mean = |c: usize| (top..bottom).map(|r| f64::from(y.data[r * w + c])).sum::<f64>() / (bottom - top) as f64;
    let left = (0..w).find(|&c| col_mean(c) >= BAR_LUMA)?;
    let right = (0..w).rev().find(|&c| col_mean(c) >= BAR_LUMA)? + 1;
    let (cw, ch) = (right - left, bottom - top);
    if cw < N || ch < N {
        return None;
    }

    // a box filter down to 32x32
    let mut small = [0f64; N * N];
    for oy in 0..N {
        let (ya, yb) = (top + oy * ch / N, top + (oy + 1) * ch / N);
        for ox in 0..N {
            let (xa, xb) = (left + ox * cw / N, left + (ox + 1) * cw / N);
            let mut sum = 0u64;
            for r in ya..yb {
                sum += y.data[r * w + xa..r * w + xb].iter().map(|&v| u64::from(v)).sum::<u64>();
            }
            small[oy * N + ox] = sum as f64 / ((yb - ya) * (xb - xa)) as f64;
        }
    }
    let small = equalise(&small)?;

    // the 2-D DCT-II, separable, of only the 8x8 lowest frequencies
    let basis: Vec<f64> = (0..8 * N)
        .map(|i| {
            let (k, x) = (i / N, i % N);
            (std::f64::consts::PI / N as f64 * (x as f64 + 0.5) * k as f64).cos()
        })
        .collect();
    let mut rows = [0f64; 8 * N]; // rows[k * N + y]: frequency k across row y
    for yy in 0..N {
        for k in 0..8 {
            rows[k * N + yy] = (0..N).map(|x| small[yy * N + x] * basis[k * N + x]).sum();
        }
    }
    let mut coefficients = [0f64; 64];
    for v in 0..8 {
        for u in 0..8 {
            coefficients[v * 8 + u] = (0..N).map(|yy| rows[u * N + yy] * basis[v * N + yy]).sum();
        }
    }
    // the DC term is the mean brightness, which equalising fixed: it says nothing
    let mut ac: Vec<f64> = coefficients[1..].to_vec();
    ac.sort_by(f64::total_cmp);
    let median = (ac[31] + ac[32]) / 2.0;
    Some(coefficients.iter().enumerate().fold(0u64, |hash, (i, &c)| if c > median { hash | 1 << i } else { hash }))
}

/// Spread the 1,024 values over 0..=255 by rank. None when they're all one value.
fn equalise(values: &[f64; N * N]) -> Option<[f64; N * N]> {
    let mut order: Vec<usize> = (0..values.len()).collect();
    order.sort_by(|&a, &b| values[a].total_cmp(&values[b]));
    if values[order[0]] == values[order[order.len() - 1]] {
        return None;
    }
    let mut out = [0f64; N * N];
    let mut i = 0;
    while i < order.len() {
        // equal values share one rank: the last of them
        let mut j = i;
        while j + 1 < order.len() && values[order[j + 1]] == values[order[i]] {
            j += 1;
        }
        let level = 255.0 * j as f64 / (order.len() - 1) as f64;
        for &k in &order[i..=j] {
            out[k] = level;
        }
        i = j + 1;
    }
    Some(out)
}

pub fn distance(a: u64, b: u64) -> u32 {
    (a ^ b).count_ones()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picture(w: usize, h: usize, f: impl Fn(usize, usize) -> u8) -> Plane8 {
        Plane8 { width: w, height: h, data: (0..w * h).map(|i| f(i % w, i / w)).collect() }
    }

    /// Something with shapes in it: a few soft blobs.
    fn scene(x: f64, y: f64) -> f64 {
        let blob = |cx: f64, cy: f64, r: f64| (-((x - cx).powi(2) + (y - cy).powi(2)) / (r * r)).exp();
        60.0 + 150.0 * blob(0.3, 0.4, 0.15) + 90.0 * blob(0.7, 0.6, 0.2) - 40.0 * blob(0.5, 0.2, 0.1)
    }

    fn render(w: usize, h: usize, gain: f64, bars: usize) -> Plane8 {
        picture(w, h, |x, y| {
            if y < bars || y >= h - bars {
                return 16;
            }
            let (u, v) = (x as f64 / w as f64, (y - bars) as f64 / (h - 2 * bars) as f64);
            (scene(u, v) * gain).clamp(16.0, 235.0) as u8
        })
    }

    #[test]
    fn the_same_picture_matches_through_bars_size_and_grade() {
        let plain = phash(&render(960, 400, 1.0, 0)).unwrap();
        // letterboxed into 16:9, smaller, and graded darker
        let other = phash(&render(640, 360, 0.8, 47)).unwrap();
        assert!(distance(plain, other) <= MATCH_BITS, "{}", distance(plain, other));
    }

    #[test]
    fn a_different_picture_doesnt_match() {
        let a = phash(&render(960, 400, 1.0, 0)).unwrap();
        let b = phash(&picture(960, 400, |x, y| ((x / 40 + y / 40) % 2 * 150 + 40) as u8)).unwrap();
        assert!(distance(a, b) > MATCH_BITS, "{}", distance(a, b));
    }

    #[test]
    fn black_or_flat_pictures_have_no_hash() {
        assert_eq!(phash(&picture(960, 400, |_, _| 16)), None);
        assert_eq!(phash(&picture(960, 400, |_, _| 120)), None);
        assert_eq!(phash(&picture(20, 20, |x, _| x as u8 * 10)), None);
    }
}
