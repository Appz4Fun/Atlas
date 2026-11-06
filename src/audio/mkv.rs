//! Audio frames out of a Matroska file: the first audio track from the file's first bytes, then that track's frames
//! from a run of bytes read anywhere in the middle, which may start part way through an element. What's read comes
//! from Usenet, soo every read is bounds checked and a bad file gives fewer frames, never a panic.
#![cfg_attr(not(test), deny(clippy::indexing_slicing, clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use crate::probe::media::{element_id, element_size};

const SEGMENT: u32 = 0x1853_8067;
const INFO: u32 = 0x1549_A966;
const TIMESTAMP_SCALE: u32 = 0x2A_D7B1;
const TRACKS: u32 = 0x1654_AE6B;
const TRACK_ENTRY: u32 = 0xAE;
const CLUSTER: u32 = 0x1F43_B675;
const TIMESTAMP: u32 = 0xE7;
const SIMPLE_BLOCK: u32 = 0xA3;
const BLOCK_GROUP: u32 = 0xA0;
const BLOCK: u32 = 0xA1;
/// What may come before a cluster's timestamp
const CRC32: u32 = 0xBF;
const VOID: u32 = 0xEC;

/// The most one frame may be: a FLAC frame of 7.1 at 192 kHz is a few hundred KB.
const MAX_FRAME: usize = 4 << 20;
/// The most frames a block's lacing may say it holds: the count is one byte.
const MAX_LACED: usize = 256;
/// The most bytes header stripping may put back on a frame: real ones are a few bytes of a frame header.
const MAX_STRIP: usize = 64;
/// The most bytes of frames one read gives, the stripped headers put back included: a window's articles hold far
/// less, and a crafted file can't make a few small laced blocks into gigabytes.
const MAX_FRAMES_BYTES: usize = 64 << 20;

/// What decoding an audio track's frames needs, from the file's header.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AudioTrack {
    pub number: u64,
    /// the Matroska codec id, `A_AAC`, `A_FLAC` and so on
    pub codec: String,
    /// CodecPrivate: AAC's AudioSpecificConfig, FLAC's metadata blocks, Vorbis's three headers
    pub private: Vec<u8>,
    pub sample_rate: Option<f64>,
    pub channels: Option<u64>,
    pub bit_depth: Option<u64>,
    /// the bytes header stripping took off the front of every frame (ContentCompAlgo 3), put back on each
    pub strip: Vec<u8>,
    /// nanoseconds in one tick of a block's timestamp
    pub timestamp_scale: u64,
}

impl AudioTrack {
    /// Ticks of a block's timestamp in milliseconds.
    pub fn millis(&self, ticks: i64) -> i64 {
        (i128::from(ticks) * i128::from(self.timestamp_scale) / 1_000_000) as i64
    }
}

/// One element at `at`: its id, the size of its body (None: unknown) and where the body starts.
fn element(b: &[u8], at: usize) -> Option<(u32, Option<u64>, usize)> {
    let (id, id_len) = element_id(b.get(at..)?)?;
    let (size, size_len) = element_size(b.get(at + id_len..)?)?;
    Some((id, size, at + id_len + size_len))
}

/// The children of a whole element body.
fn children(b: &[u8]) -> impl Iterator<Item = (u32, &[u8])> {
    let mut at = 0;
    std::iter::from_fn(move || {
        let (id, size, start) = element(b, at)?;
        let end = start.checked_add(usize::try_from(size?).ok()?)?;
        let body = b.get(start..end)?;
        at = end;
        Some((id, body))
    })
}

fn uint(b: &[u8]) -> u64 {
    b.iter().take(8).fold(0, |n, byte| n << 8 | u64::from(*byte))
}

fn float(b: &[u8]) -> Option<f64> {
    match b.len() {
        4 => Some(f64::from(f32::from_be_bytes(b.try_into().ok()?))),
        8 => Some(f64::from_be_bytes(b.try_into().ok()?)),
        _ => None,
    }
    .filter(|f| f.is_finite())
}

/// The first audio track, from a file's first bytes. None when there's none, or its frames are compressed or
/// encrypted in a way we don't undo (only header stripping is).
pub fn audio_track(front: &[u8]) -> Option<AudioTrack> {
    let mut at = 0;
    // EBML header, then the segment, whose size is often unknown
    while let Some((id, size, start)) = element(front, at) {
        if id == SEGMENT {
            return tracks_in(front.get(start..)?);
        }
        at = start.checked_add(usize::try_from(size?).ok()?)?;
    }
    None
}

fn tracks_in(segment: &[u8]) -> Option<AudioTrack> {
    let mut at = 0;
    // the default when Info doesn't say, or comes after Tracks
    let mut scale = 1_000_000;
    while let Some((id, size, start)) = element(segment, at) {
        let size = usize::try_from(size?).ok()?;
        let end = start.checked_add(size)?;
        if id == INFO {
            let body = segment.get(start..end)?;
            if let Some((_, v)) = children(body).find(|(id, _)| *id == TIMESTAMP_SCALE) {
                scale = uint(v).max(1);
            }
        }
        if id == TRACKS {
            let body = segment.get(start..end)?;
            // the first audio entry, readable or not: a later one is another language or a commentary
            let entry = children(body).filter(|(id, _)| *id == TRACK_ENTRY).map(|(_, e)| e).find(|e| is_audio(e))?;
            let mut track = track(entry)?;
            track.timestamp_scale = scale;
            return Some(track);
        }
        if id == CLUSTER {
            return None;
        }
        at = end;
    }
    None
}

fn is_audio(entry: &[u8]) -> bool {
    children(entry).any(|(id, v)| id == 0x83 && uint(v) == 2)
}

fn track(entry: &[u8]) -> Option<AudioTrack> {
    let mut t = AudioTrack { timestamp_scale: 1_000_000, ..AudioTrack::default() };
    let mut number = None;
    for (id, v) in children(entry) {
        match id {
            0xD7 => number = Some(uint(v)),
            0x86 => t.codec = String::from_utf8_lossy(v).chars().take(64).collect(),
            0x63A2 => t.private = v.to_vec(),
            0xE1 => {
                for (id, v) in children(v) {
                    match id {
                        0xB5 => t.sample_rate = float(v),
                        0x9F => t.channels = Some(uint(v)),
                        0x6264 => t.bit_depth = Some(uint(v)),
                        _ => {}
                    }
                }
            }
            0x6D80 => t.strip = stripped(v)?,
            _ => {}
        }
    }
    t.number = number.filter(|n| *n > 0)?;
    Some(t)
}

/// The bytes header stripping took off each frame, from ContentEncodings. None for anything else: zlib, bzip2,
/// LZO or encryption, which leave the frames unreadable here.
fn stripped(encodings: &[u8]) -> Option<Vec<u8>> {
    let mut strip = Vec::new();
    for (id, encoding) in children(encodings) {
        if id != 0x6240 {
            continue;
        }
        // ContentEncodingType: 0 compression (the default), 1 encryption
        if children(encoding).any(|(id, v)| id == 0x5033 && uint(v) != 0) {
            return None;
        }
        // ContentEncodingScope: 1, the frames (the default); 2 and 4 are the private data and the next encoding
        let scope = children(encoding).find(|(id, _)| *id == 0x5032).map_or(1, |(_, v)| uint(v));
        if scope & 1 == 0 {
            continue;
        }
        let compression = children(encoding).find(|(id, _)| *id == 0x5034).map(|(_, v)| v)?;
        let algo = children(compression).find(|(id, _)| *id == 0x4254).map_or(0, |(_, v)| uint(v));
        if algo != 3 {
            return None;
        }
        if let Some((_, settings)) = children(compression).find(|(id, _)| *id == 0x4255) {
            strip.extend_from_slice(settings);
        }
        if strip.len() > MAX_STRIP {
            return None;
        }
    }
    Some(strip)
}

/// One frame of a track: when its block plays, in ms, its place in the block's lace (0 for the first, the one the
/// block's time is for), and its bytes, with any stripped header put back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub millis: i64,
    pub lace: usize,
    pub bytes: Vec<u8>,
}

/// A cluster's start is a cluster id, a size, and a timestamp among its first children. Bytes that happen to look
/// like a cluster id seldom have that after them. Where its blocks start, where it ends (None: unknown size), and
/// its timestamp.
fn cluster_at(b: &[u8], at: usize) -> Option<(usize, Option<usize>, i64)> {
    let (id, size, start) = element(b, at)?;
    if id != CLUSTER {
        return None;
    }
    let size = match size {
        Some(s) => Some(usize::try_from(s).ok().filter(|s| *s >= 3 && *s <= 64 << 20)?),
        None => None,
    };
    let mut child = start;
    for _ in 0..3 {
        let (id, len, body) = element(b, child)?;
        let len = usize::try_from(len?).ok()?;
        match id {
            TIMESTAMP if len <= 8 => {
                return Some((body + len, size.map(|s| start + s), uint(b.get(body..body + len)?) as i64));
            }
            CRC32 | VOID if len <= 4096 => child = body + len,
            _ => return None,
        }
    }
    None
}

/// The track number, relative time and flags of a block body, and where its data starts.
fn block_head(b: &[u8]) -> Option<(u64, i16, u8, usize)> {
    let first = *b.first()?;
    let track_len = first.leading_zeros() as usize + 1;
    if track_len > 8 {
        return None;
    }
    let mut track = u64::from(first) & ((1u64 << (8 - track_len)) - 1);
    for byte in b.get(1..track_len)? {
        track = track << 8 | u64::from(*byte);
    }
    let time = i16::from_be_bytes([*b.get(track_len)?, *b.get(track_len + 1)?]);
    let flags = *b.get(track_len + 2)?;
    Some((track, time, flags, track_len + 3))
}

/// The frames a block's data holds, by its lacing: none, Xiph, fixed size or EBML. None when the lacing doesn't add
/// up to the data.
fn unlace(flags: u8, data: &[u8]) -> Option<Vec<&[u8]>> {
    let lacing = (flags >> 1) & 3;
    if lacing == 0 {
        return Some(vec![data]);
    }
    let count = usize::from(*data.first()?) + 1;
    if count > MAX_LACED {
        return None;
    }
    let mut at = 1;
    let mut sizes: Vec<usize> = Vec::with_capacity(count);
    match lacing {
        // Xiph: each size but the last as a run of 255s and the rest
        1 => {
            for _ in 1..count {
                let mut size = 0usize;
                loop {
                    let b = *data.get(at)?;
                    at += 1;
                    size = size.checked_add(usize::from(b))?;
                    if b != 255 {
                        break;
                    }
                }
                sizes.push(size);
            }
        }
        // fixed: the rest cut in equal parts
        2 => {
            let rest = data.len().checked_sub(at)?;
            if rest % count != 0 {
                return None;
            }
            sizes.extend(std::iter::repeat_n(rest / count, count - 1));
        }
        // EBML: the first size as a vint, the rest as signed differences from the one before
        _ if count > 1 => {
            let (first, len) = element_size(data.get(at..)?)?;
            at += len;
            let mut size = i64::try_from(first?).ok()?;
            sizes.push(usize::try_from(size).ok()?);
            for _ in 2..count {
                let (raw, len) = element_size(data.get(at..)?)?;
                at += len;
                // the vint's value less half its range
                let bias = (1i64 << (7 * len - 1)) - 1;
                size = size.checked_add(i64::try_from(raw?).ok()? - bias)?;
                sizes.push(usize::try_from(size).ok()?);
            }
        }
        _ => {}
    }
    let known: usize = sizes.iter().try_fold(0usize, |n, s| n.checked_add(*s))?;
    let last = data.len().checked_sub(at)?.checked_sub(known)?;
    sizes.push(last);
    let mut out = Vec::with_capacity(count);
    for size in sizes {
        out.push(data.get(at..at.checked_add(size)?)?);
        at += size;
    }
    Some(out)
}

/// The time of the first cluster that starts in `data`, in ms. Where a read from the middle of a file landed.
pub fn first_time(data: &[u8], track: &AudioTrack) -> Option<i64> {
    let mut from = 0;
    while let Some(pos) = next_cluster(data, from) {
        from = pos + 1;
        if let Some((_, _, time)) = cluster_at(data, pos) {
            return Some(track.millis(time));
        }
    }
    None
}

/// The time of the last cluster that starts in `data`, in ms: how far a read from the middle of a file has got.
pub fn last_time(data: &[u8], track: &AudioTrack) -> Option<i64> {
    let mut from = 0;
    let mut last = None;
    while let Some(pos) = next_cluster(data, from) {
        from = pos + 1;
        if let Some((_, _, time)) = cluster_at(data, pos) {
            last = Some(track.millis(time));
        }
    }
    last
}

/// Every frame of `track` in the clusters that start in `data`, in the order they're stored. A cluster cut off by the
/// end of `data` gives the frames that are all there.
pub fn frames(data: &[u8], track: &AudioTrack) -> Vec<Frame> {
    let mut out = Vec::new();
    let mut held = 0usize;
    // each element is two bytes at least, soo the walk reads at most half as many elements as there are bytes;
    // cluster ids hidden in Void bodies would each start a walk to the end, and the budget keeps that linear
    let mut budget = data.len() / 2 + 4096;
    let mut from = 0;
    while let Some(pos) = next_cluster(data, from) {
        from = pos + 1;
        let Some((mut at, end, cluster_time)) = cluster_at(data, pos) else { continue };
        let end = end.unwrap_or(usize::MAX);
        while at < end {
            let Some(left) = budget.checked_sub(1) else { return out };
            budget = left;
            let Some((id, size, start)) = element(data, at) else { break };
            let Some(size) = size.and_then(|s| usize::try_from(s).ok()) else { break };
            let Some(body) = start.checked_add(size).and_then(|stop| data.get(start..stop)) else { break };
            let block = match id {
                SIMPLE_BLOCK => Some(body),
                BLOCK_GROUP => children(body).find(|(id, _)| *id == BLOCK).map(|(_, b)| b),
                // a new cluster starts where an unknown sized one ends
                CLUSTER => break,
                _ => None,
            };
            at = start + size;
            let Some((number, time, flags, head)) = block.and_then(block_head) else { continue };
            if number != track.number {
                continue;
            }
            let Some(laced) = block.and_then(|b| b.get(head..)).and_then(|d| unlace(flags, d)) else { continue };
            let millis = track.millis(cluster_time + i64::from(time));
            for (lace, frame) in laced.into_iter().enumerate() {
                let len = frame.len() + track.strip.len();
                if len > MAX_FRAME {
                    continue;
                }
                held += len;
                if held > MAX_FRAMES_BYTES {
                    return out;
                }
                let mut bytes = Vec::with_capacity(len);
                bytes.extend_from_slice(&track.strip);
                bytes.extend_from_slice(frame);
                out.push(Frame { millis, lace, bytes });
            }
        }
        // the walk goes on from where this cluster ended, not from inside it
        from = from.max(at.min(data.len()));
    }
    out
}

/// The next place `data` holds a cluster id, from `from`.
fn next_cluster(data: &[u8], from: usize) -> Option<usize> {
    data.get(from..)?.windows(4).position(|w| w == [0x1F, 0x43, 0xB6, 0x75]).map(|p| p + from)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// An EBML element: id bytes, then an 8-byte size, then the body.
    pub(crate) fn el(id: &[u8], body: &[u8]) -> Vec<u8> {
        let mut out = id.to_vec();
        out.push(0x01);
        out.extend_from_slice(&(body.len() as u64).to_be_bytes()[1..]);
        out.extend_from_slice(body);
        out
    }

    pub(crate) fn header(entries: &[Vec<u8>]) -> Vec<u8> {
        let info = el(&[0x15, 0x49, 0xA9, 0x66], &el(&[0x2A, 0xD7, 0xB1], &[0x0F, 0x42, 0x40]));
        let tracks = el(&[0x16, 0x54, 0xAE, 0x6B], &entries.concat());
        let mut segment = vec![0x18, 0x53, 0x80, 0x67, 0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF];
        segment.extend(info);
        segment.extend(tracks);
        [el(&[0x1A, 0x45, 0xDF, 0xA3], &el(&[0x42, 0x86], &[1])), segment].concat()
    }

    pub(crate) fn cluster(time: u64, blocks: &[Vec<u8>]) -> Vec<u8> {
        let mut body = el(&[0xE7], &time.to_be_bytes()[6..]);
        for b in blocks {
            body.extend(el(&[0xA3], b));
        }
        el(&[0x1F, 0x43, 0xB6, 0x75], &body)
    }

    pub(crate) fn simple(track: u8, time: i16, flags: u8, data: &[u8]) -> Vec<u8> {
        let mut b = vec![0x80 | track];
        b.extend_from_slice(&time.to_be_bytes());
        b.push(flags);
        b.extend_from_slice(data);
        b
    }

    fn entry(number: u8, kind: u8, codec: &[u8], extra: Vec<u8>) -> Vec<u8> {
        el(&[0xAE], &[el(&[0xD7], &[number]), el(&[0x83], &[kind]), el(&[0x86], codec), extra].concat())
    }

    #[test]
    fn the_first_audio_track_comes_from_the_header() {
        let audio = el(&[0xE1], &[el(&[0xB5], &48_000f32.to_be_bytes()), el(&[0x9F], &[2])].concat());
        let front = header(&[
            entry(1, 1, b"V_MPEG4/ISO/AVC", Vec::new()),
            entry(2, 2, b"A_AAC", [el(&[0x63, 0xA2], &[0x11, 0x90]), audio].concat()),
            entry(3, 2, b"A_AC3", Vec::new()),
        ]);
        let t = audio_track(&front).unwrap();
        assert_eq!((t.number, t.codec.as_str(), t.private.as_slice()), (2, "A_AAC", &[0x11, 0x90][..]));
        assert_eq!((t.sample_rate, t.channels, t.timestamp_scale), (Some(48_000.0), Some(2), 1_000_000));
        assert!(t.strip.is_empty());
        // no audio at all
        assert_eq!(audio_track(&header(&[entry(1, 1, b"V_MPEG4/ISO/AVC", Vec::new())])), None);
    }

    #[test]
    fn header_stripping_is_put_back_and_zlib_refused() {
        let strip = |algo: u8| {
            let compression =
                el(&[0x50, 0x34], &[el(&[0x42, 0x54], &[algo]), el(&[0x42, 0x55], &[0xFF, 0xFB])].concat());
            el(&[0x6D, 0x80], &el(&[0x62, 0x40], &compression))
        };
        let t = audio_track(&header(&[entry(1, 2, b"A_MPEG/L3", strip(3))])).unwrap();
        assert_eq!(t.strip, [0xFF, 0xFB]);
        let data = cluster(10, &[simple(1, 0, 0x80, &[1, 2, 3])]);
        assert_eq!(frames(&data, &t)[0].bytes, [0xFF, 0xFB, 1, 2, 3]);
        assert_eq!(audio_track(&header(&[entry(1, 2, b"A_MPEG/L3", strip(0))])), None);
        // a header too long to be one, put back on every frame of a long lace, would be gigabytes
        let long = el(
            &[0x6D, 0x80],
            &el(&[0x62, 0x40], &el(&[0x50, 0x34], &[el(&[0x42, 0x54], &[3]), el(&[0x42, 0x55], &[0; 4096])].concat())),
        );
        assert_eq!(audio_track(&header(&[entry(1, 2, b"A_MPEG/L3", long)])), None);
        let mut wide = t.clone();
        wide.strip = vec![0; MAX_FRAME - 16];
        let laced = simple(1, 0, 0x04, &[255; 257]);
        let blocks: Vec<Vec<u8>> = (0..4).map(|_| laced.clone()).collect();
        let got = frames(&cluster(0, &blocks), &wide);
        assert!(got.iter().map(|f| f.bytes.len()).sum::<usize>() <= MAX_FRAMES_BYTES, "{} frames", got.len());
    }

    #[test]
    fn frames_are_found_from_any_point_and_laced_ones_split() {
        let track = AudioTrack { number: 2, timestamp_scale: 1_000_000, ..Default::default() };
        let xiph = simple(2, 20, 0x02, &[2, 3, 1, 9, 9, 9, 8, 7, 7]);
        let fixed = simple(2, 40, 0x04, &[1, 5, 5, 6, 6]);
        // EBML: sizes 2, then 2 + 1 = 3, then the rest
        let ebml = simple(2, 60, 0x06, &[2, 0x82, 0xC0, 1, 1, 2, 2, 2, 3]);
        let mut data = vec![0x00, 0x1F, 0x43, 0xB6]; // a cut off id first
        data.extend(cluster(1_000, &[simple(1, 0, 0x80, &[0; 8]), simple(2, 0, 0x80, &[4, 4]), xiph, fixed, ebml]));
        data.extend(cluster(2_000, &[simple(2, 0, 0x80, &[5])]));
        let got = frames(&data, &track);
        let bytes: Vec<&[u8]> = got.iter().map(|f| f.bytes.as_slice()).collect();
        assert_eq!(bytes, [&[4, 4][..], &[9, 9, 9], &[8], &[7, 7], &[5, 5], &[6, 6], &[1, 1], &[2, 2, 2], &[3], &[5]]);
        assert_eq!(
            got.iter().map(|f| f.millis).collect::<Vec<_>>(),
            [1000, 1020, 1020, 1020, 1040, 1040, 1060, 1060, 1060, 2000]
        );
        assert_eq!(got.iter().map(|f| f.lace).collect::<Vec<_>>(), [0, 0, 1, 2, 0, 1, 0, 1, 2, 0]);
        assert_eq!(first_time(&data, &track), Some(1000));
        // cut off part way through the second cluster's block: the first cluster's frames are all there
        let cut = &data[..data.len() - 2];
        assert_eq!(frames(cut, &track).len(), 9);
    }

    #[test]
    fn noise_finds_nothing_and_never_panics() {
        let track = AudioTrack { number: 1, timestamp_scale: 1_000_000, ..Default::default() };
        let mut x = 0x2545_f491_4f6c_dd1du64;
        let mut noise = vec![0u8; 1 << 16];
        for b in noise.iter_mut() {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            *b = x as u8;
        }
        // cluster ids everywhere
        for at in (0..noise.len() - 4).step_by(97) {
            noise[at..at + 4].copy_from_slice(&[0x1F, 0x43, 0xB6, 0x75]);
        }
        let _ = frames(&noise, &track);
        let _ = first_time(&noise, &track);
        assert_eq!(audio_track(&noise), None);
        for flags in 0..8u8 {
            for n in 0..64 {
                let _ = unlace(flags << 1, &noise[n..n + 40]);
            }
        }
        assert_eq!(unlace(0x02, &[]), None);
        assert_eq!(unlace(0x04, &[1, 1, 2, 3]), None, "3 bytes don't split in two");
    }
}
