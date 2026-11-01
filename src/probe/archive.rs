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
}

pub fn parse(data: &[u8]) -> Option<ArchiveHead> {
    if data.starts_with(RAR5) {
        Some(rar5(data))
    } else if data.starts_with(RAR4) {
        Some(rar4(data))
    } else if data.starts_with(SEVEN_Z) {
        Some(ArchiveHead { format: "7z".into(), ..ArchiveHead::default() })
    } else if data.starts_with(ZIP) {
        Some(zip(data))
    } else {
        None
    }
}

fn le16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn le32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn rar4(data: &[u8]) -> ArchiveHead {
    let mut head = ArchiveHead { format: "RAR4".into(), ..ArchiveHead::default() };
    let mut at = RAR4.len();
    while let (Some(kind), Some(flags), Some(size)) = (data.get(at + 2), le16(data, at + 3), le16(data, at + 5)) {
        let size = usize::from(size);
        if size < 7 {
            break;
        }
        match *kind {
            0x73 => {
                if flags & 0x0080 != 0 {
                    head.encrypted = true;
                    head.headers_encrypted = true;
                    break;
                }
            }
            0x74 => {
                // past 4 GiB, the high words sit before the name
                let high_word = |offset: usize| {
                    if flags & 0x0100 != 0 { u64::from(le32(data, at + offset).unwrap_or(0)) << 32 } else { 0 }
                };
                head.packed = le32(data, at + 7).map(|low| u64::from(low) | high_word(32));
                head.unpacked = le32(data, at + 11).map(|low| u64::from(low) | high_word(36));
                head.crc32 = le32(data, at + 16);
                let method = data.get(at + 25).copied();
                head.method = method.map(|m| format!("m{}", m.saturating_sub(0x30)));
                let name_len = usize::from(le16(data, at + 26).unwrap_or(0));
                let high = if flags & 0x0100 != 0 { 8 } else { 0 };
                head.name = data
                    .get(at + 32 + high..at + 32 + high + name_len)
                    .map(|n| String::from_utf8_lossy(n.split(|b| *b == 0).next().unwrap_or(n)).into_owned());
                head.encrypted |= flags & 0x0004 != 0;
                if method == Some(0x30) && !head.encrypted {
                    head.stored_at = Some(at + size);
                }
                break;
            }
            _ => {}
        }
        let add = if flags & 0x8000 != 0 { le32(data, at + 7).unwrap_or(0) as usize } else { 0 };
        at = at.saturating_add(size).saturating_add(add);
        if at >= data.len() {
            break;
        }
    }
    head
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

fn rar5(data: &[u8]) -> ArchiveHead {
    let mut head = ArchiveHead { format: "RAR5".into(), ..ArchiveHead::default() };
    let mut at = RAR5.len();
    loop {
        let start = at + 4;
        let mut p = start;
        let Some(size) = vint(data, &mut p) else { return head };
        let body = p;
        let Some(kind) = vint(data, &mut p) else { return head };
        let Some(flags) = vint(data, &mut p) else { return head };
        let extra = if flags & 1 != 0 { vint(data, &mut p).unwrap_or(0) } else { 0 };
        let packed = if flags & 2 != 0 { vint(data, &mut p).unwrap_or(0) } else { 0 };
        // past what was read: a block this big can't be walked, and a made-up one mustn't wrap around
        let Some(end) = body.checked_add(size as usize).filter(|end| *end <= data.len()) else { return head };
        match kind {
            4 => {
                head.encrypted = true;
                head.headers_encrypted = true;
                return head;
            }
            2 => {
                head.packed = Some(packed);
                let Some(file_flags) = vint(data, &mut p) else { return head };
                let unpacked = vint(data, &mut p);
                let _attributes = vint(data, &mut p);
                if file_flags & 2 != 0 {
                    p += 4;
                }
                if file_flags & 4 != 0 {
                    head.crc32 = le32(data, p);
                    p += 4;
                }
                let Some(info) = vint(data, &mut p) else { return head };
                let _host = vint(data, &mut p);
                let Some(name_len) = vint(data, &mut p) else { return head };
                head.unpacked = unpacked.filter(|_| file_flags & 8 == 0);
                head.name =
                    data.get(p..p.saturating_add(name_len as usize)).map(|n| String::from_utf8_lossy(n).into_owned());
                let method = (info >> 7) & 7;
                head.method = Some(format!("m{method}"));
                let extra_area = end.checked_sub(extra as usize).and_then(|from| data.get(from..end));
                head.encrypted = extra_area.is_some_and(has_crypt_record);
                if method == 0 && !head.encrypted {
                    head.stored_at = Some(end);
                }
                return head;
            }
            5 => return head,
            _ => {}
        }
        at = end.saturating_add(packed as usize);
        if at >= data.len() {
            return head;
        }
    }
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

fn zip(data: &[u8]) -> ArchiveHead {
    let mut head = ArchiveHead { format: "ZIP".into(), ..ArchiveHead::default() };
    let flags = le16(data, 6).unwrap_or(0);
    let method = le16(data, 8).unwrap_or(0);
    head.encrypted = flags & 1 != 0 || method == 99;
    head.method = Some(
        match method {
            0 => "store",
            8 => "deflate",
            9 => "deflate64",
            12 => "bzip2",
            14 => "lzma",
            93 => "zstd",
            99 => "aes",
            _ => "other",
        }
        .to_string(),
    );
    head.crc32 = le32(data, 14);
    head.packed = le32(data, 18).map(u64::from);
    head.unpacked = le32(data, 22).map(u64::from);
    let name_len = usize::from(le16(data, 26).unwrap_or(0));
    let extra_len = usize::from(le16(data, 28).unwrap_or(0));
    head.name = data.get(30..30 + name_len).map(|n| String::from_utf8_lossy(n).into_owned());
    if method == 0 && !head.encrypted {
        head.stored_at = Some(30 + name_len + extra_len);
    }
    head
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
}
