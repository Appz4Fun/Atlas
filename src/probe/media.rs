//! Tracks and format from the start of a media file: Matroska, WebM, MP4, AVI, ASF, MPEG program and transport
//! streams, DVD-Video title sets and Blu-ray playlists (see `avi`, `asf`, `mpegps`, `mpegts`, `dvd`, `bluray`). All total functions: bad or cut
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
    /// Matroska's SegmentUID, in hex: the same mux posted again has the same one
    pub segment_uid: Option<String>,
    /// how many chapter editions: more than one is often a seamless branching cut
    pub editions: u32,
    /// the encoder and its version, from the settings x264 and x265 write into the first frame
    pub encoder: Option<String>,
    /// those settings, as they're written
    pub encoder_options: Option<String>,
    /// files attached to it: fonts, cover art
    pub attachments: Vec<Attachment>,
    /// tags extracted from Tags element
    pub tags: HashMap<String, String>,
    /// cover art that was attached whole, kept apart from the JSON
    #[serde(skip)]
    pub cover: Option<Cover>,
    /// Everything before the video was read: the first cluster, or an MP4's `moov`, was reached. False means
    /// more of the file's start would say more.
    #[serde(skip)]
    pub header_complete: bool,
    /// The encoder's settings ran to their end in what was read, not to the end of the read.
    #[serde(skip)]
    pub encoder_complete: bool,
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
    /// The first keyframe's picture data, hashed: SHA-256 over its coded slices (no parameter sets, SEI or Dolby
    /// Vision NAL units, which muxers add and drop), first 8 bytes as 16 hex digits. A remux of the same video
    /// stream has the same; another encode doesn't. None unless that whole frame was in what was read.
    pub keyframe_key: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioTrack {
    pub codec: String,
    pub channels: Option<u64>,
    pub sample_rate: Option<f64>,
    pub language: Option<String>,
    pub name: Option<String>,
    /// from the track's name or bitstream probe
    pub atmos: bool,
    pub dtsx: bool,
    /// the commentary flag, or a name that says so
    pub commentary: bool,
    /// the player's pick when nothing else says
    pub default: bool,
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
    pub default: bool,
}

const EBML_MAGIC: &[u8] = &[0x1a, 0x45, 0xdf, 0xa3];

pub fn is_matroska(data: &[u8]) -> bool {
    data.starts_with(EBML_MAGIC)
}

pub fn is_mp4(data: &[u8]) -> bool {
    data.get(4..8) == Some(b"ftyp")
}

pub fn parse(data: &[u8]) -> Option<MediaInfo> {
    let mut info = if is_matroska(data) {
        matroska(data)
    } else if is_mp4(data) {
        mp4(data)
    } else if super::avi::is_avi(data) {
        super::avi::avi(data)
    } else if super::asf::is_asf(data) {
        super::asf::asf(data)
    } else if super::mpegps::is_mpeg_ps(data) {
        super::mpegps::mpeg_ps(data)
    } else if super::mpegts::is_mpeg_ts(data) {
        super::mpegts::mpeg_ts(data)
    } else if super::dvd::is_dvd(data) {
        super::dvd::dvd(data)
    } else if super::bluray::is_bluray(data) {
        super::bluray::bluray(data)
    } else {
        return None;
    };
    if let Some((encoder, options, complete)) = encoder_settings(data) {
        info.encoder = Some(encoder);
        info.encoder_options = options;
        info.encoder_complete = complete;
    }
    Some(info)
}

/// The most characters kept of an encoder's settings. x265's run to about 3000.
const MAX_ENCODER_OPTIONS: usize = 6000;

/// What x264 and x265 write as text into an SEI of the first frame: `x264 - core 164 r3095 baee400 - H.264/MPEG-4
/// AVC codec - ... - options: cabac=1 ref=4 ...`, or `x265 (build 199) - 3.5+1-f0c1022b6:[Linux][GCC 10.2.1][64
/// bit] 10bit - H.265/HEVC codec - ... - options: cpuid=1111039 ...`. The encoder with its version, and the options.
/// The encoder, its settings, and whether they ended before the data did: false when they may go on in the next article.
fn encoder_settings(data: &[u8]) -> Option<(String, Option<String>, bool)> {
    let (at, x265) = [(b"x264 - core ".as_slice(), false), (b"x265 (build ".as_slice(), true)]
        .into_iter()
        .find_map(|(needle, x265)| data.windows(needle.len()).position(|w| w == needle).map(|at| (at, x265)))?;
    let rest = &data[at..];
    let stop = rest.iter().position(|b| *b == 0 || !(b.is_ascii_graphic() || *b == b' '));
    let end = stop.unwrap_or(rest.len());
    let complete = stop.is_some() || end >= MAX_ENCODER_OPTIONS + 512;
    let line = String::from_utf8_lossy(&rest[..end.min(MAX_ENCODER_OPTIONS + 512)]).into_owned();
    let mut parts = line.split(" - ");
    parts.next()?;
    let encoder = if x265 {
        // `x265 (build 199)`, then `3.5+1-f0c1022b6:[Linux][GCC 10.2.1][64 bit] 10bit`
        let version = parts.next().and_then(|v| v.split(':').next()).unwrap_or_default();
        format!("x265 {}", version.trim())
    } else {
        // `x264 - core 164 r3095 baee400`
        let core = parts.next().unwrap_or_default();
        format!("x264 {}", core.trim())
    };
    let options = line
        .split_once("options: ")
        .map(|(_, o)| o.trim().chars().take(MAX_ENCODER_OPTIONS).collect::<String>())
        .filter(|o| !o.is_empty());
    Some((encoder.trim().to_string(), options, complete))
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
    number: Option<u64>,
    kind: u64,
    codec: String,
    language: Option<String>,
    name: Option<String>,
    forced: bool,
    hearing_impaired: bool,
    commentary: bool,
    /// None: not said, which means it is
    flag_default: Option<bool>,
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
        // mastering data says HDR10 even when the transfer wasn't written down
        None if mastering => Some("HDR10".into()),
        _ => None,
    }
}

fn track_entry(b: &[u8]) -> Track {
    let mut t = Track::default();
    walk(b, &mut |id, v| {
        match id {
            0xD7 => t.number = Some(uint(v)),
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
            0x88 => t.flag_default = Some(uint(v) != 0),
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
        let codec = t.codec.as_str();
        if codec.starts_with("V_MPEGH/ISO/HEVC") {
            if t.bit_depth.is_none()
                && let Some(d) = HevcDepth::from_hvcc(cp)
            {
                t.bit_depth = Some(u64::from(d));
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
        } else if codec.starts_with("V_MPEG4/ISO/AVC")
            && t.bit_depth.is_none()
            && let Some(d) = AvcDepth::from_avcc(cp)
        {
            t.bit_depth = Some(u64::from(d));
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
    let mut video_tracks: Vec<(u64, usize, usize)> = Vec::new();
    let mut audio_tracks: Vec<(u64, usize)> = Vec::new();
    let mut next_track_num = 1u64;
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
                    let t = track_entry(v);
                    let track_num = t.number.unwrap_or(next_track_num);
                    next_track_num = track_num.saturating_add(1);
                    if t.kind == 1 && !is_full(info) {
                        // lengthSizeMinusOne: byte 4 of an avcC record, byte 21 of an hvcC one
                        let at = if t.codec.contains("AVC") { 4 } else { 21 };
                        let nal_length_size = t
                            .codec_private
                            .as_deref()
                            .and_then(|cp| cp.get(at))
                            .map(|&b| usize::from(b & 0x03) + 1)
                            .unwrap_or(4);
                        video_tracks.push((track_num, info.video.len(), nal_length_size));
                    } else if t.kind == 2 && !is_full(info) {
                        audio_tracks.push((track_num, info.audio.len()));
                    }
                    add_track(t, info);
                }
                true
            }),
            0x1043_A770 if info.chapters.is_empty() => {
                info.editions = 0;
                walk(v, &mut |id, _| {
                    info.editions += u32::from(id == 0x45B9);
                    true
                });
                chapters(v, info);
            }
            0x1941_A469 => attachments(v, info),
            0x1254_C367 if info.tags.len() < MAX_TAGS => {
                walk(v, &mut |id, tag| {
                    if info.tags.len() >= MAX_TAGS {
                        return false;
                    }
                    if id == 0x7373 {
                        walk(tag, &mut |id, simple_tag| {
                            if info.tags.len() >= MAX_TAGS {
                                return false;
                            }
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
                                    && (info.tags.len() < MAX_TAGS || info.tags.contains_key(&n))
                                {
                                    info.tags.insert(n, s);
                                }
                            }
                            info.tags.len() < MAX_TAGS
                        });
                    }
                    info.tags.len() < MAX_TAGS
                });
            }
            // what's after the first cluster is the video itself
            0x1F43_B675 => {
                info.header_complete = true;
                probe_cluster(v, &video_tracks, &audio_tracks, info);
                return false;
            }
            _ => {}
        }
        true
    });
}

/// The most tags kept from Matroska `Tags` elements.
const MAX_TAGS: usize = 64;

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
pub(super) const MAX_TRACKS: usize = 128;
/// The most characters kept of a track's codec, name or language.
pub(super) const MAX_TRACK_TEXT: usize = 256;

pub(super) fn is_full(info: &MediaInfo) -> bool {
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
                    from_rpu: false,
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
                keyframe_key: None,
            })
        }
        2 => info.audio.push(AudioTrack {
            atmos: t.name.as_deref().is_some_and(|n| n.to_ascii_lowercase().contains("atmos")),
            dtsx: t.name.as_deref().is_some_and(|n| {
                let lower = n.to_ascii_lowercase();
                lower.contains("dts:x") || lower.contains("dts-x") || lower.contains("dtsx")
            }),
            commentary: t.commentary || says_commentary(t.name.as_deref()),
            default: t.flag_default.unwrap_or(true),
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
            default: t.flag_default.unwrap_or(true),
            codec: t.codec,
            language: t.language,
            name: t.name,
        }),
        _ => {}
    }
}

// ---- MP4

fn boxes<'a>(b: &'a [u8], each: &mut dyn FnMut(&[u8; 4], &'a [u8])) {
    let mut at = 0usize;
    while let Some(head_end) = at.checked_add(8)
        && let Some(head) = b.get(at..head_end)
    {
        let mut size = u32::from_be_bytes([head[0], head[1], head[2], head[3]]) as usize;
        let kind: [u8; 4] = [head[4], head[5], head[6], head[7]];
        let mut start = head_end;
        if size == 1 {
            let Some(ext_end) = at.checked_add(16) else { return };
            let Some(long) = b.get(head_end..ext_end) else { return };
            let Ok(long_size) = usize::try_from(u64::from_be_bytes(long.try_into().unwrap_or([0; 8]))) else {
                return;
            };
            size = long_size;
            start = ext_end;
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
            moov(body, data, &mut info);
        }
    });
    info.header_complete = has_whole_moov(data);

    if let Some(v) = info.video.first_mut()
        && v.dolby_vision.as_ref().is_none_or(|d| !d.rpu_present || (d.profile == 7 && !d.fel))
        && let Some(dv) = DolbyVisionInfo::parse_rpu(data)
    {
        apply_rpu_to_video_track(v, dv);
    }

    info
}

/// Whether a top level `moov` is all there.
fn has_whole_moov(data: &[u8]) -> bool {
    let mut at = 0usize;
    while let Some(head_end) = at.checked_add(8)
        && let Some(head) = data.get(at..head_end)
    {
        let mut size = u64::from(u32::from_be_bytes([head[0], head[1], head[2], head[3]]));
        let mut head_size = 8u64;
        if size == 1 {
            let Some(ext_end) = at.checked_add(16) else { return false };
            let Some(long) = data.get(head_end..ext_end) else { return false };
            size = u64::from_be_bytes(long.try_into().unwrap_or([0; 8]));
            head_size = 16;
        } else if size == 0 {
            size = (data.len() - at) as u64;
        }
        if size < head_size {
            return false;
        }
        let Some(end) = usize::try_from(size).ok().and_then(|s| at.checked_add(s)) else { return false };
        if &head[4..8] == b"moov" {
            return end <= data.len();
        }
        if end > data.len() {
            return false;
        }
        at = end;
    }
    false
}

fn be32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(at..at.checked_add(4)?)?.try_into().ok()?))
}

fn be16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(b.get(at..at.checked_add(2)?)?.try_into().ok()?))
}

fn collect_moov_chunk_offsets(moov_body: &[u8]) -> Vec<usize> {
    const MAX_CHUNK_OFFSETS: usize = 16;
    let mut all_offsets = Vec::new();
    let mut traks_seen = 0usize;
    boxes(moov_body, &mut |kind, trak_v| {
        if kind != b"trak" || traks_seen >= MAX_TRACKS {
            return;
        }
        traks_seen += 1;
        boxes(trak_v, &mut |kind, mdia_v| {
            if kind == b"mdia" {
                boxes(mdia_v, &mut |kind, minf_v| {
                    if kind == b"minf" {
                        boxes(minf_v, &mut |kind, stbl_v| {
                            if kind == b"stbl" {
                                let mut track_offsets = 0usize;
                                boxes(stbl_v, &mut |kind, v| match kind {
                                    b"stco" => {
                                        let count = be32(v, 4).unwrap_or(0).min(MAX_CHUNK_OFFSETS as u32) as usize;
                                        for i in 0..count {
                                            if track_offsets >= MAX_CHUNK_OFFSETS {
                                                break;
                                            }
                                            if let Some(off) = be32(v, 8 + i * 4).map(|o| o as usize) {
                                                all_offsets.push(off);
                                                track_offsets += 1;
                                            }
                                        }
                                    }
                                    b"co64" => {
                                        let count = be32(v, 4).unwrap_or(0).min(MAX_CHUNK_OFFSETS as u32) as usize;
                                        for i in 0..count {
                                            if track_offsets >= MAX_CHUNK_OFFSETS {
                                                break;
                                            }
                                            if let Some(bytes) = v.get(8 + i * 8..16 + i * 8)
                                                && let Ok(arr) = <[u8; 8]>::try_from(bytes)
                                                && let Ok(off) = usize::try_from(u64::from_be_bytes(arr))
                                            {
                                                all_offsets.push(off);
                                                track_offsets += 1;
                                            }
                                        }
                                    }
                                    _ => {}
                                });
                            }
                        });
                    }
                });
            }
        });
    });
    all_offsets.sort_unstable();
    all_offsets.dedup();
    all_offsets
}

/// The first sample's size, from `stsz` or `stz2`.
fn first_sample_size(stsz: Option<&[u8]>, stz2: Option<&[u8]>) -> Option<usize> {
    if let Some(v) = stsz {
        let default_size = be32(v, 4)? as usize;
        if be32(v, 8)? == 0 {
            return None;
        }
        return Some(if default_size > 0 { default_size } else { be32(v, 12)? as usize }).filter(|n| *n > 0);
    }
    let v = stz2?;
    if be32(v, 8)? == 0 {
        return None;
    }
    let size = match *v.get(7)? {
        8 => usize::from(*v.get(12)?),
        16 => usize::from(be16(v, 12)?),
        // 4-bit sizes are for tiny samples, never a keyframe
        _ => return None,
    };
    Some(size).filter(|n| *n > 0)
}

fn mp4_chunk_byte_len(
    chunk_idx_1based: u32,
    stsc: &[(u32, u32)],
    stsz: Option<&[u8]>,
    stz2: Option<&[u8]>,
    max_probe: usize,
) -> Option<usize> {
    if stsz.is_none() && stz2.is_none() {
        return None;
    }
    let chunk_idx = chunk_idx_1based.max(1);
    let (first_sample, samples_in_chunk) = if stsc.is_empty() {
        ((chunk_idx - 1) as usize, 1usize)
    } else {
        let samples_for_chunk = |c: u32| -> usize {
            stsc.iter()
                .rev()
                .find(|&&(first_chunk, _)| first_chunk <= c)
                .or_else(|| stsc.first())
                .map(|&(_, spc)| spc as usize)
                .unwrap_or(1)
        };
        let mut first = 0usize;
        for c in 1..chunk_idx {
            first = first.saturating_add(samples_for_chunk(c));
        }
        (first, samples_for_chunk(chunk_idx))
    };

    if samples_in_chunk == 0 {
        return Some(0);
    }

    if let Some(v) = stsz {
        let default_size = be32(v, 4)? as usize;
        let sample_count = be32(v, 8)? as usize;
        if default_size > 0 {
            if sample_count > 0 && first_sample >= sample_count {
                return Some(0);
            }
            let count = if sample_count > 0 {
                samples_in_chunk.min(sample_count - first_sample)
            } else {
                samples_in_chunk
            };
            return Some(count.saturating_mul(default_size).min(max_probe));
        }
        if first_sample >= sample_count {
            return Some(0);
        }
        let count = samples_in_chunk.min(sample_count - first_sample).min(256);
        let mut total = 0usize;
        for s in 0..count {
            let entry_off = 12usize.checked_add(first_sample.checked_add(s)?.checked_mul(4)?)?;
            let sz = be32(v, entry_off)? as usize;
            total = total.saturating_add(sz);
            if total >= max_probe {
                return Some(max_probe);
            }
        }
        return Some(total);
    }

    if let Some(v) = stz2 {
        let field_size = usize::from(*v.get(7)?);
        let sample_count = be32(v, 8)? as usize;
        if first_sample >= sample_count {
            return Some(0);
        }
        let count = samples_in_chunk.min(sample_count - first_sample).min(256);
        let mut total = 0usize;
        for s in 0..count {
            let idx = first_sample.checked_add(s)?;
            let sz = match field_size {
                4 => {
                    let b = *v.get(12 + idx / 2)?;
                    usize::from(if idx % 2 == 0 { b >> 4 } else { b & 0x0F })
                }
                8 => usize::from(*v.get(12 + idx)?),
                16 => usize::from(be16(v, 12 + idx.checked_mul(2)?)?),
                _ => return None,
            };
            total = total.saturating_add(sz);
            if total >= max_probe {
                return Some(max_probe);
            }
        }
        return Some(total);
    }

    None
}

fn moov(body: &[u8], full_data: &[u8], info: &mut MediaInfo) {
    let all_chunk_offsets = collect_moov_chunk_offsets(body);
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
        b"trak" => trak(v, full_data, &all_chunk_offsets, info),
        _ => {}
    });
}

fn mp4_language(code: u16) -> Option<String> {
    let c = |shift: u16| char::from(((code >> shift) & 0x1f) as u8 + 0x60);
    let lang: String = [c(10), c(5), c(0)].iter().collect();
    (lang.chars().all(|ch| ch.is_ascii_lowercase()) && lang != "und").then_some(lang)
}

/// Checks ETSI TS 102 366 Annex F `EC3SpecificBox` (`dec3`) for Dolby Atmos (JOC) extension flag.
fn parse_dec3_atmos(v: &[u8]) -> bool {
    if v.len() < 5 {
        return false;
    }
    let header = u16::from_be_bytes([v[0], v[1]]);
    let num_ind_sub = (header & 0x07) as usize;
    let mut pos = 2usize;
    for _ in 0..=num_ind_sub {
        let Some(sub) = v.get(pos..pos + 3) else { return false };
        let num_dep_sub = (sub[2] >> 1) & 0x0f;
        pos += if num_dep_sub > 0 { 4 } else { 3 };
    }
    let Some(&ext_flags) = v.get(pos) else { return false };
    (ext_flags & 0x01) != 0 && v.get(pos + 1).is_none_or(|&joc_objects| (1..=16).contains(&joc_objects))
}

/// Checks `MLPConfigBox` (`dmlp`) for TrueHD Atmos substream/channel configuration.
fn parse_dmlp_atmos(v: &[u8]) -> bool {
    if v.len() >= 10 {
        let substreams = (v[6] >> 4) & 0x0f;
        (v[6] & 0x80) != 0 || (substreams >= 1 && (v[7] & 0x01) != 0)
    } else {
        false
    }
}

fn trak(body: &[u8], full_data: &[u8], all_chunk_offsets: &[usize], info: &mut MediaInfo) {
    const MAX_CHUNK_OFFSETS: usize = 16;
    const MAX_AUDIO_CHUNK_PROBE: usize = 4096;

    if is_full(info) {
        return;
    }
    let (mut handler, mut language, mut entry): ([u8; 4], Option<String>, Option<Vec<u8>>) = ([0; 4], None, None);
    let mut chunk_offsets: Vec<(u32, usize)> = Vec::new();
    let mut stsc_entries: Vec<(u32, u32)> = Vec::new();
    let mut stsz_box: Option<&[u8]> = None;
    let mut stz2_box: Option<&[u8]> = None;
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
                        boxes(v, &mut |kind, v| match kind {
                            b"stsd" => {
                                entry = v.get(8..).map(<[u8]>::to_vec);
                            }
                            b"stco" => {
                                let count = be32(v, 4).unwrap_or(0).min(MAX_CHUNK_OFFSETS as u32) as usize;
                                for i in 0..count {
                                    if chunk_offsets.len() >= MAX_CHUNK_OFFSETS {
                                        break;
                                    }
                                    if let Some(off) = be32(v, 8 + i * 4).map(|o| o as usize)
                                        && !chunk_offsets.iter().any(|&(_, existing)| existing == off)
                                    {
                                        chunk_offsets.push((i as u32 + 1, off));
                                    }
                                }
                            }
                            b"co64" => {
                                let count = be32(v, 4).unwrap_or(0).min(MAX_CHUNK_OFFSETS as u32) as usize;
                                for i in 0..count {
                                    if chunk_offsets.len() >= MAX_CHUNK_OFFSETS {
                                        break;
                                    }
                                    if let Some(bytes) = v.get(8 + i * 8..16 + i * 8)
                                        && let Ok(arr) = <[u8; 8]>::try_from(bytes)
                                        && let Ok(off) = usize::try_from(u64::from_be_bytes(arr))
                                        && !chunk_offsets.iter().any(|&(_, existing)| existing == off)
                                    {
                                        chunk_offsets.push((i as u32 + 1, off));
                                    }
                                }
                            }
                            b"stsc" if stsc_entries.is_empty() => {
                                let count = be32(v, 4).unwrap_or(0).min(MAX_CHUNK_OFFSETS as u32) as usize;
                                for i in 0..count {
                                    let Some(first_chunk) = be32(v, 8 + i * 12) else { break };
                                    let Some(samples_per_chunk) = be32(v, 12 + i * 12) else { break };
                                    if first_chunk >= 1 {
                                        stsc_entries.push((first_chunk, samples_per_chunk));
                                    }
                                    if first_chunk > MAX_CHUNK_OFFSETS as u32 {
                                        break;
                                    }
                                }
                            }
                            b"stsz" if stsz_box.is_none() && v.len() >= 12 => {
                                stsz_box = Some(v);
                            }
                            b"stz2" if stz2_box.is_none() && v.len() >= 12 => {
                                stz2_box = Some(v);
                            }
                            _ => {}
                        });
                    }
                }),
                _ => {}
            });
        }
    });
    let chunk_slice = |chunk_idx: u32, off: usize| -> Option<&[u8]> {
        let mut end = off.saturating_add(MAX_AUDIO_CHUNK_PROBE).min(full_data.len());
        if let Some(chunk_len) =
            mp4_chunk_byte_len(chunk_idx, &stsc_entries, stsz_box, stz2_box, MAX_AUDIO_CHUNK_PROBE)
        {
            end = end.min(off.saturating_add(chunk_len));
        }
        if let Some(&next_off) = all_chunk_offsets.iter().find(|&&o| o > off) {
            end = end.min(next_off);
        }
        full_data.get(off..end)
    };
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
            let mut nal_length_size = 4usize;
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
                    if let Some(&len_byte) = v.get(21) {
                        nal_length_size = usize::from(len_byte & 0x03) + 1;
                    }
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
                } else if kind == b"avcC" {
                    if let Some(&len_byte) = v.get(4) {
                        nal_length_size = usize::from(len_byte & 0x03) + 1;
                    }
                    if track.bit_depth.is_none() {
                        track.bit_depth = AvcDepth::from_avcc(v).map(u64::from);
                    }
                }
            });
            for &(chunk_idx, off) in &chunk_offsets {
                let Some(chunk) = chunk_slice(chunk_idx, off) else { continue };
                if let Some(dv) = DolbyVisionInfo::parse_rpu_with_length_size(chunk, nal_length_size) {
                    apply_rpu_to_video_track(&mut track, dv);
                    break;
                }
            }
            // the first sample, the first keyframe of a playable track, starts the first chunk
            if let Some(&(_, off)) = chunk_offsets.iter().find(|&&(chunk, _)| chunk == 1)
                && let Some(size) = first_sample_size(stsz_box, stz2_box)
                && let Some(frame) = full_data.get(off..off.saturating_add(size))
                && frame.len() == size
            {
                track.keyframe_key = keyframe_key(&track.codec, frame, nal_length_size);
            }
            if track.hdr.is_none() {
                track.hdr = hdr_name(track.transfer, false, track.primaries, track.bit_depth);
            }
            info.video.push(track);
        }
        b"soun" => {
            let mut atmos = false;
            let mut dtsx = codec.eq_ignore_ascii_case("dtsx");
            let can_probe_dtsx = codec.is_empty() || codec.to_ascii_lowercase().contains("dts");
            boxes(entry.get(36..).unwrap_or(&[]), &mut |kind, v| {
                if (kind == b"dec3" && parse_dec3_atmos(v)) || (kind == b"dmlp" && parse_dmlp_atmos(v)) {
                    atmos = true;
                }
            });
            for &(chunk_idx, off) in &chunk_offsets {
                if atmos && (dtsx || !can_probe_dtsx) {
                    break;
                }
                let Some(chunk) = chunk_slice(chunk_idx, off) else { continue };
                if !atmos {
                    let chunk_has_atmos = match codec.as_str() {
                        "ec-3" => AudioBitstream::has_eac3_atmos_frame(chunk),
                        "mlpa" => AudioBitstream::has_truehd_atmos(chunk),
                        _ => false,
                    };
                    if chunk_has_atmos {
                        atmos = true;
                    }
                }
                if !dtsx && can_probe_dtsx && AudioBitstream::has_dtsx_sync(chunk) {
                    dtsx = true;
                }
            }
            info.audio.push(AudioTrack {
                codec,
                channels: be16(&entry, 24).map(u64::from),
                sample_rate: be32(&entry, 32).map(|rate| f64::from(rate >> 16)),
                language,
                atmos,
                dtsx,
                ..AudioTrack::default()
            });
        }
        b"subt" | b"text" | b"sbtl" => {
            info.subtitles.push(SubtitleTrack { codec, language, ..SubtitleTrack::default() });
        }
        _ => {}
    }
}

fn apply_rpu_to_video_track(v: &mut VideoTrack, mut dv: DolbyVisionInfo) {
    if v.dv_profile.is_none() {
        v.dv_profile = Some(dv.profile);
    }
    if v.dv_level.is_none() {
        v.dv_level = dv.level;
    } else if dv.level.is_none() || dv.level == Some(0) {
        dv.level = v.dv_level;
    }
    if v.dv_enhancement_layer.is_none() {
        v.dv_enhancement_layer = Some(dv.el_present);
    }
    v.dolby_vision = Some(dv);
}

fn skip_matroska_lace_header(flags: u8, payload: &[u8]) -> Option<&[u8]> {
    match (flags >> 1) & 0x03 {
        0 => Some(payload),
        1 => {
            let lace_count = usize::from(*payload.first()?);
            let mut pos = 1usize;
            for _ in 0..lace_count {
                while *payload.get(pos)? == 0xFF {
                    pos += 1;
                }
                pos += 1;
            }
            payload.get(pos..)
        }
        2 => payload.get(1..),
        3 => {
            let lace_count = usize::from(*payload.first()?);
            let mut pos = 1usize;
            for _ in 0..lace_count {
                let (_, vint_len) = element_size(payload.get(pos..)?)?;
                pos += vint_len;
            }
            payload.get(pos..)
        }
        _ => None,
    }
}

/// SHA-256 over a frame's coded slice NAL units, each as it's stored without its length, first 8 bytes in hex.
/// `frame` is `nal_length_size`-prefixed NAL units that must end exactly at its end, soo a frame cut off where the
/// read stopped gives None. HEVC slices are NAL types 0 to 31, H.264's 1 to 5; anything else gives None.
pub(super) fn keyframe_key(codec: &str, frame: &[u8], nal_length_size: usize) -> Option<String> {
    use sha2::{Digest, Sha256};
    let lower = codec.to_ascii_lowercase();
    // Matroska's V_MPEGH/ISO/HEVC and V_MPEG4/ISO/AVC; MP4's hvc1, hev1, avc1, avc3 and the Dolby Vision ones
    let hevc =
        lower.contains("hevc") || lower.contains("h265") || ["hvc1", "hev1", "dvh1", "dvhe"].contains(&lower.as_str());
    let avc = !hevc && (lower.contains("avc") || lower.contains("h264") || ["dva1", "dvav"].contains(&lower.as_str()));
    if !(hevc || avc) || !(1..=4).contains(&nal_length_size) {
        return None;
    }
    let mut h = Sha256::new();
    let (mut at, mut slices) = (0usize, 0usize);
    while at < frame.len() {
        let len = frame.get(at..at + nal_length_size)?.iter().fold(0usize, |n, b| n << 8 | usize::from(*b));
        let nal = frame.get(at + nal_length_size..(at + nal_length_size).checked_add(len)?)?;
        let first = *nal.first()?;
        let slice = if hevc { (first >> 1) & 0x3f < 32 } else { (1..=5).contains(&(first & 0x1f)) };
        if slice {
            h.update(nal);
            slices += 1;
        }
        at += nal_length_size + len;
    }
    if slices == 0 {
        return None;
    }
    Some(h.finalize()[..8].iter().map(|b| format!("{b:02x}")).collect())
}

fn probe_block(
    block: &[u8],
    complete: bool,
    video_tracks: &[(u64, usize, usize)],
    audio_tracks: &[(u64, usize)],
    video_rpu_found: &mut [bool],
    info: &mut MediaInfo,
) {
    let Some((Some(track_num), vint_len)) = element_size(block) else { return };
    let Some(&flags) = block.get(vint_len + 2) else { return };
    let Some(payload) = block.get(vint_len + 3..) else { return };

    if let Some(&(_, video_idx, nal_length_size)) =
        video_tracks.iter().find(|&&(num, _, _)| num == track_num)
    {
        // a SimpleBlock's keyframe flag, no lacing, and all of it read
        if complete
            && flags & 0x80 != 0
            && flags & 0x06 == 0
            && let Some(track) = info.video.get_mut(video_idx)
            && track.keyframe_key.is_none()
        {
            track.keyframe_key = keyframe_key(&track.codec, payload, nal_length_size);
        }
        if video_rpu_found.get(video_idx).copied() == Some(false)
            && let Some(frames) = skip_matroska_lace_header(flags, payload)
            && let Some(dv) = DolbyVisionInfo::parse_rpu_with_length_size(frames, nal_length_size)
            && let Some(track) = info.video.get_mut(video_idx)
        {
            apply_rpu_to_video_track(track, dv);
            if let Some(found) = video_rpu_found.get_mut(video_idx) {
                *found = true;
            }
        }
        return;
    }

    let Some(&(_, audio_idx)) = audio_tracks.iter().find(|&&(num, _)| num == track_num) else { return };
    let Some(track) = info.audio.get_mut(audio_idx) else { return };
    let can_probe_dtsx = track.codec.is_empty() || track.codec.to_ascii_lowercase().contains("dts");
    if track.atmos && (track.dtsx || !can_probe_dtsx) {
        return;
    }
    let Some(frames) = skip_matroska_lace_header(flags, payload) else { return };
    if !track.atmos {
        let has_atmos = match track.codec.as_str() {
            "A_TRUEHD" | "A_MLP" => AudioBitstream::has_truehd_atmos(frames),
            "A_EAC3" => AudioBitstream::has_eac3_atmos_frame(frames),
            _ => false,
        };
        if has_atmos {
            track.atmos = true;
        }
    }
    if !track.dtsx && can_probe_dtsx && AudioBitstream::has_dtsx_sync(frames) {
        track.dtsx = true;
    }
}

fn probe_cluster(
    data: &[u8],
    video_tracks: &[(u64, usize, usize)],
    audio_tracks: &[(u64, usize)],
    info: &mut MediaInfo,
) {
    let mut video_rpu_found = vec![false; info.video.len()];
    walk_sized(data, &mut |id, body, size| {
        match id {
            0xA3 => {
                let complete = size == Some(body.len() as u64);
                probe_block(body, complete, video_tracks, audio_tracks, &mut video_rpu_found, info)
            }
            0xA0 => walk(body, &mut |sub_id, sub_body| {
                // a Block's flags have no keyframe bit: it's never taken for the keyframe key
                if sub_id == 0xA1 {
                    probe_block(sub_body, false, video_tracks, audio_tracks, &mut video_rpu_found, info);
                }
                true
            }),
            _ => {}
        }
        true
    });

    if video_rpu_found.first().copied() == Some(false)
        && let Some(dv) = DolbyVisionInfo::parse_rpu(data)
        && let Some(v) = info.video.first_mut()
    {
        apply_rpu_to_video_track(v, dv);
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
    /// Whether `fel` and `cm_version` were read from an RPU. Without one they're only defaults: a `dvcC` box or a
    /// Matroska mapping says the profile, level and layers, not which enhancement layer or which content mapping.
    #[serde(default)]
    pub from_rpu: bool,
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
            fel: false,
            cm_version: "v2.9".to_string(),
            from_rpu: false,
        })
    }

    pub fn parse_rpu_with_length_size(data: &[u8], nal_length_size: usize) -> Option<Self> {
        if let Some(nal_payload) = Self::extract_length_prefixed_rpu_nal(data, nal_length_size)
            && let Some(info) = Self::parse_rpu_payload(nal_payload)
        {
            return Some(info);
        }
        Self::parse_rpu(data)
    }

    pub fn parse_rpu(data: &[u8]) -> Option<Self> {
        if data.is_empty() {
            return None;
        }

        if let Some(nal_payload) = Self::extract_rpu_nal(data)
            && let Some(info) = Self::parse_rpu_payload(nal_payload)
        {
            return Some(info);
        }

        None
    }

    pub fn parse_unspec62(nal: &[u8]) -> Option<Self> {
        Self::parse_rpu(nal)
    }

    fn extract_length_prefixed_rpu_nal(data: &[u8], nal_length_size: usize) -> Option<&[u8]> {
        if !(1..=4).contains(&nal_length_size) || data.len() < nal_length_size + 2 {
            return None;
        }
        let mut pos = 0usize;
        while pos.saturating_add(nal_length_size).saturating_add(2) <= data.len() {
            let len_bytes = data.get(pos..pos + nal_length_size)?;
            let nal_len = len_bytes.iter().fold(0usize, |acc, &b| (acc << 8) | usize::from(b));
            if nal_len < 2 {
                break;
            }
            let nal_start = pos + nal_length_size;
            let nal_end = nal_start.saturating_add(nal_len);
            let hdr0 = *data.get(nal_start)?;
            let hdr1 = *data.get(nal_start + 1)?;
            let forbidden_zero = (hdr0 & 0x80) == 0;
            let nal_type = (hdr0 >> 1) & 0x3f;
            let tid_plus1 = hdr1 & 0x07;
            if forbidden_zero && tid_plus1 != 0 && nal_type == 62 {
                let payload_end = nal_end.min(data.len());
                if nal_start + 2 < payload_end {
                    return Some(&data[nal_start + 2..payload_end]);
                }
            }
            if nal_end > data.len() {
                break;
            }
            pos = nal_end;
        }
        None
    }

    fn extract_rpu_nal(data: &[u8]) -> Option<&[u8]> {
        if data.len() >= 2 && (data[0] & 0x80) == 0 && ((data[0] >> 1) & 0x3f) == 62 && (data[1] & 0x07) != 0 {
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

        Self::extract_length_prefixed_rpu_nal(data, 4)
            .or_else(|| Self::extract_length_prefixed_rpu_nal(data, 2))
    }

    fn parse_rpu_payload(payload: &[u8]) -> Option<Self> {
        let rbsp = crate::video::bits::rbsp(payload);
        let trimmed_len = rbsp.iter().rposition(|&b| b != 0).map_or(0, |i| i + 1);
        let rbsp = &rbsp[..trimmed_len];
        if rbsp.len() < 4 {
            return None;
        }

        let mut r = crate::video::bits::BitReader::new(rbsp);
        let rpu_prefix = r.bits(8).ok()? as u8;
        if rpu_prefix != 0x19 {
            return None;
        }

        let rpu_type = r.bits(6).ok()? as u8;
        if rpu_type != 2 {
            return None;
        }
        let rpu_format = r.bits(11).ok()? as u16;
        let vdr_rpu_profile = r.bits(4).ok()? as u8;
        let vdr_rpu_level = r.bits(4).ok()? as u8;

        let mut coefficient_data_type = 0u8;
        let mut coeff_log2_denom_len = 0u32;
        let mut bl_video_full_range_flag = false;
        let mut bl_bit_depth_minus8 = 2u32;
        let mut el_bit_depth_minus8 = 2u32;
        let mut vdr_bit_depth_minus8 = 2u32;
        let mut reserved_zero_3bits = 0u8;
        let mut el_spatial_resampling_filter_flag = false;
        let mut disable_residual_flag = false;

        let vdr_seq_info_present = r.flag().ok()?;
        if vdr_seq_info_present {
            let _chroma_resampling_explicit_filter_flag = r.flag().ok()?;
            coefficient_data_type = r.bits(2).ok()? as u8;
            let coefficient_log2_denom = if coefficient_data_type == 0 {
                r.ue().ok()?
            } else {
                0
            };
            let _vdr_rpu_normalized_idc = r.bits(2).ok()?;
            bl_video_full_range_flag = r.flag().ok()?;
            if (rpu_format & 0x700) == 0 {
                bl_bit_depth_minus8 = r.ue().ok()?;
                el_bit_depth_minus8 = r.ue().ok()? & 0xFF;
                vdr_bit_depth_minus8 = r.ue().ok()?;
                let _spatial_resampling_filter_flag = r.flag().ok()?;
                reserved_zero_3bits = r.bits(3).ok()? as u8;
                el_spatial_resampling_filter_flag = r.flag().ok()?;
                disable_residual_flag = r.flag().ok()?;
            }
            coeff_log2_denom_len = match coefficient_data_type {
                0 => {
                    if coefficient_log2_denom > 32 {
                        return None;
                    }
                    coefficient_log2_denom
                }
                1 => 32,
                _ => return None,
            };
        }

        let vdr_dm_metadata_present_flag = r.flag().unwrap_or(false);
        let use_prev_vdr_rpu_flag = r.flag().unwrap_or(false);
        if use_prev_vdr_rpu_flag {
            let _prev_vdr_rpu_id = r.ue().ok()?;
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

        let mut is_fel = false;
        let mut fel_read = profile != 7;
        let mut mapping_ok = use_prev_vdr_rpu_flag;
        if !use_prev_vdr_rpu_flag
            && let Some(parsed_fel) = Self::parse_rpu_mapping_and_nlq(
                &mut r,
                rpu_format,
                disable_residual_flag,
                coefficient_data_type,
                coeff_log2_denom_len,
                bl_bit_depth_minus8,
                el_bit_depth_minus8,
            )
        {
            mapping_ok = true;
            if profile == 7
                && let Some(fel) = parsed_fel
            {
                is_fel = fel;
                fel_read = true;
            }
        }

        let mut has_cm4 = false;
        let mut cm_read = false;
        if mapping_ok
            && vdr_dm_metadata_present_flag
            && let Some(cm4) = Self::parse_vdr_dm_data(&mut r, reserved_zero_3bits == 1)
        {
            has_cm4 = cm4;
            cm_read = true;
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
            // only when this RPU's own mapping and display management data were read: a truncated one, or one that
            // points back at an earlier RPU, leaves fel and cm_version at their defaults
            from_rpu: !use_prev_vdr_rpu_flag && fel_read && cm_read,
        })
    }

    fn parse_rpu_mapping_and_nlq(
        r: &mut crate::video::bits::BitReader<'_>,
        rpu_format: u16,
        disable_residual_flag: bool,
        coefficient_data_type: u8,
        coeff_log2_denom_len: u32,
        bl_bit_depth_minus8: u32,
        el_bit_depth_minus8: u32,
    ) -> Option<Option<bool>> {
        const NUM_COMPONENTS: usize = 3;
        const NLQ_NUM_PIVOTS: usize = 2;
        const MMR_MAX_COEFFS: usize = 7;

        let _vdr_rpu_id = r.ue().ok()?;
        let _mapping_color_space = r.ue().ok()?;
        let _mapping_chroma_format_idc = r.ue().ok()?;

        let bl_bit_depth = bl_bit_depth_minus8.checked_add(8)?;
        if bl_bit_depth > 32 {
            return None;
        }

        let mut num_pivots_minus2 = [0u32; NUM_COMPONENTS];
        for num_pm2 in &mut num_pivots_minus2 {
            let npm2 = r.ue().ok()?;
            if npm2 > 64 {
                return None;
            }
            *num_pm2 = npm2;
            let num_pivots = (npm2 as usize) + 2;
            for _ in 0..num_pivots {
                let _pivot = r.bits(bl_bit_depth).ok()?;
            }
        }

        let has_nlq = (rpu_format & 0x700) == 0 && !disable_residual_flag;
        let mut nlq_method_idc = None;
        if has_nlq {
            let method = r.bits(3).ok()? as u8;
            if method != 0 {
                return None;
            }
            nlq_method_idc = Some(method);
            for _ in 0..NLQ_NUM_PIVOTS {
                let _pv = r.bits(bl_bit_depth).ok()?;
            }
        }

        let _num_x_partitions_minus1 = r.ue().ok()?;
        let _num_y_partitions_minus1 = r.ue().ok()?;

        for &npm2 in &num_pivots_minus2 {
            let num_pieces = (npm2 as usize) + 1;
            for pivot_idx in 0..num_pieces {
                let mapping_idc = r.ue().ok()?;
                match mapping_idc {
                    0 => {
                        let poly_order_minus1 = r.ue().ok()?;
                        if poly_order_minus1 > 1 {
                            return None;
                        }
                        let linear_interp_flag = if poly_order_minus1 == 0 {
                            r.flag().ok()?
                        } else {
                            false
                        };
                        if poly_order_minus1 == 0 && linear_interp_flag {
                            if coefficient_data_type == 0 {
                                let _ = r.ue().ok()?;
                            }
                            let _ = r.bits(coeff_log2_denom_len).ok()?;
                            if pivot_idx as u32 == npm2 {
                                if coefficient_data_type == 0 {
                                    let _ = r.ue().ok()?;
                                }
                                let _ = r.bits(coeff_log2_denom_len).ok()?;
                            }
                        } else {
                            let poly_coef_count = (poly_order_minus1 as usize) + 2;
                            for _ in 0..poly_coef_count {
                                if coefficient_data_type == 0 {
                                    let _ = r.se().ok()?;
                                }
                                let _ = r.bits(coeff_log2_denom_len).ok()?;
                            }
                        }
                    }
                    1 => {
                        let mmr_order_minus1 = r.bits(2).ok()? as usize;
                        if mmr_order_minus1 > 2 {
                            return None;
                        }
                        if coefficient_data_type == 0 {
                            let _ = r.se().ok()?;
                        }
                        let _ = r.bits(coeff_log2_denom_len).ok()?;
                        for _ in 0..(mmr_order_minus1 + 1) {
                            for _ in 0..MMR_MAX_COEFFS {
                                if coefficient_data_type == 0 {
                                    let _ = r.se().ok()?;
                                }
                                let _ = r.bits(coeff_log2_denom_len).ok()?;
                            }
                        }
                    }
                    _ => return None,
                }
            }
        }

        if let Some(0) = nlq_method_idc {
            let el_bit_depth = el_bit_depth_minus8.checked_add(8)?;
            if el_bit_depth > 32 {
                return None;
            }
            let mut nlq_offset = [0u32; NUM_COMPONENTS];
            let mut vdr_in_max_int = [0u32; NUM_COMPONENTS];
            let mut vdr_in_max = [0u32; NUM_COMPONENTS];
            let mut linear_deadzone_slope_int = [0u32; NUM_COMPONENTS];
            let mut linear_deadzone_slope = [0u32; NUM_COMPONENTS];
            let mut linear_deadzone_threshold_int = [0u32; NUM_COMPONENTS];
            let mut linear_deadzone_threshold = [0u32; NUM_COMPONENTS];

            for cmp in 0..NUM_COMPONENTS {
                nlq_offset[cmp] = r.bits(el_bit_depth).ok()?;
                if coefficient_data_type == 0 {
                    vdr_in_max_int[cmp] = r.ue().ok()?;
                }
                vdr_in_max[cmp] = r.bits(coeff_log2_denom_len).ok()?;
                if coefficient_data_type == 0 {
                    linear_deadzone_slope_int[cmp] = r.ue().ok()?;
                }
                linear_deadzone_slope[cmp] = r.bits(coeff_log2_denom_len).ok()?;
                if coefficient_data_type == 0 {
                    linear_deadzone_threshold_int[cmp] = r.ue().ok()?;
                }
                linear_deadzone_threshold[cmp] = r.bits(coeff_log2_denom_len).ok()?;
            }

            let is_mel = nlq_offset == [0; 3]
                && vdr_in_max_int == [1; 3]
                && vdr_in_max == [0; 3]
                && linear_deadzone_slope_int == [0; 3]
                && linear_deadzone_slope == [0; 3]
                && linear_deadzone_threshold_int == [0; 3]
                && linear_deadzone_threshold == [0; 3];
            return Some(Some(!is_mel));
        }

        Some(None)
    }

    fn parse_dm_ext_blocks(
        r: &mut crate::video::bits::BitReader<'_>,
        has_cm4: &mut bool,
    ) -> Option<()> {
        let num_ext_blocks = r.ue().ok()?;
        if num_ext_blocks > 256 {
            return None;
        }
        while !r.is_byte_aligned() {
            if r.flag().ok()? {
                return None;
            }
        }
        for _ in 0..num_ext_blocks {
            let ext_block_length = r.ue().ok()? as usize;
            let ext_block_level = r.bits(8).ok()? as u8;
            if matches!(ext_block_level, 3 | 8 | 9 | 10 | 11 | 254) {
                *has_cm4 = true;
            }
            r.skip(ext_block_length.checked_mul(8)?).ok()?;
        }
        Some(())
    }

    fn parse_vdr_dm_data(
        r: &mut crate::video::bits::BitReader<'_>,
        compressed_dm_data: bool,
    ) -> Option<bool> {
        const DM_DATA_PAYLOAD2_MIN_BITS: usize = 56;

        let _affected_dm_metadata_id = r.ue().ok()?;
        let _current_dm_metadata_id = r.ue().ok()?;
        let _scene_refresh_flag = r.ue().ok()?;
        if !compressed_dm_data {
            r.skip(509).ok()?;
        }
        let mut has_cm4 = false;
        Self::parse_dm_ext_blocks(r, &mut has_cm4)?;
        if r.bits_left() >= DM_DATA_PAYLOAD2_MIN_BITS {
            let _ = Self::parse_dm_ext_blocks(r, &mut has_cm4);
        }
        Some(has_cm4)
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
        if (nal_slice[0] & 0x80) != 0 || ((nal_slice[0] >> 1) & 0x3F) != 33 {
            return None;
        }

        let sps = crate::video::hevc::params::parse_sps(nal_slice).ok()?;
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
        Some(VuiInfo {
            colour_primaries: primaries,
            transfer_characteristics: transfer,
            matrix_coeffs: matrix,
            bit_depth,
            full_range: sps.full_range,
            hdr,
        })
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
        if let Ok(parsed) = crate::video::h264::parse_avcc(avcc) {
            for unit in &parsed.units {
                if unit.first().is_some_and(|&h| (h & 0x1F) == 7)
                    && let Some(depth) = Self::from_sps(unit)
                {
                    return Some(depth);
                }
            }
        }

        let profile = *avcc.get(1)?;
        if !matches!(profile, 100 | 110 | 122 | 144 | 244) {
            return Some(8);
        }

        let mut at = 5usize;
        let num_sps = usize::from(*avcc.get(at)? & 0x1F);
        at += 1;
        for _ in 0..num_sps {
            let len = usize::from(u16::from_be_bytes([*avcc.get(at)?, *avcc.get(at + 1)?]));
            at = at.checked_add(2)?.checked_add(len)?;
            let sps = avcc.get(at - len..at)?;
            if sps.first().is_some_and(|&h| (h & 0x1F) == 7)
                && let Some(depth) = Self::from_sps(sps)
            {
                return Some(depth);
            }
        }
        let num_pps = usize::from(*avcc.get(at)?);
        at += 1;
        for _ in 0..num_pps {
            let len = usize::from(u16::from_be_bytes([*avcc.get(at)?, *avcc.get(at + 1)?]));
            at = at.checked_add(2)?.checked_add(len)?;
            avcc.get(at - len..at)?;
        }

        if let Some(&depth_byte) = avcc.get(at + 1) {
            let depth = (depth_byte & 0x07) + 8;
            if (8..=14).contains(&depth) {
                return Some(depth);
            }
        }
        if profile == 100 {
            Some(8)
        } else {
            None
        }
    }

    pub fn from_sps(nal: &[u8]) -> Option<u8> {
        let mut nal_slice = nal;
        if nal_slice.starts_with(&[0, 0, 0, 1]) {
            nal_slice = &nal_slice[4..];
        } else if nal_slice.starts_with(&[0, 0, 1]) {
            nal_slice = &nal_slice[3..];
        }
        if let Ok(sps) = crate::video::h264::params::parse_sps(nal_slice) {
            return Some(sps.bit_depth_luma);
        }
        let data = crate::video::bits::rbsp(nal_slice.get(1..)?);
        let mut r = crate::video::bits::BitReader::new(&data);
        let profile = r.bits(8).ok()? as u8;
        r.skip(16).ok()?;
        let _id = r.ue_max(31, "SPS id").ok()?;
        if matches!(profile, 100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135) {
            let chroma_format = r.ue_max(3, "chroma format").ok()?;
            if chroma_format == 3 {
                let _ = r.flag().ok()?;
            }
            let depth_luma = r.ue_max(6, "bit depth").ok()? as u8 + 8;
            Some(depth_luma)
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
    pub fn has_truehd_atmos(data: &[u8]) -> bool {
        for (i, window) in data.windows(4).enumerate() {
            if (window == [0xF8, 0x72, 0x6F, 0xBA] || window == [0xBA, 0x6F, 0x72, 0xF8])
                && data.get(i + 17).is_some_and(|&b| b & 0x80 != 0)
            {
                return true;
            }
        }
        false
    }

    fn has_valid_joc_emdf_payload(container: &[u8]) -> bool {
        let mut r = crate::video::bits::BitReader::new(container);
        let Ok(emdf_version) = r.bits(2) else { return false };
        if emdf_version != 0 {
            return false;
        }
        let Ok(key_id) = r.bits(3) else { return false };
        if key_id == 7 {
            loop {
                let Ok(ext) = r.bits(3) else { return false };
                if ext != 7 {
                    break;
                }
            }
        }
        let Ok(mut payload_id) = r.bits(5) else { return false };
        if payload_id == 0x1F {
            let Ok(ext) = r.bits(5) else { return false };
            payload_id += ext;
        }
        if payload_id != 14 {
            return false;
        }
        // joc_header(): joc_dmx_config_idx (3b), joc_num_objects_bits (6b), joc_ext_config_idx (3b)
        r.bits(12).is_ok()
    }

    fn has_valid_joc_emdf_container(emdf_slice: &[u8]) -> bool {
        if emdf_slice.len() < 4 || emdf_slice[0] != 0x58 || emdf_slice[1] != 0x38 {
            return false;
        }
        let container_len = usize::from(u16::from_be_bytes([emdf_slice[2], emdf_slice[3]]));
        if container_len == 0 || 4 + container_len > emdf_slice.len() {
            return false;
        }
        Self::has_valid_joc_emdf_payload(&emdf_slice[4..4 + container_len])
    }

    pub fn has_eac3_atmos_frame(data: &[u8]) -> bool {
        let mut offset = 0usize;
        let mut verified_no_emdf_start = 0usize;
        while let Some(slice) = data.get(offset..) {
            let Some(pos) = find_subsequence(slice, &[0x0B, 0x77]) else { break };
            let frame_start = offset + pos;
            let frame = &data[frame_start..];
            if frame.len() >= 8 {
                let strmtyp = (frame[2] >> 6) & 0x03;
                let frmsiz = ((usize::from(frame[2] & 0x07)) << 8) | usize::from(frame[3]);
                let frame_size = (frmsiz + 1) * 2;
                let bsid = (frame[5] >> 3) & 0x1F;
                if strmtyp != 3 && frmsiz >= 3 && (11..=16).contains(&bsid) {
                    let body_start = frame_start + 6;
                    let body_end = frame_start.saturating_add(frame_size).min(data.len());
                    let scan_start = body_start.max(verified_no_emdf_start);
                    if scan_start < body_end {
                        let body = &data[scan_start..body_end];
                        let mut emdf_off = 0usize;
                        let mut max_verified_end = body_end.saturating_sub(3);
                        while let Some(emdf_slice) = body.get(emdf_off..) {
                            let Some(emdf_pos) = find_subsequence(emdf_slice, &[0x58, 0x38]) else { break };
                            if Self::has_valid_joc_emdf_container(&emdf_slice[emdf_pos..]) {
                                return true;
                            }
                            let abs_emdf_pos = scan_start + emdf_off + emdf_pos;
                            if Self::has_valid_joc_emdf_container(&data[abs_emdf_pos..]) {
                                max_verified_end = max_verified_end.min(abs_emdf_pos);
                                break;
                            }
                            emdf_off = emdf_off.saturating_add(emdf_pos + 2);
                        }
                        verified_no_emdf_start = verified_no_emdf_start.max(max_verified_end);
                    }
                }
            }
            offset = frame_start.saturating_add(2);
        }
        false
    }

    fn has_raw_emdf_payload(data: &[u8]) -> bool {
        if data.len() > 256 || is_matroska(data) || is_mp4(data) || find_subsequence(data, &[0x0B, 0x77]).is_some() {
            return false;
        }
        if let Some(pos) = find_subsequence(data, &[0x58, 0x38]) {
            data[..pos].iter().all(|&b| b == 0) && Self::has_valid_joc_emdf_container(&data[pos..])
        } else {
            false
        }
    }

    pub fn has_dtsx_sync(data: &[u8]) -> bool {
        find_subsequence(data, &[0x02, 0x00, 0x08, 0x50]).is_some()
            || find_subsequence(data, &[0x50, 0x08, 0x00, 0x02]).is_some()
            || find_subsequence(data, &[0xF1, 0x40, 0x00, 0xD0]).is_some()
    }

    pub fn probe(data: &[u8]) -> AudioProbeResult {
        let mut result = AudioProbeResult::default();

        if is_matroska(data) || is_mp4(data) {
            if let Some(info) = parse(data) {
                if let Some(track) = info.audio.iter().find(|a| a.atmos) {
                    result.has_atmos = true;
                    result.format = Some(match track.codec.as_str() {
                        "A_TRUEHD" | "A_MLP" | "mlpa" => "TrueHD Atmos".to_string(),
                        _ => "E-AC-3 Atmos".to_string(),
                    });
                }
                if info.audio.iter().any(|a| a.dtsx) {
                    result.has_dtsx = true;
                    if result.format.is_none() {
                        result.format = Some("DTS:X".to_string());
                    }
                }
            }
            return result;
        }

        if Self::has_truehd_atmos(data) {
            result.has_atmos = true;
            result.format = Some("TrueHD Atmos".to_string());
        } else if Self::has_eac3_atmos_frame(data) || Self::has_raw_emdf_payload(data) {
            result.has_atmos = true;
            result.format = Some("E-AC-3 Atmos".to_string());
        }

        if Self::has_dtsx_sync(data) {
            result.has_dtsx = true;
            if result.format.is_none() {
                result.format = Some("DTS:X".to_string());
            }
        }

        result
    }

    pub fn probe_truehd_atmos(data: &[u8]) -> bool {
        Self::has_truehd_atmos(data)
    }

    pub fn probe_dtsx(data: &[u8]) -> bool {
        Self::probe(data).has_dtsx
    }
}

pub struct Mp4Prober;

impl Mp4Prober {
    pub fn find_moov_offset(data: &[u8]) -> Option<u64> {
        let mut at = 0usize;
        while let Some(head_end) = at.checked_add(8)
            && let Some(head) = data.get(at..head_end)
        {
            let mut size = u64::from(u32::from_be_bytes([head[0], head[1], head[2], head[3]]));
            let kind = &head[4..8];
            let mut head_size = 8u64;
            if size == 1 {
                let ext_end = at.checked_add(16)?;
                let long = data.get(head_end..ext_end)?;
                size = u64::from_be_bytes(long.try_into().unwrap_or([0; 8]));
                head_size = 16;
            } else if size == 0 {
                return None;
            }
            if size < head_size {
                return None;
            }
            if kind == b"mdat" {
                return (at as u64).checked_add(size);
            }
            let step = usize::try_from(size).ok()?;
            let end = at.checked_add(step)?;
            if end > data.len() {
                return None;
            }
            at = end;
        }
        None
    }

    pub fn probe_tail_moov(data: &[u8]) -> Option<MediaInfo> {
        if is_mp4(data) || Self::has_moov_box(data) {
            Some(mp4(data))
        } else {
            None
        }
    }

    fn has_moov_box(data: &[u8]) -> bool {
        let mut at = 0usize;
        while let Some(head_end) = at.checked_add(8)
            && let Some(head) = data.get(at..head_end)
        {
            let mut size = u64::from(u32::from_be_bytes([head[0], head[1], head[2], head[3]]));
            let kind = &head[4..8];
            if !kind.iter().all(|b| b.is_ascii_graphic() || *b == b' ') {
                return false;
            }
            let mut head_size = 8u64;
            if size == 1 {
                let Some(ext_end) = at.checked_add(16) else { return false };
                let Some(long) = data.get(head_end..ext_end) else { return false };
                size = u64::from_be_bytes(long.try_into().unwrap_or([0; 8]));
                head_size = 16;
            } else if size == 0 {
                size = (data.len() - at) as u64;
            }
            if size < head_size {
                return false;
            }
            if kind == b"moov" {
                return true;
            }
            let Some(step) = usize::try_from(size).ok() else { return false };
            let Some(end) = at.checked_add(step) else { return false };
            if end > data.len() {
                return false;
            }
            at = end;
        }
        false
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
        hasher.update(video_ts);
        hasher.update(vts);
        hasher.finalize().into()
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

    /// Length-prefixed (4 bytes) HEVC NAL units, each a two-byte header of `kind` and then `body`.
    fn hevc_frame(nals: &[(u8, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        for (kind, body) in nals {
            out.extend(((body.len() + 2) as u32).to_be_bytes());
            out.extend([kind << 1, 1]);
            out.extend(*body);
        }
        out
    }

    #[test]
    fn the_keyframe_key_is_over_its_slices_alone() {
        let slice: &[u8] = &[0xAF, 0x01, 0x02, 0x03];
        let plain = hevc_frame(&[(19, slice)]);
        let muxed =
            hevc_frame(&[(32, b"vps"), (33, b"sps"), (34, b"pps"), (39, b"x265 info"), (19, slice), (62, b"rpu")]);
        let key = keyframe_key("V_MPEGH/ISO/HEVC", &plain, 4).unwrap();
        assert_eq!(key.len(), 16);
        assert_eq!(keyframe_key("hvc1", &muxed, 4).as_deref(), Some(key.as_str()), "SEI, parameter sets, RPU left out");
        assert_ne!(
            keyframe_key("hvc1", &hevc_frame(&[(19, &[0xAF, 0x01, 0x02, 0x04])]), 4).as_deref(),
            Some(key.as_str())
        );
        assert_eq!(keyframe_key("hvc1", &muxed[..muxed.len() - 1], 4), None, "a frame cut off gives none");
        assert_eq!(keyframe_key("hvc1", &hevc_frame(&[(39, b"sei only")]), 4), None);
        assert_eq!(keyframe_key("V_MPEG4/ISO/AVC", &[0, 0, 0, 2, 0x65, 0x88], 4).map(|k| k.len()), Some(16));
        assert_eq!(keyframe_key("V_AV1", &plain, 4), None);
    }

    #[test]
    fn a_matroska_keyframe_read_whole_gives_the_key() {
        let frame = hevc_frame(&[(39, b"sei"), (19, &[0xAF, 9, 9])]);
        let block = |flags: u8, declared_extra: usize| {
            let mut body = vec![0x81, 0, 0, flags];
            body.extend(&frame);
            let mut out = vec![0xA3, 0x80 | (body.len() + declared_extra) as u8];
            out.extend(body);
            out
        };
        let probe = |cluster: Vec<u8>| {
            let mut info = MediaInfo {
                video: vec![VideoTrack { codec: "V_MPEGH/ISO/HEVC".into(), ..Default::default() }],
                ..Default::default()
            };
            probe_cluster(&cluster, &[(1, 0, 4)], &[], &mut info);
            info.video[0].keyframe_key.clone()
        };
        let want = keyframe_key("V_MPEGH/ISO/HEVC", &frame, 4);
        assert!(want.is_some());
        assert_eq!(probe(block(0x80, 0)), want);
        assert_eq!(probe(block(0x00, 0)), None, "not a keyframe");
        assert_eq!(probe(block(0x80, 5)), None, "the block runs past what was read");
    }

    #[test]
    fn hdr_names_follow_the_transfer_and_mastering_data() {
        assert_eq!(hdr_name(Some(16), true, None, None).as_deref(), Some("HDR10"));
        assert_eq!(hdr_name(Some(16), false, None, None).as_deref(), Some("PQ"));
        assert_eq!(hdr_name(Some(18), false, None, None).as_deref(), Some("HLG"));
        assert_eq!(hdr_name(None, true, None, None).as_deref(), Some("HDR10"));
        assert_eq!(hdr_name(Some(1), false, None, None), None);
    }

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
    fn encoder_settings_come_from_the_first_frame() {
        let x265 = b"\x00\x00\x01\x4e\x01\x05x265 (build 199) - 3.5+1-f0c1022b6:[Linux][GCC 10.2.1][64 bit] 10bit - H.265/HEVC codec - Copyright 2013-2018 (c) Multicoreware, Inc - http://x265.org - options: cpuid=1111039 crf=18.0 preset=slow\x00\x80";
        let (encoder, options, complete) = encoder_settings(x265).unwrap();
        assert!(complete);
        assert_eq!(encoder, "x265 3.5+1-f0c1022b6");
        assert_eq!(options.as_deref(), Some("cpuid=1111039 crf=18.0 preset=slow"));
        let x264 = b"junk x264 - core 164 r3095 baee400 - H.264/MPEG-4 AVC codec - Copyleft 2003-2022 - http://www.videolan.org/x264.html - options: cabac=1 ref=4 crf=16.0\x00";
        let (encoder, options, _) = encoder_settings(x264).unwrap();
        assert_eq!(encoder, "x264 core 164 r3095 baee400");
        assert_eq!(options.as_deref(), Some("cabac=1 ref=4 crf=16.0"));
        // cut off by the end of the article: it may go on in the next
        let (_, cut, complete) = encoder_settings(&x264[..x264.len() - 4]).unwrap();
        assert_eq!(cut.as_deref(), Some("cabac=1 ref=4 crf=1"));
        assert!(!complete);
        assert_eq!(encoder_settings(b"nothing here"), None);
    }

    #[test]
    fn the_segment_uid_editions_and_default_flags_come_out() {
        let uid: Vec<u8> = (1..=16).collect();
        let info = el(&[0x73, 0xA4], &uid);
        let atom = el(&[0xB6], &el(&[0x91], &[0]));
        let chapters = el(&[0x10, 0x43, 0xA7, 0x70], &[el(&[0x45, 0xB9], &atom), el(&[0x45, 0xB9], &atom)].concat());
        let a1 = [el(&[0x83], &[2]), el(&[0x86], b"A_AC3")].concat();
        let a2 = [el(&[0x83], &[2]), el(&[0x86], b"A_AC3"), el(&[0x88], &[0])].concat();
        let tracks = el(&[0x16, 0x54, 0xAE, 0x6B], &[el(&[0xAE], &a1), el(&[0xAE], &a2)].concat());
        let segment = [el(&[0x15, 0x49, 0xA9, 0x66], &info), tracks, chapters].concat();
        let data =
            [el(&[0x1A, 0x45, 0xDF, 0xA3], &el(&[0x42, 0x82], b"matroska")), el(&[0x18, 0x53, 0x80, 0x67], &segment)]
                .concat();
        let m = parse(&data).unwrap();
        assert_eq!(m.segment_uid.as_deref(), Some("0102030405060708090a0b0c0d0e0f10"));
        assert_eq!(m.editions, 2);
        assert_eq!(m.audio.iter().map(|a| a.default).collect::<Vec<_>>(), [true, false], "unsaid means default");
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

    #[test]
    fn avc_track_with_23_byte_codec_private_uses_avcc_parser() {
        // 27-byte ISO/IEC 14496-15 High 10 avcC record:
        // 1 SPS (8 bytes -> byte 6 is 0x00), 1 PPS (4 bytes -> byte 17 is 0x00 so
        // HevcDepth::from_hvcc would misread it as 8-bit), plus 4-byte configuration extension.
        let sps = [0x67, 110, 0x00, 0x28, 0xA6, 0xCE, 0x9E, 0x40];
        let pps = [0x68, 0xEE, 0x3C, 0x80];
        let mut avcc = vec![1, 110, 0x00, 0x28, 0xFF, 0xE1, 0x00, sps.len() as u8];
        avcc.extend_from_slice(&sps);
        avcc.extend_from_slice(&[0x01, 0x00, pps.len() as u8]);
        avcc.extend_from_slice(&pps);
        avcc.extend_from_slice(&[0xFD, 0xFA, 0xFA, 0x00]);
        assert!(avcc.len() >= 23);
        assert_eq!(avcc[6], 0);
        assert_eq!(avcc[17], 0);

        let t1 = [
            el(&[0x83], &[1]),
            el(&[0x86], b"V_MPEG4/ISO/AVC"),
            el(&[0x63, 0xA2], &avcc),
        ]
        .concat();
        let tracks = el(&[0x16, 0x54, 0xAE, 0x6B], &el(&[0xAE], &t1));
        // a keyframe with 4-byte NAL lengths, as avcC byte 4 (0xFF) says: an IDR slice
        let frame = [0, 0, 0, 3, 0x65, 0x88, 0x84];
        let block = [&[0x81u8, 0, 0, 0x80][..], &frame].concat();
        let cluster = el(&[0x1F, 0x43, 0xB6, 0x75], &el(&[0xA3], &block));
        let segment = el(&[0x18, 0x53, 0x80, 0x67], &[tracks, cluster].concat());
        let mkv = [el(&[0x1A, 0x45, 0xDF, 0xA3], &el(&[0x42, 0x82], b"matroska")), segment].concat();

        let info = parse(&mkv).unwrap();
        assert_eq!(info.video[0].codec, "V_MPEG4/ISO/AVC");
        assert_eq!(info.video[0].bit_depth, Some(10));
        assert_eq!(info.video[0].keyframe_key, keyframe_key("V_MPEG4/ISO/AVC", &frame, 4));
        assert!(info.video[0].keyframe_key.is_some(), "the NAL length width comes from avcC byte 4");
    }

    #[test]
    fn probe_tail_moov_parses_standalone_moov_without_ftyp() {
        let mut entry = vec![0u8; 86];
        entry[4..8].copy_from_slice(b"hvc1");
        entry[32..34].copy_from_slice(&3840u16.to_be_bytes());
        entry[34..36].copy_from_slice(&2160u16.to_be_bytes());
        let mut stsd = vec![0u8; 8];
        stsd.extend(&entry);
        let stbl = mp4_box(b"stbl", &mp4_box(b"stsd", &stsd));
        let mut hdlr = vec![0u8; 8];
        hdlr.extend(b"vide");
        hdlr.extend([0u8; 12]);
        let mdia = [mp4_box(b"hdlr", &hdlr), mp4_box(b"minf", &stbl)].concat();
        let mut mvhd = vec![0u8; 20];
        mvhd[12..16].copy_from_slice(&1000u32.to_be_bytes());
        mvhd[16..20].copy_from_slice(&90_000u32.to_be_bytes());
        let moov = mp4_box(b"moov", &[mp4_box(b"mvhd", &mvhd), mp4_box(b"trak", &mp4_box(b"mdia", &mdia))].concat());

        // Whole file with ftyp + mdat + moov: slice at find_moov_offset starts with `moov`
        let file = [mp4_box(b"ftyp", b"isom"), mp4_box(b"mdat", &[0u8; 64]), moov.clone()].concat();
        let moov_off = Mp4Prober::find_moov_offset(&file).expect("must find moov offset") as usize;
        let tail_slice = &file[moov_off..];
        assert_eq!(&tail_slice[4..8], b"moov");

        let info = Mp4Prober::probe_tail_moov(tail_slice).expect("standalone moov tail slice must parse");
        assert_eq!(info.container, "mp4");
        assert!(info.header_complete);
        assert_eq!(info.duration_secs, Some(90.0));
        assert_eq!(info.video[0].codec, "hvc1");
        assert_eq!((info.video[0].width, info.video[0].height), (Some(3840), Some(2160)));
        assert!(Mp4Prober::probe_tail_moov(b"not a moov box").is_none());
    }

    #[test]
    fn repeated_mp4_stco_boxes_are_deduplicated_and_bounded() {
        let mut entry = vec![0u8; 36];
        entry[4..8].copy_from_slice(b"ec-3");
        entry[24..26].copy_from_slice(&6u16.to_be_bytes());
        entry[32..36].copy_from_slice(&(48000u32 << 16).to_be_bytes());
        let mut stsd = vec![0u8; 8];
        stsd.extend(&entry);

        // Build 256 repeated stco boxes each containing 64 duplicate zero offsets
        let mut stco_body = vec![0u8; 8 + 64 * 4];
        stco_body[4..8].copy_from_slice(&64u32.to_be_bytes());
        let stco = mp4_box(b"stco", &stco_body);
        let mut stbl_children = mp4_box(b"stsd", &stsd);
        for _ in 0..256 {
            stbl_children.extend_from_slice(&stco);
        }
        let stbl = mp4_box(b"stbl", &stbl_children);
        let mut hdlr = vec![0u8; 8];
        hdlr.extend(b"soun");
        hdlr.extend([0u8; 12]);
        let mdia = [mp4_box(b"hdlr", &hdlr), mp4_box(b"minf", &stbl)].concat();
        let moov = mp4_box(b"moov", &mp4_box(b"trak", &mp4_box(b"mdia", &mdia)));
        let file = [mp4_box(b"ftyp", b"isom"), moov, mp4_box(b"mdat", &vec![0u8; 65_536])].concat();

        let info = parse(&file).expect("mp4 with repeated stco must parse quickly");
        assert_eq!(info.audio.len(), 1);
        assert!(!info.audio[0].atmos);
    }

    #[test]
    fn atmos_is_validated_within_matching_audio_track_not_video_or_first_track() {
        // Track 1: Video (containing accidental 0x58 0x38 in its block)
        let t1 = [el(&[0xD7], &[1]), el(&[0x83], &[1]), el(&[0x86], b"V_MPEGH/ISO/HEVC")].concat();
        // Track 2: First audio track (stereo AC-3, non-Atmos)
        let t2 = [el(&[0xD7], &[2]), el(&[0x83], &[2]), el(&[0x86], b"A_AC3")].concat();
        // Track 3: Second audio track (E-AC-3 with Atmos frame)
        let t3 = [el(&[0xD7], &[3]), el(&[0x83], &[2]), el(&[0x86], b"A_EAC3")].concat();
        let tracks = [el(&[0xAE], &t1), el(&[0xAE], &t2), el(&[0xAE], &t3)].concat();

        // Cluster 1: only video block with 0x58 0x38 -> neither audio track should be marked Atmos
        let video_block = el(&[0xA3], &[0x81, 0x00, 0x00, 0x80, 0x11, 0x58, 0x38, 0x22]);
        let seg_video_only = [
            el(&[0x16, 0x54, 0xAE, 0x6B], &tracks),
            el(&[0x1F, 0x43, 0xB6, 0x75], &video_block),
        ]
        .concat();
        let mkv_video_only = [
            el(&[0x1A, 0x45, 0xDF, 0xA3], &el(&[0x42, 0x82], b"matroska")),
            el(&[0x18, 0x53, 0x80, 0x67], &seg_video_only),
        ]
        .concat();
        let info_no_atmos = parse(&mkv_video_only).unwrap();
        assert!(!info_no_atmos.audio[0].atmos);
        assert!(!info_no_atmos.audio[1].atmos);

        // Cluster 2: valid E-AC-3 Atmos syncframe on Track 3 (second audio track)
        // Sync 0x0B77, strmtyp=0, frmsiz=15 (32 bytes), fscod=0, bsid=16 (16 << 3 = 0x80),
        // EMDF 0x5838 + len=4 + version=0, key_id=0, payload_id=14 (0x03, 0x80) + 12-bit joc_header
        let mut eac3_frame = vec![0u8; 32];
        eac3_frame[0] = 0x0B;
        eac3_frame[1] = 0x77;
        eac3_frame[2] = 0x00;
        eac3_frame[3] = 15;
        eac3_frame[4] = 0x00;
        eac3_frame[5] = 16 << 3;
        eac3_frame[10..18].copy_from_slice(&[0x58, 0x38, 0x00, 0x04, 0x03, 0x80, 0x00, 0x00]);
        let mut audio_block_body = vec![0x83, 0x00, 0x00, 0x80]; // TrackNumber = 3
        audio_block_body.extend_from_slice(&eac3_frame);
        let cluster_with_atmos = [video_block, el(&[0xA3], &audio_block_body)].concat();
        let seg_with_atmos = [
            el(&[0x16, 0x54, 0xAE, 0x6B], &tracks),
            el(&[0x1F, 0x43, 0xB6, 0x75], &cluster_with_atmos),
        ]
        .concat();
        let mkv_with_atmos = [
            el(&[0x1A, 0x45, 0xDF, 0xA3], &el(&[0x42, 0x82], b"matroska")),
            el(&[0x18, 0x53, 0x80, 0x67], &seg_with_atmos),
        ]
        .concat();
        let info_atmos = parse(&mkv_with_atmos).unwrap();
        assert!(!info_atmos.audio[0].atmos, "first audio track must remain non-Atmos");
        assert!(info_atmos.audio[1].atmos, "second audio track must be marked Atmos");
    }

    #[test]
    fn dvcc_profile7_defers_fel_until_rpu_is_inspected() {
        let dv_box = [1, 0, 7 << 1, (6 << 3) | 0b011];
        let info = DolbyVisionInfo::from_dvcc_dvvc(&dv_box).expect("must parse dvvC");
        assert_eq!(info.profile, 7);
        assert_eq!(info.level, Some(6));
        assert!(info.el_present);
        assert!(!info.fel, "configuration record alone cannot classify MEL vs FEL");
    }

    #[test]
    fn a_flood_of_mkv_tags_is_cut_short_at_max_tags() {
        let mut tag_entries = Vec::new();
        for i in 0..(MAX_TAGS + 20) {
            let name = format!("TAG_{i}");
            let simple = [el(&[0x45, 0xA3], name.as_bytes()), el(&[0x44, 0x87], b"val")].concat();
            tag_entries.extend(el(&[0x73, 0x73], &el(&[0x67, 0xC8], &simple)));
        }
        let tags_elem = el(&[0x12, 0x54, 0xC3, 0x67], &tag_entries);
        let segment = el(&[0x18, 0x53, 0x80, 0x67], &tags_elem);
        let mkv = [el(&[0x1A, 0x45, 0xDF, 0xA3], &el(&[0x42, 0x82], b"matroska")), segment].concat();

        let info = parse(&mkv).expect("mkv with many tags must parse");
        assert_eq!(info.tags.len(), MAX_TAGS);
    }

    #[test]
    fn truehd_repeated_sync_words_scan_in_one_linear_pass() {
        let mut frame = [0u8; 24];
        frame[0..4].copy_from_slice(&[0xF8, 0x72, 0x6F, 0xBA]);
        let mut payload = frame.repeat(4_096);
        assert!(!AudioBitstream::has_truehd_atmos(&payload));
        let last = payload.len() - 24;
        // Bit 7 at +7 (channel assignment field) must NOT falsely trigger Atmos
        payload[last + 7] = 0x80;
        assert!(!AudioBitstream::has_truehd_atmos(&payload));
        // Bit 7 at +17 (substream_info 4th substream flag) must trigger Atmos
        payload[last + 17] = 0x80;
        assert!(AudioBitstream::has_truehd_atmos(&payload));
    }

    #[test]
    fn eac3_repeated_sync_words_in_4mib_scan_in_linear_time() {
        // 4 MiB of repeated [0x0B, 0x77, 0x07, 0xFF, 0x00, 0x80] (frmsiz=2047 -> 4096-byte frame, bsid=16)
        let pattern = [0x0B, 0x77, 0x07, 0xFF, 0x00, 0x80];
        let mut payload = pattern.repeat(4 * 1024 * 1024 / pattern.len());
        let start = std::time::Instant::now();
        assert!(!AudioBitstream::has_eac3_atmos_frame(&payload));
        assert!(
            start.elapsed() < std::time::Duration::from_millis(250),
            "4 MiB repeated E-AC-3 sync word scan must complete in linear time, took {:?}",
            start.elapsed()
        );
        let tail = payload.len() - 16;
        // Coincidental [0x58, 0x38, 0x00, 0x00] (zero container length) must NOT report Atmos
        payload[tail..tail + 4].copy_from_slice(&[0x58, 0x38, 0x00, 0x00]);
        assert!(!AudioBitstream::has_eac3_atmos_frame(&payload));
        // Non-JOC EMDF container (payload_id=11 -> 0x02, 0xC0) must NOT report Atmos
        payload[tail..tail + 8].copy_from_slice(&[0x58, 0x38, 0x00, 0x04, 0x02, 0xC0, 0x00, 0x00]);
        assert!(!AudioBitstream::has_eac3_atmos_frame(&payload));
        // Valid JOC EMDF container (len=4, version=0, key_id=0, payload_id=14 -> 0x03, 0x80 + 12b joc_header)
        payload[tail..tail + 8].copy_from_slice(&[0x58, 0x38, 0x00, 0x04, 0x03, 0x80, 0x00, 0x00]);
        assert!(AudioBitstream::has_eac3_atmos_frame(&payload));
    }

    #[test]
    fn mkv_simpleblock_length_prefixed_hevc_nal62_rpu_upgrades_dvcc_profile7_fel() {
        // Track 1: HEVC with dvvC BlockAdditionMapping (Profile 7, Level 6, el_present=1, fel initially false)
        let dvvc = [1u8, 0, 7 << 1, (6 << 3) | 0b111];
        let mapping = el(&[0x41, 0xE4], &el(&[0x41, 0xED], &dvvc));
        // Minimal 23-byte hvcC with lengthSizeMinusOne = 3 (4-byte NAL length prefixes) at byte 21
        let mut hvcc = [0u8; 23];
        hvcc[0] = 1;
        hvcc[21] = 0xFC | 0x03;
        let t1 = [
            el(&[0xD7], &[1]),
            el(&[0x83], &[1]),
            el(&[0x86], b"V_MPEGH/ISO/HEVC"),
            el(&[0x63, 0xA2], &hvcc),
            mapping,
        ]
        .concat();
        let tracks = el(&[0xAE], &t1);

        // Build a SimpleBlock for Track 1 containing two 4-byte length-prefixed HEVC NAL units:
        // 1. IDR slice NAL (type 19 -> header 0x26, 0x01) of 6 bytes
        // 2. Dolby Vision RPU NAL (type 62 -> header 0x7C, 0x01) with 0x19 prefix + Profile 7 FEL + CM v4.0
        let push_bits = |bits: &mut Vec<bool>, val: u32, count: u32| {
            for i in (0..count).rev() {
                bits.push(((val >> i) & 1) != 0);
            }
        };
        let push_ue = |bits: &mut Vec<bool>, val: u32| {
            let v = val + 1;
            let lz = 31 - v.leading_zeros();
            for _ in 0..lz {
                bits.push(false);
            }
            push_bits(bits, v, lz + 1);
        };
        let mut rpu_bits = Vec::new();
        push_bits(&mut rpu_bits, 0x19, 8); // rpu_nal_prefix = 25
        push_bits(&mut rpu_bits, 2, 6); // rpu_type = 2
        push_bits(&mut rpu_bits, 18, 11); // rpu_format = 18
        push_bits(&mut rpu_bits, 1, 4); // vdr_rpu_profile = 1
        push_bits(&mut rpu_bits, 6, 4); // vdr_rpu_level = 6
        rpu_bits.push(true); // vdr_seq_info_present_flag
        rpu_bits.push(false); // chroma_resampling_explicit_filter_flag
        push_bits(&mut rpu_bits, 0, 2); // coefficient_data_type = 0
        push_ue(&mut rpu_bits, 0); // coefficient_log2_denom = 0
        push_bits(&mut rpu_bits, 1, 2); // vdr_rpu_normalized_idc = 1
        rpu_bits.push(false); // bl_video_full_range_flag = false
        push_ue(&mut rpu_bits, 2); // bl_bit_depth_minus8 = 2
        push_ue(&mut rpu_bits, 2); // el_bit_depth_minus8 = 2
        push_ue(&mut rpu_bits, 4); // vdr_bit_depth_minus8 = 4 (Profile 7)
        rpu_bits.push(false); // spatial_resampling_filter_flag
        push_bits(&mut rpu_bits, 1, 3); // reserved_zero_3bits = 1 (compressed_dm_data)
        rpu_bits.push(true); // el_spatial_resampling_filter_flag = true
        rpu_bits.push(false); // disable_residual_flag = false
        rpu_bits.push(true); // vdr_dm_metadata_present_flag = true
        rpu_bits.push(false); // use_prev_vdr_rpu_flag = false
        // RpuDataMapping
        push_ue(&mut rpu_bits, 0); // vdr_rpu_id
        push_ue(&mut rpu_bits, 0); // mapping_color_space
        push_ue(&mut rpu_bits, 0); // mapping_chroma_format_idc
        for _ in 0..3 {
            push_ue(&mut rpu_bits, 0); // num_pivots_minus2 = 0
            push_bits(&mut rpu_bits, 0, 10);
            push_bits(&mut rpu_bits, 1023, 10);
        }
        push_bits(&mut rpu_bits, 0, 3); // nlq_method_idc = 0
        push_bits(&mut rpu_bits, 0, 10); // nlq_pred_pivot_value[0]
        push_bits(&mut rpu_bits, 1023, 10); // nlq_pred_pivot_value[1]
        push_ue(&mut rpu_bits, 0); // num_x_partitions_minus1
        push_ue(&mut rpu_bits, 0); // num_y_partitions_minus1
        for _ in 0..3 {
            push_ue(&mut rpu_bits, 0); // mapping_idc = 0 (Polynomial)
            push_ue(&mut rpu_bits, 0); // poly_order_minus1 = 0
            rpu_bits.push(false); // linear_interp_flag = false
            push_ue(&mut rpu_bits, 0); // poly_coef_int[0] = 0 (se(0) == ue(0))
            push_ue(&mut rpu_bits, 1); // poly_coef_int[1] = 1 (se(1) == ue(1))
        }
        // RpuDataNlq (FEL: non-zero nlq_offset)
        for _ in 0..3 {
            push_bits(&mut rpu_bits, 512, 10); // nlq_offset = 512 (non-MEL -> FEL)
            push_ue(&mut rpu_bits, 1); // vdr_in_max_int = 1
            push_ue(&mut rpu_bits, 1); // linear_deadzone_slope_int = 1
            push_ue(&mut rpu_bits, 0); // linear_deadzone_threshold_int = 0
        }
        // VdrDmData (compressed_dm_data = true + 1 CM v4.0 Level 254 block + CRC32 + 0x80)
        push_ue(&mut rpu_bits, 0); // affected_dm_metadata_id
        push_ue(&mut rpu_bits, 0); // current_dm_metadata_id
        push_ue(&mut rpu_bits, 0); // scene_refresh_flag
        push_ue(&mut rpu_bits, 0); // cmv29 num_ext_blocks = 0
        while rpu_bits.len() % 8 != 0 {
            rpu_bits.push(false);
        }
        push_ue(&mut rpu_bits, 1); // cmv40 num_ext_blocks = 1
        while rpu_bits.len() % 8 != 0 {
            rpu_bits.push(false);
        }
        push_ue(&mut rpu_bits, 2); // ext_block_length = 2 bytes
        push_bits(&mut rpu_bits, 254, 8); // ext_block_level = 254 (CM v4.0)
        push_bits(&mut rpu_bits, 0x0001, 16); // Level 254 payload
        while rpu_bits.len() % 8 != 0 {
            rpu_bits.push(false);
        }
        push_bits(&mut rpu_bits, 0x1234_5678, 32); // CRC32
        push_bits(&mut rpu_bits, 0x80, 8); // 0x80 terminator
        let mut rpu_nal = vec![0x7C, 0x01];
        for chunk in rpu_bits.chunks(8) {
            let mut b = 0u8;
            for (idx, &bit) in chunk.iter().enumerate() {
                if bit {
                    b |= 1 << (7 - idx);
                }
            }
            rpu_nal.push(b);
        }
        let idr_nal = [0x26u8, 0x01, 0xAA, 0xBB, 0xCC, 0xDD];

        let mut simple_block_body = vec![0x81, 0x00, 0x00, 0x80]; // Track 1, pts=0, keyframe, no lacing
        simple_block_body.extend_from_slice(&(idr_nal.len() as u32).to_be_bytes());
        simple_block_body.extend_from_slice(&idr_nal);
        simple_block_body.extend_from_slice(&(rpu_nal.len() as u32).to_be_bytes());
        simple_block_body.extend_from_slice(&rpu_nal);

        let cluster = el(&[0x1F, 0x43, 0xB6, 0x75], &el(&[0xA3], &simple_block_body));
        let segment = el(
            &[0x18, 0x53, 0x80, 0x67],
            &[el(&[0x16, 0x54, 0xAE, 0x6B], &tracks), cluster].concat(),
        );
        let mkv = [el(&[0x1A, 0x45, 0xDF, 0xA3], &el(&[0x42, 0x82], b"matroska")), segment].concat();

        let info = parse(&mkv).expect("mkv with length-prefixed HEVC SimpleBlock must parse");
        assert_eq!(info.video.len(), 1);
        let dv = info.video[0].dolby_vision.as_ref().expect("Dolby Vision info must be present");
        assert_eq!(dv.profile, 7);
        assert_eq!(dv.level, Some(6));
        assert!(dv.el_present);
        assert!(dv.fel, "in-band length-prefixed RPU must upgrade Profile 7 FEL flag");
        assert_eq!(dv.cm_version, "v4.0");
        assert!(dv.from_rpu, "a fully read RPU says its FEL and CM facts are read");
    }

    #[test]
    fn interleaved_mp4_short_audio_chunks_do_not_bleed_atmos_across_tracks() {
        // Track 1: 32-byte non-Atmos E-AC-3 frame at mdat offset 0
        let mut non_atmos_frame = vec![0u8; 32];
        non_atmos_frame[0] = 0x0B;
        non_atmos_frame[1] = 0x77;
        non_atmos_frame[2] = 0x00;
        non_atmos_frame[3] = 15; // frmsiz=15 -> 32 bytes
        non_atmos_frame[4] = 0x00;
        non_atmos_frame[5] = 16 << 3; // bsid=16

        // Immediately following at mdat offset 32: 32-byte Atmos E-AC-3 frame
        let mut atmos_frame = non_atmos_frame.clone();
        atmos_frame[10..18].copy_from_slice(&[0x58, 0x38, 0x00, 0x04, 0x03, 0x80, 0x00, 0x00]);

        let mut mdat_payload = Vec::new();
        mdat_payload.extend_from_slice(&non_atmos_frame);
        mdat_payload.extend_from_slice(&atmos_frame);

        let ftyp = mp4_box(b"ftyp", b"isom");

        // Build an audio trak helper with stsd + stsc (1 sample/chunk) + stsz (sample_size = 32) + stco
        let make_audio_trak = |chunk_off: u32| -> Vec<u8> {
            let mut entry = vec![0u8; 36];
            entry[4..8].copy_from_slice(b"ec-3");
            entry[24..26].copy_from_slice(&6u16.to_be_bytes());
            entry[32..36].copy_from_slice(&(48000u32 << 16).to_be_bytes());
            let mut stsd = vec![0u8; 8];
            stsd.extend(&entry);

            let mut stsc = vec![0u8; 8];
            stsc[4..8].copy_from_slice(&1u32.to_be_bytes());
            stsc.extend_from_slice(&1u32.to_be_bytes()); // first_chunk = 1
            stsc.extend_from_slice(&1u32.to_be_bytes()); // samples_per_chunk = 1
            stsc.extend_from_slice(&1u32.to_be_bytes()); // sample_description_index = 1

            let mut stsz = vec![0u8; 12];
            stsz[4..8].copy_from_slice(&32u32.to_be_bytes()); // sample_size = 32
            stsz[8..12].copy_from_slice(&1u32.to_be_bytes()); // sample_count = 1

            let mut stco = vec![0u8; 12];
            stco[4..8].copy_from_slice(&1u32.to_be_bytes());
            stco[8..12].copy_from_slice(&chunk_off.to_be_bytes());

            let stbl = mp4_box(
                b"stbl",
                &[
                    mp4_box(b"stsd", &stsd),
                    mp4_box(b"stsc", &stsc),
                    mp4_box(b"stsz", &stsz),
                    mp4_box(b"stco", &stco),
                ]
                .concat(),
            );
            let mut hdlr = vec![0u8; 8];
            hdlr.extend(b"soun");
            hdlr.extend([0u8; 12]);
            let mdia = [mp4_box(b"hdlr", &hdlr), mp4_box(b"minf", &stbl)].concat();
            mp4_box(b"trak", &mp4_box(b"mdia", &mdia))
        };

        // Dummy placeholder so we can compute exact file offsets for mdat
        let placeholder_moov = mp4_box(b"moov", &[make_audio_trak(0), make_audio_trak(0)].concat());
        let mdat_data_start = (ftyp.len() + placeholder_moov.len() + 8) as u32;
        let moov = mp4_box(
            b"moov",
            &[
                make_audio_trak(mdat_data_start),
                make_audio_trak(mdat_data_start + 32),
            ]
            .concat(),
        );
        let file = [ftyp, moov, mp4_box(b"mdat", &mdat_payload)].concat();

        let info = parse(&file).expect("interleaved mp4 must parse");
        assert_eq!(info.audio.len(), 2);
        assert!(!info.audio[0].atmos, "first non-Atmos track must not read into adjacent track's chunk");
        assert!(info.audio[1].atmos, "second Atmos track must be detected");
    }

    #[test]
    fn container_mkv_and_mp4_probe_dtsx_audio_tracks() {
        // Matroska with A_DTS audio track and SimpleBlock containing DTS:X sync word 0x02000850
        let t_audio = [
            el(&[0xD7], &[1]),
            el(&[0x83], &[2]),
            el(&[0x86], b"A_DTS"),
        ]
        .concat();
        let tracks = el(&[0xAE], &t_audio);
        let mut block = vec![0x81, 0x00, 0x00, 0x80];
        block.extend_from_slice(&[0x00, 0x00, 0x02, 0x00, 0x08, 0x50, 0x00, 0x00]);
        let cluster = el(&[0x1F, 0x43, 0xB6, 0x75], &el(&[0xA3], &block));
        let seg = el(
            &[0x18, 0x53, 0x80, 0x67],
            &[el(&[0x16, 0x54, 0xAE, 0x6B], &tracks), cluster].concat(),
        );
        let mkv = [el(&[0x1A, 0x45, 0xDF, 0xA3], &el(&[0x42, 0x82], b"matroska")), seg].concat();

        let mkv_probe = AudioBitstream::probe(&mkv);
        assert!(mkv_probe.has_dtsx, "Matroska A_DTS SimpleBlock with DTS:X sync must set has_dtsx");
        assert_eq!(mkv_probe.format.as_deref(), Some("DTS:X"));
        assert!(AudioBitstream::probe_dtsx(&mkv));

        // MP4 with dtsh audio track and mdat chunk containing DTS:X sync word 0x02000850
        let ftyp = mp4_box(b"ftyp", b"isom");
        let make_dts_trak = |chunk_off: u32| -> Vec<u8> {
            let mut entry = vec![0u8; 36];
            entry[4..8].copy_from_slice(b"dtsh");
            entry[24..26].copy_from_slice(&8u16.to_be_bytes());
            entry[32..36].copy_from_slice(&(48000u32 << 16).to_be_bytes());
            let mut stsd = vec![0u8; 8];
            stsd.extend(&entry);
            let mut stco = vec![0u8; 12];
            stco[4..8].copy_from_slice(&1u32.to_be_bytes());
            stco[8..12].copy_from_slice(&chunk_off.to_be_bytes());
            let stbl = mp4_box(b"stbl", &[mp4_box(b"stsd", &stsd), mp4_box(b"stco", &stco)].concat());
            let mut hdlr = vec![0u8; 8];
            hdlr.extend(b"soun");
            hdlr.extend([0u8; 12]);
            let mdia = [mp4_box(b"hdlr", &hdlr), mp4_box(b"minf", &stbl)].concat();
            mp4_box(b"trak", &mp4_box(b"mdia", &mdia))
        };
        let placeholder_moov = mp4_box(b"moov", &make_dts_trak(0));
        let mdat_data_start = (ftyp.len() + placeholder_moov.len() + 8) as u32;
        let moov = mp4_box(b"moov", &make_dts_trak(mdat_data_start));
        let mdat = mp4_box(b"mdat", &[0x00, 0x00, 0x02, 0x00, 0x08, 0x50, 0x00, 0x00]);
        let mp4 = [ftyp, moov, mdat].concat();

        let mp4_probe = AudioBitstream::probe(&mp4);
        assert!(mp4_probe.has_dtsx, "MP4 dtsh chunk with DTS:X sync must set has_dtsx");
        assert_eq!(mp4_probe.format.as_deref(), Some("DTS:X"));
        assert!(AudioBitstream::probe_dtsx(&mp4));
    }
}
