// w05-c: audio header parsers (AC-3, E-AC-3, TrueHD, DTS core, FLAC STREAMINFO),
// crc2 extraction, and Chromaprint fingerprint comparison. No external parser crates.
use std::env;
use std::fs;

struct Bits<'a> { d: &'a [u8], pos: usize }
impl<'a> Bits<'a> {
    fn new(d: &'a [u8]) -> Self { Bits { d, pos: 0 } }
    fn get(&mut self, n: usize) -> u64 {
        let mut v = 0u64;
        for _ in 0..n {
            let byte = self.d.get(self.pos >> 3).copied().unwrap_or(0);
            let bit = (byte >> (7 - (self.pos & 7))) & 1;
            v = (v << 1) | bit as u64;
            self.pos += 1;
        }
        v
    }
}

fn be16(d: &[u8], i: usize) -> u16 { u16::from_be_bytes([d[i], d[i + 1]]) }

fn find_all(d: &[u8], pat: &[u8]) -> Vec<usize> {
    let mut v = Vec::new();
    let mut i = 0;
    while i + pat.len() <= d.len() {
        if &d[i..i + pat.len()] == pat { v.push(i); i += pat.len(); } else { i += 1; }
    }
    v
}

// ---------- AC-3 (ATSC A/52 section 5.3 / 5.4) ----------
const AC3_KBPS: [u32; 19] = [32,40,48,56,64,80,96,112,128,160,192,224,256,320,384,448,512,576,640];
// words per frame indexed by frmsizecod (0..37), for fscod 0=48k,1=44.1k,2=32k
const AC3_WORDS: [[u32; 38]; 3] = [
    [64,64,80,80,96,96,112,112,128,128,160,160,192,192,224,224,256,256,320,320,384,384,448,448,512,512,640,640,768,768,896,896,1024,1024,1152,1152,1280,1280],
    [69,70,87,88,104,105,121,122,139,140,174,175,208,209,243,244,278,279,348,349,417,418,487,488,557,558,696,697,835,836,975,976,1114,1115,1253,1254,1393,1394],
    [96,96,120,120,144,144,168,168,192,192,240,240,288,288,336,336,384,384,480,480,576,576,672,672,768,768,960,960,1152,1152,1344,1344,1536,1536,1728,1728,1920,1920],
];
const AC3_SR: [u32; 3] = [48000, 44100, 32000];

fn acmod_chans(acmod: u64) -> u32 { [2,1,2,3,3,4,4,5][acmod as usize] }

#[derive(Default, Debug)]
struct Ac3Hdr { fscod: u64, frmsizecod: u64, bsid: u64, bsmod: u64, acmod: u64, lfeon: u64,
    dialnorm: u64, sr: u32, kbps: u32, frame_bytes: usize, channels: u32 }

fn parse_ac3(f: &[u8]) -> Option<Ac3Hdr> {
    if f.len() < 8 || be16(f, 0) != 0x0B77 { return None; }
    let mut b = Bits::new(&f[2..]);
    let _crc1 = b.get(16);
    let fscod = b.get(2);
    let frmsizecod = b.get(6);
    let bsid = b.get(5);
    let bsmod = b.get(3);
    let acmod = b.get(3);
    if acmod & 1 != 0 && acmod != 1 { b.get(2); }   // cmixlev
    if acmod & 4 != 0 { b.get(2); }                 // surmixlev
    if acmod == 2 { b.get(2); }                     // dsurmod
    let lfeon = b.get(1);
    let dialnorm = b.get(5);
    let fs = fscod.min(2) as usize;
    let words = AC3_WORDS[fs][(frmsizecod as usize).min(37)];
    Some(Ac3Hdr { fscod, frmsizecod, bsid, bsmod, acmod, lfeon, dialnorm,
        sr: AC3_SR[fs], kbps: AC3_KBPS[(frmsizecod as usize / 2).min(18)],
        frame_bytes: words as usize * 2, channels: acmod_chans(acmod) + lfeon as u32 })
}

// ---------- E-AC-3 (ETSI TS 102 366 Annex E) ----------
#[derive(Default, Debug)]
struct Eac3Hdr { strmtyp: u64, substreamid: u64, frmsiz: u64, fscod: u64, numblks: u64,
    acmod: u64, lfeon: u64, bsid: u64, dialnorm: u64, sr: u32, frame_bytes: usize,
    channels: u32, samples: u32 }

fn parse_eac3(f: &[u8]) -> Option<Eac3Hdr> {
    if f.len() < 8 || be16(f, 0) != 0x0B77 { return None; }
    let mut b = Bits::new(&f[2..]);
    let strmtyp = b.get(2);
    let substreamid = b.get(3);
    let frmsiz = b.get(11);
    let fscod = b.get(2);
    let (sr, numblks) = if fscod == 3 {
        let fscod2 = b.get(2);
        let sr = [24000, 22050, 16000].get(fscod2 as usize).copied().unwrap_or(0);
        (sr, 6)
    } else {
        let nb = b.get(2);
        ([48000, 44100, 32000][fscod as usize & 3], [1u64, 2, 3, 6][nb as usize])
    };
    let acmod = b.get(3);
    let lfeon = b.get(1);
    let bsid = b.get(5);
    let dialnorm = b.get(5);
    Some(Eac3Hdr { strmtyp, substreamid, frmsiz, fscod, numblks, acmod, lfeon, bsid, dialnorm,
        sr, frame_bytes: (frmsiz as usize + 1) * 2, channels: acmod_chans(acmod) + lfeon as u32,
        samples: numblks as u32 * 256 })
}

// ---------- TrueHD / MLP major sync (0xF8726FBA, signature 0xB752) ----------
#[derive(Debug, Default)]
struct ThdHdr { sr_nibble: u8, sr: u32, sig_ok: bool, flags_hex: String, peak_field: u32,
    chmean_hex: String, access_unit_words: u32 }

fn parse_truehd_sync(d: &[u8], sync: usize) -> ThdHdr {
    // access unit header is 4 bytes before the sync: 16-bit length (words) + 16-bit timestamp
    let au = if sync >= 4 { (be16(d, sync - 4) & 0x0FFF) as u32 } else { 0 };
    let fi = &d[sync + 4..sync + 8];           // format_info (32 bits)
    let sr_nibble = fi[0] >> 4;
    let sr = if sr_nibble & 8 != 0 { 44100u32 << (sr_nibble & 7) } else { 48000u32 << (sr_nibble & 7) };
    let sig_ok = be16(d, sync + 8) == 0xB752;
    let flags = be16(d, sync + 10);             // flags: bit15 = VBR-like, low 15 bits = peak rate field
    let chmean = &d[sync + 14..sync + 18];
    ThdHdr { sr_nibble, sr: if sr_nibble == 0 { 48000 } else { sr }, sig_ok,
        flags_hex: format!("{:04X}", flags), peak_field: (flags & 0x7FFF) as u32,
        chmean_hex: chmean.iter().map(|b| format!("{:02X}", b)).collect::<Vec<_>>().join(" "),
        access_unit_words: au }
}

// ---------- DTS core (ETSI TS 102 114 section 5.3) ----------
const DTS_SFREQ: [u32; 16] = [0,8000,16000,32000,0,0,11025,22050,44100,0,0,12000,24000,48000,0,0];
const DTS_RATE: [u32; 25] = [32000,56000,64000,96000,112000,128000,192000,224000,256000,320000,384000,448000,512000,576000,640000,768000,896000,1024000,1152000,1280000,1344000,1408000,1411200,1472000,1536000];
const DTS_CH: [u32; 16] = [1,2,2,2,2,3,3,4,4,5,6,6,6,7,8,8];

#[derive(Default, Debug)]
struct DtsHdr { ftype: u64, nblks: u64, fsize: u64, amode: u64, sfreq_code: u64, sr: u32,
    rate_code: u64, kbps: u32, lff: u64, channels: u32, samples: u32 }

fn parse_dts(f: &[u8]) -> Option<DtsHdr> {
    if f.len() < 16 || f[0..4] != [0x7F, 0xFE, 0x80, 0x01] { return None; }
    let mut b = Bits::new(&f[4..]);
    let ftype = b.get(1);
    let _short = b.get(5);
    let _cpf = b.get(1);
    let nblks = b.get(7);
    let fsize = b.get(14);
    let amode = b.get(6);
    let sfreq_code = b.get(4);
    let rate_code = b.get(5);
    // FixedBit,DYNF,TIMEF,AUXF,HDCD (5 bits), EXT_AUDIO_ID (3), EXT_AUDIO (1), ASPF (1), then LFF (2)
    b.get(1 + 1 + 1 + 1 + 1 + 3 + 1 + 1);
    let lff = b.get(2);
    let kbps = DTS_RATE.get(rate_code as usize).copied().map(|r| r / 1000).unwrap_or(0);
    let ch = DTS_CH.get(amode as usize).copied().unwrap_or(0) + if lff != 0 { 1 } else { 0 };
    Some(DtsHdr { ftype, nblks, fsize, amode, sfreq_code, sr: DTS_SFREQ[sfreq_code as usize & 15],
        rate_code, kbps, lff, channels: ch, samples: (nblks as u32 + 1) * 32 })
}

// ---------- FLAC STREAMINFO ----------
#[derive(Debug)]
struct FlacInfo { min_blk: u16, max_blk: u16, min_frame: u32, max_frame: u32, sr: u32,
    channels: u32, bps: u32, total: u64, md5: String }

fn parse_flac(d: &[u8]) -> Option<FlacInfo> {
    if &d[0..4] != b"fLaC" { return None; }
    let s = &d[8..8 + 34];
    let mut b = Bits::new(s);
    let min_blk = b.get(16) as u16; let max_blk = b.get(16) as u16;
    let min_frame = b.get(24) as u32; let max_frame = b.get(24) as u32;
    let sr = b.get(20) as u32; let channels = b.get(3) as u32 + 1; let bps = b.get(5) as u32 + 1;
    let total = b.get(36);
    let md5 = s[18..34].iter().map(|x| format!("{:02x}", x)).collect();
    Some(FlacInfo { min_blk, max_blk, min_frame, max_frame, sr, channels, bps, total, md5 })
}

// ---------- commands ----------
fn cmd_headers(path: &str) {
    let d = fs::read(path).expect("read");
    println!("## {} ({} bytes)", path, d.len());
    if d.starts_with(b"fLaC") {
        let i = parse_flac(&d).unwrap();
        println!("FLAC streaminfo: {:?}", i);
        return;
    }
    if let Some(h) = find_all(&d, &[0xF8, 0x72, 0x6F, 0xBA]).first().copied() {
        let t = parse_truehd_sync(&d, h);
        println!("TrueHD first major sync @{}: {:?}", h, t);
        println!("TrueHD major sync count: {}", find_all(&d, &[0xF8,0x72,0x6F,0xBA]).len());
        return;
    }
    if let Some(off) = find_all(&d, &[0x7F, 0xFE, 0x80, 0x01]).first().copied() {
        let h = parse_dts(&d[off..]).unwrap();
        println!("DTS core first sync @{}: {:?}", off, h);
        return;
    }
    // AC-3 / E-AC-3: walk frames
    let mut p = 0usize;
    let mut n = 0usize;
    let mut first_ac3 = None; let mut first_e = None;
    let mut ac3_frames = 0usize; let mut e_frames = 0usize;
    let mut e_bytes: u64 = 0;
    let mut e_samples: u64 = 0;
    let mut ac3_bytes: u64 = 0;
    while p + 8 <= d.len() && be16(&d, p) == 0x0B77 {
        let strmtyp = d[p + 2] >> 6;
        if strmtyp == 0 && first_ac3.is_none() && first_e.is_none() {
            // ambiguous: check bsid to decide AC-3 (bsid<=8) vs E-AC-3 (bsid 11..16)
        }
        let bsid_ac3 = (d[p + 5] >> 3) as u64;
        if bsid_ac3 <= 8 {
            let h = parse_ac3(&d[p..]).unwrap();
            if first_ac3.is_none() { first_ac3 = Some(format!("{:?}", h)); }
            ac3_frames += 1; ac3_bytes += h.frame_bytes as u64;
            p += h.frame_bytes; n += 1;
        } else {
            let h = parse_eac3(&d[p..]).unwrap();
            if first_e.is_none() { first_e = Some(format!("{:?}", h)); }
            e_frames += 1; e_bytes += h.frame_bytes as u64; e_samples += h.samples as u64;
            p += h.frame_bytes; n += 1;
        }
        if n > 100000 { break; }
    }
    if let Some(s) = first_ac3 { println!("AC-3 first frame: {}", s); }
    if let Some(s) = first_e { println!("E-AC-3 first frame: {}", s); }
    println!("AC-3 frames walked: {} ({} bytes); E-AC-3 frames: {} ({} bytes, {} samples)", ac3_frames, ac3_bytes, e_frames, e_bytes, e_samples);
    println!("walk stopped at byte {} of {}", p, d.len());
}

// crc2 = last 2 bytes of each AC-3 syncframe (frame walk via BSI frame size)
fn cmd_crc2(path: &str) {
    let d = fs::read(path).expect("read");
    let mut p = 0usize;
    let mut out: Vec<u16> = Vec::new();
    while p + 8 <= d.len() && be16(&d, p) == 0x0B77 {
        let h = match parse_ac3(&d[p..]) { Some(h) => h, None => break };
        if h.frame_bytes == 0 || p + h.frame_bytes > d.len() { break; }
        out.push(be16(&d, p + h.frame_bytes - 2));
        p += h.frame_bytes;
    }
    let hex: Vec<String> = out.iter().map(|x| format!("{:04x}", x)).collect();
    // FNV-1a over the crc2 sequence for a compact equality key
    let mut hsh: u64 = 0xcbf29ce484222325;
    for x in &out { for b in x.to_be_bytes() { hsh ^= b as u64; hsh = hsh.wrapping_mul(0x100000001b3); } }
    println!("crc2 {} frames={} fnv64={:016x} first8={}", path, out.len(), hsh, hex.iter().take(8).cloned().collect::<Vec<_>>().join(" "));
}

// ---------- fingerprint ----------
use rusty_chromaprint::{match_fingerprints, Configuration, Fingerprinter};
use symphonia::core::audio::{AudioBuffer, Signal};
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymErr;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

// decode any container/codec symphonia supports to interleaved stereo i16
fn decode_stereo(path: &str) -> (Vec<i16>, u32, u32, String) {
    let file = fs::File::open(path).expect("open");
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = std::path::Path::new(path).extension().and_then(|e| e.to_str()) { hint.with_extension(ext); }
    let probed = symphonia::default::get_probe().format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default()).expect("probe");
    let mut format = probed.format;
    let track = format.tracks().iter().find(|t| t.codec_params.codec != CODEC_TYPE_NULL).expect("track").clone();
    let codec = format!("{:?}", track.codec_params.codec);
    let sr = track.codec_params.sample_rate.unwrap_or(0);
    let mut dec = symphonia::default::get_codecs().make(&track.codec_params, &DecoderOptions::default()).expect("decoder");
    let mut out: Vec<i16> = Vec::new();
    let mut nch = 0u32;
    loop {
        let pkt = match format.next_packet() {
            Ok(p) => p,
            Err(SymErr::ResetRequired) => break,
            Err(_) => break,
        };
        if pkt.track_id() != track.id { continue; }
        let dec_buf = match dec.decode(&pkt) { Ok(b) => b, Err(_) => continue };
        let spec = *dec_buf.spec();
        nch = spec.channels.count() as u32;
        let mut buf: AudioBuffer<f32> = dec_buf.make_equivalent::<f32>();
        dec_buf.convert(&mut buf);
        let frames = buf.frames();
        let chans: Vec<&[f32]> = (0..nch as usize).map(|c| buf.chan(c)).collect();
        for i in 0..frames {
            let (l, r) = match nch {
                1 => (chans[0][i], chans[0][i]),
                2 => (chans[0][i], chans[1][i]),
                // assume FL FR FC LFE RL/SL RR/SR order (ITU 5.1 downmix, LFE dropped)
                _ => {
                    let fl = chans[0][i]; let fr = chans[1][i]; let fc = chans[2][i];
                    let rl = chans[nch as usize - 2][i]; let rr = chans[nch as usize - 1][i];
                    (fl + 0.707 * fc + 0.707 * rl, fr + 0.707 * fc + 0.707 * rr)
                }
            };
            out.push((l.clamp(-1.0, 1.0) * 32767.0) as i16);
            out.push((r.clamp(-1.0, 1.0) * 32767.0) as i16);
        }
    }
    (out, sr, nch, codec)
}

fn fingerprint(path: &str) -> (Vec<u32>, u32, u32, String, usize) {
    let (pcm, sr, nch, codec) = decode_stereo(path);
    let cfg = Configuration::preset_test2();
    let mut fp = Fingerprinter::new(&cfg);
    fp.start(sr, 2).expect("start");
    for chunk in pcm.chunks(4096) { fp.consume(chunk); }
    fp.finish();
    let v = fp.fingerprint().to_vec();
    let secs = pcm.len() / 2 / sr.max(1) as usize;
    (v, sr, nch, codec, secs)
}

// score: fraction of aligned segment items with bit error < 16/32 avg, plus mean BER over matched items
fn compare(a: &[u32], b: &[u32]) -> String {
    let cfg = Configuration::preset_test2();
    let segs = match match_fingerprints(a, b, &cfg) { Ok(s) => s, Err(e) => return format!("err {:?}", e) };
    let mut items = 0usize; let mut bits = 0u64; let mut diff = 0u64;
    for s in &segs {
        for i in 0..s.items_count {
            let x = a[s.offset1 + i] ^ b[s.offset2 + i];
            diff += x.count_ones() as u64; bits += 32; items += 1;
        }
    }
    let ber = if bits > 0 { diff as f64 / bits as f64 } else { 1.0 };
    let cover = items as f64 / a.len().min(b.len()).max(1) as f64;
    format!("segments={} matched_items={} coverage={:.3} mean_bit_error={:.4} score={:.3}", segs.len(), items, cover, ber, 1.0 - ber)
}

fn cmd_fp(paths: &[String]) {
    let mut fps = Vec::new();
    for p in paths {
        let (v, sr, nch, codec, secs) = fingerprint(p);
        println!("fp {} codec={} sr={} ch_in={} secs={} items={}", p, codec, sr, nch, secs, v.len());
        fps.push((p.clone(), v));
    }
    for i in 1..fps.len() {
        println!("compare {} vs {}: {}", fps[0].0, fps[i].0, compare(&fps[0].1, &fps[i].1));
    }
}

fn main() {
    let args: Vec<String> = env::args().collect();
    match args.get(1).map(|s| s.as_str()) {
        Some("headers") => for p in &args[2..] { cmd_headers(p) },
        Some("crc2") => for p in &args[2..] { cmd_crc2(p) },
        Some("fp") => cmd_fp(&args[2..]),
        _ => eprintln!("usage: atlas_w5c headers|crc2|fp <files>"),
    }
}
