// midwalk: mid-file random access over 700 KB Usenet articles (simulated as byte ranges).
// usage: midwalk <file.mkv> <scan|cues> <target_ms> <out_dir>
use rust_h264::decoder::{Decoder, Frame};
use rust_h264::nal::{parse_avcc, parse_avcc_config};
use std::collections::BTreeSet;
use std::time::Instant;

const ART: usize = 700_000;
const FRONT_ARTS: usize = 6; // already fetched by Atlas
const CLUSTER: u32 = 0x1F43_B675;
const LEVEL1: [u32; 8] = [0x1F43B675, 0x1C53BB6B, 0x1254C367, 0x1043A770, 0x114D9B74, 0x1549A966, 0x1654AE6B, 0x1941A469];
const CHILD_OK: [u32; 5] = [0xA3, 0xA0, 0xA7, 0xAB, 0xAF];

struct Store<'a> { file: &'a [u8], fetched: BTreeSet<usize> }
impl<'a> Store<'a> {
    fn get(&mut self, off: usize, len: usize) -> Option<&'a [u8]> {
        if off >= self.file.len() { return None; }
        let len = len.min(self.file.len() - off);
        if len > 0 { for a in off / ART..=(off + len - 1) / ART { self.fetched.insert(a); } }
        Some(&self.file[off..off + len])
    }
    fn peek(&mut self, off: usize, n: usize) -> &'a [u8] { self.get(off, n).unwrap_or(&[]) }
    fn len(&self) -> usize { self.file.len() }
}

fn vlen(b: u8) -> usize { if b == 0 { 0 } else { b.leading_zeros() as usize + 1 } }
fn read_id(b: &[u8]) -> Option<(u32, usize)> {
    let l = vlen(*b.first()?); if l == 0 || l > 4 || b.len() < l { return None; }
    Some((b[..l].iter().fold(0u32, |v, &x| (v << 8) | x as u32), l))
}
// returns (value, len); value None = unknown size (all data bits set)
fn read_size(b: &[u8]) -> Option<(Option<u64>, usize)> {
    let l = vlen(*b.first()?); if l == 0 || l > 8 || b.len() < l { return None; }
    let mut v = (b[0] as u64) & ((1u64 << (8 - l)) - 1);
    for &x in &b[1..l] { v = (v << 8) | x as u64; }
    if v == (1u64 << (7 * l)) - 1 { Some((None, l)) } else { Some((Some(v), l)) }
}
fn uint(b: &[u8]) -> u64 { b.iter().fold(0u64, |v, &x| (v << 8) | x as u64) }
// element header at absolute offset: (id, size, header_len)
fn elem(s: &mut Store, p: usize) -> Option<(u32, Option<u64>, usize)> {
    let b = s.peek(p, 12);
    let (id, il) = read_id(b)?; let (sz, sl) = read_size(&b[il..])?;
    Some((id, sz, il + sl))
}
// in-memory children of a fetched element body: (id, body)
fn children(mut b: &[u8]) -> Vec<(u32, &[u8])> {
    let mut out = vec![];
    while let Some((id, il)) = read_id(b) {
        let Some((Some(sz), sl)) = read_size(&b[il..]) else { break };
        let st = il + sl; let end = st + sz as usize;
        if end > b.len() { break; }
        out.push((id, &b[st..end])); b = &b[end..];
    }
    out
}

#[derive(Default, Debug, Clone)]
struct Track { num: u64, typ: u64, codec: String, cp: Vec<u8>, w: u64, h: u64, sr: f64, ch: u64 }

fn parse_tracks(body: &[u8]) -> Vec<Track> {
    let mut v = vec![];
    for (id, te) in children(body) { if id != 0xAE { continue; }
        let mut t = Track::default();
        for (cid, c) in children(te) { match cid {
            0xD7 => t.num = uint(c), 0x83 => t.typ = uint(c),
            0x86 => t.codec = String::from_utf8_lossy(c).into(), 0x63A2 => t.cp = c.to_vec(),
            0xE0 => for (x, d) in children(c) { match x { 0xB0 => t.w = uint(d), 0xBA => t.h = uint(d), _ => {} } },
            0xE1 => for (x, d) in children(c) { match x { 0xB5 => t.sr = if d.len() == 8 { f64::from_be_bytes(d.try_into().unwrap()) } else { f32::from_be_bytes(d.try_into().unwrap()) as f64 }, 0x9F => t.ch = uint(d), _ => {} } },
            _ => {} } }
        v.push(t);
    }
    v
}

struct Ctx { tracks: Vec<Track>, seg: usize, vid: u64, aud: u64 }

// Cluster validation at absolute offset p. Ok(timestamp) or rejection reason.
fn validate(s: &mut Store, p: usize, cx: &Ctx) -> Result<u64, &'static str> {
    let (id, sz, hl) = elem(s, p).ok_or("hdr-short")?;
    if id != CLUSTER { return Err("not-cluster"); }
    let body = p + hl;
    if let Some(sz) = sz { if body + sz as usize > s.len() { return Err("size-overflow"); } }
    // optional CRC-32 (0xBF) / Void (0xEC) children may precede Timestamp (ffmpeg writes CRC-32)
    let mut q = body; let (mut e_id, mut e_sz, mut e_hl) = elem(s, q).ok_or("no-child1")?;
    for _ in 0..3 {
        if e_id != 0xBF && e_id != 0xEC { break; }
        let z = e_sz.ok_or("bad-skip")? as usize; q += e_hl + z;
        let r = elem(s, q).ok_or("no-child1")?; e_id = r.0; e_sz = r.1; e_hl = r.2;
    }
    if e_id != 0xE7 { return Err("no-E7-first"); }
    let e_sz = e_sz.filter(|&z| (1..=8).contains(&z)).ok_or("bad-E7-size")? as usize;
    let ts = uint(s.peek(q + e_hl, e_sz));
    let c2 = q + e_hl + e_sz;
    let (c2_id, c2_sz, c2_hl) = elem(s, c2).ok_or("no-child2")?;
    if !CHILD_OK.contains(&c2_id) { return Err("bad-child2-id"); }
    if c2_id == 0xA3 {
        let tv = s.peek(c2 + c2_hl, 8);
        let (tn, _) = read_size(tv).ok_or("bad-track-vint")?;
        let tn = tn.ok_or("bad-track-vint")?;
        if !cx.tracks.iter().any(|t| t.num == tn) { return Err("unknown-track"); }
    }
    let _ = c2_sz;
    if let Some(sz) = sz {
        let e = body + sz as usize;
        if e < s.len() {
            let (nid, _, _) = elem(s, e).ok_or("bad-next-hdr")?;
            if !LEVEL1.contains(&nid) { return Err("bad-next-id"); }
        }
    }
    Ok(ts)
}

#[derive(Clone, Debug)]
struct Blk { track: u64, time_ms: i64, key: bool, frames: Vec<(usize, usize)> }

// Walk one validated cluster; returns blocks (frame byte ranges) and end offset.
fn walk_cluster(s: &mut Store, p: usize, ts: u64) -> (Vec<Blk>, usize) {
    let (_, sz, hl) = elem(s, p).unwrap();
    let body = p + hl;
    let end = sz.map(|z| body + z as usize).unwrap_or(s.len());
    let mut q = body; let mut out = vec![];
    while q < end.min(s.len()) {
        let Some((id, csz, chl)) = elem(s, q) else { break };
        if unknown_end(id) { break; }
        let Some(csz) = csz else { break };
        let pay = q + chl; let pend = pay + csz as usize;
        if id == 0xE7 { q = pend; continue; }
        if id == 0xA3 || id == 0xA1 { if let Some(b) = block(s, pay, csz as usize, ts, id == 0xA3, true) { out.push(b); } }
        if id == 0xA0 {
            let g = s.peek(pay, csz as usize).to_vec(); // small group body (block + optional ref)
            let mut is_key = true;
            for (gid, _) in children(&g) { if gid == 0xFB { is_key = false; } }
            for (gid, gb) in children(&g) { if gid == 0xA1 {
                let off = gb.as_ptr() as usize - g.as_ptr() as usize;
                if let Some(mut b) = block(s, pay + off, gb.len(), ts, false, false) { b.key = is_key; out.push(b); }
            } }
        }
        q = pend;
    }
    (out, end)
}
fn unknown_end(id: u32) -> bool { LEVEL1.contains(&id) }

fn block(s: &mut Store, pay: usize, n: usize, cl_ts: u64, simple: bool, _x: bool) -> Option<Blk> {
    let h = s.peek(pay, 16.min(n));
    let (tn, tl) = read_size(h)?; let tn = tn?;
    let rel = i16::from_be_bytes([h[tl], h[tl + 1]]) as i64;
    let flags = h[tl + 2];
    let lacing = (flags >> 1) & 3;
    let data = pay + tl + 3; let dlen = n - tl - 3;
    let mut frames = vec![];
    if lacing == 0 { frames.push((data, dlen)); }
    else {
        let cnt = s.peek(data, 1)[0] as usize + 1;
        let mut q = data + 1; let mut sizes: Vec<usize> = vec![];
        match lacing {
            1 => { for _ in 0..cnt - 1 { let mut sz = 0; loop { let b = s.peek(q, 1)[0] as usize; q += 1; sz += b; if b != 255 { break; } } sizes.push(sz); } }
            2 => { let each = (dlen - 1) / cnt; for _ in 0..cnt - 1 { sizes.push(each); } }
            3 => { let (v, l) = read_size(s.peek(q, 8))?; let mut prev = v? as i64; q += l; sizes.push(prev as usize);
                   for _ in 1..cnt - 1 { let b = s.peek(q, 8); let (raw, l) = read_size(b)?; let raw = raw? as i64; q += l;
                       let bias = (1i64 << (7 * l - 1)) - 1; prev += raw - bias; sizes.push(prev as usize); } }
            _ => {} }
        let used: usize = sizes.iter().sum::<usize>() + (q - data);
        if used > dlen { return None; }
        sizes.push(dlen - used + (q - data) - (q - data)); // placeholder fixed below
        let last = dlen - (q - data) - sizes[..sizes.len() - 1].iter().sum::<usize>();
        *sizes.last_mut().unwrap() = last;
        let mut off = q;
        for sz in sizes { frames.push((off, sz)); off += sz; }
    }
    let _ = simple;
    Some(Blk { track: tn, time_ms: cl_ts as i64 + rel, key: flags & 0x80 != 0, frames })
}

fn ac3_frames(s: &mut Store, fr: &[(usize, usize)]) -> (usize, bool) {
    let mut n = 0; let mut ok = true;
    for &(o, l) in fr { let b = s.peek(o, 2); if b != [0x0B, 0x77] || l < 8 { ok = false; } n += 1; }
    (n, ok)
}

fn decode_key(avcc: &[u8], frame: &[u8]) -> Option<Frame> {
    let cfg = parse_avcc_config(avcc).ok()?; let ls = cfg.length_size;
    let mut dec = Decoder::new();
    for n in cfg.sps_nals.iter().chain(cfg.pps_nals.iter()) { let _ = dec.decode_nal(n); }
    let mut out = None;
    for n in parse_avcc(frame, ls) { if let Ok(Some(f)) = dec.decode_nal(&n) { out = Some(f); } }
    out.or_else(|| dec.flush())
}

fn phash(y: &[u8], w: usize, h: usize) -> u64 {
    let n = 32; let mut s = vec![0f64; n * n];
    for j in 0..n { for i in 0..n {
        let (x0, x1) = (i * w / n, ((i + 1) * w / n).max(i * w / n + 1));
        let (y0, y1) = (j * h / n, ((j + 1) * h / n).max(j * h / n + 1));
        let (mut a, mut c) = (0f64, 0f64);
        for yy in y0..y1 { for xx in x0..x1 { a += y[yy * w + xx] as f64; c += 1.0; } }
        s[j * n + i] = a / c; } }
    let cs = |k: usize, x: usize| (std::f64::consts::PI * (2 * x + 1) as f64 * k as f64 / (2.0 * n as f64)).cos();
    let mut tmp = vec![0f64; n * n]; let mut d = vec![0f64; n * n];
    for v in 0..n { for x in 0..n { tmp[v * n + x] = (0..n).map(|xx| s[v * n + xx] * cs(x, xx)).sum(); } }
    for u in 0..n { for v in 0..n { d[v * n + u] = (0..n).map(|yy| tmp[yy * n + u] * cs(v, yy)).sum(); } }
    let vals: Vec<f64> = (0..8).flat_map(|v| (0..8).map(move |u| (v, u))).map(|(v, u)| d[v * n + u]).collect();
    let mut srt = vals.clone(); srt.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let med = (srt[31] + srt[32]) / 2.0;
    vals.iter().enumerate().fold(0u64, |h, (i, &v)| if v > med { h | (1 << i) } else { h })
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let file = std::fs::read(&args[1]).expect("read");
    let mode = args[2].as_str();
    let target_ms: i64 = args[3].parse().unwrap();
    let out_dir = &args[4];
    let t0 = Instant::now();
    let mut s = Store { file: &file, fetched: (0..FRONT_ARTS).collect() };
    let total_arts = (file.len() + ART - 1) / ART;

    // ---- front article parse: EBML header, Segment, Info, Tracks, SeekHead
    let (_, hsz, hhl) = elem(&mut s, 0).unwrap();
    let seg_hdr = hsz.unwrap() as usize + hhl;
    let (seg_id, seg_sz, seg_hl) = elem(&mut s, seg_hdr).unwrap();
    assert_eq!(seg_id, 0x18538067);
    let seg = seg_hdr + seg_hl;
    let seg_end = seg_sz.map(|z| seg + z as usize).unwrap_or(file.len());
    let mut tracks = vec![]; let mut seek_cues: Option<usize> = None; let mut tscale = 1_000_000u64;
    let mut q = seg;
    while q < seg_end {
        let (id, sz, hl) = elem(&mut s, q).unwrap();
        if id == CLUSTER { break; }
        let sz = sz.unwrap() as usize; let body = s.get(q + hl, sz).unwrap();
        match id {
            0x1654AE6B => tracks = parse_tracks(body),
            0x114D9B74 => for (c, cb) in children(body) { if c != 0x4DBB { continue; }
                let kids = children(cb);
                let sid = kids.iter().find(|k| k.0 == 0x53AB).map(|k| k.1).unwrap_or(&[]);
                let spos = kids.iter().find(|k| k.0 == 0x53AC).map(|k| uint(k.1)).unwrap_or(0);
                if read_id(sid).map(|x| x.0) == Some(0x1C53BB6B) { seek_cues = Some(seg + spos as usize); } },
            0x1549A966 => for (c, cb) in children(body) { if c == 0x2AD7B1 { tscale = uint(cb); } },
            _ => {} }
        q += hl + sz;
    }
    let _ = tscale;
    let vid = tracks.iter().find(|t| t.typ == 1).map(|t| t.num).unwrap_or(1);
    let aud = tracks.iter().find(|t| t.typ == 2).map(|t| t.num).unwrap_or(2);
    let cx = Ctx { tracks: tracks.clone(), seg, vid, aud };
    let avcc = tracks.iter().find(|t| t.num == vid).unwrap().cp.clone();
    eprintln!("tracks: {:?}", tracks.iter().map(|t| (t.num, t.typ, t.codec.clone(), t.w, t.h, t.sr, t.ch)).collect::<Vec<_>>());

    // ---- find start cluster
    let mut rej: std::collections::BTreeMap<&str, usize> = Default::default();
    let mut raw_hits = 0usize;
    let (mut p, ts0, start_art): (usize, u64, usize);
    if mode == "cues" {
        // Cues via SeekHead offset, then CueClusterPosition for target time (video track)
        let cpos = seek_cues.expect("no SeekHead entry for Cues");
        let (_, csz, chl) = elem(&mut s, cpos).unwrap();
        let cbody = s.get(cpos + chl, csz.unwrap() as usize).unwrap();
        let mut best: Option<(i64, usize)> = None;
        for (cid, cp) in children(cbody) { if cid != 0xBB { continue; }
            let kids = children(cp);
            let t = kids.iter().find(|k| k.0 == 0xB3).map(|k| uint(k.1) as i64).unwrap_or(0);
            if t > target_ms { continue; }
            for (x, tp) in kids.iter().filter(|k| k.0 == 0xB7) {
                let tk = children(tp);
                let trk = tk.iter().find(|k| k.0 == 0xF7).map(|k| uint(k.1)).unwrap_or(0);
                let cp_ = tk.iter().find(|k| k.0 == 0xF1).map(|k| uint(k.1)).unwrap_or(0);
                if trk == vid && best.map(|b| t >= b.0).unwrap_or(true) { best = Some((t, seg + cp_ as usize)); }
                let _ = x;
            } }
        let (bt, bp) = best.expect("no cue");
        eprintln!("cue for {} ms at {} ms, cluster abs off {}", target_ms, bt, bp);
        p = bp; start_art = p / ART;
        ts0 = 0; // set from cluster below
    } else {
        start_art = total_arts / 2; // ~50 % of file
        let mut pos = start_art * ART;
        loop {
            if pos + 4 > file.len() { panic!("no cluster {:?}", rej); }
            if s.peek(pos, 4) == [0x1F, 0x43, 0xB6, 0x75] {
                raw_hits += 1;
                match validate(&mut s, pos, &cx) { Ok(_) => break, Err(r) => *rej.entry(r).or_insert(0) += 1 }
            }
            pos += 1;
        }
        p = pos; ts0 = 0;
    }
    let ts_init = validate(&mut s, p, &cx).expect("cluster invalid");
    let _ = ts0;
    eprintln!("start cluster at {} (article {}), cluster ts {} ms", p, p / ART, ts_init);

    // ---- walk clusters: first video keyframe at/after start, then 10 s of audio
    let mut key: Option<(i64, Frame)> = None; let mut audio_ms = 0i64; let mut aud_frames = 0usize;
    let mut aud_ok = true; let mut key_bytes = 0usize; let mut ts_cur = ts_init;
    let mut clusters = 0;
    let mut key_time_ms = 0i64; let mut key_arts = 0usize;
    'outer: loop {
        let (blocks, end) = walk_cluster(&mut s, p, ts_cur);
        clusters += 1;
        for b in &blocks {
            if b.track == cx.vid && b.key && key.is_none() {
                let (o, l) = b.frames[0];
                let frame = s.get(o, l).unwrap();
                key_bytes = l;
                let t = Instant::now();
                let f = decode_key(&avcc, frame);
                eprintln!("keyframe decoded in {:?}", t.elapsed());
                key_time_ms = b.time_ms; key_arts = s.fetched.len();
                if let Some(f) = f { key = Some((b.time_ms, f)); }
            } else if b.track == cx.aud && key.is_some() && b.time_ms >= key_time_ms {
                let (n, ok) = ac3_frames(&mut s, &b.frames); aud_frames += n; aud_ok &= ok;
                for &(o, l) in &b.frames { let _ = s.get(o, l); }
                audio_ms += (n as i64) * 1536 * 1000 / 48000;
                if audio_ms >= 10_000 { break 'outer; }
            }
        }
        // next cluster: known size -> at end; unknown -> scan
        let nxt = end;
        if nxt >= file.len() { break; }
        match validate(&mut s, nxt, &cx) {
            Ok(ts) => { p = nxt; ts_cur = ts; }
            Err(r) => { *rej.entry(r).or_insert(0) += 1; break; }
        }
        if clusters > 500 { break; }
    }
    let secs = t0.elapsed().as_secs_f64();
    let _ = (aud, seek_cues);
    let fetched: Vec<usize> = s.fetched.iter().cloned().collect();
    let extra = fetched.iter().filter(|&&a| a >= FRONT_ARTS).count();
    let (kt, kf) = key.as_ref().map(|(t, f)| (*t, Some(f.clone()))).unwrap_or((-1, None));
    println!("mode={mode} file={} total_articles={total_arts} start_article={start_art}", args[1]);
    println!("start_cluster_off={p} raw_0x1F43B675_hits={raw_hits} rejected={:?}", rej);
    println!("keyframe_time_ms={kt} keyframe_bytes={key_bytes} keyframe_decoded={}", kf.is_some());
    println!("articles_fetched_total={} (incl {} front) articles_beyond_front={extra} articles_at_keyframe={key_arts}", fetched.len(), FRONT_ARTS);
    println!("audio_ac3_frames={aud_frames} audio_ms={audio_ms} audio_frames_ok={aud_ok} clusters_walked={clusters}");
    println!("wall_time_s={secs:.4} (in-memory: no network)");
    if let Some(f) = kf {
        let mut yuv = Vec::new();
        yuv.extend_from_slice(&f.y); yuv.extend_from_slice(&f.u); yuv.extend_from_slice(&f.v);
        std::fs::write(format!("{out_dir}/key.yuv"), &yuv).unwrap();
        let w = f.width as usize; let h = f.height as usize;
        println!("decoded {}x{} bit_depth={} pHash={:016x}", w, h, f.bit_depth, phash(&f.y, w, h));
        if args.len() > 5 {
            let r = std::fs::read(&args[5]).unwrap();
            let ry = &r[..w * h];
            let ph = phash(ry, w, h);
            let exact = f.y.iter().zip(ry).filter(|(a, b)| a == b).count();
            let mad: f64 = f.y.iter().zip(ry).map(|(&a, &b)| (a as f64 - b as f64).abs()).sum::<f64>() / (w * h) as f64;
            let dist = (phash(&f.y, w, h) ^ ph).count_ones();
            println!("ffmpeg_ref pHash={ph:016x} hamming={dist} luma_exact={}/{} mean_abs_diff={mad:.4}", exact, w * h);
        }
    }
}
