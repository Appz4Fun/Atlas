// w10-c: TrueHD / MLP major sync parser with the CRC rule taken from truehdd
// (truehd/src/process/extract.rs crc16_major_sync_info, structs/sync.rs, utils/crc.rs Crc16::update).
// Covered bytes: [sync, sync+C), C = 26 (FBB, or FBA without extension) or 28 + 2*ext (FBA, ext bit set).
// Stored CRC: big-endian u16 at [sync+C, sync+C+2). Update: crc = T[crc>>8] ^ (crc<<8) ^ byte, T = MSB-first poly 0x2D table, init 0.
// Usage: w10c_main <file> [<file> ...]   (std only)
use std::env;
use std::fs;

const POLY: u16 = 0x2D;

fn table_entry(i: u16) -> u16 {
    let mut c = i << 8;
    for _ in 0..8 {
        c = if c & 0x8000 != 0 { (c << 1) ^ POLY } else { c << 1 };
    }
    c
}

// truehdd Crc16::update (reversed table-lookup/XOR form, as FFmpeg's comment describes)
fn truehdd_crc16(buf: &[u8]) -> u16 {
    let mut crc: u16 = 0;
    for &b in buf {
        crc = table_entry(crc >> 8) ^ (crc << 8) ^ b as u16;
    }
    crc
}

// Major sync info length C (CRC offset from the sync word), per truehdd major_sync_info_len.
fn major_sync_crc_offset(b: &[u8]) -> usize {
    if b[3] == 0xBB {
        return 26; // FBB: always 26-byte base, extension bit ignored
    }
    if b[25] & 1 == 0 { 26 } else { 28 + 2 * (b[26] >> 4) as usize }
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

fn samplerate(code: u32) -> u32 {
    if code == 0xF { 0 } else { (if code & 8 != 0 { 44100 } else { 48000 }) << (code & 7) }
}

#[derive(Debug, Clone)]
struct Sync {
    off: usize,
    fba: bool,
    crc_off: usize,
    crc_ok: bool,
    sig_ok: bool,
    sr: u32,
    arr_a: u32,     // FBA: 6ch arrangement; FBB: channel_arrangement
    arr_b: u32,     // FBA: 8ch arrangement (13-bit); FBB: 0
    num_substreams: u32,
    ext_sub: u32,
    substream_info: u32,
}

fn parse_sync(d: &[u8], s: usize) -> Option<Sync> {
    let b = &d[s..];
    if b.len() < 28 || b[0..3] != [0xF8, 0x72, 0x6F] || (b[3] != 0xBA && b[3] != 0xBB) {
        return None;
    }
    let c = major_sync_crc_offset(b);
    if b.len() < c + 2 {
        return None;
    }
    let crc_ok = truehdd_crc16(&b[..c]) == u16::from_be_bytes([b[c], b[c + 1]]);
    let fba = b[3] == 0xBA;
    let mut r = Bits { d: b, pos: 32 };
    let (sr, arr_a, arr_b);
    if fba {
        let ratebits = r.get(4);
        r.get(4);
        r.get(2); r.get(2);
        arr_a = r.get(5);
        r.get(2);
        arr_b = r.get(13);
        sr = samplerate(ratebits);
    } else {
        let _g1 = r.get(4);
        let _g2 = r.get(4);
        let ratebits = r.get(4);
        r.get(4);
        r.get(11);
        arr_a = r.get(5);
        arr_b = 0;
        sr = samplerate(ratebits);
    }
    r.pos += 48; // 0xB752 signature + flags + reserved
    r.get(1);     // is_vbr
    r.get(15);    // peak data rate
    let num_substreams = r.get(4);
    r.get(2);
    let ext_sub = r.get(2);
    let substream_info = r.get(8);
    Some(Sync {
        off: s, fba, crc_off: c, crc_ok,
        sig_ok: u16::from_be_bytes([b[8], b[9]]) == 0xB752,
        sr, arr_a, arr_b, num_substreams, ext_sub, substream_info,
    })
}

// Atmos decisions from the first major sync.
// ffmpeg:    num_substreams == 4 && (substream_info >> 7) != 0
// mediainfo: (substream_info >> 7) != 0
// truehdd:   substream_info bit 7 selects substream 3 in the 16-channel presentation mask
//            (PresentationMap::with_substream_info masks[3] bit 3); FBB has no 16-ch presentation.
//            A set bit 7 with num_substreams != 4 is a truehdd SubstreamCountInfoInconsistent error.
//            16ch presentation present = masks[3] != 0 = bit7 || (ext_sub & 3) != 0.
fn rules(s: &Sync) -> (bool, bool, bool, bool) {
    let msb = (s.substream_info >> 7) & 1 == 1;
    let ffmpeg = s.num_substreams == 4 && msb;
    let mediainfo = msb;
    let truehdd = msb && s.fba;
    let p16 = s.fba && (msb || (s.ext_sub & 3) != 0);
    (ffmpeg, mediainfo, truehdd, p16)
}

fn main() {
    let args: Vec<String> = env::args().collect();
    for path in &args[1..] {
        let d = match fs::read(path) { Ok(v) => v, Err(e) => { println!("{}: read error {}", path, e); continue; } };
        let mut syncs = Vec::new();
        let mut i = 0usize;
        while i + 4 <= d.len() {
            if d[i] == 0xF8 && d[i + 1] == 0x72 && d[i + 2] == 0x6F && (d[i + 3] == 0xBA || d[i + 3] == 0xBB) {
                if let Some(s) = parse_sync(&d, i) { syncs.push(s); }
            }
            i += 1;
        }
        let total = syncs.len();
        let crc_pass = syncs.iter().filter(|s| s.crc_ok).count();
        let sig_pass = syncs.iter().filter(|s| s.sig_ok).count();
        match syncs.first() {
            None => println!("{}|no major sync", path),
            Some(f) => {
                let (ff, mi, td, p16) = rules(f);
                // Per-sync consistency of the Atmos flag across the file.
                let atmos_set = syncs.iter().filter(|s| rules(s).2).count();
                println!(
                    "{}|fmt={}|syncs={}|crc_ok={}/{}|sig_ok={}/{}|crc_off={}|sr={}|arr={}/{:013b}|nsub={}|ext={}|sub_info=0x{:02X}|p16={}|ffmpeg={}|mediainfo={}|truehdd={}|truehdd_atmos_syncs={}/{}",
                    path.rsplit('/').next().unwrap_or(path),
                    if f.fba { "FBA" } else { "FBB" },
                    total, crc_pass, total, sig_pass, total, f.crc_off, f.sr,
                    f.arr_a, f.arr_b, f.num_substreams, f.ext_sub, f.substream_info,
                    p16, ff, mi, td, atmos_set, total
                );
                let _ = f.off;
            }
        }
    }
}
