//! Tracks of a DVD-Video title set from its IFO, found in the front of a disc image or of a `VIDEO_TS` folder posted
//! in a stored archive. A title set's IFO (`VTS_nn_0.IFO`, or its `.BUP` copy) begins `DVDVIDEO-VTS` and holds the
//! video's format, each audio stream's coding, channels and language, each subpicture stream's language, and its
//! programs' playing times. The disc's main title is the set that plays longest. A total function like the rest of
//! `media`: cut off bytes give what was read before them.

use super::media::{AudioTrack, MediaInfo, SubtitleTrack, VideoTrack};

const VTS: &[u8] = b"DVDVIDEO-VTS";
const VMG: &[u8] = b"DVDVIDEO-VMG";

/// The most title set IFOs looked at in one front: a disc has at most 99, and each has a `.BUP` copy.
const MAX_SETS: usize = 64;

/// Whether `data` is a DVD-Video's: a title set or manager IFO in it, or an ISO 9660 image naming `VIDEO_TS`.
pub fn is_dvd(data: &[u8]) -> bool {
    find(data, VTS).is_some() || find(data, VMG).is_some() || iso_with_video_ts(data)
}

fn iso_with_video_ts(data: &[u8]) -> bool {
    data.get(0x8001..0x8006) == Some(b"CD001") && find(data, b"VIDEO_TS").is_some()
}

/// The longest title set found, as its tracks. Without one, a DVD whose header (the title set's IFO) is further on.
pub fn dvd(data: &[u8]) -> MediaInfo {
    let mut best: Option<(f64, MediaInfo)> = None;
    let mut at = 0usize;
    for _ in 0..MAX_SETS {
        let Some(n) = data.get(at..).and_then(|rest| find(rest, VTS)) else { break };
        let ifo = &data[at + n..];
        if let Some(info) = title_set(ifo) {
            let secs = info.duration_secs.unwrap_or(0.0);
            if best.as_ref().is_none_or(|(b, _)| secs > *b) {
                best = Some((secs, info));
            }
        }
        at += n + VTS.len();
    }
    best.map(|(_, info)| info).unwrap_or_else(|| MediaInfo { container: "dvd-video".into(), ..MediaInfo::default() })
}

/// One title set's IFO from its start.
fn title_set(ifo: &[u8]) -> Option<MediaInfo> {
    let attrs = ifo.get(0x200..0x256 + 32 * 6)?;
    let mut info = MediaInfo { container: "dvd-video".into(), header_complete: true, ..MediaInfo::default() };
    info.video.push(video(attrs[0], attrs[1]));
    let audio_count = usize::from(be16(attrs, 2)?).min(8);
    for n in 0..audio_count {
        info.audio.push(audio(attrs.get(4 + n * 8..12 + n * 8)?));
    }
    let sub_count = usize::from(be16(attrs, 0x54)?).min(32);
    for n in 0..sub_count {
        info.subtitles.push(subpicture(attrs.get(0x56 + n * 6..0x5C + n * 6)?));
    }
    info.duration_secs = longest_program(ifo);
    Some(info)
}

/// The video attributes: MPEG-1 or 2, NTSC or PAL, the display aspect and the picture size.
fn video(a: u8, b: u8) -> VideoTrack {
    let mpeg2 = (a >> 6) & 3 == 1;
    let pal = (a >> 4) & 3 == 1;
    let wide = (a >> 2) & 3 == 3;
    let lines = if pal { 576 } else { 480 };
    let (width, height) = match (b >> 3) & 7 {
        0 => (720, lines),
        1 => (704, lines),
        2 => (352, lines),
        _ => (352, lines / 2),
    };
    let aspect = if wide { 16.0 / 9.0 } else { 4.0 / 3.0 };
    VideoTrack {
        codec: if mpeg2 { "mpeg2video" } else { "mpeg1video" }.into(),
        width: Some(width),
        height: Some(height),
        display_width: Some((height as f64 * aspect).round() as u64),
        display_height: Some(height),
        frame_rate: Some(if pal { 25.0 } else { 30000.0 / 1001.0 }),
        ..VideoTrack::default()
    }
}

/// One audio stream's 8 bytes: coding, language, rate, channels, and what it's for.
fn audio(b: &[u8]) -> AudioTrack {
    let codec = match b[0] >> 5 {
        0 => "ac3",
        2 | 3 => "mp2",
        4 => "pcm",
        6 => "dts",
        _ => "unknown",
    };
    AudioTrack {
        codec: codec.into(),
        channels: Some(u64::from(b[1] & 7) + 1),
        sample_rate: Some(if (b[1] >> 4) & 3 == 1 { 96000.0 } else { 48000.0 }),
        language: ((b[0] >> 2) & 3 == 1).then(|| language(b[2], b[3])).flatten(),
        // 3 and 4: director's comments
        commentary: matches!(b[5], 3 | 4),
        ..AudioTrack::default()
    }
}

/// One subpicture stream's 6 bytes: its language, and whether it's forced or captions.
fn subpicture(b: &[u8]) -> SubtitleTrack {
    SubtitleTrack {
        codec: "dvd_subtitle".into(),
        language: (b[0] & 3 == 1).then(|| language(b[2], b[3])).flatten(),
        forced: b[5] == 9,
        hearing_impaired: matches!(b[5], 5..=7),
        commentary: matches!(b[5], 13..=15),
        ..SubtitleTrack::default()
    }
}

/// An ISO 639-1 code as two lowercase letters.
fn language(a: u8, b: u8) -> Option<String> {
    (a.is_ascii_alphabetic() && b.is_ascii_alphabetic())
        .then(|| [a, b].iter().map(|c| char::from(c.to_ascii_lowercase())).collect())
}

/// The longest program chain's playing time, from the title set's program chain table (its sector at 0xCC).
fn longest_program(ifo: &[u8]) -> Option<f64> {
    let table = usize::try_from(be32(ifo, 0xCC)?).ok()?.checked_mul(2048)?;
    let count = usize::from(be16(ifo, table)?).min(999);
    (0..count)
        .filter_map(|n| {
            let pgc = table.checked_add(usize::try_from(be32(ifo, table + 8 + n * 8 + 4)?).ok()?)?;
            playing_time(ifo.get(pgc + 4..pgc + 8)?)
        })
        .reduce(f64::max)
}

/// A program chain's BCD playing time: hours, minutes, seconds, and frames with the frame rate in the top two bits.
fn playing_time(t: &[u8]) -> Option<f64> {
    let bcd = |b: u8| -> Option<f64> { (b >> 4 < 10 && b & 15 < 10).then(|| f64::from((b >> 4) * 10 + (b & 15))) };
    let fps = match t[3] >> 6 {
        1 => 25.0,
        3 => 30000.0 / 1001.0,
        _ => return Some(bcd(t[0])? * 3600.0 + bcd(t[1])? * 60.0 + bcd(t[2])?),
    };
    Some(bcd(t[0])? * 3600.0 + bcd(t[1])? * 60.0 + bcd(t[2])? + bcd(t[3] & 0x3F)? / fps)
}

fn find(data: &[u8], needle: &[u8]) -> Option<usize> {
    data.windows(needle.len()).position(|w| w == needle)
}

fn be16(b: &[u8], at: usize) -> Option<u16> {
    b.get(at..at + 2).map(|x| u16::from_be_bytes([x[0], x[1]]))
}

fn be32(b: &[u8], at: usize) -> Option<u32> {
    b.get(at..at + 4).map(|x| u32::from_be_bytes([x[0], x[1], x[2], x[3]]))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A PAL 16:9 title set IFO: English 5.1 AC-3, a German stereo AC-3 commentary, a French DTS; English and
    /// forced German subpictures; two program chains of 1:52:30.00 and 0:03:10.12.
    pub(crate) fn ifo() -> Vec<u8> {
        let mut b = vec![0u8; 3 * 2048];
        b[..12].copy_from_slice(VTS);
        // the program chain table in sector 1
        b[0xCC..0xD0].copy_from_slice(&1u32.to_be_bytes());
        b[0x200] = 0b0101_1100;
        b[0x201] = 0;
        b[0x202..0x204].copy_from_slice(&3u16.to_be_bytes());
        let streams: [[u8; 8]; 3] = [
            [0x04, 0x05, b'e', b'n', 0, 1, 0, 0],
            [0x04, 0x01, b'd', b'e', 0, 3, 0, 0],
            [0xC4, 0x05, b'f', b'r', 0, 1, 0, 0],
        ];
        for (n, s) in streams.iter().enumerate() {
            b[0x204 + n * 8..0x20C + n * 8].copy_from_slice(s);
        }
        b[0x254..0x256].copy_from_slice(&2u16.to_be_bytes());
        b[0x256..0x25C].copy_from_slice(&[0x01, 0, b'e', b'n', 0, 1]);
        b[0x25C..0x262].copy_from_slice(&[0x01, 0, b'd', b'e', 0, 9]);
        let table = 2048;
        b[table..table + 2].copy_from_slice(&2u16.to_be_bytes());
        for (n, (off, time)) in
            [(0x40u32, [0x01, 0x52, 0x30, 0x40]), (0x200, [0x00, 0x03, 0x10, 0x52])].iter().enumerate()
        {
            b[table + 8 + n * 8 + 4..table + 8 + n * 8 + 8].copy_from_slice(&off.to_be_bytes());
            let pgc = table + *off as usize;
            b[pgc + 4..pgc + 8].copy_from_slice(time);
        }
        b
    }

    #[test]
    fn a_title_sets_ifo_gives_its_tracks() {
        // in a folder posted in a stored archive: other files before it
        let mut data = b"Rar!\x1a\x07\x00 some VIDEO_TS.BUP bytes ".to_vec();
        data.extend(ifo());
        assert!(is_dvd(&data));
        let info = super::super::media::parse(&data).unwrap();
        assert_eq!(info.container, "dvd-video");
        assert!(info.header_complete);
        let v = &info.video[0];
        assert_eq!(
            (v.codec.as_str(), v.width, v.height, v.frame_rate),
            ("mpeg2video", Some(720), Some(576), Some(25.0))
        );
        assert_eq!(v.display_width, Some(1024));
        let audio: Vec<(&str, Option<u64>, Option<&str>, bool)> =
            info.audio.iter().map(|a| (a.codec.as_str(), a.channels, a.language.as_deref(), a.commentary)).collect();
        assert_eq!(
            audio,
            [
                ("ac3", Some(6), Some("en"), false),
                ("ac3", Some(2), Some("de"), true),
                ("dts", Some(6), Some("fr"), false)
            ]
        );
        let subs: Vec<(Option<&str>, bool)> =
            info.subtitles.iter().map(|s| (s.language.as_deref(), s.forced)).collect();
        assert_eq!(subs, [(Some("en"), false), (Some("de"), true)]);
        assert_eq!(info.duration_secs, Some(6750.0));
    }

    #[test]
    fn the_longest_title_set_is_the_main_one() {
        let mut extra = ifo();
        extra[0x202..0x204].copy_from_slice(&1u16.to_be_bytes());
        // a 10 minute extra
        extra[2048 + 0x40 + 4..2048 + 0x40 + 8].copy_from_slice(&[0x00, 0x10, 0x00, 0x40]);
        extra[2048 + 0x200 + 4..2048 + 0x200 + 8].copy_from_slice(&[0x00, 0x00, 0x00, 0x40]);
        let data = [extra, ifo()].concat();
        let info = dvd(&data);
        assert_eq!(info.audio.len(), 3, "the feature's set, not the extra's");
        assert_eq!(info.duration_secs, Some(6750.0));
    }

    #[test]
    fn a_disc_without_its_title_set_yet_wants_more_of_the_front() {
        let mut iso = vec![0u8; 0x9000];
        iso[0x8001..0x8006].copy_from_slice(b"CD001");
        iso.extend(b"VIDEO_TS");
        let info = super::super::media::parse(&iso).unwrap();
        assert_eq!(info.container, "dvd-video");
        assert!(!info.header_complete);
        assert!(info.video.is_empty());
        // cut anywhere: safe
        let data = ifo();
        for cut in (0..data.len()).step_by(7) {
            let _ = dvd(&data[..cut]);
        }
    }
}
