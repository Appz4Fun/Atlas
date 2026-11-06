//! Matroska audio frames to sound: decoded by symphonia (pure Rust) for AAC-LC, FLAC, MP3, MP2 and Vorbis, by hand
//! for plain PCM, and mixed down to one channel. AC-3, E-AC-3, DTS, TrueHD and Opus have no pure-Rust decoder Atlas
//! may use, soo their tracks get no fingerprint.

use symphonia::core::audio::{Channels, Position};
use symphonia::core::codecs::audio::well_known::{
    CODEC_ID_AAC, CODEC_ID_FLAC, CODEC_ID_MP2, CODEC_ID_MP3, CODEC_ID_VORBIS,
};
use symphonia::core::codecs::audio::{AudioCodecParameters, AudioDecoder, AudioDecoderOptions};
use symphonia::core::packet::Packet;
use symphonia::core::units::{Duration, Timestamp};

use super::mkv::AudioTrack;

/// The codecs read, by Matroska codec id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Codec {
    Aac,
    Flac,
    Mp3,
    Mp2,
    Vorbis,
    /// little-endian integers, big-endian integers, or IEEE floats (little-endian)
    PcmLe,
    PcmBe,
    PcmFloat,
}

/// The Matroska codec ids decoded here.
const READ: [&str; 10] = [
    "A_AAC",
    "A_AAC/MPEG2/LC",
    "A_AAC/MPEG4/LC",
    "A_FLAC",
    "A_MPEG/L3",
    "A_MPEG/L2",
    "A_VORBIS",
    "A_PCM/INT/LIT",
    "A_PCM/INT/BIG",
    "A_PCM/FLOAT/IEEE",
];

fn codec(id: &str) -> Option<Codec> {
    Some(match id {
        "A_AAC" | "A_AAC/MPEG2/LC" | "A_AAC/MPEG4/LC" => Codec::Aac,
        "A_FLAC" => Codec::Flac,
        "A_MPEG/L3" => Codec::Mp3,
        "A_MPEG/L2" => Codec::Mp2,
        "A_VORBIS" => Codec::Vorbis,
        "A_PCM/INT/LIT" => Codec::PcmLe,
        "A_PCM/INT/BIG" => Codec::PcmBe,
        "A_PCM/FLOAT/IEEE" => Codec::PcmFloat,
        _ => return None,
    })
}

/// Why a track of codec `id` (the Matroska codec id, as the probe keeps it) can't be fingerprinted, in a word or
/// two; None when it can.
pub fn why_not(id: &str) -> Option<&'static str> {
    if READ.contains(&id) {
        return None;
    }
    let upper = id.to_ascii_uppercase();
    Some(if upper.starts_with("A_AC3") || upper == "A_EAC3" {
        "ac3"
    } else if upper.starts_with("A_DTS") {
        "dts"
    } else if upper.starts_with("A_TRUEHD") || upper == "A_MLP" {
        "truehd"
    } else if upper == "A_OPUS" {
        "opus"
    } else if upper.starts_with("A_AAC") {
        // HE-AAC, AAC Main and the like: symphonia decodes AAC-LC only
        "aac_profile"
    } else {
        "other"
    })
}

/// The sound of `frames`, one channel: its sample rate, and for each frame the samples it decoded to (none for a
/// frame that didn't decode). None when the track can't be decoded at all. A frame that makes a decoder panic ends the
/// decode there, never the caller.
pub fn decode(track: &AudioTrack, frames: &[&[u8]]) -> Option<(u32, Vec<Vec<i16>>)> {
    let codec = codec(&track.codec)?;
    std::panic::catch_unwind(|| match codec {
        Codec::PcmLe | Codec::PcmBe | Codec::PcmFloat => pcm(track, codec, frames),
        _ => with_symphonia(track, codec, frames),
    })
    .ok()
    .flatten()
}

/// AAC's AudioSpecificConfig for AAC-LC at the track's rate and channels, for an old codec id that comes without one.
fn aac_config(track: &AudioTrack) -> Option<Vec<u8>> {
    const RATES: [u32; 12] = [96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000];
    let rate = track.sample_rate? as u32;
    let index = RATES.iter().position(|r| *r == rate)? as u16;
    let channels = u16::try_from(track.channels.unwrap_or(2)).ok().filter(|c| (1..=2).contains(c))?;
    // object type 2 (LC), 5 bits; rate index, 4; channel configuration, 4; then three zero flags
    let bits = (2u16 << 11) | (index << 7) | (channels << 3);
    Some(bits.to_be_bytes().to_vec())
}

fn with_symphonia(track: &AudioTrack, codec: Codec, frames: &[&[u8]]) -> Option<(u32, Vec<Vec<i16>>)> {
    let mut params = AudioCodecParameters::new();
    match codec {
        Codec::Aac => {
            let config = if track.private.len() >= 2 { track.private.clone() } else { aac_config(track)? };
            params.for_codec(CODEC_ID_AAC).with_extra_data(config.into_boxed_slice());
        }
        Codec::Flac => {
            // CodecPrivate is `fLaC` and the metadata blocks; the decoder takes STREAMINFO's body, the first block
            let blocks = track.private.strip_prefix(b"fLaC")?;
            let (head, rest) = (blocks.first()?, blocks.get(4..)?);
            if head & 0x7F != 0 {
                return None;
            }
            params.for_codec(CODEC_ID_FLAC).with_extra_data(rest.get(..34)?.to_vec().into_boxed_slice());
        }
        Codec::Mp3 => {
            params.for_codec(CODEC_ID_MP3);
        }
        Codec::Mp2 => {
            params.for_codec(CODEC_ID_MP2);
        }
        Codec::Vorbis => {
            params.for_codec(CODEC_ID_VORBIS).with_extra_data(track.private.clone().into_boxed_slice());
        }
        Codec::PcmLe | Codec::PcmBe | Codec::PcmFloat => return None,
    }
    if let Some(rate) = track.sample_rate.filter(|r| (1000.0..=768_000.0).contains(r)) {
        params.with_sample_rate(rate as u32);
    }
    if let Some(n) = track.channels.and_then(|c| u16::try_from(c).ok()).filter(|c| (1..=32).contains(c)) {
        params.with_channels(if n == 2 {
            Channels::Positioned(Position::FRONT_LEFT | Position::FRONT_RIGHT)
        } else {
            Channels::Discrete(n)
        });
    }
    let mut decoder: Box<dyn AudioDecoder> =
        symphonia::default::get_codecs().make_audio_decoder(&params, &AudioDecoderOptions::default()).ok()?;
    let mut rate = None;
    let mut out = Vec::with_capacity(frames.len());
    let mut planes: Vec<Vec<f32>> = Vec::new();
    for (i, frame) in frames.iter().enumerate() {
        let packet = Packet::new(0, Timestamp::new(i as i64), Duration::new(1), frame.to_vec());
        let Ok(decoded) = decoder.decode(&packet) else {
            out.push(Vec::new());
            continue;
        };
        let this = decoded.spec().rate();
        // a stream that changes its rate part way is not one sound any more
        if *rate.get_or_insert(this) != this {
            break;
        }
        decoded.copy_to_vecs_planar::<f32>(&mut planes);
        out.push(mix(&planes));
    }
    Some((rate?, out))
}

/// Planes of samples, mixed down to one channel.
fn mix(planes: &[Vec<f32>]) -> Vec<i16> {
    let Some(len) = planes.iter().map(Vec::len).min() else { return Vec::new() };
    let n = planes.len() as f32;
    (0..len)
        .map(|i| {
            let sum: f32 = planes.iter().filter_map(|p| p.get(i)).sum();
            ((sum / n).clamp(-1.0, 1.0) * 32767.0).round() as i16
        })
        .collect()
}

/// Plain PCM: each frame is whole sample frames of every channel, interleaved.
fn pcm(track: &AudioTrack, codec: Codec, frames: &[&[u8]]) -> Option<(u32, Vec<Vec<i16>>)> {
    let rate = track.sample_rate.filter(|r| (1000.0..=768_000.0).contains(r))? as u32;
    let channels = usize::try_from(track.channels.unwrap_or(1)).ok().filter(|c| (1..=32).contains(c))?;
    let bits = track.bit_depth.unwrap_or(16);
    let width = match (codec, bits) {
        (Codec::PcmFloat, 32 | 64) | (Codec::PcmLe | Codec::PcmBe, 8 | 16 | 24 | 32) => (bits / 8) as usize,
        _ => return None,
    };
    let sample = |b: &[u8]| -> f32 {
        match (codec, width) {
            (Codec::PcmFloat, 4) => b.try_into().map(f32::from_le_bytes).unwrap_or(0.0),
            (Codec::PcmFloat, _) => b.try_into().map(f64::from_le_bytes).unwrap_or(0.0) as f32,
            // 8-bit PCM in Matroska is unsigned
            (_, 1) => (f32::from(b.first().copied().unwrap_or(128)) - 128.0) / 128.0,
            _ => {
                let mut v: i64 = 0;
                for i in 0..width {
                    let byte = if codec == Codec::PcmBe { b.get(i) } else { b.get(width - 1 - i) };
                    v = v << 8 | i64::from(*byte.unwrap_or(&0));
                }
                let shift = 64 - 8 * width as u32;
                ((v << shift) >> shift) as f32 / (1i64 << (8 * width - 1)) as f32
            }
        }
    };
    let out = frames
        .iter()
        .map(|frame| {
            frame
                .chunks_exact(width * channels)
                .map(|s| {
                    let sum: f32 = s.chunks_exact(width).map(sample).filter(|v| v.is_finite()).sum();
                    ((sum / channels as f32).clamp(-1.0, 1.0) * 32767.0).round() as i16
                })
                .collect()
        })
        .collect();
    Some((rate, out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(codec: &str, private: &[u8]) -> AudioTrack {
        AudioTrack {
            number: 1,
            codec: codec.into(),
            private: private.to_vec(),
            sample_rate: Some(48_000.0),
            channels: Some(2),
            bit_depth: Some(16),
            strip: Vec::new(),
            timestamp_scale: 1_000_000,
        }
    }

    fn noise(len: usize, seed: u64) -> Vec<u8> {
        let mut x = seed | 1;
        (0..len)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x as u8
            })
            .collect()
    }

    #[test]
    fn codecs_without_a_decoder_say_why() {
        assert_eq!(why_not("A_AAC"), None);
        assert_eq!(why_not("A_FLAC"), None);
        assert_eq!(why_not("A_AC3"), Some("ac3"));
        assert_eq!(why_not("A_EAC3"), Some("ac3"));
        assert_eq!(why_not("A_DTS/MA"), Some("dts"));
        assert_eq!(why_not("A_TRUEHD"), Some("truehd"));
        assert_eq!(why_not("A_OPUS"), Some("opus"));
        assert_eq!(why_not("A_AAC/MPEG4/LC/SBR"), Some("aac_profile"));
        assert_eq!(why_not("a_aac"), Some("aac_profile"), "codec ids are case sensitive");
        assert_eq!(why_not("ac-3"), Some("other"));
        assert!(decode(&track("A_AC3", &[]), &[&[0x0B, 0x77]]).is_none());
    }

    #[test]
    fn garbage_never_panics_whatever_the_codec() {
        let flac_private = [b"fLaC".as_slice(), &[0x80, 0, 0, 34], &noise(34, 9)].concat();
        let vorbis_private = [&[2u8, 30, 60][..], &noise(200, 3)].concat();
        for (codec, private) in [
            ("A_AAC", vec![0x12, 0x10]),
            ("A_AAC", noise(5, 1)),
            ("A_AAC/MPEG4/LC", Vec::new()),
            ("A_FLAC", flac_private),
            ("A_FLAC", b"fLaC".to_vec()),
            ("A_MPEG/L3", Vec::new()),
            ("A_MPEG/L2", Vec::new()),
            ("A_VORBIS", vorbis_private),
            ("A_VORBIS", Vec::new()),
            ("A_PCM/INT/LIT", Vec::new()),
            ("A_PCM/INT/BIG", Vec::new()),
            ("A_PCM/FLOAT/IEEE", Vec::new()),
        ] {
            let t = track(codec, &private);
            let frames: Vec<Vec<u8>> = (0..40).map(|i| noise(1 + i * 37 % 1500, i as u64 + 7)).collect();
            let refs: Vec<&[u8]> = frames.iter().map(Vec::as_slice).collect();
            let _ = decode(&t, &refs);
            let _ = decode(&t, &[]);
            let _ = decode(&t, &[&[]]);
        }
    }

    #[test]
    fn pcm_is_mixed_down() {
        // two channels of 16-bit little-endian: (1000, 3000) mixes to 2000
        let frame: Vec<u8> = [1000i16, 3000, -1000, -3000].iter().flat_map(|s| s.to_le_bytes()).collect();
        let (rate, out) = decode(&track("A_PCM/INT/LIT", &[]), &[&frame]).unwrap();
        assert_eq!((rate, out), (48_000, vec![vec![2000, -2000]]));
        let frame: Vec<u8> = [1000i16, 3000].iter().flat_map(|s| s.to_be_bytes()).collect();
        assert_eq!(decode(&track("A_PCM/INT/BIG", &[]), &[&frame]).unwrap().1, vec![vec![2000]]);
        let mut t = track("A_PCM/FLOAT/IEEE", &[]);
        t.bit_depth = Some(32);
        let frame: Vec<u8> = [0.5f32, 0.0].iter().flat_map(|s| s.to_le_bytes()).collect();
        assert_eq!(decode(&t, &[&frame]).unwrap().1, vec![vec![8192]]);
    }

    #[test]
    fn an_old_aac_id_gets_a_config() {
        assert_eq!(aac_config(&track("A_AAC/MPEG4/LC", &[])), Some(vec![0x11, 0x90]));
        let mut six = track("A_AAC/MPEG4/LC", &[]);
        six.channels = Some(6);
        assert_eq!(aac_config(&six), None, "symphonia's AAC decodes one or two channels");
    }
}
