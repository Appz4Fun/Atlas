//! Keyframes out of a Matroska file read from the middle: a run of bytes from anywhere in the file, which may
//! start part way through an element, scanned for a cluster, then for a video keyframe in it.

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
const REFERENCE_BLOCK: u32 = 0xFB;
/// What may come before a cluster's timestamp
const CRC32: u32 = 0xBF;
const VOID: u32 = 0xEC;
const SEEK_HEAD: u32 = 0x114D_9B74;
const SEEK: u32 = 0x4DBB;
const SEEK_ID: u32 = 0x53AB;
const SEEK_POSITION: u32 = 0x53AC;
const CUES: u32 = 0x1C53_BB6B;
const CUE_POINT: u32 = 0xBB;
const CUE_TIME: u32 = 0xB3;
const CUE_TRACK_POSITIONS: u32 = 0xB7;
const CUE_TRACK: u32 = 0xF7;
const CUE_CLUSTER_POSITION: u32 = 0xF1;

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
    /// nanoseconds in one tick of a block's timestamp
    pub timestamp_scale: u64,
    /// DisplayWidth and DisplayHeight: the shape the picture is shown at, when it isn't its samples'
    pub display: Option<(u64, u64)>,
}

impl VideoTrack {
    /// A timestamp from [`find_keyframe`] in milliseconds.
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
    // the default when Info doesn't say, or comes after Tracks
    let mut scale = 1_000_000;
    while let Some((id, size, start)) = element(segment, at) {
        let size = usize::try_from(size?).ok()?;
        if id == INFO {
            let body = segment.get(start..start + size)?;
            if let Some((_, v)) = children(body).find(|(id, _)| *id == TIMESTAMP_SCALE) {
                scale = uint(v).max(1);
            }
        }
        if id == TRACKS {
            let body = segment.get(start..start + size)?;
            let mut track = children(body).filter(|(id, _)| *id == TRACK_ENTRY).find_map(|(_, entry)| track(entry))?;
            track.timestamp_scale = scale;
            return Some(track);
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
    let mut display = None;
    for (id, v) in children(entry) {
        match id {
            0xE0 => display = display_size(v),
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
    let track = VideoTrack { number: number?, codec, private: private?, timestamp_scale: 1_000_000, display };
    (kind? == 1).then_some(track)
}

/// DisplayWidth and DisplayHeight from a Video element, when both are there. Whatever their unit, they give the
/// shape.
fn display_size(video: &[u8]) -> Option<(u64, u64)> {
    let (mut w, mut h) = (None, None);
    for (id, v) in children(video) {
        match id {
            0x54B0 => w = Some(uint(v)),
            0x54BA => h = Some(uint(v)),
            _ => {}
        }
    }
    Some((w.filter(|w| *w > 0)?, h.filter(|h| *h > 0)?))
}

/// Where a file's segment body starts and where its Cues element is, both from the file's start, by the SeekHead
/// in its first bytes. None when the front has no SeekHead naming the Cues: a file written as a stream.
pub fn cues_from_front(front: &[u8]) -> Option<(u64, u64)> {
    let mut at = 0;
    while let Some((id, size, start)) = element(front, at) {
        if id == SEGMENT {
            let mut child = start;
            while let Some((id, size, body_at)) = element(front, child) {
                if id == CLUSTER {
                    return None;
                }
                let size = usize::try_from(size?).ok()?;
                if id == SEEK_HEAD {
                    let body = front.get(body_at..body_at.checked_add(size)?)?;
                    for (_, seek) in children(body).filter(|(id, _)| *id == SEEK) {
                        let (mut target, mut position) = (None, None);
                        for (id, v) in children(seek) {
                            match id {
                                SEEK_ID => target = Some(uint(v)),
                                SEEK_POSITION => position = Some(uint(v)),
                                _ => {}
                            }
                        }
                        if target == Some(u64::from(CUES)) {
                            let segment = start as u64;
                            return Some((segment, segment.checked_add(position?)?));
                        }
                    }
                }
                child = body_at.checked_add(size)?;
            }
            return None;
        }
        at = start.checked_add(usize::try_from(size?).ok()?)?;
    }
    None
}

/// How many bytes the Cues element starting `head` takes, header and all, by its header. None when `head` isn't
/// the start of a Cues element.
pub fn cues_len(head: &[u8]) -> Option<u64> {
    let (id, size, start) = element(head, 0)?;
    (id == CUES).then_some(())?;
    size?.checked_add(start as u64)
}

/// The cue points of `track` in a whole Cues element: (time in the segment's ticks, the cluster's position from the
/// segment body's start), in the order they're listed.
pub fn cue_points(cues: &[u8], track: u64) -> Vec<(u64, u64)> {
    let Some((CUES, Some(size), start)) = element(cues, 0) else { return Vec::new() };
    let Some(body) = usize::try_from(size).ok().and_then(|size| cues.get(start..start.checked_add(size)?)) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (_, point) in children(body).filter(|(id, _)| *id == CUE_POINT) {
        let mut time = None;
        let mut cluster = None;
        for (id, v) in children(point) {
            match id {
                CUE_TIME => time = Some(uint(v)),
                CUE_TRACK_POSITIONS => {
                    let (mut number, mut position) = (None, None);
                    for (id, v) in children(v) {
                        match id {
                            CUE_TRACK => number = Some(uint(v)),
                            CUE_CLUSTER_POSITION => position = Some(uint(v)),
                            _ => {}
                        }
                    }
                    if number == Some(track) {
                        cluster = cluster.or(position);
                    }
                }
                _ => {}
            }
        }
        if let (Some(time), Some(cluster)) = (time, cluster) {
            out.push((time, cluster));
        }
    }
    out
}

/// The cue point nearest `percent` of the way through, by time: the last point's time stands for the length.
pub fn cue_at(points: &[(u64, u64)], percent: u8) -> Option<(u64, u64)> {
    let last = points.iter().map(|p| p.0).max()?;
    let target = u128::from(last) * u128::from(percent) / 100;
    points.iter().copied().min_by_key(|(time, _)| u128::from(*time).abs_diff(target))
}

/// A keyframe found in a run of bytes. Its timestamp is in the segment's ticks: see [`VideoTrack::millis`].
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
    // Each element is at least two bytes, so one pass over real clusters reads at most half as many elements as
    // there are bytes. Cluster ids hidden in Void bodies would each start a walk to the data's end: the budget keeps
    // that linear rather than quadratic.
    let mut budget = data.len() / 2 + 4096;
    let mut from = 0;
    while let Some(pos) = memchr_cluster(data, from) {
        from = pos + 1;
        let Some((mut at, end, cluster_time)) = cluster_at(data, pos) else { continue };
        let end = end.unwrap_or(usize::MAX);
        // the cluster's blocks, in order, until it ends or the data does
        while at < end {
            let Some(left) = budget.checked_sub(1) else { return Found::None };
            budget = left;
            let Some((id, size, start)) = element(data, at) else { break };
            let Some(size) = size.and_then(|s| usize::try_from(s).ok()) else { break };
            match id {
                SIMPLE_BLOCK | BLOCK_GROUP => {
                    let have = data.len().saturating_sub(start).min(size);
                    let body = &data[start..start + have];
                    let (block, key) = if id == SIMPLE_BLOCK {
                        (Some((0, body)), block_head(body).map(|h| h.2 & 0x80 != 0))
                    } else {
                        // a group is a keyframe when nothing's referenced; seen only once it's all here
                        let mut block = None;
                        let mut referenced = false;
                        for (cid, v) in children(body) {
                            let Some(left) = budget.checked_sub(1) else { return Found::None };
                            budget = left;
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
    fn the_cues_say_where_each_keyframe_s_cluster_starts() {
        let data = fixture();
        let t = video_track(&data).unwrap();
        let (segment, cues_at) = cues_from_front(&data[..4096]).unwrap();
        let len = cues_len(&data[cues_at as usize..]).unwrap();
        let points = cue_points(&data[cues_at as usize..(cues_at + len) as usize], t.number);
        assert!(points.len() >= 3, "{points:?}");
        for (_, cluster) in &points {
            let at = (segment + cluster) as usize;
            assert_eq!(&data[at..at + 4], &[0x1F, 0x43, 0xB6, 0x75], "a cluster starts there");
        }
        let (first, last) = (cue_at(&points, 0).unwrap(), cue_at(&points, 100).unwrap());
        assert!(first.0 < last.0);
        assert!(cue_points(&data[cues_at as usize..(cues_at + len) as usize], t.number + 7).is_empty());
        assert!(cues_len(&data[..16]).is_none(), "not a Cues element");
    }

    #[test]
    fn cluster_ids_hidden_in_voids_cost_linear_work() {
        // a real looking cluster start, then Void elements each holding another one: every hidden cluster would
        // start a walk over all the Voids after it
        let start = [0x1F, 0x43, 0xB6, 0x75, 0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xE7, 0x81, 0x00];
        let mut data = start.to_vec();
        for _ in 0..40_000 {
            data.extend([0xEC, 0x80 | start.len() as u8]);
            data.extend(start);
        }
        let began = std::time::Instant::now();
        assert_eq!(find_keyframe(&data, 1), Found::None);
        assert!(began.elapsed() < std::time::Duration::from_secs(1), "{:?}", began.elapsed());
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
                let [y, cb, cr] = p.to_8bit(320, None);
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
    fn a_track_number_past_127_is_found() {
        // a cluster at time 5 holding one keyframe of track 128, whose number takes two bytes
        let frame = [9u8; 20];
        let mut block = vec![0x40, 0x80, 0x00, 0x02, 0x80];
        block.extend(frame);
        let mut body = vec![0xE7, 0x81, 0x05, 0xA3, 0x80 | block.len() as u8];
        body.extend(&block);
        let mut data = vec![0x00, 0x1F, 0x43, 0xB6, 0x75, 0x80 | body.len() as u8];
        data.extend(&body);
        let Found::Frame { start, len, timestamp } = find_keyframe(&data, 128) else { panic!() };
        assert_eq!((&data[start..start + len], timestamp), (&frame[..], 7));
        assert_eq!(find_keyframe(&data, 1), Found::None);
    }

    #[test]
    fn timestamps_follow_the_segment_scale() {
        let mut t = video_track(&fixture()).unwrap();
        assert_eq!(t.timestamp_scale, 1_000_000);
        assert_eq!(t.millis(2500), 2500);
        t.timestamp_scale = 100_000;
        assert_eq!(t.millis(2500), 250);
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
