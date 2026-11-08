//! What a JPEG says of itself before its first scan: the frame (size, precision, coding process, sampling), JFIF and
//! Adobe markers, the comment, the EXIF fields that say who made it and with what, the XMP ids that follow an image
//! through edits, the ICC profile's name, and a quality guess from the luma quantization table. Only headers are read:
//! the entropy-coded data after the first SOS is never decoded, soo a picture of any size costs the same.
//!
//! Everything is bounds-checked against the bytes given: a cut or broken file gives what was read before the break.

use std::collections::BTreeMap;

use crate::video::jpeg::{LUMA_Q, ZIGZAG, scaled};

/// A text field longer than this is cut: a comment or a description is a line or two, and a posted file can say
/// anything.
const MAX_TEXT: usize = 512;
/// IFDs followed at most, and entries read from one: a loop of offsets or a huge count ends here.
const MAX_IFDS: usize = 8;
const MAX_ENTRIES: usize = 512;

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct JpegInfo {
    pub width: u16,
    pub height: u16,
    /// bits a sample
    pub precision: u8,
    pub components: u8,
    /// `baseline`, `extended`, `progressive` or `lossless`, with `arithmetic` after it when not Huffman coded
    pub process: String,
    /// `4:2:0` and the like, for three components
    pub sampling: Option<String>,
    /// JFIF's version, as `1.02`
    pub jfif: Option<String>,
    /// pixels an inch, when JFIF or EXIF say so
    pub dpi: Option<(u32, u32)>,
    /// an APP14 Adobe marker, and its colour transform: 0 none (RGB or CMYK), 1 YCbCr, 2 YCCK
    pub adobe_transform: Option<u8>,
    pub comment: Option<String>,
    pub exif: Exif,
    pub xmp: Xmp,
    /// the ICC profile's description: `sRGB IEC61966-2.1`, `Display P3`
    pub icc: Option<String>,
    /// The IJG quality the luma table was made with, 1 to 100; `exact` when the table is that quality's to the byte.
    /// An encoder with tables of its own gives the nearest.
    pub quality: Option<u8>,
    pub quality_exact: bool,
    /// the quantization tables as found, hashed: one encoder at one setting gives the same
    pub tables_hash: Option<u64>,
    /// bytes after the last EOI marker: zero for a clean file, more when something was stuck on the end
    pub after_end: usize,
}

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Exif {
    pub make: Option<String>,
    pub model: Option<String>,
    pub software: Option<String>,
    /// DateTimeOriginal, else DateTime, as written: `2024:05:01 12:00:00`
    pub taken: Option<String>,
    pub artist: Option<String>,
    pub copyright: Option<String>,
    pub description: Option<String>,
    pub orientation: Option<u16>,
    pub lens: Option<String>,
    pub serial: Option<String>,
    /// ImageUniqueID: 32 hex digits a camera or editor stamps on one picture
    pub unique_id: Option<String>,
    /// latitude and longitude in degrees, south and west negative
    pub gps: Option<(f64, f64)>,
    /// the size of the preview picture in IFD1, when there is one
    pub thumbnail_bytes: Option<u32>,
}

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Xmp {
    pub creator_tool: Option<String>,
    pub document_id: Option<String>,
    pub instance_id: Option<String>,
    pub original_document_id: Option<String>,
}

impl Exif {
    fn is_empty(&self) -> bool {
        *self == Exif::default()
    }
}

/// Whether bytes start as a JPEG does.
pub fn is_jpeg(bytes: &[u8]) -> bool {
    bytes.len() >= 3 && bytes[0] == 0xFF && bytes[1] == 0xD8 && bytes[2] == 0xFF
}

/// Reads a JPEG's headers. None when it isn't one, or no frame header came before the data ran out.
pub fn read(bytes: &[u8]) -> Option<JpegInfo> {
    if !is_jpeg(bytes) {
        return None;
    }
    let mut info = JpegInfo::default();
    let mut tables: BTreeMap<u8, [u16; 64]> = BTreeMap::new();
    let mut icc_chunks: BTreeMap<u8, &[u8]> = BTreeMap::new();
    let mut framed = false;
    let mut at = 2;
    while at + 4 <= bytes.len() {
        if bytes[at] != 0xFF {
            break;
        }
        let marker = bytes[at + 1];
        // fill bytes before a marker
        if marker == 0xFF {
            at += 1;
            continue;
        }
        // markers with no length: TEM and the restarts
        if marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
            at += 2;
            continue;
        }
        if marker == 0xD9 {
            break;
        }
        let len = usize::from(u16::from_be_bytes([bytes[at + 2], bytes[at + 3]]));
        if len < 2 {
            break;
        }
        let end = (at + 2 + len).min(bytes.len());
        let body = &bytes[at + 4..end];
        match marker {
            0xC0..=0xCF if marker != 0xC4 && marker != 0xC8 && marker != 0xCC => {
                if !framed && frame(marker, body, &mut info) {
                    framed = true;
                }
            }
            0xDB => quant_tables(body, &mut tables),
            0xE0 if info.jfif.is_none() => jfif(body, &mut info),
            0xE1 => {
                if let Some(tiff) = body.strip_prefix(b"Exif\0\0") {
                    exif(tiff, &mut info);
                } else if let Some(xml) = body.strip_prefix(b"http://ns.adobe.com/xap/1.0/\0") {
                    xmp(xml, &mut info.xmp);
                }
            }
            0xE2 => {
                if let Some(chunk) = body.strip_prefix(b"ICC_PROFILE\0")
                    && chunk.len() > 2
                {
                    icc_chunks.entry(chunk[0]).or_insert(&chunk[2..]);
                }
            }
            0xEE if body.starts_with(b"Adobe") && body.len() >= 12 => info.adobe_transform = Some(body[11]),
            0xFE if info.comment.is_none() => info.comment = text(body),
            // the first scan: what follows is coded data, and every header that matters came before it
            0xDA => break,
            _ => {}
        }
        if end == bytes.len() && at + 2 + len > bytes.len() {
            break;
        }
        at = end;
    }
    if !framed {
        return None;
    }
    if !icc_chunks.is_empty() {
        let profile: Vec<u8> = icc_chunks.values().flat_map(|c| c.iter().copied()).collect();
        info.icc = icc_description(&profile);
    }
    quality(&tables, &mut info);
    info.after_end = after_end(bytes);
    Some(info)
}

/// A frame header (SOF0 to SOF15): sizes, precision and how each component is sampled.
fn frame(marker: u8, body: &[u8], info: &mut JpegInfo) -> bool {
    if body.len() < 6 {
        return false;
    }
    info.precision = body[0];
    info.height = u16::from_be_bytes([body[1], body[2]]);
    info.width = u16::from_be_bytes([body[3], body[4]]);
    info.components = body[5];
    let kind = match marker & 0x03 {
        0 if marker & 0x08 == 0 && marker == 0xC0 => "baseline",
        0 | 1 => "extended",
        2 => "progressive",
        _ => "lossless",
    };
    info.process = if marker & 0x08 != 0 { format!("{kind} arithmetic") } else { kind.to_string() };
    let factors: Vec<(u8, u8)> = body[6..]
        .as_chunks::<3>()
        .0
        .iter()
        .take(usize::from(info.components))
        .map(|c| (c[1] >> 4, c[1] & 0x0F))
        .collect();
    info.sampling = sampling(&factors);
    true
}

/// The usual name for a three-component sampling, from each component's factors: luma first, then the two chromas,
/// which have to match.
fn sampling(factors: &[(u8, u8)]) -> Option<String> {
    let [(yh, yv), c1, c2] = factors else { return None };
    if c1 != c2 || c1.0 == 0 || c1.1 == 0 {
        return None;
    }
    let name = match (yh / c1.0, yv / c1.1, yh % c1.0 == 0 && yv % c1.1 == 0) {
        (1, 1, true) => "4:4:4",
        (2, 1, true) => "4:2:2",
        (2, 2, true) => "4:2:0",
        (1, 2, true) => "4:4:0",
        (4, 1, true) => "4:1:1",
        (4, 2, true) => "4:1:0",
        _ => return Some(format!("{yh}x{yv},{}x{}", c1.0, c1.1)),
    };
    Some(name.to_string())
}

/// A DQT segment's tables, each into natural order. 16-bit tables are kept as they are, soo they don't pass for 8-bit.
fn quant_tables(mut body: &[u8], tables: &mut BTreeMap<u8, [u16; 64]>) {
    while let Some((&pq_tq, rest)) = body.split_first() {
        let wide = pq_tq >> 4 != 0;
        let size = if wide { 128 } else { 64 };
        if rest.len() < size {
            return;
        }
        let mut table = [0u16; 64];
        for (k, &natural) in ZIGZAG.iter().enumerate() {
            table[natural] = if wide { u16::from_be_bytes([rest[2 * k], rest[2 * k + 1]]) } else { u16::from(rest[k]) };
        }
        tables.insert(pq_tq & 0x0F, table);
        body = &rest[size..];
    }
}

/// The IJG quality whose scaled Annex K luma table is nearest table 0, by the sum of the differences.
fn quality(tables: &BTreeMap<u8, [u16; 64]>, info: &mut JpegInfo) {
    if tables.is_empty() {
        return;
    }
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for (id, table) in tables {
        for v in std::iter::once(u16::from(*id)).chain(table.iter().copied()) {
            for b in v.to_le_bytes() {
                hash = (hash ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
            }
        }
    }
    info.tables_hash = Some(hash);
    let Some(luma) = tables.get(&0) else { return };
    let (best, off) = (1..=100u8)
        .map(|q| {
            let want = scaled(&LUMA_Q, q);
            (q, want.iter().zip(luma).map(|(&w, &got)| u32::from(w).abs_diff(u32::from(got))).sum::<u32>())
        })
        .min_by_key(|&(q, off)| (off, std::cmp::Reverse(q)))
        .unwrap_or((0, u32::MAX));
    info.quality = Some(best);
    info.quality_exact = off == 0;
}

/// APP0: JFIF's version and density. A JFXX extension (its own APP0) says nothing more worth keeping.
fn jfif(body: &[u8], info: &mut JpegInfo) {
    let Some(rest) = body.strip_prefix(b"JFIF\0") else { return };
    if rest.len() < 7 {
        return;
    }
    info.jfif = Some(format!("{}.{:02}", rest[0], rest[1]));
    let (x, y) = (u32::from(u16::from_be_bytes([rest[3], rest[4]])), u32::from(u16::from_be_bytes([rest[5], rest[6]])));
    let dpi = match rest[2] {
        1 => Some((x, y)),
        // dots a centimetre
        2 => Some(((x * 254 + 50) / 100, (y * 254 + 50) / 100)),
        _ => None,
    };
    if dpi.is_some_and(|(x, y)| x > 0 && y > 0) {
        info.dpi = dpi;
    }
}

/// A text field: ASCII or UTF-8 up to its first NUL, trimmed, cut to `MAX_TEXT`. None when nothing's left.
fn text(raw: &[u8]) -> Option<String> {
    let raw = raw.split(|&b| b == 0).next().unwrap_or_default();
    let text = String::from_utf8_lossy(raw);
    let text: String = text.trim().chars().filter(|c| !c.is_control()).take(MAX_TEXT).collect();
    (!text.is_empty()).then_some(text)
}

/// A TIFF structure's byte order.
#[derive(Clone, Copy)]
struct Tiff<'a> {
    bytes: &'a [u8],
    little: bool,
}

impl<'a> Tiff<'a> {
    fn u16(&self, at: usize) -> Option<u16> {
        let b: [u8; 2] = self.bytes.get(at..at + 2)?.try_into().ok()?;
        Some(if self.little { u16::from_le_bytes(b) } else { u16::from_be_bytes(b) })
    }

    fn u32(&self, at: usize) -> Option<u32> {
        let b: [u8; 4] = self.bytes.get(at..at + 4)?.try_into().ok()?;
        Some(if self.little { u32::from_le_bytes(b) } else { u32::from_be_bytes(b) })
    }

    /// An IFD's entries, and the offset of the next IFD (0 for none).
    fn entries(&self, at: usize) -> Option<(Vec<Entry<'a>>, usize)> {
        let count = usize::from(self.u16(at)?).min(MAX_ENTRIES);
        let mut entries = Vec::with_capacity(count);
        for i in 0..count {
            let e = at + 2 + i * 12;
            let (Some(tag), Some(kind), Some(n)) = (self.u16(e), self.u16(e + 2), self.u32(e + 4)) else { break };
            let unit = match kind {
                1 | 2 | 6 | 7 => 1,
                3 | 8 => 2,
                4 | 9 | 11 => 4,
                5 | 10 | 12 => 8,
                _ => continue,
            };
            let size = (n as usize).checked_mul(unit)?;
            // four bytes or fewer sit in the entry itself
            let data = if size <= 4 {
                self.bytes.get(e + 8..e + 8 + size)
            } else {
                let off = self.u32(e + 8)? as usize;
                self.bytes.get(off..off.checked_add(size)?)
            };
            if let Some(data) = data {
                entries.push(Entry { tag, kind, count: n, data });
            }
        }
        let next = self.u32(at + 2 + count * 12).unwrap_or(0) as usize;
        Some((entries, next))
    }

    fn short(&self, e: &Entry) -> Option<u16> {
        match e.kind {
            3 => Tiff { bytes: e.data, ..*self }.u16(0),
            4 => Tiff { bytes: e.data, ..*self }.u32(0).and_then(|v| u16::try_from(v).ok()),
            _ => None,
        }
    }

    fn long(&self, e: &Entry) -> Option<u32> {
        match e.kind {
            3 => self.short(e).map(u32::from),
            4 => Tiff { bytes: e.data, ..*self }.u32(0),
            _ => None,
        }
    }

    fn rational(&self, e: &Entry, i: usize) -> Option<f64> {
        if e.kind != 5 || (e.count as usize) <= i {
            return None;
        }
        let t = Tiff { bytes: e.data, ..*self };
        let (n, d) = (t.u32(i * 8)?, t.u32(i * 8 + 4)?);
        (d != 0).then(|| f64::from(n) / f64::from(d))
    }
}

struct Entry<'a> {
    tag: u16,
    kind: u16,
    count: u32,
    data: &'a [u8],
}

/// APP1 `Exif`: IFD0, the EXIF and GPS IFDs it points to, and IFD1's preview.
fn exif(bytes: &[u8], info: &mut JpegInfo) {
    let little = match bytes.get(..4) {
        Some(b"II*\0") => true,
        Some(b"MM\0*") => false,
        _ => return,
    };
    let t = Tiff { bytes, little };
    let Some(first) = t.u32(4) else { return };
    let mut x = Exif::default();
    let mut ifd = first as usize;
    let mut seen = Vec::new();
    let mut resolution: (Option<f64>, Option<f64>, Option<u16>) = (None, None, None);
    for index in 0..2 {
        if ifd == 0 || seen.contains(&ifd) {
            break;
        }
        seen.push(ifd);
        let Some((entries, next)) = t.entries(ifd) else { break };
        let mut thumb = (None, None);
        let mut subs = Vec::new();
        for e in &entries {
            match (index, e.tag) {
                (0, 0x010E) => x.description = text(e.data),
                (0, 0x010F) => x.make = text(e.data),
                (0, 0x0110) => x.model = text(e.data),
                (0, 0x0112) => x.orientation = t.short(e),
                (0, 0x011A) => resolution.0 = t.rational(e, 0),
                (0, 0x011B) => resolution.1 = t.rational(e, 0),
                (0, 0x0128) => resolution.2 = t.short(e),
                (0, 0x0131) => x.software = text(e.data),
                (0, 0x0132) => x.taken = x.taken.take().or_else(|| text(e.data)),
                (0, 0x013B) => x.artist = text(e.data),
                (0, 0x8298) => x.copyright = text(e.data),
                (0, 0x8769) | (0, 0x8825) => subs.extend(t.long(e).map(|at| (e.tag, at as usize))),
                (1, 0x0201) => thumb.0 = t.long(e),
                (1, 0x0202) => thumb.1 = t.long(e),
                _ => {}
            }
        }
        if let (Some(_), Some(len)) = thumb {
            x.thumbnail_bytes = Some(len);
        }
        for (tag, at) in subs {
            if seen.len() >= MAX_IFDS || seen.contains(&at) {
                continue;
            }
            seen.push(at);
            let Some((entries, _)) = t.entries(at) else { continue };
            if tag == 0x8769 {
                exif_ifd(&entries, &mut x);
            } else {
                x.gps = gps(&t, &entries);
            }
        }
        ifd = next;
    }
    if info.dpi.is_none()
        && let (Some(rx), Some(ry)) = (resolution.0, resolution.1)
    {
        let per_cm = resolution.2 == Some(3);
        let dpi = |v: f64| (if per_cm { v * 2.54 } else { v }).round();
        if rx > 0.0 && ry > 0.0 && rx < 100_000.0 && ry < 100_000.0 && resolution.2 != Some(1) {
            info.dpi = Some((dpi(rx) as u32, dpi(ry) as u32));
        }
    }
    if !x.is_empty() {
        info.exif = x;
    }
}

fn exif_ifd(entries: &[Entry], x: &mut Exif) {
    for e in entries {
        match e.tag {
            // DateTimeOriginal is when it was taken; IFD0's DateTime only when it was last changed
            0x9003 => {
                if let Some(taken) = text(e.data) {
                    x.taken = Some(taken);
                }
            }
            0xA420 => x.unique_id = text(e.data),
            0xA431 => x.serial = text(e.data),
            0xA434 => x.lens = text(e.data),
            _ => {}
        }
    }
}

/// Degrees from GPS's degrees, minutes and seconds, signed by its reference letters.
fn gps(t: &Tiff, entries: &[Entry]) -> Option<(f64, f64)> {
    let find = |tag: u16| entries.iter().find(|e| e.tag == tag);
    let degrees = |tag: u16| {
        let e = find(tag)?;
        Some(t.rational(e, 0)? + t.rational(e, 1).unwrap_or(0.0) / 60.0 + t.rational(e, 2).unwrap_or(0.0) / 3600.0)
    };
    let sign =
        |tag: u16, negative: u8| if find(tag).and_then(|e| e.data.first()) == Some(&negative) { -1.0 } else { 1.0 };
    let (lat, lon) = (degrees(0x0002)? * sign(0x0001, b'S'), degrees(0x0004)? * sign(0x0003, b'W'));
    // all zeros is a camera that had no fix and wrote its fields anyway
    ((lat != 0.0 || lon != 0.0) && lat.abs() <= 90.0 && lon.abs() <= 180.0).then_some((lat, lon))
}

/// The few XMP properties that name the tool and follow a picture through its edits, as attributes
/// (`xmpMM:DocumentID="..."`) or elements (`<xmpMM:DocumentID>...</xmpMM:DocumentID>`).
fn xmp(xml: &[u8], out: &mut Xmp) {
    let xml = String::from_utf8_lossy(xml);
    out.creator_tool = xmp_value(&xml, "xmp:CreatorTool");
    out.document_id = xmp_value(&xml, "xmpMM:DocumentID");
    out.instance_id = xmp_value(&xml, "xmpMM:InstanceID");
    out.original_document_id = xmp_value(&xml, "xmpMM:OriginalDocumentID");
}

fn xmp_value(xml: &str, name: &str) -> Option<String> {
    let mut from = 0;
    while let Some(found) = xml[from..].find(name) {
        let at = from + found;
        from = at + name.len();
        // a longer name that starts the same, as `xmpMM:DocumentIDs`
        let rest = &xml[from..];
        let before = xml[..at].chars().next_back();
        if before.is_some_and(|c| c.is_alphanumeric() || c == ':') {
            continue;
        }
        let value = if let Some(rest) = rest.trim_start().strip_prefix('=') {
            let rest = rest.trim_start();
            let quote = rest.chars().next().filter(|c| *c == '"' || *c == '\'')?;
            rest[1..].split(quote).next()
        } else if let Some(rest) = rest.strip_prefix('>') {
            rest.split('<').next()
        } else {
            continue;
        };
        if let Some(value) = value.and_then(|v| text(unescape(v).as_bytes())) {
            return Some(value);
        }
    }
    None
}

fn unescape(v: &str) -> String {
    v.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
}

/// An ICC profile's `desc` tag: the v2 text description, or the first record of a v4 `mluc`.
fn icc_description(profile: &[u8]) -> Option<String> {
    let be32 =
        |at: usize| -> Option<usize> { Some(u32::from_be_bytes(profile.get(at..at + 4)?.try_into().ok()?) as usize) };
    let count = be32(128)?.min(256);
    for i in 0..count {
        let e = 132 + i * 12;
        if profile.get(e..e + 4)? != b"desc" {
            continue;
        }
        let (off, len) = (be32(e + 4)?, be32(e + 8)?);
        let tag = profile.get(off..off.checked_add(len)?)?;
        return match tag.get(..4)? {
            b"desc" => {
                let n = u32::from_be_bytes(tag.get(8..12)?.try_into().ok()?) as usize;
                text(tag.get(12..12 + n.min(tag.len().saturating_sub(12)))?)
            }
            b"mluc" => {
                let records = u32::from_be_bytes(tag.get(8..12)?.try_into().ok()?);
                if records == 0 {
                    return None;
                }
                let n = u32::from_be_bytes(tag.get(20..24)?.try_into().ok()?) as usize;
                let at = u32::from_be_bytes(tag.get(24..28)?.try_into().ok()?) as usize;
                let units: Vec<u16> = tag
                    .get(at..at.checked_add(n)?)?
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|c| u16::from_be_bytes([c[0], c[1]]))
                    .collect();
                text(String::from_utf16_lossy(&units).as_bytes())
            }
            _ => None,
        };
    }
    None
}

/// Bytes after the last EOI. A file with none at all (cut short) has nothing after its end.
fn after_end(bytes: &[u8]) -> usize {
    bytes.windows(2).rposition(|w| w == [0xFF, 0xD9]).map_or(0, |at| bytes.len() - at - 2)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segment(out: &mut Vec<u8>, marker: u8, body: &[u8]) {
        out.extend([0xFF, marker]);
        out.extend(((body.len() + 2) as u16).to_be_bytes());
        out.extend(body);
    }

    /// A TIFF block, big-endian: IFD0 with Make, Model, Software and a pointer to an EXIF IFD holding
    /// DateTimeOriginal and ImageUniqueID, and a GPS IFD.
    fn tiff() -> Vec<u8> {
        let mut t = b"MM\0*".to_vec();
        t.extend(8u32.to_be_bytes());
        // IFD0 at 8: 5 entries, then the next offset, then the values
        let values0 = 8 + 2 + 5 * 12 + 4;
        let (make, model, soft) = (b"Canon\0".as_slice(), b"Canon EOS R5\0".as_slice(), b"GIMP 2.10\0".as_slice());
        let exif_at = values0 + make.len() + model.len() + soft.len();
        let exif_len = 2 + 2 * 12 + 4;
        let gps_at = exif_at + exif_len + 20 + 33;
        let mut ifd = 5u16.to_be_bytes().to_vec();
        let mut entry = |tag: u16, kind: u16, n: u32, value: u32| {
            ifd.extend(tag.to_be_bytes());
            ifd.extend(kind.to_be_bytes());
            ifd.extend(n.to_be_bytes());
            ifd.extend(value.to_be_bytes());
        };
        entry(0x010F, 2, make.len() as u32, values0 as u32);
        entry(0x0110, 2, model.len() as u32, (values0 + make.len()) as u32);
        entry(0x0131, 2, soft.len() as u32, (values0 + make.len() + model.len()) as u32);
        entry(0x8769, 4, 1, exif_at as u32);
        entry(0x8825, 4, 1, gps_at as u32);
        ifd.extend(0u32.to_be_bytes());
        t.extend(ifd);
        t.extend(make);
        t.extend(model);
        t.extend(soft);
        assert_eq!(t.len(), exif_at);
        let (taken, uid) = (b"2024:05:01 12:00:00\0".as_slice(), b"0123456789abcdef0123456789abcdef\0".as_slice());
        t.extend(2u16.to_be_bytes());
        for (tag, data, at) in [(0x9003u16, taken, exif_at + exif_len), (0xA420, uid, exif_at + exif_len + 20)] {
            t.extend(tag.to_be_bytes());
            t.extend(2u16.to_be_bytes());
            t.extend((data.len() as u32).to_be_bytes());
            t.extend((at as u32).to_be_bytes());
        }
        t.extend(0u32.to_be_bytes());
        t.extend(taken);
        t.extend(uid);
        assert_eq!(t.len(), gps_at);
        // GPS: N 51 30 0, W 0 7 30
        let rationals_at = gps_at + 2 + 4 * 12 + 4;
        t.extend(4u16.to_be_bytes());
        for (tag, kind, n, value) in [
            (1u16, 2u16, 2u32, u32::from_be_bytes(*b"N\0\0\0")),
            (2, 5, 3, rationals_at as u32),
            (3, 2, 2, u32::from_be_bytes(*b"W\0\0\0")),
            (4, 5, 3, (rationals_at + 24) as u32),
        ] {
            t.extend(tag.to_be_bytes());
            t.extend(kind.to_be_bytes());
            t.extend(n.to_be_bytes());
            t.extend(value.to_be_bytes());
        }
        t.extend(0u32.to_be_bytes());
        for (n, d) in [(51, 1), (30, 1), (0, 1), (0, 1), (7, 1), (30, 1)] {
            t.extend((n as u32).to_be_bytes());
            t.extend((d as u32).to_be_bytes());
        }
        t
    }

    fn picture(quality: u8) -> Vec<u8> {
        let y = vec![128u8; 16 * 16];
        let c = vec![128u8; 8 * 8];
        crate::video::jpeg::encode(&y, &c, &c, 16, 16, quality)
    }

    /// The picture with segments put in after SOI.
    fn with(segments: &[(u8, Vec<u8>)], tail: &[u8]) -> Vec<u8> {
        let base = picture(85);
        let mut out = base[..2].to_vec();
        for (marker, body) in segments {
            segment(&mut out, *marker, body);
        }
        out.extend(&base[2..]);
        out.extend(tail);
        out
    }

    #[test]
    fn our_own_stills_read_back() {
        let info = read(&picture(85)).unwrap();
        assert_eq!((info.width, info.height, info.precision, info.components), (16, 16, 8, 3));
        assert_eq!(info.process, "baseline");
        assert_eq!(info.sampling.as_deref(), Some("4:2:0"));
        assert_eq!((info.quality, info.quality_exact), (Some(85), true));
        assert_eq!(info.after_end, 0);
        assert!(info.tables_hash.is_some());
        assert_ne!(read(&picture(50)).unwrap().tables_hash, info.tables_hash, "another setting, other tables");
        for q in [10, 50, 75, 92, 100] {
            assert_eq!(read(&picture(q)).unwrap().quality, Some(q), "quality {q}");
        }
    }

    #[test]
    fn not_a_jpeg_or_no_frame() {
        assert_eq!(read(b"\x89PNG\r\n\x1a\n"), None);
        assert_eq!(read(&[0xFF, 0xD8, 0xFF, 0xD9]), None);
        let whole = picture(85);
        // cut in the middle of its headers, before the frame
        let sof = whole.windows(2).position(|w| w == [0xFF, 0xC0]).unwrap();
        assert_eq!(read(&whole[..sof + 3]), None);
    }

    #[test]
    fn exif_fields_and_their_sub_ifds() {
        let mut app1 = b"Exif\0\0".to_vec();
        app1.extend(tiff());
        let info = read(&with(&[(0xE1, app1)], b"")).unwrap();
        let x = &info.exif;
        assert_eq!(x.make.as_deref(), Some("Canon"));
        assert_eq!(x.model.as_deref(), Some("Canon EOS R5"));
        assert_eq!(x.software.as_deref(), Some("GIMP 2.10"));
        assert_eq!(x.taken.as_deref(), Some("2024:05:01 12:00:00"));
        assert_eq!(x.unique_id.as_deref(), Some("0123456789abcdef0123456789abcdef"));
        let (lat, lon) = x.gps.unwrap();
        assert!((lat - 51.5).abs() < 1e-9 && (lon + 0.125).abs() < 1e-9, "{lat} {lon}");
    }

    #[test]
    fn a_broken_exif_block_leaves_the_rest() {
        let mut app1 = b"Exif\0\0".to_vec();
        let mut t = tiff();
        // every offset past the end
        t.truncate(30);
        app1.extend(t);
        let info = read(&with(&[(0xE1, app1), (0xFE, b"made by hand".to_vec())], b"")).unwrap();
        assert_eq!(info.width, 16);
        assert_eq!(info.comment.as_deref(), Some("made by hand"));
        // an IFD that points at itself
        let mut looped = b"Exif\0\0II*\0\x08\0\0\0".to_vec();
        looped.extend(1u16.to_le_bytes());
        looped.extend(0x8769u16.to_le_bytes());
        looped.extend(4u16.to_le_bytes());
        looped.extend(1u32.to_le_bytes());
        looped.extend(8u32.to_le_bytes());
        looped.extend(8u32.to_le_bytes());
        assert!(read(&with(&[(0xE1, looped)], b"")).is_some());
    }

    #[test]
    fn jfif_xmp_icc_adobe_and_what_follows_the_end() {
        let jfif = b"JFIF\0\x01\x02\x01\x00\x48\x00\x48\0\0".to_vec();
        let mut xmp = b"http://ns.adobe.com/xap/1.0/\0".to_vec();
        xmp.extend(
            br#"<x:xmpmeta><rdf:Description xmp:CreatorTool="Adobe Photoshop 25.0" xmpMM:DocumentID="xmp.did:abc"
            xmpMM:OriginalDocumentID="xmp.did:0001"><xmpMM:InstanceID>xmp.iid:def</xmpMM:InstanceID>
            </rdf:Description></x:xmpmeta>"#,
        );
        // an ICC v2 profile with only a `desc` tag, in two chunks
        let mut icc = vec![0u8; 128];
        icc.extend(1u32.to_be_bytes());
        icc.extend(b"desc");
        icc.extend(144u32.to_be_bytes());
        let name = b"sRGB IEC61966-2.1";
        icc.extend((12 + name.len() as u32 + 1).to_be_bytes());
        icc.extend(b"desc\0\0\0\0");
        icc.extend((name.len() as u32 + 1).to_be_bytes());
        icc.extend(name);
        icc.push(0);
        let (one, two) = icc.split_at(100);
        let chunk = |n: u8, data: &[u8]| {
            let mut c = b"ICC_PROFILE\0".to_vec();
            c.extend([n, 2]);
            c.extend(data);
            c
        };
        let adobe = b"Adobe\0\x64\0\0\0\0\x01".to_vec();
        let info = read(&with(
            &[(0xE0, jfif), (0xE1, xmp), (0xE2, chunk(2, two)), (0xE2, chunk(1, one)), (0xEE, adobe)],
            b"PK\x03\x04 stuck on",
        ))
        .unwrap();
        assert_eq!(info.jfif.as_deref(), Some("1.02"));
        assert_eq!(info.dpi, Some((72, 72)));
        assert_eq!(info.xmp.creator_tool.as_deref(), Some("Adobe Photoshop 25.0"));
        assert_eq!(info.xmp.document_id.as_deref(), Some("xmp.did:abc"));
        assert_eq!(info.xmp.original_document_id.as_deref(), Some("xmp.did:0001"));
        assert_eq!(info.xmp.instance_id.as_deref(), Some("xmp.iid:def"));
        assert_eq!(info.icc.as_deref(), Some("sRGB IEC61966-2.1"));
        assert_eq!(info.adobe_transform, Some(1));
        assert_eq!(info.after_end, 13);
    }

    #[test]
    fn sampling_names() {
        assert_eq!(sampling(&[(1, 1), (1, 1), (1, 1)]).as_deref(), Some("4:4:4"));
        assert_eq!(sampling(&[(2, 1), (1, 1), (1, 1)]).as_deref(), Some("4:2:2"));
        assert_eq!(sampling(&[(2, 2), (1, 1), (1, 1)]).as_deref(), Some("4:2:0"));
        assert_eq!(sampling(&[(1, 1)]), None, "greyscale");
        assert_eq!(sampling(&[(2, 2), (1, 1), (2, 1)]), None, "chromas sampled apart");
    }

    #[test]
    fn progressive_and_arithmetic_frames() {
        let mut info = JpegInfo::default();
        let body = [8, 0, 4, 0, 4, 1, 1, 0x11, 0];
        assert!(frame(0xC2, &body, &mut info));
        assert_eq!(info.process, "progressive");
        assert!(frame(0xC9, &body, &mut info));
        assert_eq!(info.process, "extended arithmetic");
        assert!(frame(0xC3, &body, &mut info));
        assert_eq!(info.process, "lossless");
    }
}
