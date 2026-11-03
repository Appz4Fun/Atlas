//! Tracks and format from the start of a media file: Matroska, WebM and MP4. All total functions: bad or cut
//! off bytes give what was read before them.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MediaInfo {
    pub container: String,
    pub muxing_app: Option<String>,
    pub writing_app: Option<String>,
    /// the segment's own title, often the film's name or the release's
    pub title: Option<String>,
    pub duration_secs: Option<f64>,
    pub video: Vec<VideoTrack>,
    pub audio: Vec<AudioTrack>,
    pub subtitles: Vec<SubtitleTrack>,
    /// the first edition's chapters, in order
    pub chapters: Vec<Chapter>,
    /// when it was muxed, in Unix seconds
    pub muxed_at: Option<i64>,
    /// files attached to it: fonts, cover art
    pub attachments: Vec<Attachment>,
    /// segment UID from info (16 bytes hex)
    pub segment_uid: Option<String>,
    /// tags extracted from Tags element
    pub tags: HashMap<String, String>,
    /// cover art that was attached whole, kept apart from the JSON
    #[serde(skip)]
    pub cover: Option<Cover>,
    /// Everything before the video was read: the first cluster, or an MP4's `moov`, was reached. False means
    /// more of the file's start would say more.
    #[serde(skip)]
    pub header_complete: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Attachment {
    pub name: String,
    pub mime: Option<String>,
    /// as the file says: what was read may be less
    pub size: u64,
    pub description: Option<String>,
}

/// A whole JPEG or PNG attached to the file.
#[derive(Clone, Debug, PartialEq)]
pub struct Cover {
    pub mime: &'static str,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Chapter {
    pub start_secs: f64,
    pub name: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
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
    /// brightest pixel and brightest frame average, in nits
    pub max_cll: Option<u64>,
    pub max_fall: Option<u64>,
    pub dv_profile: Option<u8>,
    pub dv_level: Option<u8>,
    pub dv_enhancement_layer: Option<bool>,
    pub dolby_vision: Option<DolbyVisionInfo>,
    pub language: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioTrack {
    pub codec: String,
    pub channels: Option<u64>,
    pub sample_rate: Option<f64>,
    pub language: Option<String>,
    pub name: Option<String>,
    /// from the track's name: the bitstream's flag sits past what's read
    pub atmos: bool,
    /// the commentary flag, or a name that says so
    pub commentary: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SubtitleTrack {
    pub codec: String,
    pub language: Option<String>,
    pub name: Option<String>,
    pub forced: bool,
    pub hearing_impaired: bool,
    pub commentary: bool,
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

pub(crate) fn element_id(b: &[u8]) -> Option<(u32, usize)> {
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
pub(crate) fn element_size(b: &[u8]) -> Option<(Option<u64>, usize)> {
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
    walk_sized(b, &mut |id, body, _| each(id, body));
}

/// `walk`, also giving the size each element says it has: more than its body when it runs past the end.
fn walk_sized<'a>(b: &'a [u8], each: &mut dyn FnMut(u32, &'a [u8], Option<u64>) -> bool) {
    let mut at = 0usize;
    while at < b.len() {
        let Some((id, id_len)) = b.get(at..).and_then(element_id) else { return };
        let Some((size, size_len)) = b.get(at + id_len..).and_then(element_size) else { return };
        let start = at + id_len + size_len;
        let end = size.map_or(b.len(), |s| start.saturating_add(s as usize)).min(b.len());
        let Some(body) = b.get(start..end) else { return };
        if !each(id, body, size) {
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
    commentary: bool,
    default_duration: Option<u64>,
    width: Option<u64>,
    height: Option<u64>,
    display_width: Option<u64>,
    display_height: Option<u64>,
    bit_depth: Option<u64>,
    transfer: Option<u64>,
    primaries: Option<u64>,
    mastering: bool,
    max_cll: Option<u64>,
    max_fall: Option<u64>,
    dv: Option<(u8, u8, bool)>,
    dolby_vision: Option<DolbyVisionInfo>,
    channels: Option<u64>,
    sample_rate: Option<f64>,
    codec_private: Option<Vec<u8>>,
}

/// A Dolby Vision configuration record: profile, level, enhancement layer.
fn dv_record(b: &[u8]) -> Option<(u8, u8, bool)> {
    let (a, c) = (*b.get(2)?, *b.get(3)?);
    Some((a >> 1, (a & 1) << 5 | c >> 3, c & 0b10 != 0))
}

fn hdr_name(
    transfer: Option<u64>,
    mastering: bool,
    primaries: Option<u64>,
    bit_depth: Option<u64>,
) -> Option<String> {
    match transfer {
        Some(16) if mastering => Some("HDR10".into()),
        Some(16) if primaries == Some(9) && bit_depth.unwrap_or(0) >= 10 => Some("HDR10".into()),
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
            0x63A2 => t.codec_private = Some(v.to_vec()),
            0x22B59C => t.language = t.language.take().or_else(|| Some(text(v))),
            // the newer tag says more
            0x22B59D => t.language = Some(text(v)),
            0x536E => t.name = Some(text(v)),
            0x55AA => t.forced = uint(v) != 0,
            0x55AB => t.hearing_impaired = uint(v) != 0,
            0x55AF => t.commentary = uint(v) != 0,
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
                            0x55BC => t.max_cll = Some(uint(v)),
                            0x55BD => t.max_fall = Some(uint(v)),
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
                    t.dolby_vision = DolbyVisionInfo::from_dvcc_dvvc(&extra);
                }
            }
            _ => {}
        }
        true
    });

    if t.kind == 1 && let Some(cp) = &t.codec_private {
        if t.bit_depth.is_none() {
            if let Some(d) = HevcDepth::from_hvcc(cp) {
                t.bit_depth = Some(u64::from(d));
            } else if let Some(d) = AvcDepth::from_avcc(cp) {
                t.bit_depth = Some(u64::from(d));
            }
        }
        if let Some(sps) = extract_sps_from_hvcc(cp)
            && let Some(vui) = H265Sps::parse_vui(&sps)
        {
            if t.primaries.is_none() && vui.colour_primaries > 0 {
                t.primaries = Some(u64::from(vui.colour_primaries));
            }
            if t.transfer.is_none() && vui.transfer_characteristics > 0 {
                t.transfer = Some(u64::from(vui.transfer_characteristics));
            }
            if t.bit_depth.is_none() && vui.bit_depth > 0 {
                t.bit_depth = Some(u64::from(vui.bit_depth));
            }
        }
    }

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
                        0x7BA9 => info.title = Some(text(v)).filter(|t| !t.is_empty()),
                        0x5741 => info.writing_app = Some(text(v)),
                        0x2AD7B1 => scale = uint(v),
                        0x4489 => duration = float(v),
                        0x4461 => info.muxed_at = date_utc(v),
                        0x73A4 if v.len() == 16 => {
                            info.segment_uid = Some(v.iter().map(|b| format!("{b:02x}")).collect());
                        }
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
            0x1043_A770 if info.chapters.is_empty() => chapters(v, info),
            0x1941_A469 => attachments(v, info),
            0x1254_C367 => {
                walk(v, &mut |id, tag| {
                    if id == 0x7373 {
                        walk(tag, &mut |id, simple_tag| {
                            if id == 0x67C8 {
                                let mut name = None;
                                let mut string_val = None;
                                walk(simple_tag, &mut |id, val| {
                                    match id {
                                        0x45A3 => name = Some(text(val)),
                                        0x4487 => string_val = Some(text(val)),
                                        _ => {}
                                    }
                                    true
                                });
                                if let (Some(n), Some(s)) = (name, string_val)
                                    && !n.is_empty()
                                    && !s.is_empty()
                                {
                                    info.tags.insert(n, s);
                                }
                            }
                            true
                        });
                    }
                    true
                });
            }
            // what's after the first cluster is the video itself
            0x1F43_B675 => {
                info.header_complete = true;
                probe_cluster(v, info);
                return false;
            }
            _ => {}
        }
        true
    });
}

/// The most chapters kept. A Blu-ray has a few dozen; these come from Usenet.
const MAX_CHAPTERS: usize = 256;

/// Chapter extraction with hidden edition filtering (EditionFlagDefault / EditionFlagHidden).
fn chapters(body: &[u8], info: &mut MediaInfo) {
    let mut selected_chapters = Vec::new();
    let mut found_default = false;

    walk(body, &mut |id, edition| {
        if id != 0x45B9 {
            return true;
        }
        let mut is_hidden = false;
        let mut is_default = false;
        let mut candidate_chaps = Vec::new();

        walk(edition, &mut |id, atom| {
            match id {
                0x45BD => is_hidden = uint(atom) != 0,
                0x45DB => is_default = uint(atom) != 0,
                0xB6 if candidate_chaps.len() < MAX_CHAPTERS => {
                    let (mut start, mut chap_hidden, mut name) = (None, false, None);
                    walk(atom, &mut |id, v| {
                        match id {
                            0x91 => start = Some(uint(v)),
                            0x98 => chap_hidden = uint(v) != 0,
                            // the first display is the chapter's main name
                            0x80 if name.is_none() => walk(v, &mut |id, v| {
                                if id == 0x85 {
                                    name = Some(text(v)).filter(|n| !n.is_empty());
                                }
                                true
                            }),
                            _ => {}
                        }
                        true
                    });
                    if let (Some(start), false) = (start, chap_hidden) {
                        candidate_chaps.push(Chapter { start_secs: start as f64 / 1e9, name });
                    }
                }
                _ => {}
            }
            true
        });

        // Ignore hidden editions completely
        if is_hidden {
            return true;
        }

        if is_default && !found_default {
            selected_chapters = candidate_chaps;
            found_default = true;
        } else if selected_chapters.is_empty() {
            selected_chapters = candidate_chaps;
        }
        true
    });

    info.chapters = selected_chapters;
}

/// Matroska's dates count nanoseconds from 2001-01-01.
fn date_utc(b: &[u8]) -> Option<i64> {
    if b.len() != 8 {
        return None;
    }
    let ns = i64::from_be_bytes(b.try_into().ok()?);
    // a zero date is a muxer that didn't set one
    (ns != 0).then(|| 978_307_200 + ns.div_euclid(1_000_000_000))
}

/// The most attachments listed. Anime releases carry a few dozen fonts.
const MAX_ATTACHMENTS: usize = 64;
/// The most characters kept of an attachment's name, type or description: a list, not a store.
const MAX_ATTACHMENT_TEXT: usize = 96;

fn short(b: &[u8]) -> String {
    text(b).chars().take(MAX_ATTACHMENT_TEXT).collect()
}
/// The largest cover kept: covers are a few hundred KB, and each is stored with its release.
pub const MAX_COVER: usize = 512 * 1024;

/// Every attachment, and the cover art when it's here whole.
fn attachments(body: &[u8], info: &mut MediaInfo) {
    let mut best: Option<(u8, Cover)> = None;
    walk(body, &mut |id, file| {
        if id != 0x61A7 || info.attachments.len() >= MAX_ATTACHMENTS {
            return true;
        }
        let mut a = Attachment::default();
        let mut data: Option<(&[u8], bool)> = None;
        walk_sized(file, &mut |id, v, size| {
            match id {
                0x466E => a.name = short(v),
                0x4660 => a.mime = Some(short(v)).filter(|m| !m.is_empty()),
                0x467E => a.description = Some(short(v)).filter(|d| !d.is_empty()),
                0x465C => {
                    a.size = size.unwrap_or(v.len() as u64);
                    data = Some((v, size == Some(v.len() as u64)));
                }
                _ => {}
            }
            true
        });
        if let Some((bytes, true)) = data
            && let Some(rank) = cover_rank(&a.name)
            && best.as_ref().is_none_or(|(r, _)| rank < *r)
            && let Some(cover) = cover_of(bytes)
        {
            best = Some((rank, cover));
        }
        info.attachments.push(a);
        true
    });
    if let Some((_, cover)) = best {
        info.cover = Some(cover);
    }
}

/// How good a cover a name makes, best first: the Matroska names for cover art, then any image.
fn cover_rank(name: &str) -> Option<u8> {
    let lower = name.to_ascii_lowercase();
    let stem = lower.rsplit_once('.').map_or(lower.as_str(), |(stem, _)| stem);
    match stem {
        "cover" => Some(0),
        "cover_land" => Some(1),
        "small_cover" => Some(2),
        "small_cover_land" => Some(3),
        _ if [".jpg", ".jpeg", ".png"].iter().any(|ext| lower.ends_with(ext)) => Some(4),
        _ => None,
    }
}

/// A JPEG or PNG that is all there, by its own markers, and small enough to keep.
fn cover_of(bytes: &[u8]) -> Option<Cover> {
    if bytes.len() > MAX_COVER {
        return None;
    }
    let mime = if bytes.starts_with(&[0xff, 0xd8, 0xff]) && bytes.ends_with(&[0xff, 0xd9]) {
        "image/jpeg"
    } else if bytes.starts_with(b"\x89PNG\r\n\x1a\n")
        && bytes.len() >= 12
        && &bytes[bytes.len() - 8..bytes.len() - 4] == b"IEND"
    {
        "image/png"
    } else {
        return None;
    };
    Some(Cover { mime, bytes: bytes.to_vec() })
}

fn says_commentary(name: Option<&str>) -> bool {
    name.is_some_and(|n| n.to_ascii_lowercase().contains("commentary"))
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
        1 => {
            let mut dv_info = t.dolby_vision;
            if dv_info.is_none() && let Some((profile, level, el)) = t.dv {
                dv_info = Some(DolbyVisionInfo {
                    profile,
                    level: Some(level),
                    rpu_present: true,
                    el_present: el,
                    fel: profile == 7 && el,
                    cm_version: "v2.9".to_string(),
                });
            }
            info.video.push(VideoTrack {
                hdr: hdr_name(t.transfer, t.mastering, t.primaries, t.bit_depth),
                codec: t.codec,
                width: t.width,
                height: t.height,
                display_width: t.display_width,
                display_height: t.display_height,
                frame_rate: t.default_duration.filter(|d| *d > 0).map(|d| 1e9 / d as f64),
                bit_depth: t.bit_depth,
                primaries: t.primaries,
                transfer: t.transfer,
                max_cll: t.max_cll,
                max_fall: t.max_fall,
                dv_profile: t.dv.map(|d| d.0),
                dv_level: t.dv.map(|d| d.1),
                dv_enhancement_layer: t.dv.map(|d| d.2),
                dolby_vision: dv_info,
                language: t.language,
            })
        }
        2 => info.audio.push(AudioTrack {
            atmos: t.name.as_deref().is_some_and(|n| n.to_ascii_lowercase().contains("atmos")),
            commentary: t.commentary || says_commentary(t.name.as_deref()),
            codec: t.codec,
            channels: t.channels,
            sample_rate: t.sample_rate,
            language: t.language,
            name: t.name,
        }),
        17 => info.subtitles.push(SubtitleTrack {
            forced: t.forced,
            commentary: t.commentary || says_commentary(t.name.as_deref()),
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
    info.header_complete = has_whole_moov(data);

    let audio_probe = AudioBitstream::probe(data);
    if audio_probe.has_atmos && let Some(a) = info.audio.first_mut() {
        a.atmos = true;
    }
    if let Some(dv) = DolbyVisionInfo::parse_rpu(data) && let Some(v) = info.video.first_mut() {
        if v.dv_profile.is_none() {
            v.dv_profile = Some(dv.profile);
            v.dv_level = dv.level;
            v.dv_enhancement_layer = Some(dv.el_present);
        }
        v.dolby_vision = Some(dv);
    }

    info
}

/// Whether a top level `moov` is all there.
fn has_whole_moov(data: &[u8]) -> bool {
    let mut at = 0usize;
    while let Some(head) = data.get(at..at + 8) {
        let mut size = u64::from(u32::from_be_bytes([head[0], head[1], head[2], head[3]]));
        if size == 1 {
            let Some(long) = data.get(at + 8..at + 16) else { return false };
            size = u64::from_be_bytes(long.try_into().unwrap_or([0; 8]));
        } else if size == 0 {
            size = (data.len() - at) as u64;
        }
        if size < 8 {
            return false;
        }
        let Some(end) = usize::try_from(size).ok().and_then(|s| at.checked_add(s)) else { return false };
        if &head[4..8] == b"moov" {
            return end <= data.len();
        }
        at = end;
    }
    false
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
                    track.dolby_vision = DolbyVisionInfo::from_dvcc_dvvc(v);
                } else if kind == b"hvcC" {
                    if track.bit_depth.is_none() {
                        track.bit_depth = HevcDepth::from_hvcc(v).map(u64::from);
                    }
                    if let Some(sps) = extract_sps_from_hvcc(v)
                        && let Some(vui) = H265Sps::parse_vui(&sps)
                    {
                        if track.primaries.is_none() && vui.colour_primaries > 0 {
                            track.primaries = Some(u64::from(vui.colour_primaries));
                        }
                        if track.transfer.is_none() && vui.transfer_characteristics > 0 {
                            track.transfer = Some(u64::from(vui.transfer_characteristics));
                        }
                        if track.bit_depth.is_none() && vui.bit_depth > 0 {
                            track.bit_depth = Some(u64::from(vui.bit_depth));
                        }
                        if track.hdr.is_none() {
                            track.hdr = vui.hdr;
                        }
                    }
                } else if kind == b"avcC" && track.bit_depth.is_none() {
                    track.bit_depth = AvcDepth::from_avcc(v).map(u64::from);
                }
            });
            if track.hdr.is_none() {
                track.hdr = hdr_name(track.transfer, false, track.primaries, track.bit_depth);
            }
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

fn probe_cluster(data: &[u8], info: &mut MediaInfo) {
    let audio_probe = AudioBitstream::probe(data);
    if audio_probe.has_atmos && let Some(a) = info.audio.first_mut() {
        a.atmos = true;
    }

    if let Some(dv) = DolbyVisionInfo::parse_rpu(data) && let Some(v) = info.video.first_mut() {
        if v.dv_profile.is_none() {
            v.dv_profile = Some(dv.profile);
            v.dv_level = dv.level;
            v.dv_enhancement_layer = Some(dv.el_present);
        }
        v.dolby_vision = Some(dv);
    }
}

fn extract_sps_from_hvcc(hvcc: &[u8]) -> Option<Vec<u8>> {
    if hvcc.len() < 23 {
        return None;
    }
    let num_arrays = hvcc.get(22).copied()? as usize;
    let mut p = 23usize;
    for _ in 0..num_arrays {
        let nal_type = hvcc.get(p).copied()? & 0x3f;
        p = p.checked_add(1)?;
        let num_nalus = u16::from_be_bytes(hvcc.get(p..p + 2)?.try_into().ok()?) as usize;
        p = p.checked_add(2)?;
        for _ in 0..num_nalus {
            let nal_len = u16::from_be_bytes(hvcc.get(p..p + 2)?.try_into().ok()?) as usize;
            p = p.checked_add(2)?;
            let nal_bytes = hvcc.get(p..p + nal_len)?;
            p = p.checked_add(nal_len)?;
            if nal_type == 33 {
                return Some(nal_bytes.to_vec());
            }
        }
    }
    None
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|window| window == needle)
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DolbyVisionInfo {
    pub profile: u8,
    pub level: Option<u8>,
    pub rpu_present: bool,
    pub el_present: bool,
    pub fel: bool,
    pub cm_version: String,
}

impl DolbyVisionInfo {
    pub fn from_dvcc_dvvc(b: &[u8]) -> Option<Self> {
        let (a, c) = (*b.get(2)?, *b.get(3)?);
        let profile = a >> 1;
        let level = (a & 1) << 5 | c >> 3;
        let rpu_present = c & 0b100 != 0;
        let el_present = c & 0b010 != 0;
        Some(DolbyVisionInfo {
            profile,
            level: Some(level),
            rpu_present,
            el_present,
            fel: profile == 7 && el_present,
            cm_version: "v2.9".to_string(),
        })
    }

    pub fn parse_rpu(data: &[u8]) -> Option<Self> {
        if data.is_empty() {
            return None;
        }

        if data.len() >= 4 && (data[0] == 1 || data[0] == 0) && data[1] == 0 && (data[2] >> 1) <= 10
            && let Some(info) = Self::from_dvcc_dvvc(data)
            && [4, 5, 7, 8].contains(&info.profile)
        {
            return Some(info);
        }

        let nal_payload = Self::extract_rpu_nal(data)?;
        Self::parse_rpu_payload(nal_payload)
    }

    pub fn parse_unspec62(nal: &[u8]) -> Option<Self> {
        Self::parse_rpu(nal)
    }

    fn extract_rpu_nal(data: &[u8]) -> Option<&[u8]> {
        if data.len() >= 2 && ((data[0] >> 1) & 0x3f) == 62 {
            return Some(&data[2..]);
        }

        if data.len() >= 3 && data[0] == 0x19 {
            return Some(data);
        }

        let mut i = 0;
        while i + 4 < data.len() {
            let (start_len, offset) = if data[i..].starts_with(&[0, 0, 0, 1]) {
                (4, i + 4)
            } else if data[i..].starts_with(&[0, 0, 1]) {
                (3, i + 3)
            } else {
                i += 1;
                continue;
            };

            if offset < data.len() {
                let nal_type = (data[offset] >> 1) & 0x3f;
                if nal_type == 62 && offset + 2 <= data.len() {
                    let mut end = data.len();
                    for j in offset + 2..data.len().saturating_sub(3) {
                        if data[j..].starts_with(&[0, 0, 1]) || data[j..].starts_with(&[0, 0, 0, 1]) {
                            end = j;
                            break;
                        }
                    }
                    return Some(&data[offset + 2..end]);
                }
            }
            i += start_len;
        }

        None
    }

    fn parse_rpu_payload(payload: &[u8]) -> Option<Self> {
        let rbsp = crate::video::bits::rbsp(payload);
        if rbsp.len() < 3 {
            return None;
        }

        let mut r = crate::video::bits::BitReader::new(&rbsp);
        let _rpu_type = r.bits(6).ok()?;
        let rpu_format = r.bits(11).ok()?;
        let vdr_rpu_profile = r.bits(4).ok()? as u8;
        let vdr_rpu_level = r.bits(4).ok()? as u8;

        let bl_video_full_range_flag = false;
        let mut el_spatial_resampling_filter_flag = false;
        let mut disable_residual_flag = false;
        let mut vdr_bit_depth_minus8 = 2u32;

        let vdr_seq_info_present = r.flag().unwrap_or(false);
        if vdr_seq_info_present {
            let _chroma_resample = r.flag().ok()?;
            let _coeff_data_type = r.bits(2).ok()?;
            if (rpu_format & 0x700) == 0 {
                let _bl_depth = r.ue().ok()?;
                let _el_depth = r.ue().ok()?;
                vdr_bit_depth_minus8 = r.ue().unwrap_or(2);
            }
            let _spatial_resample = r.flag().ok()?;
            let _reserved = r.bits(3).ok()?;
            el_spatial_resampling_filter_flag = r.flag().unwrap_or(false);
            disable_residual_flag = r.flag().unwrap_or(false);
        }

        let profile = if vdr_rpu_profile == 0 && bl_video_full_range_flag {
            5
        } else if vdr_rpu_profile == 1 {
            if el_spatial_resampling_filter_flag && !disable_residual_flag {
                if vdr_bit_depth_minus8 == 4 { 7 } else { 4 }
            } else {
                8
            }
        } else if vdr_rpu_profile == 0 {
            5
        } else {
            vdr_rpu_profile
        };

        let el_present = profile == 7 || profile == 4;

        let mut is_fel = profile == 7;
        let has_nlq = (rpu_format & 0x700) == 0 && !disable_residual_flag;
        if has_nlq && profile == 7 {
            let is_mel = rbsp.windows(4).any(|w| w == [0, 0, 0, 0]) && rbsp.len() < 128;
            if is_mel {
                is_fel = false;
            }
        }

        let mut has_cm4 = false;

        // Skip the fixed 3-byte RPU header and parsed sequence info so that
        // Byte 2, which encodes (vdr_rpu_profile << 3) | (vdr_rpu_level >> 1) and
        // evaluates to 8..11 for standard Profile 4/7/8 streams, is not
        // falsely matched as a CM v4.0 extension block marker.
        let search_start = (r.position() / 8).max(3).min(rbsp.len());
        for &byte in &rbsp[search_start..] {
            if [3, 8, 9, 10, 11, 254].contains(&byte) {
                has_cm4 = true;
                break;
            }
        }

        let cm_version = if has_cm4 {
            "v4.0".to_string()
        } else {
            "v2.9".to_string()
        };

        Some(DolbyVisionInfo {
            profile,
            level: Some(vdr_rpu_level),
            rpu_present: true,
            el_present,
            fel: is_fel,
            cm_version,
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VuiInfo {
    pub colour_primaries: u8,
    pub transfer_characteristics: u8,
    pub matrix_coeffs: u8,
    pub bit_depth: u8,
    pub full_range: bool,
    pub hdr: Option<String>,
}

pub struct H265Sps;

impl H265Sps {
    pub fn parse_vui(nal: &[u8]) -> Option<VuiInfo> {
        let mut nal_slice = nal;
        if nal_slice.starts_with(&[0, 0, 0, 1]) {
            nal_slice = &nal_slice[4..];
        } else if nal_slice.starts_with(&[0, 0, 1]) {
            nal_slice = &nal_slice[3..];
        }
        if nal_slice.len() < 2 {
            return None;
        }

        if let Ok(sps) = crate::video::hevc::params::parse_sps(nal_slice) {
            let (primaries, transfer, matrix) = sps.colour.unwrap_or((0, 0, 0));
            let bit_depth = sps.bit_depth_luma;
            let hdr = if transfer == 16 && primaries == 9 && bit_depth >= 10 {
                Some("HDR10".to_string())
            } else if transfer == 18 && primaries == 9 {
                Some("HLG".to_string())
            } else if transfer == 16 {
                Some("PQ".to_string())
            } else if primaries == 1 && transfer == 1 {
                Some("BT.709".to_string())
            } else {
                None
            };
            return Some(VuiInfo {
                colour_primaries: primaries,
                transfer_characteristics: transfer,
                matrix_coeffs: matrix,
                bit_depth,
                full_range: sps.full_range,
                hdr,
            });
        }

        Self::parse_vui_direct(nal_slice)
    }

    fn parse_vui_direct(nal_slice: &[u8]) -> Option<VuiInfo> {
        let rbsp = crate::video::bits::rbsp(nal_slice.get(2..)?);
        if find_subsequence(&rbsp, &[9, 16, 9]).is_some() || find_subsequence(&rbsp, &[9, 16]).is_some() {
            return Some(VuiInfo {
                colour_primaries: 9,
                transfer_characteristics: 16,
                matrix_coeffs: 9,
                bit_depth: 10,
                full_range: false,
                hdr: Some("HDR10".to_string()),
            });
        }
        if find_subsequence(&rbsp, &[9, 18, 9]).is_some() || find_subsequence(&rbsp, &[9, 18]).is_some() {
            return Some(VuiInfo {
                colour_primaries: 9,
                transfer_characteristics: 18,
                matrix_coeffs: 9,
                bit_depth: 10,
                full_range: false,
                hdr: Some("HLG".to_string()),
            });
        }
        None
    }
}

pub struct HevcDepth;

impl HevcDepth {
    pub fn from_hvcc(hvcc: &[u8]) -> Option<u8> {
        if hvcc.len() >= 23 {
            let depth = (hvcc.get(17)? & 0x07) + 8;
            Some(depth)
        } else {
            None
        }
    }

    pub fn from_sps(sps: &[u8]) -> Option<u8> {
        H265Sps::parse_vui(sps).map(|v| v.bit_depth)
    }
}

pub struct AvcDepth;

impl AvcDepth {
    pub fn from_avcc(avcc: &[u8]) -> Option<u8> {
        if avcc.len() < 7 {
            return None;
        }
        let profile = *avcc.get(1)?;
        if profile == 110 || profile == 122 || profile == 244 {
            let depth = (avcc.get(6)? & 0x07) + 8;
            Some(depth)
        } else {
            Some(8)
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioProbeResult {
    pub has_atmos: bool,
    pub has_dtsx: bool,
    pub format: Option<String>,
}

pub struct AudioBitstream;

impl AudioBitstream {
    pub fn probe(data: &[u8]) -> AudioProbeResult {
        let mut result = AudioProbeResult::default();

        if let Some(pos) = find_subsequence(data, &[0xF8, 0x72, 0x6F, 0xBA])
            .or_else(|| find_subsequence(data, &[0xBA, 0x6F, 0x72, 0xF8]))
            && let Some(&b) = data.get(pos + 7)
            && b & 0x80 != 0
        {
            result.has_atmos = true;
            result.format = Some("TrueHD Atmos".to_string());
        }

        if !result.has_atmos && let Some(_pos) = find_subsequence(data, &[0x58, 0x38]) {
            result.has_atmos = true;
            result.format = Some("E-AC-3 Atmos".to_string());
        }

        if find_subsequence(data, &[0x02, 0x00, 0x08, 0x50]).is_some()
            || find_subsequence(data, &[0x50, 0x08, 0x00, 0x02]).is_some()
            || find_subsequence(data, &[0xF1, 0x40, 0x00, 0xD0]).is_some()
        {
            result.has_dtsx = true;
            if result.format.is_none() {
                result.format = Some("DTS:X".to_string());
            }
        }

        result
    }

    pub fn probe_truehd_atmos(data: &[u8]) -> bool {
        Self::probe(data).has_atmos
    }

    pub fn probe_dtsx(data: &[u8]) -> bool {
        Self::probe(data).has_dtsx
    }
}

pub struct Mp4Prober;

impl Mp4Prober {
    pub fn find_moov_offset(data: &[u8]) -> Option<u64> {
        let mut at = 0usize;
        while let Some(head) = data.get(at..at + 8) {
            let mut size = u64::from(u32::from_be_bytes([head[0], head[1], head[2], head[3]]));
            let kind = &head[4..8];
            let mut head_size = 8u64;
            if size == 1 {
                let long = data.get(at + 8..at + 16)?;
                size = u64::from_be_bytes(long.try_into().unwrap_or([0; 8]));
                head_size = 16;
            } else if size == 0 {
                return None;
            }
            if size < head_size {
                return None;
            }
            if kind == b"mdat" {
                return Some(at as u64 + size);
            }
            let end = at.checked_add(size as usize)?;
            at = end;
        }
        None
    }

    pub fn probe_tail_moov(data: &[u8]) -> Option<MediaInfo> {
        parse(data)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscInfo {
    pub disc_type: String,
    pub volume_id: Option<String>,
    pub dvd_disc_id: Option<String>,
    pub is_bluray: bool,
    pub is_dvd: bool,
}

pub struct DiscProber;

impl DiscProber {
    pub fn probe(data: &[u8]) -> Option<DiscInfo> {
        let mut info = DiscInfo::default();
        let mut recognized = false;

        if data.len() >= 0x8800
            && let Some(sector16) = data.get(0x8000..0x8800)
            && sector16.get(1..6) == Some(b"CD001")
        {
            recognized = true;
            info.disc_type = "ISO9660".into();
            if let Some(vol_id_bytes) = sector16.get(40..72) {
                let vol_id = String::from_utf8_lossy(vol_id_bytes).trim().to_string();
                if !vol_id.is_empty() {
                    info.volume_id = Some(vol_id);
                }
            }
        }

        if data.len() >= 0x80800
            && let Some(sector256) = data.get(0x80000..0x80800)
            && sector256.starts_with(&[0x02, 0x00])
        {
            recognized = true;
            if info.disc_type.is_empty() || info.disc_type == "ISO9660" {
                info.disc_type = "UDF".into();
            }
        }

        if find_subsequence(data, b"VIDEO_TS.IFO").is_some() || find_subsequence(data, b"video_ts.ifo").is_some() {
            info.is_dvd = true;
            info.disc_type = "DVD".into();
        }

        if find_subsequence(data, b"index.bdmv").is_some()
            || find_subsequence(data, b"INDEX.BDMV").is_some()
            || find_subsequence(data, b"BDMV").is_some()
        {
            info.is_bluray = true;
            info.disc_type = "Blu-ray".into();
        }

        if recognized || info.is_dvd || info.is_bluray {
            Some(info)
        } else {
            None
        }
    }

    pub fn dvd_disc_id(video_ts: &[u8], vts: &[u8]) -> [u8; 16] {
        use md5::{Digest, Md5};
        let mut hasher = Md5::new();
        let chunk1 = &video_ts[..video_ts.len().min(16384)];
        let chunk2 = &vts[..vts.len().min(16384)];
        hasher.update(chunk1);
        hasher.update(chunk2);
        let result = hasher.finalize();
        result.into()
    }

    pub fn dvd_disc_id_hex(video_ts: &[u8], vts: &[u8]) -> String {
        let hash = Self::dvd_disc_id(video_ts, vts);
        hash.iter().map(|b| format!("{b:02x}")).collect()
    }
}

pub struct MediaProber;

impl MediaProber {
    pub fn probe(data: &[u8]) -> Option<MediaInfo> {
        parse(data)
    }
}

pub struct MkvMetadata;

impl MkvMetadata {
    pub fn parse(data: &[u8]) -> Option<MediaInfo> {
        if is_matroska(data) {
            Some(matroska(data))
        } else {
            None
        }
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
        let t4 = [el(&[0x83], &[2]), el(&[0x86], b"A_AC3"), el(&[0x53, 0x6E], b"Director's Commentary")].concat();
        let t5 = [el(&[0x83], &[17]), el(&[0x86], b"S_TEXT/UTF8"), el(&[0x55, 0xAF], &[1])].concat();
        let tracks =
            [el(&[0xAE], &t1), el(&[0xAE], &t2), el(&[0xAE], &t3), el(&[0xAE], &t4), el(&[0xAE], &t5)].concat();
        let atom = |start_ns: u64, name: &[u8], hidden: bool| {
            let display = [el(&[0x85], name), el(&[0x43, 0x7C], b"eng")].concat();
            let mut body = [el(&[0x91], &start_ns.to_be_bytes()), el(&[0x80], &display)].concat();
            if hidden {
                body.extend(el(&[0x98], &[1]));
            }
            el(&[0xB6], &body)
        };
        let edition =
            [atom(0, b"Opening", false), atom(5_000_000_000, b"menu", true), atom(90_500_000_000, b"", false)].concat();
        let chapters = el(
            &[0x10, 0x43, 0xA7, 0x70],
            &[el(&[0x45, 0xB9], &edition), el(&[0x45, 0xB9], &atom(1, b"x", false))].concat(),
        );
        let info = [
            el(&[0x7B, 0xA9], b"The Film (2001)"),
            el(&[0x4D, 0x80], b"libebml"),
            el(&[0x57, 0x41], b"mkvmerge v80"),
            el(&[0x44, 0x89], &(7_200_000f64).to_be_bytes()),
        ]
        .concat();
        let segment = [
            el(&[0x15, 0x49, 0xA9, 0x66], &info),
            el(&[0x16, 0x54, 0xAE, 0x6B], &tracks),
            chapters,
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
        assert!(!a.commentary);
        assert_eq!(a.language.as_deref(), Some("eng"));
        assert!(m.audio[1].commentary, "named as one");
        assert!(m.subtitles[0].forced);
        assert!(m.subtitles[1].commentary, "flagged as one");
        assert_eq!(m.title.as_deref(), Some("The Film (2001)"));
        assert_eq!(
            m.chapters,
            [Chapter { start_secs: 0.0, name: Some("Opening".into()) }, Chapter { start_secs: 90.5, name: None },],
            "the hidden one and the second edition are left out"
        );
        assert!(m.header_complete);
    }

    #[test]
    fn a_header_cut_short_says_so() {
        let whole = sample_mkv();
        let cut = parse(&whole[..whole.len() - 40]).unwrap();
        assert!(!cut.header_complete);
        assert!(parse(&whole).unwrap().header_complete);
    }

    #[test]
    fn old_probe_json_still_reads() {
        let old = r#"{"container":"matroska","muxing_app":null,"writing_app":null,"duration_secs":1.0,
            "video":[],"audio":[{"codec":"A_AC3","channels":2,"sample_rate":null,"language":null,"name":null,"atmos":false}],
            "subtitles":[]}"#;
        let m: MediaInfo = serde_json::from_str(old).unwrap();
        assert!(m.chapters.is_empty());
        assert!(!m.audio[0].commentary);
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
        assert!(m.header_complete);
        assert!(!parse(&file[..file.len() - 4]).unwrap().header_complete);
        assert_eq!(m.container, "mp4");
        assert_eq!(m.duration_secs, Some(60.0));
        let v = &m.video[0];
        assert_eq!((v.codec.as_str(), v.width, v.height), ("hvc1", Some(1920), Some(1080)));
        assert_eq!((v.dv_profile, v.dv_enhancement_layer), (Some(7), Some(true)));
    }
    fn attached(name: &[u8], mime: &[u8], data: &[u8]) -> Vec<u8> {
        let body = [el(&[0x46, 0x6E], name), el(&[0x46, 0x60], mime), el(&[0x46, 0x5C], data)].concat();
        el(&[0x61, 0xA7], &body)
    }

    fn with_attachments(files: &[u8], cut: usize) -> Vec<u8> {
        let info = [el(&[0x44, 0x61], &(86_400i64 * 1_000_000_000).to_be_bytes())].concat();
        let segment = [el(&[0x15, 0x49, 0xA9, 0x66], &info), el(&[0x19, 0x41, 0xA4, 0x69], files)].concat();
        let mut data =
            [el(&[0x1A, 0x45, 0xDF, 0xA3], &el(&[0x42, 0x82], b"matroska")), el(&[0x18, 0x53, 0x80, 0x67], &segment)]
                .concat();
        data.truncate(data.len() - cut);
        data
    }

    #[test]
    fn attachments_are_listed_and_the_cover_kept() {
        let jpeg = [&[0xff, 0xd8, 0xff, 0xe0][..], &[7; 100], &[0xff, 0xd9]].concat();
        let small = [&[0xff, 0xd8, 0xff, 0xe0][..], &[1; 10], &[0xff, 0xd9]].concat();
        let files = [
            attached(b"Arial.ttf", b"font/ttf", &[0; 50]),
            attached(b"small_cover.jpg", b"image/jpeg", &small),
            attached(b"cover.jpg", b"image/jpeg", &jpeg),
        ]
        .concat();
        let m = parse(&with_attachments(&files, 0)).unwrap();
        let names: Vec<_> = m.attachments.iter().map(|a| (a.name.as_str(), a.size)).collect();
        assert_eq!(names, [("Arial.ttf", 50), ("small_cover.jpg", 16), ("cover.jpg", 106)]);
        assert_eq!(m.attachments[0].mime.as_deref(), Some("font/ttf"));
        assert_eq!(m.cover, Some(Cover { mime: "image/jpeg", bytes: jpeg.clone() }), "the better name wins");
        assert_eq!(m.muxed_at, Some(978_307_200 + 86_400));

        // cut off inside the cover: listed at its full size, but not kept
        let m = parse(&with_attachments(&attached(b"cover.jpg", b"image/jpeg", &jpeg), 10)).unwrap();
        assert_eq!(m.attachments[0].size, 106);
        assert_eq!(m.cover, None);
        let json = serde_json::to_string(&m).unwrap();
        assert!(!json.contains("cover\":{"), "the image stays out of the JSON: {json}");
    }

    #[test]
    fn a_flood_of_attachments_is_cut_short() {
        let long = vec![b'a'; 100];
        let files = attached(&long, &long, &[0; 4]).repeat(MAX_ATTACHMENTS + 2);
        let m = parse(&with_attachments(&files, 0)).unwrap();
        assert_eq!(m.attachments.len(), MAX_ATTACHMENTS);
        assert_eq!(m.attachments[0].name.len(), MAX_ATTACHMENT_TEXT);
        assert_eq!(m.attachments[0].mime.as_ref().unwrap().len(), MAX_ATTACHMENT_TEXT);
    }

    #[test]
    fn only_whole_small_images_make_a_cover() {
        let png = [&b"\x89PNG\r\n\x1a\n"[..], &[0; 20], b"\0\0\0\0IEND\xae\x42\x60\x82"].concat();
        assert_eq!(cover_of(&png).map(|c| c.mime), Some("image/png"));
        assert_eq!(cover_of(&png[..png.len() - 1]), None);
        assert_eq!(cover_of(&[0xff, 0xd8, 0xff, 0xe0, 0, 0]), None, "no end marker");
        let huge = [&[0xff, 0xd8, 0xff][..], &vec![0; MAX_COVER], &[0xff, 0xd9]].concat();
        assert_eq!(cover_of(&huge), None);
        assert_eq!(cover_rank("Cover.JPG"), Some(0));
        assert_eq!(cover_rank("font.otf"), None);
    }
}
