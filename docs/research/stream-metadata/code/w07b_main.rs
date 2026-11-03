// w07b: video robustness probes (4K cost, HDR10 remux, open-GOP CRA start, interlaced H.264 SPS/slices, MPEG-2 I-frame).
use std::io::Cursor;
use std::time::Instant;
use matroska_demuxer::{Frame as MFrame, MatroskaFile, TrackType};
use rust_h265::PixelData;

const S: &str = "/tmp/claude-0/-home-claude/2f11c7bb-b3fb-5488-8bbf-52c60549e24e/scratchpad/w7b.kcCE/s";

struct Out { w: usize, h: usize, bd: u8, y: Vec<u16> }

fn vmhwm() -> String {
    std::fs::read_to_string("/proc/self/status").unwrap_or_default().lines()
        .find(|l| l.starts_with("VmHWM")).unwrap_or("VmHWM ?").trim().to_string()
}
fn unesc(b: &[u8]) -> Vec<u8> {
    let (mut o, mut z) = (Vec::with_capacity(b.len()), 0);
    for &x in b { if z >= 2 && x == 3 { z = 0; continue; } o.push(x); z = if x == 0 { z + 1 } else { 0 }; }
    o
}
// Split Annex B into NAL payloads (header included).
fn annexb(d: &[u8]) -> Vec<&[u8]> {
    let mut starts = vec![];
    let mut i = 0;
    while i + 3 <= d.len() {
        if d[i] == 0 && d[i + 1] == 0 && d[i + 2] == 1 { starts.push(i + 3); i += 3; } else { i += 1; }
    }
    starts.iter().enumerate().map(|(k, &s)| {
        let e = if k + 1 < starts.len() { starts[k + 1] - 3 } else { d.len() };
        let mut e2 = e; while e2 > s && d[e2 - 1] == 0 { e2 -= 1; }
        &d[s..e2]
    }).collect()
}
fn psnr(a: &[u16], b: &[u16], peak: f64) -> f64 {
    if a.len() != b.len() { return f64::NAN; }
    let mse = a.iter().zip(b).map(|(&x, &y)| { let d = x as f64 - y as f64; d * d }).sum::<f64>() / a.len() as f64;
    if mse == 0.0 { f64::INFINITY } else { 10.0 * (peak * peak / mse).log10() }
}
fn ref16(name: &str, bps: usize) -> Vec<u16> {
    let b = std::fs::read(format!("{S}/{name}")).expect("ref");
    if bps == 1 { b.iter().map(|&x| x as u16).collect() } else { b.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect() }
}

fn h265_all(data: &[u8]) -> Vec<Out> {
    use rust_h265::{Decoder, parse_annex_b};
    let mut d = Decoder::new().with_display_order();
    let mut outs = vec![];
    let conv = |f: rust_h265::Frame| {
        let y = match &f.y { PixelData::U8(v) => v.iter().map(|&x| x as u16).collect(), PixelData::U16(v) => v.clone() };
        Out { w: f.width as usize, h: f.height as usize, bd: f.bit_depth, y }
    };
    for nal in parse_annex_b(data) {
        if let Some(f) = d.decode_nal(&nal).expect("decode_nal") { outs.push(conv(f)); }
        while let Some(f) = d.next_output() { outs.push(conv(f)); }
    }
    let mut n = 0;
    while let Some(f) = d.flush() { outs.push(conv(f)); n += 1; if n > 10000 { break; } }
    outs
}

fn rusty_all(data: &[u8]) -> Vec<Out> {
    use rusty_h265::Decoder;
    let mut d = Decoder::new();
    d.push_annexb(data, None).expect("push");
    d.flush();
    let mut outs = vec![];
    while let Ok(f) = d.next_frame() {
        let (w, h) = (f.width, f.height);
        let (cx, cy, _, _) = f.picture.crop;
        let p = &f.picture.planes[0];
        let mut y = Vec::with_capacity(w * h);
        for r in 0..h { let o = (cy + r) * p.stride + cx; y.extend_from_slice(&p.data[o..o + w]); }
        outs.push(Out { w, h, bd: f.bit_depth(), y });
    }
    outs
}

// Task 1: 4K 10-bit HEVC; ms/frame over all frames, min of 5 runs; bit-exact frame 0 vs ffmpeg.
fn t1(which: &str) {
    let data = std::fs::read(format!("{S}/t1_4k10.265")).unwrap();
    let reff = ref16("t1_ref4k10.y", 2)[..3840 * 2160].to_vec();
    let run = |d: &[u8]| if which == "h265" { h265_all(d) } else { rusty_all(d) };
    let first = run(&data);
    let frames = first.len();
    let p = psnr(&first[0].y, &reff, 1023.0);
    let mut best = f64::MAX;
    for _ in 0..5 { let t = Instant::now(); let _ = run(&data); best = best.min(t.elapsed().as_secs_f64() * 1000.0); }
    println!("[t1 {which}] {}x{} bd={} frames_out={} bitexact_Y0={} PSNR={:.2} ms/frame(min of 5, whole 3-frame run / frames)={:.1} | {}",
        first[0].w, first[0].h, first[0].bd, frames, first[0].y == reff, p, best / frames.max(1) as f64, vmhwm());
}

// hvcC walk: returns (array types, sei-in-hvcc, length_size)
fn hvcc_info(cp: &[u8]) -> (Vec<String>, usize) {
    let lsz = (cp[21] & 3) as usize + 1;
    let na = cp[22] as usize;
    let mut p = 23; let mut v = vec![];
    for _ in 0..na {
        let t = cp[p] & 0x3f; let n = u16::from_be_bytes([cp[p + 1], cp[p + 2]]) as usize; p += 3;
        let mut cnt = 0; let mut sei = 0;
        for _ in 0..n { let l = u16::from_be_bytes([cp[p], cp[p + 1]]) as usize; p += 2; cnt += 1; if t == 39 || t == 40 { sei += 1; let _ = sei; } p += l; }
        v.push(format!("type{}:n{}", t, cnt));
    }
    (v, lsz)
}

fn t2(mkv_path: &str) {
    let bytes = std::fs::read(mkv_path).unwrap();
    let mut mkv = MatroskaFile::open(Cursor::new(bytes.clone())).unwrap();
    println!("[t2] info: timestamp_scale={} duration={:?} muxing_app={:?} writing_app={:?} chapters={} tags={}",
        mkv.info().timestamp_scale(), mkv.info().duration(), mkv.info().muxing_app(), mkv.info().writing_app(),
        mkv.chapters().map_or(0, |c| c.len()), mkv.tags().map_or(0, |t| t.len()));
    let vt = mkv.tracks().iter().find(|t| t.track_type() == TrackType::Video).unwrap().clone();
    let cp = vt.codec_private().map(|c| c.to_vec()).unwrap_or_default();
    let v = vt.video().unwrap();
    println!("[t2] video codec_id={} codec_private_len={} px={}x{} colour={:?}", vt.codec_id(), cp.len(), v.pixel_width(), v.pixel_height(), v.colour());
    let (arrs, lsz) = hvcc_info(&cp);
    println!("[t2] hvcC arrays={:?} lengthSize={}", arrs, lsz);
    let mut fr = MFrame::from(Vec::new());
    loop { if !mkv.next_frame(&mut fr).unwrap() { break; } if fr.track == vt.track_number().get() { break; } }
    let mut p = 0; let mut kinds = vec![];
    while p + lsz <= fr.data.len() {
        let mut n = 0; for i in 0..lsz { n = (n << 8) | fr.data[p + i] as usize; }
        p += lsz; let t = (fr.data[p] >> 1) & 0x3f; kinds.push(t);
        if t == 39 || t == 40 { let pl = unesc(&fr.data[p + 2..p + n]); println!("[t2] frame0 SEI nal type {} first payload type={} size={} (in-band)", t, pl[0], pl[1]); }
        p += n;
    }
    println!("[t2] frame0 in-band NAL types: {:?}", kinds);
}

// Task 3: cut stream at the 2nd CRA (type 21, first_slice=1), decode, compare outputs with full-stream display frames.
fn t3() {
    let data = std::fs::read(format!("{S}/t3_open.265")).unwrap();
    let nals = annexb(&data);
    let mut cra = vec![]; let mut off = 0usize;
    for n in &nals {
        // locate offset of this NAL in data
        let pos = data[off..].windows(n.len()).position(|w| w == *n).unwrap() + off; off = pos + n.len();
        if (n[0] >> 1) & 0x3f == 21 && n[2] & 0x80 != 0 { cra.push(pos); }
    }
    let first_vcl = data.windows(4).position(|w| w == [0, 0, 0, 1]).unwrap_or(0);
    let _ = first_vcl;
    let c2 = cra[0] - 3; // include start code
    let prefix_end = {
        // parameter sets prefix: everything before the first VCL NAL start
        let mut e = 0; for n in &nals { let t = (n[0] >> 1) & 0x3f; if t < 32 { break; } let p = data[e..].windows(n.len()).position(|w| w == *n).unwrap() + e; e = p + n.len(); }
        e
    };
    let mut cut = data[..prefix_end].to_vec();
    cut.extend_from_slice(&data[c2..]);
    let fullref = ref16("t3_full.y", 2);
    let fsz = 960 * 540 * 3 / 2; // Y 16-bit samples per frame (yuv420p10le): 960*540 samples
    let fr = |i: usize| &fullref[i * 960 * 540 * 3 / 2..][..960 * 540];
    let _ = fsz;
    let check = |name: &str, outs: &[Out]| {
        let mut m = vec![];
        for o in outs.iter().take(8) {
            let hit = (0..120).find(|&i| fr(i) == &o.y[..]);
            m.push(hit.map(|i| i.to_string()).unwrap_or("none".into()));
        }
        let exact_seq = outs.iter().enumerate().filter(|(i, o)| 48 + i < 120 && o.y[..] == *fr(48 + *i)).count();
        println!("[t3 {name}] cut at CRA#2 (byte {}): outputs={} first display-indices(ref)={:?} exact_match_as_frames_48+i={} / {} | {}",
            c2, outs.len(), m, exact_seq, outs.len().min(72), vmhwm());
    };
    let h = h265_all(&cut); check("h265", &h);
    let r = rusty_all(&cut); check("rusty", &r);
    // whole-stream sanity: decode full with each and count frames
    let hf = h265_all(&data); println!("[t3] full-stream outputs h265={} (expect 120)", hf.len());
}

// Task 4: interlaced H.264. SPS flags + slice field_pic_flag count, bit-exact frame0 vs ffmpeg.
struct Br<'a> { d: &'a [u8], p: usize }
impl<'a> Br<'a> {
    fn u(&mut self, n: u32) -> u64 { let mut v = 0u64; for _ in 0..n { let b = (self.d[self.p >> 3] >> (7 - (self.p & 7))) & 1; v = (v << 1) | b as u64; self.p += 1; } v }
    fn ue(&mut self) -> u64 { let mut z = 0; while self.u(1) == 0 { z += 1; } ((1u64 << z) - 1) + self.u(z) }
}
fn t4() {
    let data = std::fs::read(format!("{S}/t4_il.264")).unwrap();
    let nals = annexb(&data);
    let sps = nals.iter().find(|n| n[0] & 0x1f == 7).unwrap();
    let rb = unesc(&sps[1..]);
    let mut b = Br { d: &rb, p: 0 };
    let profile = b.u(8); b.u(8); b.u(8); b.ue();
    if [100, 110, 122, 244, 44, 83, 86, 118, 128, 138, 139, 134, 135].contains(&profile) {
        let cf = b.ue(); if cf == 3 { b.u(1); } b.ue(); b.ue(); b.u(1);
        if b.u(1) == 1 { for _ in 0..(if cf != 3 { 8 } else { 12 }) { if b.u(1) == 1 { /* skip scaling list: not expected for x264 default */ } } }
    }
    let log2_max_fn = b.ue() + 4; let poc = b.ue();
    if poc == 0 { b.ue(); } else if poc == 1 { b.u(1); b.ue(); b.ue(); let n = b.ue(); for _ in 0..n { b.ue(); } }
    b.ue(); b.u(1); let wmb = b.ue() + 1; let hmap = b.ue() + 1;
    let fmo = b.u(1); let mbaff = if fmo == 0 { b.u(1) } else { 0 };
    println!("[t4 sps] profile={} width={} frame_height={} frame_mbs_only_flag={} mb_adaptive_frame_field_flag={} log2_max_frame_num={}", profile, wmb * 16, hmap * 16 * (2 - fmo), fmo, mbaff, log2_max_fn);
    // slice field_pic_flag census
    let (mut fld, mut frm) = (0, 0);
    for n in &nals {
        let t = n[0] & 0x1f;
        if t != 1 && t != 5 { continue; }
        let rb = unesc(&n[1..]);
        let mut s = Br { d: &rb, p: 0 };
        s.ue(); s.ue(); s.ue(); s.u(log2_max_fn as u32);
        if fmo == 0 { if s.u(1) == 1 { fld += 1; } else { frm += 1; } }
    }
    println!("[t4 slices] field_pic_flag=1 slices={} frame slices={}", fld, frm);
    let reff = ref16("t4_ref.y", 1)[..720 * 480].to_vec();
    let outs = { use rust_h264::decoder::OrderedDecoder; use rust_h264::nal::parse_annex_b;
        let mut d = OrderedDecoder::<()>::new(); let mut v = vec![];
        for nal in parse_annex_b(&data) { match d.decode_nal(&nal) { Ok(fs) => for f in fs { v.push(Out { w: f.width as usize, h: f.height as usize, bd: 8, y: f.y.iter().map(|&x| x as u16).collect() }); }, Err(e) => { println!("[t4] decode_nal err {:?}", e); break; } } }
        for f in d.flush() { v.push(Out { w: f.width as usize, h: f.height as usize, bd: 8, y: f.y.iter().map(|&x| x as u16).collect() }); }
        v };
    if outs.is_empty() { println!("[t4 rust_h264] no output"); return; }
    let o = &outs[0];
    println!("[t4 rust_h264] outputs={} frame0 {}x{} bitexact_Y={} PSNR={:.2} | {}", outs.len(), o.w, o.h, o.y == reff, psnr(&o.y, &reff, 255.0), vmhwm());
}

// Task 5: MPEG-2 I-frame via oxideav-mpeg12video.
fn t5() {
    let data = std::fs::read(format!("{S}/t5_mpeg2.m2v")).unwrap();
    let reff = ref16("t5_ref.y", 1)[..720 * 480].to_vec();
    let frames = oxideav_mpeg12video::decode_video_sequence(&data);
    match frames {
        Err(e) => println!("[t5 mpeg12] ERROR {:?}", e),
        Ok(v) => {
            let f = &v[0].frame;
            let y: Vec<u16> = f.y.packed_rect(f.width, f.height).iter().map(|&x| x as u16).collect();
            println!("[t5 mpeg12] frames={} {}x{} picture_coding_type={:?} bitexact_Y={} PSNR={:.2}dB len={}/{}", v.len(), f.width, f.height, v[0].picture_coding_type, y == reff, psnr(&y, &reff, 255.0), y.len(), reff.len());
        }
    }
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    match a.get(1).map(|s| s.as_str()) {
        Some("t1") => t1(&a[2]),
        Some("t2") => t2(&a[2]),
        Some("t3") => t3(),
        Some("t4") => t4(),
        Some("t5") => t5(),
        _ => eprintln!("usage"),
    }
}
