//! Tracks and format from the start of an AVI file: its `hdrl` list comes first, before the frames. A total
//! function like the rest of `media`: cut off bytes give what was read before them.

use super::media::{AudioTrack, MAX_TRACK_TEXT, MediaInfo, SubtitleTrack, VideoTrack, is_full};

pub fn is_avi(data: &[u8]) -> bool {
    data.len() >= 12 && &data[..4] == b"RIFF" && &data[8..12] == b"AVI "
}

pub fn avi(data: &[u8]) -> MediaInfo {
    let mut info = MediaInfo { container: "avi".into(), ..MediaInfo::default() };
    let mut avih: Option<Avih> = None;
    let mut odml_frames = None;
    let mut video_length = None;
    chunks(&data[12..], &mut |id, body, whole| match id {
        b"LIST" if body.len() >= 4 && &body[..4] == b"hdrl" => {
            info.header_complete = whole;
            chunks(&body[4..], &mut |id, body, _| match id {
                b"avih" => avih = Avih::parse(body),
                b"LIST" if body.len() >= 4 && &body[..4] == b"strl" => {
                    stream(&body[4..], &mut info, &mut video_length);
                }
                b"LIST" if body.len() >= 4 && &body[..4] == b"odml" => {
                    chunks(&body[4..], &mut |id, body, _| {
                        if id == b"dmlh" {
                            odml_frames = le32(body, 0);
                        }
                    });
                }
                _ => {}
            });
        }
        b"LIST" if body.len() >= 4 && &body[..4] == b"INFO" => {
            chunks(&body[4..], &mut |id, body, _| {
                if id == b"ISFT" {
                    info.writing_app = text(body);
                }
            });
        }
        _ => {}
    });
    if let Some(h) = avih {
        let video = info.video.first_mut();
        if let Some(v) = video {
            v.width = v.width.or((h.width > 0).then_some(u64::from(h.width)));
            v.height = v.height.or((h.height > 0).then_some(u64::from(h.height)));
            if v.frame_rate.is_none() && h.micros_per_frame > 0 {
                v.frame_rate = Some(1e6 / f64::from(h.micros_per_frame));
            }
        }
        // a file past 1 GB is OpenDML: the main header counts the first RIFF's frames only
        let frames = odml_frames.or(video_length).unwrap_or(h.total_frames).max(h.total_frames);
        if frames > 0 && h.micros_per_frame > 0 {
            info.duration_secs = Some(f64::from(frames) * f64::from(h.micros_per_frame) / 1e6);
        }
    }
    info
}

struct Avih {
    micros_per_frame: u32,
    total_frames: u32,
    width: u32,
    height: u32,
}

impl Avih {
    fn parse(b: &[u8]) -> Option<Avih> {
        Some(Avih {
            micros_per_frame: le32(b, 0)?,
            total_frames: le32(b, 16)?,
            width: le32(b, 32)?,
            height: le32(b, 36)?,
        })
    }
}

/// One `strl`: its `strh` says what kind of stream, its `strf` the format, its `strn` a name.
fn stream(body: &[u8], info: &mut MediaInfo, video_length: &mut Option<u32>) {
    // these come from Usenet: a header of a million stream lists keeps what `media` keeps of a Matroska one
    if is_full(info) {
        return;
    }
    let (mut strh, mut strf, mut name) = (None, None, None);
    chunks(body, &mut |id, body, _| match id {
        b"strh" => strh = Some(body),
        b"strf" => strf = Some(body),
        b"strn" => name = text(body),
        _ => {}
    });
    let Some(strh) = strh.filter(|h| h.len() >= 32) else { return };
    let (scale, rate, length) = (le32(strh, 20).unwrap_or(0), le32(strh, 24).unwrap_or(0), le32(strh, 32));
    match &strh[..4] {
        b"vids" => {
            let handler = fourcc(&strh[4..8]);
            let mut v = VideoTrack { codec: handler.unwrap_or_default(), ..VideoTrack::default() };
            if let Some(f) = strf.filter(|f| f.len() >= 20) {
                let (w, h) = (le32(f, 4).unwrap_or(0) as i32, le32(f, 8).unwrap_or(0) as i32);
                v.width = (w != 0).then_some(u64::from(w.unsigned_abs()));
                v.height = (h != 0).then_some(u64::from(h.unsigned_abs()));
                // the bitmap header's compression is what the decoder goes by; the stream handler can be anything
                if let Some(c) = fourcc(&f[16..20]) {
                    v.codec = c;
                }
            }
            if scale > 0 && rate > 0 {
                v.frame_rate = Some(f64::from(rate) / f64::from(scale));
            }
            if info.video.is_empty() {
                *video_length = length;
            }
            info.video.push(v);
        }
        b"auds" => {
            let mut a = AudioTrack { name, ..AudioTrack::default() };
            if let Some(f) = strf.filter(|f| f.len() >= 8) {
                let tag = u16::from_le_bytes([f[0], f[1]]);
                a.codec = audio_codec(tag);
                a.channels = Some(u64::from(u16::from_le_bytes([f[2], f[3]]))).filter(|c| *c > 0);
                a.sample_rate = le32(f, 4).filter(|r| *r > 0).map(f64::from);
            }
            info.audio.push(a);
        }
        b"txts" => info.subtitles.push(SubtitleTrack { codec: "text".into(), name, ..SubtitleTrack::default() }),
        _ => {}
    }
}

/// WAVEFORMATEX format tags as the codec names the other containers' tracks use.
pub(super) fn audio_codec(tag: u16) -> String {
    match tag {
        0x0001 => "pcm".into(),
        0x0050 => "mp2".into(),
        0x0055 => "mp3".into(),
        0x00ff | 0x1610 | 0x706d => "aac".into(),
        0x0160 | 0x0161 => "wma".into(),
        0x0162 => "wma pro".into(),
        0x0163 => "wma lossless".into(),
        0x2000 => "ac3".into(),
        0x2001 => "dts".into(),
        0x674f | 0x6750 | 0x6751 | 0x676f | 0x6770 | 0x6771 => "vorbis".into(),
        other => format!("0x{other:04x}"),
    }
}

/// What `chunks` hands each chunk to: its id, its body, and whether it's all there.
type OnChunk<'a, 'f> = dyn FnMut(&[u8; 4], &'a [u8], bool) + 'f;

/// RIFF chunks in `data`, each with whether it's all there. A cut one is handed over as far as it goes.
fn chunks<'a>(data: &'a [u8], f: &mut OnChunk<'a, '_>) {
    let mut at = 0usize;
    while at + 8 <= data.len() {
        let id: &[u8; 4] = data[at..at + 4].try_into().unwrap_or(&[0; 4]);
        let Some(size) = le32(data, at + 4) else { return };
        let start = at + 8;
        let end = start.saturating_add(size as usize);
        let whole = end <= data.len();
        f(id, &data[start..end.min(data.len())], whole);
        if !whole {
            return;
        }
        // chunks are padded to an even size
        at = end + (size as usize & 1);
    }
}

fn le32(b: &[u8], at: usize) -> Option<u32> {
    b.get(at..at + 4).map(|x| u32::from_le_bytes([x[0], x[1], x[2], x[3]]))
}

pub(super) fn fourcc(b: &[u8]) -> Option<String> {
    let s: String = b.iter().take_while(|c| **c != 0).map(|c| char::from(*c)).collect();
    let s = s.trim().to_string();
    (!s.is_empty() && s.chars().all(|c| c.is_ascii_graphic())).then_some(s)
}

fn text(b: &[u8]) -> Option<String> {
    let s = String::from_utf8_lossy(b.split(|c| *c == 0).next().unwrap_or_default()).trim().to_string();
    (!s.is_empty()).then(|| s.chars().take(MAX_TRACK_TEXT).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(id: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut out = id.to_vec();
        out.extend((body.len() as u32).to_le_bytes());
        out.extend(body);
        if body.len() % 2 == 1 {
            out.push(0);
        }
        out
    }

    fn list(kind: &[u8; 4], parts: &[Vec<u8>]) -> Vec<u8> {
        let mut body = kind.to_vec();
        for p in parts {
            body.extend(p);
        }
        chunk(b"LIST", &body)
    }

    fn le(n: u32) -> [u8; 4] {
        n.to_le_bytes()
    }

    /// A 640x272 Xvid at 23.976 fps, 1,000 frames, with 6-channel AC-3 at 48 kHz, the way VirtualDub writes one.
    fn sample() -> Vec<u8> {
        let mut avih = Vec::new();
        for n in [41_708u32, 0, 0, 0x10, 1_000, 0, 2, 0, 640, 272, 0, 0, 0, 0] {
            avih.extend(le(n));
        }
        let mut vstrh = b"vidsxvid".to_vec();
        for n in [0u32, 0, 1001, 24_000, 0, 1_000, 0, 0, 0] {
            vstrh.extend(le(n));
        }
        vstrh.extend([0u8; 8]);
        let mut vstrf = Vec::new();
        for n in [40u32, 640, 272] {
            vstrf.extend(le(n));
        }
        vstrf.extend([1, 0, 24, 0]);
        vstrf.extend(b"XVID");
        vstrf.extend([0u8; 20]);
        let mut astrh = b"auds\0\0\0\0".to_vec();
        for n in [0u32, 0, 1, 48_000, 0, 0, 0, 0, 0] {
            astrh.extend(le(n));
        }
        astrh.extend([0u8; 8]);
        let mut astrf = vec![0x00, 0x20, 6, 0];
        astrf.extend(le(48_000));
        astrf.extend([0u8; 10]);
        let hdrl = list(
            b"hdrl",
            &[
                chunk(b"avih", &avih),
                list(b"strl", &[chunk(b"strh", &vstrh), chunk(b"strf", &vstrf)]),
                list(b"strl", &[chunk(b"strh", &astrh), chunk(b"strf", &astrf), chunk(b"strn", b"English 5.1\0")]),
            ],
        );
        let info = list(b"INFO", &[chunk(b"ISFT", b"VirtualDubMod 1.5.10.2 (build 2540/release)\0")]);
        let movi = list(b"movi", &[chunk(b"00dc", &[0u8; 64])]);
        let mut body = b"AVI ".to_vec();
        body.extend(hdrl);
        body.extend(info);
        body.extend(movi);
        let mut out = b"RIFF".to_vec();
        out.extend(le(body.len() as u32 + 1_000_000));
        out.extend(body);
        out
    }

    #[test]
    fn an_avi_header_gives_its_tracks() {
        let data = sample();
        assert!(is_avi(&data));
        let info = super::super::media::parse(&data).unwrap();
        assert_eq!(info.container, "avi");
        assert!(info.header_complete);
        assert_eq!(info.writing_app.as_deref(), Some("VirtualDubMod 1.5.10.2 (build 2540/release)"));
        let v = &info.video[0];
        assert_eq!((v.codec.as_str(), v.width, v.height), ("XVID", Some(640), Some(272)));
        assert!((v.frame_rate.unwrap() - 23.976).abs() < 0.001, "{:?}", v.frame_rate);
        let a = &info.audio[0];
        assert_eq!((a.codec.as_str(), a.channels, a.sample_rate), ("ac3", Some(6), Some(48_000.0)));
        assert_eq!(a.name.as_deref(), Some("English 5.1"));
        assert!((info.duration_secs.unwrap() - 41.708).abs() < 0.001, "{:?}", info.duration_secs);
    }

    #[test]
    fn a_header_of_endless_streams_keeps_a_few() {
        let mut astrh = b"auds\0\0\0\0".to_vec();
        astrh.extend([0u8; 44]);
        let strl = list(b"strl", &[chunk(b"strh", &astrh), chunk(b"strn", &[b'x'; 5000])]);
        let hdrl = list(b"hdrl", &vec![strl; 10_000]);
        let mut data = b"RIFF\0\0\0\0AVI ".to_vec();
        data.extend(hdrl);
        let info = avi(&data);
        assert_eq!(info.audio.len(), super::super::media::MAX_TRACKS);
        assert_eq!(info.audio[0].name.as_ref().map(|n| n.chars().count()), Some(MAX_TRACK_TEXT));
    }

    #[test]
    fn a_cut_header_gives_what_came_before_the_cut() {
        let data = sample();
        // into the second stream list
        let cut = &data[..data.windows(4).position(|w| w == b"auds").unwrap() + 10];
        let info = avi(cut);
        assert!(!info.header_complete);
        assert_eq!(info.video.len(), 1);
        assert!(info.audio.is_empty() || info.audio[0].channels.is_none());
        assert!(!is_avi(b"RIFF\0\0\0\0WAVE"));
    }
}
