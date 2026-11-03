//! Partial-buffer header walker: PAR2 packets, RAR5 and RAR4 headers. std only.
use std::collections::BTreeMap;

// ---------- MD5 (RFC 1321) ----------
fn md5(data: &[u8]) -> [u8; 16] {
    const S: [u32; 64] = [7,12,17,22,7,12,17,22,7,12,17,22,7,12,17,22,5,9,14,20,5,9,14,20,5,9,14,20,5,9,14,20,
        4,11,16,23,4,11,16,23,4,11,16,23,4,11,16,23,6,10,15,21,6,10,15,21,6,10,15,21,6,10,15,21];
    let k: Vec<u32> = (0..64).map(|i| ((i as f64 + 1.0).sin().abs() * 4294967296.0) as u32).collect();
    let (mut a0, mut b0, mut c0, mut d0) = (0x67452301u32, 0xefcdab89u32, 0x98badcfeu32, 0x10325476u32);
    let mut msg = data.to_vec();
    let bitlen = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 { msg.push(0); }
    msg.extend_from_slice(&bitlen.to_le_bytes());
    for chunk in msg.chunks(64) {
        let m: Vec<u32> = (0..16).map(|i| u32::from_le_bytes(chunk[i*4..i*4+4].try_into().unwrap())).collect();
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let f2 = f.wrapping_add(a).wrapping_add(k[i]).wrapping_add(m[g]);
            a = d; d = c; c = b;
            b = b.wrapping_add(f2.rotate_left(S[i]));
        }
        a0 = a0.wrapping_add(a); b0 = b0.wrapping_add(b); c0 = c0.wrapping_add(c); d0 = d0.wrapping_add(d);
    }
    let mut out = [0u8; 16];
    for (i, v) in [a0, b0, c0, d0].iter().enumerate() { out[i*4..i*4+4].copy_from_slice(&v.to_le_bytes()); }
    out
}

// ---------- CRC32 (IEEE) ----------
fn crc32(data: &[u8]) -> u32 {
    let mut c = 0xffffffffu32;
    for &b in data {
        c ^= b as u32;
        for _ in 0..8 { c = if c & 1 != 0 { (c >> 1) ^ 0xedb88320 } else { c >> 1 }; }
    }
    !c
}

fn hex(b: &[u8]) -> String { b.iter().map(|x| format!("{:02x}", x)).collect() }
fn u16le(b: &[u8], o: usize) -> Option<u16> { b.get(o..o+2).map(|s| u16::from_le_bytes(s.try_into().unwrap())) }
fn u32le(b: &[u8], o: usize) -> Option<u32> { b.get(o..o+4).map(|s| u32::from_le_bytes(s.try_into().unwrap())) }
fn u64le(b: &[u8], o: usize) -> Option<u64> { b.get(o..o+8).map(|s| u64::from_le_bytes(s.try_into().unwrap())) }
fn arr16(b: &[u8], o: usize) -> Option<[u8; 16]> { b.get(o..o+16).map(|s| s.try_into().unwrap()) }

// ---------- PAR2 ----------
const PAR2_MAGIC: &[u8; 8] = b"PAR2\0PKT";

#[derive(Debug)]
enum Par2Body {
    Main { slice_size: u64, n_recoverable: u32, file_ids: Vec<[u8; 16]> },
    FileDesc { file_id: [u8; 16], md5_full: [u8; 16], md5_16k: [u8; 16], length: u64, name: String },
    Ifsc { file_id: [u8; 16], slices: Vec<([u8; 16], u32)> },
    Creator(String),
    RecvSlice { exponent: u32 },
    Other([u8; 16]),
}

#[derive(Debug)]
struct Par2Packet { offset: usize, set_id: [u8; 16], md5_ok: bool, body: Par2Body }

#[derive(Default, Debug)]
struct Par2Scan { packets: Vec<Par2Packet>, truncated: usize, bad_md5: usize }

fn par2_scan(buf: &[u8]) -> Par2Scan {
    let mut res = Par2Scan::default();
    let mut i = 0;
    while i + 8 <= buf.len() {
        // find magic
        let pos = match buf[i..].windows(8).position(|w| w == PAR2_MAGIC) { Some(p) => i + p, None => break };
        let len = match u64le(buf, pos + 8) { Some(l) => l, None => { res.truncated += 1; break } };
        if len < 64 || len % 4 != 0 { i = pos + 8; continue; } // false magic
        let end = pos.checked_add(len as usize);
        let end = match end { Some(e) if e <= buf.len() => e, _ => { res.truncated += 1; i = pos + 8; continue; } };
        let pkt = &buf[pos..end];
        let md5_ok = md5(&pkt[32..]) == pkt[16..32];
        if !md5_ok { res.bad_md5 += 1; i = pos + 8; continue; }
        let set_id = arr16(pkt, 32).unwrap();
        let ty: [u8; 16] = arr16(pkt, 48).unwrap();
        let b = &pkt[64..];
        let body = match &ty {
            t if t == b"PAR 2.0\0Main\0\0\0\0" => {
                let slice_size = u64le(b, 0).unwrap_or(0);
                let n_recoverable = u32le(b, 8).unwrap_or(0);
                let file_ids = b.get(12..).unwrap_or(&[]).chunks_exact(16).map(|c| c.try_into().unwrap()).collect();
                Par2Body::Main { slice_size, n_recoverable, file_ids }
            }
            t if t == b"PAR 2.0\0FileDesc" && b.len() >= 56 => {
                let raw = &b[56..];
                let n = raw.iter().position(|&c| c == 0).unwrap_or(raw.len());
                Par2Body::FileDesc { file_id: arr16(b, 0).unwrap(), md5_full: arr16(b, 16).unwrap(), md5_16k: arr16(b, 32).unwrap(),
                    length: u64le(b, 48).unwrap(), name: String::from_utf8_lossy(&raw[..n]).into_owned() }
            }
            t if t == b"PAR 2.0\0IFSC\0\0\0\0" && b.len() >= 16 => Par2Body::Ifsc {
                file_id: arr16(b, 0).unwrap(),
                slices: b[16..].chunks_exact(20).map(|c| (c[..16].try_into().unwrap(), u32le(c, 16).unwrap())).collect() },
            t if t == b"PAR 2.0\0Creator\0" => {
                let n = b.iter().position(|&c| c == 0).unwrap_or(b.len());
                Par2Body::Creator(String::from_utf8_lossy(&b[..n]).into_owned())
            }
            t if t == b"PAR 2.0\0RecvSlic" => Par2Body::RecvSlice { exponent: u32le(b, 0).unwrap_or(0) },
            _ => Par2Body::Other(ty),
        };
        res.packets.push(Par2Packet { offset: pos, set_id, md5_ok, body });
        i = end;
    }
    res
}

// ---------- RAR5 ----------
const RAR5_SIG: &[u8; 8] = b"Rar!\x1a\x07\x01\x00";
const RAR4_SIG: &[u8; 7] = b"Rar!\x1a\x07\x00";

fn vint(b: &[u8], o: &mut usize) -> Option<u64> {
    let mut v = 0u64;
    for sh in 0..10 {
        let c = *b.get(*o)?; *o += 1;
        v |= ((c & 0x7f) as u64) << (7 * sh);
        if c & 0x80 == 0 { return Some(v); }
    }
    None
}

#[derive(Debug, Clone, Default)]
struct RarEntry {
    service: bool, name: String, unp_size: u64, data_size: u64, crc32: Option<u32>, host_os: u64,
    comp_ver: u64, method: u64, dict_log: u64, solid: bool, is_dir: bool,
    split_before: bool, split_after: bool, unp_ver: u8, encrypted: bool,
}

#[derive(Debug, Default)]
struct RarScan {
    version: u8, sig_off: usize, main_flags: u64, vol_number: Option<u64>, locator: Option<(Option<u64>, Option<u64>)>,
    entries: Vec<RarEntry>, end_seen: bool, end_more_vols: Option<bool>,
    enc: Option<(u8, [u8; 16], Option<[u8; 12]>)>, headers_encrypted: bool,
    truncated: bool, bad_hdr_crc: usize,
}

fn rar5_walk(buf: &[u8], start: usize, r: &mut RarScan) {
    r.version = 5;
    let mut p = start + 8;
    loop {
        let hstart = p;
        let Some(hcrc) = u32le(buf, p) else { r.truncated = true; return };
        let mut o = p + 4;
        let Some(hsize) = vint(buf, &mut o) else { r.truncated = true; return };
        let body_start = o;
        let hend = body_start + hsize as usize;
        if hsize == 0 || hsize > 2 * 1024 * 1024 { r.truncated = true; return; }
        if hend > buf.len() { r.truncated = true; return; }
        if crc32(&buf[p + 4..hend]) != hcrc { r.bad_hdr_crc += 1; }
        let h = &buf[..hend];
        let htype = vint(h, &mut o).unwrap_or(0);
        let hflags = vint(h, &mut o).unwrap_or(0);
        let extra_size = if hflags & 1 != 0 { vint(h, &mut o).unwrap_or(0) } else { 0 };
        let data_size = if hflags & 2 != 0 { vint(h, &mut o).unwrap_or(0) } else { 0 };
        let extra_start = hend.saturating_sub(extra_size as usize);
        match htype {
            1 => {
                r.main_flags = vint(h, &mut o).unwrap_or(0);
                if r.main_flags & 2 != 0 { r.vol_number = vint(h, &mut o); }
                // extra area: locator record type 1
                let mut e = extra_start;
                while e < hend {
                    let Some(rsz) = vint(h, &mut e) else { break };
                    let rend = e + rsz as usize;
                    let rt = vint(h, &mut e).unwrap_or(0);
                    if rt == 1 {
                        let fl = vint(h, &mut e).unwrap_or(0);
                        let qo = if fl & 1 != 0 { vint(h, &mut e) } else { None };
                        let rr = if fl & 2 != 0 { vint(h, &mut e) } else { None };
                        r.locator = Some((qo, rr));
                    }
                    e = rend;
                }
            }
            2 | 3 => {
                let ff = vint(h, &mut o).unwrap_or(0);
                let unp = vint(h, &mut o).unwrap_or(0);
                let _attr = vint(h, &mut o);
                if ff & 2 != 0 { o += 4; }
                let crc = if ff & 4 != 0 { let c = u32le(h, o); o += 4; c } else { None };
                let ci = vint(h, &mut o).unwrap_or(0);
                let os = vint(h, &mut o).unwrap_or(0);
                let nl = vint(h, &mut o).unwrap_or(0) as usize;
                let name = h.get(o..o + nl).map(|s| String::from_utf8_lossy(s).into_owned()).unwrap_or_default();
                // file encryption extra record (type 1)
                let mut enc = false; let mut e = extra_start;
                while e < hend {
                    let Some(rsz) = vint(h, &mut e) else { break };
                    let rend = e + rsz as usize;
                    if vint(h, &mut e) == Some(1) { enc = true; }
                    e = rend;
                }
                r.entries.push(RarEntry { service: htype == 3, name, unp_size: unp, data_size, crc32: crc, host_os: os,
                    comp_ver: ci & 0x3f, solid: ci & 0x40 != 0, method: (ci >> 7) & 7, dict_log: (ci >> 10) & 0x1f,
                    is_dir: ff & 1 != 0, split_before: hflags & 8 != 0, split_after: hflags & 0x10 != 0, unp_ver: 0, encrypted: enc });
            }
            4 => {
                let _ver = vint(h, &mut o);
                let fl = vint(h, &mut o).unwrap_or(0);
                let kdf = *h.get(o).unwrap_or(&0); o += 1;
                let salt = arr16(h, o).unwrap_or([0; 16]); o += 16;
                let check = if fl & 1 != 0 { h.get(o..o + 12).map(|s| s.try_into().unwrap()) } else { None };
                r.enc = Some((kdf, salt, check));
                r.headers_encrypted = true;
                return; // everything after is AES-encrypted
            }
            5 => {
                let fl = vint(h, &mut o).unwrap_or(0);
                r.end_seen = true; r.end_more_vols = Some(fl & 1 != 0);
                return;
            }
            _ => {}
        }
        let _ = hstart;
        p = hend + data_size as usize;
        if p >= buf.len() { r.truncated = p > buf.len(); return; }
    }
}

// ---------- RAR4 (RAR 1.5 - 4.x) ----------
fn rar4_walk(buf: &[u8], start: usize, r: &mut RarScan) {
    r.version = 4;
    let mut p = start;
    loop {
        let (Some(hcrc), Some(&ty), Some(fl), Some(hsz)) = (u16le(buf, p), buf.get(p + 2), u16le(buf, p + 3), u16le(buf, p + 5))
            else { r.truncated = true; return };
        let hsz = hsz as usize;
        if hsz < 7 { r.truncated = true; return; }
        let hend = p + hsz;
        if hend > buf.len() { r.truncated = true; return; }
        let h = &buf[p..hend];
        let mut add = if fl & 0x8000 != 0 { u32le(h, 7).unwrap_or(0) as u64 } else { 0 };
        match ty {
            0x73 => {
                r.main_flags = fl as u64;
                if (crc32(&h[2..]) & 0xffff) as u16 != hcrc { r.bad_hdr_crc += 1; }
                if fl & 0x80 != 0 { r.headers_encrypted = true; return; }
            }
            0x74 | 0x7a => {
                let pack = u32le(h, 7).unwrap_or(0) as u64;
                let mut unp = u32le(h, 11).unwrap_or(0) as u64;
                let os = h.get(15).copied().unwrap_or(0) as u64;
                let crc = u32le(h, 16);
                let unp_ver = h.get(24).copied().unwrap_or(0);
                let method = h.get(25).copied().unwrap_or(0) as u64;
                let nsz = u16le(h, 26).unwrap_or(0) as usize;
                let mut no = 32;
                let mut packh = pack;
                if fl & 0x100 != 0 {
                    packh |= (u32le(h, 32).unwrap_or(0) as u64) << 32;
                    unp |= (u32le(h, 36).unwrap_or(0) as u64) << 32;
                    no = 40;
                }
                add = packh;
                let raw = h.get(no..no + nsz).unwrap_or(&[]);
                let raw = if fl & 0x200 != 0 { &raw[..raw.iter().position(|&c| c == 0).unwrap_or(raw.len())] } else { raw };
                // RAR 1.5-3.x CRC16 of header covers bytes 2..hsz (old 1.5 files may differ)
                if (crc32(&h[2..]) & 0xffff) as u16 != hcrc { r.bad_hdr_crc += 1; }
                r.entries.push(RarEntry { service: ty == 0x7a, name: String::from_utf8_lossy(raw).replace('\\', "/"), unp_size: unp, data_size: packh,
                    crc32: crc, host_os: os, method, is_dir: fl & 0xe0 == 0xe0, split_before: fl & 1 != 0, split_after: fl & 2 != 0,
                    unp_ver, encrypted: fl & 4 != 0, solid: fl & 0x10 != 0, ..Default::default() });
            }
            0x7b => { r.end_seen = true; r.end_more_vols = Some(fl & 1 != 0); return; }
            _ => {}
        }
        p = hend + add as usize;
        if p >= buf.len() { r.truncated = p > buf.len(); return; }
    }
}

fn rar_scan(buf: &[u8]) -> Option<RarScan> {
    // signature may be preceded by an SFX stub: search the first 1 MiB of the buffer
    let lim = buf.len().min(1 << 20);
    let pos = buf[..lim].windows(7).position(|w| w == RAR4_SIG || (w[..6] == RAR5_SIG[..6] && w[6] == 1))?;
    let mut r = RarScan { sig_off: pos, ..Default::default() };
    if buf.get(pos + 6) == Some(&1) { rar5_walk(buf, pos, &mut r); } else { rar4_walk(buf, pos + 7, &mut r); }
    Some(r)
}

// ---------- CLI ----------
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut limit: Option<usize> = None;
    for a in &args {
        if let Some(v) = a.strip_prefix("--prefix=") { limit = v.parse().ok(); continue; }
        if a == "--selftest" { selftest(); continue; }
        let mut data = match std::fs::read(a) { Ok(d) => d, Err(e) => { println!("ERR {} {}", a, e); continue } };
        if let Some(l) = limit { data.truncate(l); }
        println!("FILE {} bytes={}", a, data.len());
        if let Some(r) = rar_scan(&data) {
            println!("  RAR{} sig_off={} main_flags=0x{:x} vol={:?} locator={:?} end={} more_vols={:?} hdr_enc={} trunc={} bad_hdr_crc={}",
                r.version, r.sig_off, r.main_flags, r.vol_number, r.locator, r.end_seen, r.end_more_vols, r.headers_encrypted, r.truncated, r.bad_hdr_crc);
            if let Some((kdf, salt, chk)) = r.enc { println!("  ENC kdf_log2={} salt={} check={}", kdf, hex(&salt), chk.map(|c| hex(&c)).unwrap_or("-".into())); }
            for e in &r.entries {
                println!("  ENTRY {} name={:?} unp={} pack={} crc={} os={} ver={} meth={} dict={} solid={} dir={} sb={} sa={} unpver={} enc={}",
                    if e.service { "SVC" } else { "FILE" }, e.name, e.unp_size, e.data_size,
                    e.crc32.map(|c| format!("{:08x}", c)).unwrap_or("-".into()), e.host_os, e.comp_ver, e.method, e.dict_log,
                    e.solid, e.is_dir, e.split_before, e.split_after, e.unp_ver, e.encrypted);
            }
        }
        let ps = par2_scan(&data);
        if !ps.packets.is_empty() || ps.truncated > 0 {
            println!("  PAR2 packets={} truncated={} bad_md5={}", ps.packets.len(), ps.truncated, ps.bad_md5);
            let mut sets: BTreeMap<String, Vec<&Par2Packet>> = BTreeMap::new();
            for p in &ps.packets { sets.entry(hex(&p.set_id)).or_default().push(p); }
            for (sid, pk) in sets {
                let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
                for p in &pk {
                    let k = match &p.body {
                        Par2Body::Main { slice_size, n_recoverable, file_ids } => { println!("  SET {} MAIN slice={} nrec={} fileids={}", sid, slice_size, n_recoverable, file_ids.len()); "main" }
                        Par2Body::FileDesc { file_id, md5_full, md5_16k, length, name } => { println!("  SET {} FD id={} md5={} md5_16k={} len={} name={:?}", sid, hex(file_id), hex(md5_full), hex(md5_16k), length, name); "fd" }
                        Par2Body::Ifsc { file_id, slices } => { println!("  SET {} IFSC id={} slices={} first_crc={:08x}", sid, hex(file_id), slices.len(), slices.first().map(|s| s.1).unwrap_or(0)); "ifsc" }
                        Par2Body::Creator(s) => { println!("  SET {} CREATOR {:?}", sid, s); "creator" }
                        Par2Body::RecvSlice { .. } => "recv",
                        Par2Body::Other(_) => "other",
                    };
                    let _ = (p.offset, p.md5_ok);
                    *counts.entry(k).or_default() += 1;
                }
                println!("  SET {} counts={:?}", sid, counts);
            }
        }
    }
}

fn selftest() {
    assert_eq!(hex(&md5(b"")), "d41d8cd98f00b204e9800998ecf8427e");
    assert_eq!(hex(&md5(b"The quick brown fox jumps over the lazy dog")), "9e107d9d372bb6826bd81d3542a419d6");
    assert_eq!(crc32(b"123456789"), 0xcbf43926);
    println!("SELFTEST ok");
}
