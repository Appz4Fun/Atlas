//! Tracks and format from the start of a media file: Matroska, WebM and MP4. All total functions: bad or cut
//! off bytes give what was read before them.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MediaInfo {
    pub container: String,
    pub muxing_app: Option<String>,
    pub writing_app: Option<String>,
    pub duration_secs: Option<f64>,
    pub video: Vec<VideoTrack>,
    pub audio: Vec<AudioTrack>,
    pub subtitles: Vec<SubtitleTrack>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct VideoTrack {
    pub codec: String,
    pub width: Option<u64>,
    pub height: Option<u64>,
    pub display_width: Option<u64>,
    pub display_height: Option<u64>,
    pub frame_rate: Option<f64>,
    pub bit_depth: Option<u64>,
    /// `HDR10`, `HLG`, `PQ`
    pub hdr: Option<String>,
    pub primaries: Option<u64>,
    pub transfer: Option<u64>,
    pub dv_profile: Option<u8>,
    pub dv_level: Option<u8>,
    pub dv_enhancement_layer: Option<bool>,
    pub language: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AudioTrack {
    pub codec: String,
    pub channels: Option<u64>,
    pub sample_rate: Option<f64>,
    pub language: Option<String>,
    pub name: Option<String>,
    /// from the track's name: the bitstream's flag sits past what's read
    pub atmos: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SubtitleTrack {
    pub codec: String,
    pub language: Option<String>,
    pub name: Option<String>,
    pub forced: bool,
    pub hearing_impaired: bool,
}

const EBML_MAGIC: &[u8] = &[0x1a, 0x45, 0xdf, 0xa3];

pub fn is_matroska(data: &[u8]) -> bool {
    data.starts_with(EBML_MAGIC)
}

pub fn is_mp4(data: &[u8]) -> bool {
    data.get(4..8) == Some(b"ftyp")
}

pub fn parse(data: &[u8]) -> Option<MediaInfo> {
    if is_matroska(data) {
        Some(matroska(data))
    } else if is_mp4(data) {
        Some(mp4(data))
    } else {
        None
    }
}

// ---- EBML

fn element_id(b: &[u8]) -> Option<(u32, usize)> {
    let first = *b.first()?;
    let len = first.leading_zeros() as usize + 1;
    if len > 4 {
        return None;
    }
    let mut id = 0u32;
    for byte in b.get(..len)? {
        id = id << 8 | u32::from(*byte);
    }
    Some((id, len))
}

/// The size, or None for "unknown", and its length.
fn element_size(b: &[u8]) -> Option<(Option<u64>, usize)> {
    let first = *b.first()?;
    let len = first.leading_zeros() as usize + 1;
    if len > 8 {
        return None;
    }
    let mut size = u64::from(first) & ((1u64 << (8 - len)) - 1);
    let mut all_ones = size == (1u64 << (8 - len)) - 1;
    for byte in b.get(1..len)? {
        all_ones &= *byte == 0xff;
        size = size << 8 | u64::from(*byte);
    }
    Some((if all_ones { None } else { Some(size) }, len))
}

/// Calls `each` with every element in `b`. One running past the end gets what's there, and ends the walk.
fn walk(b: &[u8], each: &mut dyn FnMut(u32, &[u8]) -> bool) {
    let mut at = 0usize;
    while at < b.len() {
        let Some((id, id_len)) = b.get(at..).and_then(element_id) else { return };
        let Some((size, size_len)) = b.get(at + id_len..).and_then(element_size) else { return };
        let start = at + id_len + size_len;
        let end = size.map_or(b.len(), |s| start.saturating_add(s as usize)).min(b.len());
        let Some(body) = b.get(start..end) else { return };
        if !each(id, body) {
            return;
        }
        at = end;
    }
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
}

fn text(b: &[u8]) -> String {
    let whole = String::from_utf8_lossy(b.get(..MAX_TRACK_TEXT * 4).unwrap_or(b));
    whole.trim_end_matches('\0').trim().chars().take(MAX_TRACK_TEXT).collect()
}

#[derive(Default)]
struct Track {
    kind: u64,
    codec: String,
    language: Option<String>,
    name: Option<String>,
    forced: bool,
    hearing_impaired: bool,
    default_duration: Option<u64>,
    width: Option<u64>,
    height: Option<u64>,
    display_width: Option<u64>,
    display_height: Option<u64>,
    bit_depth: Option<u64>,
    transfer: Option<u64>,
    primaries: Option<u64>,
    mastering: bool,
    dv: Option<(u8, u8, bool)>,
    channels: Option<u64>,
    sample_rate: Option<f64>,
}

/// A Dolby Vision configuration record: profile, level, enhancement layer.
fn dv_record(b: &[u8]) -> Option<(u8, u8, bool)> {
    let (a, c) = (*b.get(2)?, *b.get(3)?);
    Some((a >> 1, (a & 1) << 5 | c >> 3, c & 0b10 != 0))
}

fn hdr_name(transfer: Option<u64>, mastering: bool) -> Option<String> {
    match transfer {
        Some(16) if mastering => Some("HDR10".into()),
        Some(16) => Some("PQ".into()),
        Some(18) => Some("HLG".into()),
        _ => None,
    }
}

fn track_entry(b: &[u8]) -> Track {
    let mut t = Track::default();
    walk(b, &mut |id, v| {
        match id {
            0x83 => t.kind = uint(v),
            0x86 => t.codec = text(v),
            0x22B59C => t.language = t.language.take().or_else(|| Some(text(v))),
            // the newer tag says more
            0x22B59D => t.language = Some(text(v)),
            0x536E => t.name = Some(text(v)),
            0x55AA => t.forced = uint(v) != 0,
            0x55AB => t.hearing_impaired = uint(v) != 0,
            0x23E383 => t.default_duration = Some(uint(v)),
            0xE0 => walk(v, &mut |id, v| {
                match id {
                    0xB0 => t.width = Some(uint(v)),
                    0xBA => t.height = Some(uint(v)),
                    0x54B0 => t.display_width = Some(uint(v)),
                    0x54BA => t.display_height = Some(uint(v)),
                    0x55B0 => walk(v, &mut |id, v| {
                        match id {
                            0x55B2 => t.bit_depth = Some(uint(v)),
                            0x55BA => t.transfer = Some(uint(v)),
                            0x55BB => t.primaries = Some(uint(v)),
                            0x55D0 => t.mastering = true,
                            _ => {}
                        }
                        true
                    }),
                    _ => {}
                }
                true
            }),
            0xE1 => walk(v, &mut |id, v| {
                match id {
                    0xB5 => t.sample_rate = float(v),
                    0x9F => t.channels = Some(uint(v)),
                    _ => {}
                }
                true
            }),
            0x41E4 => {
                let (mut kind, mut extra) = (None, None);
                walk(v, &mut |id, v| {
                    match id {
                        0x41E7 => kind = Some(uint(v)),
                        0x41ED => extra = Some(v.to_vec()),
                        _ => {}
                    }
                    true
                });
                // the fourcc of dvcC or dvvC, as a number
                if matches!(kind, Some(0x6476_6343 | 0x6476_7643))
                    && let Some(extra) = extra
                {
                    t.dv = dv_record(&extra);
                }
            }
            _ => {}
        }
        true
    });
    t
}

fn matroska(data: &[u8]) -> MediaInfo {
    let mut info = MediaInfo { container: "matroska".into(), ..MediaInfo::default() };
    walk(data, &mut |id, body| {
        match id {
            0x1A45_DFA3 => walk(body, &mut |id, v| {
                if id == 0x4282 {
                    info.container = text(v);
                }
                true
            }),
            0x1853_8067 => segment(body, &mut info),
            _ => {}
        }
        true
    });
    info
}

fn segment(body: &[u8], info: &mut MediaInfo) {
    walk(body, &mut |id, v| {
        match id {
            0x1549_A966 => {
                let (mut scale, mut duration) = (1_000_000u64, None);
                walk(v, &mut |id, v| {
                    match id {
                        0x4D80 => info.muxing_app = Some(text(v)),
                        0x5741 => info.writing_app = Some(text(v)),
                        0x2AD7B1 => scale = uint(v),
                        0x4489 => duration = float(v),
                        _ => {}
                    }
                    true
                });
                info.duration_secs = duration.map(|d| d * scale as f64 / 1e9);
            }
            0x1654_AE6B => walk(v, &mut |id, v| {
                if id == 0xAE {
                    add_track(track_entry(v), info);
                }
                true
            }),
            // what's after the first cluster is the video itself
            0x1F43_B675 => return false,
            _ => {}
        }
        true
    });
}

/// The most tracks kept. Real files have a few dozen at most, and these come from Usenet.
const MAX_TRACKS: usize = 128;
/// The most characters kept of a track's codec, name or language.
const MAX_TRACK_TEXT: usize = 256;

fn is_full(info: &MediaInfo) -> bool {
    info.video.len() + info.audio.len() + info.subtitles.len() >= MAX_TRACKS
}

fn add_track(t: Track, info: &mut MediaInfo) {
    if is_full(info) {
        return;
    }
    match t.kind {
        1 => info.video.push(VideoTrack {
            hdr: hdr_name(t.transfer, t.mastering),
            codec: t.codec,
            width: t.width,
            height: t.height,
            display_width: t.display_width,
            display_height: t.display_height,
            frame_rate: t.default_duration.filter(|d| *d > 0).map(|d| 1e9 / d as f64),
            bit_depth: t.bit_depth,
            primaries: t.primaries,
            transfer: t.transfer,
            dv_profile: t.dv.map(|d| d.0),
            dv_level: t.dv.map(|d| d.1),
            dv_enhancement_layer: t.dv.map(|d| d.2),
            language: t.language,
        }),
        2 => info.audio.push(AudioTrack {
            atmos: t.name.as_deref().is_some_and(|n| n.to_ascii_lowercase().contains("atmos")),
            codec: t.codec,
            channels: t.channels,
            sample_rate: t.sample_rate,
            language: t.language,
            name: t.name,
        }),
        17 => info.subtitles.push(SubtitleTrack {
            forced: t.forced,
            hearing_impaired: t.hearing_impaired
                || t.name.as_deref().is_some_and(|n| n.to_ascii_lowercase().contains("sdh")),
            codec: t.codec,
            language: t.language,
            name: t.name,
        }),
        _ => {}
    }
}

// ---- MP4

fn boxes(b: &[u8], each: &mut dyn FnMut(&[u8; 4], &[u8])) {
    let mut at = 0usize;
    while let Some(head) = b.get(at..at + 8) {
        let mut size = u32::from_be_bytes([head[0], head[1], head[2], head[3]]) as usize;
        let kind: [u8; 4] = [head[4], head[5], head[6], head[7]];
        let mut start = at + 8;
        if size == 1 {
            let Some(long) = b.get(at + 8..at + 16) else { return };
            size = u64::from_be_bytes(long.try_into().unwrap_or([0; 8])) as usize;
            start = at + 16;
        } else if size == 0 {
            size = b.len() - at;
        }
        if size < start - at {
            return;
        }
        let end = at.saturating_add(size).min(b.len());
        each(&kind, b.get(start..end).unwrap_or(&[]));
        at = end;
    }
}

fn mp4(data: &[u8]) -> MediaInfo {
    let mut info = MediaInfo { container: "mp4".into(), ..MediaInfo::default() };
    boxes(data, &mut |kind, body| {
        if kind == b"moov" {
            moov(body, &mut info);
        }
    });
    info
}

fn be32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn be16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn moov(body: &[u8], info: &mut MediaInfo) {
    boxes(body, &mut |kind, v| match kind {
        b"mvhd" => {
            let (scale, duration) = if v.first() == Some(&1) {
                (be32(v, 20), v.get(24..32).map(|d| u64::from_be_bytes(d.try_into().unwrap_or([0; 8]))))
            } else {
                (be32(v, 12), be32(v, 16).map(u64::from))
            };
            if let (Some(scale), Some(duration)) = (scale, duration)
                && scale > 0
            {
                info.duration_secs = Some(duration as f64 / f64::from(scale));
            }
        }
        b"trak" => trak(v, info),
        _ => {}
    });
}

fn mp4_language(code: u16) -> Option<String> {
    let c = |shift: u16| char::from(((code >> shift) & 0x1f) as u8 + 0x60);
    let lang: String = [c(10), c(5), c(0)].iter().collect();
    (lang.chars().all(|ch| ch.is_ascii_lowercase()) && lang != "und").then_some(lang)
}

fn trak(body: &[u8], info: &mut MediaInfo) {
    if is_full(info) {
        return;
    }
    let (mut handler, mut language, mut entry): ([u8; 4], Option<String>, Option<Vec<u8>>) = ([0; 4], None, None);
    boxes(body, &mut |kind, v| {
        if kind == b"mdia" {
            boxes(v, &mut |kind, v| match kind {
                b"hdlr" => {
                    if let Some(h) = v.get(8..12) {
                        handler.copy_from_slice(h);
                    }
                }
                b"mdhd" => {
                    let at = if v.first() == Some(&1) { 32 } else { 20 };
                    language = be16(v, at).and_then(mp4_language);
                }
                b"minf" => boxes(v, &mut |kind, v| {
                    if kind == b"stbl" {
                        boxes(v, &mut |kind, v| {
                            if kind == b"stsd" {
                                entry = v.get(8..).map(<[u8]>::to_vec);
                            }
                        });
                    }
                }),
                _ => {}
            });
        }
    });
    let Some(entry) = entry else { return };
    let Some(fourcc) = entry.get(4..8) else { return };
    let codec = String::from_utf8_lossy(fourcc).to_string();
    match &handler {
        b"vide" => {
            let mut track = VideoTrack {
                codec,
                width: be16(&entry, 32).map(u64::from),
                height: be16(&entry, 34).map(u64::from),
                language,
                ..VideoTrack::default()
            };
            boxes(entry.get(86..).unwrap_or(&[]), &mut |kind, v| {
                if (kind == b"dvcC" || kind == b"dvvC")
                    && track.dv_profile.is_none()
                    && let Some((profile, level, el)) = dv_record(v)
                {
                    track.dv_profile = Some(profile);
                    track.dv_level = Some(level);
                    track.dv_enhancement_layer = Some(el);
                }
            });
            info.video.push(track);
        }
        b"soun" => info.audio.push(AudioTrack {
            codec,
            channels: be16(&entry, 24).map(u64::from),
            sample_rate: be32(&entry, 32).map(|rate| f64::from(rate >> 16)),
            language,
            ..AudioTrack::default()
        }),
        b"subt" | b"text" | b"sbtl" => {
            info.subtitles.push(SubtitleTrack { codec, language, ..SubtitleTrack::default() });
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn el(id: &[u8], body: &[u8]) -> Vec<u8> {
        let mut out = id.to_vec();
        assert!(body.len() < 0x3fff);
        out.extend([0x40 | (body.len() >> 8) as u8, body.len() as u8]);
        out.extend(body);
        out
    }

    fn sample_mkv() -> Vec<u8> {
        let colour = [el(&[0x55, 0xB2], &[10]), el(&[0x55, 0xBA], &[16]), el(&[0x55, 0xD0], &[])].concat();
        let video = [el(&[0xB0], &[0x0f, 0x00]), el(&[0xBA], &[0x08, 0x70]), el(&[0x55, 0xB0], &colour)].concat();
        let dv =
            [el(&[0x41, 0xE7], &[0x64, 0x76, 0x76, 0x43]), el(&[0x41, 0xED], &[1, 0, 8 << 1, 6 << 3 | 0b101])].concat();
        let t1 = [
            el(&[0x83], &[1]),
            el(&[0x86], b"V_MPEGH/ISO/HEVC"),
            el(&[0x23, 0xE3, 0x83], &40_000_000u32.to_be_bytes()),
            el(&[0xE0], &video),
            el(&[0x41, 0xE4], &dv),
        ]
        .concat();
        let audio = [el(&[0x9F], &[6]), el(&[0xB5], &48000f32.to_be_bytes())].concat();
        let t2 = [
            el(&[0x83], &[2]),
            el(&[0x86], b"A_EAC3"),
            el(&[0x22, 0xB5, 0x9C], b"eng"),
            el(&[0x53, 0x6E], b"DD+ 5.1 Atmos"),
            el(&[0xE1], &audio),
        ]
        .concat();
        let t3 =
            [el(&[0x83], &[17]), el(&[0x86], b"S_HDMV/PGS"), el(&[0x22, 0xB5, 0x9C], b"kor"), el(&[0x55, 0xAA], &[1])]
                .concat();
        let tracks = [el(&[0xAE], &t1), el(&[0xAE], &t2), el(&[0xAE], &t3)].concat();
        let info = [
            el(&[0x4D, 0x80], b"libebml"),
            el(&[0x57, 0x41], b"mkvmerge v80"),
            el(&[0x44, 0x89], &(7_200_000f64).to_be_bytes()),
        ]
        .concat();
        let segment = [
            el(&[0x15, 0x49, 0xA9, 0x66], &info),
            el(&[0x16, 0x54, 0xAE, 0x6B], &tracks),
            el(&[0x1F, 0x43, 0xB6, 0x75], &[0; 32]),
        ]
        .concat();
        [el(&[0x1A, 0x45, 0xDF, 0xA3], &el(&[0x42, 0x82], b"matroska")), el(&[0x18, 0x53, 0x80, 0x67], &segment)]
            .concat()
    }

    #[test]
    fn a_flood_of_tracks_is_cut_short() {
        let video = el(&[0x83], &[1]);
        let entry = el(&[0xAE], &video);
        let tracks = el(&[0x16, 0x54, 0xAE, 0x6B], &entry.repeat(MAX_TRACKS * 2));
        let segment = el(&[0x18, 0x53, 0x80, 0x67], &tracks);
        let data = [el(&[0x1A, 0x45, 0xDF, 0xA3], &el(&[0x42, 0x82], b"matroska")), segment].concat();
        assert_eq!(parse(&data).unwrap().video.len(), MAX_TRACKS);
        assert_eq!(text(&[b'a'; 10_000]).len(), MAX_TRACK_TEXT);
    }

    #[test]
    fn matroska_tracks_come_out() {
        let m = parse(&sample_mkv()).unwrap();
        assert_eq!(m.container, "matroska");
        assert_eq!(m.writing_app.as_deref(), Some("mkvmerge v80"));
        assert_eq!(m.duration_secs, Some(7200.0));
        let v = &m.video[0];
        assert_eq!((v.width, v.height, v.bit_depth), (Some(3840), Some(2160), Some(10)));
        assert_eq!(v.codec, "V_MPEGH/ISO/HEVC");
        assert_eq!(v.hdr.as_deref(), Some("HDR10"));
        assert_eq!(v.frame_rate, Some(25.0));
        assert_eq!((v.dv_profile, v.dv_level, v.dv_enhancement_layer), (Some(8), Some(6), Some(false)));
        let a = &m.audio[0];
        assert_eq!((a.codec.as_str(), a.channels, a.sample_rate), ("A_EAC3", Some(6), Some(48000.0)));
        assert!(a.atmos);
        assert_eq!(a.language.as_deref(), Some("eng"));
        assert!(m.subtitles[0].forced);
    }

    #[test]
    fn a_cut_off_file_gives_what_was_read() {
        let whole = sample_mkv();
        for cut in [0, 4, 10, 40, whole.len() / 2, whole.len() - 1] {
            let _ = parse(&whole[..cut]);
        }
        assert_eq!(parse(b"not a video"), None);
    }

    fn mp4_box(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut out = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        out.extend(kind);
        out.extend(body);
        out
    }

    #[test]
    fn mp4_tracks_come_out() {
        let mut entry = vec![0u8; 86];
        entry[4..8].copy_from_slice(b"hvc1");
        entry[32..34].copy_from_slice(&1920u16.to_be_bytes());
        entry[34..36].copy_from_slice(&1080u16.to_be_bytes());
        entry.extend(mp4_box(b"dvvC", &[1, 0, 7 << 1, 6 << 3 | 0b011]));
        let mut stsd = vec![0u8; 8];
        stsd.extend(&entry);
        let stbl = mp4_box(b"stbl", &mp4_box(b"stsd", &stsd));
        let mut hdlr = vec![0u8; 8];
        hdlr.extend(b"vide");
        hdlr.extend([0u8; 12]);
        let mdia = [mp4_box(b"hdlr", &hdlr), mp4_box(b"minf", &stbl)].concat();
        let mut mvhd = vec![0u8; 20];
        mvhd[12..16].copy_from_slice(&1000u32.to_be_bytes());
        mvhd[16..20].copy_from_slice(&60_000u32.to_be_bytes());
        let moov = [mp4_box(b"mvhd", &mvhd), mp4_box(b"trak", &mp4_box(b"mdia", &mdia))].concat();
        let file = [mp4_box(b"ftyp", b"isom"), mp4_box(b"moov", &moov)].concat();
        let m = parse(&file).unwrap();
        assert_eq!(m.container, "mp4");
        assert_eq!(m.duration_secs, Some(60.0));
        let v = &m.video[0];
        assert_eq!((v.codec.as_str(), v.width, v.height), ("hvc1", Some(1920), Some(1080)));
        assert_eq!((v.dv_profile, v.dv_enhancement_layer), (Some(7), Some(true)));
    }
}
