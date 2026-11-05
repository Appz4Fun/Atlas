//! MP4 (ISO base media): the video track's sample tables, enough to say where a keyframe's bytes are in the file.
//! Unlike Matroska, an MP4's frames carry no marks of their own: the `moov` says where every sample is. What's
//! read comes from Usenet, soo every count is bounded and a bad table is None, never a panic.

use super::mkv::{Codec, VideoTrack};

/// The biggest `moov` read: a feature film's sample tables are a few MB.
pub const MAX_MOOV: u64 = 16 << 20;
/// The most samples a track's tables may list: a day of video at 60 frames a second.
const MAX_SAMPLES: u32 = 6_000_000;
/// The biggest one sample may be: a 2160p keyframe is a few MB at most.
pub const MAX_SAMPLE: u32 = 8 << 20;

/// A box's header at the start of `b`: its type, its whole size (header and body, None: to the end of the file)
/// and its header's length.
pub fn box_head(b: &[u8]) -> Option<([u8; 4], Option<u64>, usize)> {
    let size = u32::from_be_bytes(b.get(..4)?.try_into().ok()?);
    let kind: [u8; 4] = b.get(4..8)?.try_into().ok()?;
    match size {
        0 => Some((kind, None, 8)),
        1 => {
            let long = u64::from_be_bytes(b.get(8..16)?.try_into().ok()?);
            (long >= 16).then_some((kind, Some(long), 16))
        }
        n if n >= 8 => Some((kind, Some(u64::from(n)), 8)),
        _ => None,
    }
}

/// Where the top-level `moov` is, from the file's first bytes.
#[derive(Debug, PartialEq, Eq)]
pub enum Moov {
    /// at this offset, this many bytes long, header and all
    At(u64, u64),
    /// not in `front`: the next top-level box starts here, past what was read
    After(u64),
    /// not an MP4 this can read: no `moov` before the file ends, or a box that doesn't add up
    None,
}

/// Walks the top-level boxes of `front`, the first bytes of the file from offset `base`, for the `moov`.
pub fn find_moov(front: &[u8], base: u64) -> Moov {
    let mut at = 0usize;
    loop {
        let Some(rest) = front.get(at..).filter(|r| !r.is_empty()) else { return Moov::After(base + at as u64) };
        let Some((kind, size, head)) = box_head(rest) else {
            // a header cut off by the end of what was read
            return if rest.len() < 16 { Moov::After(base + at as u64) } else { Moov::None };
        };
        let Some(size) = size else {
            // runs to the end of the file: nothing after it
            return if &kind == b"moov" { Moov::At(base + at as u64, u64::MAX) } else { Moov::None };
        };
        if size < head as u64 {
            return Moov::None;
        }
        if &kind == b"moov" {
            return Moov::At(base + at as u64, size);
        }
        let Some(end) = usize::try_from(size).ok().and_then(|s| at.checked_add(s)) else {
            return Moov::After(base.saturating_add(at as u64).saturating_add(size));
        };
        if end > front.len() {
            return Moov::After(base + end as u64);
        }
        at = end;
    }
}

/// The children of a box body, as (type, body).
fn children(b: &[u8]) -> impl Iterator<Item = ([u8; 4], &[u8])> {
    let mut at = 0usize;
    std::iter::from_fn(move || {
        let rest = b.get(at..).filter(|r| r.len() >= 8)?;
        let (kind, size, head) = box_head(rest)?;
        let size = usize::try_from(size.unwrap_or(rest.len() as u64)).ok()?;
        if size < head || size > rest.len() {
            return None;
        }
        at += size;
        Some((kind, &rest[head..size]))
    })
}

fn child<'a>(b: &'a [u8], kind: &[u8; 4]) -> Option<&'a [u8]> {
    children(b).find(|(k, _)| k == kind).map(|(_, body)| body)
}

fn be32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(at..at.checked_add(4)?)?.try_into().ok()?))
}

fn be64(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_be_bytes(b.get(at..at.checked_add(8)?)?.try_into().ok()?))
}

/// A full box's table: its entry count after the version and flags, and the entries, each `width` bytes, bounded.
fn table(b: &[u8], skip: usize, width: usize) -> Option<(usize, &[u8])> {
    let count = be32(b, 4 + skip)?;
    if count > MAX_SAMPLES {
        return None;
    }
    let start = 8 + skip;
    let entries = b.get(start..start.checked_add(count as usize * width)?)?;
    Some((count as usize, entries))
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Sizes {
    /// every sample this size, this many
    Fixed(u32, u32),
    Each(Vec<u32>),
}

impl Sizes {
    fn count(&self) -> u32 {
        match self {
            Sizes::Fixed(_, n) => *n,
            Sizes::Each(v) => v.len() as u32,
        }
    }
    fn of(&self, i: u32) -> Option<u32> {
        match self {
            Sizes::Fixed(size, n) => (i < *n).then_some(*size),
            Sizes::Each(v) => v.get(i as usize).copied(),
        }
    }
}

/// An MP4's first video track: what decoding its keyframes needs, and where its samples are.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mp4Video {
    pub codec: Codec,
    /// hvcC or avcC
    pub private: Vec<u8>,
    /// the track's width and height as shown, from `tkhd`, when it has them
    pub display: Option<(u64, u64)>,
    timescale: u32,
    /// (sample count, duration each)
    stts: Vec<(u32, u32)>,
    /// the 1-based numbers of the sync samples, in order; None: every sample is one
    stss: Option<Vec<u32>>,
    sizes: Sizes,
    /// (first chunk, 1-based, samples in each chunk from it)
    stsc: Vec<(u32, u32)>,
    chunks: Vec<u64>,
}

/// A sample's place in the file, its size and its time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sample {
    pub offset: u64,
    pub size: u32,
    pub millis: i64,
}

impl Mp4Video {
    /// The track as the Matroska reader gives it, for the decoders: its times are in milliseconds.
    pub fn track(&self) -> VideoTrack {
        VideoTrack {
            number: 0,
            codec: self.codec.clone(),
            private: self.private.clone(),
            timestamp_scale: 1_000_000,
            display: self.display,
        }
    }

    /// The keyframe nearest after `percent` of the way through the track's running time (or the last one before it,
    /// at the very end). By time, not by sample count: in a variable frame rate track the two part ways.
    pub fn keyframe_at(&self, percent: u8) -> Option<Sample> {
        let count = self.sizes.count();
        if count == 0 {
            return None;
        }
        let target = self.sample_at_time(percent).min(count - 1);
        let index = match &self.stss {
            None => target,
            Some(syncs) => {
                let after = syncs.partition_point(|&n| n < target + 1);
                let n = syncs.get(after).or_else(|| syncs.get(after.checked_sub(1)?))?;
                n.checked_sub(1)?
            }
        };
        self.sample(index)
    }

    /// The first sample that starts at or after `percent` of the track's duration, by `stts`; by count when the
    /// table gives no duration.
    fn sample_at_time(&self, percent: u8) -> u32 {
        let percent = u128::from(percent.min(100));
        let total: u128 = self.stts.iter().map(|&(n, d)| u128::from(n) * u128::from(d)).sum();
        if total == 0 {
            return (u128::from(self.sizes.count()) * percent / 100) as u32;
        }
        let want = total * percent / 100;
        let (mut index, mut at) = (0u128, 0u128);
        for &(n, delta) in &self.stts {
            let run = u128::from(n) * u128::from(delta);
            if at + run >= want && delta > 0 {
                // the first sample of this run that starts at or after it
                return (index + (want - at).div_ceil(u128::from(delta))).min(u128::from(u32::MAX)) as u32;
            }
            at += run;
            index += u128::from(n);
        }
        index.min(u128::from(u32::MAX)) as u32
    }

    /// Sample `index` (0-based): where it is, by its chunk, and when.
    fn sample(&self, index: u32) -> Option<Sample> {
        let size = self.sizes.of(index)?;
        if size == 0 || size > MAX_SAMPLE {
            return None;
        }
        // the chunk holding it, and the chunk's first sample
        let mut first_sample = 0u64;
        let mut found = None;
        for (i, &(first_chunk, per_chunk)) in self.stsc.iter().enumerate() {
            let next_chunk = self.stsc.get(i + 1).map_or(self.chunks.len() as u64 + 1, |&(c, _)| u64::from(c));
            let chunks_in_run = next_chunk.checked_sub(u64::from(first_chunk))?;
            let in_run = chunks_in_run.checked_mul(u64::from(per_chunk))?;
            if u64::from(index) < first_sample + in_run {
                let into = u64::from(index) - first_sample;
                let chunk = u64::from(first_chunk) + into / u64::from(per_chunk);
                found = Some((chunk, first_sample + into / u64::from(per_chunk) * u64::from(per_chunk)));
                break;
            }
            first_sample += in_run;
        }
        let (chunk, chunk_first) = found?;
        let mut offset = *self.chunks.get(usize::try_from(chunk.checked_sub(1)?).ok()?)?;
        for s in chunk_first..u64::from(index) {
            offset = offset.checked_add(u64::from(self.sizes.of(s as u32)?))?;
        }
        // its decode time
        let (mut left, mut ticks) = (u64::from(index), 0u64);
        for &(n, delta) in &self.stts {
            let take = left.min(u64::from(n));
            ticks = ticks.saturating_add(take * u64::from(delta));
            left -= take;
            if left == 0 {
                break;
            }
        }
        let millis = (u128::from(ticks) * 1000 / u128::from(self.timescale.max(1))) as i64;
        Some(Sample { offset, size, millis })
    }
}

/// The codec of a visual sample entry we can decode, and its decoder configuration box's type.
fn entry_codec(kind: &[u8; 4]) -> Option<(Codec, &'static [u8; 4])> {
    match kind {
        b"hvc1" | b"hev1" => Some((Codec::Hevc, b"hvcC")),
        b"avc1" | b"avc3" => Some((Codec::Avc, b"avcC")),
        _ => None,
    }
}

/// The first video track of a whole `moov` body that holds HEVC or H.264, with its sample tables.
pub fn video_track(moov: &[u8]) -> Option<Mp4Video> {
    children(moov).filter(|(k, _)| k == b"trak").find_map(|(_, trak)| trak_video(trak))
}

fn trak_video(trak: &[u8]) -> Option<Mp4Video> {
    let mdia = child(trak, b"mdia")?;
    let hdlr = child(mdia, b"hdlr")?;
    if hdlr.get(8..12)? != b"vide" {
        return None;
    }
    let mdhd = child(mdia, b"mdhd")?;
    let timescale = if mdhd.first()? == &1 { be32(mdhd, 20)? } else { be32(mdhd, 12)? };
    let stbl = child(child(mdia, b"minf")?, b"stbl")?;
    // the first sample entry: a visual one's own fields are 78 bytes, before its boxes
    let stsd = child(stbl, b"stsd")?;
    let (kind, entry) = children(stsd.get(8..)?).next()?;
    let (codec, config) = entry_codec(&kind)?;
    let private = child(entry.get(78..)?, config)?.to_vec();
    let tkhd = child(trak, b"tkhd")?;
    // width and height are the box's last 8 bytes, 16.16 fixed point
    let display = tkhd.len().checked_sub(8).and_then(|at| {
        let (w, h) = (be32(tkhd, at)? >> 16, be32(tkhd, at + 4)? >> 16);
        (w > 0 && h > 0).then_some((u64::from(w), u64::from(h)))
    });

    let (_, entries) = table(child(stbl, b"stts")?, 0, 8)?;
    let stts: Vec<(u32, u32)> =
        entries.as_chunks::<8>().0.iter().map(|e| (be32(e, 0).unwrap_or(0), be32(e, 4).unwrap_or(0))).collect();
    let stss = match child(stbl, b"stss") {
        Some(b) => Some(table(b, 0, 4)?.1.as_chunks::<4>().0.iter().filter_map(|e| be32(e, 0)).collect::<Vec<_>>()),
        None => None,
    };
    let stsz = child(stbl, b"stsz")?;
    let (fixed, count) = (be32(stsz, 4)?, be32(stsz, 8)?);
    if count > MAX_SAMPLES {
        return None;
    }
    let sizes = if fixed > 0 {
        Sizes::Fixed(fixed, count)
    } else {
        Sizes::Each(table(stsz, 4, 4)?.1.as_chunks::<4>().0.iter().filter_map(|e| be32(e, 0)).collect())
    };
    let (_, entries) = table(child(stbl, b"stsc")?, 0, 12)?;
    let stsc: Vec<(u32, u32)> =
        entries.as_chunks::<12>().0.iter().map(|e| (be32(e, 0).unwrap_or(0), be32(e, 4).unwrap_or(0))).collect();
    // runs in order, each from chunk 1 on, none empty
    if stsc.first().is_none_or(|r| r.0 != 1)
        || stsc.windows(2).any(|w| w[1].0 <= w[0].0)
        || stsc.iter().any(|r| r.1 == 0)
    {
        return None;
    }
    let chunks: Vec<u64> = if let Some(stco) = child(stbl, b"stco") {
        table(stco, 0, 4)?.1.as_chunks::<4>().0.iter().filter_map(|e| be32(e, 0).map(u64::from)).collect()
    } else {
        table(child(stbl, b"co64")?, 0, 8)?.1.as_chunks::<8>().0.iter().filter_map(|e| be64(e, 0)).collect()
    };
    Some(Mp4Video { codec, private, display, timescale, stts, stss, sizes, stsc, chunks })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn mp4_box(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut b = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        b.extend_from_slice(kind);
        b.extend_from_slice(body);
        b
    }

    fn full(entries: &[&[u32]], skip: &[u32]) -> Vec<u8> {
        let mut b = vec![0u8; 4];
        for s in skip {
            b.extend(s.to_be_bytes());
        }
        b.extend((entries.len() as u32).to_be_bytes());
        for e in entries {
            for v in *e {
                b.extend(v.to_be_bytes());
            }
        }
        b
    }

    /// A `moov` with one video track of samples `sizes`, `per_chunk` to a chunk from `first_at`, sync at `syncs`.
    pub(crate) fn moov(
        entry: &[u8; 4],
        config: &[u8],
        sizes: &[u32],
        per_chunk: u32,
        first_at: u32,
        syncs: &[u32],
    ) -> Vec<u8> {
        let mut visual = vec![0u8; 78];
        visual[24..26].copy_from_slice(&320u16.to_be_bytes());
        visual[26..28].copy_from_slice(&180u16.to_be_bytes());
        let config_kind = if entry.starts_with(b"hv") || entry.starts_with(b"he") { b"hvcC" } else { b"avcC" };
        visual.extend(mp4_box(config_kind, config));
        let mut stsd = vec![0u8; 4];
        stsd.extend(1u32.to_be_bytes());
        stsd.extend(mp4_box(entry, &visual));
        let size_rows: Vec<[u32; 1]> = sizes.iter().map(|s| [*s]).collect();
        let size_refs: Vec<&[u32]> = size_rows.iter().map(|r| r.as_slice()).collect();
        let mut chunk_offsets = Vec::new();
        let mut at = first_at;
        for chunk in sizes.chunks(per_chunk as usize) {
            chunk_offsets.push([at]);
            at += chunk.iter().sum::<u32>();
        }
        let chunk_refs: Vec<&[u32]> = chunk_offsets.iter().map(|r| r.as_slice()).collect();
        let sync_rows: Vec<[u32; 1]> = syncs.iter().map(|s| [*s]).collect();
        let sync_refs: Vec<&[u32]> = sync_rows.iter().map(|r| r.as_slice()).collect();
        let stbl = [
            mp4_box(b"stsd", &stsd),
            mp4_box(b"stts", &full(&[&[sizes.len() as u32, 1001]], &[])),
            mp4_box(b"stss", &full(&sync_refs, &[])),
            mp4_box(b"stsz", &full(&size_refs, &[0])),
            mp4_box(b"stsc", &full(&[&[1, per_chunk, 1]], &[])),
            mp4_box(b"stco", &full(&chunk_refs, &[])),
        ]
        .concat();
        let mut mdhd = vec![0u8; 24];
        mdhd[12..16].copy_from_slice(&24000u32.to_be_bytes());
        let mut hdlr = vec![0u8; 24];
        hdlr[8..12].copy_from_slice(b"vide");
        let mdia =
            [mp4_box(b"mdhd", &mdhd), mp4_box(b"hdlr", &hdlr), mp4_box(b"minf", &mp4_box(b"stbl", &stbl))].concat();
        let mut tkhd = vec![0u8; 84];
        tkhd[76..80].copy_from_slice(&(320u32 << 16).to_be_bytes());
        tkhd[80..84].copy_from_slice(&(180u32 << 16).to_be_bytes());
        // a sound track first: the video one is still found
        let mut sound_hdlr = vec![0u8; 24];
        sound_hdlr[8..12].copy_from_slice(b"soun");
        let sound = mp4_box(b"trak", &mp4_box(b"mdia", &mp4_box(b"hdlr", &sound_hdlr)));
        let trak = mp4_box(b"trak", &[mp4_box(b"tkhd", &tkhd), mp4_box(b"mdia", &mdia)].concat());
        mp4_box(b"moov", &[sound, trak].concat())
    }

    #[test]
    fn keyframes_are_found_by_their_chunk_and_time() {
        let sizes: Vec<u32> = (0..100).map(|i| if i % 10 == 0 { 1000 } else { 100 + i }).collect();
        let syncs: Vec<u32> = (0..10).map(|i| i * 10 + 1).collect();
        let moov = moov(b"hvc1", &[1, 2, 3], &sizes, 3, 5000, &syncs);
        let (_, _, head) = box_head(&moov).unwrap();
        let v = video_track(&moov[head..]).unwrap();
        assert_eq!((&v.codec, v.private.as_slice(), v.display), (&Codec::Hevc, &[1u8, 2, 3][..], Some((320, 180))));
        // 25% is sample 25: the next sync is sample 31 (index 30), in chunk 11 at its start
        let s = v.keyframe_at(25).unwrap();
        let before: u32 = sizes[..30].iter().sum();
        assert_eq!(s, Sample { offset: 5000 + u64::from(before), size: 1000, millis: 30 * 1001 * 1000 / 24000 });
        // 50%: index 50, and index 51 mid-chunk lands after the samples before it in its chunk
        assert_eq!(v.keyframe_at(50).unwrap().offset, 5000 + u64::from(sizes[..50].iter().sum::<u32>()));
        assert_eq!(v.sample(52).unwrap().offset, 5000 + u64::from(sizes[..52].iter().sum::<u32>()));
        // past the last sync: the last one
        assert_eq!(v.keyframe_at(99).unwrap().offset, 5000 + u64::from(sizes[..90].iter().sum::<u32>()));
    }

    #[test]
    fn the_moov_is_found_before_or_after_the_media() {
        let ftyp = mp4_box(b"ftyp", b"isom\0\0\0\0");
        let moov = mp4_box(b"moov", &[0u8; 100]);
        let front = [ftyp.clone(), moov.clone(), mp4_box(b"mdat", &[0u8; 50])].concat();
        assert_eq!(find_moov(&front, 0), Moov::At(ftyp.len() as u64, moov.len() as u64));
        // the media first, bigger than what was read: the next box is past it
        let mut mdat_head = 1_000_000u32.to_be_bytes().to_vec();
        mdat_head.extend(b"mdat");
        let front = [ftyp.clone(), mdat_head, vec![0u8; 64]].concat();
        assert_eq!(find_moov(&front, 0), Moov::After(ftyp.len() as u64 + 1_000_000));
        // read there, the moov is at the start
        assert_eq!(find_moov(&moov[..20], 1_000_016), Moov::At(1_000_016, moov.len() as u64));
        assert_eq!(find_moov(&[0, 0, 0, 3, b'b', b'a', b'd', b'!', 0, 0, 0, 0, 0, 0, 0, 0], 0), Moov::None);
    }

    #[test]
    fn bad_tables_are_refused() {
        let moov = moov(b"avc1", &[1], &[10, 20, 30], 1, 0, &[1]);
        let (_, _, head) = box_head(&moov).unwrap();
        let body = &moov[head..];
        assert!(video_track(body).is_some());
        // a cut-off moov reads as nothing
        assert!(video_track(&body[..body.len() - 6]).is_none());
        // an entry this doesn't decode
        let other = super::tests::moov(b"mp4v", &[1], &[10], 1, 0, &[1]);
        assert!(video_track(&other[8..]).is_none());
    }

    #[test]
    fn a_variable_frame_rate_track_is_cut_by_time() {
        let sizes = vec![100u32; 100];
        let syncs: Vec<u32> = (1..=100).collect();
        let moov = moov(b"hvc1", &[1], &sizes, 1, 0, &syncs);
        let (_, _, head) = box_head(&moov).unwrap();
        let mut v = video_track(&moov[head..]).unwrap();
        // 50 samples of 1 s, then 50 of 9 s: 500 s in all, soo 25% is 125 s, in the slow run
        v.timescale = 1000;
        v.stts = vec![(50, 1000), (50, 9000)];
        let s = v.keyframe_at(25).unwrap();
        assert_eq!(s.millis, 50_000 + 9_000 * 9, "the first sample at or after 125 s starts at 131 s");
        assert_eq!(v.keyframe_at(0).unwrap().millis, 0);
        assert_eq!(v.keyframe_at(100).unwrap().millis, 50_000 + 49 * 9_000, "the last one");
    }
}
