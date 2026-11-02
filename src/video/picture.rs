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
    /// The shown picture as 8 bit Y, Cb and Cr, shrunk to at most `max_width` wide, keeping its shape. Wider
    /// samples (10 bit) lose their low bits: a still, not a grade.
    pub fn to_8bit(&self, max_width: usize) -> [Plane8; 3] {
        let (w, h) = (
            self.planes[0].width.saturating_sub(self.crop.left + self.crop.right).max(2),
            self.planes[0].height.saturating_sub(self.crop.top + self.crop.bottom).max(2),
        );
        let dw = w.min(max_width.max(2)) & !1;
        let dh = ((h * dw / w).max(2)) & !1;
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
        [y, cb, cr]
    }
}
