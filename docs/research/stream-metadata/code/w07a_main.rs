// w07-a: AC-3 decode (oxideav-ac3) -> Chromaprint vs FLAC reference; full AC-3 BSI field dump;
// E-AC-3 substream + chanmap walk. Header parsers reused from w05c_main.rs.
use std::env;
use std::fs;

use oxideav_core::{CodecId, CodecParameters, Frame, Packet, TimeBase};
use rusty_chromaprint::{match_fingerprints, Configuration, Fingerprinter};
use symphonia::core::audio::{AudioBuffer, Signal};
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymErr;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

struct Bits<'a> { d: &'a [u8], pos: usize }
impl<'a> Bits<'a> {
    fn new(d: &'a [u8]) -> Self { Bits { d, pos: 0 } }
    fn get(&mut self, n: usize) -> u64 {
        let mut v = 0u64;
        for _ in 0..n {
            let byte = self.d.get(self.pos >> 3).copied().unwrap_or(0);
            v = (v << 1) | ((byte >> (7 - (self.pos & 7))) & 1) as u64;
            self.pos += 1;
        }
        v
    }
}
fn be16(d: &[u8], i: usize) -> u16 { u16::from_be_bytes([d[i], d[i + 1]]) }

const AC3_KBPS: [u32; 19] = [32,40,48,56,64,80,96,112,128,160,192,224,256,320,384,448,512,576,640];
const AC3_WORDS: [[u32; 38]; 3] = [
    [64,64,80,80,96,96,112,112,128,128,160,160,192,192,224,224,256,256,320,320,384,384,448,448,512,512,640,640,768,768,896,896,1024,1024,1152,1152,1280,1280],
    [69,70,87,88,104,105,121,122,139,140,174,175,208,209,243,244,278,279,348,349,417,418,487,488,557,558,696,697,835,836,975,976,1114,1115,1253,1254,1393,1394],
    [96,96,120,120,144,144,168,168,192,192,240,240,288,288,336,336,384,384,480,480,576,576,672,672,768,768,960,960,1152,1152,1344,1344,1536,1536,1728,1728,1920,1920],
];
const AC3_SR: [u32; 3] = [48000, 44100, 32000];
fn acmod_chans(acmod: u64) -> u32 { [2,1,2,3,3,4,4,5][acmod as usize & 7] }

// Full BSI per A/52 5.4.2 (+ Annex D xbsi1/xbsi2 when bsid==6). Raw codes, not dB.
#[derive(Default, Debug)]
struct Ac3Bsi {
    fscod: u64, frmsizecod: u64, bsid: u64, bsmod: u64, acmod: u64, lfeon: u64,
    cmixlev: Option<u64>, surmixlev: Option<u64>, dsurmod: Option<u64>,
    dialnorm: u64, compr: Option<u64>, langcod: Option<u64>,
    mixlevel: Option<u64>, roomtyp: Option<u64>, adconvtyp: Option<u64>,
    copyright: u64, origbs: u64,
    dmixmod: Option<u64>, ltrtcmixlev: Option<u64>, ltrtsurmixlev: Option<u64>,
    lorocmixlev: Option<u64>, lorosurmixlev: Option<u64>,
    dsurexmod: Option<u64>, dheadphonmod: Option<u64>, xbsi2_adconv: Option<u64>,
    addbsi_len: Option<u64>, tail_bits: String, frame_bytes: usize, sr: u32, kbps: u32,
}

fn parse_ac3(f: &[u8]) -> Option<Ac3Bsi> {
    if f.len() < 8 || be16(f, 0) != 0x0B77 { return None; }
    let mut b = Bits::new(&f[2..]);
    let _crc1 = b.get(16);
    let fscod = b.get(2); let frmsizecod = b.get(6); let bsid = b.get(5);
    let bsmod = b.get(3); let acmod = b.get(3);
    let mut h = Ac3Bsi { fscod, frmsizecod, bsid, bsmod, acmod, ..Default::default() };
    if acmod & 1 != 0 && acmod != 1 { h.cmixlev = Some(b.get(2)); }
    if acmod & 4 != 0 { h.surmixlev = Some(b.get(2)); }
    if acmod == 2 { h.dsurmod = Some(b.get(2)); }
    h.lfeon = b.get(1);
    h.dialnorm = b.get(5);
    if b.get(1) == 1 { h.compr = Some(b.get(8)); }
    if b.get(1) == 1 { h.langcod = Some(b.get(8)); }
    if b.get(1) == 1 { h.mixlevel = Some(b.get(5)); h.roomtyp = Some(b.get(2)); h.adconvtyp = Some(b.get(1)); }
    if acmod == 0 {
        b.get(5); // dialnorm2
        if b.get(1) == 1 { b.get(8); }
        if b.get(1) == 1 { b.get(8); }
        if b.get(1) == 1 { b.get(5); b.get(2); b.get(1); }
    }
    h.copyright = b.get(1); h.origbs = b.get(1);
    let tail_pos = b.pos;
    if bsid == 6 {
        if b.get(1) == 1 {
            h.dmixmod = Some(b.get(2)); h.ltrtcmixlev = Some(b.get(3)); h.ltrtsurmixlev = Some(b.get(3));
            h.lorocmixlev = Some(b.get(3)); h.lorosurmixlev = Some(b.get(3));
        }
        if b.get(1) == 1 {
            h.dsurexmod = Some(b.get(2)); h.dheadphonmod = Some(b.get(2));
            h.xbsi2_adconv = Some(b.get(1)); b.get(8); b.get(1);
        }
    }
    if b.get(1) == 1 { b.get(14); }
    if b.get(1) == 1 { b.get(14); }
    if b.get(1) == 1 { h.addbsi_len = Some(b.get(6)); }
    let fs = fscod.min(2) as usize;
    h.tail_bits = (0..64).map(|i| { let q = tail_pos + i; let byte = f[2 + (q >> 3)]; if (byte >> (7 - (q & 7))) & 1 == 1 { '1' } else { '0' } }).collect();
    h.frame_bytes = AC3_WORDS[fs][(frmsizecod as usize).min(37)] as usize * 2;
    h.sr = AC3_SR[fs];
    h.kbps = AC3_KBPS[(frmsizecod as usize / 2).min(18)];
    Some(h)
}

#[derive(Default, Debug)]
struct Eac3Hdr {
    strmtyp: u64, substreamid: u64, frmsiz: u64, fscod: u64, numblks: u64, acmod: u64, lfeon: u64,
    bsid: u64, dialnorm: u64, chanmape: Option<u64>, chanmap: Option<u64>, frame_bytes: usize,
}
fn parse_eac3(f: &[u8]) -> Option<Eac3Hdr> {
    if f.len() < 8 || be16(f, 0) != 0x0B77 { return None; }
    let mut b = Bits::new(&f[2..]);
    let strmtyp = b.get(2); let substreamid = b.get(3); let frmsiz = b.get(11);
    let fscod = b.get(2);
    let numblks = if fscod == 3 { b.get(2); 6 } else { [1u64, 2, 3, 6][b.get(2) as usize] };
    let acmod = b.get(3); let lfeon = b.get(1); let bsid = b.get(5); let dialnorm = b.get(5);
    if b.get(1) == 1 { b.get(8); }
    if acmod == 0 { b.get(5); if b.get(1) == 1 { b.get(8); } }
    let mut h = Eac3Hdr { strmtyp, substreamid, frmsiz, fscod, numblks, acmod, lfeon, bsid, dialnorm,
        frame_bytes: (frmsiz as usize + 1) * 2, ..Default::default() };
    if strmtyp == 1 {
        let e = b.get(1); h.chanmape = Some(e);
        if e == 1 { h.chanmap = Some(b.get(16)); }
    }
    Some(h)
}

// Table E2.5 chanmap bit names (bit 15 = MSB). Bits 4..0 not named here.
fn chanmap_names(m: u64) -> String {
    const N: [(u32, &str); 11] = [(15,"Lc/Rc"),(14,"Lrs/Rrs"),(13,"Cs"),(12,"Ts"),(11,"Lsd/Rsd"),(10,"Lw/Rw"),
        (9,"Vhl/Vhr"),(8,"Vhc"),(7,"Lts/Rts"),(6,"LFE2"),(5,"LFE")];
    let v: Vec<&str> = N.iter().filter(|(bit, _)| m >> bit & 1 == 1).map(|(_, s)| *s).collect();
    if v.is_empty() { "none".into() } else { v.join("+") }
}

// ---------- decode AC-3 via oxideav-ac3 ----------
fn decode_ac3(d: &[u8]) -> (Vec<i16>, usize, String) {
    let mut params = CodecParameters::audio(CodecId::new("ac3"));
    params.channels = Some(2);
    params.sample_rate = Some(48000);
    let mut dec = oxideav_ac3::decoder::make_decoder(&params).expect("make_decoder");
    let tb = TimeBase::new(1, 48000);
    let (mut out, mut frames, mut errs, mut first_err) = (Vec::new(), 0usize, 0usize, String::new());
    let mut p = 0usize;
    while p + 8 <= d.len() && be16(d, p) == 0x0B77 {
        let h = match parse_ac3(&d[p..]) { Some(h) => h, None => break };
        if h.frame_bytes == 0 || p + h.frame_bytes > d.len() { break; }
        let pkt = Packet::new(0, tb, d[p..p + h.frame_bytes].to_vec());
        p += h.frame_bytes;
        if let Err(e) = dec.send_packet(&pkt) { errs += 1; if first_err.is_empty() { first_err = format!("send {e:?}"); } continue; }
        match dec.receive_frame() {
            Ok(Frame::Audio(a)) => {
                frames += 1;
                for c in a.data[0].chunks_exact(2) { out.push(i16::from_le_bytes([c[0], c[1]])); }
            }
            Ok(_) => {}
            Err(e) => { errs += 1; if first_err.is_empty() { first_err = format!("recv {e:?}"); } }
        }
    }
    (out, frames, format!("errs={errs} {first_err}"))
}

// ---------- FLAC/other reference decode via symphonia 0.5 (from w05c) ----------
fn decode_ref(path: &str) -> Vec<i16> {
    let file = fs::File::open(path).expect("open");
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = std::path::Path::new(path).extension().and_then(|e| e.to_str()) { hint.with_extension(ext); }
    let probed = symphonia::default::get_probe().format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default()).expect("probe");
    let mut format = probed.format;
    let track = format.tracks().iter().find(|t| t.codec_params.codec != CODEC_TYPE_NULL).expect("track").clone();
    let mut dec = symphonia::default::get_codecs().make(&track.codec_params, &DecoderOptions::default()).expect("decoder");
    let mut out = Vec::new();
    loop {
        let pkt = match format.next_packet() { Ok(p) => p, Err(SymErr::ResetRequired) => break, Err(_) => break };
        if pkt.track_id() != track.id { continue; }
        let db = match dec.decode(&pkt) { Ok(b) => b, Err(_) => continue };
        let mut buf: AudioBuffer<f32> = db.make_equivalent::<f32>();
        db.convert(&mut buf);
        let n = buf.spec().channels.count();
        for i in 0..buf.frames() {
            for c in 0..2usize.min(n) {
                let v = buf.chan(c)[i];
                out.push((v.clamp(-1.0, 1.0) * 32767.0) as i16);
            }
        }
    }
    out
}

fn fingerprint(pcm: &[i16]) -> Vec<u32> {
    let cfg = Configuration::preset_test2();
    let mut f = Fingerprinter::new(&cfg);
    f.start(48000, 2).expect("start");
    for c in pcm.chunks(4096) { f.consume(c); }
    f.finish();
    f.fingerprint().to_vec()
}

// item = one chromaprint hop; ~0.1238 s at test2 config
const ITEM_S: f64 = 0.12372;

// returns (score, coverage, first segment (item offset in a, item offset in b, items))
fn compare(a: &[u32], b: &[u32]) -> (Option<f64>, f64, Option<(usize, usize, usize)>) {
    let cfg = Configuration::preset_test2();
    let segs = match match_fingerprints(a, b, &cfg) { Ok(s) => s, Err(_) => return (None, 0.0, None) };
    let (mut items, mut diff, mut bits) = (0usize, 0u64, 0u64);
    let mut first = None;
    for s in &segs {
        if first.is_none() { first = Some((s.offset1, s.offset2, s.items_count)); }
        for i in 0..s.items_count {
            diff += (a[s.offset1 + i] ^ b[s.offset2 + i]).count_ones() as u64;
            bits += 32; items += 1;
        }
    }
    if items == 0 { return (Some(0.0), 0.0, first); }
    let ber = diff as f64 / bits as f64;
    (Some(1.0 - ber), items as f64 / a.len().min(b.len()).max(1) as f64, first)
}

fn fmt_score(r: (Option<f64>, f64, Option<(usize, usize, usize)>)) -> String {
    match r {
        (Some(s), c, Some((o1, o2, n))) => format!("score={:.3} cov={:.2} seg(a@{:.2}s,b@{:.2}s,{} items)", s, c, o1 as f64 * ITEM_S, o2 as f64 * ITEM_S, n),
        (Some(s), c, None) => format!("score={:.3} cov={:.2} no-seg", s, c),
        _ => "match error (too short?)".into(),
    }
}

fn cmd_fp(ac3: &str, reff: &str, shift_ac3: &str, shift_ref: &str, unrel: &str) {
    let refp = decode_ref(reff);
    let (dec, frames, st) = decode_ac3(&fs::read(ac3).unwrap());
    println!("ref {} samples={} ({:.2}s)", reff, refp.len() / 2, refp.len() as f64 / 96000.0);
    println!("ac3 {} decoded_frames={} pcm_s={} ({:.2}s) {}", ac3, frames, dec.len() / 2, dec.len() as f64 / 96000.0, st);
    let fr = fingerprint(&refp);
    let fd = fingerprint(&dec);
    println!("full: {}", fmt_score(compare(&fr, &fd)));
    // windows (seconds): start, length
    for &len in &[5.0f64, 10.0, 20.0] {
        for &start in &[0.0f64, 5.0, 10.0] {
            if start + len > 30.0 { continue; }
            let s0 = (start * 96000.0) as usize; let s1 = s0 + (len * 96000.0) as usize;
            let (w_ref, w_dec) = (fingerprint(&refp[s0..s1.min(refp.len())]), fingerprint(&dec[s0..s1.min(dec.len())]));
            println!("win len={:>4.1}s start={:>4.1}s: {}", len, start, fmt_score(compare(&w_ref, &w_dec)));
        }
    }
    // time-shifted: AC-3 encodes source from +3.2 s; compare against the same-shift FLAC
    let (sdec, sframes, sst) = decode_ac3(&fs::read(shift_ac3).unwrap());
    let sref = decode_ref(shift_ref);
    println!("shift ac3 frames={} {} ; shift_ref samples={}", sframes, sst, sref.len() / 2);
    println!("shift full(ref@0 vs ac3@+3.2s): {}", fmt_score(compare(&fr, &fingerprint(&sdec))));
    println!("shift full(ref@+3.2 vs ac3@+3.2s): {}", fmt_score(compare(&fingerprint(&sref), &fingerprint(&sdec))));
    // unrelated clip
    let (udec, uframes, ust) = decode_ac3(&fs::read(unrel).unwrap());
    println!("unrelated ac3 frames={} {}", uframes, ust);
    println!("unrelated full: {}", fmt_score(compare(&fr, &fingerprint(&udec))));
    let u5 = fingerprint(&udec[..(5.0 * 96000.0) as usize]);
    let r5 = fingerprint(&refp[..(5.0 * 96000.0) as usize]);
    println!("unrelated win 5s: {}", fmt_score(compare(&r5, &u5)));
}

// ---------- BSI dump ----------
fn cmd_bsi(path: &str) {
    let d = fs::read(path).unwrap();
    let h = parse_ac3(&d).expect("no AC-3 syncframe");
    println!("## {} frame0 bytes={} {:?}", path, h.frame_bytes, h);
}

// ---------- E-AC-3 / AC-3 frame walk with substream summary ----------
fn cmd_eac3(path: &str) {
    let d = fs::read(path).unwrap();
    let (mut p, mut n0, mut n1, mut ac3n) = (0usize, 0usize, 0usize, 0usize);
    let mut first0 = String::new(); let mut first1 = String::new();
    let mut chanmaps: Vec<u64> = Vec::new();
    while p + 8 <= d.len() && be16(&d, p) == 0x0B77 {
        let bsid = (d[p + 5] >> 3) as u64;
        if bsid <= 8 {
            let h = parse_ac3(&d[p..]).unwrap(); ac3n += 1; p += h.frame_bytes; continue;
        }
        let h = parse_eac3(&d[p..]).unwrap();
        if h.strmtyp == 0 { n0 += 1; if n0 == 1 { first0 = format!("{:?}", h); } }
        else if h.strmtyp == 1 {
            n1 += 1;
            if let Some(m) = h.chanmap { if !chanmaps.contains(&m) { chanmaps.push(m); } }
            if n1 == 1 { first1 = format!("{:?} chanmap_names={}", h, h.chanmap.map(chanmap_names).unwrap_or_default()); }
        }
        if h.frame_bytes == 0 { break; }
        p += h.frame_bytes;
    }
    println!("## {} ac3_frames={} eac3_indep(strmtyp0)={} eac3_dep(strmtyp1)={} stop@{}/{}", path, ac3n, n0, n1, p, d.len());
    println!("first indep: {}", first0);
    println!("first dep: {}", first1);
    println!("distinct chanmaps: {:?}", chanmaps.iter().map(|m| format!("0x{:04x}={}", m, chanmap_names(*m))).collect::<Vec<_>>());
}

fn main() {
    let args: Vec<String> = env::args().collect();
    match args.get(1).map(|s| s.as_str()) {
        Some("fp") => cmd_fp(&args[2], &args[3], &args[4], &args[5], &args[6]),
        Some("bsi") => for p in &args[2..] { cmd_bsi(p) },
        Some("eac3") => for p in &args[2..] { cmd_eac3(p) },
        _ => eprintln!("usage: w07a fp|bsi|eac3 ..."),
    }
}
