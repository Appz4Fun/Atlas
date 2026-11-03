// w06-b: TrueHD / MLP major sync parser (port of FFmpeg mlp_parse.c + mlp.c checksum16),
// std only. Usage: w06b_main <raw.thd>
use std::env;
use std::fs;

// MLP CRC-16: poly 0x2D, MSB-first, init 0 (av_crc crc_2D, le=0)
fn crc16_2d(buf: &[u8]) -> u16 {
    let mut crc: u16 = 0;
    for &b in buf {
        crc ^= (b as u16) << 8;
        for _ in 0..8 { crc = if crc & 0x8000 != 0 { (crc << 1) ^ 0x002D } else { crc << 1 }; }
    }
    crc
}

// ff_mlp_checksum16(buf, n): crc over buf[0..n-4] XOR LE16(buf[n-4..n-2]); caller compares with LE16(buf[n-2..n])
fn mlp_checksum16(buf: &[u8], n: usize) -> u16 {
    crc16_2d(&buf[..n - 4]) ^ u16::from_le_bytes([buf[n - 4], buf[n - 3]])
}

struct Bits<'a> { d: &'a [u8], pos: usize }
impl<'a> Bits<'a> {
    fn get(&mut self, n: usize) -> u32 {
        let mut v = 0u32;
        for _ in 0..n {
            let byte = self.d.get(self.pos >> 3).copied().unwrap_or(0);
            v = (v << 1) | ((byte >> (7 - (self.pos & 7))) & 1) as u32;
            self.pos += 1;
        }
        v
    }
}

fn samplerate(code: u32) -> u32 { if code == 0xF { 0 } else { (if code & 8 != 0 { 44100 } else { 48000 }) << (code & 7) } }

// thd_chancount / thd_layout order from mlp_parse.h (bit i of the 13-bit channel_meaning field)
const THD_CH: [(u32, &str); 13] = [
    (2, "L R"), (1, "C"), (1, "LFE"), (2, "Ls Rs"), (2, "Ltop Rtop"), (2, "Lc Rc"), (2, "Lrs Rrs"),
    (1, "Cs"), (1, "Ts"), (2, "Lsd Rsd"), (2, "Lw Rw"), (1, "Cvh"), (1, "LFE2"),
];
fn chanmap_channels(m: u32) -> u32 { (0..13).map(|i| THD_CH[i].0 * ((m >> i) & 1)).sum() }
fn chanmap_names(m: u32) -> String {
    (0..13).filter(|i| (m >> i) & 1 == 1).map(|i| THD_CH[i].1).collect::<Vec<_>>().join(" ")
}

#[derive(Debug, Default)]
struct Mlp {
    off: usize, au_bytes: usize, crc_ok: bool, sig_ok: bool, header_size: usize,
    stream_type: u8, ratebits: u32, sr: u32, access_unit_size: u32,
    mod0: u32, mod1: u32, mod2: u32, ch6_mask: u32, ch8_mask: u32,
    is_vbr: u32, peak_field: u32, peak_bps: u64, num_substreams: u32, ext_sub: u32, substream_info: u32,
}

fn parse_sync(d: &[u8], s: usize, au_bytes: usize) -> Option<Mlp> {
    let b = &d[s..];
    if b.len() < 28 || b[0..4] != [0xF8, 0x72, 0x6F, 0xBA] { return None; }
    let mut hs = 28;
    if b[25] & 1 != 0 { hs += 2 + 2 * (b[26] >> 4) as usize; }
    if b.len() < hs { return None; }
    let crc_ok = crc16_2d(&b[..hs - 4]) == u16::from_be_bytes([b[hs - 2], b[hs - 1]]);
    let crc_ok_ffmpeg_le = mlp_checksum16(b, hs) == u16::from_le_bytes([b[hs - 2], b[hs - 1]]);
    let _ = crc_ok_ffmpeg_le;
    let mut r = Bits { d: b, pos: 32 }; // after 0xF8726F + stream_type byte
    let ratebits = r.get(4);
    r.get(4);                             // skip
    let mod0 = r.get(2); let mod1 = r.get(2);
    let arr6 = r.get(5);
    let mod2 = r.get(2);
    let arr8 = r.get(13);
    r.pos += 48;                          // 0xB752 signature (16), flags/reserved (32)
    let is_vbr = r.get(1);
    let peak_field = r.get(15);
    let sr = samplerate(ratebits);
    let peak_bps = ((peak_field as u64) * sr as u64 + 8) >> 4;
    let num_substreams = r.get(4);
    r.get(2);
    let ext_sub = r.get(2);
    let substream_info = r.get(8);
    Some(Mlp {
        off: s, au_bytes, crc_ok, sig_ok: u16::from_be_bytes([b[8], b[9]]) == 0xB752, header_size: hs,
        stream_type: 0xBA, ratebits, sr, access_unit_size: 40 << (ratebits & 7),
        mod0, mod1, mod2, ch6_mask: arr6, ch8_mask: arr8,
        is_vbr, peak_field, peak_bps, num_substreams, ext_sub, substream_info,
    })
}

fn find_all(d: &[u8], pat: &[u8]) -> Vec<usize> {
    (0..=d.len().saturating_sub(pat.len())).filter(|&i| &d[i..i + pat.len()] == pat).collect()
}

fn main() {
    let args: Vec<String> = env::args().collect();
    let d = fs::read(&args[1]).expect("read");
    let syncs = find_all(&d, &[0xF8, 0x72, 0x6F, 0xBA]);
    println!("file={} bytes={} major_syncs={}", args[1], d.len(), syncs.len());
    let mut max_au = 0usize; let mut au_count = 0usize;
    // access unit header: 16-bit word, low 12 bits = length in 16-bit words, starts 4 bytes before the sync
    let mut aus: Vec<(usize, usize)> = Vec::new();
    let mut p = 0usize;
    while p + 4 <= d.len() {
        let w = u16::from_be_bytes([d[p], d[p + 1]]);
        let words = (w & 0x0FFF) as usize;
        if words == 0 { break; }
        aus.push((p, words * 2));
        p += words * 2;
        if p >= d.len() { break; }
    }
    for &(_, n) in &aus { max_au = max_au.max(n); au_count += 1; }
    let mut crc_fail = 0; let mut sig_fail = 0;
    let mut first: Option<Mlp> = None;
    for &s in &syncs {
        let au = aus.iter().find(|(p, n)| s >= *p && s < p + n).map(|x| x.1).unwrap_or(0);
        match parse_sync(&d, s, au) {
            Some(m) => {
                if !m.crc_ok { crc_fail += 1; }
                if !m.sig_ok { sig_fail += 1; }
                if first.is_none() { first = Some(m); }
            }
            None => crc_fail += 1,
        }
    }
    let m = match first { Some(m) => m, None => { println!("no major sync"); return; } };
    println!("first major sync @byte {} (header_size {}, stream_type 0x{:02X})", m.off, m.header_size, m.stream_type);
    println!("sync signature 0xB752 ok: {}; sync CRC ok: {}; failures across {} syncs: crc={} sig={}",
        m.sig_ok, m.crc_ok, syncs.len(), crc_fail, sig_fail);
    println!("sample_rate={} Hz (ratebits 0x{:X}) access_unit_size={} samples", m.sr, m.ratebits, m.access_unit_size);
    println!("6ch presentation mask=0x{:02X} ({} ch: {})", m.ch6_mask, chanmap_channels(m.ch6_mask), chanmap_names(m.ch6_mask));
    println!("8ch/16ch mask (13-bit)=0x{:04X} ({} ch: {}) mod0={} mod1={} mod2={}", m.ch8_mask, chanmap_channels(m.ch8_mask), chanmap_names(m.ch8_mask), m.mod0, m.mod1, m.mod2);
    println!("is_vbr={} peak_field={} peak_bps={} ({} kbps)", m.is_vbr, m.peak_field, m.peak_bps, m.peak_bps / 1000);
    println!("num_substreams={} extended_substream_info={} substream_info=0x{:02X}", m.num_substreams, m.ext_sub, m.substream_info);
    let atmos_ffmpeg = m.num_substreams == 4 && (m.substream_info >> 7) == 1;
    let atmos_mediainfo = (m.substream_info >> 7) == 1;
    println!("atmos flag: ffmpeg rule={} mediainfo rule(MSB only)={}", atmos_ffmpeg, atmos_mediainfo);
    println!("access units={} max_au_bytes={} => measured peak_bps (max AU * 8 * 1200) = {}", au_count, max_au, max_au * 8 * 1200);
    let _ = m.stream_type; let _ = m.is_vbr;
}
