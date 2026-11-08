//! Pictures inside a book or comic posted as one ZIP: an EPUB's cover and plates, a CBZ's pages. The archive step
//! leaves these files alone (they're what the post is, not a wrapper round it), soo their own directory is read here,
//! from the whole file, and the pictures in it are taken out.

use std::io::Read;

/// Entries read at most from one directory, and how big one picture may be once unpacked.
const MAX_ENTRIES: usize = 20_000;
const MAX_PICTURE: usize = 16 << 20;

/// Whether a file is a ZIP whose pictures are worth reading, by its name.
pub fn is_container_name(name: &str) -> bool {
    let lower = name.trim_end_matches('"').to_ascii_lowercase();
    lower.rsplit_once('.').is_some_and(|(_, ext)| matches!(ext, "epub" | "cbz"))
}

/// One picture taken out: its name in the archive, and its bytes.
pub struct Inside {
    pub name: String,
    pub bytes: Vec<u8>,
}

struct Entry {
    name: String,
    method: u16,
    packed: usize,
    unpacked: usize,
    local: usize,
}

/// Up to `max` pictures from a whole ZIP, a cover first, then in the directory's order. Stored and deflated entries;
/// anything encrypted, or packed another way, is passed over.
pub fn pictures(zip: &[u8], max: usize) -> Vec<Inside> {
    let Some(mut entries) = directory(zip) else { return Vec::new() };
    entries.retain(|e| super::is_image_name(&e.name) && e.unpacked > 0 && e.unpacked <= MAX_PICTURE);
    let cover = |e: &Entry| !e.name.to_ascii_lowercase().contains("cover");
    entries.sort_by_key(|e| cover(e));
    entries.iter().filter_map(|e| Some(Inside { name: e.name.clone(), bytes: unpack(zip, e)? })).take(max).collect()
}

fn le16(b: &[u8], at: usize) -> Option<usize> {
    Some(usize::from(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?)))
}

fn le32(b: &[u8], at: usize) -> Option<usize> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?) as usize)
}

/// The central directory, from the end record: its last 64 KiB plus the record hold it.
fn directory(zip: &[u8]) -> Option<Vec<Entry>> {
    let from = zip.len().saturating_sub(22 + 0xFFFF);
    let end = zip[from..].windows(4).rposition(|w| w == b"PK\x05\x06")? + from;
    let (count, at) = (le16(zip, end + 10)?, le32(zip, end + 16)?);
    let mut entries = Vec::with_capacity(count.min(MAX_ENTRIES));
    let mut p = at;
    for _ in 0..count.min(MAX_ENTRIES) {
        if zip.get(p..p + 4)? != b"PK\x01\x02" {
            break;
        }
        let flags = le16(zip, p + 8)?;
        let (name_len, extra, comment) = (le16(zip, p + 28)?, le16(zip, p + 30)?, le16(zip, p + 32)?);
        let name = String::from_utf8_lossy(zip.get(p + 46..p + 46 + name_len)?).into_owned();
        if flags & 1 == 0 {
            entries.push(Entry {
                name,
                method: le16(zip, p + 10)? as u16,
                packed: le32(zip, p + 20)?,
                unpacked: le32(zip, p + 24)?,
                local: le32(zip, p + 42)?,
            });
        }
        p += 46 + name_len + extra + comment;
    }
    Some(entries)
}

/// An entry's bytes, from where its local header says its data starts.
fn unpack(zip: &[u8], e: &Entry) -> Option<Vec<u8>> {
    if zip.get(e.local..e.local + 4)? != b"PK\x03\x04" {
        return None;
    }
    let start = e.local + 30 + le16(zip, e.local + 26)? + le16(zip, e.local + 28)?;
    let data = zip.get(start..start.checked_add(e.packed)?)?;
    match e.method {
        0 => Some(data.to_vec()),
        8 => {
            let mut out = Vec::with_capacity(e.unpacked);
            flate2::read::DeflateDecoder::new(data).take(MAX_PICTURE as u64 + 1).read_to_end(&mut out).ok()?;
            (out.len() == e.unpacked).then_some(out)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    /// A ZIP of (name, bytes, deflated) entries.
    fn zip(files: &[(&str, &[u8], bool)]) -> Vec<u8> {
        let (mut out, mut dir) = (Vec::new(), Vec::new());
        for (name, data, deflate) in files {
            let packed = if *deflate {
                let mut e = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
                e.write_all(data).unwrap();
                e.finish().unwrap()
            } else {
                data.to_vec()
            };
            let method: u16 = if *deflate { 8 } else { 0 };
            let local = out.len() as u32;
            out.extend(b"PK\x03\x04");
            out.extend([20, 0, 0, 0]);
            out.extend(method.to_le_bytes());
            out.extend([0; 8]);
            out.extend((packed.len() as u32).to_le_bytes());
            out.extend((data.len() as u32).to_le_bytes());
            out.extend((name.len() as u16).to_le_bytes());
            out.extend(0u16.to_le_bytes());
            out.extend(name.as_bytes());
            out.extend(&packed);
            dir.extend(b"PK\x01\x02");
            dir.extend([20, 0, 20, 0, 0, 0]);
            dir.extend(method.to_le_bytes());
            dir.extend([0; 8]);
            dir.extend((packed.len() as u32).to_le_bytes());
            dir.extend((data.len() as u32).to_le_bytes());
            dir.extend((name.len() as u16).to_le_bytes());
            dir.extend([0; 12]);
            dir.extend(local.to_le_bytes());
            dir.extend(name.as_bytes());
        }
        let at = out.len() as u32;
        out.extend(&dir);
        out.extend(b"PK\x05\x06");
        out.extend([0; 4]);
        out.extend((files.len() as u16).to_le_bytes());
        out.extend((files.len() as u16).to_le_bytes());
        out.extend((dir.len() as u32).to_le_bytes());
        out.extend(at.to_le_bytes());
        out.extend([0, 0]);
        out
    }

    #[test]
    fn an_epubs_cover_first_then_its_plates() {
        let book = zip(&[
            ("mimetype", b"application/epub+zip", false),
            ("OEBPS/image_a.jpg", b"plate a", true),
            ("OEBPS/c1.xhtml", b"<html/>", true),
            ("cover.jpeg", b"the cover", false),
        ]);
        let got = pictures(&book, 6);
        let names: Vec<&str> = got.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["cover.jpeg", "OEBPS/image_a.jpg"]);
        assert_eq!(got[1].bytes, b"plate a");
        assert_eq!(pictures(&book, 1).len(), 1);
        assert!(pictures(&book[..book.len() - 30], 6).is_empty(), "no end record");
        assert!(
            is_container_name("Some Book (2026).epub") && is_container_name("x.CBZ") && !is_container_name("x.zip")
        );
    }
}
