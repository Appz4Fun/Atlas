//! The first file in an archive, from its first bytes: enough to say how it's packed, and to reach the file
//! itself when it's only stored.

use serde::{Deserialize, Serialize};

const RAR4: &[u8] = b"Rar!\x1a\x07\x00";
const RAR5: &[u8] = b"Rar!\x1a\x07\x01\x00";
const SEVEN_Z: &[u8] = b"7z\xbc\xaf\x27\x1c";
const ZIP: &[u8] = b"PK\x03\x04";

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveHead {
    /// `RAR4`, `RAR5`, `7z`, `ZIP`
    pub format: String,
    pub encrypted: bool,
    pub headers_encrypted: bool,
    /// m0 to m5 for RAR, the ZIP method's name, or None when it can't be read here
    pub method: Option<String>,
    pub name: Option<String>,
    pub unpacked: Option<u64>,
    pub packed: Option<u64>,
    pub crc32: Option<u32>,
    /// where the first file's bytes start, when it's stored rather than compressed
    #[serde(skip)]
    pub stored_at: Option<usize>,
    /// the first file goes on in the next volume: this volume doesn't hold its end
    #[serde(skip)]
    pub continues: bool,
    /// where the file's bytes start, stored or not
    #[serde(skip)]
    pub data_at: Option<usize>,
    /// a folder, not a file
    #[serde(skip)]
    pub dir: bool,
    /// RAR5's compression version: 0 for RAR 5, 1 for RAR 7's longer distances. RAR4's version needed to unpack:
    /// 15, 20, 26 or 29.
    #[serde(skip)]
    pub rar_version: u8,
    /// RAR: decoding this file needs the ones before it
    #[serde(skip)]
    pub solid: bool,
    /// RAR: this file's data began in the volume before
    #[serde(skip)]
    pub split_before: bool,
}

pub fn parse(data: &[u8]) -> Option<ArchiveHead> {
    if data.starts_with(RAR5) {
        Some(first(rar5(data, 1), "RAR5"))
    } else if data.starts_with(RAR4) {
        Some(first(rar4(data, 1), "RAR4"))
    } else if data.starts_with(SEVEN_Z) {
        Some(ArchiveHead { format: "7z".into(), ..ArchiveHead::default() })
    } else if data.starts_with(ZIP) {
        Some(first(zip(data, 1), "ZIP"))
    } else {
        None
    }
}

/// Every file whose header is in `data`, in order, at most [`MAX_ENTRIES`]. A RAR with encrypted headers gives
/// one entry that says so. Empty for anything but RAR and ZIP.
pub fn entries(data: &[u8]) -> Vec<ArchiveHead> {
    if data.starts_with(RAR5) {
        rar5(data, MAX_ENTRIES)
    } else if data.starts_with(RAR4) {
        rar4(data, MAX_ENTRIES)
    } else if data.starts_with(ZIP) {
        zip(data, MAX_ENTRIES)
    } else {
        Vec::new()
    }
}

const MAX_ENTRIES: usize = 256;

/// A file name from an archive's header, cut to what a name needs before any of it is copied.
fn name_of(raw: &[u8]) -> String {
    let raw = &raw[..raw.len().min(crate::archive::MAX_NAME_BYTES)];
    String::from_utf8_lossy(raw).into_owned()
}

fn first(heads: Vec<ArchiveHead>, format: &str) -> ArchiveHead {
    heads.into_iter().next().unwrap_or_else(|| ArchiveHead { format: format.into(), ..ArchiveHead::default() })
}

fn le16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn le32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn rar4(data: &[u8], max: usize) -> Vec<ArchiveHead> {
    let mut out = Vec::new();
    let mut at = RAR4.len();
    let mut solid_archive = false;
    let mut files = 0;
    while let (Some(kind), Some(flags), Some(size)) = (data.get(at + 2), le16(data, at + 3), le16(data, at + 5)) {
        let size = usize::from(size);
        if size < 7 || out.len() >= max {
            break;
        }
        let high_word = |offset: usize| {
            if flags & 0x0100 != 0 { u64::from(le32(data, at + offset).unwrap_or(0)) << 32 } else { 0 }
        };
        match *kind {
            0x73 => {
                solid_archive = flags & 0x0008 != 0;
                if flags & 0x0080 != 0 {
                    out.push(ArchiveHead {
                        format: "RAR4".into(),
                        encrypted: true,
                        headers_encrypted: true,
                        ..ArchiveHead::default()
                    });
                    break;
                }
            }
            0x74 => {
                let mut head = ArchiveHead { format: "RAR4".into(), ..ArchiveHead::default() };
                // past 4 GiB, the high words sit before the name
                head.packed = le32(data, at + 7).map(|low| u64::from(low) | high_word(32));
                head.unpacked = le32(data, at + 11).map(|low| u64::from(low) | high_word(36));
                head.crc32 = le32(data, at + 16);
                let method = data.get(at + 25).copied();
                head.method = method.map(|m| format!("m{}", m.saturating_sub(0x30)));
                head.rar_version = data.get(at + 24).copied().unwrap_or(0);
                // before 2.0 a file's own flag isn't read: every file after the first in a solid archive is solid
                head.solid = if head.rar_version < 20 { solid_archive && files > 0 } else { flags & 0x0010 != 0 };
                head.split_before = flags & 0x0001 != 0;
                files += 1;
                let name_len = usize::from(le16(data, at + 26).unwrap_or(0));
                let high = if flags & 0x0100 != 0 { 8 } else { 0 };
                // within this header, and no longer than a name needs
                head.name = data
                    .get(at + 32 + high..(at + 32 + high + name_len).min(at + size))
                    .map(|n| name_of(n.split(|b| *b == 0).next().unwrap_or(n)));
                head.encrypted = flags & 0x0004 != 0;
                head.continues = flags & 0x0002 != 0;
                head.dir = flags & 0x00e0 == 0x00e0;
                head.data_at = Some(at + size);
                if method == Some(0x30) && !head.encrypted {
                    head.stored_at = Some(at + size);
                }
                out.push(head);
            }
            _ => {}
        }
        let add = if flags & 0x8000 != 0 {
            u64::from(le32(data, at + 7).unwrap_or(0)) | if *kind == 0x74 { high_word(32) } else { 0 }
        } else {
            0
        };
        let Some(next) = usize::try_from(add).ok().and_then(|add| at.checked_add(size)?.checked_add(add)) else {
            break;
        };
        at = next;
        if at >= data.len() {
            break;
        }
    }
    out
}

fn vint(b: &[u8], at: &mut usize) -> Option<u64> {
    let mut value = 0u64;
    for shift in (0..64).step_by(7) {
        let byte = *b.get(*at)?;
        *at += 1;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some(value);
        }
    }
    None
}

fn rar5(data: &[u8], max: usize) -> Vec<ArchiveHead> {
    let mut out = Vec::new();
    let mut at = RAR5.len();
    while out.len() < max {
        let start = at + 4;
        let mut p = start;
        let Some(size) = vint(data, &mut p) else { break };
        let body = p;
        let Some(kind) = vint(data, &mut p) else { break };
        let Some(flags) = vint(data, &mut p) else { break };
        let extra = if flags & 1 != 0 { vint(data, &mut p).unwrap_or(0) } else { 0 };
        let packed = if flags & 2 != 0 { vint(data, &mut p).unwrap_or(0) } else { 0 };
        // past what was read: a block this big can't be walked, and a made-up one mustn't wrap around
        let Some(end) = usize::try_from(size).ok().and_then(|s| body.checked_add(s)).filter(|end| *end <= data.len())
        else {
            break;
        };
        match kind {
            4 => {
                out.push(ArchiveHead {
                    format: "RAR5".into(),
                    encrypted: true,
                    headers_encrypted: true,
                    ..ArchiveHead::default()
                });
                break;
            }
            2 => {
                let Some(head) = rar5_file(data, p, end, extra, packed, flags) else { break };
                out.push(head);
            }
            5 => break,
            _ => {}
        }
        let Some(next) = usize::try_from(packed).ok().and_then(|packed| end.checked_add(packed)) else { break };
        at = next;
        if at >= data.len() {
            break;
        }
    }
    out
}

fn rar5_file(data: &[u8], mut p: usize, end: usize, extra: u64, packed: u64, flags: u64) -> Option<ArchiveHead> {
    let mut head = ArchiveHead { format: "RAR5".into(), ..ArchiveHead::default() };
    head.packed = Some(packed);
    // the block header's "data goes on in the next volume"
    head.continues = flags & 0x10 != 0;
    head.split_before = flags & 0x08 != 0;
    let file_flags = vint(data, &mut p)?;
    let unpacked = vint(data, &mut p);
    let _attributes = vint(data, &mut p);
    if file_flags & 2 != 0 {
        p += 4;
    }
    if file_flags & 4 != 0 {
        head.crc32 = le32(data, p);
        p += 4;
    }
    let info = vint(data, &mut p)?;
    let _host = vint(data, &mut p);
    let name_len = vint(data, &mut p)?;
    head.unpacked = unpacked.filter(|_| file_flags & 8 == 0);
    head.dir = file_flags & 1 != 0;
    // within this header, and no longer than a name needs
    head.name = usize::try_from(name_len)
        .ok()
        .and_then(|n| p.checked_add(n))
        .filter(|to| *to <= end)
        .and_then(|to| data.get(p..to))
        .map(name_of);
    let method = (info >> 7) & 7;
    head.method = Some(format!("m{method}"));
    head.rar_version = (info & 0x3f) as u8;
    head.solid = info & 0x40 != 0;
    let extra_area = usize::try_from(extra).ok().and_then(|e| end.checked_sub(e)).and_then(|from| data.get(from..end));
    head.encrypted = extra_area.is_some_and(has_crypt_record);
    head.data_at = Some(end);
    if method == 0 && !head.encrypted {
        head.stored_at = Some(end);
    }
    Some(head)
}

fn has_crypt_record(area: &[u8]) -> bool {
    let mut p = 0;
    while p < area.len() {
        let Some(size) = vint(area, &mut p).filter(|s| *s > 0) else { return false };
        let mut at = p;
        if vint(area, &mut at) == Some(1) {
            return true;
        }
        p = p.saturating_add(size as usize);
    }
    false
}

fn zip(data: &[u8], max: usize) -> Vec<ArchiveHead> {
    let mut out = Vec::new();
    let mut at = 0usize;
    while out.len() < max && data.get(at..at + 4) == Some(ZIP) {
        let Some(rest) = data.get(at..) else { break };
        let mut head = zip_entry(rest);
        let (Some(data_at), Some(packed), false) = (head.data_at, head.packed, head.continues) else {
            head.data_at = head.data_at.map(|d| d + at);
            head.stored_at = head.stored_at.map(|d| d + at);
            out.push(head);
            break;
        };
        head.data_at = Some(data_at + at);
        head.stored_at = head.stored_at.map(|d| d + at);
        out.push(head);
        let Some(next) = usize::try_from(packed).ok().and_then(|p| (at + data_at).checked_add(p)) else { break };
        at = next;
    }
    out
}

/// One local header. `continues` here means the sizes come after the data, soo the next header can't be found.
fn zip_entry(data: &[u8]) -> ArchiveHead {
    let mut head = ArchiveHead { format: "ZIP".into(), ..ArchiveHead::default() };
    let flags = le16(data, 6).unwrap_or(0);
    let method = le16(data, 8).unwrap_or(0);
    head.encrypted = flags & 1 != 0 || method == 99;
    head.method = Some(zip_method(method).to_string());
    // with this flag, the sizes and checksum come after the data, and here they're zeros
    let later = flags & 0x0008 != 0;
    head.crc32 = le32(data, 14).filter(|_| !later);
    let name_len = usize::from(le16(data, 26).unwrap_or(0));
    let extra_len = usize::from(le16(data, 28).unwrap_or(0));
    if let (Some(packed), Some(unpacked), false) = (le32(data, 18), le32(data, 22), later) {
        // past 4 GiB these are markers, and the extra field has the sizes. Without it they stay unknown.
        let extra = data.get(30 + name_len..30 + name_len + extra_len).unwrap_or_default();
        let (long_unpacked, long_packed) = crate::archive::zip::zip64_sizes(extra, unpacked, packed);
        head.unpacked = Some(long_unpacked).filter(|size| *size != u64::from(u32::MAX));
        head.packed = Some(long_packed).filter(|size| *size != u64::from(u32::MAX));
    }
    head.name = data.get(30..30 + name_len).map(name_of);
    head.dir = head.name.as_deref().is_some_and(|n| n.ends_with('/'));
    head.data_at = Some(30 + name_len + extra_len);
    head.continues = later;
    if method == 0 && !head.encrypted {
        head.stored_at = Some(30 + name_len + extra_len);
    }
    head
}

/// The method's name, as `ArchiveHead::method` says it.
pub fn zip_method(method: u16) -> &'static str {
    match method {
        0 => "store",
        8 => "deflate",
        9 => "deflate64",
        12 => "bzip2",
        14 => "lzma",
        93 => "zstd",
        95 => "xz",
        98 => "ppmd",
        99 => "aes",
        _ => "other",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveFormat {
    Rar5,
    SevenZip,
    Zip,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecompressError {
    ExpansionRatioBreached,
    DecodeError(String),
    LimitReached,
    UnexpectedEof,
}

pub struct ArchiveStream;

impl ArchiveStream {
    pub fn decompress(
        format: ArchiveFormat,
        input: &[u8],
        limit: usize,
    ) -> Result<Vec<u8>, DecompressError> {
        let max_allowed = limit.min(crate::probe::nest::MAX_DECODED);
        let max_ratio_limit = input.len().saturating_mul(crate::probe::nest::MAX_RATIO);
        let effective_limit = max_allowed.min(max_ratio_limit);
        let limit_err = if max_ratio_limit <= max_allowed {
            DecompressError::ExpansionRatioBreached
        } else {
            DecompressError::LimitReached
        };

        match format {
            ArchiveFormat::Zip => {
                use flate2::read::DeflateDecoder;
                use std::io::Read;
                let mut decoder = DeflateDecoder::new(input);
                let mut buf = Vec::new();
                let mut chunk = [0u8; 8192];
                loop {
                    let to_read = chunk.len().min(effective_limit.saturating_sub(buf.len()));
                    if to_read == 0 {
                        let mut test_chunk = [0u8; 1];
                        if let Ok(1) = decoder.read(&mut test_chunk) {
                            return Err(limit_err);
                        }
                        break;
                    }
                    match decoder.read(&mut chunk[..to_read]) {
                        Ok(0) => break,
                        Ok(n) => {
                            buf.extend_from_slice(&chunk[..n]);
                            if buf.len() > max_ratio_limit {
                                return Err(DecompressError::ExpansionRatioBreached);
                            }
                        }
                        Err(e) => {
                            if buf.is_empty() {
                                return Err(DecompressError::DecodeError(e.to_string()));
                            }
                            break;
                        }
                    }
                }
                Ok(buf)
            }
            ArchiveFormat::SevenZip => {
                let (buf, end) = crate::probe::lzma::lzma2(input, effective_limit.saturating_add(1));
                if end == crate::probe::lzma::End::Limit || buf.len() > effective_limit {
                    return Err(limit_err);
                }
                if buf.is_empty() && end != crate::probe::lzma::End::Done {
                    return Err(DecompressError::DecodeError("lzma2 decode failed".into()));
                }
                Ok(buf)
            }
            ArchiveFormat::Rar5 => {
                let (buf, end) =
                    crate::probe::codec::rar5::rar5(input, false, None, effective_limit.saturating_add(1));
                if end == crate::probe::lzma::End::Limit || buf.len() > effective_limit {
                    return Err(limit_err);
                }
                if buf.is_empty() && end != crate::probe::lzma::End::Done {
                    return Err(DecompressError::DecodeError("rar5 decode failed".into()));
                }
                Ok(buf)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn made_up_rar_lengths_end_the_walk() {
        // a RAR5 block claiming nearly all of memory, for a walk that would wrap around
        let mut data = RAR5.to_vec();
        data.extend([0; 4]);
        data.extend([0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01, 1, 2, 0xff, 0xff, 0xff, 0xff, 0x7f]);
        data.resize(64, 0);
        assert_eq!(parse(&data).unwrap().format, "RAR5");
        // and a RAR4 one
        let mut data = RAR4.to_vec();
        data.extend([0, 0, 0x7a, 0x00, 0x80, 0x07, 0x00, 0xff, 0xff, 0xff, 0xff]);
        data.resize(64, 0);
        assert_eq!(parse(&data).unwrap().format, "RAR4");
    }

    #[test]
    fn a_streamed_zip_has_no_sizes_up_front() {
        let mut data = ZIP.to_vec();
        data.resize(31, 0);
        data[6..8].copy_from_slice(&8u16.to_le_bytes());
        data[26..28].copy_from_slice(&1u16.to_le_bytes());
        data[30] = b'a';
        let head = parse(&data).unwrap();
        assert_eq!((head.unpacked, head.packed, head.crc32), (None, None, None));
    }

    #[test]
    fn a_zip64_file_reads_its_sizes_from_the_extra_field() {
        let mut data = ZIP.to_vec();
        data.resize(30, 0);
        data[18..26].copy_from_slice(&[0xff; 8]);
        data[26..28].copy_from_slice(&1u16.to_le_bytes());
        data[28..30].copy_from_slice(&20u16.to_le_bytes());
        data.push(b'a');
        assert_eq!(parse(&data).unwrap().unpacked, None, "no extra field read");
        data.extend(1u16.to_le_bytes());
        data.extend(16u16.to_le_bytes());
        data.extend(((8u64 << 30) + 456).to_le_bytes());
        data.extend(7u64.to_le_bytes());
        let head = parse(&data).unwrap();
        assert_eq!((head.unpacked, head.packed), (Some((8 << 30) + 456), Some(7)));
    }

    #[test]
    fn a_big_rar4_file_keeps_its_high_size_words() {
        let mut data = RAR4.to_vec();
        let at = data.len();
        data.resize(at + 41, 0);
        data[at + 2] = 0x74;
        data[at + 3..at + 5].copy_from_slice(&0x0100u16.to_le_bytes());
        data[at + 5..at + 7].copy_from_slice(&41u16.to_le_bytes());
        data[at + 11..at + 15].copy_from_slice(&456u32.to_le_bytes());
        data[at + 25] = 0x30;
        data[at + 26..at + 28].copy_from_slice(&1u16.to_le_bytes());
        data[at + 36..at + 40].copy_from_slice(&2u32.to_le_bytes());
        data[at + 40] = b'a';
        assert_eq!(parse(&data).unwrap().unpacked, Some((2u64 << 32) + 456));
    }

    #[test]
    fn real_rar5_headers_read_as_encrypted() {
        let file = parse(include_bytes!("../../tests/fixtures/rar5/rar5-psw.rar")).unwrap();
        assert_eq!(file.format, "RAR5");
        assert!(file.encrypted && !file.headers_encrypted);
        assert!(file.name.is_some());
        let names = parse(include_bytes!("../../tests/fixtures/rar5/rar5-hpsw.rar")).unwrap();
        assert!(names.encrypted && names.headers_encrypted && names.name.is_none());
    }

    fn rar4_stored(method: u8, flags: u16) -> Vec<u8> {
        let name = b"movie.mkv";
        let size = 32 + name.len();
        let mut h = vec![0u8, 0, 0x74];
        h.extend(flags.to_le_bytes());
        h.extend((size as u16).to_le_bytes());
        h.extend(5u32.to_le_bytes());
        h.extend(5u32.to_le_bytes());
        h.push(3);
        h.extend(0xdead_beefu32.to_le_bytes());
        h.extend(0u32.to_le_bytes());
        h.push(29);
        h.push(method);
        h.extend((name.len() as u16).to_le_bytes());
        h.extend(0u32.to_le_bytes());
        h.extend(name);
        [RAR4, &[0, 0, 0x73, 0, 0, 13, 0, 0, 0, 0, 0, 0, 0], h.as_slice(), b"inner"].concat()
    }

    #[test]
    fn a_stored_rar4_points_at_its_file() {
        let data = rar4_stored(0x30, 0);
        let head = parse(&data).unwrap();
        assert_eq!(head.method.as_deref(), Some("m0"));
        assert_eq!(head.name.as_deref(), Some("movie.mkv"));
        assert_eq!(head.crc32, Some(0xdead_beef));
        assert_eq!(&data[head.stored_at.unwrap()..], b"inner");
        let compressed = parse(&rar4_stored(0x33, 0)).unwrap();
        assert_eq!(compressed.method.as_deref(), Some("m3"));
        assert_eq!(compressed.stored_at, None);
        assert!(parse(&rar4_stored(0x30, 4)).unwrap().encrypted);
        assert!(!head.continues);
        assert!(parse(&rar4_stored(0x30, 2)).unwrap().continues, "split after");
    }

    #[test]
    fn zip_says_its_method() {
        let mut z = b"PK\x03\x04\x14\0\0\0\0\0\0\0\0\0".to_vec();
        z.extend(7u32.to_le_bytes());
        z.extend(5u32.to_le_bytes());
        z.extend(5u32.to_le_bytes());
        z.extend(1u16.to_le_bytes());
        z.extend(0u16.to_le_bytes());
        z.extend(b"a");
        z.extend(b"inner");
        let head = parse(&z).unwrap();
        assert_eq!((head.method.as_deref(), head.crc32, head.name.as_deref()), (Some("store"), Some(7), Some("a")));
        assert_eq!(&z[head.stored_at.unwrap()..], b"inner");
        assert_eq!(parse(b"plain"), None);
    }

    #[test]
    fn seven_zip_reports_limit_and_expansion_ratio_breach() {
        // 10,000 bytes of b'A' compressed with LZMA2 (51 bytes -> ~196:1 expansion ratio)
        let bomb_lzma2: [u8; 51] = [
            224, 39, 15, 0, 43, 93, 0, 32, 239, 251, 191, 254, 163, 177, 94, 229, 248, 63, 178, 170, 38, 85, 248, 104,
            112, 65, 112, 21, 15, 141, 253, 30, 76, 27, 138, 66, 183, 25, 244, 105, 24, 113, 174, 102, 35, 134, 188,
            244, 42, 0, 0,
        ];

        // 1. Explicit limit smaller than 100x input (64 < 5,100): must return LimitReached
        let res_limit = ArchiveStream::decompress(ArchiveFormat::SevenZip, &bomb_lzma2, 64);
        assert_eq!(res_limit, Err(DecompressError::LimitReached));

        // 2. Stream expanding beyond 100:1 input ratio (10,000 > 5,100): must return ExpansionRatioBreached
        let res_ratio = ArchiveStream::decompress(ArchiveFormat::SevenZip, &bomb_lzma2, 100_000);
        assert_eq!(res_ratio, Err(DecompressError::ExpansionRatioBreached));

        // 3. Valid LZMA2 stream within limit and ratio: must succeed
        let small_lzma2 = [0x01, 0x00, 0x03, b't', b'e', b's', b't', 0x00];
        let res_ok = ArchiveStream::decompress(ArchiveFormat::SevenZip, &small_lzma2, 64);
        assert_eq!(res_ok.as_deref(), Ok(&b"test"[..]));
    }
}
