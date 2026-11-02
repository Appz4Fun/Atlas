//! Keyframes out of a Matroska file read from the middle: a run of bytes from anywhere in the file, which may
//! start part way through an element, scanned for a cluster, then for a video keyframe in it.

use crate::probe::media::{element_id, element_size};

const SEGMENT: u32 = 0x1853_8067;
const TRACKS: u32 = 0x1654_AE6B;
const TRACK_ENTRY: u32 = 0xAE;
const CLUSTER: u32 = 0x1F43_B675;
const TIMESTAMP: u32 = 0xE7;
const SIMPLE_BLOCK: u32 = 0xA3;
const BLOCK_GROUP: u32 = 0xA0;
const BLOCK: u32 = 0xA1;
const REFERENCE_BLOCK: u32 = 0xFB;
/// What may come before a cluster's timestamp
const CRC32: u32 = 0xBF;
const VOID: u32 = 0xEC;

/// The most a keyframe may be: a 2160p IDR at a high bit rate is a few MB.
pub const MAX_KEYFRAME: usize = 16 << 20;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Codec {
    Hevc,
    Avc,
}

/// What decoding a video track's frames needs, from the file's header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VideoTrack {
    pub number: u64,
    pub codec: Codec,
    /// hvcC or avcC
    pub private: Vec<u8>,
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

/// The first video track that we can decode, from a file's first bytes.
pub fn video_track(front: &[u8]) -> Option<VideoTrack> {
    let mut at = 0;
    // EBML header, then the segment, whose size is often unknown
    while let Some((id, size, start)) = element(front, at) {
        if id == SEGMENT {
            return tracks_in(&front[start..]);
        }
        at = start.checked_add(usize::try_from(size?).ok()?)?;
    }
    None
}

fn tracks_in(segment: &[u8]) -> Option<VideoTrack> {
    let mut at = 0;
    while let Some((id, size, start)) = element(segment, at) {
        let size = usize::try_from(size?).ok()?;
        if id == TRACKS {
            let body = segment.get(start..start + size)?;
            return children(body).filter(|(id, _)| *id == TRACK_ENTRY).find_map(|(_, entry)| track(entry));
        }
        if id == CLUSTER {
            return None;
        }
        at = start + size;
    }
    None
}

fn track(entry: &[u8]) -> Option<VideoTrack> {
    let (mut number, mut kind, mut codec, mut private) = (None, None, None, None);
    for (id, v) in children(entry) {
        match id {
            0xD7 => number = Some(uint(v)),
            0x83 => kind = Some(uint(v)),
            0x86 => codec = Some(v),
            0x63A2 => private = Some(v.to_vec()),
            _ => {}
        }
    }
    let codec = match codec? {
        b"V_MPEGH/ISO/HEVC" => Codec::Hevc,
        b"V_MPEG4/ISO/AVC" => Codec::Avc,
        _ => return None,
    };
    (kind? == 1).then_some(VideoTrack { number: number?, codec, private: private? })
}

/// A keyframe found in a run of bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Found {
    /// the frame, all there: `data[start..start + len]`
    Frame {
        start: usize,
        len: usize,
        timestamp: i64,
    },
    /// one starts at `start`, but only `have` of its `len` bytes are here
    Partial {
        start: usize,
        len: usize,
        have: usize,
    },
    None,
}

/// A cluster's start is a cluster id, a size, and a timestamp among its first children. Bytes that happen to
/// look like a cluster id seldom have that after them.
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

/// The track number and flags of a block body, and where its frame starts. Laced blocks aren't wanted.
fn block_head(b: &[u8]) -> Option<(u64, i16, u8, usize)> {
    let (track_len, first) = (b.first()?.leading_zeros() as usize + 1, *b.first()?);
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

/// The first keyframe of `track` in a cluster that starts in `data`.
pub fn find_keyframe(data: &[u8], track: u64) -> Found {
    let mut from = 0;
    while let Some(pos) = memchr_cluster(data, from) {
        from = pos + 1;
        let Some((mut at, end, cluster_time)) = cluster_at(data, pos) else { continue };
        let end = end.unwrap_or(usize::MAX);
        // the cluster's blocks, in order, until it ends or the data does
        while at < end {
            let Some((id, size, start)) = element(data, at) else { break };
            let Some(size) = size.and_then(|s| usize::try_from(s).ok()) else { break };
            match id {
                SIMPLE_BLOCK | BLOCK_GROUP => {
                    let have = data.len().saturating_sub(start).min(size);
                    let body = &data[start..start + have];
                    let (block, key) = if id == SIMPLE_BLOCK {
                        (Some((0, body)), body.get(..4).and_then(block_head).map(|h| h.2 & 0x80 != 0))
                    } else {
                        // a group is a keyframe when nothing's referenced; seen only once it's all here
                        let mut block = None;
                        let mut referenced = false;
                        for (cid, v) in children(body) {
                            if cid == BLOCK && block.is_none() {
                                block = Some((offset_of(body, v), v));
                            }
                            referenced |= cid == REFERENCE_BLOCK;
                        }
                        (block, (have == size).then_some(!referenced))
                    };
                    let Some((inner, block)) = block else {
                        if have < size {
                            break;
                        }
                        at = start + size;
                        continue;
                    };
                    let Some((number, time, flags, head)) = block_head(block) else { break };
                    if number == track && key == Some(true) && flags & 0x06 == 0 {
                        let frame_start = start + inner + head;
                        let frame_len = if id == SIMPLE_BLOCK { size - head } else { block.len() - head };
                        if frame_len > MAX_KEYFRAME {
                            break;
                        }
                        if have < size {
                            return Found::Partial { start: frame_start, len: frame_len, have: have - head };
                        }
                        return Found::Frame {
                            start: frame_start,
                            len: frame_len,
                            timestamp: cluster_time + i64::from(time),
                        };
                    }
                    if have < size {
                        break;
                    }
                    at = start + size;
                }
                // a new cluster starts where an unknown sized one ends
                CLUSTER => break,
                _ => at = start.saturating_add(size),
            }
        }
    }
    Found::None
}

fn offset_of(outer: &[u8], inner: &[u8]) -> usize {
    inner.as_ptr() as usize - outer.as_ptr() as usize
}

/// The next place `data` holds a cluster id, from `from`.
fn memchr_cluster(data: &[u8], from: usize) -> Option<usize> {
    data.get(from..)?.windows(4).position(|w| w == [0x1F, 0x43, 0xB6, 0x75]).map(|p| p + from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Vec<u8> {
        std::fs::read(format!("{}/tests/fixtures/hevc/clip.mkv", env!("CARGO_MANIFEST_DIR"))).unwrap()
    }

    #[test]
    fn the_video_track_and_its_hvcc_come_from_the_header() {
        let t = video_track(&fixture()).unwrap();
        assert_eq!((t.number, &t.codec), (1, &Codec::Hevc));
        assert_eq!(t.private[0], 1, "hvcC version");
    }

    #[test]
    fn a_keyframe_is_found_from_any_point_and_decodes() {
        let data = fixture();
        let t = video_track(&data).unwrap();
        let hvcc = crate::video::hevc::nal::parse_hvcc(&t.private).unwrap();
        let mut stamps = Vec::new();
        // a keyframe a second, for 12 s
        for from in [data.len() / 10, data.len() * 35 / 100, data.len() * 6 / 10] {
            let chunk = &data[from..];
            let Found::Frame { start, len, timestamp } = find_keyframe(chunk, t.number) else {
                panic!("nothing found from {from}")
            };
            let mut units = hvcc.units.clone();
            units.extend(crate::video::hevc::nal::split_sized(&chunk[start..start + len], hvcc.length_size).unwrap());
            let p = crate::video::hevc::decode_keyframe(&units).unwrap();
            assert_eq!(p.luma8().0, 320);
            if let Ok(dir) = std::env::var("ATLAS_STILL_DIR") {
                let [y, cb, cr] = p.to_8bit(320);
                let jpeg = crate::video::jpeg::encode(&y.data, &cb.data, &cr.data, y.width, y.height, 85);
                std::fs::write(format!("{dir}/still{from}.jpg"), jpeg).unwrap();
            }
            stamps.push(timestamp);
        }
        assert!(stamps.windows(2).all(|w| w[0] < w[1]), "{stamps:?}");
    }

    #[test]
    fn a_cut_off_keyframe_says_how_much_is_there() {
        let data = fixture();
        let t = video_track(&data).unwrap();
        let from = data.len() / 2;
        let Found::Frame { start, len, .. } = find_keyframe(&data[from..], t.number) else { panic!() };
        let cut = &data[from..from + start + len / 2];
        assert_eq!(find_keyframe(cut, t.number), Found::Partial { start, len, have: len / 2 });
    }

    #[test]
    fn noise_finds_nothing() {
        let mut noise = vec![0u8; 100_000];
        let mut x = 7u32;
        for b in &mut noise {
            x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
            *b = (x >> 16) as u8;
        }
        noise[5000..5004].copy_from_slice(&[0x1F, 0x43, 0xB6, 0x75]);
        assert_eq!(find_keyframe(&noise, 1), Found::None);
        assert_eq!(video_track(&noise), None);
    }
}
