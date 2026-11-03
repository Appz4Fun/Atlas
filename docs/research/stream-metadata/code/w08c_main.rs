// w08c_main.rs - std-only: reach the elementary stream inside TS/M2TS and MP4 (moov / faststart / fragmented)
// usage:  w08c_main ts   <file> <from> [idr_out.bin]   resync at byte offset, PAT/PMT, PES, HEVC NAL split, AC-3 syncframe
//         w08c_main mp4  <file> [kf_out.bin]           moov walk (hvcC, stsz/stsc/stco/co64/stss), keyframe near 50%
//         w08c_main frag <file> <from> [out.bin]       resync on 'moof' + sane size, trun sample 0
use std::collections::BTreeSet;
use std::env;
use std::fs;

const ART: usize = 700_000; // Atlas article size

fn be16(b: &[u8], p: usize) -> u16 { u16::from_be_bytes([b[p], b[p + 1]]) }
fn be32(b: &[u8], p: usize) -> u32 { u32::from_be_bytes([b[p], b[p + 1], b[p + 2], b[p + 3]]) }
fn be64(b: &[u8], p: usize) -> u64 { u64::from_be_bytes(b[p..p + 8].try_into().unwrap()) }
fn arts(s: usize, e: usize) -> BTreeSet<usize> { (s / ART..=(e.max(s + 1) - 1) / ART).collect() }
fn stype_name(t: u8) -> &'static str {
    match t { 0x02 => "MPEG2", 0x1B => "AVC", 0x24 => "HEVC", 0x81 => "AC-3", 0x87 => "E-AC-3",
        0x83 => "TrueHD", 0x86 => "DTS-HD/DTS", 0x06 => "PES-private", _ => "other" }
}

// ---------------- MPEG-TS / M2TS ----------------
// Resync: a packet start p is valid if 0x47 repeats 5x at stride 188 (plain) or 192 (M2TS, sync at +4).
fn resync(b: &[u8], from: usize) -> Option<(usize, usize)> {
    let mut p = from;
    while p + 188 * 5 + 192 <= b.len() {
        for &sz in &[188usize, 192] {
            if (0..5).all(|i| b[p + i * sz + (sz - 188)] == 0x47) { return Some((p, sz)); }
        }
        p += 1;
    }
    None
}

struct Pkt<'a> { pid: u16, pusi: bool, pay: &'a [u8] }

fn pkt(b: &[u8], p: usize, sz: usize) -> Option<Pkt<'_>> {
    let h = p + sz - 188;
    if h + 188 > b.len() || b[h] != 0x47 { return None; }
    let afc = (b[h + 3] >> 4) & 3;
    let mut s = h + 4;
    if afc & 2 != 0 { s += 1 + b[s] as usize; } // adaptation field (PCR, RAI, ...) skipped
    if afc & 1 == 0 || s >= h + 188 { return None; }
    Some(Pkt { pid: ((b[h + 1] as u16 & 0x1f) << 8) | b[h + 2] as u16, pusi: b[h + 1] & 0x40 != 0, pay: &b[s..h + 188] })
}

// PSI section from a PUSI payload (pointer_field first)
fn section(pay: &[u8]) -> Option<&[u8]> {
    let ptr = *pay.first()? as usize;
    let s = pay.get(1 + ptr..)?;
    let len = (((*s.get(1)? & 0x0f) as usize) << 8) | *s.get(2)? as usize;
    s.get(..(3 + len).min(s.len()))
}
fn parse_pat(s: &[u8]) -> Option<u16> {
    let end = s.len().saturating_sub(4);
    let mut i = 8;
    while i + 4 <= end {
        if be16(s, i) != 0 { return Some(((s[i + 2] as u16 & 0x1f) << 8) | s[i + 3] as u16); }
        i += 4;
    }
    None
}
fn parse_pmt(s: &[u8]) -> Vec<(u8, u16)> {
    let mut out = vec![];
    if s.len() < 12 { return out; }
    let pil = ((s[10] as usize & 0x0f) << 8) | s[11] as usize;
    let end = s.len().saturating_sub(4);
    let mut i = 12 + pil;
    while i + 5 <= end {
        let pid = ((s[i + 1] as u16 & 0x1f) << 8) | s[i + 2] as u16;
        out.push((s[i], pid));
        i += 5 + (((s[i + 3] as usize & 0x0f) << 8) | s[i + 4] as usize);
    }
    out
}

// PES header: returns (PTS in 90 kHz ticks, payload offset)
fn pes_hdr(b: &[u8]) -> Option<(Option<u64>, usize)> {
    if b.len() < 9 || b[0..3] != [0, 0, 1] { return None; }
    let hdl = b[8] as usize;
    let pts = if b[7] & 0x80 != 0 && b.len() >= 14 {
        let p = &b[9..14];
        Some((((p[0] as u64 >> 1) & 7) << 30) | ((p[1] as u64) << 22) | (((p[2] as u64 >> 1) & 0x7f) << 15)
            | ((p[3] as u64) << 7) | ((p[4] as u64 >> 1) & 0x7f))
    } else { None };
    Some((pts, 9 + hdl))
}

// Annex B: split on 00 00 01, trim the zero bytes of 4-byte start codes / trailing zeros
fn nals(b: &[u8]) -> Vec<&[u8]> {
    let mut st = vec![];
    let mut i = 0;
    while i + 3 <= b.len() {
        if b[i] == 0 && b[i + 1] == 0 && b[i + 2] == 1 { st.push(i + 3); i += 3; } else { i += 1; }
    }
    (0..st.len()).map(|k| {
        let s = st[k];
        let mut e = if k + 1 < st.len() { st[k + 1] - 3 } else { b.len() };
        while e > s && b[e - 1] == 0 { e -= 1; }
        &b[s..e]
    }).collect()
}

#[derive(Default)]
struct Vid { have: [bool; 3], nal: Vec<u32>, rpu: u32, pics: u32, idr: Option<(usize, Option<u64>, Vec<u8>)> }

fn vid_pes(v: &mut Vid, buf: &[u8], pkt_idx: usize, pts: Option<u64>) {
    for n in nals(buf) {
        if n.is_empty() { continue; }
        let t = ((n[0] >> 1) & 0x3f) as usize;
        v.nal[t] += 1;
        if (32..=34).contains(&t) { v.have[t - 32] = true; }
        if t == 62 { v.rpu += 1; }
        if t <= 9 || (16..=21).contains(&t) { v.pics += 1; }
        if (16..=21).contains(&t) && v.have == [true; 3] && v.idr.is_none() {
            v.idr = Some((pkt_idx, pts, n.to_vec()));
        }
    }
}

// AC-3 syncframe: (offset, sample_rate, kbps, channels incl. LFE, frame bytes)
const AC3_WORDS: [[u16; 3]; 19] = [[64, 69, 96], [80, 87, 120], [96, 104, 144], [112, 121, 168], [128, 139, 192],
    [160, 174, 240], [192, 208, 288], [224, 243, 336], [256, 278, 384], [320, 348, 480], [384, 417, 576],
    [448, 487, 672], [512, 557, 768], [640, 696, 960], [768, 835, 1152], [896, 975, 1344], [1024, 1114, 1536],
    [1152, 1253, 1728], [1280, 1393, 1920]];
fn ac3_parse(b: &[u8]) -> Option<(usize, u32, u32, u32, usize)> {
    let i = (0..b.len().saturating_sub(8)).find(|&i| b[i] == 0x0b && b[i + 1] == 0x77)?;
    let fscod = (b[i + 4] >> 6) as usize;
    let fsc = (b[i + 4] & 0x3f) as usize;
    if fscod > 2 || fsc > 37 { return None; }
    let acmod = (b[i + 6] >> 5) as usize;
    let bit = |bp: usize| (b[bp / 8] >> (7 - bp % 8)) & 1;
    let mut bp = (i + 6) * 8 + 3; // after bsmod(3) acmod(3) bits
    if acmod & 1 != 0 && acmod != 1 { bp += 2; } // cmixlev
    if acmod & 4 != 0 { bp += 2; }               // surmixlev
    if acmod == 2 { bp += 2; }                   // dsurmod
    let lfe = bit(bp) as u32;
    let nch = [2u32, 1, 2, 3, 3, 4, 4, 5][acmod] + lfe;
    let sr = [48000u32, 44100, 32000][fscod];
    let kbps = [32u32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384, 448, 512, 576, 640][fsc / 2];
    Some((i, sr, kbps, nch, AC3_WORDS[fsc / 2][fscod] as usize * 2))
}

fn ts_walk(b: &[u8], from: usize, idr_out: Option<&String>) {
    let (start, sz) = resync(b, from).expect("no TS sync");
    println!("resync from {from}: packet size {sz}, first packet at {start}");
    let (mut pmt_pid, mut vpid, mut apid) = (None::<u16>, None::<u16>, None::<u16>);
    let mut vid = Vid { nal: vec![0; 64], ..Default::default() };
    let (mut vbuf, mut vstart, mut vpts, mut vstarted) = (Vec::<u8>::new(), 0usize, None::<u64>, false);
    let (mut abuf, mut ac3, mut a_done) = (Vec::<u8>::new(), None, false);
    let mut pi = 0usize;
    let mut p = start;
    while p + sz <= b.len() && !(vid.idr.is_some() && a_done) {
        if let Some(k) = pkt(b, p, sz) {
            if k.pid == 0 && k.pusi && pmt_pid.is_none() {
                if let Some(s) = section(k.pay) { pmt_pid = parse_pat(s); }
            } else if Some(k.pid) == pmt_pid && k.pusi && vpid.is_none() {
                if let Some(s) = section(k.pay) {
                    for (st, pid) in parse_pmt(s) {
                        println!("  PMT stream_type 0x{st:02X} ({}) pid 0x{pid:04X}", stype_name(st));
                        match st {
                            0x24 | 0x1B if vpid.is_none() => vpid = Some(pid),
                            0x81 | 0x87 | 0x83 | 0x86 if apid.is_none() => apid = Some(pid),
                            _ => {}
                        }
                    }
                }
            } else if Some(k.pid) == vpid {
                if k.pusi {
                    if vstarted { vid_pes(&mut vid, &vbuf, vstart, vpts); }
                    vbuf.clear();
                    vstarted = true;
                    vstart = pi;
                    if let Some((pts, off)) = pes_hdr(k.pay) {
                        vpts = pts;
                        if off <= k.pay.len() { vbuf.extend_from_slice(&k.pay[off..]); }
                    }
                } else if vstarted { vbuf.extend_from_slice(k.pay); }
            } else if Some(k.pid) == apid && !a_done {
                let mut body = k.pay;
                if k.pusi { if let Some((_, off)) = pes_hdr(k.pay) { if off <= k.pay.len() { body = &k.pay[off..]; } } }
                abuf.extend_from_slice(body);
                if let Some(info) = ac3_parse(&abuf) { ac3 = Some(info); a_done = true; }
            }
        }
        p += sz;
        pi += 1;
    }
    println!("video pid {:?}  audio pid {:?}  packets walked {pi}", vpid, apid);
    let names = [(32, "VPS"), (33, "SPS"), (34, "PPS"), (39, "SEI pre"), (40, "SEI suf"), (62, "DV RPU"), (19, "IDR_W_RADL"), (20, "IDR_N_LP"), (21, "CRA")];
    let counts: Vec<String> = names.iter().filter(|(t, _)| vid.nal[*t] > 0).map(|(t, n)| format!("{n}={}", vid.nal[*t])).collect();
    println!("HEVC NALs in walked part: {}  pics={} (have VPS/SPS/PPS = {:?})", counts.join(" "), vid.pics, vid.have);
    match &vid.idr {
        Some((k, pts, nal)) => {
            println!("first decodable IDR: PES/TS packet index {k}, abs byte offset {} ({} bytes after resync), pts {:?}, NAL len {}",
                start + k * sz, k * sz, pts, nal.len());
            println!("  NAL header bytes {:02X?}", &nal[..2.min(nal.len())]);
            if let Some(o) = idr_out { fs::write(o, nal).unwrap(); }
        }
        None => println!("no decodable IDR found"),
    }
    match ac3 {
        Some((off, sr, kbps, nch, fb)) => println!("AC-3 first syncframe at buffer offset {off}: {sr} Hz, {kbps} kbps, {nch} ch, {fb} bytes/frame"),
        None => println!("no AC-3 syncframe found"),
    }
}

// ---------------- ISO BMFF (MP4) ----------------
#[derive(Clone, Copy, Debug)]
struct BX { t: [u8; 4], s: usize, body: usize, e: usize }

fn boxes(b: &[u8], s: usize, e: usize) -> Vec<BX> {
    let mut v = vec![];
    let mut p = s;
    while p + 8 <= e {
        let mut sz = be32(b, p) as usize;
        let t = [b[p + 4], b[p + 5], b[p + 6], b[p + 7]];
        let mut hdr = 8;
        if sz == 1 { sz = be64(b, p + 8) as usize; hdr = 16; } else if sz == 0 { sz = e - p; }
        if sz < hdr || p + sz > e { break; }
        v.push(BX { t, s: p, body: p + hdr, e: p + sz });
        p += sz;
    }
    v
}
fn kids(b: &[u8], s: usize, e: usize, t: &[u8; 4]) -> Option<BX> { boxes(b, s, e).into_iter().find(|x| &x.t == t) }
fn child(b: &[u8], p: BX, t: &[u8; 4]) -> Option<BX> { kids(b, p.body, p.e, t) }

struct VT { ts: u32, w: u16, h: u16, hvcc: Option<(usize, usize)>, entry_kids: Vec<String>, sizes: Vec<u32>, sc: Vec<(u32, u32)>, chunk_off: Vec<u64>, stss: Vec<u32> }

fn video_trak(b: &[u8], moov: BX) -> Option<VT> {
    for tk in boxes(b, moov.body, moov.e).into_iter().filter(|x| &x.t == b"trak") {
        let mdia = child(b, tk, b"mdia")?;
        let hdlr = child(b, mdia, b"hdlr")?;
        if &b[hdlr.body + 8..hdlr.body + 12] != b"vide" { continue; }
        let mdhd = child(b, mdia, b"mdhd")?;
        let ts = if b[mdhd.body] == 1 { be32(b, mdhd.body + 20) } else { be32(b, mdhd.body + 12) };
        let stbl = child(b, child(b, mdia, b"minf")?, b"stbl")?;
        let stsd = child(b, stbl, b"stsd")?;
        let ent = boxes(b, stsd.body + 8, stsd.e)[0]; // first VisualSampleEntry (hvc1/hev1)
        let ek = boxes(b, ent.body + 78, ent.e).iter().map(|x| String::from_utf8_lossy(&x.t).into_owned()).collect();
        let hvcc = kids(b, ent.body + 78, ent.e, b"hvcC").map(|x| (x.body, x.e));
        let stsz = child(b, stbl, b"stsz")?;
        let (fixed, n) = (be32(b, stsz.body + 4), be32(b, stsz.body + 8) as usize);
        let sizes: Vec<u32> = if fixed != 0 { vec![fixed; n] } else { (0..n).map(|i| be32(b, stsz.body + 12 + 4 * i)).collect() };
        let stsc = child(b, stbl, b"stsc")?;
        let ne = be32(b, stsc.body + 4) as usize;
        let sc = (0..ne).map(|i| (be32(b, stsc.body + 8 + 12 * i), be32(b, stsc.body + 12 + 12 * i))).collect();
        let (co, is64) = match child(b, stbl, b"co64") { Some(x) => (x, true), None => (child(b, stbl, b"stco")?, false) };
        let nc = be32(b, co.body + 4) as usize;
        let chunk_off = (0..nc).map(|i| if is64 { be64(b, co.body + 8 + 8 * i) } else { be32(b, co.body + 8 + 4 * i) as u64 }).collect();
        let stss = match child(b, stbl, b"stss") {
            Some(x) => (0..be32(b, x.body + 4) as usize).map(|i| be32(b, x.body + 8 + 4 * i)).collect(),
            None => vec![],
        };
        return Some(VT { ts, w: be16(b, ent.body + 24), h: be16(b, ent.body + 26), hvcc, entry_kids: ek, sizes, sc, chunk_off, stss });
    }
    None
}

// absolute file offset and size of 0-based sample n, from stsc + stco/co64
fn sample_loc(t: &VT, n: usize) -> Option<(u64, u32)> {
    let mut s0 = 0usize;
    for c in 0..t.chunk_off.len() {
        let spc = t.sc.iter().rev().find(|&&(fc, _)| (fc as usize) <= c + 1)?.1 as usize;
        if n < s0 + spc {
            let mut off = t.chunk_off[c];
            for k in s0..n { off += t.sizes[k] as u64; }
            return Some((off, t.sizes[n]));
        }
        s0 += spc;
    }
    None
}

fn mp4_walk(b: &[u8], out: Option<&String>) {
    let top = boxes(b, 0, b.len());
    let tl: Vec<String> = top.iter().map(|x| format!("{}@{}({})", String::from_utf8_lossy(&x.t), x.s, x.e - x.s)).collect();
    println!("top level: {}", tl.join(" "));
    let moov = *top.iter().find(|x| &x.t == b"moov").expect("no moov");
    let vt = video_trak(b, moov).expect("no vide trak");
    println!("video {}x{} timescale {} samples {} sync samples {} ; sample entry children {:?}", vt.w, vt.h, vt.ts, vt.sizes.len(), vt.stss.len(), vt.entry_kids);
    if let Some((s, e)) = vt.hvcc {
        let h = &b[s..e];
        let nlen = (h[21] & 3) + 1;
        let mut i = 23;
        let mut arrays = vec![];
        for _ in 0..h[22] {
            let t = h[i] & 0x3f; let n = u16::from_be_bytes([h[i + 1], h[i + 2]]); i += 3;
            for _ in 0..n { let l = u16::from_be_bytes([h[i], h[i + 1]]) as usize; i += 2 + l; }
            arrays.push(t);
        }
        println!("hvcC: lengthSize {nlen}, param-set NAL types {:?}, {} bytes", arrays, e - s);
    }
    let mid = vt.sizes.len() / 2;
    let kf = {
        let k = vt.stss.partition_point(|&s| (s as usize) - 1 < mid).min(vt.stss.len() - 1);
        vt.stss[k] as usize - 1
    };
    let (koff, ksz) = sample_loc(&vt, kf).expect("sample loc");
    let (ka, kb) = (arts(koff as usize, koff as usize + ksz as usize), arts(moov.s, moov.e));
    println!("moov @{} len {}  -> articles for moov {:?}", moov.s, moov.e - moov.s, kb);
    println!("keyframe sample {kf} (~50% = sample {mid}) @{koff} len {ksz}, article {:?}", ka);
    println!("article count (moov + keyframe, distinct) = {}", ka.union(&kb).count());
    if let Some(o) = out { fs::write(o, &b[koff as usize..koff as usize + ksz as usize]).unwrap(); }
}

// fragmented: find a moof at an arbitrary offset (type at p, size sane, next box mdat/moof/sidx)
fn find_moof(b: &[u8], from: usize) -> Option<usize> {
    let mut p = from.max(4);
    while p + 8 <= b.len() {
        if &b[p..p + 4] == b"moof" {
            let s = p - 4;
            let sz = be32(b, s) as usize;
            if (16..=1 << 24).contains(&sz) && s + sz + 8 <= b.len() && matches!(&b[s + sz + 4..s + sz + 8], b"mdat" | b"moof" | b"sidx") {
                return Some(s);
            }
        }
        p += 1;
    }
    None
}

fn frag_walk(b: &[u8], from: usize, out: Option<&String>) {
    let m = find_moof(b, from).expect("no moof");
    let msz = be32(b, m) as usize;
    let moof = BX { t: *b"moof", s: m, body: m + 8, e: m + msz };
    let mut sample0 = None;
    for traf in boxes(b, moof.body, moof.e).into_iter().filter(|x| &x.t == b"traf") {
        let tfhd = child(b, traf, b"tfhd").expect("tfhd");
        let tf = be32(b, tfhd.body) & 0xff_ffff;
        let mut q = tfhd.body + 8;
        let base = if tf & 1 != 0 { let v = be64(b, q); q += 8; Some(v) } else { None };
        if tf & 2 != 0 { q += 4; }
        if tf & 8 != 0 { q += 4; }
        let dflt_size = if tf & 0x10 != 0 { be32(b, q) } else { 0 };
        let trun = child(b, traf, b"trun").expect("trun");
        let rf = be32(b, trun.body) & 0xff_ffff;
        let n = be32(b, trun.body + 4) as usize;
        let mut q = trun.body + 8;
        let doff = if rf & 1 != 0 { let v = be32(b, q) as i32; q += 4; Some(v) } else { None };
        let fflags = if rf & 4 != 0 { let v = be32(b, q); q += 4; Some(v) } else { None };
        let mut sizes = vec![];
        for i in 0..n {
            if rf & 0x100 != 0 { q += 4; }
            sizes.push(if rf & 0x200 != 0 { let v = be32(b, q); q += 4; v } else { dflt_size });
            if rf & 0x400 != 0 { q += 4; }
            if rf & 0x800 != 0 { q += 4; }
            let _ = i;
        }
        let first_flags = fflags.unwrap_or(0);
        let base_off = base.unwrap_or(m as u64) as i64;
        let first = base_off + doff.unwrap_or(0) as i64;
        println!("moof @{m} size {msz} (article {}), tfhd flags 0x{tf:X} base {:?} default-base-is-moof {}", m / ART, base, tf & 0x20000 != 0);
        println!("trun samples {n} data_offset {:?} first_sample_flags {:?} sync {}", doff, fflags, first_flags & 0x1_0000 == 0);
        println!("sample 0 @{first} size {} ; sample sizes sum {}", sizes[0], sizes.iter().map(|&x| x as u64).sum::<u64>());
        sample0 = Some((first as usize, sizes[0] as usize));
        break;
    }
    let (s0, z0) = sample0.expect("no traf");
    let head = arts(0, 1); // init segment (ftyp+moov) at file start
    let need = head.union(&arts(m, s0 + z0)).count();
    println!("article count: init(ftyp+moov) article 0 + moof..sample0 = {need} (moof+sample0 span {} bytes)", s0 + z0 - m);
    if let Some(o) = out { fs::write(o, &b[s0..s0 + z0]).unwrap(); }
}

fn main() {
    let a: Vec<String> = env::args().collect();
    if a.len() < 3 { eprintln!("usage: w08c_main ts|mp4|frag <file> [from] [out]"); std::process::exit(2); }
    let b = fs::read(&a[2]).expect("read");
    match a[1].as_str() {
        "ts" => ts_walk(&b, a[3].parse().unwrap(), a.get(4)),
        "mp4" => mp4_walk(&b, a.get(3)),
        "frag" => frag_walk(&b, a[3].parse().unwrap(), a.get(4)),
        _ => eprintln!("unknown mode"),
    }
}
