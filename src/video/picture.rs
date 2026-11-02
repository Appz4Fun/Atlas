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
