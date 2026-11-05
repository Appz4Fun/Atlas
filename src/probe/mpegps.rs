//! Tracks from the start of an MPEG program stream: `.mpg`, `.mpeg`, and a DVD's `.vob`. Unlike the other formats
//! it has no header listing its streams, soo the packs at the front are walked and each stream is named from its
//! first payload: an MPEG video sequence header, an MPEG audio frame, or in private stream 1 a DVD substream (AC-3,
//! DTS, LPCM, subpictures). A total function like the rest of `media`: cut off bytes give what was read before them.

use super::media::{AudioTrack, MediaInfo, SubtitleTrack, VideoTrack, is_full};

/// The most bytes walked: a few MB of packs hold every stream of a DVD or a capture.
const MAX_WALK: usize = 8 << 20;

pub fn is_mpeg_ps(data: &[u8]) -> bool {
    // a pack header, MPEG-2 (`01` after it) or MPEG-1 (`0010`)
    data.len() >= 14 && data[..4] == [0, 0, 1, 0xBA] && (data[4] & 0xC0 == 0x40 || data[4] & 0xF0 == 0x20)
}

pub fn mpeg_ps(data: &[u8]) -> MediaInfo {
    let mut info = MediaInfo { container: "mpeg-ps".into(), ..MediaInfo::default() };
    let data = &data[..data.len().min(MAX_WALK)];
    // stream ids seen, with the private stream 1 substream: one track each
    let mut seen: Vec<Seen> = Vec::new();
    let mut at = 0usize;
    while at + 4 <= data.len() {
        if data[at..at + 3] != [0, 0, 1] {
            // lost: on to the next start code
            match data[at + 1..].windows(3).position(|w| w == [0, 0, 1]) {
                Some(n) => at += 1 + n,
                None => break,
            }
            continue;
        }
        let id = data[at + 3];
        match id {
            0xBA => {
                at += if data.get(at + 4).is_some_and(|b| b & 0xC0 == 0x40) {
                    // MPEG-2: 14 bytes and the stuffing the last says
                    14 + usize::from(data.get(at + 13).map_or(0, |b| b & 7))
                } else {
                    12
                };
            }
            0xB9 => break,
            0xBB..=0xFF => {
                let Some(len) = data.get(at + 4..at + 6).map(|l| usize::from(u16::from_be_bytes([l[0], l[1]]))) else {
                    break;
                };
                let end = (at + 6 + len).min(data.len());
                if matches!(id, 0xBD | 0xC0..=0xEF)
                    && let Some(payload) = pes_payload(&data[at + 6..end])
                {
                    stream(id, payload, &mut seen, &mut info);
                }
                at += 6 + len;
            }
            _ => at += 4,
        }
        if is_full(&info) {
            break;
        }
    }
    // a video stream named from a cut header, or one without its sequence header yet, wants more of the front
    info.header_complete = !info.video.is_empty() && info.video.iter().all(|v| v.width.is_some());
    info
}

/// A PES packet's payload, past its header: MPEG-2's, or MPEG-1's stuffing, buffer size and time stamps.
fn pes_payload(b: &[u8]) -> Option<&[u8]> {
    if b.first()? & 0xC0 == 0x80 {
        return b.get(3 + usize::from(*b.get(2)?)..);
    }
    let mut at = b.iter().position(|x| *x != 0xFF)?;
    if b.get(at)? & 0xC0 == 0x40 {
        at += 2;
    }
    let skip = match b.get(at)? >> 4 {
        0x2 => 5,
        0x3 => 10,
        _ => 1,
    };
    b.get(at + skip..)
}

/// What's known of a stream so far: where its track is, and a sequence header whose extension (MPEG-2's mark) may
/// begin the stream's next packet.
struct Seen {
    id: u8,
    sub: u8,
    place: Place,
    seq: Option<[u8; 4]>,
    /// the end of the stream's last payload while its header isn't read: a header can straddle two packets
    carry: Vec<u8>,
}

#[derive(Clone, Copy)]
enum Place {
    Video(usize),
    Audio(usize),
    Other,
}

/// The bytes kept of a payload for the next one of its stream: more than any header this reads.
const CARRY: usize = 64;

fn tail(b: &[u8]) -> Vec<u8> {
    b[b.len().saturating_sub(CARRY)..].to_vec()
}

/// A payload's elementary stream bytes: past the substream id, frame count and pointer of private stream 1.
fn elementary(id: u8, payload: &[u8]) -> &[u8] {
    if id == 0xBD { payload.get(4..).unwrap_or_default() } else { payload }
}

fn stream(id: u8, payload: &[u8], seen: &mut Vec<Seen>, info: &mut MediaInfo) {
    let sub = if id == 0xBD { *payload.first().unwrap_or(&0) } else { 0 };
    let es = elementary(id, payload);
    if let Some(known) = seen.iter_mut().find(|k| (k.id, k.sub) == (id, sub)) {
        // what a stream's first packet didn't hold, a later one of its own can, with the end of the one before
        match known.place {
            Place::Video(n) => {
                let Some(v) = info.video.get_mut(n) else { return };
                if v.width.is_none() {
                    known.seq = video(&[known.carry.as_slice(), es].concat(), v);
                    known.carry = if v.width.is_none() { tail(es) } else { Vec::new() };
                } else if let Some(h) = known.seq {
                    // the carried bytes may hold the header itself: what counts is what comes after it
                    let joined = [known.carry.as_slice(), es].concat();
                    let after = joined.windows(4).rposition(|w| w == [0, 0, 1, 0xB3]).map_or(0, |at| at + 4);
                    match first_start_code(&joined[after..]) {
                        Some((0xB5, next)) if next >> 4 == 1 => {
                            v.codec = "mpeg2video".into();
                            aspect(v, h, true);
                            known.seq = None;
                        }
                        Some(_) => known.seq = None,
                        None => known.carry = tail(es),
                    }
                }
            }
            Place::Audio(n) => {
                if let Some(a) = info.audio.get_mut(n).filter(|a| a.sample_rate.is_none()) {
                    let joined = [known.carry.as_slice(), es].concat();
                    *a = audio(id, sub, payload, &joined).unwrap_or_else(|| a.clone());
                    known.carry = if a.sample_rate.is_none() { tail(es) } else { Vec::new() };
                }
            }
            Place::Other => {}
        }
        return;
    }
    let mut known = Seen { id, sub, place: Place::Other, seq: None, carry: Vec::new() };
    match id {
        0xE0..=0xEF => {
            let mut v = VideoTrack { codec: "mpeg2video".into(), ..VideoTrack::default() };
            known.seq = video(es, &mut v);
            if v.width.is_none() || known.seq.is_some() {
                known.carry = tail(es);
            }
            known.place = Place::Video(info.video.len());
            info.video.push(v);
        }
        _ if (0x20..=0x3F).contains(&sub) && id == 0xBD => {
            info.subtitles.push(SubtitleTrack { codec: "dvd_subtitle".into(), ..SubtitleTrack::default() });
        }
        _ => {
            let Some(a) = audio(id, sub, payload, es) else { return };
            if a.sample_rate.is_none() {
                known.carry = tail(es);
            }
            known.place = Place::Audio(info.audio.len());
            info.audio.push(a);
        }
    }
    seen.push(known);
}

/// An audio stream's track from its elementary bytes `es` (LPCM's from the payload's own header): MPEG audio, or a
/// DVD substream. None for a stream that isn't audio.
fn audio(id: u8, sub: u8, payload: &[u8], es: &[u8]) -> Option<AudioTrack> {
    Some(match (id, sub) {
        (0xC0..=0xDF, _) => {
            let mut a = AudioTrack { codec: "mp2".into(), ..AudioTrack::default() };
            mpeg_audio(es, &mut a);
            a
        }
        (0xBD, 0x80..=0x87) => ac3(es),
        (0xBD, 0x88..=0x8F) => dts(es),
        (0xBD, 0xA0..=0xA7) => lpcm(payload),
        _ => return None,
    })
}

/// The first start code in `b` and the byte after it.
fn first_start_code(b: &[u8]) -> Option<(u8, u8)> {
    let at = b.windows(3).position(|w| w == [0, 0, 1])?;
    Some((*b.get(at + 3)?, *b.get(at + 4).unwrap_or(&0)))
}

/// From an MPEG-1/2 sequence header: size, aspect and frame rate; MPEG-2 when a sequence extension follows. The
/// header when the payload ended before saying which (its extension may start the stream's next packet).
fn video(payload: &[u8], v: &mut VideoTrack) -> Option<[u8; 4]> {
    let Some(at) = payload.windows(4).position(|w| w == [0, 0, 1, 0xB3]) else {
        if payload.windows(4).any(|w| w == [0, 0, 1, 0x09] || w == [0, 0, 1, 0x67]) {
            v.codec = "h264".into();
        }
        return None;
    };
    let h: [u8; 4] = payload.get(at + 4..at + 8)?.try_into().ok()?;
    let width = (u64::from(h[0]) << 4) | u64::from(h[1] >> 4);
    let height = (u64::from(h[1] & 0x0F) << 8) | u64::from(h[2]);
    v.width = (width > 0).then_some(width);
    v.height = (height > 0).then_some(height);
    v.frame_rate = match h[3] & 0x0F {
        1 => Some(24000.0 / 1001.0),
        2 => Some(24.0),
        3 => Some(25.0),
        4 => Some(30000.0 / 1001.0),
        5 => Some(30.0),
        6 => Some(50.0),
        7 => Some(60000.0 / 1001.0),
        8 => Some(60.0),
        _ => None,
    };
    // the next start code after the header (past its 8 bytes and any quantiser matrices): an extension with id 1
    // says MPEG-2, anything else MPEG-1, none yet nothing
    let next = first_start_code(payload.get(at + 8..).unwrap_or_default());
    let mpeg2 = matches!(next, Some((0xB5, n)) if n >> 4 == 1);
    v.codec = if mpeg2 { "mpeg2video" } else { "mpeg1video" }.into();
    aspect(v, h, mpeg2);
    next.is_none().then_some(h)
}

/// The aspect code, as a display size of the same height: MPEG-2's is the display's aspect, MPEG-1's a pixel's
/// height over its width (ISO 11172-2 table 2-D.2).
fn aspect(v: &mut VideoTrack, h: [u8; 4], mpeg2: bool) {
    let (Some(width), Some(height)) = (v.width, v.height) else { return };
    let code = usize::from(h[3] >> 4);
    let display_width = if mpeg2 {
        [None, None, Some(4.0 / 3.0), Some(16.0 / 9.0), Some(2.21)]
            .get(code)
            .copied()
            .flatten()
            .map(|a: f64| height as f64 * a)
    } else {
        const PIXEL: [f64; 14] = [
            1.0, 0.6735, 0.7031, 0.7615, 0.8055, 0.8437, 0.8935, 0.9157, 0.9815, 1.0255, 1.0695, 1.0950, 1.1575, 1.2015,
        ];
        code.checked_sub(1).and_then(|n| PIXEL.get(n)).map(|p| width as f64 / p)
    };
    v.display_width = display_width.map(|w| w.round() as u64);
    v.display_height = display_width.map(|_| height);
}

/// An MPEG audio frame header: layer, rate, mono or not.
fn mpeg_audio(payload: &[u8], a: &mut AudioTrack) {
    let Some(at) = payload.windows(2).position(|w| w[0] == 0xFF && w[1] & 0xE0 == 0xE0) else { return };
    let Some(h) = payload.get(at..at + 4) else { return };
    let version = (h[1] >> 3) & 3;
    let layer = (h[1] >> 1) & 3;
    a.codec = match layer {
        1 => "mp3",
        2 => "mp2",
        3 => "mp1",
        _ => "mp2",
    }
    .into();
    let rates: [f64; 3] = match version {
        3 => [44100.0, 48000.0, 32000.0],
        2 => [22050.0, 24000.0, 16000.0],
        0 => [11025.0, 12000.0, 8000.0],
        _ => return,
    };
    a.sample_rate = rates.get(usize::from((h[2] >> 2) & 3)).copied();
    a.channels = Some(if h[3] >> 6 == 3 { 1 } else { 2 });
}

/// An AC-3 sync frame: rate, and channels from acmod and the LFE flag.
fn ac3(b: &[u8]) -> AudioTrack {
    const CHANNELS: [u32; 8] = [2, 1, 2, 3, 3, 4, 4, 5];
    let mut a = AudioTrack { codec: "ac3".into(), ..AudioTrack::default() };
    let Some(at) = b.windows(2).position(|w| w == [0x0B, 0x77]) else { return a };
    let Some(f) = b.get(at..at + 8) else { return a };
    let (fscod, bsid) = (f[4] >> 6, f[5] >> 3);
    if bsid > 10 {
        // E-AC-3 (ETSI TS 102 366 E.1.2): rate code, a second one or the block count, acmod, LFE, all in byte 4
        a.codec = "eac3".into();
        let rate = if fscod == 3 {
            [24000.0, 22050.0, 16000.0].get(usize::from((f[4] >> 4) & 3))
        } else {
            [48000.0, 44100.0, 32000.0].get(usize::from(fscod))
        };
        a.sample_rate = rate.copied();
        a.channels = Some(u64::from(CHANNELS[usize::from((f[4] >> 1) & 7)] + u32::from(f[4] & 1)));
        return a;
    }
    // bsid 9 and 10 are AC-3 at a half and a quarter of the rate
    let shift = u32::from(bsid.saturating_sub(8));
    a.sample_rate = [48000.0, 44100.0, 32000.0].get(usize::from(fscod)).map(|r| r / f64::from(1u32 << shift));
    let mut bits = Bits { b: &f[6..], at: 0 };
    let acmod = bits.take(3);
    if acmod & 1 != 0 && acmod != 1 {
        bits.take(2);
    }
    if acmod & 4 != 0 {
        bits.take(2);
    }
    if acmod == 2 {
        bits.take(2);
    }
    let lfe = bits.take(1);
    a.channels = Some(u64::from(CHANNELS[acmod as usize] + lfe));
    a
}

/// A DTS core frame: rate, and channels from the arrangement and the LFE field.
fn dts(b: &[u8]) -> AudioTrack {
    let mut a = AudioTrack { codec: "dts".into(), ..AudioTrack::default() };
    let Some(at) = b.windows(4).position(|w| w == [0x7F, 0xFE, 0x80, 0x01]) else { return a };
    let Some(f) = b.get(at + 4..at + 12) else { return a };
    let mut bits = Bits { b: f, at: 0 };
    // frame type 1, deficit 5, crc 1, blocks 7, frame size 14
    bits.take(28);
    let amode = bits.take(6);
    let sfreq = bits.take(4);
    // rate 5, five flags, extension id 3, extension 1, predictor history 1, then lfe 2
    bits.take(15);
    let lfe = bits.take(2);
    a.sample_rate = match sfreq {
        1 => Some(8000.0),
        2 => Some(16000.0),
        3 => Some(32000.0),
        6 => Some(11025.0),
        7 => Some(22050.0),
        8 => Some(44100.0),
        11 => Some(12000.0),
        12 => Some(24000.0),
        13 => Some(48000.0),
        _ => None,
    };
    let channels = match amode {
        0 => Some(1),
        1..=4 => Some(2),
        5 | 6 => Some(3),
        7 | 8 => Some(4),
        9 => Some(5),
        10..=12 => Some(6),
        _ => None,
    };
    a.channels = channels.map(|c: u64| c + u64::from(lfe != 0));
    a
}

/// A DVD LPCM substream header: rate and channels.
fn lpcm(b: &[u8]) -> AudioTrack {
    let mut a = AudioTrack { codec: "pcm".into(), ..AudioTrack::default() };
    if let Some(h) = b.get(5) {
        a.sample_rate = Some(if (h >> 4) & 3 == 1 { 96000.0 } else { 48000.0 });
        a.channels = Some(u64::from(h & 7) + 1);
    }
    a
}

/// Big-endian bits, zeros past the end.
struct Bits<'a> {
    b: &'a [u8],
    at: usize,
}

impl Bits<'_> {
    fn take(&mut self, n: usize) -> u32 {
        let mut v = 0;
        for _ in 0..n {
            let bit = self.b.get(self.at / 8).map_or(0, |byte| (byte >> (7 - self.at % 8)) & 1);
            v = (v << 1) | u32::from(bit);
            self.at += 1;
        }
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pack() -> Vec<u8> {
        // an MPEG-2 pack header, no stuffing
        vec![0, 0, 1, 0xBA, 0x44, 0, 4, 0, 4, 1, 0x01, 0x89, 0xC3, 0xF8]
    }

    fn pes(id: u8, payload: &[u8]) -> Vec<u8> {
        let mut out = vec![0, 0, 1, id];
        out.extend(((payload.len() + 3) as u16).to_be_bytes());
        out.extend([0x81, 0x00, 0x00]);
        out.extend(payload);
        out
    }

    /// A PAL DVD's VOB: 720x576 16:9 MPEG-2 at 25 fps, 5.1 AC-3, stereo DTS, 2 channel LPCM, MPEG-1 layer 2 and a
    /// subpicture stream.
    fn vob() -> Vec<u8> {
        let mut seq = vec![0, 0, 1, 0xB3, 0x2D, 0x02, 0x40, 0x33, 0xFF, 0xFF, 0xE0, 0x18];
        seq.extend([0, 0, 1, 0xB5, 0x14, 0x8A, 0, 1, 0, 0]);
        // AC-3: 48 kHz, bsid 8, acmod 7 (3/2), lfe on
        let ac3 = [0x80, 1, 0, 1, 0x0B, 0x77, 0, 0, 0x04, 0x40, 0xE1, 0x10];
        // DTS: amode 2 (stereo), 48 kHz, no lfe
        let dts = [0x88, 1, 0, 1, 0x7F, 0xFE, 0x80, 0x01, 0xFC, 0x3C, 0x90, 0x00, 0xB4, 0x00, 0x00, 0x00];
        let lpcm = [0xA0, 1, 0, 1, 0x00, 0x01, 0x80];
        let mp2 = [0xFF, 0xFD, 0x84, 0x04];
        let sub = [0x20, 0, 0];
        let mut out = pack();
        for p in [pes(0xE0, &seq), pes(0xBD, &ac3), pes(0xBD, &dts), pes(0xBD, &lpcm), pes(0xC0, &mp2), pes(0xBD, &sub)]
        {
            out.extend(p);
            out.extend(pack());
        }
        // the same streams again add nothing
        out.extend(pes(0xBD, &ac3));
        out.extend(pes(0xE0, &[0u8; 32]));
        out
    }

    #[test]
    fn a_vobs_packs_name_its_streams() {
        let data = vob();
        assert!(is_mpeg_ps(&data));
        let info = super::super::media::parse(&data).unwrap();
        assert_eq!(info.container, "mpeg-ps");
        assert!(info.header_complete);
        let v = &info.video[0];
        assert_eq!(
            (v.codec.as_str(), v.width, v.height, v.frame_rate),
            ("mpeg2video", Some(720), Some(576), Some(25.0))
        );
        assert_eq!((v.display_width, v.display_height), (Some(1024), Some(576)), "16:9");
        let audio: Vec<(&str, Option<u64>, Option<f64>)> =
            info.audio.iter().map(|a| (a.codec.as_str(), a.channels, a.sample_rate)).collect();
        assert_eq!(
            audio,
            [
                ("ac3", Some(6), Some(48000.0)),
                ("dts", Some(2), Some(48000.0)),
                ("pcm", Some(2), Some(48000.0)),
                ("mp2", Some(2), Some(48000.0)),
            ]
        );
        assert_eq!(info.subtitles.len(), 1);
        assert_eq!(info.subtitles[0].codec, "dvd_subtitle");
    }

    #[test]
    fn each_video_stream_gets_its_own_header_and_mpeg1_aspect_is_a_pixels() {
        // E0 and E1 start without a sequence header; E0's comes later and is E0's alone
        let mpeg1 = [0, 0, 1, 0xB3, 0x16, 0x01, 0x20, 0x83, 0xFF, 0xFF, 0xE0, 0x18];
        let mut data = pack();
        for p in [pes(0xE0, &[0u8; 8]), pes(0xE1, &[0u8; 8]), pes(0xE0, &mpeg1)] {
            data.extend(p);
        }
        let info = mpeg_ps(&data);
        assert_eq!(info.video[0].codec, "mpeg1video");
        assert_eq!((info.video[0].width, info.video[0].height), (Some(352), Some(288)));
        // code 8: a 0.9157 pixel, CCIR 601 625-line
        assert_eq!(info.video[0].display_width, Some(384));
        assert_eq!(info.video[1].width, None, "E1's own header hasn't come");
        assert!(!info.header_complete, "more of the front could name E1");
    }

    #[test]
    fn a_header_split_over_packets_is_read_from_the_next_ones() {
        let mut data = pack();
        // the sequence header ends one packet, its MPEG-2 extension starts the next
        data.extend(pes(0xE0, &[0, 0, 1, 0xB3, 0x2D, 0x02, 0x40, 0x33, 0xFF, 0xFF, 0xE0, 0x18]));
        data.extend(pes(0xE0, &[0, 0, 1, 0xB5, 0x14, 0x8A, 0, 1, 0, 0, 0, 0, 1, 0]));
        // an MPEG audio frame header cut after its first byte, then a whole one
        data.extend(pes(0xC0, &[0x00, 0xFF]));
        data.extend(pes(0xC0, &[0xFD, 0x84, 0x04, 0x00, 0xFF, 0xFD, 0x84, 0x04]));
        let info = mpeg_ps(&data);
        assert_eq!(info.video[0].codec, "mpeg2video");
        assert_eq!(info.video[0].display_width, Some(1024), "16:9 by MPEG-2's table");
        assert_eq!((info.audio[0].codec.as_str(), info.audio[0].sample_rate), ("mp2", Some(48000.0)));
        assert_eq!(info.audio.len(), 1);
    }

    #[test]
    fn ac3_at_reduced_rates_and_eac3_are_read() {
        // AC-3 bsid 9: half of 48 kHz; stereo (acmod 2), no LFE
        let half = ac3(&[0x0B, 0x77, 0, 0, 0x04, 9 << 3, 0x40, 0x00]);
        assert_eq!((half.codec.as_str(), half.sample_rate, half.channels), ("ac3", Some(24000.0), Some(2)));
        // E-AC-3 bsid 16: fscod 3 with fscod2 0 is 24 kHz; acmod 7 with LFE is 5.1
        let e = ac3(&[0x0B, 0x77, 0, 0, 0b1100_1111, 16 << 3, 0, 0]);
        assert_eq!((e.codec.as_str(), e.sample_rate, e.channels), ("eac3", Some(24000.0), Some(6)));
        let e48 = ac3(&[0x0B, 0x77, 0, 0, 0b0011_0100, 16 << 3, 0, 0]);
        assert_eq!((e48.sample_rate, e48.channels), (Some(48000.0), Some(2)));
    }

    #[test]
    fn a_sequence_header_cut_by_a_packet_boundary_is_joined() {
        let seq = [0, 0, 1, 0xB3, 0x2D, 0x02, 0x40, 0x33, 0xFF, 0xFF, 0xE0, 0x18, 0, 0, 1, 0xB5, 0x14, 0x8A];
        let mut data = pack();
        data.extend(pes(0xE0, &[&[0x47u8; 20][..], &seq[..6]].concat()));
        data.extend(pes(0xE0, &seq[6..]));
        // an AC-3 sync frame cut after its sync word
        data.extend(pes(0xBD, &[0x80, 1, 0, 1, 0x0B, 0x77]));
        data.extend(pes(0xBD, &[0x80, 1, 0, 1, 0, 0, 0x04, 0x40, 0xE1, 0x10]));
        let info = mpeg_ps(&data);
        let v = &info.video[0];
        assert_eq!(
            (v.codec.as_str(), v.width, v.height, v.frame_rate),
            ("mpeg2video", Some(720), Some(576), Some(25.0))
        );
        assert_eq!((info.audio[0].sample_rate, info.audio[0].channels), (Some(48000.0), Some(6)));
    }

    #[test]
    fn a_cut_sequence_header_wants_more_of_the_front() {
        let mut data = pack();
        data.extend(pes(0xE0, &[0, 0, 1, 0xB3, 0x2D]));
        let info = mpeg_ps(&data);
        assert_eq!(info.video.len(), 1);
        assert!(!info.header_complete);
    }

    #[test]
    fn cut_or_hostile_packs_are_safe() {
        let data = vob();
        for cut in 0..data.len() {
            let _ = mpeg_ps(&data[..cut]);
        }
        // packets with every substream id keep media's few tracks
        let mut many = pack();
        for _ in 0..1000 {
            for sub in 0x20u8..0x40 {
                many.extend(pes(0xBD, &[sub, 0, 0]));
            }
        }
        assert!(mpeg_ps(&many).subtitles.len() <= super::super::media::MAX_TRACKS);
        assert!(!is_mpeg_ps(b"\0\0\x01\xba"));
    }
}
