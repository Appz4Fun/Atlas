//! Archives inside archives, unwrapped from the front: a RAR holding a 7z holding a zip holding a video is followed
//! down to the video, up to [`MAX_DEPTH`] layers.
//!
//! Each layer sees the one above it one of two ways. A stored file is a plain slice of its parent, soo a chain of
//! stored layers stays one offset into the outer file. A compressed file is decoded from its start, as far as the
//! bytes read so far go, into a new buffer. Encryption ends the chain, as does a method there's no decoder for yet.

use std::borrow::Cow;

use serde::{Deserialize, Serialize};

use super::archive::{self, ArchiveHead};
use super::codec::{self, bcj::Filter};
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
    /// plaintext is a WinZip AES preview whose authentication trailer was not available
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unauthenticated: bool,
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
    /// bytes decoded across every layer, kept or not
    pub decoded: usize,
}

/// How a layer's file inside is reached.
enum View {
    /// `from..to` of the layer's bytes, `to` cut to what was read
    Stored { from: usize, to: usize },
    /// decoded from `from` on
    Decode { from: usize, codec: Codec },
    /// decrypted (and, when compressed, decoded) from `from` on
    Decrypted { from: usize, out: Vec<u8>, capped: bool, unauthenticated: bool },
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
    /// xz's LZMA2, then the filters in front of it, undone last first
    Xz {
        filters: [Option<Filter>; 3],
    },
    Zstd,
    Bzip2,
    Deflate64,
    Lzw,
    Lz4,
    Rar5 {
        extra_dist: bool,
        size: Option<u64>,
        packed: Option<u64>,
    },
    /// RAR4's compressions by the version needed to unpack: 15, 20 and 26, or 29
    Rar4 {
        version: u8,
        size: Option<u64>,
        packed: Option<u64>,
        /// the stream goes on in the next volume
        continues: bool,
    },
    /// PPMd variant I, as zip has it (method 98)
    PpmdZip {
        size: Option<u64>,
        packed: Option<u64>,
    },
}

/// A layer found: what to record, and how to go on.
struct Opened {
    layer: Layer,
    view: View,
}

/// Follows `data` down through every archive and compressed stream it starts with.
pub fn unwrap(data: &[u8]) -> Nest<'_> {
    unwrap_for(data, &|_| false, MAX_DECODED)
}

/// Like [`unwrap`], decrypting RAR4/RAR5/ZIP headers and entries with `password` when present.
pub fn unwrap_with_password<'a>(data: &'a [u8], password: Option<&str>) -> Nest<'a> {
    unwrap_for_with_password(data, &|_| false, MAX_DECODED, password)
}

/// Like [`unwrap_with_password`], preferring an `.nzb` entry when an archive holds multiple files.
pub fn unwrap_nzb_with_password<'a>(data: &'a [u8], password: Option<&str>) -> Nest<'a> {
    unwrap_for_with_password(
        data,
        &|name| crate::archive::select::classify(name) == crate::archive::select::Kind::Nzb,
        MAX_DECODED,
        password,
    )
}

/// `unwrap`, taking in a RAR, zip or tar the biggest file whose name `want` says yes to, when there is one, over the
/// biggest, and decoding up to `max_decoded` bytes in all rather than `MAX_DECODED`.
pub fn unwrap_for<'a>(data: &'a [u8], want: &dyn Fn(&str) -> bool, max_decoded: usize) -> Nest<'a> {
    unwrap_for_with_password(data, want, max_decoded, None)
}

pub fn unwrap_for_with_password<'a>(
    data: &'a [u8],
    want: &dyn Fn(&str) -> bool,
    max_decoded: usize,
    password: Option<&str>,
) -> Nest<'a> {
    let mut layers = Vec::new();
    let mut buf: Cow<'_, [u8]> = Cow::Borrowed(data);
    let (mut lo, mut hi) = (0usize, data.len());
    let mut outer_at = Some(0usize);
    let mut budget = max_decoded;
    let mut rar4_cache = crate::archive::rar::Rar4KeyCache::default();
    while let Some(Opened { mut layer, view }) = open(
        &buf[lo..hi],
        want,
        if layers.len() >= MAX_DEPTH { None } else { password },
        budget,
        &mut rar4_cache,
    ) {
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
            View::Decrypted { from, out, capped, unauthenticated } => {
                layer.offset = from as u64;
                if out.is_empty() {
                    layer.stop = Some("nothing decoded".to_string());
                    layers.push(layer);
                    break;
                }
                layer.capped = capped;
                layer.unauthenticated = unauthenticated;
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
    Nest { layers, inner, outer_at, decoded: max_decoded - budget }
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
        Codec::Xz { filters } => {
            let (mut out, end) = lzma::lzma2(input, limit);
            let mut last = out.len();
            for f in filters.iter().rev().flatten() {
                let done = f.undo(&mut out[..last]);
                // a branch cut by where decoding stopped can't be undone yet
                if end != End::Done {
                    last = done;
                }
            }
            out.truncate(last);
            (out, end)
        }
        Codec::Zstd => unzstd(input, limit),
        Codec::Bzip2 => codec::bzip2::bzip2(input, limit),
        Codec::Deflate64 => codec::inflate::deflate64(input, limit),
        Codec::Lzw => codec::lzw::uncompress(input, limit),
        Codec::Lz4 => codec::lz4::lz4(input, limit),
        Codec::Rar5 { extra_dist, size, packed } => {
            let to = packed.and_then(|p| usize::try_from(p).ok()).map_or(input.len(), |p| p.min(input.len()));
            codec::rar5::rar5(&input[..to], extra_dist, size, limit)
        }
        Codec::Rar4 { version, size, packed, continues } => {
            let to = packed.and_then(|p| usize::try_from(p).ok()).map_or(input.len(), |p| p.min(input.len()));
            // a volume's chunk is only the front of the stream
            let complete = !continues && packed.is_some_and(|p| p <= input.len() as u64);
            let decode = match version {
                15 => codec::rar15::rar15,
                20 | 26 => codec::rar20::rar20,
                _ => codec::rar3::rar29,
            };
            decode(&input[..to], size, limit, complete)
        }
        Codec::PpmdZip { size, packed } => {
            // the range coder reads ahead, so it must not read on into the next entry
            let to = packed.and_then(|p| usize::try_from(p).ok()).map_or(input.len(), |p| p.min(input.len()));
            codec::ppmd_i::zip(&input[..to], size, limit)
        }
    }
}

/// What `data` starts with, when it's an archive or a compressed stream.
fn open(
    data: &[u8],
    want: &dyn Fn(&str) -> bool,
    password: Option<&str>,
    budget: usize,
    rar4_cache: &mut crate::archive::rar::Rar4KeyCache,
) -> Option<Opened> {
    let layer = |format: &str| Layer { format: format.into(), ..Layer::default() };
    if data.starts_with(b"Rar!\x1a\x07") || data.starts_with(b"PK\x03\x04") {
        return Some(from_entries(data, want, password, budget, rar4_cache));
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
    // skippable frames are both zstd's and LZ4's: what follows them says which
    if data.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]) || (zstd_skippable(data) && !lz4_after_skippable(data)) {
        let mut l = layer("ZSTD");
        l.method = Some("zstd".into());
        return Some(Opened { layer: l, view: View::Decode { from: 0, codec: Codec::Zstd } });
    }
    if let Some(opened) = lzma_alone(data) {
        return Some(opened);
    }
    if let Some(opened) = tar(data, want) {
        return Some(opened);
    }
    let decode = |format: &str, method: &str, codec| {
        let layer = Layer { format: format.into(), method: Some(method.into()), ..Layer::default() };
        Some(Opened { layer, view: View::Decode { from: 0, codec } })
    };
    if data.starts_with(b"BZh") && data.get(3).is_some_and(|b| (b'1'..=b'9').contains(b)) {
        return decode("BZIP2", "bzip2", Codec::Bzip2);
    }
    if data.starts_with(&[0x1f, 0x9d]) {
        return decode("COMPRESS", "lzw", Codec::Lzw);
    }
    if data.starts_with(&[0x04, 0x22, 0x4d, 0x18])
        || data.starts_with(&[0x02, 0x21, 0x4c, 0x18])
        || lz4_after_skippable(data)
    {
        return decode("LZ4", "lz4", Codec::Lz4);
    }
    if data.starts_with(b"\x89LZO\0\r\n\x1a\n") {
        return Some(Opened { layer: layer("LZOP"), view: View::Stop("no decoder for this yet") });
    }
    None
}

/// RAR and zip: the biggest file whose bytes start in what was read, of those `want` names if any.
fn from_entries(
    data: &[u8],
    want: &dyn Fn(&str) -> bool,
    password: Option<&str>,
    budget: usize,
    rar4_cache: &mut crate::archive::rar::Rar4KeyCache,
) -> Opened {
    let heads = archive::entries_with_password_and_cache(data, password, rar4_cache);
    let format = if data.starts_with(b"PK") {
        "ZIP"
    } else if data.get(6) == Some(&1) {
        "RAR5"
    } else {
        "RAR4"
    };
    let reachable = heads.iter().filter(|h| !h.dir && h.data_at.is_some_and(|at| at < data.len()));
    let wanted = |h: &&archive::ArchiveHead| h.name.as_deref().is_some_and(want);
    let Some(head) =
        reachable.max_by_key(|h| (wanted(h), h.unpacked.or(h.packed).unwrap_or(0), std::cmp::Reverse(h.data_at)))
    else {
        let mut layer = Layer { format: format.into(), ..Layer::default() };
        let encrypted = heads.iter().any(|h| h.headers_encrypted);
        layer.encrypted = encrypted;
        let why = if encrypted { "encrypted" } else { "no file starts in what was read" };
        return Opened { layer, view: View::Stop(why) };
    };
    let mut layer = layer_of(head);
    let at = head.data_at.unwrap_or(data.len());
    let end = |packed: Option<u64>| {
        packed
            .and_then(|p| usize::try_from(p).ok())
            .and_then(|p| at.checked_add(p))
            .map_or(data.len(), |e| e.min(data.len()))
    };
    if head.encrypted {
        let Some(pw) = password.filter(|pw| !pw.is_empty()) else {
            return Opened { layer, view: View::Stop("encrypted") };
        };
        let to = end(head.packed);
        return match decrypt_entry(data, at, to, head, format, pw, budget, rar4_cache) {
            Ok((out, capped, unauthenticated)) => {
                Opened { layer, view: View::Decrypted { from: at, out, capped, unauthenticated } }
            }
            Err(why) => Opened { layer, view: View::Stop(why) },
        };
    }
    if head.stored_at.is_some() {
        layer.stored = true;
        return Opened { layer, view: View::Stored { from: at, to: end(head.packed) } };
    }
    let method = head.method.as_deref().unwrap_or_default();
    let view = if format == "ZIP" {
        let to = end(head.packed);
        match method {
            "deflate" => View::Decode { from: at, codec: Codec::Deflate },
            "deflate64" => View::Decode { from: at, codec: Codec::Deflate64 },
            "bzip2" => View::Decode { from: at, codec: Codec::Bzip2 },
            "zstd" => View::Decode { from: at, codec: Codec::Zstd },
            "ppmd" => View::Decode { from: at, codec: Codec::PpmdZip { size: head.unpacked, packed: head.packed } },
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
    } else if head.split_before {
        View::Stop("starts in the volume before")
    } else if head.solid {
        View::Stop("solid: needs the files before it")
    } else if format == "RAR5" && head.rar_version <= 1 {
        let codec = Codec::Rar5 { extra_dist: head.rar_version == 1, size: head.unpacked, packed: head.packed };
        View::Decode { from: at, codec }
    } else if format == "RAR5" {
        View::Stop("a RAR algorithm newer than this knows")
    } else if format == "RAR4" && matches!(head.rar_version, 15 | 20 | 26 | 29 | 36) {
        let codec = Codec::Rar4 {
            version: head.rar_version,
            size: head.unpacked,
            packed: head.packed,
            continues: head.continues,
        };
        View::Decode { from: at, codec }
    } else {
        View::Stop("no decoder for this yet")
    };
    if let View::Stored { .. } = view {
        layer.stored = true;
    }
    Opened { layer, view }
}

#[allow(clippy::too_many_arguments)]
fn decrypt_entry(
    data: &[u8],
    at: usize,
    to: usize,
    head: &ArchiveHead,
    format: &str,
    pw: &str,
    budget: usize,
    rar4_cache: &mut crate::archive::rar::Rar4KeyCache,
) -> Result<(Vec<u8>, bool, bool), &'static str> {
    let packed_slice = data.get(at..to).filter(|s| !s.is_empty()).ok_or("no file starts in what was read")?;
    let complete_packed = !head.continues && head.packed.is_some_and(|p| p <= packed_slice.len() as u64);
    let verify_crc = format == "RAR4" || (format == "ZIP" && head.zip_aes.is_none());
    let check_crc = |out: &[u8], capped: bool| -> Result<(), &'static str> {
        if verify_crc
            && complete_packed
            && !capped
            && head.unpacked.is_some_and(|u| u == out.len() as u64)
            && head.crc32.is_some_and(|c| c != crate::archive::crc32(out))
        {
            return Err("encrypted");
        }
        Ok(())
    };
    let finish_stored = |mut dec: Vec<u8>| -> Result<(Vec<u8>, bool, bool), &'static str> {
        let capped = if let Some(unpacked) = head.unpacked.and_then(|u| usize::try_from(u).ok()) {
            let cap = unpacked.min(budget);
            dec.truncate(cap);
            unpacked > budget
        } else {
            let capped = dec.len() > budget;
            dec.truncate(budget);
            capped
        };
        check_crc(&dec, capped)?;
        if dec.is_empty() {
            return Err("nothing decoded");
        }
        Ok((dec, capped, false))
    };
    let method = head.method.as_deref().unwrap_or_default();
    match format {
        "RAR5" => {
            let (kdf, salt, iv, check) = head.rar5_crypt.ok_or("encrypted")?;
            let key = crate::archive::rar::derive_rar5_key(pw, kdf, &salt, check).ok_or("encrypted")?;
            let dec = crate::archive::aes::decrypt_cbc(&key, &iv, packed_slice).ok_or("encrypted")?;
            if method == "m0" {
                finish_stored(dec)
            } else if head.split_before {
                Err("starts in the volume before")
            } else if head.solid {
                Err("solid: needs the files before it")
            } else if head.rar_version <= 1 {
                let limit = budget.min(dec.len().saturating_mul(MAX_RATIO));
                if limit == 0 {
                    return Err("decode budget spent");
                }
                let codec = Codec::Rar5 {
                    extra_dist: head.rar_version == 1,
                    size: head.unpacked,
                    packed: Some(dec.len() as u64),
                };
                let (out, end) = decode(codec, &dec, limit);
                if out.is_empty() {
                    return Err("nothing decoded");
                }
                let capped = end == End::Limit;
                check_crc(&out, capped)?;
                Ok((out, capped, false))
            } else {
                Err("a RAR algorithm newer than this knows")
            }
        }
        "RAR4" => {
            let salt = head.rar4_salt.ok_or("encrypted")?;
            let (key, iv) = rar4_cache.derive(pw, &salt).ok_or("encrypted")?;
            let dec = crate::archive::aes::decrypt_cbc(&key, &iv, packed_slice).ok_or("encrypted")?;
            if method == "m0" {
                finish_stored(dec)
            } else if head.split_before {
                Err("starts in the volume before")
            } else if head.solid {
                Err("solid: needs the files before it")
            } else if matches!(head.rar_version, 15 | 20 | 26 | 29 | 36) {
                let limit = budget.min(dec.len().saturating_mul(MAX_RATIO));
                if limit == 0 {
                    return Err("decode budget spent");
                }
                let codec = Codec::Rar4 {
                    version: head.rar_version,
                    size: head.unpacked,
                    packed: Some(dec.len() as u64),
                    continues: head.continues,
                };
                let (out, end) = decode(codec, &dec, limit);
                if out.is_empty() {
                    return Err("nothing decoded");
                }
                let capped = end == End::Limit;
                check_crc(&out, capped)?;
                Ok((out, capped, false))
            } else {
                Err("no decoder for this yet")
            }
        }
        "ZIP" => {
            let (dec, actual_method, unauthenticated) = if let Some((strength, actual)) = head.zip_aes {
                let full_payload = head.packed == Some(packed_slice.len() as u64);
                let d = if full_payload {
                    crate::archive::zip::decrypt_winzip_aes(pw, strength, packed_slice, head.packed)
                } else {
                    crate::archive::zip::decrypt_winzip_aes_preview(pw, strength, packed_slice, head.packed)
                }
                .ok_or("encrypted")?;
                (d, archive::zip_method(actual), !full_payload)
            } else if let Some(check) = head.zip_crypto_check {
                let d = crate::archive::zip::decrypt_zipcrypto(pw, check, packed_slice).ok_or("encrypted")?;
                (d, method, false)
            } else {
                return Err("encrypted");
            };
            match actual_method {
                "store" => finish_stored(dec).map(|(out, capped, _)| (out, capped, unauthenticated)),
                "xz" => {
                    if dec.is_empty() {
                        Err("nothing decoded")
                    } else {
                        Ok((dec, false, unauthenticated))
                    }
                }
                "deflate" | "deflate64" | "bzip2" | "zstd" | "ppmd" | "lzma" => {
                    let limit = budget.min(dec.len().saturating_mul(MAX_RATIO));
                    if limit == 0 {
                        return Err("decode budget spent");
                    }
                    let (codec, slice) = match actual_method {
                        "deflate" => (Codec::Deflate, dec.as_slice()),
                        "deflate64" => (Codec::Deflate64, dec.as_slice()),
                        "bzip2" => (Codec::Bzip2, dec.as_slice()),
                        "zstd" => (Codec::Zstd, dec.as_slice()),
                        "ppmd" => (
                            Codec::PpmdZip { size: head.unpacked, packed: Some(dec.len() as u64) },
                            dec.as_slice(),
                        ),
                        "lzma" => {
                            let (skip, props) = zip_lzma(&dec).ok_or("damaged")?;
                            (Codec::Lzma { props, size: head.unpacked }, dec.get(skip..).ok_or("damaged")?)
                        }
                        _ => return Err("no decoder for this yet"),
                    };
                    let (out, end) = decode(codec, slice, limit);
                    if out.is_empty() {
                        return Err("nothing decoded");
                    }
                    let capped = end == End::Limit;
                    if head.zip_aes.is_none() {
                        check_crc(&out, capped)?;
                    }
                    Ok((out, capped, unauthenticated))
                }
                _ => Err("no decoder for this yet"),
            }
        }
        _ => Err("encrypted"),
    }
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

/// Whether the first frame after a run of skippable frames is an LZ4 one.
fn lz4_after_skippable(data: &[u8]) -> bool {
    let mut p = 0usize;
    for _ in 0..64 {
        let Some(rest) = data.get(p..) else { return false };
        if !zstd_skippable(rest) {
            return rest.starts_with(&[0x04, 0x22, 0x4d, 0x18]) || rest.starts_with(&[0x02, 0x21, 0x4c, 0x18]);
        }
        let Some(size) = rest.get(4..8).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize) else {
            return false;
        };
        p = p.saturating_add(8).saturating_add(size);
    }
    false
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

/// xz: its first block, LZMA2 with up to three filters in front.
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
    let mut props = Vec::new();
    for _ in 0..=(flags & 3) {
        let (Some(id), Some(len)) = (vli(&mut p), vli(&mut p)) else { return stop(layer, "damaged") };
        let to = usize::try_from(len).ok().and_then(|l| p.checked_add(l));
        let Some(prop) = to.and_then(|to| header.get(p..to)) else { return stop(layer, "damaged") };
        p += prop.len();
        ids.push(id);
        props.push(prop);
    }
    let name = |id: u64| match id {
        0x21 => "lzma2".to_string(),
        0x03 => "delta".to_string(),
        0x04 => "x86".to_string(),
        0x05 => "powerpc".to_string(),
        0x06 => "ia64".to_string(),
        0x07 => "arm".to_string(),
        0x08 => "armthumb".to_string(),
        0x09 => "sparc".to_string(),
        0x0a => "arm64".to_string(),
        0x0b => "riscv".to_string(),
        _ => format!("filter {id:#x}"),
    };
    layer.method = Some(ids.iter().map(|id| name(*id)).collect::<Vec<_>>().join("+"));
    // up to three filters, then LZMA2 last
    if ids.last() != Some(&0x21) {
        return stop(layer, "no decoder for this filter yet");
    }
    let mut filters = [None; 3];
    for (i, (id, prop)) in ids.iter().zip(&props).take(ids.len() - 1).enumerate() {
        let Some(f) = Filter::from_xz(*id, prop) else { return stop(layer, "no decoder for this filter yet") };
        filters[i] = Some(f);
    }
    let codec = if ids.len() == 1 { Codec::Lzma2 } else { Codec::Xz { filters } };
    Opened { layer, view: View::Decode { from: 12 + header_len, codec } }
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
/// read, of those `want` names if any, with GNU and pax long names.
fn tar(data: &[u8], want: &dyn Fn(&str) -> bool) -> Option<Opened> {
    let first = data.get(..512)?;
    if !tar_checksum_ok(first) {
        return None;
    }
    let mut at = 0usize;
    let mut long_name: Option<String> = None;
    let mut long_size: Option<u64> = None;
    let mut best: Option<(bool, u64, usize, String)> = None;
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
                let wanted = want(&name);
                if body < data.len() && best.as_ref().is_none_or(|(w, s, _, _)| (wanted, size) > (*w, *s)) {
                    best = Some((wanted, size, body, name));
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
    let Some((_, size, from, name)) = best else {
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
