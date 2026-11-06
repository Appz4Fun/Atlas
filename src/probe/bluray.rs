//! Tracks of a Blu-ray title from its playlist, found in the front of a disc image or of a `BDMV` folder posted in a
//! stored archive. A playlist (`PLAYLIST/nnnnn.mpls`, also under `BACKUP`) begins `MPLS` and its version, and lists
//! the clips it plays with their in and out times and, per clip, a stream number table: each video stream's codec,
//! format and frame rate, each audio stream's codec, layout, rate and language, each subtitle stream's language.
//! The disc's main title is the playlist that plays longest. Its `CERTIFICATE/id.bdmv`, when it is in what was read,
//! gives the disc's org and disc IDs. A total function like the rest of `media`: cut off bytes give what was read
//! before them.

use super::media::{AudioTrack, MediaInfo, SubtitleTrack, VideoTrack};

/// The most playlists looked at in one front, and play items in one.
const MAX_PLAYLISTS: usize = 256;
const MAX_ITEMS: usize = 999;

/// Whether `data` is a Blu-ray's: a playlist or index in it, or a disc image naming `BDMV`.
pub fn is_bluray(data: &[u8]) -> bool {
    playlists(data).next().is_some()
        || [b"INDX0100", b"INDX0200", b"INDX0240", b"INDX0300"].iter().any(|v| find(data, *v).is_some())
        || (find(data, b"BDMV").is_some() && (data.get(0x8001..0x8006) == Some(b"CD001") || udf(data)))
}

/// UDF's volume recognition sequence, in the sectors from 16 on.
fn udf(data: &[u8]) -> bool {
    let area = data.get(0x8000..).map(|d| &d[..d.len().min(0x8000)]).unwrap_or_default();
    [b"BEA01", b"NSR02", b"NSR03"].iter().any(|m| find(area, *m).is_some())
}

/// Whether `data` holds a playlist.
pub fn has_playlist(data: &[u8]) -> bool {
    playlists(data).next().is_some()
}

/// The longest playlist found, as its first clip's streams. Without one, a Blu-ray whose header is further on.
pub fn bluray(data: &[u8]) -> MediaInfo {
    let mut best: Option<MediaInfo> = None;
    // a playlist that doesn't parse is most likely cut off: more of the front may make it the longest
    let mut cut = false;
    for at in playlists(data).take(MAX_PLAYLISTS) {
        match playlist(&data[at..]) {
            Some(p)
                if best.as_ref().is_none_or(|b| p.duration_secs.unwrap_or(0.0) > b.duration_secs.unwrap_or(0.0)) =>
            {
                best = Some(p);
            }
            Some(_) => {}
            None => cut = true,
        }
    }
    let mut info = best.unwrap_or_else(|| MediaInfo { container: "bluray".into(), ..MediaInfo::default() });
    info.header_complete &= !cut;
    info.disc_id = disc_id(data);
    info
}

/// The disc's IDs from its `CERTIFICATE/id.bdmv` (libbluray's `bdid_parse`): `BDID` and a four digit version, then at
/// byte 40 the 4-byte org ID and at 44 the 16-byte disc ID, as they're stored, in hex, org ID first (40 digits).
/// None without a whole one, or when both are zero, as on discs authored without them. Whether two pressings of one
/// title share it isn't known yet: a value to show and compare, not to merge on.
pub fn disc_id(data: &[u8]) -> Option<String> {
    let at = data
        .windows(8)
        .enumerate()
        .filter(|(_, w)| w.starts_with(b"BDID") && w.get(4..).is_some_and(|v| v.iter().all(u8::is_ascii_digit)))
        .map(|(at, _)| at)
        .find(|at| data.get(at + 40..at + 60).is_some_and(|ids| ids.iter().any(|b| *b != 0)))?;
    let ids = data.get(at + 40..at + 60)?;
    Some(ids.iter().map(|b| format!("{b:02x}")).collect())
}

/// Where each playlist starts: `MPLS` and a version this reads.
fn playlists(data: &[u8]) -> impl Iterator<Item = usize> + '_ {
    data.windows(8)
        .enumerate()
        .filter(|(_, w)| matches!(w, [b'M', b'P', b'L', b'S', b'0', b'1'..=b'3', b'0', b'0']))
        .map(|(at, _)| at)
}

fn playlist(b: &[u8]) -> Option<MediaInfo> {
    let list = usize::try_from(be32(b, 8)?).ok()?;
    let items = usize::from(be16(b, list + 6)?).min(MAX_ITEMS);
    let mut at = list + 10;
    let mut ticks = 0u64;
    let mut streams = None;
    for _ in 0..items {
        let len = usize::from(be16(b, at)?);
        let item = b.get(at + 2..at + 2 + len)?;
        let (inn, out) = (be32(item, 12)?, be32(item, 16)?);
        ticks += u64::from(out.saturating_sub(inn));
        if streams.is_none() {
            streams = Some(stn(item)?);
        }
        at += 2 + len;
    }
    let mut info = streams?;
    // in and out times are in 45 kHz ticks
    info.duration_secs = (ticks > 0).then(|| ticks as f64 / 45_000.0);
    Some(info)
}

/// A play item's stream number table, after its fixed fields and any angles.
fn stn(item: &[u8]) -> Option<MediaInfo> {
    // 11 reserved bits, then is_multi_angle, then the connection condition
    let multi_angle = item.get(10)? & 0x10 != 0;
    let mut at = 32;
    if multi_angle {
        let angles = usize::from(*item.get(at)?);
        at += 2 + angles.saturating_sub(1) * 10;
    }
    let table = item.get(at + 2..)?;
    let (videos, audios, pgs) = (*table.get(2)?, *table.get(3)?, *table.get(4)?);
    let mut info = MediaInfo { container: "bluray".into(), header_complete: true, ..MediaInfo::default() };
    let mut at = 14;
    let mut next = || -> Option<&[u8]> {
        // a stream entry (its length, then the PID it plays), then its attributes (their length, then them)
        let entry = usize::from(*table.get(at)?);
        let attrs_at = at + 1 + entry;
        let attrs = usize::from(*table.get(attrs_at)?);
        let body = table.get(attrs_at + 1..attrs_at + 1 + attrs)?;
        at = attrs_at + 1 + attrs;
        Some(body)
    };
    for _ in 0..videos {
        let a = next()?;
        info.video.push(video(a));
    }
    for _ in 0..audios {
        let a = next()?;
        info.audio.push(audio(a));
    }
    for _ in 0..pgs {
        let a = next()?;
        info.subtitles.push(SubtitleTrack {
            codec: if a.first() == Some(&0x92) { "text" } else { "pgs" }.into(),
            // a text subtitle's character code comes before its language
            language: language(a.get(if a.first() == Some(&0x92) { 2 } else { 1 }..)),
            ..SubtitleTrack::default()
        });
    }
    Some(info)
}

fn video(a: &[u8]) -> VideoTrack {
    let codec = match a.first() {
        Some(0x01) => "mpeg1video",
        Some(0x02) => "mpeg2video",
        Some(0x1B) => "h264",
        Some(0x20) => "mvc",
        Some(0x24) => "hevc",
        Some(0xEA) => "vc1",
        _ => "unknown",
    };
    let byte = a.get(1).copied().unwrap_or(0);
    let (width, height) = match byte >> 4 {
        1 | 3 => (Some(720), Some(480)),
        2 | 7 => (Some(720), Some(576)),
        4 | 6 => (Some(1920), Some(1080)),
        5 => (Some(1280), Some(720)),
        8 => (Some(3840), Some(2160)),
        _ => (None, None),
    };
    let frame_rate = match byte & 15 {
        1 => Some(24000.0 / 1001.0),
        2 => Some(24.0),
        3 => Some(25.0),
        4 => Some(30000.0 / 1001.0),
        6 => Some(50.0),
        7 => Some(60000.0 / 1001.0),
        _ => None,
    };
    VideoTrack { codec: codec.into(), width, height, frame_rate, ..VideoTrack::default() }
}

fn audio(a: &[u8]) -> AudioTrack {
    let codec = match a.first() {
        Some(0x03 | 0x04) => "mp2",
        Some(0x80) => "pcm",
        Some(0x81) => "ac3",
        Some(0x82) => "dts",
        Some(0x83) => "truehd",
        Some(0x84 | 0xA1) => "eac3",
        Some(0x85) => "dts-hd hra",
        Some(0x86) => "dts-hd ma",
        Some(0xA2) => "dts-hd",
        _ => "unknown",
    };
    let byte = a.get(1).copied().unwrap_or(0);
    AudioTrack {
        codec: codec.into(),
        // mono, stereo; multi-channel doesn't say how many
        channels: match byte >> 4 {
            1 => Some(1),
            3 => Some(2),
            _ => None,
        },
        sample_rate: match byte & 15 {
            1 | 12 | 14 => Some(48000.0),
            4 => Some(96000.0),
            5 => Some(192000.0),
            _ => None,
        },
        language: language(a.get(2..)),
        ..AudioTrack::default()
    }
}

/// An ISO 639-2 code as three lowercase letters.
fn language(b: Option<&[u8]>) -> Option<String> {
    let code = b?.get(..3)?;
    code.iter().all(u8::is_ascii_alphabetic).then(|| code.iter().map(|c| char::from(c.to_ascii_lowercase())).collect())
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
pub(crate) mod tests {
    use super::*;

    fn stream(entry_pid: u16, attrs: &[u8]) -> Vec<u8> {
        let mut out = vec![9, 1];
        out.extend(entry_pid.to_be_bytes());
        out.extend([0u8; 6]);
        out.push(attrs.len() as u8);
        out.extend(attrs);
        out
    }

    /// A playlist of `items` clips of `secs` each: 1080p24 AVC, English TrueHD 7.1 at 48 kHz, a French AC-3 2.0,
    /// English and French PGS.
    pub(crate) fn mpls(items: u32, secs: u32) -> Vec<u8> {
        let streams = [
            stream(0x1011, &[0x1B, 0x61, 0, 0, 0]),
            stream(0x1100, &[0x83, 0x61, b'e', b'n', b'g']),
            stream(0x1101, &[0x81, 0x31, b'f', b'r', b'a']),
            stream(0x1200, &[0x90, b'e', b'n', b'g']),
            stream(0x1201, &[0x90, b'f', b'r', b'a']),
        ]
        .concat();
        let mut table = vec![0, 0, 1, 2, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        table.extend(&streams);
        let mut stn = ((table.len()) as u16).to_be_bytes().to_vec();
        stn.extend(&table);
        let mut list = Vec::new();
        for n in 0..items {
            let mut item = b"00001M2TS".to_vec();
            item.extend([0x00, 0x01, 0]);
            let start = 27_000_000 + n * secs * 45_000;
            item.extend(start.to_be_bytes());
            item.extend((start + secs * 45_000).to_be_bytes());
            item.extend([0u8; 8]);
            item.extend([0, 0, 0, 0]);
            item.extend(&stn);
            list.extend((item.len() as u16).to_be_bytes());
            list.extend(item);
        }
        let mut b = b"MPLS0200".to_vec();
        b.extend(58u32.to_be_bytes());
        b.extend(0u32.to_be_bytes());
        b.extend(0u32.to_be_bytes());
        b.resize(58, 0);
        b.extend(((list.len() + 6) as u32).to_be_bytes());
        b.extend([0, 0]);
        b.extend((items as u16).to_be_bytes());
        b.extend(0u16.to_be_bytes());
        b.extend(list);
        b
    }

    #[test]
    fn the_longest_playlist_gives_the_titles_streams() {
        // a menu loop first, then the feature in two clips, the way a BDMV folder lists them
        let mut data = b"Rar!\x1a\x07\x00 BDMV/index.bdmv INDX0200 ".to_vec();
        data.extend(mpls(1, 30));
        data.extend(mpls(2, 3_600));
        assert!(is_bluray(&data));
        let info = super::super::media::parse(&data).unwrap();
        assert_eq!(info.container, "bluray");
        assert!(info.header_complete);
        assert_eq!(info.duration_secs, Some(7_200.0));
        let v = &info.video[0];
        assert_eq!((v.codec.as_str(), v.width, v.height), ("h264", Some(1920), Some(1080)));
        assert!((v.frame_rate.unwrap() - 23.976).abs() < 0.001);
        type Row<'a> = (&'a str, Option<u64>, Option<f64>, Option<&'a str>);
        let audio: Vec<Row<'_>> =
            info.audio.iter().map(|a| (a.codec.as_str(), a.channels, a.sample_rate, a.language.as_deref())).collect();
        assert_eq!(audio, [("truehd", None, Some(48000.0), Some("eng")), ("ac3", Some(2), Some(48000.0), Some("fra"))]);
        let subs: Vec<(&str, Option<&str>)> =
            info.subtitles.iter().map(|s| (s.codec.as_str(), s.language.as_deref())).collect();
        assert_eq!(subs, [("pgs", Some("eng")), ("pgs", Some("fra"))]);
    }

    /// An `id.bdmv` with these org and disc IDs.
    pub(crate) fn id_bdmv(org: [u8; 4], disc: [u8; 16]) -> Vec<u8> {
        let mut b = b"BDID0200".to_vec();
        // where the data and the extension data start
        b.extend(40u32.to_be_bytes());
        b.extend(0u32.to_be_bytes());
        b.resize(40, 0);
        b.extend(org);
        b.extend(disc);
        b.resize(b.len() + 4, 0);
        b
    }

    #[test]
    fn the_disc_id_comes_from_id_bdmv() {
        let disc: [u8; 16] = std::array::from_fn(|n| 0xA0 + n as u8);
        let id = id_bdmv([0x00, 0x01, 0x02, 0x03], disc);
        let mut data = b"Rar!\x1a\x07\x00 BDMV/index.bdmv INDX0200 ".to_vec();
        data.extend(mpls(1, 3_600));
        data.extend(b" CERTIFICATE/id.bdmv ");
        data.extend(&id);
        let want = "00010203a0a1a2a3a4a5a6a7a8a9aaabacadaeaf";
        assert_eq!(disc_id(&data).as_deref(), Some(want));
        let info = super::super::media::parse(&data).unwrap();
        assert_eq!(info.disc_id.as_deref(), Some(want));
        assert_eq!(info.duration_secs, Some(3_600.0), "the playlist still reads");
        // the backup copy beside it says the same
        assert_eq!(disc_id(&[data.as_slice(), &id].concat()).as_deref(), Some(want));
    }

    #[test]
    fn no_disc_id_from_a_cut_or_empty_id_bdmv() {
        let id = id_bdmv([1, 2, 3, 4], [9; 16]);
        assert_eq!(disc_id(&id[..59]), None, "the disc ID is cut");
        assert_eq!(disc_id(&id_bdmv([0; 4], [0; 16])), None, "all zero says nothing");
        assert_eq!(disc_id(&id[..]), Some(format!("01020304{}", "09".repeat(16))));
        // not a version
        let mut odd = id.clone();
        odd[4] = b'x';
        assert_eq!(disc_id(&odd), None);
        for cut in 0..id.len() {
            let _ = disc_id(&id[..cut]);
            let _ = bluray(&id[..cut]);
        }
        assert_eq!(bluray(&mpls(1, 60)).disc_id, None);
    }

    #[test]
    fn a_cut_playlist_after_a_whole_one_wants_more() {
        let feature = mpls(2, 3_600);
        let data = [mpls(1, 30), feature[..feature.len() - 1].to_vec()].concat();
        let info = bluray(&data);
        assert_eq!(info.duration_secs, Some(30.0));
        assert!(!info.header_complete, "the feature may be whole in the next article");
    }

    #[test]
    fn a_disc_without_its_playlists_yet_wants_more_and_cuts_are_safe() {
        let mut iso = vec![0u8; 0x9000];
        iso[0x8001..0x8006].copy_from_slice(b"BEA01");
        iso.extend(b"BDMV");
        let info = super::super::media::parse(&iso).unwrap();
        assert_eq!(info.container, "bluray");
        assert!(!info.header_complete);
        for index in [b"INDX0100", b"INDX0240"] {
            assert!(is_bluray(&[b"Rar!\x1a\x07\x00 BDMV/index.bdmv ".as_slice(), index].concat()));
        }
        let data = mpls(2, 60);
        for cut in 0..data.len() {
            let _ = bluray(&data[..cut]);
        }
        // a playlist claiming a huge list stays bounded
        let mut huge = mpls(1, 60);
        huge[64..66].copy_from_slice(&u16::MAX.to_be_bytes());
        let _ = bluray(&huge);
    }
}
