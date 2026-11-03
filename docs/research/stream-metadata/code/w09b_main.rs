// w09b: same-film keyframe hash test. usage: w09b <workdir>   (workdir has var/*.264 var/*.265 var/ref_*.yuv)
// Decodes frames at display indices IDX from each variant with rust_h264 / rust_h265, then hashes
// them under 4 preprocessings (none, letterbox crop, histogram eq, crop+eq) with dHash8, pHash8 (DCT 32->8), blockhash16.
use image::GrayImage;
use image_hasher::{HashAlg, HasherConfig};
use std::fs;

const IDX: [usize; 5] = [12, 60, 120, 180, 228]; // 0.5, 2.5, 5.0, 7.5, 9.5 s at 24 fps
const PREP: [&str; 4] = ["none", "crop", "eq", "crop+eq"];
const HN: [&str; 3] = ["dHash8", "pHash8", "blk16"];
const THR: [u32; 3] = [10, 10, 40]; // 10/64 for 64-bit hashes, 40/256 for blockhash16

struct Sel { w: usize, h: usize, bd: u8, y: Vec<u16> }

#[derive(Clone)]
struct Hs { d: u64, p: u64, b: Vec<u8> }

fn dec_h264(data: &[u8]) -> Vec<Sel> {
    use rust_h264::decoder::OrderedDecoder;
    use rust_h264::nal::parse_annex_b;
    let mut d = OrderedDecoder::<()>::new();
    let (mut out, mut n) = (vec![], 0usize);
    let mut take = |f: rust_h264::decoder::Frame, out: &mut Vec<Sel>| {
        if IDX.contains(&n) {
            out.push(Sel { w: f.width as usize, h: f.height as usize, bd: 8, y: f.y.iter().map(|&v| v as u16).collect() });
        }
        n += 1;
    };
    for nal in parse_annex_b(data) {
        for f in d.decode_nal(&nal).expect("h264 decode") { take(f, &mut out); }
    }
    for f in d.flush() { take(f, &mut out); }
    eprintln!("  h264 output frames: {n}");
    out
}

fn dec_h265(data: &[u8]) -> Vec<Sel> {
    use rust_h265::{Decoder, PixelData, parse_annex_b};
    let mut d = Decoder::new().with_display_order();
    let (mut out, mut n) = (vec![], 0usize);
    let mut take = |f: rust_h265::Frame, out: &mut Vec<Sel>| {
        if IDX.contains(&n) {
            let y = match f.y { PixelData::U8(v) => v.iter().map(|&x| x as u16).collect(), PixelData::U16(v) => v };
            out.push(Sel { w: f.width as usize, h: f.height as usize, bd: f.bit_depth, y });
        }
        n += 1;
    };
    for nal in parse_annex_b(data) {
        d.decode_nal(&nal).expect("h265 decode");
        while let Some(f) = d.next_output() { take(f, &mut out); }
    }
    if let Some(f) = d.flush() { take(f, &mut out); }
    while let Some(f) = d.next_output() { take(f, &mut out); }
    eprintln!("  h265 output frames: {n}");
    out
}

fn to8(y: &[u16], bd: u8) -> Vec<u8> {
    let s = bd.saturating_sub(8);
    y.iter().map(|&v| (v >> s) as u8).collect()
}

// Drop leading/trailing rows and columns whose mean luma is below 20 (8-bit scale).
fn crop(p: &[u8], w: usize, h: usize) -> (Vec<u8>, usize, usize) {
    let rm: Vec<f64> = (0..h).map(|y| p[y * w..(y + 1) * w].iter().map(|&v| v as f64).sum::<f64>() / w as f64).collect();
    let cm: Vec<f64> = (0..w).map(|x| (0..h).map(|y| p[y * w + x] as f64).sum::<f64>() / h as f64).collect();
    match (rm.iter().position(|&m| m >= 20.0), rm.iter().rposition(|&m| m >= 20.0),
           cm.iter().position(|&m| m >= 20.0), cm.iter().rposition(|&m| m >= 20.0)) {
        (Some(a), Some(b), Some(c), Some(d)) => {
            let mut o = Vec::with_capacity((b - a + 1) * (d - c + 1));
            for y in a..=b { o.extend_from_slice(&p[y * w + c..y * w + d + 1]); }
            (o, d - c + 1, b - a + 1)
        }
        _ => (p.to_vec(), w, h),
    }
}

// Histogram equalisation on 8-bit luma (rank-based, so invariant to any monotonic luma remap).
fn eq(p: &[u8]) -> Vec<u8> {
    let mut hist = [0u64; 256];
    for &v in p { hist[v as usize] += 1; }
    let n = p.len() as u64;
    let mut cdf = [0u64; 256];
    let mut acc = 0;
    for i in 0..256 { acc += hist[i]; cdf[i] = acc; }
    let cmin = *cdf.iter().find(|&&c| c > 0).unwrap();
    let lut: [u8; 256] = std::array::from_fn(|i| {
        if n == cmin { 0 } else { (cdf[i].saturating_sub(cmin) as f64 * 255.0 / (n - cmin) as f64).round() as u8 }
    });
    p.iter().map(|&v| lut[v as usize]).collect()
}

fn box_resize(p: &[u8], w: usize, h: usize, dw: usize, dh: usize) -> Vec<f64> {
    let mut o = vec![0f64; dw * dh];
    for oy in 0..dh {
        for ox in 0..dw {
            let (x0, x1) = (ox * w / dw, ((ox + 1) * w / dw).max(ox * w / dw + 1));
            let (y0, y1) = (oy * h / dh, ((oy + 1) * h / dh).max(oy * h / dh + 1));
            let (mut s, mut n) = (0f64, 0f64);
            for y in y0..y1 { for x in x0..x1 { s += p[y * w + x] as f64; n += 1.0; } }
            o[oy * dw + ox] = s / n;
        }
    }
    o
}

fn dhash(p: &[u8], w: usize, h: usize) -> u64 {
    let r = box_resize(p, w, h, 9, 8);
    let mut bits = 0u64;
    for y in 0..8 { for x in 0..8 { if r[y * 9 + x] > r[y * 9 + x + 1] { bits |= 1 << (y * 8 + x); } } }
    bits
}

fn phash(p: &[u8], w: usize, h: usize) -> u64 {
    const N: usize = 32;
    let r = box_resize(p, w, h, N, N);
    let c = |k: usize, x: usize| (std::f64::consts::PI * (2 * x + 1) as f64 * k as f64 / (2.0 * N as f64)).cos();
    let mut tmp = [0f64; N * N];
    for v in 0..N { for x in 0..N { tmp[v * N + x] = (0..N).map(|xx| r[v * N + xx] * c(x, xx)).sum(); } }
    let mut d = vec![0f64; 64];
    for v in 0..8 { for u in 0..8 { d[v * 8 + u] = (0..N).map(|yy| tmp[yy * N + u] * c(v, yy)).sum(); } }
    let mut srt = d.clone();
    srt.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let med = (srt[31] + srt[32]) / 2.0;
    d.iter().enumerate().fold(0u64, |acc, (i, &v)| if v > med { acc | (1 << i) } else { acc })
}

fn blockhash(p: &[u8], w: usize, h: usize) -> Vec<u8> {
    let img = GrayImage::from_raw(w as u32, h as u32, p.to_vec()).unwrap();
    let hasher = HasherConfig::new().hash_alg(HashAlg::Blockhash).hash_size(16, 16).to_hasher();
    hasher.hash_image(&img).as_bytes().to_vec()
}

fn hs(p: &[u8], w: usize, h: usize) -> Hs {
    Hs { d: dhash(p, w, h), p: phash(p, w, h), b: blockhash(p, w, h) }
}

fn dist(a: &Hs, b: &Hs, hi: usize) -> u32 {
    match hi {
        0 => (a.d ^ b.d).count_ones(),
        1 => (a.p ^ b.p).count_ones(),
        _ => a.b.iter().zip(&b.b).map(|(x, y)| (x ^ y).count_ones()).sum(),
    }
}

// Compare decoded luma with ffmpeg's native-format dump of the same frames (sanity check of the decoder path).
fn refcheck(path: &str, sels: &[Sel]) -> String {
    let r = match fs::read(path) { Ok(r) => r, Err(_) => return "no ref".into() };
    let (mut exact, mut tot, mut mad) = (0usize, 0usize, 0f64);
    let mut off = 0usize;
    for s in sels {
        let bps = if s.bd > 8 { 2 } else { 1 };
        for i in 0..s.w * s.h {
            let v = if bps == 1 { r[off + i] as u16 } else { u16::from_le_bytes([r[off + 2 * i], r[off + 2 * i + 1]]) };
            let d = (v as i64 - s.y[i] as i64).abs();
            exact += (d == 0) as usize;
            mad += d as f64;
            tot += 1;
        }
        off += s.w * s.h * bps * 3 / 2;
    }
    format!("ref luma exact {}/{} ({:.2}%), mean abs diff {:.4}", exact, tot, 100.0 * exact as f64 / tot as f64, mad / tot as f64)
}

fn main() {
    let dir = std::env::args().nth(1).expect("workdir");
    let vars = [("a", "h264"), ("c", "h264"), ("d", "h264"), ("f", "h264"), ("g", "h264"), ("b", "h265"), ("e", "h265")];
    // hashes[v][k][prep]
    let mut hashes: Vec<Vec<Vec<Hs>>> = vec![];
    for (name, codec) in vars {
        let file = format!("{dir}/var/{name}.{}", if codec == "h264" { "264" } else { "265" });
        let data = fs::read(&file).expect("read stream");
        let t = std::time::Instant::now();
        let sels = if codec == "h264" { dec_h264(&data) } else { dec_h265(&data) };
        println!("variant {name} ({codec}): decoded {} selected frames in {:.1}s, {}", sels.len(), t.elapsed().as_secs_f64(), refcheck(&format!("{dir}/var/ref_{name}.yuv"), &sels));
        let mut per_k = vec![];
        for s in &sels {
            let p8 = to8(&s.y, s.bd);
            let (c, cw, ch) = crop(&p8, s.w, s.h);
            let e = eq(&p8);
            let ce = eq(&c);
            let variants = [(p8.clone(), s.w, s.h), (c, cw, ch), (e, s.w, s.h), (ce, cw, ch)];
            per_k.push(variants.iter().map(|(p, w, h)| hs(p, *w, *h)).collect::<Vec<Hs>>());
        }
        hashes.push(per_k);
    }
    let a = &hashes[0];
    println!("\nDistance vs (a), 5 positions, bits (thr {:?} for dHash/pHash/blk16): mean / max / pass-of-5 / vote-of-3 {{0,2,4}}", THR);
    for (vi, (name, _)) in vars.iter().enumerate().skip(1) {
        for p in 0..4 {
            let mut cols = String::new();
            for hi in 0..3 {
                let ds: Vec<u32> = (0..5).map(|k| dist(&a[k][p], &hashes[vi][k][p], hi)).collect();
                let mean = ds.iter().sum::<u32>() as f64 / 5.0;
                let mx = ds.iter().max().unwrap();
                let pass = ds.iter().filter(|&&d| d <= THR[hi]).count();
                let vote = [0, 2, 4].iter().filter(|&&k| ds[k] <= THR[hi]).count() >= 2;
                cols += &format!(" | {} {:.1}/{}/{}/{}", HN[hi], mean, mx, pass, vote as u8);
            }
            println!("{name} {:<8}{cols}", PREP[p]);
        }
    }
    println!("\nSame film, different scene (a[k] vs g[k]) and within-clip (a[i] vs a[j], 10 pairs):");
    for p in 0..4 {
        let mut cols = String::new();
        for hi in 0..3 {
            let g: Vec<u32> = (0..5).map(|k| dist(&a[k][p], &hashes[4][k][p], hi)).collect();
            let mut pr = vec![];
            for i in 0..5 { for j in i + 1..5 { pr.push(dist(&a[i][p], &a[j][p], hi)); } }
            let gm = g.iter().sum::<u32>() as f64 / 5.0;
            let gmin = g.iter().min().unwrap();
            let pm = pr.iter().sum::<u32>() as f64 / 10.0;
            let pmin = pr.iter().min().unwrap();
            let fp = pr.iter().filter(|&&d| d <= THR[hi]).count();
            let gfp = g.iter().filter(|&&d| d <= THR[hi]).count();
            cols += &format!(" | {} g:{:.1}/min{} fp{}/5 pairs:{:.1}/min{} fp{}/10", HN[hi], gm, gmin, gfp, pm, pmin, fp);
        }
        println!("{:<8}{cols}", PREP[p]);
    }
}
