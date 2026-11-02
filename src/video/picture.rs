//! A decoded picture: Y, Cb and Cr planes, 4:2:0, as stored samples of any bit depth.

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plane {
    pub width: usize,
    pub height: usize,
    pub data: Vec<u16>,
}

impl Plane {
    pub fn new(width: usize, height: usize, fill: u16) -> Plane {
        Plane { width, height, data: vec![fill; width * height] }
    }

    #[inline]
    pub fn get(&self, x: usize, y: usize) -> u16 {
        self.data[y * self.width + x]
    }

    #[inline]
    pub fn set(&mut self, x: usize, y: usize, v: u16) {
        self.data[y * self.width + x] = v;
    }
}

/// What the stream says is shown of a picture coded bigger: its conformance window, in luma samples.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Crop {
    pub left: usize,
    pub right: usize,
    pub top: usize,
    pub bottom: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Picture {
    /// Y, Cb, Cr
    pub planes: [Plane; 3],
    pub bit_depth: u8,
    pub crop: Crop,
    /// samples use the whole range, not video's 16-235 (at 8 bits)
    pub full_range: bool,
    /// the stream's matrix_coefficients (H.273), when it says
    pub matrix: Option<u8>,
}

/// Kr and Kb of a matrix: BT.601, BT.709 or BT.2020. Unsaid, HD and up is taken as BT.709, as players do.
fn weights(matrix: Option<u8>, height: usize) -> (f32, f32) {
    const BT601: (f32, f32) = (0.299, 0.114);
    const BT709: (f32, f32) = (0.2126, 0.0722);
    match matrix {
        Some(1) => BT709,
        Some(4) => (0.30, 0.11),
        Some(5 | 6) => BT601,
        Some(7) => (0.212, 0.087),
        Some(9 | 10) => (0.2627, 0.0593),
        _ if height >= 600 => BT709,
        _ => BT601,
    }
}

/// Full range Y, Cb and Cr in `from`'s weights to BT.601's, which is what a JPEG (JFIF) is read as.
fn to_bt601(y: &mut Plane8, cb: &mut Plane8, cr: &mut Plane8, (kr, kb): (f32, f32)) {
    let (to_kr, to_kb) = (0.299f32, 0.114f32);
    let convert = |yv: f32, u: f32, v: f32| {
        let (r, b) = (yv + 2.0 * (1.0 - kr) * v, yv + 2.0 * (1.0 - kb) * u);
        let g = (yv - kr * r - kb * b) / (1.0 - kr - kb);
        let ny = to_kr * r + (1.0 - to_kr - to_kb) * g + to_kb * b;
        (ny, (b - ny) / (2.0 * (1.0 - to_kb)), (r - ny) / (2.0 * (1.0 - to_kr)))
    };
    let byte = |v: f32| v.round().clamp(0.0, 255.0) as u8;
    let cw = cb.width;
    // each output from the samples as they were
    let (old_y, old_cb, old_cr) = (y.data.clone(), cb.data.clone(), cr.data.clone());
    for row in 0..y.height {
        for col in 0..y.width {
            let c = (row / 2).min(cb.height.saturating_sub(1)) * cw + (col / 2).min(cw.saturating_sub(1));
            let (Some(&u), Some(&v)) = (old_cb.get(c), old_cr.get(c)) else { continue };
            let i = row * y.width + col;
            let (ny, _, _) = convert(f32::from(old_y[i]), f32::from(u) - 128.0, f32::from(v) - 128.0);
            y.data[i] = byte(ny);
        }
    }
    for row in 0..cb.height {
        for col in 0..cw {
            let mut sum = 0u32;
            for (dy, dx) in [(0, 0), (0, 1), (1, 0), (1, 1)] {
                let (yr, yc) = ((row * 2 + dy).min(y.height - 1), (col * 2 + dx).min(y.width - 1));
                sum += u32::from(old_y[yr * y.width + yc]);
            }
            let c = row * cw + col;
            let (_, u, v) = convert(sum as f32 / 4.0, f32::from(old_cb[c]) - 128.0, f32::from(old_cr[c]) - 128.0);
            cb.data[c] = byte(u + 128.0);
            cr.data[c] = byte(v + 128.0);
        }
    }
}

impl Picture {
    /// The luma that's shown, scaled to 8 bits.
    pub fn luma8(&self) -> (usize, usize, Vec<u8>) {
        let y = &self.planes[0];
        let (w, h) = (
            y.width.saturating_sub(self.crop.left + self.crop.right),
            y.height.saturating_sub(self.crop.top + self.crop.bottom),
        );
        let shift = self.bit_depth.saturating_sub(8);
        let mut out = Vec::with_capacity(w * h);
        for row in self.crop.top..self.crop.top + h {
            for col in self.crop.left..self.crop.left + w {
                out.push((y.get(col, row) >> shift).min(255) as u8);
            }
        }
        (w, h, out)
    }
}

/// One 8 bit plane, `width` x `height`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plane8 {
    pub width: usize,
    pub height: usize,
    pub data: Vec<u8>,
}

/// The mean of the samples each output sample covers: a box filter, which is all shrinking needs.
#[allow(clippy::too_many_arguments)]
fn shrink(src: &Plane, x0: usize, y0: usize, sw: usize, sh: usize, dw: usize, dh: usize, shift: u8) -> Plane8 {
    let mut data = Vec::with_capacity(dw * dh);
    for oy in 0..dh {
        let (ya, yb) = (y0 + oy * sh / dh, y0 + ((oy + 1) * sh / dh).max(oy * sh / dh + 1));
        for ox in 0..dw {
            let (xa, xb) = (x0 + ox * sw / dw, x0 + ((ox + 1) * sw / dw).max(ox * sw / dw + 1));
            let mut sum = 0u64;
            for y in ya..yb {
                for x in xa..xb {
                    sum += u64::from(src.get(x, y));
                }
            }
            let n = ((yb - ya) * (xb - xa)) as u64;
            data.push((((sum + n / 2) / n) >> shift).min(255) as u8);
        }
    }
    Plane8 { width: dw, height: dh, data }
}

impl Picture {
    /// The shown picture as full range BT.601 Y, Cb and Cr at 8 bits, as a JPEG wants them, shrunk to at most
    /// `max_width` wide. Its shape is `display` (width to height) when that's given and near the samples' shape,
    /// for anamorphic video, else the samples'. Wider samples (10 bit) lose their low bits: a still, not a grade.
    pub fn to_8bit(&self, max_width: usize, display: Option<(u64, u64)>) -> [Plane8; 3] {
        let (w, h) = (
            self.planes[0].width.saturating_sub(self.crop.left + self.crop.right).max(2),
            self.planes[0].height.saturating_sub(self.crop.top + self.crop.bottom).max(2),
        );
        let dw = w.min(max_width.max(2)) & !1;
        // a display shape far off the coded one is a mistake: anamorphic video is squeezed 2x at most
        let shape = display.filter(|&(dw, dh)| {
            let (shown, coded) = (dw as f64 / dh as f64, w as f64 / h as f64);
            (coded / 4.0..=coded * 4.0).contains(&shown)
        });
        let dh = match shape {
            Some((sw, sh)) => (dw as u128 * u128::from(sh) / u128::from(sw)) as usize,
            None => h * dw / w,
        };
        // never taller than the samples, nor than twice the width it's kept at: what's allocated stays within what
        // the decoder already allowed
        let dh = dh.min(h).min(max_width.max(2) * 2).max(2) & !1;
        let shift = self.bit_depth.saturating_sub(8);
        let mut y = shrink(&self.planes[0], self.crop.left, self.crop.top, w, h, dw, dh, shift);
        let chroma = |p: &Plane| shrink(p, self.crop.left / 2, self.crop.top / 2, w / 2, h / 2, dw / 2, dh / 2, shift);
        let (mut cb, mut cr) = (chroma(&self.planes[1]), chroma(&self.planes[2]));
        if !self.full_range {
            // a JPEG's samples are full range
            for v in &mut y.data {
                *v = ((i32::from(*v) - 16) * 255 / 219).clamp(0, 255) as u8;
            }
            for v in cb.data.iter_mut().chain(cr.data.iter_mut()) {
                *v = ((i32::from(*v) - 128) * 255 / 224 + 128).clamp(0, 255) as u8;
            }
        }
        let from = weights(self.matrix, h);
        if from != (0.299, 0.114) {
            to_bt601(&mut y, &mut cb, &mut cr, from);
        }
        [y, cb, cr]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(w: usize, h: usize, [y, cb, cr]: [u16; 3], matrix: Option<u8>) -> Picture {
        let planes = [Plane::new(w, h, y), Plane::new(w / 2, h / 2, cb), Plane::new(w / 2, h / 2, cr)];
        Picture { planes, bit_depth: 8, crop: Crop::default(), full_range: true, matrix }
    }

    #[test]
    fn anamorphic_video_is_shown_at_its_display_shape() {
        let p = flat(1440, 1080, [128; 3], Some(1));
        let [y, cb, _] = p.to_8bit(960, Some((1920, 1080)));
        assert_eq!((y.width, y.height, cb.width, cb.height), (960, 540, 480, 270));
        let [y, ..] = p.to_8bit(960, None);
        assert_eq!((y.width, y.height), (960, 720));
        // nonsense is ignored
        let [y, ..] = p.to_8bit(960, Some((100_000, 1)));
        assert_eq!((y.width, y.height), (960, 720));
        // a shape that would stretch it tall is held to the samples' height
        let tall = flat(960, 9824, [128; 3], None);
        let [y, ..] = tall.to_8bit(960, Some((240, 9824)));
        assert!(y.height <= 1920, "{}", y.height);
    }

    #[test]
    fn bt709_colours_are_turned_into_jpegs_bt601() {
        // pure red in BT.709, full range: Y 54, Cb 99, Cr 255
        let p = flat(16, 16, [54, 99, 255], Some(1));
        let [y, cb, cr] = p.to_8bit(16, None);
        // pure red in BT.601: Y 76, Cb 85, Cr 255
        assert!((i32::from(y.data[0]) - 76).abs() <= 1, "{}", y.data[0]);
        assert!((i32::from(cb.data[0]) - 85).abs() <= 1, "{}", cb.data[0]);
        assert!(cr.data[0] >= 254, "{}", cr.data[0]);
        // grey stays grey, and BT.601 is left alone
        let [y, cb, cr] = flat(16, 16, [100, 128, 128], Some(1)).to_8bit(16, None);
        assert_eq!((y.data[5], cb.data[5], cr.data[5]), (100, 128, 128));
        let [y, cb, _] = flat(16, 16, [54, 99, 255], Some(6)).to_8bit(16, None);
        assert_eq!((y.data[0], cb.data[0]), (54, 99));
    }
}
