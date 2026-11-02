//! Archives inside archives, unwrapped from the front: a RAR holding a 7z holding a zip holding a video is followed
//! down to the video, up to [`MAX_DEPTH`] layers.
//!
//! Each layer sees the one above it one of two ways. A stored file is a plain slice of its parent, soo a chain of
//! stored layers stays one offset into the outer file. A compressed file is decoded from its start, as far as the
//! bytes read so far go, into a new buffer. Encryption ends the chain, as does a method there's no decoder for yet.

use std::borrow::Cow;

use serde::{Deserialize, Serialize};

use super::archive::{self, ArchiveHead};
use super::lzma::{self, End, Props};

/// Layers unwrapped at most. One more found past these is recorded and left shut.
pub const MAX_DEPTH: usize = 10;
/// Bytes decoded at most, across every layer of one look.
pub const MAX_DECODED: usize = 16 * 1024 * 1024;
/// A layer decodes to at most this many times its input: a zip bomb gets this far and no further.
pub const MAX_RATIO: usize = 100;

/// One layer of the chain: what it is and how its file inside was reached.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Layer {
    /// `RAR4`, `RAR5`, `7z`, `ZIP`, `TAR`, `GZIP`, `XZ`, `LZMA`, `LZIP`, `ZSTD`, `BZIP2`, `COMPRESS`, `LZ4`, `LZOP`
    pub format: String,
    /// how the file inside is packed, in the format's own terms
    #[serde(skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    /// the file inside, when the format names it
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unpacked: Option<u64>,
    /// where the file inside starts, in this layer's bytes
    pub offset: u64,
    /// the file inside is these bytes as they are, not decoded
    pub stored: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub encrypted: bool,
    /// why nothing below this layer was read, when something stopped it
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop: Option<String>,
    /// the decoded bytes were cut short by a limit, not by the end of what was read
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub capped: bool,
    /// the file inside goes on in another volume
    #[serde(skip)]
    pub continues: bool,
}

/// The chain, and the bytes at the bottom of it.
pub struct Nest<'a> {
    pub layers: Vec<Layer>,
    pub inner: Cow<'a, [u8]>,
    /// Where `inner` starts in the outer bytes, when every layer above it is stored.
    pub outer_at: Option<usize>,
}

/// How a layer's file inside is reached.
enum View {
    /// `from..to` of the layer's bytes, `to` cut to what was read
    Stored { from: usize, to: usize },
    /// decoded from `from` on
    Decode { from: usize, codec: Codec },
    /// not reached: the reason why
    Stop(&'static str),
}

#[derive(Clone, Copy, Debug)]
enum Codec {
    Deflate,
    /// gzip's members, headers and all
    Gzip,
    Lzma {
        props: Props,
        size: Option<u64>,
    },
    Lzma2,
    Zstd,
}

/// A layer found: what to record, and how to go on.
struct Opened {
    layer: Layer,
    view: View,
}

/// Follows `data` down through every archive and compressed stream it starts with.
pub fn unwrap(data: &[u8]) -> Nest<'_> {
    let mut layers = Vec::new();
    let mut buf: Cow<'_, [u8]> = Cow::Borrowed(data);
    let (mut lo, mut hi) = (0usize, data.len());
    let mut outer_at = Some(0usize);
    let mut budget = MAX_DECODED;
    while let Some(Opened { mut layer, view }) = open(&buf[lo..hi]) {
        // a name comes from the poster's bytes: keep what a name needs
        layer.name = layer.name.map(|n| cap_name(&n));
        if layers.len() == MAX_DEPTH {
            layer.stop = Some(format!("deeper than {MAX_DEPTH} layers"));
            layers.push(layer);
            break;
        }
        match view {
            View::Stop(why) => {
                layer.stop = Some(why.to_string());
                layers.push(layer);
                break;
            }
            View::Stored { from, to } if from >= to || lo + to > hi => {
                layer.stop = Some("no file starts in what was read".to_string());
                layers.push(layer);
                break;
            }
            View::Stored { from, to } => {
                layer.offset = from as u64;
                layer.stored = true;
                outer_at = outer_at.map(|o| o + from);
                hi = lo + to;
                lo += from;
                layers.push(layer);
            }
            View::Decode { from, codec } => {
                layer.offset = from as u64;
                let Some(input) = buf.get(lo + from..hi).filter(|i| !i.is_empty()) else {
                    layer.stop = Some("no file starts in what was read".to_string());
                    layers.push(layer);
                    break;
                };
                let limit = budget.min(input.len().saturating_mul(MAX_RATIO));
                let (out, end) = decode(codec, input, limit);
                if out.is_empty() {
                    layer.stop = Some(if limit == 0 { "decode budget spent" } else { "nothing decoded" }.to_string());
                    layers.push(layer);
                    break;
                }
                layer.capped = end == End::Limit;
                budget -= out.len().min(budget);
                outer_at = None;
                (lo, hi) = (0, out.len());
                buf = Cow::Owned(out);
                layers.push(layer);
            }
        }
    }
    let inner = match buf {
        Cow::Borrowed(b) => Cow::Borrowed(&b[lo..hi]),
        Cow::Owned(mut v) => {
            v.truncate(hi);
            v.drain(..lo);
            Cow::Owned(v)
        }
    };
    Nest { layers, inner, outer_at }
}

/// At most [`crate::archive::MAX_NAME_BYTES`], cut on a character.
fn cap_name(name: &str) -> String {
    let mut end = name.len().min(crate::archive::MAX_NAME_BYTES);
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    name[..end].to_string()
}

fn decode(codec: Codec, input: &[u8], limit: usize) -> (Vec<u8>, End) {
    match codec {
        Codec::Deflate => inflate(input, limit),
        Codec::Gzip => gunzip(input, limit),
        Codec::Lzma { props, size } => lzma::lzma(input, props, size, limit),
        Codec::Lzma2 => lzma::lzma2(input, limit),
        Codec::Zstd => unzstd(input, limit),
    }
}

/// What `data` starts with, when it's an archive or a compressed stream.
fn open(data: &[u8]) -> Option<Opened> {
    let layer = |format: &str| Layer { format: format.into(), ..Layer::default() };
    if data.starts_with(b"Rar!\x1a\x07") || data.starts_with(b"PK\x03\x04") {
        return Some(from_entries(data));
    }
    if data.starts_with(b"7z\xbc\xaf\x27\x1c") {
        return Some(seven_z(data));
    }
    if data.starts_with(&[0x1f, 0x8b, 8]) {
        return Some(gzip(data));
    }
    if data.starts_with(b"\xfd7zXZ\0") {
        return Some(xz(data));
    }
    if data.starts_with(b"LZIP\x01") {
        let mut l = layer("LZIP");
        l.method = Some("lzma".into());
        let props = Props { lc: 3, lp: 0, pb: 2 };
        return Some(Opened { layer: l, view: View::Decode { from: 6, codec: Codec::Lzma { props, size: None } } });
    }
    if data.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]) || zstd_skippable(data) {
        let mut l = layer("ZSTD");
        l.method = Some("zstd".into());
        return Some(Opened { layer: l, view: View::Decode { from: 0, codec: Codec::Zstd } });
    }
    if let Some(opened) = lzma_alone(data) {
        return Some(opened);
    }
    if let Some(opened) = tar(data) {
        return Some(opened);
    }
    let not_yet = |format: &str| Some(Opened { layer: layer(format), view: View::Stop("no decoder for this yet") });
    if data.starts_with(b"BZh")
        && data.get(3).is_some_and(|b| (b'1'..=b'9').contains(b))
        && data.get(4..10) == Some(b"1AY&SY")
    {
        return not_yet("BZIP2");
    }
    if data.starts_with(&[0x1f, 0x9d]) {
        return not_yet("COMPRESS");
    }
    if data.starts_with(&[0x04, 0x22, 0x4d, 0x18]) {
        return not_yet("LZ4");
    }
    if data.starts_with(b"\x89LZO\0\r\n\x1a\n") {
        return not_yet("LZOP");
    }
    None
}

/// RAR and zip: the biggest file whose bytes start in what was read.
fn from_entries(data: &[u8]) -> Opened {
    let heads = archive::entries(data);
    let format = if data.starts_with(b"PK") {
        "ZIP"
    } else if data.get(6) == Some(&1) {
        "RAR5"
    } else {
        "RAR4"
    };
    let reachable = heads.iter().filter(|h| !h.dir && h.data_at.is_some_and(|at| at < data.len()));
    let Some(head) = reachable.max_by_key(|h| (h.unpacked.or(h.packed).unwrap_or(0), std::cmp::Reverse(h.data_at)))
    else {
        let mut layer = Layer { format: format.into(), ..Layer::default() };
        let encrypted = heads.iter().any(|h| h.headers_encrypted);
        layer.encrypted = encrypted;
        let why = if encrypted { "encrypted" } else { "no file starts in what was read" };
        return Opened { layer, view: View::Stop(why) };
    };
    let mut layer = layer_of(head);
    let at = head.data_at.unwrap_or(data.len());
    if head.encrypted {
        return Opened { layer, view: View::Stop("encrypted") };
    }
    let end = |packed: Option<u64>| {
        packed
            .and_then(|p| usize::try_from(p).ok())
            .and_then(|p| at.checked_add(p))
            .map_or(data.len(), |e| e.min(data.len()))
    };
    if head.stored_at.is_some() {
        layer.stored = true;
        return Opened { layer, view: View::Stored { from: at, to: end(head.packed) } };
    }
    let method = head.method.as_deref().unwrap_or_default();
    let view = if format == "ZIP" {
        let to = end(head.packed);
        match method {
            "deflate" => View::Decode { from: at, codec: Codec::Deflate },
            "zstd" => View::Decode { from: at, codec: Codec::Zstd },
            "lzma" => match zip_lzma(&data[at..to]) {
                Some((skip, props)) => {
                    View::Decode { from: at + skip, codec: Codec::Lzma { props, size: head.unpacked } }
                }
                None => View::Stop("damaged"),
            },
            // an xz stream: the next layer reads it
            "xz" => View::Stored { from: at, to },
            _ => View::Stop("no decoder for this yet"),
        }
    } else {
        View::Stop("no decoder for this yet")
    };
    if let View::Stored { .. } = view {
        layer.stored = true;
    }
    Opened { layer, view }
}

fn layer_of(head: &ArchiveHead) -> Layer {
    Layer {
        format: head.format.clone(),
        method: head.method.clone(),
        name: head.name.clone(),
        unpacked: head.unpacked,
        encrypted: head.encrypted,
        continues: head.continues,
        ..Layer::default()
    }
}

/// A zip's LZMA: a version, the properties' size, the 5 bytes of properties, then the stream.
fn zip_lzma(data: &[u8]) -> Option<(usize, Props)> {
    let size = usize::from(u16::from_le_bytes(data.get(2..4)?.try_into().ok()?));
    let props = Props::from_byte(*data.get(4)?)?;
    (size == 5).then_some((4 + size, props))
}

/// A 7z keeps its file list at the end, soo from the front its packed streams just start at byte 32 with no word
/// on how they're packed. Copy is told by what the bytes there are; LZMA2 by its first chunk's control byte; LZMA
/// by its range coder's leading zero, with the usual properties.
fn seven_z(data: &[u8]) -> Opened {
    const AT: usize = 32;
    let mut layer = Layer { format: "7z".into(), ..Layer::default() };
    let next_offset = data.get(12..20).and_then(|b| b.try_into().ok()).map(u64::from_le_bytes);
    // the packed streams run up to the file list
    let to = next_offset
        .and_then(|n| usize::try_from(n).ok())
        .and_then(|n| AT.checked_add(n))
        .map_or(data.len(), |e| e.min(data.len()));
    let Some(body) = data.get(AT..to).filter(|b| !b.is_empty()) else {
        return Opened { layer, view: View::Stop("no file starts in what was read") };
    };
    // by magic number only: opening it here would recurse past the depth limit
    if known_start(body) || crate::archive::identify(body) != crate::archive::Format::Unknown {
        layer.method = Some("copy".into());
        layer.stored = true;
        return Opened { layer, view: View::Stored { from: AT, to } };
    }
    match body[0] {
        1 | 0xe0..=0xff => {
            layer.method = Some("lzma2".into());
            Opened { layer, view: View::Decode { from: AT, codec: Codec::Lzma2 } }
        }
        0 => {
            layer.method = Some("lzma".into());
            let props = Props { lc: 3, lp: 0, pb: 2 };
            Opened { layer, view: View::Decode { from: AT, codec: Codec::Lzma { props, size: None } } }
        }
        _ => Opened { layer, view: View::Stop("packed some way the front doesn't show, or encrypted") },
    }
}

/// A zstd skippable frame, which a stream can open with before its real one.
fn zstd_skippable(data: &[u8]) -> bool {
    data.get(1..4) == Some(&[0x2a, 0x4d, 0x18][..]) && data[0] & 0xf0 == 0x50
}

/// Whether `data` starts the way something [`open`] reads does, without opening it.
fn known_start(data: &[u8]) -> bool {
    const MAGIC: &[&[u8]] = &[
        b"Rar!\x1a\x07",
        b"PK\x03\x04",
        b"7z\xbc\xaf\x27\x1c",
        &[0x1f, 0x8b, 8],
        b"\xfd7zXZ\0",
        b"LZIP\x01",
        &[0x28, 0xb5, 0x2f, 0xfd],
        b"BZh",
        &[0x1f, 0x9d],
        &[0x04, 0x22, 0x4d, 0x18],
        &[0x02, 0x21, 0x4c, 0x18],
        b"\x89LZO\0\r\n\x1a\n",
    ];
    MAGIC.iter().any(|m| data.starts_with(m))
        || zstd_skippable(data)
        || data.get(..512).is_some_and(tar_checksum_ok)
        || lzma_alone(data).is_some()
}

/// gzip: a header, with the original file's name when it kept one, then raw deflate; and maybe more members after,
/// each with its own header, whose output follows on.
fn gzip(data: &[u8]) -> Opened {
    let mut layer = Layer { format: "GZIP".into(), method: Some("deflate".into()), ..Layer::default() };
    let Some((p, name)) = gzip_header(data) else {
        return Opened { layer, view: View::Stop("no file starts in what was read") };
    };
    layer.name = name;
    if p >= data.len() {
        return Opened { layer, view: View::Stop("no file starts in what was read") };
    }
    Opened { layer, view: View::Decode { from: 0, codec: Codec::Gzip } }
}

/// Where a gzip member's deflate starts, and the name it kept.
fn gzip_header(data: &[u8]) -> Option<(usize, Option<String>)> {
    if !data.starts_with(&[0x1f, 0x8b, 8]) {
        return None;
    }
    let flags = *data.get(3)?;
    let mut p = 10usize;
    if flags & 4 != 0 {
        let extra = data.get(p..p + 2).map_or(0, |b| usize::from(u16::from_le_bytes([b[0], b[1]])));
        p += 2 + extra;
    }
    let cstr = |p: &mut usize| -> Option<String> {
        let rest = data.get(*p..)?;
        let n = rest.iter().position(|b| *b == 0)?;
        *p += n + 1;
        Some(String::from_utf8_lossy(&rest[..n.min(crate::archive::MAX_NAME_BYTES)]).into_owned())
    };
    let mut name = None;
    if flags & 8 != 0 {
        name = Some(cstr(&mut p)?);
    }
    if flags & 16 != 0 {
        cstr(&mut p)?;
    }
    if flags & 2 != 0 {
        p += 2;
    }
    Some((p, name))
}

/// Every gzip member in turn, from the front, to `limit` bytes.
fn gunzip(input: &[u8], limit: usize) -> (Vec<u8>, End) {
    let mut out = Vec::new();
    let mut at = 0usize;
    loop {
        let Some((p, _)) = gzip_header(&input[at..]) else { return (out, End::Cut) };
        let Some(member) = input.get(at + p..) else { return (out, End::Cut) };
        let (more, end, used) = inflate_counted(member, limit - out.len());
        out.extend_from_slice(&more);
        match end {
            End::Done => {}
            e => return (out, e),
        }
        // past the deflate, the member's CRC and size
        at += p + used + 8;
        if !input.get(at..).is_some_and(|rest| rest.starts_with(&[0x1f, 0x8b, 8])) {
            return (out, End::Done);
        }
    }
}

/// xz: its first block, when its one filter is LZMA2.
fn xz(data: &[u8]) -> Opened {
    let mut layer = Layer { format: "XZ".into(), ..Layer::default() };
    let stop = |layer: Layer, why| Opened { layer, view: View::Stop(why) };
    let Some(&size_byte) = data.get(12) else { return stop(layer, "no file starts in what was read") };
    if size_byte == 0 {
        return stop(layer, "empty");
    }
    let header_len = (usize::from(size_byte) + 1) * 4;
    let Some(header) = data.get(12..12 + header_len) else { return stop(layer, "no file starts in what was read") };
    let flags = header[1];
    let mut p = 2usize;
    let vli = |p: &mut usize| -> Option<u64> {
        let mut value = 0u64;
        for shift in (0..63).step_by(7) {
            let b = *header.get(*p)?;
            *p += 1;
            value |= u64::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                return Some(value);
            }
        }
        None
    };
    if flags & 0x40 != 0 && vli(&mut p).is_none() {
        return stop(layer, "damaged");
    }
    if flags & 0x80 != 0 {
        layer.unpacked = vli(&mut p);
    }
    let mut ids = Vec::new();
    for _ in 0..=(flags & 3) {
        let (Some(id), Some(len)) = (vli(&mut p), vli(&mut p)) else { return stop(layer, "damaged") };
        p += usize::try_from(len).unwrap_or(usize::MAX / 2);
        ids.push(id);
    }
    let name = |id: u64| match id {
        0x21 => "lzma2".to_string(),
        0x03 => "delta".to_string(),
        0x04..=0x0b => "bcj".to_string(),
        _ => format!("filter {id:#x}"),
    };
    layer.method = Some(ids.iter().map(|id| name(*id)).collect::<Vec<_>>().join("+"));
    if ids != [0x21] {
        return stop(layer, "no decoder for this filter yet");
    }
    Opened { layer, view: View::Decode { from: 12 + header_len, codec: Codec::Lzma2 } }
}

/// `.lzma`: a properties byte, the dictionary size, the unpacked size, then the stream. No magic number, soo only
/// the common properties byte, and a stream that starts as LZMA's always does.
fn lzma_alone(data: &[u8]) -> Option<Opened> {
    if data.first() != Some(&0x5d) || data.get(13) != Some(&0) {
        return None;
    }
    let dict = u32::from_le_bytes(data.get(1..5)?.try_into().ok()?);
    let size = u64::from_le_bytes(data.get(5..13)?.try_into().ok()?);
    // xz writes powers of two, or 2^n + 2^(n-1); and a known size is a sane one
    let dict_ok = dict >= 4096 && (dict.is_power_of_two() || (dict / 3).is_power_of_two() && dict % 3 == 0);
    if !dict_ok || (size != u64::MAX && size >= 1 << 48) {
        return None;
    }
    let size = (size != u64::MAX).then_some(size);
    let layer = Layer { format: "LZMA".into(), method: Some("lzma".into()), unpacked: size, ..Layer::default() };
    let props = Props::from_byte(0x5d)?;
    Some(Opened { layer, view: View::Decode { from: 13, codec: Codec::Lzma { props, size } } })
}

/// tar: 512 byte headers, each file's bytes after its own, stored. The biggest file whose bytes start in what was
/// read, with GNU and pax long names.
fn tar(data: &[u8]) -> Option<Opened> {
    let first = data.get(..512)?;
    if !tar_checksum_ok(first) {
        return None;
    }
    let mut at = 0usize;
    let mut long_name: Option<String> = None;
    let mut long_size: Option<u64> = None;
    let mut best: Option<(u64, usize, String)> = None;
    for _ in 0..256 {
        let Some(h) = data.get(at..at + 512) else { break };
        if h.iter().all(|b| *b == 0) || !tar_checksum_ok(h) {
            break;
        }
        let mut size = tar_size(&h[124..136])?;
        let kind = h[156];
        let body = at + 512;
        let body_bytes = |n: u64| usize::try_from(n).ok().and_then(|n| data.get(body..body.checked_add(n)?));
        match kind {
            b'L' => {
                long_name = body_bytes(size.min(crate::archive::MAX_NAME_BYTES as u64 + 1)).map(cstr);
            }
            b'x' => {
                if let Some(records) = body_bytes(size) {
                    let (path, pax_size) = pax(records);
                    long_name = path.or(long_name);
                    long_size = pax_size.or(long_size);
                }
            }
            b'0' | 0 | b'7' => {
                if let Some(s) = long_size.take() {
                    size = s;
                }
                let name = long_name.take().unwrap_or_else(|| {
                    let prefix = if &h[257..262] == b"ustar" { cstr(&h[345..500]) } else { String::new() };
                    let name = cstr(&h[..100]);
                    if prefix.is_empty() { name } else { format!("{prefix}/{name}") }
                });
                if body < data.len() && best.as_ref().is_none_or(|(s, _, _)| size > *s) {
                    best = Some((size, body, name));
                }
            }
            _ => {
                long_name = None;
                long_size = None;
            }
        }
        let Some(next) = usize::try_from(size.div_ceil(512) * 512).ok().and_then(|s| body.checked_add(s)) else {
            break;
        };
        at = next;
    }
    let mut layer = Layer { format: "TAR".into(), method: Some("store".into()), ..Layer::default() };
    let Some((size, from, name)) = best else {
        return Some(Opened { layer, view: View::Stop("no file starts in what was read") });
    };
    layer.name = Some(name);
    layer.unpacked = Some(size);
    layer.stored = true;
    let to = usize::try_from(size).ok().and_then(|s| from.checked_add(s)).map_or(data.len(), |e| e.min(data.len()));
    Some(Opened { layer, view: View::Stored { from, to } })
}

fn cstr(b: &[u8]) -> String {
    String::from_utf8_lossy(b.split(|c| *c == 0).next().unwrap_or(b)).into_owned()
}

fn tar_checksum_ok(h: &[u8]) -> bool {
    let Some(stored) = std::str::from_utf8(&h[148..156])
        .ok()
        .map(|s| s.trim_matches(|c: char| c == '\0' || c == ' '))
        .and_then(|s| u32::from_str_radix(s, 8).ok())
    else {
        return false;
    };
    let sum: u32 = h.iter().enumerate().map(|(i, b)| if (148..156).contains(&i) { 32 } else { u32::from(*b) }).sum();
    sum == stored
}

/// Octal, or for big files GNU's base 256 with the top bit set.
fn tar_size(field: &[u8]) -> Option<u64> {
    if field[0] & 0x80 != 0 {
        return field[1..].iter().try_fold(0u64, |acc, b| acc.checked_mul(256).map(|a| a | u64::from(*b)));
    }
    let s = std::str::from_utf8(field).ok()?.trim_matches(|c: char| c == '\0' || c == ' ');
    if s.is_empty() {
        return Some(0);
    }
    u64::from_str_radix(s, 8).ok()
}

/// `path=` and `size=` from a pax header's `<len> key=value\n` records, the path cut to what a name needs before
/// any of it is copied.
fn pax(body: &[u8]) -> (Option<String>, Option<u64>) {
    let (mut path, mut size) = (None, None);
    let mut p = 0usize;
    while p < body.len() {
        let rest = &body[p..];
        let Some(space) = rest.iter().take(20).position(|b| *b == b' ') else { break };
        let Some(len) = std::str::from_utf8(&rest[..space]).ok().and_then(|n| n.parse::<usize>().ok()) else {
            break;
        };
        let Some(record) = rest.get(space + 1..len).filter(|_| len > space + 1) else { break };
        let record = record.strip_suffix(b"\n").unwrap_or(record);
        if let Some(v) = record.strip_prefix(b"path=") {
            path = Some(String::from_utf8_lossy(&v[..v.len().min(crate::archive::MAX_NAME_BYTES)]).into_owned());
        } else if let Some(v) = record.strip_prefix(b"size=") {
            size = std::str::from_utf8(v).ok().and_then(|v| v.parse().ok());
        }
        p += len;
    }
    (path, size)
}

/// Raw deflate, from the front, to `limit` bytes.
fn inflate(input: &[u8], limit: usize) -> (Vec<u8>, End) {
    let (out, end, _) = inflate_counted(input, limit);
    (out, end)
}

/// Raw deflate, and how much of the input it took.
fn inflate_counted(input: &[u8], limit: usize) -> (Vec<u8>, End, usize) {
    let mut d = flate2::Decompress::new(false);
    let mut out = Vec::new();
    let mut window = Vec::with_capacity(1 << 16);
    loop {
        if out.len() >= limit {
            out.truncate(limit);
            return (out, End::Limit, d.total_in() as usize);
        }
        window.clear();
        let before = d.total_in();
        let rest = input.get(usize::try_from(before).unwrap_or(usize::MAX)..).unwrap_or_default();
        let status = d.decompress_vec(rest, &mut window, flate2::FlushDecompress::None);
        out.extend_from_slice(&window);
        match status {
            Ok(flate2::Status::StreamEnd) if out.len() <= limit => return (out, End::Done, d.total_in() as usize),
            Ok(flate2::Status::StreamEnd) => {}
            Ok(_) if d.total_in() == before && window.is_empty() => return (out, End::Cut, d.total_in() as usize),
            Ok(_) => {}
            Err(_) => return (out, End::Cut, d.total_in() as usize),
        }
    }
}

/// zstd, from the front, to `limit` bytes. The zstd library is already Atlas's, for the sealed log.
fn unzstd(input: &[u8], limit: usize) -> (Vec<u8>, End) {
    use zstd::stream::raw::{Decoder, InBuffer, Operation, OutBuffer};
    let mut out = Vec::new();
    let Ok(mut d) = Decoder::new() else { return (out, End::Cut) };
    let mut src = InBuffer::around(input);
    let mut chunk = vec![0u8; 1 << 16];
    loop {
        if out.len() >= limit {
            out.truncate(limit);
            return (out, End::Limit);
        }
        let before = src.pos();
        let mut dst = OutBuffer::around(&mut chunk[..]);
        let hint = match d.run(&mut src, &mut dst) {
            Ok(hint) => hint,
            Err(_) => return (out, End::Cut),
        };
        let wrote = dst.pos();
        out.extend_from_slice(&chunk[..wrote]);
        if out.len() >= limit {
            out.truncate(limit);
            return (out, End::Limit);
        }
        // a frame ended, and nothing's left to start another (a skippable frame is followed by the real one)
        if hint == 0 && wrote < chunk.len() && src.pos() == input.len() {
            return (out, End::Done);
        }
        if wrote == 0 && src.pos() == before {
            return (out, End::Cut);
        }
    }
}

#[cfg(test)]
mod tests;
