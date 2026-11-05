//! Tracks from the start of an MPEG transport stream: a broadcast capture's `.ts`, or a Blu-ray's `.m2ts` (each
//! 188-byte packet behind a 4-byte time code). The program map table lists each stream's type and its descriptors
//! (language, AC-3, DTS, DVB subtitles); the video's size and frame rate come from its first sequence header or
//! SPS, an audio stream's rate and channels from its first frame. A total function like the rest of `media`: cut
//! off bytes give what was read before them.

use super::media::{AudioTrack, MAX_TRACKS, MediaInfo, SubtitleTrack, VideoTrack};

/// The most bytes walked: a few MB hold a program map and each stream's first frames.
const MAX_WALK: usize = 8 << 20;
/// The most of a stream's payload kept to look for its header in.
const MAX_ES: usize = 512 << 10;

/// The packet size and where the first sync byte is, when `data` starts as a transport stream: 188 bytes, or 192
/// with a time code in front.
fn layout(data: &[u8]) -> Option<(usize, usize)> {
    [(188, 0), (192, 4)].into_iter().find(|(size, first)| (0..5).all(|n| data.get(first + n * size) == Some(&0x47)))
}

pub fn is_mpeg_ts(data: &[u8]) -> bool {
    layout(data).is_some()
}

/// One elementary stream the program map names.
struct Es {
    pid: u16,
    kind: Kind,
    language: Option<String>,
    payload: Vec<u8>,
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Video(&'static str),
    Audio(&'static str),
    Subtitle(&'static str),
}

pub fn mpeg_ts(data: &[u8]) -> MediaInfo {
    let mut info = MediaInfo { container: "mpeg-ts".into(), ..MediaInfo::default() };
    let Some((size, first)) = layout(data) else { return info };
    if size == 192 {
        info.container = "m2ts".into();
    }
    let data = &data[..data.len().min(MAX_WALK)];
    let mut pmt_pids: Vec<u16> = Vec::new();
    let mut streams: Option<Vec<Es>> = None;
    // a table's section as its packets come, from its pointer field on: a map can run over several packets
    let (mut pat_buf, mut pmt_buf): (Option<Vec<u8>>, Option<(u16, Vec<u8>)>) = (None, None);
    let mut at = first;
    while let Some(packet) = data.get(at..at + 188) {
        at += size;
        if packet[0] != 0x47 {
            continue;
        }
        let start = packet[1] & 0x40 != 0;
        let pid = u16::from_be_bytes([packet[1] & 0x1F, packet[2]]);
        let Some(payload) = payload(packet) else { continue };
        if pid == 0 && pmt_pids.is_empty() {
            if start {
                pat_buf = Some(Vec::new());
            }
            if let Some(buf) = pat_buf.as_mut() {
                buf.extend_from_slice(payload);
                if whole(buf) {
                    pmt_pids = pat(buf);
                    pat_buf = None;
                }
            }
        } else if streams.is_none() && pmt_pids.contains(&pid) {
            if start {
                pmt_buf = Some((pid, Vec::new()));
            }
            if let Some((_, buf)) = pmt_buf.as_mut().filter(|(p, _)| *p == pid) {
                buf.extend_from_slice(payload);
                if whole(buf) {
                    streams = pmt(buf);
                    pmt_buf = None;
                }
            }
        } else if let Some(es) = streams.as_mut().and_then(|s| s.iter_mut().find(|e| e.pid == pid))
            && es.payload.len() < MAX_ES
        {
            // a unit's start carries its PES header; past it, its elementary bytes
            let body = if start { pes_body(payload).unwrap_or_default() } else { payload };
            es.payload.extend_from_slice(body);
        }
        if streams.as_ref().is_some_and(|s| s.iter().all(|e| e.payload.len() >= MAX_ES)) {
            break;
        }
    }
    let Some(streams) = streams else { return info };
    for es in streams.into_iter().take(MAX_TRACKS) {
        match es.kind {
            Kind::Video(codec) => {
                let mut v = VideoTrack { codec: codec.into(), language: es.language, ..VideoTrack::default() };
                video(codec, &es.payload, &mut v);
                info.video.push(v);
            }
            Kind::Audio(codec) => {
                let mut a = audio(codec, &es.payload);
                a.language = es.language;
                info.audio.push(a);
            }
            Kind::Subtitle(codec) => {
                info.subtitles.push(SubtitleTrack {
                    codec: codec.into(),
                    language: es.language,
                    ..SubtitleTrack::default()
                });
            }
        }
    }
    // the map read, and the video's size: more packets could give a size not found yet
    info.header_complete = info.video.iter().all(|v| v.width.is_some());
    info
}

/// A packet's payload, past its header and adaptation field.
fn payload(p: &[u8]) -> Option<&[u8]> {
    let control = (p[3] >> 4) & 3;
    match control {
        1 => p.get(4..),
        3 => p.get(5 + usize::from(*p.get(4)?)..),
        _ => None,
    }
}

/// Whether a section buffered from its pointer field on has all its declared bytes, or more than any section has.
fn whole(buf: &[u8]) -> bool {
    let Some(&pointer) = buf.first() else { return false };
    let s = &buf[(1 + usize::from(pointer)).min(buf.len())..];
    match (s.get(1), s.get(2)) {
        (Some(hi), Some(lo)) => s.len() >= 3 + usize::from(u16::from_be_bytes([hi & 0x0F, *lo])),
        _ => buf.len() > 4096,
    }
}

/// A section's body after its pointer field, as far as its length says.
fn section(payload: &[u8]) -> Option<&[u8]> {
    let s = payload.get(1 + usize::from(*payload.first()?)..)?;
    let len = usize::from(u16::from_be_bytes([*s.get(1)? & 0x0F, *s.get(2)?]));
    // past the CRC
    s.get(..(3 + len).min(s.len()).saturating_sub(4))
}

/// The program association table: each program's map PID.
fn pat(payload: &[u8]) -> Vec<u16> {
    let Some(s) = section(payload).filter(|s| s.first() == Some(&0)) else { return Vec::new() };
    s.get(8..)
        .unwrap_or_default()
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|e| u16::from_be_bytes([e[0], e[1]]) != 0)
        .map(|e| u16::from_be_bytes([e[2] & 0x1F, e[3]]))
        .take(16)
        .collect()
}

/// The program map table: each stream's type, PID and descriptors.
fn pmt(payload: &[u8]) -> Option<Vec<Es>> {
    let s = section(payload).filter(|s| s.first() == Some(&2))?;
    let info_len = usize::from(u16::from_be_bytes([*s.get(10)? & 0x0F, *s.get(11)?]));
    let mut at = 12 + info_len;
    let mut out = Vec::new();
    while let Some(e) = s.get(at..at + 5) {
        let (ty, pid) = (e[0], u16::from_be_bytes([e[1] & 0x1F, e[2]]));
        let es_len = usize::from(u16::from_be_bytes([e[3] & 0x0F, e[4]]));
        let descriptors = s.get(at + 5..(at + 5 + es_len).min(s.len())).unwrap_or_default();
        at += 5 + es_len;
        if let Some(kind) = kind(ty, descriptors) {
            out.push(Es { pid, kind, language: language(descriptors), payload: Vec::new() });
        }
        if out.len() >= MAX_TRACKS {
            break;
        }
    }
    Some(out)
}

/// What a stream is, from its type, or for private data (0x06) from its descriptors.
fn kind(ty: u8, d: &[u8]) -> Option<Kind> {
    Some(match ty {
        0x01 => Kind::Video("mpeg1video"),
        0x02 => Kind::Video("mpeg2video"),
        0x10 => Kind::Video("mpeg4"),
        0x1B => Kind::Video("h264"),
        0x20 => Kind::Video("mvc"),
        0x24 => Kind::Video("hevc"),
        0xEA => Kind::Video("vc1"),
        0x03 | 0x04 => Kind::Audio("mp2"),
        0x0F | 0x11 => Kind::Audio("aac"),
        0x80 => Kind::Audio("pcm"),
        0x81 => Kind::Audio("ac3"),
        0x82 | 0x8A => Kind::Audio("dts"),
        0x83 => Kind::Audio("truehd"),
        0x84 | 0x87 | 0xA1 => Kind::Audio("eac3"),
        0x85 => Kind::Audio("dts-hd hra"),
        0x86 => Kind::Audio("dts-hd ma"),
        0x90 => Kind::Subtitle("pgs"),
        0x92 => Kind::Subtitle("text"),
        0x06 => {
            let tags: Vec<u8> = descriptors(d).map(|(tag, _)| tag).collect();
            if tags.contains(&0x6A) {
                Kind::Audio("ac3")
            } else if tags.contains(&0x7A) {
                Kind::Audio("eac3")
            } else if tags.contains(&0x7B) {
                Kind::Audio("dts")
            } else if tags.contains(&0x7C) {
                Kind::Audio("aac")
            } else if tags.contains(&0x59) {
                Kind::Subtitle("dvb_subtitle")
            } else if tags.contains(&0x56) {
                Kind::Subtitle("teletext")
            } else {
                return None;
            }
        }
        _ => return None,
    })
}

fn descriptors(d: &[u8]) -> impl Iterator<Item = (u8, &[u8])> {
    let mut at = 0usize;
    std::iter::from_fn(move || {
        let (tag, len) = (*d.get(at)?, usize::from(*d.get(at + 1)?));
        let body = d.get(at + 2..at + 2 + len)?;
        at += 2 + len;
        Some((tag, body))
    })
}

/// The language a stream's descriptors give: ISO 639 (0x0A), or the first of a DVB subtitling or teletext list.
fn language(d: &[u8]) -> Option<String> {
    descriptors(d).find_map(|(tag, body)| match tag {
        0x0A | 0x59 | 0x56 => {
            let code = body.get(..3)?;
            code.iter()
                .all(u8::is_ascii_alphabetic)
                .then(|| code.iter().map(|c| char::from(c.to_ascii_lowercase())).collect())
        }
        _ => None,
    })
}

/// A PES packet's elementary bytes, past its header.
fn pes_body(p: &[u8]) -> Option<&[u8]> {
    if p.get(..3)? != [0, 0, 1] {
        return None;
    }
    p.get(9 + usize::from(*p.get(8)?)..)
}

fn video(codec: &str, es: &[u8], v: &mut VideoTrack) {
    match codec {
        "mpeg1video" | "mpeg2video" => super::mpegps::sequence(es, v),
        "h264" => {
            let sps = crate::video::h264::split_annex_b(es)
                .into_iter()
                .filter(|n| n.first().is_some_and(|h| h & 0x1F == 7))
                .find_map(|n| crate::video::h264::params::parse_sps(n).ok());
            if let Some(s) = sps {
                let width = (s.width_mbs * 16).saturating_sub(s.crop[0] + s.crop[1]);
                let height = (s.height_mbs * 16).saturating_sub(s.crop[2] + s.crop[3]);
                v.width = (width > 0).then_some(u64::from(width));
                v.height = (height > 0).then_some(u64::from(height));
                v.bit_depth = Some(u64::from(s.bit_depth_luma));
            }
        }
        "hevc" => {
            let sps = crate::video::h264::split_annex_b(es)
                .into_iter()
                .filter(|n| n.first().is_some_and(|h| (h >> 1) & 0x3F == 33))
                .find_map(|n| crate::video::hevc::params::parse_sps(n).ok());
            if let Some(s) = sps {
                let width = s.width.saturating_sub(s.crop[0] + s.crop[1]);
                let height = s.height.saturating_sub(s.crop[2] + s.crop[3]);
                v.width = (width > 0).then_some(u64::from(width));
                v.height = (height > 0).then_some(u64::from(height));
                v.bit_depth = Some(u64::from(s.bit_depth_luma));
            }
        }
        _ => {}
    }
}

fn audio(codec: &str, es: &[u8]) -> AudioTrack {
    match codec {
        "ac3" | "eac3" => {
            // the map's codec stands when no frame was read to say otherwise
            let mut a = super::mpegps::ac3(es);
            if a.sample_rate.is_none() {
                a.codec = codec.into();
            }
            a
        }
        "dts" => super::mpegps::dts(es),
        "mp2" => {
            let mut a = AudioTrack { codec: "mp2".into(), ..AudioTrack::default() };
            super::mpegps::mpeg_audio(es, &mut a);
            a
        }
        "aac" => adts(es),
        other => AudioTrack { codec: other.into(), ..AudioTrack::default() },
    }
}

/// An AAC ADTS frame header: rate and channel configuration.
fn adts(es: &[u8]) -> AudioTrack {
    let mut a = AudioTrack { codec: "aac".into(), ..AudioTrack::default() };
    let Some(at) = es.windows(2).position(|w| w[0] == 0xFF && w[1] & 0xF6 == 0xF0) else { return a };
    let Some(h) = es.get(at..at + 4) else { return a };
    const RATES: [f64; 12] =
        [96000.0, 88200.0, 64000.0, 48000.0, 44100.0, 32000.0, 24000.0, 22050.0, 16000.0, 12000.0, 11025.0, 8000.0];
    a.sample_rate = RATES.get(usize::from((h[2] >> 2) & 15)).copied();
    let config = ((h[2] & 1) << 2) | (h[3] >> 6);
    a.channels = match config {
        1..=6 => Some(u64::from(config)),
        7 => Some(8),
        _ => None,
    };
    a
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Packets of `pid` carrying `data`, the first starting a unit, padded with 0xFF.
    fn packets(pid: u16, data: &[u8], start: bool) -> Vec<u8> {
        let mut out = Vec::new();
        for (n, chunk) in data.chunks(184).enumerate() {
            let mut p = vec![0x47, ((pid >> 8) as u8 & 0x1F) | if start && n == 0 { 0x40 } else { 0 }, pid as u8, 0x10];
            p.extend(chunk);
            p.resize(188, 0xFF);
            out.extend(p);
        }
        out
    }

    fn section(table: u8, body: &[u8]) -> Vec<u8> {
        let len = body.len() + 5 + 4;
        let mut s = vec![0, table, 0xB0 | (len >> 8) as u8, len as u8, 0, 1, 0xC1, 0, 0];
        s.extend(body);
        s.extend([0, 0, 0, 0]);
        s
    }

    fn pes(body: &[u8]) -> Vec<u8> {
        let mut p = vec![0, 0, 1, 0xE0, 0, 0, 0x80, 0, 0];
        p.extend(body);
        p
    }

    /// A DVB capture: 720x576 MPEG-2, German MPEG audio, English AC-3 5.1 (private data with an AC-3 descriptor),
    /// and German DVB subtitles.
    fn capture() -> Vec<u8> {
        let pat = section(0, &[0, 1, 0xE1, 0x00]);
        let mut map = vec![0xE1, 0x00, 0xF0, 0x00];
        map.extend([0x02, 0xE1, 0x01, 0xF0, 0x00]);
        map.extend([0x03, 0xE1, 0x02, 0xF0, 0x06, 0x0A, 0x04, b'd', b'e', b'u', 0]);
        map.extend([0x06, 0xE1, 0x03, 0xF0, 0x09, 0x6A, 0x01, 0x00, 0x0A, 0x04, b'e', b'n', b'g', 0]);
        map.extend([0x06, 0xE1, 0x04, 0xF0, 0x0A, 0x59, 0x08, b'g', b'e', b'r', 0x10, 0, 1, 0, 1]);
        let pmt = section(2, &map);
        let seq = [0, 0, 1, 0xB3, 0x2D, 0x02, 0x40, 0x33, 0xFF, 0xFF, 0xE0, 0x18, 0, 0, 1, 0xB5, 0x14, 0x8A];
        let mp2 = [0xFF, 0xFD, 0x84, 0x04];
        let ac3 = [0x0B, 0x77, 0, 0, 0x04, 0x40, 0xE1, 0x10];
        [
            packets(0, &pat, true),
            packets(0x100, &pmt, true),
            packets(0x101, &pes(&seq), true),
            packets(0x102, &pes(&mp2), true),
            packets(0x103, &pes(&ac3), true),
        ]
        .concat()
    }

    #[test]
    fn a_captures_program_map_names_its_streams() {
        let data = capture();
        assert!(is_mpeg_ts(&data));
        let info = super::super::media::parse(&data).unwrap();
        assert_eq!(info.container, "mpeg-ts");
        assert!(info.header_complete);
        let v = &info.video[0];
        assert_eq!(
            (v.codec.as_str(), v.width, v.height, v.frame_rate),
            ("mpeg2video", Some(720), Some(576), Some(25.0))
        );
        let audio: Vec<(&str, Option<u64>, Option<&str>)> =
            info.audio.iter().map(|a| (a.codec.as_str(), a.channels, a.language.as_deref())).collect();
        assert_eq!(audio, [("mp2", Some(2), Some("deu")), ("ac3", Some(6), Some("eng"))]);
        assert_eq!(info.subtitles[0].codec, "dvb_subtitle");
        assert_eq!(info.subtitles[0].language.as_deref(), Some("ger"));
    }

    #[test]
    fn a_map_over_several_packets_is_read_whole_and_eac3_stays_eac3() {
        let pat = section(0, &[0, 1, 0xE1, 0x00]);
        let mut map = vec![0xE1, 0x00, 0xF0, 0x00];
        for n in 0..20u8 {
            map.extend([0x03, 0xE1, 0x10 + n, 0xF0, 0x06, 0x0A, 0x04, b'e', b'n', b'g', 0]);
        }
        // E-AC-3 by type, with no frame read
        map.extend([0x87, 0xE1, 0x40, 0xF0, 0x00]);
        let pmt = section(2, &map);
        assert!(pmt.len() > 184);
        // and null packets, as a stream has between
        let data =
            [packets(0, &pat, true), packets(0x100, &pmt, true), packets(0x1FFF, &[0u8; 184 * 3], false)].concat();
        assert!(!whole(&[&[0u8][..], &pmt[1..183]].concat()), "the first packet's part isn't the section");
        let info = mpeg_ts(&data);
        assert_eq!(info.audio.len(), 21);
        assert_eq!(info.audio[20].codec, "eac3");
    }

    #[test]
    fn an_m2ts_has_a_time_code_before_each_packet_and_cuts_are_safe() {
        let ts = capture();
        let mut m2ts = Vec::new();
        for p in ts.chunks(188) {
            m2ts.extend([0, 0, 0, 0]);
            m2ts.extend(p);
        }
        let info = mpeg_ts(&m2ts);
        assert_eq!(info.container, "m2ts");
        assert_eq!(info.audio.len(), 2);
        for cut in (0..ts.len()).step_by(5) {
            let _ = mpeg_ts(&ts[..cut]);
        }
        assert!(!is_mpeg_ts(&[0x47; 100]));
    }
}
