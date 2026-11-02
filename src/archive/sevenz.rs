//! Inner file names from 7z headers: the start header says where the end header
//! is, and that one may be LZMA packed.
#![cfg_attr(not(test), deny(clippy::indexing_slicing, clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use super::{Encryption, Listing, MAX_ENTRIES, crc32};

const MAX_NEXT: u64 = 16 * 1024 * 1024;
const MAX_DECODED: usize = 16 * 1024 * 1024;
const MAX_FOLDERS: u64 = 4_096;
/// each byte of a header can claim one: these are what keeps a header from turning 8 MB into 64
const MAX_PACK_STREAMS: u64 = 4_096;
const MAX_CODERS: u64 = 32;
/// AES has the longest a real coder uses, 34 bytes. A header's unused folder could otherwise claim most of its size
const MAX_CODER_PROPS: u64 = 64;
const MAX_ROUNDS: usize = 4;
const AES: &[u8] = &[0x06, 0xf1, 0x07, 0x01];
const LZMA: &[u8] = &[0x03, 0x01, 0x01];

/// What the signature header says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Start {
    pub offset: u64,
    pub size: u64,
    pub crc: u32,
}

fn le32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at.checked_add(4)?)?.try_into().ok()?))
}

fn le64(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(at..at.checked_add(8)?)?.try_into().ok()?))
}

/// The signature header. None: short, another magic or major version, or a wrong CRC.
pub fn start(head: &[u8]) -> Option<Start> {
    if !head.starts_with(b"7z\xbc\xaf\x27\x1c") || head.get(6) != Some(&0) {
        return None;
    }
    if le32(head, 8)? != crc32(head.get(12..32)?) {
        return None;
    }
    Some(Start { offset: le64(head, 12)?, size: le64(head, 20)?, crc: le32(head, 28)? })
}

/// 7z's `NUMBER`: leading 1 bits say how many little-endian bytes follow
pub(crate) fn number(b: &[u8], pos: &mut usize) -> Option<u64> {
    let first = *b.get(*pos)?;
    *pos += 1;
    let extra = first.leading_ones();
    let mut value = 0u64;
    for i in 0..extra {
        value |= u64::from(*b.get(*pos)?) << (8 * i);
        *pos += 1;
    }
    if extra < 8 {
        value |= u64::from(first & (0x7fu8 >> extra)) << (8 * extra);
    }
    Some(value)
}

struct Reader<'a> {
    b: &'a [u8],
    p: usize,
}

impl<'a> Reader<'a> {
    fn byte(&mut self) -> Option<u8> {
        let b = *self.b.get(self.p)?;
        self.p += 1;
        Some(b)
    }

    fn number(&mut self) -> Option<u64> {
        number(self.b, &mut self.p)
    }

    fn take(&mut self, n: u64) -> Option<&'a [u8]> {
        let n = usize::try_from(n).ok()?;
        let s = self.b.get(self.p..self.p.checked_add(n)?)?;
        self.p += n;
        Some(s)
    }

    fn left(&self) -> u64 {
        self.b.len().saturating_sub(self.p) as u64
    }

    /// most significant bit first. Needing more bits than there is data: None, before any allocation
    fn bits(&mut self, count: u64) -> Option<Vec<bool>> {
        if count.div_ceil(8) > self.left() {
            return None;
        }
        let bytes = self.take(count.div_ceil(8))?;
        let mut out = Vec::with_capacity(usize::try_from(count).ok()?);
        for i in 0..usize::try_from(count).ok()? {
            let byte = *bytes.get(i / 8)?;
            out.push(byte & (0x80 >> (i % 8)) != 0);
        }
        Some(out)
    }

    fn digests(&mut self, count: u64) -> Option<Vec<Option<u32>>> {
        if count > self.left().saturating_mul(8) {
            return None;
        }
        let defined = if self.byte()? == 0 { self.bits(count)? } else { vec![true; usize::try_from(count).ok()?] };
        defined
            .into_iter()
            .map(|d| {
                if d {
                    let crc = u32::from_le_bytes(self.take(4)?.try_into().ok()?);
                    Some(Some(crc))
                } else {
                    Some(None)
                }
            })
            .collect()
    }
}

#[derive(Default)]
struct Folder {
    aes: bool,
    /// the coder's id and properties when there is one
    coders: Vec<(Vec<u8>, Vec<u8>)>,
    bound_out: Vec<u64>,
    out_streams: u64,
    unpack: Vec<u64>,
    crc: Option<u32>,
}

impl Folder {
    /// its output stream no bind pair uses
    fn size(&self) -> Option<u64> {
        (0..self.out_streams)
            .find(|i| !self.bound_out.contains(i))
            .and_then(|i| self.unpack.get(usize::try_from(i).ok()?).copied())
    }
}

#[derive(Default)]
struct Streams {
    pack_pos: u64,
    pack_sizes: Vec<u64>,
    folders: Vec<Folder>,
    /// each file's size, in order, for files with a stream
    sizes: Vec<u64>,
}

fn folder(r: &mut Reader<'_>) -> Option<Folder> {
    let mut f = Folder::default();
    let coders = r.number()?;
    if coders == 0 || coders > MAX_CODERS {
        return None;
    }
    let (mut in_total, mut out_total) = (0u64, 0u64);
    for _ in 0..coders {
        let flags = r.byte()?;
        if flags & 0xc0 != 0 {
            return None;
        }
        let id = r.take(u64::from(flags & 0x0f))?.to_vec();
        let (mut ins, mut outs) = (1u64, 1u64);
        if flags & 0x10 != 0 {
            ins = r.number()?;
            outs = r.number()?;
            if ins > MAX_CODERS || outs > MAX_CODERS {
                return None;
            }
        }
        let props = if flags & 0x20 != 0 {
            let size = r.number()?;
            if size > MAX_CODER_PROPS {
                return None;
            }
            r.take(size)?.to_vec()
        } else {
            Vec::new()
        };
        if id == AES {
            f.aes = true;
        }
        f.coders.push((id, props));
        in_total += ins;
        out_total += outs;
    }
    if in_total > MAX_CODERS || out_total > MAX_CODERS {
        return None;
    }
    let pairs = out_total.checked_sub(1)?;
    let packed = in_total.checked_sub(pairs)?;
    for _ in 0..pairs {
        let _in_index = r.number()?;
        f.bound_out.push(r.number()?);
    }
    if packed > 1 {
        for _ in 0..packed {
            let _index = r.number()?;
        }
    }
    f.out_streams = out_total;
    Some(f)
}

fn streams(r: &mut Reader<'_>) -> Option<Streams> {
    let mut s = Streams::default();
    let mut id = r.byte()?;
    if id == 0x06 {
        s.pack_pos = r.number()?;
        let count = r.number()?;
        if count > r.left() || count > MAX_PACK_STREAMS {
            return None;
        }
        loop {
            match r.byte()? {
                0x00 => break,
                0x09 => {
                    for _ in 0..count {
                        s.pack_sizes.push(r.number()?);
                    }
                }
                0x0a => {
                    r.digests(count)?;
                }
                _ => return None,
            }
        }
        id = r.byte()?;
    }
    if id == 0x07 {
        if r.byte()? != 0x0b {
            return None;
        }
        let count = r.number()?;
        if count > MAX_FOLDERS || r.byte()? != 0 {
            return None;
        }
        for _ in 0..count {
            s.folders.push(folder(r)?);
        }
        if r.byte()? != 0x0c {
            return None;
        }
        for f in &mut s.folders {
            for _ in 0..f.out_streams {
                f.unpack.push(r.number()?);
            }
        }
        loop {
            match r.byte()? {
                0x00 => break,
                0x0a => {
                    let digests = r.digests(count)?;
                    for (f, d) in s.folders.iter_mut().zip(digests) {
                        f.crc = d;
                    }
                }
                _ => return None,
            }
        }
        id = r.byte()?;
    }
    // by default a folder holds one stream
    let mut per_folder: Vec<u64> = vec![1; s.folders.len()];
    let mut listed = false;
    if id == 0x08 {
        id = r.byte()?;
        if id == 0x0d {
            for n in &mut per_folder {
                *n = r.number()?;
            }
            id = r.byte()?;
        }
        let cap = 4 * MAX_ENTRIES as u64;
        if per_folder.iter().try_fold(0u64, |t, n| t.checked_add(*n))? > cap {
            return None;
        }
        if id == 0x09 {
            listed = true;
            for (f, n) in s.folders.iter().zip(&per_folder) {
                if *n == 0 {
                    continue;
                }
                let mut left = f.size()?;
                for _ in 1..*n {
                    let size = r.number()?;
                    left = left.checked_sub(size)?;
                    s.sizes.push(size);
                }
                s.sizes.push(left);
            }
            id = r.byte()?;
        }
        while id != 0x00 {
            if id != 0x0a {
                return None;
            }
            // every stream but a folder with one stream and a known CRC
            let count: u64 =
                s.folders.iter().zip(&per_folder).map(|(f, n)| if *n == 1 && f.crc.is_some() { 0 } else { *n }).sum();
            r.digests(count)?;
            id = r.byte()?;
        }
        id = r.byte()?;
    }
    if !listed {
        for (f, n) in s.folders.iter().zip(&per_folder) {
            if *n == 1 {
                s.sizes.push(f.size()?);
            }
        }
    }
    (id == 0x00).then_some(s)
}

/// a run of UTF-16LE names, each ended by 00 00
fn names(data: &[u8], count: u64) -> Option<Vec<String>> {
    let mut out = Vec::new();
    let mut units = data.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c));
    for _ in 0..count {
        let mut name = Vec::new();
        loop {
            let unit = units.next()?;
            if unit == 0 {
                break;
            }
            name.push(unit);
        }
        out.push(String::from_utf16_lossy(&name));
    }
    Some(out)
}

fn files(r: &mut Reader<'_>, l: &mut Listing, sizes: &[u64]) -> Option<()> {
    let count = r.number()?;
    // a name takes at least a byte's worth of header: more is a lie, seen before anything is allocated
    if count > r.left().saturating_mul(8) {
        return None;
    }
    let (mut empty_stream, mut empty_file, mut read): (Vec<bool>, Vec<bool>, Vec<String>) = Default::default();
    loop {
        let ty = r.number()?;
        if ty == 0 {
            break;
        }
        let size = r.number()?;
        let mut part = Reader { b: r.take(size)?, p: 0 };
        match ty {
            0x0e => empty_stream = part.bits(count)?,
            0x0f => {
                let empties = empty_stream.iter().filter(|e| **e).count() as u64;
                empty_file = part.bits(empties)?;
            }
            0x11 => {
                if part.byte()? != 0 {
                    return None;
                }
                let rest = part.b.get(part.p..)?;
                let limit = count.min(MAX_ENTRIES as u64 + 1);
                read = names(rest, limit)?;
            }
            _ => {}
        }
    }
    let (mut stream, mut empty) = (0usize, 0usize);
    let mut full = false;
    for (i, name) in read.iter().enumerate() {
        if empty_stream.get(i).copied().unwrap_or(false) {
            let is_file = empty_file.get(empty).copied().unwrap_or(false);
            empty += 1;
            if !is_file {
                continue;
            }
            if l.files.len() >= MAX_ENTRIES {
                full = true;
                break;
            }
            l.push(name, Some(0));
        } else {
            let size = sizes.get(stream).copied();
            stream += 1;
            if l.files.len() >= MAX_ENTRIES {
                full = true;
                break;
            }
            l.push(name, size);
        }
    }
    if read.len() as u64 > MAX_ENTRIES as u64 || full {
        l.complete = false;
        return Some(());
    }
    l.complete = true;
    Some(())
}

fn header(r: &mut Reader<'_>, l: &mut Listing) -> Option<()> {
    let mut id = r.byte()?;
    if id == 0x02 {
        loop {
            let ty = r.number()?;
            if ty == 0 {
                break;
            }
            let size = r.number()?;
            r.take(size)?;
        }
        id = r.byte()?;
    }
    if id == 0x03 {
        streams(r)?;
        id = r.byte()?;
    }
    let mut sizes = Vec::new();
    if id == 0x04 {
        let main = streams(r)?;
        if main.folders.iter().any(|f| f.aes) {
            l.encrypted = true;
            l.encryption = Some(Encryption::SevenZipAes256);
        }
        sizes = main.sizes;
        id = r.byte()?;
    }
    if id == 0x05 {
        files(r, l, &sizes)?;
        id = r.byte()?;
    }
    (id == 0x00).then_some(())
}

/// LZMA, with third-party code fed attacker bytes: a panic in it means a bad archive.
fn decode(props: &[u8], packed: &[u8], unpacked: u64) -> Option<Vec<u8>> {
    let mut input = Vec::with_capacity(props.len() + packed.len());
    input.extend_from_slice(props);
    input.extend_from_slice(packed);
    let options = lzma_rs::decompress::Options {
        unpacked_size: lzma_rs::decompress::UnpackedSize::UseProvided(Some(unpacked)),
        memlimit: Some(MAX_DECODED),
        allow_incomplete: false,
    };
    std::panic::catch_unwind(move || {
        let mut out = Vec::new();
        lzma_rs::lzma_decompress_with_options(&mut input.as_slice(), &mut out, &options).ok()?;
        Some(out)
    })
    .ok()
    .flatten()
}

/// `head` is the archive's first bytes and `tail` its last, either possibly empty.
pub fn list(head: &[u8], tail: &[u8]) -> Listing {
    let mut l = Listing::default();
    if run(head, tail, &mut l).is_none() {
        l.damaged = true;
        l.files.clear();
        l.complete = false;
    }
    l
}

/// None: damaged.
fn run(head: &[u8], tail: &[u8], l: &mut Listing) -> Option<()> {
    let s = start(head)?;
    if s.size == 0 {
        if s.offset == 0 && s.crc == 0 {
            l.complete = true;
            return Some(());
        }
        return None;
    }
    if s.size > MAX_NEXT {
        return None;
    }
    let len = 32u64.checked_add(s.offset)?.checked_add(s.size)?;
    let in_head = usize::try_from(len).ok().and_then(|n| head.get(..n));
    let (window, at): (&[u8], u64) = match in_head {
        Some(w) => (w, 0),
        None => (tail, len.saturating_sub(tail.len() as u64)),
    };
    let size = usize::try_from(s.size).ok()?;
    let Some(next_at) = window.len().checked_sub(size) else {
        l.need_more_tail = true;
        return Some(());
    };
    let next = window.get(next_at..)?;
    if crc32(next) != s.crc {
        return None;
    }

    nested(head, window, at, next.to_vec(), l)
}

/// A decoded header, itself maybe packed again. Each round's bytes go before the next is kept.
fn nested(head: &[u8], window: &[u8], at: u64, first: Vec<u8>, l: &mut Listing) -> Option<()> {
    let mut bytes = first;
    let mut round = 0;
    loop {
        let mut r = Reader { b: &bytes, p: 0 };
        match r.byte()? {
            0x01 => return header(&mut r, l),
            0x17 if round <= MAX_ROUNDS => {}
            _ => return None,
        }
        let encoded = streams(&mut r)?;
        if encoded.folders.iter().any(|f| f.aes) {
            l.encrypted = true;
            l.encryption = Some(Encryption::SevenZipAes256);
            l.headers_encrypted = true;
            return Some(());
        }
        let folder = encoded.folders.first()?;
        let [(id, props)] = folder.coders.as_slice() else { return None };
        if id != LZMA || props.len() != 5 {
            return None;
        }
        let unpacked = folder.size()?;
        if unpacked > MAX_DECODED as u64 {
            return None;
        }
        let pack_len = *encoded.pack_sizes.first()?;
        let from = 32u64.checked_add(encoded.pack_pos)?;
        let to = from.checked_add(pack_len)?;
        let in_window = from
            .checked_sub(at)
            .zip(to.checked_sub(at))
            .and_then(|(a, b)| window.get(usize::try_from(a).ok()?..usize::try_from(b).ok()?));
        let packed = in_window.or_else(|| head.get(usize::try_from(from).ok()?..usize::try_from(to).ok()?));
        let Some(packed) = packed else {
            l.need_more_tail = true;
            return Some(());
        };
        let out = decode(props, packed, unpacked)?;
        if folder.crc.is_some_and(|c| c != crc32(&out)) {
            return None;
        }
        bytes = out;
        round += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::testkit::sz_num;

    /// pack info with `count` one-byte sizes, then the end of it and of the streams
    fn pack_info(count: u64) -> Vec<u8> {
        let mut b = vec![0x06, 0x00];
        b.extend(sz_num(count));
        b.push(0x09);
        b.extend(vec![0u8; count as usize]);
        b.extend([0x00, 0x00]);
        b
    }

    #[test]
    fn a_huge_pack_count_is_refused_before_its_sizes_are_kept() {
        let ok = pack_info(MAX_PACK_STREAMS);
        assert!(streams(&mut Reader { b: &ok, p: 0 }).is_some());
        let over = pack_info(MAX_PACK_STREAMS + 1);
        assert!(streams(&mut Reader { b: &over, p: 0 }).is_none());
    }

    /// one LZMA coder with a property of `size` bytes
    fn coder_with_props(size: u64) -> Vec<u8> {
        let mut b = vec![0x01, 0x23, 0x03, 0x01, 0x01];
        b.extend(sz_num(size));
        b.extend(vec![0u8; size as usize]);
        b
    }

    #[test]
    fn a_huge_coder_property_is_refused() {
        let ok = coder_with_props(MAX_CODER_PROPS);
        assert!(folder(&mut Reader { b: &ok, p: 0 }).is_some());
        let over = coder_with_props(MAX_CODER_PROPS + 1);
        assert!(folder(&mut Reader { b: &over, p: 0 }).is_none());
    }
}
