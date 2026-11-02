use std::fs;
fn crc16(buf: &[u8]) -> u16 {
    let mut c: u16 = 0;
    for &b in buf { c ^= (b as u16) << 8; for _ in 0..8 { c = if c & 0x8000 != 0 { (c << 1) ^ 0x2D } else { c << 1 }; } }
    c
}
fn main() {
    for p in std::env::args().skip(1) {
        let d = fs::read(&p).unwrap();
        let s0 = d.windows(4).position(|w| w == [0xF8,0x72,0x6F,0xBA] || w == [0xF8,0x72,0x6F,0xBB]).unwrap();
        let mut hits = Vec::new();
        for start in (0..=8usize).step_by(4) { for end in (start+4)..=40usize { for f in end..=(end+2) {
            if s0 + f + 2 > d.len() { continue; }
            let c = crc16(&d[s0+start..s0+end]);
            let be = u16::from_be_bytes([d[s0+f], d[s0+f+1]]);
            let le = u16::from_le_bytes([d[s0+f], d[s0+f+1]]);
            if c == be { hits.push(format!("BE start=+{} end=+{} field=+{}", start, end, f)); }
            if c == le { hits.push(format!("LE start=+{} end=+{} field=+{}", start, end, f)); }
        }}}
        println!("{} syncoff={} hits={} first: {}", p, s0, hits.len(), hits.iter().take(4).cloned().collect::<Vec<_>>().join(" | "));
    }
}
