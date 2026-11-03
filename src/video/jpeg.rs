//! A baseline JPEG writer (ITU T.81): 4:2:0 YCbCr straight from a decoded picture, with the example tables of
//! Annex K. Small and slow, which is plenty for a few stills a release.

/// Annex K.1's quantization tables, in natural order.
const LUMA_Q: [u16; 64] = [
    16, 11, 10, 16, 24, 40, 51, 61, 12, 12, 14, 19, 26, 58, 60, 55, 14, 13, 16, 24, 40, 57, 69, 56, 14, 17, 22, 29, 51,
    87, 80, 62, 18, 22, 37, 56, 68, 109, 103, 77, 24, 35, 55, 64, 81, 104, 113, 92, 49, 64, 78, 87, 103, 121, 120, 101,
    72, 92, 95, 98, 112, 100, 103, 99,
];
const CHROMA_Q: [u16; 64] = [
    17, 18, 24, 47, 99, 99, 99, 99, 18, 21, 26, 66, 99, 99, 99, 99, 24, 26, 56, 99, 99, 99, 99, 99, 47, 66, 99, 99, 99,
    99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99,
    99, 99, 99, 99, 99, 99,
];

const ZIGZAG: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20, 13, 6, 7, 14, 21,
    28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59, 52, 45, 38, 31, 39, 46, 53, 60, 61, 54,
    47, 55, 62, 63,
];

/// Annex K.3's Huffman tables: code counts by length, then the values.
const DC_LUMA: ([u8; 16], &[u8]) =
    ([0, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0], &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11]);
const DC_CHROMA: ([u8; 16], &[u8]) =
    ([0, 3, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0], &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11]);
const AC_LUMA: ([u8; 16], &[u8]) = (
    [0, 2, 1, 3, 3, 2, 4, 3, 5, 5, 4, 4, 0, 0, 1, 0x7d],
    &[
        0x01, 0x02, 0x03, 0x00, 0x04, 0x11, 0x05, 0x12, 0x21, 0x31, 0x41, 0x06, 0x13, 0x51, 0x61, 0x07, 0x22, 0x71,
        0x14, 0x32, 0x81, 0x91, 0xa1, 0x08, 0x23, 0x42, 0xb1, 0xc1, 0x15, 0x52, 0xd1, 0xf0, 0x24, 0x33, 0x62, 0x72,
        0x82, 0x09, 0x0a, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x34, 0x35, 0x36, 0x37,
        0x38, 0x39, 0x3a, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49, 0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59,
        0x5a, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6a, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a, 0x83,
        0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9a, 0xa2, 0xa3,
        0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3,
        0xc4, 0xc5, 0xc6, 0xc7, 0xc8, 0xc9, 0xca, 0xd2, 0xd3, 0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda, 0xe1, 0xe2,
        0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8, 0xe9, 0xea, 0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa,
    ],
);
const AC_CHROMA: ([u8; 16], &[u8]) = (
    [0, 2, 1, 2, 4, 4, 3, 4, 7, 5, 4, 4, 0, 1, 2, 0x77],
    &[
        0x00, 0x01, 0x02, 0x03, 0x11, 0x04, 0x05, 0x21, 0x31, 0x06, 0x12, 0x41, 0x51, 0x07, 0x61, 0x71, 0x13, 0x22,
        0x32, 0x81, 0x08, 0x14, 0x42, 0x91, 0xa1, 0xb1, 0xc1, 0x09, 0x23, 0x33, 0x52, 0xf0, 0x15, 0x62, 0x72, 0xd1,
        0x0a, 0x16, 0x24, 0x34, 0xe1, 0x25, 0xf1, 0x17, 0x18, 0x19, 0x1a, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x35, 0x36,
        0x37, 0x38, 0x39, 0x3a, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49, 0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58,
        0x59, 0x5a, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6a, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a,
        0x82, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9a,
        0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xba,
        0xc2, 0xc3, 0xc4, 0xc5, 0xc6, 0xc7, 0xc8, 0xc9, 0xca, 0xd2, 0xd3, 0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda,
        0xe2, 0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8, 0xe9, 0xea, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa,
    ],
);

/// Codes and lengths for each value of a table (Annex C).
struct Huffman {
    code: [u16; 256],
    len: [u8; 256],
}

impl Huffman {
    fn new((counts, values): ([u8; 16], &[u8])) -> Huffman {
        let mut h = Huffman { code: [0; 256], len: [0; 256] };
        let (mut code, mut k) = (0u16, 0);
        for (bits, &count) in counts.iter().enumerate() {
            for _ in 0..count {
                h.code[usize::from(values[k])] = code;
                h.len[usize::from(values[k])] = bits as u8 + 1;
                code += 1;
                k += 1;
            }
            code <<= 1;
        }
        h
    }
}

struct Bits {
    out: Vec<u8>,
    acc: u32,
    n: u32,
}

impl Bits {
    fn put(&mut self, value: u32, len: u32) {
        if len == 0 {
            return;
        }
        self.acc = self.acc << len | (value & ((1 << len) - 1));
        self.n += len;
        while self.n >= 8 {
            let byte = (self.acc >> (self.n - 8)) as u8;
            self.out.push(byte);
            // a 0xFF in the data is followed by a stuffed zero
            if byte == 0xff {
                self.out.push(0);
            }
            self.n -= 8;
        }
        self.acc &= (1 << self.n) - 1;
    }

    fn flush(&mut self) {
        let pad = (8 - self.n % 8) % 8;
        self.put((1 << pad) - 1, pad);
    }
}

fn scaled(table: &[u16; 64], quality: u8) -> [u8; 64] {
    let q = u32::from(quality.clamp(1, 100));
    let scale = if q < 50 { 5000 / q } else { 200 - 2 * q };
    let mut out = [0u8; 64];
    for (o, &t) in out.iter_mut().zip(table) {
        *o = ((u32::from(t) * scale + 50) / 100).clamp(1, 255) as u8;
    }
    out
}

/// The forward DCT of an 8x8 block of samples less 128, straight from its definition.
fn fdct(block: &[f32; 64], cos: &[[f32; 8]; 8]) -> [f32; 64] {
    let mut tmp = [0f32; 64];
    for y in 0..8 {
        for u in 0..8 {
            tmp[y * 8 + u] = (0..8).map(|x| block[y * 8 + x] * cos[u][x]).sum();
        }
    }
    let mut out = [0f32; 64];
    for v in 0..8 {
        for u in 0..8 {
            out[v * 8 + u] = (0..8).map(|y| tmp[y * 8 + u] * cos[v][y]).sum();
        }
    }
    out
}

fn magnitude(v: i32) -> (u32, u32) {
    let size = 32 - v.unsigned_abs().leading_zeros();
    let bits = if v < 0 { (v - 1) as u32 } else { v as u32 };
    (size, bits)
}

struct Encoder {
    bits: Bits,
    cos: [[f32; 8]; 8],
}

impl Encoder {
    fn block(&mut self, samples: &[f32; 64], q: &[u8; 64], dc: &Huffman, ac: &Huffman, previous: &mut i32) {
        let coefs = fdct(samples, &self.cos);
        let mut zz = [0i32; 64];
        for (i, &n) in ZIGZAG.iter().enumerate() {
            zz[i] = (coefs[n] / f32::from(q[n])).round() as i32;
        }
        let diff = zz[0] - *previous;
        *previous = zz[0];
        let (size, bits) = magnitude(diff);
        self.bits.put(u32::from(dc.code[size as usize]), u32::from(dc.len[size as usize]));
        self.bits.put(bits, size);
        let mut run = 0;
        for &v in &zz[1..] {
            if v == 0 {
                run += 1;
                continue;
            }
            while run > 15 {
                self.bits.put(u32::from(ac.code[0xf0]), u32::from(ac.len[0xf0]));
                run -= 16;
            }
            let (size, bits) = magnitude(v);
            let symbol = (run << 4 | size) as usize;
            self.bits.put(u32::from(ac.code[symbol]), u32::from(ac.len[symbol]));
            self.bits.put(bits, size);
            run = 0;
        }
        if run > 0 {
            self.bits.put(u32::from(ac.code[0]), u32::from(ac.len[0]));
        }
    }
}

fn segment(out: &mut Vec<u8>, marker: u8, body: &[u8]) {
    out.extend([0xff, marker]);
    out.extend(((body.len() + 2) as u16).to_be_bytes());
    out.extend(body);
}

fn huffman_segment(out: &mut Vec<u8>, class_id: u8, (counts, values): ([u8; 16], &[u8])) {
    let mut body = vec![class_id];
    body.extend(counts);
    body.extend(values);
    segment(out, 0xc4, &body);
}

/// Encodes 8 bit planes: luma `w` x `h`, and chroma at half that each way (rounded up).
pub fn encode(y: &[u8], cb: &[u8], cr: &[u8], w: usize, h: usize, quality: u8) -> Vec<u8> {
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    assert!(y.len() >= w * h && cb.len() >= cw * ch && cr.len() >= cw * ch && w > 0 && h > 0 && w < 65536 && h < 65536);
    let (ql, qc) = (scaled(&LUMA_Q, quality), scaled(&CHROMA_Q, quality));
    let mut out = vec![0xff, 0xd8];
    segment(&mut out, 0xe0, b"JFIF\0\x01\x01\0\0\x01\0\x01\0\0");
    for (id, q) in [(0u8, &ql), (1, &qc)] {
        let mut body = vec![id];
        body.extend(ZIGZAG.iter().map(|&n| q[n]));
        segment(&mut out, 0xdb, &body);
    }
    let mut sof = vec![8];
    sof.extend((h as u16).to_be_bytes());
    sof.extend((w as u16).to_be_bytes());
    sof.extend([3, 1, 0x22, 0, 2, 0x11, 1, 3, 0x11, 1]);
    segment(&mut out, 0xc0, &sof);
    huffman_segment(&mut out, 0x00, DC_LUMA);
    huffman_segment(&mut out, 0x10, AC_LUMA);
    huffman_segment(&mut out, 0x01, DC_CHROMA);
    huffman_segment(&mut out, 0x11, AC_CHROMA);
    segment(&mut out, 0xda, &[3, 1, 0x00, 2, 0x11, 3, 0x11, 0, 63, 0]);

    let mut cos = [[0f32; 8]; 8];
    for (u, row) in cos.iter_mut().enumerate() {
        let c = if u == 0 { std::f32::consts::FRAC_1_SQRT_2 } else { 1.0 };
        for (x, v) in row.iter_mut().enumerate() {
            *v = 0.5 * c * (((2 * x + 1) * u) as f32 * std::f32::consts::PI / 16.0).cos();
        }
    }
    let mut e = Encoder { bits: Bits { out: Vec::new(), acc: 0, n: 0 }, cos };
    let (dcl, acl, dcc, acc) =
        (Huffman::new(DC_LUMA), Huffman::new(AC_LUMA), Huffman::new(DC_CHROMA), Huffman::new(AC_CHROMA));
    let mut dc = [0i32; 3];
    // samples past the edge repeat the last row and column
    let fetch = |plane: &[u8], pw: usize, ph: usize, x: usize, y: usize| {
        f32::from(plane[y.min(ph - 1) * pw + x.min(pw - 1)]) - 128.0
    };
    for my in (0..h).step_by(16) {
        for mx in (0..w).step_by(16) {
            for (bx, by) in [(0, 0), (8, 0), (0, 8), (8, 8)] {
                let mut s = [0f32; 64];
                for (i, v) in s.iter_mut().enumerate() {
                    *v = fetch(y, w, h, mx + bx + i % 8, my + by + i / 8);
                }
                e.block(&s, &ql, &dcl, &acl, &mut dc[0]);
            }
            for (c, plane) in [cb, cr].into_iter().enumerate() {
                let mut s = [0f32; 64];
                for (i, v) in s.iter_mut().enumerate() {
                    *v = fetch(plane, cw, ch, mx / 2 + i % 8, my / 2 + i / 8);
                }
                e.block(&s, &qc, &dcc, &acc, &mut dc[c + 1]);
            }
        }
    }
    e.bits.flush();
    out.extend(e.bits.out);
    out.extend([0xff, 0xd9]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_huffman_tables_are_the_standards() {
        let dc = Huffman::new(DC_LUMA);
        assert_eq!((dc.code[0], dc.len[0]), (0b00, 2));
        assert_eq!((dc.code[11], dc.len[11]), (0b1_1111_1110, 9));
        let ac = Huffman::new(AC_LUMA);
        assert_eq!((ac.code[0], ac.len[0]), (0b1010, 4), "end of block");
        assert_eq!((ac.code[0xf0], ac.len[0xf0]), (0b111_1111_1001, 11), "zero run");
        assert_eq!(AC_LUMA.1.len(), 162);
        assert_eq!(AC_CHROMA.1.len(), 162);
    }

    #[test]
    fn a_picture_is_framed_as_a_jpeg() {
        let (w, h) = (37, 21);
        let y: Vec<u8> = (0..w * h).map(|i| (i * 7 % 256) as u8).collect();
        let c = vec![128u8; 19 * 11];
        let jpeg = encode(&y, &c, &c, w, h, 85);
        assert_eq!(&jpeg[..2], &[0xff, 0xd8]);
        assert_eq!(&jpeg[jpeg.len() - 2..], &[0xff, 0xd9]);
        // the frame header says the size
        let sof = jpeg.windows(2).position(|m| m == [0xff, 0xc0]).unwrap();
        assert_eq!(&jpeg[sof + 5..sof + 9], &[0, 21, 0, 37]);
        // no marker inside the scan: every 0xFF there is stuffed
        let sos = jpeg.windows(2).position(|m| m == [0xff, 0xda]).unwrap() + 14;
        let scan = &jpeg[sos..jpeg.len() - 2];
        assert!(scan.windows(2).all(|p| p[0] != 0xff || p[1] == 0));
    }
}
