//! Tracks and format from the start of an ASF file (WMV, WMA): its header object comes first, before the data
//! packets. A total function like the rest of `media`: cut off bytes give what was read before them.

use super::media::{AudioTrack, MAX_TRACK_TEXT, MediaInfo, VideoTrack, is_full};

/// The objects' GUIDs as they are on disk: the first three fields little-endian.
const HEADER: [u8; 16] = guid(0x75B2_2630, 0x668E, 0x11CF, [0xA6, 0xD9, 0x00, 0xAA, 0x00, 0x62, 0xCE, 0x6C]);
const FILE_PROPERTIES: [u8; 16] = guid(0x8CAB_DCA1, 0xA947, 0x11CF, [0x8E, 0xE4, 0x00, 0xC0, 0x0C, 0x20, 0x53, 0x65]);
const STREAM_PROPERTIES: [u8; 16] = guid(0xB7DC_0791, 0xA9B7, 0x11CF, [0x8E, 0xE6, 0x00, 0xC0, 0x0C, 0x20, 0x53, 0x65]);
const CONTENT_DESCRIPTION: [u8; 16] =
    guid(0x75B2_2633, 0x668E, 0x11CF, [0xA6, 0xD9, 0x00, 0xAA, 0x00, 0x62, 0xCE, 0x6C]);
const HEADER_EXTENSION: [u8; 16] = guid(0x5FBF_03B5, 0xA92E, 0x11CF, [0x8E, 0xE3, 0x00, 0xC0, 0x0C, 0x20, 0x53, 0x65]);
const EXTENDED_STREAM_PROPERTIES: [u8; 16] =
    guid(0x14E6_A5CB, 0xC672, 0x4332, [0x83, 0x99, 0xA9, 0x69, 0x52, 0x06, 0x5B, 0x5A]);
const LANGUAGE_LIST: [u8; 16] = guid(0x7C43_46A9, 0xEFE0, 0x4BFC, [0xB2, 0x29, 0x39, 0x3E, 0xDE, 0x41, 0x5C, 0x85]);
const AUDIO_MEDIA: [u8; 16] = guid(0xF869_9E40, 0x5B4D, 0x11CF, [0xA8, 0xFD, 0x00, 0x80, 0x5F, 0x5C, 0x44, 0x2B]);
const VIDEO_MEDIA: [u8; 16] = guid(0xBC19_EFC0, 0x5B4D, 0x11CF, [0xA8, 0xFD, 0x00, 0x80, 0x5F, 0x5C, 0x44, 0x2B]);

const fn guid(a: u32, b: u16, c: u16, d: [u8; 8]) -> [u8; 16] {
    let (a, b, c) = (a.to_le_bytes(), b.to_le_bytes(), c.to_le_bytes());
    [a[0], a[1], a[2], a[3], b[0], b[1], c[0], c[1], d[0], d[1], d[2], d[3], d[4], d[5], d[6], d[7]]
}

pub fn is_asf(data: &[u8]) -> bool {
    data.len() >= 30 && data[..16] == HEADER
}

pub fn asf(data: &[u8]) -> MediaInfo {
    let mut info = MediaInfo { container: "asf".into(), ..MediaInfo::default() };
    let header_size = le64(data, 16).unwrap_or(0);
    info.header_complete = header_size >= 30 && header_size <= data.len() as u64;
    let end = usize::try_from(header_size).unwrap_or(usize::MAX).min(data.len());
    // per stream number: its track, and the language list's index for it
    let mut streams: Vec<(u16, Track)> = Vec::new();
    let mut ext: Vec<(u16, Option<u64>, Option<u16>)> = Vec::new();
    let mut languages: Vec<String> = Vec::new();
    let mut duration = None;
    objects(data.get(30..end).unwrap_or_default(), &mut |id, body| match *id {
        FILE_PROPERTIES => {
            // file id 16, file size 8, creation date 8, packets 8, then play duration (100 ns) and preroll (ms)
            let play = le64(body, 40);
            let preroll = le64(body, 56).unwrap_or(0);
            duration = play.map(|p| (p as f64 / 1e7 - preroll as f64 / 1e3).max(0.0));
        }
        STREAM_PROPERTIES => {
            if streams.len() < super::media::MAX_TRACKS
                && let Some(s) = stream(body)
            {
                streams.push(s);
            }
        }
        CONTENT_DESCRIPTION => info.title = content_title(body),
        HEADER_EXTENSION => {
            // reserved GUID 16, reserved 2, data size 4, then objects
            objects(body.get(22..).unwrap_or_default(), &mut |id, body| match *id {
                EXTENDED_STREAM_PROPERTIES => {
                    if ext.len() < super::media::MAX_TRACKS {
                        ext.push(extended(body));
                    }
                }
                LANGUAGE_LIST => languages = language_list(body),
                _ => {}
            });
        }
        _ => {}
    });
    for (number, track) in streams {
        let more = ext.iter().find(|(n, _, _)| *n == number);
        let language = more.and_then(|(_, _, l)| *l).and_then(|l| languages.get(usize::from(l))).cloned();
        match track {
            Track::Video(mut v) => {
                if let Some(per_frame) = more.and_then(|(_, t, _)| *t).filter(|t| *t > 0) {
                    v.frame_rate = Some(1e7 / per_frame as f64);
                }
                v.language = language;
                if !is_full(&info) {
                    info.video.push(v);
                }
            }
            Track::Audio(mut a) => {
                a.language = language;
                if !is_full(&info) {
                    info.audio.push(a);
                }
            }
        }
    }
    info.duration_secs = duration.filter(|d| *d > 0.0);
    info
}

enum Track {
    Video(VideoTrack),
    Audio(AudioTrack),
}

/// A Stream Properties object: its kind, error correction kind, time offset 8, the two data sizes, flags, reserved
/// 4, then the kind's own data.
fn stream(b: &[u8]) -> Option<(u16, Track)> {
    let kind: [u8; 16] = b.get(..16)?.try_into().ok()?;
    let specific_len = le32(b, 40)? as usize;
    let number = le16(b, 48)? & 0x7f;
    let specific = b.get(54..54usize.saturating_add(specific_len))?;
    match kind {
        AUDIO_MEDIA => {
            let tag = le16(specific, 0)?;
            let channels = le16(specific, 2).map(u64::from).filter(|c| *c > 0);
            let sample_rate = le32(specific, 4).filter(|r| *r > 0).map(f64::from);
            Some((
                number,
                Track::Audio(AudioTrack {
                    codec: super::avi::audio_codec(tag),
                    channels,
                    sample_rate,
                    ..AudioTrack::default()
                }),
            ))
        }
        VIDEO_MEDIA => {
            // width 4, height 4, flags 1, format size 2, then a BITMAPINFOHEADER
            let (w, h) = (le32(specific, 0)?, le32(specific, 4)?);
            let codec = specific.get(27..31).and_then(super::avi::fourcc).unwrap_or_default();
            Some((
                number,
                Track::Video(VideoTrack {
                    codec,
                    width: (w > 0).then_some(u64::from(w)),
                    height: (h > 0).then_some(u64::from(h)),
                    ..VideoTrack::default()
                }),
            ))
        }
        _ => None,
    }
}

/// An Extended Stream Properties object's stream number, time per frame (100 ns) and language index.
fn extended(b: &[u8]) -> (u16, Option<u64>, Option<u16>) {
    (le16(b, 48).unwrap_or(0), le64(b, 52), le16(b, 50))
}

/// The Language List: a count, then each as a byte length and UTF-16.
fn language_list(b: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut at = 2usize;
    for _ in 0..le16(b, 0).unwrap_or(0).min(super::media::MAX_TRACKS as u16) {
        let Some(&len) = b.get(at) else { break };
        let Some(text) = b.get(at + 1..at + 1 + usize::from(len)) else { break };
        out.push(utf16(text).unwrap_or_default());
        at += 1 + usize::from(len);
    }
    out
}

/// The Content Description's title: five lengths, then the title first, as UTF-16.
fn content_title(b: &[u8]) -> Option<String> {
    let len = usize::from(le16(b, 0)?);
    utf16(b.get(10..10 + len)?)
}

fn utf16(b: &[u8]) -> Option<String> {
    let units: Vec<u16> = b.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).take_while(|u| *u != 0).collect();
    let s = String::from_utf16_lossy(&units).trim().chars().take(MAX_TRACK_TEXT).collect::<String>();
    (!s.is_empty()).then_some(s)
}

/// ASF objects in `data`: a GUID, a size counting those 24 bytes, the body. A cut one is handed over as far as it goes.
fn objects<'a>(data: &'a [u8], f: &mut dyn FnMut(&[u8; 16], &'a [u8])) {
    let mut at = 0usize;
    while at + 24 <= data.len() {
        let Ok(id) = <&[u8; 16]>::try_from(&data[at..at + 16]) else { return };
        let Some(size) = le64(data, at + 16).and_then(|s| usize::try_from(s).ok()).filter(|s| *s >= 24) else { return };
        let end = at.saturating_add(size);
        f(id, &data[at + 24..end.min(data.len())]);
        // a cut one is the last
        if end >= data.len() {
            return;
        }
        at = end;
    }
}

fn le16(b: &[u8], at: usize) -> Option<u16> {
    b.get(at..at + 2).map(|x| u16::from_le_bytes([x[0], x[1]]))
}

fn le32(b: &[u8], at: usize) -> Option<u32> {
    b.get(at..at + 4).map(|x| u32::from_le_bytes([x[0], x[1], x[2], x[3]]))
}

fn le64(b: &[u8], at: usize) -> Option<u64> {
    b.get(at..at + 8).map(|x| u64::from_le_bytes(x.try_into().unwrap_or([0; 8])))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn object(id: [u8; 16], body: &[u8]) -> Vec<u8> {
        let mut out = id.to_vec();
        out.extend((body.len() as u64 + 24).to_le_bytes());
        out.extend(body);
        out
    }

    fn utf16z(s: &str) -> Vec<u8> {
        s.encode_utf16().chain([0]).flat_map(u16::to_le_bytes).collect()
    }

    fn stream_props(kind: [u8; 16], number: u16, specific: &[u8]) -> Vec<u8> {
        let mut b = kind.to_vec();
        b.extend([0u8; 16]);
        b.extend(0u64.to_le_bytes());
        b.extend((specific.len() as u32).to_le_bytes());
        b.extend(0u32.to_le_bytes());
        b.extend(number.to_le_bytes());
        b.extend(0u32.to_le_bytes());
        b.extend(specific);
        object(STREAM_PROPERTIES, &b)
    }

    /// A 720x480 WMV9 at 29.97 fps with 2-channel WMA at 44.1 kHz in English, 95 s long, the way Windows Media
    /// Encoder writes one.
    fn sample() -> Vec<u8> {
        let mut file = vec![0u8; 40];
        file.extend(950_000_000u64.to_le_bytes());
        file.extend(0u64.to_le_bytes());
        file.extend(3_000u64.to_le_bytes());
        file.extend([0u8; 20]);
        let mut video = Vec::new();
        video.extend(720u32.to_le_bytes());
        video.extend(480u32.to_le_bytes());
        video.push(2);
        video.extend(40u16.to_le_bytes());
        video.extend(40u32.to_le_bytes());
        video.extend(720u32.to_le_bytes());
        video.extend(480u32.to_le_bytes());
        video.extend([1, 0, 24, 0]);
        video.extend(b"WMV3");
        video.extend([0u8; 20]);
        let mut audio = vec![0x61, 0x01, 2, 0];
        audio.extend(44_100u32.to_le_bytes());
        audio.extend([0u8; 10]);
        let mut ext_video = vec![0u8; 48];
        ext_video.extend(1u16.to_le_bytes());
        ext_video.extend(0u16.to_le_bytes());
        ext_video.extend(333_667u64.to_le_bytes());
        ext_video.extend([0u8; 4]);
        let mut ext_audio = vec![0u8; 48];
        ext_audio.extend(2u16.to_le_bytes());
        ext_audio.extend(0u16.to_le_bytes());
        ext_audio.extend(0u64.to_le_bytes());
        ext_audio.extend([0u8; 4]);
        let en = utf16z("en-us");
        let mut langs = 1u16.to_le_bytes().to_vec();
        langs.push(en.len() as u8);
        langs.extend(&en);
        let mut extension = vec![0u8; 18];
        let inner = [
            object(EXTENDED_STREAM_PROPERTIES, &ext_video),
            object(EXTENDED_STREAM_PROPERTIES, &ext_audio),
            object(LANGUAGE_LIST, &langs),
        ]
        .concat();
        extension.extend((inner.len() as u32).to_le_bytes());
        extension.extend(inner);
        let title = utf16z("Some Show");
        let mut description = (title.len() as u16).to_le_bytes().to_vec();
        description.extend([0u8; 8]);
        description.extend(&title);
        let objects = [
            object(FILE_PROPERTIES, &file),
            stream_props(VIDEO_MEDIA, 1, &video),
            stream_props(AUDIO_MEDIA, 2, &audio),
            object(HEADER_EXTENSION, &extension),
            object(CONTENT_DESCRIPTION, &description),
        ]
        .concat();
        let mut out = HEADER.to_vec();
        out.extend((objects.len() as u64 + 30).to_le_bytes());
        out.extend(5u32.to_le_bytes());
        out.extend([1, 2]);
        out.extend(objects);
        out.extend([0u8; 100]);
        out
    }

    #[test]
    fn an_asf_header_gives_its_tracks() {
        let data = sample();
        let info = super::super::media::parse(&data).unwrap();
        assert_eq!(info.container, "asf");
        assert!(info.header_complete);
        assert_eq!(info.title.as_deref(), Some("Some Show"));
        let v = &info.video[0];
        assert_eq!((v.codec.as_str(), v.width, v.height), ("WMV3", Some(720), Some(480)));
        assert!((v.frame_rate.unwrap() - 29.97).abs() < 0.01, "{:?}", v.frame_rate);
        assert_eq!(v.language.as_deref(), Some("en-us"));
        let a = &info.audio[0];
        assert_eq!((a.codec.as_str(), a.channels, a.sample_rate), ("wma", Some(2), Some(44_100.0)));
        assert_eq!(info.duration_secs, Some(92.0));
    }

    #[test]
    fn a_cut_or_endless_header_is_safe() {
        let data = sample();
        for cut in [16, 30, 100, 200, data.len() - 120] {
            let info = asf(&data[..cut.max(30).min(data.len())]);
            assert!(!info.header_complete || cut >= data.len() - 100);
        }
        // a million stream objects keep media's few
        let one = stream_props(AUDIO_MEDIA, 1, &[0x61, 0x01, 2, 0, 0x44, 0xac, 0, 0]);
        let many: Vec<u8> = one.iter().copied().cycle().take(one.len() * 10_000).collect();
        let mut data = HEADER.to_vec();
        data.extend((many.len() as u64 + 30).to_le_bytes());
        data.extend([0u8; 6]);
        data.extend(many);
        assert_eq!(asf(&data).audio.len(), super::super::media::MAX_TRACKS);
        // a child object claiming to be as big as can be ends the walk
        let mut huge = HEADER.to_vec();
        huge.extend(200u64.to_le_bytes());
        huge.extend([0u8; 6]);
        huge.extend(FILE_PROPERTIES);
        huge.extend(u64::MAX.to_le_bytes());
        huge.extend(STREAM_PROPERTIES);
        huge.extend(u64::MAX.to_le_bytes());
        huge.resize(200, 0);
        assert!(asf(&huge).video.is_empty());
    }
}
