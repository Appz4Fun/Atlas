//! par2meta: prototype PAR2 / SFV / SRR metadata extractor.
//! Pure safe Rust, deps: md-5, crc32fast. Never indexes without bounds checks;
//! every parse step returns Option / records a truncation flag instead of panicking.

use crc32fast::Hasher;
use md5::{Digest, Md5};
use std::collections::BTreeMap;
use std::env;
use std::fs;

const PAR2_MAGIC: &[u8] = b"PAR2\0PKT";
const CRC_POLY: u32 = 0xEDB8_8320; // reflected IEEE 802.3
const MAX_PACKET: u64 = 1 << 32;

// ---------- byte helpers (all bounds-checked) ----------

fn rd_u16(b: &[u8], o: usize) -> Option<u16> {
    let s = b.get(o..o.checked_add(2)?)?;
    Some(u16::from_le_bytes([s[0], s[1]]))
}
fn rd_u32(b: &[u8], o: usize) -> Option<u32> {
    let s = b.get(o..o.checked_add(4)?)?;
    Some(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}
fn rd_u64(b: &[u8], o: usize) -> Option<u64> {
    let s = b.get(o..o.checked_add(8)?)?;
    let mut a = [0u8; 8];
    a.copy_from_slice(s);
    Some(u64::from_le_bytes(a))
}
fn rd_arr16(b: &[u8], o: usize) -> Option<[u8; 16]> {
    let s = b.get(o..o.checked_add(16)?)?;
    let mut a = [0u8; 16];
    a.copy_from_slice(s);
    Some(a)
}
fn trim_nul(s: &[u8]) -> &[u8] {
    let mut e = s.len();
    while e > 0 && s[e - 1] == 0 {
        e -= 1;
    }
    &s[..e]
}
fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{:02x}", x)).collect()
}
fn find_bytes(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

// ---------- CRC helpers ----------
// Register-level view of reflected CRC-32: one zero bit = r' = (r>>1) ^ (r&1 ? POLY : 0).
// Used to strip zero padding from the last IFSC slice CRC without the data.

fn crc_shift_bits(mut r: u32, bits: u64) -> u32 {
    for _ in 0..bits {
        r = (r >> 1) ^ (if r & 1 == 1 { CRC_POLY } else { 0 });
    }
    r
}
/// Inverse of crc_shift_bits. Bit 31 of r' equals bit 0 of r (POLY has bit 31 set).
fn crc_unshift_bits(mut r: u32, bits: u64) -> u32 {
    for _ in 0..bits {
        r = if r >> 31 == 1 { ((r ^ CRC_POLY) << 1) | 1 } else { r << 1 };
    }
    r
}
/// Given the finalized CRC `cp` of (D || zeros[s-m]) with total length s, return CRC(D), |D| = m.
/// Derivation: R = crc register with init 0xFFFFFFFF; L = init-0 register.
/// L(D||Z) = shift_{8p}(L(D)) because L(zeros) = 0. Solve for L(D), then re-apply init.
fn unpad_crc(cp: u32, s: u64, m: u64) -> Option<u32> {
    if m > s {
        return None;
    }
    let p = s - m;
    let rp = cp ^ 0xFFFF_FFFF;
    let shift_s = crc_shift_bits(0xFFFF_FFFF, s.checked_mul(8)?);
    let l_dshift = rp ^ shift_s;
    let l_d = crc_unshift_bits(l_dshift, p.checked_mul(8)?);
    let rd = crc_shift_bits(0xFFFF_FFFF, m.checked_mul(8)?) ^ l_d;
    Some(rd ^ 0xFFFF_FFFF)
}
/// Whole-file CRC32 from per-slice CRC32s (IFSC order). Last slice is unpadded first.
fn file_crc_from_slices(crcs: &[u32], slice_size: u64, file_len: u64) -> Result<u32, String> {
    if slice_size == 0 {
        return Err("slice_size is 0".into());
    }
    let n = crcs.len() as u64;
    let expected = file_len / slice_size + u64::from(file_len % slice_size != 0);
    if n != expected {
        return Err(format!("IFSC has {} slices, expected {} for length {}", n, expected, file_len));
    }
    let mut acc = Hasher::new();
    for (i, &c) in crcs.iter().enumerate() {
        let real = if (i as u64) + 1 == n { file_len - (n - 1) * slice_size } else { slice_size };
        let crc = if real == slice_size {
            c
        } else {
            unpad_crc(c, slice_size, real).ok_or("unpad overflow")?
        };
        acc.combine(&Hasher::new_with_initial_len(crc, real));
    }
    Ok(acc.finalize())
}

// ---------- PAR2 ----------

#[derive(Default, Debug)]
struct FileInfo {
    name: Option<String>,
    uni_name: Option<String>,
    md5_full: Option<[u8; 16]>,
    md5_16k: Option<[u8; 16]>,
    length: Option<u64>,
    filedesc_copies: usize,
    ifsc_slices: Option<Vec<(([u8; 16]), u32)>>,
}

#[derive(Default, Debug)]
struct Par2Set {
    slice_size: Option<u64>,
    file_count: Option<u32>,
    nonrec_ids: Vec<[u8; 16]>,
    rec_ids: Vec<[u8; 16]>,
    files: BTreeMap<[u8; 16], FileInfo>,
    creators: Vec<String>,
    comments: Vec<String>,
    recvslic: Vec<(u32, usize)>, // (exponent, data bytes)
    type_counts: BTreeMap<&'static str, usize>,
}

#[derive(Default, Debug)]
struct Par2Report {
    sets: BTreeMap<[u8; 16], Par2Set>,
    magic_hits: usize,
    valid: usize,
    bad_md5: usize,
    invalid_len: usize,
    truncated: Vec<(usize, u64)>,
    unknown_type: usize,
}

fn par2_type_name(t: &[u8]) -> Option<&'static str> {
    Some(match t {
        b"PAR 2.0\0Main\0\0\0\0" => "Main",
        b"PAR 2.0\0FileDesc" => "FileDesc",
        b"PAR 2.0\0IFSC\0\0\0\0" => "IFSC",
        b"PAR 2.0\0RecvSlic" => "RecvSlic",
        b"PAR 2.0\0Creator\0" => "Creator",
        b"PAR 2.0\0UniFileN" => "UniFileN",
        b"PAR 2.0\0CommASCI" => "CommASCI",
        b"PAR 2.0\0CommUni\0" => "CommUni",
        b"PAR 2.0\0FileSlic" => "FileSlic",
        b"PAR 2.0\0RFSC\0\0\0\0" => "RFSC",
        b"PAR 2.0\0PkdMain\0" => "PkdMain",
        b"PAR 2.0\0PkdRecvS" => "PkdRecvS",
        _ => return None,
    })
}

fn parse_par2(buf: &[u8]) -> Par2Report {
    let mut rep = Par2Report::default();
    let mut i = 0usize;
    while i < buf.len() {
        let rest = match buf.get(i..) {
            Some(r) => r,
            None => break,
        };
        let pos = match find_bytes(rest, PAR2_MAGIC) {
            Some(p) => i + p,
            None => break,
        };
        rep.magic_hits += 1;
        let len = match rd_u64(buf, pos + 8) {
            Some(v) => v,
            None => {
                rep.truncated.push((pos, 0));
                break;
            }
        };
        if len < 64 || len % 4 != 0 || len > MAX_PACKET {
            rep.invalid_len += 1;
            i = pos + 1;
            continue;
        }
        let end = match (pos as u64).checked_add(len) {
            Some(e) if e <= buf.len() as u64 => e as usize,
            _ => {
                // Declared length runs past the buffer: partial download / truncated packet.
                rep.truncated.push((pos, len));
                i = pos + 1;
                continue;
            }
        };
        let pkt = match buf.get(pos..end) {
            Some(p) => p,
            None => break,
        };
        let want = match pkt.get(16..32) {
            Some(w) => w,
            None => break,
        };
        let got = Md5::digest(pkt.get(32..).unwrap_or(&[]));
        if got.as_slice() != want {
            rep.bad_md5 += 1;
            i = pos + 1;
            continue;
        }
        rep.valid += 1;
        handle_packet(pkt, &mut rep);
        i = end;
    }
    rep
}

fn handle_packet(pkt: &[u8], rep: &mut Par2Report) {
    let set_id = match rd_arr16(pkt, 32) {
        Some(s) => s,
        None => return,
    };
    let ptype = match pkt.get(48..64) {
        Some(t) => t,
        None => return,
    };
    let body = pkt.get(64..).unwrap_or(&[]);
    let name = match par2_type_name(ptype) {
        Some(n) => n,
        None => {
            rep.unknown_type += 1;
            return;
        }
    };
    let set = rep.sets.entry(set_id).or_default();
    *set.type_counts.entry(name).or_insert(0) += 1;

    match name {
        "Main" => {
            if set.slice_size.is_some() {
                return; // duplicate Main packets are identical by spec
            }
            let ss = match rd_u64(body, 0) {
                Some(v) => v,
                None => return,
            };
            let n = match rd_u32(body, 8) {
                Some(v) => v,
                None => return,
            };
            set.slice_size = Some(ss);
            set.file_count = Some(n);
            let all = body.len().saturating_sub(12) / 16;
            for k in 0..all {
                if let Some(id) = rd_arr16(body, 12 + 16 * k) {
                    if (k as u64) < u64::from(n) {
                        set.nonrec_ids.push(id);
                    } else {
                        set.rec_ids.push(id);
                    }
                }
            }
        }
        "FileDesc" => {
            let id = match rd_arr16(body, 0) {
                Some(v) => v,
                None => return,
            };
            let fi = set.files.entry(id).or_default();
            fi.filedesc_copies += 1;
            if fi.md5_full.is_some() {
                return;
            }
            fi.md5_full = rd_arr16(body, 16);
            fi.md5_16k = rd_arr16(body, 32);
            fi.length = rd_u64(body, 48);
            fi.name = body.get(56..).map(|n| String::from_utf8_lossy(trim_nul(n)).into_owned());
        }
        "UniFileN" => {
            if let Some(id) = rd_arr16(body, 0) {
                let fi = set.files.entry(id).or_default();
                // Encoding assumed UTF-8 (UNVERIFIED against ParPar source).
                fi.uni_name = body.get(16..).map(|n| String::from_utf8_lossy(trim_nul(n)).into_owned());
            }
        }
        "IFSC" => {
            let id = match rd_arr16(body, 0) {
                Some(v) => v,
                None => return,
            };
            let fi = set.files.entry(id).or_default();
            if fi.ifsc_slices.is_some() {
                return;
            }
            let rest = body.get(16..).unwrap_or(&[]);
            let mut v = Vec::with_capacity(rest.len() / 20);
            for chunk in rest.chunks_exact(20) {
                let mut m = [0u8; 16];
                m.copy_from_slice(&chunk[..16]);
                let crc = u32::from_le_bytes([chunk[16], chunk[17], chunk[18], chunk[19]]);
                v.push((m, crc));
            }
            fi.ifsc_slices = Some(v);
        }
        "RecvSlic" => {
            let exp = rd_u32(body, 0).unwrap_or(0);
            set.recvslic.push((exp, body.len().saturating_sub(4)));
        }
        "Creator" => set.creators.push(String::from_utf8_lossy(trim_nul(body)).into_owned()),
        "CommASCI" => set.comments.push(String::from_utf8_lossy(trim_nul(body)).into_owned()),
        "CommUni" => {
            // UTF-16LE assumed (UNVERIFIED).
            let units: Vec<u16> = body.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
            let s = String::from_utf16_lossy(&units);
            set.comments.push(s.trim_end_matches('\0').to_string());
        }
        _ => {}
    }
}

/// Classify a PAR2 creator string. Strings are not hard-coded beyond the verified ParPar format.
fn classify_creator(s: &str) -> String {
    let lower = s.to_ascii_lowercase();
    let toks: Vec<&str> = s.split_whitespace().collect();
    let version_after = |key: &str| -> Option<String> {
        let idx = toks.iter().position(|t| t.to_ascii_lowercase().contains(key))?;
        toks.get(idx + 1).map(|t| t.trim_start_matches('v').to_string())
    };
    if lower.contains("parpar") {
        let ver = toks
            .iter()
            .find(|t| t.starts_with('v') && t.len() > 1 && t.as_bytes()[1].is_ascii_digit())
            .map(|t| t.trim_start_matches('v').to_string())
            .unwrap_or_else(|| "?".into());
        let arch = toks.iter().find(|t| matches!(**t, "x64" | "x86" | "arm64" | "arm" | "ia32")).copied().unwrap_or("?");
        format!("ParPar version={} arch={}", ver, arch)
    } else if lower.contains("par2cmdline") {
        let variant = if lower.contains("turbo") {
            "par2cmdline-turbo"
        } else if lower.contains("tbb") {
            "par2cmdline-tbb"
        } else if lower.contains("-mt") {
            "par2cmdline-mt"
        } else {
            "par2cmdline"
        };
        format!("{} version={}", variant, version_after("version").unwrap_or_else(|| "?".into()))
    } else if lower.contains("multipar") || lower.contains("par2j") {
        "MultiPar/par2j (UNVERIFIED string)".into()
    } else if lower.contains("quickpar") {
        "QuickPar (UNVERIFIED string)".into()
    } else if lower.contains("phpar2") {
        "phpar2 (UNVERIFIED string)".into()
    } else {
        "unknown".into()
    }
}

fn print_par2(rep: &Par2Report) {
    println!("format: PAR2");
    println!("magic_hits: {}  valid_packets: {}  bad_md5: {}  invalid_len: {}  unknown_type: {}", rep.magic_hits, rep.valid, rep.bad_md5, rep.invalid_len, rep.unknown_type);
    println!("truncated_packets: {}", rep.truncated.len());
    for (off, len) in rep.truncated.iter().take(8) {
        println!("  truncated at offset {} declared_len {}", off, len);
    }
    for (sid, set) in &rep.sets {
        println!("recovery_set_id: {}", hex(sid));
        println!("  packets_per_type: {:?}", set.type_counts);
        println!("  slice_size: {:?}", set.slice_size);
        println!(
            "  main: file_count={:?} nonrecovery_ids={} recovery_ids={}",
            set.file_count,
            set.nonrec_ids.len(),
            set.rec_ids.len()
        );
        for c in &set.creators {
            println!("  creator: {:?} -> {}", c, classify_creator(c));
        }
        for c in &set.comments {
            println!("  comment: {:?}", c);
        }
        for (exp, sz) in &set.recvslic {
            println!("  recovery_slice: exponent={} data_bytes={}", exp, sz);
        }
        for (id, fi) in &set.files {
            let ifsc_n = fi.ifsc_slices.as_ref().map(|v| v.len());
            // Whole-file CRC only if we have IFSC, length and slice size.
            let whole = match (&fi.ifsc_slices, fi.length, set.slice_size) {
                (Some(v), Some(len), Some(ss)) => {
                    let crcs: Vec<u32> = v.iter().map(|(_, c)| *c).collect();
                    match file_crc_from_slices(&crcs, ss, len) {
                        Ok(c) => format!("0x{:08x}", c),
                        Err(e) => format!("unavailable ({})", e),
                    }
                }
                _ => "unavailable (missing IFSC/FileDesc/Main)".into(),
            };
            println!("  file id={} name={:?}", hex(id), fi.name);
            if let Some(u) = &fi.uni_name {
                println!("    unicode_name={:?}", u);
            }
            println!(
                "    length={:?} md5={} md5_16k={} filedesc_copies={} ifsc_slices={:?}",
                fi.length,
                fi.md5_full.map(|m| hex(&m)).unwrap_or("-".into()),
                fi.md5_16k.map(|m| hex(&m)).unwrap_or("-".into()),
                fi.filedesc_copies,
                ifsc_n
            );
            println!("    whole_file_crc32: {}", whole);
        }
        println!("  nonrecovery_file_ids_without_FileDesc: {}", set.nonrec_ids.iter().filter(|i| !set.files.get(*i).map(|f| f.name.is_some()).unwrap_or(false)).count());
    }
}

// ---------- SFV ----------

#[derive(Debug, Default)]
struct SfvReport {
    header_comments: Vec<String>,
    entries: Vec<(String, u32)>,
    unparsed_lines: usize,
}

fn parse_sfv(buf: &[u8]) -> Option<SfvReport> {
    if buf.contains(&0) {
        return None;
    }
    let text = String::from_utf8_lossy(buf);
    let mut rep = SfvReport::default();
    for raw in text.lines() {
        let line = raw.trim_end_matches('\r');
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        if t.starts_with(';') {
            if rep.header_comments.len() < 32 {
                rep.header_comments.push(line.to_string());
            }
            continue;
        }
        let (name, crc) = match t.rfind(|c: char| c.is_ascii_whitespace()) {
            Some(idx) => (t[..idx].trim(), t[idx + 1..].trim()),
            None => ("", t),
        };
        if crc.len() == 8 && crc.bytes().all(|b| b.is_ascii_hexdigit()) && !name.is_empty() {
            if let Ok(v) = u32::from_str_radix(crc, 16) {
                rep.entries.push((name.to_string(), v));
                continue;
            }
        }
        rep.unparsed_lines += 1;
    }
    if rep.entries.is_empty() {
        None
    } else {
        Some(rep)
    }
}

fn print_sfv(rep: &SfvReport) {
    println!("format: SFV");
    for c in &rep.header_comments {
        println!("  comment (verbatim): {}", c);
    }
    println!("  entries: {}  unparsed_lines: {}", rep.entries.len(), rep.unparsed_lines);
    for (n, c) in &rep.entries {
        println!("  {:08x}  {}", c, n);
    }
}

// ---------- SRR ----------

#[derive(Debug, Default)]
struct SrrReport {
    blocks: Vec<(u8, u16, usize)>, // (type, flags, total size)
    app_name: Option<String>,
    stored: Vec<(String, u64, usize, bool)>, // (name, declared size, bytes available, truncated)
    oso: Vec<(String, u64, String)>,         // (name, size, hash hex)
    rar_volumes: Vec<String>,
    rar_block_types: BTreeMap<u8, usize>,
    padding_bytes: u64,
    truncated_at: Option<usize>,
    bad_block_at: Option<usize>,
}

fn parse_srr(buf: &[u8]) -> Option<SrrReport> {
    if buf.get(2).copied() != Some(0x69) {
        return None;
    }
    let mut rep = SrrReport::default();
    let mut i = 0usize;
    let mut in_rar = false;
    while i < buf.len() {
        let typ = match buf.get(i + 2) {
            Some(t) => *t,
            None => {
                rep.truncated_at = Some(i);
                break;
            }
        };
        let (flags, head_size) = match (rd_u16(buf, i + 3), rd_u16(buf, i + 5)) {
            (Some(f), Some(h)) => (f, h as usize),
            _ => {
                rep.truncated_at = Some(i);
                break;
            }
        };
        if head_size < 7 {
            rep.bad_block_at = Some(i);
            break;
        }
        let has_add = flags & 0x8000 != 0;
        let add = if has_add {
            match rd_u32(buf, i + 7) {
                Some(a) => a as u64,
                None => {
                    rep.truncated_at = Some(i);
                    break;
                }
            }
        } else {
            0
        };
        let body = i + 7 + if has_add { 4 } else { 0 };
        let head_end = i.saturating_add(head_size);
        if head_end > buf.len() || body > head_end {
            rep.truncated_at = Some(i);
            break;
        }
        // Blocks 0x72..=0x7B are RAR headers embedded after 0x71 (type + size only).
        let total_u64 = head_size as u64 + add;
        let total = usize::try_from(total_u64).unwrap_or(usize::MAX);
        rep.blocks.push((typ, flags, total));

        match typ {
            0x69 => {
                if flags & 1 != 0 {
                    if let Some(n) = rd_u16(buf, body) {
                        let s = buf.get(body + 2..body + 2 + n as usize).unwrap_or(&[]);
                        rep.app_name = Some(String::from_utf8_lossy(trim_nul(s)).into_owned());
                    }
                }
            }
            0x6A => {
                let size = add;
                if let Some(n) = rd_u16(buf, body) {
                    let s = buf.get(body + 2..body + 2 + n as usize).unwrap_or(&[]);
                    let avail = buf.len().saturating_sub(head_end);
                    let tr = (avail as u64) < size;
                    rep.stored.push((String::from_utf8_lossy(s).into_owned(), size, avail.min(size as usize), tr));
                }
            }
            0x6B => {
                let fsize = rd_u64(buf, body).unwrap_or(0);
                let hash = buf.get(body + 8..body + 16).map(hex).unwrap_or_default();
                if let Some(n) = rd_u16(buf, body + 16) {
                    let s = buf.get(body + 18..body + 18 + n as usize).unwrap_or(&[]);
                    rep.oso.push((String::from_utf8_lossy(s).into_owned(), fsize, hash));
                }
            }
            0x71 => {
                in_rar = true;
                if let Some(n) = rd_u16(buf, body) {
                    let s = buf.get(body + 2..body + 2 + n as usize).unwrap_or(&[]);
                    rep.rar_volumes.push(String::from_utf8_lossy(s).into_owned());
                }
            }
            0x6C => rep.padding_bytes += add,
            0x72..=0x7B if in_rar => {
                *rep.rar_block_types.entry(typ).or_insert(0) += 1;
                // Embedded RAR block: header only. Skip data only for types that carry add_size.
                let skip_data = has_add && typ != 0x74 && typ != 0x7A;
                let t2 = if skip_data { total } else { head_size };
                i = i.saturating_add(t2);
                if t2 == 0 {
                    break;
                }
                continue;
            }
            _ => {}
        }
        if total == 0 {
            rep.bad_block_at = Some(i);
            break;
        }
        i = i.saturating_add(total);
    }
    Some(rep)
}

fn print_srr(rep: &SrrReport) {
    println!("format: SRR");
    println!("  blocks: {}  truncated_at: {:?}  bad_block_at: {:?}", rep.blocks.len(), rep.truncated_at, rep.bad_block_at);
    println!("  app_name: {:?}", rep.app_name);
    for (n, s, avail, tr) in &rep.stored {
        println!("  stored_file: {:?} size={} bytes_present={} truncated={}", n, s, avail, tr);
    }
    for (n, s, h) in &rep.oso {
        println!("  oso_hash: {:?} size={} hash={}", n, s, h);
    }
    for v in &rep.rar_volumes {
        println!("  rar_volume: {:?}", v);
    }
    println!("  embedded_rar_blocks_by_type: {:?}", rep.rar_block_types);
    println!("  padding_bytes: {}", rep.padding_bytes);
    let mut counts: BTreeMap<u8, usize> = BTreeMap::new();
    for (t, _, _) in &rep.blocks {
        *counts.entry(*t).or_insert(0) += 1;
    }
    println!("  block_type_counts: {:?}", counts);
}

// ---------- driver ----------

fn process(path: &str) {
    println!("== {}", path);
    let buf = match fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            println!("read error: {}", e);
            return;
        }
    };
    println!("bytes: {}", buf.len());
    if find_bytes(&buf, PAR2_MAGIC).is_some() {
        print_par2(&parse_par2(&buf));
        return;
    }
    if let Some(r) = parse_srr(&buf) {
        if r.blocks.first().map(|b| b.0) == Some(0x69) {
            print_srr(&r);
            return;
        }
    }
    if let Some(r) = parse_sfv(&buf) {
        print_sfv(&r);
        return;
    }
    println!("format: unknown");
}

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("usage: par2meta <file>...");
        return;
    }
    for a in &args {
        process(a);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unpad_matches_crc32_of_real_tail() {
        let data: Vec<u8> = (0..1000u32).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect();
        for &m in &[1usize, 7, 333, 999, 1000] {
            let s = 1000u64;
            let mut padded = data[..m].to_vec();
            padded.resize(s as usize, 0);
            let cp = crc32fast::hash(&padded);
            let want = crc32fast::hash(&data[..m]);
            assert_eq!(unpad_crc(cp, s, m as u64), Some(want), "m={}", m);
        }
    }

    #[test]
    fn combine_matches_whole() {
        let data: Vec<u8> = (0..5000u32).map(|i| (i * 31 % 251) as u8).collect();
        let crcs: Vec<u32> = data.chunks(1024).map(|c| { let mut p = c.to_vec(); p.resize(1024, 0); crc32fast::hash(&p) }).collect();
        let got = file_crc_from_slices(&crcs, 1024, 5000);
        assert_eq!(got, Ok(crc32fast::hash(&data)));
    }

    #[test]
    fn never_panics_on_garbage() {
        let mut seed = 0x1234_5678u32;
        for len in 0..600usize {
            let mut b = Vec::with_capacity(len);
            for _ in 0..len {
                seed = seed.wrapping_mul(1103515245).wrapping_add(12345);
                b.push((seed >> 16) as u8);
            }
            let mut p = b.clone();
            p.splice(0..0, PAR2_MAGIC.iter().copied());
            let _ = parse_par2(&p);
            let _ = parse_srr(&b);
            let _ = parse_sfv(&b);
        }
    }
}
