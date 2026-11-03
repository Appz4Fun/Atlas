// rar_meta: prototype RAR4 + RAR5 header metadata extractor, safe on partial buffers.
// Usage: rar_meta <file> [max_bytes]     Output: one JSON object per line.
// Deps: crc32fast only. Never indexes without bounds checks; stops at first incomplete header.

use std::env;
use std::fs::File;
use std::io::Read;

type R<T> = Result<T, &'static str>;

// ---------- bounded little reader ----------
struct Rd<'a> {
    b: &'a [u8],
    p: usize,
    end: usize,
}

impl<'a> Rd<'a> {
    fn new(b: &'a [u8], p: usize, end: usize) -> Self {
        let end = end.min(b.len());
        let p = p.min(end);
        Rd { b, p, end }
    }
    fn left(&self) -> usize {
        self.end - self.p
    }
    fn bytes(&mut self, n: usize) -> R<&'a [u8]> {
        if n > self.left() {
            return Err("field overruns header bounds");
        }
        let s = &self.b[self.p..self.p + n];
        self.p += n;
        Ok(s)
    }
    fn u8(&mut self) -> R<u8> {
        Ok(self.bytes(1)?[0])
    }
    fn u16(&mut self) -> R<u16> {
        let s = self.bytes(2)?;
        Ok(u16::from_le_bytes([s[0], s[1]]))
    }
    fn u32(&mut self) -> R<u32> {
        let s = self.bytes(4)?;
        Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
    }
    fn u64(&mut self) -> R<u64> {
        let s = self.bytes(8)?;
        let mut a = [0u8; 8];
        a.copy_from_slice(s);
        Ok(u64::from_le_bytes(a))
    }
    fn vint(&mut self) -> R<u64> {
        let mut v: u64 = 0;
        let mut sh: u32 = 0;
        loop {
            let c = self.u8()?;
            if sh < 64 {
                v |= ((c & 0x7f) as u64) << sh;
            }
            sh += 7;
            if c & 0x80 == 0 {
                return Ok(v);
            }
            if sh >= 70 {
                return Err("vint too long");
            }
        }
    }
}

// ---------- JSON-ish output ----------
fn jstr(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{:02x}", x)).collect()
}

struct J(Vec<String>);
impl J {
    fn new(kind: &str) -> J {
        let mut j = J(Vec::new());
        j.s("kind", kind);
        j
    }
    fn s(&mut self, k: &str, v: &str) {
        self.0.push(format!("{}:{}", jstr(k), jstr(v)));
    }
    fn n(&mut self, k: &str, v: impl std::fmt::Display) {
        self.0.push(format!("{}:{}", jstr(k), v));
    }
    fn b(&mut self, k: &str, v: bool) {
        self.n(k, v);
    }
    fn os(&mut self, k: &str, v: Option<String>) {
        match v {
            Some(x) => self.s(k, &x),
            None => self.0.push(format!("{}:null", jstr(k))),
        }
    }
    fn on<T: std::fmt::Display>(&mut self, k: &str, v: Option<T>) {
        match v {
            Some(x) => self.n(k, x),
            None => self.0.push(format!("{}:null", jstr(k))),
        }
    }
    fn emit(&self) {
        println!("{{{}}}", self.0.join(","));
    }
}

fn stop(kind: &str, off: u64, reason: &str) {
    let mut j = J::new(kind);
    j.n("offset", off);
    j.s("reason", reason);
    j.emit();
}

// ---------- time helpers ----------
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn iso(ts: i64) -> String {
    let days = ts.div_euclid(86400);
    let secs = ts.rem_euclid(86400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        y,
        m,
        d,
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}

fn dos_to_unix(dos: u32) -> Option<i64> {
    let date = (dos >> 16) as i64;
    let time = (dos & 0xffff) as i64;
    let mo = (date >> 5) & 0xf;
    let d = date & 0x1f;
    if date == 0 || mo == 0 || d == 0 {
        return None;
    }
    let y = ((date >> 9) & 0x7f) + 1980;
    let h = (time >> 11) & 0x1f;
    let mi = (time >> 5) & 0x3f;
    let s = (time & 0x1f) * 2;
    Some(days_from_civil(y, mo, d) * 86400 + h * 3600 + mi * 60 + s)
}

fn filetime_to_unix(ft: u64) -> (i64, u32) {
    let secs = (ft / 10_000_000) as i64 - 11_644_473_600;
    (secs, ((ft % 10_000_000) * 100) as u32)
}

fn mode_type(m: u32) -> &'static str {
    match m & 0o170000 {
        0o040000 => "dir",
        0o100000 => "regular",
        0o120000 => "symlink",
        0o020000 => "chardev",
        0o060000 => "blockdev",
        0o010000 => "fifo",
        0o140000 => "socket",
        _ => "unknown",
    }
}

fn mode_str(m: u32) -> String {
    let mut s = String::new();
    let bits = [
        (0o400, 'r'), (0o200, 'w'), (0o100, 'x'), (0o040, 'r'), (0o020, 'w'), (0o010, 'x'),
        (0o004, 'r'), (0o002, 'w'), (0o001, 'x'),
    ];
    for (b, c) in bits.iter() {
        s.push(if m & b != 0 { *c } else { '-' });
    }
    format!("{}{}", mode_type(m).chars().next().unwrap_or('?'), s)
}

// ---------- signature / archive dispatch ----------
fn find_sig(buf: &[u8]) -> Option<(usize, u8)> {
    let lim = buf.len().min(4 << 20);
    let mut i = 0usize;
    while i + 7 <= buf.len() && i < lim {
        if &buf[i..i + 6] == b"Rar!\x1a\x07" {
            if buf[i + 6] == 0 {
                return Some((i, 4));
            }
            if buf[i + 6] == 1 && i + 8 <= buf.len() && buf[i + 7] == 0 {
                return Some((i, 5));
            }
        }
        i += 1;
    }
    None
}

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: rar_meta <file> [max_bytes]");
        std::process::exit(2);
    }
    let path = &args[1];
    let file_size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let limit: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(u64::MAX);
    let mut buf = Vec::new();
    match File::open(path) {
        Ok(f) => {
            if let Err(e) = f.take(limit).read_to_end(&mut buf) {
                eprintln!("read error: {}", e);
                std::process::exit(1);
            }
        }
        Err(e) => {
            eprintln!("open error: {}", e);
            std::process::exit(1);
        }
    }
    let mut j = J::new("input");
    j.s("path", path);
    j.n("file_size", file_size);
    j.n("buffer_len", buf.len());
    j.emit();

    match find_sig(&buf) {
        None => {
            let mut j = J::new("error");
            j.s("reason", "no RAR signature in scanned region");
            j.emit();
        }
        Some((off, 4)) => parse_rar4(&buf, off),
        Some((off, _)) => parse_rar5(&buf, off),
    }
}

// ---------- RAR4 ----------
fn host4_name(h: u8) -> &'static str {
    match h {
        0 => "msdos",
        1 => "os2",
        2 => "win32",
        3 => "unix",
        4 => "macos",
        5 => "beos",
        _ => "unknown",
    }
}

// RAR4 FHD_UNICODE name: ASCII part, NUL, then 2-bit-coded stream (libarchive algorithm).
fn rar4_unicode_name(raw: &[u8]) -> String {
    let nul = raw.iter().position(|&c| c == 0).unwrap_or(raw.len());
    let ascii = &raw[..nul];
    if nul + 1 >= raw.len() {
        return String::from_utf8_lossy(ascii).into_owned();
    }
    let end = raw.len();
    let mut off = nul + 1;
    let highbyte = raw[off] as u16;
    off += 1;
    let mut out: Vec<u16> = Vec::new();
    let mut flagbyte: u8 = 0;
    let mut flagbits: i32 = 0;
    while off < end {
        if flagbits == 0 {
            flagbyte = raw[off];
            off += 1;
            flagbits = 8;
            if off > end {
                break;
            }
        }
        flagbits -= 2;
        let code = (flagbyte >> flagbits) & 3;
        match code {
            0 => {
                if off >= end {
                    break;
                }
                out.push(raw[off] as u16);
                off += 1;
            }
            1 => {
                if off >= end {
                    break;
                }
                out.push((highbyte << 8) | raw[off] as u16);
                off += 1;
            }
            2 => {
                if off + 1 >= end {
                    break;
                }
                out.push(((raw[off + 1] as u16) << 8) | raw[off] as u16);
                off += 2;
            }
            _ => {
                if off >= end {
                    break;
                }
                let l = raw[off];
                off += 1;
                let mut extra: u8 = 0;
                let mut high: u16 = 0;
                if l & 0x80 != 0 {
                    if off >= end {
                        break;
                    }
                    extra = raw[off];
                    off += 1;
                    high = highbyte;
                }
                let count = (l & 0x7f) as usize + 2;
                for _ in 0..count {
                    let idx = out.len();
                    let c = ascii.get(idx).copied().unwrap_or(0).wrapping_add(extra);
                    out.push((high << 8) | c as u16);
                }
            }
        }
    }
    String::from_utf16_lossy(&out)
}

// RAR4 EXTTIME (FHEXTRA via LHD_EXTTIME 0x1000). Returns (name, unix, remainder_raw).
fn rar4_exttime(r: &mut Rd, dos_mtime: u32) -> R<Vec<(&'static str, i64, u32)>> {
    let fl = r.u16()?;
    let mut out = Vec::new();
    let names = ["arctime", "atime", "ctime", "mtime"];
    for i in (0..4usize).rev() {
        let rmode = (fl >> (i * 4)) & 0xf;
        if rmode & 8 == 0 {
            continue;
        }
        let base = if i == 3 { dos_mtime } else { r.u32()? };
        let count = (rmode & 3) as usize;
        let mut rem: u32 = 0;
        for j in 0..count {
            let c = r.u8()? as u32;
            rem |= c << ((j + 3 - count) * 8);
        }
        let mut t = dos_to_unix(base).unwrap_or(0);
        if rmode & 4 != 0 {
            t += 1;
        }
        out.push((names[i], t, rem));
    }
    Ok(out)
}

fn parse_rar4(buf: &[u8], base: usize) {
    let len = buf.len();
    let mut a = J::new("archive");
    a.s("format", "rar4");
    a.n("sfx_offset", base);
    a.emit();
    let mut pos = base + 7;
    loop {
        if pos == len {
            stop("truncated_at", pos as u64, "buffer_end_at_block_boundary");
            return;
        }
        if pos + 7 > len {
            stop("truncated_at", pos as u64, "truncated_base_header");
            return;
        }
        let (head_crc, typ, flags, hsize) = {
            let mut r = Rd::new(buf, pos, pos + 7);
            match (|| -> R<(u16, u8, u16, u16)> { Ok((r.u16()?, r.u8()?, r.u16()?, r.u16()?)) })() {
                Ok(v) => v,
                Err(_) => {
                    stop("truncated_at", pos as u64, "truncated_base_header");
                    return;
                }
            }
        };
        let hsize = hsize as usize;
        if hsize < 7 {
            stop("bad_header", pos as u64, "head_size<7");
            return;
        }
        if pos + hsize > len {
            stop("truncated_at", pos as u64, "truncated_header");
            return;
        }
        let crc_ok = (crc32fast::hash(&buf[pos + 2..pos + hsize]) & 0xffff) as u16 == head_crc;
        let add: u64 = if flags & 0x8000 != 0 && hsize >= 11 {
            Rd::new(buf, pos + 7, pos + 11).u32().unwrap_or(0) as u64
        } else {
            0
        };
        let next = (pos + hsize) as u64 + add;
        match typ {
            0x73 => {
                let mut j = J::new("main_header");
                j.s("fmt", "rar4");
                j.n("offset", pos);
                j.b("header_crc_ok", crc_ok);
                j.b("volume", flags & 0x1 != 0);
                j.b("first_volume", flags & 0x100 != 0);
                j.b("solid", flags & 0x8 != 0);
                j.b("locked", flags & 0x4 != 0);
                j.b("recovery_record", flags & 0x40 != 0);
                j.b("comment_present", flags & 0x2 != 0);
                j.b("new_numbering", flags & 0x10 != 0);
                j.b("authenticity_av", flags & 0x20 != 0);
                j.b("encrypted_headers", flags & 0x80 != 0);
                j.emit();
                if flags & 0x80 != 0 {
                    stop("stop", pos as u64, "encrypted_headers_cannot_parse_further");
                    return;
                }
            }
            0x74 | 0x7a => {
                if let Err(e) = rar4_file(buf, pos, hsize, flags, typ, add, crc_ok) {
                    stop("bad_header", pos as u64, e);
                    return;
                }
            }
            0x78 => {
                let mut r = Rd::new(buf, pos + 7, pos + hsize);
                let res = (|| -> R<(u8, u16, u32)> {
                    let _add = r.u32()?;
                    let v = r.u8()?;
                    let rec = r.u16()?;
                    let tot = r.u32()?;
                    Ok((v, rec, tot))
                })();
                let mut j = J::new("service");
                j.s("fmt", "rar4");
                j.n("offset", pos);
                j.s("name", "RR_PROTECT");
                j.b("header_crc_ok", crc_ok);
                j.n("data_size", add);
                j.n("data_offset", pos + hsize);
                if let Ok((v, rec, tot)) = res {
                    j.n("rr_version", v);
                    j.n("rr_recovery_sectors", rec);
                    j.n("rr_total_blocks", tot);
                    let pct = if tot > 0 { (rec as f64) * 100.0 / (tot as f64) } else { 0.0 };
                    j.s("rr_percent_estimate", &format!("{:.2}", pct));
                }
                j.emit();
            }
            0x7b => {
                let mut r = Rd::new(buf, pos + 7, pos + hsize);
                let eflags = flags;
                let mut j = J::new("end_of_archive");
                j.n("offset", pos);
                j.b("next_volume", eflags & 0x1 != 0);
                j.b("data_crc_present", eflags & 0x2 != 0);
                j.b("volume_number_present", eflags & 0x8 != 0);
                if eflags & 0x8 != 0 {
                    j.on("volume_number", r.u32().ok());
                } else {
                    j.on::<u32>("volume_number", None);
                }
                j.emit();
                stop("stop", pos as u64, "end_of_archive");
                return;
            }
            _ => {
                let mut j = J::new("block");
                j.s("fmt", "rar4");
                j.n("offset", pos);
                j.s("type", &format!("0x{:02x}", typ));
                j.n("header_size", hsize);
                j.n("data_size", add);
                j.emit();
            }
        }
        if next > len as u64 {
            stop("truncated_at", next, "next_block_beyond_buffer");
            return;
        }
        pos = next as usize;
    }
}

fn rar4_file(buf: &[u8], pos: usize, hsize: usize, flags: u16, typ: u8, _add: u64, crc_ok: bool) -> R<()> {
    let len = buf.len();
    let mut r = Rd::new(buf, pos + 7, pos + hsize);
    let pack_lo = r.u32()? as u64;
    let unp_lo = r.u32()? as u64;
    let host = r.u8()?;
    let fcrc = r.u32()?;
    let ftime = r.u32()?;
    let unp_ver = r.u8()?;
    let method = r.u8()?;
    let name_size = r.u16()? as usize;
    let attr = r.u32()?;
    let large = flags & 0x100 != 0;
    let (hi_pack, hi_unp) = if large { (r.u32()? as u64, r.u32()? as u64) } else { (0, 0) };
    let name_raw = r.bytes(name_size)?;
    let unicode = flags & 0x200 != 0;
    let name = if unicode {
        rar4_unicode_name(name_raw)
    } else {
        String::from_utf8_lossy(name_raw).into_owned()
    };
    let salt = if flags & 0x400 != 0 { Some(r.bytes(8)?) } else { None };
    let mut times: Vec<(&str, i64, u32)> = Vec::new();
    let mut exttime_err: Option<&str> = None;
    if flags & 0x1000 != 0 {
        match rar4_exttime(&mut r, ftime) {
            Ok(t) => times = t,
            Err(e) => exttime_err = Some(e),
        }
    }
    let packed = pack_lo + (hi_pack << 32);
    let unp = unp_lo + (hi_unp << 32);
    let data_start = pos + hsize;
    let avail = (len - data_start).min(packed as usize);
    let is_dir = typ == 0x74 && (flags & 0xe0) == 0xe0;
    let is_unix = host == 3;

    let mut j = J::new(if typ == 0x7a { "service" } else { "file" });
    j.s("fmt", "rar4");
    j.n("offset", pos);
    j.n("header_size", hsize);
    j.b("header_crc_ok", crc_ok);
    j.s("name", &name);
    j.b("unicode_name", unicode);
    j.s("host_os", host4_name(host));
    j.s("attr_hex", &format!("0x{:08x}", attr));
    if is_unix {
        j.s("unix_mode", &format!("{:o}", attr & 0o7777));
        j.s("unix_type", mode_type(attr));
        j.s("unix_mode_str", &mode_str(attr));
    }
    j.b("directory", is_dir);
    j.n("unp_ver", unp_ver);
    j.s("version", &format!("{}.{}", unp_ver / 10, unp_ver % 10));
    j.n("method", method);
    j.s(
        "method_name",
        &if method == 0x30 { "store".to_string() } else { format!("m{}", method.saturating_sub(0x30)) },
    );
    j.n("pack_size", packed);
    j.n("unp_size", unp);
    j.n("data_offset", data_start);
    j.n("data_in_buffer", avail);
    j.s("crc32", &format!("{:08x}", fcrc));
    j.b("split_before", flags & 0x1 != 0);
    j.b("split_after", flags & 0x2 != 0);
    j.b("encrypted", flags & 0x4 != 0);
    j.b("solid", flags & 0x10 != 0);
    j.b("large", large);
    j.s("salt_hex", &salt.map(hex).unwrap_or_default());
    j.os("dos_mtime_iso", dos_to_unix(ftime).map(iso));
    for (nm, t, rem) in &times {
        j.s(&format!("exttime_{}_iso", nm), &iso(*t));
        j.n(&format!("exttime_{}_rem_raw", nm), rem);
    }
    if let Some(e) = exttime_err {
        j.s("exttime_error", e);
    }
    if typ == 0x7a {
        j.s("subblock_name", &name);
        if name == "CMT" && method == 0x30 && packed <= 65536 {
            let end = (data_start + avail).min(len);
            j.s("cmt_text", &String::from_utf8_lossy(&buf[data_start..end]));
        }
        if name == "UOW" {
            let end = (data_start + avail).min(len);
            j.s("uow_body_hex_preview", &hex(&buf[data_start..end.min(data_start + 64)]));
        }
    }
    j.emit();
    Ok(())
}

// ---------- RAR5 ----------
#[derive(Default)]
struct Extra {
    crypt: Option<(u64, u64, u8, String, String, String)>,
    blake2: Option<String>,
    htime: Vec<(&'static str, i64, u32)>,
    version: Option<(u64, u64)>,
    redir: Option<(u64, u64, String)>,
    owner: Option<(Option<String>, Option<String>, Option<u64>, Option<u64>)>,
    subdata: Option<u64>,
    locator: Option<(u64, Option<u64>, Option<u64>)>, // (flags, qo_rel, rr_rel)
}

fn next_extra<'a>(ex: &mut Rd<'a>) -> R<(u64, Rd<'a>)> {
    let sz = ex.vint()? as usize;
    if sz == 0 || sz > ex.left() {
        return Err("bad extra record size");
    }
    let start = ex.p;
    ex.p += sz;
    let mut r = Rd::new(ex.b, start, start + sz);
    let t = r.vint()?;
    Ok((t, r))
}

fn extra_one(t: u64, x: &mut Rd, ex: &mut Extra) -> R<()> {
    match t {
        0x01 => {
            // encryption
            let ver = x.vint()?;
            let fl = x.vint()?;
            let kdf = x.u8()?;
            let salt = hex(x.bytes(16)?);
            let iv = hex(x.bytes(16)?);
            let chk = if fl & 1 != 0 { hex(x.bytes(12)?) } else { String::new() };
            ex.crypt = Some((ver, fl, kdf, salt, iv, chk));
        }
        0x02 => {
            let ht = x.vint()?;
            if ht == 0 {
                ex.blake2 = Some(format!("blake2sp:{}", hex(x.bytes(32)?)));
            } else {
                ex.blake2 = Some(format!("hashtype:{}", ht));
            }
        }
        0x03 => {
            let fl = x.vint()?;
            let unixfmt = fl & 1 != 0;
            let ns = fl & 0x10 != 0;
            let width = if unixfmt && !ns { 4 } else { 8 };
            for (bit, nm) in [(2u64, "mtime"), (4, "ctime"), (8, "atime")] {
                if fl & bit != 0 {
                    let v = if width == 4 { x.u32()? as u64 } else { x.u64()? };
                    let (secs, nsr) = if unixfmt {
                        if ns { ((v / 1_000_000_000) as i64, (v % 1_000_000_000) as u32) } else { (v as i64, 0) }
                    } else {
                        filetime_to_unix(v)
                    };
                    ex.htime.push((nm, secs, nsr));
                }
            }
        }
        0x04 => {
            let fl = x.vint()?;
            let v = x.vint()?;
            ex.version = Some((fl, v));
        }
        0x05 => {
            let rt = x.vint()?;
            let rf = x.vint()?;
            let nl = x.vint()? as usize;
            let nm = String::from_utf8_lossy(x.bytes(nl)?).into_owned();
            ex.redir = Some((rt, rf, nm));
        }
        0x06 => {
            let fl = x.vint()?;
            let mut user = None;
            let mut group = None;
            let mut uid = None;
            let mut gid = None;
            if fl & 1 != 0 {
                let l = x.vint()? as usize;
                user = Some(String::from_utf8_lossy(x.bytes(l)?).into_owned());
            }
            if fl & 2 != 0 {
                let l = x.vint()? as usize;
                group = Some(String::from_utf8_lossy(x.bytes(l)?).into_owned());
            }
            if fl & 4 != 0 {
                uid = Some(x.vint()?);
            }
            if fl & 8 != 0 {
                gid = Some(x.vint()?);
            }
            ex.owner = Some((user, group, uid, gid));
        }
        0x07 => {
            ex.subdata = Some(x.vint()?);
        }
        _ => {}
    }
    Ok(())
}

fn parse_rar5(buf: &[u8], base: usize) {
    let len = buf.len();
    let mut a = J::new("archive");
    a.s("format", "rar5");
    a.n("sfx_offset", base);
    a.emit();
    let mut main_pos: usize = 0;
    let mut pos = base + 8;
    loop {
        if pos == len {
            stop("truncated_at", pos as u64, "buffer_end_at_block_boundary");
            return;
        }
        let mut r = Rd::new(buf, pos, len);
        let crc = match r.u32() {
            Ok(v) => v,
            Err(_) => {
                stop("truncated_at", pos as u64, "truncated_block_crc");
                return;
            }
        };
        let hs = match r.vint() {
            Ok(v) => v,
            Err(_) => {
                stop("truncated_at", pos as u64, "truncated_header_size_vint");
                return;
            }
        };
        let hstart = r.p;
        if hs == 0 {
            stop("bad_header", pos as u64, "zero header size");
            return;
        }
        let hend64 = hstart as u64 + hs;
        if hend64 > len as u64 {
            stop("truncated_at", pos as u64, "truncated_header");
            return;
        }
        let hend = hend64 as usize;
        let crc_ok = crc32fast::hash(&buf[pos + 4..hend]) == crc;
        let mut h = Rd::new(buf, hstart, hend);
        let parsed = (|| -> R<(u64, u64, u64, usize, u64)> {
            let htype = h.vint()?;
            let hflags = h.vint()?;
            let extra = if hflags & 1 != 0 { h.vint()? } else { 0 };
            let dsize = if hflags & 2 != 0 { h.vint()? } else { 0 };
            if extra as usize > h.left() {
                return Err("extra area larger than header");
            }
            let extra_start = hend - extra as usize;
            Ok((htype, hflags, dsize, extra_start, extra))
        })();
        let (htype, hflags, dsize, extra_start, _extra) = match parsed {
            Ok(v) => v,
            Err(e) => {
                stop("bad_header", pos as u64, e);
                return;
            }
        };
        // Body reader: from current position in h to extra_start.
        let body_start = h.p;
        let mut body = Rd::new(buf, body_start, extra_start);
        let data_start = hend;
        let data_end = data_start as u64 + dsize;
        let mut ex = Rd::new(buf, extra_start, hend);
        let mut stop_after = false;
        match htype {
            1 => {
                main_pos = pos;
                let res = (|| -> R<()> {
                    let aflags = body.vint()?;
                    let volnum = if aflags & 2 != 0 { Some(body.vint()?) } else { None };
                    let mut ext = Extra::default();
                    while ex.left() > 0 {
                        match next_extra(&mut ex) {
                            Ok((t, mut x)) => {
                                if t == 1 {
                                    let fl = x.vint()?;
                                    let qo = if fl & 1 != 0 { Some(x.vint()?) } else { None };
                                    let rr = if fl & 2 != 0 { Some(x.vint()?) } else { None };
                                    ext.locator = Some((fl, qo, rr));
                                }
                            }
                            Err(_) => break,
                        }
                    }
                    let mut j = J::new("main_header");
                    j.s("fmt", "rar5");
                    j.n("offset", pos);
                    j.b("header_crc_ok", crc_ok);
                    j.b("volume", aflags & 1 != 0);
                    j.on("volume_number", volnum);
                    j.b("solid", aflags & 4 != 0);
                    j.b("recovery_record", aflags & 8 != 0);
                    j.b("locked", aflags & 0x10 != 0);
                    match ext.locator {
                        Some((fl, qo, rr)) => {
                            j.n("locator_flags", fl);
                            j.on("locator_qo_rel", qo);
                            j.on("locator_qo_abs", qo.map(|v| main_pos as u64 + v));
                            j.on("locator_rr_rel", rr);
                            j.on("locator_rr_abs", rr.map(|v| main_pos as u64 + v));
                        }
                        None => {
                            j.on::<u64>("locator_qo_rel", None);
                            j.on::<u64>("locator_rr_rel", None);
                        }
                    }
                    j.emit();
                    Ok(())
                })();
                if res.is_err() {
                    stop("bad_header", pos as u64, "bad main header body");
                    return;
                }
            }
            2 | 3 => {
                let svc = htype == 3;
                let res = (|| -> R<()> {
                    let fflags = body.vint()?;
                    let unp = body.vint()?;
                    let attr = body.vint()?;
                    let mtime = if fflags & 2 != 0 { Some(body.u32()? as i64) } else { None };
                    let dcrc = if fflags & 4 != 0 { Some(body.u32()?) } else { None };
                    let comp = body.vint()?;
                    let host = body.vint()?;
                    let nl = body.vint()? as usize;
                    let name = String::from_utf8_lossy(body.bytes(nl)?).into_owned();
                    let mut ext = Extra::default();
                    while ex.left() > 0 {
                        match next_extra(&mut ex) {
                            Ok((t, mut x)) => {
                                let _ = extra_one(t, &mut x, &mut ext);
                            }
                            Err(_) => break,
                        }
                    }
                    let is_dir = fflags & 1 != 0;
                    let method = (comp >> 7) & 7;
                    let avail = (len - data_start).min(dsize as usize);
                    let mut j = J::new(if svc { "service" } else { "file" });
                    j.s("fmt", "rar5");
                    j.n("offset", pos);
                    j.b("header_crc_ok", crc_ok);
                    j.s("name", &name);
                    j.s("host_os", if host == 1 { "unix" } else if host == 0 { "windows" } else { "unknown" });
                    j.b("directory", is_dir);
                    j.n("unp_size", if fflags & 8 != 0 { 0 } else { unp });
                    j.b("unp_size_unknown", fflags & 8 != 0);
                    j.n("pack_size", dsize);
                    j.n("data_offset", data_start);
                    j.n("data_in_buffer", avail);
                    j.s("attr_hex", &format!("0x{:x}", attr));
                    if host == 1 {
                        let m = attr as u32;
                        j.s("unix_mode", &format!("{:o}", m & 0o7777));
                        j.s("unix_type", mode_type(m));
                        j.s("unix_mode_str", &mode_str(m));
                    } else {
                        j.b("dos_directory_attr", attr & 0x10 != 0);
                        j.b("dos_readonly_attr", attr & 0x1 != 0);
                    }
                    j.os("mtime_iso", mtime.map(iso));
                    j.s("crc32", &dcrc.map(|c| format!("{:08x}", c)).unwrap_or_default());
                    j.s("version", &format!("{}", comp & 0x3f));
                    j.b("solid", comp & 0x40 != 0);
                    j.s("method_name", if method == 0 { "store" } else { "lzss" });
                    j.n("method", method);
                    let dict_bits = (comp >> 10) & 0x1f;
                    j.on("dict_bytes", 0x20000u64.checked_shl(dict_bits as u32));
                    j.b("split_before", hflags & 0x8 != 0);
                    j.b("split_after", hflags & 0x10 != 0);
                    j.os("blake2", ext.blake2.clone());
                    if let Some((v, fl, kdf, salt, iv, chk)) = &ext.crypt {
                        j.n("enc_version", v);
                        j.n("enc_flags", fl);
                        j.n("enc_kdf_log2", kdf);
                        j.s("enc_salt_hex", salt);
                        j.s("enc_iv_hex", iv);
                        j.s("enc_check_hex", chk);
                    }
                    for (nm, secs, nsr) in &ext.htime {
                        j.s(&format!("htime_{}_iso", nm), &iso(*secs));
                        j.n(&format!("htime_{}_ns", nm), nsr);
                    }
                    if let Some((fl, v)) = ext.version {
                        j.n("file_version_flags", fl);
                        j.n("file_version", v);
                    }
                    if let Some((rt, rf, tgt)) = &ext.redir {
                        j.n("redir_type", rt);
                        j.n("redir_flags", rf);
                        j.s("redir_target", tgt);
                    }
                    if let Some((u, g, uid, gid)) = &ext.owner {
                        j.os("owner_user", u.clone());
                        j.os("owner_group", g.clone());
                        j.on("owner_uid", *uid);
                        j.on("owner_gid", *gid);
                    }
                    if svc {
                        j.s("subblock_name", &name);
                        if let Some(p) = ext.subdata {
                            if name == "RR" {
                                j.n("rr_percent", p);
                            }
                        }
                        if name == "CMT" && method == 0 && avail <= 65536 {
                            let end = (data_start + avail).min(len);
                            j.s("cmt_text", &String::from_utf8_lossy(&buf[data_start..end]));
                        }
                    }
                    j.emit();
                    Ok(())
                })();
                if res.is_err() {
                    stop("bad_header", pos as u64, "bad file/service body");
                    return;
                }
            }
            4 => {
                let res = (|| -> R<()> {
                    let ver = body.vint()?;
                    let fl = body.vint()?;
                    let kdf = body.u8()?;
                    let salt = hex(body.bytes(16)?);
                    let chk = if fl & 1 != 0 { hex(body.bytes(12)?) } else { String::new() };
                    let mut j = J::new("encryption_header");
                    j.n("offset", pos);
                    j.n("enc_version", ver);
                    j.n("enc_flags", fl);
                    j.n("kdf_count_log2", kdf);
                    j.s("salt_hex", &salt);
                    j.s("check_value_hex", &chk);
                    j.emit();
                    Ok(())
                })();
                if res.is_err() {
                    stop("bad_header", pos as u64, "bad encryption header");
                    return;
                }
                stop("stop", pos as u64, "headers_encrypted_cannot_parse_further");
                return;
            }
            5 => {
                let fl = body.vint().unwrap_or(0);
                let mut j = J::new("end_of_archive");
                j.n("offset", pos);
                j.b("next_volume", fl & 1 != 0);
                j.emit();
                stop_after = true;
            }
            _ => {
                let mut j = J::new("block");
                j.n("offset", pos);
                j.n("type", htype);
                j.n("pack_size", dsize);
                j.emit();
            }
        }
        if stop_after {
            stop("stop", pos as u64, "end_of_archive");
            return;
        }
        if data_end > len as u64 {
            stop("truncated_at", data_end, "next_block_beyond_buffer");
            return;
        }
        pos = data_end as usize;
    }
}
