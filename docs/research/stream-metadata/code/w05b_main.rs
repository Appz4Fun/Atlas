use std::panic;
use std::time::Instant;

use image::GrayImage;
use image_hasher::{HashAlg, HasherConfig};

const S: &str = "/tmp/claude-0/-home-claude/2f11c7bb-b3fb-5488-8bbf-52c60549e24e/scratchpad/w5b.VgV4/samples";

/// Decoded luma of the first output frame, cropped.
struct Out {
    w: usize,
    h: usize,
    bit_depth: u8,
    luma: Vec<u16>,
}

fn dec_h265(data: &[u8]) -> Result<Out, String> {
    use rust_h265::{Decoder, PixelData, parse_annex_b};
    let mut d = Decoder::new().with_display_order();
    let mut first = None;
    for nal in parse_annex_b(data) {
        d.decode_nal(&nal).map_err(|e| format!("decode_nal {:?}", e))?;
        if first.is_none() {
            first = d.next_output();
        }
    }
    if first.is_none() {
        first = d.next_output();
    }
    if first.is_none() {
        first = d.flush();
    }
    let f = first.ok_or("no frame output")?;
    let luma = match &f.y {
        PixelData::U8(v) => v.iter().map(|&x| x as u16).collect(),
        PixelData::U16(v) => v.clone(),
    };
    Ok(Out { w: f.width as usize, h: f.height as usize, bit_depth: f.bit_depth, luma })
}

fn dec_rusty(data: &[u8]) -> Result<Out, String> {
    use rusty_h265::Decoder;
    let mut d = Decoder::new();
    d.push_annexb(data, None).map_err(|e| format!("push {:?}", e))?;
    d.flush();
    let f = d.next_frame().map_err(|e| format!("next_frame {:?}", e))?;
    let (w, h) = (f.width, f.height);
    let (cx, cy, _, _) = f.picture.crop;
    let p = &f.picture.planes[0];
    let mut luma = Vec::with_capacity(w * h);
    for y in 0..h {
        let o = (cy + y) * p.stride + cx;
        luma.extend_from_slice(&p.data[o..o + w]);
    }
    Ok(Out { w, h, bit_depth: f.bit_depth(), luma })
}

fn dec_h264(data: &[u8]) -> Result<Out, String> {
    use rust_h264::decoder::OrderedDecoder;
    use rust_h264::nal::parse_annex_b;
    let mut d = OrderedDecoder::<()>::new();
    let mut first = None;
    for nal in parse_annex_b(data) {
        let v = d.decode_nal(&nal).map_err(|e| format!("decode_nal {:?}", e))?;
        if first.is_none() {
            first = v.into_iter().next();
        }
    }
    if first.is_none() {
        first = d.flush().into_iter().next();
    }
    let f = first.ok_or("no frame output")?;
    let luma = f.y.iter().map(|&x| x as u16).collect();
    Ok(Out { w: f.width as usize, h: f.height as usize, bit_depth: 8, luma })
}

fn read_ref(name: &str, bytes_per_sample: usize) -> Vec<u16> {
    let b = std::fs::read(format!("{}/{}", S, name)).expect("ref");
    if bytes_per_sample == 1 {
        b.iter().map(|&x| x as u16).collect()
    } else {
        b.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect()
    }
}

fn psnr(a: &[u16], b: &[u16], peak: f64) -> f64 {
    if a.len() != b.len() {
        return f64::NAN;
    }
    let mse: f64 = a.iter().zip(b).map(|(&x, &y)| {
        let d = x as f64 - y as f64;
        d * d
    }).sum::<f64>() / a.len() as f64;
    if mse == 0.0 { f64::INFINITY } else { 10.0 * (peak * peak / mse).log10() }
}

/// 32x32 box-average downscale of luma (samples already reduced to 8 bit).
fn to_32(o: &Out) -> GrayImage {
    let shift = o.bit_depth.saturating_sub(8);
    let mut out = vec![0u8; 32 * 32];
    for oy in 0..32 {
        for ox in 0..32 {
            let x0 = ox * o.w / 32;
            let x1 = ((ox + 1) * o.w / 32).max(x0 + 1);
            let y0 = oy * o.h / 32;
            let y1 = ((oy + 1) * o.h / 32).max(y0 + 1);
            let mut sum = 0u64;
            let mut n = 0u64;
            for y in y0..y1 {
                for x in x0..x1 {
                    sum += (o.luma[y * o.w + x] >> shift) as u64;
                    n += 1;
                }
            }
            out[oy * 32 + ox] = (sum / n) as u8;
        }
    }
    GrayImage::from_raw(32, 32, out).unwrap()
}

fn hashes(img: &GrayImage) -> Vec<(String, image_hasher::ImageHash)> {
    let algs = [
        ("mean8", HasherConfig::new().hash_alg(HashAlg::Mean).hash_size(8, 8)),
        ("gradient8", HasherConfig::new().hash_alg(HashAlg::Gradient).hash_size(8, 8)),
        ("blockhash16", HasherConfig::new().hash_alg(HashAlg::Blockhash).hash_size(16, 16)),
    ];
    algs.into_iter().map(|(n, c)| (n.to_string(), c.to_hasher().hash_image(img))).collect()
}

fn vmhwm() -> String {
    std::fs::read_to_string("/proc/self/status")
        .unwrap_or_default()
        .lines()
        .find(|l| l.starts_with("VmHWM"))
        .unwrap_or("VmHWM ?")
        .to_string()
}

fn run(name: &str, file: &str, bytes_per_sample: usize, reff: &str, peak: f64) {
    let data = std::fs::read(format!("{}/{}", S, file)).unwrap();
    let decode = |d: &[u8]| -> Result<Out, String> {
        match name {
            "h265" => dec_h265(d),
            "rusty" => dec_rusty(d),
            _ => dec_h264(d),
        }
    };
    // Panic-safe single decode for correctness.
    let res = panic::catch_unwind(panic::AssertUnwindSafe(|| decode(&data)));
    let out = match res {
        Err(_) => {
            println!("[{name}] {file}: PANIC");
            return;
        }
        Ok(Err(e)) => {
            println!("[{name}] {file}: ERROR {e}");
            return;
        }
        Ok(Ok(o)) => o,
    };
    let refv = read_ref(reff, bytes_per_sample);
    let p = psnr(&out.luma, &refv, peak);
    // Timing: 10 timed iterations, report min and median.
    let mut ts = Vec::new();
    for _ in 0..10 {
        let t = Instant::now();
        let r = panic::catch_unwind(panic::AssertUnwindSafe(|| decode(&data)));
        let el = t.elapsed().as_secs_f64() * 1000.0;
        if r.is_err() {
            println!("[{name}] panic during timed run");
            return;
        }
        ts.push(el);
    }
    ts.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let hs = hashes(&to_32(&out));
    println!(
        "[{name}] {file}: {}x{} bd={} ref={} PSNR={:.2}dB ms/frame min={:.2} median={:.2} | {}",
        out.w, out.h, out.bit_depth, reff, p, ts[0], ts[ts.len() / 2], vmhwm()
    );
    for (n, h) in &hs {
        println!("    {n}: {}", h.to_base64());
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let which = args.get(1).map(|s| s.as_str()).unwrap_or("all");
    panic::set_hook(Box::new(|i| eprintln!("panic hook: {i}")));
    if which == "hash" {
        // Resolution comparison: decode 720p / 1080p / 360p and hash each.
        let mut sets = Vec::new();
        for (f, label) in [("one8.265", "1280x720"), ("one8_1080.265", "1920x1080"), ("one8_360.265", "640x360")] {
            let data = std::fs::read(format!("{}/{}", S, f)).unwrap();
            let o = dec_h265(&data).expect("h265 decode");
            let h = hashes(&to_32(&o));
            println!("[hash] {label} {}x{}", o.w, o.h);
            sets.push(h);
        }
        for alg in 0..3 {
            let base = &sets[0][alg];
            let name = &base.0;
            let d1080 = base.1.dist(&sets[1][alg].1);
            let d360 = base.1.dist(&sets[2][alg].1);
            println!("[hash] {name}: dist(720,1080)={d1080} dist(720,360)={d360} (bits: {})", base.1.as_bytes().len() * 8);
        }
        return;
    }
    if which == "all" || which == "h265" {
        run("h265", "one8.265", 1, "ref8.y", 255.0);
        run("h265", "one10.265", 2, "ref10.y", 1023.0);
    }
    if which == "all" || which == "rusty" {
        run("rusty", "one8.265", 1, "ref8.y", 255.0);
        run("rusty", "one10.265", 2, "ref10.y", 1023.0);
    }
    if which == "all" || which == "h264" {
        run("h264", "oneb.264", 1, "refb.y", 255.0);
    }
}
