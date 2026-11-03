//! Scaling and the inverse transforms (H.265 8.6.2 to 8.6.4).

/// cos(πa/64) as the standard's integer matrix has it, for a = 0..=32
const V: [i32; 33] = [
    64, 90, 90, 90, 89, 88, 87, 85, 83, 82, 80, 78, 75, 73, 70, 67, 64, 61, 57, 54, 50, 46, 43, 38, 36, 31, 25, 22, 18,
    13, 9, 4, 0,
];

/// The 32 point DCT matrix: row k, column n.
pub fn dct32() -> [[i32; 32]; 32] {
    let mut m = [[0; 32]; 32];
    for (k, row) in m.iter_mut().enumerate() {
        for (n, cell) in row.iter_mut().enumerate() {
            if k == 0 {
                *cell = 64;
                continue;
            }
            let a = ((2 * n + 1) * k) % 128;
            *cell = match a {
                0..=32 => V[a],
                33..=64 => -V[64 - a],
                65..=96 => -V[a - 64],
                _ => V[128 - a],
            };
        }
    }
    m
}

const DST4: [[i32; 4]; 4] = [[29, 55, 74, 84], [74, 74, 0, -74], [84, -29, -74, 55], [55, -84, 74, -29]];

pub struct Transforms {
    m32: [[i32; 32]; 32],
}

impl Default for Transforms {
    fn default() -> Transforms {
        Transforms { m32: dct32() }
    }
}

impl Transforms {
    #[inline]
    fn coef(&self, size: usize, dst: bool, k: usize, n: usize) -> i32 {
        if dst { DST4[k][n] } else { self.m32[k * (32 / size)][n] }
    }

    /// `block` (row major, y * size + x) from scaled coefficients to residuals, in place.
    pub fn inverse(&self, block: &mut [i32], size: usize, dst: bool, bit_depth: u8) {
        let mut tmp = [0i32; 32 * 32];
        // columns first: e[x][y] = Σk T[k][y] d[x][k]
        for x in 0..size {
            // most coefficients are zero: only the rows that have any count
            let last = (0..size).rev().find(|&k| block[k * size + x] != 0);
            let Some(last) = last else { continue };
            for y in 0..size {
                let mut sum = 0i64;
                for k in 0..=last {
                    let d = block[k * size + x];
                    if d != 0 {
                        sum += i64::from(self.coef(size, dst, k, y)) * i64::from(d);
                    }
                }
                tmp[y * size + x] = ((sum + 64) >> 7).clamp(-32768, 32767) as i32;
            }
        }
        let shift = 20 - u32::from(bit_depth);
        let round = 1i64 << (shift - 1);
        for y in 0..size {
            let row = &tmp[y * size..y * size + size];
            let last = (0..size).rev().find(|&k| row[k] != 0);
            for x in 0..size {
                let mut sum = 0i64;
                if let Some(last) = last {
                    for (k, &g) in row.iter().enumerate().take(last + 1) {
                        if g != 0 {
                            sum += i64::from(self.coef(size, dst, k, x)) * i64::from(g);
                        }
                    }
                }
                block[y * size + x] = ((sum + round) >> shift) as i32;
            }
        }
    }
}

/// Transform skip: the residual is the coefficient, scaled.
pub fn transform_skip(block: &mut [i32], bit_depth: u8) {
    let shift = 20 - u32::from(bit_depth);
    let round = 1i32 << (shift - 1);
    for v in block.iter_mut() {
        *v = ((*v << 7) + round) >> shift;
    }
}

pub const LEVEL_SCALE: [i32; 6] = [40, 45, 51, 57, 64, 72];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_matrix_has_the_standards_rows() {
        let m = dct32();
        assert_eq!(&m[1][..16], &[90, 90, 88, 85, 82, 78, 73, 67, 61, 54, 46, 38, 31, 22, 13, 4]);
        assert_eq!(&m[2][..8], &[90, 87, 80, 70, 57, 43, 25, 9]);
        assert_eq!(&m[8][..4], &[83, 36, -36, -83]);
        assert_eq!(&m[16][..4], &[64, -64, -64, 64]);
        assert_eq!(m[31][31], -4);
    }

    #[test]
    fn a_dc_coefficient_spreads_evenly() {
        let t = Transforms::default();
        let mut b = vec![0; 16];
        b[0] = 64;
        t.inverse(&mut b, 4, false, 8);
        assert!(b.iter().all(|v| *v == b[0]), "{b:?}");
    }
}
