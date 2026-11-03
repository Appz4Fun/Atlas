// PROTOTYPE: 7z + ZIP header metadata extractor working from HEAD (+ optional TAIL) buffers.
// Usage: sz_zip_meta <file> [head_bytes] [tail_bytes]
use std::fmt::Write as _;
use std::fs::File;
use std::io::{Cursor, Read, Seek, SeekFrom};

// ---------------- output ----------------
struct Out(Vec<String>);
impl Out {
    fn kv<T: std::fmt::Display>(&mut self, k: impl AsRef<str>, v: T) {
        self.0.push(format!("{} = {}", k.as_ref(), v));
    }
}

fn hex(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len() * 2);
    for x in b {
        let _ = write!(s, "{:02X}", x);
    }
    s
}

// ---------------- byte reader (never panics) ----------------
struct Rd<'a> {
    d: &'a [u8],
    p: usize,
}
impl<'a> Rd<'a> {
    fn new(d: &'a [u8]) -> Self {
        Rd { d, p: 0 }
    }
    fn at(d: &'a [u8], p: usize) -> Self {
        Rd { d, p }
    }
    fn u8(&mut self) -> Option<u8> {
        let v = *self.d.get(self.p)?;
        self.p += 1;
        Some(v)
    }
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.p.checked_add(n)?;
        let s = self.d.get(self.p..end)?;
        self.p = end;
        Some(s)
    }
    fn rest(&mut self) -> &'a [u8] {
        let s = self.d.get(self.p..).unwrap_or(&[]);
        self.p = self.d.len();
        s
    }
    fn u16(&mut self) -> Option<u16> {
        let s = self.take(2)?;
        Some(u16::from_le_bytes([s[0], s[1]]))
    }
    fn u32(&mut self) -> Option<u32> {
        let s = self.take(4)?;
        Some(u32::from_le_bytes(s.try_into().ok()?))
    }
    fn u64(&mut self) -> Option<u64> {
        let s = self.take(8)?;
        Some(u64::from_le_bytes(s.try_into().ok()?))
    }
    // 7z variable-length NUMBER
    fn number(&mut self) -> Option<u64> {
        let first = self.u8()?;
        let mut mask: u8 = 0x80;
        let mut n = 0usize;
        while n < 8 && (first & mask) != 0 {
            n += 1;
            mask >>= 1;
        }
        if n == 8 {
            return self.u64();
        }
        let rest = self.take(n)?;
        let mut v: u64 = 0;
        for (i, b) in rest.iter().enumerate() {
            v |= (*b as u64) << (8 * i);
        }
        let high = (first & mask.wrapping_sub(1)) as u64;
        Some(v | (high << (8 * n)))
    }
    // NUMBER used as a count/index; capped to keep allocations bounded
    fn count(&mut self) -> Option<usize> {
        let v = self.number()?;
        if v > (1 << 20) {
            return None;
        }
        Some(v as usize)
    }
    fn bits(&mut self, n: usize) -> Option<Vec<bool>> {
        let bytes = self.take(n.div_ceil(8))?;
        Some((0..n).map(|i| bytes[i / 8] & (0x80 >> (i % 8)) != 0).collect())
    }
    fn def_vec(&mut self, n: usize) -> Option<Vec<bool>> {
        let all = self.u8()?;
        if all != 0 {
            Some(vec![true; n])
        } else {
            self.bits(n)
        }
    }
}

fn digests(r: &mut Rd, n: usize) -> Option<Vec<Option<u32>>> {
    let defined = r.def_vec(n)?;
    let mut out = Vec::with_capacity(n);
    for d in defined {
        out.push(if d { Some(r.u32()?) } else { None });
    }
    Some(out)
}

// ---------------- time helpers ----------------
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}
fn iso(secs: i64) -> String {
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", y, m, d, rem / 3600, (rem % 3600) / 60, rem % 60)
}
fn filetime_to_unix(ft: u64) -> i64 {
    (ft / 10_000_000) as i64 - 11_644_473_600
}
fn filetime_iso(ft: u64) -> String {
    iso(filetime_to_unix(ft))
}
// returns (local_as_if_utc_secs, valid)
fn dos_local_secs(date: u16, time: u16) -> Option<i64> {
    let (y, mo, d) = ((date >> 9) as i64 + 1980, ((date >> 5) & 0xF) as i64, (date & 0x1F) as i64);
    let (h, mi, s) = ((time >> 11) as i64, ((time >> 5) & 0x3F) as i64, ((time & 0x1F) * 2) as i64);
    if mo == 0 || d == 0 || h > 23 || mi > 59 {
        return None;
    }
    Some(days_from_civil(y, mo, d) * 86400 + h * 3600 + mi * 60 + s)
}
fn dos_iso(date: u16, time: u16) -> String {
    match dos_local_secs(date, time) {
        Some(s) => format!("{} (local, no tz)", iso(s)),
        None => "invalid".into(),
    }
}

// ---------------- 7z structures ----------------
#[derive(Clone, Default, Debug)]
struct Coder {
    id: Vec<u8>,
    num_in: usize,
    num_out: usize,
    props: Vec<u8>,
}
#[derive(Clone, Default, Debug)]
struct Folder {
    coders: Vec<Coder>,
    bind_pairs: Vec<(usize, usize)>,
    packed: Vec<usize>,
    unpack_sizes: Vec<u64>,
    crc: Option<u32>,
}
impl Folder {
    fn total_out(&self) -> usize {
        self.coders.iter().map(|c| c.num_out).sum()
    }
    fn total_in(&self) -> usize {
        self.coders.iter().map(|c| c.num_in).sum()
    }
    fn main_out(&self) -> Option<usize> {
        (0..self.total_out()).find(|i| !self.bind_pairs.iter().any(|bp| bp.1 == *i))
    }
    fn unpack_size(&self) -> u64 {
        self.main_out().and_then(|i| self.unpack_sizes.get(i).copied()).unwrap_or(0)
    }
}
#[derive(Clone, Default, Debug)]
struct Streams {
    pack_pos: u64,
    pack_sizes: Vec<u64>,
    folders: Vec<Folder>,
    nus: Vec<usize>,
    sizes: Vec<u64>,
    crcs: Vec<Option<u32>>,
}

fn folder(r: &mut Rd) -> Option<Folder> {
    let nc = r.count()?;
    let mut f = Folder::default();
    for _ in 0..nc {
        let flags = r.u8()?;
        if flags & 0x80 != 0 {
            return None; // alternative methods: not supported
        }
        let id = r.take((flags & 0xF) as usize)?.to_vec();
        let (ni, no) = if flags & 0x10 != 0 { (r.count()?, r.count()?) } else { (1, 1) };
        let props = if flags & 0x20 != 0 {
            let n = r.count()?;
            r.take(n)?.to_vec()
        } else {
            vec![]
        };
        f.coders.push(Coder { id, num_in: ni, num_out: no, props });
    }
    let tout = f.total_out();
    let tin = f.total_in();
    if tout == 0 {
        return None;
    }
    for _ in 0..tout - 1 {
        let a = r.count()?;
        let b = r.count()?;
        f.bind_pairs.push((a, b));
    }
    let npacked = tin.checked_sub(tout - 1)?;
    if npacked == 1 {
        let idx = (0..tin).find(|i| !f.bind_pairs.iter().any(|bp| bp.0 == *i))?;
        f.packed.push(idx);
    } else {
        for _ in 0..npacked {
            f.packed.push(r.count()?);
        }
    }
    Some(f)
}

fn pack_info(r: &mut Rd, s: &mut Streams) -> Option<()> {
    s.pack_pos = r.number()?;
    let n = r.count()?;
    loop {
        match r.number()? {
            0 => break,
            0x09 => {
                for _ in 0..n {
                    s.pack_sizes.push(r.number()?);
                }
            }
            0x0A => {
                digests(r, n)?;
            }
            _ => return None,
        }
    }
    Some(())
}

fn unpack_info(r: &mut Rd, s: &mut Streams) -> Option<()> {
    if r.number()? != 0x0B {
        return None;
    }
    let nf = r.count()?;
    if r.u8()? != 0 {
        return None; // external
    }
    for _ in 0..nf {
        s.folders.push(folder(r)?);
    }
    if r.number()? != 0x0C {
        return None;
    }
    for f in s.folders.iter_mut() {
        let n = f.total_out();
        for _ in 0..n {
            let v = r.number()?;
            f.unpack_sizes.push(v);
        }
    }
    loop {
        match r.number()? {
            0 => break,
            0x0A => {
                let d = digests(r, nf)?;
                for (f, c) in s.folders.iter_mut().zip(d) {
                    f.crc = c;
                }
            }
            _ => return None,
        }
    }
    Some(())
}

fn substreams(r: &mut Rd, s: &mut Streams) -> Option<()> {
    let nf = s.folders.len();
    s.nus = vec![1; nf];
    let mut id = r.number()?;
    if id == 0x0D {
        for i in 0..nf {
            s.nus[i] = r.count()?;
        }
        id = r.number()?;
    }
    s.sizes.clear();
    let have_sizes = id == 0x09;
    for (i, f) in s.folders.iter().enumerate() {
        let n = s.nus[i];
        if n == 0 {
            continue;
        }
        let total = f.unpack_size();
        if have_sizes {
            let mut sum = 0u64;
            for _ in 0..n - 1 {
                let x = r.number()?;
                sum = sum.checked_add(x)?;
                s.sizes.push(x);
            }
            s.sizes.push(total.checked_sub(sum)?);
        } else {
            if n != 1 {
                return None;
            }
            s.sizes.push(total);
        }
    }
    if have_sizes {
        id = r.number()?;
    }
    let mut need = 0usize;
    for (i, f) in s.folders.iter().enumerate() {
        let n = s.nus[i];
        if !(n == 1 && f.crc.is_some()) {
            need += n;
        }
    }
    let mut digs = vec![];
    if id == 0x0A {
        digs = digests(r, need)?;
        id = r.number()?;
    }
    if id != 0 {
        return None;
    }
    s.crcs.clear();
    let mut k = 0usize;
    for (i, f) in s.folders.iter().enumerate() {
        let n = s.nus[i];
        if n == 1 && f.crc.is_some() {
            s.crcs.push(f.crc);
        } else {
            for _ in 0..n {
                s.crcs.push(digs.get(k).copied().flatten());
                k += 1;
            }
        }
    }
    Some(())
}

fn streams_info(r: &mut Rd) -> Option<Streams> {
    let mut s = Streams::default();
    let mut got_sub = false;
    loop {
        match r.number()? {
            0 => break,
            0x06 => pack_info(r, &mut s)?,
            0x07 => unpack_info(r, &mut s)?,
            0x08 => {
                substreams(r, &mut s)?;
                got_sub = true;
            }
            _ => return None,
        }
    }
    if !got_sub {
        s.nus = vec![1; s.folders.len()];
        s.sizes = s.folders.iter().map(|f| f.unpack_size()).collect();
        s.crcs = s.folders.iter().map(|f| f.crc).collect();
    }
    Some(s)
}

// ---------------- 7z codecs ----------------
fn method_name(id: &[u8]) -> &'static str {
    match id {
        [0x00] => "Copy",
        [0x03] => "Delta",
        [0x04] => "BCJ_x86",
        [0x05] => "PPC",
        [0x06] => "IA64",
        [0x07] => "ARM",
        [0x08] => "ARMT",
        [0x09] => "SPARC",
        [0x0A] => "ARM64",
        [0x21] => "LZMA2",
        [3, 1, 1] => "LZMA",
        [3, 3, 1, 3] => "BCJ_x86",
        [3, 3, 1, 0x1B] => "BCJ2",
        [3, 4, 1] => "PPMD",
        [4, 1, 8] => "Deflate",
        [4, 2, 2] => "BZip2",
        [6, 0xF1, 7, 1] => "AES-256+SHA-256",
        _ => "unknown",
    }
}
fn lzma2_dict(props: &[u8]) -> Option<u32> {
    let b = *props.first()?;
    if b > 40 {
        None
    } else if b == 40 {
        Some(u32::MAX)
    } else {
        Some((2 | (b as u32 & 1)) << (b / 2 + 11))
    }
}
fn lzma_props(props: &[u8]) -> Option<(u32, u32, u32, u32)> {
    if props.len() < 5 {
        return None;
    }
    let p = props[0] as u32;
    let dict = u32::from_le_bytes([props[1], props[2], props[3], props[4]]);
    Some((p % 9, (p / 9) % 5, p / 45, dict))
}
// AES props -> (cycles_power, salt, iv)
fn aes_props(props: &[u8]) -> Option<(u8, Vec<u8>, Vec<u8>)> {
    let b0 = *props.first()?;
    let b1 = *props.get(1).unwrap_or(&0);
    let ncp = b0 & 0x3F;
    let salt_n = (((b0 >> 7) & 1) + (b1 >> 4)) as usize;
    let iv_n = (((b0 >> 6) & 1) + (b1 & 0x0F)) as usize;
    let mut r = Rd::at(props, 2);
    let salt = r.take(salt_n)?.to_vec();
    let iv = r.take(iv_n)?.to_vec();
    Some((ncp, salt, iv))
}

// Decodes up to `limit` bytes of a single-coder folder from `packed`.
// Returns (bytes, error-if-any). Partial output is kept on error.
fn decode_stream(f: &Folder, packed: &[u8], limit: usize) -> (Vec<u8>, Option<String>) {
    let mut out = Vec::new();
    if f.coders.len() != 1 {
        return (out, Some(format!("{}-coder chain not supported in prototype", f.coders.len())));
    }
    let c = &f.coders[0];
    let size = f.unpack_size();
    let want = (size as usize).min(limit);
    let res: Result<(), String> = match c.id.as_slice() {
        [0x00] => {
            let n = want.min(packed.len());
            out.extend_from_slice(&packed[..n]);
            Ok(())
        }
        [0x21] => match lzma2_dict(&c.props) {
            Some(d) => {
                let r = lzma_rust2::Lzma2Reader::new(Cursor::new(packed), d, None);
                read_upto(r, want, &mut out)
            }
            None => Err("bad LZMA2 props".into()),
        },
        [3, 1, 1] => match lzma_props(&c.props) {
            Some((lc, lp, pb, dict)) => {
                let pbyte = ((pb * 5 + lp) * 9 + lc) as u8;
                match lzma_rust2::LzmaReader::new_with_props(Cursor::new(packed), size, pbyte, dict, None) {
                    Ok(r) => read_upto(r, want, &mut out),
                    Err(e) => Err(format!("LZMA init: {:?}", e)),
                }
            }
            None => Err("bad LZMA props".into()),
        },
        [6, 0xF1, 7, 1] => Err("AES-encrypted coder: no password, stop".into()),
        other => Err(format!("method {} not supported in prototype", method_name(other))),
    };
    (out, res.err())
}

fn read_upto<R: Read>(mut r: R, limit: usize, out: &mut Vec<u8>) -> Result<(), String> {
    let mut buf = vec![0u8; 16384];
    while out.len() < limit {
        let want = (limit - out.len()).min(buf.len());
        match r.read(&mut buf[..want]) {
            Ok(0) => break,
            Ok(n) => out.extend_from_slice(&buf[..n]),
            Err(e) => return Err(format!("{:?}", e)),
        }
    }
    Ok(())
}

// ---------------- 7z header ----------------
#[derive(Clone, Default)]
struct Entry {
    name: String,
    empty_stream: bool,
    empty_file: bool,
    anti: bool,
    attr: Option<u32>,
    mtime: Option<u64>,
    ctime: Option<u64>,
    atime: Option<u64>,
    size: u64,
    crc: Option<u32>,
    folder: Option<usize>,
}
struct Header {
    main: Option<Streams>,
    entries: Vec<Entry>,
}

fn files_info(r: &mut Rd, h: &mut Header) -> Option<()> {
    let n = r.count()?;
    let mut e = vec![Entry::default(); n];
    let mut n_empty = 0usize;
    loop {
        let t = r.number()?;
        if t == 0 {
            break;
        }
        let sz = r.number()? as usize;
        let body = r.take(sz)?;
        let mut b = Rd::new(body);
        match t {
            0x0E => {
                let f = b.bits(n)?;
                n_empty = f.iter().filter(|x| **x).count();
                for (i, x) in f.into_iter().enumerate() {
                    e[i].empty_stream = x;
                }
            }
            0x0F | 0x10 => {
                let f = b.bits(n_empty)?;
                let mut k = 0;
                for x in e.iter_mut() {
                    if x.empty_stream {
                        if t == 0x0F {
                            x.empty_file = f[k];
                        } else {
                            x.anti = f[k];
                        }
                        k += 1;
                    }
                }
            }
            0x11 => {
                if b.u8()? != 0 {
                    return None;
                }
                let units: Vec<u16> = b.rest().chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
                let mut names = units.split(|u| *u == 0);
                for x in e.iter_mut() {
                    x.name = String::from_utf16_lossy(names.next().unwrap_or(&[]));
                }
            }
            0x12 | 0x13 | 0x14 => {
                let def = b.def_vec(n)?;
                if b.u8()? != 0 {
                    return None;
                }
                for (i, d) in def.into_iter().enumerate() {
                    if d {
                        let v = Some(b.u64()?);
                        match t {
                            0x12 => e[i].ctime = v,
                            0x13 => e[i].atime = v,
                            _ => e[i].mtime = v,
                        }
                    }
                }
            }
            0x15 => {
                let def = b.def_vec(n)?;
                if b.u8()? != 0 {
                    return None;
                }
                for (i, d) in def.into_iter().enumerate() {
                    if d {
                        e[i].attr = Some(b.u32()?);
                    }
                }
            }
            _ => {} // kEmptyStream-adjacent, comments, start pos, dummy: skipped by size
        }
    }
    // map non-empty entries onto substreams, in order
    if let Some(ms) = &h.main {
        let mut streams: Vec<(usize, u64, Option<u32>)> = vec![];
        for (fi, nu) in ms.nus.iter().enumerate() {
            for _ in 0..*nu {
                let idx = streams.len();
                streams.push((fi, ms.sizes.get(idx).copied().unwrap_or(0), ms.crcs.get(idx).copied().flatten()));
            }
        }
        let mut k = 0usize;
        for x in e.iter_mut() {
            if !x.empty_stream {
                if let Some(s) = streams.get(k) {
                    x.folder = Some(s.0);
                    x.size = s.1;
                    x.crc = s.2;
                }
                k += 1;
            }
        }
    }
    h.entries = e;
    Some(())
}

fn parse_header(d: &[u8]) -> Option<Header> {
    let mut r = Rd::new(d);
    let mut h = Header { main: None, entries: vec![] };
    let mut id = r.number()?;
    if id == 0x02 {
        // ArchiveProperties: (type, size, bytes)* until 0
        loop {
            if r.number()? == 0 {
                break;
            }
            let sz = r.number()? as usize;
            r.take(sz)?;
        }
        id = r.number()?;
    }
    if id == 0x03 {
        streams_info(&mut r)?;
        id = r.number()?;
    }
    if id == 0x04 {
        h.main = Some(streams_info(&mut r)?);
        id = r.number()?;
    }
    if id == 0x05 {
        files_info(&mut r, &mut h)?;
        id = r.number()?;
    }
    if id != 0 {
        return None;
    }
    Some(h)
}

fn report_streams(pfx: &str, s: &Streams, out: &mut Out) {
    out.kv(format!("{pfx}.pack_pos_abs_from_32"), s.pack_pos);
    out.kv(format!("{pfx}.pack_streams"), s.pack_sizes.len());
    for (i, p) in s.pack_sizes.iter().enumerate() {
        out.kv(format!("{pfx}.pack_size[{i}]"), p);
    }
    out.kv(format!("{pfx}.folders"), s.folders.len());
    for (fi, f) in s.folders.iter().enumerate() {
        let fp = format!("{pfx}.folder[{fi}]");
        out.kv(format!("{fp}.unpack_size"), f.unpack_size());
        out.kv(format!("{fp}.crc"), f.crc.map(|c| format!("{:08X}", c)).unwrap_or("none".into()));
        out.kv(format!("{fp}.substreams"), s.nus.get(fi).copied().unwrap_or(0));
        for (ci, c) in f.coders.iter().enumerate() {
            let cp = format!("{fp}.coder[{ci}]");
            out.kv(format!("{cp}.method_id"), hex(&c.id));
            out.kv(format!("{cp}.method"), method_name(&c.id));
            out.kv(format!("{cp}.streams_in_out"), format!("{}/{}", c.num_in, c.num_out));
            out.kv(format!("{cp}.props_hex"), hex(&c.props));
            match c.id.as_slice() {
                [3, 1, 1] => {
                    if let Some((lc, lp, pb, dict)) = lzma_props(&c.props) {
                        out.kv(format!("{cp}.lzma.lc"), lc);
                        out.kv(format!("{cp}.lzma.lp"), lp);
                        out.kv(format!("{cp}.lzma.pb"), pb);
                        out.kv(format!("{cp}.lzma.dict"), dict);
                    }
                }
                [0x21] => {
                    out.kv(format!("{cp}.lzma2.dict"), lzma2_dict(&c.props).map(|x| x.to_string()).unwrap_or("bad".into()));
                }
                [6, 0xF1, 7, 1] => {
                    if let Some((ncp, salt, iv)) = aes_props(&c.props) {
                        out.kv(format!("{cp}.aes.num_cycles_power"), ncp);
                        out.kv(format!("{cp}.aes.salt_hex"), hex(&salt));
                        out.kv(format!("{cp}.aes.iv_hex"), hex(&iv));
                    }
                }
                _ => {}
            }
        }
        out.kv(format!("{fp}.bind_pairs"), format!("{:?}", f.bind_pairs));
        out.kv(format!("{fp}.packed_streams"), format!("{:?}", f.packed));
    }
}

fn sniff(b: &[u8]) -> &'static str {
    if b.len() >= 4 && b[..4] == [0x1A, 0x45, 0xDF, 0xA3] {
        "Matroska/WebM (EBML)"
    } else if b.len() >= 8 && &b[4..8] == b"ftyp" {
        "ISO BMFF (MP4/MOV family)"
    } else if b.starts_with(b"Rar!") {
        "RAR"
    } else if b.starts_with(b"PK\x03\x04") {
        "ZIP"
    } else if b.starts_with(&[0x37, 0x7A, 0xBC, 0xAF, 0x27, 0x1C]) {
        "7z"
    } else if b.starts_with(b"RIFF") {
        "RIFF (AVI/WAV)"
    } else if b.starts_with(b"OggS") {
        "Ogg"
    } else if b.starts_with(b"fLaC") {
        "FLAC"
    } else if b.starts_with(b"ID3") {
        "MP3 (ID3)"
    } else if b.starts_with(&[0x00, 0x00, 0x01, 0xBA]) {
        "MPEG-PS"
    } else if b.starts_with(&[0x47]) {
        "MPEG-TS? (0x47 sync)"
    } else {
        "unknown"
    }
}

// After a plain header is available: report folders, files, and the prefix probe.
fn report_header(h: &Header, head: &[u8], out: &mut Out) {
    if let Some(ms) = &h.main {
        report_streams("7z.main", ms, out);
        // prefix decode of folder 0
        if let Some(f) = ms.folders.first() {
            let simple = f.coders.len() == 1 && matches!(f.coders[0].id.as_slice(), [0] | [0x21] | [3, 1, 1]);
            if simple {
                let abs = 32u64.saturating_add(ms.pack_pos) as usize;
                let packed = head.get(abs..).unwrap_or(&[]);
                let (data, err) = decode_stream(f, packed, 65536);
                let first8 = &data[..data.len().min(8)];
                out.kv("7z.prefix.folder0.decoded_bytes", data.len());
                out.kv("7z.prefix.folder0.first8_hex", hex(first8));
                out.kv("7z.prefix.folder0.sniff", sniff(&data));
                if let Some(e) = err {
                    out.kv("7z.prefix.folder0.note", e);
                }
            } else {
                out.kv("7z.prefix.folder0", "not probed (coder chain not Copy/LZMA2/LZMA)");
            }
        }
    } else {
        out.kv("7z.main", "no MainStreamsInfo (empty or header-only archive)");
    }
    out.kv("7z.files", h.entries.len());
    for (i, e) in h.entries.iter().enumerate() {
        let p = format!("7z.file[{i}]");
        out.kv(format!("{p}.name"), &e.name);
        let kind = if e.empty_stream && !e.empty_file {
            "dir"
        } else if e.empty_stream {
            "empty_file"
        } else {
            "file"
        };
        out.kv(format!("{p}.kind"), kind);
        out.kv(format!("{p}.anti"), e.anti);
        out.kv(format!("{p}.size"), e.size);
        if let Some(c) = e.crc {
            out.kv(format!("{p}.crc32"), format!("{:08X}", c));
        }
        if let Some(f) = e.folder {
            out.kv(format!("{p}.folder"), f);
        }
        if let Some(t) = e.mtime {
            out.kv(format!("{p}.mtime"), filetime_iso(t));
        }
        if let Some(t) = e.ctime {
            out.kv(format!("{p}.ctime"), filetime_iso(t));
        }
        if let Some(t) = e.atime {
            out.kv(format!("{p}.atime"), filetime_iso(t));
        }
        if let Some(a) = e.attr {
            out.kv(format!("{p}.attr"), format!("0x{:08X}", a));
            out.kv(format!("{p}.attr.dir"), a & 0x10 != 0);
            out.kv(format!("{p}.attr.readonly"), a & 1 != 0);
            if a & 0x8000 != 0 {
                let mode = a >> 16;
                out.kv(format!("{p}.unix_mode"), format!("{:o}", mode));
            }
        }
    }
}

fn locate<'a>(head: &'a [u8], tail: Option<(u64, &'a [u8])>, abs: u64, len: u64) -> Option<(&'static str, &'a [u8])> {
    let end = abs.checked_add(len)?;
    if end <= head.len() as u64 {
        return Some(("head", &head[abs as usize..end as usize]));
    }
    if let Some((toff, tb)) = tail {
        let tend = toff.checked_add(tb.len() as u64)?;
        if abs >= toff && end <= tend {
            return Some(("tail", &tb[(abs - toff) as usize..(end - toff) as usize]));
        }
    }
    None
}

fn analyze_7z(head: &[u8], tail: Option<(u64, &[u8])>, total: u64, out: &mut Out) {
    out.kv("format", "7z");
    let sh = &head[..32];
    out.kv("7z.total_size", total);
    out.kv("7z.sig.version", format!("{}.{}", sh[6], sh[7]));
    let stored = u32::from_le_bytes([sh[8], sh[9], sh[10], sh[11]]);
    let calc = crc32fast::hash(&sh[12..32]);
    out.kv("7z.sig.start_header_crc_valid", stored == calc);
    let next_off = Rd::at(sh, 12).u64().unwrap_or(0);
    let next_size = Rd::at(sh, 20).u64().unwrap_or(0);
    let next_crc = Rd::at(sh, 28).u32().unwrap_or(0);
    out.kv("7z.next_header.offset_rel32", next_off);
    out.kv("7z.next_header.size", next_size);
    out.kv("7z.next_header.crc", format!("{:08X}", next_crc));
    let abs = 32u64.saturating_add(next_off);
    out.kv("7z.next_header.abs_offset", abs);
    if next_size == 0 {
        out.kv("7z.next_header", "empty archive");
        return;
    }
    let Some((src, block)) = locate(head, tail, abs, next_size) else {
        out.kv("7z.next_header.source", "unavailable: outside head and tail buffers");
        return;
    };
    out.kv("7z.next_header.source", src);
    out.kv("7z.next_header.crc_valid", crc32fast::hash(block) == next_crc);
    match block.first() {
        Some(0x01) => {
            out.kv("7z.next_header.kind", "plain kHeader");
            match parse_header(&block[1..]) {
                Some(h) => report_header(&h, head, out),
                None => out.kv("7z.header.parse", "FAILED"),
            }
        }
        Some(0x17) => {
            out.kv("7z.next_header.kind", "kEncodedHeader");
            let mut r = Rd::new(&block[1..]);
            let Some(si) = streams_info(&mut r) else {
                out.kv("7z.encoded_header.parse", "FAILED");
                return;
            };
            report_streams("7z.encoded_header", &si, out);
            let Some(f) = si.folders.first() else {
                out.kv("7z.encoded_header", "no folder");
                return;
            };
            if f.coders.iter().any(|c| c.id == [6, 0xF1, 7, 1]) {
                out.kv("7z.header_encrypted", true);
                out.kv("7z.header_encrypted.note", "file list is AES-encrypted; need password; stopping");
                return;
            }
            let Some(&ps) = si.pack_sizes.first() else {
                out.kv("7z.encoded_header", "no pack size");
                return;
            };
            let pabs = 32u64.saturating_add(si.pack_pos);
            let Some((psrc, packed)) = locate(head, tail, pabs, ps) else {
                out.kv("7z.encoded_header.packed_source", "unavailable: packed header outside buffers");
                return;
            };
            out.kv("7z.encoded_header.packed_source", psrc);
            let (data, err) = decode_stream(f, packed, 64 << 20);
            if let Some(e) = err {
                out.kv("7z.encoded_header.decode_error", e);
                return;
            }
            if let Some(c) = f.crc {
                out.kv("7z.encoded_header.folder_crc_valid", crc32fast::hash(&data) == c);
            }
            if data.first() != Some(&0x01) {
                out.kv("7z.encoded_header.decoded", "does not start with kHeader");
                return;
            }
            match parse_header(&data[1..]) {
                Some(h) => report_header(&h, head, out),
                None => out.kv("7z.header.parse", "FAILED after decode"),
            }
        }
        _ => out.kv("7z.next_header.kind", "unknown first byte"),
    }
}

// ---------------- ZIP ----------------
#[derive(Default)]
struct Extra {
    mtime: Option<i64>,
    atime: Option<i64>,
    ctime: Option<i64>,
    zip64: Option<Vec<u64>>,
}

fn extra_one(id: u16, body: &[u8], pfx: &str, out: &mut Out, ex: &mut Extra) -> Option<()> {
    let mut b = Rd::new(body);
    match id {
        0x5455 => {
            let fl = b.u8()?;
            if fl & 1 != 0 {
                ex.mtime = Some(b.u32()? as i32 as i64);
            }
            if fl & 2 != 0 {
                ex.atime = Some(b.u32()? as i32 as i64);
            }
            if fl & 4 != 0 {
                ex.ctime = Some(b.u32()? as i32 as i64);
            }
            if let Some(t) = ex.mtime {
                out.kv(format!("{pfx}.x5455.mtime_utc"), iso(t));
            }
            out.kv(format!("{pfx}.x5455.flags"), fl);
        }
        0x000a => {
            b.u32()?;
            while let Some(tag) = b.u16() {
                let sz = b.u16()? as usize;
                let tb = b.take(sz)?;
                if tag == 1 && sz >= 24 {
                    let mut t = Rd::new(tb);
                    let m = t.u64()?;
                    let a = t.u64()?;
                    let c = t.u64()?;
                    ex.mtime = Some(filetime_to_unix(m));
                    ex.atime = Some(filetime_to_unix(a));
                    ex.ctime = Some(filetime_to_unix(c));
                    out.kv(format!("{pfx}.ntfs.mtime_utc"), filetime_iso(m));
                    out.kv(format!("{pfx}.ntfs.atime_utc"), filetime_iso(a));
                    out.kv(format!("{pfx}.ntfs.ctime_utc"), filetime_iso(c));
                }
            }
        }
        0x7875 => {
            let ver = b.u8()?;
            let us = b.u8()? as usize;
            let uid = b.take(us)?;
            let gs = b.u8()? as usize;
            let gid = b.take(gs)?;
            let le = |x: &[u8]| x.iter().rev().fold(0u64, |a, v| (a << 8) | *v as u64);
            out.kv(format!("{pfx}.x7875.version"), ver);
            out.kv(format!("{pfx}.x7875.uid"), le(uid));
            out.kv(format!("{pfx}.x7875.gid"), le(gid));
        }
        0x7075 => {
            let ver = b.u8()?;
            let crc = b.u32()?;
            out.kv(format!("{pfx}.x7075.version"), ver);
            out.kv(format!("{pfx}.x7075.name_crc"), format!("{:08X}", crc));
            out.kv(format!("{pfx}.x7075.name_utf8"), String::from_utf8_lossy(b.rest()));
        }
        0x9901 => {
            let ver = b.u16()?;
            let vendor = b.take(2)?;
            let strength = b.u8()?;
            let method = b.u16()?;
            out.kv(format!("{pfx}.aes.version"), ver);
            out.kv(format!("{pfx}.aes.vendor"), String::from_utf8_lossy(vendor));
            out.kv(format!("{pfx}.aes.strength_bits"), match strength { 1 => 128, 2 => 192, 3 => 256, _ => 0 });
            out.kv(format!("{pfx}.aes.actual_method"), method);
        }
        0x0001 => {
            let mut v = vec![];
            while let Some(x) = b.u64() {
                v.push(x);
            }
            out.kv(format!("{pfx}.zip64.values"), format!("{:?}", v));
            ex.zip64 = Some(v);
        }
        0x5855 => {
            let a = b.u32()?;
            out.kv(format!("{pfx}.x5855.atime_utc"), iso(a as i32 as i64));
            if let Some(m) = b.u32() {
                ex.mtime = Some(m as i32 as i64);
                out.kv(format!("{pfx}.x5855.mtime_utc"), iso(m as i32 as i64));
            }
        }
        0x000d => {
            let a = b.u32()?;
            let m = b.u32()?;
            ex.mtime = Some(m as i32 as i64);
            out.kv(format!("{pfx}.x000d.atime_utc"), iso(a as i32 as i64));
            out.kv(format!("{pfx}.x000d.mtime_utc"), iso(m as i32 as i64));
        }
        _ => {
            out.kv(format!("{pfx}.extra.0x{id:04x}.raw_hex_head"), hex(&body[..body.len().min(32)]));
        }
    }
    Some(())
}

fn parse_extras(x: &[u8], pfx: &str, out: &mut Out) -> Extra {
    let mut ex = Extra::default();
    let mut r = Rd::new(x);
    while let (Some(id), Some(sz)) = (r.u16(), r.u16()) {
        let Some(body) = r.take(sz as usize) else {
            out.kv(format!("{pfx}.extra.truncated_at"), id);
            break;
        };
        out.kv(format!("{pfx}.extra.0x{id:04x}.size"), sz);
        let _ = extra_one(id, body, pfx, out, &mut ex);
    }
    ex
}

// UTC offset estimate: DOS local time (2 s granularity) vs. UTC mtime from an extra field
fn report_offset(pfx: &str, date: u16, time: u16, mtime: Option<i64>, out: &mut Out) {
    if let (Some(local), Some(utc)) = (dos_local_secs(date, time), mtime) {
        let raw = local - utc;
        out.kv(format!("{pfx}.utc_offset_raw_sec"), raw);
        let cand = ((raw as f64 / 900.0).round() as i64) * 900;
        out.kv(format!("{pfx}.utc_offset_candidate"), format!("{}{:02}:{:02}", if cand < 0 { "-" } else { "+" }, cand.abs() / 3600, (cand.abs() % 3600) / 60));
    }
}

fn zip_local(head: &[u8], out: &mut Out) {
    let mut pos = 0usize;
    let mut idx = 0;
    while let Some(sig) = Rd::at(head, pos).u32() {
        if sig != 0x04034b50 {
            break;
        }
        let mut r = Rd::at(head, pos + 4);
        let fields = (|| -> Option<_> {
            let ver = r.u16()?;
            let gp = r.u16()?;
            let method = r.u16()?;
            let time = r.u16()?;
            let date = r.u16()?;
            let crc = r.u32()?;
            let csz = r.u32()?;
            let usz = r.u32()?;
            let nlen = r.u16()? as usize;
            let xlen = r.u16()? as usize;
            let name = r.take(nlen)?;
            let x = r.take(xlen)?;
            Some((ver, gp, method, time, date, crc, csz, usz, name, x))
        })();
        let Some((ver, gp, method, time, date, crc, csz, usz, name, x)) = fields else {
            out.kv("zip.local.truncated_at", pos);
            break;
        };
        let p = format!("zip.local[{idx}]");
        out.kv(format!("{p}.offset"), pos);
        out.kv(format!("{p}.name"), String::from_utf8_lossy(name));
        out.kv(format!("{p}.version_needed"), format!("{}.{}", ver / 10, ver % 10));
        out.kv(format!("{p}.gp_flags"), format!("0x{:04X}", gp));
        out.kv(format!("{p}.encrypted"), gp & 1 != 0);
        out.kv(format!("{p}.data_descriptor"), gp & 8 != 0);
        out.kv(format!("{p}.utf8_names"), gp & 0x800 != 0);
        out.kv(format!("{p}.method"), method);
        out.kv(format!("{p}.dos_time"), dos_iso(date, time));
        out.kv(format!("{p}.crc32"), format!("{:08X}", crc));
        let mut sub = Out(vec![]);
        let ex = parse_extras(x, &p, &mut sub);
        out.0.extend(sub.0);
        let mut csz64 = csz as u64;
        let mut usz64 = usz as u64;
        if let Some(z) = &ex.zip64 {
            let mut k = 0;
            if usz == 0xFFFF_FFFF {
                usz64 = *z.get(k).unwrap_or(&0);
                k += 1;
            }
            if csz == 0xFFFF_FFFF {
                csz64 = *z.get(k).unwrap_or(&0);
            }
        }
        out.kv(format!("{p}.compressed_size"), csz64);
        out.kv(format!("{p}.uncompressed_size"), usz64);
        report_offset(&p, date, time, ex.mtime, out);
        let data_start = pos + 30 + nlen_of(head, pos);
        let _ = name;
        if gp & 8 != 0 && csz == 0 {
            out.kv(format!("{p}.note"), "data descriptor with zero csz: cannot skip to next local header");
            break;
        }
        let next = data_start as u64 + csz64;
        if next >= head.len() as u64 {
            break;
        }
        pos = next as usize;
        idx += 1;
    }
    out.kv("zip.local.count_in_head", idx);
}

fn nlen_of(h: &[u8], pos: usize) -> usize {
    Rd::at(h, pos + 26).u16().unwrap_or(0) as usize + Rd::at(h, pos + 28).u16().unwrap_or(0) as usize
}

fn zip_tail(toff: u64, tb: &[u8], total: u64, out: &mut Out) {
    let len = tb.len();
    let mut eocd = None;
    if len >= 22 {
        let lo = len.saturating_sub(22 + 65535);
        for p in (lo..=len - 22).rev() {
            if Rd::at(tb, p).u32() == Some(0x06054b50) {
                let clen = Rd::at(tb, p + 20).u16().unwrap_or(0) as usize;
                if p + 22 + clen <= len {
                    eocd = Some(p);
                    break;
                }
            }
        }
    }
    let Some(p) = eocd else {
        out.kv("zip.eocd", "not found in tail buffer");
        return;
    };
    let mut r = Rd::at(tb, p + 4);
    let disk = r.u16().unwrap_or(0);
    let cd_disk = r.u16().unwrap_or(0);
    let ent_disk = r.u16().unwrap_or(0) as u64;
    let ent_total = r.u16().unwrap_or(0) as u64;
    let mut cd_size = r.u32().unwrap_or(0) as u64;
    let mut cd_off = r.u32().unwrap_or(0) as u64;
    let clen = r.u16().unwrap_or(0) as usize;
    out.kv("zip.eocd.abs_offset", toff + p as u64);
    out.kv("zip.eocd.disk_number", disk);
    out.kv("zip.eocd.cd_start_disk", cd_disk);
    out.kv("zip.eocd.entries_this_disk", ent_disk);
    out.kv("zip.eocd.entries_total", ent_total);
    out.kv("zip.eocd.cd_size", cd_size);
    out.kv("zip.eocd.cd_offset", cd_off);
    out.kv("zip.eocd.comment_len", clen);
    out.kv("zip.eocd.comment", String::from_utf8_lossy(&tb[p + 22..p + 22 + clen]));
    out.kv("zip.eocd.trailing_bytes", len - (p + 22 + clen));
    let mut ent_total = ent_total;
    // zip64 locator + record
    if p >= 20 && Rd::at(tb, p - 20).u32() == Some(0x07064b50) {
        let z64_abs = Rd::at(tb, p - 12).u64().unwrap_or(0);
        out.kv("zip.zip64_locator.eocd64_offset", z64_abs);
        if z64_abs >= toff && z64_abs - toff + 56 <= len as u64 {
            let o = (z64_abs - toff) as usize;
            if Rd::at(tb, o).u32() == Some(0x06064b50) {
                let mut r = Rd::at(tb, o + 12);
                let _ver_made = r.u16();
                let _ver_need = r.u16();
                let _d = r.u32();
                let _cd = r.u32();
                let _ed = r.u64();
                if let Some(et) = r.u64() {
                    ent_total = et;
                    out.kv("zip.zip64.entries_total", et);
                }
                if let Some(cs) = r.u64() {
                    cd_size = cs;
                    out.kv("zip.zip64.cd_size", cs);
                }
                if let Some(co) = r.u64() {
                    cd_off = co;
                    out.kv("zip.zip64.cd_offset", co);
                }
            } else {
                out.kv("zip.zip64.eocd", "signature missing at locator offset");
            }
        } else {
            out.kv("zip.zip64.eocd", "not in tail buffer");
        }
    }
    out.kv("zip.cd.abs_offset", cd_off);
    out.kv("zip.cd.size", cd_size);
    if cd_off >= toff && cd_off.saturating_add(cd_size) <= toff + len as u64 {
        out.kv("zip.cd.source", "tail");
        let base = (cd_off - toff) as usize;
        let cd = &tb[base..(base + cd_size as usize).min(len)];
        zip_cd(cd, out);
    } else {
        out.kv("zip.cd.source", "unavailable: central directory outside tail buffer");
    }
    let _ = (total, ent_total);
}

fn zip_cd(cd: &[u8], out: &mut Out) {
    let mut pos = 0usize;
    let mut i = 0;
    while Rd::at(cd, pos).u32() == Some(0x02014b50) {
        let mut r = Rd::at(cd, pos + 4);
        let fields = (|| -> Option<_> {
            let made = r.u16()?;
            let needed = r.u16()?;
            let gp = r.u16()?;
            let method = r.u16()?;
            let time = r.u16()?;
            let date = r.u16()?;
            let crc = r.u32()?;
            let csz = r.u32()?;
            let usz = r.u32()?;
            let nlen = r.u16()? as usize;
            let xlen = r.u16()? as usize;
            let clen = r.u16()? as usize;
            let disk = r.u16()?;
            let iattr = r.u16()?;
            let eattr = r.u32()?;
            let loff = r.u32()?;
            let name = r.take(nlen)?;
            let x = r.take(xlen)?;
            let com = r.take(clen)?;
            Some((made, needed, gp, method, time, date, crc, csz, usz, nlen, xlen, clen, disk, iattr, eattr, loff, name, x, com))
        })();
        let Some((made, needed, gp, method, time, date, crc, csz, usz, nlen, xlen, clen, disk, iattr, eattr, loff, name, x, com)) = fields else {
            out.kv("zip.cd.truncated_at_index", i);
            break;
        };
        let p = format!("zip.cd[{i}]");
        out.kv(format!("{p}.name"), String::from_utf8_lossy(name));
        let host = made >> 8;
        let host_name = match host {
            0 => "MS-DOS/FAT",
            3 => "Unix",
            7 => "macOS",
            10 => "NTFS(Win)",
            11 => "VFAT",
            _ => "other",
        };
        out.kv(format!("{p}.made_by.host"), format!("{} ({})", host, host_name));
        out.kv(format!("{p}.made_by.spec_version"), format!("{}.{}", (made & 0xFF) / 10, (made & 0xFF) % 10));
        out.kv(format!("{p}.version_needed"), format!("{}.{}", needed / 10, needed % 10));
        out.kv(format!("{p}.gp_flags"), format!("0x{:04X}", gp));
        out.kv(format!("{p}.encrypted"), gp & 1 != 0);
        out.kv(format!("{p}.utf8_names"), gp & 0x800 != 0);
        out.kv(format!("{p}.method"), method);
        out.kv(format!("{p}.dos_time"), dos_iso(date, time));
        out.kv(format!("{p}.crc32"), format!("{:08X}", crc));
        out.kv(format!("{p}.internal_attr"), format!("0x{:04X}", iattr));
        out.kv(format!("{p}.external_attr"), format!("0x{:08X}", eattr));
        out.kv(format!("{p}.dos_attr_dir"), eattr & 0x10 != 0);
        if host == 3 {
            out.kv(format!("{p}.unix_mode"), format!("{:o}", eattr >> 16));
        }
        out.kv(format!("{p}.disk_start"), disk);
        out.kv(format!("{p}.local_header_offset"), loff);
        let mut xs = Out(vec![]);
        let ex = parse_extras(x, &p, &mut xs);
        out.0.extend(xs.0);
        let mut usz64 = usz as u64;
        let mut csz64 = csz as u64;
        let mut loff64 = loff as u64;
        if let Some(z) = &ex.zip64 {
            let mut k = 0;
            let mut take = || {
                let v = z.get(k).copied();
                k += 1;
                v
            };
            if usz == 0xFFFF_FFFF {
                usz64 = take().unwrap_or(0);
            }
            if csz == 0xFFFF_FFFF {
                csz64 = take().unwrap_or(0);
            }
            if loff == 0xFFFF_FFFF {
                loff64 = take().unwrap_or(0);
            }
        }
        out.kv(format!("{p}.compressed_size"), csz64);
        out.kv(format!("{p}.uncompressed_size"), usz64);
        out.kv(format!("{p}.local_header_offset_final"), loff64);
        report_offset(&p, date, time, ex.mtime, out);
        if nlen + xlen + clen > 0 {
            let cm = String::from_utf8_lossy(com);
            if !cm.is_empty() {
                out.kv(format!("{p}.comment"), cm);
            }
        }
        pos = pos + 46 + nlen + xlen + clen;
        i += 1;
    }
    out.kv("zip.cd.entries_parsed", i);
}

fn analyze_zip(head: &[u8], tail: Option<(u64, &[u8])>, total: u64, out: &mut Out) {
    out.kv("format", "zip");
    out.kv("zip.total_size", total);
    zip_local(head, out);
    match tail {
        Some((toff, tb)) => zip_tail(toff, tb, total, out),
        None => out.kv("zip.tail", "not provided"),
    }
}

fn analyze(head: &[u8], tail: Option<(u64, &[u8])>, total: u64, out: &mut Out) {
    if head.len() >= 32 && head[..6] == [0x37, 0x7A, 0xBC, 0xAF, 0x27, 0x1C] {
        analyze_7z(head, tail, total, out);
    } else if head.starts_with(b"PK\x03\x04") || head.starts_with(b"PK\x05\x06") {
        analyze_zip(head, tail, total, out);
    } else {
        out.kv("format", format!("not 7z/zip (sniff: {})", sniff(head)));
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let Some(path) = args.get(1) else {
        eprintln!("usage: sz_zip_meta <file> [head_bytes] [tail_bytes]");
        std::process::exit(2);
    };
    let head_len: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(700 * 1024);
    let tail_len: u64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(700 * 1024);
    let mut f = File::open(path).expect("open");
    let total = f.metadata().map(|m| m.len()).unwrap_or(0);
    let mut head = vec![0u8; total.min(head_len) as usize];
    f.read_exact(&mut head).expect("read head");
    let tl = total.min(tail_len);
    let toff = total - tl;
    let tail_buf = if toff == 0 {
        None
    } else {
        let mut t = vec![0u8; tl as usize];
        f.seek(SeekFrom::Start(toff)).expect("seek");
        f.read_exact(&mut t).expect("read tail");
        Some((toff, t))
    };
    let tail_ref = match &tail_buf {
        Some((o, t)) => Some((*o, t.as_slice())),
        None => Some((0u64, head.as_slice())), // whole file fits in head
    };
    let mut out = Out(vec![]);
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        analyze(&head, tail_ref, total, &mut out);
    }));
    use std::io::Write as _;
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    for l in &out.0 {
        if writeln!(lock, "{l}").is_err() {
            break;
        }
    }
    if res.is_err() {
        println!("!! PANIC caught (bug)");
    }
}
